//! High-level match state assembled from the entity snapshot.
//!
//! Every field is an `Option`, because the reader degrades field-by-field rather than
//! failing a whole tick: a schema miss on one field must not take out the scoreboard.
//!
//! The pieces live in submodules and are re-exported flat: [`MatchClock`] and
//! [`PauseState`] (clock), [`Modifier`] (modifier), [`PlayerRow`], [`AbilityUpgrade`],
//! [`StatContribution`] and [`BonusCounters`] (player), [`TeamStats`]
//! (team), and [`Objective`], [`ObjectiveKind`] and [`StructureState`] (objective).

mod clock;
mod modifier;
mod objective;
mod player;
mod team;

pub use clock::{MatchClock, PauseState};
pub use modifier::{
    BACKDOOR_PROTECTION, CrowdControl, HOLDING_URN, Modifier, PARRIED_STUN, PARRY_WINDOW,
    URN_CASH_IN_TIMER, URN_RETURN_TIMER, UrnCarry, classify, has_backdoor_protection,
    read_modifiers, read_urn_carry,
};
pub use objective::{OBJECTIVE_CLASSES, Objective, ObjectiveKind, StructureState};
pub use player::{AbilityUpgrade, BonusCounters, PlayerRow, StatContribution};
pub use team::TeamStats;

use std::collections::HashMap;

use deadlock_core::{HeroId, Team};

use crate::drift::Drift;
use crate::entity::EntitySnapshot;
use crate::error::{Error, Result};
use crate::reader::Reader;
use crate::timers::Timers;
use crate::tunables::{Tunables, class_in};

// Re-exported so callers need only one import to work with a snapshot.
pub use deadlock_core::{GameMode, GameState, MatchMode};

/// Base class that declares `m_iTeamNum`, `m_iHealth` and `m_flSimulationTime`.
pub(crate) const BASE_ENTITY: &str = "C_BaseEntity";

const RULES: &str = "C_CitadelGameRules";

/// Every `C_CitadelGameRules` field a snapshot reads, across this module, the clock and
/// the objective timers.
///
/// Used to size one bulk read that all three then share. Adding a read without adding its
/// name here is not a correctness problem - the accessor falls through to a live read -
/// but it does give up the saving for that field.
const RULES_FIELDS: &[&str] = &[
    "m_eGameState",
    "m_eMatchMode",
    "m_eGameMode",
    "m_unMatchID",
    "m_bGamePaused",
    "m_bServerPaused",
    "m_nPauseStartTick",
    "m_nTotalPausedTicks",
    "m_iPauseTeam",
    "m_pausingPlayerId",
    "m_nHideoutOwner",
    "m_bMatchNotScored",
    "m_unExpectedPlayerCount",
    "m_bDontUploadStats",
    "m_flGameStartTime",
    "m_iWinningTeam",
    "m_iMidbossKillCount",
    "m_tNextMidBossSpawnTime",
    "m_iAmberRejuvCount",
    "m_iSapphireRejuvCount",
    // read by `snapshot::clock`
    "m_flGameStateStartTime",
    "m_flGameStateEndTime",
    "m_flMatchClockAtLastUpdate",
    "m_nMatchClockUpdateTick",
    // read by `timers`
    "m_nKothScoringTeam",
    "m_timeKothCashInStarted",
    "m_timeKothScoring",
    "m_timeKothGiveUp",
    "m_nAmberGold",
    "m_nSapphireGold",
    // Read as a `Vector` off the process rather than out of this buffer, but named here
    // so the span still reaches it if that ever changes; it sits below
    // `m_tNextMidBossSpawnTime`, so it widens nothing.
    "m_vKothCashInCurrentLocation",
];
const RULES_PROXY: &str = "C_CitadelGameRulesProxy";

/// Whose eyes the client is looking through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Perspective {
    /// Neither a local player nor an observer target could be identified.
    #[default]
    Unknown,
    /// The local player is on a playing side, so "current player" is them.
    Playing,
    /// The client is observing; "current player" is whoever the camera follows.
    Spectating,
}

/// Where the player currently is, at a coarser grain than [`GameState`].
///
/// The two offline maps are told apart by the entity classes they load: the client leaves
/// `m_eGameMode` at `Invalid` there, so the rules say nothing. See
/// [`Tunables::sandbox_classes`] and [`Tunables::explore_nyc_classes`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Context {
    /// Game rules exist but no pattern fits: loading, menus, tutorial, or an offline map
    /// whose classes are ambiguous.
    #[default]
    Other,
    /// Sitting in the Hideout.
    Hideout,
    /// In a real match (queued, custom, bot, or brawl).
    Match,
    /// On the offline Sandbox map.
    Sandbox,
    /// On the offline Explore NYC map.
    ExploreNyc,
}

/// Whether a menu screen is up, read from the UI entities the client keeps loaded.
///
/// Facts, not a state machine: the play-mode screen and the hero menu both raise the
/// camera count, and only the hero menu spawns a hero-preview unit. Meaningful in the
/// Hideout, where the menus are reachable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MenuState {
    /// Hero-preview units loaded.
    pub portrait_units: usize,
    /// UI cameras loaded.
    pub point_cameras: usize,
    /// A hero-preview unit exists, which is what the hero menu spawns.
    pub hero_menu_open: bool,
    /// More UI cameras than [`Tunables::menu_point_camera_baseline`]: some menu screen,
    /// the hero menu included, is showing.
    pub menu_open: bool,
}

/// Counts of the entity classes that place the client, gathered in one pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Survey {
    hideout: bool,
    sandbox: bool,
    explore_nyc: bool,
    portrait_units: usize,
    point_cameras: usize,
}

impl Survey {
    pub(crate) fn of<'a>(classes: impl IntoIterator<Item = &'a str>, t: &Tunables) -> Self {
        let mut out = Survey::default();
        for class in classes {
            out.hideout |= class_in(&t.hideout_classes, class);
            out.sandbox |= class_in(&t.sandbox_classes, class);
            out.explore_nyc |= class_in(&t.explore_nyc_classes, class);
            out.portrait_units += usize::from(class_in(&t.portrait_unit_classes, class));
            out.point_cameras += usize::from(class_in(&t.point_camera_classes, class));
        }
        out
    }

    pub(crate) fn menu(&self, t: &Tunables) -> MenuState {
        MenuState {
            portrait_units: self.portrait_units,
            point_cameras: self.point_cameras,
            hero_menu_open: self.portrait_units > 0,
            menu_open: self.point_cameras > t.menu_point_camera_baseline,
        }
    }
}

/// Street Brawl round state, from the `CStreetBrawlController` the rules class embeds.
///
/// Street Brawl runs in rounds with a buy phase, a combat phase and a scoring phase, none
/// of which the ordinary match clock describes. Every field is an `Option` for the usual
/// reason: a schema miss on one must not take out the rest.
///
/// Present only when the controller could be read at all. It exists on the rules class in
/// every mode, so a zeroed controller is what a normal match looks like rather than a
/// signal - use [`LiveSnapshot::is_street_brawl`], which keys on the game mode, to decide
/// whether these numbers mean anything.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct StreetBrawl {
    /// Raw `m_eStreetBrawlState`.
    pub state_raw: Option<u32>,
    /// Enumerator name straight from the runtime schema, e.g. `"ESBGS_Combat"`.
    ///
    /// `EStreetBrawlGameState` runs `ESBGS_Init`, `PreBuy`, `Buy`, `Combat`, `PreScoring`,
    /// `Scoring`, with `ESBGS_Count` as the sentinel. Resolved rather than transcribed, so
    /// a patch that adds a phase reports the new name instead of a stale one.
    pub state_name: Option<String>,
    /// Engine time the current phase began, in [`MatchClock::now`]'s base.
    pub state_start_time: Option<f32>,
    /// Engine time the current phase ends, in [`MatchClock::now`]'s base.
    pub next_state_time: Option<f32>,
    /// Seconds this match has spent outside the combat phase.
    pub total_non_combat_time: Option<f32>,
    /// Round number.
    pub round: Option<i32>,
    /// Buy-phase countdown the game last published.
    pub last_buy_countdown: Option<i32>,
    /// Amber's round score.
    pub amber_score: Option<i32>,
    /// Sapphire's round score.
    pub sapphire_score: Option<i32>,
}

