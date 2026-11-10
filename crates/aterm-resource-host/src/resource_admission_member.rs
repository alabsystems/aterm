// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One host-owned member borrows the original job's resources. Its original
//! child and descriptor capabilities survive errors until durable scope retirement.
use super::job::Events;
use super::member_custody::{ProducerInputs, ProducerRetirement, ReturnedProducer};
use super::start::clocks;
use aterm_capacity::cohort::launch::{CohortChild, CohortSignal, LaunchError};
use aterm_capacity::retained_cohort::CancelError;
use aterm_ctl::resource_admission_transport::Peer;
use aterm_types::resource_admission::{Binding, CompletionCause, Event, Outcome};
use aterm_uds::{exitwatch::ExitMoment, process, spawnfd};
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::time::{Duration, Instant};

#[path = "resource_admission_member/sampling_decision.rs"]
mod sampling_decision;

/// Native lineage is evidence of scope; time spent reading it is only a hang detector.
pub(super) fn scope_covers(peer: Peer, pid: u32, birth: u64) -> bool {
    scope_covers_at(peer, pid, birth, Instant::now())
}

fn scope_covers_at(peer: Peer, pid: u32, birth: u64, started: Instant) -> bool {
    let deadline = started + Duration::from_secs(60);
    process::stable_descendant(
        peer.pid,
        process::Birth(peer.birth),
        pid,
        process::Birth(birth),
        deadline,
    )
}

