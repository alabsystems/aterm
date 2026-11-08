// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Startup transfer of original all-domain guards. The early locator is never
//! authority. Only the authenticated live owner sends the original descriptions;
//! decoding a ReadyReply or opening a matching inode cannot construct custody.

use std::fs::File;
use std::io;
use std::mem::{size_of, size_of_val};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Instant;

use crate::workload::host::{self, ReadyReply, ReadyRequest};
use crate::{Channel, FileIdentity};

mod census;
pub use census::CensusSnapshot;
mod produced;
pub use produced::RegisteredOutput;
mod output;
pub use output::RetainedOutputLocks;

mod store;
mod worker;
pub use store::{
    EarlyHostStore, PendingHostStore, capture_host_store, capture_host_store_descriptor,
    snapshot_host_store, snapshot_host_store_descriptor,
};
pub use worker::{AuthenticatedWorkerCpu, WorkerLoan};

/// Root handoffs create no child owner. Unknown retains this original control
/// in its poisoned state; callers must not replay or resume its phase/bank.
#[derive(Debug)]
pub enum WorkloadControlError {
    BeforeRequest(io::Error),
    Refused { reason: String },
    Unknown(io::Error),
}
impl std::fmt::Display for WorkloadControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native workload control {self:?}")
    }
}
impl std::error::Error for WorkloadControlError {}

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "original workload CPU transfer unavailable")
}
fn identity(fd: RawFd) -> io::Result<FileIdentity> {
    // SAFETY: initialized native output, observing one existing description.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: geteuid only observes the current identity.
    if stat.st_uid != unsafe { libc::geteuid() }
        || stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_mode & 0o077 != 0
        || stat.st_nlink != 1
    {
        return Err(refused());
    }
    Ok(FileIdentity { device: stat.st_dev as u64, inode: stat.st_ino as u64 })
}

/// No descriptor allocation, locking or user configuration is performed here.
pub struct PendingCpuSet {
    originals: Vec<(RawFd, FileIdentity)>,
}
impl PendingCpuSet {
    pub fn is_present(&self) -> bool {
        !self.originals.is_empty()
    }
}
/// Close-only owned early descriptions. They retain startup custody but never manufacture
/// Lease values or prove that an arbitrary offered file holds a flock.
pub struct EarlyCpuSet {
    originals: Vec<(RawFd, FileIdentity, OwnedFd)>,
}
impl EarlyCpuSet {
    pub fn is_present(&self) -> bool {
        !self.originals.is_empty()
    }
}

/// Call alongside snapshot_sdk_lease BEFORE GNU import or descriptor allocation.
pub fn snapshot_cpu_set() -> io::Result<PendingCpuSet> {
    let Some(value) = std::env::var_os(host::CPU_SET_FDS_ENV) else {
        return Ok(PendingCpuSet { originals: Vec::new() });
    };
    snapshot(value.to_str().ok_or_else(refused)?)
}
fn snapshot(value: &str) -> io::Result<PendingCpuSet> {
    if value.len() > host::MAX_CPU_SET * 12 {
        return Err(refused());
    }
    let mut originals = Vec::new();
    for word in value.split(',') {
        if originals.len() == host::MAX_CPU_SET {
            return Err(refused());
        }
        let fd: RawFd = word.parse().map_err(|_| refused())?;
        if fd < 6 || fd.to_string() != word {
            return Err(refused());
        }
        let backing = identity(fd)?;
        if originals
            .iter()
            .any(|(other_fd, other_backing)| *other_fd == fd || *other_backing == backing)
        {
            return Err(refused());
        }
        originals.push((fd, backing));
    }
    if originals.is_empty() {
        return Err(refused());
    }
    Ok(PendingCpuSet { originals })
}

