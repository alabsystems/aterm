// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Captured compiler output under the ordinary owned-root admission and supervision boundary.

use std::io;
use std::path::Path;
use std::process::{ChildStderr, ChildStdout, Output};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use aterm_uds::spawnfd::Command;

use super::{Admission, Build, Held, Seat, Tool, admit, classify};

/// Run the prepared compiler command with captured output under ordinary build admission.
/// Its actual arguments are classified by `tool`; probes that compile nothing claim nothing.
/// Configured stdio, environment and inherited handles keep [`Command::output`]'s behavior.
/// A claimed build propagates its cover marker, is supervised through its actual owning reap,
/// and settles its metered peak before this returns. This grants no cohort custody.
///
/// # Errors
/// The directory or arguments cannot be read as admission inputs, admission refuses or is
/// interrupted, a pipe reader cannot start, or spawning, reading, joining or waiting fails.
/// A compiling call starts no child before its admission decision and both pipe readers are
/// ready. Its owning wait settles before both readers are joined and a read error is returned.
pub fn output(command: &mut Command, tool: Tool) -> io::Result<Output> {
    let cwd = std::env::current_dir()?;
    let cwd = command
        .get_current_dir()
        .map_or(cwd.clone(), |path| cwd.join(path));
    output_at(command, tool, &cwd, &Seat::live(), &mut |line| {
        eprintln!("{line}")
    })
}

/// [`output`] at the caller's actual working directory and admission seat.
///
/// # Errors
/// As [`output`].
pub fn output_at(
    command: &mut Command,
    tool: Tool,
    cwd: &Path,
    seat: &Seat<'_>,
    say: &mut dyn FnMut(&str),
) -> io::Result<Output> {
    let args = command
        .get_args()
        .map(|arg| {
            arg.to_str().map(str::to_owned).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "compiler arguments are not UTF-8",
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let Some(build) = classify(&args, tool) else {
        return command.output();
    };
    let held = match admit(&build, cwd, seat, say) {
        Admission::Claimed(held) => Some(*held),
        Admission::Covered(_) | Admission::Own(_) => None,
        Admission::Stopped(why) => return Err(io::Error::new(io::ErrorKind::Interrupted, why)),
        Admission::Refused(why) => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, why));
        }
    };
    capture(held, command, say, &build)
}

/// Readers wait for their pipes before the child is born. A failure starting the second reader
/// closes and joins the first without starting work; after birth, neither pipe can fill behind
/// the owning wait. Drop also closes the handoff and joins on an early setup error.
struct Drain<P> {
    pipe: Option<Sender<Option<P>>>,
    thread: Option<JoinHandle<io::Result<Vec<u8>>>>,
}

impl<P: io::Read + Send + 'static> Drain<P> {
    fn new(name: &str) -> io::Result<Self> {
        Self::with_reader(name, |mut pipe| {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes)?;
            Ok(bytes)
        })
    }

    fn with_reader(
        name: &str,
        read: impl FnOnce(P) -> io::Result<Vec<u8>> + Send + 'static,
    ) -> io::Result<Self> {
        let (pipe, incoming) = mpsc::channel();
        let thread =
            std::thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || match incoming.recv() {
                    Ok(Some(pipe)) => read(pipe),
                    Ok(None) | Err(_) => Ok(Vec::new()),
                })?;
        Ok(Self {
            pipe: Some(pipe),
            thread: Some(thread),
        })
    }

    fn hand(&mut self, pipe: Option<P>) {
        if let Some(sender) = self.pipe.take() {
            // A disconnected reader is observed by its owning join below.
            let _ = sender.send(pipe);
        }
    }

    fn finish(mut self) -> io::Result<Vec<u8>> {
        self.pipe.take();
        self.thread
            .take()
            .expect("the reader is joined once")
            .join()
            .map_err(|_| io::Error::other("compiler output reader panicked"))?
    }
}

impl<P> Drop for Drain<P> {
    fn drop(&mut self) {
        self.pipe.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn capture(
    held: Option<Held>,
    command: &mut Command,
    say: &mut dyn FnMut(&str),
    compiler: &Build,
) -> io::Result<Output> {
    let stdout = Drain::new("build-stdout")?;
    let stderr = Drain::new("build-stderr")?;
    capture_with(held, command, say, stdout, stderr, compiler)
}

fn capture_with(
    held: Option<Held>,
    command: &mut Command,
    say: &mut dyn FnMut(&str),
    mut stdout: Drain<ChildStdout>,
    mut stderr: Drain<ChildStderr>,
    compiler: &Build,
) -> io::Result<Output> {
    let status = command.with_output_defaults(|command| {
        super::supervise_launch(
            held,
            command,
            say,
            super::front::Front::arm,
            |owned, _| {
                let child = owned.child_mut();
                // Command::output closes a configured stdin pipe before its owning wait.
                drop(child.stdin.take());
                stdout.hand(child.stdout.take());
                stderr.hand(child.stderr.take());
            },
            None,
            Some(compiler),
        )
        .map(super::JobOutcome::root_status)
    });
    let stdout = stdout.finish();
    let stderr = stderr.finish();
    Ok(Output {
        status: status?,
        stdout: stdout?,
        stderr: stderr?,
    })
}

#[cfg(test)]
#[path = "build_admit_output_tests.rs"]
mod tests;
