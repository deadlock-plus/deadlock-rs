//! A durable, per-match event timeline.
//!
//! [`EventTracker`](deadlock_reader::events::EventTracker) produces a stream of changes and
//! then forgets them. Companion keeps its equivalent stream, one row per event, and drives
//! its match-timeline UI off the result; nothing here did. This module is the record shape,
//! and [`EventSink`] is the hole a consumer plugs its storage into.
//!
//! ```
//! use deadlock_events::timeline::{EventSink, SinkError, TimelineEvent, TimelineRecorder};
//! use deadlock_reader::events::Event;
//!
//! # struct Discard;
//! # impl EventSink for Discard {
//! #     fn write(&mut self, _: &TimelineEvent) -> Result<(), SinkError> { Ok(()) }
//! # }
//! let mut recorder = TimelineRecorder::new(Discard);
//!
//! // Once per tick: the events the reader produced, and the match clock they happened at.
//! let events = vec![Event::MidbossKilled { total: 1 }];
//! let clock = Some(612.5); // snapshot.clock.playing_seconds()
//! recorder.record_all(&events, clock)?;
//!
//! assert_eq!(recorder.status().recorded, 1);
//! # Ok::<(), SinkError>(())
//! ```
//!
//! # The schema is Companion's, the storage is not
//!
//! Companion's own table:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS match_events (
//!     match_id   TEXT NOT NULL,        -- u64 as string
//!     kind       TEXT NOT NULL,        -- kill | death | assist
//!     slot       INTEGER,              -- the (local) player slot the event belongs to
//!     clock      REAL,                 -- game clock seconds at the event (nullable)
//!     offset_ms  INTEGER NOT NULL,     -- ms into the continuous recording
//! ```
//!
//! [`TimelineEvent`] is those five fields under those five names, plus [`TimelineEvent::seq`]
//! (see below). It is deliberately no wider. In particular there is no `video_path`, no
//! `clip_id`: Companion's timeline exists to index a video recording and this crate has
//! no recording. The `assist` kind is present - `EventTracker` diffs `m_iPlayerAssists` -
//! though an assist is still not attributed to the kill it belongs to.
//! Emitting an always-null column would look like a measurement that came out empty.
//!
//! Three of Companion's five differ here, each for a stated reason:
//!
//! - `match_id` is nullable. Companion only ever records inside a match; this records
//!   whatever the reader emits, and the Hideout has no match id. `null` is that state, and
//!   is not the same as `0`.
//! - `kind` covers all seventeen of the reader's event variants, not three. Narrowing to
//!   kill/death would throw away objectives, pauses and item purchases that the stream
//!   already carries for free.
//! - `slot` is the slot the event belongs to, not specifically the local player's. The
//!   reader sees all twelve; which of them is local is a question [`LiveSnapshot`] answers,
//!   and duplicating the answer per row would let the two disagree.
//!
//! [`LiveSnapshot`]: deadlock_reader::snapshot::LiveSnapshot
//!
//! # Two clocks, and why they are two
//!
//! [`TimelineEvent::clock`] and [`TimelineEvent::offset_ms`] are not two spellings of the
//! same instant, and code that treats them as interchangeable will be wrong in exactly the
//! cases that matter.
//!
//! | | [`clock`](TimelineEvent::clock) | [`offset_ms`](TimelineEvent::offset_ms) |
//! |---|---|---|
//! | Means | seconds on the in-game match clock | milliseconds since this recorder started |
//! | Source | the caller, from [`MatchClock::playing_seconds`] | a [`std::time::Instant`] |
//! | Can be absent | **yes** | no |
//! | Can stand still | yes, the game pauses | no |
//! | Can go backwards | yes, a new match restarts it | no |
//! | Answers | "when in the match" | "when in the recording" |
//!
//! [`MatchClock::playing_seconds`]: deadlock_reader::snapshot::MatchClock::playing_seconds
//!
//! **An unreadable match clock is [`None`] and serialises as JSON `null`.** It is never
//! `0`, which would read as "at the moment the match started" - a measurement rather than
//! its absence. A non-finite reading is treated the same way, for the same reason: `NaN` is
//! not a time. Anything joining timeline rows to a match must therefore handle a missing
//! clock rather than assume one, and the offset is what remains usable when it does.
//!
//! Note also that `clock` is the *playing* clock, pauses removed, because that is the
//! number the game puts on screen and the number a viewer would name. Two events either
//! side of a five-minute pause are five minutes apart in `offset_ms` and not in `clock`.
//!
//! # Order within a tick
//!
//! The reader polls at around 10 Hz and a teamfight resolves several events into one tick.
//! Those events share a `clock` and share an `offset_ms`, so neither orders them, and the
//! order is real information - a kill before a death is a different story from the reverse.
//!
//! [`TimelineEvent::seq`] is the recorder's own counter and makes `(clock, offset_ms, seq)`
//! a total order that survives a consumer sorting the file. It is the one field with no
//! source in the game: it is assigned here, and says only what order this recorder saw
//! things in.

use std::time::Instant;

use deadlock_reader::events::Event as MatchEvent;

