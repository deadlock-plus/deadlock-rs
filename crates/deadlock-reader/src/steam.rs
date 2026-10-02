//! Local Steam account lookup.
//!
//! The logged-in account id comes from the registry rather than from game memory
//! (`LAYOUT.md` §7.3). Steam mirrors the same tree on Linux in a text file, so both
//! platforms answer the same question from the same logical location:
//!
//! ```text
//! Windows  HKCU\Software\Valve\Steam\ActiveProcess\ActiveUser   (REG_DWORD)
//! Linux    ~/.steam/registry.vdf -> HKCU/Software/Valve/Steam/ActiveProcess/ActiveUser
//! ```

#[cfg(target_os = "linux")]
use crate::error::Error;
use crate::error::Result;

pub use deadlock_core::steam::STEAM64_BASE;

/// Convert a 32-bit account id to a Steam64 id.
pub fn to_steam64(account_id: u32) -> u64 {
    deadlock_core::AccountId(account_id).to_steam64().get()
}

/// Convert a Steam64 id back to a 32-bit account id.
///
/// Returns `None` for ids below the individual-universe base.
pub fn to_account_id(steam64: u64) -> Option<u32> {
    deadlock_core::SteamId(steam64)
        .to_account_id()
        .map(|a| a.get())
}

/// Pull `ActiveUser` out of a `registry.vdf`.
///
/// Split out from the file reading so it can be tested on any platform. VDF is nested
/// quoted key/value pairs; we only need one scalar, so a targeted scan avoids pulling
/// in a parser.
#[cfg(any(target_os = "linux", test))]
pub fn parse_active_user(vdf: &str) -> Option<u32> {
    let mut lines = vdf.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        if !t.starts_with("\"ActiveUser\"") {
            continue;
        }
        // "ActiveUser"		"123456"   - value is the second quoted field.
        let mut quoted = t.split('"').skip(1); // skip up to the key
        let _key = quoted.next()?;
        // Everything between key and value is whitespace/tabs.
        for field in quoted {
            let field = field.trim();
            if field.is_empty() {
                continue;
            }
            if let Ok(v) = field.parse::<u32>() {
                return Some(v);
            }
        }
        let _ = lines;
    }
    None
}

/// Read the currently logged-in Steam account id (32-bit).
///
/// Returns `Ok(None)` when Steam is installed but nobody is signed in (the value reads
/// back as zero), and an error only when the lookup itself fails.
#[cfg(windows)]
pub fn active_account_id() -> Result<Option<u32>> {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let subkey = wide(r"Software\Valve\Steam\ActiveProcess");
    let value = wide("ActiveUser");

    let mut data: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;

    // SAFETY: both name pointers come from `wide`, which NUL-terminates, and the `Vec`s
    // outlive the call. `data` is a live `u32` and `size` says exactly that; `RRF_RT_REG_DWORD`
    // makes the call fail rather than write anything if the value is some other type, so
    // the four bytes are the most that can land there.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut size,
        )
    };

    if rc != ERROR_SUCCESS {
        return Err(deadlock_memory::Error::Registry(rc).into());
    }
    Ok((data != 0).then_some(data))
}

/// Candidate locations for Steam's registry mirror, in preference order.
#[cfg(target_os = "linux")]
pub fn registry_paths() -> Vec<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let mut out = Vec::new();
    if let Some(h) = home {
        out.push(h.join(".steam/registry.vdf"));
        out.push(h.join(".steam/steam/registry.vdf"));
        out.push(h.join(".local/share/Steam/registry.vdf"));
        // Flatpak Steam keeps its own home.
        out.push(h.join(".var/app/com.valvesoftware.Steam/.steam/registry.vdf"));
    }
    out
}

/// Read the currently logged-in Steam account id (32-bit).
#[cfg(target_os = "linux")]
pub fn active_account_id() -> Result<Option<u32>> {
    // A path that is simply absent is the ordinary case: Steam has four possible homes and
    // at most one of them exists. A path that *is* there and cannot be read is not - that
    // is a permissions problem, and swallowing it made it identical to "nobody is signed
    // in", which is the answer that sends you looking somewhere else entirely.
    let mut blocked: Option<(std::path::PathBuf, String)> = None;
    for path in registry_paths() {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                blocked.get_or_insert((path, e.to_string()));
                continue;
            }
        };
        if let Some(id) = parse_active_user(&text) {
            return Ok((id != 0).then_some(id));
        }
    }
    // Only reported when no other candidate answered: one unreadable path among four is
    // not worth failing over if a later one works.
    match blocked {
        Some((path, source)) => Err(Error::SteamRegistryUnreadable { path, source }),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_id_roundtrip() {
        assert_eq!(to_steam64(1), 76_561_197_960_265_729);
        assert_eq!(to_account_id(76_561_197_960_265_729), Some(1));
        assert_eq!(to_account_id(0), None);
    }

    /// Shape of a real `~/.steam/registry.vdf`.
    #[test]
    fn finds_active_user_in_a_registry_vdf() {
        let vdf = "\
\"Registry\"
{
\t\"HKCU\"
\t{
\t\t\"Software\"
\t\t{
\t\t\t\"Valve\"
\t\t\t{
\t\t\t\t\"Steam\"
\t\t\t\t{
\t\t\t\t\t\"language\"\t\t\"english\"
\t\t\t\t\t\"ActiveUser\"\t\t\"387246372\"
\t\t\t\t\t\"AutoLoginUser\"\t\t\"someone\"
\t\t\t\t}
\t\t\t}
\t\t}
\t}
}
";
        assert_eq!(parse_active_user(vdf), Some(387_246_372));
    }

    #[test]
    fn absent_or_zero_active_user() {
        assert_eq!(parse_active_user("\"Registry\"\n{\n}\n"), None);
        assert_eq!(parse_active_user("\t\"ActiveUser\"\t\t\"0\"\n"), Some(0));
    }

    #[test]
    fn ignores_a_key_that_merely_contains_the_name() {
        let vdf = "\t\"LastActiveUserName\"\t\t\"bob\"\n\t\"ActiveUser\"\t\t\"42\"\n";
        assert_eq!(parse_active_user(vdf), Some(42));
    }
}
