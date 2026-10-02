//! The memory-access abstraction everything else is built on.
//!
//! Everything built on a process needs exactly four things from the operating
//! system: read some bytes, read some bytes best-effort, list loaded modules, list mapped
//! regions. [`MemoryReader`] is that
//! surface, and it is the only thing a new platform has to implement.
//!
//! Backends:
//!
//! * `process::Process` - Windows, via `ReadProcessMemory` / `VirtualQueryEx`.
//! * `linux::LinuxProcess` - Linux, via `process_vm_readv` and `/proc/<pid>/maps`. Works
//!   against the Proton-hosted Windows build too.
//! * [`crate::mock::MockMemory`] - an in-memory fake, for tests on any platform.
//!
//! The first two are not intra-doc links on purpose: each exists on only one platform, so
//! linking either breaks the doc build on the other. `MockMemory` is unconditional and can
//! be linked.
//!
//! Use [`crate::attach_process`] rather than naming a backend, and the calling code stays
//! platform-independent.

use std::path::PathBuf;

use crate::error::{Error, Result};

/// A loaded module in the target process.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Module {
    /// Module file name, e.g. `client.dll`.
    pub name: String,
    /// Base address in the target process.
    pub base: u64,
    /// Image size in bytes.
    pub size: usize,
    /// Full path on disk, when the platform reports one.
    pub path: PathBuf,
}

/// A committed, readable-writable region worth scanning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Region {
    /// Base address in the target process.
    pub base: u64,
    /// Size in bytes.
    pub size: usize,
}

/// Chunk size for the bulk module image read.
pub const IMAGE_CHUNK: usize = 1024 * 1024;
/// Retry granularity when a chunk read fails.
pub const IMAGE_RETRY_CHUNK: usize = 4096;

/// Ceiling on a single sized read.
///
/// 16 MiB is far above any struct, string or vector this crate reads, and far below the
/// point where a corrupt length field becomes a problem of its own. The whole point of a
/// cap here is that the length usually comes *from* the target process.
pub const MAX_READ_BYTES: usize = 16 * 1024 * 1024;

/// Read-only access to another process's address space.
///
/// Implementors provide the five required methods; the typed readers are supplied.
/// The trait is object-safe, so a `Reader` can hold `Arc<dyn MemoryReader>` and callers
/// never see a generic parameter.
///
/// `Send + Sync` is required rather than optional: the intended consumers are GUI and
/// server apps that poll from a background thread and share the reader across handlers.
pub trait MemoryReader: Send + Sync {
    /// The target process id.
    fn pid(&self) -> u32;

    /// Read exactly `buf.len()` bytes from `addr`, or fail.
    fn read_into(&self, addr: u64, buf: &mut [u8]) -> Result<()>;

    /// Best-effort read: returns how many bytes were transferred, never fails.
    ///
    /// Used wherever partial success is normal - bulk image copies, and scanning regions
    /// that may be unmapped by the time we get to them.
    fn read_partial(&self, addr: u64, buf: &mut [u8]) -> usize;

    /// Every module loaded in the target.
    fn modules(&self) -> Result<Vec<Module>>;

    /// Every committed readable-writable region in the target.
    fn regions(&self) -> Result<Vec<Region>>;

    /// The committed readable-writable region containing `addr`, if there is one.
    ///
    /// Worth its own method rather than a filter over [`MemoryReader::regions`]: walking
    /// the whole address space measured **280 ms** against a live client with 6,900
    /// regions, which is longer than searching the region it finds. A caller that already
    /// knows roughly where to look should not have to pay for the walk.
    ///
    /// The default implementation does pay for it. A backend that can answer directly -
    /// on Windows, one `VirtualQueryEx` - should override this.
    fn region_at(&self, addr: u64) -> Option<Region> {
        self.regions()
            .ok()?
            .into_iter()
            .find(|r| addr >= r.base && addr - r.base < r.size as u64)
    }

