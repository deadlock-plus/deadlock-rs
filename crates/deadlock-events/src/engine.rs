//! Runs sources on their own schedules and merges what they produce.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Instant;

use deadlock_reader::supervise::Shutdown;

use crate::source::{Event, Health, Source};

/// Something the engine wants to tell you.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Notification {
    /// A source observed something.
    Event {
        /// Which source.
        source: &'static str,
        /// What changed.
        event: Event,
    },
    /// A source's health changed.
    ///
    /// Only sent on a change, so a healthy source is silent rather than reporting itself
    /// every tick.
    Health {
        /// Which source.
        source: &'static str,
        /// Its new state.
        health: Health,
    },
}

/// A set of sources, each polled on its own schedule.
///
/// ```no_run
/// use std::sync::Arc;
/// use std::time::Duration;
/// use deadlock_events::{Engine, Notification, PartySource, ReaderSource};
/// use deadlock_reader::Reader;
///
/// let reader = Arc::new(Reader::attach()?);
/// let account = deadlock_reader::steam::active_account_id()?.unwrap_or(0);
///
/// let (_engine, rx) = Engine::new()
///     .with(ReaderSource::new(Arc::clone(&reader)).every(Duration::from_millis(100)))
///     .with(PartySource::new(Arc::clone(&reader), account).every(Duration::from_secs(2)))
///     .start()
///     .expect("the engine could not spawn its threads");
///
/// for note in rx {
///     println!("{note:?}");
/// }
/// # Ok::<(), deadlock_reader::Error>(())
/// ```
#[derive(Default)]
pub struct Engine {
    sources: Vec<Box<dyn Source>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field(
                "sources",
                &self.sources.iter().map(|s| s.name()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Engine {
    /// An engine with no sources.
    pub fn new() -> Self {
        Engine::default()
    }

    /// Add a source. Each gets its own thread when the engine starts.
    pub fn with(mut self, source: impl Source + 'static) -> Self {
        self.sources.push(Box::new(source));
        self
    }

    /// How many sources are registered.
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether no sources are registered.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Start every source and return a handle plus the merged stream.
    ///
    /// Dropping the handle stops every thread. The receiver stays valid until the last
    /// thread exits, so a consumer iterating it sees the stream end rather than an error.
    pub fn start(self) -> std::io::Result<(EngineHandle, Receiver<Notification>)> {
        let (tx, rx) = channel();
        let stop = Arc::new(Shutdown::new());
        let mut threads = Vec::with_capacity(self.sources.len());
        for source in self.sources {
            match spawn_source(source, tx.clone(), Arc::clone(&stop)) {
                Ok(t) => threads.push(t),
                Err(e) => {
                    // Stop and join whatever did start. Returning the error while leaving
                    // threads running would hand back nothing to stop them with, and they
                    // would outlive the engine that was never built.
                    stop.stop();
                    for t in threads {
                        let _ = t.join();
                    }
                    return Err(e);
                }
            }
        }
        Ok((EngineHandle { stop, threads }, rx))
    }
}

fn spawn_source(
    mut source: Box<dyn Source>,
    tx: Sender<Notification>,
    stop: Arc<Shutdown>,
) -> std::io::Result<JoinHandle<()>> {
    let name = source.name();
    let interval = source.interval();
    std::thread::Builder::new()
        .name(format!("deadlock-{name}"))
        .spawn(move || {
            let mut last_health: Option<Health> = None;
            while !stop.is_stopped() {
                let started = Instant::now();
                let mut give_up = false;
                let (events, health) = match source.poll() {
                    Ok(p) => (p.events, p.health),
                    Err(e) => {
                        // A transient failure is usually the game not being up yet, so the
                        // source stays alive and tries again. A fatal one would otherwise
                        // repeat the same message every interval for the life of the
                        // process, which is noise standing in for a thing nobody can fix
                        // by waiting.
                        give_up = !e.is_transient();
                        (Vec::new(), Some(Health::Failed(e.message().to_string())))
                    }
                };

                for event in events {
                    if tx
                        .send(Notification::Event {
                            source: name,
                            event,
                        })
                        .is_err()
                    {
                        return; // the consumer went away
                    }
                }
                if let Some(h) = health
                    && last_health.as_ref() != Some(&h)
                {
                    if tx
                        .send(Notification::Health {
                            source: name,
                            health: h.clone(),
                        })
                        .is_err()
                    {
                        return;
                    }
                    last_health = Some(h);
                }
                if give_up {
                    // Only this source stops. The others know nothing about it and have no
                    // reason to be torn down with it.
                    return;
                }

                //
                // `started` is taken before the poll, so the period is the interval rather
                // than the interval plus however long the poll took.
                if !stop.sleep_until(started + interval) {
                    return;
                }
            }
        })
}

/// Keeps the engine's threads alive. Dropping it stops them.
#[derive(Debug)]
pub struct EngineHandle {
    stop: Arc<Shutdown>,
    threads: Vec<JoinHandle<()>>,
}

impl EngineHandle {
    /// Stop every source and wait for its thread.
    pub fn stop(&mut self) {
        self.stop.stop();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Whether any source thread is still running.
    pub fn is_running(&self) -> bool {
        self.threads.iter().any(|t| !t.is_finished())
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::party::PartyEvent;
    use crate::source::{Poll, PollError};
    use std::time::Duration;

    /// A source that emits a fixed script, then goes quiet.
    struct Scripted {
        name: &'static str,
        script: Vec<Poll>,
        at: usize,
    }

    impl Source for Scripted {
        fn name(&self) -> &'static str {
            self.name
        }
        fn interval(&self) -> Duration {
            Duration::from_millis(1)
        }
        fn poll(&mut self) -> Result<Poll, PollError> {
            let p = self.script.get(self.at).cloned().unwrap_or_default();
            self.at += 1;
            Ok(p)
        }
    }

    struct AlwaysFails;

    impl Source for AlwaysFails {
        fn name(&self) -> &'static str {
            "failing"
        }
        fn interval(&self) -> Duration {
            Duration::from_millis(1)
        }
        fn poll(&mut self) -> Result<Poll, PollError> {
            Err(PollError::Transient("no game".into()))
        }
    }

    fn formed(id: u64) -> Event {
        Event::Party(PartyEvent::Formed {
            party_id: id,
            members: vec![1],
        })
    }

    #[test]
    fn events_reach_the_consumer_tagged_with_their_source() {
        let (_h, rx) = Engine::new()
            .with(Scripted {
                name: "scripted",
                script: vec![Poll::events(vec![formed(7)])],
                at: 0,
            })
            .start()
            .expect("spawning test source threads");

        let note = rx.recv_timeout(Duration::from_secs(5)).expect("an event");
        assert_eq!(
            note,
            Notification::Event {
                source: "scripted",
                event: formed(7),
            }
        );
    }

    #[test]
    fn health_is_reported_on_change_and_not_repeated() {
        let (_h, rx) = Engine::new()
            .with(Scripted {
                name: "s",
                script: vec![
                    Poll::health(Health::Idle("nothing yet".into())),
                    Poll::health(Health::Idle("nothing yet".into())),
                    Poll::health(Health::Ok),
                ],
                at: 0,
            })
            .start()
            .expect("spawning test source threads");

        let mut seen = Vec::new();
        while let Ok(n) = rx.recv_timeout(Duration::from_secs(2)) {
            if let Notification::Health { health, .. } = n {
                seen.push(health);
            }
            if seen.len() == 2 {
                break;
            }
        }
        assert_eq!(
            seen,
            vec![Health::Idle("nothing yet".into()), Health::Ok],
            "the repeated Idle must be suppressed"
        );
    }

    /// A source that cannot reach the game must not take the engine down with it: the
    /// usual cause is the game simply not being started yet.
    #[test]
    fn a_failing_source_reports_and_keeps_running() {
        let (h, rx) = Engine::new()
            .with(AlwaysFails)
            .start()
            .expect("threads spawn");
        let note = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a health report");
        assert_eq!(
            note,
            Notification::Health {
                source: "failing",
                health: Health::Failed("no game".into()),
            }
        );
        assert!(h.is_running(), "it retries rather than exiting");
    }

    /// A source that says its problem is permanent is taken at its word and stopped.
    ///
    /// Before `PollError` existed the engine could not tell the two apart, so a source
    /// with an unfixable problem repeated the same failure every interval for the life of
    /// the process - noise standing in for something nobody can fix by waiting.
    #[test]
    fn a_fatally_failing_source_stops_instead_of_repeating_itself() {
        struct FatallyBroken;

        impl Source for FatallyBroken {
            fn name(&self) -> &'static str {
                "broken"
            }
            fn interval(&self) -> Duration {
                Duration::from_millis(1)
            }
            fn poll(&mut self) -> Result<Poll, PollError> {
                Err(PollError::Fatal("no such capability".into()))
            }
        }

        let (mut h, rx) = Engine::new()
            .with(FatallyBroken)
            .start()
            .expect("threads spawn");

        // The reason is reported once, before the source gives up.
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("a health report"),
            Notification::Health {
                source: "broken",
                health: Health::Failed("no such capability".into()),
            }
        );

        // And then nothing. The channel closing is the source's thread having exited; a
        // second `Health` would mean it had gone round again.
        match rx.recv_timeout(Duration::from_secs(5)) {
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
            other => panic!("expected the source to stop, got {other:?}"),
        }
        h.stop();
    }

    /// The counterpart: one source giving up permanently must not take the others with it.
    #[test]
    fn a_fatal_source_does_not_stop_its_neighbours() {
        struct FatallyBroken;

        impl Source for FatallyBroken {
            fn name(&self) -> &'static str {
                "broken"
            }
            fn interval(&self) -> Duration {
                Duration::from_millis(1)
            }
            fn poll(&mut self) -> Result<Poll, PollError> {
                Err(PollError::Fatal("no such capability".into()))
            }
        }

        let (h, rx) = Engine::new()
            .with(FatallyBroken)
            .with(Scripted {
                name: "healthy",
                script: vec![Poll::health(Health::Ok)],
                at: 0,
            })
            .start()
            .expect("threads spawn");

        // Both report. Order is not fixed - they are separate threads - so collect until
        // the healthy one has been heard from.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_healthy = false;
        while Instant::now() < deadline && !saw_healthy {
            if let Ok(Notification::Health { source, .. }) = rx.recv_timeout(Duration::from_secs(1))
            {
                saw_healthy |= source == "healthy";
            }
        }
        assert!(saw_healthy, "the healthy source kept running");
        assert!(h.is_running(), "the engine outlives one source giving up");
    }

