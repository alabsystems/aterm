// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact child custody while its parent's metadata and lock handoff can still fail.

use super::{Cmd, Live, RunFailed};
use aterm_uds::spawnfd::{Child, Stdio};
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};

/// The parent's entire final start message. A one-byte pipe write cannot partially commit.
pub const START_BYTE: u8 = b'1';
/// A must-happen detector for start/abort, not a latency budget.
pub const START_LIMIT: Duration = Duration::from_secs(60);

/// Parent custody for a cooperative child that awaits the start gate before effects.
/// The pipe does not suspend arbitrary executables or their startup code. The caller must
/// use run-job (or the same gate-before-effects protocol), and abort BEFORE releasing its
/// holder lock; an unproven abort preserves that lock.
/// Dropping an unresolved exact child transfers custody to the existing reaper, never to
/// a reconstructed PID or a new process registry.
pub trait StagedChild: Send {
    /// The retained child's ID, safe to record while its handle keeps the PID unreaped.
    fn id(&self) -> u32;
    /// Deliver the final byte and detach. Error means no byte delivered and child retained.
    /// # Errors
    /// Gate write failed, or this owner has already begun cancellation.
    fn commit(&mut self) -> Result<(), RunFailed>;
    /// Close the gate, signal only this retained child/group and prove direct-child reap.
    /// Error retains ownership; caller must leave the lock in place.
    /// # Errors
    /// Wait failed, reap detector expired, or the job is already committed.
    fn abort(&mut self) -> Result<(), RunFailed>;
}

pub(super) fn admit_request(cmd: &Cmd) -> Result<(), RunFailed> {
    if !cmd.program.is_absolute() {
        return Err(RunFailed::Refused(format!(
            "{} is not an absolute path",
            cmd.program.display()
        )));
    }
    if cmd.stdin.is_some() {
        return Err(RunFailed::Refused(
            "a staged child reserves stdin for its start gate".into(),
        ));
    }
    Ok(())
}

pub(super) fn spawn<'s>(
    live: &'s Live,
    cmd: &Cmd,
    log: &Path,
) -> Result<Box<dyn StagedChild + 's>, RunFailed> {
    use std::os::unix::fs::OpenOptionsExt as _;
    admit_request(cmd)?;
    let (program, args) = live.argv(cmd)?;
    let (read, write) = io::pipe().map_err(|e| RunFailed::Spawn(e.to_string()))?;
    let open_log = || {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(log)
            .map_err(|e| RunFailed::Spawn(format!("{}: {e}", log.display())))
    };
    // All fallible descriptor setup precedes spawn. No missing ChildStdin extraction
    // can abandon a successfully spawned child on a later setup error.
    let stdout = open_log()?;
    let stderr = open_log()?;
    let mut command = aterm_uds::spawnfd::command(&program);
    command
        .args(&args)
        .env_clear()
        .current_dir("/")
        .setsid(true)
        .stdin(Stdio::from(read))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in live.child_env(cmd) {
        command.env(key, value);
    }
    let child = command
        .spawn()
        .map_err(|e| RunFailed::Spawn(e.to_string()))?;
    let pid = child.id();
    Ok(Box::new(LiveStaged {
        child: Some(child),
        gate: Some(write),
        pid,
        committed: false,
        abort_attempted: false,
    }))
}

struct LiveStaged {
    child: Option<Child>,
    gate: Option<io::PipeWriter>,
    pid: u32,
    committed: bool,
    abort_attempted: bool,
}

/// Called only after try_wait proved this retained child remains unreaped. Its PID and
/// original setsid group cannot have been reused; no host/name scan grants signal rights.
fn signal_owned(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id())
        && pid > 0
    {
        // SAFETY: the exact unreaped child owns this group ID; it was spawned with setsid.
        let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
}

/// Reuse the existing reaper while retaining the exact Child object. Wait errors keep
/// custody rather than claiming disappearance; there is no detached PID lookup/kill.
fn to_reaper(mut child: Child) {
    aterm_uds::reaper::hand_off(move || matches!(child.try_wait(), Ok(Some(_))));
}

impl StagedChild for LiveStaged {
    fn id(&self) -> u32 {
        self.pid
    }

