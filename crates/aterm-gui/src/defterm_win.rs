// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Windows 11 default-terminal (DefTerm) handoff: registration.
//!
//! When aterm is the default terminal, a console program (a double-clicked `.bat`,
//! `Win+R cmd`) opens in aterm instead of conhost: conhost creates the pseudoconsole
//! and hands its far ends over COM (`ITerminalHandoff*::EstablishPtyHandoff`) to the
//! registered terminal, whose receiving half is `aterm_pty::adopt_handoff`.
//!
//! This module registers nothing: registration is gated on
//! [`handoff_server_available`], which is false. Removal is always available.
//!
//! # The key
//!
//! * `HKCU\Console\%%Startup` exists on stock Windows 11 with no values ("Let Windows
//!   decide"). `%%Startup`, `DelegationConsole` and `DelegationTerminal` appear
//!   verbatim in `conhost.exe`; the doubled `%` is two literal percent signs.
//! * Values are `REG_SZ` CLSIDs in braced registry form. Windows Terminal writes
//!   `DelegationConsole = {2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}` (its
//!   `OpenConsole.exe`) and `DelegationTerminal = {E12CFF52-A866-4C77-9A90-F570A7AA2C6B}`.
//!
//! # Per-user and reversible
//!
//! Everything lives under `HKCU` and needs no admin. [`unset_default_terminal`] is
//! never gated, because a dangling delegation CLSID can stop consoles from opening,
//! but it removes only aterm's registration: the values are usually another
//! terminal's.

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

/// The delegation key, exactly as conhost spells it: the doubled `%` is literal,
/// and a raw string keeps any escaping layer from halving it.
pub(crate) const STARTUP_KEY: &str = r"Console\%%Startup";
/// Value naming the console host to delegate to.
pub(crate) const VALUE_CONSOLE: &str = "DelegationConsole";
/// Value naming the terminal to delegate to.
pub(crate) const VALUE_TERMINAL: &str = "DelegationTerminal";

/// aterm's terminal-side CLSID: the class conhost would `CoCreateInstance` and call
/// `EstablishPtyHandoff` on. Stable forever: changing it would orphan every
/// registry that holds it.
pub(crate) const CLSID_ATERM_TERMINAL: &str = "{02E7D4D8-6B51-4BB2-84CE-5949B78B2308}";

/// aterm's console-host CLSID slot. Nothing writes it.
///
/// `DelegationConsole` names the console host, not the terminal (Windows Terminal
/// points it at its `OpenConsole.exe`). aterm ships no console host, and whether
/// this slot should name an aterm-shipped OpenConsole or the inbox host (whose GUID
/// is not established) is open; a wrong CLSID here stops consoles from opening.
/// Read only by [`delegation_owner`], to recognize aterm's own half-state.
pub(crate) const CLSID_ATERM_CONSOLE: &str = "{17A55D2B-8FC4-4990-8805-617293BD5A4D}";

/// Is a COM handoff server able to answer `EstablishPtyHandoff`? Always `false`,
/// which keeps this module inert: writing the delegation CLSIDs while nothing
/// answers would hand every new console to a class that cannot be created, so no
/// flag may reach that state.
///
/// The blocker is marshalling, not the vtable (`docs/NATIVE_WINDOWS_DESIGN.md` §5):
/// `EstablishPtyHandoff` is cross-process with `system_handle` parameters, so its
/// IIDs need a registered proxy/stub. Windows Terminal's resolve only through its
/// MSIX identity's packaged-COM catalog (none appear in `HKCR\Interface` or
/// `HKCR\CLSID`), so an unpackaged `aterm.exe` needs its own MIDL-generated
/// proxy/stub DLL or MSIX identity. The receiving half (`aterm_pty::adopt_handoff`
/// and the signal-pipe resize) is built; the STA broker thread for the class
/// factory's apartment is written together with the server.
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
    // Size probe: data = NULL asks for the byte length.
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
    // Round up to whole u16s; the registry does not promise an even byte count.
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

/// The current delegation pair: `(DelegationConsole, DelegationTerminal)`. `None` for a
/// value that is absent or not a string: the stock state, "Windows decides" (conhost).
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

/// True when the delegation currently points at aterm (case-insensitive: the
/// registry preserves whatever case was written, and other writers vary).
#[must_use]
pub(crate) fn is_aterm_default(terminal: Option<&str>) -> bool {
    terminal.is_some_and(|t| t.eq_ignore_ascii_case(CLSID_ATERM_TERMINAL))
}

