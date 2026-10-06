//! Player rows: the scoreboard, ability upgrades, and camera identification.

use std::collections::HashMap;

use deadlock_core::{ConnectionState, HeroId, Team};

use crate::entity::EntitySnapshot;
use crate::reader::{Object, Reader};
use crate::snapshot::BASE_ENTITY;
use deadlock_memory::mem::MemoryReader;

pub(crate) const CONTROLLER: &str = "CCitadelPlayerController";
/// Declares `m_hPawn`; `CCitadelPlayerController` inherits it.
pub(crate) const BASE_CONTROLLER: &str = "CBasePlayerController";
pub(crate) const PDG: &str = "PlayerDataGlobal_t";
pub(crate) const PAWN: &str = "C_CitadelPlayerPawn";
/// The pawn a spectating client drives.
pub(crate) const OBSERVER_PAWN: &str = "C_CitadelObserverPawn";

/// Bytes read for `m_iszPlayerName`. Sized from the gap to the next schema field
/// (`m_iszPlayerName` @ 0x6f0, `m_steamID` @ 0x778); `read_cstr` stops at the NUL, so
/// this is only an upper bound.
const MAX_NAME: usize = 128;

/// One ability's upgrade state, decoded from `AbilityUpgradeState_t`.
///
/// `m_nUpgradeInfo` packs a tier bitmask into its high 16 bits: bit 0 means the ability
/// is unlocked at all, and each further bit is one point spent on it. Verified against a
/// live match where a Yamato had Power Slash at 1 point (`0x3_0001`), Flying Slash at 2
/// (`0x7_0001`), Crimson Slash at 0 (`0x1_0001`) and an unlearned ultimate (`0x1`).
///
/// The low 16 bits read `0x0001` on every sample seen so far; their meaning is unknown,
/// so [`AbilityUpgrade::raw`] keeps the whole value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AbilityUpgrade {
    /// The ability's item id; resolve a name with `deadlock-data`.
    pub item_id: u32,
    /// Raw `m_nUpgradeInfo`.
    pub raw: u32,
    /// Whether the ability is learned at all. False for an unlearned ultimate.
    pub unlocked: bool,
    /// Points spent upgrading it, beyond simply having it. `0..=3`.
    pub points: u32,
}

impl AbilityUpgrade {
    /// Decode one `(m_ItemID, m_nUpgradeInfo)` pair.
    pub fn from_raw(item_id: u32, raw: u32) -> Self {
        let mask = raw >> 16;
        AbilityUpgrade {
            item_id,
            raw,
            unlocked: mask != 0,
            // Bit 0 is "unlocked", so the points are the remaining set bits.
            points: mask.count_ones().saturating_sub(1),
        }
    }

    /// The tier bitmask, i.e. the high 16 bits of `m_nUpgradeInfo`.
    pub fn mask(&self) -> u32 {
        self.raw >> 16
    }
}

/// Element type of `PlayerDataGlobal_t::m_vecStatViewerModifierValues`.
const STAT_VIEWER: &str = "StatViewerModifierValues_t";

/// Cap on stat-attribution entries read per player, so a corrupt count cannot cause a
/// wild allocation.
///
/// Not derivable from `EModifierValue`'s 225 real stats: more than one modifier can
/// contribute to the same stat, so the list is not bounded by the enum. This is a ceiling
/// on the read rather than a claim about the game â€” at the ~0.7 us a cross-process read
/// costs, it holds the whole twelve-player pass under about 2 ms even if every count is
/// nonsense.
///
/// Belongs beside `max_items` and `max_ability_upgrades` in [`crate::Tunables`], where it
/// would also be overridable after a patch.
const MAX_STAT_CONTRIBUTIONS: usize = 256;

/// Cap on entries read from each of the four bonus-counter vectors, per player.
///
/// Same reasoning and same intended home as [`MAX_STAT_CONTRIBUTIONS`]. Matched to
/// `DEFAULT_MAX_ITEMS`, since the counters are driven by the item and ability roster.
const MAX_BONUS_COUNTERS: usize = 64;

/// Largest member window [`read_stat_element`] will read in one call, in bytes.
///
/// Bounds a stack buffer, so a schema reporting absurd member offsets fails the read
/// rather than sizing an array from a number the game supplied.
const MAX_STAT_SPAN: usize = 64;

/// Where `StatViewerModifierValues_t`'s three members sit inside one element.
///
/// The runtime schema declares all four numbers, so this is resolved per read and only
/// falls back to [`StatLayout::DECLARED`] when the schema walk failed. That is the same
/// contract every other field here has, and it means a patch that grows the element
/// corrects itself rather than reading the next element's prologue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StatLayout {
    /// `sizeof(StatViewerModifierValues_t)`, which is the walk stride.
    stride: u64,
    /// Offset of `m_SourceModifierID`.
    source_modifier_id: u64,
    /// Offset of `m_eValType`.
    val_type: u64,
    /// Offset of `m_flValue`.
    value: u64,
}

impl StatLayout {
    /// What build 6683's schema declares: 64-byte elements, members at 0x30, 0x34, 0x38.
    ///
    /// The 0x30 prologue before the first member is not padding a reader may skip. The
    /// cross-check licensing these numbers is `DEFAULT_ABILITY_UPGRADE_LAYOUT`, which was
    /// probed out of the live process independently of the schema and lands on exactly
    /// what the schema declares for its own element type.
    const DECLARED: StatLayout = StatLayout {
        stride: 0x40,
        source_modifier_id: 0x30,
        val_type: 0x34,
        value: 0x38,
    };

    /// Prefer the running game's own numbers; fall back to what was measured.
    fn resolve(reader: &Reader) -> Self {
        let off = |field: &str| reader.offset_of(STAT_VIEWER, field).map(u64::from);
        match (
            reader.class_size(STAT_VIEWER),
            off("m_SourceModifierID"),
            off("m_eValType"),
            off("m_flValue"),
        ) {
            (Some(stride), Some(source_modifier_id), Some(val_type), Some(value)) => StatLayout {
                stride: u64::from(stride),
                source_modifier_id,
                val_type,
                value,
            },
            _ => Self::DECLARED,
        }
    }

