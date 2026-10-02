//! `dlrs replay`: what is inside a local `.dem` file.
//!
//! Two questions, both answered without unpacking a replay: what build and map produced
//! it, and what commands it carries. Neither needs the whole file - the header packet is
//! the first one in every local replay and the tables sit in the first megabyte or so - so
//! the scan is bounded by default and says so in its own output.
//!
//! # Why the byte cap is the default and `--all` is not
//!
//! [`DemoLimits::whole_file`] over the 359 MB `32433914.dem` takes seconds cold, and
//! the local replays run up to 773 MB. That is usable but it is not what "tell me about
//! this file" should cost, and worse, the numbers it produces are a different measurement
//! from the bounded one. So the cap is a flag with a default of
//! [`DemoLimits::default`] (64 MiB) and every line of the census is qualified by how
//! much was actually read.
//!
//! # What this does not do
//!
//! `deadlock-replay` can also decode a `.meta.bz2` match-metadata payload through
//! `MatchMetaData`. That path is **not** exposed here, and the reason is that it cannot be
//! exercised: there are no `.meta.bz2` files in a local install - only `.dem` - and the
//! bzip2 dependency is a decoder, so no synthetic fixture can be built to stand in for
//! one either. A command surface that has never been run against a single real input is a
//! claim, not a feature.

use std::path::{Path, PathBuf};

use deadlock_replay::demo::{Census, DemoLimits, TableSearch, scan, server_build};
use deadlock_replay::valveprotos::common::CDemoFileHeader;

use crate::output::opt;

/// Flags this command accepts, as `dlrs help` lists them.
///
/// The list is also what the parser is tested against, so a flag cannot be accepted and
/// undocumented, or documented and rejected.
pub const FLAGS: &[(&str, &str)] = &[
    (
        "--all",
        "scan the whole file instead of the first --cap MiB",
    ),
    ("--cap <mib>", "how much of the file to read; default 64"),
    (
        "--find <name>",
        "also say whether the serializer tables contain <name>",
    ),
];

/// One `--cap` unit, in bytes.
const MIB: u64 = 1024 * 1024;

/// What `dlrs replay` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Opts {
    /// The `.dem` to read.
    path: PathBuf,
    /// How far the scan may go.
    limits: DemoLimits,
    /// Whether the caller asked for the whole file, as opposed to a cap that happens to be
    /// larger than it. The two produce the same census on a small file and different
    /// sentences about it, which is the point.
    whole_file: bool,
    /// A symbol to look for in the serializer tables, e.g. `m_vecBannedHeroes`.
    find: Option<String>,
}

impl Opts {
    /// Parse the arguments after `replay`.
    ///
    /// Errors carry the message the user sees; the caller prints it and exits 2.
    fn parse(args: &[&str]) -> Result<Opts, String> {
        let mut path: Option<PathBuf> = None;
        let mut limits = DemoLimits::default();
        let mut whole_file = false;
        let mut find = None;

        let mut i = 0;
        while i < args.len() {
            let arg = args[i];
            if arg.starts_with('-') && !documented(arg) {
                return Err(format!("unknown flag {arg:?}"));
            }
            match arg {
                "--all" => whole_file = true,
                "--cap" => {
                    let raw = value(args, &mut i, "--cap")?;
                    let mib: u64 = raw
                        .parse()
                        .map_err(|_| format!("--cap wants a number of MiB, got {raw:?}"))?;
                    if mib == 0 {
                        // A zero cap is not "read nothing", it is a scan that cannot even
                        // get past the sixteen-byte prelude, and reporting an empty census
                        // for it would look like an empty file.
                        return Err("--cap 0 would not read even the file header".to_owned());
                    }
                    limits.max_total_bytes = mib.saturating_mul(MIB);
                }
                "--find" => find = Some(value(args, &mut i, "--find")?.to_owned()),
                other if other.starts_with('-') => {
                    return Err(format!("unknown flag {other:?}"));
                }
                other => {
                    if path.is_some() {
                        return Err(format!("only one path is read; {other:?} is a second"));
                    }
                    path = Some(PathBuf::from(other));
                }
            }
            i += 1;
        }

        let Some(path) = path else {
            return Err("which .dem file? dlrs replay <path>".to_owned());
        };
        if whole_file {
            if limits.max_total_bytes != DemoLimits::default().max_total_bytes {
                // Two answers to the same question. Silently letting one win would make
                // the coverage line disagree with what was asked for.
                return Err("--all and --cap contradict each other".to_owned());
            }
            limits = DemoLimits::whole_file();
        }
        Ok(Opts {
            path,
            limits,
            whole_file,
            find,
        })
    }
}

