//! Refresh the hero catalog from deadlock-api.com and diff it against the vendored
//! snapshot.
//!
//! ```text
//! cargo run -p deadlock-data --features online,bundled --example refresh
//! ```

#[cfg(not(all(feature = "online", feature = "bundled")))]
fn main() {
    // The point of the example is the diff, which needs a snapshot to diff against.
    eprintln!("build with --features online,bundled");
}

#[cfg(all(feature = "online", feature = "bundled"))]
fn main() {
    use deadlock_core::{HeroId, HeroNames};
    use deadlock_data::HeroCatalog;

    let bundled = HeroCatalog::bundled();
    println!("bundled: {} heroes", bundled.len());

    match deadlock_data::api::fetch_heroes("english") {
        Ok(live) => {
            println!("api:     {} heroes", live.len());
            let mut merged = bundled.clone();
            let changed = merged.merge(&live);
            println!("merge changed {changed} entries (0 means the snapshot is current)");
            for h in live.all().iter().take(3) {
                let name = live.hero_name(h.id).unwrap_or(&h.class_name);
                println!("  {} -> {name} ({})", h.id, h.class_name);
            }
            assert_eq!(live.hero_name(HeroId(1)), Some("Infernus"));
            println!("ok");
        }
        // Offline is a normal condition, not a failure.
        Err(e) => println!("offline or unavailable, bundled snapshot still usable: {e}"),
    }
}
