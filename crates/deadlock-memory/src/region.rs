//! Region filtering for scanning a process's writable heap.
//!
//! The filter itself is platform-independent and unit-tested everywhere; the enumeration
//! that feeds it lives in each [`crate::mem::MemoryReader`] backend.

use crate::mem::{MemoryReader, Region};

/// `MEM_COMMIT`.
pub const MEM_COMMIT: u32 = 0x1000;
/// `MEM_PRIVATE`.
pub const MEM_PRIVATE: u32 = 0x2_0000;
/// `MEM_MAPPED`.
pub const MEM_MAPPED: u32 = 0x4_0000;
/// `PAGE_NOACCESS`.
pub const PAGE_NOACCESS: u32 = 0x01;
/// `PAGE_READWRITE`.
pub const PAGE_READWRITE: u32 = 0x04;
/// `PAGE_WRITECOPY`.
pub const PAGE_WRITECOPY: u32 = 0x08;
/// `PAGE_GUARD`.
pub const PAGE_GUARD: u32 = 0x100;

/// Regions larger than this are skipped, .
pub const MAX_REGION: usize = 0x800_0000; // 128 MiB

/// Scanning starts here; below this is the null-pointer guard region.
pub const SCAN_START: u64 = 0x1_0000;

/// Upper bound on the user-mode address space walk, as `addr >> 16`.
pub const SCAN_END_SHIFTED: u64 = 0x7FFF_FFFF;

/// Decide whether a region is worth scanning.
pub fn accepts(state: u32, protect: u32, ty: u32, size: usize) -> bool {
    if state != MEM_COMMIT {
        return false;
    }
    if ty != MEM_PRIVATE && ty != MEM_MAPPED {
        return false;
    }
    // Reject inaccessible and guard pages: touching a guard page would perturb the target.
    if protect & (PAGE_NOACCESS | PAGE_GUARD) != 0 {
        return false;
    }
    // Require writable data, which is where heap-resident heap objects live.
    if protect & (PAGE_READWRITE | PAGE_WRITECOPY) == 0 {
        return false;
    }
    size <= MAX_REGION
}

/// How much of a region is read at a time.
///
/// Bounds each worker's buffer instead of sizing it to the largest region, which matters
/// once the scan is threaded: one buffer per thread at [`MAX_REGION`] would reserve more
/// memory than the scan reads in a second.
pub const SCAN_CHUNK: usize = 16 * 1024 * 1024;

/// Probe granularity when a region has to be picked apart to find its readable parts.
pub const PROBE_PAGE: usize = 4096;

/// Contiguous readable spans within a region, as `(base, len)`, found by probing a page
/// at a time.
///
/// The recovery path, not the normal one. `ReadProcessMemory` refuses an entire range if
/// any page in it is inaccessible, so a region with one unmapped page in the middle reads
/// as zero bytes and its other pages are never searched at all - against a live client
/// that silently hid up to 21 regions at a time. A caller reaches for this only after a
/// straight read of the region has come up short, because nothing cheaper can tell an
/// interior hole from a region that simply reads whole.
pub fn readable_spans(mem: &dyn MemoryReader, r: &Region) -> Vec<(u64, usize)> {
    let mut probe = [0u8; 1];
    let mut spans = Vec::new();
    let mut run_start: Option<u64> = None;
    let mut at = 0usize;
    while at < r.size {
        let addr = r.base + at as u64;
        let readable = mem.read_partial(addr, &mut probe) == 1;
        match (readable, run_start) {
            (true, None) => run_start = Some(addr),
            (false, Some(start)) => {
                spans.push((start, (addr - start) as usize));
                run_start = None;
            }
            _ => {}
        }
        at += PROBE_PAGE;
    }
    if let Some(start) = run_start {
        spans.push((start, (r.base + r.size as u64 - start) as usize));
    }
    spans
}

/// Scan the target's committed RW regions for a byte needle.
///
/// Returns every remote address at which `needle` occurs, in ascending order. Occurrences
/// may overlap. Unreadable pages are skipped, which is expected in a live process.
///
/// The work is split across threads when there is enough of it to be worth the split; a
/// scan of one region stays on the calling thread.
pub fn scan_for(mem: &dyn MemoryReader, regions: &[Region], needle: &[u8]) -> Vec<u64> {
    if needle.is_empty() || regions.is_empty() {
        return Vec::new();
    }

    let threads = std::thread::available_parallelism()
        // Not `NonZero::get`: the generic alias is 1.79, and the manifest says 1.75.
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(regions.len())
        .min(8);

    let mut hits = if threads <= 1 {
        scan_stripe(mem, regions, needle, 0, 1)
    } else {
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|i| scope.spawn(move || scan_stripe(mem, regions, needle, i, threads)))
                .collect();
            workers
                .into_iter()
                .flat_map(|w| {
                    // A panicked worker is a bug in the scan, not a condition to absorb.
                    // Dropping its stripe returned a short hit list that looked complete,
                    // so a sweep would quietly miss the object it was looking for. Better
                    // to fail loudly than to search half the heap and report success.
                    w.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
                })
                .collect()
        })
    };

    // Striping hands regions out round-robin, so the merged hits are not ordered.
    hits.sort_unstable();
    hits
}

