// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed resolver queries have a distinct request role and authentication.
//! Only descriptor transfer is shared with the existing metadata transport.
use aterm_ctl::resource_admission_transport as transport;
use aterm_ctl::resource_subgroup_client::AuthorizedSubgroup;
use aterm_types::{
    resource_admission::{Binding, Submission},
    resource_subgroup as local,
};
use aterm_uds::CtlStream;
use std::{io, time::Instant};
use trust_resource_controller::{self as wire, resolver::SealedResolverQueryRequest};

pub(super) struct Incoming {
    pub subgroup: AuthorizedSubgroup,
    pub sealed: SealedResolverQueryRequest,
}

fn local_request(
    sealed: &SealedResolverQueryRequest,
    peer: transport::Peer,
) -> io::Result<local::Request> {
    sealed.validate()?;
    let request = &sealed.member;
    let wire::GnuTransport::NumericPipe { read, write } = request.delegations.gnu else {
        return Err(io::ErrorKind::PermissionDenied.into());
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
            // The original claim must cover this before receiving rights.
            // This is a lower bound, never a new memory allocation.
            memory_bytes: 1,
            label: "sealed resolver query".into(),
            timeout_ms: request.timeout_ns.map(|ns| ns.div_ceil(1_000_000)),
        },
        cpu: local::CpuDelegation::FixedBorrow { units: 1 },
        gnu: local::GnuTransport::NumericPipe {
            read_target: read.target,
            write_target: write.target,
        },
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
    receive_authorized(
        stream,
        job,
        deadline,
        crate::actor::authorize_resolver_query,
    )
}

pub(super) fn receive_authorized(
    stream: &CtlStream,
    job: &Binding,
    deadline: Instant,
    authorize: impl FnOnce(&local::Request, transport::Peer) -> bool,
) -> io::Result<Incoming> {
    stream.set_nonblocking(true)?;
    let peer = transport::local_peer(stream)?;
    let sealed = SealedResolverQueryRequest::decode(&transport::read_frame(stream, deadline)?)?;
    let request = local_request(&sealed, peer)?;
    let subgroup = crate::metadata_transport::receive_rights(
        stream, job, deadline, request, peer, false, authorize,
    )?;
    Ok(Incoming { subgroup, sealed })
}
