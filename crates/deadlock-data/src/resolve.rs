//! Choosing which sources answer, and in what order.
//!
//! Features decide which sources are *compiled in*; this decides which of them a given
//! call actually *consults*, and in what order. A consumer that wants nothing but the
//! vendored snapshot asks for it at runtime rather than rebuilding the crate:
//!
//! ```
//! use deadlock_data::{Catalogs, Source};
//!
//! // Only the vendored snapshot is consulted. The installed game and the API are not,
//! // regardless of which features this build has compiled in.
//! let heroes = Catalogs::only(Source::Bundled).heroes();
//! assert_ne!(heroes.provenance().names, Some(Source::Client));
//! ```
//!
//! # Degradation order
//!
//! The default is `client → online → bundled`, first that answers wins, applied **per
//! facet** rather than per catalog:
//!
//! | Facet | Order |
//! |---|---|
//! | Roster | client → online → bundled |
//! | Names | client (requested language) → online → bundled |
//! | Art | online → bundled — client never participates |
//!
//! Per-facet resolution is what makes strict first-wins safe. Applied to whole catalogs
//! it would be a regression: without the `vpk` feature the installed game answers for
//! names and not for the roster, so a whole-catalog first-wins would hand back a catalog
//! with no ids in it and `hero_name(HeroId(1))` would return `None`. It also never has to
//! answer all-or-nothing — the client can supply the roster and names while art still
//! comes from the snapshot.
//!
//! Art skips the client because those URLs point at `assets-bucket.deadlock-api.com` and
//! are a deadlock-api.com construct. No client-side source can ever produce them, so
//! consulting one would be a guaranteed miss rather than a fallback.
//!
//! # What each source can answer for today
//!
//! | Source | Roster | Names | Art | Needs |
//! |---|---|---|---|---|
//! | [`Source::Bundled`] | yes | English | yes | `bundled` |
//! | [`Source::Api`] | yes | one language | yes | `online` + network |
//! | [`Source::Client`] | with `vpk` | 29 languages | never | `client` / `vpk` + [`Catalogs::game_dir`] |
//!
//! A source that is not compiled in, or that has nothing to answer with, is skipped
//! rather than failing the call. Ask [`crate::Provenance`] what actually answered.

#[cfg(feature = "client")]
use std::path::Path;
use std::path::PathBuf;

use crate::names::Names;
use crate::source::{Provenance, Source};

/// The degradation order from the design: client, then online, then bundled.
pub const DEFAULT_ORDER: &[Source] = &[Source::Client, Source::Api, Source::Bundled];

/// Builds catalogs from a chosen set of sources.
///
/// See the [module docs](self) for the order and what each source can answer for.
#[derive(Clone, Debug)]
pub struct Catalogs {
    order: Vec<Source>,
    citadel_dir: Option<PathBuf>,
    language: String,
}

impl Default for Catalogs {
    fn default() -> Self {
        Self::new()
    }
}

impl Catalogs {
    /// Consult every source, in the default degradation order.
    ///
    /// Note this includes [`Source::Api`], so on a build with the `online` feature a
    /// resolve may perform network I/O. Drop it from the order for an offline guarantee:
    /// `.sources([Source::Client, Source::Bundled])`.
    pub fn new() -> Self {
        Catalogs {
            order: DEFAULT_ORDER.to_vec(),
            citadel_dir: None,
            language: crate::DEFAULT_LANGUAGE.to_string(),
        }
    }

    /// Consult exactly these sources, in this order of preference.
    ///
    /// Anything omitted is never consulted, even if its feature is enabled. This is the
    /// runtime half of source selection; features are the compile-time half.
    pub fn sources(mut self, order: impl IntoIterator<Item = Source>) -> Self {
        self.order = order.into_iter().collect();
        self
    }

    /// Consult one source and nothing else.
    pub fn only(source: Source) -> Self {
        Self::new().sources([source])
    }