/// Scan every `stride`th region starting at `first`.
fn scan_stripe(
    mem: &dyn MemoryReader,
    regions: &[Region],
    needle: &[u8],
    first: usize,
    stride: usize,
) -> Vec<u64> {
    let mut scanner = SpanScanner::new(mem, needle);

    for r in regions.iter().skip(first).step_by(stride) {
        let mark = scanner.hit_count();
        if scanner.scan(r.base, r.size) == Coverage::Complete {
            continue;
        }
        // Something in the region is unmapped, so the reads came up short and whatever
        // they did produce covers only part of it. Drop those hits and go again over the
        // parts that can actually be read, rather than losing the region wholesale.
        scanner.rewind_to(mark);
        for (base, len) in readable_spans(mem, r) {
            scanner.scan(base, len);
        }
    }
    scanner.into_hits()
}

/// Whether a span was read in full.
///
/// Was a bare `bool` whose meaning lived only in a doc comment, at a call site that read
/// `if scan_span(..) { continue }` - which says nothing about what is being continued past.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coverage {
    /// Every chunk read in full, so the hits from this span are all of them.
    Complete,
    /// Part of the span is not mapped, so the hits gathered from it are incomplete and
    /// should be discarded in favour of a pass over the readable parts.
    Partial,
}

/// One worker's scanning state: everything that stays the same across the spans it visits.
///
/// `scan_span` took seven arguments, two of them `&mut` out-params, and five of those were
/// identical on every call a worker made. Holding them here leaves the span itself as the
/// only thing a call has to say.
struct SpanScanner<'a> {
    mem: &'a dyn MemoryReader,
    finder: memchr::memmem::Finder<'a>,
    needle_len: usize,
    /// Sized once, on first use, and reused for every span this worker touches. Clearing
    /// and re-zeroing per region memset the whole heap for nothing: the read overwrites
    /// every byte it reports.
    buf: Vec<u8>,
    hits: Vec<u64>,
}

impl<'a> SpanScanner<'a> {
    fn new(mem: &'a dyn MemoryReader, needle: &'a [u8]) -> Self {
        SpanScanner {
            mem,
            finder: memchr::memmem::Finder::new(needle),
            needle_len: needle.len(),
            buf: Vec::new(),
            hits: Vec::new(),
        }
    }

    /// Hits so far, for marking a point to rewind to.
    fn hit_count(&self) -> usize {
        self.hits.len()
    }

    /// Discard everything found since `mark`.
    fn rewind_to(&mut self, mark: usize) {
        self.hits.truncate(mark);
    }

    fn into_hits(self) -> Vec<u64> {
        self.hits
    }

