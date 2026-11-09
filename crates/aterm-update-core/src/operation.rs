// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cooperative cancellation for a dedicated package-operation process.

use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
struct OperationState {
    requested: AtomicBool,
    deadline: Option<std::time::Instant>,
    #[cfg(unix)]
    helpers: Mutex<usize>,
    #[cfg(unix)]
    retired: std::sync::Condvar,
}

impl OperationState {
    fn requested(&self) -> bool {
        if self.deadline.is_some() {
            self.requested_at(std::time::Instant::now())
        } else {
            self.requested.load(Ordering::Acquire)
        }
    }

    fn requested_at(&self, now: std::time::Instant) -> bool {
        self.requested.load(Ordering::Acquire)
            || self.deadline.is_some_and(|deadline| now >= deadline)
    }
}

static STOP: Mutex<Option<Arc<OperationState>>> = Mutex::new(None);

/// Whether this dedicated process is currently inside an owned package operation.
#[must_use]
pub fn operation_active() -> bool {
    STOP.lock().unwrap_or_else(|p| p.into_inner()).is_some()
}

/// Whether optional post-install work must wait for a later upkeep pass.
/// Owner-bound installs finish their bounded request first. Self-budgeted updates
/// retain ordinary upkeep while their budget remains live.
#[must_use]
pub fn operation_defers_maintenance() -> bool {
    STOP.lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|stop| stop.deadline.is_none() || stop.requested())
}

/// Whether this dedicated operation's owner cancelled it or its own budget expired.
#[must_use]
pub fn operation_cancelled() -> bool {
    STOP.lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|stop| stop.requested())
}

/// Refuse another operation step after cancellation. Completed mutations remain completed.
///
/// # Errors
/// `Interrupted` once the operation is cancelled.
pub fn check_operation() -> io::Result<()> {
    if operation_cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "package operation cancelled",
        ))
    } else {
        Ok(())
    }
}

/// Process-wide operation scope. Only dedicated CLI processes install one; helpers do not
/// inherit its pipe. Drop requests stop and retires every registered helper and the observer
/// before clearing its exact scope.
#[cfg(unix)]
pub struct OperationCancellation {
    stop: Arc<OperationState>,
    shutdown: Option<std::io::PipeWriter>,
    observer: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl OperationCancellation {
    /// Own a monotonic budget inside the package process, independent of its launcher.
    /// Expiry cooperatively retires helpers and refuses further mutation; cleanup keeps
    /// its existing custody until it finishes.
    ///
    /// # Errors
    /// Refuses an already active operation.
    pub fn until(deadline: std::time::Instant) -> io::Result<Self> {
        Self::install(Some(deadline))
    }

    fn install(deadline: Option<std::time::Instant>) -> io::Result<Self> {
        let stop = Arc::new(OperationState {
            deadline,
            ..OperationState::default()
        });
        let mut active = STOP.lock().unwrap_or_else(|p| p.into_inner());
        if active.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "an operation scope is already active",
            ));
        }
        *active = Some(stop.clone());
        Ok(Self {
            stop,
            shutdown: None,
            observer: None,
        })
    }

    /// Adopt a read-only cancellation pipe from stdin, replacing stdin with `/dev/null`.
    ///
    /// # Errors
    /// Invalid stdin, an already active operation, pipe creation or thread startup failure.
    pub fn from_stdin() -> io::Result<Self> {
        Self::watch(aterm_uds::kept_out(|| aterm_uds::lifeline::adopt(0))?)
    }

    fn watch(reader: std::os::fd::OwnedFd) -> io::Result<Self> {
        use std::os::fd::AsFd as _;
        let (shutdown, held) = aterm_uds::kept_out(std::io::pipe)?;
        let mut scope = Self::install(None)?;
        let stop = scope.stop.clone();
        scope.shutdown = Some(held);
        scope.observer = Some(
            std::thread::Builder::new()
                .name("package-cancel".into())
                .spawn(move || {
                    let ready = aterm_uds::ready::either(reader.as_fd(), shutdown.as_fd());
                    if match ready {
                        Ok(ready) => ready.first,
                        Err(_) => true,
                    } {
                        stop.requested.store(true, Ordering::Release);
                    }
                })?,
        );
        Ok(scope)
    }
}

