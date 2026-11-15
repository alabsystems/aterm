// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The original cohort alone lends and returns its actual supplementary byte.
//! Every uncertain branch retains its actual operation, bundle and child.
use super::{
    alive, journal,
    preparation::Prepared,
    refused, spawnfd,
    transport::{Delivery, Peek},
    wire,
};
use crate::actor::owner::{bootstrap_graph::ArchiveOperation, start};
use aterm_capacity::{
    cohort::launch::{CohortChild, CohortSignal, LaunchError},
    leadership::Fence,
    retained_cohort::{GnuLoan, GnuLoanClosure, GnuLoanReturnError, RetainedCohort},
};
use aterm_types::resource_admission::Binding;
use aterm_uds::{CtlStream, exitwatch::ExitMoment, process};
use std::{io, os::unix::process::ExitStatusExt as _, time::Instant};
use trust_resource_controller as control;

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
    status: std::process::ExitStatus,
    accounting: aterm_types::resource_admission::AccountingObservation,
    ended: Option<Instant>,
}
impl State {
    fn accept_launch(
        &mut self,
        binding: &Binding,
        result: Result<CohortChild, LaunchError>,
    ) -> io::Result<()> {
        match result {
            Ok(mut child) => {
                let pid = child.child_mut().id();
                crate::resource_observation::born(binding, pid);
                // Retain the actual child before fallible identity/watch reads.
                *self = State::Bound(Box::new(Bound {
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
                let State::Bound(bound) = self else {
                    unreachable!()
                };
                bound.identity.birth = process::read_birth(pid as i32)
                    .ok_or_else(|| refused("archive actual birth unavailable"))?
                    .0;
                if bound.exit.is_none() {
                    return Err(refused("archive native exit observer unavailable"));
                }
                Ok(())
            }
            Err(LaunchError::NotSpawned(proof)) => {
                let why = proof.error().to_string();
                *self = State::NoChild(Some(proof));
                Err(io::Error::other(why))
            }
            Err(LaunchError::Unbound { mut spawned, error }) => {
                crate::resource_observation::born(binding, spawned.child_mut().id());
                *self = State::Unbound(Some(spawned));
                Err(error)
            }
            Err(LaunchError::Refused(error)) => {
                *self = State::Unknown;
                Err(error)
            }
        }
    }
}
pub(in crate::actor::owner) struct Archive {
    operation: Option<ArchiveOperation>,
    prepared: Option<Prepared>,
    stream: Option<CtlStream>,
    state: State,
    loan: Option<GnuLoan>,
    closure: Option<GnuLoanClosure>,
    return_error: Option<Box<GnuLoanReturnError>>,
    returned: bool,
    journal_created: bool,
    journal_settled: bool,
    failure: Option<io::Error>,
    delivery: Option<Delivery>,
    acknowledged: bool,
    inputs_unchanged: Option<Result<(), String>>,
    closed: bool,
}
/// Issued only from the actual native closure path below. DATA cannot create it.
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
/// SAME originals stay in the actual Root until ClosedRoot. This is neither an
/// artifact/seed owner nor availability for an unrelated operation.
pub(in crate::actor::owner) struct Retained {
    original: Box<Archive>,
    success: bool,
}
impl Retained {
    pub(in crate::actor::owner) fn request(&self) -> &wire::Request {
        &self
            .original
            .operation
            .as_ref()
            .expect("same original archive")
            .request
    }
    pub(in crate::actor::owner) fn context(&self) -> &control::ContextReply {
        &self
            .original
            .operation
            .as_ref()
            .expect("same original archive")
            .context
    }
    pub(in crate::actor::owner) fn success(&self) -> bool {
        self.success
    }
}
impl Archive {
    pub(in crate::actor::owner) fn prepare(operation: ArchiveOperation, stream: CtlStream) -> Self {
        let mut this = Self {
            operation: Some(operation),
            prepared: None,
            stream: Some(stream),
            state: State::Unused,
            loan: None,
            closure: None,
            return_error: None,
            returned: false,
            journal_created: false,
            journal_settled: false,
            failure: None,
            delivery: None,
            acknowledged: false,
            inputs_unchanged: None,
            closed: false,
        };
        match Prepared::new(this.operation.as_ref().expect("same actual archive")) {
            Ok(prepared) => this.prepared = Some(prepared),
            Err(error) => this.fail(error),
        }
        this
    }
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
            _ => Err(refused("archive native sampling owner unresolved")),
        }
    }
    fn binding(&self) -> Binding {
        let request = &self
            .operation
            .as_ref()
            .expect("same original archive")
            .request;
        Binding {
            session: request.key.graph.query.binding.session.clone(),
            attempt: request.key.operation,
        }
    }
    fn check_root(&self, root: &mut CohortChild) -> io::Result<()> {
        let expected = self
            .operation
            .as_ref()
            .expect("same original archive")
            .context
            .context
            .original_root;
        if root.child_mut().id() != expected.pid
            || root.child_mut().is_reaped()
            || root.child_mut().exited()?
            || root.cohort().group() != expected.pid as i32
            || process::read_birth(expected.pid as i32).map(|birth| birth.0) != Some(expected.birth)
        {
            return Err(refused("archive differs from actual original Root"));
        }
        Ok(())
    }
    fn begin(
        &mut self,
        fence: &Fence,
        journal: &journal::Journal,
        cohort: &mut RetainedCohort,
        root: &mut CohortChild,
    ) -> io::Result<()> {
        self.check_root(root)?;
        let binding = self.binding();
        let operation = self.operation.as_ref().expect("same original archive");
        let prepared = self
            .prepared
            .as_mut()
            .ok_or_else(|| refused("archive preparation missing"))?;
        prepared.validate(operation)?;
        let memory = cohort.observation(fence)?.memory;
        self.journal_created = true;
        journal.create(&binding)?;
        journal.update(
            &binding,
            "member_prepared",
            aterm_json::json!({"purpose":wire::PURPOSE,"memory":memory.to_json()}),
        )?;
        self.loan = Some(cohort.try_loan_gnu(fence)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "no actual supplementary original GNU byte",
            )
        })?);
        let prepared = self.prepared.as_mut().expect("same archive preparation");
        prepared.validate(self.operation.as_ref().expect("same original archive"))?;
        let mut command = prepared
            .command
            .take()
            .ok_or_else(|| refused("archive launch already attempted"))?;
        self.state = State::Unknown;
        let launched = cohort.launch_loaned_subgroup(
            fence,
            root,
            &mut command,
            self.loan.as_mut().expect("same original loan"),
        );
        drop(command); // release every parent's write copy; EOF must be physical
        self.accept_launch(launched)
    }
    fn accept_launch(&mut self, result: Result<CohortChild, LaunchError>) -> io::Result<()> {
        let binding = self.binding();
        self.state.accept_launch(&binding, result)
    }
    fn fail(&mut self, error: io::Error) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        if let Some(operation) = &self.operation {
            operation.inputs.quarantine();
        }
        if matches!(self.state, State::Unused)
            && let Some(prepared) = &mut self.prepared
        {
            drop(prepared.command.take());
        }
    }
    pub(in crate::actor::owner) fn poll(
        &mut self,
        fence: &Fence,
        journal: &journal::Journal,
        cohort: &mut RetainedCohort,
        root: &mut CohortChild,
        cancel: bool,
    ) {
        if let Err(error) = self.poll_current(fence, journal, cohort, root, cancel) {
            self.fail(error);
        }
    }
    fn poll_current(
        &mut self,
        fence: &Fence,
        journal: &journal::Journal,
        cohort: &mut RetainedCohort,
        root: &mut CohortChild,
        cancel: bool,
    ) -> io::Result<()> {
        let binding = self.binding();
        let deadline = self
            .operation
            .as_ref()
            .expect("same original archive")
            .deadline;
        let expired = Instant::now() >= deadline;
        let root_closing = root.child_mut().exited()?;
        if let Some(stream) = &self.stream
            && !matches!(
                super::transport::Socket::peek_original(stream)?,
                Peek::Waiting
            )
        {
            self.fail(refused(
                "archive requester closed or sent control before terminal",
            ));
        }
        if expired {
            self.fail(io::ErrorKind::TimedOut.into());
        }
        if cancel || root_closing {
            self.fail(refused("original archive Root is closing"));
        }
        for stdout in [true, false] {
            if let Some(prepared) = &mut self.prepared {
                let result = if stdout {
                    prepared.stdout.poll()
                } else {
                    prepared.stderr.poll()
                };
                if let Err(error) = result {
                    self.fail(error);
                }
            }
        }
        if self.failure.is_none() {
            self.check_root(root)?;
            if matches!(self.state, State::Unused) {
                self.begin(fence, journal, cohort, root)?;
            }
        }
        if let State::Unbound(slot) = &mut self.state {
            if let Some(actual) = slot.as_mut() {
                let _ = actual.child_mut().kill();
            }
            if let Some(actual) = slot.take() {
                let result = cohort.bind_loaned_subgroup(
                    fence,
                    *actual,
                    self.loan.as_mut().expect("same original loan"),
                );
                self.accept_launch(result)?;
            }
        }
        let cancelling = self.failure.is_some();
        if let State::Bound(bound) = &mut self.state {
            let prepared = self
                .prepared
                .as_mut()
                .expect("same actual archive preparation");
            let operation = self.operation.as_ref().expect("same actual archive");
            let ended = bound.exit.as_ref().and_then(ExitMoment::get);
            if cancelling && bound.waited.is_none() {
                if ended.is_none() {
                    bound.cause = if expired {
                        control::CompletionCause::Timeout
                    } else {
                        control::CompletionCause::Cancelled
                    };
                }
                let _ = bound.child.request_group(CohortSignal::Kill);
            }
            if !bound.go_attempted && !cancelling {
                prepared.validate(operation)?;
                cohort.observation(fence)?;
                if root.child_mut().exited()? {
                    return Err(refused("archive Root closed before GO"));
                }
                let held = &prepared.held;
                if prepared.prelude.poll(bound.child.child_mut(), |pid| {
                    held.validate_loaded(
                        trust_resource_provenance::process::read_identity(pid)?,
                        deadline,
                    )
                })? {
                    let target = prepared
                        .prelude
                        .take_retirement()
                        .ok_or_else(|| refused("archive no-fork target unavailable"))?;
                    let matches = target.matches(
                        bound.identity.pid,
                        bound.identity.birth,
                        bound.child.cohort().group(),
                    );
                    bound.target = Some(target);
                    if !matches {
                        return Err(refused("archive no-fork target belongs to another child"));
                    }
                    prepared.handoff.bind_child(bound.identity.pid)?;
                    prepared.validate(operation)?;
                    journal.update(
                        &binding,
                        "member_bound",
                        aterm_json::json!({"pid":bound.identity.pid,"birth":bound.identity.birth}),
                    )?;
                    journal.update(&binding, "member_running", aterm_json::json!({}))?;
                    let (monotonic_ns, unix_ns) = start::clocks()?;
                    if root.child_mut().exited()? || bound.child.child_mut().exited()? {
                        return Err(refused("archive target/Root exited before GO"));
                    }
                    alive(deadline)?;
                    let at = Instant::now();
                    bound.go_attempted = true;
                    bound.child.child_mut().resume()?;
                    bound.started = Some((
                        at,
                        control::Started {
                            identity: bound.identity,
                            monotonic_ns,
                            unix_ns,
                        },
                    ));
                }
            }
            let exited = bound.child.child_mut().exited()?;
            if bound.go_attempted && !cancelling {
                prepared.poll_control(operation, exited)?;
            }
            if bound.waited.is_none()
                && exited
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
                    self.loan.as_mut().expect("same original loan"),
                )?));
            }
        }
        if let State::NoChild(slot) = &mut self.state
            && let Some(proof) = slot.take()
        {
            match cohort.cancel_loaned_subgroup(
                fence,
                *proof,
                self.loan.as_mut().expect("same original loan"),
            ) {
                Ok(closure) => self.closure = Some(closure),
                Err(failure) => {
                    let (error, original) = failure.into_parts();
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
                journal.settle(&self.binding())?;
                self.journal_settled = true;
            }
        }
        if self.closure.is_some() && !self.journal_settled {
            journal.update(&self.binding(), "member_terminal", aterm_json::json!({}))?;
            journal.settle(&self.binding())?;
            self.journal_settled = true;
        }
        if self.journal_settled
            && self.return_error.is_none()
            && let Some(closure) = self.closure.take()
        {
            match cohort.return_gnu_loan(
                fence,
                self.loan.as_mut().expect("same original loan"),
                closure,
            ) {
                Ok(()) => self.returned = true,
                Err(error) => {
                    self.return_error = Some(Box::new(error));
                    return Err(refused("archive original GNU return unknown"));
                }
            }
        }
        // Even after cancellation/deadline, physical EOF is independently
        // observable; it cannot be replaced by the child's status or report.
        if let Some(prepared) = &mut self.prepared {
            match prepared.control.peek()? {
                Peek::Eof => prepared.control_eof = true,
                Peek::Bytes if cancelling => {
                    prepared.control_eof = prepared.control.discard_to_eof()?
                }
                _ => {}
            }
        }
        if !self.returned || !self.journal_settled || self.failure.is_some() {
            return Ok(());
        }
        let State::Bound(bound) = &self.state else {
            return Ok(());
        };
        let (Some((started, observation)), Some(waited)) = (&bound.started, &bound.waited) else {
            return Ok(());
        };
        let prepared = self.prepared.as_mut().expect("same archive preparation");
        if !prepared.stdout.complete()
            || !prepared.stderr.complete()
            || !prepared.control_eof
            || !prepared.run_sent
        {
            return Ok(());
        }
        let operation = self.operation.as_ref().expect("same original archive");
        if self.delivery.is_none() {
            let outcome = if let Some(code) = waited.status.code() {
                control::Outcome::Exit(code)
            } else if let Some(signal) = waited.status.signal() {
                control::Outcome::Signal(signal)
            } else {
                return Err(refused("archive owning wait has no outcome"));
            };
            let ended = waited
                .ended
                .ok_or_else(|| refused("archive owning exit stamp unavailable"))?;
            let runtime_ns = u64::try_from(
                ended
                    .checked_duration_since(*started)
                    .ok_or_else(|| refused("archive runtime bookends differ"))?
                    .as_nanos(),
            )
            .map_err(|_| refused("archive runtime overflow"))?;
            let inputs = prepared.validate(operation).map_err(|e| e.to_string());
            self.inputs_unchanged = Some(inputs.clone());
            let terminal = wire::Terminal::Completed {
                completion: Box::new(control::MetadataCompletion {
                    finished: control::StatusFrame {
                        root_attempt: operation.request.key.graph.query.binding.root_attempt,
                        member_attempt: operation.request.key.operation,
                        status: control::Status::Finished {
                            outcome,
                            cause: bound.cause,
                            runtime_ns,
                        },
                    },
                    inputs_unchanged: inputs,
                }),
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
            let mut frames = vec![
                wire::Event::Prepared(Box::new(prepared.inventory.clone()))
                    .encode(&operation.request, prepared.digest)?,
                wire::Event::Started(*observation).encode(&operation.request, prepared.digest)?,
            ];
            for (capture, stream) in [
                (&prepared.stdout, wire::Stream::Stdout),
                (&prepared.stderr, wire::Stream::Stderr),
            ] {
                capture.append_frames(
                    &mut frames,
                    |offset, bytes| {
                        wire::Event::Output {
                            stream,
                            offset,
                            bytes,
                        }
                        .encode(&operation.request, prepared.digest)
                    },
                    |bytes| {
                        wire::Event::Eof { stream, bytes }
                            .encode(&operation.request, prepared.digest)
                    },
                )?;
            }
            frames.push(terminal.encode(&operation.request, prepared.digest)?);
            let ack = wire::Ack::encode(&operation.request, prepared.digest, &terminal)?;
            let stream = self
                .stream
                .take()
                .ok_or_else(|| refused("archive accepted stream missing"))?;
            self.delivery = Some(Delivery::new(stream, frames, ack)?);
        }
        self.acknowledged = self
            .delivery
            .as_mut()
            .expect("same archive delivery")
            .poll(deadline)?;
        if self.acknowledged {
            let check = prepared.validate(operation);
            self.inputs_unchanged = Some(check.as_ref().map(|_| ()).map_err(|e| e.to_string()));
            check?;
            if !waited.status.success() {
                operation.inputs.quarantine();
                self.failure = Some(refused("archive service exited unsuccessfully"));
            }
        }
        if self.acknowledged {
            self.check_root(root)?;
        }
        Ok(())
    }
    pub(in crate::actor::owner) fn into_retained(
        mut self: Box<Self>,
    ) -> Result<Retained, Box<Self>> {
        let no_launch = matches!(self.state, State::Unused) && self.loan.is_none();
        let native_closed = no_launch || (self.returned && self.return_error.is_none());
        let journal_closed = !self.journal_created || self.journal_settled;
        let eof = self
            .prepared
            .as_ref()
            .is_none_or(|p| p.stdout.at_eof() && p.stderr.at_eof() && p.control_eof);
        let success = self.acknowledged
            && self.failure.is_none()
            && self.inputs_unchanged == Some(Ok(()))
            && matches!(&self.state, State::Bound(b) if b.waited.as_ref().is_some_and(|w| w.status.success()));
        if !native_closed || !journal_closed || !eof || (!success && self.failure.is_none()) {
            return Err(self);
        }
        if !success {
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
        }
        let operation = self.operation.as_mut().expect("same original archive");
        #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
        if success {
            crate::actor::owner::tests::bootstrap_object_engineering::archive_oracle::observe(
                operation,
            );
        }
        let proof = NativeClosed {
            request: operation.request.clone(),
            context: operation.context.clone(),
        };
        if operation.close_work(&proof).is_err() {
            return Err(self);
        }
        self.closed = true;
        Ok(Retained {
            original: self,
            success,
        })
    }
}
impl Drop for Archive {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        if let Some(operation) = &self.operation {
            operation.inputs.quarantine();
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

#[cfg(all(
    test,
    target_pointer_width = "64",
    any(target_os = "macos", target_os = "linux")
))]
#[path = "execution_tests.rs"]
mod tests;
