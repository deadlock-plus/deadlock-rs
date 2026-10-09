//! Rosters read from the installed game's own compiled data files.
//!
//! This is the third source, and the only one that is exactly version-matched: it reads
//! whatever build is on disk. It needs an install path, not a running process — the id
//! tables live in `pak01_dir.vpk`, so the game does not have to be running, and this
//! crate still needs no dependency on `deadlock-memory`.
//!
//! Behind the `vpk` feature, which pulls in [`source2`] and, through it, a zstd decoder.
//!
//! # What it adds over the vendored snapshot
//!
//! | | Snapshot | Installed game |
//! |---|---|---|
//! | Heroes | 66 | **67 blocks**, `hero_base` excluded by its zero id |
//! | Items | 726 | every top-level block in `abilities.vdata_c` |
//! | Item ids | published, and can go stale | derived from the class name, so they cannot |
//! | `m_bPlayerSelectable` | absent | absent since "City Never Sleeps"; read if it returns |
//!
//! Art is not here and never will be: those URLs are a deadlock-api.com construct rather
//! than game data.
//!
//! [`source2`]: https://github.com/deadlock-plus/source2-rs

use std::path::Path;

use deadlock_core::{HeroId, ItemId, ItemKind};
use source2::ext::VpkResourceKv3;
use source2::kv3;
use source2::resource::BlockKind;
use source2::vpk::Vpk;

use crate::error::{Error, Result};
use crate::heroes::Hero;
use crate::items::Item;

pub use crate::install::ARCHIVE;
/// Path of the hero table inside the archive.
pub const HEROES_PATH: &str = "scripts/heroes.vdata_c";
/// Path of the ability, item and weapon table inside the archive.
pub const ABILITIES_PATH: &str = "scripts/abilities.vdata_c";

/// Where the accolade definitions live inside the archive.
pub const ACCOLADES_PATH: &str = "scripts/accolades.vdata_c";

/// Hero roster from an installed game.
///
/// `citadel_dir` is the directory containing `pak01_dir.vpk`, i.e.
/// `.../Deadlock/game/citadel`.
///
/// Entries with a zero id are skipped. That is not a heuristic: `hero_base` is the
/// template the real heroes inherit from and it carries `m_HeroID = 0`, so the id itself
/// is what separates it from a real hero.
///
/// # Errors
///
/// If the archive cannot be opened, the entry is missing, or the resource does not decode.
pub fn hero_roster(citadel_dir: impl AsRef<Path>) -> Result<Vec<Hero>> {
    let doc = read_document(citadel_dir.as_ref(), HEROES_PATH)?;
    let root = doc
        .root
        .as_object()
        .ok_or_else(|| Error::Parse(format!("{HEROES_PATH}: root is not an object")))?;

    let mut heroes = Vec::new();
    for (class_name, value) in root.iter() {
        // The root also carries scalars like `generic_data_type`; only objects are heroes.
        let Some(hero) = value.as_object() else {
            continue;
        };
        let Some(id) = hero.get("m_HeroID").and_then(kv3::Value::as_u32) else {
            continue;
        };
        if id == 0 {
            continue;
        }
        let state = hero
            .get("m_eHeroDevelopmentState")
            .and_then(kv3::Value::as_str);
        heroes.push(Hero {
            id: HeroId(id),
            class_name: class_name.to_string(),
            in_development: in_development(flag(hero, "m_bInDevelopment"), state),
            disabled: flag(hero, "m_bDisabled"),
            pre_release: state == Some("EHeroDevState_PreRelease"),
            player_selectable: hero
                .get("m_bPlayerSelectable")
                .and_then(kv3::Value::as_bool),
        });
    }
    heroes.sort_by_key(|h| h.id);
    Ok(heroes)
}

