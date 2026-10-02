//! The hero catalog, split into three facets.
//!
//! Identity, display names and art come from different places and are not available
//! together. `id -> class_name` lives in `scripts/heroes.vdata_c` inside the game's VPK
//! archives, which is expensive to reach; `class_name -> display name` is in loose
//! localisation files, which is cheap; art URLs are published only by deadlock-api.com
//! and no client-side source can ever produce them.
//!
//! So a catalog holds a **roster** ([`Hero`]), a [`Names`] table and an art table
//! ([`HeroArt`]), each merged independently. Merging a roster never disturbs art, which
//! is what lets a source that carries only some of the three be layered over one that
//! carries the rest. [`HeroCatalog::provenance`] reports which source answered for each.

use std::collections::HashMap;
#[cfg(any(feature = "bundled", feature = "client"))]
use std::path::Path;

use deadlock_core::{HeroId, HeroNames};

use crate::error::{Error, Result};
#[cfg(feature = "client")]
use crate::localization::{HERO_NAMES_BUNDLE, LocalizationFile};
use crate::names::Names;
use crate::source::{Provenance, Source};

/// Snapshot of the hero list taken from deadlock-api.com, vendored so the crate works
/// with no network and no game install.
#[cfg(feature = "bundled")]
const BUNDLED: &str = include_str!("../assets/heroes.json");

/// One hero's identity.
///
/// Display name and art are deliberately absent: they live in [`Names`] and [`HeroArt`]
/// because they have different availability. Reach them through
/// [`HeroCatalog::hero_name`] and [`HeroCatalog::art`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Hero {
    /// Numeric id, as `m_nHeroID` reports it in memory and `m_HeroID` in game data.
    pub id: HeroId,
    /// Internal class name, e.g. `hero_inferno`. Also the localisation token.
    pub class_name: String,
    /// Valve has this hero behind a development flag.
    #[cfg_attr(feature = "serde", serde(default))]
    pub in_development: bool,
    /// Valve has this hero disabled.
    #[cfg_attr(feature = "serde", serde(default))]
    pub disabled: bool,
    /// Valve has shipped the hero's data but not released it: it cannot be picked yet.
    ///
    /// Distinct from [`Hero::in_development`]: a pre-release hero carries neither
    /// development nor disabled flag, only `EHeroDevState_PreRelease`.
    #[cfg_attr(feature = "serde", serde(default))]
    pub pre_release: bool,
    /// The game's own `m_bPlayerSelectable`, when the source knows it.
    ///
    /// `None` from every source but the installed game: neither the vendored snapshot nor
    /// deadlock-api.com publishes it. Where it is known it is the exact predicate for
    /// "appears in a real match", which [`Hero::is_playable`] can only approximate.
    #[cfg_attr(feature = "serde", serde(default))]
    pub player_selectable: Option<bool>,
}

impl Hero {
    /// Whether the hero is selectable in a normal match.
    ///
    /// An approximation from the two flags every source carries, and deliberately *not*
    /// source-dependent: it answers the same way for the same flags whoever supplied
    /// them. Where the installed game was read, [`Hero::selectable`] gives the game's own
    /// answer instead.
    pub fn is_playable(&self) -> bool {
        !self.in_development && !self.disabled && !self.pre_release
    }

    /// The best available answer to "can a player pick this hero".
    ///
    /// The game's own [`Hero::player_selectable`] when it is known, falling back to the
    /// [`Hero::is_playable`] approximation otherwise. Prefer this for display; prefer
    /// `is_playable` when you need an answer that does not change with the source.
    pub fn selectable(&self) -> bool {
        self.player_selectable.unwrap_or_else(|| self.is_playable())
    }
}

/// Art URLs for one hero.
///
/// These point at `assets-bucket.deadlock-api.com`, so rendering them needs network even
/// though the URL itself may be vendored. A field being `None` means the hero has no
/// published art of that kind; a hero having no entry at all means no art source was
/// consulted, which [`HeroCatalog::provenance`] reports separately.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HeroArt {
    /// Small square portrait - the art the in-game scoreboard uses.
    #[cfg_attr(feature = "serde", serde(default))]
    pub portrait: Option<String>,
    /// Full hero card art, for anything bigger than a scoreboard row.
    #[cfg_attr(feature = "serde", serde(default))]
    pub card: Option<String>,
}

impl HeroArt {
    /// Whether any art at all is known.
    pub fn is_empty(&self) -> bool {
        self.portrait.is_none() && self.card.is_none()
    }
}

/// A lookup from hero id to name, art and flags.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(from = "HeroCatalogRepr"))]
pub struct HeroCatalog {
    roster: Vec<Hero>,
    names: Names,
    art: HashMap<HeroId, HeroArt>,
    provenance: Provenance,
    #[cfg_attr(feature = "serde", serde(skip))]
    by_id: HashMap<HeroId, usize>,
    #[cfg_attr(feature = "serde", serde(skip))]
    by_class: HashMap<String, usize>,
}

