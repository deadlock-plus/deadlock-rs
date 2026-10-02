//! Turn a stream of snapshots into a stream of changes.
//!
//! [`Reader::live_snapshot`](crate::Reader::live_snapshot) gives you the world as it is
//! right now. Most consumers - overlays, rich presence, notifiers, stat loggers - care
//! about what just *happened*, and end up hand-rolling the same diff. This module is that
//! diff, written once.
//!
//! ```no_run
//! use deadlock_reader::{Reader, events::EventTracker};
//!
//! let reader = Reader::attach()?;
//! let mut tracker = EventTracker::new();
//! loop {
//!     if let Some(snap) = reader.live_snapshot()? {
//!         for event in tracker.update(&snap) {
//!             println!("{event:?}");
//!         }
//!     }
//!     std::thread::sleep(std::time::Duration::from_millis(500));
//! }
//! # Ok::<(), deadlock_reader::Error>(())
//! ```
//!
//! # What this can and cannot tell you
//!
//! Everything here is inferred by comparing two snapshots. There is no event feed in the
//! client to read, so the resolution is bounded by how often you poll: two kills between
//! two calls to [`EventTracker::update`] arrive as one event carrying the new total, not
//! as two events.
//!
//! It also means **kills are not attributed**. The client exposes each player's own
//! counters, not who killed whom. A kill and a death in the same tick are reported as two
//! independent events; pairing them up would be a guess, and a wrong one whenever two
//! fights resolve together. If you want an approximation, do it in your own code where
//! the uncertainty is visible.
//!
//! Counters only ever fire on an increase, so a fresh match - which resets everything to
//! zero - produces no phantom events. A changed match id resets the tracker outright.
//!
//! # Capturing the end of a match
//!
//! [`Event::MatchEnded`] is the one event that carries data rather than describing it. The
//! client keeps the full scoreboard readable while the post-match screen is up and then
//! tears it down, so the final state exists for a matter of seconds. Measured across five
//! real endings, the window from `PostGame` to the match data disappearing was 12 to 19
//! seconds.
//!
//! That makes the poll interval part of the contract. Poll faster than about ten seconds
//! and the ending is captured with its scoreboard attached; poll slower and the tracker
//! sees only [`Event::MatchLeft`], with nothing to attach. There is no way to recover it
//! afterwards, because by then the entities are gone.

use std::collections::HashMap;

use deadlock_core::{GameState, HeroId, ItemId, Team};

use crate::snapshot::{LiveSnapshot, ObjectiveKind, PlayerRow};

/// Something that changed between two snapshots.
///
/// `slot` is the lobby slot (`1..=12`) where one is known. Events for a player always
/// carry it, so a consumer can join back to [`LiveSnapshot::players`] without holding on
/// to addresses.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Event {
    /// A match id appeared, or replaced a different one.
    MatchStarted {
        /// The new match id, if the game reports one.
        match_id: Option<u64>,
    },
    /// The match reached a terminal phase, with the final state attached.
    ///
    /// Fired on the transition into [`GameState::is_over`], not when the match id goes
    /// away. That distinction is the whole point: the client tears the match down a few
    /// seconds after the scoreboard appears, and by the time the id clears there is
    /// nothing left to read. Measured across five real endings, the window from
    /// `PostGame` to the match data disappearing was 12 to 19 seconds.
    ///
    /// **A consumer polling slower than about ten seconds will miss this.** The final
    /// state is captured on the tick that observes the transition, so the poll interval
    /// bounds whether it can be captured at all.
    ///
    /// See [`Event::MatchLeft`] for the later, weaker signal.
    MatchEnded {
        /// The id of the match that just ended.
        match_id: Option<u64>,
        /// Everything the reader could see at the moment the match ended: final
        /// scoreboard, team totals, objectives and clock.
        ///
        /// Boxed because a snapshot dwarfs every other variant in this enum.
        final_state: Box<LiveSnapshot>,
    },
    /// The match id went away, i.e. the client returned to the Hideout or a menu.
    ///
    /// This is bookkeeping, not an ending. The match data is already gone by now; use
    /// [`Event::MatchEnded`] to capture it.
    MatchLeft {
        /// The id of the match that was left.
        match_id: Option<u64>,
    },
    /// The game phase changed.
    GameStateChanged {
        /// Phase we were in.
        from: GameState,
        /// Phase we are in now.
        to: GameState,
    },
    /// The match was paused or unpaused.
    PauseChanged {
        /// Whether the game is paused now.
        paused: bool,
    },
    /// A player's kill count went up.
    ///
    /// The victim is not identified; see the module docs.
    Kill {
        /// Lobby slot of the killer.
        slot: Option<u32>,
        /// Their hero.
        hero: Option<HeroId>,
        /// Their new kill total.
        total: u32,
        /// How many kills this covers - more than one if polling was slower than the game.
        delta: u32,
    },
    /// A player died.
    Death {
        /// Lobby slot of whoever died.
        slot: Option<u32>,
        /// Their hero.
        hero: Option<HeroId>,
        /// Their new death total.
        total: u32,
        /// How many deaths this covers.
        delta: u32,
    },
    /// A player assisted a kill.
    ///
    /// Diffed from `m_iPlayerAssists`, and carries the same caveat every counter here
    /// does: the client exposes each player's own total and nothing linking an assist to
    /// the kill it belongs to, so this says who assisted and when, never whom.
    Assist {
        /// Lobby slot of the assister.
        slot: Option<u32>,
        /// Their hero.
        hero: Option<HeroId>,
        /// Their new assist total.
        total: u32,
        /// How many assists this covers - more than one if polling was slower than the game.
        delta: u32,
    },
    /// A player gained one or more levels.
    LevelUp {
        /// Lobby slot.
        slot: Option<u32>,
        /// Their hero.
        hero: Option<HeroId>,
        /// The level they are now.
        level: u32,
    },
    /// A player's item list gained an entry.
    ItemPurchased {
        /// Lobby slot.
        slot: Option<u32>,
        /// The item that appeared.
        item: ItemId,
    },
    /// A player's item list lost an entry - sold, or an expiring item running out.
    ItemLost {
        /// Lobby slot.
        slot: Option<u32>,
        /// The item that disappeared.
        item: ItemId,
    },
    /// A player put points into an ability.
    AbilityUpgraded {
        /// Lobby slot.
        slot: Option<u32>,
        /// Which ability, by the id the game uses for it.
        ability: ItemId,
        /// Points in it now.
        points: u32,
    },
    /// A structure was destroyed - it left the entity list or dropped to zero health.
    ObjectiveDestroyed {
        /// What it was, e.g. `walker`, `guardian`, `patron`.
        kind: ObjectiveKind,
        /// Which side lost it.
        team: Option<Team>,
    },
    /// The Midboss was killed.
    MidbossKilled {
        /// How many times it has now been killed this match.
        total: u32,
    },
    /// A player left the match.
    PlayerLeft {
        /// Lobby slot.
        slot: Option<u32>,
        /// Their Steam persona name, if it was readable.
        name: Option<String>,
    },
    /// A player who had left came back.
    PlayerRejoined {
        /// Lobby slot.
        slot: Option<u32>,
        /// Their Steam persona name, if it was readable.
        name: Option<String>,
    },
    /// The match clock jumped backwards: the stream was seeked, not played.
    ///
    /// A replay or a spectate can be scrubbed, and `m_unMatchID` does not change when it
    /// is - so the tracker's usual re-baseline on a new match never fires, and the counters
    /// it diffs walk back over ground already reported and then forward over it again.
    ///
    /// The tick that jumps is not diffed: every counter moves at once across a seek, and a
    /// comparison of the two sides of it describes nothing that happened. Diffing resumes
    /// from where the seek landed.
    ///
    /// Reported rather than swallowed, for the same reason as [`Event::StatAnomaly`]: a
    /// consumer folding this stream into a match history must be able to tell a replayed
    /// kill from a second one, and only this event tells it where to look.
    ClockRewound {
        /// The clock the previous tick read, in seconds.
        from: f32,
        /// The clock this tick reads, in seconds.
        to: f32,
    },
    /// A player swapped hero mid-match.
    ///
    /// Only a swap between two known heroes. Learning a player's hero for the first time
    /// (`None -> Some`) is not reported: that is a read succeeding or a draft resolving,
    /// and treating it as a change would announce all twelve players at match start. A
    /// hero id that stops reading (`Some -> None`) is not reported either - a failed read
    /// says nothing about the hero.
    HeroChanged {
        /// Lobby slot, when the row had one.
        slot: Option<u32>,
        /// The hero they were on.
        from: HeroId,
        /// The hero they are on now.
        to: HeroId,
    },
    /// Too many players' stat counters moved in one tick to be gameplay.
    ///
    /// The counter events for that tick are withheld rather than reported. A tick differ
    /// cannot tell the post-game scoreboard resolving from a teamfight: every counter moves
    /// at once, and the naive reading is a twelve-player kill spree in a hundredth of a
    /// second.
    ///
    /// Reported rather than swallowed silently, so a consumer can tell "nothing happened"
    /// from "something happened that I do not believe". `players` is how many distinct
    /// players moved; the bound is [`EventTracker::anomaly_threshold`].
    StatAnomaly {
        /// How many distinct players' counters moved in the offending tick.
        players: usize,
    },
    /// The camera moved to a different player - you switched spectate target, or died and
    /// the game put you on a teammate.
    CurrentPlayerChanged {
        /// Lobby slot now on screen, if any.
        slot: Option<u32>,
        /// Hero now on screen.
        hero: Option<HeroId>,
    },
}

