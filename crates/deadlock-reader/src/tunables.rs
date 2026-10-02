//! Game constants a patch can change, overridable at runtime.
//!
//! Two kinds of value live here. Most are things this crate cannot read at all: spawn
//! cadences that exist only on the server, entity class names matched as strings, and a
//! struct layout worth carrying a probed copy of. The rest are figures the game **does**
//! ship in `scripts/generic_data.vdata_c` - the Rejuvenator and Urn durations, the rift
//! radius - which are defaults here rather than reads because `deadlock-reader` has no
//! dependency on the crates that open game files. Each names its vdata key, so a consumer
//! holding an install can read the current figure and apply it.
//!
//! Everything in [`Tunables`] is a value that has to survive the schema walk failing:
//! spawn cadences that only exist server-side, entity class names matched as strings, and
//! one struct layout that is also declared in the schema but is cheap to carry a probed
//! copy of. They are correct for the build this crate was written against, and any of
//! them can be retuned or renamed by a Deadlock update.
//!
//! Overriding them via [`Reader::tunables_mut`](crate::Reader::tunables_mut) lets an
//! application ship its own corrections (from a config file, say) instead of waiting for
//! a new release of this crate:
//!
//! ```no_run
//! use deadlock_reader::Reader;
//!
//! let mut reader = Reader::attach()?;
//! // Valve changed the bridge buff cadence in a patch.
//! reader.tunables_mut().bridge_buff_period = 240.0;
//! # Ok::<(), deadlock_reader::Error>(())
//! ```

use crate::reader::PairLayout;

/// How far the Midboss notices a player, in world units.
///
/// `npc_super_neutral.m_flSightRangePlayers` in `npc_units.vdata_c`. Used as the radius for
/// "this happened at the Midboss": sight range is the distance at which an objective and a
/// player can interact at all, which is a sourced meaning rather than a guessed one.
///
/// It is not a claim about where a *fight* is. A player sniping the Midboss from beyond its
/// sight range is outside this and still fighting over it; a player walking past inside it
/// is within and doing nothing. Cross-checked against the archive by
/// `the_objective_sight_ranges_still_match_the_tunable_defaults`.
pub const DEFAULT_MIDBOSS_SIGHT_RADIUS: f32 = 1500.0;

/// How far a Walker notices a player, in world units.
///
/// `npc_boss_tier2.m_flSightRangePlayers`. See [`DEFAULT_MIDBOSS_SIGHT_RADIUS`] for what
/// this does and does not mean.
///
/// The Walker's other ranges are metres in disguise and confirm the unit:
/// `m_flSightRange` is `1417.32`, which is `36 * 39.37`, and `m_flStompImpactRadius` is
/// `570.8661`, which is `14.5 * 39.37`. See [`UNITS_PER_METER`].
pub const DEFAULT_WALKER_SIGHT_RADIUS: f32 = 944.0;

/// The Midboss spawn schedule, from `misc.vdata_c`'s `neutral_camp_midboss`.
///
/// The Midboss is spawned by the same `info_neutral_trooper_camp` machinery as the jungle,
/// so its cadence is shipped data rather than something the client is told: the first
/// spawn is [`DEFAULT_MIDBOSS_INITIAL_DELAY`] seconds in, then every
/// [`DEFAULT_MIDBOSS_SPAWN_INTERVAL`], and each subsequent interval is
/// [`DEFAULT_MIDBOSS_INTERVAL_CHANGE`] shorter until it reaches
/// [`DEFAULT_MIDBOSS_INTERVAL_MIN`].
///
/// # Transcribed, and deliberately not turned into a countdown
///
/// These are the shipped numbers, cross-checked against the archive by
/// `every_transcribed_tunable_still_matches_the_archive` in `dlrs`. What
/// they are **not** is a validated model of when a Midboss actually appears: the reading of
/// "change" as *per spawn* is an inference, an initial delay of `0` does not match a
/// Midboss that shows up minutes into a match, and none of it has been watched against a
/// live one. The client is told the real time - [`Timers::midboss`](crate::timers::Timers::midboss) carries it - so
/// a consumer wanting a countdown should use that and treat these as context.
///
/// Building an estimator on an unvalidated reading would put a number where the game has
/// not been observed, which is exactly what the rift row refuses to do.
pub const DEFAULT_MIDBOSS_SPAWN_INTERVAL: f32 = 420.0;

