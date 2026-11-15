// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A typed compiler stand-in that records admission inputs and fills both captured pipes.

use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const WORLD: &str = "ATPKG_CAPTURE_TEST_WORLD";
const BYTES: usize = 256 << 10;

pub fn entry() -> Option<ExitCode> {
    let mut args = std::env::args_os().skip(1);
    if args.next()?.as_os_str() != "__captured_compiler" {
        return None;
    }
    Some(match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("captured compiler fixture: {error}");
            ExitCode::FAILURE
        }
    })
}

fn run(args: impl Iterator<Item = std::ffi::OsString>) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "expected positive --jobs=N");
    let mut jobs = std::env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| "1".to_owned());
    for arg in args {
        jobs = arg
            .to_str()
            .and_then(|arg| arg.strip_prefix("--jobs="))
            .ok_or_else(invalid)?
            .to_owned();
    }
    let jobs: u32 = jobs.parse().ok().filter(|&n| n > 0).ok_or_else(invalid)?;
    let root = PathBuf::from(std::env::var_os(WORLD).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the owned world is missing")
    })?);
    std::fs::write(
        root.join("cover"),
        std::env::var(aterm_capacity::COVER_ENV).unwrap_or_default(),
    )?;
    std::fs::write(root.join("jobs"), jobs.to_string())?;
    std::fs::write(root.join("started"), std::process::id().to_string())?;
    if root.join("gated").exists() {
        wait_for_parent(&root)?;
    }
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    std::fs::write(root.join("input"), input)?;
    std::io::stdout().write_all(&vec![b'o'; BYTES])?;
    std::io::stderr().write_all(&vec![b'e'; BYTES])?;
    std::io::stdout().write_all(b"\n{\"reason\":\"build-finished\",\"success\":true}\n")
}

fn wait_for_parent(root: &Path) -> io::Result<()> {
    let began = Instant::now();
    while !root.join("go").exists() {
        if began.elapsed() >= Duration::from_secs(60) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "hang detector: the parent never observed the claim",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
