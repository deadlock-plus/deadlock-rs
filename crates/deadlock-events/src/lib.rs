//! A multi-source event engine for Deadlock.
//!
//! Two very different things are worth watching, and they cannot share a schedule:
//!
//! | Source | Reads | Poll cost | Sensible interval |
//! |---|---|---|---|
//! | [`ReaderSource`] | entity and schema systems | a few hundred microseconds | 50 to 100 ms |
//! | [`PartySource`] | Game Coordinator shared objects | 2.6 us pinned, ~0.3 s to re-locate | 1 to 2 s |
//! | [`PostGameSource`] | game phase, then the match metadata object | one snapshot per tick; a heap search per attempt | 500 ms |
//!
//! # Why each source gets its own thread
//!
//! This is not a stylistic choice. A cold Game Coordinator sweep measured **1.2 seconds**,
//! against a match source that wants polling every **50 to 100 ms**, and a finished match
//! stays readable for only **12 to 19 seconds**. On a shared thread a sweep swallows a
//! dozen match ticks, and a slower machine or a larger heap eats further into the window
//! in which the final scoreboard can still be read. The cadences have to be genuinely
//! independent.
//!
//! # Health is per source
//!
//! "No party is resident" and "the game closed" are different situations, and a consumer
//! usually wants to act differently on them. [`Health`] is reported per source and only
//! when it changes, so a healthy source is silent. Note that [`Health::Idle`] is the
//! ordinary state for solo play and is not an error.
//!
//! ```no_run
//! use std::sync::Arc;
//! use std::time::Duration;
//! use deadlock_events::{Engine, Event, Notification, PartySource, ReaderSource};
//! use deadlock_reader::Reader;
//!
//! let reader = Arc::new(Reader::attach()?);
//! let account = deadlock_reader::steam::active_account_id()?.unwrap_or(0);
//!
//! let (_engine, rx) = Engine::new()
//!     .with(ReaderSource::new(Arc::clone(&reader)).every(Duration::from_millis(100)))
//!     .with(PartySource::new(Arc::clone(&reader), account).every(Duration::from_secs(2)))
//!     .start()
//!     .expect("the engine could not spawn its threads");
//!
//! for note in rx {
//!     match note {
//!         Notification::Event { source, event: Event::Party(p) } => {
//!             println!("[{source}] {p:?}");
//!         }
//!         Notification::Event { source, event: Event::Match(m) } => {
//!             println!("[{source}] {m:?}");
//!         }
//!         Notification::Health { source, health } => {
//!             println!("[{source}] now {health:?}");
//!         }
//!         // Notification is #[non_exhaustive]: a new variant must not break this.
//!         _ => {}
//!     }
//! }
//! # Ok::<(), deadlock_reader::Error>(())
//! ```
//!
//! # Post-game capture
//!
//! [`PostGameSource`] captures each finished match's `CMsgMatchMetaDataContents` from the
//! client's heap. The client only holds it once the post-game screen is up, and only for
//! the match being shown, so the source waits [`DEFAULT_FIRST_ATTEMPT_DELAY`] after
//! `PostGame`, retries every [`postgame::DEFAULT_RETRY_INTERVAL`] and gives up after
//! [`DEFAULT_DEADLINE`]. It emits a [`PostGameEvent::Captured`] for the first complete copy,
//! a [`PostGameEvent::Updated`] whenever a later read differs, or a single
//! [`PostGameEvent::Missed`] so a consumer is not left waiting. The policy is a pure state
//! machine, [`CapturePolicy`], driven by an injected clock and fetcher.
//!
//! # Derived metrics
//!
//! Some of what a consumer wants is not in any snapshot: nothing in the client totals how
//! long a player spent stunned this match. [`crowd_control`], behind the non-default
//! `crowd-control` feature, accumulates that across ticks and reports it under Companion's
//! own metric names. It is a feature rather than a default because it needs
//! `deadlock-reader`'s `modifiers` read, which roughly triples the cost of a tick.
//!
//! The `landed_casts` module, behind the non-default `landed-casts` feature, is the same
//! layer for ability attribution: which casts landed, and how many enemies each caught.
//! Read its module docs before using the numbers - unlike crowd control, the rule that
//! groups applications into casts is **inferred** rather than observed, and the module
//! says so at every level. What is *not* inferred is which ability a cast belongs to: the
//! modifier's `m_nAbilitySubclassID` is a token of the ability's vdata key, measured 15 of
//! 15 against a live client, so a consumer holding the roster resolves a name from
//! `AbilityCastTotals::subclass_id` with no dependency from this crate to `deadlock-data`.
//!
//! The `objective_context` module, behind the non-default `objective-context` feature,
//! buckets a player's stats by *where* they happened - at the Midboss, at a walker, or in
//! the open. It is inferred end to end, the same way `landed_casts` is, and it needs
//! `deadlock-reader`'s `positions` at compile time rather than at runtime: a build with no
//! coordinates does not get a tracker that reports zeros, it does not get a tracker.
//!
//! # Persisting the stream
//!
//! Nothing above outlives the process. The `timeline` module, behind the non-default
//! `timeline` feature, is the durable half: `TimelineEvent` is one row of a per-match event
//! timeline - Companion's schema, under its own column names - and `EventSink` is a
//! two-method trait a consumer implements against whatever storage it already has. The
//! `timeline-jsonl` feature adds `JsonlSink`, an append-only one-record-per-line file
//! sink, deliberately not a database.
//!
//! Read that module's docs before using `TimelineEvent::clock`: it and
//! `TimelineEvent::offset_ms` are two different clocks, only one of them can be absent, and
//! only one of them can go backwards.
//!
//! # Adding your own source
//!
//! [`Source`] is a small trait: a name, an interval, and a `poll`. Anything implementing
//! it joins the same stream with the same health reporting, so a consumer does not care
//! where an event came from beyond the tag it carries.

