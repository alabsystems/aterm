// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_containment::consent::{DebuggerMember, DebuggerMode};

use super::*;
use crate::control::Scope;
use crate::control_privacy::{
    ConsentObservation, ConsentTuple, ConsentWait, ConsentWaitDecision, observe_debugger,
    observe_fda,
};
use crate::privacy_report::PrivacyForm;
fn wake_on(kind: SlotKind, tx: std::sync::mpsc::Sender<()>) -> SlotWake {
    Arc::new(move |published| {
        if published == kind {
            let _ = tx.send(());
        }
    })
}

fn census_answer(found: Vec<aterm_containment::consent::Claimant>) -> Claimants {
    Claimants {
        found,
        enumeration: aterm_containment::consent::Enumeration::Complete,
    }
}

#[test]
fn the_census_runs_on_a_worker_and_survives_focus_and_config_invalidation() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    fn counting_census(_id: &str) -> Claimants {
        RUNS.fetch_add(1, Ordering::AcqRel);
        census_answer(Vec::new())
    }
    RUNS.store(0, Ordering::Release);
    let mut state = ConsentState::inert();
    state.probes.live = true;
    state.probes.census = counting_census;
    let (tx, rx) = std::sync::mpsc::channel();
    state.set_slot_wake(wake_on(SlotKind::Census, tx));
    let wait = || {
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the worker publishes")
    };

    let (answer, pending) = state.claimants(Some("x"), true);
    assert!(
        answer.is_none() && pending,
        "the reading thread never waits"
    );
    wait();
    let (answer, pending) = state.claimants(Some("x"), true);
    assert!(answer.is_some() && !pending);
    assert_eq!(RUNS.load(Ordering::Acquire), 1);

    state.invalidate();
    let (answer, _) = state.claimants(Some("x"), true);
    assert!(
        answer.is_some(),
        "focus and config changes do not change the disk"
    );
    assert_eq!(RUNS.load(Ordering::Acquire), 1, "served, not rerun");

    state.invalidate_census();
    let (answer, pending) = state.claimants(Some("x"), true);
    assert!(answer.is_none() && pending);
    wait();
    assert!(state.claimants(Some("x"), true).0.is_some());
    assert_eq!(RUNS.load(Ordering::Acquire), 2);
}

#[test]
fn a_census_out_when_the_disk_changes_publishes_nothing() {
    use std::sync::atomic::AtomicBool;
    static RELEASE: AtomicBool = AtomicBool::new(false);
    fn held(_id: &str) -> Claimants {
        while !RELEASE.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(2));
        }
        census_answer(Vec::new())
    }
    let mut state = ConsentState::inert();
    state.probes.live = true;
    state.probes.census = held;
    let (tx, rx) = std::sync::mpsc::channel();
    state.set_slot_wake(wake_on(SlotKind::Census, tx));
    let wait = || {
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the worker publishes")
    };
    assert_eq!(state.claimants(Some("x"), true), (None, true));
    state.invalidate_census();
    RELEASE.store(true, Ordering::Release);
    wait();
    let (answer, pending) = state.claimants(Some("x"), true);
    assert!(answer.is_none() && pending, "the stale answer is dropped");
    wait();
    assert!(state.claimants(Some("x"), true).0.is_some());
}

#[test]
fn the_census_is_off_without_an_id_a_live_instance_or_the_master_switch() {
    static RAN: AtomicU64 = AtomicU64::new(0);
    fn never(_id: &str) -> Claimants {
        RAN.fetch_add(1, Ordering::AcqRel);
        Claimants::default()
    }
    let mut state = ConsentState::inert();
    state.probes.live = true;
    state.probes.census = never;
    assert_eq!(state.claimants(None, true), (None, false));
    assert_eq!(state.claimants(Some("x"), false), (None, false));
    let mut inert = ConsentState::inert();
    inert.probes.census = never;
    assert_eq!(inert.claimants(Some("x"), true), (None, false));
    assert_eq!(RAN.load(Ordering::Acquire), 0);
}

#[test]
fn consent_at_risk_is_a_conjunction_and_is_silent_without_a_cwd() {
    let roots = vec![PathBuf::from("/u/me/Protected")];
    let inside = Some("/u/me/Protected/work");
    assert!(consent_at_risk(FsConsent::Unknown, inside, &roots));
    assert!(consent_at_risk(FsConsent::Denied, inside, &roots));
    assert!(!consent_at_risk(FsConsent::Covered, inside, &roots));
    assert!(!consent_at_risk(
        FsConsent::Unknown,
        Some("/u/me/src"),
        &roots
    ));
    for verdict in [FsConsent::Unknown, FsConsent::Denied, FsConsent::Covered] {
        assert!(!consent_at_risk(verdict, None, &roots));
    }
}

