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
    Submit {
        submission: Box<AuthorizedSubmission>,
        accounted: bool,
    },
    SubmitMetadata(Box<crate::metadata_transport::Incoming>),
    SubmitResolverQuery(Box<crate::resolver_transport::Incoming>),
    SubmitMember(Box<aterm_ctl::resource_subgroup_client::AuthorizedSubgroup>),
    SubmitProxy(Box<crate::resource_proxy_intake::Incoming>),
    ProvisionalProxy {
        request: Box<trust_resource_controller::Request>,
        helper: Peer,
        reply: SyncSender<bool>,
    },
    AuthorizeMember {
        request: owner::members::Authorization,
        reply: SyncSender<bool>,
    },
    Context {
        peer: Peer,
        query: trust_resource_controller::ContextQuery,
        reply: SyncSender<io::Result<trust_resource_controller::ContextReply>>,
    },
    MetadataReady {
        peer: Peer,
        query: trust_resource_controller::ContextQuery,
        reply: SyncSender<io::Result<trust_resource_controller::ContextReply>>,
    },
    ResolverReady {
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
    submit_mode(submission, false)
}
pub(crate) fn submit_accounted(submission: AuthorizedSubmission) -> io::Result<()> {
    if !crate::resource_accounting::supported() {
        return Err(unavailable());
    }
    submit_mode(submission, true)
}
fn submit_mode(submission: AuthorizedSubmission, accounted: bool) -> io::Result<()> {
    send(Message::Submit {
        submission: Box::new(submission),
        accounted,
    })
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
pub(super) fn authorize_metadata(
    request: &aterm_types::resource_subgroup::Request,
    peer: Peer,
) -> bool {
    let Ok(mut request) = owner::members::Authorization::of(request, peer) else {
        return false;
    };
    request.metadata = true;
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::AuthorizeMember { request, reply }).is_ok()
        && read.recv_timeout(RPC).unwrap_or(false)
}
pub(super) fn submit_metadata(member: crate::metadata_transport::Incoming) -> io::Result<()> {
    send(Message::SubmitMetadata(Box::new(member)))
}
pub(super) fn authorize_resolver_query(
    request: &aterm_types::resource_subgroup::Request,
    peer: Peer,
) -> bool {
    let Ok(mut request) = owner::members::Authorization::of(request, peer) else {
        return false;
    };
    request.resolver = true;
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::AuthorizeMember { request, reply }).is_ok()
        && read.recv_timeout(RPC).unwrap_or(false)
}
pub(super) fn submit_resolver_query(member: crate::resolver_transport::Incoming) -> io::Result<()> {
    send(Message::SubmitResolverQuery(Box::new(member)))
}
pub(super) fn resolver_binding(value: &aterm_json::Value) -> io::Result<Binding> {
    owner::members::binding_role(value, "sealed-resolver-query-v1")
}
pub(super) fn metadata_binding(value: &aterm_json::Value) -> io::Result<Binding> {
    owner::members::binding_role(value, "sealed-metadata-v1")
}
pub(super) fn member_binding(value: &aterm_json::Value) -> io::Result<Binding> {
    owner::members::binding(value)
}
pub(super) fn submit_member(
    member: aterm_ctl::resource_subgroup_client::AuthorizedSubgroup,
) -> io::Result<()> {
    send(Message::SubmitMember(Box::new(member)))
}
pub(super) fn provisional_proxy(
    request: &trust_resource_controller::Request,
    helper: Peer,
) -> bool {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::ProvisionalProxy {
        request: Box::new(request.clone()),
        helper,
        reply,
    })
    .is_ok()
        && read.recv_timeout(RPC).unwrap_or(false)
}
pub(super) fn proxy_binding(value: &aterm_json::Value) -> io::Result<Binding> {
    owner::members::binding_role(value, "proxy-member")
}
pub(super) fn proxy_binding_accounted(value: &aterm_json::Value) -> io::Result<Binding> {
    if !crate::resource_accounting::supported()
        || value.as_object().is_none_or(|object| object.len() != 5)
        || value["accounting"].as_str() != Some(trust_resource_controller::accounting::CAPABILITY)
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    // The five-field negotiation was checked above. The unchanged original
    // four-field binding parser still validates every routing field exactly.
    let mut routing = value.clone();
    routing
        .as_object_mut()
        .ok_or_else(unavailable)?
        .remove("accounting");
    owner::members::binding_role(&routing, trust_resource_controller::accounting::PROXY_ROLE)
}
pub(super) fn submit_proxy(incoming: crate::resource_proxy_intake::Incoming) -> io::Result<()> {
    send(Message::SubmitProxy(Box::new(incoming)))
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

pub(super) fn metadata_ready(
    peer: Peer,
    query: trust_resource_controller::ContextQuery,
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::MetadataReady { peer, query, reply })?;
    read.recv_timeout(RPC).map_err(|_| unavailable())?
}

