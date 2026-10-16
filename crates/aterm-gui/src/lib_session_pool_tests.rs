// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A minimal `Session` for pool tests: a real `Terminal` and `SessionCtx` with
/// `master = -1`, so `Session::drop` is a no-op.
fn test_session(id: u64) -> Session {
    let self_id = SessionId::generate();
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let ctx = Arc::new(SessionCtx {
        sink: Arc::new(SinkWriter::new(-1)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: mode_mirror_of(&term),
        ui_waiting: Arc::default(),
        edges: std::sync::Mutex::new(EdgeTable::new()),
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
        turns: Arc::new(std::sync::Mutex::new(
            crate::turn_ledger::TurnLedger::default(),
        )),
        meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
        timeline: Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::default(),
        )),
        fabric: std::sync::Arc::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        generation_look: Default::default(),
        reset_lane: Default::default(),
        update_parked: Default::default(),
        fenced_writes: Default::default(),
    });
    Session {
        child_reaped: std::sync::atomic::AtomicBool::new(false),
        child_exit: std::sync::OnceLock::new(),
        exit_watch: None,
        shell_start: None,
        pty_closed_at: Arc::default(),
        id,
        term,
        vi_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        adopt_pulse_hold: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        master: -1,
        pid: -1,
        shell_identity: aterm_pty::ShellIdentity::unproven(-1),
        handoff_local_id: None,
        frozen_path: false,
        rekey_channel: false,
        body_loader: false,
        integration_frozen: false,
        identity: None,
        ctx,
        child_proxy: None,
        output_wake_pending: Arc::new(AtomicU64::new(0)),
        last_output_ns: Arc::new(AtomicU64::new(0)),
        output_request_booked_ns: AtomicU64::new(0),
        latest_output_activity_ns: Arc::new(AtomicU64::new(0)),
        reader_join: None,
        wake_wr: -1,
        reader_stop: Arc::new(AtomicBool::new(false)),
        temporal_writer_join: None,
        reflow_cancel: Arc::new(AtomicBool::new(false)),
        fg_holder: Arc::new(std::sync::atomic::AtomicI32::new(0)),
        handoff_history: None,
        _pty_census: crate::metrics::PtyCensus::none(),
        handoff_commit: None,
        journal_key: None,
    }
}

/// Dropping the last pool view drops the `Session`, whose `Drop` raises `reflow_cancel`
/// for an in-flight `aterm-reflow` worker (`drive_reflow_job`). Every mid-run close path
/// funnels through this.
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

#[test]
fn insert_starts_at_one_view_and_is_resolvable() {
    let mut pool = SessionPool::default();
    assert_eq!(pool.insert(test_session(7)), 7);
    assert!(pool.get(7).is_some(), "inserted session resolves by id");
    assert_eq!(pool.iter().count(), 1);
    // An unknown id is fail-closed None (mirrors the registry discipline).
    assert!(pool.get(999).is_none());
}

#[test]
fn detach_drops_session_exactly_when_last_view_leaves() {
    let mut pool = SessionPool::default();
    pool.insert(test_session(1));
    // A second window now views the one session (views = 2).
    pool.attach(1);
    // First detach: a view remains, so the session is not dropped — the
    // precondition for same-session-in-two-windows / detach with zero PTY churn.
    assert!(
        !pool.detach(1),
        "detach with a remaining view must not drop"
    );
    assert!(
        pool.get(1).is_some(),
        "session still owned while a view remains"
    );
    // Last detach: refcount hits 0, the session is dropped (its PTY would close).
    assert!(pool.detach(1), "the last detach drops the session");
    assert!(
        pool.get(1).is_none(),
        "a dropped session no longer resolves"
    );
    // Detaching an already-gone id is a fail-closed no-op, not a panic.
    assert!(!pool.detach(1));
    assert_eq!(pool.iter().count(), 0);
}

/// Tier-1 conformance of the real session lifecycle to `proxy_registry_model`
/// (`ProxyRegistry`): the proxy table never holds more child capabilities than live
/// sessions. `Spawn` is `spawn::provision_child_proxy` (scratch socket dir) plus the pool
/// taking a session holding its `ChildProxy` (stored by hand, since `spawn_session` needs
/// an event loop); `Close` is the last detach. Every step of a fill-and-drain script must
/// be a model step, and a closed child's 0600 token file must be gone.
///
/// Negative control: `Buggy = 1` (a `Drop` that forgets to deregister) rejects the real
/// `Close` steps.
#[test]
fn session_close_conforms_to_proxy_registry_no_leak() {
    use aterm_spec::verify::validate_transition_tiered;
    let model = aterm_spec::derive::proxy_registry_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let dir = std::env::temp_dir().join(format!("aterm-s1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch socket dir");
    let mut pool = SessionPool::default();
    // (pool id, child sid, edge-token file) for every child provisioned.
    let mut children: Vec<(u64, SessionId, std::path::PathBuf)> = Vec::new();
    let project = |pool: &SessionPool, children: &[(u64, SessionId, std::path::PathBuf)]| {
        let table = crate::proxy::proxies();
        let table = table.read().unwrap_or_else(|p| p.into_inner());
        std::collections::BTreeMap::from([
            (
                "live",
                children
                    .iter()
                    .filter(|(id, ..)| pool.get(*id).is_some())
                    .count() as i64,
            ),
            (
                "registered",
                children
                    .iter()
                    .filter(|(_, sid, _)| table.contains_key(sid))
                    .count() as i64,
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
                children.push((step, child.sid().clone(), edge_file));
                let mut session = test_session(step);
                session.child_proxy = Some(child);
                pool.insert(session);
            }
            _ => {
                assert!(pool.detach(step), "the last view drops session {step}");
                let (_, _, edge_file) = children
                    .iter()
                    .find(|(id, ..)| *id == step)
                    .expect("a spawned session");
                assert!(
                    !edge_file.exists(),
                    "session {step}'s edge-token file outlived it"
                );
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
        "the forgot-to-deregister mutant must part from every real close"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