fn fake_debugger(runs: &std::sync::atomic::AtomicUsize) -> DebuggerProbe {
    runs.fetch_add(1, Ordering::AcqRel);
    DebuggerProbe {
        mode: DebuggerMode::Disabled,
        member: DebuggerMember::Admin,
        label: DebuggerProbeLabel::StatusOk,
    }
}

#[test]
fn the_debugger_probe_runs_on_a_worker_and_focus_gain_retires_it() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    static HARDENING_READS: AtomicUsize = AtomicUsize::new(0);
    fn counting(_gate: ProbeGate) -> DebuggerProbe {
        fake_debugger(&RUNS)
    }
    fn hardened() -> SelfHardening {
        HARDENING_READS.fetch_add(1, Ordering::AcqRel);
        SelfHardening::NotHardened
    }
    let mut state = ConsentState::inert();
    state.probes.live = true;
    state.probes.debugger = counting;
    state.probes.hardening = hardened;
    let (tx, rx) = std::sync::mpsc::channel();
    state.set_slot_wake(wake_on(SlotKind::Debugger, tx));
    let wait = || {
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the worker publishes")
    };

    let cold = state.debugger(ProbeGate::on());
    assert_eq!(cold, DebuggerRow::awaiting(SelfHardening::Unknown));
    assert!(cold.pending(), "the reading thread never waits");
    wait();
    let warm = state.debugger(ProbeGate::on());
    assert_eq!(warm.probe.mode, DebuggerMode::Disabled);
    assert_eq!(warm.probe.member, DebuggerMember::Admin);
    assert_eq!(warm.self_hardened, SelfHardening::NotHardened);
    assert!(warm.probe_age_ms.is_some(), "a completed probe has an age");
    assert_eq!(RUNS.load(Ordering::Acquire), 1);

    state.invalidate();
    assert!(!state.debugger(ProbeGate::on()).pending());
    assert_eq!(RUNS.load(Ordering::Acquire), 1, "served, not rerun");

    state.invalidate_debugger();
    state.shared.debugger.clear_floor();
    assert!(state.debugger(ProbeGate::on()).pending());
    wait();
    assert!(!state.debugger(ProbeGate::on()).pending());
    assert_eq!(RUNS.load(Ordering::Acquire), 2);
    assert_eq!(
        HARDENING_READS.load(Ordering::Acquire),
        1,
        "the hardening bits are fixed at exec: read once"
    );
}

#[test]
fn a_pending_debugger_row_keeps_the_hardening_already_read() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    fn counting(_gate: ProbeGate) -> DebuggerProbe {
        fake_debugger(&RUNS)
    }
    fn hardened() -> SelfHardening {
        SelfHardening::Hardened
    }
    let mut state = ConsentState::inert();
    state.probes.live = true;
    state.probes.debugger = counting;
    state.probes.hardening = hardened;
    let (tx, rx) = std::sync::mpsc::channel();
    state.set_slot_wake(wake_on(SlotKind::Debugger, tx));
    let wait = || {
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the worker publishes")
    };

    let cold = state.debugger(ProbeGate::on());
    assert!(cold.pending());
    assert_eq!(
        cold.self_hardened(),
        SelfHardening::Unknown,
        "nothing read it"
    );
    wait();
    assert_eq!(
        state.debugger(ProbeGate::on()).self_hardened(),
        SelfHardening::Hardened
    );

    state.invalidate_debugger();
    state.shared.debugger.clear_floor();
    let retired = state.debugger(ProbeGate::on());
    assert_eq!(retired, DebuggerRow::awaiting(SelfHardening::Hardened));
    assert!(retired.pending(), "the rule itself is read again");
    wait();
    assert_eq!(RUNS.load(Ordering::Acquire), 2);

    let closed = ProbeGate {
        enabled: false,
        check: true,
    };
    assert_eq!(
        state.debugger(closed).self_hardened(),
        SelfHardening::Unknown,
        "a closed gate reports nothing, the hardening included"
    );
}

#[test]
fn the_master_switch_makes_the_debugger_row_unknown() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    static HARDENING_READS: AtomicUsize = AtomicUsize::new(0);
    fn counting(_gate: ProbeGate) -> DebuggerProbe {
        fake_debugger(&RUNS)
    }
    fn hardened() -> SelfHardening {
        HARDENING_READS.fetch_add(1, Ordering::AcqRel);
        SelfHardening::Hardened
    }
    let mut app = crate::App::headless_for_test();
    app.consent.probes.live = true;
    app.consent.probes.debugger = counting;
    app.consent.probes.hardening = hardened;
    app.set_privacy_for_test(crate::app_config::PrivacyConfig {
        enabled: Some(false),
        ..Default::default()
    });
    let lines = app
        .privacy_report_for_test(PrivacyForm::Lines, Scope::Owner)
        .0;
    let debugger = lines
        .iter()
        .find(|l| l.starts_with("debugger "))
        .expect("the posture carries a debugger row");
    assert!(
        debugger.starts_with("debugger mode=unknown ")
            && debugger.contains(" probe=refused_disabled "),
        "{debugger}"
    );
    assert_eq!(RUNS.load(Ordering::Acquire), 0, "nothing was spawned");
    assert_eq!(HARDENING_READS.load(Ordering::Acquire), 0);

    let state = &app.consent;
    state.shared.debugger.seed_for_test(
        DebuggerEntry {
            probe: fake_debugger(&AtomicUsize::new(0)),
            self_hardened: SelfHardening::Hardened,
        },
        Instant::now(),
    );
    let closed = ProbeGate {
        enabled: false,
        check: true,
    };
    assert_eq!(
        state.debugger(closed),
        DebuggerRow::refused(DebuggerProbeLabel::RefusedDisabled)
    );
    assert!(
        state.shared.debugger.slot.lock_or_recover().entry.is_none(),
        "the closed gate retired the warm answer"
    );
    assert_eq!(RUNS.load(Ordering::Acquire), 0);
}

