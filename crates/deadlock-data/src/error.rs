//! Errors from loading reference data.

use std::path::PathBuf;

/// Something went wrong loading reference data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A file could not be read.
    Io {
        /// Path that was attempted.
        path: PathBuf,
        /// What kind of failure it was.
        ///
        /// Kept alongside the message because it is the part a caller can act on:
        /// `NotFound` means the game is not installed there, `PermissionDenied` means it
        /// is and something needs fixing. Flattening both into a string made them the
        /// same to everyone downstream.
        kind: std::io::ErrorKind,
        /// Underlying message.
        source: String,
    },
    /// Data was read but could not be understood.
    Parse(String),
    /// A network request failed. Only produced with the `online` feature.
    Http {
        /// HTTP status, when the server answered at all.
        ///
        /// `None` means the request never got a response - DNS, connect or timeout.
        status: Option<u16>,
        /// Underlying message.
        source: String,
    },
    /// No installed game could be located.
    GameNotFound,
    /// The installed game's archive has no entry at this path.
    AssetNotFound(String),
}

impl Error {
    /// Whether trying the same request again could plausibly succeed.
    ///
    /// Rate limits, request timeouts and server-side faults are worth another attempt; a
    /// 404 is not. Without this a caller could not tell them apart at all, because every
    /// failure arrived as one opaque string.
    ///
    /// No retry happens automatically. This is a blocking API, and a call that silently
    /// sleeps for seconds inside a UI thread is worse than an error the caller can decide
    /// about.
    pub fn is_retryable(&self) -> bool {
        match self {
            // No response at all: DNS, connect, or a timeout. Usually transient.
            Error::Http { status: None, .. } => true,
            Error::Http {
                status: Some(code), ..
            } => matches!(code, 408 | 429 | 500..=599),
            _ => false,
        }
    }

    /// HTTP status, when this was a request that got a response.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Http { status, .. } => *status,
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io { path, source, .. } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            Error::Parse(m) => write!(f, "could not parse data: {m}"),
            Error::Http {
                status: Some(code),
                source,
            } => write!(f, "request failed with status {code}: {source}"),
            Error::Http {
                status: None,
                source,
            } => write!(f, "request failed: {source}"),
            Error::GameNotFound => f.write_str("no Deadlock installation found"),
            Error::AssetNotFound(path) => write!(f, "the game has no file at {path}"),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