/// Capture only after the fixed early GNU/SDK importer, before configuration,
/// threads, callbacks or child creation. Takes the exclusively mapped child
/// descriptors, sets their local CLOEXEC, and removes the now-private locator.
/// # Safety
/// No code between snapshot and capture may close/reuse advertised descriptors;
/// the native custodian transfers exclusive ownership of the mapped set,
/// disjoint from stdio/GNU/SDK bootstrap fds. No other thread may read the process
/// environment while the early locator is removed.
pub unsafe fn capture_cpu_set(pending: PendingCpuSet) -> io::Result<EarlyCpuSet> {
    // SAFETY: this is the fixed single-threaded bootstrap described above.
    unsafe { std::env::remove_var(host::CPU_SET_FDS_ENV) };
    // SAFETY: the caller transfers exclusive ownership of the mapped set.
    unsafe { take_owned_cpu_set(pending) }
}
unsafe fn take_owned_cpu_set(pending: PendingCpuSet) -> io::Result<EarlyCpuSet> {
    let originals: Vec<_> = pending
        .originals
        .into_iter()
        .map(|(fd, before)| {
            // SAFETY: the exact early role transferred these unique mapped fds,
            // with no other owner permitted by this function's contract.
            (fd, before, unsafe { OwnedFd::from_raw_fd(fd) })
        })
        .collect();
    for (fd, before, held) in &originals {
        if identity(*fd)? != *before || identity(held.as_raw_fd())? != *before {
            return Err(refused());
        }
        // SAFETY: local flags of this exclusively owned child descriptor only;
        // never changes the host's original fd flags or shared lock state.
        let flags = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(*fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(EarlyCpuSet { originals })
}

/// Non-Clone, non-deserializable custody received from the original live native
/// owner. There is no constructor from paths, counters, observations or Files.
pub struct AuthenticatedWorkloadStartup {
    reply: ReadyReply,
    originals: Vec<OwnedFd>,
    control: WorkloadControl,
}
/// One registered compiler's original primary. No Clone/Deserialize/path/File
/// constructor is exposed; only the authenticated live member exchange mints it.
pub struct AuthenticatedMemberCpu {
    reply: host::MemberOriginalReply,
    original: OwnedFd,
}
impl AuthenticatedMemberCpu {
    pub fn observation(&self) -> &host::MemberOriginalReply {
        &self.reply
    }
    pub fn into_original(self) -> (host::MemberOriginalReply, File) {
        (self.reply, File::from(self.original))
    }
}
impl super::Client {
    pub fn workload_member_original(
        &self,
        request: &host::MemberOriginalRequest,
        early: EarlyCpuSet,
        deadline: Instant,
    ) -> Result<AuthenticatedMemberCpu, super::CallError> {
        let payload = request.encode().map_err(super::before)?;
        let original = self.host.connect(deadline).map_err(super::before)?;
        let mut channel = Channel::new(original.stream().try_clone().map_err(super::before)?)
            .map_err(super::before)?;
        channel.send(&request.hello().map_err(super::before)?, deadline).map_err(super::before)?;
        let exchange = (|| {
            original.revalidate(deadline)?;
            channel.send(&payload, deadline)?;
            let reply = host::MemberOriginalReply::decode(&channel.receive(deadline)?, request)?;
            live_context(&reply.context.context, Some(request.member_attempt))?;
            super::sdk::validate_sdk_keeper(&reply.context.context)?;
            let mut originals = receive_set(&channel, &[&reply.cpu], early, deadline)?;
            original.revalidate(deadline)?;
            channel.send(&[1], deadline)?;
            Ok(AuthenticatedMemberCpu { reply, original: originals.remove(0) })
        })();
        exchange.map_err(|error| super::CallError { delivery: super::Delivery::Unknown, error })
    }
}
impl AuthenticatedWorkloadStartup {
    pub fn observation(&self) -> &ReadyReply {
        &self.reply
    }
    /// Transfer exactly the original received kernel descriptions to the native
    /// lease owner. This method grants nothing to a decoded ReadyReply.
    pub fn into_originals(self) -> (ReadyReply, Vec<File>, WorkloadControl) {
        (self.reply, self.originals.into_iter().map(File::from).collect(), self.control)
    }
}

pub(super) struct ReceivedOriginals {
    reply: ReadyReply,
    originals: Vec<OwnedFd>,
}
impl ReceivedOriginals {
    pub(super) fn attach_control(
        self,
        channel: Channel,
        host: std::sync::Arc<trust_resource_provenance::PinnedHelper>,
    ) -> AuthenticatedWorkloadStartup {
        let control = WorkloadControl::from_original_channel(
            channel,
            host,
            self.reply.context.context.root_attempt,
            self.reply.invocation_sha256,
        );
        AuthenticatedWorkloadStartup { reply: self.reply, originals: self.originals, control }
    }
}

impl WorkloadControl {
    /// Internal assembly after authenticated original-description receipt.
    /// Plain context or digest observations cannot call this constructor.
    pub(super) fn from_original_channel(
        channel: Channel,
        host: std::sync::Arc<trust_resource_provenance::PinnedHelper>,
        root_attempt: [u8; 16],
        invocation_sha256: [u8; 32],
    ) -> Self {
        Self {
            channel,
            host,
            root_attempt,
            invocation_sha256,
            last_sequence: 0,
            last_phase_sequence: 0,
            active_phase: None,
            output_locks: None,
            pending: None,
            failed: false,
            drained: false,
        }
    }
}

/// The original authenticated root connection. One broker thread owns it;
/// queue/phase coordinators communicate with that thread, never race readers.
/// Channel loss is Unknown and may not return any original bank loan.
pub struct WorkloadControl {
    channel: Channel,
    host: std::sync::Arc<trust_resource_provenance::PinnedHelper>,
    root_attempt: [u8; 16],
    invocation_sha256: [u8; 32],
    last_sequence: u64,
    last_phase_sequence: u64,
    active_phase: Option<([u8; 16], u64)>,
    output_locks: Option<host::output::Request>,
    pending: Option<host::bank::Request>,
    failed: bool,
    drained: bool,
}
impl WorkloadControl {
    pub fn poll_bank_request(
        &mut self,
        deadline: Instant,
    ) -> io::Result<Option<host::bank::Request>> {
        if self.failed || self.drained || self.pending.is_some() {
            return Err(refused());
        }
        let result = (|| {
            self.host.validate(deadline)?;
            if self.channel.at_eof()? {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            if !self.channel.ready_to_receive()? {
                return Ok(None);
            }
            let request = host::bank::Request::decode(&self.channel.receive(deadline)?)?;
            if request.key.root_attempt != self.root_attempt
                || request.key.invocation_sha256 != self.invocation_sha256
                || request.key.sequence != self.last_sequence.checked_add(1).ok_or_else(refused)?
            {
                return Err(refused());
            }
            self.last_sequence = request.key.sequence;
            self.pending = Some(request.clone());
            Ok(Some(request))
        })();
        if result.is_err() {
            self.failed = true
        }
        result
    }
    /// The caller's bank holds a real BankLoan through this transfer and until
    /// the matching positive return. Passing a descriptor is transport only;
    /// it cannot select a free slot or change the native bank's state.
    pub fn reply_bank(
        &mut self,
        reply: &host::bank::Reply,
        original: Option<std::os::fd::BorrowedFd<'_>>,
        deadline: Instant,
    ) -> io::Result<()> {
        if self.failed || self.drained {
            return Err(refused());
        }
        let result = (|| {
            let request = self.pending.as_ref().ok_or_else(refused)?;
            let bytes = reply.encode()?;
            host::bank::Reply::decode(&bytes, request)?;
            match (&reply.decision, original) {
                (host::bank::Decision::Granted { backing, .. }, Some(fd))
                    if identity(fd.as_raw_fd())? == *backing => {}
                (host::bank::Decision::Granted { .. }, _) => return Err(refused()),
                (_, None) => {}
                (_, Some(_)) => return Err(refused()),
            }
            self.host.validate(deadline)?;
            self.channel.send(&bytes, deadline)?;
            if let Some(fd) = original {
                // Borrow the existing stream; this duplicate has no new peer,
                // resource or replay state and is closed after the one send.
                let raw = unsafe {
                    libc::fcntl(self.channel.as_fd().as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5)
                };
                if raw < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: fresh owned duplicate of the same authenticated socket.
                let stream =
                    std::os::unix::net::UnixStream::from(unsafe { OwnedFd::from_raw_fd(raw) });
                jobserver_utils::descriptor_transfer::send(
                    &stream,
                    &[fd],
                    host::bank::ORIGINAL_MARKER,
                    deadline,
                )?;
            }
            self.pending = None;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true
        }
        result
    }
    /// Called ONLY by the same original broker after queue/bank parking and
    /// actual worker joins. Enter borrows the SAME original GNU FIFO endpoints
    /// while the caller retains its real unused-token escrow. Returned bytes
    /// establish no CPU guard or produced-artifact eligibility by themselves.
    pub fn request_phase(
        &mut self,
        request: &host::phase::Request,
        gnu: Option<[std::os::fd::BorrowedFd<'_>; 2]>,
        deadline: Instant,
    ) -> io::Result<host::phase::Reply> {
        if self.failed || self.drained || self.pending.is_some() {
            return Err(refused());
        }
        let result = (|| {
            let bytes = request.encode()?;
            if request.key.root_attempt != self.root_attempt
                || request.key.invocation_sha256 != self.invocation_sha256
                || request.key.sequence
                    != self.last_phase_sequence.checked_add(1).ok_or_else(refused)?
            {
                return Err(refused());
            }
            match (request.operation, gnu) {
                (host::phase::Operation::Enter { gnu: backing, .. }, Some([read, write])) => {
                    if self.output_locks.is_none()
                        || self.active_phase.is_some()
                        || read.as_raw_fd() == write.as_raw_fd()
                        || fifo_identity(read, libc::O_RDONLY)? != backing
                        || fifo_identity(write, libc::O_WRONLY)? != backing
                    {
                        return Err(refused());
                    }
                }
                (host::phase::Operation::Enter { .. }, None) => return Err(refused()),
                (_, None) => {}
                (_, Some(_)) => return Err(refused()),
            }
            match request.operation {
                host::phase::Operation::Leave { phase }
                    if self.active_phase.map(|(nonce, _)| nonce) != Some(phase) =>
                {
                    return Err(refused());
                }
                host::phase::Operation::Drained if self.active_phase.is_some() => {
                    return Err(refused());
                }
                _ => {}
            }
            self.host.validate(deadline)?;
            self.channel.send(&bytes, deadline)?;
            if let Some(fds) = gnu {
                let raw = unsafe {
                    libc::fcntl(self.channel.as_fd().as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5)
                };
                if raw < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: fresh close-only duplicate of the SAME control
                // connection. It introduces no other reader or grant source.
                let stream =
                    std::os::unix::net::UnixStream::from(unsafe { OwnedFd::from_raw_fd(raw) });
                jobserver_utils::descriptor_transfer::send(
                    &stream,
                    &fds,
                    host::phase::GNU_MARKER,
                    deadline,
                )?;
            }
            let reply = host::phase::Reply::decode(&self.channel.receive(deadline)?, request)?;
            self.host.validate(deadline)?;
            self.last_phase_sequence = request.key.sequence;
            match &reply.decision {
                host::phase::Decision::Ready { phase, .. } => {
                    self.active_phase = Some((*phase, request.key.sequence))
                }
                host::phase::Decision::Left { .. } => self.active_phase = None,
                _ => {}
            }
            if matches!(reply.decision, host::phase::Decision::Drained) {
                self.drained = true;
            }
            Ok(reply)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

fn fifo_identity(fd: std::os::fd::BorrowedFd<'_>, access: libc::c_int) -> io::Result<FileIdentity> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: live borrowed description and initialized native output.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if stat.st_mode & libc::S_IFMT != libc::S_IFIFO
        || flags < 0
        || flags & libc::O_ACCMODE != access
    {
        return Err(refused());
    }
    Ok(FileIdentity { device: stat.st_dev as u64, inode: stat.st_ino as u64 })
}

pub(super) fn receive_originals(
    channel: &Channel,
    reply: ReadyReply,
    early: EarlyCpuSet,
    request: &ReadyRequest,
    deadline: Instant,
) -> io::Result<ReceivedOriginals> {
    use sha2::{Digest, Sha256};
    let digest: [u8; 32] = Sha256::digest(request.invocation.encode()?).into();
    let context = &reply.context.context;
    if reply.invocation_sha256 != digest {
        return Err(refused());
    }
    live_context(context, None)?;
    super::sdk::validate_sdk_keeper(&reply.context.context)?;
    let set: Vec<_> = reply.cpu_set().collect();
    let originals = receive_set(channel, &set, early, deadline)?;
    Ok(ReceivedOriginals { reply, originals })
}
pub(super) fn live_context(context: &crate::Context, member: Option<[u8; 16]>) -> io::Result<()> {
    let (machine, boot) = trust_resource_provenance::process::host_boot()?;
    let current = trust_resource_provenance::process::read_identity(std::process::id())?;
    #[cfg(target_os = "macos")]
    let birth_kind = crate::BirthKind::MacosWallMicroseconds;
    #[cfg(target_os = "linux")]
    let birth_kind = crate::BirthKind::LinuxStartTicks;
    let identity = crate::Identity { pid: current.pid, birth: current.birth, birth_kind };
    let subject = match member {
        None => crate::Subject::Root { identity },
        Some(attempt) => crate::Subject::RegisteredMember { identity, attempt },
    };
    if context.host != machine
        || context.boot != boot
        || context.subject != subject
        || context.original_root.birth_kind != birth_kind
        || (member.is_none() && context.original_root != identity)
    {
        return Err(refused());
    }
    Ok(())
}
pub(super) fn receive_set(
    channel: &Channel,
    set: &[&host::PrimaryCpu],
    early: EarlyCpuSet,
    deadline: Instant,
) -> io::Result<Vec<OwnedFd>> {
    if set.len() != early.originals.len() {
        return Err(refused());
    }
    for (cpu, (fd, backing, held)) in set.iter().zip(&early.originals) {
        if cpu.inherited_fd != *fd as u32
            || cpu.backing != *backing
            || identity(held.as_raw_fd())? != *backing
            || identity(*fd)? != *backing
        {
            return Err(refused());
        }
    }
    let originals = receive(channel, set.len(), deadline)?;
    for (cpu, fd) in set.iter().zip(&originals) {
        if identity(fd.as_raw_fd())? != cpu.backing {
            return Err(refused());
        }
    }
    // Initial caller-offered originals are now closed, even if independently
    // opened. They can never become the returned native execution authority.
    drop(early);
    Ok(originals)
}

fn receive(channel: &Channel, count: usize, deadline: Instant) -> io::Result<Vec<OwnedFd>> {
    if count == 0 || count > host::MAX_CPU_SET {
        return Err(refused());
    }
    let mut byte = [0u8];
    // Large enough for every kernel-permitted SCM_RIGHTS population on our
    // supported Unix hosts, so rejection adopts every delivered descriptor.
    let mut control = [0usize; 4096 / size_of::<usize>()];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    // SAFETY: zero initializes optional native pointers/counts.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = size_of_val(&control) as _;
    let received = loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC;
        #[cfg(target_os = "macos")]
        let flags = libc::MSG_DONTWAIT;
        // SAFETY: every message buffer remains live and bounded.
        let n = unsafe { libc::recvmsg(channel.as_fd().as_raw_fd(), &mut msg, flags) };
        if n >= 0 {
            break n;
        }
        let error = io::Error::last_os_error();
        if !matches!(error.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) {
            return Err(error);
        }
        let millis = deadline.saturating_duration_since(Instant::now()).as_millis().min(50) as i32;
        let mut poll =
            libc::pollfd { fd: channel.as_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one initialized poll descriptor; deadline is never renewed.
        if unsafe { libc::poll(&mut poll, 1, millis) } < 0
            && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
        {
            return Err(io::Error::last_os_error());
        }
        msg.msg_controllen = size_of_val(&control) as _;
        msg.msg_flags = 0;
    };
    let mut fds = Vec::new();
    let mut valid = received == 1
        && byte == [host::CPU_SET_MARKER]
        && msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) == 0;
    if msg.msg_controllen as usize > size_of_val(&control) {
        valid = false;
        msg.msg_controllen = size_of_val(&control) as _;
    }
    // SAFETY: inspect only complete native headers and own every received fd,
    // including on malformed/replayed/truncated transfers before rejecting.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&msg);
        while !header.is_null() {
            let base = libc::CMSG_LEN(0) as usize;
            let remaining =
                msg.msg_controllen as usize - (header as usize - msg.msg_control as usize);
            if remaining < base || ((*header).cmsg_len as usize) < base {
                valid = false;
                break;
            }
            let length = (*header).cmsg_len as usize;
            let payload = length.min(remaining) - base;
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                valid &= payload % size_of::<libc::c_int>() == 0;
                let data = libc::CMSG_DATA(header).cast::<libc::c_int>();
                for index in 0..payload / size_of::<libc::c_int>() {
                    fds.push(OwnedFd::from_raw_fd(data.add(index).read_unaligned()));
                }
            } else {
                valid = false;
            }
            if length > remaining {
                valid = false;
                break;
            }
            header = libc::CMSG_NXTHDR(&msg, header);
        }
    }
    if !valid || fds.len() != count {
        return Err(refused());
    }
    for fd in &fds {
        // SAFETY: setting CLOEXEC only on each fresh owned fd changes no source.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        if flags < 0
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(fds)
}

#[cfg(test)]
mod tests;
