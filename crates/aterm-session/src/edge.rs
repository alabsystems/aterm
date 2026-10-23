// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-edge, operation-scoped, fail-closed session authority table.

use std::collections::HashMap;

use crate::id::{LaunchNonce, SessionId};
use crate::{from_hex, hex};

/// Operation authorized by a session [`Edge`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Op {
    /// Read the rendered terminal surface.
    ReadScreen,
    /// Inject input events (keys, mouse, paste, resize, focus).
    WriteInput,
    /// Send an out-of-band signal to the destination's foreground process group.
    Signal,
    /// Modify the destination's durable configuration.
    ConfigWrite,
    /// Copy the destination's selection to the system clipboard.
    ClipboardWrite,
    /// Feed bytes derived from reading an untrusted node back as input.
    DeriveLoop,
}

impl Op {
    /// Returns the wire token for this operation.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Op::ReadScreen => "read-screen",
            Op::WriteInput => "write-input",
            Op::Signal => "signal",
            Op::ConfigWrite => "config-write",
            Op::ClipboardWrite => "clipboard-write",
            Op::DeriveLoop => "derive-loop",
        }
    }

    /// Parses an [`Op`] from its wire token.
    #[must_use]
    #[inline]
    pub fn parse(s: &str) -> Option<Op> {
        match s {
            "read-screen" => Some(Op::ReadScreen),
            "write-input" => Some(Op::WriteInput),
            "signal" => Some(Op::Signal),
            "config-write" => Some(Op::ConfigWrite),
            "clipboard-write" => Some(Op::ClipboardWrite),
            "derive-loop" => Some(Op::DeriveLoop),
            _ => None,
        }
    }

    /// Returns whether `self` is part of the standard human-seat vocabulary.
    #[must_use]
    #[inline]
    pub const fn is_human_seat_op(self) -> bool {
        matches!(self, Op::ReadScreen | Op::WriteInput | Op::Signal)
    }

    /// Returns whether `self` is an elevated authority not minted by [`ConnectionKind`].
    #[must_use]
    #[inline]
    pub const fn is_elevated_op(self) -> bool {
        matches!(self, Op::ConfigWrite | Op::ClipboardWrite | Op::DeriveLoop)
    }

    /// Returns whether `self` mutates or signals the destination session.
    #[must_use]
    #[inline]
    pub const fn mutates_destination(self) -> bool {
        !matches!(self, Op::ReadScreen)
    }
}

/// High-level connection direction minting standard human-seat [`Op`]s.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ConnectionKind {
    /// Read the destination's rendered surface (`ReadScreen`).
    Pull,
    /// Inject input and signal the destination (`WriteInput` and `Signal`).
    Push,
    /// Both `Pull` and `Push` authority.
    Both,
}

impl ConnectionKind {
    /// Returns the slice of [`Op`]s granted by this connection kind.
    #[must_use]
    #[inline]
    pub const fn ops(&self) -> &'static [Op] {
        match self {
            ConnectionKind::Pull => &[Op::ReadScreen],
            ConnectionKind::Push => &[Op::WriteInput, Op::Signal],
            ConnectionKind::Both => &[Op::ReadScreen, Op::WriteInput, Op::Signal],
        }
    }

    /// Returns the number of [`Op`]s granted by this connection kind.
    #[must_use]
    #[inline]
    pub const fn op_count(self) -> usize {
        match self {
            ConnectionKind::Pull => 1,
            ConnectionKind::Push => 2,
            ConnectionKind::Both => 3,
        }
    }

    /// Returns whether this connection kind includes `ReadScreen`.
    #[must_use]
    #[inline]
    pub const fn includes_pull(self) -> bool {
        matches!(self, ConnectionKind::Pull | ConnectionKind::Both)
    }

    /// Returns whether this connection kind includes `WriteInput` and `Signal`.
    #[must_use]
    #[inline]
    pub const fn includes_push(self) -> bool {
        matches!(self, ConnectionKind::Push | ConnectionKind::Both)
    }

    /// Returns whether this connection kind grants `op`.
    #[must_use]
    #[inline]
    pub const fn grants_op(self, op: Op) -> bool {
        match self {
            ConnectionKind::Pull => matches!(op, Op::ReadScreen),
            ConnectionKind::Push => matches!(op, Op::WriteInput | Op::Signal),
            ConnectionKind::Both => matches!(op, Op::ReadScreen | Op::WriteInput | Op::Signal),
        }
    }
}

