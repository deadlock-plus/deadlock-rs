//! Modifiers: the buffs, debuffs and passives applied to an entity.
//!
//! Every entity carries a `CModifierProperty` at `C_BaseEntity::m_pModifierProp`, whose
//! `m_vecModifiers` is a vector of **pointers** to `CBaseModifier`. Pointers rather than
//! inline records because the elements are polymorphic: `CBaseModifier` is 144 bytes and
//! its `CCitadel_Modifier_*` subclasses are 192, 208, 216 and so on.
//!
//! # What a duration of -1 means
//!
//! `m_flDuration` is `-1.0` for a modifier that does not expire, which is what every
//! passive looks like. Reading it as a number of seconds gives an expiry in the past and
//! so a permanent modifier that always reads as finished. [`Modifier::expires_at`] returns
//! `None` for these instead.
//!
//! Observed against a live client: a player pawn in the Hideout carries seven to nine
//! modifiers, every one of them with `duration = -1.0` and a creation time a second or two
//! after the map loaded.
//!
//! # Naming a modifier
//!
//! A modifier is **not an entity**, so its concrete class cannot be recovered the way an
//! entity's is - it has no entry in the entity list to look up. What it does carry is
//! `m_nAbilitySubclassID`, and `scripts/modifiers.vdata_c` (81 top-level blocks, readable
//! since the LZ4 work) is the table that gives those ids meaning.
//!
//! The route that works is the object's own vtable. Deadlock's Windows build is MSVC, and
//! MSVC emits RTTI: the slot *before* a vtable points at a Complete Object Locator, which
//! carries the module-relative address of a type descriptor, whose name field holds the
//! mangled class name. Chasing that gives an exact answer with nothing to keep in step -
//! no table, no heuristics, and it names classes that appear in no data file.
//!
//! Read back off a live client in the Hideout, this yields
//! `CCitadel_Modifier_InHideoutMap`, `CCitadel_Modifier_HeroUpgradeBonuses`,
//! `CCitadel_Modifier_CombatStatus`, `CCitadel_Modifier_BarrierTracker`,
//! `CCitadel_Modifier_Slide_Debuff` and `CCitadel_Stamina_Regen_Jump_Reduction` - a set
//! that is obviously right for a player idling in the Hideout.
//!
//! One route was tried and refuted, recorded so it is not retried:
//! `m_nAbilitySubclassID` is *not* a `CUtlStringToken` of the modifier's own vdata block
//! name - hashing all 81 names in `scripts/modifiers.vdata_c` with the `MurmurHash2` that
//! derives item ids matches none of the ids a live pawn reports.
//!
//! But it is not meaningless, and an earlier note here called it a weak handle on the
//! strength of that one failed test. It identifies the **ability that applied the
//! modifier**: it equals that ability entity's `C_CitadelBaseAbility::m_nSubclassID`.
//! Measured on a live client, `CCitadel_Modifier_Slide_Debuff` carries `2335418656` and
//! `CCitadel_Ability_Slide` declares exactly that; `CCitadel_Stamina_Regen_Jump_Reduction`
//! carries `2207638101` against `CCitadel_Ability_Dash`.
//!
//! So a caller holding an [`EntitySnapshot`](crate::entity::EntitySnapshot) can name the
//! source ability by matching subclass ids across the ability entities on the map - but
//! that is the weaker of two routes and resolves only for abilities that are entities
//! right now.
//!
//! The stronger route needs no entity list at all. The id is a `CUtlStringToken` of the
//! ability's **vdata key**, the block name in `scripts/abilities.vdata_c`, which is how
//! `deadlock-data` already derives its item and ability ids. Joining every distinct live
//! ability subclass id against that roster matched **15 of 15, none unmatched**:
//! `2335418656` is `citadel_ability_slide`, `2207638101` is `citadel_ability_dash`,
//! `1065103387` is `citadel_ability_lightning_ball`.
//!
//! It is deliberately **not** a token of the C++ class name - hashing those agreed 0 of 17
//! times - and the difference is not cosmetic. `CCitadel_Ability_HoldMelee` resolves to
//! `citadel_ability_melee_gigawatt` and `CCitadel_Ability_PrimaryWeapon_Empty` to
//! `citadel_weapon_gigawatt_set`: the class is shared across heroes where the vdata key is
//! specific to one, so a class-name join would collapse abilities that are not the same.
//!
//! The id is still reported raw here. Resolving it needs the roster, and this crate has no
//! edge to `deadlock-data`.

