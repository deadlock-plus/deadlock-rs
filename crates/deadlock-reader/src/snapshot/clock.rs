//! Match clock and pause state.

use crate::entity::EntitySnapshot;
use crate::reader::Reader;
use crate::snapshot::BASE_ENTITY;
use crate::tunables::{DEFAULT_TICK_RATE, class_in};

/// Everything the game exposes about the pause state.
///
/// Several pause fields exist and it is not obvious which one a spectating client sees
/// replicated, so all of them are surfaced and [`PauseState::is_paused`] combines them
/// rather than trusting one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PauseState {
    /// `m_bGamePaused` (`@0x38`), adjacent to the pause tick counters.
    pub game_paused: Option<bool>,
    /// `m_bServerPaused` (`@0x9f80`), in the pause-request block.
    pub server_paused: Option<bool>,
    /// `m_nPauseStartTick`; non-zero while a pause is in effect, reset on unpause.
    pub pause_start_tick: Option<u32>,
    /// `m_nTotalPausedTicks`, cumulative across the match.
    pub total_paused_ticks: Option<u32>,
    /// `m_iPauseTeam`; `0xFFFFFFFF` when nobody has paused.
    pub pause_team: Option<u32>,
    /// `m_pausingPlayerId`; `0xFFFFFFFF` when nobody has paused.
    pub pausing_player_id: Option<u32>,
}

impl PauseState {
    /// Whether the match is currently paused.
    ///
    /// True if any of the three independent signals says so. A spectating client did not
    /// see `m_bServerPaused` flip during an observed pause, so relying on one field
    /// misses real pauses; `m_nPauseStartTick` becoming non-zero is the most direct
    /// evidence.
    pub fn is_paused(&self) -> bool {
        self.game_paused.unwrap_or(false)
            || self.server_paused.unwrap_or(false)
            || self.pause_start_tick.unwrap_or(0) != 0
    }

    /// Whether any pause has happened this match.
    pub fn ever_paused(&self) -> bool {
        self.total_paused_ticks.unwrap_or(0) > 0
    }
}

/// The match clock.
///
/// `C_CitadelGameRules::m_flMatchClockAtLastUpdate` looked authoritative but was
/// observed frozen across a live match, so the working clock is derived instead:
/// a live entity's `m_flSimulationTime` advances in real time, and `m_flGameStartTime`
/// marks when the match began.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MatchClock {
    /// `m_flGameStartTime`, the engine time at which the match started.
    pub game_start_time: Option<f32>,
    /// Current engine time: the highest `m_flSimulationTime` across several
    /// always-simulated entities, so one stalled entity cannot freeze the clock.
    pub now: Option<f32>,
    /// `m_nTotalPausedTicks`, cumulative across the match.
    pub total_paused_ticks: Option<u32>,
    /// `m_flGameStateStartTime`, the engine time the current [`GameState`] began.
    ///
    /// [`GameState`]: crate::snapshot::GameState
    pub game_state_start_time: Option<f32>,
    /// `m_flGameStateEndTime`, the engine time the current game state is scheduled to
    /// end.
    ///
    /// Only meaningful for states the game actually times out, such as the pre-game and
    /// hero-pick countdowns; it is `0` or stale during `GameInProgress`.
    /// [`MatchClock::seconds_until_game_state_ends`] filters for that.
    pub game_state_end_time: Option<f32>,
    /// Raw `m_flMatchClockAtLastUpdate`. Observed stale; kept for completeness.
    pub raw_match_clock: Option<f32>,
    /// Raw `m_nMatchClockUpdateTick`.
    pub raw_update_tick: Option<u32>,
    /// Tick rate used to convert paused ticks to seconds. Copied from
    /// [`Tunables::tick_rate`](crate::Tunables) when the clock is read.
    pub tick_rate: f32,
}

impl Default for MatchClock {
    fn default() -> Self {
        MatchClock {
            game_start_time: None,
            now: None,
            total_paused_ticks: None,
            game_state_start_time: None,
            game_state_end_time: None,
            raw_match_clock: None,
            raw_update_tick: None,
            tick_rate: DEFAULT_TICK_RATE,
        }
    }
}

impl MatchClock {
    /// Seconds since the match started, pauses included.
    pub fn elapsed_seconds(&self) -> Option<f32> {
        Some(self.now? - self.game_start_time?)
    }

    /// Seconds of actual play, with paused time removed.
    pub fn playing_seconds(&self) -> Option<f32> {
        let paused = self.total_paused_ticks.unwrap_or(0) as f32 / self.tick_rate;
        Some((self.elapsed_seconds()? - paused).max(0.0))
    }

    /// The match clock as `m:ss`, the way the game shows it.
    ///
    /// This is [`MatchClock::playing_seconds`], not [`MatchClock::elapsed_seconds`]: the
    /// in-game clock stops while the match is paused. Use
    /// [`MatchClock::elapsed_display`] for wall time since the match started.
    pub fn display(&self) -> Option<String> {
        Self::mmss(self.playing_seconds()?)
    }

    /// Wall time since the match started, pauses included, as `m:ss`.
    pub fn elapsed_display(&self) -> Option<String> {
        Self::mmss(self.elapsed_seconds()?)
    }