#[test]
fn the_polled_consent_paths_never_dispatch_the_debugger_probe() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    static HARDENING_READS: AtomicUsize = AtomicUsize::new(0);
    fn counting(_gate: ProbeGate) -> DebuggerProbe {
        fake_debugger(&RUNS)
    }
    fn hardened() -> SelfHardening {
        HARDENING_READS.fetch_add(1, Ordering::AcqRel);
        SelfHardening::Hardened
    }
    let mut app = crate::App::headless_for_test();
    app.consent.probes.live = true;
    app.consent.probes.debugger = counting;
    app.consent.probes.hardening = hardened;
    let (tx, rx) = std::sync::mpsc::channel();
    app.consent.set_slot_wake(wake_on(SlotKind::Debugger, tx));
    assert!(
        app.consent_policy().gate.permits(),
        "the gate is open, so only the path can keep the probe quiet"
    );
    let slot_idle = |app: &crate::App| {
        let slot = app.consent.shared.debugger.slot.lock_or_recover();
        !slot.in_flight && slot.entry.is_none()
    };
    for _ in 0..3 {
        let _ = observe_fda(&app.consent, Attribution::Live, Instant::now());
        let _ = app.session_consent(0, None);
    }
    assert_eq!(RUNS.load(Ordering::Acquire), 0, "a poll spawned the probe");
    assert_eq!(HARDENING_READS.load(Ordering::Acquire), 0);
    assert!(slot_idle(&app), "a poll left a debugger worker in flight");

    let _ = app.privacy_report_for_test(PrivacyForm::Lines, Scope::Owner);
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the full report's worker publishes");
    assert_eq!(RUNS.load(Ordering::Acquire), 1);
    assert!(!slot_idle(&app), "the full report's answer is cached");
}

#[test]
fn status_reads_the_fda_cache_and_never_dispatches_the_probe() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    fn counting(_gate: ProbeGate) -> FdaProbe {
        RUNS.fetch_add(1, Ordering::AcqRel);
        consent::classify_probe(consent::ProbeOutcome::Ok)
    }
    let mut app = crate::App::headless_for_test();
    app.consent.probes.live = true;
    app.consent.probes.fda = counting;
    let (tx, rx) = std::sync::mpsc::channel();
    app.consent.set_slot_wake(wake_on(SlotKind::Fda, tx));
    app.set_privacy_for_test(crate::app_config::PrivacyConfig {
        probe_interval_ms: Some(3_600_000),
        ..Default::default()
    });
    let policy = app.consent_policy();
    assert!(
        policy.gate.permits(),
        "the gate is open, so only the path can keep the probe quiet"
    );
    assert!(app.pool.get(0).is_some(), "the read reaches the cache");
    let slot_idle = |app: &crate::App| {
        let slot = app.consent.shared.fda.slot.lock_or_recover();
        !slot.in_flight && slot.entry.is_none() && slot.retry_after.is_none()
    };

    for _ in 0..3 {
        let _ = app.session_consent(0, None);
    }
    assert_eq!(
        RUNS.load(Ordering::Acquire),
        0,
        "a status read spawned the probe"
    );
    assert!(slot_idle(&app), "a status read claimed the worker");

    let _ = app.consent.fda(policy.gate, policy.interval);
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the dispatching read's worker publishes");
    assert_eq!(RUNS.load(Ordering::Acquire), 1);
    for _ in 0..3 {
        let _ = app.session_consent(0, None);
    }
    assert_eq!(RUNS.load(Ordering::Acquire), 1, "a status read refreshed");
    assert_eq!(
        app.consent.fda_cached(policy.gate, policy.interval).0,
        consent::classify_probe(consent::ProbeOutcome::Ok),
        "the passive read serves the published verdict"
    );
}

fn fake_devtools(runs: &std::sync::atomic::AtomicUsize, at_ms: i64) -> DevToolProbe {
    runs.fetch_add(1, Ordering::AcqRel);
    DevToolProbe {
        state: aterm_containment::DevToolState::Unlisted,
        label: DevToolProbeLabel::Observed,
        requests: Some(3),
        not_allowed: Some(3),
        answered_at_ms: Some(at_ms),
    }
}

