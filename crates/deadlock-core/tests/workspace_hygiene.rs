//! Workspace hygiene that no single crate can check about itself.
//!
//! Lives in `deadlock-core` because it is the crate everything else depends on and the
//! natural place for a check that is about the workspace rather than about any one member.
//! It walks up from `CARGO_MANIFEST_DIR` to the workspace root and reads files there.
//!
//! # Why this is a test and not only a CI job
//!
//! CI already enumerates the library crates twice - once for MSRV, once for the licence
//! check - and both lists are written by hand. That list has fallen behind reality
//! **twice**: `deadlock-vpk` was missing from the licence job at the same time it was
//! missing its `LICENSE.md`, so the job that existed to catch exactly that could not, and
//! the `cross` job was missing both `deadlock-vpk` and `deadlock-replay`, which are the
//! two crates whose whole reason for picking pure-Rust decompressors is to cross-compile.
//!
//! A hand-maintained list of crates in a file that is not compiled will drift again. This
//! makes the drift fail at `cargo test`, where it is noticed, instead of on a CI run this
//! repository does not currently have.

use std::path::{Path, PathBuf};

/// The workspace root, found by walking up from this crate.
fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..4 {
        if dir.join("Cargo.toml").exists() && dir.join("crates").is_dir() {
            return dir;
        }
        dir = match dir.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
    }
    panic!(
        "could not find the workspace root from {}",
        env!("CARGO_MANIFEST_DIR")
    );
}

/// Every directory under `crates/`, which is exactly the set of library crates.
///
/// Read from the filesystem rather than from a list, because a list is the thing that
/// keeps going stale. `apps/` is deliberately excluded: those are binaries, are not
/// published, and carry the workspace licence by reference.
fn library_crates(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join("crates"))
        .expect("read crates/")
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    assert!(out.len() >= 7, "found only {} crates: {out:?}", out.len());
    out
}

/// Every library crate ships its own licence text.
///
/// The workspace is LGPL-3.0-or-later and each crate is published separately, so a crate
/// without `LICENSE.md` would publish without its terms. This is what `deadlock-vpk`
/// failed at the time nothing was checking it.
#[test]
fn every_library_crate_carries_its_licence() {
    let root = workspace_root();
    let missing: Vec<&String> = library_crates(&root)
        .iter()
        .filter(|c| !root.join("crates").join(c).join("LICENSE.md").is_file())
        .cloned()
        .collect::<Vec<String>>()
        .leak()
        .iter()
        .collect();
    assert!(missing.is_empty(), "crates without LICENSE.md: {missing:?}");
}

/// Every library crate appears in **both** CI jobs that name crates individually.
///
/// The `msrv` job spells out a `cargo check -p ...` list and the `package` job spells out
/// a shell `for c in ...` list. Neither can notice a crate that was never added to it: a
/// job silently checking seven of eight crates reports success, which is worse than
/// reporting nothing.
///
/// The two lists are extracted **separately** rather than searching the file as a whole.
/// Searching the whole text is what made an earlier draft of this test useless: every
/// crate in the `msrv` list is a substring match for the `package` list too, so dropping a
/// crate from one job alone still passed. Comment lines are stripped for the same reason -
/// `ci.yml` names `deadlock-vpk` and `deadlock-replay` in prose explaining why they are
/// listed, and prose must not satisfy the check.
#[test]
fn every_library_crate_is_named_in_both_ci_jobs_that_enumerate_them() {
    /// The text between an anchor and the blank line that ends its block.
    fn block<'a>(code: &'a str, anchor: &str) -> &'a str {
        let start = code
            .find(anchor)
            .unwrap_or_else(|| panic!("ci.yml no longer contains {anchor:?}"));
        let rest = &code[start..];
        match rest.find("\n\n") {
            Some(end) => &rest[..end],
            None => rest,
        }
    }

    let root = workspace_root();
    let ci = root.join(".github").join("workflows").join("ci.yml");
    let text = std::fs::read_to_string(&ci).expect("read ci.yml");
    let code: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let msrv = block(&code, "cargo check");
    let licence = block(&code, "for c in ");
    assert!(
        msrv.contains("deadlock-core") && licence.contains("deadlock-core"),
        "the two job blocks were not located correctly"
    );

    let mut absent = Vec::new();
    for c in library_crates(&root) {
        if !msrv.contains(&c) {
            absent.push(format!("{c}: missing from the MSRV job"));
        }
        if !licence.contains(&c) {
            absent.push(format!("{c}: missing from the licence job"));
        }
    }
    assert!(
        absent.is_empty(),
        "library crates missing from a CI job that enumerates crates: {absent:#?}\nsee {}",
        ci.display()
    );
}

