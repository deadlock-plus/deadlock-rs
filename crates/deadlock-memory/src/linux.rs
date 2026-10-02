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
//! layout constant transfers unchanged. [`crate::abi::Abi::detect`] confirms this from
//! the `MZ` magic rather than assuming it.
//!
//! Three things to know:
//!
//! * A Proton prefix runs several wine processes (`steam.exe`, `services.exe`,
//!   `explorer.exe`). [`crate::linux::LinuxProcess::find_pid`] matches on
//!   `/proc/<pid>/cmdline`, not
//!   `comm`, which is truncated to 15 bytes.
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
    let scope = ptrace_scope();
    let scope_line = match scope {
        Some(0) => "ptrace_scope is 0, so this is probably a uid mismatch: the game runs as another user.".to_string(),
        Some(n) => format!("/proc/sys/kernel/yama/ptrace_scope is {n} (only descendants may be read)."),
        None => "Yama does not appear to be enabled; this may be a uid mismatch or a container boundary.".to_string(),
    };
    format!(
        "{scope_line}\n  Fix one of:\n    \
         sudo setcap cap_sys_ptrace+ep <this binary>   # per-binary, preferred\n    \
         sudo sysctl -w kernel.yama.ptrace_scope=0     # session-wide, resets on reboot"
    )
}

/// How many mapped regions [`LinuxProcess::open`] will probe before concluding that reads
/// are being refused rather than that it was unlucky.
const PROBE_ATTEMPTS: usize = 4;

impl LinuxProcess {
    /// Find a pid whose `cmdline` names `exe_name`.
    ///
    /// Matches the basename of `argv[0]` so it works for both a native `deadlock` and a
    /// Proton-hosted `.../drive_c/.../deadlock.exe`. Wine paths use backslashes, so both
    /// separators are honoured.
    pub fn find_pid(exe_name: &str) -> Option<u32> {
        let mut best = None;
        for entry in fs::read_dir("/proc").ok()? {
            let Ok(entry) = entry else { continue };
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            // cmdline is NUL-separated; argv[0] is what we want.
            let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or_default();
            let Ok(argv0) = std::str::from_utf8(argv0) else {
                continue;
            };
            let base = argv0
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(argv0)
                .trim_end_matches('\u{0}');
            if base.eq_ignore_ascii_case(exe_name) {
                // Prefer a process that actually has the client module mapped; a Proton
                // prefix can briefly show more than one match.
                let looks_real = fs::read_to_string(entry.path().join("maps"))
                    .map(|m| m.contains("client.dll") || m.contains("client.so"))
                    .unwrap_or(false);
                if looks_real {
                    return Some(pid);
                }
                best.get_or_insert(pid);
            }
        }
        best
    }

    /// Attach by pid, verifying it is readable.
    ///
    /// Probes so permission problems surface here rather than deep inside a walk, where
    /// they look like fields that will not resolve.
    pub fn open(pid: u32) -> Result<Self> {
        let me = LinuxProcess { pid };
        let maps = fs::read_to_string(format!("/proc/{pid}/maps"))
            .map_err(|_| Error::ProcessNotFound(format!("pid {pid}")))?;
        let entries = procmaps::parse_maps(&maps);

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
        let text = fs::read_to_string(format!("/proc/{}/maps", self.pid))
            .map_err(|_| Error::ProcessNotFound(format!("pid {}", self.pid)))?;
        Ok(procmaps::parse_maps(&text))
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
            let err = std::io::Error::last_os_error();
            let os = err.raw_os_error().unwrap_or(0) as u32;
            if err.raw_os_error() == Some(libc::EPERM) {
                return Err(Error::PtraceDenied {
                    pid: self.pid,
                    hint: permission_hint(),
                });
            }
            return Err(Error::ReadMemory {
                addr,
                len: buf.len(),
                os,
            });
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