fn fake_subject() -> Option<&'static str> {
    Some("com.example.subject")
}

#[test]
fn the_devtools_probe_runs_on_a_worker_and_focus_gain_retires_it() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    static SUBJECT_SEEN: Mutex<Option<String>> = Mutex::new(None);
    fn counting(_gate: ProbeGate, subject: &str) -> DevToolProbe {
        *SUBJECT_SEEN.lock().unwrap_or_else(|p| p.into_inner()) = Some(subject.to_owned());
        fake_devtools(&RUNS, unix_now_ms() - 120_000)
    }
    let mut state = ConsentState::inert();
    state.probes.live = true;
    state.probes.devtools = counting;
    state.probes.subject = fake_subject;
    let (tx, rx) = std::sync::mpsc::channel();
    state.set_slot_wake(wake_on(SlotKind::Devtools, tx));
    let wait = || {
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the worker publishes")
    };

    let cold = state.devtools(ProbeGate::on(), false);
    assert!(cold.pending(), "the reading thread never waits");
    assert_eq!(cold.probe.state, aterm_containment::DevToolState::Unknown);
    wait();
    let warm = state.devtools(ProbeGate::on(), false);
    assert_eq!(warm.probe.state, aterm_containment::DevToolState::Unlisted);
    assert_eq!(warm.probe.requests, Some(3));
    assert!(warm.probe_age_ms.is_some(), "a completed probe has an age");
    let answer_age = warm.answer_age_ms.expect("tccd's answer has an age");
    assert!(
        answer_age >= 120_000,
        "aged against the wall clock: {answer_age}"
    );
    assert_eq!(RUNS.load(Ordering::Acquire), 1);
    assert_eq!(
        SUBJECT_SEEN
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_deref(),
        Some("com.example.subject"),
        "the worker asks about the subject arm's bundle id"
    );

    state.invalidate();
    state.invalidate_debugger();
    assert!(!state.devtools(ProbeGate::on(), false).pending());
    assert_eq!(RUNS.load(Ordering::Acquire), 1, "served, not rerun");

    // A read made just before: its retry floor still holds.
    state.shared.devtools.slot.lock_or_recover().retry_after =
        Some(Instant::now() + Duration::from_secs(60));
    state.invalidate_devtools();
    assert!(state.devtools(ProbeGate::on(), false).pending());
    assert!(
        state.devtools_slot_idle_for_test(),
        "the control: retired inside the floor, no read is sent and nothing will answer"
    );
    state.refresh_devtools();
    assert!(state.devtools(ProbeGate::on(), false).pending());
    wait();
    assert!(!state.devtools(ProbeGate::on(), false).pending());
    assert_eq!(RUNS.load(Ordering::Acquire), 2, "a refresh reads at once");

    {
        let mut slot = state.shared.devtools.slot.lock_or_recover();
        let entry = slot.entry.as_mut().expect("a cached answer");
        entry.at = Instant::now()
            .checked_sub(DEVTOOLS_TTL + Duration::from_secs(1))
            .expect("the clock is past one TTL");
        slot.retry_after = None;
    }
    for _ in 0..5 {
        assert!(!state.devtools(ProbeGate::on(), false).pending());
    }
    wait();
    assert_eq!(RUNS.load(Ordering::Acquire), 3, "one refresh per TTL");
}

