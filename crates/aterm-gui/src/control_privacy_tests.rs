// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_containment::consent::{self, DebuggerProbeLabel, ProbeGate, ProbeOutcome};
use std::sync::atomic::Ordering;
use std::sync::mpsc;

use super::*;

fn synthetic_state() -> ConsentState {
    let mut state = ConsentState::inert();
    state.probes_for_test().live = true;
    state
}

fn grant() -> FdaProbe {
    consent::classify_probe(consent::ProbeOutcome::Ok)
}

#[test]
fn a_failed_privacy_hop_is_the_reply() {
    assert_eq!(
        privacy(
            PrivacyForm::Lines,
            || Err(MainHopError::Reason("event loop gone")),
            || panic!("no hop, nothing read")
        ),
        "ERR event loop gone\n"
    );
    let app = crate::App::headless_for_test();
    let reply = privacy(
        PrivacyForm::Lines,
        || Ok(app.privacy_facts(Scope::Owner)),
        || {
            Some(crate::vitals_jobs::HeldCount {
                held: 2,
                oldest_ms: 35_940_000,
                stranded: Some(1),
            })
        },
    );
    let (head, body) = reply.split_once('\n').expect("a framed reply");
    assert_eq!(head, format!("OK {}", body.lines().count()), "{reply}");
    assert!(
        body.lines()
            .any(|l| l.starts_with("full_disk_access=unknown probe=refused_disabled ")),
        "{reply}"
    );
    assert!(
        body.lines().any(|l| l.starts_with("devtools ")
            && l.ends_with(
                " program_check=2 program_check_oldest_ms=35940000 program_check_stranded=1"
            )),
        "the worker's count: {reply}"
    );
    let json = privacy(
        PrivacyForm::Json,
        || Ok(app.privacy_facts(Scope::Owner)),
        || None,
    );
    assert!(json.starts_with("OK 1\n{\"schema\":1,"), "{json}");
    assert_eq!(json.lines().count(), 2, "{json}");
}

#[test]
fn the_verb_takes_only_the_json_flag() {
    assert_eq!(parse_privacy_form(false, ""), Ok(PrivacyForm::Lines));
    assert_eq!(parse_privacy_form(false, "   "), Ok(PrivacyForm::Lines));
    assert_eq!(parse_privacy_form(true, ""), Ok(PrivacyForm::Json));
    for json in [false, true] {
        let err = parse_privacy_form(json, "folders").expect_err("a guessed modifier is refused");
        assert!(err.starts_with("ERR usage: privacy [--json]"), "{err}");
        assert!(err.contains("folders"), "the usage echoes the input: {err}");
    }
}

#[test]
fn await_consent_has_a_finite_default_and_a_shared_ceiling() {
    let args = |kind, timeout_ms| Ok(ConsentArgs { kind, timeout_ms });
    for (rest, want) in [
        ("consent", args(ConsentKind::Fda, 300_000)),
        ("consent timeout 1500", args(ConsentKind::Fda, 1_500)),
        ("consent timeout=99999999", args(ConsentKind::Fda, 600_000)),
        (
            "consent kind=debugger",
            args(ConsentKind::Debugger, 300_000),
        ),
        (
            "consent timeout 2500 kind=debugger",
            args(ConsentKind::Debugger, 2_500),
        ),
        (
            "consent kind=debugger timeout=99999999",
            args(ConsentKind::Debugger, 600_000),
        ),
    ] {
        assert_eq!(parse_consent_args(rest), want, "{rest:?}");
    }
    for bad in [
        "consent kind=fda",
        "consent kind debugger",
        "consent idle 10",
    ] {
        assert!(
            parse_consent_args(bad).is_err_and(|e| e.starts_with("ERR usage: await consent")),
            "{bad:?}"
        );
    }
}

#[test]
fn a_debugger_reading_with_no_mode_is_pending_to_the_waiter() {
    for label in [
        DebuggerProbeLabel::Pending,
        DebuggerProbeLabel::RefusedDisabled,
        DebuggerProbeLabel::TimedOut,
        DebuggerProbeLabel::StatusUnrecognized,
    ] {
        assert_eq!(
            debugger_observation(DebuggerProbe::refused(label)),
            ConsentObservation::Pending,
            "{label:?} observed no mode"
        );
    }
}