    fn mmss(secs: f32) -> Option<String> {
        let s = secs.max(0.0) as u32;
        Some(format!("{}:{:02}", s / 60, s % 60))
    }

    /// Seconds left in the current game state, for the states that are on a timer.
    ///
    /// This is the countdown behind the pre-game and hero-pick screens. `None` during
    /// `GameInProgress`, which has no scheduled end; a past-or-zero end time is reported
    /// as "not on a timer" rather than as a negative duration.
    pub fn seconds_until_game_state_ends(&self) -> Option<f32> {
        let end = self.game_state_end_time.filter(|t| *t > 0.0)?;
        let remaining = end - self.now?;
        (remaining > 0.0).then_some(remaining)
    }

    /// Seconds the current game state has been running.
    pub fn seconds_in_game_state(&self) -> Option<f32> {
        Some((self.now? - self.game_state_start_time?).max(0.0))
    }
}

/// Read the match clock.
///
/// `now` is the highest `m_flSimulationTime` across the configured clock sources, not
/// the first one found. Any individual entity can stop being simulated (a dead or
/// out-of-PVS pawn keeps its last value indefinitely), and reading whichever pawn came
/// first in entity order let one frozen sample stall the match clock and the midboss
/// timer with it. A maximum over several always-present entities cannot go backwards.
pub(crate) fn read_clock(
    reader: &Reader,
    entities: &EntitySnapshot,
    rules: &crate::reader::Object<'_>,
) -> MatchClock {
    const RULES: &str = "C_CitadelGameRules";

    let tunables = reader.tunables();
    let now = entities
        .all()
        .iter()
        .filter(|e| class_in(&tunables.clock_source_classes, e.best_name()))
        .filter_map(|e| reader.field_f32(e.instance, BASE_ENTITY, "m_flSimulationTime"))
        .filter(|t| t.is_finite())
        .reduce(f32::max);

    // Every field below comes out of the one buffer the snapshot already read, so the
    // two values shared with `LiveSnapshot` cost nothing to read twice.
    MatchClock {
        game_start_time: rules.f32(RULES, "m_flGameStartTime"),
        now,
        total_paused_ticks: rules.u32(RULES, "m_nTotalPausedTicks"),
        game_state_start_time: rules.f32(RULES, "m_flGameStateStartTime"),
        game_state_end_time: rules.f32(RULES, "m_flGameStateEndTime"),
        raw_match_clock: rules.f32(RULES, "m_flMatchClockAtLastUpdate"),
        raw_update_tick: rules.u32(RULES, "m_nMatchClockUpdateTick"),
        tick_rate: tunables.tick_rate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_derives_elapsed_from_simulation_time() {
        let c = MatchClock {
            game_start_time: Some(65.109),
            now: Some(1566.0),
            total_paused_ticks: Some(323),
            raw_match_clock: Some(575.5),
            raw_update_tick: Some(41323),
            ..Default::default()
        };
        let elapsed = c.elapsed_seconds().unwrap();
        assert!((elapsed - 1500.891).abs() < 0.01, "{elapsed}");
        let playing = c.playing_seconds().unwrap();
        assert!((elapsed - playing - 5.047).abs() < 0.01, "{playing}");
        assert_eq!(c.display().as_deref(), Some("24:55"));
        assert_eq!(c.elapsed_display().as_deref(), Some("25:00"));
    }

    #[test]
    fn clock_is_none_without_both_ends() {
        let c = MatchClock {
            now: Some(100.0),
            ..Default::default()
        };
        assert!(c.elapsed_seconds().is_none());
        assert!(c.display().is_none());
    }

    #[test]
    fn playing_time_never_goes_negative() {
        let c = MatchClock {
            game_start_time: Some(100.0),
            now: Some(101.0),
            total_paused_ticks: Some(u32::MAX),
            ..Default::default()
        };
        assert_eq!(c.playing_seconds(), Some(0.0));
    }

    #[test]
    fn tick_rate_override_changes_pause_conversion() {
        let c = MatchClock {
            game_start_time: Some(0.0),
            now: Some(100.0),
            total_paused_ticks: Some(640),
            tick_rate: 32.0,
            ..Default::default()
        };
        assert_eq!(c.playing_seconds(), Some(80.0));
    }

    /// A spectating client did not see `m_bServerPaused` flip during a real pause, so
    /// any one signal is enough to call it paused.
    #[test]
    fn pause_is_detected_from_any_signal() {
        let none = PauseState {
            game_paused: Some(false),
            server_paused: Some(false),
            pause_start_tick: Some(0),
            total_paused_ticks: Some(3011),
            ..Default::default()
        };
        assert!(!none.is_paused());
        assert!(none.ever_paused());

        for s in [
            PauseState {
                game_paused: Some(true),
                ..none
            },
            PauseState {
                server_paused: Some(true),
                ..none
            },
            PauseState {
                pause_start_tick: Some(91_234),
                ..none
            },
        ] {
            assert!(s.is_paused(), "{s:?}");
        }

        assert!(!PauseState::default().is_paused());
    }
}
