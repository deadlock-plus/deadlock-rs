//! Bucketing a player's stats by *where* they happened.
//!
//! Companion's per-match JSON does not carry one `kills` per player, it carries several,
//! keyed by the objective the kill happened at. The shape is:
//!
//! ```text
//! open.{kills,deaths,assists,heroDamage,objectiveDamage,healing,timeSeconds}
//! midboss.{...,rejuvSecured}
//! walker.{...}
//! urn.{timeSeconds}
//! ```
//!
//! plus the match-level `urnFights` and `urnFightSeconds`, and `rift_fights` alongside
//! them.
//!
//! # Every rule in this module is inferred
//!
//! The output *shape* above is known. **How the bucketing is computed is not** - it was
//! never traced from anything. What this module implements is proximity and participation
//! attribution against the NPC classes `deadlock-reader` maps (`LAYOUT.md` §6.3).
//!
//! So the radii, the attribution rule, the bucket precedence and the definition of "a
//! fight" are all **choices made here**, not facts read from anywhere. Each is a named
//! constant or a named function whose docs say what choosing it wrong does in each
//! direction. A disagreement with another tracker's numbers is evidence about this module,
//! not about that tracker.
//!
//! One number in the whole module has a real source behind it, and it is not used as-is;
//! see [`RIFT_RADIUS`].
//!
//! # Nothing here has been seen against a live match
//!
//! The client this was written against sat in the **Hideout**, which reports **zero
//! objectives**. Every test below is synthetic: hand-built snapshots fed to the tracker.
//! No radius, no precedence rule and no fight definition has been checked against a
//! running game, because there was no running game to check it against.
//!
//! # Positions are a compile-time requirement, not a runtime one
//!
//! Without world coordinates nothing here can be attributed at all. Two runtime
//! behaviours were available for that case - report nothing, or report everything as
//! `open` - and both were rejected: each produces a full set of plausible-looking keys
//! whose zeros a consumer cannot tell from a measurement. So the module is behind the
//! non-default `objective-context` feature, which forwards to `deadlock-reader`'s
//! `positions`. A build without positions does not get a tracker that reports nothing; it
//! does not get a tracker.
//!
//! The runtime version of the same failure survives, because the feature only guarantees
//! the *field*, not a successful read. A tick where a player's position could not be read
//! is unattributable, and lands in [`PlayerBuckets::unattributed`] and
//! [`PlayerBuckets::unattributable_intervals`] rather than in `open`. A match whose
//! `unattributable_intervals` equals its tick count is one where positions never read, and
//! that is visible in the output instead of looking like a player who did nothing.
//!
//! # Distance is measured in the horizontal plane
//!
//! `X` and `Y` only; `Z` is dropped. Deadlock is a vertical map and both objectives this
//! module buckets on are tall: a walker's origin is at its feet, and a player fighting it
//! from the walkway above or the lane beneath it differs from that origin mostly in `Z`. A
//! sphere centred on the origin excludes exactly the fights the bucket is meant to catch.
//!
//! The cost is stated rather than hidden: a player in a tunnel directly under the Midboss
//! pit, or riding a zipline over it, is inside the cylinder and buckets to `midboss`
//! though they are nowhere near the fight. Trading that for the walkway case is a choice.
//! A 3D test would need a separate vertical tolerance rather than a radius, and there is
//! no datum for one.
//!
//! # What a "fight" is
//!
//! `urn_fights`, `urn_fight_seconds` and `rift_fights` all need a definition and the gap
//! analysis gives none. See [`FightContext`] and [`FIGHT_GAP_SECONDS`] for the rule and
//! its failure modes. It is deliberately positionless, because the Urn has no position to
//! be near - see the next section.
//!
//! # The Urn cannot be bucketed by proximity, and this module says so
//!
//! [`LiveSnapshot`] carries no Urn position. `Objective` covers structures only, and
//! `C_NPC_Neutral_SinnersSacrifice` is explicitly excluded from it as a creep class;
//! [`Timers::urn`](deadlock_reader::timers::Timers::urn) reports presence and a count and
//! nothing else. There is no coordinate to measure a distance to.
//!
//! So `urn.time_seconds` is **not** the proximity metric the other buckets are, and is
//! not what Companion publishes under that key. It is defined here as *seconds this player
//! was alive while an Urn was on the map*, which is a strict superset of any proximity
//! reading of the same key, and it is close to a constant across the ten living players.
//! It is reported because the key exists and omitting it silently would be worse, and it
//! is documented at every level so nobody mistakes it for a measurement of where anyone
//! stood. Give this module an Urn position and the rule becomes the same one the other
//! buckets use.
//!
//! There is one better input available and it is deliberately not taken.
//! [`HOLDING_URN`](deadlock_reader::snapshot::HOLDING_URN) is the modifier the carrier
//! wears, so "who is carrying it" is readable exactly - but it lives on
//! [`PlayerRow::modifiers`](deadlock_reader::snapshot::PlayerRow::modifiers), behind
//! `deadlock-reader`'s `modifiers` feature, which re-reads every player's modifier list
//! every tick and roughly triples the cost of a snapshot. This feature advertises itself
//! as the cheap one - positions and nothing else - and quietly tripling a consumer's tick
//! cost to improve one key would break that promise. It is also a different claim:
//! *carrying* the Urn is not the same as *fighting over* it, and swapping one weak reading
//! of the key for another weak reading is not obviously progress. A carry-aware bucket
//! belongs behind its own feature.

use std::collections::HashMap;

use deadlock_reader::Team;
use deadlock_reader::snapshot::{
    LiveSnapshot, Objective, ObjectiveKind, PlayerRow, StructureState,
};

/// How far from the Midboss counts as fighting at it, in world units.
///
/// `npc_super_neutral.m_flSightRangePlayers` from `npc_units.vdata_c`, carried as
/// [`DEFAULT_MIDBOSS_SIGHT_RADIUS`](deadlock_reader::tunables::DEFAULT_MIDBOSS_SIGHT_RADIUS)
/// and cross-checked against the archive. Sight range is the distance at which the
/// objective and a player can interact at all, which is a **sourced** meaning rather than a
/// guessed one.
///
/// An earlier revision called this "inferred, and uncalibrated" and reasoned about the size
/// of the Midboss pit against the coordinate scale. The number it arrived at was `1500`,
/// which is exactly what the game ships - a good guess, but a guess, and nothing said so
/// except the doc comment. The value has not changed; what changed is that it now has a
/// source and a test that fails if the game moves it.
///
/// # What it is still not
///
/// It is not a claim about where a *fight* is, and the two failure directions are real:
///
/// - **Too small** leaves genuine Midboss fights in `open`. `midboss.*` under-reports
///   uniformly, and the loss is invisible: `open` is a plausible bucket for a fight, so
///   nothing about the output looks wrong.
/// - **Too large** pulls unrelated jungle skirmishes into `midboss.*` and empties `open`
///   of everything happening in the middle of the map. That failure is at least visible,
///   because `midboss.time_seconds` starts approaching the match length.
///
/// A player sniping the Midboss from beyond its sight range is outside this and still
/// fighting over it.
///
/// # Observed against a real match, and the "too large" direction is the one that happens
///
/// Measured while spectating match `100650421`. One minute in, four players accumulated
/// **21 to 27 seconds each** in the Midboss bucket — and at 3:24 the Midboss was still at
/// `13195/13195`, untouched. Nobody was fighting it. Mid lane runs past the pit, and 1500
/// units is 38 metres, so ordinary lane traffic sits inside the radius.
///
/// The bucket is doing exactly what it says: it measures **proximity**, not contest. But a
/// consumer reading `midboss.time_seconds = 26` will reasonably hear "contested the
/// Midboss", and on that evidence it means "walked past it". The failure direction the
/// docs above call *too large* is the one that occurs in practice; the *too small*
/// direction was not observed at all.
///
/// Qualifying the bucket on damage — the way [`FightContext`] already qualifies the Urn
/// and rift — would separate the two. That is a design change rather than a tuning one,
/// so it is recorded here rather than made silently.
///
/// Override it with [`ObjectiveContextTracker::with_midboss_radius`].
pub const MIDBOSS_RADIUS: f32 = deadlock_reader::tunables::DEFAULT_MIDBOSS_SIGHT_RADIUS;

/// How far from a walker counts as fighting at it, in world units.
///
/// `npc_boss_tier2.m_flSightRangePlayers`, carried as
/// [`DEFAULT_WALKER_SIGHT_RADIUS`](deadlock_reader::tunables::DEFAULT_WALKER_SIGHT_RADIUS).
/// Read [`MIDBOSS_RADIUS`] first: what this means, and how it fails, are the same.
///
/// **This value changed.** An earlier revision guessed `1200`; the shipped sight range is
/// `944`, so the radius narrowed by about a fifth. Unlike the Midboss, the guess was not
/// close, and a walker fight between 944 and 1200 units out now files under `open` rather
/// than `walker`. That is the trade of a sourced number over a plausible one.
///
/// The Walker's neighbouring ranges are metres in disguise, which is a third independent
/// confirmation of the unit: `m_flSightRange` is `1417.32`, or `36 m`, and
/// `m_flStompImpactRadius` is `570.8661`, or `14.5 m`, at 39.37 units per metre.
pub const WALKER_RADIUS: f32 = deadlock_reader::tunables::DEFAULT_WALKER_SIGHT_RADIUS;

/// How far from the rift cash-in point counts as being at the rift, in world units.
///
/// # The one measured radius in this module
///
/// Unlike [`MIDBOSS_RADIUS`] and [`WALKER_RADIUS`], this is not a judgement call. The game
/// states the rift capture zone twice, in two units, and the two agree:
///
/// - `scripts/generic_data.vdata_c`, `m_KothParams.m_flKothRadius = 20`
/// - `scripts/misc.vdata_c`, `citadel_koth_cashin.m_AuraModifier.m_flAuraRadius = 787.402`
///
/// `787.402 / 20` is `39.3701`, inches per metre, so the first is metres and the second is
/// world units - which is what positions are measured in. This constant is that second
/// figure, and it equals
/// [`DEFAULT_RIFT_RADIUS`](deadlock_reader::tunables::DEFAULT_RIFT_RADIUS).
///
/// An earlier draft of this module used `1200.0` and argued at length that the datum could
/// not be used, on the grounds that 20 against coordinates like `[11570.56, -6601.84,
/// -82.0]` is a circle a quarter of a hero's height across. That observation was right and
/// the conclusion drawn from it was wrong: the mismatch was a unit, not an unknowable, and
/// the game ships the conversion in the aura that applies the capture modifier. The
/// guessed figure was too wide by half.
///
/// # The zone is a cylinder, and the game says so
///
/// `citadel_koth_cashin` gives it `m_flZoneHeightMeters = 14`, and the aura is a
/// `modifier_base_aura_cylinder` with `m_flAuraTargetingCylinderHalfHeight = 1000` and
/// `m_flAuraTargetingCylinderUpOffset = 400`. So the horizontal-only distance test this
/// module uses is not an approximation of a sphere - it is the shape the engine uses.
///
/// Failure directions: this radius only widens or narrows the *gate* on a rift fight, so
/// getting it wrong moves `rift_fights` but nothing per-player. Override it with
/// [`ObjectiveContextTracker::with_rift_radius`], which is also how a caller who has
/// retuned their [`Reader`](deadlock_reader::Reader) passes their own
/// `Tunables::rift_radius` in - see [`ObjectiveContextTracker`] for why it cannot be
/// picked up automatically.
///
/// Still never observed against a live rift: the client used for this work sat in the
/// Hideout throughout. The radius is sourced; the behaviour around it is not.
pub const RIFT_RADIUS: f32 = 787.402;

