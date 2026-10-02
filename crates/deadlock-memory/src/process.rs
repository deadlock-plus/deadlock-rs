//! Windows [`MemoryReader`] backend. Read-only by construction.
//!
//! The only Win32 calls made are `OpenProcess`, `ReadProcessMemory`, `VirtualQueryEx`,
//! the Toolhelp32 snapshot family, and `CloseHandle`. There is deliberately no write
//! path.

use std::ffi::c_void;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{
    CloseHandle, FALSE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W,
    Process32FirstW, Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQueryEx};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

use crate::error::{Error, Result};
use crate::mem::{MemoryReader, Module, Region};
use crate::region;

/// Access mask for the reading handle: query + read, nothing else.
///
/// Matches the original binary's `OpenProcess(0x410, ...)`.
pub const ACCESS_READ: u32 = PROCESS_QUERY_INFORMATION | PROCESS_VM_READ;

/// Access mask for a query-only handle, as the original uses for region enumeration.
pub const ACCESS_QUERY: u32 = PROCESS_QUERY_INFORMATION;

/// An open, read-only handle to a process.
#[derive(Debug)]
pub struct Process {
    handle: HANDLE,
    pid: u32,
}

// SAFETY: a process handle is a kernel object reference. `ReadProcessMemory` and
// `VirtualQueryEx` are thread-safe against the same handle, and this type exposes no
// interior mutability, so sharing it across threads is sound. Required so `Reader` can
// live in a Tauri `State` or behind an `Arc` in a polling thread.
unsafe impl Send for Process {}
// SAFETY: as above. `Sync` is the same argument: no `&self` method mutates anything, so
// concurrent shared access cannot race.
unsafe impl Sync for Process {}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            // SAFETY: the handle came from `OpenProcess` in this type's constructor and is
            // closed exactly once, here, because `Process` is not `Copy` or `Clone`. The
            // guard above rejects the two values `OpenProcess` never returns.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// `GetLastError` for the calling thread.
