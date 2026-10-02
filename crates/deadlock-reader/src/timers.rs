//! Match clock and objective timers.
//!
//! # What the client actually knows
//!
//! Only one objective publishes its next spawn to the client:
//!
//! | Objective | Next spawn | In-progress state |
//! |---|---|---|
//! | Midboss | `m_tNextMidBossSpawnTime`, read from the game | entity presence, health |
//! | Unstable Rift (Koth) | server-only | scoring team, cash-in and give-up times, per-team capture, entity presence |
//! | Urn (Idol, CTF) | server-only | entity presence |
//! | Bridge buffs (crates) | server-only | nothing reliable |
//!
//! `m_timeNextKothSpawn` and `m_timeLastSpawnCrates` do exist, but only on
//! `CCitadelGameRules`, the server class. The networked client class,
//! `C_CitadelGameRules`, does not carry them, so a client-side reader cannot see them.
//! This is not an offset that needs finding; the data is not there.
//!
//! Re-checked field by field against build 6683's runtime schema, because a rift
//! countdown is the one number a consumer most wants: `CCitadelGameRules` declares
//! `m_timeNextKothSpawn`, `m_timeNextKothSpawnWindowTime`, `m_vNextKothLocation` and
//! `m_nKothWindowWarning`, and `C_CitadelGameRules` declares none of the four. There is
//! therefore no next-rift time to read and no cadence in the client to derive one from,
//! which is why [`Timers::rift`] carries [`TimerSource::Unknown`] and not a
//! [`TimerSource::Schedule`] estimate. The shipped vdata does not supply one either:
//! `m_KothParams` in `scripts/generic_data.vdata_c` has eight members and all eight are
//! particles, sounds and a radius. So the number is absent rather than estimated.
//!
//! The Rejuvenator has no remaining time to report. Every field in the 3,606-class
//! runtime schema whose name contains "rejuv" is one of four things: the two rules
//! counters `m_iAmberRejuvCount` / `m_iSapphireRejuvCount` (already on
//! [`LiveSnapshot`](crate::snapshot::LiveSnapshot)),
//! `PlayerDataGlobal_t::m_bHasRejuvenator`, which is a flag rather than a clock, and
//! designer fields on `*VData` classes. The vdata does ship a duration -
//! `m_RejuvParams.m_flRejuvinatorBuffDuration`, 180 seconds - but a duration counts down
//! from a start, and the client is networked no stamp for when the buff was taken. With
//! nothing to subtract from, [`MidbossState`] offers no remaining time.
//!
//! Every timer therefore says where its number came from via [`TimerSource`]. Treat
//! [`TimerSource::Schedule`] as an estimate: it is arithmetic on the match clock using
//! [`Tunables::bridge_buff_period`](crate::Tunables), and it goes wrong the moment Valve
//! retunes the cadence. Override the tunable when that happens.

use deadlock_core::Team;

use crate::entity::EntitySnapshot;
use crate::reader::Reader;
use crate::snapshot::{MatchClock, Objective};
use crate::tunables::class_in;

/// Where a timer's value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum TimerSource {
    /// Read from a field the game networks to the client. Trust it.
    Game,
    /// Computed from the match clock and a cadence constant. An estimate.
    Schedule,
    /// The client does not publish this and there is no cadence to fall back on.
    Unknown,
}

/// One objective's availability and next spawn.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ObjectiveTimer {
    /// Whether the objective is on the map right now, from the entity list.
    ///
    /// `None` means the client exposes no entity class that reliably marks this
    /// objective, not that it is absent. A UI can hide an unknown, but must not draw it
    /// as "down".
    pub present: Option<bool>,
    /// How many are on the map, when that is observable.
    pub count: Option<usize>,
    /// Seconds until it next spawns, or `None` while it is already up or unknown.
    pub next_spawn_in: Option<f32>,
    /// Where `next_spawn_in` came from.
    pub source: TimerSource,
}

impl ObjectiveTimer {
    /// An objective the client can see on the map but whose schedule it cannot.
    fn observed_only(count: usize) -> Self {
        ObjectiveTimer {
            present: Some(count > 0),
            count: Some(count),
            next_spawn_in: None,
            source: TimerSource::Unknown,
        }
    }

    /// The rift, whose point is on the map but whose cadence is server-side.
    ///
    /// `count` is how many `CCitadel_KothCashIn` entities the entity list holds. That
    /// entity being present is what `present` reports, which is not quite the same claim
    /// as "a rift is running" - see [`Timers::rift`] - and [`RiftState::contested`] is
    /// the check for the latter.
    fn rift(count: usize) -> Self {
        ObjectiveTimer {
            present: Some(count > 0),
            count: Some(count),
            next_spawn_in: None,
            source: TimerSource::Unknown,
        }
    }

    /// An objective the client exposes nothing usable about.
    fn opaque() -> Self {
        ObjectiveTimer {
            present: None,
            count: None,
            next_spawn_in: None,
            source: TimerSource::Unknown,
        }
    }

    /// `next_spawn_in` as `m:ss`, or `None` when there is no number to show.
    pub fn display(&self) -> Option<String> {
        let s = self.next_spawn_in?.max(0.0) as u32;
        Some(format!("{}:{:02}", s / 60, s % 60))
    }
}

