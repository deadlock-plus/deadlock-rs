//! `dlrs metrics`: record the cross-tick accumulator totals over a match.
//!
//! Three things in `deadlock-events` accumulate across ticks and hold totals nobody else
//! has: [`CrowdControlTracker`], [`LandedCastTracker`] and - where the build has world
//! coordinates - `ObjectiveContextTracker`. `dlrs timeline` records the *event* stream;
//! nothing recorded the *accumulator* output, so a real match went by and the derived
//! numbers were never seen. This command is what makes one match maximally informative.
//!
//! Every module it drives says, in its own docs, that it has never been checked against a
//! live match. This command does not fix that; it is the instrument that lets somebody
//! else fix it, so its job is to be legible enough that the numbers can be argued with.
//!
//! # What it costs to run
//!
//! `crowd-control` and `landed-casts` both forward to `deadlock-reader`'s `modifiers`,
//! which re-reads every player's modifier list every tick: measured at build 6683, a
//! release snapshot goes from about 150 us to about 300 us, roughly 50 us per player.
//! `objective-context` forwards to `positions`, which is one extra pointer chase per
//! entity. `dlrs` already asks for `modifiers` unconditionally, because `dlrs modifiers`
//! needs it, so this command adds no cost the binary was not already paying; and it never
//! turns `positions` on for anybody who did not ask.
//!
//! # Two outputs, because they answer different questions
//!
//! **stdout** gets one progress line per snapshot interval and a full report at the end of
//! a bounded run. It is for watching, and for reading afterwards next to the game's own
//! post-match scoreboard.
//!
//! **`--out <path>`** gets JSONL, one record per line, appended - the same format
//! [`dlrs timeline`](mod@super::timeline) writes, so one match capture is one format. It is
//! for the numbers.
//!
//! # Why a periodic dump and not a per-tick one
//!
//! Three shapes were available.
//!
//! - **Every tick.** A 40-minute match at 10 Hz is 24,000 ticks, and one tick's full
//!   metric set is several hundred numbers. That is tens of millions of numbers to say
//!   what a few hundred would, and almost all of it is the previous tick repeated.
//! - **Once at the end.** One dump loses the shape of the match entirely: a total of 41
//!   stuns says nothing about whether they arrived steadily or all in one teamfight, and
//!   ctrl-c - which is how an unbounded recording actually ends - would lose everything.
//! - **Periodically, in full.** Every `--snapshot <secs>`, default 30, the whole ledger is
//!   written out. A 40-minute match is about 80 dumps: enough to plot any metric against
//!   the match clock, and small enough to read. Because each dump is a full ledger rather
//!   than a delta, ctrl-c costs at most the resolution of the last interval and never the
//!   file, and no consumer has to sum anything to get a total.
//!
//! A final dump is always written when a bounded run ends, whether or not it lands on a
//! snapshot boundary, and it carries `"final":true`.
//!
//! # Every JSONL line stands alone
//!
//! One line per *scope* rather than one fat line per dump: a dump is joined back together
//! on `seq`, and `jq 'select(.slot==3)'` works. Each line repeats the clock, the match id
//! and the sequence number so that a file truncated by ctrl-c mid-dump is still entirely
//! parseable, and so no line has to be interpreted in the light of another.
//!
//! Four kinds of line, distinguished by `kind`:
//!
//! - `header`, once, first: which accumulators were compiled in, what the tuning constants
//!   were, and which of the numbers below are inferred rather than observed.
//! - `progress`, one per dump: where the run is, and what it has been able to read.
//! - `metrics`, one per scope per dump: a scope is the match, a player slot, or one
//!   ability of one caster.
//! - `summary`, once, last.
//!
//! # An absent number is `null`, and on the terminal a dash
//!
//! Never a zero, anywhere. The four that are routinely absent:
//!
//! - the match clock, where `0` is the first second of a match;
//! - the match id, which the reader reports as [`None`] outside a match rather than as the
//!   `0` the game stores;
//! - an ability's display name, when the subclass id resolves to nothing;
//!
//! The key set is the crate's, verbatim.
//!
//! # Telling "nothing happened" from "nothing was read"
//!
//! This is the whole reason the command is worth more than a single dump at the end.
//!
//! - `objective_context` already separates them:
//!   `PlayerBuckets::unattributed` holds what could not be placed and
//!   `PlayerBuckets::unattributable_intervals` counts the intervals. Its `metrics()`
//!   emits the first two but **not** `PlayerBuckets::attributed_intervals`, so the
//!   denominator is added here as `dlrs.attributed_intervals`. A player whose intervals
//!   are all unattributable is one whose position never read, and the report says so in
//!   as many words rather than showing three empty buckets.
//! - `crowd_control` and `landed_casts` answer the same question at *tracker* level, with
//!   `ticks_observed()` and `ticks_without_modifiers()`. Neither is in any `metrics()`, so
//!   both are read through the accessors and filed on the match record as
//!   `dlrs.<tracker>.ticks_observed` and `dlrs.<tracker>.ticks_without_modifiers`. Equal
//!   values mean no modifier list was ever read on any player on any tick - which is what
//!   a build without `deadlock-reader/modifiers` looks like from the outside, and what a
//!   denied read looks like too. "0 crowd control over 4,200 ticks, all of which read
//!   modifiers" is a result; "0 over 4,200, none of which read anything" is a broken
//!   capture, and the report says which.
//! - Those are tracker-wide, so they cannot say *which* player went unread. This command
//!   adds the per-slot split itself, `dlrs.modifier_ticks` and
//!   `dlrs.modifier_ticks_unreadable`, counted off `PlayerRow::modifiers` on each row.
//!   They are counted on every readable tick, where the tracker's are counted only on
//!   ticks whose clock read as well, so the two do not have to agree exactly.
//!
//! Keys beginning `dlrs.` are this command's own bookkeeping and have no Companion
//! counterpart. Everything else is the accumulator's own key, unchanged.
//!
//! # The Hideout is not a quiet match
//!
//! An idle client in the Hideout is readable: it has a game-rules entity, a clock reading
//! the session's uptime, and two or three bot players. Every accumulator therefore runs,
//! and most of what they produce is zero - which is exactly what a real match with all-zero
//! results would look like.
//!
//! [`LiveSnapshot::context`] tells them apart, and it is not inferred: the reader reports
//! [`Context::Hideout`] or [`Context::Match`] directly. Every tick is counted by context,
//! the counts are in every `progress` line and in the summary, and a run in which no tick
//! was [`Context::Match`] and no match id was ever seen is announced as
//! `NO MATCH WAS RECORDED` rather than summarised as one. See [`Verdict`].
//!
//! # What a failed write does
//!
//! The same thing it does in `dlrs timeline`, for the same reason: a match cannot be
//! re-recorded, so one refused write must not throw away the rest of it, and a partial
//! file that looks complete is worse than no file. The failure is surfaced three times -
//! on stderr the first time, in the summary, and in the exit code.
//!
//! # How the loop ends
//!
//! `--for <secs>` elapses, or the client stops answering for [`CLIENT_GONE_AFTER`]
//! consecutive polls. Without `--for` it runs until ctrl-c, which ends the process before
//! the report - which is why the periodic dump into `--out` is a full ledger.
//!
//! [`CrowdControlTracker`]: deadlock_events::crowd_control::CrowdControlTracker
//! [`LandedCastTracker`]: deadlock_events::landed_casts::LandedCastTracker
//! [`CasterCasts::metrics`]: deadlock_events::landed_casts::CasterCasts::metrics

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use deadlock_core::{HeroNames, ItemId, ItemNames};
use deadlock_events::crowd_control::CrowdControlTracker;
use deadlock_events::landed_casts::{LandedCastTracker, ability_key};
use deadlock_reader::snapshot::{Context, LiveSnapshot};

use super::attach;
use crate::catalog::{hero_catalog, item_catalog};
use crate::output::{opt, trunc};

#[cfg(feature = "positions")]
use deadlock_events::objective_context::ObjectiveContextTracker;

/// Flags this command accepts, as `dlrs help` lists them.
///
/// Also what the parser is tested against, so a flag cannot be accepted and undocumented,
/// or documented and rejected.
pub const FLAGS: &[(&str, &str)] = &[
    (
        "--out <path>",
        "append JSONL records to <path>; default is stdout only",
    ),
    (
        "--for <secs>",
        "stop after <secs> and print the report; default is until ctrl-c",
    ),
    (
        "--every <ms>",
        "poll interval; default 100. every tick re-reads all modifiers",
    ),
    (
        "--snapshot <secs>",
        "how often to emit a full metrics dump; default 30",
    ),
];

/// Default poll interval, matching `deadlock_events::DEFAULT_MATCH_INTERVAL`.
const DEFAULT_INTERVAL: Duration = Duration::from_millis(100);

/// Default gap between full metric dumps.
///
/// **Chosen, not measured.** Thirty seconds puts about eighty dumps in a forty-minute
/// match, which is fine enough to see a teamfight as a step in a cumulative curve and
/// coarse enough that the file stays readable. Too fine and the file is mostly the
/// previous dump again; too coarse and ctrl-c throws away more of the match than anybody
/// wanted.
const DEFAULT_SNAPSHOT: Duration = Duration::from_secs(30);

/// Consecutive failed reads before the client is presumed gone.
///
/// Twenty polls is two seconds at the default interval. **Inferred, not measured**, and
/// deliberately the same number [`dlrs timeline`](mod@super::timeline) uses: two commands that
/// poll the same client at the same rate should not disagree about when it has gone away.
const CLIENT_GONE_AFTER: u32 = 20;

/// How wide the hero column is in the report.
const HERO_COLUMN: usize = 14;

/// How wide the ability-name column is in the report.
const ABILITY_COLUMN: usize = 22;

/// What `dlrs metrics` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Opts {
    /// Where to append JSONL, or [`None`] for stdout only.
    out: Option<PathBuf>,
    /// How long to record, or [`None`] to run until ctrl-c.
    duration: Option<Duration>,
    /// How often to poll.
    interval: Duration,
    /// How often to emit a full dump.
    snapshot: Duration,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            out: None,
            duration: None,
            interval: DEFAULT_INTERVAL,
            snapshot: DEFAULT_SNAPSHOT,
        }
    }
}

impl Opts {
    /// Parse the arguments after `metrics`.
    fn parse(args: &[&str]) -> Result<Opts, String> {
        let mut opts = Opts::default();
        let mut i = 0;
        while i < args.len() {
            if args[i].starts_with('-') && !documented(args[i]) {
                return Err(format!("unknown flag {:?}", args[i]));
            }
            match args[i] {
                "--out" => opts.out = Some(PathBuf::from(value(args, &mut i, "--out")?)),
                "--for" => opts.duration = Some(seconds(value(args, &mut i, "--for")?, "--for")?),
                "--every" => {
                    let raw = value(args, &mut i, "--every")?;
                    let ms: u64 = raw
                        .parse()
                        .map_err(|_| format!("--every wants milliseconds, got {raw:?}"))?;
                    if ms == 0 {
                        // A zero interval is a busy loop that reads every player's modifier
                        // list as fast as the kernel will allow, which is not a poll
                        // interval.
                        return Err("--every 0 would spin".to_owned());
                    }
                    opts.interval = Duration::from_millis(ms);
                }
                "--snapshot" => {
                    opts.snapshot = seconds(value(args, &mut i, "--snapshot")?, "--snapshot")?;
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
            i += 1;
        }
        Ok(opts)
    }
}

/// A positive, finite number of seconds.
fn seconds(raw: &str, flag: &str) -> Result<Duration, String> {
    let secs: f64 = raw
        .parse()
        .map_err(|_| format!("{flag} wants seconds, got {raw:?}"))?;
    if !(secs.is_finite() && secs > 0.0) {
        return Err(format!("{flag} wants a positive number of seconds"));
    }
    Ok(Duration::from_secs_f64(secs))
}

/// Whether [`FLAGS`] lists `flag`.
///
/// The parser asks before it dispatches, which is what makes the help table the authority
/// rather than a copy of it: an arm below for a flag that is not listed above is
/// unreachable, so a flag cannot be accepted and undocumented.
fn documented(flag: &str) -> bool {
    FLAGS
        .iter()
        .any(|(spelling, _)| spelling.split_whitespace().next() == Some(flag))
}

/// The value after a flag, advancing the index past it.
fn value<'a>(args: &[&'a str], i: &mut usize, flag: &str) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i)
        .copied()
        .ok_or_else(|| format!("{flag} wants a value"))
}

