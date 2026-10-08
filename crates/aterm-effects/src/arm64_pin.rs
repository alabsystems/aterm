// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact-bit pins measured on Apple silicon, and how x86_64 macOS treats them.
//!
//! Several tests pin a `u64` fold of what the engine renders: `cursor_glow`'s
//! `DELETION_GOLDENS` (RainbowKitty and other styles), the flat spelling's
//! `FLAT_GOLDENS`, the rainbow kitty voice's `LETTER_PATH_FOLD`, and the music
//! box's `ORACLE_SCRIPT_FOLD` and `BRRRRING_FOLD`. The values were measured on
//! Apple silicon, and every script runs through libm transcendentals (`powf`,
//! `hypot`, `asin`, `atan2`, `exp`, `sin`, `cos`), where two libms need not
//! agree in the last bit. On an x86_64 Mac (macOS 13.7) these pins miss, with
//! the tree and a Trust-specific compiler difference ruled out; the likely
//! cause is different last bits in the transcendental math (libm, or a
//! target-dependent lowering).
//!
//! So everywhere but x86_64 macOS each helper below is the plain assertion
//! (same comparison, message and failure), written as a pair with the
//! `not(all(target_arch = "x86_64", target_os = "macos"))` arm first. x86_64
//! Linux and Windows keep the assertions. On x86_64 macOS a mismatch prints
//! one stderr line (test, row, got, want, and why it is not asserted) instead
//! of failing, and determinism is still asserted: [`deterministic_on_x86_64`]
//! renders the pinned script again and demands the same bits.

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

/// The one line an x86_64 macOS run prints per moved row: where the pin was
/// measured and why this host does not assert it, claiming no cause. Written
/// to the stderr handle rather than via `eprintln!` (which libtest captures
/// for passing tests), so a waived pin shows in every run.
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
