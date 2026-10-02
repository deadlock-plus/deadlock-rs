//! A read-only external process-memory client.
//!
//! Attach to a process by image name, read its memory, list its modules and mapped regions,
//! and scan for byte signatures. Nothing here knows about any particular program.
//!
//! # What it does not do
//!
//! There is no write path anywhere in this crate. No `WriteProcessMemory`, no injection,
//! no hooking, no remote thread creation, no calls into the target. It only observes.
//!
//! # Quick start
//!
//! ```no_run
//! use deadlock_memory::{attach_process, MemoryReader};
//!
//! let process = attach_process("notepad.exe")?;
//! for module in process.modules()? {
//!     println!("{} @ {:#x} ({} bytes)", module.name, module.base, module.size);
//! }
//! # Ok::<(), deadlock_memory::Error>(())
//! ```
//!
//! Tests that need no live process can use `mock::MockMemory`, behind the `mock` feature.

pub mod error;
pub mod mem;
/// Test scaffolding, behind the `mock` feature.
///
/// Gated because it is only ever useful to a test: shipping a fake address space in every
/// release build of a library whose entire job is reading a real one is dead weight in the
/// binary and a confusing thing to find in the public API. The crate's own tests enable it
/// through a dev-dependency on itself, and a consumer writing tests against
/// [`MemoryReader`] can do the same.
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod procmaps;
pub mod region;
pub mod sig;

/// Linux [`mem::MemoryReader`] backend, covering Proton-hosted and native clients.
#[cfg(target_os = "linux")]
pub mod linux;
/// Windows [`mem::MemoryReader`] backend.
#[cfg(windows)]
pub mod process;

/// Attach to a process by executable name, using whichever backend this platform has.
///
/// `process::Process` and `linux::LinuxProcess` are the same idea behind two
/// `#[cfg]`s, and naming either one directly makes the calling code platform-specific
/// too. That is how fourteen of this workspace's examples ended up unable to compile on
/// Linux while the crate advertised Proton support.
///
/// Neither name above is an intra-doc link, and cannot be: each exists on only one
/// platform, so whichever one is linked fails to resolve when the docs are built on the
/// other.
///
/// The returned reader is boxed so callers never name the concrete backend.
#[cfg(any(windows, target_os = "linux"))]
pub fn attach_process(exe_name: &str) -> Result<Box<dyn MemoryReader>> {
    #[cfg(windows)]
    {
        Ok(Box::new(process::Process::attach(exe_name)?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::LinuxProcess::attach(exe_name)?))
    }
}

pub use error::{Error, Result};
pub use mem::{MemoryReader, Module, Region};
