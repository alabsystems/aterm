// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only native context framing. Only the owner's callback can authenticate
//! current custody; transport failures never settle or recreate an attempt.
use aterm_ctl::resource_admission_transport::{self as wire, Peer};
use aterm_uds::CtlStream;
use std::{io, time::Instant};
use trust_resource_controller::{ContextQuery, ContextReply, ContextResponse, ContextUnavailable};

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
pub(crate) fn serve(
    stream: &CtlStream,
    peer: Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
    observe: impl FnOnce(Peer, ContextQuery) -> io::Result<ContextReply>,
) -> io::Result<()> {
    let session = hello["session"].as_str().ok_or_else(refused)?;
    if hello["version"].as_u64() != Some(3)
        || hello["role"].as_str() != Some("native-context")
        || hello.as_object().is_none_or(|object| object.len() != 3)
    {
        return Err(refused());
    }
    let query = ContextQuery::decode(&wire::read_frame(stream, deadline)?)?;
    if query.binding.session != session || wire::local_peer(stream)? != peer {
        return Err(refused());
    }
    let response = match observe(peer, query.clone()) {
        Ok(reply) => ContextResponse::Ready(reply),
        Err(_) => ContextResponse::Unavailable(ContextUnavailable::for_query(
            &query,
            "no authenticated live native context".into(),
        )?),
    };
    if wire::local_peer(stream)? != peer {
        return Err(refused());
    }
    wire::write_frame(stream, &response.encode()?, deadline)?;
    // The peer may close immediately after ACK. Darwin can then discard its
    // credentials; the immutable stream and pre-response peer checks already
    // authenticate this read-only reply. ACK only retains our lifetime through
    // the client's post-response provenance validation.
    if wire::read_frame(stream, deadline)?.as_slice() != [1] {
        return Err(refused());
    }
    Ok(())
}