//
// Hand-rolled, because `dlrs` does not depend on a JSON encoder and this is not worth one.
// The only strings that reach it that are not ASCII literals are hero names, Steam names
// and localised ability names, all of which are arbitrary UTF-8 and one of which is
// attacker-controlled, so the escaping is the part that is tested rather than assumed.

/// One JSON object under construction, as already-encoded `(key, value)` pairs.
///
/// Insertion order is preserved, which is what makes a line diffable against another line.
#[derive(Debug, Default)]
struct Obj(Vec<(String, String)>);

impl Obj {
    fn new() -> Obj {
        Obj(Vec::new())
    }

    /// A value that is already valid JSON.
    fn raw(mut self, key: &str, encoded: String) -> Obj {
        self.0.push((key.to_owned(), encoded));
        self
    }

    fn text(self, key: &str, v: &str) -> Obj {
        self.raw(key, quote(v))
    }

    /// A string, or `null` when there is nothing to say. Never an empty string standing in
    /// for an absent one.
    fn opt_text(self, key: &str, v: Option<&str>) -> Obj {
        match v {
            Some(s) => self.text(key, s),
            None => self.raw(key, "null".to_owned()),
        }
    }

    fn uint(self, key: &str, v: u64) -> Obj {
        self.raw(key, v.to_string())
    }

    fn flag(self, key: &str, v: bool) -> Obj {
        self.raw(key, v.to_string())
    }

    /// A number, or `null` when it is absent or not finite. `0` is a measurement and is
    /// never what an absent value renders as.
    fn opt_number(self, key: &str, v: Option<f64>) -> Obj {
        self.raw(key, number(v))
    }

    fn finish(&self) -> String {
        let body: Vec<String> = self
            .0
            .iter()
            .map(|(k, v)| format!("{}:{v}", quote(k)))
            .collect();
        format!("{{{}}}", body.join(","))
    }
}

/// A JSON string literal.
///
/// Escapes the two characters JSON requires, the five it names, and every other control
/// character as `\u00XX`. Everything else is passed through as UTF-8, which is what the
/// format is defined over.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A number, or `null`.
///
/// `NaN` and the infinities have no JSON spelling, and writing `NaN` produces a file no
/// parser will read. They render as `null` for the same reason an absent value does: the
/// number is not there.
fn number(v: Option<f64>) -> String {
    match v {
        Some(v) if v.is_finite() => {
            // `{}` on an integral f64 prints `3`, not `3.0`; both are JSON numbers, and a
            // consumer reading them as floats gets the same value either way.
            format!("{v}")
        }
        _ => "null".to_owned(),
    }
}

/// A JSON array of already-encoded values.
fn array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

/// Who a set of metrics belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Scope {
    /// The match itself: fight counts, which belong to nobody in particular.
    Match,
    /// One player, by lobby slot.
    Player {
        /// Lobby slot, which is what every accumulator keys players by.
        slot: u32,
    },
    /// One ability of one caster.
    ///
    /// Keyed by caster *handle*, not by slot: `landed_casts` cannot map one to the other,
    /// and inventing a slot here would be a claim the accumulator does not make.
    Ability {
        /// `m_hCaster`, an entity handle. Meaningless outside this match.
        caster: u32,
        /// The accumulator's own middle key segment, `subclass_<id>` or `handle_<hex>`.
        key: String,
        /// The subclass id, when the ledger had one.
        subclass: Option<ItemId>,
        /// Display name for that id, or [`None`] when it resolves to nothing.
        name: Option<String>,
        /// The vdata key the id is a token of, e.g. `citadel_ability_slide`.
        class_name: Option<String>,
        /// `ability` or `upgrade`: whether a hero ability or a bought item applied it.
        ///
        /// `m_nAbilitySubclassID` names whatever applied the modifier, and a Deadlock
        /// item is an ability mechanically, so the ledger counts item procs alongside
        /// hero abilities. §5.2's key set is hero abilities, so a total compared against
        /// Companion's is inflated by items unless the two are told apart. The
        /// accumulator cannot do it - `deadlock-events` has no edge to `deadlock-data` -
        /// but the id resolves through `ItemNames`, which knows.
        kind: Option<String>,
    },
}

/// One scope's metrics at one instant.
#[derive(Clone, Debug, PartialEq)]
struct Record {
    scope: Scope,
    /// The accumulator's own keys, in the accumulator's own order. [`None`] is a value
    /// that is not there, and renders as `null` or as a dash.
    metrics: Vec<(String, Option<f64>)>,
}

impl Record {
    /// The value under `key`, for tests and for the report's column picking.
    fn get(&self, key: &str) -> Option<f64> {
        self.metrics
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| *v)
    }
}

/// What the run has managed to read, counted by this command rather than by any
/// accumulator.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RunStats {
    /// Polls attempted.
    polls: u64,
    /// Polls that produced a snapshot.
    readable: u64,
    /// Polls that found no game-rules entity to read.
    unreadable: u64,
    /// Polls that failed outright.
    read_errors: u64,
    /// Readable polls whose context was the Hideout.
    hideout_ticks: u64,
    /// Readable polls whose context was a match.
    match_ticks: u64,
    /// Readable polls that were neither: loading, menus, tutorial.
    other_ticks: u64,
    /// Every distinct match id seen. The reader never reports `0`, so an entry here names
    /// a match.
    match_ids: BTreeSet<u64>,
}

impl RunStats {
    fn observe(&mut self, snap: &LiveSnapshot) {
        self.readable += 1;
        match snap.context {
            Context::Hideout => self.hideout_ticks += 1,
            Context::Match => self.match_ticks += 1,
            Context::Other => self.other_ticks += 1,
        }
        if let Some(id) = snap.match_id {
            self.match_ids.insert(id);
        }
    }

    fn json(&self) -> String {
        let ids: Vec<String> = self
            .match_ids
            .iter()
            .map(|id| quote(&id.to_string()))
            .collect();
        Obj::new()
            .uint("polls", self.polls)
            .uint("readable", self.readable)
            .uint("unreadable", self.unreadable)
            .uint("read_errors", self.read_errors)
            .uint("hideout_ticks", self.hideout_ticks)
            .uint("match_ticks", self.match_ticks)
            .uint("other_ticks", self.other_ticks)
            .raw("match_ids", array(&ids))
            .finish()
    }
}

/// What the run actually captured, decided from the contexts it saw.
///
/// The distinction the Hideout case needs: an accumulator that read a Hideout produces the
/// same all-zero shape a match with no activity would, and only the context separates
/// them. Nothing here is inferred - [`LiveSnapshot::context`] is reported by the reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// At least one tick was in a match, or at least one match id was seen.
    Match,
    /// Ticks were readable, none was in a match, and no match id was ever seen.
    NoMatch,
    /// Nothing was readable at all, so there is no evidence either way.
    NothingRead,
}

impl Verdict {
    fn of(run: &RunStats) -> Verdict {
        if run.match_ticks > 0 || !run.match_ids.is_empty() {
            return Verdict::Match;
        }
        if run.readable == 0 {
            return Verdict::NothingRead;
        }
        Verdict::NoMatch
    }

    fn as_str(self) -> &'static str {
        match self {
            Verdict::Match => "match",
            Verdict::NoMatch => "no_match",
            Verdict::NothingRead => "nothing_read",
        }
    }
}

/// One full dump of every accumulator, at one instant.
#[derive(Clone, Debug, PartialEq)]
struct Dump {
    /// Which dump this is, from zero. Joins a `progress` line to its `metrics` lines.
    seq: u64,
    /// Milliseconds since the recording started.
    offset_ms: u64,
    /// The match clock, or [`None`] when it could not be read. Never zero for absent.
    clock: Option<f32>,
    /// The match the trackers believe they are accumulating for.
    match_id: Option<u64>,
    /// The context of the most recent readable tick.
    context: Option<Context>,
    /// Whether this is the last dump of the run.
    is_final: bool,
    run: RunStats,
    records: Vec<Record>,
}

impl Dump {
    /// The envelope every line of this dump repeats, so each line stands alone.
    fn envelope(&self, kind: &str) -> Obj {
        Obj::new()
            .text("kind", kind)
            .uint("seq", self.seq)
            .flag("final", self.is_final)
            .uint("offset_ms", self.offset_ms)
            .opt_number("clock", self.clock.map(f64::from))
            // A string, not a number: match ids are identifiers, and a JSON reader that
            // parses them as doubles starts losing digits. Same choice `TimelineEvent`
            // makes.
            .opt_text(
                "match_id",
                self.match_id.map(|id| id.to_string()).as_deref(),
            )
            .opt_text("context", self.context.map(context_name))
    }

    /// Every JSONL line for this dump: one `progress`, then one `metrics` per scope.
    fn lines(&self, labels: &BTreeMap<u32, PlayerLabel>) -> Vec<String> {
        let mut out = vec![
            self.envelope("progress")
                .text("verdict", Verdict::of(&self.run).as_str())
                .raw("run", self.run.json())
                .uint("records", self.records.len() as u64)
                .finish(),
        ];
        for record in &self.records {
            out.push(self.metrics_line(record, labels));
        }
        out
    }

    fn metrics_line(&self, record: &Record, labels: &BTreeMap<u32, PlayerLabel>) -> String {
        let mut obj = self.envelope("metrics");
        match &record.scope {
            Scope::Match => obj = obj.text("scope", "match"),
            Scope::Player { slot } => {
                let label = labels.get(slot).cloned().unwrap_or_default();
                obj = obj
                    .text("scope", "player")
                    .uint("slot", u64::from(*slot))
                    .opt_text("hero", label.hero.as_deref())
                    .opt_text("player", label.name.as_deref())
                    .opt_text("team", label.team.as_deref());
            }
            Scope::Ability {
                caster,
                key,
                subclass,
                name,
                class_name,
                kind,
            } => {
                obj = obj
                    .text("scope", "ability")
                    .text("caster", &format!("{caster:#010x}"))
                    .text("ability_key", key)
                    .opt_text(
                        "subclass_id",
                        subclass.map(|id| id.get().to_string()).as_deref(),
                    )
                    .opt_text("ability_name", name.as_deref())
                    .opt_text("ability_class_name", class_name.as_deref())
                    .opt_text("ability_kind", kind.as_deref());
            }
        }
        let body: Vec<String> = record
            .metrics
            .iter()
            .map(|(k, v)| format!("{}:{}", quote(k), number(*v)))
            .collect();
        obj.raw("metrics", format!("{{{}}}", body.join(",")))
            .finish()
    }
}

/// The reader's own word for a context. Not a guess; see [`Context`].
fn context_name(c: Context) -> &'static str {
    match c {
        Context::Hideout => "hideout",
        Context::Match => "match",
        Context::Other => "other",
    }
}

/// How a player is named on the terminal and in the file.
///
/// Latched rather than re-read: a hero id that becomes readable on tick 300 should not be
/// lost again on tick 301, and none of these change within a match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PlayerLabel {
    hero: Option<String>,
    name: Option<String>,
    team: Option<String>,
}

/// This command's own per-player tick counting.
///
/// `crowd_control` and `landed_casts` both leave a ledger untouched when
/// `PlayerRow::modifiers` is [`None`], and an untouched ledger reads exactly like a player
/// nothing ever landed on. Neither module exposes a counter to tell the two apart, so this
/// does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SlotTicks {
    read: u64,
    unreadable: u64,
}

/// Every cross-tick accumulator this build has, driven off one snapshot each tick.
#[derive(Debug, Default)]
struct Accumulators {
    crowd_control: CrowdControlTracker,
    landed_casts: LandedCastTracker,
    #[cfg(feature = "positions")]
    objectives: ObjectiveContextTracker,
    ticks: BTreeMap<u32, SlotTicks>,
    labels: BTreeMap<u32, PlayerLabel>,
}

