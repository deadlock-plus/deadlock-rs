//! Queue flag lookup tests over a hand-built client image.

use deadlock_memory::mock::MockMemory;
use deadlock_walker::{Error, QueueFlag, QueueRequest};

const BASE: u64 = 0x7ffa_2000_0000;
const SIZE: usize = 0x6000;
const PID: u32 = 4242;

const TEXT: usize = 0x1000;
const RDATA: usize = 0x2000;
const DATA: usize = 0x3000;
const PDATA: usize = 0x4000;

const STRING_AT: usize = 0x2100;
const DECOY_FLAG: usize = 0x3020;
const FLAG: usize = 0x3010;
const LATER_FLAG: usize = 0x3030;
const SINGLETON: usize = 0x3040;
const DECOY_SINGLETON: usize = 0x3060;
const GUARD: usize = 0x3080;
const DECOY_FN: usize = 0x1000;
const FUNCTION: usize = 0x1100;
const FUNCTION_END: usize = 0x1200;
const NEEDLE: &[u8] = b"#Citadel_Popup_AlreadySearching_Header\0";

#[derive(Clone, Copy)]
struct Build {
    string: bool,
    lea: bool,
    pdata: bool,
    cmp: bool,
    singleton: bool,
}

const FULL: Build = Build {
    string: true,
    lea: true,
    pdata: true,
    cmp: true,
    singleton: true,
};

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn rel(from_end: usize, target: usize) -> u32 {
    (target as i64 - from_end as i64) as i32 as u32
}

fn cmp_byte(b: &mut [u8], at: usize, target: usize) {
    b[at..at + 2].copy_from_slice(&[0x80, 0x3d]);
    put_u32(b, at + 2, rel(at + 7, target));
    b[at + 6] = 0;
}

fn lea_rcx_call(b: &mut [u8], at: usize, target: usize) -> usize {
    b[at..at + 3].copy_from_slice(&[0x48, 0x8d, 0x0d]);
    put_u32(b, at + 3, rel(at + 7, target));
    b[at + 7] = 0xe8;
    at + 12
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
    if build.pdata {
        put_u32(&mut b, opt + 112 + 3 * 8, PDATA as u32);
        put_u32(&mut b, opt + 112 + 3 * 8 + 4, 24);
    }
    let mut sh = opt + opt_size;
    for (name, rva, size, flags) in [
        (".text", TEXT, 0x1000u32, 0x6000_0020u32),
        (".rdata", RDATA, 0x1000, 0x4000_0040),
        (".data", DATA, 0x1000, 0xc000_0040),
        (".pdata", PDATA, 0x100, 0x4000_0040),
    ] {
        b[sh..sh + name.len()].copy_from_slice(name.as_bytes());
        put_u32(&mut b, sh + 8, size);
        put_u32(&mut b, sh + 12, rva as u32);
        put_u32(&mut b, sh + 36, flags);
        sh += 40;
    }
    b[TEXT..TEXT + 0x1000].fill(0xcc);

    if build.string {
        b[STRING_AT..STRING_AT + NEEDLE.len()].copy_from_slice(NEEDLE);
    }
    cmp_byte(&mut b, DECOY_FN + 0x10, DECOY_FLAG);

    b[FUNCTION..FUNCTION + 6].copy_from_slice(&[0x40, 0x53, 0x48, 0x83, 0xec, 0x20]);
    let mut at = lea_rcx_call(&mut b, FUNCTION + 6, DECOY_SINGLETON);
    if build.cmp {
        cmp_byte(&mut b, at, FLAG);
        at += 7;
        b[at..at + 2].copy_from_slice(&[0x74, 0x10]);
        at += 2;
    }
    if build.lea {
        b[at..at + 3].copy_from_slice(&[0x48, 0x8d, 0x15]);
        put_u32(&mut b, at + 3, rel(at + 7, STRING_AT));
        at += 7;
    }
    if build.cmp {
        cmp_byte(&mut b, at, LATER_FLAG);
        at += 7;
    }
    if build.singleton {
        at = lea_rcx_call(&mut b, at, SINGLETON);
    }
    b[at] = 0xc3;
    at += 1;
    b[at..at + 2].copy_from_slice(&[0x83, 0x3d]);
    put_u32(&mut b, at + 2, rel(at + 7, GUARD));
    b[at + 6] = 0xff;
    lea_rcx_call(&mut b, at + 7, GUARD);

    if build.pdata {
        for (i, (start, end)) in [(DECOY_FN, FUNCTION), (FUNCTION, FUNCTION_END)]
            .into_iter()
            .enumerate()
        {
            let e = PDATA + i * 12;
            put_u32(&mut b, e, start as u32);
            put_u32(&mut b, e + 4, end as u32);
            put_u32(&mut b, e + 8, 0x2800);
        }
    }
    b
}

fn mem_of(bytes: &[u8]) -> MockMemory {
    let mut m = MockMemory::new(PID);
    m.write(BASE, bytes);
    m.add_module("client.dll", BASE, SIZE);
    m
}

fn mem(build: Build) -> MockMemory {
    mem_of(&image(build))
}

fn set_flag(m: &mut MockMemory, value: u8) {
    m.write(BASE + FLAG as u64, &[value]);
}

#[test]
fn finds_the_first_compare_of_the_function_that_uses_the_string() {
    let m = mem(FULL);
    let flag = QueueFlag::new(&m).unwrap();
    assert_eq!(flag.address(), BASE + FLAG as u64);
}

