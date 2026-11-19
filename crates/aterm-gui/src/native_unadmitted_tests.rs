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
    /// What a re-entry can name: nothing (0), only a registration the agent saved from a host
    /// process since recreated (1), or this host's own current-epoch registration (2).
    reg: i64,
    /// The first succession breach the host admitted, as the model books it (`lapse`).
    lapse: i64,
}

impl Host {
    /// The asked upgrade into a recreated host, naming the passive registration `observation`.
    fn reenter(&mut self, observation: aterm_agent::harness::native_observe::ObservationBinding) {
        self.request.asked = Some(AskedIntent::Reentry(
            aterm_agent::harness::native_act::AskedReentryIntent {
                observation,
                ask: None,
            },
        ));
        self.request.duty = Scope::Upgrade;
        assert!(self.request.asked_restart_shape());
    }

    /// A new host process for this session: a fresh operation table and observation store. The
    /// registration the agent saved from the previous process is now another epoch's.
    fn recreate(&mut self) {
        *self.handle.ctx.fenced_writes.native.lock().unwrap() = Operations::default();
        if self.reg == 2 {
            self.reg = 1;
        }
    }

    /// Register the request's live vendor under its shell in this host's passive store, through
    /// the store's own reserve, arm and publish, as the observation lane does.
    fn register(&mut self) -> aterm_agent::harness::native_observe::ObservationBinding {
        let NativePlan::Restart { process, shell, .. } = &self.request.plan else {
            panic!("a restart prepare");
        };
        let registration = aterm_agent::harness::native_observe::Registration {
            id: "7e".repeat(16),
            host_epoch: host_epoch().unwrap().to_string(),
            vendor: process.clone(),
            shell: shell.clone(),
        };
        let ctx = &self.handle.ctx;
        let context = {
            let meta = ctx.meta.lock().unwrap();
            observe_control::context(ctx, &meta, &self.actor, &self.facts).unwrap()
        };
        let reply = observe_control::store(ctx).lock().unwrap().register_with(
            &registration,
            &context,
            -1,
            self.facts.clock.load(Ordering::SeqCst),
            &ObservationKernel(&self.facts),
            || Some("5".repeat(64)),
        );
        let aterm_agent::harness::native_observe::Reply::Registered(binding) = reply else {
            panic!("the live vendor registers: {reply:?}");
        };
        self.reg = 2;
        binding
    }

    /// Data-only shape for the absence theorem, never a fabricated settled predecessor.
    fn ask(&mut self, relaunch: bool) {
        self.request.asked = Some(if relaunch {
            AskedIntent::Relaunch(aterm_agent::harness::native_act::AskedRelaunchIntent {
                predecessor: "de".repeat(16),
                ask: "memory:unadmitted-control".into(),
            })
        } else {
            AskedIntent::Upgrade(aterm_agent::harness::native_act::AskedUpgradeIntent {
                predecessor: "de".repeat(16),
            })
        });
        self.request.duty = if relaunch {
            Scope::Relaunch
        } else {
            Scope::Upgrade
        };
        assert!(self.request.asked_restart_shape());
    }

    /// A built-in restart prepare whose deadline is still too far for admission.
    fn new(local: u64, configure: impl FnOnce(&mut Facts)) -> Self {
        Self::claimed(local, &[Scope::Upgrade], configure)
    }