    /// Search one span, chunk by chunk.
    fn scan(&mut self, base: u64, len: usize) -> Coverage {
        if len < self.needle_len {
            // Too small to hold the needle, so nothing was missed by not reading it.
            return Coverage::Complete;
        }
        let mut whole = true;
        // Chunks carry enough of the next one to hold a needle that straddles the
        // boundary, and each chunk only reports hits starting inside its own share, so a
        // straddling occurrence is found exactly once.
        let overlap = self.needle_len - 1;
        let mut at = 0usize;
        while at < len {
            let want = (len - at).min(SCAN_CHUNK + overlap);
            if self.buf.len() < want {
                self.buf.resize(want, 0);
            }
            let got = self
                .mem
                .read_partial(base + at as u64, &mut self.buf[..want]);
            let last = at + want >= len;
            whole &= got == want;
            if got >= self.needle_len {
                let hay = &self.buf[..got];
                let limit = if last { got } else { got.min(SCAN_CHUNK) };
                let mut from = 0usize;
                while let Some(rel) = self.finder.find(&hay[from..]) {
                    let pos = from + rel;
                    if pos >= limit {
                        break;
                    }
                    self.hits.push(base + (at + pos) as u64);
                    // Occurrences of a self-overlapping needle must not be skipped; the
                    // caller decides which of them is a real field, not this function.
                    from = pos + 1;
                    if from + self.needle_len > hay.len() {
                        break;
                    }
                }
            }
            if last {
                break;
            }
            at += SCAN_CHUNK;
        }
        if whole {
            Coverage::Complete
        } else {
            Coverage::Partial
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_private_committed_rw() {
        assert!(accepts(MEM_COMMIT, PAGE_READWRITE, MEM_PRIVATE, 0x1000));
        assert!(accepts(MEM_COMMIT, PAGE_WRITECOPY, MEM_MAPPED, 0x1000));
    }

    #[test]
    fn rejects_reserved_guard_and_image() {
        assert!(!accepts(0x2000, PAGE_READWRITE, MEM_PRIVATE, 0x1000));
        assert!(!accepts(
            MEM_COMMIT,
            PAGE_READWRITE | PAGE_GUARD,
            MEM_PRIVATE,
            0x1000
        ));
        assert!(!accepts(MEM_COMMIT, PAGE_NOACCESS, MEM_PRIVATE, 0x1000));
        assert!(!accepts(MEM_COMMIT, PAGE_READWRITE, 0x100_0000, 0x1000));
    }

    #[test]
    fn rejects_read_only_and_oversized() {
        const PAGE_READONLY: u32 = 0x02;
        assert!(!accepts(MEM_COMMIT, PAGE_READONLY, MEM_PRIVATE, 0x1000));
        assert!(!accepts(
            MEM_COMMIT,
            PAGE_READWRITE,
            MEM_PRIVATE,
            MAX_REGION + 1
        ));
        assert!(accepts(MEM_COMMIT, PAGE_READWRITE, MEM_PRIVATE, MAX_REGION));
    }

    use crate::mock::MockMemory;

    const BASE: u64 = 0x2000_0000;

    fn mapped(segments: &[(u64, Vec<u8>)], region: (u64, usize)) -> MockMemory {
        let mut m = MockMemory::new(1);
        for (at, bytes) in segments {
            m.write(*at, bytes);
        }
        m.add_region(region.0, region.1);
        m
    }

    #[test]
    fn finds_every_occurrence_including_overlapping_ones() {
        let mut seg = vec![b'.'; 64];
        seg[10..15].copy_from_slice(b"aaaaa");
        let m = mapped(&[(BASE, seg.clone())], (BASE, seg.len()));
        assert_eq!(
            scan_for(&m, &m.regions().unwrap(), b"aaa"),
            vec![BASE + 10, BASE + 11, BASE + 12]
        );
    }

    /// Chunking is an implementation detail of the read, and a needle lying across a chunk
    /// boundary must be reported exactly once: not twice from the overlap, not zero times
    /// from being split.
    #[test]
    fn a_needle_straddling_a_chunk_boundary_is_found_once() {
        let needle = b"NEEDLE";
        let at = SCAN_CHUNK - 3;
        let mut seg = vec![0u8; SCAN_CHUNK + 4096];
        seg[at..at + needle.len()].copy_from_slice(needle);
        let m = mapped(&[(BASE, seg.clone())], (BASE, seg.len()));
        assert_eq!(
            scan_for(&m, &m.regions().unwrap(), needle),
            vec![BASE + at as u64]
        );
    }

    /// One unmapped page in the middle of a region used to hide the entire region, because
    /// the bulk read fails outright rather than partially.
    #[test]
    fn a_hole_does_not_hide_the_rest_of_its_region() {
        let gap = BASE + 3 * PROBE_PAGE as u64;
        let m = mapped(
            &[
                (BASE, vec![b'x'; 2 * PROBE_PAGE]),
                (gap, vec![b'y'; 2 * PROBE_PAGE]),
            ],
            (BASE, 5 * PROBE_PAGE),
        );
        assert_eq!(
            readable_spans(&m, &m.regions().unwrap()[0]),
            vec![(BASE, 2 * PROBE_PAGE), (gap, 2 * PROBE_PAGE)]
        );
    }

    /// The recovery has to actually find things, on both sides of the hole, and report
    /// each of them once.
    #[test]
    fn a_scan_recovers_hits_on_both_sides_of_a_hole() {
        let gap = BASE + 3 * PROBE_PAGE as u64;
        let mut before = vec![b'.'; 2 * PROBE_PAGE];
        before[100..106].copy_from_slice(b"NEEDLE");
        let mut after = vec![b'.'; 2 * PROBE_PAGE];
        after[200..206].copy_from_slice(b"NEEDLE");
        let m = mapped(&[(BASE, before), (gap, after)], (BASE, 5 * PROBE_PAGE));
        assert_eq!(
            scan_for(&m, &m.regions().unwrap(), b"NEEDLE"),
            vec![BASE + 100, gap + 200]
        );
    }

    #[test]
    fn an_empty_needle_or_region_set_finds_nothing() {
        let m = mapped(&[(BASE, vec![b'a'; 32])], (BASE, 32));
        assert!(scan_for(&m, &m.regions().unwrap(), b"").is_empty());
        assert!(scan_for(&m, &[], b"a").is_empty());
    }

    #[test]
    fn constants_match_win32() {
        assert_eq!(
            (MEM_COMMIT, MEM_PRIVATE, MEM_MAPPED),
            (0x1000, 0x20000, 0x40000)
        );
        assert_eq!(PAGE_NOACCESS | PAGE_GUARD, 0x101);
        assert_eq!(PAGE_READWRITE | PAGE_WRITECOPY, 0x0C);
    }
}
