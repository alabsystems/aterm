// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Only the original Root's actual retained Epoch pays for this query's
//! implicit unit. The Root keeps its own implicit unit; no pause/ACK/count is
//! treated as a CPU transfer. Unknown custody never returns the borrowed byte.

use super::{alive, delivery::Delivery, journal, preparation::Prepared, refused, spawnfd, wire};
use crate::actor::owner::{bootstrap_graph::QueryOperation, start};
use aterm_capacity::{
    cohort::launch::{CohortChild, CohortSignal, LaunchError},
    leadership::Fence,
    retained_cohort::{GnuLoan, GnuLoanClosure, GnuLoanReturnError, RetainedCohort},
};
use aterm_types::resource_admission::Binding;
use aterm_uds::{CtlStream, exitwatch::ExitMoment, process};
use std::{io, os::unix::process::ExitStatusExt as _, time::Instant};
use trust_resource_controller as control;

#[path = "diagnostics.rs"]
mod diagnostics;

enum State {
    Unused,
    Unknown,
    NoChild(Option<Box<spawnfd::NotSpawned>>),
    Unbound(Option<Box<spawnfd::Spawned>>),
    Bound(Box<Bound>),
}
struct Bound {
    child: CohortChild,
    identity: control::Identity,
    exit: Option<ExitMoment<Instant>>,
    target: Option<crate::metadata_prelude::NoForkTarget>,
    go_attempted: bool,
    started: Option<(Instant, control::Started)>,
    waited: Option<Waited>,
    cause: control::CompletionCause,
}
struct Waited {
    // Actual owning wait outcome survives later retirement/input/delivery error.
    status: std::process::ExitStatus,
    accounting: aterm_types::resource_admission::AccountingObservation,
    ended: Option<Instant>,
}

/// Stored in the original Job until either its private Completed is consumed
/// or actual failed-attempt cleanup finishes. Dropping an unknown attempt
/// retains its actual operation, input, stream and loan owners conservatively.
pub(in crate::actor::owner) struct Query {
    operation: Option<QueryOperation>,
    prepared: Option<Prepared>,
    stream: Option<CtlStream>,
    loan: Option<GnuLoan>,
    state: State,
    closure: Option<GnuLoanClosure>,
    return_error: Option<Box<GnuLoanReturnError>>,
    returned: bool,
    journal_created: bool,
    journal_settled: bool,
    delivery: Option<Delivery>,
    acknowledged: bool,
    failure: Option<io::Error>,
    input_check: Option<Result<(), String>>,
}

/// No constructor from Status/Terminal/ACK/Context exists. The same private
/// operation reaches this value only through real retirement and delivery.
pub(in crate::actor::owner) struct Completed {
    inner: Option<Box<CompletedInner>>,
}
struct CompletedInner {
    operation: QueryOperation,
    _prepared: Prepared,
    _loan: GnuLoan,
    _delivery: Delivery,
    bound: Box<Bound>,
    input_check: Option<Result<(), String>>,
}

/// The issuer alone creates this proof after one of its actual closure paths.
/// Request/context are binding expectations, never constructors of the proof.
pub(in crate::actor::owner) struct NativeClosed {
    request: wire::Request,
    context: control::ContextReply,
}
impl NativeClosed {
    pub(in crate::actor::owner) fn request(&self) -> &wire::Request {
        &self.request
    }
    pub(in crate::actor::owner) fn context(&self) -> &control::ContextReply {
        &self.context
    }
}

/// Constructed only after this SAME operation accepted its actual NativeClosed
/// proof. It is not exported to actor/data constructors.
pub(super) struct WorkClosed {
    _original: NativeClosed,
}

