//! `dlrs timeline`: record the live event stream as a timeline.
//!
//! Polls the client, feeds each snapshot to an [`EventTracker`], and hands what comes out
//! to a [`TimelineRecorder`]. The events and the match clock come from **the same
//! snapshot**, which is why this drives the tracker itself rather than going through
//! `deadlock_events::Engine`: the engine delivers events without the snapshot they were
//! diffed from, so the clock would have to come from a second, later read, and
//! [`TimelineEvent::clock`] would then name an instant the event did not happen at.
//!
//! # What it prints, and where it writes
//!
//! The default is stdout and nothing else. A command that starts writing to the user's
//! disk because it was run is a command that surprises people; `--out <path>` is how a
//! caller asks for a file, and it appends rather than truncating, matching what
//! [`JsonlSink::open`] does.
//!
//! A tick with no events prints nothing. The two state lines print on the transition and
//! not on every poll, so an idle client at 10 Hz is silent rather than a hundred lines a
//! second.
//!
//! Those two lines say "match state is readable" and not "in a match", because they are
//! not the same claim and the difference is measured: an idle client in the Hideout has a
//! readable game-rules entity, and `dlrs timeline` against one records a `match_started`
//! whose match id is `0` and whose clock is the session's own uptime. That comes from the
//! reader, not from here - `EventTracker` fires `MatchStarted` for any match id it can
//! read, including zero - and this command records what it is told rather than deciding
//! which ids are real.
//!
//! # How the loop ends
//!
//! Two ways, and only two:
//!
//! - `--for <secs>` elapses. This is the bounded run, and the only one that reaches the
//!   summary, so it is the one to use in a script.
//! - The client stops answering for [`CLIENT_GONE_AFTER`] consecutive polls.
//!
//! Without `--for` the command runs until ctrl-c, like `dlrs watch` and `dlrs events`.
//! **Ctrl-c kills the process before the summary is printed**, which is why the first
//! write failure is announced on stderr the moment it happens rather than only at the end,
//! and why the file sink is left on its default flush policy, `EveryRecord`: every record
//! the sink accepted is already with the operating system, so a ctrl-c loses none of them.
//! Handling the signal properly would need a dependency this workspace does not have.
//!
//! Leaving a match does not end the recording. A session can span several matches, and
//! [`TimelineRecorder`] tracks the match id across them; that is the whole reason
//! [`TimelineEvent::offset_ms`] exists alongside the match clock.
//!
//! # What a failed write does
//!
//! It does not stop the recording, and it is not swallowed either.
//!
//! A live timeline cannot be re-recorded: the match that is being written down is
//! happening now. So one refused record must not throw away the rest of the match, and
//! every subsequent event is still offered to the sink. But a partial timeline that looks
//! complete is worse than no timeline, so the failure is surfaced three times over: on
//! stderr the first time it happens, in the summary at the end, and in the exit code,
//! which is `1` if a single record was refused.
//!
//! [`EventTracker`]: deadlock_reader::events::EventTracker

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use deadlock_events::sink::JsonlSink;
use deadlock_events::timeline::{
    EventSink, RecorderStatus, SinkError, TimelineEvent, TimelineRecorder,
};
use deadlock_reader::events::{Event as MatchEvent, EventTracker};

use super::attach;
use crate::output::opt;

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
        "stop after <secs> and print a summary; default is until ctrl-c",
    ),
    ("--every <ms>", "poll interval; default 100"),
    (
        "--quiet",
        "do not print records, only write them; needs --out",
    ),
];

/// Default poll interval, matching `deadlock_events::DEFAULT_MATCH_INTERVAL`.
///
/// Well inside the 12-to-19-second window in which a finished match is still readable,
/// which is what bounds how slow this may be.
const DEFAULT_INTERVAL: Duration = Duration::from_millis(100);

/// Consecutive failed reads before the client is presumed gone.
///
/// Twenty polls is two seconds at the default interval. **Inferred, not measured**: the
/// number is chosen so a single denied read or a half-unmapped page does not end a
/// recording, and is not tuned against an observed failure distribution.
const CLIENT_GONE_AFTER: u32 = 20;