#[test]
fn the_master_switch_and_observer_off_make_the_devtools_row_unknown() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    fn counting(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        fake_devtools(&RUNS, 0)
    }
    fn no_subject() -> Option<&'static str> {
        None
    }
    fn unusable_subject() -> Option<&'static str> {
        Some("a b")
    }
    let mut app = crate::App::headless_for_test();
    app.consent.probes.live = true;
    app.consent.probes.devtools = counting;
    app.consent.probes.subject = fake_subject;
    let row = |app: &crate::App| {
        app.privacy_report_for_test(PrivacyForm::Lines, Scope::Owner)
            .0
            .into_iter()
            .find(|l| l.starts_with("devtools "))
            .expect("the posture carries a devtools row")
    };
    let refused = |app: &crate::App, label: &str| {
        let row = row(app);
        assert!(
            row.starts_with("devtools state=unknown ")
                && row.contains(&format!(" probe={label} "))
                && row.contains(&format!(" list_probe={label} ")),
            "{row}"
        );
    };
    let warm = |app: &crate::App| {
        app.consent.shared.devtools.seed_for_test(
            DevToolsEntry {
                probe: fake_devtools(&AtomicUsize::new(0), 0),
                list: DevToolList::pending(),
                subject: fake_subject(),
            },
            Instant::now(),
        );
    };
    let cold = |app: &crate::App| {
        app.consent
            .shared
            .devtools
            .slot
            .lock_or_recover()
            .entry
            .is_none()
    };

    for privacy in [
        crate::app_config::PrivacyConfig {
            enabled: Some(false),
            ..Default::default()
        },
        crate::app_config::PrivacyConfig {
            check: Some(false),
            ..Default::default()
        },
    ] {
        app.set_privacy_for_test(privacy);
        warm(&app);
        refused(&app, "refused_disabled");
        assert!(cold(&app), "the closed gate retired the warm answer");
    }
    app.set_privacy_for_test(crate::app_config::PrivacyConfig {
        observer: Some(false),
        ..Default::default()
    });
    warm(&app);
    refused(&app, "refused_observer_off");
    assert!(cold(&app), "observer = false retired the warm answer");
    app.set_privacy_for_test(crate::app_config::PrivacyConfig::default());
    let (tx, rx) = std::sync::mpsc::channel();
    app.consent.set_slot_wake(wake_on(SlotKind::Devtools, tx));
    app.consent.probes.subject = unusable_subject;
    assert!(
        row(&app).contains(" probe=refused_no_subject "),
        "{}",
        row(&app)
    );
    assert!(
        app.consent.devtools_slot_idle_for_test(),
        "no worker was dispatched for it"
    );
    app.consent.probes.subject = no_subject;
    app.consent.probes.warm_subject = no_subject;
    assert!(row(&app).contains(" probe=pending "), "{}", row(&app));
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the worker publishes");
    assert!(
        row(&app).contains(" subject=- ") && row(&app).contains(" probe=refused_no_subject "),
        "{}",
        row(&app)
    );
    assert_eq!(RUNS.load(Ordering::Acquire), 0, "nothing was spawned");

    app.consent.probes.subject = fake_subject;
    app.consent.invalidate_devtools();
    app.consent.shared.devtools.clear_floor();
    assert!(row(&app).contains(" probe=pending "), "{}", row(&app));
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the worker publishes");
    assert_eq!(RUNS.load(Ordering::Acquire), 1);
    assert!(row(&app).contains(" probe=observed "), "{}", row(&app));
}

#[test]
fn a_cold_identity_is_read_on_the_devtools_worker() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    static WARMS: AtomicUsize = AtomicUsize::new(0);
    static SUBJECT_SEEN: Mutex<Option<String>> = Mutex::new(None);
    fn counting(_gate: ProbeGate, subject: &str) -> DevToolProbe {
        *SUBJECT_SEEN.lock().unwrap_or_else(|p| p.into_inner()) = Some(subject.to_owned());
        fake_devtools(&RUNS, 0)
    }
    fn cold_identity() -> Option<&'static str> {
        None
    }
    fn warming() -> Option<&'static str> {
        WARMS.fetch_add(1, Ordering::AcqRel);
        fake_subject()
    }
    let mut state = ConsentState::inert();
    state.set_devtools_arms_for_test(counting, cold_identity);
    state.probes.warm_subject = warming;
    let (tx, rx) = std::sync::mpsc::channel();
    state.set_slot_wake(wake_on(SlotKind::Devtools, tx));
    let wait = || {
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the worker publishes")
    };

    let first = state.devtools(ProbeGate::on(), false);
    assert!(first.pending(), "a cold identity is not refused: {first:?}");
    assert_eq!(first.subject, None);
    wait();
    let warm = state.devtools(ProbeGate::on(), false);
    assert_eq!(warm.probe.label, DevToolProbeLabel::Observed);
    assert_eq!(
        warm.subject,
        Some("com.example.subject"),
        "the row names what the worker asked about"
    );
    assert_eq!(
        SUBJECT_SEEN
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_deref(),
        Some("com.example.subject")
    );
    assert_eq!(RUNS.load(Ordering::Acquire), 1);
    assert_eq!(WARMS.load(Ordering::Acquire), 1);
}

#[test]
fn the_polled_consent_paths_never_dispatch_the_devtools_probe() {
    use std::sync::atomic::AtomicUsize;
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    fn counting(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        fake_devtools(&RUNS, 0)
    }
    fn quiet_debugger(_gate: ProbeGate) -> DebuggerProbe {
        DebuggerProbe::refused(DebuggerProbeLabel::StatusUnrecognized)
    }
    let mut app = crate::App::headless_for_test();
    app.consent.probes.live = true;
    app.consent.probes.devtools = counting;
    app.consent.probes.debugger = quiet_debugger;
    app.consent.probes.subject = fake_subject;
    let (tx, rx) = std::sync::mpsc::channel();
    app.consent.set_slot_wake(wake_on(SlotKind::Devtools, tx));
    assert!(app.consent_policy().gate.permits());
    let slot_idle = |app: &crate::App| {
        let slot = app.consent.shared.devtools.slot.lock_or_recover();
        !slot.in_flight && slot.entry.is_none()
    };
    for _ in 0..3 {
        let _ = observe_fda(&app.consent, Attribution::Live, Instant::now());
        let _ = observe_debugger(&app.consent, Instant::now());
        let _ = app.session_consent(0, None);
        let _ = app.consent.debugger_cached(ProbeGate::on());
        let _ = app.consent.devtools_cached(ProbeGate::on(), false);
    }
    assert_eq!(RUNS.load(Ordering::Acquire), 0, "a poll spawned the probe");
    assert!(slot_idle(&app), "a poll left a devtools worker in flight");

    let _ = app.privacy_report_for_test(PrivacyForm::Lines, Scope::Owner);
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the full report's worker publishes");
    assert_eq!(RUNS.load(Ordering::Acquire), 1);
    assert!(!slot_idle(&app), "the full report's answer is cached");
}

