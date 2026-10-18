//! Party queue lookup tests over a hand-built client image.

use deadlock_memory::mock::MockMemory;
use deadlock_walker::{
    Error, PartyLocation, PartyModeOffsets, PartyQueue, PartyQueueState, PeImage,
};
use valveprotos::deadlock::{ECitadelBotDifficulty, ECitadelGameMode, ECitadelMatchMode};

const BASE: u64 = 0x7ffa_2000_0000;
const SIZE: usize = 0x6000;
const PID: u32 = 4242;

const TEXT: usize = 0x1000;
const RDATA: usize = 0x2000;
const DATA: usize = 0x3000;
const PDATA: usize = 0x5000;

const STRING_AT: usize = 0x2100;
const CONTROLLER: usize = 0x3040;
const OBJECT: usize = 0x3100;
const POPUP: usize = 0x1100;
const POPUP_END: usize = 0x1180;
const STOP: usize = 0x1200;
const STOP_END: usize = 0x1300;
const GETTER: usize = 0x1400;
const DECOY_GETTER: usize = 0x1440;
const CHECK: usize = 0x1480;
const DECOY_SLOT: usize = 0x14c0;
const SERIALIZER: usize = 0x1500;
const SERIALIZER_END: usize = 0x1700;
const PARTY_LOCATOR: usize = 0x2200;
const PARTY_VTABLE: usize = 0x2300;
const PARTY_TYPE: usize = 0x3200;
const PARTY_CLASS: &[u8] = b".?AVCSOCitadelParty@@\0";
/// Where the party object keeps its `CSOCitadelParty`.
const PROTO_AT: u32 = 8;
const NEEDLE: &[u8] = b"#Citadel_Popup_AlreadySearching_Header\0";

const PARTY: u64 = 0x2_0000_0000;

#[derive(Clone, Copy)]
struct Build {
    /// REX prefix and `ModRM` byte of the `mov reg, [base+disp32]` after the getter call.
    load: (u8, u8),
    slot_disp: u32,
    start_offset: u32,
    getter_lea: bool,
    check_seta: bool,
    stop_pdata: bool,
    decoy_first: bool,
    /// Offsets in the party object the serializer is built to read; `None` builds no
    /// serializer.
    modes: Option<PartyModeOffsets>,
}

const MODES: PartyModeOffsets = PartyModeOffsets {
    match_mode: 0xb4,
    game_mode: 0xb8,
    bot_difficulty: 0xb0,
};

const FULL: Build = Build {
    load: (0x48, 0xb8),
    slot_disp: 0x630,
    start_offset: 0xbc,
    getter_lea: true,
    check_seta: true,
    stop_pdata: true,
    decoy_first: false,
    modes: Some(MODES),
};

/// Proto fields 7 and 9-11 moved: no offset sits where `FULL` has it.
const REORDERED: Build = Build {
    start_offset: 0xa8,
    modes: Some(PartyModeOffsets {
        match_mode: 0xc0,
        game_mode: 0xac,
        bot_difficulty: 0xcc,
    }),
    ..FULL
};

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn rel(from_end: usize, target: usize) -> u32 {
    (target as i64 - from_end as i64) as i32 as u32
}

fn call(b: &mut [u8], at: usize, target: usize) -> usize {
    b[at] = 0xe8;
    put_u32(b, at + 1, rel(at + 5, target));
    at + 5
}

