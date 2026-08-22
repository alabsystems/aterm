// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Windows Explorer "Open aterm here" context-menu registration.
//!
//! Right-clicking a folder, a folder's empty background, or a drive gains an
//! **Open aterm here** verb that launches `aterm-gui -d <that path>` — so a new
//! terminal opens already `cd`'d into the location. Registered PER-USER under
//! `HKCU\Software\Classes` (no admin, no MSIX identity needed) and trivially
//! reversible (`--uninstall-context-menu`).
//!
//! Hand-rolled `advapi32` FFI (flat C, the house style — mirrors the extern blocks
//! in `aterm-pty/src/windows/ffi.rs` and `notify.rs`); no `windows`-crate dependency.
#![cfg(windows)]

use core::ptr;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

type Hkey = isize;
const HKEY_CURRENT_USER: Hkey = 0x8000_0001u32 as Hkey;
const REG_SZ: u32 = 1;
const KEY_WRITE: u32 = 0x0002_0006;
const REG_OPTION_NON_VOLATILE: u32 = 0;

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegCreateKeyExW(
        hkey: Hkey,
        subkey: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        sam: u32,
        security: *const core::ffi::c_void,
        result: *mut Hkey,
        disposition: *mut u32,
    ) -> i32;
    fn RegSetValueExW(
        hkey: Hkey,
        name: *const u16,
        reserved: u32,
        ty: u32,
        data: *const u8,
        cb: u32,
    ) -> i32;
    fn RegCloseKey(hkey: Hkey) -> i32;
    fn RegDeleteTreeW(hkey: Hkey, subkey: *const u16) -> i32;
}

/// The three verb roots (base subkey, path-argument token). `%V` is the folder for a
/// background click; `%1` is the selected folder/drive.
const VERBS: [(&str, &str); 3] = [
    (r"Software\Classes\Directory\Background\shell\aterm", "%V"),
    (r"Software\Classes\Directory\shell\aterm", "%1"),
    (r"Software\Classes\Drive\shell\aterm", "%1"),
];

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

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

/// Set a `REG_SZ`; `name = None` sets the key's default value.
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
/// The trailing dot is load-bearing. Explorer substitutes `%1`/`%V` textually,
/// and for a drive root that text is `C:\` — so the naive `-d "%V"` expands to
/// `-d "C:\"`, where the backslash escapes the closing quote under the MSVCRT
/// argv rules and the operand parses as `C:"` (reproduced: `rustc "C:\"` →
/// ``couldn't read `C:"``). `Path::is_dir()` then fails and the GUI-subsystem
/// exe exits 2 into a null stderr — right-clicking a drive was a *silent*
/// no-op. With the dot the expansion is `-d "C:\."`: the quote survives, and
/// `C:\.` resolves to the root. Ordinary folders expand to e.g. `C:\src.`,
/// which Win32 path normalization treats as `C:\src` (trailing dots on a final
/// component are stripped), so they are unaffected. This is the workaround Git
/// for Windows ships in its own shell verb (`git_shell\command` = `"--cd=%v."`).
/// Rejected alternatives: dropping the quotes breaks any path with spaces, and
/// special-casing drive roots is impossible — the substitution happens in
/// Explorer long after this one static template is written.
fn verb_command(exe: &str, token: &str) -> String {
    format!("\"{exe}\" -d \"{token}.\"")
}

/// Install the "Open aterm here" verb on directories, directory backgrounds, and
/// drives, pointing at the CURRENT `aterm-gui.exe` with `-d <path>`.
pub(crate) fn install() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let exe_s = exe.to_string_lossy();
    for (base, arg) in VERBS {
        let hk = create_key(base)?;
        let r1 = set_string(hk, None, "Open aterm here");
        // `Icon` gives the verb the aterm glyph (the exe's first embedded icon).
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

/// Remove the verb (recursively deletes each root). Missing keys are not an error.
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

    /// A drive root's `%1`/`%V` expands to `C:\`, and a backslash directly
    /// before the closing quote escapes it (see [`verb_command`]'s doc). Guard
    /// both the raw template and its worst-case expansion, for every verb root.
    #[test]
    fn drive_root_expansion_does_not_escape_the_closing_quote() {
        for (_, token) in VERBS {
            let template = verb_command(r"C:\apps\aterm\aterm-gui.exe", token);
            assert!(
                !template.ends_with("\\\""),
                "template ends in backslash-quote: {template}"
            );
            // Simulate Explorer's textual substitution on a drive root — the
            // one path that ends in a backslash and cannot be spelled otherwise.
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
