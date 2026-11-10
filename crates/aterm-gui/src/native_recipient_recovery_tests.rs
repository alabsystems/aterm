// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real authenticated history and host replies projected onto RecipientClosure.

use super::*;
use aterm_agent::harness::native_receipts::{
    Recipient, RecipientHistory,
    testing::{Journal, RecipientAppend},
};
use aterm_spec::{derive::Model, interp::State};

const EPOCH_CHANGED: &str = "ERR native host epoch changed\n";

pub(super) struct World {
    _home: aterm_tempfile::TempDir,
    handle: crate::session_store::SessionHandle,
    facts: Arc<Facts>,
    request: Prepare,
    actor: SupervisorInputCaller,
    origin: BootStamp,
    complete: bool,
    live: bool,
    late: bool,
    delivered: [bool; 2],
    unknown: i64,
    clock: i64,
    seen: i64,
    sc: i64,
    sf: i64,
    sr: i64,
    probed: i64,
    fp: i64,
    rp: i64,
    answer: i64,
    fresh: bool,
}

impl World {
    fn new(local: u64) -> Self {
        let home = aterm_tempfile::tempdir().unwrap();
        let (handle, mut facts, mut request, actor) = closed_restart::fixture(local);
        cold_restart(&mut request);
        request.host_epoch = "34".repeat(16);
        let origin = facts.boot().unwrap();
        request.boot = Some(origin.clone());
        facts.native_journal = Some(Journal::new(home.path()));
        Self {
            _home: home,
            handle,
            facts: Arc::new(facts),
            request,
            actor,
            origin,
            complete: false,
            live: true,
            late: false,
            delivered: [false; 2],
            unknown: 0,
            clock: 0,
            seen: 0,
            sc: 0,
            sf: 0,
            sr: 0,
            probed: 0,
            fp: 0,
            rp: 0,
            answer: 0,
            fresh: false,
        }
    }

    fn recipient(index: usize) -> Recipient {
        let pid = 7101 + u32::try_from(index).unwrap();
        Recipient {
            pid,
            birth: 91,
            group: pid,
            session: 400,
        }
    }

    fn asked_upgrade(&mut self) {
        let NativePlan::Restart { cold, rollback, .. } = &mut self.request.plan else {
            unreachable!()
        };
        *cold = None;
        *rollback = Some(Box::new(restart_command(2)));
        self.request.asked = Some(AskedIntent::Upgrade(
            aterm_agent::harness::native_act::AskedUpgradeIntent {
                predecessor: "de".repeat(16),
            },
        ));
        self.request.duty = Scope::Upgrade;
        assert!(self.request.asked_restart_shape());
    }

    fn binding(&self) -> Binding {
        Binding {
            id: self.request.id.clone(),
            capability: "cd".repeat(32),
        }
    }
    fn journal(&self) -> &Journal {
        self.facts.native_journal.as_ref().unwrap()
    }
    fn closure(&self) -> Closure {
        self.journal().observe_closure(&self.request).unwrap()
    }
    fn history(&self) -> Option<RecipientHistory> {
        match self.closure() {
            Closure::Recipients(h) => Some(h),
            _ => None,
        }
    }
    fn send(&self) -> String {
        run(
            &self.handle.ctx,
            &self.facts,
            Request::prepare(self.request.clone()),
            self.actor.clone(),
        )
    }
    fn lapsed(&self) -> String {
        aterm_agent::harness::native_act::lapsed_reply(&self.request.id).unwrap()
    }
    fn spawn(&self, index: usize) {
        let who = Self::recipient(index);
        let mut child = process(who.pid, 400, who.group, who.session, 20);
        child.birth = who.birth;
        self.facts.processes.lock().unwrap().insert(who.pid, child);
    }
    fn exit(&self, index: usize) {
        let who = Self::recipient(index);
        let mut processes = self.facts.processes.lock().unwrap();
        processes.remove(&who.pid);
        let child = process(who.pid + 100, 1, who.group, who.session, 9);
        processes.insert(child.pid, child);
    }
    fn clear(&self, index: usize) {
        let group = Self::recipient(index).group;
        self.facts
            .processes
            .lock()
            .unwrap()
            .retain(|_, p| p.group != group);
    }
    fn stage(&self, index: usize) -> i64 {
        let who = self
            .history()
            .and_then(|h| if index == 0 { h.forward } else { h.rollback });
        let who = who
            .or_else(|| (!self.complete && self.delivered[index]).then(|| Self::recipient(index)));
        let Some(who) = who else {
            return 0;
        };
        let processes = self.facts.processes.lock().unwrap();
        if processes
            .get(&who.pid)
            .is_some_and(|p| p.birth == who.birth)
        {
            if self.delivered[index] { 2 } else { 1 }
        } else if processes.values().any(|p| p.group == who.group) {
            3
        } else {
            4
        }
    }
    fn scope(&self) -> i64 {
        match self.facts.boot() {
            None => 3,
            Some(boot) if boot.host != self.origin.host => 2,
            Some(boot) if boot.boot != self.origin.boot => 1,
            Some(_) => 0,
        }
    }

