// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_session::sink::SinkWriter;
use aterm_session::{EdgeDecision, decide_edge};

use crate::session_store::{SessionHandle, SessionState};

fn ids() -> (SessionId, SessionId, LaunchNonce) {
    (
        SessionId::new("s-a"),
        SessionId::new("s-b"),
        LaunchNonce::from_bytes([7u8; 16]),
    )
}

/// A minimal registrable handle (the `session_store` test idiom): dead fds, fresh identity, empty edge table.
fn handle(local_id: u64) -> SessionHandle {
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let observed = crate::session_watch::Observed::default();
    let ctx = Arc::new(crate::SessionCtx {
        sink: Arc::new(SinkWriter::new(-1)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: Arc::default(),
        ui_waiting: Arc::default(),
        edges: std::sync::Arc::new(Mutex::new(EdgeTable::new())),
        self_id: sid.clone(),
        nonce,
        turn_lease: Mutex::new(None),
        cast: Arc::new(Mutex::new(crate::cast::CastRecorder::new(80, 24))),
        temporal: Arc::new(Mutex::new(crate::temporal::TemporalRecorder::new())),
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
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        witness: Default::default(),
        fenced_writes: Default::default(),
        native_policy: Default::default(),
        spawn_mark: Default::default(),
    });
    SessionHandle {
        sid,
        nonce,
        local_id,
        parent: None,
        state: SessionState::Alive,
        title: format!("tab-{local_id}"),
        term: Arc::new(Mutex::new(aterm_core::terminal::Terminal::new(24, 80))),
        master: -1,
        shell_identity: aterm_pty::ShellIdentity::unproven(0),
        ctx,
    }
}

/// Fail-soft grouping: a carried op set re-mints the largest kind it fully proves and drops the rest, never widening.
#[test]
fn carried_kind_never_widens_and_drops_the_rest() {
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        carried_kind(&s(&["read-screen"])),
        (Some(ConnectionKind::Pull), vec![])
    );
    assert_eq!(
        carried_kind(&s(&["signal", "write-input"])),
        (Some(ConnectionKind::Push), vec![])
    );
    assert_eq!(
        carried_kind(&s(&["read-screen", "signal", "write-input"])),
        (Some(ConnectionKind::Both), vec![])
    );
    assert_eq!(
        carried_kind(&s(&["write-input"])),
        (None, s(&["write-input"]))
    );
    assert_eq!(
        carried_kind(&s(&["read-screen", "signal"])),
        (Some(ConnectionKind::Pull), s(&["signal"]))
    );
    assert_eq!(
        carried_kind(&s(&["config-write", "read-screen", "not-an-op"])),
        (
            Some(ConnectionKind::Pull),
            s(&["config-write", "not-an-op"])
        )
    );
    assert_eq!(carried_kind(&[]), (None, vec![]));
}

