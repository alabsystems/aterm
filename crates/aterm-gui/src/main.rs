// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thin standalone entry for `aterm-gui` — the windowed terminal lives in the
//! library (`aterm_gui::main_entry`) so the ONE `aterm` binary can serve the
//! window mode; this bin exists for dev builds and local automation
//! (validators) and is NOT shipped (the bundle
//! carries an `aterm-gui` argv0 symlink onto `aterm`).

// Windows GUI subsystem, so Explorer/Start-menu launches open no stray console;
// `attach_parent_console` in `main_entry` reattaches stdio when run from a console.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    // The window's tethered helpers start as this image's own tether verb, as the front door's.
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "this dev-only image is the window and nothing else; it is never shipped"
    )]
    aterm_gui::main_entry(std::env::args_os().skip(1).collect());
    std::process::ExitCode::SUCCESS
}