    /// The `(offset, length)` window covering all three members.
    ///
    /// One element then costs one read instead of three. Measured against the live client:
    /// a 4-byte read and a 12-byte read both cost about 0.7 us, because the price is the
    /// crossing, not the width. Over twelve players that is the difference between roughly
    /// 2.5 ms and 0.8 ms per tick on a hundred-entry list.
    ///
    /// `None` when the members do not all fall inside one element, which is a layout this
    /// code has no business reading.
    fn span(&self) -> Option<(u64, usize)> {
        let lo = self.source_modifier_id.min(self.val_type).min(self.value);
        let hi = self
            .source_modifier_id
            .max(self.val_type)
            .max(self.value)
            .checked_add(4)?;
        let len = usize::try_from(hi.checked_sub(lo)?).ok()?;
        (hi <= self.stride && len <= MAX_STAT_SPAN).then_some((lo, len))
    }
}

/// Decode one `StatViewerModifierValues_t` at `at`, in a single read.
///
/// `None` when the element is unreadable or `layout` does not describe one, so a caller
/// collecting into `Option<Vec<_>>` gets a whole list or nothing. See
/// [`crate::Reader::field_vec_u32`] for why a short list would be worse.
fn read_stat_element(
    mem: &dyn MemoryReader,
    at: u64,
    layout: StatLayout,
) -> Option<(u32, u32, f32)> {
    let (lo, len) = layout.span()?;
    let mut buf = [0u8; MAX_STAT_SPAN];
    let buf = buf.get_mut(..len)?;
    // Wrapping: `at` came from the game, and a base near the top of the address space
    // would otherwise panic in a debug build before the read got to report it.
    mem.read_into(at.wrapping_add(lo), buf).ok()?;
    let word = |off: u64| -> Option<[u8; 4]> {
        let start = usize::try_from(off.checked_sub(lo)?).ok()?;
        buf.get(start..start.checked_add(4)?)?.try_into().ok()
    };
    Some((
        u32::from_le_bytes(word(layout.source_modifier_id)?),
        u32::from_le_bytes(word(layout.val_type)?),
        f32::from_le_bytes(word(layout.value)?),
    ))
}

/// One stat's contribution from one source, decoded from `StatViewerModifierValues_t`.
///
/// This is the decomposition of totals the crate already reads: a Tech Power number on
/// `CCitadel_Modifier_HeroUpgradeBonuses` says what a player has, and these rows say which
/// item, ability or buff each part of it came from.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct StatContribution {
    /// `m_SourceModifierID`: which modifier produced this contribution.
    ///
    /// An id into the game's modifier table, not an entity handle and not an item id.
    pub source_modifier_id: u32,
    /// Raw `m_eValType`: which stat is being contributed to.
    ///
    /// An `EModifierValue`, which has 227 enumerators on build 6683. Two of them are not
    /// stats: 225 is the `MODIFIER_VALUE_COUNT` sentinel and 255 is
    /// `MODIFIER_VALUE_INVALID`, the absent marker.
    pub val_type_raw: u32,
    /// Enumerator name straight from the runtime schema, e.g.
    /// `"MODIFIER_VALUE_TECH_POWER"`.
    ///
    /// Nothing about this mapping is baked into the crate, so it cannot go stale: a patch
    /// that adds or renumbers a stat is reported correctly the moment the game ships it.
    /// `None` only when the schema walk failed, in which case
    /// [`StatContribution::val_type_raw`] still stands on its own.
    pub val_type: Option<String>,
    /// `m_flValue`: the contribution itself, in whatever unit the stat uses.
    pub value: f32,
}

/// The four `PlayerDataGlobal_t` bonus-counter vectors: stacking item and hero counters.
///
/// Each is read as a `CUtlVector<u32>`. The four sit `0x18` apart, so they are ordinary
/// vectors rather than the strided kind `m_vecStatViewerModifierValues` needs.
///
/// **The pairing is inferred, not observed.** The names divide cleanly into two halves â€”
/// `m_vecBonusCounterAbilities` with `m_vecBonusCounterValues`, and
/// `m_vecBonusCounterModifiers` with `m_vecModifierBonusCounterValues`, whose spelling
/// mirrors its partner â€” and [`BonusCounters::ability_counters`] and
/// [`BonusCounters::modifier_counters`] zip them on that reading. It has not been checked
/// against a populated list: every one of the four read empty on the only client state
/// available while this was written, which was the Hideout rather than a live match. The
/// four raw lists are public so a consumer that disagrees can pair them differently, and
/// [`BonusCounters::pairing_holds`] reports whether the lengths even permit the zip.
///
/// The element interpretation is inferred on the same footing: the schema names the
/// vectors but not their element type, and 4-byte ids and counts are what the names
/// suggest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct BonusCounters {
    /// `m_vecBonusCounterAbilities`.
    pub abilities: Vec<u32>,
    /// `m_vecBonusCounterValues`.
    pub values: Vec<u32>,
    /// `m_vecBonusCounterModifiers`.
    pub modifiers: Vec<u32>,
    /// `m_vecModifierBonusCounterValues`.
    pub modifier_values: Vec<u32>,
}

impl BonusCounters {
    /// Whether all four lists are empty, which is the normal state before anything with a
    /// counter is bought.
    pub fn is_empty(&self) -> bool {
        self.abilities.is_empty()
            && self.values.is_empty()
            && self.modifiers.is_empty()
            && self.modifier_values.is_empty()
    }

    /// Whether both halves are length-matched, i.e. whether the inferred pairing is even
    /// arithmetically possible on this tick.
    ///
    /// False is a signal that the reading documented on [`BonusCounters`] is wrong, or
    /// that the lists were read mid-update. Neither iterator below reports it, because
    /// both stop at the shorter side.
    pub fn pairing_holds(&self) -> bool {
        self.abilities.len() == self.values.len()
            && self.modifiers.len() == self.modifier_values.len()
    }

