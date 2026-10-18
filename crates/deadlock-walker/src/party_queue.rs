//! When the local player's party started searching for a match.
//!
//! A party search never sets the client's solo [`QueueFlag`](crate::QueueFlag); the start
//! time lives on the client's copy of the party object instead. The slot holding that object
//! and the offset of the start time move with every build, so both are read out of the
//! function that stops a search: it loads the party through a getter that returns a static
//! object, then passes the party to a one-line check of the start time.

use deadlock_memory::mem::MemoryReader;
use valveprotos::deadlock::{ECitadelBotDifficulty, ECitadelGameMode, ECitadelMatchMode};

use crate::error::{Error, Result};
use crate::pe::PeImage;
use crate::queue_flag::{call_target, find_stop, rip_target};
use crate::rtti::VtableResolver;

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

const PARTY_MESSAGE: &str = "CSOCitadelParty";

/// More slots than protobuf-cpp gives a message vtable; the walk stops at the first slot
/// that does not point into code.
const VTABLE_SLOTS: usize = 32;

/// Varint wire tags of `CSOCitadelParty` fields: field number << 3.
const BOT_DIFFICULTY_TAG: u8 = 7 << 3;
const MATCH_MODE_TAG: u8 = 9 << 3;
const GAME_MODE_TAG: u8 = 10 << 3;
const START_TAG: u8 = 11 << 3;

/// How far before its tag write a field's load may end. The serializer puts only the
/// `lea` of the next stream position in between.
const TAG_WINDOW: usize = 0x10;

/// Furthest a mode field may sit from the start time. They are scalars of one message,
/// inside a party object smaller than this; a wider spread means the match was wrong.
const MAX_FIELD_DISTANCE: u32 = 0x100;

/// Where this build of the client keeps the party.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartyLocation {
    /// Address of the pointer to the party object; null while not in a party.
    pub slot: u64,
    /// Offset of the search start time, unix seconds, in the party object.
    pub start_offset: u32,
    /// Where the queued modes are; `None` when this build's party serializer did not match.
    /// The start time reads without them.
    pub modes: Option<PartyModeOffsets>,
}

/// Offsets of the queued modes in the party object, each a raw enum `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartyModeOffsets {
    /// `CSOCitadelParty.match_mode`, field 9.
    pub match_mode: u32,
    /// `CSOCitadelParty.game_mode`, field 10.
    pub game_mode: u32,
    /// `CSOCitadelParty.bot_difficulty`, field 7.
    pub bot_difficulty: u32,
}

/// The local player's party, as far as the search is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartyQueueState {
    /// When the party started searching, unix seconds; 0 while it is not searching.
    pub start_unix: u32,
    /// The queued match mode; `None` while not searching or for a value this build does not
    /// know.
    pub match_mode: Option<ECitadelMatchMode>,
    /// The queued game mode; `None` while not searching or for a value this build does not
    /// know.
    pub game_mode: Option<ECitadelGameMode>,
    /// The queued bot difficulty; `None` while not searching or for a value this build does
    /// not know.
    pub bot_difficulty: Option<ECitadelBotDifficulty>,
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
        let mut location = (stop as usize..end as usize)
            .find_map(|at| party_load(image, at, end as usize))
            .ok_or(Error::AnchorNotFound("party load in the stop function"))?;
        location.modes = mode_offsets(image, location.start_offset);
        Ok(location)
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
        let start = self.location.start_offset;
        let modes = self.location.modes;
        let fields = [
            Some(start),
            modes.map(|m| m.match_mode),
            modes.map(|m| m.game_mode),
            modes.map(|m| m.bot_difficulty),
        ];
        let fields = fields.iter().flatten();
        let low = fields.clone().copied().min().unwrap_or(start);
        let high = fields.copied().max().unwrap_or(start) + 4;
        let Ok(bytes) = mem.read_bytes(party + u64::from(low), (high - low) as usize) else {
            return Ok(None);
        };
        let word = |offset: u32| {
            let at = (offset - low) as usize;
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let start_unix = word(start);
        let modes = modes.filter(|_| start_unix != 0);
        Ok(Some(PartyQueueState {
            start_unix,
            match_mode: modes.and_then(|m| known(word(m.match_mode))),
            game_mode: modes.and_then(|m| known(word(m.game_mode))),
            bot_difficulty: modes.and_then(|m| known(word(m.bot_difficulty))),
        }))
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
        modes: None,
    })
}

