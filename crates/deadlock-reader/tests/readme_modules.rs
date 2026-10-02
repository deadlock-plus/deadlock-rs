//! The README's module table has to keep up with the crate.
//!
//! This crate's README carries a `Layout` table, one row per public module, and it is the
//! map a reader uses to find anything here. It is maintained by hand, and two modules -
//! `drift` and `supervise` - had been public for some time without appearing in it.
//!
//! That is the fourth hand-maintained list in this repository to drift, after the MSRV job,
//! the licence job and the root README's crate table. All four failed the same silent way:
//! nothing misbehaved, no test went red, an entry was simply never added. The fix each time
//! is to compare the list against reality rather than against memory, which is what this
//! does.
//!
//! Only the two backend crates have such a table, so the check lives with each of them
//! rather than in the shared workspace-hygiene tests.

use std::path::Path;

/// Every `pub mod` in `lib.rs` appears in the README's layout table.
#[test]
fn the_readme_layout_table_lists_every_public_module() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib = std::fs::read_to_string(dir.join("src").join("lib.rs")).expect("read lib.rs");
    let readme = std::fs::read_to_string(dir.join("README.md")).expect("read README.md");

    let declared: Vec<&str> = lib
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub mod "))
        .filter_map(|l| l.strip_suffix(';'))
        .collect();
    assert!(
        declared.len() > 15,
        "only found {} public modules, so the scan stopped matching lib.rs",
        declared.len()
    );

    let missing: Vec<&&str> = declared
        .iter()
        .filter(|m| !readme.contains(&format!("| `{m}` |")))
        .collect();
    assert!(
        missing.is_empty(),
        "public modules with no row in the README layout table: {missing:?}"
    );
}

/// `unsafe` appears only where the OS forces it.
///
/// The one place this crate needs it is the Steam account id read from the Windows
/// registry. Everything else here - the schema walk, the entity walk, every snapshot, every
/// parser - is safe Rust operating on bytes `deadlock-memory` handed over. An `unsafe`
/// appearing anywhere else means that boundary has moved, and moving it is a decision worth
/// making on purpose.
///
/// Matched on `unsafe {`, `unsafe fn` and `unsafe impl` rather than the bare word, because
/// `drift.rs` discusses an "unsafe list" in prose and prose is not code.
#[test]
fn unsafe_stays_at_the_operating_system_boundary() {
    const AT_THE_BOUNDARY: [&str; 1] = ["steam.rs"];

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src];
    let mut offenders = Vec::new();
    let mut seen = 0;

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            seen += 1;
            let name = path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or_default()
                .to_owned();
            if AT_THE_BOUNDARY.contains(&name.as_str()) {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("read source");
            for (n, line) in body.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if code.contains("unsafe {")
                    || code.contains("unsafe fn")
                    || code.contains("unsafe impl")
                {
                    offenders.push(format!("{name}:{}: {}", n + 1, code.trim()));
                }
            }
        }
    }

    assert!(seen > 15, "only scanned {seen} sources");
    assert!(
        offenders.is_empty(),
        "unsafe outside the OS boundary ({AT_THE_BOUNDARY:?}): {offenders:#?}"
    );
}
