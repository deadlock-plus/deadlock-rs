//! macOS [`MemoryReader`] backend. Read-only by construction.
//!
//! Best effort and unverified on real hardware: it compiles for `aarch64-apple-darwin`
//! and `x86_64-apple-darwin`, and its decisions live in the pure [`crate::attach`] module,
//! which is tested everywhere.
//!
//! The Mach calls made are `task_for_pid`, `mach_vm_read_overwrite` and `mach_vm_region`,
//! plus `proc_listpids`, `proc_pidpath` and `proc_regionfilename` from libproc and the
//! `KERN_PROCARGS2` sysctl for discovery. There is no write path.
//!
//! # Wine on macOS
//!
//! Deadlock has no macOS build, so the target is a Windows process under a Wine wrapper
//! app. Its image is a `wine64-preloader` host with the game's path as an argument, and
//! `client.dll` is a file-backed mapping that `proc_regionfilename` resolves, so modules
//! are rebuilt from region paths the same way [`crate::procmaps`] does on Linux.
//!
//! # Permissions
//!
//! `task_for_pid` is refused for any non-root caller unless the target is debuggable. That
//! surfaces as [`Error::PtraceDenied`] with a hint to run under `sudo`.

use std::ffi::{c_int, c_void};
use std::ptr;

use crate::attach;
use crate::error::{Error, Result};
use crate::mem::{MemoryReader, Module, Region};
use crate::procmaps::{self, MapEntry};

type KernReturn = i32;
type MachPort = u32;

const KERN_SUCCESS: KernReturn = 0;
const VM_REGION_BASIC_INFO_64: c_int = 9;
const VM_REGION_BASIC_INFO_COUNT_64: u32 = 9;
const VM_PROT_READ: i32 = 1;
const VM_PROT_WRITE: i32 = 2;
const VM_PROT_EXECUTE: i32 = 4;
const PROC_ALL_PIDS: u32 = 1;
const CTL_KERN: c_int = 1;
const KERN_PROCARGS2: c_int = 49;
const MAXPATHLEN: usize = 1024;
const PAGE: u64 = 4096;

/// Upper bound on regions walked, so a misbehaving target cannot spin the loop forever.
const MAX_REGIONS: usize = 1 << 20;

// SAFETY: these are the documented signatures from <mach/mach_vm.h>, <libproc.h> and
// <sys/sysctl.h>, all in libSystem, which every macOS binary links.
unsafe extern "C" {
    static mach_task_self_: MachPort;
    fn task_for_pid(target: MachPort, pid: c_int, task: *mut MachPort) -> KernReturn;
    fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
    fn mach_vm_read_overwrite(
        task: MachPort,
        address: u64,
        size: u64,
        data: u64,
        out_size: *mut u64,
    ) -> KernReturn;
    fn mach_vm_region(
        task: MachPort,
        address: *mut u64,
        size: *mut u64,
        flavor: c_int,
        info: *mut c_int,
        info_count: *mut u32,
        object_name: *mut MachPort,
    ) -> KernReturn;
    fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut c_void, size: c_int) -> c_int;
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
    fn proc_regionfilename(pid: c_int, address: u64, buffer: *mut c_void, size: u32) -> c_int;
    fn sysctl(
        name: *mut c_int,
        namelen: u32,
        old: *mut c_void,
        oldlen: *mut usize,
        new: *mut c_void,
        newlen: usize,
    ) -> c_int;
}

/// A macOS process opened for reading through its Mach task port.
#[derive(Debug)]
pub struct MacProcess {
    pid: u32,
    task: MachPort,
}

impl Drop for MacProcess {
    fn drop(&mut self) {
        // SAFETY: `task` is a send right this type obtained from `task_for_pid` and owns
        // exclusively (the type is neither `Copy` nor `Clone`), released once, here.
        unsafe {
            mach_port_deallocate(mach_task_self_, self.task);
        }
    }
}