/// Diagnostic observations, with no descriptor, child, launch or release
/// authority. Missing fields stay absent instead of becoming measured zero.
pub(in crate::actor::owner) struct NativeObservation {
    pub(in crate::actor::owner) identity: Option<control::Identity>,
    pub(in crate::actor::owner) go_attempted: bool,
    pub(in crate::actor::owner) started: Option<control::Started>,
    pub(in crate::actor::owner) started_at: Option<Instant>,
    pub(in crate::actor::owner) status: Option<std::process::ExitStatus>,
    pub(in crate::actor::owner) accounting:
        Option<aterm_types::resource_admission::AccountingObservation>,
    pub(in crate::actor::owner) ended: Option<Instant>,
    pub(in crate::actor::owner) cause: Option<control::CompletionCause>,
}
impl NativeObservation {
    fn known_identity(identity: control::Identity) -> Option<control::Identity> {
        // accept_launch retains the real child before its fallible birth read.
        // Its temporary birth=0 is unavailable, not an observed identity.
        (identity.birth != 0).then_some(identity)
    }

    fn none() -> Self {
        Self {
            identity: None,
            go_attempted: false,
            started: None,
            started_at: None,
            status: None,
            accounting: None,
            ended: None,
            cause: None,
        }
    }
    fn bound(bound: &Bound) -> Self {
        Self {
            identity: Self::known_identity(bound.identity),
            go_attempted: bound.go_attempted,
            started: bound.started.map(|(_, started)| started),
            started_at: bound.started.map(|(at, _)| at),
            status: bound.waited.as_ref().map(|waited| waited.status),
            accounting: bound.waited.as_ref().map(|waited| waited.accounting),
            ended: bound.waited.as_ref().and_then(|waited| waited.ended),
            cause: Some(bound.cause),
        }
    }
}

pub(in crate::actor::owner) enum TransportClosure {
    AcknowledgedAndEof,
    FailedShutdown,
}

pub(in crate::actor::owner) struct RetainedObservation {
    pub(in crate::actor::owner) prepared: Option<super::preparation::PreparedObservation>,
    pub(in crate::actor::owner) native: NativeObservation,
    pub(in crate::actor::owner) inputs_unchanged: Option<Result<(), String>>,
    pub(in crate::actor::owner) transport: TransportClosure,
    pub(in crate::actor::owner) delivery: Option<super::delivery::DeliveryObservation>,
    pub(in crate::actor::owner) failure: Option<io::Error>,
}

/// Original operation inputs/diagnostics stay in the SAME Root through its
/// actual ClosedRoot, after only this query's real work guard has closed.
pub(in crate::actor::owner) struct Retained {
    operation: QueryOperation,
    _inputs: Option<super::preparation::RetainedInputs>,
    _loan: Option<GnuLoan>,
    _closed: WorkClosed,
    observation: RetainedObservation,
}
impl Completed {
    fn inner(&self) -> &CompletedInner {
        self.inner.as_deref().expect("original completed query")
    }
    pub(in crate::actor::owner) fn request(&self) -> &wire::Request {
        self.inner().operation.request()
    }
    pub(in crate::actor::owner) fn context(&self) -> &control::ContextReply {
        self.inner().operation.context()
    }
    pub(in crate::actor::owner) fn into_retained(mut self) -> Result<Retained, Self> {
        let closed = NativeClosed {
            request: self.request().clone(),
            context: self.context().clone(),
        };
        if self
            .inner
            .as_mut()
            .expect("original completed query")
            .operation
            .close_work(&closed)
            .is_err()
        {
            return Err(self);
        }
        let original = *self.inner.take().expect("same closed original query");
        let closed = WorkClosed { _original: closed };
        let native = NativeObservation::bound(&original.bound);
        let delivery = original._delivery.observation();
        let (inputs, prepared) = original._prepared.into_retained(&closed);
        // These are the actual already-reaped handle/watch and exhausted
        // transport. Only this known closure path disposes them.
        drop(original.bound);
        drop(original._delivery);
        Ok(Retained {
            operation: original.operation,
            _inputs: Some(inputs),
            _loan: Some(original._loan),
            _closed: closed,
            observation: RetainedObservation {
                prepared: Some(prepared),
                native,
                inputs_unchanged: original.input_check,
                transport: TransportClosure::AcknowledgedAndEof,
                delivery: Some(delivery),
                failure: None,
            },
        })
    }
}