impl StreetBrawl {
    /// Seconds until the current phase ends.
    ///
    /// `now` is [`MatchClock::now`]. Clamped at zero: the field keeps its value after the
    /// deadline passes, and a negative countdown reads as a timer running backwards rather
    /// than as one that has expired.
    ///
    /// `None` when either the deadline or the clock is unavailable.
    pub fn seconds_until_next_state(&self, now: f32) -> Option<f32> {
        Some((self.next_state_time? - now).max(0.0))
    }

    /// Whether any of the controller's fields carry a non-default value.
    ///
    /// The controller is embedded in the rules class in every mode, so this distinguishes
    /// a live Street Brawl from the zeroed struct an ordinary match leaves there. It is a
    /// weaker signal than the game mode and exists to corroborate it, not to replace it.
    pub fn is_active(&self) -> bool {
        self.state_raw.unwrap_or(0) != 0
            || self.round.unwrap_or(0) != 0
            || self.next_state_time.unwrap_or(0.0) != 0.0
    }
}

/// Everything the reader can say about the current match.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct LiveSnapshot {
    /// Match id, from `C_CitadelGameRules::m_unMatchID`.
    ///
    /// `u64`, which is the width the game stores it at. The `un` prefix is Valve's
    /// notation for *unsigned*, not for a width, and the schema is unambiguous:
    /// `m_unMatchID` is a `MatchID_t`, which is a boxed integer type wrapping a single
    /// `uint64`.
    ///
    /// This matches `CSOCitadelLobby::match_id`, which the Game Coordinator
    /// reports as a protobuf varint, so the two compare directly (not a link: this crate
    /// does not depend on that one).
    ///
    /// Read as a `u32` until build 6683, which was a narrowing that stayed lossless only
    /// because live ids sit around 4e7. A truncated match id is a number that looks
    /// entirely plausible, so nothing would have reported the day that stopped holding.
    ///
    /// **Zero is never reported.** `m_unMatchID` reads successfully outside a match and
    /// returns `0`, which is the game's "no match" rather than a match numbered zero;
    /// `None` covers both that and a failed read, because neither names a match.
    pub match_id: Option<u64>,
    /// Raw `m_eGameState`.
    pub game_state_raw: Option<u32>,
    /// Decoded phase.
    pub game_state: Option<GameState>,
    /// Enumerator name straight from the runtime schema, e.g.
    /// `"EGameState_GameInProgress"`. Present only when the schema walk succeeded, and
    /// authoritative when it disagrees with [`LiveSnapshot::game_state`].
    pub game_state_schema_name: Option<String>,
    /// Raw `m_eMatchMode`.
    pub match_mode_raw: Option<u32>,
    /// Decoded matchmaking mode: ranked, custom lobby, bots, and so on.
    pub match_mode: Option<MatchMode>,
    /// Raw `m_eGameMode`.
    pub game_mode_raw: Option<u32>,
    /// Decoded game mode: normal, Street Brawl, sandbox, and so on.
    pub game_mode: Option<GameMode>,
    /// Coarse location: Hideout versus a real match.
    pub context: Context,
    /// Whose eyes the client is looking through.
    pub perspective: Perspective,
    /// The match clock.
    pub clock: MatchClock,
    /// Match clock and objective timers, in the form most consumers want.
    ///
    /// [`Timers::match_time`] is the clock the game shows, pauses removed. Only the
    /// Midboss timer is read from the game; see the [`crate::timers`] docs for which
    /// are estimates and why.
    pub timers: Timers,
    /// Per-team aggregates, for the two playing sides.
    pub teams: Vec<TeamStats>,
    /// Menu screens the client has up. See [`MenuState`].
    pub menu: MenuState,
    /// `m_nHideoutOwner`, when readable.
    pub hideout_owner: Option<u32>,
    /// `m_bMatchNotScored`: the match does not count towards ratings or records.
    pub match_not_scored: Option<bool>,
    /// `m_bDontUploadStats`: the client will not upload this match's stats.
    pub dont_upload_stats: Option<bool>,
    /// `m_unExpectedPlayerCount`, the player count the server expects to join.
    ///
    /// `0` in the Hideout and `u32::MAX` in some unset states; nothing here interprets it.
    pub expected_player_count: Option<u32>,
    /// Whether the match is paused, combining every available signal.
    pub paused: Option<bool>,
    /// The individual pause fields, for callers that want to see which one fired.
    pub pause: PauseState,
    /// Team number -> name, read from the game's own `C_CitadelTeam` entities.
    pub team_names: HashMap<Team, String>,
    /// Match start time in game clock units.
    pub game_start_time: Option<f32>,
    /// How many times the Midboss has been killed this match. Not an aliveness flag;
    /// check [`LiveSnapshot::objectives`] for a live `midboss` entry.
    pub midboss_kills: Option<u32>,
    /// `m_tNextMidBossSpawnTime`, the engine time at which the Midboss next spawns.
    ///
    /// Same time base as [`MatchClock::now`]. Meaningless while the Midboss is alive,
    /// so prefer [`LiveSnapshot::seconds_until_midboss`], which accounts for that.
    pub midboss_next_spawn_time: Option<f32>,
    /// Street Brawl round state, when the embedded controller could be read.
    ///
    /// Meaningful only in Street Brawl; see [`StreetBrawl`].
    pub street_brawl: Option<StreetBrawl>,
    /// Heroes banned this match, from `m_vecBannedHeroes`.
    ///
    /// The ban phase is usually reached only through replay ingestion, as
    /// `CCitadelUserMsgBannedHeroes`. The networked rules class carries it too, so it is
    /// readable **during** the match rather than only after it - which is when a draft
    /// overlay would want it.
    ///
    /// Empty is a real answer: most modes ban nothing. `None` means the vector could not
    /// be read in full, which is deliberately distinct - a consumer diffing ticks must not
    /// read a failed read as bans being lifted.
    pub banned_heroes: Option<Vec<HeroId>>,
    /// Winning side once the match is decided, from `m_iWinningTeam`.
    ///
    /// Meaningless before the match ends; gate on
    /// [`GameState::is_over`](deadlock_core::GameState::is_over).
    pub winning_team: Option<Team>,
    /// Amber rejuvenator count.
    pub amber_rejuv: Option<u32>,
    /// Sapphire rejuvenator count.
    pub sapphire_rejuv: Option<u32>,
    /// Player rows, spectators included. Use [`LiveSnapshot::scoreboard`] for the
    /// twelve real players.
    pub players: Vec<PlayerRow>,
    /// Map structures: patron, walkers, guardians, shrines, midboss.
    ///
    /// Lane troopers and neutral camps are excluded; there are several hundred of them
    /// in a live match, which drowns out the ~20 structures. Read them from the entity
    /// snapshot if you need them.
    pub objectives: Vec<Objective>,
    /// Total entities in the snapshot this was built from.
    pub entity_count: usize,
    /// Anything noticed that suggests the game has changed under the reader.
    ///
    /// Empty is the healthy state. Non-empty means a field, class or enum this crate
    /// depends on by name has moved, and some of this snapshot may be wrong rather than
    /// merely incomplete. See [`crate::drift`].
    ///
    /// Carried here rather than left on the reader because a signal a caller has to
    /// remember to ask for is one most callers never see.
    pub drift: Vec<Drift>,
}

/// What [`Reader::live_state`] found.
///
/// [`Reader::live_snapshot`] answers `Ok(None)` both while the client is loading a map and
/// when there is nothing to read, and a map change keeps it there for 3-26 seconds. A
/// consumer that wants to hold the last state through a load instead of showing "no game"
/// needs the two told apart, and this is that.
///
/// "No game" is not here: with no game process, attaching fails (see
/// [`Attached::Absent`](crate::supervise::Attached::Absent)) and there is no reader to
/// ask. A reader that is attached and sees no game-rules entity is loading.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum LiveState {
    /// Attached, but the game-rules entity is not there: a map is loading or unloading.
    Loading(Loading),
    /// A snapshot was built. Boxed because it dwarfs the other variant.
    Live(Box<LiveSnapshot>),
}

/// What is known while a map loads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Loading {
    /// Entities in the list at the time. Small, and growing, while a map streams in.
    pub entity_count: usize,
}

impl LiveState {
    /// Whether the client is between maps.
    pub fn is_loading(&self) -> bool {
        matches!(self, LiveState::Loading(_))
    }

    /// The snapshot, if there is one. The same answer as [`Reader::live_snapshot`] gives.
    pub fn snapshot(&self) -> Option<&LiveSnapshot> {
        match self {
            LiveState::Live(s) => Some(s),
            LiveState::Loading(_) => None,
        }
    }

