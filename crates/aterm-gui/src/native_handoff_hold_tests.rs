// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A native restart's hold on the update's handoff ends at its close (ATERM-KI-1112-151), bound
//! to the `NativeHandoffHold` model by lockstep projection. The host's steps are real requests
//! and sweeps on the fake kernel; the update is booked beside it (a latched and completed
//! handoff), as are the clock's deadline and close, which the test moves. What the harness does
//! on a `closed` answer is `NativeClosedWatch`'s, bound in aterm-agent. An own-image move has no
//! close: one still open at its deadline ends Unknown there (`native_act_tests.rs`,
//! `an_expired_own_image_move_closes_unknown_before_releasing_the_update`).

use super::*;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;

const EPOCH_CHANGED: &str = "ERR native host epoch changed\n";

fn closed(id: &str) -> String {
    aterm_agent::harness::native_act::closed_reply(id).unwrap()
}

/// The model's end for a phase: none, committed, closed, or an end with every process accounted
/// for (a retirement, or Unknown for a grant that was never delivered).
fn outcome_of(phase: Phase) -> i64 {
    match phase {
        Phase::Completed(Decision::Committed) => 1,
        Phase::Completed(Decision::Closed) => 2,
        Phase::Completed(Decision::Retired(_) | Decision::Unknown) => 3,
        Phase::Completed(_) => panic!("not an end a granted restart reaches"),
        _ => 0,
    }
}

/// The real host under one signalled restart whose launcher is attached ([`super::issuer::ready`]),
/// and the history beside it the model keeps and the host does not.
pub(super) struct Host {
    handle: crate::session_store::SessionHandle,
    facts: Facts,
    binding: Binding,
    actor: SupervisorInputCaller,
    launcher: SupervisorInputCaller,
    request: Prepare,
    operation: Arc<Mutex<Operation>>,
    guard: std::sync::Weak<aterm_types::update_boundary::Held<'static>>,
    delivered: bool,
    outcome: i64,
    early: bool,
    late_commit: bool,
    latch: bool,
    epoch: i64,
    cut: bool,
    heard: bool,
}

impl Host {
    fn new(local: u64) -> Self {
        let (handle, facts, binding, actor, launcher) = super::issuer::ready(local);
        Self::on(handle, facts, binding, actor, launcher)
    }

    /// The real host under one restart signalled and never resumed, which past its deadline can
    /// start nothing more.
    fn signalled(local: u64) -> Self {
        let (handle, facts, request, actor) = closed_restart::fixture(local);
        let binding = admitted_binding(&run(
            &handle.ctx,
            &facts,
            Request::prepare(request),
            actor.clone(),
        ));
        assert_eq!(
            run(
                &handle.ctx,
                &facts,
                Request::Signal(binding.clone()),
                actor.clone()
            ),
            "OK native-act state=signalled\n"
        );
        Self::on(handle, facts, binding, actor.clone(), actor)
    }

    fn on(
        handle: crate::session_store::SessionHandle,
        facts: Facts,
        binding: Binding,
        actor: SupervisorInputCaller,
        launcher: SupervisorInputCaller,
    ) -> Self {
        let operation = lookup(&handle.ctx, &binding).unwrap();
        let (request, guard) = {
            let op = operation.lock().unwrap();
            let guard = op.custody.as_ref().unwrap().handoff_guard().unwrap();
            (op.request.clone(), Arc::downgrade(&guard))
        };
        Self {
            handle,
            facts,
            binding,
            actor,
            launcher,
            request,
            operation,
            guard,
            delivered: false,
            outcome: 0,
            early: false,
            late_commit: false,
            latch: false,
            epoch: 1,
            cut: false,
            heard: false,
        }
    }

    fn granted(&self) -> bool {
        matches!(
            self.facts
                .journal
                .lock()
                .unwrap()
                .get(&self.request.id)
                .map(|entry| entry.state),
            Some(JournalState::MayExecute | JournalState::Sealed { may_execute: true })
        )
    }

    fn held(&self) -> bool {
        self.guard.upgrade().is_some()
    }