/// How long a lull may run before a fight is treated as over, in seconds.
///
/// **Inferred.** Nothing says what Companion counts as one fight, so this is the whole of
/// the boundary between "one fight with a pause in it" and "two fights".
///
/// - **Too short** splits a single contest into several: `urn_fights` and `rift_fights`
///   inflate while `urn_fight_seconds` is unchanged, because seconds are accumulated per
///   exchange rather than per fight. A fight where both sides back off to heal is the
///   worst case.
/// - **Too long** merges contests that were genuinely separate. The counts deflate, and a
///   window longer than the gap between two waves makes them indistinguishable.
pub const FIGHT_GAP_SECONDS: f32 = 8.0;

/// The longest gap between two snapshots that still forms a usable interval, in seconds.
///
/// Every number in this module is a difference between two ticks: seconds are `now`
/// minus the previous `now`, and a kill is a counter that went up between them. Across a
/// long gap neither is safe - a player can enter and leave three buckets, and the
/// counters cannot say which one the kills belong to.
///
/// - **Too short** discards ordinary intervals on a machine that stutters, so every total
///   under-reports and the loss shows up in
///   [`PlayerBuckets::unattributable_intervals`].
/// - **Too long** lets a real polling gap be attributed to whichever bucket happened to
///   hold at both ends, which is a fabricated attribution rather than a lost one.
///
/// Sized as several times the 50-100 ms the reader source polls at, so a hitch is
/// tolerated and a genuine stall is not.
pub const MAX_TICK_GAP: f32 = 1.0;

/// A stale interval must stay far shorter than a lull.
///
/// [`MAX_TICK_GAP`] bounds how stale one interval may be and [`FIGHT_GAP_SECONDS`] how
/// long a fight may go quiet. If the first ever approached the second, a single stale
/// interval could span an entire lull and two separate fights would silently become one.
///
/// Checked at compile time rather than in a test: both sides are constants, so a
/// violation is a build error and never reaches a test run.
const _: () = assert!(MAX_TICK_GAP * 4.0 <= FIGHT_GAP_SECONDS);

/// The `unattributed.` prefix, which is this crate's own and not one of Companion's keys.
const UNATTRIBUTED: &str = "unattributed";

/// Where something happened, as far as proximity can tell.
///
/// The set is Companion's, not the game's: there is no `patron` or `guardian` bucket here
/// because Companion publishes none, so a fight at the Patron buckets to
/// [`Open`](ObjectiveBucket::Open) like any other fight away from a walker or the Midboss.
///
/// The Urn is absent for a different reason - it has no position to be near at all. See
/// the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ObjectiveBucket {
    /// Away from every objective this module can locate.
    ///
    /// A positive claim, not a fallback for ignorance: it means the player's position was
    /// read and was near nothing. A player whose position could not be read is not `Open`,
    /// it is unattributable.
    Open,
    /// Within [`MIDBOSS_RADIUS`] of a Midboss that is not reported destroyed.
    Midboss,
    /// Within [`WALKER_RADIUS`] of a walker that is not reported destroyed.
    Walker,
}

impl ObjectiveBucket {
    /// Every bucket, in the order [`PlayerBuckets::metrics`] emits them.
    pub const ALL: [ObjectiveBucket; 3] = [
        ObjectiveBucket::Open,
        ObjectiveBucket::Midboss,
        ObjectiveBucket::Walker,
    ];

    /// The metric-key segment, e.g. `midboss`.
    pub fn label(self) -> &'static str {
        match self {
            ObjectiveBucket::Open => "open",
            ObjectiveBucket::Midboss => "midboss",
            ObjectiveBucket::Walker => "walker",
        }
    }

    /// The radius a player must be inside to fall in this bucket, in world units.
    ///
    /// `None` for [`Open`](ObjectiveBucket::Open), which is defined by being inside
    /// nothing rather than by a distance of its own. These are the module-level defaults;
    /// a tracker built with [`ObjectiveContextTracker::with_midboss_radius`] or its
    /// siblings does not use them.
    pub fn radius(self) -> Option<f32> {
        match self {
            ObjectiveBucket::Open => None,
            ObjectiveBucket::Midboss => Some(MIDBOSS_RADIUS),
            ObjectiveBucket::Walker => Some(WALKER_RADIUS),
        }
    }

    /// Precedence when a player is inside two zones at once. Lower wins.
    ///
    /// See [`bucket_at`] for the whole rule and why the Midboss outranks a walker.
    fn rank(self) -> u8 {
        match self {
            ObjectiveBucket::Midboss => 0,
            ObjectiveBucket::Walker => 1,
            ObjectiveBucket::Open => 2,
        }
    }
}

impl std::fmt::Display for ObjectiveBucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.label())
    }
}

/// Which objective a match-level fight was being fought over.
///
/// # What counts as a fight, and what that costs
///
/// **Inferred.** A fight is a run of consecutive intervals in which *both* playing sides
/// recorded a positive hero-damage delta, while the objective's own context held. A lull
/// of up to [`FIGHT_GAP_SECONDS`] does not end the run; a longer one does.
///
/// The rule is positionless on purpose. The Urn has no coordinate on a snapshot at all
/// (see the module docs), so a proximity-based fight rule could only ever have applied to
/// the rift, and two match-level counters computed by two different rules would not be
/// comparable with each other.
///
/// Its failure modes, all real:
///
/// - **Two players trading poke across the map** while the Urn is up reads as an Urn
///   fight. Nothing in the rule ties the damage to the objective beyond both happening at
///   once; that is the whole cost of having no position.
/// - **A one-sided gank counts for nothing.** If the victim never lands a hit, only one
///   side has a damage delta, so it is not a fight - and a clean pick at the Urn is
///   exactly the event most worth counting.
/// - **A fight that outlives its context** stops being counted the moment the objective
///   goes, even though the same players are still fighting over it.
/// - **A damage counter that could not be read** contributes nothing, so a tick with a
///   failed read looks like a tick with no damage and can split one fight into two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum FightContext {
    /// An Urn was on the map. Presence only; there is no zone to be in.
    Urn,
    /// A rift was being contested, and at least one player was inside [`RIFT_RADIUS`] of
    /// the cash-in point when the client reported where that was.
    Rift,
}

impl FightContext {
    /// The metric-key segment, e.g. `urn`.
    pub fn label(self) -> &'static str {
        match self {
            FightContext::Urn => "urn",
            FightContext::Rift => "rift",
        }
    }
}

impl std::fmt::Display for FightContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.label())
    }
}

/// Something that changed in a player's bucketing, or in the match's fight state.
///
/// Ordering within one tick is: every player event in lobby-slot order, then every fight
/// event in [`FightContext`] order.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum ObjectiveContextEvent {
    /// A player was first seen in a bucket they were not in on the previous tick.
    ///
    /// Emitted for [`ObjectiveBucket::Open`] too. Suppressing it there would leave
    /// `open.time_seconds` unexplainable from the event stream, and a consumer that does
    /// not care can filter on one field.
    Entered {
        /// Lobby slot of the player.
        slot: u32,
        /// The bucket entered.
        bucket: ObjectiveBucket,
        /// [`MatchClock::now`](deadlock_reader::snapshot::MatchClock) of the tick it was
        /// first seen on.
        at: f32,
    },
    /// A player is no longer in a bucket they were in.
    Left {
        /// Lobby slot of the player.
        slot: u32,
        /// The bucket left.
        bucket: ObjectiveBucket,
        /// The tick the departure was noticed on, which is the first tick they were not
        /// in it.
        at: f32,
        /// Seconds between the first and last tick they were seen in it.
        ///
        /// Measured to the *last sighting* rather than to `at`, so it agrees exactly with
        /// what this visit contributed to [`BucketTotals::time_seconds`]. The interval
        /// during which they left is credited to nobody, because its two endpoints
        /// disagree about where the player was.
        seconds: f32,
    },
    /// A fight opened over an objective.
    FightStarted {
        /// Which objective it is being fought over.
        context: FightContext,
        /// The tick the first two-sided exchange was seen on.
        at: f32,
    },
    /// A fight was declared over, [`FIGHT_GAP_SECONDS`] having passed with no exchange.
    FightEnded {
        /// Which objective it was fought over.
        context: FightContext,
        /// The tick of the last exchange, not the tick the end was noticed on.
        at: f32,
        /// Seconds of two-sided exchange in this fight. Lulls inside it are excluded; see
        /// [`MatchFights::urn_fight_seconds`].
        seconds: f32,
    },
}

/// One bucket's totals for one player.
///
/// Every field is a sum of per-interval deltas that this module was able to attribute.
/// All of them are lower bounds: an interval it could not attribute is counted in
/// [`PlayerBuckets::unattributed`] instead of here, never silently in `open`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BucketTotals {
    /// Kills whose interval fell in this bucket.
    pub kills: u32,
    /// Deaths whose interval fell in this bucket.
    ///
    /// This is the field the death exception exists for: a dead player has no pawn and
    /// therefore no position, so under the plain both-endpoints-agree rule a death would be
    /// unattributable almost every time. A player who died during the interval keeps the
    /// bucket they were in at its start - which is still an inference, and credits a death
    /// to a delayed effect after disengaging to the fight they left.
    pub deaths: u32,
    /// Assists whose interval fell in this bucket.
    pub assists: u32,
    /// Hero damage dealt during intervals in this bucket.
    pub hero_damage: u32,
    /// Objective damage dealt during intervals in this bucket.
    ///
    /// Bucketed by where the *player* was, not by what they hit, because a snapshot says
    /// nothing about what a damage counter was raised against. A player shelling a walker
    /// from outside [`WALKER_RADIUS`] credits `open.objective_damage`.
    pub objective_damage: u32,
    /// Healing done during intervals in this bucket.
    pub healing: u32,
    /// Seconds spent in this bucket.
    ///
    /// Only intervals whose two endpoints agreed on the bucket are counted, so entering
    /// and leaving each cost one interval - at most two poll intervals per visit. That is
    /// an under-count with a stated bound, where crediting a transition to either side
    /// would be a claim about time nobody observed.
    pub time_seconds: f32,
}

impl BucketTotals {
    /// Whether anything at all landed here.
    pub fn is_empty(&self) -> bool {
        *self == BucketTotals::default()
    }

    fn add(&mut self, d: &Deltas) {
        self.kills += d.kills;
        self.deaths += d.deaths;
        self.assists += d.assists;
        self.hero_damage += d.hero_damage;
        self.objective_damage += d.objective_damage;
        self.healing += d.healing;
    }

    fn push_metrics(&self, prefix: &str, out: &mut Vec<(String, f64)>) {
        out.push((format!("{prefix}.kills"), f64::from(self.kills)));
        out.push((format!("{prefix}.deaths"), f64::from(self.deaths)));
        out.push((format!("{prefix}.assists"), f64::from(self.assists)));
        out.push((format!("{prefix}.hero_damage"), f64::from(self.hero_damage)));
        out.push((
            format!("{prefix}.objective_damage"),
            f64::from(self.objective_damage),
        ));
        out.push((format!("{prefix}.healing"), f64::from(self.healing)));
        out.push((
            format!("{prefix}.time_seconds"),
            f64::from(self.time_seconds),
        ));
    }
}

/// Match-level fight counts.
///
/// See [`FightContext`] for what a fight is and for the four ways the rule is wrong.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MatchFights {
    /// Fights that opened while an Urn was on the map.
    pub urn_fights: u32,
    /// Seconds of two-sided exchange inside those fights.
    ///
    /// Accumulated per exchange, not as the wall-clock span of a fight: a lull that does
    /// not end the fight contributes no seconds. So this is at most the sum of the fights'
    /// lengths, and reads as "how long were both sides actually trading" rather than "how
    /// long did the fights last". The other reading was available and was not chosen
    /// because it would credit up to [`FIGHT_GAP_SECONDS`] of silence to a fight.
    pub urn_fight_seconds: f32,
    /// Fights that opened while a rift was being contested.
    pub rift_fights: u32,
    /// The rift's equivalent of [`MatchFights::urn_fight_seconds`].
    ///
    /// Kept because it costs nothing and the asymmetry would be strange, but **not**
    /// emitted by [`MatchFights::metrics`]: Companion publishes `urnFightSeconds` and no
    /// rift counterpart was ever seen in its output, and inventing a key that looks like
    /// one of Companion's is exactly the kind of thing this module refuses to do.
    pub rift_fight_seconds: f32,
}

