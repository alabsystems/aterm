// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact-bit pins measured on Apple silicon, waived on x86_64 macOS.
//!
//! Some tests pin a `u64` fold of a render (`cursor_glow`'s `DELETION_GOLDENS` and
//! `FLAT_GOLDENS`, the rainbow kitty voice's `LETTER_PATH_FOLD`, the music box's
//! `ORACLE_SCRIPT_FOLD` and `BRRRRING_FOLD`) whose script runs through libm
//! transcendentals (`powf`, `hypot`, `asin`, `atan2`, `exp`, `sin`, `cos`). Their last
//! bits need not agree between libms, and on x86_64 macOS they do not: there a
//! mismatch prints one stderr line instead of failing, and [`deterministic_on_x86_64`]
//! still demands that a second render reproduce the same bits. Everywhere else each
//! helper is the plain assertion.

use core::fmt;

/// `assert_eq!(got, want, "{message}")` against a pin measured on Apple
/// silicon, on every target but x86_64 macOS.
#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
#[track_caller]
pub(crate) fn assert_pinned(
    _test: &str,
    _row: &str,
    got: u64,
    want: u64,
    message: fmt::Arguments<'_>,
) {
    assert_eq!(got, want, "{message}");
}

/// x86_64 macOS: a mismatch against the Apple silicon pin prints its `report` line
/// and fails nothing.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
pub(crate) fn assert_pinned(
    test: &str,
    row: &str,
    got: u64,
    want: u64,
    _message: fmt::Arguments<'_>,
) {
    report(test, row, got, want);
}

/// Whether `got` moved off its Apple silicon pin `want` in a way that must
/// fail the caller — exactly `got != want` on every target but x86_64 macOS.
#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
pub(crate) fn moved(_test: &str, _row: &str, got: u64, want: u64) -> bool {
    got != want
}

/// x86_64 macOS: a mismatch prints its `report` line and is not a failure.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
pub(crate) fn moved(test: &str, row: &str, got: u64, want: u64) -> bool {
    report(test, row, got, want);
    false
}

/// Every target but x86_64 macOS: nothing, and `again` is never called (the
/// pin itself proves the script deterministic there).
#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
pub(crate) fn deterministic_on_x86_64<T: PartialEq + fmt::Debug>(
    _what: &str,
    _got: &T,
    _again: impl FnOnce() -> T,
) {
}

/// x86_64 macOS, where the pin is waived: `again()`, a second independent
/// render of the pinned script, must reproduce `got` exactly.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
#[track_caller]
pub(crate) fn deterministic_on_x86_64<T: PartialEq + fmt::Debug>(
    what: &str,
    got: &T,
    again: impl FnOnce() -> T,
) {
    assert_eq!(
        &again(),
        got,
        "{what} rendered different bits a second time: nondeterministic, which no platform excuses"
    );
}

/// The one stderr line an x86_64 macOS run prints per moved row. Written to the
/// stderr handle, not `eprintln!` (which libtest captures for passing tests),
/// so a waived pin shows in every run.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
fn report(test: &str, row: &str, got: u64, want: u64) {
    use std::io::Write as _;
    if got != want {
        let line = format!(
            "{test}: {row}: got {got} ({got:#018x}) want {want} ({want:#018x}) — not asserted on \
             x86_64 macOS: pinned on Apple silicon, and this libm need not match Apple silicon's \
             last bits (arm64_pin.rs records what was measured where)\n"
        );
        let _ = std::io::stderr().write_all(line.as_bytes());
    }
}
