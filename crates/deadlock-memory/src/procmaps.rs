//! Parsing `/proc/<pid>/maps` into modules and scannable regions.
//!
//! Deliberately platform-independent and pure: it takes the file's text, not a pid. That
//! keeps the risky part - the parsing and the module grouping - testable on any host,
//! including from recorded fixtures of a real Proton process.
//!
//! On Linux this one file replaces both `VirtualQueryEx` and the Toolhelp32 module
//! snapshot.

use std::path::PathBuf;

use crate::mem::{Module, Region};
use crate::region::MAX_REGION;

/// One line of `/proc/<pid>/maps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapEntry {
    /// First address of the mapping.
    pub start: u64,
    /// One past the last address.
    pub end: u64,
    /// `r` bit.
    pub readable: bool,
    /// `w` bit.
    pub writable: bool,
    /// `x` bit.
    pub executable: bool,
    /// `p` (private) rather than `s` (shared).
    pub private: bool,
    /// Offset into the backing file.
    pub offset: u64,
    /// Backing inode, `0` for anonymous mappings.
    pub inode: u64,
    /// Backing path, or a pseudo-name like `[heap]`.
    pub path: Option<String>,
}

impl MapEntry {
    /// Size of the mapping in bytes.
    pub fn size(&self) -> usize {
        self.end.saturating_sub(self.start) as usize
    }

    /// Whether this is file-backed rather than anonymous.
    pub fn is_file_backed(&self) -> bool {
        self.inode != 0
            && self
                .path
                .as_deref()
                .map(|p| p.starts_with('/'))
                .unwrap_or(false)
    }

    /// Kernel pseudo-mappings that must never be read.
    ///
    /// `[vvar]` and `[vsyscall]` misbehave under `process_vm_readv`, and reading them
    /// buys nothing.
    pub fn is_unreadable_pseudo(&self) -> bool {
        matches!(
            self.path.as_deref(),
            Some("[vvar]") | Some("[vsyscall]") | Some("[vdso]")
        )
    }

    /// File name component of the backing path.
    pub fn file_name(&self) -> Option<&str> {
        let p = self.path.as_deref()?;
        if !p.starts_with('/') {
            return None;
        }
        p.rsplit('/').next().filter(|s| !s.is_empty())
    }
}

/// Parse a whole `/proc/<pid>/maps` file.
///
/// Lines look like:
///
/// ```text
/// 7f8c0e000000-7f8c0e021000 r--p 00000000 08:02 1234  /path/to/client.dll
/// 5566a1000000-5566a1021000 rw-p 00000000 00:00 0     [heap]
/// ```
///
/// Malformed lines are skipped rather than failing the parse: this file is read from a
/// live process and can race with mapping changes.
pub fn parse_maps(text: &str) -> Vec<MapEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(6, ' ');
        let Some(range) = parts.next() else { continue };
        let Some(perms) = parts.next() else { continue };
        let Some(offset) = parts.next() else { continue };
        let Some(_dev) = parts.next() else { continue };
        let Some(inode) = parts.next() else { continue };
        // The path field is space-padded and may itself contain spaces.
        let path = parts.next().map(str::trim).filter(|s| !s.is_empty());

        let Some((lo, hi)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (u64::from_str_radix(lo, 16), u64::from_str_radix(hi, 16))
        else {
            continue;
        };
        let perms = perms.as_bytes();
        if perms.len() < 4 {
            continue;
        }

        out.push(MapEntry {
            start,
            end,
            readable: perms[0] == b'r',
            writable: perms[1] == b'w',
            executable: perms[2] == b'x',
            private: perms[3] == b'p',
            offset: u64::from_str_radix(offset, 16).unwrap_or(0),
            inode: inode.parse().unwrap_or(0),
            path: path.map(str::to_owned),
        });
    }
    out
}

/// Group file-backed mappings into modules.
///
/// A PE or ELF image occupies several consecutive VMAs with different protections; the
/// module base is the lowest, and the span runs to the highest. Modules are keyed by
/// path so two libraries with the same basename stay distinct, but [`Module::name`] is
/// the basename because that is what callers match on.
///
/// Under Proton the paths are container-relative and will not match host paths, which is
/// exactly why lookup elsewhere is by basename.
pub fn modules_from_maps(entries: &[MapEntry]) -> Vec<Module> {
    use std::collections::BTreeMap;

    let mut by_path: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for e in entries {
        if !e.is_file_backed() {
            continue;
        }
        let Some(path) = e.path.as_deref() else {
            continue;
        };
        let span = by_path.entry(path).or_insert((e.start, e.end));
        span.0 = span.0.min(e.start);
        span.1 = span.1.max(e.end);
    }

    by_path
        .into_iter()
        .map(|(path, (base, end))| Module {
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            base,
            size: end.saturating_sub(base) as usize,
            path: PathBuf::from(path),
        })
        .collect()
}

