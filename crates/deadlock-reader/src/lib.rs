//! A read-only external reader for Deadlock's live match state.
//!
//! The memory and schema layout this relies on is written up in `LAYOUT.md`, beside this
//! crate's README. The process access underneath it is [`deadlock_memory`].
//!
//! # What it does
//!
//! 1. Attaches to `deadlock.exe` with `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ`.
//! 2. Copies `client.dll`'s image and AOB-scans it for three globals.
//! 3. Walks the Source 2 schema system to recover field offsets **at runtime**, so the
//!    reader survives game patches.
//! 4. Walks the entity system into a per-tick snapshot.
//! 5. Reads match, player and objective state by *name*, not by hardcoded offset.
//!
//! # What it does not do
//!
//! There is no write path anywhere in this crate. No `WriteProcessMemory`, no injection,
//! no hooking, no remote thread creation, no calls into the target. It only observes.
//!
//! # Quick start
//!
//! ```no_run
//! use deadlock_reader::Reader;
//!
//! let reader = Reader::attach()?;
//! println!("pid {}, client.dll @ {:#x}", reader.pid(), reader.client_base());
//!
//! let entities = reader.entities()?;
//! println!("{} live entities", entities.len());
//!
//! if let Some(live) = reader.live_snapshot()? {
//!     println!("match {:?}, {} players", live.match_id, live.players.len());
//! }
//! # Ok::<(), deadlock_reader::Error>(())
//! ```
//!
//! # Confidence
//!
//! Constants come from two independent sources and the docs say which is which.
//! Signatures, access masks, image chunking, the handle mask and the whole field table
//! were recovered byte-for-byte by static extraction, and `LAYOUT.md` records them. The
//! schema-system and entity-identity struct layouts ([`schema::SchemaLayout::DEADLOCK`],
//! [`entity::EntityLayout::DEADLOCK`]) were measured against a live client, because the
//! original's schema walk operates on already-copied buffers and its internal offsets
//! are not statically recoverable.
//!
//! This has been run against a running game: all three signatures resolved, the schema
//! walk recovered 3605 classes across 20 scopes, and the entity walk returned correct
//! class names, a real scoreboard and real objective health. After a Deadlock update,
//! re-run `cargo run --release --example probe` to re-derive the layout constants.

pub mod error;
pub mod fields;
pub mod globals;

pub mod abi;
pub mod cache;
pub mod drift;
pub mod entity;
#[cfg(feature = "events")]
pub mod events;
/// The user's own ping, packet loss and jitter, read from the engine's net channel.
pub mod netchan;
pub mod reader;
pub mod schema;
pub mod snapshot;
/// Keeping a [`reader::Reader`] attached across the game starting, stopping and hiccuping.
pub mod supervise;
pub mod timers;
pub mod tunables;
#[cfg(feature = "events")]
pub mod watcher;

/// Local Steam account lookup.
#[cfg(any(windows, target_os = "linux"))]
pub mod steam;

pub use abi::Abi;
pub use drift::Drift;
pub use error::{Error, Result};

/// Shared vocabulary, re-exported so downstream crates need one dependency.
pub use deadlock_core as core_types;
pub use deadlock_core::{GameMode, GameState, HeroId, HeroNames, MatchMode, Team};
pub use reader::Reader;
pub use tunables::Tunables;

/// Image name of the game process on Windows and under Proton.
pub const DEADLOCK_PROCESS: &str = "deadlock.exe";

/// Image name a native Linux build would use, by Source 2 convention.
pub const DEADLOCK_PROCESS_NATIVE: &str = "deadlock";

/// Module the signatures are scanned in.
pub const CLIENT_MODULE: &str = "client.dll";

/// Deadlock's Steam `AppID`, used in replay CDN paths.
pub const DEADLOCK_APP_ID: u32 = 1_422_450;

/// Build the replay metadata URL for a match.
///
/// The template is `LAYOUT.md` §8:
/// `http://replay{cluster}.valve.net/1422450/{match_id}_{salt}.meta.bz2`
pub fn replay_meta_url(cluster_id: u32, match_id: u64, metadata_salt: u32) -> String {
    format!(
        "http://replay{cluster_id}.valve.net/{DEADLOCK_APP_ID}/{match_id}_{metadata_salt}.meta.bz2"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay_url_matches_the_original_template() {
        assert_eq!(
            super::replay_meta_url(123, 42_000_000, 987_654),
            "http://replay123.valve.net/1422450/42000000_987654.meta.bz2"
        );
    }
}