    pub(super) fn probe(&self, model: &Model) -> State {
        let recorded = match self.closure() {
            Closure::Absent(_) => None,
            Closure::Exact(state) => Some(state),
            Closure::Recipients(history) => Some(history.state),
            Closure::Other => panic!("exact fixture history"),
        };
        let journal = match recorded {
            None => 0,
            Some(Recorded::Prepared) => 1,
            Some(Recorded::MayExecute) => 2,
            Some(Recorded::Sealed { may_execute: false }) => 3,
            Some(Recorded::Sealed { may_execute: true }) => 4,
            Some(Recorded::NoExec) => 5,
        };
        let mut state = model.init_state();
        for (name, value) in [
            ("mode", i64::from(self.complete)),
            ("journal", journal),
            ("live", i64::from(self.live)),
            ("late", i64::from(self.late)),
            ("scope", self.scope()),
            ("f", self.stage(0)),
            ("r", self.stage(1)),
            ("fd", i64::from(self.delivered[0])),
            ("rd", i64::from(self.delivered[1])),
            ("unknown", self.unknown),
            ("clock", self.clock),
            ("seen", self.seen),
            ("sc", self.sc),
            ("sf", self.sf),
            ("sr", self.sr),
            ("probe", self.probed),
            ("fp", self.fp),
            ("rp", self.rp),
            ("answer", self.answer),
            ("fresh", i64::from(self.fresh)),
        ] {
            state.insert(name, value);
        }
        state
    }