impl Drop for Completed {
    fn drop(&mut self) {
        if let Some(original) = self.inner.take() {
            // Abandonment or failed SAME-root work-guard validation must not
            // perform descriptor compaction through ordinary field Drop.
            std::mem::forget(original);
        }
    }
}

/// Failed-operation cleanup, never a successful SDK query or graph permission.
/// Only native closure/unused cancellation plus real output EOF and explicit
/// accepted-socket shutdown can construct it. Unknown borrower return cannot.
pub(in crate::actor::owner) struct ClosedFailed {
    inner: Option<Box<ClosedFailedInner>>,
}
struct ClosedFailedInner {
    operation: QueryOperation,
    prepared: Option<Prepared>,
    _loan: Option<GnuLoan>,
    _state: State,
    _stream: Option<CtlStream>,
    _delivery: Option<Delivery>,
    failure: io::Error,
    input_check: Option<Result<(), String>>,
}
impl ClosedFailed {
    fn inner(&self) -> &ClosedFailedInner {
        self.inner.as_deref().expect("original failed query")
    }
    pub(in crate::actor::owner) fn request(&self) -> &wire::Request {
        self.inner().operation.request()
    }
    pub(in crate::actor::owner) fn context(&self) -> &control::ContextReply {
        self.inner().operation.context()
    }
    pub(in crate::actor::owner) fn into_retained(mut self) -> Result<Retained, Self> {
        let closed = NativeClosed {
            request: self.request().clone(),
            context: self.context().clone(),
        };
        if self
            .inner
            .as_mut()
            .expect("original failed query")
            .operation
            .close_work(&closed)
            .is_err()
        {
            return Err(self);
        }
        let original = *self
            .inner
            .take()
            .expect("same closed original failed query");
        let closed = WorkClosed { _original: closed };
        let delivery = original._delivery.as_ref().map(Delivery::observation);
        let native = match &original._state {
            State::Bound(bound) => NativeObservation::bound(bound),
            _ => NativeObservation::none(),
        };
        let (inputs, prepared) = match original.prepared {
            Some(prepared) => {
                let (inputs, observation) = prepared.into_retained(&closed);
                (Some(inputs), Some(observation))
            }
            None => (None, None),
        };
        // into_closed_failed already established native closure/unused proof,
        // all original stream EOFs and successful same-socket shutdown.
        drop(original._state);
        drop(original._stream);
        drop(original._delivery);
        Ok(Retained {
            operation: original.operation,
            _inputs: inputs,
            _loan: original._loan,
            _closed: closed,
            observation: RetainedObservation {
                prepared,
                native,
                inputs_unchanged: original.input_check,
                transport: TransportClosure::FailedShutdown,
                delivery,
                failure: Some(original.failure),
            },
        })
    }
}

impl Drop for ClosedFailed {
    fn drop(&mut self) {
        if let Some(original) = self.inner.take() {
            std::mem::forget(original);
        }
    }
}

impl Query {
    /// Sampling DATA only. Never attribute a saved numeric group after an
    /// owning reap, failed preparation, uncertain launch or custody error.
    /// Actual GNU/journal/native closure remains independent of this result.
    pub(in crate::actor::owner) fn sampling_group(&self) -> io::Result<Option<i32>> {
        if self.returned {
            return Ok(None);
        }
        match &self.state {
            State::Unused | State::NoChild(_) => Ok(None),
            State::Bound(bound)
                if self.failure.is_none()
                    && bound.waited.is_none()
                    && bound.identity.birth != 0 =>
            {
                Ok(Some(bound.child.cohort().group()))
            }
            _ => Err(refused("query native sampling scope is unresolved")),
        }
    }

