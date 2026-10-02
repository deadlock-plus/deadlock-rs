//! Push events instead of polling for them.
//!
//! [`crate::events::EventTracker`] is the engine: hand it snapshots, get back what
//! changed. That suits a caller that already owns a loop (a UI redrawing every frame);
//! otherwise you end up writing the loop, the attach retry and the reattach yourself.
//!
//! A [`Watcher`] owns that loop on its own thread and hands you the events.
//!
//! ```no_run
//! use std::time::Duration;
//! use deadlock_reader::events::Event;
//! use deadlock_reader::watcher::{Notification, Watcher};
//!
//! // Stays alive until the handle is dropped.
//! let _watcher = Watcher::every(Duration::from_millis(500)).on(|n| match n {
//!     Notification::Event(Event::Kill { slot, total, .. }) => {
//!         println!("slot {slot:?} is on {total}");
//!     }
//!     Notification::Attached { pid } => println!("game up, pid {pid}"),
//!     Notification::Detached => println!("game closed"),
//!     _ => {}
//! })?;
//! # Ok::<(), deadlock_reader::Error>(())
//! ```
//!
//! Or take a channel and drive it from your own loop:
//!
//! ```no_run
//! # use std::time::Duration;
//! # use deadlock_reader::watcher::Watcher;
//! let (_watcher, events) = Watcher::every(Duration::from_millis(500)).channel()?;
//! for notification in events {
//!     println!("{notification:?}");
//! }
//! # Ok::<(), deadlock_reader::Error>(())
//! ```
//!
//! # Behaviour
//!
//! * **It starts before the game does.** With no reader supplied the watcher attaches
//!   itself and keeps retrying, so a tray app can launch at boot and just work. You get
//!   [`Notification::Attached`] when it succeeds and [`Notification::Detached`] when the
//!   game goes away.
//! * **The handler runs on the watcher thread.** Keep it short. Anything slow belongs
//!   behind a channel, or you delay the next poll.
//! * **Stopping is prompt.** The thread wakes on a short tick rather than sleeping out the
//!   full interval, so dropping the handle does not block for up to `interval`.
//! * **Resolution is still the poll rate.** This is the same sampling under a nicer
//!   shape; the client has no event feed to tap. See [`crate::events`].
//!
//! # Choosing an interval
//!
//! Two numbers bound it, both measurable with `dlrs bench` against your own machine and
//! match:
//!
//! * **New data appears at roughly 50-60 Hz**, or every ~18 ms - near the engine's 64 Hz
//!   tick. Polling faster than [`MIN_USEFUL_INTERVAL`] re-reads bytes that have not
//!   changed and produces no extra events; it only costs CPU.
//! * **A snapshot costs a few hundred microseconds** in a release build. On a spectated
//!   ranked match - 4,040 entities, 13 players - ~720 us. Debug builds are several times
//!   slower and will mislead you; benchmark in release.
//!
//! The watcher takes a **full, uncached snapshot every tick** by default, so an objective
//! spawning is seen immediately. [`WatcherBuilder::cached`] trades that for ~40% less work
//! per tick; read [`crate::cache::SnapshotCache`] before enabling it.
//!
//! Those absolute figures came off a Ryzen 7 9800X3D and are close to a best case. The
//! cost is dominated by cross-process reads, so a slower machine, a busier scene, or
//! Proton's `process_vm_readv` path will all push them up - possibly severalfold. Run
//! `dlrs bench` on the target machine rather than trusting the numbers here.
//!
//! [`DEFAULT_INTERVAL`] is a sensible starting point, with room to go faster: at ~720 us
//! a tick, a 20 ms interval is under 4% of one core. Doing better than polling would
//! require hooking the game, which this crate does not and will not do.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::cache::SnapshotCache;
use crate::error::{Error, Result};
use crate::events::{Event, EventTracker};
use crate::reader::Reader;
use crate::supervise::Shutdown;

/// Below this, polling cannot surface anything new.
///
/// The client's simulation time was observed advancing at ~53-55 Hz once the read path
/// was fast enough to sample it properly (earlier measurements of ~37 Hz were
/// undersampled by an expensive reader). ~18 ms is therefore the shortest gap that
/// reliably contains a change. A guide, not a floor the builder enforces.
pub const MIN_USEFUL_INTERVAL: Duration = Duration::from_millis(18);

