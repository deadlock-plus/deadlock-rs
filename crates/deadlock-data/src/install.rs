//! Finding an installed game without a running process.
//!
//! The vdata source reads files, not memory, so it works with the game installed and
//! closed — but only if something can say where the install is. A reader can answer that
//! from a live process; with nothing running, this is the fallback.
//!
//! Three steps, in order:
//!
//! 1. The [`ENV_OVERRIDE`] environment variable, pointing straight at the `citadel`
//!    directory. Always wins, and is the reliable answer for an unusual layout.
//! 2. Steam's `steamapps/libraryfolders.vdf`, which lists every library root, checked for
//!    a Deadlock install under each.
//! 3. Well-known Steam locations, plus one level below each drive root on Windows, since
//!    people move Steam off the system disk far more often than not.
//!
//! # What this deliberately does not do
//!
//! Steam records its own location in the Windows registry, and that is the only method
//! that finds *every* install. Reading it means either `unsafe` calls or a new
//! dependency, and this crate keeps `#![forbid(unsafe_code)]` and a near-empty dependency
//! list. So discovery is best-effort by design: when it fails, [`ENV_OVERRIDE`] or a path
//! from the caller is the answer, and both are cheap for a consumer to offer.

use std::path::{Path, PathBuf};

/// Environment variable naming the `citadel` directory outright.
///
/// `.../steamapps/common/Deadlock/game/citadel`.
pub const ENV_OVERRIDE: &str = "DEADLOCK_CITADEL_DIR";

/// Deadlock's Steam application id.
pub const APP_ID: &str = "1422450";

/// The packed archive that marks a real `citadel` directory.
///
/// Lives here rather than in [`crate::vdata`] because discovery is useful without the
/// `vpk` feature: the localisation source needs an install path too.
pub const ARCHIVE: &str = "pak01_dir.vpk";

/// Path of the game directory relative to a Steam library root.
const RELATIVE: &[&str] = &["steamapps", "common", "Deadlock", "game", "citadel"];

/// Locate an installed game, or `None` if no search step finds one.
///
/// Only paths that actually contain the game are returned: each candidate is confirmed by
/// [`is_citadel_dir`] rather than assumed from its shape.
pub fn find_citadel_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(ENV_OVERRIDE) {
        let dir = PathBuf::from(dir);
        // An override that is set but wrong is a mistake worth surfacing, so it is not
        // silently fallen through - the caller gets `None` and can say so.
        return is_citadel_dir(&dir).then_some(dir);
    }
    steam_roots()
        .into_iter()
        .flat_map(|root| library_roots(&root))
        .find_map(|library| {
            let dir = citadel_dir_in(&library);
            is_citadel_dir(&dir).then_some(dir)
        })
}

/// Whether a directory really is a `citadel` directory.
///
/// Checks for the packed archive rather than the directory name: a leftover empty folder
/// with the right name is not an install, and the archive is what the vdata source needs.
pub fn is_citadel_dir(dir: impl AsRef<Path>) -> bool {
    dir.as_ref().join(ARCHIVE).is_file()
}

/// The game directory beneath a Steam library root.
pub fn citadel_dir_in(library_root: impl AsRef<Path>) -> PathBuf {
    RELATIVE
        .iter()
        .fold(library_root.as_ref().to_path_buf(), |p, part| p.join(part))
}

/// Every library root a Steam installation knows about, itself included.
///
/// Reads `steamapps/libraryfolders.vdf`, which lists the other drives Steam has been
/// pointed at. Only the `"path"` values are taken: each is then checked for the game
/// directly, which avoids parsing the nested `"apps"` block just to learn something a
/// single `is_file` answers.
pub fn library_roots(steam_root: impl AsRef<Path>) -> Vec<PathBuf> {
    let steam_root = steam_root.as_ref();
    let mut roots = vec![steam_root.to_path_buf()];

    let vdf = steam_root.join("steamapps").join("libraryfolders.vdf");
    let Ok(text) = std::fs::read_to_string(&vdf) else {
        return roots;
    };
    for line in text.lines() {
        let Some(path) = vdf_string_value(line, "path") else {
            continue;
        };
        // Valve escapes backslashes in VDF, so `D:\\Games` means `D:\Games`.
        let path = PathBuf::from(path.replace("\\\\", "\\"));
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    roots
}

/// Pull the value out of a `"key"  "value"` VDF line, when the key matches.
fn vdf_string_value(line: &str, key: &str) -> Option<String> {
    let mut parts = line.split('"').skip(1);
    let found = parts.next()?;
    if found != key {
        return None;
    }
    // Between the key and the value is whitespace; the next quoted run is the value.
    parts.nth(1).map(str::to_string)
}

/// Places a Steam installation plausibly lives.
fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |p: PathBuf| {
        if p.join("steamapps").is_dir() && !roots.contains(&p) {
            roots.push(p);
        }
    };

    for var in ["STEAM_ROOT", "STEAMPATH"] {
        if let Some(v) = std::env::var_os(var) {
            push(PathBuf::from(v));
        }
    }

    if cfg!(windows) {
        for var in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(v) = std::env::var_os(var) {
                push(PathBuf::from(v).join("Steam"));
            }
        }
        // Steam is routinely moved off the system disk, and its location is only recorded
        // in the registry - which this crate will not read. One level under each drive
        // root covers the common `D:\Steam` and `D:\Games\Steam` shapes for the cost of a
        // few directory listings.
        for letter in 'A'..='Z' {
            let drive = PathBuf::from(format!("{letter}:\\"));
            if !drive.is_dir() {
                continue;
            }
            push(drive.join("Steam"));
            let Ok(entries) = std::fs::read_dir(&drive) else {
                continue;
            };
            for entry in entries.flatten().take(MAX_SCAN_PER_DRIVE) {
                push(entry.path().join("Steam"));
            }
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        let xdg = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
        for candidate in unix_steam_candidates(&PathBuf::from(home), xdg, cfg!(target_os = "macos"))
        {
            push(candidate);
        }
    }
    roots
}

