// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retain an admitted native writer through cooperative cancellation and settlement.

use super::{Command, Output, Path, Tool};
use aterm_uds::deadline::ChildOutput;
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::time::Instant;

const KEEP: usize = 512 * 1024 * 1024;

/// The native work window bounds the cancellation request, never the writer's cleanup.
/// Once started, a timeout or interrupt closes only its control peer and keeps draining until
/// the actual owner returns. Transport damage remains an error even if it later exits zero.
pub(crate) fn run(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: Tool,
    args: &[OsString],
    until: Instant,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, String> {
    match tool {
        Tool::Git => {
            let _held = crate::exec::group::hold();
            super::run_unless(root, git, tool, args, until, stopped)
        }
        Tool::Agh => {
            let mut cmd = super::agh_command(root, true)?;
            cmd.args(args);
            capture(&mut cmd, until, stopped, KEEP).map_err(|why| why.to_string())
        }
    }
}

fn capture(
    cmd: &mut Command,
    until: Instant,
    stopped: &mut dyn FnMut() -> bool,
    keep: usize,
) -> io::Result<Output> {
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
) -> io::Result<Output> {
    let _held = crate::exec::group::hold();
    let (parent, child) = aterm_uds::kept_out(aterm_uds::CtlStream::pair)?;
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
    )?;
    if let Some(why) = damaged.into_inner() {
        return Err(io::Error::other(why));
    }
    let status = status.ok_or_else(|| io::Error::other("native mutation did not start"))?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
#[path = "remote_mutation_tests.rs"]
mod tests;