/// Select the regions worth scanning for resident heap objects.
///
/// Mirrors the Windows filter in [`crate::region::accepts`]: committed (everything in
/// `maps` is), readable and writable, private-anonymous or file-backed, and not larger
/// than the cap. `PROT_NONE` guard mappings drop out because they are not readable.
pub fn regions_from_maps(entries: &[MapEntry]) -> Vec<Region> {
    entries
        .iter()
        .filter(|e| e.readable && e.writable)
        .filter(|e| !e.is_unreadable_pseudo())
        .filter(|e| e.size() > 0 && e.size() <= MAX_REGION)
        .map(|e| Region {
            base: e.start,
            size: e.size(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape of a real Proton process: the game's PE mapped by Wine across several
    /// VMAs, plus wine's own libraries, heap and stack.
    const PROTON_MAPS: &str = "\
00400000-00401000 r--p 00000000 08:02 100 /home/u/.steam/steamapps/common/Deadlock/game/citadel/bin/win64/client.dll
00401000-03a00000 r-xp 00001000 08:02 100 /home/u/.steam/steamapps/common/Deadlock/game/citadel/bin/win64/client.dll
03a00000-03c00000 rw-p 03a00000 08:02 100 /home/u/.steam/steamapps/common/Deadlock/game/citadel/bin/win64/client.dll
03c00000-03c40000 r--p 03c00000 08:02 100 /home/u/.steam/steamapps/common/Deadlock/game/citadel/bin/win64/client.dll
7f0000000000-7f0000021000 rw-p 00000000 00:00 0
7f1000000000-7f1000010000 ---p 00000000 00:00 0
7f2000000000-7f2000100000 r-xp 00000000 08:02 200 /usr/lib/wine/x86_64-unix/ntdll.so
7ffd00000000-7ffd00021000 rw-p 00000000 00:00 0 [stack]
7ffd10000000-7ffd10002000 r--p 00000000 00:00 0 [vvar]
ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0 [vsyscall]
";

    #[test]
    fn parses_a_representative_maps_file() {
        let e = parse_maps(PROTON_MAPS);
        assert_eq!(e.len(), 10);

        let first = &e[0];
        assert_eq!(first.start, 0x0040_0000);
        assert_eq!(first.end, 0x0040_1000);
        assert!(first.readable && !first.writable && !first.executable && first.private);
        assert_eq!(first.inode, 100);
        assert_eq!(first.file_name(), Some("client.dll"));

        let anon = &e[4];
        assert!(anon.path.is_none());
        assert_eq!(anon.inode, 0);
        assert!(!anon.is_file_backed());
    }

    #[test]
    fn groups_pe_sections_into_one_module() {
        let mods = modules_from_maps(&parse_maps(PROTON_MAPS));
        let client = mods
            .iter()
            .find(|m| m.name == "client.dll")
            .expect("client");
        assert_eq!(client.base, 0x0040_0000);
        assert_eq!(client.size, 0x03c4_0000 - 0x0040_0000);
        assert_eq!(mods.len(), 2);
        assert!(mods.iter().any(|m| m.name == "ntdll.so"));
    }

    #[test]
    fn pseudo_mappings_are_never_offered_as_regions() {
        let r = regions_from_maps(&parse_maps(PROTON_MAPS));
        assert!(!r.iter().any(|x| x.base == 0x7ffd_1000_0000));
        assert!(!r.iter().any(|x| x.base == 0xffff_ffff_ff60_0000));
    }

    #[test]
    fn region_filter_matches_the_windows_semantics() {
        let r = regions_from_maps(&parse_maps(PROTON_MAPS));
        let bases: Vec<u64> = r.iter().map(|x| x.base).collect();
        assert!(bases.contains(&0x7f00_0000_0000));
        assert!(bases.contains(&0x03a0_0000));
        assert!(!bases.contains(&0x7f10_0000_0000));
        assert!(!bases.contains(&0x0040_1000));
        assert!(bases.contains(&0x7ffd_0000_0000));
    }

    #[test]
    fn oversized_regions_are_skipped() {
        let huge = format!(
            "100000000000-{:x} rw-p 00000000 00:00 0 \n",
            0x1000_0000_0000u64 + MAX_REGION as u64 + 0x1000
        );
        assert!(regions_from_maps(&parse_maps(&huge)).is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let text = "garbage\n\
                    zzzz-yyyy rw-p 0 00:00 0 \n\
                    00400000-00401000 rw-p 00000000 00:00 0 \n";
        let e = parse_maps(text);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].start, 0x40_0000);
    }

    #[test]
    fn paths_with_spaces_survive() {
        let text = "00400000-00401000 r--p 00000000 08:02 1 /home/u/My Games/client.dll\n";
        let e = parse_maps(text);
        assert_eq!(e[0].file_name(), Some("client.dll"));
        assert_eq!(e[0].path.as_deref(), Some("/home/u/My Games/client.dll"));
    }

    /// Deleted-file mappings appear as `/path (deleted)`; they must not be mistaken for
    /// a different module.
    #[test]
    fn deleted_suffix_does_not_split_a_module() {
        let text = "\
00400000-00401000 r--p 00000000 08:02 1 /tmp/client.dll (deleted)
00401000-00402000 rw-p 00001000 08:02 1 /tmp/client.dll (deleted)
";
        let mods = modules_from_maps(&parse_maps(text));
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].size, 0x2000);
    }
}
