// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Child processes: one bounded run that captures its output, and the
//! kill-on-close job a smoked instance is born in (`aterm_uds::deadline::spawn_in_job`).

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Command, Stdio};

/// What a finished child said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// Its exit code.
    pub code: Option<i32>,
    /// Its standard output.
    pub stdout: String,
    /// Its standard error.
    pub stderr: String,
}

impl Ran {
    /// Whether it exited 0.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// Both streams, standard error first: `aterm ctl` answers some verbs'
    /// header there and their rows on standard output.
    #[must_use]
    pub fn text(&self) -> String {
        format!("{}\n{}", self.stderr.trim_end(), self.stdout.trim_end())
            .trim()
            .to_string()
    }
}

/// A child's environment and working directory.
#[derive(Debug, Clone, Copy, Default)]
pub struct Place<'a> {
    /// `Some`: the whole environment (the caller's is cleared). `None`: the caller's.
    pub env: Option<&'a [(String, String)]>,
    /// The working directory, the caller's when `None`.
    pub cwd: Option<&'a Path>,
}

/// `program args…` placed as `place` says, not yet started.
#[must_use]
pub fn command<S: AsRef<OsStr>>(program: &Path, args: &[S], place: Place<'_>) -> Command {
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args);
    if let Some(env) = place.env {
        cmd.env_clear();
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    if let Some(cwd) = place.cwd {
        cmd.current_dir(cwd);
    }
    cmd
}

/// Runs `program args…` to its end, at most `within`, capturing both streams.
///
/// # Errors
/// It did not start, or did not end within `within` (it is then killed).
pub fn run<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    place: Place<'_>,
    within: Duration,
) -> Result<Ran, String> {
    let mut cmd = command(program, args, place);
    let out = aterm_uds::deadline::output_within(&mut cmd, within)
        .map_err(|e| format!("{}: {e}", program.display()))?;
    Ok(Ran {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// How long ending a job may take before it is left to its handle's close.
const END_HANG: Duration = Duration::from_secs(60);

/// Runs `program args…` with both streams written to `file` (made anew) as
/// they come, in a kill-on-close job, at most `within`. `Ok(Some(code))`: it
/// exited (`-1` when it ended without a code). `Ok(None)`: it ran past
/// `within`, and it and everything it started were ended through the job.
///
/// # Errors
/// `file` could not be made, the child did not start, or it could not be waited on
/// (it is then ended through the job).
pub fn run_to_file<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    file: &Path,
    within: Duration,
) -> Result<Option<i32>, String> {
    let named = |e: std::io::Error| format!("{}: {e}", program.display());
    let sink = std::fs::File::create(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let errors = sink
        .try_clone()
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let mut cmd = command(program, args, Place::default());
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(sink))
        .stderr(Stdio::from(errors));
    let (mut child, job) = Job::start(&mut cmd).map_err(named)?;
    let by = Instant::now() + within;
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status.code().unwrap_or(-1))),
            Ok(None) => {}
            Err(e) => {
                let _ = job.end(END_HANG);
                return Err(named(e));
            }
        }
        let now = Instant::now();
        if now >= by {
            let _ = job.end(END_HANG);
            let _ = child.try_wait();
            return Ok(None);
        }
        exit.wait(by - now);
    }
}

/// A kill-on-close job object and the child that was born in it. The smoke
/// starts its instance in one, so the instance, its console host and its shell
/// end by a handle this process holds: never by a name, a numeric pid or a
/// parent-pid sweep, which Windows' pid reuse can aim at another program.
pub struct Job(imp::Job);

impl Job {
    /// Starts `cmd` suspended, puts it in a new job and only then lets it run:
    /// every process it ever starts is born in the job.
    ///
    /// # Errors
    /// The OS refused the job, the start, the assignment or the resume; a
    /// child that did start was ended by its own handle.
    pub fn start(cmd: &mut Command) -> std::io::Result<(Child, Self)> {
        imp::start(cmd).map(|(child, job)| (child, Self(job)))
    }

    /// How many processes the job holds now.
    ///
    /// # Errors
    /// The OS refused the count.
    pub fn active(&self) -> std::io::Result<u32> {
        self.0.active()
    }

    /// Ends every process in the job and waits, at most `hang`, until none is
    /// left. `Ok(false)`: some process was still there at `hang`.
    ///
    /// # Errors
    /// The OS refused the termination or the count.
    pub fn end(&self, hang: Duration) -> std::io::Result<bool> {
        self.0.end(hang)
    }
}

#[cfg(windows)]
mod imp {
    use aterm_uds::spawnfd::{Child, Command};

    pub(super) use aterm_uds::deadline::JobHold as Job;

    pub(super) fn start(cmd: &mut Command) -> std::io::Result<(Child, Job)> {
        aterm_uds::deadline::spawn_in_job(cmd)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::time::Duration;

    use aterm_uds::spawnfd::{Child, Command};

    /// Off Windows there is no job object, and no verb of this tool starts an instance.
    pub(super) struct Job;

    pub(super) fn start(_cmd: &mut Command) -> io::Result<(Child, Job)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "job objects are Windows'",
        ))
    }

    impl Job {
        pub(super) fn active(&self) -> io::Result<u32> {
            Ok(0)
        }

        pub(super) fn end(&self, _hang: Duration) -> io::Result<bool> {
            Ok(true)
        }
    }
}

#[cfg(test)]
#[path = "proc_tests.rs"]
mod tests;
