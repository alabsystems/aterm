// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-edge, op-scoped, fail-closed authority table (design §7.1, A.6).
//!
//! An [`Edge`] is one directed `src → dst` authority for one [`Op`]. A cycle
//! `A → B → A` is two independent rows; a self-loop `A → A` is the one row
//! `(A, A, op)`. [`decide_edge`] is a per-call point lookup, never a
//! reachability walk, so no graph shape can cause recursion or transitive
//! authority accumulation in the gate.

use std::collections::HashMap;

use crate::id::{LaunchNonce, SessionId};
use crate::{from_hex, hex};

/// The operation an edge authorizes. Split so a `WriteInput` edge cannot signal
/// and a `ReadScreen` edge cannot write (design §7.2).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Op {
    /// Read the rendered surface (screen/cells/blocks/scrollback/search/image/
    /// timeline). The least-power edge.
    ReadScreen,
    /// Inject input: the full human vocabulary (keys, mouse, wheel, selection,
    /// paste, resize, focus), all converging on the one `App::input` seam.
    WriteInput,
    /// Signal the destination's foreground process group (a human's Ctrl-C is a
    /// byte, not this out-of-band signal).
    Signal,
    /// Rewrite the destination's durable configuration (`settings set|unset` →
    /// `aterm.toml`). Separate from `WriteInput` so an input edge cannot flip a
    /// default-off security knob (allow_osc52_query/window_ops/…). Not
    /// provisioned to child edges.
    ConfigWrite,
    /// Copy the destination's selection onto the system clipboard (the `copy`
    /// exfiltration boundary). Separate from `ReadScreen` because writing the OS
    /// pasteboard leaves the process. Not provisioned to child edges.
    ClipboardWrite,
    /// Feed bytes derived from a read of an untrusted node back as input (the
    /// semantic-cycle path); off by default (§7.6).
    DeriveLoop,
}

impl Op {
    /// The stable wire token (e.g. for the ctl `grant <src> <op>` verb).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Op::ReadScreen => "read-screen",
            Op::WriteInput => "write-input",
            Op::Signal => "signal",
            Op::ConfigWrite => "config-write",
            Op::ClipboardWrite => "clipboard-write",
            Op::DeriveLoop => "derive-loop",
        }
    }

    /// Parse the wire token. Unknown strings return `None` (fail closed).
    #[must_use]
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
}

/// The connection vocabulary the UX and wire mint in, deliberately smaller than
/// [`Op`]. **Push** is the full human seat: keystrokes plus the Ctrl-C-class
/// signal ([`Op::WriteInput`] + [`Op::Signal`], never one without the other).
/// **Pull** is the rendered screen as a human reads it ([`Op::ReadScreen`]). No
/// kind spells `ConfigWrite`/`ClipboardWrite`/`DeriveLoop`, so a connection
/// cannot mint those greater authorities: unrepresentable, not merely unchecked.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ConnectionKind {
    /// Read the destination's rendered surface: what a human at that terminal
    /// sees, and nothing more.
    Pull,
    /// Drive the destination as a human would sit at it: inject input and
    /// signal its foreground process group.
    Push,
    /// Both directions of authority over the destination: pull + push.
    Both,
}

impl ConnectionKind {
    /// The exact ops this kind mints; [`EdgeTable::grant_connection`] draws only
    /// from these.
    #[must_use]
    pub fn ops(&self) -> &'static [Op] {
        match self {
            ConnectionKind::Pull => &[Op::ReadScreen],
            ConnectionKind::Push => &[Op::WriteInput, Op::Signal],
            ConnectionKind::Both => &[Op::ReadScreen, Op::WriteInput, Op::Signal],
        }
    }
}

/// A directed authority edge `src → dst` for one [`Op`]: a labelled row, with no
/// graph object and no traversal.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Edge {
    pub src: SessionId,
    pub dst: SessionId,
    pub op: Op,
}

/// An unforgeable per-edge bearer secret (design §7.1): 32 random bytes recorded
/// in the destination's [`EdgeTable`] and presented by the source on every `ctl`
/// connect. Not the per-instance token: holding `(A, B, WriteInput)` confers
/// nothing toward `(B, A, *)`. Redacted in `Debug`; never logged in the clear.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeToken([u8; 32]);

impl EdgeToken {
    /// Wrap raw token bytes (e.g. a token the launcher provisioned out-of-band).
    #[must_use]
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// The raw token bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Mint a fresh random token from the OS CSPRNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut b = [0u8; 32];
        crate::fill_random(&mut b);
        Self(b)
    }

    /// Lowercase-hex (64 chars), the `TOKEN <hex>` handshake form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Parse from the 64-char hex handshake form.
    #[must_use]
    pub fn from_hex(s: &str) -> Option<Self> {
        let mut b = [0u8; 32];
        from_hex(s, &mut b)?;
        Some(Self(b))
    }

    /// Constant-time equality, for any comparison outside the table lookup.
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

