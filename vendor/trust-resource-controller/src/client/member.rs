// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Actual local helper ownership, never a fabricated target std::process::Child.
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
#[cfg(trust_std)]
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

use super::Client;
use crate::{
    BirthKind, Cancel, Channel, HelperFds, Identity, ProducerChallenge, Request, Started, Status,
    StatusFrame,
};

/// Actual helper sources, distinct from future target slots in Request.
pub struct MemberIo {
    pub stdin: Stdio,
    pub stdout: Stdio,
    pub stderr: Stdio,
    pub descriptions: Vec<OwnedFd>,
}
/// Once Request is offered, ambiguous delivery retains the original child.
pub enum MemberError {
    BeforeRequest(io::Error),
    Unknown { child: Box<ProxyChild>, error: io::Error },
}
impl std::fmt::Debug for MemberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforeRequest(error) => f.debug_tuple("BeforeRequest").field(error).finish(),
            Self::Unknown { error, .. } => {
                f.debug_struct("Unknown").field("error", error).finish_non_exhaustive()
            }
        }
    }
}
impl std::fmt::Display for MemberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native member {self:?}")
    }
}
impl std::error::Error for MemberError {}

/// Target PID is observation only. Unknown poisons the caller's borrowed
/// execution unit: it must not resume or retry using that same logical unit.
pub struct ProxyChild {
    helper: Child,
    host: std::sync::Arc<trust_resource_provenance::PinnedHelper>,
    control: Channel,
    proof: Channel,
    binding: ([u8; 16], [u8; 16]),
    descriptions: Vec<OwnedFd>,
    proof_done: bool,
    attached: bool,
    started: Option<Started>,
    terminal: Option<Status>,
    accounted: bool,
    accounting: Option<crate::accounting::Observation>,
    pending_terminal: Option<Status>,
    observed_terminal: Option<Status>,
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
/// Explicit descriptor actions are a compiler capability, not a request flag.
/// STOCK EXCEPTION: a stock compiler cannot launch members through this API.
fn require_member_inheritance() -> io::Result<()> {
    if cfg!(trust_std) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native member launch requires explicit descriptor inheritance",
        ))
    }
}