    /// The caller supplies actual SAME Job resources and a nonconstructible
    /// operation issued by its graph guard after native peer authentication.
    /// Every fallible action after that issuance returns this owning value.
    pub(in crate::actor::owner) fn begin(
        operation: QueryOperation,
        stream: CtlStream,
        fence: &Fence,
        journal: &journal::Journal,
        cohort: &mut RetainedCohort,
        root: &mut CohortChild,
        caller_deadline: Instant,
    ) -> Self {
        let mut this = Self {
            operation: Some(operation),
            prepared: None,
            stream: Some(stream),
            loan: None,
            state: State::Unused,
            closure: None,
            return_error: None,
            returned: false,
            journal_created: false,
            journal_settled: false,
            delivery: None,
            acknowledged: false,
            failure: None,
            input_check: None,
        };
        let result = this.begin_current(fence, journal, cohort, root, caller_deadline);
        if let Err(error) = result {
            this.fail(error);
        }
        this
    }

    fn binding(&self) -> Binding {
        let request = self
            .operation
            .as_ref()
            .expect("original operation")
            .request();
        Binding {
            session: request.key.graph.query.binding.session.clone(),
            attempt: request.key.operation,
        }
    }

    fn check_original_root(&self, root: &mut CohortChild) -> io::Result<()> {
        let expected = self
            .operation
            .as_ref()
            .expect("original operation")
            .context()
            .context
            .original_root;
        let child = root.child_mut();
        if child.id() != expected.pid
            || child.is_reaped()
            || child.exited()?
            || process::read_birth(expected.pid as i32).map(|birth| birth.0) != Some(expected.birth)
            || root.cohort().group() != expected.pid as i32
        {
            return Err(refused(
                "query operation differs from its actual original Root",
            ));
        }
        Ok(())
    }