    /// Project the host onto the model: the hold from the custody's guard, the grant from the
    /// issuer journal and the operation's append, the grantee from the kernel, the clock, the
    /// sealed end, and the history booked beside them.
    pub(super) fn probe(&self, model: &Model) -> State {
        let (writing, sent, phase) = {
            let op = self.operation.lock().unwrap();
            (
                matches!(op.grant_record, GrantRecord::Writing(_)),
                op.children.iter().flatten().any(|child| child.execute_sent),
                op.phase,
            )
        };
        let child = if !(sent || self.delivered) {
            0
        } else if self.facts.process(600).is_some() {
            1
        } else {
            2
        };
        let clock = self.facts.clock.load(Ordering::SeqCst);
        let mut state = model.init_state();
        for (var, value) in [
            ("held", i64::from(self.held())),
            ("writing", i64::from(writing)),
            ("granted", i64::from(self.granted())),
            ("child", child),
            ("late", i64::from(clock >= self.request.deadline_us)),
            ("closed", i64::from(clock >= self.request.close_us())),
            ("outcome", outcome_of(phase)),
            ("early", i64::from(self.early)),
            ("late_commit", i64::from(self.late_commit)),
            ("latch", i64::from(self.latch)),
            ("epoch", self.epoch),
            ("cut", i64::from(self.cut)),
            ("heard", i64::from(self.heard)),
            ("answer", 0),
        ] {
            state.insert(var, value);
        }
        state
    }

    /// Book what the host just did that it keeps no record of: a delivered grant (its child
    /// record goes with the seal), and when an end was sealed, which side of the close it was.
    fn observe(&mut self) {
        let (sent, phase) = {
            let op = self.operation.lock().unwrap();
            (
                op.children.iter().flatten().any(|child| child.execute_sent),
                op.phase,
            )
        };
        self.delivered |= sent;
        let outcome = outcome_of(phase);
        let closed = self.facts.clock.load(Ordering::SeqCst) >= self.request.close_us();
        if self.outcome == 0 && outcome == 2 {
            self.early = !closed;
        }
        if self.outcome == 0 && outcome == 1 {
            self.late_commit = closed;
        }
        self.outcome = outcome;
    }

    fn send(&mut self, request: Request, caller: &SupervisorInputCaller) -> String {
        let reply = run(&self.handle.ctx, &self.facts, request, caller.clone());
        self.observe();
        if reply == closed(&self.request.id) {
            self.heard = true;
        }
        reply
    }

    fn execute(&mut self) -> String {
        let (binding, launcher) = (self.binding.clone(), self.launcher.clone());
        self.send(Request::Execute(binding, ImagePhase::Forward), &launcher)
    }

    fn precommit(&mut self) -> String {
        let (binding, actor) = (self.binding.clone(), self.actor.clone());
        self.send(Request::Precommit(binding, ImagePhase::Forward), &actor)
    }

    fn commit(&mut self) -> String {
        let (binding, actor) = (self.binding.clone(), self.actor.clone());
        self.send(
            Request::Commit(binding, ImagePhase::Forward, "0.1.0+g123".into()),
            &actor,
        )
    }

    fn query(&mut self) -> String {
        let (binding, actor) = (self.binding.clone(), self.actor.clone());
        self.send(Request::Query(binding), &actor)
    }

    fn sweep(&mut self) {
        reconcile_extinct_by(&self.handle.ctx, &self.facts);
        self.observe();
    }

    /// The grantee runs the forward image its Precommit and Commit read.
    fn forward_image(&self) {
        self.facts
            .processes
            .lock()
            .unwrap()
            .get_mut(&600)
            .unwrap()
            .image = image(9);
    }

    fn at(&self, clock: u64) {
        self.facts.clock.store(clock, Ordering::SeqCst);
    }

    /// The model takes `actions`, and the projection must be where they went.
    fn follow(&self, model: &Model, state: &mut State, actions: &[&str]) {
        for action in actions {
            assert!(model.fire(action, state), "{action} from {state:?}");
        }
        assert_eq!(self.probe(model), *state, "after {actions:?}");
    }