impl Accumulators {
    /// Fold one tick into every accumulator.
    ///
    /// The trackers reset themselves when the match id changes, so a session spanning two
    /// matches does not carry the first one's totals into the second. The tick counts here
    /// are the recording's, not the match's, and deliberately are not reset: they say what
    /// this run was able to read.
    fn observe(&mut self, snap: &LiveSnapshot, heroes: &dyn HeroNames) {
        self.crowd_control.update(snap);
        self.landed_casts.update(snap);
        #[cfg(feature = "positions")]
        self.objectives.update(snap);

        for row in &snap.players {
            let Some(slot) = row.slot else { continue };
            let counts = self.ticks.entry(slot).or_default();
            if row.modifiers.is_some() {
                counts.read += 1;
            } else {
                counts.unreadable += 1;
            }
            let label = self.labels.entry(slot).or_default();
            if let Some(id) = row.hero_id
                && let Some(name) = heroes.hero_name(id)
            {
                label.hero = Some(name.to_owned());
            }
            if let Some(name) = &row.name {
                label.name = Some(name.clone());
            }
            if let Some(team) = &row.team_name {
                label.team = Some(team.clone());
            }
        }
    }

    /// The match the accumulators believe they are accumulating for.
    ///
    /// Taken from a tracker rather than from the last snapshot, so it names the match the
    /// totals below actually belong to.
    fn match_id(&self) -> Option<u64> {
        self.crowd_control.match_id()
    }

    /// Every scope's metrics, in report order: the match, then players by slot, then
    /// abilities by caster.
    fn records(&self, items: &dyn ItemNames) -> Vec<Record> {
        let mut out = Vec::new();
        out.extend(self.match_records());
        out.extend(self.player_records());
        out.extend(self.ability_records(items));
        out
    }

    /// The match-level record: the two trackers' own tick counts, and - where the build
    /// has positions - the three fight counts.
    ///
    /// [`CrowdControlTracker::ticks_observed`] and
    /// [`CrowdControlTracker::ticks_without_modifiers`] are *tracker*-level rather than
    /// per player, and are not in any `metrics()`, so they are read through the accessors
    /// and filed here. They are the difference between "0 crowd control over 4,200 ticks,
    /// all of which read a modifier list" and "0 crowd control over 4,200 ticks, none of
    /// which read anything": the first is a result and the second is a broken capture, and
    /// the file is read back later with nobody watching the client.
    ///
    /// [`CrowdControlTracker::ticks_observed`]: deadlock_events::crowd_control::CrowdControlTracker::ticks_observed
    /// [`CrowdControlTracker::ticks_without_modifiers`]: deadlock_events::crowd_control::CrowdControlTracker::ticks_without_modifiers
    fn match_records(&self) -> Vec<Record> {
        let mut metrics: Vec<(String, Option<f64>)> = vec![
            (
                "dlrs.crowd_control.ticks_observed".to_owned(),
                Some(f64::from(self.crowd_control.ticks_observed())),
            ),
            (
                "dlrs.crowd_control.ticks_without_modifiers".to_owned(),
                Some(f64::from(self.crowd_control.ticks_without_modifiers())),
            ),
            (
                "dlrs.landed_casts.ticks_observed".to_owned(),
                Some(f64::from(self.landed_casts.ticks_observed())),
            ),
            (
                "dlrs.landed_casts.ticks_without_modifiers".to_owned(),
                Some(f64::from(self.landed_casts.ticks_without_modifiers())),
            ),
        ];
        metrics.extend(self.fight_metrics());
        vec![Record {
            scope: Scope::Match,
            metrics,
        }]
    }

    /// The three fight keys, or nothing at all in a build with no world coordinates.
    ///
    /// Nothing, rather than three zeros: without positions the tracker does not exist, and
    /// `urn_fights: 0` from a build that never looked is the exact shape of lie the
    /// `objective-context` feature was made compile-time to avoid.
    #[cfg(feature = "positions")]
    fn fight_metrics(&self) -> Vec<(String, Option<f64>)> {
        self.objectives
            .fights()
            .metrics()
            .into_iter()
            .map(|(k, v)| (k, Some(v)))
            .collect()
    }

    #[cfg(not(feature = "positions"))]
    fn fight_metrics(&self) -> Vec<(String, Option<f64>)> {
        Vec::new()
    }

    fn player_records(&self) -> Vec<Record> {
        let mut slots: BTreeSet<u32> = self.ticks.keys().copied().collect();
        for (slot, _) in self.crowd_control.players() {
            slots.insert(slot);
        }
        #[cfg(feature = "positions")]
        for (slot, _) in self.objectives.players() {
            slots.insert(slot);
        }
        slots
            .into_iter()
            .map(|slot| Record {
                scope: Scope::Player { slot },
                metrics: self.player_metrics(slot),
            })
            .collect()
    }

    fn player_metrics(&self, slot: u32) -> Vec<(String, Option<f64>)> {
        let mut metrics: Vec<(String, Option<f64>)> = Vec::new();
        if let Some(cc) = self.crowd_control.player(slot) {
            metrics.extend(cc.metrics().into_iter().map(|(k, v)| (k, Some(v))));
        }
        #[cfg(feature = "positions")]
        if let Some(buckets) = self.objectives.player(slot) {
            metrics.extend(buckets.metrics().into_iter().map(|(k, v)| (k, Some(v))));
            // `PlayerBuckets::metrics` emits `unattributed.intervals` and not its
            // denominator, and the ratio is the whole point: all intervals unattributable
            // is a match where positions never read.
            metrics.push((
                "dlrs.attributed_intervals".to_owned(),
                Some(f64::from(buckets.attributed_intervals())),
            ));
        }
        let ticks = self.ticks.get(&slot).copied().unwrap_or_default();
        metrics.push(("dlrs.modifier_ticks".to_owned(), Some(ticks.read as f64)));
        metrics.push((
            "dlrs.modifier_ticks_unreadable".to_owned(),
            Some(ticks.unreadable as f64),
        ));
        metrics
    }

    /// One record per (caster, ability), with the id resolved to a name where the roster
    /// has one.
    ///
    /// The keys are `CasterCasts::metrics`'s own, rebuilt through
    /// [`ability_key`] rather than copied, so the raw `ability.subclass_<id>.*` spelling
    /// survives next to the resolved name and a consumer joining on the key still can.
    fn ability_records(&self, items: &dyn ItemNames) -> Vec<Record> {
        let mut out = Vec::new();
        for (caster, casts) in self.landed_casts.casters() {
            for (ability, totals) in casts.totals_by_ability() {
                let key = ability_key(ability);
                let subclass = totals.subclass_id.or_else(|| ability.subclass_id());
                let prefix = format!("ability.{key}");
                let avg = totals.avg_enemies_per_landed_cast();
                out.push(Record {
                    scope: Scope::Ability {
                        caster,
                        key,
                        subclass,
                        name: subclass
                            .and_then(|id| items.item_name(id))
                            .map(str::to_owned),
                        class_name: subclass
                            .and_then(|id| items.item_class_name(id))
                            .map(str::to_owned),
                        kind: subclass
                            .and_then(|id| items.item_kind(id))
                            .map(|k| format!("{k:?}").to_lowercase()),
                    },
                    metrics: vec![
                        (
                            format!("{prefix}.landed_casts"),
                            Some(f64::from(totals.landed_casts)),
                        ),
                        (
                            format!("{prefix}.enemies_hit"),
                            Some(f64::from(totals.enemies_hit)),
                        ),
                        (
                            format!("{prefix}.max_enemies_in_cast"),
                            Some(f64::from(totals.max_enemies_in_cast)),
                        ),
                        (format!("{prefix}.avg_enemies_per_landed_cast"), avg),
                    ],
                });
            }
        }
        out
    }
}

/// Which accumulators this build compiled in, for the header line and the report.
///
/// A build without `positions` is not a match where nothing was attributed, and the file
/// has to say which it was.
fn compiled_in() -> Vec<(&'static str, bool)> {
    vec![
        ("crowd_control", true),
        ("landed_casts", true),
        ("objective_context", cfg!(feature = "positions")),
    ]
}

/// The `header` line: what was compiled in, what the constants were, and what is inferred.
fn header_line(opts: &Opts) -> String {
    let compiled: Vec<String> = compiled_in()
        .iter()
        .map(|(name, on)| format!("{}:{on}", quote(name)))
        .collect();
    let inferred = [
        "landed_casts: the cast grouping is inferred from Companion's output shape, never traced",
        "landed_casts: m_iTeam is read as the applying side, which is inferred",
        "objective_context: radii, bucket precedence and the fight rule are all choices made here",
        "objective_context: urn.time_seconds is alive-while-an-urn-exists, not proximity",
        "dlrs.* keys are this command's own bookkeeping and have no Companion counterpart",
    ];
    let mut obj = Obj::new()
        .text("kind", "header")
        .text("tool", "dlrs metrics")
        .text("tool_version", env!("CARGO_PKG_VERSION"))
        .opt_number("unix_time", unix_time())
        .uint("poll_ms", opts.interval.as_millis() as u64)
        .opt_number("snapshot_secs", Some(opts.snapshot.as_secs_f64()))
        .raw("accumulators", format!("{{{}}}", compiled.join(",")))
        .opt_number(
            "cast_window_seconds",
            Some(f64::from(
                deadlock_events::landed_casts::DEFAULT_CAST_WINDOW,
            )),
        );
    obj = objective_constants(obj);
    obj.raw(
        "inferred",
        array(&inferred.iter().map(|s| quote(s)).collect::<Vec<_>>()),
    )
    .finish()
}

/// The tuning constants `objective_context` chose, when the build has that module.
///
/// Every one of them is a choice made in that module rather than a number read anywhere,
/// which is why they are in the header: a disagreement with Companion's output is only
/// interpretable next to the radii it was produced with.
#[cfg(feature = "positions")]
fn objective_constants(obj: Obj) -> Obj {
    use deadlock_events::objective_context as oc;
    obj.opt_number("midboss_radius", Some(f64::from(oc::MIDBOSS_RADIUS)))
        .opt_number("walker_radius", Some(f64::from(oc::WALKER_RADIUS)))
        .opt_number("rift_radius", Some(f64::from(oc::RIFT_RADIUS)))
        .opt_number("fight_gap_seconds", Some(f64::from(oc::FIGHT_GAP_SECONDS)))
}

#[cfg(not(feature = "positions"))]
fn objective_constants(obj: Obj) -> Obj {
    obj
}

/// Wall-clock seconds since the epoch, so a capture can be lined up against anything else
/// recorded at the same time. [`None`] if the system clock is before the epoch.
fn unix_time() -> Option<f64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs_f64())
}

/// The match clock as `m:ss`, or a dash.
///
/// A dash and never `0:00`: the clock is [`None`] when it could not be read, and `0:00` is
/// the first second of a match.
fn clock_cell(secs: Option<f32>) -> String {
    match secs {
        Some(s) if s.is_finite() && s >= 0.0 => {
            let s = s as u32;
            format!("{}:{:02}", s / 60, s % 60)
        }
        _ => "-".into(),
    }
}

/// A number for a column, or a dash. Absent is never zero.
fn num_cell(v: Option<f64>) -> String {
    match v {
        Some(v) if v.is_finite() => {
            if (v - v.round()).abs() < f64::EPSILON {
                format!("{}", v.round() as i64)
            } else {
                format!("{v:.2}")
            }
        }
        _ => "-".into(),
    }
}

/// The header the progress lines line up under.
fn progress_header() -> String {
    format!(
        "{:>9} {:>7} {:>10} {:>8} {:>8} {:>7} {:>8} {:>7}  {}",
        "offset", "clock", "match", "polls", "readable", "cc_ev", "cc_secs", "casts", "context"
    )
}

/// One dump as a single line, so a forty-minute recording is eighty lines and not eighty
/// screens. The full ledger goes to `--out`, and to stdout once at the end.
fn progress_line(dump: &Dump) -> String {
    let sum = |suffix: &str| -> f64 {
        dump.records
            .iter()
            .filter(|r| matches!(r.scope, Scope::Player { .. }))
            .flat_map(|r| r.metrics.iter())
            .filter(|(k, _)| k.starts_with("cc.received.") && k.ends_with(suffix))
            .filter_map(|(_, v)| *v)
            .sum()
    };
    let casts: f64 = dump
        .records
        .iter()
        .filter_map(Record::get_landed_casts)
        .sum();
    format!(
        "{:>9} {:>7} {:>10} {:>8} {:>8} {:>7} {:>8} {:>7}  {}",
        format!("+{:.1}s", dump.offset_ms as f64 / 1000.0),
        clock_cell(dump.clock),
        opt(dump.match_id),
        dump.run.polls,
        dump.run.readable,
        num_cell(Some(sum(".events"))),
        num_cell(Some(sum(".seconds"))),
        num_cell(Some(casts)),
        opt(dump.context.map(context_name)),
    )
}

