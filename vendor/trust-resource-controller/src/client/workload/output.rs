//! Same original root control transfers the actual close-only Layout OFDs.
#[cfg(test)]
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::os::unix::fs::MetadataExt;

use super::*;
use crate::workload::host::output::{self, Reply, Request};

/// Positive live handoff observation, not a lock constructor or execution
/// grant. The host privately retains the actual originals through root/cohort
/// settlement; it does not release them at a phase boundary or this Drop.
pub struct RetainedOutputLocks {
    request: Request,
}
impl RetainedOutputLocks {
    pub fn observation(&self) -> &Request {
        &self.request
    }
}

fn originals(request: &Request, descriptions: &[BorrowedFd<'_>]) -> io::Result<()> {
    request.validate()?;
    if descriptions.len() != request.locks.len() {
        return Err(refused());
    }
    let mut fds = std::collections::BTreeSet::new();
    for (observed, fd) in request.locks.iter().zip(descriptions) {
        // SAFETY: observe flags/identity of an existing caller-owned description.
        let mode = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        let meta = File::from(fd.try_clone_to_owned()?).metadata()?;
        let uid = unsafe { libc::geteuid() };
        if mode < 0
            || mode & libc::O_ACCMODE != libc::O_RDWR
            || flags < 0
            || flags & libc::FD_CLOEXEC == 0
            || !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != uid
            || meta.mode() & 0o022 != 0
            || (meta.dev(), meta.ino()) != (observed.backing.device, observed.backing.inode)
            || !fds.insert(fd.as_raw_fd())
        {
            return Err(refused());
        }
    }
    Ok(())
}
impl WorkloadControl {
    /// Transfer the complete actual original workload Layout guard census.
    /// # Safety
    /// SAME trusted SDK coordinator owns the original compile/output capsule;
    /// every guard was switched to close-only BEFORE any duplicate/SCM offer.
    /// These borrowed descriptions come directly from that capsule, not opens,
    /// paths, saved records, generic files or another worktree. No compiler was
    /// dispatched; all earlier query loans retired. No guard is omitted or
    /// truncated to satisfy MAX_LOCKS. The caller retains the same capsule
    /// through actual compilation/actions, and Host retains received originals
    /// independently through real root/cohort settlement. No re-lock is allowed.
    pub unsafe fn retain_output_locks(
        &mut self,
        request: &Request,
        descriptions: &[BorrowedFd<'_>],
        deadline: Instant,
    ) -> Result<RetainedOutputLocks, WorkloadControlError> {
        if self.failed
            || self.drained
            || self.pending.is_some()
            || self.active_phase.is_some()
            || self.output_locks.is_some()
            || request.key.root_attempt != self.root_attempt
            || request.key.invocation_sha256 != self.invocation_sha256
        {
            return Err(WorkloadControlError::BeforeRequest(refused()));
        }
        let payload = request.encode().map_err(WorkloadControlError::BeforeRequest)?;
        originals(request, descriptions).map_err(WorkloadControlError::BeforeRequest)?;
        let result = (|| {
            self.host.validate(deadline)?;
            self.channel.send(&payload, deadline)?;
            let stream =
                std::os::unix::net::UnixStream::from(self.channel.as_fd().try_clone_to_owned()?);
            jobserver_utils::descriptor_transfer::send(
                &stream,
                descriptions,
                output::MARKER,
                deadline,
            )?;
            let reply = Reply::decode(&self.channel.receive(deadline)?, request)?;
            self.host.validate(deadline)?;
            originals(request, descriptions)?;
            Ok(reply)
        })();
        match result {
            Ok(Reply::Retained { .. }) => {
                self.output_locks = Some(request.clone());
                Ok(RetainedOutputLocks { request: request.clone() })
            }
            Ok(Reply::Refused { reason, .. }) => Err(WorkloadControlError::Refused { reason }),
            Err(error) => {
                self.failed = true;
                Err(WorkloadControlError::Unknown(error))
            }
        }
    }
}

#[cfg(test)]
mod tests;
