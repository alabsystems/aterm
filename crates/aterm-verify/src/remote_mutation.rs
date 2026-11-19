// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retain an admitted native writer through cooperative cancellation and settlement.

use super::{Command, Failed, Output, Path, Tool};
use aterm_uds::deadline::{ChildOutput, CompleteOutputError, RunError};
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::time::Instant;

const KEEP: usize = 512 * 1024 * 1024;

/// The native work window bounds the cancellation request, never the writer's cleanup.
/// Once started, a timeout or interrupt closes only its control peer and keeps draining until
/// the actual owner returns. Transport damage remains an error even if it later exits zero.
/// The landing's watchdog is held from before `stopped` is first read until the owner returns,
/// and no child starts once `stopped` reads true or the window has ended: such a write, like
/// one whose command could not be made or started, sent nothing. A local repository's git,
/// which opens no intent, is ended at the window or the interrupt.
///
/// # Errors
/// [`Failed::Unstarted`] where no child started, so nothing was sent; [`Failed::Ended`], of
/// unknown outcome, where one started and its output was not whole, or it was ended or asked to
/// stop.
pub(crate) fn run(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: Tool,
    args: &[OsString],
    until: Instant,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, Failed> {
    let _held = crate::exec::group::hold();
    let refused =
        |why: &str| Failed::Unstarted(format!("{tool:?} remote write: {why}; nothing was sent"));
    if stopped() {
        return Err(refused("interrupted before the write started"));
    }
    if Instant::now() >= until {
        return Err(refused("its work window ended before the write started"));
    }
    match tool {
        Tool::Git => {
            let mut cmd = super::command(root, git, tool, args)
                .map_err(|why| refused(&format!("could not be made: {why}")))?;
            aterm_uds::deadline::output_until_complete_stdout_from_unless(
                &mut cmd,
                aterm_uds::spawnfd::Stdio::null(),
                until,
                stopped,
            )
            .map_err(|error| match error {
                CompleteOutputError::Run(RunError::Spawn(_)) => refused(&error.to_string()),
                _ => Failed::Ended(format!("{tool:?} remote operation: {error}")),
            })
        }
        Tool::Agh => {
            let mut cmd = super::agh_command(root, true)
                .map_err(|why| refused(&format!("could not be made: {why}")))?;
            cmd.args(args);
            capture(&mut cmd, until, stopped, KEEP)
        }
    }
}

fn capture(
    cmd: &mut Command,
    until: Instant,
    stopped: &mut dyn FnMut() -> bool,
    keep: usize,
) -> Result<Output, Failed> {
    capture_with(cmd, until, stopped, keep, &mut Instant::now, &mut |_, _| {
        Ok(())
    })
}

fn capture_with(
    cmd: &mut Command,
    until: Instant,
    stopped: &mut dyn FnMut() -> bool,
    keep: usize,
    now: &mut dyn FnMut() -> Instant,
    observe: &mut dyn FnMut(ChildOutput, &[u8]) -> io::Result<()>,
) -> Result<Output, Failed> {
    let _held = crate::exec::group::hold();
    let (parent, child) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).map_err(|why| {
        Failed::Unstarted(format!(
            "native mutation control could not be made: {why}; nothing was sent"
        ))
    })?;
    let parent = RefCell::new(Some(parent));
    let close = || {
        parent.borrow_mut().take();
    };
    let started = Cell::new(false);
    let damaged = RefCell::new(None::<String>);
    let cancel = |why: &str| {
        if damaged.borrow().is_none() {
            *damaged.borrow_mut() = Some(why.to_owned());
        }
        close();
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut retire = close;
    let status = aterm_uds::deadline::stream_with_stdin_until_stopped(
        cmd,
        aterm_uds::spawnfd::Stdio::from(OwnedFd::from(child)),
        None,
        |_| started.set(true),
        |pipe, bytes| {
            observe(pipe, bytes)?;
            if damaged.borrow().is_some() {
                return Ok(());
            }
            let out = match pipe {
                ChildOutput::Stdout => &mut stdout,
                ChildOutput::Stderr => &mut stderr,
            };
            if bytes.len() > keep.saturating_sub(out.len()) {
                cancel("native mutation output exceeds its capture limit");
            } else {
                out.extend_from_slice(bytes);
            }
            Ok(())
        },
        || {
            if stopped() {
                cancel("native mutation interrupted");
            } else if now() >= until {
                cancel("native mutation work deadline expired");
            }
            damaged.borrow().is_some() && !started.get()
        },
        Some(&mut retire),
    );
    // Whether the child started decides what a write that gave no whole output means.
    let started = started.get();
    let failed = |why: String| {
        if started {
            Failed::Ended(why)
        } else {
            Failed::Unstarted(format!("{why}; nothing was sent"))
        }
    };
    let status = status.map_err(|why| failed(why.to_string()))?;
    if let Some(why) = damaged.into_inner() {
        return Err(failed(why));
    }
    let status = status.ok_or_else(|| failed("native mutation did not start".to_owned()))?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
#[path = "remote_mutation_tests.rs"]
mod tests;