/// One team's progress on the rift, from a `TeamKothState_t`.
///
/// # The client never fills this in
///
/// `m_vecTeamKothStates` reports a count of 2 with its backing store pointing into
/// `client.dll`'s image rather than the heap. The header shape is the ordinary one and
/// the stride is the schema's declared 88 bytes; the storage behind it is never written.
/// Watched across a live rift on build 6683, both elements stayed byte-identical while a
/// real capture ran to roughly 27%, so this is not "no rift here" - the vector is dead.
/// [`RiftState::scoring_team`] and [`RiftState::give_up_at`] were confirmed against that
/// same rift, so the rift is readable, just not through here.
///
/// There is deliberately no `participants` member for `m_vecParticipants @ +0x40`: it
/// sits in the same never-written bytes, so it could only ever return placeholder data.
/// The point entity carries no substitute either - across the same rifts its
/// `m_hTouchingEntities` stayed empty and its `m_pUIWorldEventTimer` stayed null - so who
/// is standing on the rift is not a question this client answers.
///
/// [`TeamKothState::is_plausible`] keeps the placeholder from reaching a consumer as fact.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TeamKothState {
    /// `m_flCaptureProgressFrac`, nominally `0.0..=1.0`.
    pub capture_progress: Option<f32>,
    /// `m_nCapturerCount`, how many of this side are on the point.
    pub capturers: Option<u32>,
    /// `m_bIsBlocked`, set while the other side contests it.
    pub blocked: Option<bool>,
}

impl TeamKothState {
    /// Highest capturer count worth believing.
    ///
    /// Six players a side, so anything above that is a read of something else.
    const MAX_CAPTURERS: u32 = 6;

    /// Whether the values look like a real capture rather than uninitialised storage.
    ///
    /// A fraction outside `0.0..=1.0`, a non-finite one, or more capturers than a team
    /// has players all mean the vector was read while its backing store held something
    /// other than rift state. Placeholder storage produces exactly that: values like
    /// `-2.6e-30` and a capturer count of 32,761.
    pub fn is_plausible(&self) -> bool {
        let progress_ok = self
            .capture_progress
            .is_none_or(|f| f.is_finite() && (0.0..=1.0).contains(&f));
        let capturers_ok = self.capturers.is_none_or(|c| c <= Self::MAX_CAPTURERS);
        progress_ok && capturers_ok
    }

    /// Whether this side is making progress right now.
    pub fn is_capturing(&self) -> bool {
        self.is_plausible() && self.capturers.unwrap_or(0) > 0 && !self.blocked.unwrap_or(false)
    }

    /// Seconds until this side finishes the capture, at `progress_per_second`.
    ///
    /// The rate is the caller's to supply, and deliberately so. A rate needs two samples
    /// of [`TeamKothState::capture_progress`] and this crate is a snapshot reader that
    /// keeps nothing between ticks; differencing belongs in `deadlock-events`, which is a
    /// layer above precisely so that the reader stays stateless. Taking the rate as an
    /// argument keeps the arithmetic here, where it is testable, without the state.
    ///
    /// `None` rather than a number whenever the answer would mislead: implausible state,
    /// a rate that is zero, negative or non-finite - a blocked point makes no progress
    /// and "0 seconds" reads as a capture about to land - or a bar already full.
    pub fn seconds_until_capture(&self, progress_per_second: f32) -> Option<f32> {
        if !self.is_plausible() || !progress_per_second.is_finite() || progress_per_second <= 0.0 {
            return None;
        }
        let remaining = 1.0 - self.capture_progress?;
        (remaining > 0.0).then(|| remaining / progress_per_second)
    }
}

/// The Unstable Rift, a king-of-the-hill objective.
///
/// The next-spawn time is server-only, but once a rift is being contested the client
/// does see who is scoring and how long they have.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct RiftState {
    /// `m_nKothScoringTeam`.
    ///
    /// Do not read this as "who is on the point right now". Sampled repeatedly across a
    /// live match it held a constant team long after any rift had resolved, so it
    /// behaves like the last side to score rather than a live contest flag. Gate on
    /// [`RiftState::contested`], which additionally requires an unexpired give-up
    /// window.
    pub scoring_team: Option<Team>,
    /// `m_timeKothCashInStarted`, engine time the current cash-in began.
    pub cash_in_started: Option<f32>,
    /// `m_timeKothScoring`, engine time scoring started.
    pub scoring_since: Option<f32>,
    /// `m_timeKothGiveUp`, engine time the attempt lapses if nobody holds it.
    pub give_up_at: Option<f32>,
    /// `m_nAmberGold`, the pot Amber has on the rift.
    ///
    /// # Inferred association
    ///
    /// The field name says nothing about the rift. What ties it here is the class
    /// layout: it sits at `+0x1f8`, between `m_timeKothGiveUp` at `+0x1f4` and
    /// `m_vKothCashInCurrentLocation` at `+0x200`, in the middle of the run of six
    /// Koth-named members. Declaration adjacency is suggestive, not proof, and the
    /// Hideout reads it as 0 like everything else in that run.
    pub amber_gold: Option<i32>,
    /// `m_nSapphireGold`, the same for Sapphire. Same inference; see
    /// [`RiftState::amber_gold`].
    pub sapphire_gold: Option<i32>,
    /// `m_vKothCashInCurrentLocation`, where the cash-in point is.
    ///
    /// `None` when it reads as the origin, which is what the Hideout returns and is the
    /// game's unset value rather than a point at world zero.
    ///
    /// Not behind the `positions` feature, unlike [`PlayerRow`](crate::snapshot::PlayerRow)'s
    /// coordinates: that gate exists because live player positions are information the
    /// game does not show you, and the rift's location is pinged onto both teams'
    /// minimaps by the game itself.
    pub cash_in_location: Option<[f32; 3]>,
    /// `CCitadel_KothCashIn::m_nEnableState`, raw and undecoded.
    ///
    /// # Deliberately unnamed
    ///
    /// The runtime schema gives this field no enum type, so the client offers no
    /// enumerator names to decode it with. Sampled across live rifts on build 6683 it
    /// took only two values, `0` while the point was on the map but not yet live and `2`
    /// once it was, but two observed values are not an enumeration and naming them would
    /// be a guess dressed as a decode. It is passed through raw.
    pub cash_in_enable_state: Option<i32>,
    /// Live `CCitadel_KothCashIn` entities. The point itself.
    ///
    /// Zero is an observation, not a gap: the entity list reads fine and holds none.
    /// Whether this entity exists between rifts or only during one could not be checked
    /// with no match running - the Hideout has neither.
    pub cash_in_count: usize,
    /// Live `CCitadelItemKothSpawner` entities.
    ///
    /// Same caveat as [`RiftState::cash_in_count`]: observed as zero in the Hideout, and
    /// its behaviour across a rift's life is unverified.
    pub spawner_count: usize,
    /// Per-team capture progress, in the order the game stores it.
    ///
    /// Empty when the vector could not be read, or when what it held failed
    /// [`TeamKothState::is_plausible`] - the Hideout case, where the vector claims two
    /// elements backed by placeholder storage.
    pub teams: Vec<TeamKothState>,
}