fn duplicate_for_inheritance(source: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    // SAFETY: duplicate a held description at a fresh slot >=5, replacing nothing.
    let raw = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful duplication transferred this fresh description.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // This NEW copy is intentionally inherited by the paired helper. Clear
    // close-on-fork on the new copy, never on the original guarded source.
    // SAFETY: change flags only on this freshly allocated owned descriptor.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}
fn inheritance_copies(
    request: &Request,
    originals: &[OwnedFd],
) -> io::Result<(Request, Vec<OwnedFd>)> {
    let mut mapped = std::collections::BTreeMap::new();
    let mut copies = Vec::new();
    for original in originals {
        let copy = duplicate_for_inheritance(original.as_fd())?;
        mapped.insert(original.as_raw_fd() as u32, copy.as_raw_fd() as u32);
        copies.push(copy);
    }
    let mut offered = request.clone();
    let remap = |descriptor: &mut crate::Descriptor| -> io::Result<()> {
        descriptor.source = *mapped.get(&descriptor.source).ok_or_else(invalid)?;
        Ok(())
    };
    if let Some(cpu) = &mut offered.delegations.cpu {
        remap(cpu)?;
    }
    for fence in &mut offered.delegations.fences {
        remap(fence)?;
    }
    match &mut offered.delegations.gnu {
        crate::GnuTransport::None => {}
        crate::GnuTransport::NumericPipe { read, write }
        | crate::GnuTransport::NamedFifo { read, write, .. } => {
            remap(read)?;
            remap(write)?;
        }
    }
    Ok((offered, copies))
}
fn source_slots(request: &Request) -> Vec<u32> {
    let mut slots: Vec<_> = request.delegations.fences.iter().map(|d| d.source).collect();
    if let Some(cpu) = request.delegations.cpu {
        slots.push(cpu.source);
    }
    match &request.delegations.gnu {
        crate::GnuTransport::None => {}
        crate::GnuTransport::NumericPipe { read, write }
        | crate::GnuTransport::NamedFifo { read, write, .. } => {
            slots.extend([read.source, write.source])
        }
    }
    slots.sort_unstable();
    slots
}
impl Client {
    /// Caller retains its logical CPU/GNU unit until positive Finished/Refused.
    /// Unknown requires conservative root failure, never argument-file retry.
    pub fn member(
        &self,
        request: &Request,
        io: MemberIo,
        deadline: Instant,
    ) -> Result<ProxyChild, MemberError> {
        self.member_inner(request, io, deadline, false)
    }
    /// Require the negotiated original-owning-wait route. Unsupported helpers
    /// or custodians refuse; this never retries through ordinary execution.
    pub fn member_accounted(
        &self,
        request: &Request,
        io: MemberIo,
        deadline: Instant,
    ) -> Result<ProxyChild, MemberError> {
        self.member_inner(request, io, deadline, true)
    }
    fn member_inner(
        &self,
        request: &Request,
        io: MemberIo,
        deadline: Instant,
        accounted: bool,
    ) -> Result<ProxyChild, MemberError> {
        require_member_inheritance().map_err(MemberError::BeforeRequest)?;
        let prepare = || -> io::Result<_> {
            request.validate()?;
            if matches!(request.role, crate::Role::SealedMetadata | crate::Role::ResolverQuery) {
                return Err(invalid());
            }
            // Even metadata borrows the original real CPU continuation; live
            // memory context alone never grants a second implicit CPU unit.
            if request.delegations.cpu.is_none() {
                return Err(invalid());
            }
            let mut actual: Vec<_> =
                io.descriptions.iter().map(|fd| fd.as_raw_fd() as u32).collect();
            actual.sort_unstable();
            if actual != source_slots(request) {
                return Err(invalid());
            }
            for fd in &io.descriptions {
                // SAFETY: observe flags of a held original description only.
                let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
                if flags < 0 || flags & libc::FD_CLOEXEC == 0 {
                    return Err(invalid());
                }
            }
            let (offered, descriptions) = inheritance_copies(request, &io.descriptions)?;
            let payload = if accounted {
                crate::accounting::AccountedRequest {
                    request: offered.clone(),
                    scope: crate::accounting::Scope::ChildAndWaitedDescendantsV1,
                }
                .encode()?
            } else {
                offered.encode()?
            };
            self.host.validate(deadline)?;
            let (control, child_control) = UnixStream::pair()?;
            let (proof, child_proof) = UnixStream::pair()?;
            let child_control = duplicate_for_inheritance(child_control.as_fd())?;
            let child_proof = duplicate_for_inheritance(child_proof.as_fd())?;
            let fds = HelperFds {
                control: child_control.as_raw_fd() as u32,
                producer: child_proof.as_raw_fd() as u32,
            };
            fds.validate_delegations(&offered.delegations)?;
            let mut inherited: Vec<_> = descriptions.iter().map(AsRawFd::as_raw_fd).collect();
            inherited.extend([child_control.as_raw_fd(), child_proof.as_raw_fd()]);
            let mut command = Command::new(self.host.helper_path());
            command
                .args(if accounted { fds.accounted_arguments()? } else { fds.arguments()? })
                .env_clear();
            // These transient intentional copies are not close-on-fork guarded.
            // All local v3 launches must use this paired helper; any future
            // foreign local phase needs a shared fork guard or SCM transport.
            // Keep the helper in this owned group. Explicit Trust std file
            // actions preserve only these descriptions, without forcing a
            // fork/pre_exec copy of a potentially large compiler address space.
            inherit_descriptions(&mut command, &inherited)?;
            Ok((payload, command, control, proof, child_control, child_proof, descriptions))
        };
        let (payload, mut command, control, proof, child_control, child_proof, mut descriptions) =
            prepare().map_err(MemberError::BeforeRequest)?;
        command.stdin(io.stdin).stdout(io.stdout).stderr(io.stderr);
        let control = Channel::new(control).map_err(MemberError::BeforeRequest)?;
        let proof = Channel::new(proof).map_err(MemberError::BeforeRequest)?;
        let helper = command.spawn().map_err(MemberError::BeforeRequest)?;
        drop((child_control, child_proof));
        descriptions.extend(io.descriptions);
        let mut child = ProxyChild {
            helper,
            host: self.host.clone(),
            control,
            proof,
            binding: (request.binding.root_attempt, request.member_attempt),
            descriptions,
            proof_done: false,
            attached: false,
            started: None,
            terminal: None,
            accounted,
            accounting: None,
            pending_terminal: None,
            observed_terminal: None,
        };
        if let Err(error) = child.host.validate_child(&child.helper, deadline) {
            // A helper was actually spawned. An identity/refusal failure does
            // not establish its retirement or return the caller's paid loan.
            return Err(MemberError::Unknown { child: Box::new(child), error });
        }
        if let Err(error) = child.control.send(&payload, deadline) {
            return Err(MemberError::Unknown { child: Box::new(child), error });
        }
        Ok(child)
    }
}

/// Native member launch requires explicit child-only inheritance, never a
/// process-wide CLOEXEC change or an implicit stock-std inheritance fallback.
/// Configure only native per-child file actions, granting no admission or
/// descriptor ownership. The caller must retain each actual original owned
/// description through spawn. Unsupported compilers refuse without a flag or
/// pre_exec fallback, and parent descriptor flags remain untouched.
pub fn inherit_descriptions(
    command: &mut Command,
    descriptions: &[std::os::fd::RawFd],
) -> io::Result<()> {
    #[cfg(trust_std)]
    {
        for &fd in descriptions {
            command.inherit_fd(fd);
        }
        Ok(())
    }
    #[cfg(not(trust_std))]
    {
        let _ = (command, descriptions);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native resource member launch requires Trust std explicit descriptor inheritance",
        ))
    }
}