    /// Drive each premise on the fake kernel or the books, in lockstep with the model.
    fn steps(&mut self, model: &Model, state: &mut State, actions: &[&str]) {
        for action in actions {
            match *action {
                "Expire" => self.at(self.request.deadline_us),
                "PassClose" => self.at(self.request.close_us()),
                "ChildExits" => {
                    self.facts.processes.lock().unwrap().remove(&600);
                }
                "Latch" => self.latch = true,
                "Handoff" => {
                    assert!(
                        !self.held(),
                        "the handoff goes on only once nothing holds it"
                    );
                    self.cut = self.outcome == 0
                        && (self.granted()
                            || matches!(
                                self.operation.lock().unwrap().grant_record,
                                GrantRecord::Writing(_)
                            ));
                    self.epoch = 2;
                }
                other => unreachable!("{other} is not a premise"),
            }
            self.follow(model, state, &[action]);
        }
    }

    fn holds(&self, model: &Model, state: &State) {
        for law in &model.invariants {
            assert!(model.check_invariant(law.name, state), "{}", law.name);
        }
    }
}

/// The real successor host for one cold restart an earlier epoch minted and journaled: it
/// answers a replay of the prepare from the journal and the boot's clock. The old host's steps
/// are booked; the grant is the journal's.
pub(super) struct Successor {
    handle: crate::session_store::SessionHandle,
    facts: Facts,
    request: Prepare,
    actor: SupervisorInputCaller,
    held: bool,
    writing: bool,
    child: i64,
    outcome: i64,
    latch: bool,
    epoch: i64,
    answer: bool,
}

impl Successor {
    fn new(local: u64) -> Self {
        let (handle, facts, mut request, actor) = closed_restart::fixture(local);
        cold_restart(&mut request);
        request.host_epoch = "34".repeat(16);
        assert_ne!(host_epoch(), Some(request.host_epoch.as_str()));
        let successor = Self {
            handle,
            facts,
            request,
            actor,
            held: true,
            writing: false,
            child: 0,
            outcome: 0,
            latch: false,
            epoch: 1,
            answer: false,
        };
        // The earlier epoch admitted it and recorded the prepare.
        assert!(
            successor
                .facts
                .record_prepared(&successor.request, &successor.binding())
        );
        successor
    }

    fn binding(&self) -> Binding {
        Binding {
            id: self.request.id.clone(),
            capability: "cd".repeat(32),
        }
    }

    fn granted(&self) -> bool {
        matches!(
            self.facts
                .journal
                .lock()
                .unwrap()
                .get(&self.request.id)
                .map(|entry| entry.state),
            Some(JournalState::MayExecute | JournalState::Sealed { may_execute: true })
        )
    }

    /// Project the successor onto the model: the grant from the journal, the clock, and the old
    /// host's and the harness's history booked beside them.
    pub(super) fn probe(&self, model: &Model) -> State {
        let clock = self.facts.clock.load(Ordering::SeqCst);
        let mut state = model.init_state();
        for (var, value) in [
            ("held", i64::from(self.held)),
            ("writing", i64::from(self.writing)),
            ("granted", i64::from(self.granted())),
            ("child", self.child),
            ("late", i64::from(clock >= self.request.deadline_us)),
            ("closed", i64::from(clock >= self.request.close_us())),
            ("outcome", self.outcome),
            ("latch", i64::from(self.latch)),
            ("epoch", self.epoch),
            ("answer", i64::from(self.answer)),
        ] {
            state.insert(var, value);
        }
        state
    }

    /// One real replay of the prepare to the successor, booked with the answer it gave.
    fn replay(&mut self) -> String {
        let reply = run(
            &self.handle.ctx,
            &self.facts,
            Request::prepare(self.request.clone()),
            self.actor.clone(),
        );
        if reply == closed(&self.request.id) {
            self.answer = true;
        }
        reply
    }

