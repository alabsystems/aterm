// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_containment::consent::DebuggerMode;

use super::*;
use std::sync::mpsc;

fn synthetic_state() -> ConsentState {
    let mut state = ConsentState::inert();
    state.probes.live = true;
    state
}

fn grant() -> FdaProbe {
    consent::classify_probe(consent::ProbeOutcome::Ok)
}

fn complete(state: &ConsentState, ticket: Ticket, probe: FdaProbe, at: Instant) {
    state
        .shared
        .finish(&state.shared.fda, ticket, Some(probe), at);
}

fn debugger_entry() -> DebuggerEntry {
    DebuggerEntry {
        probe: DebuggerProbe::refused(DebuggerProbeLabel::TimedOut),
        self_hardened: SelfHardening::Unknown,
    }
}

fn devtools_entry() -> DevToolsEntry {
    DevToolsEntry {
        probe: DevToolProbe::refused(DevToolProbeLabel::TimedOut),
        list: DevToolList::refused(DevToolListLabel::TimedOut),
        subject: None,
    }
}

fn probe_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        AsyncConsentProbePublication {
            const Buggy = 0;
            const Serve = 0;
            var epoch = 0;
            var request_epoch = 0;
            var flight = 0;
            var cached = 0;
            var cached_epoch = 0;
            var ready = 1;
            action Start when (flight == 0 && (cached == 0 || Serve == 1) && ready == 1) {
                flight = 1;
                request_epoch = epoch;
                ready = 0;
            }
            action Cooldown when (ready == 0) {
                ready = 1;
            }
            action Invalidate when (epoch <= 1) {
                epoch = epoch + 1;
                cached = 0;
                cached_epoch = 0;
            }
            action Complete when (flight == 1) {
                flight = 0;
                cached = if request_epoch == epoch || Buggy == 1 { 1 } else { 0 };
                cached_epoch = if request_epoch == epoch || Buggy == 1 { request_epoch } else { 0 };
            }
            action Expire when (cached == 1) {
                cached = 0;
                cached_epoch = 0;
            }
            invariant Bounds: epoch <= 2 && request_epoch <= 2 && flight <= 1 && cached <= 1 && ready <= 1;
            invariant CurrentPublication: cached == 0 || cached_epoch == epoch;
            invariant OneWorkerOrCache: Serve == 1 || flight + cached <= 1;
        }
    }
}

fn probe_model_serving(serve: bool) -> aterm_spec::derive::Model {
    aterm_spec::interp::with_consts(&probe_model(), &[("Serve", i64::from(serve))])
}

#[test]
fn async_probe_model_proves_and_catches_stale_publication() {
    for serve in [false, true] {
        let model = probe_model_serving(serve);
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }
}

fn admission_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        AsyncConsentProbeAdmission {
            const Buggy = 0;
            var flight = 0;
            var ready = 1;
            var elapsed = 1;
            action Start when (flight == 0 && ready == 1) {
                flight = 1;
                ready = 0;
                elapsed = 0;
            }
            action Complete when (flight == 1) { flight = 0; }
            action Invalidate when (ready == 0) {
                ready = if Buggy == 1 { 1 } else { 0 };
            }
            action Cooldown when (elapsed == 0) {
                ready = 1;
                elapsed = 1;
            }
            action Forget when (ready == 0) {
                ready = 1;
                elapsed = 1;
            }
            invariant FloorSurvivesInvalidation: ready == 0 || elapsed == 1;
            invariant Bounds: flight <= 1 && ready <= 1 && elapsed <= 1;
        }
    }
}

