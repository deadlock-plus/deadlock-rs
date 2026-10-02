//! Accumulating crowd control across ticks.
//!
//! `deadlock-reader` reports the modifiers on a pawn *right now*. Nothing in the client
//! totals "this player spent 26.2 seconds slowed this match" - that number only exists if
//! something watches successive snapshots and adds it up. This module is that something,
//! and it emits the pair `cc.received.<type>.events` and `cc.received.<type>.seconds`.
//!
//! # Why here and not in `deadlock_reader::events`
//!
//! [`EventTracker`](deadlock_reader::events::EventTracker) diffs counters the *game*
//! already maintains: kills, souls, objectives. It holds one tick of history and reports
//! the delta, so its state is bounded and its output is a restatement of what was read.
//!
//! This tracker owns totals nobody else has. It retains a per-player ledger for the life
//! of a match and the number it produces is derived, not read. That is a different layer -
//! derived, cross-tick tracking - and it is also the layer that needs `deadlock-reader`'s
//! `modifiers` feature, which roughly triples the cost of a tick. Putting it in the reader
//! would make every reader consumer pay for a metric most of them never ask for.
//!
//! # What identifies one application
//!
//! Neither field on its own is enough.
//!
//! - `address` is a heap address. The allocator recycles it, so a modifier that ends and
//!   an unrelated one that begins can share it. Keying on it alone loses the second.
//! - `serial` (`m_nSerialNumber`) distinguishes two applications of the same modifier, but
//!   it is a small counter and says nothing about *which* modifier: two different
//!   modifiers on the same pawn can carry the same value, and an unreadable field leaves
//!   it `None` on every one of them.
//!
//! So an application is `(address, serial, creation_time)`. A re-application at a recycled
//! address differs in `serial` or in `creation_time`; a fresh `serial` at a new address
//! differs in `address`. Two applications agreeing on all three would have to be the same
//! object created at the same instant, which is one application.
//!
//! # Seconds come from `m_flDuration`, not from the poll rate
//!
//! Every duration Companion reports is a multiple of `1/64`, because it is the server's
//! own tick-exact number rather than something sampled off a host clock. So a crowd
//! control that runs its course is credited its **whole `m_flDuration`**, once, regardless
//! of how often this tracker was fed. Accumulating `now - previous_now` per tick would
//! make the answer a function of the poll interval, and would be wrong by up to one
//! interval at each end.
//!
//! The exception is a crowd control removed early - dispelled, cleansed, or gone because
//! the pawn died. There is no tick-exact record of *when*, so it is credited from its
//! `m_flCreationTime` to the last tick it was actually seen. That under-counts by at most
//! one poll interval, and never claims time nothing observed.
//!
//! # What a permanent one counts as
//!
//! `m_flDuration == -1.0` means "until something removes it", so there is no nominal
//! length to credit. One of these counts one `events` the moment it is first seen, and
//! contributes to `seconds` only once it goes away - at which point it is credited the
//! span it was actually applied for, `m_flCreationTime` to the last tick it was seen. A
//! permanent crowd control that is still active therefore shows in `events` and not in
//! `seconds`, which is the honest answer: its duration is not yet a number.
//!
//! # A crowd control shorter than the poll gap
//!
//! The reader polls at about 10 Hz and plenty of stuns are shorter than 100 ms. Two cases,
//! and they are not the same:
//!
//! - **Seen at least once.** Crediting `m_flDuration` rather than observed deltas means
//!   one sighting is enough for an exact answer, even if that sighting lands after the
//!   modifier already expired (the client holds expired modifiers in the list briefly).
//!   This is the main reason the duration-based rule is worth the extra bookkeeping.
//! - **Never sampled.** Applied and removed entirely between two snapshots, it leaves no
//!   trace in either one. Nothing here can recover it and nothing here pretends to: it is
//!   simply not counted. `events` is therefore a lower bound, and the gap narrows with the
//!   poll interval.

use std::collections::{BTreeMap, HashMap, HashSet};

use deadlock_reader::snapshot::{CrowdControl, LiveSnapshot, Modifier, classify};

/// The `cc.received.` prefix every metric this module emits carries.
const METRIC_PREFIX: &str = "cc.received";

/// Something that happened to a player's crowd-control state.
///
/// Ordering within one tick is per player in slot order, and an application's
/// [`Applied`](CrowdControlEvent::Applied) always precedes its
/// [`Ended`](CrowdControlEvent::Ended) - including when both land on the same tick, which
/// is what a crowd control shorter than the poll gap looks like.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CrowdControlEvent {
    /// A crowd control was seen on a player for the first time.
    ///
    /// Fired on first *sighting*, which is not necessarily first application: one that was
    /// already running when polling started is reported here too, because there is no way
    /// to tell the two apart from a snapshot.
    Applied {
        /// Lobby slot of the player it landed on.
        slot: u32,
        /// Which kind of crowd control.
        kind: CrowdControl,
        /// Modifier class it came from, e.g. `"CCitadel_Modifier_Stunned"`.
        class: String,
        /// `m_flCreationTime`, in [`MatchClock::now`](deadlock_reader::snapshot::MatchClock)'s
        /// base. `None` when the field could not be read.
        at: Option<f32>,
        /// `m_flDuration`. Negative means it does not expire; see the module docs.
        duration: Option<f32>,
    },
    /// A crowd control finished, and its seconds have been credited.
    Ended {
        /// Lobby slot of the player it was on.
        slot: u32,
        /// Which kind of crowd control.
        kind: CrowdControl,
        /// Modifier class it came from.
        class: String,
        /// Seconds credited for this application, and added to the player's total.
        seconds: f32,
        /// Whether it ran to its own deadline rather than being removed early.
        ///
        /// `true` means `seconds` is the modifier's tick-exact `m_flDuration`. `false`
        /// means it was cleansed, dispelled or lost with the pawn, and `seconds` is
        /// bounded by the poll interval - see the module docs.
        expired: bool,
    },
}

