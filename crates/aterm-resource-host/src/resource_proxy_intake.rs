// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed native-helper intake. Neither provisional acceptance nor returned
//! transport proof authorizes execution or creates a resource reservation.
use crate::resource_producer::TransportProof;
use aterm_ctl::{
    resource_admission_transport::{self as transport, Peer},
    resource_subgroup_client::Delegations,
};
use aterm_types::{resource_admission::Binding, resource_subgroup as subgroup};
use aterm_uds::{
    CtlStream,
    ready::{self, Interest},
};
use std::{
    io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::UnixStream,
    },
    sync::Arc,
    time::Instant,
};
use trust_resource_controller::{GnuTransport, Request};
use trust_resource_provenance::{PinnedProgram, process};

pub(crate) struct Rights {
    pub stdin: OwnedFd,
    pub stdout: OwnedFd,
    pub stderr: OwnedFd,
    pub events: OwnedFd,
    pub delegated: Delegations,
}
impl aterm_uds::spawn_lock::Descriptors for Rights {
    fn mark(&self) -> io::Result<()> {
        for fd in [&self.stdin, &self.stdout, &self.stderr, &self.events]
            .into_iter()
            .chain(self.delegated.cpu.iter())
            .chain(self.delegated.gnu.iter())
            .chain(self.delegated.fences.iter())
        {
            aterm_uds::keep_out_of_children(fd)?;
        }
        Ok(())
    }
}
pub(crate) struct Incoming {
    pub request: Request,
    pub helper: Peer,
    pub proof: TransportProof,
    pub rights: Rights,
}
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "native helper intake refused",
    )
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn pin(
    stream: &CtlStream,
    program: &PinnedProgram,
    helper: Peer,
    deadline: Instant,
) -> io::Result<()> {
    if transport::local_peer(stream)? != helper {
        return Err(refused());
    }
    let native = process::read_identity(helper.pid)?;
    if native.birth != helper.birth {
        return Err(refused());
    }
    program.validate_cached_process(native, deadline)?;
    if transport::local_peer(stream)? != helper || process::read_identity(helper.pid)? != native {
        return Err(refused());
    }
    Ok(())
}
fn gnu_count(request: &Request) -> usize {
    match request.delegations.gnu {
        GnuTransport::None => 0,
        _ => 2,
    }
}
fn count(request: &Request) -> io::Result<usize> {
    request.validate()?;
    if !request.delegations.fences.is_empty() {
        return Err(invalid());
    }
    let count = 5 + usize::from(request.delegations.cpu.is_some()) + gnu_count(request);
    if count > aterm_uds::fdpass::MAX_FDS {
        return Err(invalid());
    }
    Ok(count)
}
/// Runtime has authenticated the native role and already sent PREPARE. READY
/// admits only descriptor transfer; ACK belongs to the parent after actor acceptance.
/// # Errors
/// Wrong cached helper, job, request, provisional policy, rights or native producer
/// refuses. Every error drops all received descriptors, without launch or grant.
pub(crate) fn receive(
    stream: &CtlStream,
    expected_job: &Binding,
    program: &Arc<PinnedProgram>,
    deadline: Instant,
    provisional: impl FnOnce(&Request, Peer) -> bool,
) -> io::Result<Incoming> {
    stream.set_nonblocking(true)?;
    let helper = transport::local_peer(stream)?;
    pin(stream, program, helper, deadline)?;
    let request = Request::decode(&transport::read_frame(stream, deadline)?)?;
    let fd_count = count(&request)?;
    if request.binding.session != expected_job.session
        || request.binding.root_attempt != expected_job.attempt
        || !provisional(&request, helper)
    {
        transport::write_frame(stream, &[0], deadline)?;
        return Err(refused());
    }
    pin(stream, program, helper, deadline)?;
    transport::write_frame(stream, subgroup::READY, deadline)?;
    let (rights, producer) = receive_rights(stream, &request, fd_count, deadline)?;
    pin(stream, program, helper, deadline)?;
    let events = CtlStream::from(rights.events);
    if transport::local_peer(&events)? != helper {
        return Err(refused());
    }
    events.set_nonblocking(true)?;
    let proof = TransportProof::authenticate(
        UnixStream::from(producer),
        helper,
        program.clone(),
        request.binding.root_attempt,
        request.member_attempt,
        deadline,
    )?;
    pin(stream, program, helper, deadline)?;
    proof.validate()?;
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(Incoming {
        request,
        helper,
        proof,
        rights: Rights {
            events: events.into(),
            ..rights
        },
    })
}
fn receive_rights(
    stream: &CtlStream,
    request: &Request,
    fd_count: usize,
    deadline: Instant,
) -> io::Result<(Rights, OwnedFd)> {
    loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let result = aterm_uds::kept_out(|| {
            let mut marker = [0; 2];
            let received = aterm_uds::fdpass::recv_with_fds(stream, &mut marker, fd_count)?;
            if received.bytes != 1
                || marker[0] != subgroup::FD_MARKER
                || received.fds.len() != fd_count
            {
                return Err(invalid());
            }
            let mut fds = received.fds.into_iter();
            let stdin = fds.next().ok_or_else(invalid)?;
            let stdout = fds.next().ok_or_else(invalid)?;
            let stderr = fds.next().ok_or_else(invalid)?;
            let events = fds.next().ok_or_else(invalid)?;
            let producer = fds.next().ok_or_else(invalid)?;
            let cpu = if request.delegations.cpu.is_some() {
                Some(fds.next().ok_or_else(invalid)?)
            } else {
                None
            };
            let gnu = fds.by_ref().take(gnu_count(request)).collect::<Vec<_>>();
            if gnu.len() != gnu_count(request) || fds.next().is_some() {
                return Err(invalid());
            }
            Ok((
                Rights {
                    stdin,
                    stdout,
                    stderr,
                    events,
                    delegated: Delegations {
                        cpu,
                        gnu,
                        fences: Vec::new(),
                    },
                },
                producer,
            ))
        });
        match result {
            Ok(rights) => return Ok(rights),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, deadline)?
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}
#[cfg(test)]
#[path = "resource_proxy_intake/tests.rs"]
mod tests;
