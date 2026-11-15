// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A live process whose parent can inspect its session before releasing stdin.

use std::io::{self, Write as _};
use std::process::ExitCode;

pub fn entry() -> Option<ExitCode> {
    let report = std::env::var_os("ATPKG_WARM_TEST_REPORT")?;
    Some(match run(std::path::Path::new(&report)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("warm session fixture: {error}");
            ExitCode::FAILURE
        }
    })
}

fn run(report: &std::path::Path) -> io::Result<()> {
    // SAFETY: `getsid(0)` reads this process's own session.
    let session = unsafe { libc::getsid(0) };
    if session < 0 {
        return Err(io::Error::last_os_error());
    }
    std::fs::write(report, format!("{} {session}\n", std::process::id()))?;
    println!("warm-session-ready");
    io::stdout().flush()?;
    io::copy(&mut io::stdin().lock(), &mut io::sink())?;
    Ok(())
}
