//! An append-only JSONL implementation of [`EventSink`].
//!
//! One record per line, `\n`-terminated, in the order they were written. That is the whole
//! format. Companion's equivalent is a SQLite table, and only the *schema* is copied here:
//! a database would be the first non-pure-Rust dependency in this workspace, and the file
//! it would produce is less useful than one `grep`, `jq`, `tail -f` and `wc -l` already
//! understand.
//!
//! ```no_run
//! use deadlock_events::sink::{FlushPolicy, JsonlSink};
//! use deadlock_events::timeline::TimelineRecorder;
//!
//! let sink = JsonlSink::open("timeline.jsonl")?.with_policy(FlushPolicy::EveryRecord);
//! let mut recorder = TimelineRecorder::new(sink);
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # What "append-only" actually buys, and what it does not
//!
//! The file is opened with the append flag set, so every write goes to the current end of
//! file and the sink never seeks. A second process, or a second run, adds to the file
//! rather than replacing it, and no write can reach a byte an earlier record already
//! occupies. That is the guarantee: **records already on disk are never rewritten.**
//!
//! What is *not* claimed:
//!
//! - **Not atomic per record.** A record is submitted in a single `write_all` of one
//!   contiguous buffer, and on the platforms this runs on a small append normally lands
//!   whole - but nothing here enforces it. A write cut short leaves a truncated line.
//! - **Not durable against power loss.** [`JsonlSink::flush`] hands bytes to the operating
//!   system; it does not `fsync`. A process that dies loses nothing already flushed. A
//!   machine that loses power may lose whatever the OS had not written out.
//! - **Not safe for two sinks on one file.** Two processes appending to the same path may
//!   interleave lines. Give each recording its own file.
//!
//! # What a torn write leaves behind
//!
//! Damage is bounded to the record that failed. After a failed write the sink knows the
//! file may end mid-line, and starts the next record with a newline so the fragment is
//! terminated rather than glued to the record after it. A reader then sees one unparseable
//! line, with every record before and after it intact.
//!
//! That costs one empty line when the failed write had in fact managed nothing at all,
//! which is the right trade: an empty line is skipped by every line-oriented reader,
//! whereas two records fused into one is a record lost.
//!
//! **A reader must therefore tolerate both a trailing partial line and an occasional
//! unparseable one.** Parse line by line and skip what does not parse; do not treat the
//! file as a JSON array.
//!
//! # Flushing
//!
//! [`FlushPolicy::EveryRecord`] is the default. It costs one `write` syscall per record,
//! which at the rate this crate produces events - a poll every 50 to 100 ms, and most polls
//! produce nothing - is not a cost worth trading durability for. `flush` here is a write to
//! the OS, not an `fsync`, so it is cheaper than the name suggests.
//!
//! [`FlushPolicy::every`] batches, bounding what a crash can lose to `n` records, and
//! [`FlushPolicy::Manual`] hands the decision to the caller entirely. Dropping the sink
//! flushes what is buffered, best-effort: `Drop` cannot report a failure, so it is a
//! backstop and not the mechanism to rely on.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::num::NonZeroU64;
use std::path::Path;

use crate::timeline::{EventSink, SinkError, TimelineEvent};

/// When a [`JsonlSink`] pushes its buffer at the writer.
///
/// The two extremes are both wrong in the general case - a syscall per record is wasteful
/// at high rates, and never flushing loses everything a crash interrupts - so the policy is
/// the caller's to set. The default is stated on [`JsonlSink::open`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FlushPolicy {
    /// Write through on every record. Nothing is ever waiting in this crate's buffer.
    EveryRecord,
    /// Write through once `n` records have accumulated. A crash loses at most `n - 1`.
    ///
    /// Build with [`FlushPolicy::every`]; a batch of zero is not a policy.
    EveryN(NonZeroU64),
    /// Write through only when asked. A crash loses everything buffered.
    Manual,
}

impl FlushPolicy {
    /// Batch `n` records per write, or [`None`] if `n` is zero.
    ///
    /// Zero is rejected rather than silently treated as one, because a policy that never
    /// becomes due is a sink that never writes - a stall that would look exactly like a
    /// quiet match.
    pub fn every(n: u64) -> Option<FlushPolicy> {
        NonZeroU64::new(n).map(FlushPolicy::EveryN)
    }