    /// Where the installed game lives - the directory containing `resource/`, i.e.
    /// `.../Deadlock/game/citadel`.
    ///
    /// Without this, [`Source::Client`] has nothing to read and is skipped.
    pub fn game_dir(mut self, citadel_dir: impl Into<PathBuf>) -> Self {
        self.citadel_dir = Some(citadel_dir.into());
        self
    }

    /// Set the game directory from an optional path, as a reader reports it.
    pub fn maybe_game_dir(mut self, citadel_dir: Option<impl Into<PathBuf>>) -> Self {
        self.citadel_dir = citadel_dir.map(Into::into);
        self
    }

    /// Search for an installed game, when no directory has been set already.
    ///
    /// A reader can name the directory only while the game is running; this is what lets
    /// the installed-game sources answer with it closed, which is the whole point of them
    /// reading files rather than memory. Explicitly opt-in, because it touches the disk.
    ///
    /// Does nothing if [`Catalogs::game_dir`] already supplied one, and leaves the
    /// directory unset if nothing is found.
    #[cfg(feature = "client")]
    pub fn discover_game_dir(mut self) -> Self {
        if self.citadel_dir.is_none() {
            self.citadel_dir = crate::install::find_citadel_dir();
        }
        self
    }

    /// Which language display names should be in. Defaults to
    /// [`crate::DEFAULT_LANGUAGE`].
    pub fn language(mut self, language: impl Into<String>) -> Self {
        self.language = language.into();
        self
    }

    /// The sources this will consult, in order.
    pub fn order(&self) -> &[Source] {
        &self.order
    }

    #[cfg(feature = "client")]
    fn dir(&self) -> Option<&Path> {
        self.citadel_dir.as_deref()
    }
}

/// Resolve one facet by walking the order and taking the first source that answers.
///
/// `answer` returns `None` for a source that is not compiled in, has no input to work
/// from, or simply has nothing for this facet.
fn first_answer<T>(
    order: &[Source],
    mut answer: impl FnMut(Source) -> Option<T>,
) -> Option<(Source, T)> {
    order.iter().find_map(|&s| answer(s).map(|v| (s, v)))
}

impl Catalogs {
    /// Build a hero catalog from the chosen sources.
    ///
    /// Never fails: a source that cannot answer is skipped, and if none answer for a
    /// facet that facet is simply empty. [`crate::HeroCatalog::provenance`] reports what
    /// happened.
    pub fn heroes(&self) -> crate::HeroCatalog {
        use crate::heroes::HeroCatalog;

        let roster = first_answer(&self.order, |s| self.hero_roster(s));
        let (roster_src, roster) = match roster {
            Some((s, r)) => (Some(s), r),
            None => (None, Vec::new()),
        };

        let names = first_answer(&self.order, |s| self.hero_names(s, &roster));
        let art = first_answer(&self.order, |s| self.hero_art(s));

        let provenance = Provenance {
            roster: roster_src,
            names: names.as_ref().map(|(s, _)| *s),
            art: art.as_ref().map(|(s, _)| *s),
        };
        HeroCatalog::from_facets(
            roster,
            names.map(|(_, n)| n).unwrap_or_default(),
            art.map(|(_, a)| a).unwrap_or_default(),
            provenance,
        )
    }

    fn hero_roster(&self, source: Source) -> Option<Vec<crate::heroes::Hero>> {
        if source == Source::Client {
            #[cfg(feature = "vpk")]
            {
                let roster = crate::vdata::hero_roster(self.dir()?).ok()?;
                return (!roster.is_empty()).then_some(roster);
            }
            #[cfg(not(feature = "vpk"))]
            return None;
        }
        let c = self.hero_catalog_from(source)?;
        (!c.is_empty()).then(|| c.into_facets().0)
    }

