//! The two concrete sources: the live match, and the party.

use std::sync::Arc;
use std::time::Duration;

use deadlock_memory::MemoryReader;
use deadlock_reader::Reader;
use deadlock_reader::cache::SnapshotCache;
use deadlock_reader::events::EventTracker;
use deadlock_reader::supervise::{Attached, DEFAULT_RETRY_INTERVAL, ReaderSupervisor};
use deadlock_walker::GcSession;
use valveprotos::deadlock::CsoCitadelParty;

#[cfg(feature = "crowd-control")]
use crate::crowd_control::CrowdControlTracker;
use crate::party::PartyTracker;
use crate::source::{Event, Health, Poll, PollError, Source};

/// Default poll interval for the live match.
///
/// Comfortably inside the 12 to 19 second window in which a finished match is still
/// readable, and a snapshot costs a few hundred microseconds, so this is cheap.
pub const DEFAULT_MATCH_INTERVAL: Duration = Duration::from_millis(100);

/// Default poll interval for the party.
///
/// A pinned re-read measured about 30 microseconds, so this is not about cost. Party
/// membership simply does not change often enough to look at it more than this.
pub const DEFAULT_PARTY_INTERVAL: Duration = Duration::from_secs(2);

/// A poll interval of zero is a busy loop, not a fast poll.
///
/// [`DEFAULT_MATCH_INTERVAL`] paces the reader thread. At zero it would re-read the whole
/// snapshot as fast as the OS allows and burn a core, which is the sort of default that
/// looks perfect in a short test and ruins a machine over an evening. The upper bound is
/// the other half: this has to stay comfortably inside the 12-to-19 second window in which
/// a finished match is still readable, or the end of a match would be missed entirely.
///
/// Checked at compile time, because both sides are constants and a paced loop that has
/// stopped pacing should not depend on a test being run.
const _: () = assert!(!DEFAULT_MATCH_INTERVAL.is_zero());
const _: () = assert!(DEFAULT_MATCH_INTERVAL.as_millis() <= 2_000);

/// Live match state: kills, objectives, phase changes, and the end of a match.
pub struct ReaderSource {
    supervisor: ReaderSupervisor,
    tracker: EventTracker,
    /// Accumulates crowd control taken, which no snapshot totals on its own.
    #[cfg(feature = "crowd-control")]
    crowd_control: CrowdControlTracker,
    cache: Option<SnapshotCache>,
    interval: Duration,
}

impl ReaderSource {
    /// The name this source tags its notifications with.
    ///
    /// A constant rather than a literal buried in the trait impl, so a consumer routing on
    /// the name can match this instead of retyping the string.
    pub const NAME: &'static str = "match";

    /// A match source over an attached reader.
    ///
    /// The reader is supervised rather than merely held: if the game exits, this source
    /// reattaches when it comes back. Holding a bare `Arc<Reader>` meant every poll after
    /// the game closed failed forever, with no path back short of rebuilding the engine.
    ///
    /// See [`ReaderSource::NAME`] for the name it tags notifications with.
    pub fn new(reader: Arc<Reader>) -> Self {
        ReaderSource {
            supervisor: ReaderSupervisor::with_reader(reader, DEFAULT_RETRY_INTERVAL),
            tracker: EventTracker::new(),
            #[cfg(feature = "crowd-control")]
            crowd_control: CrowdControlTracker::new(),
            cache: None,
            interval: DEFAULT_MATCH_INTERVAL,
        }
    }

    /// A match source that attaches itself, and keeps trying until the game appears.
    pub fn attaching() -> Self {
        ReaderSource {
            supervisor: ReaderSupervisor::new(DEFAULT_RETRY_INTERVAL),
            tracker: EventTracker::new(),
            #[cfg(feature = "crowd-control")]
            crowd_control: CrowdControlTracker::new(),
            cache: None,
            interval: DEFAULT_MATCH_INTERVAL,
        }
    }