    fn commit(&mut self) -> Result<(), RunFailed> {
        use io::Write as _;
        if self.committed {
            return Ok(());
        }
        if self.abort_attempted {
            return Err(RunFailed::Refused(
                "staged child cancellation has begun".into(),
            ));
        }
        let gate = self
            .gate
            .as_mut()
            .ok_or_else(|| RunFailed::Refused("start gate is closed".into()))?;
        match gate.write(&[START_BYTE]) {
            Ok(1) => {}
            Ok(_) => return Err(RunFailed::Refused("start byte was not delivered".into())),
            Err(e) => return Err(RunFailed::Spawn(format!("start byte: {e}"))),
        }
        // Nothing fallible may turn a delivered byte into a start failure.
        self.committed = true;
        drop(self.gate.take());
        if let Some(child) = self.child.take() {
            to_reaper(child);
        }
        Ok(())
    }

    fn abort(&mut self) -> Result<(), RunFailed> {
        if self.committed {
            return Err(RunFailed::Refused(
                "committed child is no longer cancellation-owned".into(),
            ));
        }
        self.abort_attempted = true;
        drop(self.gate.take());
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        match child.try_wait() {
            Ok(Some(_)) => {
                self.child = None;
                return Ok(());
            }
            Ok(None) => signal_owned(child),
            Err(e) => {
                return Err(RunFailed::Refused(format!(
                    "child abort wait unproven: {e}; keep its lock"
                )));
            }
        }
        let by = Instant::now() + START_LIMIT;
        let mut exit = aterm_uds::deadline::ExitWait::new(child);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.child = None;
                    return Ok(());
                }
                Ok(None) => {
                    let left = by.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(RunFailed::Refused(
                            "child abort reap unproven after 60s; keep its lock".into(),
                        ));
                    }
                    exit.wait(left);
                }
                Err(e) => {
                    return Err(RunFailed::Refused(format!(
                        "child abort wait unproven: {e}; keep its lock"
                    )));
                }
            }
        }
    }
}

impl Drop for LiveStaged {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if !self.abort_attempted {
            let _ = self.abort();
        }
        drop(self.gate.take());
        if let Some(mut child) = self.child.take() {
            // Do not repeat a completed detector or signal after an unproven wait error.
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => signal_owned(&mut child),
                Err(_) => {}
            }
            to_reaper(child);
        }
    }
}

/// One inherited pipe endpoint, duplicated and kept alive through poll/read. Nonblocking
/// status prevents a competing read from turning readiness into an unbounded read; attempt
/// to restore the original status on exit. run-job owns stdin and has no other reader at this point.
struct StartPipe {
    fd: std::os::fd::OwnedFd,
    flags: i32,
}

impl StartPipe {
    fn stdin() -> Result<Self, RunFailed> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        // SAFETY: duplicate the inherited stdin into an owned close-on-exec descriptor.
        let raw = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
        if raw < 0 {
            return Err(RunFailed::Spawn(format!(
                "start stdin: {}",
                io::Error::last_os_error()
            )));
        }
        // SAFETY: fcntl returned a fresh owned descriptor.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: live fd and valid output storage; successful fstat initializes it.
        if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(RunFailed::Spawn(format!(
                "start pipe metadata: {}",
                io::Error::last_os_error()
            )));
        }
        // SAFETY: successful fstat initialized the full stat.
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFIFO {
            return Err(RunFailed::Refused(
                "start stdin is not an inherited pipe".into(),
            ));
        }
        // SAFETY: fcntl reads the owned descriptor's status flags.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(RunFailed::Spawn(format!(
                "start flags: {}",
                io::Error::last_os_error()
            )));
        }
        // SAFETY: changes only this inherited pipe's open-file status; restored on Drop.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(RunFailed::Spawn(format!(
                "start nonblocking: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Self { fd, flags })
    }
}

impl Drop for StartPipe {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd as _;
        // SAFETY: descriptor remains owned until after this destructor returns.
        let _ = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, self.flags) };
    }
}

