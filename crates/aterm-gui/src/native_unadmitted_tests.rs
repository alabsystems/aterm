// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's `unadmitted` answer to an expired restart prepare, bound to the
//! `NativeUnadmittedPrepare` model by lockstep projection. The template is the
//! `NativeRestartSlots` model and its lockstep test (`native_restart_slots_tests.rs`).

use super::*;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use std::sync::Condvar;

type Release = Arc<(Mutex<bool>, Condvar)>;

fn gate() -> (mpsc::SyncSender<()>, mpsc::Receiver<()>, Release) {
    let (entered, waiting) = mpsc::sync_channel(1);
    (
        entered,
        waiting,
        Arc::new((Mutex::new(false), Condvar::new())),
    )
}

fn release(gate: &Release) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

fn fire(model: &Model, state: &mut State, actions: &[&str]) {
    for action in actions {
        assert!(model.fire(action, state), "{action} from {state:?}");
    }
}

/// The real host under one built-in restart prepare, and the history the model keeps of it.
pub(super) struct Host {
    handle: crate::session_store::SessionHandle,
    facts: Facts,
    request: Prepare,
    actor: SupervisorInputCaller,
    published: bool,
    answered: bool,
    admitted: bool,
    stale: bool,
    early: bool,
}

impl Host {
    /// A built-in restart prepare whose deadline is still too far for admission.
    fn new(local: u64, configure: impl FnOnce(&mut Facts)) -> Self {
        let (handle, mut facts, mut request, actor) = closed_restart::fixture(local);
        configure(&mut facts);
        request.deadline_us = facts.clock.load(Ordering::SeqCst) + MAX_PREPARE_US + 1_000_000;
        Self {
            handle,
            facts,
            request,
            actor,
            published: false,
            answered: false,
            admitted: false,
            stale: false,
            early: false,
        }
    }

    fn unadmitted(&self) -> String {
        aterm_agent::harness::native_act::unadmitted_reply(&self.request.id).unwrap()
    }

    /// Project the host onto the model: its clock against the deadline, the id's place in the
    /// operation table, its issuer record, and the answers it gave.
    pub(super) fn probe(&self, model: &Model) -> State {
        let now = self.facts.clock.load(Ordering::SeqCst);
        let deadline = match self.request.deadline_us.checked_sub(now) {
            None | Some(0) => 2,
            Some(left) if left > MAX_PREPARE_US => 0,
            Some(_) => 1,
        };
        let op = {
            let table = self.handle.ctx.fenced_writes.native.lock().unwrap();
            if table.reservations.contains_key(&self.request.id) {
                1
            } else if let Some(operation) = table.by_id.get(&self.request.id) {
                let op = operation.lock().unwrap();
                match op.phase {
                    Phase::Queued if op.preparation.recorded => 3,
                    Phase::Queued => 2,
                    Phase::Completed(_) => 5,
                    Phase::Started | Phase::Precommitted(_) | Phase::Committing(_) => 4,
                }
            } else if self.published {
                6
            } else {
                0
            }
        };
        let mut state = model.init_state();
        for (var, value) in [
            ("deadline", deadline),
            ("op", op),
            (
                "journal",
                i64::from(self.facts.recorded_operation(&self.request.id) == Ok(true)),
            ),
            (
                "own",
                i64::from(matches!(self.request.plan, NativePlan::OwnImage { .. })),
            ),
            ("answered", i64::from(self.answered)),
            ("admitted", i64::from(self.admitted)),
            ("stale", i64::from(self.stale)),
            ("early", i64::from(self.early)),
        ] {
            state.insert(var, value);
        }
        state
    }

    /// Book one reply the host gave from the projected state `before`.
    fn note(&mut self, before: &State, reply: &str) {
        match Reply::parse(reply) {
            Some(Reply::Unadmitted(id)) => {
                assert_eq!(id, self.request.id);
                self.answered = true;
                self.stale |= !matches!(before["op"], 0 | 6);
                self.early |= before["deadline"] != 2;
            }
            Some(Reply::Admitted(_)) => self.admitted = true,
            _ => {}
        }
        self.published |= self
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .contains_key(&self.request.id);
    }

    fn send(&mut self, model: &Model) -> String {
        let before = self.probe(model);
        let reply = run(
            &self.handle.ctx,
            &self.facts,
            Request::prepare(self.request.clone()),
            self.actor.clone(),
        );
        self.note(&before, &reply);
        reply
    }