/// Item, ability and weapon roster from an installed game.
///
/// Ids are derived with [`ItemId::from_class_name`] rather than read: Source 2 assigns
/// them as `CUtlStringToken` of the class name, so there is no id table in the file and
/// a derived id cannot be stale.
///
/// # Errors
///
/// As [`hero_roster`].
pub fn item_roster(citadel_dir: impl AsRef<Path>) -> Result<Vec<Item>> {
    let doc = read_document(citadel_dir.as_ref(), ABILITIES_PATH)?;
    let root = doc
        .root
        .as_object()
        .ok_or_else(|| Error::Parse(format!("{ABILITIES_PATH}: root is not an object")))?;

    let mut items = Vec::new();
    for (class_name, value) in root.iter() {
        let Some(item) = value.as_object() else {
            continue;
        };
        // Every real entry declares what it is. Anything without the field is a stray
        // scalar or a shape this reader does not model, and is skipped rather than
        // guessed at.
        let Some(kind) = item.get("m_eAbilityType").and_then(kv3::Value::as_str) else {
            continue;
        };
        items.push(Item {
            id: ItemId::from_class_name(class_name),
            class_name: class_name.to_string(),
            kind: kind_from_ability_type(kind),
        });
    }
    items.sort_by_key(|i| i.id);
    items.dedup_by(|a, b| a.id == b.id);
    Ok(items)
}

/// Map the game's `EAbilityType_*` tag onto [`ItemKind`].
///
/// The game's classification is not always the one deadlock-api.com publishes -
/// `weapon_upgrade_t1` is `EAbilityType_Item` here and `weapon` there - so a catalogue
/// built from this source can disagree with one built from the API. The game's own answer
/// is the one that matches what the client does.
fn kind_from_ability_type(tag: &str) -> ItemKind {
    match tag {
        "EAbilityType_Item" => ItemKind::Upgrade,
        "EAbilityType_Weapon" => ItemKind::Weapon,
        // Signature, Ultimate, Innate and anything else Valve adds are all abilities.
        _ => ItemKind::Ability,
    }
}

/// `m_bInDevelopment` unless the hero says it is released.
///
/// Valve left the flag set on Baba after release, so an explicit `Release` state is the
/// better evidence. Heroes without a state key (the sandbox placeholder) keep the flag.
fn in_development(flag: bool, state: Option<&str>) -> bool {
    flag && state != Some("EHeroDevState_Release")
}

/// A boolean field, treating absence as false.
///
/// The compiler omits fields left at their default, so a missing `m_bDisabled` means "not
/// disabled" rather than "unknown".
fn flag(object: &kv3::Object, key: &str) -> bool {
    object
        .get(key)
        .and_then(kv3::Value::as_bool)
        .unwrap_or(false)
}

fn read_document(citadel_dir: &Path, path: &str) -> Result<kv3::Document> {
    let archive = citadel_dir.join(ARCHIVE);
    let vpk = Vpk::open(&archive).map_err(|e| vpk_error(e, &archive))?;
    vpk.read_resource_kv3(path, BlockKind::DATA)
        .map_err(|e| match e {
            source2::Error::Vpk(e) => vpk_error(e, &archive),
            other => Error::Parse(format!("{path}: {other}")),
        })
}

/// Translate a VPK failure, keeping I/O distinguishable from a malformed archive.
pub(crate) fn vpk_error(e: source2::vpk::Error, archive: &Path) -> Error {
    match e {
        source2::vpk::Error::Io { path, source } => Error::Io {
            // A missing archive is the normal "no game installed here" answer, and
            // callers key off the kind rather than the message.
            kind: std::io::ErrorKind::NotFound,
            path,
            source: source.to_string(),
        },
        other => Error::Parse(format!("{}: {other}", archive.display())),
    }
}