pub(super) fn await_pipe_start(limit: Duration) -> Result<(), RunFailed> {
    use std::os::fd::AsRawFd as _;
    let by = Instant::now()
        .checked_add(limit)
        .ok_or_else(|| RunFailed::Refused("invalid start gate bound".into()))?;
    let pipe = StartPipe::stdin()?;
    loop {
        let left = by.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(RunFailed::Refused(
                "start gate did not commit before its deadline".into(),
            ));
        }
        let ms = i32::try_from(
            left.as_millis()
                .saturating_add(u128::from(left.subsec_nanos() % 1_000_000 != 0)),
        )
        .unwrap_or(i32::MAX);
        let mut poll = libc::pollfd {
            fd: pipe.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live initialized pollfd; poll neither retains nor closes it.
        let ready = unsafe { libc::poll(&mut poll, 1, ms) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(RunFailed::Spawn(format!("start poll: {error}")));
        }
        if ready == 0 {
            continue;
        }
        let mut byte = 0u8;
        // SAFETY: one writable byte and a live nonblocking inherited pipe.
        let got = unsafe { libc::read(pipe.fd.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
        if got == 1 {
            return if byte == START_BYTE {
                Ok(())
            } else {
                Err(RunFailed::Refused("invalid start gate byte".into()))
            };
        }
        if got == 0 {
            return Err(RunFailed::Refused(
                "start gate closed without a commit".into(),
            ));
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            continue;
        }
        return Err(RunFailed::Spawn(format!("start read: {error}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, System};

    fn fake() -> Fake {
        let mut sys = Fake::root();
        sys.next_pid = 4242;
        sys
    }

    #[test]
    fn staged_commit_transfers_once_but_child_admission_needs_an_explicit_gate_answer() {
        let sys = fake();
        let cmd = Cmd::new("/synthetic-job").args(["run-job", "1-5"]);
        let mut child = sys.stage_detached(&cmd, Path::new("/ram/job.log")).unwrap();
        assert_eq!(child.id(), 4242);
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.starts_with("stage committed"))
        );
        child.commit().unwrap();
        child.commit().unwrap();
        drop(child);
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| *call == "stage committed 4242")
                .count(),
            1
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.starts_with("stage abort"))
        );
        // A spawned/committed fake PID is not itself a child-side delivered-byte fact.
        assert!(sys.await_start(START_LIMIT).is_err());
        sys.start_answer(Ok(()));
        sys.await_start(START_LIMIT).unwrap();
        sys.start_answer(Err(RunFailed::Refused("synthetic EOF".into())));
        assert!(sys.await_start(START_LIMIT).is_err());
        assert!(sys.await_start(START_LIMIT).is_err());
    }

    #[test]
    fn a_refused_commit_keeps_the_stage_abort_owned() {
        let sys = fake();
        sys.stage_commit_refuses(true);
        let mut child = sys
            .stage_detached(&Cmd::new("/synthetic-job"), Path::new("/ram/job.log"))
            .unwrap();
        assert!(child.commit().is_err());
        assert_eq!(child.id(), 4242);
        child.abort().unwrap();
        assert!(child.commit().is_err());
        drop(child);
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| *call == "stage aborted 4242")
                .count(),
            1
        );
        assert!(!sys.calls().iter().any(|call| call.starts_with("stage committed") || call.starts_with("stage reaper")));
    }

    #[test]
    fn unproven_abort_preserves_custody_and_never_becomes_a_commit() {
        let sys = fake();
        sys.stage_abort_refuses(true);
        let mut child = sys
            .stage_detached(&Cmd::new("/synthetic-job"), Path::new("/ram/job.log"))
            .unwrap();
        assert!(child.abort().is_err());
        assert_eq!(child.id(), 4242);
        assert!(child.commit().is_err());
        drop(child);
        assert!(
            sys.calls()
                .iter()
                .any(|call| call == "stage abort unproven 4242")
        );
        assert!(
            sys.calls()
                .iter()
                .any(|call| call == "stage reaper custody 4242")
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.starts_with("stage committed") || call == "stage aborted 4242")
        );
        // Invalid requests and unknown synthetic IDs refuse before allocating a stage.
        let sys = Fake::root();
        assert!(
            sys.stage_detached(&Cmd::new("/synthetic-job"), Path::new("/ram/log"))
                .is_err()
        );
        let sys = fake();
        assert!(
            sys.stage_detached(&Cmd::new("relative"), Path::new("/ram/log"))
                .is_err()
        );
        let with_stdin = Cmd::new("/synthetic-job").stdin_file(Path::new("/ram/payload"));
        assert!(
            sys.stage_detached(&with_stdin, Path::new("/ram/log"))
                .is_err()
        );
        assert!(sys.calls().is_empty());
    }
}
