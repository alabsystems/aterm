// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's `lapsed` answer (`historical_prepare`) bound to the `NativeHistoricalClosure`
//! model by lockstep projection. The issuer's steps are the model's premises: the test drives
//! them on the fake kernel (its journal, clock, custody and boot) or books them (a delivered
//! grant, its grantee, the issuing epoch's end, the harness's fresh plan). The reader's steps
//! are one real replay of the refused prepare, which reads the clock and then the journal.

use super::*;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;

const EPOCH_CHANGED: &str = "ERR native host epoch changed\n";

const ANSWERS: [&str; 3] = ["AnswerLapsed", "AnswerBoot", "AnswerNoExec"];

/// The real host under one cold restart prepare an earlier epoch minted, and the history of
/// it the model keeps and the host does not.
pub(super) struct Host {
    handle: crate::session_store::SessionHandle,
    facts: Facts,
    request: Prepare,
    actor: SupervisorInputCaller,
    /// The machine and boot the issuer ran on.
    origin: BootStamp,
    /// The custody the stamp names, once a fresh one replaced it.
    set_aside: Option<BTreeMap<String, JournalEntry>>,
    legacy: bool,
    live: bool,
    granted: std::cell::Cell<bool>,
    child: std::cell::Cell<bool>,
    rollback_granted: bool,
    rollback: bool,
    clock: i64,
    seen: i64,
    stood: bool,
    answer: i64,
    fresh: bool,
}

impl Host {
    fn new(local: u64) -> Self {
        let (handle, facts, mut request, actor) = closed_restart::fixture(local);
        cold_restart(&mut request);
        // Minted by an earlier epoch of this host: every replay here is refused, and reaches
        // the historical answer.
        request.host_epoch = "34".repeat(16);
        assert_ne!(host_epoch(), Some(request.host_epoch.as_str()));
        let origin = facts.boot().unwrap();
        Self {
            handle,
            facts,
            request,
            actor,
            origin,
            set_aside: None,
            legacy: false,
            live: true,
            granted: std::cell::Cell::new(false),
            child: std::cell::Cell::new(false),
            rollback_granted: false,
            rollback: false,
            clock: 0,
            seen: 0,
            stood: false,
            answer: 0,
            fresh: false,
        }
    }

    fn binding(&self) -> Binding {
        Binding {
            id: self.request.id.clone(),
            capability: "cd".repeat(32),
        }
    }

    fn lapsed(&self) -> String {
        aterm_agent::harness::native_act::lapsed_reply(&self.request.id).unwrap()
    }

    /// Project the host onto the model: the prepare's record in the custody its stamp names,
    /// its stamps, the fake kernel's clock, boot and custody, and the history booked beside it.
    pub(super) fn probe(&self, model: &Model) -> State {
        let journal = {
            let current = self.facts.journal.lock().unwrap();
            let named = self.set_aside.as_ref().unwrap_or(&current);
            match named.get(&self.request.id) {
                None => 0,
                Some(entry) => {
                    assert_eq!(
                        entry.prepare, self.request,
                        "the record is the exact prepare's"
                    );
                    match entry.state {
                        JournalState::Prepared => 1,
                        JournalState::MayExecute => 2,
                        JournalState::Sealed { may_execute: false } => 3,
                        JournalState::Sealed { may_execute: true } => 4,
                        JournalState::NoExec => 5,
                    }
                }
            }
        };
        let mut state = model.init_state();
        for (var, value) in [
            ("journal", journal),
            ("bstamp", i64::from(self.request.boot.is_some())),
            ("cstamp", i64::from(self.request.custody.is_some())),
            ("legacy", i64::from(self.legacy)),
            ("moved", i64::from(self.set_aside.is_some())),
            ("live", i64::from(self.live)),
            (
                "late",
                i64::from(self.facts.clock.load(Ordering::SeqCst) >= self.request.deadline_us),
            ),
            (
                "boot",
                i64::from(self.facts.boot() != Some(self.origin.clone())),
            ),
            ("granted", i64::from(self.granted.get())),
            ("child", i64::from(self.child.get())),
            ("rollback_granted", i64::from(self.rollback_granted)),
            ("rollback", i64::from(self.rollback)),
            ("clock", self.clock),
            ("seen", self.seen),
            ("stood", i64::from(self.stood)),
            ("answer", self.answer),
            ("fresh", i64::from(self.fresh)),
        ] {
            state.insert(var, value);
        }
        state
    }

