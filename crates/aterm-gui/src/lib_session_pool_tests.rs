// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn test_session(id: u64) -> Session {
    let self_id = SessionId::generate();
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let observed = crate::session_watch::Observed::default();
    let ctx = Arc::new(SessionCtx {
        sink: Arc::new(SinkWriter::new(-1)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: mode_mirror_of(&term),
        ui_waiting: Arc::default(),
        edges: std::sync::Arc::new(std::sync::Mutex::new(EdgeTable::new())),
        turn_lease: std::sync::Mutex::new(None),
        self_id,
        nonce: LaunchNonce::generate(),
        cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
            80, 24,
        ))),
        temporal: Arc::new(std::sync::Mutex::new(
            crate::temporal::TemporalRecorder::new(),
        )),
        byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
        turns: observed.turns,
        meta: Arc::default(),
        timeline: observed.timeline,
        fabric: std::sync::Arc::default(),
        idem: Default::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        licence: Default::default(),
        launch: Default::default(),
        reset_lane: Default::default(),
        crash_journal: Default::default(),
        typed: Default::default(),
        bells: Default::default(),
        screen_moves: Default::default(),
        job_feed: Default::default(),
        ran: Default::default(),
        box_banner: Default::default(),
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        witness: Default::default(),
        fenced_writes: Default::default(),
        native_policy: Default::default(),
        spawn_mark: Default::default(),
    });
    Session {
        child_reaped: std::sync::atomic::AtomicBool::new(false),
        teardown_started: std::sync::atomic::AtomicBool::new(false),
        child_exit: std::sync::OnceLock::new(),
        exit_watch: None,
        shell_start: None,
        pty_closed_at: Arc::default(),
        id,
        term,
        adopt_pulse_hold: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        master: -1,
        pid: -1,
        shell_identity: aterm_pty::ShellIdentity::unproven(-1),
        handoff_local_id: None,
        frozen_path: false,
        rekey_channel: false,
        body_loader: false,
        integration_frozen: false,
        ctx,
        child_proxy: None,
        output_wake_pending: Arc::new(AtomicU64::new(0)),
        last_output_ns: Arc::new(AtomicU64::new(0)),
        output_request_booked_ns: AtomicU64::new(0),
        latest_output_activity_ns: Arc::new(AtomicU64::new(0)),
        reader_join: None,
        wake_wr: -1,
        reader_stop: Arc::new(AtomicBool::new(false)),
        temporal_tx: None,
        temporal_drain: None,
        temporal_arm: Arc::default(),
        reflow_cancel: Arc::new(AtomicBool::new(false)),
        fg_holder: Arc::new(std::sync::atomic::AtomicI32::new(0)),
        handoff_history: None,
        _pty_census: crate::metrics::PtyCensus::none(),
        handoff_commit: None,
        journal_key: None,
    }
}

#[test]
fn dropping_last_view_raises_reflow_cancel() {
    let s = test_session(1);
    let cancel = s.reflow_cancel.clone();
    let mut pool = SessionPool::default();
    pool.insert(s);
    assert!(
        !cancel.load(std::sync::atomic::Ordering::Acquire),
        "flag must start clear (no spurious cancel of a live session's job)"
    );
    assert!(pool.detach(1), "last view detaches → Session dropped");
    assert!(
        cancel.load(std::sync::atomic::Ordering::Acquire),
        "Session::drop must signal an in-flight reflow worker to abandon its job"
    );
}