    fn begin_current(
        &mut self,
        fence: &Fence,
        journal: &journal::Journal,
        cohort: &mut RetainedCohort,
        root: &mut CohortChild,
        caller_deadline: Instant,
    ) -> io::Result<()> {
        let operation = self.operation.as_ref().expect("original operation");
        self.prepared = Some(Prepared::new(
            operation.request().clone(),
            operation.sdk(),
            operation.gnu(),
            operation.deadline(),
            caller_deadline,
        )?);
        let prepared = self.prepared.as_mut().expect("same query preparation");
        prepared.validate()?;
        // The private graph operation is an expectation for this SAME actual
        // child/custody; it cannot authorize another Job's paid reservation.
        let expected_root = self
            .operation
            .as_ref()
            .expect("original operation")
            .context()
            .context
            .original_root;
        if root.child_mut().id() != expected_root.pid
            || root.child_mut().exited()?
            || process::read_birth(expected_root.pid as i32).map(|birth| birth.0)
                != Some(expected_root.birth)
            || root.cohort().group() != expected_root.pid as i32
        {
            return Err(refused("Bootstrap query original Root differs"));
        }
        let memory = cohort.observation(fence)?.memory;
        // The new query row owns no additional fixed claim. Cold recovery sees
        // only the original aggregate; native Epoch closure remains independent.
        let binding = Binding {
            session: prepared.request.key.graph.query.binding.session.clone(),
            attempt: prepared.request.key.operation,
        };
        // Retain uncertainty even if publication itself returns an error after
        // creating the actual row. No ARM precedes its acknowledged update.
        self.journal_created = true;
        journal.create(&binding)?;
        journal.update(&binding, "member_prepared", aterm_json::json!({
            "parent":aterm_codec::hex::encode_lower(&prepared.request.key.graph.query.binding.root_attempt),
            "memory":memory.to_json(), "purpose":wire::PURPOSE,
            "request_sha256":aterm_codec::hex::encode_lower(&prepared.digest)
        }))?;
        self.loan = Some(cohort.try_loan_gnu(fence)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "no actual supplementary Root GNU byte available",
            )
        })?);
        prepared.validate()?;
        let mut command = prepared
            .command
            .take()
            .ok_or_else(|| refused("query launch was already attempted"))?;
        // State becomes Unknown BEFORE attempting ARM. Neither Refused nor an
        // unwind can be mistaken for the lexical unused-loan cancellation.
        self.state = State::Unknown;
        let result = cohort.launch_loaned_subgroup(
            fence,
            root,
            &mut command,
            self.loan.as_mut().expect("actual original byte"),
        );
        // Release the parent's write-end copies regardless of launch outcome.
        // Physical EOF must come from the actual child, not a retained Command.
        drop(command);
        self.accept_launch(result)
    }

    fn accept_launch(&mut self, result: Result<CohortChild, LaunchError>) -> io::Result<()> {
        match result {
            Ok(mut child) => {
                let pid = child.child_mut().id();
                // Retain actual child before fallible identity/watch reads.
                self.state = State::Bound(Box::new(Bound {
                    child,
                    identity: control::Identity {
                        pid,
                        birth: 0,
                        birth_kind: if cfg!(target_os = "macos") {
                            control::BirthKind::MacosWallMicroseconds
                        } else {
                            control::BirthKind::LinuxStartTicks
                        },
                    },
                    exit: ExitMoment::watch_then(pid, Instant::now, crate::actor::wake),
                    target: None,
                    go_attempted: false,
                    started: None,
                    waited: None,
                    cause: control::CompletionCause::Exited,
                }));
                let State::Bound(bound) = &mut self.state else {
                    unreachable!()
                };
                bound.identity.birth = process::read_birth(pid as i32)
                    .ok_or_else(|| refused("query actual birth unavailable"))?
                    .0;
                if bound.exit.is_none() {
                    return Err(refused("query native exit observation unavailable"));
                }
                Ok(())
            }
            Err(LaunchError::NotSpawned(proof)) => {
                let why = proof.error().to_string();
                self.state = State::NoChild(Some(proof));
                Err(io::Error::other(format!(
                    "native query target did not start: {why}"
                )))
            }
            Err(LaunchError::Unbound { spawned, error }) => {
                self.state = State::Unbound(Some(spawned));
                Err(error)
            }
            Err(LaunchError::Refused(error)) => {
                self.state = State::Unknown;
                Err(error)
            }
        }
    }

    fn fail(&mut self, error: io::Error) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        if matches!(self.state, State::Unused)
            && let Some(prepared) = &mut self.prepared
        {
            // No launch was called. Closing these actual unused parent
            // write descriptions permits physical read EOF during cleanup.
            drop(prepared.command.take());
        }
    }

    /// Cancellation/faults are requests only. The SAME native owner still
    /// waits, captures, retires and returns its actual original borrower.
    pub(in crate::actor::owner) fn poll(
        &mut self,
        fence: &Fence,
        journal: &journal::Journal,
        cohort: &mut RetainedCohort,
        root: &mut CohortChild,
        cancel: Option<control::CompletionCause>,
    ) -> bool {
        let result = self.poll_current(fence, journal, cohort, root, cancel);
        match result {
            Ok(ready) => ready,
            Err(error) => {
                self.fail(error);
                false
            }
        }
    }

    fn poll_current(
        &mut self,
        fence: &Fence,
        journal: &journal::Journal,
        cohort: &mut RetainedCohort,
        root: &mut CohortChild,
        cancel: Option<control::CompletionCause>,
    ) -> io::Result<bool> {
        let binding = self.binding();
        if let Some(prepared) = &mut self.prepared
            && let Err(error) = prepared.stdout.poll()
        {
            self.fail(error);
        }
        if let Some(prepared) = &mut self.prepared
            && let Err(error) = prepared.stderr.poll()
        {
            self.fail(error);
        }
        let expired = self
            .prepared
            .as_ref()
            .is_some_and(|p| Instant::now() >= p.deadline);
        let root_closing = root.child_mut().exited()?;
        let cancelling = cancel.is_some() || root_closing || expired || self.failure.is_some();
        if expired {
            self.fail(io::ErrorKind::TimedOut.into());
        }
        if root_closing || cancel.is_some() {
            self.fail(refused("original Bootstrap query Root is closing"));
        }
        if !cancelling {
            self.check_original_root(root)?;
        }
        if let State::Unbound(spawned) = &mut self.state {
            if let Some(actual) = spawned.as_mut() {
                let _ = actual.child_mut().kill();
            }
            if let Some(actual) = spawned.take() {
                let result = cohort.bind_loaned_subgroup(
                    fence,
                    *actual,
                    self.loan.as_mut().expect("original loan"),
                );
                self.accept_launch(result)?;
            }
        }
        if let State::Bound(bound) = &mut self.state {
            let prepared = self.prepared.as_mut().expect("same actual inputs");
            let ended = bound.exit.as_ref().and_then(ExitMoment::get);
            if cancelling && bound.waited.is_none() {
                // Preserve an already observed natural exit. A later actor
                // cancellation/ACK delay does not rewrite its actual cause.
                if ended.is_none() {
                    bound.cause = cancel.unwrap_or(if expired {
                        control::CompletionCause::Timeout
                    } else {
                        control::CompletionCause::Cancelled
                    });
                } else if ended.is_some_and(|end| end >= prepared.deadline) {
                    bound.cause = control::CompletionCause::Timeout;
                }
                // An unreaped original group owner is the only signal issuer.
                // No post-reap PID/group number is signalled.
                let _ = bound.child.request_group(CohortSignal::Kill);
            }
            if !bound.go_attempted && !cancelling {
                prepared.validate()?;
                cohort.observation(fence)?;
                if root.child_mut().exited()? {
                    return Err(refused("query original Root began closing before GO"));
                }
                let held = &prepared.held;
                if prepared.prelude.poll(bound.child.child_mut(), |pid| {
                    held.validate_loaded(
                        trust_resource_provenance::process::read_identity(pid)?,
                        prepared.deadline,
                    )
                })? {
                    let target = prepared
                        .prelude
                        .take_retirement()
                        .ok_or_else(|| refused("query actual no-fork owner missing"))?;
                    if !target.matches(
                        bound.identity.pid,
                        bound.identity.birth,
                        bound.child.cohort().group(),
                    ) {
                        bound.target = Some(target);
                        return Err(refused("query native containment belongs to another child"));
                    }
                    bound.target = Some(target);
                    prepared.handoff.bind_child(bound.identity.pid)?;
                    prepared.validate()?;
                    journal.update(
                        &binding,
                        "member_bound",
                        aterm_json::json!({"pid":bound.identity.pid,"birth":bound.identity.birth}),
                    )?;
                    journal.update(&binding, "member_running", aterm_json::json!({}))?;
                    let (monotonic_ns, unix_ns) = start::clocks()?;
                    if root.child_mut().exited()? || bound.child.child_mut().exited()? {
                        return Err(refused("query target or original Root exited before GO"));
                    }
                    alive(prepared.deadline)?;
                    let started = Instant::now();
                    bound.go_attempted = true;
                    bound.child.child_mut().resume()?;
                    bound.started = Some((
                        started,
                        control::Started {
                            identity: bound.identity,
                            monotonic_ns,
                            unix_ns,
                        },
                    ));
                }
            }
            if bound.waited.is_none()
                && bound.child.child_mut().exited()?
                && (!bound.go_attempted || ended.is_some())
                && let Some((status, accounting)) =
                    crate::resource_accounting::try_reap(bound.child.child_mut(), true)?
            {
                bound.waited = Some(Waited {
                    status,
                    accounting,
                    ended,
                });
            }
            if bound.waited.is_some()
                && self.closure.is_none()
                && !self.returned
                && self.return_error.is_none()
            {
                self.closure = Some(GnuLoanClosure::Retired(cohort.retire_loaned_subgroup(
                    fence,
                    &mut bound.child,
                    self.loan.as_mut().expect("same original byte"),
                )?));
            }
        }
        if let State::NoChild(slot) = &mut self.state
            && let Some(proof) = slot.take()
        {
            match cohort.cancel_loaned_subgroup(
                fence,
                *proof,
                self.loan.as_mut().expect("same original byte"),
            ) {
                Ok(closure) => self.closure = Some(closure),
                Err(error) => {
                    let (error, original) = error.into_parts();
                    if let Some(original) = original {
                        *slot = Some(Box::new(original));
                    } else {
                        self.state = State::Unknown;
                    }
                    return Err(error);
                }
            }
        }
        if matches!(self.state, State::Unused) && self.closure.is_none() && !self.returned {
            if let Some(loan) = &mut self.loan {
                self.closure = Some(cohort.cancel_unused_gnu_loan(fence, loan)?);
            } else if self.journal_created && !self.journal_settled {
                journal.settle(&binding)?;
                self.journal_settled = true;
            }
        }
        if self.closure.is_some() && !self.journal_settled {
            journal.update(&binding, "member_terminal", aterm_json::json!({}))?;
            journal.settle(&binding)?;
            self.journal_settled = true;
        }
        if self.journal_settled
            && self.return_error.is_none()
            && let Some(closure) = self.closure.take()
        {
            match cohort.return_gnu_loan(
                fence,
                self.loan.as_mut().expect("same original byte"),
                closure,
            ) {
                Ok(()) => self.returned = true,
                Err(error) => {
                    self.return_error = Some(Box::new(error));
                    return Err(refused("query original GNU return remains unknown"));
                }
            }
        }
        if !self.returned || self.failure.is_some() || !self.journal_settled {
            return Ok(false);
        }
        let State::Bound(bound) = &self.state else {
            return Ok(false);
        };
        let Some((started, observation)) = &bound.started else {
            return Ok(false);
        };
        let Some(waited) = &bound.waited else {
            return Ok(false);
        };
        let prepared = self.prepared.as_ref().expect("original query inputs");
        if !prepared.stdout.complete() || !prepared.stderr.complete() {
            return Ok(false);
        }
        if self.delivery.is_none() {
            let outcome = if let Some(code) = waited.status.code() {
                control::Outcome::Exit(code)
            } else if let Some(signal) = waited.status.signal() {
                control::Outcome::Signal(signal)
            } else {
                return Err(refused("query original owning wait has no native outcome"));
            };
            let ended = waited
                .ended
                .ok_or_else(|| refused("query native exit stamp unavailable"))?;
            let runtime_ns = u64::try_from(
                ended
                    .checked_duration_since(*started)
                    .ok_or_else(|| refused("query native runtime bookends differ"))?
                    .as_nanos(),
            )
            .map_err(|_| refused("query native runtime exceeds format bound"))?;
            let inputs_unchanged = prepared.validate().map_err(|error| error.to_string());
            self.input_check = Some(inputs_unchanged.clone());
            let terminal = wire::Terminal::Completed {
                completion: control::MetadataCompletion {
                    finished: control::StatusFrame {
                        root_attempt: prepared.request.key.graph.query.binding.root_attempt,
                        member_attempt: prepared.request.key.operation,
                        status: control::Status::Finished {
                            outcome,
                            cause: bound.cause,
                            runtime_ns,
                        },
                    },
                    inputs_unchanged,
                },
                observation: control::accounting::Observation {
                    scope: control::accounting::Scope::ChildAndWaitedDescendantsV1,
                    cpu_times: waited.accounting.cpu_times.map(|cpu| {
                        control::accounting::CpuTimes {
                            user_ns: cpu.user_ns,
                            system_ns: cpu.system_ns,
                        }
                    }),
                    peak_rss_bytes: waited.accounting.peak_rss_bytes,
                },
            };
            let stream = self
                .stream
                .take()
                .ok_or_else(|| refused("original accepted query stream missing"))?;
            self.delivery = Some(Delivery::after_retirement(
                stream,
                prepared,
                *observation,
                terminal,
            )?);
        }
        self.acknowledged = self
            .delivery
            .as_mut()
            .expect("original delivery")
            .poll(prepared.deadline)?;
        Ok(self.acknowledged)
    }

    pub(in crate::actor::owner) fn into_completed(
        mut self: Box<Self>,
    ) -> Result<Completed, Box<Self>> {
        if !self.acknowledged || !self.returned || !self.journal_settled || self.failure.is_some() {
            return Err(self);
        }
        if !matches!(&self.state, State::Bound(bound) if bound.waited.is_some()) {
            return Err(self);
        }
        let State::Bound(bound) = std::mem::replace(&mut self.state, State::Unknown) else {
            unreachable!()
        };
        Ok(Completed {
            inner: Some(Box::new(CompletedInner {
                operation: self.operation.take().expect("same original operation"),
                _prepared: self.prepared.take().expect("same original inputs"),
                _loan: self.loan.take().expect("same original returned byte"),
                _delivery: self
                    .delivery
                    .take()
                    .expect("same acknowledged accepted stream"),
                bound,
                input_check: self.input_check.take(),
            })),
        })
    }

    /// Consumed only by the original graph's failed-root cleanup, which must
    /// remain permanently failed. This cannot clear the normal query gate or
    /// authorize enrollment/Steps. Its input owners live in the SAME Root until
    /// actual ClosedRoot, independently of native GNU/capacity closure.
    pub(in crate::actor::owner) fn into_closed_failed(
        mut self: Box<Self>,
    ) -> Result<ClosedFailed, Box<Self>> {
        let no_arm = matches!(self.state, State::Unused) && self.loan.is_none();
        let journal_closed = !self.journal_created || self.journal_settled;
        let native_closed = no_arm || (self.returned && self.return_error.is_none());
        if self.failure.is_none() || !native_closed || !journal_closed {
            return Err(self);
        }
        if self
            .prepared
            .as_ref()
            .is_some_and(|p| !p.stdout.at_eof() || !p.stderr.at_eof())
        {
            return Err(self);
        }
        if let Some(stream) = &self.stream
            && stream.shutdown(std::net::Shutdown::Both).is_err()
        {
            return Err(self);
        }
        if let Some(delivery) = &self.delivery
            && delivery.abort().is_err()
        {
            return Err(self);
        }
        Ok(ClosedFailed {
            inner: Some(Box::new(ClosedFailedInner {
                operation: self.operation.take().expect("same original operation"),
                prepared: self.prepared.take(),
                _loan: self.loan.take(),
                _state: std::mem::replace(&mut self.state, State::Unknown),
                _stream: self.stream.take(),
                _delivery: self.delivery.take(),
                failure: self.failure.take().expect("actual failed attempt"),
                input_check: self.input_check.take(),
            })),
        })
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // A consumed Completed has taken these original owners already. Every
        // other abandoned query, including pre-GO failure, is conservative.
        if self.operation.is_none() {
            return;
        }
        match &mut self.state {
            State::Bound(bound) => {
                if !bound.child.child_mut().is_reaped() {
                    let _ = bound.child.request_group(CohortSignal::Kill);
                }
            }
            State::Unbound(Some(spawned)) => {
                let _ = spawned.child_mut().kill();
            }
            _ => {}
        }
        if let Some(value) = self.operation.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.prepared.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.stream.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.loan.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.closure.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.return_error.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.delivery.take() {
            std::mem::forget(value);
        }
        std::mem::forget(std::mem::replace(&mut self.state, State::Unknown));
    }
}

#[cfg(test)]
#[path = "execution_observation_tests.rs"]
mod observation_tests;