impl Record {
    /// `landed_casts` for an ability record, and nothing for any other scope.
    fn get_landed_casts(&self) -> Option<f64> {
        match &self.scope {
            Scope::Ability { key, .. } => self.get(&format!("ability.{key}.landed_casts")),
            _ => None,
        }
    }
}

/// The full report, printed once at the end of a bounded run.
///
/// Deliberately three tables rather than one: crowd control is per player per kind,
/// landed casts are per caster per ability, and objective context is per player per
/// bucket. One table would have to leave most of its cells empty.
fn report(dump: &Dump, labels: &BTreeMap<u32, PlayerLabel>) -> Vec<String> {
    let mut out = verdict_lines(&dump.run);
    out.extend(crowd_control_table(dump, labels));
    out.extend(ability_table(dump));
    out.extend(objective_table(dump, labels));
    out
}

/// What the run captured, said before any numbers so nothing below is read as a match that
/// was not one.
fn verdict_lines(run: &RunStats) -> Vec<String> {
    let ids: Vec<String> = run.match_ids.iter().map(u64::to_string).collect();
    let mut out = Vec::new();
    match Verdict::of(run) {
        Verdict::Match => out.push(format!(
            "recorded a match: {} of {} readable tick(s) were in one, match id(s) {}",
            run.match_ticks,
            run.readable,
            if ids.is_empty() {
                "-".to_owned()
            } else {
                ids.join(", ")
            }
        )),
        Verdict::NoMatch => out.push(format!(
            "NO MATCH WAS RECORDED. {} readable tick(s), none in a match \
             ({} hideout, {} other), and no match id was ever seen. \
             the totals below are not a match's.",
            run.readable, run.hideout_ticks, run.other_ticks
        )),
        Verdict::NothingRead => out.push(format!(
            "NOTHING WAS READ. {} poll(s), {} with no match state and {} that failed. \
             the totals below are empty because nothing was observed, not because \
             nothing happened.",
            run.polls, run.unreadable, run.read_errors
        )),
    }
    let missing: Vec<&str> = compiled_in()
        .iter()
        .filter(|(_, on)| !on)
        .map(|(name, _)| *name)
        .collect();
    if !missing.is_empty() {
        out.push(format!(
            "NOT COMPILED IN: {}. those metrics are absent from this recording, \
             which is not the same as their being zero. rebuild with --features positions.",
            missing.join(", ")
        ));
    }
    out
}

/// What the two modifier-driven trackers were able to read, said before their numbers.
///
/// Both are tracker-level rather than per player, so they come off the match record rather
/// than out of anybody's `metrics()`. Equal counts mean no modifier list was ever read on
/// any player on any tick, which is what a build without `deadlock-reader/modifiers` looks
/// like from the outside - and what a denied read looks like too. Every zero underneath is
/// then a broken capture rather than a result.
fn modifier_capture_lines(dump: &Dump, tracker: &str) -> Vec<String> {
    let Some(record) = dump.records.iter().find(|r| r.scope == Scope::Match) else {
        return Vec::new();
    };
    let observed = record.get(&format!("dlrs.{tracker}.ticks_observed"));
    let blind = record.get(&format!("dlrs.{tracker}.ticks_without_modifiers"));
    let mut out = vec![format!(
        "      {} tick(s) observed, {} of them with no modifier list read on any player",
        num_cell(observed),
        num_cell(blind)
    )];
    if observed.is_some_and(|o| o > 0.0) && observed == blind {
        out.push(
            "      NO MODIFIER LIST WAS EVER READ. every number below is zero because \
             nothing was seen, not because nothing happened - which is also what a build \
             without deadlock-reader/modifiers looks like."
                .to_owned(),
        );
    }
    out
}

/// How a player is titled in a table cell.
fn who(labels: &BTreeMap<u32, PlayerLabel>, slot: u32) -> String {
    let label = labels.get(&slot).cloned().unwrap_or_default();
    trunc(&opt(label.hero.as_deref()), HERO_COLUMN)
}

fn crowd_control_table(dump: &Dump, labels: &BTreeMap<u32, PlayerLabel>) -> Vec<String> {
    let mut out = vec![
        String::new(),
        "crowd control received (events and seconds are different measurements; \
         seconds come from m_flDuration, not from the poll rate)"
            .to_owned(),
    ];
    out.extend(modifier_capture_lines(dump, "crowd_control"));
    out.push(format!(
        "{:>5} {:<14} {:<12} {:>8} {:>9} {:>8} {:>10}",
        "slot", "hero", "kind", "events", "seconds", "mod tk", "unreadable"
    ));
    let mut any = false;
    for record in &dump.records {
        let Scope::Player { slot } = record.scope else {
            continue;
        };
        let read = record.get("dlrs.modifier_ticks");
        let unread = record.get("dlrs.modifier_ticks_unreadable");
        let mut kinds: BTreeSet<String> = BTreeSet::new();
        for (key, _) in &record.metrics {
            if let Some(rest) = key.strip_prefix("cc.received.")
                && let Some(kind) = rest.rsplit_once('.').map(|(k, _)| k)
            {
                kinds.insert(kind.to_owned());
            }
        }
        for kind in kinds {
            any = true;
            out.push(format!(
                "{:>5} {:<14} {:<12} {:>8} {:>9} {:>8} {:>10}",
                slot,
                who(labels, slot),
                kind,
                num_cell(record.get(&format!("cc.received.{kind}.events"))),
                num_cell(record.get(&format!("cc.received.{kind}.seconds"))),
                num_cell(read),
                num_cell(unread),
            ));
        }
        if unread.is_some_and(|u| u > 0.0) && read.is_some_and(|r| r == 0.0) {
            out.push(format!(
                "      slot {slot}: the modifier list never read on any tick. \
                 no crowd control could have been seen, which is why there is none above."
            ));
        }
    }
    if !any {
        out.push("      no crowd control was seen on any player".to_owned());
    }
    out
}

fn ability_table(dump: &Dump) -> Vec<String> {
    let mut out = vec![
        String::new(),
        "ability landed casts (grouping is INFERRED; casters are entity handles, which \
         no accumulator can map to a lobby slot)"
            .to_owned(),
    ];
    out.extend(modifier_capture_lines(dump, "landed_casts"));
    out.push(format!(
        "{:>12} {:<24} {:<22} {:<8} {:>7} {:>8} {:>5} {:>6}",
        "caster", "key", "name", "kind", "casts", "enemies", "max", "avg"
    ));
    let mut any = false;
    for record in &dump.records {
        let Scope::Ability {
            caster,
            key,
            name,
            kind,
            ..
        } = &record.scope
        else {
            continue;
        };
        any = true;
        let prefix = format!("ability.{key}");
        out.push(format!(
            "{:>12} {:<24} {:<22} {:<8} {:>7} {:>8} {:>5} {:>6}",
            format!("{caster:#010x}"),
            trunc(key, 24),
            trunc(&opt(name.as_deref()), ABILITY_COLUMN),
            opt(kind.as_deref()),
            num_cell(record.get(&format!("{prefix}.landed_casts"))),
            num_cell(record.get(&format!("{prefix}.enemies_hit"))),
            num_cell(record.get(&format!("{prefix}.max_enemies_in_cast"))),
            num_cell(record.get(&format!("{prefix}.avg_enemies_per_landed_cast"))),
        ));
    }
    if !any {
        out.push("      no ability landed on an enemy".to_owned());
    }
    out
}

fn objective_table(dump: &Dump, labels: &BTreeMap<u32, PlayerLabel>) -> Vec<String> {
    if !cfg!(feature = "positions") {
        return vec![
            String::new(),
            "objective context: not compiled in (needs --features positions). \
             no bucketing was attempted, so there is nothing to show and no zeros to \
             mistake for measurements."
                .to_owned(),
        ];
    }
    let mut out = vec![
        String::new(),
        "objective context (radii, precedence and the fight rule are all INFERRED; \
         urn.time_seconds is alive-while-an-urn-exists, NOT proximity)"
            .to_owned(),
        format!(
            "{:>5} {:<14} {:<14} {:>6} {:>7} {:>8} {:>9} {:>8} {:>8} {:>9}",
            "slot",
            "hero",
            "bucket",
            "kills",
            "deaths",
            "assists",
            "hero dmg",
            "obj dmg",
            "healing",
            "seconds"
        ),
    ];
    let buckets = ["open", "midboss", "walker", "unattributed"];
    let mut any = false;
    for record in &dump.records {
        let Scope::Player { slot } = record.scope else {
            continue;
        };
        if record.get("unattributed.intervals").is_none() {
            continue;
        }
        any = true;
        for bucket in buckets {
            out.push(format!(
                "{:>5} {:<14} {:<14} {:>6} {:>7} {:>8} {:>9} {:>8} {:>8} {:>9}",
                slot,
                who(labels, slot),
                bucket,
                num_cell(record.get(&format!("{bucket}.kills"))),
                num_cell(record.get(&format!("{bucket}.deaths"))),
                num_cell(record.get(&format!("{bucket}.assists"))),
                num_cell(record.get(&format!("{bucket}.hero_damage"))),
                num_cell(record.get(&format!("{bucket}.objective_damage"))),
                num_cell(record.get(&format!("{bucket}.healing"))),
                num_cell(record.get(&format!("{bucket}.time_seconds"))),
            ));
        }
        out.extend(interval_lines(record, slot));
    }
    if !any {
        out.push("      no player was bucketed".to_owned());
    }
    for record in &dump.records {
        if record.scope != Scope::Match {
            continue;
        }
        out.push(format!(
            "match: urn_fights {}  urn_fight_seconds {}  rift_fights {}",
            num_cell(record.get("urn_fights")),
            num_cell(record.get("urn_fight_seconds")),
            num_cell(record.get("rift_fights")),
        ));
    }
    out
}

/// The attributed/unattributable split, and the sentence that has to be said when the
/// split is all one way.
fn interval_lines(record: &Record, slot: u32) -> Vec<String> {
    let attributed = record.get("dlrs.attributed_intervals");
    let unattributable = record.get("unattributed.intervals");
    let mut out = vec![format!(
        "      slot {slot}: {} interval(s) attributed, {} unattributable, \
         urn.time_seconds {} (rejuv secured {})",
        num_cell(attributed),
        num_cell(unattributable),
        num_cell(record.get("urn.time_seconds")),
        num_cell(record.get("midboss.rejuv_secured")),
    )];
    if attributed == Some(0.0) && unattributable.is_some_and(|u| u > 0.0) {
        out.push(format!(
            "      slot {slot}: POSITIONS NEVER READ. every bucket above is empty for \
             that reason, not because the player did nothing."
        ));
    }
    out
}

/// The `--out` file, and what it has refused.
///
/// Not [`JsonlSink`](deadlock_events::sink::JsonlSink): that writes a `TimelineEvent` and
/// nothing else. The flush policy is the same one it defaults to, and for the same reason -
/// every accepted line is already with the operating system, so ctrl-c loses none of them.
#[derive(Debug)]
struct Out {
    file: std::fs::File,
    path: PathBuf,
    written: u64,
    failed: u64,
    first_error: Option<String>,
    announced: bool,
}