/// Directed authority edge `src → dst` for a single [`Op`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Edge {
    pub src: SessionId,
    pub dst: SessionId,
    pub op: Op,
}

/// Unforgeable 32-byte per-edge bearer secret.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeToken([u8; 32]);

impl EdgeToken {
    /// Wraps raw 32-byte token bytes.
    #[must_use]
    #[inline]
    pub const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Returns the raw 32-byte token bytes.
    #[must_use]
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Mints a random token from the OS CSPRNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut b = [0u8; 32];
        crate::fill_random(&mut b);
        Self(b)
    }

    /// Encodes the token as 64 lowercase hex characters.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Parses a token from a 64-character hex string.
    #[must_use]
    pub fn from_hex(s: &str) -> Option<Self> {
        let mut b = [0u8; 32];
        from_hex(s, &mut b)?;
        Some(Self(b))
    }

    /// Compares two tokens in constant time.
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        crate::ct_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for EdgeToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret — only that it is one.
        f.write_str("EdgeToken(<redacted 32B>)")
    }
}

/// What a recorded edge token grants, on the destination side.
#[derive(Clone, Debug)]
struct EdgeEntry {
    src: SessionId,
    dst: SessionId,
    op: Op,
    nonce: LaunchNonce,
}

/// Destination-side table mapping each [`EdgeToken`] to its authorized `(src, dst, op, nonce)`.
#[derive(Default)]
pub struct EdgeTable {
    rows: HashMap<EdgeToken, EdgeEntry>,
}

impl EdgeTable {
    /// Creates an empty [`EdgeTable`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: HashMap::new(),
        }
    }

    /// Mints and records a new edge, returning its bearer token.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn grant(
        &mut self,
        src: SessionId,
        dst: SessionId,
        op: Op,
        nonce: LaunchNonce,
    ) -> EdgeToken {
        let token = EdgeToken::generate();
        self.rows.insert(
            token,
            EdgeEntry {
                src,
                dst,
                op,
                nonce,
            },
        );
        token
    }

    /// Mints one edge per operation of `kind` from `src` to `dst`.
    #[must_use]
    pub fn grant_connection(
        &mut self,
        src: &SessionId,
        dst: &SessionId,
        kind: ConnectionKind,
        nonce: &LaunchNonce,
    ) -> Vec<(Op, EdgeToken)> {
        if src == dst {
            return Vec::new();
        }
        kind.ops()
            .iter()
            .map(|&op| (op, self.grant(src.clone(), dst.clone(), op, *nonce)))
            .collect()
    }

    /// Records a pre-minted token, returning `false` if the token already exists.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn insert(
        &mut self,
        token: EdgeToken,
        src: SessionId,
        dst: SessionId,
        op: Op,
        nonce: LaunchNonce,
    ) -> bool {
        if self.rows.contains_key(&token) {
            return false;
        }
        self.rows.insert(
            token,
            EdgeEntry {
                src,
                dst,
                op,
                nonce,
            },
        );
        true
    }

    /// Removes the edge for `token`, returning `true` if it was present.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn revoke(&mut self, token: &EdgeToken) -> bool {
        self.rows.remove(token).is_some()
    }

    /// Removes all edges from `src` and returns the number removed.
    pub fn revoke_src(&mut self, src: &SessionId) -> usize {
        self.revoke_src_ops(src, None)
    }

    /// Removes edges from `src` matching `ops` (`None` removes all ops) and returns the count.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn revoke_src_ops(&mut self, src: &SessionId, ops: Option<&[Op]>) -> usize {
        let before = self.rows.len();
        self.rows
            .retain(|_, e| e.src != *src || ops.is_some_and(|ops| !ops.contains(&e.op)));
        before.saturating_sub(self.rows.len())
    }

    /// Returns the authorized [`Op`] if `token` matches `dst` and `current_nonce`.
    #[must_use]
    pub fn authorize(
        &self,
        presented: &EdgeToken,
        dst: &SessionId,
        nonce: &LaunchNonce,
    ) -> Option<Op> {
        let e = self.rows.get(presented)?;
        if e.dst == *dst && e.nonce.ct_eq(nonce) {
            Some(e.op)
        } else {
            None
        }
    }

    /// Retains only grants for `op` in this table.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn keep_only(&mut self, op: Op) {
        self.rows.retain(|_, e| e.op == op);
    }

    /// Returns the source session bound to `token`.
    #[must_use]
    pub fn src_of(&self, token: &EdgeToken) -> Option<&SessionId> {
        self.rows.get(token).map(|e| &e.src)
    }

    /// Returns the [`Edge`] bound to `token`.
    #[must_use]
    pub fn edge_of(&self, token: &EdgeToken) -> Option<Edge> {
        self.rows.get(token).map(|e| Edge {
            src: e.src.clone(),
            dst: e.dst.clone(),
            op: e.op,
        })
    }

    /// Returns all live edges as `(src, dst, op)` records.
    #[must_use]
    pub fn edges(&self) -> Vec<Edge> {
        self.rows
            .values()
            .map(|e| Edge {
                src: e.src.clone(),
                dst: e.dst.clone(),
                op: e.op,
            })
            .collect()
    }

    /// Returns the number of live edges.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns whether the table contains no edges.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Authorization verdict returned by [`decide_edge`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeDecision {
    /// The edge is authorized for the exact `(token, dst, op, nonce)`.
    Permit,
    /// Authorization denied (fail closed).
    Deny,
}