/// Which kind of event a row is.
///
/// Both an enum and a string, deliberately, because the two consumers this file has want
/// different things. On disk it is a lowercase `snake_case` string - `"kind":"kill"` - so
/// `grep` and `jq` work on the file without a schema. In Rust it is an enum, so a consumer
/// matches exhaustively and a typo does not compile.
///
/// [`EventKind::Other`] is the catch-all. `deadlock_reader::events::Event` is
/// `#[non_exhaustive]`, so a variant added there after this map was written lands here
/// rather than being dropped: the timeline still records that something happened at that
/// instant, to that slot, even though it cannot name it. It is also what an unknown string
/// deserialises to, so a file written by a newer build stays readable by an older one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventKind {
    /// [`MatchStarted`](MatchEvent::MatchStarted).
    MatchStarted,
    /// [`MatchEnded`](MatchEvent::MatchEnded). The attached scoreboard is not recorded.
    MatchEnded,
    /// [`MatchLeft`](MatchEvent::MatchLeft).
    MatchLeft,
    /// [`GameStateChanged`](MatchEvent::GameStateChanged).
    GameStateChanged,
    /// [`PauseChanged`](MatchEvent::PauseChanged).
    PauseChanged,
    /// [`Kill`](MatchEvent::Kill).
    Kill,
    /// [`Death`](MatchEvent::Death).
    Death,
    /// [`Assist`](MatchEvent::Assist).
    ///
    /// The third of Companion's three player kinds. It had no source here until
    /// `deadlock-reader` began diffing `m_iPlayerAssists`; before that an `assist` column
    /// could only ever have been empty, which reads as a measurement that came out zero.
    Assist,
    /// [`LevelUp`](MatchEvent::LevelUp).
    LevelUp,
    /// [`ItemPurchased`](MatchEvent::ItemPurchased).
    ItemPurchased,
    /// [`ItemLost`](MatchEvent::ItemLost).
    ItemLost,
    /// [`AbilityUpgraded`](MatchEvent::AbilityUpgraded).
    AbilityUpgraded,
    /// [`ObjectiveDestroyed`](MatchEvent::ObjectiveDestroyed).
    ObjectiveDestroyed,
    /// [`MidbossKilled`](MatchEvent::MidbossKilled).
    MidbossKilled,
    /// [`PlayerLeft`](MatchEvent::PlayerLeft).
    PlayerLeft,
    /// [`PlayerRejoined`](MatchEvent::PlayerRejoined).
    PlayerRejoined,
    /// [`StatAnomaly`](MatchEvent::StatAnomaly) - a tick whose counter events were withheld.
    ///
    /// Worth recording rather than dropping: a gap in the timeline that is explained is not
    /// the same as one that is not.
    StatAnomaly,
    /// [`CurrentPlayerChanged`](MatchEvent::CurrentPlayerChanged).
    CurrentPlayerChanged,
    /// [`HeroChanged`](MatchEvent::HeroChanged) - a player swapped hero mid-match.
    HeroChanged,
    /// [`ClockRewound`](MatchEvent::ClockRewound) - the stream was seeked.
    ///
    /// The one kind a consumer folding this file into a match history must act on rather
    /// than display: events either side of it may describe the same moment twice.
    ClockRewound,
    /// A kind this build does not know. See the type docs.
    Other,
}

impl EventKind {
    /// Every kind except [`EventKind::Other`], in the order the reader declares them.
    const NAMED: [(EventKind, &'static str); 20] = [
        (EventKind::MatchStarted, "match_started"),
        (EventKind::MatchEnded, "match_ended"),
        (EventKind::MatchLeft, "match_left"),
        (EventKind::GameStateChanged, "game_state_changed"),
        (EventKind::PauseChanged, "pause_changed"),
        (EventKind::Kill, "kill"),
        (EventKind::Death, "death"),
        (EventKind::Assist, "assist"),
        (EventKind::LevelUp, "level_up"),
        (EventKind::ItemPurchased, "item_purchased"),
        (EventKind::ItemLost, "item_lost"),
        (EventKind::AbilityUpgraded, "ability_upgraded"),
        (EventKind::ObjectiveDestroyed, "objective_destroyed"),
        (EventKind::MidbossKilled, "midboss_killed"),
        (EventKind::PlayerLeft, "player_left"),
        (EventKind::PlayerRejoined, "player_rejoined"),
        (EventKind::StatAnomaly, "stat_anomaly"),
        (EventKind::CurrentPlayerChanged, "current_player_changed"),
        (EventKind::HeroChanged, "hero_changed"),
        (EventKind::ClockRewound, "clock_rewound"),
    ];

    /// The kind of a reader event.
    ///
    /// A variant added to `deadlock_reader::events::Event` after this was written maps to
    /// [`EventKind::Other`] rather than failing to compile, because that enum is
    /// `#[non_exhaustive]` and a wildcard arm is required. That is a real fidelity limit:
    /// the timeline will show the instant but not the name.
    pub fn of(event: &MatchEvent) -> EventKind {
        match event {
            MatchEvent::MatchStarted { .. } => EventKind::MatchStarted,
            MatchEvent::MatchEnded { .. } => EventKind::MatchEnded,
            MatchEvent::MatchLeft { .. } => EventKind::MatchLeft,
            MatchEvent::GameStateChanged { .. } => EventKind::GameStateChanged,
            MatchEvent::PauseChanged { .. } => EventKind::PauseChanged,
            MatchEvent::Kill { .. } => EventKind::Kill,
            MatchEvent::Death { .. } => EventKind::Death,
            MatchEvent::Assist { .. } => EventKind::Assist,
            MatchEvent::LevelUp { .. } => EventKind::LevelUp,
            MatchEvent::ItemPurchased { .. } => EventKind::ItemPurchased,
            MatchEvent::ItemLost { .. } => EventKind::ItemLost,
            MatchEvent::AbilityUpgraded { .. } => EventKind::AbilityUpgraded,
            MatchEvent::ObjectiveDestroyed { .. } => EventKind::ObjectiveDestroyed,
            MatchEvent::MidbossKilled { .. } => EventKind::MidbossKilled,
            MatchEvent::PlayerLeft { .. } => EventKind::PlayerLeft,
            MatchEvent::PlayerRejoined { .. } => EventKind::PlayerRejoined,
            MatchEvent::StatAnomaly { .. } => EventKind::StatAnomaly,
            MatchEvent::CurrentPlayerChanged { .. } => EventKind::CurrentPlayerChanged,
            MatchEvent::HeroChanged { .. } => EventKind::HeroChanged,
            MatchEvent::ClockRewound { .. } => EventKind::ClockRewound,
            _ => EventKind::Other,
        }
    }

    /// The name this kind is written under.
    pub fn as_str(&self) -> &'static str {
        Self::NAMED
            .iter()
            .find(|(kind, _)| kind == self)
            .map_or("other", |(_, name)| *name)
    }

