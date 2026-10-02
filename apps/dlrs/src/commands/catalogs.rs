//! Offline catalogues: `dlrs heroes` and `dlrs items`.

use deadlock_core::{HeroNames, ItemNames};
use deadlock_reader::Reader;

use crate::catalog::{describe, hero_catalog, item_catalog};

/// Hero id -> name. Works with no game running and no network: the bundled snapshot
/// covers it, and the installed game's localisation is layered over when available.
pub fn heroes(filter: Option<&str>) -> i32 {
    let reader = Reader::attach().ok();
    let c = hero_catalog(reader.as_ref());
    let source = describe(c.provenance());
    outln!("{} heroes, {source}", c.len());
    outln!("");
    outln!(
        "{:>5}  {:<24} {:<22} {}",
        "id",
        "name",
        "class_name",
        "status"
    );

    let mut shown = 0;
    for h in c.all() {
        if let Some(f) = filter {
            let f = f.to_lowercase();
            let name = c.hero_name(h.id).unwrap_or(&h.class_name);
            if !name.to_lowercase().contains(&f) && !h.class_name.to_lowercase().contains(&f) {
                continue;
            }
        }
        let status = if h.disabled {
            "disabled"
        } else if h.in_development {
            "in development"
        } else {
            ""
        };
        outln!(
            "{:>5}  {:<24} {:<22} {}",
            h.id.get(),
            c.hero_name(h.id).unwrap_or(&h.class_name),
            h.class_name,
            status
        );
        shown += 1;
    }
    outln!("");
    outln!("{shown} shown, {} playable overall", c.playable().count());
    0
}

/// Item / ability / weapon catalogue. Works with no game running.
///
/// The filter matches name or class name, or an exact kind (`ability`, `upgrade`,
/// `weapon`).
pub fn items(filter: Option<&str>) -> i32 {
    let reader = Reader::attach().ok();
    let c = item_catalog(reader.as_ref());
    let source = describe(c.provenance());
    outln!(
        "{} entries, {source}; {} have localised names",
        c.len(),
        c.localised_count()
    );
    outln!("");
    outln!(
        "{:>12}  {:<9} {:<32} {}",
        "id",
        "kind",
        "name",
        "class_name"
    );

    let mut shown = 0;
    for i in c.all() {
        if let Some(f) = filter {
            let f = f.to_lowercase();
            let name = c.item_name(i.id).unwrap_or(&i.class_name);
            let matches = name.to_lowercase().contains(&f)
                || i.class_name.to_lowercase().contains(&f)
                || i.kind.as_str() == f;
            if !matches {
                continue;
            }
        }
        outln!(
            "{:>12}  {:<9} {:<32} {}",
            i.id.get(),
            i.kind.as_str(),
            c.item_name(i.id).unwrap_or(&i.class_name),
            i.class_name
        );
        shown += 1;
    }
    outln!("");
    outln!("{shown} shown");
    0
}
