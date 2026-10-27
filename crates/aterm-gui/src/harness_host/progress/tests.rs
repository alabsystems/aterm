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
