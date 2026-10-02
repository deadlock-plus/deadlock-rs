//! Destructible map structures.

use std::collections::HashMap;

use deadlock_core::Team;

use crate::entity::{EntitySnapshot, is_structure_class};
use crate::reader::Reader;
use crate::snapshot::BASE_ENTITY;

/// What kind of destructible structure this is.
///
/// Was a `&'static str`, compared literally at every call site - `o.label == "midboss"`,
/// `structures("walker")`. A typo in any of those matched nothing and returned an empty
/// iterator or `false`, which reads as "there are no walkers on this map" rather than as a
/// mistake. There are nine of these and the game defines all of them, so the closed set is
/// the honest type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectiveKind {
    /// Lane guardian (`C_NPC_TrooperBoss`).
    Guardian,
    /// Walker (`C_NPC_Boss_Tier2`).
    Walker,
    /// Base guardian (`C_NPC_BarrackBoss`).
    BaseGuardian,
    /// Patron (`C_NPC_Boss_Tier3`).
    Patron,
    /// Destructible shrine (`C_Citadel_Destroyable_Building`).
    Shrine,
    /// Lane trooper (`C_NPC_Trooper`).
    LaneTrooper,
    /// Neutral camp creep (`C_NPC_TrooperNeutral`).
    NeutralCreep,
    /// Sinner's Sacrifice (`C_NPC_Neutral_SinnersSacrifice`).
    SinnersSacrifice,
    /// Mid-boss (`C_NPC_MidBoss`).
    Midboss,
}

/// Every class name [`ObjectiveKind::from_class`] recognises.
///
/// A list rather than only match arms, because a `match` on `&str` is not exhaustive and
/// the compiler cannot tell you a class was dropped. These names are compared against an
/// entity's own class name and never used to look up a field, so a wrong one resolves to
/// nothing and the objective silently never appears - see
/// `every_objective_class_is_a_class_the_client_declares`.
pub const OBJECTIVE_CLASSES: &[&str] = &[
    "C_NPC_TrooperBoss",
    "C_NPC_Boss_Tier2",
    "C_NPC_BarrackBoss",
    "C_NPC_Boss_Tier3",
    "C_Citadel_Destroyable_Building",
    "C_NPC_Trooper",
    "C_NPC_TrooperNeutral",
    "C_NPC_Neutral_SinnersSacrifice",
    "C_NPC_MidBoss",
];

impl ObjectiveKind {
    /// Which kind a schema class name denotes, if any.
    ///
    /// The class-to-kind mapping is `LAYOUT.md` §6.3.
    /// `None` for everything that is not a destructible structure, which is most of a live
    /// match - use [`crate::entity::is_structure_class`] to tell structures from creeps.
    pub fn from_class(class: &str) -> Option<Self> {
        Some(match class {
            "C_NPC_TrooperBoss" => ObjectiveKind::Guardian,
            "C_NPC_Boss_Tier2" => ObjectiveKind::Walker,
            "C_NPC_BarrackBoss" => ObjectiveKind::BaseGuardian,
            "C_NPC_Boss_Tier3" => ObjectiveKind::Patron,
            "C_Citadel_Destroyable_Building" => ObjectiveKind::Shrine,
            "C_NPC_Trooper" => ObjectiveKind::LaneTrooper,
            "C_NPC_TrooperNeutral" => ObjectiveKind::NeutralCreep,
            "C_NPC_Neutral_SinnersSacrifice" => ObjectiveKind::SinnersSacrifice,
            "C_NPC_MidBoss" => ObjectiveKind::Midboss,
            _ => return None,
        })
    }

    /// The short label, e.g. `walker`.
    ///
    /// These are the strings the field used to hold, so anything printing or serialising
    /// an objective is unchanged.
    pub fn label(self) -> &'static str {
        match self {
            ObjectiveKind::Guardian => "guardian",
            ObjectiveKind::Walker => "walker",
            ObjectiveKind::BaseGuardian => "base_guardian",
            ObjectiveKind::Patron => "patron",
            ObjectiveKind::Shrine => "shrine",
            ObjectiveKind::LaneTrooper => "lane_trooper",
            ObjectiveKind::NeutralCreep => "neutral_creep",
            ObjectiveKind::SinnersSacrifice => "sinners_sacrifice",
            ObjectiveKind::Midboss => "midboss",
        }
    }
}

