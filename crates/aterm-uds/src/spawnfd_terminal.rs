// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PROGRAM STARTED AS THE LEADER OF A SESSION ON A TERMINAL, as a forked tab shell is, by the
//! clone every Linux spawn makes ([`super::clone_spawn`]), so a start copies nothing of this
//! process (ATERM-KI-1113-811). The child takes the forked shell's steps, in this order: every
//! signal below 32 back to its default, every descriptor from 3 marked close-on-exec, then
//! `login_tty`'s (a session of its own, the slave its controlling terminal and its stdio), the
//! check that the slave still answers, its resource limits, its working directory, a passed
//! descriptor at its number, an empty mask, and an `execve` of the program exactly as named.

use super::PassAt;
use super::clone_spawn::{self, Outcome, Plan, Spec};
use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd};
use std::time::Duration;

/// What [`start`] starts, on whichever terminal each try gives it.
pub struct Start<'a> {
    /// The program, exactly as named: no search, and no shell over it.
    pub program: &'a CStr,
    pub argv: &'a [CString],
    pub envp: &'a [CString],
    pub cwd: Option<&'a CStr>,
    /// Resource limits the child sets on itself, decided here (`aterm_sandbox::Limits::plan`).
    pub limits: &'a [Limit],
    pub pass: Option<PassAt>,
}

/// One resource limit: an `RLIMIT_*` resource and its soft and hard values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub resource: u32,
    pub soft: u64,
    pub hard: u64,
}

/// The step a child stopped at before its exec, the forked shell's letter beside each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// `T`: its session, the slave its controlling terminal, or the slave on 0, 1 and 2.
    Terminal,
    /// `D`: the slave at 0 failed `fstat` or `TCGETS` once taken: it died.
    Live,
    /// `S`: a resource limit.
    Limits,
    /// `C`: the working directory.
    Folder,
    /// `P`: the passed descriptor at its number.
    Pass,
    /// `E`: the exec.
    Exec,
}

/// How a start ended.
#[derive(Debug)]
pub enum Started {
    /// The program runs: its pid, the caller's to reap.
    Running(i32),
    /// The child stopped at `step` with `error`, and is reaped (or the reaper's).
    Stopped { step: Step, error: io::Error },
    /// The child had not exec'd within the bound: it was killed, and is reaped (or the
    /// reaper's).
    Hung,
}

/// Start `start`'s program as the leader of a session on the terminal of `slave`, bounded by
/// `within`.
///
/// # Errors
/// The plan's (`/dev/null`'s open), the clone's, or the report pipe's.
pub fn start(start: &Start<'_>, slave: BorrowedFd<'_>, within: Duration) -> io::Result<Started> {
    let &Start {
        program,
        argv,
        envp,
        cwd,
        limits,
        pass,
    } = start;
    let image = super::Image {
        program: program.to_owned(),
        argv: argv.to_vec(),
        envp: envp.to_vec(),
        search: None,
    };
    let plan = Plan::new(
        &image,
        Spec {
            cwd: cwd.map(CStr::to_owned),
            terminal: Some(slave.as_raw_fd()),
            limits: limits
                .iter()
                .map(|limit| clone_spawn::Limit {
                    resource: limit.resource,
                    value: [limit.soft, limit.hard],
                })
                .collect(),
            pass: pass.map(|pass| (pass.from, pass.at)),
            exact: true,
            ..Spec::default()
        },
    )?;
    Ok(match plan.start_within(within)? {
        Outcome::Started(pid) => Started::Running(pid),
        Outcome::Hung { .. } => Started::Hung,
        Outcome::Stopped(step, error) => match step {
            clone_spawn::Step::Terminal => Started::Stopped {
                step: Step::Terminal,
                error,
            },
            clone_spawn::Step::Live => Started::Stopped {
                step: Step::Live,
                error,
            },
            clone_spawn::Step::Limits => Started::Stopped {
                step: Step::Limits,
                error,
            },
            clone_spawn::Step::Folder => Started::Stopped {
                step: Step::Folder,
                error,
            },
            clone_spawn::Step::Pass => Started::Stopped {
                step: Step::Pass,
                error,
            },
            clone_spawn::Step::Exec => Started::Stopped {
                step: Step::Exec,
                error,
            },
            other => Started::Stopped {
                step: Step::Exec,
                error: io::Error::other(format!(
                    "the child stopped at a step a terminal start does not take ({other:?}: \
                     {error})"
                )),
            },
        },
    })
}

/// TEST SEAM: the children of this thread's spawns wait `ms` before their first step, as a
/// loaded machine starves one (0: none).
#[cfg(any(test, debug_assertions))]
pub fn stall_children(ms: u32) {
    clone_spawn::stall_children(ms);
}

#[cfg(test)]
#[path = "spawnfd_terminal_tests.rs"]
mod tests;
