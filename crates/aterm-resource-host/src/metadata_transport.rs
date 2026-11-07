// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The distinct sealed metadata negotiation. Bytes select an original job;
//! native peer, original resources and held program custody authorize launch.
use aterm_ctl::resource_admission_transport as transport;
use aterm_ctl::resource_subgroup_client::{AuthorizedSubgroup, Delegations, Rights};
use aterm_types::{
    resource_admission::{Binding, Submission},
    resource_subgroup as local,
};
use aterm_uds::{
    CtlStream,
    ready::{self, Interest},
};
use std::{io, os::fd::AsFd, time::Instant};
use trust_resource_controller::{self as wire, MetadataCpu, SealedMetadataRequest};

pub(super) struct Incoming {
    pub subgroup: AuthorizedSubgroup,
    pub sealed: SealedMetadataRequest,
    #[cfg(all(test, target_os = "macos"))]
    pub request_bytes: Vec<u8>,
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "sealed metadata transport refused",
    )
}
fn target(source: wire::Descriptor) -> u32 {
    source.target
}
/// Translate only representation. CPU and memory authority stay in the owner.
fn local_request(
    sealed: &SealedMetadataRequest,
    peer: transport::Peer,
) -> io::Result<local::Request> {
    sealed.validate()?;
    let request = &sealed.member;
    let cpu = match sealed.cpu {
        MetadataCpu::SharedBorrow => local::CpuDelegation::SharedBorrow {
            target_fd: target(request.delegations.cpu.ok_or_else(invalid)?),
        },
        MetadataCpu::FixedBorrow => local::CpuDelegation::FixedBorrow { units: 1 },
    };
    let gnu = match &request.delegations.gnu {
        wire::GnuTransport::NumericPipe { read, write } => local::GnuTransport::NumericPipe {
            read_target: target(*read),
            write_target: target(*write),
        },
        // The first closed purpose supports its actual host-owned numeric
        // pipe. A named or unrelated pool never becomes a replacement root.
        _ => return Err(invalid()),
    };
    let mut argv = vec![request.command.program.clone()];
    argv.extend(request.command.args.clone());
    let result = local::Request {
        job: Binding {
            session: request.binding.session.clone(),
            attempt: request.binding.root_attempt,
        },
        capability: local::Capability::from_bytes(request.binding.capability),
        member: Submission {
            binding: Binding {
                session: request.binding.session.clone(),
                attempt: request.member_attempt,
            },
            peer_pid: peer.pid,
            peer_birth: peer.birth,
            argv,
            cwd: request.command.cwd.clone(),
            env: request.command.environment.clone(),
            cpu_units: 1,
            // No new claim: validate against the actual original claim before
            // preparation. The wire intentionally carries no numeric memory grant.
            memory_bytes: 1,
            label: "sealed metadata".into(),
            timeout_ms: request.timeout_ns.map(|ns| ns.div_ceil(1_000_000)),
        },
        cpu,
        gnu,
        fence_targets: Vec::new(),
    };
    result.validate()?;
    Ok(result)
}
pub(super) fn receive(
    stream: &CtlStream,
    job: &Binding,
    deadline: Instant,
) -> io::Result<Incoming> {
    receive_authorized(stream, job, deadline, crate::actor::authorize_metadata)
}
pub(super) fn receive_authorized(
    stream: &CtlStream,
    job: &Binding,
    deadline: Instant,
    authorize: impl FnOnce(&local::Request, transport::Peer) -> bool,
) -> io::Result<Incoming> {
    stream.set_nonblocking(true)?;
    let peer = transport::local_peer(stream)?;
    let request_bytes = transport::read_frame(stream, deadline)?;
    let sealed = SealedMetadataRequest::decode(&request_bytes)?;
    let request = local_request(&sealed, peer)?;
    let subgroup = receive_rights(
        stream,
        job,
        deadline,
        request,
        peer,
        sealed.cpu == MetadataCpu::SharedBorrow,
        authorize,
    )?;
    Ok(Incoming {
        subgroup,
        sealed,
        #[cfg(all(test, target_os = "macos"))]
        request_bytes,
    })
}

/// Shared descriptor transfer only. Each purpose validates its own closed
/// request and original owner before this receives any execution rights.
pub(super) fn receive_rights(
    stream: &CtlStream,
    job: &Binding,
    deadline: Instant,
    request: local::Request,
    peer: transport::Peer,
    shared_cpu: bool,
    authorize: impl FnOnce(&local::Request, transport::Peer) -> bool,
) -> io::Result<AuthorizedSubgroup> {
    if shared_cpu != matches!(request.cpu, local::CpuDelegation::SharedBorrow { .. }) {
        return Err(invalid());
    }
    if &request.job != job || !authorize(&request, peer) || transport::local_peer(stream)? != peer {
        transport::write_frame(stream, &[0], deadline)?;
        return Err(invalid());
    }
    transport::write_frame(stream, local::READY, deadline)?;
    let count = request.fd_count()?;
    let rights = loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let result = aterm_uds::kept_out(|| {
            let mut marker = [0; 2];
            let got = aterm_uds::fdpass::recv_with_fds(stream, &mut marker, count)?;
            if got.bytes != 1 || marker[0] != local::FD_MARKER || got.fds.len() != count {
                return Err(invalid());
            }
            let mut fds = got.fds.into_iter();
            Ok(Rights {
                stdin: fds.next().ok_or_else(invalid)?,
                stdout: fds.next().ok_or_else(invalid)?,
                stderr: fds.next().ok_or_else(invalid)?,
                events: fds.next().ok_or_else(invalid)?,
                delegated: Delegations {
                    cpu: if shared_cpu {
                        Some(fds.next().ok_or_else(invalid)?)
                    } else {
                        None
                    },
                    gnu: fds.collect(),
                    fences: Vec::new(),
                },
            })
        });
        match result {
            Ok(rights) => break rights,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, deadline)?
            }
            Err(error) => return Err(error),
        }
    };
    let gnu_count = count - 4 - usize::from(shared_cpu);
    if transport::local_peer(stream)? != peer || rights.delegated.gnu.len() != gnu_count {
        return Err(invalid());
    }
    let events = CtlStream::from(rights.events);
    if transport::local_peer(&events)? != peer {
        return Err(invalid());
    }
    events.set_nonblocking(true)?;
    Ok(AuthorizedSubgroup {
        request,
        peer,
        rights: Rights {
            events: events.into(),
            ..rights
        },
    })
}
