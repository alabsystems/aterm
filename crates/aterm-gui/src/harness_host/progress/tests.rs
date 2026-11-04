// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SID: &str = "s-progress";
const CONVERSATION: &str = "conversation-a";
const BORN: u64 = 100;

fn snapshot() -> Snapshot {
    Snapshot {
        tab: SID.into(),
        pid: 4242,
        start: "process-a".into(),
        shell: 4241,
        program: PathBuf::from("/synthetic/claude"),
        argv: vec!["/synthetic/claude".into()],
        session: Some(CONVERSATION.into()),
        cwd: "/synthetic".into(),
        version: None,
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
        stored_login: false,
        profile: None,
    }
}

fn hooks() -> Hooks {
    Hooks {
        roster: Arc::new(Vec::new),
        autopilot: Arc::new(HashMap::new),
        still_wanted: Arc::new(|_, _| panic!("reader must not inspect worker liveness")),
        body: Arc::new(|_| panic!("reader must not run a worker")),
        badge: Arc::new(|_, _| panic!("reader must not publish a badge")),
        waiting: Arc::new(|_, _| panic!("reader must not schedule work")),
        fact: Arc::new(|_, _| panic!("reader must not emit actions")),
        needs: Arc::new(|_, _, _| panic!("reader must not ask for credentials")),
        hold: Arc::new(|_, _| panic!("reader must not change holds")),
        said: Arc::new(|_, _| panic!("reader must not store a hold's words")),
        receipt: Arc::new(|_, _| panic!("reader must not store a receipt")),
        claim_epoch: Arc::new(|| 0),
        acts: Acts::inert(),
        backoff: Arc::new(|_| Duration::ZERO),
        pause: Arc::new(|_| Duration::ZERO),
        carry_clock: Arc::new(MonotonicClock),
        upgrade_view: Arc::new(|_, _, _, _| ViewNext::default()),
        upgrade_sources: Arc::new(crate::upgrade_host::UpgradeSources::default),
        disk: Arc::new(|_| panic!("reader must not inspect disk")),
        codex_roster: Arc::new(Vec::new),
        note: Arc::new(|_| panic!("reader must not emit notes")),
        log_usage: Arc::new(|_, _, _| panic!("reader must not invent usage")),
    }
}

fn fixture() -> (HostHandle, Arc<Mutex<Kept>>) {
    let switches = Arc::new(Switches::default());
    switches
        .account_controls
        .store(ACCOUNTS_WALL, Ordering::SeqCst);
    let snap = snapshot();
    let mut kept = Kept {
        snapshot: Some(snap.clone()),
        ..Kept::default()
    };
    let mut value = AccountProgress::unknown(10);
    value.phase = ProgressPhase::Watching;
    kept.account.progress = Some(BoundProgress {
        snapshot: snap,
        generation: kept.account.landing.clone(),
        born: Some(BORN),
        state: None,
        labels: BTreeSet::new(),
        rows: Arc::from([]),
        policy: Arc::clone(&kept.account.progress_policy),
        execution_until: None,
        value,
    });
    let kept = Arc::new(Mutex::new(kept));
    let host = host_for(Arc::clone(&kept), switches, hooks());
    (host, kept)
}

pub(in crate::harness_host) fn host_for(
    kept: Arc<Mutex<Kept>>,
    switches: Arc<Switches>,
    hooks: Hooks,
) -> HostHandle {
    let sid = kept.lock().unwrap().snapshot.as_ref().unwrap().tab.clone();
    let shared = Shared {
        state: Mutex::new(State::default()),
        headless: true,
        desk: crate::desk::Desk::of(true),
        faults: Mutex::default(),
        host_faults: Mutex::default(),
        kept: Mutex::new(HashMap::from([(sid, kept)])),
        switches,
        activations: Arc::default(),
        wake: std::sync::OnceLock::new(),
        host_done: AtomicBool::new(false),
        net: NetProbe::new(),
        login_latches: Arc::default(),
        restored: Mutex::default(),
        restored_wake: Condvar::new(),
        inbox_lane: 0,
        restart_panics: AtomicBool::new(false),
        passes_panic: AtomicBool::new(false),
    };
    HostHandle {
        shared: Arc::new(shared),
        thread: Arc::default(),
        hooks,
    }
}

fn read(host: &HostHandle) -> Option<AccountProgress> {
    host.account_progress(SID, 4242, Some(BORN), Some(CONVERSATION))
        .unwrap()
}

#[test]
fn expired_deadlines_are_not_next_events_without_login_activity() {
    let (host, kept) = fixture();
    for (deadline, expected) in [
        (Some(1), None),
        (Some(0), None),
        (Some(-1), None),
        (Some(i64::MAX), Some(i64::MAX)),
        (None, None),
    ] {
        {
            let mut kept = kept.lock().unwrap();
            let bound = kept.account.progress.as_mut().unwrap();
            assert!(
                bound.state.is_none(),
                "no login activity reader is available"
            );
            bound.value.phase = ProgressPhase::Waiting;
            bound.value.next_at = deadline;
        }
        let progress = read(&host).unwrap();
        assert_eq!(progress.next_at, expected);
        assert_eq!(progress.observed_at, 10, "a UI read is not a new decision");
        assert_eq!(progress.phase, ProgressPhase::Waiting);
        let retained = kept
            .lock()
            .unwrap()
            .account
            .progress
            .as_ref()
            .unwrap()
            .value
            .next_at;
        assert_eq!(
            retained, deadline,
            "the display must not mutate the supervisor's decision"
        );
    }
}

#[test]
fn displayed_wait_deadlines_round_up_and_refuse_overflow() {
    for (wait, expected) in [
        (Duration::ZERO, Some(100)),
        (Duration::from_nanos(1), Some(101)),
        (Duration::from_secs(1), Some(101)),
        (Duration::new(1, 1), Some(102)),
        (Duration::MAX, None),
    ] {
        assert_eq!(wait_deadline(100, wait), expected);
    }
    assert_eq!(wait_deadline(i64::MAX, Duration::from_nanos(1)), None);
}

#[test]
fn private_execution_deadlines_notify_only_when_the_gui_must_rearm() {
    let base = Instant::now();
    let now = base + Duration::from_secs(10);
    for (old, next, notify) in [
        (20, 30, false),
        (30, 20, true),
        (5, 20, true),
        (5, 8, false),
    ] {
        let (_host, kept) = fixture();
        let mut kept = kept.lock().unwrap();
        let mut bound = kept.account.progress.as_ref().unwrap().clone();
        bound.value.phase = ProgressPhase::Waiting;
        bound.value.next_at = None; // Missing/frozen wall time can leave both values identical.
        bound.execution_until = Some(base + Duration::from_secs(old));
        kept.account.execution_progress = Some(bound.clone());
        let value = bound.value.clone();
        bound.execution_until = Some(base + Duration::from_secs(next));
        assert_eq!(
            replace_execution_progress(&mut kept, Some(bound), now),
            notify
        );
        let retained = kept.account.execution_progress.as_ref().unwrap();
        assert_eq!(
            retained.execution_until,
            Some(base + Duration::from_secs(next))
        );
        assert_eq!(
            retained.value, value,
            "private currentness never restamps visible evidence"
        );
    }
}

