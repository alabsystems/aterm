// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Job-local cancellation messages. Only the supervisor holds native group authority.

use super::{JobControl, JobEvents, SupervisedChild, SupervisedEvent, notify_job_group};
use aterm_capacity::cohort::launch::CohortSignal;
use std::{
    io,
    os::{fd::AsFd as _, unix::net::UnixDatagram},
    sync::Arc,
    time::{Duration, Instant},
};

/// A job-local intent, not a process selector or group-control capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelRequest {
    /// TERM and CONT now; KILL at the configured grace deadline before root reap.
    Terminate,
    /// KILL now, before relinquishing the original leader pin.
    Kill,
}

/// Clonable bounded native mailbox sender. Kernel acceptance is reported by the
/// supervisor's events, not by successfully queueing this message.
#[derive(Clone, Debug)]
pub struct CancelHandle(Arc<UnixDatagram>);

impl CancelHandle {
    /// Queue one job-local request without blocking. No pid or decoded cohort is accepted.
    /// # Errors
    /// Disconnected/failed mailbox or a full kernel queue (WouldBlock); no silent loss.
    pub fn request(&self, request: CancelRequest) -> io::Result<()> {
        let byte = match request {
            CancelRequest::Terminate => 1,
            CancelRequest::Kill => 2,
        };
        if self.0.send(&[byte])? == 1 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "cancellation request was not queued",
            ))
        }
    }
}

#[derive(Debug)]
enum Phase {
    Idle,
    Terminating(Instant),
    ReadyToReap,
}

/// The sole receiver, moved into its native supervisor. Grace is explicit policy;
/// after TERM it retains the original root pin until KILL acceptance, even when
/// the root has already exited. Normal uncancelled exits are reaped immediately.
#[derive(Debug)]
pub struct Cancellation {
    inbox: UnixDatagram,
    wake: Arc<aterm_uds::childwake::ChildWake>,
    owner: u32,
    grace: Duration,
    phase: Phase,
    exit_observed: bool,
}

impl Cancellation {
    /// Create a bounded mailbox and kernel wake source in the controlling process.
    /// No signal disposition or mask changes, guardian or polling thread.
    /// # Errors
    /// Unsupported child notifications, invalid grace or native descriptor setup failure.
    pub fn channel(grace: Duration) -> io::Result<(CancelHandle, Self)> {
        Self::channel_on(grace, Arc::new(aterm_uds::childwake::ChildWake::new()?))
    }

    pub(super) fn channel_on(
        grace: Duration,
        wake: Arc<aterm_uds::childwake::ChildWake>,
    ) -> io::Result<(CancelHandle, Self)> {
        if Instant::now().checked_add(grace).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cancellation grace overflows the monotonic clock",
            ));
        }
        let (sender, inbox) = aterm_uds::kept_out(UnixDatagram::pair)?;
        sender.set_nonblocking(true)?;
        inbox.set_nonblocking(true)?;
        Ok((
            CancelHandle(Arc::new(sender)),
            Self {
                inbox,
                wake,
                owner: std::process::id(),
                grace,
                phase: Phase::Idle,
                exit_observed: false,
            },
        ))
    }

    pub(super) fn check_owner(&self) -> io::Result<()> {
        if self.owner == std::process::id() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cancellation inbox belongs to another process",
            ))
        }
    }

    pub(super) fn service(
        &mut self,
        owned: &mut SupervisedChild,
        events: &mut JobEvents<'_>,
    ) -> io::Result<()> {
        self.check_owner()?;
        let mut message = [0u8; 2];
        match self.inbox.recv(&mut message) {
            Ok(1) => match message[0] {
                1 => self.terminate(owned, events)?,
                2 => self.kill(owned, events)?,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid cancellation request",
                    ));
                }
            },
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid cancellation packet length",
                ));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
        if matches!(self.phase, Phase::Terminating(deadline) if Instant::now() >= deadline) {
            self.kill(owned, events)?;
        }
        Ok(())
    }

    pub(super) fn terminate(
        &mut self,
        owned: &mut SupervisedChild,
        events: &mut JobEvents<'_>,
    ) -> io::Result<()> {
        if !matches!(self.phase, Phase::Idle) {
            return Ok(());
        }
        let deadline = Instant::now().checked_add(self.grace).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cancellation grace overflows the monotonic clock",
            )
        })?;
        if !self.issue(owned, CohortSignal::Terminate)? {
            self.phase = Phase::ReadyToReap;
            return Ok(());
        }
        self.phase = Phase::Terminating(deadline);
        notify_job_group(CohortSignal::Terminate, events)?;
        if self.issue(owned, CohortSignal::Continue)? {
            notify_job_group(CohortSignal::Continue, events)
        } else {
            self.phase = Phase::ReadyToReap;
            Ok(())
        }
    }

    pub(super) fn kill(
        &mut self,
        owned: &mut SupervisedChild,
        events: &mut JobEvents<'_>,
    ) -> io::Result<()> {
        if matches!(self.phase, Phase::ReadyToReap) {
            return Ok(());
        }
        let accepted = self.issue(owned, CohortSignal::Kill)?;
        self.phase = Phase::ReadyToReap;
        if accepted {
            notify_job_group(CohortSignal::Kill, events)
        } else {
            Ok(())
        }
    }

    fn issue(&self, owned: &mut SupervisedChild, signal: CohortSignal) -> io::Result<bool> {
        match owned.request_group(signal) {
            Ok(()) => Ok(true),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) && owned.group_drained() => {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn holds_root(&self) -> bool {
        matches!(self.phase, Phase::Terminating(_))
    }

    #[cfg(target_os = "macos")]
    pub(super) fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Terminating(deadline) => Some(deadline),
            _ => None,
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn inbox(&self) -> std::os::fd::BorrowedFd<'_> {
        self.inbox.as_fd()
    }

    pub(super) fn requested(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    pub(super) const fn exit_observed(&self) -> bool {
        self.exit_observed
    }

    pub(super) fn observe_exit(&mut self, events: &mut JobEvents<'_>) -> io::Result<()> {
        if !self.exit_observed {
            self.exit_observed = true;
            if events(SupervisedEvent::RootExitObserved)? != JobControl::Wait {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "live group decisions use the cancellation inbox",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn wait(&self) -> io::Result<()> {
        use aterm_uds::ready;
        let found = match self.phase {
            Phase::Terminating(deadline) => {
                ready::either_until(self.wake.as_fd(), self.inbox.as_fd(), deadline)?
            }
            Phase::Idle | Phase::ReadyToReap => {
                Some(ready::either(self.wake.as_fd(), self.inbox.as_fd())?)
            }
        };
        if found.is_some_and(|ready| ready.first) {
            self.wake.drain()?;
        }
        Ok(())
    }
}