/// Steam install locations under a Unix home directory, most likely first.
///
/// Pure so the layouts can be checked on any host. macOS keeps Steam under
/// `Library/Application Support`; Linux has the classic symlinks, the XDG data directory,
/// and the Flatpak and Snap sandboxes, each with its own copy of the tree.
fn unix_steam_candidates(home: &Path, xdg_data_home: Option<PathBuf>, macos: bool) -> Vec<PathBuf> {
    if macos {
        return vec![
            home.join("Library")
                .join("Application Support")
                .join("Steam"),
        ];
    }
    let mut out = vec![
        home.join(".steam").join("steam"),
        home.join(".steam").join("root"),
    ];
    if let Some(xdg) = xdg_data_home {
        out.push(xdg.join("Steam"));
    }
    out.push(home.join(".local").join("share").join("Steam"));
    out.push(
        home.join(".var")
            .join("app")
            .join("com.valvesoftware.Steam")
            .join(".local")
            .join("share")
            .join("Steam"),
    );
    out.push(
        home.join("snap")
            .join("steam")
            .join("common")
            .join(".local")
            .join("share")
            .join("Steam"),
    );
    out
}

/// How many top-level directories to look inside per drive.
///
/// A bound rather than a guess at the right number: without it a drive with thousands of
/// entries, or a slow network mount, would make discovery pause noticeably.
const MAX_SCAN_PER_DRIVE: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_path_line_from_a_library_file() {
        assert_eq!(
            vdf_string_value(r#"		"path"		"D:\\Programs\\Steam""#, "path").as_deref(),
            Some(r"D:\\Programs\\Steam")
        );
        assert_eq!(vdf_string_value(r#"    "label"    "x""#, "path"), None);
        assert_eq!(vdf_string_value("{", "path"), None);
        assert_eq!(vdf_string_value("", "path"), None);
    }

    #[test]
    fn library_roots_always_include_the_steam_root_itself() {
        let roots = library_roots("Z:/nonexistent-steam");
        assert_eq!(roots, vec![PathBuf::from("Z:/nonexistent-steam")]);
    }

    #[test]
    fn library_roots_reads_the_paths_out_of_a_real_file() {
        let dir = std::env::temp_dir().join("deadlock-data-libfolders-test");
        let steamapps = dir.join("steamapps");
        std::fs::create_dir_all(&steamapps).unwrap();
        std::fs::write(
            steamapps.join("libraryfolders.vdf"),
            "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\Programs\\\\Steam\"\n\t\t\"label\"\t\t\"\"\n\t}\n}\n",
        )
        .unwrap();

        let roots = library_roots(&dir);
        assert_eq!(roots.len(), 2, "{roots:?}");
        assert_eq!(roots[0], dir);
        assert_eq!(roots[1], PathBuf::from(r"D:\Programs\Steam"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn macos_steam_lives_under_application_support() {
        let c = unix_steam_candidates(Path::new("/Users/u"), None, true);
        assert_eq!(
            c,
            vec![PathBuf::from("/Users/u/Library/Application Support/Steam")]
        );
    }

    #[test]
    fn linux_candidates_cover_native_xdg_flatpak_and_snap() {
        let c = unix_steam_candidates(Path::new("/home/u"), Some(PathBuf::from("/data")), false);
        let has = |tail: &str| {
            c.iter()
                .any(|p| p.to_string_lossy().replace('\\', "/").ends_with(tail))
        };
        assert!(has("/home/u/.steam/steam"));
        assert!(has("/data/Steam"));
        assert!(has("/home/u/.local/share/Steam"));
        assert!(has("com.valvesoftware.Steam/.local/share/Steam"));
        assert!(has("snap/steam/common/.local/share/Steam"));
        assert!(!has("Application Support/Steam"));
    }

    #[test]
    fn a_directory_without_the_archive_is_not_an_install() {
        let dir = std::env::temp_dir().join("deadlock-data-not-citadel");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_citadel_dir(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_game_directory_hangs_off_a_library_root() {
        let p = citadel_dir_in("D:/Steam");
        assert!(
            p.ends_with("steamapps/common/Deadlock/game/citadel")
                || p.ends_with(r"steamapps\common\Deadlock\game\citadel"),
            "{}",
            p.display()
        );
    }

    /// Against whatever this machine has. Records the answer rather than asserting one:
    /// discovery is best-effort, and a machine with no Steam is a legitimate outcome.
    #[test]
    #[ignore = "depends on the machine's Steam layout"]
    fn reports_what_discovery_finds_here() {
        match find_citadel_dir() {
            Some(dir) => {
                println!("found: {}", dir.display());
                assert!(is_citadel_dir(&dir));
            }
            None => println!("no install found; set {ENV_OVERRIDE}"),
        }
    }
}
