// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native phase state belongs to the original live root, not a decoded ACK.
use super::*;
use std::os::fd::{AsRawFd, OwnedFd};
use trust_resource_controller::workload::host::phase as wire;

#[derive(Default)]
pub(super) struct Phases {
    last_sequence: u64,
    active: Option<Active>,
    original_gnu: Option<OriginalGnu>,
    seen: std::collections::BTreeSet<[u8; 16]>,
    drained: bool,
}
pub(super) struct Active {
    pub(super) id: [u8; 16],
    pub(super) kind: wire::Kind,
    pub(super) ordinal: u64,
}
struct OriginalGnu {
    descriptions: Arc<[OwnedFd; 2]>,
    backing: trust_resource_controller::FileIdentity,
}
impl Phases {
    pub(super) fn matches_gnu(
        &self,
        key: &host::produced::Key,
        expected: trust_resource_controller::FileIdentity,
    ) -> io::Result<()> {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.id != key.phase || active.ordinal != key.ordinal)
        {
            return Err(refused("contained operation has no original phase"));
        }
        let original = self
            .original_gnu
            .as_ref()
            .ok_or_else(|| refused("contained original GNU missing"))?;
        if original.backing != expected {
            return Err(refused("contained operation original GNU differs"));
        }
        no_supplemental(original)
    }
    pub(super) fn descriptions(&self, key: &host::produced::Key) -> io::Result<[OwnedFd; 2]> {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.id != key.phase || active.ordinal != key.ordinal)
        {
            return Err(refused("produced member has no original active phase"));
        }
        let original = self
            .original_gnu
            .as_ref()
            .ok_or_else(|| refused("original phase GNU missing"))?;
        no_supplemental(original)?;
        Ok([
            aterm_uds::kept_out(|| original.descriptions[0].try_clone())?,
            aterm_uds::kept_out(|| original.descriptions[1].try_clone())?,
        ])
    }
    pub(super) fn retained_descriptions(
        &self,
        key: &host::produced::Key,
    ) -> io::Result<Arc<[OwnedFd; 2]>> {
        let original = self
            .original_gnu
            .as_ref()
            .ok_or_else(|| refused("original phase GNU missing"))?;
        self.matches_gnu(key, original.backing)?;
        Ok(Arc::clone(&original.descriptions))
    }
    pub(super) fn active(&self) -> Option<&Active> {
        self.active.as_ref()
    }
    pub(super) fn cooperative(&self) -> bool {
        self.active.is_none() && !self.drained
    }
}
fn fifo(fd: &OwnedFd, access: libc::c_int) -> io::Result<trust_resource_controller::FileIdentity> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    let stat = spawnfd::descriptor_metadata(fd.as_raw_fd())?;
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if !stat.file_type().is_fifo()
        || stat.uid() != unsafe { libc::geteuid() }
        || stat.mode() & 0o077 != 0
        || stat.nlink() != 1
        || flags < 0
        || flags & libc::O_ACCMODE != access
    {
        return Err(refused("original named GNU FIFO profile differs"));
    }
    Ok(trust_resource_controller::FileIdentity {
        device: stat.dev(),
        inode: stat.ino(),
    })
}
fn no_supplemental(gnu: &OriginalGnu) -> io::Result<()> {
    if fifo(&gnu.descriptions[0], libc::O_RDONLY)? != gnu.backing
        || fifo(&gnu.descriptions[1], libc::O_WRONLY)? != gnu.backing
    {
        return Err(refused("original GNU descriptions changed"));
    }
    let mut bytes: libc::c_int = -1;
    // SAFETY: actual held FIFO, native FIONREAD output; no read, write, token
    // consumption or shared OFD flag mutation occurs.
    if unsafe { libc::ioctl(gnu.descriptions[0].as_raw_fd(), libc::FIONREAD, &mut bytes) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if bytes != 0 {
        return Err(refused(
            "original GNU supplemental tokens were not escrowed",
        ));
    }
    Ok(())
}
impl Owner {
    pub(in crate::actor) fn workload_phase(
        &mut self,
        peer: Peer,
        session: &str,
        request: &wire::Request,
        mut descriptions: Vec<OwnedFd>,
    ) -> io::Result<wire::Reply> {
        request.encode()?;
        self.fence.check_current()?;
        if self.faulted {
            return Err(refused("workload phase custody is faulted"));
        }
        let job = self
            .jobs
            .get_mut(&(session.to_owned(), request.key.root_attempt))
            .ok_or_else(|| refused("original phase root unavailable"))?;
        if !job.launched
            || job.reaped
            || job.is_settled()
            || job.cleanup_at.is_some()
            || peer.pid != job.child.child_mut().id()
            || peer.birth != job.root_birth
            || !matches!(job.custody, Custody::Cooperative(_))
        {
            return Err(refused(
                "phase peer is not its original live cooperative root",
            ));
        }
        let sdk = job
            .metadata_sdk
            .as_ref()
            .ok_or_else(|| refused("original phase SDK unavailable"))?;
        sdk.validate_loaded(peer.pid)?;
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("original phase owner unavailable"))?;
        let ready = state
            .ready
            .as_ref()
            .ok_or_else(|| refused("original workload is not ready"))?;
        if state.phase.drained
            || request.key.invocation_sha256 != ready.sha256
            || request.key.sequence
                != state
                    .phase
                    .last_sequence
                    .checked_add(1)
                    .ok_or_else(|| refused("phase sequence exhausted"))?
            || descriptions.len() != request.description_count()
        {
            return Err(refused(
                "phase is replayed, foreign or has wrong original descriptions",
            ));
        }
        let deadline = (job.started
            + job
                .timeout
                .ok_or_else(|| refused("original phase deadline missing"))?)
        .min(native_inputs::deadline(Instant::now()));
        ready._output.validate(deadline)?;
        if let Some(output) = &state.output {
            output.validate(&ready._output, deadline)?
        } else if !matches!(request.operation, wire::Operation::Drained) {
            return Err(refused("original graph output locks not retained"));
        }
        let reserved = state.primary.observations()?;
        if reserved.len() != job.cpu_ceiling as usize || self.debt.reserved()? != job.cpu_ceiling {
            return Err(refused(
                "original complete managed-domain reservation changed",
            ));
        }
        let quiescent = job.members.is_empty()
            && job.loans.is_empty()
            && state.bank.drained()
            && !state.produced.preparing();
        let decision = match request.operation {
            wire::Operation::Enter { phase, kind, gnu } => {
                if state.phase.active.is_some()
                    || state.phase.seen.contains(&phase)
                    || state.phase.seen.len() >= 4096
                {
                    return Err(refused(
                        "foreign phase is already active, replayed or exhausted",
                    ));
                }
                let write = descriptions
                    .pop()
                    .ok_or_else(|| refused("original GNU writer missing"))?;
                let read = descriptions
                    .pop()
                    .ok_or_else(|| refused("original GNU reader missing"))?;
                if fifo(&read, libc::O_RDONLY)? != gnu || fifo(&write, libc::O_WRONLY)? != gnu {
                    return Err(refused(
                        "phase GNU is not the offered original named transport",
                    ));
                }
                if let Some(original) = &state.phase.original_gnu {
                    if original.backing != gnu {
                        return Err(refused("phase substituted a different GNU server"));
                    }
                } else {
                    state.phase.original_gnu = Some(OriginalGnu {
                        descriptions: Arc::new([read, write]),
                        backing: gnu,
                    });
                }
                let original = state
                    .phase
                    .original_gnu
                    .as_ref()
                    .ok_or_else(|| refused("original GNU custody missing"))?;
                no_supplemental(original)?;
                if !quiescent {
                    wire::Decision::Busy
                } else {
                    state.phase.seen.insert(phase);
                    state.phase.active = Some(Active {
                        id: phase,
                        kind,
                        ordinal: request.key.sequence,
                    });
                    wire::Decision::Ready {
                        phase,
                        reserved_domain: job.cpu_ceiling,
                        gnu,
                    }
                }
            }
            wire::Operation::Leave { phase } => {
                if !state
                    .phase
                    .active
                    .as_ref()
                    .is_some_and(|active| active.id == phase)
                {
                    return Err(refused("foreign phase is not the original active phase"));
                }
                no_supplemental(
                    state
                        .phase
                        .original_gnu
                        .as_ref()
                        .ok_or_else(|| refused("original GNU missing"))?,
                )?;
                if !quiescent {
                    wire::Decision::Busy
                } else {
                    // Later DAG compilation may legitimately add or replace
                    // runtime outputs. No active child or preparation remains;
                    // retire this phase's held closures while keeping the
                    // original output-root and SDK owners for the whole graph.
                    state.produced.leave(phase);
                    state.phase.active = None;
                    wire::Decision::Left { phase }
                }
            }
            wire::Operation::Drained => {
                if !state.phase.cooperative() || !quiescent {
                    wire::Decision::Busy
                } else {
                    state.phase.drained = true;
                    wire::Decision::Drained
                }
            }
        };
        state.phase.last_sequence = request.key.sequence;
        let reply = wire::Reply {
            key: request.key,
            decision,
        };
        reply.encode()?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::OpenOptions,
        io::{Read as _, Write as _},
        os::unix::fs::OpenOptionsExt as _,
    };
    #[test]
    fn actual_fifo_escrow_observation_never_consumes_a_credit_or_changes_shared_flags() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("original-gnu");
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let mut reader = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let mut writer = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let read = reader.as_fd().try_clone_to_owned().unwrap();
        let write = writer.as_fd().try_clone_to_owned().unwrap();
        let backing = fifo(&read, libc::O_RDONLY).unwrap();
        assert_eq!(fifo(&write, libc::O_WRONLY).unwrap(), backing);
        assert!(fifo(&read, libc::O_WRONLY).is_err());
        let flags = unsafe { libc::fcntl(read.as_raw_fd(), libc::F_GETFL) };
        let original = OriginalGnu {
            descriptions: Arc::new([read, write]),
            backing,
        };
        no_supplemental(&original).unwrap();
        writer.write_all(b"+").unwrap();
        assert!(
            no_supplemental(&original).is_err(),
            "a still-available original credit must refuse Enter"
        );
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(
            byte, *b"+",
            "observation must not consume or replace an actual original token"
        );
        no_supplemental(&original).unwrap();
        let after = unsafe { libc::fcntl(original.descriptions[0].as_raw_fd(), libc::F_GETFL) };
        #[cfg(target_os = "macos")]
        let kernel_markers = 0x1000 | 0x2000; // XNU FMARK/FDEFER are not semantic OFD flags.
        #[cfg(not(target_os = "macos"))]
        let kernel_markers = 0;
        assert_eq!(flags & !kernel_markers, after & !kernel_markers);
    }
}
