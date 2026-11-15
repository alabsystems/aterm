// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Debug-only discovery holds, following `ATERM_DEBUG_LOOP_BUILD_HOLD`'s marker/release pattern.
//! A requested reader waits once, on its own worker, in an admitted private lab window only.
//! The named directory must already exist under that lab's HOME. For reader `claude` or
//! `fleet`, a fresh `<reader>.held` names the PID, `<reader>.release` must contain `release\n`,
//! and only that observed release produces `<reader>.passed`. A one-minute hang detector
//! produces `<reader>.timed-out` instead. No shipped build compiles this module.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::sync::Once;
use std::time::{Duration, Instant};

use aterm_uds::private_file::{DirectoryView, PrivateDirectory, PrivateFileError};

const HANG: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);

pub(super) fn claude() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        requested(
            "claude",
            aterm_types::dev_seam!("ATERM_DEBUG_CLAUDE_DISCOVERY_HOLD"),
        );
    });
}

pub(super) fn fleet() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        requested(
            "fleet",
            aterm_types::dev_seam!("ATERM_DEBUG_FLEET_DISCOVERY_HOLD"),
        );
    });
}

fn requested(reader: &str, path: Option<OsString>) {
    let Some(path) = path else {
        return;
    };
    let result = (|| {
        if !crate::lab_window_custody::present()
            || !aterm_types::dirs::runs_under_private_state_root()
        {
            return Err(io::Error::other(
                "discovery hold requires a private lab window",
            ));
        }
        let home = aterm_types::dirs::home_dir()
            .ok_or_else(|| io::Error::other("the lab HOME is unavailable"))?;
        let directory = open_gate(Path::new(&path), &home)?;
        wait_at(&directory, reader, HANG)
    })();
    match result {
        Ok(Outcome::Released) => {
            crate::logging::stderr_line!("discovery hold: {reader} released");
        }
        Ok(Outcome::TimedOut) => {
            crate::logging::stderr_line!("discovery hold: {reader} TIMED OUT");
        }
        Err(error) => {
            crate::logging::stderr_line!("discovery hold: {reader} REFUSED: {error}");
        }
    }
}

fn open_gate(path: &Path, home: &Path) -> io::Result<PrivateDirectory> {
    if !path.is_absolute() {
        return Err(io::Error::other("discovery hold directory is not absolute"));
    }
    let home = home.canonicalize()?;
    let resolved = path.canonicalize()?;
    if resolved == home || !resolved.starts_with(&home) {
        return Err(io::Error::other(
            "discovery hold directory is outside the lab HOME",
        ));
    }
    let directory = PrivateDirectory::open(path)?;
    if !directory.is_at(&resolved)? {
        return Err(io::Error::other(
            "discovery hold directory changed during admission",
        ));
    }
    Ok(directory)
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Released,
    TimedOut,
}

fn released(view: &DirectoryView<'_>, reader: &str) -> io::Result<bool> {
    match view.read(&format!("{reader}.release"), 64) {
        Ok(bytes) if bytes == b"release\n" => Ok(true),
        Ok(_) => Err(io::Error::other("discovery release has the wrong contents")),
        Err(PrivateFileError::Missing) => Ok(false),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn wait_at(directory: &PrivateDirectory, reader: &str, bound: Duration) -> io::Result<Outcome> {
    let held = format!("{reader}.held");
    let release = format!("{reader}.release");
    let passed = format!("{reader}.passed");
    let timed_out = format!("{reader}.timed-out");
    let marker = format!("pid={} reader={reader}\n", std::process::id());
    let until = Instant::now() + bound;
    let view = directory.view()?;
    for name in [&held, &release, &passed, &timed_out] {
        match view.read(name, 256) {
            Err(PrivateFileError::Missing) => {}
            Ok(_) => return Err(io::Error::other("discovery hold marker already exists")),
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    view.write(&held, marker.as_bytes())?;
    loop {
        let view = directory.view()?;
        if Instant::now() >= until {
            view.write(&timed_out, marker.as_bytes())?;
            return Ok(Outcome::TimedOut);
        }
        if released(&view, reader)? {
            view.write(&passed, marker.as_bytes())?;
            return Ok(Outcome::Released);
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
#[path = "summarize_discovery_hold_tests.rs"]
mod tests;
