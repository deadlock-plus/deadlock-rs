//! The shape a GUI or server app wants: one shared reader, a background poller, and
//! snapshots delivered to the UI thread.
//!
//! ```text
//! cargo run --release --example poll
//! ```
//!
//! This uses only `std`, but it is deliberately the same structure a Tauri app needs:
//!
//! ```text
//! // main.rs
//! struct AppState { reader: Arc<Reader> }          // Reader is Send + Sync
//!
//! tauri::Builder::default()
//!     .setup(|app| {
//!         let reader = Arc::new(Reader::attach()?);
//!         app.manage(AppState { reader: reader.clone() });
//!         let handle = app.handle().clone();
//!         std::thread::spawn(move || loop {
//!             if let Ok(Some(s)) = reader.live_snapshot() {
//!                 handle.emit("match-update", &s).ok();   // needs feature = "serde"
//!             }
//!             std::thread::sleep(Duration::from_millis(500));
//!         });
//!         Ok(())
//!     })
//!     .invoke_handler(tauri::generate_handler![scoreboard])
//!     .run(tauri::generate_context!())?;
//!
//! #[tauri::command]
//! fn scoreboard(state: tauri::State<AppState>) -> Option<LiveSnapshot> {
//!     state.reader.live_snapshot().ok().flatten()
//! }
//! ```
//!
//! Enable `features = ["serde"]` to serialise `LiveSnapshot` straight to the frontend.

#[cfg(not(windows))]
fn main() {
    eprintln!("poll attaches to a live Windows game process; see README for porting notes.");
}

#[cfg(windows)]
fn main() {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use deadlock_reader::Reader;
    use deadlock_reader::snapshot::LiveSnapshot;

    // Attach once. This is the expensive step (~270ms: 60 MiB image copy + schema walk).
    let reader = match Reader::attach() {
        Ok(r) => Arc::new(r),
        Err(e) => {
            eprintln!("attach failed: {e}");
            eprintln!("is Deadlock running?");
            return;
        }
    };
    println!("{reader:?}\n");

    let (tx, rx) = mpsc::channel::<LiveSnapshot>();

    // Background poller. `Reader` is Sync, so this needs only an Arc - no mutex.
    let poller = {
        let reader = Arc::clone(&reader);
        std::thread::spawn(move || {
            for _ in 0..10 {
                match reader.live_snapshot() {
                    Ok(Some(s)) => {
                        if tx.send(s).is_err() {
                            break; // consumer went away
                        }
                    }
                    Ok(None) => eprintln!("(not in a lobby or match)"),
                    Err(e) => eprintln!("(read failed: {e})"),
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };

    // Consumer: whatever your UI does with each update.
    let mut ticks = 0;
    let start = Instant::now();
    for snap in rx {
        ticks += 1;
        println!(
            "[{:>5.1?}] {:<24} state={:<16} players={} objectives={}",
            start.elapsed(),
            snap.describe(),
            snap.game_state.map(|g| g.name()).unwrap_or("?"),
            snap.players.len(),
            snap.objectives.len(),
        );
        if let Some(me) = snap.local_player() {
            println!(
                "          you: hero {:?} lvl {:?} {}/{}/{} networth {:?}",
                me.hero_id,
                me.level,
                me.kills.unwrap_or(0),
                me.deaths.unwrap_or(0),
                me.assists.unwrap_or(0),
                me.net_worth,
            );
        }
    }
    poller.join().ok();
    println!("\n{ticks} updates delivered");
}
