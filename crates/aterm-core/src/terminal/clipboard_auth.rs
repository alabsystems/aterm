// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Host-side authorization for OSC 52 clipboard set and query.
//!
//! PTY bytes (a `cat` of a hostile file) look identical to a program's legitimate
//! OSC 52 write or read, so the clipboard callback is reachable only through a
//! zero-sized capability ([`ClipboardWriteCapability`] / [`ClipboardQueryCapability`],
//! private `_seal`) minted by `ClipboardAuth::try_mint_*_capability_with_engine`, and
//! only the helpers that demand one ([`invoke_set`], [`invoke_query`],
//! [`invoke_clear`]) call it. Handlers can mint but never authorize: the host grants
//! and revokes through `Terminal::authorize_clipboard_access` /
//! `revoke_clipboard_access`. Fresh terminals start with both revoked.
//!
//! That is the engine's posture. The native GUI (`aterm-gui`'s `configure_clipboard`)
//! authorizes write, with no prompt as in kitty and WezTerm, for every tab not spawned
//! under Containment, and query only when `allow_osc52_query` is set; a Containment
//! tab gets neither, and no callback (`host_reach`). The standard policy's
//! `OSC 52 set` rule requires `User` origin, so PTY bytes fall to
//! `BridgeDecision::Fallback` and the host's bit decides.

use super::callbacks::ClipboardCallback;
use super::policy_gates::PolicyGates;
use super::types::{ClipboardOperation, ClipboardSelection};
use aterm_policy::selector::DispatchedSequence;

/// Selector for the two classes of OSC 52 clipboard access, used by
/// [`super::Terminal::authorize_clipboard_access`] and
/// [`super::Terminal::revoke_clipboard_access`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardAccess {
    /// OSC 52 clipboard set / clear (write).
    Write,
    /// OSC 52 clipboard query (read back through the PTY response stream).
    Query,
}

/// Capability for OSC 52 clipboard set, minted only by `ClipboardAuth`'s
/// `try_mint_write_capability_with_engine`. [`invoke_set`] consumes it by value, so
/// a stashed token cannot be replayed.
#[must_use = "ClipboardWriteCapability has no effect unless passed to invoke_set"]
pub(super) struct ClipboardWriteCapability {
    _seal: super::Seal,
}

/// Capability for OSC 52 clipboard query, minted only by `ClipboardAuth`'s
/// `try_mint_query_capability_with_engine`.
#[must_use = "ClipboardQueryCapability has no effect unless passed to invoke_query"]
pub(super) struct ClipboardQueryCapability {
    _seal: super::Seal,
}

/// Host-side authorization state for OSC 52 clipboard access.
///
/// Forwarded to [`super::TerminalHandler`] so handlers can mint; the
/// `authorize_*` / `revoke_*` methods are `pub(crate)` and host-only.
#[derive(Debug, Default)]
pub(crate) struct ClipboardAuth {
    write_authorized: bool,
    query_authorized: bool,
}

impl ClipboardAuth {
    /// Construct an authorization state with no capabilities granted.
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            write_authorized: false,
            query_authorized: false,
        }
    }

    /// Authorize OSC 52 clipboard set: PTY-origin set sequences then mint a
    /// [`ClipboardWriteCapability`] and reach the callback.
    #[inline]
    pub(crate) const fn authorize_write(&mut self) {
        self.write_authorized = true;
    }

    /// Revoke OSC 52 clipboard *set*: PTY-origin sets are dropped until
    /// [`Self::authorize_write`] is called again.
    #[inline]
    pub(crate) const fn revoke_write(&mut self) {
        self.write_authorized = false;
    }

    /// Authorize OSC 52 clipboard *query* (read).
    #[inline]
    pub(crate) const fn authorize_query(&mut self) {
        self.query_authorized = true;
    }

    /// Revoke OSC 52 clipboard *query*.
    #[inline]
    pub(crate) const fn revoke_query(&mut self) {
        self.query_authorized = false;
    }

    /// Mint a write capability, reading the `OSC 52 set` gate that
    /// [`super::policy_gates`] compiled from [`probe_osc52_set`] at
    /// [`super::policy_gates::GATE_ORIGIN`]:
    ///
    /// * a sequence-specific `Execute` rule mints;
    /// * a universal wildcard `Execute` rule (`response any`) falls back to
    ///   `write_authorized`, so broad profiles cannot reopen this
    ///   deny-by-default sink;
    /// * a rule with any other response refuses, whatever `write_authorized` says;
    /// * no matching rule, or no policy installed
    ///   (`BridgeDecision::Fallback`), falls back to `write_authorized`.
    ///
    /// The probe and origin are literals, so the verdict is resolved once per
    /// installed policy; `policy_bridge.rs` has the decision tree.
    #[inline]
    pub(super) const fn try_mint_write_capability_with_engine(
        &self,
        gates: PolicyGates,
    ) -> Option<ClipboardWriteCapability> {
        if gates.osc52_set().resolve(self.write_authorized) {
            Some(ClipboardWriteCapability { _seal: super::Seal })
        } else {
            None
        }
    }

    /// Mint a query capability: the same rules as
    /// [`Self::try_mint_write_capability_with_engine`], against the
    /// `OSC 52 query` selector and `query_authorized`.
    #[inline]
    pub(super) const fn try_mint_query_capability_with_engine(
        &self,
        gates: PolicyGates,
    ) -> Option<ClipboardQueryCapability> {
        if gates.osc52_query().resolve(self.query_authorized) {
            Some(ClipboardQueryCapability { _seal: super::Seal })
        } else {
            None
        }
    }
}

