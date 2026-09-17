// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact-bit pins measured on Apple silicon, and what an x86_64 macOS run does
//! with them.
//!
//! Six tests in this crate pin a `u64` fold of what the engine renders: the
//! RainbowKitty rows and the other styles' rows of `cursor_glow`'s
//! `DELETION_GOLDENS`, the flat spelling's four RainbowKitty rows
//! (`FLAT_GOLDENS`), the rainbow kitty voice's `LETTER_PATH_FOLD`, and the
//! music box's `ORACLE_SCRIPT_FOLD` and `BRRRRING_FOLD`. Every one of those
//! values was measured on an Apple silicon Mac, and every script behind them
//! runs through libm's transcendentals (`powf`, `hypot`, `asin`, `atan2`,
//! `exp`, `sin`, `cos`), where nothing requires two libms to agree in the last
//! bit. On an x86_64 Mac (macOS 13.7) the first five miss on untouched main
//! `8d0dae0e4`; `FLAT_GOLDENS` was set upstream on 2026-09-15 and missed on the
//! same host on all four rows the first time it ran there (main `92965189e`,
//! the same engine and the same measured reason as the deletion goldens' kitty
//! rows, which it moves with). What was probed on that host, and where:
//!
//! - The four RainbowKitty rows and `LETTER_PATH_FOLD` already miss at
//!   `387e25337`, the commit that set them, with the values main reads.
//! - The other styles' test misses at `387e25337` at its first row, Lumen
//!   dark, with the value main reads. Those rows were set at `2db804c3b`,
//!   except Fire dark, re-baked in the merge `58cdba648`; neither was probed, and `assert_eq!` stops at the first miss, so the
//!   other seven were first seen missing through this module's report, on
//!   main's engine.
//! - The music box's two folds already missed at `4b77cf17b`, which
//!   introduced them, against the values they carried then. Their current
//!   values come from later re-bakes, `BRRRRING_FOLD`'s from `a6567f1f8` and
//!   `ORACLE_SCRIPT_FOLD`'s from the merge `82eacd94e`, and neither was
//!   probed.
//! - Stock stable 1.98.1 was run only on the two `cursor_glow` tests at
//!   `387e25337`, and read the Trust toolchain's values for every row it
//!   reached: the four kitty rows and Lumen dark. `LETTER_PATH_FOLD` and the
//!   music box folds ran under Trust only.
//!
//! That rules out the tree for the kitty rows and `LETTER_PATH_FOLD`, and a
//! Trust-specific compiler difference for the kitty rows and Lumen dark — both
//! compilers share LLVM's x86_64 backend, so a lowering that differs by target
//! (`powi` needs no libm) stays open. Every miss is consistent with this
//! x86_64 / macOS 13 host computing different last bits from the pinning
//! host's in the transcendental math, through its libm or such a lowering;
//! outside those probes that is the likely cause, not a measured one.
//!
//! So everywhere but x86_64 macOS — the one host measured — each helper below
//! IS the assertion those tests
//! always made — same comparison, same message, same failure — and each is
//! written as a pair, the `not(all(target_arch = "x86_64", target_os =
//! "macos"))` arm first, so that is readable at a glance. x86_64 Linux and
//! Windows keep upstream's assertions: nothing was measured there. On x86_64
//! macOS a mismatch against an arm64 pin prints one
//! stderr line (test, row, got, want, and why it is not asserted) instead of
//! failing, and what the tests prove beyond the arm64 bits still holds there,
//! determinism above all: [`deterministic_on_x86_64`] renders the pinned
//! script a second time and demands the same bits, so a nondeterminism
//! regression still goes red.

use core::fmt;

/// `assert_eq!(got, want, "{message}")` against a pin measured on Apple
/// silicon — the assertion the caller made before this module existed,
/// unchanged, on every target but x86_64 macOS.
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

/// Every target but x86_64 macOS: nothing, and `again` is never called — the pin
/// itself proves the script deterministic there, so those runs are exactly
/// what they were.
#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
pub(crate) fn deterministic_on_x86_64<T: PartialEq + fmt::Debug>(
    _what: &str,
    _got: &T,
    _again: impl FnOnce() -> T,
) {
}

/// x86_64 macOS, where the pin is waived: `again()` — a second, independent render
/// of the pinned script — must reproduce `got` exactly. A pin proves
/// determinism as well as bits, and no platform excuses losing the first.
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

/// The one line an x86_64 macOS run prints per moved row. It names where the
/// pin was measured and why this host does not assert it, and claims no cause:
/// the same line prints for any miss here, a regression or another host
/// included, so what was measured where lives in the module docs above.
/// Written to the stderr handle rather than through `eprintln!`, which
/// libtest captures and drops for a passing test: a waived pin must show in
/// every run, not only under `--nocapture`.
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
