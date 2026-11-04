// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private ownership of an actual helper, its transport and all CPU backing.
//! Observations never manufacture a child or release capability. This does not
//! install a computer-wide allocator or confine escaped descendants.

use crate::{
    Claim,
    build_cpu::RetainedLease,
    cohort::launch::{CohortChild, LaunchError},
    epoch_endpoint::Endpoint,
    epoch_wire::{self as wire, Event},
    leadership::{Binding, Fence},
};
use aterm_uds::spawnfd::{self, Child, Command, NotSpawned, Spawned, Stdio};
use std::{
    fmt,
    io::{self, Read as _, Write as _},
    os::fd::{AsFd as _, AsRawFd as _},
    path::{Path, PathBuf},
    process::{ChildStdin, ChildStdout, ExitStatus},
};

fn bad(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}
fn waiting(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, why)
}

/// Explicit private helper inputs. Backing width comes from actual supplied capabilities.
#[derive(Clone, Debug)]
pub struct Config {
    /// Qualified genuine Targo executable selected by the execution owner.
    pub helper: PathBuf,
    /// Absent private transport-directory leaf beneath an owned parent.
    pub directory: PathBuf,
    /// One-use lowercase hexadecimal epoch identity, exactly 32 bytes.
    pub epoch: String,
    /// Number of separately backed implicit Targo root units.
    pub roots: u32,
}

/// Construction refusal retaining every actual input for explicit recovery.
#[derive(Debug)]
pub struct BindError {
    error: io::Error,
    inputs: Box<(Claim, Vec<RetainedLease>, Config)>,
}

impl BindError {
    /// Why construction refused before helper launch.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }
    /// Recover the original claim, every CPU capability and configuration.
    #[must_use]
    pub fn into_parts(self) -> (io::Error, Claim, Vec<RetainedLease>, Config) {
        let (claim, cpu, config) = *self.inputs;
        (self.error, claim, cpu, config)
    }
}
impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "private epoch binding refused: {}", self.error)
    }
}
impl std::error::Error for BindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Read-only progress. None of these values is release authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// The actual helper's Opened frame and original endpoint were validated.
    pub opened: bool,
    /// The original helper pipe supplied one matching retirement observation.
    pub retired: bool,
    /// The actual owned helper was reaped.
    pub helper_exited: bool,
    /// The original private output pipe reached EOF without trailing bytes.
    pub control_eof: bool,
    /// Published storage was durably removed, or unpublished storage was
    /// confirmed absent after genuine no-child cancellation.
    pub endpoint_cleaned: bool,
    /// Individual CPU settlements that returned success.
    pub settled_units: usize,
    /// Consumed CPU capabilities whose settlement returned an uncertain error.
    pub uncertain_units: usize,
    /// Actual CPU capabilities still owned and unattempted.
    pub remaining_units: usize,
    /// Requested roots whose actual exit observations were validated.
    pub completed_roots: usize,
    /// Completed roots whose actual exit observations reported failure.
    pub failed_roots: usize,
    /// A protocol or ownership failure prevents successful epoch completion.
    pub faulted: bool,
    /// The actual helper child remains owned after an unacknowledged BIND.
    pub unbound: bool,
    /// A genuine controlled no-child receipt remains owned here.
    pub not_spawned: bool,
}

/// One actual helper-reported root exit, observed through its private pipe.
/// This reports build results and supplies no resource-release authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootOutcome {
    /// The one-use root identifier requested in this epoch.
    pub id: u32,
    /// The actual root PID matched against its preceding birth observation.
    pub pid: u32,
    /// The native exit code, absent when the root ended without one.
    pub code: Option<i32>,
    /// Whether the actual root completed successfully.
    pub success: bool,
}

enum Helper {
    Bound(CohortChild),
    Unbound(Spawned),
    Rebound(Spawned),
}
impl Helper {
    fn child(&mut self) -> &mut Child {
        match self {
            Self::Bound(child) => child.child_mut(),
            Self::Unbound(child) | Self::Rebound(child) => child.child_mut(),
        }
    }
    fn bound(&self) -> bool {
        !matches!(self, Self::Unbound(_))
    }
}

#[derive(Default)]
struct Root {
    requested: bool,
    pid: Option<u32>,
    outcome: Option<RootOutcome>,
}

// No Clone, decoding, path constructor or public setter. This value is created
// only by this owning controller after reading its actual helper's private pipe.
struct Retired;

