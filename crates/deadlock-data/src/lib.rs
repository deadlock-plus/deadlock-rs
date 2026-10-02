//! Reference data for Deadlock: hero, ability and item names.
//!
//! Everything the game *is* rather than what it is currently *doing*. Where
//! [`deadlock-memory`] reads a live client, this crate answers "what is hero 65 called?".
//!
//! # Three sources, selected by feature
//!
//! | Source | Feature | Offline | Version-matched | Supplies |
//! |---|---|---|---|---|
//! | Vendored snapshot | `bundled` | yes | at build time | roster, English names, art |
//! | Installed game | `client` | yes | exactly | names, in any of [`LANGUAGES`] |
//! | Installed game | `vpk` | yes | exactly | roster: ids, class names, flags |
//! | deadlock-api.com | `online` | no | yes | roster, names for one language, art |
//!
//! `bundled` and `client` are on by default, which is the behaviour this crate has always
//! had. At least one **roster** source has to be enabled, because every consumer looks
//! data up by [`deadlock_core::HeroId`] and names on their own are not addressable by id.
//!
//! The client source is two features, because its two halves cost very different things.
//! `client` reads the loose localisation files for display names and needs nothing but
//! std. `vpk` reads the game's own id tables out of the compiled `vdata_c` files in the
//! VPK archives, which needs a VPK reader and a zstd decoder; it is what lets
//! [`Source::Client`] answer for the roster.
//!
//! # Facets
//!
//! A catalog is three tables rather than one, because the sources do not carry the same
//! things:
//!
//! - **Roster** — [`Hero`] / [`Item`]: id, class name, flags.
//! - **Names** — [`Names`]: `class_name -> display name`, per language.
//! - **Art** — [`HeroArt`] / [`ItemArt`]: portrait, card and icon URLs.
//!
//! They merge independently, so layering a source that carries only one of the three
//! cannot blank the other two, and [`HeroCatalog::provenance`] reports which source
//! answered for each. Art is *structurally* absent from the installed game rather than
//! incidentally missing: those URLs point at `assets-bucket.deadlock-api.com` and are a
//! deadlock-api.com construct, not game data.
//!
//! The intended pattern is to layer them: start from the bundled snapshot so nothing ever
//! fails, then overlay the installed game's files so display names are correct and in the
//! player's language, and optionally refresh from the API to pick up heroes released since
//! this crate was built. [`HeroCatalog::for_game`] does the first two.
//!
//! [`deadlock-memory`]: https://docs.rs/deadlock-memory

#![forbid(unsafe_code)]

// Names are reachable without a roster, but nothing can be looked up by id without one,
// and every consumer of this crate goes through `HeroId` / `ItemId`. Cargo features are
// additive and unify across the graph, so "exactly one source" is not expressible; "at
// least one" is the enforceable form of the invariant.
#[cfg(not(any(feature = "bundled", feature = "online", feature = "vpk")))]
compile_error!(
    "deadlock-data needs at least one roster source: enable `bundled`, `online` or `vpk`. \
     `client` on its own supplies display names only; the game's own id tables are \
     compiled `vdata_c` files inside the VPK archives, which `vpk` reads."
);

pub mod error;
pub mod heroes;
pub mod items;
pub mod names;
pub mod ranks;
pub mod resolve;
pub mod source;

#[cfg(feature = "client")]
pub mod install;
#[cfg(feature = "vpk")]
pub mod vdata;

#[cfg(feature = "client")]
pub mod localization;

#[cfg(feature = "online")]
pub mod api;

pub use error::{Error, Result};
pub use heroes::{Hero, HeroArt, HeroCatalog};
pub use items::{Item, ItemArt, ItemCatalog};
pub use names::Names;
pub use ranks::{RankFamily, RankNames};
pub use resolve::Catalogs;
pub use source::{Provenance, Source};

#[cfg(feature = "client")]
pub use localization::{BUNDLES, LocalizationFile};

/// Languages Deadlock ships localisation for, as they appear in file names.
///
/// Verified against a retail install, and against **every** bundle rather than one: each
/// of the ten directories `localization::BUNDLES` names holds exactly these 29 and nothing
/// else. That is the claim callers need, because `LocalizationFile::load` takes an
/// arbitrary bundle - a list vouched for by a single bundle would let a valid language and
/// a valid bundle combine into a missing file.
///
/// Named rather than linked because `localization` is behind the `client` feature and this
/// list is not.
pub const LANGUAGES: &[&str] = &[
    "brazilian",
    "bulgarian",
    "czech",
    "danish",
    "dutch",
    "english",
    "finnish",
    "french",
    "german",
    "greek",
    "hungarian",
    "indonesian",
    "italian",
    "japanese",
    "koreana",
    "latam",
    "norwegian",
    "polish",
    "portuguese",
    "romanian",
    "russian",
    "schinese",
    "spanish",
    "swedish",
    "tchinese",
    "thai",
    "turkish",
    "ukrainian",
    "vietnamese",
];

/// The default language when none is specified.
pub const DEFAULT_LANGUAGE: &str = "english";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_list_is_sorted_and_unique() {
        assert!(LANGUAGES.windows(2).all(|w| w[0] < w[1]));
        assert!(LANGUAGES.contains(&DEFAULT_LANGUAGE));
    }
}