#[test]
fn admission_model_proves_and_catches_focus_churn_bypassing_the_floor() {
    let model = admission_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn focus_churn_conforms_to_the_admission_floor() {
    fn churn<E: Clone>(cell: &SlotCell<E>, value: E, forget: Option<&dyn Fn()>) {
        let model = admission_model();
        let now = Instant::now();
        let ttl = Duration::from_secs(5);
        let mut expected = model.init_state();
        let (_, ticket) = cell.read(now, ttl, true, SlotLock::Try);
        assert!(model.fire("Start", &mut expected));
        assert!(cell.complete(ticket.expect("a cold read admits"), Some(value), now));
        assert!(model.fire("Complete", &mut expected));
        for index in 0..100 {
            let at = now + Duration::from_millis(index);
            let _ = cell.retire();
            assert!(model.fire("Invalidate", &mut expected));
            let (answer, ticket) = cell.read(at, ttl, true, SlotLock::Try);
            assert!(answer.is_none(), "{:?}: a retired answer", cell.kind);
            assert_eq!(ticket.is_some(), model.action_enabled("Start", &expected));
            assert!(!cell.slot.lock_or_recover().in_flight);
            assert_eq!(cell.next_refresh_deadline(at, ttl), now + PROBE_RETRY_FLOOR);
            let mut cleared_floor = expected.clone();
            cleared_floor.insert("ready", 1);
            assert!(!model.check_invariant("FloorSurvivesInvalidation", &cleared_floor));
        }
        let at = if let Some(forget) = forget {
            forget();
            assert!(model.fire("Forget", &mut expected));
            let mut as_invalidation = expected.clone();
            as_invalidation.insert("elapsed", 0);
            assert!(!model.check_invariant("FloorSurvivesInvalidation", &as_invalidation));
            now + Duration::from_millis(100)
        } else {
            assert!(model.fire("Cooldown", &mut expected));
            now + PROBE_RETRY_FLOOR
        };
        let (_, ticket) = cell.read(at, ttl, true, SlotLock::Try);
        assert_eq!(ticket.is_some(), model.action_enabled("Start", &expected));
        assert!(
            ticket.is_some(),
            "{:?}: the read after the floor",
            cell.kind
        );
    }
    let state = synthetic_state();
    churn(&state.shared.fda, grant(), None);
    churn(&state.shared.debugger, debugger_entry(), None);
    churn(&state.shared.devtools, devtools_entry(), None);
    churn(&state.shared.census, Claimants::default(), None);
    let state = synthetic_state();
    churn(
        &state.shared.census,
        Claimants::default(),
        Some(&|| state.invalidate_census()),
    );
}

#[test]
fn a_publication_between_read_and_park_is_not_lost() {
    let state = synthetic_state();
    let now = Instant::now();
    let ttl = Duration::from_secs(5);
    let seen = state.publication();
    let ((probe, _), request) = state.read_at(ProbeGate::on(), ttl, now);
    assert_eq!(probe.label, ProbeLabel::Pending, "the read saw no answer");
    complete(&state, request.expect("a cold read admits"), grant(), now);
    assert!(
        state.wait_for_change(seen, &[ChangeKind::Fda], aterm_spec::wait::MUST_HAPPEN),
        "the completion between the read and the park is seen"
    );
    assert!(
        !state.wait_for_change(
            seen,
            &[ChangeKind::Debugger, ChangeKind::Policy],
            Duration::ZERO
        ),
        "only the watched kinds count"
    );

    let late = state.publication();
    assert!(!state.wait_for_change(late, &[ChangeKind::Fda], Duration::ZERO));

    let (tx, rx) = mpsc::channel();
    let waiter = state.clone();
    std::thread::spawn(move || {
        let _ = tx.send(waiter.wait_for_change(
            late,
            &[ChangeKind::Fda],
            aterm_spec::wait::MUST_HAPPEN,
        ));
    });
    state.invalidate();
    assert!(
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the waiter returns"),
        "the retired grant reached the parked waiter"
    );
}

#[test]
fn a_no_op_retire_publishes_nothing() {
    let seq = |state: &ConsentState, kind: ChangeKind| state.publication().0[kind as usize];
    let closed = ProbeGate {
        enabled: false,
        check: true,
    };
    let ttl = Duration::from_secs(5);
    let state = synthetic_state();

    let before = seq(&state, ChangeKind::Debugger);
    for _ in 0..3 {
        let _ = state.debugger(closed);
    }
    assert_eq!(
        seq(&state, ChangeKind::Debugger),
        before,
        "cold: nothing to forget"
    );
    state
        .shared
        .debugger
        .seed_for_test(debugger_entry(), Instant::now());
    for _ in 0..3 {
        let _ = state.debugger(closed);
    }
    assert_eq!(seq(&state, ChangeKind::Debugger), before + 1, "warm: once");

    let before = seq(&state, ChangeKind::Devtools);
    let _ = state.devtools(closed, false);
    assert_eq!(seq(&state, ChangeKind::Devtools), before);
    state
        .shared
        .devtools
        .seed_for_test(devtools_entry(), Instant::now());
    let _ = state.devtools(ProbeGate::on(), true);
    let _ = state.devtools(ProbeGate::on(), true);
    assert_eq!(seq(&state, ChangeKind::Devtools), before + 1);

    let before = seq(&state, ChangeKind::Census);
    state.invalidate_census();
    assert_eq!(seq(&state, ChangeKind::Census), before);
    state
        .shared
        .census
        .seed_for_test(Claimants::default(), Instant::now());
    state.invalidate_census();
    state.invalidate_census();
    assert_eq!(seq(&state, ChangeKind::Census), before + 1);

    let before = seq(&state, ChangeKind::Fda);
    for _ in 0..3 {
        state.invalidate();
        let _ = state.read_at(closed, ttl, Instant::now());
    }
    assert_eq!(
        seq(&state, ChangeKind::Fda),
        before,
        "cold: nothing to forget"
    );
    {
        let _held = state.shared.fda.slot.lock_or_recover();
        for _ in 0..3 {
            state.invalidate();
        }
    }
    assert_eq!(seq(&state, ChangeKind::Fda), before, "contended and cold");
    let now = Instant::now();
    let (_, request) = state.read_at(ProbeGate::on(), ttl, now);
    complete(&state, request.expect("admitted"), grant(), now);
    assert_eq!(seq(&state, ChangeKind::Fda), before + 1, "the completion");
    state.invalidate();
    state.invalidate();
    assert_eq!(seq(&state, ChangeKind::Fda), before + 2, "the grant, once");

    let before = seq(&state, ChangeKind::Policy);
    state.publish_policy((*state.policy()).clone());
    assert_eq!(seq(&state, ChangeKind::Policy), before, "an equal policy");
    let off = crate::app_config::Config {
        privacy: Some(crate::app_config::PrivacyConfig {
            enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };
    state.publish_policy(ConsentPolicy::from_config(&off));
    assert_eq!(seq(&state, ChangeKind::Policy), before + 1);
}

fn section(privacy: crate::app_config::PrivacyConfig) -> ConsentPolicy {
    ConsentPolicy::from_config(&crate::app_config::Config {
        privacy: Some(privacy),
        ..Default::default()
    })
}

fn checks_off() -> ConsentPolicy {
    section(crate::app_config::PrivacyConfig {
        check: Some(false),
        ..Default::default()
    })
}

#[test]
fn a_worker_probes_only_under_the_published_gate() {
    use crate::app_config::PrivacyConfig;
    use std::sync::atomic::AtomicUsize;
    static FDA: AtomicUsize = AtomicUsize::new(0);
    static DEBUGGER: AtomicUsize = AtomicUsize::new(0);
    static CENSUS: AtomicUsize = AtomicUsize::new(0);
    static DEVTOOLS: AtomicUsize = AtomicUsize::new(0);
    fn fda(_gate: ProbeGate) -> FdaProbe {
        FDA.fetch_add(1, Ordering::AcqRel);
        grant()
    }
    fn debugger(_gate: ProbeGate) -> DebuggerProbe {
        DEBUGGER.fetch_add(1, Ordering::AcqRel);
        DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: consent::DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        }
    }
    fn census(_bundle_id: &str) -> Claimants {
        CENSUS.fetch_add(1, Ordering::AcqRel);
        Claimants::default()
    }
    const SUBJECT: &str = "com.example.subject";
    fn cold_subject() -> Option<&'static str> {
        None
    }
    fn warm_subject() -> Option<&'static str> {
        DEVTOOLS.fetch_add(1, Ordering::AcqRel);
        Some(SUBJECT)
    }
    fn devtools(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        DEVTOOLS.fetch_add(1, Ordering::AcqRel);
        DevToolProbe::refused(DevToolProbeLabel::TimedOut)
    }
    fn list(_gate: ProbeGate, _subject: &str) -> DevToolList {
        DEVTOOLS.fetch_add(1, Ordering::AcqRel);
        DevToolList::refused(DevToolListLabel::RefusedDisabled)
    }
    fn held<E>(cell: &SlotCell<E>) -> (bool, bool) {
        let slot = cell.slot.lock_or_recover();
        (slot.in_flight, slot.entry.is_some())
    }
    let runs = || [&FDA, &DEBUGGER, &CENSUS, &DEVTOOLS].map(|arm| arm.load(Ordering::Acquire));
    let cases = [
        (section(PrivacyConfig::default()), [1, 1, 1, 3]),
        (checks_off(), [0, 0, 1, 0]),
        (
            section(PrivacyConfig {
                enabled: Some(false),
                ..Default::default()
            }),
            [0, 0, 0, 0],
        ),
        (
            section(PrivacyConfig {
                observer: Some(false),
                ..Default::default()
            }),
            [1, 1, 1, 0],
        ),
    ];
    for (published, expected) in cases {
        let mut state = synthetic_state();
        state.probes.fda = fda;
        state.probes.debugger = debugger;
        state.probes.census = census;
        state.probes.devtools = devtools;
        state.probes.devtools_list = list;
        state.probes.subject = cold_subject;
        state.probes.warm_subject = warm_subject;
        state.publish_policy(published.clone());
        let before = runs();
        let seen = state.publication();
        let (probe, _) = state.fda(ProbeGate::on(), Duration::from_secs(5));
        assert_eq!(probe.label, ProbeLabel::Pending, "an FDA worker is out");
        assert!(
            state.debugger(ProbeGate::on()).pending(),
            "a debugger worker is out"
        );
        assert!(
            state.claimants(Some(SUBJECT), true).1,
            "a census worker is out"
        );
        assert!(
            state.devtools(ProbeGate::on(), false).pending(),
            "a Developer Tools worker is out"
        );
        for kind in [
            ChangeKind::Fda,
            ChangeKind::Debugger,
            ChangeKind::Census,
            ChangeKind::Devtools,
        ] {
            assert!(
                state.wait_for_change(seen, &[kind], aterm_spec::wait::MUST_HAPPEN),
                "the {kind:?} worker frees its slot under {published:?}"
            );
        }
        let after = runs();
        let ran: [usize; 4] = std::array::from_fn(|arm| after[arm] - before[arm]);
        assert_eq!(ran, expected, "the arms run under {published:?}");
        assert_eq!(
            [
                held(&state.shared.fda),
                held(&state.shared.debugger),
                held(&state.shared.census),
                held(&state.shared.devtools),
            ],
            expected.map(|runs| (false, runs > 0)),
            "every slot is free, with an answer only from a probe, under {published:?}"
        );
    }
}