    /// The kind a name refers to, or [`None`] if this build does not know it.
    ///
    /// Distinct from what deserialisation does, which maps an unknown name to
    /// [`EventKind::Other`]. A caller asking the question directly usually wants to know
    /// that the name was not recognised - Companion's `assist`, for instance, has no
    /// source in this workspace and so is not a kind here.
    pub fn from_name(name: &str) -> Option<EventKind> {
        if name == "other" {
            return Some(EventKind::Other);
        }
        Self::NAMED
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(kind, _)| *kind)
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for EventKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<EventKind, D::Error> {
        let name = <std::borrow::Cow<'_, str> as serde::Deserialize>::deserialize(d)?;
        Ok(EventKind::from_name(&name).unwrap_or(EventKind::Other))
    }
}

/// The lobby slot an event belongs to, where it has one.
///
/// Match-level events - a pause, an objective, the Midboss - have no player, and get
/// [`None`] rather than a stand-in.
fn slot_of(event: &MatchEvent) -> Option<u32> {
    match event {
        MatchEvent::Kill { slot, .. }
        | MatchEvent::Death { slot, .. }
        | MatchEvent::Assist { slot, .. }
        | MatchEvent::LevelUp { slot, .. }
        | MatchEvent::ItemPurchased { slot, .. }
        | MatchEvent::ItemLost { slot, .. }
        | MatchEvent::AbilityUpgraded { slot, .. }
        | MatchEvent::PlayerLeft { slot, .. }
        | MatchEvent::PlayerRejoined { slot, .. }
        | MatchEvent::CurrentPlayerChanged { slot, .. } => *slot,
        _ => None,
    }
}

/// A match id is written as a JSON **string**, not a JSON number.
///
/// Companion's column is `TEXT NOT NULL -- u64 as string` and the reason outlives the
/// change of storage: a JSON number is a double to `jq`, to every browser, and to most
/// scripting-language JSON parsers, and a 19-digit id does not survive a round trip through
/// one. Rust would read it back exactly; nothing else would, and a JSONL file exists to be
/// read by other things.
mod match_id_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(value: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(id) => s.serialize_str(&id.to_string()),
            None => s.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        let raw = Option::<std::borrow::Cow<'_, str>>::deserialize(d)?;
        raw.map(|text| text.parse::<u64>().map_err(serde::de::Error::custom))
            .transpose()
    }
}

/// One row of the timeline: something happened, to whom, and when - twice over.
///
/// The fields are Companion's `match_events` columns, under Companion's names, plus
/// [`seq`](TimelineEvent::seq). See the [module docs](self) for the two-clock rule, which is
/// the part of this type that is easy to get wrong.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TimelineEvent {
    /// The match this happened in, or [`None`] outside one.
    ///
    /// `u64`, which is the width `MatchID_t` is. Read as a `u32` in this workspace until
    /// build 6683, a narrowing that stayed lossless only because live ids happened to be
    /// small; a truncated match id is a plausible-looking number that nothing reports.
    ///
    /// Written as a JSON **string**, not a JSON number. Companion's column is
    /// `TEXT NOT NULL -- u64 as string` and the reason outlives the change of storage: a
    /// JSON number is a double to `jq`, to every browser and to most scripting-language
    /// parsers, and an id above 2^53 does not survive a round trip through one. Rust would
    /// read it back exactly; nothing else would, and a JSONL file exists to be read by
    /// other things.
    #[serde(with = "match_id_string")]
    pub match_id: Option<u64>,
    /// What happened. A string on disk, an enum here; see [`EventKind`].
    pub kind: EventKind,
    /// The lobby slot (`1..=12`) this belongs to, or [`None`] for a match-level event.
    pub slot: Option<u32>,
    /// Seconds on the in-game match clock, pauses removed.
    ///
    /// [`None`] when the clock could not be read, which is a real and common state: the
    /// clock is derived from entity simulation time and there is not always an entity to
    /// derive it from. **It is never `0` for "unknown"**; `0` means the match had just
    /// started. A non-finite reading is normalised to [`None`] by
    /// [`TimelineEvent::new`], because `NaN` is not a time.
    ///
    /// Do not use this to order events. Use [`offset_ms`](TimelineEvent::offset_ms), which
    /// exists.
    pub clock: Option<f32>,
    /// Milliseconds since the recorder started, from a monotonic [`Instant`].
    ///
    /// Never absent, never backwards, and unaffected by pauses or by a new match starting.
    /// This is the field that orders the file; [`clock`](TimelineEvent::clock) is the field
    /// that locates an event in the match. It is *not* a wall-clock time and says nothing
    /// about when the recording began.
    pub offset_ms: u64,
    /// The order this recorder saw the event in, from zero.
    ///
    /// Assigned here rather than read from the game. Several events routinely share a tick
    /// and therefore share both other clocks, so without this a consumer that sorts the
    /// file loses the order it was written in. `(clock, offset_ms, seq)` is a total order;
    /// `(clock, offset_ms)` is not.
    pub seq: u64,
}

impl TimelineEvent {
    /// Assemble a row, normalising a non-finite clock to [`None`].
    ///
    /// That normalisation is the only thing this does beyond a struct literal, and it is
    /// why it exists: `NaN` and the infinities are not readings, and letting one through
    /// would put a value in the file that a reader has to invent a meaning for.
    pub fn new(
        match_id: Option<u64>,
        kind: EventKind,
        slot: Option<u32>,
        clock: Option<f32>,
        offset_ms: u64,
        seq: u64,
    ) -> TimelineEvent {
        TimelineEvent {
            match_id,
            kind,
            slot,
            clock: clock.filter(|c| c.is_finite()),
            offset_ms,
            seq,
        }
    }
}

