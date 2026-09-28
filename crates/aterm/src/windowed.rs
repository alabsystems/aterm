// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-windowed` — the WINDOWED image of the one front door (Windows).
//!
//! The PE subsystem is a header field of ONE FILE, and a Windows shell keys on
//! it: it waits for a console-subsystem child and returns at once from a
//! GUI-subsystem one. `aterm.exe` (built from `src/main.rs`) is the console
//! image, so `aterm help`, `aterm ctl …` and every CLI alias hardlinked onto it
//! print inline, pipe, and are waited for. THIS bin is the same front door —
//! `main.rs` is included below as a module, byte for byte, so there is still
//! exactly one `main` and one routing table — with the one attribute the
//! Start Menu, the Explorer verb, the jump list and a pinned tile need: no
//! console flashes on launch. `build.ps1` ships it as `aterm-gui.exe`; under
//! that argv0 (and under its own target name) the router serves the window
//! in-process, and the console image hands every window it is asked for to
//! this file's sibling copy (`main.rs`'s `run_window`). See
//! `docs/NATIVE_WINDOWS_DESIGN.md` §7 and the header of `main.rs`.
//!
//! On macOS and Linux this target still builds — cargo has no per-target `cfg`
//! for a `[[bin]]` — but as a std-only STUB: including the front door there
//! would make every `build --release -p aterm` (the release cutter's per-arch
//! slice, `tools/dev-app.sh`, `tools/install.sh`, the conformance support
//! build) run a second whole-program `lto = true` link of the largest binary,
//! for an artifact no bundle, release or install list carries.

// `not(test)` on the attribute as well: a test harness is not a window, and a
// GUI-subsystem harness would have no console to print its results to.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

// The front door's body — on Windows only (see the module docs), and under
// `not(test)` so `cargo test -p aterm` runs the tests it carries ONCE, in the
// `aterm` bin's harness, whose root the file is, rather than a second time here.
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
