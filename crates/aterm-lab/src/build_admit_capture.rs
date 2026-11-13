// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Opt-in stacks of an owned, unreaped build front. Sampling uses the readiness deadline;
//! killing and reaping the sampler is separate safety cleanup and can finish after it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Command, Stdio, command};

const POLL: Duration = Duration::from_millis(20);
static CAPTURE: Mutex<()> = Mutex::new(());

/// Hold before a hard exit releases the front barriers: a sampler still owns their pids.
pub(super) fn lock() -> MutexGuard<'static, ()> {
    CAPTURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A sampler always has a private group. No caller reaps its leader before this guard ends it.
struct Sampler {
    child: Option<Child>,
}

impl Sampler {
    fn finish(&mut self) -> Result<ExitStatus, String> {
        let child = self.child.as_mut().ok_or("sampler was already reaped")?;
        let signalled = super::sweep_group(child.id());
        // Also end the spawned leader if the group sweep could not deliver a signal.
        let _ = child.kill();
        // Safety cleanup deliberately does not stop at the sampling/readiness deadline.
        // After SIGKILL the kernel may still have to finish an exec assessment or a syscall.
        let status = child.wait().map_err(|e| format!("reap sampler: {e}"));
        // A wait was attempted: never signal this group again after a possibly reaping wait.
        self.child = None;
        if let Err(error) = signalled {
            return Err(format!("end sampler group: {error}; wait: {status:?}"));
        }
        status
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.finish();
        }
    }
}

fn sidecar(output: &Path, suffix: &str) -> PathBuf {
    let mut path = output.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn sampler(pid: u32, output: &Path) -> Command {
    let mut cmd = command("/usr/bin/sample");
    cmd.arg(pid.to_string())
        .args(["1", "10", "-mayDie", "-fullPaths", "-file"])
        .arg(output);
    cmd
}

/// Capture one stack receipt. Failure is diagnostic only; this never reaps or signals target.
pub(super) fn capture(target: &mut Child, output: &Path, ready_by: Instant) -> String {
    capture_using(
        target,
        output,
        ready_by,
        &|pid, path| Ok(sampler(pid, path)),
        &|| crate::stop::check().is_err(),
    )
}

fn capture_using(
    target: &mut Child,
    output: &Path,
    ready_by: Instant,
    make_sampler: &dyn Fn(u32, &Path) -> io::Result<Command>,
    interrupted: &dyn Fn() -> bool,
) -> String {
    // This guard outlives the sampler guard, including during panic unwinding. The hard-exit
    // callback can release target barriers only after the sampler is ended and reaped.
    let _capture = lock();
    let pid = target.id();
    let unavailable = |why: &str| format!("sample pid={pid}: unavailable ({why})");
    match target.exited() {
        Ok(false) => {}
        Ok(true) => return unavailable("owned target exited; it was not sampled or reaped"),
        Err(error) => return unavailable(&format!("target exit query: {error}")),
    }
    let image = i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::process::read_process_path);
    let observed = format!("sample pid={pid} observed_image={image:?} (may exec-change)");
    let unavailable = |why: &str| format!("{observed}: unavailable ({why})");
    if interrupted() {
        return unavailable("interrupted before sampling");
    }
    if Instant::now() >= ready_by {
        return unavailable("readiness deadline elapsed before sampling");
    }
    let stdout = sidecar(output, ".stdout");
    let stderr = sidecar(output, ".stderr");
    let spawn = || -> io::Result<Child> {
        // Truncate stale output before the attempt, so a successful empty capture cannot
        // accidentally inherit evidence from a previous invocation.
        drop(crate::util::create_stdio_file(output)?);
        let mut cmd = make_sampler(pid, output)?;
        cmd.stdin(Stdio::null())
            .stdout(crate::util::create_stdio_file(&stdout)?)
            .stderr(crate::util::create_stdio_file(&stderr)?)
            .process_group(0)
            .setsid(true);
        if interrupted() || Instant::now() >= ready_by {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "sampling cancelled before spawn",
            ));
        }
        cmd.spawn()
    };
    let child = match spawn() {
        Ok(child) => child,
        Err(error) => return unavailable(&format!("sampler unavailable: {error}")),
    };
    let mut running = Sampler { child: Some(child) };
    let mut exit =
        aterm_uds::deadline::ExitWait::new(running.child.as_ref().expect("owned sampler"));
    let outcome = loop {
        if interrupted() {
            break Err("interrupted during sampling".to_owned());
        }
        let left = ready_by.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break Err("readiness deadline elapsed during sampling".to_owned());
        }
        match running.child.as_mut().expect("owned sampler").exited() {
            Ok(true) => break Ok(()),
            Ok(false) => exit.wait(POLL.min(left)),
            Err(error) => break Err(format!("sampler exit query: {error}")),
        }
    };
    let status = running.finish();
    match (outcome, status) {
        (Err(why), cleanup) => unavailable(&format!("{why}; cleanup={cleanup:?}")),
        (Ok(()), Err(why)) => unavailable(&why),
        (Ok(()), Ok(status)) if !status.success() => {
            unavailable(&format!("sampler {status}; stderr={}", stderr.display()))
        }
        (Ok(()), Ok(_)) => match fs::metadata(output) {
            Ok(metadata) if metadata.is_file() && metadata.len() > 0 => {
                format!("{observed}: captured {}", output.display())
            }
            Ok(_) => unavailable("sampler produced no nonempty output file"),
            Err(error) => unavailable(&format!("sampler output unavailable: {error}")),
        },
    }
}

#[cfg(test)]
#[path = "build_admit_capture_tests.rs"]
mod tests;