#[test]
fn a_concurrent_silent_extension_keeps_either_the_new_or_the_earlier_expiry_wake() {
    let base = Instant::now();
    let old_until = base + Duration::from_secs(5);
    let new_until = base + Duration::from_secs(20);
    for extend_in_clock in [true, false] {
        let (host, kept) = fixture();
        {
            let mut kept = kept.lock().unwrap();
            let mut bound = kept.account.progress.as_ref().unwrap().clone();
            bound.execution_until = Some(old_until);
            bound.state = Some("/synthetic/state".into());
            bound.value.phase = ProgressPhase::Waiting;
            kept.account.execution_progress = Some(bound);
        }
        let extend = || {
            let mut kept = kept.lock().unwrap();
            let mut next = kept.account.execution_progress.as_ref().unwrap().clone();
            next.execution_until = Some(new_until);
            assert!(!replace_execution_progress(
                &mut kept,
                Some(next),
                old_until - Duration::from_nanos(1)
            ));
        };
        let read = host
            .account_progress_read_with_activity(
                SID,
                4242,
                Some(BORN),
                Some(CONVERSATION),
                || {
                    if extend_in_clock {
                        extend();
                    }
                    (
                        if extend_in_clock {
                            old_until
                        } else {
                            old_until - Duration::from_nanos(1)
                        },
                        Some(100),
                    )
                },
                |_, _, _| {
                    if !extend_in_clock {
                        extend();
                    }
                    Some(Vec::new())
                },
            )
            .unwrap();
        assert_eq!(
            read.progress.as_ref().unwrap().phase,
            ProgressPhase::Waiting
        );
        // A snapshot that still carries the old lease must retain even an already-past wake.
        assert_eq!(
            read.next_poll(old_until + Duration::from_secs(1)),
            Some(if extend_in_clock {
                new_until
            } else {
                old_until
            })
        );
    }
}

#[test]
fn an_unavailable_activity_read_without_a_gate_remains_unavailable() {
    let (host, kept) = fixture();
    kept.lock()
        .unwrap()
        .account
        .progress
        .as_mut()
        .unwrap()
        .state = Some("/synthetic/state".into());
    assert!(
        host.account_progress_with_activity(
            SID,
            4242,
            Some(BORN),
            Some(CONVERSATION),
            || (Instant::now(), Some(100)),
            |_, _, _| None
        )
        .is_err()
    );
}

#[test]
fn recent_login_activity_keeps_the_blocker_and_filters_unrelated_or_invalid_times() {
    use aterm_agent::harness::accounts_sync::{LoginActivity, LoginIdentity};
    let mut value = AccountProgress::unknown(100);
    value.phase = ProgressPhase::Waiting;
    value.details.push("existing blocker".into());
    let labels = BTreeSet::from(["current".into()]);
    let row = accounts::Row {
        label: "current".into(),
        ..accounts::Row::default()
    };
    let activity = |label: &str, at| LoginActivity {
        label: label.into(),
        identity: Some(LoginIdentity::for_test(&row)),
        route: None,
        active: false,
        observed_at: at,
        next_at: Some(150),
        diagnostic: None,
    };
    apply_login_activity(
        &mut value,
        &labels,
        std::slice::from_ref(&row),
        vec![
            activity("other", 90),
            activity("current", 0),
            activity("current", 101),
            activity("current", 90),
        ],
        100,
    );
    assert_eq!(value.phase, ProgressPhase::Waiting);
    assert_eq!(value.details[0], "existing blocker");
    assert_eq!(value.details.len(), 3);
    let timestamp = aterm_agent::harness::usage::rfc3339_utc(90);
    assert!(value.details[2].contains(&timestamp));
    assert_eq!(value.next_at, Some(150));
    let facts = crate::fact_menu::FactMenuFacts {
        account_progress: Some(value),
        ..Default::default()
    };
    let rows = crate::fact_menu::rows(crate::claude_lights::Fact::Account, &facts);
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    for cols in [60, 120] {
        let rect = crate::autopilot_menu::place(&rows, (25, 0), (0..26, 0..cols), (cols, 26), 0)
            .expect("account menu fits");
        let painted = crate::autopilot_menu::paint(rect, &rows, Default::default(), None, theme);
        assert!(
            painted.iter().any(|row| {
                row.iter()
                    .map(|cell| cell.ch)
                    .collect::<String>()
                    .contains(&timestamp)
            }),
            "the activity time must survive the menu's content-width cap"
        );
    }
}

#[test]
fn login_activity_requires_one_exact_captured_account_row() {
    use aterm_agent::harness::accounts_sync::{LoginActivity, LoginIdentity};
    let original = accounts::Row {
        label: "current".into(),
        email: Some("expected@example.test".into()),
        dir: Some("/synthetic/original-login".into()),
        ..accounts::Row::default()
    };
    let mut changed_email = original.clone();
    changed_email.email = Some("replacement@example.test".into());
    let mut changed_dir = original.clone();
    changed_dir.dir = Some("/synthetic/replacement-login".into());
    let identity = || Some(LoginIdentity::for_test(&original));
    for (rows, captured, label, admitted) in [
        (vec![original.clone()], identity(), "current", true),
        (vec![changed_email], identity(), "current", false),
        (vec![changed_dir], identity(), "current", false),
        (
            vec![original.clone(), original.clone()],
            identity(),
            "current",
            false,
        ),
        (vec![], identity(), "current", false),
        (vec![original.clone()], None, "current", false),
        (vec![original.clone()], identity(), "different", false),
    ] {
        for active in [false, true] {
            let (host, kept) = fixture();
            let retained = {
                let mut kept = kept.lock().unwrap();
                let bound = kept.account.progress.as_mut().unwrap();
                bound.state = Some("/synthetic/state".into());
                bound.labels = BTreeSet::from([original.label.clone()]);
                bound.rows = rows.clone().into();
                bound.value.phase = ProgressPhase::Waiting;
                bound.value.account = Some(original.label.clone());
                bound.value.known = Some(2);
                bound.value.eligible = Some(0);
                bound.value.next_at = Some(160);
                bound.value.details = vec!["existing blocker".into()];
                bound.value.clone()
            };
            let read = |activity| {
                host.account_progress_with_activity(
                    SID,
                    4242,
                    Some(BORN),
                    Some(CONVERSATION),
                    || (Instant::now(), Some(100)),
                    |_, _, _| Some(activity),
                )
                .unwrap()
                .unwrap()
            };
            let baseline = read(Vec::new());
            let got = read(vec![LoginActivity {
                label: label.into(),
                identity: captured.clone(),
                route: Some("mailbox".into()),
                active,
                observed_at: 90,
                next_at: Some(150),
                diagnostic: None,
            }]);
            if admitted {
                assert_eq!(
                    got.phase,
                    if active {
                        ProgressPhase::SigningIn {
                            label: "current".into(),
                            route: Some("mailbox".into()),
                        }
                    } else {
                        baseline.phase.clone()
                    }
                );
                assert_eq!(got.next_at, Some(150));
                assert_eq!(got.details.len(), baseline.details.len() + 2);
                assert_eq!(got.details[0], baseline.details[0]);
                assert_eq!(got.observed_at, baseline.observed_at);
                assert_eq!(got.known, baseline.known);
                assert_eq!(got.eligible, baseline.eligible);
            } else {
                assert_eq!(
                    got, baseline,
                    "unbound or replaced activity changed current recovery"
                );
            }
            assert_eq!(
                kept.lock()
                    .unwrap()
                    .account
                    .progress
                    .as_ref()
                    .unwrap()
                    .value,
                retained
            );
        }
    }
}

