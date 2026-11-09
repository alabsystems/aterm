// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The same accepted original graph channel, never an execution/SDK grant.
use aterm_ctl::resource_admission_transport::{self as transport, Peer};
use aterm_uds::CtlStream;
use std::{io, time::Instant};
use trust_resource_controller::{Channel, bootstrap_graph as wire};

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}
struct Receiving {
    // Actual channel closes before preparation can leave settlement's barrier,
    // including actor timeout, dropped reply and partially delivered rights.
    channel: Channel,
    preparing: crate::actor::BootstrapGraphPreparing,
}
pub(crate) fn support(
    stream: &CtlStream,
    peer: Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
) -> io::Result<()> {
    let mut channel = Channel::new(stream.try_clone()?)?;
    let query = wire::SupportQuery::decode(&channel.receive(deadline)?)?;
    let expected: aterm_json::Value =
        aterm_json::from_slice(&query.hello()?).map_err(|_| refused())?;
    if *hello != expected || transport::local_peer(stream)? != peer {
        return Err(refused());
    }
    let observation = crate::actor::bootstrap_graph_support(peer, query.clone());
    let reply = match observation {
        Ok(observation) => {
            // Sample the SAME queued Job deadline immediately before encoding;
            // neither this connection nor its actor wait restarts the Root.
            let remaining = observation
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or(io::ErrorKind::TimedOut)?;
            let remaining_root_ns = u64::try_from(remaining.as_nanos()).map_err(|_| refused())?;
            wire::SupportReply::Supported {
                key: query.key.clone(),
                context: Box::new(observation.context),
                remaining_root_ns,
            }
        }
        Err(_) => wire::SupportReply::Refused {
            key: query.key.clone(),
            reason: "no authenticated original fixed Bootstrap Root/SDK custody".into(),
        },
    };
    time(deadline)?;
    if transport::local_peer(stream)? != peer {
        return Err(refused());
    }
    channel.send(&reply.encode()?, deadline)?;
    trust_resource_controller::runtime_input_retention::transport::finish_reply(&mut channel)?;
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
    control_deadline: Instant,
) -> io::Result<()> {
    let mut channel = Channel::new(stream.try_clone()?)?;
    let request = wire::Enrollment::decode(&channel.receive(control_deadline)?)?;
    let expected: aterm_json::Value =
        aterm_json::from_slice(&request.hello()?).map_err(|_| refused())?;
    if *hello != expected || transport::local_peer(stream)? != peer {
        return Err(refused());
    }
    let preparing = match crate::actor::bootstrap_graph_begin(peer, request.clone()) {
        Ok(preparing) => preparing,
        Err(_) => {
            channel.send(
                &wire::EnrollmentReply::Refused {
                    key: request.key,
                    reason: "original Bootstrap graph enrollment refused".into(),
                }
                .encode()?,
                control_deadline,
            )?;
            return Err(refused());
        }
    };
    let transfer_deadline = control_deadline.min(preparing.deadline());
    let context = preparing.context().clone();
    let mut receiving = Receiving { channel, preparing };
    let digest = aterm_digest::Sha256::digest(&request.binding_preimage()?);
    receiving.channel.send(
        &wire::EnrollmentReply::Prepared {
            key: request.key.clone(),
            enrollment_sha256: digest,
            context: context.clone(),
        }
        .encode()?,
        transfer_deadline,
    )?;
    let revalidate = || -> io::Result<()> {
        if transport::local_peer(stream)? != peer
            || crate::actor::bootstrap_graph_context(peer, request.key.clone())? != context
        {
            return Err(refused());
        }
        Ok(())
    };
    let gnu = wire::transport::receive_gnu(
        &mut receiving.channel,
        &request,
        transfer_deadline,
        revalidate,
    )?;
    let current = crate::actor::bootstrap_graph_retain(
        receiving.preparing.enrollment_attachment()?,
        gnu,
        stream.try_clone()?.into(),
    )?;
    revalidate()?;
    receiving.channel.send(
        &wire::EnrollmentReply::Enrolled {
            key: request.key.clone(),
            enrollment_sha256: digest,
            context: current,
        }
        .encode()?,
        transfer_deadline,
    )?;

    // Costly real Steps/lock acquisition may happen before output exists. The
    // original Job deadline includes queue/preparation and is never an RPC reset.
    let deadline = receiving.preparing.deadline();
    let output = wire::OutputAttachment::decode(&receiving.channel.receive(deadline)?)?;
    let token = crate::actor::bootstrap_graph_output_begin(
        receiving.preparing.output_transfer(output.clone())?,
    )?;
    let digest = aterm_digest::Sha256::digest(&output.binding_preimage()?);
    receiving.channel.send(
        &wire::OutputReply::Prepared {
            key: output.key.clone(),
            attachment_sha256: digest,
            context: token.context().clone(),
        }
        .encode()?,
        deadline,
    )?;
    let descriptions =
        wire::transport::receive_output(&mut receiving.channel, &output, deadline, revalidate)?;
    let current = crate::actor::bootstrap_graph_output_retain(token, descriptions)?;
    revalidate()?;
    receiving.channel.send(
        &wire::OutputReply::Attached {
            key: output.key,
            attachment_sha256: digest,
            context: current,
        }
        .encode()?,
        deadline,
    )?;
    // No graph-producing or retirement operation is implemented by an EOF.
    // Keep the real preparation through the accepted stream's physical end.
    trust_resource_controller::runtime_input_retention::transport::require_reply_eof(
        &mut receiving.channel,
        deadline,
    )
}