    /// Names for the resolved roster.
    ///
    /// The roster matters here because the client source builds its table by overlaying
    /// localisation onto a roster; for items in particular, only tokens the roster has a
    /// row for are item names at all.
    fn hero_names(&self, source: Source, roster: &[crate::heroes::Hero]) -> Option<Names> {
        if source == Source::Client {
            #[cfg(feature = "client")]
            {
                let mut c = crate::heroes::HeroCatalog::new(roster.to_vec());
                c.merge_game_dir_lang(self.dir()?, &self.language).ok()?;
                let names = c.into_facets().1;
                return (!names.is_empty()).then_some(names);
            }
            #[cfg(not(feature = "client"))]
            {
                let _ = roster;
                return None;
            }
        }
        let names = self.hero_catalog_from(source)?.into_facets().1;
        (!names.is_empty()).then_some(names)
    }

    fn hero_art(
        &self,
        source: Source,
    ) -> Option<std::collections::HashMap<deadlock_core::HeroId, crate::heroes::HeroArt>> {
        // The installed game has no art to give; asking it is a guaranteed miss.
        if source == Source::Client {
            return None;
        }
        let art = self.hero_catalog_from(source)?.into_facets().2;
        (!art.is_empty()).then_some(art)
    }

    /// A whole catalog straight from one source, or `None` if it cannot supply one.
    fn hero_catalog_from(&self, source: Source) -> Option<crate::heroes::HeroCatalog> {
        match source {
            #[cfg(feature = "bundled")]
            Source::Bundled => Some(crate::heroes::HeroCatalog::bundled()),
            #[cfg(feature = "online")]
            Source::Api => crate::api::fetch_heroes(&self.language).ok(),
            // The id table is a compiled `vdata_c` in the VPK archives, not read yet, so
            // the client cannot answer with a whole catalog. Its names are built against
            // a roster in `hero_names` instead.
            _ => None,
        }
    }
}

impl Catalogs {
    /// Build an item catalog from the chosen sources. See [`Catalogs::heroes`].
    pub fn items(&self) -> crate::ItemCatalog {
        use crate::items::ItemCatalog;

        let roster = first_answer(&self.order, |s| self.item_roster(s));
        let (roster_src, roster) = match roster {
            Some((s, r)) => (Some(s), r),
            None => (None, Vec::new()),
        };

        let names = first_answer(&self.order, |s| self.item_names(s, &roster));
        let art = first_answer(&self.order, |s| self.item_art(s));

        let provenance = Provenance {
            roster: roster_src,
            names: names.as_ref().map(|(s, _)| *s),
            art: art.as_ref().map(|(s, _)| *s),
        };
        ItemCatalog::from_facets(
            roster,
            names.map(|(_, n)| n).unwrap_or_default(),
            art.map(|(_, a)| a).unwrap_or_default(),
            provenance,
        )
    }

    fn item_roster(&self, source: Source) -> Option<Vec<crate::items::Item>> {
        if source == Source::Client {
            #[cfg(feature = "vpk")]
            {
                let roster = crate::vdata::item_roster(self.dir()?).ok()?;
                return (!roster.is_empty()).then_some(roster);
            }
            #[cfg(not(feature = "vpk"))]
            return None;
        }
        let c = self.item_catalog_from(source)?;
        (!c.is_empty()).then(|| c.into_facets().0)
    }

    fn item_names(&self, source: Source, roster: &[crate::items::Item]) -> Option<Names> {
        if source == Source::Client {
            #[cfg(feature = "client")]
            {
                let mut c = crate::items::ItemCatalog::new(roster.to_vec());
                c.merge_game_dir_lang(self.dir()?, &self.language).ok()?;
                let names = c.into_facets().1;
                return (!names.is_empty()).then_some(names);
            }
            #[cfg(not(feature = "client"))]
            {
                let _ = roster;
                return None;
            }
        }
        let names = self.item_catalog_from(source)?.into_facets().1;
        (!names.is_empty()).then_some(names)
    }

    fn item_art(
        &self,
        source: Source,
    ) -> Option<std::collections::HashMap<deadlock_core::ItemId, crate::items::ItemArt>> {
        if source == Source::Client {
            return None;
        }
        let art = self.item_catalog_from(source)?.into_facets().2;
        (!art.is_empty()).then_some(art)
    }