fn at_the_first_bump(
    state: &ConsentState,
    publish: impl FnOnce() + Send + 'static,
) -> (bool, ProbeGate) {
    let seen = state.publication();
    let held = state.shared.publications.lock.lock_or_recover();
    std::thread::spawn(publish);
    let bumped = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the publisher's first bump",
        || Some(state.publication()).filter(|now| *now != seen),
    );
    let gate = state
        .shared
        .policy
        .try_lock()
        .map(|published| published.gate)
        .expect("the section is not held while waiters are told");
    drop(held);
    for kind in [ChangeKind::Policy, ChangeKind::Fda] {
        assert!(
            state.wait_for_change(seen, &[kind], aterm_spec::wait::MUST_HAPPEN),
            "{kind:?} is told"
        );
    }
    let fda = ChangeKind::Fda as usize;
    (bumped.0[fda] != seen.0[fda], gate)
}

#[test]
fn a_new_gate_is_published_before_its_retirement_wakes_anyone() {
    let answered = || {
        let state = synthetic_state();
        let now = Instant::now();
        let (_, request) = state.read_at(ProbeGate::on(), Duration::from_secs(5), now);
        complete(&state, request.expect("admitted"), grant(), now);
        state
    };

    let state = answered();
    let publisher = state.clone();
    let (fda_first, gate) = at_the_first_bump(&state, move || {
        publisher.publish_policy(checks_off());
    });
    assert!(!fda_first, "the section is told first");
    assert!(!gate.permits(), "nothing is told before the swap");
    assert!(
        state.shared.fda.slot.lock_or_recover().entry.is_none(),
        "the gate change retired the answer"
    );

    let state = answered();
    let publisher = state.clone();
    let (fda_first, gate) = at_the_first_bump(&state, move || {
        publisher.invalidate();
        publisher.publish_policy(checks_off());
    });
    assert!(
        fda_first && gate.permits(),
        "the retirement shows under the replaced gate"
    );

    let state = answered();
    let before = state.publication();
    let mut slower = (*state.policy()).clone();
    slower.interval *= 2;
    state.publish_policy(slower);
    assert!(
        state.shared.fda.slot.lock_or_recover().entry.is_some(),
        "the same gate keeps the answer"
    );
    assert_eq!(
        state.publication().0[ChangeKind::Fda as usize],
        before.0[ChangeKind::Fda as usize]
    );
}