    /// Set the poll interval.
    ///
    /// Keep this well under ten seconds. The final state of a match is only readable for
    /// 12 to 19 seconds after it ends, and it is captured on the tick that observes the
    /// transition, so a slow interval loses it with no way to recover it afterwards.
    pub fn every(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Reuse pinned entities between full walks.
    ///
    /// Cheaper, at the cost of entity *presence* lagging by the cache's refresh window.
    /// Spawn-driven events (a Midboss or Urn appearing) are delayed by that much;
    /// destruction is not affected.
    pub fn cached(mut self, cache: SnapshotCache) -> Self {
        self.cache = Some(cache);
        self
    }
}

impl Source for ReaderSource {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn poll(&mut self) -> Result<Poll, PollError> {
        let reader = match self.supervisor.acquire() {
            Attached::Fresh(r) | Attached::Held(r) => r,
            Attached::Absent | Attached::Waiting => {
                // Nothing to read from. Idle rather than failed: the game not being open
                // is not a fault, and the source will pick it up when it appears.
                return Ok(Poll::health(Health::Idle("no client attached".into())));
            }
            // Transient: attaching can fail for reasons that go away - the game
            // starting up, a handle briefly denied - and `ReaderSupervisor` already
            // reports each distinct reason only once.
            Attached::Failed(why) => return Err(PollError::Transient(why)),
        };

        let taken = match self.cache.as_mut() {
            Some(cache) => reader.live_snapshot_cached(cache),
            None => reader.live_snapshot(),
        };
        let snapshot = match taken {
            Ok(s) => {
                self.supervisor.succeeded();
                s
            }
            Err(e) => {
                // A run of failures means the game went away; one is a hiccup. Either way
                // the caller is told, but only the run costs the reader.
                if self.supervisor.failed() {
                    if let Some(c) = self.cache.as_mut() {
                        c.invalidate();
                    }
                    self.tracker.reset();
                    // A gap in polling leaves open crowd-control applications with no
                    // observed end, so they are dropped rather than credited a guess.
                    #[cfg(feature = "crowd-control")]
                    self.crowd_control.reset();
                }
                return Err(PollError::Transient(e.to_string()));
            }
        };

        let Some(snapshot) = snapshot else {
            // No game-rules entity: sitting in a menu, not a failure.
            return Ok(Poll::health(Health::Idle("not in a match".into())));
        };

        // Drift that corrupts outranks a clean poll: the snapshot arrived, and part of
        // it is probably wrong. A consumer that only ever sees `Ok` would have no way to
        // learn that without asking the reader directly.
        let health = match snapshot.drift.iter().find(|d| d.is_corrupting()) {
            Some(d) => Health::Degraded(d.to_string()),
            None => Health::Ok,
        };

        #[cfg_attr(not(feature = "crowd-control"), allow(unused_mut))]
        let mut events: Vec<Event> = self
            .tracker
            .update(&snapshot)
            .into_iter()
            .map(Event::Match)
            .collect();
        #[cfg(feature = "crowd-control")]
        events.extend(
            self.crowd_control
                .update(&snapshot)
                .into_iter()
                .map(Event::CrowdControl),
        );
        Ok(Poll {
            events,
            health: Some(health),
        })
    }
}

/// Party membership, read out of the Game Coordinator's shared objects.
///
/// This is the source that answers "who am I partied with" in situations the entity
/// system cannot: during a match, after someone leaves, and when a former member is no
/// longer in the same game.
pub struct PartySource {
    reader: Arc<Reader>,
    party: PartyReader,
    tracker: PartyTracker,
    interval: Duration,
}

/// Reads the party through a [`GcSession`] built on first use.
///
/// Building a session parses `client.dll`, which needs the game to be up, so a source made
/// before the game is must not fail at construction; the first poll that can build it does.
struct PartyReader {
    account_id: u32,
    sweep_interval: Option<Duration>,
    session: Option<GcSession>,
}

impl PartyReader {
    fn new(account_id: u32) -> Self {
        PartyReader {
            account_id,
            sweep_interval: None,
            session: None,
        }
    }

