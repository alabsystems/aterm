// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-local connection records, connect/disconnect acts, and close-time source sweep ([`EdgeTable`], [`ConnectionRecord`], [`crate::proxy`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use aterm_session::{ConnectionKind, EdgeTable, EdgeToken, LaunchNonce, Op, SessionId};

use crate::session_edge_audit::{self, EdgeAction};
use crate::session_store::Store;

/// One live connection `src → dst` and the edge rows it minted into the destination's table.
#[derive(Clone, Debug)]
pub(crate) struct ConnectionRecord {
    pub tokens: Vec<(Op, EdgeToken)>,
}

impl ConnectionRecord {
    /// The [`ConnectionKind`] this record's op set spells.
    #[must_use]
    pub(crate) fn kind(&self) -> Option<ConnectionKind> {
        let ops: Vec<Op> = self.tokens.iter().map(|(op, _)| *op).collect();
        [
            ConnectionKind::Pull,
            ConnectionKind::Push,
            ConnectionKind::Both,
        ]
        .into_iter()
        .find(|kind| {
            ops.len() == kind.ops().len()
                && kind.ops().iter().all(|op| ops.contains(op))
                && ops.iter().all(|op| kind.ops().contains(op))
        })
    }
}

/// The process's record of live connections.
#[derive(Default)]
pub(crate) struct ConnectionTable {
    records: Mutex<HashMap<(SessionId, SessionId), ConnectionRecord>>,
    /// Monotonic revision, bumped by every connection-authority change this table can observe.
    revision: AtomicU64,
}

impl ConnectionTable {
    /// The record map, poison-recovered.
    pub(crate) fn records(
        &self,
    ) -> MutexGuard<'_, HashMap<(SessionId, SessionId), ConnectionRecord>> {
        self.records.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The current freshness epoch.
    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// Bump the freshness epoch after an authority change.
    pub(crate) fn bump_revision(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }
}

/// The shared handle to one [`ConnectionTable`].
pub(crate) type ConnectionStore = Arc<ConnectionTable>;

/// What a connected spawn's newborn is to its origin session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConnectedSpawnKind {
    Controlled,
    Controller,
}

/// Where a connected spawn places the newborn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConnectedSpawnPlace {
    Window,
    Tab,
}

/// The [`ConnectionKind`] a carried seamless-handoff op set spells.
pub(crate) fn carried_kind(ops: &[String]) -> (Option<ConnectionKind>, Vec<String>) {
    let has = |op: Op| ops.iter().any(|s| s == op.as_str());
    let pull = has(Op::ReadScreen);
    let push = has(Op::WriteInput) && has(Op::Signal);
    let kind = match (pull, push) {
        (true, true) => Some(ConnectionKind::Both),
        (true, false) => Some(ConnectionKind::Pull),
        (false, true) => Some(ConnectionKind::Push),
        (false, false) => None,
    };
    let covered: &[Op] = kind.map_or(&[], |k| k.ops());
    let dropped = ops
        .iter()
        .filter(|s| !covered.iter().any(|c| c.as_str() == s.as_str()))
        .cloned()
        .collect();
    (kind, dropped)
}

/// A fresh, empty connection store (tests run against a private one).
#[must_use]
pub(crate) fn new_connection_store() -> ConnectionStore {
    Arc::new(ConnectionTable::default())
}

/// The process-wide connection store, one per process (the `PROXIES` idiom in `proxy.rs`): a singleton saves threading a handle through every UI/control caller, and a process has one connection fabric.
static CONNECTIONS: OnceLock<ConnectionStore> = OnceLock::new();

/// The process-wide [`ConnectionStore`] (lazily initialized, cloned Arc).
#[must_use]
pub(crate) fn connections() -> ConnectionStore {
    CONNECTIONS.get_or_init(new_connection_store).clone()
}

/// Establish the connection `src → dst` of `kind`.
pub(crate) fn connect_in(
    conn: &ConnectionStore,
    src: &SessionId,
    dst: &SessionId,
    dst_edges: &Mutex<EdgeTable>,
    dst_nonce: &LaunchNonce,
    kind: ConnectionKind,
    origin: &str,
) -> bool {
    if src == dst {
        return false;
    }
    let key = (src.clone(), dst.clone());
    let mut records = conn.records();
    if records.get(&key).is_some_and(|r| r.kind() == Some(kind)) {
        return true;
    }
    let old = records.remove(&key);
    let (revoked, minted) = {
        let mut edges = dst_edges.lock().unwrap_or_else(|p| p.into_inner());
        let mut revoked = Vec::new();
        if let Some(old) = &old {
            for (op, tok) in &old.tokens {
                if edges.revoke(tok) {
                    revoked.push(*op);
                }
            }
        }
        let minted = edges.grant_connection(src, dst, kind, dst_nonce);
        (revoked, minted)
    };
    records.insert(
        key,
        ConnectionRecord {
            tokens: minted.clone(),
        },
    );
    drop(records);
    conn.bump_revision();
    for op in revoked {
        session_edge_audit::emit(EdgeAction::Revoke, origin, src, dst, op.as_str());
    }
    for (op, _) in &minted {
        session_edge_audit::emit(EdgeAction::Grant, origin, src, dst, op.as_str());
    }
    true
}

