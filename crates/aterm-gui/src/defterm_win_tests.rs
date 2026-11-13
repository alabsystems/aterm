// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}",
        "{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}",
    ] {
        assert!(!CLSID_ATERM_TERMINAL.eq_ignore_ascii_case(wt));
        assert!(!CLSID_ATERM_CONSOLE.eq_ignore_ascii_case(wt));
    }
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
        (None, None, Unregistered),
        (Some(""), Some(""), Unregistered),
        (None, Some(CLSID_ATERM_TERMINAL), Aterm),
        (None, Some("{02e7d4d8-6b51-4bb2-84ce-5949b78b2308}"), Aterm),
        (Some(wt_console), Some(CLSID_ATERM_TERMINAL), Aterm),
        (Some(CLSID_ATERM_CONSOLE), None, Aterm),
        (Some(wt_console), Some(wt_terminal), Other),
        (None, Some(wt_terminal), Other),
        (Some(wt_console), None, Other),
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
/// writing the live console settings.
struct ScratchKey(String);

impl ScratchKey {
    fn new(name: &str) -> Self {
        let path = format!("Software\\aterm-defterm-selftest-{name}");
        let k = Self(path);
        k.delete();
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
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_WRITE, &mut hk) };
        assert_eq!(rc, 0, "opening the scratch key must succeed");
        let n = wide(name);
        let d = wide(value);
        let cb = (d.len() * std::mem::size_of::<u16>()) as u32;
        // SAFETY: open key; buffers outlive the call; `cb` is their byte length.
        let rc = unsafe { RegSetValueExW(hk, n.as_ptr(), 0, REG_SZ, d.as_ptr().cast::<u8>(), cb) };
        // SAFETY: `hk` was opened above.
        unsafe { RegCloseKey(hk) };
        assert_eq!(rc, 0, "writing the scratch value must succeed");
    }

    fn delete(&self) {
        let sub = wide(&self.0);
        // SAFETY: valid NUL-terminated wide subkey under HKCU.
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, sub.as_ptr()) };
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
}

/// Against a scratch key: a foreign pair survives unset byte for byte, and only
/// aterm's own registration is removed, with or without a console value beside it.
/// Removal never consults the availability gate: it is the escape hatch from a
/// delegation nothing can answer.
#[test]
fn unset_spares_a_foreign_registration_and_removes_only_ours() {
    let wt_terminal = "{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}";
    let wt_console = "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}";
    let key = ScratchKey::new("foreign-vs-ours");

    assert_eq!(
        unset_delegation_at(&key.0).expect("unset must not error"),
        UnsetOutcome::NothingRegistered
    );

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

    assert_eq!(
        unset_delegation_at(&key.0).expect("unset must not error"),
        UnsetOutcome::NothingRegistered
    );

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
