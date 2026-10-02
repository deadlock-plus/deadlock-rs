//! Error type for the reader.

use std::fmt;
use std::path::PathBuf;

/// Everything that can go wrong attaching to and reading the game.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Attaching to, or reading from, the process failed.
    Memory(deadlock_memory::Error),
    /// Steam's registry mirror exists but could not be read.
    ///
    /// Kept apart from "nobody is signed in", which is what this used to look like. The
    /// two need opposite responses: one is a permissions problem on a known file, the
    /// other is a perfectly healthy machine with Steam logged out.
    SteamRegistryUnreadable {
        /// The file that could not be read.
        path: PathBuf,
        /// Underlying message.
        source: String,
    },
    /// The schema system could not be walked; field offsets fall back to the baked table.
    SchemaUnresolved(String),
    /// The entity system global was never resolved, so no entity walk is possible.
    EntitySystemUnresolved,
    /// An [`EntityLayout`](crate::entity::EntityLayout) the entity walk cannot run with.
    ///
    /// The layout's fields are public, so a caller can supply a zero stride or an absurd
    /// chunk shift. Those would panic or allocate wildly during a walk, so they are
    /// rejected at the point they are set.
    InvalidLayout(&'static str),
    /// An operation needed a live attachment that has not been established.
    NotAttached,
    /// The target's ABI has no derived constants in this build.
    UnsupportedAbi {
        /// The detected ABI.
        abi: crate::abi::Abi,
    },
    /// A [`crate::watcher::Watcher`] could not be configured or started.
    Watcher(String),
}

impl From<deadlock_memory::Error> for Error {
    fn from(e: deadlock_memory::Error) -> Self {
        Error::Memory(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Memory(e) => e.fmt(f),
            Error::SteamRegistryUnreadable { path, source } => write!(
                f,
                "Steam's registry mirror at {} could not be read: {source}",
                path.display()
            ),
            Error::SchemaUnresolved(why) => write!(f, "schema unresolved: {why}"),
            Error::EntitySystemUnresolved => write!(f, "entity_system global unresolved"),
            Error::InvalidLayout(why) => write!(f, "invalid entity layout: {why}"),
            Error::NotAttached => write!(f, "not attached to the game process"),
            Error::UnsupportedAbi { abi } => write!(
                f,
                "target is {abi}, for which no signatures or struct layouts have been \
                 derived yet; see the porting notes in README.md"
            ),
            Error::Watcher(what) => write!(f, "watcher: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Memory(e) => Some(e),
            _ => None,
        }
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