impl HeroCatalog {
    /// Build from a roster alone, with no names and no art.
    pub fn new(roster: Vec<Hero>) -> Self {
        let mut c = HeroCatalog {
            roster,
            ..Default::default()
        };
        c.reindex();
        c
    }

    /// Build from all three facets at once.
    pub fn from_facets(
        roster: Vec<Hero>,
        names: Names,
        art: HashMap<HeroId, HeroArt>,
        provenance: Provenance,
    ) -> Self {
        let mut c = HeroCatalog {
            roster,
            names,
            art,
            provenance,
            ..Default::default()
        };
        c.reindex();
        c
    }

    /// The vendored snapshot. Does no I/O.
    ///
    /// This is what makes the crate useful with no game installed and no network. It
    /// goes stale as Valve adds heroes, so layer a live source over it when you can.
    ///
    /// ```
    /// use deadlock_data::HeroCatalog;
    /// use deadlock_core::{HeroId, HeroNames};
    ///
    /// // Never fails, no I/O.
    /// let heroes = HeroCatalog::bundled();
    /// assert_eq!(heroes.hero_name(HeroId(1)), Some("Infernus"));
    /// ```
    ///
    /// # Panics
    ///
    /// If the vendored JSON compiled into the binary does not parse. That is a build-time
    /// mistake in this crate rather than anything a caller can cause or handle, and
    /// `bundled_snapshot_is_valid_and_populated` fails first if it ever happens.
    #[cfg(feature = "bundled")]
    pub fn bundled() -> Self {
        let mut c =
            Self::from_json(BUNDLED).expect("vendored hero snapshot is valid; checked by tests");
        c.provenance = Provenance::all(Source::Bundled);
        c.names.set_language(crate::DEFAULT_LANGUAGE);
        c
    }

    /// The vendored snapshot, overlaid with an installed game's own names when one is
    /// given.
    ///
    /// This pairing - bundled first, then whatever the install can add - is what every
    /// consumer actually wants, and it was being written out at each of them. A failed or
    /// absent merge is not an error: the bundled names are still correct, just not
    /// localised. Check [`HeroCatalog::provenance`] to find out which happened.
    ///
    /// Pass `None` to get [`HeroCatalog::bundled`] unchanged.
    ///
    /// ```no_run
    /// use deadlock_data::HeroCatalog;
    /// use std::path::Path;
    ///
    /// let heroes = HeroCatalog::for_game(Some(Path::new("C:/.../Deadlock/game/citadel")));
    /// println!("names from {:?}", heroes.provenance().names);
    /// ```
    #[cfg(feature = "bundled")]
    pub fn for_game(citadel_dir: Option<&Path>) -> Self {
        let mut c = Self::bundled();
        c.overlay_install(citadel_dir);
        c
    }

    /// Overlay an install's own names, when there is an install and this build can read
    /// one. A failure leaves the snapshot's names in place and shows up in the
    /// provenance rather than as an error.
    #[cfg(all(feature = "bundled", feature = "client"))]
    fn overlay_install(&mut self, citadel_dir: Option<&Path>) {
        if let Some(dir) = citadel_dir {
            let _ = self.merge_game_dir(dir);
        }
    }

    /// Without the `client` feature there is nothing to overlay.
    #[cfg(all(feature = "bundled", not(feature = "client")))]
    fn overlay_install(&mut self, _citadel_dir: Option<&Path>) {}

    /// Parse a catalog from JSON in the deadlock-api.com shape.
    ///
    /// Accepts the full API response - unknown fields are ignored - so a cached response
    /// can be fed straight back in.
    pub fn from_json(json: &str) -> Result<Self> {
        let raw: Vec<RawHero> =
            serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;

        let mut roster = Vec::with_capacity(raw.len());
        let mut names = Names::new();
        let mut art = HashMap::with_capacity(raw.len());
        for mut r in raw {
            let id = HeroId(r.id);
            let pre_release = r.pre_release();
            art.insert(
                id,
                HeroArt {
                    portrait: r.portrait_url(),
                    card: r.card_url(),
                },
            );
            // The API echoes the class name back as `name` for anything nobody has
            // localised. Storing that would make an unlocalised entry indistinguishable
            // from a localised one, so it is left out and the class name is used as the
            // fallback at lookup time instead - the same rule `ItemCatalog` follows.
            if !r.name.is_empty() && r.name != r.class_name {
                names.insert(r.class_name.clone(), r.name);
            }
            roster.push(Hero {
                id,
                class_name: r.class_name,
                in_development: r.in_development,
                disabled: r.disabled,
                pre_release,
                // Not published by the snapshot or the API; only the installed game has it.
                player_selectable: None,
            });
        }
        Ok(Self::from_facets(
            roster,
            names,
            art,
            Provenance::all(Source::Api),
        ))
    }