#[test]
fn the_await_baseline_is_the_whole_consent_tuple() {
    let tuple = |fs_consent, fda, attribution| ConsentTuple::Fda {
        fs_consent,
        fda,
        attribution,
    };
    let baseline = tuple(FsConsent::Unknown, FdaState::Denied, Attribution::Live);
    assert_eq!(
        baseline.to_string(),
        "fs_consent=unknown fda=denied attribution=live",
        "the tuple is (fs_consent, fda, attribution)"
    );
    for (moved, what) in [
        (
            tuple(FsConsent::Unknown, FdaState::Granted, Attribution::Live),
            "the instance grant",
        ),
        (
            tuple(FsConsent::Unknown, FdaState::Denied, Attribution::Adopted),
            "this session's attribution",
        ),
        (
            tuple(FsConsent::Denied, FdaState::Denied, Attribution::Live),
            "this session's fs_consent",
        ),
    ] {
        assert_ne!(moved, baseline, "{what}");
        assert_ne!(moved.to_string(), baseline.to_string(), "{what}");
    }
    let policy = ConsentPolicy::from_config(&crate::app_config::Config::default());
    let denied = consent::classify_probe(ProbeOutcome::Errno(consent::ERRNO_EPERM));
    assert_eq!(
        fda_observation(&policy, Attribution::Live, denied),
        ConsentObservation::Complete(baseline)
    );
    assert_eq!(
        fda_observation(&policy, Attribution::Live, FdaProbe::pending()),
        ConsentObservation::Pending
    );
}

fn wait_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        AsyncConsentWaitObservation {
            const Buggy = 0;
            var baseline = 0;
            var result = 0;
            var expired = 0;
            var complete = 0;
            var armed = 0;
            action Pending when (result == 0 && expired == 0) {
                complete = 0;
                armed = if baseline == 0 { 0 } else { 1 };
                result = if Buggy == 1 && baseline > 0 { 1 } else { 0 };
            }
            action ObserveA when (result == 0 && expired == 0) {
                complete = 1;
                armed = if baseline == 0 { 0 } else { 1 };
                result = if baseline == 2 { 1 } else { 0 };
                baseline = if baseline == 0 { 1 } else { baseline };
            }
            action ObserveB when (result == 0 && expired == 0) {
                complete = 1;
                armed = if baseline == 0 { 0 } else { 1 };
                result = if baseline == 1 { 1 } else { 0 };
                baseline = if baseline == 0 { 2 } else { baseline };
            }
            action Exit when (result == 0 && expired == 0) { result = 2; }
            action Expire when (result == 0 && expired == 0) { expired = 1; }
            action Timeout when (result == 0 && expired == 1) { result = 3; }
            invariant OnlyCompletedChanges: result == 0 || result > 1 || (complete == 1 && armed == 1);
            invariant DeadlineWins: expired == 0 || result == 0 || result == 3;
            invariant Bounds: baseline <= 2 && result <= 3 && expired <= 1 && complete <= 1 && armed <= 1;
        }
    }
}