#[test]
fn reads_the_flag_as_it_changes() {
    let mut m = mem(FULL);
    let mut flag = QueueFlag::new(&m).unwrap();
    assert_eq!(flag.queueing(&m).unwrap(), Some(false));
    set_flag(&mut m, 1);
    assert_eq!(flag.queueing(&m).unwrap(), Some(true));
    set_flag(&mut m, 0);
    assert_eq!(flag.queueing(&m).unwrap(), Some(false));
}

#[test]
fn a_build_without_the_string_has_no_anchor() {
    let m = mem(Build {
        string: false,
        ..FULL
    });
    assert!(matches!(QueueFlag::new(&m), Err(Error::AnchorNotFound(_))));
}

#[test]
fn a_string_nothing_loads_has_no_anchor() {
    let m = mem(Build { lea: false, ..FULL });
    assert!(matches!(QueueFlag::new(&m), Err(Error::AnchorNotFound(_))));
}

#[test]
fn a_loader_outside_every_known_function_has_no_anchor() {
    let m = mem(Build {
        pdata: false,
        ..FULL
    });
    assert!(matches!(QueueFlag::new(&m), Err(Error::AnchorNotFound(_))));
}

#[test]
fn a_function_without_the_compare_has_no_anchor() {
    let m = mem(Build { cmp: false, ..FULL });
    assert!(matches!(QueueFlag::new(&m), Err(Error::AnchorNotFound(_))));
}

fn set_request(m: &mut MockMemory, words: [u32; 3]) {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    m.write(BASE + (SINGLETON + 8) as u64, &bytes);
}

#[test]
fn finds_the_singleton_the_function_hands_to_its_callee() {
    let m = mem(FULL);
    let flag = QueueFlag::new(&m).unwrap();
    assert_eq!(flag.request_address(), Some(BASE + SINGLETON as u64));
}

#[test]
fn the_request_is_the_three_raw_mode_words_while_searching() {
    let mut m = mem(FULL);
    let mut flag = QueueFlag::new(&m).unwrap();
    set_request(&mut m, [1, 4, 0]);
    set_flag(&mut m, 1);
    assert_eq!(
        flag.request(&m).unwrap(),
        Some(QueueRequest {
            match_mode: 1,
            game_mode: 4,
            bot_difficulty: 0
        })
    );
    set_request(&mut m, [2, 1, 2]);
    assert_eq!(
        flag.request(&m).unwrap(),
        Some(QueueRequest {
            match_mode: 2,
            game_mode: 1,
            bot_difficulty: 2
        })
    );
}

#[test]
fn unknown_values_stay_raw_numbers() {
    let mut m = mem(FULL);
    let mut flag = QueueFlag::new(&m).unwrap();
    set_request(&mut m, [77, 88, 99]);
    set_flag(&mut m, 1);
    assert_eq!(
        flag.request(&m).unwrap(),
        Some(QueueRequest {
            match_mode: 77,
            game_mode: 88,
            bot_difficulty: 99
        })
    );
}

#[test]
fn no_request_while_not_searching() {
    let mut m = mem(FULL);
    let mut flag = QueueFlag::new(&m).unwrap();
    set_request(&mut m, [1, 4, 0]);
    assert_eq!(flag.request(&m).unwrap(), None);
}

#[test]
fn a_build_without_the_singleton_still_reads_the_flag() {
    let mut m = mem(Build {
        singleton: false,
        ..FULL
    });
    let mut flag = QueueFlag::new(&m).unwrap();
    assert_eq!(flag.request_address(), None);
    set_flag(&mut m, 1);
    assert_eq!(flag.queueing(&m).unwrap(), Some(true));
    assert_eq!(flag.request(&m).unwrap(), None);
}

#[test]
fn an_unreadable_singleton_has_no_request() {
    let mut m = mem(FULL);
    set_flag(&mut m, 1);
    let mut flag = QueueFlag::new(&m).unwrap();
    let mut short = MockMemory::new(PID);
    short.write(BASE, &image(FULL)[..SINGLETON]);
    short.write(BASE + FLAG as u64, &[1]);
    assert_eq!(flag.request(&short).unwrap(), None);
}

#[test]
fn a_request_for_another_process_is_refused() {
    let m = mem(FULL);
    let mut flag = QueueFlag::new(&m).unwrap();
    let mut other = MockMemory::new(PID + 1);
    other.write(BASE, &image(FULL));
    assert!(matches!(flag.request(&other), Err(Error::WrongProcess)));
}

#[test]
fn another_process_is_refused() {
    let m = mem(FULL);
    let mut flag = QueueFlag::new(&m).unwrap();
    let mut other = MockMemory::new(PID + 1);
    other.write(BASE, &image(FULL));
    assert!(matches!(flag.queueing(&other), Err(Error::WrongProcess)));
}

#[test]
fn a_poll_is_one_read_and_never_looks_for_the_anchor_again() {
    let mut flag = QueueFlag::new(&mem(FULL)).unwrap();
    let later = mem(Build {
        string: false,
        ..FULL
    });
    assert_eq!(flag.queueing(&later).unwrap(), Some(false));
    later.reset_reads();
    assert_eq!(flag.queueing(&later).unwrap(), Some(false));
    assert_eq!(later.reads(), 1);
}

#[test]
fn an_unreadable_flag_is_unknown() {
    let m = mem(FULL);
    let mut flag = QueueFlag::new(&m).unwrap();
    let mut gone = MockMemory::new(PID);
    gone.write(BASE, &image(FULL)[..DATA]);
    assert_eq!(flag.queueing(&gone).unwrap(), None);
}

#[allow(dead_code)]
const _SECTIONS: [usize; 2] = [TEXT, RDATA];
