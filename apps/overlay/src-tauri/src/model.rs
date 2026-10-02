//! What the overlay sends to the window.
//!
//! Deliberately its own set of structs rather than serialising the reader's types. The
//! window wants strings it can paint, and pinning that shape here means a change in the
//! reader shows up as a compile error rather than as a blank panel.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use deadlock_core::{HeroId, HeroNames, RankBadge, Team};
use deadlock_data::HeroCatalog;
use deadlock_reader::events::{Event, EventTracker};
use deadlock_reader::snapshot::{LiveSnapshot, PlayerRow, TeamStats};
use deadlock_walker::ext::RosterExt;
use serde::Serialize;

/// A hero named for display, carrying whatever art the catalog knows about it.
///
/// Art is a URL rather than bytes: the overlay never fetches anything itself, and the
/// window decides whether to try. `None` is not "no hero" - it means the catalog has no
/// published art of that kind, which the window has to draw as a deliberate stand-in
/// rather than as a gap. `hero_id` is always real, so the window can key a stable colour
/// off it even with no art at all.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeroRef {
    pub hero_id: u32,
    /// Display name, already falling back to `hero 42` for an id the catalog lacks.
    pub name: String,
    /// Small square scoreboard portrait.
    pub portrait: Option<String>,
    /// Full hero card art, for anywhere with more room than a row.
    pub card: Option<String>,
}

impl HeroRef {
    fn new(id: HeroId, heroes: &HeroCatalog) -> Self {
        HeroRef {
            hero_id: id.0,
            name: heroes.display_name(id),
            portrait: heroes.portrait(id).map(str::to_owned),
            card: heroes.card(id).map(str::to_owned),
        }
    }
}

/// How long a swap keeps being carried on the match view after it happened.
///
/// This is not how long the popup is shown - the window owns that - it is how long the
/// window has to *notice*. Long enough to survive a stretch of dropped frames, short
/// enough that a swap cannot resurface in a meaningfully different match state.
pub const SWAP_RETENTION: Duration = Duration::from_secs(10);

/// Most swaps carried at once.
///
/// A bound rather than a expectation: twelve players cannot swap more than a handful of
/// times inside [`SWAP_RETENTION`], and a payload that could grow without limit is a
/// payload a stuck reader could grow without limit.
pub const SWAP_CAPACITY: usize = 12;

/// A player swapping from one hero to another, mid-match.
///
/// Both heroes are always known: [`Event::HeroChanged`] fires only for a swap between two
/// identified heroes, never for a hero id that has just started or stopped reading. The
/// *player* is a different matter - `slot` and `player` are each `None` when the reader
/// could not attribute the swap, which the window has to state rather than paint as slot
/// zero or a nameless row.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeroSwapView {
    /// Monotonic id, unique for the life of the process.
    ///
    /// The same swap is carried on several consecutive frames (see [`SwapLog`]), and this
    /// is what lets the window tell a re-sent swap from a second one. It never changes
    /// while a swap is carried, and never repeats afterwards.
    pub seq: u64,
    /// Lobby slot, or `None` when the row carried no slot. Never `0` as a stand-in.
    pub slot: Option<u32>,
    /// Steam persona name, or `None` when it did not read.
    ///
    /// Deliberately not [`PlayerRow::display_name`]'s `Slot 4` fallback: `slot` already
    /// says that, and a made-up name in this field would be indistinguishable from a real
    /// one.
    pub player: Option<String>,
    /// The hero they were on.
    pub from: HeroRef,
    /// The hero they are on now.
    pub to: HeroRef,
}

/// Turns the snapshot stream into the hero swaps the window should be told about.
///
/// # Why the swaps are carried rather than fired once
///
/// A swap is a transient: it exists on exactly the tick the differ notices it, and the
/// tick after that the world simply looks like a player on a different hero. The window is
/// on the other side of an event bridge it can miss frames of - it is not attached until
/// it mounts, and it drops whatever arrives while it is busy - so a swap published on one
/// frame and never again is a swap that vanishes whenever the timing is unlucky.
///
/// So the log holds each swap for [`SWAP_RETENTION`] and republishes it on every frame in
/// that window. That fixes the dropped frame, and creates the opposite problem: a window
/// that treated each frame's list as new would re-fire the same popup forty times. Hence
/// [`HeroSwapView::seq`] - stable while a swap is carried, never reused - so the window can
/// keep a high-water mark and act on each swap exactly once however many times it sees it.
#[derive(Debug, Default)]
pub struct SwapLog {
    tracker: EventTracker,
    /// Carried swaps with the instant each was noticed, oldest first.
    recent: VecDeque<(Instant, HeroSwapView)>,
    seq: u64,
}

impl SwapLog {
    /// A log with no history. The first snapshot is a baseline and produces no swaps.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the match: no snapshot to diff against, and no swaps worth carrying.
    ///
    /// For leaving a match or losing the client, not for a single failed read - a hiccup
    /// that dropped the baseline would silently lose a swap that happened across it.
    pub fn clear(&mut self) {
        self.tracker.reset();
        self.recent.clear();
    }

    /// Feed a snapshot; get back every swap still worth carrying, oldest first.
    pub fn observe(&mut self, snap: &LiveSnapshot, heroes: &HeroCatalog) -> Vec<HeroSwapView> {
        self.observe_at(snap, heroes, Instant::now())
    }

    fn observe_at(
        &mut self,
        snap: &LiveSnapshot,
        heroes: &HeroCatalog,
        now: Instant,
    ) -> Vec<HeroSwapView> {
        let mut fresh = false;
        let mut found = Vec::new();
        for event in self.tracker.update(snap) {
            match event {
                // A different lobby. Whatever was carried belongs to players who are no
                // longer there, and the tracker has re-baselined anyway.
                Event::MatchStarted { .. } | Event::MatchLeft { .. } => fresh = true,
                Event::HeroChanged { slot, from, to } => {
                    self.seq += 1;
                    found.push((
                        now,
                        HeroSwapView {
                            seq: self.seq,
                            slot,
                            player: swapper_name(snap, slot),
                            from: HeroRef::new(from, heroes),
                            to: HeroRef::new(to, heroes),
                        },
                    ));
                }
                _ => {}
            }
        }

        if fresh {
            self.recent.clear();
        }
        self.recent.extend(found);
        while self
            .recent
            .front()
            .is_some_and(|(seen, _)| now.saturating_duration_since(*seen) >= SWAP_RETENTION)
        {
            self.recent.pop_front();
        }
        while self.recent.len() > SWAP_CAPACITY {
            self.recent.pop_front();
        }
        self.recent.iter().map(|(_, s)| s.clone()).collect()
    }
}

/// The persona name of whoever is in `slot`, when there is one to give.
///
/// An empty string is an absence rather than a name: the reader hands back what it read,
/// and a row whose name buffer was blank must not become a player called nothing.
fn swapper_name(snap: &LiveSnapshot, slot: Option<u32>) -> Option<String> {
    let slot = slot?;
    snap.scoreboard()
        .find(|p| p.slot == Some(slot))
        .and_then(|p| p.name.clone())
        .filter(|n| !n.trim().is_empty())
}

/// One scoreboard row.
///
/// Where a Statlocker PP lookup for a player currently stands.
///
/// Five outcomes rather than one nullable rating, because they call for five different
/// things on screen. In particular `absent` - Statlocker has never seen this account -
/// is not `unavailable`, and neither is a score of zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StatlockerPpState {
    /// Nothing was asked and nothing will be; see [`StatlockerPpView::reason`].
    Unavailable,
    /// Asked for, no answer yet. An answer is expected on a later frame.
    Pending,
    /// Statlocker answered with a PP rating.
    Known,
    /// Statlocker answered, and holds no PP rating for this account.
    Absent,
    /// The request failed; see [`StatlockerPpView::reason`].
    Failed,
}

