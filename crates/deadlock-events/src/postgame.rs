//! Capturing a finished match's metadata while the client still holds it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use deadlock_memory::MemoryReader;
use deadlock_reader::supervise::{
    Attached, DEFAULT_RETRY_INTERVAL as ATTACH_RETRY, ReaderSupervisor,
};
use deadlock_reader::{GameState, Reader};
use deadlock_walker::GcSession;
use valveprotos::deadlock::CMsgMatchMetaDataContents;

use crate::source::{Event, Health, Poll, PollError, Source};

/// How long after the phase becomes `PostGame` the first read is made.
///
/// The client does not have the metadata at the instant the phase flips; the reference
/// implementation's first attempt came about 5 s in and its capture about 9 s in. Asking
/// earlier only spends heap searches on an object that is not there yet.
pub const DEFAULT_FIRST_ATTEMPT_DELAY: Duration = Duration::from_secs(5);

/// How long to wait between reads once the first has come back empty.
pub const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// How long after `PostGame` to keep trying before reporting the match as missed.
///
/// Well past the observed capture time of about 9 s, and short enough that a match which
/// never becomes resident (a different match's post-game screen is up) is reported while
/// the user still cares.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(60);

/// What happened to the capture of one match.
///
/// Per match: one `Captured` and any number of `Updated`, or a single `Missed`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum PostGameEvent {
    /// A complete copy of the match's metadata was read.
    Captured {
        /// The match the metadata belongs to.
        match_id: u64,
        /// The metadata, as the client holds it.
        metadata: Box<CMsgMatchMetaDataContents>,
    },
    /// A later read of an already captured match differs from the last emitted copy.
    ///
    /// The client can fill fields in after the first complete read; this carries the full
    /// object as it now stands.
    Updated {
        /// The match the metadata belongs to.
        match_id: u64,
        /// The metadata, as the client holds it now.
        metadata: Box<CMsgMatchMetaDataContents>,
    },
    /// The deadline passed without a complete copy turning up.
    ///
    /// The metadata may simply never have been resident: the client only keeps the match
    /// whose post-game screen is open.
    Missed {
        /// The match that was being waited for.
        match_id: u64,
        /// How many reads were made, failed ones included.
        attempts: u32,
    },
}

impl PostGameEvent {
    /// The match this outcome is about.
    pub fn match_id(&self) -> u64 {
        match self {
            PostGameEvent::Captured { match_id, .. }
            | PostGameEvent::Updated { match_id, .. }
            | PostGameEvent::Missed { match_id, .. } => *match_id,
        }
    }
}

/// Timing for a capture window, measured from the moment `PostGame` is first seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureTiming {
    /// Delay before the first read.
    pub first_attempt_delay: Duration,
    /// Gap between later reads.
    pub retry_interval: Duration,
    /// Total time before the match is reported missed.
    pub deadline: Duration,
}

impl Default for CaptureTiming {
    fn default() -> Self {
        CaptureTiming {
            first_attempt_delay: DEFAULT_FIRST_ATTEMPT_DELAY,
            retry_interval: DEFAULT_RETRY_INTERVAL,
            deadline: DEFAULT_DEADLINE,
        }
    }
}

#[derive(Debug)]
struct Pending {
    match_id: u64,
    deadline: Instant,
    next_attempt: Instant,
    attempts: u32,
    /// The last copy emitted; later reads are compared against it.
    last: Option<CMsgMatchMetaDataContents>,
}

/// The capture policy, with no IO: phase and match id in, at most one outcome out.
///
/// The caller supplies the clock and the fetcher, so the whole policy runs under test
/// without sleeping or touching a process.
#[derive(Debug, Default)]
pub struct CapturePolicy {
    timing: CaptureTiming,
    /// The last real match id seen. The id can read zero once the match ends, so it is
    /// kept from while the match was live.
    remembered: Option<u64>,
    /// The last match whose window closed, either way. Stops a phase that flaps back into
    /// `PostGame` from opening a second window for the same match.
    settled: Option<u64>,
    pending: Option<Pending>,
}

