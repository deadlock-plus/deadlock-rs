//! Item and ability identity.
//!
//! Deadlock models abilities, purchasable upgrades and weapons as one "item" space with
//! a shared id namespace, which is why a hero's ability appears in the same table as a
//! shop item. [`ItemKind`] separates them.

/// An item, ability or weapon id.
///
/// These are the values found in `PlayerDataGlobal_t::m_vecUpgrades` (purchased items)
/// and in `m_vecAbilityUpgradeState` (the AP spend order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ItemId(pub u32);

impl ItemId {
    /// No item.
    pub const NONE: ItemId = ItemId(0);

    /// Whether this refers to something.
    pub fn is_some(&self) -> bool {
        self.0 != 0
    }

    /// The raw numeric id.
    pub fn get(&self) -> u32 {
        self.0
    }

    /// Derive the id Source 2 assigns to a class name.
    ///
    /// Item ids are not arbitrary: they are `CUtlStringToken`, which is `MurmurHash2` of the
    /// lowercased class name with seed `0x31415926`. So an id can be computed from a name
    /// with no lookup table at all, which is how a source that has class names but no id
    /// table - the game's own `abilities.vdata_c` - still yields addressable ids.
    ///
    /// It is also the check that catches a stale published id: a derived id cannot go out
    /// of date while the class name is unchanged.
    ///
    /// ```
    /// use deadlock_core::ItemId;
    ///
    /// assert_eq!(ItemId::from_class_name("upgrade_clip_size"), ItemId(1548066885));
    /// // Case-insensitive, exactly as the engine treats it.
    /// assert_eq!(
    ///     ItemId::from_class_name("Upgrade_Clip_Size"),
    ///     ItemId::from_class_name("upgrade_clip_size"),
    /// );
    /// ```
    pub fn from_class_name(class_name: &str) -> ItemId {
        ItemId(murmur2_ascii_lowercase(class_name.as_bytes(), 0x3141_5926))
    }
}

/// `MurmurHash2` (32-bit), lowercasing ASCII input as it reads.
///
/// Ported from the reference implementation rather than a crate: it is twenty lines, the
/// constants are load-bearing, and `CUtlStringToken` depends on reproducing the original's
/// tail handling exactly - the `match` below falls through the way the C `switch` does.
fn murmur2_ascii_lowercase(data: &[u8], seed: u32) -> u32 {
    const M: u32 = 0x5bd1_e995;
    const R: u32 = 24;

    let mut h = seed ^ (data.len() as u32);
    let (chunks, tail) = data.as_chunks::<4>();
    for c in chunks {
        let mut k = u32::from_le_bytes([
            c[0].to_ascii_lowercase(),
            c[1].to_ascii_lowercase(),
            c[2].to_ascii_lowercase(),
            c[3].to_ascii_lowercase(),
        ]);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M);
        h ^= k;
    }

    if !tail.is_empty() {
        if tail.len() >= 3 {
            h ^= u32::from(tail[2].to_ascii_lowercase()) << 16;
        }
        if tail.len() >= 2 {
            h ^= u32::from(tail[1].to_ascii_lowercase()) << 8;
        }
        h ^= u32::from(tail[0].to_ascii_lowercase());
        h = h.wrapping_mul(M);
    }

    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^= h >> 15;
    h
}

impl From<u32> for ItemId {
    fn from(v: u32) -> Self {
        ItemId(v)
    }
}

impl From<ItemId> for u32 {
    fn from(v: ItemId) -> Self {
        v.0
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What an item actually is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum ItemKind {
    /// A hero ability.
    Ability,
    /// A purchasable shop upgrade.
    Upgrade,
    /// A weapon.
    Weapon,
    /// Something this build does not classify.
    Other,
}

impl ItemKind {
    /// Parse the API's `type` field.
    pub fn from_str_lossy(s: &str) -> ItemKind {
        match s {
            "ability" => ItemKind::Ability,
            "upgrade" => ItemKind::Upgrade,
            "weapon" => ItemKind::Weapon,
            _ => ItemKind::Other,
        }
    }

    /// The wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemKind::Ability => "ability",
            ItemKind::Upgrade => "upgrade",
            ItemKind::Weapon => "weapon",
            ItemKind::Other => "other",
        }
    }
}