#[cfg(all(test, not(trust_std)))]
mod inheritance_tests {
    use super::*;

    #[test]
    fn unsupported_inheritance_keeps_parent_descriptions_closed_on_exec() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let fd = socket.as_raw_fd();
        // SAFETY: the socket owns this live descriptor throughout the test.
        let before = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(before >= 0 && before & libc::FD_CLOEXEC != 0);
        let mut command = Command::new("unused-helper");
        assert_eq!(require_member_inheritance().unwrap_err().kind(), io::ErrorKind::Unsupported);
        let error = inherit_descriptions(&mut command, &[fd]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        // SAFETY: the same socket still owns the descriptor.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, before);
    }
}
impl ProxyChild {
    /// These are real helper-owned pipe endpoints forwarded as target stdio;
    /// they confer no target process identity or lifecycle control.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.helper.stdin.take()
    }
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.helper.stdout.take()
    }
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.helper.stderr.take()
    }
    /// Drain piped output concurrently before waiting, as for a native Child.
    /// Host timeout starts at actual target GO, never during admission wait.
    pub fn wait(&mut self) -> Status {
        loop {
            if let Some(status) = self.try_wait(Instant::now() + std::time::Duration::from_secs(8))
            {
                return status;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    pub fn started(&self) -> Option<Started> {
        self.started
    }
    /// Available only from the negotiated terminal for this original attempt.
    /// Inner absent fields remain uncollected, even after a known child exit.
    pub fn accounting(&self) -> Option<crate::accounting::Observation> {
        if matches!(self.terminal, Some(Status::Finished { .. } | Status::Refused { .. })) {
            self.accounting
        } else {
            None
        }
    }
    /// Last well-framed reported terminal, retained for diagnostics if later
    /// transport failure makes the actual result Unknown. This is no grant.
    pub fn observed_terminal(&self) -> Option<&Status> {
        self.observed_terminal.as_ref()
    }
    pub fn cancel(&mut self, deadline: Instant) -> io::Result<()> {
        if matches!(self.terminal, Some(Status::Finished { .. } | Status::Refused { .. })) {
            return Ok(());
        }
        self.control.send(
            &Cancel { root_attempt: self.binding.0, member_attempt: self.binding.1 }.encode()?,
            deadline,
        )
    }
    fn answer(&mut self, deadline: Instant) -> io::Result<()> {
        let proof = ProducerChallenge::decode(&self.proof.receive(deadline)?)?;
        let actual = self.host.validate_child(&self.helper, deadline)?;
        let identity = Identity {
            pid: actual.pid,
            birth: actual.birth,
            #[cfg(target_os = "macos")]
            birth_kind: BirthKind::MacosWallMicroseconds,
            #[cfg(target_os = "linux")]
            birth_kind: BirthKind::LinuxStartTicks,
        };
        if proof.root_attempt != self.binding.0
            || proof.member_attempt != self.binding.1
            || proof.helper != identity
            || self.proof_done
        {
            return Err(invalid());
        }
        self.proof.send(&proof.answer()?, deadline)?;
        self.host.validate_child(&self.helper, deadline)?;
        self.proof_done = true;
        Ok(())
    }
    /// No blocking between frames. Partial frames have a bounded deadline;
    /// malformed/late/lost frames poison this one borrowed execution context.
    pub fn try_wait(&mut self, deadline: Instant) -> Option<Status> {
        if let Some(status) = &self.terminal {
            return Some(status.clone());
        }
        match self.poll(deadline) {
            Ok(Some(status)) => {
                self.terminal = Some(status.clone());
                Some(status)
            }
            Ok(None) => None,
            Err(_) => {
                self.accounting = None;
                let status = Status::Unknown {
                    reason: "native member lifecycle unavailable".into(),
                    started: self.started,
                };
                self.terminal = Some(status.clone());
                Some(status)
            }
        }
    }
    fn poll(&mut self, deadline: Instant) -> io::Result<Option<Status>> {
        if self.pending_terminal.is_some() {
            let eof = self.control.at_eof()?;
            if self.control.ready_to_receive()? && !eof {
                return Err(invalid());
            }
            if !eof {
                return Ok(None);
            }
            if self.proof.ready_to_receive()? && !self.proof.at_eof()? {
                return Err(invalid());
            }
            return match self.helper.try_wait()? {
                Some(status) if status.success() => Ok(self.pending_terminal.take()),
                Some(_) => Err(invalid()),
                None => Ok(None),
            };
        }
        if !self.proof_done && self.proof.ready_to_receive()? {
            self.answer(deadline)?;
        }
        if self.control.ready_to_receive()? {
            let bytes = self.control.receive(deadline)?;
            let (frame, accounting) = if self.accounted {
                match crate::accounting::TerminalFrame::decode(
                    &bytes,
                    self.binding.0,
                    self.binding.1,
                ) {
                    Ok(value) => (value.finished, Some(value.observation)),
                    Err(_) => {
                        let plain = StatusFrame::decode(&bytes, self.binding.0, self.binding.1)?;
                        if matches!(plain.status, Status::Finished { .. } | Status::Refused { .. })
                        {
                            return Err(invalid());
                        }
                        (plain, None)
                    }
                }
            } else {
                (StatusFrame::decode(&bytes, self.binding.0, self.binding.1)?, None)
            };
            match frame.status {
                Status::Attached if !self.attached && self.started.is_none() => {
                    self.attached = true
                }
                Status::Started(started) if self.proof_done && self.started.is_none() => {
                    self.started = Some(started)
                }
                Status::Finished { .. } if self.proof_done && self.started.is_some() => {
                    self.accounting = accounting;
                    self.observed_terminal = Some(frame.status.clone());
                    if self.accounted {
                        self.pending_terminal = Some(frame.status);
                        return Ok(None);
                    }
                    return Ok(Some(frame.status));
                }
                Status::Refused { .. } if self.started.is_none() => {
                    self.accounting = accounting;
                    self.observed_terminal = Some(frame.status.clone());
                    if self.accounted {
                        self.pending_terminal = Some(frame.status);
                        return Ok(None);
                    }
                    return Ok(Some(frame.status));
                }
                Status::Unknown { started, .. } => {
                    if self.started.is_some() && started != self.started {
                        return Err(invalid());
                    }
                    self.started = started.or(self.started);
                    return Ok(Some(frame.status));
                }
                _ => return Err(invalid()),
            }
            return Ok(None);
        }
        if self.helper.try_wait()?.is_some() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(None)
    }
}
impl Drop for ProxyChild {
    fn drop(&mut self) {
        // Actual helper only: no target-PID wait/kill. Closing control asks the
        // host to cancel; it retains accepted copies until actual scope drain.
        let _ = self.helper.kill();
        let _ = self.helper.wait();
        self.descriptions.clear();
    }
}