impl CapturePolicy {
    /// A policy with no history.
    pub fn new(timing: CaptureTiming) -> Self {
        CapturePolicy {
            timing,
            ..Self::default()
        }
    }

    /// Forget everything, as when the game process was replaced.
    pub fn reset(&mut self) {
        self.remembered = None;
        self.settled = None;
        self.pending = None;
    }

    /// Whether a capture window is open.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Feed one observation.
    ///
    /// `phase` is `None` when no snapshot could be taken; that changes nothing. `fetch` is
    /// called at most once, with the match id to look for. Its error is returned after the
    /// attempt has been counted, so a failing read still runs out the deadline.
    pub fn step<E>(
        &mut self,
        phase: Option<GameState>,
        match_id: Option<u64>,
        now: Instant,
        fetch: impl FnOnce(u64) -> Result<Option<CMsgMatchMetaDataContents>, E>,
    ) -> Result<Option<PostGameEvent>, E> {
        let match_id = match_id.filter(|id| *id != 0);
        match (match_id, phase) {
            (Some(id), _) => self.remembered = Some(id),
            // A real match that ended may read zero in its final phases; anything else
            // reading zero (the Hideout, a lobby) is no match at all.
            (None, Some(p)) if !p.is_over() => self.remembered = None,
            _ => {}
        }

        let mut displaced = None;
        if phase == Some(GameState::PostGame)
            && let Some(id) = self.remembered
            && self.settled != Some(id)
            && self.pending.as_ref().map(|p| p.match_id) != Some(id)
        {
            displaced = self.pending.take().and_then(|p| {
                p.last.is_none().then_some(PostGameEvent::Missed {
                    match_id: p.match_id,
                    attempts: p.attempts,
                })
            });
            self.pending = Some(Pending {
                match_id: id,
                deadline: now + self.timing.deadline,
                next_attempt: now + self.timing.first_attempt_delay,
                attempts: 0,
                last: None,
            });
        }
        if let Some(missed) = displaced {
            self.settled = Some(missed.match_id());
            return Ok(Some(missed));
        }

        let Some(p) = self.pending.as_mut() else {
            return Ok(None);
        };

        let mut failure = None;
        if now >= p.next_attempt {
            p.attempts += 1;
            p.next_attempt = now + self.timing.retry_interval;
            match fetch(p.match_id) {
                Ok(Some(metadata)) if p.last.as_ref() != Some(&metadata) => {
                    let match_id = p.match_id;
                    let first = p.last.is_none();
                    p.last = Some(metadata.clone());
                    let metadata = Box::new(metadata);
                    return Ok(Some(if first {
                        PostGameEvent::Captured { match_id, metadata }
                    } else {
                        PostGameEvent::Updated { match_id, metadata }
                    }));
                }
                Ok(Some(_)) => {}
                Ok(None) => {}
                Err(e) => failure = Some(e),
            }
        }

        if now >= p.deadline {
            let closed = p.last.is_none().then_some(PostGameEvent::Missed {
                match_id: p.match_id,
                attempts: p.attempts,
            });
            self.settled = Some(p.match_id);
            self.pending = None;
            return Ok(closed);
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }
}

/// How often the phase is sampled by default.
///
/// The capture window is seconds long, so this only needs to notice `PostGame` promptly
/// and keep the retry cadence honest; a snapshot is cheap.
pub const DEFAULT_POSTGAME_INTERVAL: Duration = Duration::from_millis(500);

/// Captures the metadata of each finished match.
///
/// Watches the game phase through the reader. When a real match enters `PostGame` it opens
/// a window: the first read comes [`DEFAULT_FIRST_ATTEMPT_DELAY`] later, then reads repeat
/// every [`DEFAULT_RETRY_INTERVAL`] until [`DEFAULT_DEADLINE`] passes. The first complete
/// copy is emitted as [`PostGameEvent::Captured`]; each later read that differs from the
/// last emitted copy is emitted as [`PostGameEvent::Updated`]. A window that never saw a
/// complete copy ends with [`PostGameEvent::Missed`]; one that did ends silently.
///
/// Health: [`Health::Idle`] while no window is open, [`Health::Ok`] while one is, and
/// [`Health::Failed`] on a poll error (the window stays open and the deadline still runs).
/// Only the match whose post-game screen is up is resident, so a missed match is normal
/// when the player leaves the screen early.
pub struct PostGameSource {
    supervisor: ReaderSupervisor,
    account_id: u32,
    session: Option<GcSession>,
    policy: CapturePolicy,
    interval: Duration,
}

impl PostGameSource {
    /// The name this source tags its notifications with.
    pub const NAME: &'static str = "postgame";

