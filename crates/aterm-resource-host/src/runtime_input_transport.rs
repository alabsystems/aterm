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
    finish_support_reply(
        &mut channel,
        matches!(reply, wire::SupportReply::Supported { .. }),
        deadline,
    )
}

/// Keep this SAME accepted peer alive through the client's final native audit.
/// ACK permits no release or execution: the client still checks its original
/// peer after sending it. Full peer close can invalidate Darwin's audit token,
/// so retain the socket until the client ends its send half after those checks.
/// Refusals also wait for that EOF so native checks do not mask their reason.
pub(crate) fn finish_support_reply(
    channel: &mut Channel,
    requires_ack: bool,
    deadline: Instant,
) -> io::Result<()> {
    wire::transport::finish_reply(channel)?;
    if requires_ack && channel.receive(deadline)?.as_slice() != [1] {
        return Err(refused());
    }
    wire::transport::require_reply_eof(channel, deadline)
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

#[cfg(test)]
mod tests {
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use trust_resource_provenance::process;

    fn connected() -> (aterm_tempfile::TempDir, UnixStream, UnixStream) {
        let directory = aterm_tempfile::Builder::new()
            .prefix("support-peer-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let path = directory.path().join("socket");
        let listener = UnixListener::bind(path).unwrap();
        let caller =
            UnixStream::connect(listener.local_addr().unwrap().as_pathname().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        (directory, caller, accepted)
    }

    #[test]
    fn support_peer_stays_native_authenticated_after_reply_eof_and_ack() {
        // These are transport-only frames, not a fabricated context or SDK.
        for requires_ack in [true, false] {
            let (_directory, caller, accepted) = connected();
            let own = process::read_identity(std::process::id()).unwrap();
            let peer = caller.try_clone().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let (done, returned) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let result = {
                    let mut channel = Channel::new(accepted).unwrap();
                    channel.send(b"transport-only reply", deadline).unwrap();
                    finish_support_reply(&mut channel, requires_ack, deadline)
                };
                // The result is sent only after every server socket owner drops.
                done.send(result).unwrap();
            });
            let mut client = Channel::new(caller).unwrap();
            assert_eq!(client.receive(deadline).unwrap(), b"transport-only reply");
            wire::transport::require_reply_eof(&mut client, deadline).unwrap();
            process::check_peer(&peer, own).unwrap();
            if requires_ack {
                client.send(&[1], deadline).unwrap();
            }
            assert!(matches!(
                returned.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            process::check_peer(&peer, own).unwrap();
            let mut changed = own;
            changed.birth ^= 1;
            assert!(process::check_peer(&peer, changed).is_err());

            wire::transport::finish_reply(&mut client).unwrap();
            returned
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap();
            server.join().unwrap();
            #[cfg(target_os = "macos")]
            assert!(process::check_peer(&peer, own).is_err());
        }
    }

    #[test]
    fn support_close_refuses_wrong_ack_and_trailing_frames() {
        for wrong_ack in [true, false] {
            let (_directory, caller, accepted) = connected();
            let deadline = Instant::now() + Duration::from_secs(5);
            let server = std::thread::spawn(move || {
                let mut channel = Channel::new(accepted).unwrap();
                channel.send(b"transport-only reply", deadline).unwrap();
                finish_support_reply(&mut channel, true, deadline)
            });
            let mut client = Channel::new(caller).unwrap();
            client.receive(deadline).unwrap();
            wire::transport::require_reply_eof(&mut client, deadline).unwrap();
            client
                .send(if wrong_ack { &[2] } else { &[1] }, deadline)
                .unwrap();
            if !wrong_ack {
                client.send(b"trailing frame", deadline).unwrap();
            }
            wire::transport::finish_reply(&mut client).unwrap();
            assert!(server.join().unwrap().is_err());
        }
    }

    #[test]
    fn support_close_keeps_the_original_deadline_when_client_remains_open() {
        let (_directory, caller, accepted) = connected();
        let deadline = Instant::now() + Duration::from_secs(5);
        let expired = Instant::now();
        let server = std::thread::spawn(move || {
            let mut channel = Channel::new(accepted).unwrap();
            channel.send(b"transport-only reply", deadline).unwrap();
            finish_support_reply(&mut channel, false, expired)
        });
        let mut client = Channel::new(caller).unwrap();
        client.receive(deadline).unwrap();
        wire::transport::require_reply_eof(&mut client, deadline).unwrap();
        assert_eq!(
            server.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }
}