fn image(build: Build) -> Vec<u8> {
    let mut b = vec![0u8; SIZE];
    b[0..2].copy_from_slice(b"MZ");
    let nt = 0x80;
    put_u32(&mut b, 0x3c, nt as u32);
    b[nt..nt + 4].copy_from_slice(b"PE\0\0");
    b[nt + 4..nt + 6].copy_from_slice(&0x8664u16.to_le_bytes());
    b[nt + 6..nt + 8].copy_from_slice(&4u16.to_le_bytes());
    let opt_size = 0xf0;
    b[nt + 20..nt + 22].copy_from_slice(&(opt_size as u16).to_le_bytes());
    let opt = nt + 24;
    b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    put_u32(&mut b, opt + 56, SIZE as u32);
    put_u32(&mut b, opt + 112 + 3 * 8, PDATA as u32);
    let mut sh = opt + opt_size;
    for (name, rva, size, flags) in [
        (".text", TEXT, 0x1000u32, 0x6000_0020u32),
        (".rdata", RDATA, 0x1000, 0x4000_0040),
        (".data", DATA, 0x2000, 0xc000_0040),
        (".pdata", PDATA, 0x100, 0x4000_0040),
    ] {
        b[sh..sh + name.len()].copy_from_slice(name.as_bytes());
        put_u32(&mut b, sh + 8, size);
        put_u32(&mut b, sh + 12, rva as u32);
        put_u32(&mut b, sh + 36, flags);
        sh += 40;
    }
    b[TEXT..TEXT + 0x1000].fill(0xcc);
    b[STRING_AT..STRING_AT + NEEDLE.len()].copy_from_slice(NEEDLE);

    let mut at = POPUP;
    b[at..at + 3].copy_from_slice(&[0x48, 0x8d, 0x15]);
    put_u32(&mut b, at + 3, rel(at + 7, STRING_AT));
    at += 7;
    b[at..at + 3].copy_from_slice(&[0x48, 0x8d, 0x0d]);
    put_u32(&mut b, at + 3, rel(at + 7, CONTROLLER));
    at = call(&mut b, at + 7, STOP);
    b[at] = 0xc3;

    let mut at = STOP;
    b[at..at + 5].copy_from_slice(&[0x48, 0x89, 0x5c, 0x24, 0x08]);
    at += 5;
    // A load through a disp8 is not the slot read.
    at = call(&mut b, at, CHECK);
    b[at..at + 4].copy_from_slice(&[0x48, 0x8b, 0x48, 0x08]);
    at += 4;
    if build.decoy_first {
        at = call(&mut b, at, DECOY_GETTER);
        b[at..at + 3].copy_from_slice(&[0x48, 0x8b, 0x80]);
        put_u32(&mut b, at + 3, 0x40);
        at += 7;
    }
    at = call(&mut b, at, GETTER);
    let (rex, modrm) = build.load;
    b[at..at + 3].copy_from_slice(&[rex, 0x8b, modrm]);
    put_u32(&mut b, at + 3, build.slot_disp);
    at += 7;
    // test rdi, rdi; je +0x10; mov rcx, rdi
    b[at..at + 8].copy_from_slice(&[0x48, 0x85, 0xff, 0x74, 0x10, 0x48, 0x8b, 0xcf]);
    at = call(&mut b, at + 8, CHECK);
    b[at] = 0xc3;

    b[GETTER..GETTER + 3].copy_from_slice(&[
        0x48,
        if build.getter_lea { 0x8d } else { 0x8b },
        0x05,
    ]);
    put_u32(&mut b, GETTER + 3, rel(GETTER + 7, OBJECT));
    b[GETTER + 7] = 0xc3;
    b[DECOY_GETTER..DECOY_GETTER + 3].copy_from_slice(&[0x48, 0x8b, 0x05]);
    put_u32(&mut b, DECOY_GETTER + 3, rel(DECOY_GETTER + 7, OBJECT));
    b[DECOY_GETTER + 7] = 0xc3;

    b[CHECK..CHECK + 2].copy_from_slice(&[0x83, 0xb9]);
    put_u32(&mut b, CHECK + 2, build.start_offset);
    let setcc = if build.check_seta { 0x97 } else { 0x94 };
    b[CHECK + 6..CHECK + 11].copy_from_slice(&[0x00, 0x0f, setcc, 0xc0, 0xc3]);

    party_class(&mut b, build);

    let mut functions = vec![(POPUP, POPUP_END)];
    if build.stop_pdata {
        functions.push((STOP, STOP_END));
    }
    functions.extend([
        (GETTER, GETTER + 8),
        (DECOY_GETTER, DECOY_GETTER + 8),
        (CHECK, CHECK + 11),
        (DECOY_SLOT, DECOY_SLOT + 3),
        (SERIALIZER, SERIALIZER_END),
    ]);
    for (i, (start, end)) in functions.iter().enumerate() {
        let e = PDATA + i * 12;
        put_u32(&mut b, e, *start as u32);
        put_u32(&mut b, e + 4, *end as u32);
        put_u32(&mut b, e + 8, 0x2800);
    }
    put_u32(&mut b, opt + 112 + 3 * 8 + 4, (functions.len() * 12) as u32);
    b
}