#[test]
fn async_probe_real_publication_conforms_to_the_bounded_model() {
    fn replay<E: Clone + PartialEq + std::fmt::Debug>(kind: SlotKind, stale: Stale, value: E) {
        let serve = stale == Stale::Serve;
        for invalidations_before in 0..=2 {
            for invalidations_during in 0..=2 - invalidations_before {
                let cell = SlotCell::new(kind, stale);
                let now = Instant::now();
                let ttl = Duration::from_secs(5);
                let model = probe_model_serving(serve);
                let mut expected = model.init_state();
                let observed_at = std::cell::Cell::new(now);
                let check = |expected: &std::collections::BTreeMap<&'static str, i64>| {
                    let slot = cell.slot.lock_or_recover();
                    let epoch = cell.epoch.load(Ordering::Acquire);
                    assert_eq!(epoch as i64, expected["epoch"]);
                    assert_eq!(slot.request_epoch as i64, expected["request_epoch"]);
                    assert_eq!(i64::from(slot.in_flight), expected["flight"]);
                    assert_eq!(i64::from(slot.entry.is_some()), expected["cached"]);
                    assert_eq!(
                        i64::from(slot.retry_after.is_none_or(|at| at <= observed_at.get())),
                        expected["ready"]
                    );
                    assert_eq!(
                        slot.entry.as_ref().map_or(0, |entry| entry.epoch) as i64,
                        expected["cached_epoch"]
                    );
                    assert!(model.check_invariant("CurrentPublication", expected));
                    assert!(model.check_invariant("OneWorkerOrCache", expected));
                };
                for _ in 0..invalidations_before {
                    assert!(!cell.retire(), "nothing is held to forget");
                    assert!(model.fire("Invalidate", &mut expected));
                    check(&expected);
                }
                let (pending, ticket) = cell.read(now, ttl, true, SlotLock::Try);
                assert_eq!(pending, None);
                let ticket = ticket.expect("a cold read admits");
                assert!(model.fire("Start", &mut expected));
                check(&expected);
                for _ in 0..invalidations_during {
                    let _ = cell.retire();
                    assert!(model.fire("Invalidate", &mut expected));
                    check(&expected);
                }
                for _ in 0..100 {
                    let (pending, next) = cell.read(now, ttl, true, SlotLock::Try);
                    assert_eq!(pending, None);
                    assert!(next.is_none());
                    assert!(!model.action_enabled("Start", &expected));
                    check(&expected);
                }
                let mut old_unchecked_publisher = expected.clone();
                old_unchecked_publisher.insert("flight", 0);
                old_unchecked_publisher.insert("cached", 1);
                old_unchecked_publisher.insert("cached_epoch", ticket.epoch as i64);
                if invalidations_during != 0 {
                    assert!(!model.check_invariant("CurrentPublication", &old_unchecked_publisher));
                }
                assert!(cell.complete(ticket, Some(value.clone()), now));
                assert!(model.fire("Complete", &mut expected));
                check(&expected);
                if invalidations_during != 0 {
                    let (pending, blocked) = cell.read(now, ttl, true, SlotLock::Try);
                    assert_eq!(pending, None);
                    assert!(
                        blocked.is_none(),
                        "invalidation retains the admission floor"
                    );
                    assert!(!model.action_enabled("Start", &expected));
                    observed_at.set(now + PROBE_RETRY_FLOOR);
                    assert!(model.fire("Cooldown", &mut expected));
                    let (pending, current) = cell.read(observed_at.get(), ttl, true, SlotLock::Try);
                    assert_eq!(pending, None);
                    assert!(model.fire("Start", &mut expected));
                    check(&expected);
                    assert!(cell.complete(
                        current.expect("admitted at the floor"),
                        Some(value.clone()),
                        observed_at.get()
                    ));
                    assert!(model.fire("Complete", &mut expected));
                    check(&expected);
                }
                assert_eq!(
                    cell.read(observed_at.get(), ttl, true, SlotLock::Try)
                        .0
                        .map(|(served, _)| served),
                    Some(value.clone())
                );
                observed_at.set(observed_at.get() + ttl);
                assert!(model.fire("Cooldown", &mut expected));
                let (answer, refresh) = cell.read(observed_at.get(), ttl, true, SlotLock::Try);
                assert!(refresh.is_some());
                if serve {
                    assert_eq!(answer.map(|(served, _)| served), Some(value.clone()));
                } else {
                    assert_eq!(answer, None);
                    assert!(model.fire("Expire", &mut expected));
                }
                assert!(model.fire("Start", &mut expected));
                check(&expected);
            }
        }
    }
    replay(SlotKind::Fda, Stale::Drop, grant());
    replay(SlotKind::Debugger, Stale::Serve, debugger_entry());
}

