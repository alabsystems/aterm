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
    #[cfg(unix)]
    helpers: Mutex<usize>,
    #[cfg(unix)]
    retired: std::sync::Condvar,
}

static STOP: Mutex<Option<Arc<OperationState>>> = Mutex::new(None);

/// Whether this dedicated process is currently inside an owned package operation.
#[must_use]
pub fn operation_active() -> bool {
    STOP.lock().unwrap_or_else(|p| p.into_inner()).is_some()
}

/// Whether this dedicated operation's owner closed its cancellation pipe.
#[must_use]
pub fn operation_cancelled() -> bool {
    STOP.lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|stop| stop.requested.load(Ordering::Acquire))
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
        let stop = Arc::new(OperationState::default());
        {
            let mut active = STOP.lock().unwrap_or_else(|p| p.into_inner());
            if active.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "an operation scope is already active",
                ));
            }
            *active = Some(stop.clone());
        }
        let mut scope = Self {
            stop: stop.clone(),
            shutdown: Some(held),
            observer: None,
        };
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
    if stop.requested.load(Ordering::Acquire) {
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
            || {
                stopped()
                    || helper
                        .as_ref()
                        .is_some_and(|helper| helper.0.requested.load(Ordering::Acquire))
            },
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