/// Why a sink could not take a record.
///
/// Deliberately not generic over a storage error type: the point of [`EventSink`] is that a
/// consumer can implement it against a database this crate has never heard of, and a
/// generic error parameter would put that database's error type in every signature that
/// touches a sink.
#[derive(Debug)]
#[non_exhaustive]
pub enum SinkError {
    /// The underlying writer failed.
    Io(std::io::Error),
    /// A record could not be turned into bytes.
    ///
    /// Carried as a string rather than a `serde_json::Error` because this type is available
    /// without the `timeline-jsonl` feature, and a sink writing some other format has its
    /// own encoder. Unreachable for [`TimelineEvent`] as it stands - every field is a
    /// primitive - and kept for implementations whose records are not.
    Encode(String),
    /// Something else the sink wants to report.
    Other(String),
}

impl std::fmt::Display for SinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SinkError::Io(e) => write!(f, "{e}"),
            SinkError::Encode(m) => write!(f, "could not encode a timeline record: {m}"),
            SinkError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for SinkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SinkError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SinkError {
    fn from(e: std::io::Error) -> SinkError {
        SinkError::Io(e)
    }
}

/// Somewhere a timeline can be written.
///
/// Two methods, one of them defaulted, so that a consumer wanting SQLite or Postgres can
/// implement it in a dozen lines and this crate never has to pick a database.
/// [`JsonlSink`](crate::sink::JsonlSink) is one implementation, not the interface.
///
/// # Why `write` returns a `Result`
///
/// The alternative - an infallible `write` that swallows failures - would leave an
/// implementor no way to say a row did not land, and would make a full disk look identical
/// to a quiet match. So the trait is honest and the *caller* is given somewhere to put the
/// decision: [`TimelineRecorder`] also returns the error, and additionally counts it in
/// [`RecorderStatus`], so a poll loop that has nothing useful to do mid-tick can write
/// `let _ = recorder.record(..)` and still find out later. `Result` is `#[must_use]`, so
/// ignoring it is at least deliberate.
///
/// The trait is not `Send`. Box it as `Box<dyn EventSink + Send>` if it has to cross a
/// thread; requiring it here would rule out sinks that legitimately cannot.
pub trait EventSink {
    /// Take one record.
    ///
    /// Returning `Ok` means the sink accepted it, not that it reached durable storage. A
    /// buffering sink may still lose it; see the implementation's own docs for what its
    /// `Ok` is worth.
    fn write(&mut self, event: &TimelineEvent) -> Result<(), SinkError>;

    /// Push anything buffered as far towards storage as the sink can.
    ///
    /// Defaulted to a no-op for sinks that do not buffer.
    fn flush(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
}

impl<S: EventSink + ?Sized> EventSink for Box<S> {
    fn write(&mut self, event: &TimelineEvent) -> Result<(), SinkError> {
        (**self).write(event)
    }

    fn flush(&mut self) -> Result<(), SinkError> {
        (**self).flush()
    }
}

/// What a [`TimelineRecorder`] has managed so far.
///
/// This exists so that a caller which deliberately ignores the `Result` from every
/// [`TimelineRecorder::record`] can still discover that the timeline is incomplete. It is
/// the same idea as [`Health`](crate::Health): report out of band, on the consumer's own
/// schedule, rather than interrupting a tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecorderStatus {
    /// Records the sink accepted.
    pub recorded: u64,
    /// Records the sink refused. Each one is a hole in the timeline.
    pub failed: u64,
    /// The first failure, which is usually the one that explains the rest.
    pub first_error: Option<String>,
    /// The most recent failure.
    pub last_error: Option<String>,
}

impl RecorderStatus {
    /// Whether every record offered so far was accepted.
    pub fn is_healthy(&self) -> bool {
        self.failed == 0
    }
}

/// Turns reader events into [`TimelineEvent`]s and hands them to a sink.
///
/// It is stateful for two reasons, both of which would otherwise land on the caller:
///
/// - **The match id.** Only three of the reader's seventeen variants carry one, so
///   something has to remember it between them. The recorder latches it on
///   [`MatchStarted`](MatchEvent::MatchStarted) and [`MatchEnded`](MatchEvent::MatchEnded)
///   and clears it after recording [`MatchLeft`](MatchEvent::MatchLeft) - the row for
///   leaving still names the match that was left.
/// - **The recording offset.** [`TimelineEvent::offset_ms`] is measured from when this
///   recorder was constructed, so "when recording started" is defined by its lifetime.
///
/// The match clock is *not* held, and is passed to each call instead. It belongs to the
/// snapshot the events came from, and a recorder holding its own copy could only ever hold
/// a stale one.
pub struct TimelineRecorder<S: EventSink> {
    sink: S,
    started: Instant,
    #[cfg(test)]
    scripted: Vec<u64>,
    seq: u64,
    match_id: Option<u64>,
    last_offset_ms: u64,
    status: RecorderStatus,
    last: Option<TimelineEvent>,
}

impl<S: EventSink> TimelineRecorder<S> {
    /// Start recording into `sink`. The recording offset is measured from this call.
    pub fn new(sink: S) -> TimelineRecorder<S> {
        TimelineRecorder {
            sink,
            started: Instant::now(),
            #[cfg(test)]
            scripted: Vec::new(),
            seq: 0,
            match_id: None,
            last_offset_ms: 0,
            status: RecorderStatus::default(),
            last: None,
        }
    }