    /// Consume into the snapshot, if there is one.
    pub fn into_snapshot(self) -> Option<LiveSnapshot> {
        match self {
            LiveState::Live(s) => Some(*s),
            LiveState::Loading(_) => None,
        }
    }
}

/// How far the scheduled Midboss spawn may slip before it counts as overdue.
///
/// Ticks are read from a running process rather than delivered, so the match clock and the
/// spawn schedule can disagree by a fraction of a second with nothing wrong. Anything
/// inside this window is reported as an imminent spawn rather than a lapsed one.
///
/// Chosen, not measured: the lapse observed live ran to a minute, so the exact boundary
/// only has to separate tick jitter from that.
pub const MIDBOSS_SPAWN_GRACE: f32 = 5.0;

/// What the client's Midboss spawn schedule currently says.
///
/// Exists because [`LiveSnapshot::seconds_until_midboss`] cannot distinguish its two
/// zeroes: a spawn happening this instant and a spawn whose scheduled time passed long ago
/// both read as `Some(0.0)`. The second was observed live, holding at zero for a full
/// minute of match time while `m_iMidbossKillCount` advanced - so the Midboss spawned and
/// died while the countdown still claimed it was about to arrive.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MidbossSchedule {
    /// On the map now, so there is nothing to count down to.
    Alive,
    /// No spawn scheduled, or the clock could not be read.
    ///
    /// A schedule that was never read cannot have lapsed, so this is deliberately not
    /// [`Overdue`](MidbossSchedule::Overdue).
    Unscheduled,
    /// Seconds until the scheduled spawn. Zero means within [`MIDBOSS_SPAWN_GRACE`].
    In(f32),
    /// The scheduled time passed this many seconds ago and it is still not on the map.
    ///
    /// Either the client's schedule went stale or the Midboss is alive and undetected.
    /// Which of the two is not established; both are worth naming rather than smoothing
    /// into a countdown at zero.
    Overdue(f32),
}

impl LiveSnapshot {
    /// Assemble a snapshot.
    ///
    /// `Ok(None)` means there is no game-rules entity, i.e. genuinely not in a lobby or
    /// match. An `Err` means the entity is there but could not be read.
    ///
    /// The distinction matters more than it looks. Both used to be `None`, so a rename of
    /// `m_pGameRules` - one field - made the entire library report "not in a match"
    /// forever, with no error and no drift signal, because drift is only attached to a
    /// snapshot that was actually built.
    pub fn build(reader: &Reader, entities: &EntitySnapshot) -> Result<Option<Self>> {
        let Some(proxy) = entities.first_of_class(RULES_PROXY) else {
            return Ok(None);
        };
        let Some(rules) = reader.field_ptr(proxy.instance, RULES_PROXY, "m_pGameRules") else {
            return Err(Error::SchemaUnresolved(format!(
                "{RULES_PROXY}::m_pGameRules could not be resolved or read; the game-rules \
                 entity is present, so this is a broken read rather than an absent match"
            )));
        };

        // Every rules field this snapshot touches, read once. These were two dozen
        // separate cross-process reads against the same object.
        let rules = reader.read_object_spanning(rules, RULES, RULES_FIELDS);

        let game_state_raw = rules.u32(RULES, "m_eGameState");
        let match_mode_raw = rules.u32(RULES, "m_eMatchMode");
        let game_mode_raw = rules.u32(RULES, "m_eGameMode");
        // Zero is the game's "no match", not a match numbered zero. Filtering here rather
        // than at each call site keeps one rule in one place: the field already meant
        // "absent" as `None`, and `Some(0)` was a second spelling of the same fact that
        // every consumer had to know about separately.
        let match_id = rules.u64(RULES, "m_unMatchID").filter(|id| *id != 0);

        // Prefer the name the runtime schema gives, so a game update that renumbers the
        // enum is reported correctly even before the baked mapping catches up.
        let game_state_schema_name = game_state_raw.and_then(|v| {
            reader
                .schema()?
                .enum_name_for(RULES, "m_eGameState", v as i64)
                .map(str::to_owned)
        });

        // The game names its own enumerators, so a renumbering is detectable rather than
        // silent. This is the failure that once had a live match reporting `PostGame`:
        // the values were inferred from string order and were wrong twice over. The
        // schema name is authoritative; a disagreement means the baked mapping is stale.
        if let (Some(raw), Some(schema_name)) = (game_state_raw, game_state_schema_name.as_deref())
        {
            let baked = GameState::from_raw(raw).name();
            // Schema enumerators are prefixed, e.g. `EGameState_GameInProgress`.
            if !schema_name.ends_with(baked) {
                reader.note(Drift::EnumMismatch {
                    class: RULES.to_owned(),
                    field: "m_eGameState".to_owned(),
                    raw,
                    schema: schema_name.to_owned(),
                    baked: baked.to_owned(),
                });
            }
        }

        let survey = Survey::of(
            entities.all().iter().map(crate::entity::Entity::best_name),
            reader.tunables(),
        );
        let context = detect_context(&survey, match_id, game_mode_raw);

        // Several pause fields exist and it is unclear which a spectating client sees
        // replicated, so read them all and let PauseState combine them.
        let pause = PauseState {
            game_paused: rules.bool(RULES, "m_bGamePaused"),
            server_paused: rules.bool(RULES, "m_bServerPaused"),
            pause_start_tick: rules.u32(RULES, "m_nPauseStartTick"),
            total_paused_ticks: rules.u32(RULES, "m_nTotalPausedTicks"),
            pause_team: rules.u32(RULES, "m_iPauseTeam"),
            pausing_player_id: rules.u32(RULES, "m_pausingPlayerId"),
        };

        let street_brawl = read_street_brawl(reader, &rules);

        // The game names its own teams; prefer that over any table of ours. This also
        // records where each team entity lives, which `summarise_teams` needs.
        let teams = team::read_teams(reader, entities);

        let mut snap = LiveSnapshot {
            match_id,
            game_state_raw,
            game_state: game_state_raw.map(GameState::from_raw),
            game_state_schema_name,
            match_mode_raw,
            match_mode: match_mode_raw.map(MatchMode::from_raw),
            game_mode_raw,
            game_mode: game_mode_raw.map(GameMode::from_raw),
            context,
            perspective: Perspective::Unknown,
            menu: survey.menu(reader.tunables()),
            hideout_owner: rules.u32(RULES, "m_nHideoutOwner"),
            match_not_scored: rules.bool(RULES, "m_bMatchNotScored"),
            dont_upload_stats: rules.bool(RULES, "m_bDontUploadStats"),
            expected_player_count: rules.u32(RULES, "m_unExpectedPlayerCount"),
            paused: Some(pause.is_paused()),
            pause,
            // Cloned because `collect_players` and `collect_objectives` below still
            // need to borrow it. Two short strings per snapshot.
            team_names: teams.names.clone(),
            game_start_time: rules.f32(RULES, "m_flGameStartTime"),
            midboss_kills: rules.u32(RULES, "m_iMidbossKillCount"),
            midboss_next_spawn_time: rules.f32(RULES, "m_tNextMidBossSpawnTime"),
            street_brawl,
            banned_heroes: reader
                .field_vec_u32(
                    rules.base(),
                    RULES,
                    "m_vecBannedHeroes",
                    reader.tunables().max_banned_heroes,
                )
                .map(|ids| ids.into_iter().map(HeroId).collect()),
            // Negative means undecided, which is every tick before the match ends.
            winning_team: rules
                .i32(RULES, "m_iWinningTeam")
                .filter(|t| *t > 0)
                .map(|t| Team(t as u32)),
            amber_rejuv: rules.u32(RULES, "m_iAmberRejuvCount"),
            sapphire_rejuv: rules.u32(RULES, "m_iSapphireRejuvCount"),
            players: Vec::new(),
            objectives: Vec::new(),
            entity_count: entities.len(),
            drift: Vec::new(),
            clock: MatchClock::default(),
            teams: Vec::new(),
            timers: Timers::default(),
        };

        let observed = player::read_observed_controller(reader, entities);
        snap.players = player::collect_players(reader, entities, &teams.names, observed);
        snap.perspective = match snap.local_player() {
            Some(me) if me.is_playing() => Perspective::Playing,
            _ if snap.observed_player().is_some() => Perspective::Spectating,
            _ => Perspective::Unknown,
        };
        snap.objectives = objective::collect_objectives(reader, entities, &teams.names);
        snap.clock = clock::read_clock(reader, entities, &rules);
        // After `collect_objectives`, which is where the Midboss's health comes from.
        snap.timers = crate::timers::read_timers(
            reader,
            entities,
            &rules,
            &snap.clock,
            &snap.objectives,
            snap.midboss_next_spawn_time,
        );
        snap.teams = team::summarise_teams(
            reader,
            &teams,
            &snap.players,
            &snap.objectives,
            (snap.amber_rejuv, snap.sapphire_rejuv),
        );
        // Last, so it covers every read this build performed rather than a prefix of them.
        snap.drift = reader.drift();
        Ok(Some(snap))
    }