#[test]
fn await_consent_kind_debugger_latches_on_a_mode_change_without_a_probe_per_poll() {
    use std::sync::atomic::{AtomicU8, AtomicUsize};
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    static MODE: AtomicU8 = AtomicU8::new(0);
    fn fake_status(_gate: ProbeGate) -> DebuggerProbe {
        RUNS.fetch_add(1, Ordering::AcqRel);
        DebuggerProbe {
            mode: if MODE.load(Ordering::Acquire) == 0 {
                DebuggerMode::Disabled
            } else {
                DebuggerMode::Enabled
            },
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        }
    }
    fn hardened() -> SelfHardening {
        SelfHardening::Hardened
    }
    let mut app = crate::App::headless_for_test();
    app.consent.probes.live = true;
    app.consent.probes.debugger = fake_status;
    app.consent.probes.hardening = hardened;
    let (tx, rx) = std::sync::mpsc::channel();
    app.consent.set_slot_wake(wake_on(SlotKind::Debugger, tx));
    let published = || {
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the worker publishes")
    };
    let clear_floor = |app: &crate::App| {
        app.consent.shared.debugger.clear_floor();
    };
    let observe = |app: &crate::App| observe_debugger(&app.consent, Instant::now()).0;
    let mode = |mode| ConsentObservation::Complete(ConsentTuple::Debugger { mode });
    let fda_seq = |app: &crate::App| app.consent.publication().0[ChangeKind::Fda as usize];
    let fda_before = fda_seq(&app);
    let now = Instant::now();
    let mut wait = ConsentWait::new(now, Duration::from_secs(300));

    assert_eq!(observe(&app), ConsentObservation::Pending);
    assert_eq!(
        wait.observe_at(ConsentObservation::Pending, now),
        ConsentWaitDecision::Wait
    );
    published();
    let baseline = observe(&app);
    assert_eq!(baseline, mode(DebuggerMode::Disabled));
    assert_eq!(wait.observe_at(baseline, now), ConsentWaitDecision::Wait);
    MODE.store(1, Ordering::Release);
    for _ in 0..20 {
        clear_floor(&app);
        let (same, due) = observe_debugger(&app.consent, Instant::now());
        let expires = app
            .consent
            .shared
            .debugger
            .slot
            .lock_or_recover()
            .entry
            .as_ref()
            .map(|entry| entry.at + DEBUGGER_TTL);
        assert_eq!(due, expires, "the refresh is due when the answer expires");
        assert_eq!(wait.observe_at(same, now), ConsentWaitDecision::Wait);
    }
    assert_eq!(RUNS.load(Ordering::Acquire), 1, "a read spawned the probe");

    app.consent.invalidate_debugger();
    clear_floor(&app);
    let retired = observe(&app);
    assert_eq!(retired, ConsentObservation::Pending);
    assert_eq!(wait.observe_at(retired, now), ConsentWaitDecision::Wait);
    published();
    let fresh = observe(&app);
    assert_eq!(
        wait.observe_at(fresh, now),
        ConsentWaitDecision::Changed(ConsentTuple::Debugger {
            mode: DebuggerMode::Enabled
        })
    );
    assert_eq!(
        ConsentTuple::Debugger {
            mode: DebuggerMode::Enabled
        }
        .to_string(),
        "debugger_mode=enabled"
    );
    assert_eq!(RUNS.load(Ordering::Acquire), 2);

    {
        let mut slot = app.consent.shared.debugger.slot.lock_or_recover();
        let entry = slot.entry.as_mut().expect("a cached answer");
        entry.at = Instant::now()
            .checked_sub(DEBUGGER_TTL + Duration::from_secs(1))
            .expect("the clock is past one TTL");
        slot.retry_after = None;
    }
    for _ in 0..5 {
        assert_eq!(
            observe(&app),
            mode(DebuggerMode::Enabled),
            "a stale answer is served while it refreshes"
        );
    }
    published();
    assert_eq!(RUNS.load(Ordering::Acquire), 3, "one refresh per TTL");
    assert_eq!(
        fda_seq(&app),
        fda_before,
        "debugger churn published no Full Disk Access change"
    );
}