/// What `dlrs timeline` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Opts {
    /// Where to append JSONL, or [`None`] for stdout only.
    out: Option<PathBuf>,
    /// How long to record, or [`None`] to run until ctrl-c.
    duration: Option<Duration>,
    /// How often to poll.
    interval: Duration,
    /// Suppress the printed rows.
    quiet: bool,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            out: None,
            duration: None,
            interval: DEFAULT_INTERVAL,
            quiet: false,
        }
    }
}

impl Opts {
    /// Parse the arguments after `timeline`.
    fn parse(args: &[&str]) -> Result<Opts, String> {
        let mut opts = Opts::default();
        let mut i = 0;
        while i < args.len() {
            if args[i].starts_with('-') && !documented(args[i]) {
                return Err(format!("unknown flag {:?}", args[i]));
            }
            match args[i] {
                "--out" => opts.out = Some(PathBuf::from(value(args, &mut i, "--out")?)),
                "--for" => {
                    let raw = value(args, &mut i, "--for")?;
                    let secs: f64 = raw
                        .parse()
                        .map_err(|_| format!("--for wants seconds, got {raw:?}"))?;
                    if !(secs.is_finite() && secs > 0.0) {
                        return Err("--for wants a positive number of seconds".to_owned());
                    }
                    opts.duration = Some(Duration::from_secs_f64(secs));
                }
                "--every" => {
                    let raw = value(args, &mut i, "--every")?;
                    let ms: u64 = raw
                        .parse()
                        .map_err(|_| format!("--every wants milliseconds, got {raw:?}"))?;
                    if ms == 0 {
                        // A zero interval is a busy loop that reads the game's memory as
                        // fast as the kernel will allow, which is not a poll interval.
                        return Err("--every 0 would spin".to_owned());
                    }
                    opts.interval = Duration::from_millis(ms);
                }
                "--quiet" => opts.quiet = true,
                other => return Err(format!("unknown argument {other:?}")),
            }
            i += 1;
        }
        if opts.quiet && opts.out.is_none() {
            // Otherwise the command would poll the game, record nothing anywhere, and
            // print nothing about it.
            return Err("--quiet needs --out, or there is nowhere for a record to go".to_owned());
        }
        Ok(opts)
    }
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

/// Why the loop stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    /// `--for` elapsed.
    Duration,
    /// The client stopped answering.
    ClientGone,
}

/// A sink that accepts everything and keeps nothing.
///
/// What `dlrs timeline` records into when there is no `--out`. It is not a hole in the
/// reporting: the summary says "printed" rather than "recorded" in that case, because
/// nothing was written anywhere and claiming otherwise would be the same mistake as
/// rendering an absent value as zero.
struct NullSink;

impl EventSink for NullSink {
    fn write(&mut self, _: &TimelineEvent) -> Result<(), SinkError> {
        Ok(())
    }
}

/// The match clock as `m:ss`, or a dash.
///
/// A dash and never `0:00`: [`TimelineEvent::clock`] is [`None`] when the clock could not
/// be read, and `0:00` is the first second of a match.
fn clock_cell(secs: Option<f32>) -> String {
    match secs {
        Some(s) if s.is_finite() && s >= 0.0 => {
            let s = s as u32;
            format!("{}:{:02}", s / 60, s % 60)
        }
        _ => "-".into(),
    }
}

/// The column header the rows line up under.
fn header() -> String {
    format!(
        "{:>7} {:>9} {:>5}  {:<24} {:>4}  {}",
        "clock", "offset", "seq", "kind", "slot", "match"
    )
}

/// One recorded event as a row.
///
/// Both clocks are shown, because they answer different questions and the file keeps both:
/// `clock` locates the event in the match and can be absent, `offset` locates it in the
/// recording and never can.
fn line(e: &TimelineEvent) -> String {
    format!(
        "{:>7} {:>9} {:>5}  {:<24} {:>4}  {}",
        clock_cell(e.clock),
        format!("+{:.1}s", e.offset_ms as f64 / 1000.0),
        e.seq,
        e.kind.as_str(),
        opt(e.slot),
        opt(e.match_id),
    )
}

