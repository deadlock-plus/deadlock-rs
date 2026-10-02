use std::path::PathBuf;

use prost::Message;
use valveprotos::common::{CDemoFileHeader, EDemoCommands};

use super::*;

fn varint(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// A demo whose commands are `(command, tick, payload)`, uncompressed.
fn synthetic_demo(commands: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"PBDEMS2\0".to_vec();
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    for (command, tick, payload) in commands {
        varint(&mut out, *command);
        varint(&mut out, *tick);
        varint(&mut out, u32::try_from(payload.len()).unwrap());
        out.extend_from_slice(payload);
    }
    out
}

fn file_header(map: &str, game_directory: &str) -> Vec<u8> {
    CDemoFileHeader {
        map_name: Some(map.to_owned()),
        game_directory: Some(game_directory.to_owned()),
        ..CDemoFileHeader::default()
    }
    .encode_to_vec()
}

const FILE_HEADER: u32 = EDemoCommands::DemFileHeader as u32;
const CONSOLE_CMD: u32 = EDemoCommands::DemConsoleCmd as u32;

struct TempDemo(PathBuf);

impl TempDemo {
    fn new(name: &str, bytes: &[u8]) -> TempDemo {
        let path =
            std::env::temp_dir().join(format!("deadlock-replay-{}-{name}.dem", std::process::id()));
        std::fs::write(&path, bytes).expect("could not write the fixture");
        TempDemo(path)
    }
}

impl Drop for TempDemo {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn padded(count: u32) -> Vec<(u32, u32, Vec<u8>)> {
    (0..count)
        .map(|i| (CONSOLE_CMD, i, vec![0xab; 4096]))
        .collect()
}

#[test]
fn a_synthetic_demo_reports_its_header_and_commands() {
    let f = TempDemo::new(
        "commands",
        &synthetic_demo(&[
            (
                FILE_HEADER,
                0,
                file_header("dl_test", "/opt/srcds/deadlock/citadel_v6670/citadel"),
            ),
            (CONSOLE_CMD, 1, vec![0; 16]),
            (CONSOLE_CMD, 2, vec![0; 16]),
        ]),
    );
    let s = scan(&f.0, DemoLimits::default(), None).unwrap();
    let c = s.census;
    assert_eq!(c.packets, 3);
    assert_eq!(c.compressed_packets, 0);
    assert!(!c.hit_budget);
    assert_eq!(c.by_command[&EDemoCommands::DemFileHeader], 1);
    assert_eq!(c.by_command[&EDemoCommands::DemConsoleCmd], 2);
    let header = c.file_header.unwrap();
    assert_eq!(header.map_name.as_deref(), Some("dl_test"));
    assert_eq!(server_build(&header), Some("citadel_v6670"));
    assert_eq!(s.search, None);
}

#[test]
fn bytes_consumed_counts_the_prelude_and_every_frame() {
    let bytes = synthetic_demo(&padded(3));
    let f = TempDemo::new("consumed", &bytes);
    let c = scan(&f.0, DemoLimits::whole_file(), None).unwrap().census;
    assert_eq!(c.bytes_consumed, bytes.len() as u64);
    assert_eq!(c.decompressed_bytes, 3 * 4096);
    assert_eq!(c.largest_packet_bytes, 4096);
}

#[test]
fn the_cap_bounds_what_is_read() {
    let bytes = synthetic_demo(&padded(40));
    let total = bytes.len() as u64;
    let f = TempDemo::new("capped", &bytes);

    let capped = scan(
        &f.0,
        DemoLimits {
            max_total_bytes: 64 * 1024,
        },
        None,
    )
    .unwrap()
    .census;
    assert!(
        capped.hit_budget,
        "a capped scan of a larger file must say so"
    );
    assert!(
        capped.bytes_consumed <= 64 * 1024,
        "read {} bytes under a 65536-byte cap",
        capped.bytes_consumed
    );
    assert!(
        capped.packets > 0 && capped.packets < 40,
        "{}",
        capped.packets
    );

    let whole = scan(&f.0, DemoLimits::whole_file(), None).unwrap().census;
    assert!(!whole.hit_budget);
    assert_eq!(whole.packets, 40);
    assert_eq!(whole.bytes_consumed, total);
}

#[test]
fn a_cap_exactly_the_file_size_is_not_a_hit() {
    let bytes = synthetic_demo(&padded(2));
    let f = TempDemo::new("exact", &bytes);
    let c = scan(
        &f.0,
        DemoLimits {
            max_total_bytes: bytes.len() as u64,
        },
        None,
    )
    .unwrap()
    .census;
    assert!(!c.hit_budget);
    assert_eq!(c.packets, 2);
}

#[test]
fn something_that_is_not_a_demo_is_not_a_demo() {
    let f = TempDemo::new("sentence", b"this is not a replay, it is a sentence");
    assert_eq!(
        scan(&f.0, DemoLimits::default(), None).unwrap_err(),
        Error::NotDemo
    );
    assert_eq!(read_file_header(&f.0).unwrap_err(), Error::NotDemo);
}

#[test]
fn a_missing_file_is_an_io_error() {
    let missing = std::env::temp_dir().join("deadlock-replay-not-here.dem");
    let err = scan(&missing, DemoLimits::default(), None).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn a_command_that_would_overflow_the_parser_buffer_is_an_error_not_a_panic() {
    let huge = vec![(CONSOLE_CMD, 0, vec![0u8; 3 * 1024 * 1024])];
    let f = TempDemo::new("huge", &synthetic_demo(&huge));
    let err = scan(&f.0, DemoLimits::whole_file(), None).unwrap_err();
    assert!(matches!(err, Error::Demo(_)), "{err:?}");
}

#[test]
fn read_file_header_reads_only_the_first_command() {
    let f = TempDemo::new(
        "header",
        &synthetic_demo(&[
            (
                FILE_HEADER,
                0,
                file_header("dl_test", "/x/citadel_v1/citadel"),
            ),
            (CONSOLE_CMD, 1, vec![0xff; 8]),
        ]),
    );
    let h = read_file_header(&f.0).unwrap();
    assert_eq!(h.map_name.as_deref(), Some("dl_test"));

    let g = TempDemo::new("noheader", &synthetic_demo(&[(CONSOLE_CMD, 0, vec![0; 4])]));
    assert_eq!(
        read_file_header(&g.0).unwrap_err(),
        Error::MissingFileHeader
    );
}

#[test]
fn find_without_tables_is_inconclusive() {
    let f = TempDemo::new("notables", &synthetic_demo(&padded(2)));
    let s = scan(&f.0, DemoLimits::default(), Some("m_vecBannedHeroes")).unwrap();
    assert_eq!(s.search, Some(TableSearch::Inconclusive));
}

#[test]
fn server_build_is_none_without_a_citadel_component() {
    let h = CDemoFileHeader {
        game_directory: Some("/opt/srcds/other".to_owned()),
        ..CDemoFileHeader::default()
    };
    assert_eq!(server_build(&h), None);
    assert_eq!(server_build(&CDemoFileHeader::default()), None);
}

const REAL_DEM: &str =
    r"D:\Programs\Steam\steamapps\common\Deadlock\game\citadel\addons\replays\109529566.dem";

#[test]
#[ignore = "needs a local replay"]
fn a_real_replay_has_its_serializer_tables_inside_the_default_cap() {
    let path = std::path::Path::new(REAL_DEM);
    let s = scan(path, DemoLimits::default(), Some("m_vecBannedHeroes")).unwrap();
    assert!(s.census.file_header.is_some());
    assert_ne!(s.search, Some(TableSearch::Inconclusive));
    assert_eq!(
        s.search,
        scan(path, DemoLimits::whole_file(), Some("m_vecBannedHeroes"))
            .unwrap()
            .search
    );
    let h = read_file_header(path).unwrap();
    assert!(server_build(&h).is_some_and(|b| b.starts_with("citadel_v")));
}