impl MatchFights {
    /// The three match-level keys.
    ///
    /// Snake case, matching every other metric key this crate emits, where Companion's own
    /// JSON is camel case.
    pub fn metrics(&self) -> Vec<(String, f64)> {
        vec![
            ("urn_fights".to_owned(), f64::from(self.urn_fights)),
            (
                "urn_fight_seconds".to_owned(),
                f64::from(self.urn_fight_seconds),
            ),
            ("rift_fights".to_owned(), f64::from(self.rift_fights)),
        ]
    }
}

/// What one interval added to a player's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Deltas {
    kills: u32,
    deaths: u32,
    assists: u32,
    hero_damage: u32,
    objective_damage: u32,
    healing: u32,
}

impl Deltas {
    fn is_empty(&self) -> bool {
        *self == Deltas::default()
    }
}

/// One tick's reading of a player, kept so the next tick can difference against it.
#[derive(Clone, Copy, Debug)]
struct Sample {
    now: f32,
    bucket: Option<ObjectiveBucket>,
    alive: Option<bool>,
    has_rejuvenator: Option<bool>,
    kills: Option<u32>,
    deaths: Option<u32>,
    assists: Option<u32>,
    hero_damage: Option<u32>,
    objective_damage: Option<u32>,
    healing: Option<u32>,
}

/// A visit to one bucket, open until the player is seen somewhere else.
#[derive(Clone, Copy, Debug)]
struct Visit {
    bucket: ObjectiveBucket,
    since: f32,
    last_seen: f32,
}

/// One player's bucketed ledger for the current match.
#[derive(Clone, Debug, Default)]
pub struct PlayerBuckets {
    buckets: HashMap<ObjectiveBucket, BucketTotals>,
    unattributed: BucketTotals,
    unattributable_intervals: u32,
    attributed_intervals: u32,
    urn_seconds: f32,
    rejuv_secured: u32,
    visit: Option<Visit>,
    last: Option<Sample>,
}

impl PlayerBuckets {
    /// Totals for one bucket. Zeroed for a bucket this player never reached.
    pub fn totals(&self, bucket: ObjectiveBucket) -> BucketTotals {
        self.buckets.get(&bucket).copied().unwrap_or_default()
    }

    /// Every bucket, in [`ObjectiveBucket::ALL`] order.
    ///
    /// Buckets with nothing in them are present and zero, unlike the crowd-control
    /// module's kinds. The set is closed and three wide, so a zero here is a measurement -
    /// "this player did nothing at the walkers" - rather than the absence of a kind
    /// nobody has been taught about. The genuinely unmeasured case has its own home in
    /// [`PlayerBuckets::unattributed`].
    pub fn by_bucket(&self) -> Vec<(ObjectiveBucket, BucketTotals)> {
        ObjectiveBucket::ALL
            .iter()
            .map(|b| (*b, self.totals(*b)))
            .collect()
    }

    /// What could not be placed in any bucket.
    ///
    /// An interval lands here when the player's position was unreadable at an endpoint, or
    /// when the two endpoints disagreed about where they were. It is this module's own
    /// bookkeeping and has no Companion counterpart, and it exists so that a consumer
    /// summing the three buckets and finding less than the scoreboard can see the
    /// remainder instead of inferring a bug.
    pub fn unattributed(&self) -> BucketTotals {
        self.unattributed
    }

    /// How many intervals could not be attributed, whether or not anything happened in
    /// them.
    ///
    /// Compare it with [`PlayerBuckets::attributed_intervals`]. All intervals
    /// unattributable means positions never read this match, which otherwise looks exactly
    /// like a player who stood still and did nothing.
    pub fn unattributable_intervals(&self) -> u32 {
        self.unattributable_intervals
    }

    /// How many intervals were placed in a bucket.
    pub fn attributed_intervals(&self) -> u32 {
        self.attributed_intervals
    }

    /// Seconds this player was alive while an Urn was on the map.
    ///
    /// **Not a proximity metric.** Read the module docs before using it: there is no Urn
    /// position on a snapshot, so this is presence-and-alive, which is a strict superset
    /// of what Companion's `urn.timeSeconds` can mean and is nearly the same number for
    /// every living player.
    pub fn urn_seconds(&self) -> f32 {
        self.urn_seconds
    }

    /// Rising edges of `m_bHasRejuvenator` on this player.
    ///
    /// **Inferred, and deliberately not bucketed.** The Rejuvenator only exists as a
    /// consequence of a Midboss kill, so it is reported under `midboss.` wherever the
    /// player was standing when they picked it up. Bucketing it by proximity would drop
    /// the pickup of a statue that dropped and was collected elsewhere, which is still a
    /// secure; the cost is that collecting a team-mate's dropped statue across the map is
    /// credited here too.
    ///
    /// Both endpoints must be readable, so an unreadable flag turning readable is not an
    /// edge.
    pub fn rejuv_secured(&self) -> u32 {
        self.rejuv_secured
    }

    /// The ledger as flat metric keys, e.g. `midboss.hero_damage`.
    ///
    /// The `open.`, `midboss.`, `walker.`, `urn.` and `midboss.rejuv_secured` keys are
    /// Companion's, in this crate's snake case. The `unattributed.` keys are this
    /// module's own and are documented as such on
    /// [`PlayerBuckets::unattributed`]; they are emitted because a consumer cannot audit
    /// the other three without them.
    pub fn metrics(&self) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        for (bucket, totals) in self.by_bucket() {
            totals.push_metrics(bucket.label(), &mut out);
            if bucket == ObjectiveBucket::Midboss {
                out.push((
                    "midboss.rejuv_secured".to_owned(),
                    f64::from(self.rejuv_secured),
                ));
            }
        }
        out.push(("urn.time_seconds".to_owned(), f64::from(self.urn_seconds)));
        self.unattributed.push_metrics(UNATTRIBUTED, &mut out);
        out.push((
            format!("{UNATTRIBUTED}.intervals"),
            f64::from(self.unattributable_intervals),
        ));
        out
    }
}

/// Squared distance in the horizontal plane.
///
/// `Z` is dropped; the module docs say why, and what it costs. Squared so nothing needs a
/// square root to compare against a radius.
fn planar_distance_sq(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

/// Which bucket a kind of structure contributes, if any.
///
/// Only the two Companion publishes. Guardians, base guardians, the Patron and shrines
/// have no bucket of their own, so a fight at any of them is `open`.
fn bucket_of_kind(kind: ObjectiveKind) -> Option<ObjectiveBucket> {
    match kind {
        ObjectiveKind::Midboss => Some(ObjectiveBucket::Midboss),
        ObjectiveKind::Walker => Some(ObjectiveBucket::Walker),
        _ => None,
    }
}

/// Whether a structure is still a place worth bucketing at.
///
/// [`StructureState::Unknown`] counts as standing. An unreadable state is not a destroyed
/// one, and silently moving a fight to `open` because a read failed is the same mistake
/// `deadlock-reader` avoids by having the third variant at all.
///
/// A destroyed walker is not a place: the tick after it falls, the fight over its rubble
/// is `open`. The cost is that the killing blow itself - a counter jump seen on the tick
/// after the structure went - is bucketed to `open` rather than to `walker`.
fn is_a_place(state: StructureState) -> bool {
    state != StructureState::Destroyed
}
/// Whether this bucket's radius is measured through the air or along the floor.
///
/// Not a detail — it decides what the Midboss bucket means.
///
/// **Structures are measured along the floor.** A walker's origin is at its feet and it is
/// tall; a fight from the walkway above or the lane beneath is a fight at the walker, and a
/// spherical test would exclude exactly the fights the bucket exists to catch. The rift is
/// the same, and the game agrees: its capture aura is a `modifier_base_aura_cylinder` with
/// a stated `m_flZoneHeightMeters`.
///
/// **The Midboss is measured through the air**, because it is underground and blue lane
/// runs over the top of it. Measured in match `100650421`: the Midboss sits at `z = -768`
/// and lane traffic passes at `z = +376`, 1144 units above. On the floor those players are
/// 1228 to 2119 units away and inside the radius; through the air they are 1679 to 2473 and
/// outside it. Along the floor the bucket counted them as fighting a Midboss that never
/// took a point of damage.
///
/// No new constant is needed for that, which is the tell that it is the right reading:
/// `m_flSightRangePlayers` is how far the NPC can *see*, and sight is not measured along
/// the ground.
fn measured_through_the_air(bucket: ObjectiveBucket) -> bool {
    matches!(bucket, ObjectiveBucket::Midboss)
}

/// Where a position falls, given the objectives on the map.
///
/// # Precedence
///
/// Deterministic, in three stages: **kind rank first** (the Midboss outranks a walker),
/// then **nearest**, then **lowest entity address**.
///
/// Rank before distance is a choice. A fight in range of both is rarer and more decisive
/// as a Midboss fight than as a walker one, so calling it `midboss` loses less than the
/// reverse; and the two radii should not overlap on this map anyway, which makes the rule
/// a tie-break rather than a policy most of the time. The cost, stated: where they do
/// overlap, a fight plainly at the walker reads as a Midboss fight.
///
/// The address tie-break **cannot change which bucket comes out**, and an earlier draft of
/// this comment claimed otherwise. Rank is a function of kind, so two candidates that tie
/// on rank and on distance are two objectives of the same kind, which map to the same
/// bucket: the map's symmetry makes exact ties real, but it makes them harmless. It is
/// kept because it makes the comparison a total order rather than one with an arbitrary
/// residue, and because a future bucket that is finer-grained than a kind - per-lane
/// walkers, say - would make it load-bearing overnight. Nothing at the public surface can
/// observe it today, which is why no test asserts it.
fn bucket_at(position: [f32; 3], objectives: &[Objective], radii: &Radii) -> ObjectiveBucket {
    let mut best: Option<(u8, f32, u64, ObjectiveBucket)> = None;
    for o in objectives {
        let Some(bucket) = bucket_of_kind(o.kind) else {
            continue;
        };
        if !is_a_place(o.state) {
            continue;
        }
        let Some(at) = o.position else {
            continue;
        };
        let radius = radii.of(bucket);
        // Through the air for the Midboss, along the floor for structures; see
        // `measured_through_the_air`.
        let d2 = if measured_through_the_air(bucket) {
            let dz = position[2] - at[2];
            planar_distance_sq(position, at) + dz * dz
        } else {
            planar_distance_sq(position, at)
        };
        if d2 > radius * radius {
            continue;
        }
        let candidate = (bucket.rank(), d2, o.address, bucket);
        if is_better(&candidate, best.as_ref()) {
            best = Some(candidate);
        }
    }
    best.map_or(ObjectiveBucket::Open, |(_, _, _, bucket)| bucket)
}

/// The three-stage comparison [`bucket_at`] documents, in one place.
fn is_better(
    candidate: &(u8, f32, u64, ObjectiveBucket),
    best: Option<&(u8, f32, u64, ObjectiveBucket)>,
) -> bool {
    let Some(best) = best else { return true };
    candidate
        .0
        .cmp(&best.0)
        .then_with(|| candidate.1.total_cmp(&best.1))
        .then_with(|| candidate.2.cmp(&best.2))
        .is_lt()
}

/// The bucket an interval's *seconds* belong to.
///
/// Both endpoints must be bucketable and must agree. Time is the one metric where a wrong
/// answer is a direct fabrication - "this player spent forty seconds at the Midboss" when
/// they walked past it - so a transition is credited to neither side rather than to a
/// guess about which half of the interval was longer.
fn interval_time_bucket(prev: &Sample, now: &Sample) -> Option<ObjectiveBucket> {
    match (prev.bucket, now.bucket) {
        (Some(a), Some(b)) if a == b => Some(a),
        _ => None,
    }
}

/// The bucket an interval's *counter deltas* belong to.
///
/// [`interval_time_bucket`]'s rule, plus one exception: **a player who died during the
/// interval keeps the bucket they were in at its start.**
///
/// The exception is not a fudge for a missing position, it is the case where the missing
/// position has a readable cause. A dead player has no pawn and so no coordinates, which
/// is precisely the state a death produces - without this, `midboss.deaths` and
/// `walker.deaths` would be near-permanently zero while `unattributed.deaths` held every
/// death in the match.
///
/// It is still an inference: the player is credited to where they last stood, and a death
/// to a delayed effect after disengaging is attributed to the fight they left.
fn interval_counter_bucket(prev: &Sample, now: &Sample) -> Option<ObjectiveBucket> {
    if let Some(bucket) = interval_time_bucket(prev, now) {
        return Some(bucket);
    }
    if now.bucket.is_none() && now.alive == Some(false) {
        return prev.bucket;
    }
    None
}

/// One counter's rise across an interval.
///
/// Both endpoints must be readable: an unreadable counter contributes nothing rather than
/// being treated as zero, which would turn the next successful read into a delta the size
/// of the whole match. A counter that went *down* - the post-game scoreboard, or a reset -
/// contributes nothing either.
fn delta(prev: Option<u32>, now: Option<u32>) -> u32 {
    match (prev, now) {
        (Some(a), Some(b)) => b.saturating_sub(a),
        _ => 0,
    }
}

/// The radii one tracker is using.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Radii {
    midboss: f32,
    walker: f32,
    rift: f32,
}