    /// Whether `pending` buffered records are enough to write through.
    fn due(&self, pending: u64) -> bool {
        match self {
            FlushPolicy::EveryRecord => true,
            FlushPolicy::EveryN(n) => pending >= n.get(),
            FlushPolicy::Manual => false,
        }
    }
}

/// An append-only JSONL [`EventSink`].
///
/// Generic over the writer so it is not only a file: a test writes into a `Vec<u8>`, and a
/// caller can pipe a timeline anywhere that implements [`Write`]. [`JsonlSink::open`] is
/// the file case, which is the one item 7.2 asks for.
///
/// Read the [module docs](self) for the durability guarantee before relying on one.
pub struct JsonlSink<W: Write> {
    out: W,
    buf: Vec<u8>,
    pending: u64,
    policy: FlushPolicy,
    /// Set after a failed write, when the file may end part way through a record.
    torn: bool,
    written: u64,
    dropped: u64,
}

impl JsonlSink<File> {
    /// Open `path` for appending, creating it if it is not there.
    ///
    /// The file is *not* truncated: an existing timeline is added to. The parent directory
    /// is not created - a missing one is an error, because silently writing a timeline
    /// somewhere the caller did not mean is worse than failing.
    ///
    /// Starts on [`FlushPolicy::EveryRecord`]; use [`JsonlSink::with_policy`] to change it.
    ///
    /// # Errors
    ///
    /// Whatever [`OpenOptions::open`] returns: a missing directory, a permission problem, a
    /// path that is not a file.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<JsonlSink<File>> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_ref())?;
        Ok(JsonlSink::new(file))
    }
}

impl<W: Write> JsonlSink<W> {
    /// Wrap an arbitrary writer.
    ///
    /// Nothing here seeks, so the writer only has to be able to take bytes in order. It is
    /// the caller's job to make sure it is positioned at the end of whatever it is
    /// appending to - [`JsonlSink::open`] does that with the append flag.
    pub fn new(out: W) -> JsonlSink<W> {
        JsonlSink {
            out,
            buf: Vec::new(),
            pending: 0,
            policy: FlushPolicy::EveryRecord,
            torn: false,
            written: 0,
            dropped: 0,
        }
    }

    /// Set the flush policy. See [`FlushPolicy`].
    pub fn with_policy(mut self, policy: FlushPolicy) -> JsonlSink<W> {
        self.policy = policy;
        self
    }

    /// The flush policy in force.
    pub fn policy(&self) -> FlushPolicy {
        self.policy
    }

    /// Records buffered here and not yet handed to the writer.
    ///
    /// Always `0` under [`FlushPolicy::EveryRecord`]. Under the others this is what a crash
    /// would lose.
    pub fn buffered(&self) -> u64 {
        self.pending
    }

    /// Records handed to the writer without error.
    ///
    /// Not the same as records accepted: under a batching policy a record is accepted, sits
    /// in the buffer, and only counts here once the batch goes out.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Records that will never reach the writer, because a write failed or a record could
    /// not be encoded.
    ///
    /// Every one is a hole in the timeline. A caller that ignores the `Result` from
    /// [`EventSink::write`] can watch this instead.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Hand the buffer to the writer and clear it.
    ///
    /// On failure the buffer is dropped rather than retained for a retry: a failed
    /// `write_all` may have written some of it, and re-sending would duplicate whatever did
    /// land. The records are counted in [`JsonlSink::dropped`] and the sink remembers that
    /// the file may end mid-line.
    fn flush_buffer(&mut self) -> Result<(), SinkError> {
        if self.buf.is_empty() {
            return self.out.flush().map_err(SinkError::from);
        }
        let records = self.pending;
        let outcome = self.out.write_all(&self.buf);
        self.buf.clear();
        self.pending = 0;
        match outcome {
            Ok(()) => self.written += records,
            Err(error) => {
                self.dropped += records;
                self.torn = true;
                return Err(SinkError::from(error));
            }
        }
        self.out.flush().map_err(SinkError::from)
    }
}

