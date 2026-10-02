//! Readers for Deadlock's replay files.
//!
//! Two kinds of file, two modules:
//!
//! - [`meta`]: the `.meta.bz2` match metadata Valve publishes when a match ends. It is the
//!   only place several numbers exist at all: weapon accuracy, headshot kills, damage
//!   mitigated, per-death positions, the damage matrix. The messages are `valveprotos`'.
//! - [`demo`]: the `.dem` files the game leaves in `citadel/replays/`, parsed by
//!   `haste_core`. They run to hundreds of megabytes, so [`demo::scan`] reads them under a
//!   byte budget.
//!
//! ```
//! # fn downloaded() -> Vec<u8> { Vec::new() }
//! # fn main() -> Result<(), deadlock_replay::Error> {
//! # let payload = downloaded();
//! # if payload.is_empty() { return Ok(()); }
//! let wrapper = deadlock_replay::meta::from_bz2(&payload)?;
//! let contents = deadlock_replay::meta::contents(&wrapper)?;
//! let info = contents.match_info.expect("a finished match has match_info");
//! # let _ = info;
//! # Ok(())
//! # }
//! ```
//!
//! No network here: the crate decodes bytes it is handed. Build the metadata URL with
//! `deadlock_reader::replay_meta_url`.
//!
//! The protobuf types come from `valveprotos`, pinned to a revision. A game patch that adds
//! a field is invisible until that pin moves; unknown fields are skipped, not rejected.

#![forbid(unsafe_code)]

#[cfg(feature = "bzip2")]
pub mod bz2;
pub mod demo;
pub mod error;
pub mod meta;

#[cfg(feature = "bzip2")]
pub use bz2::{DEFAULT_MAX_DECOMPRESSED, decompress, decompress_capped};
pub use demo::{
    Census, DEFAULT_MAX_TOTAL_BYTES, DemoLimits, Scan, TableSearch, read_file_header, scan,
    server_build,
};
pub use error::{Error, Result};
pub use prost;
pub use valveprotos;

#[cfg(test)]
mod tests;