#[test]
fn disabled_gates_override_warm_grants_and_late_workers() {
    for gate in [
        ProbeGate {
            enabled: false,
            check: true,
        },
        ProbeGate {
            enabled: true,
            check: false,
        },
        ProbeGate {
            enabled: false,
            check: false,
        },
    ] {
        for finish_before_disable in [false, true] {
            let state = synthetic_state();
            let now = Instant::now();
            let ttl = Duration::from_secs(5);
            let (_, request) = state.read_at(ProbeGate::on(), ttl, now);
            let request = request.unwrap();
            if finish_before_disable {
                complete(&state, request, grant(), now);
            }
            let (disabled, next) = state.read_at(gate, ttl, now);
            assert_eq!(disabled.0.label, ProbeLabel::RefusedDisabled);
            assert_eq!(disabled.0.state, FdaState::Unknown);
            assert!(next.is_none());
            if !finish_before_disable {
                assert!(state.shared.fda.slot.lock().unwrap().in_flight);
                complete(&state, request, grant(), now);
            }
            let (reenabled, request) = state.read_at(ProbeGate::on(), ttl, now + PROBE_RETRY_FLOOR);
            assert_eq!(reenabled.0.label, ProbeLabel::Pending);
            assert!(request.is_some(), "reenabling requires a new probe");
        }
    }
}

