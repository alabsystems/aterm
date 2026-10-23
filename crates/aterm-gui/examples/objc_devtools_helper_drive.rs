// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless AppKit drive for the Developer Tools helper panel (`run_devtools_helper_drive`).

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
