// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private owned-child bootstrap qualification. Not a SolverMember recipe or
//! exported resource receipt: actual root/claim/profile integration is separate.
//! The original Child remains with the caller through every error and cleanup.
use aterm_ctl::resource_admission_transport as transport;
use aterm_uds::{CtlStream, process, spawnfd};
use std::{
    io::{self, Read as _},
    os::fd::OwnedFd,
    time::Instant,
};

#[path = "solver_bootstrap/wire.rs"]
mod wire;
use wire::{Frame, LEN, invalid};

enum State {
    Offer,
    Bootstrap,
    Ready,
    RunWriting,
    Authorized,
    Failed,
}
enum StopSource {
    Initial,
    Prepared(crate::metadata_prelude::PreparedTarget),
}

pub(super) struct Owner {
    channel: CtlStream,
    child_channel: Option<OwnedFd>,
    offer: Frame,
    original: Option<process::Lineage>,
    state: State,
    stop: StopSource,
    retirement: Option<crate::metadata_prelude::NoForkTarget>,
    output: [u8; LEN],
    written: usize,
    input: [u8; LEN],
    read: usize,
    deadline: Instant,
}

/// Immutable intent for the actor's durable original-member journal. Neither
/// this observation nor the private protocol creates CPU or memory ownership.
pub(super) struct RunIntent {
    pub attempt: [u8; 16],
    pub pid: u32,
    pub birth: u64,
    pub units: u32,
    pub footprint_mib: u32,
    pub monotonic_ns: u64,
}

pub(super) type Accepted = trust_resource_controller::solver_bootstrap::Accepted;