/// A player's Statlocker PP rating, or the reason there is not one.
///
/// PP - "performance points" - is Statlocker's own ladder, from before Valve shipped
/// ranked matchmaking. It is a different measurement from [`PlayerView::rank`], which is
/// the game's own badge read out of memory, and the two are free to disagree.
///
/// Flat rather than a tagged union so the window can read `state` and then the fields
/// that state fills in. Every field other than `state` is `None` unless that state has
/// something to put in it, and `None` here never means "zero".
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatlockerPpView {
    /// Which of the five outcomes this is. Read this first.
    pub state: StatlockerPpState,
    /// The performance-points score itself, which is the reported value. `None` unless
    /// `state` is `known`.
    pub score: Option<f64>,
    /// The badge that score draws as, packed and rendered as `tier-subrank`.
    pub rank: Option<String>,
    /// Statlocker's own name for that tier, which is **not** one of the game's current
    /// tier names - its ladder kept the pre-matchmaking ones.
    pub tier_name: Option<String>,
    /// **Old-era** badge art for `rank`, or `None` when there is no badge to draw.
    ///
    /// The era of the artwork is what identifies the source, so this is always the
    /// pre-matchmaking art and never the game's current badge set. `None` here is not a
    /// blank cell: read `state` to find out whether that is "no rating", "not looked up"
    /// or "the request failed", and draw a deliberate placeholder for each.
    pub art: Option<String>,
    /// Matches played towards calibration, when the answer carried a count.
    pub calibration_matches: Option<u32>,
    /// Whether Statlocker considers the rating settled.
    ///
    /// `false` means it is still calibrating and Statlocker itself would not draw the
    /// badge as a rank yet. Always `true` when `state` is not `known`, which the window
    /// must not read as a claim about anything.
    pub calibrated: bool,
    /// Why, for `failed` and `unavailable`. `None` for the other three.
    pub reason: Option<String>,
}

impl Default for StatlockerPpView {
    /// What a row carries when no lookup has been attached to the view at all.
    ///
    /// Deliberately not `pending`: nothing is coming, and a spinner that never resolves
    /// is worse than an honest "not available".
    fn default() -> Self {
        StatlockerPpView {
            state: StatlockerPpState::Unavailable,
            score: None,
            rank: None,
            tier_name: None,
            art: None,
            calibration_matches: None,
            calibrated: true,
            reason: Some("no Statlocker lookup attached to this view".into()),
        }
    }
}

/// Every counter below is flattened to a number for the window to paint, so a field the
/// reader could not read arrives as `0` and is indistinguishable from a real zero. That is
/// the right trade for a panel refreshed several times a second - a row that blanks out
/// mid-match reads as a bug - but it means nothing here should be treated as authoritative
/// for anything other than display.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerView {
    /// Lobby slot, or `None` before one is assigned.
    pub slot: Option<u32>,
    /// Team number as the game numbers them; `0` when it could not be read, which is not a
    /// team any player is ever on.
    pub team: u32,
    /// Display name, already falling back to something printable.
    pub name: String,
    /// Hero display name, or `"-"` when the id is unknown to the catalog.
    pub hero: String,
    pub hero_id: Option<u32>,
    /// Scoreboard portrait URL for this row's hero.
    ///
    /// `None` covers two different facts - the hero id did not read, and the catalog
    /// publishes no art for it - and the window tells them apart by `hero_id`. Neither is
    /// a blank cell: an unidentified hero draws as unknown, a known one without art draws
    /// as its initials.
    pub hero_portrait: Option<String>,
    /// Hero card art URL, for the places with more room than a row.
    pub hero_card: Option<String>,
    pub level: Option<u32>,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub hero_damage: u32,
    pub objective_damage: u32,
    pub healing: u32,
    pub net_worth: u32,
    /// Current health. Signed because the game's own field is; below zero means dead and
    /// still being counted down.
    pub health: i32,
    pub max_health: i32,
    /// The account behind this row, 32-bit rather than Steam64.
    ///
    /// `None` when the id did not read, or when it is not an individual account - a bot
    /// slot reads a Steam64 that has no account id at all. Narrow on purpose: a Steam64
    /// exceeds what a JSON number survives, and the window would silently round it.
    pub account_id: Option<u32>,
    /// Packed rank badge, rendered as `tier-subrank`; empty when unranked.
    ///
    /// Empty covers both "unranked" and "the field did not read"; [`PlayerView::rank_read`]
    /// is what tells those apart. Never draw an empty string as unranked on its own.
    pub rank: String,
    /// Whether [`PlayerView::rank`] is a measurement rather than a gap.
    ///
    /// `false` means `m_unPackedRank` did not resolve, which is a different fact from a
    /// player who has no rank yet. Both leave `rank` empty, and only this separates them.
    pub rank_read: bool,
    /// **New-era** badge art for [`PlayerView::rank`], or `None` when there is no badge.
    ///
    /// `None` covers three different facts - the field did not read, the player is
    /// unranked, and the badge sits off the ladder - and [`PlayerView::rank_read`] and
    /// [`PlayerView::rank`] tell them apart. None of them is a blank cell.
    ///
    /// Always the game's current badge set, never the old one: the era of the artwork is
    /// what tells a viewer this badge is the game's own rank and not the Statlocker PP
    /// rating beside it, so it must never fall back to the other era's art.
    pub rank_art: Option<String>,
    /// This player's PP rating according to Statlocker, which is a separate ladder from
    /// the game's own badge above and can disagree with it.
    ///
    /// Its own five-state type because "not fetched yet", "Statlocker has never seen this
    /// player" and "the request fell over" are three different things and must not paint
    /// as one blank.
    pub statlocker_pp: StatlockerPpView,
    /// Falls back to inferring from health when the game does not say directly.
    pub alive: bool,
    /// The account running this client.
    pub is_local: bool,
    /// Whoever the camera is on, which is only the local player when not spectating.
    pub is_observed: bool,
    /// The row the client is effectively *being* right now - you, or who you are watching.
    ///
    /// Neither [`PlayerView::is_local`] nor [`PlayerView::is_observed`] answers that on its
    /// own. While spectating, `is_local` is the client's observer controller, which sits on
    /// [`Team::SPECTATOR`] and is not a scoreboard row at all; while dead, `is_observed`
    /// wanders onto whichever teammate the dead-cam picked. `LiveSnapshot::current_player`
    /// resolves the two, and this is that resolution carried per row.
    ///
    /// At most one row has this. **No** row has it when the reader could not identify
    /// anyone - see [`MatchView::current_identified`], which is what stops "nobody is
    /// highlighted" from being mistaken for a broken highlight.
    pub is_current: bool,
    pub abandoned: bool,
    /// Whether this row's summed statistics actually read.
    ///
    /// `false` means every number below is a placeholder zero, not a result. They fail
    /// together: `m_PlayerDataGlobal` failing to resolve drops all of a player's
    /// statistics at once, which is why this is one flag rather than an `Option` on each.
    /// The same reasoning is on `TeamStats::complete`, which is the side-level version of
    /// the same fact.
    pub stats_read: bool,
}

impl PlayerView {
    fn from_row(p: &PlayerRow, heroes: &HeroCatalog, is_current: bool) -> Self {
        let rank = p
            .packed_rank
            .map(RankBadge)
            .filter(deadlock_core::RankBadge::is_ranked)
            .map(|b| b.to_string())
            .unwrap_or_default();
        PlayerView {
            slot: p.slot,
            team: p.team.map(|t| t.get()).unwrap_or(0),
            name: p.display_name(),
            hero: p
                .hero_id
                .map(|h| heroes.display_name(h))
                .unwrap_or_else(|| "-".into()),
            hero_id: p.hero_id.map(|h| h.0),
            hero_portrait: p
                .hero_id
                .and_then(|h| heroes.portrait(h))
                .map(str::to_owned),
            hero_card: p.hero_id.and_then(|h| heroes.card(h)).map(str::to_owned),
            level: p.level,
            kills: p.kills.unwrap_or(0),
            deaths: p.deaths.unwrap_or(0),
            assists: p.assists.unwrap_or(0),
            hero_damage: p.hero_damage.unwrap_or(0),
            objective_damage: p.objective_damage.unwrap_or(0),
            healing: p.healing.unwrap_or(0),
            net_worth: p.net_worth.unwrap_or(0),
            health: p.health.unwrap_or(0),
            max_health: p.max_health.unwrap_or(0),
            account_id: p
                .steam_id
                .map(deadlock_core::SteamId)
                .and_then(|s| s.to_account_id())
                .map(|a| a.0),
            rank,
            rank_read: p.packed_rank.is_some(),
            rank_art: p
                .packed_rank
                .map(RankBadge)
                .and_then(crate::rank_art::new_era_art),
            // A view built from a snapshot alone has no lookup behind it;
            // `MatchView::with_statlocker_pp` is what fills these in.
            statlocker_pp: StatlockerPpView::default(),
            alive: p.is_alive.unwrap_or_else(|| p.alive()),
            is_local: p.is_local.unwrap_or(false),
            is_observed: p.is_observed,
            is_current,
            abandoned: p.has_abandoned(),
            // Any one of them reading means the block resolved; they do not fail
            // individually.
            stats_read: p.kills.is_some()
                || p.deaths.is_some()
                || p.assists.is_some()
                || p.hero_damage.is_some()
                || p.objective_damage.is_some()
                || p.healing.is_some()
                || p.net_worth.is_some(),
        }
    }
}

