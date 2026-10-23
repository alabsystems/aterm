// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Windows Explorer "Open aterm here" context-menu verb registration under `HKCU\Software\Classes`.
#![cfg(windows)]

use core::ptr;
use std::io;

use crate::win_ffi::{
    HKEY_CURRENT_USER, Hkey, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegSetValueExW, wide,
};

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegDeleteTreeW(hkey: Hkey, subkey: *const u16) -> i32;
}

const VERBS: [(&str, &str); 3] = [
    (r"Software\Classes\Directory\Background\shell\aterm", "%V"),
    (r"Software\Classes\Directory\shell\aterm", "%1"),
    (r"Software\Classes\Drive\shell\aterm", "%1"),
];

fn create_key(subkey: &str) -> io::Result<Hkey> {
    let sub = wide(subkey);
    let mut hk: Hkey = 0;
    // SAFETY: valid NUL-terminated wide subkey; out-param `hk` is written on success.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            ptr::null(),
            &mut hk,
            ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    Ok(hk)
}

fn set_string(hk: Hkey, name: Option<&str>, value: &str) -> io::Result<()> {
    let data = wide(value);
    let name_w = name.map(wide);
    let name_ptr = name_w.as_ref().map_or(ptr::null(), |w| w.as_ptr());
    let cb = (data.len() * std::mem::size_of::<u16>()) as u32;
    // SAFETY: `hk` is an open key; `data`/`name_w` outlive the call; `cb` is the byte
    // length of the NUL-terminated UTF-16 buffer, as REG_SZ requires.
    let rc = unsafe { RegSetValueExW(hk, name_ptr, 0, REG_SZ, data.as_ptr().cast::<u8>(), cb) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    Ok(())
}

/// Build the verb's registry command line: `"<exe>" -d "<token>."`.
///
/// The trailing dot prevents a drive root (`C:\`) from escaping the closing quote under MSVCRT
/// argv rules.
fn verb_command(exe: &str, token: &str) -> String {
    format!("\"{exe}\" -d \"{token}.\"")
}

/// Install the "Open aterm here" verb pointing at this install's windowed image (`aterm-gui.exe`).
pub(crate) fn install() -> io::Result<()> {
    let current = std::env::current_exe()?;
    let Some(exe) = crate::win32::windowed_front_door_beside(&current) else {
        return Err(io::Error::other(format!(
            "no windowed aterm image (aterm-gui.exe) beside {} for Explorer to start",
            current.display()
        )));
    };
    let exe_s = exe.to_string_lossy();
    for (base, arg) in VERBS {
        let hk = create_key(base)?;
        let r1 = set_string(hk, None, "Open aterm here");
        let r2 = set_string(hk, Some("Icon"), &format!("\"{exe_s}\",0"));
        // SAFETY: `hk` was opened by create_key above.
        unsafe { RegCloseKey(hk) };
        r1?;
        r2?;
        let cmd = create_key(&format!(r"{base}\command"))?;
        let r3 = set_string(cmd, None, &verb_command(&exe_s, arg));
        // SAFETY: `cmd` was opened by create_key above.
        unsafe { RegCloseKey(cmd) };
        r3?;
    }
    Ok(())
}

/// Remove the verb (recursively deletes each root).
pub(crate) fn uninstall() -> io::Result<()> {
    for (base, _) in VERBS {
        let sub = wide(base);
        // SAFETY: valid NUL-terminated wide subkey; return code (incl. not-found) ignored.
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, sub.as_ptr()) };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_root_expansion_does_not_escape_the_closing_quote() {
        for (_, token) in VERBS {
            let template = verb_command(r"C:\apps\aterm\aterm-gui.exe", token);
            assert!(
                !template.ends_with("\\\""),
                "template ends in backslash-quote: {template}"
            );
            let expanded = template.replace(token, r"C:\");
            assert!(
                !expanded.ends_with("\\\""),
                "expanded verb ends in backslash-quote (operand would parse as `C:\"`): {expanded}"
            );
            assert!(
                expanded.ends_with("\\.\""),
                "expanded verb should end root-dot-quote so the operand stays `C:\\.`: {expanded}"
            );
        }
    }
}
