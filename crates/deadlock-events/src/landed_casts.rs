//! Attributing landed casts to the ability that landed them.
//!
//! Companion's per-match JSON carries, per ability, four numbers:
//!
//! ```text
//! "ability.hunger.landedCasts": 51, "ability.hunger.enemiesHit": 73,
//! "ability.hunger.maxEnemiesInCast": 4,
//! "ability.hunger.avgEnemiesPerLandedCast": 1.4313725490196079
//! ```
//!
//! # The grouping is inferred, not observed
//!
//! That *shape* was read off Companion's own output files, so it is known. **How it is
//! computed was never traced** - no call site, no disassembly, nothing. Everything below
//! is inferred, and this module claims no more than that. What it implements is this
//! inference:
//!
//! > group modifier applications sharing an `m_hAbility` within an `m_flCreationTime`
//! > window across distinct targets. One group is one landed cast; its size is
//! > `enemiesInCast`.
//!
//! So every number out of here is a plausible reconstruction of a number Companion
//! publishes, not a number the game reports. Nothing here has been checked against a live
//! match beside Companion's own output, and until it has, a disagreement is evidence about
//! this module rather than about Companion.
//!
//! # What counts as landed
//!
//! A modifier applied to an **enemy** is the signal. An ability that goes off and touches
//! nobody leaves nothing on anybody's pawn, and an ability that only buffs its own caster
//! is not a landed cast in Companion's sense - otherwise every self-buff would read as a
//! cast that landed on nobody.
//!
//! Deciding "enemy" needs the applying side, and that is the weakest link here. The
//! modifier sits on the *target's* pawn and carries `m_iTeam`, which this module reads as
//! the **applying** side and compares against the target row's own
//! [`PlayerRow::team`](deadlock_reader::snapshot::PlayerRow::team). That reading is
//! inferred as well. Its failure direction is worth stating, because it is the safe one:
//! if `m_iTeam` should turn out to be the *target's* team, it equals the row's team on
//! every application, nothing is ever an enemy hit, and every total here reads zero. A
//! visible zero is recoverable; a silently inflated count would not be.
//!
//! # What identifies one cast
//!
//! `(m_hCaster, m_hAbility, creation-time window)`.
//!
//! An ability in Source 2 is its own entity and each hero owns its own instances, so
//! `m_hAbility` should already differ between two players casting the same ability
//! *class*: there is no shared "Bebop's hook" object for them to collide on. That is a
//! premise about the engine rather than something this crate verified, which is why the
//! caster handle is part of the key rather than merely carried alongside it. A ledger
//! keyed by caster cannot merge two players' casts even if the premise is wrong.
//!
//! Windows are anchored on the first application of a group rather than laid out on a
//! fixed grid of `floor(creation_time / window)`. A grid splits any cast that happens to
//! straddle a boundary, and which casts those are depends on nothing but the arbitrary
//! phase of the grid.
//!
//! # Naming the ability
//!
//! Plan item 6.3 asked for `m_hAbility` to be resolved through an `EntitySnapshot` to an
//! ability **class** and then to a display name. That is not the route taken here, and it
//! is not a route that could have worked - see the last two rows of the table below.
//!
//! What works is [`Modifier::subclass_id`], read from `m_nAbilitySubclassID`. It names the
//! ability that *applied* the modifier: it equals that ability entity's
//! `C_CitadelBaseAbility::m_nSubclassID`. And that number is a `CUtlStringToken` of the
//! ability's **vdata key** - the block name in `scripts/abilities.vdata_c` - not of its C++
//! class name.
//!
//! Both halves were measured against a live client rather than reasoned about:
//!
//! - hashing the C++ class name and comparing against the live subclass id agreed **0 of
//!   17** times;
//! - joining every distinct live subclass id against the ability roster read out of
//!   `scripts/abilities.vdata_c`, whose ids are derived with `ItemId::from_class_name`,
//!   matched **15 of 15, with nothing left unmatched**.
//!
//! | live `m_nSubclassID` | C++ class | vdata key it resolves to |
//! |---|---|---|
//! | 2335418656 | `CCitadel_Ability_Slide` | `citadel_ability_slide` |
//! | 2207638101 | `CCitadel_Ability_Dash` | `citadel_ability_dash` |
//! | 1065103387 | `CCitadel_Ability_LightningBall` | `citadel_ability_lightning_ball` |
//! | 2372182196 | `CCitadel_Ability_HoldMelee` | `citadel_ability_melee_gigawatt` |
//! | 2695625322 | `CCitadel_Ability_PrimaryWeapon_Empty` | `citadel_weapon_gigawatt_set` |
//!
//! The last two rows are what sinks the class-name route: the class is generic where the
//! vdata key is hero-specific. A name derived from `CCitadel_Ability_HoldMelee` would read
//! the same for every hero in the match, and no `EntitySnapshot` would have fixed that.
//!
//! So the id is already an item id in `deadlock-core`'s terms, which is why it is reported
//! as an [`ItemId`] rather than a bare number. Resolving it to a display name needs the
//! roster, and this crate deliberately does not link it: the join is a consumer's, and no
//! edge from here to `deadlock-data` exists or should be added.
//!
//! No such route exists for the **caster**. `m_hCaster` names the pawn that cast the
//! ability, nothing in a [`LiveSnapshot`] turns an entity handle into a lobby slot, and
//! modifiers carry no equivalent id for the applier. Ledgers are therefore still keyed by
//! caster handle rather than by slot, and naming the caster is still the caller's problem.
//!
//! # Grouping by handle, reporting by subclass id
//!
//! These are two different questions and they get two different keys.
//!
//! *Which instance cast this* is the handle's job: two heroes' copies of one ability share
//! a subclass id and differ only by handle, so grouping on the subclass id would merge two
//! simultaneous casts into one that hit twice as many enemies as either did. Groups are
//! keyed by `m_hAbility`, as item 6.1 built them.
//!
//! *Which ability is this* is the subclass id's job, and only it can do it: a handle is an
//! entity index plus a serial, so it is meaningless in the next match and changes if the
//! ability entity is recreated inside this one. Totals are therefore accumulated per
//! subclass id, which means two handles that share one - the recreated-entity case - go on
//! adding to a single running total instead of splitting into two half-totals nobody can
//! rejoin.
//!
//! A modifier with no usable subclass id - absent, or the zero that
//! [`ItemId::NONE`] is - falls back to the handle, so an application is never dropped for
//! want of a name. That leaves two kinds of identifier in one namespace, so both are
//! **tagged** rather than left to be told apart by their shape:
//! `ability.subclass_2335418656.landed_casts` against
//! `ability.handle_0x00018005.landed_casts`. An untagged decimal beside an untagged `0x`
//! hex would be distinguishable today and quietly wrong the first time anything renders a
//! handle in decimal or an id in hex. [`AbilityId`] makes the same distinction at the type
//! level, and [`AbilityCastTotals::subclass_id`] hands a consumer the id itself, so
//! resolving a name never means parsing a key string back apart.
//!
//! # A cast that falls between two polls is invisible
//!
//! The honesty the `crowd-control` module owes about missed ticks applies
//! here, and slightly harder. A modifier applied and expired entirely between two
//! snapshots leaves no trace in either, so its cast is never seen and never counted.
//!
//! Unlike a crowd control's seconds, one sighting is not enough to recover the answer:
//! [`AbilityCastTotals::enemies_hit`] counts *targets observed carrying the modifier*, so
//! a cast that hit four enemies but was first sampled once two of them had been cleansed
//! reads as a cast that hit two. Every total here is a lower bound, and the gap narrows
//! with the poll interval.
//!
//! # A caster is usually a player, and sometimes is not
//!
//! Measured against a live match (`100650421`): of 397 modifier casters in one snapshot,
//! **390 were `C_CitadelPlayerPawn`**. The rest were not. A `C_NPC_Boss_Tier2` — a walker —
//! and a `CCitadelCatapultTrigger` each applied modifiers, and two handles resolved to no
//! entity at all.
//!
//! That matters less than it first appears, and the reason is worth stating. Every
//! NPC-applied modifier observed carried `m_nAbilitySubclassID == 0`, which this module
//! already treats as absent: those applications file under `handle_<hex>` rather than
//! `subclass_<id>`, so they never masquerade as a named hero ability. A `handle_` key in
//! the output is the signal that something without a resolvable ability applied it.
//!
//! What is *not* safe is reading a caster handle as "a player". It is an entity handle, and
//! entities include walkers and map triggers.