/// A good default: events feel prompt, at about 1% of one core.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(500);

/// Something the watcher wants to tell you.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Notification {
    /// Attached to a running client.
    Attached {
        /// Process id of the game.
        pid: u32,
    },
    /// The game went away. The watcher keeps trying to reattach.
    Detached,
    /// Something happened in the match.
    Event(Event),
    /// A read failed but the game is still there. Usually transient.
    ReadFailed(String),
    /// Attaching failed for a reason other than the game not running.
    ///
    /// Reported once per distinct reason rather than every tick. A denied `ptrace` scope
    /// or a permissions problem is fixable, and retrying forever in silence hides it -
    /// which is what happened when every attach error was swallowed alike.
    AttachFailed(String),
}

// The failure rule lives with the rest of the supervision logic: `watcher` is behind the
// `events` feature, and `supervise` is not, so it cannot depend on this module.
pub use crate::supervise::{DETACH_AFTER_FAILURES, FailureBudget};

/// A running watcher.
///
/// Stops when dropped, so bind it to a variable - `let _ = Watcher::every(..).on(..)`
/// drops it immediately and nothing will ever fire.
#[derive(Debug)]
pub struct Watcher {
    stop: Arc<Shutdown>,
    handle: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Start configuring a watcher that polls on `interval`.
    pub fn every(interval: Duration) -> WatcherBuilder {
        WatcherBuilder {
            interval,
            reader: None,
            cache: None,
        }
    }

    /// Stop the thread and wait for it to finish.
    ///
    /// Also runs on drop; call it explicitly when you want to be sure the handler has
    /// stopped before doing something else.
    pub fn stop(&mut self) {
        self.stop.stop();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }

    /// Whether the watcher thread is still running.
    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Configuration for a [`Watcher`].
#[derive(Debug)]
pub struct WatcherBuilder {
    interval: Duration,
    reader: Option<Arc<Reader>>,
    cache: Option<SnapshotCache>,
}

impl WatcherBuilder {
    /// Use a reader you already have instead of letting the watcher attach.
    ///
    /// The watcher will not reattach for you in this mode - it cannot replace a reader it
    /// does not own - so a closed game surfaces as [`Notification::ReadFailed`] rather
    /// than [`Notification::Detached`]. Prefer the self-attaching form unless you need to
    /// share one reader with the rest of your app.
    pub fn with_reader(mut self, reader: Arc<Reader>) -> Self {
        self.reader = Some(reader);
        self
    }

