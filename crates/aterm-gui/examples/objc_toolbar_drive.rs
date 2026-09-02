// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `objc_toolbar_drive` — `toolbar.rs`'s four declared classes and 268 ported
//! binding sites, driven on a real `NSWindow`.
//!
//! AN EXAMPLE AND NOT A `#[test]`, for the same reason its two siblings are:
//! libtest runs every test body on a spawned thread, `pthread_main_np()`
//! answers 0 there, and `EventLoop` construction panics — so only a target
//! owning `fn main` can create the window whose toolbar this drives.
//!
//! THIN, for the same reason `src/bin/aterm-redraw-conformance.rs` is: `mod
//! toolbar`, `Wake` and `WindowId` are all library-private, so the body lives
//! in `aterm_gui::toolbar_drive` and this file supplies only the main thread
//! and the exit code. The contract those codes carry (`0` pass / `1` finding /
//! `2` NOT RUN / `3` hung) is stated there and read by
//! `aterm_verify::stages::toolbar_drive_outcome`.

/// The drive could not execute here. NOT a pass — see the module docs in
/// `aterm_gui::toolbar_drive`.
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