#[test]
fn contended_retire_refuses_old_grants() {
    fn contend<E: Clone>(cell: &SlotCell<E>, value: E) {
        let now = Instant::now();
        let ttl = Duration::from_secs(5);
        let (_, ticket) = cell.read(now, ttl, true, SlotLock::Try);
        assert!(cell.complete(ticket.expect("a cold read admits"), Some(value), now));
        assert!(cell.read(now, ttl, false, SlotLock::Try).0.is_some());
        let guard = cell.slot.lock_or_recover();
        let (answer, ticket) = cell.read(now, ttl, true, SlotLock::Try);
        assert!(
            answer.is_none() && ticket.is_none(),
            "{:?}: the read does not wait",
            cell.kind
        );
        assert!(
            !cell.retire(),
            "{:?}: the retirement does not wait",
            cell.kind
        );
        drop(guard);
        assert!(cell.slot.lock_or_recover().entry.is_some());
        for admit in [false, true] {
            assert!(
                cell.read(now, ttl, admit, SlotLock::Try).0.is_none(),
                "{:?}: the retired answer is served",
                cell.kind
            );
        }
    }
    contend(&SlotCell::new(SlotKind::Fda, Stale::Drop), grant());
    contend(
        &SlotCell::new(SlotKind::Debugger, Stale::Serve),
        debugger_entry(),
    );
}

#[test]
fn a_read_overtaken_by_a_retirement_reads_again() {
    fn overtake<E: Clone + Send + Sync + 'static>(kind: SlotKind, stale: Stale, value: E) {
        let now = Instant::now();
        let ttl = Duration::from_secs(5);
        let expired = now + ttl.max(PROBE_RETRY_FLOOR);
        let retired_while_held = || {
            let cell = Arc::new(SlotCell::new(kind, stale));
            cell.seed_for_test(value.clone(), now);
            let retirer = Arc::clone(&cell);
            *cell.held_read_hook.lock_or_recover() = Some(Box::new(move || {
                assert!(!retirer.retire(), "the read holds the lock");
            }));
            cell
        };
        let ran = |cell: &SlotCell<E>| {
            assert!(
                cell.held_read_hook.lock_or_recover().is_none(),
                "the retirement ran"
            );
        };

        let cell = retired_while_held();
        let ((answer, _), epoch) = cell.read_once(now, ttl, false, SlotLock::Wait);
        assert!(
            answer.is_some() && cell.epoch.load(Ordering::Acquire) != epoch,
            "{kind:?}: one pass serves the answer the retirement retired"
        );
        let cell = retired_while_held();
        let ((answer, ticket), _) = cell.read_once(expired, ttl, true, SlotLock::Wait);
        assert_eq!(
            answer.is_some(),
            stale == Stale::Serve,
            "{kind:?}: one pass serves the expired answer where stale answers are served"
        );
        assert!(cell.complete(
            ticket.expect("an expired answer admits"),
            Some(value.clone()),
            expired
        ));
        assert!(
            cell.read(expired, ttl, false, SlotLock::Wait).0.is_none(),
            "{kind:?}: one pass's claim keeps nothing"
        );

        for admit in [false, true] {
            let cell = retired_while_held();
            assert!(
                cell.read(now, ttl, admit, SlotLock::Wait).0.is_none(),
                "{kind:?}: the read served a retired answer (admit={admit})"
            );
            ran(&cell);
        }
        let cell = retired_while_held();
        let (answer, ticket) = cell.read(expired, ttl, true, SlotLock::Wait);
        ran(&cell);
        assert!(
            answer.is_none(),
            "{kind:?}: the read that claimed the refresh served a retired answer"
        );
        assert!(cell.complete(
            ticket.expect("the claim is kept"),
            Some(value.clone()),
            expired
        ));
        assert!(
            cell.read(expired, ttl, false, SlotLock::Wait).0.is_some(),
            "{kind:?}: the claimed worker's answer is kept"
        );
    }
    overtake(SlotKind::Fda, Stale::Drop, grant());
    overtake(SlotKind::Debugger, Stale::Serve, debugger_entry());
}