impl Default for Radii {
    fn default() -> Self {
        Radii {
            midboss: MIDBOSS_RADIUS,
            walker: WALKER_RADIUS,
            rift: RIFT_RADIUS,
        }
    }
}

impl Radii {
    fn of(&self, bucket: ObjectiveBucket) -> f32 {
        match bucket {
            ObjectiveBucket::Midboss => self.midboss,
            ObjectiveBucket::Walker => self.walker,
            // Unreachable through `bucket_at`, which never asks about `Open`. Zero rather
            // than a panic: a radius nothing is ever inside is the right meaning for a
            // bucket defined by being inside nothing.
            ObjectiveBucket::Open => 0.0,
        }
    }
}

/// One interval, as the fight detector sees it.
#[derive(Clone, Copy, Debug)]
struct Exchange {
    /// Whether the objective's own context held on this tick.
    in_context: bool,
    /// Whether both playing sides recorded hero damage over this interval.
    both_sides: bool,
    prev_now: f32,
    now: f32,
}

/// One objective's fight run, open while exchanges keep arriving.
#[derive(Clone, Copy, Debug, Default)]
struct FightRun {
    last_exchange_at: Option<f32>,
    seconds: f32,
}

impl FightRun {
    /// Fold one interval in, appending whatever it opened or closed to `out`.
    fn observe(
        &mut self,
        context: FightContext,
        interval: Exchange,
        fights: &mut MatchFights,
        out: &mut Vec<ObjectiveContextEvent>,
    ) {
        let Exchange {
            in_context,
            both_sides: exchanged,
            prev_now,
            now,
        } = interval;
        if in_context && exchanged {
            if self.last_exchange_at.is_none() {
                match context {
                    FightContext::Urn => fights.urn_fights += 1,
                    FightContext::Rift => fights.rift_fights += 1,
                }
                out.push(ObjectiveContextEvent::FightStarted { context, at: now });
            } else {
                let seconds = (now - prev_now).max(0.0);
                self.seconds += seconds;
                match context {
                    FightContext::Urn => fights.urn_fight_seconds += seconds,
                    FightContext::Rift => fights.rift_fight_seconds += seconds,
                }
            }
            self.last_exchange_at = Some(now);
            return;
        }
        if let Some(last) = self.last_exchange_at
            && now - last > FIGHT_GAP_SECONDS
        {
            out.push(ObjectiveContextEvent::FightEnded {
                context,
                at: last,
                seconds: self.seconds,
            });
            *self = FightRun::default();
        }
    }

    /// Close a run without waiting out [`FIGHT_GAP_SECONDS`], for a polling gap or a
    /// match change. Its seconds are already credited, so this only reports.
    fn close(&mut self, context: FightContext, out: &mut Vec<ObjectiveContextEvent>) {
        if let Some(last) = self.last_exchange_at {
            out.push(ObjectiveContextEvent::FightEnded {
                context,
                at: last,
                seconds: self.seconds,
            });
        }
        *self = FightRun::default();
    }
}

/// Accumulates objective-context buckets, per player, across snapshots.
///
/// Feed it every snapshot. It keeps a ledger per lobby slot for the life of a match and
/// clears the lot when the match id changes, because slots and every counter are recycled
/// between matches.
///
/// # Why the radii are not read from `Tunables`
///
/// [`Tunables`](deadlock_reader::Tunables) lives on the
/// [`Reader`](deadlock_reader::Reader), and a [`LiveSnapshot`] carries no reference to it.
/// So this crate genuinely cannot see a caller's `Tunables::rift_radius`, and the module
/// does not quietly keep a second copy of it either: [`RIFT_RADIUS`] is asserted equal to
/// [`DEFAULT_RIFT_RADIUS`](deadlock_reader::tunables::DEFAULT_RIFT_RADIUS), so the two
/// cannot drift. A caller who has retuned their reader passes their value in with
/// [`ObjectiveContextTracker::with_rift_radius`].
///
/// ```no_run
/// use deadlock_events::objective_context::ObjectiveContextTracker;
/// use deadlock_reader::Reader;
///
/// let reader = Reader::attach()?;
/// let mut buckets = ObjectiveContextTracker::new();
/// if let Some(snap) = reader.live_snapshot()? {
///     for event in buckets.update(&snap) {
///         println!("{event:?}");
///     }
///     if let Some(player) = buckets.player(1) {
///         for (key, value) in player.metrics() {
///             println!("{key} = {value}");
///         }
///     }
///     for (key, value) in buckets.fights().metrics() {
///         println!("{key} = {value}");
///     }
/// }
/// # Ok::<(), deadlock_reader::Error>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct ObjectiveContextTracker {
    match_id: Option<u64>,
    radii: Radii,
    /// The clock reading of the previous snapshot, so that the match-level interval is one
    /// number rather than whatever each player row happened to agree on.
    last_now: Option<f32>,
    players: HashMap<u32, PlayerBuckets>,
    fights: MatchFights,
    urn: FightRun,
    rift: FightRun,
}

impl ObjectiveContextTracker {
    /// A tracker with no history, using the module's default radii.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bucket to the Midboss at a different radius.
    ///
    /// Read [`MIDBOSS_RADIUS`] first: the two ways of getting this wrong hide in different
    /// places. A negative radius is clamped to zero, which puts nobody in the bucket.
    pub fn with_midboss_radius(mut self, world_units: f32) -> Self {
        self.radii.midboss = world_units.max(0.0);
        self
    }

    /// Bucket to walkers at a different radius. See [`WALKER_RADIUS`].
    pub fn with_walker_radius(mut self, world_units: f32) -> Self {
        self.radii.walker = world_units.max(0.0);
        self
    }

    /// Gate rift fights at a different radius.
    ///
    /// This is where a caller holding a [`Reader`](deadlock_reader::Reader) passes their
    /// own `Tunables::rift_radius` in - but read [`RIFT_RADIUS`] first, because the shipped
    /// value of that tunable is 20 and its unit does not reconcile with world coordinates.
    pub fn with_rift_radius(mut self, world_units: f32) -> Self {
        self.radii.rift = world_units.max(0.0);
        self
    }

    /// Forget every ledger.
    ///
    /// Call this after a gap in polling. Open visits and open fights are dropped rather
    /// than closed: neither end was observed, so reporting either would be a guess.
    pub fn reset(&mut self) {
        self.match_id = None;
        self.last_now = None;
        self.players.clear();
        self.fights = MatchFights::default();
        self.urn = FightRun::default();
        self.rift = FightRun::default();
    }

    /// The match these ledgers belong to.
    pub fn match_id(&self) -> Option<u64> {
        self.match_id
    }

    /// One player's ledger, by lobby slot.
    pub fn player(&self, slot: u32) -> Option<&PlayerBuckets> {
        self.players.get(&slot)
    }

    /// Every ledger, in slot order.
    pub fn players(&self) -> Vec<(u32, &PlayerBuckets)> {
        let mut out: Vec<(u32, &PlayerBuckets)> =
            self.players.iter().map(|(s, p)| (*s, p)).collect();
        out.sort_unstable_by_key(|(slot, _)| *slot);
        out
    }

    /// The match-level fight counts.
    pub fn fights(&self) -> MatchFights {
        self.fights
    }

