//! Platform-independent half of attaching to a process: deciding which process is the
//! target, and turning raw OS error codes into this crate's errors.
//!
//! Pure on purpose. The Linux and macOS backends only gather inputs (a `cmdline`, a
//! `/proc/<pid>/maps` text, an errno, a Mach return code) and hand them here, so every
//! decision that can be wrong is testable from any host without a Wine prefix or a Mac.

use crate::error::Error;

/// How a process matched the wanted image name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameMatch {
    /// The name appears only as a later argument: a launcher (`reaper`, `steam.exe`, a
    /// pressure-vessel wrapper) that merely carries the game's path on its command line.
    Wrapper,
    /// The process image itself is the wanted program: `argv[0]`, the executable path, or
    /// the kernel's `comm` name.
    Image,
}

/// A running process that matched by name, plus what was learned about it cheaply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Process id.
    pub pid: u32,
    /// How the name matched.
    pub kind: NameMatch,
    /// Whether the process has the game's client module mapped.
    pub has_client_module: bool,
}

/// Longest process name the Linux kernel keeps in `comm`.
const COMM_MAX: usize = 15;

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Whether one command-line token names `exe`.
///
/// Wine rewrites `/proc/<pid>/cmdline` so a Windows path can arrive as one token with
/// spaces in it (`Z:\My Games\deadlock.exe -novid`), so each space-separated piece is
/// tried as well as the whole token.
fn token_names(token: &str, exe: &str) -> bool {
    let token = token.trim_matches('"');
    if basename(token).eq_ignore_ascii_case(exe) {
        return true;
    }
    token
        .split(' ')
        .any(|piece| basename(piece.trim_matches('"')).eq_ignore_ascii_case(exe))
}

/// Match a process by its argument vector.
///
/// `exec_path` is the executable path when the platform reports one separately from
/// `argv[0]` (macOS does); it counts as the image.
pub fn match_args<'a>(
    exec_path: Option<&str>,
    args: impl IntoIterator<Item = &'a str>,
    exe: &str,
) -> Option<NameMatch> {
    if exec_path.is_some_and(|p| basename(p).eq_ignore_ascii_case(exe)) {
        return Some(NameMatch::Image);
    }
    let mut result = None;
    for (i, arg) in args.into_iter().enumerate() {
        if arg.is_empty() || !token_names(arg, exe) {
            continue;
        }
        if i == 0 {
            return Some(NameMatch::Image);
        }
        result = Some(NameMatch::Wrapper);
    }
    result
}

/// Match a Linux process from its raw `/proc/<pid>/cmdline` and `comm`.
///
/// `comm` is truncated to 15 bytes by the kernel, so a longer wanted name is compared
/// against its own 15-byte prefix. It rescues processes whose `cmdline` is empty or
/// rewritten past recognition.
pub fn match_cmdline(cmdline: &[u8], comm: Option<&str>, exe: &str) -> Option<NameMatch> {
    let text = String::from_utf8_lossy(cmdline);
    if let Some(found) = match_args(None, text.split('\0'), exe) {
        return Some(found);
    }
    let comm = comm?.trim_end_matches('\n');
    let want = exe.as_bytes();
    let want = &want[..want.len().min(COMM_MAX)];
    (!comm.is_empty() && comm.as_bytes().eq_ignore_ascii_case(want)).then_some(NameMatch::Image)
}

/// Match a macOS process.
///
/// A Windows program run through Wine on macOS is a `wine64-preloader` (or
/// similar) host process with the Windows path as an argument, so the host counts as the
/// image: there is no `client.dll` check to fall back on without a task port.
pub fn match_mac_process<'a>(
    exec_path: &str,
    args: impl IntoIterator<Item = &'a str>,
    exe: &str,
) -> Option<NameMatch> {
    let found = match_args(Some(exec_path), args, exe)?;
    let host_is_wine = basename(exec_path).to_ascii_lowercase().starts_with("wine");
    Some(if host_is_wine {
        NameMatch::Image
    } else {
        found
    })
}

/// Whether a `/proc/<pid>/maps` text mentions the game's client module.
///
/// Matches the Windows image under Proton and the native shared object, which is
/// `libclient.so`.
pub fn maps_mention_client(maps: &str) -> bool {
    crate::procmaps::parse_maps(maps)
        .iter()
        .filter(|entry| entry.is_file_backed())
        .any(|entry| {
            entry.file_name().is_some_and(|name| {
                ["client.dll", "client.so", "libclient.so"]
                    .iter()
                    .any(|client| name.eq_ignore_ascii_case(client))
            })
        })
}