/// Both sides summed, which is the half of the panel read at a glance.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamView {
    pub team: u32,
    /// Side name as shown, not the raw team number.
    pub name: String,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub hero_damage: u32,
    pub objective_damage: u32,
    pub healing: u32,
    pub souls: u32,
    /// How many players are on this side - a count, unlike `MatchView::players`, which is
    /// the rows themselves.
    pub players: u32,
    /// Souls ahead of the other side; negative when behind.
    pub soul_lead: i64,
    /// Whether every counted player reported every summed statistic.
    ///
    /// `false` means the totals above are **understated**, not that the side has none. The
    /// usual cause is `m_PlayerDataGlobal` failing to resolve, which drops all of one
    /// player's statistics at once; without this the resulting zeros look like a side that
    /// genuinely scored nothing. See `TeamStats::complete`, where the same reasoning lives.
    pub complete: bool,
}

impl TeamView {
    fn from_stats(t: &TeamStats, name: &str, other_souls: u32) -> Self {
        TeamView {
            team: t.team.get(),
            name: name.to_string(),
            kills: t.kills,
            deaths: t.deaths,
            assists: t.assists,
            hero_damage: t.hero_damage,
            objective_damage: t.objective_damage,
            healing: t.healing,
            souls: t.souls,
            players: t.players,
            soul_lead: t.souls as i64 - other_souls as i64,
            complete: t.complete,
        }
    }
}

/// The objective timers, formatted for display.
///
/// Strings rather than numbers, because the window should not own the rule for what an
/// absent timer looks like. That distinction carries real weight here: most of these are
/// absent because the client is *never told them*, which is a different fact from a timer
/// having run out, and a panel that renders both as `0:00` invites the reader to act on a
/// countdown that does not exist.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectivesView {
    /// Midboss health as `19240/19240`, or `None` when it is not on the map.
    pub midboss_health: Option<String>,
    /// Midboss health as a fraction, for a bar.
    pub midboss_fraction: Option<f32>,
    /// Time until the Midboss respawns, `m:ss`.
    pub midboss_respawn: Option<String>,
    /// Whether a rift is being contested right now.
    pub rift_contested: bool,
    /// Furthest capture progress any side has, `0.0..=1.0`.
    pub rift_progress: Option<f32>,
    /// Time until the current rift attempt lapses, `m:ss`.
    pub rift_give_up: Option<String>,
    /// Urn pickups on the map.
    pub urn_count: usize,
    /// Time until the next bridge-buff spawn, `m:ss`.
    ///
    /// Schedule-derived rather than read from the game, and the only objective timer here
    /// that is. See `deadlock_reader::timers`.
    pub bridge_buff: Option<String>,
    /// Street Brawl phase, when that is the mode.
    pub brawl_phase: Option<String>,
    /// Time until the Street Brawl phase ends, `m:ss`.
    pub brawl_next_phase: Option<String>,
    /// Street Brawl round number.
    pub brawl_round: Option<i32>,
}

/// `m:ss`, or `None` when there is no answer to give.
fn mmss(secs: Option<f32>) -> Option<String> {
    let s = secs.filter(|s| s.is_finite() && *s >= 0.0)? as u32;
    Some(format!("{}:{:02}", s / 60, s % 60))
}

impl ObjectivesView {
    /// Build from a snapshot.
    fn from_snapshot(s: &LiveSnapshot) -> Self {
        let t = &s.timers;
        let now = s.clock.now;
        let m = &t.midboss_state;
        ObjectivesView {
            midboss_health: match (m.health, m.max_health) {
                (Some(h), Some(max)) => Some(format!("{h}/{max}")),
                _ => None,
            },
            midboss_fraction: m.health_fraction(),
            midboss_respawn: t.midboss.as_ref().and_then(|o| mmss(o.next_spawn_in)),
            rift_contested: t.rift_state.contested(now),
            rift_progress: t.rift_state.best_progress(),
            rift_give_up: mmss(t.rift_state.seconds_until_give_up(now)),
            urn_count: t.urn.as_ref().and_then(|o| o.count).unwrap_or(0),
            bridge_buff: t.bridge_buffs.as_ref().and_then(|o| mmss(o.next_spawn_in)),
            brawl_phase: s
                .street_brawl
                .as_ref()
                .filter(|_| s.is_street_brawl())
                .and_then(|b| b.state_name.clone()),
            brawl_next_phase: s
                .street_brawl
                .as_ref()
                .filter(|_| s.is_street_brawl())
                .and_then(|b| now.and_then(|n| mmss(b.seconds_until_next_state(n)))),
            brawl_round: s
                .street_brawl
                .as_ref()
                .filter(|_| s.is_street_brawl())
                .and_then(|b| b.round),
        }
    }
}

/// One frame of match state.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchView {
    /// Whether a client is attached at all.
    pub attached: bool,
    /// Whether that client is in a readable match.
    pub in_match: bool,
    /// One line of context, e.g. `"Ranked / Normal"` or `"Hideout"`.
    pub status: String,
    pub match_id: Option<u64>,
    /// Already formatted for display, because the window should not own the rule for what
    /// a paused or not-yet-started clock looks like.
    pub clock: Option<String>,
    pub teams: Vec<TeamView>,
    /// Objective timers and health.
    pub objectives: ObjectivesView,
    /// Every row, both sides, in scoreboard order.
    pub players: Vec<PlayerView>,
    /// Heroes banned this match, named and with art, in the order the game lists them.
    ///
    /// Empty when nothing is banned **and** when the ban vector could not be read; the two
    /// are told apart by [`MatchView::bans_readable`]. Most modes ban nothing, so an empty
    /// list is the common case rather than a sign of trouble.
    pub bans: Vec<HeroRef>,
    /// Whether [`MatchView::bans`] is a measurement rather than a gap.
    ///
    /// `false` means the vector did not read. A window must not draw that as "no bans" -
    /// during a draft it would look like the bans had been cleared.
    pub bans_readable: bool,
    /// Whether the reader identified whoever is on screen at all.
    ///
    /// `false` means free camera, a replay with no target, or a tick before anyone has
    /// spawned - and it is a different fact from "the current player is not on this side of
    /// the scoreboard". Without it, no row carrying [`PlayerView::is_current`] would be
    /// indistinguishable from a highlight that had stopped working, and the window would
    /// have no honest way to say which. Never guess a row instead.
    pub current_identified: bool,
    /// Hero swaps recent enough to still be worth announcing, oldest first.
    ///
    /// Not a per-frame diff: each entry is carried for [`SWAP_RETENTION`] and re-sent on
    /// every frame in that window, so a window that missed a frame still sees it. Act on
    /// [`HeroSwapView::seq`] rather than on the list changing - see [`SwapLog`].
    pub hero_swaps: Vec<HeroSwapView>,
    /// The winning side, once the match is actually decided.
    ///
    /// `None` until then. `m_iWinningTeam` carries a value before the match ends and it is
    /// meaningless, so this is gated on the game state rather than on the field looking
    /// plausible - the field's own pre-decision value is indistinguishable from a real one.
    pub winner: Option<u32>,
    /// Set when the reader could not be attached, for the window to explain itself.
    pub error: Option<String>,
}

impl MatchView {
    /// Nothing attached yet; the window shows this as a waiting state, not an error.
    pub fn detached(error: Option<String>) -> Self {
        MatchView {
            attached: false,
            status: "waiting for Deadlock".into(),
            error,
            ..Default::default()
        }
    }