/// The mode offsets in the party object, from the `CSOCitadelParty` serializer. It loads
/// each field just before writing its wire tag, so the offsets are found by field number and
/// survive a layout change. Each is moved by the distance between the start time's offset in
/// the message and `start_offset` in the party object.
fn mode_offsets(image: &PeImage, start_offset: u32) -> Option<PartyModeOffsets> {
    let vtable = VtableResolver::new(image).vtable(PARTY_MESSAGE).ok()?;
    let at = usize::try_from(vtable.checked_sub(image.base())?).ok()?;
    let bytes = image.bytes();
    (0..VTABLE_SLOTS)
        .map_while(|i| {
            let slot = bytes.get(at + i * 8..at + i * 8 + 8)?;
            let target = u64::from_le_bytes(slot.try_into().ok()?);
            image.is_code_address(target).then(|| target - image.base())
        })
        .find_map(|rva| serialized_offsets(image, u32::try_from(rva).ok()?, start_offset))
}

fn serialized_offsets(image: &PeImage, rva: u32, start_offset: u32) -> Option<PartyModeOffsets> {
    let (begin, end) = image.function_containing(rva)?;
    let code = image.bytes().get(begin as usize..end as usize)?;
    let shift = start_offset.checked_sub(field_load(code, START_TAG)?)?;
    let offset = |tag| {
        let offset = field_load(code, tag)?.checked_add(shift)?;
        (offset.abs_diff(start_offset) <= MAX_FIELD_DISTANCE).then_some(offset)
    };
    Some(PartyModeOffsets {
        match_mode: offset(MATCH_MODE_TAG)?,
        game_mode: offset(GAME_MODE_TAG)?,
        bot_difficulty: offset(BOT_DIFFICULTY_TAG)?,
    })
}

/// The displacement of the 32-bit field load before the one `mov byte [reg], tag` in `code`.
fn field_load(code: &[u8], tag: u8) -> Option<u32> {
    let mut loads = (0..code.len().saturating_sub(2))
        .filter(|&at| {
            // mod 00, reg 000; rm 100 needs a SIB byte and rm 101 is rip-relative.
            code[at] == 0xc6
                && code[at + 2] == tag
                && code[at + 1] & 0xf8 == 0
                && !matches!(code[at + 1], 4 | 5)
        })
        .filter_map(|write| {
            (write.saturating_sub(TAG_WINDOW)..=write)
                .rev()
                .find_map(|end| load_ending(code, end))
        });
    let disp = loads.next()?;
    loads.next().is_none().then_some(disp)
}

/// The displacement of a `mov r32, [reg+disp32]` or `movsxd r64, [reg+disp32]` that ends
/// at `end`.
fn load_ending(code: &[u8], end: usize) -> Option<u32> {
    let disp = u32::from_le_bytes(code.get(end.checked_sub(4)?..end)?.try_into().ok()?);
    // mod 10 is a disp32 base; rm 100 would need a SIB byte.
    let disp32_base = |modrm: u8| modrm & 0xc0 == 0x80 && modrm & 0x07 != 4;
    let modrm = *code.get(end.checked_sub(5)?)?;
    let opcode = *code.get(end.checked_sub(6)?)?;
    let prefix = end.checked_sub(7).and_then(|at| code.get(at)).copied();
    let rex_w = prefix.is_some_and(|p| p & 0xf8 == 0x48);
    let loads = match opcode {
        0x8b => !rex_w,
        0x63 => rex_w,
        _ => false,
    };
    (loads && disp32_base(modrm)).then_some(disp)
}

/// A raw enum value as `E`, or `None` for a value this build does not know.
fn known<E: TryFrom<i32>>(raw: u32) -> Option<E> {
    E::try_from(i32::try_from(raw).ok()?).ok()
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