impl RiftState {
    /// Longest give-up window worth believing, in seconds.
    ///
    /// Every rift watched on build 6683 stamped `m_timeKothGiveUp` exactly 60 seconds past
    /// `m_timeKothCashInStarted`. This leaves an order of magnitude of headroom over that,
    /// so a retune stays readable, while the `f32::MAX` a point carries between spawning
    /// and its window opening does not become a countdown.
    const MAX_GIVE_UP_WINDOW: f32 = 600.0;

    /// Seconds until the current attempt lapses, if one is running.
    ///
    /// `None` for a stamp the engine has not set. A point on the map whose window has not
    /// opened reads `f32::MAX` rather than zero, which subtracts to a countdown of 1.1e31
    /// years, so a stamp further ahead than `RiftState::MAX_GIVE_UP_WINDOW` is treated
    /// as unset rather than believed.
    pub fn seconds_until_give_up(&self, now: Option<f32>) -> Option<f32> {
        let at = self.give_up_at.filter(|t| t.is_finite() && *t > 0.0)?;
        let left = at - now?;
        (left > 0.0 && left <= Self::MAX_GIVE_UP_WINDOW).then_some(left)
    }

    /// Whether a rift is actually running right now.
    ///
    /// Requires a live give-up window, not just a scoring team; see
    /// [`RiftState::scoring_team`] for why that field alone is not evidence.
    pub fn contested(&self, now: Option<f32>) -> bool {
        self.seconds_until_give_up(now).is_some()
            && self.scoring_team.is_some_and(|t| t.is_playing())
    }

    /// The furthest any side has got toward capturing.
    pub fn best_progress(&self) -> Option<f32> {
        self.teams
            .iter()
            .filter_map(|t| t.capture_progress)
            .fold(None, |best: Option<f32>, f| {
                Some(best.map_or(f, |b| b.max(f)))
            })
    }

    /// Whether any side is on the point and unblocked.
    pub fn being_captured(&self) -> bool {
        self.teams.iter().any(TeamKothState::is_capturing)
    }

    /// Seconds since the current cash-in began.
    ///
    /// This is the nearest the client gets to "time since the rift was revealed", and
    /// the mapping is **inferred**: the cash-in point appearing is taken to be the
    /// reveal. With no live rift to watch, nothing here establishes whether the game
    /// stamps `m_timeKothCashInStarted` when the "An Unstable Rift is Opening..."
    /// warning fires or when a player first stands on the point.
    pub fn seconds_since_cash_in(&self, now: Option<f32>) -> Option<f32> {
        elapsed_since(self.cash_in_started, now)
    }

    /// Seconds since a side started scoring, from `m_timeKothScoring`.
    pub fn seconds_since_scoring(&self, now: Option<f32>) -> Option<f32> {
        elapsed_since(self.scoring_since, now)
    }

    /// One side's rift pot. `None` for a team that has no such field, spectators
    /// included.
    pub fn gold(&self, team: Team) -> Option<i32> {
        if team == Team::AMBER {
            self.amber_gold
        } else if team == Team::SAPPHIRE {
            self.sapphire_gold
        } else {
            None
        }
    }
}

/// Seconds between an engine timestamp and `now`.
///
/// Zero is how the game spells "not set" for all four Koth stamps - the Hideout returns
/// exactly `0.0` for every one of them - so it answers `None` rather than "the whole
/// match ago". A stamp ahead of the clock is a read of something that is not a timestamp,
/// and answers `None` too.
fn elapsed_since(stamp: Option<f32>, now: Option<f32>) -> Option<f32> {
    let at = stamp.filter(|t| *t > 0.0)?;
    let since = now? - at;
    (since >= 0.0).then_some(since)
}

/// The Midboss, beyond the next-spawn time the game hands out.
///
/// # Not verified against a live Midboss
///
/// The client this was written against sat in the Hideout, which has no
/// `C_NPC_MidBoss` entity, so both numbers read as `None` there. The read path is the
/// same one [`Objective`] uses for every other structure, and that is exercised against
/// live walkers and guardians; what is unverified is only that the Midboss carries
/// sensible values in these particular members.
///
/// There is no Rejuvenator field to add here. See the module docs for the sweep that
/// established the client is never told a duration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MidbossState {
    /// `m_iHealth` off the live entity, or `None` when it is not on the map.
    pub health: Option<i32>,
    /// `m_iMaxHealth` off the live entity.
    pub max_health: Option<i32>,
}

impl MidbossState {
    /// Health as a fraction of maximum, clamped to `0.0..=1.0`.
    ///
    /// `None` unless both numbers were read and the maximum is positive. A maximum of
    /// zero is a failed read rather than a boss at nought percent, and dividing by it
    /// yields an infinity or a NaN that a caller multiplies straight into a bar width.
    ///
    /// The clamp covers the other direction: Source games do publish health above
    /// maximum, and a stale negative after a kill.
    pub fn health_fraction(&self) -> Option<f32> {
        let max = self.max_health.filter(|m| *m > 0)?;
        Some((self.health? as f32 / max as f32).clamp(0.0, 1.0))
    }

