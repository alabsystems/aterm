// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `objc_toolbar_drive` — drives `toolbar.rs`'s declared classes and binding
//! sites on a real `NSWindow`.
//!
//! An example rather than a `#[test]`: libtest runs tests off the main thread,
//! where `EventLoop` construction panics. The body lives in
//! `aterm_gui::toolbar_drive` (the types it needs are library-private); this
//! file supplies the main thread and the exit code (`0` pass, `1` finding,
//! `2` not run, `3` hung; see `aterm_verify::stages::OBJC_DRIVES`).

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