pub(super) fn resolver_ready(
    peer: Peer,
    query: trust_resource_controller::ContextQuery,
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::ResolverReady { peer, query, reply })?;
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
                Message::Submit {
                    submission,
                    accounted,
                } => {
                    let actual = authorized
                        .remove(&key(&submission.request.binding))
                        .map(|entry| entry.0);
                    if actual == Some(submission.peer) {
                        if accounted {
                            owner.submit_mode(*submission, true);
                        } else {
                            owner.submit(*submission);
                        }
                    } else {
                        refuse_mode(*submission, accounted);
                    }
                }
                Message::AuthorizeMember { request, reply } => {
                    let _ = reply.try_send(owner.authorize_member(&request));
                }
                Message::SubmitMember(member) => owner.submit_member(*member),
                Message::SubmitMetadata(member) => owner.submit_metadata(*member),
                Message::SubmitResolverQuery(member) => owner.submit_resolver_query(*member),

                Message::SubmitProxy(incoming) => owner.submit_proxy(*incoming),
                Message::ProvisionalProxy {
                    request,
                    helper,
                    reply,
                } => {
                    let _ = reply.try_send(owner.provisional_proxy(&request, helper));
                }
                Message::Context { peer, query, reply } => {
                    let _ = reply.try_send(owner.context(peer, &query));
                }
                Message::MetadataReady { peer, query, reply } => {
                    let _ = reply.try_send(owner.metadata_ready(peer, &query));
                }
                Message::ResolverReady { peer, query, reply } => {
                    let _ = reply.try_send(owner.resolver_ready(peer, &query));
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
fn refuse_mode(submission: AuthorizedSubmission, accounted: bool) {
    let stream = aterm_uds::CtlStream::from(submission.fds.events);
    let event = EventFrame {
        binding: submission.request.binding,
        event: Event::Refused {
            reason: "host admission refused before launch".into(),
        },
    };
    let bytes = if accounted {
        aterm_types::resource_admission::AccountedEventFrame {
            finished: event,
            observation: aterm_types::resource_admission::AccountingObservation::uncollected(),
        }
        .encode()
    } else {
        event.encode()
    };
    if let Ok(bytes) = bytes {
        let _ = aterm_ctl::resource_admission_transport::write_frame(
            &stream,
            &bytes,
            Instant::now() + Duration::from_millis(100),
        );
    }
}

pub(super) fn wake() {
    if let Ok(handle) = actor() {
        let _ = handle.sender.try_send(Message::Wake);
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn metadata_native_role() {
    owner::metadata_native_role();
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn member_fixture(
    args: &[std::ffi::OsString],
    gnu: Option<[std::os::fd::OwnedFd; 2]>,
) -> std::process::ExitCode {
    owner::member_fixture(args, gnu)
}

#[cfg(all(
    test,
    target_pointer_width = "64",
    any(target_os = "macos", target_os = "linux")
))]
mod accounting_routing_tests {
    use super::*;
    #[test]
    fn accounted_routing_is_exact_and_does_not_relax_legacy_handshake() {
        let hello = aterm_json::json!({"version":3,"role":trust_resource_controller::accounting::PROXY_ROLE,"session":"s-a","attempt":"12121212121212121212121212121212","accounting":trust_resource_controller::accounting::CAPABILITY});
        assert!(proxy_binding_accounted(&hello).is_ok());
        assert!(proxy_binding(&hello).is_err());
        for key in ["version", "role", "session", "attempt", "accounting"] {
            let mut missing = hello.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(proxy_binding_accounted(&missing).is_err());
        }
        let mut wrong = hello.clone();
        wrong
            .as_object_mut()
            .unwrap()
            .insert("accounting".into(), "unknown".into());
        assert!(proxy_binding_accounted(&wrong).is_err());
        let mut extra = hello;
        extra
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), 1.into());
        assert!(proxy_binding_accounted(&extra).is_err());
    }
}
