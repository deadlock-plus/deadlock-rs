//! An in-memory [`MemoryReader`] for tests, available on every platform.
//!
//! Lets code built on [`MemoryReader`] be exercised without a live process - and without
//! Windows. Downstream crates can use it to test their own code against synthetic memory.
//!
//! ```
//! use deadlock_memory::mem::MemoryReader;
//! use deadlock_memory::mock::MockMemory;
//!
//! let mut m = MockMemory::new(1234);
//! m.write(0x1000, &0xDEAD_BEEFu32.to_le_bytes());
//! m.write_cstr(0x2000, "C_CitadelPlayerPawn");
//!
//! assert_eq!(m.read_u32(0x1000).unwrap(), 0xDEAD_BEEF);
//! assert_eq!(m.read_cstr(0x2000, 64).unwrap(), "C_CitadelPlayerPawn");
//! // Unmapped addresses fail rather than returning zeroes.
//! assert!(m.read_u32(0x9999_9999).is_err());
//! ```

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};
use crate::mem::{MemoryReader, Module, Region};

/// A sparse fake address space.
#[derive(Debug, Default)]
pub struct MockMemory {
    pid: u32,
    /// Base address -> bytes. Segments must not overlap.
    segments: BTreeMap<u64, Vec<u8>>,
    modules: Vec<Module>,
    regions: Vec<Region>,
    /// How many times the address space has been read.
    ///
    /// Lets a test assert that a bulk read really is one read, rather than only that the
    /// values came out right, which stays true even if the bulk read is silently doing
    /// nothing. Interior mutability because `MemoryReader` reads through `&self`.
    reads: AtomicUsize,
}

// Hand-written: `AtomicUsize` is not `Clone`, and a clone should start its own count
// rather than inherit one taken before the copy.
impl Clone for MockMemory {
    fn clone(&self) -> Self {
        MockMemory {
            pid: self.pid,
            segments: self.segments.clone(),
            modules: self.modules.clone(),
            regions: self.regions.clone(),
            reads: AtomicUsize::new(0),
        }
    }
}

impl MockMemory {
    /// An empty address space.
    pub fn new(pid: u32) -> Self {
        MockMemory {
            pid,
            ..Default::default()
        }
    }

    /// How many reads have reached the address space so far.
    ///
    /// Counts calls, not bytes: one bulk read of an object is one, and so is one four-byte
    /// field read.
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }

    /// Reset the counter, so a test can measure one operation rather than everything since
    /// the fixture was built.
    pub fn reset_reads(&self) {
        self.reads.store(0, Ordering::Relaxed);
    }

    /// Map `bytes` at `addr`, replacing any segment with the same base.
    pub fn write(&mut self, addr: u64, bytes: &[u8]) -> &mut Self {
        self.segments.insert(addr, bytes.to_vec());
        self
    }

    /// Map a NUL-terminated string at `addr`.
    pub fn write_cstr(&mut self, addr: u64, s: &str) -> &mut Self {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        self.write(addr, &v)
    }

    /// Map a little-endian `u64` at `addr`.
    pub fn write_u64(&mut self, addr: u64, v: u64) -> &mut Self {
        self.write(addr, &v.to_le_bytes())
    }

    /// Map a little-endian `u32` at `addr`.
    pub fn write_u32(&mut self, addr: u64, v: u32) -> &mut Self {
        self.write(addr, &v.to_le_bytes())
    }

    /// Declare a module.
    pub fn add_module(&mut self, name: &str, base: u64, size: usize) -> &mut Self {
        self.modules.push(Module {
            name: name.to_string(),
            base,
            size,
            path: std::path::PathBuf::from(name),
        });
        self
    }

    /// Declare a scannable region.
    pub fn add_region(&mut self, base: u64, size: usize) -> &mut Self {
        self.regions.push(Region { base, size });
        self
    }

    /// Copy out whatever is mapped at `addr`, up to `len` bytes.
    ///
    /// Returns the number of contiguous bytes available from `addr`.
    fn gather(&self, addr: u64, buf: &mut [u8]) -> usize {
        self.reads.fetch_add(1, Ordering::Relaxed);
        // Find the segment containing `addr`.
        let Some((&base, seg)) = self.segments.range(..=addr).next_back() else {
            return 0;
        };
        let end = base + seg.len() as u64;
        if addr >= end {
            return 0;
        }
        let off = (addr - base) as usize;
        let n = buf.len().min(seg.len() - off);
        buf[..n].copy_from_slice(&seg[off..off + n]);
        n
    }
}

impl MemoryReader for MockMemory {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn read_into(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        let n = self.gather(addr, buf);
        if n != buf.len() {
            return Err(Error::ShortRead {
                addr,
                want: buf.len(),
                got: n,
            });
        }
        Ok(())
    }

    fn read_partial(&self, addr: u64, buf: &mut [u8]) -> usize {
        self.gather(addr, buf)
    }

    fn modules(&self) -> Result<Vec<Module>> {
        Ok(self.modules.clone())
    }

    fn regions(&self) -> Result<Vec<Region>> {
        Ok(self.regions.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_within_a_segment() {
        let mut m = MockMemory::new(1);
        m.write(0x1000, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(m.read_u32(0x1000).unwrap(), 0x0403_0201);
        assert_eq!(m.read_u32(0x1004).unwrap(), 0x0807_0605);
    }

    #[test]
    fn unmapped_reads_fail_rather_than_returning_zero() {
        let m = MockMemory::new(1);
        assert!(m.read_u32(0x1000).is_err());
        assert_eq!(m.read_partial(0x1000, &mut [0u8; 4]), 0);
    }

    #[test]
    fn reads_do_not_run_off_the_end_of_a_segment() {
        let mut m = MockMemory::new(1);
        m.write(0x1000, &[1, 2, 3, 4]);
        assert!(m.read_u64(0x1000).is_err());
        assert_eq!(m.read_partial(0x1000, &mut [0u8; 8]), 4);
    }

    #[test]
    fn cstr_stops_at_the_nul() {
        let mut m = MockMemory::new(1);
        m.write_cstr(0x2000, "hello");
        assert_eq!(m.read_cstr(0x2000, 64).unwrap(), "hello");
    }

    #[test]
    fn module_lookup_is_case_insensitive() {
        let mut m = MockMemory::new(1);
        m.add_module("client.dll", 0x1_0000, 0x1000);
        assert_eq!(m.module("CLIENT.DLL").unwrap().base, 0x1_0000);
        assert!(m.module("engine2.dll").is_err());
    }
}
