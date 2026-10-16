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
/// `try_mint_write_capability*`. [`invoke_set`] consumes it by value, so a stashed
/// token cannot be replayed.
#[must_use = "ClipboardWriteCapability has no effect unless passed to invoke_set"]
pub(super) struct ClipboardWriteCapability {
    _seal: super::Seal,
}

/// Capability for OSC 52 clipboard query, minted only by `ClipboardAuth`'s
/// `try_mint_query_capability*`.
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
    pub(crate) const fn new() -> Self {
        Self {
            write_authorized: false,
            query_authorized: false,
        }
    }

    /// Authorize OSC 52 clipboard set: PTY-origin set sequences then mint a
    /// [`ClipboardWriteCapability`] and reach the callback.
    pub(crate) fn authorize_write(&mut self) {
        self.write_authorized = true;
    }

    /// Revoke OSC 52 clipboard *set*: PTY-origin sets are dropped until
    /// [`Self::authorize_write`] is called again.
    pub(crate) fn revoke_write(&mut self) {
        self.write_authorized = false;
    }

    /// Authorize OSC 52 clipboard *query* (read).
    pub(crate) fn authorize_query(&mut self) {
        self.query_authorized = true;
    }

    /// Revoke OSC 52 clipboard *query*.
    pub(crate) fn revoke_query(&mut self) {
        self.query_authorized = false;
    }

    /// Whether OSC 52 clipboard *set* is currently authorized.
    #[cfg(test)]
    pub(crate) const fn is_write_authorized(&self) -> bool {
        self.write_authorized
    }

    /// Whether OSC 52 clipboard *query* is currently authorized.
    #[cfg(test)]
    pub(crate) const fn is_query_authorized(&self) -> bool {
        self.query_authorized
    }

    /// Mint a write capability iff [`Self::authorize_write`] was called and not
    /// since revoked.
    #[cfg(test)]
    pub(super) fn try_mint_write_capability(&self) -> Option<ClipboardWriteCapability> {
        if self.write_authorized {
            Some(ClipboardWriteCapability { _seal: super::Seal })
        } else {
            None
        }
    }

    /// Mint a query capability iff [`Self::authorize_query`] was called and not
    /// since revoked.
    #[cfg(test)]
    pub(super) fn try_mint_query_capability(&self) -> Option<ClipboardQueryCapability> {
        if self.query_authorized {
            Some(ClipboardQueryCapability { _seal: super::Seal })
        } else {
            None
        }
    }

    /// Engine-consulting variant of `Self::try_mint_write_capability`, reading
    /// the `OSC 52 set` gate that [`super::policy_gates`] compiled from
    /// [`probe_osc52_set`] at [`super::policy_gates::GATE_ORIGIN`]:
    ///
    /// * a sequence-specific rule with response `Execute` mints;
    /// * only a universal wildcard `Execute` rule (`response any`) falls back
    ///   to `write_authorized`, so broad profiles cannot reopen this
    ///   deny-by-default sink;
    /// * a rule with any other response refuses, whatever `write_authorized` says;
    /// * no matching rule, or no policy installed
    ///   (`BridgeDecision::Fallback`), falls back to `write_authorized`.
    ///
    /// The probe and origin are literals, so the verdict is resolved once per
    /// installed policy; `policy_bridge.rs` has the decision tree.
    pub(super) fn try_mint_write_capability_with_engine(
        &self,
        gates: PolicyGates,
    ) -> Option<ClipboardWriteCapability> {
        if gates.osc52_set().resolve(self.write_authorized) {
            Some(ClipboardWriteCapability { _seal: super::Seal })
        } else {
            None
        }
    }

    /// Engine-consulting variant of `Self::try_mint_query_capability`: the same
    /// rules as [`Self::try_mint_write_capability_with_engine`], against the
    /// `OSC 52 query` selector and `query_authorized`.
    pub(super) fn try_mint_query_capability_with_engine(
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
    use aterm_policy::{OriginTag, Policy, Response, profiles};

    fn policy_with_rule(sequence: &str, response: Response) -> Policy {
        crate::terminal::policy_bridge::one_rule_policy(
            sequence,
            response,
            OriginTag::Pty,
            Response::Drop,
        )
    }

    #[test]
    fn default_has_no_capabilities() {
        let auth = ClipboardAuth::new();
        assert!(auth.try_mint_write_capability().is_none());
        assert!(auth.try_mint_query_capability().is_none());
        assert!(!auth.is_write_authorized());
        assert!(!auth.is_query_authorized());
    }

    #[test]
    fn authorize_write_grants_only_write() {
        let mut auth = ClipboardAuth::new();
        auth.authorize_write();
        assert!(auth.try_mint_write_capability().is_some());
        assert!(auth.try_mint_query_capability().is_none());
        assert!(auth.is_write_authorized());
        assert!(!auth.is_query_authorized());
    }

    #[test]
    fn authorize_query_grants_only_query() {
        let mut auth = ClipboardAuth::new();
        auth.authorize_query();
        assert!(auth.try_mint_write_capability().is_none());
        assert!(auth.try_mint_query_capability().is_some());
        assert!(!auth.is_write_authorized());
        assert!(auth.is_query_authorized());
    }

    #[test]
    fn revoke_disables_future_mint() {
        let mut auth = ClipboardAuth::new();
        auth.authorize_write();
        auth.authorize_query();
        auth.revoke_write();
        auth.revoke_query();
        assert!(auth.try_mint_write_capability().is_none());
        assert!(auth.try_mint_query_capability().is_none());
    }

    #[test]
    fn write_and_query_are_independent() {
        let mut auth = ClipboardAuth::new();
        auth.authorize_write();
        auth.authorize_query();
        auth.revoke_write();
        assert!(auth.try_mint_write_capability().is_none());
        assert!(auth.try_mint_query_capability().is_some());
        auth.authorize_write();
        auth.revoke_query();
        assert!(auth.try_mint_write_capability().is_some());
        assert!(auth.try_mint_query_capability().is_none());
    }

    #[test]
    fn standard_profile_wildcard_execute_does_not_overgrant_revoked_clipboard_access() {
        let auth = ClipboardAuth::new();
        let engine = PolicyEngine::new(profiles::standard());

        assert!(
            auth.try_mint_write_capability_with_engine(PolicyGates::compile(Some(&engine)))
                .is_none()
        );
        assert!(
            auth.try_mint_query_capability_with_engine(PolicyGates::compile(Some(&engine)))
                .is_none()
        );
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

    #[test]
    fn invoke_set_delivers_content_via_callback() {
        use std::sync::{Arc, Mutex};
        let mut auth = ClipboardAuth::new();
        auth.authorize_write();
        let token = auth.try_mint_write_capability().expect("write authorized");

        let captured = Arc::new(Mutex::new(None::<String>));
        let captured_clone = Arc::clone(&captured);
        let mut slot: Option<ClipboardCallback> = Some(Box::new(move |op| {
            if let ClipboardOperation::Set { content, .. } = op {
                *captured_clone.lock().expect("poisoned") = Some(content);
            }
            None
        }));

        invoke_set(
            &mut slot,
            token,
            &[ClipboardSelection::Clipboard],
            "hello".to_string(),
        );

        assert_eq!(
            *captured.lock().expect("poisoned"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn invoke_query_returns_host_content() {
        let mut auth = ClipboardAuth::new();
        auth.authorize_query();
        let token = auth.try_mint_query_capability().expect("query authorized");
        let mut slot: Option<ClipboardCallback> =
            Some(Box::new(|_op| Some("clipboard".to_string())));
        let result = invoke_query(&mut slot, token, &[ClipboardSelection::Clipboard]);
        assert_eq!(result, Some("clipboard".to_string()));
    }

    #[test]
    fn invoke_query_returns_none_when_host_denies() {
        let mut auth = ClipboardAuth::new();
        auth.authorize_query();
        let token = auth.try_mint_query_capability().expect("query authorized");
        let mut slot: Option<ClipboardCallback> = Some(Box::new(|_op| None));
        let result = invoke_query(&mut slot, token, &[ClipboardSelection::Clipboard]);
        assert_eq!(result, None);
    }
}