///
/// Wrapped rather than called inline in five places: it takes no arguments, touches only
/// the caller's own thread-local error slot, and cannot fail, so there is exactly one
/// safety argument to make and no reason to restate it at each call site.
fn last_error() -> u32 {
    // SAFETY: no arguments to get wrong, no pointers involved, and the value read belongs
    // to this thread. Sound to call at any time.
    unsafe { GetLastError() }
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// RAII wrapper so early returns inside the snapshot loops still close the handle.
struct Snapshot(HANDLE);

impl Drop for Snapshot {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: same as `Process`: a snapshot handle from `CreateToolhelp32Snapshot`,
            // owned solely by this wrapper and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

impl Snapshot {
    fn new(flags: u32, pid: u32) -> Option<Self> {
        // SAFETY: both arguments are plain integers. The call allocates a snapshot and
        // returns a handle or a sentinel, and no pointer crosses the boundary.
        let h = unsafe { CreateToolhelp32Snapshot(flags, pid) };
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            None
        } else {
            Some(Snapshot(h))
        }
    }
}

impl Process {
    /// Find the pid of the first process whose image name matches `exe_name`
    /// (case-insensitive).
    pub fn find_pid(exe_name: &str) -> Option<u32> {
        let snap = Snapshot::new(TH32CS_SNAPPROCESS, 0)?;
        // SAFETY: `PROCESSENTRY32W` is a C struct of integers and fixed-size arrays, so
        // all-zeroes is a valid value with no padding or niche to violate. Win32 requires
        // it be zeroed and `dwSize` set before the walk, which the next line does.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        // SAFETY: `snap.0` is live for the whole loop - the `Snapshot` guard outlives it -
        // and `entry` is a correctly sized, initialised struct that Win32 only writes
        // through the pointer it is given.
        let mut ok = unsafe { Process32FirstW(snap.0, &mut entry) };
        while ok != FALSE {
            if wide_to_string(&entry.szExeFile).eq_ignore_ascii_case(exe_name) {
                return Some(entry.th32ProcessID);
            }
            // SAFETY: as for `Process32FirstW`; the snapshot and entry are unchanged.
            ok = unsafe { Process32NextW(snap.0, &mut entry) };
        }
        None
    }

    /// Open a process by pid with the given access mask.
    pub fn open(pid: u32, access: u32) -> Result<Self> {
        // SAFETY: three integer arguments. Returns a fresh handle this type then owns, or
        // null, which the caller checks.
        let handle = unsafe { OpenProcess(access, FALSE, pid) };
        if handle.is_null() {
            return Err(Error::OpenProcess {
                pid,
                os: last_error(),
            });
        }
        Ok(Process { handle, pid })
    }

    /// Find `exe_name` and open it for reading.
    pub fn attach(exe_name: &str) -> Result<Self> {
        let pid =
            Self::find_pid(exe_name).ok_or_else(|| Error::ProcessNotFound(exe_name.to_string()))?;
        Self::open(pid, ACCESS_READ)
    }

    /// Raw handle, for callers that need their own read-only Win32 calls.
    ///
    /// # Safety
    /// Valid only for the lifetime of this `Process`. It was opened without write
    /// access, so writes through it will fail.
    pub unsafe fn raw_handle(&self) -> HANDLE {
        self.handle
    }
}

impl MemoryReader for Process {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn read_into(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let mut got: usize = 0;
        // SAFETY: `buf` is a live, exclusively borrowed slice and `buf.len()` is its true
        // length, so the only memory written in this process is inside it. `addr` is in the
        // *target* process and is not dereferenced here: an invalid one fails the call
        // rather than faulting. `got` is a live local.
        let ok = unsafe {
            ReadProcessMemory(
                self.handle,
                addr as *const c_void,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                &mut got,
            )
        };
        if ok == FALSE {
            return Err(Error::ReadMemory {
                addr,
                len: buf.len(),
                os: last_error(),
            });
        }
        if got != buf.len() {
            return Err(Error::ShortRead {
                addr,
                want: buf.len(),
                got,
            });
        }
        Ok(())
    }

    fn read_partial(&self, addr: u64, buf: &mut [u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        let mut got: usize = 0;
        // The return value is deliberately ignored: `got` is meaningful either way. A
        // range running into an inaccessible page fails with `STATUS_PARTIAL_COPY` having
        // already transferred the bytes up to it, and reporting zero there throws away
        // everything that was read. `got` starts at zero, so a failure that transferred
        // nothing still reports nothing.
        // SAFETY: as in `read_into`. The return value is ignored on purpose (see above),
        // which changes nothing about the call's soundness - `got` is written either way.
        unsafe {
            ReadProcessMemory(
                self.handle,
                addr as *const c_void,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                &mut got,
            );
        }
        got
    }

    fn modules(&self) -> Result<Vec<Module>> {
        let snap = Snapshot::new(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, self.pid)
            // Not `Registry`: nothing here touches the registry, and a failed toolhelp
            // snapshot on a pid we hold a handle to means the process is on its way out.
            .ok_or_else(|| Error::ProcessGone {
                pid: self.pid,
                os: last_error(),
            })?;

        let mut out = Vec::new();
        // SAFETY: as for `PROCESSENTRY32W`: integers and fixed-size arrays only, and
        // `dwSize` is set immediately below as Win32 requires.
        let mut entry: MODULEENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;

        // SAFETY: `snap.0` is live for the whole loop and `entry` is a correctly sized,
        // initialised struct written only through the pointer passed.
        let mut ok = unsafe { Module32FirstW(snap.0, &mut entry) };
        if ok == FALSE {
            // A live process always has at least its own image mapped, so an empty walk
            // means the snapshot failed - almost always because the process exited
            // between opening it and walking it. Returning `Ok(vec![])` turned that into
            // `ModuleNotFound("client.dll")`, which reads as "a game update broke us".
            return Err(Error::ProcessGone {
                pid: self.pid,
                os: last_error(),
            });
        }
        while ok != FALSE {
            out.push(Module {
                name: wide_to_string(&entry.szModule),
                base: entry.modBaseAddr as u64,
                size: entry.modBaseSize as usize,
                path: PathBuf::from(wide_to_string(&entry.szExePath)),
            });
            // SAFETY: as for `Module32FirstW`; the snapshot and entry are unchanged.
            ok = unsafe { Module32NextW(snap.0, &mut entry) };
        }
        Ok(out)
    }

    /// One `VirtualQueryEx` instead of a walk from the bottom of the address space.
    fn region_at(&self, addr: u64) -> Option<Region> {
        // SAFETY: a C struct of integers and pointers; all-zeroes is a valid value, and
        // `VirtualQueryEx` overwrites it before anything reads it.
        let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: as in `regions` - a live, correctly sized `mbi`, and an address in the
        // target process that is only queried.
        let n = unsafe {
            VirtualQueryEx(
                self.handle,
                addr as *const c_void,
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if n == 0 {
            return None;
        }
        let size = mbi.RegionSize;
        region::accepts(mbi.State, mbi.Protect, mbi.Type, size).then_some(Region {
            base: mbi.BaseAddress as u64,
            size,
        })
    }

    fn regions(&self) -> Result<Vec<Region>> {
        let mut out = Vec::new();
        let mut addr = region::SCAN_START;

        while (addr >> 16) < region::SCAN_END_SHIFTED {
            // SAFETY: a C struct of integers and pointers; all-zeroes is a valid value, and
            // `VirtualQueryEx` overwrites it before anything reads it.
            let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `mbi` is a live local of exactly the size passed, and `addr` names
            // an address in the target process that is queried rather than dereferenced.
            let n = unsafe {
                VirtualQueryEx(
                    self.handle,
                    addr as *const c_void,
                    &mut mbi,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if n == 0 {
                if out.is_empty() && addr == region::SCAN_START {
                    return Err(Error::Registry(last_error()));
                }
                break;
            }

            let base = mbi.BaseAddress as u64;
            let size = mbi.RegionSize;

            if region::accepts(mbi.State, mbi.Protect, mbi.Type, size) {
                out.push(Region { base, size });
            }

            let next = base.saturating_add(size as u64);
            if next <= addr {
                break; // no forward progress; bail rather than spin
            }
            addr = next;
        }
        Ok(out)
    }
}