    fn send(&self) -> String {
        run(
            &self.handle.ctx,
            &self.facts,
            Request::prepare(self.request.clone()),
            self.actor.clone(),
        )
    }

    /// Book the reader's clock read as the clock stands in `now`.
    fn read_clock(&mut self, now: &State) {
        self.clock = if now["late"] == 1 { 2 } else { 1 };
    }

    /// Book the reader's journal read as the journal stands in `now`: the stamped custody's
    /// record, or an absence once another custody replaced it.
    fn read_journal(&mut self, now: &State) {
        self.seen = if now["moved"] == 1 {
            7
        } else {
            now["journal"] + 1
        };
        self.stood = now["journal"] == 5;
    }

    /// Book the answer `reply` gives from the booked reads: from the journal where the record or
    /// an absence under the custody stamp proves it, else from the other boot the boot stamp
    /// names.
    fn answered(&mut self, model: &Model, reply: &str) {
        let now = self.probe(model);
        if reply == NOEXEC {
            self.answer = 3;
        } else if let Some(Reply::Lapsed(id)) = Reply::parse(reply) {
            assert_eq!(id, self.request.id);
            let (clock, seen) = (self.clock, self.seen);
            let journal = seen == 4
                || (seen == 2 && clock == 2)
                || (seen == 1 && now["cstamp"] == 1 && clock == 2);
            self.answer = if !journal && now["bstamp"] == 1 && now["boot"] == 1 {
                2
            } else {
                1
            };
        }
    }

    /// One real replay, which reads the clock and then the journal while nothing else moves,
    /// booked with the answer it gave.
    fn read(&mut self, model: &Model) -> String {
        let now = self.probe(model);
        self.read_clock(&now);
        self.read_journal(&now);
        let reply = self.send();
        self.answered(model, &reply);
        reply
    }

    /// The model takes `action`, and the projection must be where it went.
    fn follow(&self, model: &Model, state: &mut State, action: &str) {
        assert!(model.fire(action, state), "{action} from {state:?}");
        assert_eq!(self.probe(model), *state, "after {action}");
    }

    /// Drive each premise on the fake kernel or the books, in lockstep with the model.
    fn steps(&mut self, model: &Model, state: &mut State, actions: &[&str]) {
        for action in actions {
            let (prepare, binding) = (&self.request, self.binding());
            match *action {
                "StampBoot" => self.request.boot = self.facts.boot(),
                "StampCustody" => self.request.custody = self.facts.custody(),
                "Legacy" => self.legacy = true,
                "Record" => assert!(self.facts.record_prepared(prepare, &binding)),
                "May" => assert!(self.facts.record_may_execute(prepare, &binding)),
                "Deliver" => {
                    self.granted.set(true);
                    self.child.set(true);
                }
                "DeliverRollback" => {
                    self.rollback_granted = true;
                    self.rollback = true;
                }
                "SealOpen" | "SealMay" => assert!(self.facts.record_sealed(prepare, &binding)),
                "NoExec" => {
                    assert!(
                        self.facts
                            .record_noexec(prepare, &binding, ImagePhase::Forward)
                    );
                }
                "Expire" => self
                    .facts
                    .clock
                    .store(self.request.deadline_us, Ordering::SeqCst),
                "Crash" => self.live = false,
                "ChildExits" => self.child.set(false),
                "RollbackExits" => self.rollback = false,
                "Reboot" => {
                    *self.facts.boot.lock().unwrap() = Some(BootStamp {
                        host: self.origin.host.clone(),
                        boot: "e7".repeat(16),
                    });
                    self.live = false;
                    self.child.set(false);
                    self.rollback = false;
                }
                "Move" => {
                    self.set_aside = Some(std::mem::take(&mut *self.facts.journal.lock().unwrap()));
                    *self.facts.custody.lock().unwrap() = Some("d6".repeat(32));
                }
                "Fresh" => self.fresh = true,
                other => unreachable!("{other} is not a premise"),
            }
            self.follow(model, state, action);
        }
    }