    /// Serialise back to the vendored shape, for caching.
    ///
    /// Pinned to the bundled/API shape deliberately: this is a cache format, and it does
    /// not track whatever layout the game's own data happens to use.
    pub fn to_json(&self) -> Result<String> {
        let raw: Vec<RawHero> = self
            .roster
            .iter()
            .map(|h| {
                let art = self.art.get(&h.id);
                RawHero {
                    id: h.id.0,
                    name: self
                        .names
                        .get(&h.class_name)
                        .unwrap_or(&h.class_name)
                        .to_string(),
                    class_name: h.class_name.clone(),
                    in_development: h.in_development,
                    disabled: h.disabled,
                    pre_release: h.pre_release,
                    development_state: None,
                    portrait: art.and_then(|a| a.portrait.clone()),
                    card: art.and_then(|a| a.card.clone()),
                    images: None,
                }
            })
            .collect();
        serde_json::to_string_pretty(&raw).map_err(|e| Error::Parse(e.to_string()))
    }

    /// Start from the bundled snapshot and overlay an installed game's own names.
    ///
    /// This is the recommended offline path: ids from the snapshot, names from the game,
    /// so the result is correct and in the requested language even for heroes renamed
    /// since this crate was built.
    #[cfg(all(feature = "bundled", feature = "client"))]
    pub fn from_game_dir(citadel_dir: impl AsRef<Path>, language: &str) -> Result<Self> {
        let mut c = Self::bundled();
        c.merge_game_dir_lang(citadel_dir, language)?;
        Ok(c)
    }

    /// Overlay display names from an installed game, in the default language.
    #[cfg(feature = "client")]
    pub fn merge_game_dir(&mut self, citadel_dir: impl AsRef<Path>) -> Result<usize> {
        self.merge_game_dir_lang(citadel_dir, crate::DEFAULT_LANGUAGE)
    }

    /// Overlay display names from an installed game, in a specific language.
    ///
    /// Returns how many names were replaced.
    #[cfg(feature = "client")]
    pub fn merge_game_dir_lang(
        &mut self,
        citadel_dir: impl AsRef<Path>,
        language: &str,
    ) -> Result<usize> {
        let loc = LocalizationFile::load(citadel_dir, HERO_NAMES_BUNDLE, language)?;
        Ok(self.merge_localization(&loc))
    }

    /// Overlay display names from an already-parsed localisation bundle.
    ///
    /// Every `hero_`-prefixed token is taken, not just the ones the roster covers: the
    /// installed game localises more heroes than any id table knows about, and a name
    /// with no roster row is still reachable through
    /// [`HeroCatalog::name_for_class`]. Returns how many names changed.
    #[cfg(feature = "client")]
    pub fn merge_localization(&mut self, loc: &LocalizationFile) -> usize {
        let mut changed = 0;
        for (token, name) in &loc.tokens {
            // A blank translation is a fact about the bundle, not a name. Letting it
            // through would blank a name the snapshot had and count that as a change.
            if name.is_empty() {
                continue;
            }
            if token.starts_with("hero_") && self.names.insert(token.clone(), name.clone()) {
                changed += 1;
            }
        }
        self.names.set_language(&loc.language);
        self.provenance.names = Some(Source::Client);
        changed
    }

    /// Overlay another catalog's roster: ids, class names and flags only.
    ///
    /// Names and art are left exactly as they were. Returns how many rows changed.
    pub fn merge_roster(&mut self, other: &HeroCatalog) -> usize {
        let changed = self.merge_roster_rows(other.roster.iter().cloned());
        if changed > 0 {
            self.provenance.roster = other.provenance.roster;
        }
        changed
    }

    /// Overlay another catalog's display names. Returns how many names changed.
    pub fn merge_names(&mut self, other: &HeroCatalog) -> usize {
        let changed = self.names.merge(&other.names);
        if changed > 0 {
            self.provenance.names = other.provenance.names;
        }
        changed
    }

    /// Overlay another catalog's art.
    ///
    /// Only fields the other catalog actually has are written, so merging a source that
    /// publishes no art cannot erase art already held. Returns how many heroes changed.
    pub fn merge_art(&mut self, other: &HeroCatalog) -> usize {
        let mut changed = 0;
        for (id, incoming) in &other.art {
            let slot = self.art.entry(*id).or_default();
            let mut hit = false;
            if let Some(p) = &incoming.portrait {
                hit |= slot.portrait.as_ref() != Some(p);
                slot.portrait = Some(p.clone());
            }
            if let Some(k) = &incoming.card {
                hit |= slot.card.as_ref() != Some(k);
                slot.card = Some(k.clone());
            }
            changed += usize::from(hit);
        }
        if changed > 0 {
            self.provenance.art = other.provenance.art;
        }
        changed
    }

    /// Merge every facet of another catalog in.
    ///
    /// Used to fold an API refresh over the bundled snapshot. Each facet is merged by its
    /// own rule, so a source missing one of the three cannot blank it.
    pub fn merge(&mut self, other: &HeroCatalog) -> usize {
        self.merge_roster(other) + self.merge_names(other) + self.merge_art(other)
    }