    fn approach(&self) {
        self.facts.clock.fetch_add(2_000_000, Ordering::SeqCst);
    }

    fn expire(&self) {
        self.facts
            .clock
            .store(self.request.deadline_us, Ordering::SeqCst);
    }

    fn reclaim(&self) {
        let now = self.facts.clock.load(Ordering::SeqCst);
        let retired = self
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .reclaim(now);
        drop(retired);
    }
}

#[test]
fn an_expired_restart_prepare_the_host_never_held_is_answered_unadmitted_without_effects() {
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    let mut state = model.init_state();
    let mut host = Host::new(7960, |_| {});
    assert_eq!(host.probe(&model), state);
    // Too far is not expired: it is refused, and it changes nothing.
    assert!(!model.action_enabled("Answer", &state));
    assert_eq!(host.send(&model), DEADLINE);
    assert_eq!(host.probe(&model), state);
    host.approach();
    fire(&model, &mut state, &["Approach"]);
    assert_eq!(host.probe(&model), state);
    // Negative control: the same proof asked in the window, past the caller's deadline arm, says
    // `unadmitted`; booked through the projection, that answer breaks OnlyExpired.
    let early = unadmitted_after_deadline(
        &host.handle.ctx,
        &host.request,
        &host.actor,
        None,
        None,
        &host.facts,
    );
    host.note(&state, &early);
    assert!(!model.check_invariant("OnlyExpired", &host.probe(&model)));
    (host.answered, host.early) = (false, false);
    assert_eq!(host.probe(&model), state);
    host.expire();
    fire(&model, &mut state, &["Expire"]);
    assert_eq!(host.probe(&model), state);
    // The answer passes through the terminal publication boundary, which owns no receipt for
    // it, and every replay gets it again.
    for _ in 0..2 {
        assert_eq!(host.send(&model), host.unadmitted());
        fire(&model, &mut state, &["Answer"]);
        assert_eq!(host.probe(&model), state);
    }
    for law in &model.invariants {
        assert!(model.check_invariant(law.name, &state), "{}", law.name);
    }
    let table = host.handle.ctx.fenced_writes.native.lock().unwrap();
    assert!(table.by_id.is_empty() && table.reservations.is_empty());
    assert!(table.cancelled.is_empty() && table.receipt_flights.is_empty());
    drop(table);
    for history in [
        &host.facts.prepared_history,
        &host.facts.receipt_history,
        &host.facts.sealed_history,
    ] {
        assert!(history.lock().unwrap().is_empty());
    }
    assert_eq!(host.facts.watch_count.load(Ordering::SeqCst), 0);
    assert_eq!(host.facts.signals.load(Ordering::SeqCst), 0);
    assert_eq!(host.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn an_admitted_restart_keeps_the_deadline_refusal_after_completion_and_reclaim() {
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    for sealed in [false, true] {
        let mut state = model.init_state();
        let mut host = Host::new(7961 + u64::from(sealed), |_| {});
        host.approach();
        fire(&model, &mut state, &["Approach"]);
        let binding = admitted_binding(&host.send(&model));
        fire(
            &model,
            &mut state,
            &["Reserve", "Publish", "Record", "Start"],
        );
        assert_eq!(host.probe(&model), state);
        let ctx = &host.handle.ctx;
        if sealed {
            assert_eq!(
                run(
                    ctx,
                    &host.facts,
                    Request::Abort(binding),
                    host.actor.clone()
                ),
                ROLLBACK_SAFE
            );
        } else {
            assert_eq!(
                run(
                    ctx,
                    &host.facts,
                    Request::Signal(binding.clone()),
                    host.actor.clone()
                ),
                "OK native-act state=signalled\n"
            );
            host.facts.processes.lock().unwrap().remove(&4242);
            host.facts.foreground.store(400, Ordering::SeqCst);
            assert_eq!(
                run(
                    ctx,
                    &host.facts,
                    Request::Reconcile(binding),
                    host.actor.clone()
                ),
                NOEXEC
            );
        }
        fire(&model, &mut state, &["Complete"]);
        assert_eq!(host.probe(&model), state);
        host.expire();
        fire(&model, &mut state, &["Expire"]);
        host.reclaim();
        fire(&model, &mut state, &["Reclaim"]);
        assert_eq!(host.probe(&model), state, "sealed={sealed}");
        // The table forgot the id; its issuer record keeps the refusal. A positive NoExec
        // receipt is still served as one.
        assert!(!model.action_enabled("Answer", &state));
        assert_eq!(
            host.send(&model),
            if sealed { DEADLINE } else { NOEXEC },
            "sealed={sealed}"
        );
        assert_eq!(host.probe(&model), state);
        // Negative control: with the issuer record gone, the same host answers `unadmitted`
        // for an operation it admitted, which the projection refuses.
        host.facts.prepared_history.lock().unwrap().clear();
        host.facts.receipt_history.lock().unwrap().clear();
        assert_eq!(host.send(&model), host.unadmitted());
        let bad = host.probe(&model);
        assert!(!model.check_invariant("NeverAdmitted", &bad));
        assert!(!model.successors("Answer", &state).contains(&bad));
    }
}

/// Answer the expired replay from the host's proof with the id hidden from the table, as the
/// replay's first look at it saw: the negative control for reading it again after the clock.
fn early_view(host: &Host) -> String {
    let id = &host.request.id;
    let (held, reserved) = {
        let mut table = host.handle.ctx.fenced_writes.native.lock().unwrap();
        (table.by_id.remove(id), table.reservations.remove(id))
    };
    let answer = unadmitted_after_deadline(
        &host.handle.ctx,
        &host.request,
        &host.actor,
        None,
        None,
        &host.facts,
    );
    let mut table = host.handle.ctx.fenced_writes.native.lock().unwrap();
    if let Some(operation) = held {
        table.by_id.insert(id.clone(), operation);
    }
    if let Some(relation) = reserved {
        table.reservations.insert(id.clone(), relation);
    }
    answer
}

#[test]
fn an_expired_replay_rereads_the_table_a_concurrent_prepare_reserved_or_published() {
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    for published in [false, true] {
        let mut state = model.init_state();
        let (stop_entered, stop_waiting, stop_go) = gate();
        let mut host = Host::new(7966 + u64::from(published), |facts| {
            let stop = Some((stop_entered, Arc::clone(&stop_go)));
            if published {
                facts.prepare_journal_gate = stop;
            } else {
                facts.prepare_gate = stop;
            }
        });
        host.approach();
        fire(&model, &mut state, &["Approach"]);
        let (check_entered, check_waiting, check_go) = gate();
        *host.facts.constructor_gate.lock().unwrap() = Some((check_entered, Arc::clone(&check_go)));
        let host_ref = &host;
        let (ctx, facts, request, actor) =
            (&host.handle.ctx, &host.facts, &host.request, &host.actor);
        let (late, original, early) = std::thread::scope(|scope| {
            let send = move || run(ctx, facts, Request::prepare(request.clone()), actor.clone());
            // The replay looks the id up (nothing yet) and stops at its last check before the
            // clock.
            let late = scope.spawn(send);
            check_waiting
                .recv_timeout(PATIENCE)
                .expect("the replay reached its constructor check");
            // The original passes that check, reserves, and stops before it publishes, or
            // publishes and stops before its issuer record.
            let original = scope.spawn(send);
            stop_waiting
                .recv_timeout(PATIENCE)
                .expect("the original reserved or published");
            fire(
                &model,
                &mut state,
                if published {
                    &["Reserve", "Publish"][..]
                } else {
                    &["Reserve"][..]
                },
            );
            assert_eq!(host_ref.probe(&model), state, "published={published}");
            host_ref.expire();
            fire(&model, &mut state, &["Expire"]);
            assert_eq!(host_ref.probe(&model), state);
            assert!(!model.action_enabled("Answer", &state));
            release(&check_go);
            let late = late.join().unwrap();
            assert_eq!(host_ref.probe(&model), state);
            // Negative control: the replay's first look at the table makes the same proof say
            // `unadmitted` for an operation in memory.
            let early = early_view(host_ref);
            assert_eq!(early, host_ref.unadmitted());
            release(&stop_go);
            (late, original.join().unwrap(), early)
        });
        // Booked through the projection, that answer breaks NothingInMemory, and NeverRecorded
        // once a published original's record has landed; then the history forgets it.
        let kept = (host.answered, host.stale, host.early);
        host.note(&state, &early);
        let bad = host.probe(&model);
        assert!(!model.check_invariant("NothingInMemory", &bad));
        assert_eq!(
            model.check_invariant("NeverRecorded", &bad),
            !published,
            "published={published}"
        );
        (host.answered, host.stale, host.early) = kept;
        host.note(&state, &late);
        assert_eq!(
            late, DEADLINE,
            "the replay saw the original, published={published}"
        );
        assert!(
            !matches!(Reply::parse(&original), Some(Reply::Admitted(_))),
            "an expired original never starts: {original}"
        );
        host.note(&state, &original);
        if published {
            // Its record lands after the deadline, which only closes it.
            fire(&model, &mut state, &["Record", "Complete"]);
            assert_eq!(host.probe(&model), state);
            assert!(!model.action_enabled("Answer", &state));
        } else {
            // Its final check refuses it, and the dropped reservation leaves nothing behind.
            assert_eq!(original, DEADLINE);
            fire(&model, &mut state, &["Drop"]);
            assert_eq!(host.probe(&model), state);
            assert_eq!(host.send(&model), host.unadmitted());
            fire(&model, &mut state, &["Answer"]);
            assert_eq!(host.probe(&model), state);
        }
        assert_eq!(host.facts.signals.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn a_lost_issuer_acknowledgement_keeps_the_refusal_past_the_deadline() {
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    let mut state = model.init_state();
    let mut host = Host::new(7968, |facts| {
        facts.prepare_journal_ack.store(false, Ordering::SeqCst);
    });
    host.approach();
    fire(&model, &mut state, &["Approach"]);
    assert_eq!(host.send(&model), UNKNOWN);
    fire(&model, &mut state, &["Reserve", "Publish", "LostAck"]);
    assert_eq!(host.probe(&model), state);
    host.expire();
    fire(&model, &mut state, &["Expire"]);
    assert!(!model.action_enabled("Answer", &state));
    assert_eq!(host.send(&model), UNKNOWN);
    assert_eq!(host.probe(&model), state);
    // Storage recovery after the deadline closes the operation; it never starts.
    host.facts.prepare_journal_ack.store(true, Ordering::SeqCst);
    let closed = host.send(&model);
    assert!(
        !matches!(
            Reply::parse(&closed),
            Some(Reply::Admitted(_) | Reply::Unadmitted(_))
        ),
        "{closed}"
    );
    fire(&model, &mut state, &["Record", "Complete"]);
    assert_eq!(host.probe(&model), state);
    assert_eq!(host.facts.signals.load(Ordering::SeqCst), 0);
}

#[test]
fn the_answer_is_only_for_a_readable_builtin_restart_on_this_host_epoch() {
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    let host = Host::new(7964, |_| {});
    host.expire();
    let ctx = &host.handle.ctx;
    let send = |request: &Prepare, actor: &SupervisorInputCaller| {
        run(
            ctx,
            &host.facts,
            Request::prepare(request.clone()),
            actor.clone(),
        )
    };
    let instance = SupervisorInputCaller {
        principal: Principal::Instance,
        ..host.actor.clone()
    };
    assert_eq!(send(&host.request, &instance), DEADLINE);
    let mut other_epoch = host.request.clone();
    other_epoch.host_epoch = "34".repeat(16);
    assert_eq!(
        send(&other_epoch, &host.actor),
        "ERR native host epoch changed\n"
    );
    host.facts.receipt_readable.store(false, Ordering::SeqCst);
    assert_eq!(send(&host.request, &host.actor), DEADLINE);
    host.facts.receipt_readable.store(true, Ordering::SeqCst);
    assert_eq!(send(&host.request, &host.actor), host.unadmitted());
    // A positive NoExec receipt for the exact prepare is served as one.
    host.facts.receipt_history.lock().unwrap().push((
        host.request.clone(),
        Binding {
            id: host.request.id.clone(),
            capability: "cd".repeat(32),
        },
    ));
    assert_eq!(send(&host.request, &host.actor), NOEXEC);
    // A self-exec plan writes no issuer record, so its absence proves nothing.
    let mut own = Host::new(7965, |_| {});
    own.request.plan = plan(&own.handle.ctx, &own.facts).plan;
    let mut state = model.init_state();
    fire(&model, &mut state, &["OwnImage", "Approach", "Expire"]);
    own.approach();
    own.expire();
    assert_eq!(own.probe(&model), state);
    assert!(!model.action_enabled("Answer", &state));
    assert_eq!(own.send(&model), DEADLINE);
    assert_eq!(own.probe(&model), state);
}