    fn steps(&mut self, model: &Model, state: &mut State, actions: &[&str]) {
        for action in actions {
            let (prepare, binding) = (self.request.clone(), self.binding());
            match *action {
                "Reserve" => self.writing = true,
                "GrantAck" => {
                    assert!(self.facts.record_may_execute(&prepare, &binding));
                    self.writing = false;
                }
                "Deliver" => self.child = 1,
                "ChildExits" => self.child = 2,
                "Expire" => self
                    .facts
                    .clock
                    .store(self.request.deadline_us, Ordering::SeqCst),
                "PassClose" => self
                    .facts
                    .clock
                    .store(self.request.close_us(), Ordering::SeqCst),
                // The old host's ends, each published as a seal after possible execution.
                "Commit" | "Sweep" => {
                    assert!(self.facts.record_sealed(&prepare, &binding));
                    self.outcome = if *action == "Commit" { 1 } else { 2 };
                    self.held = false;
                }
                "Latch" => self.latch = true,
                "Handoff" => self.epoch = 2,
                other => unreachable!("{other} is not a premise"),
            }
            assert!(model.fire(action, state), "{action} from {state:?}");
            assert_eq!(self.probe(model), *state, "after {action}");
        }
    }
}

/// THE FIX: a restart that granted Execute and never committed holds the update's handoff
/// through its deadline and its grace, and at its close the sweep seals it `closed`, which
/// drops the hold; every later reply answers `closed`, published once as a seal after possible
/// execution.
#[test]
fn a_granted_restart_past_its_close_seals_closed_and_lets_the_update_through() {
    let model = aterm_models_harness::native_handoff_hold_model();
    let mut host = Host::new(8510);
    let mut state = model.init_state();
    assert_eq!(host.probe(&model), state);
    assert!(matches!(
        Reply::parse(&host.execute()),
        Some(Reply::Execute { .. })
    ));
    host.follow(&model, &mut state, &["Reserve", "GrantAck", "Deliver"]);
    let (deadline, close) = (host.request.deadline_us, host.request.close_us());
    assert_eq!(
        close,
        deadline + aterm_agent::harness::native_act::COMPLETION_US
    );
    host.at(deadline - 1);
    host.sweep();
    host.follow(&model, &mut state, &[]);
    host.steps(&model, &mut state, &["Expire"]);
    for clock in [deadline, deadline + 1, close - 1] {
        host.at(clock);
        host.sweep();
        host.follow(&model, &mut state, &[]);
        assert!(host.held(), "the hold stays before the close ({clock})");
    }
    host.steps(&model, &mut state, &["PassClose"]);
    host.sweep();
    host.follow(&model, &mut state, &["Sweep"]);
    assert!(!host.held(), "at its close the update's handoff goes on");
    {
        let op = host.operation.lock().unwrap();
        assert_eq!(op.phase, Phase::Completed(Decision::Closed));
        assert!(op.custody.is_none());
    }
    assert_eq!(
        host.facts.journal.lock().unwrap()[&host.request.id].state,
        JournalState::MayExecute,
        "the sweep publishes nothing"
    );
    let id = host.request.id.clone();
    assert_eq!(host.query(), closed(&id));
    host.follow(&model, &mut state, &["Hear"]);
    assert_eq!(host.facts.sealed_history.lock().unwrap().len(), 1);
    assert_eq!(
        host.facts.journal.lock().unwrap()[&id].state,
        JournalState::Sealed { may_execute: true }
    );
    let (binding, actor) = (host.binding.clone(), host.actor.clone());
    for request in [
        Request::Commit(binding.clone(), ImagePhase::Forward, "0.1.0+g123".into()),
        Request::Precommit(binding.clone(), ImagePhase::Forward),
        Request::Recover(binding.clone()),
        Request::Reconcile(binding.clone()),
        Request::Query(binding),
    ] {
        assert_eq!(host.send(request, &actor), closed(&id));
    }
    assert_eq!(host.facts.sealed_history.lock().unwrap().len(), 1);
    host.steps(&model, &mut state, &["Latch", "Handoff", "ChildExits"]);
    host.holds(&model, &state);
}

/// A slow commit inside the grace still commits: the close bounds Commit, not the deadline.
#[test]
fn a_slow_commit_inside_the_grace_still_commits() {
    let model = aterm_models_harness::native_handoff_hold_model();
    let mut host = Host::new(8511);
    let mut state = model.init_state();
    assert!(matches!(
        Reply::parse(&host.execute()),
        Some(Reply::Execute { .. })
    ));
    host.follow(&model, &mut state, &["Reserve", "GrantAck", "Deliver"]);
    host.forward_image();
    host.steps(&model, &mut state, &["Expire"]);
    host.at(host.request.close_us() - 1);
    host.sweep();
    host.follow(&model, &mut state, &[]);
    assert_eq!(host.precommit(), "OK native-act state=precommitted\n");
    let reply = host.commit();
    assert!(
        reply.starts_with("OK native-act state=committed "),
        "{reply}"
    );
    host.follow(&model, &mut state, &["Commit"]);
    assert!(!host.held());
    host.steps(&model, &mut state, &["Latch", "Handoff"]);
    host.holds(&model, &state);
}

