// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Separately negotiated workload queries. Representation translation creates
//! no scheduling unit; actual original-root authority precedes SCM attachment.

use aterm_ctl::resource_admission_transport as transport;
use aterm_ctl::resource_subgroup_client::AuthorizedSubgroup;
use aterm_types::{
    resource_admission::{Binding, Submission},
    resource_subgroup as local,
};
use aterm_uds::CtlStream;
use std::os::fd::AsFd as _;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::time::Duration;
use std::{io, time::Instant};
use trust_resource_controller::workload::host::{
    CompilerQuery, READY_CAPABILITIES_JSON, ReadyRequest,
};

fn incoming_deadline(root_deadline: Instant, started: Instant) -> Instant {
    root_deadline.min(crate::runtime::control_deadline(started))
}

pub(crate) struct BankAnswer {
    pub(crate) reply: trust_resource_controller::workload::host::bank::Reply,
    pub(crate) original: Option<std::os::fd::OwnedFd>,
}
struct BankMessage {
    request: trust_resource_controller::workload::host::bank::Request,
    deadline: Instant,
    reply: SyncSender<io::Result<BankAnswer>>,
}
pub(crate) struct Link {
    send: SyncSender<BankMessage>,
    closed: Arc<AtomicBool>,
    drained: Arc<AtomicBool>,
}
pub(crate) struct BankPending {
    receive: Receiver<io::Result<BankAnswer>>,
}
impl BankPending {
    pub(crate) fn poll(&self) -> io::Result<Option<BankAnswer>> {
        match self.receive.try_recv() {
            Ok(value) => value.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
}
struct Driver {
    receive: Receiver<BankMessage>,
    closed: Arc<AtomicBool>,
    drained: Arc<AtomicBool>,
}
impl Link {
    fn pair() -> (Self, Driver) {
        let (send, receive) = mpsc::sync_channel(128);
        let closed = Arc::new(AtomicBool::new(false));
        let drained = Arc::new(AtomicBool::new(false));
        (
            Self {
                send,
                closed: Arc::clone(&closed),
                drained: Arc::clone(&drained),
            },
            Driver {
                receive,
                closed,
                drained,
            },
        )
    }
    pub(crate) fn try_begin(
        &self,
        request: trust_resource_controller::workload::host::bank::Request,
        deadline: Instant,
    ) -> io::Result<Option<BankPending>> {
        if self.closed() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let (reply, receive) = mpsc::sync_channel(1);
        match self.send.try_send(BankMessage {
            request,
            deadline,
            reply,
        }) {
            Ok(()) => Ok(Some(BankPending { receive })),
            Err(mpsc::TrySendError::Full(_)) => Ok(None),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
    pub(crate) fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    pub(crate) fn drained(&self) -> bool {
        self.drained.load(Ordering::Acquire)
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        crate::actor::wake();
    }
}
impl Driver {
    fn serve(
        self,
        stream: &CtlStream,
        deadline: Instant,
        peer: transport::Peer,
        session: &str,
    ) -> io::Result<()> {
        use trust_resource_controller::workload::host::bank;
        stream.set_nonblocking(true)?;
        loop {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.receive.recv_timeout(Duration::from_millis(10)) {
                Ok(message) => {
                    let limit = deadline.min(message.deadline);
                    let result = (|| {
                        transport::write_frame(stream, &message.request.encode()?, limit)?;
                        let reply = bank::Reply::decode(
                            &transport::read_frame(stream, limit)?,
                            &message.request,
                        )?;
                        let original = if let bank::Decision::Granted { backing, .. } =
                            &reply.decision
                        {
                            let mut byte = [0];
                            let received = loop {
                                if Instant::now() >= limit {
                                    return Err(io::ErrorKind::TimedOut.into());
                                }
                                match aterm_uds::fdpass::recv_with_fds(stream, &mut byte, 1) {
                                    Ok(received) => break received,
                                    Err(error)
                                        if matches!(
                                            error.kind(),
                                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                                        ) =>
                                    {
                                        std::thread::sleep(Duration::from_millis(1))
                                    }
                                    Err(error) => return Err(error),
                                }
                            };
                            if received.bytes != 1
                                || byte != [bank::ORIGINAL_MARKER]
                                || received.fds.len() != 1
                            {
                                return Err(io::ErrorKind::InvalidData.into());
                            }
                            let mut fds = received.fds;
                            let original = fds.remove(0);
                            use std::os::{fd::AsRawFd as _, unix::fs::MetadataExt as _};
                            let meta =
                                aterm_uds::spawnfd::descriptor_metadata(original.as_raw_fd())?;
                            if meta.dev() != backing.device || meta.ino() != backing.inode {
                                return Err(io::ErrorKind::PermissionDenied.into());
                            }
                            Some(original)
                        } else {
                            None
                        };
                        Ok(BankAnswer { reply, original })
                    })();
                    let failed = result.is_err();
                    let _ = message.reply.try_send(result);
                    crate::actor::wake();
                    if failed {
                        return Err(io::ErrorKind::BrokenPipe.into());
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let mut byte = [0];
                    use std::os::fd::AsRawFd as _;
                    // SAFETY: private held socket and one writable byte; peek
                    // consumes no frame or original resource description.
                    let n = unsafe {
                        libc::recv(
                            stream.as_raw_fd(),
                            byte.as_mut_ptr().cast(),
                            1,
                            libc::MSG_PEEK | libc::MSG_DONTWAIT,
                        )
                    };
                    if n == 0 {
                        return if self.drained.load(Ordering::Acquire) {
                            Ok(())
                        } else {
                            Err(io::ErrorKind::UnexpectedEof.into())
                        };
                    }
                    if n > 0 {
                        use trust_resource_controller::workload::host::phase;
                        let limit = incoming_deadline(deadline, Instant::now());
                        let frame = transport::read_frame(stream, limit)?;
                        let request = match phase::Request::decode(&frame) {
                            Ok(request) => request,
                            Err(_) => {
                                if let Ok(request)=trust_resource_controller::workload::host::output::Request::decode(&frame) {
                                    let descriptions=receive_output_locks(stream,request.locks.len(),limit)?;
                                    if transport::local_peer(stream)?!=peer {return Err(io::ErrorKind::PermissionDenied.into())}
                                    let reply=crate::actor::workload_output_locks(peer,session.to_owned(),request,descriptions)?;
                                    if transport::local_peer(stream)?!=peer {return Err(io::ErrorKind::PermissionDenied.into())}
                                    transport::write_frame(stream,&reply.encode()?,limit)?;
                                    continue;
                                }
                                if let Ok(request)=trust_resource_controller::workload::host::produced::Registration::decode(&frame) {
                                    register_output(stream,peer,session,request,limit,deadline)?;
                                    continue;
                                }
                                let request=trust_resource_controller::workload::host::census::Request::decode(&frame)?;
                                if transport::local_peer(stream)? != peer {
                                    return Err(io::ErrorKind::PermissionDenied.into());
                                }
                                let reply = crate::actor::workload_census(
                                    peer,
                                    session.to_owned(),
                                    request,
                                )?;
                                if transport::local_peer(stream)? != peer {
                                    return Err(io::ErrorKind::PermissionDenied.into());
                                }
                                transport::write_frame(stream, &reply.encode()?, limit)?;
                                continue;
                            }
                        };
                        if transport::local_peer(stream)? != peer {
                            return Err(io::ErrorKind::PermissionDenied.into());
                        }
                        let mut descriptions = Vec::new();
                        if request.description_count() != 0 {
                            let mut marker = [0];
                            let received = loop {
                                if Instant::now() >= limit {
                                    return Err(io::ErrorKind::TimedOut.into());
                                }
                                match aterm_uds::fdpass::recv_with_fds(
                                    stream,
                                    &mut marker,
                                    request.description_count(),
                                ) {
                                    Ok(received) => break received,
                                    Err(error)
                                        if matches!(
                                            error.kind(),
                                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                                        ) =>
                                    {
                                        std::thread::sleep(Duration::from_millis(1))
                                    }
                                    Err(error) => return Err(error),
                                }
                            };
                            if received.bytes != 1
                                || marker != [phase::GNU_MARKER]
                                || received.fds.len() != request.description_count()
                            {
                                return Err(io::ErrorKind::InvalidData.into());
                            }
                            descriptions = received.fds;
                        }
                        let reply = crate::actor::workload_phase(
                            peer,
                            session.to_owned(),
                            request,
                            descriptions,
                        )?;
                        if transport::local_peer(stream)? != peer {
                            return Err(io::ErrorKind::PermissionDenied.into());
                        }
                        transport::write_frame(stream, &reply.encode()?, limit)?;
                        if matches!(reply.decision, phase::Decision::Drained) {
                            self.drained.store(true, Ordering::Release);
                        }
                        continue;
                    } // Only the explicit phase and census grammars originate here.
                    let error = io::Error::last_os_error();
                    if !matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) {
                        return Err(error);
                    }
                }
            }
        }
    }
}

pub(super) struct Incoming {
    pub subgroup: AuthorizedSubgroup,
    pub sealed: CompilerQuery,
}
pub(crate) struct ProducedIncoming {
    pub(crate) request: trust_resource_controller::workload::host::produced::ProducedRequest,
    pub(crate) contained: Option<trust_resource_controller::workload::host::contained::Request>,
    pub(crate) transport: Option<std::os::fd::OwnedFd>,
    pub(crate) peer: transport::Peer,
    pub(crate) stdin: std::os::fd::OwnedFd,
    pub(crate) stdout: std::os::fd::OwnedFd,
    pub(crate) stderr: std::os::fd::OwnedFd,
    pub(crate) events: std::os::fd::OwnedFd,
}
struct ProducedRights([std::os::fd::OwnedFd; 4]);
impl aterm_uds::spawn_lock::Descriptors for ProducedRights {
    fn mark(&self) -> io::Result<()> {
        for fd in &self.0 {
            aterm_uds::keep_out_of_children(fd)?;
        }
        Ok(())
    }
}
struct OutputRights(Vec<std::os::fd::OwnedFd>);
impl aterm_uds::spawn_lock::Descriptors for OutputRights {
    fn mark(&self) -> io::Result<()> {
        for fd in &self.0 {
            aterm_uds::keep_out_of_children(fd)?;
        }
        Ok(())
    }
}
fn receive_output_locks(
    stream: &CtlStream,
    count: usize,
    deadline: Instant,
) -> io::Result<Vec<std::os::fd::OwnedFd>> {
    use trust_resource_controller::workload::host::output;
    if count == 0 || count > output::MAX_LOCKS {
        return Err(io::ErrorKind::InvalidData.into());
    }
    loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let result = aterm_uds::kept_out(|| {
            let mut marker = [0; 2];
            let got = aterm_uds::fdpass::recv_with_fds(stream, &mut marker, count)?;
            if got.bytes != 1 || marker[0] != output::MARKER || got.fds.len() != count {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            Ok(OutputRights(got.fds))
        });
        match result {
            Ok(rights) => return Ok(rights.0),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                aterm_uds::ready::until(stream.as_fd(), aterm_uds::ready::Interest::Read, deadline)?
            }
            Err(error) => return Err(error),
        }
    }
}
pub(super) fn receive_produced(
    stream: &CtlStream,
    job: &Binding,
    deadline: Instant,
) -> io::Result<ProducedIncoming> {
    receive_produced_inner(stream, job, deadline, false)
}
pub(super) fn receive_contained_produced(
    stream: &CtlStream,
    job: &Binding,
    deadline: Instant,
) -> io::Result<ProducedIncoming> {
    receive_produced_inner(stream, job, deadline, true)
}
fn receive_produced_inner(
    stream: &CtlStream,
    job: &Binding,
    deadline: Instant,
    contained_mode: bool,
) -> io::Result<ProducedIncoming> {
    use std::os::fd::AsRawFd as _;
    use trust_resource_controller::workload::host::produced::ProducedRequest;
    stream.set_nonblocking(true)?;
    let peer = transport::local_peer(stream)?;
    let bytes = transport::read_frame(stream, deadline)?;
    let contained = if contained_mode {
        Some(trust_resource_controller::workload::host::contained::Request::decode(&bytes)?)
    } else {
        None
    };
    let request = match &contained {
        Some(request) => request.produced.clone(),
        None => ProducedRequest::decode(&bytes)?,
    };
    let authorized = match &contained {
        Some(request) => crate::actor::authorize_contained_produced(peer, request.clone()),
        None => crate::actor::authorize_workload_produced(peer, request.clone()),
    };
    if request.member.binding.session != job.session
        || request.key.root_attempt != job.attempt
        || !authorized
        || transport::local_peer(stream)? != peer
    {
        transport::write_frame(stream, &[0], deadline)?;
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    transport::write_frame(stream, &[0x41], deadline)?;
    let received = loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let result = aterm_uds::kept_out(|| {
            let mut marker = [0; 2];
            let received = aterm_uds::fdpass::recv_with_fds(stream, &mut marker, 4)?;
            if received.bytes != 1 || marker[0] != 0x46 || received.fds.len() != 4 {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            let mut fds = received.fds.into_iter();
            let mut next = || {
                fds.next()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
            };
            Ok(ProducedRights([next()?, next()?, next()?, next()?]))
        });
        match result {
            Ok(received) => break received,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                aterm_uds::ready::until(stream.as_fd(), aterm_uds::ready::Interest::Read, deadline)?
            }
            Err(error) => return Err(error),
        }
    };
    if transport::local_peer(stream)? != peer {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let ProducedRights([stdin, stdout, stderr, events]) = received;
    let events = CtlStream::from(events);
    for (fd, read) in [(&stdin, true), (&stdout, false), (&stderr, false)] {
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || (read && flags & libc::O_ACCMODE == libc::O_WRONLY)
            || (!read && flags & libc::O_ACCMODE == libc::O_RDONLY)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    if transport::local_peer(&events)? != peer {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    events.set_nonblocking(true)?;
    Ok(ProducedIncoming {
        request,
        contained,
        transport: if contained_mode {
            Some(stream.try_clone()?.into())
        } else {
            None
        },
        peer,
        stdin,
        stdout,
        stderr,
        events: events.into(),
    })
}

fn register_output(
    stream: &CtlStream,
    peer: transport::Peer,
    session: &str,
    request: trust_resource_controller::workload::host::produced::Registration,
    frame_deadline: Instant,
    root_deadline: Instant,
) -> io::Result<()> {
    use trust_resource_controller::workload::host::produced;
    if transport::local_peer(stream)? != peer {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut marker = [0];
    let received = loop {
        if Instant::now() >= frame_deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match aterm_uds::fdpass::recv_with_fds(stream, &mut marker, 1) {
            Ok(received) => break received,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(error) => return Err(error),
        }
    };
    if received.bytes != 1 || marker != [produced::REGISTRATION_MARKER] || received.fds.len() != 1 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let original = received
        .fds
        .into_iter()
        .next()
        .ok_or(io::ErrorKind::InvalidData)?;
    if transport::local_peer(stream)? != peer {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let pending = crate::actor::workload_register(peer, session.to_owned(), request, original)?;
    loop {
        if Instant::now() >= root_deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if transport::local_peer(stream)? != peer {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if let Some(reply) = pending.poll()? {
            return transport::write_frame(stream, &reply.encode()?, root_deadline);
        }
        use std::os::fd::AsRawFd as _;
        let mut byte = [0];
        // Nothing may follow a one-use registration before its response. EOF
        // or an extra frame cancels this original route, not the paid worker.
        // Its actor still owns and joins that worker before root retirement.
        let n = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                byte.as_mut_ptr().cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if n > 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let error = io::Error::last_os_error();
        if !matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ) {
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn local_request(sealed: &CompilerQuery, peer: transport::Peer) -> io::Result<local::Request> {
    sealed.validate()?;
    let request = &sealed.member;
    let cpu = request
        .delegations
        .cpu
        .ok_or(io::ErrorKind::PermissionDenied)?;
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
            memory_bytes: 1,
            label: "sealed workload compiler query".into(),
            timeout_ms: request.timeout_ns.map(|ns| ns.div_ceil(1_000_000)),
        },
        cpu: local::CpuDelegation::SharedBorrow {
            target_fd: cpu.target,
        },
        gnu: local::GnuTransport::None,
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
    stream.set_nonblocking(true)?;
    let peer = transport::local_peer(stream)?;
    let sealed = CompilerQuery::decode(&transport::read_frame(stream, deadline)?)?;
    let request = local_request(&sealed, peer)?;
    let subgroup = crate::metadata_transport::receive_rights(
        stream,
        job,
        deadline,
        request,
        peer,
        true,
        |request, peer| crate::actor::authorize_workload_query(request, peer, &sealed),
    )?;
    Ok(Incoming { subgroup, sealed })
}

pub(super) fn ready(
    stream: &CtlStream,
    peer: transport::Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
    observe: impl FnOnce(transport::Peer, ReadyRequest) -> io::Result<crate::actor::WorkloadReady>,
) -> io::Result<()> {
    let capabilities: aterm_json::Value =
        aterm_json::from_str(READY_CAPABILITIES_JSON).map_err(|_| io::ErrorKind::InvalidData)?;
    if hello["version"].as_u64() != Some(3)
        || hello["role"].as_str() != Some(trust_resource_controller::workload::WORKLOAD_READY_ROLE)
        || hello["capabilities"] != capabilities
        || hello.as_object().is_none_or(|fields| {
            fields.len() != 4
                || ["version", "role", "session", "capabilities"]
                    .iter()
                    .any(|key| !fields.contains_key(*key))
        })
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let request = ReadyRequest::decode(&transport::read_frame(stream, deadline)?)?;
    if hello["session"].as_str() != Some(request.query.binding.session.as_str())
        || transport::local_peer(stream)? != peer
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let reply = observe(peer, request)?;
    let (link, driver) = Link::pair();
    crate::actor::install_workload_control(
        peer,
        reply.reply.context.context.session.clone(),
        reply.reply.context.context.root_attempt,
        link,
    )?;
    if transport::local_peer(stream)? != peer {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    transport::write_frame(stream, &reply.reply.encode()?, deadline)?;
    let originals = reply.descriptions()?;
    jobserver_utils::descriptor_transfer::send(
        stream,
        &originals.iter().map(|fd| fd.as_fd()).collect::<Vec<_>>(),
        trust_resource_controller::workload::host::CPU_SET_MARKER,
        deadline,
    )?;
    // Attachment acknowledgement only: it holds the original authenticated
    // connection through the client's provenance check and grants no GO.
    if transport::read_frame(stream, deadline)? != [1] {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    driver.serve(
        stream,
        reply.deadline(),
        peer,
        &reply.reply.context.context.session,
    )
}

pub(super) fn member_original(
    stream: &CtlStream,
    peer: transport::Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
    observe: impl FnOnce(
        transport::Peer,
        trust_resource_controller::workload::host::MemberOriginalRequest,
    ) -> io::Result<crate::actor::WorkloadMemberOriginal>,
) -> io::Result<()> {
    use trust_resource_controller::workload::host;
    let binding = crate::actor::member_binding_role(hello, host::MEMBER_ORIGINAL_PURPOSE)?;
    let request = host::MemberOriginalRequest::decode(&transport::read_frame(stream, deadline)?)?;
    if binding.session != request.query.binding.session
        || binding.attempt != request.query.binding.root_attempt
        || transport::local_peer(stream)? != peer
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let reply = observe(peer, request)?;
    if transport::local_peer(stream)? != peer {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    transport::write_frame(stream, &reply.reply.encode()?, deadline)?;
    jobserver_utils::descriptor_transfer::send(
        stream,
        &[reply.description()],
        host::CPU_SET_MARKER,
        deadline,
    )?;
    if transport::read_frame(stream, deadline)? != [1] {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

fn worker_binding(
    stream: &CtlStream,
    peer: transport::Peer,
    hello: &aterm_json::Value,
    purpose: &str,
    binding: &trust_resource_controller::JobBinding,
) -> io::Result<()> {
    let selected = crate::actor::member_binding_role(hello, purpose)?;
    if selected.session != binding.session
        || selected.attempt != binding.root_attempt
        || transport::local_peer(stream)? != peer
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
pub(super) fn worker(
    stream: &CtlStream,
    peer: transport::Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
) -> io::Result<()> {
    use trust_resource_controller::workload::host::{self, worker};
    let request = worker::Request::decode(&transport::read_frame(stream, deadline)?)?;
    worker_binding(
        stream,
        peer,
        hello,
        worker::PURPOSE,
        &request.original.query.binding,
    )?;
    let mut offer = crate::actor::workload_worker_offer(peer, request)?;
    let deadline = deadline.min(offer.deadline());
    while !offer.poll()? {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let reply = crate::actor::workload_worker_accept(offer)?;
    if transport::local_peer(stream)? != peer {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    transport::write_frame(stream, &reply.reply.encode()?, deadline)?;
    if let Some(original) = reply.description() {
        jobserver_utils::descriptor_transfer::send(
            stream,
            &[original],
            host::CPU_SET_MARKER,
            deadline,
        )?;
    }
    if transport::read_frame(stream, deadline)? != [1] {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
pub(super) fn worker_return(
    stream: &CtlStream,
    peer: transport::Peer,
    hello: &aterm_json::Value,
    deadline: Instant,
) -> io::Result<()> {
    use trust_resource_controller::workload::host::worker;
    let request = worker::Return::decode(&transport::read_frame(stream, deadline)?)?;
    worker_binding(
        stream,
        peer,
        hello,
        worker::RETURN_PURPOSE,
        &request.request.original.query.binding,
    )?;
    let mut returned = crate::actor::workload_worker_return(peer, request)?;
    let deadline = deadline.min(returned.deadline());
    while !returned.poll()? {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let reply = crate::actor::workload_worker_returned(returned)?;
    if transport::local_peer(stream)? != peer {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    transport::write_frame(stream, &reply.encode()?, deadline)?;
    if transport::read_frame(stream, deadline)? != [1] {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "workload_transport/bank_pending_tests.rs"]
mod bank_pending_tests;

#[cfg(test)]
#[path = "workload_transport_tests.rs"]
mod tests;