impl std::fmt::Display for ItemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolves item and ability ids to names.
///
/// The counterpart to [`crate::HeroNames`]: it lets a memory reader print
/// "Radiant Daggers" without knowing anything about game files or CDNs.
pub trait ItemNames: Send + Sync {
    /// Display name for an item, if known.
    fn item_name(&self, id: ItemId) -> Option<&str>;

    /// Internal class name, e.g. `upgrade_clip_size`, if known.
    fn item_class_name(&self, _id: ItemId) -> Option<&str> {
        None
    }

    /// What kind of thing this id refers to, if known.
    fn item_kind(&self, _id: ItemId) -> Option<ItemKind> {
        None
    }

    /// Display name, falling back to `item <id>` so callers always render something.
    fn item_display_name(&self, id: ItemId) -> String {
        self.item_name(id)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("item {}", id.0))
    }
}

impl<T: ItemNames + ?Sized> ItemNames for std::sync::Arc<T> {
    fn item_name(&self, id: ItemId) -> Option<&str> {
        (**self).item_name(id)
    }
    fn item_class_name(&self, id: ItemId) -> Option<&str> {
        (**self).item_class_name(id)
    }
    fn item_kind(&self, id: ItemId) -> Option<ItemKind> {
        (**self).item_kind(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_id_basics() {
        assert!(!ItemId::NONE.is_some());
        assert!(ItemId(3977876567).is_some());
        assert_eq!(u32::from(ItemId(7)), 7);
        assert_eq!(ItemId(7).to_string(), "7");
    }

    #[test]
    fn kind_roundtrip() {
        for (s, k) in [
            ("ability", ItemKind::Ability),
            ("upgrade", ItemKind::Upgrade),
            ("weapon", ItemKind::Weapon),
        ] {
            assert_eq!(ItemKind::from_str_lossy(s), k);
            assert_eq!(k.as_str(), s);
        }
        assert_eq!(ItemKind::from_str_lossy("something_new"), ItemKind::Other);
    }

    struct One;
    impl ItemNames for One {
        fn item_name(&self, id: ItemId) -> Option<&str> {
            (id.0 == 1).then_some("Extended Magazine")
        }
    }

    #[test]
    fn display_always_renders() {
        assert_eq!(One.item_display_name(ItemId(1)), "Extended Magazine");
        assert_eq!(One.item_display_name(ItemId(42)), "item 42");
    }

    /// Ids observed in a live match, against the class names the game ships. If the
    /// derivation is wrong these do not merely differ, they are unrelated numbers.
    #[test]
    fn class_names_hash_to_the_ids_the_game_uses() {
        for (class_name, id) in [
            ("upgrade_clip_size", 1548066885u32),
            ("upgrade_kinetic_sash", 3977876567),
            ("upgrade_pristine_emblem", 2064029594),
            ("upgrade_blitz_bullets", 4104549924),
            ("ability_unicorn_radiantblast", 3443575800),
        ] {
            assert_eq!(
                ItemId::from_class_name(class_name),
                ItemId(id),
                "{class_name}"
            );
        }
    }

    /// `CUtlStringToken` lowercases, so a mixed-case class name and its lowercase form
    /// are the same token. `gunslinger_demonMark` is the one that actually matters.
    #[test]
    fn hashing_ignores_case() {
        assert_eq!(
            ItemId::from_class_name("gunslinger_demonMark"),
            ItemId::from_class_name("gunslinger_demonmark"),
        );
        assert_eq!(
            ItemId::from_class_name("gunslinger_demonMark"),
            ItemId(3673718559)
        );
    }

    /// Every tail length has to be right: `MurmurHash2`'s trailing bytes fall through a
    /// switch, and a port that turns it into exclusive branches is wrong for 2 and 3.
    #[test]
    fn every_tail_length_is_handled() {
        let hashes: Vec<u32> = ["a", "ab", "abc", "abcd", "abcde", "abcdef", "abcdefg"]
            .iter()
            .map(|s| ItemId::from_class_name(s).get())
            .collect();
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), hashes.len(), "collision: {hashes:?}");
        assert_eq!(ItemId::from_class_name("").get(), 3050872623);
    }
}