pub(crate) fn query(
    stream: &CtlStream,
    peer: Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
) -> io::Result<()> {
    let started = Instant::now();
    let mut channel = Channel::new(stream.try_clone()?)?;
    let request = wire::query::Request::decode(&channel.receive(deadline)?)?;
    let expected: aterm_json::Value =
        aterm_json::from_slice(&request.hello()?).map_err(|_| refused())?;
    if *hello != expected || transport::local_peer(stream)? != peer {
        return Err(refused());
    }
    time(deadline)?;
    // The actual issuer, not this reply or Request, owns the original epoch
    // loan/native subgroup and accepted stream until native/ACK closure.
    crate::actor::submit_bootstrap_query(peer, request, stream.try_clone()?, started, deadline)
}

/// Distinct object protocol. The actor retains the actual operation and stream
/// before Prepared, so partial source delivery cannot disappear from settlement.
pub(crate) fn object(
    stream: &CtlStream,
    peer: Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
) -> io::Result<()> {
    let started = Instant::now();
    let mut channel = Channel::new(stream.try_clone()?)?;
    let request = wire::object::Request::decode(&channel.receive(deadline)?)?;
    let expected: aterm_json::Value =
        aterm_json::from_slice(&request.hello()?).map_err(|_| refused())?;
    if *hello != expected || transport::local_peer(stream)? != peer {
        return Err(refused());
    }
    time(deadline)?;
    let offer = crate::actor::prepare_bootstrap_object(
        peer,
        request,
        stream.try_clone()?,
        started,
        deadline,
    )?;
    let source = (|| {
        let request = offer.request();
        channel.send(
            &wire::object::Event::Prepared(Box::new(offer.prepared().clone()))
                .encode(request, offer.digest())?,
            offer.deadline(),
        )?;
        wire::object::transport::receive_source(&mut channel, request, offer.deadline(), || {
            time(offer.deadline())?;
            if transport::local_peer(stream)? != peer
                || crate::actor::bootstrap_graph_context(peer, request.key.graph.clone())?
                    != *offer.context()
            {
                return Err(refused());
            }
            Ok(())
        })
    })();
    // Failed transfer is also delivered to the same owning slot, so lexical
    // no-launch cleanup can close work only after real pipe/socket shutdown.
    crate::actor::bootstrap_object_source(peer, offer, source)
}