    /// Find one module by name, case-insensitively.
    fn module(&self, name: &str) -> Result<Module> {
        self.modules()?
            .into_iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::ModuleNotFound(name.to_string()))
    }

    /// Read `len` bytes from `addr`.
    ///
    /// # Errors
    ///
    /// [`Error::ReadTooLarge`] if `len` is above [`MAX_READ_BYTES`], refused before
    /// anything is allocated. Any read error from the backend otherwise.
    fn read_bytes(&self, addr: u64, len: usize) -> Result<Vec<u8>> {
        if len > MAX_READ_BYTES {
            return Err(Error::ReadTooLarge {
                addr,
                len,
                max: MAX_READ_BYTES,
            });
        }
        let mut buf = vec![0u8; len];
        self.read_into(addr, &mut buf)?;
        Ok(buf)
    }

    /// Read a `u8`.
    fn read_u8(&self, addr: u64) -> Result<u8> {
        let mut b = [0u8; 1];
        self.read_into(addr, &mut b)?;
        Ok(b[0])
    }

    /// Read a `u16`.
    fn read_u16(&self, addr: u64) -> Result<u16> {
        let mut b = [0u8; 2];
        self.read_into(addr, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }

    /// Read a `u32`.
    fn read_u32(&self, addr: u64) -> Result<u32> {
        let mut b = [0u8; 4];
        self.read_into(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Read an `i32`.
    fn read_i32(&self, addr: u64) -> Result<i32> {
        Ok(self.read_u32(addr)? as i32)
    }

    /// Read a `u64`.
    fn read_u64(&self, addr: u64) -> Result<u64> {
        let mut b = [0u8; 8];
        self.read_into(addr, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// Read an `f32`.
    fn read_f32(&self, addr: u64) -> Result<f32> {
        Ok(f32::from_bits(self.read_u32(addr)?))
    }

    /// Read a pointer-sized value.
    fn read_ptr(&self, addr: u64) -> Result<u64> {
        self.read_u64(addr)
    }

    /// Read three consecutive `f32`s, i.e. a `Vector`.
    fn read_vec3(&self, addr: u64) -> Result<[f32; 3]> {
        let mut b = [0u8; 12];
        self.read_into(addr, &mut b)?;
        Ok([
            f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            f32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            f32::from_le_bytes([b[8], b[9], b[10], b[11]]),
        ])
    }

    /// Read a NUL-terminated UTF-8 string, up to `max` bytes.
    ///
    /// Reads in 64-byte steps so a string near the end of a mapping does not fail the
    /// whole read.
    ///
    /// An address that yields nothing at all is an error rather than an empty string.
    /// The two are very different to a caller - one names a class, the other means the
    /// read failed - and collapsing them is what allowed a single transient failure to be
    /// memoised as a real name, after which every entity of that class stopped matching.
    /// A string that runs to `max` without a terminator still returns what was read.
    fn read_cstr(&self, addr: u64, max: usize) -> Result<String> {
        const STEP: usize = 64;
        if max == 0 {
            return Ok(String::new());
        }
        let mut out: Vec<u8> = Vec::with_capacity(STEP.min(max));
        // One stack buffer for the whole walk; the old code allocated per chunk.
        let mut buf = [0u8; STEP];
        let mut off = 0usize;
        while off < max {
            let want = STEP.min(max - off);
            // Wrapping: `addr` came from the game and can sit anywhere, including where
            // a plain `+` overflows - which panics in a debug build and wraps in release.
            let got = self.read_partial(addr.wrapping_add(off as u64), &mut buf[..want]);
            if got == 0 {
                if off == 0 {
                    return Err(Error::ShortRead { addr, want, got: 0 });
                }
                break;
            }
            if let Some(nul) = buf[..got].iter().position(|&c| c == 0) {
                out.extend_from_slice(&buf[..nul]);
                return Ok(String::from_utf8_lossy(&out).into_owned());
            }
            out.extend_from_slice(&buf[..got]);
            off += got;
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Copy a whole module image into a local buffer.
    ///
    /// 1 MiB chunks, retrying failed chunks page by page so a few unreadable pages do
    /// not abort the scan. Unreadable bytes are left zeroed. Returns the buffer and the
    /// number of bytes actually transferred.
    fn read_image(&self, base: u64, size: usize) -> (Vec<u8>, usize) {
        let mut image = vec![0u8; size];
        let mut transferred = 0usize;
        let mut off = 0usize;

        while off < size {
            let chunk = IMAGE_CHUNK.min(size - off);
            let got =
                self.read_partial(base.wrapping_add(off as u64), &mut image[off..off + chunk]);
            if got == chunk {
                transferred += got;
                off += chunk;
                continue;
            }
            let end = off + chunk;
            let mut p = off;
            while p < end {
                let step = IMAGE_RETRY_CHUNK.min(end - p);
                transferred +=
                    self.read_partial(base.wrapping_add(p as u64), &mut image[p..p + step]);
                p += step;
            }
            off = end;
        }
        (image, transferred)
    }
}

/// Blanket impl so `&R`, `Arc<R>` and `Box<R>` are usable wherever a reader is expected.
impl<T: MemoryReader + ?Sized> MemoryReader for std::sync::Arc<T> {
    fn pid(&self) -> u32 {
        (**self).pid()
    }
    fn read_into(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        (**self).read_into(addr, buf)
    }
    fn read_partial(&self, addr: u64, buf: &mut [u8]) -> usize {
        (**self).read_partial(addr, buf)
    }
    fn modules(&self) -> Result<Vec<Module>> {
        (**self).modules()
    }
    fn regions(&self) -> Result<Vec<Region>> {
        (**self).regions()
    }
    // Forwarded rather than left to the default, which would walk the whole address space
    // instead of using the backend's single-query answer.
    fn region_at(&self, addr: u64) -> Option<Region> {
        (**self).region_at(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockMemory;

    /// The bug this guards: an unreadable address used to return `Ok("")`, which callers
    /// could not tell from a genuinely empty name. `NameCache` then memoised it.
    #[test]
    fn an_unreadable_address_is_an_error_not_an_empty_string() {
        let m = MockMemory::new(1);
        match m.read_cstr(0xDEAD_0000, 96) {
            Err(Error::ShortRead { addr, got, .. }) => {
                assert_eq!(addr, 0xDEAD_0000);
                assert_eq!(got, 0);
            }
            other => panic!("expected a ShortRead, got {other:?}"),
        }
    }

    /// A string that really is empty still reads as one, so the error above cannot be
    /// mistaken for "every empty string now fails".
    #[test]
    fn a_genuinely_empty_string_still_reads_as_empty() {
        let mut m = MockMemory::new(1);
        m.write(0x1000, &[0u8, b'x', b'y']);
        assert_eq!(m.read_cstr(0x1000, 96).unwrap(), "");
    }

    #[test]
    fn a_terminated_string_reads_up_to_the_nul() {
        let mut m = MockMemory::new(1);
        m.write(0x1000, b"C_CitadelPlayerPawn\0trailing rubbish");
        assert_eq!(m.read_cstr(0x1000, 96).unwrap(), "C_CitadelPlayerPawn");
    }

    /// Longer than one 64-byte step, to exercise the chunk loop and the reused buffer.
    #[test]
    fn a_string_spanning_several_chunks_reads_whole() {
        let mut m = MockMemory::new(1);
        let long = "a".repeat(150);
        let mut bytes = long.clone().into_bytes();
        bytes.push(0);
        m.write(0x1000, &bytes);
        assert_eq!(m.read_cstr(0x1000, 256).unwrap(), long);
    }

    /// No terminator inside `max` is a truncation, not a failure: the caller asked for a
    /// bounded read and gets what fitted.
    #[test]
    fn an_unterminated_string_returns_what_fitted() {
        let mut m = MockMemory::new(1);
        m.write(0x1000, &[b'z'; 200]);
        assert_eq!(m.read_cstr(0x1000, 8).unwrap(), "zzzzzzzz");
    }

    #[test]
    fn a_zero_length_request_is_not_an_error() {
        let m = MockMemory::new(1);
        assert_eq!(m.read_cstr(0xDEAD_0000, 0).unwrap(), "");
    }
}
