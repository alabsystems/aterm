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
#[path = "connections_tests.rs"]
mod tests;