/// What the run managed, printed once at the end of a bounded run.
fn summary(status: &RecorderStatus, ended: Ended, out: Option<&Path>) -> Vec<String> {
    let mut lines = vec![match ended {
        Ended::Duration => "stopped: the recording ran for as long as --for asked".to_owned(),
        Ended::ClientGone => "stopped: the client stopped answering".to_owned(),
    }];
    match out {
        Some(path) => lines.push(format!(
            "recorded {} record(s) to {}, {} refused",
            status.recorded,
            path.display(),
            status.failed
        )),
        // Not "recorded": with no --out nothing was written anywhere, and the count is of
        // rows that went past on the terminal.
        None => lines.push(format!(
            "printed {} record(s); no --out, so none were written to a file",
            status.recorded
        )),
    }
    if !status.is_healthy() {
        lines.push(format!(
            "THE TIMELINE HAS {} HOLE(S) IN IT. first: {}",
            status.failed,
            opt(status.first_error.as_deref())
        ));
        if status.last_error != status.first_error {
            lines.push(format!("last: {}", opt(status.last_error.as_deref())));
        }
    }
    lines
}

/// The process exit code for a finished run.
///
/// Three things can make a recording not be the recording that was asked for, and each
/// has to be visible to a script that cannot read the summary:
///
/// - a refused record, which is a hole in the file;
/// - a bounded run that ended early because the client went away, which is a recording
///   shorter than the one requested;
/// - neither, which is `0`.
///
/// An *unbounded* run ending because the client went away is not a failure: no length was
/// asked for, so the run got what it asked for.
fn exit_code(ended: Ended, bounded: bool, status: &RecorderStatus) -> i32 {
    if !status.is_healthy() {
        return 1;
    }
    match (ended, bounded) {
        (Ended::ClientGone, true) => 1,
        _ => 0,
    }
}

/// One recording: the recorder, whether to print, and whether the first failure has been
/// announced yet.
struct Session<S: EventSink> {
    recorder: TimelineRecorder<S>,
    print: bool,
    announced: bool,
}

impl<S: EventSink> Session<S> {
    fn new(sink: S, print: bool) -> Session<S> {
        Session {
            recorder: TimelineRecorder::new(sink),
            print,
            announced: false,
        }
    }

    /// Record a tick's events and return the rows to print, which is nothing at all for a
    /// tick that produced no events.
    ///
    /// Every event is offered to the sink even after one has been refused; see the module
    /// docs for why. Records are offered one at a time rather than through
    /// `record_all` because each row needs the [`TimelineEvent`] that was built for it,
    /// and only the last of a batch is retrievable afterwards.
    fn tick(&mut self, events: &[MatchEvent], clock: Option<f32>) -> Vec<String> {
        let mut lines = Vec::new();
        for event in events {
            // Deliberately ignored: the immediate error has nowhere useful to go inside a
            // tick, and `RecorderStatus` carries it to `take_alert` and to the summary.
            let _ = self.recorder.record(event, clock);
            if self.print
                && let Some(record) = self.recorder.last()
            {
                lines.push(line(record));
            }
        }
        lines
    }

    /// The first write failure, once, so an unbounded run that never reaches its summary
    /// still says out loud that the file is incomplete.
    fn take_alert(&mut self) -> Option<String> {
        if self.announced || self.recorder.status().is_healthy() {
            return None;
        }
        self.announced = true;
        Some(format!(
            "the timeline sink refused a record, so the file now has a hole in it; \
             recording continues. first failure: {}",
            opt(self.recorder.status().first_error.as_deref())
        ))
    }

    fn status(&self) -> &RecorderStatus {
        self.recorder.status()
    }

    /// Push the sink's buffer at storage, counting a failed flush as a failure like any
    /// other so it reaches the summary and the exit code.
    fn finish(&mut self) -> Option<String> {
        match self.recorder.flush() {
            Ok(()) => None,
            Err(e) => Some(format!("the timeline sink would not flush: {e}")),
        }
    }
}

