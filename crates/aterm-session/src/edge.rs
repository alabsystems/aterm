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
        aterm_digest::ct_eq(&self.0, &other.0)
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
#[path = "edge_tests.rs"]
mod tests;
