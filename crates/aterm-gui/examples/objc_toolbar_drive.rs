// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drives `toolbar.rs`'s declared classes and binding sites on a real `NSWindow`, then reads
//! the menu bar's This Mac row off a real status item.

/// The drive could not execute here; not a pass.
#[cfg(not(target_os = "macos"))]
const NOT_RUN: u8 = 2;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-toolbar-drive: NOT RUN — this drive is about \
         crates/aterm-gui/src/toolbar.rs's macOS tab strip, which does not \
         exist off macOS."
    );
    std::process::ExitCode::from(NOT_RUN)
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(u8::try_from(aterm_gui::run_toolbar_drive()).unwrap_or(1))
}
