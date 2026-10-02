//! What a pollable source of events is, and how it reports its own health.

use std::time::Duration;

use deadlock_reader::events::Event as ReaderEvent;

#[cfg(feature = "crowd-control")]
use crate::crowd_control::CrowdControlEvent;
use crate::party::PartyEvent;
use crate::postgame::PostGameEvent;

/// Something that changed, tagged by where it came from.
///
/// Kept as one enum so a consumer can take a single stream, but each variant wraps its
/// source's own type rather than flattening everything: the reader and the Game
/// Coordinator have nothing in common beyond both being polled, and pretending otherwise
/// would produce an enum where most variants are meaningless to most consumers.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// Something changed in the live match.
    Match(ReaderEvent),
    /// Something changed about the party.
    Party(PartyEvent),
    /// A finished match's metadata was captured, or could not be.
    PostGame(PostGameEvent),
    /// A player took crowd control, or the crowd control they were under finished.
    ///
    /// Behind the non-default `crowd-control` feature, which is what makes the modifier
    /// lists this is derived from readable in the first place.
    #[cfg(feature = "crowd-control")]
    CrowdControl(CrowdControlEvent),
}

/// How a source is currently faring.
///
/// Reported per source rather than globally. "The party data is stale because the sweep
/// found nothing" and "the game closed" are different situations and a consumer usually
/// wants to act differently on them, so collapsing both into one status is the kind of
/// thing that is painful to undo later.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Health {
    /// Polling normally.
    Ok,
    /// Working, but not returning what it looks for.
    ///
    /// The party source reports this when no party is resident, which is the ordinary
    /// state for someone playing solo and is not an error.
    Idle(String),
    /// Working, but some of what it returns is likely wrong.
    ///
    /// The match source reports this when the reader notices drift that corrupts rather
    /// than merely omits: a field resolved from a stale baked offset, or an enum the
    /// running game numbers differently. Values still arrive; do not trust them.
    Degraded(String),
    /// A poll failed. The engine keeps the source alive and retries.
    Failed(String),
}

impl Health {
    /// Whether this state is worth telling a user about.
    ///
    /// [`Health::Idle`] deliberately is not: playing solo means no party is resident,
    /// which is the ordinary state and not a fault. Without this, a consumer has to match
    /// all four variants to find out, and the easy mistake is to treat "not `Ok`" as
    /// "broken" - which reports a problem to someone who does not have one.
    pub fn is_problem(&self) -> bool {
        matches!(self, Health::Degraded(_) | Health::Failed(_))
    }

    /// Whether values from this source can be believed.
    ///
    /// [`Health::Degraded`] is the only state where data arrives *and* is wrong, which is
    /// what makes it the one worth alerting on. [`Health::Failed`] produces nothing to
    /// distrust.
    pub fn data_is_trustworthy(&self) -> bool {
        !matches!(self, Health::Degraded(_))
    }
}

/// One poll's worth of output.
#[derive(Clone, Debug, Default)]
pub struct Poll {
    /// Events observed this tick.
    pub events: Vec<Event>,
    /// The source's health after this tick, when it changed.
    ///
    /// `None` means unchanged, so a healthy source does not emit a status every tick.
    pub health: Option<Health>,
}

impl Poll {
    /// A tick that produced nothing and changed nothing.
    pub fn quiet() -> Self {
        Poll::default()
    }

    /// A tick that produced events.
    pub fn events(events: Vec<Event>) -> Self {
        Poll {
            events,
            health: None,
        }
    }

    /// A tick that changed the source's health.
    pub fn health(health: Health) -> Self {
        Poll {
            events: Vec::new(),
            health: Some(health),
        }
    }
}

