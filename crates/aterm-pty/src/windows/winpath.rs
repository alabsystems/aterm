// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Refreshes the child shell's `PATH` from the live Windows registry at spawn.

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

const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
const RRF_RT_REG_SZ: u32 = 0x0000_0002;
const ERROR_SUCCESS: i32 = 0;

const MACHINE_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const USER_ENV: &str = "Environment";

fn read_reg_sz(hkey: isize, sub_key: &str, value: &str) -> Option<OsString> {
    let sub: Vec<u16> = OsString::from(sub_key).encode_wide().chain([0]).collect();
    let val: Vec<u16> = OsString::from(value).encode_wide().chain([0]).collect();
    // SAFETY: `RegGetValueW` is a standard advapi32 call; the sub-key/value pointers are
    // NUL-terminated wide strings that outlive the call, the size query passes null data with a
    // valid `pcb_data`, and the fetch passes a buffer sized to the reported byte count.
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

fn union_paths(first: &OsString, extra: Option<&OsString>) -> OsString {
    let key_of = |seg: &str| seg.trim().trim_end_matches('\\').to_ascii_lowercase();
    let first = first.to_string_lossy();
    let extra = extra.map(|extra| extra.to_string_lossy());
    let first_keys: Vec<String> = first.split(';').map(key_of).collect();
    let extra_segs: Vec<&str> = extra
        .as_deref()
        .map_or_else(Vec::new, |e| e.split(';').collect());
    let lead = extra_segs
        .iter()
        .position(|seg| {
            let key = key_of(seg);
            !key.is_empty() && first_keys.contains(&key)
        })
        .unwrap_or(extra_segs.len());
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<OsString> = Vec::new();
    let mut add = |seg: &str| {
        let seg = seg.trim();
        if seg.is_empty() {
            return;
        }
        let key = key_of(seg);
        if seen.contains(&key) {
            return;
        }
        seen.push(key);
        out.push(OsString::from(seg));
    };
    for seg in &extra_segs[..lead] {
        add(seg);
    }
    for seg in first.split(';') {
        add(seg);
    }
    for seg in &extra_segs[lead..] {
        add(seg);
    }
    let mut joined = OsString::new();
    for (i, seg) in out.iter().enumerate() {
        if i > 0 {
            joined.push(";");
        }
        joined.push(seg);
    }
    joined
}

/// Updates `env_pairs` with the union of the live registry `PATH` and inherited `PATH`.
pub(crate) fn refresh_child_path(env_pairs: &mut Vec<(OsString, OsString)>) {
    let Some(reg) = registry_path() else {
        return;
    };
    let is_path = |k: &OsString| k.to_string_lossy().eq_ignore_ascii_case("path");
    let inherited = env_pairs
        .iter()
        .rfind(|(k, _)| is_path(k))
        .map(|(_, v)| v.clone());
    let merged = union_paths(&reg, inherited.as_ref());
    if let Some(first) = env_pairs.iter().position(|(k, _)| is_path(k)) {
        env_pairs[first].1 = merged;
        let mut idx = first + 1;
        while idx < env_pairs.len() {
            if is_path(&env_pairs[idx].0) {
                env_pairs.remove(idx);
            } else {
                idx += 1;
            }
        }
    } else {
        env_pairs.push((OsString::from("PATH"), merged));
    }
}

#[cfg(test)]
mod tests {
    use super::union_paths;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

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

    #[test]
    fn union_keeps_the_inherited_lead_in_front() {
        let reg = os(r"C:\Windows\system32;C:\Users\u\AppData\Local\Programs\aterm");
        let inherited = os(
            r"C:\Users\u\pkg\agents;D:\scratch\aterm;;C:\WINDOWS\system32;C:\session-only;D:\scratch\aterm",
        );
        assert_eq!(
            union_paths(&reg, Some(&inherited)).to_string_lossy(),
            r"C:\Users\u\pkg\agents;D:\scratch\aterm;C:\Windows\system32;C:\Users\u\AppData\Local\Programs\aterm;C:\session-only"
        );
        assert_eq!(
            union_paths(&reg, Some(&os(r"D:\only"))).to_string_lossy(),
            r"D:\only;C:\Windows\system32;C:\Users\u\AppData\Local\Programs\aterm"
        );
    }

    #[test]
    fn union_registry_only() {
        let reg = os(r"C:\a;;C:\b;");
        assert_eq!(union_paths(&reg, None).to_string_lossy(), r"C:\a;C:\b");
    }
}