    /// Attached, but not in a match the reader can see.
    pub fn idle(status: String) -> Self {
        MatchView {
            attached: true,
            in_match: false,
            status,
            ..Default::default()
        }
    }

    /// Attach the swaps a [`SwapLog`] is currently carrying.
    ///
    /// Separate from [`MatchView::from_snapshot`] because a swap is not visible in one
    /// snapshot: it only exists as a difference between two, which is state the view
    /// itself does not hold.
    #[must_use]
    pub fn with_swaps(mut self, swaps: Vec<HeroSwapView>) -> Self {
        self.hero_swaps = swaps;
        self
    }

    /// Fill in each row's Statlocker PP rating from a lookup.
    ///
    /// Takes a closure rather than the lookup itself so this file stays free of anything
    /// that can touch a socket: the caller decides what a lookup costs, and the contract
    /// here is only that it must answer without waiting - this runs on the poll loop.
    ///
    /// A row whose account id did not read is left at [`StatlockerPpView::default`],
    /// which is `unavailable` and says why. It is not guessed at from the persona name.
    #[must_use]
    pub fn with_statlocker_pp(mut self, mut lookup: impl FnMut(u32) -> StatlockerPpView) -> Self {
        for player in &mut self.players {
            if let Some(account) = player.account_id {
                player.statlocker_pp = lookup(account);
            }
        }
        self
    }

    pub fn from_snapshot(s: &LiveSnapshot, heroes: &HeroCatalog) -> Self {
        // A side the reader has not managed to read yet still gets a row, because the
        // window indexes both by position. `unwrap_or_default` would give it `team: 0`,
        // which is the value `PlayerView` uses to mean "could not be read" - so the row
        // would sit in the Amber slot while claiming not to know which side it was.
        let side = |team: Team| {
            s.teams
                .iter()
                .find(|t| t.team == team)
                .copied()
                .unwrap_or(TeamStats {
                    team,
                    ..Default::default()
                })
        };
        let amber = side(Team::AMBER);
        let sapphire = side(Team::SAPPHIRE);

        let teams = vec![
            TeamView::from_stats(&amber, s.team_name(Team::AMBER), sapphire.souls),
            TeamView::from_stats(&sapphire, s.team_name(Team::SAPPHIRE), amber.souls),
        ];

        // Matched on the controller address, which is unique for the life of a match, so
        // no row can be flagged by resembling the current player. `None` flags nothing.
        let current = s.current_player().map(|p| p.controller);

        MatchView {
            attached: true,
            in_match: true,
            status: s.describe(),
            match_id: s.match_id,
            clock: s.timers.display().or_else(|| s.clock.elapsed_display()),
            teams,
            objectives: ObjectivesView::from_snapshot(s),
            players: s
                .scoreboard()
                .map(|p| PlayerView::from_row(p, heroes, current == Some(p.controller)))
                .collect(),
            current_identified: current.is_some(),
            bans: s
                .banned_heroes
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|h| HeroRef::new(*h, heroes))
                .collect(),
            bans_readable: s.banned_heroes.is_some(),
            // A snapshot on its own cannot see a swap; `with_swaps` attaches them.
            hero_swaps: Vec::new(),
            // Gated on the state, not on the value: the field carries something before
            // the match ends and it means nothing.
            winner: s
                .winning_team
                .filter(|_| s.game_state.is_some_and(|g| g.is_over()))
                .map(|t| t.0),
            error: None,
        }
    }
}

/// One party member, from the Game Coordinator rather than the entity system.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyMemberView {
    pub account_id: u32,
    pub name: String,
    pub rank: String,
    pub ready: Option<bool>,
    pub is_creator: bool,
    /// Heroes queued as, most wanted first, with whatever art the catalog knows.
    pub queued_as: Vec<HeroRef>,
}

/// Party and queue state, which is readable outside a match as well as inside one.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyView {
    pub party_id: Option<u64>,
    pub join_code: Option<String>,
    pub queueing: bool,
    /// e.g. `"Unranked / Normal"`, empty when not queueing.
    pub queue: String,
    /// Seconds since matchmaking began. May span more than one queue attempt.
    pub matchmaking_seconds: Option<u64>,
    pub region: String,
    pub preference: String,
    pub members: Vec<PartyMemberView>,
    /// Set when members disagree on the game build, which silently blocks queueing.
    pub build_mismatch: bool,
}

