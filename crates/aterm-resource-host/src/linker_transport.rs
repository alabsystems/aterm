// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Direct native linker continuation. The actual compiler peer and its parked
//! original CPU are checked before rights are accepted; no proxy is spawned.
use aterm_ctl::resource_admission_transport as transport;
use aterm_ctl::resource_subgroup_client::AuthorizedSubgroup;
use aterm_types::{
    resource_admission::{Binding, Submission},
    resource_subgroup as local,
};
use aterm_uds::CtlStream;
use std::{io, time::Instant};
use trust_resource_controller::linker;

pub(crate) struct Incoming {
    pub(crate) subgroup: AuthorizedSubgroup,
    pub(crate) sealed: linker::Request,
}
pub(crate) fn receive(
    stream: &CtlStream,
    session: &str,
    deadline: Instant,
) -> io::Result<Incoming> {
    stream.set_nonblocking(true)?;
    let peer = transport::local_peer(stream)?;
    let sealed = linker::Request::decode(&transport::read_frame(stream, deadline)?)?;
    let member = &sealed.member;
    if member.binding.session != session {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let cpu = member.delegations.cpu.ok_or(io::ErrorKind::InvalidData)?;
    let job = Binding {
        session: session.into(),
        attempt: member.binding.root_attempt,
    };
    let request = local::Request {
        job: job.clone(),
        capability: local::Capability::from_bytes(member.binding.capability),
        member: Submission {
            binding: Binding {
                session: session.into(),
                attempt: member.member_attempt,
            },
            peer_pid: peer.pid,
            peer_birth: peer.birth,
            argv: std::iter::once(member.command.program.clone())
                .chain(member.command.args.iter().cloned())
                .collect(),
            cwd: member.command.cwd.clone(),
            env: member.command.environment.clone(),
            cpu_units: 1,
            memory_bytes: 1,
            label: "native parked compiler linker".into(),
            timeout_ms: None,
        },
        cpu: local::CpuDelegation::SharedBorrow {
            target_fd: cpu.target,
        },
        gnu: local::GnuTransport::None,
        fence_targets: Vec::new(),
    };
    request.validate()?;
    let subgroup = crate::metadata_transport::receive_rights(
        stream,
        &job,
        deadline,
        request,
        peer,
        true,
        |request, peer| crate::actor::authorize_linker(request, peer, &sealed),
    )?;
    Ok(Incoming { subgroup, sealed })
}