use crate::reader::Reader;

/// Class that owns the modifier list.
const PROPERTY: &str = "CModifierProperty";
/// Base class of every modifier.
pub(crate) const MODIFIER: &str = "CBaseModifier";

/// One modifier applied to an entity.
///
/// Every field is an `Option` for the usual reason: a schema miss on one must not take out
/// the rest of the record.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Modifier {
    /// Address of the `CBaseModifier`, for callers that want to read more off it.
    pub address: u64,
    /// Concrete class name, e.g. `"CCitadel_Modifier_Stunned"`.
    ///
    /// Recovered from the object's vtable through MSVC RTTI; see the module docs.
    /// `None` when the chain does not check out, which is what a non-MSVC build, a
    /// destroyed modifier or a moved layout looks like. Never a guess.
    pub class: Option<String>,
    /// `m_nAbilitySubclassID`: which **ability** applied this, not which modifier it is.
    ///
    /// Equal to the applying ability entity's `C_CitadelBaseAbility::m_nSubclassID`, and
    /// a `CUtlStringToken` of that ability's **vdata key** - so `deadlock-data`'s item
    /// roster resolves it to a name with no entity list involved (15 of 15 live ids
    /// matched). Not the C++ class name, which agreed 0 of 17 times and would in any case
    /// collapse hero-specific abilities that share a class.
    ///
    /// Zero when nothing applied it - a passive the game grants directly reports zero,
    /// which is most of them. See the module docs for the measurement.
    pub subclass_id: Option<u32>,
    /// `m_nSerialNumber`, which distinguishes two applications of the same modifier.
    pub serial: Option<u32>,
    /// `m_flCreationTime`, in [`crate::snapshot::MatchClock::now`]'s base.
    pub creation_time: Option<f32>,
    /// `m_flDuration`. Negative means the modifier does not expire.
    pub duration: Option<f32>,
    /// `m_flLastAppliedTime`, which moves when a stack refreshes.
    pub last_applied_time: Option<f32>,
    /// Current stack count.
    pub stacks: Option<u16>,
    /// Highest stack count this modifier allows.
    pub max_stacks: Option<u16>,
    /// Handle of the ability that applied it, for grouping applications by cast.
    pub ability: Option<u32>,
    /// Handle of the entity that applied it.
    pub caster: Option<u32>,
    /// `m_iTeam`, the team of whatever **applied** the modifier - not the target's.
    ///
    /// Established against a live client rather than assumed, and the evidence is the
    /// case with no applier at all. Self-applied modifiers cannot separate the two
    /// readings, since applier and target are the same side. But
    /// `CCitadel_Modifier_InHideoutMap` carries `m_hCaster = 0xffffffff`
    /// ([`INVALID_HANDLE`](crate::entity::INVALID_HANDLE)) and reads `m_iTeam = 0` on
    /// pawns that are themselves team 2 and team 3. Were this the target's team it would
    /// read 2 and 3; it tracks the absent applier instead.
    ///
    /// That is what makes "was this applied by an enemy" answerable:
    /// `modifier.team != target.team` means an opposing side put it there.
    pub team: Option<u8>,
    /// Whether the modifier is present but currently doing nothing.
    pub disabled: Option<bool>,
}

impl Modifier {
    /// Engine time this modifier runs out.
    ///
    /// `None` when it does not expire, or when either input was unreadable. A negative
    /// `m_flDuration` is the game's way of saying "no expiry"; treating it as seconds
    /// would place the deadline before the creation time.
    pub fn expires_at(&self) -> Option<f32> {
        let duration = self.duration?;
        if duration < 0.0 {
            return None;
        }
        Some(self.creation_time? + duration)
    }

    /// Seconds left before this modifier expires.
    ///
    /// Clamped at zero, so an expired modifier that has not been removed yet reads as
    /// finished rather than as a timer running backwards. `None` when it does not expire.
    pub fn remaining(&self, now: f32) -> Option<f32> {
        Some((self.expires_at()? - now).max(0.0))
    }

    /// Whether the modifier lasts until something removes it.
    pub fn is_permanent(&self) -> bool {
        self.duration.is_some_and(|d| d < 0.0)
    }

    /// Seconds since it was applied.
    pub fn age(&self, now: f32) -> Option<f32> {
        Some((now - self.creation_time?).max(0.0))
    }
}