    fn item_catalog_from(&self, source: Source) -> Option<crate::items::ItemCatalog> {
        match source {
            #[cfg(feature = "bundled")]
            Source::Bundled => Some(crate::items::ItemCatalog::bundled()),
            #[cfg(feature = "online")]
            Source::Api => crate::api::fetch_items(&self.language).ok(),
            _ => None,
        }
    }
}

#[cfg(all(test, feature = "bundled"))]
mod tests {
    use super::*;
    use deadlock_core::{HeroId, HeroNames, ItemNames};

    #[test]
    fn default_order_is_the_documented_degradation_order() {
        assert_eq!(
            Catalogs::new().order(),
            [Source::Client, Source::Api, Source::Bundled]
        );
    }

    #[test]
    fn bundled_only_answers_every_facet() {
        let c = Catalogs::only(Source::Bundled).heroes();
        assert_eq!(c.provenance(), Provenance::all(Source::Bundled));
        assert_eq!(c.hero_name(HeroId(1)), Some("Infernus"));
        assert!(c.portrait(HeroId(1)).is_some());
    }

    /// A source nobody asked for must not answer, even with its feature compiled in.
    #[test]
    fn an_unlisted_source_is_never_consulted() {
        let c = Catalogs::new()
            .sources([Source::Client])
            .game_dir("does/not/exist")
            .heroes();
        assert_eq!(c.provenance(), Provenance::EMPTY);
        assert!(c.is_empty());
        assert_eq!(c.hero_name(HeroId(1)), None);
    }

    /// The regression §7 exists to prevent: whole-catalog first-wins would take the
    /// client's names-only answer and discard every id with it.
    #[test]
    fn facets_resolve_independently_of_each_other() {
        let c = Catalogs::new()
            .sources([Source::Client, Source::Bundled])
            .game_dir("does/not/exist")
            .heroes();
        assert_eq!(c.provenance().roster, Some(Source::Bundled));
        assert_eq!(c.hero_name(HeroId(1)), Some("Infernus"));
    }

    #[test]
    fn art_never_asks_the_client() {
        let c = Catalogs::new()
            .sources([Source::Client, Source::Bundled])
            .heroes();
        assert_eq!(c.provenance().art, Some(Source::Bundled));
    }

    /// A consumer deciding for itself what counts as failure. The crate reports which
    /// facets were answered; the policy is the caller's.
    #[test]
    fn a_consumer_can_define_its_own_failure_condition() {
        fn strict(c: &crate::HeroCatalog) -> Result<(), &'static str> {
            match c.provenance().roster {
                Some(_) => Ok(()),
                None => Err("no source could supply a hero roster"),
            }
        }

        let c = Catalogs::only(Source::Client)
            .game_dir("does/not/exist")
            .heroes();

        assert!(
            c.provenance().roster.is_none(),
            "nothing answered for the roster"
        );
        assert_eq!(c.provenance(), Provenance::EMPTY, "nor for anything else");
        assert!(c.is_empty());

        assert!(strict(&c).is_err());

        let ok = Catalogs::only(Source::Bundled).heroes();
        assert!(strict(&ok).is_ok());
        assert_eq!(ok.provenance().roster, Some(Source::Bundled));

