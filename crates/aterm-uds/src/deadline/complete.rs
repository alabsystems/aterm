// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Additive complete-stdout capture for consumers that replace data or admit an inventory.
//! The older diagnostic capture keeps its tail/grace semantics unchanged.

use super::{
    DRAIN_GRACE, ExitWait, Grace, OUTPUT_KEEP, PipeEnd, RunError, drain, pipe_end, reap_within,
};
use crate::spawnfd::{Child, Command, Stdio};
use std::io::{self, Read};
use std::process::Output;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Why a call requiring complete stdout cannot use the child's output.
#[derive(Debug)]
pub enum CompleteOutputError {
    /// The existing child spawn/wait/deadline refusal.
    Run(RunError),
    /// Reading stdout failed; bytes read before it are not a complete result.
    StdoutRead(io::Error),
    /// At least one stdout byte exceeded the bounded complete capture.
    StdoutTooLarge {
        /// The inclusive maximum complete stdout length.
        limit: usize,
    },
    /// Stdout did not prove EOF by the collection deadline, or its reader disappeared.
    StdoutIncomplete,
}

impl std::fmt::Display for CompleteOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(error) => std::fmt::Display::fmt(error, f),
            Self::StdoutRead(error) => write!(f, "stdout could not be read completely: {error}"),
            Self::StdoutTooLarge { limit } => {
                write!(
                    f,
                    "stdout exceeds the complete-capture limit of {limit} bytes"
                )
            }
            Self::StdoutIncomplete => f.write_str("stdout did not reach proven EOF"),
        }
    }
}

impl std::error::Error for CompleteOutputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Run(error) => Some(error),
            Self::StdoutRead(error) => Some(error),
            Self::StdoutTooLarge { .. } | Self::StdoutIncomplete => None,
        }
    }
}

/// Run with closed stdin and require complete stdout, as
/// [`output_until_complete_stdout_from`]. Stderr remains a bounded diagnostic tail.
///
/// # Errors
/// A child refusal or stdout whose EOF, read or size cannot be vouched for.
pub fn output_until_complete_stdout(
    cmd: &mut Command,
    deadline: Instant,
) -> Result<Output, CompleteOutputError> {
    output_until_complete_stdout_from(cmd, Stdio::null(), deadline)
}

/// Run to the existing child deadline with supplied stdin. Success proves stdout reached
/// EOF without a read error and without discarding any byte, with at most [`OUTPUT_KEEP`]
/// bytes (exactly that many is admitted). No partial stdout is returned on refusal.
///
/// Child group/deadline handling matches [`super::output_until_from`], except that a
/// refusal waits at most one 60-second reserve to reap after killing. On Unix the
/// dropped spawnfd handle hands a child still unsettled to the existing reaper;
/// returning a refusal does not certify child or descendant exit. After child exit,
/// stdout may finish for the existing [`DRAIN_GRACE`]; an open descendant-held stream is a
/// refusal, not a complete empty/short result. Past the grace a pipe no descendant holds is
/// read on ([`Grace`]). Stderr deliberately keeps the older
/// diagnostic tail behavior; this API does not certify stderr completeness.
///
/// As in the older capture, a reader still blocked after grace can outlive the call on
/// its own owned stream and bounded buffer. This API does not establish descendant exit.
///
/// # Errors
/// [`CompleteOutputError`] for the child's refusal, stdout read failure, overflow or
/// missing EOF. Existing diagnostic capture APIs and their behavior are unchanged.
pub fn output_until_complete_stdout_from(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
) -> Result<Output, CompleteOutputError> {
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CompleteOutputError::Run(RunError::Spawn(error)))?;
    let stdout = match child.stdout.take() {
        Some(stream) => match drain_stdout(pipe_end(&stream), stream) {
            Ok(stdout) => stdout,
            Err(error) => {
                end_bounded(&mut child);
                return Err(CompleteOutputError::Run(RunError::Spawn(error)));
            }
        },
        None => {
            end_bounded(&mut child);
            return Err(CompleteOutputError::StdoutIncomplete);
        }
    };
    let stderr = match child
        .stderr
        .take()
        .map(|stream| {
            let pipe = pipe_end(&stream);
            drain(stream, pipe, "stderr")
        })
        .transpose()
    {
        Ok(stderr) => stderr,
        Err(error) => {
            end_bounded(&mut child);
            return Err(CompleteOutputError::Run(RunError::Spawn(error)));
        }
    };
    let mut exit = ExitWait::new(&child);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    end_bounded(&mut child);
                    return Err(CompleteOutputError::Run(RunError::TimedOut));
                }
                exit.wait(remaining);
            }
            Err(error) => {
                end_bounded(&mut child);
                return Err(CompleteOutputError::Run(RunError::Wait(error)));
            }
        }
    };
    let grace = Grace {
        by: Instant::now() + DRAIN_GRACE,
        until: deadline,
        group: Some(child.id()),
    };
    let stdout = stdout.collect(&grace)?;
    let stderr = stderr
        .map(|stream| stream.collect(&grace))
        .unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Kill as the diagnostic path does, then use the existing bounded reap loop.