#![forbid(unsafe_code)]

pub mod engine;
pub mod party;
pub mod postgame;
pub mod source;
pub mod sources;

#[cfg(feature = "crowd-control")]
pub mod crowd_control;

#[cfg(feature = "landed-casts")]
pub mod landed_casts;

#[cfg(feature = "objective-context")]
pub mod objective_context;

#[cfg(feature = "timeline-jsonl")]
pub mod sink;
#[cfg(feature = "timeline")]
pub mod timeline;

pub use engine::{Engine, EngineHandle, Notification};
pub use party::{PartyEvent, PartyTracker};
pub use postgame::{
    CapturePolicy, CaptureTiming, DEFAULT_DEADLINE, DEFAULT_FIRST_ATTEMPT_DELAY,
    DEFAULT_POSTGAME_INTERVAL, PostGameEvent, PostGameSource,
};
pub use source::{Event, Health, Poll, PollError, Source};

#[cfg(feature = "crowd-control")]
pub use crowd_control::{
    CrowdControlEvent, CrowdControlTotals, CrowdControlTracker, PlayerCrowdControl,
};

#[cfg(feature = "landed-casts")]
pub use landed_casts::{
    AbilityCastTotals, AbilityId, CasterCasts, DEFAULT_CAST_WINDOW, LandedCastEvent,
    LandedCastTracker, ability_key,
};
#[cfg(feature = "objective-context")]
pub use objective_context::{
    BucketTotals, FIGHT_GAP_SECONDS, FightContext, MAX_TICK_GAP, MIDBOSS_RADIUS, MatchFights,
    ObjectiveBucket, ObjectiveContextEvent, ObjectiveContextTracker, PlayerBuckets, RIFT_RADIUS,
    WALKER_RADIUS,
};
#[cfg(feature = "timeline-jsonl")]
pub use sink::{FlushPolicy, JsonlSink};
pub use sources::{DEFAULT_MATCH_INTERVAL, DEFAULT_PARTY_INTERVAL, PartySource, ReaderSource};
#[cfg(feature = "timeline")]
pub use timeline::{
    EventKind, EventSink, RecorderStatus, SinkError, TimelineEvent, TimelineRecorder,
};

/// Re-exported so a consumer needs one dependency to match on what arrives.
pub use deadlock_reader::events::Event as MatchEvent;
