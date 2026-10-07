//! The client's "searching for a match" flag, read from a fixed address in its data section.
//!
//! The address moves with every client build, so it is derived from the code rather than
//! hard-coded: the "already searching" popup is the only code that loads its header string,
//! and the first byte compare in that function is the check of the flag. The same function
//! ends by passing the play controller (the object holding the request that started the
//! search) to its callee, so the first `lea rcx, [rip+disp32]; call` after the string load
//! is that object. The lookup copies the client image once; every poll afterwards is a
//! small read.

use deadlock_memory::mem::MemoryReader;
use memchr::{memchr_iter, memmem};

use crate::error::{Error, Result};
use crate::pe::PeImage;

const CLIENT_MODULE: &str = "client.dll";

/// Text only the "already searching" popup loads.
const ANCHOR_STRING: &[u8] = b"#Citadel_Popup_AlreadySearching_Header\0";

/// How far into the popup function the flag check is looked for.
const FUNCTION_WINDOW: u32 = 0x400;

/// Offset of the requested match mode in the play controller; the game mode and the bot
/// difficulty follow as consecutive 32-bit words.
const REQUEST_OFFSET: u64 = 0x8;

/// `lea reg, [rip+disp32]` is 7 bytes; so is `cmp byte [rip+disp32], imm8`.
const RIP_RELATIVE_LEN: usize = 7;

/// The match the player asked for, as the raw numbers the client holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueRequest {
    /// `ECitadelMatchMode`.
    pub match_mode: u32,
    /// `ECitadelGameMode`.
    pub game_mode: u32,
    /// `ECitadelBotDifficulty`; 0 when no bots were asked for.
    pub bot_difficulty: u32,
}

/// Reads whether the local player is searching for a match.
#[derive(Debug)]
pub struct QueueFlag {
    pid: u32,
    address: u64,
    request: Option<u64>,
}

impl QueueFlag {
    /// Find the flag in the client in `mem`.
    ///
    /// # Errors
    ///
    /// The client cannot be read, or this build does not match the anchor ([`Error::AnchorNotFound`]).
    pub fn new(mem: &dyn MemoryReader) -> Result<Self> {
        let client = mem.module(CLIENT_MODULE)?;
        let image = PeImage::read(mem, &client)?;
        let (address, request) = find_flag(&image)?;
        Ok(QueueFlag {
            pid: mem.pid(),
            address,
            request,
        })
    }

    /// Address of the flag byte in the target.
    pub fn address(&self) -> u64 {
        self.address
    }

    /// Address of the play controller in the target, if this build's code yielded one.
    pub fn request_address(&self) -> Option<u64> {
        self.request
    }

    /// What the player asked for while a search is running.
    ///
    /// `None` when not searching, when the play controller was not found, or when it cannot
    /// be read. The client resets these words the moment the search ends, so they are only
    /// meaningful while the flag is set.
    ///
    /// # Errors
    ///
    /// [`Error::WrongProcess`] if `mem` is not the process the flag was found in.
    pub fn request(&mut self, mem: &dyn MemoryReader) -> Result<Option<QueueRequest>> {
        if self.queueing(mem)? != Some(true) {
            return Ok(None);
        }
        let Some(controller) = self.request else {
            return Ok(None);
        };
        let mut words = [0u8; 12];
        if mem
            .read_into(controller + REQUEST_OFFSET, &mut words)
            .is_err()
        {
            return Ok(None);
        }
        let word = |at: usize| {
            u32::from_le_bytes([words[at], words[at + 1], words[at + 2], words[at + 3]])
        };
        Ok(Some(QueueRequest {
            match_mode: word(0),
            game_mode: word(4),
            bot_difficulty: word(8),
        }))
    }

    /// Whether the player is searching; `None` if the byte cannot be read.
    ///
    /// Any non-zero value counts as searching. A found match clears the flag the same way a
    /// cancel does.
    ///
    /// # Errors
    ///
    /// [`Error::WrongProcess`] if `mem` is not the process the flag was found in.
    pub fn queueing(&mut self, mem: &dyn MemoryReader) -> Result<Option<bool>> {
        if mem.pid() != self.pid {
            return Err(Error::WrongProcess);
        }
        Ok(mem.read_u8(self.address).ok().map(|state| state != 0))
    }
}