impl EdgeDecision {
    /// Returns whether the decision is [`EdgeDecision::Permit`].
    #[must_use]
    #[inline]
    pub const fn is_permitted(self) -> bool {
        matches!(self, EdgeDecision::Permit)
    }
}

/// Evaluates whether `token` authorizes `op` on `dst` at `current_nonce`.
#[must_use]
pub fn decide_edge(
    table: &EdgeTable,
    presented: &EdgeToken,
    dst: &SessionId,
    op: Op,
    nonce: &LaunchNonce,
) -> EdgeDecision {
    match table.rows.get(presented) {
        Some(e) if e.op == op && e.dst == *dst && e.nonce.ct_eq(nonce) => EdgeDecision::Permit,
        _ => EdgeDecision::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (SessionId, SessionId, LaunchNonce) {
        (
            SessionId::new("s-a"),
            SessionId::new("s-b"),
            LaunchNonce::from_bytes([7u8; 16]),
        )
    }

    #[test]
    fn op_wire_tokens_roundtrip_and_unknown_fails_closed() {
        for op in [
            Op::ReadScreen,
            Op::WriteInput,
            Op::Signal,
            Op::ConfigWrite,
            Op::ClipboardWrite,
            Op::DeriveLoop,
        ] {
            assert_eq!(Op::parse(op.as_str()), Some(op));
        }
        assert_eq!(
            Op::parse("write-keys"),
            None,
            "an unknown op must not parse"
        );
        assert_eq!(Op::parse(""), None);
    }

    /// Retained grants keep their token identity and remain revocable;
    /// grants for other operations are removed from the same table.
    #[test]
    fn retaining_an_op_preserves_its_grants_and_removes_others() {
        let (a, b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        let read = tbl.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
        let write = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);
        assert!(
            decide_edge(&tbl, &write, &b, Op::WriteInput, &nonce).is_permitted(),
            "positive control: the write was granted"
        );
        tbl.keep_only(Op::ReadScreen);
        assert_eq!(tbl.len(), 1);
        assert!(decide_edge(&tbl, &read, &b, Op::ReadScreen, &nonce).is_permitted());
        assert!(!decide_edge(&tbl, &write, &b, Op::WriteInput, &nonce).is_permitted());
        assert!(!tbl.revoke(&write), "the write grant was removed");
        assert_eq!(tbl.revoke_src(&a), 1);
        assert!(!decide_edge(&tbl, &read, &b, Op::ReadScreen, &nonce).is_permitted());
    }

    #[test]
    fn decide_edge_permits_only_exact_match() {
        let (a, b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        let tok = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);

        assert_eq!(
            decide_edge(&tbl, &tok, &b, Op::WriteInput, &nonce),
            EdgeDecision::Permit
        );

        assert_eq!(
            decide_edge(&tbl, &tok, &b, Op::ReadScreen, &nonce),
            EdgeDecision::Deny
        );
        assert_eq!(
            decide_edge(&tbl, &tok, &b, Op::Signal, &nonce),
            EdgeDecision::Deny
        );

        assert_eq!(
            decide_edge(&tbl, &tok, &a, Op::WriteInput, &nonce),
            EdgeDecision::Deny
        );

        let restarted = LaunchNonce::from_bytes([9u8; 16]);
        assert_eq!(
            decide_edge(&tbl, &tok, &b, Op::WriteInput, &restarted),
            EdgeDecision::Deny
        );

        assert_eq!(
            decide_edge(&tbl, &EdgeToken::generate(), &b, Op::WriteInput, &nonce),
            EdgeDecision::Deny
        );
    }

    #[test]
    fn revoke_makes_the_edge_deny() {
        let (a, b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        let tok = tbl.grant(a, b.clone(), Op::ReadScreen, nonce);
        assert!(decide_edge(&tbl, &tok, &b, Op::ReadScreen, &nonce).is_permitted());
        assert!(tbl.revoke(&tok), "revoke removes a live edge");
        assert!(!tbl.revoke(&tok), "double-revoke is a no-op");
        assert_eq!(
            decide_edge(&tbl, &tok, &b, Op::ReadScreen, &nonce),
            EdgeDecision::Deny
        );
    }

    #[test]
    fn authorize_returns_op_for_a_live_edge_else_none() {
        let (a, b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        let tok = tbl.grant(a, b.clone(), Op::ReadScreen, nonce);
        assert_eq!(tbl.authorize(&tok, &b, &nonce), Some(Op::ReadScreen));
        assert_eq!(tbl.authorize(&tok, &SessionId::new("s-x"), &nonce), None);
        assert_eq!(
            tbl.authorize(&tok, &b, &LaunchNonce::from_bytes([0u8; 16])),
            None
        );
        assert_eq!(tbl.authorize(&EdgeToken::generate(), &b, &nonce), None);
    }

    #[test]
    fn edges_enumerates_rows_without_leaking_tokens() {
        let (a, b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        assert!(tbl.edges().is_empty(), "empty table enumerates nothing");
        let _t1 = tbl.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
        let _t2 = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);

        let mut got = tbl.edges();
        assert_eq!(got.len(), 2, "both granted edges enumerate");
        got.sort_by_key(|e| e.op.as_str());
        assert_eq!(
            got[0],
            Edge {
                src: a.clone(),
                dst: b.clone(),
                op: Op::ReadScreen
            }
        );
        assert_eq!(
            got[1],
            Edge {
                src: a.clone(),
                dst: b.clone(),
                op: Op::WriteInput
            }
        );

        let to_revoke = *tbl.rows.keys().next().expect("a token");
        tbl.revoke(&to_revoke);
        assert_eq!(tbl.edges().len(), 1, "revoked edge no longer enumerates");
    }

    #[test]
    fn edge_of_reads_the_row_identity_without_the_token() {
        let (a, b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        let tok = tbl.grant(a.clone(), b.clone(), Op::Signal, nonce);
        assert_eq!(
            tbl.edge_of(&tok),
            Some(Edge {
                src: a,
                dst: b,
                op: Op::Signal
            })
        );
        assert_eq!(tbl.edge_of(&EdgeToken::generate()), None);
        tbl.revoke(&tok);
        assert_eq!(tbl.edge_of(&tok), None);
    }

    #[test]
    fn authority_does_not_flow_along_a_cycle_or_self_loop() {
        let a = SessionId::new("s-a");
        let b = SessionId::new("s-b");
        let na = LaunchNonce::from_bytes([1u8; 16]);
        let nb = LaunchNonce::from_bytes([2u8; 16]);

        let mut tbl_b = EdgeTable::new();
        let mut tbl_a = EdgeTable::new();
        let tok_ab = tbl_b.grant(a.clone(), b.clone(), Op::WriteInput, nb);
        let tok_ba = tbl_a.grant(b.clone(), a.clone(), Op::WriteInput, na);

        assert!(decide_edge(&tbl_b, &tok_ab, &b, Op::WriteInput, &nb).is_permitted());
        assert!(decide_edge(&tbl_a, &tok_ba, &a, Op::WriteInput, &na).is_permitted());
        assert_eq!(
            decide_edge(&tbl_a, &tok_ab, &a, Op::WriteInput, &na),
            EdgeDecision::Deny
        );

        let mut tbl_self = EdgeTable::new();
        let tok_aa = tbl_self.grant(a.clone(), a.clone(), Op::WriteInput, na);
        assert!(decide_edge(&tbl_self, &tok_aa, &a, Op::WriteInput, &na).is_permitted());
    }

    #[test]
    fn revoke_src_sweeps_exactly_the_matching_source() {
        let (a, b, nonce) = ids();
        let c = SessionId::new("s-c");
        let mut tbl = EdgeTable::new();
        let t1 = tbl.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
        let t2 = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);
        let t3 = tbl.grant(c.clone(), b.clone(), Op::WriteInput, nonce);

        assert_eq!(tbl.revoke_src(&a), 2);
        assert_eq!(
            decide_edge(&tbl, &t1, &b, Op::ReadScreen, &nonce),
            EdgeDecision::Deny
        );
        assert_eq!(
            decide_edge(&tbl, &t2, &b, Op::WriteInput, &nonce),
            EdgeDecision::Deny
        );
        assert!(decide_edge(&tbl, &t3, &b, Op::WriteInput, &nonce).is_permitted());

        assert_eq!(tbl.revoke_src(&a), 0);
        assert_eq!(tbl.len(), 1);
    }

    #[test]
    fn revoke_src_ops_filters_by_op() {
        let (a, b, nonce) = ids();
        let c = SessionId::new("s-c");
        let mut tbl = EdgeTable::new();
        let read = tbl.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
        let write = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);
        let sig = tbl.grant(a.clone(), b.clone(), Op::Signal, nonce);
        let other = tbl.grant(c.clone(), b.clone(), Op::WriteInput, nonce);

        assert_eq!(
            tbl.revoke_src_ops(&a, Some(&[Op::WriteInput, Op::Signal])),
            2
        );
        assert!(decide_edge(&tbl, &read, &b, Op::ReadScreen, &nonce).is_permitted());
        assert_eq!(
            decide_edge(&tbl, &write, &b, Op::WriteInput, &nonce),
            EdgeDecision::Deny
        );
        assert_eq!(
            decide_edge(&tbl, &sig, &b, Op::Signal, &nonce),
            EdgeDecision::Deny
        );
        assert!(decide_edge(&tbl, &other, &b, Op::WriteInput, &nonce).is_permitted());

        assert_eq!(tbl.revoke_src_ops(&a, Some(&[Op::Signal])), 0);
        assert_eq!(tbl.revoke_src_ops(&a, None), 1);
        assert_eq!(tbl.len(), 1, "only the foreign source's row remains");
    }

    #[test]
    fn connection_kind_ops_are_the_closed_human_fidelity_mapping() {
        assert_eq!(ConnectionKind::Pull.ops(), [Op::ReadScreen]);
        assert_eq!(ConnectionKind::Push.ops(), [Op::WriteInput, Op::Signal]);
        assert_eq!(
            ConnectionKind::Both.ops(),
            [Op::ReadScreen, Op::WriteInput, Op::Signal]
        );
        for kind in [
            ConnectionKind::Pull,
            ConnectionKind::Push,
            ConnectionKind::Both,
        ] {
            for op in [Op::ConfigWrite, Op::ClipboardWrite, Op::DeriveLoop] {
                assert!(
                    !kind.ops().contains(&op),
                    "{kind:?} must not express {op:?}"
                );
            }
        }
    }

    #[test]
    fn grant_connection_mints_exactly_the_kinds_ops() {
        let (a, b, nonce) = ids();

        let mut tbl = EdgeTable::new();
        let pulled = tbl.grant_connection(&a, &b, ConnectionKind::Pull, &nonce);
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].0, Op::ReadScreen);
        assert_eq!(tbl.len(), 1, "one row per minted op, nothing extra");

        let mut tbl = EdgeTable::new();
        let both = tbl.grant_connection(&a, &b, ConnectionKind::Both, &nonce);
        let ops: Vec<Op> = both.iter().map(|(op, _)| *op).collect();
        assert_eq!(ops, [Op::ReadScreen, Op::WriteInput, Op::Signal]);
        assert_eq!(tbl.len(), 3);
    }

    #[test]
    fn grant_connection_refuses_a_self_loop() {
        let (a, _b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        for kind in [
            ConnectionKind::Pull,
            ConnectionKind::Push,
            ConnectionKind::Both,
        ] {
            assert!(
                tbl.grant_connection(&a, &a, kind, &nonce).is_empty(),
                "{kind:?} self-loop must mint nothing"
            );
        }
        assert!(tbl.is_empty(), "a refused self-loop leaves no row behind");
    }

    #[test]
    fn grant_connection_tokens_authorize_and_revoke_src_kills_them() {
        let (a, b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        let minted = tbl.grant_connection(&a, &b, ConnectionKind::Push, &nonce);
        assert_eq!(minted.len(), 2);

        for (op, tok) in &minted {
            assert!(decide_edge(&tbl, tok, &b, *op, &nonce).is_permitted());
        }

        assert_eq!(tbl.revoke_src(&a), 2);
        for (op, tok) in &minted {
            assert_eq!(decide_edge(&tbl, tok, &b, *op, &nonce), EdgeDecision::Deny);
        }
    }

    #[test]
    fn edge_and_backlog_const_helpers_invariants() {
        assert!(Op::ReadScreen.is_human_seat_op() && !Op::ReadScreen.is_elevated_op());
        assert!(!Op::ReadScreen.mutates_destination());
        assert!(Op::WriteInput.mutates_destination());
        assert!(Op::ConfigWrite.is_elevated_op() && !Op::ConfigWrite.is_human_seat_op());

        assert_eq!(ConnectionKind::Pull.op_count(), 1);
        assert_eq!(ConnectionKind::Push.op_count(), 2);
        assert_eq!(ConnectionKind::Both.op_count(), 3);
        assert!(ConnectionKind::Pull.includes_pull() && !ConnectionKind::Pull.includes_push());
        assert!(ConnectionKind::Push.includes_push() && !ConnectionKind::Push.includes_pull());
        assert!(ConnectionKind::Both.includes_pull() && ConnectionKind::Both.includes_push());
        assert!(ConnectionKind::Push.grants_op(Op::WriteInput));
        assert!(!ConnectionKind::Push.grants_op(Op::ReadScreen));

        let idle = crate::input_backlog::Contention::from_per_cpu_milli(500);
        assert!(!idle.is_oversubscribed());
        assert_eq!(idle.factor_milli(), 1000);
        assert_eq!(idle.stretch_millis(10_000), 10_000);
        assert_eq!(idle.fair_share_ns(1_000_000), 1_000_000);

        let heavy = crate::input_backlog::Contention::from_per_cpu_milli(25_000);
        assert!(heavy.is_oversubscribed() && heavy.is_at_max_stretch());
        assert_eq!(heavy.factor_milli(), 10_000);
        assert_eq!(heavy.stretch_millis(10_000), 100_000);
        assert_eq!(heavy.fair_share_ns(1_000_000), 100_000);

        assert_eq!(crate::input_backlog::oldest_unread_offset(100, 30), 70);
        assert!(crate::input_backlog::unread_within_accepted(100, 30));
        assert!(!crate::input_backlog::unread_within_accepted(30, 100));
        let wm = crate::input_backlog::encode_backlog_watermarks_le(1000, 250);
        assert_eq!(
            crate::input_backlog::decode_backlog_watermarks_le(wm),
            (1000, 250)
        );
    }
}
