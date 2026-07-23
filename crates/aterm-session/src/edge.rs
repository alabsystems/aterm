// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-edge, op-scoped, fail-closed authority table (design §7.1, A.6).
//!
//! An [`Edge`] is a single directed `src → dst` authority for one [`Op`]. A cycle
//! `A → B → A` is two independent rows; a self-loop `A → A` is the one row
//! `(A, A, op)`. [`decide_edge`] is a per-call POINT LOOKUP, never a reachability
//! walk — so an arbitrary cyclic graph provably cannot cause recursion or
//! transitive authority accumulation in the gate.

use std::collections::HashMap;

use crate::id::{LaunchNonce, SessionId};
use crate::{from_hex, hex};

/// The operation an edge authorizes. Split so a `WriteInput` edge cannot signal and
/// a `ReadScreen` edge cannot write (design §7.2). Mirrors
/// `aterm_cap::effects::{ReadScreen, WriteInput, SignalEdge}` (the coarse class
/// gate); this is the fine, object-scoped identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Op {
    /// Read the rendered surface (screen/cells/blocks/scrollback/search/image/
    /// timeline). The least-power edge.
    ReadScreen,
    /// Inject input — the full human vocabulary (keys, mouse, wheel, selection,
    /// paste, resize, focus), all converging on the one `App::input` seam.
    WriteInput,
    /// Signal the destination's foreground process group (distinct from a keyboard
    /// byte — a human's Ctrl-C is a byte, not an out-of-band signal).
    Signal,
    /// Rewrite the destination's DURABLE on-disk configuration (the `settings
    /// set|unset` path → `aterm.toml`). Split OUT of `WriteInput` so a keystroke-
    /// injection edge cannot silently flip a default-OFF security knob
    /// (allow_osc52_query/window_ops/…): persisting config is a strictly greater
    /// authority than typing a byte, and is NOT provisioned to child edges.
    ConfigWrite,
    /// Move the destination's SELECTION out of the process onto the system
    /// clipboard (the `copy` exfil boundary). Split OUT of `ReadScreen` because a
    /// read stays in-process, whereas writing the OS pasteboard crosses the trust
    /// boundary; NOT provisioned to child edges.
    ClipboardWrite,
    /// Feed bytes DERIVED from a read of an untrusted node back as input — the
    /// semantic-cycle path; off by default (§7.6).
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

/// A directed authority edge `src → dst` for one [`Op`]. There is no graph object
/// and no traversal: this is just a labelled row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Edge {
    pub src: SessionId,
    pub dst: SessionId,
    pub op: Op,
}

/// An unforgeable per-edge bearer secret (design §7.1): 32 random bytes recorded in
/// the DESTINATION's [`EdgeTable`] and presented by the source on every `ctl`
/// connect. **Not** the per-instance god-token: holding `(A, B, WriteInput)` confers
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
/// `(src, dst, op, launch-nonce)` it authorizes. Default-empty; an absent token
/// means no authority (fail closed). It holds edges for the local session(s) this
/// process serves; storing `dst` per row keeps it correct for the future
/// multi-session-per-process case.
#[derive(Default)]
pub struct EdgeTable {
    rows: HashMap<EdgeToken, EdgeEntry>,
}

impl EdgeTable {
    /// A fresh, empty table (no authority — fail closed by default).
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: HashMap::new(),
        }
    }

    /// Mint and record a new edge, returning the bearer token the source presents.
    /// `nonce` is the DESTINATION's current launch nonce (so the edge fails closed
    /// across a restart).
    // Skip: bottoms out at `HashMap::insert` — growth can formally panic on
    // capacity overflow (len > isize::MAX bytes), unreachable for a table
    // bounded by live session count; key code is the derived Hash/Eq of
    // `EdgeToken([u8; 32])` (panic-free). Audited-alloc class; verify-only.
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

    /// Record a PRE-MINTED token (e.g. one provisioned by the launcher with the
    /// `control_auth` file discipline). Returns `false` (and records nothing) if the
    /// token already exists — minting must never silently overwrite a live edge.
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
    // Skip: `HashMap::remove` runs no allocation — only the derived Hash/Eq
    // of `EdgeToken([u8; 32])` (panic-free) — but the keyed-op totality is
    // per-corpus (T3 keyed-writes lane); audited here, same tier as `grant`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn revoke(&mut self, token: &EdgeToken) -> bool {
        self.rows.remove(token).is_some()
    }

    /// Resolve a presented token to the OP it authorizes, IF it is a live edge whose
    /// dst and launch-nonce match `dst`/`nonce` — the fail-closed lookup the `ctl`
    /// auth handshake uses to scope a connection to a single op. `None` on any miss
    /// or mismatch (so the handshake fails closed). This is the same predicate as
    /// [`decide_edge`] but returns the op instead of taking it as input, since at
    /// connect time the op (which verbs are allowed) is what we want to learn.
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

    /// A clone of every live edge as `(src, dst, op)` triples — the introspection
    /// query surface for the `edges`/`grants` control verb. The bearer TOKEN is
    /// DELIBERATELY omitted: it is an unforgeable secret (the per-edge capability),
    /// so enumerating the table for a human/agent must never leak it. Order is the
    /// HashMap's arbitrary iteration order; the caller sorts for a stable listing.
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

/// TOTAL, FAIL-CLOSED, per-call authority gate (design §7.1). Default `Deny`.
///
/// Permits **iff** the presented token is recorded **and** its op `== op` **and**
/// its dst `== dst` **and** its recorded launch-nonce matches `nonce` (the target
/// has not restarted under the same name/pid). Any miss or mismatch → `Deny`.
///
/// NEVER traverses the graph: each edge is an independent row, so a cycle or
/// self-loop cannot cause recursion or transitive authority accumulation. Authority
/// does not flow along edges — holding one token says nothing about any other.
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
        // The triples are the granted (src, dst, op) — and the only fields an Edge
        // carries are src/dst/op, so no secret token can be reached through them.
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
    fn authority_does_not_flow_along_a_cycle_or_self_loop() {
        let a = SessionId::new("s-a");
        let b = SessionId::new("s-b");
        let na = LaunchNonce::from_bytes([1u8; 16]);
        let nb = LaunchNonce::from_bytes([2u8; 16]);

        // A cycle A->B->A is two INDEPENDENT rows, in two tables (one per dst).
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
}