/// Choose the process to attach to.
///
/// A process with the client module mapped wins outright: a Proton launch leaves several
/// processes carrying the game's name, and only one has the game in it. Failing that, the
/// process whose own image is the wanted name. A bare wrapper is never chosen, since
/// attaching to it would read a launcher. Ties go to the lowest pid, so the answer does not
/// depend on directory order.
pub fn pick_pid(candidates: &[Candidate]) -> Option<u32> {
    let lowest = |keep: &dyn Fn(&Candidate) -> bool| {
        candidates.iter().filter(|c| keep(c)).map(|c| c.pid).min()
    };
    lowest(&|c| c.has_client_module).or_else(|| lowest(&|c| c.kind == NameMatch::Image))
}

/// Split a macOS `KERN_PROCARGS2` buffer into the executable path and `argv`.
///
/// Layout: `argc` as a native `i32`, the executable path, NUL padding, then `argc`
/// NUL-terminated arguments, then the environment (ignored).
pub fn parse_procargs2(buf: &[u8]) -> Option<(String, Vec<String>)> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?;
    let mut rest = buf.get(4..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    let exec = String::from_utf8_lossy(&rest[..end]).into_owned();
    rest = &rest[end..];
    let skip = rest.iter().position(|&b| b != 0).unwrap_or(rest.len());
    rest = &rest[skip..];

    let mut args = Vec::new();
    for _ in 0..argc {
        let end = rest.iter().position(|&b| b == 0)?;
        args.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = &rest[end + 1..];
    }
    Some((exec, args))
}

const LINUX_EPERM: i32 = 1;
const LINUX_ENOENT: i32 = 2;
const LINUX_ESRCH: i32 = 3;
const LINUX_EACCES: i32 = 13;

/// What to tell a user whose Linux read was refused, given the Yama `ptrace_scope` value.
pub fn permission_hint_for(scope: Option<u8>) -> String {
    const SETCAP: &str = "sudo setcap cap_sys_ptrace+ep <this binary>   # per-binary, preferred";
    const SYSCTL: &str =
        "sudo sysctl -w kernel.yama.ptrace_scope=0     # session-wide, resets on reboot";
    match scope {
        Some(0) => "ptrace_scope is 0, so this is probably a uid mismatch: the game runs as \
                    another user. Run the reader as that user."
            .to_string(),
        Some(3) => "/proc/sys/kernel/yama/ptrace_scope is 3: process inspection is disabled \
                    until the next reboot and nothing can lower it. Boot with \
                    kernel.yama.ptrace_scope set to 0 or 1 to read the game."
            .to_string(),
        Some(2) => format!(
            "/proc/sys/kernel/yama/ptrace_scope is 2 (only admins may read other processes).\n  \
             Fix one of:\n    {SETCAP}\n    {SYSCTL}"
        ),
        Some(n) => format!(
            "/proc/sys/kernel/yama/ptrace_scope is {n} (only descendants may be read).\n  \
             Fix one of:\n    {SETCAP}\n    {SYSCTL}"
        ),
        None => "Yama does not appear to be enabled; this may be a uid mismatch, a container \
                 boundary, or a seccomp filter (a sandboxed Steam such as the Flatpak blocks \
                 process_vm_readv)."
            .to_string(),
    }
}

/// Map the errno from a failed `process_vm_readv` to an error.
pub fn linux_read_error(pid: u32, addr: u64, len: usize, errno: i32, scope: Option<u8>) -> Error {
    match errno {
        LINUX_EPERM | LINUX_EACCES => Error::PtraceDenied {
            pid,
            hint: permission_hint_for(scope),
        },
        LINUX_ESRCH => Error::ProcessGone {
            pid,
            os: errno as u32,
        },
        _ => Error::ReadMemory {
            addr,
            len,
            os: errno as u32,
        },
    }
}

/// Map the errno from failing to read `/proc/<pid>/maps` to an error.
///
/// `EACCES` here is the same ptrace access check that gates reading memory, so it gets
/// the same advice rather than being reported as a missing process.
pub fn linux_maps_error(pid: u32, errno: i32, scope: Option<u8>) -> Error {
    match errno {
        LINUX_EPERM | LINUX_EACCES => Error::PtraceDenied {
            pid,
            hint: permission_hint_for(scope),
        },
        LINUX_ENOENT | LINUX_ESRCH => Error::ProcessGone {
            pid,
            os: errno as u32,
        },
        _ => Error::ProcessNotFound(format!("pid {pid}")),
    }
}

