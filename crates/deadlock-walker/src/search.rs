//! Heap search: objects whose first qword is a given vtable.

use std::ops::Range;

use deadlock_memory::mem::{MemoryReader, Module, Region};
use memchr::memmem;

use crate::error::Result;

/// Knobs for [`find_instances`].
#[derive(Clone, Debug)]
pub struct SearchConfig {
    /// Bytes read per request. Rounded down to a multiple of 8.
    pub chunk: usize,
    /// Address ranges to skip, normally the loaded modules. Statically allocated default
    /// instances live there and carry the same vtable as real objects.
    pub exclude: Vec<Range<u64>>,
    /// Stop after this many hits.
    pub limit: usize,
    /// Most threads a search may use. A sweep reads gigabytes, so every extra thread spends
    /// that much more of the machine at once while the game is running.
    pub threads: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            // Reading a live process is copy-bound; a buffer that stays in cache between the
            // copy and the scan measured about 15% faster than 4 MiB.
            chunk: 1024 * 1024,
            exclude: Vec::new(),
            limit: 4096,
            threads: 2,
        }
    }
}

impl SearchConfig {
    /// Skip the whole address range of `module`.
    pub fn excluding_module(mut self, module: &Module) -> Self {
        self.exclude
            .push(module.base..module.base + module.size as u64);
        self
    }
}

/// Addresses of every 8-aligned qword in the target's readable regions equal to `vtable`.
///
/// Unreadable pages are skipped, not fatal: the regions list is a snapshot and pages go
/// away between listing and reading.
pub fn find_instances(
    mem: &dyn MemoryReader,
    vtable: u64,
    config: &SearchConfig,
) -> Result<Vec<u64>> {
    let mut sweep = find_many(mem, &[vtable], config)?;
    Ok(sweep.hits.pop().unwrap_or_default())
}

/// What [`find_many`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    /// Regions searched.
    pub regions: usize,
    /// Hits per vtable, in the order the vtables were given, each in address order.
    pub hits: Vec<Vec<u64>>,
}

/// Like [`find_instances`] for several vtables at once, reading each region only once.
///
/// `config.limit` caps the hits of each vtable separately.
pub fn find_many(mem: &dyn MemoryReader, vtables: &[u64], config: &SearchConfig) -> Result<Sweep> {
    let regions = mem.regions()?;
    let hits = find_in(mem, &regions, vtables, config);
    Ok(Sweep {
        regions: regions.len(),
        hits,
    })
}

/// [`find_many`] restricted to `regions`.
///
/// A large region set is split across threads; the reads dominate and the reader is
/// `Sync`.
pub fn find_in(
    mem: &dyn MemoryReader,
    regions: &[Region],
    vtables: &[u64],
    config: &SearchConfig,
) -> Vec<Vec<u64>> {
    let needles: Vec<[u8; 8]> = vtables.iter().map(|v| v.to_le_bytes()).collect();
    let available = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let threads = worker_count(available, regions.len(), config.threads);

    let mut hits: Vec<Vec<u64>> = vec![Vec::new(); needles.len()];
    if threads <= 1 {
        search_stripe(mem, regions, &needles, config, 0, 1, &mut hits);
    } else {
        let parts: Vec<Vec<Vec<u64>>> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|first| {
                    let needles = &needles;
                    scope.spawn(move || {
                        let mut part = vec![Vec::new(); needles.len()];
                        search_stripe(mem, regions, needles, config, first, threads, &mut part);
                        part
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
                .collect()
        });
        for part in parts {
            for (all, mine) in hits.iter_mut().zip(part) {
                all.extend(mine);
            }
        }
    }
    for h in &mut hits {
        h.sort_unstable();
        h.dedup();
        h.truncate(config.limit);
    }
    hits
}

const MAX_THREADS: usize = 8;

fn worker_count(available: usize, regions: usize, wanted: usize) -> usize {
    available.min(regions).min(wanted.clamp(1, MAX_THREADS))
}

const PAGE: usize = 4096;

fn search_stripe(
    mem: &dyn MemoryReader,
    regions: &[Region],
    needles: &[[u8; 8]],
    config: &SearchConfig,
    first: usize,
    stride: usize,
    hits: &mut [Vec<u64>],
) {
    let chunk = (config.chunk & !7).max(8);
    let mut buf = vec![0u8; chunk];
    for region in regions.iter().skip(first).step_by(stride) {
        if hits.iter().all(|h| h.len() >= config.limit) {
            return;
        }
        let start = region.base.next_multiple_of(8);
        let end = region.base.saturating_add(region.size as u64);
        let mut cursor = start;
        while cursor < end {
            let want = chunk.min((end - cursor) as usize);
            let got = mem.read_partial(cursor, &mut buf[..want]);
            if got == want {
                scan(&buf[..got], cursor, needles, config, hits);
            } else {
                // Some page in the chunk is gone. Rescan it page by page so one bad page
                // does not hide the readable ones around it.
                let mut off = 0;
                while off < want {
                    let step = PAGE.min(want - off);
                    let n = mem.read_partial(cursor + off as u64, &mut buf[off..off + step]);
                    scan(
                        &buf[off..off + n],
                        cursor + off as u64,
                        needles,
                        config,
                        hits,
                    );
                    off += step;
                }
            }
            cursor += want as u64;
        }
    }
}

fn scan(buf: &[u8], base: u64, needles: &[[u8; 8]], config: &SearchConfig, hits: &mut [Vec<u64>]) {
    for (needle, found) in needles.iter().zip(hits) {
        for at in memmem::find_iter(buf, needle) {
            let addr = base + at as u64;
            if addr & 7 == 0 && !config.exclude.iter().any(|r| r.contains(&addr)) {
                found.push(addr);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configured_cap_bounds_the_workers() {
        assert_eq!(worker_count(16, 10_000, 2), 2);
        assert_eq!(worker_count(16, 10_000, 100), MAX_THREADS);
    }

    #[test]
    fn never_more_workers_than_cores_or_regions_and_at_least_one() {
        assert_eq!(worker_count(1, 10_000, 4), 1);
        assert_eq!(worker_count(16, 3, 8), 3);
        assert_eq!(worker_count(16, 10_000, 0), 1);
    }

    #[test]
    fn a_default_search_is_gentle() {
        assert_eq!(SearchConfig::default().threads, 2);
    }
}
