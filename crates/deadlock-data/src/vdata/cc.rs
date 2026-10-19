use std::collections::BTreeMap;
use std::path::Path;

use source2::kv3;

use super::{ABILITIES_PATH, HEROES_PATH, read_document};
use crate::error::{Error, Result};

use CcKind::{Disarm, Freeze, Hex, Immobilize, Knockup, Pull, Silence, Sleep, Stun, Tether};
use Pat::{Ends, Has, Is, Starts};

// The name tables below are what decides the set. Review them when a patch adds modifier
// classes or renames properties: a new C++ CC class is invisible until a row matches it.

/// Never CC, whatever the data says. Watcher's Covenant tethers an ally and Rescue Beam pulls
/// one, but both carry the same tether/pull shapes as enemy CC.
const EXCLUDED: &[&str] = &["citadel_ability_tangotether", "upgrade_rescue_beam"];

/// Modifier classes whose CC lives in C++ rather than in a state mask. The first row that
/// matches wins, so the `None` row must stay first: it keeps caster-side, ally-side and
/// immunity classes out.
const CLASS_RULES: &[(&[Pat], Option<CcKind>)] = &[
    (
        &[
            Has("immun"),
            Ends("_self"),
            Has("watcher"),
            Has("proc_ready"),
            Has("buildup"),
            Has("_ally"),
            Has("friendly"),
            Has("no_connection"),
            Ends("_waiting"),
            Has("tether_receiver"),
        ],
        None,
    ),
    (
        &[
            Has("stunned"),
            Has("delayed_stun"),
            Has("parried_stun"),
            Ends("_stun"),
            Has("petrify"),
            Ends("knockdown"),
            Has("staticcharge"),
            // Mo & Krill's Combo holds its target in C++; the drag it also carries is a pull.
            Has("ult_combo_target"),
        ],
        Some(Stun),
    ),
    (&[Ends("_root"), Has("immobiliz")], Some(Immobilize)),
    (
        &[
            Has("tether"),
            Has("chain_connection"),
            Has("containment_victim"),
        ],
        Some(Tether),
    ),
    (&[Has("silenc")], Some(Silence)),
    (&[Has("asleep"), Has("sleeping")], Some(Sleep)),
    (&[Has("disarm")], Some(Disarm)),
    (&[Has("_hex"), Has("hexarea")], Some(Hex)),
    (&[Has("freeze")], Some(Freeze)),
    (
        &[
            Has("psychiclift"),
            Has("tornado_lift"),
            Has("pulldown_to_ground"),
        ],
        Some(Knockup),
    ),
    (
        &[
            Has("_pull"),
            Has("enemy_pulled"),
            Has("drag"),
            Has("grab_enemy"),
        ],
        Some(Pull),
    ),
];

/// `m_nEnabledStateMask` flags, `MODIFIER_STATE_` stripped, that are hard CC on a generic
/// `modifier_base` / `modifier_slow_base`.
const STATE_RULES: &[(&str, CcKind)] = &[
    ("STUNNED", Stun),
    ("IMMOBILIZED", Immobilize),
    ("MOVEMENT_INPUT_DISABLED", Immobilize),
    ("CHAINED", Tether),
    ("SILENCED", Silence),
    ("IS_ASLEEP", Sleep),
    ("DISARMED", Disarm),
    ("PULLDOWN_TO_GROUND", Knockup),
];

/// Flags only the caster's own modifiers carry (channels, dashes, ultimate states).
const CASTER_STATES: &[Pat] = &[
    Starts("ALLOW_"),
    Ends("_ULT_ACTIVE"),
    Is("ABILITY_MOVEMENT"),
    Is("BUSY_WITH_ACTION"),
    Is("SPRINTING"),
    Is("FLYING"),
    Starts("CASTING_"),
    Is("UNSTOPPABLE"),
    Is("INVULNERABLE"),
    Is("FORCE_CAN_PARRY"),
];

/// Words in a modifier's key or subclass name, lowercase, that place it on the caster or
/// an ally. An enemy word in the key itself overrides them.
const CASTER_KEY_WORDS: &[&str] = &[
    "self", "buff", "friendly", "ally", "caster", "precast", "channel", "summon", "spawning",
    "barrier", "shield", "flying", "bonus", "linger", "immun", "watcher", "ready",
];