    fn merge_roster_rows(&mut self, rows: impl IntoIterator<Item = Hero>) -> usize {
        let mut changed = 0;
        for h in rows {
            match self.by_id.get(&h.id) {
                Some(&i) => {
                    if self.roster[i] != h {
                        self.roster[i] = h;
                        changed += 1;
                    }
                }
                None => {
                    self.roster.push(h);
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            self.reindex();
        }
        changed
    }

    fn reindex(&mut self) {
        self.roster.sort_by_key(|h| h.id);
        self.by_id = self
            .roster
            .iter()
            .enumerate()
            .map(|(i, h)| (h.id, i))
            .collect();
        self.by_class = self
            .roster
            .iter()
            .enumerate()
            .map(|(i, h)| (h.class_name.clone(), i))
            .collect();
    }

    /// Take the catalog apart into its facets.
    ///
    /// The inverse of [`Self::from_facets`], and what a resolver needs to pick one
    /// facet from one source and another facet from another without cloning tables it
    /// is about to discard.
    pub fn into_facets(self) -> (Vec<Hero>, Names, HashMap<HeroId, HeroArt>, Provenance) {
        (self.roster, self.names, self.art, self.provenance)
    }

    /// Which source answered for each facet.
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// The display name table, keyed by class name.
    pub fn names(&self) -> &Names {
        &self.names
    }

    /// Look up by id.
    pub fn get(&self, id: HeroId) -> Option<&Hero> {
        self.by_id.get(&id).map(|&i| &self.roster[i])
    }

    /// Art for a hero, when an art source was consulted and knows this hero.
    pub fn art(&self, id: HeroId) -> Option<&HeroArt> {
        self.art.get(&id)
    }

    /// Scoreboard portrait URL for a hero, when one is published.
    pub fn portrait(&self, id: HeroId) -> Option<&str> {
        self.art.get(&id)?.portrait.as_deref()
    }

    /// Hero card art URL, when one is published.
    pub fn card(&self, id: HeroId) -> Option<&str> {
        self.art.get(&id)?.card.as_deref()
    }

    /// Look up by internal class name, e.g. `hero_inferno`.
    pub fn by_class_name(&self, class_name: &str) -> Option<&Hero> {
        self.by_class.get(class_name).map(|&i| &self.roster[i])
    }

    /// Display name for an internal class name, including heroes with no roster row.
    pub fn name_for_class(&self, class_name: &str) -> Option<&str> {
        self.names.get(class_name)
    }

    /// Find a hero whose codename appears in an entity class name.
    ///
    /// Live ability entities embed the hero codename - `CCitadel_Ability_Bebop_LaserBeam`
    /// belongs to `hero_bebop` - so this recovers hero identity from memory alone, with
    /// no ids involved.
    pub fn by_entity_class(&self, entity_class: &str) -> Option<&Hero> {
        let lower = entity_class.to_ascii_lowercase();
        self.roster
            .iter()
            .filter_map(|h| {
                let code = h.class_name.strip_prefix("hero_")?;
                // Match on a delimited segment so `hero_lash` does not match
                // `..._Lashing_...`, and prefer the longest match.
                lower
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|seg| seg == code)
                    .then_some((code.len(), h))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, h)| h)
    }

    /// Every hero, ordered by id.
    pub fn all(&self) -> &[Hero] {
        &self.roster
    }

    /// Heroes selectable in a normal match.
    pub fn playable(&self) -> impl Iterator<Item = &Hero> {
        self.roster.iter().filter(|h| h.is_playable())
    }

    /// How many heroes are known.
    pub fn len(&self) -> usize {
        self.roster.len()
    }

    /// Whether the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.roster.is_empty()
    }

    /// Whether a real display name is known, as opposed to falling back to the class
    /// name.
    pub fn is_localised(&self, id: HeroId) -> bool {
        self.get(id)
            .is_some_and(|h| self.names.contains(&h.class_name))
    }

    /// How many entries have a real display name rather than a class-name fallback.
    pub fn localised_count(&self) -> usize {
        self.roster
            .iter()
            .filter(|h| self.names.contains(&h.class_name))
            .count()
    }
}

impl HeroNames for HeroCatalog {
    fn hero_name(&self, id: HeroId) -> Option<&str> {
        let hero = self.get(id)?;
        Some(self.names.get(&hero.class_name).unwrap_or(&hero.class_name))
    }

    fn hero_class_name(&self, id: HeroId) -> Option<&str> {
        self.get(id).map(|h| h.class_name.as_str())
    }
}

/// Deserialisation shadow for [`HeroCatalog`].
///
/// The two indices are derived, not stored, so they are skipped on the wire - which means
/// a plain derive would hand back a catalog whose every lookup returns `None`. Going
/// through this and rebuilding them is what makes a round trip actually usable.
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct HeroCatalogRepr {
    #[serde(default)]
    roster: Vec<Hero>,
    #[serde(default)]
    names: Names,
    #[serde(default)]
    art: HashMap<HeroId, HeroArt>,
    #[serde(default)]
    provenance: Provenance,
}

