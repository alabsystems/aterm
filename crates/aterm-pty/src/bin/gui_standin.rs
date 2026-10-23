// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only Windows GUI-subsystem stand-in that logs its PID and sleeps briefly.

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
    std::thread::sleep(Duration::from_millis(600));
}
