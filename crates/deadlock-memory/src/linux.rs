//! Linux [`MemoryReader`] backend. Read-only by construction.
//!
//! Reads use `process_vm_readv(2)`: one syscall, no `PTRACE_ATTACH`, and the target is
//! never stopped. Modules and regions both come from `/proc/<pid>/maps`, parsed by
//! [`crate::procmaps`].
//!
//! # Proton
//!
//! Under Proton the game is the ordinary Windows build: Wine maps the real PE honouring
//! `SectionAlignment`, so `client.dll` appears in `maps` and every signature, offset and
//! layout constant transfers unchanged.
//!
//! Three things to know:
//!
//! * A Proton prefix runs several wine processes (`steam.exe`, `services.exe`,
//!   `explorer.exe`). [`crate::linux::LinuxProcess::find_pid`] matches on
//!   `/proc/<pid>/cmdline` first and falls back to `comm` (truncated to 15 bytes by the
//!   kernel) only when that does not name the game.
//! * pressure-vessel gives the game its own mount namespace, so paths in `maps` are
//!   container-relative. Module lookup is therefore by basename.
//! * Run the reader on the host, not inside the container, and PID-namespace
//!   differences stop mattering.
//!
//! # Permissions
//!
//! Yama's `ptrace_scope` is `1` on most distributions, which makes `process_vm_readv`
//! against a non-child fail with `EPERM`. [`crate::linux::ptrace_scope`] reads the
//! current setting so
//! the error can say what to do about it; see [`Error::PtraceDenied`].

use std::ffi::c_void;
use std::fs;

use crate::attach;
use crate::error::{Error, Result};
use crate::mem::{MemoryReader, Module, Region};
use crate::procmaps;

/// A Linux process opened for reading.
///
/// There is no handle to hold: `process_vm_readv` takes the pid directly, so this is
/// just a validated pid. That also means it is trivially `Send + Sync`.
#[derive(Debug, Clone)]
pub struct LinuxProcess {
    pid: u32,
}

/// Value of `/proc/sys/kernel/yama/ptrace_scope`, if the file exists.
///
/// * `0` - any process of the same uid may be read.
/// * `1` - only descendants (the common default; needs a workaround).
/// * `2` - admin only.
/// * `3` - no process may be attached to at all.
pub fn ptrace_scope() -> Option<u8> {
    fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Advice to print when a read is refused.
pub fn permission_hint() -> String {
    attach::permission_hint_for(ptrace_scope())
}

/// How many mapped regions [`LinuxProcess::open`] will probe before concluding that reads
/// are being refused rather than that it was unlucky.
const PROBE_ATTEMPTS: usize = 4;

impl LinuxProcess {
    /// Find the pid of the process running `exe_name`.
    ///
    /// Matches `argv[0]` (either path separator, since Wine paths use backslashes) and the
    /// kernel `comm`. A Proton launch leaves several processes whose command line carries
    /// the game's path, so the one with the client module mapped wins, and a bare launcher
    /// is never chosen. See [`attach::pick_pid`].
    pub fn find_pid(exe_name: &str) -> Option<u32> {
        let mut candidates = Vec::new();
        for entry in fs::read_dir("/proc").ok()? {
            let Ok(entry) = entry else { continue };
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let dir = entry.path();
            let cmdline = fs::read(dir.join("cmdline")).unwrap_or_default();
            let comm = fs::read_to_string(dir.join("comm")).ok();
            let Some(kind) = attach::match_cmdline(&cmdline, comm.as_deref(), exe_name) else {
                continue;
            };
            let has_client_module = fs::read(dir.join("maps"))
                .map(|m| attach::maps_mention_client(&String::from_utf8_lossy(&m)))
                .unwrap_or(false);
            candidates.push(attach::Candidate {
                pid,
                kind,
                has_client_module,
            });
        }
        attach::pick_pid(&candidates)
    }

    /// Attach by pid, verifying it is readable.
    ///
    /// Probes so permission problems surface here rather than deep inside a walk, where
    /// they look like fields that will not resolve.
    pub fn open(pid: u32) -> Result<Self> {
        let me = LinuxProcess { pid };
        let entries = me.maps().map_err(|e| match e {
            Error::ProcessGone { .. } => Error::ProcessNotFound(format!("pid {pid}")),
            other => other,
        })?;

        // Several regions, not one. A single zero-byte read is ambiguous: it means
        // `process_vm_readv` was refused, or it means that one region was unmapped between
        // reading `maps` and probing it, which a running game does constantly. Only every
        // attempt failing is evidence of a permissions problem, and printing the
        // "fix your ptrace_scope" hint at someone whose scope is fine costs them an
        // afternoon.
        let mut attempts = 0usize;
        for e in entries
            .iter()
            .filter(|e| e.readable && !e.is_unreadable_pseudo())
        {
            let mut probe = [0u8; 1];
            if me.read_partial(e.start, &mut probe) == 1 {
                return Ok(me);
            }
            attempts += 1;
            if attempts >= PROBE_ATTEMPTS {
                break;
            }
        }
        if attempts == 0 {
            // `maps` parsed but offered nothing ordinary to read. Handing back a reader
            // would defer the failure to the first real read and report it as a missing
            // field rather than as a process we cannot see into.
            return Err(Error::NoReadableMapping(pid));
        }
        Err(Error::PtraceDenied {
            pid,
            hint: permission_hint(),
        })
    }

    /// Find `exe_name` and open it.
    pub fn attach(exe_name: &str) -> Result<Self> {
        let pid =
            Self::find_pid(exe_name).ok_or_else(|| Error::ProcessNotFound(exe_name.to_string()))?;
        Self::open(pid)
    }

    fn maps(&self) -> Result<Vec<procmaps::MapEntry>> {
        let bytes = fs::read(format!("/proc/{}/maps", self.pid)).map_err(|e| {
            attach::linux_maps_error(self.pid, e.raw_os_error().unwrap_or(0), ptrace_scope())
        })?;
        Ok(procmaps::parse_maps_bytes(&bytes))
    }

    fn vm_readv(&self, addr: u64, buf: &mut [u8]) -> isize {
        let local = libc::iovec {
            iov_base: buf.as_mut_ptr().cast::<c_void>(),
            iov_len: buf.len(),
        };
        let remote = libc::iovec {
            iov_base: addr as *mut c_void,
            iov_len: buf.len(),
        };
        // SAFETY: both iovecs describe valid ranges - the local one is `buf`, and the
        // remote one is only ever read by the kernel, which validates it and returns
        // EFAULT rather than faulting us.
        unsafe { libc::process_vm_readv(self.pid as libc::pid_t, &local, 1, &remote, 1, 0) }
    }
}

impl MemoryReader for LinuxProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn read_into(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let n = self.vm_readv(addr, buf);
        if n < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(attach::linux_read_error(
                self.pid,
                addr,
                buf.len(),
                errno,
                ptrace_scope(),
            ));
        }
        if n as usize != buf.len() {
            return Err(Error::ShortRead {
                addr,
                want: buf.len(),
                got: n as usize,
            });
        }
        Ok(())
    }

    fn read_partial(&self, addr: u64, buf: &mut [u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        let n = self.vm_readv(addr, buf);
        if n < 0 { 0 } else { n as usize }
    }

    fn modules(&self) -> Result<Vec<Module>> {
        Ok(procmaps::modules_from_maps(&self.maps()?))
    }

    fn regions(&self) -> Result<Vec<Region>> {
        Ok(procmaps::regions_from_maps(&self.maps()?))
    }
}