/// This reserve is cleanup containment, not a required child response time.
fn end_bounded(child: &mut Child) {
    #[cfg(unix)]
    super::children::kill_group(child);
    #[cfg(not(unix))]
    let _ = child.kill();
    reap_within(child, None, Instant::now() + Duration::from_secs(60));
}

/// A result exists only after the actual Read observed EOF (or an explicit read error).
struct CompleteDrain {
    ended: mpsc::Receiver<Result<Vec<u8>, CompleteOutputError>>,
    pipe: Option<PipeEnd>,
}

impl CompleteDrain {
    /// The stdout, once its reader saw EOF; a refusal when `grace` stops it first.
    fn collect(self, grace: &Grace) -> Result<Vec<u8>, CompleteOutputError> {
        let ended = match self
            .ended
            .recv_timeout(grace.by.saturating_duration_since(Instant::now()))
        {
            Err(mpsc::RecvTimeoutError::Timeout) => match grace.read_on(self.pipe.as_ref()) {
                Some(wait) => self.ended.recv_timeout(wait),
                None => Err(mpsc::RecvTimeoutError::Timeout),
            },
            ended => ended,
        };
        ended.map_err(|_| CompleteOutputError::StdoutIncomplete)?
    }
}

/// The same bounded read loop the strict child path uses, over an injected Read in tests.
/// Continue consuming overflow to EOF so the child's pipe is not left blocked by the cap;
/// the overflow flag is sticky and no retained prefix can be returned as complete.
fn read_stdout(mut stream: impl Read) -> Result<Vec<u8>, CompleteOutputError> {
    let mut bytes = Vec::new();
    let mut too_large = false;
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => {
                return if too_large {
                    Err(CompleteOutputError::StdoutTooLarge { limit: OUTPUT_KEEP })
                } else {
                    Ok(bytes)
                };
            }
            Ok(n) => {
                let keep = n.min(OUTPUT_KEEP.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&chunk[..keep]);
                too_large |= keep != n;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(CompleteOutputError::StdoutRead(error)),
        }
    }
}

// wasm32 never links aterm-uds; the census's wasm closure scans it anyway (OB-12).
#[cfg(not(target_arch = "wasm32"))]
fn drain_stdout(
    pipe: Option<PipeEnd>,
    stream: impl Read + Send + 'static,
) -> io::Result<CompleteDrain> {
    let (finished, ended) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("child-complete-stdout".into())
        .spawn(move || {
            let result = read_stdout(stream);
            let _ = finished.send(result);
        })?;
    Ok(CompleteDrain { ended, pipe })
}

#[cfg(test)]
#[path = "complete_tests.rs"]
mod tests;