    /// Whether the player is sitting in the Hideout rather than in a match.
    pub fn is_hideout(&self) -> bool {
        self.context == Context::Hideout
    }

    /// Whether this is a real match, of any mode.
    pub fn is_match(&self) -> bool {
        self.context == Context::Match
    }

    /// Whether the client is on the offline Sandbox map.
    pub fn is_sandbox(&self) -> bool {
        self.context == Context::Sandbox
    }

    /// Whether the client is on the offline Explore NYC map.
    pub fn is_explore_nyc(&self) -> bool {
        self.context == Context::ExploreNyc
    }

    /// Whether this is a ranked match.
    pub fn is_ranked(&self) -> bool {
        self.is_match() && self.match_mode.map(|m| m.is_ranked()).unwrap_or(false)
    }

    /// Whether this is a custom / private lobby.
    pub fn is_custom(&self) -> bool {
        self.is_match() && self.match_mode.map(|m| m.is_custom()).unwrap_or(false)
    }

    /// Whether this is a Street Brawl.
    pub fn is_street_brawl(&self) -> bool {
        self.game_mode == Some(GameMode::StreetBrawl)
    }

    /// One-line description, e.g. `"Ranked / Normal"` or `"Hideout"`.
    ///
    /// English, and deliberately a convenience rather than the source of truth. Everything
    /// it composes is already on this struct - [`LiveSnapshot::context`],
    /// [`LiveSnapshot::match_mode`] and [`LiveSnapshot::game_mode`] - so a consumer that
    /// needs another language, another format, or to branch on the values should read
    /// those instead of parsing this back apart.
    ///
    /// It stays here because every consumer in this workspace wants exactly this string and
    /// would otherwise each write it out; the alternative is not "no presentation", it is
    /// the same presentation copied three times.
    pub fn describe(&self) -> String {
        match self.context {
            Context::Hideout => "Hideout".into(),
            Context::Sandbox => "Sandbox".into(),
            Context::ExploreNyc => "Explore NYC".into(),
            Context::Match => format!(
                "{} / {}",
                self.match_mode.map(|m| m.name()).unwrap_or("?"),
                self.game_mode.map(|m| m.name()).unwrap_or("?")
            ),
            Context::Other => format!(
                "{} (mode {} / {})",
                self.game_state.map(|s| s.name()).unwrap_or("?"),
                self.match_mode.map(|m| m.name()).unwrap_or("?"),
                self.game_mode.map(|m| m.name()).unwrap_or("?")
            ),
        }
    }

    /// Players on a given team.
    pub fn team(&self, team: Team) -> impl Iterator<Item = &PlayerRow> {
        self.players.iter().filter(move |p| p.team == Some(team))
    }

    /// The real scoreboard: players on Amber or Sapphire, spectators excluded.
    pub fn scoreboard(&self) -> impl Iterator<Item = &PlayerRow> {
        self.players.iter().filter(|p| p.is_playing())
    }

    /// Observers, including your own controller while spectating.
    pub fn spectators(&self) -> impl Iterator<Item = &PlayerRow> {
        self.players.iter().filter(|p| p.is_spectator)
    }

    /// Name the game gives a team number.
    pub fn team_name(&self, team: Team) -> &str {
        self.team_names
            .get(&team)
            .map(String::as_str)
            .unwrap_or_else(|| team.default_name())
    }

    /// Live structures of one kind, e.g. [`ObjectiveKind::Walker`].
    pub fn structures(&self, kind: ObjectiveKind) -> impl Iterator<Item = &Objective> + '_ {
        self.objectives.iter().filter(move |o| o.kind == kind)
    }

    /// Whether the Midboss is currently alive on the map.
    pub fn midboss_alive(&self) -> bool {
        self.objectives
            .iter()
            .any(|o| o.kind == ObjectiveKind::Midboss)
    }

    /// Seconds until the Midboss next spawns.
    ///
    /// `None` while it is alive, or before the game has scheduled a spawn. Clamped at
    /// zero so a stale timer never reads as negative.
    ///
    /// There is no fallback when the field is absent. `m_tNextMidBossSpawnTime` is the
    /// one objective schedule the client is actually sent, so an absent value means the
    /// game has not scheduled a spawn - inventing a cadence to fill the gap would put a
    /// made-up number where the game is saying "not yet".
    pub fn seconds_until_midboss(&self) -> Option<f32> {
        if self.midboss_alive() {
            return None;
        }
        let spawn = self.midboss_next_spawn_time.filter(|t| *t > 0.0)?;
        Some((spawn - self.clock.now?).max(0.0))
    }

    /// What the client's Midboss spawn schedule says, with a lapsed schedule named.
    ///
    /// The richer form of [`seconds_until_midboss`](LiveSnapshot::seconds_until_midboss),
    /// which clamps a lapsed schedule to zero and so cannot be told apart from a spawn
    /// happening this instant. Prefer this for anything a person reads.
    pub fn midboss_schedule(&self) -> MidbossSchedule {
        if self.midboss_alive() {
            return MidbossSchedule::Alive;
        }
        let (Some(spawn), Some(now)) = (
            self.midboss_next_spawn_time.filter(|t| *t > 0.0),
            self.clock.now,
        ) else {
            return MidbossSchedule::Unscheduled;
        };
        let delta = spawn - now;
        if delta < -MIDBOSS_SPAWN_GRACE {
            MidbossSchedule::Overdue(-delta)
        } else {
            MidbossSchedule::In(delta.max(0.0))
        }
    }

    /// The Midboss's current and maximum health, off the live entity.
    ///
    /// `None` unless both were read, so a caller cannot end up rendering a bar against a
    /// maximum it had to guess.
    pub fn midboss_health(&self) -> Option<(i32, i32)> {
        let s = self.timers.midboss_state;
        s.health.zip(s.max_health)
    }

    /// The Midboss's health as a fraction of maximum, clamped to `0.0..=1.0`.
    ///
    /// See [`MidbossState::health_fraction`](crate::timers::MidbossState::health_fraction)
    /// for when this declines to answer.
    pub fn midboss_health_fraction(&self) -> Option<f32> {
        self.timers.midboss_state.health_fraction()
    }

    /// Aggregates for one team.
    pub fn team_stats(&self, team: Team) -> Option<&TeamStats> {
        self.teams.iter().find(|t| t.team == team)
    }

    /// Which side is ahead on souls, and by how much.
    ///
    /// `None` if either side is missing; `0` on a dead heat.
    pub fn soul_lead(&self) -> Option<(Team, u32)> {
        let a = self.team_stats(Team::AMBER)?;
        let s = self.team_stats(Team::SAPPHIRE)?;
        Some(if a.souls >= s.souls {
            (Team::AMBER, a.souls - s.souls)
        } else {
            (Team::SAPPHIRE, s.souls - a.souls)
        })
    }

    /// Souls per minute for a side, over the time actually played.
    ///
    /// Divided by [`MatchClock::playing_seconds`], not elapsed wall time: a paused match
    /// keeps accruing elapsed seconds while nobody earns anything, so a rate over elapsed
    /// time decays for the length of every pause and then never recovers. Companion
    /// treats that as a correctness bug rather than a rounding one, and it is the reason
    /// the pause fields are read at all.
    ///
    /// `None` before the clock is readable, and before any time has been played - a rate
    /// over zero seconds is not a large number, it is not a number.
    pub fn souls_per_minute(&self, team: Team) -> Option<f32> {
        let played = self.clock.playing_seconds()?;
        if played <= 0.0 {
            return None;
        }
        let souls = self.team_stats(team)?.souls;
        Some(f64::from(souls) as f32 / (played / 60.0))
    }

    /// Souls per minute for one player, over the time actually played.
    ///
    /// Same pause handling as [`LiveSnapshot::souls_per_minute`].
    pub fn player_souls_per_minute(&self, player: &PlayerRow) -> Option<f32> {
        let played = self.clock.playing_seconds()?;
        if played <= 0.0 {
            return None;
        }
        Some(f64::from(player.net_worth?) as f32 / (played / 60.0))
    }

    /// How far ahead the leading side is, per minute played.
    ///
    /// The sign is dropped: [`LiveSnapshot::soul_lead`] already says who leads, and a
    /// signed rate here would only invite the caller to decide that twice.
    pub fn soul_lead_per_minute(&self) -> Option<(Team, f32)> {
        let (team, lead) = self.soul_lead()?;
        let played = self.clock.playing_seconds()?;
        if played <= 0.0 {
            return None;
        }
        Some((team, f64::from(lead) as f32 / (played / 60.0)))
    }

    /// The local player's row, if identified.
    ///
    /// While spectating this is your *observer* controller, which sits on
    /// [`Team::SPECTATOR`] and has no hero. Use [`LiveSnapshot::current_player`] when
    /// you want "whoever is on screen".
    pub fn local_player(&self) -> Option<&PlayerRow> {
        self.players.iter().find(|p| p.is_local == Some(true))
    }

    /// The player the client's camera is following, if any.
    ///
    /// Set while spectating, and also while dead and watching a teammate.
    pub fn observed_player(&self) -> Option<&PlayerRow> {
        self.players.iter().find(|p| p.is_observed)
    }

    /// Whoever the client is effectively "being" right now.
    ///
    /// Your own row when you are playing, otherwise the player you are watching. This
    /// is the one row that corresponds to what is on screen, which is what a HUD,
    /// overlay or rich-presence app almost always wants.
    ///
    /// Returns `None` in a replay or free-camera with no target, or before players have
    /// spawned.
    pub fn current_player(&self) -> Option<&PlayerRow> {
        match self.local_player() {
            // Playing: you are the subject, even while dead-cam is following a teammate.
            Some(me) if me.is_playing() => Some(me),
            _ => self.observed_player(),
        }
    }
}