    /// [`Self::new`] under an external claim taking `duties`, so the built-in harness keeps the
    /// rest: an asked upgrade needs the upgrade duty kept.
    fn claimed(local: u64, duties: &[Scope], configure: impl FnOnce(&mut Facts)) -> Self {
        let (handle, mut facts, mut request, actor) = closed_restart::fixture_claiming(
            crate::session_store::test_handle_on(local, -1),
            duties,
        );
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
            reg: 0,
            lapse: 0,
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
        let (settled, bound, qual) = {
            let table = self.handle.ctx.fenced_writes.native.lock().unwrap();
            let qual = table.by_id.get(&self.request.id).map_or(0, |operation| {
                let op = operation.lock().unwrap();
                match (&op.authority.asked, &op.request.asked) {
                    (Some(_), Some(AskedIntent::Reentry(_))) => 2,
                    (Some(_), Some(_)) => 1,
                    _ => 0,
                }
            });
            (
                i64::from(matches!(
                    table.asked_binding,
                    asked_upgrade::CurrentBinding::Settled(_)
                )),
                i64::from(!matches!(
                    table.asked_binding,
                    asked_upgrade::CurrentBinding::Unknown
                )),
                qual,
            )
        };
        let mut state = model.init_state();
        for (var, value) in [
            ("settled", settled),
            ("bound", bound),
            ("reg", self.reg),
            ("qual", qual),
            ("lapse", self.lapse),
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
            (
                "asked",
                if self.request.asked.is_none() {
                    0
                } else if self.request.asked_restart_shape() {
                    1
                } else {
                    2
                },
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
            Some(Reply::Admitted(_)) => {
                self.admitted = true;
                if self.lapse == 0 {
                    self.lapse = match &self.request.asked {
                        Some(AskedIntent::Reentry(_)) if before["reg"] <= 1 => 1,
                        Some(AskedIntent::Reentry(_)) if before["bound"] == 1 => 2,
                        Some(AskedIntent::Upgrade(_) | AskedIntent::Relaunch(_))
                            if before["settled"] == 0 =>
                        {
                            3
                        }
                        _ => 0,
                    };
                }
            }
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

/// The admission a host that ignored a re-entry rule would have given `host`'s request, booked
/// through the projection from `before`: the real-host negative control for that rule.
fn book_admission(host: &mut Host, before: &State, law: &str, model: &Model) {
    let reply = admitted(&Binding {
        id: host.request.id.clone(),
        capability: "6".repeat(64),
    });
    host.note(before, &reply);
    assert!(!model.check_invariant(law, &host.probe(model)), "{law}");
    (host.admitted, host.lapse) = (false, 0);
}

#[test]
fn a_recreated_host_reenters_only_from_its_own_registration_with_nothing_bound() {
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    // The agent names the registration it saved from the host process before this one.
    let mut host = Host::claimed(8150, &[Scope::TurnEnd], |_| {});
    let mut state = model.init_state();
    host.reg = 2;
    host.recreate();
    host.approach();
    host.reenter(aterm_agent::harness::native_observe::ObservationBinding {
        id: "7f".repeat(16),
        host_epoch: "0".repeat(32),
        capability: "5".repeat(64),
    });
    fire(
        &model,
        &mut state,
        &["Register", "Recreate", "Approach", "Ask"],
    );
    assert_eq!(host.probe(&model), state);
    assert_eq!(host.send(&model), DENIED);
    assert_eq!(host.probe(&model), state);
    assert!(!model.action_enabled("Reenter", &state));
    let mutant = aterm_spec::interp::with_buggy(&model, 8);
    let mut bad = state.clone();
    assert!(mutant.fire("Reenter", &mut bad));
    assert!(!model.check_invariant("ReentryOwnRegistration", &bad));
    book_admission(&mut host, &state, "ReentryOwnRegistration", &model);
    // Nor does a binding spelled in this host's epoch that its own store never issued.
    host.reenter(aterm_agent::harness::native_observe::ObservationBinding {
        id: "7f".repeat(16),
        host_epoch: host_epoch().unwrap().to_string(),
        capability: "5".repeat(64),
    });
    assert_eq!(host.probe(&model), state);
    assert_eq!(host.send(&model), DENIED);
    assert_eq!(host.probe(&model), state);

    // This host's own registration, with another operation of its epoch bound in the table.
    let mut host = Host::claimed(8151, &[Scope::TurnEnd], |_| {});
    let mut state = model.init_state();
    let binding = host.register();
    host.handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .asked_binding = asked_upgrade::CurrentBinding::Unsettled {
        binding: Binding {
            id: "ab".repeat(16),
            capability: "cd".repeat(32),
        },
        prior: None,
    };
    host.approach();
    host.reenter(binding);
    fire(&model, &mut state, &["Register", "Bind", "Approach", "Ask"]);
    assert_eq!(host.probe(&model), state);
    assert_eq!(host.send(&model), DENIED);
    assert_eq!(host.probe(&model), state);
    assert!(!model.action_enabled("Reenter", &state));
    let mutant = aterm_spec::interp::with_buggy(&model, 9);
    let mut bad = state.clone();
    assert!(mutant.fire("Reenter", &mut bad));
    assert!(!model.check_invariant("ReentryUnbound", &bad));
    book_admission(&mut host, &state, "ReentryUnbound", &model);

    // The recreated host's own current-epoch registration re-enters, and nothing else does.
    let mut host = Host::claimed(8152, &[Scope::TurnEnd], |_| {});
    let mut state = model.init_state();
    host.reg = 2;
    host.recreate();
    let binding = host.register();
    host.approach();
    host.reenter(binding);
    fire(
        &model,
        &mut state,
        &["Register", "Recreate", "Register", "Approach", "Ask"],
    );
    assert_eq!(host.probe(&model), state);
    let admitted = admitted_binding(&host.send(&model));
    assert_eq!(admitted.id, host.request.id);
    fire(
        &model,
        &mut state,
        &["Reenter", "Reserve", "Publish", "Record", "Start"],
    );
    assert_eq!(host.probe(&model), state);
    for law in &model.invariants {
        assert!(model.check_invariant(law.name, &state), "{}", law.name);
    }
    // The settled predecessor's upgrade is the previous process's: this host never revives it.
    let mut upgrade = Host::claimed(8153, &[Scope::TurnEnd], |_| {});
    let mut state = model.init_state();
    upgrade.recreate();
    upgrade.approach();
    upgrade.ask(false);
    fire(
        &model,
        &mut state,
        &["Settle", "Recreate", "Approach", "Ask"],
    );
    assert_eq!(upgrade.probe(&model), state);
    assert_eq!(upgrade.send(&model), DENIED);
    assert_eq!(upgrade.probe(&model), state);
    assert!(!model.action_enabled("QualifyUpgrade", &state));
    let mutant = aterm_spec::interp::with_buggy(&model, 10);
    let mut bad = state.clone();
    assert!(mutant.fire("QualifyUpgrade", &mut bad));
    assert!(!model.check_invariant("NoRevival", &bad));
    book_admission(&mut upgrade, &state, "NoRevival", &model);
}

/// A RE-ENTRY ON A STORED LOGIN IS NOT REFUSED BY THE ROOT SLOT IT RESERVED ITSELF (the stalled
/// sessions on m19, m41 and m29 run on their stored login): the host reserves a stored forward's
/// account lineage root under the request's own id before it qualifies the request, so a
/// qualification that refused any lineage held denied every such re-entry, and the agent's
/// retained prepare replayed into the same denial until its deadline, then asked again. The
/// recreated host's own registration re-enters the stored login (the account lineage it reserved
/// is the request's own); another operation's lineage still refuses it. Its effect-free abort
/// then leaves the epoch to the next re-entry once the succession read compacts the root.
///
/// NEGATIVE CONTROL: with `qualify_reentry` refusing any lineage held
/// (`!self.account_lineages.is_empty()`), the first send is DENIED.
#[test]
fn a_reentry_on_a_stored_login_is_admitted_past_its_own_root_slot_and_no_other_lineage() {
    use aterm_agent::harness::accounts_pool::{ExecutionIntent, Purpose};
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    let stored = |host: &mut Host| {
        let NativePlan::Restart {
            forward, rollback, ..
        } = &mut host.request.plan
        else {
            panic!("a restart prepare");
        };
        for command in std::iter::once(&mut **forward).chain(rollback.as_deref_mut()) {
            command.account_intent = Some(Box::new(ExecutionIntent::Stored {
                namespace: "ab".repeat(32),
                login: Some("/logins/work".into()),
                owner: Some("work".into()),
                generation: "cd".repeat(32),
                purpose: Purpose::SameLogin,
            }));
        }
    };

    // Another operation's lineage frontier, held by the recreated host: refused.
    let mut held = Host::claimed(8155, &[Scope::TurnEnd], |_| {});
    stored(&mut held);
    held.reg = 2;
    held.recreate();
    let binding = held.register();
    held.handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .account_lineages
        .reserve(&AccountSlot::Root, &"ef".repeat(16), 0)
        .unwrap();
    held.approach();
    held.reenter(binding);
    assert_eq!(held.send(&model), DENIED, "another operation's lineage");

    // The request's own root slot alone: admitted, and the run stays the model's.
    let mut host = Host::claimed(8154, &[Scope::TurnEnd], |_| {});
    stored(&mut host);
    let mut state = model.init_state();
    host.reg = 2;
    host.recreate();
    let binding = host.register();
    host.approach();
    host.reenter(binding);
    fire(
        &model,
        &mut state,
        &["Register", "Recreate", "Register", "Approach", "Ask"],
    );
    assert_eq!(host.probe(&model), state);
    let admitted = admitted_binding(&host.send(&model));
    assert_eq!(admitted.id, host.request.id);
    assert!(
        host.handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .account_lineages
            .pins(&host.request.id),
        "the stored login's root lineage is the re-entry's own"
    );
    fire(
        &model,
        &mut state,
        &["Reenter", "Reserve", "Publish", "Record", "Start"],
    );
    assert_eq!(host.probe(&model), state);
    for law in &model.invariants {
        assert!(model.check_invariant(law.name, &state), "{}", law.name);
    }

    // ITS EFFECT-FREE ABORT LEAVES THE EPOCH TO THE NEXT RE-ENTRY, NOT TO ITS OWN ROOT (the
    // succession read settled without compacting, so the aborted stored-login re-entry's own root
    // lineage answered `bound` until the host was recreated): the read reclaims as a prepare
    // does, the root compacts, and the epoch binds nothing again.
    // NEGATIVE CONTROL: with `succession_reply` reading the table unreclaimed, the answer is
    // `bound`.
    for request in [Request::Abort(admitted.clone()), Request::Query(admitted)] {
        assert_eq!(
            run(&host.handle.ctx, &host.facts, request, host.actor.clone()),
            ROLLBACK_SAFE
        );
    }
    fire(&model, &mut state, &["Complete"]);
    assert_eq!(host.probe(&model), state);
    assert!(
        host.handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .account_lineages
            .pins(&host.request.id),
        "the aborted re-entry's own root is held until the table is reclaimed"
    );
    assert_eq!(
        run(
            &host.handle.ctx,
            &host.facts,
            Request::Succession(Some(host.request.id.clone())),
            host.actor.clone(),
        ),
        aterm_agent::harness::native_act::succession_reply(
            host_epoch().unwrap(),
            &aterm_agent::harness::native_act::Succession::Reentry
        )
        .unwrap()
    );
    fire(&model, &mut state, &["Unbind"]);
    assert_eq!(host.probe(&model), state);
}

/// AN OPERATION THAT ABORTS WITH NO EFFECT AND NOTHING SETTLED BEFORE IT LEAVES THE EPOCH TO A
/// RE-ENTRY (a recreated host's exit-lane cold relaunch, published and cancelled before any
/// effect, bound the epoch for good: only a re-entry's own effect-free abort settled back to
/// unbound, so every re-entry after it was denied `qualification-reentry-bound`, and the agent,
/// which cannot tell that abort from one after an effect, prepared and replayed it again and
/// again): another operation of the recreated host's epoch (the fixture's plain restart) is
/// published and aborted before any effect, so the table binds nothing again; the host's
/// succession answer says so, and the host's own registration re-enters, as the model's `Unbind`
/// lets it. One whose abort followed an effect keeps the epoch bound, and the answer says that.
///
/// NEGATIVE CONTROL: with the effect-free abort settling back to unbound for a re-entry alone
/// (and a plain restart's settlement refused at its constructor first), the answer is `bound`
/// and the re-entry is DENIED.
#[test]
fn an_effect_free_abort_with_nothing_settled_before_it_leaves_the_epoch_to_a_reentry() {
    use aterm_agent::harness::native_act::{Succession, succession_reply};
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    let succession = |host: &Host| {
        run(
            &host.handle.ctx,
            &host.facts,
            Request::Succession(Some("5e".repeat(16))),
            host.actor.clone(),
        )
    };
    let epoch = || host_epoch().unwrap().to_string();

    let mut host = Host::claimed(8156, &[Scope::TurnEnd], |_| {});
    let mut state = model.init_state();
    host.reg = 2;
    host.recreate();
    let binding = host.register();
    host.approach();
    assert_eq!(
        succession(&host),
        succession_reply(&epoch(), &Succession::Reentry).unwrap(),
        "a recreated host binds nothing"
    );
    // Another operation of this epoch, published, then aborted before any effect.
    let mut plain = host.request.clone();
    plain.id = "a1".repeat(16);
    let published = admitted_binding(&run(
        &host.handle.ctx,
        &host.facts,
        Request::prepare(plain),
        host.actor.clone(),
    ));
    assert_eq!(
        succession(&host),
        succession_reply(
            &epoch(),
            &Succession::Bound("settlement-not-completed".into())
        )
        .unwrap(),
        "an operation in flight binds the epoch"
    );
    assert_eq!(
        run(
            &host.handle.ctx,
            &host.facts,
            Request::Abort(published.clone()),
            host.actor.clone(),
        ),
        ROLLBACK_SAFE
    );
    assert_eq!(
        run(
            &host.handle.ctx,
            &host.facts,
            Request::Query(published),
            host.actor.clone(),
        ),
        ROLLBACK_SAFE
    );
    assert_eq!(
        succession(&host),
        succession_reply(&epoch(), &Succession::Reentry).unwrap(),
        "its effect-free abort leaves nothing bound"
    );
    host.reenter(binding);
    fire(
        &model,
        &mut state,
        &[
            "Register", "Recreate", "Register", "Approach", "Bind", "Unbind", "Ask",
        ],
    );
    assert_eq!(host.probe(&model), state);
    let admitted = admitted_binding(&host.send(&model));
    assert_eq!(admitted.id, host.request.id);
    fire(
        &model,
        &mut state,
        &["Reenter", "Reserve", "Publish", "Record", "Start"],
    );
    assert_eq!(host.probe(&model), state);
    for law in &model.invariants {
        assert!(model.check_invariant(law.name, &state), "{}", law.name);
    }

    // CONTROL: an abort after an effect (the plain restart's signal was sent) keeps the epoch
    // bound, and the answer says so before any re-entry is prepared.
    let mut effected = Host::claimed(8157, &[Scope::TurnEnd], |_| {});
    effected.reg = 2;
    effected.recreate();
    let _binding = effected.register();
    effected.approach();
    let mut plain = effected.request.clone();
    plain.id = "a2".repeat(16);
    let published = admitted_binding(&run(
        &effected.handle.ctx,
        &effected.facts,
        Request::prepare(plain),
        effected.actor.clone(),
    ));
    let operation = lookup(&effected.handle.ctx, &published).unwrap();
    operation.lock().unwrap().signal_sent = true;
    let _aborted = run(
        &effected.handle.ctx,
        &effected.facts,
        Request::Abort(published.clone()),
        effected.actor.clone(),
    );
    let _queried = run(
        &effected.handle.ctx,
        &effected.facts,
        Request::Query(published),
        effected.actor.clone(),
    );
    assert_eq!(
        succession(&effected),
        succession_reply(
            &epoch(),
            &Succession::Bound("settlement-not-completed".into())
        )
        .unwrap(),
        "an abort after an effect is no retry grant"
    );
}

/// AN OPERATION THAT REACHED ITS CLOSE LEAVES THE EPOCH TO A RE-ENTRY (an asked upgrade whose
/// relaunched build first-exec'd past its close, under a slow assessment, ended `closed` and held
/// the table unsettled for the rest of the epoch: a close is never acknowledged as a terminal
/// append and never settles forward, so every later upgrade and Memory restart of the session
/// waited native-reentry-bound until aterm was updated, while the agent's closed watch had
/// already adopted the relaunched vendor): a restart granted Execute and swept `closed` at its
/// close owes nothing more, and nothing a later asked move succeeds, so the epoch binds nothing
/// and the host answers `reentry`, qualified afresh from its own registration of whatever vendor
/// runs. An operation still in flight keeps the epoch bound.
///
/// NEGATIVE CONTROL: with a close kept unsettled (`settle_asked_binding` refusing any end but a
/// commit or an effect-free abort), the answer is `bound`.
#[test]
fn an_operation_closed_past_its_close_leaves_the_epoch_to_a_reentry() {
    use aterm_agent::harness::native_act::{Succession, succession_reply};
    let (handle, facts, binding, actor, launcher) = super::issuer::ready(8159);
    let ctx = &handle.ctx;
    let succession = || {
        run(
            ctx,
            &facts,
            Request::Succession(Some("5e".repeat(16))),
            actor.clone(),
        )
    };
    let answer = |admits: Succession| succession_reply(host_epoch().unwrap(), &admits).unwrap();
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher
        )),
        Some(Reply::Execute { .. })
    ));
    // CONTROL: the granted restart in flight binds the epoch.
    assert_eq!(
        succession(),
        answer(Succession::Bound("settlement-not-completed".into()))
    );
    let close = lookup(ctx, &binding)
        .unwrap()
        .lock()
        .unwrap()
        .request
        .close_us();
    facts.clock.store(close, Ordering::SeqCst);
    reconcile_extinct_by(ctx, &facts);
    assert_eq!(
        lookup(ctx, &binding).unwrap().lock().unwrap().phase,
        Phase::Completed(Decision::Closed)
    );
    assert_eq!(succession(), answer(Succession::Reentry));
    assert!(matches!(
        ctx.fenced_writes.native.lock().unwrap().asked_binding,
        asked_upgrade::CurrentBinding::Unknown
    ));
}

