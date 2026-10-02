//! Error type for process attachment and memory reads.

use std::fmt;

/// Everything that can go wrong attaching to a process and reading its memory.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// No process with the given image name is running.
    ProcessNotFound(String),
    /// The process is running but the requested module is not loaded in it.
    ModuleNotFound(String),
    /// `OpenProcess` failed; carries the raw `GetLastError` value.
    OpenProcess {
        /// Target process id.
        pid: u32,
        /// Raw Win32 error code.
        os: u32,
    },
    /// `ReadProcessMemory` failed outright.
    ReadMemory {
        /// Remote address that was requested.
        addr: u64,
        /// Number of bytes requested.
        len: usize,
        /// Raw Win32 error code.
        os: u32,
    },
    /// `ReadProcessMemory` succeeded but returned fewer bytes than requested.
    ShortRead {
        /// Remote address that was requested.
        addr: u64,
        /// Bytes requested.
        want: usize,
        /// Bytes actually transferred.
        got: usize,
    },
    /// A signature pattern was malformed.
    BadPattern(String),
    /// A signature did not match anywhere in the module image.
    SignatureNotFound(&'static str),
    /// A read asked for more bytes than [`crate::mem::MAX_READ_BYTES`].
    ///
    /// The length is refused before anything is allocated. The obvious use of a sized read
    /// is a length field taken out of the target process, and a corrupt one would
    /// otherwise turn into an arbitrary allocation.
    ReadTooLarge {
        /// Address the read was for.
        addr: u64,
        /// Length requested.
        len: usize,
        /// The cap.
        max: usize,
    },
    /// The process is there, but exposes no readable mapping to validate against.
    ///
    /// Not [`Error::PtraceDenied`]: nothing was refused. Not [`Error::ProcessNotFound`]:
    /// it exists. A zombie, or a process being torn down, looks like this.
    NoReadableMapping(u32),
    /// The signature matched, but the displacement it points at runs past the image.
    ///
    /// Distinct from [`Error::SignatureNotFound`] on purpose: "not found" reads as a
    /// patch having changed the code, and sends you looking for a new pattern. This means
    /// the pattern is still there and the image is short - a partial module read, or a
    /// descriptor whose `disp_off` is wrong for it.
    SignatureTruncated {
        /// Which signature.
        name: &'static str,
        /// Byte the displacement would have ended at.
        needs: usize,
        /// Bytes actually available.
        image_len: usize,
    },
    /// A signature matched but resolved to an address outside the module image, which
    /// means the pattern almost certainly matched the wrong instruction.
    OutsideModule {
        /// Signature name.
        name: &'static str,
        /// The bogus resolved address.
        va: u64,
    },
    /// The target process went away while it was being read.
    ///
    /// Distinct from [`Error::ModuleNotFound`] and [`Error::SignatureNotFound`] on
    /// purpose: those say "the target is not shaped the way this build expects", which
    /// sends someone looking for a target update. This says the process simply exited,
    /// which needs no investigation at all.
    ProcessGone {
        /// Target process id.
        pid: u32,
        /// Raw OS error code, where the platform gave one.
        os: u32,
    },
    /// A registry read failed; carries the raw Win32 error code.
    Registry(u32),
    /// The kernel refused to read the target's memory (Linux `EPERM`, usually Yama).
    PtraceDenied {
        /// Target process id.
        pid: u32,
        /// What the user can do about it.
        hint: String,
    },
    /// The current platform is not supported.
    Unsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ProcessNotFound(n) => write!(f, "{n} process not found"),
            Error::ModuleNotFound(n) => write!(f, "{n} module not found in target process"),
            Error::OpenProcess { pid, os } => {
                write!(f, "OpenProcess({pid}) failed: os error {os}")
            }
            Error::ReadMemory { addr, len, os } => {
                write!(
                    f,
                    "ReadProcessMemory({addr:#x}, {len}) failed: os error {os}"
                )
            }
            Error::ShortRead { addr, want, got } => write!(
                f,
                "ReadProcessMemory({addr:#x}) returned unexpected number of bytes read: \
                 wanted {want}, got {got}"
            ),
            Error::BadPattern(p) => write!(f, "malformed signature pattern: {p}"),
            Error::SignatureNotFound(n) => write!(f, "signature {n:?} not found in module image"),
            Error::ReadTooLarge { addr, len, max } => write!(
                f,
                "read of {len} bytes at {addr:#x} exceeds the {max}-byte cap"
            ),
            Error::NoReadableMapping(pid) => write!(
                f,
                "pid {pid} exposes no readable mapping; it is probably exiting"
            ),
            Error::SignatureTruncated {
                name,
                needs,
                image_len,
            } => write!(
                f,
                "signature {name:?} matched but its displacement needs {needs} bytes of a \
                 {image_len}-byte image"
            ),
            Error::OutsideModule { name, va } => {
                write!(
                    f,
                    "{name} resolved to {va:#x}, outside the module image - suspect"
                )
            }
            Error::Registry(os) => write!(f, "registry read failed: os error {os}"),
            Error::ProcessGone { pid, os } => {
                write!(f, "process {pid} exited while being read (os error {os})")
            }
            Error::PtraceDenied { pid, hint } => {
                write!(f, "not permitted to read pid {pid}\n  {hint}")
            }
            Error::Unsupported(what) => write!(f, "unsupported on this platform: {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