#[test]
fn completed_login_diagnostics_reach_the_menu_only_for_the_captured_account() {
    use aterm_agent::harness::accounts_sync::{LoginActivity, LoginDiagnostic, LoginIdentity};
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};
    use aterm_agent::harness::open_url::{Met, MetWord};

    let row = accounts::Row {
        label: "shared".into(),
        email: Some("expected@example.test".into()),
        dir: Some("/synthetic/login".into()),
        ..accounts::Row::default()
    };
    let diagnosis = |at| {
        LoginDiagnostic::from_exit_for_test(
            &row,
            at,
            1,
            Some(Met {
                word: MetWord::BrowserControl,
                host: Some("https://must-not-appear.invalid/token".into()),
            }),
        )
    };
    let activity = |diagnostic, active| LoginActivity {
        label: row.label.clone(),
        identity: Some(LoginIdentity::for_test(&row)),
        route: Some("mailbox".into()),
        active,
        observed_at: 70,
        next_at: Some(150),
        diagnostic,
    };
    let project = |rows: &[accounts::Row], entry| {
        let mut value = AccountProgress::unknown(100);
        value.phase = ProgressPhase::Waiting;
        value.known = Some(2);
        value.eligible = Some(0);
        value.next_at = Some(160);
        value.details.push("existing blocker".into());
        apply_login_activity(
            &mut value,
            &BTreeSet::from([row.label.clone()]),
            rows,
            vec![entry],
            100,
        );
        value
    };
    let rpc = |value: &AccountProgress, busy| {
        let mut timeline = crate::session_timeline::SessionTimeline::for_test();
        timeline.note_foreground_group(4242);
        timeline.set_program(4242, Some("node".into()));
        timeline.set_agent_reader_program_for_test(Some(Program::Claude));
        assert!(timeline.set_claude_footer(
            4242,
            Some(BORN),
            Some(FooterFacts {
                owner: Some(FactsOwner {
                    floor: Some(BORN),
                    session_id: CONVERSATION.into(),
                }),
                ..FooterFacts::default()
            }),
        ));
        assert!(timeline.set_account_progress(
            4242,
            Some(BORN),
            Some(CONVERSATION),
            Some(value.clone()),
        ));
        if busy {
            assert!(timeline.set_account_progress_busy(4242, Some(BORN), Some(CONVERSATION)));
        }
        let reply = crate::harness_recovery_verb::cmd("recovery", true, || {
            Ok(crate::harness_recovery_verb::snapshot(&timeline))
        });
        assert_eq!(
            timeline.claude_footer().unwrap().account_progress.as_ref(),
            Some(value)
        );
        reply
            .split_whitespace()
            .filter_map(|word| word.split_once('='))
            .map(|(key, value)| (key.to_owned(), aterm_control::wire::pct_decode(value)))
            .collect::<BTreeMap<_, _>>()
    };
    let rows = [row.clone()];
    let value = project(&rows, activity(Some(diagnosis(80)), false));
    let control = project(&rows, activity(None, false));
    assert_eq!(value.phase, control.phase);
    assert_eq!(value.eligible, control.eligible);
    assert_eq!(value.known, control.known);
    assert_eq!(value.next_at, control.next_at);
    assert_eq!(value.observed_at, control.observed_at);
    assert_eq!(value.details[0], "existing blocker");
    assert_eq!(value.details, control.details);
    assert_eq!(value.login_diagnostics.len(), 1);
    let text = value.login_diagnostics[0].words();
    assert!(text.contains("headless browser"), "{text}");
    assert!(!text.contains("must-not-appear"));
    assert!(!text.contains("expected@example.test"));
    assert!(!text.contains("/synthetic/login"));
    let reply = rpc(&value, false);
    let no_diagnostic = rpc(&control, false);
    assert_eq!(reply["login_diagnostic_count"], "1");
    assert_eq!(reply["login_diagnostic_0_account"], row.label);
    assert_eq!(reply["login_diagnostic_0_completed_at"], "80");
    assert_eq!(reply["login_diagnostic_0_words"], text);
    assert_eq!(no_diagnostic["login_diagnostic_count"], "0");
    assert!(!no_diagnostic.contains_key("login_diagnostic_0_words"));
    for (key, value) in &no_diagnostic {
        if key != "login_diagnostic_count" {
            assert_eq!(reply.get(key), Some(value));
        }
    }
    for private in [
        "must-not-appear",
        "expected@example.test",
        "/synthetic/login",
    ] {
        assert!(reply.values().all(|value| !value.contains(private)));
    }
    let mut paused = value.clone();
    pause_progress(&mut paused, true, false);
    for (history, busy, phase, paused) in [
        (&value, true, "unknown", "0"),
        (&paused, false, "paused", "1"),
        (&paused, true, "unknown", "1"),
    ] {
        let retained = rpc(history, busy);
        assert_eq!(retained["phase"], phase);
        assert_eq!(retained["forecasts_paused"], paused);
        assert_eq!(retained["eligible"], "-");
        assert_eq!(retained["next_at"], "-");
        assert_eq!(retained["observed_at"], "100");
        for key in [
            "login_diagnostic_count",
            "login_diagnostic_0_account",
            "login_diagnostic_0_completed_at",
            "login_diagnostic_0_words",
        ] {
            assert_eq!(retained[key], reply[key]);
        }
    }
    let lab = crate::lab_screen::progress_facts_at(Some(&value), Some(100));
    let lab_text = format!("{lab:?}");
    assert!(lab_text.contains("headless browser"));
    assert!(!lab_text.contains("must-not-appear"));
    assert!(!lab_text.contains("expected@example.test"));
    assert!(!lab_text.contains("/synthetic/login"));
    let mut spoofed = control.clone();
    spoofed
        .details
        .push(format!("Last local sign-in for shared: {text}"));
    let spoofed_lab = crate::lab_screen::progress_facts_at(Some(&spoofed), Some(100));
    assert!(!format!("{spoofed_lab:?}").contains("headless browser"));
    for at in [0, 101] {
        let mut invalid = value.clone();
        invalid.login_diagnostics[0].completed_at = at;
        let lab = crate::lab_screen::progress_facts_at(Some(&invalid), Some(100));
        assert!(!format!("{lab:?}").contains("headless browser"));
    }

    let mut replaced = row.clone();
    replaced.email = Some("replacement@example.test".into());
    let mut relocated = row.clone();
    relocated.dir = Some("/synthetic/replacement".into());
    for candidates in [
        vec![],
        vec![replaced],
        vec![relocated],
        vec![row.clone(), row.clone()],
    ] {
        let suppressed = project(&candidates, activity(Some(diagnosis(80)), false));
        assert_eq!(suppressed, project(&candidates, activity(None, false)),);
        assert_eq!(rpc(&suppressed, false)["login_diagnostic_count"], "0");
    }
    for at in [0, 101] {
        let suppressed = project(&rows, activity(Some(diagnosis(at)), false));
        assert_eq!(suppressed, control,);
        assert_eq!(rpc(&suppressed, false)["login_diagnostic_count"], "0");
    }
    assert_eq!(
        project(&rows, activity(Some(diagnosis(80)), true)),
        project(&rows, activity(None, true)),
    );

    let facts = crate::fact_menu::FactMenuFacts {
        account_progress: Some(value),
        ..Default::default()
    };
    let menu = crate::fact_menu::rows(crate::claude_lights::Fact::Account, &facts);
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    for cols in [60, 120] {
        let rect = crate::autopilot_menu::place(&menu, (25, 0), (0..26, 0..cols), (cols, 26), 0)
            .expect("account menu fits");
        let painted = crate::autopilot_menu::paint(rect, &menu, Default::default(), None, theme);
        let text: String = painted
            .iter()
            .flat_map(|line| line.iter().map(|cell| cell.ch))
            .collect();
        assert!(
            text.contains("headless") && text.contains("browser"),
            "{text}"
        );
        assert!(!text.contains("must-not-appear"));
    }
}