#[test]
fn connect_disconnect_round_trip_on_a_synthetic_table() {
    let conn = new_connection_store();
    let (a, b, nonce) = ids();
    let dst_edges = Mutex::new(EdgeTable::new());

    assert!(connect_in(
        &conn,
        &a,
        &b,
        &dst_edges,
        &nonce,
        ConnectionKind::Push,
        "test"
    ));
    let minted = {
        let records = conn.records();
        let rec = records
            .get(&(a.clone(), b.clone()))
            .expect("connect records the pair");
        assert_eq!(rec.kind(), Some(ConnectionKind::Push));
        rec.tokens.clone()
    };
    assert_eq!(minted.len(), 2);
    for (op, tok) in &minted {
        assert!(decide_edge(&dst_edges.lock().unwrap(), tok, &b, *op, &nonce).is_permitted());
    }

    assert!(disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test").is_some());
    assert!(conn.records().is_empty(), "record gone");
    assert!(dst_edges.lock().unwrap().is_empty(), "rows gone");
    for (op, tok) in &minted {
        assert_eq!(
            decide_edge(&dst_edges.lock().unwrap(), tok, &b, *op, &nonce),
            EdgeDecision::Deny
        );
    }
    assert!(disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test").is_none());
}

#[test]
fn connect_refuses_a_self_loop_with_no_residue() {
    let conn = new_connection_store();
    let (a, _b, nonce) = ids();
    let dst_edges = Mutex::new(EdgeTable::new());
    for kind in [
        ConnectionKind::Pull,
        ConnectionKind::Push,
        ConnectionKind::Both,
    ] {
        assert!(
            !connect_in(&conn, &a, &a, &dst_edges, &nonce, kind, "test"),
            "{kind:?} self-loop must be refused"
        );
    }
    assert!(conn.records().is_empty(), "no record behind a refusal");
    assert!(
        dst_edges.lock().unwrap().is_empty(),
        "no row behind a refusal"
    );
}

#[test]
fn reconnect_is_idempotent_on_same_kind_and_atomic_on_rekind() {
    let conn = new_connection_store();
    let (a, b, nonce) = ids();
    let dst_edges = Mutex::new(EdgeTable::new());

    assert!(connect_in(
        &conn,
        &a,
        &b,
        &dst_edges,
        &nonce,
        ConnectionKind::Pull,
        "test"
    ));
    let (pull_op, pull_tok) = conn.records()[&(a.clone(), b.clone())].tokens[0];

    assert!(connect_in(
        &conn,
        &a,
        &b,
        &dst_edges,
        &nonce,
        ConnectionKind::Pull,
        "test"
    ));
    assert_eq!(dst_edges.lock().unwrap().len(), 1);
    assert!(
        decide_edge(&dst_edges.lock().unwrap(), &pull_tok, &b, pull_op, &nonce).is_permitted(),
        "idempotent re-connect must not rotate the token"
    );

    assert!(connect_in(
        &conn,
        &a,
        &b,
        &dst_edges,
        &nonce,
        ConnectionKind::Both,
        "test"
    ));
    assert_eq!(
        decide_edge(&dst_edges.lock().unwrap(), &pull_tok, &b, pull_op, &nonce),
        EdgeDecision::Deny,
        "the replaced kind's token must die in the transition"
    );
    let records = conn.records();
    assert_eq!(records.len(), 1);
    let rec = &records[&(a.clone(), b.clone())];
    assert_eq!(rec.kind(), Some(ConnectionKind::Both));
    assert_eq!(dst_edges.lock().unwrap().len(), 3);
    for (op, tok) in &rec.tokens {
        assert!(decide_edge(&dst_edges.lock().unwrap(), tok, &b, *op, &nonce).is_permitted());
    }
}

#[test]
fn disconnect_kind_filters_a_recorded_connection_pair_precisely() {
    let conn = new_connection_store();
    let (a, b, nonce) = ids();
    let dst_edges = Mutex::new(EdgeTable::new());
    assert!(connect_in(
        &conn,
        &a,
        &b,
        &dst_edges,
        &nonce,
        ConnectionKind::Both,
        "test"
    ));

    assert_eq!(
        disconnect_kind_in(
            &conn,
            &a,
            &b,
            &dst_edges,
            Some(ConnectionKind::Pull),
            "test"
        ),
        Some(1)
    );
    let ops: Vec<Op> = dst_edges
        .lock()
        .unwrap()
        .edges()
        .iter()
        .map(|e| e.op)
        .collect();
    assert_eq!(ops.len(), 2);
    assert!(!ops.contains(&Op::ReadScreen), "pull half gone: {ops:?}");
    assert!(ops.contains(&Op::WriteInput) && ops.contains(&Op::Signal));
    assert_eq!(
        conn.records()[&(a.clone(), b.clone())].kind(),
        Some(ConnectionKind::Push),
        "the record keeps its surviving ops"
    );

    assert_eq!(
        disconnect_kind_in(
            &conn,
            &a,
            &b,
            &dst_edges,
            Some(ConnectionKind::Pull),
            "test"
        ),
        Some(0)
    );

    assert_eq!(
        disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test"),
        Some(2)
    );
    assert!(conn.records().is_empty());
    assert!(dst_edges.lock().unwrap().is_empty());
    assert_eq!(
        disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test"),
        None
    );
}

#[test]
fn disconnect_kind_sweeps_unrecorded_wire_grants_op_filtered() {
    let conn = new_connection_store();
    let (a, b, nonce) = ids();
    let dst_edges = Mutex::new(EdgeTable::new());
    {
        let mut edges = dst_edges.lock().unwrap();
        let _ = edges.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
        let _ = edges.grant(a.clone(), b.clone(), Op::WriteInput, nonce);
        let _ = edges.grant(a.clone(), b.clone(), Op::Signal, nonce);
    }

    assert_eq!(
        disconnect_kind_in(
            &conn,
            &a,
            &b,
            &dst_edges,
            Some(ConnectionKind::Push),
            "test"
        ),
        Some(2)
    );
    let ops: Vec<Op> = dst_edges
        .lock()
        .unwrap()
        .edges()
        .iter()
        .map(|e| e.op)
        .collect();
    assert_eq!(ops, vec![Op::ReadScreen], "only pull remains: {ops:?}");

    assert_eq!(
        disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test"),
        Some(1)
    );
    assert_eq!(
        disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test"),
        None
    );
}

#[test]
fn roles_computes_the_mark_predicate_per_scenario() {
    let (a, b, nonce) = ids();

    let (ta, tb) = (EdgeTable::new(), EdgeTable::new());
    let r = roles([(&a, &ta), (&b, &tb)]);
    assert_eq!(r.len(), 2);
    assert_eq!(r[&a], SessionRoles::default());
    assert_eq!(r[&b], SessionRoles::default());

    let ta = EdgeTable::new();
    let mut tb = EdgeTable::new();
    let _ = tb.grant_connection(&a, &b, ConnectionKind::Pull, &nonce);
    let r = roles([(&a, &ta), (&b, &tb)]);
    assert_eq!(
        r[&a],
        SessionRoles {
            inbound: false,
            outbound: true
        }
    );
    assert_eq!(
        r[&b],
        SessionRoles {
            inbound: true,
            outbound: false
        }
    );

    let mut ta = EdgeTable::new();
    let mut tb = EdgeTable::new();
    let _ = tb.grant_connection(&a, &b, ConnectionKind::Push, &nonce);
    let _ = ta.grant_connection(&b, &a, ConnectionKind::Push, &nonce);
    let r = roles([(&a, &ta), (&b, &tb)]);
    for sid in [&a, &b] {
        assert_eq!(
            r[sid],
            SessionRoles {
                inbound: true,
                outbound: true
            }
        );
    }

    let mut ta = EdgeTable::new();
    let _ = ta.grant(a.clone(), a.clone(), Op::WriteInput, nonce);
    let r = roles([(&a, &ta)]);
    assert_eq!(r[&a], SessionRoles::default());

    let x = SessionId::new("s-foreign");
    let mut tb = EdgeTable::new();
    let _ = tb.grant(x.clone(), b.clone(), Op::WriteInput, nonce);
    let r = roles([(&b, &tb)]);
    assert_eq!(
        r[&b],
        SessionRoles {
            inbound: true,
            outbound: false
        }
    );
    assert_eq!(
        r[&x],
        SessionRoles {
            inbound: false,
            outbound: true
        }
    );
}

#[test]
fn close_time_sweep_dissolves_records_and_survivor_rows() {
    let conn = new_connection_store();
    let store = crate::session_store::new_store();
    let (ha, hb) = (handle(1), handle(2));
    let (a, b) = (ha.sid.clone(), hb.sid.clone());
    let (actx, bctx) = (ha.ctx.clone(), hb.ctx.clone());
    {
        let mut s = store.write().unwrap();
        s.register(ha);
        s.register(hb);
    }

    assert!(connect_in(
        &conn,
        &a,
        &b,
        &bctx.edges,
        &bctx.nonce,
        ConnectionKind::Push,
        "test"
    ));
    assert!(connect_in(
        &conn,
        &b,
        &a,
        &actx.edges,
        &actx.nonce,
        ConnectionKind::Pull,
        "test"
    ));
    let push_tokens = conn.records()[&(a.clone(), b.clone())].tokens.clone();

    assert_eq!(store.write().unwrap().deregister_local(1), Some(a.clone()));
    sweep_session_closed_in(&conn, &a, &store);

    assert!(conn.records().is_empty());
    assert!(bctx.edges.lock().unwrap().is_empty());
    for (op, tok) in &push_tokens {
        assert_eq!(
            decide_edge(&bctx.edges.lock().unwrap(), tok, &b, *op, &bctx.nonce),
            EdgeDecision::Deny
        );
    }
    let r = roles_by_local(&store);
    assert_eq!(r[&2], SessionRoles::default());
    assert!(!r.contains_key(&1));
    sweep_session_closed_in(&conn, &a, &store);
    assert!(conn.records().is_empty());
}
