//! Shared vocabulary for the `deadlock-rs` crates.
//!
//! Types that describe the *game*, independent of where the values came from. A hero id
//! means the same thing whether it was read out of a live client's memory or fetched
//! from an API, so both [`deadlock-memory`] and [`deadlock-data`] speak these types and
//! neither has to depend on the other.
//!
//! Deliberately dependency-free (optional `serde` aside) so it costs nothing to pull in.
//!
//! [`deadlock-memory`]: https://docs.rs/deadlock-memory
//! [`deadlock-data`]: https://docs.rs/deadlock-data

#![forbid(unsafe_code)]

pub mod hero;
pub mod item;
pub mod rank;
pub mod state;
pub mod steam;
pub mod team;

pub use hero::{HeroId, HeroNames};
pub use item::{ItemId, ItemKind, ItemNames};
pub use rank::RankBadge;
pub use state::{
    BotDifficulty, ChatMode, ConnectionState, GameMode, GameState, LobbyTeam, MatchMode,
    MmPreference, Platform, PlayerType, RankedType, RegionMode,
};
pub use steam::{AccountId, SteamId};
pub use team::Team;
