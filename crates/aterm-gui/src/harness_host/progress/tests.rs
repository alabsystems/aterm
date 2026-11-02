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
    use aterm_agent::harness::accounts_sync::LoginActivity;
    let mut value = AccountProgress::unknown(100);
    value.phase = ProgressPhase::Waiting;
    value.details.push("existing blocker".into());
    let labels = BTreeSet::from(["current".into()]);
    let activity = |label: &str, at| LoginActivity {
        label: label.into(),
        route: None,
        active: false,
        observed_at: at,
        next_at: Some(150),
    };
    apply_login_activity(
        &mut value,
        &labels,
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
        let painted = crate::autopilot_menu::paint(rect, &rows, None, theme);
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
fn recent_login_activity_does_not_replace_an_account_move() {
    use aterm_agent::harness::accounts_sync::LoginActivity;
    for phase in [
        ProgressPhase::Switching { to: "next".into() },
        ProgressPhase::Verifying { to: "next".into() },
    ] {
        let mut value = AccountProgress::unknown(100);
        value.phase = phase.clone();
        apply_login_activity(
            &mut value,
            &BTreeSet::from(["current".into()]),
            vec![LoginActivity {
                label: "current".into(),
                route: Some("profile".into()),
                active: true,
                observed_at: 90,
                next_at: None,
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
    use aterm_agent::harness::accounts_sync::LoginActivity;
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};

    for discard_contention in [false, true] {
        let start = Instant::now();
        let until = start + Duration::from_secs(100);
        let mut app = crate::App::headless_for_test();
        let local = app.focused_session_id(crate::WindowId(0)).unwrap();
        let ctx = app.pool.get(local).unwrap().ctx.clone();
        let sid = app.session_sid(local).unwrap();
        let (host, kept) = fixture();
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
                                    route: None,
                                    active: true,
                                    observed_at: stamp,
                                    next_at: None,
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
