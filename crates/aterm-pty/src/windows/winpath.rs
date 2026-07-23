// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Refresh the child shell's `PATH` from the LIVE registry at spawn (Windows).
//!
//! A GUI-launched terminal (Start Menu / Explorer) inherits whatever `PATH`
//! `explorer.exe` held at login. If the user installs a tool AFTER that — or an
//! installer edited `HKCU\Environment\Path` / the machine `Environment\Path` — the
//! new directory is on the *registry* PATH but NOT on Explorer's stale process PATH,
//! so a freshly-opened terminal cannot find the tool until the whole session is
//! restarted (or the machine rebooted). This is a real, common Windows papercut
//! (`docs/NATIVE_WINDOWS_DESIGN.md` §9: "read the broad user+system PATH from the
//! registry, not just the process env").
//!
//! [`refresh_child_path`] reads the authoritative Machine + User `Path` values
//! straight from the registry (auto-expanding `%VAR%` references), unions them with
//! whatever `PATH` was inherited (so a session-specific addition is never LOST), and
//! sets that as the child shell's `PATH`. Opt out with `ATERM_NO_PATH_REFRESH`.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        hkey: isize,
        sub_key: *const u16,
        value: *const u16,
        flags: u32,
        pdw_type: *mut u32,
        pv_data: *mut core::ffi::c_void,
        pcb_data: *mut u32,
    ) -> i32;
}

/// `HKEY_LOCAL_MACHINE` / `HKEY_CURRENT_USER` — the predefined keys are
/// `(LONG)0x8000000N` sign-extended to a pointer-sized handle, which the
/// `u32 -> i32 -> isize` cast chain reproduces exactly (0xFFFFFFFF_8000000N on x64).
const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
/// `RRF_RT_REG_SZ` — accept `REG_SZ`, and auto-EXPAND a `REG_EXPAND_SZ` value to a
/// plain string (the documented behaviour when `RRF_NOEXPAND` is not set), so the
/// returned PATH has `%SystemRoot%` etc. already resolved.
const RRF_RT_REG_SZ: u32 = 0x0000_0002;
const ERROR_SUCCESS: i32 = 0;

/// The Machine environment key (system-wide PATH).
const MACHINE_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
/// The per-User environment key (user PATH).
const USER_ENV: &str = "Environment";

/// Read a `REG_SZ`/`REG_EXPAND_SZ` value (expanded) as an `OsString`, or `None` if
/// the value is absent / unreadable. Two-call pattern: size, then fetch.
fn read_reg_sz(hkey: isize, sub_key: &str, value: &str) -> Option<OsString> {
    let sub: Vec<u16> = OsString::from(sub_key).encode_wide().chain([0]).collect();
    let val: Vec<u16> = OsString::from(value).encode_wide().chain([0]).collect();
    // SAFETY: `RegGetValueW` is a standard advapi32 call; the sub-key/value pointers
    // are NUL-terminated wide strings that outlive the call, the size query passes
    // null data with a valid `pcb_data`, and the fetch passes a buffer sized to the
    // reported byte count. No handles leak (the predefined HKEY needs no close).
    unsafe {
        let mut cb: u32 = 0;
        let r = RegGetValueW(
            hkey,
            sub.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_SZ,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            &mut cb,
        );
        if r != ERROR_SUCCESS || cb == 0 {
            return None;
        }
        // `cb` is a byte count; +1 wchar of headroom in case the API omits the NUL.
        let mut buf = vec![0u16; (cb as usize / 2) + 1];
        let mut cb2 = cb;
        let r2 = RegGetValueW(
            hkey,
            sub.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_SZ,
            core::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut cb2,
        );
        if r2 != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if len == 0 {
            return None;
        }
        Some(OsString::from_wide(&buf[..len]))
    }
}

/// The authoritative PATH composed from the registry: Machine PATH first, then User
/// PATH appended — the same order Windows itself uses when building a login session's
/// PATH. `None` only if neither value is readable.
fn registry_path() -> Option<OsString> {
    let machine = read_reg_sz(HKEY_LOCAL_MACHINE, MACHINE_ENV, "Path");
    let user = read_reg_sz(HKEY_CURRENT_USER, USER_ENV, "Path");
    match (machine, user) {
        (Some(m), Some(u)) => {
            let mut s = m;
            s.push(";");
            s.push(u);
            Some(s)
        }
        (Some(m), None) => Some(m),
        (None, Some(u)) => Some(u),
        (None, None) => None,
    }
}

/// Union two `;`-separated PATH strings, case-insensitively de-duplicated, `first`
/// entries before `extra` ones — preserving order and never emitting a dir twice.
/// PURE (no I/O), so it is unit-tested below.
fn union_paths(first: &OsString, extra: Option<&OsString>) -> OsString {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<OsString> = Vec::new();
    let mut add = |seg: &str| {
        let seg = seg.trim();
        if seg.is_empty() {
            return;
        }
        let key = seg.trim_end_matches('\\').to_ascii_lowercase();
        if seen.contains(&key) {
            return;
        }
        seen.push(key);
        out.push(OsString::from(seg));
    };
    let first = first.to_string_lossy();
    for seg in first.split(';') {
        add(seg);
    }
    if let Some(extra) = extra {
        let extra = extra.to_string_lossy();
        for seg in extra.split(';') {
            add(seg);
        }
    }
    // Re-join with ';'.
    let mut joined = OsString::new();
    for (i, seg) in out.iter().enumerate() {
        if i > 0 {
            joined.push(";");
        }
        joined.push(seg);
    }
    joined
}

/// Overwrite the child env's `PATH` with the union of the live registry PATH and the
/// inherited PATH (registry first, inherited extras kept). No-op when the registry is
/// unreadable or `ATERM_NO_PATH_REFRESH` is set. `PATH` lookup is case-insensitive
/// (Windows env names are), so the single canonical `PATH` entry is updated in place.
pub(crate) fn refresh_child_path(env_pairs: &mut Vec<(OsString, OsString)>) {
    if std::env::var_os("ATERM_NO_PATH_REFRESH").is_some() {
        return;
    }
    let Some(reg) = registry_path() else {
        return;
    };
    let inherited = env_pairs
        .iter()
        .find(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case("path"))
        .map(|(_, v)| v.clone());
    let merged = union_paths(&reg, inherited.as_ref());
    match env_pairs
        .iter_mut()
        .find(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case("path"))
    {
        Some(slot) => slot.1 = merged,
        None => env_pairs.push((OsString::from("PATH"), merged)),
    }
}

#[cfg(test)]
mod tests {
    use super::union_paths;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    /// The union keeps registry order first, appends only inherited dirs not already
    /// present (case-insensitive, trailing-slash-insensitive), and drops empties.
    #[test]
    fn union_dedupes_and_preserves_order() {
        let reg = os(r"C:\Windows\system32;C:\Windows;C:\tools");
        let inherited = os(r"c:\windows\system32\;C:\session-only;C:\TOOLS");
        let merged = union_paths(&reg, Some(&inherited))
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            merged,
            r"C:\Windows\system32;C:\Windows;C:\tools;C:\session-only"
        );
    }

    /// With no inherited PATH the registry PATH passes through unchanged (minus empties).
    #[test]
    fn union_registry_only() {
        let reg = os(r"C:\a;;C:\b;");
        assert_eq!(union_paths(&reg, None).to_string_lossy(), r"C:\a;C:\b");
    }
}