fn find_flag(image: &PeImage) -> Result<(u64, Option<u64>)> {
    let string = find_string(image).ok_or(Error::AnchorNotFound("popup header string"))?;
    let loader =
        find_loader(image, string).ok_or(Error::AnchorNotFound("code loading the popup header"))?;
    let (begin, end) = image
        .function_containing(loader)
        .ok_or(Error::AnchorNotFound(
            "function around the popup header load",
        ))?;
    let flag = find_compare(image, begin, end)
        .ok_or(Error::AnchorNotFound("flag check in the popup function"))?;
    let request = find_request(image, loader, end).map(|rva| image.base() + u64::from(rva));
    Ok((image.base() + u64::from(flag), request))
}

fn find_string(image: &PeImage) -> Option<u32> {
    image.data_sections().find_map(|(start, bytes)| {
        let at = memmem::find(bytes, ANCHOR_STRING)?;
        u32::try_from(start + at).ok()
    })
}

/// RVA of the `lea` that loads the string at `string`.
fn find_loader(image: &PeImage, string: u32) -> Option<u32> {
    image.code_sections().find_map(|(start, bytes)| {
        memchr_iter(0x8d, bytes).find_map(|at| {
            let begin = at.checked_sub(1)?;
            let rex = *bytes.get(begin)?;
            let modrm = *bytes.get(at + 1)?;
            if !matches!(rex, 0x48 | 0x4c) || modrm & 0xc7 != 0x05 {
                return None;
            }
            let target = rip_target(start + begin, bytes.get(at + 2..at + 6)?)?;
            (target == string).then_some(u32::try_from(start + begin).ok()?)
        })
    })
}

/// RVA of the data the first `cmp byte [rip+disp32], 0` in `[begin, end)` reads.
fn find_compare(image: &PeImage, begin: u32, end: u32) -> Option<u32> {
    let end = end.min(begin.saturating_add(FUNCTION_WINDOW)) as usize;
    let window = image.bytes().get(begin as usize..end)?;
    memchr_iter(0x80, window).find_map(|at| {
        if *window.get(at + 1)? != 0x3d || *window.get(at + RIP_RELATIVE_LEN - 1)? != 0 {
            return None;
        }
        let target = rip_target(begin as usize + at, window.get(at + 2..at + 6)?)?;
        let in_data = image.bytes().len() > target as usize
            && !image.is_code_address(image.base() + u64::from(target));
        in_data.then_some(target)
    })
}

/// RVA of the data the first `lea rcx, [rip+disp32]` directly followed by a `call` in
/// `[from, end)` points at. The static-init guard of the same object is loaded the same way
/// in the cold block after the function's first `ret`, so it comes later; it is also skipped
/// by name, because the function compares it against a constant (`cmp dword [guard], imm8`).
fn find_request(image: &PeImage, from: u32, end: u32) -> Option<u32> {
    let window = image.bytes().get(from as usize..end as usize)?;
    memmem::find_iter(window, &[0x48, 0x8d, 0x0d]).find_map(|at| {
        if *window.get(at + RIP_RELATIVE_LEN)? != 0xe8 {
            return None;
        }
        let target = rip_target(from as usize + at, window.get(at + 3..at + 7)?)?;
        let in_data = image.bytes().len() > target as usize
            && !image.is_code_address(image.base() + u64::from(target));
        let is_guard = memmem::find_iter(window, &[0x83, 0x3d]).any(|c| {
            window
                .get(c + 2..c + 6)
                .and_then(|d| rip_target(from as usize + c, d))
                == Some(target)
        });
        (in_data && !is_guard).then_some(target)
    })
}

/// RVA a 7-byte RIP-relative instruction at `at` refers to, given its 32-bit displacement.
fn rip_target(at: usize, disp: &[u8]) -> Option<u32> {
    let disp = i32::from_le_bytes(disp.try_into().ok()?);
    u32::try_from(i64::try_from(at + RIP_RELATIVE_LEN).ok()? + i64::from(disp)).ok()
}