#[test]
fn recent_login_activity_does_not_replace_an_account_move() {
    use aterm_agent::harness::accounts_sync::{LoginActivity, LoginIdentity};
    let row = accounts::Row {
        label: "current".into(),
        ..accounts::Row::default()
    };
    for phase in [
        ProgressPhase::Switching { to: "next".into() },
        ProgressPhase::Verifying { to: "next".into() },
    ] {
        let mut value = AccountProgress::unknown(100);
        value.phase = phase.clone();
        apply_login_activity(
            &mut value,
            &BTreeSet::from(["current".into()]),
            std::slice::from_ref(&row),
            vec![LoginActivity {
                label: "current".into(),
                identity: Some(LoginIdentity::for_test(&row)),
                route: Some("profile".into()),
                active: true,
                observed_at: 90,
                next_at: None,
                diagnostic: None,
            }],
            100,
        );
        assert_eq!(value.phase, phase);
        assert_eq!(value.details.len(), 2);
    }
}

#[test]
fn account_progress_fences_current_process_conversation_and_generation() {
    let (host, kept) = fixture();
    assert!(read(&host).is_some());
    for (pid, birth, conversation) in [
        (4243, Some(BORN), Some(CONVERSATION)),
        (4242, Some(BORN + 1), Some(CONVERSATION)),
        (4242, None, Some(CONVERSATION)),
        (4242, Some(BORN), Some("conversation-b")),
        (4242, Some(BORN), None),
    ] {
        assert!(
            host.account_progress(SID, pid, birth, conversation)
                .unwrap()
                .is_none()
        );
    }
    {
        let mut kept = kept.lock().unwrap();
        kept.snapshot.as_mut().unwrap().session = Some("conversation-b".into());
    }
    assert!(
        read(&host).is_none(),
        "a stale footer cannot revive the previous conversation"
    );
    {
        let mut kept = kept.lock().unwrap();
        kept.snapshot.as_mut().unwrap().session = Some(CONVERSATION.into());
        kept.account.landing = Arc::new(Mutex::new(()));
    }
    assert!(
        read(&host).is_none(),
        "a replaced landing generation fences old progress"
    );
}

#[test]
fn account_progress_only_reports_a_probe_owned_by_the_current_process() {
    let (host, kept) = fixture();
    kept.lock().unwrap().account.probing = Some(Probing {
        at: 0,
        label: "current".into(),
        pid: 4243,
        start: "process-a".into(),
        row: accounts::Row::default(),
        who: accounts::ProcessLogin {
            login: None,
            default_home: true,
            stored_login: true,
        },
        roster: PathBuf::from("/synthetic/roster"),
        state: PathBuf::from("/synthetic/state"),
    });
    assert_eq!(read(&host).unwrap().phase, ProgressPhase::Watching);
    {
        let mut kept = kept.lock().unwrap();
        let probe = kept.account.probing.as_mut().unwrap();
        probe.pid = 4242;
        probe.start = "process-before-reuse".into();
    }
    assert_eq!(read(&host).unwrap().phase, ProgressPhase::Watching);
    kept.lock().unwrap().account.probing.as_mut().unwrap().start = "process-a".into();
    assert_eq!(read(&host).unwrap().phase, ProgressPhase::Probing);
}

#[test]
fn account_progress_distinguishes_refused_and_in_flight_restarts() {
    let (host, kept) = fixture();
    kept.lock().unwrap().account.flying = Some(Flying {
        origin: AccountContext {
            roster: None,
            state: PathBuf::from("/synthetic/state"),
            home: PathBuf::from("/synthetic/home"),
        },
        to: "alternate".into(),
        at: AccountAt::Wall,
        login: None,
        pid: 4242,
        start: "process-a".into(),
        rejected: Some(RejectedMove {
            on: "alternate".into(),
            why: accounts::Strike::NotMoved("fixture refusal".into()),
            canary: None,
            session: None,
            held: None,
        }),
    });
    let refused = read(&host).unwrap();
    assert_eq!(refused.phase, ProgressPhase::Waiting);
    assert_eq!(
        (refused.known, refused.eligible, refused.next_at),
        (None, None, None)
    );
    assert_eq!(read(&host), Some(refused));
    assert_eq!(
        kept.lock()
            .unwrap()
            .account
            .progress
            .as_ref()
            .unwrap()
            .value
            .phase,
        ProgressPhase::Watching,
        "polling derives a view without rewriting the recorded decision"
    );
    kept.lock()
        .unwrap()
        .account
        .flying
        .as_mut()
        .unwrap()
        .rejected = None;
    assert_eq!(
        read(&host).unwrap().phase,
        ProgressPhase::Switching {
            to: "alternate".into()
        }
    );
    kept.lock().unwrap().account.flying.as_mut().unwrap().pid = 4243;
    assert_eq!(read(&host).unwrap().phase, ProgressPhase::Watching);
}

#[test]
fn a_retired_judgment_keeps_independently_observed_work_visible() {
    use aterm_agent::harness::accounts_sync::{LoginActivity, LoginDiagnostic, LoginIdentity};
    let row = accounts::Row {
        label: "alternate".into(),
        ..accounts::Row::default()
    };
    for kind in 0..4 {
        let (host, kept) = fixture();
        let origin = AccountContext {
            roster: None,
            state: "/synthetic/state".into(),
            home: "/synthetic/home".into(),
        };
        let retired = {
            let mut held = kept.lock().unwrap();
            let bound = held.account.progress.as_mut().unwrap();
            bound.state = Some("/synthetic/state".into());
            bound.labels = BTreeSet::from([row.label.clone()]);
            bound.rows = vec![row.clone()].into();
            bound.value.phase = ProgressPhase::Ready;
            bound.value.eligible = Some(1);
            bound.value.next_at = Some(200);
            bound
                .value
                .login_diagnostics
                .push(LoginDiagnostic::from_exit_for_test(&row, 9, 1, None));
            retire_judgment(
                &mut bound.value,
                AccountPoint::TurnEnd,
                aterm_agent::harness::account_progress::BINDINGS_CHANGED_FINDING,
            );
            let retired = bound.value.clone();
            match kind {
                0 => {
                    held.account.probing = Some(Probing {
                        at: 0,
                        label: row.label.clone(),
                        pid: 4242,
                        start: "process-a".into(),
                        row: row.clone(),
                        who: accounts::ProcessLogin {
                            login: None,
                            default_home: true,
                            stored_login: true,
                        },
                        roster: "/synthetic/roster".into(),
                        state: "/synthetic/state".into(),
                    })
                }
                1 => {
                    held.account.flying = Some(Flying {
                        origin,
                        to: row.label.clone(),
                        at: AccountAt::Wall,
                        login: None,
                        pid: 4242,
                        start: "process-a".into(),
                        rejected: None,
                    })
                }
                2 => {
                    held.account.moved = Some(Moved {
                        id: MoveId::default(),
                        origin,
                        to: row.label.clone(),
                        at: AccountAt::Wall,
                        login: None,
                        process: held.snapshot.clone().unwrap(),
                        answered: false,
                        untold: false,
                    })
                }
                _ => {}
            }
            retired
        };
        let value = host
            .account_progress_with_activity(
                SID,
                4242,
                Some(BORN),
                Some(CONVERSATION),
                || (Instant::now(), Some(100)),
                |_, _, _| {
                    Some(if kind == 3 {
                        vec![LoginActivity {
                            label: row.label.clone(),
                            identity: Some(LoginIdentity::for_test(&row)),
                            route: Some("mailbox".into()),
                            active: true,
                            observed_at: 90,
                            next_at: None,
                            diagnostic: None,
                        }]
                    } else {
                        Vec::new()
                    })
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            value.phase,
            match kind {
                0 => ProgressPhase::Probing,
                1 => ProgressPhase::Switching {
                    to: row.label.clone()
                },
                2 => ProgressPhase::Verifying {
                    to: row.label.clone()
                },
                _ => ProgressPhase::SigningIn {
                    label: row.label.clone(),
                    route: Some("mailbox".into())
                },
            }
        );
        assert_eq!(value.next_at, None);
        assert_eq!(value.eligible, None);
        assert_eq!(value.login_diagnostics, retired.login_diagnostics);
        assert_eq!(
            kept.lock()
                .unwrap()
                .account
                .progress
                .as_ref()
                .unwrap()
                .value,
            retired
        );
    }
}

#[test]
fn account_progress_distinguishes_current_policy_from_an_old_disabled_observation() {
    let (host, kept) = fixture();
    kept.lock()
        .unwrap()
        .account
        .progress
        .as_mut()
        .unwrap()
        .value
        .phase = ProgressPhase::Disabled;
    assert_eq!(read(&host).unwrap().phase, ProgressPhase::Unknown);
    host.shared
        .switches
        .account_controls
        .store(ACCOUNTS_OFF, Ordering::SeqCst);
    assert_eq!(read(&host).unwrap().phase, ProgressPhase::Disabled);
    host.shared
        .switches
        .account_controls
        .store(ACCOUNTS_WALL, Ordering::SeqCst);
    assert_eq!(read(&host).unwrap().phase, ProgressPhase::Unknown);
}

#[test]
fn account_progress_contention_is_unavailable_and_never_waits_for_a_writer() {
    let (host, kept) = fixture();
    let query = || {
        let host = host.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _ = send.send(host.account_progress(SID, 4242, Some(BORN), Some(CONVERSATION)));
        });
        (receive, thread)
    };
    let map = host.shared.kept.lock().unwrap();
    let (reply, thread) = query();
    let observed = reply.recv_timeout(Duration::from_secs(60));
    drop(map);
    thread.join().unwrap();
    assert!(observed.expect("the nonblocking read returned").is_err());
    let writer = kept.lock().unwrap();
    let (reply, thread) = query();
    let observed = reply.recv_timeout(Duration::from_secs(60));
    drop(writer);
    thread.join().unwrap();
    assert!(observed.expect("the nonblocking read returned").is_err());
    assert!(read(&host).is_some());
}