/// Words in a modifier's key or subclass name, lowercase, that place it on an enemy.
const ENEMY_KEY_WORDS: &[&str] = &[
    "target", "victim", "debuff", "enemy", "impact", "stun", "root", "immobil", "silence", "sleep",
    "disarm", "hex", "tether", "chain", "petrify", "slow", "drag", "pull", "lift", "glitch",
];

/// Ability properties (case-sensitive) that size each kind, and names to skip. A CC the
/// game applies from code by a global name leaves nothing but these behind. A property at
/// 0 that an upgrade raises gates the CC to that upgrade; one at 0 that nothing raises
/// (`HasDisarm`, `FreezeEnemies`) switches it off even when a modifier is present.
const PROPERTY_RULES: &[(CcKind, &[Pat], &[Pat])] = &[
    (
        Stun,
        &[Has("Stun"), Is("PetrifyDuration")],
        &[
            Has("Self"),
            Has("Immun"),
            Has("Reduce"),
            Has("Distance"),
            Has("Delay"),
            Has("Parried"),
        ],
    ),
    (
        Immobilize,
        &[
            Has("Immobilize"),
            Is("RootDuration"),
            Is("EntangleDuration"),
        ],
        &[],
    ),
    (Tether, &[Is("TetherDuration"), Is("ChainDuration")], &[]),
    (
        Silence,
        &[Has("Silence")],
        &[
            Has("Speed"),
            Has("Radius"),
            Has("Stamina"),
            Has("LockTime"),
            Has("Immun"),
            Has("Movement"),
        ],
    ),
    (
        Sleep,
        &[
            Is("Sleep"),
            Is("SleepDuration"),
            Is("MinSleepTime"),
            Is("MinimumSleepTime"),
        ],
        &[],
    ),
    (
        Knockup,
        &[
            Has("Toss"),
            Is("LiftDuration"),
            Is("LiftHeight"),
            Is("LiftTime"),
            Is("EnemyLiftDuration"),
            Is("EnemyLiftHeight"),
            Is("EnemyLiftTime"),
            Has("KnockUp"),
            Is("KnockupSpeed"),
            Is("KnockupRadius"),
        ],
        &[
            Has("Immun"),
            Has("Friendly"),
            Has("Ally"),
            Has("Angle"),
            Has("Ratio"),
        ],
    ),
    (
        Pull,
        &[
            Is("PullDistance"),
            Is("PullDuration"),
            Is("PullSpeed"),
            Is("PullForceMax"),
            Is("PullTargetSpeed"),
            Is("PullDownDuration"),
        ],
        &[],
    ),
    (Disarm, &[Has("Disarm")], &[]),
    (Hex, &[Is("HexDuration")], &[]),
    (Freeze, &[Is("FreezeEnemies"), Is("InitialFreezeTime")], &[]),
];

