// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-windowed`: the Windows GUI-subsystem front-door binary (`aterm-gui.exe`).

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

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