/// A tiny ordered map, for the two lookups the differ rebuilds on every tick.
///
/// Both are small - twelve players, a handful of ability upgrades each - and both are
/// thrown away and rebuilt ten times a second. At that size a sorted `Vec` costs one
/// allocation and no hashing to build, and beats a `HashMap` on lookup too.
///
/// The order is the reason as much as the cost. Iterating a `HashMap` yields an arbitrary
/// order, so two abilities upgraded on the same tick emitted their events in whichever
/// order the map felt like - which made the event stream unreproducible for anything
/// downstream that cared, and any test over more than one simultaneous change flaky.
#[derive(Clone, Debug, Default)]
struct SortedMap<K, V>(Vec<(K, V)>);

impl<K: Ord, V> SortedMap<K, V> {
    fn get(&self, key: &K) -> Option<&V> {
        self.0
            .binary_search_by(|(k, _)| k.cmp(key))
            .ok()
            .map(|i| &self.0[i].1)
    }

    fn iter(&self) -> std::slice::Iter<'_, (K, V)> {
        self.0.iter()
    }

    fn iter_mut(&mut self) -> std::slice::IterMut<'_, (K, V)> {
        self.0.iter_mut()
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for SortedMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut v: Vec<(K, V)> = iter.into_iter().collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        // A duplicate key would leave `get` returning whichever entry the search landed
        // on. Neither source can produce one - controller addresses are unique per player,
        // ability ids unique per player - so this only guards the invariant `get` needs.
        v.dedup_by(|a, b| a.0 == b.0);
        SortedMap(v)
    }
}

/// Per-player state carried between ticks.
///
/// Only the fields the differ compares, so a tracker costs a few hundred bytes rather
/// than a full snapshot clone.
#[derive(Clone, Debug, Default)]
struct PlayerState {
    slot: Option<u32>,
    hero: Option<HeroId>,
    name: Option<String>,
    kills: Option<u32>,
    deaths: Option<u32>,
    assists: Option<u32>,
    level: Option<u32>,
    items: Option<Vec<u32>>,
    upgrades: Option<SortedMap<u32, u32>>,
    gone: bool,
}

impl PlayerState {
    fn from_row(p: &PlayerRow) -> Self {
        PlayerState {
            slot: p.slot,
            hero: p.hero_id,
            name: p.name.clone(),
            // Never defaulted to zero: a field that failed to read would record 0, and
            // the next successful tick would diff 0 -> N into a burst of phantom kills,
            // deaths and level-ups.
            kills: p.kills,
            deaths: p.deaths,
            assists: p.assists,
            level: p.level,
            items: p.items.clone(),
            upgrades: p
                .ability_upgrades
                .as_ref()
                .map(|ups| ups.iter().map(|a| (a.item_id, a.points)).collect()),
            gone: p.has_abandoned(),
        }
    }
}

impl PlayerState {
    /// Keep the last known value for anything that failed to read this tick.
    ///
    /// A failed read is an absence of information, not a change. Storing `None` would be
    /// enough to suppress a phantom event now, but it would also leave the *next* tick
    /// with nothing to diff against, silently dropping a kill that really did happen in
    /// between. Carrying the old value forward suppresses the phantom and keeps the real
    /// event.
    fn carry_forward(&mut self, prev: &PlayerState) {
        self.kills = self.kills.or(prev.kills);
        self.deaths = self.deaths.or(prev.deaths);
        self.assists = self.assists.or(prev.assists);
        self.level = self.level.or(prev.level);
        if self.items.is_none() {
            self.items = prev.items.clone();
        }
        if self.upgrades.is_none() {
            self.upgrades = prev.upgrades.clone();
        }
    }
}