/// Probe sequence for the OSC 52 set policy lookup, compiled into the gate by
/// [`super::policy_gates`]. Any second param other than `?` matches the `OSC 52 set`
/// alias, so `SGVsbG8=` stands in for every payload.
#[inline]
pub(super) fn probe_osc52_set() -> DispatchedSequence {
    DispatchedSequence::osc(52, [String::from("c"), String::from("SGVsbG8=")])
}

/// Probe sequence for the OSC 52 query policy lookup: the literal `?` second param
/// selects the `OSC 52 query` alias.
#[inline]
pub(super) fn probe_osc52_query() -> DispatchedSequence {
    DispatchedSequence::osc(52, [String::from("c"), String::from("?")])
}

/// Invoke the host clipboard callback with a set operation, consuming the
/// capability. A no-op without a callback.
#[inline]
pub(super) fn invoke_set(
    callback_slot: &mut Option<ClipboardCallback>,
    _token: ClipboardWriteCapability,
    selections: &[ClipboardSelection],
    content: String,
) {
    if let Some(ref mut callback) = *callback_slot {
        let op = ClipboardOperation::Set {
            selections: selections.to_vec(),
            content,
        };
        let _ = callback(op);
    }
}

/// Invoke the host clipboard callback with a query operation: the host's content, or
/// `None` when it denies, has none, or no callback is wired.
#[inline]
pub(super) fn invoke_query(
    callback_slot: &mut Option<ClipboardCallback>,
    _token: ClipboardQueryCapability,
    selections: &[ClipboardSelection],
) -> Option<String> {
    let callback = callback_slot.as_mut()?;
    let op = ClipboardOperation::Query {
        selections: selections.to_vec(),
    };
    callback(op)
}

/// Invoke the host clipboard callback with a clear operation. Clear takes the write
/// capability: it can only empty the clipboard, and a host that wants clear but not
/// set enforces that in the callback.
#[inline]
pub(super) fn invoke_clear(
    callback_slot: &mut Option<ClipboardCallback>,
    _token: ClipboardWriteCapability,
    selections: &[ClipboardSelection],
) {
    if let Some(ref mut callback) = *callback_slot {
        let op = ClipboardOperation::Clear {
            selections: selections.to_vec(),
        };
        let _ = callback(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_policy::engine::PolicyEngine;
    use aterm_policy::{OriginTag, Policy, Response};

    fn policy_with_rule(sequence: &str, response: Response) -> Policy {
        crate::terminal::policy_bridge::one_rule_policy(
            sequence,
            response,
            OriginTag::Pty,
            Response::Drop,
        )
    }

    /// With no policy installed the host's bits decide: a fresh state mints
    /// nothing, each grant mints only its own capability, and revoking one
    /// leaves the other.
    #[test]
    fn host_bits_mint_independently_and_default_to_deny() {
        let mints = |auth: &ClipboardAuth| {
            let gates = PolicyGates::compile(None);
            (
                auth.try_mint_write_capability_with_engine(gates).is_some(),
                auth.try_mint_query_capability_with_engine(gates).is_some(),
            )
        };
        let mut auth = ClipboardAuth::new();
        assert_eq!(mints(&auth), (false, false), "deny by default");
        auth.authorize_write();
        assert_eq!(mints(&auth), (true, false));
        auth.authorize_query();
        auth.revoke_write();
        assert_eq!(mints(&auth), (false, true), "revoking write keeps query");
        auth.authorize_write();
        auth.revoke_query();
        assert_eq!(mints(&auth), (true, false), "revoking query keeps write");
        auth.revoke_write();
        assert_eq!(mints(&auth), (false, false));
    }

    #[test]
    fn explicit_osc52_set_rule_still_allows_revoked_write_access() {
        let auth = ClipboardAuth::new();
        let engine = PolicyEngine::new(policy_with_rule("OSC 52 set", Response::Execute));

        assert!(
            auth.try_mint_write_capability_with_engine(PolicyGates::compile(Some(&engine)))
                .is_some()
        );
    }

    #[test]
    fn explicit_osc52_query_rule_still_allows_revoked_query_access() {
        let auth = ClipboardAuth::new();
        let engine = PolicyEngine::new(policy_with_rule("OSC 52 query", Response::Execute));

        assert!(
            auth.try_mint_query_capability_with_engine(PolicyGates::compile(Some(&engine)))
                .is_some()
        );
    }
}
