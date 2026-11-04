// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stop notifications preserve actual child ownership and never establish release.

use super::{Child, Rusage, rusage};
use std::io;
use std::os::unix::process::ExitStatusExt as _;

/// A kernel stop is distinct from completion or an accepted resume request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildStop {
    /// Darwin's stopped-at-exec notification, which carries no stopping signal.
    SuspendedStart,
    /// An actual stopping signal reported by the kernel.
    Signal(i32),
}

fn stopped(signal: i32) -> io::Result<ChildStop> {
    if cfg!(target_vendor = "apple") && signal == 0 {
        Ok(ChildStop::SuspendedStart)
    } else if signal > 0 {
        Ok(ChildStop::Signal(signal))
    } else {
        Err(io::Error::other("invalid owned-child stopping signal"))
    }
}

/// What the kernel reported, before any stop acknowledgement or owning reap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildNotice {
    /// A stop was observed, not proof that the child remains stopped now.
    Stopped(ChildStop),
    /// Terminal exit was observed, with the child still unreaped.
    Exited,
}

/// An actual acknowledged transition; only Exited carries complete resource accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildEvent {
    /// Acknowledged a stop without reaping the child or releasing its reservation.
    Stopped(ChildStop),
    /// The original owner reaped its child with actual exit status and usage.
    Exited(Rusage),
}

/// An exclusive borrow of the actual unreaped child. It cannot be cloned, decoded,
/// applied to another child, or held while this Child is reaped through its safe API.
/// Dropping it consumes no notification and changes no terminal state.
pub struct ChildObservation<'a> {
    child: &'a mut Child,
    notice: ChildNotice,
}

impl ChildObservation<'_> {
    /// The observed notification; this value alone supplies no process authority.
    pub const fn notice(&self) -> ChildNotice {
        self.notice
    }

    /// Its actual child's pid, observed here, not a signalling capability.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Yield terminal custody before acknowledging any event that might race exit.
    /// A stop leaves the child owned. A raced exit is cached with resource accounting;
    /// a raced continuation can leave no event. No outcome implicitly resumes work.
    /// Signal-forwarding owners must gate sends across this potential reap.
    /// # Errors
    /// Terminal restoration or the actual owning kernel wait fails.
    pub fn acknowledge(self) -> io::Result<Option<ChildEvent>> {
        self.child.restore_foreground()?;
        let Some(usage) = rusage::take_job(self.child.id())? else {
            return Ok(None);
        };
        let raw = usage.status.into_raw();
        if libc::WIFSTOPPED(raw) {
            return Ok(Some(ChildEvent::Stopped(stopped(libc::WSTOPSIG(raw))?)));
        }
        if !libc::WIFEXITED(raw) && !libc::WIFSIGNALED(raw) {
            return Err(io::Error::other("unexpected owning job wait status"));
        }
        self.child.handle.status = Some(usage.status);
        self.child.usage = Some(usage);
        Ok(Some(ChildEvent::Exited(usage)))
    }
}