#[test]
fn service_progress_retries_a_busy_activity_read_and_then_shows_sign_in() {
    use aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING;
    use aterm_agent::harness::accounts_sync::{LoginActivity, LoginIdentity};
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};

    for discard_contention in [false, true] {
        let start = Instant::now();
        let until = start + Duration::from_secs(100);
        let mut app = crate::App::headless_for_test();
        let local = app.focused_session_id(crate::WindowId(0)).unwrap();
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        let sid = app.session_sid(local).unwrap();
        let (host, kept) = fixture();
        let activity_row = accounts::Row {
            label: "alternate".into(),
            ..accounts::Row::default()
        };
        host.shared
            .switches
            .account_controls
            .store(ACCOUNTS_OFF, Ordering::SeqCst);
        {
            let mut map = host.shared.kept.lock().unwrap();
            let entry = map.remove(SID).unwrap();
            map.insert(sid.clone(), entry);
        }
        let retained = {
            let mut kept = kept.lock().unwrap();
            kept.snapshot.as_mut().unwrap().tab = sid.clone();
            let mut bound = kept.account.progress.take().unwrap();
            bound.snapshot.tab = sid.clone();
            bound.state = Some("/synthetic/state".into());
            bound.labels.insert("alternate".into());
            bound.rows = vec![activity_row.clone()].into();
            bound.value.phase = ProgressPhase::Waiting;
            bound.value.observed_at = 100;
            bound.value.next_at = Some(200);
            bound.value.details.push(EXECUTION_PREPARED_FINDING.into());
            bound.execution_until = Some(until);
            let retained = bound.value.clone();
            kept.account.execution_progress = Some(bound);
            retained
        };
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(4242);
            timeline.set_program(4242, Some("node".into()));
            timeline.set_agent_reader_program_for_test(Some(Program::Claude));
            timeline.set_claude_footer(
                4242,
                Some(BORN),
                Some(FooterFacts {
                    owner: Some(FactsOwner {
                        floor: Some(BORN),
                        session_id: CONVERSATION.into(),
                    }),
                    ..FooterFacts::default()
                }),
            );
        }
        app.harness = Some(host);
        for step in 0..3 {
            let now = start + Duration::from_secs(step);
            let mut calls = 0;
            app.service_harness_account_progress_with_reader(
                now,
                |host, asked_sid, pid, birth, conversation| {
                    calls += 1;
                    assert_eq!(asked_sid, sid);
                    let mut read = host
                        .account_progress_read_with_activity(
                            asked_sid,
                            pid,
                            birth,
                            conversation,
                            || (now, Some(100)),
                            |_, _, stamp| match step {
                                0 => None,
                                1 => Some(vec![LoginActivity {
                                    label: "alternate".into(),
                                    identity: Some(LoginIdentity::for_test(&activity_row)),
                                    route: None,
                                    active: true,
                                    observed_at: stamp,
                                    next_at: None,
                                    diagnostic: None,
                                }]),
                                _ => Some(Vec::new()),
                            },
                        )
                        .unwrap();
                    if discard_contention {
                        read.activity_contended = false;
                    }
                    Ok(read)
                },
            );
            assert_eq!(calls, 1);
            let observed = ctx
                .timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .account_progress
                .clone()
                .unwrap();
            assert_eq!(
                observed.phase,
                if step == 1 {
                    ProgressPhase::SigningIn {
                        label: "alternate".into(),
                        route: None,
                    }
                } else {
                    ProgressPhase::Waiting
                }
            );
            assert_eq!(observed.next_at, retained.next_at);
            assert_eq!(observed.details.first(), retained.details.first());
            assert!(
                observed
                    .details
                    .iter()
                    .any(|s| s == "Automatic account rotation is off")
            );
            if discard_contention {
                // Identical Waiting reaches the timeline, but lost contention misses the earlier retry.
                assert_eq!(app.harness_progress_next_poll, Some(until));
                break;
            }
            assert_eq!(
                app.harness_progress_next_poll,
                Some(if step != 2 {
                    now + Duration::from_secs(1)
                } else {
                    until
                })
            );
            assert_eq!(
                kept.lock()
                    .unwrap()
                    .account
                    .execution_progress
                    .as_ref()
                    .unwrap()
                    .value,
                retained
            );
        }
    }
}

fn app_execution_progress(
    until: Instant,
) -> (crate::App, HostHandle, Arc<Mutex<Kept>>, u64, String) {
    use aterm_agent::harness::account_progress::{
        EXECUTION_PREPARED_FINDING, OwnClaimBlocker, OwnClaimFinding,
    };
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};
    let mut app = crate::App::headless_for_test();
    app.proxy = None;
    let local = app.focused_session_id(crate::WindowId(0)).unwrap();
    let sid = app.session_sid(local).unwrap();
    let (host, kept) = fixture();
    {
        let mut map = host.shared.kept.lock().unwrap();
        let entry = map.remove(SID).unwrap();
        map.insert(sid.clone(), entry);
    }
    {
        let mut kept = kept.lock().unwrap();
        kept.snapshot.as_mut().unwrap().tab = sid.clone();
        let mut bound = kept.account.progress.take().unwrap();
        bound.snapshot.tab = sid.clone();
        bound.execution_until = Some(until);
        bound.value.observed_at = 100;
        bound.value.account = Some("recorded-account".into());
        bound.value.known = Some(2);
        bound.value.eligible = Some(1);
        bound.value.phase = ProgressPhase::Waiting;
        bound.value.wall = Some("weekly".into());
        bound.value.next_at = Some(200);
        bound.value.details.push(EXECUTION_PREPARED_FINDING.into());
        bound.value.own_claim = Some(OwnClaimFinding {
            blocker: OwnClaimBlocker::HeldElsewhere { until: 150 },
            pinned: true,
        });
        kept.account.execution_progress = Some(bound);
    }
    {
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        let mut timeline = ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(4242);
        timeline.set_program(4242, Some("node".into()));
        timeline.set_agent_reader_program_for_test(Some(Program::Claude));
        timeline.set_claude_footer(
            4242,
            Some(BORN),
            Some(FooterFacts {
                owner: Some(FactsOwner {
                    floor: Some(BORN),
                    session_id: CONVERSATION.into(),
                }),
                ..Default::default()
            }),
        );
    }
    app.harness = Some(host.clone());
    (app, host, kept, local, sid)
}