use std::collections::{HashMap, HashSet};

use deadlock_reader::Team;
use deadlock_reader::core_types::ItemId;
use deadlock_reader::entity::INVALID_HANDLE;
use deadlock_reader::snapshot::{LiveSnapshot, Modifier};

/// The `ability.` prefix every metric this module emits carries.
const METRIC_PREFIX: &str = "ability";

/// How far apart two applications may be created and still count as one cast, in seconds.
///
/// The width is the whole of the inference: it is the only thing separating "one ability
/// that swept four enemies" from "four casts of one ability". It has **not** been
/// calibrated, because nothing observed of Companion says what it uses. It is a small
/// multiple of the server's `1/64` tick - sixteen ticks - picked so that an ability whose
/// applications land over a few ticks of projectile or sweep travel stays in one group.
///
/// Both ways of getting it wrong are real, and they fail in opposite directions:
///
/// - **Too narrow** splits one cast into several. `landed_casts` inflates,
///   `max_enemies_in_cast` deflates, and `avg_enemies_per_landed_cast` falls toward 1.0.
///   A lingering area effect that applies to enemies as they walk into it is the worst
///   case: it can read as one cast per enemy.
/// - **Too wide** merges casts that were genuinely separate. `landed_casts` deflates,
///   `max_enemies_in_cast` inflates, and the average rises. A short-cooldown ability fired
///   twice in succession is the worst case, and a window wider than the cooldown makes the
///   two indistinguishable.
///
/// `enemies_hit` moves in one direction only, and not symmetrically with the rest.
/// Splitting a group keeps every target distinct, so a too-narrow window leaves the count
/// alone. Merging two groups that shared a target collapses that target to a single hit,
/// so a too-wide window deflates `enemies_hit` as well as `landed_casts`. Change the width
/// with [`LandedCastTracker::with_window`].
pub const DEFAULT_CAST_WINDOW: f32 = 0.25;

/// What one ability's totals are filed under.
///
/// Two kinds of identifier, deliberately not interchangeable. Ordering puts every
/// [`Subclass`](AbilityId::Subclass) before every [`Handle`](AbilityId::Handle), so a
/// ledger's report order is stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AbilityId {
    /// `m_nAbilitySubclassID`, which is a `CUtlStringToken` of the ability's vdata key.
    ///
    /// The identifier to prefer: stable across matches, and resolvable to a name by any
    /// consumer holding the ability roster. See the module docs for the measurement.
    Subclass(ItemId),
    /// `m_hAbility`, used only when the modifier carried no usable subclass id.
    ///
    /// Distinguishes one ability *instance* within one match and nothing beyond that: a
    /// handle is an entity index plus a serial, so it names nothing in the next match and
    /// resolves to no name in this one.
    Handle(u32),
}

impl AbilityId {
    /// The subclass id, when this is one.
    ///
    /// `None` for a fallback handle, which is what a consumer resolving names should skip
    /// rather than look up.
    pub fn subclass_id(&self) -> Option<ItemId> {
        match self {
            AbilityId::Subclass(id) => Some(*id),
            AbilityId::Handle(_) => None,
        }
    }

    /// The ability handle, when this is a fallback.
    pub fn handle(&self) -> Option<u32> {
        match self {
            AbilityId::Subclass(_) => None,
            AbilityId::Handle(handle) => Some(*handle),
        }
    }
}

/// The metric key an ability is reported under.
///
/// `subclass_2335418656` for an [`AbilityId::Subclass`] - the decimal
/// `m_nAbilitySubclassID`, which is the `CUtlStringToken` of the ability's
/// `scripts/abilities.vdata_c` key and so an [`ItemId`] a consumer can look up directly.
/// `handle_0x00018005` for an [`AbilityId::Handle`], the fallback used when no subclass id
/// was readable.
///
/// Both are tagged, and that is the point rather than decoration: the two carry different
/// meanings into one namespace, and an untagged pair would rely on a decimal never
/// colliding with a hex rendering - true today, and quietly wrong the first time either
/// side changes base. A consumer reads the tag, or matches on [`AbilityId`] and never
/// touches the string.
pub fn ability_key(ability: AbilityId) -> String {
    match ability {
        AbilityId::Subclass(id) => format!("subclass_{}", id.get()),
        AbilityId::Handle(handle) => format!("handle_{handle:#010x}"),
    }
}

/// Something that happened to a caster's landed-cast ledger.
///
/// Both variants describe one modifier application landing on an enemy, grouped into a
/// cast by the inferred rule in the module docs. Ordering within one tick is by
/// `m_flCreationTime`, then by target slot.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum LandedCastEvent {
    /// A cast landed on its first enemy, so a new group was opened.
    Landed {
        /// `m_hCaster`, the handle of whoever cast it. Not resolvable to a lobby slot from
        /// a snapshot alone; see the module docs.
        caster: u32,
        /// `m_hAbility`, the instance this group is keyed on.
        ability: u32,
        /// `m_nAbilitySubclassID`, the id the totals are filed under.
        ///
        /// `None` when the modifier carried none, which is when the ledger falls back to
        /// [`ability`](LandedCastEvent::Landed::ability). Resolvable to a name through the
        /// ability roster; see the module docs.
        subclass_id: Option<ItemId>,
        /// `m_flCreationTime` of the application that opened the group.
        at: f32,
        /// Lobby slot of the enemy it landed on.
        slot: u32,
        /// Enemies in this cast so far, which is 1 for a group that has just opened.
        enemies_in_cast: u32,
    },
    /// A further distinct enemy joined a cast already counted.
    ///
    /// This does not add to [`AbilityCastTotals::landed_casts`] - the cast was counted
    /// when it opened - and it is the only thing that can raise
    /// [`AbilityCastTotals::max_enemies_in_cast`].
    Extended {
        /// `m_hCaster`, as for [`Landed`](LandedCastEvent::Landed).
        caster: u32,
        /// `m_hAbility`.
        ability: u32,
        /// `m_nAbilitySubclassID`, as for [`Landed`](LandedCastEvent::Landed).
        subclass_id: Option<ItemId>,
        /// `m_flCreationTime` of the application that joined the group.
        at: f32,
        /// Lobby slot of the enemy that joined.
        slot: u32,
        /// Enemies in this cast including the one that just joined.
        enemies_in_cast: u32,
    },
}

/// One ability's landed-cast totals for one caster.
///
/// Every field is derived from the inferred grouping the module docs describe, and every
/// one is a lower bound: a cast nobody sampled is not here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AbilityCastTotals {
    /// The ability these totals belong to, when it could be named.
    ///
    /// `m_nAbilitySubclassID`, which is the `CUtlStringToken` of the ability's
    /// `scripts/abilities.vdata_c` key: a consumer holding the roster resolves it to a
    /// display name directly, with no key string to parse and no dependency from this
    /// crate to `deadlock-data`. `None` when no subclass id was readable and the ledger
    /// fell back to the ability handle, and on the zeroed totals a never-landed ability
    /// reads as.
    pub subclass_id: Option<ItemId>,
    /// Groups that landed on at least one enemy.
    pub landed_casts: u32,
    /// Distinct enemies hit, summed over those groups.
    ///
    /// A target hit twice by one cast - two modifiers from one ability - counts once,
    /// which is what "across distinct targets" means. The same target hit by two separate
    /// casts counts twice.
    pub enemies_hit: u32,
    /// The largest any one group reached.
    ///
    /// The peak over the match, not the size of the most recent cast.
    pub max_enemies_in_cast: u32,
}