/// One kind of crowd control, totalled for one player.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CrowdControlTotals {
    /// How many separate applications were seen. A lower bound; see the module docs.
    pub events: u32,
    /// Seconds credited so far, in the game's own time base.
    ///
    /// Only settled applications contribute. An active crowd control is already counted in
    /// [`events`](CrowdControlTotals::events) but does not reach here until it finishes,
    /// so this never claims time that has not elapsed.
    pub seconds: f32,
}

/// The metric name a kind is reported under, matching Companion's key set.
///
/// [`CrowdControl`] is `#[non_exhaustive]`, so a variant added upstream that this crate
/// has not been taught about reports as `unknown` rather than breaking the build. Several
/// such variants would share the one name; the per-kind totals from
/// [`PlayerCrowdControl::totals`] stay distinct regardless.
pub fn metric_name(kind: CrowdControl) -> &'static str {
    match kind {
        CrowdControl::Stun => "stun",
        CrowdControl::Silence => "silence",
        CrowdControl::Root => "root",
        CrowdControl::Slow => "slow",
        CrowdControl::Sleep => "sleep",
        CrowdControl::Knockup => "knockup",
        CrowdControl::Knockdown => "knockdown",
        CrowdControl::Disarm => "disarm",
        CrowdControl::Immobilize => "immobilize",
        _ => "unknown",
    }
}

/// What identifies one application of a modifier across ticks.
///
/// See the module docs for why all three fields are needed. `creation_time` is kept as raw
/// bits because it is only ever compared, never used in arithmetic here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ApplicationId {
    address: u64,
    serial: Option<u32>,
    created: Option<u32>,
}

impl ApplicationId {
    fn of(m: &Modifier) -> Self {
        ApplicationId {
            address: m.address,
            serial: m.serial,
            created: m.creation_time.map(f32::to_bits),
        }
    }
}

/// One application being watched.
#[derive(Clone, Debug)]
struct Active {
    kind: CrowdControl,
    /// The reading it was first seen with. Held whole rather than field by field so the
    /// `-1.0`-means-permanent rule lives in one place: [`Modifier::expires_at`].
    modifier: Modifier,
    first_seen: f32,
    last_seen: f32,
    /// Whether its seconds have already been credited. The client keeps an expired
    /// modifier in the list for a tick or two, and without this it would be credited on
    /// every one of them.
    settled: bool,
}

impl Active {
    /// Seconds to credit for an application that did not reach its own deadline.
    ///
    /// Measured from `m_flCreationTime` where it is readable, and from the first sighting
    /// where it is not, so an unreadable field costs accuracy rather than the whole
    /// record. Capped at the nominal duration: a modifier held in the list past its
    /// deadline must not read as having lasted longer than it could.
    fn observed_seconds(&self) -> f32 {
        let start = self.modifier.creation_time.unwrap_or(self.first_seen);
        let span = (self.last_seen - start).max(0.0);
        match self.modifier.duration {
            Some(d) if d >= 0.0 => span.min(d),
            _ => span,
        }
    }

    fn class(&self) -> String {
        self.modifier.class.clone().unwrap_or_default()
    }
}

/// One player's crowd-control ledger for the current match.
#[derive(Clone, Debug, Default)]
pub struct PlayerCrowdControl {
    totals: HashMap<CrowdControl, CrowdControlTotals>,
    active: HashMap<ApplicationId, Active>,
}

impl PlayerCrowdControl {
    /// Totals for one kind. Zeroed for a kind this player never took.
    pub fn totals(&self, kind: CrowdControl) -> CrowdControlTotals {
        self.totals.get(&kind).copied().unwrap_or_default()
    }

    /// Every kind this player has taken, ordered by [`metric_name`].
    ///
    /// Kinds with nothing recorded are absent rather than present and zero, which is how
    /// Companion's own output reads.
    pub fn totals_by_kind(&self) -> Vec<(CrowdControl, CrowdControlTotals)> {
        let mut out: Vec<(CrowdControl, CrowdControlTotals)> =
            self.totals.iter().map(|(k, v)| (*k, *v)).collect();
        out.sort_by_key(|(k, _)| metric_name(*k));
        out
    }

    /// How many applications are still running.
    ///
    /// These are counted in `events` and not yet in `seconds`; see the module docs.
    pub fn active_applications(&self) -> usize {
        self.active.values().filter(|a| !a.settled).count()
    }

