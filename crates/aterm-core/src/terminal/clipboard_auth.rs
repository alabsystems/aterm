// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Clipboard-access capability authority: host-side authorization for OSC 52
//! clipboard set and query.
//!
//! OSC 52 lets a program write (`OSC 52 ; c ; <base64> ST`) and read
//! (`OSC 52 ; c ; ? ST`) the system clipboard, and PTY bytes (a `cat` of a
//! hostile file) look identical to a legitimate write. So the clipboard
//! callback is reachable only through a zero-sized capability
//! ([`ClipboardWriteCapability`] / [`ClipboardQueryCapability`], private
//! `_seal`) minted by `ClipboardAuth::try_mint_*_capability`.
//!
//! The host grants and revokes access with [`ClipboardAuth::authorize_write`],
//! [`ClipboardAuth::authorize_query`] and their `revoke_*` counterparts,
//! exposed as `Terminal::authorize_clipboard_access` /
//! `revoke_clipboard_access`. Handlers can mint but never authorize (those
//! methods are `pub(crate)` and called only from the host API), so the parser
//! has no path from "no authorization" to "has capability".
//!
//! A handler that skips the mint has no token to pass, and the callback sits
//! behind helpers ([`invoke_set`], [`invoke_query`], [`invoke_clear`]) that
//! demand one. Fresh terminals start with both capabilities revoked.
//!
//! That is the engine's posture, not aterm's shipped one: the native GUI
//! (`aterm-gui`'s `configure_clipboard`) authorizes write for every tab, so a
//! program's OSC 52 set reaches the system clipboard with no prompt (as in kitty
//! and WezTerm), and authorizes query only when the user sets `allow_osc52_query`.
//! The standard policy's `OSC 52 set` rule requires `User` origin, so PTY bytes
//! fall to `BridgeDecision::Fallback` and the host's bit decides.

use super::callbacks::ClipboardCallback;
use super::policy_gates::PolicyGates;
use super::types::{ClipboardOperation, ClipboardSelection};
use aterm_policy::selector::DispatchedSequence;

// ---------------------------------------------------------------------------
// Public API selector for host-facing `authorize_*` / `revoke_*` methods.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Capability tokens — zero-sized, unforgeable outside this module.
// ---------------------------------------------------------------------------

/// Capability for OSC 52 clipboard set.
///
/// Zero-sized; minted only by `ClipboardAuth::try_mint_write_capability` after
/// [`ClipboardAuth::authorize_write`]. Consumed by value by [`invoke_set`], so
/// one authorization cannot be replayed from a stashed token.
#[must_use = "ClipboardWriteCapability has no effect unless passed to invoke_set"]
pub(super) struct ClipboardWriteCapability {
    _seal: (),
}

/// Capability for OSC 52 clipboard query, minted only by
/// `ClipboardAuth::try_mint_query_capability` after
/// [`ClipboardAuth::authorize_query`].
#[must_use = "ClipboardQueryCapability has no effect unless passed to invoke_query"]
pub(super) struct ClipboardQueryCapability {
    _seal: (),
}

// ---------------------------------------------------------------------------
// Authorization state.
// ---------------------------------------------------------------------------

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
    /// [`authorize_write`] is called again.
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

    /// Mint a write capability iff [`authorize_write`] was called and not
    /// since revoked.
    #[cfg(test)]
    pub(super) fn try_mint_write_capability(&self) -> Option<ClipboardWriteCapability> {
        if self.write_authorized {
            Some(ClipboardWriteCapability { _seal: () })
        } else {
            None
        }
    }

    /// Mint a query capability iff [`authorize_query`] was called and not
    /// since revoked.
    #[cfg(test)]
    pub(super) fn try_mint_query_capability(&self) -> Option<ClipboardQueryCapability> {
        if self.query_authorized {
            Some(ClipboardQueryCapability { _seal: () })
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
    /// installed policy. `terminal/policy_bridge.rs` has the decision tree.
    pub(super) fn try_mint_write_capability_with_engine(
        &self,
        gates: PolicyGates,
    ) -> Option<ClipboardWriteCapability> {
        if gates.osc52_set().resolve(self.write_authorized) {
            Some(ClipboardWriteCapability { _seal: () })
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
            Some(ClipboardQueryCapability { _seal: () })
        } else {
            None
        }
    }
}

/// Probe sequence for the OSC 52 set policy lookup.
///
/// Any second param other than `?` matches the `OSC 52 set` alias, so
/// `SGVsbG8=` stands in for every payload. `pub(super)` so
/// [`super::policy_gates`] compiles the gate from this one definition.
#[inline]
pub(super) fn probe_osc52_set() -> DispatchedSequence {
    DispatchedSequence::osc(52, [String::from("c"), String::from("SGVsbG8=")])
}

/// Probe sequence used by the OSC 52 **query** policy lookup. The second
/// param is literal `?`, which selects the `OSC 52 query` alias bucket.
///
/// Constant, and `pub(super)`, for the same reason as [`probe_osc52_set`].
#[inline]
pub(super) fn probe_osc52_query() -> DispatchedSequence {
    DispatchedSequence::osc(52, [String::from("c"), String::from("?")])
}

// ---------------------------------------------------------------------------
// Callback invokers — the only way to reach `ClipboardCallback` from a
// handler.
// ---------------------------------------------------------------------------

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

/// Invoke the host clipboard callback with a *query* operation.
///
/// Returns the host-provided content (or `None` if the host denies /
/// has no content / no callback is wired).
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

/// Invoke the host clipboard callback with a clear operation.
///
/// Clear takes the write capability rather than a token of its own: it can
/// only empty the clipboard. Hosts that want clear but not set enforce that
/// in the callback.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
        // Revoking write must not touch query.
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
    fn invoke_set_no_callback_is_noop() {
        let mut auth = ClipboardAuth::new();
        auth.authorize_write();
        let token = auth.try_mint_write_capability().expect("write authorized");
        let mut slot: Option<ClipboardCallback> = None;
        invoke_set(
            &mut slot,
            token,
            &[ClipboardSelection::Clipboard],
            "data".to_string(),
        );
        // No panic, no observable state — the callback is unwired.
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

    #[test]
    fn invoke_clear_takes_write_capability() {
        let mut auth = ClipboardAuth::new();
        auth.authorize_write();
        let token = auth.try_mint_write_capability().expect("write authorized");
        let mut slot: Option<ClipboardCallback> = None;
        invoke_clear(&mut slot, token, &[ClipboardSelection::Clipboard]);
    }
}
