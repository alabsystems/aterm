// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A GUI-subsystem program for this crate's Windows tests (dev-only, never
//! shipped): it appends `ran <pid>` to the file its first argument names, stays
//! up a moment and exits, with no window. Windows connects no console to a GUI
//! image, so a tab that runs one (`aterm -e explorer .`) must neither wait for
//! its console host to answer it nor start it a second time
//! (`tests/windows_openconsole.rs`).

#![windows_subsystem = "windows"]

use std::io::Write as _;
use std::time::Duration;

fn main() {
    let Some(marker) = std::env::args_os().nth(1) else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker)
    {
        let _ = writeln!(file, "ran {}", std::process::id());
    }
    // Up past a console host's 250 ms health window, as a program the user
    // started would be.
    std::thread::sleep(Duration::from_millis(600));
}