    /// The ledger as Companion's flat metric keys, e.g. `cc.received.slow.seconds`.
    pub fn metrics(&self) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        for (kind, totals) in self.totals_by_kind() {
            let name = metric_name(kind);
            out.push((
                format!("{METRIC_PREFIX}.{name}.events"),
                f64::from(totals.events),
            ));
            out.push((
                format!("{METRIC_PREFIX}.{name}.seconds"),
                f64::from(totals.seconds),
            ));
        }
        out
    }

    /// Fold one tick's modifier list in, returning the applications it contained.
    fn observe(
        &mut self,
        slot: u32,
        list: &[Modifier],
        now: f32,
        out: &mut Vec<CrowdControlEvent>,
    ) -> HashSet<ApplicationId> {
        let mut present = HashSet::new();
        let mut fresh = Vec::new();

        for m in list {
            let Some(class) = m.class.as_deref() else {
                continue;
            };
            let Some(kind) = classify(class) else {
                continue;
            };
            let id = ApplicationId::of(m);
            present.insert(id);

            if let Some(active) = self.active.get_mut(&id) {
                active.last_seen = active.last_seen.max(now);
                continue;
            }
            self.active.insert(
                id,
                Active {
                    kind,
                    modifier: m.clone(),
                    first_seen: now,
                    last_seen: now,
                    settled: false,
                },
            );
            self.totals.entry(kind).or_default().events += 1;
            fresh.push(CrowdControlEvent::Applied {
                slot,
                kind,
                class: class.to_owned(),
                at: m.creation_time,
                duration: m.duration,
            });
        }

        // A modifier list has no meaningful order, so sort to keep the stream reproducible.
        fresh.sort_by(|a, b| event_order(a).cmp(&event_order(b)));
        out.append(&mut fresh);
        present
    }

    /// Settle whatever finished, and drop whatever is gone.
    ///
    /// `present` is `None` when this tick could not say what the player carries - an
    /// unreadable modifier property, or no row at all. That is not evidence of removal, so
    /// nothing is retired on it; the deadline rule still applies, because a modifier's own
    /// duration does not depend on anyone watching.
    fn retire(
        &mut self,
        slot: u32,
        present: Option<&HashSet<ApplicationId>>,
        now: f32,
        out: &mut Vec<CrowdControlEvent>,
    ) {
        let mut ended = Vec::new();

        self.active.retain(|id, active| {
            let gone = present.is_some_and(|p| !p.contains(id));
            if !active.settled {
                if active.modifier.expires_at().is_some_and(|at| at <= now) {
                    active.settled = true;
                    // Ran its course, so its own duration is the tick-exact answer.
                    ended.push((active.kind, active.class(), active.modifier.duration, true));
                } else if gone {
                    active.settled = true;
                    ended.push((
                        active.kind,
                        active.class(),
                        Some(active.observed_seconds()),
                        false,
                    ));
                }
            }
            !gone
        });

        let mut events: Vec<CrowdControlEvent> = ended
            .into_iter()
            .map(|(kind, class, seconds, expired)| {
                let seconds = seconds.unwrap_or(0.0).max(0.0);
                self.totals.entry(kind).or_default().seconds += seconds;
                CrowdControlEvent::Ended {
                    slot,
                    kind,
                    class,
                    seconds,
                    expired,
                }
            })
            .collect();
        events.sort_by(|a, b| event_order(a).cmp(&event_order(b)));
        out.append(&mut events);
    }
}

/// Sort key that makes a tick's output independent of hash order.
///
/// Two applications of one class on one tick are otherwise indistinguishable in the event,
/// so the third component is whatever number the variant carries: it breaks the tie
/// reproducibly without needing the address in the public shape.
fn event_order(e: &CrowdControlEvent) -> (&'static str, &str, u32) {
    match e {
        CrowdControlEvent::Applied {
            kind, class, at, ..
        } => (
            metric_name(*kind),
            class.as_str(),
            at.map_or(0, f32::to_bits),
        ),
        CrowdControlEvent::Ended {
            kind,
            class,
            seconds,
            ..
        } => (metric_name(*kind), class.as_str(), seconds.to_bits()),
    }
}

/// Accumulates crowd control taken, per player, across snapshots.
///
/// Feed it every snapshot. It keeps a ledger per lobby slot for the life of a match and
/// clears the lot when the match id changes, because slots, controllers and every counter
/// are recycled between matches.
///
/// ```no_run
/// use deadlock_events::crowd_control::CrowdControlTracker;
/// use deadlock_reader::Reader;
///
/// let reader = Reader::attach()?;
/// let mut cc = CrowdControlTracker::new();
/// if let Some(snap) = reader.live_snapshot()? {
///     for event in cc.update(&snap) {
///         println!("{event:?}");
///     }
///     if let Some(player) = cc.player(1) {
///         for (key, value) in player.metrics() {
///             println!("{key} = {value}");
///         }
///     }
/// }
/// # Ok::<(), deadlock_reader::Error>(())
/// ```
#[derive(Debug, Default)]
pub struct CrowdControlTracker {
    match_id: Option<u64>,
    players: HashMap<u32, PlayerCrowdControl>,
    ticks_observed: u32,
    ticks_without_modifiers: u32,
}

impl CrowdControlTracker {
    /// A tracker with no history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every ledger.
    ///
    /// Call this after a gap in polling. Active applications are dropped rather than
    /// settled: their end was not observed, so crediting seconds for them would be a
    /// guess.
    pub fn reset(&mut self) {
        self.match_id = None;
        self.players.clear();
        self.ticks_observed = 0;
        self.ticks_without_modifiers = 0;
    }

    /// The match these ledgers belong to.
    pub fn match_id(&self) -> Option<u64> {
        self.match_id
    }

    /// One player's ledger, by lobby slot.
    pub fn player(&self, slot: u32) -> Option<&PlayerCrowdControl> {
        self.players.get(&slot)
    }