/// One original helper/cohort epoch and all its actual retained CPU units.
/// Drop closes local custody but never unlinks the endpoint or settles CPU debt.
pub struct EpochController {
    binding: Binding,
    claim: Claim,
    cpu: Vec<Option<RetainedLease>>,
    config: Config,
    endpoint: Endpoint,
    helper: Option<Helper>,
    not_spawned: Option<NotSpawned>,
    cancellation_started: bool,
    attempted: bool,
    input: Option<ChildStdin>,
    output: Option<ChildStdout>,
    incoming: Vec<u8>,
    outgoing: Vec<u8>,
    sent: u64,
    queued: u64,
    close_end: Option<u64>,
    closing: bool,
    closing_ack: bool,
    opened: bool,
    retired: Option<Retired>,
    eof: bool,
    exit: Option<ExitStatus>,
    roots: Vec<Root>,
    claim_finished: bool,
    confirmed: usize,
    uncertain: usize,
    faulted: bool,
}

impl fmt::Debug for EpochController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EpochController")
            .field("config", &self.config)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl EpochController {
    /// Bind original unused admission and N already durable CPU capabilities.
    /// No helper, root or reusable credit may already use these units elsewhere.
    /// # Errors
    /// Foreign/stale/adopted/used claim, invalid private configuration, or unsafe
    /// transport parent. The refusal preserves all original inputs.
    pub fn new(
        fence: &Fence,
        claim: Claim,
        cpu: Vec<RetainedLease>,
        config: Config,
    ) -> Result<Self, BindError> {
        let checked = (|| {
            let binding = fence.capture_binding(&claim)?;
            claim.check_retained_description()?;
            if cpu.is_empty()
                || cpu.len() > 256
                || config.roots == 0
                || config.roots > 64
                || config.roots as usize > cpu.len()
                || !config.helper.is_absolute()
                || config.epoch.len() != 32
                || !config
                    .epoch
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(bad("invalid private epoch configuration"));
            }
            let endpoint = Endpoint::prepare(&config.directory)?;
            Ok((binding, endpoint))
        })();
        match checked {
            Err(error) => Err(BindError {
                error,
                inputs: Box::new((claim, cpu, config)),
            }),
            Ok((binding, endpoint)) => {
                let roots = (0..config.roots).map(|_| Root::default()).collect();
                Ok(Self {
                    binding,
                    claim,
                    cpu: cpu.into_iter().map(Some).collect(),
                    config,
                    endpoint,
                    helper: None,
                    not_spawned: None,
                    cancellation_started: false,
                    attempted: false,
                    input: None,
                    output: None,
                    incoming: Vec::new(),
                    outgoing: Vec::new(),
                    sent: 0,
                    queued: 0,
                    close_end: None,
                    closing: false,
                    closing_ack: false,
                    opened: false,
                    retired: None,
                    eof: false,
                    exit: None,
                    roots,
                    claim_finished: false,
                    confirmed: 0,
                    uncertain: 0,
                    faulted: false,
                })
            }
        }
    }

    /// Start the configured genuine Targo helper with exclusively owned control pipes.
    /// # Errors
    /// Foreign/stale leadership, reused launch, or native launch/binding/pipe errors.
    /// Any real unbound child or no-child receipt remains owned by this object.
    pub fn launch(&mut self, fence: &Fence) -> io::Result<()> {
        let mut command = spawnfd::command(&self.config.helper);
        command
            .arg0("targo")
            .args(["--unverified", "__resource-epoch", "--directory"])
            .arg(self.endpoint.directory())
            .args([
                "--epoch",
                &self.config.epoch,
                "--backing",
                &self.cpu.len().to_string(),
                "--roots",
                &self.config.roots.to_string(),
            ]);
        self.launch_command(fence, &mut command)
    }

    fn launch_command(&mut self, fence: &Fence, command: &mut Command) -> io::Result<()> {
        self.binding.check(fence)?;
        if self.attempted {
            return Err(bad("private epoch helper launch is one-use"));
        }
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        command
            .env_remove("CARGO_MAKEFLAGS")
            .env_remove("MAKEFLAGS")
            .env_remove("MFLAGS");
        self.attempted = true;
        match self.claim.launch_cohort(command) {
            Ok(child) => {
                self.helper = Some(Helper::Bound(child));
                self.take_pipes()
            }
            Err(LaunchError::Unbound { spawned, error }) => {
                self.helper = Some(Helper::Unbound(*spawned));
                Err(error)
            }
            Err(LaunchError::NotSpawned(proof)) => {
                let error = io::Error::new(proof.error().kind(), proof.error().to_string());
                self.not_spawned = Some(*proof);
                Err(error)
            }
            Err(LaunchError::Refused(error)) => {
                self.faulted = true;
                Err(error)
            }
        }
    }

    fn take_pipes(&mut self) -> io::Result<()> {
        let child = self
            .helper
            .as_mut()
            .ok_or_else(|| bad("missing actual helper"))?
            .child();
        self.input = child.stdin.take();
        self.output = child.stdout.take();
        let set = (|| {
            let input = self
                .input
                .as_ref()
                .ok_or_else(|| bad("helper control input missing"))?;
            let output = self
                .output
                .as_ref()
                .ok_or_else(|| bad("helper control output missing"))?;
            for fd in [input.as_fd(), output.as_fd()] {
                aterm_uds::keep_out_of_children(&fd)?;
                // SAFETY: these descriptors are exclusively owned private pipe ends.
                let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
                if flags < 0
                    || unsafe {
                        libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                    } < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        })();
        if set.is_err() {
            self.fail_protocol();
        }
        set
    }

    /// Retry BIND using the actual child retained from this exact failed launch.
    /// # Errors
    /// Foreign leadership, no original unbound child, or journal/pipe failure.
    pub fn retry_binding(&mut self, fence: &Fence) -> io::Result<()> {
        self.binding.check(fence)?;
        let Some(Helper::Unbound(spawned)) = &mut self.helper else {
            return Err(bad("no actual unbound helper"));
        };
        self.claim.bind_cohort(spawned)?;
        let Some(Helper::Unbound(spawned)) = self.helper.take() else {
            unreachable!()
        };
        self.helper = Some(Helper::Rebound(spawned));
        self.take_pipes()
    }

    /// Cancel the exact controlled no-child attempt while retaining all CPU debt.
    /// A retry completes the same original receipt already transferred to Claim.
    /// # Errors
    /// Foreign leadership, missing/foreign receipt, or durable cancellation failure.
    pub fn cancel_not_spawned(&mut self, fence: &Fence) -> io::Result<()> {
        self.binding.check(fence)?;
        if self.cancellation_started {
            return self.claim.finish_cohort();
        }
        let proof = self
            .not_spawned
            .as_ref()
            .ok_or_else(|| bad("no actual no-child receipt"))?;
        self.claim.check_cohort_cancel(proof)?;
        // Claim retains this exact proof before transaction I/O. Record the
        // transfer first, so a failed durable cancellation remains retryable.
        self.cancellation_started = true;
        self.claim
            .cancel_cohort(self.not_spawned.take().expect("checked actual receipt"))
    }

    fn enqueue(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        if self.outgoing.len() + bytes.len() > wire::BUFFER {
            return Err(waiting("private epoch control queue is full"));
        }
        self.queued = self
            .queued
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| bad("private epoch control position exhausted"))?;
        self.outgoing.extend_from_slice(&bytes);
        Ok(())
    }

    /// Queue one root with a controller-selected stable target outside transport storage.
    /// # Errors
    /// Stale leadership, unopened/closing/faulted epoch, reused ID, invalid path or full queue.
    pub fn spawn_root(
        &mut self,
        fence: &Fence,
        id: u32,
        manifest: &Path,
        target: &Path,
    ) -> io::Result<()> {
        self.binding.check(fence)?;
        if !self.opened
            || self.closing
            || self.faulted
            || id as usize >= self.roots.len()
            || self.roots[id as usize].requested
        {
            return Err(bad("private epoch root is not available"));
        }
        self.enqueue(wire::spawn(&self.config.epoch, id, manifest, target)?)?;
        self.roots[id as usize].requested = true;
        Ok(())
    }

    /// Seal new local root requests and queue one Close command. This releases no unit.
    /// # Errors
    /// Stale leadership, unopened/faulted/already-closing epoch or a full queue.
    pub fn close(&mut self, fence: &Fence) -> io::Result<()> {
        self.binding.check(fence)?;
        if !self.opened || self.closing || self.faulted {
            return Err(bad("private epoch cannot close"));
        }
        self.enqueue(wire::close(&self.config.epoch)?)?;
        self.close_end = Some(self.queued);
        self.closing = true;
        Ok(())
    }

    fn receive(&mut self, event: Event) -> io::Result<()> {
        if self.retired.is_some() {
            return Err(bad("helper output followed final retirement"));
        }
        match event {
            Event::Opened {
                backing,
                roots,
                published,
                fifo,
            } => {
                if self.opened
                    || backing as usize != self.cpu.len()
                    || roots != self.config.roots
                    || published != backing - roots
                    || fifo != self.endpoint.fifo_path()
                {
                    return Err(bad("helper Opened does not match original epoch"));
                }
                self.endpoint.capture()?;
                self.opened = true;
            }
            Event::Spawned { id, pid } => {
                let root = self
                    .roots
                    .get_mut(id as usize)
                    .ok_or_else(|| bad("foreign helper root ID"))?;
                if !self.opened || !root.requested || root.pid.is_some() {
                    return Err(bad("unexpected helper root birth"));
                }
                root.pid = Some(pid);
            }
            Event::RootExit {
                id,
                pid,
                code,
                success,
            } => {
                let root = self
                    .roots
                    .get_mut(id as usize)
                    .ok_or_else(|| bad("foreign helper root exit"))?;
                if root.pid != Some(pid) || root.outcome.is_some() {
                    return Err(bad("helper root exit does not match birth"));
                }
                root.outcome = Some(RootOutcome {
                    id,
                    pid,
                    code,
                    success,
                });
            }
            Event::Closing => {
                if !self.opened
                    || !self.closing
                    || self.closing_ack
                    || !self.close_end.is_some_and(|end| self.sent >= end)
                {
                    return Err(bad("unexpected helper close acknowledgement"));
                }
                self.closing_ack = true;
            }
            Event::Retired { withdrawn, fifo } => {
                if !self.opened
                    || !self.closing_ack
                    || withdrawn as usize != self.cpu.len() - self.config.roots as usize
                    || fifo != self.endpoint.fifo_path()
                    || self
                        .roots
                        .iter()
                        .any(|root| root.requested && root.outcome.is_none())
                {
                    return Err(bad("helper retirement does not match original epoch"));
                }
                self.endpoint.validate()?;
                self.retired = Some(Retired);
            }
        }
        Ok(())
    }

    fn progress(&mut self) -> io::Result<()> {
        if self.helper.as_ref().is_none_or(|helper| !helper.bound()) {
            return Err(bad("private epoch helper is not durably bound"));
        }
        if !self.outgoing.is_empty() {
            let input = self
                .input
                .as_mut()
                .ok_or_else(|| bad("helper input missing"))?;
            match input.write(&self.outgoing) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => {
                    self.outgoing.drain(..count);
                    self.sent += count as u64;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        // Complete already buffered frames before another read, especially EOF:
        // a bounded sixteen-frame pass may leave a valid complete burst queued.
        if !self.eof && !self.incoming.contains(&b'\n') {
            let output = self
                .output
                .as_mut()
                .ok_or_else(|| bad("helper output missing"))?;
            let mut bytes = [0; 4096];
            match output.read(&mut bytes) {
                Ok(0) => {
                    if !self.incoming.is_empty() || self.retired.is_none() {
                        return Err(bad("helper ended without complete retirement"));
                    }
                    self.eof = true;
                }
                Ok(count) => {
                    if self.incoming.len() + count > wire::BUFFER {
                        return Err(bad("helper output buffer exceeded bound"));
                    }
                    self.incoming.extend_from_slice(&bytes[..count]);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        // At most sixteen frames per call; unread bytes and actual pipes remain owned.
        for _ in 0..16 {
            let Some(end) = self.incoming.iter().position(|b| *b == b'\n') else {
                break;
            };
            if end == 0 || end > wire::FRAME {
                return Err(bad("helper frame exceeded bound"));
            }
            let frame: Vec<_> = self.incoming.drain(..=end).collect();
            self.receive(wire::decode(&frame[..end], &self.config.epoch)?)?;
        }
        if self.incoming.len() > wire::FRAME && !self.incoming.contains(&b'\n') {
            return Err(bad("unterminated helper frame exceeded bound"));
        }
        self.observe_exit()?;
        Ok(())
    }

    fn observe_exit(&mut self) -> io::Result<()> {
        if self.exit.is_none()
            && let Some(helper) = &mut self.helper
        {
            self.exit = helper.child().try_wait()?;
        }
        Ok(())
    }

    fn fail_protocol(&mut self) {
        self.faulted = true;
        // No further births can be requested through our pipe. Dropping the
        // failed output end also prevents an unwatched writer from blocking on
        // control output. The actual child and every CPU unit remain owned;
        // later polls still perform nonblocking reap observation.
        drop(self.input.take());
        drop(self.output.take());
        self.outgoing.clear();
    }

    /// Advance bounded nonblocking pipe I/O and the actual helper wait. No CPU is released.
    /// # Errors
    /// Stale leadership, malformed/foreign/trailing protocol, pipe or native wait failure.
    /// The original helper, claim, node and every unattempted CPU capability remain owned.
    pub fn poll(&mut self, fence: &Fence) -> io::Result<Status> {
        self.binding.check(fence)?;
        if self.faulted {
            self.observe_exit()?;
            return Err(bad("private epoch is faulted"));
        }
        // Before launch, genuine no-child refusal and unacknowledged BIND are
        // recoverable custody states, never malformed helper observations.
        if self.helper.as_ref().is_none_or(|helper| !helper.bound()) {
            return Ok(self.status());
        }
        if let Err(error) = self.progress() {
            self.fail_protocol();
            self.observe_exit()?;
            return Err(error);
        }
        Ok(self.status())
    }

    /// Complete original group custody, durably unlink the original endpoint, then
    /// settle at most one CPU unit. Repeated calls progress unattempted units.
    /// A genuine no-child attempt instead retries durable cancellation and
    /// confirms that its unpublished endpoint remains absent.
    /// # Errors
    /// Missing retirement/EOF/helper exit or live/unknown group returns WouldBlock.
    /// Journal/cleanup failures retain every unattempted capability. A CPU error
    /// consumes only that unit and remains uncertain forever; it cannot become
    /// an all-N success, while later calls may settle other actual owned units.
    pub fn try_finish(&mut self, fence: &Fence) -> io::Result<()> {
        self.binding.check(fence)?;
        if self.faulted {
            return Err(bad("private epoch is faulted"));
        }
        if !self.cancellation_started {
            if self.retired.is_none() || !self.eof || self.exit.is_none() {
                return Err(waiting(
                    "private epoch retirement and actual helper exit are incomplete",
                ));
            }
            if !self.exit.is_some_and(|status| status.success()) {
                return Err(bad("retired helper did not exit successfully"));
            }
        }
        if !self.claim_finished {
            self.claim.finish_cohort()?;
            self.claim_finished = true;
        }
        if self.cancellation_started {
            self.endpoint.finish_unlaunched()?;
        } else {
            self.endpoint.cleanup()?;
        }
        if let Some(capability) = self.cpu.iter_mut().find(|capability| capability.is_some()) {
            let capability = capability.take().expect("actual owned CPU unit");
            match capability.settle() {
                Ok(()) => self.confirmed += 1,
                Err(error) => {
                    self.uncertain += 1;
                    return Err(error);
                }
            }
        }
        if self.uncertain != 0 {
            return Err(bad("one or more CPU settlement outcomes remain uncertain"));
        }
        if self.cpu.iter().any(Option::is_some) {
            return Err(waiting("more actual CPU units await settlement"));
        }
        Ok(())
    }

    /// Read-only progress for the owner or its observation projection.
    #[must_use]
    pub fn status(&self) -> Status {
        Status {
            opened: self.opened,
            retired: self.retired.is_some(),
            helper_exited: self.exit.is_some(),
            control_eof: self.eof,
            endpoint_cleaned: self.endpoint.is_cleaned(),
            settled_units: self.confirmed,
            uncertain_units: self.uncertain,
            remaining_units: self.cpu.iter().filter(|unit| unit.is_some()).count(),
            completed_roots: self
                .roots
                .iter()
                .filter(|root| root.outcome.is_some())
                .count(),
            failed_roots: self
                .roots
                .iter()
                .filter(|root| root.outcome.is_some_and(|outcome| !outcome.success))
                .count(),
            faulted: self.faulted,
            unbound: matches!(self.helper, Some(Helper::Unbound(_))),
            not_spawned: self.not_spawned.is_some(),
        }
    }
    /// Observe one validated root exit. Absence means it has not been observed.
    /// Build success and CPU settlement are independent outcomes.
    #[must_use]
    pub fn root_outcome(&self, id: u32) -> Option<RootOutcome> {
        self.roots.get(id as usize).and_then(|root| root.outcome)
    }
    /// True only after every original CPU settlement returned success.
    /// This describes CPU custody alone; inspect root outcomes for build results.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.endpoint.is_cleaned() && self.confirmed == self.cpu.len() && self.uncertain == 0
    }
    /// Original helper PID for diagnostics only, never a release or signal capability.
    #[must_use]
    pub fn helper_id(&mut self) -> Option<u32> {
        self.helper.as_mut().map(|helper| helper.child().id())
    }

    #[cfg(test)]
    pub(crate) fn launch_for_test(
        &mut self,
        fence: &Fence,
        command: &mut Command,
    ) -> io::Result<()> {
        self.launch_command(fence, command)
    }
    #[cfg(test)]
    pub(crate) fn launch_member_for_test(
        &mut self,
        fence: &Fence,
        command: &mut Command,
    ) -> io::Result<Child> {
        self.binding.check(fence)?;
        let Some(Helper::Bound(leader)) = &mut self.helper else {
            return Err(bad("fixture requires original bound helper"));
        };
        self.claim.launch_cohort_member(leader, command)
    }
}
