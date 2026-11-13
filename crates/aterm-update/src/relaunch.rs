// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Argv construction for relaunching the binary across platforms.

/// Forward this process's argv to a successor minus leading `--window` pins.
pub fn reexec_forwarded_args(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    let mut forwarded = Vec::new();
    let mut leading = true;
    for arg in args {
        if leading && arg == "--window" {
            continue;
        }
        leading = false;
        forwarded.push(arg);
    }
    forwarded
}

/// Whether a forwarded argv (pins stripped) is a windowing verb's
/// (`aterm new-window|new-tab|split-pane …`).
fn starts_with_a_verb(forwarded: &[std::ffi::OsString]) -> bool {
    forwarded
        .first()
        .is_some_and(|first| !first.to_string_lossy().starts_with('-'))
}

/// The argv of the boot swap's re-exec: one leading `--window` pin unless launched with a verb.
pub fn window_successor_args(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    let forwarded = reexec_forwarded_args(args);
    if starts_with_a_verb(&forwarded) {
        return forwarded;
    }
    let mut successor = vec![std::ffi::OsString::from("--window")];
    successor.extend(forwarded);
    successor
}

/// The flag that names a harness's lifeline descriptor: the instance's own spelling
/// where the lifeline exists (unix), and that same word elsewhere, where this module
/// still compiles but no instance takes one.
#[cfg(unix)]
const LIFELINE_FLAG: &str = aterm_uds::lifeline::FLAG;
#[cfg(not(unix))]
const LIFELINE_FLAG: &str = "--lifeline-fd";

/// The flag that names a harness's ready signal: the write end of a pipe a headless
/// instance writes one byte to once its control socket listens (aterm-gui's `--ready-fd`).
const READY_FLAG: &str = "--ready-fd";

/// Environment variable marking an update's relaunch of an already-running window.
pub const RELAUNCH_OF_RUNNING_ENV: &str = "ATERM_RELAUNCH_OF_RUNNING";

/// The argv of a running window's update successor.
pub fn running_window_successor_args(
    args: impl Iterator<Item = std::ffi::OsString>,
    lifeline_fd: Option<i32>,
) -> Vec<std::ffi::OsString> {
    let forwarded = reexec_forwarded_args(args);
    let mut successor = vec![std::ffi::OsString::from("--window")];
    if starts_with_a_verb(&forwarded) {
        return successor;
    }
    let mut tokens = forwarded.into_iter();
    while let Some(arg) = tokens.next() {
        match arg.to_str() {
            Some("-e" | "--command" | "--") => {
                successor.push(arg);
                successor.extend(tokens.by_ref());
            }
            Some(LIFELINE_FLAG) => {
                let _stale = tokens.next();
                if let Some(fd) = lifeline_fd {
                    successor.push(std::ffi::OsString::from(LIFELINE_FLAG));
                    successor.push(std::ffi::OsString::from(fd.to_string()));
                }
            }
            Some(READY_FLAG) => {
                let _said = tokens.next();
            }
            _ => successor.push(arg),
        }
    }
    successor
}

#[cfg(test)]
#[path = "relaunch_tests.rs"]
mod tests;