/// Read every modifier applied to `entity`.
///
/// `None` when the entity has no readable modifier property, which is the ordinary case
/// for entities that never take one.
///
/// Public because it is useful on its own: any entity can be asked, not only the twelve
/// players a snapshot walks. Reading it costs one pointer chase plus one small object read
/// per modifier, so it is deliberately not folded into every snapshot.
pub fn read_modifiers(reader: &Reader, entity: u64) -> Option<Vec<Modifier>> {
    let property = reader.field_ptr(entity, super::BASE_ENTITY, "m_pModifierProp")?;
    let limit = reader.tunables().max_modifiers;
    let addresses = reader.field_vec_ptrs(property, PROPERTY, "m_vecModifiers", limit)?;
    Some(
        addresses
            .into_iter()
            .map(|at| read_one(reader, at))
            .collect(),
    )
}

fn read_one(reader: &Reader, at: u64) -> Modifier {
    let obj = reader.read_object(at, MODIFIER);
    Modifier {
        address: at,
        class: read_class_name(reader, at),
        subclass_id: obj.u32(MODIFIER, "m_nAbilitySubclassID"),
        serial: obj.u32(MODIFIER, "m_nSerialNumber"),
        creation_time: obj.f32(MODIFIER, "m_flCreationTime"),
        duration: obj.f32(MODIFIER, "m_flDuration"),
        last_applied_time: obj.f32(MODIFIER, "m_flLastAppliedTime"),
        stacks: obj.u16(MODIFIER, "m_iStackCount"),
        max_stacks: obj.u16(MODIFIER, "m_iMaxStackCount"),
        ability: obj.u32(MODIFIER, "m_hAbility"),
        caster: obj.u32(MODIFIER, "m_hCaster"),
        team: obj.u8(MODIFIER, "m_iTeam"),
        disabled: obj.bool(MODIFIER, "m_bDisabled"),
    }
}

/// A kind of crowd control.
///
/// Deliberately more than the eight kinds Companion tracks: the game also ships disarms
/// and immobilises, which it does not mention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum CrowdControl {
    /// Cannot act at all.
    Stun,
    /// Cannot cast.
    Silence,
    /// Cannot move, can still act.
    Root,
    /// Movement slowed.
    Slow,
    /// Asleep until woken.
    Sleep,
    /// Thrown upward.
    Knockup,
    /// Put on the floor.
    Knockdown,
    /// Cannot shoot.
    Disarm,
    /// Held in place by an ability rather than a root.
    Immobilize,
}

/// The modifier the Urn carrier holds while carrying it.
///
/// A subclass with its own fields beyond `CBaseModifier`, which is why
/// [`read_urn_carry`] exists rather than everything living on [`Modifier`].
pub const HOLDING_URN: &str = "CCitadel_Modifier_HoldingGoldenIdol";

/// Counts down while a dropped Urn returns to its spawn.
pub const URN_RETURN_TIMER: &str = "CCitadel_Modifier_IdolReturnTimer";

/// Counts down while the Urn is being cashed in.
pub const URN_CASH_IN_TIMER: &str = "CCitadel_Modifier_IdolCashInTimer";

/// What the Urn is worth to whoever is carrying it, and whether they are visible.
///
/// Read off `CCitadel_Modifier_HoldingGoldenIdol`, which carries these beyond the fields
/// every modifier has.
///
/// Not verified against a live carry: the client this was written against sat in the
/// Hideout, which has no Urn. The class and both field offsets are confirmed present in
/// the runtime schema, and the read is the ordinary object read used everywhere else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct UrnCarry {
    /// `m_nGoldValue`, what cashing it in is worth.
    pub gold_value: Option<i32>,
    /// `m_bRevealed`, whether the carrier is shown to the other side.
    pub revealed: Option<bool>,
}

impl Modifier {
    /// Whether this is the Urn-carrying modifier.
    pub fn is_urn_carry(&self) -> bool {
        self.class.as_deref() == Some(HOLDING_URN)
    }

    /// Whether this is one of the Urn's countdowns.
    ///
    /// Both are ordinary timed modifiers, so [`Modifier::remaining`] gives the countdown.
    pub fn is_urn_timer(&self) -> bool {
        matches!(
            self.class.as_deref(),
            Some(URN_RETURN_TIMER | URN_CASH_IN_TIMER)
        )
    }
}

