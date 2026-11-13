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
    epoch_recipe::{self, CapturedLogs, RootOutput, RootRecipe},
    epoch_target::{TargetLease, TargetLoan},
    epoch_wire::{self as wire, Event},
    leadership::{Binding, Fence},
};
use aterm_uds::ready::{Interest, Watch};
use aterm_uds::spawnfd::{self, Child, Command, NotSpawned, SpawnFailure, Spawned, Stdio};
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
    /// The original no-child receipt was transferred to Claim for durable cancellation.
    pub cancellation_started: bool,
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

/// Borrowed readiness hints for the execution owner's existing native wait set.
/// These do not consume protocol bytes, observe child exit or release resources.
/// Register the owner's child-state watch before launch, poll before waiting,
/// and recheck actual child state after a hint. A helper may exit after its output
/// reaches EOF; the output descriptor is deliberately omitted once EOF is read.
#[derive(Clone, Copy, Debug)]
pub struct Readiness<'a> {
    // Opaque handles: this projection exposes neither protocol reader nor writer.
    interests: [Option<Watch<'a>>; 2],
    /// Buffered frames or already-cleaned remaining CPU units need another turn
    /// without sleeping. This is bounded work, never a completion certificate.
    pub immediate: bool,
    /// Settlement can be attempted, but group drain or durable cleanup may still
    /// refuse. Pace retries alongside native readiness: an orphan's exit need
    /// not notify this owner. Stop retrying once no actual capability remains.
    pub retry_finish: bool,
}

impl<'a> Readiness<'a> {
    /// At most two interests, suitable for the owner's shared native wait set.
    pub fn interests(self) -> impl Iterator<Item = Watch<'a>> {
        self.interests.into_iter().flatten()
    }
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
    cancellation: Option<RootCancellation>,
    cancel_end: Option<u64>,
    manifest: Option<PathBuf>,
    target: Option<PathBuf>,
    logs: Option<CapturedLogs>,
    capture_error: Option<String>,
    target_lease: Option<TargetLoan>,
}