#[test]
fn an_off_loop_read_waits_out_a_held_slot_lock() {
    let state = synthetic_state();
    let ttl = Duration::from_secs(5);
    let (_, ticket) = state.read_at(ProbeGate::on(), ttl, Instant::now());
    complete(
        &state,
        ticket.expect("a cold read admits"),
        grant(),
        Instant::now(),
    );
    let guard = state.shared.fda.slot.lock().unwrap();
    assert_eq!(
        state.fda(ProbeGate::on(), ttl).0.label,
        ProbeLabel::Pending,
        "the event loop never waits on the lock"
    );
    let (started_tx, started_rx) = mpsc::channel();
    let (read_tx, read_rx) = mpsc::channel();
    let reader = state.clone();
    std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        read_tx
            .send(reader.fda_off_loop(ProbeGate::on(), ttl))
            .unwrap();
    });
    started_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    assert!(read_rx.recv_timeout(Duration::from_millis(20)).is_err());
    drop(guard);
    let (probe, _age) = read_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert_eq!(probe.state, FdaState::Granted);
}

#[test]
fn pending_deadlines_stay_future_and_cold_inert_reads_stay_inert() {
    let state = synthetic_state();
    let now = Instant::now();
    let (_, request) = state.read_at(ProbeGate::on(), Duration::from_millis(1), now);
    assert!(request.is_some());
    for step in 0..100 {
        let at = now + Duration::from_millis(step);
        assert_eq!(
            state.next_refresh_deadline(at, Duration::from_millis(1)),
            Some(at + PROBE_RETRY_FLOOR)
        );
        assert!(
            state
                .read_at(ProbeGate::on(), Duration::from_millis(1), at)
                .1
                .is_none()
        );
    }
    let inert = ConsentState::inert();
    assert_eq!(
        inert.fda(ProbeGate::on(), Duration::ZERO).0.label,
        ProbeLabel::RefusedDisabled
    );
    assert_eq!(inert.next_refresh_deadline(now, Duration::ZERO), None);
}

#[test]
fn zero_interval_cannot_turn_completion_into_a_probe_wake_loop() {
    let state = synthetic_state();
    let now = Instant::now();
    let (_, request) = state.read_at(ProbeGate::on(), Duration::ZERO, now);
    complete(&state, request.unwrap(), grant(), now);
    for elapsed in [Duration::ZERO, PROBE_RETRY_FLOOR - Duration::from_nanos(1)] {
        let (answer, next) = state.read_at(ProbeGate::on(), Duration::ZERO, now + elapsed);
        assert_eq!(answer.0, grant());
        assert!(next.is_none(), "completion must not immediately re-arm");
        assert_eq!(
            state.next_refresh_deadline(now + elapsed, Duration::ZERO),
            Some(now + PROBE_RETRY_FLOOR)
        );
    }
    let (expired, refresh) =
        state.read_at(ProbeGate::on(), Duration::ZERO, now + PROBE_RETRY_FLOOR);
    assert_eq!(expired.0.label, ProbeLabel::Pending);
    assert!(refresh.is_some(), "minimum freshness still expires");
}

#[test]
fn fda_cached_never_admits_and_serves_only_a_fresh_entry() {
    let state = synthetic_state();
    let now = Instant::now();
    let ttl = Duration::from_secs(5);
    let cached = |at: Instant| state.cached_at(ProbeGate::on(), ttl, at);
    let untouched = |epoch: u64, held: bool| {
        let slot = state.shared.fda.slot.lock().unwrap();
        assert!(!slot.in_flight, "a passive read claimed the worker");
        assert_eq!(
            slot.entry.is_some(),
            held,
            "a passive read dropped the entry"
        );
        assert_eq!(state.shared.fda.epoch.load(Ordering::Acquire), epoch);
    };

    let cold = state.shared.fda.epoch.load(Ordering::Acquire);
    assert_eq!(cached(now), (FdaProbe::pending(), Duration::ZERO));
    untouched(cold, false);
    assert!(state.shared.fda.slot.lock().unwrap().retry_after.is_none());

    let (_, ticket) = state.read_at(ProbeGate::on(), ttl, now);
    complete(
        &state,
        ticket.expect("a cold dispatching read admits"),
        grant(),
        now,
    );
    let epoch = state.shared.fda.epoch.load(Ordering::Acquire);
    let age = ttl - Duration::from_nanos(1);
    assert_eq!(cached(now + age), (grant(), age));
    let closed = ProbeGate {
        enabled: false,
        check: true,
    };
    assert_eq!(
        state.cached_at(closed, ttl, now).0.label,
        ProbeLabel::RefusedDisabled
    );
    assert_eq!(cached(now + ttl).0.label, ProbeLabel::Pending);
    untouched(epoch, true);
    assert_eq!(cached(now).0, grant(), "the entry is still served");

    let (answer, refresh) = state.read_at(ProbeGate::on(), ttl, now + ttl);
    assert_eq!(answer.0.label, ProbeLabel::Pending);
    assert!(
        refresh.is_some(),
        "the dispatching read admits at the interval"
    );

    let state = synthetic_state();
    let (_, ticket) = state.read_at(ProbeGate::on(), ttl, now);
    complete(&state, ticket.expect("admitted"), grant(), now);
    let guard = state.shared.fda.slot.lock().unwrap();
    state.invalidate();
    drop(guard);
    assert!(state.shared.fda.slot.lock().unwrap().entry.is_some());
    assert_eq!(
        state.cached_at(ProbeGate::on(), ttl, now).0.label,
        ProbeLabel::Pending
    );
}