/// What to tell a user whose macOS `task_for_pid` was refused.
pub fn mac_permission_hint() -> String {
    "macOS only hands out another process's task port to root, or to a binary signed with \
     the debugger entitlement.\n  Run the reader with sudo. A target under the Hardened \
     Runtime, or protected by SIP, cannot be read at all; Wine builds shipped by \
     CrossOver and Whisky are normally not."
        .to_string()
}

const KERN_INVALID_ARGUMENT: i32 = 4;
const MACH_SEND_INVALID_DEST: i32 = 0x1000_0003;
const KERN_FAILURE: i32 = 5;

/// Map a Mach `kern_return_t` from `task_for_pid` to an error.
///
/// `KERN_FAILURE` is what a refused request returns; `KERN_INVALID_ARGUMENT` is a pid that
/// no longer exists.
pub fn mac_task_error(pid: u32, kr: i32) -> Error {
    match kr {
        KERN_FAILURE => Error::PtraceDenied {
            pid,
            hint: mac_permission_hint(),
        },
        KERN_INVALID_ARGUMENT => Error::ProcessGone { pid, os: kr as u32 },
        _ => Error::OpenProcess { pid, os: kr as u32 },
    }
}

/// Map the `kern_return_t` from a failed `mach_vm_read_overwrite` to an error.
pub fn mac_read_error(pid: u32, addr: u64, len: usize, kr: i32) -> Error {
    match kr {
        KERN_INVALID_ARGUMENT | MACH_SEND_INVALID_DEST => Error::ProcessGone { pid, os: kr as u32 },
        _ => Error::ReadMemory {
            addr,
            len,
            os: kr as u32,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "deadlock.exe";

    fn cmd(parts: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        for p in parts {
            v.extend_from_slice(p.as_bytes());
            v.push(0);
        }
        v
    }

    fn cand(pid: u32, kind: NameMatch, has_client_module: bool) -> Candidate {
        Candidate {
            pid,
            kind,
            has_client_module,
        }
    }

    #[test]
    fn a_wine_argv0_with_backslashes_is_the_image() {
        let c = cmd(&[
            "Z:\\home\\u\\.steam\\Deadlock\\game\\bin\\win64\\deadlock.exe",
            "-novid",
        ]);
        assert_eq!(match_cmdline(&c, None, EXE), Some(NameMatch::Image));
    }

    #[test]
    fn a_native_path_is_the_image_case_insensitively() {
        let c = cmd(&["/games/Deadlock/DEADLOCK.EXE"]);
        assert_eq!(match_cmdline(&c, None, EXE), Some(NameMatch::Image));
    }

    #[test]
    fn a_launcher_carrying_the_path_is_only_a_wrapper() {
        let c = cmd(&[
            "reaper",
            "SteamLaunch",
            "--",
            "proton",
            "waitforexitandrun",
            "/g/deadlock.exe",
        ]);
        assert_eq!(match_cmdline(&c, None, EXE), Some(NameMatch::Wrapper));
    }

    #[test]
    fn a_windows_path_with_spaces_in_one_token_still_matches() {
        let c = cmd(&["Z:\\home\\u\\My Games\\Deadlock\\deadlock.exe -novid"]);
        assert_eq!(match_cmdline(&c, None, EXE), Some(NameMatch::Image));
    }

    #[test]
    fn quoted_paths_match() {
        let c = cmd(&["\"C:\\Games\\deadlock.exe\""]);
        assert_eq!(match_cmdline(&c, None, EXE), Some(NameMatch::Image));
    }

    #[test]
    fn a_longer_name_does_not_match_a_shorter_one() {
        assert_eq!(
            match_cmdline(&cmd(&["/games/notdeadlock.exe"]), None, EXE),
            None
        );
        assert_eq!(
            match_cmdline(&cmd(&["/games/deadlock.exe.bak"]), None, EXE),
            None
        );
    }

    #[test]
    fn an_empty_cmdline_falls_back_to_comm() {
        assert_eq!(
            match_cmdline(b"", Some("deadlock.exe\n"), EXE),
            Some(NameMatch::Image)
        );
        assert_eq!(match_cmdline(b"", Some("bash\n"), EXE), None);
        assert_eq!(match_cmdline(b"", Some("\n"), EXE), None);
    }

    #[test]
    fn comm_is_compared_at_its_kernel_truncation() {
        let long = "deadlock-client-debug.exe";
        assert_eq!(match_cmdline(b"", Some("deadlock-clien\n"), long), None);
        assert_eq!(
            match_cmdline(b"", Some("deadlock-client\n"), long),
            Some(NameMatch::Image)
        );
    }

    #[test]
    fn non_utf8_cmdline_does_not_panic_or_hide_a_match() {
        let mut c = vec![0xff, 0xfe, 0];
        c.extend_from_slice(b"/g/deadlock.exe\0");
        assert_eq!(match_cmdline(&c, None, EXE), Some(NameMatch::Wrapper));
    }

    #[test]
    fn macos_exec_path_counts_as_the_image() {
        assert_eq!(
            match_args(
                Some("/Applications/Whisky/wine64-preloader"),
                ["wine64-preloader", "C:\\g\\deadlock.exe"],
                EXE
            ),
            Some(NameMatch::Wrapper)
        );
        assert_eq!(
            match_args(Some("/opt/deadlock.exe"), ["x"], EXE),
            Some(NameMatch::Image)
        );
    }

    #[test]
    fn a_wine_host_on_macos_is_the_image_but_other_hosts_are_not() {
        let args = ["wine64-preloader", r"C:\g\deadlock.exe"];
        assert_eq!(
            match_mac_process("/Users/u/Library/Whisky/wine64-preloader", args, EXE),
            Some(NameMatch::Image)
        );
        assert_eq!(
            match_mac_process("/usr/bin/open", ["open", "/g/deadlock.exe"], EXE),
            Some(NameMatch::Wrapper)
        );
        assert_eq!(match_mac_process("/bin/ls", ["ls"], EXE), None);
    }

    #[test]
    fn a_failed_mach_read_distinguishes_a_bad_address_from_a_dead_task() {
        assert_eq!(
            mac_read_error(3, 0x10, 4, 1),
            Error::ReadMemory {
                addr: 0x10,
                len: 4,
                os: 1
            }
        );
        assert_eq!(
            mac_read_error(3, 0x10, 4, 0x1000_0003),
            Error::ProcessGone {
                pid: 3,
                os: 0x1000_0003
            }
        );
    }

    #[test]
    fn the_process_with_the_client_module_wins_over_a_lower_pid() {
        let c = [
            cand(10, NameMatch::Image, false),
            cand(20, NameMatch::Wrapper, false),
            cand(30, NameMatch::Image, true),
        ];
        assert_eq!(pick_pid(&c), Some(30));
    }

    #[test]
    fn without_a_client_module_an_image_beats_a_wrapper() {
        let c = [
            cand(5, NameMatch::Wrapper, false),
            cand(9, NameMatch::Image, false),
        ];
        assert_eq!(pick_pid(&c), Some(9));
    }

    #[test]
    fn a_lone_wrapper_is_never_attached_to() {
        assert_eq!(pick_pid(&[cand(5, NameMatch::Wrapper, false)]), None);
        assert_eq!(pick_pid(&[]), None);
    }

    #[test]
    fn ties_go_to_the_lowest_pid() {
        let c = [
            cand(9, NameMatch::Image, true),
            cand(4, NameMatch::Image, true),
        ];
        assert_eq!(pick_pid(&c), Some(4));
    }

    #[test]
    fn client_module_is_recognised_for_both_builds() {
        assert!(maps_mention_client(
            "1-2 r--p 0 0:0 1 /x/win64/client.dll\n"
        ));
        assert!(maps_mention_client(
            "1-2 r--p 0 0:0 1 /x/linuxsteamrt64/libclient.so\n"
        ));
        assert!(!maps_mention_client(
            "1-2 r--p 0 0:0 1 /x/win64/engine2.dll\n"
        ));
    }

    #[test]
    fn steam_and_other_client_libraries_do_not_identify_the_game() {
        for path in [
            "/steam/linux64/steamclient.so",
            "/proton/lib/wine/x86_64-unix/lsteamclient.so",
            "/proton/lib/wine/x86_64-windows/lsteamclient.dll",
            "/game/bin/win64/panoramauiclient.dll",
            "/usr/lib/libwayland-client.so.0.26.0",
        ] {
            let maps = format!("1-2 r--p 0 0:0 1 {path}\n");
            assert!(!maps_mention_client(&maps), "{path}");
        }
    }

    #[test]
    fn proton_game_beats_a_steam_launcher_with_steamclient_mapped() {
        let launcher = b"c:\\windows\\system32\\steam.exe\0S:\\common\\Deadlock\\game\\bin\\win64\\deadlock.exe\0";
        let game = b"S:\\common\\Deadlock\\game\\bin\\win64\\deadlock.exe\0";
        let launcher_maps =
            "7f9e15200000-7f9e17e81000 r-xp 00000000 08:01 100 /steam/linux64/steamclient.so\n";
        let game_maps =
            "180000000-184000000 r-xp 00000000 08:01 200 /game/citadel/bin/win64/client.dll\n";
        let candidates = [
            cand(
                10,
                match_cmdline(launcher, Some("steam.exe\n"), EXE).unwrap(),
                maps_mention_client(launcher_maps),
            ),
            cand(
                20,
                match_cmdline(game, Some("MainThrd\n"), EXE).unwrap(),
                maps_mention_client(game_maps),
            ),
        ];
        assert_eq!(pick_pid(&candidates), Some(20));
    }

    fn procargs(argc: i32, exec: &str, pad: usize, args: &[&str]) -> Vec<u8> {
        let mut v = argc.to_ne_bytes().to_vec();
        v.extend_from_slice(exec.as_bytes());
        v.extend(std::iter::repeat_n(0u8, pad));
        for a in args {
            v.extend_from_slice(a.as_bytes());
            v.push(0);
        }
        v.extend_from_slice(b"HOME=/Users/u\0");
        v
    }

    #[test]
    fn procargs2_splits_path_args_and_skips_the_environment() {
        let b = procargs(
            2,
            "/opt/wine64-preloader",
            3,
            &["wine64-preloader", "C:\\deadlock.exe"],
        );
        let (exec, args) = parse_procargs2(&b).unwrap();
        assert_eq!(exec, "/opt/wine64-preloader");
        assert_eq!(args, ["wine64-preloader", "C:\\deadlock.exe"]);
    }

    #[test]
    fn procargs2_rejects_truncated_input() {
        assert!(parse_procargs2(&[1, 0]).is_none());
        assert!(parse_procargs2(&procargs(3, "/x", 1, &["only-one"])[..12]).is_none());
        assert!(parse_procargs2(&(-1i32).to_ne_bytes()).is_none());
    }

    #[test]
    fn eperm_on_read_is_a_permission_error_with_scope_advice() {
        match linux_read_error(7, 0x1000, 8, 1, Some(1)) {
            Error::PtraceDenied { pid, hint } => {
                assert_eq!(pid, 7);
                assert!(hint.contains("ptrace_scope is 1"), "{hint}");
                assert!(hint.contains("setcap"), "{hint}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn esrch_on_read_means_the_process_exited() {
        assert_eq!(
            linux_read_error(7, 0, 1, 3, None),
            Error::ProcessGone { pid: 7, os: 3 }
        );
    }

    #[test]
    fn efault_on_read_stays_a_plain_read_failure() {
        assert_eq!(
            linux_read_error(7, 0x40, 8, 14, Some(1)),
            Error::ReadMemory {
                addr: 0x40,
                len: 8,
                os: 14
            }
        );
    }

    #[test]
    fn eacces_on_maps_is_a_permission_error_not_a_missing_process() {
        assert!(matches!(
            linux_maps_error(7, 13, Some(1)),
            Error::PtraceDenied { .. }
        ));
        assert_eq!(
            linux_maps_error(7, 2, None),
            Error::ProcessGone { pid: 7, os: 2 }
        );
        assert_eq!(
            linux_maps_error(7, 5, None),
            Error::ProcessNotFound("pid 7".into())
        );
    }

    #[test]
    fn hints_differ_by_scope_and_never_offer_a_dead_end() {
        assert!(permission_hint_for(Some(0)).contains("uid"));
        let three = permission_hint_for(Some(3));
        assert!(
            three.contains("reboot") && !three.contains("sysctl -w"),
            "{three}"
        );
        assert!(permission_hint_for(Some(2)).contains("admins"));
        assert!(permission_hint_for(None).contains("Flatpak"));
    }

    #[test]
    fn a_refused_task_port_says_to_use_sudo() {
        match mac_task_error(9, 5) {
            Error::PtraceDenied { hint, .. } => assert!(hint.contains("sudo"), "{hint}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(mac_task_error(9, 4), Error::ProcessGone { pid: 9, os: 4 });
        assert_eq!(mac_task_error(9, 46), Error::OpenProcess { pid: 9, os: 46 });
    }
}