/// Borrowed original claim/fence/root access, never a second memory claim.
pub(super) trait MemberCustody {
    fn before_arm(&mut self) -> io::Result<()>;
    fn before_go(&mut self) -> io::Result<()>;
    /// A protocol-gated target must durably bind its actual Started metadata
    /// before offering Run. Ordinary native-resume targets keep their existing path.
    fn commit_started(
        &mut self,
        _: &Binding,
        _: &Started,
        _: Option<RequestedStartLimits>,
    ) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn launch(&mut self, command: &mut spawnfd::Command) -> Result<CohortChild, LaunchError>;
    fn bind(&mut self, child: spawnfd::Spawned) -> Result<CohortChild, LaunchError>;
    fn cancel_no_child(&mut self, proof: spawnfd::NotSpawned) -> Result<(), CancelError>;
    fn retire(&mut self, child: &CohortChild) -> io::Result<()>;
    fn bind_producer(&mut self, _: &mut ProducerInputs) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// General group retirement cannot certify escaped descendants. Only the
    /// original custody implementation may compose enforced containment with
    /// its actual native retirement into the stronger one-use result.
    fn retire_producer(
        &mut self,
        _: &mut CohortChild,
        _: &mut Option<ProducerInputs>,
    ) -> io::Result<ProducerRetirement> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn return_producer(
        &mut self,
        original: Box<ProducerRetirement>,
    ) -> Result<ProducerReturn, (io::Error, Box<ProducerRetirement>)> {
        Err((io::ErrorKind::Unsupported.into(), original))
    }
    /// Called only after lexical NoLaunch, authenticated NoChild cancellation,
    /// or successful actual scope retirement. Retry bookkeeping without
    /// repeating or reconstructing the consumed native closure proof.
    fn return_unit(&mut self, receipt: ReturnReceipt) -> io::Result<ReturnProgress>;
    fn validate(&mut self) -> io::Result<()>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReturnProgress {
    Pending,
    Returned,
}
pub(super) enum ProducerReturn {
    Pending(Box<ProducerRetirement>),
    Returned(Box<ReturnedProducer>),
}
/// Constructed only at this owned attempt's lexical no-ARM boundary or after
/// its native cancellation/retirement receipt. No wire value creates one.
pub(super) enum ReturnReceipt {
    NoArm,
    ScopeClosed,
}
/// The actual prepared CPU/GNU descriptors and one-use handoff binding.
pub(super) trait MemberResources {
    fn configure(&mut self, command: &mut spawnfd::Command) -> io::Result<()>;
    fn bind_child(&mut self, pid: u32) -> io::Result<()>;
    /// A separately negotiated produced profile reports its actual selected
    /// suspended identity only after both image holds, before this owner's GO.
    fn publish_prepared(&mut self, _: (u32, u64), _: &mut Events) -> io::Result<()> {
        Ok(())
    }
    fn finish_inputs(&mut self) -> Option<Result<(), String>> {
        None
    }
    /// A trusted same-process prelude may prepare a second stopped-at-exec
    /// image. False is preparation only, never target Started or a new grant.
    fn prepare_target(&mut self, _: &mut spawnfd::Child) -> io::Result<bool> {
        Ok(true)
    }
    /// Poll the original target's execution gate. A Pending result retains
    /// exactly this child/resources; it is not a request to repeat preparation,
    /// journal commitment, native loan binding or launch under another attempt.
    fn poll_start(
        &mut self,
        _: &mut spawnfd::Child,
        _: &mut dyn FnMut() -> bool,
        _: &mut dyn FnMut(&Started, Option<RequestedStartLimits>) -> io::Result<()>,
    ) -> io::Result<TargetStart> {
        Ok(TargetStart::Resume)
    }
    fn take_producer_inputs(&mut self) -> io::Result<Option<ProducerInputs>> {
        Ok(None)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Started {
    pub pid: u32,
    pub birth: u64,
    pub monotonic_ns: u64,
    pub unix_ns: u64,
}
/// The original private offer's requested policy, not an installed-limit receipt
/// or CPU/memory ownership. A production profile must bind retained resources first.
#[derive(Clone, Copy)]
pub(super) struct RequestedStartLimits {
    pub units: u32,
    pub footprint_mib: u32,
}
pub(super) enum TargetStart {
    /// The original native child remains held before its first execution.
    Resume,
    /// A private execution gate has not completed. `offered` is a fact about
    /// delivery of this SAME Run, never permission to replay it elsewhere.
    #[cfg(all(test, target_os = "macos"))]
    Pending { offered: bool },
    /// The protocol dispatched exactly the metadata previously durably committed.
    #[cfg(all(test, target_os = "macos"))]
    Started { metadata: Started, since: Instant },
}
#[derive(Clone, Debug)]
pub(super) struct Completed {
    pub outcome: Outcome,
    pub cause: CompletionCause,
    pub runtime_ns: u64,
    pub accounting: aterm_types::resource_admission::AccountingObservation,
}
pub(super) struct Progress {
    pub go_attempted: bool,
    pub launched: bool,
    pub ready_to_start: bool,
    pub completed: Option<Completed>,
    pub retired: bool,
    pub uncertain: bool,
    pub drained: bool,
}
enum State {
    NotAttempted,
    Unknown,
    NoChild(Option<Box<spawnfd::NotSpawned>>),
    Unbound(Option<Box<spawnfd::Spawned>>),
    Bound(Box<Bound>),
}
struct Bound {
    child: CohortChild,
    pid: u32,
    birth: u64,
    exit: Option<ExitMoment<Instant>>,
    ended: Option<Instant>,
    started: Option<Instant>,
    go_attempted: bool,
    dispatching: bool,
    committed_start: Option<Started>,
    launched: bool,
    reaped: bool,
    cleanup_at: Option<Instant>,
    cause: CompletionCause,
    completed: Option<Completed>,
    producer: Option<ProducerInputs>,
}
impl Bound {
    fn new(mut child: CohortChild, wake: fn()) -> Self {
        let pid = child.child_mut().id();
        let birth = i32::try_from(pid)
            .ok()
            .and_then(process::read_birth)
            .map_or(0, |v| v.0);
        Self {
            child,
            pid,
            birth,
            exit: ExitMoment::watch_then(pid, Instant::now, wake),
            ended: None,
            started: None,
            go_attempted: false,
            dispatching: false,
            committed_start: None,
            launched: false,
            reaped: false,
            cleanup_at: None,
            cause: CompletionCause::Exited,
            completed: None,
            producer: None,
        }
    }
    fn stop(&mut self, cause: CompletionCause) {
        if self.cleanup_at.is_none() || cause_priority(cause) > cause_priority(self.cause) {
            self.cause = cause;
            self.cleanup_at.get_or_insert_with(Instant::now);
            // ResourceLimit and any future non-natural policy cause kill
            // immediately. Cancellation/natural exit allow a short cleanup grace.
            let signal = if !self.go_attempted {
                CohortSignal::Kill
            } else if matches!(cause, CompletionCause::Exited | CompletionCause::Cancelled) {
                CohortSignal::Terminate
            } else {
                CohortSignal::Kill
            };
            let _ = self.child.request_group(signal);
        }
    }
}

/// The actor must keep this value until retired and its events drained. No
/// decoded PID, child exit, failed launch, or dropped resource proves closure.
#[must_use]
pub(super) struct OwnedAttempt<D> {
    binding: Binding,
    state: State,
    resources: Option<D>,
    events: Events,
    timeout: Option<Duration>,
    // Configuration evidence only. A profile that moves its selected command
    // into a prelude must supply an actual policy witness before using Run;
    // an absent value here cannot authorize a requested footprint.
    configured_footprint_mib: Option<u32>,
    wake: fn(),
    retired: bool,
    scope_closed: bool,
    uncertain: bool,
    failure: Option<String>,
    producer_retirement: Option<Box<ProducerRetirement>>,
    producer_returned: Option<Box<ReturnedProducer>>,
}
impl<D: MemberResources> OwnedAttempt<D> {
    /// The owner has durably journaled this member and emitted Queued. ARM/BIND
    /// are supplied by its original custody adapter. No target can GO here.
    pub fn launch(
        custody: &mut impl MemberCustody,
        mut command: spawnfd::Command,
        mut resources: D,
        events: Events,
        binding: Binding,
        timeout: Option<Duration>,
        wake: fn(),
    ) -> Self {
        let prepared = resources.configure(&mut command);
        #[cfg(target_os = "macos")]
        let configured_footprint_mib = command.get_footprint_limit().map(|limit| limit.mib());
        #[cfg(not(target_os = "macos"))]
        let configured_footprint_mib = None;
        command.start_suspended(true);
        let mut this = Self {
            binding,
            state: State::Unknown,
            resources: Some(resources),
            events,
            timeout,
            configured_footprint_mib,
            wake,
            retired: false,
            scope_closed: false,
            uncertain: false,
            failure: None,
            producer_retirement: None,
            producer_returned: None,
        };
        if prepared.is_err()
            || timeout == Some(Duration::ZERO)
            || this.events.cancelled()
            || custody.before_arm().is_err()
        {
            this.state = State::NotAttempted;
            this.scope_closed = true; // No ARM or native launch was called.
            this.finish_return(custody);
            return this;
        }
        this.accept_launch(custody.launch(&mut command));
        this
    }
    fn accept_launch(&mut self, result: Result<CohortChild, LaunchError>) {
        self.uncertain = false;
        self.state = match result {
            Ok(child) => State::Bound(Box::new(Bound::new(child, self.wake))),
            Err(LaunchError::NotSpawned(proof)) => State::NoChild(Some(proof)),
            Err(LaunchError::Unbound { spawned, error }) => {
                self.failure = Some(format!("bind: {error:?}"));
                self.uncertain = true;
                State::Unbound(Some(spawned))
            }
            Err(LaunchError::Refused(error)) => {
                self.failure = Some(format!("launch: {error:?}"));
                self.uncertain = true;
                self.events.abandon();
                State::Unknown
            }
        };
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    pub(super) fn test_resources_mut(&mut self) -> Option<&mut D> {
        self.resources.as_mut()
    }
    pub(super) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
    pub(super) fn contained(&self) -> bool {
        self.events.is_contained()
    }
    /// Observation for private BOUND journaling, never an owned-child capability.
    pub fn identity(&self) -> Option<(u32, u64)> {
        match &self.state {
            State::Bound(bound) if bound.birth != 0 => Some((bound.pid, bound.birth)),
            _ => None,
        }
    }
    /// Stop attributing the numeric group after its owning reap, independently
    /// of scope retirement. Original resources remain held until retirement.
    /// This observation supplies neither signal nor release authority.
    pub fn group(&self) -> Option<i32> {
        match &self.state {
            State::Bound(bound)
                if sampling_decision::may_attribute_group(bound.reaped, self.retired) =>
            {
                Some(bound.child.cohort().group())
            }
            _ => None,
        }
    }
    /// The actor has durably written BOUND/RUNNING and installed its member map.
    /// Validate the original current claim again immediately before actual GO.
    pub fn start(&mut self, custody: &mut impl MemberCustody) -> io::Result<Started> {
        self.start_inner(custody, false)
    }
    /// A producing action cannot substitute legacy process-group coverage for
    /// enforced containment. Refusal occurs before target GO.
    pub fn start_producer(&mut self, custody: &mut impl MemberCustody) -> io::Result<Started> {
        self.start_inner(custody, true)
    }
    fn start_inner(
        &mut self,
        custody: &mut impl MemberCustody,
        require_containment: bool,
    ) -> io::Result<Started> {
        let State::Bound(bound) = &mut self.state else {
            return Err(refused("member is not durably bound"));
        };
        if (bound.go_attempted && !bound.dispatching) || bound.cleanup_at.is_some() {
            return Err(refused("member cannot start or replay"));
        }
        if bound.birth == 0 || bound.exit.is_none() {
            self.events.abandon();
            bound.stop(CompletionCause::Cancelled);
            return Err(refused("member identity or exit witness unavailable"));
        }
        let first_dispatch = !bound.dispatching;
        let prepared = if first_dispatch {
            self.resources
                .as_mut()
                .ok_or_else(|| refused("member descriptors missing"))?
                .prepare_target(bound.child.child_mut())
        } else {
            Ok(true)
        };
        match prepared {
            Ok(false) => return Err(io::ErrorKind::WouldBlock.into()),
            Ok(true) => {}
            Err(error) => {
                self.events.abandon();
                bound.stop(CompletionCause::Cancelled);
                return Err(error);
            }
        }
        let prepare = (|| {
            if first_dispatch {
                bound.producer = self
                    .resources
                    .as_mut()
                    .ok_or_else(|| refused("member descriptors missing"))?
                    .take_producer_inputs()?;
                if let Some(inputs) = &bound.producer {
                    inputs.check_child(&bound.child, bound.pid, bound.birth)?;
                } else if require_containment {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "producer descendant retirement requires enforced native containment",
                    ));
                }
                self.resources
                    .as_mut()
                    .ok_or_else(|| refused("member descriptors missing"))?
                    .bind_child(bound.pid)?;
                self.resources
                    .as_mut()
                    .ok_or_else(|| refused("member descriptors missing"))?
                    .publish_prepared((bound.pid, bound.birth), &mut self.events)?;
                custody.validate()?;
                if let Some(inputs) = &mut bound.producer {
                    custody.bind_producer(inputs)?;
                }
                custody.before_go()?;
                bound.dispatching = true;
            }
            custody.validate()?;
            if self.events.cancelled() || bound.child.child_mut().exited()? {
                return Err(refused("member cancelled or exited before GO"));
            }
            let pid = bound.pid;
            let birth = bound.birth;
            let committed = &mut bound.committed_start;
            let configured_footprint_mib = self.configured_footprint_mib;
            let dispatch = self
                .resources
                .as_mut()
                .ok_or_else(|| refused("member descriptors missing"))?
                .poll_start(
                    bound.child.child_mut(),
                    &mut || self.events.cancelled(),
                    &mut |started, requested| {
                        if committed.is_some()
                            || started.pid != pid
                            || started.birth != birth
                            || started.monotonic_ns == 0
                            || started.unix_ns == 0
                            || requested.is_some_and(|limits| {
                                limits.units == 0
                                    || limits.footprint_mib == 0
                                    || configured_footprint_mib != Some(limits.footprint_mib)
                            })
                        {
                            return Err(refused("member Start commitment changed or replayed"));
                        }
                        custody.commit_started(&self.binding, started, requested)?;
                        custody.validate()?;
                        *committed = Some(started.clone());
                        Ok(())
                    },
                )?;
            let (started, since) = match dispatch {
                TargetStart::Resume => {
                    if !first_dispatch || bound.committed_start.is_some() {
                        return Err(refused(
                            "pending execution gate cannot become native resume",
                        ));
                    }
                    let (monotonic_ns, unix_ns) = clocks()?;
                    let since = Instant::now();
                    let metadata = Started {
                        pid,
                        birth,
                        monotonic_ns,
                        unix_ns,
                    };
                    custody.commit_started(&self.binding, &metadata, None)?;
                    custody.validate()?;
                    bound.committed_start = Some(metadata.clone());
                    if self.events.cancelled() || bound.child.child_mut().exited()? {
                        return Err(refused("member cancelled or exited before native resume"));
                    }
                    bound.go_attempted = true;
                    bound.child.child_mut().resume()?;
                    (metadata, since)
                }
                #[cfg(all(test, target_os = "macos"))]
                TargetStart::Pending { offered } => {
                    if offered && bound.committed_start.is_none() {
                        return Err(refused("member Run offered without durable Start"));
                    }
                    bound.go_attempted |= offered;
                    return Ok(None);
                }
                #[cfg(all(test, target_os = "macos"))]
                TargetStart::Started { metadata, since } => {
                    if bound.committed_start.as_ref() != Some(&metadata) {
                        return Err(refused("member Run differs from durable Start"));
                    }
                    bound.go_attempted = true;
                    (metadata, since)
                }
            };
            bound.started = Some(since);
            bound.launched = true;
            bound.dispatching = false;
            Ok(Some(started))
        })();
        match prepare {
            Ok(None) => Err(io::ErrorKind::WouldBlock.into()),
            Ok(Some(started)) => {
                if self
                    .events
                    .emit(Event::Started {
                        pid: started.pid,
                        birth: started.birth,
                        monotonic_ns: started.monotonic_ns,
                        unix_ns: started.unix_ns,
                    })
                    .is_err()
                {
                    bound.stop(CompletionCause::Cancelled);
                }
                Ok(started)
            }
            Err(error) => {
                // Resume failure is not affirmative no-child/no-execution proof.
                self.events.abandon();
                bound.stop(CompletionCause::Cancelled);
                Err(error)
            }
        }
    }
    pub fn covers(&mut self, peer: Peer) -> bool {
        let State::Bound(bound) = &mut self.state else {
            return false;
        };
        if !bound.launched
            || bound.cleanup_at.is_some()
            || bound.reaped
            || bound.child.child_mut().exited().unwrap_or(true)
        {
            return false;
        }
        let root = process::Birth(bound.birth);
        let native = i32::try_from(bound.pid).ok().and_then(process::read_birth);
        native == Some(root)
            && scope_covers(peer, bound.pid, bound.birth)
            && i32::try_from(bound.pid).ok().and_then(process::read_birth) == Some(root)
    }
    pub fn poll(
        &mut self,
        custody: &mut impl MemberCustody,
        cause: Option<CompletionCause>,
    ) -> Progress {
        self.events.flush();
        if let State::Unbound(spawned) = &mut self.state {
            if cause.is_some() || self.events.cancelled() {
                // This actual helper is held before GO. No target has run;
                // owned-child kill is safe even before durable group BIND.
                if let Some(child) = spawned.as_mut() {
                    let _ = child.child_mut().kill();
                }
            }
            if let Some(spawned) = spawned.take() {
                self.accept_launch(custody.bind(*spawned));
            }
        }
        if let State::NoChild(proof) = &mut self.state
            && let Some(proof) = proof.take()
        {
            // On an I/O failure the backend may retain the exact receipt;
            // this caller cannot turn that uncertainty into free capacity.
            match custody.cancel_no_child(*proof) {
                Ok(()) => {
                    self.scope_closed = true;
                    self.uncertain = false;
                }
                Err(error) => {
                    let (error, proof) = error.into_parts();
                    self.failure = Some(format!("cancel no child: {error:?}"));
                    self.state = match proof {
                        Some(proof) => State::NoChild(Some(Box::new(proof))),
                        None => State::Unknown,
                    };
                    self.uncertain = true;
                    self.events.abandon();
                }
            }
        }
        if let State::Bound(bound) = &mut self.state {
            let failure = poll_bound(bound, &mut self.events, self.timeout, cause);
            // A healthy later native wait is not a durable return receipt.
            // Retain earlier custody uncertainty through typed bank Pending;
            // only positive finish_return may resolve it and drop resources.
            self.uncertain |= failure.is_some();
            if let Some(failure) = failure {
                self.failure = Some(failure);
            }
            if bound.reaped && !self.scope_closed {
                // Retain actual CPU and original memory until input recheck.
                if let Some(result) = self
                    .resources
                    .as_mut()
                    .and_then(MemberResources::finish_inputs)
                {
                    self.events.inputs_unchanged(result);
                }
            }
            if bound.reaped && !self.scope_closed {
                if bound.producer.is_some() {
                    match custody.retire_producer(&mut bound.child, &mut bound.producer) {
                        Ok(retirement) => {
                            self.producer_retirement = Some(Box::new(retirement));
                            self.scope_closed = true;
                        }
                        Err(error) => {
                            self.failure = Some(format!("retire producer: {error:?}"));
                            self.uncertain = true;
                        }
                    }
                } else if custody.retire(&bound.child).is_ok() {
                    self.scope_closed = true;
                }
            }
        }
        self.finish_return(custody);
        let (ready, completed) = match &self.state {
            State::Bound(bound) => (
                (!bound.go_attempted || bound.dispatching)
                    && !bound.launched
                    && bound.cleanup_at.is_none(),
                bound.completed.clone(),
            ),
            _ => (false, None),
        };
        let (go_attempted, launched) = match &self.state {
            State::Bound(bound) => (bound.go_attempted, bound.launched),
            _ => (false, false),
        };
        Progress {
            go_attempted,
            launched,
            ready_to_start: ready,
            completed,
            retired: self.retired,
            uncertain: self.uncertain,
            drained: self.retired && self.events.drained(),
        }
    }
    fn finish_return(&mut self, custody: &mut impl MemberCustody) {
        if !self.scope_closed || self.retired {
            return;
        }
        let receipt = if matches!(self.state, State::NotAttempted) {
            ReturnReceipt::NoArm
        } else {
            ReturnReceipt::ScopeClosed
        };
        if let Some(retirement) = self.producer_retirement.take() {
            match custody.return_producer(retirement) {
                Ok(ProducerReturn::Returned(returned)) => self.producer_returned = Some(returned),
                Ok(ProducerReturn::Pending(original)) => {
                    // The SAME accepted original-bank request is outstanding.
                    // Preserve closure/resources without fabricating a fault or
                    // replaying retirement while its durable reply is pending.
                    self.producer_retirement = Some(original);
                    return;
                }
                Err((error, original)) => {
                    self.failure = Some(format!("return producer: {error:?}"));
                    self.producer_retirement = Some(original);
                    self.uncertain = true;
                    return;
                }
            }
        } else if !matches!(custody.return_unit(receipt), Ok(ReturnProgress::Returned)) {
            return;
        }
        // Consume once, only after the SAME original custody has durably
        // returned the unit. A failed return retains the result and originals.
        self.retired = true;
        self.uncertain = false;
        self.resources.take();
        if self.events.is_contained() {
            return;
        }
        // Native closure and the durable logical return are separate receipts.
        // Never re-consume a NoChild proof or re-retire a scope on write retry.
        let event = match &self.state {
            State::Bound(bound) => bound.completed.as_ref().map(|complete| Event::Finished {
                outcome: complete.outcome.clone(),
                cause: complete.cause,
                runtime_ns: complete.runtime_ns,
            }),
            State::NotAttempted | State::NoChild(None) => Some(Event::Refused {
                reason: "member target did not start".into(),
            }),
            _ => None,
        };
        if let Some(event) = event {
            let accounting = match &self.state {
                State::Bound(bound) => bound.completed.as_ref().map(|complete| complete.accounting),
                _ => None,
            }
            .unwrap_or_else(aterm_types::resource_admission::AccountingObservation::uncollected);
            let _ = self.events.emit_terminal(event, accounting);
        }
    }
    /// Actual journal persistence is performed here before the one-use native
    /// issuer may write its distinct terminal. No settled bool is accepted.
    pub(super) fn settle_contained(&mut self, journal: &super::journal::Journal) -> io::Result<()> {
        if !self.retired || !self.events.is_contained() {
            return Err(refused("contained operation is not actually retired"));
        }
        if let Some(returned) = &self.producer_returned {
            returned.settle(journal, &self.binding)?;
        } else {
            journal.settle(&self.binding)?;
        }
        let terminal = self.emit_contained_completion();
        if terminal.is_err() {
            // Actual journal settlement and native retirement already happened.
            // Lost/malformed delivery remains Unknown to the client; the SAME
            // issuer and accepted socket remain retained by the original Job.
            self.events.abandon();
        }
        Ok(())
    }
    fn emit_contained_completion(&mut self) -> io::Result<()> {
        let bound = match &self.state {
            State::NotAttempted | State::NoChild(None) => {
                return self.events.emit_contained_refused();
            }
            State::Bound(bound) if !bound.go_attempted => {
                return self.events.emit_contained_refused();
            }
            State::Bound(bound) => bound,
            _ => return Err(refused("contained original child missing")),
        };
        let completed = bound
            .completed
            .as_ref()
            .ok_or_else(|| refused("contained original wait missing"))?;
        self.events.emit_contained_terminal(
            self.producer_returned
                .as_ref()
                .ok_or_else(|| refused("contained native issuer missing"))?,
            Event::Finished {
                outcome: completed.outcome.clone(),
                cause: completed.cause,
                runtime_ns: completed.runtime_ns,
            },
            completed.accounting,
        )
    }
}
impl<D> Drop for OwnedAttempt<D> {
    fn drop(&mut self) {
        if !self.retired {
            if let State::Bound(bound) = &mut self.state
                && !bound.reaped
            {
                let _ = bound.child.request_group(CohortSignal::Kill);
            }
            if let State::Unbound(Some(child)) = &mut self.state {
                // The actual helper is held before GO; no target has run.
                let _ = child.child_mut().kill();
            }
            // Misuse must not return a borrowed CPU credit while an unknown
            // actual member may still run. The actor normally retains this
            // value; fail-closed debt lasts through the custodian lifetime.
            if let Some(resources) = self.resources.take() {
                std::mem::forget(resources);
            }
        }
    }
}

fn poll_bound(
    bound: &mut Bound,
    events: &mut Events,
    timeout: Option<Duration>,
    cause: Option<CompletionCause>,
) -> Option<String> {
    if bound.reaped {
        return None;
    }
    let exited = match bound.child.child_mut().exited() {
        Ok(exited) => exited,
        Err(error) => {
            bound.stop(cause.unwrap_or(CompletionCause::Cancelled));
            events.abandon();
            return Some(format!("observe child exit: {error:?}"));
        }
    };
    if bound.ended.is_none() {
        bound.ended = bound.exit.as_ref().and_then(ExitMoment::get);
    }
    if let Some(cause) = cause {
        bound.stop(cause);
    }
    if bound.cleanup_at.is_none() {
        let reason = if let (Some(start), Some(end)) = (bound.started, bound.ended) {
            Some(
                if timeout.is_some_and(|limit| end.saturating_duration_since(start) >= limit) {
                    CompletionCause::Timeout
                } else {
                    CompletionCause::Exited
                },
            )
        } else if exited {
            None
        }
        // Wait for the registered kernel exit stamp.
        else if bound
            .started
            .is_some_and(|start| timeout.is_some_and(|limit| start.elapsed() >= limit))
        {
            Some(CompletionCause::Timeout)
        } else if events.cancelled() {
            Some(CompletionCause::Cancelled)
        } else {
            None
        };
        if let Some(cause) = reason {
            bound.stop(cause);
        }
    }
    let others_exited = exited && crate::resource_memory::other_members_exited(bound.pid);
    if bound
        .cleanup_at
        .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
    {
        // Escalate even if a TERM-ignoring leader is still alive. A failed
        // group signal never justifies losing its original unreaped pin.
        if let Err(error) = bound.child.request_group(CohortSignal::Kill)
            && !others_exited
        {
            return Some(format!("kill member group: {error:?}"));
        }
    }
    if bound.cleanup_at.is_none() || !others_exited || (bound.launched && bound.ended.is_none()) {
        return None;
    }
    let (status, accounting) = match crate::resource_accounting::try_reap(
        bound.child.child_mut(),
        events.collect_accounting(),
    ) {
        Ok(Some(value)) => value,
        Ok(None) => return None,
        Err(error) => {
            events.abandon();
            return Some(format!("reap member: {error:?}"));
        }
    };
    bound.reaped = true;
    let outcome = match (status.code(), status.signal()) {
        (Some(code), _) => Some(Outcome::Exit(code)),
        (_, Some(signal)) => Some(Outcome::Signal(signal)),
        _ => None,
    };
    if bound.launched {
        if let (Some(outcome), Some(start), Some(end)) = (outcome, bound.started, bound.ended) {
            let complete = Completed {
                outcome,
                cause: bound.cause,
                runtime_ns: u64::try_from(end.saturating_duration_since(start).as_nanos())
                    .unwrap_or(u64::MAX),
                accounting,
            };
            bound.completed = Some(complete);
        } else {
            events.abandon();
        }
    } else {
        events.abandon();
    }
    None
}
fn cause_priority(cause: CompletionCause) -> u8 {
    if cause == CompletionCause::Exited {
        0
    } else if cause == CompletionCause::Cancelled {
        1
    } else if cause == CompletionCause::Timeout {
        2
    } else {
        3
    } // ResourceLimit is stronger than a pending graceful cancellation.
}
fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why)
}
#[cfg(test)]
#[path = "resource_admission_member/tests.rs"]
mod tests;