    /// Every ledger, in slot order.
    pub fn players(&self) -> Vec<(u32, &PlayerCrowdControl)> {
        let mut out: Vec<(u32, &PlayerCrowdControl)> =
            self.players.iter().map(|(s, p)| (*s, p)).collect();
        out.sort_by_key(|(slot, _)| *slot);
        out
    }
    /// Ticks this tracker actually processed.
    ///
    /// A snapshot whose clock could not be read is not counted: nothing about it can be
    /// placed in time, so it is not an observation of anything.
    pub fn ticks_observed(&self) -> u32 {
        self.ticks_observed
    }

    /// Of those, how many had **no** readable modifier list on any player.
    ///
    /// This is what separates a quiet match from a blind one. Everything here comes from
    /// `PlayerRow::modifiers`, which is `None` when the list could not be read - and a
    /// player who took no crowd control reports nothing either. Equal to
    /// [`Self::ticks_observed`] means the reader never saw a modifier at all, which is what
    /// a build without `deadlock-reader/modifiers` looks like, and is a very different fact
    /// from nobody having been stunned.
    ///
    /// One player reading is enough for a tick to count as read; a partial read is a
    /// different and rarer problem, and folding it in here would hide the common case.
    pub fn ticks_without_modifiers(&self) -> u32 {
        self.ticks_without_modifiers
    }