    /// The reader's pass: the model reads the clock, then the journal, and gives `answer` or
    /// none, exactly as the real replay does; returns its reply.
    fn replay(&mut self, model: &Model, state: &mut State, answer: Option<&str>) -> String {
        let reply = self.read(model);
        for action in ["ReadClock", "ReadJournal"] {
            assert!(model.fire(action, state), "{action} from {state:?}");
        }
        match answer {
            Some(action) => assert!(model.fire(action, state), "{action} from {state:?}"),
            None => {
                for action in ANSWERS {
                    assert!(
                        !model.action_enabled(action, state),
                        "{action} from {state:?}"
                    );
                }
            }
        }
        assert_eq!(self.probe(model), *state, "{reply}");
        reply
    }

    fn holds(&self, model: &Model, state: &State) {
        for law in &model.invariants {
            assert!(model.check_invariant(law.name, state), "{}", law.name);
        }
    }
}

#[test]
fn the_lapsed_answer_refines_the_historical_closure_model() {
    let model = aterm_models_harness::native_historical_closure_model();
    let lapsed = Some("AnswerLapsed");
    let boot = Some("AnswerBoot");
    let noexec = Some("AnswerNoExec");
    let paths: [(&[&str], Option<&str>, &[&str]); 16] = [
        // A stamped absence, and a prepared record, before the deadline and after it.
        (&["StampBoot", "StampCustody", "Crash"], None, &[]),
        (
            &["StampBoot", "StampCustody", "Crash", "Expire"],
            lapsed,
            &["Fresh"],
        ),
        // The custody stamp alone proves the absence (a boot stamp alone the reboot below).
        (&["StampCustody", "Crash", "Expire"], lapsed, &["Fresh"]),
        (&["Record", "Crash"], None, &[]),
        (&["Record", "Crash", "Expire"], lapsed, &["Fresh"]),
        // A seal with no possible execution, at any clock.
        (&["Record", "SealOpen", "Crash"], lapsed, &["Fresh"]),
        // Possible execution on this boot holds, sealed or not, its grantee gone or not.
        (
            &[
                "StampBoot",
                "StampCustody",
                "Record",
                "May",
                "Deliver",
                "Crash",
                "Expire",
            ],
            None,
            &[],
        ),
        (
            &[
                "Record",
                "May",
                "Deliver",
                "SealMay",
                "Crash",
                "ChildExits",
                "Expire",
            ],
            None,
            &[],
        ),
        // A positive no-grant is served as one, on this boot or another.
        (&["Record", "NoExec", "Crash", "Expire"], noexec, &[]),
        (
            &["StampBoot", "StampCustody", "Record", "NoExec", "Reboot"],
            noexec,
            &[],
        ),
        // Another boot of this machine ends every process of the stamped one: a record needs
        // the boot stamp alone, an absence the custody stamp too.
        (
            &[
                "StampBoot",
                "StampCustody",
                "Record",
                "May",
                "Deliver",
                "Reboot",
            ],
            boot,
            &["Fresh"],
        ),
        (&["StampBoot", "Record", "Reboot"], boot, &["Fresh"]),
        (&["StampBoot", "StampCustody", "Reboot"], boot, &["Fresh"]),
        // An unstamped absence proves nothing, however late: a legacy issuer granted without one.
        (&["Legacy", "Deliver", "Crash", "Expire"], None, &[]),
        // Nor does an absence read from a custody that replaced the stamped one.
        (
            &[
                "StampBoot",
                "StampCustody",
                "Record",
                "May",
                "Deliver",
                "Crash",
                "Move",
                "Expire",
            ],
            None,
            &[],
        ),
        (
            &[
                "StampBoot",
                "StampCustody",
                "Crash",
                "Move",
                "Expire",
                "Reboot",
            ],
            None,
            &[],
        ),
    ];
    for (case, (before, answer, after)) in paths.into_iter().enumerate() {
        let mut host = Host::new(8410 + case as u64);
        let mut state = model.init_state();
        assert_eq!(host.probe(&model), state);
        host.steps(&model, &mut state, before);
        let reply = host.replay(&model, &mut state, answer);
        let expected = match answer {
            Some("AnswerNoExec") => NOEXEC.to_string(),
            Some(_) => host.lapsed(),
            None => EPOCH_CHANGED.to_string(),
        };
        assert_eq!(reply, expected, "{before:?}");
        host.steps(&model, &mut state, after);
        host.holds(&model, &state);
    }
}

