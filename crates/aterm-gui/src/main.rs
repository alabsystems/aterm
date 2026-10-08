// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thin standalone entry for `aterm-gui` — the windowed terminal lives in the
//! library (`aterm_gui::main_entry`) so the ONE `aterm` binary can serve the
//! window mode; this bin exists for dev builds and local automation
//! (perf-arena, validators) and is NOT shipped (the bundle
//! carries an `aterm-gui` argv0 symlink onto `aterm`).

// Windows GUI subsystem, so Explorer/Start-menu launches open no stray console;
// `attach_parent_console` in `main_entry` reattaches stdio when run from a console.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    aterm_gui::main_entry(std::env::args_os().skip(1).collect());
}
