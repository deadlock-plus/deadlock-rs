//! Errors from decoding replay metadata and replay demo files.

use std::fmt;

/// What went wrong turning a `.meta.bz2` payload, or a `.dem` file, into messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A protobuf message did not decode.
    Decode(prost::DecodeError),
    /// [`contents`] was asked for a wrapper that carries no `match_details`.
    ///
    /// [`contents`]: crate::meta::contents
    MissingMatchDetails,
    /// The payload does not begin with a bzip2 header.
    ///
    /// Checked before any block is decoded, because the common failure is not a corrupt
    /// stream but the CDN answering with something that is not a stream at all.
    NotBzip2,
    /// The bzip2 decoder rejected a block.
    Bzip2(String),
    /// The bzip2 stream ended before its last block was complete.
    TruncatedStream,
    /// Decompression would have produced more than the caller allowed.
    TooLarge {
        /// The cap that was exceeded, in bytes.
        cap: usize,
    },
    /// The file does not open with the eight-byte `PBDEMS2\0` demo magic.
    NotDemo,
    /// The demo parser failed partway through.
    ///
    /// Carries the parser's own message, which is an `anyhow` chain and so not comparable.
    Demo(String),
    /// A demo carries no `DEM_FileHeader` where one was required.
    MissingFileHeader,
    /// The file could not be read.
    ///
    /// The kind is kept alongside the message because a missing replay and an unreadable
    /// one are different answers, and `std::io::Error` itself is neither `Clone` nor
    /// `PartialEq`, which this enum is.
    Io {
        /// The underlying [`std::io::ErrorKind`].
        kind: std::io::ErrorKind,
        /// The underlying error, rendered.
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Decode(e) => write!(f, "protobuf: {e}"),
            Error::MissingMatchDetails => {
                write!(f, "match metadata carries no match_details block")
            }
            Error::NotBzip2 => write!(f, "payload does not start with a bzip2 header"),
            Error::Bzip2(m) => write!(f, "bzip2: {m}"),
            Error::TruncatedStream => write!(f, "bzip2 stream ended mid-block"),
            Error::TooLarge { cap } => {
                write!(f, "decompressed output would exceed {cap} bytes")
            }
            Error::NotDemo => write!(f, "file does not start with the PBDEMS2 demo magic"),
            Error::Demo(m) => write!(f, "demo: {m}"),
            Error::MissingFileHeader => write!(f, "demo carries no DEM_FileHeader"),
            Error::Io { message, .. } => write!(f, "io: {message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io {
            kind: e.kind(),
            message: e.to_string(),
        }
    }
}

impl From<prost::DecodeError> for Error {
    fn from(e: prost::DecodeError) -> Self {
        Error::Decode(e)
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