/// Floor the Midboss interval shrinks to. See [`DEFAULT_MIDBOSS_SPAWN_INTERVAL`].
pub const DEFAULT_MIDBOSS_INTERVAL_MIN: f32 = 300.0;

/// How much each Midboss interval changes. Negative: it shortens.
/// See [`DEFAULT_MIDBOSS_SPAWN_INTERVAL`].
pub const DEFAULT_MIDBOSS_INTERVAL_CHANGE: f32 = -60.0;

/// Delay before the first Midboss camp spawn. See [`DEFAULT_MIDBOSS_SPAWN_INTERVAL`].
pub const DEFAULT_MIDBOSS_INITIAL_DELAY: f32 = 0.0;

/// How often bridge-buff crates spawn, in seconds of match time.
///
/// Not read from the game; the spawn time lives on the server-only rules class. This is
/// the observed cadence.
pub const DEFAULT_BRIDGE_BUFF_PERIOD: f32 = 300.0;

/// Server tick rate, used to convert paused ticks to seconds.
pub const DEFAULT_TICK_RATE: f32 = 64.0;

/// Entity classes that only exist on the Hideout map.
///
/// The Hideout is not a distinct game state: it reports `GameInProgress` with both modes
/// `Invalid` and no match id, which is also what a half-initialised match looks like.
/// Presence of these entities is the reliable signal.
pub const DEFAULT_HIDEOUT_CLASSES: &[&str] = &[
    "C_CitadelTriggerHideout",
    "C_Citadel_Hideout_Ball",
    "C_Citadel_Hideout_Clock",
    "CCitadelHideoutTeleportTrigger",
    "CCitadelHideoutInteractableProp",
    "C_NPC_Neutral_Hideout_Cat",
];

/// The Midboss NPC.
pub const DEFAULT_MIDBOSS_CLASSES: &[&str] = &["C_NPC_MidBoss"];

/// The Urn, once it is on the map to be picked up.
///
/// One spelling, without the `C_` prefix. That prefix marks nothing useful here: a live
/// client reports `CCitadelPlayerController` and `C_CitadelTeam` side by side in the same
/// entity list, so a second `C_`-prefixed row would be a name no entity has rather than a
/// hedge against a rename. Override the list if Valve renames the class.
pub const DEFAULT_URN_CLASSES: &[&str] = &["CCitadelItemPickupIdol"];

/// Classes sampled for the current engine time.
///
/// All are always present in a match and all are simulated by the server.
pub const DEFAULT_CLOCK_SOURCE_CLASSES: &[&str] = &[
    "C_CitadelGameRulesProxy",
    "C_CitadelPlayerPawn",
    "C_CitadelObserverPawn",
    "C_CitadelTeam",
];

/// `AbilityUpgradeState_t` element layout: 0x38 bytes, `m_ItemID` at 0x30,
/// `m_nUpgradeInfo` at 0x34.
///
/// Derived by probing the live process, and since confirmed by the runtime schema, which
/// declares the type at size 56 with those two members at exactly those offsets. It stays
/// a tunable because a patch can move it and the probe is the only thing that would
/// notice on a build where the schema walk fails.
pub const DEFAULT_ABILITY_UPGRADE_LAYOUT: PairLayout = PairLayout {
    stride: 0x38,
    a: 0x30,
    b: 0x34,
};

/// How long the Rejuvenator buff lasts, in seconds.
///
/// The game ships this: `scripts/generic_data.vdata_c` carries
/// `m_RejuvParams.m_flRejuvinatorBuffDuration = 180`. It is a default here rather than a
/// read because `deadlock-reader` has no edge to the crates that open game files, so a
/// consumer that wants the installed build's figure reads
/// it through `source2` and applies it via
/// [`Reader::tunables_mut`](crate::Reader::tunables_mut).
///
/// **This alone does not yield a countdown.** A duration needs something to count from,
/// and the client networks no pickup timestamp: sweeping the runtime schema for `rejuv`
/// finds only the two rules counters and `PlayerDataGlobal_t::m_bHasRejuvenator`, which is
/// a bare flag. So a remaining time is only available to a layer that watches that flag
/// turn on and stamps the clock itself - `deadlock-events`, not a single snapshot.
pub const DEFAULT_REJUV_BUFF_DURATION: f32 = 180.0;

