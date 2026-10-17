//! When the local player's party started searching for a match.
//!
//! A party search never sets the client's solo [`QueueFlag`](crate::QueueFlag); the start
//! time lives on the client's copy of the party object instead. The slot holding that object
//! and the offset of the start time move with every build, so both are read out of the
//! function that stops a search: it loads the party through a getter that returns a static
//! object, then passes the party to a one-line check of the start time.

use deadlock_memory::mem::MemoryReader;

use crate::error::{Error, Result};
use crate::pe::PeImage;
use crate::queue_flag::{call_target, find_stop, rip_target};

const CLIENT_MODULE: &str = "client.dll";

/// `call rel32`.
const CALL_LEN: usize = 5;

/// `mov reg, [rax+disp32]` with a REX.W prefix.
const LOAD_LEN: usize = 7;

/// How far past the party load the call to the start check may sit. The stop function
/// only null-checks the party and moves it into `rcx` in between.
const CHECK_WINDOW: usize = 0x20;

/// `cmp dword [rcx+disp32], 0; seta al; ret`, with the displacement cut out.
const CHECK_HEAD: [u8; 2] = [0x83, 0xb9];
const CHECK_TAIL: [u8; 5] = [0x00, 0x0f, 0x97, 0xc0, 0xc3];

/// Where this build of the client keeps the party.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartyLocation {
    /// Address of the pointer to the party object; null while not in a party.
    pub slot: u64,
    /// Offset of the search start time, unix seconds, in the party object.
    pub start_offset: u32,
}

/// The local player's party, as far as the search is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartyQueueState {
    /// When the party started searching, unix seconds; 0 while it is not searching.
    pub start_unix: u32,
}

/// Reads the local player's party search.
#[derive(Debug)]
pub struct PartyQueue {
    pid: u32,
    location: PartyLocation,
}

impl PartyQueue {
    /// Find the party slot in the client in `mem`.
    ///
    /// # Errors
    ///
    /// The client cannot be read, or this build does not match the code patterns
    /// ([`Error::AnchorNotFound`]).
    pub fn new(mem: &dyn MemoryReader) -> Result<Self> {
        let client = mem.module(CLIENT_MODULE)?;
        let image = PeImage::read(mem, &client)?;
        Self::from_image(mem, &image)
    }

    /// Find the party slot in `image`, a copy of the client in `mem`, without reading `mem`.
    ///
    /// Lets one copy of the client serve several lookups.
    ///
    /// # Errors
    ///
    /// [`Error::AnchorNotFound`] if this build does not match the code patterns.
    pub fn from_image(mem: &dyn MemoryReader, image: &PeImage) -> Result<Self> {
        Ok(PartyQueue {
            pid: mem.pid(),
            location: Self::locate(image)?,
        })
    }

    /// Find the party slot and the start time offset in a client image.
    ///
    /// # Errors
    ///
    /// [`Error::AnchorNotFound`] if this build does not match the code patterns.
    pub fn locate(image: &PeImage) -> Result<PartyLocation> {
        let stop = find_stop(image)?;
        let (_, end) = image
            .function_containing(stop)
            .ok_or(Error::AnchorNotFound(
                "stop function in the exception table",
            ))?;
        (stop as usize..end as usize)
            .find_map(|at| party_load(image, at, end as usize))
            .ok_or(Error::AnchorNotFound("party load in the stop function"))
    }

    /// Where the party is read from.
    pub fn location(&self) -> PartyLocation {
        self.location
    }

    /// The party's search state; `None` while not in a party or when it cannot be read.
    ///
    /// # Errors
    ///
    /// [`Error::WrongProcess`] if `mem` is not the process the slot was found in.
    pub fn read(&self, mem: &dyn MemoryReader) -> Result<Option<PartyQueueState>> {
        if mem.pid() != self.pid {
            return Err(Error::WrongProcess);
        }
        let party = match mem.read_u64(self.location.slot) {
            Ok(0) | Err(_) => return Ok(None),
            Ok(party) => party,
        };
        Ok(mem
            .read_u32(party + u64::from(self.location.start_offset))
            .ok()
            .map(|start_unix| PartyQueueState { start_unix }))
    }
}

/// The party location if `at` is `call getter; mov reg, [rax+disp32]` and the call after it
/// is the start check.
fn party_load(image: &PeImage, at: usize, end: usize) -> Option<PartyLocation> {
    let bytes = image.bytes();
    let load = at + CALL_LEN;
    let [rex, opcode, modrm] = *bytes.get(load..load + 3)? else {
        return None;
    };
    // mod 10 with rm 000 is `[rax+disp32]`; REX.B would make the base r8 instead.
    if !matches!(rex, 0x48 | 0x4c) || opcode != 0x8b || modrm & 0xc7 != 0x80 {
        return None;
    }
    let object = static_getter(image, call_target(bytes, at)?)?;
    let disp = i32::from_le_bytes(bytes.get(load + 3..load + LOAD_LEN)?.try_into().ok()?);
    let slot = u32::try_from(i64::from(object) + i64::from(disp)).ok()?;
    if slot as usize >= bytes.len() || image.is_code_address(image.base() + u64::from(slot)) {
        return None;
    }
    let after = load + LOAD_LEN;
    let start_offset = (after..(after + CHECK_WINDOW).min(end))
        .find_map(|call| start_check(bytes, call_target(bytes, call)?))?;
    Some(PartyLocation {
        slot: image.base() + u64::from(slot),
        start_offset,
    })
}

/// RVA of the object a `lea rax, [rip+disp32]; ret` getter at `rva` returns.
fn static_getter(image: &PeImage, rva: u32) -> Option<u32> {
    let at = rva as usize;
    let code = image.bytes().get(at..at + 8)?;
    if code[..3] != [0x48, 0x8d, 0x05] || code[7] != 0xc3 {
        return None;
    }
    rip_target(at, &code[3..7])
}

/// The displacement of the start check at `rva`.
fn start_check(bytes: &[u8], rva: u32) -> Option<u32> {
    let at = rva as usize;
    let code = bytes.get(at..at + 11)?;
    if code[..2] != CHECK_HEAD || code[6..] != CHECK_TAIL {
        return None;
    }
    Some(u32::from_le_bytes(code[2..6].try_into().ok()?))
}