        assert!(ok.provenance().names.is_some());
        assert!(ok.provenance().art.is_some());
    }

    #[test]
    fn an_empty_order_answers_nothing_without_panicking() {
        let c = Catalogs::new().sources([]).heroes();
        assert!(c.is_empty());
        assert_eq!(c.provenance(), Provenance::EMPTY);
        let i = Catalogs::new().sources([]).items();
        assert!(i.is_empty());
        assert_eq!(i.item_name(deadlock_core::ItemId(1)), None);
    }

    #[test]
    fn items_resolve_from_bundled_too() {
        let c = Catalogs::only(Source::Bundled).items();
        assert!(c.len() >= 700);
        assert_eq!(c.provenance().roster, Some(Source::Bundled));
        assert_eq!(
            c.item_name(deadlock_core::ItemId(3977876567)),
            Some("Kinetic Dash")
        );
    }

    /// Against a real install: names come from the game while art still comes from the
    /// snapshot, which is the whole point of resolving facets separately.
    ///
    /// Which source wins the *roster* depends on the build - see the assertion below -
    /// and that difference is exactly what the `vpk` feature buys.
    ///
    /// Ignored by default because it needs Deadlock installed. Run it with:
    /// `DEADLOCK_CITADEL_DIR=".../Deadlock/game/citadel" cargo test -- --ignored`
    #[cfg(feature = "client")]
    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn a_real_install_answers_for_names_and_art_separately() {
        let Ok(dir) = std::env::var("DEADLOCK_CITADEL_DIR") else {
            panic!("set DEADLOCK_CITADEL_DIR to the citadel directory");
        };
        let c = Catalogs::new()
            .sources([Source::Client, Source::Bundled])
            .game_dir(&dir)
            .heroes();
        #[cfg(not(feature = "vpk"))]
        assert_eq!(c.provenance().roster, Some(Source::Bundled), "roster");
        #[cfg(feature = "vpk")]
        assert_eq!(c.provenance().roster, Some(Source::Client), "roster");

        assert_eq!(c.provenance().names, Some(Source::Client), "names");
        assert_eq!(c.provenance().art, Some(Source::Bundled), "art");
        assert_eq!(c.hero_name(HeroId(1)), Some("Infernus"));
        assert!(c.portrait(HeroId(1)).is_some());

        let de = Catalogs::new()
            .sources([Source::Client, Source::Bundled])
            .game_dir(&dir)
            .language("german")
            .heroes();
        assert_eq!(de.names().language(), Some("german"));

        let items = Catalogs::new()
            .sources([Source::Client, Source::Bundled])
            .game_dir(&dir)
            .items();
        assert_eq!(items.provenance().names, Some(Source::Client));
        assert!(items.localised_count() > 400, "{}", items.localised_count());
    }

    /// The whole point of §8: with `vpk` the installed game answers for the roster too,
    /// so a catalogue can be built from the game alone and still be addressable by id.
    #[cfg(feature = "vpk")]
    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn a_real_install_answers_for_the_roster_as_well() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let c = Catalogs::new()
            .sources([Source::Client, Source::Bundled])
            .game_dir(&dir)
            .heroes();

        assert_eq!(c.provenance().roster, Some(Source::Client), "roster");
        assert_eq!(c.provenance().names, Some(Source::Client), "names");
        assert_eq!(c.provenance().art, Some(Source::Bundled), "art");

        assert_eq!(c.len(), 67, "hero count");
        assert_eq!(c.hero_name(HeroId(1)), Some("Infernus"));
        assert!(
            c.by_class_name("hero_base").is_none(),
            "hero_base must not appear"
        );

        assert!(c.by_class_name("hero_genericperson").is_some());
        assert!(c.by_class_name("hero_targetdummy").is_some());

        let selectable = c.all().iter().filter(|h| h.selectable()).count();
        assert!(
            selectable > 20 && selectable < c.len(),
            "{selectable} of {}",
            c.len()
        );

        let items = Catalogs::new()
            .sources([Source::Client, Source::Bundled])
            .game_dir(&dir)
            .items();
        assert_eq!(items.provenance().roster, Some(Source::Client));
        assert!(items.len() > 726, "only {} items", items.len());
        let id = deadlock_core::ItemId::from_class_name("gunslinger_demonMark");
        assert!(items.get(id).is_some(), "hash-derived id must be present");
    }

    /// Listing a source twice, or listing one whose feature is off, must not break the
    /// walk.
    #[test]
    fn duplicate_and_unavailable_sources_are_tolerated() {
        let c = Catalogs::new()
            .sources([Source::Bundled, Source::Bundled, Source::Api])
            .heroes();
        assert_eq!(c.provenance().roster, Some(Source::Bundled));
        assert_eq!(c.hero_name(HeroId(1)), Some("Infernus"));
    }
}