    /// Fold one snapshot in and return what changed.
    ///
    /// A snapshot with no readable
    /// [`MatchClock::now`](deadlock_reader::snapshot::MatchClock) is ignored outright,
    /// state included: every number here is a difference between two positions on that
    /// clock, and a tick that cannot be placed in time can neither open nor close an
    /// interval.
    ///
    /// Rows with no lobby slot, and rows not on a playing side, are skipped: spectators
    /// have no scoreboard entry to attribute anything to and no side to be one half of a
    /// fight.
    ///
    /// An interval longer than [`MAX_TICK_GAP`] - or one whose clock went backwards -
    /// credits nothing to anybody and closes any open fight. The samples are still
    /// replaced, so the *next* interval is usable.
    pub fn update(&mut self, snap: &LiveSnapshot) -> Vec<ObjectiveContextEvent> {
        if snap.match_id != self.match_id {
            self.players.clear();
            self.fights = MatchFights::default();
            self.urn = FightRun::default();
            self.rift = FightRun::default();
            self.last_now = None;
            self.match_id = snap.match_id;
        }
        let Some(now) = snap.clock.now else {
            return Vec::new();
        };

        let urn_present = snap
            .timers
            .urn
            .as_ref()
            .and_then(|t| t.present)
            .unwrap_or(false);
        let rift_contested = self.rift_in_context(snap, now);

        let mut out = Vec::new();
        let mut rows: Vec<(u32, &PlayerRow, Team)> = snap
            .players
            .iter()
            .filter_map(|row| {
                let (Some(slot), Some(team)) = (row.slot, row.team) else {
                    return None;
                };
                team.is_playing().then_some((slot, row, team))
            })
            .collect();
        // Slot order, so the event stream does not depend on the order rows were walked.
        rows.sort_unstable_by_key(|(slot, _, _)| *slot);

        let mut exchanged: HashMap<Team, bool> = HashMap::new();
        // The match-level interval, shared by every row and by the fight detector. A row
        // that joined mid-interval still differences against its own previous sample.
        let interval = self.last_now.and_then(|prev| {
            let dt = now - prev;
            (0.0..=MAX_TICK_GAP).contains(&dt).then_some(dt)
        });
        self.last_now = Some(now);

        for (slot, row, team) in rows {
            let sample = Sample {
                now,
                bucket: row
                    .position
                    .map(|p| bucket_at(p, &snap.objectives, &self.radii)),
                alive: row.is_alive,
                has_rejuvenator: row.has_rejuvenator,
                kills: row.kills,
                deaths: row.deaths,
                assists: row.assists,
                hero_damage: row.hero_damage,
                objective_damage: row.objective_damage,
                healing: row.healing,
            };
            let player = self.players.entry(slot).or_default();
            let Some(prev) = player.last else {
                player.last = Some(sample);
                Self::track_visit(player, slot, &sample, &mut out);
                continue;
            };
            let dt = now - prev.now;
            if dt < 0.0 || dt > MAX_TICK_GAP {
                player.last = Some(sample);
                player.visit = None;
                Self::track_visit(player, slot, &sample, &mut out);
                continue;
            }

            let deltas = Deltas {
                kills: delta(prev.kills, sample.kills),
                deaths: delta(prev.deaths, sample.deaths),
                assists: delta(prev.assists, sample.assists),
                hero_damage: delta(prev.hero_damage, sample.hero_damage),
                objective_damage: delta(prev.objective_damage, sample.objective_damage),
                healing: delta(prev.healing, sample.healing),
            };
            if deltas.hero_damage > 0 {
                exchanged.insert(team, true);
            }
            if prev.has_rejuvenator == Some(false) && sample.has_rejuvenator == Some(true) {
                player.rejuv_secured += 1;
            }
            if urn_present && prev.alive == Some(true) && sample.alive == Some(true) {
                player.urn_seconds += dt;
            }

            match interval_counter_bucket(&prev, &sample) {
                Some(bucket) => {
                    player.attributed_intervals += 1;
                    if !deltas.is_empty() {
                        player.buckets.entry(bucket).or_default().add(&deltas);
                    }
                }
                None => {
                    player.unattributable_intervals += 1;
                    if !deltas.is_empty() {
                        player.unattributed.add(&deltas);
                    }
                }
            }
            if let Some(bucket) = interval_time_bucket(&prev, &sample) {
                player.buckets.entry(bucket).or_default().time_seconds += dt;
            }

            player.last = Some(sample);
            Self::track_visit(player, slot, &sample, &mut out);
        }

        let both_sides = exchanged.get(&Team::AMBER).copied().unwrap_or(false)
            && exchanged.get(&Team::SAPPHIRE).copied().unwrap_or(false);
        match interval {
            Some(dt) => {
                let prev_now = now - dt;
                self.urn.observe(
                    FightContext::Urn,
                    Exchange {
                        in_context: urn_present,
                        both_sides,
                        prev_now,
                        now,
                    },
                    &mut self.fights,
                    &mut out,
                );
                self.rift.observe(
                    FightContext::Rift,
                    Exchange {
                        in_context: rift_contested,
                        both_sides,
                        prev_now,
                        now,
                    },
                    &mut self.fights,
                    &mut out,
                );
            }
            // No usable interval anywhere: a polling gap, or the first tick of a match.
            // An open fight cannot be carried across it, because the exchange that would
            // have continued it was never sampled.
            None => {
                self.urn.close(FightContext::Urn, &mut out);
                self.rift.close(FightContext::Rift, &mut out);
            }
        }
        out
    }

    /// Whether a rift is running *and* somebody is on the point.
    ///
    /// [`RiftState::contested`](deadlock_reader::timers::RiftState::contested) is the
    /// gate `deadlock-reader` says to use; `scoring_team` alone holds a stale value long
    /// after a rift resolves, and `best_progress` was considered and rejected for the same
    /// reason - partial progress persists.
    ///
    /// The proximity half is skipped when the client does not report a cash-in location,
    /// which widens the gate rather than closing it. That is the safe direction: a rift
    /// the client says is contested is a rift, and refusing to count it because one
    /// optional field was unreadable would drop real fights.
    fn rift_in_context(&self, snap: &LiveSnapshot, now: f32) -> bool {
        if !snap.timers.rift_state.contested(Some(now)) {
            return false;
        }
        let Some(point) = snap.timers.rift_state.cash_in_location else {
            return true;
        };
        let radius_sq = self.radii.rift * self.radii.rift;
        snap.players
            .iter()
            .filter(|row| row.slot.is_some() && row.is_playing())
            .filter_map(|row| row.position)
            .any(|p| planar_distance_sq(p, point) <= radius_sq)
    }