/// Resolve hero ids to names and art for a member's queue roster.
pub fn roster_names(
    m: &valveprotos::deadlock::cso_citadel_party::Member,
    heroes: &HeroCatalog,
) -> Vec<HeroRef> {
    m.hero_roster
        .as_ref()
        .map(|r| {
            r.by_priority()
                .into_iter()
                .filter_map(|h| h.hero_id)
                .map(|id| HeroRef::new(HeroId(id), heroes))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(slot: u32, team: Team) -> PlayerRow {
        PlayerRow {
            slot: Some(slot),
            team: Some(team),
            hero_id: Some(HeroId(1)),
            kills: Some(3),
            deaths: Some(1),
            assists: Some(2),
            health: Some(500),
            max_health: Some(700),
            ..Default::default()
        }
    }

    /// A snapshot carrying just the teams, which is all these assertions look at.
    fn snap_with(teams: Vec<TeamStats>) -> LiveSnapshot {
        LiveSnapshot {
            teams,
            ..Default::default()
        }
    }

    fn stats(team: Team, souls: u32) -> TeamStats {
        TeamStats {
            team,
            souls,
            ..Default::default()
        }
    }

    /// The window indexes `teams[0]` and `teams[1]` directly, so both sides have to be
    /// present in a fixed order whatever the snapshot contains. A reader that has only
    /// managed to read one team - or neither, early in a match - must still produce two.
    #[test]
    fn both_sides_are_always_present_in_a_fixed_order() {
        let s = LiveSnapshot::default();
        let v = MatchView::from_snapshot(&s, &HeroCatalog::bundled());
        assert_eq!(v.teams.len(), 2, "an empty snapshot still has two sides");
        assert_eq!(v.teams[0].team, Team::AMBER.get());
        assert_eq!(v.teams[1].team, Team::SAPPHIRE.get());

        let s = snap_with(vec![
            stats(Team::SAPPHIRE, 30_000),
            stats(Team::AMBER, 20_000),
        ]);
        let v = MatchView::from_snapshot(&s, &HeroCatalog::bundled());
        assert_eq!(v.teams[0].team, Team::AMBER.get());
        assert_eq!(v.teams[0].souls, 20_000);
        assert_eq!(v.teams[1].souls, 30_000);
    }

    /// `soul_lead` is the one piece of arithmetic in this module. It is signed on purpose:
    /// the losing side's lead is negative, and computing it in `u32` would wrap to about
    /// four billion, which the panel would render as a colossal lead for whoever is behind.
    #[test]
    fn the_trailing_side_gets_a_negative_soul_lead() {
        let s = snap_with(vec![
            stats(Team::AMBER, 20_000),
            stats(Team::SAPPHIRE, 30_000),
        ]);
        let v = MatchView::from_snapshot(&s, &HeroCatalog::bundled());

        assert_eq!(v.teams[0].soul_lead, -10_000, "Amber is behind");
        assert_eq!(v.teams[1].soul_lead, 10_000, "Sapphire is ahead");
        assert_eq!(
            v.teams[0].soul_lead + v.teams[1].soul_lead,
            0,
            "the two leads are the same number with opposite signs"
        );
    }

    /// Every counter is flattened to a number for the window to paint, so a field the
    /// reader could not read arrives as `0`. That is deliberate - a row that blanks out
    /// mid-match reads as a bug - and it is worth pinning, because it is also the reason
    /// nothing in a `PlayerView` may be treated as authoritative.
    #[test]
    fn unreadable_fields_flatten_to_zero_rather_than_disappearing() {
        let mut p = player(1, Team::AMBER);
        p.kills = None;
        p.net_worth = None;
        p.team = None;
        p.hero_id = None;

        let v = PlayerView::from_row(&p, &HeroCatalog::bundled(), false);
        assert_eq!(v.kills, 0);
        assert_eq!(v.net_worth, 0);
        assert_eq!(v.team, 0, "0 is not a team any player is ever on");
        assert_eq!(
            v.hero, "-",
            "an unknown hero shows a dash, not an empty cell"
        );
        assert_eq!(v.rank, "", "no packed rank means unranked, not rank 0-0");
    }

    /// An unread Valve rank and an unranked player both leave `rank` empty, and only
    /// `rank_read` separates them.
    ///
    /// The empty string was doing two jobs: `m_unPackedRank` failing to resolve and a
    /// player with no badge yet produced the same payload, so the window had no way to
    /// avoid drawing a gap in the reader as "unranked".
    #[test]
    fn an_unread_valve_rank_is_not_an_unranked_player() {
        let heroes = HeroCatalog::bundled();

        let mut unread = player(1, Team::AMBER);
        unread.packed_rank = None;
        let unread = PlayerView::from_row(&unread, &heroes, false);
        assert_eq!(unread.rank, "");
        assert!(
            !unread.rank_read,
            "a field that did not resolve is not a rank"
        );

        let mut unranked = player(1, Team::AMBER);
        unranked.packed_rank = Some(0);
        let unranked = PlayerView::from_row(&unranked, &heroes, false);
        assert_eq!(unranked.rank, "", "badge 0 is unranked, not rank 0-0");
        assert!(
            unranked.rank_read,
            "the game said unranked; that is a reading"
        );

        let mut ranked = player(1, Team::AMBER);
        ranked.packed_rank = Some(93);
        let ranked = PlayerView::from_row(&ranked, &heroes, false);
        assert_eq!(ranked.rank, "9-3");
        assert!(ranked.rank_read);

        assert_eq!(ranked.rank_art, crate::rank_art::new_era_art(RankBadge(93)));
        assert_ne!(
            ranked.rank_art,
            crate::rank_art::old_era_art(RankBadge(93)),
            "the Valve badge must never draw in the pre-matchmaking art"
        );
        assert_eq!(unread.rank_art, None);
        assert_eq!(unranked.rank_art, None);
    }

    /// The account id is narrowed from the Steam64, and never invented.
    ///
    /// A Steam64 does not survive a JSON number, so the window gets the 32-bit id the
    /// lookups key on. Anything that is not an individual account has none, and `0` is
    /// not a stand-in for that.
    #[test]
    fn an_account_id_is_narrowed_or_absent() {
        let heroes = HeroCatalog::bundled();

        let mut real = player(1, Team::AMBER);
        real.steam_id = Some(76_561_198_347_512_100);
        assert_eq!(
            PlayerView::from_row(&real, &heroes, false).account_id,
            Some(387_246_372)
        );

        let mut unread = player(1, Team::AMBER);
        unread.steam_id = None;
        assert_eq!(
            PlayerView::from_row(&unread, &heroes, false).account_id,
            None
        );

        let mut bot = player(1, Team::AMBER);
        bot.steam_id = Some(0);
        assert_eq!(
            PlayerView::from_row(&bot, &heroes, false).account_id,
            None,
            "an id with no account behind it must not narrow to 0"
        );
    }

    /// Statlocker PP ratings reach only the rows that have an account to look up.
    ///
    /// A row whose id did not read stays at the default, which says it is unavailable
    /// rather than pending - nothing is coming for it.
    #[test]
    fn statlocker_pp_lands_on_identified_rows_only() {
        let heroes = HeroCatalog::bundled();
        let mut named = player(1, Team::AMBER);
        named.steam_id = Some(76_561_197_960_265_729);
        let mut nameless = player(2, Team::SAPPHIRE);
        nameless.steam_id = None;

        let s = LiveSnapshot {
            players: vec![named, nameless],
            ..Default::default()
        };
        let mut asked: Vec<u32> = Vec::new();
        let view = MatchView::from_snapshot(&s, &heroes).with_statlocker_pp(|account| {
            asked.push(account);
            StatlockerPpView {
                state: StatlockerPpState::Known,
                score: Some(4950.0),
                rank: Some("9-3".into()),
                tier_name: Some("Phantom".into()),
                art: crate::rank_art::old_era_art(RankBadge(93)),
                calibration_matches: None,
                calibrated: true,
                reason: None,
            }
        });

        assert_eq!(asked, vec![1], "only the identified row is looked up");
        let by_slot = |slot: u32| {
            view.players
                .iter()
                .find(|p| p.slot == Some(slot))
                .expect("a row")
                .statlocker_pp
                .clone()
        };
        assert_eq!(by_slot(1).state, StatlockerPpState::Known);
        assert_eq!(by_slot(1).rank.as_deref(), Some("9-3"));
        assert!(by_slot(1).art.is_some(), "a known PP rating draws a badge");

        let missing = by_slot(2);
        assert_eq!(
            missing.state,
            StatlockerPpState::Unavailable,
            "an unidentified row must not read as pending forever"
        );
        assert_eq!(missing.rank, None);
        assert_eq!(
            missing.art, None,
            "no rating means no badge, not a blank one"
        );
        assert!(missing.reason.is_some(), "unavailable has to say why");
    }

    /// Only players are scoreboard rows. A spectator has a slot field that reads as 1 and
    /// collides with the first Amber player, so letting one through would show a duplicate.
    #[test]
    fn spectators_are_not_scoreboard_rows() {
        let mut spec = player(1, Team::SPECTATOR);
        spec.is_spectator = true;
        let s = LiveSnapshot {
            players: vec![player(1, Team::AMBER), spec, player(2, Team::SAPPHIRE)],
            ..Default::default()
        };

        let v = MatchView::from_snapshot(&s, &HeroCatalog::bundled());
        assert_eq!(v.players.len(), 2);
        assert!(v.players.iter().all(|p| p.team != Team::SPECTATOR.get()));
    }

    /// The window tells three states apart by these two flags alone, and getting them the
    /// wrong way round is the difference between "the game is not running" and "you are in
    /// the Hideout".
    #[test]
    fn the_three_window_states_are_distinguishable() {
        let detached = MatchView::detached(Some("denied".into()));
        assert!(!detached.attached && !detached.in_match);
        assert_eq!(detached.error.as_deref(), Some("denied"));

        let idle = MatchView::idle("Hideout".into());
        assert!(idle.attached && !idle.in_match);
        assert!(idle.error.is_none(), "not being in a match is not an error");

        let live = MatchView::from_snapshot(&LiveSnapshot::default(), &HeroCatalog::bundled());
        assert!(live.attached && live.in_match);
        assert!(live.error.is_none());
    }

    /// A member with no roster is not queued as anything. `None` and an empty list mean the
    /// same thing to the window, and both have to arrive as an empty list rather than as a
    /// panic or a placeholder entry.
    #[test]
    fn a_member_with_no_roster_queues_as_nothing() {
        let m = valveprotos::deadlock::cso_citadel_party::Member::default();
        assert!(roster_names(&m, &HeroCatalog::bundled()).is_empty());
    }

    /// Banned heroes reach the window as names, and an empty ban list is not a failed read.
    ///
    /// `m_vecBannedHeroes` is on the networked rules class, so a draft is readable while it
    /// is happening rather than only afterwards. Most modes ban nothing, so `Some(vec![])`
    /// and `None` are different facts: the first is "no bans", the second is "the vector
    /// could not be read", and a window that drew both as an empty list would tell a viewer
    /// a draft had been cleared when the read had merely failed.
    #[test]
    fn bans_arrive_as_names_and_absence_is_not_emptiness() {
        let heroes = HeroCatalog::bundled();

        let s = LiveSnapshot {
            banned_heroes: Some(vec![HeroId(1), HeroId(2)]),
            ..Default::default()
        };
        let v = MatchView::from_snapshot(&s, &heroes);
        let named: Vec<&str> = v.bans.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(named, ["Infernus", "Seven"]);
        assert!(v.bans_readable);

        let none = LiveSnapshot {
            banned_heroes: None,
            ..Default::default()
        };
        let v = MatchView::from_snapshot(&none, &heroes);
        assert!(v.bans.is_empty());
        assert!(
            !v.bans_readable,
            "an unread ban vector must not look like no bans"
        );

        let empty = LiveSnapshot {
            banned_heroes: Some(Vec::new()),
            ..Default::default()
        };
        let v = MatchView::from_snapshot(&empty, &heroes);
        assert!(v.bans.is_empty());
        assert!(
            v.bans_readable,
            "a mode that bans nothing did read successfully"
        );
    }

    /// Hero art reaches the window as a URL, and its absence is a distinct fact.
    ///
    /// The overlay never fetches anything itself; it forwards what the catalog publishes
    /// and lets the window decide whether to try. Two different absences share one `None`
    /// here - the hero id did not read, and the catalog has no art for it - so the window
    /// leans on `hero_id` to tell them apart. Pinned because a row that silently drops to
    /// a blank square looks identical to a row whose hero never resolved.
    #[test]
    fn hero_art_travels_with_the_row_and_absence_is_not_emptiness() {
        let heroes = HeroCatalog::bundled();

        let known = PlayerView::from_row(&player(1, Team::AMBER), &heroes, false);
        assert_eq!(known.hero_id, Some(1));
        assert!(
            known.hero_portrait.is_some(),
            "the bundled snapshot publishes portraits; hero 1 arrived without one"
        );

        let mut unknown = player(1, Team::AMBER);
        unknown.hero_id = None;
        let v = PlayerView::from_row(&unknown, &heroes, false);
        assert_eq!(v.hero_id, None);
        assert_eq!(
            v.hero_portrait, None,
            "an unread hero has no art to publish, and must not borrow anyone else's"
        );
        assert_eq!(v.hero_card, None);
    }

    /// The winner is only reported once the match is actually over.
    ///
    /// `m_iWinningTeam` is meaningless before that, and a window that drew it early would
    /// announce a winner mid-match. Gated on the game state rather than on the field being
    /// non-zero, because the field's own default is indistinguishable from a real answer.
    #[test]
    fn no_winner_is_announced_before_the_match_ends() {
        use deadlock_core::GameState;

        let heroes = HeroCatalog::bundled();
        let mut s = LiveSnapshot {
            winning_team: Some(Team::AMBER),
            game_state: Some(GameState::GameInProgress),
            ..Default::default()
        };
        assert_eq!(MatchView::from_snapshot(&s, &heroes).winner, None);

        s.game_state = Some(GameState::PostGame);
        assert_eq!(
            MatchView::from_snapshot(&s, &heroes).winner,
            Some(Team::AMBER.0)
        );
    }

    /// Every key the Rust view serialises is declared in `types.ts`.
    ///
    /// `types.ts` opens by claiming a change on the Rust side "shows up as a type error
    /// here instead of as an undefined at runtime". Nothing enforced that. A field added to
    /// `MatchView` and forgotten in `types.ts` compiles on both sides and arrives in the
    /// window as `undefined`, which renders as an empty cell rather than as an error - the
    /// exact failure the comment promises does not happen.
    ///
    /// Serialises a real value rather than reading the struct definition, so it checks the
    /// names the window actually receives, `serde(rename_all = "camelCase")` included.
    #[test]
    fn the_typescript_view_declares_every_key_the_rust_view_sends() {
        /// The body of one `export interface`, so a same-named key on another view cannot
        /// stand in for a missing one here.
        fn interface<'a>(text: &'a str, name: &str) -> &'a str {
            let head = String::from("export interface ") + name + " {";
            let start = text
                .find(&head)
                .unwrap_or_else(|| panic!("no {name} interface in types.ts"));
            &text[start..start + text[start..].find('}').expect("unterminated")]
        }

        /// Every key of a serialised value is declared in its interface.
        ///
        /// `{key}:`, not the bare key: `bans` is a substring of `bansReadable`, so a plain
        /// `contains` reported a declared field when only its neighbour survived.
        fn check(value: &serde_json::Value, block: &str, name: &str, least: usize) {
            let keys: Vec<&String> = value.as_object().expect("an object").keys().collect();
            assert!(keys.len() >= least, "{name} has only {} keys", keys.len());
            let missing: Vec<&&String> = keys
                .iter()
                .filter(|k| !block.contains(&(k.as_str().to_owned() + ":")))
                .collect();
            assert!(
                missing.is_empty(),
                "{name} sends keys types.ts does not declare: {missing:?}"
            );
        }

        let ts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("src")
            .join("lib")
            .join("types.ts");
        let text =
            std::fs::read_to_string(&ts).unwrap_or_else(|e| panic!("read {}: {e}", ts.display()));

        let s = LiveSnapshot {
            teams: vec![TeamStats {
                team: Team::AMBER,
                ..Default::default()
            }],
            players: vec![PlayerRow {
                slot: Some(1),
                team: Some(Team::AMBER),
                ..Default::default()
            }],
            ..Default::default()
        };
        let heroes = HeroCatalog::bundled();
        let view = MatchView::from_snapshot(&s, &heroes).with_swaps(vec![HeroSwapView {
            seq: 1,
            slot: Some(3),
            player: Some("Vex".into()),
            from: HeroRef::new(HeroId(1), &heroes),
            to: HeroRef::new(HeroId(2), &heroes),
        }]);
        let json = serde_json::to_value(&view).expect("serialise");

        check(&json, interface(&text, "MatchView"), "MatchView", 9);
        check(
            &json["heroSwaps"][0],
            interface(&text, "HeroSwapView"),
            "HeroSwapView",
            5,
        );
        check(
            &json["teams"][0],
            interface(&text, "TeamView"),
            "TeamView",
            10,
        );
        check(
            &json["players"][0],
            interface(&text, "PlayerView"),
            "PlayerView",
            10,
        );
        check(
            &json["objectives"],
            interface(&text, "ObjectivesView"),
            "ObjectivesView",
            8,
        );
        check(
            &json["players"][0]["statlockerPp"],
            interface(&text, "StatlockerPpView"),
            "StatlockerPpView",
            8,
        );
    }

    /// A reported winner always has a team row the window can name it from.
    ///
    /// The bar renders the winning side by looking its number up in `teams`, so a winner
    /// with no matching row would fall back to `team 3` in front of a viewer. `MatchView`
    /// emits both sides unconditionally - a side that did not read still gets a row - and
    /// this is what keeps the two facts tied together. The scoreboard is readable in every
    /// post-game state (see `GameState::is_over`), so a decided match has real rows.
    #[test]
    fn a_winner_can_always_be_named_from_the_team_rows() {
        use deadlock_core::GameState;

        for team in [Team::AMBER, Team::SAPPHIRE] {
            let s = LiveSnapshot {
                winning_team: Some(team),
                game_state: Some(GameState::PostGame),
                ..Default::default()
            };
            let v = MatchView::from_snapshot(&s, &HeroCatalog::bundled());
            assert_eq!(v.winner, Some(team.0));
            assert!(
                v.teams.iter().any(|t| t.team == team.0),
                "no team row for the winning side {team:?}: {:?}",
                v.teams.iter().map(|t| t.team).collect::<Vec<_>>()
            );
        }
    }

    /// Understated team totals reach the window labelled as understated.
    ///
    /// `TeamStats::complete` exists because `m_PlayerDataGlobal` failing to resolve drops
    /// all of a player's statistics at once, and the resulting zeros are indistinguishable
    /// from a side that genuinely has none — that reasoning is written on the field itself.
    /// `TeamView` dropped the flag on the way to the window, so the overlay drew understated
    /// numbers as real ones, which is the exact failure the flag was added to prevent.
    ///
    /// A viewer cannot be expected to guess. The window can choose how to show it; what it
    /// cannot do is not be told.
    #[test]
    fn a_side_whose_totals_are_understated_says_so() {
        let heroes = HeroCatalog::bundled();

        let s = LiveSnapshot {
            teams: vec![TeamStats {
                team: Team::AMBER,
                kills: 12,
                complete: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        let v = MatchView::from_snapshot(&s, &heroes);
        let amber = v
            .teams
            .iter()
            .find(|t| t.team == Team::AMBER.get())
            .expect("a row for amber");
        assert!(!amber.complete, "an understated side reported as complete");

        let ok = LiveSnapshot {
            teams: vec![TeamStats {
                team: Team::AMBER,
                kills: 12,
                complete: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let v = MatchView::from_snapshot(&ok, &heroes);
        assert!(v.teams.iter().any(|t| t.complete));
    }

    /// A player whose statistics did not read is not a player who did nothing.
    ///
    /// Every numeric field on `PlayerView` is `unwrap_or(0)`, so a row whose
    /// `m_PlayerDataGlobal` failed to resolve renders as `0/0/0` with no damage — exactly
    /// what a player who has done nothing looks like. `dlrs` enforces "a dash, never a
    /// zero" with a test of its own; the window did not.
    ///
    /// A single flag rather than nine `Option`s, because that is how the failure actually
    /// happens: `m_PlayerDataGlobal` failing drops **all** of a player's statistics at
    /// once, which is the reasoning already written on `TeamStats::complete`. Nine
    /// independent absences would model a failure mode the reader does not have.
    #[test]
    fn a_row_whose_statistics_did_not_read_says_so() {
        let heroes = HeroCatalog::bundled();

        let read = PlayerRow {
            slot: Some(1),
            kills: Some(0),
            deaths: Some(0),
            net_worth: Some(0),
            ..Default::default()
        };
        assert!(
            PlayerView::from_row(&read, &heroes, false).stats_read,
            "zeros that were actually read are a result"
        );

        let blind = PlayerRow {
            slot: Some(1),
            kills: None,
            deaths: None,
            assists: None,
            hero_damage: None,
            objective_damage: None,
            healing: None,
            net_worth: None,
            ..Default::default()
        };
        let v = PlayerView::from_row(&blind, &heroes, false);
        assert!(!v.stats_read, "an unread row reported as read");
        assert_eq!(v.kills, 0, "the number itself still renders as zero");
    }

    /// Liveness prefers the game's own flag and falls back to health, in that order.
    ///
    /// `m_lifeState` is authoritative; `PlayerRow::alive()` is `health > 0`, which is only
    /// a stand-in. Nothing pinned the chain, so replacing it with `unwrap_or(true)` or
    /// `unwrap_or(false)` passed — and either would be visible to a viewer as every player
    /// alive, or every player greyed out, on a tick where the flag did not read.
    ///
    /// The last case is the one that matters: a dead player with health still readable as
    /// positive must stay dead. Deferring to health there would resurrect them on screen.
    #[test]
    fn liveness_prefers_the_flag_and_falls_back_to_health() {
        let heroes = HeroCatalog::bundled();
        let view = |row: PlayerRow| PlayerView::from_row(&row, &heroes, false).alive;

        assert!(!view(PlayerRow {
            is_alive: Some(false),
            health: Some(900),
            ..Default::default()
        }));

        assert!(view(PlayerRow {
            is_alive: Some(true),
            health: Some(0),
            ..Default::default()
        }));

        assert!(view(PlayerRow {
            is_alive: None,
            health: Some(1),
            ..Default::default()
        }));
        assert!(!view(PlayerRow {
            is_alive: None,
            health: Some(0),
            ..Default::default()
        }));

        assert!(!view(PlayerRow {
            is_alive: None,
            health: None,
            ..Default::default()
        }));
    }

    /// A scoreboard row with the two flags `current_player` is resolved from.
    fn seat(controller: u64, slot: u32, team: Team, local: bool, observed: bool) -> PlayerRow {
        PlayerRow {
            controller,
            slot: Some(slot),
            team: Some(team),
            is_spectator: team == Team::SPECTATOR,
            is_local: Some(local),
            is_observed: observed,
            hero_id: Some(HeroId(1)),
            ..Default::default()
        }
    }

    /// Which rows the view says are on screen, by slot.
    fn current_slots(v: &MatchView) -> Vec<Option<u32>> {
        v.players
            .iter()
            .filter(|p| p.is_current)
            .map(|p| p.slot)
            .collect()
    }

    /// While playing, the row on screen is your own.
    ///
    /// `is_local` alone was not enough to say this: it is set on the client's controller
    /// whether or not that controller is playing, and while spectating that is an observer
    /// row that is not on the scoreboard at all.
    #[test]
    fn the_row_on_screen_while_playing_is_your_own() {
        let s = LiveSnapshot {
            players: vec![
                seat(0x10, 1, Team::AMBER, true, false),
                seat(0x20, 2, Team::SAPPHIRE, false, false),
            ],
            ..Default::default()
        };
        let v = MatchView::from_snapshot(&s, &HeroCatalog::bundled());
        assert!(v.current_identified);
        assert_eq!(current_slots(&v), vec![Some(1)], "exactly your row");
    }

    /// While spectating, the row on screen is whoever the camera is on.
    ///
    /// The client's own controller is a spectator with no hero and no scoreboard row, so
    /// highlighting `is_local` here would highlight nothing at all - which looks the same
    /// as a reader that could not tell, and is a different fact.
    #[test]
    fn the_row_on_screen_while_spectating_is_the_watched_player() {
        let s = LiveSnapshot {
            players: vec![
                seat(0x10, 1, Team::SPECTATOR, true, false),
                seat(0x20, 2, Team::AMBER, false, true),
                seat(0x30, 3, Team::SAPPHIRE, false, false),
            ],
            ..Default::default()
        };
        let v = MatchView::from_snapshot(&s, &HeroCatalog::bundled());
        assert!(v.current_identified);
        assert_eq!(current_slots(&v), vec![Some(2)], "the watched player");
        assert!(
            v.players.iter().all(|p| !p.is_local),
            "the spectator controller is not a scoreboard row"
        );
    }

    /// A camera on nobody highlights nobody, and says so.
    ///
    /// Free camera, a replay with no target, or a tick before anyone has spawned. The
    /// window must be able to tell that from "you are slot 1": highlighting an arbitrary
    /// row would be a claim the reader never made, and silently highlighting none is
    /// indistinguishable from the highlight being broken.
    #[test]
    fn a_camera_on_nobody_highlights_nobody_and_says_so() {
        let s = LiveSnapshot {
            players: vec![
                seat(0x10, 1, Team::AMBER, false, false),
                seat(0x20, 2, Team::SAPPHIRE, false, false),
            ],
            ..Default::default()
        };
        let v = MatchView::from_snapshot(&s, &HeroCatalog::bundled());
        assert!(
            !v.current_identified,
            "no current player must not be reported as one"
        );
        assert!(current_slots(&v).is_empty(), "and no row may be flagged");

        assert!(!MatchView::idle("Hideout".into()).current_identified);
        assert!(!MatchView::detached(None).current_identified);
    }

    /// Dead and watching a teammate: you are still the subject.
    ///
    /// `LiveSnapshot::current_player` draws this line and the view has to follow it, or the
    /// highlight would jump around the scoreboard every time the dead-cam moved.
    #[test]
    fn dead_cam_keeps_the_highlight_on_your_own_row() {
        let s = LiveSnapshot {
            players: vec![
                seat(0x10, 1, Team::AMBER, true, false),
                seat(0x20, 2, Team::AMBER, false, true),
            ],
            ..Default::default()
        };
        let v = MatchView::from_snapshot(&s, &HeroCatalog::bundled());
        assert_eq!(current_slots(&v), vec![Some(1)]);
    }

    /// A snapshot of one match with one player on one hero.
    fn swap_snap(hero: Option<u32>, slot: Option<u32>, name: Option<&str>) -> LiveSnapshot {
        LiveSnapshot {
            match_id: Some(42),
            players: vec![PlayerRow {
                controller: 0x1000,
                slot,
                team: Some(Team::AMBER),
                hero_id: hero.map(HeroId),
                name: name.map(str::to_owned),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// A swap reaches the window with both heroes named and the player identified.
    ///
    /// The whole popup is the two names and who swapped; a view that carried ids alone
    /// would leave the window inventing the words, which is the job this module exists to
    /// do.
    #[test]
    fn a_hero_swap_reaches_the_window_named_on_both_sides() {
        let heroes = HeroCatalog::bundled();
        let mut log = SwapLog::new();
        let t0 = Instant::now();

        assert!(
            log.observe_at(&swap_snap(Some(1), Some(3), Some("Vex")), &heroes, t0)
                .is_empty(),
            "the first tick is a baseline, not a swap"
        );

        let out = log.observe_at(&swap_snap(Some(2), Some(3), Some("Vex")), &heroes, t0);
        assert_eq!(out.len(), 1, "one swap, got {out:?}");
        assert_eq!(out[0].from.name, "Infernus");
        assert_eq!(out[0].to.name, "Seven");
        assert_eq!(out[0].slot, Some(3));
        assert_eq!(out[0].player.as_deref(), Some("Vex"));
    }

    /// Learning a player's hero is not a swap the window is told about.
    ///
    /// `EventTracker` already draws this line - see `Event::HeroChanged` - and the popup is
    /// the reason it matters: reporting `None -> Some` would put twelve toasts on screen the
    /// moment a match resolves. Pinned here because the overlay is what would show them.
    #[test]
    fn learning_a_hero_is_not_a_swap_the_window_hears_about() {
        let heroes = HeroCatalog::bundled();
        let mut log = SwapLog::new();
        let t0 = Instant::now();

        log.observe_at(&swap_snap(None, Some(3), Some("Vex")), &heroes, t0);
        assert!(
            log.observe_at(&swap_snap(Some(1), Some(3), Some("Vex")), &heroes, t0)
                .is_empty(),
            "a hero id that finally read is not a swap"
        );
        assert!(
            log.observe_at(&swap_snap(None, Some(3), Some("Vex")), &heroes, t0)
                .is_empty()
        );
    }

    /// A swap is carried on several frames and then stops being carried.
    ///
    /// The window is a separate process reading an event stream it can miss frames of: a
    /// swap published on exactly one frame is a swap that disappears whenever the window is
    /// busy on the tick it lands. So the log republishes it for a while - and then drops it,
    /// because a swap republished forever is a popup that never goes away.
    #[test]
    fn a_swap_is_carried_for_a_while_and_then_dropped() {
        let heroes = HeroCatalog::bundled();
        let mut log = SwapLog::new();
        let t0 = Instant::now();

        log.observe_at(&swap_snap(Some(1), Some(3), None), &heroes, t0);
        let first = log.observe_at(&swap_snap(Some(2), Some(3), None), &heroes, t0);
        assert_eq!(first.len(), 1);

        let later = log.observe_at(
            &swap_snap(Some(2), Some(3), None),
            &heroes,
            t0 + Duration::from_secs(1),
        );
        assert_eq!(later.len(), 1, "a quiet tick still carries it: {later:?}");
        assert_eq!(later[0].seq, first[0].seq, "and it keeps its identity");

        let expired = log.observe_at(
            &swap_snap(Some(2), Some(3), None),
            &heroes,
            t0 + SWAP_RETENTION + Duration::from_secs(1),
        );
        assert!(
            expired.is_empty(),
            "still carried after its life: {expired:?}"
        );
    }

    /// Every swap gets its own sequence number, and keeps it while it is carried.
    ///
    /// This is the only thing that lets the window tell a re-sent swap from a second one.
    /// Without it the republishing above would re-fire the same popup on every frame; with
    /// a number that changed on re-send it would do exactly the same thing.
    #[test]
    fn every_swap_gets_a_distinct_stable_sequence_number() {
        let heroes = HeroCatalog::bundled();
        let mut log = SwapLog::new();
        let t0 = Instant::now();

        log.observe_at(&swap_snap(Some(1), Some(3), None), &heroes, t0);
        let one = log.observe_at(&swap_snap(Some(2), Some(3), None), &heroes, t0);
        let two = log.observe_at(&swap_snap(Some(4), Some(3), None), &heroes, t0);

        assert_eq!(one.len(), 1);
        assert_eq!(two.len(), 2, "both swaps are still in the window: {two:?}");
        assert_eq!(two[0].seq, one[0].seq, "the first keeps its number");
        assert!(
            two[1].seq > two[0].seq,
            "the second is newer: {:?} vs {:?}",
            two[0].seq,
            two[1].seq
        );
    }

    /// A swap whose slot or name did not read says so rather than showing a zero.
    ///
    /// `slot: 0` is not a lobby slot and an empty name is not a name. Both have to arrive
    /// as `None` so the window can say the reader could not identify the player, which is a
    /// different statement from naming slot zero.
    #[test]
    fn a_swap_the_reader_could_not_attribute_says_so_rather_than_showing_zero() {
        let heroes = HeroCatalog::bundled();
        let mut log = SwapLog::new();
        let t0 = Instant::now();

        log.observe_at(&swap_snap(Some(1), None, None), &heroes, t0);
        let out = log.observe_at(&swap_snap(Some(2), None, None), &heroes, t0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].slot, None, "an unread slot is not slot 0");
        assert_eq!(out[0].player, None, "an unread name is not an empty name");

        let mut log = SwapLog::new();
        log.observe_at(&swap_snap(Some(1), Some(2), Some("")), &heroes, t0);
        let out = log.observe_at(&swap_snap(Some(2), Some(2), Some("")), &heroes, t0);
        assert_eq!(out[0].player, None);
    }

    /// A new match drops the swaps from the old one.
    ///
    /// They belong to players who are no longer in the lobby, and a popup naming slot 4 of
    /// the previous match while a new one is loading is worse than no popup.
    #[test]
    fn a_new_match_clears_the_carried_swaps() {
        let heroes = HeroCatalog::bundled();
        let mut log = SwapLog::new();
        let t0 = Instant::now();

        log.observe_at(&swap_snap(Some(1), Some(3), None), &heroes, t0);
        assert_eq!(
            log.observe_at(&swap_snap(Some(2), Some(3), None), &heroes, t0)
                .len(),
            1
        );

        let mut next = swap_snap(Some(2), Some(3), None);
        next.match_id = Some(43);
        assert!(
            log.observe_at(&next, &heroes, t0).is_empty(),
            "last match's swaps must not follow the lobby"
        );
    }

    /// The carried set is bounded, so a pathological run cannot grow the payload forever.
    #[test]
    fn the_carried_swaps_are_bounded() {
        let heroes = HeroCatalog::bundled();
        let mut log = SwapLog::new();
        let t0 = Instant::now();

        log.observe_at(&swap_snap(Some(1), Some(3), None), &heroes, t0);
        let mut last = Vec::new();
        for i in 0..SWAP_CAPACITY as u32 * 3 {
            let hero = if i % 2 == 0 { 2 } else { 1 };
            last = log.observe_at(&swap_snap(Some(hero), Some(3), None), &heroes, t0);
        }
        assert!(
            last.len() <= SWAP_CAPACITY,
            "carried {} swaps, cap is {SWAP_CAPACITY}",
            last.len()
        );
        assert_eq!(
            last.len(),
            SWAP_CAPACITY,
            "and it is the newest that survive"
        );
    }

    /// The swaps reach the window on the match view rather than on a stream of their own.
    ///
    /// Deliberate: the window already receives one `match` payload per poll, and a swap
    /// riding on it cannot arrive out of order with the scoreboard that explains it.
    #[test]
    fn swaps_travel_on_the_match_view() {
        let heroes = HeroCatalog::bundled();
        let mut log = SwapLog::new();
        let t0 = Instant::now();
        let before = swap_snap(Some(1), Some(3), Some("Vex"));
        let after = swap_snap(Some(2), Some(3), Some("Vex"));

        log.observe_at(&before, &heroes, t0);
        let view = MatchView::from_snapshot(&after, &heroes)
            .with_swaps(log.observe_at(&after, &heroes, t0));
        assert_eq!(view.hero_swaps.len(), 1);
        assert_eq!(view.hero_swaps[0].to.name, "Seven");

        assert!(MatchView::idle("Hideout".into()).hero_swaps.is_empty());
    }

    /// Only the local player is the local player.
    ///
    /// `is_local` drives which row the window highlights as "you". Unread must be `false`:
    /// highlighting an arbitrary row as the viewer is worse than highlighting none, and
    /// `unwrap_or(true)` passed every test.
    #[test]
    fn an_unread_local_flag_highlights_nobody() {
        let heroes = HeroCatalog::bundled();
        let local = |row: PlayerRow| PlayerView::from_row(&row, &heroes, false).is_local;

        assert!(local(PlayerRow {
            is_local: Some(true),
            ..Default::default()
        }));
        assert!(!local(PlayerRow {
            is_local: Some(false),
            ..Default::default()
        }));
        assert!(!local(PlayerRow {
            is_local: None,
            ..Default::default()
        }));
    }
}