    /// `(ability, value)` pairs, on the inferred pairing. See [`BonusCounters`].
    pub fn ability_counters(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.abilities
            .iter()
            .copied()
            .zip(self.values.iter().copied())
    }

    /// `(modifier, value)` pairs, on the inferred pairing. See [`BonusCounters`].
    pub fn modifier_counters(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.modifiers
            .iter()
            .copied()
            .zip(self.modifier_values.iter().copied())
    }
}

/// One player's scoreboard row.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PlayerRow {
    /// Address of the `CCitadelPlayerController`.
    pub controller: u64,
    /// Lobby slot, `1..=12`. `None` for spectators, whose slot field is not meaningful
    /// and collides with a real player's.
    pub slot: Option<u32>,
    /// Team number. See [`Team::AMBER`], [`Team::SAPPHIRE`], [`Team::SPECTATOR`].
    pub team: Option<Team>,
    /// Team name as the game reports it, e.g. `"Amber"`.
    pub team_name: Option<String>,
    /// True for observers, including your own controller while spectating. These are not
    /// scoreboard entries.
    pub is_spectator: bool,
    /// True for the player the client's camera is currently following.
    pub is_observed: bool,
    /// Hero id.
    pub hero_id: Option<HeroId>,
    /// Steam64 id.
    pub steam_id: Option<u64>,
    /// A bot: named `Bot<N>` by the game and carrying no Steam id.
    ///
    /// Bots read a Steam id of `0` rather than an absent field, so this checks for either.
    /// A human who is literally named `Bot7` still has an id and is not a bot.
    pub is_bot: bool,
    /// Steam persona name, from `m_iszPlayerName`.
    ///
    /// `None` if the field is unreadable or empty. Bots and not-yet-connected slots can
    /// legitimately have no name.
    pub name: Option<String>,
    /// Connection state, from `m_iConnected`.
    ///
    /// See [`PlayerRow::is_connected`] and [`PlayerRow::has_abandoned`] rather than
    /// comparing this directly.
    pub connected: Option<ConnectionState>,
    /// Whether this controller is the local player.
    pub is_local: Option<bool>,
    /// Hero level as the game's UI shows it.
    ///
    /// This is [`PlayerRow::level_raw`] minus one: `m_iLevel` reads one higher than the
    /// displayed level, verified against a spectated match at two different levels. The
    /// behaviour at level 1 has not been observed, so the subtraction saturates.
    pub level: Option<u32>,
    /// Raw `PlayerDataGlobal_t::m_iLevel`, unadjusted.
    pub level_raw: Option<u32>,
    /// Net worth in souls.
    pub net_worth: Option<u32>,
    /// Kills.
    pub kills: Option<u32>,
    /// Deaths.
    pub deaths: Option<u32>,
    /// Assists.
    pub assists: Option<u32>,
    /// Last hits.
    pub last_hits: Option<u32>,
    /// Denies.
    pub denies: Option<u32>,
    /// Hero damage dealt.
    pub hero_damage: Option<u32>,
    /// Objective damage dealt.
    pub objective_damage: Option<u32>,
    /// Healing done.
    pub healing: Option<u32>,
    /// Address of the player's pawn, when one is live.
    pub pawn: Option<u64>,
    /// The pawn's entity handle, when one is live.
    ///
    /// An address identifies the object; a handle is what the game's own fields point
    /// with. `CBaseModifier::m_hCaster` and `m_hAbility` are handles, so without this a
    /// consumer holding a modifier cannot say which player applied it - the two
    /// identifiers never meet. Resolve either with
    /// [`EntitySnapshot::by_handle`](crate::entity::EntitySnapshot::by_handle), or compare
    /// this directly.
    ///
    /// That comparison is what makes a self-buff distinguishable from an enemy's work
    /// without inferring anything: `modifier.caster == row.pawn_handle` is exact, where
    /// reasoning from [`Modifier::team`](crate::snapshot::Modifier::team) is a step
    /// removed.
    pub pawn_handle: Option<u32>,
    /// Current health.
    pub health: Option<i32>,
    /// Maximum health.
    pub max_health: Option<i32>,
    /// World position.
    ///
    /// Behind the `positions` feature: unlike the rest of this struct, live coordinates
    /// could confer an unfair advantage.
    #[cfg(feature = "positions")]
    pub position: Option<[f32; 3]>,
    /// Every modifier applied to this player's pawn.
    ///
    /// Behind the `modifiers` feature, on cost rather than on principle: this is the one
    /// read that dominates a tick instead of adding to it. See the feature's note in
    /// `Cargo.toml` for the measurement.
    ///
    /// `None` when the pawn is not live or its modifier property could not be read, which
    /// is distinct from an empty list.
    #[cfg(feature = "modifiers")]
    pub modifiers: Option<Vec<super::Modifier>>,

    // Extended stats.
    /// `m_iAPNetWorth`, ability points earned.
    pub ability_points: Option<u32>,
    /// Souls from creeps, total.
    pub creep_souls: Option<u32>,
    /// Souls from secured orbs.
    pub secured_souls: Option<u32>,
    /// Souls denied to the enemy.
    pub denied_souls: Option<u32>,
    /// Souls from neutral camps.
    pub neutral_souls: Option<u32>,
    /// Expected souls at this point, the game's own farm benchmark.
    pub farm_baseline: Option<u32>,
    /// Current kill streak.
    pub kill_streak: Option<u32>,
    /// Self-healing done.
    pub self_healing: Option<u32>,
    /// Whether the player is alive, from `m_bAlive` rather than inferred from health.
    pub is_alive: Option<bool>,
    /// Engine time at which the player respawns.
    pub respawn_time: Option<f32>,
    /// Whether the ultimate has been unlocked.
    pub ultimate_trained: Option<bool>,
    /// Ultimate cooldown start, in engine time.
    pub ultimate_cooldown_start: Option<f32>,
    /// Ultimate cooldown end, in engine time.
    pub ultimate_cooldown_end: Option<f32>,
    /// Carrying the Rejuvenator (the golden statue buff).
    pub has_rejuvenator: Option<bool>,
    /// Carrying a Rebirth effect.
    pub has_rebirth: Option<bool>,
    /// The game has flagged this account as a cheater.
    pub flagged_as_cheater: Option<bool>,
    /// The player abandoned the match.
    pub abandoned: Option<bool>,
    /// `m_unPackedRank`, the player's ranked badge, packed.
    pub packed_rank: Option<u32>,
    /// Item ids bought, in purchase order (`m_vecUpgrades`).
    ///
    /// Ids only; resolve names with `deadlock-data`. `None` means the list could not be
    /// read in full this tick, which is deliberately distinct from an empty list: a
    /// consumer diffing ticks must not read a failed read as items being sold.
    pub items: Option<Vec<u32>>,
    /// Per-ability upgrade state from `m_vecAbilityUpgradeState`, in slot order.
    ///
    /// `None` on an incomplete read, as with [`PlayerRow::items`].
    pub ability_upgrades: Option<Vec<AbilityUpgrade>>,
    /// Per-stat attribution from `m_vecStatViewerModifierValues`: what each modifier
    /// contributes to which stat.
    ///
    /// This is the game's own stat viewer, decomposed. `None` on an incomplete read, as
    /// with [`PlayerRow::items`].
    pub stat_attribution: Option<Vec<StatContribution>>,
    /// Stacking item and hero counters from the four `m_vecBonusCounter*` vectors.
    ///
    /// `None` unless all four read completely, so a consumer never sees three lists that
    /// belong to one tick beside a fourth that does not. See [`BonusCounters`] for what is
    /// inferred about them.
    pub bonus_counters: Option<BonusCounters>,
}

