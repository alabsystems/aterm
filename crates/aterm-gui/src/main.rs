// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thin standalone entry for `aterm-gui` — the windowed terminal lives in the
//! library (`aterm_gui::main_entry`) so the ONE `aterm` binary can serve the
//! window mode; this bin exists for dev builds and local automation
//! (perf-arena, validators, aterm-nest) and is NOT shipped (the bundle
//! carries an `aterm-gui` argv0 symlink onto `aterm`).

// GUI subsystem on Windows: rust binaries default to the CONSOLE subsystem,
// which pops a stray blank console window alongside the terminal on every
// Explorer / Start-menu launch. The library's `attach_parent_console` (first
// thing in `main_entry`) reattaches stdio when launched FROM a console.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    aterm_gui::main_entry(std::env::args_os().skip(1).collect());
}