/// Every optional feature a crate declares is named in that crate's README.
///
/// Features are the part of a crate's surface most likely to go undocumented, because
/// adding one is a two-line edit in `Cargo.toml` and nothing fails when the README is not
/// touched. Item 11.4 brought the READMEs up to date once; this is what keeps them there.
///
/// It found three real omissions when written. `deadlock-memory/modifiers` was absent -
/// the one feature in this workspace with a measured tick cost attached, so a consumer
/// enabling it blind pays roughly 220 us a tick without being told. `deadlock-data/vpk`
/// was absent, and it is the feature that decides whether an installed game can answer
/// for the *roster* or only for names. A crate's `serde` feature was once absent because its README
/// had no features section at all.
///
/// `default` is excluded: it is a composition of the others, not a thing to document on
/// its own, and every README that lists what is on by default already says so in prose.
///
/// Named, not explained - this asserts the string appears, which is the weakest useful
/// claim. A feature mentioned once in passing satisfies it. That is deliberate: a test
/// that tried to judge whether prose was adequate would either be unfalsifiable or
/// permanently annoying.
#[test]
fn every_feature_is_named_in_its_crate_readme() {
    let root = workspace_root();
    let mut undocumented = Vec::new();

    for c in library_crates(&root) {
        let dir = root.join("crates").join(&c);
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).expect("read Cargo.toml");

        let Some(rest) = toml.split("\n[features]\n").nth(1) else {
            continue;
        };
        let table = match rest.find("\n[") {
            Some(end) => &rest[..end],
            None => rest,
        };

        let readme = match std::fs::read_to_string(dir.join("README.md")) {
            Ok(r) => r,
            Err(_) => {
                undocumented.push(format!("{c}: has features but no README.md"));
                continue;
            }
        };

        for line in table.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, _)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name == "default" || name.is_empty() {
                continue;
            }
            if !readme.contains(name) {
                undocumented.push(format!("{c}: `{name}` is not named in README.md"));
            }
        }
    }

    assert!(
        undocumented.is_empty(),
        "features declared but not documented: {undocumented:#?}"
    );
}

/// Every library crate has a row in the root README's crate table.
///
/// The README is the front door: a crate absent from that table is a crate a reader does
/// not know exists. `deadlock-replay` was missing from it — a whole crate, with the demo
/// reader, the snappy decoder and the match-metadata decode in it — while being listed
/// correctly in both CI jobs and carrying its own licence. Nothing was wrong except that
/// nobody could find it.
///
/// This is the third list of crates in this repository that has drifted, after the MSRV job
/// and the licence job. Lists of crates maintained by hand go stale; the fix each time is
/// to check them against the directory listing rather than against memory.
///
/// Matched on the `crates/<name>` link target rather than the bare name, so a crate merely
/// mentioned in prose elsewhere in the file cannot satisfy the check.
#[test]
fn every_library_crate_has_a_row_in_the_root_readme() {
    let root = workspace_root();
    let readme = root.join("README.md");
    let text = std::fs::read_to_string(&readme).expect("read README.md");

    let missing: Vec<String> = library_crates(&root)
        .into_iter()
        .filter(|c| !text.contains(&format!("crates/{c}")))
        .collect();
    assert!(
        missing.is_empty(),
        "library crates with no row in the README crate table: {missing:?}\nsee {}",
        readme.display()
    );
}