fn service_at(app: &mut crate::App, mono: Instant, wall: i64) {
    app.service_harness_account_progress_with_reader(
        mono,
        |host, sid, pid, birth, conversation| {
            host.account_progress_read_with_activity(
                sid,
                pid,
                birth,
                conversation,
                || (mono, Some(wall)),
                |_, _, _| Some(Vec::new()),
            )
        },
    );
}

pub(in crate::harness_host) fn with_progress_writer<T: Send, R>(
    writer: &Mutex<T>,
    run: impl FnOnce() -> R,
) -> R {
    std::thread::scope(|scope| {
        let (ready, held) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let worker = scope.spawn(move || {
            let _guard = writer.lock().unwrap();
            ready.send(()).expect("the App waits for its writer");
            match released.recv_timeout(Duration::from_secs(60)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    panic!("the App did not return before its writer's hang detector");
                }
            }
        });
        held.recv_timeout(Duration::from_secs(60))
            .expect("the actual writer acquired its mutex");
        // Only the worker owns a guard. Dropping this sender also releases it if `run`
        // unwinds, before thread::scope joins; no test guard nests App/global locks.
        let result = run();
        drop(release);
        worker.join().expect("the writer released its mutex");
        result
    })
}

fn publish_test_off(host: &HostHandle, sid: &str, off: bool) {
    let policy = OffSessions {
        words: HashMap::from([(
            sid.to_owned(),
            if off {
                Autopilot::Off
            } else {
                Autopilot::Cruise
            },
        )]),
        default: Autopilot::Cruise,
    };
    let mut hooks = host.hooks.clone();
    hooks.note = Arc::new(|_| {});
    publish_off(
        &host.shared,
        &Workers::default(),
        &HashMap::new(),
        &policy,
        &hooks,
    );
}

#[test]
fn off_policy_reads_are_nonblocking_and_a_changed_verdict_cannot_publish() {
    for change_policy in [false, true] {
        let start = Instant::now();
        let (mut app, host, kept, local, sid) =
            app_execution_progress(start + Duration::from_secs(10));
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        {
            let mut kept = kept.lock().unwrap();
            let mut normal = kept.account.execution_progress.take().unwrap();
            normal.execution_until = None;
            kept.account.progress = Some(normal);
        }
        service_at(&mut app, start, 100);
        let retained = ctx
            .timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .account_progress
            .clone();
        if change_policy {
            let model = aterm_models_harness::account_progress_policy_model();
            let mut policy = model.init_state();
            for action in ["Read", "Deliver"] {
                assert!(model.fire(action, &mut policy));
            }
            let initial = Arc::clone(&kept.lock().unwrap().account.progress_policy);
            app.service_harness_account_progress_with_reader(
                start,
                |host, sid, pid, born, talk| {
                    let result = host.account_progress_read_with_activity(
                        sid,
                        pid,
                        born,
                        talk,
                        || (start, Some(100)),
                        |_, _, _| Some(Vec::new()),
                    );
                    assert!(model.fire("Read", &mut policy));
                    publish_test_off(host, sid, true);
                    assert!(model.fire("Off", &mut policy));
                    result
                },
            );
            assert!(model.fire("Deliver", &mut policy));
            let view =
                crate::harness_recovery_verb::snapshot(&ctx.timeline.lock().unwrap()).unwrap();
            assert!(
                !view.forecasts_paused,
                "the stale read establishes no new policy verdict"
            );
            assert_eq!(
                ctx.timeline
                    .lock()
                    .unwrap()
                    .claude_footer()
                    .unwrap()
                    .account_forecast_refusal(),
                Some(aterm_agent::harness::outlook::Refusal::ObserverBusy)
            );
            let (epoch, record_epoch, record_value) = {
                let kept = kept.lock().unwrap();
                (
                    i64::from(!Arc::ptr_eq(&initial, &kept.account.progress_policy)),
                    i64::from(!kept.account.progress.as_ref().unwrap().policy_is(&initial)),
                    i64::from(
                        kept.account.progress.as_ref().unwrap().value.observed_at
                            != retained.as_ref().unwrap().observed_at,
                    ),
                )
            };
            let off = host.shared.lock().off.contains(&sid);
            for (field, actual) in [
                ("turn", i64::from(off)),
                ("epoch", epoch),
                ("record_epoch", record_epoch),
                ("record_value", record_value),
                ("producer_epoch", 3),
                (
                    "view",
                    if view.phase == ProgressPhase::Unknown {
                        3
                    } else {
                        1
                    },
                ),
                ("next", i64::from(view.next_at.is_some())),
            ] {
                assert_eq!(policy[field], actual, "{field}: {policy:?}");
            }
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &policy),
                    "{}",
                    invariant.name
                );
            }
            let mut former = policy.clone();
            former.insert("view", 1);
            former.insert("next", 1);
            assert!(!model.check_invariant("StaleReadWithdrawsClaims", &former));
        } else {
            with_progress_writer(&host.shared.state, || service_at(&mut app, start, 100));
        }
        let facts = ctx
            .timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .clone();
        assert!(facts.account_progress_busy);
        assert_eq!(facts.account_progress, retained);
        assert_eq!(facts.account_progress_view().unwrap().next_at, None);
        assert!(!facts.account_progress_view().unwrap().forecasts_paused);
        assert_eq!(
            facts.account_forecast_refusal(),
            Some(aterm_agent::harness::outlook::Refusal::ObserverBusy)
        );
        assert_eq!(
            app.harness_progress_next_poll,
            Some(start + Duration::from_secs(1))
        );
        service_at(&mut app, start + Duration::from_secs(1), 100);
        let facts = ctx
            .timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .clone();
        assert!(!facts.account_progress_busy);
        assert_eq!(
            facts.account_progress_view().unwrap().forecasts_paused,
            change_policy
        );
        assert_eq!(
            facts.account_progress_view().unwrap().phase,
            if change_policy {
                ProgressPhase::Paused
            } else {
                ProgressPhase::Waiting
            }
        );
        assert_eq!(
            kept.lock()
                .unwrap()
                .account
                .progress
                .as_ref()
                .unwrap()
                .value,
            retained.unwrap()
        );
    }
}

#[test]
fn off_without_a_retained_session_is_explicit_but_invents_no_account() {
    use aterm_agent::harness::account_progress::SESSION_AUTOPILOT_OFF_FINDING;
    let (host, _) = fixture();
    host.shared.kept.lock().unwrap().remove(SID);
    let mut hooks = host.hooks.clone();
    let notes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let noted = Arc::clone(&notes);
    hooks.note = Arc::new(move |note| {
        assert!(matches!(note, HostNote::AccountProgress));
        noted.fetch_add(1, Ordering::SeqCst);
    });
    let wanted = HashMap::from([(SID.to_owned(), Program::Claude)]);
    let off = OffSessions {
        words: HashMap::new(),
        default: Autopilot::Off,
    };
    for _ in 0..2 {
        publish_off(&host.shared, &Workers::default(), &wanted, &off, &hooks);
        assert_eq!(notes.load(Ordering::SeqCst), 1);
    }
    let progress = read(&host).unwrap();
    assert_eq!(progress.phase, ProgressPhase::Paused);
    assert!(progress.forecasts_paused);
    assert_eq!(
        (
            progress.observed_at,
            progress.known,
            progress.eligible,
            progress.next_at
        ),
        (0, None, None, None)
    );
    assert_eq!(progress.account, None);
    assert_eq!(progress.details, [SESSION_AUTOPILOT_OFF_FINDING]);
}