/// Local intent and actual driver acknowledgement, separate from exit outcome.
/// None of these states authorizes releasing CPU or descendant custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootCancellation {
    /// A stop request is queued or in flight; the driver has not acknowledged it.
    Requested,
    /// The actual driver acknowledged its sticky stop-dispatch gate.
    Stopped,
    /// Actual root exit was observed; existing work cannot be stopped retroactively.
    AlreadyExited,
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
    recipes: Vec<RootRecipe>,
    encoded_recipes: Vec<String>,
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
    finalize_end: Option<u64>,
    finalize_nonce: Option<String>,
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
        Self::new_with_recipes(fence, claim, cpu, config, Vec::new())
    }

    /// Bind original admission with bounded genuine-Targo recipes prepared before dispatch.
    /// Refusals preserve original resource custody; recipe descriptions are plain cloneable data.
    /// # Errors
    /// The same refusals as [`Self::new`], or invalid/oversized recipe descriptions.
    pub fn new_with_recipes(
        fence: &Fence,
        claim: Claim,
        cpu: Vec<RetainedLease>,
        config: Config,
        recipes: Vec<RootRecipe>,
    ) -> Result<Self, BindError> {
        let checked = (|| {
            let encoded = epoch_recipe::encoded(&recipes, config.roots)?;
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
            Ok((binding, endpoint, encoded))
        })();
        match checked {
            Err(error) => Err(BindError {
                error,
                inputs: Box::new((claim, cpu, config)),
            }),
            Ok((binding, endpoint, encoded_recipes)) => {
                let roots = (0..config.roots).map(|_| Root::default()).collect();
                Ok(Self {
                    binding,
                    claim,
                    cpu: cpu.into_iter().map(Some).collect(),
                    config,
                    recipes,
                    encoded_recipes,
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
                    finalize_end: None,
                    finalize_nonce: None,
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

    /// Start and durably bind the configured genuine Targo helper.
    /// # Errors
    /// Foreign/stale leadership, reused launch, or native launch/binding/pipe errors.
    /// Any real unbound child or no-child receipt remains owned by this object.
    pub fn launch(&mut self, fence: &Fence) -> io::Result<()> {
        let mut command = self.helper_command();
        self.launch_command(fence, &mut command)
    }

    /// Durably arm and dispatch the configured genuine helper, retaining its
    /// actual child before BIND. Success acknowledges owned birth only. The
    /// owner must call [`Self::retry_binding`] before protocol or root issuance.
    /// Control pipes stay private and unread until durable binding succeeds.
    /// # Errors
    /// Foreign/stale leadership, reused birth, or native ARM/spawn failure.
    /// Genuine no-child receipts remain owned; unknown refusal retains all debt.
    pub fn dispatch(&mut self, fence: &Fence) -> io::Result<()> {
        let mut command = self.helper_command();
        self.dispatch_command(fence, &mut command)
    }

    fn helper_command(&self) -> Command {
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
        for recipe in &self.encoded_recipes {
            command.arg("--root-recipe").arg(recipe);
        }
        command
    }

    fn prepare_launch(&mut self, fence: &Fence, command: &mut Command) -> io::Result<()> {
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
        Ok(())
    }

    fn dispatch_command(&mut self, fence: &Fence, command: &mut Command) -> io::Result<()> {
        self.prepare_launch(fence, command)?;
        let attempt = self
            .claim
            .arm_cohort()
            .map_err(|error| self.retain_launch_error(LaunchError::Refused(error)))?;
        match command.spawn_cohort(attempt) {
            Ok(spawned) => {
                // Preserve the exact controlled birth, without constructing a
                // replacement child from a PID or reading its private output.
                self.helper = Some(Helper::Unbound(spawned));
                Ok(())
            }
            Err(SpawnFailure::NotSpawned(proof)) => {
                Err(self.retain_launch_error(LaunchError::NotSpawned(proof)))
            }
            Err(SpawnFailure::Refused(error)) => {
                Err(self.retain_launch_error(LaunchError::Refused(error)))
            }
            Err(SpawnFailure::Unbound { spawned, error }) => {
                Err(self.retain_launch_error(LaunchError::Unbound { spawned, error }))
            }
        }
    }

    fn launch_command(&mut self, fence: &Fence, command: &mut Command) -> io::Result<()> {
        self.prepare_launch(fence, command)?;
        match self.claim.launch_cohort(command) {
            Ok(child) => {
                self.helper = Some(Helper::Bound(child));
                self.take_pipes()
            }
            Err(error) => Err(self.retain_launch_error(error)),
        }
    }

    fn retain_launch_error(&mut self, error: LaunchError) -> io::Error {
        match error {
            LaunchError::Unbound { spawned, error } => {
                self.helper = Some(Helper::Unbound(*spawned));
                error
            }
            LaunchError::NotSpawned(proof) => {
                let error = io::Error::new(proof.error().kind(), proof.error().to_string());
                self.not_spawned = Some(*proof);
                error
            }
            LaunchError::Refused(error) => {
                self.faulted = true;
                error
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

    /// Bind the actual child retained from dispatch or retry its failed BIND.
    /// Actual reap preserves the original sealed Spawned receipt; it never
    /// supplies no-child cancellation or CPU-release authority.
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
        let target = crate::epoch_target::resolve_legacy_target(target)?;
        let mut lease = TargetLease::acquire(&target)?;
        self.spawn_root_with_target(fence, id, manifest, &target, lease.loan()?)
    }

    /// Queue one root using the caller's original target exclusion loan.
    /// The loan remains owned through settled output or whole-cohort retirement.
    /// # Errors
    /// The same refusals as [`Self::spawn_root`], or foreign/replaced target custody.
    pub fn spawn_root_with_target(
        &mut self,
        fence: &Fence,
        id: u32,
        manifest: &Path,
        target: &Path,
        target_lease: TargetLoan,
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
        if self.recipes.iter().any(|recipe| recipe.id == id)
            && self.roots[id as usize].logs.is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                self.roots[id as usize]
                    .capture_error
                    .as_deref()
                    .unwrap_or("original recipe logs were not captured"),
            ));
        }
        target_lease.validate(target)?;
        self.enqueue(wire::spawn(&self.config.epoch, id, manifest, target)?)?;
        self.roots[id as usize].requested = true;
        self.roots[id as usize].manifest = Some(manifest.to_owned());
        self.roots[id as usize].target = Some(target.to_owned());
        self.roots[id as usize].target_lease = Some(target_lease);
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

    /// Request cooperative stop of one original root without terminating peers.
    /// Active work still drains; IDs and targets remain reserved. Repeated calls
    /// return existing intent. An observed exit records intent locally even when
    /// finalization is queued; its actual code/success remains unchanged.
    /// # Errors
    /// Stale leadership, unopened/faulted epoch, unknown root or full control queue.
    pub fn cancel_root(&mut self, fence: &Fence, id: u32) -> io::Result<RootCancellation> {
        self.binding.check(fence)?;
        let root = self
            .roots
            .get(id as usize)
            .ok_or_else(|| bad("unknown cancellation root"))?;
        if !self.opened || self.faulted || !root.requested {
            return Err(bad("private root cannot be cancelled"));
        }
        if let Some(state) = root.cancellation {
            return Ok(state);
        }
        if root.outcome.is_some() {
            self.roots[id as usize].cancellation = Some(RootCancellation::AlreadyExited);
            return Ok(RootCancellation::AlreadyExited);
        }
        if self.finalize_end.is_some() || self.retired.is_some() {
            return Err(bad("private root control is finalized"));
        }
        self.enqueue(wire::cancel(&self.config.epoch, id)?)?;
        let root = &mut self.roots[id as usize];
        root.cancel_end = Some(self.queued);
        root.cancellation = Some(RootCancellation::Requested);
        Ok(RootCancellation::Requested)
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
                // Recipes are fixed at startup and the genuine helper creates
                // their original log inodes before Opened. No root request is
                // accepted until this pinning pass has completed.
                for recipe in &self.recipes {
                    let root = &mut self.roots[recipe.id as usize];
                    match self.endpoint.capture_logs(recipe.id) {
                        Ok(logs) => root.logs = Some(logs),
                        Err(error) => root.capture_error = Some(error.to_string()),
                    }
                }
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
            Event::Cancelled { id, pid, stopped } => {
                let root = self
                    .roots
                    .get_mut(id as usize)
                    .ok_or_else(|| bad("foreign cancellation root"))?;
                if root.pid != Some(pid)
                    || root.cancellation != Some(RootCancellation::Requested)
                    || !root.cancel_end.is_some_and(|end| self.sent >= end)
                    || (!stopped && root.outcome.is_none())
                {
                    return Err(bad(
                        "cancellation acknowledgement lacks original request or exit",
                    ));
                }
                root.cancellation = Some(if stopped {
                    RootCancellation::Stopped
                } else {
                    RootCancellation::AlreadyExited
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
            Event::Retired {
                withdrawn,
                fifo,
                finalization,
            } => {
                if !self.opened
                    || !self.closing_ack
                    || !self.finalize_end.is_some_and(|end| self.sent >= end)
                    || self.finalize_nonce.as_deref() != Some(finalization.as_str())
                    || withdrawn as usize != self.cpu.len() - self.config.roots as usize
                    || fifo != self.endpoint.fifo_path()
                    || self
                        .roots
                        .iter()
                        .any(|root| root.requested && root.outcome.is_none())
                    || self
                        .roots
                        .iter()
                        .any(|root| root.cancellation == Some(RootCancellation::Requested))
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
            let event = wire::decode(&self.incoming[..end], &self.config.epoch);
            self.incoming.drain(..=end);
            self.receive(event?)?;
        }
        if self.incoming.len() > wire::FRAME && !self.incoming.contains(&b'\n') {
            return Err(bad("unterminated helper frame exceeded bound"));
        }
        // Close seals new births; only the execution owner can finish the
        // control phase after observing every requested root's actual exit.
        // Keep this after bounded frame decoding so an early Retired cannot
        // cause its own missing finalization request to be manufactured.
        if self.closing_ack
            && self.finalize_end.is_none()
            && self.outgoing.is_empty()
            && self
                .roots
                .iter()
                .all(|root| !root.requested || root.outcome.is_some())
            && self
                .roots
                .iter()
                .all(|root| root.cancellation != Some(RootCancellation::Requested))
        {
            let nonce = aterm_uds::rand::hex_token::<16>()?;
            self.enqueue(wire::finalize(&self.config.epoch, &nonce)?)?;
            self.finalize_nonce = Some(nonce);
            self.finalize_end = Some(self.queued);
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
        if let Err(error) = self.binding.check(fence) {
            // Leadership refusal revokes new protocol/settlement actions, not
            // custody of the actual already-owned child. Reap observation alone
            // consumes no commands, endpoint cleanup or CPU capability.
            self.observe_exit()?;
            return Err(error);
        }
        if self.faulted {
            self.observe_exit()?;
            return Err(bad("private epoch is faulted"));
        }
        // Before launch, genuine no-child refusal and unacknowledged BIND are
        // recoverable custody states, never malformed helper observations.
        if self.helper.as_ref().is_none_or(|helper| !helper.bound()) {
            self.observe_exit()?;
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
            cancellation_started: self.cancellation_started,
        }
    }
    /// Project current wait interests without consuming or duplicating custody.
    /// No timer is required while only native I/O or the actual helper is pending.
    /// An unbound launch or no-child receipt needs the owner's explicit recovery
    /// action rather than an always-ready descriptor or an automatic retry loop.
    #[must_use]
    pub fn readiness(&self) -> Readiness<'_> {
        let active = !self.faulted && self.helper.as_ref().is_some_and(Helper::bound);
        let remaining = self.cpu.iter().any(Option::is_some);
        let finish = !self.faulted
            && remaining
            && (self.cancellation_started
                || (self.retired.is_some()
                    && self.eof
                    && self.exit.is_some_and(|status| status.success())));
        Readiness {
            interests: [
                self.output
                    .as_ref()
                    .filter(|_| active && !self.eof)
                    .map(|output| Watch::new(output.as_fd(), Interest::Read)),
                self.input
                    .as_ref()
                    .filter(|_| active && !self.outgoing.is_empty())
                    .map(|input| Watch::new(input.as_fd(), Interest::Write)),
            ],
            immediate: !self.faulted
                && (self.incoming.contains(&b'\n')
                    || (finish && self.claim_finished && self.endpoint.is_cleaned())),
            retry_finish: finish,
        }
    }
    /// Observe one validated root exit. Absence means it has not been observed.
    /// Build success and CPU settlement are independent outcomes.
    #[must_use]
    pub fn root_outcome(&self, id: u32) -> Option<RootOutcome> {
        self.roots.get(id as usize).and_then(|root| root.outcome)
    }
    /// Observe cancellation intent/acknowledgement independently from actual exit.
    #[must_use]
    pub fn root_cancellation(&self, id: u32) -> Option<RootCancellation> {
        self.roots
            .get(id as usize)
            .and_then(|root| root.cancellation)
    }

    /// Number of configured output reservations, including roots not yet spawned.
    #[must_use]
    pub fn captured_root_count(&self) -> usize {
        self.recipes.len()
    }

    /// Move complete original log custody to its explicit consumer, once.
    /// A failed or cancelled root still has diagnostic output; callers must
    /// separately refuse artifact acceptance for those outcomes.
    /// # Errors
    /// Original cohort closure, endpoint cleanup or every CPU settlement is incomplete.
    pub fn take_root_output(&mut self, id: u32) -> io::Result<Option<RootOutput>> {
        if !self.is_settled() || self.faulted || !self.claim_finished {
            return Err(waiting("original epoch is not fully settled"));
        }
        let Some(recipe) = self.recipes.iter().find(|recipe| recipe.id == id).cloned() else {
            return Ok(None);
        };
        let root = &mut self.roots[id as usize];
        let Some(outcome) = root.outcome else {
            return Ok(None);
        };
        let Some(logs) = root.logs.take() else {
            return Ok(None);
        };
        Ok(Some(RootOutput {
            logs,
            _target_lease: root.target_lease.take().expect("original root target loan"),
            recipe,
            manifest: root
                .manifest
                .clone()
                .expect("actual requested root manifest"),
            target: root.target.clone().expect("actual requested root target"),
            epoch: self.config.epoch.clone(),
            outcome,
            cancellation: root.cancellation,
        }))
    }
    /// Root-level acceptance hint. Live activation also needs the current source
    /// generation's independent acceptance fence; this is no publication capability.
    #[must_use]
    pub fn root_eligible_for_activation(&self, id: u32) -> bool {
        self.roots.get(id as usize).is_some_and(|root| {
            root.cancellation.is_none() && root.outcome.is_some_and(|outcome| outcome.success)
        })
    }
    /// Observe a validated root birth before its outcome is available.
    /// This diagnostic PID supplies no process-control or release authority.
    #[must_use]
    pub fn root_pid(&self, id: u32) -> Option<u32> {
        self.roots.get(id as usize).and_then(|root| root.pid)
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
    pub(crate) fn dispatch_for_test(
        &mut self,
        fence: &Fence,
        command: &mut Command,
    ) -> io::Result<()> {
        self.dispatch_command(fence, command)
    }
    #[cfg(test)]
    pub(crate) fn output_bytes_for_test(&self) -> io::Result<usize> {
        let output = self
            .output
            .as_ref()
            .ok_or_else(|| bad("missing fixture output"))?;
        let mut count: libc::c_int = 0;
        // SAFETY: the original live output pipe and initialized integer output
        // remain owned here. FIONREAD consumes neither bytes nor child state.
        if unsafe { libc::ioctl(output.as_raw_fd(), libc::FIONREAD, &raw mut count) } < 0 {
            return Err(io::Error::last_os_error());
        }
        usize::try_from(count).map_err(|_| bad("negative fixture output availability"))
    }
    #[cfg(test)]
    pub(crate) fn finalization_sent_for_test(&self) -> bool {
        self.finalize_end.is_some_and(|end| self.sent >= end)
    }
    #[cfg(test)]
    pub(crate) fn finalization_nonce_for_test(&self) -> Option<&str> {
        self.finalization_sent_for_test()
            .then_some(self.finalize_nonce.as_deref())
            .flatten()
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