#[derive(Clone, Copy)]
enum Pat {
    Has(&'static str),
    Starts(&'static str),
    Ends(&'static str),
    Is(&'static str),
}

fn matches_any(patterns: &[Pat], text: &str) -> bool {
    patterns.iter().any(|&p| match p {
        Has(s) => text.contains(s),
        Starts(s) => text.starts_with(s),
        Ends(s) => text.ends_with(s),
        Is(s) => text == s,
    })
}

/// One kind of hard crowd control. Slows are not hard CC and have no kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum CcKind {
    /// Stunned, petrified or knocked down.
    Stun,
    /// Rooted, or movement input disabled.
    Immobilize,
    /// Tethered or chained to a point or a unit.
    Tether,
    /// Abilities locked.
    Silence,
    /// Put to sleep.
    Sleep,
    /// Thrown up or pulled down to the ground.
    Knockup,
    /// Dragged or pulled.
    Pull,
    /// Weapon locked.
    Disarm,
    /// Turned into a critter.
    Hex,
    /// Frozen in place.
    Freeze,
}

/// What an ability with hard CC belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum CcOwner {
    /// A signature ability of the hero with this class name, e.g. `hero_necro`.
    Hero(String),
    /// A shop item.
    Item,
}

/// The hard CC one ability or item can apply.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CcAbility {
    /// Hero ability or shop item.
    pub owner: CcOwner,
    /// Each kind with the ability upgrade that turns it on: 0 at base, 1 to 3 for an upgrade.
    pub kinds: BTreeMap<CcKind, u8>,
}

impl CcAbility {
    /// Whether every hard CC here needs an ability upgrade first.
    pub fn gated(&self) -> bool {
        self.kinds.values().all(|&tier| tier > 0)
    }
}

/// Every released hero's signature abilities and every shop item that can apply hard CC,
/// keyed by class name (`ability_necro_gravestone`, `upgrade_target_stun`).
///
/// `citadel_dir` is the directory containing `pak01_dir.vpk`.
///
/// Read from the ability definitions rather than a hand list: a modifier the ability
/// carries, a CC state flag on it, or a property that sizes a CC the game applies from code.
/// The name patterns that drive this are tables at the top of this file and need a review
/// when a patch adds new modifier classes.
///
/// # Errors
///
/// If the archive cannot be opened, an entry is missing, or a resource does not decode.
pub fn cc_abilities(citadel_dir: impl AsRef<Path>) -> Result<BTreeMap<String, CcAbility>> {
    let dir = citadel_dir.as_ref();
    let heroes = read_document(dir, HEROES_PATH)?;
    let abilities = read_document(dir, ABILITIES_PATH)?;
    classify(&heroes, &abilities)
}

fn classify(
    heroes: &kv3::Document,
    abilities: &kv3::Document,
) -> Result<BTreeMap<String, CcAbility>> {
    let heroes = heroes
        .root
        .as_object()
        .ok_or_else(|| Error::Parse(format!("{HEROES_PATH}: root is not an object")))?;
    let abilities = abilities
        .root
        .as_object()
        .ok_or_else(|| Error::Parse(format!("{ABILITIES_PATH}: root is not an object")))?;

    let mut out = BTreeMap::new();
    let mut add = |class: &str, ability: &kv3::Value, owner: CcOwner| {
        if EXCLUDED.contains(&class) || out.contains_key(class) {
            return;
        }
        let kinds = ability_kinds(ability);
        if !kinds.is_empty() {
            out.insert(class.to_owned(), CcAbility { owner, kinds });
        }
    };

    for (hero_class, hero) in heroes.iter() {
        let released = hero
            .get("m_HeroID")
            .and_then(kv3::Value::as_u64)
            .unwrap_or(0)
            != 0
            && !is_true(hero.get("m_bDisabled"))
            && hero
                .get("m_eHeroDevelopmentState")
                .and_then(kv3::Value::as_str)
                == Some("EHeroDevState_Release");
        if !released {
            continue;
        }
        for slot in SIGNATURE_SLOTS {
            let Some(class) = hero
                .get("m_mapBoundAbilities")
                .and_then(|b| b.get(slot))
                .and_then(kv3::Value::as_str)
            else {
                continue;
            };
            if let Some(ability) = abilities.get(class) {
                add(class, ability, CcOwner::Hero(hero_class.to_owned()));
            }
        }
    }

    for (class, ability) in abilities.iter() {
        let shop_item = ability.get("m_eAbilityType").and_then(kv3::Value::as_str)
            == Some("EAbilityType_Item")
            && is_truthy(ability.get("m_iItemTier"))
            && !is_true(ability.get("m_bDisabled"));
        if shop_item {
            add(class, ability, CcOwner::Item);
        }
    }
    Ok(out)
}

const SIGNATURE_SLOTS: [&str; 4] = [
    "ESlot_Signature_1",
    "ESlot_Signature_2",
    "ESlot_Signature_3",
    "ESlot_Signature_4",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Caster,
    Ally,
    Enemy,
    Unknown,
}

struct ModifierHit<'a> {
    key: &'a str,
    side: Side,
    kinds: Vec<CcKind>,
}

