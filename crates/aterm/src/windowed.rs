// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-windowed`: the windowed image of the one front door (Windows).
//!
//! A Windows shell keys on a file's PE subsystem: it waits for a console-subsystem child and
//! returns at once from a GUI one. `aterm.exe` (`src/main.rs`) is the console image, so
//! `aterm help`, `aterm ctl …` and every CLI alias hardlinked onto it print inline, pipe and are
//! waited for. This bin includes `main.rs` as a module (one `main`, one routing table) and adds
//! the attribute the Start Menu, the Explorer verb, the jump list and a pinned tile need: no
//! console flash. `build.ps1` ships it as `aterm-gui.exe`; under that argv0 (and its own target
//! name) the router serves the window in-process, and the console image hands every window to
//! this sibling (`main.rs`'s `run_window`). See `docs/NATIVE_WINDOWS_DESIGN.md` §7.
//!
//! Elsewhere it builds as a std-only stub (cargo has no per-target `[[bin]]`): including the
//! front door would add a second whole-program `lto = true` link of the largest binary to every
//! `build --release -p aterm`, for an artifact nothing ships.

// `not(test)`: a GUI-subsystem test harness would have no console to print to.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

// The front door's body: Windows only, and `not(test)` so its tests run once, in the `aterm`
// bin's harness.
#[cfg(all(windows, not(test)))]
#[path = "main.rs"]
mod front_door;

#[cfg(all(windows, not(test)))]
fn main() -> std::process::ExitCode {
    front_door::main()
}

/// Off Windows there is one image, `aterm`, and this name is not it.
#[cfg(all(not(windows), not(test)))]
fn main() -> std::process::ExitCode {
    eprintln!("aterm-windowed is the Windows windowed image; run aterm");
    std::process::ExitCode::from(2)
}
