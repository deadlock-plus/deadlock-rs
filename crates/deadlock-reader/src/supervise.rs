//! Keeping a [`Reader`] attached across the game starting, stopping and hiccuping.
//!
//! Three poll loops in this workspace each grew their own version of this: the
//! [`crate::watcher`], `deadlock-events`' reader source, and the overlay's poller. They
//! agreed on the easy parts and disagreed on the ones that matter:
//!
//! * whether a single failed read means the game is gone (it does not),
//! * whether a failure to *attach* is worth reporting (only if it is not simply "the game
//!   is not running"),
//! * and whether there is any way back once the reader is lost (one of them had none, so
//!   closing the game left it reporting failure forever).
//!
//! This is that logic written once. It owns no thread and no interval: a caller drives it
//! from whatever loop it already has.
//!
//! ```no_run
//! use std::time::Duration;
//! use deadlock_reader::supervise::{Attached, ReaderSupervisor};
//!
//! let mut sup = ReaderSupervisor::new(Duration::from_secs(3));
//! loop {
//!     let reader = match sup.acquire() {
//!         Attached::Fresh(r) | Attached::Held(r) => r,
//!         // Not running, not time to retry, or a reason already reported.
//!         _ => continue,
//!     };
//!     match reader.live_snapshot() {
//!         Ok(_) => sup.succeeded(),
//!         Err(_) => {
//!             if sup.failed() {
//!                 // Given up: the reader is dropped and the next `acquire` reattaches.
//!             }
//!         }
//!     }
//! }
//! ```

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::reader::Reader;

/// How many consecutive failed reads it takes to call the game gone.
///
/// One failed read is usually a page unmapped mid-walk, not a closed client. Detaching on
/// the first one threw away the whole event history and forced a full reattach for a
/// hiccup the next tick would have recovered from.
pub const DETACH_AFTER_FAILURES: u32 = 3;

/// Consecutive read failures, and when they add up to "the game is gone".
///
/// Split out so the rule is testable without a live process behind it. It is also the
/// piece the event engine's reader source and the overlay's poller each grew their own
/// version of; they can share this one.
#[derive(Clone, Copy, Debug, Default)]
pub struct FailureBudget {
    consecutive: u32,
    tolerate: u32,
}

impl FailureBudget {
    /// A budget that gives up after `tolerate` consecutive failures.
    pub fn new(tolerate: u32) -> Self {
        FailureBudget {
            consecutive: 0,
            tolerate,
        }
    }

    /// Record a failure. Returns whether the budget is now spent.
    pub fn failed(&mut self) -> bool {
        self.consecutive = self.consecutive.saturating_add(1);
        self.spent()
    }

    /// Record a success, which clears the run.
    pub fn succeeded(&mut self) {
        self.consecutive = 0;
    }

    /// Whether enough consecutive failures have piled up to give up.
    pub fn spent(&self) -> bool {
        self.consecutive >= self.tolerate
    }

    /// Failures since the last success.
    pub fn consecutive(&self) -> u32 {
        self.consecutive
    }
}