#[test]
fn wait_model_proves_and_catches_pending_refresh_as_a_false_change() {
    let model = wait_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn real_await_guards_conform_for_cold_warm_pending_and_deadline_observations() {
    let model = wait_model();
    let a = ConsentTuple::Fda {
        fs_consent: FsConsent::Unknown,
        fda: FdaState::Denied,
        attribution: Attribution::Live,
    };
    let b = ConsentTuple::Fda {
        fs_consent: FsConsent::Unknown,
        fda: FdaState::Granted,
        attribution: Attribution::Live,
    };
    for sequence in 0..256 {
        for expire_at in 0..=4 {
            let now = Instant::now();
            let deadline = now + Duration::from_millis(10);
            let mut wait = ConsentWait::new(now, Duration::from_millis(10));
            let mut expected = model.init_state();
            for index in 0..4 {
                let choice = (sequence >> (index * 2)) & 3;
                let (action, observation) = match choice {
                    0 => ("Pending", ConsentObservation::Pending),
                    1 => ("ObserveA", ConsentObservation::Complete(a)),
                    2 => ("ObserveB", ConsentObservation::Complete(b)),
                    _ => ("Exit", ConsentObservation::Exited),
                };
                let at = if index >= expire_at { deadline } else { now };
                if index >= expire_at {
                    assert!(model.fire("Expire", &mut expected));
                    assert!(model.fire("Timeout", &mut expected));
                } else {
                    assert!(model.fire(action, &mut expected));
                }
                let result = wait.observe_at(observation, at);
                let projected = match result {
                    ConsentWaitDecision::Wait => 0,
                    ConsentWaitDecision::Changed(_) => 1,
                    ConsentWaitDecision::Exited => 2,
                    ConsentWaitDecision::TimedOut => 3,
                };
                assert_eq!(projected, expected["result"]);
                assert_eq!(
                    match wait.baseline {
                        None => 0,
                        Some(tuple) if tuple == a => 1,
                        Some(tuple) if tuple == b => 2,
                        other => panic!("unexpected baseline: {other:?}"),
                    },
                    expected["baseline"]
                );
                assert_eq!(wait.deadline, deadline, "pending cannot extend the request");
                assert!(model.check_invariant("OnlyCompletedChanges", &expected));
                assert!(model.check_invariant("DeadlineWins", &expected));
                if action == "Pending" && expected["baseline"] != 0 && projected == 0 {
                    let mut old_string_comparison = expected.clone();
                    old_string_comparison.insert("result", 1);
                    assert!(!model.check_invariant("OnlyCompletedChanges", &old_string_comparison));
                }
                if projected != 0 {
                    break;
                }
            }
        }
    }
}

#[test]
fn expiring_real_cache_never_completes_await_until_the_observed_tuple_changes() {
    let state = synthetic_state();
    let now = Instant::now();
    let ttl = Duration::from_secs(5);
    let policy = state.policy();
    let sample = |probe| fda_observation(&policy, Attribution::Live, probe);
    let mut wait = ConsentWait::new(now, ttl * 4);
    let ((pending, _), request) = state.read_at(ProbeGate::on(), ttl, now);
    assert_eq!(
        wait.observe_at(sample(pending), now),
        ConsentWaitDecision::Wait
    );
    assert!(wait.baseline.is_none(), "cold pending is not the baseline");
    let denied = consent::classify_probe(consent::ProbeOutcome::Errno(consent::ERRNO_EPERM));
    state.complete_fda_for_test(request.unwrap(), denied, now);
    let ((probe, _), _) = state.read_at(ProbeGate::on(), ttl, now);
    assert_eq!(
        wait.observe_at(sample(probe), now),
        ConsentWaitDecision::Wait
    );
    let baseline = wait.baseline;
    for (at, refreshed) in [(now + ttl, denied), (now + ttl * 2, grant())] {
        let ((pending, _), request) = state.read_at(ProbeGate::on(), ttl, at);
        assert_eq!(pending.label, ProbeLabel::Pending);
        assert_eq!(
            wait.observe_at(sample(pending), at),
            ConsentWaitDecision::Wait
        );
        assert_eq!(wait.baseline, baseline, "refresh retains the armed tuple");
        state.complete_fda_for_test(request.unwrap(), refreshed, at);
        let ((probe, _), _) = state.read_at(ProbeGate::on(), ttl, at);
        let result = wait.observe_at(sample(probe), at);
        if refreshed == denied {
            assert_eq!(result, ConsentWaitDecision::Wait);
        } else {
            assert_eq!(
                result,
                ConsentWaitDecision::Changed(ConsentTuple::Fda {
                    fs_consent: FsConsent::Unknown,
                    fda: FdaState::Granted,
                    attribution: Attribution::Live,
                })
            );
        }
    }
}

struct FakeWait {
    clock: std::cell::Cell<Instant>,
    reads: std::cell::Cell<usize>,
    parks: std::cell::RefCell<Vec<Duration>>,
}

impl FakeWait {
    fn new(at: Instant) -> Self {
        Self {
            clock: std::cell::Cell::new(at),
            reads: std::cell::Cell::new(0),
            parks: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn run(
        &self,
        exited: &dyn Fn(usize) -> bool,
        hung_up: bool,
        grant: &dyn Fn(Duration) -> Duration,
        wait: impl FnOnce(&WaitEnv<'_>) -> String,
    ) -> String {
        wait(&WaitEnv {
            now: &|| self.clock.get(),
            exited: &|| {
                self.reads.set(self.reads.get() + 1);
                exited(self.reads.get())
            },
            hung_up: &|| hung_up,
            park: &|requested| {
                self.parks.borrow_mut().push(requested);
                self.clock.set(self.clock.get() + grant(requested));
                Duration::ZERO
            },
        })
    }
}

#[test]
fn await_consent_takes_one_hop_however_long_it_waits() {
    let state = synthetic_state();
    let interval = state.policy().interval;
    let t0 = Instant::now();
    let (_, request) = state.read_at(ProbeGate::on(), interval, t0);
    let held = request.expect("a probe out that never answers");
    let fake = FakeWait::new(t0);
    let hops = std::cell::Cell::new(0_usize);
    let args = ConsentArgs {
        kind: ConsentKind::Fda,
        timeout_ms: 300_000,
    };
    let reply = fake.run(&|_| false, false, &|park| park, |env| {
        await_consent(
            args,
            true,
            || {
                hops.set(hops.get() + 1);
                Ok(ConsentArm {
                    consent: state.clone(),
                    adoption: Some(Attribution::Live),
                })
            },
            env,
        )
    });
    assert_eq!(reply, "OK timeout\n");
    assert_eq!(hops.get(), 1, "one hop");
    let parks = fake.parks.borrow();
    assert_eq!(fake.reads.get(), parks.len(), "one park after every read");
    assert!(
        parks
            .iter()
            .all(|park| !park.is_zero() && *park <= CONSENT_EXIT_POLL),
        "{parks:?}"
    );
    let timeout = Duration::from_millis(args.timeout_ms).as_millis();
    let intervals = timeout / interval.as_millis();
    assert!(
        parks.len() as u128 <= timeout / CONSENT_EXIT_POLL.as_millis() + intervals + 2,
        "{} parks",
        parks.len()
    );
    assert_eq!(
        state.fda_in_flight_for_test(),
        Some(held),
        "no second worker"
    );

    let fake = FakeWait::new(t0);
    let reply = fake.run(
        &|_| false,
        false,
        &|park| park.max(Duration::from_secs(1)),
        |env| {
            run_consent_wait(
                ConsentWait::new(t0, Duration::from_secs(3)),
                &[ChangeKind::Fda],
                &state,
                |now| (ConsentObservation::Pending, Some(now)),
                env,
            )
        },
    );
    assert_eq!(reply, "OK timeout\n");
    assert!(
        fake.parks.borrow().contains(&Duration::ZERO),
        "a deadline that is not after the read asks for an empty park"
    );
}

#[test]
fn a_consent_wait_ends_with_its_session_its_caller_or_its_hop() {
    let state = ConsentState::inert();
    let t0 = Instant::now();
    let args = ConsentArgs {
        kind: ConsentKind::Fda,
        timeout_ms: 300_000,
    };
    let hops = std::cell::Cell::new(0_usize);
    let arm = |adoption| {
        hops.set(hops.get() + 1);
        Ok(ConsentArm {
            consent: state.clone(),
            adoption,
        })
    };
    let wait = |exited: &dyn Fn(usize) -> bool,
                hung_up: bool,
                authorized: bool,
                arm: &dyn Fn() -> Result<ConsentArm, MainHopError>| {
        FakeWait::new(t0).run(exited, hung_up, &|park| park, |env| {
            await_consent(args, authorized, arm, env)
        })
    };
    let live = || arm(Some(Attribution::Live));
    assert_eq!(wait(&|_| false, false, false, &live), "ERR exited\n");
    assert_eq!(hops.get(), 0, "an unauthorized caller costs no hop");
    assert_eq!(
        wait(&|_| false, false, true, &|| Err(MainHopError::Reason(
            "event loop gone"
        ))),
        "ERR event loop gone\n"
    );
    assert_eq!(wait(&|_| false, false, true, &|| arm(None)), "ERR exited\n");
    assert_eq!(
        wait(&|reads| reads >= 3, false, true, &live),
        "ERR exited\n"
    );
    assert_eq!(
        wait(&|_| false, true, true, &live),
        crate::control::HUNG_UP_REPLY
    );
    assert_eq!(wait(&|_| false, false, true, &live), "OK timeout\n");
}

#[test]
fn await_consent_latches_on_a_focus_retire_and_regrant() {
    use std::sync::atomic::AtomicBool;
    static GRANTED: AtomicBool = AtomicBool::new(false);
    fn flipping(_gate: ProbeGate) -> FdaProbe {
        consent::classify_probe(if GRANTED.load(Ordering::Acquire) {
            consent::ProbeOutcome::Ok
        } else {
            consent::ProbeOutcome::Errno(consent::ERRNO_EPERM)
        })
    }
    let mut state = synthetic_state();
    state.probes_for_test().fda = flipping;
    let (armed_tx, armed_rx) = mpsc::channel();
    let (reply_tx, reply_rx) = mpsc::channel();
    let waiter = state.clone();
    std::thread::spawn(move || {
        let observe = |now| {
            let read = observe_fda(&waiter, Attribution::Live, now);
            if matches!(read.0, ConsentObservation::Complete(_)) {
                let _ = armed_tx.send(());
            }
            read
        };
        let reply = run_consent_wait(
            ConsentWait::new(Instant::now(), aterm_spec::wait::MUST_HAPPEN),
            &[ChangeKind::Fda, ChangeKind::Policy],
            &waiter,
            observe,
            &WaitEnv {
                now: &Instant::now,
                exited: &|| false,
                hung_up: &|| false,
                park: &|park| park,
            },
        );
        let _ = reply_tx.send(reply);
    });
    armed_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the denial is read");
    GRANTED.store(true, Ordering::Release);
    state.invalidate();
    let reply = reply_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the waiter answers");
    assert!(
        reply.starts_with("OK consent fs_consent=unknown fda=granted attribution=live elapsed_ms="),
        "{reply}"
    );
}
