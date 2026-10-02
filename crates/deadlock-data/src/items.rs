//! The item catalog: abilities, shop upgrades and weapons.
//!
//! Deadlock puts all three in one id space, so a hero's ability and a shop item are
//! looked up the same way; [`ItemKind`] separates them.
//!
//! Split into facets exactly like [`crate::HeroCatalog`]: a roster of [`Item`] identity,
//! a [`Names`] table keyed by class name, and an art table of [`ItemArt`]. Item names are
//! spread across several localisation bundles (upgrades in `citadel_gc_mod_names`,
//! abilities in `citadel_heroes`), so the offline path reads all of them; see
//! [`ITEM_NAME_BUNDLES`].

use std::collections::HashMap;
#[cfg(any(feature = "bundled", feature = "client"))]
use std::path::Path;

use deadlock_core::{ItemId, ItemKind, ItemNames};

use crate::error::{Error, Result};
#[cfg(feature = "client")]
use crate::localization::LocalizationFile;
use crate::names::Names;
use crate::source::{Provenance, Source};

/// Snapshot taken from deadlock-api.com, vendored so the crate works offline.
#[cfg(feature = "bundled")]
const BUNDLED: &str = include_str!("../assets/items.json");

/// Localisation bundles that carry item and ability display names.
///
/// Together these cover roughly 500 of the ~726 known ids; the remainder are unreleased
/// or internal entries which the API also leaves unlocalised.
pub const ITEM_NAME_BUNDLES: &[&str] = &[
    "citadel_gc_mod_names",
    "citadel_heroes",
    "citadel_main",
    "citadel_mods",
    "citadel_attributes",
];

/// One item, ability or weapon.
///
/// Display name and art are held separately - see [`crate::HeroCatalog`] for why - and
/// reached through [`ItemCatalog::item_name`] and [`ItemCatalog::image`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Item {
    /// Numeric id, as it appears in `m_vecUpgrades` and `m_vecAbilityUpgradeState`.
    ///
    /// Source 2 derives this from the class name: it is `CUtlStringToken`, `MurmurHash2` of
    /// the lowercased class name with seed `0x31415926`.
    pub id: ItemId,
    /// Internal class name, e.g. `upgrade_clip_size`. Also the localisation token.
    pub class_name: String,
    /// Ability, upgrade or weapon.
    pub kind: ItemKind,
}

/// Art URLs for one item.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ItemArt {
    /// Icon URL, when the asset service publishes one.
    ///
    /// The API exposes two icon sets and they are not the same artwork. `shop_image` is
    /// the current shop art and is what this holds for the 184 purchasable upgrades;
    /// `image` is the older icon set, and for those same items points somewhere else
    /// entirely (`items/weapon/basic_magazine` vs `upgrades/mods_weapon/clip_size`).
    /// Abilities and weapon sets have no shop art, so they fall back to `image`.
    ///
    /// Points at `assets-bucket.deadlock-api.com`, so displaying it needs network even
    /// though the URL itself is vendored. Not derivable from `class_name`, which is why
    /// it is stored rather than constructed.
    #[cfg_attr(feature = "serde", serde(default))]
    pub image: Option<String>,
}

/// A lookup from item id to name, kind and art.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(from = "ItemCatalogRepr"))]
pub struct ItemCatalog {
    roster: Vec<Item>,
    names: Names,
    art: HashMap<ItemId, ItemArt>,
    provenance: Provenance,
    #[cfg_attr(feature = "serde", serde(skip))]
    by_id: HashMap<ItemId, usize>,
    #[cfg_attr(feature = "serde", serde(skip))]
    by_class: HashMap<String, usize>,
}

impl ItemCatalog {
    /// Build from a roster alone, with no names and no art.
    pub fn new(roster: Vec<Item>) -> Self {
        let mut c = ItemCatalog {
            roster,
            ..Default::default()
        };
        c.reindex();
        c
    }