impl AbilityCastTotals {
    /// [`enemies_hit`](AbilityCastTotals::enemies_hit) over
    /// [`landed_casts`](AbilityCastTotals::landed_casts).
    ///
    /// Derived here rather than stored: it is a ratio of two counts this module already
    /// keeps, and nothing in the game reports it.
    ///
    /// `None` when nothing has landed. Zero is a ratio a cast could in principle have, so
    /// returning it for "no casts" would put an answer where there is no question - and a
    /// consumer averaging across abilities would fold the invented zeroes in. An entry
    /// exists in the ledger only because a cast landed, so in practice this is `None` only
    /// for an ability that was never recorded.
    pub fn avg_enemies_per_landed_cast(&self) -> Option<f64> {
        if self.landed_casts == 0 {
            return None;
        }
        Some(f64::from(self.enemies_hit) / f64::from(self.landed_casts))
    }
}

/// A cast still open for further applications.
///
/// `anchor` is the earliest `m_flCreationTime` in the group. Applications are folded in
/// creation-time order, so nothing can move it earlier afterwards.
#[derive(Clone, Debug)]
struct OpenCast {
    anchor: f32,
    /// The id this group was filed under when it opened.
    ///
    /// Held rather than recomputed per application. `open` is keyed by the ability handle
    /// but `totals` is keyed by [`AbilityId`], and a subclass id that reads on one
    /// application of a group and not the next would otherwise send the extension to a
    /// different entry than the one that counted the cast.
    id: AbilityId,
    /// Targets already counted. Membership is what makes a second modifier from one cast
    /// on one enemy a single hit, and it is also what stops a long-lasting modifier being
    /// counted again on every tick it stays in the target's list.
    targets: HashSet<u32>,
    enemies: u32,
}

/// One application, lifted out of a snapshot so the fold can order them.
///
/// Only enemy applications carrying everything needed to place them in a window get here.
#[derive(Clone, Copy, Debug)]
struct Application {
    caster: u32,
    /// `m_hAbility`: which instance, and so which group this joins.
    ability: u32,
    /// `m_nAbilitySubclassID`: which ability, and so which totals this lands in.
    subclass: Option<ItemId>,
    created: f32,
    slot: u32,
    /// Modifier address, used only to break ties so the event order is reproducible.
    address: u64,
}

impl Application {
    /// What this application's totals are filed under.
    fn id(&self) -> AbilityId {
        match self.subclass {
            Some(id) => AbilityId::Subclass(id),
            None => AbilityId::Handle(self.ability),
        }
    }
}

/// One caster's ledger for the current match.
///
/// Groups are keyed by ability *handle* and totals by [`AbilityId`]; the module docs say
/// why those are not the same key.
#[derive(Clone, Debug, Default)]
pub struct CasterCasts {
    totals: HashMap<AbilityId, AbilityCastTotals>,
    open: HashMap<u32, OpenCast>,
}

impl CasterCasts {
    /// Totals for one ability. Zeroed for an ability that has landed nothing.
    pub fn totals(&self, ability: AbilityId) -> AbilityCastTotals {
        self.totals.get(&ability).copied().unwrap_or_default()
    }

    /// Every ability this caster has landed something with, subclass ids first.
    ///
    /// Abilities with nothing recorded are absent rather than present and zero, which is
    /// how Companion's own output reads.
    pub fn totals_by_ability(&self) -> Vec<(AbilityId, AbilityCastTotals)> {
        let mut out: Vec<(AbilityId, AbilityCastTotals)> =
            self.totals.iter().map(|(a, t)| (*a, *t)).collect();
        out.sort_unstable_by_key(|(ability, _)| *ability);
        out
    }

    /// How many groups are still open to further applications.
    ///
    /// A group stays open until an application of the same ability arrives beyond the
    /// window, so this is bookkeeping rather than a claim that anything is still in
    /// flight.
    pub fn open_casts(&self) -> usize {
        self.open.len()
    }

    /// The ledger as flat metric keys, e.g. `ability.subclass_2335418656.landed_casts`.
    ///
    /// The middle segment is [`ability_key`]'s rendering of the [`AbilityId`], so it reads
    /// `subclass_<decimal id>` for an ability that named itself and
    /// `handle_<0x hex handle>` for one that did not. Resolving the id to a display name
    /// is the consumer's, through the ability roster; [`AbilityCastTotals::subclass_id`]
    /// hands it over without going back through the string.
    pub fn metrics(&self) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        for (ability, totals) in self.totals_by_ability() {
            let key = ability_key(ability);
            out.push((
                format!("{METRIC_PREFIX}.{key}.landed_casts"),
                f64::from(totals.landed_casts),
            ));
            out.push((
                format!("{METRIC_PREFIX}.{key}.enemies_hit"),
                f64::from(totals.enemies_hit),
            ));
            out.push((
                format!("{METRIC_PREFIX}.{key}.max_enemies_in_cast"),
                f64::from(totals.max_enemies_in_cast),
            ));
            if let Some(avg) = totals.avg_enemies_per_landed_cast() {
                out.push((
                    format!("{METRIC_PREFIX}.{key}.avg_enemies_per_landed_cast"),
                    avg,
                ));
            }
        }
        out
    }

    /// Totals for one ability, opened with the id already filled in.
    fn totals_mut(&mut self, id: AbilityId) -> &mut AbilityCastTotals {
        self.totals.entry(id).or_insert(AbilityCastTotals {
            subclass_id: id.subclass_id(),
            ..AbilityCastTotals::default()
        })
    }

    /// Fold one application in, returning the event it produced if it counted.
    fn fold(&mut self, app: &Application, window: f32) -> Option<LandedCastEvent> {
        let id = app.id();
        if let Some(cast) = self.open.get_mut(&app.ability) {
            // Created before the anchor means this belongs to a group already closed: its
            // modifier is simply still in the target's list. Counting it would re-open a
            // finished cast on every tick until the modifier expires.
            if app.created < cast.anchor {
                return None;
            }
            if app.created - cast.anchor <= window {
                if !cast.targets.insert(app.slot) {
                    return None;
                }
                cast.enemies += 1;
                let enemies = cast.enemies;
                let id = cast.id;
                let totals = self.totals_mut(id);
                totals.enemies_hit += 1;
                totals.max_enemies_in_cast = totals.max_enemies_in_cast.max(enemies);
                return Some(LandedCastEvent::Extended {
                    caster: app.caster,
                    ability: app.ability,
                    subclass_id: app.subclass,
                    at: app.created,
                    slot: app.slot,
                    enemies_in_cast: enemies,
                });
            }
        }

        self.open.insert(
            app.ability,
            OpenCast {
                anchor: app.created,
                id,
                targets: HashSet::from([app.slot]),
                enemies: 1,
            },
        );
        let totals = self.totals_mut(id);
        totals.landed_casts += 1;
        totals.enemies_hit += 1;
        totals.max_enemies_in_cast = totals.max_enemies_in_cast.max(1);
        Some(LandedCastEvent::Landed {
            caster: app.caster,
            ability: app.ability,
            subclass_id: app.subclass,
            at: app.created,
            slot: app.slot,
            enemies_in_cast: 1,
        })
    }
}

/// Whether a handle names anything.
///
/// Zero is what a modifier no ability applied carries, and [`INVALID_HANDLE`] is the
/// engine's explicit "nothing". Grouping on either would collect every unrelated passive
/// in the match into one enormous cast.
fn names_something(handle: u32) -> bool {
    handle != 0 && handle != INVALID_HANDLE
}

/// The ability id a modifier names, if it names one.
///
/// `m_nAbilitySubclassID` reads zero for a modifier no ability applied - most passives -
/// and zero is [`ItemId::NONE`], which resolves to nothing. Filing those under
/// `subclass_0` would collect every unnamed ability in the match into one total, so they
/// fall back to the handle instead. `None` here is a fallback, never a name.
fn applying_ability(m: &Modifier) -> Option<ItemId> {
    m.subclass_id.map(ItemId).filter(ItemId::is_some)
}

/// The applying side, as read off the modifier.
///
/// `m_iTeam` being the *caster's* team rather than the target's is inferred; see the
/// module docs, including what happens if it is the other way round. A value that is not
/// one of the two playing sides yields `None`, so an unassigned or neutral applier is
/// never an enemy of anybody.
fn applying_team(m: &Modifier) -> Option<Team> {
    let team = Team(u32::from(m.team?));
    team.is_playing().then_some(team)
}