#[test]
fn a_slot_refresh_is_always_due_after_now() {
    let ttl = DEBUGGER_TTL;
    let second = Duration::from_secs(1);
    let t0 = Instant::now();
    let cell = SlotCell::<u8>::new(SlotKind::Debugger, Stale::Serve);
    assert_eq!(
        cell.next_refresh_deadline(t0, ttl),
        t0 + ttl,
        "nothing held"
    );
    let ticket = cell
        .read(t0, ttl, true, SlotLock::Try)
        .1
        .expect("a cold read admits");
    assert_eq!(
        cell.next_refresh_deadline(t0, ttl),
        t0 + ttl,
        "a worker is out"
    );
    let t1 = t0 + second;
    assert!(cell.complete(ticket, Some(1), t1));
    assert_eq!(
        cell.next_refresh_deadline(t1 + second, ttl),
        t1 + ttl,
        "the answer's expiry"
    );

    let stale = t1 + ttl * 2;
    let (served, refresh) = cell.read(stale, ttl, true, SlotLock::Try);
    assert!(
        served.is_some() && refresh.is_some(),
        "stale: served, refreshed"
    );
    assert!(t1 + ttl <= stale, "the naive rule's expiry is in the past");
    assert_eq!(
        cell.next_refresh_deadline(stale, ttl),
        stale + ttl,
        "never the past expiry"
    );

    cell.spawn_failed(refresh.expect("admitted"));
    assert_eq!(
        cell.read(stale, ttl, true, SlotLock::Try).1,
        None,
        "a refused worker backs off"
    );
    assert_eq!(
        cell.next_refresh_deadline(stale + second, ttl),
        stale + ttl,
        "by one TTL"
    );
    assert!(cell.retire(), "a held answer is forgotten");
    assert!(!cell.retire(), "a second retirement forgets nothing");
    assert_eq!(
        cell.next_refresh_deadline(stale + second, ttl),
        stale + ttl,
        "the floor survives the retirement"
    );
    assert_eq!(
        cell.next_refresh_deadline(stale + ttl, ttl),
        stale + ttl * 2,
        "past the floor, one TTL on"
    );
}

#[test]
fn await_consent_kind_debugger_never_latches_without_a_reading() {
    let app = crate::App::headless_for_test();
    assert!(
        !app.consent.probes.live,
        "a headless app has the inert arms"
    );
    for _ in 0..3 {
        assert_eq!(
            observe_debugger(&app.consent, Instant::now()),
            (ConsentObservation::Pending, None)
        );
    }
    let mut closed = crate::App::headless_for_test();
    closed.consent.probes.live = true;
    closed.set_privacy_for_test(crate::app_config::PrivacyConfig {
        enabled: Some(false),
        ..Default::default()
    });
    assert_eq!(
        observe_debugger(&closed.consent, Instant::now()),
        (ConsentObservation::Pending, None)
    );
    let slot = closed.consent.shared.debugger.slot.lock_or_recover();
    assert!(
        slot.entry.is_none() && !slot.in_flight,
        "a closed gate dispatches nothing"
    );
}

#[test]
fn the_master_switch_makes_every_consent_field_unknown() {
    let mut app = crate::App::headless_for_test();
    assert_eq!(app.session_consent(0, None).attribution, Attribution::Live);
    assert_eq!(
        app.session_consent(4242, None).attribution,
        Attribution::Unknown
    );
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .handoff_local_id = Some(3);
    assert_eq!(
        app.session_consent(0, None).attribution,
        Attribution::Adopted,
        "with the lane ON the adoption record shows"
    );

    let panel = app.consent_panel_facts();
    assert!(panel.enabled);
    assert_eq!((panel.sessions_total, panel.sessions_adopted), (1, 1));

    app.set_privacy_for_test(crate::app_config::PrivacyConfig {
        enabled: Some(false),
        ..Default::default()
    });
    let off = app.session_consent(0, None);
    let panel_off = app.consent_panel_facts();
    assert!(!panel_off.enabled);
    assert_eq!(panel_off.probe, ProbeLabel::RefusedDisabled);
    assert_eq!(panel_off.fda, FdaState::Unknown);
    assert_eq!(
        (panel_off.sessions_total, panel_off.sessions_adopted),
        (1, 0)
    );
    assert_eq!(
        (panel_off.install, panel_off.running),
        (panel.install, panel.running)
    );
    assert_eq!(
        off.attribution,
        Attribution::Unknown,
        "the switch outranks the record"
    );
    assert_eq!(off.fs_consent, FsConsent::Unknown);
    assert!(!off.at_risk);

    let lines = app
        .privacy_report_for_test(PrivacyForm::Lines, Scope::Owner)
        .0;
    assert!(
        lines
            .iter()
            .any(|l| l.contains("full_disk_access=unknown") && l.contains("probe=")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("attribution_root=unknown")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("=denied")),
        "a switched-off lane never reads as a denial: {lines:?}"
    );
}