    /// Build from all three facets at once.
    pub fn from_facets(
        roster: Vec<Item>,
        names: Names,
        art: HashMap<ItemId, ItemArt>,
        provenance: Provenance,
    ) -> Self {
        let mut c = ItemCatalog {
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
    /// # Panics
    ///
    /// If the vendored JSON compiled into the binary does not parse - a build-time mistake
    /// in this crate, not something a caller can cause, and covered by this module's tests.
    #[cfg(feature = "bundled")]
    pub fn bundled() -> Self {
        let mut c =
            Self::from_json(BUNDLED).expect("vendored item snapshot is valid; checked by tests");
        c.provenance = Provenance::all(Source::Bundled);
        c.names.set_language(crate::DEFAULT_LANGUAGE);
        c
    }

    /// The vendored snapshot, overlaid with an installed game's own names when one is
    /// given. See [`crate::HeroCatalog::for_game`].
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

    /// Parse from JSON in the deadlock-api.com shape. Unknown fields are ignored, so a
    /// cached full response can be fed straight back in.
    pub fn from_json(json: &str) -> Result<Self> {
        let raw: Vec<RawItem> =
            serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;

        let mut roster = Vec::with_capacity(raw.len());
        let mut names = Names::new();
        let mut art = HashMap::with_capacity(raw.len());
        for mut r in raw {
            let id = ItemId(r.id);
            art.insert(
                id,
                ItemArt {
                    image: r.icon_url(),
                },
            );
            // The API echoes the class name back as `name` for anything nobody has
            // localised. Storing that would make an unlocalised entry indistinguishable
            // from a localised one, so it is left out and the class name is used as the
            // fallback at lookup time instead.
            if !r.name.is_empty() && r.name != r.class_name {
                names.insert(r.class_name.clone(), r.name);
            }
            roster.push(Item {
                id,
                class_name: r.class_name,
                kind: ItemKind::from_str_lossy(&r.r#type),
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
    /// Pinned to the bundled/API shape: this is a cache format, not a mirror of whatever
    /// layout the game's own data uses.
    pub fn to_json(&self) -> Result<String> {
        let raw: Vec<RawItem> = self
            .roster
            .iter()
            .map(|i| RawItem {
                id: i.id.0,
                name: self
                    .names
                    .get(&i.class_name)
                    .unwrap_or(&i.class_name)
                    .to_string(),
                class_name: i.class_name.clone(),
                r#type: i.kind.as_str().to_string(),
                icon: self.art.get(&i.id).and_then(|a| a.image.clone()),
                shop_image_webp: None,
                image_webp: None,
            })
            .collect();
        serde_json::to_string_pretty(&raw).map_err(|e| Error::Parse(e.to_string()))
    }

    /// Bundled snapshot overlaid with an installed game's own names.
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

    /// Overlay display names from an installed game, reading every bundle in
    /// [`ITEM_NAME_BUNDLES`].
    ///
    /// Missing bundles are skipped rather than failing: Valve moves tokens between them,
    /// and a partial overlay is better than none. Returns how many names were replaced.
    #[cfg(feature = "client")]
    pub fn merge_game_dir_lang(
        &mut self,
        citadel_dir: impl AsRef<Path>,
        language: &str,
    ) -> Result<usize> {
        let dir = citadel_dir.as_ref();
        let mut changed = 0;
        let mut any = false;
        for bundle in ITEM_NAME_BUNDLES {
            if let Ok(loc) = LocalizationFile::load(dir, bundle, language) {
                any = true;
                changed += self.merge_localization(&loc);
            }
        }
        if !any {
            return Err(Error::GameNotFound);
        }
        Ok(changed)
    }

    /// Overlay display names from an already-parsed localisation bundle.
    ///
    /// Only tokens the roster has a row for are taken. Unlike hero bundles, which hold
    /// nothing but hero names, the item bundles are general-purpose - `citadel_main`
    /// alone carries several thousand interface strings - so an unfiltered import would
    /// bury the item names in unrelated text.
    #[cfg(feature = "client")]
    pub fn merge_localization(&mut self, loc: &LocalizationFile) -> usize {
        let mut changed = 0;
        for i in 0..self.roster.len() {
            let class_name = &self.roster[i].class_name;
            let Some(name) = loc.get(class_name) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let (class_name, name) = (class_name.clone(), name.to_string());
            if self.names.insert(class_name, name) {
                changed += 1;
            }
        }
        self.names.set_language(&loc.language);
        self.provenance.names = Some(Source::Client);
        changed
    }

    /// Overlay another catalog's roster: ids, class names and kinds only.
    ///
    /// Names and art are left exactly as they were. Returns how many rows changed.
    pub fn merge_roster(&mut self, other: &ItemCatalog) -> usize {
        let mut changed = 0;
        for i in &other.roster {
            match self.by_id.get(&i.id) {
                Some(&at) => {
                    if self.roster[at] != *i {
                        self.roster[at] = i.clone();
                        changed += 1;
                    }
                }
                None => {
                    self.roster.push(i.clone());
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            self.reindex();
            self.provenance.roster = other.provenance.roster;
        }
        changed
    }

    /// Overlay another catalog's display names. Returns how many names changed.
    pub fn merge_names(&mut self, other: &ItemCatalog) -> usize {
        let changed = self.names.merge(&other.names);
        if changed > 0 {
            self.provenance.names = other.provenance.names;
        }
        changed
    }

    /// Overlay another catalog's art.
    ///
    /// Only fields the other catalog actually has are written, so merging a source that
    /// publishes no art cannot erase art already held. Returns how many items changed.
    pub fn merge_art(&mut self, other: &ItemCatalog) -> usize {
        let mut changed = 0;
        for (id, incoming) in &other.art {
            let Some(image) = &incoming.image else {
                continue;
            };
            let slot = self.art.entry(*id).or_default();
            if slot.image.as_ref() != Some(image) {
                slot.image = Some(image.clone());
                changed += 1;
            }
        }
        if changed > 0 {
            self.provenance.art = other.provenance.art;
        }
        changed
    }

    /// Merge every facet of another catalog in.
    ///
    /// Each facet is merged by its own rule, so a source missing one of the three cannot
    /// blank it.
    pub fn merge(&mut self, other: &ItemCatalog) -> usize {
        self.merge_roster(other) + self.merge_names(other) + self.merge_art(other)
    }

    fn reindex(&mut self) {
        self.roster.sort_by_key(|i| i.id);
        self.by_id = self
            .roster
            .iter()
            .enumerate()
            .map(|(n, i)| (i.id, n))
            .collect();
        self.by_class = self
            .roster
            .iter()
            .enumerate()
            .map(|(n, i)| (i.class_name.clone(), n))
            .collect();
    }

    /// Take the catalog apart into its facets.
    ///
    /// The inverse of [`Self::from_facets`], and what a resolver needs to pick one
    /// facet from one source and another facet from another without cloning tables it
    /// is about to discard.
    pub fn into_facets(self) -> (Vec<Item>, Names, HashMap<ItemId, ItemArt>, Provenance) {
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
    pub fn get(&self, id: ItemId) -> Option<&Item> {
        self.by_id.get(&id).map(|&i| &self.roster[i])
    }

    /// Art for an item, when an art source was consulted and knows this item.
    pub fn art(&self, id: ItemId) -> Option<&ItemArt> {
        self.art.get(&id)
    }

    /// Icon URL for an item, when one is published.
    pub fn image(&self, id: ItemId) -> Option<&str> {
        self.art.get(&id)?.image.as_deref()
    }

    /// Look up by internal class name.
    pub fn by_class_name(&self, class_name: &str) -> Option<&Item> {
        self.by_class.get(class_name).map(|&i| &self.roster[i])
    }

    /// Display name for an internal class name, when one has been localised.
    pub fn name_for_class(&self, class_name: &str) -> Option<&str> {
        self.names.get(class_name)
    }

    /// Whether a real display name is known, as opposed to falling back to the class
    /// name.
    pub fn is_localised(&self, id: ItemId) -> bool {
        self.get(id)
            .is_some_and(|i| self.names.contains(&i.class_name))
    }

    /// Every item, ordered by id.
    pub fn all(&self) -> &[Item] {
        &self.roster
    }

    /// Items of one kind.
    pub fn of_kind(&self, kind: ItemKind) -> impl Iterator<Item = &Item> {
        self.roster.iter().filter(move |i| i.kind == kind)
    }

    /// How many items are known.
    pub fn len(&self) -> usize {
        self.roster.len()
    }

    /// Whether the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.roster.is_empty()
    }

    /// How many entries have a real display name rather than a class-name fallback.
    pub fn localised_count(&self) -> usize {
        self.roster
            .iter()
            .filter(|i| self.names.contains(&i.class_name))
            .count()
    }
}

impl ItemNames for ItemCatalog {
    /// The localised name, falling back to the class name for entries nobody has
    /// translated - which is what the API itself returns for them.
    fn item_name(&self, id: ItemId) -> Option<&str> {
        let item = self.get(id)?;
        Some(self.names.get(&item.class_name).unwrap_or(&item.class_name))
    }

    fn item_class_name(&self, id: ItemId) -> Option<&str> {
        self.get(id).map(|i| i.class_name.as_str())
    }

    fn item_kind(&self, id: ItemId) -> Option<ItemKind> {
        self.get(id).map(|i| i.kind)
    }
}

/// Deserialisation shadow for [`ItemCatalog`]. See [`crate::heroes::HeroCatalog`]'s.
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct ItemCatalogRepr {
    #[serde(default)]
    roster: Vec<Item>,
    #[serde(default)]
    names: Names,
    #[serde(default)]
    art: HashMap<ItemId, ItemArt>,
    #[serde(default)]
    provenance: Provenance,
}

#[cfg(feature = "serde")]
impl From<ItemCatalogRepr> for ItemCatalog {
    fn from(r: ItemCatalogRepr) -> Self {
        ItemCatalog::from_facets(r.roster, r.names, r.art, r.provenance)
    }
}

/// Wire shape shared by the vendored snapshot and the API response.
///
/// One type for both on purpose. They are not two formats: the vendored file is the API's
/// own shape with the icon already resolved and the rest of the response dropped, so the
/// fields coincide except for where the icon sits. Keeping them together is what lets
/// [`Self::from_json`] take a cached API response and a vendored file through the same
/// entry point.
///
/// The installed game's data is a genuinely different shape, and it does not appear here:
/// `crate::vdata` walks the decoded `KV3` tree into facets directly, with no wire type in
/// between. So the "one raw type per source" the design called for is what exists - there
/// are two sources behind this one type only because they share a format.
#[derive(serde::Serialize, serde::Deserialize)]
struct RawItem {
    id: u32,
    class_name: String,
    name: String,
    r#type: String,
    /// Only the vendored snapshot writes this: it is the already-resolved pick. Named
    /// `icon` rather than `image` precisely so it cannot collide with the API's own
    /// `image` field, which is the legacy art we do *not* want.
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    shop_image_webp: Option<String>,
    #[serde(default)]
    image_webp: Option<String>,
}

impl RawItem {
    /// Current shop art when there is any, else the older icon set.
    ///
    /// See [`ItemArt::image`] for why the two sets are not interchangeable.
    fn icon_url(&mut self) -> Option<String> {
        self.icon
            .take()
            .or_else(|| self.shop_image_webp.take())
            .or_else(|| self.image_webp.take())
    }
}

#[cfg(all(test, feature = "bundled"))]
mod tests {
    use super::*;

    #[test]
    fn most_items_carry_an_icon_url() {
        let c = ItemCatalog::bundled();
        let with = c.all().iter().filter(|i| c.image(i.id).is_some()).count();
        assert!(with > 600, "only {with} of {} have icons", c.len());
        let url = c.image(ItemId(3977876567)).expect("icon");
        assert!(url.starts_with("https://"), "{url}");
    }

    #[test]
    fn purchasable_items_use_shop_art_not_the_legacy_icon() {
        let c = ItemCatalog::bundled();
        let id = c.by_class_name("upgrade_clip_size").expect("present").id;
        let url = c.image(id).expect("icon");
        assert!(
            url.contains("/images/items/"),
            "legacy art leaked through: {url}"
        );

        let shop = c
            .all()
            .iter()
            .filter(|i| c.image(i.id).is_some_and(|u| u.contains("/images/items/")))
            .count();
        assert!(shop > 150, "only {shop} items resolved to shop art");
    }

    #[test]
    fn api_response_prefers_shop_art_over_the_legacy_image_field() {
        let json = r#"[{"id":1,"class_name":"upgrade_clip_size","name":"Extended Magazine",
            "type":"upgrade","image":"https://x/upgrades/mods_weapon/clip_size.png",
            "image_webp":"https://x/upgrades/mods_weapon/clip_size.webp",
            "shop_image":"https://x/items/weapon/basic_magazine.png",
            "shop_image_webp":"https://x/items/weapon/basic_magazine.webp"}]"#;
        let c = ItemCatalog::from_json(json).expect("parse");
        assert_eq!(
            c.image(ItemId(1)),
            Some("https://x/items/weapon/basic_magazine.webp")
        );
    }

    /// Every bundled id must equal the token Source 2 derives from its class name.
    ///
    /// This is the guard on the snapshot, not on the hash. The vendored data comes from
    /// deadlock-api.com, which published `2839987102` for `gunslinger_demonMark` against
    /// the correct `3673718559`. That one went unnoticed because Gunslinger is a disabled
    /// dev hero: Infernal Brand is only reachable in Hero Labs or a sandbox match, so no
    /// ordinary game ever exercised the wrong id. Re-deriving is cheap enough to run over
    /// the whole snapshot, which closes the class of defect rather than the one instance.
    ///
    /// Every mismatch is collected before failing - a bulk divergence after a data refresh
    /// is only legible as a list.
    #[test]
    fn every_bundled_item_id_is_the_hash_of_its_own_class_name() {
        let c = ItemCatalog::bundled();
        assert!(!c.all().is_empty(), "nothing checked");

        let stale: Vec<String> = c
            .all()
            .iter()
            .filter_map(|i| {
                let derived = ItemId::from_class_name(&i.class_name);
                (derived != i.id)
                    .then(|| format!("{} is {} but hashes to {derived}", i.class_name, i.id))
            })
            .collect();

        assert!(
            stale.is_empty(),
            "{} of {} bundled ids disagree with their class name: {stale:#?}",
            stale.len(),
            c.len()
        );
    }

    /// Defect H1, pinned: the id that was stale, and the name it resolves to.
    #[test]
    fn the_previously_stale_infernal_brand_id_resolves() {
        let c = ItemCatalog::bundled();
        let id = ItemId::from_class_name("gunslinger_demonMark");
        assert_eq!(id, ItemId(3673718559));
        assert_eq!(c.item_name(id), Some("Infernal Brand"));
        assert_eq!(c.item_kind(id), Some(ItemKind::Ability));
    }

    #[test]
    fn bundled_snapshot_is_valid_and_populated() {
        let c = ItemCatalog::bundled();
        assert!(c.len() >= 700, "only {} items", c.len());
        assert!(c.of_kind(ItemKind::Ability).count() >= 300);
        assert!(c.of_kind(ItemKind::Upgrade).count() >= 200);
        assert!(c.of_kind(ItemKind::Weapon).count() >= 50);
        for i in c.all() {
            assert_eq!(c.get(i.id).map(|x| &x.class_name), Some(&i.class_name));
        }
    }

    /// Items read from a real player's `m_vecUpgrades` must resolve.
    #[test]
    fn resolves_purchased_items_from_a_live_match() {
        let c = ItemCatalog::bundled();
        for (id, name) in [
            (3977876567u32, "Kinetic Dash"),
            (2064029594, "Opening Rounds"),
            (4104549924, "Swift Striker"),
            (2566692615, "Healing Booster"),
            (1009965641, "Monster Rounds"),
            (2407033488, "Intensifying Magazine"),
        ] {
            assert_eq!(c.item_name(ItemId(id)), Some(name), "item {id}");
            assert_eq!(c.item_kind(ItemId(id)), Some(ItemKind::Upgrade));
        }
    }

    /// Ability ids from `m_vecAbilityUpgradeState` resolve to that hero's abilities.
    #[test]
    fn resolves_ability_upgrade_ids() {
        let c = ItemCatalog::bundled();
        for (id, name) in [
            (3443575800u32, "Light Eater"),
            (1950738949, "Dazzling Trick"),
            (1011349580, "Radiant Daggers"),
            (2590796390, "Shining Wonder"),
        ] {
            assert_eq!(c.item_name(ItemId(id)), Some(name), "ability {id}");
            assert_eq!(c.item_kind(ItemId(id)), Some(ItemKind::Ability));
        }
    }

    #[test]
    fn unknown_ids_do_not_panic() {
        let c = ItemCatalog::bundled();
        assert_eq!(c.item_name(ItemId(1)), None);
        assert_eq!(c.item_display_name(ItemId(1)), "item 1");
    }

    #[test]
    fn class_name_lookup() {
        let c = ItemCatalog::bundled();
        let id = c.by_class_name("upgrade_clip_size").expect("present").id;
        assert_eq!(c.item_name(id), Some("Extended Magazine"));
        assert!(c.by_class_name("upgrade_nonexistent").is_none());
    }

    /// An unlocalised entry falls back to its class name, and says so.
    #[test]
    fn unlocalised_entries_fall_back_to_the_class_name() {
        let json =
            r#"[{"id":7,"class_name":"upgrade_secret","name":"upgrade_secret","type":"upgrade"}]"#;
        let c = ItemCatalog::from_json(json).expect("parse");
        assert_eq!(c.item_name(ItemId(7)), Some("upgrade_secret"));
        assert!(!c.is_localised(ItemId(7)));
        assert_eq!(c.localised_count(), 0);
        assert_eq!(c.name_for_class("upgrade_secret"), None);
    }

    #[cfg(feature = "client")]
    #[test]
    fn localisation_overlay_replaces_names() {
        let mut c = ItemCatalog::bundled();
        let loc =
            LocalizationFile::parse("\t\"upgrade_clip_size:n\"\t\"Grosses Magazin\"\n", "german");
        assert_eq!(c.merge_localization(&loc), 1);
        let id = c.by_class_name("upgrade_clip_size").expect("present").id;
        assert_eq!(c.item_name(id), Some("Grosses Magazin"));
        assert_eq!(c.provenance().names, Some(Source::Client));
    }

    /// An empty localisation value must not wipe a good name.
    #[cfg(feature = "client")]
    #[test]
    fn empty_localisation_values_are_ignored() {
        let mut c = ItemCatalog::bundled();
        let id = c.by_class_name("upgrade_clip_size").expect("present").id;
        let before = c.item_name(id).map(str::to_owned);
        let loc = LocalizationFile::parse("\t\"upgrade_clip_size:n\"\t\"\"\n", "english");
        assert_eq!(c.merge_localization(&loc), 0);
        assert_eq!(c.item_name(id).map(str::to_owned), before);
    }

    /// Item bundles carry thousands of unrelated interface strings; none of them are
    /// item names.
    #[cfg(feature = "client")]
    #[test]
    fn unrelated_tokens_are_not_imported_as_item_names() {
        let mut c = ItemCatalog::bundled();
        let before = c.names().len();
        let loc = LocalizationFile::parse(
            "\t\"Citadel_Menu_Play:n\"\t\"Play\"\n\t\"SomeUiString:n\"\t\"Text\"\n",
            "english",
        );
        assert_eq!(c.merge_localization(&loc), 0);
        assert_eq!(c.names().len(), before);
        assert_eq!(c.name_for_class("Citadel_Menu_Play"), None);
    }

    #[test]
    fn merging_an_artless_roster_keeps_existing_art() {
        let mut c = ItemCatalog::bundled();
        let id = c.by_class_name("upgrade_clip_size").expect("present").id;
        let before = c.image(id).map(str::to_owned);
        assert!(before.is_some(), "fixture needs art to be meaningful");

        let roster_only = ItemCatalog::new(vec![Item {
            id,
            class_name: "upgrade_clip_size".into(),
            kind: ItemKind::Upgrade,
        }]);
        c.merge(&roster_only);

        assert_eq!(c.image(id).map(str::to_owned), before);
        assert_eq!(c.item_name(id), Some("Extended Magazine"));
    }

    #[test]
    fn json_roundtrip() {
        let c = ItemCatalog::bundled();
        let back = ItemCatalog::from_json(&c.to_json().unwrap()).unwrap();
        assert_eq!(back.len(), c.len());
        assert_eq!(back.localised_count(), c.localised_count());
        assert_eq!(
            back.item_name(ItemId(3977876567)),
            c.item_name(ItemId(3977876567))
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_roundtrip_rebuilds_the_indices() {
        let c = ItemCatalog::bundled();
        let json = serde_json::to_string(&c).expect("write");
        let back: ItemCatalog = serde_json::from_str(&json).expect("read");
        assert_eq!(back.len(), c.len());
        assert_eq!(
            back.item_name(ItemId(3977876567)),
            Some("Kinetic Dash"),
            "indices were not rebuilt"
        );
        assert!(back.by_class_name("upgrade_clip_size").is_some());
        assert_eq!(back.provenance(), c.provenance());
    }

    #[test]
    fn tolerates_the_full_api_response_shape() {
        let json = r#"[{"id":1,"class_name":"upgrade_x","name":"X","type":"upgrade",
                        "properties":{},"heroes":[],"image":"http://..."}]"#;
        let c = ItemCatalog::from_json(json).unwrap();
        assert_eq!(c.item_name(ItemId(1)), Some("X"));
        assert_eq!(c.item_kind(ItemId(1)), Some(ItemKind::Upgrade));
    }

    #[test]
    fn unknown_type_becomes_other_not_an_error() {
        let json = r#"[{"id":2,"class_name":"c","name":"N","type":"brand_new_kind"}]"#;
        let c = ItemCatalog::from_json(json).unwrap();
        assert_eq!(c.item_kind(ItemId(2)), Some(ItemKind::Other));
    }

    /// A live ability subclass id joins to the roster, and to a name when one is shipped.
    ///
    /// This is the join item 6.3 ends at, checked end to end against a retail install.
    /// `m_nAbilitySubclassID` is a `CUtlStringToken` of the ability's vdata key, so the
    /// same id `deadlock-events` files a landed cast under is the [`ItemId`] this catalogue
    /// is keyed by - no entity list, no handle, and no crate edge from `deadlock-events`.
    ///
    /// # Not every ability has a display name, and the fallback is detectable
    ///
    /// [`ItemNames::item_name`] falls back to the class name for anything nobody
    /// translated, which is deliberate and matches what the upstream API returns. Measured
    /// here: `citadel_ability_dash` is "Dash" and `citadel_ability_lightning_ball` is
    /// "Lightning Ball", and `citadel_ability_slide` became "Slide" with "City Never
    /// Sleeps", while `citadel_ability_sprint` still comes back as its own class name - an
    /// internal movement ability the game never puts in front of a player, so no bundle
    /// carries a token for it.
    ///
    /// A consumer that needs to know the difference compares against
    /// [`ItemNames::item_class_name`]; equal means no shipped name. This asserts that
    /// escape hatch works, because otherwise a UI would print `citadel_ability_slide` to a
    /// viewer believing it had a display name.
    ///
    /// # Why this beats a short label
    ///
    /// Companion keys on its own shorthand - `hook`, `sleep`, `flog`, `hunger`,
    /// `intimidate`. Measured against the shipped vdata, four of the five are the block key
    /// with its prefix and hero segment stripped (`citadel_ability_hook`,
    /// `ability_lash_flog`, `ability_drifter_hunger`, `ability_intimidate`), and one is
    /// not: Haze's block is `ability_sleep_dagger`, which yields `sleep_dagger`, not
    /// `sleep`. `ult` has no basis in the data at all - it names a slot, not an ability.
    /// No ability block carries a short label; `_class` merely repeats the block key. A
    /// derivation would be right four times in five and quietly wrong otherwise, which is
    /// worse than emitting the id.
    #[cfg(feature = "vpk")]
    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn a_live_ability_subclass_id_joins_the_roster_and_names_what_it_can() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let roster = crate::vdata::item_roster(&dir).expect("roster from the install");
        let mut items = ItemCatalog::new(roster);
        items.merge_game_dir(&dir).expect("names from the install");

        let measured = [
            (2207638101u32, "citadel_ability_dash", true),
            (1065103387, "citadel_ability_lightning_ball", true),
            (2335418656, "citadel_ability_slide", true),
            (1443159618, "citadel_ability_sprint", false),
        ];

        for (id, class, named) in measured {
            let id = ItemId(id);
            assert_eq!(
                items.item_class_name(id),
                Some(class),
                "{id:?} does not join to the block it is the token of"
            );
            let name = items
                .item_name(id)
                .expect("in the roster, so it has a name");
            assert_eq!(
                name != class,
                named,
                "{class}: shipped-name expectation moved; got {name:?}"
            );
        }

        assert!(
            measured
                .iter()
                .any(|(id, _, _)| items.item_name(ItemId(*id)) == Some("Dash")),
            "no measured id resolved to a shipped display name"
        );
    }
}