    fn follow(&self, model: &Model, state: &mut State, action: &str) {
        assert!(model.fire(action, state), "{action}: {state:?}");
        assert_eq!(self.probe(model), *state, "after {action}");
    }
    fn steps(&mut self, model: &Model, state: &mut State, actions: &[&str]) {
        for action in actions {
            match *action {
                "Complete" => self.complete = true,
                "Prepare" if self.complete => {
                    self.journal()
                        .record_recipient(&self.request, &self.binding(), RecipientAppend::Prepare)
                        .unwrap();
                }
                "Prepare" => assert!(self.facts.record_prepared(&self.request, &self.binding())),
                "RecordF" | "RecordR" => {
                    let index = usize::from(*action == "RecordR");
                    let who = Self::recipient(index);
                    let event = if index == 0 {
                        RecipientAppend::Forward(who)
                    } else {
                        RecipientAppend::Rollback(who)
                    };
                    self.journal()
                        .record_recipient(&self.request, &self.binding(), event)
                        .unwrap();
                    self.spawn(index);
                }
                "DeliverF" | "DeliverR" => {
                    self.delivered[usize::from(*action == "DeliverR")] = true
                }
                "LegacyDeliver" => {
                    self.journal()
                        .record(
                            &self.request,
                            &self.binding(),
                            aterm_agent::harness::native_receipts::testing::Append::MayExecute,
                        )
                        .unwrap();
                    assert!(
                        self.facts
                            .record_may_execute(&self.request, &self.binding())
                    );
                    self.spawn(0);
                    self.delivered[0] = true;
                }
                "ExitF" | "ExitR" => self.exit(usize::from(*action == "ExitR")),
                "ClearF" | "ClearR" => self.clear(usize::from(*action == "ClearR")),
                "SealOpen" | "SealMay" => {
                    self.journal()
                        .record_recipient(&self.request, &self.binding(), RecipientAppend::Seal)
                        .unwrap();
                }
                "NoExec" => {
                    self.journal()
                        .record_recipient(&self.request, &self.binding(), RecipientAppend::NoExec)
                        .unwrap();
                }
                "Crash" => self.live = false,
                "Expire" => {
                    self.late = true;
                    self.facts
                        .clock
                        .store(self.request.deadline_us, Ordering::SeqCst);
                }
                "Reboot" => {
                    let mut boot = self.origin.clone();
                    boot.boot = "e7".repeat(16);
                    *self.facts.boot.lock().unwrap() = Some(boot);
                    self.live = false;
                    self.clear(0);
                    self.clear(1);
                }
                "Foreign" => {
                    let mut boot = self.origin.clone();
                    boot.host = format!("macos:{}", "e8".repeat(16));
                    *self.facts.boot.lock().unwrap() = Some(boot);
                    self.facts
                        .clock
                        .store(self.request.deadline_us, Ordering::SeqCst);
                }
                "UnknownBoot" => {
                    *self.facts.boot.lock().unwrap() = None;
                    self.facts
                        .clock
                        .store(self.request.deadline_us, Ordering::SeqCst);
                }
                "UnknownBirth" | "UnknownGroup" => {
                    self.unknown = if *action == "UnknownBirth" { 1 } else { 2 };
                    for index in 0..2 {
                        let who = Self::recipient(index);
                        if self.unknown == 1 {
                            self.facts
                                .birth_observations
                                .lock()
                                .unwrap()
                                .insert(who.pid, None);
                        } else {
                            self.facts
                                .group_observations
                                .lock()
                                .unwrap()
                                .insert(who.group, None);
                        }
                    }
                }
                "Fresh" => self.fresh = true,
                other => panic!("not a premise: {other}"),
            }
            self.follow(model, state, action);
        }
    }
    fn capture_reads(&mut self, model: &Model) {
        let before = self.probe(model);
        self.clock = if self.facts.clock.load(Ordering::SeqCst) >= self.request.deadline_us {
            2
        } else {
            1
        };
        self.seen = before["journal"] + 1;
        self.sc = before["mode"];
        self.sf = i64::from(self.complete && before["f"] > 0);
        self.sr = i64::from(self.complete && before["r"] > 0);
    }
    fn capture_facts(&mut self) {
        if self.complete && matches!(self.seen, 3 | 5) && self.clock == 2 && self.scope() == 0 {
            self.probed = 1;
            let retired = |seen: i64, index| {
                let who = Self::recipient(index);
                i64::from(
                    seen == 0
                        || (self.facts.birth_is_alive(who.pid, who.birth) == Some(false)
                            && self.facts.group_gone(who.group) == Some(true)),
                )
            };
            self.fp = retired(self.sf, 0);
            self.rp = retired(self.sr, 1);
        }
    }
    fn replay(&mut self, model: &Model, state: &mut State, expected: Option<&str>) -> String {
        self.capture_reads(model);
        let reply = self.send();
        self.capture_facts();
        self.answer = if reply == self.lapsed() {
            1
        } else if reply == NOEXEC {
            2
        } else {
            0
        };
        for action in ["ReadClock", "ReadJournal"] {
            assert!(model.fire(action, state));
        }
        if self.probed == 1 {
            assert!(model.fire("Probe", state));
        }
        if let Some(action) = expected {
            assert!(model.fire(action, state), "{action}: {state:?}");
        } else {
            for action in ["Answer", "AnswerBoot", "AnswerNoExec"] {
                assert!(!model.action_enabled(action, state), "{action}: {state:?}");
            }
        }
        assert_eq!(self.probe(model), *state, "{reply}");
        reply
    }
}