impl Out {
    fn open(path: &Path) -> Result<Out, std::io::Error> {
        // Appends rather than truncates, matching what `JsonlSink::open` does: a second
        // recording into one path must not silently eat the first.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Out {
            file,
            path: path.to_path_buf(),
            written: 0,
            failed: 0,
            first_error: None,
            announced: false,
        })
    }

    /// Offer one line. A refusal is counted and remembered, and never stops the run.
    fn write(&mut self, line: &str) {
        match writeln!(self.file, "{line}") {
            Ok(()) => self.written += 1,
            Err(e) => {
                self.failed += 1;
                if self.first_error.is_none() {
                    self.first_error = Some(e.to_string());
                }
            }
        }
    }

    fn is_healthy(&self) -> bool {
        self.failed == 0
    }

    /// The first write failure, once, so an unbounded run that never reaches its summary
    /// still says out loud that the file is incomplete.
    fn take_alert(&mut self) -> Option<String> {
        if self.announced || self.is_healthy() {
            return None;
        }
        self.announced = true;
        Some(format!(
            "the metrics file refused a line, so {} now has a hole in it; \
             recording continues. first failure: {}",
            self.path.display(),
            opt(self.first_error.as_deref())
        ))
    }

    fn finish(&mut self) -> Option<String> {
        match self.file.flush() {
            Ok(()) => None,
            Err(e) => Some(format!("the metrics file would not flush: {e}")),
        }
    }
}

/// Where dumps go: the file if there is one, and always the terminal.
#[derive(Debug, Default)]
struct Sink {
    out: Option<Out>,
    /// Whether the progress header has been printed.
    headed: bool,
}

impl Sink {
    fn emit(&mut self, dump: &Dump, labels: &BTreeMap<u32, PlayerLabel>) {
        if let Some(out) = &mut self.out {
            for line in dump.lines(labels) {
                out.write(&line);
            }
            if let Some(alert) = out.take_alert() {
                eprintln!("{alert}");
            }
        }
        if !self.headed {
            self.headed = true;
            outln!("{}", progress_header());
        }
        outln!("{}", progress_line(dump));
    }
}

/// Why the loop stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    /// `--for` elapsed.
    Duration,
    /// The client stopped answering.
    ClientGone,
}

/// What the run managed, printed once at the end of a bounded run.
fn summary(dump: &Dump, ended: Ended, out: Option<&Out>) -> Vec<String> {
    let mut lines = vec![match ended {
        Ended::Duration => "stopped: the recording ran for as long as --for asked".to_owned(),
        Ended::ClientGone => "stopped: the client stopped answering".to_owned(),
    }];
    let dumps = dump.seq + 1;
    match out {
        Some(out) => lines.push(format!(
            "wrote {} line(s) over {dumps} dump(s) to {}, {} refused",
            out.written,
            out.path.display(),
            out.failed
        )),
        // Not "wrote": with no --out nothing was written anywhere, and the count is of
        // dumps that went past on the terminal.
        None => lines.push(format!(
            "printed {dumps} dump(s); no --out, so no metrics were written to a file"
        )),
    }
    if let Some(out) = out
        && !out.is_healthy()
    {
        lines.push(format!(
            "THE METRICS FILE HAS {} HOLE(S) IN IT. first: {}",
            out.failed,
            opt(out.first_error.as_deref())
        ));
    }
    lines
}

/// The `summary` JSONL line.
fn summary_line(dump: &Dump, ended: Ended, refused: u64) -> String {
    Obj::new()
        .text("kind", "summary")
        .uint("seq", dump.seq)
        .uint("dumps", dump.seq + 1)
        .uint("offset_ms", dump.offset_ms)
        .text(
            "ended",
            match ended {
                Ended::Duration => "duration",
                Ended::ClientGone => "client_gone",
            },
        )
        .text("verdict", Verdict::of(&dump.run).as_str())
        .raw("run", dump.run.json())
        .uint("refused_lines", refused)
        .finish()
}

/// The process exit code for a finished run.
///
/// Two things make a recording not the recording that was asked for, and each has to be
/// visible to a script that cannot read the summary: a refused line, which is a hole in the
/// file, and a bounded run that ended early because the client went away.
///
/// A run that recorded no match is **not** one of them. Running this in the Hideout is a
/// legitimate thing to do - it is how the command itself is smoke-tested - and the verdict
/// says so loudly enough without also failing the process.
fn exit_code(ended: Ended, bounded: bool, healthy: bool) -> i32 {
    if !healthy {
        return 1;
    }
    match (ended, bounded) {
        (Ended::ClientGone, true) => 1,
        _ => 0,
    }
}

/// Whether a dump is due at `elapsed`, and the deadline that replaces `next` if it is.
///
/// The deadline walks past `elapsed` rather than moving by one `every`, so a run that
/// stalled - a hitching client, a machine that went to sleep - does not then emit a burst
/// of identical dumps catching up on intervals nothing was observed in.
fn due(elapsed: Duration, next: Duration, every: Duration) -> Option<Duration> {
    if elapsed < next {
        return None;
    }
    let mut after = next;
    while after <= elapsed {
        after += every;
    }
    Some(after)
}

/// Poll the client until the run ends, dumping every `--snapshot`.
fn record(reader: &deadlock_reader::Reader, opts: &Opts, sink: &mut Sink) -> (Ended, Dump) {
    let heroes = hero_catalog(Some(reader));
    let items = item_catalog(Some(reader));
    let mut acc = Accumulators::default();
    let mut run = RunStats::default();
    let mut seq = 0u64;
    let mut clock = None;
    let mut context = None;
    let mut consecutive_errors = 0u32;

    let started = Instant::now();
    let mut next_dump = opts.snapshot;

    let ended = loop {
        if opts.duration.is_some_and(|d| started.elapsed() >= d) {
            break Ended::Duration;
        }
        run.polls += 1;
        match reader.live_snapshot() {
            Ok(Some(snapshot)) => {
                consecutive_errors = 0;
                run.observe(&snapshot);
                clock = snapshot.clock.playing_seconds();
                context = Some(snapshot.context);
                acc.observe(&snapshot, &heroes);
            }
            Ok(None) => {
                consecutive_errors = 0;
                run.unreadable += 1;
            }
            Err(e) => {
                consecutive_errors += 1;
                run.read_errors += 1;
                if consecutive_errors == 1 {
                    eprintln!("read failed: {e}");
                }
                if consecutive_errors >= CLIENT_GONE_AFTER {
                    break Ended::ClientGone;
                }
            }
        }
        let elapsed = started.elapsed();
        if let Some(after) = due(elapsed, next_dump, opts.snapshot) {
            let dump = Dump {
                seq,
                offset_ms: elapsed.as_millis() as u64,
                clock,
                match_id: acc.match_id(),
                context,
                is_final: false,
                run: run.clone(),
                records: acc.records(&items),
            };
            sink.emit(&dump, &acc.labels);
            seq += 1;
            next_dump = after;
        }
        std::thread::sleep(opts.interval);
    };

    let dump = Dump {
        seq,
        offset_ms: started.elapsed().as_millis() as u64,
        clock,
        match_id: acc.match_id(),
        context,
        is_final: true,
        run,
        records: acc.records(&items),
    };
    sink.emit(&dump, &acc.labels);
    for line in report(&dump, &acc.labels) {
        outln!("{line}");
    }
    (ended, dump)
}

pub fn metrics(args: &[&str]) -> i32 {
    let opts = match Opts::parse(args) {
        Ok(o) => o,
        Err(why) => {
            eprintln!("{why}");
            return 2;
        }
    };
    let Some(reader) = attach() else { return 1 };

    let mut sink = Sink::default();
    if let Some(path) = opts.out.as_deref() {
        match Out::open(path) {
            Ok(mut out) => {
                // The header goes in before anything else, so a reader of the file knows
                // which accumulators produced what follows.
                out.write(&header_line(&opts));
                sink.out = Some(out);
                outln!("appending JSONL to {}", path.display());
            }
            Err(e) => {
                // The directory has to exist. Creating it would mean writing a recording
                // somewhere the caller did not name.
                eprintln!("could not open {}: {e}", path.display());
                return 1;
            }
        }
    }
    announce(&opts);

    let (ended, dump) = record(&reader, &opts, &mut sink);

    let mut flush_error = None;
    if let Some(out) = &mut sink.out {
        out.write(&summary_line(&dump, ended, out.failed));
        flush_error = out.finish();
    }
    if let Some(why) = &flush_error {
        eprintln!("{why}");
    }
    for row in summary(&dump, ended, sink.out.as_ref()) {
        outln!("{row}");
    }
    let healthy = sink.out.as_ref().is_none_or(Out::is_healthy);
    if flush_error.is_some() {
        return 1;
    }
    exit_code(ended, opts.duration.is_some(), healthy)
}