/// RTTI for `CSOCitadelParty`, a vtable whose first slot is not the serializer, and the
/// serializer when the build has one.
fn party_class(b: &mut [u8], build: Build) {
    b[PARTY_TYPE + 16..PARTY_TYPE + 16 + PARTY_CLASS.len()].copy_from_slice(PARTY_CLASS);
    put_u32(b, PARTY_LOCATOR, 1);
    put_u32(b, PARTY_LOCATOR + 12, PARTY_TYPE as u32);
    put_u32(b, PARTY_LOCATOR + 20, PARTY_LOCATOR as u32);
    b[PARTY_VTABLE..PARTY_VTABLE + 8].copy_from_slice(&(BASE + PARTY_LOCATOR as u64).to_le_bytes());
    b[DECOY_SLOT..DECOY_SLOT + 3].copy_from_slice(&[0xb0, 0x01, 0xc3]);
    let mut slots = vec![DECOY_SLOT];
    if let Some(modes) = build.modes {
        serializer(b, build.start_offset, modes);
        slots.push(SERIALIZER);
    }
    for (i, slot) in slots.iter().enumerate() {
        let at = PARTY_VTABLE + 8 + i * 8;
        b[at..at + 8].copy_from_slice(&(BASE + *slot as u64).to_le_bytes());
    }
}

/// The shape protobuf-cpp emits for `_InternalSerialize`: per field, a has-bit test, a
/// space check, the load from `this` (rbp), then the tag byte written to the stream (rbx).
fn serializer(b: &mut [u8], start_offset: u32, modes: PartyModeOffsets) {
    let proto = |offset: u32| offset - PROTO_AT;
    let mut at = SERIALIZER;
    let mut put = |b: &mut [u8], code: &[u8]| {
        b[at..at + code.len()].copy_from_slice(code);
        at += code.len();
    };
    put(b, &[0x48, 0x89, 0x5c, 0x24, 0x10, 0x48, 0x8b, 0xe9]);
    // party_id: a 64-bit load, tag 0x08.
    put(
        b,
        &[
            0x48, 0x8b, 0x8d, 0x98, 0, 0, 0, 0x48, 0x8d, 0x53, 0x01, 0xc6, 0x03, 0x08,
        ],
    );
    for (tag, offset) in [
        (0x38, modes.bot_difficulty),
        (0x48, modes.match_mode),
        (0x50, modes.game_mode),
    ] {
        put(b, &[0x48, 0x3b, 0x1f, 0x72, 0x0e, 0x48, 0x63, 0x85]);
        put(b, &proto(offset).to_le_bytes());
        put(
            b,
            &[
                0x48, 0x8d, 0x53, 0x01, 0xc6, 0x03, tag, 0x48, 0x8b, 0xc8, 0x88, 0x0a,
            ],
        );
    }
    put(b, &[0x48, 0x3b, 0x1f, 0x72, 0x0e, 0x8b, 0x8d]);
    put(b, &proto(start_offset).to_le_bytes());
    put(
        b,
        &[0x48, 0x8d, 0x53, 0x01, 0xc6, 0x03, 0x58, 0x88, 0x0a, 0xc3],
    );
    assert!(at <= SERIALIZER_END);
}

fn located(build: Build) -> Result<PartyLocation, Error> {
    PartyQueue::locate(&PeImage::from_bytes(BASE, image(build)).unwrap())
}

fn mem(build: Build) -> MockMemory {
    let mut m = MockMemory::new(PID);
    m.write(BASE, &image(build));
    m.add_module("client.dll", BASE, SIZE);
    m
}

fn slot(build: Build) -> u64 {
    BASE + (OBJECT + build.slot_disp as usize) as u64
}

fn join(m: &mut MockMemory, build: Build, start: u32) {
    search(m, build, start, (0, 0, 0));
}