/// Snapshot state the differ needs, minus everything it does not compare.
#[derive(Clone, Debug, Default)]
struct WorldState {
    match_id: Option<u64>,
    /// The match clock, kept only to notice it running backwards.
    match_time: Option<f32>,
    game_state: Option<GameState>,
    paused: bool,
    midboss_kills: u32,
    objectives: Vec<(ObjectiveKind, Option<Team>)>,
    current: Option<u64>,
    /// Keyed by controller address, which is unique and stable for the life of a match.
    players: SortedMap<u64, PlayerState>,
}

/// Diffs consecutive snapshots into [`Event`]s.
///
/// How far the match clock may slip backwards before it counts as a seek.
///
/// Ticks are read from a running process rather than delivered, so two reads can land out
/// of order by a fraction of a second without anything having been scrubbed. A real seek
/// moves seconds at least; the smallest observed was roughly thirty.
pub const SEEK_EPSILON: f32 = 1.0;

/// How many distinct players' counters must move in one tick before it is disbelieved.
///
/// Above what a real teamfight produces and below a whole scoreboard. The busiest ordinary
/// tick moves two or three players; a scoreboard resolving moves twelve. Five sits closer
/// to the fight than to the scoreboard on purpose, because withholding a real event is the
/// lesser failure: a consumer that misses a kill has a gap, one that believes twelve fake
/// kills acts on them.
pub const DEFAULT_ANOMALY_PLAYERS: usize = 5;

/// Feed it every snapshot you take. It holds one tick of history, so it is cheap to keep
/// alive for a whole session.
#[derive(Debug)]
pub struct EventTracker {
    prev: Option<WorldState>,
    /// Whether this match's ending has already been reported. A match passes through
    /// more than one terminal phase (`PostGame` then often `End`), and only the first
    /// should fire. Cleared when the match id changes.
    ended_reported: bool,
    anomaly_players: usize,
}

impl Default for EventTracker {
    fn default() -> Self {
        EventTracker {
            prev: None,
            ended_reported: false,
            anomaly_players: DEFAULT_ANOMALY_PLAYERS,
        }
    }
}

impl EventTracker {
    /// A tracker with no history. The first [`EventTracker::update`] establishes a
    /// baseline and reports only match-level facts, never a burst of synthetic events for
    /// state that was already there.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set how many distinct players' counters moving in one tick counts as implausible.
    ///
    /// Lower is stricter. See [`DEFAULT_ANOMALY_PLAYERS`] for why the default sits where it
    /// does.
    #[must_use]
    pub fn anomaly_threshold(mut self, players: usize) -> Self {
        self.anomaly_players = players;
        self
    }

    /// Forget the previous tick.
    ///
    /// Call this if you stop polling for a while and do not want the gap to surface as one
    /// enormous delta.
    pub fn reset(&mut self) {
        self.prev = None;
        self.ended_reported = false;
    }

    /// Compare a snapshot against the previous one and return what changed.
    ///
    /// Events are ordered match-level first, then per-player, then objectives.
    pub fn update(&mut self, snap: &LiveSnapshot) -> Vec<Event> {
        let mut next = WorldState::from_snapshot(snap);
        if let Some(prev) = self.prev.as_ref() {
            next.carry_forward(prev);
        }
        let mut out = Vec::new();

        match self.prev.take() {
            None => {
                // First tick: no history to diff against, so announcing the match is the
                // only honest thing to report.
                if next.match_id.is_some() {
                    out.push(Event::MatchStarted {
                        match_id: next.match_id,
                    });
                }
                self.ended_reported = false;
            }
            Some(prev) if prev.match_id != next.match_id => {
                // A different match. Counters have reset and controller addresses have
                // been recycled, so nothing from the old state is comparable.
                if prev.match_id.is_some() {
                    out.push(Event::MatchLeft {
                        match_id: prev.match_id,
                    });
                }
                if next.match_id.is_some() {
                    out.push(Event::MatchStarted {
                        match_id: next.match_id,
                    });
                }
                self.ended_reported = false;
            }
            // A clock that went backwards means the stream was seeked. Checked before the
            // ordinary diff because nothing derived from comparing across a seek is true.
            Some(prev) if seeked(&prev, &next) => {
                if let (Some(from), Some(to)) = (prev.match_time, next.match_time) {
                    out.push(Event::ClockRewound { from, to });
                }
            }
            Some(prev) => {
                // Match end goes first: it happens before anything else this tick, and
                // it is the one event that carries data the next tick will not have.
                let was_over = prev.game_state.is_some_and(|g| g.is_over());
                let is_over = next.game_state.is_some_and(|g| g.is_over());
                if is_over && !was_over && !self.ended_reported {
                    self.ended_reported = true;
                    out.push(Event::MatchEnded {
                        match_id: next.match_id,
                        final_state: Box::new(snap.clone()),
                    });
                }
                let before = out.len();
                diff(&prev, &next, &mut out);
                suppress_stat_anomaly(&mut out, before, self.anomaly_players);
            }
        }

        self.prev = Some(next);
        out
    }
}

impl WorldState {
    fn from_snapshot(s: &LiveSnapshot) -> Self {
        WorldState {
            match_id: s.match_id,
            match_time: s.timers.match_time,
            game_state: s.game_state,
            paused: s.pause.is_paused(),
            midboss_kills: s.midboss_kills.unwrap_or(0),
            objectives: s.objectives.iter().map(|o| (o.kind, o.team)).collect(),
            current: s.current_player().map(|p| p.controller),
            players: s
                .players
                .iter()
                .filter(|p| !p.is_spectator)
                .map(|p| (p.controller, PlayerState::from_row(p)))
                .collect(),
        }
    }
}

/// Withhold this tick's counter events when too many players moved at once.
///
/// `from` is where this tick's events begin, so anything produced earlier in the same call
/// is untouched - notably [`Event::MatchEnded`], which carries a snapshot no later tick
/// has. An anomaly says the *stat counters* are not to be trusted, not that nothing
/// happened.
///
/// Only per-player counter events are withheld. Objective and world events do not come
/// from those counters and a resolving scoreboard does not implicate them.
/// Did the match clock run backwards between these two ticks?
///
/// Only a seek does that. The clock is paused-aware and stops rather than reversing, and a
/// new match is caught earlier by its match id, so a decrease here is the viewer scrubbing.
///
/// [`SEEK_EPSILON`] keeps float noise and the odd late tick from reading as one.
fn seeked(prev: &WorldState, next: &WorldState) -> bool {
    match (prev.match_time, next.match_time) {
        (Some(before), Some(after)) => after < before - SEEK_EPSILON,
        // A clock that could not be read is not a clock that went backwards.
        _ => false,
    }
}