fn list_pids() -> Vec<c_int> {
    // SAFETY: a null buffer with size 0 asks only for the byte count needed.
    let needed = unsafe { proc_listpids(PROC_ALL_PIDS, 0, ptr::null_mut(), 0) };
    let Ok(needed) = usize::try_from(needed) else {
        return Vec::new();
    };
    // Headroom: processes can start between the two calls.
    let mut pids = vec![0 as c_int; needed / size_of::<c_int>() + 64];
    // SAFETY: `pids` is live and the byte length passed is its true size.
    let got = unsafe {
        proc_listpids(
            PROC_ALL_PIDS,
            0,
            pids.as_mut_ptr().cast(),
            (pids.len() * size_of::<c_int>()) as c_int,
        )
    };
    let Ok(got) = usize::try_from(got) else {
        return Vec::new();
    };
    pids.truncate(got / size_of::<c_int>());
    pids.retain(|&p| p > 0);
    pids
}

fn proc_args(pid: c_int) -> Option<Vec<u8>> {
    let mut mib = [CTL_KERN, KERN_PROCARGS2, pid];
    let mut len = 0usize;
    // SAFETY: `mib` is a live 3-element array, and a null `old` with a live `len` asks only
    // for the required size.
    let rc = unsafe {
        sysctl(
            mib.as_mut_ptr(),
            3,
            ptr::null_mut(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` is live and `len` is its true size; the kernel writes at most that
    // many bytes and updates `len` to the count written.
    let rc = unsafe {
        sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);
    Some(buf)
}

fn pid_path(pid: c_int) -> Option<String> {
    let mut buf = [0u8; 4 * MAXPATHLEN];
    // SAFETY: `buf` is live and the size passed is its true length.
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

impl MacProcess {
    /// Find the pid of the process running `exe_name`, including a Wine host carrying it
    /// as an argument.
    pub fn find_pid(exe_name: &str) -> Option<u32> {
        let mut candidates = Vec::new();
        for pid in list_pids() {
            let parsed = proc_args(pid).and_then(|b| attach::parse_procargs2(&b));
            let kind = match &parsed {
                Some((exec, args)) => {
                    attach::match_mac_process(exec, args.iter().map(String::as_str), exe_name)
                }
                None => pid_path(pid)
                    .and_then(|p| attach::match_args(Some(&p), std::iter::empty(), exe_name)),
            };
            if let Some(kind) = kind {
                candidates.push(attach::Candidate {
                    pid: pid as u32,
                    kind,
                    has_client_module: false,
                });
            }
        }
        attach::pick_pid(&candidates)
    }

    /// Acquire the task port for `pid`.
    pub fn open(pid: u32) -> Result<Self> {
        let mut task: MachPort = 0;
        // SAFETY: `task` is a live local the call writes a port name into; `pid` is a plain
        // integer. The call grants a right this type then owns.
        let kr = unsafe { task_for_pid(mach_task_self_, pid as c_int, &mut task) };
        if kr != KERN_SUCCESS {
            return Err(attach::mac_task_error(pid, kr));
        }
        Ok(MacProcess { pid, task })
    }

    /// Find `exe_name` and open it.
    pub fn attach(exe_name: &str) -> Result<Self> {
        let pid =
            Self::find_pid(exe_name).ok_or_else(|| Error::ProcessNotFound(exe_name.to_string()))?;
        Self::open(pid)
    }

    fn region_info(&self, addr: u64) -> Option<(u64, u64, [c_int; 9])> {
        let mut start = addr;
        let mut size = 0u64;
        let mut info = [0 as c_int; 9];
        let mut count = VM_REGION_BASIC_INFO_COUNT_64;
        let mut object: MachPort = 0;
        // SAFETY: every out-pointer is a live local of the type the call documents, and
        // `info` holds the nine words `VM_REGION_BASIC_INFO_COUNT_64` promises. `addr` is
        // only queried in the target, never dereferenced here.
        let kr = unsafe {
            mach_vm_region(
                self.task,
                &mut start,
                &mut size,
                VM_REGION_BASIC_INFO_64,
                info.as_mut_ptr(),
                &mut count,
                &mut object,
            )
        };
        (kr == KERN_SUCCESS && size > 0).then_some((start, size, info))
    }

    fn region_path(&self, addr: u64) -> Option<String> {
        let mut buf = [0u8; MAXPATHLEN];
        // SAFETY: `buf` is live and the size passed is its true length.
        let n = unsafe {
            proc_regionfilename(
                self.pid as c_int,
                addr,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
            )
        };
        let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    fn entry(&self, start: u64, size: u64, info: &[c_int; 9], with_path: bool) -> MapEntry {
        let path = if with_path {
            self.region_path(start)
        } else {
            None
        };
        MapEntry {
            start,
            end: start.saturating_add(size),
            readable: info[0] & VM_PROT_READ != 0,
            writable: info[0] & VM_PROT_WRITE != 0,
            executable: info[0] & VM_PROT_EXECUTE != 0,
            private: info[3] == 0,
            offset: 0,
            inode: u64::from(path.is_some()),
            path,
        }
    }

    fn entries(&self, with_path: bool) -> Vec<MapEntry> {
        let mut out = Vec::new();
        let mut addr = 0u64;
        while out.len() < MAX_REGIONS {
            let Some((start, size, info)) = self.region_info(addr) else {
                break;
            };
            out.push(self.entry(start, size, &info, with_path));
            let next = start.saturating_add(size);
            if next <= addr {
                break;
            }
            addr = next;
        }
        out
    }

    fn vm_read(&self, addr: u64, buf: &mut [u8]) -> std::result::Result<(), KernReturn> {
        let mut out = 0u64;
        // SAFETY: `buf` is a live, exclusively borrowed slice and its true length is
        // passed, so the kernel writes only inside it. `addr` is in the target and is
        // validated by the kernel, which fails the call rather than faulting.
        let kr = unsafe {
            mach_vm_read_overwrite(
                self.task,
                addr,
                buf.len() as u64,
                buf.as_mut_ptr() as u64,
                &mut out,
            )
        };
        if kr == KERN_SUCCESS && out == buf.len() as u64 {
            Ok(())
        } else if kr == KERN_SUCCESS {
            Err(-1)
        } else {
            Err(kr)
        }
    }
}

impl MemoryReader for MacProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn read_into(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.vm_read(addr, buf)
            .map_err(|kr| attach::mac_read_error(self.pid, addr, buf.len(), kr))
    }

    /// `mach_vm_read_overwrite` fails the whole call if any page is unreadable, so a failed
    /// range is retried page by page to keep the bytes before the bad page.
    fn read_partial(&self, addr: u64, buf: &mut [u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        if self.vm_read(addr, buf).is_ok() {
            return buf.len();
        }
        let mut done = 0usize;
        while done < buf.len() {
            let here = addr.wrapping_add(done as u64);
            let to_boundary = (PAGE - here % PAGE) as usize;
            let step = to_boundary.min(buf.len() - done);
            if self.vm_read(here, &mut buf[done..done + step]).is_err() {
                break;
            }
            done += step;
        }
        done
    }

    fn modules(&self) -> Result<Vec<Module>> {
        let entries = self.entries(true);
        if entries.is_empty() {
            return Err(Error::ProcessGone {
                pid: self.pid,
                os: 0,
            });
        }
        Ok(procmaps::modules_from_maps(&entries))
    }

    fn regions(&self) -> Result<Vec<Region>> {
        Ok(procmaps::regions_from_maps(&self.entries(false)))
    }

    fn region_at(&self, addr: u64) -> Option<Region> {
        let (start, size, info) = self.region_info(addr)?;
        if addr < start || addr - start >= size {
            return None;
        }
        procmaps::regions_from_maps(&[self.entry(start, size, &info, false)]).pop()
    }
}