impl Owner {
    pub fn new(
        attempt: [u8; 16],
        units: u32,
        limit: spawnfd::FootprintLimit,
        deadline: Instant,
    ) -> io::Result<Self> {
        if attempt == [0; 16] || units == 0 {
            return Err(invalid());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut challenge = [0; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut challenge)?;
        if challenge == [0; 32] {
            return Err(invalid());
        }
        let (channel, child_channel) = aterm_uds::kept_out(CtlStream::pair)?;
        channel.set_nonblocking(true)?;
        child_channel.set_nonblocking(true)?;
        Ok(Self {
            channel,
            child_channel: Some(child_channel.into()),
            offer: Frame {
                kind: 1,
                attempt,
                challenge,
                pid: 0,
                birth: 0,
                footprint_mib: limit.mib(),
                units,
                monotonic_ns: 0,
            },
            original: None,
            state: State::Offer,
            stop: StopSource::Initial,
            retirement: None,
            output: [0; LEN],
            written: 0,
            input: [0; LEN],
            read: 0,
            deadline,
        })
    }

    pub fn extra(&mut self, target: i32) -> io::Result<(OwnedFd, i32)> {
        if target < 3 {
            return Err(invalid());
        }
        Ok((self.child_channel.take().ok_or_else(invalid)?, target))
    }

    pub fn bind_prepared(
        &mut self,
        target: crate::metadata_prelude::PreparedTarget,
    ) -> io::Result<()> {
        if !matches!(self.state, State::Offer)
            || !matches!(self.stop, StopSource::Initial)
            || self.original.is_some()
            || self.written != 0
            || self.read != 0
            || self.child_channel.is_some()
            || target.configured_footprint_mib() != self.offer.footprint_mib
        {
            self.state = State::Failed;
            return Err(invalid());
        }
        let original = target.lineage();
        self.offer.pid = original.identity.pid as u32;
        self.offer.birth = original.identity.birth.0;
        self.output = self.offer.encode();
        self.original = Some(original);
        self.stop = StopSource::Prepared(target);
        Ok(())
    }

    pub fn take_retirement(&mut self) -> Option<crate::metadata_prelude::NoForkTarget> {
        self.retirement.take()
    }

    fn validate(
        &self,
        child: &mut spawnfd::Child,
        image: &mut impl FnMut(u32) -> io::Result<()>,
    ) -> io::Result<process::Lineage> {
        if Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if child.exited()? {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let current = process::read_lineage(child.id() as i32).ok_or_else(invalid)?;
        if current.parent != std::process::id()
            || self.original.is_some_and(|original| original != current)
        {
            return Err(invalid());
        }
        image(child.id())?;
        if process::read_lineage(child.id() as i32) != Some(current) {
            return Err(invalid());
        }
        Ok(current)
    }

    /// Nonblocking actor poll. Startup work runs before Ready, without a solver
    /// runtime clock or a synthetic Started. `image` checks the retained artifact.
    pub fn poll_ready(
        &mut self,
        child: &mut spawnfd::Child,
        mut image: impl FnMut(u32) -> io::Result<()>,
    ) -> io::Result<bool> {
        let result = self.ready_step(child, &mut image);
        if result.is_err() {
            self.state = State::Failed;
        }
        result
    }

    fn ready_step(
        &mut self,
        child: &mut spawnfd::Child,
        image: &mut impl FnMut(u32) -> io::Result<()>,
    ) -> io::Result<bool> {
        if matches!(self.state, State::Ready) {
            self.validate(child, image)?;
            return Ok(true);
        }
        if !matches!(self.state, State::Offer | State::Bootstrap) {
            return Err(invalid());
        }
        let current = self.validate(child, image)?;
        if self.original.is_none() {
            self.original = Some(current);
            self.offer.pid = child.id();
            self.offer.birth = current.identity.birth.0;
            self.output = self.offer.encode();
        }
        if matches!(self.state, State::Offer) {
            if !self.write_pending()? {
                return Ok(false);
            }
            self.validate(child, image)?;
            // Only the trusted early bootstrap runs after this owned resume.
            self.state = State::Bootstrap;
            match &mut self.stop {
                StopSource::Initial => {
                    let stop = child.try_job_event()?.ok_or_else(invalid)?;
                    if !matches!(
                        stop.notice(),
                        spawnfd::ChildNotice::Stopped(spawnfd::ChildStop::SuspendedStart)
                    ) || !matches!(
                        stop.acknowledge()?,
                        Some(spawnfd::ChildEvent::Stopped(
                            spawnfd::ChildStop::SuspendedStart
                        ))
                    ) {
                        return Err(invalid());
                    }
                    child.resume()?;
                }
                StopSource::Prepared(target) => target.resume(child)?,
            }
        }
        while self.read < LEN {
            match (&self.channel).read(&mut self.input[self.read..]) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(count) => self.read += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        if Frame::decode(&self.input)? != self.offer.response(2, 0) {
            return Err(invalid());
        }
        let mut extra = [0];
        match (&self.channel).read(&mut extra) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
            _ => return Err(invalid()),
        }
        self.validate(child, image)?;
        if !trust_exec_policy::fork_denied(child.id())? {
            return Err(invalid());
        }
        if let StopSource::Prepared(target) = std::mem::replace(&mut self.stop, StopSource::Initial)
        {
            self.retirement = Some(target.sealed(child)?);
        }
        self.state = State::Ready;
        Ok(true)
    }

    /// Select and durably record the exact Run clock at dispatch, then check
    /// cancellation/image again before the first byte. Later I/O polls never
    /// re-commit or change that recorded clock. Errors poison this same attempt.
    pub fn poll_run(
        &mut self,
        child: &mut spawnfd::Child,
        mut image: impl FnMut(u32) -> io::Result<()>,
        mut cancelled: impl FnMut() -> bool,
        commit: impl FnOnce(&RunIntent) -> io::Result<()>,
    ) -> io::Result<bool> {
        if matches!(self.state, State::Authorized) {
            return Ok(true);
        }
        if !matches!(self.state, State::Ready | State::RunWriting) {
            return Err(invalid());
        }
        let result = (|| {
            if cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.validate(child, &mut image)?;
            if matches!(self.state, State::Ready) {
                // Poison before calling durable storage: an error may mean the
                // record committed but its acknowledgement was lost.
                self.state = State::Failed;
                let selected_clock = monotonic_ns()?;
                self.output = self.offer.response(3, selected_clock).encode();
                self.written = 0;
                commit(&RunIntent {
                    attempt: self.offer.attempt,
                    pid: self.offer.pid,
                    birth: self.offer.birth,
                    units: self.offer.units,
                    footprint_mib: self.offer.footprint_mib,
                    monotonic_ns: selected_clock,
                })?;
                self.validate(child, &mut image)?;
                if cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.state = State::RunWriting;
            }
            self.write_pending()
        })();
        match result {
            Ok(true) => {
                self.state = State::Authorized;
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(error) => {
                self.state = State::Failed;
                Err(error)
            }
        }
    }

    /// An observation of this original frame's delivery, never replay authority.
    pub fn run_offered(&self) -> bool {
        matches!(self.state, State::RunWriting | State::Authorized) && self.written > 0
    }

    fn write_pending(&mut self) -> io::Result<bool> {
        if Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        while self.written < LEN {
            match aterm_uds::fdpass::send_vectored(
                &self.channel,
                &[io::IoSlice::new(&self.output[self.written..])],
            ) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => self.written += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }
}

fn monotonic_ns() -> io::Result<u64> {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: read-only native clock initializes the writable timespec on success.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, time.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful native call initialized this complete timespec.
    let time = unsafe { time.assume_init() };
    u64::try_from(time.tv_sec)
        .ok()
        .and_then(|s| s.checked_mul(1_000_000_000))
        .zip(
            u64::try_from(time.tv_nsec)
                .ok()
                .filter(|n| *n < 1_000_000_000),
        )
        .and_then(|(s, n)| s.checked_add(n))
        .filter(|n| *n > 0)
        .ok_or_else(invalid)
}

/// The maintained final-image hook. `authenticate` must establish the actual
/// parent Host/Store/claim; the socketpair's peer alone proves only its creator.
/// This function never discovers a helper from PATH or executes another image.
pub(super) fn receive(
    channel: CtlStream,
    deadline: Instant,
    mut authenticate: impl FnMut(transport::Peer) -> io::Result<()>,
) -> io::Result<Accepted> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: inspect only this held original endpoint. The shared protocol
    // uses per-call nonblocking I/O and never mutates inherited OFD flags.
    let flags = unsafe { libc::fcntl(channel.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || flags & libc::O_NONBLOCK == 0 {
        return Err(invalid());
    }
    trust_resource_controller::solver_bootstrap::receive(
        trust_resource_controller::solver_bootstrap::BootstrapChannel::new(channel)?,
        deadline,
        |peer, _offer| {
            authenticate(transport::Peer {
                pid: peer.pid,
                birth: peer.birth,
            })
        },
        |_offer| trust_exec_policy::ExecutionSeal::NoForkOrExec.install_current(),
    )
}

#[cfg(test)]
#[path = "solver_bootstrap/tests.rs"]
pub(super) mod tests;