fn ability_kinds(ability: &kv3::Value) -> BTreeMap<CcKind, u8> {
    let targets = ability
        .get("m_nAbilityTargetTypes")
        .and_then(kv3::Value::as_str)
        .unwrap_or("");
    // `ALL` also admits `ALL_FRIENDLY`, which is why Watcher's Covenant needs its exclusion:
    // Air Drop targets friendlies too, so narrowing this would drop real CC.
    let enemy_ok = targets.contains("ENEMY")
        || targets.contains("ALL")
        || targets.trim() == "CITADEL_UNIT_TARGET_HERO";

    let mut hits = Vec::new();
    collect_modifiers(ability, "", Side::Unknown, &mut hits);
    // Kind -> whether one of its modifiers sits under a `Debuff` key.
    let mut via_modifier: BTreeMap<CcKind, bool> = BTreeMap::new();
    for hit in &hits {
        match hit.side {
            Side::Caster | Side::Ally => continue,
            Side::Unknown if !enemy_ok => continue,
            _ => {}
        }
        for &kind in &hit.kinds {
            *via_modifier.entry(kind).or_default() |= hit.key.contains("Debuff");
        }
    }

    let props = ability
        .get("m_mapAbilityProperties")
        .and_then(kv3::Value::as_object);
    let upgrades = ability
        .get("m_vecAbilityUpgrades")
        .and_then(kv3::Value::as_array)
        .unwrap_or_default();
    let mut out = BTreeMap::new();
    for &(kind, include, exclude) in PROPERTY_RULES {
        let modifier = via_modifier.get(&kind).copied();
        let gate = property_gate(props, upgrades, include, exclude, modifier == Some(true));
        let tier = match (modifier, gate) {
            (_, Some(Gate::Dormant)) => continue,
            (Some(_), None) => 0,
            (Some(_), Some(Gate::Tier(t))) => t,
            (None, Some(Gate::Tier(t))) if enemy_ok => t,
            (None, _) => continue,
        };
        out.insert(kind, tier);
    }
    out
}

fn collect_modifiers<'a>(
    value: &'a kv3::Value,
    key: &'a str,
    parent: Side,
    out: &mut Vec<ModifierHit<'a>>,
) {
    match value.kind() {
        kv3::Kind::Object(object) => {
            let mut here = parent;
            let class = object.get("_class").and_then(kv3::Value::as_str);
            if let Some(class) = class.filter(|c| !c.is_empty() && !key.is_empty()) {
                let text = |k: &str| object.get(k).and_then(kv3::Value::as_str).unwrap_or("");
                let mask = text("m_nEnabledStateMask");
                here = side_of(key, text("_my_subclass_name"), parent);
                if states(mask).any(|s| matches_any(CASTER_STATES, s)) {
                    here = Side::Caster;
                }
                // Subclass names echo the ability's own name, so only the class is matched.
                let mut kinds: Vec<CcKind> = class_kind(class).into_iter().collect();
                kinds.extend(states(mask).filter_map(state_kind));
                if !kinds.is_empty() {
                    out.push(ModifierHit {
                        key,
                        side: here,
                        kinds,
                    });
                }
                // An aura's children are what it hands out, to the side it searches.
                let aura = text("m_iAuraSearchType");
                if aura.contains("ENEMY") {
                    here = Side::Enemy;
                } else if aura.contains("FRIENDLY") {
                    here = Side::Ally;
                }
            }
            for (k, child) in object.iter() {
                if k == "m_mapAbilityProperties" || k.starts_with("m_bits") {
                    continue;
                }
                collect_modifiers(child, k, here, out);
            }
        }
        // A list element keeps its list's key and side.
        kv3::Kind::Array(items) => {
            for item in items {
                collect_modifiers(item, key, parent, out);
            }
        }
        _ => {}
    }
}

fn side_of(key: &str, subclass: &str, parent: Side) -> Side {
    if matches!(parent, Side::Caster | Side::Ally)
        && !["Enemy", "Victim", "Target", "Debuff"]
            .iter()
            .any(|p| key.contains(p))
    {
        return parent;
    }
    let key_lower = key.to_ascii_lowercase();
    let blob = format!("{key_lower} {}", subclass.to_ascii_lowercase());
    let enemy_key = ENEMY_KEY_WORDS.iter().any(|w| key_lower.contains(w));
    if !enemy_key && CASTER_KEY_WORDS.iter().any(|w| blob.contains(w)) {
        return Side::Caster;
    }
    if ENEMY_KEY_WORDS.iter().any(|w| blob.contains(w)) {
        return Side::Enemy;
    }
    parent
}