/// Class of the controller `C_CitadelGameRules::m_tStreetBrawl` embeds.
const BRAWL: &str = "CStreetBrawlController";

/// Read the embedded Street Brawl controller.
///
/// Read as its own small object rather than out of the bulk rules buffer. That buffer is
/// sized to the highest listed field plus eight, and the controller's last member sits
/// 0x24 into a 0x28-byte struct, so covering it from there would mean either overreading
/// the rules class or letting six of the eight members fall through to individual live
/// reads. One 40-byte read is cheaper than either.
///
/// `None` when the controller's address cannot be resolved at all, which is the case on a
/// build whose schema walk missed the field.
fn read_street_brawl(reader: &Reader, rules: &crate::reader::Object<'_>) -> Option<StreetBrawl> {
    let at = rules.member_addr(RULES, "m_tStreetBrawl")?;
    let obj = reader.read_object(at, BRAWL);
    let state_raw = obj.u32(BRAWL, "m_eStreetBrawlState");
    Some(StreetBrawl {
        state_raw,
        state_name: state_raw.and_then(|v| {
            reader
                .schema()?
                .enum_name_for(BRAWL, "m_eStreetBrawlState", i64::from(v))
                .map(str::to_owned)
        }),
        state_start_time: obj.f32(BRAWL, "m_flStreetBrawlStateStartTime"),
        next_state_time: obj.f32(BRAWL, "m_flNextStateTime"),
        total_non_combat_time: obj.f32(BRAWL, "m_flStreetBrawlTotalNonCombatTime"),
        round: obj.i32(BRAWL, "m_iRound"),
        last_buy_countdown: obj.i32(BRAWL, "m_iLastBuyCountDown"),
        amber_score: obj.i32(BRAWL, "m_iTeamAmberScore"),
        sapphire_score: obj.i32(BRAWL, "m_iTeamSapphireScore"),
    })
}

