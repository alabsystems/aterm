// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Windows 11 **default-terminal (DefTerm) handoff** — registration surface.
//!
//! The marquee Windows integration: when aterm is the default terminal, a
//! double-clicked `.bat`, `Win+R cmd`, or an installer's console window opens
//! *in aterm* instead of conhost. Windows implements this by letting a terminal
//! register two CLSIDs; when a console program starts, conhost creates the
//! pseudoconsole and hands the far ends to that terminal over COM
//! (`ITerminalHandoff*::EstablishPtyHandoff`), which then adopts them —
//! `aterm_pty::adopt_handoff` is the receiving half and is fully built.
//!
//! **This module registers NOTHING today, by design.** See
//! [`handoff_server_available`] for the one blocker and the field evidence
//! behind it. Everything here is the verified, reviewed registration path
//! waiting on that gate, plus the always-available *removal* path.
//!
//! # Field study (this machine, 2026-08; all values MACHINE-VERIFIED)
//!
//! The design doc said to verify the literal key name against a live conhost
//! before shipping, because a wrong key silently disables handoff. Done:
//!
//! * `HKCU\Console\%%Startup` — the key exists on a stock Windows 11 26200 with
//!   zero values (the "Let Windows decide" default). The literal string
//!   `%%Startup`, and the value names `DelegationConsole` / `DelegationTerminal`,
//!   all appear verbatim in `C:\Windows\System32\conhost.exe`, alongside the
//!   telemetry names `SrvInit_FoundDelegationConsole` /
//!   `SrvInit_FoundDelegationTerminal` and the source path
//!   `...\propslib\delegationconfig.cpp` that reads them. The doubled `%%` is
//!   NOT an escape — it is two literal percent signs in the key name.
//! * Values are `REG_SZ` CLSIDs in braced registry form.
//!
//! Windows Terminal 1.24.11911.0's own values, read out of its AppxManifest:
//! `DelegationConsole = {2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}` (its
//! `OpenConsole.exe`) and `DelegationTerminal =
//! {E12CFF52-A866-4C77-9A90-F570A7AA2C6B}` (its `WindowsTerminal.exe`).
//!
//! # Why this is per-user and reversible
//!
//! Everything written lives under `HKCU`, needs no admin, and
//! [`unset_default_terminal`] removes it in every build — including when the exe
//! that wrote it is already gone. That is the whole reason the removal path is
//! never gated on [`handoff_server_available`]: a dangling delegation CLSID is
//! the one failure mode that could stop consoles from opening at all, so getting
//! OUT must always work even when getting IN cannot.
//!
//! Removal is ungated but NOT unconditional: it removes **aterm's**
//! registration only. The delegation values are a shared, machine-wide console
//! setting normally owned by another terminal, and clobbering a third party's
//! configuration is not an escape hatch — see [`unset_default_terminal`].

#![cfg(windows)]

use core::ptr;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

type Hkey = isize;
const HKEY_CURRENT_USER: Hkey = 0x8000_0001u32 as Hkey;
const REG_SZ: u32 = 1;
const KEY_WRITE: u32 = 0x0002_0006;
const KEY_READ: u32 = 0x0002_0019;
const REG_OPTION_NON_VOLATILE: u32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;

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
    fn RegOpenKeyExW(
        hkey: Hkey,
        subkey: *const u16,
        options: u32,
        sam: u32,
        result: *mut Hkey,
    ) -> i32;
    fn RegSetValueExW(
        hkey: Hkey,
        name: *const u16,
        reserved: u32,
        ty: u32,
        data: *const u8,
        cb: u32,
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
    fn RegCloseKey(hkey: Hkey) -> i32;
}

/// The delegation key, exactly as conhost spells it. The doubled `%` is
/// LITERAL — see the module docs. Written as a raw string so no layer of
/// escaping can quietly halve it.
pub(crate) const STARTUP_KEY: &str = r"Console\%%Startup";
/// Value naming the console HOST implementation to delegate to.
pub(crate) const VALUE_CONSOLE: &str = "DelegationConsole";
/// Value naming the TERMINAL implementation to delegate to.
pub(crate) const VALUE_TERMINAL: &str = "DelegationTerminal";

/// aterm's terminal-side CLSID — the class conhost would `CoCreateInstance` and
/// call `EstablishPtyHandoff` on. Freshly minted for aterm and STABLE from here
/// on: it is written into a user's registry, so changing it later would orphan
/// every machine that registered the old one.
pub(crate) const CLSID_ATERM_TERMINAL: &str = "{02E7D4D8-6B51-4BB2-84CE-5949B78B2308}";