/// Whether [`FLAGS`] lists `flag`.
///
/// The parser asks before it dispatches, which is what makes the help table the authority
/// rather than a copy of it: an arm below for a flag that is not listed above is
/// unreachable, so a flag cannot be accepted and undocumented.
fn documented(flag: &str) -> bool {
    FLAGS
        .iter()
        .any(|(spelling, _)| spelling.split_whitespace().next() == Some(flag))
}

/// The value after a flag, advancing the index past it.
fn value<'a>(args: &[&'a str], i: &mut usize, flag: &str) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i)
        .copied()
        .ok_or_else(|| format!("{flag} wants a value"))
}

/// The header block: what produced this replay.
///
/// Every field is optional on the wire, so every absent one is a dash. In particular
/// `patch_version`, `build_num` and `server_start_tick` are numbers whose absence is
/// not zero - a replay with no `build_num` is one this reader could not find it in, and
/// build 0 is a build.
fn header_lines(header: Option<&CDemoFileHeader>) -> Vec<String> {
    let h = header.cloned().unwrap_or_default();
    vec![
        format!("build       {}", opt(server_build(&h))),
        format!("map         {}", opt(h.map_name.as_deref())),
        format!("server      {}", opt(h.server_name.as_deref())),
        format!("client      {}", opt(h.client_name.as_deref())),
        format!("patch       {}", opt(h.patch_version)),
        format!("build_num   {}", opt(h.build_num)),
        format!(
            "demo        {} (fullpackets {})",
            opt(h.demo_version_name.as_deref()),
            opt(h.fullpackets_version)
        ),
        format!("start tick  {}", opt(h.server_start_tick)),
    ]
}

/// How much of the file the census covers, in one sentence.
///
/// This is the line the rest of the output has to be read against, so it is never
/// omitted and never says "the whole file" unless the scan actually reached the end of
/// one.
fn coverage_line(c: &Census, size: Option<u64>, whole_file: bool) -> String {
    let of_file = match size {
        Some(total) => format!(" of {total}"),
        None => String::new(),
    };
    if c.hit_budget {
        return format!(
            "scanned     {}{of_file} bytes; the census below covers only these bytes, not the whole file",
            c.bytes_consumed
        );
    }
    if whole_file {
        format!(
            "scanned     {} bytes: the whole file, as --all asked",
            c.bytes_consumed
        )
    } else {
        format!(
            "scanned     {} bytes: the whole file, which fits inside the cap",
            c.bytes_consumed
        )
    }
}

/// Everything `dlrs replay` prints for one file.
///
/// Split out from the command so it can be tested against a hand-built [`Census`], which
/// is the only way to pin the two numbers that are easy to confuse: bytes read from the
/// file, and payload bytes after decompression.
fn render(
    path: &Path,
    size: Option<u64>,
    c: &Census,
    whole_file: bool,
    find: Option<(&str, TableSearch)>,
) -> Vec<String> {
    let mut out = vec![
        format!(
            "file        {}",
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned()
            )
        ),
        format!("size        {} bytes", opt(size)),
    ];
    out.extend(header_lines(c.file_header.as_ref()));
    out.push(String::new());
    out.push(coverage_line(c, size, whole_file));
    out.push(format!(
        "packets     {} ({} of them compressed)",
        c.packets, c.compressed_packets
    ));
    // Deliberately not the same number as `scanned`, and labelled so it cannot be read as
    // one: a compressed packet contributes its framed size to the bytes read and its
    // decompressed size here.
    out.push(format!(
        "payload     {} bytes after decompression",
        c.decompressed_bytes
    ));
    out.push(format!(
        "largest     {} bytes, which is what the scan held at once",
        c.largest_packet_bytes
    ));

    if let Some((needle, found)) = find {
        // The scan itself says whether it established anything. The three tables sit in
        // the first megabyte, so an ordinary cap reads them and absence is a fact even
        // though the scan stopped long before the end of the file.
        out.push(format!(
            "{needle}  {}",
            match found {
                TableSearch::Present => "present in the serializer tables",
                TableSearch::Absent => "absent from the serializer tables",
                TableSearch::Inconclusive =>
                    "not established; the scan hit its cap before reading the tables",
            }
        ));
    }

    if c.packets > 0 {
        out.push(String::new());
        for (command, count) in &c.by_command {
            out.push(format!("{:<26} {count}", command.as_str_name()));
        }
    }
    out
}

