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
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
    }
    if let Ok(mut file) = opts.open(marker)
        && file.metadata().is_ok_and(|m| m.is_file())
    {
        let _ = writeln!(file, "ran {}", std::process::id());
    }
    std::thread::sleep(Duration::from_millis(600));
}