#[cfg(unix)]
impl Drop for OperationCancellation {
    fn drop(&mut self) {
        {
            // Serialize retirement with helper registration, so no admitted helper can
            // appear after the zero-count observation that lets this scope retire.
            let _active = STOP.lock().unwrap_or_else(|p| p.into_inner());
            self.stop.requested.store(true, Ordering::Release);
        }
        self.shutdown.take();
        if let Some(observer) = self.observer.take() {
            let _ = observer.join();
        }
        let mut helpers = self.stop.helpers.lock().unwrap_or_else(|p| p.into_inner());
        while *helpers != 0 {
            helpers = self
                .stop
                .retired
                .wait(helpers)
                .unwrap_or_else(|p| p.into_inner());
        }
        drop(helpers);
        let mut active = STOP.lock().unwrap_or_else(|p| p.into_inner());
        if active
            .as_ref()
            .is_some_and(|stop| Arc::ptr_eq(stop, &self.stop))
        {
            *active = None;
        }
    }
}

#[cfg(unix)]
struct HelperLease(Arc<OperationState>);

#[cfg(unix)]
impl Drop for HelperLease {
    fn drop(&mut self) {
        let mut helpers = self.0.helpers.lock().unwrap_or_else(|p| p.into_inner());
        *helpers -= 1;
        self.0.retired.notify_all();
    }
}

#[cfg(unix)]
fn helper_scope() -> io::Result<Option<HelperLease>> {
    let active = STOP.lock().unwrap_or_else(|p| p.into_inner());
    let Some(stop) = active.as_ref() else {
        return Ok(None);
    };
    if stop.requested() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "package operation cancelled",
        ));
    }
    *stop.helpers.lock().unwrap_or_else(|p| p.into_inner()) += 1;
    Ok(Some(HelperLease(stop.clone())))
}

/// Run one operation helper, retaining its owning child through cancellation and cleanup.
/// With no operation scope this preserves ordinary command capture.
///
/// # Errors
/// Spawn, output or wait errors, or `Interrupted` after cooperative cancellation.
pub fn run_operation_command(
    cmd: &mut aterm_uds::spawnfd::Command,
) -> io::Result<std::process::Output> {
    run_command_until(cmd, || false)
}