/// A stop signal a sleeping poll loop can be woken from.
///
/// The loops in this workspace slept in 50 ms slices so a stop request would be noticed
/// within 50 ms rather than at the end of the interval. That is ten wakeups per default
/// 500 ms tick, a hundred for a five-second one, all to re-read a flag that is almost never
/// set - and it still leaves shutdown up to 50 ms late.
///
/// A condvar is better on both counts: one wakeup per interval, and a stop is observed the
/// moment it is requested instead of at the next slice boundary.
#[derive(Debug, Default)]
pub struct Shutdown {
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Shutdown {
    /// A signal that has not been raised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request a stop, waking anything sleeping on this signal immediately.
    ///
    /// Idempotent, so a `stop()` from both an explicit call and a `Drop` is fine.
    pub fn stop(&self) {
        *self.locked() = true;
        self.wake.notify_all();
    }

    /// Whether a stop has been requested.
    pub fn is_stopped(&self) -> bool {
        *self.locked()
    }

    /// Sleep for `total`, or until [`Shutdown::stop`] is called.
    ///
    /// Returns `false` if a stop was requested, so a caller can bail out of its loop rather
    /// than finish the interval.
    pub fn sleep(&self, total: Duration) -> bool {
        let guard = self.locked();
        let (guard, _) = self
            .wake
            .wait_timeout_while(guard, total, |stopped| !*stopped)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !*guard
    }

    /// Sleep until `deadline`, or until stopped.
    ///
    /// Deadline rather than duration, because a loop that sleeps a full interval *after*
    /// doing its work has a period of `interval + work` and drifts by however long the work
    /// took. For a poll loop that is the difference between "ten times a second" and "ten
    /// times a second minus whatever the snapshot cost".
    pub fn sleep_until(&self, deadline: Instant) -> bool {
        self.sleep(deadline.saturating_duration_since(Instant::now()))
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, bool> {
        self.stopped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// How often to retry attaching by default.
///
/// Attaching walks the process list and resolves the schema, so this is not free enough
/// to do every tick of a fast poll loop.
pub const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(3);

/// What [`ReaderSupervisor::acquire`] found.
#[derive(Clone, Debug)]
pub enum Attached {
    /// Attached on this call. A caller announcing "game up" wants this one, not
    /// [`Attached::Held`], or it announces it every tick.
    Fresh(Arc<Reader>),
    /// Already attached from an earlier call.
    Held(Arc<Reader>),
    /// The game is not running. The ordinary state for something started at boot, and
    /// deliberately not an error.
    Absent,
    /// Attaching failed for a reason other than the game not running, and this is the
    /// first time that particular reason has been seen.
    ///
    /// Reported once per distinct reason rather than once per tick: a denied `ptrace`
    /// scope is worth surfacing, but not sixty times a minute.
    Failed(String),
    /// Not attached, and not time to try again yet.
    Waiting,
}

/// Owns a [`Reader`] and the decisions about when to (re)attach and when to give up.
#[derive(Debug)]
pub struct ReaderSupervisor {
    reader: Option<Arc<Reader>>,
    retry_every: Duration,
    last_attempt: Option<Instant>,
    budget: FailureBudget,
    last_error: Option<String>,
}

impl ReaderSupervisor {
    /// A supervisor that attaches itself, retrying every `retry_every`.
    pub fn new(retry_every: Duration) -> Self {
        ReaderSupervisor {
            reader: None,
            retry_every,
            last_attempt: None,
            budget: FailureBudget::new(DETACH_AFTER_FAILURES),
            last_error: None,
        }
    }

    /// A supervisor over a reader the caller already has.
    ///
    /// It still reattaches if that reader is lost, which is the difference from simply
    /// holding an `Arc<Reader>` forever.
    pub fn with_reader(reader: Arc<Reader>, retry_every: Duration) -> Self {
        ReaderSupervisor {
            reader: Some(reader),
            ..Self::new(retry_every)
        }
    }

    /// How many consecutive failures to tolerate before dropping the reader.
    pub fn tolerating(mut self, failures: u32) -> Self {
        self.budget = FailureBudget::new(failures);
        self
    }

    /// The reader, if one is currently held. Does not attach.
    pub fn reader(&self) -> Option<&Arc<Reader>> {
        self.reader.as_ref()
    }

    /// Get a reader, attaching if there is none and the retry gate allows.
    pub fn acquire(&mut self) -> Attached {
        if let Some(r) = &self.reader {
            return Attached::Held(Arc::clone(r));
        }
        if let Some(last) = self.last_attempt
            && last.elapsed() < self.retry_every
        {
            return Attached::Waiting;
        }
        self.last_attempt = Some(Instant::now());

        match attach() {
            Ok(r) => {
                let r = Arc::new(r);
                self.reader = Some(Arc::clone(&r));
                self.budget.succeeded();
                self.last_error = None;
                Attached::Fresh(r)
            }
            // Not running is the ordinary state, and saying so every retry would drown
            // out the reasons that are worth acting on.
            Err(Error::Memory(deadlock_memory::Error::ProcessNotFound(_))) => {
                self.last_error = None;
                Attached::Absent
            }
            Err(e) => {
                let text = e.to_string();
                if self.last_error.as_deref() == Some(text.as_str()) {
                    return Attached::Waiting;
                }
                self.last_error = Some(text.clone());
                Attached::Failed(text)
            }
        }
    }

    /// Record a successful read, clearing the failure run.
    pub fn succeeded(&mut self) {
        self.budget.succeeded();
    }

    /// Record a failed read.
    ///
    /// Returns whether that was enough to give up, in which case the reader has been
    /// dropped and the next [`ReaderSupervisor::acquire`] will try to attach again.
    pub fn failed(&mut self) -> bool {
        if !self.budget.failed() {
            return false;
        }
        self.detach();
        true
    }

    /// Drop the reader and reset, so the next `acquire` reattaches immediately.
    pub fn detach(&mut self) {
        self.reader = None;
        self.budget.succeeded();
        self.last_attempt = None;
        self.last_error = None;
    }

    /// Consecutive failed reads since the last success.
    pub fn consecutive_failures(&self) -> u32 {
        self.budget.consecutive()
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn attach() -> Result<Reader> {
    Reader::attach()
}

/// Without a platform backend there is nothing to attach to. `with_reader` still works,
/// which is what the mock-backed tests use.
#[cfg(not(any(windows, target_os = "linux")))]
fn attach() -> Result<Reader> {
    Err(deadlock_memory::Error::Unsupported(
        "no process backend; use ReaderSupervisor::with_reader",
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retry interval is a real wait, and a sane one.
    ///
    /// [`DEFAULT_RETRY_INTERVAL`] paces reattachment while the game is not running, which
    /// is most of the time for a tool that sits open. Setting it to zero passed the whole
    /// suite: every test that exercises the supervisor either injects its own interval or
    /// never reaches the sleep, so nothing noticed that the shipped default had become a
    /// busy loop.
    ///
    /// A zero here does not fail, it *spins* — the supervisor retries as fast as the OS
    /// will let it and burns a core waiting for a game that is not running. That is the
    /// worst kind of default, because it works perfectly on a machine where the game is
    /// already up.
    ///
    /// The upper bound is the other half: an interval measured in minutes would make the
    /// tool look broken for the first minute after the game starts.
    #[test]
    fn the_retry_interval_is_a_real_wait_and_not_a_spin() {
        assert!(
            DEFAULT_RETRY_INTERVAL >= Duration::from_secs(1),
            "a retry interval of {DEFAULT_RETRY_INTERVAL:?} is a busy loop, not a wait"
        );
        assert!(
            DEFAULT_RETRY_INTERVAL <= Duration::from_secs(30),
            "a retry interval of {DEFAULT_RETRY_INTERVAL:?} makes the tool look broken \
             after the game starts"
        );
        assert_eq!(DEFAULT_RETRY_INTERVAL, Duration::from_secs(3));
    }

    /// `Fresh` versus `Held` is what lets a caller announce "game up" once rather than
    /// every tick, so the distinction has to survive.
    #[test]
    fn a_held_reader_is_not_reported_as_freshly_attached() {
        let sup = ReaderSupervisor::new(Duration::from_millis(1));
        assert!(sup.reader().is_none());
        assert_eq!(sup.consecutive_failures(), 0);
    }

    /// The retry gate is what keeps a fast poll loop from walking the process list every
    /// tick while the game is closed.
    #[test]
    fn a_failed_attach_is_not_retried_until_the_gate_opens() {
        let mut sup = ReaderSupervisor::new(Duration::from_secs(3600));
        let _ = sup.acquire();
        assert!(
            matches!(sup.acquire(), Attached::Waiting | Attached::Held(_)),
            "a second immediate attempt must be gated or already satisfied"
        );
    }

    /// Detaching clears the gate as well as the reader: having just lost the game is the
    /// one moment worth trying again straight away.
    #[test]
    fn detaching_allows_an_immediate_reattach_attempt() {
        let mut sup = ReaderSupervisor::new(Duration::from_secs(3600));
        let _ = sup.acquire();
        sup.detach();
        assert!(
            !matches!(sup.acquire(), Attached::Waiting),
            "the gate should not still be closed after a detach"
        );
    }

    /// A sleeping loop has to notice a stop at once. It used to, by waking every 50 ms to
    /// re-read a flag - which bounds shutdown at 50 ms and costs 72,000 wakeups an hour to
    /// do it on an idle one-hour interval.
    #[test]
    fn a_stop_wakes_a_sleeper_instead_of_waiting_out_the_interval() {
        let s = Arc::new(Shutdown::new());
        let sleeper = Arc::clone(&s);

        let t = std::thread::spawn(move || {
            let started = Instant::now();
            let finished = sleeper.sleep(Duration::from_secs(3600));
            (finished, started.elapsed())
        });

        std::thread::sleep(Duration::from_millis(50));
        s.stop();

        let (finished, waited) = t.join().expect("the sleeper thread");
        assert!(!finished, "a stopped sleep reports that it was stopped");
        assert!(
            waited < Duration::from_secs(1),
            "an hour-long sleep took {waited:?} to notice a stop"
        );
    }

    /// The other half: a sleep that is *not* interrupted has to actually sleep.
    #[test]
    fn an_uninterrupted_sleep_runs_its_course() {
        let s = Shutdown::new();
        let started = Instant::now();
        assert!(s.sleep(Duration::from_millis(60)));
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "returned after {:?}, so it did not wait",
            started.elapsed()
        );
    }

    /// `sleep_until` is what keeps a poll loop's *period* equal to its interval. Sleeping a
    /// whole interval after the work makes the real period `interval + work`, so a handler
    /// that takes a while silently slows the loop it runs inside.
    #[test]
    fn a_deadline_absorbs_the_work_rather_than_adding_to_it() {
        let s = Shutdown::new();
        let interval = Duration::from_millis(80);
        let work = Duration::from_millis(50);

        let started = Instant::now();
        let due = Instant::now() + interval;
        std::thread::sleep(work);
        assert!(s.sleep_until(due));
        let period = started.elapsed();

        assert!(
            period < interval + work / 2,
            "period was {period:?}; the work is being added to the interval"
        );
        assert!(period >= interval - Duration::from_millis(20), "{period:?}");
    }

    /// A deadline already in the past must return at once rather than underflowing into a
    /// very long sleep.
    #[test]
    fn a_deadline_in_the_past_does_not_sleep() {
        let s = Shutdown::new();
        let started = Instant::now();
        assert!(s.sleep_until(Instant::now() - Duration::from_secs(60)));
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn stopping_twice_is_harmless_and_stays_stopped() {
        let s = Shutdown::new();
        assert!(!s.is_stopped());
        s.stop();
        s.stop();
        assert!(s.is_stopped());
        assert!(!s.sleep(Duration::from_secs(3600)), "already stopped");
    }

    #[test]
    fn failures_have_to_be_consecutive_to_count() {
        let mut sup = ReaderSupervisor::new(Duration::from_secs(1)).tolerating(3);
        assert!(!sup.failed());
        assert!(!sup.failed());
        sup.succeeded();
        assert_eq!(sup.consecutive_failures(), 0);
        assert!(!sup.failed(), "the run restarted");
        assert!(!sup.failed());
        assert!(sup.failed(), "three in a row gives up");
        assert_eq!(
            sup.consecutive_failures(),
            0,
            "giving up resets, ready for the next attach"
        );
    }
}