/// Join with raw `(match_mode, game_mode, bot_difficulty)`.
///
/// The party is written as one segment: the mock serves a read from a single segment, so
/// a field written on top would hide from a read spanning its neighbours.
fn search(m: &mut MockMemory, build: Build, start: u32, raw: (u32, u32, u32)) {
    let mut party = vec![0u8; 0x100];
    put_u32(&mut party, build.start_offset as usize, start);
    if let Some(modes) = build.modes {
        put_u32(&mut party, modes.match_mode as usize, raw.0);
        put_u32(&mut party, modes.game_mode as usize, raw.1);
        put_u32(&mut party, modes.bot_difficulty as usize, raw.2);
    }
    m.write_u64(slot(build), PARTY);
    m.write(PARTY, &party);
}

fn idle(start_unix: u32) -> PartyQueueState {
    PartyQueueState {
        start_unix,
        match_mode: None,
        game_mode: None,
        bot_difficulty: None,
    }
}

/// Searching with every mode field zero, as `join` leaves them.
fn zero_modes(start_unix: u32) -> PartyQueueState {
    PartyQueueState {
        start_unix,
        match_mode: Some(ECitadelMatchMode::KECitadelMatchModeInvalid),
        game_mode: Some(ECitadelGameMode::KECitadelGameModeInvalid),
        bot_difficulty: Some(ECitadelBotDifficulty::KECitadelBotDifficultyNone),
    }
}

#[test]
fn finds_the_slot_the_stop_function_reads_through_the_getter() {
    assert_eq!(
        located(FULL).unwrap(),
        PartyLocation {
            slot: BASE + (OBJECT + 0x630) as u64,
            start_offset: 0xbc,
            modes: Some(MODES),
        }
    );
}

#[test]
fn offsets_come_from_the_code() {
    let build = Build {
        slot_disp: 0x5a8,
        start_offset: 0xc4,
        ..FULL
    };
    assert_eq!(
        located(build).unwrap(),
        PartyLocation {
            slot: BASE + (OBJECT + 0x5a8) as u64,
            start_offset: 0xc4,
            modes: Some(MODES),
        }
    );
}

#[test]
fn a_load_into_an_extended_register_counts() {
    let build = Build {
        load: (0x4c, 0xb0),
        ..FULL
    };
    assert_eq!(located(build).unwrap().slot, slot(FULL));
}

#[test]
fn a_load_through_another_base_register_has_no_anchor() {
    let build = Build {
        load: (0x48, 0xb9),
        ..FULL
    };
    assert!(matches!(located(build), Err(Error::AnchorNotFound(_))));
}

#[test]
fn a_getter_that_is_not_lea_ret_has_no_anchor() {
    let build = Build {
        getter_lea: false,
        ..FULL
    };
    assert!(matches!(located(build), Err(Error::AnchorNotFound(_))));
}

#[test]
fn a_decoy_load_through_a_non_lea_getter_is_skipped() {
    let build = Build {
        decoy_first: true,
        ..FULL
    };
    assert_eq!(located(build).unwrap().slot, slot(FULL));
}

#[test]
fn a_start_check_that_is_not_cmp_seta_has_no_anchor() {
    let build = Build {
        check_seta: false,
        ..FULL
    };
    assert!(matches!(located(build), Err(Error::AnchorNotFound(_))));
}

#[test]
fn a_stop_function_the_exception_table_misses_has_no_anchor() {
    let build = Build {
        stop_pdata: false,
        ..FULL
    };
    assert!(matches!(located(build), Err(Error::AnchorNotFound(_))));
}

#[test]
fn no_party_reads_as_none() {
    let m = mem(FULL);
    let queue = PartyQueue::new(&m).unwrap();
    assert_eq!(queue.read(&m).unwrap(), None);
}

#[test]
fn reads_the_start_time_as_it_changes() {
    let mut m = mem(FULL);
    let queue = PartyQueue::new(&m).unwrap();
    join(&mut m, FULL, 0);
    assert_eq!(queue.read(&m).unwrap(), Some(idle(0)));
    join(&mut m, FULL, 1_760_700_000);
    assert_eq!(queue.read(&m).unwrap(), Some(zero_modes(1_760_700_000)));
    m.write_u64(slot(FULL), 0);
    assert_eq!(queue.read(&m).unwrap(), None);
}