    /// Open, extend or close this player's visit to a bucket.
    ///
    /// The visit's seconds run to the *last sighting*, not to the tick the departure was
    /// noticed on, so [`ObjectiveContextEvent::Left`]'s `seconds` equals exactly what the
    /// visit contributed to [`BucketTotals::time_seconds`].
    fn track_visit(
        player: &mut PlayerBuckets,
        slot: u32,
        sample: &Sample,
        out: &mut Vec<ObjectiveContextEvent>,
    ) {
        if let Some(visit) = player.visit {
            if sample.bucket == Some(visit.bucket) {
                player.visit = Some(Visit {
                    last_seen: sample.now,
                    ..visit
                });
                return;
            }
            out.push(ObjectiveContextEvent::Left {
                slot,
                bucket: visit.bucket,
                at: sample.now,
                seconds: (visit.last_seen - visit.since).max(0.0),
            });
            player.visit = None;
        }
        if let Some(bucket) = sample.bucket {
            out.push(ObjectiveContextEvent::Entered {
                slot,
                bucket,
                at: sample.now,
            });
            player.visit = Some(Visit {
                bucket,
                since: sample.now,
                last_seen: sample.now,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deadlock_reader::snapshot::MatchClock;
    use deadlock_reader::timers::{ObjectiveTimer, RiftState, TimerSource, Timers};

    const AMBER: Team = Team::AMBER;
    const SAPPHIRE: Team = Team::SAPPHIRE;

    /// Coordinates on the scale a live pawn actually returned, so the radii are being
    /// exercised against realistic magnitudes rather than against numbers near the origin.
    const MID: [f32; 3] = [11570.0, -6601.0, -82.0];

    fn offset(from: [f32; 3], dx: f32, dy: f32, dz: f32) -> [f32; 3] {
        [from[0] + dx, from[1] + dy, from[2] + dz]
    }

    fn objective(kind: ObjectiveKind, address: u64, position: [f32; 3]) -> Objective {
        Objective {
            kind,
            team_name: None,
            class: "C_NPC_MidBoss".into(),
            address,
            team: None,
            health: Some(1000),
            max_health: Some(1000),
            lane: None,
            state: StructureState::Standing,
            position: Some(position),
        }
    }

    /// One player row as the reader would report it, with every counter readable.
    #[derive(Clone, Copy, Debug)]
    struct P {
        slot: u32,
        team: Team,
        position: Option<[f32; 3]>,
        alive: bool,
        kills: u32,
        deaths: u32,
        hero_damage: u32,
        rejuv: bool,
    }

    impl P {
        fn new(slot: u32, team: Team, position: Option<[f32; 3]>) -> Self {
            P {
                slot,
                team,
                position,
                alive: true,
                kills: 0,
                deaths: 0,
                hero_damage: 0,
                rejuv: false,
            }
        }

        fn at(mut self, position: [f32; 3]) -> Self {
            self.position = Some(position);
            self
        }

        fn nowhere(mut self) -> Self {
            self.position = None;
            self
        }

        fn kills(mut self, kills: u32) -> Self {
            self.kills = kills;
            self
        }

        fn deaths(mut self, deaths: u32) -> Self {
            self.deaths = deaths;
            self
        }

        fn hero_damage(mut self, damage: u32) -> Self {
            self.hero_damage = damage;
            self
        }

        fn dead(mut self) -> Self {
            self.alive = false;
            self
        }

        fn rejuv(mut self) -> Self {
            self.rejuv = true;
            self
        }

        fn row(self) -> PlayerRow {
            PlayerRow {
                slot: Some(self.slot),
                team: Some(self.team),
                position: self.position,
                is_alive: Some(self.alive),
                has_rejuvenator: Some(self.rejuv),
                kills: Some(self.kills),
                deaths: Some(self.deaths),
                assists: Some(0),
                hero_damage: Some(self.hero_damage),
                objective_damage: Some(0),
                healing: Some(0),
                ..Default::default()
            }
        }
    }

    #[derive(Clone, Debug, Default)]
    struct Tick {
        now: f32,
        players: Vec<P>,
        objectives: Vec<Objective>,
        urn: bool,
        rift: Option<[f32; 3]>,
    }

    impl Tick {
        fn new(now: f32) -> Self {
            Tick {
                now,
                ..Default::default()
            }
        }

        fn with(mut self, players: &[P]) -> Self {
            self.players = players.to_vec();
            self
        }

        fn near(mut self, objectives: Vec<Objective>) -> Self {
            self.objectives = objectives;
            self
        }

        fn urn(mut self) -> Self {
            self.urn = true;
            self
        }

        fn rift(mut self, at: [f32; 3]) -> Self {
            self.rift = Some(at);
            self
        }

        fn snap(self) -> LiveSnapshot {
            let now = self.now;
            LiveSnapshot {
                match_id: Some(42),
                clock: MatchClock {
                    now: Some(now),
                    ..Default::default()
                },
                timers: Timers {
                    urn: Some(ObjectiveTimer {
                        present: Some(self.urn),
                        count: Some(usize::from(self.urn)),
                        next_spawn_in: None,
                        source: TimerSource::Unknown,
                    }),
                    rift_state: RiftState {
                        scoring_team: self.rift.map(|_| AMBER),
                        give_up_at: self.rift.map(|_| now + 30.0),
                        cash_in_location: self.rift,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                players: self.players.into_iter().map(P::row).collect(),
                objectives: self.objectives,
                ..Default::default()
            }
        }
    }

    fn midboss() -> Vec<Objective> {
        vec![objective(ObjectiveKind::Midboss, 0x1000, MID)]
    }

    fn totals(t: &ObjectiveContextTracker, slot: u32, bucket: ObjectiveBucket) -> BucketTotals {
        t.player(slot).map(|p| p.totals(bucket)).unwrap_or_default()
    }

    /// This module's rift radius is the same measured figure `deadlock-reader` carries.
    ///
    /// The two constants exist separately because this crate cannot reach a caller's
    /// `Tunables` - see [`ObjectiveContextTracker`] - not because they disagree. An
    /// earlier revision asserted the opposite, that they were deliberately different,
    /// which was correct only while [`RIFT_RADIUS`] was a guess. Pinning equality is what
    /// stops the guess creeping back.
    #[test]
    fn the_rift_radius_agrees_with_the_shipped_tunable() {
        assert_eq!(
            RIFT_RADIUS,
            deadlock_reader::tunables::DEFAULT_RIFT_RADIUS,
            "the two copies of the rift radius have drifted apart"
        );
    }

    /// The radius is metres converted, not a round number someone liked.
    ///
    /// Fails if the conversion is dropped (787.402 back to 20) or applied with a made-up
    /// factor such as 100, both of which look plausible in isolation.
    #[test]
    fn the_rift_radius_is_twenty_metres() {
        let meters = RIFT_RADIUS / deadlock_reader::tunables::UNITS_PER_METER;
        assert!(
            (meters - 20.0).abs() < 0.001,
            "RIFT_RADIUS is {meters} m, but m_flKothRadius is 20"
        );
    }

    /// The whole point of the module: a kill next to the Midboss is a Midboss kill.
    ///
    /// The failure this guards is a tracker that totals kills without consulting a
    /// position at all, which would put every kill in `open` and make the three buckets
    /// indistinguishable from one.
    #[test]
    fn a_kill_next_to_the_midboss_buckets_to_midboss_and_not_to_open() {
        let mut t = ObjectiveContextTracker::new();
        let near = offset(MID, 300.0, 200.0, 0.0);
        let p = P::new(1, AMBER, Some(near));

        t.update(&Tick::new(100.0).with(&[p]).near(midboss()).snap());
        t.update(&Tick::new(100.2).with(&[p.kills(1)]).near(midboss()).snap());

        assert_eq!(totals(&t, 1, ObjectiveBucket::Midboss).kills, 1);
        assert_eq!(
            totals(&t, 1, ObjectiveBucket::Open).kills,
            0,
            "a kill cannot be in two buckets"
        );
        assert_eq!(t.player(1).unwrap().unattributed().kills, 0);
    }

    /// The same kill, made far from anything, is an open-world kill.
    ///
    /// The mirror of the test above, and the failure it guards is the opposite one: a
    /// radius so large, or a check so absent, that everything reads as a Midboss fight.
    #[test]
    fn the_same_kill_made_far_from_the_midboss_buckets_to_open() {
        let mut t = ObjectiveContextTracker::new();
        let far = offset(MID, MIDBOSS_RADIUS + 500.0, 0.0, 0.0);
        let p = P::new(1, AMBER, Some(far));

        t.update(&Tick::new(100.0).with(&[p]).near(midboss()).snap());
        t.update(&Tick::new(100.2).with(&[p.kills(1)]).near(midboss()).snap());

        assert_eq!(totals(&t, 1, ObjectiveBucket::Open).kills, 1);
        assert_eq!(totals(&t, 1, ObjectiveBucket::Midboss).kills, 0);
    }

    /// A player whose position could not be read is placed nowhere, not in `open`.
    ///
    /// The failure this guards is the one the module exists to avoid: `open` used as a
    /// dumping ground for ignorance, so that a match where positions never read looks like
    /// a match fought entirely in the open.
    #[test]
    fn a_player_with_no_position_buckets_nowhere_rather_than_into_open() {
        let mut t = ObjectiveContextTracker::new();
        let p = P::new(1, AMBER, None);

        t.update(&Tick::new(100.0).with(&[p]).near(midboss()).snap());
        t.update(&Tick::new(100.2).with(&[p.kills(1)]).near(midboss()).snap());

        let player = t.player(1).unwrap();
        assert_eq!(
            player.totals(ObjectiveBucket::Open),
            BucketTotals::default()
        );
        assert_eq!(
            player.totals(ObjectiveBucket::Midboss),
            BucketTotals::default()
        );
        assert_eq!(player.unattributed().kills, 1);
        assert_eq!(player.unattributable_intervals(), 1);
        assert_eq!(player.attributed_intervals(), 0);
    }

    /// A player inside both zones is bucketed by rank, then by distance, then by address -
    /// deterministically, and the same way whichever order the objectives are listed in.
    ///
    /// The failure this guards is an attribution that depends on the order the entity list
    /// happened to be walked, which would make two runs over one match disagree.
    #[test]
    fn a_player_inside_two_zones_at_once_resolves_by_rank_then_distance_then_address() {
        let walker_at = offset(MID, 200.0, 0.0, 0.0);
        let here = offset(MID, 250.0, 0.0, 0.0);
        let boss = objective(ObjectiveKind::Midboss, 0x2000, MID);
        let walker = objective(ObjectiveKind::Walker, 0x1000, walker_at);

        for order in [
            vec![boss.clone(), walker.clone()],
            vec![walker.clone(), boss.clone()],
        ] {
            let mut t = ObjectiveContextTracker::new();
            let p = P::new(1, AMBER, Some(here));
            t.update(&Tick::new(100.0).with(&[p]).near(order.clone()).snap());
            t.update(&Tick::new(100.2).with(&[p.kills(1)]).near(order).snap());
            assert_eq!(
                totals(&t, 1, ObjectiveBucket::Midboss).kills,
                1,
                "the Midboss outranks a nearer walker"
            );
            assert_eq!(totals(&t, 1, ObjectiveBucket::Walker).kills, 0);
        }

        let left = objective(ObjectiveKind::Walker, 0x1000, offset(MID, -100.0, 0.0, 0.0));
        let right = objective(ObjectiveKind::Walker, 0x2000, offset(MID, 100.0, 0.0, 0.0));
        for order in [
            vec![left.clone(), right.clone()],
            vec![right.clone(), left.clone()],
        ] {
            let p = P::new(1, AMBER, Some(MID));
            let mut t = ObjectiveContextTracker::new();
            t.update(&Tick::new(100.0).with(&[p]).near(order.clone()).snap());
            t.update(&Tick::new(100.2).with(&[p.kills(1)]).near(order).snap());
            assert_eq!(totals(&t, 1, ObjectiveBucket::Walker).kills, 1);
        }
    }

    /// Time is credited only for intervals whose two ends agree on where the player was.
    ///
    /// The failure this guards is a tracker that credits the current tick's bucket for the
    /// whole interval: a player who walks past the Midboss once would be credited the
    /// entire gap since the last poll as time spent there.
    #[test]
    fn time_accumulates_only_while_the_player_is_actually_near_the_objective() {
        let mut t = ObjectiveContextTracker::new();
        let near = offset(MID, 100.0, 0.0, 0.0);
        let away = offset(MID, MIDBOSS_RADIUS + 1000.0, 0.0, 0.0);
        let p = P::new(1, AMBER, Some(away));

        for (now, at) in [
            (100.0, away),
            (100.5, near),
            (101.0, near),
            (101.5, near),
            (102.0, away),
        ] {
            t.update(&Tick::new(now).with(&[p.at(at)]).near(midboss()).snap());
        }

        let midboss_seconds = totals(&t, 1, ObjectiveBucket::Midboss).time_seconds;
        assert!(
            (midboss_seconds - 1.0).abs() < 1e-4,
            "expected the two whole intervals inside the zone, got {midboss_seconds}"
        );
        let open_seconds = totals(&t, 1, ObjectiveBucket::Open).time_seconds;
        assert_eq!(
            open_seconds, 0.0,
            "the entry and exit intervals belong to neither bucket"
        );
    }

    /// A visit's reported seconds are exactly what it contributed to the bucket's total.
    ///
    /// The failure this guards is an event stream that cannot be reconciled with the
    /// totals: measuring the visit to the tick the departure was *noticed* on would make
    /// `Left.seconds` one poll interval longer than the time actually credited.
    #[test]
    fn a_visits_reported_seconds_equal_what_it_added_to_the_buckets_total() {
        let mut t = ObjectiveContextTracker::new();
        let near = offset(MID, 100.0, 0.0, 0.0);
        let away = offset(MID, MIDBOSS_RADIUS + 1000.0, 0.0, 0.0);
        let p = P::new(1, AMBER, Some(away));

        let mut left_seconds = None;
        for (now, at) in [(100.0, away), (100.5, near), (101.0, near), (101.5, away)] {
            for event in t.update(&Tick::new(now).with(&[p.at(at)]).near(midboss()).snap()) {
                if let ObjectiveContextEvent::Left {
                    bucket: ObjectiveBucket::Midboss,
                    seconds,
                    ..
                } = event
                {
                    left_seconds = Some(seconds);
                }
            }
        }

        let credited = totals(&t, 1, ObjectiveBucket::Midboss).time_seconds;
        assert!(
            (left_seconds.expect("a Left event") - credited).abs() < 1e-4,
            "visit reported {left_seconds:?}, bucket credited {credited}"
        );
    }

    /// A player who dies keeps the bucket they were standing in.
    ///
    /// The failure this guards is the one the death exception exists for: a dead player
    /// has no pawn and so no position, so the strict rule alone would leave every
    /// `midboss.deaths` and `walker.deaths` at zero while `unattributed.deaths` held the
    /// whole match.
    #[test]
    fn a_death_is_credited_to_where_the_player_was_standing_when_they_died() {
        let mut t = ObjectiveContextTracker::new();
        let near = offset(MID, 100.0, 0.0, 0.0);
        let p = P::new(1, AMBER, Some(near));

        t.update(&Tick::new(100.0).with(&[p]).near(midboss()).snap());
        t.update(
            &Tick::new(100.2)
                .with(&[p.deaths(1).dead().nowhere()])
                .near(midboss())
                .snap(),
        );

        assert_eq!(totals(&t, 1, ObjectiveBucket::Midboss).deaths, 1);
        assert_eq!(
            totals(&t, 1, ObjectiveBucket::Midboss).time_seconds,
            0.0,
            "the exception credits the event, not the time after it"
        );
        assert_eq!(t.player(1).unwrap().unattributed().deaths, 0);
    }

    /// A player who merely vanishes from view keeps nothing.
    ///
    /// The pair to the test above, and the reason the exception is narrow: only a
    /// *readable* death explains a missing position. Without this distinction the
    /// exception would become "use the last bucket whenever the position is missing",
    /// which is the `open`-as-dumping-ground failure wearing a different hat.
    #[test]
    fn a_player_whose_position_simply_stops_reading_is_not_treated_as_having_died_there() {
        let mut t = ObjectiveContextTracker::new();
        let near = offset(MID, 100.0, 0.0, 0.0);
        let p = P::new(1, AMBER, Some(near));

        t.update(&Tick::new(100.0).with(&[p]).near(midboss()).snap());
        t.update(
            &Tick::new(100.2)
                .with(&[p.kills(1).nowhere()])
                .near(midboss())
                .snap(),
        );

        assert_eq!(totals(&t, 1, ObjectiveBucket::Midboss).kills, 0);
        assert_eq!(t.player(1).unwrap().unattributed().kills, 1);
    }

    /// A destroyed walker stops being a place.
    ///
    /// The failure this guards is bucketing fights over rubble: once a walker is down, the
    /// ground it stood on is open map, and continuing to credit `walker.*` there would
    /// inflate that bucket for the rest of the match.
    #[test]
    fn a_destroyed_walker_no_longer_buckets_the_fight_around_it() {
        let mut t = ObjectiveContextTracker::new();
        let here = offset(MID, 100.0, 0.0, 0.0);
        let p = P::new(1, AMBER, Some(here));
        let mut down = objective(ObjectiveKind::Walker, 0x1000, MID);
        down.state = StructureState::Destroyed;

        t.update(&Tick::new(100.0).with(&[p]).near(vec![down.clone()]).snap());
        t.update(&Tick::new(100.2).with(&[p.kills(1)]).near(vec![down]).snap());

        assert_eq!(totals(&t, 1, ObjectiveBucket::Walker).kills, 0);
        assert_eq!(totals(&t, 1, ObjectiveBucket::Open).kills, 1);
    }

    /// A structure nothing could be read from is still a place.
    ///
    /// The failure this guards is a failed read looking exactly like a demolished
    /// building, which is the same mistake `StructureState::Unknown` exists in
    /// `deadlock-reader` to prevent.
    #[test]
    fn a_structure_whose_state_is_unknown_still_buckets_the_fight_around_it() {
        let mut t = ObjectiveContextTracker::new();
        let here = offset(MID, 100.0, 0.0, 0.0);
        let p = P::new(1, AMBER, Some(here));
        let mut unknown = objective(ObjectiveKind::Walker, 0x1000, MID);
        unknown.state = StructureState::Unknown;

        t.update(
            &Tick::new(100.0)
                .with(&[p])
                .near(vec![unknown.clone()])
                .snap(),
        );
        t.update(
            &Tick::new(100.2)
                .with(&[p.kills(1)])
                .near(vec![unknown])
                .snap(),
        );

        assert_eq!(totals(&t, 1, ObjectiveBucket::Walker).kills, 1);
    }

    /// Vertical separation does not move a player out of a **structure's** bucket.
    ///
    /// The failure this guards is a spherical test applied to a walker: a player on the
    /// walkway above one, or in the lane beneath it, differs from its origin mostly in `Z`,
    /// and a 3D radius would push exactly those fights into `open`.
    ///
    /// It used to assert this of the **Midboss**, which was wrong and a real match showed
    /// it: the Midboss is underground with blue lane running over the pit, so "above it" is
    /// a different place rather than the same place higher up. See
    /// `a_player_in_the_lane_above_the_midboss_pit_is_not_at_the_midboss` and
    /// `measured_through_the_air`. The reasoning was always about tall structures; it is
    /// now applied to one.
    #[test]
    fn a_player_directly_above_a_structure_is_still_at_it() {
        let mut t = ObjectiveContextTracker::new();
        let overhead = offset(MID, 50.0, 50.0, 4000.0);
        let p = P::new(1, AMBER, Some(overhead));
        let walker = vec![objective(ObjectiveKind::Walker, 0x1000, MID)];

        t.update(&Tick::new(100.0).with(&[p]).near(walker.clone()).snap());
        t.update(&Tick::new(100.2).with(&[p.kills(1)]).near(walker).snap());

        assert_eq!(totals(&t, 1, ObjectiveBucket::Walker).kills, 1);
    }

    /// The walker radius is the shipped sight range, and the boundary is where it says.
    ///
    /// Nothing pinned this before: every walker test used a distance far inside the radius
    /// or far outside it, so narrowing the constant from a guessed `1200` to the shipped
    /// `944` changed no test at all. A radius nothing tests at its edge is a number, not a
    /// behaviour.
    ///
    /// 1000 units is the interesting distance - inside the old guess, outside the shipped
    /// range - so this test fails against either a stale constant or a lost source.
    #[test]
    fn the_walker_radius_is_the_shipped_sight_range_and_bounds_attribution() {
        assert_eq!(
            WALKER_RADIUS,
            deadlock_reader::tunables::DEFAULT_WALKER_SIGHT_RADIUS,
            "the radius stopped tracking the shipped sight range"
        );

        let mut inside = ObjectiveContextTracker::new();
        let near = P::new(1, AMBER, Some(offset(MID, 900.0, 0.0, 0.0)));
        inside.update(
            &Tick::new(100.0)
                .with(&[near])
                .near(vec![objective(ObjectiveKind::Walker, 0x1000, MID)])
                .snap(),
        );
        inside.update(
            &Tick::new(100.2)
                .with(&[near.kills(1)])
                .near(vec![objective(ObjectiveKind::Walker, 0x1000, MID)])
                .snap(),
        );
        assert_eq!(
            totals(&inside, 1, ObjectiveBucket::Walker).kills,
            1,
            "900 units is inside the shipped 944 and must attribute"
        );

        let mut outside = ObjectiveContextTracker::new();
        let far = P::new(1, AMBER, Some(offset(MID, 1000.0, 0.0, 0.0)));
        outside.update(
            &Tick::new(100.0)
                .with(&[far])
                .near(vec![objective(ObjectiveKind::Walker, 0x1000, MID)])
                .snap(),
        );
        outside.update(
            &Tick::new(100.2)
                .with(&[far.kills(1)])
                .near(vec![objective(ObjectiveKind::Walker, 0x1000, MID)])
                .snap(),
        );
        assert_eq!(
            totals(&outside, 1, ObjectiveBucket::Walker).kills,
            0,
            "1000 units is outside the shipped 944; the old guess of 1200 would attribute it"
        );
    }

    /// The Midboss radius is the shipped sight range, and its boundary is where it says.
    ///
    /// The sibling of the walker test, and it exists for the same reason: every Midboss
    /// test placed the player at the objective or directly above it, so the radius could
    /// have been almost any value and nothing would have noticed. Deliberately moving the
    /// constant to `800` failed no test until this one existed.
    #[test]
    fn the_midboss_radius_is_the_shipped_sight_range_and_bounds_attribution() {
        assert_eq!(
            MIDBOSS_RADIUS,
            deadlock_reader::tunables::DEFAULT_MIDBOSS_SIGHT_RADIUS,
            "the radius stopped tracking the shipped sight range"
        );

        let mut inside = ObjectiveContextTracker::new();
        let near = P::new(1, AMBER, Some(offset(MID, 1400.0, 0.0, 0.0)));
        inside.update(&Tick::new(100.0).with(&[near]).near(midboss()).snap());
        inside.update(
            &Tick::new(100.2)
                .with(&[near.kills(1)])
                .near(midboss())
                .snap(),
        );
        assert_eq!(
            totals(&inside, 1, ObjectiveBucket::Midboss).kills,
            1,
            "1400 units is inside the shipped 1500 and must attribute"
        );

        let mut outside = ObjectiveContextTracker::new();
        let far = P::new(1, AMBER, Some(offset(MID, 1600.0, 0.0, 0.0)));
        outside.update(&Tick::new(100.0).with(&[far]).near(midboss()).snap());
        outside.update(
            &Tick::new(100.2)
                .with(&[far.kills(1)])
                .near(midboss())
                .snap(),
        );
        assert_eq!(
            totals(&outside, 1, ObjectiveBucket::Midboss).kills,
            0,
            "1600 units is outside the shipped 1500"
        );
    }

    /// Blue lane runs over the Midboss pit, and being above it is not being at it.
    ///
    /// The Midboss sits **underground**. Measured in match `100650421`, it is at
    /// `z = -768` while players in the lane above it are at `z = +376` — 1144 units, some
    /// 29 metres, straight up. A horizontal-only test put three of them inside
    /// [`MIDBOSS_RADIUS`] at 1228, 1333 and 2119 units of ground distance, and they
    /// accumulated 70-90% of a twenty-second window in the `midboss` bucket while the
    /// Midboss sat at full health, untouched.
    ///
    /// Their **3D** distances are 1679, 1757 and 2473. All three are outside the same 1500
    /// radius, which is why this needs no new constant: `m_flSightRangePlayers` is what an
    /// NPC can see, and sight is not measured along the floor.
    ///
    /// A player actually in the pit is level with it, so their vertical offset is small and
    /// the two measures agree. Only the lane overhead separates them.
    #[test]
    fn a_player_in_the_lane_above_the_midboss_pit_is_not_at_the_midboss() {
        let mut t = ObjectiveContextTracker::new();

        let overhead = offset(MID, 1228.0, 0.0, 1144.0);
        let p = P::new(1, AMBER, Some(overhead));
        t.update(&Tick::new(100.0).with(&[p]).near(midboss()).snap());
        t.update(&Tick::new(100.2).with(&[p.kills(1)]).near(midboss()).snap());
        assert_eq!(
            totals(&t, 1, ObjectiveBucket::Midboss).kills,
            0,
            "a player 1144 units above the pit is in the lane, not at the Midboss"
        );
        assert_eq!(totals(&t, 1, ObjectiveBucket::Open).kills, 1);

        let mut u = ObjectiveContextTracker::new();
        let in_pit = offset(MID, 1228.0, 0.0, 40.0);
        let q = P::new(1, AMBER, Some(in_pit));
        u.update(&Tick::new(100.0).with(&[q]).near(midboss()).snap());
        u.update(&Tick::new(100.2).with(&[q.kills(1)]).near(midboss()).snap());
        assert_eq!(
            totals(&u, 1, ObjectiveBucket::Midboss).kills,
            1,
            "level with the pit and inside the radius is at the Midboss"
        );
    }

    /// Two sides trading damage while an Urn is up is one Urn fight, not one per tick.
    #[test]
    fn a_run_of_two_sided_exchanges_while_the_urn_is_up_counts_as_one_fight() {
        let mut t = ObjectiveContextTracker::new();
        let a = P::new(1, AMBER, Some(MID));
        let b = P::new(7, SAPPHIRE, Some(MID));

        t.update(&Tick::new(100.0).with(&[a, b]).urn().snap());
        for (i, now) in [100.2_f32, 100.4, 100.6].iter().enumerate() {
            let bump = (i as u32 + 1) * 100;
            t.update(
                &Tick::new(*now)
                    .with(&[a.hero_damage(bump), b.hero_damage(bump)])
                    .urn()
                    .snap(),
            );
        }

        assert_eq!(t.fights().urn_fights, 1);
        assert!(
            (t.fights().urn_fight_seconds - 0.4).abs() < 1e-4,
            "seconds accumulate per exchange after the one that opened the fight, got {}",
            t.fights().urn_fight_seconds
        );
    }

    /// One side hitting the other with no reply is not a fight.
    ///
    /// This is the rule's most important stated failure - a clean pick at the Urn is
    /// exactly the event most worth counting - and it is tested so that it is a known
    /// property rather than a surprise.
    #[test]
    fn a_one_sided_gank_is_not_counted_as_a_fight_by_the_two_sided_rule() {
        let mut t = ObjectiveContextTracker::new();
        let a = P::new(1, AMBER, Some(MID));
        let b = P::new(7, SAPPHIRE, Some(MID));

        t.update(&Tick::new(100.0).with(&[a, b]).urn().snap());
        t.update(&Tick::new(100.2).with(&[a.hero_damage(900), b]).urn().snap());

        assert_eq!(t.fights().urn_fights, 0);
    }

    /// The same exchange with no Urn on the map is no Urn fight.
    #[test]
    fn an_exchange_with_no_urn_on_the_map_is_not_an_urn_fight() {
        let mut t = ObjectiveContextTracker::new();
        let a = P::new(1, AMBER, Some(MID));
        let b = P::new(7, SAPPHIRE, Some(MID));

        t.update(&Tick::new(100.0).with(&[a, b]).snap());
        t.update(
            &Tick::new(100.2)
                .with(&[a.hero_damage(100), b.hero_damage(100)])
                .snap(),
        );

        assert_eq!(t.fights().urn_fights, 0);
    }

    /// The two timing constants hold their chosen values, and their relationship.
    ///
    /// Every other test here is written in terms of these constants - `FIGHT_GAP_SECONDS +
    /// 1.0`, `MAX_TICK_GAP + 1.0` - which is right for checking the *mechanism*, because
    /// the test then follows the constant wherever it goes. The cost is that the values
    /// themselves are unpinned: widening `FIGHT_GAP_SECONDS` from 8 to 60, or `MAX_TICK_GAP`
    /// from 1 to 30, changed no test at all. Both were measured that way.
    ///
    /// Unlike the radii, these have no source in the game; they are judgements, documented
    /// on the constants. Pinning them does not make them right - it makes changing them
    /// deliberate, which is the most a judgement can be asked to be.
    ///
    /// The ordering is the part that is not merely a restatement. `MAX_TICK_GAP` bounds how
    /// stale one interval may be, and `FIGHT_GAP_SECONDS` how long a fight may go quiet. If
    /// the first ever approached the second, a single stale interval could span an entire
    /// lull and two separate fights would silently become one.
    #[test]
    fn the_timing_constants_hold_their_values_and_their_ordering() {
        assert_eq!(
            FIGHT_GAP_SECONDS, 8.0,
            "the fight gap moved; was that deliberate?"
        );
        assert_eq!(
            MAX_TICK_GAP, 1.0,
            "the tick gap moved; was that deliberate?"
        );
    }

    /// Two exchanges separated by more than the gap are two fights.
    ///
    /// The failure this guards is a detector with no gap at all, which would count one
    /// fight per match for as long as the Urn kept respawning.
    #[test]
    fn two_exchanges_further_apart_than_the_gap_are_two_separate_fights() {
        let mut t = ObjectiveContextTracker::new();
        let a = P::new(1, AMBER, Some(MID));
        let b = P::new(7, SAPPHIRE, Some(MID));
        let mut damage = 0;

        let mut tick = |t: &mut ObjectiveContextTracker, now: f32, fight: bool| {
            if fight {
                damage += 100;
            }
            t.update(
                &Tick::new(now)
                    .with(&[a.hero_damage(damage), b.hero_damage(damage)])
                    .urn()
                    .snap(),
            );
        };

        tick(&mut t, 100.0, false);
        tick(&mut t, 100.2, true);
        let mut now = 100.2_f32;
        while now < 100.2 + FIGHT_GAP_SECONDS + 1.0 {
            now += 0.5;
            tick(&mut t, now, false);
        }
        tick(&mut t, now + 0.2, true);

        assert_eq!(t.fights().urn_fights, 2);
    }

    /// A contested rift with somebody on the point makes an exchange a rift fight.
    #[test]
    fn an_exchange_inside_a_contested_rift_counts_as_a_rift_fight() {
        let mut t = ObjectiveContextTracker::new();
        let a = P::new(1, AMBER, Some(MID));
        let b = P::new(7, SAPPHIRE, Some(MID));

        t.update(&Tick::new(100.0).with(&[a, b]).rift(MID).snap());
        t.update(
            &Tick::new(100.2)
                .with(&[a.hero_damage(100), b.hero_damage(100)])
                .rift(MID)
                .snap(),
        );

        assert_eq!(t.fights().rift_fights, 1);
        assert_eq!(t.fights().urn_fights, 0);
    }

    /// A contested rift nobody is standing on does not gate a fight.
    ///
    /// This is what [`RIFT_RADIUS`] actually does, and the failure it guards is a
    /// `rift_fights` that counts every skirmish anywhere on the map for as long as a rift
    /// happens to be running.
    #[test]
    fn an_exchange_far_from_the_cash_in_point_is_not_a_rift_fight() {
        let mut t = ObjectiveContextTracker::new();
        let far = offset(MID, RIFT_RADIUS * 4.0, 0.0, 0.0);
        let a = P::new(1, AMBER, Some(far));
        let b = P::new(7, SAPPHIRE, Some(far));

        t.update(&Tick::new(100.0).with(&[a, b]).rift(MID).snap());
        t.update(
            &Tick::new(100.2)
                .with(&[a.hero_damage(100), b.hero_damage(100)])
                .rift(MID)
                .snap(),
        );

        assert_eq!(t.fights().rift_fights, 0);
    }

    /// Urn seconds are presence-and-alive, and stop when the player dies.
    ///
    /// Named for what it is: this is not the proximity metric the other buckets are, and
    /// the test asserts the weaker rule the module documents rather than pretending to the
    /// stronger one there is no Urn position to support.
    #[test]
    fn urn_seconds_count_time_alive_while_an_urn_is_up_and_nothing_more() {
        let mut t = ObjectiveContextTracker::new();
        let p = P::new(1, AMBER, Some(MID));

        t.update(&Tick::new(100.0).with(&[p]).urn().snap());
        t.update(&Tick::new(100.5).with(&[p]).urn().snap());
        t.update(&Tick::new(101.0).with(&[p.dead().nowhere()]).urn().snap());
        t.update(&Tick::new(101.5).with(&[p]).snap());

        let seconds = t.player(1).unwrap().urn_seconds();
        assert!(
            (seconds - 0.5).abs() < 1e-4,
            "only the interval alive with the urn up counts, got {seconds}"
        );
    }

    /// The Rejuvenator is counted on its rising edge and reported under `midboss.`.
    #[test]
    fn picking_up_the_rejuvenator_is_a_midboss_secure_wherever_it_happened() {
        let mut t = ObjectiveContextTracker::new();
        let far = offset(MID, MIDBOSS_RADIUS * 5.0, 0.0, 0.0);
        let p = P::new(1, AMBER, Some(far));

        t.update(&Tick::new(100.0).with(&[p]).near(midboss()).snap());
        t.update(&Tick::new(100.2).with(&[p.rejuv()]).near(midboss()).snap());
        t.update(&Tick::new(100.4).with(&[p.rejuv()]).near(midboss()).snap());

        assert_eq!(t.player(1).unwrap().rejuv_secured(), 1);
        let metrics = t.player(1).unwrap().metrics();
        assert!(metrics.contains(&("midboss.rejuv_secured".to_owned(), 1.0)));
    }

    /// A counter that could not be read contributes nothing, in either direction.
    ///
    /// Added after a deliberate break: replacing the both-endpoints-readable rule with
    /// `unwrap_or(0)` on each side broke nothing in the suite, which meant the guard was
    /// untested. The failure it prevents is severe and quiet - one failed read of
    /// `m_iKills` mid-match, and the next successful read arrives as a delta the size of
    /// the player's whole match, dumped into whichever bucket they happened to be in.
    #[test]
    fn a_counter_that_could_not_be_read_contributes_nothing_at_either_end() {
        let mut t = ObjectiveContextTracker::new();
        let p = P::new(1, AMBER, Some(MID));
        let mut blind = p.kills(5).row();
        blind.kills = None;

        t.update(&Tick::new(100.0).with(&[p.kills(5)]).snap());
        let mut middle = Tick::new(100.2).snap();
        middle.players = vec![blind];
        t.update(&middle);
        t.update(&Tick::new(100.4).with(&[p.kills(5)]).snap());

        assert_eq!(
            totals(&t, 1, ObjectiveBucket::Open).kills,
            0,
            "an unreadable endpoint is not a zero to difference against"
        );
        assert_eq!(t.player(1).unwrap().unattributed().kills, 0);
    }

    /// A lull shorter than the gap does not split one fight into two.
    ///
    /// Added after a deliberate break: cutting the tolerance to zero, so that any quiet
    /// tick ends the fight, broke nothing in the suite. Every tick of the one-fight test
    /// carried an exchange, so it never reached the closing branch at all - the tolerance
    /// that [`FIGHT_GAP_SECONDS`] exists to provide was not covered by anything.
    #[test]
    fn a_lull_shorter_than_the_gap_does_not_split_one_fight_into_two() {
        let mut t = ObjectiveContextTracker::new();
        let a = P::new(1, AMBER, Some(MID));
        let b = P::new(7, SAPPHIRE, Some(MID));

        t.update(&Tick::new(100.0).with(&[a, b]).urn().snap());
        t.update(
            &Tick::new(100.2)
                .with(&[a.hero_damage(100), b.hero_damage(100)])
                .urn()
                .snap(),
        );
        let mut now = 100.2_f32;
        for _ in 0..4 {
            now += 0.5;
            t.update(
                &Tick::new(now)
                    .with(&[a.hero_damage(100), b.hero_damage(100)])
                    .urn()
                    .snap(),
            );
        }
        t.update(
            &Tick::new(now + 0.2)
                .with(&[a.hero_damage(200), b.hero_damage(200)])
                .urn()
                .snap(),
        );

        assert_eq!(
            t.fights().urn_fights,
            1,
            "a pause in the trading is part of the fight, not the end of it"
        );
    }

    /// A counter that falls contributes nothing.
    ///
    /// The failure this guards is the post-game scoreboard, or a counter reset, arriving
    /// as an enormous negative that a wrapping subtraction would turn into billions of
    /// kills.
    #[test]
    fn a_counter_that_goes_backwards_contributes_nothing_rather_than_wrapping() {
        let mut t = ObjectiveContextTracker::new();
        let p = P::new(1, AMBER, Some(MID));

        t.update(&Tick::new(100.0).with(&[p.kills(9)]).snap());
        t.update(&Tick::new(100.2).with(&[p.kills(0)]).snap());

        assert_eq!(totals(&t, 1, ObjectiveBucket::Open).kills, 0);
        assert_eq!(t.player(1).unwrap().unattributed().kills, 0);
    }

    /// An interval longer than the poll gap allows credits nothing.
    ///
    /// The failure this guards is a fabricated attribution: across a ten-second stall a
    /// player can enter and leave three zones, and crediting the whole stall to whichever
    /// bucket happened to hold at both ends invents a fight that was never observed.
    #[test]
    fn an_interval_longer_than_the_maximum_tick_gap_credits_nothing_to_anybody() {
        let mut t = ObjectiveContextTracker::new();
        let p = P::new(1, AMBER, Some(MID));

        t.update(&Tick::new(100.0).with(&[p]).near(midboss()).snap());
        t.update(
            &Tick::new(100.0 + MAX_TICK_GAP + 1.0)
                .with(&[p.kills(4)])
                .near(midboss())
                .snap(),
        );

        assert_eq!(
            totals(&t, 1, ObjectiveBucket::Midboss),
            BucketTotals::default()
        );
        assert_eq!(t.player(1).unwrap().unattributed(), BucketTotals::default());
    }

    /// A snapshot whose clock could not be read changes nothing at all.
    ///
    /// Every number here is a difference on that clock, so a tick that cannot be placed in
    /// time must not be allowed to close an interval or open a visit.
    #[test]
    fn a_snapshot_with_no_readable_clock_is_ignored_state_and_all() {
        let mut t = ObjectiveContextTracker::new();
        let p = P::new(1, AMBER, Some(MID));

        t.update(&Tick::new(100.0).with(&[p]).near(midboss()).snap());
        let mut blind = Tick::new(0.0).with(&[p.kills(3)]).near(midboss()).snap();
        blind.clock.now = None;
        assert!(t.update(&blind).is_empty());
        t.update(&Tick::new(100.2).with(&[p.kills(3)]).near(midboss()).snap());

        assert_eq!(
            totals(&t, 1, ObjectiveBucket::Midboss).kills,
            3,
            "the blind tick neither consumed nor invented the delta"
        );
    }

    /// A new match id wipes every ledger, per-player and match-level alike.
    #[test]
    fn a_change_of_match_id_clears_the_ledgers_and_the_fight_counts() {
        let mut t = ObjectiveContextTracker::new();
        let a = P::new(1, AMBER, Some(MID));
        let b = P::new(7, SAPPHIRE, Some(MID));

        t.update(&Tick::new(100.0).with(&[a, b]).urn().snap());
        t.update(
            &Tick::new(100.2)
                .with(&[a.hero_damage(100).kills(1), b.hero_damage(100)])
                .urn()
                .snap(),
        );
        assert_eq!(t.fights().urn_fights, 1);

        let mut next = Tick::new(10.0).with(&[a, b]).urn().snap();
        next.match_id = Some(43);
        t.update(&next);

        assert_eq!(t.fights(), MatchFights::default());
        assert_eq!(
            totals(&t, 1, ObjectiveBucket::Open),
            BucketTotals::default()
        );
        assert_eq!(t.match_id(), Some(43));
    }

    /// Spectators are not players and cannot be one half of a fight.
    #[test]
    fn rows_with_no_slot_or_no_playing_side_are_skipped_entirely() {
        let mut t = ObjectiveContextTracker::new();
        let mut spectator = P::new(1, AMBER, Some(MID)).row();
        spectator.team = Some(Team::SPECTATOR);
        let mut slotless = P::new(2, AMBER, Some(MID)).row();
        slotless.slot = None;

        let mut snap = Tick::new(100.0).snap();
        snap.players = vec![spectator, slotless];
        t.update(&snap);

        assert!(t.players().is_empty());
    }

    /// The emitted key set is the one §5.3 quotes, in this crate's snake case.
    ///
    /// The failure this guards is a silent rename: a consumer joining these against
    /// Companion's output matches on the key, so a bucket that stops emitting one of its
    /// seven names disappears without an error.
    #[test]
    fn the_metric_keys_are_the_ones_the_gap_analysis_quotes() {
        let mut t = ObjectiveContextTracker::new();
        let p = P::new(1, AMBER, Some(MID));
        t.update(&Tick::new(100.0).with(&[p]).snap());

        let keys: Vec<String> = t
            .player(1)
            .unwrap()
            .metrics()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        for bucket in ["open", "midboss", "walker"] {
            for stat in [
                "kills",
                "deaths",
                "assists",
                "hero_damage",
                "objective_damage",
                "healing",
                "time_seconds",
            ] {
                assert!(
                    keys.contains(&format!("{bucket}.{stat}")),
                    "missing {bucket}.{stat}"
                );
            }
        }
        assert!(keys.contains(&"midboss.rejuv_secured".to_owned()));
        assert!(keys.contains(&"urn.time_seconds".to_owned()));
        assert!(!keys.contains(&"walker.rejuv_secured".to_owned()));

        let fights: Vec<String> = t.fights().metrics().into_iter().map(|(k, _)| k).collect();
        assert_eq!(fights, ["urn_fights", "urn_fight_seconds", "rift_fights"]);
    }
}