    /// A post-game source over an attached reader, for the given local Steam account id.
    ///
    /// The account id anchors the Game Coordinator scan; get it from
    /// [`deadlock_reader::steam::active_account_id`].
    pub fn new(reader: Arc<Reader>, account_id: u32) -> Self {
        PostGameSource {
            supervisor: ReaderSupervisor::with_reader(reader, ATTACH_RETRY),
            account_id,
            session: None,
            policy: CapturePolicy::default(),
            interval: DEFAULT_POSTGAME_INTERVAL,
        }
    }

    /// Set how often the phase is sampled.
    pub fn every(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Set the capture window timing.
    pub fn timing(mut self, timing: CaptureTiming) -> Self {
        self.policy = CapturePolicy::new(timing);
        self
    }
}

fn read_metadata(
    session: &mut Option<GcSession>,
    account_id: u32,
    mem: &dyn MemoryReader,
    match_id: u64,
) -> Result<Option<CMsgMatchMetaDataContents>, PollError> {
    let transient = |e: deadlock_walker::Error| PollError::Transient(e.to_string());
    if session.is_none() {
        *session = Some(GcSession::new(mem, account_id).map_err(transient)?);
    }
    let Some(live) = session.as_mut() else {
        return Ok(None);
    };
    let found = live.match_metadata(mem, match_id);
    if matches!(found, Err(deadlock_walker::Error::WrongProcess)) {
        *session = None;
    }
    found.map_err(transient)
}

impl Source for PostGameSource {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn poll(&mut self) -> Result<Poll, PollError> {
        let reader = match self.supervisor.acquire() {
            Attached::Fresh(r) => {
                // A new process can reuse a match id and holds none of the old objects.
                self.policy.reset();
                self.session = None;
                r
            }
            Attached::Held(r) => r,
            Attached::Absent | Attached::Waiting => {
                return Ok(Poll::health(Health::Idle("no client attached".into())));
            }
            Attached::Failed(why) => return Err(PollError::Transient(why)),
        };

        let snapshot = match reader.live_snapshot() {
            Ok(s) => {
                self.supervisor.succeeded();
                s
            }
            Err(e) => {
                if self.supervisor.failed() {
                    self.policy.reset();
                    self.session = None;
                }
                return Err(PollError::Transient(e.to_string()));
            }
        };
        let (phase, match_id) = snapshot
            .as_ref()
            .map_or((None, None), |s| (s.game_state, s.match_id));

        let (session, account_id) = (&mut self.session, self.account_id);
        let outcome = self.policy.step(phase, match_id, Instant::now(), |id| {
            read_metadata(session, account_id, reader.memory(), id)
        })?;