pub(crate) fn run_command_until(
    cmd: &mut aterm_uds::spawnfd::Command,
    stopped: impl Fn() -> bool,
) -> io::Result<std::process::Output> {
    #[cfg(unix)]
    {
        let helper = helper_scope()?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = aterm_uds::deadline::stream_until_stopped(
            cmd,
            None,
            |_| {},
            |stream, chunk| {
                match stream {
                    aterm_uds::deadline::ChildOutput::Stdout => stdout.extend_from_slice(chunk),
                    aterm_uds::deadline::ChildOutput::Stderr => stderr.extend_from_slice(chunk),
                }
                Ok(())
            },
            || stopped() || helper.as_ref().is_some_and(|helper| helper.0.requested()),
        )?;
        status
            .map(|status| std::process::Output {
                status,
                stdout,
                stderr,
            })
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::Interrupted, "package operation cancelled")
            })
    }
    #[cfg(not(unix))]
    {
        if stopped() || operation_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "package operation cancelled",
            ));
        }
        cmd.output()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn operation_deadline_expires_at_the_boundary_and_cannot_outlive_cancellation() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let stop = OperationState {
            deadline: Some(deadline),
            ..OperationState::default()
        };
        assert!(!stop.requested_at(deadline - std::time::Duration::from_nanos(1)));
        assert!(stop.requested_at(deadline));
        assert!(stop.requested_at(deadline + std::time::Duration::from_secs(1)));
        stop.requested.store(true, Ordering::Release);
        assert!(stop.requested_at(deadline - std::time::Duration::from_secs(1)));
        let unbounded = OperationState::default();
        assert!(!unbounded.requested_at(deadline));
    }

    #[test]
    fn operation_pipe_cancels_its_owned_helper_and_scope_drop_releases_observer() {
        const CHILD: &str = "ATERM_OPERATION_CANCEL_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            use std::os::fd::OwnedFd;
            let dir = aterm_tempfile::tempdir().unwrap();
            let witness = dir.path().join("pid");
            let (read, write) = aterm_uds::kept_out(std::io::pipe).unwrap();
            let scope = OperationCancellation::watch(OwnedFd::from(read)).unwrap();
            assert!(operation_active());
            assert!(!operation_cancelled());
            assert!(operation_defers_maintenance());
            let path = witness.clone();
            let closer = std::thread::spawn(move || {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while !path.is_file() && std::time::Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                drop(write);
                assert!(path.is_file(), "helper execution hang detector");
            });
            let mut cmd = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(
                std::path::Path::new("/bin/sh"),
            ));
            cmd.args(["-c", "printf '%s\\n' \"$$\" > \"$0\"; exec \"$1\" 600"])
                .arg(&witness)
                .arg(aterm_uds::deadline::stand_in(std::path::Path::new(
                    "/bin/sleep",
                )));
            let error = run_operation_command(&mut cmd).unwrap_err();
            closer.join().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert!(operation_cancelled());
            assert!(check_operation().is_err());
            let mut absent = aterm_uds::spawnfd::command("/no-such-cancelled-helper");
            assert_eq!(
                run_operation_command(&mut absent).unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
            let pid = std::fs::read_to_string(&witness)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert_eq!(
                aterm_uds::process::process_presence(pid),
                aterm_uds::process::ProcessPresence::Absent
            );
            drop(scope);
            assert!(!operation_active());
            assert!(!operation_cancelled());
            // Normal scope retirement must not wait for the client's still-open writer.
            let (read, held) = aterm_uds::kept_out(std::io::pipe).unwrap();
            let scope = OperationCancellation::watch(OwnedFd::from(read)).unwrap();
            drop(scope);
            assert!(!operation_cancelled());
            drop(held);
            let (read, held) = aterm_uds::kept_out(std::io::pipe).unwrap();
            let scope = OperationCancellation::watch(OwnedFd::from(read)).unwrap();
            let witness = dir.path().join("inflight-pid");
            let path = witness.clone();
            let helper = std::thread::spawn(move || {
                let mut cmd = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(
                    std::path::Path::new("/bin/sh"),
                ));
                cmd.args(["-c", "printf '%s\\n' \"$$\" > \"$0\"; exec \"$1\" 600"])
                    .arg(path)
                    .arg(aterm_uds::deadline::stand_in(std::path::Path::new(
                        "/bin/sleep",
                    )));
                run_operation_command(&mut cmd)
            });
            let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !witness.is_file() && std::time::Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let ran = witness.is_file();
            drop(scope);
            let pid = std::fs::read_to_string(&witness)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert_eq!(
                aterm_uds::process::process_presence(pid),
                aterm_uds::process::ProcessPresence::Absent,
                "scope retirement retained the helper's owning wait"
            );
            assert_eq!(
                helper.join().unwrap().unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
            assert!(ran, "helper execution hang detector");
            drop(held);

            let scope = OperationCancellation::until(std::time::Instant::now()).unwrap();
            assert!(operation_active());
            assert!(operation_cancelled());
            assert!(operation_defers_maintenance());
            assert!(check_operation().is_err());
            let mut absent = aterm_uds::spawnfd::command("/no-such-expired-helper");
            assert_eq!(
                run_operation_command(&mut absent).unwrap_err().kind(),
                io::ErrorKind::Interrupted,
                "the expired scope refuses before spawning another helper"
            );
            assert!(OperationCancellation::until(std::time::Instant::now()).is_err());
            drop(scope);
            assert!(!operation_active());
            assert!(!operation_cancelled());
            let scope = OperationCancellation::until(
                std::time::Instant::now() + std::time::Duration::from_secs(3600),
            )
            .unwrap();
            assert!(!operation_cancelled());
            assert!(!operation_defers_maintenance());
            drop(scope);
            assert!(
                !operation_active(),
                "normal exit retires without waiting for the budget"
            );
            return;
        }
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        cmd.args(["--exact", "operation::tests::operation_pipe_cancels_its_owned_helper_and_scope_drop_releases_observer", "--nocapture"])
            .env(CHILD, "1");
        let output = aterm_uds::deadline::output_until(
            &mut cmd,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