/// The `MODIFIER_STATE_*` names in a flag mask, prefix stripped.
fn states(mask: &str) -> impl Iterator<Item = &str> {
    mask.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter_map(|t| t.strip_prefix("MODIFIER_STATE_"))
}

fn class_kind(class: &str) -> Option<CcKind> {
    CLASS_RULES
        .iter()
        .find(|(patterns, _)| matches_any(patterns, class))
        .and_then(|&(_, kind)| kind)
}

fn state_kind(state: &str) -> Option<CcKind> {
    STATE_RULES
        .iter()
        .find(|(name, _)| *name == state)
        .map(|&(_, kind)| kind)
}

enum Gate {
    /// Live from this upgrade on; 0 is the base ability.
    Tier(u8),
    /// The property exists but is 0 and no upgrade raises it.
    Dormant,
}

/// Whether the properties that size `include`'s CC are live, and from which upgrade.
///
/// `None` when the ability has no such property, which for a modifier-found CC means the
/// size lives elsewhere and the CC is taken as live at base.
fn property_gate(
    props: Option<&kv3::Object>,
    upgrades: &[kv3::Value],
    include: &[Pat],
    exclude: &[Pat],
    debuff_modifier: bool,
) -> Option<Gate> {
    let props = props?;
    let mut hits: Vec<&str> = props
        .keys()
        .filter(|name| matches_any(include, name) && !matches_any(exclude, name))
        .collect();
    // Most code-applied debuffs are sized by this one generic duration.
    if hits.is_empty() && debuff_modifier && props.contains_key("DebuffDuration") {
        hits.push("DebuffDuration");
    }
    if hits.is_empty() {
        return None;
    }
    if hits
        .iter()
        .any(|name| leading_number(props.get(name).and_then(|p| p.get("m_strValue"))) > 0.0)
    {
        return Some(Gate::Tier(0));
    }
    for (tier, upgrade) in (1u8..).zip(upgrades) {
        let raises = upgrade
            .get("m_vecPropertyUpgrades")
            .and_then(kv3::Value::as_array)
            .unwrap_or_default()
            .iter()
            .any(|p| {
                p.get("m_strPropertyName")
                    .and_then(kv3::Value::as_str)
                    .is_some_and(|name| hits.contains(&name))
                    && leading_number(p.get("m_strBonus")) > 0.0
            });
        if raises {
            return Some(Gate::Tier(tier));
        }
    }
    Some(Gate::Dormant)
}

/// A property value as a number. They are mostly strings with a unit (`"14m"`, `"0.5"`),
/// sometimes plain numbers; anything unreadable counts as 0.
fn leading_number(value: Option<&kv3::Value>) -> f64 {
    let Some(value) = value else {
        return 0.0;
    };
    if let Some(text) = value.as_str() {
        let text = text.trim_start();
        let sign = usize::from(text.starts_with('-'));
        let end = text[sign..]
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .map_or(text.len(), |i| i + sign);
        return text[..end].parse().unwrap_or(0.0);
    }
    value.to_f64_lossy().unwrap_or(0.0)
}

fn is_true(value: Option<&kv3::Value>) -> bool {
    value.is_some_and(|v| v.as_bool() == Some(true) || v.as_str() == Some("true"))
}

