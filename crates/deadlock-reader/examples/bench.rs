//! Time the operations a polling app performs every tick.
//!
//! ```text
//! cargo run --release --example bench
//! ```

#[cfg(not(windows))]
fn main() {
    eprintln!("bench attaches to a live Windows game process.");
}

#[cfg(windows)]
use deadlock_reader::Reader;
#[cfg(windows)]
use std::time::Instant;

#[cfg(windows)]
fn main() {
    let t0 = Instant::now();
    let r = match Reader::attach() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("attach: {e}");
            return;
        }
    };
    println!("attach (one-off)      {:>8.1?}", t0.elapsed());
    println!(
        "  image {} MiB, schema {} classes",
        r.client_size() / (1024 * 1024),
        r.schema()
            .map(deadlock_reader::schema::SchemaIndex::class_count)
            .unwrap_or(0)
    );

    let n = 10;

    let t = Instant::now();
    let mut count = 0;
    for _ in 0..n {
        count = r.entities().map(|e| e.len()).unwrap_or(0);
    }
    println!(
        "entities() x{n}         {:>8.1?} each  ({count} entities)",
        t.elapsed() / n
    );

    let t = Instant::now();
    let mut players = 0;
    for _ in 0..n {
        players = r
            .live_snapshot()
            .ok()
            .flatten()
            .map(|s| s.players.len())
            .unwrap_or(0);
    }
    println!(
        "live_snapshot() x{n}    {:>8.1?} each  ({players} players)",
        t.elapsed() / n
    );
}