/// Read the Urn-specific fields off a carrying modifier.
///
/// `None` when `modifier` is not [`HOLDING_URN`] - the fields belong to that subclass and
/// reading them off any other modifier would return whatever happens to sit at those
/// offsets.
pub fn read_urn_carry(reader: &Reader, modifier: &Modifier) -> Option<UrnCarry> {
    if !modifier.is_urn_carry() {
        return None;
    }
    let obj = reader.read_object(modifier.address, HOLDING_URN);
    Some(UrnCarry {
        gold_value: obj.i32(HOLDING_URN, "m_nGoldValue"),
        revealed: obj.bool(HOLDING_URN, "m_bRevealed"),
    })
}

/// The modifier a player carries during their own parry window.
pub const PARRY_WINDOW: &str = "CCitadel_Modifier_Parry";

/// The stun applied to whoever got parried.
///
/// Its `caster` is the parrier, so this is both "was parried" and "by whom" in one record.
pub const PARRIED_STUN: &str = "CCitadel_Modifier_ParriedStun";

impl Modifier {
    /// Whether this is the parry window a player is inside while parrying.
    pub fn is_parry_window(&self) -> bool {
        self.class.as_deref() == Some(PARRY_WINDOW)
    }

    /// Whether this is the stun a parried player takes.
    ///
    /// Also classifies as [`CrowdControl::Stun`], which is correct - it is a real stun and
    /// should count toward crowd-control totals like any other. This distinguishes *why*
    /// it was applied.
    pub fn is_parried_stun(&self) -> bool {
        self.class.as_deref() == Some(PARRIED_STUN)
    }

    /// The handle of whoever parried this player, if this is a parry stun.
    ///
    /// A caller resolves it with [`EntitySnapshot::by_handle`](crate::entity::EntitySnapshot::by_handle);
    /// comparing it against the local player's pawn is what answers "did I parry them".
    pub fn parried_by(&self) -> Option<u32> {
        self.is_parried_stun().then_some(self.caster).flatten()
    }
}

/// The modifier a structure carries while it cannot be damaged from out of lane.
///
/// Backdoor protection is not a field on `C_Citadel_Destroyable_Building`, which declares
/// only `m_bDestroyed`, `m_bActive` and `m_bFinal`. The only per-structure signal the
/// client carries is this modifier, so answering "is this walker protected right now"
/// means reading the structure's own modifier list.
///
/// `STeamFOWEntity::m_bBackdoorProtectionActive` also exists, but that is the minimap
/// fog-of-war record rather than the structure, and it covers a team's entities as the
/// minimap sees them.
pub const BACKDOOR_PROTECTION: &str = "CCitadel_Modifier_Backdoor_Protection";

/// Whether any of these modifiers is backdoor protection.
///
/// Pass the result of [`read_modifiers`] for a structure's entity address.
///
/// Not verified against a live structure: the client this was written against sat in the
/// Hideout, which has none. The class name is confirmed present in the runtime schema and
/// the read path is the same one that names a player's modifiers exactly.
pub fn has_backdoor_protection(modifiers: &[Modifier]) -> bool {
    modifiers
        .iter()
        .any(|m| m.class.as_deref() == Some(BACKDOOR_PROTECTION) && m.disabled != Some(true))
}

