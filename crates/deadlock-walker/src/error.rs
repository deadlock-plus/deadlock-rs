//! Error type shared by the resolver, the heap search and the walker.

use std::fmt;

/// Everything that can go wrong between an RTTI name and a block of wire bytes.
#[derive(Debug)]
pub enum Error {
    /// A read against the target process failed.
    Memory(deadlock_memory::Error),
    /// The module image is not a PE64 file this crate can read.
    BadImage(&'static str),
    /// No type descriptor, complete object locator or vtable for this class.
    ClassNotFound(String),
    /// The descriptor pool has no message with this name.
    UnknownMessage(String),
    /// No layout was supplied for this message.
    NoLayout(String),
    /// A field uses a protobuf feature the walker does not read (map, group, oneof).
    Unsupported {
        /// Message the field belongs to.
        message: String,
        /// Field number.
        field: u32,
        /// What is not supported.
        what: &'static str,
    },
    /// The image carries no compiled protobuf `DescriptorTable` this crate can read.
    NoTables,
    /// The descriptors embedded in the client do not decode into a pool.
    BadDescriptor(String),
    /// The object was freed, reused or overwritten while it was being read.
    Changed,
    /// A count, depth or size read from the target was beyond the configured limit.
    Limit(&'static str),
    /// A session was used against a different process than the one it was built on. Its
    /// vtable addresses belong to that process; build a new session.
    WrongProcess,
    /// A code pattern the lookup is anchored on is not in this build of the image.
    AnchorNotFound(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Memory(e) => write!(f, "memory read failed: {e}"),
            Error::BadImage(why) => write!(f, "unreadable module image: {why}"),
            Error::ClassNotFound(name) => write!(f, "no RTTI vtable for class {name}"),
            Error::UnknownMessage(name) => write!(f, "unknown message {name}"),
            Error::NoLayout(name) => write!(f, "no object layout for message {name}"),
            Error::Unsupported {
                message,
                field,
                what,
            } => write!(f, "{message} field {field}: {what} is not supported"),
            Error::NoTables => write!(f, "no compiled protobuf descriptor tables in the image"),
            Error::BadDescriptor(why) => write!(f, "client descriptors do not decode: {why}"),
            Error::Changed => write!(f, "object changed while it was being read"),
            Error::Limit(what) => write!(f, "limit exceeded: {what}"),
            Error::WrongProcess => write!(f, "session was built on a different process"),
            Error::AnchorNotFound(what) => write!(f, "anchor not found in the image: {what}"),
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

impl From<deadlock_memory::Error> for Error {
    fn from(e: deadlock_memory::Error) -> Self {
        Error::Memory(e)
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