#[cfg(feature = "serde")]
impl From<HeroCatalogRepr> for HeroCatalog {
    fn from(r: HeroCatalogRepr) -> Self {
        HeroCatalog::from_facets(r.roster, r.names, r.art, r.provenance)
    }
}

/// Wire shape shared by the vendored snapshot and the API response.
///
/// One type for both on purpose. They are not two formats: the vendored file is the API's
/// own shape with the art already resolved and the rest of the response dropped, so the
/// fields coincide except for where the art sits. Keeping them together is what lets
/// [`Self::from_json`] take a cached API response and a vendored file through the same
/// entry point.
///
/// The installed game's data is a genuinely different shape, and it does not appear here:
/// `crate::vdata` walks the decoded `KV3` tree into facets directly, with no wire type in
/// between. So the "one raw type per source" the design called for is what exists - there
/// are two sources behind this one type only because they share a format.
#[derive(serde::Serialize, serde::Deserialize)]
struct RawHero {
    id: u32,
    class_name: String,
    name: String,
    #[serde(default)]
    in_development: bool,
    #[serde(default)]
    disabled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pre_release: bool,
    /// API only: `release` or `pre_release`; absent for heroes the API has no state for.
    #[serde(default, skip_serializing)]
    development_state: Option<String>,
    /// Vendored snapshot only: the already-resolved picks, stored flat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    portrait: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    card: Option<String>,
    /// API only: the same art, nested and under different names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    images: Option<RawHeroImages>,
}

/// The subset of the API's `images` object worth vendoring.
///
/// It publishes a dozen variants per hero (png and webp of each). We take webp, which is
/// what a modern webview wants and roughly half the bytes.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct RawHeroImages {
    #[serde(default)]
    icon_image_small_webp: Option<String>,
    #[serde(default)]
    icon_hero_card_webp: Option<String>,
}

impl RawHero {
    fn pre_release(&self) -> bool {
        self.pre_release || self.development_state.as_deref() == Some("pre_release")
    }

    /// Flat field first - it is the vendored, already-chosen value - then the API shape.
    fn portrait_url(&mut self) -> Option<String> {
        self.portrait.take().or_else(|| {
            self.images
                .as_mut()
                .and_then(|i| i.icon_image_small_webp.take())
        })
    }

    fn card_url(&mut self) -> Option<String> {
        self.card.take().or_else(|| {
            self.images
                .as_mut()
                .and_then(|i| i.icon_hero_card_webp.take())
        })
    }
}

#[cfg(all(test, feature = "bundled"))]
mod tests {
    use super::*;

    #[test]
    fn bundled_snapshot_is_valid_and_populated() {
        let c = HeroCatalog::bundled();
        assert!(c.len() >= 50, "only {} heroes", c.len());
        assert!(c.playable().count() >= 30);
        for h in c.all() {
            assert_eq!(c.get(h.id).map(|x| &x.class_name), Some(&h.class_name));
        }
    }

    #[test]
    fn bundled_reports_itself_as_the_source_of_every_facet() {
        let p = HeroCatalog::bundled().provenance();
        assert_eq!(p, Provenance::all(Source::Bundled));
    }

    /// The twelve heroes seen in a real match must all resolve.
    #[test]
    fn resolves_ids_from_a_live_match() {
        let c = HeroCatalog::bundled();
        let expected = [
            (65, "Venator"),
            (14, "Holliday"),
            (67, "Paige"),
            (1, "Infernus"),
            (81, "Celeste"),
            (19, "Shiv"),
            (31, "Lash"),
            (63, "Mina"),
            (60, "Sinclair"),
            (15, "Bebop"),
            (7, "Wraith"),
            (52, "Mirage"),
        ];
        for (id, name) in expected {
            assert_eq!(c.hero_name(HeroId(id)), Some(name), "hero {id}");
        }
    }

    #[test]
    fn unknown_ids_do_not_panic() {
        let c = HeroCatalog::bundled();
        assert_eq!(c.hero_name(HeroId(9999)), None);
        assert_eq!(c.display_name(HeroId(9999)), "hero 9999");
        assert_eq!(c.hero_name(HeroId(0)), None);
    }

    #[test]
    fn class_name_lookup() {
        let c = HeroCatalog::bundled();
        assert!(c.by_class_name("hero_inferno").is_some());
        assert_eq!(c.name_for_class("hero_inferno"), Some("Infernus"));
        assert_eq!(c.name_for_class("hero_gigawatt"), Some("Seven"));
        assert!(c.by_class_name("hero_nonexistent").is_none());
        assert_eq!(c.name_for_class("hero_nonexistent"), None);
    }