impl<W: Write> EventSink for JsonlSink<W> {
    /// Encode one record and, depending on the [`FlushPolicy`], write it through.
    ///
    /// An encoding failure leaves nothing behind: the buffer is rewound to where the record
    /// started, so a half-encoded record cannot reach the file.
    fn write(&mut self, event: &TimelineEvent) -> Result<(), SinkError> {
        if self.torn {
            // The file may end part way through the previous record. Terminate it so the
            // damage stays on its own line.
            self.buf.push(b'\n');
            self.torn = false;
        }

        let mark = self.buf.len();
        if let Err(error) = serde_json::to_writer(&mut self.buf, event) {
            self.buf.truncate(mark);
            self.dropped += 1;
            return Err(SinkError::Encode(error.to_string()));
        }
        self.buf.push(b'\n');
        self.pending += 1;

        if self.policy.due(self.pending) {
            self.flush_buffer()
        } else {
            Ok(())
        }
    }

    /// Write anything buffered through to the operating system.
    ///
    /// This is not an `fsync`; see the [module docs](self).
    fn flush(&mut self) -> Result<(), SinkError> {
        self.flush_buffer()
    }
}

/// Best-effort flush, because a buffered tail lost to a forgotten `flush` is the easy way
/// to lose the end of a match.
///
/// `Drop` cannot report a failure, so a caller that needs to know the timeline is complete
/// calls [`EventSink::flush`] and checks the result. This only stops the common mistake
/// from being silent data loss.
impl<W: Write> Drop for JsonlSink<W> {
    fn drop(&mut self) {
        let _ = self.flush_buffer();
    }
}