/// Say how the run will end, and what it will cost, before it starts.
fn announce(opts: &Opts) {
    outln!(
        "polling every {}ms; a full dump every {:.1}s. \
         every tick re-reads every player's modifier list, which roughly doubles what a \
         snapshot costs.",
        opts.interval.as_millis(),
        opts.snapshot.as_secs_f64()
    );
    match opts.duration {
        Some(d) => outln!(
            "recording for {:.1}s (ctrl-c stops it sooner, without the report)",
            d.as_secs_f64()
        ),
        None => outln!("recording until ctrl-c, which ends it without the report"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deadlock_data::ItemCatalog;
    use deadlock_reader::Team;
    use deadlock_reader::snapshot::{MatchClock, Modifier, PlayerRow};

    /// A temp path unique to this process, for the same reason `timeline` has one: this
    /// suite is routinely run more than once at a time, and two runs sharing a fixed path
    /// produce failures that pass when re-run alone.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dlrs-metrics-{}-{name}", std::process::id()))
    }

    /// `ability_bebop_stickybomb2`, which the vendored snapshot names "Sticky Bomb".
    ///
    /// A subclass id that resolves, so the name column can be asserted without a game.
    const STICKY: u32 = 3_303_049_331;

    /// A handle shaped like a real one: a low entity index with a serial in the high bits.
    const CASTER: u32 = 0x0001_8004;
    const ABILITY: u32 = 0x0001_8005;

    fn amber() -> Team {
        Team::AMBER
    }

    fn sapphire() -> Team {
        Team::SAPPHIRE
    }

    /// The vendored roster, which is compiled in unconditionally and needs no game.
    fn catalog() -> ItemCatalog {
        ItemCatalog::bundled()
    }

    fn stun(created: f32, address: u64, subclass: Option<u32>) -> Modifier {
        Modifier {
            address,
            class: Some("CCitadel_Modifier_Stunned".to_owned()),
            subclass_id: subclass,
            serial: Some(1),
            creation_time: Some(created),
            duration: Some(1.5),
            ability: Some(ABILITY),
            caster: Some(CASTER),
            team: Some(amber().get() as u8),
            ..Default::default()
        }
    }

    /// One tick. `players` is `(slot, team, modifier list)`; a [`None`] list is one the
    /// reader could not read.
    fn snapshot(
        now: f32,
        context: Context,
        match_id: Option<u64>,
        players: Vec<(u32, Team, Option<Vec<Modifier>>)>,
    ) -> LiveSnapshot {
        LiveSnapshot {
            match_id,
            context,
            clock: MatchClock {
                now: Some(now),
                ..Default::default()
            },
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

    /// A tracker fed one stun landing on slot 7 from an Amber caster.
    fn hit(now: f32, address: u64, subclass: Option<u32>) -> LiveSnapshot {
        snapshot(
            now,
            Context::Match,
            Some(42),
            vec![(7, sapphire(), Some(vec![stun(now, address, subclass)]))],
        )
    }

    fn dump_of(acc: &Accumulators, run: RunStats) -> Dump {
        Dump {
            seq: 0,
            offset_ms: 0,
            clock: Some(60.0),
            match_id: acc.match_id(),
            context: Some(Context::Match),
            is_final: true,
            run,
            records: acc.records(&catalog()),
        }
    }

    fn matched_run() -> RunStats {
        RunStats {
            polls: 10,
            readable: 10,
            match_ticks: 10,
            match_ids: BTreeSet::from([42]),
            ..RunStats::default()
        }
    }

    fn player_record(dump: &Dump, slot: u32) -> &Record {
        dump.records
            .iter()
            .find(|r| r.scope == Scope::Player { slot })
            .unwrap_or_else(|| panic!("no record for slot {slot}"))
    }

    fn match_record(dump: &Dump) -> &Record {
        dump.records
            .iter()
            .find(|r| r.scope == Scope::Match)
            .expect("no match record")
    }

    fn ability_records(dump: &Dump) -> Vec<&Record> {
        dump.records
            .iter()
            .filter(|r| matches!(r.scope, Scope::Ability { .. }))
            .collect()
    }

    fn columns(row: &str) -> Vec<&str> {
        row.split_whitespace().collect()
    }

    #[test]
    fn no_arguments_prints_to_stdout_and_runs_until_ctrl_c() {
        let o = Opts::parse(&[]).unwrap();
        assert_eq!(o.out, None);
        assert_eq!(o.duration, None);
        assert_eq!(o.interval, DEFAULT_INTERVAL);
        assert_eq!(o.snapshot, DEFAULT_SNAPSHOT);
    }

    #[test]
    fn the_flags_that_change_the_run() {
        let o = Opts::parse(&[
            "--out",
            "m.jsonl",
            "--for",
            "2.5",
            "--every",
            "50",
            "--snapshot",
            "0.5",
        ])
        .unwrap();
        assert_eq!(o.out, Some(PathBuf::from("m.jsonl")));
        assert_eq!(o.duration, Some(Duration::from_millis(2500)));
        assert_eq!(o.interval, Duration::from_millis(50));
        assert_eq!(o.snapshot, Duration::from_millis(500));
    }

    #[test]
    fn the_arguments_that_are_not_a_recording() {
        for args in [
            &["--out"][..],
            &["--for"],
            &["--for", "0"],
            &["--for", "-3"],
            &["--for", "soon"],
            &["--every"],
            &["--every", "0"],
            &["--every", "fast"],
            &["--snapshot"],
            &["--snapshot", "0"],
            &["--snapshot", "-1"],
            &["--snapshot", "often"],
            &["--nope"],
            &["metrics.jsonl"],
        ] {
            assert!(
                Opts::parse(args).is_err(),
                "{args:?} should not parse into a recording"
            );
        }
    }

    /// The other direction: a flag the parser has an arm for has to be listed. See the
    /// note on `documented`, which is what stops an unlisted arm being reachable.
    #[test]
    fn every_flag_the_parser_handles_is_listed() {
        for name in ["--out", "--for", "--every", "--snapshot"] {
            assert!(documented(name), "{name} parses but is not listed");
        }
    }

    #[test]
    fn every_documented_flag_is_accepted() {
        for (spelling, help) in FLAGS {
            assert!(!help.is_empty(), "{spelling} has no description");
            let name = spelling.split_whitespace().next().unwrap();
            let mut args = vec![name];
            if spelling.contains('<') {
                args.push("1");
            }
            assert!(
                Opts::parse(&args).is_ok(),
                "{name} is documented but {args:?} does not parse"
            );
        }
    }

    /// Hero names, Steam names and localised ability names are arbitrary UTF-8 and one of
    /// them is chosen by a stranger. A quote that is not escaped is a file no parser reads.
    #[test]
    fn strings_are_escaped_so_a_hostile_name_cannot_break_the_file() {
        assert_eq!(quote("plain"), "\"plain\"");
        assert_eq!(quote("say \"hi\""), r#""say \"hi\"""#);
        assert_eq!(quote("back\\slash"), r#""back\\slash""#);
        assert_eq!(quote("a\nb\tc\rd"), r#""a\nb\tc\rd""#);
        assert_eq!(quote("\u{1}"), r#""\u0001""#);
        assert_eq!(quote("\u{8}\u{c}"), r#""\b\f""#);
        assert_eq!(quote("Ｂｅｂｏｐ"), "\"Ｂｅｂｏｐ\"");
        let evil = quote(r#"","landed_casts":9999,"x":""#);
        assert_eq!(
            evil.matches(r#"\""#).count(),
            6,
            "every quote in {evil} must be escaped"
        );
    }

    /// `NaN` has no JSON spelling, and writing one makes the whole file unreadable.
    #[test]
    fn a_number_that_is_not_a_number_is_null() {
        assert_eq!(number(Some(1.5)), "1.5");
        assert_eq!(number(Some(0.0)), "0");
        assert_eq!(number(None), "null");
        assert_eq!(number(Some(f64::NAN)), "null");
        assert_eq!(number(Some(f64::INFINITY)), "null");
    }

    #[test]
    fn an_object_keeps_its_keys_in_order() {
        let o = Obj::new()
            .text("kind", "metrics")
            .uint("seq", 3)
            .flag("final", true)
            .opt_number("clock", None)
            .opt_text("match_id", None);
        assert_eq!(
            o.finish(),
            r#"{"kind":"metrics","seq":3,"final":true,"clock":null,"match_id":null}"#
        );
    }

    /// The project rule, on every value this command can fail to have.
    #[test]
    fn an_absent_value_is_a_dash_and_never_a_zero() {
        assert_eq!(clock_cell(None), "-");
        assert_eq!(num_cell(None), "-");
        assert_eq!(num_cell(Some(f64::NAN)), "-");
        assert_eq!(opt(None::<u64>), "-");
        assert_eq!(clock_cell(Some(0.0)), "0:00");
        assert_eq!(num_cell(Some(0.0)), "0");
    }

    /// The same rule on disk: a clock that could not be read is `null`, and a consumer
    /// joining on it must not find a zero there.
    #[test]
    fn an_unreadable_clock_and_match_id_are_null_on_disk() {
        let dump = Dump {
            seq: 0,
            offset_ms: 0,
            clock: None,
            match_id: None,
            context: None,
            is_final: false,
            run: RunStats::default(),
            records: Vec::new(),
        };
        let lines = dump.lines(&BTreeMap::new());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains(r#""clock":null"#), "{}", lines[0]);
        assert!(!lines[0].contains(r#""clock":0"#), "{}", lines[0]);
        assert!(lines[0].contains(r#""match_id":null"#), "{}", lines[0]);
        assert!(lines[0].contains(r#""context":null"#), "{}", lines[0]);
    }

    /// `0.0` is the first second of a match, so it is a reading and not an absence.
    #[test]
    fn second_zero_of_a_match_is_not_an_absent_clock() {
        let dump = Dump {
            seq: 0,
            offset_ms: 0,
            clock: Some(0.0),
            match_id: Some(42),
            context: Some(Context::Match),
            is_final: false,
            run: RunStats::default(),
            records: Vec::new(),
        };
        let lines = dump.lines(&BTreeMap::new());
        assert!(lines[0].contains(r#""clock":0"#), "{}", lines[0]);
        assert!(lines[0].contains(r#""match_id":"42""#), "{}", lines[0]);
    }

    /// Events and seconds are different measurements and the output must not collapse
    /// them: one long stun and three short ones are not the same thing.
    #[test]
    fn crowd_control_reports_events_and_seconds_separately() {
        let mut acc = Accumulators::default();
        acc.observe(&hit(100.0, 0x1000, Some(STICKY)), &hero_names());
        acc.observe(
            &snapshot(
                102.0,
                Context::Match,
                Some(42),
                vec![(7, sapphire(), Some(vec![]))],
            ),
            &hero_names(),
        );
        let dump = dump_of(&acc, matched_run());
        let r = player_record(&dump, 7);
        assert_eq!(r.get("cc.received.stun.events"), Some(1.0));
        assert_eq!(r.get("cc.received.stun.seconds"), Some(1.5));

        let table = crowd_control_table(&dump, &acc.labels);
        let row = table
            .iter()
            .find(|l| l.contains("stun"))
            .unwrap_or_else(|| panic!("{table:#?}"));
        let cells = columns(row);
        assert_eq!(cells[2], "stun");
        assert_eq!(cells[3], "1", "events");
        assert_eq!(cells[4], "1.50", "seconds");
    }

    /// A modifier list that never read is not a player nobody stunned, and the accumulator
    /// cannot tell the difference on its own.
    #[test]
    fn an_unread_modifier_list_is_counted_and_said_out_loud() {
        let mut acc = Accumulators::default();
        for tick in 0u8..3 {
            acc.observe(
                &snapshot(
                    100.0 + f32::from(tick),
                    Context::Match,
                    Some(42),
                    vec![(7, sapphire(), None)],
                ),
                &hero_names(),
            );
        }
        let dump = dump_of(&acc, matched_run());
        let r = player_record(&dump, 7);
        assert_eq!(r.get("dlrs.modifier_ticks"), Some(0.0));
        assert_eq!(r.get("dlrs.modifier_ticks_unreadable"), Some(3.0));

        let table = crowd_control_table(&dump, &acc.labels);
        assert!(
            table.iter().any(|l| l.contains("never read")),
            "a ledger nobody could read must not pass for a quiet one: {table:#?}"
        );
    }

    /// The reason to resolve the id at all: `ability.subclass_3303049331.landed_casts`
    /// A landed cast says whether it was an ability or a shop item.
    ///
    /// `m_nAbilitySubclassID` names whatever applied the modifier, and in Deadlock a bought
    /// item is an ability mechanically — both live in `abilities.vdata_c`. So the ledger
    /// faithfully counts item procs alongside hero abilities. Observed in match
    /// `100650421`: `Mystic Slow`, `Knockdown` and `Spirit Shredder Bullets` appeared beside
    /// `Arctic Beam`, `Powder Keg` and `Assassinate`.
    ///
    /// That is a real difference from the key set this mirrors, which is hero abilities
    /// only, and anyone comparing totals would find this one inflated without knowing
    /// why. The accumulator cannot tell them apart — `deadlock-events` has no edge
    /// to `deadlock-data` — but this command already resolves the id through `ItemNames`,
    /// which knows the kind. So it reports it rather than leaving the reader to recognise
    /// item names by eye.
    #[test]
    fn a_landed_cast_reports_whether_it_was_an_ability_or_an_item() {
        let mut acc = Accumulators::default();
        acc.observe(&hit(100.0, 0x1000, Some(STICKY)), &hero_names());
        let dump = dump_of(&acc, matched_run());

        let kinds: Vec<Option<&str>> = dump
            .records
            .iter()
            .filter_map(|r| match &r.scope {
                Scope::Ability { kind, .. } => Some(kind.as_deref()),
                _ => None,
            })
            .collect();
        assert!(!kinds.is_empty(), "no ability records in the dump");
        assert!(
            kinds.iter().all(Option::is_some),
            "a resolved ability with no kind: {kinds:?}"
        );

        let table = ability_table(&dump);
        assert!(
            table.iter().any(|l| l.contains("kind")),
            "the ability table has no kind column: {table:#?}"
        );
    }

    /// tells a reader nothing, and "Sticky Bomb" tells them everything.
    #[test]
    fn a_subclass_id_is_resolved_to_a_name_with_the_raw_key_kept() {
        let mut acc = Accumulators::default();
        acc.observe(&hit(100.0, 0x1000, Some(STICKY)), &hero_names());
        let dump = dump_of(&acc, matched_run());
        let records = ability_records(&dump);
        assert_eq!(records.len(), 1, "{records:#?}");
        let Scope::Ability {
            key,
            subclass,
            name,
            class_name,
            ..
        } = &records[0].scope
        else {
            panic!("not an ability scope");
        };
        assert_eq!(key, &format!("subclass_{STICKY}"));
        assert_eq!(*subclass, Some(ItemId(STICKY)));
        assert_eq!(name.as_deref(), Some("Sticky Bomb"));
        assert_eq!(class_name.as_deref(), Some("ability_bebop_stickybomb2"));
        assert!(
            records[0]
                .metrics
                .iter()
                .all(|(k, _)| k.starts_with(&format!("ability.subclass_{STICKY}.")))
        );

        let table = ability_table(&dump);
        let row = table
            .iter()
            .find(|l| l.contains(&format!("subclass_{STICKY}")))
            .unwrap_or_else(|| panic!("{table:#?}"));
        assert!(row.contains("Sticky Bomb"), "{row}");
    }

    /// An id nothing resolves is a dash, not a made-up name and not the raw number
    /// wearing a name's clothes.
    #[test]
    fn an_unresolvable_ability_is_a_dash_and_the_key_still_names_it() {
        let mut acc = Accumulators::default();
        acc.observe(&hit(100.0, 0x1000, Some(7)), &hero_names());
        let dump = dump_of(&acc, matched_run());
        let records = ability_records(&dump);
        let Scope::Ability { name, key, .. } = &records[0].scope else {
            panic!("not an ability scope");
        };
        assert_eq!(*name, None);
        assert_eq!(key, "subclass_7");
        let table = ability_table(&dump);
        let row = table.iter().find(|l| l.contains("subclass_7")).unwrap();
        assert!(columns(row).contains(&"-"), "{row}");
    }

    /// No subclass id at all falls back to the handle, which resolves to no name by
    /// construction - a handle names nothing outside this match.
    #[test]
    fn an_ability_with_no_subclass_id_falls_back_to_its_handle() {
        let mut acc = Accumulators::default();
        acc.observe(&hit(100.0, 0x1000, None), &hero_names());
        let dump = dump_of(&acc, matched_run());
        let records = ability_records(&dump);
        let Scope::Ability {
            key,
            subclass,
            name,
            ..
        } = &records[0].scope
        else {
            panic!("not an ability scope");
        };
        assert_eq!(key, &format!("handle_{ABILITY:#010x}"));
        assert_eq!(*subclass, None);
        assert_eq!(*name, None);
    }

    /// The key set is the accumulator's, so a consumer can join this file against anything
    /// else built on `CasterCasts::metrics`.
    #[test]
    fn the_ability_keys_are_the_accumulators_own() {
        let mut acc = Accumulators::default();
        acc.observe(&hit(100.0, 0x1000, Some(STICKY)), &hero_names());
        let dump = dump_of(&acc, matched_run());
        let mine: Vec<String> = ability_records(&dump)
            .iter()
            .flat_map(|r| r.metrics.iter().map(|(k, _)| k.clone()))
            .collect();
        let theirs: Vec<String> = acc
            .landed_casts
            .casters()
            .iter()
            .flat_map(|(_, c)| c.metrics().into_iter().map(|(k, _)| k))
            .collect();
        assert_eq!(mine, theirs);
        assert!(!mine.is_empty(), "the fixture landed nothing");
    }

    /// The one value deliberately not the accumulator's: no landed casts is not an average
    /// of zero enemies per cast, it is no average at all.
    #[test]
    fn an_average_over_no_casts_is_absent_and_not_zero() {
        let record = Record {
            scope: Scope::Ability {
                caster: CASTER,
                key: "subclass_1".to_owned(),
                subclass: Some(ItemId(1)),
                name: None,
                class_name: None,
                kind: None,
            },
            metrics: vec![
                ("ability.subclass_1.landed_casts".to_owned(), Some(0.0)),
                ("ability.subclass_1.enemies_hit".to_owned(), Some(0.0)),
                (
                    "ability.subclass_1.max_enemies_in_cast".to_owned(),
                    Some(0.0),
                ),
                (
                    "ability.subclass_1.avg_enemies_per_landed_cast".to_owned(),
                    None,
                ),
            ],
        };
        let dump = Dump {
            seq: 0,
            offset_ms: 0,
            clock: None,
            match_id: None,
            context: None,
            is_final: true,
            run: RunStats::default(),
            records: vec![record],
        };
        let row = ability_table(&dump)
            .into_iter()
            .find(|l| l.contains("subclass_1"))
            .unwrap();
        assert_eq!(*columns(&row).last().unwrap(), "-", "{row}");
        let line = &dump.lines(&BTreeMap::new())[1];
        assert!(
            line.contains(r#""ability.subclass_1.avg_enemies_per_landed_cast":null"#),
            "{line}"
        );
    }

    /// The instruction the module docs give a consumer, kept: a match where positions
    /// never read looks exactly like a player who did nothing unless the intervals are
    /// shown.
    #[cfg(feature = "positions")]
    #[test]
    fn the_unattributed_bucket_and_both_interval_counts_are_surfaced() {
        let mut acc = Accumulators::default();
        for tick in 0u8..4 {
            acc.observe(
                &snapshot(
                    100.0 + f32::from(tick) * 0.5,
                    Context::Match,
                    Some(42),
                    vec![(7, sapphire(), Some(vec![]))],
                ),
                &hero_names(),
            );
        }
        let dump = dump_of(&acc, matched_run());
        let r = player_record(&dump, 7);
        assert_eq!(r.get("dlrs.attributed_intervals"), Some(0.0));
        assert!(
            r.get("unattributed.intervals").is_some_and(|v| v > 0.0),
            "{:#?}",
            r.metrics
        );
        assert!(r.get("unattributed.kills").is_some());

        let table = objective_table(&dump, &acc.labels);
        assert!(
            table.iter().any(|l| l.contains("unattributed")),
            "{table:#?}"
        );
        assert!(
            table.iter().any(|l| l.contains("POSITIONS NEVER READ")),
            "a match where positions never read must not read as a quiet player: {table:#?}"
        );
        assert!(
            table.iter().any(|l| l.contains("interval(s) attributed")),
            "{table:#?}"
        );
    }

    /// The match-level keys are the accumulator's three, and they are emitted even when
    /// they are all zero, because a closed set of zeros is a measurement.
    #[cfg(feature = "positions")]
    #[test]
    fn the_match_record_carries_the_fight_counts() {
        let acc = Accumulators::default();
        let dump = dump_of(&acc, matched_run());
        let r = match_record(&dump);
        let keys: Vec<&str> = r.metrics.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            keys.ends_with(&["urn_fights", "urn_fight_seconds", "rift_fights"]),
            "{keys:?}"
        );
    }

    /// The counters are tracker-level and are in no `metrics()`, so they have to be read
    /// through the accessors or they are simply not in the file.
    #[test]
    fn the_match_record_carries_both_trackers_tick_counts() {
        let mut acc = Accumulators::default();
        acc.observe(&hit(100.0, 0x1000, Some(STICKY)), &hero_names());
        acc.observe(&hit(100.5, 0x1000, Some(STICKY)), &hero_names());
        let dump = dump_of(&acc, matched_run());
        let r = match_record(&dump);
        for tracker in ["crowd_control", "landed_casts"] {
            assert_eq!(
                r.get(&format!("dlrs.{tracker}.ticks_observed")),
                Some(2.0),
                "{tracker}: {:#?}",
                r.metrics
            );
            assert_eq!(
                r.get(&format!("dlrs.{tracker}.ticks_without_modifiers")),
                Some(0.0),
                "{tracker}"
            );
        }
        let line = dump
            .lines(&acc.labels)
            .into_iter()
            .find(|l| l.contains(r#""scope":"match""#))
            .expect("no match line");
        assert!(
            line.contains(r#""dlrs.crowd_control.ticks_observed":2"#),
            "{line}"
        );
        assert!(
            line.contains(r#""dlrs.landed_casts.ticks_without_modifiers":0"#),
            "{line}"
        );
    }

    /// The finding this exists for: a capture where the modifier list never read produces
    /// exactly the numbers a quiet match produces, and only these counters separate them.
    #[test]
    fn a_capture_that_never_read_a_modifier_list_says_so_in_both_tables() {
        let mut acc = Accumulators::default();
        for tick in 0u8..5 {
            acc.observe(
                &snapshot(
                    100.0 + f32::from(tick),
                    Context::Match,
                    Some(42),
                    vec![(7, sapphire(), None)],
                ),
                &hero_names(),
            );
        }
        let dump = dump_of(&acc, matched_run());
        let r = match_record(&dump);
        assert_eq!(r.get("dlrs.crowd_control.ticks_observed"), Some(5.0));
        assert_eq!(
            r.get("dlrs.crowd_control.ticks_without_modifiers"),
            Some(5.0)
        );

        for table in [
            crowd_control_table(&dump, &acc.labels),
            ability_table(&dump),
        ] {
            assert!(
                table
                    .iter()
                    .any(|l| l.contains("NO MODIFIER LIST WAS EVER READ")),
                "a blind capture must not read as a quiet match: {table:#?}"
            );
            assert!(
                table.iter().any(|l| l.contains("tick(s) observed")),
                "{table:#?}"
            );
        }
    }

    /// The other direction: a match where the lists read and nothing landed is a result,
    /// and must not be shouted about.
    #[test]
    fn a_quiet_match_that_did_read_its_modifiers_is_not_called_blind() {
        let mut acc = Accumulators::default();
        for tick in 0u8..5 {
            acc.observe(
                &snapshot(
                    100.0 + f32::from(tick),
                    Context::Match,
                    Some(42),
                    vec![(7, sapphire(), Some(Vec::new()))],
                ),
                &hero_names(),
            );
        }
        let dump = dump_of(&acc, matched_run());
        let r = match_record(&dump);
        assert_eq!(r.get("dlrs.crowd_control.ticks_observed"), Some(5.0));
        assert_eq!(
            r.get("dlrs.crowd_control.ticks_without_modifiers"),
            Some(0.0)
        );
        let table = crowd_control_table(&dump, &acc.labels);
        assert!(
            !table.iter().any(|l| l.contains("NO MODIFIER LIST")),
            "{table:#?}"
        );
    }

    /// A build with no `positions` must not print an objective table full of zeros: it did
    /// not measure and find nothing, it did not measure.
    #[cfg(not(feature = "positions"))]
    #[test]
    fn without_positions_the_objective_table_says_so_instead_of_showing_zeros() {
        let acc = Accumulators::default();
        let dump = dump_of(&acc, matched_run());
        let table = objective_table(&dump, &acc.labels);
        assert!(
            table.iter().any(|l| l.contains("not compiled in")),
            "{table:#?}"
        );
        assert!(
            !table.iter().any(|l| l.contains("unattributed")),
            "{table:#?}"
        );
        assert!(
            verdict_lines(&matched_run())
                .iter()
                .any(|l| l.contains("NOT COMPILED IN"))
        );
    }

    /// The Hideout is readable, has players and a clock, and produces the same all-zero
    /// shape a quiet match would. Only the context tells them apart, so only the context
    /// decides what the report claims.
    #[test]
    fn a_hideout_recording_does_not_claim_to_be_a_match() {
        let run = RunStats {
            polls: 20,
            readable: 20,
            hideout_ticks: 20,
            ..RunStats::default()
        };
        assert_eq!(Verdict::of(&run), Verdict::NoMatch);
        let lines = verdict_lines(&run);
        assert!(
            lines.iter().any(|l| l.contains("NO MATCH WAS RECORDED")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("recorded a match")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_match_tick_or_a_match_id_is_what_makes_it_a_match() {
        let by_context = RunStats {
            polls: 5,
            readable: 5,
            match_ticks: 5,
            ..RunStats::default()
        };
        assert_eq!(Verdict::of(&by_context), Verdict::Match);
        let by_id = RunStats {
            polls: 5,
            readable: 5,
            other_ticks: 5,
            match_ids: BTreeSet::from([34_851_234]),
            ..RunStats::default()
        };
        assert_eq!(Verdict::of(&by_id), Verdict::Match);
        assert!(verdict_lines(&by_id).iter().any(|l| l.contains("34851234")));
    }

    /// Nothing readable is not "no match": there is no evidence either way, and the two
    /// have different fixes.
    #[test]
    fn nothing_readable_is_reported_as_nothing_read() {
        let run = RunStats {
            polls: 9,
            unreadable: 7,
            read_errors: 2,
            ..RunStats::default()
        };
        assert_eq!(Verdict::of(&run), Verdict::NothingRead);
        let lines = verdict_lines(&run);
        assert!(
            lines.iter().any(|l| l.contains("NOTHING WAS READ")),
            "{lines:?}"
        );
    }

    #[test]
    fn the_verdict_reaches_the_file() {
        let dump = Dump {
            seq: 0,
            offset_ms: 0,
            clock: None,
            match_id: None,
            context: Some(Context::Hideout),
            is_final: true,
            run: RunStats {
                polls: 3,
                readable: 3,
                hideout_ticks: 3,
                ..RunStats::default()
            },
            records: Vec::new(),
        };
        let progress = &dump.lines(&BTreeMap::new())[0];
        assert!(progress.contains(r#""verdict":"no_match""#), "{progress}");
        assert!(progress.contains(r#""hideout_ticks":3"#), "{progress}");
        assert!(progress.contains(r#""context":"hideout""#), "{progress}");
        let summary = summary_line(&dump, Ended::Duration, 0);
        assert!(summary.contains(r#""verdict":"no_match""#), "{summary}");
    }

    /// A file that does not say which accumulators produced it cannot be read six months
    /// later, and a missing accumulator is not a zero one.
    #[test]
    fn the_header_says_what_was_compiled_in_and_what_is_inferred() {
        let line = header_line(&Opts::default());
        assert!(line.starts_with(r#"{"kind":"header""#), "{line}");
        assert!(line.contains(r#""crowd_control":true"#), "{line}");
        assert!(line.contains(r#""landed_casts":true"#), "{line}");
        #[cfg(feature = "positions")]
        assert!(line.contains(r#""objective_context":true"#), "{line}");
        #[cfg(not(feature = "positions"))]
        assert!(line.contains(r#""objective_context":false"#), "{line}");
        assert!(line.contains(r#""cast_window_seconds":0.25"#), "{line}");
        assert!(line.contains("inferred"), "{line}");
        assert!(line.contains(r#""snapshot_secs":30"#), "{line}");
    }

    /// One line per scope, each carrying the whole envelope, so a file cut off mid-dump is
    /// still entirely parseable and no line has to be read in the light of another.
    #[test]
    fn every_line_of_a_dump_stands_alone() {
        let mut acc = Accumulators::default();
        acc.observe(&hit(100.0, 0x1000, Some(STICKY)), &hero_names());
        let dump = dump_of(&acc, matched_run());
        let lines = dump.lines(&acc.labels);
        assert!(lines.len() > 2, "{lines:#?}");
        for line in &lines {
            assert!(line.contains(r#""seq":0"#), "{line}");
            assert!(line.contains(r#""match_id":"42""#), "{line}");
            assert!(line.contains(r#""clock":60"#), "{line}");
            assert!(line.contains(r#""final":true"#), "{line}");
        }
        assert!(lines[0].contains(r#""kind":"progress""#), "{}", lines[0]);
        assert!(lines[1].contains(r#""kind":"metrics""#), "{}", lines[1]);
        assert!(
            lines[0].contains(&format!(r#""records":{}"#, dump.records.len())),
            "{}",
            lines[0]
        );
    }

    /// A player line carries who the slot is, because a slot number cannot be compared
    /// against the game's own post-match scoreboard and a hero name can.
    #[test]
    fn a_player_line_names_the_player() {
        let labels = BTreeMap::from([(
            7,
            PlayerLabel {
                hero: Some("Bebop".to_owned()),
                name: Some("say \"hi\"".to_owned()),
                team: Some("Sapphire".to_owned()),
            },
        )]);
        let dump = Dump {
            seq: 1,
            offset_ms: 30_000,
            clock: Some(61.0),
            match_id: Some(42),
            context: Some(Context::Match),
            is_final: false,
            run: matched_run(),
            records: vec![Record {
                scope: Scope::Player { slot: 7 },
                metrics: vec![("cc.received.stun.events".to_owned(), Some(2.0))],
            }],
        };
        let line = &dump.lines(&labels)[1];
        assert!(line.contains(r#""slot":7"#), "{line}");
        assert!(line.contains(r#""hero":"Bebop""#), "{line}");
        assert!(line.contains(r#""team":"Sapphire""#), "{line}");
        assert!(line.contains(r#""player":"say \"hi\"""#), "{line}");
        assert!(
            line.contains(r#""metrics":{"cc.received.stun.events":2}"#),
            "{line}"
        );
    }

    /// A slot with no label is a dash, not an empty column and not a guess.
    #[test]
    fn an_unnamed_slot_is_a_dash() {
        let labels = BTreeMap::new();
        assert_eq!(who(&labels, 3), "-");
        let dump = Dump {
            seq: 0,
            offset_ms: 0,
            clock: None,
            match_id: None,
            context: None,
            is_final: true,
            run: RunStats::default(),
            records: vec![Record {
                scope: Scope::Player { slot: 3 },
                metrics: Vec::new(),
            }],
        };
        let line = &dump.lines(&labels)[1];
        assert!(line.contains(r#""hero":null"#), "{line}");
        assert!(line.contains(r#""player":null"#), "{line}");
    }

    #[test]
    fn the_progress_header_lines_up_with_a_progress_line() {
        let dump = Dump {
            seq: 0,
            offset_ms: 30_000,
            clock: Some(612.5),
            match_id: Some(34_851_234),
            context: Some(Context::Match),
            is_final: false,
            run: matched_run(),
            records: Vec::new(),
        };
        let (head, row) = (progress_header(), progress_line(&dump));
        assert_eq!(columns(&head).len(), columns(&row).len(), "{head}\n{row}");
        assert_eq!(columns(&row)[1], "10:12");
        assert_eq!(columns(&row)[2], "34851234");
        assert_eq!(*columns(&row).last().unwrap(), "match");
    }

    /// The progress line is a running total, not a delta, so it has to move with the
    /// ledger.
    #[test]
    fn the_progress_line_totals_what_the_accumulators_hold() {
        let mut acc = Accumulators::default();
        acc.observe(&hit(100.0, 0x1000, Some(STICKY)), &hero_names());
        acc.observe(
            &snapshot(
                102.0,
                Context::Match,
                Some(42),
                vec![(7, sapphire(), Some(vec![]))],
            ),
            &hero_names(),
        );
        let dump = dump_of(&acc, matched_run());
        let row = progress_line(&dump);
        let cells = columns(&row);
        assert_eq!(cells[5], "1", "one crowd control event");
        assert_eq!(cells[6], "1.50", "its seconds");
        assert_eq!(cells[7], "1", "one landed cast");
    }

    /// The whole reason `--snapshot` exists: a run that emits only a final dump has lost
    /// the shape of the match, which is the thing the periodic dump is for.
    #[test]
    fn a_dump_is_due_once_per_interval_and_not_before() {
        let every = Duration::from_secs(30);
        let next = every;
        assert_eq!(due(Duration::from_secs(0), next, every), None);
        assert_eq!(due(Duration::from_secs(29), next, every), None);
        assert_eq!(
            due(Duration::from_secs(30), next, every),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            due(Duration::from_millis(30_100), next, every),
            Some(Duration::from_secs(60))
        );
    }

    /// A stall must not then produce one dump per interval it slept through: they would
    /// all be the same ledger, and the file would say a burst of activity happened where
    /// nothing was observed at all.
    #[test]
    fn a_stalled_run_does_not_emit_a_burst_of_catch_up_dumps() {
        let every = Duration::from_secs(30);
        let after = due(Duration::from_secs(75), Duration::from_secs(30), every)
            .expect("a dump is overdue");
        assert_eq!(after, Duration::from_secs(90));
        assert_eq!(due(Duration::from_secs(75), after, every), None);
    }

    #[test]
    fn a_refused_line_does_not_end_the_recording_and_is_announced_once() {
        let path = scratch("refuse.jsonl");
        let _ = std::fs::remove_file(&path);
        let mut out = Out::open(&path).unwrap();
        out.write("{}");
        drop(std::mem::replace(&mut out.file, unwritable()));
        out.write("{}");
        out.write("{}");
        assert_eq!(out.written, 1);
        assert_eq!(out.failed, 2);
        assert!(!out.is_healthy());
        let alert = out.take_alert().expect("a refused line must be announced");
        assert!(alert.contains("hole"), "{alert}");
        assert_eq!(out.take_alert(), None, "the alert must not repeat");
        let _ = std::fs::remove_file(&path);
    }

    /// A file opened for reading refuses every write, which is how the test above gets a
    /// failing sink without needing a full disk.
    fn unwritable() -> std::fs::File {
        let path = scratch("unwritable");
        let _ = std::fs::write(&path, b"");
        std::fs::File::open(&path).expect("the scratch file was just written")
    }

    #[test]
    fn a_healthy_file_never_alerts() {
        let path = scratch("healthy.jsonl");
        let _ = std::fs::remove_file(&path);
        let mut out = Out::open(&path).unwrap();
        out.write("{}");
        assert!(out.is_healthy());
        assert_eq!(out.take_alert(), None);
        assert_eq!(out.finish(), None);
        let _ = std::fs::remove_file(&path);
    }

    /// `Out::open` appends, so a second recording into the same path must not truncate the
    /// first.
    #[test]
    fn a_second_recording_appends() {
        let path = scratch("append.jsonl");
        let _ = std::fs::remove_file(&path);
        for _ in 0..2 {
            let mut out = Out::open(&path).unwrap();
            out.write(&header_line(&Opts::default()));
            assert_eq!(out.finish(), None);
        }
        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(written.lines().filter(|l| !l.is_empty()).count(), 2);
    }

    /// A path whose directory does not exist is an error the command reports, not a
    /// recording that silently keeps nothing.
    #[test]
    fn an_unopenable_out_path_is_an_error() {
        let path = scratch("no-such-dir").join("m.jsonl");
        assert!(Out::open(&path).is_err());
        let arg = path.to_string_lossy().into_owned();
        assert_eq!(metrics(&["--out", arg.as_str(), "--for", "1"]), 1);
    }

    #[test]
    fn the_summary_says_where_the_dumps_went() {
        let path = scratch("summary.jsonl");
        let _ = std::fs::remove_file(&path);
        let dump = Dump {
            seq: 2,
            offset_ms: 90_000,
            clock: Some(90.0),
            match_id: Some(42),
            context: Some(Context::Match),
            is_final: true,
            run: matched_run(),
            records: Vec::new(),
        };
        let mut out = Out::open(&path).unwrap();
        out.write("{}");
        let to_file = summary(&dump, Ended::Duration, Some(&out));
        assert!(
            to_file.iter().any(|l| l.contains("3 dump(s)")),
            "{to_file:?}"
        );
        assert!(
            to_file.iter().any(|l| l.contains("summary.jsonl")),
            "{to_file:?}"
        );

        let to_stdout = summary(&dump, Ended::Duration, None);
        assert!(
            to_stdout.iter().any(|l| l.contains("printed 3 dump(s)")),
            "{to_stdout:?}"
        );
        assert!(
            !to_stdout.iter().any(|l| l.contains("wrote")),
            "with no --out nothing was written anywhere: {to_stdout:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_hole_in_the_file_is_a_failed_run() {
        for bounded in [true, false] {
            for ended in [Ended::Duration, Ended::ClientGone] {
                assert_eq!(exit_code(ended, bounded, false), 1);
            }
        }
    }

    #[test]
    fn a_short_recording_is_only_a_failure_when_a_length_was_asked_for() {
        assert_eq!(exit_code(Ended::Duration, true, true), 0);
        assert_eq!(exit_code(Ended::ClientGone, true, true), 1);
        assert_eq!(exit_code(Ended::ClientGone, false, true), 0);
    }

    /// Recording no match is loud, and is not a process failure: the Hideout smoke test
    /// runs exactly this way.
    #[test]
    fn recording_no_match_is_not_an_error_exit() {
        assert_eq!(exit_code(Ended::Duration, true, true), 0);
    }

    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn a_bounded_recording_against_the_live_client_writes_a_file() {
        let path = scratch("live.jsonl");
        let _ = std::fs::remove_file(&path);
        let arg = path.to_string_lossy().into_owned();
        let rc = metrics(&["--out", arg.as_str(), "--for", "4", "--snapshot", "1"]);
        assert_eq!(rc, 0, "a clean three-second recording should exit 0");
        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let lines: Vec<&str> = written.lines().filter(|l| !l.is_empty()).collect();
        assert!(lines.len() >= 3, "{written}");
        assert!(lines[0].contains(r#""kind":"header""#), "{}", lines[0]);
        let progress = lines
            .iter()
            .filter(|l| l.contains(r#""kind":"progress""#))
            .count();
        assert!(progress >= 3, "only {progress} dump(s) in {written}");
        assert!(
            lines.last().unwrap().contains(r#""kind":"summary""#),
            "{written}"
        );
        let verdicts = [
            "\"verdict\":\"match\"",
            "\"verdict\":\"no_match\"",
            "\"verdict\":\"nothing_read\"",
        ];
        assert!(
            verdicts.iter().any(|v| written.contains(v)),
            "no verdict in {written}"
        );
    }

    /// A hero-name source that needs no game, so the fixtures above are deterministic.
    fn hero_names() -> impl HeroNames {
        struct None_;
        impl HeroNames for None_ {
            fn hero_name(&self, _: deadlock_core::HeroId) -> Option<&str> {
                Option::None
            }
        }
        None_
    }
}