    #[test]
    fn sources_run_independently_of_each_other() {
        struct Slow;
        impl Source for Slow {
            fn name(&self) -> &'static str {
                "slow"
            }
            fn interval(&self) -> Duration {
                Duration::from_millis(1)
            }
            fn poll(&mut self) -> Result<Poll, PollError> {
                std::thread::sleep(Duration::from_millis(400));
                Ok(Poll::quiet())
            }
        }

        let (_h, rx) = Engine::new()
            .with(Slow)
            .with(Scripted {
                name: "fast",
                script: vec![Poll::events(vec![formed(1)]), Poll::events(vec![formed(2)])],
                at: 0,
            })
            .start()
            .expect("spawning test source threads");

        let start = Instant::now();
        let mut fast = 0;
        while fast < 2 {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Notification::Event { source: "fast", .. }) => fast += 1,
                Ok(_) => {}
                Err(e) => panic!("timed out after {fast} fast events: {e}"),
            }
        }
        assert!(
            start.elapsed() < Duration::from_millis(350),
            "the fast source waited on the slow one: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn dropping_the_handle_stops_the_threads() {
        let (mut h, _rx) = Engine::new()
            .with(AlwaysFails)
            .start()
            .expect("threads spawn");
        assert!(h.is_running());
        h.stop();
        assert!(!h.is_running(), "stop must join every thread");
    }

    #[test]
    fn an_engine_with_no_sources_is_valid_and_ends_its_stream() {
        let (_h, rx) = Engine::new()
            .start()
            .expect("an engine with no sources spawns nothing");
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_err());
    }
}