impl<W: Write> std::fmt::Debug for JsonlSink<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlSink")
            .field("policy", &self.policy)
            .field("buffered", &self.pending)
            .field("written", &self.written)
            .field("dropped", &self.dropped)
            .field("torn", &self.torn)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::timeline::{EventKind, TimelineEvent};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A directory that removes itself. The workspace has no `tempfile` dependency and this
    /// is not a good enough reason to add one.
    ///
    /// On Windows a directory with an open handle in it cannot be removed, so every test
    /// here drops its sink before this runs. The removal is best-effort regardless: a test
    /// that leaves a file behind should not fail for that reason alone.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "deadlock-events-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Writes at most `allow` more bytes and then fails, so a torn write is reachable
    /// without a full disk. The budget is shared with the test so it can be topped up.
    #[derive(Clone)]
    struct TornWriter {
        out: Rc<RefCell<Vec<u8>>>,
        allow: Rc<Cell<usize>>,
    }

    impl TornWriter {
        fn new(allow: usize) -> TornWriter {
            TornWriter {
                out: Rc::new(RefCell::new(Vec::new())),
                allow: Rc::new(Cell::new(allow)),
            }
        }

        fn text(&self) -> String {
            String::from_utf8(self.out.borrow().clone()).unwrap()
        }
    }

    impl std::io::Write for TornWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.allow.get() == 0 {
                return Err(std::io::Error::other("no room left"));
            }
            let n = buf.len().min(self.allow.get());
            self.out.borrow_mut().extend_from_slice(&buf[..n]);
            self.allow.set(self.allow.get() - n);
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn record(seq: u64) -> TimelineEvent {
        TimelineEvent::new(
            Some(18_446_744_073_709_551_615),
            EventKind::Kill,
            Some(3),
            Some(60.0 + seq as f32),
            1_000 * seq,
            seq,
        )
    }

    fn lines(path: &PathBuf) -> Vec<String> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The whole point of "append-only". A second sink over the same path must not remove
    /// what the first one wrote, which is exactly what a plain `File::create` would do.
    #[test]
    fn a_second_sink_appends_and_does_not_truncate() {
        let dir = TempDir::new("append");
        let path = dir.file("timeline.jsonl");

        let mut first = JsonlSink::open(&path).unwrap();
        first.write(&record(0)).unwrap();
        drop(first);

        let mut second = JsonlSink::open(&path).unwrap();
        second.write(&record(1)).unwrap();
        drop(second);

        let lines = lines(&path);
        assert_eq!(lines.len(), 2, "the reopen truncated: {lines:?}");
        let seqs: Vec<u64> = lines
            .iter()
            .map(|l| serde_json::from_str::<TimelineEvent>(l).unwrap().seq)
            .collect();
        assert_eq!(seqs, vec![0, 1]);
    }

    /// JSONL is one record per line by definition. A record containing a newline, or two
    /// records sharing a line, makes the file unreadable line by line.
    #[test]
    fn one_record_is_exactly_one_line() {
        let dir = TempDir::new("lines");
        let path = dir.file("timeline.jsonl");

        let mut sink = JsonlSink::open(&path).unwrap();
        for seq in 0..5 {
            sink.write(&record(seq)).unwrap();
        }
        drop(sink);

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'), "the last record has no terminator");
        assert_eq!(text.lines().count(), 5);
        for line in text.lines() {
            serde_json::from_str::<TimelineEvent>(line).unwrap();
        }
    }

    /// File order is the primary order of a JSONL timeline; the sequence number only has to
    /// agree with it.
    #[test]
    fn records_read_back_in_the_order_they_were_written() {
        let dir = TempDir::new("order");
        let path = dir.file("timeline.jsonl");

        let mut sink = JsonlSink::open(&path).unwrap();
        for seq in 0..20 {
            sink.write(&record(seq)).unwrap();
        }
        drop(sink);

        let seqs: Vec<u64> = lines(&path)
            .iter()
            .map(|l| serde_json::from_str::<TimelineEvent>(l).unwrap().seq)
            .collect();
        assert_eq!(seqs, (0..20).collect::<Vec<_>>());
    }

    /// The default policy is the one a crash tests: a process that dies after a write must
    /// leave that write on disk, which means it cannot still be sitting in our buffer.
    #[test]
    fn the_default_policy_puts_every_record_on_disk_before_returning() {
        let dir = TempDir::new("flush-default");
        let path = dir.file("timeline.jsonl");

        let mut sink = JsonlSink::open(&path).unwrap();
        assert_eq!(sink.policy(), FlushPolicy::EveryRecord);
        sink.write(&record(0)).unwrap();

        assert_eq!(lines(&path).len(), 1);
    }

    /// The caller can trade durability for syscalls, which is the only reason the policy is
    /// configurable at all. `Manual` is the far end: nothing reaches the file until asked.
    #[test]
    fn a_manual_policy_holds_records_until_it_is_told_to_flush() {
        let dir = TempDir::new("flush-manual");
        let path = dir.file("timeline.jsonl");

        let mut sink = JsonlSink::open(&path)
            .unwrap()
            .with_policy(FlushPolicy::Manual);
        sink.write(&record(0)).unwrap();
        sink.write(&record(1)).unwrap();
        assert!(lines(&path).is_empty(), "Manual wrote through anyway");
        assert_eq!(sink.buffered(), 2);

        sink.flush().unwrap();
        assert_eq!(lines(&path).len(), 2);
        assert_eq!(sink.buffered(), 0);
    }

    /// The middle setting: a bounded number of records at risk, rather than one or all.
    #[test]
    fn an_every_n_policy_writes_in_batches_of_n() {
        let dir = TempDir::new("flush-n");
        let path = dir.file("timeline.jsonl");

        let mut sink = JsonlSink::open(&path)
            .unwrap()
            .with_policy(FlushPolicy::every(3).unwrap());

        sink.write(&record(0)).unwrap();
        sink.write(&record(1)).unwrap();
        assert!(lines(&path).is_empty(), "flushed before the batch was full");
        sink.write(&record(2)).unwrap();
        assert_eq!(lines(&path).len(), 3);
        drop(sink);
    }

    /// A zero-record batch is not a policy, it is a stall. Rejected at construction rather
    /// than silently treated as one.
    #[test]
    fn a_batch_of_zero_is_not_a_policy() {
        assert!(FlushPolicy::every(0).is_none());
        assert!(FlushPolicy::every(1).is_some());
    }

    /// Forgetting to flush is the normal way to lose a buffered tail, so the drop covers it.
    /// It cannot report a failure, which is why it is not the mechanism a caller relies on.
    #[test]
    fn dropping_the_sink_flushes_what_is_still_buffered() {
        let dir = TempDir::new("drop-flush");
        let path = dir.file("timeline.jsonl");

        let mut sink = JsonlSink::open(&path)
            .unwrap()
            .with_policy(FlushPolicy::Manual);
        sink.write(&record(0)).unwrap();
        assert!(lines(&path).is_empty());
        drop(sink);

        assert_eq!(
            lines(&path).len(),
            1,
            "the buffered record was lost on drop"
        );
    }

    /// A sink that cannot write says so rather than pretending, and keeps a count for a
    /// caller that ignored the `Result`.
    #[test]
    fn a_failed_write_is_returned_and_counted() {
        let writer = TornWriter::new(0);
        let mut sink = JsonlSink::new(writer.clone());

        assert!(sink.write(&record(0)).is_err());
        assert_eq!(sink.written(), 0);
        assert_eq!(sink.dropped(), 1);
        assert!(writer.text().is_empty());
    }

    /// The durability claim, stated as a test. A write cut off part way leaves one damaged
    /// line and the next record starts cleanly after it, so the damage is bounded to the
    /// record that failed - earlier records and later records both still parse.
    #[test]
    fn a_torn_write_damages_one_line_and_no_others() {
        let writer = TornWriter::new(0);
        let mut sink = JsonlSink::new(writer.clone());

        writer.allow.set(4096);
        sink.write(&record(0)).unwrap();

        writer.allow.set(20);
        assert!(sink.write(&record(1)).is_err());

        writer.allow.set(4096);
        sink.write(&record(2)).unwrap();
        sink.flush().unwrap();

        let text = writer.text();
        let good: Vec<TimelineEvent> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<TimelineEvent>(l).ok())
            .collect();
        assert_eq!(
            good.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 2],
            "the torn write took a neighbour with it: {text:?}"
        );

        let bad = text
            .lines()
            .filter(|l| !l.is_empty() && serde_json::from_str::<TimelineEvent>(l).is_err())
            .count();
        assert_eq!(bad, 1, "damage was not bounded to one line: {text:?}");
    }

    /// A crash between the write and the flush leaves whatever the OS already had. Reading
    /// that file back must not throw away the records that did land, so the trailing partial
    /// line has to be the only casualty. This simulates the crash by truncating the file.
    #[test]
    fn a_file_truncated_mid_record_still_yields_every_complete_record() {
        let dir = TempDir::new("torn-tail");
        let path = dir.file("timeline.jsonl");

        let mut sink = JsonlSink::open(&path).unwrap();
        for seq in 0..4 {
            sink.write(&record(seq)).unwrap();
        }
        drop(sink);

        let full = fs::read(&path).unwrap();
        let cut = full.len() - 30;
        fs::write(&path, &full[..cut]).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let good: Vec<u64> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<TimelineEvent>(l).ok())
            .map(|e| e.seq)
            .collect();
        assert_eq!(
            good,
            vec![0, 1, 2],
            "a complete record was lost with the tail"
        );
    }

    /// `written` is what reached the writer, not what was handed to the sink. A caller
    /// comparing it against its own count is how a silent shortfall becomes visible.
    #[test]
    fn the_sink_counts_what_actually_reached_the_writer() {
        let dir = TempDir::new("counts");
        let path = dir.file("timeline.jsonl");

        let mut sink = JsonlSink::open(&path)
            .unwrap()
            .with_policy(FlushPolicy::Manual);
        sink.write(&record(0)).unwrap();
        sink.write(&record(1)).unwrap();
        assert_eq!(sink.written(), 0, "buffered is not written");
        sink.flush().unwrap();
        assert_eq!(sink.written(), 2);
        assert_eq!(sink.dropped(), 0);
        drop(sink);
    }

    /// Held behind the trait, which is the shape item 7.3 is for.
    #[test]
    fn a_jsonl_sink_is_usable_as_a_boxed_event_sink() {
        let dir = TempDir::new("boxed");
        let path = dir.file("timeline.jsonl");

        {
            let mut sink: Box<dyn EventSink> = Box::new(JsonlSink::open(&path).unwrap());
            sink.write(&record(0)).unwrap();
            sink.flush().unwrap();
        }

        assert_eq!(lines(&path).len(), 1);
    }

    /// The sink writes into a file that already exists and does not care what is in it, but
    /// it will not conjure a directory that is not there.
    #[test]
    fn opening_under_a_missing_directory_fails_rather_than_creating_it() {
        let dir = TempDir::new("missing");
        let path = dir.file("nope").join("timeline.jsonl");
        assert!(JsonlSink::open(&path).is_err());
    }
}