/// aterm's console-host CLSID slot.
///
/// **Deliberately unused, and the second blocker after the proxy/stub.**
/// `DelegationConsole` names the console HOST, not the terminal: Windows
/// Terminal points it at the `OpenConsole.exe` it ships. aterm ships no console
/// host today (that is the separate §5.4 "own the console host" work), so the
/// correct value here is either aterm's own sideloaded OpenConsole once that
/// lands, or the inbox host's well-known CLSID. **Which of those, and the inbox
/// host's literal GUID, are NOT established** — and a wrong CLSID in this value
/// is precisely the failure that stops consoles from opening. So this constant
/// exists to name the hole rather than to fill it: nothing WRITES it. It is read
/// only by [`delegation_owner`], to recognize our own half-state if a future
/// wave ever does write it.
pub(crate) const CLSID_ATERM_CONSOLE: &str = "{17A55D2B-8FC4-4990-8805-617293BD5A4D}";

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Is a COM handoff server actually able to answer `EstablishPtyHandoff`?
///
/// **`false`, and honestly so.** Registration is gated on this and there is no
/// build configuration that flips it yet, which is what keeps this whole module
/// inert. Writing the delegation CLSIDs while nothing answers would not merely
/// fail to work — conhost would hand every new console to a class that cannot
/// be created, which is the "consoles stop opening" outcome. Opt-in or not, that
/// is not a state a user can be allowed to reach by typing a flag.
///
/// The blocker is NOT the vtable. That was established from the shipping
/// `OpenConsoleProxy.dll` (see `docs/NATIVE_WINDOWS_DESIGN.md` §5). It is
/// MARSHALLING: `EstablishPtyHandoff` is a cross-process call whose parameters
/// are `system_handle`s, so the IIDs need a registered proxy/stub. On this
/// machine none of the five handoff IIDs, and neither of Windows Terminal's two
/// delegation CLSIDs, appear anywhere in `HKCR\Interface` or `HKCR\CLSID` —
/// they resolve only through the PACKAGED-COM catalog its MSIX identity
/// provides. An unpackaged `aterm.exe` therefore cannot borrow WT's proxy and
/// has none of its own; supplying one means shipping a MIDL-generated
/// proxy/stub DLL (or acquiring MSIX identity), which is its own project.
///
/// Until then the receiving half — `aterm_pty::adopt_handoff`, the signal-pipe
/// resize, the broker's Wake plumbing — is built, tested and waiting.
#[must_use]
pub(crate) fn handoff_server_available() -> bool {
    false
}

/// Open a delegation key for reading. `Ok(None)` = the key does not exist
/// (a clean machine that has never chosen a terminal).
///
/// Takes the subkey so the tests can drive the whole read/ownership/remove
/// pipeline against a SCRATCH key instead of the live console settings — the
/// ownership guard below is the one thing in this crate that must never be
/// tested by writing `HKCU\Console\%%Startup`.
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

/// The current delegation pair: `(DelegationConsole, DelegationTerminal)`.
/// `None` for a value that is absent or not a string — which is the stock state
/// and means "Windows decides" (i.e. conhost).
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

/// Who the delegation values belong to. The whole point of naming this is that
/// [`unset_default_terminal`] must never touch a registration it did not write:
/// `HKCU\Console\%%Startup` is a SHARED, machine-wide console setting, and the
/// app that owns it right now is usually somebody else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DelegationOwner {
    /// Neither value names a terminal — the stock "Let Windows decide" state.
    Unregistered,
    /// aterm wrote this (or would have): ours to remove.
    Aterm,
    /// Another terminal (Windows Terminal, a third-party host) owns it.
    Other,
}

/// Classify a delegation pair. PURE, so the guard that protects a third party's
/// setting is decided by a function with no registry in it.
///
/// `DelegationTerminal` is the deciding value — it names the terminal, and it
/// is the only one [`set_default_terminal`] writes. A pair whose terminal slot
/// is empty but whose console slot is aterm's is still ours (the half-state a
/// future console-host wave could produce); anything else with a value in it
/// belongs to someone else.
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

/// What [`unset_default_terminal`] actually did — so the CLI can say something
/// TRUE. The old `bool` could not distinguish "removed aterm's registration"
/// from "deleted whatever was there", which is exactly how the CLI came to
/// report a foreign terminal's removal as its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnsetOutcome {
    /// Nothing was registered; nothing was written.
    NothingRegistered,
    /// aterm's registration was removed.
    Removed,
    /// Another terminal owns the delegation. **Nothing was changed.**
    NotOurs {
        console: Option<String>,
        terminal: Option<String>,
    },
}