        let health = if self.policy.is_pending() {
            Health::Ok
        } else {
            Health::Idle("no capture pending".into())
        };
        Ok(Poll {
            events: outcome.map(Event::PostGame).into_iter().collect(),
            health: Some(health),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use valveprotos::deadlock::c_msg_match_meta_data_contents::MatchInfo;

    const SEC: Duration = Duration::from_secs(1);

    type Answer = Result<Option<CMsgMatchMetaDataContents>, ()>;

    fn meta(match_id: u64) -> CMsgMatchMetaDataContents {
        CMsgMatchMetaDataContents {
            match_info: Some(MatchInfo {
                match_id: Some(match_id),
                ..Default::default()
            }),
        }
    }

    /// Scripted fetcher: each call pops the next answer, `None` once the script is spent.
    #[derive(Default)]
    struct Fake {
        answers: RefCell<VecDeque<Answer>>,
        asked: RefCell<Vec<u64>>,
    }

    impl Fake {
        fn answering(answers: Vec<Answer>) -> Self {
            Fake {
                answers: RefCell::new(answers.into()),
                asked: RefCell::default(),
            }
        }
        fn calls(&self) -> usize {
            self.asked.borrow().len()
        }
    }

    struct Rig {
        policy: CapturePolicy,
        t0: Instant,
        fake: Fake,
    }

    impl Rig {
        fn new(fake: Fake) -> Self {
            Rig {
                policy: CapturePolicy::new(CaptureTiming::default()),
                t0: Instant::now(),
                fake,
            }
        }

        fn at(
            &mut self,
            secs: f32,
            phase: GameState,
            id: Option<u64>,
        ) -> Result<Option<PostGameEvent>, ()> {
            let now = self.t0 + Duration::from_secs_f32(secs);
            let fake = &self.fake;
            self.policy.step(Some(phase), id, now, |m| {
                fake.asked.borrow_mut().push(m);
                fake.answers.borrow_mut().pop_front().unwrap_or(Ok(None))
            })
        }
    }

    use GameState::{End, GameInProgress, HeroSelection, PostGame};

    fn meta_with_duration(match_id: u64, duration_s: u32) -> CMsgMatchMetaDataContents {
        CMsgMatchMetaDataContents {
            match_info: Some(MatchInfo {
                match_id: Some(match_id),
                duration_s: Some(duration_s),
                ..Default::default()
            }),
        }
    }

    fn updated_duration(e: &Option<PostGameEvent>) -> Option<u32> {
        match e {
            Some(PostGameEvent::Updated { metadata, .. }) => {
                metadata.match_info.as_ref().and_then(|i| i.duration_s)
            }
            _ => None,
        }
    }

    fn captured_id(e: &Option<PostGameEvent>) -> Option<u64> {
        match e {
            Some(PostGameEvent::Captured { match_id, .. }) => Some(*match_id),
            _ => None,
        }
    }

    #[test]
    fn it_does_not_arm_outside_post_game_or_without_a_real_match_id() {
        let mut r = Rig::new(Fake::answering(vec![Ok(Some(meta(5)))]));
        for (s, phase, id) in [
            (0.0, GameInProgress, Some(5)),
            (10.0, HeroSelection, Some(5)),
            (20.0, GameInProgress, None),
            (30.0, PostGame, None),
            (40.0, PostGame, Some(0)),
        ] {
            assert_eq!(r.at(s, phase, id), Ok(None));
        }
        assert_eq!(r.fake.calls(), 0);
        assert!(!r.policy.is_pending());
    }

    #[test]
    fn the_first_attempt_waits_for_the_delay() {
        let mut r = Rig::new(Fake::answering(vec![Ok(Some(meta(7)))]));
        r.at(0.0, GameInProgress, Some(7)).unwrap();
        assert_eq!(r.at(1.0, PostGame, Some(7)), Ok(None));
        assert_eq!(r.at(5.9, PostGame, Some(7)), Ok(None));
        assert_eq!(r.fake.calls(), 0, "asked before the delay elapsed");
        assert!(r.policy.is_pending());
        let got = r.at(6.0, PostGame, Some(7)).unwrap();
        assert_eq!(captured_id(&got), Some(7));
        assert_eq!(*r.fake.asked.borrow(), vec![7]);
    }

    #[test]
    fn it_retries_on_none_at_the_retry_cadence() {
        let mut r = Rig::new(Fake::answering(vec![Ok(None), Ok(None), Ok(Some(meta(7)))]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        assert_eq!(r.at(5.0, PostGame, Some(7)), Ok(None));
        assert_eq!(r.fake.calls(), 1);
        assert_eq!(r.at(6.0, PostGame, Some(7)), Ok(None));
        assert_eq!(r.fake.calls(), 1, "retried before the retry interval");
        assert_eq!(r.at(7.0, PostGame, Some(7)), Ok(None));
        assert_eq!(r.fake.calls(), 2);
        let got = r.at(9.0, PostGame, Some(7)).unwrap();
        assert_eq!(captured_id(&got), Some(7));
        assert_eq!(r.fake.calls(), 3);
    }

    #[test]
    fn unchanged_reads_after_the_capture_emit_nothing() {
        let mut r = Rig::new(Fake::answering(vec![Ok(Some(meta(7))); 40]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        assert!(r.at(5.0, PostGame, Some(7)).unwrap().is_some());
        for s in [6.0, 7.0, 9.0, 30.0, 59.0] {
            assert_eq!(r.at(s, PostGame, Some(7)), Ok(None));
        }
        assert!(r.fake.calls() > 1, "stopped asking after the capture");
    }

    #[test]
    fn a_change_after_the_capture_emits_updated_with_the_new_data() {
        let mut r = Rig::new(Fake::answering(vec![
            Ok(Some(meta_with_duration(7, 300))),
            Ok(Some(meta_with_duration(7, 300))),
            Ok(Some(meta_with_duration(7, 310))),
            Ok(Some(meta_with_duration(7, 310))),
        ]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        let first = r.at(5.0, PostGame, Some(7)).unwrap();
        assert_eq!(captured_id(&first), Some(7));
        assert_eq!(r.at(7.0, PostGame, Some(7)), Ok(None));
        let second = r.at(9.0, PostGame, Some(7)).unwrap();
        assert_eq!(updated_duration(&second), Some(310));
        assert_eq!(second.as_ref().map(PostGameEvent::match_id), Some(7));
        assert_eq!(r.at(11.0, PostGame, Some(7)), Ok(None));
    }

    #[test]
    fn a_read_that_finds_nothing_after_the_capture_is_ignored() {
        let mut r = Rig::new(Fake::answering(vec![
            Ok(Some(meta_with_duration(7, 300))),
            Ok(None),
            Ok(Some(meta_with_duration(7, 310))),
        ]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        r.at(5.0, PostGame, Some(7)).unwrap();
        assert_eq!(r.at(7.0, PostGame, Some(7)), Ok(None));
        let got = r.at(9.0, PostGame, Some(7)).unwrap();
        assert_eq!(updated_duration(&got), Some(310));
    }

    #[test]
    fn the_window_closes_quietly_at_the_deadline_after_a_capture() {
        let mut r = Rig::new(Fake::answering(vec![Ok(Some(meta(7))); 100]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        r.at(5.0, PostGame, Some(7)).unwrap();
        assert_eq!(r.at(60.0, PostGame, Some(7)), Ok(None));
        assert!(!r.policy.is_pending());
        let calls = r.fake.calls();
        assert_eq!(r.at(80.0, PostGame, Some(7)), Ok(None));
        assert_eq!(r.fake.calls(), calls, "asked again after the window closed");
    }

    #[test]
    fn it_reports_missed_once_after_the_deadline() {
        let mut r = Rig::new(Fake::default());
        r.at(0.0, PostGame, Some(7)).unwrap();
        let mut outcomes = Vec::new();
        let mut s = 1.0;
        while s < 200.0 {
            if let Some(e) = r.at(s, PostGame, Some(7)).unwrap() {
                outcomes.push((s, e));
            }
            s += 1.0;
        }
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        let (at, e) = &outcomes[0];
        assert!((60.0..62.0).contains(at), "reported at {at}");
        let PostGameEvent::Missed { match_id, attempts } = e else {
            panic!("{e:?}");
        };
        assert_eq!(*match_id, 7);
        assert!(*attempts >= 25, "attempts {attempts}");
        assert_eq!(r.fake.calls() as u32, *attempts);
        assert!(!r.policy.is_pending());
    }

    #[test]
    fn failed_reads_count_as_attempts_and_still_run_out_the_deadline() {
        let mut r = Rig::new(Fake::answering(vec![Err(()), Err(())]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        assert_eq!(r.at(5.0, PostGame, Some(7)), Err(()));
        assert_eq!(r.at(7.0, PostGame, Some(7)), Err(()));
        assert!(r.policy.is_pending());
        let e = r.at(61.0, PostGame, Some(7)).unwrap();
        assert!(matches!(
            e,
            Some(PostGameEvent::Missed {
                match_id: 7,
                attempts: 3
            })
        ));
    }

    #[test]
    fn a_flapping_phase_does_not_emit_twice() {
        let mut r = Rig::new(Fake::answering(vec![Ok(Some(meta(7)))]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        assert!(r.at(5.0, PostGame, Some(7)).unwrap().is_some());
        assert_eq!(r.at(6.0, GameInProgress, Some(7)), Ok(None));
        assert_eq!(r.at(20.0, PostGame, Some(7)), Ok(None));
        assert_eq!(r.at(40.0, PostGame, Some(7)), Ok(None));
        assert_eq!(r.at(100.0, PostGame, Some(7)), Ok(None));
    }

    #[test]
    fn a_flap_mid_capture_keeps_one_window() {
        let mut r = Rig::new(Fake::answering(vec![Ok(None), Ok(Some(meta(7)))]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        r.at(5.0, PostGame, Some(7)).unwrap();
        r.at(6.0, GameInProgress, Some(7)).unwrap();
        r.at(7.0, PostGame, Some(7)).unwrap();
        assert!(r.at(8.0, PostGame, Some(7)).unwrap().is_none());
        assert_eq!(r.fake.calls(), 2, "re-entry must not restart the delay");
    }

    #[test]
    fn a_missed_match_is_not_retried_when_the_phase_flaps() {
        let mut r = Rig::new(Fake::default());
        r.at(0.0, PostGame, Some(7)).unwrap();
        r.at(61.0, PostGame, Some(7)).unwrap();
        let calls = r.fake.calls();
        r.at(62.0, GameInProgress, Some(7)).unwrap();
        assert_eq!(r.at(70.0, PostGame, Some(7)), Ok(None));
        assert_eq!(r.at(80.0, PostGame, Some(7)), Ok(None));
        assert_eq!(r.fake.calls(), calls);
    }

    #[test]
    fn a_new_match_id_arms_again() {
        let mut r = Rig::new(Fake::answering(vec![
            Ok(Some(meta(7))),
            Ok(Some(meta(7))),
            Ok(Some(meta(8))),
        ]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        assert_eq!(captured_id(&r.at(5.0, PostGame, Some(7)).unwrap()), Some(7));
        assert_eq!(r.at(10.0, GameInProgress, Some(8)), Ok(None));
        r.at(100.0, PostGame, Some(8)).unwrap();
        assert_eq!(r.at(104.0, PostGame, Some(8)), Ok(None));
        assert_eq!(
            captured_id(&r.at(105.0, PostGame, Some(8)).unwrap()),
            Some(8)
        );
        assert_eq!(*r.fake.asked.borrow(), vec![7, 7, 8]);
    }

    #[test]
    fn a_new_match_displacing_a_captured_one_reports_no_miss() {
        let mut r = Rig::new(Fake::answering(vec![Ok(Some(meta(7)))]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        r.at(5.0, PostGame, Some(7)).unwrap();
        assert_eq!(r.at(8.0, PostGame, Some(8)), Ok(None));
        assert!(r.policy.is_pending());
    }

    #[test]
    fn a_normal_match_goes_through_end_and_the_id_may_read_zero_after() {
        let mut r = Rig::new(Fake::answering(vec![Ok(None), Ok(Some(meta(7)))]));
        r.at(0.0, GameInProgress, Some(7)).unwrap();
        r.at(1.0, PostGame, None).unwrap();
        r.at(3.0, End, None).unwrap();
        assert_eq!(r.fake.calls(), 0);
        r.at(6.0, PostGame, None).unwrap();
        assert_eq!(*r.fake.asked.borrow(), vec![7], "the live id is remembered");
        let got = r.at(8.0, PostGame, None).unwrap();
        assert_eq!(captured_id(&got), Some(7));
        assert_eq!(r.at(9.0, GameInProgress, None), Ok(None));
        assert_eq!(r.at(30.0, GameInProgress, None), Ok(None));
    }

    #[test]
    fn street_brawl_goes_straight_back_to_play_and_still_captures() {
        let mut r = Rig::new(Fake::answering(vec![Ok(Some(meta(7)))]));
        r.at(0.0, GameInProgress, Some(7)).unwrap();
        r.at(1.0, PostGame, Some(7)).unwrap();
        r.at(2.0, GameInProgress, Some(8)).unwrap();
        assert!(r.policy.is_pending(), "the window outlives the phase");
        let got = r.at(6.0, GameInProgress, Some(8)).unwrap();
        assert_eq!(captured_id(&got), Some(7));
        assert_eq!(*r.fake.asked.borrow(), vec![7]);
    }

    #[test]
    fn a_remembered_id_does_not_leak_into_the_hideout() {
        let mut r = Rig::new(Fake::default());
        r.at(0.0, GameInProgress, Some(7)).unwrap();
        r.at(1.0, GameInProgress, None).unwrap();
        r.at(2.0, PostGame, None).unwrap();
        r.at(30.0, PostGame, None).unwrap();
        assert_eq!(r.fake.calls(), 0);
    }

    #[test]
    fn a_second_post_game_while_pending_reports_the_first_as_missed() {
        let mut r = Rig::new(Fake::default());
        r.at(0.0, PostGame, Some(7)).unwrap();
        let e = r.at(3.0, PostGame, Some(8)).unwrap();
        assert!(matches!(
            e,
            Some(PostGameEvent::Missed {
                match_id: 7,
                attempts: 0
            })
        ));
        r.at(9.0, PostGame, Some(8)).unwrap();
        assert_eq!(*r.fake.asked.borrow(), vec![8]);
    }

    #[test]
    fn reset_forgets_everything() {
        let mut r = Rig::new(Fake::answering(vec![Ok(Some(meta(7))), Ok(Some(meta(7)))]));
        r.at(0.0, PostGame, Some(7)).unwrap();
        assert!(r.at(5.0, PostGame, Some(7)).unwrap().is_some());
        r.policy.reset();
        r.at(100.0, PostGame, Some(7)).unwrap();
        assert!(r.policy.is_pending(), "a restarted game can repeat an id");
        assert!(r.at(105.0, PostGame, Some(7)).unwrap().is_some());

        r.at(200.0, GameInProgress, Some(9)).unwrap();
        r.policy.reset();
        assert!(!r.policy.is_pending());
        r.at(201.0, PostGame, None).unwrap();
        assert!(!r.policy.is_pending(), "the remembered id is gone too");
    }

    #[test]
    fn a_missing_client_is_a_transient_read_failure() {
        let mem = deadlock_memory::mock::MockMemory::new(1);
        let mut session = None;
        let err = read_metadata(&mut session, 1, &mem, 7).unwrap_err();
        assert!(err.is_transient(), "{err}");
        assert!(session.is_none());
    }

    #[test]
    fn custom_timing_is_honoured() {
        let mut r = Rig::new(Fake::answering(vec![Ok(Some(meta(7)))]));
        r.policy = CapturePolicy::new(CaptureTiming {
            first_attempt_delay: SEC,
            retry_interval: SEC,
            deadline: Duration::from_secs(3),
        });
        r.at(0.0, PostGame, Some(7)).unwrap();
        assert!(r.at(1.0, PostGame, Some(7)).unwrap().is_some());
    }
}