/// Who the delegation values belong to: [`unset_default_terminal`] never touches a
/// registration aterm did not write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DelegationOwner {
    /// Neither value names a terminal — the stock "Let Windows decide" state.
    Unregistered,
    /// aterm wrote this (or would have): ours to remove.
    Aterm,
    /// Another terminal (Windows Terminal, a third-party host) owns it.
    Other,
}

/// Classify a delegation pair. Pure, so the guard that protects a third party's
/// setting has no registry in it.
///
/// `DelegationTerminal` decides: it names the terminal and is the only value
/// [`set_default_terminal`] writes. An empty terminal slot with aterm's console
/// CLSID is still aterm's (a possible half-state); any other value is someone else's.
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

/// What [`unset_default_terminal`] did, so the CLI can tell removing aterm's
/// registration from finding another terminal's.
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

/// Register aterm as the default terminal.
///
/// # Errors
/// `Unsupported` while [`handoff_server_available`] is false, checked before any key
/// is created so a refusal leaves the registry untouched.
pub(crate) fn set_default_terminal() -> io::Result<()> {
    if !handoff_server_available() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this build has no COM handoff server, so registering aterm as the default \
             terminal would point conhost at a class nothing can create — every new \
             console would fail to open. Refusing to write the delegation keys.",
        ));
    }
    // Unreachable while the gate is false, and incomplete: only `DelegationTerminal`
    // is written. `DelegationConsole` must be filled in here too before the gate
    // flips (see `CLSID_ATERM_CONSOLE`), or this writes a half-registration.
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
    // of the NUL-terminated UTF-16 buffer, as REG_SZ requires.
    let rc =
        unsafe { RegSetValueExW(hk, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast::<u8>(), cb) };
    // SAFETY: `hk` was opened above.
    unsafe { RegCloseKey(hk) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    Ok(())
}

/// Remove aterm's delegation registration, restoring "Let Windows decide".
///
/// Never gated on [`handoff_server_available`]: it is the escape hatch from a
/// registration that points at a class nothing can create, so it works in every
/// build, including one whose registering exe is gone.
///
/// Guarded on ownership: `HKCU\Console\%%Startup` usually holds another terminal's
/// setting, so a pair aterm did not write is left untouched and reported as
/// [`UnsetOutcome::NotOurs`] (as `uninstall.ps1` does). Deletes the values, not the
/// key, which is Windows' and may hold other settings. Idempotent.
pub(crate) fn unset_default_terminal() -> io::Result<UnsetOutcome> {
    unset_delegation_at(STARTUP_KEY)
}