/// Register aterm as the default terminal.
///
/// # Errors
/// `Unsupported` while [`handoff_server_available`] is false — which is always,
/// today. The gate is checked FIRST, before any key is created, so a refusal
/// leaves the registry byte-for-byte untouched.
pub(crate) fn set_default_terminal() -> io::Result<()> {
    if !handoff_server_available() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this build has no COM handoff server, so registering aterm as the default \
             terminal would point conhost at a class nothing can create — every new \
             console would fail to open. Refusing to write the delegation keys.",
        ));
    }
    // Unreachable today, and DELIBERATELY INCOMPLETE: only `DelegationTerminal`
    // is written. `DelegationConsole` is the second blocker (see
    // `CLSID_ATERM_CONSOLE` — aterm ships no console host and the inbox host's
    // CLSID is not established), so flipping `handoff_server_available` alone
    // would produce exactly the half-registration the module docs warn about.
    // The gate is NOT the only thing the next wave has to flip; the console slot
    // has to be filled in here too.
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

/// Remove **aterm's** delegation registration, restoring "Let Windows decide".
///
/// NEVER GATED on [`handoff_server_available`]: clearing the delegation is the
/// escape hatch from a registration that points at a class nothing can create —
/// the one failure mode where new consoles stop opening at all — so it must work
/// in every build, including one whose registering exe is already gone.
///
/// **GUARDED ON OWNERSHIP, which is not the same thing.** `HKCU\Console\%%Startup`
/// is Windows' own key and the values in it are usually SOMEBODY ELSE'S: a user
/// running Windows Terminal who types `aterm-gui --unset-default-terminal` — to
/// see what it does, or on advice — must not silently lose their setting. So a
/// pair we did not write is left byte-for-byte alone and reported as
/// [`UnsetOutcome::NotOurs`]; the escape hatch is unaffected, because a
/// delegation that points at ATERM's broken CLSID is by definition ours to
/// clear. (`uninstall.ps1` has always guarded this way; the CLI path did not.)
///
/// Deletes the VALUES, not the key — the key is Windows' and may hold other
/// settings. Missing values are not an error (idempotent).
pub(crate) fn unset_default_terminal() -> io::Result<UnsetOutcome> {
    unset_delegation_at(STARTUP_KEY)
}

/// [`unset_default_terminal`], against an arbitrary subkey (see
/// [`open_startup_read_at`] for why the seam exists).
fn unset_delegation_at(subkey: &str) -> io::Result<UnsetOutcome> {
    // Read FIRST, and decide before anything is opened for writing.
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
    // Both values go: they are a vendor PAIR, and the terminal slot we just
    // matched says the pair is aterm's.
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

    /// The doubled percent is LITERAL. A well-meaning "fix" to a single `%`, or
    /// any escaping layer that halves it, silently disables handoff — the exact
    /// failure the design doc warned about. Pinned against the string found
    /// verbatim inside conhost.exe.
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

    /// Value names are conhost's, verbatim.
    #[test]
    fn delegation_value_names_match_conhost() {
        assert_eq!(VALUE_CONSOLE, "DelegationConsole");
        assert_eq!(VALUE_TERMINAL, "DelegationTerminal");
    }

    /// The CLSID must be registry-shaped: braced, upper-case, 8-4-4-4-12.
    /// A malformed CLSID here is written into a user's registry and is exactly
    /// as bad as a wrong one.
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

    /// THE SAFETY INVARIANT of this whole bundle: while no COM server can
    /// answer, `set_default_terminal` must refuse — never half-register a CLSID
    /// that makes consoles fail to open. If a future change flips
    /// `handoff_server_available` without landing a server, this test is the
    /// thing that fails.
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

    /// A refusal must be inert: the delegation state before and after must be
    /// identical. Read-only against the real registry — this test never writes.
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

    /// The ownership classifier, exhaustively — this pure function is the whole
    /// guard that keeps `--unset-default-terminal` off another app's setting.
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
            // Our own half-state, if a console-host wave ever writes that slot.
            (Some(CLSID_ATERM_CONSOLE), None, Aterm),
            // THE CASE THIS EXISTS FOR: Windows Terminal is the default.
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

    /// A scratch key under our own name, so the removal path can be driven end
    /// to end without ever writing the live console settings. Deleted on drop,
    /// including on panic.
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
            // Take the parent too, but only with `RegDeleteKeyW`, which FAILS
            // when the key still has subkeys — so a sibling test's scratch key
            // running in parallel can never be swept up with ours.
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

    /// THE MAJOR THIS FIXES, driven end to end against a real (scratch) key:
    /// `--unset-default-terminal` used to `RegDeleteValueW` both delegation
    /// values with no ownership check, so a user with Windows Terminal as their
    /// default silently lost that setting — and was told aterm's registration
    /// had been removed. A foreign pair must now survive BYTE FOR BYTE, and only
    /// aterm's own registration may be removed.
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

        // 3. aterm owns it. Now — and only now — both values go.
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

    /// The removal path must never consult the availability gate: it is the
    /// escape hatch from a delegation nothing can answer. Behavioural — the gate
    /// is false right now, and unset still works against a live key.
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

    /// Reading the live key must be total: present, absent, or empty are all
    /// answers, never an error the CLI would have to explain.
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