/// Place the client from the classes it has loaded and what the rules say.
///
/// Order matters. The Hideout wins over everything because its entities stay loaded while
/// spectating from it. A match id or a real game mode is a match. Only then do the offline
/// maps get a say, and a map whose classes both lists claim is left as `Other` rather than
/// guessed.
fn detect_context(survey: &Survey, match_id: Option<u64>, game_mode_raw: Option<u32>) -> Context {
    if survey.hideout {
        return Context::Hideout;
    }
    if match_id.is_some() {
        return Context::Match;
    }
    match game_mode_raw.map(GameMode::from_raw) {
        Some(GameMode::Sandbox) => return Context::Sandbox,
        Some(GameMode::ExploreNyc) => return Context::ExploreNyc,
        Some(GameMode::Invalid) | None => {}
        Some(_) => return Context::Match,
    }
    match (survey.sandbox, survey.explore_nyc) {
        (true, false) => Context::Sandbox,
        (false, true) => Context::ExploreNyc,
        _ => Context::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(classes: &[&str], match_id: Option<u64>, game_mode_raw: Option<u32>) -> Context {
        let t = Tunables::default();
        let survey = Survey::of(classes.iter().copied(), &t);
        detect_context(&survey, match_id, game_mode_raw)
    }

    const HIDEOUT_SET: &[&str] = &[
        "C_CitadelTriggerHideout",
        "CCitadelHideoutInterestPoint",
        "CCitadelHideoutTeleportTrigger",
        "C_PointCamera",
    ];
    const SANDBOX_SET: &[&str] = &[
        "CCitadelHideoutTeleportTrigger",
        "CCitadelTunnelTrigger",
        "CCitadel_ShopProp",
        "C_NPC_Boss_Tier2",
    ];
    const NYC_SET: &[&str] = &[
        "CCitadelTriggerCapturePoint",
        "CCitadelItemKothSpawner",
        "C_NPC_BarrackBoss",
    ];

    #[test]
    fn the_hideout_is_told_from_its_own_classes() {
        assert_eq!(place(HIDEOUT_SET, None, Some(0)), Context::Hideout);
    }

    /// The Sandbox loads the Hideout's teleport trigger, which once made it read as the
    /// Hideout.
    #[test]
    fn the_sandbox_is_not_the_hideout_though_it_loads_a_teleport_trigger() {
        assert_eq!(place(SANDBOX_SET, None, Some(0)), Context::Sandbox);
        assert_eq!(
            place(&["CCitadelHideoutTeleportTrigger"], None, Some(0)),
            Context::Other,
            "one shared class is not enough to name any place"
        );
    }

    #[test]
    fn explore_nyc_is_told_from_its_own_classes() {
        assert_eq!(place(NYC_SET, None, Some(0)), Context::ExploreNyc);
    }

    #[test]
    fn a_class_set_both_offline_maps_claim_is_left_unnamed() {
        let both: Vec<&str> = SANDBOX_SET.iter().chain(NYC_SET).copied().collect();
        assert_eq!(place(&both, None, Some(0)), Context::Other);
    }

    #[test]
    fn a_match_id_beats_the_map_classes() {
        assert_eq!(place(SANDBOX_SET, Some(42), Some(1)), Context::Match);
        assert_eq!(place(NYC_SET, Some(42), Some(0)), Context::Match);
    }

    #[test]
    fn the_hideout_beats_the_map_classes_it_shares_the_list_with() {
        let both: Vec<&str> = HIDEOUT_SET.iter().chain(NYC_SET).copied().collect();
        assert_eq!(place(&both, None, Some(0)), Context::Hideout);
    }

    #[test]
    fn a_game_mode_the_client_does_set_still_decides() {
        assert_eq!(place(&[], None, Some(3)), Context::Sandbox);
        assert_eq!(place(&[], None, Some(5)), Context::ExploreNyc);
        assert_eq!(place(&[], None, Some(1)), Context::Match);
        assert_eq!(place(&[], None, Some(4)), Context::Match);
        assert_eq!(place(&[], None, Some(0)), Context::Other);
        assert_eq!(place(&[], None, None), Context::Other);
    }

    #[test]
    fn describe_names_the_offline_maps() {
        let mut s = LiveSnapshot {
            context: Context::Sandbox,
            ..Default::default()
        };
        assert_eq!(s.describe(), "Sandbox");
        assert!(s.is_sandbox() && !s.is_explore_nyc() && !s.is_match() && !s.is_hideout());
        s.context = Context::ExploreNyc;
        assert_eq!(s.describe(), "Explore NYC");
        assert!(s.is_explore_nyc() && !s.is_sandbox());
    }

    fn menu(classes: &[&str]) -> MenuState {
        let t = Tunables::default();
        Survey::of(classes.iter().copied(), &t).menu(&t)
    }

    #[test]
    fn a_hero_preview_unit_means_the_hero_menu_is_open() {
        let mut classes = vec!["C_PointCamera"; 14];
        classes.push("C_PortraitWorldUnit");
        let m = menu(&classes);
        assert_eq!((m.portrait_units, m.point_cameras), (1, 14));
        assert!(m.hero_menu_open && m.menu_open);
    }

    /// The play-mode screen raises the camera count without spawning a preview unit.
    #[test]
    fn extra_cameras_alone_mean_a_menu_but_not_the_hero_menu() {
        let m = menu(&["C_PointCamera"; 14]);
        assert!(m.menu_open && !m.hero_menu_open);
    }

    #[test]
    fn the_baseline_camera_count_is_no_menu() {
        let m = menu(&["C_PointCamera"; 6]);
        assert!(!m.menu_open && !m.hero_menu_open);
        assert_eq!(menu(&[]), MenuState::default());
    }

    #[test]
    fn the_menu_baseline_is_overridable() {
        let t = Tunables {
            menu_point_camera_baseline: 20,
            ..Tunables::default()
        };
        let s = Survey::of(["C_PointCamera"; 14], &t);
        assert!(!s.menu(&t).menu_open);
    }

    #[test]
    fn a_loading_state_has_no_snapshot() {
        let loading = LiveState::Loading(Loading { entity_count: 12 });
        assert!(loading.is_loading());
        assert!(loading.snapshot().is_none());
        assert!(loading.into_snapshot().is_none());

        let live = LiveState::Live(Box::default());
        assert!(!live.is_loading());
        assert!(live.snapshot().is_some());
        assert!(live.into_snapshot().is_some());
    }

    /// A match id of zero is the game's "no match", not a match numbered zero.
    ///
    /// `m_unMatchID` reads successfully outside a match and returns `0`. Reporting that as
    /// `Some(0)` says a read succeeded *and* names a match, which is two different claims
    /// from the one the client is making. It leaked: a timeline recorded in the Hideout
    /// wrote `"match_id":"0"` on every row, and `dlrs status` printed `id=Some(0)`, both of
    /// which are the zero-for-absent substitution this crate refuses everywhere else.
    ///
    /// The internal `Context` check already treated zero as "no match id"; this makes the
    /// public field agree with it.
    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn a_hideout_client_reports_no_match_id_rather_than_zero() {
        let reader = crate::Reader::attach().expect("attach");
        let snap = reader
            .live_snapshot()
            .expect("snapshot")
            .expect("a snapshot");
        assert_ne!(
            snap.match_id,
            Some(0),
            "zero is the game's absent value and must not be reported as an id"
        );
    }

    /// Values read from the live client's own `EGameState` binding.
    ///
    /// An earlier revision inferred these from the order the original binary's string
    /// pool lists them, which was wrong twice over: it missed `Invalid = 0`, and it had
    /// `WaitForMapToLoad` and `WaitingForPlayersToJoin` the other way round. The symptom
    /// was a live match reporting `PostGame`.
    #[test]
    fn game_state_matches_the_schema_enum() {
        for (raw, name) in [
            (0u32, "Invalid"),
            (1, "Init"),
            (2, "WaitingForPlayersToJoin"),
            (3, "HeroSelection"),
            (4, "MatchIntro"),
            (5, "WaitForMapToLoad"),
            (6, "PreGameWait"),
            (7, "GameInProgress"),
            (8, "PostGame"),
            (9, "PostGame_PlayOfTheGame"),
            (10, "Abandoned"),
            (11, "End"),
        ] {
            assert_eq!(GameState::from_raw(raw).name(), name, "raw {raw}");
        }
        assert!(GameState::from_raw(7).is_live());
        assert!(!GameState::from_raw(8).is_live());
        assert_eq!(GameState::from_raw(99), GameState::Unknown(99));
    }

    #[test]
    fn match_mode_matches_the_schema_enum() {
        for (raw, name) in [
            (0u32, "Invalid"),
            (1, "Unranked"),
            (2, "PrivateLobby"),
            (3, "CoopBot"),
            (4, "Ranked"),
            (5, "ServerTest"),
            (6, "Tutorial"),
            (7, "HeroLabs"),
            (8, "NewPlayerPlacement"),
        ] {
            assert_eq!(MatchMode::from_raw(raw).name(), name, "raw {raw}");
        }
        assert!(MatchMode::from_raw(4).is_ranked());
        assert!(!MatchMode::from_raw(1).is_ranked());
        assert!(MatchMode::from_raw(2).is_custom());
    }

    #[test]
    fn game_mode_matches_the_schema_enum() {
        for (raw, name) in [
            (0u32, "Invalid"),
            (1, "Normal"),
            (2, "1v1Test"),
            (3, "Sandbox"),
            (4, "StreetBrawl"),
            (5, "ExploreNYC"),
            (6, "Internal"),
        ] {
            assert_eq!(GameMode::from_raw(raw).name(), name, "raw {raw}");
        }
    }

    #[test]
    fn describe_reads_sensibly() {
        let mut s = LiveSnapshot {
            context: Context::Hideout,
            ..Default::default()
        };
        assert_eq!(s.describe(), "Hideout");
        assert!(s.is_hideout() && !s.is_match());

        s.context = Context::Match;
        s.match_mode = Some(MatchMode::Ranked);
        s.game_mode = Some(GameMode::Normal);
        assert_eq!(s.describe(), "Ranked / Normal");
        assert!(s.is_ranked() && !s.is_custom() && !s.is_street_brawl());

        s.match_mode = Some(MatchMode::PrivateLobby);
        s.game_mode = Some(GameMode::StreetBrawl);
        assert!(s.is_custom() && s.is_street_brawl() && !s.is_ranked());
    }

    /// The Hideout reports `GameInProgress` with no match id and `Invalid` modes, so a
    /// state-only check cannot distinguish it from a half-initialised match.
    #[test]
    fn hideout_is_not_distinguishable_by_state_alone() {
        let hideout = LiveSnapshot {
            game_state: Some(GameState::GameInProgress),
            match_id: Some(0),
            match_mode: Some(MatchMode::Invalid),
            game_mode: Some(GameMode::Invalid),
            context: Context::Hideout,
            ..Default::default()
        };
        assert!(hideout.game_state.unwrap().is_live());
        assert!(hideout.is_hideout());
        assert!(!hideout.is_match());
    }

    /// A per-minute rate must not decay across a pause.
    ///
    /// This is the failure the original treated as a correctness bug: "pause flag
    /// `m_bServerPaused` not in the runtime schema - pause detection is dead (per-minute
    /// stats will decay through pauses)". Dividing by elapsed time rather than played
    /// time means a five-minute pause permanently drags every rate down, and it never
    /// recovers, because the lost seconds stay in the denominator for the rest of the
    /// match.
    #[test]
    fn a_per_minute_rate_is_taken_over_played_time_not_elapsed_time() {
        let mut snap = LiveSnapshot {
            clock: MatchClock {
                game_start_time: Some(0.0),
                now: Some(600.0),
                total_paused_ticks: Some(60 * 64),
                tick_rate: 64.0,
                ..Default::default()
            },
            teams: vec![TeamStats {
                team: Team::AMBER,
                souls: 9_000,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(snap.clock.elapsed_seconds(), Some(600.0));
        assert_eq!(snap.clock.playing_seconds(), Some(540.0));

        let rate = snap.souls_per_minute(Team::AMBER).expect("a rate");
        assert!((rate - 1000.0).abs() < 0.01, "{rate}");

        snap.clock.total_paused_ticks = Some(120 * 64);
        let paused_longer = snap.souls_per_minute(Team::AMBER).expect("a rate");
        assert!(paused_longer > rate, "{paused_longer} should exceed {rate}");
    }

    /// A rate over no elapsed play is absent, not enormous.
    #[test]
    fn a_rate_before_any_play_is_no_answer_rather_than_a_division_by_zero() {
        let snap = LiveSnapshot {
            clock: MatchClock {
                game_start_time: Some(100.0),
                now: Some(100.0),
                ..Default::default()
            },
            teams: vec![TeamStats {
                team: Team::AMBER,
                souls: 500,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(snap.clock.playing_seconds(), Some(0.0));
        assert_eq!(snap.souls_per_minute(Team::AMBER), None);
        assert_eq!(snap.soul_lead_per_minute(), None);
        assert_eq!(LiveSnapshot::default().souls_per_minute(Team::AMBER), None);
    }

    /// The lead rate names the same side the lead does.
    #[test]
    fn the_lead_rate_agrees_with_the_lead_about_who_is_ahead() {
        let snap = LiveSnapshot {
            clock: MatchClock {
                game_start_time: Some(0.0),
                now: Some(600.0),
                tick_rate: 64.0,
                ..Default::default()
            },
            teams: vec![
                TeamStats {
                    team: Team::AMBER,
                    souls: 5_000,
                    ..Default::default()
                },
                TeamStats {
                    team: Team::SAPPHIRE,
                    souls: 8_000,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (who, lead) = snap.soul_lead().expect("a lead");
        let (rate_who, rate) = snap.soul_lead_per_minute().expect("a rate");
        assert_eq!(who, Team::SAPPHIRE);
        assert_eq!(rate_who, who);
        assert_eq!(lead, 3_000);
        assert!((rate - 300.0).abs() < 0.01, "{rate}");
    }

    /// An empty ban list and an unreadable one are different answers.
    ///
    /// Most modes ban nothing, so `Some([])` is the ordinary result and means the read
    /// succeeded. Collapsing that to `None` would make a consumer diffing ticks read a
    /// failed read as bans being lifted - the same failure `items` and `ability_upgrades`
    /// already guard against.
    #[test]
    fn an_empty_ban_list_is_not_the_same_as_an_unread_one() {
        let read = LiveSnapshot {
            banned_heroes: Some(Vec::new()),
            ..Default::default()
        };
        assert_eq!(read.banned_heroes.as_deref(), Some(&[][..]));

        let unread = LiveSnapshot::default();
        assert_eq!(unread.banned_heroes, None);

        let drafted = LiveSnapshot {
            banned_heroes: Some(vec![HeroId(1), HeroId(15)]),
            ..Default::default()
        };
        assert_eq!(
            drafted.banned_heroes.as_deref().map(<[HeroId]>::len),
            Some(2)
        );
    }

    /// The winning team is absent until there is one.
    ///
    /// `m_iWinningTeam` reads as a non-positive number for the whole match, which would
    /// map to `Team(0)` - unassigned - and look like a real answer.
    #[test]
    fn the_winning_team_is_absent_rather_than_unassigned_before_the_match_ends() {
        assert_eq!(LiveSnapshot::default().winning_team, None);
        let won = LiveSnapshot {
            winning_team: Some(Team::SAPPHIRE),
            ..Default::default()
        };
        assert_eq!(won.winning_team, Some(Team::SAPPHIRE));
    }

    /// The phase countdown clamps instead of running negative.
    ///
    /// `m_flNextStateTime` keeps its value after the deadline passes, so subtracting a
    /// later clock from it yields a negative number that reads as a timer counting
    /// backwards rather than one that has expired.
    #[test]
    fn a_street_brawl_phase_countdown_stops_at_zero_rather_than_going_negative() {
        let b = StreetBrawl {
            next_state_time: Some(900.0),
            ..Default::default()
        };
        assert_eq!(b.seconds_until_next_state(880.0), Some(20.0));
        assert_eq!(b.seconds_until_next_state(900.0), Some(0.0));
        assert_eq!(b.seconds_until_next_state(925.5), Some(0.0));
        assert_eq!(StreetBrawl::default().seconds_until_next_state(880.0), None);
    }

    /// A zeroed controller is what every non-brawl match leaves behind.
    ///
    /// The rules class embeds it in every mode, so presence says nothing. Only a non-
    /// default value does, and even then the game mode is the signal worth trusting.
    #[test]
    fn a_zeroed_street_brawl_controller_is_not_an_active_brawl() {
        assert!(!StreetBrawl::default().is_active());
        assert!(
            !StreetBrawl {
                state_raw: Some(0),
                round: Some(0),
                amber_score: Some(0),
                ..Default::default()
            }
            .is_active()
        );
        assert!(
            StreetBrawl {
                state_raw: Some(3),
                ..Default::default()
            }
            .is_active()
        );
        assert!(
            StreetBrawl {
                round: Some(2),
                ..Default::default()
            }
            .is_active()
        );
    }

    #[test]
    fn midboss_kills_is_a_count_not_an_aliveness_flag() {
        let snap = LiveSnapshot {
            midboss_kills: Some(1),
            objectives: vec![Objective {
                kind: ObjectiveKind::Midboss,
                team_name: Some("Neutrals".into()),
                class: "C_NPC_MidBoss".into(),
                address: 0x1000,
                team: Some(Team::NEUTRAL),
                health: Some(19240),
                max_health: Some(19240),
                lane: None,
                state: StructureState::Standing,
                #[cfg(feature = "positions")]
                position: None,
            }],
            ..Default::default()
        };
        assert_eq!(snap.midboss_kills, Some(1));
        assert!(snap.midboss_alive());

        let gone = LiveSnapshot {
            midboss_kills: Some(1),
            ..Default::default()
        };
        assert!(!gone.midboss_alive());
    }

    #[test]
    fn team_names_fall_back_when_unread() {
        let mut names: HashMap<Team, String> = HashMap::new();
        names.insert(Team::AMBER, "Amber".to_string());
        let snap = LiveSnapshot {
            team_names: names,
            ..Default::default()
        };
        assert_eq!(snap.team_name(Team::AMBER), "Amber");
        assert_eq!(snap.team_name(Team::SAPPHIRE), "Sapphire");
        assert_eq!(snap.team_name(Team::NEUTRAL), "Neutrals");
        assert_eq!(snap.team_name(Team(99)), "Unknown");
        assert!(Team::AMBER.is_playing() && Team::SAPPHIRE.is_playing());
        assert!(!Team::SPECTATOR.is_playing() && !Team::NEUTRAL.is_playing());
    }

    fn row(team: Team, slot: Option<u32>, raw_level: u32) -> PlayerRow {
        PlayerRow {
            team: Some(team),
            is_spectator: team == Team::SPECTATOR,
            slot: if team == Team::SPECTATOR { None } else { slot },
            level_raw: Some(raw_level),
            level: Some(raw_level.saturating_sub(1)),
            ..Default::default()
        }
    }

    /// A spectated match shows a team-1 controller whose slot reads `1`, colliding with
    /// the first Amber player. It must not appear as a duplicate scoreboard entry.
    #[test]
    fn spectators_are_excluded_and_carry_no_slot() {
        let snap = LiveSnapshot {
            players: vec![
                row(Team::SPECTATOR, Some(1), 1),
                row(Team::AMBER, Some(1), 29),
                row(Team::SAPPHIRE, Some(7), 31),
            ],
            ..Default::default()
        };
        assert_eq!(snap.scoreboard().count(), 2);
        assert_eq!(snap.spectators().count(), 1);
        assert!(snap.spectators().next().unwrap().slot.is_none());
        let slots: Vec<_> = snap.scoreboard().map(|p| p.slot).collect();
        let mut uniq = slots.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(slots.len(), uniq.len());
    }

    /// `m_iLevel` reads one higher than the level the game's UI shows, observed twice
    /// in a live match (23 shown / 24 read, 27 shown / 28 read).
    #[test]
    fn level_is_reported_as_the_game_displays_it() {
        let p = row(Team::AMBER, Some(1), 24);
        assert_eq!(p.level_raw, Some(24));
        assert_eq!(p.level, Some(23));

        let p0 = row(Team::AMBER, Some(1), 0);
        assert_eq!(p0.level, Some(0));
    }
}

#[cfg(test)]
mod objective_timer_tests {
    use super::*;

    fn snap_with(midboss_alive: bool, spawn: Option<f32>, now: Option<f32>) -> LiveSnapshot {
        LiveSnapshot {
            midboss_next_spawn_time: spawn,
            clock: MatchClock {
                now,
                ..Default::default()
            },
            objectives: if midboss_alive {
                vec![Objective {
                    kind: ObjectiveKind::Midboss,
                    team_name: None,
                    class: "C_NPC_MidBoss".into(),
                    address: 1,
                    team: Some(Team::NEUTRAL),
                    health: Some(1),
                    max_health: Some(1),
                    lane: None,
                    state: StructureState::Standing,
                    #[cfg(feature = "positions")]
                    position: None,
                }]
            } else {
                Vec::new()
            },
            ..Default::default()
        }
    }

    /// Observed live: spawn at 1772.9 with the clock at 1583.5 is ~189s out.
    #[test]
    fn counts_down_to_the_next_midboss_spawn() {
        let s = snap_with(false, Some(1772.922), Some(1583.5));
        let d = s.seconds_until_midboss().unwrap();
        assert!((d - 189.42).abs() < 0.1, "{d}");
    }

    /// While it is on the map the scheduled time is meaningless.
    #[test]
    fn no_countdown_while_the_midboss_is_alive() {
        let s = snap_with(true, Some(1772.922), Some(1583.5));
        assert!(s.midboss_alive());
        assert_eq!(s.seconds_until_midboss(), None);
    }

    /// A stale timer must clamp rather than report a negative countdown.
    #[test]
    fn overdue_spawn_clamps_to_zero() {
        let s = snap_with(false, Some(100.0), Some(500.0));
        assert_eq!(s.seconds_until_midboss(), Some(0.0));
    }

    #[test]
    fn unscheduled_or_unclocked_yields_nothing() {
        assert_eq!(
            snap_with(false, Some(0.0), Some(10.0)).seconds_until_midboss(),
            None
        );
        assert_eq!(
            snap_with(false, None, Some(10.0)).seconds_until_midboss(),
            None
        );
        assert_eq!(
            snap_with(false, Some(100.0), None).seconds_until_midboss(),
            None
        );
    }

    #[test]
    fn soul_lead_reports_the_leader_and_margin() {
        let mk = |team, souls| TeamStats {
            team,
            souls,
            ..Default::default()
        };
        let s = LiveSnapshot {
            teams: vec![mk(Team::AMBER, 179_798), mk(Team::SAPPHIRE, 154_347)],
            ..Default::default()
        };
        assert_eq!(s.soul_lead(), Some((Team::AMBER, 25_451)));
        assert_eq!(s.team_stats(Team::SAPPHIRE).map(|t| t.souls), Some(154_347));
        assert!(s.team_stats(Team::NEUTRAL).is_none());

        let s2 = LiveSnapshot {
            teams: vec![mk(Team::AMBER, 10), mk(Team::SAPPHIRE, 40)],
            ..Default::default()
        };
        assert_eq!(s2.soul_lead(), Some((Team::SAPPHIRE, 30)));
        let s3 = LiveSnapshot {
            teams: vec![mk(Team::AMBER, 7), mk(Team::SAPPHIRE, 7)],
            ..Default::default()
        };
        assert_eq!(s3.soul_lead(), Some((Team::AMBER, 0)));

        let s4 = LiveSnapshot {
            teams: vec![mk(Team::AMBER, 5)],
            ..Default::default()
        };
        assert_eq!(s4.soul_lead(), None);
    }

    /// A spawn time long past is overdue, not imminent.
    ///
    /// Observed in a live spectate: `dlrs objectives` read `next spawn 0:00` for a full
    /// minute of match time while the midboss stayed off the map, and
    /// `m_iMidbossKillCount` went from 1 to 2 across the window - so it spawned and died
    /// while the countdown still claimed it was about to arrive.
    ///
    /// [`LiveSnapshot::seconds_until_midboss`] clamps at zero so a stale timer never reads
    /// as negative, which is right as far as it goes: the clamped value cannot mislead
    /// about direction. It does mislead about *distance*, because zero is also what a spawn
    /// happening this instant looks like, and a reader cannot tell the two apart.
    ///
    /// Which of the two causes this is - the client's schedule going stale, or the midboss
    /// being alive and undetected - is not settled here. Both are things a consumer wants
    /// named rather than smoothed into a countdown at zero.
    #[test]
    fn a_spawn_time_long_past_is_overdue_rather_than_imminent() {
        let s = snap_with(false, Some(100.0), Some(500.0));
        assert_eq!(s.midboss_schedule(), MidbossSchedule::Overdue(400.0));

        assert_eq!(s.seconds_until_midboss(), Some(0.0));
    }

    /// A spawn a tick or two past is still imminent, not overdue.
    ///
    /// Ticks are read from a running process rather than delivered, so the clock and the
    /// schedule can disagree by a fraction of a second with nothing wrong. Calling that
    /// overdue would put a warning on every spawn.
    #[test]
    fn a_spawn_a_moment_past_is_still_imminent() {
        let s = snap_with(false, Some(100.0), Some(100.5));
        assert_eq!(s.midboss_schedule(), MidbossSchedule::In(0.0));
    }

    #[test]
    fn a_scheduled_spawn_still_ahead_counts_down() {
        let s = snap_with(false, Some(500.0), Some(100.0));
        assert_eq!(s.midboss_schedule(), MidbossSchedule::In(400.0));
    }

    #[test]
    fn a_midboss_on_the_map_has_no_schedule_to_report() {
        assert_eq!(
            snap_with(true, Some(500.0), Some(100.0)).midboss_schedule(),
            MidbossSchedule::Alive
        );
    }

    /// An unread clock and an unscheduled spawn are both "nothing to say", and neither is
    /// overdue: a schedule that was never read cannot have lapsed.
    #[test]
    fn an_unscheduled_or_unclocked_spawn_is_not_overdue() {
        for s in [
            snap_with(false, Some(0.0), Some(10.0)),
            snap_with(false, None, Some(10.0)),
            snap_with(false, Some(100.0), None),
        ] {
            assert_eq!(s.midboss_schedule(), MidbossSchedule::Unscheduled);
        }
    }
}

#[cfg(test)]
mod current_player_tests {
    use super::*;

    fn row(team: Team, local: bool, observed: bool) -> PlayerRow {
        PlayerRow {
            team: Some(team),
            is_spectator: team == Team::SPECTATOR,
            is_local: Some(local),
            is_observed: observed,
            ..Default::default()
        }
    }

    /// Playing: you are the subject, and there is no observer target.
    #[test]
    fn playing_resolves_to_yourself() {
        let s = LiveSnapshot {
            players: vec![
                row(Team::AMBER, true, false),
                row(Team::SAPPHIRE, false, false),
            ],
            ..Default::default()
        };
        let me = s.current_player().expect("current");
        assert_eq!(me.team, Some(Team::AMBER));
        assert_eq!(me.is_local, Some(true));
        assert!(s.observed_player().is_none());
    }

    /// Spectating: the local controller sits on the spectator team with no hero, so the
    /// camera target is the useful answer.
    #[test]
    fn spectating_resolves_to_the_observed_player() {
        let s = LiveSnapshot {
            players: vec![
                row(Team::SPECTATOR, true, false),
                row(Team::AMBER, false, true),
                row(Team::SAPPHIRE, false, false),
            ],
            ..Default::default()
        };
        assert_eq!(
            s.local_player().map(|p| p.team),
            Some(Some(Team::SPECTATOR))
        );
        let cur = s.current_player().expect("current");
        assert_eq!(cur.team, Some(Team::AMBER));
        assert!(cur.is_observed);
    }

    /// Dead and watching a teammate: you are still the subject for stats purposes, but
    /// the observed player remains reachable.
    #[test]
    fn dead_player_watching_a_teammate_still_reports_themselves() {
        let s = LiveSnapshot {
            players: vec![row(Team::AMBER, true, false), row(Team::AMBER, false, true)],
            ..Default::default()
        };
        assert_eq!(s.current_player().map(|p| p.is_local), Some(Some(true)));
        assert_eq!(s.observed_player().map(|p| p.is_local), Some(Some(false)));
    }

    /// Free camera or replay with no target: nothing to report, rather than a wrong
    /// row.
    #[test]
    fn no_local_and_no_target_yields_nothing() {
        let s = LiveSnapshot {
            players: vec![row(Team::AMBER, false, false)],
            ..Default::default()
        };
        assert!(s.current_player().is_none());
        assert_eq!(s.perspective, Perspective::Unknown);
    }

    #[test]
    fn empty_snapshot_is_safe() {
        let s = LiveSnapshot::default();
        assert!(s.current_player().is_none());
        assert!(s.observed_player().is_none());
        assert!(s.local_player().is_none());
    }
}

#[cfg(test)]
mod midboss_detail_tests {
    use super::*;
    use crate::timers::{MidbossState, Timers};

    fn snap(state: MidbossState) -> LiveSnapshot {
        LiveSnapshot {
            timers: Timers {
                midboss_state: state,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_snapshot_surfaces_the_midboss_health_read_off_the_live_entity() {
        let s = snap(MidbossState {
            health: Some(4_810),
            max_health: Some(19_240),
        });
        assert_eq!(s.midboss_health(), Some((4_810, 19_240)));
        assert!((s.midboss_health_fraction().unwrap() - 0.25).abs() < 0.001);
    }

    /// No Midboss on the map means no health, not zero health.
    #[test]
    fn a_snapshot_with_no_midboss_reports_no_health_rather_than_zero() {
        let s = LiveSnapshot::default();
        assert!(!s.midboss_alive());
        assert_eq!(s.midboss_health(), None);
        assert_eq!(s.midboss_health_fraction(), None);
    }
}
