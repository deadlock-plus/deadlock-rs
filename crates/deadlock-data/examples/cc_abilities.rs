//! Write the hard-CC ability set of an installed game as JSON.
//!
//! ```text
//! cargo run -p deadlock-data --features vpk,serde --example cc_abilities -- [citadel dir] [out.json]
//! ```

#[cfg(not(all(feature = "vpk", feature = "serde")))]
fn main() {
    eprintln!("build with --features vpk,serde");
}

#[cfg(all(feature = "vpk", feature = "serde"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use deadlock_data::vdata::{CcOwner, cc_abilities};

    let mut args = std::env::args_os().skip(1);
    let dir = match args.next() {
        Some(dir) => std::path::PathBuf::from(dir),
        None => {
            deadlock_data::install::find_citadel_dir().ok_or(deadlock_data::Error::GameNotFound)?
        }
    };
    let set = cc_abilities(&dir)?;
    let json = serde_json::to_string_pretty(&set)? + "\n";
    match args.next() {
        Some(out) => std::fs::write(out, json)?,
        None => print!("{json}"),
    }

    let items = set.values().filter(|a| a.owner == CcOwner::Item).count();
    let gated = set.values().filter(|a| a.gated()).count();
    eprintln!(
        "{} with hard CC: {} hero abilities, {items} items; {gated} only after an upgrade",
        set.len(),
        set.len() - items
    );
    Ok(())
}