    /// A recorder whose offsets come from `offsets` rather than from the clock.
    ///
    /// Test-only. Wall time cannot be made to stand still or run backwards on demand, and
    /// both are things the monotonicity rule has to be tested against.
    #[cfg(test)]
    fn scripted(sink: S, offsets: &[u64]) -> TimelineRecorder<S> {
        let mut recorder = TimelineRecorder::new(sink);
        recorder.scripted = offsets.iter().copied().rev().collect();
        recorder
    }

    #[cfg(not(test))]
    fn raw_offset(&mut self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    #[cfg(test)]
    fn raw_offset(&mut self) -> u64 {
        match self.scripted.pop() {
            Some(scripted) => scripted,
            None => u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }

    /// Record one event as of `clock`, the in-game match clock in seconds.
    ///
    /// Pass [`None`] for `clock` when it could not be read - typically
    /// [`MatchClock::playing_seconds`](deadlock_reader::snapshot::MatchClock::playing_seconds)
    /// straight off the snapshot the event came from. Do not substitute `0`.
    ///
    /// The returned `Result` is the immediate signal. Ignoring it is allowed and sometimes
    /// right in a poll loop, and [`TimelineRecorder::status`] is there so that doing so
    /// does not make the failure invisible.
    pub fn record(&mut self, event: &MatchEvent, clock: Option<f32>) -> Result<(), SinkError> {
        if let MatchEvent::MatchStarted { match_id } | MatchEvent::MatchEnded { match_id, .. } =
            event
        {
            self.match_id = *match_id;
        }

        // `Instant` is monotonic by contract, so this clamp should never bind. It is here
        // because `offset_ms` never going backwards is a promise the record makes to its
        // readers, and a promise nothing enforces is a comment.
        let offset_ms = self.raw_offset().max(self.last_offset_ms);
        self.last_offset_ms = offset_ms;

        let record = TimelineEvent::new(
            self.match_id,
            EventKind::of(event),
            slot_of(event),
            clock,
            offset_ms,
            self.seq,
        );
        self.seq += 1;

        if matches!(event, MatchEvent::MatchLeft { .. }) {
            self.match_id = None;
        }

        let outcome = self.sink.write(&record);
        match &outcome {
            Ok(()) => self.status.recorded += 1,
            Err(error) => {
                let message = error.to_string();
                self.status.failed += 1;
                if self.status.first_error.is_none() {
                    self.status.first_error = Some(message.clone());
                }
                self.status.last_error = Some(message);
            }
        }
        self.last = Some(record);
        outcome
    }

    /// Record a whole tick's worth of events at one clock reading.
    ///
    /// Every event is offered to the sink even if an earlier one failed - stopping at the
    /// first failure would turn one bad write into a lost tick - and the first error is
    /// returned. [`TimelineRecorder::status`] has the count.
    pub fn record_all(
        &mut self,
        events: &[MatchEvent],
        clock: Option<f32>,
    ) -> Result<(), SinkError> {
        let mut first: Option<SinkError> = None;
        for event in events {
            if let Err(error) = self.record(event, clock)
                && first.is_none()
            {
                first = Some(error);
            }
        }
        match first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Push the sink's buffers towards storage.
    pub fn flush(&mut self) -> Result<(), SinkError> {
        self.sink.flush()
    }

    /// What has been recorded and what has failed. See [`RecorderStatus`].
    pub fn status(&self) -> &RecorderStatus {
        &self.status
    }

    /// The match id currently latched, if any.
    pub fn match_id(&self) -> Option<u64> {
        self.match_id
    }

    /// The most recent record built, whether or not the sink accepted it.
    pub fn last(&self) -> Option<&TimelineEvent> {
        self.last.as_ref()
    }

    /// The sink, for a caller that needs to ask it something.
    pub fn sink(&self) -> &S {
        &self.sink
    }

    /// The sink, mutably.
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// Give the sink back, ending the recording.
    pub fn into_sink(self) -> S {
        self.sink
    }
}

impl<S: EventSink + std::fmt::Debug> std::fmt::Debug for TimelineRecorder<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimelineRecorder")
            .field("sink", &self.sink)
            .field("seq", &self.seq)
            .field("match_id", &self.match_id)
            .field("last_offset_ms", &self.last_offset_ms)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deadlock_reader::core_types::{GameState, HeroId, ItemId, Team};
    use deadlock_reader::events::Event as MatchEvent;
    use deadlock_reader::snapshot::{LiveSnapshot, ObjectiveKind};

    /// Collects what the recorder handed the sink, so a test can assert on it.
    #[derive(Default)]
    struct MemorySink {
        events: Vec<TimelineEvent>,
        flushes: usize,
    }

    impl EventSink for MemorySink {
        fn write(&mut self, event: &TimelineEvent) -> Result<(), SinkError> {
            self.events.push(event.clone());
            Ok(())
        }

        fn flush(&mut self) -> Result<(), SinkError> {
            self.flushes += 1;
            Ok(())
        }
    }

    /// Fails every write, so the recorder's error path is reachable without a filesystem.
    struct DeadSink;

    impl EventSink for DeadSink {
        fn write(&mut self, _: &TimelineEvent) -> Result<(), SinkError> {
            Err(SinkError::from(std::io::Error::other("the disk went away")))
        }
    }

    /// One of every `deadlock_reader::events::Event` variant, in declaration order.
    fn one_of_each() -> Vec<MatchEvent> {
        vec![
            MatchEvent::MatchStarted { match_id: Some(7) },
            MatchEvent::MatchEnded {
                match_id: Some(7),
                final_state: Box::new(LiveSnapshot::default()),
            },
            MatchEvent::MatchLeft { match_id: Some(7) },
            MatchEvent::GameStateChanged {
                from: GameState::PreGameWait,
                to: GameState::GameInProgress,
            },
            MatchEvent::PauseChanged { paused: true },
            MatchEvent::Kill {
                slot: Some(3),
                hero: Some(HeroId(6)),
                total: 4,
                delta: 1,
            },
            MatchEvent::Death {
                slot: Some(4),
                hero: Some(HeroId(6)),
                total: 2,
                delta: 1,
            },
            MatchEvent::Assist {
                slot: Some(4),
                hero: None,
                total: 3,
                delta: 1,
            },
            MatchEvent::LevelUp {
                slot: Some(5),
                hero: Some(HeroId(6)),
                level: 9,
            },
            MatchEvent::ItemPurchased {
                slot: Some(6),
                item: ItemId(11),
            },
            MatchEvent::ItemLost {
                slot: Some(7),
                item: ItemId(11),
            },
            MatchEvent::AbilityUpgraded {
                slot: Some(8),
                ability: ItemId(12),
                points: 2,
            },
            MatchEvent::ObjectiveDestroyed {
                kind: ObjectiveKind::Walker,
                team: Some(Team::AMBER),
            },
            MatchEvent::MidbossKilled { total: 1 },
            MatchEvent::PlayerLeft {
                slot: Some(9),
                name: None,
            },
            MatchEvent::PlayerRejoined {
                slot: Some(9),
                name: None,
            },
            MatchEvent::StatAnomaly { players: 8 },
            MatchEvent::HeroChanged {
                slot: Some(4),
                from: HeroId(1),
                to: HeroId(7),
            },
            MatchEvent::ClockRewound {
                from: 1644.0,
                to: 1585.0,
            },
            MatchEvent::CurrentPlayerChanged {
                slot: Some(1),
                hero: Some(HeroId(6)),
            },
        ]
    }

    /// A JSONL consumer greps for `"kind":"kill"`; a Rust consumer wants an exhaustive
    /// match. One field serving both is the whole point of making `kind` an enum that
    /// serialises as a string, so both halves are asserted together.
    #[test]
    fn kind_is_a_string_on_disk_and_an_enum_in_rust() {
        let ev = TimelineEvent::new(Some(1), EventKind::Kill, Some(3), Some(12.0), 500, 0);
        let line = serde_json::to_string(&ev).unwrap();
        assert!(
            line.contains("\"kind\":\"kill\""),
            "a grep-able string is the point of the on-disk form: {line}"
        );

        let matched = match ev.kind {
            EventKind::Kill => "kill",
            _ => "something else",
        };
        assert_eq!(matched, "kill");
    }

    /// Every variant the reader can emit has to land on a kind of its own. A mapping that
    /// collapses two variants onto one name loses information the timeline exists to keep.
    #[test]
    fn every_reader_event_maps_to_a_distinct_named_kind() {
        let kinds: Vec<EventKind> = one_of_each().iter().map(EventKind::of).collect();

        let mut produced: Vec<&str> = kinds.iter().map(EventKind::as_str).collect();
        let mut named: Vec<&str> = EventKind::NAMED.iter().map(|(_, n)| *n).collect();
        produced.sort_unstable();
        produced.dedup();
        named.sort_unstable();
        assert_eq!(
            produced, named,
            "the fixture and the name table disagree about which kinds exist"
        );

        for (event, kind) in one_of_each().iter().zip(&kinds) {
            assert_ne!(
                *kind,
                EventKind::Other,
                "{event:?} fell through to the catch-all"
            );
        }

        let mut names: Vec<&str> = kinds.iter().map(EventKind::as_str).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(
            before,
            names.len(),
            "two variants share a kind name: {names:?}"
        );
    }

    /// All three of Companion's player kinds have a source, including `assist`.
    ///
    /// An earlier revision asserted the opposite - that no `assist` kind should exist,
    /// because `PlayerRow::assists` was read but never diffed, and a column that is always
    /// empty reads as a measurement that came out zero. That was right at the time and is
    /// no longer: `deadlock_reader::events::EventTracker` now diffs `m_iPlayerAssists` into
    /// `MatchEvent::Assist`, so the kind is backed by an event rather than by a hope.
    #[test]
    fn every_companion_player_kind_has_a_source() {
        for name in ["kill", "death", "assist"] {
            assert!(EventKind::from_name(name).is_some(), "no kind named {name}");
        }
        assert_eq!(
            EventKind::of(&MatchEvent::Assist {
                slot: Some(4),
                hero: None,
                total: 3,
                delta: 1,
            }),
            EventKind::Assist
        );
        assert_eq!(EventKind::Assist.as_str(), "assist");
    }

    /// The match clock and the recording offset are different clocks and the record has to
    /// keep them apart. This is the test that catches them being swapped.
    #[test]
    fn the_match_clock_and_the_recording_offset_are_separate_fields() {
        let ev = TimelineEvent::new(Some(1), EventKind::Death, Some(2), Some(612.5), 90_000, 4);
        assert_eq!(ev.clock, Some(612.5));
        assert_eq!(ev.offset_ms, 90_000);

        let json: serde_json::Value = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["clock"].as_f64().unwrap(), 612.5);
        assert_eq!(json["offset_ms"].as_u64().unwrap(), 90_000);
    }

    /// An unreadable match clock must not serialise as `0`, which reads as "at the very
    /// start of the match" - a measurement, not a gap.
    #[test]
    fn an_absent_match_clock_is_null_and_never_zero() {
        let ev = TimelineEvent::new(Some(1), EventKind::Kill, Some(2), None, 1_000, 0);
        let json: serde_json::Value = serde_json::to_value(&ev).unwrap();
        assert!(json["clock"].is_null(), "got {}", json["clock"]);
        assert!(
            !json["clock"].is_number(),
            "an unreadable clock became the number {}, which reads as a measurement",
            json["clock"]
        );
    }

    /// A non-finite clock is not a measurement either. It is normalised to absent at
    /// construction so nothing downstream has to guess what `NaN` meant.
    #[test]
    fn a_non_finite_match_clock_is_recorded_as_absent() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let ev = TimelineEvent::new(Some(1), EventKind::Kill, None, Some(bad), 0, 0);
            assert_eq!(
                ev.clock, None,
                "{bad} should not survive as a clock reading"
            );
        }
    }

    /// The recording offset is monotonic by contract. A record whose offset went backwards
    /// would break every consumer that treats the file as ordered.
    #[test]
    fn the_recording_offset_never_goes_backwards() {
        let mut rec = TimelineRecorder::scripted(MemorySink::default(), &[0, 500, 100, 900]);
        for _ in 0..4 {
            rec.record(&MatchEvent::MidbossKilled { total: 1 }, None)
                .unwrap();
        }

        let offsets: Vec<u64> = rec.sink().events.iter().map(|e| e.offset_ms).collect();
        assert_eq!(offsets, vec![0, 500, 500, 900]);
        assert!(
            offsets.windows(2).all(|w| w[1] >= w[0]),
            "offsets went backwards: {offsets:?}"
        );
    }

    /// The match clock resets between matches and can vanish mid-match; the offset does
    /// neither. Conflating them would make either the reset visible in the offset or the
    /// gap invisible in the clock.
    #[test]
    fn a_match_clock_reset_does_not_disturb_the_recording_offset() {
        let mut rec = TimelineRecorder::scripted(MemorySink::default(), &[1_000, 2_000, 3_000]);
        rec.record(
            &MatchEvent::Kill {
                slot: Some(1),
                hero: None,
                total: 1,
                delta: 1,
            },
            Some(300.0),
        )
        .unwrap();
        rec.record(&MatchEvent::MatchLeft { match_id: Some(7) }, None)
            .unwrap();
        rec.record(&MatchEvent::MatchStarted { match_id: Some(8) }, Some(5.0))
            .unwrap();

        let events = &rec.sink().events;
        assert_eq!(
            events.iter().map(|e| e.clock).collect::<Vec<_>>(),
            vec![Some(300.0), None, Some(5.0)]
        );
        assert_eq!(
            events.iter().map(|e| e.offset_ms).collect::<Vec<_>>(),
            vec![1_000, 2_000, 3_000]
        );
    }

    /// Several events land on one tick and share a clock and an offset. Order between them
    /// is real information, so the record carries a sequence number rather than leaving it
    /// implicit in file order - a consumer sorting by clock has to be able to recover it.
    #[test]
    fn events_sharing_a_clock_keep_their_order_through_a_sort() {
        let mut rec = TimelineRecorder::scripted(MemorySink::default(), &[7_000, 7_000, 7_000]);
        let tick = vec![
            MatchEvent::Kill {
                slot: Some(1),
                hero: None,
                total: 1,
                delta: 1,
            },
            MatchEvent::Death {
                slot: Some(2),
                hero: None,
                total: 1,
                delta: 1,
            },
            MatchEvent::LevelUp {
                slot: Some(1),
                hero: None,
                level: 4,
            },
        ];
        rec.record_all(&tick, Some(60.0)).unwrap();

        let mut events = rec.sink().events.clone();
        assert_eq!(
            events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        events.reverse();
        events.sort_by(|a, b| {
            a.clock
                .partial_cmp(&b.clock)
                .unwrap()
                .then(a.offset_ms.cmp(&b.offset_ms))
                .then(a.seq.cmp(&b.seq))
        });
        assert_eq!(
            events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            vec![EventKind::Kill, EventKind::Death, EventKind::LevelUp],
            "a stable order was not recoverable from the record alone"
        );
    }

    /// `MatchID_t` is a `uint64`. This workspace already fixed a 32-bit narrowing on it
    /// once; the timeline must not reintroduce one.
    #[test]
    fn a_match_id_keeps_all_64_bits() {
        let wide = u64::MAX - 1;
        assert!(wide > u64::from(u32::MAX));

        let ev = TimelineEvent::new(Some(wide), EventKind::Kill, None, None, 0, 0);
        let line = serde_json::to_string(&ev).unwrap();
        assert!(line.contains("18446744073709551614"), "{line}");

        let back: TimelineEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back.match_id, Some(wide));
    }

    /// Companion stores the id as `TEXT -- u64 as string`, and the reason survives the
    /// change of storage: JSON numbers are doubles to most readers, so `jq` and every
    /// browser-side consumer silently round a 19-digit id. A quoted id cannot be rounded.
    #[test]
    fn a_match_id_is_quoted_so_a_double_based_reader_cannot_round_it() {
        let wide = 9_007_199_254_740_993_u64;
        assert_ne!(
            wide as f64 as u64, wide,
            "the chosen id does not actually demonstrate the rounding"
        );

        let line = serde_json::to_string(&TimelineEvent::new(
            Some(wide),
            EventKind::Kill,
            None,
            None,
            0,
            0,
        ))
        .unwrap();
        assert!(
            line.contains("\"match_id\":\"9007199254740993\""),
            "the id has to be a JSON string, not a JSON number: {line}"
        );

        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(json["match_id"].is_string());
        assert_eq!(
            json["match_id"].as_str().unwrap().parse::<u64>().unwrap(),
            wide
        );
    }

    /// No match id is a real state - the Hideout, a menu - and is not the same as an id of
    /// zero.
    #[test]
    fn an_absent_match_id_is_null_and_never_zero() {
        let json: serde_json::Value =
            serde_json::to_value(TimelineEvent::new(None, EventKind::Kill, None, None, 0, 0))
                .unwrap();
        assert!(json["match_id"].is_null(), "got {}", json["match_id"]);
    }

    /// The recorder holds the match id so the caller does not have to thread it through
    /// every event: only three of the seventeen variants carry one.
    #[test]
    fn the_recorder_carries_the_match_id_across_events_that_do_not_name_one() {
        let mut rec = TimelineRecorder::new(MemorySink::default());
        rec.record(&MatchEvent::MatchStarted { match_id: Some(42) }, None)
            .unwrap();
        rec.record(&MatchEvent::MidbossKilled { total: 1 }, None)
            .unwrap();
        rec.record(&MatchEvent::MatchLeft { match_id: Some(42) }, None)
            .unwrap();
        rec.record(&MatchEvent::MidbossKilled { total: 2 }, None)
            .unwrap();

        let ids: Vec<Option<u64>> = rec.sink().events.iter().map(|e| e.match_id).collect();
        assert_eq!(ids, vec![Some(42), Some(42), Some(42), None]);
    }

    /// Player events carry the slot they belong to; match-level ones have no player and
    /// must say `null` rather than pick one.
    #[test]
    fn only_player_events_carry_a_slot() {
        let mut rec = TimelineRecorder::new(MemorySink::default());
        rec.record(
            &MatchEvent::Kill {
                slot: Some(11),
                hero: None,
                total: 1,
                delta: 1,
            },
            None,
        )
        .unwrap();
        rec.record(&MatchEvent::MidbossKilled { total: 1 }, None)
            .unwrap();

        assert_eq!(rec.sink().events[0].slot, Some(11));
        assert_eq!(rec.sink().events[1].slot, None);
    }

    /// A failed write is not allowed to be silent. The `Result` is the immediate signal and
    /// `unused_must_use` makes ignoring it deliberate; the status is the one a poll loop
    /// that deliberately ignores it can still read.
    #[test]
    fn a_failed_write_is_both_returned_and_counted() {
        let mut rec = TimelineRecorder::new(DeadSink);
        let first = rec.record(&MatchEvent::MidbossKilled { total: 1 }, None);
        assert!(first.is_err(), "a sink that failed must not report success");

        let _ = rec.record(&MatchEvent::MidbossKilled { total: 2 }, None);

        let status = rec.status();
        assert_eq!(status.failed, 2);
        assert_eq!(status.recorded, 0);
        assert!(!status.is_healthy());
        assert!(status.first_error.as_deref().unwrap().contains("disk"));
        assert!(status.last_error.is_some());
    }

    /// A healthy recorder counts what it wrote, so "nothing happened" is distinguishable
    /// from "everything failed".
    #[test]
    fn a_healthy_recorder_counts_what_it_wrote() {
        let mut rec = TimelineRecorder::new(MemorySink::default());
        rec.record(&MatchEvent::MidbossKilled { total: 1 }, None)
            .unwrap();
        assert_eq!(rec.status().recorded, 1);
        assert_eq!(rec.status().failed, 0);
        assert!(rec.status().is_healthy());
    }

    /// `record_all` is the shape a poll loop actually has - one tick, several events. A
    /// failure part way through must not stop the rest being offered to the sink.
    #[test]
    fn record_all_keeps_going_past_a_failure_and_reports_it() {
        let mut rec = TimelineRecorder::new(DeadSink);
        let err = rec.record_all(
            &[
                MatchEvent::MidbossKilled { total: 1 },
                MatchEvent::MidbossKilled { total: 2 },
            ],
            None,
        );
        assert!(err.is_err());
        assert_eq!(rec.status().failed, 2, "the second event was never offered");
    }

    /// A sequence number that restarted, or that skipped a failed write, would stop being a
    /// total order over the recording.
    #[test]
    fn the_sequence_number_is_dense_and_strictly_increasing() {
        let mut rec = TimelineRecorder::new(MemorySink::default());
        for i in 0..5 {
            rec.record(&MatchEvent::MidbossKilled { total: i }, None)
                .unwrap();
        }
        let seqs: Vec<u64> = rec.sink().events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4]);
    }

    /// The timeline is an index of when things happened, not an archive of state. The
    /// scoreboard `MatchEnded` carries is deliberately dropped, and the record still has to
    /// be the same six fields wide as every other one.
    #[test]
    fn match_ended_records_the_instant_and_not_the_scoreboard() {
        let mut rec = TimelineRecorder::new(MemorySink::default());
        rec.record(
            &MatchEvent::MatchEnded {
                match_id: Some(9),
                final_state: Box::new(LiveSnapshot::default()),
            },
            Some(1800.0),
        )
        .unwrap();

        let line = serde_json::to_string(&rec.sink().events[0]).unwrap();
        assert!(line.contains("\"kind\":\"match_ended\""), "{line}");
        assert!(
            !line.contains("players"),
            "the snapshot leaked into the timeline: {line}"
        );
        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 6, "{line}");
    }

    /// The record round-trips, which is what makes the file readable back into Rust rather
    /// than only greppable.
    #[test]
    fn a_record_round_trips_through_json() {
        let ev = TimelineEvent::new(
            Some(1234),
            EventKind::ItemPurchased,
            Some(6),
            Some(90.25),
            12_345,
            3,
        );
        let back: TimelineEvent =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(back, ev);
    }

    /// A kind written by a newer version of this crate must not make an older reader throw
    /// the record away: the instant and the slot are still true.
    #[test]
    fn an_unknown_kind_reads_back_as_other_rather_than_failing() {
        let line = "{\"match_id\":null,\"kind\":\"teleported\",\"slot\":2,\"clock\":null,\"offset_ms\":5,\"seq\":1}";
        let ev: TimelineEvent = serde_json::from_str(line).unwrap();
        assert_eq!(ev.kind, EventKind::Other);
        assert_eq!(ev.slot, Some(2));
        assert_eq!(ev.offset_ms, 5);
    }

    /// Held as a trait object by anything that wants to swap sinks at runtime.
    #[test]
    fn sinks_are_object_safe() {
        let mut sink: Box<dyn EventSink> = Box::new(MemorySink::default());
        sink.write(&TimelineEvent::new(None, EventKind::Kill, None, None, 0, 0))
            .unwrap();
        sink.flush().unwrap();
    }
}