#[test]
fn complete_recipient_recovery_refines_the_model() {
    let model = aterm_models_harness::recipient_closure_model();
    for (case, path, expected) in [
        (0, "Complete Prepare RecordF DeliverF Crash Expire", None),
        (
            1,
            "Complete Prepare RecordF DeliverF ExitF Crash Expire",
            None,
        ),
        (
            2,
            "Complete Prepare RecordF DeliverF ExitF ClearF Crash Expire",
            Some("Answer"),
        ),
        (
            3,
            "Complete Prepare RecordF DeliverF ExitF ClearF RecordR DeliverR Crash Expire",
            None,
        ),
        (
            4,
            "Complete Prepare RecordF DeliverF ExitF ClearF RecordR DeliverR ExitR Crash Expire",
            None,
        ),
        (
            5,
            "Complete Prepare RecordF DeliverF ExitF ClearF RecordR DeliverR ExitR ClearR SealMay Crash Expire",
            Some("Answer"),
        ),
        (
            6,
            "Complete Prepare RecordR DeliverR ExitR ClearR Crash Expire",
            Some("Answer"),
        ),
        (7, "Complete Prepare RecordF Crash Expire", None),
        (
            8,
            "Complete Prepare RecordF ExitF ClearF Crash Expire",
            Some("Answer"),
        ),
        (9, "Complete Prepare Crash Expire", Some("Answer")),
        (10, "Complete Prepare SealOpen Crash", Some("Answer")),
        (11, "Complete Prepare NoExec Crash", Some("AnswerNoExec")),
        (
            12,
            "Complete Prepare RecordR DeliverR Reboot",
            Some("AnswerBoot"),
        ),
        (13, "Complete Prepare Foreign", None),
        (14, "Complete Prepare UnknownBoot", None),
        (
            15,
            "Complete Prepare RecordF DeliverF ExitF ClearF UnknownBirth Crash Expire",
            None,
        ),
        (
            16,
            "Complete Prepare RecordR DeliverR ExitR ClearR UnknownGroup Crash Expire",
            None,
        ),
        (17, "Prepare LegacyDeliver ExitF ClearF Crash Expire", None),
        (
            18,
            "Complete Prepare RecordF DeliverF ExitF ClearF Crash",
            None,
        ),
    ] {
        let mut world = World::new(8500 + case);
        if path.contains("RecordR") {
            world.asked_upgrade();
        }
        let mut state = model.init_state();
        world.steps(
            &model,
            &mut state,
            &path.split_whitespace().collect::<Vec<_>>(),
        );
        let before = (
            world.journal().record_count().unwrap(),
            world.facts.signals.load(Ordering::SeqCst),
            world.facts.watch_count.load(Ordering::SeqCst),
            world.facts.grant_journal_calls.load(Ordering::SeqCst),
        );
        let reply = world.replay(&model, &mut state, expected);
        assert_eq!(
            before,
            (
                world.journal().record_count().unwrap(),
                world.facts.signals.load(Ordering::SeqCst),
                world.facts.watch_count.load(Ordering::SeqCst),
                world.facts.grant_journal_calls.load(Ordering::SeqCst)
            )
        );
        let table = world.handle.ctx.fenced_writes.native.lock().unwrap();
        assert!(table.by_id.is_empty() && table.reservations.is_empty());
        drop(table);
        assert_eq!(
            reply,
            match expected {
                Some("AnswerNoExec") => NOEXEC.to_owned(),
                Some(_) => world.lapsed(),
                None => EPOCH_CHANGED.to_owned(),
            }
        );
        if expected.is_some_and(|a| a != "AnswerNoExec") {
            world.steps(&model, &mut state, &["Fresh"]);
        }
        for law in &model.invariants {
            assert!(
                model.check_invariant(law.name, &state),
                "{}: {path}",
                law.name
            );
        }
    }
}

#[test]
fn a_clock_read_before_the_journal_prevents_recovery_from_becoming_late_in_flight() {
    let model = aterm_models_harness::recipient_closure_model();
    let mut world = World::new(8530);
    let mut state = model.init_state();
    world.steps(
        &model,
        &mut state,
        &["Complete", "Prepare", "RecordF", "DeliverF"],
    );
    world.capture_reads(&model);
    let (entered, waiting) = mpsc::sync_channel(1);
    let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    *world.facts.closure_gate.lock().unwrap() = Some((entered, Arc::clone(&proceed)));
    let (ctx, facts, request, actor) = (
        Arc::clone(&world.handle.ctx),
        Arc::clone(&world.facts),
        world.request.clone(),
        world.actor.clone(),
    );
    let reply = std::thread::scope(|scope| {
        let pending = scope.spawn(move || run(&ctx, &facts, Request::prepare(request), actor));
        waiting
            .recv_timeout(PATIENCE)
            .expect("the journal was read");
        assert!(model.fire("ReadClock", &mut state));
        assert!(model.fire("ReadJournal", &mut state));
        world.steps(&model, &mut state, &["ExitF", "ClearF", "Expire"]);
        *proceed.0.lock().unwrap() = true;
        proceed.1.notify_all();
        pending.join().unwrap()
    });
    assert_eq!(reply, EPOCH_CHANGED);
    assert_eq!(world.probe(&model), state);
    assert!(!model.action_enabled("Answer", &state));
}

#[test]
fn a_recipient_recorded_after_the_expired_snapshot_cannot_gain_execute() {
    let model = aterm_models_harness::recipient_closure_model();
    let mut world = World::new(8531);
    let mut state = model.init_state();
    world.steps(&model, &mut state, &["Complete", "Prepare", "Expire"]);
    world.capture_reads(&model);
    let (entered, waiting) = mpsc::sync_channel(1);
    let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    *world.facts.closure_gate.lock().unwrap() = Some((entered, Arc::clone(&proceed)));
    let (ctx, facts, request, actor) = (
        Arc::clone(&world.handle.ctx),
        Arc::clone(&world.facts),
        world.request.clone(),
        world.actor.clone(),
    );
    let reply = std::thread::scope(|scope| {
        let pending = scope.spawn(move || run(&ctx, &facts, Request::prepare(request), actor));
        waiting
            .recv_timeout(PATIENCE)
            .expect("the journal was read");
        assert!(model.fire("ReadClock", &mut state));
        assert!(model.fire("ReadJournal", &mut state));
        world.steps(&model, &mut state, &["RecordF"]);
        assert!(!model.action_enabled("DeliverF", &state));
        *proceed.0.lock().unwrap() = true;
        proceed.1.notify_all();
        pending.join().unwrap()
    });
    assert_eq!(reply, world.lapsed());
    world.answer = 1;
    world.follow(&model, &mut state, "Answer");
    world.steps(&model, &mut state, &["Fresh"]);
    assert!(model.check_invariant("NoDoubleExec", &state));
}

