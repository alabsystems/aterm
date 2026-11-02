// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drives a real window's display link through the deck's clock (`run_frame_clock_drive`):
//! real ticks at the display's period that target the coming vsync and never stall, a link kept
//! paused (by its own `-isPaused`) firing nothing while the clock's hang detector trips and the
//! next tick resuming it, ticks while the main run loop runs in `NSEventTrackingRunLoopMode`, and
//! the close invalidating every link the window's view made.

/// The drive could not execute here; not a pass.
#[cfg(not(all(target_os = "macos", debug_assertions)))]
const NOT_RUN: u8 = 2;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-frame-clock-drive: NOT RUN — this drives \
         crates/aterm-window/src/platform_impl/macos/frame_clock.rs, a CADisplayLink, which \
         does not exist off macOS."
    );
    std::process::ExitCode::from(NOT_RUN)
}

#[cfg(all(target_os = "macos", not(debug_assertions)))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-frame-clock-drive: NOT RUN — built without debug assertions: the drive takes the \
         window's display links through aterm-window's debug seams frame_clock_link and \
         frame_clock_links. Build it as verify appkit does \
         (`targo --unverified build --profile test -p aterm-gui --example objc_frame_clock_drive`)."
    );
    std::process::ExitCode::from(NOT_RUN)
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(u8::try_from(aterm_gui::run_frame_clock_drive()).unwrap_or(1))
}