    /// Hero identity recovered from a live ability entity's class name.
    #[test]
    fn identifies_heroes_from_entity_class_names() {
        let c = HeroCatalog::bundled();
        for (entity, expect) in [
            ("CCitadel_Ability_Bebop_LaserBeam", "Bebop"),
            ("CCitadel_Ability_Lash_Flog", "Lash"),
            ("CCitadel_Ability_Unicorn_DazzlingOrb", "Celeste"),
            ("CCitadel_Ability_Wraith_RapidFire", "Wraith"),
            ("CCitadel_Ability_Magician_AnimalHexArea", "Sinclair"),
            ("CCitadel_Ability_Shiv_KillingBlow", "Shiv"),
        ] {
            match c.by_entity_class(entity) {
                Some(h) => assert_eq!(c.name_for_class(&h.class_name), Some(expect), "{entity}"),
                None => panic!("no hero matched {entity}"),
            }
        }
        assert!(c.by_entity_class("CCitadel_Ability_Sprint").is_none());
        assert!(c.by_entity_class("C_CitadelPlayerPawn").is_none());
        assert!(
            c.by_entity_class("CCitadel_Modifier_Airheart_Mark")
                .is_none()
        );
    }

    #[cfg(feature = "client")]
    #[test]
    fn localization_overlay_replaces_names() {
        let mut c = HeroCatalog::bundled();
        let loc = LocalizationFile::parse(
            "\t\"hero_inferno:n\"\t\"Brandmeister\"\n\t\"hero_bebop:n\"\t\"Bebop\"\n",
            "german",
        );
        let changed = c.merge_localization(&loc);
        assert_eq!(changed, 1);
        assert_eq!(c.hero_name(HeroId(1)), Some("Brandmeister"));
        assert_eq!(c.hero_name(HeroId(7)), Some("Wraith"));
        assert_eq!(c.provenance().names, Some(Source::Client));
        assert_eq!(c.provenance().roster, Some(Source::Bundled));
    }

    /// A name the id table has no row for is still reachable by class name.
    #[cfg(feature = "client")]
    #[test]
    fn names_without_a_roster_row_are_kept() {
        let mut c = HeroCatalog::bundled();
        assert!(c.by_class_name("hero_airheart").is_none());
        let loc = LocalizationFile::parse("\t\"hero_airheart:n\"\t\"Airheart\"\n", "english");
        assert_eq!(c.merge_localization(&loc), 1);
        assert_eq!(c.name_for_class("hero_airheart"), Some("Airheart"));
        assert!(c.by_class_name("hero_airheart").is_none());
    }