    /// Whether the Midboss has taken damage. `false` when there is nothing to compare.
    pub fn is_damaged(&self) -> bool {
        self.health_fraction().is_some_and(|f| f < 1.0)
    }
}

/// Everything time-related about a match, in one place.
///
/// [`Timers::match_time`] is the clock the game itself shows, pauses removed. Reach for
/// it rather than [`MatchClock::elapsed_seconds`], which is wall-clock since the match
/// started and runs on through a pause.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Timers {
    /// Seconds of actual play, pauses excluded. The in-game match clock.
    ///
    /// Present in the Hideout too, where it counts up from that map loading. An earlier
    /// revision suppressed it there on the reasoning that it was not "match time"; that was
    /// wrong, and the Hideout really does hold a running clock. `None` means the clock could
    /// not be read, and nothing else.
    pub match_time: Option<f32>,
    /// Next Midboss spawn. The only objective timer the client is told.
    pub midboss: Option<ObjectiveTimer>,
    /// Unstable Rift.
    ///
    /// `present` is whether a `CCitadel_KothCashIn` entity is on the map, which is a
    /// weaker claim than "a rift is running": whether that entity persists between
    /// rifts could not be checked without a live match. Gate a "rift now" indicator on
    /// [`RiftState::contested`] instead.
    ///
    /// `next_spawn_in` is always `None`. The cadence is server-side and this crate has
    /// no vdata-sourced schedule to stand in for it; see the module docs.
    pub rift: Option<ObjectiveTimer>,
    /// Urn / Sinner's Sacrifice, a capture-and-carry objective.
    pub urn: Option<ObjectiveTimer>,
    /// Bridge-buff crates.
    ///
    /// Schedule-derived only. `C_Citadel_Pickup_Modifier` was the obvious candidate for
    /// counting them, but a live match showed seven at once with no designer names, so
    /// it is a superset of some kind and counting it would report a wrong number.
    /// Presence is left unknown rather than guessed.
    pub bridge_buffs: Option<ObjectiveTimer>,
    /// Where the Urn is, when one is on the map.
    ///
    /// The origin of the `CCitadelItemPickupIdol` entity, which is the same entity
    /// [`Timers::urn`] counts. `None` means no Urn, or an Urn whose position did not
    /// read - deliberately not distinguished here, because both mean "do not attribute
    /// anything to the Urn this tick".
    ///
    /// # Why this is gated and [`RiftState::cash_in_location`] is not
    ///
    /// The rift's location is ungated because the game pings it onto both teams'
    /// minimaps, so reporting it tells a caller nothing the game withholds. The Urn is
    /// **carried**, and it could not be checked whether the pickup entity's origin
    /// follows its carrier - the Hideout has no Urn to watch. If it does, this is a
    /// player's position wearing an objective's name, which is exactly what the
    /// `positions` gate exists for. Gated until someone can watch a carried Urn and say.
    #[cfg(feature = "positions")]
    pub urn_location: Option<[f32; 3]>,
    /// Rift contest state, when one is running.
    pub rift_state: RiftState,
    /// Midboss health, when it is on the map.
    pub midboss_state: MidbossState,
}

impl Timers {
    /// `match_time` as `m:ss`, the way the in-game clock reads.
    pub fn display(&self) -> Option<String> {
        let s = self.match_time?.max(0.0) as u32;
        Some(format!("{}:{:02}", s / 60, s % 60))
    }
}

/// Seconds until the next bridge-buff spawn, given the match time and cadence.
///
/// Exactly on a boundary rolls to the next full period rather than reporting zero.
fn next_bridge_buff_in(match_time: f32, period: f32) -> f32 {
    period - match_time.rem_euclid(period)
}

const RULES: &str = "C_CitadelGameRules";

/// Read every timer.
/// Element type of `m_vecTeamKothStates`.
const KOTH_STATE: &str = "TeamKothState_t";

/// Read the per-team rift capture states.
///
/// Empty rather than partial when anything looks wrong. The vector's header reads
/// correctly even outside a rift, but its backing store is a placeholder inside the module
/// image, so the elements are whatever happens to sit there; `is_plausible` is what stops
/// that reaching a consumer. See [`TeamKothState`].
fn read_koth_states(reader: &Reader, rules: &crate::reader::Object<'_>) -> Vec<TeamKothState> {
    // Stride is the element's declared class size, the same rule every other strided
    // vector here follows.
    let Some(stride) = reader.class_size(KOTH_STATE).map(u64::from) else {
        return Vec::new();
    };
    let read = |mem: &dyn deadlock_memory::mem::MemoryReader, e: u64| {
        Some(TeamKothState {
            capture_progress: mem.read_f32(e + 0x30).ok(),
            capturers: mem.read_u32(e + 0x34).ok(),
            blocked: mem.read_u8(e + 0x38).ok().map(|v| v != 0),
        })
    };
    let states = reader
        .field_vec_strided(
            rules.base(),
            RULES,
            "m_vecTeamKothStates",
            stride,
            MAX_KOTH_TEAMS,
            read,
        )
        .unwrap_or_default();
    if states.iter().all(TeamKothState::is_plausible) {
        states
    } else {
        Vec::new()
    }
}

/// Most teams a rift can be contested by.
const MAX_KOTH_TEAMS: usize = 4;

/// The rift point, and the entity that places it.
///
/// Spelled out here rather than added to [`Tunables`](crate::Tunables) because neither
/// name has ever been seen on a live entity - the Hideout has no rift - and a tunable
/// invites overriding a default that has not itself been confirmed. Both spellings are
/// the runtime schema's own.
const KOTH_CASH_IN: &str = "CCitadel_KothCashIn";
/// See [`KOTH_CASH_IN`].
const KOTH_SPAWNER: &str = "CCitadelItemKothSpawner";

