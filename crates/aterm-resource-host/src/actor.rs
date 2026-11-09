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
pub(super) use owner::bootstrap_graph::{
    EnrollmentAttachment as BootstrapGraphAttachment, OutputTransfer as BootstrapGraphOutput,
    Preparing as BootstrapGraphPreparing, Support as BootstrapGraphSupport,
};
pub(super) use owner::bootstrap_object::SourceOffer as BootstrapObjectOffer;
pub(super) use owner::runtime_inputs::Attachment as RuntimeInputsAttachment;
pub(super) use owner::runtime_inputs::Preparing as RuntimeInputsPreparing;
pub(super) use owner::workload::MemberOriginalCustody as WorkloadMemberOriginal;
pub(super) use owner::workload::ReadyCustody as WorkloadReady;
pub(super) use owner::workload::{WorkerCustody, WorkerOffer, WorkerReturn};

const RPC: Duration = Duration::from_secs(60);
fn receive_reply<T>(read: mpsc::Receiver<T>) -> Result<T, mpsc::RecvTimeoutError> {
    receive_reply_at(read, Instant::now())
}
fn receive_reply_at<T>(
    read: mpsc::Receiver<T>,
    started: Instant,
) -> Result<T, mpsc::RecvTimeoutError> {
    let remaining = (started + RPC)
        .checked_duration_since(Instant::now())
        .ok_or(mpsc::RecvTimeoutError::Timeout)?;
    read.recv_timeout(remaining)
}
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
    BootstrapGraphSupport {
        peer: Peer,
        query: Box<trust_resource_controller::bootstrap_graph::SupportQuery>,
        reply: SyncSender<io::Result<BootstrapGraphSupport>>,
    },
    BootstrapGraphContext {
        peer: Peer,
        key: Box<trust_resource_controller::bootstrap_graph::Key>,
        reply: SyncSender<io::Result<trust_resource_controller::ContextReply>>,
    },
    BootstrapGraphBegin {
        peer: Peer,
        request: Box<trust_resource_controller::bootstrap_graph::Enrollment>,
        reply: SyncSender<io::Result<BootstrapGraphPreparing>>,
    },
    BootstrapGraphRetain {
        token: Box<BootstrapGraphAttachment>,
        descriptions: [std::os::fd::OwnedFd; 2],
        control: std::os::fd::OwnedFd,
        reply: SyncSender<io::Result<trust_resource_controller::ContextReply>>,
    },
    BootstrapGraphOutputBegin {
        token: Box<BootstrapGraphOutput>,
        reply: SyncSender<io::Result<BootstrapGraphOutput>>,
    },
    BootstrapGraphOutputRetain {
        token: Box<BootstrapGraphOutput>,
        descriptions: [std::os::fd::OwnedFd; 2],
        reply: SyncSender<io::Result<trust_resource_controller::ContextReply>>,
    },
    SubmitBootstrapQuery {
        peer: Peer,
        request: Box<trust_resource_controller::bootstrap_graph::query::Request>,
        stream: aterm_uds::CtlStream,
        started: Instant,
        deadline: Instant,
        reply: SyncSender<io::Result<()>>,
    },
    PrepareBootstrapObject {
        peer: Peer,
        request: Box<trust_resource_controller::bootstrap_graph::object::Request>,
        stream: aterm_uds::CtlStream,
        started: Instant,
        deadline: Instant,
        reply: SyncSender<io::Result<BootstrapObjectOffer>>,
    },
    BootstrapObjectSource {
        peer: Peer,
        offer: Box<BootstrapObjectOffer>,
        source: io::Result<[std::os::fd::OwnedFd; 2]>,
        reply: SyncSender<io::Result<()>>,
    },
    RuntimeInputsBegin {
        peer: Peer,
        request: Box<trust_resource_controller::runtime_input_retention::Request>,
        reply: SyncSender<io::Result<RuntimeInputsPreparing>>,
    },
    RuntimeInputsRetain {
        preparing: Box<RuntimeInputsAttachment>,
        descriptions: Vec<std::os::fd::OwnedFd>,
        transport: std::os::fd::OwnedFd,
        reply: SyncSender<io::Result<trust_resource_controller::ContextReply>>,
    },
    WorkloadOutputLocks {
        peer: Peer,
        session: String,
        request: trust_resource_controller::workload::host::output::Request,
        descriptions: Vec<std::os::fd::OwnedFd>,
        reply: SyncSender<io::Result<trust_resource_controller::workload::host::output::Reply>>,
    },
    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    TestStop(SyncSender<bool>),
    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    TestControlEofBeforeReap {
        binding: Binding,
        reply: SyncSender<bool>,
    },
    AuthorizeWorkloadProduced {
        peer: Peer,
        request: Box<trust_resource_controller::workload::host::produced::ProducedRequest>,
        contained: Option<Box<trust_resource_controller::workload::host::contained::Request>>,
        reply: SyncSender<bool>,
    },
    SubmitWorkloadProduced {
        incoming: Box<crate::workload_transport::ProducedIncoming>,
        reply: SyncSender<io::Result<()>>,
    },
    WorkloadRegister {
        peer: Peer,
        session: String,
        request: trust_resource_controller::workload::host::produced::Registration,
        original: std::os::fd::OwnedFd,
        reply: SyncSender<io::Result<owner::workload::RegistrationPending>>,
    },
    WorkloadCensus {
        peer: Peer,
        session: String,
        request: trust_resource_controller::workload::host::census::Request,
        reply: SyncSender<io::Result<trust_resource_controller::workload::host::census::Reply>>,
    },
    WorkloadPhase {
        peer: Peer,
        session: String,
        request: trust_resource_controller::workload::host::phase::Request,
        descriptions: Vec<std::os::fd::OwnedFd>,
        reply: SyncSender<io::Result<trust_resource_controller::workload::host::phase::Reply>>,
    },
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
    SubmitWorkloadQuery(Box<crate::workload_transport::Incoming>),
    SubmitLinker(Box<crate::linker_transport::Incoming>),
    SubmitMember(Box<aterm_ctl::resource_subgroup_client::AuthorizedSubgroup>),
    SubmitProxy(Box<crate::resource_proxy_intake::Incoming>),
    ProvisionalProxy {
        request: Box<trust_resource_controller::Request>,
        helper: Peer,
        reply: SyncSender<bool>,
    },
    AuthorizeMember {
        request: Box<owner::members::Authorization>,
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
    WorkloadReady {
        peer: Peer,
        request: Box<trust_resource_controller::workload::host::ReadyRequest>,
        reply: SyncSender<io::Result<WorkloadReady>>,
    },
    WorkloadMemberOriginal {
        peer: Peer,
        request: trust_resource_controller::workload::host::MemberOriginalRequest,
        reply: SyncSender<io::Result<WorkloadMemberOriginal>>,
    },
    InstallWorkloadControl {
        peer: Peer,
        session: String,
        attempt: [u8; 16],
        link: crate::workload_transport::Link,
        reply: SyncSender<io::Result<()>>,
    },
    WorkloadWorkerOffer {
        peer: Peer,
        request: trust_resource_controller::workload::host::worker::Request,
        reply: SyncSender<io::Result<WorkerOffer>>,
    },
    WorkloadWorkerAccept {
        offer: WorkerOffer,
        reply: SyncSender<io::Result<WorkerCustody>>,
    },
    WorkloadWorkerReturn {
        peer: Peer,
        request: trust_resource_controller::workload::host::worker::Return,
        reply: SyncSender<io::Result<WorkerReturn>>,
    },
    WorkloadWorkerReturned {
        returned: WorkerReturn,
        reply: SyncSender<io::Result<trust_resource_controller::workload::host::worker::Returned>>,
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
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
fn activate_test(owner: owner::Owner) -> io::Result<std::thread::JoinHandle<()>> {
    let (sender, receiver) = mpsc::sync_channel(MAX_REQUESTS);
    HOST.set(Handle { sender }).map_err(|_| unavailable())?;
    std::thread::Builder::new()
        .name("native-owner-fixture".into())
        .spawn(move || run(receiver, owner))
}
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
fn stop_test() -> io::Result<bool> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::TestStop(reply))?;
    receive_reply(read).map_err(|_| unavailable())
}
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
pub(super) fn control_eof_before_reap_test(binding: Binding) -> io::Result<bool> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::TestControlEofBeforeReap { binding, reply })?;
    receive_reply(read).map_err(|_| unavailable())
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
        && let Ok(value) = receive_reply(read)
    {
        return value;
    }
    aterm_types::resource_inspection::unavailable("retained host unavailable")
}
pub(super) fn idle() -> bool {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::Idle(reply)).is_ok() && receive_reply(read).unwrap_or(false)
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
        && receive_reply(read).unwrap_or(false)
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
    send(Message::AuthorizeMember {
        request: Box::new(request),
        reply,
    })
    .is_ok()
        && receive_reply(read).unwrap_or(false)
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
    send(Message::AuthorizeMember {
        request: Box::new(request),
        reply,
    })
    .is_ok()
        && receive_reply(read).unwrap_or(false)
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
    send(Message::AuthorizeMember {
        request: Box::new(request),
        reply,
    })
    .is_ok()
        && receive_reply(read).unwrap_or(false)
}
pub(super) fn submit_resolver_query(member: crate::resolver_transport::Incoming) -> io::Result<()> {
    send(Message::SubmitResolverQuery(Box::new(member)))
}
pub(super) fn resolver_binding(value: &aterm_json::Value) -> io::Result<Binding> {
    owner::members::binding_role(value, "sealed-resolver-query-v1")
}
pub(super) fn workload_binding(value: &aterm_json::Value) -> io::Result<Binding> {
    owner::members::binding_role(
        value,
        trust_resource_controller::workload::host::QUERY_PURPOSE,
    )
}
pub(super) fn authorize_workload_query(
    request: &aterm_types::resource_subgroup::Request,
    peer: Peer,
    query: &trust_resource_controller::workload::host::CompilerQuery,
) -> bool {
    let Ok(mut request) = owner::members::Authorization::of(request, peer) else {
        return false;
    };
    request.workload_query = Some(query.clone());
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::AuthorizeMember {
        request: Box::new(request),
        reply,
    })
    .is_ok()
        && receive_reply(read).unwrap_or(false)
}
pub(super) fn submit_workload_query(member: crate::workload_transport::Incoming) -> io::Result<()> {
    send(Message::SubmitWorkloadQuery(Box::new(member)))
}
pub(crate) fn authorize_linker(
    request: &aterm_types::resource_subgroup::Request,
    peer: Peer,
    sealed: &trust_resource_controller::linker::Request,
) -> bool {
    let Ok(mut request) = owner::members::Authorization::of(request, peer) else {
        return false;
    };
    request.linker = Some(sealed.clone());
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::AuthorizeMember {
        request: Box::new(request),
        reply,
    })
    .is_ok()
        && receive_reply(read).unwrap_or(false)
}
pub(crate) fn submit_linker(incoming: crate::linker_transport::Incoming) -> io::Result<()> {
    send(Message::SubmitLinker(Box::new(incoming)))
}
pub(super) fn workload_ready(
    peer: Peer,
    request: trust_resource_controller::workload::host::ReadyRequest,
) -> io::Result<WorkloadReady> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadReady {
        peer,
        request: Box::new(request),
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn workload_member_original(
    peer: Peer,
    request: trust_resource_controller::workload::host::MemberOriginalRequest,
) -> io::Result<WorkloadMemberOriginal> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadMemberOriginal {
        peer,
        request,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn install_workload_control(
    peer: Peer,
    session: String,
    attempt: [u8; 16],
    link: crate::workload_transport::Link,
) -> io::Result<()> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::InstallWorkloadControl {
        peer,
        session,
        attempt,
        link,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn workload_worker_offer(
    peer: Peer,
    request: trust_resource_controller::workload::host::worker::Request,
) -> io::Result<WorkerOffer> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadWorkerOffer {
        peer,
        request,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn workload_worker_accept(offer: WorkerOffer) -> io::Result<WorkerCustody> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadWorkerAccept { offer, reply })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn workload_worker_return(
    peer: Peer,
    request: trust_resource_controller::workload::host::worker::Return,
) -> io::Result<WorkerReturn> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadWorkerReturn {
        peer,
        request,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn workload_worker_returned(
    returned: WorkerReturn,
) -> io::Result<trust_resource_controller::workload::host::worker::Returned> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadWorkerReturned { returned, reply })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn workload_phase(
    peer: Peer,
    session: String,
    request: trust_resource_controller::workload::host::phase::Request,
    descriptions: Vec<std::os::fd::OwnedFd>,
) -> io::Result<trust_resource_controller::workload::host::phase::Reply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadPhase {
        peer,
        session,
        request,
        descriptions,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn workload_census(
    peer: Peer,
    session: String,
    request: trust_resource_controller::workload::host::census::Request,
) -> io::Result<trust_resource_controller::workload::host::census::Reply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadCensus {
        peer,
        session,
        request,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn workload_output_locks(
    peer: Peer,
    session: String,
    request: trust_resource_controller::workload::host::output::Request,
    descriptions: Vec<std::os::fd::OwnedFd>,
) -> io::Result<trust_resource_controller::workload::host::output::Reply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadOutputLocks {
        peer,
        session,
        request,
        descriptions,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn bootstrap_graph_support(
    peer: Peer,
    query: trust_resource_controller::bootstrap_graph::SupportQuery,
) -> io::Result<BootstrapGraphSupport> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::BootstrapGraphSupport {
        peer,
        query: Box::new(query),
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn bootstrap_graph_context(
    peer: Peer,
    key: trust_resource_controller::bootstrap_graph::Key,
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::BootstrapGraphContext {
        peer,
        key: Box::new(key),
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn bootstrap_graph_begin(
    peer: Peer,
    request: trust_resource_controller::bootstrap_graph::Enrollment,
) -> io::Result<BootstrapGraphPreparing> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::BootstrapGraphBegin {
        peer,
        request: Box::new(request),
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn bootstrap_graph_retain(
    token: BootstrapGraphAttachment,
    descriptions: [std::os::fd::OwnedFd; 2],
    control: std::os::fd::OwnedFd,
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::BootstrapGraphRetain {
        token: Box::new(token),
        descriptions,
        control,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn bootstrap_graph_output_begin(
    token: BootstrapGraphOutput,
) -> io::Result<BootstrapGraphOutput> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::BootstrapGraphOutputBegin {
        token: Box::new(token),
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn bootstrap_graph_output_retain(
    token: BootstrapGraphOutput,
    descriptions: [std::os::fd::OwnedFd; 2],
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::BootstrapGraphOutputRetain {
        token: Box::new(token),
        descriptions,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn prepare_bootstrap_object(
    peer: Peer,
    request: trust_resource_controller::bootstrap_graph::object::Request,
    stream: aterm_uds::CtlStream,
    started: Instant,
    deadline: Instant,
) -> io::Result<BootstrapObjectOffer> {
    let (write, read) = mpsc::sync_channel(1);
    send(Message::PrepareBootstrapObject {
        peer,
        request: Box::new(request),
        stream,
        started,
        deadline,
        reply: write,
    })?;
    receive_reply_at(read, started).map_err(|_| unavailable())?
}
pub(super) fn bootstrap_object_source(
    peer: Peer,
    offer: BootstrapObjectOffer,
    source: io::Result<[std::os::fd::OwnedFd; 2]>,
) -> io::Result<()> {
    let (write, read) = mpsc::sync_channel(1);
    send(Message::BootstrapObjectSource {
        peer,
        offer: Box::new(offer),
        source,
        reply: write,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}

pub(super) fn submit_bootstrap_query(
    peer: Peer,
    request: trust_resource_controller::bootstrap_graph::query::Request,
    stream: aterm_uds::CtlStream,
    started: Instant,
    deadline: Instant,
) -> io::Result<()> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::SubmitBootstrapQuery {
        peer,
        request: Box::new(request),
        stream,
        started,
        deadline,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn runtime_inputs_begin(
    peer: Peer,
    request: trust_resource_controller::runtime_input_retention::Request,
) -> io::Result<RuntimeInputsPreparing> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::RuntimeInputsBegin {
        peer,
        request: Box::new(request),
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn runtime_inputs_retain(
    preparing: RuntimeInputsAttachment,
    descriptions: Vec<std::os::fd::OwnedFd>,
    transport: std::os::fd::OwnedFd,
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::RuntimeInputsRetain {
        preparing: Box::new(preparing),
        descriptions,
        transport,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn metadata_binding(value: &aterm_json::Value) -> io::Result<Binding> {
    owner::members::binding_role(value, "sealed-metadata-v1")
}
pub(super) fn workload_register(
    peer: Peer,
    session: String,
    request: trust_resource_controller::workload::host::produced::Registration,
    original: std::os::fd::OwnedFd,
) -> io::Result<owner::workload::RegistrationPending> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::WorkloadRegister {
        peer,
        session,
        request,
        original,
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn authorize_workload_produced(
    peer: Peer,
    request: trust_resource_controller::workload::host::produced::ProducedRequest,
) -> bool {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::AuthorizeWorkloadProduced {
        peer,
        request: Box::new(request),
        contained: None,
        reply,
    })
    .is_ok()
        && receive_reply(read).unwrap_or(false)
}
pub(super) fn authorize_contained_produced(
    peer: Peer,
    request: trust_resource_controller::workload::host::contained::Request,
) -> bool {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::AuthorizeWorkloadProduced {
        peer,
        request: Box::new(request.produced.clone()),
        contained: Some(Box::new(request)),
        reply,
    })
    .is_ok()
        && receive_reply(read).unwrap_or(false)
}
pub(super) fn submit_workload_produced(
    incoming: crate::workload_transport::ProducedIncoming,
) -> io::Result<()> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::SubmitWorkloadProduced {
        incoming: Box::new(incoming),
        reply,
    })?;
    receive_reply(read).map_err(|_| unavailable())?
}
pub(super) fn member_binding(value: &aterm_json::Value) -> io::Result<Binding> {
    owner::members::binding(value)
}
pub(super) fn member_binding_role(value: &aterm_json::Value, role: &str) -> io::Result<Binding> {
    owner::members::binding_role(value, role)
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
        && receive_reply(read).unwrap_or(false)
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
    receive_reply(read).map_err(|_| unavailable())?
}

pub(super) fn context(
    peer: Peer,
    query: trust_resource_controller::ContextQuery,
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::Context { peer, query, reply })?;
    receive_reply(read).map_err(|_| unavailable())?
}

pub(super) fn metadata_ready(
    peer: Peer,
    query: trust_resource_controller::ContextQuery,
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::MetadataReady { peer, query, reply })?;
    receive_reply(read).map_err(|_| unavailable())?
}

pub(super) fn resolver_ready(
    peer: Peer,
    query: trust_resource_controller::ContextQuery,
) -> io::Result<trust_resource_controller::ContextReply> {
    let (reply, read) = mpsc::sync_channel(1);
    send(Message::ResolverReady { peer, query, reply })?;
    receive_reply(read).map_err(|_| unavailable())?
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
                Message::BootstrapGraphSupport { peer, query, reply } => {
                    let _ = reply.try_send(owner.bootstrap_graph_support(peer, *query));
                }
                Message::BootstrapGraphContext { peer, key, reply } => {
                    let _ = reply.try_send(owner.bootstrap_graph_context(peer, &key));
                }
                Message::BootstrapGraphBegin {
                    peer,
                    request,
                    reply,
                } => {
                    let _ = reply.try_send(owner.bootstrap_graph_begin(peer, *request));
                }
                Message::BootstrapGraphRetain {
                    token,
                    descriptions,
                    control,
                    reply,
                } => {
                    let _ =
                        reply.try_send(owner.bootstrap_graph_retain(*token, descriptions, control));
                }
                Message::BootstrapGraphOutputBegin { token, reply } => {
                    let _ = reply.try_send(owner.bootstrap_graph_output_begin(*token));
                }
                Message::BootstrapGraphOutputRetain {
                    token,
                    descriptions,
                    reply,
                } => {
                    let _ =
                        reply.try_send(owner.bootstrap_graph_output_retain(*token, descriptions));
                }
                Message::PrepareBootstrapObject {
                    peer,
                    request,
                    stream,
                    started,
                    deadline,
                    reply,
                } => {
                    let _ = reply.send(
                        owner.prepare_bootstrap_object(peer, *request, stream, started, deadline),
                    );
                }
                Message::BootstrapObjectSource {
                    peer,
                    offer,
                    source,
                    reply,
                } => {
                    let _ = reply.send(owner.bootstrap_object_source(peer, *offer, source));
                }
                Message::SubmitBootstrapQuery {
                    peer,
                    request,
                    stream,
                    started,
                    deadline,
                    reply,
                } => {
                    let _ = reply.try_send(
                        owner.submit_bootstrap_query(peer, *request, stream, started, deadline),
                    );
                }
                Message::RuntimeInputsBegin {
                    peer,
                    request,
                    reply,
                } => {
                    let _ = reply.try_send(owner.runtime_inputs_begin(peer, *request));
                }
                Message::RuntimeInputsRetain {
                    preparing,
                    descriptions,
                    transport,
                    reply,
                } => {
                    let _ = reply.try_send(owner.runtime_inputs_retain(
                        *preparing,
                        descriptions,
                        transport,
                    ));
                }
                Message::WorkloadOutputLocks {
                    peer,
                    session,
                    request,
                    descriptions,
                    reply,
                } => {
                    let _ = reply.try_send(owner.workload_output_locks(
                        peer,
                        &session,
                        request,
                        descriptions,
                    ));
                }
                #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
                Message::TestControlEofBeforeReap { binding, reply } => {
                    let _ = reply.try_send(owner.control_eof_before_reap_test(&binding));
                }
                #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
                Message::TestStop(reply) => {
                    let idle = owner.idle();
                    if !idle {
                        // Fixture unwind requests cancellation through the
                        // SAME original owners. It never guesses a target PID
                        // or drops paid custody to make the test finish.
                        owner.cancel_test();
                    }
                    let _ = reply.try_send(idle);
                    if idle {
                        break;
                    }
                }
                Message::AuthorizeWorkloadProduced {
                    peer,
                    request,
                    contained,
                    reply,
                } => {
                    let allowed = match contained {
                        Some(contained) if contained.produced == *request => {
                            owner.authorize_contained_produced(peer, &contained)
                        }
                        Some(_) => false,
                        None => owner.authorize_workload_produced(peer, &request),
                    };
                    let _ = reply.try_send(allowed);
                }
                Message::SubmitWorkloadProduced { incoming, reply } => {
                    let _ = reply.try_send(owner.submit_workload_produced(*incoming));
                }
                Message::WorkloadRegister {
                    peer,
                    session,
                    request,
                    original,
                    reply,
                } => {
                    let _ =
                        reply.try_send(owner.workload_register(peer, &session, request, original));
                }
                Message::WorkloadCensus {
                    peer,
                    session,
                    request,
                    reply,
                } => {
                    let _ = reply.try_send(owner.workload_census(peer, &session, &request));
                }
                Message::WorkloadPhase {
                    peer,
                    session,
                    request,
                    descriptions,
                    reply,
                } => {
                    let _ = reply.try_send(owner.workload_phase(
                        peer,
                        &session,
                        &request,
                        descriptions,
                    ));
                }
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
                Message::SubmitWorkloadQuery(member) => owner.submit_workload_query(*member),
                Message::SubmitLinker(member) => owner.submit_linker(*member),

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
                Message::WorkloadReady {
                    peer,
                    request,
                    reply,
                } => {
                    let _ = reply.try_send(owner.workload_ready(peer, &request));
                }
                Message::WorkloadMemberOriginal {
                    peer,
                    request,
                    reply,
                } => {
                    let _ = reply.try_send(owner.workload_member_original(peer, &request));
                }
                Message::InstallWorkloadControl {
                    peer,
                    session,
                    attempt,
                    link,
                    reply,
                } => {
                    let _ = reply
                        .try_send(owner.install_workload_control(peer, &session, attempt, link));
                }
                Message::WorkloadWorkerOffer {
                    peer,
                    request,
                    reply,
                } => {
                    let _ = reply.try_send(owner.workload_worker_offer(peer, request));
                }
                Message::WorkloadWorkerAccept { offer, reply } => {
                    let _ = reply.try_send(owner.workload_worker_accept(offer));
                }
                Message::WorkloadWorkerReturn {
                    peer,
                    request,
                    reply,
                } => {
                    let _ = reply.try_send(owner.workload_worker_return(peer, request));
                }
                Message::WorkloadWorkerReturned { returned, reply } => {
                    let _ = reply.try_send(owner.workload_worker_returned(returned));
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
    fn a_valid_owner_reply_survives_scheduling_delay() {
        for (age, expected) in [(3, Ok(true)), (61, Err(mpsc::RecvTimeoutError::Timeout))] {
            let (send, read) = mpsc::sync_channel(1);
            send.send(true).unwrap();
            assert_eq!(
                receive_reply_at(read, Instant::now() - Duration::from_secs(age)),
                expected
            );
        }
    }
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