    #[test]
    fn a_pre_release_hero_is_not_playable_even_with_no_other_flag() {
        let c = HeroCatalog::from_json(
            r#"[{"id":88,"class_name":"hero_baba","name":"Baba","development_state":"pre_release"},
                {"id":1,"class_name":"hero_inferno","name":"Infernus","development_state":"release"}]"#,
        )
        .expect("parse");
        let baba = c.all().iter().find(|h| h.id == HeroId(88)).expect("baba");
        assert!(baba.pre_release && !baba.is_playable() && !baba.selectable());
        assert!(c.all().iter().any(|h| h.id == HeroId(1) && h.is_playable()));
    }

    #[test]
    fn a_pre_release_flag_survives_the_cache_round_trip() {
        let c = HeroCatalog::from_json(
            r#"[{"id":88,"class_name":"hero_baba","name":"Baba","pre_release":true}]"#,
        )
        .expect("parse");
        let again = HeroCatalog::from_json(&c.to_json().expect("serialise")).expect("parse");
        assert!(again.all()[0].pre_release);
    }

    #[test]
    fn merge_adds_unknown_heroes() {
        let mut c = HeroCatalog::new(vec![Hero {
            id: HeroId(1),
            class_name: "hero_inferno".into(),
            in_development: false,
            disabled: false,
            pre_release: false,
            player_selectable: None,
        }]);
        let other = HeroCatalog::new(vec![Hero {
            id: HeroId(999),
            class_name: "hero_future".into(),
            in_development: true,
            disabled: false,
            pre_release: false,
            player_selectable: None,
        }]);
        assert_eq!(c.merge_roster(&other), 1);
        assert_eq!(c.len(), 2);
        assert_eq!(c.get(HeroId(999)).map(|h| h.in_development), Some(true));
        assert!(c.all().windows(2).all(|w| w[0].id <= w[1].id));
    }

    /// The regression the facet split exists to prevent: a roster-only source must not
    /// blank art that a previous source supplied.
    #[test]
    fn merging_an_artless_roster_keeps_existing_art() {
        let mut c = HeroCatalog::bundled();
        let before = c.portrait(HeroId(1)).map(str::to_owned);
        assert!(before.is_some(), "fixture needs art to be meaningful");

        let roster_only = HeroCatalog::new(vec![Hero {
            id: HeroId(1),
            class_name: "hero_inferno".into(),
            in_development: false,
            disabled: true,
            pre_release: false,
            player_selectable: None,
        }]);
        c.merge(&roster_only);

        assert_eq!(c.portrait(HeroId(1)).map(str::to_owned), before);
        assert_eq!(c.hero_name(HeroId(1)), Some("Infernus"));
        assert_eq!(c.get(HeroId(1)).map(|h| h.disabled), Some(true));
    }

    /// An unlocalised hero falls back to its class name, and says so.
    ///
    /// The API echoes the class name back as `name` for anything nobody has localised.
    /// Storing that makes an unlocalised entry indistinguishable from a localised one -
    /// `hero_name` answers `Some("hero_venator")`, which has the shape of a display name
    /// and is not one.
    ///
    /// Not hypothetical, and measured: 217 of the 726 entries in the shipped `items.json`
    /// carry `name == class_name`. `heroes.json` currently has none, which is exactly why
    /// the hero path never had the guard the item path has carried all along.
    #[test]
    fn an_unlocalised_hero_falls_back_to_the_class_name_and_says_so() {
        let json = r#"[{"id":65,"class_name":"hero_venator","name":"hero_venator"}]"#;
        let c = HeroCatalog::from_json(json).expect("parse");

        assert_eq!(c.hero_name(HeroId(65)), Some("hero_venator"));
        assert!(!c.is_localised(HeroId(65)));
        assert_eq!(c.localised_count(), 0);
        assert_eq!(c.name_for_class("hero_venator"), None);
    }

    /// A localised hero is still reported as localised.
    #[test]
    fn a_localised_hero_is_counted_as_one() {
        let json = r#"[{"id":1,"class_name":"hero_inferno","name":"Infernus"}]"#;
        let c = HeroCatalog::from_json(json).expect("parse");
        assert_eq!(c.hero_name(HeroId(1)), Some("Infernus"));
        assert!(c.is_localised(HeroId(1)));
        assert_eq!(c.localised_count(), 1);
    }

    /// A round trip must not launder an absent name into a present one.
    ///
    /// `to_json` writes the class name for an entry that has no display name, so without
    /// the guard on the way back in, one save and load turns "nobody has named this hero"
    /// into "this hero is called `hero_venator`".
    #[test]
    fn a_round_trip_keeps_an_unlocalised_hero_unlocalised() {
        let json = r#"[{"id":65,"class_name":"hero_venator","name":"hero_venator"}]"#;
        let c = HeroCatalog::from_json(json).expect("parse");
        let back = HeroCatalog::from_json(&c.to_json().expect("write")).expect("reparse");
        assert!(!back.is_localised(HeroId(65)), "laundered by a round trip");
        assert_eq!(back.localised_count(), 0);
    }

    /// An empty localisation value must not wipe a good name.
    ///
    /// `LocalizationFile` keeps `"hero_inferno:n" ""` as `Some("")` on purpose - a blank
    /// translation is a fact about the bundle. Letting it through here would blank a name
    /// the snapshot had, and count the blanking as a successful replacement.
    #[cfg(feature = "client")]
    #[test]
    fn empty_hero_localisation_values_are_ignored() {
        let mut c = HeroCatalog::bundled();
        let before = c.hero_name(HeroId(1)).map(str::to_owned);
        let loc = LocalizationFile::parse("\t\"hero_inferno:n\"\t\"\"\n", "english");
        assert_eq!(c.merge_localization(&loc), 0);
        assert_eq!(c.hero_name(HeroId(1)).map(str::to_owned), before);
    }
    #[test]
    fn json_roundtrip() {
        let c = HeroCatalog::bundled();
        let json = c.to_json().unwrap();
        let back = HeroCatalog::from_json(&json).unwrap();
        assert_eq!(back.len(), c.len());
        assert_eq!(back.hero_name(HeroId(1)), c.hero_name(HeroId(1)));
    }

    #[test]
    fn tolerates_the_full_api_response_shape() {
        let json = r#"[{"id":1,"class_name":"hero_inferno","name":"Infernus",
                        "description":{"lore":"..."},"images":{},"items":{}}]"#;
        let c = HeroCatalog::from_json(json).unwrap();
        assert_eq!(c.hero_name(HeroId(1)), Some("Infernus"));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_roundtrip_rebuilds_the_indices() {
        let c = HeroCatalog::bundled();
        let json = serde_json::to_string(&c).expect("write");
        let back: HeroCatalog = serde_json::from_str(&json).expect("read");
        assert_eq!(back.len(), c.len());
        assert_eq!(back.hero_name(HeroId(1)), Some("Infernus"));
        assert!(back.by_class_name("hero_inferno").is_some());
        assert_eq!(back.portrait(HeroId(1)), c.portrait(HeroId(1)));
        assert_eq!(back.provenance(), c.provenance());
    }
}

#[cfg(all(test, feature = "bundled"))]
mod art_tests {
    use super::*;

    #[test]
    fn most_heroes_carry_portrait_and_card_art() {
        let c = HeroCatalog::bundled();
        let p = c
            .all()
            .iter()
            .filter(|h| c.portrait(h.id).is_some())
            .count();
        let k = c.all().iter().filter(|h| c.card(h.id).is_some()).count();
        assert!(p >= 50, "only {p} of {} have portraits", c.len());
        assert!(k >= 50, "only {k} of {} have cards", c.len());
        let url = c.portrait(HeroId(1)).expect("portrait");
        assert!(url.starts_with("https://"), "{url}");
        assert!(url.ends_with(".webp"), "want webp, got {url}");
    }