/// A world position, or `None` when the game means "unset".
///
/// The engine spells "no position" as exactly `[0, 0, 0]` rather than with a null, and a
/// Hideout client returns that for every unplaced objective. Treating it as a point at
/// world zero would put an objective in the corner of the map and pass every proximity
/// test near it.
///
/// The cost is stated: a real entity standing exactly at the origin reads as absent. No
/// playable Deadlock map has anything there, so the trade is one that cannot fire in
/// practice - but it is a trade, not a free check.
///
/// Not gated on `positions`, even though [`Timers::urn_location`] is: the rift's
/// [`RiftState::cash_in_location`] is ungated and shares this rule, and a build without
/// `positions` must still apply it.
fn usable_location(v: Option<[f32; 3]>) -> Option<[f32; 3]> {
    v.filter(|p| *p != [0.0; 3])
}

pub(crate) fn read_timers(
    reader: &Reader,
    entities: &EntitySnapshot,
    rules: &crate::reader::Object<'_>,
    clock: &MatchClock,
    objectives: &[Objective],
    midboss_next_spawn_time: Option<f32>,
) -> Timers {
    let tunables = reader.tunables();
    let count = |names: &[String]| {
        entities
            .all()
            .iter()
            .filter(|e| class_in(names, e.best_name()))
            .count()
    };

    let midboss_count = count(&tunables.midboss_classes);
    let midboss = ObjectiveTimer {
        present: Some(midboss_count > 0),
        count: Some(midboss_count),
        // m_tNextMidBossSpawnTime is an absolute engine time, so it is compared against
        // `now` (raw simulation time), not against match_time.
        next_spawn_in: if midboss_count > 0 {
            None
        } else {
            midboss_next_spawn_time
                .filter(|t| *t > 0.0)
                .zip(clock.now)
                .map(|(at, now)| (at - now).max(0.0))
        },
        source: TimerSource::Game,
    };

    // Health comes off the objective list rather than a second read of the same entity:
    // `collect_objectives` has already pulled `m_iHealth` and `m_iMaxHealth` off the live
    // `C_NPC_MidBoss`, and re-reading it would cost another object fetch per tick to
    // arrive at the same two numbers.
    let midboss_state = objectives
        .iter()
        .find(|o| class_in(&tunables.midboss_classes, &o.class))
        .map(|o| MidbossState {
            health: o.health,
            max_health: o.max_health,
        })
        .unwrap_or_default();

    let bridge_buffs = ObjectiveTimer {
        // Derived, not read: crates come round on a fixed cadence measured against the
        // match clock.
        next_spawn_in: clock
            .playing_seconds()
            .map(|t| next_bridge_buff_in(t, tunables.bridge_buff_period)),
        source: TimerSource::Schedule,
        ..ObjectiveTimer::opaque()
    };

    let cash_in_count = entities.of_class(KOTH_CASH_IN).count();

    let rift_state = RiftState {
        scoring_team: rules
            .i32(RULES, "m_nKothScoringTeam")
            .filter(|t| *t >= 0)
            .map(|t| Team(t as u32)),
        cash_in_started: rules.f32(RULES, "m_timeKothCashInStarted"),
        scoring_since: rules.f32(RULES, "m_timeKothScoring"),
        give_up_at: rules.f32(RULES, "m_timeKothGiveUp"),
        amber_gold: rules.i32(RULES, "m_nAmberGold"),
        sapphire_gold: rules.i32(RULES, "m_nSapphireGold"),
        // The origin is the unset value, not a point at world zero: the Hideout returns
        // exactly [0, 0, 0] with no rift anywhere. Shared with the Urn below so the two
        // cannot come to disagree about what "unset" looks like.
        cash_in_location: usable_location(reader.field_vec3(
            rules.base(),
            RULES,
            "m_vKothCashInCurrentLocation",
        )),
        // Off the entity, so there is nothing to read unless the point exists.
        cash_in_enable_state: entities
            .first_of_class(KOTH_CASH_IN)
            .and_then(|e| reader.field_i32(e.instance, KOTH_CASH_IN, "m_nEnableState")),
        cash_in_count,
        spawner_count: entities.of_class(KOTH_SPAWNER).count(),
        teams: read_koth_states(reader, rules),
    };

    Timers {
        match_time: clock.playing_seconds(),
        midboss: Some(midboss),
        // The rift has no client-visible spawn time, so presence is the whole of what
        // this row can say; the contest state below carries the rest.
        rift: Some(ObjectiveTimer::rift(cash_in_count)),
        urn: Some(ObjectiveTimer::observed_only(count(&tunables.urn_classes))),
        #[cfg(feature = "positions")]
        // First match, not all: there is at most one Urn on the map, and the count in
        // `urn` above is what reports it if that ever stops being true.
        urn_location: usable_location(
            entities
                .all()
                .iter()
                .filter(|e| class_in(&tunables.urn_classes, e.best_name()))
                .find_map(|e| {
                    reader
                        .field_ptr(e.instance, "C_BaseEntity", "m_pGameSceneNode")
                        .and_then(|n| reader.field_vec3(n, "CGameSceneNode", "m_vecAbsOrigin"))
                }),
        ),
        bridge_buffs: Some(bridge_buffs),
        rift_state,
        midboss_state,
    }
}

#[cfg(test)]
mod tests {

