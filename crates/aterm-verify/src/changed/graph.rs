// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One original dependency-graph child, with bounded, cancellable capture.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{ExitStatus, Output};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Command, command};

pub(super) struct Read {
    command: String,
    output: Output,
}

impl Read {
    pub(super) fn refusal(&self, why: &str) -> String {
        diagnostic(
            &self.command,
            why,
            Some(self.output.status),
            &self.output.stdout,
            &self.output.stderr,
        )
    }

    pub(super) fn text(&self) -> Result<&str, String> {
        std::str::from_utf8(&self.output.stdout).map_err(|_| self.refusal("stdout is not UTF-8"))
    }
}

// Debug quoting retains every byte, including non-UTF-8, rather than replacing
// it with U+FFFD. These are this child's original streams, never a second call.
fn bytes(value: &[u8]) -> String {
    match std::str::from_utf8(value) {
        Ok(text) => format!("{text:?}"),
        Err(_) => value
            .iter()
            .flat_map(|byte| std::ascii::escape_default(*byte))
            .map(char::from)
            .collect(),
    }
}

fn diagnostic(
    command: &str,
    why: &str,
    status: Option<ExitStatus>,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    let status = status.map_or_else(|| "not observed".to_owned(), |s| s.to_string());
    format!(
        "{command}: {why}; natural exit: {status}\nstdout ({} bytes): {}\nstderr ({} bytes): {}",
        stdout.len(),
        bytes(stdout),
        stderr.len(),
        bytes(stderr)
    )
}

pub(super) fn tree(root: &Path, targo: &Path, path: &OsStr, args: &[&str]) -> Result<Read, String> {
    let mut cmd = command(targo);
    cmd.args(args)
        .args(["--color", "never"])
        .current_dir(root)
        .env("PATH", path);
    capture(&mut cmd, crate::engine::PATIENCE, &mut crate::exec::ending)
}

#[cfg(unix)]
fn capture(
    cmd: &mut Command,
    within: Duration,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Read, String> {
    capture_when(cmd, within, &mut |_, _| stopped())
}

#[cfg(unix)]
fn capture_when(
    cmd: &mut Command,
    within: Duration,
    stopped: &mut dyn FnMut(&[u8], &[u8]) -> bool,
) -> Result<Read, String> {
    use aterm_uds::deadline::{
        ChildOutput, MAX_COMPLETE_STDOUT_BYTES, stream_until_stopped_observing_exit,
    };
    use std::cell::{Cell, RefCell};
    let shown = format!("{cmd:?}");
    let started = Cell::new(false);
    let cancelled = Cell::new(false);
    let mut observed = None;
    let streams = RefCell::new([Vec::new(), Vec::new()]);
    let result = stream_until_stopped_observing_exit(
        cmd,
        Some(Instant::now() + within),
        |_| started.set(true),
        |stream, chunk| {
            let mut streams = streams.borrow_mut();
            let target = match stream {
                ChildOutput::Stdout => &mut streams[0],
                ChildOutput::Stderr => &mut streams[1],
            };
            if chunk.len() > MAX_COMPLETE_STDOUT_BYTES.saturating_sub(target.len()) {
                return Err(std::io::Error::other(
                    "graph stream exceeds the complete-capture limit; retained bytes are a prefix",
                ));
            }
            target.extend_from_slice(chunk);
            Ok(())
        },
        || {
            let streams = streams.borrow();
            let stop = stopped(&streams[0], &streams[1]);
            cancelled.set(cancelled.get() || stop);
            stop
        },
        |status| observed = Some(status),
    );
    let [stdout, stderr] = streams.into_inner();
    let status = match result {
        Ok(Some(status)) => status,
        Ok(None) => {
            return Err(diagnostic(
                &shown,
                "collection was cancelled; stream completion unproven",
                observed,
                &stdout,
                &stderr,
            ));
        }
        Err(error) => {
            let phase = if cancelled.get() {
                "collection was cancelled; stream completion unproven"
            } else if started.get() {
                "capture failed after child creation; stream completion unproven"
            } else {
                "graph child creation/start was not observed"
            };
            return Err(diagnostic(
                &shown,
                &format!("{phase}: {error}"),
                observed,
                &stdout,
                &stderr,
            ));
        }
    };
    let read = Read {
        command: shown,
        output: Output {
            status,
            stdout,
            stderr,
        },
    };
    if !status.success() {
        return Err(read.refusal("graph child exited unsuccessfully"));
    }
    Ok(read)
}

#[cfg(not(unix))]
fn capture(
    cmd: &mut Command,
    within: Duration,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Read, String> {
    let shown = format!("{cmd:?}");
    let output = aterm_uds::deadline::output_until_complete_stdout_with_limit_unless(
        cmd,
        Instant::now() + within,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        stopped,
    )
    .map_err(|error| {
        format!("{shown}: graph collection failed: {error}; original partial streams unavailable")
    })?;
    let read = Read {
        command: shown,
        output,
    };
    if !read.output.status.success() {
        return Err(read.refusal("graph child exited unsuccessfully"));
    }
    Ok(read)
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