/// What a modifier class name means, when it is one this table is sure about.
///
/// # Conservative on purpose
///
/// Matching on substrings would be wrong far more often than it is right. Of the 55
/// modifier classes whose name carries a crowd-control word, several are the **opposite**
/// of crowd control or belong to the caster rather than the target:
///
/// | Class | What it actually is |
/// |---|---|
/// | `CCitadel_Modifier_SlowImmunity` | immunity *to* slow |
/// | `CCitadel_Modifier_SilenceProc_Immunity` | immunity to silence |
/// | `CCitadel_Modifier_ZiplineKnockdownImmune` | immunity to knockdown |
/// | `CCitadel_Modifier_UppercutClipSize` | a clip-size buff |
/// | `CCitadel_Modifier_Uppercut_Buff` | a buff on the caster |
/// | `CCitadel_Modifier_Priest_KnockbackBuff` | a buff on the caster |
/// | `CCitadel_Modifier_DisarmProcWatcher` | a watcher, not the debuff |
/// | `CCitadel_Modifier_SilenceProcWatcher` | a watcher |
/// | `CCitadel_Modifier_Silence_Buildup` | accumulating toward a silence, not one yet |
///
/// So this is an exact-name table, and an unlisted class returns `None` rather than a
/// guess. Being incomplete is recoverable; misreporting an immunity as the crowd control
/// it protects against is not.
///
/// Names come from [`Modifier::class`], which reads them off the object's vtable, so they
/// are the runtime classes rather than anything transcribed.
pub fn classify(class: &str) -> Option<CrowdControl> {
    Some(match class {
        // The base classes, which every generic application goes through.
        "CCitadel_Modifier_Stunned"
        | "CCitadel_Modifier_CatapultStun"
        | "CCitadel_Modifier_Delayed_Stun"
        | "CCitadel_Modifier_ParriedStun" => CrowdControl::Stun,
        "CCitadel_Modifier_Silenced"
        | "CCitadel_Modifier_SilenceBomb_Debuff"
        | "CCitadel_Modifier_SilenceProc_Debuff"
        | "CCitadel_Modifier_Cadence_SilenceContraptionsDebuff"
        | "CCitadel_Modifier_Item_AOESilence_Target" => CrowdControl::Silence,
        "CCitadel_Modifier_Root" => CrowdControl::Root,
        "CCitadel_Modifier_Slow"
        | "CCitadel_Modifier_DiminishingSlow"
        | "CCitadel_Modifier_CapacitorSlowDebuff"
        | "CCitadel_Modifier_Chrono_KineticCarbine_Slow"
        | "CCitadel_Modifier_ClimbRopeSlow"
        | "CCitadel_Modifier_IceBeam_Stacking_Slow"
        | "CCitadel_Modifier_PulseGrenade_TimeSlow"
        | "CCitadel_Modifier_RampSlow"
        | "CCitadel_Modifier_ThrownShiv_Slow_Debuff" => CrowdControl::Slow,
        "CCitadel_Modifier_Sleep"
        | "CCitadel_Modifier_Cadence_Sleeping"
        | "CCitadel_Modifier_SleepBomb_Asleep"
        | "CCitadel_Modifier_SleepDagger_Asleep" => CrowdControl::Sleep,
        "CCitadel_Modifier_TossUp" => CrowdControl::Knockup,
        "CCitadel_Modifier_Knockdown" => CrowdControl::Knockdown,
        "CCitadel_Modifier_Disarmed" => CrowdControl::Disarm,
        "CCitadel_Modifier_Bookworm_Immobilize"
        | "CCitadel_Modifier_Priest_Immobilize"
        | "CCitadel_Modifier_Trapper_Immobilize" => CrowdControl::Immobilize,
        _ => return None,
    })
}

impl Modifier {
    /// What crowd control this modifier applies, if this table knows.
    ///
    /// `None` covers both "not crowd control" and "not recognised"; see [`classify`] for
    /// why those are deliberately not distinguished.
    pub fn crowd_control(&self) -> Option<CrowdControl> {
        classify(self.class.as_deref()?)
    }
}

/// Offsets inside MSVC's RTTI structures, which are fixed by the ABI rather than by the
/// game, so they do not belong in `Tunables`.
mod rtti {
    /// The Complete Object Locator pointer sits one pointer *before* the vtable.
    pub const COL_BACK: u64 = 8;
    /// Signature word: 1 on 64-bit, where the descriptor addresses are module-relative.
    pub const COL_SIGNATURE: u64 = 0x00;
    /// Module-relative address of the type descriptor.
    pub const COL_TYPE_DESCRIPTOR: u64 = 0x0c;
    /// Mangled name, inside the type descriptor.
    pub const TYPE_DESCRIPTOR_NAME: u64 = 0x10;
    /// Longest mangled name worth reading. The longest real one is well under this.
    pub const MAX_NAME: usize = 256;
    /// Value `COL_SIGNATURE` must hold for the module-relative reading to be right.
    pub const SIGNATURE_64: u32 = 1;
}

/// Recover a C++ object's class name from its vtable, via MSVC RTTI.
///
/// Every step is checked, because this walks pointers the schema knows nothing about: the
/// vtable must land inside `client.dll`, the locator's signature must say 64-bit, and the
/// mangled name must have the shape MSVC emits. A failure at any point yields `None`
/// rather than a name that might be rubbish.
fn read_class_name(reader: &Reader, object: u64) -> Option<String> {
    let mem = reader.memory();
    let base = reader.client_base();
    let end = base + reader.client_size() as u64;
    let in_module = |p: u64| p >= base && p < end;

    let vtable = mem.read_ptr(object).ok()?;
    if !in_module(vtable) {
        return None;
    }
    let locator = mem.read_ptr(vtable.checked_sub(rtti::COL_BACK)?).ok()?;
    if !in_module(locator) {
        return None;
    }
    if mem.read_u32(locator + rtti::COL_SIGNATURE).ok()? != rtti::SIGNATURE_64 {
        return None;
    }
    let descriptor = base + u64::from(mem.read_u32(locator + rtti::COL_TYPE_DESCRIPTOR).ok()?);
    if !in_module(descriptor) {
        return None;
    }
    let mangled = mem
        .read_cstr(descriptor + rtti::TYPE_DESCRIPTOR_NAME, rtti::MAX_NAME)
        .ok()?;
    demangle(&mangled)
}

