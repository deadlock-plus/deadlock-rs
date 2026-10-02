//! Hero and item catalogs, layered from the bundled snapshot and the installed game.
//!
//! Both the layering rule and the "where is the game" rule now live in the libraries -
//! `HeroCatalog::for_game` and `Reader::game_dir` - because the overlay needed the same
//! two and had its own copy of each.

use deadlock_data::{Catalogs, HeroCatalog, ItemCatalog, Provenance, Source};
use deadlock_reader::Reader;

/// Build a hero catalog: vendored snapshot, overlaid with the installed game's own
/// localisation when the reader can tell us where the game lives.
///
/// Never fails; the bundled snapshot is always there, so names render even with no game
/// files and no network.
pub fn hero_catalog(reader: Option<&Reader>) -> HeroCatalog {
    plan(reader).heroes()
}

/// Item/ability catalog, bundled snapshot overlaid with the installed game's names.
pub fn item_catalog(reader: Option<&Reader>) -> ItemCatalog {
    plan(reader).items()
}

/// Which sources the CLI consults, and where the install is.
///
/// Deliberately not the crate's default order: that one includes the API, and a
/// command-line lookup should not reach the network unless asked. `Client` first so names
/// match the running build, `Bundled` behind it so ids always resolve.
fn plan(reader: Option<&Reader>) -> Catalogs {
    let plan = Catalogs::new()
        .sources([Source::Client, Source::Bundled])
        .maybe_game_dir(reader.and_then(Reader::game_dir));
    // A reader can only name the install while the game is running. Searching for it
    // otherwise is what makes the installed-game sources useful with the game closed,
    // which is the point of their reading files rather than memory.
    #[cfg(feature = "client")]
    let plan = plan.discover_game_dir();
    plan
}

/// Where the ids and names on screen actually came from.
///
/// Worth reporting rather than inferring from "is the game running": an install can be
/// found and still fail to yield names, and the two look identical from the outside.
pub fn describe(p: Provenance) -> String {
    fn label(s: Option<Source>) -> &'static str {
        match s {
            Some(Source::Bundled) => "bundled snapshot",
            Some(Source::Api) => "deadlock-api.com",
            Some(Source::Client) => "installed game",
            None => "nothing",
        }
    }
    let (roster, names) = (label(p.roster), label(p.names));
    if roster == names {
        format!("all from {roster}")
    } else {
        format!("ids from {roster}, names from {names}")
    }
}