/// The destination-side table mapping each presented [`EdgeToken`] to the
/// `(src, dst, op, launch-nonce)` it authorizes. Empty by default; an absent
/// token means no authority (fail closed). Storing `dst` per row keeps it correct
/// if one process serves several sessions.
#[derive(Default)]
pub struct EdgeTable {
    rows: HashMap<EdgeToken, EdgeEntry>,
}

impl EdgeTable {
    /// A fresh, empty table: no authority.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: HashMap::new(),
        }
    }

    /// Mint and record a new edge, returning the bearer token the source
    /// presents. `nonce` is the destination's current launch nonce, so the edge
    /// fails closed across a restart.
    // Skip: `HashMap::insert` can formally panic on capacity overflow,
    // unreachable for a table bounded by live sessions; the key's derived
    // Hash/Eq is panic-free. Audited-alloc class; verify-only.
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

    /// Mint one edge per op of `kind` from `src` into this (the destination's)
    /// table, returning the `(op, token)` pairs to deliver to the source. This is
    /// the one connection mint path (the `connect` verb and the UI). Ops come only
    /// from [`ConnectionKind::ops`]; there is no raw-op parameter. A self-loop
    /// (`src == dst`) is refused (empty `Vec`, nothing minted) so a
    /// connection-to-self never becomes authority by accident.
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

    /// Record a pre-minted token (e.g. one the launcher provisioned under the
    /// `control_auth` file discipline). Returns `false` and records nothing if the
    /// token exists: minting never overwrites a live edge.
    // Skip: same audited `HashMap::insert` alloc class as `grant` above.
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

    /// Remove an edge. Returns `true` if it existed (the basis for `revoke`).
    // Skip: `HashMap::remove` does not allocate and the key's Hash/Eq is
    // panic-free, but keyed-op totality is unproved; audited like `grant`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn revoke(&mut self, token: &EdgeToken) -> bool {
        self.rows.remove(token).is_some()
    }

    /// Remove every row whose src is `src` (any op) and return the count: the
    /// source-death sweep. Required for safety: a destination restart fails
    /// closed via its nonce, but a dead source's tokens stay live against a
    /// running destination, and tokens are deliberately non-enumerable
    /// ([`EdgeTable::edges`]), so nothing else could revoke them.
    pub fn revoke_src(&mut self, src: &SessionId) -> usize {
        self.revoke_src_ops(src, None)
    }

    /// [`EdgeTable::revoke_src`] with an op filter: remove rows whose src is
    /// `src` and whose op is in `ops` (`None` = any op). This lets the
    /// `disconnect … kind=` verb revoke exactly the push half and keep pull, for
    /// rows whose token was never recorded.
    // Skip: `HashMap::retain` does not allocate; the closure is panic-free
    // (derived Hash/Eq, `SessionId` equality, slice `contains`). Same tier as
    // `revoke`. `saturating_sub` is exact since retain never grows the map.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn revoke_src_ops(&mut self, src: &SessionId, ops: Option<&[Op]>) -> usize {
        let before = self.rows.len();
        self.rows
            .retain(|_, e| e.src != *src || ops.is_some_and(|ops| !ops.contains(&e.op)));
        before.saturating_sub(self.rows.len())
    }

    /// Resolve a presented token to the op it authorizes, if it is a live edge
    /// whose dst and launch nonce match `dst`/`nonce`; `None` on any miss or
    /// mismatch. The `ctl` auth handshake uses it to scope a connection to one
    /// op. Same predicate as [`decide_edge`], returning the op instead of taking it.
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

    /// The source bound to a token, for audit (`who injected into whom`).
    #[must_use]
    pub fn src_of(&self, token: &EdgeToken) -> Option<&SessionId> {
        self.rows.get(token).map(|e| &e.src)
    }

    /// The `(src, dst, op)` bound to a token, which the audit seam records
    /// before [`EdgeTable::revoke`] deletes the row. Exposes no token.
    #[must_use]
    pub fn edge_of(&self, token: &EdgeToken) -> Option<Edge> {
        self.rows.get(token).map(|e| Edge {
            src: e.src.clone(),
            dst: e.dst.clone(),
            op: e.op,
        })
    }

    /// Every live edge as `(src, dst, op)`, for the `edges`/`grants` control
    /// verb. The bearer token is deliberately omitted: enumerating the table must
    /// never leak a capability. Order is arbitrary; callers sort.
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

    /// Number of live edges.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table holds no edges.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The decision [`decide_edge`] returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeDecision {
    /// The edge is authorized for this exact `(token, dst, op, nonce)`.
    Permit,
    /// Default. Any miss or mismatch denies — fail closed.
    Deny,
}

impl EdgeDecision {
    /// Whether the decision permits the action.
    #[must_use]
    pub fn is_permitted(self) -> bool {
        matches!(self, EdgeDecision::Permit)
    }
}