#[test]
fn historical_closure_keeps_a_rollback_recipient_in_the_execution_history() {
    let model = aterm_models_harness::native_historical_closure_model();
    for (case, tail, answer) in [
        (0, &["Crash", "Expire"][..], None),
        (
            1,
            &["RollbackExits", "SealMay", "Crash", "Expire"][..],
            None,
        ),
        (2, &["Reboot"][..], Some("AnswerBoot")),
    ] {
        let mut host = Host::new(8460 + case);
        let mut state = model.init_state();
        host.steps(
            &model,
            &mut state,
            &[
                "StampBoot",
                "Record",
                "May",
                "Deliver",
                "ChildExits",
                "DeliverRollback",
            ],
        );
        host.steps(&model, &mut state, tail);
        let reply = host.replay(&model, &mut state, answer);
        if answer.is_some() {
            assert_eq!(reply, host.lapsed());
            host.steps(&model, &mut state, &["Fresh"]);
        } else {
            assert_eq!(reply, EPOCH_CHANGED);
        }
        host.holds(&model, &state);
        if case == 0 {
            // Negative control: treating the forward child's exit as no possible execution
            // makes the real reader authorize a fresh plan beside the live rollback child.
            *host.facts.closure_view.lock().unwrap() = Some(Closure::Exact(Recorded::Prepared));
            assert_eq!(host.read(&model), host.lapsed());
            let bad = host.probe(&model);
            assert!(!model.successors("AnswerLapsed", &state).contains(&bad));
            assert!(!model.check_invariant("LapsedNeverGranted", &bad));
            host.fresh = true;
            assert!(!model.check_invariant("NoDoubleExec", &host.probe(&model)));
        }
    }
}

#[test]
fn the_host_reads_the_clock_before_the_journal_a_grant_lands_beside() {
    let model = aterm_models_harness::native_historical_closure_model();
    for expired in [false, true] {
        let mut host = Host::new(8432 + u64::from(expired));
        let (entered, waiting) = mpsc::sync_channel(1);
        let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        *host.facts.closure_gate.lock().unwrap() = Some((entered, Arc::clone(&proceed)));
        let mut state = model.init_state();
        host.steps(
            &model,
            &mut state,
            if expired {
                &["Record", "Expire"][..]
            } else {
                &["Record"][..]
            },
        );
        let start = host.probe(&model);
        host.read_clock(&start);
        host.read_journal(&start);
        let (ctx, facts, request, actor) = (
            Arc::clone(&host.handle.ctx),
            &host.facts,
            host.request.clone(),
            host.actor.clone(),
        );
        let binding = host.binding();
        let reply = std::thread::scope(|scope| {
            let replay = scope.spawn(move || run(&ctx, facts, Request::prepare(request), actor));
            // The replay has read the clock, then the journal, and is held before it answers.
            waiting
                .recv_timeout(PATIENCE)
                .expect("the replay read the journal");
            for action in ["ReadClock", "ReadJournal"] {
                assert!(model.fire(action, &mut state), "{action}");
            }
            assert_eq!(host.probe(&model), state);
            assert!(facts.record_may_execute(&host.request, &binding));
            host.follow(&model, &mut state, "May");
            if expired {
                // Possible execution recorded after the deadline is never delivered.
                assert!(!model.action_enabled("Deliver", &state));
            } else {
                // A grant delivered in time, and its deadline passing, both after the clock read.
                host.granted.set(true);
                host.child.set(true);
                host.follow(&model, &mut state, "Deliver");
                facts
                    .clock
                    .store(host.request.deadline_us, Ordering::SeqCst);
                host.follow(&model, &mut state, "Expire");
            }
            *proceed.0.lock().unwrap() = true;
            proceed.1.notify_all();
            replay.join().unwrap()
        });
        host.answered(&model, &reply);
        if expired {
            assert_eq!(reply, host.lapsed());
            host.follow(&model, &mut state, "AnswerLapsed");
            host.steps(&model, &mut state, &["Fresh"]);
        } else {
            assert_eq!(reply, EPOCH_CHANGED);
            for action in ANSWERS {
                assert!(!model.action_enabled(action, &state), "{action}");
            }
            assert_eq!(host.probe(&model), state);
        }
        host.holds(&model, &state);
    }
}