    #[test]
    fn api_shape_lifts_art_out_of_the_nested_images_object() {
        let json = r#"[{"id":1,"class_name":"hero_inferno","name":"Infernus","images":{
            "icon_image_small":"https://x/inferno_sm.png",
            "icon_image_small_webp":"https://x/inferno_sm.webp",
            "icon_hero_card_webp":"https://x/inferno_card.webp",
            "minimap_image_webp":"https://x/inferno_mm.webp"}}]"#;
        let c = HeroCatalog::from_json(json).expect("parse");
        assert_eq!(c.portrait(HeroId(1)), Some("https://x/inferno_sm.webp"));
        assert_eq!(c.card(HeroId(1)), Some("https://x/inferno_card.webp"));
    }

    #[test]
    fn art_survives_a_json_roundtrip() {
        let c = HeroCatalog::bundled();
        let back = HeroCatalog::from_json(&c.to_json().expect("write")).expect("read");
        assert_eq!(back.portrait(HeroId(1)), c.portrait(HeroId(1)));
        assert_eq!(back.card(HeroId(1)), c.card(HeroId(1)));
    }

    /// A hero with no published art and a hero nobody looked art up for are different
    /// answers, and the catalog has to be able to say which.
    #[test]
    fn absent_art_and_unconsulted_art_are_distinguishable() {
        let roster_only = HeroCatalog::new(vec![Hero {
            id: HeroId(1),
            class_name: "hero_inferno".into(),
            in_development: false,
            disabled: false,
            pre_release: false,
            player_selectable: None,
        }]);
        assert!(roster_only.art(HeroId(1)).is_none());
        assert_eq!(roster_only.provenance().art, None);

        let looked_up =
            HeroCatalog::from_json(r#"[{"id":1,"class_name":"hero_inferno","name":"Infernus"}]"#)
                .expect("parse");
        assert_eq!(looked_up.art(HeroId(1)), Some(&HeroArt::default()));
        assert_eq!(looked_up.provenance().art, Some(Source::Api));
    }

    /// The vendored snapshot is the roster minus the sandbox placeholders.
    ///
    /// A sandbox placeholder is a hero that is in development **and not disabled**; today
    /// that is `hero_genericperson` alone. Every unreleased hero - `hero_bomber`,
    /// `hero_vandal` and the rest - carries both flags, and `hero_targetdummy` had its
    /// development flag cleared by the "City Never Sleeps" patch, so it is vendored. The
    /// rule is `in_development && !disabled`, and this pins it: a patch that adds a
    /// sandbox tool, or that ships a hero without clearing the flag, fails here rather
    /// than silently changing what the snapshot means.
    #[cfg(feature = "vpk")]
    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn the_vendored_snapshot_is_the_roster_minus_the_sandbox_placeholders() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let live = crate::vdata::hero_roster(&dir).expect("hero roster");
        let bundled = HeroCatalog::bundled();

        let name = |h: &Hero| format!("{} ({})", h.class_name, h.id.0);
        let sandbox = |h: &Hero| h.in_development && !h.disabled;

        let mut expected: Vec<String> = live.iter().filter(|h| !sandbox(h)).map(name).collect();
        let mut vendored: Vec<String> = bundled.all().iter().map(name).collect();
        expected.sort();
        vendored.sort();
        assert_eq!(
            expected, vendored,
            "the vendored snapshot no longer matches the roster minus sandbox placeholders;              regenerate crates/deadlock-data/assets/heroes.json"
        );

        let ids: std::collections::HashSet<u32> = bundled.all().iter().map(|h| h.id.0).collect();
        let missing: Vec<String> = live
            .iter()
            .filter(|h| h.selectable() && !ids.contains(&h.id.0))
            .map(name)
            .collect();
        assert!(
            missing.is_empty(),
            "selectable heroes absent from the snapshot: {missing:#?}"
        );
    }

    /// The installed game no longer ships `m_bPlayerSelectable`.
    ///
    /// Through build 6683 it was the exact predicate for "appears in a real match", and it
    /// disagreed with `m_bDisabled` for six heroes. The "City Never Sleeps" patch dropped
    /// the key from every hero block and added `m_eHeroDevelopmentState` instead, so
    /// [`Hero::selectable`] now answers from the flags alone. If the key comes back this
    /// fails, and the fallback in `selectable` is the thing to revisit.
    #[cfg(feature = "vpk")]
    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn the_installed_game_no_longer_says_who_is_selectable() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let live = crate::vdata::hero_roster(&dir).expect("hero roster");
        assert!(live.iter().all(|h| h.player_selectable.is_none()));
        assert!(live.iter().all(|h| h.selectable() == h.is_playable()));
    }
}