    /// Trade objective freshness for speed by caching pinned entities.
    ///
    /// **Off by default.** Caching saves roughly 40% of a tick, but an
    /// objective that spawns is not seen until the next full entity walk - so a Midboss or
    /// Urn appearing can be reported late by up to the cache's refresh interval. Kills,
    /// deaths, souls and levels are never affected either way.
    ///
    /// Turn it on when you are polling hard enough for 40% to matter and can accept that
    /// window. Pass [`SnapshotCache::every`] to choose the window explicitly.
    pub fn cached(mut self, cache: SnapshotCache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Deliver notifications to `handler`, on the watcher's own thread.
    pub fn on<F>(self, handler: F) -> Result<Watcher>
    where
        F: FnMut(Notification) + Send + 'static,
    {
        self.start(handler)
    }

    /// Deliver notifications into a channel.
    ///
    /// The receiver ends when the returned [`Watcher`] is dropped, so a `for` loop over it
    /// terminates cleanly rather than hanging.
    pub fn channel(self) -> Result<(Watcher, Receiver<Notification>)> {
        let (tx, rx) = channel();
        // A closed receiver is not an error: the consumer stopped listening, and the
        // watcher will be dropped along with it.
        let w = self.start(move |n| {
            let _ = tx.send(n);
        })?;
        Ok((w, rx))
    }

    fn start<F>(self, mut handler: F) -> Result<Watcher>
    where
        F: FnMut(Notification) + Send + 'static,
    {
        if self.interval.is_zero() {
            return Err(Error::Watcher("interval must be greater than zero".into()));
        }
        let owns_reader = self.reader.is_none();
        let stop = Arc::new(Shutdown::new());
        let flag = Arc::clone(&stop);
        let interval = self.interval;
        let mut reader = self.reader;
        let mut cache = self.cache;

        let handle = std::thread::Builder::new()
            .name("deadlock-watcher".into())
            .spawn(move || {
                let mut tracker = EventTracker::new();
                let mut budget = FailureBudget::new(DETACH_AFTER_FAILURES);
                // Reported once per distinct reason, not once per tick.
                let mut last_attach_error: Option<String> = None;
                // Announce a reader we were handed, so a consumer sees the same
                // Attached/... sequence either way.
                if let Some(r) = &reader {
                    handler(Notification::Attached { pid: r.pid() });
                }

                while !flag.is_stopped() {
                    // The deadline is taken *before* the work, so the period is the
                    // interval rather than the interval plus however long a snapshot took.
                    let due = Instant::now() + interval;
                    if reader.is_none() {
                        match attach() {
                            Ok(r) => {
                                handler(Notification::Attached { pid: r.pid() });
                                last_attach_error = None;
                                budget.succeeded();
                                reader = Some(Arc::new(r));
                                // A new process means the old history is meaningless, and
                                // every pinned address belongs to a dead process.
                                tracker.reset();
                                if let Some(c) = cache.as_mut() {
                                    c.invalidate();
                                }
                            }
                            Err(e) => {
                                // "Not running yet" is the ordinary state for something
                                // started at boot, and is not worth a notification every
                                // tick. Anything else is: it is usually fixable, and
                                // swallowing it made a denied ptrace scope look exactly
                                // like a game that simply was not open.
                                if !matches!(
                                    e,
                                    Error::Memory(deadlock_memory::Error::ProcessNotFound(_))
                                ) {
                                    let text = e.to_string();
                                    if last_attach_error.as_deref() != Some(text.as_str()) {
                                        handler(Notification::AttachFailed(text.clone()));
                                        last_attach_error = Some(text);
                                    }
                                }
                                if !flag.sleep_until(due) {
                                    return;
                                }
                                continue;
                            }
                        }
                    }

                    let Some(r) = reader.as_ref() else { continue };
                    let taken = match cache.as_mut() {
                        Some(c) => r.live_snapshot_cached(c),
                        // Default: a full walk every tick, so a spawning objective is seen
                        // the moment it exists.
                        None => r.live_snapshot(),
                    };
                    match taken {
                        Ok(Some(snap)) => {
                            budget.succeeded();
                            for ev in tracker.update(&snap) {
                                handler(Notification::Event(ev));
                            }
                        }
                        // Attached, but not in a match. Drop history so the next match
                        // does not diff against the last one.
                        Ok(None) => {
                            budget.succeeded();
                            tracker.reset();
                            // Between matches the entity list is about to be replaced
                            // wholesale; nothing pinned survives it.
                            if let Some(c) = cache.as_mut() {
                                c.invalidate();
                            }
                        }
                        Err(e) => {
                            handler(Notification::ReadFailed(e.to_string()));
                            // Only a sustained run of failures means the game went away.
                            // Dropping the reader on a single one would discard event
                            // history for a transient short read.
                            if owns_reader && budget.failed() {
                                handler(Notification::Detached);
                                reader = None;
                                tracker.reset();
                                budget.succeeded();
                                if let Some(c) = cache.as_mut() {
                                    c.invalidate();
                                }
                            }
                        }
                    }

                    if !flag.sleep_until(due) {
                        return;
                    }
                }
            })
            .map_err(|e| Error::Watcher(format!("could not start thread: {e}")))?;

        Ok(Watcher {
            stop,
            handle: Some(handle),
        })
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn attach() -> Result<Reader> {
    Reader::attach()
}

/// Without a platform backend there is nothing to attach to, so a self-attaching watcher
/// simply never fires. `with_reader` still works, which is what the mock-backed tests use.
#[cfg(not(any(windows, target_os = "linux")))]
fn attach() -> Result<Reader> {
    Err(
        deadlock_memory::Error::Unsupported("no process backend; use WatcherBuilder::with_reader")
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One bad read is a hiccup; a run of them is a closed game. Detaching on the first
    /// threw away the event history every time a page happened to be unmapped mid-walk.
    #[test]
    fn a_single_failure_does_not_count_as_the_game_going_away() {
        let mut b = FailureBudget::new(DETACH_AFTER_FAILURES);
        assert!(!b.failed(), "one failure is not enough");
        assert!(!b.failed(), "nor two");
        assert_eq!(b.consecutive(), 2);
        assert!(b.failed(), "three in a row is");
        assert!(b.spent());
    }

    /// The run has to be consecutive: a success in between clears it, or a game that
    /// fails one read a minute would eventually be declared gone.
    #[test]
    fn a_success_clears_the_run() {
        let mut b = FailureBudget::new(DETACH_AFTER_FAILURES);
        assert!(!b.failed());
        assert!(!b.failed());
        b.succeeded();
        assert_eq!(b.consecutive(), 0);
        assert!(!b.spent());
        assert!(!b.failed(), "counting restarts from zero");
    }

    /// A budget of zero gives up immediately, and must not need a failure first to say so.
    #[test]
    fn a_zero_budget_is_spent_from_the_start() {
        let b = FailureBudget::new(0);
        assert!(b.spent());
    }

    #[test]
    fn a_zero_interval_is_rejected_rather_than_spinning() {
        let err = Watcher::every(Duration::ZERO).on(|_| {});
        assert!(err.is_err(), "zero interval would busy-loop a core");
    }

    #[test]
    fn dropping_the_watcher_stops_the_thread() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&ticks);
        let w = Watcher::every(Duration::from_millis(10))
            .on(move |_| {
                seen.fetch_add(1, Ordering::Relaxed);
            })
            .expect("spawn");
        assert!(w.is_running());
        drop(w);
        let after = ticks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(ticks.load(Ordering::Relaxed), after);
    }

    /// A one-hour interval must not make shutdown take an hour.
    ///
    /// This used to hold because the thread woke every 50 ms to re-read a flag - which
    /// bounded shutdown at 50 ms and cost 72,000 wakeups an hour to do it. It now holds
    /// because the sleeping thread is woken directly, so the bound is far tighter and the
    /// idle cost is one wakeup per interval.
    #[test]
    fn stop_returns_promptly_even_with_a_long_interval() {
        let mut w = Watcher::every(Duration::from_secs(3600))
            .on(|_| {})
            .expect("spawn");
        let start = Instant::now();
        w.stop();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "stop took {:?}",
            start.elapsed()
        );
        assert!(!w.is_running());
    }

    #[test]
    fn stop_is_idempotent() {
        let mut w = Watcher::every(Duration::from_millis(10))
            .on(|_| {})
            .expect("spawn");
        w.stop();
        w.stop();
    }

    #[test]
    fn the_channel_ends_when_the_watcher_is_dropped() {
        let (w, rx) = Watcher::every(Duration::from_millis(10))
            .channel()
            .expect("spawn");
        drop(w);
        let drained: Vec<_> = rx.into_iter().collect();
        assert!(drained.len() < 1000, "unexpected flood: {}", drained.len());
    }

    #[test]
    fn a_handler_that_panics_does_not_take_the_process_with_it() {
        let w = Watcher::every(Duration::from_millis(5))
            .on(|_| panic!("handler blew up"))
            .expect("spawn");
        std::thread::sleep(Duration::from_millis(50));
        let _ = w.is_running();
    }

    #[test]
    fn notifications_are_send_so_they_can_cross_a_channel() {
        fn assert_send<T: Send + 'static>() {}
        assert_send::<Notification>();
        assert_send::<Watcher>();
    }

    #[test]
    fn a_shared_handler_can_collect_across_ticks() {
        let log: Arc<Mutex<Vec<Notification>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        let mut w = Watcher::every(Duration::from_millis(5))
            .on(move |n| sink.lock().unwrap().push(n))
            .expect("spawn");
        std::thread::sleep(Duration::from_millis(30));
        w.stop();
        assert!(log.lock().unwrap().len() < 100);
    }
}