/// Accumulates landed casts per caster, per ability, across snapshots.
///
/// Feed it every snapshot. It keeps a ledger per caster handle for the life of a match and
/// clears the lot when the match id changes, because handles, controllers and every
/// counter are recycled between matches.
///
/// Ledgers are keyed by `m_hCaster` rather than by lobby slot: a [`LiveSnapshot`] carries
/// nothing that maps an entity handle to a player row, so naming the caster is the
/// caller's job. See the module docs, which also say why the grouping this performs is
/// inferred rather than observed.
///
/// ```no_run
/// use deadlock_events::landed_casts::LandedCastTracker;
/// use deadlock_reader::Reader;
///
/// let reader = Reader::attach()?;
/// let mut casts = LandedCastTracker::new();
/// if let Some(snap) = reader.live_snapshot()? {
///     for event in casts.update(&snap) {
///         println!("{event:?}");
///     }
///     for (caster, ledger) in casts.casters() {
///         for (key, value) in ledger.metrics() {
///             println!("{caster:#010x} {key} = {value}");
///         }
///     }
/// }
/// # Ok::<(), deadlock_reader::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct LandedCastTracker {
    match_id: Option<u64>,
    window: f32,
    casters: HashMap<u32, CasterCasts>,
    ticks_observed: u32,
    ticks_without_modifiers: u32,
}

impl Default for LandedCastTracker {
    fn default() -> Self {
        LandedCastTracker {
            match_id: None,
            window: DEFAULT_CAST_WINDOW,
            casters: HashMap::new(),
            ticks_observed: 0,
            ticks_without_modifiers: 0,
        }
    }
}

impl LandedCastTracker {
    /// A tracker with no history, grouping at [`DEFAULT_CAST_WINDOW`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Group at a different window width.
    ///
    /// Read [`DEFAULT_CAST_WINDOW`] first: the two ways of getting this wrong move
    /// different totals in opposite directions. A negative width is clamped to zero, which
    /// groups only applications sharing a creation time exactly.
    pub fn with_window(mut self, seconds: f32) -> Self {
        self.window = seconds.max(0.0);
        self
    }

    /// The window width this tracker groups at.
    pub fn window(&self) -> f32 {
        self.window
    }

    /// Forget every ledger.
    ///
    /// Call this after a gap in polling. Open groups are dropped rather than closed: an
    /// application seen after a gap cannot be told apart from a re-application, so
    /// carrying a group across would merge two casts that polling simply missed.
    pub fn reset(&mut self) {
        self.match_id = None;
        self.casters.clear();
        self.ticks_observed = 0;
        self.ticks_without_modifiers = 0;
    }

    /// The match these ledgers belong to.
    pub fn match_id(&self) -> Option<u64> {
        self.match_id
    }

    /// One caster's ledger, by `m_hCaster`.
    pub fn caster(&self, caster: u32) -> Option<&CasterCasts> {
        self.casters.get(&caster)
    }

    /// Every ledger, in caster-handle order.
    pub fn casters(&self) -> Vec<(u32, &CasterCasts)> {
        let mut out: Vec<(u32, &CasterCasts)> = self.casters.iter().map(|(c, l)| (*c, l)).collect();
        out.sort_unstable_by_key(|(caster, _)| *caster);
        out
    }
    /// Ticks this tracker processed.
    pub fn ticks_observed(&self) -> u32 {
        self.ticks_observed
    }

    /// Of those, how many had **no** readable modifier list on any playing player.
    ///
    /// Everything here comes from `PlayerRow::modifiers`, which is `None` when the list
    /// could not be read, and a caster who landed nothing reports nothing either. Equal to
    /// [`Self::ticks_observed`] means no modifier was ever seen - what a build without
    /// `deadlock-reader/modifiers` looks like - which is a very different fact from an
    /// empty ledger.
    pub fn ticks_without_modifiers(&self) -> u32 {
        self.ticks_without_modifiers
    }