/// Poll the client until the run ends.
///
/// Returns why it stopped, what the recorder managed, and a flush failure if there was
/// one. The status is returned by value rather than the session, so that the two sink
/// types - a file and [`NullSink`] - do not have to unify at the call site.
fn record<S: EventSink>(
    reader: &deadlock_reader::Reader,
    opts: &Opts,
    sink: S,
) -> (Ended, RecorderStatus, Option<String>) {
    let mut tracker = EventTracker::new();
    let mut session = Session::new(sink, !opts.quiet);
    if session.print {
        outln!("{}", header());
    }

    let started = Instant::now();
    let mut readable: Option<bool> = None;
    let mut consecutive_errors = 0u32;

    let ended = loop {
        if opts.duration.is_some_and(|d| started.elapsed() >= d) {
            break Ended::Duration;
        }
        match reader.live_snapshot() {
            Ok(Some(snapshot)) => {
                consecutive_errors = 0;
                if readable != Some(true) {
                    readable = Some(true);
                    outln!("match state is readable");
                }
                // The clock comes off the same snapshot the events were diffed from. Do
                // not substitute a later reading, and do not substitute zero.
                let clock = snapshot.clock.playing_seconds();
                for row in session.tick(&tracker.update(&snapshot), clock) {
                    outln!("{row}");
                }
            }
            Ok(None) => {
                consecutive_errors = 0;
                if readable != Some(false) {
                    readable = Some(false);
                    // Not an error and not the end of the run: there is no game-rules
                    // entity to read, and the recording waits for one.
                    outln!("no match state to read; waiting");
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors == 1 {
                    eprintln!("read failed: {e}");
                }
                if consecutive_errors >= CLIENT_GONE_AFTER {
                    break Ended::ClientGone;
                }
            }
        }
        if let Some(alert) = session.take_alert() {
            eprintln!("{alert}");
        }
        std::thread::sleep(opts.interval);
    };
    let flush_error = session.finish();
    (ended, session.status().clone(), flush_error)
}

pub fn timeline(args: &[&str]) -> i32 {
    let opts = match Opts::parse(args) {
        Ok(o) => o,
        Err(why) => {
            eprintln!("{why}");
            return 2;
        }
    };
    let Some(reader) = attach() else { return 1 };

    let (ended, status, flush_error) = match opts.out.as_deref() {
        Some(path) => {
            let sink = match JsonlSink::open(path) {
                Ok(s) => s,
                Err(e) => {
                    // The directory has to exist. Creating it would mean writing a
                    // timeline somewhere the caller did not name.
                    eprintln!("could not open {}: {e}", path.display());
                    return 1;
                }
            };
            outln!("appending JSONL to {}", path.display());
            announce(&opts);
            record(&reader, &opts, sink)
        }
        None => {
            announce(&opts);
            record(&reader, &opts, NullSink)
        }
    };

    if let Some(why) = &flush_error {
        eprintln!("{why}");
    }
    for row in summary(&status, ended, opts.out.as_deref()) {
        outln!("{row}");
    }
    let code = exit_code(ended, opts.duration.is_some(), &status);
    // A flush that would not go through means accepted records may never have landed,
    // which is the same kind of hole as a refused one even though nothing can say how
    // many records it cost.
    if flush_error.is_some() {
        return 1;
    }
    code
}

/// Say how the run will end, before it starts.
fn announce(opts: &Opts) {
    match opts.duration {
        Some(d) => outln!(
            "recording for {:.1}s (ctrl-c stops it sooner, without a summary)",
            d.as_secs_f64()
        ),
        None => outln!("recording until ctrl-c, which ends it without a summary"),
    }
}

#[cfg(test)]
mod tests {
    /// A temp path unique to this process.
    ///
    /// The process id is what keeps two concurrent runs of this suite apart. Without it a
    /// second test binary writes and deletes the same fixed paths mid-run, and the
    /// workspace suite is routinely run more than once at a time here - by a person and an
    /// agent, or by two agents. Measured before this existed: six concurrent runs of the
    /// `dlrs` test binary produced two failures, in `a_second_recording_appends` and in the
    /// `--cap` tests, and every one of those tests passes alone.
    ///
    /// A flaky suite is worse than a slow one: it trains everybody to re-run rather than
    /// read the failure.
    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dlrs-{}-{name}", std::process::id()))
    }

    use super::*;
    use deadlock_events::timeline::EventKind;

    /// A sink that refuses the writes named in `fail_on`, and remembers the rest.
    struct ScriptedSink {
        seen: Vec<TimelineEvent>,
        offered: usize,
        fail_on: Vec<usize>,
    }

    impl ScriptedSink {
        fn never_fails() -> ScriptedSink {
            ScriptedSink {
                seen: Vec::new(),
                offered: 0,
                fail_on: Vec::new(),
            }
        }

        fn failing_on(fail_on: &[usize]) -> ScriptedSink {
            ScriptedSink {
                seen: Vec::new(),
                offered: 0,
                fail_on: fail_on.to_vec(),
            }
        }
    }

    impl EventSink for ScriptedSink {
        fn write(&mut self, event: &TimelineEvent) -> Result<(), SinkError> {
            let n = self.offered;
            self.offered += 1;
            if self.fail_on.contains(&n) {
                return Err(SinkError::Other(format!("refused record {n}")));
            }
            self.seen.push(event.clone());
            Ok(())
        }
    }

    fn kill(slot: u32) -> MatchEvent {
        MatchEvent::Kill {
            slot: Some(slot),
            hero: None,
            total: 1,
            delta: 1,
        }
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
        assert!(!o.quiet);
    }

    #[test]
    fn the_flags_that_change_the_run() {
        let o = Opts::parse(&[
            "--out", "t.jsonl", "--for", "2.5", "--every", "50", "--quiet",
        ])
        .unwrap();
        assert_eq!(o.out, Some(PathBuf::from("t.jsonl")));
        assert_eq!(o.duration, Some(Duration::from_millis(2500)));
        assert_eq!(o.interval, Duration::from_millis(50));
        assert!(o.quiet);
    }

    #[test]
    fn the_arguments_that_are_not_a_recording() {
        for args in [
            // --quiet with no --out records nothing and says nothing.
            &["--quiet"][..],
            &["--out"],
            &["--for"],
            &["--for", "0"],
            &["--for", "-3"],
            &["--for", "soon"],
            &["--every"],
            &["--every", "0"],
            &["--every", "fast"],
            &["--nope"],
            &["timeline.jsonl"],
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
        for name in ["--out", "--for", "--every", "--quiet"] {
            assert!(documented(name), "{name} parses but is not listed");
        }
    }

    #[test]
    fn every_documented_flag_is_accepted() {
        for (spelling, help) in FLAGS {
            assert!(!help.is_empty(), "{spelling} has no description");
            let name = spelling.split_whitespace().next().unwrap();
            let mut args = vec!["--out", "t.jsonl"];
            if name != "--out" {
                args.push(name);
                if spelling.contains('<') {
                    args.push("1");
                }
            }
            assert!(
                Opts::parse(&args).is_ok(),
                "{name} is documented but {args:?} does not parse"
            );
        }
    }

    /// The project rule, on the two fields that are routinely absent. An unreadable match
    /// clock is not second zero of the match, and an event with no player is not slot
    /// zero.
    #[test]
    fn an_absent_value_is_a_dash_and_never_a_zero() {
        let e = TimelineEvent::new(None, EventKind::MidbossKilled, None, None, 0, 0);
        let row = line(&e);
        assert_eq!(
            columns(&row),
            ["-", "+0.0s", "0", "midboss_killed", "-", "-"],
            "{row}"
        );
    }

    #[test]
    fn a_row_shows_both_clocks() {
        let e = TimelineEvent::new(
            Some(34_851_234),
            EventKind::Kill,
            Some(3),
            Some(612.5),
            3100,
            4,
        );
        let row = line(&e);
        assert_eq!(
            columns(&row),
            ["10:12", "+3.1s", "4", "kill", "3", "34851234"]
        );
    }

    /// `clock` is a match clock, so `0.0` is a reading and must not render as the dash an
    /// unreadable clock gets.
    #[test]
    fn second_zero_of_a_match_is_not_an_absent_clock() {
        let e = TimelineEvent::new(None, EventKind::MatchStarted, None, Some(0.0), 0, 0);
        let row = line(&e);
        assert_eq!(columns(&row)[0], "0:00");
    }

    #[test]
    fn the_header_lines_up_with_a_row() {
        let e = TimelineEvent::new(Some(1), EventKind::Kill, Some(12), Some(61.0), 1000, 1);
        let (head, row) = (header(), line(&e));
        assert_eq!(columns(&head).len(), columns(&row).len());
    }

    #[test]
    fn a_tick_with_no_events_prints_nothing_and_records_nothing() {
        let mut s = Session::new(ScriptedSink::never_fails(), true);
        assert!(s.tick(&[], Some(10.0)).is_empty());
        assert!(s.tick(&[], None).is_empty());
        assert_eq!(s.status().recorded, 0);
        assert_eq!(s.status().failed, 0);
        assert_eq!(s.take_alert(), None);
    }

    #[test]
    fn quiet_records_without_printing() {
        let mut s = Session::new(ScriptedSink::never_fails(), false);
        assert!(s.tick(&[kill(1), kill(2)], Some(10.0)).is_empty());
        assert_eq!(s.status().recorded, 2);
    }

    /// The write-failure policy, in one test: a refused record does not stop the run, and
    /// every later event is still offered.
    #[test]
    fn a_refused_record_does_not_end_the_recording() {
        let mut s = Session::new(ScriptedSink::failing_on(&[1]), true);
        assert_eq!(s.tick(&[kill(1), kill(2), kill(3)], Some(1.0)).len(), 3);
        assert_eq!(s.tick(&[kill(4)], Some(2.0)).len(), 1);
        assert_eq!(s.status().failed, 1);
        assert_eq!(s.status().recorded, 3);
        // The three that were not refused reached the sink, including the two after the
        // failure.
        let seen = &s.recorder.sink().seen;
        assert_eq!(seen.len(), 3);
        assert_eq!(
            seen.iter().map(|e| e.slot).collect::<Vec<_>>(),
            [Some(1), Some(3), Some(4)]
        );
    }

    #[test]
    fn a_sink_that_refuses_everything_still_records_the_whole_run() {
        let mut s = Session::new(
            ScriptedSink::failing_on(&(0..100).collect::<Vec<_>>()),
            true,
        );
        for _ in 0..5 {
            assert_eq!(s.tick(&[kill(1), kill(2)], None).len(), 2);
        }
        assert_eq!(s.status().recorded, 0);
        assert_eq!(s.status().failed, 10);
    }

    /// A failure that only ever appeared in a summary would be invisible to an unbounded
    /// run, which ctrl-c ends before the summary. It is announced when it happens, and
    /// only once, so a broken sink does not become the output.
    #[test]
    fn the_first_failure_is_announced_once() {
        let mut s = Session::new(ScriptedSink::failing_on(&[0, 1, 2]), true);
        s.tick(&[kill(1)], None);
        let first = s.take_alert().expect("a refused record must be announced");
        assert!(first.contains("hole"), "{first}");
        assert!(first.contains("refused record 0"), "{first}");
        s.tick(&[kill(2), kill(3)], None);
        assert_eq!(s.take_alert(), None, "the alert must not repeat every tick");
    }

    #[test]
    fn a_healthy_session_never_alerts() {
        let mut s = Session::new(ScriptedSink::never_fails(), true);
        s.tick(&[kill(1)], Some(5.0));
        assert_eq!(s.take_alert(), None);
    }

    /// The recorder latches the match id from `MatchStarted`, and the rows have to show
    /// it: an event carrying no match id of its own still belongs to a match.
    #[test]
    fn rows_carry_the_match_the_recorder_latched() {
        let mut s = Session::new(ScriptedSink::never_fails(), true);
        s.tick(
            &[MatchEvent::MatchStarted {
                match_id: Some(999),
            }],
            Some(0.0),
        );
        let rows = s.tick(&[kill(4)], Some(61.0));
        assert_eq!(columns(&rows[0])[5], "999");
    }

    #[test]
    fn the_summary_says_where_the_records_went() {
        let mut s = Session::new(ScriptedSink::never_fails(), true);
        s.tick(&[kill(1), kill(2)], Some(1.0));

        let to_file = summary(s.status(), Ended::Duration, Some(Path::new("t.jsonl")));
        assert!(
            to_file.iter().any(|l| l.contains("recorded 2")),
            "{to_file:?}"
        );
        assert!(to_file.iter().any(|l| l.contains("t.jsonl")), "{to_file:?}");

        // With no --out nothing was written anywhere, and the summary must not imply it
        // was.
        let to_stdout = summary(s.status(), Ended::Duration, None);
        assert!(
            to_stdout.iter().any(|l| l.contains("printed 2")),
            "{to_stdout:?}"
        );
        assert!(
            !to_stdout.iter().any(|l| l.contains("recorded")),
            "{to_stdout:?}"
        );
    }

    #[test]
    fn the_summary_reports_holes() {
        let mut s = Session::new(ScriptedSink::failing_on(&[0, 1]), true);
        s.tick(&[kill(1), kill(2), kill(3)], None);
        let lines = summary(s.status(), Ended::ClientGone, Some(Path::new("t.jsonl")));
        assert!(lines.iter().any(|l| l.contains("2 refused")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("HOLE")), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("refused record 0")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("the client stopped answering"))
        );
    }

    #[test]
    fn a_hole_in_the_timeline_is_a_failed_run() {
        let refused = RecorderStatus {
            failed: 1,
            ..RecorderStatus::default()
        };
        for bounded in [true, false] {
            for ended in [Ended::Duration, Ended::ClientGone] {
                assert_eq!(exit_code(ended, bounded, &refused), 1);
            }
        }
    }

    #[test]
    fn a_short_recording_is_only_a_failure_when_a_length_was_asked_for() {
        let ok = RecorderStatus::default();
        assert_eq!(exit_code(Ended::Duration, true, &ok), 0);
        assert_eq!(exit_code(Ended::ClientGone, true, &ok), 1);
        // Nothing was promised, so nothing was broken.
        assert_eq!(exit_code(Ended::ClientGone, false, &ok), 0);
    }

    /// The dash rule has a second half on disk: an unreadable clock is JSON `null`, and a
    /// consumer joining on it must not find a zero there.
    #[test]
    fn an_unreadable_clock_is_null_on_disk_and_not_zero() {
        let path = scratch("null-clock.jsonl");
        let _ = std::fs::remove_file(&path);
        {
            let mut s = Session::new(JsonlSink::open(&path).unwrap(), false);
            s.tick(&[MatchEvent::MatchStarted { match_id: Some(7) }], None);
            s.tick(&[kill(2)], None);
            s.tick(&[MatchEvent::MidbossKilled { total: 1 }], Some(120.0));
            assert_eq!(s.finish(), None);
            assert_eq!(s.status().recorded, 3);
        }
        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let rows: Vec<&str> = written.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(rows.len(), 3, "{written}");
        assert!(rows[0].contains(r#""clock":null"#), "{}", rows[0]);
        assert!(!rows[0].contains(r#""clock":0"#), "{}", rows[0]);
        assert!(rows[1].contains(r#""kind":"kill""#), "{}", rows[1]);
        assert!(rows[1].contains(r#""slot":2"#), "{}", rows[1]);
        // A match-level event has no slot, and no slot is null rather than slot zero.
        assert!(rows[2].contains(r#""slot":null"#), "{}", rows[2]);
        assert!(rows[2].contains(r#""clock":120.0"#), "{}", rows[2]);
        // The match id is latched across ticks and written as a string, not a number.
        assert!(rows[2].contains(r#""match_id":"7""#), "{}", rows[2]);
    }

    /// `JsonlSink::open` appends, so a second recording into the same path must not
    /// truncate the first.
    #[test]
    fn a_second_recording_appends() {
        let path = scratch("append.jsonl");
        let _ = std::fs::remove_file(&path);
        for _ in 0..2 {
            let mut s = Session::new(JsonlSink::open(&path).unwrap(), false);
            s.tick(&[kill(1)], None);
            assert_eq!(s.finish(), None);
        }
        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(written.lines().filter(|l| !l.is_empty()).count(), 2);
    }

    /// A path whose directory does not exist is an error the command reports, not a
    /// recording that silently keeps nothing.
    #[test]
    fn an_unopenable_out_path_is_an_error() {
        let path = scratch("dlrs-timeline-no-such-dir").join("t.jsonl");
        assert!(JsonlSink::open(&path).is_err());
        let arg = path.to_string_lossy().into_owned();
        // 1, not 2: exit 2 is a malformed argument, and this is a well-formed one that
        // could not be honoured. On a machine with no game running the same 1 comes from
        // the attach a step earlier, so this pins the code and not which step produced it.
        assert_eq!(timeline(&["--out", arg.as_str(), "--for", "1"]), 1);
    }

    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn a_bounded_recording_against_the_live_client_writes_a_file() {
        let path = scratch("live.jsonl");
        let _ = std::fs::remove_file(&path);
        let arg = path.to_string_lossy().into_owned();
        let rc = timeline(&["--out", arg.as_str(), "--for", "2"]);
        assert_eq!(rc, 0, "a clean two-second recording should exit 0");
        // The file exists even for an idle client in the Hideout: it is opened when the
        // recording starts, not when the first event arrives.
        assert!(path.exists(), "{} was not created", path.display());
        let _ = std::fs::remove_file(&path);
    }
}