    /// The values placeholder storage actually produced on a live client.
    ///
    /// Outside a rift the vector's header reads correctly - count 2 - but its backing
    /// store points into `client.dll`'s image, so the elements are module bytes. These are
    /// the real numbers that came back: a capture fraction of -2.6e-30 and 32,761 players
    /// on the point. Passing them through would have a consumer render a rift that is not
    /// happening.
    #[test]
    fn placeholder_storage_does_not_read_as_a_capture() {
        let junk = TeamKothState {
            capture_progress: Some(-2.564_040_2e-30),
            capturers: Some(32_761),
            blocked: Some(true),
        };
        assert!(!junk.is_plausible());
        assert!(!junk.is_capturing());

        let also_junk = TeamKothState {
            capture_progress: Some(f32::NAN),
            ..Default::default()
        };
        assert!(!also_junk.is_plausible());
    }

    /// `f32::MAX` is how the engine spells "no give-up deadline yet".
    ///
    /// A point that has spawned but not started its window reads exactly that, seen on a
    /// live rift between the entity appearing and the stamp being set. Subtracting the
    /// clock from it yields a countdown of 1.1e31 years.
    #[test]
    fn a_sentinel_give_up_stamp_is_not_a_countdown() {
        let spawning = RiftState {
            give_up_at: Some(f32::MAX),
            cash_in_started: Some(1593.4375),
            scoring_team: Some(Team::AMBER),
            ..Default::default()
        };
        assert_eq!(spawning.seconds_until_give_up(Some(1593.5)), None);
        assert!(!spawning.contested(Some(1593.5)));

        let running = RiftState {
            give_up_at: Some(1657.0313),
            cash_in_started: Some(1597.0313),
            scoring_team: Some(Team::AMBER),
            ..Default::default()
        };
        assert_eq!(running.seconds_until_give_up(Some(1600.0)), Some(57.031_25));
        assert!(running.contested(Some(1600.0)));
    }

    /// The two elements as they read on every sample across a live rift.
    #[test]
    fn a_live_rift_reads_as_placeholder_storage_too() {
        let first = TeamKothState {
            capture_progress: Some(-4.822_014e-27),
            capturers: Some(32_761),
            blocked: Some(true),
        };
        let second = TeamKothState {
            capture_progress: Some(-3.551_688_3e-28),
            capturers: Some(32_761),
            blocked: Some(false),
        };
        for state in [first, second] {
            assert!(!state.is_plausible());
            assert!(!state.is_capturing());
            assert_eq!(state.seconds_until_capture(0.1), None);
        }

        let rift = RiftState {
            teams: vec![first, second],
            ..Default::default()
        };
        assert!(!rift.being_captured());
    }

    #[test]
    fn a_real_capture_reads_as_one() {
        let held = TeamKothState {
            capture_progress: Some(0.42),
            capturers: Some(2),
            blocked: Some(false),
        };
        assert!(held.is_plausible());
        assert!(held.is_capturing());

        let contested = TeamKothState {
            blocked: Some(true),
            ..held
        };
        assert!(contested.is_plausible());
        assert!(!contested.is_capturing());

        let empty = TeamKothState {
            capturers: Some(0),
            blocked: Some(false),
            capture_progress: Some(0.42),
        };
        assert!(!empty.is_capturing());
    }

    #[test]
    fn the_rift_reports_the_furthest_side_along() {
        let r = RiftState {
            teams: vec![
                TeamKothState {
                    capture_progress: Some(0.1),
                    capturers: Some(1),
                    blocked: Some(false),
                },
                TeamKothState {
                    capture_progress: Some(0.75),
                    capturers: Some(0),
                    blocked: Some(false),
                },
            ],
            ..Default::default()
        };
        assert_eq!(r.best_progress(), Some(0.75));
        assert!(r.being_captured());
        assert_eq!(RiftState::default().best_progress(), None);
        assert!(!RiftState::default().being_captured());
    }

    use super::*;
    use crate::tunables::DEFAULT_BRIDGE_BUFF_PERIOD;

    #[test]
    fn bridge_buffs_land_on_the_period() {
        let at = |t: f32| next_bridge_buff_in(t, DEFAULT_BRIDGE_BUFF_PERIOD);
        assert!((at(240.0) - 60.0).abs() < 0.01);
        assert!((at(300.0) - 300.0).abs() < 0.01);
        assert!((at(750.0) - 150.0).abs() < 0.01);
    }

    #[test]
    fn bridge_buff_period_is_tunable() {
        assert!((next_bridge_buff_in(240.0, 240.0) - 240.0).abs() < 0.01);
        assert!((next_bridge_buff_in(100.0, 240.0) - 140.0).abs() < 0.01);
    }

    #[test]
    fn a_live_objective_reports_no_countdown() {
        let t = ObjectiveTimer {
            present: Some(true),
            count: Some(1),
            next_spawn_in: None,
            source: TimerSource::Game,
        };
        assert!(
            t.display().is_none(),
            "nothing to count down to while it is up"
        );
    }

    #[test]
    fn timer_display_is_minutes_and_seconds() {
        let t = ObjectiveTimer {
            present: Some(false),
            count: Some(0),
            next_spawn_in: Some(322.0),
            source: TimerSource::Game,
        };
        assert_eq!(t.display().as_deref(), Some("5:22"));
    }

    #[test]
    fn unobservable_presence_is_none_not_false() {
        let t = ObjectiveTimer::opaque();
        assert_eq!(t.present, None, "absent and unobservable must not collapse");
        assert_eq!(t.count, None);
    }

    #[test]
    fn an_uncontested_rift_has_no_scoring_team() {
        let r = RiftState::default();
        assert!(!r.contested(Some(100.0)));
        assert!(r.seconds_until_give_up(Some(100.0)).is_none());
    }

    #[test]
    fn a_stale_scoring_team_alone_is_not_a_live_rift() {
        let r = RiftState {
            scoring_team: Some(Team::AMBER),
            give_up_at: Some(0.0),
            ..Default::default()
        };
        assert!(!r.contested(Some(500.0)));

        let live = RiftState {
            scoring_team: Some(Team::AMBER),
            give_up_at: Some(560.0),
            ..Default::default()
        };
        assert!(live.contested(Some(500.0)));
    }