/// Strip MSVC's decoration from a type descriptor name.
///
/// `.?AVCCitadel_Modifier_Stunned@@` is a class, `.?AU...@@` a struct. A name carrying
/// further `@`-separated segments is namespaced or templated, which nothing here is, so it
/// is refused rather than half-parsed.
fn demangle(mangled: &str) -> Option<String> {
    let body = mangled
        .strip_prefix(".?AV")
        .or_else(|| mangled.strip_prefix(".?AU"))?
        .strip_suffix("@@")?;
    if body.is_empty() || body.contains('@') {
        return None;
    }
    Some(body.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Urn's fields live on a subclass, so they are refused on any other modifier.
    ///
    /// `m_nGoldValue` sits at 0x4c4 of `CCitadel_Modifier_HoldingGoldenIdol`. Read off a
    /// 192-byte modifier that is not that subclass, the offset is past the object
    /// entirely; read off a larger one it returns whatever happens to be there. Neither
    /// failure announces itself, so the guard is on the class rather than on the read.
    #[test]
    fn urn_fields_are_only_read_from_the_carrying_modifier() {
        let carry = Modifier {
            class: Some(HOLDING_URN.into()),
            ..Default::default()
        };
        assert!(carry.is_urn_carry());
        assert!(!carry.is_urn_timer());

        for other in [
            "CCitadel_Modifier_Stunned",
            "CCitadel_Modifier_BarrierTracker",
            URN_RETURN_TIMER,
        ] {
            let m = Modifier {
                class: Some(other.into()),
                ..Default::default()
            };
            assert!(!m.is_urn_carry(), "{other} is not the carry modifier");
        }
        assert!(!Modifier::default().is_urn_carry());
    }

    /// Both Urn countdowns are ordinary timed modifiers, so `remaining` reads them.
    #[test]
    fn the_urn_countdowns_are_timed_modifiers_like_any_other() {
        for class in [URN_RETURN_TIMER, URN_CASH_IN_TIMER] {
            let m = Modifier {
                class: Some(class.into()),
                creation_time: Some(100.0),
                duration: Some(12.5),
                ..Default::default()
            };
            assert!(m.is_urn_timer(), "{class}");
            assert!(!m.is_urn_carry());
            assert_eq!(m.expires_at(), Some(112.5));
            assert_eq!(m.remaining(105.0), Some(7.5));
        }
        assert!(!Modifier::default().is_urn_timer());
    }

    /// A parry is two modifiers on two different players, not one event.
    ///
    /// The parrier carries the window; whoever was parried carries the stun, whose caster
    /// is the parrier. Reading only one of them answers half the question.
    #[test]
    fn a_parry_is_visible_from_both_sides() {
        const PARRIER_PAWN: u32 = 0x2a;

        let window = Modifier {
            class: Some(PARRY_WINDOW.into()),
            ..Default::default()
        };
        assert!(window.is_parry_window());
        assert!(!window.is_parried_stun());
        assert_eq!(window.crowd_control(), None);
        assert_eq!(window.parried_by(), None);

        let stunned = Modifier {
            class: Some(PARRIED_STUN.into()),
            caster: Some(PARRIER_PAWN),
            ..Default::default()
        };
        assert!(stunned.is_parried_stun());
        assert!(!stunned.is_parry_window());
        assert_eq!(stunned.parried_by(), Some(PARRIER_PAWN));
        assert_eq!(stunned.crowd_control(), Some(CrowdControl::Stun));
    }

    /// An ordinary stun was not a parry, and must not claim a parrier.
    #[test]
    fn an_ordinary_stun_reports_no_parrier() {
        let stun = Modifier {
            class: Some("CCitadel_Modifier_Stunned".into()),
            caster: Some(0x2a),
            ..Default::default()
        };
        assert_eq!(stun.crowd_control(), Some(CrowdControl::Stun));
        assert!(!stun.is_parried_stun());
        assert_eq!(
            stun.parried_by(),
            None,
            "caster is set but it was not a parry"
        );
    }

    /// Backdoor protection is a modifier, not a field on the building.
    ///
    /// `C_Citadel_Destroyable_Building` declares `m_bDestroyed`, `m_bActive` and
    /// `m_bFinal` and nothing about backdoor state, so a reader looking for a field there
    /// would find nothing and conclude wrongly that the client is not told.
    #[test]
    fn backdoor_protection_is_recognised_from_the_structures_modifier_list() {
        let protection = Modifier {
            class: Some(BACKDOOR_PROTECTION.into()),
            ..Default::default()
        };
        assert!(has_backdoor_protection(std::slice::from_ref(&protection)));

        let disabled = Modifier {
            disabled: Some(true),
            ..protection.clone()
        };
        assert!(!has_backdoor_protection(&[disabled]));

        assert_eq!(protection.crowd_control(), None);
        assert!(!has_backdoor_protection(&[]));
        assert!(!has_backdoor_protection(&[Modifier {
            class: Some("CCitadel_Modifier_BarrierTracker".into()),
            ..Default::default()
        }]));
    }

    /// Every class this table claims is crowd control, by kind.
    #[test]
    fn the_table_classifies_the_kinds_the_game_ships() {
        for (class, want) in [
            ("CCitadel_Modifier_Stunned", CrowdControl::Stun),
            ("CCitadel_Modifier_ParriedStun", CrowdControl::Stun),
            ("CCitadel_Modifier_Silenced", CrowdControl::Silence),
            ("CCitadel_Modifier_Root", CrowdControl::Root),
            ("CCitadel_Modifier_DiminishingSlow", CrowdControl::Slow),
            ("CCitadel_Modifier_SleepDagger_Asleep", CrowdControl::Sleep),
            ("CCitadel_Modifier_TossUp", CrowdControl::Knockup),
            ("CCitadel_Modifier_Knockdown", CrowdControl::Knockdown),
            ("CCitadel_Modifier_Disarmed", CrowdControl::Disarm),
            (
                "CCitadel_Modifier_Trapper_Immobilize",
                CrowdControl::Immobilize,
            ),
        ] {
            assert_eq!(classify(class), Some(want), "{class}");
        }
    }

    /// Every `CrowdControl` variant is reachable from some class name.
    ///
    /// [`classify`] is an exact-name table, so a variant with no arm can never be produced:
    /// the modifier is simply not crowd control as far as this crate is concerned, and
    /// nothing anywhere reports a gap. `the_table_classifies_the_kinds_the_game_ships`
    /// checks the pairs it lists and cannot notice a variant nobody listed.
    ///
    /// Checked by reading this file rather than by keeping a second list, because a second
    /// list is the thing that goes stale. It compares the enum's declaration against the
    /// arms of `classify`, so both sides come from the source and neither can drift alone.
    ///
    /// All nine variants have an arm today; this exists so the tenth cannot arrive without
    /// one.
    #[test]
    fn every_crowd_control_kind_is_reachable_from_some_class() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("snapshot")
                .join("modifier.rs"),
        )
        .expect("read this file");

        let declared: Vec<String> = src
            .split("pub enum CrowdControl {")
            .nth(1)
            .expect("the enum declaration")
            .split("\n}")
            .next()
            .expect("the enum body")
            .lines()
            .map(str::trim)
            .filter(|l| l.ends_with(',') && !l.starts_with("///") && !l.starts_with('#'))
            .map(|l| l.trim_end_matches(',').to_owned())
            .collect();
        assert_eq!(declared.len(), 9, "found variants {declared:?}");

        let body = src
            .split("pub fn classify(")
            .nth(1)
            .expect("the classifier")
            .split("\n}")
            .next()
            .expect("the classifier body");

        let unreachable: Vec<&String> = declared
            .iter()
            .filter(|v| !body.contains(&format!("CrowdControl::{v}")))
            .collect();
        assert!(
            unreachable.is_empty(),
            "crowd-control kinds no class maps to: {unreachable:?}"
        );
    }

    /// The names that carry a crowd-control word and are not crowd control.
    ///
    /// This is what the table exists to get right. A substring match would report every
    /// one of these, and reporting an *immunity* as the thing it protects against is a
    /// worse answer than reporting nothing.
    #[test]
    fn an_immunity_or_a_caster_buff_is_not_crowd_control() {
        for class in [
            "CCitadel_Modifier_SlowImmunity",
            "CCitadel_Modifier_SilenceProc_Immunity",
            "CCitadel_Modifier_ZiplineKnockdownImmune",
            "CCitadel_Modifier_UppercutClipSize",
            "CCitadel_Modifier_Uppercut_Buff",
            "CCitadel_Modifier_Priest_KnockbackBuff",
            "CCitadel_Modifier_DisarmProcWatcher",
            "CCitadel_Modifier_SilenceProcWatcher",
            "CCitadel_Modifier_Silence_Buildup",
            "CCitadel_Modifier_InHideoutMap",
            "CCitadel_Modifier_HeroUpgradeBonuses",
        ] {
            assert_eq!(classify(class), None, "{class} must not classify");
        }
    }

    /// A modifier with no readable class cannot be classified, rather than defaulting.
    #[test]
    fn a_modifier_with_no_class_has_no_crowd_control() {
        assert_eq!(Modifier::default().crowd_control(), None);
        let stun = Modifier {
            class: Some("CCitadel_Modifier_Stunned".into()),
            ..Default::default()
        };
        assert_eq!(stun.crowd_control(), Some(CrowdControl::Stun));
    }

    /// The exact strings a live client returned for a Hideout player's modifiers.
    #[test]
    fn a_class_type_descriptor_demangles_to_its_class_name() {
        for (mangled, want) in [
            (
                ".?AVCCitadel_Modifier_InHideoutMap@@",
                "CCitadel_Modifier_InHideoutMap",
            ),
            (
                ".?AVCCitadel_Modifier_HeroUpgradeBonuses@@",
                "CCitadel_Modifier_HeroUpgradeBonuses",
            ),
            (
                ".?AVCCitadel_Stamina_Regen_Jump_Reduction@@",
                "CCitadel_Stamina_Regen_Jump_Reduction",
            ),
            (".?AUSomeStruct@@", "SomeStruct"),
        ] {
            assert_eq!(demangle(mangled).as_deref(), Some(want), "{mangled}");
        }
    }

    /// Anything that is not the shape MSVC emits for a plain global class is refused.
    ///
    /// A half-parsed name is worse than no name: it would be reported with the same
    /// confidence as a real one and quietly key a lookup against nothing.
    #[test]
    fn a_name_that_is_not_a_plain_class_is_refused_rather_than_half_parsed() {
        for bad in [
            "",
            "CCitadel_Modifier_Stunned",     // undecorated
            ".?AVCCitadel_Modifier_Stunned", // no terminator
            "CCitadel_Modifier_Stunned@@",   // no prefix
            ".?AV@@",                        // empty body
            ".?AVNested@Namespace@@",        // namespaced
            ".?AWEnumType@@",                // an enum, not a class
        ] {
            assert_eq!(demangle(bad), None, "{bad:?} should not demangle");
        }
    }

    /// A duration of -1 is the game saying "until something removes it".
    ///
    /// Observed on every one of the seven to nine modifiers a Hideout player pawn carries.
    /// Read as seconds it places the deadline before the creation time, so a permanent
    /// modifier would report as expired on the tick it was applied.
    #[test]
    fn a_negative_duration_is_no_expiry_rather_than_one_in_the_past() {
        let m = Modifier {
            creation_time: Some(2.359_375),
            duration: Some(-1.0),
            ..Default::default()
        };
        assert!(m.is_permanent());
        assert_eq!(m.expires_at(), None);
        assert_eq!(m.remaining(500.0), None);
        assert_eq!(m.age(12.359_375), Some(10.0));
    }

    #[test]
    fn a_timed_modifier_counts_down_and_stops_at_zero() {
        let m = Modifier {
            creation_time: Some(100.0),
            duration: Some(6.0),
            ..Default::default()
        };
        assert!(!m.is_permanent());
        assert_eq!(m.expires_at(), Some(106.0));
        assert_eq!(m.remaining(100.0), Some(6.0));
        assert_eq!(m.remaining(104.5), Some(1.5));
        assert_eq!(m.remaining(120.0), Some(0.0));
    }

    /// An unreadable field must not invent a deadline from the one that did read.
    #[test]
    fn a_half_read_modifier_reports_no_expiry_at_all() {
        let no_duration = Modifier {
            creation_time: Some(100.0),
            ..Default::default()
        };
        assert_eq!(no_duration.expires_at(), None);
        assert!(!no_duration.is_permanent());

        let no_creation = Modifier {
            duration: Some(6.0),
            ..Default::default()
        };
        assert_eq!(no_creation.expires_at(), None);
        assert_eq!(no_creation.remaining(100.0), None);
    }
}