#[test]
fn a_journal_read_before_the_clock_would_answer_a_delivered_grant() {
    let model = aterm_models_harness::native_historical_closure_model();
    let mut host = Host::new(8430);
    let (entered, waiting) = mpsc::sync_channel(1);
    let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    host.facts.grant_journal_gate = Some((entered, Arc::clone(&proceed)));
    let mut state = model.init_state();
    host.steps(&model, &mut state, &["Record"]);
    // The grant's append begins, and a journal read taken before it lands sees Prepared.
    let first = std::thread::scope(|scope| {
        let (facts, request, binding) = (&host.facts, &host.request, host.binding());
        let append = scope.spawn(move || facts.record_may_execute(request, &binding));
        waiting
            .recv_timeout(PATIENCE)
            .expect("the grant's append began");
        let first = facts.historical_closure(request).unwrap();
        *proceed.0.lock().unwrap() = true;
        proceed.1.notify_all();
        assert!(append.join().unwrap());
        first
    });
    assert_eq!(first, Closure::Exact(Recorded::Prepared));
    host.follow(&model, &mut state, "May");
    host.steps(&model, &mut state, &["Deliver", "Crash", "Expire"]);
    // The host reads the clock first, and then the journal holds the grant: it refuses.
    assert_eq!(host.send(), EPOCH_CHANGED);
    // Negative control: answered from the journal read before the clock, the same host says
    // `lapsed` for a delivered grant, which the projection refuses.
    *host.facts.closure_view.lock().unwrap() = Some(first);
    assert_eq!(host.read(&model), host.lapsed());
    let bad = host.probe(&model);
    assert!(!model.check_invariant("LapsedNeverGranted", &bad));
    let mut read = state.clone();
    for action in ["ReadClock", "ReadJournal"] {
        assert!(model.fire(action, &mut read));
    }
    assert!(!model.successors("AnswerLapsed", &read).contains(&bad));
}

#[test]
fn an_absence_read_from_a_replaced_custody_would_answer_a_delivered_grant() {
    let model = aterm_models_harness::native_historical_closure_model();
    let mut host = Host::new(8431);
    let mut state = model.init_state();
    host.steps(
        &model,
        &mut state,
        &[
            "StampBoot",
            "StampCustody",
            "Record",
            "May",
            "Deliver",
            "Crash",
            "Move",
            "Expire",
        ],
    );
    let unread = state.clone();
    // The absence is read from a custody the stamp does not name: the host refuses.
    assert_eq!(host.replay(&model, &mut state, None), EPOCH_CHANGED);
    // Negative control: with the custody check gone, the same host reads that absence as the
    // stamped custody's and says `lapsed`; the projection refuses it, and the fresh plan it
    // licenses runs beside the grantee.
    (host.clock, host.seen, host.stood) = (0, 0, false);
    assert_eq!(host.probe(&model), unread);
    *host.facts.closure_view.lock().unwrap() = Some(Closure::Absent(host.request.custody.clone()));
    assert_eq!(host.read(&model), host.lapsed());
    let bad = host.probe(&model);
    assert!(!model.check_invariant("LapsedNeverGranted", &bad));
    assert!(!model.successors("AnswerLapsed", &state).contains(&bad));
    host.fresh = true;
    assert!(!model.check_invariant("NoDoubleExec", &host.probe(&model)));
}
