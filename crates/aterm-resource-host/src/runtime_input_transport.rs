// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runtime input attachment to the authenticated original Job. Input custody
//! and a Retained observation establish neither SDK eligibility nor execution.

use aterm_ctl::resource_admission_transport::{self as transport, Peer};
use aterm_uds::CtlStream;
use std::{io, time::Instant};
use trust_resource_controller::{Channel, runtime_input_retention as wire};

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

struct Receiving {
    // Field drop order closes our actual channel before ending preparation,
    // including actor timeout, dropped replies and final-write failure.
    channel: Channel,
    preparing: crate::actor::RuntimeInputsPreparing,
}

pub(crate) fn support(
    stream: &CtlStream,
    peer: Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
) -> io::Result<()> {
    support_with(stream, peer, hello, deadline, crate::actor::context)
}

pub(crate) fn support_with(
    stream: &CtlStream,
    peer: Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
    observe: impl FnOnce(
        Peer,
        trust_resource_controller::ContextQuery,
    ) -> io::Result<trust_resource_controller::ContextReply>,
) -> io::Result<()> {
    let mut channel = Channel::new(stream.try_clone()?)?;
    let query = wire::SupportQuery::decode(&channel.receive(deadline)?)?;
    let expected: aterm_json::Value =
        aterm_json::from_slice(&query.hello()?).map_err(|_| refused())?;
    if *hello != expected || transport::local_peer(stream)? != peer {
        return Err(refused());
    }
    let reply = match observe(peer, query.query.clone()) {
        Ok(context) => wire::SupportReply::Supported { context },
        Err(_) => wire::SupportReply::Refused {
            root_attempt: query.query.binding.root_attempt,
            call_attempt: query.query.call_attempt,
            reason: "no authenticated live original runtime input context".into(),
        },
    };
    if transport::local_peer(stream)? != peer {
        return Err(refused());
    }
    channel.send(&reply.encode()?, deadline)?;
    wire::transport::finish_reply(&mut channel)?;
    if matches!(reply, wire::SupportReply::Supported { .. })
        && channel.receive(deadline)?.as_slice() != [1]
    {
        return Err(refused());
    }
    Ok(())
}

pub(crate) fn serve(
    stream: &CtlStream,
    peer: Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
) -> io::Result<()> {
    serve_with(
        stream,
        peer,
        hello,
        deadline,
        crate::actor::runtime_inputs_begin,
        crate::actor::context,
        crate::actor::runtime_inputs_retain,
    )
}

pub(crate) fn serve_with(
    stream: &CtlStream,
    peer: Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
    begin: impl FnOnce(Peer, wire::Request) -> io::Result<crate::actor::RuntimeInputsPreparing>,
    mut observe: impl FnMut(
        Peer,
        trust_resource_controller::ContextQuery,
    ) -> io::Result<trust_resource_controller::ContextReply>,
    retain: impl FnOnce(
        crate::actor::RuntimeInputsAttachment,
        Vec<std::os::fd::OwnedFd>,
        std::os::fd::OwnedFd,
    ) -> io::Result<trust_resource_controller::ContextReply>,
) -> io::Result<()> {
    let mut channel = Channel::new(stream.try_clone()?)?;
    let request = wire::Request::decode(&channel.receive(deadline)?)?;
    let expected: aterm_json::Value =
        aterm_json::from_slice(&request.hello()?).map_err(|_| refused())?;
    if *hello != expected || transport::local_peer(stream)? != peer {
        return Err(refused());
    }
    let preparing = match begin(peer, request.clone()) {
        Ok(preparing) => preparing,
        Err(_) => {
            channel.send(
                &wire::Reply::Refused {
                    key: request.key(),
                    reason: "no authenticated original runtime input owner".into(),
                }
                .encode()?,
                deadline,
            )?;
            return Err(refused());
        }
    };
    let accepted = preparing.context().clone();
    let mut receiving = Receiving { channel, preparing };
    let mut revalidate = || -> io::Result<()> {
        if transport::local_peer(stream)? != peer
            || observe(peer, request.query.clone())? != accepted
        {
            return Err(refused());
        }
        Ok(())
    };
    revalidate()?;
    receiving.channel.send(
        &wire::Reply::Accepted {
            key: request.key(),
            context: accepted.clone(),
        }
        .encode()?,
        deadline,
    )?;
    // Preparing remains live until all actual input work finishes, including
    // on malformed prefixes, timeout, peer death and actor queue failure.
    let descriptions =
        wire::transport::receive_inputs(&mut receiving.channel, &request, deadline, revalidate)?;
    let retained_transport = stream.try_clone()?.into();
    let context = retain(
        receiving.preparing.attachment()?,
        descriptions,
        retained_transport,
    )?;
    receiving.channel.send(
        &wire::Reply::Retained {
            key: request.key(),
            context,
        }
        .encode()?,
        deadline,
    )?;
    // Both write halves terminate their closed exchange. The original Job
    // retains this same accepted endpoint, independent of the wire outcome.
    stream.shutdown(std::net::Shutdown::Write)
}