/// A MEMORY RESTART'S RE-ENTRY NEEDS ITS CHECKED PRODUCER (a coordinated session's memory
/// restart into a recreated host re-enters from that host's own registration, as an upgrade
/// does, and carries its ask): the host qualifies the relaunch duty's re-entry only under the
/// scoped authorization its checked relaunch producer issued for exactly that request, as it
/// does a Memory restart naming its predecessor. Without one the same registration's re-entry is
/// DENIED on the relaunch duty, while the upgrade's re-entry of it is admitted.
///
/// NEGATIVE CONTROL: with the producer sites keyed on a predecessor-naming Memory restart alone
/// (`Prepare::checked_relaunch` as `AskedIntent::Relaunch`), the relaunch duty's re-entry is
/// admitted.
#[test]
fn a_memory_restart_reentry_is_qualified_only_under_its_checked_producer() {
    use aterm_agent::harness::native_act::AskedReentryIntent;
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    let mut host = Host::claimed(8158, &[Scope::TurnEnd], |_| {});
    host.reg = 2;
    host.recreate();
    let binding = host.register();
    host.approach();
    host.request.duty = Scope::Relaunch;
    host.request.asked = Some(AskedIntent::Reentry(AskedReentryIntent {
        observation: binding.clone(),
        ask: Some("memory@33".into()),
    }));
    assert!(host.request.asked_restart_shape());
    assert_eq!(
        host.send(&model),
        DENIED,
        "no checked producer authorized it"
    );
    assert!(
        !host
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .contains_key(&host.request.id),
        "nothing published"
    );
    // CONTROL: the upgrade's re-entry of the same registration needs no producer.
    host.reenter(binding);
    let admitted = admitted_binding(&host.send(&model));
    assert_eq!(admitted.id, host.request.id);
}