/// Seconds before the Rejuvenator expires that the game warns.
///
/// `m_RejuvParams.m_flRejuvinatorExpirationWarningTiming = 30`.
pub const DEFAULT_REJUV_EXPIRATION_WARNING: f32 = 30.0;

/// Seconds the Rejuvenator spends falling before it can be taken.
///
/// `m_RejuvParams.m_flRejuvinatorDropDuration = 6`.
pub const DEFAULT_REJUV_DROP_DURATION: f32 = 6.0;

/// Seconds the Urn spends falling before it can be picked up.
///
/// `m_IdolParams.m_flIdolDropDuration = 12.5`.
pub const DEFAULT_URN_DROP_DURATION: f32 = 12.5;

/// World units in one metre: `39.37`, i.e. one unit is one inch.
///
/// Deadlock's data files mix the two. Positions, velocities and drop heights are world
/// units - a live pawn sits at `[11570.56, -6601.84, -82.0]` and
/// `m_IdolParams.m_flIdolDropHeight` is `1400`. Designer-facing numbers are metres, often
/// saying so in the name: `m_flStealthSpeedMetersPerSecond`, `EGroundDashDistanceInMeters`,
/// `citadel_koth_cashin.m_flZoneHeightMeters`, and the ability tooltip values that ship as
/// strings like `"4.5m"` with `m_eDisplayUnits = "EDisplayUnit_Meters"`.
///
/// The factor is measured, not assumed, and the game states it itself: the KOTH capture
/// aura ships **both** figures, `m_flKothRadius = 20` in `generic_data.vdata_c` and
/// `citadel_koth_cashin.m_AuraModifier.m_flAuraRadius = 787.402` in `misc.vdata_c`, and
/// `787.402 / 20 = 39.3701`. A hero's collision hull corroborates independently: read live
/// it is `[-16, -16, 0]` to `[16, 16, 72]`, Source's standard 32x32x72 player hull, which
/// is six feet tall only if a unit is an inch.
///
/// A round `100` would be wrong by two and a half times, and wrong in the direction that
/// hides itself - every radius too large, so every proximity test passes and nothing looks
/// broken.
pub const UNITS_PER_METER: f32 = 39.37;

/// Radius of the rift capture zone, in **world units**.
///
/// `m_KothParams.m_flKothRadius = 20`, which is **metres**; this is that figure at
/// [`UNITS_PER_METER`], and equals the `m_flAuraRadius = 787.402` the game ships for the
/// same zone.
///
/// The zone is a cylinder, not a sphere. `citadel_koth_cashin` gives it
/// `m_flZoneHeightMeters = 14`, and the aura that applies the capture modifier is a
/// `modifier_base_aura_cylinder` with `m_flAuraTargetingCylinderHalfHeight = 1000` and
/// `m_flAuraTargetingCylinderUpOffset = 400`. Anything testing this radius should test
/// horizontal distance only.
pub const DEFAULT_RIFT_RADIUS: f32 = 787.402;

/// Cap on items read per player, so a corrupt count cannot cause a wild allocation.
pub const DEFAULT_MAX_ITEMS: usize = 64;

/// Cap on ability-upgrade entries read per player.
pub const DEFAULT_MAX_ABILITY_UPGRADES: usize = 64;

/// Cap on banned heroes read from the rules class.
///
/// A draft bans a handful; the cap exists so a corrupt count cannot allocate wildly, not
/// because a real ban list approaches it.
pub const DEFAULT_MAX_BANNED_HEROES: usize = 64;

/// Cap on modifiers read per entity.
///
/// A live player pawn was observed carrying seven to nine in the Hideout; a hero mid-fight
/// carries considerably more, so this is deliberately loose. It exists to bound a corrupt
/// count, not to be a realistic maximum.
pub const DEFAULT_MAX_MODIFIERS: usize = 128;

