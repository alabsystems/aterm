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
        value,
    });
    let kept = Arc::new(Mutex::new(kept));
    let shared = Shared {
        state: Mutex::new(State::default()),
        headless: true,
        desk: crate::desk::Desk::of(true),
        faults: Mutex::default(),
        host_faults: Mutex::default(),
        kept: Mutex::new(HashMap::from([(SID.to_owned(), kept.clone())])),
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
    (
        HostHandle {
            shared: Arc::new(shared),
            thread: Arc::default(),
            hooks: hooks(),
        },
        kept,
    )
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