/// A config reload's `[privacy] protected_roots` reach the wake files' admission too
/// (`reconcile_consent_config`), which guards them beside its fixed set. NEGATIVE CONTROL:
/// before the reload the root is not among them.
#[test]
fn a_config_reload_sets_the_wake_files_guarded_roots() {
    use crate::app_colorscheme::install_resolved_colors::apply_config_text;
    let root = PathBuf::from(if cfg!(windows) {
        "C:/opt/aterm-wake-privacy"
    } else {
        "/opt/aterm-wake-privacy"
    });
    let privacy = format!(
        "[privacy]\ncheck = false\nprotected_roots = [\"{}\"]\n",
        root.display()
    );
    let dir = aterm_tempfile::Builder::new()
        .prefix("wake-privacy")
        .tempdir()
        .expect("a scratch folder");
    let mut app = crate::App::headless_for_test();
    assert!(
        crate::occupancy_wakes::privacy().is_none_or(|roots| !roots.contains(&root)),
        "NEGATIVE CONTROL: not before the reload"
    );
    apply_config_text(&mut app, dir.path(), &privacy);
    assert!(
        crate::occupancy_wakes::privacy().is_some_and(|roots| roots.contains(&root)),
        "{:?}",
        crate::occupancy_wakes::privacy()
    );
}

#[test]
fn a_config_reload_republishes_the_privacy_mirror() {
    use crate::app_colorscheme::install_resolved_colors::apply_config_text;
    let root = format!(
        "{}/aterm-privacy-mirror",
        if cfg!(windows) { "C:/tmp" } else { "/tmp" }
    );
    let privacy = format!("[privacy]\ncheck = false\nprotected_roots = [\"{root}\"]\n");
    let config: crate::app_config::Config = aterm_toml::from_str(&privacy).expect("valid toml");
    let expected = ConsentPolicy::from_config(&config);
    assert_eq!(expected.protected_roots(), [PathBuf::from(&root)]);
    let cwd = format!("{root}/repo");
    let dir = std::env::temp_dir().join(format!("aterm-privacy-mirror-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let mut app = crate::App::headless_for_test();
    assert_ne!(*app.consent.policy(), expected, "precondition");
    assert!(!app.session_consent(0, Some(&cwd)).at_risk, "precondition");

    apply_config_text(&mut app, &dir, &privacy);
    assert_eq!(*app.consent.policy(), expected);
    assert!(
        app.session_consent(0, Some(&cwd)).at_risk,
        "status judges the cwd against the republished roots"
    );

    let published = app.consent.policy();
    apply_config_text(
        &mut app,
        &dir,
        &format!("{privacy}[claude_footer]\nusage = false\n"),
    );
    assert_eq!(
        app.config.claude_footer.as_ref().and_then(|f| f.usage),
        Some(false),
        "the unrelated generation was admitted"
    );
    assert!(
        Arc::ptr_eq(&published, &app.consent.policy()),
        "an equal section keeps the published mirror"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Accessibility read keeps the in-bundle fence of the other consent probes: macOS answers
/// `AXIsProcessTrusted` for the app responsible for this process, which outside a `.app` is the
/// terminal that started it, so no exe and an exe outside a bundle refuse without calling the
/// read, and only an exe inside a `.app` reads, reporting what the read answered.
#[test]
fn the_accessibility_read_refuses_outside_an_app_bundle_without_reading() {
    use std::cell::Cell;
    use std::path::Path;
    let reads = Cell::new(0_usize);
    let read = |answer: bool| {
        let reads = &reads;
        move || {
            reads.set(reads.get() + 1);
            answer
        }
    };
    for (exe, expected) in [
        (None, AxProbe::RefusedNoExe),
        (
            Some(Path::new("/usr/local/bin/aterm")),
            AxProbe::RefusedOutOfBundle,
        ),
        (
            Some(Path::new("/Users//me/aterm/target/debug/aterm")),
            AxProbe::RefusedOutOfBundle,
        ),
        (
            Some(Path::new(
                "/Applications/aterm.app/Contents/Resources/aterm",
            )),
            AxProbe::RefusedOutOfBundle,
        ),
    ] {
        assert_eq!(accessibility_with(exe, read(true)), expected, "{exe:?}");
        assert_eq!(reads.get(), 0, "{exe:?}: the read ran outside a bundle");
        assert_eq!(expected.grant(), "unknown", "{expected:?}");
    }
    assert_eq!(AxProbe::RefusedNoExe.label(), "refused_no_exe");
    assert_eq!(AxProbe::RefusedOutOfBundle.label(), "refused_out_of_bundle");

    let bundled = Some(Path::new("/Applications/aterm.app/Contents/MacOS/aterm"));
    for trusted in [true, false] {
        let before = reads.get();
        assert_eq!(
            accessibility_with(bundled, read(trusted)),
            AxProbe::Read { trusted },
            "inside a bundle the row is the read's answer"
        );
        assert_eq!(reads.get(), before + 1, "one read inside a bundle");
    }
}
