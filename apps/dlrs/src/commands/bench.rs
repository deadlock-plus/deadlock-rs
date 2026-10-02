//! `dlrs bench`: time the read path so a poll interval can be chosen from data.

use super::attach;

/// Reports the cost of a full snapshot and of the cheapest useful read, plus how often
/// the values underneath actually change.
pub fn bench() -> i32 {
    use std::time::Instant;

    const N: u32 = 200;

    let Some(r) = attach() else { return 1 };

    // A debug build is several times slower and skews which half looks expensive.
    if cfg!(debug_assertions) {
        outln!("WARNING: debug build. Numbers below are not representative -");
        outln!("         rebuild with --release before drawing conclusions.");
        outln!("");
    }

    // Warm the name cache first; the first walk pays for every class name and is not
    // representative of steady state.
    let _ = r.live_snapshot();

    let mut snap_us = Vec::with_capacity(N as usize);
    for _ in 0..N {
        let t = Instant::now();
        let _ = r.live_snapshot();
        snap_us.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let mut ent_us = Vec::with_capacity(N as usize);
    for _ in 0..N {
        let t = Instant::now();
        let _ = r.entities();
        ent_us.push(t.elapsed().as_secs_f64() * 1e6);
    }

    let stat = |label: &str, mut v: Vec<f64>| {
        // `total_cmp` rather than `partial_cmp().unwrap()`: the durations here cannot be
        // NaN, but a benchmark that panics instead of printing its numbers is a poor trade
        // for a comparison that has a total order available.
        v.sort_by(f64::total_cmp);
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        outln!(
            "{label:<18} mean {:>8.0} us   p50 {:>8.0}   p99 {:>8.0}   max {:>8.0}",
            mean,
            v[v.len() / 2],
            v[v.len() * 99 / 100],
            v[v.len() - 1]
        );
    };
    // Same work, but reusing pinned entities between full walks.
    let mut cache = deadlock_reader::cache::SnapshotCache::new();
    let _ = r.live_snapshot_cached(&mut cache);
    let mut cached_us = Vec::with_capacity(N as usize);
    for _ in 0..N {
        let t = Instant::now();
        let _ = r.live_snapshot_cached(&mut cache);
        cached_us.push(t.elapsed().as_secs_f64() * 1e6);
    }

    stat("live_snapshot", snap_us.clone());
    stat("entities only", ent_us);
    stat("cached snapshot", cached_us.clone());
    let (walks, reuses) = cache.stats();
    outln!("  cache: {walks} full walks, {reuses} reuses");

    // The cached path must agree with the uncached one on every field it reports.
    // Checked against the live game rather than a mock, because the whole risk is
    // pinned addresses drifting in a real process.
    match (r.live_snapshot(), r.live_snapshot_cached(&mut cache)) {
        (Ok(Some(a)), Ok(Some(b))) => {
            let mut diffs = Vec::new();
            if a.players.len() != b.players.len() {
                diffs.push(format!(
                    "players {} vs {}",
                    a.players.len(),
                    b.players.len()
                ));
            }
            for (x, y) in a.players.iter().zip(b.players.iter()) {
                if x.controller != y.controller {
                    diffs.push(format!(
                        "controller {:#x} vs {:#x}",
                        x.controller, y.controller
                    ));
                }
                if x.kills != y.kills || x.deaths != y.deaths || x.net_worth != y.net_worth {
                    diffs.push(format!("slot {:?} counters differ", x.slot));
                }
                if x.name != y.name {
                    diffs.push(format!("slot {:?} name differs", x.slot));
                }
            }
            if a.objectives.len() != b.objectives.len() {
                diffs.push(format!(
                    "objectives {} vs {}",
                    a.objectives.len(),
                    b.objectives.len()
                ));
            }
            if a.match_id != b.match_id {
                diffs.push("match_id".into());
            }
            outln!("");
            if diffs.is_empty() {
                outln!("cached vs uncached: identical across players, objectives, match id");
            } else {
                outln!("cached vs uncached DIFFERS: {}", diffs.join("; "));
            }
        }
        _ => outln!("cached vs uncached: not in a match, skipped"),
    }

    // How often does the underlying data actually move? Sampling faster than this just
    // re-reads identical bytes.
    let mut changes = 0u32;
    let mut last = None;
    let start = Instant::now();
    let mut samples = 0u32;
    while start.elapsed().as_millis() < 2000 {
        if let Ok(Some(s)) = r.live_snapshot() {
            samples += 1;
            if last != s.clock.now {
                if last.is_some() {
                    changes += 1;
                }
                last = s.clock.now;
            }
        }
    }
    let secs = start.elapsed().as_secs_f64();
    outln!("");
    outln!("simulation time advanced {changes}x in {secs:.1}s over {samples} reads");
    outln!(
        "  -> underlying update rate ~{:.0} Hz",
        changes as f64 / secs
    );
    let mean = snap_us.iter().sum::<f64>() / snap_us.len() as f64;
    let cmean = cached_us.iter().sum::<f64>() / cached_us.len() as f64;
    outln!("  -> uncached: {:.1}% of a core at 26 ms", mean / 260.0);
    outln!("  -> cached:   {:.1}% of a core at 26 ms", cmean / 260.0);
    0
}