pub fn replay(args: &[&str]) -> i32 {
    let opts = match Opts::parse(args) {
        Ok(o) => o,
        Err(why) => {
            eprintln!("{why}");
            return 2;
        }
    };

    let scanned = match scan(&opts.path, opts.limits, opts.find.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            // A path that is not there is an error, not an empty replay. `scan` opens
            // the file itself, so this covers a missing file, a directory, and bytes that
            // do not begin the demo magic.
            eprintln!("{}: {e}", opts.path.display());
            return 1;
        }
    };
    let census = scanned.census;
    let find = opts.find.as_deref().zip(scanned.search);

    let size = std::fs::metadata(&opts.path).ok().map(|m| m.len());
    for line in render(&opts.path, size, &census, opts.whole_file, find) {
        outln!("{line}");
    }
    0
}

#[cfg(test)]
mod tests {
    /// A temp path unique to this process.
    ///
    /// The process id is what keeps two concurrent runs of this suite apart. Without it a
    /// second test binary writes and deletes the same fixed paths mid-run, and the
    /// workspace suite is routinely run more than once at a time here - by a person and an
    /// agent, or by two agents. Measured before this existed: six concurrent runs of the
    /// `dlrs` test binary produced two failures, in `a_second_recording_appends` and in the
    /// `--cap` tests, and every one of those tests passes alone.
    ///
    /// A flaky suite is worse than a slow one: it trains everybody to re-run rather than
    /// read the failure.
    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dlrs-{}-{name}", std::process::id()))
    }

    use super::*;
    use deadlock_replay::demo::DEFAULT_MAX_TOTAL_BYTES;
    use deadlock_replay::prost::Message;
    use deadlock_replay::valveprotos::common::EDemoCommands;
    use std::collections::BTreeMap;

    /// The line whose prefix is `field`, with the prefix and its padding removed.
    fn field<'a>(lines: &'a [String], field: &str) -> &'a str {
        lines
            .iter()
            .find(|l| l.starts_with(field))
            .unwrap_or_else(|| panic!("no {field} line in {lines:#?}"))[field.len()..]
            .trim()
    }

    fn census_with(packets: u64) -> Census {
        Census {
            packets,
            ..Census::default()
        }
    }

    #[test]
    fn a_bare_path_scans_under_the_default_cap() {
        let o = Opts::parse(&["x.dem"]).unwrap();
        assert_eq!(o.path, PathBuf::from("x.dem"));
        assert_eq!(o.limits.max_total_bytes, DEFAULT_MAX_TOTAL_BYTES);
        assert!(!o.whole_file);
        assert_eq!(o.find, None);
    }

    #[test]
    fn all_lifts_the_total_cap_and_keeps_the_per_packet_one() {
        let o = Opts::parse(&["x.dem", "--all"]).unwrap();
        assert!(o.whole_file);
        assert_eq!(o.limits, DemoLimits::whole_file());
    }

    #[test]
    fn cap_is_read_in_mib() {
        let o = Opts::parse(&["--cap", "2", "x.dem"]).unwrap();
        assert_eq!(o.limits.max_total_bytes, 2 * 1024 * 1024);
        assert!(!o.whole_file);
    }

    #[test]
    fn the_arguments_that_are_not_a_scan() {
        for args in [
            &["x.dem", "--cap"][..],
            &["x.dem", "--cap", "eight"],
            &["x.dem", "--cap", "0"],
            &["x.dem", "--find"],
            &["x.dem", "--nope"],
            &["x.dem", "y.dem"],
            &["x.dem", "--all", "--cap", "2"],
            &[],
        ] {
            assert!(
                Opts::parse(args).is_err(),
                "{args:?} should not parse into a scan"
            );
        }
    }

    /// The other direction: a flag the parser has an arm for has to be listed, or it is
    /// a feature only the source reveals. The arms are named here rather than discovered,
    /// because a parser cannot enumerate itself - what makes that safe is `documented`,
    /// which rejects anything the table does not list, so an arm added without a table
    /// entry is unreachable and this test fails on it.
    #[test]
    fn every_flag_the_parser_handles_is_listed() {
        for name in ["--all", "--cap", "--find"] {
            assert!(documented(name), "{name} parses but is not listed");
        }
    }

    #[test]
    fn every_documented_flag_is_accepted() {
        for (spelling, help) in FLAGS {
            assert!(!help.is_empty(), "{spelling} has no description");
            let name = spelling.split_whitespace().next().unwrap();
            let args: Vec<&str> = if spelling.contains('<') {
                vec!["x.dem", name, "1"]
            } else {
                vec!["x.dem", name]
            };
            assert!(
                Opts::parse(&args).is_ok(),
                "{name} is documented but {args:?} does not parse"
            );
        }
    }

    /// The project rule: an absent value is a dash. A header where the reader found
    /// nothing must not report patch 0, build 0 and tick 0, all of which are real
    /// values some file could carry.
    #[test]
    fn an_absent_header_field_renders_as_a_dash_and_never_as_zero() {
        let c = Census {
            file_header: Some(CDemoFileHeader::default()),
            ..Census::default()
        };
        let lines = render(Path::new("x.dem"), None, &c, false, None);
        for name in [
            "build ",
            "map ",
            "server ",
            "client ",
            "patch",
            "build_num",
            "start tick",
        ] {
            assert_eq!(field(&lines, name), "-", "{name} should be a dash");
        }
        assert_eq!(field(&lines, "size"), "- bytes");
        assert_eq!(field(&lines, "demo"), "- (fullpackets -)");
    }

    #[test]
    fn a_header_that_was_read_is_reported() {
        let c = Census {
            file_header: Some(CDemoFileHeader {
                game_directory: Some("/opt/srcds/deadlock/citadel_v6670/citadel".to_owned()),
                map_name: Some("start".to_owned()),
                patch_version: Some(48),
                build_num: Some(10854),
                ..CDemoFileHeader::default()
            }),
            ..Census::default()
        };
        let lines = render(Path::new("x.dem"), Some(773), &c, false, None);
        assert_eq!(field(&lines, "build "), "citadel_v6670");
        assert_eq!(field(&lines, "map "), "start");
        assert_eq!(field(&lines, "patch"), "48");
        assert_eq!(field(&lines, "build_num"), "10854");
        assert_eq!(field(&lines, "size"), "773 bytes");
    }

    /// The two numbers that must never be conflated: bytes read from the file, and
    /// payload bytes after decompression. A compressed replay produces a far larger
    /// second number than first, and reporting either as the other is a lie about both
    /// what the scan cost and what it saw.
    #[test]
    fn bytes_read_and_decompressed_bytes_are_reported_separately() {
        let c = Census {
            packets: 4,
            compressed_packets: 3,
            bytes_consumed: 200,
            decompressed_bytes: 5000,
            largest_packet_bytes: 4096,
            ..Census::default()
        };
        let lines = render(Path::new("x.dem"), Some(200), &c, true, None);
        let scanned = lines.iter().find(|l| l.starts_with("scanned")).unwrap();
        assert!(scanned.contains("200"), "{scanned}");
        assert!(
            !scanned.contains("5000"),
            "the coverage line must not report decompressed bytes: {scanned}"
        );
        let payload = lines.iter().find(|l| l.starts_with("payload")).unwrap();
        assert!(
            payload.contains("5000") && payload.contains("decompression"),
            "{payload}"
        );
        assert!(!payload.contains("200"), "{payload}");
        assert_eq!(field(&lines, "packets"), "4 (3 of them compressed)");
    }

    /// A bounded census says plainly that it covers only what it read.
    #[test]
    fn a_capped_scan_says_it_did_not_cover_the_file() {
        let c = Census {
            hit_budget: true,
            bytes_consumed: 64 * MIB,
            ..census_with(10)
        };
        let lines = render(Path::new("x.dem"), Some(359_000_000), &c, false, None);
        let scanned = lines.iter().find(|l| l.starts_with("scanned")).unwrap();
        assert!(
            scanned.contains("only these bytes"),
            "a partial scan must say so: {scanned}"
        );
        assert!(
            !scanned.contains("the whole file, "),
            "a partial scan must not claim the whole file: {scanned}"
        );
        assert!(scanned.contains("359000000"), "{scanned}");
    }

    #[test]
    fn a_whole_file_scan_says_so_only_when_it_was_one() {
        let whole = render(Path::new("x.dem"), Some(10), &census_with(2), true, None);
        assert!(field(&whole, "scanned").contains("--all"));

        let under_cap = render(Path::new("x.dem"), Some(10), &census_with(2), false, None);
        assert!(field(&under_cap, "scanned").contains("fits inside the cap"));
    }

    #[test]
    fn a_command_census_lists_every_command_it_saw() {
        let mut by_command = BTreeMap::new();
        by_command.insert(EDemoCommands::DemFileHeader, 1);
        by_command.insert(EDemoCommands::DemPacket, 7);
        by_command.insert(EDemoCommands::DemRecovery, 2);
        let c = Census {
            packets: 10,
            by_command,
            ..Census::default()
        };
        let lines = render(Path::new("x.dem"), None, &c, false, None);
        assert_eq!(field(&lines, "DEM_FileHeader"), "1");
        assert_eq!(field(&lines, "DEM_Packet"), "7");
        assert_eq!(field(&lines, "DEM_Recovery"), "2");
    }

    /// A `--find` miss inside a capped scan is not the same answer as a `--find` miss
    /// that reached the end of the file.
    #[test]
    fn find_distinguishes_absent_from_not_reached() {
        let found = render(
            Path::new("x.dem"),
            None,
            &census_with(1),
            false,
            Some(("m_vecBannedHeroes", TableSearch::Present)),
        );
        assert!(field(&found, "m_vecBannedHeroes").starts_with("present"));

        let absent = render(
            Path::new("x.dem"),
            None,
            &census_with(1),
            false,
            Some(("m_vecBannedHeroes", TableSearch::Absent)),
        );
        assert_eq!(
            field(&absent, "m_vecBannedHeroes"),
            "absent from the serializer tables"
        );

        // A scan that hit its cap and still read all three tables reports the absence it
        // established. An earlier revision keyed this off the census's `hit_budget` and
        // hedged here, which threw away a definite answer: the tables sit in the first
        // megabyte, so nearly every capped scan reaches them.
        let c = Census {
            hit_budget: true,
            ..census_with(1)
        };
        let capped = render(
            Path::new("x.dem"),
            None,
            &c,
            false,
            Some(("m_vecBannedHeroes", TableSearch::Absent)),
        );
        assert_eq!(
            field(&capped, "m_vecBannedHeroes"),
            "absent from the serializer tables",
            "a cap that still read the tables settles the question"
        );

        // Only the scan saying it established nothing produces the hedge.
        let unreached = render(
            Path::new("x.dem"),
            None,
            &c,
            false,
            Some(("m_vecBannedHeroes", TableSearch::Inconclusive)),
        );
        assert!(field(&unreached, "m_vecBannedHeroes").contains("hit its cap"));
    }

    /// A demo whose packets are `(command, tick, payload)`.
    ///
    /// Nothing here is compressed; the real files are covered by the ignored tests below.
    fn synthetic_demo(packets: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
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
        let mut out = b"PBDEMS2\0".to_vec();
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        for (command, tick, payload) in packets {
            varint(&mut out, *command);
            varint(&mut out, *tick);
            varint(&mut out, payload.len() as u32);
            out.extend_from_slice(payload);
        }
        out
    }

    /// A `CDemoFileHeader` carrying `map_name` and `game_directory`, which are the two
    /// fields the rendered header is built out of.
    fn file_header_payload(map: &str, game_directory: &str) -> Vec<u8> {
        CDemoFileHeader {
            map_name: Some(map.to_owned()),
            game_directory: Some(game_directory.to_owned()),
            ..CDemoFileHeader::default()
        }
        .encode_to_vec()
    }

    /// A file under the scratch directory, removed when the test ends.
    struct TempDemo(PathBuf);

    impl TempDemo {
        fn new(name: &str, bytes: &[u8]) -> TempDemo {
            let path = scratch(&format!("{name}.dem"));
            std::fs::write(&path, bytes).expect("could not write the fixture");
            TempDemo(path)
        }
    }

    impl Drop for TempDemo {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_missing_file_is_an_error_and_not_an_empty_census() {
        let missing = scratch("not-here.dem");
        let _ = std::fs::remove_file(&missing);
        let path = missing.to_string_lossy().into_owned();
        assert_eq!(replay(&[path.as_str()]), 1);
    }

    #[test]
    fn something_that_is_not_a_demo_is_an_error() {
        let f = TempDemo::new("not-a-demo", b"this is not a replay, it is a sentence");
        let path = f.0.to_string_lossy().into_owned();
        assert_eq!(replay(&[path.as_str()]), 1);
    }

    /// The cap is not decoration: a scan under one must stop, say it stopped, and read no
    /// more than it was allowed.
    #[test]
    fn the_cap_bounds_what_is_read() {
        // 40 packets of 4 KiB is 160 KiB of payload, comfortably past a 64 KiB cap.
        let packets: Vec<(u32, u32, Vec<u8>)> = (0..40)
            .map(|i| (EDemoCommands::DemConsoleCmd as u32, i, vec![0xab; 4096]))
            .collect();
        let f = TempDemo::new("capped", &synthetic_demo(&packets));
        let total = std::fs::metadata(&f.0).unwrap().len();

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
        assert!(capped.packets < 40, "a capped scan saw every packet");

        let whole = scan(&f.0, DemoLimits::whole_file(), None).unwrap().census;
        assert!(!whole.hit_budget);
        assert_eq!(whole.packets, 40);
        assert_eq!(whole.bytes_consumed, total);

        // And the two are told apart in the output rather than only in the struct.
        let capped_out = render(&f.0, Some(total), &capped, false, None);
        assert!(field(&capped_out, "scanned").contains("only these bytes"));
        let whole_out = render(&f.0, Some(total), &whole, true, None);
        assert!(field(&whole_out, "scanned").contains("the whole file"));
    }

    #[test]
    fn a_synthetic_demo_reports_its_commands() {
        let header = file_header_payload("dl_test", "/opt/srcds/deadlock/citadel_v6670/citadel");
        let header_len = header.len();
        let f = TempDemo::new(
            "commands",
            &synthetic_demo(&[
                (EDemoCommands::DemFileHeader as u32, 0, header),
                (EDemoCommands::DemConsoleCmd as u32, 1, vec![0; 16]),
                (EDemoCommands::DemConsoleCmd as u32, 2, vec![0; 16]),
                (EDemoCommands::DemRecovery as u32, 3, vec![0; 4]),
            ]),
        );
        let c = scan(&f.0, DemoLimits::default(), None).unwrap().census;
        assert_eq!(c.packets, 4);
        assert_eq!(c.compressed_packets, 0);
        assert_eq!(c.decompressed_bytes as usize, header_len + 16 + 16 + 4);
        let lines = render(&f.0, None, &c, false, None);
        assert_eq!(field(&lines, "build "), "citadel_v6670");
        assert_eq!(field(&lines, "map "), "dl_test");
        // Absent header fields stay dashes even when the header itself was read.
        assert_eq!(field(&lines, "patch"), "-");
        assert_eq!(field(&lines, "DEM_ConsoleCmd"), "2");
        assert_eq!(field(&lines, "DEM_Recovery"), "1");
        let path = f.0.to_string_lossy().into_owned();
        assert_eq!(replay(&[path.as_str()]), 0);
    }

    /// Replays live beside the install, under `citadel/replays`.
    fn local_replays() -> Vec<PathBuf> {
        let Ok(dir) = std::env::var("DEADLOCK_CITADEL_DIR") else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(PathBuf::from(dir).join("replays")) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "dem"))
            .collect();
        out.sort();
        out
    }

    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn every_local_replay_reports_a_build_and_a_map() {
        let replays = local_replays();
        assert!(
            !replays.is_empty(),
            "no .dem files under DEADLOCK_CITADEL_DIR"
        );
        for path in &replays {
            let header = deadlock_replay::demo::read_file_header(path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let build = deadlock_replay::demo::server_build(&header)
                .unwrap_or_else(|| panic!("{} has no build", path.display()));
            assert!(build.starts_with("citadel_v"), "{build}");
            assert!(header.map_name.is_some(), "{} has no map", path.display());

            let c = Census {
                file_header: Some(header),
                ..Census::default()
            };
            let lines = render(path, None, &c, false, None);
            assert_ne!(field(&lines, "build "), "-");
            assert_ne!(field(&lines, "map "), "-");
        }
    }

    /// The cap against a real replay, which is where it actually matters: these files run
    /// from 269 to 773 MB and the default scan must read a small fraction of one.
    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn the_default_cap_reads_a_fraction_of_a_real_replay() {
        let replays = local_replays();
        let path = replays.first().expect("no .dem files");
        let size = std::fs::metadata(path).unwrap().len();
        assert!(
            size > DEFAULT_MAX_TOTAL_BYTES,
            "{} is small",
            path.display()
        );

        let c = scan(path, DemoLimits::default(), None).unwrap().census;
        assert!(c.hit_budget);
        assert!(c.bytes_consumed <= DEFAULT_MAX_TOTAL_BYTES);
        assert!(c.bytes_consumed < size);
        assert!(
            field(&render(path, Some(size), &c, false, None), "scanned")
                .contains("only these bytes")
        );
    }
}