#[test]
fn an_off_observation_cannot_cross_a_rebound_process_or_conversation() {
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};
    for (pid, born, talk) in [
        (4243, BORN, CONVERSATION),
        (4242, BORN + 1, CONVERSATION),
        (4242, BORN, "another-conversation"),
    ] {
        let now = Instant::now();
        let (mut app, host, kept, local, sid) =
            app_execution_progress(now + Duration::from_secs(10));
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        {
            let mut kept = kept.lock().unwrap();
            let mut normal = kept.account.execution_progress.take().unwrap();
            normal.execution_until = None;
            kept.account.progress = Some(normal);
        }
        publish_test_off(&host, &sid, true);
        app.service_harness_account_progress_with_reader(
            now,
            |host, sid, old_pid, old_born, old_talk| {
                let read = host.account_progress_read_with_activity(
                    sid,
                    old_pid,
                    old_born,
                    old_talk,
                    || (now, Some(100)),
                    |_, _, _| Some(Vec::new()),
                );
                assert_eq!(
                    read.as_ref().unwrap().progress.as_ref().unwrap().phase,
                    ProgressPhase::Paused
                );
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(pid);
                timeline.set_program(pid, Some("claude".into()));
                timeline.set_claude_footer(
                    pid,
                    Some(born),
                    Some(FooterFacts {
                        owner: Some(FactsOwner {
                            floor: Some(born),
                            session_id: talk.into(),
                        }),
                        ..Default::default()
                    }),
                );
                read
            },
        );
        let timeline = ctx.timeline.lock().unwrap();
        let facts = timeline.claude_footer().unwrap();
        assert_eq!(facts.account_progress, None);
        assert!(!facts.account_progress_busy);
        assert!(!facts.account_forecasts_paused());
    }
}

fn check_app_busy_expiry_laws(facts: &aterm_agent::harness::footer::FooterFacts) {
    use aterm_agent::harness::account_progress::{
        EXECUTION_PREPARED_FINDING, RECOVERY_HISTORY_PREFIX,
    };
    let model = aterm_models_harness::account_execution_visibility_model();
    let mut context = model.init_state();
    context.insert("case", 21); // An expired gate whose observation requires another display read.
    for action in ["Observe", "Elapse", "Project"] {
        assert!(model.fire(action, &mut context));
    }
    let project = |facts: &aterm_agent::harness::footer::FooterFacts| {
        let mut state = context.clone();
        let view = facts.account_progress_view().unwrap();
        state.insert(
            "view",
            match view.phase {
                ProgressPhase::Unknown => 8,
                ProgressPhase::Waiting => 1,
                _ => panic!("this fixture projects only waiting and unknown recovery"),
            },
        );
        state.insert("deadline", i64::from(view.next_at.is_some()));
        state.insert(
            "current",
            i64::from(view.details.iter().any(|s| s == EXECUTION_PREPARED_FINDING)),
        );
        state.insert(
            "historical",
            i64::from(view.details.iter().any(|s| {
                s.strip_prefix(RECOVERY_HISTORY_PREFIX) == Some(EXECUTION_PREPARED_FINDING)
            })),
        );
        state
    };
    let actual = project(facts);
    let mut former = facts.clone();
    former.account_progress_busy = false; // Former outer-Err behavior retained these same facts.
    let former = project(&former);
    // Only these four display fields are projected; this is not a new full transition proof
    // or a claim that the model's gate/inventory inputs cover App mutex contention.
    for law in [
        "NormalPhaseRestored",
        "ActualRecheckShown",
        "CurrentHoldRequiresUnexpiredLease",
        "ExpiredFindingIsHistorical",
    ] {
        assert!(model.check_invariant(law, &actual), "{law}: {actual:?}");
        assert!(
            !model.check_invariant(law, &former),
            "former outer-Err behavior escaped {law}"
        );
    }
}