    #[test]
    fn a_spectator_team_does_not_count_as_contesting() {
        let r = RiftState {
            scoring_team: Some(Team::SPECTATOR),
            give_up_at: Some(560.0),
            ..Default::default()
        };
        assert!(!r.contested(Some(500.0)));
    }

    #[test]
    fn give_up_in_the_past_is_not_reported_as_time_remaining() {
        let r = RiftState {
            give_up_at: Some(50.0),
            ..Default::default()
        };
        assert!(r.seconds_until_give_up(Some(100.0)).is_none());
        assert_eq!(r.seconds_until_give_up(Some(20.0)), Some(30.0));
    }

    /// A rate the caller measured across two ticks, because this crate holds one.
    #[test]
    fn a_capture_eta_comes_from_a_rate_the_caller_supplies() {
        let half = TeamKothState {
            capture_progress: Some(0.5),
            capturers: Some(2),
            blocked: Some(false),
        };
        let eta = half.seconds_until_capture(0.1).unwrap();
        assert!((eta - 5.0).abs() < 0.001, "{eta}");
    }

    #[test]
    fn a_stalled_capture_reports_no_eta_rather_than_zero() {
        let held = TeamKothState {
            capture_progress: Some(0.5),
            capturers: Some(1),
            blocked: Some(true),
        };
        assert_eq!(held.seconds_until_capture(0.0), None);
        assert_eq!(held.seconds_until_capture(-0.2), None);
        assert_eq!(held.seconds_until_capture(f32::NAN), None);
        assert_eq!(held.seconds_until_capture(f32::INFINITY), None);
    }

    #[test]
    fn a_finished_capture_has_nothing_left_to_count_down() {
        let done = TeamKothState {
            capture_progress: Some(1.0),
            capturers: Some(3),
            blocked: Some(false),
        };
        assert_eq!(done.seconds_until_capture(0.5), None);
    }

    #[test]
    fn placeholder_storage_never_produces_a_capture_eta() {
        let junk = TeamKothState {
            capture_progress: Some(-2.564_040_2e-30),
            capturers: Some(32_761),
            blocked: Some(true),
        };
        assert_eq!(junk.seconds_until_capture(0.1), None);
        assert_eq!(TeamKothState::default().seconds_until_capture(0.1), None);
    }

    #[test]
    fn time_since_the_cash_in_started_needs_a_cash_in_and_a_clock() {
        let live = RiftState {
            cash_in_started: Some(500.0),
            ..Default::default()
        };
        assert_eq!(live.seconds_since_cash_in(Some(512.5)), Some(12.5));
        assert_eq!(
            RiftState::default().seconds_since_cash_in(Some(512.5)),
            None
        );
        assert_eq!(live.seconds_since_cash_in(None), None);
        assert_eq!(live.seconds_since_cash_in(Some(499.0)), None);
    }

    #[test]
    fn time_since_scoring_started_needs_scoring_to_have_started() {
        let live = RiftState {
            scoring_since: Some(480.0),
            ..Default::default()
        };
        assert_eq!(live.seconds_since_scoring(Some(500.0)), Some(20.0));
        assert_eq!(
            RiftState::default().seconds_since_scoring(Some(500.0)),
            None
        );
        assert_eq!(live.seconds_since_scoring(Some(479.0)), None);
    }

    #[test]
    fn the_rift_pot_is_reported_per_team() {
        let r = RiftState {
            amber_gold: Some(1_200),
            sapphire_gold: Some(0),
            ..Default::default()
        };
        assert_eq!(r.gold(Team::AMBER), Some(1_200));
        assert_eq!(r.gold(Team::SAPPHIRE), Some(0));
        assert_eq!(r.gold(Team::SPECTATOR), None);
    }

    /// The Urn's location is read the same way the rift's is, and unset the same way.
    ///
    /// The Hideout has no Urn, so the honest answer there is `None` - and it must be
    /// `None` because the entity is absent, never because a position read returned the
    /// origin and got filtered. Those are different facts and a caller diffing ticks
    /// cannot tell them apart if both collapse to `None` silently, so the origin filter
    /// is asserted separately from the absence.
    ///
    /// # What this test cannot check, and why
    ///
    /// With no Urn on the map, the read itself never runs, so only the absent case is
    /// observable. Two deliberate breaks confirmed the gap rather than closed it:
    /// removing the urn class filter still passes, and reading `m_vecAbsVelocity` instead
    /// of `m_vecAbsOrigin` still passes. The first is masked by entity order - a live
    /// client holds 462 entities and every one resolves a scene node, but the first is
    /// `C_World` at exactly `[0, 0, 0]`, so an unfiltered search returns the origin and
    /// the unset check absorbs it. Both breaks would be caught by a match with an Urn in
    /// it, and neither can be caught here. Recorded rather than papered over with a test
    /// that only appears to check them.
    #[cfg(feature = "positions")]
    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn the_urn_has_no_location_in_the_hideout() {
        let reader = crate::Reader::attach().expect("attach");
        let Some(snap) = reader.live_snapshot().expect("snapshot") else {
            panic!("no snapshot; is the client past the main menu?");
        };
        let urn = snap.timers.urn.as_ref().expect("urn row");
        assert_eq!(
            urn.present,
            Some(false),
            "an Urn was on the map, so this test cannot say what absence looks like"
        );
        assert_eq!(
            snap.timers.urn_location, None,
            "no Urn entity, but a location was reported"
        );
    }