    /// Fold one snapshot in and return what changed.
    ///
    /// Applications are gathered from every player row that has a lobby slot on a playing
    /// side, then folded in `m_flCreationTime` order, so a group's window is anchored on
    /// its own earliest application regardless of the order rows were walked.
    ///
    /// Skipped, each for a reason that would otherwise mean a fabricated group:
    ///
    /// - a modifier with no readable `m_flCreationTime`, which cannot be placed in any
    ///   window;
    /// - a modifier whose `m_hAbility` or `m_hCaster` names nothing, which no cast owns;
    /// - a modifier applied by the target's own side, which is a buff and not a hit;
    /// - a spectator row, which has no scoreboard entry and no side.
    ///
    /// A missing `m_nAbilitySubclassID` is **not** on that list: it costs the application
    /// its name, not its place in a cast, and the totals fall back to the ability handle.
    ///
    /// The match clock is deliberately not consulted. Every comparison here is between two
    /// `m_flCreationTime` readings, so a tick whose clock could not be read still carries
    /// usable applications - unlike the `crowd-control` module, where every
    /// number is a position on that clock.
    pub fn update(&mut self, snap: &LiveSnapshot) -> Vec<LandedCastEvent> {
        if snap.match_id != self.match_id {
            self.casters.clear();
            self.ticks_observed = 0;
            self.ticks_without_modifiers = 0;
            self.match_id = snap.match_id;
        }

        self.ticks_observed += 1;
        let mut any_read = false;

        let mut applications: Vec<Application> = Vec::new();
        for row in &snap.players {
            let (Some(slot), Some(team)) = (row.slot, row.team) else {
                continue;
            };
            if !team.is_playing() {
                continue;
            }
            let Some(list) = row.modifiers.as_deref() else {
                continue;
            };
            any_read = true;
            for m in list {
                let (Some(ability), Some(caster), Some(created)) =
                    (m.ability, m.caster, m.creation_time)
                else {
                    continue;
                };
                if !names_something(ability) || !names_something(caster) {
                    continue;
                }
                if applying_team(m) != team.opponent() {
                    continue;
                }
                applications.push(Application {
                    caster,
                    ability,
                    subclass: applying_ability(m),
                    created,
                    slot,
                    address: m.address,
                });
            }
        }
        if !any_read {
            self.ticks_without_modifiers += 1;
        }

        // Creation-time order is what anchors a window on the earliest application of its
        // group. Slot and address only break ties, so that the event stream does not
        // depend on the order rows or modifier lists happened to be walked.
        applications.sort_unstable_by(|a, b| {
            a.created
                .total_cmp(&b.created)
                .then(a.slot.cmp(&b.slot))
                .then(a.address.cmp(&b.address))
        });

        let window = self.window;
        let mut out = Vec::new();
        for app in &applications {
            let ledger = self.casters.entry(app.caster).or_default();
            if let Some(event) = ledger.fold(app, window) {
                out.push(event);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deadlock_reader::snapshot::PlayerRow;

    const AMBER: Team = Team::AMBER;
    const SAPPHIRE: Team = Team::SAPPHIRE;

    /// Handles shaped like real ones: a low entity index with a serial in the high bits.
    const HOOK: u32 = 0x0001_8005;
    const BEBOP: u32 = 0x0001_8004;

    /// What every application below is filed under, the subclass id rather than the
    /// handle.
    const HOOK_ID: AbilityId = AbilityId::Subclass(ItemId(HOOK_SUBCLASS));

    /// One modifier as the reader would report it, applied by `by`'s side.
    ///
    /// `m_nAbilitySubclassID` is present, which is the ordinary case for a modifier an
    /// ability applied; the tests that care about it being absent set it themselves.
    fn applied_by(ability: u32, caster: u32, by: Team, created: f32, address: u64) -> Modifier {
        Modifier {
            address,
            class: Some("CCitadel_Modifier_Stunned".to_owned()),
            subclass_id: Some(HOOK_SUBCLASS),
            creation_time: Some(created),
            duration: Some(1.0),
            ability: Some(ability),
            caster: Some(caster),
            team: Some(by.get() as u8),
            ..Default::default()
        }
    }

    /// One tick: `players` is `(slot, team, modifier list)`.
    fn snapshot(players: Vec<(u32, Team, Option<Vec<Modifier>>)>) -> LiveSnapshot {
        LiveSnapshot {
            match_id: Some(42),
            players: players
                .into_iter()
                .map(|(slot, team, modifiers)| PlayerRow {
                    slot: Some(slot),
                    team: Some(team),
                    modifiers,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// Three enemies carrying one ability's modifier, all created at the same instant.
    fn sweep(created: f32) -> LiveSnapshot {
        snapshot(vec![
            (
                7,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, created, 0x1000)]),
            ),
            (
                8,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, created, 0x2000)]),
            ),
            (
                9,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, created, 0x3000)]),
            ),
        ])
    }

    fn totals(t: &LandedCastTracker, caster: u32, ability: AbilityId) -> AbilityCastTotals {
        t.caster(caster)
            .map(|c| c.totals(ability))
            .unwrap_or_default()
    }

    /// The claim §5.2 makes: one group across distinct targets is one landed cast.
    ///
    /// Guards against counting a landed cast per *application*, which is what a tracker
    /// that never groups reports - three casts of one enemy each, and an average of 1.0
    /// where the answer this rule gives is 3.0.
    /// A match with no landed casts and a match with no modifier reads are told apart.
    ///
    /// The same gap `crowd_control` had, for the same reason: everything here comes from
    /// `PlayerRow::modifiers`, so a build without `deadlock-reader/modifiers` produces an
    /// empty ledger that reads exactly like a player who landed nothing. In a recorded
    /// match, where the numbers are read back later, that difference is the difference
    /// between a result and a bug.
    #[test]
    fn an_unread_modifier_list_is_counted_separately_from_a_quiet_one() {
        let mut t = LandedCastTracker::new();
        assert_eq!(t.ticks_observed(), 0);
        assert_eq!(t.ticks_without_modifiers(), 0);

        t.update(&snapshot(vec![(1, AMBER, None), (7, SAPPHIRE, None)]));
        assert_eq!(t.ticks_observed(), 1);
        assert_eq!(
            t.ticks_without_modifiers(),
            1,
            "no player carried a readable list"
        );

        t.update(&snapshot(vec![
            (1, AMBER, Some(Vec::new())),
            (7, SAPPHIRE, None),
        ]));
        assert_eq!(t.ticks_observed(), 2);
        assert_eq!(t.ticks_without_modifiers(), 1);
    }

    #[test]
    fn one_cast_landing_on_three_enemies_is_one_landed_cast_with_three_hits() {
        let mut t = LandedCastTracker::new();
        let events = t.update(&sweep(100.0));

        assert_eq!(events.len(), 3, "one event per distinct enemy");
        assert!(matches!(events[0], LandedCastEvent::Landed { slot: 7, .. }));
        assert!(matches!(
            events[1],
            LandedCastEvent::Extended {
                slot: 8,
                enemies_in_cast: 2,
                ..
            }
        ));
        assert!(matches!(
            events[2],
            LandedCastEvent::Extended {
                slot: 9,
                enemies_in_cast: 3,
                ..
            }
        ));

        let got = totals(&t, BEBOP, HOOK_ID);
        assert_eq!(
            got,
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 1,
                enemies_hit: 3,
                max_enemies_in_cast: 3,
            }
        );
        assert_eq!(got.avg_enemies_per_landed_cast(), Some(3.0));
    }

    /// The same ability landing twice, further apart than the window, is two casts.
    ///
    /// The other half of the grouping rule. A tracker keying a cast on the ability handle
    /// alone reports one cast that hit six enemies with a peak of six, none of which
    /// happened.
    #[test]
    fn two_casts_of_the_same_ability_at_different_times_are_two_landed_casts() {
        let mut t = LandedCastTracker::new();
        t.update(&sweep(100.0));
        t.update(&sweep(140.0));

        assert_eq!(
            totals(&t, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 2,
                enemies_hit: 6,
                max_enemies_in_cast: 3,
            }
        );
    }

    /// A buff the caster put on its own side shares the ability handle and is not a hit.
    ///
    /// Plenty of abilities apply a modifier to their own caster - a speed buff, a cooldown
    /// tracker - carrying the same `m_hAbility` as the debuff they put on enemies.
    /// Counting those inflates `enemies_hit` by one per cast, and for an ability that
    /// missed entirely it invents a landed cast that landed on nobody.
    #[test]
    fn a_self_buff_sharing_the_ability_handle_is_not_an_enemy_hit() {
        let mut t = LandedCastTracker::new();
        let events = t.update(&snapshot(vec![
            (
                1,
                AMBER,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x0100)]),
            ),
            (
                7,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000)]),
            ),
        ]));

        assert_eq!(events.len(), 1, "only the enemy application counts");
        assert!(matches!(events[0], LandedCastEvent::Landed { slot: 7, .. }));
        assert_eq!(
            totals(&t, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 1,
                enemies_hit: 1,
                max_enemies_in_cast: 1,
            }
        );
    }

    /// An ability that touched no enemy has landed nothing.
    #[test]
    fn an_ability_that_touches_no_enemy_records_no_landed_cast() {
        let mut t = LandedCastTracker::new();
        let events = t.update(&snapshot(vec![
            (
                1,
                AMBER,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x0100)]),
            ),
            (
                2,
                AMBER,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x0200)]),
            ),
        ]));

        assert!(events.is_empty());
        assert_eq!(totals(&t, BEBOP, HOOK_ID), AbilityCastTotals::default());
        assert_eq!(
            totals(&t, BEBOP, HOOK_ID).avg_enemies_per_landed_cast(),
            None,
            "no landed casts means no average, not an average of zero"
        );
    }

    /// Two players casting the same ability class at the same instant are two casts.
    ///
    /// Each hero owns its own ability entity, so the handles differ, and the ledger is
    /// keyed by caster besides. A tracker grouping on the ability *class* - or on any key
    /// two players share - reports one cast of four enemies with a peak of four.
    #[test]
    fn two_players_casting_the_same_ability_class_do_not_merge() {
        const OTHER_BEBOP: u32 = 0x0001_8104;
        const OTHER_HOOK: u32 = 0x0001_8105;

        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![
            (
                7,
                SAPPHIRE,
                Some(vec![
                    applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000),
                    applied_by(OTHER_HOOK, OTHER_BEBOP, AMBER, 100.0, 0x1100),
                ]),
            ),
            (
                8,
                SAPPHIRE,
                Some(vec![
                    applied_by(HOOK, BEBOP, AMBER, 100.0, 0x2000),
                    applied_by(OTHER_HOOK, OTHER_BEBOP, AMBER, 100.0, 0x2100),
                ]),
            ),
        ]));

        for (caster, ability) in [(BEBOP, HOOK_ID), (OTHER_BEBOP, HOOK_ID)] {
            assert_eq!(
                totals(&t, caster, ability),
                AbilityCastTotals {
                    subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                    landed_casts: 1,
                    enemies_hit: 2,
                    max_enemies_in_cast: 2,
                },
                "{caster:#x} must own its own cast"
            );
        }
    }

    /// Two casters that somehow shared an ability handle still do not merge.
    ///
    /// That `m_hAbility` is unique per player is a premise about the engine, not something
    /// this crate verified. Keying the ledger by caster as well means the premise failing
    /// costs a naming collision rather than a merged cast.
    #[test]
    fn two_casters_sharing_an_ability_handle_still_get_separate_casts() {
        const OTHER_CASTER: u32 = 0x0001_8204;

        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![
                applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000),
                applied_by(HOOK, OTHER_CASTER, AMBER, 100.0, 0x1100),
            ]),
        )]));

        assert_eq!(totals(&t, BEBOP, HOOK_ID).landed_casts, 1);
        assert_eq!(totals(&t, OTHER_CASTER, HOOK_ID).landed_casts, 1);
    }

    /// `max_enemies_in_cast` is the peak over the match, not the last cast's size.
    ///
    /// A tracker that assigns rather than maximises reports 1 here, having overwritten the
    /// four-enemy cast with the single-enemy one that followed it.
    #[test]
    fn max_enemies_in_cast_keeps_the_peak_rather_than_the_most_recent_cast() {
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![
            (
                7,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000)]),
            ),
            (
                8,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x2000)]),
            ),
            (
                9,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x3000)]),
            ),
            (
                10,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x4000)]),
            ),
        ]));
        assert_eq!(totals(&t, BEBOP, HOOK_ID).max_enemies_in_cast, 4);

        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![applied_by(HOOK, BEBOP, AMBER, 200.0, 0x5000)]),
        )]));

        let got = totals(&t, BEBOP, HOOK_ID);
        assert_eq!(
            got,
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 2,
                enemies_hit: 5,
                max_enemies_in_cast: 4,
            }
        );
        assert_eq!(got.avg_enemies_per_landed_cast(), Some(2.5));
    }

    /// A modifier that outlives the poll interval is one hit, not one per tick.
    ///
    /// A six-second slow sits in the target's list for sixty ticks at 10 Hz. Without
    /// per-group target membership every one of them reads as another enemy hit.
    #[test]
    fn a_modifier_still_in_the_list_on_later_ticks_is_counted_once() {
        let mut t = LandedCastTracker::new();
        let long = applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000);
        let tick = |m: &Modifier| snapshot(vec![(7, SAPPHIRE, Some(vec![m.clone()]))]);

        assert_eq!(t.update(&tick(&long)).len(), 1);
        for _ in 0..5 {
            assert!(t.update(&tick(&long)).is_empty());
        }

        assert_eq!(
            totals(&t, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 1,
                enemies_hit: 1,
                max_enemies_in_cast: 1,
            }
        );
    }

    /// An earlier cast's modifier, still in the list, must not re-open a finished group.
    ///
    /// The hard ordering case: a long modifier from the first cast is still on its target
    /// when the second cast opens, so every later tick presents an application created
    /// *before* the current group's anchor.
    #[test]
    fn a_lingering_modifier_from_an_earlier_cast_does_not_reopen_it() {
        let mut t = LandedCastTracker::new();
        let first = applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000);
        let second = applied_by(HOOK, BEBOP, AMBER, 103.0, 0x2000);
        let both = || {
            snapshot(vec![(
                7,
                SAPPHIRE,
                Some(vec![first.clone(), second.clone()]),
            )])
        };

        t.update(&snapshot(vec![(7, SAPPHIRE, Some(vec![first.clone()]))]));
        t.update(&both());
        for _ in 0..3 {
            assert!(t.update(&both()).is_empty());
        }

        assert_eq!(
            totals(&t, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 2,
                enemies_hit: 2,
                max_enemies_in_cast: 1,
            }
        );
    }

    /// A group's anchor is its earliest application, whatever order the rows arrived in.
    ///
    /// Slot 7 is walked first and was hit 0.2 s later than slot 8. Folding in row order
    /// anchors the group at 100.2, which makes slot 8's application older than the anchor -
    /// indistinguishable from a finished cast's lingering modifier - and drops the hit
    /// entirely. Nothing about a snapshot promises rows or modifier lists arrive in time
    /// order.
    #[test]
    fn a_group_is_anchored_on_its_earliest_application_not_the_first_row_walked() {
        let mut t = LandedCastTracker::new();
        let events = t.update(&snapshot(vec![
            (
                7,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.2, 0x1000)]),
            ),
            (
                8,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x2000)]),
            ),
        ]));

        assert!(
            matches!(events[0], LandedCastEvent::Landed { slot: 8, .. }),
            "the earliest application opens the group: {events:?}"
        );
        assert_eq!(
            totals(&t, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 1,
                enemies_hit: 2,
                max_enemies_in_cast: 2,
            }
        );
    }

    /// A lingering modifier must not be re-counted into the group that replaced its own.
    ///
    /// The case per-group target membership does not cover: the first cast's modifier is
    /// still on slot 7 when a second cast opens on slot 8, so every later tick offers an
    /// application created *before* the open group's anchor, against a target that group
    /// has never seen. Without the anchor guard it joins, and slot 7 is counted again on
    /// every tick until the modifier expires.
    #[test]
    fn a_lingering_modifier_is_not_counted_into_a_later_cast_on_another_target() {
        let mut t = LandedCastTracker::new();
        let first = applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000);
        let second = applied_by(HOOK, BEBOP, AMBER, 103.0, 0x2000);
        let both = || {
            snapshot(vec![
                (7, SAPPHIRE, Some(vec![first.clone()])),
                (8, SAPPHIRE, Some(vec![second.clone()])),
            ])
        };

        t.update(&snapshot(vec![(7, SAPPHIRE, Some(vec![first.clone()]))]));
        t.update(&both());
        for _ in 0..3 {
            assert!(t.update(&both()).is_empty());
        }

        assert_eq!(
            totals(&t, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 2,
                enemies_hit: 2,
                max_enemies_in_cast: 1,
            }
        );
    }

    /// Two modifiers from one cast on one target are one enemy hit.
    ///
    /// "Across distinct targets" is the rule §5.2 states. An ability applying both a stun
    /// and a slow would otherwise report twice the enemies it hit.
    #[test]
    fn two_modifiers_from_one_cast_on_one_target_are_a_single_hit() {
        let mut t = LandedCastTracker::new();
        let events = t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![
                applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000),
                applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1008),
            ]),
        )]));

        assert_eq!(events.len(), 1);
        assert_eq!(totals(&t, BEBOP, HOOK_ID).enemies_hit, 1);
    }

    /// The window is a tunable, and widening it merges what the default keeps apart.
    ///
    /// The two directions the window's own doc comment describes, demonstrated rather than
    /// asserted: the same two applications read as two casts of one enemy or as one cast
    /// of two, and nothing in the data decides which is right.
    #[test]
    fn the_window_width_decides_whether_two_applications_are_one_cast() {
        let tick = || {
            snapshot(vec![
                (
                    7,
                    SAPPHIRE,
                    Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000)]),
                ),
                (
                    8,
                    SAPPHIRE,
                    Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.9, 0x2000)]),
                ),
            ])
        };

        let mut narrow = LandedCastTracker::new();
        narrow.update(&tick());
        assert_eq!(
            totals(&narrow, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 2,
                enemies_hit: 2,
                max_enemies_in_cast: 1,
            },
            "0.9 s apart is beyond the 0.25 s default"
        );

        let mut wide = LandedCastTracker::new().with_window(1.0);
        wide.update(&tick());
        assert_eq!(
            totals(&wide, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 1,
                enemies_hit: 2,
                max_enemies_in_cast: 2,
            },
            "the same data reads as one cast at a wider window"
        );

        assert_eq!(
            totals(&narrow, BEBOP, HOOK_ID).enemies_hit,
            totals(&wide, BEBOP, HOOK_ID).enemies_hit,
            "splitting a group keeps every target distinct, so enemies_hit is unmoved"
        );
    }

    /// Widening the window deflates `enemies_hit` when the merged groups shared a target.
    ///
    /// The asymmetry [`DEFAULT_CAST_WINDOW`] describes: `enemies_hit` survives the window
    /// being too narrow and does not survive it being too wide, because two hits on one
    /// enemy collapse to one the moment the two casts become one.
    #[test]
    fn a_wider_window_loses_a_second_hit_on_a_target_the_merged_casts_shared() {
        let tick = |t: &mut LandedCastTracker| {
            t.update(&snapshot(vec![(
                7,
                SAPPHIRE,
                Some(vec![
                    applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000),
                    applied_by(HOOK, BEBOP, AMBER, 100.9, 0x2000),
                ]),
            )]));
        };

        let mut narrow = LandedCastTracker::new();
        tick(&mut narrow);
        assert_eq!(totals(&narrow, BEBOP, HOOK_ID).enemies_hit, 2);

        let mut wide = LandedCastTracker::new().with_window(1.0);
        tick(&mut wide);
        assert_eq!(
            totals(&wide, BEBOP, HOOK_ID).enemies_hit,
            1,
            "one cast that hit one enemy, where the narrow window saw two of each"
        );
    }

    /// An application missing what it needs to be grouped is skipped, not guessed at.
    #[test]
    fn an_application_missing_what_it_needs_to_be_grouped_is_skipped() {
        let mut t = LandedCastTracker::new();
        let base = applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000);

        let variants = vec![
            Modifier {
                creation_time: None,
                ..base.clone()
            },
            Modifier {
                ability: None,
                ..base.clone()
            },
            Modifier {
                ability: Some(0),
                ..base.clone()
            },
            Modifier {
                ability: Some(INVALID_HANDLE),
                ..base.clone()
            },
            Modifier {
                caster: None,
                ..base.clone()
            },
            Modifier {
                caster: Some(INVALID_HANDLE),
                ..base.clone()
            },
            Modifier {
                team: None,
                ..base.clone()
            },
        ];

        let events = t.update(&snapshot(vec![(7, SAPPHIRE, Some(variants))]));

        assert!(events.is_empty(), "nothing here can be placed in a cast");
        assert_eq!(totals(&t, BEBOP, HOOK_ID), AbilityCastTotals::default());
    }

    /// An unreadable modifier list contributes nothing rather than closing a group.
    #[test]
    fn an_unreadable_modifier_list_neither_lands_nor_closes_anything() {
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000)]),
        )]));
        assert!(t.update(&snapshot(vec![(7, SAPPHIRE, None)])).is_empty());

        t.update(&snapshot(vec![(
            8,
            SAPPHIRE,
            Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.1, 0x2000)]),
        )]));
        assert_eq!(
            totals(&t, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 1,
                enemies_hit: 2,
                max_enemies_in_cast: 2,
            }
        );
    }

    /// A new match id starts every ledger again.
    #[test]
    fn a_new_match_clears_every_ledger() {
        let mut t = LandedCastTracker::new();
        t.update(&sweep(100.0));
        assert_eq!(totals(&t, BEBOP, HOOK_ID).landed_casts, 1);

        let mut next = sweep(100.0);
        next.match_id = Some(43);
        t.update(&next);

        assert_eq!(t.match_id(), Some(43));
        assert_eq!(
            totals(&t, BEBOP, HOOK_ID).landed_casts,
            1,
            "the new match's own cast, not the old one's as well"
        );
    }

    /// The four flat keys a consumer reads, under the id that resolves to a name.
    #[test]
    fn metrics_are_emitted_under_the_four_key_names_the_gap_analysis_quotes() {
        let mut t = LandedCastTracker::new();
        t.update(&sweep(100.0));

        let metrics = t
            .caster(BEBOP)
            .map(CasterCasts::metrics)
            .unwrap_or_default();
        assert_eq!(
            metrics,
            vec![
                ("ability.subclass_2335418656.landed_casts".to_owned(), 1.0),
                ("ability.subclass_2335418656.enemies_hit".to_owned(), 3.0),
                (
                    "ability.subclass_2335418656.max_enemies_in_cast".to_owned(),
                    3.0
                ),
                (
                    "ability.subclass_2335418656.avg_enemies_per_landed_cast".to_owned(),
                    3.0
                ),
            ]
        );
        assert_eq!(ability_key(HOOK_ID), "subclass_2335418656");
        assert_eq!(ability_key(AbilityId::Handle(HOOK)), "handle_0x00018005");
    }

    /// The subclass id measured live for `CCitadel_Ability_Slide`.
    const HOOK_SUBCLASS: u32 = 2_335_418_656;

    /// One modifier with an explicit `m_nAbilitySubclassID`.
    fn applied_with(
        subclass: Option<u32>,
        ability: u32,
        caster: u32,
        by: Team,
        created: f32,
        address: u64,
    ) -> Modifier {
        Modifier {
            subclass_id: subclass,
            ..applied_by(ability, caster, by, created, address)
        }
    }

    fn keys(t: &LandedCastTracker, caster: u32) -> Vec<String> {
        t.caster(caster)
            .map(CasterCasts::metrics)
            .unwrap_or_default()
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    }

    /// The metric key is the subclass id, which is the half of this that has a name.
    ///
    /// A handle is an entity index plus a serial: it means nothing in the next match and
    /// resolves to nothing in this one. The subclass id is a `CUtlStringToken` of the
    /// ability's `scripts/abilities.vdata_c` key, so a consumer holding the roster reads a
    /// name straight off it - measured 15 of 15 against the live client, see the module
    /// docs.
    #[test]
    fn a_metric_key_carries_the_subclass_id_rather_than_the_handle() {
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![applied_with(
                Some(HOOK_SUBCLASS),
                HOOK,
                BEBOP,
                AMBER,
                100.0,
                0x1000,
            )]),
        )]));
        assert_eq!(
            keys(&t, BEBOP)[0],
            "ability.subclass_2335418656.landed_casts"
        );
    }

    /// No subclass id costs the application its name, not its place in the ledger.
    ///
    /// The alternatives are both worse: dropping it loses a landed cast that happened, and
    /// filing it under `subclass_0` invents a name and collects every unnamed ability into
    /// one total.
    #[test]
    fn an_absent_subclass_id_falls_back_to_a_tagged_handle_key() {
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![applied_with(None, HOOK, BEBOP, AMBER, 100.0, 0x1000)]),
        )]));
        assert_eq!(keys(&t, BEBOP)[0], "ability.handle_0x00018005.landed_casts");
    }

    /// Zero is `ItemId::NONE`, so it is an absent id rather than a small one.
    ///
    /// `m_nAbilitySubclassID` reads zero for a modifier no ability applied, which is most
    /// passives. Treating that as a name files unrelated passives together under one key
    /// that resolves to nothing.
    #[test]
    fn a_zero_subclass_id_names_nothing_and_falls_back_to_the_handle() {
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![applied_with(
                Some(0),
                HOOK,
                BEBOP,
                AMBER,
                100.0,
                0x1000,
            )]),
        )]));
        assert_eq!(keys(&t, BEBOP)[0], "ability.handle_0x00018005.landed_casts");
    }

    /// The two kinds of key stay apart even when the numbers coincide.
    ///
    /// `0x00018005` is 98309, so an untagged scheme would put `98309` beside `0x00019005`
    /// and leave a consumer to infer the kind from the base. Both are tagged instead, so
    /// one is never read as the other.
    #[test]
    fn a_subclass_key_is_never_mistakable_for_a_handle_key() {
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![
                applied_with(Some(HOOK), 0x0002_0000, BEBOP, AMBER, 100.0, 0x1000),
                applied_with(None, HOOK, BEBOP, AMBER, 100.0, 0x1100),
            ]),
        )]));

        let got = keys(&t, BEBOP);
        assert!(
            got.contains(&"ability.subclass_98309.landed_casts".to_owned()),
            "{got:?}"
        );
        assert!(
            got.contains(&"ability.handle_0x00018005.landed_casts".to_owned()),
            "{got:?}"
        );
        let mut distinct: Vec<&String> = got.iter().collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), got.len(), "keys must not collide: {got:?}");
    }

    /// Grouping is per instance; totals are per ability. Both, in one tick.
    ///
    /// Two handles carrying one subclass id - what an ability entity recreated mid-match
    /// looks like - land on one target at the same instant. Grouping by handle keeps them
    /// two casts, which is what happened; reporting by subclass id puts both in one
    /// running total rather than two half-totals nobody can rejoin. Grouping on the
    /// subclass id instead would report one cast of one enemy, losing a cast.
    #[test]
    fn two_instances_of_one_ability_report_under_one_key_and_stay_two_casts() {
        const SECOND: u32 = 0x0002_8005;
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![
                applied_with(Some(HOOK_SUBCLASS), HOOK, BEBOP, AMBER, 100.0, 0x1000),
                applied_with(Some(HOOK_SUBCLASS), SECOND, BEBOP, AMBER, 100.0, 0x1100),
            ]),
        )]));
        let metrics = t
            .caster(BEBOP)
            .map(CasterCasts::metrics)
            .unwrap_or_default();
        assert_eq!(
            metrics,
            vec![
                ("ability.subclass_2335418656.landed_casts".to_owned(), 2.0),
                ("ability.subclass_2335418656.enemies_hit".to_owned(), 2.0),
                (
                    "ability.subclass_2335418656.max_enemies_in_cast".to_owned(),
                    1.0
                ),
                (
                    "ability.subclass_2335418656.avg_enemies_per_landed_cast".to_owned(),
                    1.0
                ),
            ]
        );
    }

    /// The whole 32 bits of the id reach the key.
    ///
    /// Live ids run to 2695625322, well past 16 bits, and two abilities differing only
    /// above bit 16 must not collide into one total.
    #[test]
    fn the_high_bits_of_a_subclass_id_survive_into_the_key() {
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![
                applied_with(Some(0x0001_0001), HOOK, BEBOP, AMBER, 100.0, 0x1000),
                applied_with(Some(0x0002_0001), 0x0001_9005, BEBOP, AMBER, 100.0, 0x1100),
            ]),
        )]));
        let got = keys(&t, BEBOP);
        assert!(
            got.contains(&"ability.subclass_65537.landed_casts".to_owned()),
            "{got:?}"
        );
        assert!(
            got.contains(&"ability.subclass_131073.landed_casts".to_owned()),
            "{got:?}"
        );
    }

    /// The totals carry the id, so naming an ability never means parsing a key.
    ///
    /// A consumer resolving names holds the roster and wants an `ItemId`; making it split
    /// `ability.subclass_2335418656.landed_casts` back apart and re-parse the middle
    /// segment would put a string format between it and a number this module already has.
    #[test]
    fn the_totals_expose_the_subclass_id_a_consumer_resolves_a_name_with() {
        let mut t = LandedCastTracker::new();
        t.update(&sweep(100.0));

        let got = totals(&t, BEBOP, HOOK_ID);
        assert_eq!(got.subclass_id, Some(ItemId(HOOK_SUBCLASS)));
        assert_eq!(
            t.caster(BEBOP).unwrap().totals_by_ability(),
            vec![(HOOK_ID, got)]
        );
        assert_eq!(HOOK_ID.subclass_id(), Some(ItemId(HOOK_SUBCLASS)));
        assert_eq!(HOOK_ID.handle(), None);
    }

    /// A fallback total says so rather than reporting an id it does not have.
    ///
    /// `Some(ItemId::NONE)` would be the tempting shape and it is a lie: zero resolves to
    /// nothing, and a consumer looking it up gets an empty answer instead of knowing not
    /// to look.
    #[test]
    fn a_fallback_total_reports_no_subclass_id_at_all() {
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![applied_with(None, HOOK, BEBOP, AMBER, 100.0, 0x1000)]),
        )]));

        let id = AbilityId::Handle(HOOK);
        let got = totals(&t, BEBOP, id);
        assert_eq!(got.landed_casts, 1);
        assert_eq!(got.subclass_id, None);
        assert_eq!(id.handle(), Some(HOOK));
        assert_eq!(id.subclass_id(), None);
        assert_eq!(
            totals(&t, BEBOP, HOOK_ID),
            AbilityCastTotals::default(),
            "a fallback total must not answer to the subclass key"
        );
    }

    /// Events carry the id too, for a consumer that reads the stream rather than the
    /// ledger.
    #[test]
    fn both_events_carry_the_subclass_id() {
        let mut t = LandedCastTracker::new();
        let events = t.update(&sweep(100.0));

        assert!(matches!(
            events[0],
            LandedCastEvent::Landed {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                ability: HOOK,
                ..
            }
        ));
        assert!(matches!(
            events[1],
            LandedCastEvent::Extended {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                ability: HOOK,
                ..
            }
        ));

        let fallback = t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![applied_with(
                Some(0),
                0x0001_9005,
                BEBOP,
                AMBER,
                200.0,
                0x9000,
            )]),
        )]));
        assert!(matches!(
            fallback[0],
            LandedCastEvent::Landed {
                subclass_id: None,
                ability: 0x0001_9005,
                ..
            }
        ));
    }

    /// Report order is stable, with named abilities ahead of unnamed ones.
    ///
    /// Two kinds of key in one namespace need a total order or `metrics()` reshuffles
    /// between ticks on nothing but hash iteration order.
    #[test]
    fn ledger_order_puts_every_subclass_id_before_every_fallback_handle() {
        let mut t = LandedCastTracker::new();
        t.update(&snapshot(vec![(
            7,
            SAPPHIRE,
            Some(vec![
                applied_with(None, 0x0001_0001, BEBOP, AMBER, 100.0, 0x1000),
                applied_with(Some(HOOK_SUBCLASS), HOOK, BEBOP, AMBER, 100.0, 0x1100),
                applied_with(Some(7), 0x0001_9005, BEBOP, AMBER, 100.0, 0x1200),
            ]),
        )]));

        assert_eq!(
            t.caster(BEBOP)
                .unwrap()
                .totals_by_ability()
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            vec![
                AbilityId::Subclass(ItemId(7)),
                AbilityId::Subclass(ItemId(HOOK_SUBCLASS)),
                AbilityId::Handle(0x0001_0001),
            ]
        );
    }

    /// Spectators have no side and no scoreboard row, so nothing lands on them.
    #[test]
    fn a_spectator_row_is_not_a_target() {
        let mut t = LandedCastTracker::new();
        let events = t.update(&snapshot(vec![(
            7,
            Team::SPECTATOR,
            Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000)]),
        )]));
        assert!(events.is_empty());
        assert!(t.casters().is_empty());
    }

    /// A subclass id that goes missing mid-cast must not split one cast's accounting.
    ///
    /// `open` is keyed by the ability handle, `totals` by [`AbilityId`] - and the id is the
    /// subclass when one is readable and the handle when it is not. So a subclass read that
    /// succeeds for one application of a group and fails for the next sends the *extension*
    /// to a different totals entry than the one that counted the cast. The result is an
    /// entry reading `landed_casts: 0, enemies_hit: 1`: an ability that hit somebody
    /// without ever landing, which no gameplay can produce.
    ///
    /// Reachable because a subclass id is a memory read like any other. The NPC-cast
    /// modifiers measured against match `100650421` carry a subclass of zero outright, and
    /// this crate treats zero as absent.
    ///
    /// The rule: a cast group belongs to whichever id it opened under, for its whole life.
    #[test]
    fn a_subclass_id_lost_mid_cast_does_not_split_the_totals() {
        let mut t = LandedCastTracker::new();
        let mut without = applied_by(HOOK, BEBOP, AMBER, 100.0, 0x2000);
        without.subclass_id = None;

        t.update(&snapshot(vec![
            (
                7,
                SAPPHIRE,
                Some(vec![applied_by(HOOK, BEBOP, AMBER, 100.0, 0x1000)]),
            ),
            (8, SAPPHIRE, Some(vec![without])),
        ]));

        assert_eq!(
            totals(&t, BEBOP, HOOK_ID),
            AbilityCastTotals {
                subclass_id: Some(ItemId(HOOK_SUBCLASS)),
                landed_casts: 1,
                enemies_hit: 2,
                max_enemies_in_cast: 2,
            },
            "the group opened under the subclass id and both hits belong to it"
        );

        assert_eq!(
            totals(&t, BEBOP, AbilityId::Handle(HOOK)),
            AbilityCastTotals::default(),
            "the handle key must stay empty rather than hold a hit with no cast"
        );
    }

    /// An ability with nothing recorded has no average, rather than an average of zero.
    ///
    /// `0.0` is a real ratio - it is what "landed on nobody" would mean, if that were a
    /// state this tracker could hold. It is not: an entry exists only because a cast landed,
    /// so every entry in the map has `landed_casts >= 1`. The only way to see a zero is to
    /// ask about an ability that was never recorded, and answering that with a number
    /// invites a consumer to average it in alongside real ones.
    #[test]
    fn an_ability_with_nothing_recorded_has_no_average() {
        assert_eq!(
            AbilityCastTotals::default().avg_enemies_per_landed_cast(),
            None,
            "nothing recorded is not the same as a cast that hit nobody"
        );

        let mut t = LandedCastTracker::new();
        t.update(&sweep(100.0));
        assert_eq!(
            totals(&t, BEBOP, AbilityId::Handle(0xdead_beef)).avg_enemies_per_landed_cast(),
            None,
            "an ability this caster never landed has no average"
        );
        assert_eq!(
            totals(&t, BEBOP, HOOK_ID).avg_enemies_per_landed_cast(),
            Some(3.0)
        );
    }
}