fn suppress_stat_anomaly(out: &mut Vec<Event>, from: usize, threshold: usize) {
    fn counter_slot(e: &Event) -> Option<Option<u32>> {
        match e {
            Event::Kill { slot, .. }
            | Event::Death { slot, .. }
            | Event::Assist { slot, .. }
            | Event::LevelUp { slot, .. } => Some(*slot),
            _ => None,
        }
    }

    let mut movers: Vec<Option<u32>> = out[from..].iter().filter_map(counter_slot).collect();
    movers.sort_unstable();
    movers.dedup();
    if movers.len() < threshold {
        return;
    }

    let players = movers.len();
    let kept: Vec<Event> = out
        .drain(from..)
        .filter(|e| counter_slot(e).is_none())
        .collect();
    out.extend(kept);
    out.push(Event::StatAnomaly { players });
}

fn diff(prev: &WorldState, next: &WorldState, out: &mut Vec<Event>) {
    if let (Some(from), Some(to)) = (prev.game_state, next.game_state)
        && from != to
    {
        out.push(Event::GameStateChanged { from, to });
    }
    if prev.paused != next.paused {
        out.push(Event::PauseChanged {
            paused: next.paused,
        });
    }
    if next.midboss_kills > prev.midboss_kills {
        out.push(Event::MidbossKilled {
            total: next.midboss_kills,
        });
    }
    if prev.current != next.current {
        let p = next.current.and_then(|c| next.players.get(&c));
        out.push(Event::CurrentPlayerChanged {
            slot: p.and_then(|p| p.slot),
            hero: p.and_then(|p| p.hero),
        });
    }

    for (controller, now) in next.players.iter() {
        let Some(was) = prev.players.get(controller) else {
            // A player we have not seen before. Nothing to diff against, and treating
            // their whole history as "just happened" would be wrong.
            continue;
        };
        diff_player(was, now, out);
    }

    diff_objectives(prev, next, out);
}

impl WorldState {
    /// Apply [`PlayerState::carry_forward`] to every player still present.
    fn carry_forward(&mut self, prev: &WorldState) {
        for (controller, now) in self.players.iter_mut() {
            if let Some(before) = prev.players.get(controller) {
                now.carry_forward(before);
            }
        }
    }
}

fn diff_player(was: &PlayerState, now: &PlayerState, out: &mut Vec<Event>) {
    // Both sides must be known: see `Event::HeroChanged` for why neither learning a hero
    // nor losing the read counts as a swap.
    if let (Some(from), Some(to)) = (was.hero, now.hero)
        && from != to
    {
        out.push(Event::HeroChanged {
            slot: now.slot,
            from,
            to,
        });
    }

    // Every comparison below needs both sides. A tick where the field could not be read
    // says nothing about what changed, so it produces no events rather than a guess.
    //
    // Increase only. A decrease means a reset or a bad read, not an un-kill.
    if let (Some(before), Some(after)) = (was.kills, now.kills)
        && after > before
    {
        out.push(Event::Kill {
            slot: now.slot,
            hero: now.hero,
            total: after,
            delta: after - before,
        });
    }
    if let (Some(before), Some(after)) = (was.deaths, now.deaths)
        && after > before
    {
        out.push(Event::Death {
            slot: now.slot,
            hero: now.hero,
            total: after,
            delta: after - before,
        });
    }
    if let (Some(before), Some(after)) = (was.assists, now.assists)
        && after > before
    {
        out.push(Event::Assist {
            slot: now.slot,
            hero: now.hero,
            total: after,
            delta: after - before,
        });
    }
    if let (Some(before), Some(after)) = (was.level, now.level)
        && after > before
    {
        out.push(Event::LevelUp {
            slot: now.slot,
            hero: now.hero,
            level: after,
        });
    }

    diff_items(was, now, out);
    diff_upgrades(was, now, out);
}

/// Items are a multiset: the same item can legitimately be held twice, so compare by
/// count rather than by set membership.
fn diff_items(was: &PlayerState, now: &PlayerState, out: &mut Vec<Event>) {
    let (Some(before), Some(after)) = (&was.items, &now.items) else {
        return;
    };
    for (item, delta) in multiset_delta(before, after) {
        let ev = if delta > 0 {
            Event::ItemPurchased {
                slot: now.slot,
                item: ItemId(item),
            }
        } else {
            Event::ItemLost {
                slot: now.slot,
                item: ItemId(item),
            }
        };
        for _ in 0..delta.unsigned_abs() {
            out.push(ev.clone());
        }
    }
}

fn diff_upgrades(was: &PlayerState, now: &PlayerState, out: &mut Vec<Event>) {
    let (Some(before), Some(after)) = (&was.upgrades, &now.upgrades) else {
        return;
    };
    for (ability, points) in after.iter() {
        if before.get(ability).copied().unwrap_or(0) < *points {
            out.push(Event::AbilityUpgraded {
                slot: now.slot,
                ability: ItemId(*ability),
                points: *points,
            });
        }
    }

    if !was.gone && now.gone {
        out.push(Event::PlayerLeft {
            slot: now.slot,
            name: now.name.clone(),
        });
    } else if was.gone && !now.gone {
        out.push(Event::PlayerRejoined {
            slot: now.slot,
            name: now.name.clone(),
        });
    }
}

/// Objectives are keyed by `(kind, team)` and there are several of each, so the only
/// thing that can be said is how many of a kind a side still has.
fn diff_objectives(prev: &WorldState, next: &WorldState, out: &mut Vec<Event>) {
    let mut before: HashMap<(ObjectiveKind, Option<Team>), i32> = HashMap::new();
    for k in &prev.objectives {
        *before.entry(*k).or_default() += 1;
    }
    for k in &next.objectives {
        *before.entry(*k).or_default() -= 1;
    }
    let mut lost: Vec<_> = before.into_iter().filter(|(_, n)| *n > 0).collect();
    // HashMap order is not stable; sort so the event stream is reproducible.
    lost.sort_by_key(|((kind, team), _)| (*kind, team.map(|t| t.get())));
    for ((kind, team), count) in lost {
        for _ in 0..count {
            out.push(Event::ObjectiveDestroyed { kind, team });
        }
    }
}