    /// The origin is the unset value, not a point at world zero.
    ///
    /// Pinned as a unit test rather than only live, because the live half above can only
    /// ever observe the absent case: a Hideout client has no Urn to place at the origin.
    ///
    /// The one-zero-component cases are the point. All three zero means unset; a single
    /// zero coordinate is an ordinary position on an axis, and a check written as "reject
    /// anything with a zero in it" would throw away real ground-level readings - `z` is
    /// the coordinate most likely to land on exactly zero.
    #[test]
    fn a_location_at_the_origin_is_not_a_location() {
        assert_eq!(super::usable_location(Some([0.0, 0.0, 0.0])), None);
        assert_eq!(super::usable_location(None), None);
        let real = [11570.56, -6601.84, -82.0];
        assert_eq!(super::usable_location(Some(real)), Some(real));

        for one_zero in [
            [0.0, -6601.84, -82.0],
            [11570.56, 0.0, -82.0],
            [11570.56, -6601.84, 0.0],
            [0.0, 0.0, -82.0],
        ] {
            assert_eq!(
                super::usable_location(Some(one_zero)),
                Some(one_zero),
                "{one_zero:?} is a position, not the unset value"
            );
        }
    }

    /// The Hideout does hold a match clock, and it runs.
    ///
    /// An earlier revision of this crate made [`Timers::match_time`] `None` in the Hideout,
    /// on the reasoning that `now - m_flGameStartTime` there is time since that map loaded
    /// and so not "match time". **That was wrong**, and the correction came from someone
    /// who plays the game: the Hideout genuinely holds a match clock, and it advances —
    /// observed at `405:10` and again at `488:08` roughly ninety minutes later.
    ///
    /// The lesson is narrower than "read the game's fields as given". A match **id** of
    /// zero really is the game's "no match" and is still filtered, because zero is that
    /// field's absent value. A clock that counts up is not absent; it is a measurement of
    /// something, and refusing to report it lost information rather than avoiding a false
    /// claim.
    ///
    /// This asserts the clock is present and moving rather than any particular value, since
    /// the value depends on how long the client has been open.
    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn the_hideout_holds_a_match_clock_that_runs() {
        let reader = crate::Reader::attach().expect("attach");
        let first = reader
            .live_snapshot()
            .expect("snapshot")
            .expect("a snapshot");
        if !first.is_hideout() {
            eprintln!("client is not in the Hideout; nothing to assert");
            return;
        }
        let before = first
            .timers
            .match_time
            .expect("the Hideout reports no match clock");
        assert!(before > 0.0, "the Hideout clock reads {before}");

        std::thread::sleep(std::time::Duration::from_millis(400));
        let after = reader
            .live_snapshot()
            .expect("snapshot")
            .expect("a snapshot")
            .timers
            .match_time
            .expect("the clock stopped being reported");
        assert!(
            after > before,
            "the Hideout clock did not advance: {before} then {after}"
        );
    }

    /// Every rift field the Hideout client actually returned, in one struct.
    #[test]
    fn the_hideout_reads_no_rift_at_all() {
        let hideout = RiftState {
            scoring_team: Some(Team(0)),
            cash_in_started: Some(0.0),
            scoring_since: Some(0.0),
            give_up_at: Some(0.0),
            cash_in_location: None,
            amber_gold: Some(0),
            sapphire_gold: Some(0),
            cash_in_enable_state: None,
            spawner_count: 0,
            cash_in_count: 0,
            teams: Vec::new(),
        };
        assert!(!hideout.contested(Some(1_000.0)));
        assert!(!hideout.being_captured());
        assert_eq!(hideout.seconds_until_give_up(Some(1_000.0)), None);
        assert_eq!(hideout.seconds_since_cash_in(Some(1_000.0)), None);
        assert_eq!(hideout.seconds_since_scoring(Some(1_000.0)), None);
        assert_eq!(hideout.best_progress(), None);
    }

    #[test]
    fn the_rift_reports_presence_but_never_a_next_spawn() {
        let up = ObjectiveTimer::rift(1);
        assert_eq!(up.present, Some(true));
        assert_eq!(up.count, Some(1));
        assert_eq!(up.next_spawn_in, None);
        assert_eq!(up.source, TimerSource::Unknown);

        let down = ObjectiveTimer::rift(0);
        assert_eq!(down.present, Some(false));
        assert_eq!(down.display(), None);
    }

    #[test]
    fn midboss_health_is_a_fraction_only_when_both_numbers_are_there() {
        let alive = MidbossState {
            health: Some(9_620),
            max_health: Some(19_240),
        };
        assert!((alive.health_fraction().unwrap() - 0.5).abs() < 0.001);
        assert!(alive.is_damaged());

        assert_eq!(MidbossState::default().health_fraction(), None);
        assert_eq!(
            MidbossState {
                health: Some(100),
                max_health: None,
            }
            .health_fraction(),
            None
        );
    }

    #[test]
    fn midboss_health_never_divides_by_a_non_positive_maximum() {
        assert_eq!(
            MidbossState {
                health: Some(10),
                max_health: Some(0),
            }
            .health_fraction(),
            None
        );
        assert_eq!(
            MidbossState {
                health: Some(10),
                max_health: Some(-1),
            }
            .health_fraction(),
            None
        );
    }

    #[test]
    fn midboss_health_is_clamped_to_the_bar() {
        let over = MidbossState {
            health: Some(30_000),
            max_health: Some(19_240),
        };
        assert_eq!(over.health_fraction(), Some(1.0));
        assert!(!over.is_damaged());
        let under = MidbossState {
            health: Some(-50),
            max_health: Some(19_240),
        };
        assert_eq!(under.health_fraction(), Some(0.0));
    }

    #[test]
    fn match_time_display_excludes_paused_time() {
        let t = Timers {
            match_time: Some(1_087.0),
            ..Default::default()
        };
        assert_eq!(t.display().as_deref(), Some("18:07"));
    }
}