#[test]
fn omitting_a_live_rollback_recipient_would_authorize_a_second_agent() {
    let model = aterm_models_harness::recipient_closure_model();
    let mut world = World::new(8532);
    world.asked_upgrade();
    let mut state = model.init_state();
    world.steps(
        &model,
        &mut state,
        &[
            "Complete", "Prepare", "RecordF", "DeliverF", "ExitF", "ClearF", "RecordR", "DeliverR",
            "Crash", "Expire",
        ],
    );
    assert_eq!(world.send(), EPOCH_CHANGED);
    let mut incomplete = world.history().unwrap();
    incomplete.rollback = None;
    *world.facts.closure_view.lock().unwrap() = Some(Closure::Recipients(incomplete));
    assert_eq!(world.send(), world.lapsed());
    world.fresh = true;
    assert!(!model.check_invariant("NoDoubleExec", &world.probe(&model)));
}

#[test]
fn a_snapshot_taken_before_the_clock_would_hide_a_delivered_recipient() {
    let model = aterm_models_harness::recipient_closure_model();
    let mut world = World::new(8535);
    let mut state = model.init_state();
    world.steps(&model, &mut state, &["Complete", "Prepare"]);
    let stale = world.history().unwrap();
    world.steps(&model, &mut state, &["RecordF", "DeliverF", "Expire"]);
    assert_eq!(world.replay(&model, &mut state, None), EPOCH_CHANGED);
    *world.facts.closure_view.lock().unwrap() = Some(Closure::Recipients(stale));
    assert_eq!(world.send(), world.lapsed());
    world.fresh = true;
    assert!(!model.check_invariant("NoDoubleExec", &world.probe(&model)));
}

#[test]
fn pid_reuse_is_not_rebirth_of_the_recorded_recipient() {
    let model = aterm_models_harness::recipient_closure_model();
    let mut world = World::new(8533);
    let mut state = model.init_state();
    world.steps(
        &model,
        &mut state,
        &[
            "Complete", "Prepare", "RecordF", "DeliverF", "ExitF", "ClearF", "Crash", "Expire",
        ],
    );
    let who = World::recipient(0);
    let mut replacement = process(who.pid, 1, who.group + 200, who.session, 9);
    replacement.birth = who.birth + 1;
    world
        .facts
        .processes
        .lock()
        .unwrap()
        .insert(who.pid, replacement.clone());
    assert_eq!(
        world.replay(&model, &mut state, Some("Answer")),
        world.lapsed()
    );
    assert_eq!(
        world.facts.processes.lock().unwrap().get(&who.pid),
        Some(&replacement)
    );
}

#[test]
fn inconsistent_complete_histories_do_not_turn_missing_recipients_into_proof() {
    let model = aterm_models_harness::recipient_closure_model();
    let mut world = World::new(8534);
    let mut state = model.init_state();
    world.steps(
        &model,
        &mut state,
        &[
            "Complete", "Prepare", "RecordF", "DeliverF", "Crash", "Expire",
        ],
    );
    for recorded in [
        Recorded::Prepared,
        Recorded::NoExec,
        Recorded::Sealed { may_execute: false },
    ] {
        let mut inconsistent = world.history().unwrap();
        inconsistent.state = recorded;
        *world.facts.closure_view.lock().unwrap() = Some(Closure::Recipients(inconsistent));
        assert_eq!(world.send(), EPOCH_CHANGED);
    }
    let mut empty = world.history().unwrap();
    empty.forward = None;
    *world.facts.closure_view.lock().unwrap() = Some(Closure::Recipients(empty));
    assert_eq!(world.send(), EPOCH_CHANGED);
}

#[test]
fn raw_birth_probe_distinguishes_this_identity_without_an_image() {
    let pid = std::process::id();
    let birth = aterm_uds::process::read_birth(i32::try_from(pid).unwrap())
        .unwrap()
        .0;
    assert_eq!(process_birth_alive(pid, birth), Some(true));
    assert_eq!(process_birth_alive(pid, birth + 1), Some(false));
    assert_eq!(process_birth_alive(0, birth), None);
    assert_eq!(process_birth_alive(1, birth), None);
}