/// A Commit that arrives at the close is answered `closed`, never committed; so are a Precommit
/// and a Recover, and one that reads the clock pass the close during its own kernel reads seals
/// `closed` at the seal, once.
#[test]
fn a_late_commit_after_the_close_is_refused_closed() {
    let model = aterm_models_harness::native_handoff_hold_model();
    for (case, request) in ["commit", "precommit", "recover", "race"]
        .into_iter()
        .enumerate()
    {
        let mut host = Host::new(8512 + case as u64);
        let mut state = model.init_state();
        assert!(matches!(
            Reply::parse(&host.execute()),
            Some(Reply::Execute { .. })
        ));
        host.follow(&model, &mut state, &["Reserve", "GrantAck", "Deliver"]);
        host.forward_image();
        assert_eq!(host.precommit(), "OK native-act state=precommitted\n");
        host.steps(&model, &mut state, &["Expire"]);
        let id = host.request.id.clone();
        let (binding, actor) = (host.binding.clone(), host.actor.clone());
        let reply = match request {
            "race" => {
                // The Commit passes the entry fence before the close, and its image reads
                // outlast it: the seal reads the clock again under its locks.
                host.at(host.request.close_us() - 1);
                *host.facts.clock_jump.lock().unwrap() = Some((1, host.request.close_us()));
                let reply = host.commit();
                assert!(host.facts.clock_jump.lock().unwrap().is_none());
                host.follow(&model, &mut state, &["PassClose", "Sweep", "Hear"]);
                reply
            }
            _ => {
                host.steps(&model, &mut state, &["PassClose"]);
                let reply = match request {
                    "commit" => host.commit(),
                    "precommit" => host.precommit(),
                    _ => host.send(Request::Recover(binding.clone()), &actor),
                };
                host.follow(&model, &mut state, &["Sweep", "Hear"]);
                reply
            }
        };
        assert_eq!(reply, closed(&id), "{request}");
        assert!(!host.held(), "{request}");
        host.sweep();
        host.follow(&model, &mut state, &[]);
        assert_eq!(
            host.handle
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .completed
                .iter()
                .filter(|done| **done == id)
                .count(),
            1,
            "{request}: finished once"
        );
        assert_eq!(host.commit(), closed(&id), "{request}");
        assert_eq!(host.precommit(), closed(&id), "{request}");
        host.follow(&model, &mut state, &[]);
        host.holds(&model, &state);
    }
}

/// A grant whose journal append is in flight at the close is not closed until the append is a
/// fact; then a grant recorded past the deadline, never delivered, closes like any other.
#[test]
fn a_grant_append_in_flight_at_the_close_holds_until_it_is_a_fact() {
    let model = aterm_models_harness::native_handoff_hold_model();
    let mut host = Host::new(8516);
    let (entered, waiting) = mpsc::sync_channel(1);
    let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    host.facts.grant_journal_gate = Some((entered, Arc::clone(&proceed)));
    let mut state = model.init_state();
    let (ctx, binding, launcher) = (
        Arc::clone(&host.handle.ctx),
        host.binding.clone(),
        host.launcher.clone(),
    );
    let reply = std::thread::scope(|scope| {
        let facts = &host.facts;
        let worker = scope.spawn(move || {
            run(
                &ctx,
                facts,
                Request::Execute(binding, ImagePhase::Forward),
                launcher,
            )
        });
        waiting
            .recv_timeout(PATIENCE)
            .expect("the grant's append began");
        let mut state_now = state.clone();
        host.follow(&model, &mut state_now, &["Reserve"]);
        host.at(host.request.close_us());
        assert!(model.fire("Expire", &mut state_now));
        assert!(model.fire("PassClose", &mut state_now));
        reconcile_extinct_by(&host.handle.ctx, &host.facts);
        assert_eq!(
            host.probe(&model),
            state_now,
            "the close waits on the append"
        );
        assert!(host.held());
        state = state_now;
        *proceed.0.lock().unwrap() = true;
        proceed.1.notify_all();
        worker.join().unwrap()
    });
    assert!(
        !matches!(Reply::parse(&reply), Some(Reply::Execute { .. })),
        "nothing is delivered past the deadline: {reply}"
    );
    host.observe();
    host.follow(&model, &mut state, &["GrantAck"]);
    host.sweep();
    host.follow(&model, &mut state, &["Sweep"]);
    assert!(!host.held());
    let id = host.request.id.clone();
    assert_eq!(host.query(), closed(&id));
    host.follow(&model, &mut state, &["Hear"]);
    host.steps(&model, &mut state, &["Latch", "Handoff"]);
    host.holds(&model, &state);
}

