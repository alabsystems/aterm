// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One process-affine execution owner. Control/UI readers never own jobs and
//! never turn lifecycle observations into reservations.
use aterm_ctl::resource_admission_transport::{AuthorizedSubmission, Peer};
use aterm_types::{
    resource_admission::{Binding, Event, EventFrame, Submission},
    resource_inspection::Inspection,
};
use std::{
    io,
    sync::{
        OnceLock,
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};

#[path = "resource_admission_owner.rs"]
mod owner;

const RPC: Duration = Duration::from_secs(2);
const MAX_REQUESTS: usize = 256;
type Key = (String, [u8; 16]);
fn key(binding: &Binding) -> Key {
    (binding.session.clone(), binding.attempt)
}
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "retained host admission is not ready",
    )
}

enum Message {
    Wake,
    Capabilities(SyncSender<aterm_json::Value>),
    Idle(SyncSender<bool>),
    Authorize {
        binding: Binding,
        peer: Peer,
        reply: SyncSender<bool>,
    },
    Submit(Box<AuthorizedSubmission>),
    SubmitMember(Box<aterm_ctl::resource_subgroup_client::AuthorizedSubgroup>),
    AuthorizeMember {
        request: owner::members::Authorization,
        reply: SyncSender<bool>,
    },
    Context {
        peer: Peer,
        query: trust_resource_controller::ContextQuery,
        reply: SyncSender<io::Result<trust_resource_controller::ContextReply>>,
    },
    Inspect {
        peer: Peer,
        session: String,
        reply: SyncSender<io::Result<Inspection>>,
    },
}
struct Handle {
    sender: SyncSender<Message>,
}
static HOST: OnceLock<Handle> = OnceLock::new();
pub(super) fn prerequisites() -> io::Result<()> {
    owner::Owner::prerequisites()
}
pub(super) fn activate_process() -> io::Result<()> {
    if HOST.get().is_some() {
        return Err(unavailable());
    }
    let owner = owner::Owner::open()?;
    let (sender, receiver) = mpsc::sync_channel(MAX_REQUESTS);
    std::thread::Builder::new()
        .name("resource-admission-owner".into())
        .spawn(move || run(receiver, owner))?;
    HOST.set(Handle { sender }).map_err(|_| unavailable())
}
fn actor() -> io::Result<&'static Handle> {
    // Actual child controls must never be created in the GUI process.
    // Only the authenticated immutable custodian bootstrap enables this role.
    HOST.get().ok_or_else(unavailable)
}
fn send(message: Message) -> io::Result<()> {
    actor()?.sender.try_send(message).map_err(|_| unavailable())
}
/// Query canonical readiness; no reservation is created.
pub fn capabilities() -> aterm_json::Value {
    let (reply, read) = mpsc::sync_channel(1);
    if send(Message::Capabilities(reply)).is_ok()
        && let Ok(value) = read.recv_timeout(RPC)
    {
        return value;
    }
    aterm_types::resource_inspection::unavailable("retained host unavailable")
}
pub(super) fn idle() -> bool {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::Idle(reply)).is_ok() && read.recv_timeout(RPC).unwrap_or(false)
}
/// Check an already authenticated GUI submission. The caller must establish
/// actual kernel peer and live session authorization before this boundary.
pub fn authorize(request: &Submission, peer: Peer) -> bool {
    if request.validate().is_err()
        || request.peer_pid != peer.pid
        || request.peer_birth != peer.birth
    {
        return false;
    }
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::Authorize {
        binding: request.binding.clone(),
        peer,
        reply,
    })
    .is_ok()
        && read.recv_timeout(RPC).unwrap_or(false)
}
/// Accept four privately delegated descriptors after caller authorization.
/// Acceptance alone supplies no launch or allocation fact.
pub fn submit(submission: AuthorizedSubmission) -> io::Result<()> {
    send(Message::Submit(Box::new(submission)))
}
pub(super) fn authorize_member(
    request: &aterm_types::resource_subgroup::Request,
    peer: Peer,
) -> bool {
    let Ok(request) = owner::members::Authorization::of(request, peer) else {
        return false;
    };
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::AuthorizeMember { request, reply }).is_ok()
        && read.recv_timeout(RPC).unwrap_or(false)
}
pub(super) fn member_binding(value: &aterm_json::Value) -> io::Result<Binding> {
    owner::members::binding(value)
}
pub(super) fn submit_member(
    member: aterm_ctl::resource_subgroup_client::AuthorizedSubgroup,
) -> io::Result<()> {
    send(Message::SubmitMember(Box::new(member)))
}
/// Observe only original custody for the authenticated kernel peer tree.
pub fn inspect(peer: Peer, session: &str) -> io::Result<Inspection> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::Inspect {
        peer,
        session: session.to_owned(),
        reply,
    })?;
    read.recv_timeout(RPC).map_err(|_| unavailable())?
}

pub(super) fn context(
    peer: Peer,
    query: trust_resource_controller::ContextQuery,
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::Context { peer, query, reply })?;
    read.recv_timeout(RPC).map_err(|_| unavailable())?
}

fn run(receiver: mpsc::Receiver<Message>, mut owner: owner::Owner) {
    let mut authorized = std::collections::BTreeMap::new();
    loop {
        // Only un-attached handshakes live here. Actual attempts are guarded
        // by the owner's durable journal, not a lifetime in-memory quota.
        authorized
            .retain(|_, entry: &mut (Peer, Instant)| entry.1.elapsed() < Duration::from_secs(20));
        let message = match receiver.recv_timeout(owner.wake_after()) {
            Ok(message) => Some(message),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some(message) = message {
            match message {
                Message::Wake => {}
                Message::Idle(reply) => {
                    let _ = reply.try_send(owner.idle());
                }
                Message::Capabilities(reply) => {
                    let _ = reply.try_send(owner.capabilities());
                }
                Message::Authorize {
                    binding,
                    peer,
                    reply,
                } => {
                    let accepted = owner.accepts(&binding)
                        && authorized.len() < MAX_REQUESTS
                        && !authorized.contains_key(&key(&binding));
                    if accepted {
                        authorized.insert(key(&binding), (peer, Instant::now()));
                    }
                    let _ = reply.try_send(accepted);
                }
                Message::Submit(submission) => {
                    let actual = authorized
                        .remove(&key(&submission.request.binding))
                        .map(|entry| entry.0);
                    if actual == Some(submission.peer) {
                        owner.submit(*submission);
                    } else {
                        refuse(*submission);
                    }
                }
                Message::AuthorizeMember { request, reply } => {
                    let _ = reply.try_send(owner.authorize_member(&request));
                }
                Message::SubmitMember(member) => owner.submit_member(*member),
                Message::Context { peer, query, reply } => {
                    let _ = reply.try_send(owner.context(peer, &query));
                }
                Message::Inspect {
                    peer,
                    session,
                    reply,
                } => {
                    let result = owner.inspect(peer, &session);
                    let _ = reply.try_send(result);
                }
            }
        }
        owner.tick();
    }
}
fn refuse(submission: AuthorizedSubmission) {
    let stream = aterm_uds::CtlStream::from(submission.fds.events);
    let event = EventFrame {
        binding: submission.request.binding,
        event: Event::Refused {
            reason: "host admission refused before launch".into(),
        },
    };
    let _ = aterm_ctl::resource_admission_transport::send_event(
        &stream,
        &event,
        Instant::now() + Duration::from_millis(100),
    );
}

pub(super) fn wake() {
    if let Ok(handle) = actor() {
        let _ = handle.sender.try_send(Message::Wake);
    }
}