/// Dissolve the connection `src → dst`, filtered to `kind`.
pub(crate) fn disconnect_kind_in(
    conn: &ConnectionStore,
    src: &SessionId,
    dst: &SessionId,
    dst_edges: &Mutex<EdgeTable>,
    kind: Option<ConnectionKind>,
    origin: &str,
) -> Option<usize> {
    let key = (src.clone(), dst.clone());
    let filter = kind.map(|k| k.ops());
    let mut records = conn.records();
    if let Some(rec) = records.remove(&key) {
        let (revoke_toks, keep_toks): (Vec<_>, Vec<_>) = rec
            .tokens
            .into_iter()
            .partition(|(op, _)| filter.is_none_or(|ops| ops.contains(op)));
        let revoked: Vec<Op> = {
            let mut edges = dst_edges.lock().unwrap_or_else(|p| p.into_inner());
            revoke_toks
                .iter()
                .filter(|(_, tok)| edges.revoke(tok))
                .map(|(op, _)| *op)
                .collect()
        };
        if !keep_toks.is_empty() {
            records.insert(key, ConnectionRecord { tokens: keep_toks });
        }
        drop(records);
        conn.bump_revision();
        let n = revoked.len();
        for op in revoked {
            session_edge_audit::emit(EdgeAction::Revoke, origin, src, dst, op.as_str());
        }
        return Some(n);
    }
    let removed: Vec<(&'static str, usize)> = match filter {
        None => {
            let n = {
                let mut edges = dst_edges.lock().unwrap_or_else(|p| p.into_inner());
                edges.revoke_src(src)
            };
            vec![("*", n)]
        }
        Some(ops) => {
            let mut edges = dst_edges.lock().unwrap_or_else(|p| p.into_inner());
            ops.iter()
                .map(|op| (op.as_str(), edges.revoke_src_ops(src, Some(&[*op]))))
                .collect()
        }
    };
    drop(records);
    let total: usize = removed.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return None;
    }
    conn.bump_revision();
    for (op, n) in removed {
        if n > 0 {
            session_edge_audit::emit(EdgeAction::RevokeSrc, origin, src, dst, op);
        }
    }
    Some(total)
}

/// Every live edge row across every registered session's table.
pub(crate) fn all_edges(sessions: &Store) -> Vec<aterm_session::Edge> {
    let handles = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot();
    let mut out: Vec<aterm_session::Edge> = Vec::new();
    for h in &handles {
        let rows = {
            let edges = h.ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            edges.edges()
        };
        out.extend(rows);
    }
    out.sort_by(|a, b| {
        (a.src.as_str(), a.dst.as_str(), a.op.as_str()).cmp(&(
            b.src.as_str(),
            b.dst.as_str(),
            b.op.as_str(),
        ))
    });
    out
}

/// One session's connection roles, for the mark.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) struct SessionRoles {
    pub inbound: bool,
    pub outbound: bool,
}

/// The role predicate over the live edge tables, and only the tables.
#[cfg(test)]
pub(crate) fn roles<'a>(
    tables: impl IntoIterator<Item = (&'a SessionId, &'a EdgeTable)>,
) -> HashMap<SessionId, SessionRoles> {
    let mut out: HashMap<SessionId, SessionRoles> = HashMap::new();
    for (owner, table) in tables {
        out.entry(owner.clone()).or_default();
        fold_roles(&mut out, table.edges());
    }
    out
}

/// Fold one table's rows into the role map: the role predicate per row, shared by every entry point so they cannot drift.
fn fold_roles(out: &mut HashMap<SessionId, SessionRoles>, rows: Vec<aterm_session::Edge>) {
    for edge in rows {
        if edge.src == edge.dst {
            continue;
        }
        out.entry(edge.dst).or_default().inbound = true;
        out.entry(edge.src).or_default().outbound = true;
    }
}

/// The role fold over a registry snapshot: every owner an entry, every table's rows folded.
fn fold_registry(
    handles: &[crate::session_store::SessionHandle],
) -> HashMap<SessionId, SessionRoles> {
    let mut out: HashMap<SessionId, SessionRoles> = HashMap::new();
    for h in handles {
        let rows = {
            let edges = h.ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            edges.edges()
        };
        out.entry(h.sid.clone()).or_default();
        fold_roles(&mut out, rows);
    }
    out
}

/// The role fold over every registered session's table.
pub(crate) fn roles_by_local(sessions: &Store) -> HashMap<u64, SessionRoles> {
    let handles = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot();
    let by_sid = fold_registry(&handles);
    handles
        .iter()
        .filter_map(|h| by_sid.get(&h.sid).map(|roles| (h.local_id, *roles)))
        .collect()
}

/// The close-time sweep, a security obligation rather than bookkeeping.
pub(crate) fn sweep_session_closed_in(
    conn: &ConnectionStore,
    closing: &SessionId,
    sessions: &Store,
) {
    conn.records()
        .retain(|(src, dst), _| src != closing && dst != closing);
    conn.bump_revision();
    let survivors: Vec<(SessionId, Arc<crate::SessionCtx>)> = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot()
        .into_iter()
        .map(|h| (h.sid, h.ctx))
        .collect();
    for (sid, ctx) in survivors {
        if sid == *closing {
            continue;
        }
        let removed = {
            let mut edges = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            edges.revoke_src(closing)
        };
        if removed > 0 {
            session_edge_audit::emit(EdgeAction::RevokeSrc, "close", closing, &sid, "*");
        }
    }
    let closed: Vec<(SessionId, Arc<Mutex<EdgeTable>>)> = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .closed_logs()
        .read_tables();
    for (sid, reads) in closed {
        let removed = {
            let mut edges = reads.lock().unwrap_or_else(|p| p.into_inner());
            edges.revoke_src(closing)
        };
        if removed > 0 {
            session_edge_audit::emit(EdgeAction::RevokeSrc, "close", closing, &sid, "*");
        }
    }
}

#[cfg(test)]
mod tests {
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
            meta: Mutex::new(crate::session_timeline::SessionMeta::default()),
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
            update_parked: Default::default(),
            watchers: observed.watchers,
            frame: Default::default(),
            witness: Default::default(),
            fenced_writes: Default::default(),
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
}