impl PlayerRow {
    /// Total ability points visibly spent on upgrades.
    ///
    /// Unlocking an ultimate also costs a point but is not counted here, so this can be
    /// lower than [`PlayerRow::ability_points`], which is the total earned.
    pub fn spent_ability_points(&self) -> u32 {
        self.ability_upgrades
            .iter()
            .flatten()
            .map(|a| a.points)
            .sum()
    }

    /// Whether the player currently has a live pawn with positive health.
    pub fn alive(&self) -> bool {
        self.health.unwrap_or(0) > 0
    }

    /// Whether this row belongs to one of the two playing sides.
    pub fn is_playing(&self) -> bool {
        self.team.map(|t| t.is_playing()).unwrap_or(false)
    }

    /// Whether the player is present in the match, mid-reconnect included.
    ///
    /// `true` when the state is unreadable: a row that exists is far more likely to be a
    /// live player than a phantom, and hiding real players is the worse failure.
    pub fn is_connected(&self) -> bool {
        self.connected
            .map(deadlock_core::ConnectionState::is_in_match)
            .unwrap_or(true)
    }

    /// Whether the player has left the match.
    ///
    /// Distinct from [`PlayerRow::alive`], which is only about the current respawn
    /// timer.
    pub fn has_abandoned(&self) -> bool {
        self.connected
            .map(deadlock_core::ConnectionState::is_gone)
            .unwrap_or(false)
    }

    /// The best display label available: Steam name, else the slot. Never empty.
    ///
    /// The fallbacks (`"Slot 5"`, `"Unknown"`) are English, and are a convenience for the
    /// common case of putting a row on a screen. [`PlayerRow::name`] is the raw field and
    /// [`PlayerRow::slot`] the raw number; a consumer that needs its own wording should
    /// build from those rather than pattern-matching on this.
    pub fn display_name(&self) -> String {
        if let Some(n) = self.name.as_deref().filter(|n| !n.is_empty()) {
            return n.to_string();
        }
        match self.slot {
            Some(s) => format!("Slot {s}"),
            None => "Unknown".to_string(),
        }
    }
}

