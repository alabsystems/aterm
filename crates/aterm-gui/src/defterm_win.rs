// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows 11 default-terminal (`DefTerm`) registry handoff inspection and removal.

#![cfg(windows)]

use core::ptr;
use std::io;

use crate::win_ffi::{
    HKEY_CURRENT_USER, Hkey, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegSetValueExW, wide,
};

const KEY_READ: u32 = 0x0002_0019;
const ERROR_FILE_NOT_FOUND: i32 = 2;

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        hkey: Hkey,
        subkey: *const u16,
        options: u32,
        sam: u32,
        result: *mut Hkey,
    ) -> i32;
    fn RegQueryValueExW(
        hkey: Hkey,
        name: *const u16,
        reserved: *mut u32,
        ty: *mut u32,
        data: *mut u8,
        cb: *mut u32,
    ) -> i32;
    fn RegDeleteValueW(hkey: Hkey, name: *const u16) -> i32;
}

/// The delegation registry key (`Console\%%Startup`).
pub(crate) const STARTUP_KEY: &str = r"Console\%%Startup";
/// Registry value naming the console host to delegate to.
pub(crate) const VALUE_CONSOLE: &str = "DelegationConsole";
/// Registry value naming the terminal to delegate to.
pub(crate) const VALUE_TERMINAL: &str = "DelegationTerminal";

/// aterm's terminal-side COM CLSID.
pub(crate) const CLSID_ATERM_TERMINAL: &str = "{02E7D4D8-6B51-4BB2-84CE-5949B78B2308}";

/// aterm's console-host COM CLSID slot.
pub(crate) const CLSID_ATERM_CONSOLE: &str = "{17A55D2B-8FC4-4990-8805-617293BD5A4D}";

/// Returns whether a COM handoff server can answer `EstablishPtyHandoff` (always `false`).
#[must_use]
pub(crate) fn handoff_server_available() -> bool {
    false
}

/// Open a delegation key for reading; `Ok(None)` when the key does not exist. Takes the
/// subkey so tests drive the read/remove path against a scratch key, never the live one.
fn open_startup_read_at(subkey: &str) -> io::Result<Option<Hkey>> {
    let sub = wide(subkey);
    let mut hk: Hkey = 0;
    // SAFETY: valid NUL-terminated wide subkey; `hk` written on success.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_READ, &mut hk) };
    match rc {
        0 => Ok(Some(hk)),
        ERROR_FILE_NOT_FOUND => Ok(None),
        e => Err(io::Error::from_raw_os_error(e)),
    }
}

/// Read one `REG_SZ` value, trimming the trailing NUL(s) the API includes.
fn read_string(hk: Hkey, name: &str) -> Option<String> {
    let n = wide(name);
    let mut ty: u32 = 0;
    let mut cb: u32 = 0;
    // SAFETY: NULL data with a valid `cb` out-param is the documented probe form.
    let rc = unsafe {
        RegQueryValueExW(
            hk,
            n.as_ptr(),
            ptr::null_mut(),
            &mut ty,
            ptr::null_mut(),
            &mut cb,
        )
    };
    if rc != 0 || ty != REG_SZ || cb == 0 {
        return None;
    }
    let mut buf = vec![0u16; (cb as usize).div_ceil(2)];
    let mut cb2 = (buf.len() * 2) as u32;
    // SAFETY: `buf` is `cb2` bytes and outlives the call.
    let rc = unsafe {
        RegQueryValueExW(
            hk,
            n.as_ptr(),
            ptr::null_mut(),
            &mut ty,
            buf.as_mut_ptr().cast::<u8>(),
            &mut cb2,
        )
    };
    if rc != 0 {
        return None;
    }
    let used = (cb2 as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..used]);
    Some(s.trim_end_matches('\0').to_string())
}

/// Reads the current `(DelegationConsole, DelegationTerminal)` pair from `HKCU`.
pub(crate) fn current_delegation() -> io::Result<(Option<String>, Option<String>)> {
    current_delegation_at(STARTUP_KEY)
}

/// [`current_delegation`], against an arbitrary subkey (see
/// [`open_startup_read_at`] for why the seam exists).
fn current_delegation_at(subkey: &str) -> io::Result<(Option<String>, Option<String>)> {
    let Some(hk) = open_startup_read_at(subkey)? else {
        return Ok((None, None));
    };
    let console = read_string(hk, VALUE_CONSOLE);
    let terminal = read_string(hk, VALUE_TERMINAL);
    // SAFETY: `hk` was opened by open_startup_read_at above.
    unsafe { RegCloseKey(hk) };
    Ok((console, terminal))
}