/// [`unset_default_terminal`], against an arbitrary subkey (see
/// [`open_startup_read_at`] for why the seam exists).
fn unset_delegation_at(subkey: &str) -> io::Result<UnsetOutcome> {
    // Decide from a read before anything is opened for writing.
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
    // Both values go: they are one vendor's pair, and the terminal slot says it is aterm's.
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
mod tests {
    use super::*;

    /// The doubled percent is literal, as found verbatim in conhost.exe; halving it
    /// silently disables handoff.
    #[test]
    fn startup_key_keeps_its_two_literal_percent_signs() {
        assert_eq!(STARTUP_KEY, "Console\\%%Startup");
        assert!(
            STARTUP_KEY.contains("%%"),
            "the %% is literal, not an escape"
        );
        assert_eq!(STARTUP_KEY.matches('%').count(), 2);
        // And the wide encoding the registry actually sees keeps both.
        let w = wide(STARTUP_KEY);
        assert_eq!(w.iter().filter(|&&c| c == u16::from(b'%')).count(), 2);
        assert_eq!(*w.last().expect("non-empty"), 0, "must be NUL-terminated");
    }

    /// Both CLSIDs are registry-shaped: braced, upper-case, 8-4-4-4-12.
    #[test]
    fn aterm_clsid_is_well_formed_braced_uppercase() {
        for clsid in [CLSID_ATERM_TERMINAL, CLSID_ATERM_CONSOLE] {
            assert!(clsid.starts_with('{') && clsid.ends_with('}'), "{clsid}");
            assert_eq!(clsid.len(), 38, "{clsid}");
            let body = &clsid[1..clsid.len() - 1];
            let groups: Vec<&str> = body.split('-').collect();
            assert_eq!(groups.len(), 5, "{clsid}");
            assert_eq!(
                groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
                vec![8, 4, 4, 4, 12],
                "{clsid}"
            );
            assert!(
                body.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
                "{clsid}"
            );
            assert!(
                !body.chars().any(|c| c.is_ascii_lowercase()),
                "registry CLSIDs are conventionally upper-case: {clsid}"
            );
        }
        assert_ne!(
            CLSID_ATERM_TERMINAL, CLSID_ATERM_CONSOLE,
            "the console and terminal slots are different classes"
        );
    }

    /// aterm's CLSID must not collide with Windows Terminal's — writing WT's
    /// CLSID would hand every console to WT (or to nothing, if it is absent).
    #[test]
    fn aterm_clsid_is_not_windows_terminals() {
        for wt in [
            "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}", // OpenConsole.exe
            "{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}", // WindowsTerminal.exe
        ] {
            assert!(!CLSID_ATERM_TERMINAL.eq_ignore_ascii_case(wt));
            assert!(!CLSID_ATERM_CONSOLE.eq_ignore_ascii_case(wt));
        }
    }

    /// While no COM server can answer, `set_default_terminal` refuses rather than
    /// register a CLSID that makes consoles fail to open.
    #[test]
    fn registration_refuses_while_no_handoff_server_can_answer() {
        assert!(
            !handoff_server_available(),
            "no COM handoff server ships in this build yet"
        );
        let err = set_default_terminal()
            .expect_err("registration must refuse while nothing can answer the handoff");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    /// A refused registration leaves the live delegation as it was (the test only reads).
    #[test]
    fn a_refused_registration_changes_nothing() {
        let before = current_delegation().expect("reading the delegation must not fail");
        let _ = set_default_terminal();
        let after = current_delegation().expect("reading the delegation must not fail");
        assert_eq!(
            before, after,
            "a refused registration must not touch the registry"
        );
    }

    /// Ownership check is case-insensitive and never claims an empty/foreign value.
    #[test]
    fn is_aterm_default_only_matches_our_clsid() {
        assert!(is_aterm_default(Some(CLSID_ATERM_TERMINAL)));
        assert!(is_aterm_default(Some(
            &CLSID_ATERM_TERMINAL.to_ascii_lowercase()
        )));
        assert!(!is_aterm_default(None));
        assert!(!is_aterm_default(Some("")));
        assert!(!is_aterm_default(Some(
            "{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}"
        )));
    }

    /// The ownership classifier, exhaustively: it is the whole guard that keeps
    /// `--unset-default-terminal` off another app's setting.
    #[test]
    fn delegation_owner_classifies_the_pair() {
        let wt_terminal = "{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}";
        let wt_console = "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}";
        use DelegationOwner::{Aterm, Other, Unregistered};
        let cases: &[(Option<&str>, Option<&str>, DelegationOwner)] = &[
            // (console, terminal, owner)
            (None, None, Unregistered),
            (Some(""), Some(""), Unregistered),
            (None, Some(CLSID_ATERM_TERMINAL), Aterm),
            // Case-insensitively ours: the registry keeps whatever case was written.
            (None, Some("{02e7d4d8-6b51-4bb2-84ce-5949b78b2308}"), Aterm),
            // Ours even when the console slot is a stranger's: the terminal slot
            // is the deciding one and it names us.
            (Some(wt_console), Some(CLSID_ATERM_TERMINAL), Aterm),
            // aterm's own half-state, should it ever write the console slot.
            (Some(CLSID_ATERM_CONSOLE), None, Aterm),
            // Windows Terminal is the default.
            (Some(wt_console), Some(wt_terminal), Other),
            (None, Some(wt_terminal), Other),
            // A console slot we did not write, with no terminal: not ours to touch.
            (Some(wt_console), None, Other),
            // Garbage we did not write is still not ours.
            (None, Some("{00000000-0000-0000-0000-000000000000}"), Other),
        ];
        for (console, terminal, want) in cases {
            assert_eq!(
                delegation_owner(*console, *terminal),
                *want,
                "console={console:?} terminal={terminal:?}"
            );
        }
    }

    /// A scratch key under our own name, so the removal path runs end to end without
    /// writing the live console settings. Deleted on drop, including on panic.
    struct ScratchKey(String);

    impl ScratchKey {
        fn new(name: &str) -> Self {
            let path = format!("Software\\aterm-defterm-selftest\\{name}");
            let k = Self(path);
            k.delete(); // any leftover from an interrupted run
            let sub = wide(&k.0);
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
            assert_eq!(rc, 0, "creating the scratch key must succeed");
            // SAFETY: `hk` was opened above.
            unsafe { RegCloseKey(hk) };
            k
        }

        fn set(&self, name: &str, value: &str) {
            let sub = wide(&self.0);
            let mut hk: Hkey = 0;
            // SAFETY: valid NUL-terminated wide subkey; `hk` written on success.
            let rc =
                unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_WRITE, &mut hk) };
            assert_eq!(rc, 0, "opening the scratch key must succeed");
            let n = wide(name);
            let d = wide(value);
            let cb = (d.len() * std::mem::size_of::<u16>()) as u32;
            // SAFETY: open key; buffers outlive the call; `cb` is their byte length.
            let rc =
                unsafe { RegSetValueExW(hk, n.as_ptr(), 0, REG_SZ, d.as_ptr().cast::<u8>(), cb) };
            // SAFETY: `hk` was opened above.
            unsafe { RegCloseKey(hk) };
            assert_eq!(rc, 0, "writing the scratch value must succeed");
        }

        fn delete(&self) {
            let sub = wide(&self.0);
            // SAFETY: valid NUL-terminated wide subkey under HKCU.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, sub.as_ptr()) };
            // Take the parent too, but only with `RegDeleteKeyW`, which fails while
            // the key has subkeys, so a parallel test's scratch key is never swept up.
            let parent = wide("Software\\aterm-defterm-selftest");
            // SAFETY: valid NUL-terminated wide subkey under HKCU.
            unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, parent.as_ptr()) };
        }
    }

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            self.delete();
        }
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegDeleteTreeW(hkey: Hkey, subkey: *const u16) -> i32;
        fn RegDeleteKeyW(hkey: Hkey, subkey: *const u16) -> i32;
    }

    /// Against a scratch key: a foreign pair survives unset byte for byte, and only
    /// aterm's own registration is removed.
    #[test]
    fn unset_spares_a_foreign_registration_and_removes_only_ours() {
        let wt_terminal = "{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}";
        let wt_console = "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}";
        let key = ScratchKey::new("foreign-vs-ours");

        // 1. Empty: nothing registered, nothing to do.
        assert_eq!(
            unset_delegation_at(&key.0).expect("unset must not error"),
            UnsetOutcome::NothingRegistered
        );

        // 2. Windows Terminal owns it. Nothing may change.
        key.set(VALUE_TERMINAL, wt_terminal);
        key.set(VALUE_CONSOLE, wt_console);
        assert_eq!(
            unset_delegation_at(&key.0).expect("unset must not error"),
            UnsetOutcome::NotOurs {
                console: Some(wt_console.to_string()),
                terminal: Some(wt_terminal.to_string()),
            },
            "a foreign registration must be reported as foreign"
        );
        assert_eq!(
            current_delegation_at(&key.0).expect("read"),
            (Some(wt_console.to_string()), Some(wt_terminal.to_string())),
            "a foreign registration must survive --unset-default-terminal untouched"
        );

        // 3. aterm owns it: both values go.
        key.set(VALUE_TERMINAL, CLSID_ATERM_TERMINAL);
        assert_eq!(
            unset_delegation_at(&key.0).expect("unset must not error"),
            UnsetOutcome::Removed
        );
        assert_eq!(
            current_delegation_at(&key.0).expect("read"),
            (None, None),
            "our own registration must really be gone, both values"
        );

        // 4. Idempotent.
        assert_eq!(
            unset_delegation_at(&key.0).expect("unset must not error"),
            UnsetOutcome::NothingRegistered
        );
    }

    /// The removal path never consults the availability gate: it is the escape hatch
    /// from a delegation nothing can answer.
    #[test]
    fn unset_is_not_gated_on_the_handoff_server() {
        assert!(!handoff_server_available());
        let key = ScratchKey::new("ungated");
        key.set(VALUE_TERMINAL, CLSID_ATERM_TERMINAL);
        assert_eq!(
            unset_delegation_at(&key.0).expect("unset must work while the gate is false"),
            UnsetOutcome::Removed
        );
    }

    /// Reading the live key is total: present, absent and empty are all answers.
    #[test]
    fn current_delegation_reads_the_live_machine_without_erroring() {
        let (console, terminal) = current_delegation().expect("must not error");
        for v in [console, terminal].into_iter().flatten() {
            assert!(
                !v.contains('\0'),
                "a returned CLSID must be NUL-trimmed, got {v:?}"
            );
        }
    }
}