    /// Fold one snapshot in and return what changed.
    ///
    /// A snapshot with no readable [`MatchClock::now`](deadlock_reader::snapshot::MatchClock)
    /// is ignored outright, state included: every number here is a position on that clock,
    /// and a tick that cannot be placed in time can neither open nor close an application
    /// without corrupting the ones already open.
    ///
    /// Rows with no lobby slot - spectators - are skipped. They have no scoreboard entry
    /// to attribute anything to.
    pub fn update(&mut self, snap: &LiveSnapshot) -> Vec<CrowdControlEvent> {
        if snap.match_id != self.match_id {
            // The counters describe the current match, so they start again with it.
            self.players.clear();
            self.ticks_observed = 0;
            self.ticks_without_modifiers = 0;
            self.match_id = snap.match_id;
        }
        let Some(now) = snap.clock.now else {
            return Vec::new();
        };
        self.ticks_observed += 1;

        // Slot order, so the event stream does not depend on the order rows were walked.
        let mut rows: BTreeMap<u32, Option<&[Modifier]>> = BTreeMap::new();
        for row in &snap.players {
            let Some(slot) = row.slot else { continue };
            rows.insert(slot, row.modifiers.as_deref());
        }
        if !rows.is_empty() && rows.values().all(Option::is_none) {
            self.ticks_without_modifiers += 1;
        }

        let mut out = Vec::new();
        for (&slot, list) in &rows {
            let player = self.players.entry(slot).or_default();
            let present = list.map(|l| player.observe(slot, l, now, &mut out));
            player.retire(slot, present.as_ref(), now, &mut out);
        }

        // A player who vanished from the snapshot is treated exactly like one whose
        // modifier list could not be read: no evidence of removal, but deadlines still
        // pass.
        let mut absent: Vec<u32> = self
            .players
            .keys()
            .copied()
            .filter(|s| !rows.contains_key(s))
            .collect();
        absent.sort_unstable();
        for slot in absent {
            if let Some(player) = self.players.get_mut(&slot) {
                player.retire(slot, None, now, &mut out);
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deadlock_reader::snapshot::{MatchClock, PlayerRow};

    /// A modifier as the reader would report it.
    fn modifier(class: &str, address: u64, serial: u32, created: f32, duration: f32) -> Modifier {
        Modifier {
            address,
            class: Some(class.to_owned()),
            serial: Some(serial),
            creation_time: Some(created),
            duration: Some(duration),
            ..Default::default()
        }
    }

    /// One tick: a single player in slot 1 carrying `modifiers`, at engine time `now`.
    fn snapshot(now: f32, modifiers: Option<Vec<Modifier>>) -> LiveSnapshot {
        snapshot_with(now, vec![(1, modifiers)])
    }

    fn snapshot_with(now: f32, players: Vec<(u32, Option<Vec<Modifier>>)>) -> LiveSnapshot {
        LiveSnapshot {
            match_id: Some(42),
            clock: MatchClock {
                now: Some(now),
                ..Default::default()
            },
            players: players
                .into_iter()
                .map(|(slot, modifiers)| PlayerRow {
                    slot: Some(slot),
                    modifiers,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn totals(t: &CrowdControlTracker, slot: u32, kind: CrowdControl) -> CrowdControlTotals {
        t.player(slot).map(|p| p.totals(kind)).unwrap_or_default()
    }

    /// A quiet match and an unread modifier list are told apart.
    ///
    /// Everything this tracker reports comes from `PlayerRow::modifiers`, which is `None`
    /// when the list could not be read — and a player who took no crowd control also
    /// reports nothing. Without a count of how often the list actually read, a match
    /// summary of "no crowd control" means either "nobody was stunned" or "the reader never
    /// saw a modifier", and those are not the same fact.
    ///
    /// `objective_context` already reports this for its own inputs through
    /// `unattributable_intervals`; this is the same idea for the modifier-driven trackers.
    /// It matters most for a recorded match, where nobody is watching the client at the
    /// time and the numbers are read back later.
    #[test]
    fn an_unread_modifier_list_is_counted_separately_from_a_quiet_one() {
        let mut t = CrowdControlTracker::new();
        assert_eq!(t.ticks_observed(), 0);
        assert_eq!(t.ticks_without_modifiers(), 0);

        t.update(&snapshot(100.0, Some(Vec::new())));
        t.update(&snapshot(100.2, Some(Vec::new())));
        assert_eq!(t.ticks_observed(), 2);
        assert_eq!(
            t.ticks_without_modifiers(),
            0,
            "an empty list is a successful read"
        );

        t.update(&snapshot(100.4, None));
        assert_eq!(t.ticks_observed(), 3);
        assert_eq!(t.ticks_without_modifiers(), 1);

        let mut blind = snapshot(100.6, Some(Vec::new()));
        blind.clock.now = None;
        t.update(&blind);
        assert_eq!(
            t.ticks_observed(),
            3,
            "a clockless tick is not an observation"
        );
    }

    /// One player reading is enough for the tick to count as observed.
    ///
    /// The counter answers "did the reader see any modifiers this tick", not "did every
    /// player's list read". A partial read is a different and much rarer problem, and
    /// conflating the two would make the common case — the feature being off, so *no* list
    /// ever reads — harder to see.
    #[test]
    fn a_tick_counts_as_read_when_any_player_list_reads() {
        let mut t = CrowdControlTracker::new();
        t.update(&snapshot_with(
            100.0,
            vec![(1, None), (7, Some(Vec::new()))],
        ));
        assert_eq!(t.ticks_observed(), 1);
        assert_eq!(t.ticks_without_modifiers(), 0);

        t.update(&snapshot_with(100.2, vec![(1, None), (7, None)]));
        assert_eq!(t.ticks_without_modifiers(), 1);
    }

    /// The whole point of the module: a stun seen once is credited its own duration.
    ///
    /// Guards against crediting the time between polls instead. A tracker that summed
    /// `now - previous_now` would report 0.9 here (three 0.3 s gaps) rather than the
    /// tick-exact 1.0, and would report something different again at a different poll
    /// rate.
    #[test]
    fn a_stun_that_runs_its_course_is_credited_its_tick_exact_duration() {
        let mut t = CrowdControlTracker::new();
        let stun = modifier("CCitadel_Modifier_Stunned", 0x1000, 7, 100.0, 1.0);

        let applied = t.update(&snapshot(100.1, Some(vec![stun.clone()])));
        assert_eq!(
            applied,
            vec![CrowdControlEvent::Applied {
                slot: 1,
                kind: CrowdControl::Stun,
                class: "CCitadel_Modifier_Stunned".into(),
                at: Some(100.0),
                duration: Some(1.0),
            }]
        );
        assert_eq!(totals(&t, 1, CrowdControl::Stun).events, 1);
        assert_eq!(
            totals(&t, 1, CrowdControl::Stun).seconds,
            0.0,
            "an application still running must not be credited seconds it has not spent"
        );

        assert!(
            t.update(&snapshot(100.4, Some(vec![stun.clone()])))
                .is_empty()
        );
        assert!(
            t.update(&snapshot(100.7, Some(vec![stun.clone()])))
                .is_empty()
        );

        let ended = t.update(&snapshot(101.0, Some(vec![])));
        assert_eq!(
            ended,
            vec![CrowdControlEvent::Ended {
                slot: 1,
                kind: CrowdControl::Stun,
                class: "CCitadel_Modifier_Stunned".into(),
                seconds: 1.0,
                expired: true,
            }]
        );
        assert_eq!(
            totals(&t, 1, CrowdControl::Stun),
            CrowdControlTotals {
                events: 1,
                seconds: 1.0
            }
        );
    }

    /// A second stun is a second event, not a refresh of the first.
    ///
    /// This is the case the identity rule exists for. Keying on the modifier class - or on
    /// anything else a re-application shares with the application it replaces - reports one
    /// event of two seconds where the player was actually stunned twice.
    #[test]
    fn the_same_kind_applied_twice_counts_two_events() {
        let mut t = CrowdControlTracker::new();
        let first = modifier("CCitadel_Modifier_Stunned", 0x1000, 7, 100.0, 1.0);
        let second = modifier("CCitadel_Modifier_Stunned", 0x2000, 8, 105.0, 1.0);

        t.update(&snapshot(100.1, Some(vec![first])));
        t.update(&snapshot(101.5, Some(vec![])));
        t.update(&snapshot(105.1, Some(vec![second])));
        t.update(&snapshot(106.5, Some(vec![])));

        assert_eq!(
            totals(&t, 1, CrowdControl::Stun),
            CrowdControlTotals {
                events: 2,
                seconds: 2.0
            }
        );
    }

    /// One application ends and an identical one begins without an empty tick in between.
    ///
    /// The hard case for identity: the allocator handed the second application the address
    /// the first just freed, so between two polls the same address carries a different
    /// application. Keying on `address` alone reads that as the first one still being
    /// there - one event where there were two, and the second silence credited nothing.
    #[test]
    fn a_reapplication_at_a_recycled_address_is_still_a_second_event() {
        let mut t = CrowdControlTracker::new();
        let first = modifier("CCitadel_Modifier_Silenced", 0x1000, 3, 100.0, 1.0);
        let second = modifier("CCitadel_Modifier_Silenced", 0x1000, 4, 101.0, 1.0);

        t.update(&snapshot(100.5, Some(vec![first])));
        let events = t.update(&snapshot(101.5, Some(vec![second])));

        assert_eq!(
            events,
            vec![
                CrowdControlEvent::Applied {
                    slot: 1,
                    kind: CrowdControl::Silence,
                    class: "CCitadel_Modifier_Silenced".into(),
                    at: Some(101.0),
                    duration: Some(1.0),
                },
                CrowdControlEvent::Ended {
                    slot: 1,
                    kind: CrowdControl::Silence,
                    class: "CCitadel_Modifier_Silenced".into(),
                    seconds: 1.0,
                    expired: true,
                },
            ],
            "a recycled address must not swallow the second application"
        );
        assert_eq!(totals(&t, 1, CrowdControl::Silence).events, 2);

        t.update(&snapshot(102.5, Some(vec![])));
        assert_eq!(
            totals(&t, 1, CrowdControl::Silence),
            CrowdControlTotals {
                events: 2,
                seconds: 2.0
            }
        );
    }

    /// Two kinds at once are two independent ledgers.
    ///
    /// Guards against a tracker that keeps one "currently crowd controlled" flag, which
    /// would attribute overlapping crowd control to whichever kind happened to be seen
    /// first and lose the other entirely.
    #[test]
    fn two_kinds_at_once_are_totalled_separately() {
        let mut t = CrowdControlTracker::new();
        let root = modifier("CCitadel_Modifier_Root", 0x1000, 1, 100.0, 1.5);
        let slow = modifier("CCitadel_Modifier_Slow", 0x2000, 2, 100.0, 4.0);

        let applied = t.update(&snapshot(100.1, Some(vec![root.clone(), slow.clone()])));
        assert_eq!(applied.len(), 2);

        let ended = t.update(&snapshot(101.6, Some(vec![slow.clone()])));
        assert_eq!(
            ended,
            vec![CrowdControlEvent::Ended {
                slot: 1,
                kind: CrowdControl::Root,
                class: "CCitadel_Modifier_Root".into(),
                seconds: 1.5,
                expired: true,
            }]
        );
        assert_eq!(
            totals(&t, 1, CrowdControl::Slow),
            CrowdControlTotals {
                events: 1,
                seconds: 0.0
            }
        );

        t.update(&snapshot(104.1, Some(vec![])));
        assert_eq!(
            totals(&t, 1, CrowdControl::Slow),
            CrowdControlTotals {
                events: 1,
                seconds: 4.0
            }
        );
        assert_eq!(
            totals(&t, 1, CrowdControl::Root),
            CrowdControlTotals {
                events: 1,
                seconds: 1.5
            }
        );
    }

    /// `m_flDuration == -1.0` is "until something removes it", not an expiry in the past.
    ///
    /// Reading it as seconds puts the deadline before the creation time, so a permanent
    /// crowd control would settle on the tick it was first seen and be credited a negative
    /// or zero duration forever after. It is instead credited the span it was actually
    /// applied for, and only once it goes away.
    #[test]
    fn a_permanent_crowd_control_is_credited_the_span_it_was_applied_for() {
        let mut t = CrowdControlTracker::new();
        let held = modifier(
            "CCitadel_Modifier_Trapper_Immobilize",
            0x1000,
            1,
            100.0,
            -1.0,
        );

        t.update(&snapshot(100.5, Some(vec![held.clone()])));
        assert_eq!(
            totals(&t, 1, CrowdControl::Immobilize),
            CrowdControlTotals {
                events: 1,
                seconds: 0.0
            },
            "a permanent one has no nominal length to credit while it is still running"
        );

        t.update(&snapshot(103.0, Some(vec![held.clone()])));
        assert_eq!(
            t.player(1).map(PlayerCrowdControl::active_applications),
            Some(1)
        );

        let ended = t.update(&snapshot(103.3, Some(vec![])));
        assert_eq!(
            ended,
            vec![CrowdControlEvent::Ended {
                slot: 1,
                kind: CrowdControl::Immobilize,
                class: "CCitadel_Modifier_Trapper_Immobilize".into(),
                seconds: 3.0,
                expired: false,
            }]
        );
    }

    /// Present in one tick and gone in the next, before its deadline: removed early.
    ///
    /// Credited to the last tick it was seen rather than to its nominal duration, because
    /// a cleanse means the duration never ran. Reporting `expired: false` is what tells a
    /// consumer the number is poll-bounded rather than tick-exact.
    #[test]
    fn a_modifier_removed_before_its_deadline_is_credited_only_what_was_observed() {
        let mut t = CrowdControlTracker::new();
        let sleep = modifier("CCitadel_Modifier_Sleep", 0x1000, 1, 100.0, 10.0);

        t.update(&snapshot(100.25, Some(vec![sleep.clone()])));
        t.update(&snapshot(101.25, Some(vec![sleep.clone()])));
        let ended = t.update(&snapshot(101.75, Some(vec![])));

        assert_eq!(
            ended,
            vec![CrowdControlEvent::Ended {
                slot: 1,
                kind: CrowdControl::Sleep,
                class: "CCitadel_Modifier_Sleep".into(),
                seconds: 1.25,
                expired: false,
            }]
        );
        assert_eq!(
            totals(&t, 1, CrowdControl::Sleep),
            CrowdControlTotals {
                events: 1,
                seconds: 1.25
            }
        );
    }

    /// The client keeps an expired modifier in the list for a tick or two.
    ///
    /// Settling on the deadline and then seeing it again must not credit it twice, and
    /// must not re-open it as a new application when it is finally removed.
    #[test]
    fn a_modifier_held_past_its_deadline_is_credited_exactly_once() {
        let mut t = CrowdControlTracker::new();
        let stun = modifier("CCitadel_Modifier_Stunned", 0x1000, 1, 100.0, 1.0);

        t.update(&snapshot(100.1, Some(vec![stun.clone()])));
        let ended = t.update(&snapshot(101.1, Some(vec![stun.clone()])));
        assert_eq!(ended.len(), 1, "settles on the deadline, list or no list");

        assert!(
            t.update(&snapshot(101.4, Some(vec![stun.clone()])))
                .is_empty()
        );
        assert!(t.update(&snapshot(101.7, Some(vec![]))).is_empty());

        assert_eq!(
            totals(&t, 1, CrowdControl::Stun),
            CrowdControlTotals {
                events: 1,
                seconds: 1.0
            }
        );
    }

    /// One sighting is enough, even when it lands after the modifier already expired.
    ///
    /// This is what makes the duration rule worth the bookkeeping: at a 10 Hz poll a stun
    /// shorter than the gap is often only ever seen once, and a delta-accumulating tracker
    /// would credit it nothing at all.
    #[test]
    fn a_stun_seen_only_once_still_reports_its_full_duration() {
        let mut t = CrowdControlTracker::new();
        let stun = modifier("CCitadel_Modifier_Stunned", 0x1000, 1, 100.0, 0.25);

        let events = t.update(&snapshot(100.4, Some(vec![stun])));
        assert_eq!(events.len(), 2, "applied and ended on the same tick");
        assert!(matches!(events[0], CrowdControlEvent::Applied { .. }));
        assert_eq!(
            totals(&t, 1, CrowdControl::Stun),
            CrowdControlTotals {
                events: 1,
                seconds: 0.25
            }
        );
    }

    /// An unreadable modifier list is not evidence that anything was removed.
    ///
    /// `None` means the pawn's modifier property could not be read - a dead pawn, a failed
    /// chase - which is deliberately distinct from an empty list. Retiring on it would
    /// credit a live crowd control as removed early and then count the same one again when
    /// the list came back.
    #[test]
    fn an_unreadable_modifier_list_neither_ends_nor_restarts_an_application() {
        let mut t = CrowdControlTracker::new();
        let slow = modifier("CCitadel_Modifier_Slow", 0x1000, 1, 100.0, 6.0);

        t.update(&snapshot(100.1, Some(vec![slow.clone()])));
        assert!(t.update(&snapshot(101.0, None)).is_empty());
        assert!(
            t.update(&snapshot(102.0, Some(vec![slow.clone()])))
                .is_empty()
        );
        assert_eq!(
            totals(&t, 1, CrowdControl::Slow).events,
            1,
            "the gap must not look like a second application"
        );

        t.update(&snapshot(106.1, Some(vec![])));
        assert_eq!(
            totals(&t, 1, CrowdControl::Slow),
            CrowdControlTotals {
                events: 1,
                seconds: 6.0
            }
        );
    }

    /// A deadline passes whether or not anyone is watching the player.
    ///
    /// A row that disappears - the player left, or the walk missed them - is treated like
    /// an unreadable list, but a timed modifier still settles on its own duration.
    #[test]
    fn a_player_who_leaves_the_snapshot_still_settles_a_timed_modifier() {
        let mut t = CrowdControlTracker::new();
        let stun = modifier("CCitadel_Modifier_Stunned", 0x1000, 1, 100.0, 2.0);

        t.update(&snapshot_with(100.1, vec![(1, Some(vec![stun]))]));
        let ended = t.update(&snapshot_with(102.5, vec![(2, Some(vec![]))]));
        assert_eq!(
            ended,
            vec![CrowdControlEvent::Ended {
                slot: 1,
                kind: CrowdControl::Stun,
                class: "CCitadel_Modifier_Stunned".into(),
                seconds: 2.0,
                expired: true,
            }]
        );
    }

    /// Ledgers are per player, and the slot is what joins them back to the scoreboard.
    #[test]
    fn each_slot_keeps_its_own_ledger() {
        let mut t = CrowdControlTracker::new();
        let stun = modifier("CCitadel_Modifier_Stunned", 0x1000, 1, 100.0, 1.0);
        let slow = modifier("CCitadel_Modifier_Slow", 0x2000, 1, 100.0, 3.0);

        t.update(&snapshot_with(
            100.1,
            vec![(1, Some(vec![stun])), (5, Some(vec![slow]))],
        ));
        t.update(&snapshot_with(
            104.0,
            vec![(1, Some(vec![])), (5, Some(vec![]))],
        ));

        assert_eq!(
            totals(&t, 1, CrowdControl::Stun),
            CrowdControlTotals {
                events: 1,
                seconds: 1.0
            }
        );
        assert_eq!(
            totals(&t, 1, CrowdControl::Slow),
            CrowdControlTotals::default()
        );
        assert_eq!(
            totals(&t, 5, CrowdControl::Slow),
            CrowdControlTotals {
                events: 1,
                seconds: 3.0
            }
        );
        assert_eq!(t.players().len(), 2);
    }

    /// Modifiers this table does not recognise are not crowd control.
    ///
    /// The classification is deliberately exact-name, so an immunity or a caster buff whose
    /// name carries a crowd-control word must contribute nothing at all.
    #[test]
    fn an_unclassified_modifier_contributes_nothing() {
        let mut t = CrowdControlTracker::new();
        let immunity = modifier("CCitadel_Modifier_SlowImmunity", 0x1000, 1, 100.0, 5.0);
        let passive = modifier("CCitadel_Modifier_InHideoutMap", 0x2000, 1, 100.0, -1.0);

        assert!(
            t.update(&snapshot(100.1, Some(vec![immunity, passive])))
                .is_empty()
        );
        assert_eq!(
            t.player(1).map(PlayerCrowdControl::active_applications),
            Some(0)
        );
        assert!(t.player(1).is_some_and(|p| p.metrics().is_empty()));
    }

    /// Every counter resets between matches, so carrying a ledger across would add one
    /// match's crowd control to the next.
    #[test]
    fn a_new_match_clears_every_ledger() {
        let mut t = CrowdControlTracker::new();
        let stun = modifier("CCitadel_Modifier_Stunned", 0x1000, 1, 100.0, 1.0);
        t.update(&snapshot(100.1, Some(vec![stun])));
        t.update(&snapshot(101.5, Some(vec![])));
        assert_eq!(totals(&t, 1, CrowdControl::Stun).events, 1);

        let mut next = snapshot(10.0, Some(vec![]));
        next.match_id = Some(43);
        t.update(&next);

        assert_eq!(t.match_id(), Some(43));
        assert_eq!(
            totals(&t, 1, CrowdControl::Stun),
            CrowdControlTotals::default()
        );
    }

    /// Without a clock nothing can be placed in time, so the tick is ignored outright.
    ///
    /// Folding it in with a stand-in time would settle live applications against a number
    /// that means nothing, which is worse than skipping the tick.
    #[test]
    fn a_snapshot_with_no_clock_changes_nothing() {
        let mut t = CrowdControlTracker::new();
        let stun = modifier("CCitadel_Modifier_Stunned", 0x1000, 1, 100.0, 1.0);
        t.update(&snapshot(100.1, Some(vec![stun.clone()])));

        let mut blind = snapshot(0.0, Some(vec![]));
        blind.clock.now = None;
        assert!(t.update(&blind).is_empty());
        assert_eq!(
            totals(&t, 1, CrowdControl::Stun),
            CrowdControlTotals {
                events: 1,
                seconds: 0.0
            },
            "the application must still be open, not settled against a made-up time"
        );

        t.update(&snapshot(101.5, Some(vec![])));
        assert_eq!(totals(&t, 1, CrowdControl::Stun).seconds, 1.0);
    }

    /// A spectator's slot field is not a scoreboard position, so there is nothing to
    /// attribute crowd control to.
    #[test]
    fn a_row_with_no_slot_is_skipped() {
        let mut t = CrowdControlTracker::new();
        let stun = modifier("CCitadel_Modifier_Stunned", 0x1000, 1, 100.0, 1.0);
        let snap = LiveSnapshot {
            match_id: Some(42),
            clock: MatchClock {
                now: Some(100.1),
                ..Default::default()
            },
            players: vec![PlayerRow {
                slot: None,
                modifiers: Some(vec![stun]),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(t.update(&snap).is_empty());
        assert!(t.players().is_empty());
    }

    /// The emitted keys are Companion's, verbatim.
    ///
    /// `cc.received.slow.events` / `cc.received.slow.seconds` are what the enriched JSON
    /// carries; a different spelling makes the output useless for comparison against it.
    #[test]
    fn metrics_use_companions_key_names() {
        let mut t = CrowdControlTracker::new();
        let slow = modifier("CCitadel_Modifier_DiminishingSlow", 0x1000, 1, 100.0, 2.5);
        t.update(&snapshot(100.1, Some(vec![slow])));
        t.update(&snapshot(103.0, Some(vec![])));

        let metrics = t.player(1).expect("slot 1 has a ledger").metrics();
        assert_eq!(
            metrics,
            vec![
                ("cc.received.slow.events".to_string(), 1.0),
                ("cc.received.slow.seconds".to_string(), 2.5),
            ]
        );
    }

    /// One name per kind, and no two kinds sharing one.
    ///
    /// A collision would silently merge two ledgers at the point they are emitted, which
    /// is the one place the merge is invisible in the totals.
    #[test]
    fn every_kind_reports_under_a_distinct_name() {
        let kinds = [
            CrowdControl::Stun,
            CrowdControl::Silence,
            CrowdControl::Root,
            CrowdControl::Slow,
            CrowdControl::Sleep,
            CrowdControl::Knockup,
            CrowdControl::Knockdown,
            CrowdControl::Disarm,
            CrowdControl::Immobilize,
        ];
        let mut names: Vec<&str> = kinds.iter().map(|k| metric_name(*k)).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two kinds share a metric name");
        assert!(!names.contains(&"unknown"), "a known kind fell through");
    }

    /// Resetting drops open applications rather than settling them.
    #[test]
    fn resetting_forgets_everything() {
        let mut t = CrowdControlTracker::new();
        let stun = modifier("CCitadel_Modifier_Stunned", 0x1000, 1, 100.0, 5.0);
        t.update(&snapshot(100.1, Some(vec![stun])));
        t.reset();
        assert_eq!(t.match_id(), None);
        assert!(t.players().is_empty());
    }
}