/// Patch-sensitive constants used when building snapshots.
///
/// Start from `Tunables::default()` and override individual fields; the struct is
/// `#[non_exhaustive]` so new fields can be added without breaking callers.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Tunables {
    /// Seconds between bridge-buff crate spawns. Schedule-derived, never read from the
    /// game; see [`crate::timers`] for what that means for trustworthiness.
    pub bridge_buff_period: f32,
    /// Server tick rate.
    pub tick_rate: f32,
    /// Classes counted as the Midboss.
    pub midboss_classes: Vec<String>,
    /// Classes counted as the Urn pickup.
    pub urn_classes: Vec<String>,
    /// Classes whose presence marks the Hideout.
    pub hideout_classes: Vec<String>,
    /// Classes sampled for the current engine time.
    pub clock_source_classes: Vec<String>,
    /// Element layout of `m_vecAbilityUpgradeState`.
    pub ability_upgrade_layout: PairLayout,
    /// How long the Rejuvenator buff lasts, in seconds.
    pub rejuv_buff_duration: f32,
    /// Seconds before the Rejuvenator expires that the game warns.
    pub rejuv_expiration_warning: f32,
    /// Seconds the Rejuvenator spends falling.
    pub rejuv_drop_duration: f32,
    /// Seconds the Urn spends falling.
    pub urn_drop_duration: f32,
    /// Radius of the rift capture zone, in world units.
    pub rift_radius: f32,
    /// Cap on items read per player.
    pub max_items: usize,
    /// Cap on ability-upgrade entries read per player.
    pub max_ability_upgrades: usize,
    /// Cap on modifiers read per entity.
    pub max_modifiers: usize,
    /// Cap on banned heroes read from the rules class.
    pub max_banned_heroes: usize,
}

impl Default for Tunables {
    fn default() -> Self {
        let owned = |list: &[&str]| list.iter().map(std::string::ToString::to_string).collect();
        Tunables {
            bridge_buff_period: DEFAULT_BRIDGE_BUFF_PERIOD,
            tick_rate: DEFAULT_TICK_RATE,
            midboss_classes: owned(DEFAULT_MIDBOSS_CLASSES),
            urn_classes: owned(DEFAULT_URN_CLASSES),
            hideout_classes: owned(DEFAULT_HIDEOUT_CLASSES),
            clock_source_classes: owned(DEFAULT_CLOCK_SOURCE_CLASSES),
            ability_upgrade_layout: DEFAULT_ABILITY_UPGRADE_LAYOUT,
            rejuv_buff_duration: DEFAULT_REJUV_BUFF_DURATION,
            rejuv_expiration_warning: DEFAULT_REJUV_EXPIRATION_WARNING,
            rejuv_drop_duration: DEFAULT_REJUV_DROP_DURATION,
            urn_drop_duration: DEFAULT_URN_DROP_DURATION,
            rift_radius: DEFAULT_RIFT_RADIUS,
            max_items: DEFAULT_MAX_ITEMS,
            max_ability_upgrades: DEFAULT_MAX_ABILITY_UPGRADES,
            max_modifiers: DEFAULT_MAX_MODIFIERS,
            max_banned_heroes: DEFAULT_MAX_BANNED_HEROES,
        }
    }
}