/// Returns whether `terminal` matches [`CLSID_ATERM_TERMINAL`] (case-insensitive).
#[must_use]
pub(crate) fn is_aterm_default(terminal: Option<&str>) -> bool {
    terminal.is_some_and(|t| t.eq_ignore_ascii_case(CLSID_ATERM_TERMINAL))
}

/// Ownership classification of the current delegation values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DelegationOwner {
    /// Neither value names a terminal — the stock "Let Windows decide" state.
    Unregistered,
    /// aterm wrote this (or would have): ours to remove.
    Aterm,
    /// Another terminal (Windows Terminal, a third-party host) owns it.
    Other,
}

/// Classifies a `(DelegationConsole, DelegationTerminal)` pair by owner.
#[must_use]
pub(crate) fn delegation_owner(console: Option<&str>, terminal: Option<&str>) -> DelegationOwner {
    if is_aterm_default(terminal) {
        return DelegationOwner::Aterm;
    }
    let console_is_ours = console.is_some_and(|c| c.eq_ignore_ascii_case(CLSID_ATERM_CONSOLE));
    match (terminal, console) {
        (None | Some(""), None | Some("")) => DelegationOwner::Unregistered,
        (None | Some(""), _) if console_is_ours => DelegationOwner::Aterm,
        _ => DelegationOwner::Other,
    }
}

/// Outcome of [`unset_default_terminal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnsetOutcome {
    /// Nothing was registered; nothing was written.
    NothingRegistered,
    /// aterm's registration was removed.
    Removed,
    /// Another terminal owns the delegation; nothing was changed.
    NotOurs {
        console: Option<String>,
        terminal: Option<String>,
    },
}

/// Registers aterm as the default terminal.
///
/// # Errors
/// Returns `Unsupported` while [`handoff_server_available`] is false (checked before
/// any key is created, so a refusal leaves the registry untouched), or a registry error.
pub(crate) fn set_default_terminal() -> io::Result<()> {
    if !handoff_server_available() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this build has no COM handoff server, so registering aterm as the default \
             terminal would point conhost at a class nothing can create — every new \
             console would fail to open. Refusing to write the delegation keys.",
        ));
    }
    let sub = wide(STARTUP_KEY);
    let mut hk: Hkey = 0;
    // SAFETY: valid NUL-terminated wide subkey; `hk` written on success.
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
    let name = wide(VALUE_TERMINAL);
    let data = wide(CLSID_ATERM_TERMINAL);
    let cb = (data.len() * std::mem::size_of::<u16>()) as u32;
    // SAFETY: open key; `data`/`name` outlive the call; `cb` is the byte length
    // of `data`, its NUL terminator included, as REG_SZ requires.
    let rc =
        unsafe { RegSetValueExW(hk, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast::<u8>(), cb) };
    // SAFETY: `hk` was opened above.
    unsafe { RegCloseKey(hk) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    Ok(())
}

/// Removes aterm's delegation registration if owned by aterm.
pub(crate) fn unset_default_terminal() -> io::Result<UnsetOutcome> {
    unset_delegation_at(STARTUP_KEY)
}

/// [`unset_default_terminal`], against an arbitrary subkey (see
/// [`open_startup_read_at`] for why the seam exists).
fn unset_delegation_at(subkey: &str) -> io::Result<UnsetOutcome> {
    let (console, terminal) = current_delegation_at(subkey)?;
    match delegation_owner(console.as_deref(), terminal.as_deref()) {
        DelegationOwner::Unregistered => return Ok(UnsetOutcome::NothingRegistered),
        DelegationOwner::Other => return Ok(UnsetOutcome::NotOurs { console, terminal }),
        DelegationOwner::Aterm => {}
    }

    let sub = wide(subkey);
    let mut hk: Hkey = 0;
    // SAFETY: valid NUL-terminated wide subkey; `hk` written on success.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_WRITE, &mut hk) };
    if rc == ERROR_FILE_NOT_FOUND {
        // Raced away between the read and the open: nothing to remove.
        return Ok(UnsetOutcome::NothingRegistered);
    }
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    for v in [VALUE_TERMINAL, VALUE_CONSOLE] {
        let n = wide(v);
        // SAFETY: open key + valid NUL-terminated wide value name.
        unsafe { RegDeleteValueW(hk, n.as_ptr()) };
    }
    // SAFETY: `hk` was opened above.
    unsafe { RegCloseKey(hk) };
    Ok(UnsetOutcome::Removed)
}

#[cfg(test)]
#[path = "defterm_win_tests.rs"]
mod tests;