/// A pollable source of events, driven on its own schedule.
///
/// Each source runs on its own thread. That is not incidental: a cold Game Coordinator
/// sweep measured **1.2 seconds** against a match source polled every **50 to 100 ms**,
/// and the window in which a finished match can still be read is **12 to 19 seconds**.
/// Sharing one thread would drop a dozen match ticks per sweep, so the two cadences have
/// to be genuinely independent.
pub trait Source: Send {
    /// Short stable name, used to tag events and health reports.
    fn name(&self) -> &'static str;

    /// How long to wait between polls.
    ///
    /// Read once when the engine starts the source.
    fn interval(&self) -> Duration;

    /// Do one unit of work.
    ///
    /// See [`PollError`] for what the engine does with each kind of failure.
    fn poll(&mut self) -> Result<Poll, PollError>;
}

/// Why a poll failed, and whether polling again could help.
///
/// This was a bare `String`, which left the engine no way to tell "the game is not running
/// yet" from "this source can never work" - so it retried both forever, and a source with a
/// permanent problem reported the same failure every interval for the life of the process.
/// A downstream implementor had no way to say otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PollError {
    /// The source could not reach what it reads this time.
    ///
    /// The game is not running, a handle went stale, a scan found nothing. The engine
    /// reports it as [`Health::Failed`] and polls again next interval. This is the common
    /// case and the right default when unsure.
    Transient(String),
    /// The source cannot work at all, and polling again will fail the same way.
    ///
    /// A missing capability, a configuration that cannot be satisfied. The engine reports
    /// it once and stops polling *this* source; the rest of the engine carries on.
    Fatal(String),
}

impl PollError {
    /// The message, whichever kind this is.
    pub fn message(&self) -> &str {
        match self {
            PollError::Transient(m) | PollError::Fatal(m) => m,
        }
    }

    /// Whether polling again could plausibly succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, PollError::Transient(_))
    }
}

impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PollError::Transient(m) => write!(f, "{m}"),
            PollError::Fatal(m) => write!(f, "{m} (not retryable)"),
        }
    }
}

impl std::error::Error for PollError {}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counter(u32);

    impl Source for Counter {
        fn name(&self) -> &'static str {
            "counter"
        }
        fn interval(&self) -> Duration {
            Duration::from_millis(1)
        }
        fn poll(&mut self) -> Result<Poll, PollError> {
            self.0 += 1;
            Ok(Poll::quiet())
        }
    }

    #[test]
    fn a_quiet_poll_reports_nothing() {
        let p = Poll::quiet();
        assert!(p.events.is_empty());
        assert!(
            p.health.is_none(),
            "silence must not emit a status every tick"
        );
    }

    #[test]
    fn sources_are_object_safe() {
        let mut s: Box<dyn Source> = Box::new(Counter(0));
        assert_eq!(s.name(), "counter");
        assert!(s.poll().is_ok());
    }

    /// This used to be `assert_ne!(Degraded, Ok)` and friends, which asserts that a
    /// derived `PartialEq` distinguishes two variants of an enum - true of every enum ever
    /// written, and nothing at all about this one. What actually matters is which states a
    /// consumer should act on.
    #[test]
    fn only_degraded_and_failed_are_problems() {
        assert!(!Health::Ok.is_problem());
        assert!(
            !Health::Idle("no party resident".into()).is_problem(),
            "solo play is the ordinary state, not a fault to report"
        );
        assert!(Health::Degraded("stale offset".into()).is_problem());
        assert!(Health::Failed("no game".into()).is_problem());
    }

    /// Degraded is the one that matters and the easiest to get wrong: the source is
    /// returning data and the data is untrustworthy. Failed returns nothing, so there is
    /// nothing to distrust - collapsing the two loses that distinction.
    #[test]
    fn degraded_is_the_only_state_whose_data_is_suspect() {
        assert!(Health::Ok.data_is_trustworthy());
        assert!(Health::Idle("nothing yet".into()).data_is_trustworthy());
        assert!(Health::Failed("no game".into()).data_is_trustworthy());
        assert!(!Health::Degraded("stale offset".into()).data_is_trustworthy());
    }
}