/// An input attempt that may have written a byte and can write no more (sealed, idle, uncertain)
/// does not keep the hold past the close; one that can still write does. An uncertain grant
/// append, never delivered, closes too: the model's Sweep from no recorded grant, whose seal
/// lands after Prepared, so the journal says no execution was possible.
#[test]
fn an_idle_uncertain_input_and_an_uncertain_grant_close_at_the_close() {
    let model = aterm_models_harness::native_handoff_hold_model();
    for case in 0..3 {
        let mut host = Host::new(8517 + case);
        let mut state = model.init_state();
        if case < 2 {
            assert!(matches!(
                Reply::parse(&host.execute()),
                Some(Reply::Execute { .. })
            ));
            host.follow(&model, &mut state, &["Reserve", "GrantAck", "Deliver"]);
            let attempt = host
                .operation
                .lock()
                .unwrap()
                .resume_attempt
                .clone()
                .unwrap();
            let mut input = attempt.state.lock().unwrap();
            input.uncertain = true;
            if case == 1 {
                input.pending = 1;
            }
        } else {
            host.facts
                .grant_journal_allowed
                .store(false, Ordering::SeqCst);
            assert_eq!(host.execute(), UNKNOWN);
            // A failed append records no grant and delivers none: no step.
            host.follow(&model, &mut state, &[]);
            assert!(matches!(
                host.operation.lock().unwrap().grant_record,
                GrantRecord::Uncertain(_)
            ));
        }
        host.steps(&model, &mut state, &["Expire", "PassClose"]);
        host.sweep();
        let phase = host.operation.lock().unwrap().phase;
        if case == 1 {
            host.follow(&model, &mut state, &[]);
            assert!(
                host.held(),
                "an attempt that may still write keeps the hold"
            );
            assert!(!matches!(phase, Phase::Completed(_)));
        } else {
            host.follow(&model, &mut state, &["Sweep"]);
            assert!(!host.held(), "case {case}");
            assert_eq!(phase, Phase::Completed(Decision::Closed), "case {case}");
            let id = host.request.id.clone();
            assert_eq!(host.query(), closed(&id), "case {case}");
            host.follow(&model, &mut state, &["Hear"]);
            assert_eq!(
                host.facts.journal.lock().unwrap()[&id].state,
                JournalState::Sealed {
                    may_execute: case == 0
                },
                "case {case}: the close is published as the journal recorded it"
            );
        }
        host.holds(&model, &state);
    }
}

/// A restart signalled and never resumed stops holding the handoff at its deadline with nothing
/// sealed, its close seals nothing, and once its source is gone it retires NotGranted: the
/// model's ReleaseUngranted and Retire, each a real sweep projected in lockstep.
#[test]
fn an_ungranted_restart_releases_at_its_deadline_and_retires_once_its_source_is_gone() {
    let model = aterm_models_harness::native_handoff_hold_model();
    let mut host = Host::signalled(8545);
    let mut state = model.init_state();
    assert_eq!(host.probe(&model), state);
    host.sweep();
    host.follow(&model, &mut state, &[]);
    assert!(host.held(), "before its deadline it holds");
    host.steps(&model, &mut state, &["Expire"]);
    host.sweep();
    host.follow(&model, &mut state, &["ReleaseUngranted"]);
    host.steps(&model, &mut state, &["PassClose"]);
    host.sweep();
    host.follow(&model, &mut state, &[]);
    assert!(!matches!(
        host.operation.lock().unwrap().phase,
        Phase::Completed(_)
    ));
    host.facts.processes.lock().unwrap().remove(&4242);
    host.sweep();
    host.follow(&model, &mut state, &["Retire"]);
    assert_eq!(
        host.operation.lock().unwrap().phase,
        Phase::Completed(Decision::Retired(Retirement::NotGranted))
    );
    host.steps(&model, &mut state, &["Latch", "Handoff"]);
    host.holds(&model, &state);
}