impl Child {
    /// Wait for a stop or exit notification while retaining an exclusive unreaped
    /// child borrow. Unlike wait(), this never closes stdin to cause completion.
    /// # Errors
    /// Already reaped, unsupported kernel, traced/debugger event, or wait failure.
    pub fn wait_job_event(&mut self) -> io::Result<ChildObservation<'_>> {
        self.job_event(true)?
            .ok_or_else(|| io::Error::other("no job event observed"))
    }

    /// Observe a pending stop or exit without blocking, consuming it or reaping.
    /// # Errors
    /// As [`Self::wait_job_event`].
    pub fn try_job_event(&mut self) -> io::Result<Option<ChildObservation<'_>>> {
        self.job_event(false)
    }

    fn job_event(&mut self, block: bool) -> io::Result<Option<ChildObservation<'_>>> {
        if self.handle.status.is_some() || self.usage.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "job observation requires an unreaped child",
            ));
        }
        Ok(notice(self.id(), block)?.map(|notice| ChildObservation {
            child: self,
            notice,
        }))
    }
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn notice(pid: u32, block: bool) -> io::Result<Option<ChildNotice>> {
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: this is the caller's actual unreaped child, with a live output;
        // WNOWAIT consumes neither a stop notification nor the child's identity.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                info.as_mut_ptr(),
                libc::WEXITED
                    | libc::WSTOPPED
                    | libc::WNOWAIT
                    | if block { 0 } else { libc::WNOHANG },
            )
        };
        if result == 0 {
            // SAFETY: the zeroed record was initialized before the kernel call.
            let info = unsafe { info.assume_init() };
            if info.si_signo == 0 && !block {
                return Ok(None);
            }
            if info.si_signo != libc::SIGCHLD
                // SAFETY: SIGCHLD admits this initialized record's pid arm.
                || u32::try_from(unsafe { info.si_pid() }).ok() != Some(pid)
            {
                return Err(io::Error::other(
                    "job wait did not identify this owned child",
                ));
            }
            return match info.si_code {
                libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED => {
                    Ok(Some(ChildNotice::Exited))
                }
                // SAFETY: SIGCHLD selects the initialized status arm.
                libc::CLD_STOPPED => Ok(Some(ChildNotice::Stopped(stopped(unsafe {
                    info.si_status()
                })?))),
                libc::CLD_TRAPPED => Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "debugger traps require their own controller",
                )),
                _ => Err(io::Error::other("unexpected owned-child job notification")),
            };
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn notice(_pid: u32, _block: bool) -> io::Result<Option<ChildNotice>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "job observations require Darwin or Linux",
    ))
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;
    use crate::spawnfd::{Stdio, command};
    use std::io::Write as _;

    struct Owned(Child);
    impl std::ops::Deref for Owned {
        type Target = Child;
        fn deref(&self) -> &Child {
            &self.0
        }
    }
    impl std::ops::DerefMut for Owned {
        fn deref_mut(&mut self) -> &mut Child {
            &mut self.0
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn acknowledged_stops_preserve_ownership_until_the_actual_accounted_exit() {
        let mut child = Owned(
            command("/bin/sh")
                .args([
                    "-c",
                    "kill -STOP $$; read first; kill -STOP $$; read second; exit 7",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let pid = child.id();
        let mut input = child.stdin.take().unwrap();
        for line in [b"first\n".as_slice(), b"second\n".as_slice()] {
            let observed = child.wait_job_event().unwrap();
            assert_eq!(observed.id(), pid);
            assert_eq!(
                observed.notice(),
                ChildNotice::Stopped(ChildStop::Signal(libc::SIGSTOP))
            );
            assert_eq!(
                observed.acknowledge().unwrap(),
                Some(ChildEvent::Stopped(ChildStop::Signal(libc::SIGSTOP)))
            );
            assert!(!child.is_reaped());
            assert!(!child.exited().unwrap());
            assert!(
                child.try_job_event().unwrap().is_none(),
                "a stop is acknowledged only once"
            );
            child.resume().unwrap();
            input.write_all(line).unwrap();
        }
        let observed = child.wait_job_event().unwrap();
        assert_eq!(observed.notice(), ChildNotice::Exited);
        let Some(ChildEvent::Exited(usage)) = observed.acknowledge().unwrap() else {
            panic!("actual exit was not accounted")
        };
        assert_eq!(usage.status.code(), Some(7));
        assert!(child.is_reaped());
        assert_eq!(
            child.wait_with_rusage().unwrap(),
            usage,
            "the same owner retains complete accounting"
        );
        assert!(
            child.try_job_event().is_err(),
            "a reaped pid cannot mint new observations"
        );
    }

    #[test]
    fn dropped_exit_notice_remains_waitable_after_an_owned_kill_request() {
        for kill in [false, true] {
            let mut child = Owned(command("/bin/sh").args(["-c", "exit 7"]).spawn().unwrap());
            {
                let observed = child.wait_job_event().unwrap();
                assert_eq!(observed.notice(), ChildNotice::Exited);
            }
            if kill {
                let _ = child.kill();
            }
            let observed = child
                .try_job_event()
                .unwrap()
                .expect("unreaped exit remains observable");
            assert_eq!(observed.notice(), ChildNotice::Exited);
            let Some(ChildEvent::Exited(usage)) = observed.acknowledge().unwrap() else {
                panic!("actual accounted exit");
            };
            assert_eq!(usage.status.code(), Some(7));
        }
    }

    #[test]
    fn suspended_start_is_not_a_user_signal_or_an_exit() {
        let mut child = Owned(
            command("/bin/sh")
                .args(["-c", "exit 7"])
                .start_suspended(true)
                .spawn()
                .unwrap(),
        );
        let observed = child.wait_job_event().unwrap();
        assert_eq!(
            observed.notice(),
            ChildNotice::Stopped(ChildStop::SuspendedStart)
        );
        assert_eq!(
            observed.acknowledge().unwrap(),
            Some(ChildEvent::Stopped(ChildStop::SuspendedStart))
        );
        assert!(!child.is_reaped());
        assert!(!child.exited().unwrap());
        assert!(child.try_job_event().unwrap().is_none());
        child.resume().unwrap();
        let Some(ChildEvent::Exited(usage)) =
            child.wait_job_event().unwrap().acknowledge().unwrap()
        else {
            panic!("actual exit required")
        };
        assert_eq!(usage.status.code(), Some(7));
        assert_eq!(child.wait_with_rusage().unwrap(), usage);
    }
}