impl std::fmt::Display for ObjectiveKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.label())
    }
}

/// Serialised as its label rather than its variant name, so the JSON shape is exactly what
/// it was when this field held a `&'static str`.
#[cfg(feature = "serde")]
impl serde::Serialize for ObjectiveKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.label())
    }
}

/// A destructible objective.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Objective {
    /// What kind of structure this is.
    pub kind: ObjectiveKind,
    /// Team name as the game reports it.
    pub team_name: Option<String>,
    /// Schema class name.
    pub class: String,
    /// Entity instance address.
    pub address: u64,
    /// Team number.
    pub team: Option<Team>,
    /// Current health.
    pub health: Option<i32>,
    /// Maximum health.
    pub max_health: Option<i32>,
    /// Lane index, where the class exposes one.
    pub lane: Option<i32>,
    /// Whether the game reports this structure as still standing.
    ///
    /// Settled from the destruction signals the game networks rather than from
    /// [`Objective::health`], which can read a stale positive value for a tick after a
    /// structure falls.
    pub state: StructureState,
    /// World position.
    ///
    /// Behind the `positions` feature, for the same reason [`PlayerRow::position`](crate::snapshot::PlayerRow::position) is:
    /// live coordinates are the one thing here the game does not already put on screen.
    ///
    /// A structure does not move, so this is less sensitive than a player's - but the
    /// Midboss and the Urn are objectives too, and where those are is exactly the kind of
    /// advantage the feature gate exists to keep opt-in.
    ///
    /// This is what proximity attribution needs: bucketing a fight as happening *at* the
    /// Midboss rather than merely while it was alive means comparing positions.
    ///
    /// Not verified against a live structure - the client this was written against sat in
    /// the Hideout, which reports zero objectives. The chain is the same one
    /// [`PlayerRow::position`](crate::snapshot::PlayerRow::position) uses, `m_pGameSceneNode` then
    /// `CGameSceneNode::m_vecAbsOrigin`, and that does read: a live pawn returned
    /// `[11570.56, -6601.84, -82.0]` through it.
    #[cfg(feature = "positions")]
    pub position: Option<[f32; 3]>,
}

/// Whether a structure is still standing.
///
/// The third variant is the point: a structure whose state could not be read is not a
/// destroyed one, and a counter that cannot tell the two apart reports a side as having
/// lost buildings it still holds.
///
/// Serialised as a lower-case label - `standing`, `destroyed`, `unknown` - to match the
/// case of every other string in a serialised [`Objective`], including
/// [`ObjectiveKind`]'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "snake_case")
)]
pub enum StructureState {
    /// Up, and none of the destruction signals say otherwise.
    Standing,
    /// The game reports it destroyed.
    Destroyed,
    /// Nothing could be read from it.
    Unknown,
}

/// The `m_lifeState` value an entity that is up carries.
///
/// Inferred, not read: `m_lifeState` is a bare `uint8` and the runtime schema declares no
/// enum for it, so the Source convention - `LIFE_ALIVE = 0`, then dying, dead,
/// respawnable, respawning - is engine lore rather than anything this workspace can check
/// against the game. Only "zero is up, anything else is not" is relied on, which holds
/// for every value in that convention, and no destroyed structure has been observed live
/// to confirm it.
const LIFE_ALIVE: u8 = 0;

/// Settle a structure's state from the three signals, most authoritative first.
///
/// `m_bDestroyed` is the game's own flag and decides on its own, but only
/// `C_Citadel_Destroyable_Building` declares one; the five NPC classes among the
/// structures have nothing like it. `m_lifeState` covers all six, at the cost of the
/// assumption on [`LIFE_ALIVE`]. Health comes last: it can read a stale positive value
/// for a tick after a structure falls, and a health that could not be read says nothing
/// at all rather than zero.
fn structure_state(
    destroyed: Option<bool>,
    life_state: Option<u8>,
    health: Option<i32>,
) -> StructureState {
    if let Some(destroyed) = destroyed {
        return if destroyed {
            StructureState::Destroyed
        } else {
            StructureState::Standing
        };
    }
    if let Some(life) = life_state {
        return if life == LIFE_ALIVE {
            StructureState::Standing
        } else {
            StructureState::Destroyed
        };
    }
    match health {
        Some(hp) if hp > 0 => StructureState::Standing,
        Some(_) => StructureState::Destroyed,
        None => StructureState::Unknown,
    }
}