/// A granted restart whose processes all exit before its close retires Exited at the first
/// sweep past its deadline, the model's Retire, and never waits for its close.
#[test]
fn a_granted_restart_whose_processes_exit_retires_before_its_close() {
    let model = aterm_models_harness::native_handoff_hold_model();
    let mut host = Host::new(8546);
    let mut state = model.init_state();
    assert!(matches!(
        Reply::parse(&host.execute()),
        Some(Reply::Execute { .. })
    ));
    host.follow(&model, &mut state, &["Reserve", "GrantAck", "Deliver"]);
    host.steps(&model, &mut state, &["Expire"]);
    host.sweep();
    host.follow(&model, &mut state, &[]);
    assert!(host.held(), "its grantee runs");
    host.steps(&model, &mut state, &["ChildExits"]);
    host.sweep();
    host.follow(&model, &mut state, &["Retire"]);
    assert_eq!(
        host.operation.lock().unwrap().phase,
        Phase::Completed(Decision::Retired(Retirement::Exited))
    );
    host.steps(&model, &mut state, &["Latch", "Handoff"]);
    host.holds(&model, &state);
}

/// A restart whose signal the kernel refused, whose grantee ran and exited, and whose rollback
/// Resume was refused after it reset the Resume phase, took effect: its grant record says so. It
/// is never read as one that took none, whose rollback-safe seal it cannot take and whose sweep
/// skipped it for good; it retires, or, where its grantee's group does not read gone, closes at
/// its close, and either way stops holding the handoff.
#[test]
fn a_granted_restart_whose_rollback_resume_was_refused_still_retires_or_closes() {
    let model = aterm_models_harness::native_handoff_hold_model();
    for retires in [true, false] {
        let (handle, facts, binding, actor, launcher) =
            super::issuer::ready_after_signal(8548 + u64::from(retires), false);
        let mut host = Host::on(handle, facts, binding, actor, launcher);
        let mut state = model.init_state();
        assert!(matches!(
            Reply::parse(&host.execute()),
            Some(Reply::Execute { .. })
        ));
        host.follow(&model, &mut state, &["Reserve", "GrantAck", "Deliver"]);
        host.steps(&model, &mut state, &["ChildExits"]);
        let (binding, actor) = (host.binding.clone(), host.actor.clone());
        let seq = host.handle.ctx.human_input.seq();
        assert_eq!(
            host.send(
                Request::Resume(binding.clone(), ImagePhase::Rollback, "1.2".into(), seq),
                &actor
            ),
            DENIED
        );
        host.follow(&model, &mut state, &[]);
        {
            let op = host.operation.lock().unwrap();
            assert_eq!(op.signal_accepted, Some(false));
            assert_eq!(op.resume_phase, Some(ImagePhase::Rollback));
            assert!(!op.resume_sent && op.resume_attempt.is_none());
            assert_eq!(op.grant_record, GrantRecord::Recorded);
        }
        if !retires {
            host.facts
                .group_observations
                .lock()
                .unwrap()
                .insert(600, None);
        }
        host.steps(&model, &mut state, &["Expire"]);
        host.sweep();
        let id = host.request.id.clone();
        if retires {
            host.follow(&model, &mut state, &["Retire"]);
            assert!(
                !host.held(),
                "it retires at the first sweep past its deadline"
            );
            assert_eq!(
                host.operation.lock().unwrap().phase,
                Phase::Completed(Decision::Retired(Retirement::Exited))
            );
        } else {
            host.follow(&model, &mut state, &[]);
            assert!(host.held(), "before its close an unread group holds");
            host.steps(&model, &mut state, &["PassClose"]);
            host.sweep();
            host.follow(&model, &mut state, &["Sweep"]);
            assert!(!host.held(), "at its close the handoff goes on");
            assert_eq!(
                host.operation.lock().unwrap().phase,
                Phase::Completed(Decision::Closed)
            );
            assert_eq!(host.send(Request::Reconcile(binding), &actor), closed(&id));
            host.follow(&model, &mut state, &["Hear"]);
        }
        host.steps(&model, &mut state, &["Latch", "Handoff"]);
        host.holds(&model, &state);
    }
}

