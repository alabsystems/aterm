// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `objc_devtools_helper_drive` — builds the Developer Tools helper panel's
//! AppKit views in no window and captures them (`aterm_gui`'s
//! `devtools_helper_drive`).
//!
//! An example rather than a `#[test]`: libtest runs tests off the main thread,
//! where AppKit's views cannot be built. This file supplies the main thread and
//! the exit code (`0` pass, `1` finding, `2` not run). It is not an
//! `aterm_verify::stages::OBJC_DRIVES` row: run it by hand, with a directory
//! for the PNGs as its one argument (`-- <dir>`; none without it).

/// The drive could not execute here; not a pass.
#[cfg(not(target_os = "macos"))]
const NOT_RUN: u8 = 2;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-devtools-helper-drive: NOT RUN — the helper panel stands beside \
         macOS's System Settings and does not exist off macOS."
    );
    std::process::ExitCode::from(NOT_RUN)
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    let snapshots = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let code = aterm_gui::run_devtools_helper_drive(snapshots.as_deref());
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}