#[test]
fn an_unreadable_party_reads_as_none() {
    let mut m = mem(FULL);
    let queue = PartyQueue::new(&m).unwrap();
    m.write_u64(slot(FULL), PARTY);
    assert_eq!(queue.read(&m).unwrap(), None);
}

#[test]
fn mode_offsets_come_from_the_party_serializer() {
    assert_eq!(located(FULL).unwrap().modes, Some(MODES));
}

#[test]
fn reordered_mode_fields_are_found_by_field_number() {
    let location = located(REORDERED).unwrap();
    assert_eq!(location.start_offset, 0xa8);
    assert_eq!(location.modes, REORDERED.modes);
}

#[test]
fn reads_the_queued_modes_while_searching() {
    for build in [FULL, REORDERED] {
        let mut m = mem(build);
        let queue = PartyQueue::new(&m).unwrap();
        search(&mut m, build, 1_760_700_000, (4, 1, 0));
        assert_eq!(
            queue.read(&m).unwrap(),
            Some(PartyQueueState {
                start_unix: 1_760_700_000,
                match_mode: Some(ECitadelMatchMode::KECitadelMatchModeRanked),
                game_mode: Some(ECitadelGameMode::KECitadelGameModeNormal),
                bot_difficulty: Some(ECitadelBotDifficulty::KECitadelBotDifficultyNone),
            })
        );
    }
}

#[test]
fn a_party_that_is_not_searching_has_no_modes() {
    let mut m = mem(FULL);
    let queue = PartyQueue::new(&m).unwrap();
    search(&mut m, FULL, 0, (4, 4, 2));
    assert_eq!(queue.read(&m).unwrap(), Some(idle(0)));
}

#[test]
fn an_unknown_raw_mode_reads_as_none() {
    let mut m = mem(FULL);
    let queue = PartyQueue::new(&m).unwrap();
    search(&mut m, FULL, 5, (99, 4, u32::MAX));
    assert_eq!(
        queue.read(&m).unwrap(),
        Some(PartyQueueState {
            start_unix: 5,
            match_mode: None,
            game_mode: Some(ECitadelGameMode::KECitadelGameModeStreetBrawl),
            bot_difficulty: None,
        })
    );
}

#[test]
fn a_build_without_the_party_serializer_still_reads_the_start() {
    let build = Build {
        modes: None,
        ..FULL
    };
    assert_eq!(located(build).unwrap().modes, None);
    let mut m = mem(build);
    let queue = PartyQueue::new(&m).unwrap();
    join(&mut m, build, 11);
    assert_eq!(queue.read(&m).unwrap(), Some(idle(11)));
}

#[test]
fn a_read_is_two_small_reads() {
    let mut m = mem(FULL);
    let queue = PartyQueue::new(&m).unwrap();
    search(&mut m, FULL, 7, (1, 1, 0));
    m.reset_reads();
    queue.read(&m).unwrap();
    assert_eq!(m.reads(), 2);
}

#[test]
fn another_process_is_refused() {
    let m = mem(FULL);
    let queue = PartyQueue::new(&m).unwrap();
    let mut other = MockMemory::new(PID + 1);
    other.write(BASE, &image(FULL));
    assert!(matches!(queue.read(&other), Err(Error::WrongProcess)));
}

#[test]
fn a_queue_built_from_a_copied_image_reads_nothing_more() {
    let mut m = mem(FULL);
    let client = deadlock_memory::mem::MemoryReader::module(&m, "client.dll").unwrap();
    let image = PeImage::read(&m, &client).unwrap();
    m.reset_reads();
    let queue = PartyQueue::from_image(&m, &image).unwrap();
    assert_eq!(m.reads(), 0);
    assert_eq!(queue.location(), located(FULL).unwrap());
    join(&mut m, FULL, 9);
    assert_eq!(queue.read(&m).unwrap(), Some(zero_modes(9)));
}

#[test]
fn a_queue_from_another_build_has_no_anchor() {
    let m = mem(FULL);
    let image = PeImage::from_bytes(
        BASE,
        image(Build {
            getter_lea: false,
            ..FULL
        }),
    )
    .unwrap();
    assert!(matches!(
        PartyQueue::from_image(&m, &image),
        Err(Error::AnchorNotFound(_))
    ));
}