/// A successor host answers a closed restart `closed` from the journal once the close has
/// passed on the boot's clock, and nothing before it: a grant the old host committed, or one it
/// sealed `closed`, both published as a seal after possible execution.
#[test]
fn a_successor_answers_closed_from_the_journal_past_the_close_and_nothing_before() {
    let model = aterm_models_harness::native_handoff_hold_model();
    for (case, before) in [
        &[
            "Reserve", "GrantAck", "Deliver", "Commit", "Latch", "Handoff",
        ][..],
        &[
            "Reserve",
            "GrantAck",
            "Deliver",
            "Expire",
            "PassClose",
            "Sweep",
            "Latch",
            "Handoff",
        ][..],
    ]
    .into_iter()
    .enumerate()
    {
        let mut host = Successor::new(8530 + case as u64);
        let mut state = model.init_state();
        assert_eq!(host.probe(&model), state);
        host.steps(&model, &mut state, before);
        let id = host.request.id.clone();
        if case == 0 {
            for step in [None, Some("Expire")] {
                if let Some(step) = step {
                    host.steps(&model, &mut state, &[step]);
                }
                let unread = state.clone();
                assert_eq!(host.replay(), EPOCH_CHANGED, "{step:?}");
                assert!(!model.action_enabled("Answer", &state));
                assert_eq!(host.probe(&model), state);
                if step.is_some() {
                    // Negative control: a successor that answered `closed` here, before the
                    // close, is a step the model refuses.
                    host.answer = true;
                    let bad = host.probe(&model);
                    assert!(!model.check_invariant("ClosedAnswerOnlyPastClose", &bad));
                    assert!(!model.successors("Answer", &unread).contains(&bad));
                    host.answer = false;
                }
            }
            host.steps(&model, &mut state, &["PassClose"]);
        }
        assert_eq!(host.replay(), closed(&id), "case {case}");
        assert!(model.fire("Answer", &mut state));
        assert_eq!(host.probe(&model), state);
        host.steps(&model, &mut state, &["ChildExits"]);
        for law in &model.invariants {
            assert!(model.check_invariant(law.name, &state), "{}", law.name);
        }
    }
}

/// Negative control for the hold: a host that sealed `closed` before the close, or released a
/// granted operation's hold with nothing sealed, takes a step the model refuses.
#[test]
fn a_close_before_the_close_or_a_bare_release_is_refused_by_the_model() {
    let model = aterm_models_harness::native_handoff_hold_model();
    for early in [true, false] {
        let mut host = Host::new(8540 + u64::from(early));
        let mut state = model.init_state();
        assert!(matches!(
            Reply::parse(&host.execute()),
            Some(Reply::Execute { .. })
        ));
        host.follow(&model, &mut state, &["Reserve", "GrantAck", "Deliver"]);
        host.steps(&model, &mut state, &["Expire"]);
        let bad = if early {
            let completion = host
                .operation
                .lock()
                .unwrap()
                .seal_completion(Decision::Closed);
            let _ = finish(&host.handle.ctx, &host.operation, completion);
            host.observe();
            host.probe(&model)
        } else {
            host.operation
                .lock()
                .unwrap()
                .custody
                .as_ref()
                .unwrap()
                .release_handoff_guard();
            host.probe(&model)
        };
        assert!(!host.held());
        let law = if early {
            "NoCloseBeforeClose"
        } else {
            "ReleaseOnlyAnswerable"
        };
        assert!(!model.check_invariant(law, &bad), "{law}");
        for action in ["Sweep", "ReleaseUngranted", "Commit", "Retire"] {
            assert!(!model.successors(action, &state).contains(&bad), "{action}");
        }
    }
}