#[cfg(unix)]
#[test]
fn quit_then_drop_hangs_up_a_session_only_once() {
    if !crate::test_alone::alone() {
        return;
    }
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let fifo = scratch.path().join("release");
    let count = scratch.path().join("hups");
    let done = scratch.path().join("done");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: this test creates the FIFO and owns the resulting descriptor.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    let gate = unsafe { libc::open(c_fifo.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    assert!(gate >= 0);
    let gate = unsafe { OwnedFd::from_raw_fd(gate) };
    let quote =
        |path: &std::path::Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    let handler = format!(
        "printf H >> {}; read go < {}; printf done > {}; exit 0",
        quote(&count),
        quote(&fifo),
        quote(&done)
    );
    let handler = format!("'{}'", handler.replace('\'', "'\\''"));
    let script = format!("trap {handler} HUP; printf READY; while :; do read line; done");
    // SAFETY: the test owns its synthetic shell and trusted spawn capabilities.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let exec = vec![
        std::ffi::OsString::from("/bin/bash"),
        "-c".into(),
        script.into(),
    ];
    let child = aterm_pty::spawn_shell_with_pid(
        24,
        80,
        &cap,
        &sandbox,
        &[],
        None,
        None,
        None,
        Some(&exec),
        None,
        None,
        aterm_sandbox::Limits::inherit(),
    )
    .unwrap();
    aterm_pty::set_nonblocking(child.master, true).unwrap();
    let master = unsafe { OwnedFd::from_raw_fd(child.master) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut ready = Vec::new();
    while !ready.windows(5).any(|word| word == b"READY") && std::time::Instant::now() < deadline {
        let mut bytes = [0; 256];
        let n = aterm_pty::read(master.as_raw_fd(), &mut bytes);
        if n > 0 {
            ready.extend_from_slice(&bytes[..n as usize]);
        } else {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    assert!(ready.windows(5).any(|word| word == b"READY"));
    let mut session = test_session(7623);
    session.master = master.as_raw_fd();
    session.pid = child.pid;
    session.shell_identity = aterm_pty::record_spawned_shell(child.pid);
    let hold = aterm_pty::hold_reap_kills_for_test();
    session.hang_up_and_reap(false);
    while !count.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(count.exists(), "the first hang-up entered its handler");
    session.hang_up_and_reap(false);
    drop(session);
    drop(master);
    // SAFETY: the test retains the writable FIFO endpoint until the shell has been released.
    assert_eq!(
        unsafe { libc::write(gate.as_raw_fd(), b"go\n".as_ptr().cast(), 3) },
        3
    );
    while !done.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(done.exists(), "a second hang-up did not interrupt cleanup");
    assert_eq!(std::fs::read(&count).unwrap(), b"H");
    drop(hold);
    assert!(aterm_pty::wait_for_reaps(std::time::Duration::from_secs(
        60
    )));
}

#[test]
fn detach_drops_session_exactly_when_last_view_leaves() {
    let mut pool = SessionPool::default();
    pool.insert(test_session(1));
    pool.attach(1);
    assert!(
        !pool.detach(1),
        "detach with a remaining view must not drop"
    );
    assert!(
        pool.get(1).is_some(),
        "session still owned while a view remains"
    );
    assert!(pool.detach(1), "the last detach drops the session");
    assert!(
        pool.get(1).is_none(),
        "a dropped session no longer resolves"
    );
    assert!(!pool.detach(1));
    assert_eq!(pool.iter().count(), 0);
}

#[test]
fn session_close_conforms_to_proxy_registry_no_leak() {
    use aterm_spec::verify::validate_transition_tiered;
    let model = aterm_models_session::proxy_registry_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let dir = std::env::temp_dir().join(format!("aterm-s1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch socket dir");
    let mut pool = SessionPool::default();
    let mut children: Vec<(u64, std::path::PathBuf)> = Vec::new();
    let project = |pool: &SessionPool, children: &[(u64, std::path::PathBuf)]| {
        std::collections::BTreeMap::from([
            (
                "live",
                children
                    .iter()
                    .filter(|(id, _)| pool.get(*id).is_some())
                    .count() as i64,
            ),
            (
                "registered",
                children.iter().filter(|(_, file)| file.exists()).count() as i64,
            ),
        ])
    };
    let mut state = project(&pool, &children);
    assert_eq!(state, model.init_state());
    let mut buggy_rejected_closes = 0;
    for (step, action) in [
        (1, "Spawn"),
        (2, "Spawn"),
        (3, "Spawn"),
        (1, "Close"),
        (4, "Spawn"),
        (3, "Close"),
        (2, "Close"),
        (4, "Close"),
    ] {
        match action {
            "Spawn" => {
                let (env, child) = crate::spawn::provision_child_proxy(Some(&dir));
                let edge_file = env
                    .iter()
                    .find(|(key, _)| key == aterm_types::domain::ENV_EDGE_TOKENS)
                    .map(|(_, path)| std::path::PathBuf::from(path))
                    .expect("the file channel with a private dir");
                children.push((step, edge_file));
                let mut session = test_session(step);
                session.child_proxy = Some(child);
                pool.insert(session);
            }
            _ => {
                assert!(pool.detach(step), "the last view drops session {step}");
            }
        }
        let next = project(&pool, &children);
        let (ok, why) = validate_transition_tiered(
            &model,
            &[],
            &state,
            &next,
            Some(action),
            "ProxyRegistry(Session)",
        );
        assert!(
            ok,
            "{action} {step}: the real registry moved {state:?} -> {next:?}, which the \
             model does not admit — {why}"
        );
        assert!(model.check_invariant("NoRegistryLeak", &next));
        if action == "Close" {
            let (admitted, _) = validate_transition_tiered(
                &buggy,
                &[],
                &state,
                &next,
                Some(action),
                "ProxyRegistry(Buggy=1)",
            );
            buggy_rejected_closes += usize::from(!admitted);
        }
        state = next;
    }
    assert_eq!(
        buggy_rejected_closes, 4,
        "the forgot-to-remove mutant must part from every real close"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