#[test]
fn an_unavailable_supervisor_read_withdraws_current_claims_across_execution_expiry() {
    use aterm_agent::harness::account_progress::{
        EXECUTION_PREPARED_FINDING, EXECUTION_REFRESH_FINDING, OBSERVER_BUSY_FINDING,
        RECOVERY_HISTORY_PREFIX,
    };
    for hold_map in [true, false] {
        let start = Instant::now();
        let until = start + Duration::from_secs(10);
        let (mut app, host, kept, local, sid) = app_execution_progress(until);
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        service_at(&mut app, start, 100);
        let retained = ctx
            .timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .account_progress
            .clone()
            .unwrap();
        assert_eq!(app.harness_progress_next_poll, Some(until));
        assert_eq!(retained.eligible, Some(1));
        let mut previous = None;
        for step in 0..3 {
            let now = until + Duration::from_secs(step);
            // A concurrent writer holds each actual production lock. The App thread owns
            // no guard, and its timeline stays available for the unavailable-read fallback.
            let read = || service_at(&mut app, now, 99); // Wall rollback cannot renew the lease.
            if hold_map {
                with_progress_writer(&host.shared.kept, read);
            } else {
                with_progress_writer(kept.as_ref(), read);
            }
            let facts = ctx
                .timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .clone();
            assert!(facts.account_progress_busy);
            assert_eq!(facts.account_progress.as_ref(), Some(&retained));
            check_app_busy_expiry_laws(&facts);
            let view = facts.account_progress_view().unwrap().into_owned();
            assert_eq!(view.phase, ProgressPhase::Unknown);
            assert_eq!(view.eligible, None);
            assert_eq!(view.next_at, None);
            assert_eq!(view.observed_at, retained.observed_at);
            assert_eq!(view.account, retained.account);
            assert_eq!(view.known, retained.known);
            assert_eq!(view.wall, retained.wall);
            assert_eq!(view.own_claim, retained.own_claim);
            assert_eq!(
                view.details.first().map(String::as_str),
                Some(OBSERVER_BUSY_FINDING)
            );
            assert!(view.details.contains(&format!(
                "{RECOVERY_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}"
            )));
            if let Some(previous) = &previous {
                assert_eq!(previous, &view);
            }
            previous = Some(view.clone());
            assert_eq!(
                app.harness_progress_next_poll,
                Some(now + Duration::from_secs(1))
            );
            let footer = aterm_agent::harness::footer::account_progress_text(&view, false);
            assert!(footer.contains(&aterm_agent::harness::footer::account_progress_phase(&view)));
            let old_next = aterm_agent::harness::usage::rfc3339_utc(200);
            assert!(!footer.contains(&old_next));

            // Actual menu opening uses its production facts adapter; absent proxy prevents
            // its independent optional account reader from touching any account records.
            app.open_fact_menu(
                crate::WindowId(0),
                local,
                crate::claude_lights::Fact::Account,
                (0, 0),
                false,
            );
            let menu = &app.autopilot_menu.as_ref().unwrap().rows;
            let notes: Vec<_> = menu
                .iter()
                .filter_map(|row| match row {
                    crate::autopilot_menu::MenuRow::Note { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert!(
                notes
                    .iter()
                    .any(|line| line.contains(OBSERVER_BUSY_FINDING))
            );
            assert!(notes.iter().all(|line| !line.contains(&old_next)));
            assert!(!notes.contains(&EXECUTION_PREPARED_FINDING));
            let lab = app.harness_state().lab;
            let lab = lab
                .sessions
                .iter()
                .find(|session| session.local_id == local)
                .unwrap();
            assert!(
                lab.facts
                    .iter()
                    .any(|(_, value)| value == OBSERVER_BUSY_FINDING)
            );
            assert!(lab.facts.iter().all(
                |(_, value)| value != EXECUTION_PREPARED_FINDING && !value.contains(&old_next)
            ));
            let observed =
                crate::harness_recovery_verb::snapshot(&ctx.timeline.lock().unwrap()).unwrap();
            assert_eq!(observed, view);
            let reply = crate::harness_recovery_verb::cmd("recovery", true, || Ok(Some(observed)));
            for field in [
                "phase=unknown",
                "eligible=-",
                "next_at=-",
                "known=2",
                "observed_at=100",
            ] {
                assert!(
                    reply.split_whitespace().any(|word| word == field),
                    "{reply}"
                );
            }
        }
        // The reader becomes available, but an old receipt remains expired even with the
        // wall clock behind its original observation. Its source record was never rewritten.
        service_at(&mut app, until + Duration::from_secs(3), 99);
        let facts = ctx
            .timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .clone();
        assert!(!facts.account_progress_busy);
        let observed = facts.account_progress_view().unwrap();
        assert_eq!(observed.phase, ProgressPhase::Unknown);
        assert_eq!(observed.next_at, None);
        assert_eq!(
            observed.details.first().map(String::as_str),
            Some(EXECUTION_REFRESH_FINDING)
        );
        assert_eq!(app.harness_progress_next_poll, None);
        assert_eq!(
            kept.lock()
                .unwrap()
                .account
                .execution_progress
                .as_ref()
                .unwrap()
                .value,
            retained
        );
        assert!(host.shared.kept.lock().unwrap().contains_key(&sid));
    }
}

#[test]
fn a_successful_same_decision_clears_the_apps_busy_projection_before_expiry() {
    let start = Instant::now();
    let until = start + Duration::from_secs(10);
    let (mut app, host, kept, local, _) = app_execution_progress(until);
    let ctx = app.pool.get(local).unwrap().ctx.clone();
    service_at(&mut app, start, 100);
    let retained = ctx
        .timeline
        .lock()
        .unwrap()
        .claude_footer()
        .unwrap()
        .account_progress
        .clone();
    // A real host note bypasses the still-future lease wake; its real reader must encounter
    // a concurrent writer's production map. No new decision or synthetic error is supplied.
    with_progress_writer(&host.shared.kept, || {
        app.account_progress_noted(start + Duration::from_secs(1));
    });
    assert!(
        ctx.timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .account_progress_busy
    );
    service_at(&mut app, start + Duration::from_secs(2), 100);
    let facts = ctx
        .timeline
        .lock()
        .unwrap()
        .claude_footer()
        .unwrap()
        .clone();
    assert!(!facts.account_progress_busy);
    assert_eq!(facts.account_progress, retained);
    assert_eq!(
        facts.account_progress_view().unwrap().as_ref(),
        retained.as_ref().unwrap()
    );
    assert_eq!(app.harness_progress_next_poll, Some(until));
    assert_eq!(
        kept.lock()
            .unwrap()
            .account
            .execution_progress
            .as_ref()
            .unwrap()
            .value,
        retained.unwrap()
    );
}

#[test]
fn an_unavailable_read_cannot_mark_a_rebound_foreground_as_busy() {
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};
    for (new_pid, new_birth, new_conversation) in [
        (4243, BORN, CONVERSATION),
        (4242, BORN + 1, CONVERSATION),
        (4242, BORN, "new-conversation"),
    ] {
        let start = Instant::now();
        let until = start + Duration::from_secs(10);
        let (mut app, host, _, local, _) = app_execution_progress(until);
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        service_at(&mut app, start, 100);
        let mut unavailable = false;
        with_progress_writer(&host.shared.kept, || {
            app.service_harness_account_progress_with_reader(
                until,
                |host, sid, pid, birth, conversation| {
                    let result = host.account_progress_read_with_activity(
                        sid,
                        pid,
                        birth,
                        conversation,
                        || (until, Some(99)),
                        |_, _, _| panic!("the actual held map must refuse first"),
                    );
                    unavailable = result.is_err();
                    let mut timeline = ctx.timeline.lock().unwrap();
                    timeline.note_foreground_group(new_pid);
                    timeline.set_program(new_pid, Some("node".into()));
                    timeline.set_agent_reader_program_for_test(Some(Program::Claude));
                    timeline.set_claude_footer(
                        new_pid,
                        Some(new_birth),
                        Some(FooterFacts {
                            owner: Some(FactsOwner {
                                floor: Some(new_birth),
                                session_id: new_conversation.into(),
                            }),
                            ..Default::default()
                        }),
                    );
                    result
                },
            );
        });
        assert!(
            unavailable,
            "the real concurrent writer must make the read unavailable"
        );
        let facts = ctx
            .timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .clone();
        assert!(!facts.account_progress_busy);
        assert!(facts.account_progress.is_none());
        assert_eq!(
            app.harness_progress_next_poll,
            Some(until + Duration::from_secs(1))
        );
        service_at(&mut app, until + Duration::from_secs(1), 99);
        assert!(
            !ctx.timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .account_progress_busy
        );
        assert!(
            ctx.timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .account_progress
                .is_none()
        );
    }
}

#[test]
fn service_progress_follows_reader_owned_claude_but_not_an_unrelated_program() {
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};

    for (program, reader, accepted) in [
        ("node", Some(Program::Claude), true),
        ("bun", Some(Program::Claude), true),
        ("vim", Some(Program::Claude), false),
        ("node", None, false),
    ] {
        let mut app = crate::App::headless_for_test();
        let local = app.focused_session_id(crate::WindowId(0)).unwrap();
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        let sid = app.session_sid(local).unwrap();
        let (host, kept) = fixture();
        {
            let mut map = host.shared.kept.lock().unwrap();
            let entry = map.remove(SID).unwrap();
            map.insert(sid.clone(), entry);
        }
        {
            let mut kept = kept.lock().unwrap();
            kept.snapshot.as_mut().unwrap().tab = sid.clone();
            let bound = kept.account.progress.as_mut().unwrap();
            bound.snapshot.tab = sid.clone();
            bound.value.phase = ProgressPhase::Waiting;
            bound.value.known = Some(2);
            bound.value.eligible = Some(0);
            bound.value.wall = Some("weekly allowance".into());
        }
        let expected = host
            .account_progress(&sid, 4242, Some(BORN), Some(CONVERSATION))
            .unwrap()
            .unwrap();
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(4242);
            timeline.set_program(4242, Some(program.into()));
            timeline.set_agent_reader_program_for_test(reader);
            timeline.set_claude_footer(
                4242,
                Some(BORN),
                Some(FooterFacts {
                    owner: Some(FactsOwner {
                        floor: Some(BORN),
                        session_id: CONVERSATION.into(),
                    }),
                    ..FooterFacts::default()
                }),
            );
        }
        app.harness = Some(host);
        app.service_harness_account_progress(Instant::now());
        let timeline = ctx.timeline.lock().unwrap();
        let observed = timeline.claude_footer().unwrap().account_progress.as_ref();
        assert_eq!(
            observed,
            accepted.then_some(&expected),
            "{program} with reader {reader:?}: only the current Claude owner receives its bound observation"
        );
    }
}