/// Per-id count difference between two multisets, skipping unchanged ids.
fn multiset_delta(was: &[u32], now: &[u32]) -> Vec<(u32, i32)> {
    let mut counts: HashMap<u32, i32> = HashMap::new();
    for i in now {
        *counts.entry(*i).or_default() += 1;
    }
    for i in was {
        *counts.entry(*i).or_default() -= 1;
    }
    let mut out: Vec<_> = counts.into_iter().filter(|(_, n)| *n != 0).collect();
    out.sort_by_key(|(id, _)| *id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{AbilityUpgrade, Objective, PauseState, StructureState};

    fn player(controller: u64, slot: u32) -> PlayerRow {
        PlayerRow {
            controller,
            slot: Some(slot),
            team: Some(Team::AMBER),
            hero_id: Some(HeroId(1)),
            kills: Some(0),
            deaths: Some(0),
            assists: Some(0),
            level: Some(1),
            // A successful read of an empty list, which is what a real snapshot gives.
            // `None` here would mean "could not be read" and is tested separately.
            items: Some(Vec::new()),
            ability_upgrades: Some(Vec::new()),
            ..Default::default()
        }
    }

    fn snap(players: Vec<PlayerRow>) -> LiveSnapshot {
        LiveSnapshot {
            match_id: Some(42),
            game_state: Some(GameState::GameInProgress),
            players,
            ..Default::default()
        }
    }

    /// A whole scoreboard jumping at once is not a teamfight.
    ///
    /// The original suppressed on exactly this shape, logging "N events in a row within
    /// Ns of each other - stat-counter anomaly (post-game scoreboard?)". A tick differ
    /// cannot tell the post-game screen from gameplay: every counter moves at once, and
    /// the naive reading is a twelve-player kill spree in a hundredth of a second.
    #[test]
    fn a_whole_scoreboard_moving_at_once_is_reported_as_an_anomaly_not_as_kills() {
        let mut t = EventTracker::new();
        let before: Vec<PlayerRow> = (0..12).map(|i| player(i + 1, i as u32)).collect();
        t.update(&snap(before.clone()));

        let after: Vec<PlayerRow> = before
            .iter()
            .enumerate()
            .map(|(i, p)| PlayerRow {
                kills: Some(i as u32 + 3),
                deaths: Some(i as u32 + 2),
                ..p.clone()
            })
            .collect();
        let ev = t.update(&snap(after));

        assert!(
            ev.iter().any(|e| matches!(e, Event::StatAnomaly { .. })),
            "expected an anomaly report, got {ev:?}"
        );
        assert!(
            !ev.iter()
                .any(|e| matches!(e, Event::Kill { .. } | Event::Death { .. })),
            "counter events must be withheld, got {ev:?}"
        );
    }

    /// A real teamfight still reports, and is not mistaken for the scoreboard.
    ///
    /// The suppression is worthless if it also swallows the busiest moment of a match, so
    /// the threshold has to sit above what a fight actually produces in one tick.
    #[test]
    fn a_teamfight_is_still_reported() {
        let mut t = EventTracker::new();
        let before: Vec<PlayerRow> = (0..12).map(|i| player(i + 1, i as u32)).collect();
        t.update(&snap(before.clone()));

        let mut after = before.clone();
        after[0].kills = Some(1);
        after[1].kills = Some(1);
        after[2].deaths = Some(1);
        after[3].deaths = Some(1);
        let ev = t.update(&snap(after));

        assert!(
            !ev.iter().any(|e| matches!(e, Event::StatAnomaly { .. })),
            "a fight is not an anomaly, got {ev:?}"
        );
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, Event::Kill { .. }))
                .count(),
            2
        );
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, Event::Death { .. }))
                .count(),
            2
        );
    }

    /// The threshold is adjustable, because what counts as implausible is a judgement.
    #[test]
    fn the_anomaly_threshold_can_be_tightened() {
        let mut t = EventTracker::new().anomaly_threshold(3);
        let before: Vec<PlayerRow> = (0..12).map(|i| player(i + 1, i as u32)).collect();
        t.update(&snap(before.clone()));

        let mut after = before.clone();
        for p in after.iter_mut().take(3) {
            p.kills = Some(1);
        }
        let ev = t.update(&snap(after));
        assert!(ev.iter().any(|e| matches!(e, Event::StatAnomaly { .. })));
    }

    /// Non-counter events survive the suppression.
    ///
    /// An anomaly says the *stat counters* cannot be trusted this tick. It says nothing
    /// about the match having ended, which is the one event whose data the next tick will
    /// not carry - withholding that would lose it for good.
    #[test]
    fn an_anomaly_does_not_withhold_the_end_of_the_match() {
        let mut t = EventTracker::new();
        let before: Vec<PlayerRow> = (0..12).map(|i| player(i + 1, i as u32)).collect();
        t.update(&snap(before.clone()));

        let after: Vec<PlayerRow> = before
            .iter()
            .enumerate()
            .map(|(i, p)| PlayerRow {
                kills: Some(i as u32 + 3),
                ..p.clone()
            })
            .collect();
        let mut over = snap(after);
        over.game_state = Some(GameState::PostGame);
        let ev = t.update(&over);

        assert!(ev.iter().any(|e| matches!(e, Event::MatchEnded { .. })));
        assert!(ev.iter().any(|e| matches!(e, Event::StatAnomaly { .. })));
        assert!(!ev.iter().any(|e| matches!(e, Event::Kill { .. })));
    }

    /// The bug: a field that failed to read recorded 0, and the next good tick diffed
    /// 0 -> N into a burst of kills, deaths and level-ups that never happened.
    #[test]
    fn a_field_that_could_not_be_read_produces_no_events() {
        let mut t = EventTracker::new();

        let mut blind = player(1, 1);
        blind.kills = None;
        blind.deaths = None;
        blind.level = None;
        t.update(&snap(vec![blind]));

        let mut good = player(1, 1);
        good.kills = Some(7);
        good.deaths = Some(3);
        good.level = Some(12);
        let ev = t.update(&snap(vec![good]));
        assert_eq!(ev, vec![], "recovering a read is not seven kills");
    }

    /// Once both sides are known again, real changes are still reported.
    #[test]
    fn events_resume_after_a_blind_tick() {
        let mut t = EventTracker::new();
        t.update(&snap(vec![player(1, 1)]));

        let mut blind = player(1, 1);
        blind.kills = None;
        assert_eq!(t.update(&snap(vec![blind])), vec![]);

        let mut good = player(1, 1);
        good.kills = Some(1);
        let ev = t.update(&snap(vec![good]));
        assert_eq!(
            ev,
            vec![Event::Kill {
                slot: Some(1),
                hero: Some(HeroId(1)),
                total: 1,
                delta: 1,
            }],
            "a genuine kill after a blind tick is still a kill"
        );
    }

    /// An incomplete item read used to arrive as a shortened list, which the multiset
    /// diff read as the missing items having been sold.
    #[test]
    fn an_unreadable_item_list_is_not_a_sale() {
        let mut t = EventTracker::new();
        let mut a = player(1, 1);
        a.items = Some(vec![10, 11, 12]);
        t.update(&snap(vec![a]));

        let mut blind = player(1, 1);
        blind.items = None;
        assert_eq!(t.update(&snap(vec![blind])), vec![], "no phantom sales");

        let mut back = player(1, 1);
        back.items = Some(vec![10, 11, 12]);
        assert_eq!(t.update(&snap(vec![back])), vec![], "no phantom purchases");
    }

    #[test]
    fn first_tick_announces_the_match_and_nothing_else() {
        let mut t = EventTracker::new();
        let ev = t.update(&snap(vec![player(1, 1)]));
        assert_eq!(ev, vec![Event::MatchStarted { match_id: Some(42) }]);
    }

    #[test]
    fn a_steady_state_produces_no_events() {
        let mut t = EventTracker::new();
        let s = snap(vec![player(1, 1)]);
        t.update(&s);
        assert!(t.update(&s).is_empty());
    }

    /// An assist is diffed the same way a kill is, and is not a kill.
    ///
    /// Companion's timeline has a `kind` set of `kill | death | assist`, and this crate
    /// read `m_iPlayerAssists` into [`PlayerRow::assists`](crate::snapshot::PlayerRow)
    /// without ever diffing it, so the third column could only ever have been empty - which
    /// reads as a measurement that came out zero rather than as a column with no source.
    #[test]
    fn an_assist_is_its_own_event_and_does_not_look_like_a_kill() {
        let mut t = EventTracker::new();
        t.update(&snap(vec![player(1, 1)]));

        let mut a = player(1, 1);
        a.assists = Some(2);
        let ev = t.update(&snap(vec![a]));

        assert_eq!(
            ev,
            vec![Event::Assist {
                slot: Some(1),
                hero: Some(HeroId(1)),
                total: 2,
                delta: 2,
            }]
        );
    }

    /// An assist counter that stops reading does not become an assist when it comes back.
    ///
    /// The same guard every other counter has: a decrease is a reset or a bad read, not an
    /// un-assist, and a tick missing either endpoint says nothing about what changed.
    #[test]
    fn an_assist_counter_going_backwards_or_missing_produces_nothing() {
        let mut t = EventTracker::new();
        let mut start = player(1, 1);
        start.assists = Some(5);
        t.update(&snap(vec![start]));

        let mut back = player(1, 1);
        back.assists = Some(2);
        assert!(
            t.update(&snap(vec![back])).is_empty(),
            "a decrease is not an event"
        );

        let mut gone = player(1, 1);
        gone.assists = None;
        assert!(
            t.update(&snap(vec![gone])).is_empty(),
            "an unread counter is not an event"
        );
    }

    /// An assist that happens across an unreadable tick is still reported.
    ///
    /// The carry-forward every counter has, exercised for assists. Without it the last
    /// known total is lost when one tick fails to read, and the next successful tick has
    /// no `before` to diff against - so the assist is **dropped silently** rather than
    /// duplicated. That is the quiet direction of the failure, which is why it needs a
    /// test of its own rather than being assumed from the kill case.
    #[test]
    fn an_assist_across_an_unreadable_tick_is_not_lost() {
        let mut t = EventTracker::new();
        let mut start = player(1, 1);
        start.assists = Some(1);
        t.update(&snap(vec![start]));

        let mut blind = player(1, 1);
        blind.assists = None;
        assert!(t.update(&snap(vec![blind])).is_empty());

        let mut back = player(1, 1);
        back.assists = Some(3);
        assert_eq!(
            t.update(&snap(vec![back])),
            vec![Event::Assist {
                slot: Some(1),
                hero: Some(HeroId(1)),
                total: 3,
                delta: 2,
            }]
        );
    }

    /// Assists count toward the scoreboard-anomaly guard, like the other counters.
    ///
    /// The guard exists because a tick differ cannot tell a post-game scoreboard resolving
    /// from a teamfight. Assists move in exactly that burst, so leaving them out would let
    /// a scoreboard through as a spray of assist events after the kills were suppressed.
    #[test]
    fn assists_are_counted_by_the_anomaly_guard() {
        let mut t = EventTracker::new();
        let start: Vec<_> = (1..=6).map(|s| player(s, s as u32)).collect();
        t.update(&snap(start));

        let moved: Vec<_> = (1..=6)
            .map(|s| {
                let mut p = player(s, s as u32);
                p.assists = Some(4);
                p
            })
            .collect();
        let ev = t.update(&snap(moved));
        assert!(
            ev.iter().any(|e| matches!(e, Event::StatAnomaly { .. })),
            "six players' assists moving at once should be disbelieved: {ev:#?}"
        );
        assert!(
            !ev.iter().any(|e| matches!(e, Event::Assist { .. })),
            "the assists themselves must be withheld: {ev:#?}"
        );
    }

    #[test]
    fn kills_and_deaths_are_reported_separately_and_not_paired() {
        let mut t = EventTracker::new();
        t.update(&snap(vec![player(1, 1), player(2, 2)]));

        let mut a = player(1, 1);
        a.kills = Some(1);
        let mut b = player(2, 2);
        b.deaths = Some(1);
        let ev = t.update(&snap(vec![a, b]));

        assert!(ev.contains(&Event::Kill {
            slot: Some(1),
            hero: Some(HeroId(1)),
            total: 1,
            delta: 1,
        }));
        assert!(ev.contains(&Event::Death {
            slot: Some(2),
            hero: Some(HeroId(1)),
            total: 1,
            delta: 1,
        }));
    }

    #[test]
    fn a_slow_poll_collapses_into_one_event_carrying_the_delta() {
        let mut t = EventTracker::new();
        t.update(&snap(vec![player(1, 1)]));
        let mut a = player(1, 1);
        a.kills = Some(3);
        let ev = t.update(&snap(vec![a]));
        assert_eq!(
            ev,
            vec![Event::Kill {
                slot: Some(1),
                hero: Some(HeroId(1)),
                total: 3,
                delta: 3,
            }]
        );
    }

    #[test]
    fn counters_going_backwards_emit_nothing() {
        let mut t = EventTracker::new();
        let mut a = player(1, 1);
        a.kills = Some(5);
        t.update(&snap(vec![a]));
        let ev = t.update(&snap(vec![player(1, 1)]));
        assert!(ev.is_empty(), "{ev:?}");
    }

    /// Several abilities can be levelled between two polls, and several players can do it
    /// on the same tick. While these were `HashMap`s the resulting events came out in
    /// whatever order iteration happened to give, so a consumer replaying the stream got a
    /// different sequence each run for identical input.
    #[test]
    fn simultaneous_upgrades_are_emitted_in_a_stable_order() {
        let ups = |pairs: &[(u32, u32)]| {
            Some(
                pairs
                    .iter()
                    .map(|&(item_id, points)| AbilityUpgrade {
                        item_id,
                        points,
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            )
        };

        let run = || {
            let mut t = EventTracker::new();
            let (mut a, mut b) = (player(0x2000, 1), player(0x1000, 2));
            a.ability_upgrades = ups(&[(30, 1), (10, 1), (20, 1)]);
            b.ability_upgrades = ups(&[(40, 1)]);
            t.update(&snap(vec![a, b]));

            let (mut a, mut b) = (player(0x2000, 1), player(0x1000, 2));
            a.ability_upgrades = ups(&[(30, 2), (10, 2), (20, 2)]);
            b.ability_upgrades = ups(&[(40, 2)]);
            t.update(&snap(vec![a, b]))
        };

        let expected = vec![
            Event::AbilityUpgraded {
                slot: Some(2),
                ability: ItemId(40),
                points: 2,
            },
            Event::AbilityUpgraded {
                slot: Some(1),
                ability: ItemId(10),
                points: 2,
            },
            Event::AbilityUpgraded {
                slot: Some(1),
                ability: ItemId(20),
                points: 2,
            },
            Event::AbilityUpgraded {
                slot: Some(1),
                ability: ItemId(30),
                points: 2,
            },
        ];
        for _ in 0..8 {
            assert_eq!(run(), expected);
        }
    }

    #[test]
    fn duplicate_items_are_counted_not_deduplicated() {
        let mut t = EventTracker::new();
        let mut a = player(1, 1);
        a.items = Some(vec![10, 10]);
        t.update(&snap(vec![a]));

        let mut b = player(1, 1);
        b.items = Some(vec![10, 10, 10]);
        let ev = t.update(&snap(vec![b]));
        assert_eq!(
            ev,
            vec![Event::ItemPurchased {
                slot: Some(1),
                item: ItemId(10),
            }]
        );
    }

    #[test]
    fn selling_one_of_a_pair_reports_a_single_loss() {
        let mut t = EventTracker::new();
        let mut a = player(1, 1);
        a.items = Some(vec![10, 10]);
        t.update(&snap(vec![a]));

        let mut b = player(1, 1);
        b.items = Some(vec![10]);
        let ev = t.update(&snap(vec![b]));
        assert_eq!(
            ev,
            vec![Event::ItemLost {
                slot: Some(1),
                item: ItemId(10),
            }]
        );
    }

    #[test]
    fn ability_points_report_the_new_total() {
        let mut t = EventTracker::new();
        t.update(&snap(vec![player(1, 1)]));
        let mut a = player(1, 1);
        a.ability_upgrades = Some(vec![AbilityUpgrade {
            item_id: 7,
            points: 2,
            unlocked: true,
            raw: 0,
        }]);
        let ev = t.update(&snap(vec![a]));
        assert_eq!(
            ev,
            vec![Event::AbilityUpgraded {
                slot: Some(1),
                ability: ItemId(7),
                points: 2,
            }]
        );
    }

    #[test]
    fn a_new_match_id_closes_the_old_match_and_suppresses_stale_diffs() {
        let mut t = EventTracker::new();
        let mut a = player(1, 1);
        a.kills = Some(9);
        t.update(&snap(vec![a]));

        let mut next = snap(vec![player(1, 1)]);
        next.match_id = Some(43);
        let ev = t.update(&next);
        assert_eq!(
            ev,
            vec![
                Event::MatchLeft { match_id: Some(42) },
                Event::MatchStarted { match_id: Some(43) },
            ],
            "controller 1 is a different person in a new match; its counters must not diff"
        );
    }

    #[test]
    fn a_destroyed_structure_is_reported_once() {
        let obj = |kind: ObjectiveKind| Objective {
            kind,
            team: Some(Team::AMBER),
            team_name: None,
            health: Some(100),
            max_health: Some(100),
            class: "C_NPC_TrooperBoss".into(),
            address: 0,
            lane: None,
            state: StructureState::Standing,
            #[cfg(feature = "positions")]
            position: None,
        };
        let mut before = snap(vec![]);
        before.objectives = vec![obj(ObjectiveKind::Walker), obj(ObjectiveKind::Walker)];
        let mut after = snap(vec![]);
        after.objectives = vec![obj(ObjectiveKind::Walker)];

        let mut t = EventTracker::new();
        t.update(&before);
        assert_eq!(
            t.update(&after),
            vec![Event::ObjectiveDestroyed {
                kind: ObjectiveKind::Walker,
                team: Some(Team::AMBER),
            }]
        );
    }

    #[test]
    fn pausing_flips_once_not_every_tick() {
        let mut t = EventTracker::new();
        let s = snap(vec![]);
        t.update(&s);

        let mut paused = snap(vec![]);
        paused.pause = PauseState {
            game_paused: Some(true),
            ..Default::default()
        };
        assert_eq!(
            t.update(&paused),
            vec![Event::PauseChanged { paused: true }]
        );
        assert!(t.update(&paused).is_empty());
    }

    #[test]
    fn reset_drops_history_so_the_next_tick_is_a_baseline() {
        let mut t = EventTracker::new();
        let mut a = player(1, 1);
        a.kills = Some(5);
        t.update(&snap(vec![a]));
        t.reset();
        let ev = t.update(&snap(vec![player(1, 1)]));
        assert_eq!(ev, vec![Event::MatchStarted { match_id: Some(42) }]);
    }

    /// Build a snapshot in a given phase carrying a two-row scoreboard, so the captured
    /// final state has something in it worth asserting on.
    fn snap_in(state: GameState) -> LiveSnapshot {
        let mut s = snap(vec![player(1, 1), player(2, 7)]);
        s.game_state = Some(state);
        s
    }

    /// The whole point of the event: the final scoreboard travels with it, because the
    /// client tears the match down 12 to 19 seconds later and the next tick has nothing.
    #[test]
    fn match_end_carries_the_final_snapshot() {
        let mut t = EventTracker::new();
        t.update(&snap_in(GameState::GameInProgress));

        let ev = t.update(&snap_in(GameState::PostGame));
        let captured = ev
            .iter()
            .find_map(|e| match e {
                Event::MatchEnded {
                    match_id,
                    final_state,
                } => Some((*match_id, final_state.clone())),
                _ => None,
            })
            .expect("PostGame must produce a MatchEnded");
        assert_eq!(captured.0, Some(42));
        assert_eq!(
            captured.1.scoreboard().count(),
            2,
            "the roster came with it"
        );
        assert_eq!(captured.1.game_state, Some(GameState::PostGame));
    }

    /// A match passes through `PostGame` and then usually `End`. Only the first is an
    /// ending; firing on both would deliver two conflicting "final" snapshots.
    #[test]
    fn match_end_fires_once_across_several_terminal_phases() {
        let mut t = EventTracker::new();
        t.update(&snap_in(GameState::GameInProgress));

        let ended = |evs: &[Event]| {
            evs.iter()
                .filter(|e| matches!(e, Event::MatchEnded { .. }))
                .count()
        };
        assert_eq!(ended(&t.update(&snap_in(GameState::PostGame))), 1);
        assert_eq!(
            ended(&t.update(&snap_in(GameState::End))),
            0,
            "End must not refire"
        );
        assert_eq!(ended(&t.update(&snap_in(GameState::End))), 0);
    }

    /// Only about 40% of matches reach `End`, so a match that goes straight from
    /// `PostGame` back to the Hideout must still have produced its ending.
    #[test]
    fn leaving_is_a_separate_weaker_event_than_ending() {
        let mut t = EventTracker::new();
        t.update(&snap_in(GameState::GameInProgress));
        assert_eq!(
            t.update(&snap_in(GameState::PostGame))
                .iter()
                .filter(|e| matches!(e, Event::MatchEnded { .. }))
                .count(),
            1
        );

        let mut gone = snap(vec![]);
        gone.match_id = None;
        gone.game_state = Some(GameState::GameInProgress);
        let ev = t.update(&gone);
        assert!(ev.contains(&Event::MatchLeft { match_id: Some(42) }));
        assert!(
            !ev.iter().any(|e| matches!(e, Event::MatchEnded { .. })),
            "leaving must not masquerade as an ending"
        );
    }

    /// A tracker that first sees the game already in `PostGame` has observed no
    /// transition, so it cannot tell "just ended" from "was already over".
    #[test]
    fn no_ending_is_invented_without_observing_the_transition() {
        let mut t = EventTracker::new();
        let ev = t.update(&snap_in(GameState::PostGame));
        assert!(!ev.iter().any(|e| matches!(e, Event::MatchEnded { .. })));
    }

    /// A new match must be able to end too; the once-per-match latch has to clear.
    #[test]
    fn a_second_match_can_also_end() {
        let mut t = EventTracker::new();
        t.update(&snap_in(GameState::GameInProgress));
        t.update(&snap_in(GameState::PostGame));

        let mut next = snap_in(GameState::GameInProgress);
        next.match_id = Some(43);
        t.update(&next);
        let mut over = snap_in(GameState::PostGame);
        over.match_id = Some(43);

        let ev = t.update(&over);
        assert!(ev.iter().any(|e| matches!(
            e,
            Event::MatchEnded {
                match_id: Some(43),
                ..
            }
        )));
    }

    /// A replay seeking backwards is reported, not silently diffed across.
    ///
    /// Found in a live spectate: the match clock read 27:24, then 27:16, then 26:25, while
    /// `m_unMatchID` never changed. The viewer was scrubbing. `EventTracker` re-baselines
    /// only on a new match id, so a seek is invisible to it, and the counters it diffs walk
    /// backwards and then forwards over ground already reported.
    ///
    /// Re-reporting the events either side of a seek is defensible - a viewer watching that
    /// stretch again is genuinely seeing them again - but doing it *silently* is not, and a
    /// recorder folding the stream into a match history has no way to tell a duplicate from
    /// a second kill. Same treatment as [`Event::StatAnomaly`]: name the discontinuity and
    /// let the consumer decide.
    ///
    /// The tick that jumps is not diffed at all. Every counter moves at once across a seek,
    /// and nothing derived from comparing the two sides of it means anything.
    #[test]
    fn a_match_clock_running_backwards_is_reported_as_a_seek() {
        let mut t = EventTracker::new();
        let at = |secs: f32, kills: u32| {
            let mut p = player(1, 0);
            p.kills = Some(kills);
            let mut s = snap(vec![p]);
            s.timers.match_time = Some(secs);
            s
        };

        t.update(&at(100.0, 0));
        let forward = t.update(&at(101.0, 1));
        assert!(
            forward.iter().any(|e| matches!(e, Event::Kill { .. })),
            "a clock running forward still diffs normally: {forward:?}"
        );

        let seek = t.update(&at(60.0, 0));
        assert_eq!(
            seek.len(),
            1,
            "the seek tick reports the seek and nothing else: {seek:?}"
        );
        match seek[0] {
            Event::ClockRewound { from, to } => {
                assert!((from - 101.0).abs() < 0.01, "from {from}");
                assert!((to - 60.0).abs() < 0.01, "to {to}");
            }
            ref other => panic!("expected a seek, got {other:?}"),
        }

        let after = t.update(&at(61.0, 1));
        assert!(
            after.iter().any(|e| matches!(e, Event::Kill { .. })),
            "diffing resumes after the seek: {after:?}"
        );
    }

    /// A player swapping hero is reported; first learning their hero is not.
    ///
    /// The distinction is the whole design. `None -> Some(hero)` is this crate learning
    /// what was always true - the read finally succeeded, or the draft resolved - and
    /// reporting it as a change would fire a popup for all twelve players the moment a
    /// match starts. Only `Some(a) -> Some(b)` is somebody actually swapping.
    ///
    /// `Some -> None` is likewise not a change: a hero id that stopped reading says
    /// nothing about the hero, and announcing a swap on a failed read is the exact
    /// absent-versus-zero mistake this crate is built to avoid.
    #[test]
    fn a_hero_swap_is_reported_but_learning_a_hero_is_not() {
        let mut t = EventTracker::new();
        let with_hero = |hero: Option<u32>| {
            let mut p = player(1, 0);
            p.hero_id = hero.map(HeroId);
            snap(vec![p])
        };

        t.update(&with_hero(None));
        let learned = t.update(&with_hero(Some(1)));
        assert!(
            !learned
                .iter()
                .any(|e| matches!(e, Event::HeroChanged { .. })),
            "learning a hero is not a swap: {learned:?}"
        );

        let swapped = t.update(&with_hero(Some(7)));
        let found = swapped
            .iter()
            .find(|e| matches!(e, Event::HeroChanged { .. }))
            .expect("a swap must be reported");
        match found {
            Event::HeroChanged { slot, from, to } => {
                assert_eq!(*slot, Some(0));
                assert_eq!(*from, HeroId(1));
                assert_eq!(*to, HeroId(7));
            }
            other => panic!("expected a swap, got {other:?}"),
        }

        let same = t.update(&with_hero(Some(7)));
        assert!(!same.iter().any(|e| matches!(e, Event::HeroChanged { .. })));

        let unread = t.update(&with_hero(None));
        assert!(
            !unread
                .iter()
                .any(|e| matches!(e, Event::HeroChanged { .. })),
            "an unreadable hero id is not a swap: {unread:?}"
        );
    }
}