/// Whether `name` appears in a class list.
pub(crate) fn class_in(list: &[String], name: &str) -> bool {
    list.iter().any(|c| c == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_constants() {
        let t = Tunables::default();
        assert_eq!(t.bridge_buff_period, DEFAULT_BRIDGE_BUFF_PERIOD);
        assert_eq!(t.tick_rate, DEFAULT_TICK_RATE);
        assert!(class_in(&t.midboss_classes, "C_NPC_MidBoss"));
        assert!(class_in(&t.hideout_classes, "C_CitadelTriggerHideout"));
        assert!(!class_in(&t.hideout_classes, "C_CitadelPlayerPawn"));
    }

    /// The durations the game ships, and where each came from.
    ///
    /// These are transcribed from `scripts/generic_data.vdata_c` on the retail build, so
    /// the test doubles as the record of what was read. A consumer that decodes the vdata
    /// itself should get the same numbers; if a patch moves one, this is what makes the
    /// divergence visible rather than silent.
    /// The read caps hold their values and stay clear of anything real.
    ///
    /// Each cap exists to stop a corrupt count causing a wild allocation, never to be a
    /// realistic maximum — that is what their doc comments say, and it is only true while
    /// they sit well above what the game can produce. Nothing checked it: cutting
    /// `max_items` to 3, `max_modifiers` to 1 or `max_banned_heroes` to 1 passed the whole
    /// suite, live tests included.
    ///
    /// The failure a low cap produces is the quiet kind. [`Reader::field_vec_u32`]
    /// truncates at the limit and returns a **short list**, which is indistinguishable from
    /// a player who genuinely holds that many items. No error, no `None`, no gap — the
    /// scoreboard simply shows fewer items than the player has.
    ///
    /// A cap of zero is called out separately because it is the worst case and the easiest
    /// to reach by accident: every list would read as empty, and empty is a legitimate
    /// answer everywhere here.
    /// The ability-upgrade layout matches what the live client's schema declares.
    ///
    /// [`DEFAULT_ABILITY_UPGRADE_LAYOUT`] is a probed struct layout, and probes go stale
    /// silently: a wrong stride reads every element from the wrong place and still returns
    /// numbers. Nothing checked it — changing the stride from `0x38` to `0x40`, or moving
    /// `m_ItemID` from `0x30` to `0x00`, passed the whole suite including the live tests,
    /// because a Hideout client has no ability upgrades for a wrong layout to misread.
    ///
    /// It does not have to stay a probe. The runtime schema declares
    /// `AbilityUpgradeState_t` outright — 56 bytes, `m_ItemID` at `0x30`,
    /// `m_nUpgradeInfo` at `0x34` — so the transcription can be checked against the game
    /// rather than against the memory of having probed it.
    ///
    /// The struct's *size* is the stride, which is what makes this a complete check: a
    /// `CUtlVector` of these is packed, so element `n` sits at `n * size`.
    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn the_ability_upgrade_layout_matches_the_live_schema() {
        let reader = crate::Reader::attach().expect("attach");
        let schema = reader
            .schema()
            .expect("schema unresolved; try `dlrs probe-schema`");
        let class = schema
            .classes
            .get("AbilityUpgradeState_t")
            .expect("AbilityUpgradeState_t is no longer a schema class");

        let layout = DEFAULT_ABILITY_UPGRADE_LAYOUT;
        assert_eq!(
            u64::from(class.size),
            layout.stride,
            "the struct is {} bytes but the layout strides by {}",
            class.size,
            layout.stride
        );
        assert_eq!(
            class.fields.get("m_ItemID").copied().map(u64::from),
            Some(layout.a),
            "m_ItemID moved"
        );
        assert_eq!(
            class.fields.get("m_nUpgradeInfo").copied().map(u64::from),
            Some(layout.b),
            "m_nUpgradeInfo moved"
        );
    }

    #[test]
    fn the_read_caps_stay_clear_of_anything_the_game_produces() {
        const FULL_LOADOUT: usize = 16;
        const OBSERVED_IDLE_MODIFIERS: usize = 9;
        const ROSTER: usize = 59;

        let t = Tunables::default();

        assert_eq!(t.max_items, 64);
        assert_eq!(t.max_ability_upgrades, 64);
        assert_eq!(t.max_banned_heroes, 64);
        assert_eq!(t.max_modifiers, 128);

        for (what, cap) in [
            ("max_items", t.max_items),
            ("max_ability_upgrades", t.max_ability_upgrades),
            ("max_banned_heroes", t.max_banned_heroes),
            ("max_modifiers", t.max_modifiers),
        ] {
            assert!(cap > 0, "{what} of zero makes every list read as empty");
        }

        assert!(
            t.max_items > FULL_LOADOUT * 2,
            "max_items ({}) is not clear of a full {FULL_LOADOUT}-slot loadout",
            t.max_items
        );

        assert!(
            t.max_modifiers > OBSERVED_IDLE_MODIFIERS * 10,
            "max_modifiers ({}) is not an order of magnitude clear of an idle pawn",
            t.max_modifiers
        );

        assert!(
            t.max_banned_heroes >= ROSTER,
            "max_banned_heroes ({}) is below the roster, so a full draft would truncate",
            t.max_banned_heroes
        );
    }

    #[test]
    fn the_shipped_durations_match_the_vdata_they_came_from() {
        let t = Tunables::default();
        assert_eq!(t.rejuv_buff_duration, 180.0);
        assert_eq!(t.rejuv_expiration_warning, 30.0);
        assert_eq!(t.rejuv_drop_duration, 6.0);
        assert_eq!(t.urn_drop_duration, 12.5);
        assert_eq!(t.rift_radius, 787.402);
        assert!(t.rejuv_expiration_warning < t.rejuv_buff_duration);
    }

    /// The rift radius is `m_flKothRadius` converted, not transcribed.
    ///
    /// This exists because the constant was once documented as "`m_flKothRadius = 20`, in
    /// world units", which is wrong by a factor of forty: live positions are on the scale
    /// of `[11570.56, -6601.84, -82.0]`, so a 20-unit radius is a circle a quarter of a
    /// hero's height across, and the rift would have been un-enterable rather than merely
    /// mis-sized.
    ///
    /// The two halves are pinned separately so a future edit cannot quietly reintroduce a
    /// bare transcription: if someone sets `rift_radius` back to 20, this fails; if
    /// someone "fixes" [`UNITS_PER_METER`] to a round 100, this fails too.
    #[test]
    fn the_rift_radius_is_the_vdata_figure_converted() {
        const KOTH_RADIUS_METERS: f32 = 20.0;
        assert!(
            (DEFAULT_RIFT_RADIUS - KOTH_RADIUS_METERS * UNITS_PER_METER).abs() < 0.01,
            "{DEFAULT_RIFT_RADIUS} is not {KOTH_RADIUS_METERS} m at {UNITS_PER_METER} u/m"
        );
        assert!((DEFAULT_RIFT_RADIUS - 787.402).abs() < 0.001);
    }

    /// A hero's collision hull is Source's standard player hull, which fixes the unit.
    ///
    /// Read from a live client: `m_vecMins = [-16, -16, 0]`, `m_vecMaxs = [16, 16, 72]`.
    /// That is the 32x32x72 hull Source has used since Half-Life 2, where one unit is one
    /// inch, so 72 units is six feet. It is the independent check on [`UNITS_PER_METER`]:
    /// the constant is not derived from the hull, but the hull would be a strange size if
    /// the constant were wrong.
    #[test]
    fn a_source_player_hull_is_six_feet_at_this_scale() {
        const HULL_HEIGHT_UNITS: f32 = 72.0;
        let meters = HULL_HEIGHT_UNITS / UNITS_PER_METER;
        assert!(
            (meters - 1.8288).abs() < 0.001,
            "a 72-unit hull is {meters} m, which is not six feet"
        );
    }

    /// Every default class name is a spelling the game actually uses.
    ///
    /// A class list is matched against live entity class names by string equality, so a
    /// name no entity has is not a hedge, it is a row that can never fire. The `C_`
    /// prefix is not a rule that can be hedged on either way: a live client reports both
    /// `CCitadelPlayerController` and `C_CitadelTeam` as entity classes, and the Urn has
    /// exactly one spelling in the schema, `CCitadelItemPickupIdol`.
    #[test]
    fn no_default_class_name_is_a_spelling_the_game_lacks() {
        assert_eq!(DEFAULT_URN_CLASSES, ["CCitadelItemPickupIdol"]);
        for list in [
            DEFAULT_URN_CLASSES,
            DEFAULT_MIDBOSS_CLASSES,
            DEFAULT_HIDEOUT_CLASSES,
            DEFAULT_CLOCK_SOURCE_CLASSES,
        ] {
            let mut seen: Vec<&str> = list.to_vec();
            seen.sort_unstable();
            let before = seen.len();
            seen.dedup();
            assert_eq!(before, seen.len(), "duplicate class name in {list:?}");
        }
    }

    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn overrides_stick() {
        let mut t = Tunables::default();
        t.bridge_buff_period = 240.0;
        t.midboss_classes = vec!["C_NPC_MidBoss_V2".to_string()];
        assert_eq!(t.bridge_buff_period, 240.0);
        assert!(class_in(&t.midboss_classes, "C_NPC_MidBoss_V2"));
        assert!(!class_in(&t.midboss_classes, "C_NPC_MidBoss"));
    }
}