#[test]
fn failed_worker_creation_releases_its_slot_with_bounded_backoff() {
    fn refused<E: Clone>(cell: &SlotCell<E>, ttl: Duration) {
        let now = Instant::now();
        let (_, ticket) = cell.read(now, ttl, true, SlotLock::Try);
        cell.spawn_failed(ticket.expect("a cold read admits"));
        let backoff = ttl.max(PROBE_RETRY_FLOOR);
        for offset in [Duration::ZERO, backoff - Duration::from_nanos(1)] {
            let (answer, next) = cell.read(now + offset, ttl, true, SlotLock::Try);
            assert!(answer.is_none() && next.is_none(), "{:?}", cell.kind);
            assert!(
                !cell.slot.lock_or_recover().in_flight,
                "{:?}: no worker owns the slot",
                cell.kind
            );
        }
        assert!(
            cell.read(now + backoff, ttl, true, SlotLock::Try)
                .1
                .is_some(),
            "{:?}: the read after the backoff admits",
            cell.kind
        );
    }
    let state = synthetic_state();
    refused(&state.shared.fda, Duration::ZERO);
    refused(&state.shared.debugger, DEBUGGER_TTL);
}

#[test]
fn a_census_completion_never_ticks_the_fda_card() {
    let state = synthetic_state();
    let (tx, rx) = mpsc::channel();
    state.set_slot_wake(Arc::new(move |kind| {
        let _ = tx.send(kind);
    }));
    let next = || {
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the worker publishes")
    };
    assert!(
        state.claimants(Some("com.example.subject"), true).1,
        "a census worker is out"
    );
    let census = next();
    assert_eq!(census, SlotKind::Census);
    assert!(matches!(census.wake(), Wake::ConsentCensusReady));

    let _ = state.fda(ProbeGate::on(), Duration::from_secs(5));
    let fda = next();
    assert_eq!(fda, SlotKind::Fda);
    assert!(matches!(fda.wake(), Wake::ConsentProbeReady));
}

struct BlockedProbe {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
static BLOCKED_PROBE: Mutex<Option<BlockedProbe>> = Mutex::new(None);
static BLOCKED_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn blocked_probe(_gate: ProbeGate) -> FdaProbe {
    BLOCKED_CALLS.fetch_add(1, Ordering::SeqCst);
    let fixture = BLOCKED_PROBE
        .lock()
        .unwrap()
        .take()
        .expect("only one worker");
    fixture.entered.send(()).unwrap();
    fixture
        .release
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    grant()
}

#[test]
fn a_blocked_real_worker_cannot_block_reads_or_multiply_on_invalidation() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    *BLOCKED_PROBE.lock().unwrap() = Some(BlockedProbe {
        entered: entered_tx,
        release: release_rx,
    });
    let mut state = synthetic_state();
    state.probes.fda = blocked_probe;
    let weak = Arc::downgrade(&state.shared);
    state.set_slot_wake(Arc::new(move |kind| {
        assert_eq!(kind, SlotKind::Fda);
        assert!(
            weak.upgrade().unwrap().fda.slot.try_lock().is_ok(),
            "wake runs unlocked"
        );
        done_tx.send(()).unwrap();
    }));
    let ttl = Duration::from_secs(5);
    assert_eq!(state.fda(ProbeGate::on(), ttl).0.label, ProbeLabel::Pending);
    entered_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    for _ in 0..100 {
        assert_eq!(state.fda(ProbeGate::on(), ttl).0.label, ProbeLabel::Pending);
        state.invalidate();
    }
    assert!(state.shared.fda.slot.lock().unwrap().in_flight);
    assert_eq!(BLOCKED_CALLS.load(Ordering::SeqCst), 1);
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(!state.shared.fda.slot.lock().unwrap().in_flight);
    assert!(
        state.shared.fda.slot.lock().unwrap().entry.is_none(),
        "stale grant refused"
    );
}