/// Total, fail-closed, per-call authority gate (design §7.1). Default `Deny`.
///
/// Permits iff the presented token is recorded and its op `== op`, its dst
/// `== dst`, and its recorded launch nonce matches `nonce` (the target has not
/// restarted under the same name/pid). Any miss or mismatch denies.
///
/// Never traverses the graph: each edge is an independent row, and holding one
/// token says nothing about any other.
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

    #[test]
    fn decide_edge_permits_only_exact_match() {
        let (a, b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        let tok = tbl.grant(a.clone(), b.clone(), Op::WriteInput, nonce);

        // Exact match permits.
        assert_eq!(
            decide_edge(&tbl, &tok, &b, Op::WriteInput, &nonce),
            EdgeDecision::Permit
        );

        // Wrong op denies (a write token cannot read or signal).
        assert_eq!(
            decide_edge(&tbl, &tok, &b, Op::ReadScreen, &nonce),
            EdgeDecision::Deny
        );
        assert_eq!(
            decide_edge(&tbl, &tok, &b, Op::Signal, &nonce),
            EdgeDecision::Deny
        );

        // Wrong dst denies (a token minted for B cannot drive A).
        assert_eq!(
            decide_edge(&tbl, &tok, &a, Op::WriteInput, &nonce),
            EdgeDecision::Deny
        );

        // Wrong nonce denies (target restarted -> fail closed).
        let restarted = LaunchNonce::from_bytes([9u8; 16]);
        assert_eq!(
            decide_edge(&tbl, &tok, &b, Op::WriteInput, &restarted),
            EdgeDecision::Deny
        );

        // Unknown token denies (default fail-closed).
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
        // A live edge resolves to its op (what the ctl handshake scopes the conn to).
        assert_eq!(tbl.authorize(&tok, &b, &nonce), Some(Op::ReadScreen));
        // Wrong dst / restarted nonce / unknown token all fail closed.
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
        // The triples are the granted (src, dst, op); an Edge carries only those
        // fields, so no token is reachable through them.
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

        // Revoking one drops it from the enumeration.
        let to_revoke = *tbl.rows.keys().next().expect("a token");
        tbl.revoke(&to_revoke);
        assert_eq!(tbl.edges().len(), 1, "revoked edge no longer enumerates");
    }

    #[test]
    fn edge_of_reads_the_row_identity_without_the_token() {
        let (a, b, nonce) = ids();
        let mut tbl = EdgeTable::new();
        let tok = tbl.grant(a.clone(), b.clone(), Op::Signal, nonce);
        // A live token resolves to its (src, dst, op), the triple the audit seam
        // records before `revoke` deletes the row.
        assert_eq!(
            tbl.edge_of(&tok),
            Some(Edge {
                src: a,
                dst: b,
                op: Op::Signal
            })
        );
        // An unknown (or revoked) token resolves to nothing — fail closed.
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

        // A cycle A->B->A is two independent rows, in two tables (one per dst).
        let mut tbl_b = EdgeTable::new(); // edges whose dst is B
        let mut tbl_a = EdgeTable::new(); // edges whose dst is A
        let tok_ab = tbl_b.grant(a.clone(), b.clone(), Op::WriteInput, nb);
        let tok_ba = tbl_a.grant(b.clone(), a.clone(), Op::WriteInput, na);

        // Each token only works for its own (dst, nonce); holding one says nothing
        // about the other direction.
        assert!(decide_edge(&tbl_b, &tok_ab, &b, Op::WriteInput, &nb).is_permitted());
        assert!(decide_edge(&tbl_a, &tok_ba, &a, Op::WriteInput, &na).is_permitted());
        // tok_ab presented against A's table (the reverse edge) -> Deny.
        assert_eq!(
            decide_edge(&tbl_a, &tok_ab, &a, Op::WriteInput, &na),
            EdgeDecision::Deny
        );

        // A self-loop A->A is the single row (A,A,op): one grant, permits A driving A.
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

        // Sweeping A drops BOTH of A's rows (any op) and counts them; C's row
        // — same dst, different src — survives untouched.
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

        // A second sweep of the now-absent source removes nothing.
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

        // The push half (write-input + signal) sweeps; pull survives.
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
        // A different source's row of a filtered op is untouched.
        assert!(decide_edge(&tbl, &other, &b, Op::WriteInput, &nonce).is_permitted());

        // A filter matching nothing removes nothing; `None` is the bare sweep.
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
        // The greater authorities are unreachable through any kind.
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

        // Pull mints exactly the one read edge.
        let mut tbl = EdgeTable::new();
        let pulled = tbl.grant_connection(&a, &b, ConnectionKind::Pull, &nonce);
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].0, Op::ReadScreen);
        assert_eq!(tbl.len(), 1, "one row per minted op, nothing extra");

        // Both mints exactly the three human-fidelity ops.
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

        // Each minted token is a real, live edge: it authorizes its own op.
        for (op, tok) in &minted {
            assert!(decide_edge(&tbl, tok, &b, *op, &nonce).is_permitted());
        }

        // Source death sweeps the whole connection; every token now denies.
        assert_eq!(tbl.revoke_src(&a), 2);
        for (op, tok) in &minted {
            assert_eq!(decide_edge(&tbl, tok, &b, *op, &nonce), EdgeDecision::Deny);
        }
    }
}