    fn read(&mut self, mem: &dyn MemoryReader) -> Result<Option<CsoCitadelParty>, PollError> {
        let transient = |e: deadlock_walker::Error| PollError::Transient(e.to_string());
        let session = match &mut self.session {
            Some(session) => session,
            slot => {
                let mut session = GcSession::new(mem, self.account_id).map_err(transient)?;
                if let Some(interval) = self.sweep_interval {
                    session = session.every(interval);
                }
                slot.insert(session)
            }
        };
        let party = session.party(mem);
        if matches!(party, Err(deadlock_walker::Error::WrongProcess)) {
            self.session = None;
        }
        party.map_err(transient)
    }
}

impl PartySource {
    /// The name this source tags its notifications with.
    ///
    /// A constant rather than a literal buried in the trait impl, so a consumer routing on
    /// the name can match this instead of retyping the string - and so a test can compare
    /// the two names without needing a live `Reader` to construct a source first.
    pub const NAME: &'static str = "party";

    /// A party source for the given Steam account id.
    ///
    /// The account id is what the party has to list as a member, so it has to be the local
    /// account. Get it from [`deadlock_reader::steam::active_account_id`].
    pub fn new(reader: Arc<Reader>, account_id: u32) -> Self {
        PartySource {
            reader,
            party: PartyReader::new(account_id),
            tracker: PartyTracker::new(),
            interval: DEFAULT_PARTY_INTERVAL,
        }
    }

    /// Set the poll interval for pinned re-reads.
    pub fn every(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Set how often the expensive full sweep may run.
    ///
    /// A cold sweep measured about 1.2 seconds, and a re-find near the last known address
    /// about 0.3. It only happens when there is no live pin, but since a shared object is
    /// likely reallocated when it changes, that is precisely when the party changed. This
    /// interval therefore bounds how late a membership change can be reported.
    pub fn sweep_every(mut self, interval: Duration) -> Self {
        self.party.sweep_interval = Some(interval);
        self.party.session = None;
        self
    }
}

impl Source for PartySource {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn poll(&mut self) -> Result<Poll, PollError> {
        let party = self.party.read(self.reader.memory())?;

        let events = self
            .tracker
            .update(party.as_ref())
            .into_iter()
            .map(Event::Party)
            .collect();

        // No party is the ordinary state for solo play, not a failure.
        let health = match &party {
            Some(_) => Health::Ok,
            None => Health::Idle("no party resident".into()),
        };
        Ok(Poll {
            events,
            health: Some(health),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_intervals_respect_the_measured_constraints() {
        assert!(DEFAULT_MATCH_INTERVAL < Duration::from_secs(10));
        assert!(DEFAULT_PARTY_INTERVAL >= Duration::from_secs(1));
    }

    /// This used to read `assert_ne!("match", "party")`, which compares two literals and
    /// would keep passing after either source was renamed to match the other. Comparing
    /// the constants the sources actually return is the same assertion about the code
    /// rather than about the test.
    #[test]
    fn sources_name_themselves_distinctly() {
        assert_ne!(ReaderSource::NAME, PartySource::NAME);
        assert!(!ReaderSource::NAME.is_empty());
        assert!(!PartySource::NAME.is_empty());
    }

    /// The game not being up yet is the ordinary state before a match, and the session is
    /// built on the first poll that can, so a failure must be retryable rather than fatal.
    #[test]
    fn a_missing_client_is_a_transient_failure_and_is_retried() {
        let mem = deadlock_memory::mock::MockMemory::new(1);
        let mut reader = PartyReader::new(1);
        for _ in 0..2 {
            let err = reader.read(&mem).unwrap_err();
            assert!(err.is_transient(), "{err}");
            assert!(reader.session.is_none());
        }
    }
}