/// Set to anything but zero or empty; `m_iItemTier` is an enum name (`EModTier_3`).
fn is_truthy(value: Option<&kv3::Value>) -> bool {
    value.is_some_and(|v| match v.kind() {
        kv3::Kind::String(s) => !s.is_empty(),
        kv3::Kind::Bool(b) => *b,
        _ => v.to_f64_lossy().is_some_and(|n| n != 0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEROES: &str = include_str!("../../tests/fixtures/cc_heroes.kv3");
    const ABILITIES: &str = include_str!("../../tests/fixtures/cc_abilities.kv3");

    fn fixture() -> BTreeMap<String, CcAbility> {
        classify(
            &kv3::Document::from_text(HEROES).expect("heroes parse"),
            &kv3::Document::from_text(ABILITIES).expect("abilities parse"),
        )
        .expect("classify")
    }

    fn kinds(set: &BTreeMap<String, CcAbility>, class: &str) -> Vec<(CcKind, u8)> {
        let ability = set.get(class).unwrap_or_else(|| panic!("{class} missing"));
        ability.kinds.iter().map(|(&k, &t)| (k, t)).collect()
    }

    #[test]
    fn grasping_hands_roots_and_tethers_through_its_modifiers() {
        let set = fixture();
        assert_eq!(
            kinds(&set, "ability_necro_zombiewall"),
            [(CcKind::Immobilize, 0), (CcKind::Tether, 0)]
        );
        assert_eq!(
            set["ability_necro_zombiewall"].owner,
            CcOwner::Hero("hero_necro".into())
        );
    }

    #[test]
    fn a_knockup_applied_from_code_is_found_by_its_property() {
        let set = fixture();
        assert_eq!(
            kinds(&set, "ability_necro_gravestone"),
            [(CcKind::Knockup, 0)]
        );
        assert_eq!(
            kinds(&set, "ability_bounce_pad"),
            [(CcKind::Stun, 3), (CcKind::Knockup, 0)]
        );
        assert!(!set["ability_bounce_pad"].gated());
    }

    #[test]
    fn grapple_arm_immobilizes_through_its_state_mask() {
        assert_eq!(
            kinds(&fixture(), "citadel_ability_hook"),
            [(CcKind::Immobilize, 0)]
        );
    }

    #[test]
    fn combo_counts_as_a_stun() {
        assert!(kinds(&fixture(), "ability_ult_combo").contains(&(CcKind::Stun, 0)));
    }

    #[test]
    fn a_cc_that_only_an_upgrade_turns_on_is_kept_and_gated() {
        let set = fixture();
        assert_eq!(kinds(&set, "ability_health_swap"), [(CcKind::Silence, 2)]);
        assert!(set["ability_health_swap"].gated());
    }

    #[test]
    fn an_item_with_a_stun_is_an_item() {
        let set = fixture();
        assert_eq!(kinds(&set, "upgrade_target_stun"), [(CcKind::Stun, 0)]);
        assert_eq!(set["upgrade_target_stun"].owner, CcOwner::Item);
    }

    #[test]
    fn a_switch_property_left_off_and_never_upgraded_is_not_cc() {
        let set = fixture();
        // Frozen Shelter's freeze is behind `FreezeEnemies = 0`; it only slows.
        assert!(!set.contains_key("ability_ice_dome"));
        // Spirit Snare carries a disarm modifier behind `DisarmTargets = 0`.
        assert_eq!(
            kinds(&set, "ability_immobilize_trap"),
            [(CcKind::Tether, 0), (CcKind::Silence, 0)]
        );
    }

    #[test]
    fn slows_are_not_hard_cc() {
        assert!(!fixture().contains_key("ability_incendiary_projectile"));
    }

    #[test]
    fn ally_tethers_and_pulls_are_excluded_by_name() {
        let set = fixture();
        assert!(!set.contains_key("citadel_ability_tangotether"));
        assert!(!set.contains_key("upgrade_rescue_beam"));
    }

    #[test]
    fn the_fixture_holds_exactly_the_expected_set() {
        let set = fixture();
        let names: Vec<&str> = set.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "ability_bounce_pad",
                "ability_health_swap",
                "ability_immobilize_trap",
                "ability_necro_gravestone",
                "ability_necro_zombiewall",
                "ability_ult_combo",
                "citadel_ability_hook",
                "upgrade_target_stun",
            ]
        );
    }

    #[test]
    fn a_missing_archive_is_an_io_error() {
        let err = cc_abilities(std::env::temp_dir().join("no-such-citadel")).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Io {
                    kind: std::io::ErrorKind::NotFound,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn the_installed_game_has_hero_and_item_cc() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let set = cc_abilities(&dir).expect("cc abilities");
        let items = set.values().filter(|a| a.owner == CcOwner::Item).count();
        assert!(
            set.len() - items > 40,
            "{} hero abilities",
            set.len() - items
        );
        assert!(items > 5, "{items} items");
    }
}
