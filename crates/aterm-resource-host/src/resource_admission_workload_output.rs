// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual graph-wide original Layout descriptions, transferred by the live
//! trusted SDK before compilation. Never a lock constructor from data or paths.
use super::*;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStringExt as _;
use std::os::{fd::OwnedFd, unix::fs::MetadataExt as _};
use trust_resource_controller::workload::host::output as wire;

struct Lock {
    original: File,
    path: PathBuf,
    backing: trust_resource_controller::FileIdentity,
}
/// Close-only host copies survive phase image retirement, root death and any
/// uncertain native member retention. No Drop issues LOCK_UN on the shared OFD.
pub(in crate::actor::owner) struct OutputLocks {
    request: wire::Request,
    locks: Vec<Lock>,
}
fn physical_path(file: &File) -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut bytes = [0_u8; libc::PATH_MAX as usize];
        // Read-only location observation of the actual received description.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, bytes.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let n = bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| refused("original output lock path is unbounded"))?;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(
            bytes[..n].to_vec(),
        )))
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = file;
        Err(io::ErrorKind::Unsupported.into())
    }
}
fn validate_lock(lock: &Lock) -> io::Result<()> {
    trust_resource_provenance::require_no_acl_grants(&lock.original)?;
    let actual = lock.original.metadata()?;
    let flags = unsafe { libc::fcntl(lock.original.as_raw_fd(), libc::F_GETFL) };
    let fdflags = unsafe { libc::fcntl(lock.original.as_raw_fd(), libc::F_GETFD) };
    if flags < 0
        || flags & libc::O_ACCMODE != libc::O_RDWR
        || fdflags < 0
        || fdflags & libc::FD_CLOEXEC == 0
        || !actual.is_file()
        || actual.uid() != unsafe { libc::geteuid() }
        || actual.nlink() != 1
        || actual.mode() & 0o7022 != 0
        || (actual.dev(), actual.ino()) != (lock.backing.device, lock.backing.inode)
        || physical_path(&lock.original)? != lock.path
    {
        return Err(refused(
            "original output lock description changed or has unsupported ownership",
        ));
    }
    let named = std::fs::symlink_metadata(&lock.path)?;
    if !named.is_file()
        || named.file_type().is_symlink()
        || (named.dev(), named.ino()) != (actual.dev(), actual.ino())
    {
        return Err(refused("original output lock pathname was substituted"));
    }
    Ok(())
}
impl OutputLocks {
    pub(super) fn validate(
        &self,
        output: &trust_resource_provenance::program::produced::HeldOutputRoot,
        deadline: Instant,
    ) -> io::Result<()> {
        output.validate(deadline)?;
        if output.path().as_os_str().as_bytes() != self.request.output_root {
            return Err(refused("original output lock root changed"));
        }
        for lock in &self.locks {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            validate_lock(lock)?;
        }
        output.validate(deadline)
    }
}
impl Owner {
    pub(in crate::actor) fn workload_output_locks(
        &mut self,
        peer: Peer,
        session: &str,
        request: wire::Request,
        descriptions: Vec<OwnedFd>,
    ) -> io::Result<wire::Reply> {
        request.validate()?;
        self.fence.check_current()?;
        let job = self
            .jobs
            .get_mut(&(session.to_owned(), request.key.root_attempt))
            .ok_or_else(|| refused("original output handoff root unavailable"))?;
        if self.faulted
            || !job.launched
            || job.reaped
            || job.settled
            || job.cleanup_at.is_some()
            || peer.pid != job.child.child_mut().id()
            || peer.birth != job.root_birth
            || !matches!(job.custody, Custody::Cooperative(_))
        {
            return Err(refused(
                "output handoff peer is not its original live cooperative root",
            ));
        }
        job.metadata_sdk
            .as_ref()
            .ok_or_else(|| refused("original output handoff SDK missing"))?
            .validate_loaded(peer.pid)?;
        job::inspection(&self.fence, &self.debt, job)?;
        if self.debt.reserved()? != job.cpu_ceiling {
            return Err(refused(
                "original all-domain output handoff reservation changed",
            ));
        }
        let deadline = (job.started
            + job
                .timeout
                .ok_or_else(|| refused("original output handoff deadline missing"))?)
        .min(native_inputs::deadline(Instant::now()));
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("original output handoff owner missing"))?;
        let ready = state
            .ready
            .as_ref()
            .ok_or_else(|| refused("output handoff precedes actual readiness"))?;
        if request.key.invocation_sha256 != ready.sha256
            || request.output_root != ready.invocation.output_root
            || state
                .control
                .as_ref()
                .is_none_or(|control| control.closed())
        {
            return Err(refused(
                "output handoff is foreign to original live readiness",
            ));
        }
        let decision = (|| -> io::Result<OutputLocks> {
            if state.output_attempted
                || state.output.is_some()
                || !state.phase.cooperative()
                || !job.members.is_empty()
                || !job.loans.is_empty()
                || !state.bank.drained()
                || state
                    .census
                    .values()
                    .any(|entry| !matches!(entry.kind, CensusKind::Query))
            {
                return Err(refused(
                    "output lock handoff is replayed or follows compiler dispatch",
                ));
            }
            state.output_attempted = true;
            if descriptions.len() != request.locks.len() {
                return Err(refused("original output guard census is incomplete"));
            }
            ready._output.validate(deadline)?;
            let locks = request
                .locks
                .iter()
                .zip(descriptions)
                .map(|(observed, original)| Lock {
                    original: File::from(original),
                    path: PathBuf::from(std::ffi::OsStr::from_bytes(&observed.path)),
                    backing: observed.backing,
                })
                .collect();
            let held = OutputLocks {
                request: request.clone(),
                locks,
            };
            // Exact live trusted SDK delivery and its unsafe original capsule
            // contract establish OFD lineage/held state. These observations
            // check identity only; no historical flock flag or re-lock grants.
            held.validate(&ready._output, deadline)?;
            Ok(held)
        })();
        let reply = match decision {
            Ok(held) => {
                state.output = Some(Arc::new(held));
                wire::Reply::Retained {
                    key: request.key,
                    locks: request.locks,
                }
            }
            Err(error) => {
                let reason: String = error
                    .to_string()
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(512)
                    .collect();
                wire::Reply::Refused {
                    key: request.key,
                    reason: if reason.is_empty() {
                        "original output lock handoff refused".into()
                    } else {
                        reason
                    },
                }
            }
        };
        reply.encode()?;
        Ok(reply)
    }
}