#[test]
fn expired_supported_asked_shapes_share_only_the_never_admitted_answer() {
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    for relaunch in [false, true] {
        let mut host = Host::new(8120 + u64::from(relaunch), |_| {});
        host.ask(relaunch);
        let mut state = model.init_state();
        fire(&model, &mut state, &["Ask"]);
        assert_eq!(host.probe(&model), state);
        host.approach();
        host.expire();
        fire(&model, &mut state, &["Approach", "Expire"]);
        assert_eq!(host.send(&model), host.unadmitted());
        fire(&model, &mut state, &["Answer"]);
        assert_eq!(host.probe(&model), state);
        for law in &model.invariants {
            assert!(model.check_invariant(law.name, &state), "{}", law.name);
        }
        assert_eq!(host.facts.watch_count.load(Ordering::SeqCst), 0);
        assert_eq!(host.facts.signals.load(Ordering::SeqCst), 0);
        assert!(host.facts.prepared_history.lock().unwrap().is_empty());
        let mut malformed = Host::new(8122 + u64::from(relaunch), |_| {});
        malformed.ask(relaunch);
        malformed.request.duty = if relaunch {
            Scope::Upgrade
        } else {
            Scope::Relaunch
        };
        malformed.approach();
        malformed.expire();
        let mut unsupported = model.init_state();
        fire(
            &model,
            &mut unsupported,
            &["UnsupportedAsk", "Approach", "Expire"],
        );
        assert_eq!(malformed.probe(&model), unsupported);
        assert!(!model.action_enabled("Answer", &unsupported));
        assert_eq!(
            unadmitted_after_deadline(
                &malformed.handle.ctx,
                &malformed.request,
                &malformed.actor,
                None,
                None,
                &malformed.facts,
            ),
            DEADLINE
        );
        // Negative control: merely deleting the asked guard would answer the unsupported shape.
        let reply = malformed.unadmitted();
        malformed.note(&unsupported, &reply);
        assert!(!model.check_invariant("SupportedShape", &malformed.probe(&model)));
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
fn an_admitted_restart_is_never_unadmitted_after_completion_and_reclaim() {
    let model = aterm_models_harness::native_unadmitted_prepare_model();
    for sealed in [false, true] {
        let mut state = model.init_state();
        let mut host = Host::new(7961 + u64::from(sealed), |_| {});
        if sealed {
            // A sealed cold restart's history answers `lapsed`; a private one's has no answer.
            cold_restart(&mut host.request);
        }
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
        // The cold restart ended rollback-safe before any effect, and the private restart retired
        // with no exec granted, each owing nothing and with nothing a later asked move succeeds:
        // its settlement at reclaim leaves the table binding nothing.
        fire(&model, &mut state, &["Unbind"]);
        assert_eq!(host.probe(&model), state, "sealed={sealed}");
        // The table forgot the id; its issuer record keeps `unadmitted` away. A positive
        // NoExec receipt is still served as one, and a seal that never recorded possible
        // execution as `lapsed`, which is no unadmitted Answer.
        assert!(!model.action_enabled("Answer", &state));
        let expected = if sealed {
            aterm_agent::harness::native_act::lapsed_reply(&host.request.id).unwrap()
        } else {
            NOEXEC.to_string()
        };
        assert_eq!(host.send(&model), expected, "sealed={sealed}");
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
    // A private restart has no historical answer; another epoch's stamped cold restart
    // lapses past its deadline (`native_lapsed_tests.rs`).
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

#[cfg(unix)]
#[test]
fn an_older_epoch_prepare_is_durably_rejected_before_the_client_may_drop_it() {
    use aterm_agent::harness::native_receipts::testing::{Append, Journal};
    for ask in 0..3 {
        let home = aterm_tempfile::tempdir().unwrap();
        let mut host = Host::new(7970 + ask, |facts| {
            facts.native_journal = Some(Journal::new(home.path()));
        });
        if ask > 0 {
            host.ask(ask == 2);
        }
        host.request.host_epoch = "34".repeat(16);
        let send = |request: &Prepare, actor: &SupervisorInputCaller| {
            run(
                &host.handle.ctx,
                &host.facts,
                Request::prepare(request.clone()),
                actor.clone(),
            )
        };
        let instance = SupervisorInputCaller {
            principal: Principal::Instance,
            ..host.actor.clone()
        };
        assert_eq!(
            send(&host.request, &host.actor),
            "ERR native host epoch changed\n"
        );
        assert_eq!(
            run(
                &host.handle.ctx,
                &host.facts,
                Request::RejectionDomain,
                instance.clone()
            ),
            DENIED
        );
        let domain = run(
            &host.handle.ctx,
            &host.facts,
            Request::RejectionDomain,
            host.actor.clone(),
        );
        let Some(Reply::RejectionDomain(domain)) = Reply::parse(&domain) else {
            panic!("the host did not name its rejection domain");
        };
        host.request.rejection_domain = Some(domain);
        assert_eq!(
            send(&host.request, &instance),
            "ERR native host epoch changed\n"
        );
        let journal = host.facts.native_journal.as_ref().unwrap();
        assert!(!journal.contains_operation(&host.request.id).unwrap());
        host.facts.receipt_readable.store(false, Ordering::SeqCst);
        assert_eq!(
            send(&host.request, &host.actor),
            "ERR native host epoch changed\n"
        );
        host.facts.receipt_readable.store(true, Ordering::SeqCst);
        // The first reply may be lost. Reopening the journal and retrying must
        // still give the same answer without spending another record.
        assert_eq!(send(&host.request, &host.actor), host.unadmitted());
        assert_eq!(send(&host.request, &host.actor), host.unadmitted());
        assert_eq!(journal.record_count().unwrap(), 1);
        assert!(journal.contains_operation(&host.request.id).unwrap());
        let binding = Binding {
            id: host.request.id.clone(),
            capability: "ab".repeat(32),
        };
        assert!(
            journal
                .record(&host.request, &binding, Append::Prepare)
                .is_err()
        );
        let mut changed = host.request.clone();
        changed.deadline_us += 1;
        assert_eq!(
            send(&changed, &host.actor),
            "ERR native host epoch changed\n"
        );
        assert_eq!(host.facts.signals.load(Ordering::SeqCst), 0);
        assert!(
            host.handle
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .by_id
                .is_empty()
        );
    }
}

#[cfg(unix)]
#[test]
fn a_previous_epoch_admission_is_never_reinterpreted_as_unadmitted() {
    use aterm_agent::harness::native_receipts::testing::{Append, Journal};
    for events in [
        vec![Append::Prepare],
        vec![Append::Prepare, Append::MayExecute],
        vec![Append::Prepare, Append::Seal],
        vec![Append::Prepare, Append::MayExecute, Append::Seal],
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let mut host = Host::new(7974, |facts| {
            facts.native_journal = Some(Journal::new(home.path()));
        });
        host.request.host_epoch = "34".repeat(16);
        host.request.rejection_domain = Some(
            host.facts
                .native_journal
                .as_ref()
                .unwrap()
                .rejection_domain()
                .unwrap(),
        );
        let binding = Binding {
            id: host.request.id.clone(),
            capability: "ab".repeat(32),
        };
        let journal = host.facts.native_journal.as_ref().unwrap();
        for event in events {
            journal.record(&host.request, &binding, event).unwrap();
        }
        let before = journal.record_count().unwrap();
        assert_eq!(
            run(
                &host.handle.ctx,
                &host.facts,
                Request::prepare(host.request.clone()),
                host.actor.clone()
            ),
            "ERR native host epoch changed\n"
        );
        assert_eq!(journal.record_count().unwrap(), before);
        assert_eq!(host.facts.signals.load(Ordering::SeqCst), 0);
    }
}

#[cfg(unix)]
#[test]
fn a_foreign_journal_domain_grants_neither_current_admission_nor_old_epoch_recovery() {
    use aterm_agent::harness::native_receipts::testing::{Append, Journal};
    let home = aterm_tempfile::tempdir().unwrap();
    let other_home = aterm_tempfile::tempdir().unwrap();
    let other = Journal::new(other_home.path());
    let mut host = Host::new(7975, |facts| {
        facts.native_journal = Some(Journal::new(home.path()));
    });
    host.request.rejection_domain = Some(other.rejection_domain().unwrap());
    assert_eq!(
        run(
            &host.handle.ctx,
            &host.facts,
            Request::prepare(host.request.clone()),
            host.actor.clone()
        ),
        DENIED
    );
    host.request.host_epoch = "34".repeat(16);
    assert_eq!(
        run(
            &host.handle.ctx,
            &host.facts,
            Request::prepare(host.request.clone()),
            host.actor.clone()
        ),
        "ERR native host epoch changed\n"
    );
    let local = host.facts.native_journal.as_ref().unwrap();
    assert!(!local.contains_operation(&host.request.id).unwrap());
    let binding = Binding {
        id: host.request.id.clone(),
        capability: "ab".repeat(32),
    };
    other
        .record(&host.request, &binding, Append::Prepare)
        .unwrap();
    assert_eq!(host.facts.signals.load(Ordering::SeqCst), 0);
}
