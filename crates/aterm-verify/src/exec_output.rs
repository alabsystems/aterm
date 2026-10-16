// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded walk's children. The walk can be stuck in Rust or filesystem IO,
//! but its output calls have one cancellable owner. Closing admission and
//! acquiring a child permit share a lock, so no child can start after close
//! has returned. A permit is released only after kill/reap, including unwind.

use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::io;
use std::process::{ExitStatus, Output};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

#[derive(Default)]
struct State {
    closed: bool,
    active: usize,
}

#[derive(Default)]
pub(super) struct Scope {
    state: Mutex<State>,
    drained: Condvar,
}

impl Scope {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn enter(&self) -> io::Result<Permit<'_>> {
        let mut state = self.state();
        if state.closed {
            return Err(cancelled());
        }
        state.active += 1;
        Ok(Permit(self))
    }

    fn closed(&self) -> bool {
        self.state().closed
    }

    pub(super) fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        while state.active != 0 {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(super) fn output(&self, command: &mut Command) -> io::Result<Output> {
        // File capture preserves complete cargo-tree/metadata output without
        // pipe reader threads that a descendant could keep blocked forever.
        let directory = aterm_tempfile::tempdir()?;
        let stdout = directory.path().join("stdout");
        let stderr = directory.path().join("stderr");
        command
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&stdout)?)
            .stderr(std::fs::File::create(&stderr)?);
        let permit = self.enter()?;
        let status = OwnedChild::run(command, self);
        // The owned child is gone now. A stuck read of the captured files does
        // not keep close waiting for an OS child that no longer exists.
        drop(permit);
        Ok(Output {
            status: status?,
            stdout: std::fs::read(stdout)?,
            stderr: std::fs::read(stderr)?,
        })
    }
}

struct Permit<'a>(&'a Scope);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.active -= 1;
        self.0.drained.notify_all();
    }
}

fn cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "the bounded walk's command scope was closed",
    )
}

struct OwnedChild {
    child: Option<Child>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    live: super::group::Live,
}

impl OwnedChild {
    fn run(command: &mut Command, scope: &Scope) -> io::Result<ExitStatus> {
        // The group/reaping fence below is defined on the two Unix hosts the
        // check runner supports. Refuse another Unix rather than spawn a child
        // whose whole group this executor cannot safely own.
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
        {
            let _ = (command, scope);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "bounded output ownership is unavailable on this Unix",
            ));
        }
        #[cfg(any(not(unix), target_os = "macos", target_os = "linux"))]
        {
            #[cfg(unix)]
            let spawning = super::group::Spawning::begin();
            #[cfg(unix)]
            if super::group::stopping() {
                return Err(io::Error::other(
                    "the run was interrupted before this child started",
                ));
            }
            if scope.closed() {
                return Err(cancelled());
            }
            #[cfg(unix)]
            command.process_group(0);
            let child = command.spawn()?;
            let mut owned = Self {
                #[cfg(unix)]
                live: super::group::Live::enter(child.id(), false),
                child: Some(child),
            };
            #[cfg(unix)]
            drop(spawning);
            let mut nap = Duration::from_millis(1);
            loop {
                if scope.closed() {
                    // Drop sends SIGKILL and reaps before the permit can leave.
                    return Err(cancelled());
                }
                if let Some(status) = owned.poll()? {
                    return Ok(status);
                }
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(25));
            }
        }
    }

    fn poll(&mut self) -> io::Result<Option<ExitStatus>> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            if !self.live.exited(false)? {
                return Ok(None);
            }
            // Keep the leader unreaped while disposing any descendants. Output
            // calls own finite commands, never a detached daemon's lifetime.
            let _ = self.live.kill();
            let status = self
                .live
                .reap_output(self.child.as_mut().expect("owned child"))?;
            self.child.take();
            Ok(Some(status))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let status = self.child.as_mut().expect("owned child").try_wait()?;
            if status.is_some() {
                self.child.take();
            }
            Ok(status)
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            // Every signal precedes the only reap, while the group id is held.
            let _ = self.live.kill();
            let _ = self.live.reap_output(&mut child);
            if super::group::stopping() {
                // The interrupt handler may remember this group. Keep its
                // leader unreaped, exactly as the stage executor does.
                std::mem::forget(child);
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