/// One accolade the game can award after a match.
///
/// The names are **tokens**, not display strings: `m_sFlavorName` reads
/// `#Citadel_VData_accolades_kills_FlavorName`, which resolves through the
/// `citadel_vdata/accolades` localisation bundle - see
/// [`ACCOLADES_BUNDLE`](crate::localization::ACCOLADES_BUNDLE). Resolving them needs a
/// language, so this keeps the tokens and leaves that to the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Accolade {
    /// `m_unAccoladeID`, which is what a replay's `PlayerAccolade` carries.
    pub id: u32,
    /// The block's own name in the vdata, e.g. `kills`.
    pub key: String,
    /// `m_sTrackedStatName`, the stat this accolade is measured on.
    pub tracked_stat: Option<String>,
    /// Localisation token for the flavour name, leading `#` stripped.
    pub flavor_token: Option<String>,
    /// Localisation token for the description, leading `#` stripped.
    ///
    /// These carry a `:f` suffix in the vdata (`..._Description:f`), which is part of the
    /// key the bundle is written with, so it is kept rather than trimmed.
    pub description_token: Option<String>,
}

/// Accolade definitions from an installed game.
///
/// `citadel_dir` is the directory containing `pak01_dir.vpk`.
///
/// This is what turns a replay's `accolade_id` into something a person can read: the
/// replay carries only the number.
///
/// # Errors
///
/// If the archive cannot be opened, the entry is missing, or the resource does not decode.
pub fn accolades(citadel_dir: impl AsRef<Path>) -> Result<Vec<Accolade>> {
    let doc = read_document(citadel_dir.as_ref(), ACCOLADES_PATH)?;
    let root = doc
        .root
        .as_object()
        .ok_or_else(|| Error::Parse(format!("{ACCOLADES_PATH}: root is not an object")))?;

    // A leading `#` marks a localisation reference rather than a literal; the bundle is
    // keyed without it.
    let token = |o: &kv3::Object, key: &str| -> Option<String> {
        let raw = o.get(key)?.as_str()?;
        Some(raw.strip_prefix('#').unwrap_or(raw).to_owned())
    };

    let mut out = Vec::new();
    for (key, value) in root.iter() {
        // The root also carries scalars such as `generic_data_type`.
        let Some(block) = value.as_object() else {
            continue;
        };
        let Some(id) = block.get("m_unAccoladeID").and_then(kv3::Value::as_u32) else {
            continue;
        };
        out.push(Accolade {
            id,
            key: key.to_owned(),
            tracked_stat: block
                .get("m_sTrackedStatName")
                .and_then(kv3::Value::as_str)
                .map(str::to_owned),
            flavor_token: token(block, "m_sFlavorName"),
            description_token: token(block, "m_sDescription"),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_release_state_overrides_a_stale_development_flag() {
        assert!(!in_development(true, Some("EHeroDevState_Release")));
        assert!(in_development(true, Some("EHeroDevState_PreRelease")));
        assert!(in_development(true, None));
        assert!(!in_development(false, None));
    }

    /// Every shipped accolade carries an id and a resolvable flavour token.
    ///
    /// A replay's `PlayerAccolade` carries only `accolade_id`, so this table is the whole
    /// route from that number to something readable. The join is asserted rather than
    /// assumed: each token must actually be present in the localisation bundle.
    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn the_shipped_accolades_resolve_to_names() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let list = accolades(&dir).expect("accolades");
        assert!(list.len() > 20, "only {} accolades", list.len());

        let names = crate::localization::LocalizationFile::load(
            &dir,
            crate::localization::ACCOLADES_BUNDLE,
            "english",
        )
        .expect("accolade bundle");

        let mut unresolved = Vec::new();
        for a in &list {
            match a.flavor_token.as_deref().and_then(|t| names.get(t)) {
                Some(n) if !n.is_empty() => {}
                _ => unresolved.push(format!("{} ({})", a.key, a.id)),
            }
        }
        assert!(
            unresolved.is_empty(),
            "no flavour name for: {unresolved:#?}"
        );

        let mut ids: Vec<u32> = list.iter().map(|a| a.id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate accolade ids");
    }
}