pub(crate) fn collect_objectives(
    reader: &Reader,
    entities: &EntitySnapshot,
    team_names: &HashMap<Team, String>,
) -> Vec<Objective> {
    let mut out = Vec::new();
    for e in entities.all() {
        let class = e.best_name();
        // Structures only. A live match holds several hundred troopers and neutral
        // creeps, which would bury the ~20 things anyone means by "objectives".
        if !is_structure_class(class) {
            continue;
        }
        let Some(kind) = ObjectiveKind::from_class(class) else {
            continue;
        };
        // One read covers every field below. The highest of them, `m_bDestroyed` at
        // 0xf00, still sits inside the 4 KiB this pulls, and anything past it would fall
        // back to a live read rather than read wrong.
        let obj = reader.read_object(e.instance, class);
        let team = obj.u8(BASE_ENTITY, "m_iTeamNum").map(|t| Team(t as u32));
        let health = obj.i32(BASE_ENTITY, "m_iHealth");
        // Look m_bDestroyed up on the entity's own class, the way m_iLane is: one of the
        // six structure classes declares it, and asking a class that lacks it has to miss
        // rather than resolve to whatever sits at that offset.
        //
        // That class also networks `m_bActive` and `m_bFinal`. Neither is consulted,
        // because what they mean for a shrine that stands but is inactive is not
        // established.
        let destroyed = obj.bool(class, "m_bDestroyed");
        // m_lifeState is C_BaseEntity's, so every structure carries one - as long as the
        // runtime schema is in play. There is no baked fallback offset for it, so a
        // reader running on the offset table alone falls through to health.
        let life_state = obj.u8(BASE_ENTITY, "m_lifeState");
        #[cfg(feature = "positions")]
        let position = reader
            .field_ptr(e.instance, BASE_ENTITY, "m_pGameSceneNode")
            .and_then(|node| reader.field_vec3(node, "CGameSceneNode", "m_vecAbsOrigin"));
        out.push(Objective {
            kind,
            team_name: team.and_then(|t| team_names.get(&t).cloned()),
            class: class.to_string(),
            address: e.instance,
            team,
            health,
            max_health: obj.i32(BASE_ENTITY, "m_iMaxHealth"),
            // Look m_iLane up on the entity's own class: only some objective types
            // declare or inherit it, and asking a class that lacks it yields garbage.
            lane: obj.i32(class, "m_iLane"),
            state: structure_state(destroyed, life_state, health),
            #[cfg(feature = "positions")]
            position,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::is_structure_class;

    /// Troopers and neutral camps must not be reported as objectives; a live match has
    /// several hundred of them against ~20 structures.
    #[test]
    fn only_structures_count_as_objectives() {
        for c in [
            "C_NPC_Boss_Tier3",
            "C_NPC_Boss_Tier2",
            "C_NPC_TrooperBoss",
            "C_NPC_BarrackBoss",
            "C_Citadel_Destroyable_Building",
            "C_NPC_MidBoss",
        ] {
            assert!(is_structure_class(c), "{c} should be a structure");
        }
        for c in [
            "C_NPC_Trooper",
            "C_NPC_TrooperNeutral",
            "C_NPC_Neutral_SinnersSacrifice",
            "C_CitadelPlayerPawn",
        ] {
            assert!(!is_structure_class(c), "{c} should not be a structure");
        }
    }

    /// Every class the game defines, and nothing else. These strings were the whole
    /// mapping before it became a type, so a lost entry would silently stop an objective
    /// from ever appearing in a snapshot.
    #[test]
    fn every_structure_class_maps_to_a_kind() {
        let expected = [
            ("C_NPC_TrooperBoss", ObjectiveKind::Guardian),
            ("C_NPC_Boss_Tier2", ObjectiveKind::Walker),
            ("C_NPC_BarrackBoss", ObjectiveKind::BaseGuardian),
            ("C_NPC_Boss_Tier3", ObjectiveKind::Patron),
            ("C_Citadel_Destroyable_Building", ObjectiveKind::Shrine),
            ("C_NPC_Trooper", ObjectiveKind::LaneTrooper),
            ("C_NPC_TrooperNeutral", ObjectiveKind::NeutralCreep),
            (
                "C_NPC_Neutral_SinnersSacrifice",
                ObjectiveKind::SinnersSacrifice,
            ),
            ("C_NPC_MidBoss", ObjectiveKind::Midboss),
        ];
        for (class, kind) in expected {
            assert_eq!(ObjectiveKind::from_class(class), Some(kind), "{class}");
        }
        assert_eq!(ObjectiveKind::from_class("C_CitadelPlayerPawn"), None);
        assert_eq!(ObjectiveKind::from_class(""), None);
    }

    /// The list and the mapping cannot drift apart.
    ///
    /// A `match` on `&str` is not exhaustive, so nothing makes the two agree by
    /// construction; this is what does. Both directions matter: a class in the list that
    /// the mapping forgot returns `None`, and a kind no class produces is a variant
    /// nothing can ever be.
    #[test]
    fn the_objective_class_list_and_the_mapping_agree() {
        use std::collections::BTreeSet;
        let mut kinds = BTreeSet::new();
        for class in OBJECTIVE_CLASSES {
            let kind = ObjectiveKind::from_class(class)
                .unwrap_or_else(|| panic!("{class} is listed but maps to no kind"));
            assert!(kinds.insert(kind.label()), "{class} duplicates a kind");
        }
        let all = [
            ObjectiveKind::Guardian,
            ObjectiveKind::Walker,
            ObjectiveKind::BaseGuardian,
            ObjectiveKind::Patron,
            ObjectiveKind::Shrine,
            ObjectiveKind::LaneTrooper,
            ObjectiveKind::NeutralCreep,
            ObjectiveKind::SinnersSacrifice,
            ObjectiveKind::Midboss,
        ];
        for kind in all {
            assert!(
                kinds.contains(kind.label()),
                "no class produces {}",
                kind.label()
            );
        }
    }

    /// Every class name the objective mapping knows is a class the live client declares.
    ///
    /// The `(class, field)` scan in `fields.rs` cannot reach these. A class named here is
    /// matched against an entity's own name and never used to look up a field, so a
    /// misspelling resolves to nothing and the objective simply never appears - no
    /// compile error, no panic, no `None` anywhere a caller can see. `C_CitadelItemPickupIdol`
    /// was exactly that mistake once, spelled with an underscore the game does not use.
    ///
    /// Checks the mapping's own list rather than a copy of it, so a class added to
    /// `ObjectiveKind::from_class` without being added here cannot pass by being
    /// forgotten.
    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn every_objective_class_is_a_class_the_client_declares() {
        let reader = crate::Reader::attach().expect("attach");
        let schema = reader
            .schema()
            .expect("schema unresolved; try `dlrs probe-schema`");

        let missing: Vec<&str> = OBJECTIVE_CLASSES
            .iter()
            .copied()
            .filter(|c| !schema.classes.contains_key(*c))
            .collect();
        assert!(
            missing.is_empty(),
            "classes the live client does not declare: {missing:#?}"
        );

        for c in crate::entity::STRUCTURE_CLASSES {
            assert!(
                OBJECTIVE_CLASSES.contains(c),
                "{c} is a structure but has no ObjectiveKind"
            );
        }
    }

    /// The labels are what everything downstream prints and serialises, and two variants
    /// sharing one would make them indistinguishable in output while staying distinct in
    /// the type - the worst of both.
    #[test]
    fn labels_are_the_strings_the_field_used_to_hold_and_are_distinct() {
        let all = [
            ObjectiveKind::Guardian,
            ObjectiveKind::Walker,
            ObjectiveKind::BaseGuardian,
            ObjectiveKind::Patron,
            ObjectiveKind::Shrine,
            ObjectiveKind::LaneTrooper,
            ObjectiveKind::NeutralCreep,
            ObjectiveKind::SinnersSacrifice,
            ObjectiveKind::Midboss,
        ];
        let labels: std::collections::BTreeSet<_> = all.iter().map(|k| k.label()).collect();
        assert_eq!(labels.len(), all.len(), "two kinds share a label");
        assert_eq!(ObjectiveKind::Walker.label(), "walker");
        assert_eq!(ObjectiveKind::Midboss.to_string(), "midboss");
        assert_eq!(ObjectiveKind::BaseGuardian.label(), "base_guardian");
    }

    /// This field was a plain string, so anything consuming the JSON expects one. Deriving
    /// `Serialize` on the enum would have quietly changed `"walker"` to `"Walker"`.
    #[cfg(feature = "serde")]
    #[test]
    fn the_json_shape_is_unchanged_by_the_field_becoming_a_type() {
        let json = serde_json::to_string(&ObjectiveKind::BaseGuardian).unwrap();
        assert_eq!(json, "\"base_guardian\"");
    }

    /// A structure whose read settled on `state`.
    #[cfg(feature = "serde")]
    fn objective(state: StructureState) -> Objective {
        Objective {
            kind: ObjectiveKind::Shrine,
            team_name: None,
            class: "C_Citadel_Destroyable_Building".into(),
            address: 0x1000,
            team: None,
            health: Some(4000),
            max_health: Some(4000),
            lane: None,
            state,
            #[cfg(feature = "positions")]
            position: None,
        }
    }

    /// The failure this guards: a consumer reading the JSON has no reader to ask, so a
    /// destruction state that is not in the serialised objective is one it cannot see at
    /// all. The labels are lower-case for the same reason [`ObjectiveKind`]'s are.
    #[cfg(feature = "serde")]
    #[test]
    fn a_structures_state_travels_with_it_into_the_serialised_shape() {
        let json = serde_json::to_string(&objective(StructureState::Destroyed)).unwrap();
        assert!(json.contains(r#""state":"destroyed""#), "{json}");

        let json = serde_json::to_string(&objective(StructureState::Unknown)).unwrap();
        assert!(json.contains(r#""state":"unknown""#), "{json}");
    }

    /// The failure this guards: a shrine reports its own destruction, and counting it by
    /// health alone left it standing for as long as the health field kept a stale
    /// positive value.
    #[test]
    fn a_structure_flagged_destroyed_is_not_standing_whatever_its_health_reads() {
        assert_eq!(
            structure_state(Some(true), Some(0), Some(4000)),
            StructureState::Destroyed
        );
        assert_eq!(
            structure_state(Some(false), Some(2), Some(0)),
            StructureState::Standing,
            "the class's own flag settles it without consulting anything below"
        );
    }

    /// The failure this guards: `health.unwrap_or(0) > 0` turned a health that could not
    /// be read into a destroyed structure, so a refused read understated a side's
    /// standing structures and looked exactly like the side having lost them.
    #[test]
    fn a_structure_nothing_could_be_read_from_is_unknown_rather_than_destroyed() {
        assert_eq!(
            structure_state(None, None, None),
            StructureState::Unknown,
            "an unreadable structure is not a destroyed one"
        );
    }

    /// Five of the six structure classes declare no `m_bDestroyed`, so their state comes
    /// from `m_lifeState` and, when that cannot be resolved either, from health. The
    /// failure this guards is a fall-through that stops at the missing flag and reports
    /// every guardian, walker and patron as unknown.
    #[test]
    fn a_structure_with_no_destruction_flag_falls_back_to_life_state_then_to_health() {
        assert_eq!(
            structure_state(None, Some(LIFE_ALIVE), Some(100)),
            StructureState::Standing
        );
        assert_eq!(
            structure_state(None, Some(2), Some(100)),
            StructureState::Destroyed,
            "a dead life state outranks a health that has not caught up"
        );
        assert_eq!(
            structure_state(None, None, Some(100)),
            StructureState::Standing
        );
        assert_eq!(
            structure_state(None, None, Some(0)),
            StructureState::Destroyed
        );
    }
}