/// Whether a controller is one of the game's bots.
///
/// The game names them `Bot0`, `Bot1`, ... and never gives them a Steam id.
pub(crate) fn looks_like_bot(name: Option<&str>, steam_id: Option<u64>) -> bool {
    let numbered = name
        .and_then(|n| n.strip_prefix("Bot"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    numbered && steam_id.unwrap_or(0) == 0
}

/// Address of the controller whose pawn the camera is following, if any.
///
/// The chain is `local controller -> m_hPawn -> m_pObserverServices ->
/// m_hObserverTarget -> pawn -> m_hController`.
///
/// It has to start from the local controller. Our own pawn is not the only one with
/// observer services attached: every dead player is watching someone, and on a spectated
/// match other observers (HLTV among them) are networked in as well. Searching by class
/// and taking the first hit picks an arbitrary person's camera, and entity iteration
/// order shifts as entities churn, which reads as the target flipping between players at
/// random.
///
/// Anchoring on `m_bIsLocalPlayerController` covers both cases with one path: while
/// spectating the local pawn is the observer pawn, and while dead mid-match it is an
/// ordinary player pawn carrying the same services.
///
/// Returns `None` rather than guessing when the chain cannot be walked. A highlight that
/// blinks out for a tick is recoverable; one pointing at the wrong player is not.
pub(crate) fn read_observed_controller(reader: &Reader, entities: &EntitySnapshot) -> Option<u64> {
    let resolve = |pawn: u64| -> Option<u64> {
        let svc = reader.field_ptr(pawn, "C_BasePlayerPawn", "m_pObserverServices")?;
        let handle = reader.field_u32(svc, "CPlayer_ObserverServices", "m_hObserverTarget")?;
        let target = entities.by_handle(handle)?;
        // The target is a pawn; walk back to its controller.
        let ctrl = reader.field_u32(target.instance, PAWN, "m_hController")?;
        Some(entities.by_handle(ctrl)?.instance)
    };

    let local = entities.find(|e| {
        e.best_name() == CONTROLLER
            && reader.field_bool(e.instance, CONTROLLER, "m_bIsLocalPlayerController") == Some(true)
    });

    if let Some(local) = local {
        let handle = reader.field_u32(local.instance, BASE_CONTROLLER, "m_hPawn")?;
        let pawn = entities.by_handle(handle)?;
        return resolve(pawn.instance);
    }

    // No local controller yet (early in a connect). A lone observer pawn can only be
    // ours; more than one is ambiguous and we decline to pick.
    let mut observers = entities
        .all()
        .iter()
        .filter(|e| e.best_name() == OBSERVER_PAWN);
    let only = observers.next()?;
    if observers.next().is_some() {
        return None;
    }
    resolve(only.instance)
}

pub(crate) fn collect_players(
    reader: &Reader,
    entities: &EntitySnapshot,
    team_names: &HashMap<Team, String>,
    observed: Option<u64>,
) -> Vec<PlayerRow> {
    let mut rows: Vec<PlayerRow> = entities
        .of_class(CONTROLLER)
        .map(|ctrl| read_controller(reader, ctrl.instance, team_names, observed))
        .collect();
    attach_pawn_data(reader, entities, &mut rows);
    // Spectators last, then by team and slot.
    rows.sort_by_key(|r| {
        (
            r.is_spectator,
            r.team.map(|t| t.get()).unwrap_or(u32::MAX),
            r.slot.unwrap_or(u32::MAX),
        )
    });
    rows
}

/// One player's controller: who they are, plus their whole `PlayerDataGlobal_t`.
fn read_controller(
    reader: &Reader,
    base: u64,
    team_names: &HashMap<Team, String>,
    observed: Option<u64>,
) -> PlayerRow {
    // The whole controller in one read; every field below comes out of this buffer
    // instead of costing its own round trip into the game (~35 syscalls per player
    // become one). Fields the buffer does not reach fall back to a direct read, so a
    // missing schema size costs speed only.
    let obj = reader.read_object(base, CONTROLLER);

    // m_PlayerDataGlobal is an embedded struct, not a pointer, so it lives inside
    // the buffer we just took; `pdg_off` indexes into it and `pdg` is its real
    // address, still needed for the CUtlVectors that point outside the object.
    let pdg_off = obj.member_off(CONTROLLER, "m_PlayerDataGlobal");
    let pdg = reader.field_addr(base, CONTROLLER, "m_PlayerDataGlobal");

    // Source 2 stores team and lobby slot as bytes at offsets that are not 4-aligned
    // (m_iTeamNum @ 0x3f3, m_unLobbyPlayerSlot @ 0xc31 in this build). Reading them
    // as u32 drags in neighbouring fields.
    let team = obj.u8(CONTROLLER, "m_iTeamNum").map(|t| Team(t as u32));
    let is_spectator = team == Some(Team::SPECTATOR);
    let raw_slot = obj.u8(CONTROLLER, "m_unLobbyPlayerSlot").map(u32::from);

    let steam_id = obj.u64(CONTROLLER, "m_steamID");
    // m_iszPlayerName is an inline char array like m_szTeamname, not a pointer.
    let name = obj
        .cstr(BASE_CONTROLLER, "m_iszPlayerName", MAX_NAME)
        .filter(|s| !s.is_empty());

    let mut row = PlayerRow {
        controller: base,
        // A spectator's slot field is not a lobby slot: it reads as 1 and collides
        // with the first Amber player. Suppress it rather than report a duplicate.
        slot: if is_spectator { None } else { raw_slot },
        team,
        team_name: team.and_then(|t| team_names.get(&t).cloned()),
        is_spectator,
        is_observed: observed == Some(base),
        is_bot: looks_like_bot(name.as_deref(), steam_id),
        steam_id,
        name,
        connected: obj
            .i32(BASE_CONTROLLER, "m_iConnected")
            .map(ConnectionState::from_raw),
        is_local: obj.bool(CONTROLLER, "m_bIsLocalPlayerController"),
        ..Default::default()
    };

    if let (Some(pdg), Some(at)) = (pdg, pdg_off) {
        read_player_data_global(reader, &obj, at, pdg, &mut row);
    }
    row
}

/// The embedded `PlayerDataGlobal_t`: every per-match statistic on the scoreboard.
///
/// `at` is its byte offset inside the controller's buffer, so these are slices of a read
/// already taken. `pdg` is its real address, still needed for the two `CUtlVector`s whose
/// backing stores live outside the object.
fn read_player_data_global(
    reader: &Reader,
    obj: &Object<'_>,
    at: u64,
    pdg: u64,
    row: &mut PlayerRow,
) {
    let tunables = reader.tunables();
    row.hero_id = obj.u32_at(at, PDG, "m_nHeroID").map(HeroId);
    row.level_raw = obj.u32_at(at, PDG, "m_iLevel");
    // m_iLevel reads one higher than the level the game displays.
    row.level = row.level_raw.map(|v| v.saturating_sub(1));
    row.net_worth = obj.u32_at(at, PDG, "m_iGoldNetWorth");
    row.kills = obj.u32_at(at, PDG, "m_iPlayerKills");
    row.deaths = obj.u32_at(at, PDG, "m_iDeaths");
    row.assists = obj.u32_at(at, PDG, "m_iPlayerAssists");
    row.last_hits = obj.u32_at(at, PDG, "m_iLastHits");
    row.denies = obj.u32_at(at, PDG, "m_iDenies");
    row.hero_damage = obj.u32_at(at, PDG, "m_iHeroDamage");
    row.objective_damage = obj.u32_at(at, PDG, "m_iObjectiveDamage");
    row.healing = obj.u32_at(at, PDG, "m_iHeroHealing");

    // Extended stats. Bools sit at unaligned byte offsets (0x80..0x83), so they
    // must be read as u8; a u32 read there pulls in three neighbours.
    row.ability_points = obj.u32_at(at, PDG, "m_iAPNetWorth");
    row.creep_souls = obj.u32_at(at, PDG, "m_iCreepGold");
    row.secured_souls = obj.u32_at(at, PDG, "m_iCreepGoldAirOrb");
    row.denied_souls = obj.u32_at(at, PDG, "m_iCreepGoldDeny");
    row.neutral_souls = obj.u32_at(at, PDG, "m_iCreepGoldNeutral");
    row.farm_baseline = obj.u32_at(at, PDG, "m_iFarmBaseline");
    row.kill_streak = obj.u32_at(at, PDG, "m_iKillStreak");
    row.self_healing = obj.u32_at(at, PDG, "m_iSelfHealing");
    row.is_alive = obj.u8_at(at, PDG, "m_bAlive").map(|v| v != 0);
    row.respawn_time = obj.f32_at(at, PDG, "m_flRespawnTime");
    row.ultimate_trained = obj.u8_at(at, PDG, "m_bUltimateTrained").map(|v| v != 0);
    row.ultimate_cooldown_start = obj.f32_at(at, PDG, "m_flUltimateCooldownStart");
    row.ultimate_cooldown_end = obj.f32_at(at, PDG, "m_flUltimateCooldownEnd");
    row.has_rejuvenator = obj.u8_at(at, PDG, "m_bHasRejuvenator").map(|v| v != 0);
    row.has_rebirth = obj.u8_at(at, PDG, "m_bHasRebirth").map(|v| v != 0);
    row.flagged_as_cheater = obj.u8_at(at, PDG, "m_bFlaggedAsCheater").map(|v| v != 0);
    row.abandoned = obj.u8_at(at, PDG, "m_bAbandon").map(|v| v != 0);
    row.packed_rank = obj.u32_at(at, PDG, "m_unPackedRank");

    row.items = reader.field_vec_u32(pdg, PDG, "m_vecUpgrades", tunables.max_items);
    row.ability_upgrades = reader
        .field_vec_pairs(
            pdg,
            PDG,
            "m_vecAbilityUpgradeState",
            tunables.ability_upgrade_layout,
            tunables.max_ability_upgrades,
        )
        .map(|pairs| {
            pairs
                .into_iter()
                .map(|(id, info)| AbilityUpgrade::from_raw(id, info))
                .collect()
        });
    row.stat_attribution = read_stat_attribution(reader, pdg);
    row.bonus_counters = read_bonus_counters(reader, pdg);
}

/// Read `m_vecStatViewerModifierValues`, naming each stat from the runtime schema.
///
/// Unconditional rather than behind a feature. The crate's two existing gates are not cost
/// gates â€” `positions` is off because live coordinates confer an advantage, and `events`
/// because it is a layer over the reader rather than part of it â€” and this is neither: the
/// game shows every one of these numbers in its own stat viewer. What it costs is one
/// 12-byte read per element, measured at ~0.7 us against the live client, so a hundred
/// entries across twelve players is under a millisecond on a tick that already spends
/// about that much walking entities. The [`MAX_STAT_CONTRIBUTIONS`] cap bounds the
/// pathological case.
///
/// Reading the whole backing store in one call per player would cut that to about 17 us,
/// since a 4 KB read costs 1.1 us against a 12-byte read's 0.7. That needs a bulk variant
/// of [`Reader::field_vec_strided`], which lives in `reader.rs`.
fn read_stat_attribution(reader: &Reader, pdg: u64) -> Option<Vec<StatContribution>> {
    let layout = StatLayout::resolve(reader);
    let raw = reader.field_vec_strided(
        pdg,
        PDG,
        "m_vecStatViewerModifierValues",
        layout.stride,
        MAX_STAT_CONTRIBUTIONS,
        |mem, at| read_stat_element(mem, at, layout),
    )?;
    let schema = reader.schema();
    Some(
        raw.into_iter()
            .map(
                |(source_modifier_id, val_type_raw, value)| StatContribution {
                    source_modifier_id,
                    val_type_raw,
                    val_type: schema
                        .and_then(|s| {
                            s.enum_name_for(STAT_VIEWER, "m_eValType", i64::from(val_type_raw))
                        })
                        .map(str::to_owned),
                    value,
                },
            )
            .collect(),
    )
}

/// Read all four bonus-counter vectors, or none of them.
fn read_bonus_counters(reader: &Reader, pdg: u64) -> Option<BonusCounters> {
    let read = |field: &str| reader.field_vec_u32(pdg, PDG, field, MAX_BONUS_COUNTERS);
    Some(BonusCounters {
        abilities: read("m_vecBonusCounterAbilities")?,
        values: read("m_vecBonusCounterValues")?,
        modifiers: read("m_vecBonusCounterModifiers")?,
        modifier_values: read("m_vecModifierBonusCounterValues")?,
    })
}

/// Health, position and the pawn address, matched back to the rows already read.
///
/// Walks pawns to their controllers rather than the other way round. That is a choice,
/// not a necessity: `CCitadelPlayerController` declares `m_hHeroPawn`, and
/// `CBasePlayerController::m_hPawn` is already used elsewhere in this file. The pawn-first
/// loop wins because every pawn has to be read for health and position regardless, so
/// resolving the link from the side already being read costs one field instead of a
/// handle lookup per controller.
fn attach_pawn_data(reader: &Reader, entities: &EntitySnapshot, rows: &mut [PlayerRow]) {
    for pawn in entities.of_class(PAWN) {
        // Controller handle and both health fields out of one read.
        let obj = reader.read_object(pawn.instance, PAWN);
        let handle = match obj.u32(PAWN, "m_hController") {
            Some(h) => h,
            None => continue,
        };
        let ctrl_ent = match entities.by_handle(handle) {
            Some(e) => e,
            None => continue,
        };
        if let Some(row) = rows.iter_mut().find(|r| r.controller == ctrl_ent.instance) {
            row.pawn = Some(pawn.instance);
            row.pawn_handle = Some(pawn.handle);
            row.health = obj
                .i32(PAWN, "m_iHealth")
                .or_else(|| obj.i32(BASE_ENTITY, "m_iHealth"));
            row.max_health = obj
                .i32(PAWN, "m_iMaxHealth")
                .or_else(|| obj.i32(BASE_ENTITY, "m_iMaxHealth"));
            #[cfg(feature = "positions")]
            {
                row.position = reader
                    .field_ptr(pawn.instance, PAWN, "m_pGameSceneNode")
                    .and_then(|node| reader.field_vec3(node, "CGameSceneNode", "m_vecAbsOrigin"));
            }
            #[cfg(feature = "modifiers")]
            {
                row.modifiers = super::read_modifiers(reader, pawn.instance);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handle is what links a modifier back to the player who applied it.
    ///
    /// `m_hCaster` is a handle; `PlayerRow::pawn` is an address. The two identify the same
    /// entity and never compare equal, so a consumer holding a modifier had no way to name
    /// its caster - it had to infer from team numbers instead, which is a step removed and
    /// wrong for anything a neutral applies.
    #[test]
    fn a_pawn_handle_identifies_the_caster_a_pawn_address_cannot() {
        const PAWN_HANDLE: u32 = 0x1234;
        const PAWN_ADDRESS: u64 = 0x7fff_0000_1234;

        let row = PlayerRow {
            pawn: Some(PAWN_ADDRESS),
            pawn_handle: Some(PAWN_HANDLE),
            ..Default::default()
        };

        let caster_handle: u32 = PAWN_HANDLE;
        assert_eq!(row.pawn_handle, Some(caster_handle), "self-applied");
        assert_ne!(u64::from(caster_handle), PAWN_ADDRESS);

        let enemy_handle: u32 = 0x5678;
        assert_ne!(row.pawn_handle, Some(enemy_handle));

        assert_eq!(PlayerRow::default().pawn_handle, None);
    }
    use deadlock_memory::mock::MockMemory;

    /// Base of the fake `StatViewerModifierValues_t` array in [`stat_array`].
    const DATA: u64 = 0x2000;

    /// Three elements laid out exactly as the runtime schema declares the type, mapped as
    /// one segment so a read running past the end fails rather than sliding into whatever
    /// is next.
    ///
    /// The values are distinct per element and per member, so a read that lands on the
    /// wrong element or the wrong member cannot accidentally agree with the right one.
    fn stat_array() -> MockMemory {
        let l = StatLayout::DECLARED;
        let mut buf = vec![0u8; (l.stride * 3) as usize];
        for (i, (source, val_type, value)) in [(11u32, 7u32, 1.5f32), (22, 8, 2.5), (33, 9, 3.5)]
            .into_iter()
            .enumerate()
        {
            let at = i * l.stride as usize;
            let put = |buf: &mut [u8], off: u64, bytes: [u8; 4]| {
                let a = at + off as usize;
                buf[a..a + 4].copy_from_slice(&bytes);
            };
            put(&mut buf, l.source_modifier_id, source.to_le_bytes());
            put(&mut buf, l.val_type, val_type.to_le_bytes());
            put(&mut buf, l.value, value.to_le_bytes());
        }
        let mut mem = MockMemory::new(1);
        mem.write(DATA, &buf);
        mem
    }

    /// Decode all three elements walking by `stride`, the way `field_vec_strided` does.
    fn walk(mem: &MockMemory, stride: u64) -> Vec<Option<(u32, u32, f32)>> {
        (0..3)
            .map(|i| read_stat_element(mem, DATA + i * stride, StatLayout::DECLARED))
            .collect()
    }

    /// The failure this guards: `AbilityUpgradeState_t` is 0x38 bytes and
    /// `StatViewerModifierValues_t` is 0x40, and both carry their members at 0x30 and up.
    /// Reusing the ability stride reads eight bytes into the previous element's tail from
    /// the second element onwards, which decodes as a plausible-looking record rather than
    /// as a failure.
    #[test]
    fn stat_elements_are_walked_by_the_declared_element_size() {
        let mem = stat_array();
        assert_eq!(
            walk(&mem, StatLayout::DECLARED.stride),
            [Some((11, 7, 1.5)), Some((22, 8, 2.5)), Some((33, 9, 3.5)),]
        );
    }

    /// The same array read at `AbilityUpgradeState_t`'s stride must not agree with itself:
    /// only the first element can be right, because only it starts at the array base.
    #[test]
    fn the_ability_upgrade_stride_does_not_decode_this_element_type() {
        let mem = stat_array();
        let wrong = walk(&mem, 0x38);
        assert_eq!(
            wrong[0],
            Some((11, 7, 1.5)),
            "element zero is stride-agnostic"
        );
        assert_ne!(wrong[1], Some((22, 8, 2.5)));
        assert_ne!(wrong[2], Some((33, 9, 3.5)));
    }

    /// An element the array does not reach fails rather than decoding zeroes. A short list
    /// is worse than no list; see `Reader::field_vec_u32`.
    #[test]
    fn an_element_past_the_end_of_the_array_fails_to_read() {
        let mem = stat_array();
        let past = DATA + StatLayout::DECLARED.stride * 3;
        assert_eq!(read_stat_element(&mem, past, StatLayout::DECLARED), None);
    }

    /// The declared layout's three members sit at 0x30, 0x34 and 0x38, so one read of
    /// twelve bytes covers all of them.
    #[test]
    fn the_declared_layout_spans_twelve_bytes_from_the_first_member() {
        assert_eq!(StatLayout::DECLARED.span(), Some((0x30, 12)));
    }

    /// A layout whose members do not fit the element is refused before any read. The stack
    /// buffer is fixed, and an offset the game supplied must not be able to size a read.
    #[test]
    fn a_layout_whose_members_escape_the_element_is_refused() {
        let overrun = StatLayout {
            stride: 0x38,
            ..StatLayout::DECLARED
        };
        assert_eq!(overrun.span(), None);

        let absurd = StatLayout {
            stride: u64::MAX,
            value: u64::MAX - 3,
            ..StatLayout::DECLARED
        };
        assert_eq!(absurd.span(), None);

        let mem = stat_array();
        assert_eq!(read_stat_element(&mem, DATA, overrun), None);
    }

    /// Members in a different order still decode by name rather than by position, so a
    /// patch that reorders the record does not silently swap the stat for its source.
    #[test]
    fn members_are_decoded_by_offset_not_by_order() {
        let l = StatLayout {
            stride: 0x40,
            source_modifier_id: 0x38,
            val_type: 0x30,
            value: 0x34,
        };
        let mut buf = vec![0u8; 0x40];
        buf[0x38..0x3c].copy_from_slice(&99u32.to_le_bytes());
        buf[0x30..0x34].copy_from_slice(&5u32.to_le_bytes());
        buf[0x34..0x38].copy_from_slice(&(-2.5f32).to_le_bytes());
        let mut mem = MockMemory::new(1);
        mem.write(DATA, &buf);
        assert_eq!(read_stat_element(&mem, DATA, l), Some((99, 5, -2.5)));
    }

    /// One element is one crossing into the other process, not three. Twelve players'
    /// worth of stat rows is the largest per-tick read this file adds, and the whole
    /// gating argument for doing it unconditionally rests on this number.
    #[test]
    fn decoding_one_element_costs_exactly_one_read() {
        let mem = stat_array();
        mem.reset_reads();
        assert!(read_stat_element(&mem, DATA, StatLayout::DECLARED).is_some());
        assert_eq!(mem.reads(), 1);
    }

    /// The two halves are zipped on an inferred pairing, so length-mismatched lists must
    /// be reported rather than quietly truncated into plausible pairs.
    #[test]
    fn bonus_counter_pairing_reports_a_length_mismatch() {
        let mut c = BonusCounters {
            abilities: vec![1, 2],
            values: vec![10, 20],
            modifiers: vec![3],
            modifier_values: vec![30],
        };
        assert!(c.pairing_holds());
        assert!(!c.is_empty());
        assert_eq!(c.ability_counters().collect::<Vec<_>>(), [(1, 10), (2, 20)]);
        assert_eq!(c.modifier_counters().collect::<Vec<_>>(), [(3, 30)]);

        c.values.pop();
        assert!(!c.pairing_holds());
        assert_eq!(c.ability_counters().collect::<Vec<_>>(), [(1, 10)]);
    }

    #[test]
    fn empty_bonus_counters_are_empty() {
        assert!(BonusCounters::default().is_empty());
        assert!(BonusCounters::default().pairing_holds());
    }

    #[test]
    fn alive_needs_positive_health() {
        let mut r = PlayerRow::default();
        assert!(!r.alive());
        r.health = Some(0);
        assert!(!r.alive());
        r.health = Some(1);
        assert!(r.alive());
    }

    /// Ground truth from a live match: a Yamato with Power Slash at 1 point, Flying
    /// Slash at 2, Crimson Slash at 0, and an ultimate not yet learned.
    #[test]
    fn decodes_observed_upgrade_values() {
        let cases = [
            (0x0003_0001u32, true, 1), // Power Slash, 1 point
            (0x0007_0001, true, 2),    // Flying Slash, 2 points
            (0x0001_0001, true, 0),    // Crimson Slash, unlocked, 0 points
            (0x0000_0001, false, 0),   // ultimate, not learned
        ];
        for (raw, unlocked, points) in cases {
            let a = AbilityUpgrade::from_raw(42, raw);
            assert_eq!(a.unlocked, unlocked, "raw {raw:#x}");
            assert_eq!(a.points, points, "raw {raw:#x}");
            assert_eq!(a.raw, raw);
            assert_eq!(a.item_id, 42);
        }
    }

    /// A fully upgraded ability, seen on a level-31 Celeste.
    #[test]
    fn decodes_a_maxed_ability() {
        let a = AbilityUpgrade::from_raw(1, 0x000F_0001);
        assert!(a.unlocked);
        assert_eq!(a.points, 3);
        assert_eq!(a.mask(), 0xF);
    }

    /// The mask is a contiguous run of low bits, so point count and set-bit count agree.
    #[test]
    fn mask_and_points_stay_consistent() {
        for (mask, points) in [(0x1u32, 0), (0x3, 1), (0x7, 2), (0xF, 3)] {
            let a = AbilityUpgrade::from_raw(0, mask << 16 | 0x0001);
            assert_eq!(a.mask(), mask);
            assert_eq!(a.points, points);
            assert_eq!(a.points, a.mask().count_ones() - 1);
        }
    }

    /// An all-zero record must not underflow into a huge point count.
    #[test]
    fn zero_does_not_underflow() {
        let a = AbilityUpgrade::from_raw(0, 0);
        assert!(!a.unlocked);
        assert_eq!(a.points, 0);
    }

    #[test]
    fn spent_points_sum_across_abilities() {
        let p = PlayerRow {
            ability_upgrades: Some(vec![
                AbilityUpgrade::from_raw(1, 0x0003_0001),
                AbilityUpgrade::from_raw(2, 0x0007_0001),
                AbilityUpgrade::from_raw(3, 0x0001_0001),
                AbilityUpgrade::from_raw(4, 0x0000_0001),
            ]),
            ..Default::default()
        };
        assert_eq!(p.spent_ability_points(), 3);
    }

    #[test]
    fn a_numbered_name_with_no_steam_id_is_a_bot() {
        assert!(looks_like_bot(Some("Bot0"), Some(0)));
        assert!(looks_like_bot(Some("Bot28"), None));
    }

    #[test]
    fn a_human_named_like_a_bot_is_not_one() {
        assert!(!looks_like_bot(Some("Bot7"), Some(76_561_198_347_512_100)));
    }

    #[test]
    fn names_that_only_resemble_bots_are_not_bots() {
        for name in [
            None,
            Some(""),
            Some("Bot"),
            Some("Bots1"),
            Some("Bot1x"),
            Some("bot1"),
            Some("Flint Snow"),
        ] {
            assert!(!looks_like_bot(name, Some(0)), "{name:?}");
        }
    }
}
