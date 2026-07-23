// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Clipboard-access capability authority — host-side authorization for
//! OSC 52 clipboard *set* and *query* operations.
//!
//! # The privilege-conflation problem
//!
//! OSC 52 lets a program write (`OSC 52 ; c ; <base64> ST`) and read
//! (`OSC 52 ; c ; ? ST`) the system clipboard. The parser delivers these
//! sequences through `ActionSink::osc_dispatch` like any other escape
//! sequence, so the clipboard callback in `Terminal::handle_osc_52_set`
//! and `Terminal::handle_osc_52_query` sees attacker-controlled bytes
//! (a `cat malicious.txt` from the PTY) as bit-identical to a legitimate
//! host-driven clipboard write. See
//! `reports/2026-04-18-privilege-conflation-audit.md` — **CF-004** and
//! **CF-005**.
//!
//! Prior state:
//! * OSC 52 *set* was **ungated** — any PTY-origin bytes could reach the
//!   host's clipboard delegate (CF-004).
//! * OSC 52 *query* was gated by a **runtime bool** (`allow_osc52_query`)
//!   that lived on `TerminalModes`. A refactor that moved the check or a
//!   new clipboard-query sequence that forgot to check it would silently
//!   re-open the hole (CF-005).
//!
//! # The structural fix — mirror of [`super::modal_auth`]
//!
//! Clipboard invocation sites no longer take a `&mut ClipboardCallback`
//! directly. They call [`ClipboardAuth::try_mint_write_capability`] or
//! [`ClipboardAuth::try_mint_query_capability`], each of which returns
//! `Option<ClipboardWriteCapability>` / `Option<ClipboardQueryCapability>`.
//! These types are **zero-sized with a private `_seal: ()`** — no code
//! outside this module can construct one.
//!
//! Authorization is set by the host:
//!
//! * [`ClipboardAuth::authorize_write`] / [`revoke_write`]
//! * [`ClipboardAuth::authorize_query`] / [`revoke_query`]
//!
//! Exposed on [`super::Terminal`] as
//! `authorize_clipboard_access` / `revoke_clipboard_access`. Like
//! `ModalProtocolAuth`, the `ClipboardAuth` field is reachable from the
//! parser handler (so [`try_mint_write_capability`] can be called from
//! inside `handle_osc_52_set`), but the **authorize/revoke** methods are
//! `pub(crate)` and never called from handler code — they are reached
//! only through the host-facing `Terminal::authorize_clipboard_access`
//! API. The parser therefore has no structural path to escalate itself
//! from "no authorization" to "has capability": the only transition is
//! through a host API call, which is the definition of the trust
//! boundary.
//!
//! # Why zero-sized capabilities (and not an enum returned by a gate
//! function)?
//!
//! The zero-sized token pattern matches `modal_auth` for consistency and
//! for the same reason: the token's lifetime statically flows from the
//! authorization check to the callback invocation. A future reviewer who
//! inlines the `if let Some(cap) = ...` block and forgets to check the
//! capability will get a compiler error (`cap` is unused); a future
//! handler that calls `clipboard.callback` directly (bypassing the
//! capability) will see the callback hidden behind a private helper that
//! demands a token.
//!
//! # Default posture
//!
//! Fresh terminals start with both capabilities **revoked**. Hosts must
//! opt in. This matches the existing `allow_osc52_query` default
//! (`false`) and tightens the previous-default OSC 52 *set*, which was
//! effectively "allowed iff a clipboard callback is set."

use super::callbacks::ClipboardCallback;
use super::policy_bridge::{BridgeDecision, engine_decision_deny_by_default_capability};
use super::types::{ClipboardOperation, ClipboardSelection};
use aterm_policy::{OriginTag, engine::PolicyEngine, selector::DispatchedSequence};

// ---------------------------------------------------------------------------
// Public API selector for host-facing `authorize_*` / `revoke_*` methods.
// ---------------------------------------------------------------------------

/// Selector for the two classes of OSC 52 clipboard access.
///
/// Used by host-facing APIs
/// [`super::Terminal::authorize_clipboard_access`] and
/// [`super::Terminal::revoke_clipboard_access`] to identify which
/// capability to grant or revoke. Parallel to
/// [`super::modal_auth::ModalProtocol`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardAccess {
    /// OSC 52 clipboard *set* / *clear* (write).
    ///
    /// Gates CF-004: attacker-controlled OSC 52 bytes writing to the
    /// host clipboard.
    Write,
    /// OSC 52 clipboard *query* (read).
    ///
    /// Gates CF-005: attacker-controlled OSC 52 query reading the host
    /// clipboard back through the PTY response stream. Formerly gated
    /// by the runtime bool `TerminalModes::allow_osc52_query`.
    Query,
}

// ---------------------------------------------------------------------------
// Capability tokens — zero-sized, unforgeable outside this module.
// ---------------------------------------------------------------------------

/// Capability for OSC 52 clipboard *set*.
///
/// Zero-sized. Its constructor is private to this module, so the only
/// way to obtain one is through [`ClipboardAuth::try_mint_write_capability`],
/// which requires the host to have previously called
/// [`ClipboardAuth::authorize_write`].
///
/// Consumed by value when passed to [`invoke_set`]; this prevents a
/// single authorization from being silently replayed if the token were
/// accidentally stashed (the Rust move semantics enforce it).
#[must_use = "ClipboardWriteCapability has no effect unless passed to invoke_set"]
pub(super) struct ClipboardWriteCapability {
    _seal: (),
}

impl ClipboardWriteCapability {
    /// Provenance ceremony: lift this clipboard-write capability into a
    /// [`aterm_provenance::HostAuthorizationToken`] borrowed for the
    /// capability's lifetime.
    ///
    /// Part of the #8001 `authorize_*` wiring (design §6 migration table).
    /// Holding a `ClipboardWriteCapability` proves that the host
    /// authorized OSC 52 clipboard *set* access at the dispatch frame;
    /// the returned token lets downstream `authorize_pty_to_host`
    /// consumers lift Pty-origin base64-decoded clipboard content into
    /// `Host`-origin.
    #[allow(
        dead_code,
        reason = "audit-only provenance ceremony retained until production callers consume the host-authorization token directly"
    )]
    #[must_use]
    pub(crate) fn as_host_auth_token(&self) -> aterm_provenance::HostAuthorizationToken<'_> {
        let _ = self;
        aterm_provenance::HostAuthorizationToken::__new_for_capability_only()
    }
}

/// Capability for OSC 52 clipboard *query*.
///
/// Parallel to [`ClipboardWriteCapability`]. Minted only through
/// [`ClipboardAuth::try_mint_query_capability`] after a host call to
/// [`ClipboardAuth::authorize_query`].
#[must_use = "ClipboardQueryCapability has no effect unless passed to invoke_query"]
pub(super) struct ClipboardQueryCapability {
    _seal: (),
}

impl ClipboardQueryCapability {
    /// Provenance ceremony: lift this clipboard-query capability into a
    /// [`aterm_provenance::HostAuthorizationToken`] borrowed for the
    /// capability's lifetime.
    ///
    /// Part of the #8001 `authorize_*` wiring (design §6 migration table).
    /// Holding a `ClipboardQueryCapability` proves that the host
    /// authorized OSC 52 clipboard *query* at the dispatch frame; the
    /// returned token lets downstream `authorize_pty_to_host` consumers
    /// lift Pty-origin query selectors into `Host`-origin so the host
    /// callback can treat them as policy-approved.
    #[allow(
        dead_code,
        reason = "audit-only provenance ceremony retained until production callers consume the host-authorization token directly"
    )]
    #[must_use]
    pub(crate) fn as_host_auth_token(&self) -> aterm_provenance::HostAuthorizationToken<'_> {
        let _ = self;
        aterm_provenance::HostAuthorizationToken::__new_for_capability_only()
    }
}

// ---------------------------------------------------------------------------
// Authorization state.
// ---------------------------------------------------------------------------

/// Host-side authorization state for OSC 52 clipboard access.
///
/// Lives on [`super::Terminal`]. Forwarded through [`super::TerminalHandler`]
/// so parser handlers can call `try_mint_*_capability`, but the
/// `authorize_*` / `revoke_*` methods are `pub(crate)` — handlers cannot
/// escalate their own authorization.
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

    /// Authorize OSC 52 clipboard *set* (write).
    ///
    /// After this call, subsequent PTY-origin `OSC 52 ; <selection> ; <base64> ST`
    /// sequences will mint a [`ClipboardWriteCapability`] and invoke the
    /// clipboard callback with the decoded content.
    pub(crate) fn authorize_write(&mut self) {
        self.write_authorized = true;
    }

    /// Revoke OSC 52 clipboard *set*. After this call, subsequent
    /// PTY-origin OSC 52 set sequences are silently dropped (no callback
    /// invocation) until [`authorize_write`] is called again.
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
    ///
    /// Read-only observer for host APIs / FFI. Does **not** mint a
    /// capability; for that, use [`try_mint_write_capability`].
    pub(crate) const fn is_write_authorized(&self) -> bool {
        self.write_authorized
    }

    /// Whether OSC 52 clipboard *query* is currently authorized.
    pub(crate) const fn is_query_authorized(&self) -> bool {
        self.query_authorized
    }

    /// Attempt to mint a write capability. Returns `Some` iff
    /// [`authorize_write`] has been called and not since revoked.
    ///
    /// This is the only way a handler can obtain a
    /// [`ClipboardWriteCapability`], and the capability is the only
    /// argument that unlocks [`invoke_set`]. A handler that bypasses
    /// this and tries to call the callback directly will find the
    /// callback field is not part of its public API surface (it lives
    /// behind [`invoke_set`] / [`invoke_query`] / [`invoke_clear`]).
    #[allow(
        dead_code,
        reason = "#7994: retained as the non-engine fallback API; all OSC 52 set handlers now route through try_mint_write_capability_with_engine per design §6.3 (Release N). Kept for post-migration simplification."
    )]
    pub(super) fn try_mint_write_capability(&self) -> Option<ClipboardWriteCapability> {
        if self.write_authorized {
            Some(ClipboardWriteCapability { _seal: () })
        } else {
            None
        }
    }

    /// Attempt to mint a query capability. Returns `Some` iff
    /// [`authorize_query`] has been called and not since revoked.
    #[allow(
        dead_code,
        reason = "#7994: retained as the non-engine fallback API; all OSC 52 query handlers now route through try_mint_query_capability_with_engine per design §6.3 (Release N). Kept for post-migration simplification."
    )]
    pub(super) fn try_mint_query_capability(&self) -> Option<ClipboardQueryCapability> {
        if self.query_authorized {
            Some(ClipboardQueryCapability { _seal: () })
        } else {
            None
        }
    }

    /// Engine-consulting variant of [`Self::try_mint_write_capability`]
    /// (#7994). Consults the [`PolicyEngine`] first with an `OSC 52 set`
    /// probe at the given `origin`:
    ///
    /// * Engine matches a sequence-specific rule whose response is
    ///   `Execute` → mint allowed.
    /// * Engine matches only a universal wildcard `Execute` rule
    ///   (`response any`) → falls back to the legacy `write_authorized`
    ///   bool so broad profiles cannot silently reopen this deny-by-default
    ///   sink.
    /// * Engine matches a rule with any other response → mint denied
    ///   (fail-closed, regardless of the legacy `authorize_write` state).
    /// * Engine falls through to `defaults.unmatched` (no matching rule)
    ///   → fall back to the legacy `write_authorized` bool. This is the
    ///   Release N backward-compat guarantee from design §6.2/§6.3.
    ///
    /// When `engine` is `None` (host has not installed a policy) the
    /// behavior is identical to [`Self::try_mint_write_capability`].
    ///
    /// See `terminal/policy_bridge.rs` for the decision tree.
    pub(super) fn try_mint_write_capability_with_engine(
        &self,
        engine: Option<&PolicyEngine>,
        origin: OriginTag,
    ) -> Option<ClipboardWriteCapability> {
        let seq = probe_osc52_set();
        if allow(
            engine_decision_deny_by_default_capability(engine, &seq, origin),
            self.write_authorized,
        ) {
            Some(ClipboardWriteCapability { _seal: () })
        } else {
            None
        }
    }

    /// Engine-consulting variant of [`Self::try_mint_query_capability`]
    /// (#7994). Same bridge semantics as
    /// [`Self::try_mint_write_capability_with_engine`] but against the
    /// `OSC 52 query` selector and the `query_authorized` legacy bool.
    /// Wildcard `Execute` rules likewise fall back instead of overgranting.
    pub(super) fn try_mint_query_capability_with_engine(
        &self,
        engine: Option<&PolicyEngine>,
        origin: OriginTag,
    ) -> Option<ClipboardQueryCapability> {
        let seq = probe_osc52_query();
        if allow(
            engine_decision_deny_by_default_capability(engine, &seq, origin),
            self.query_authorized,
        ) {
            Some(ClipboardQueryCapability { _seal: () })
        } else {
            None
        }
    }
}

/// Probe sequence used by the OSC 52 **set** policy lookup.
///
/// The engine's selector matcher treats the second param as a Base64
/// payload for the `OSC 52 set` alias (anything that is not literal
/// `?`). We use `SGVsbG8=` as the canonical representative — the exact
/// content doesn't matter, only that the alias matches.
#[inline]
fn probe_osc52_set() -> DispatchedSequence {
    DispatchedSequence::osc(52, [String::from("c"), String::from("SGVsbG8=")])
}

/// Probe sequence used by the OSC 52 **query** policy lookup. The second
/// param is literal `?`, which selects the `OSC 52 query` alias bucket.
#[inline]
fn probe_osc52_query() -> DispatchedSequence {
    DispatchedSequence::osc(52, [String::from("c"), String::from("?")])
}

#[inline]
fn allow(decision: BridgeDecision, legacy_allow: bool) -> bool {
    decision.resolve(legacy_allow)
}

// ---------------------------------------------------------------------------
// Callback invokers — the only way to reach `ClipboardCallback` from a
// handler.
// ---------------------------------------------------------------------------

/// Invoke the host clipboard callback with a *set* operation.
///
/// The capability token is consumed by value. If the callback is
/// unwired, this is a no-op (same behavior as the pre-capability code:
/// a clipboard write with no host delegate is silently dropped). A
/// clear consequence of the `_token` parameter is that static analysis
/// of the call-graph can prove the callback is reached iff a capability
/// was minted, which is iff `ClipboardAuth::authorize_write` was called
/// through the host API.
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

/// Invoke the host clipboard callback with a *clear* operation.
///
/// Clear is **not** capability-gated separately. The policy choice here
/// is that a terminal which has authorized write implicitly authorizes
/// clear (clear is less dangerous than set — the attacker can only
/// empty the clipboard, not inject arbitrary content). We therefore
/// take a [`ClipboardWriteCapability`] rather than introducing a third
/// token type. Hosts that want to block clear entirely can revoke write
/// authorization; hosts that want to allow clear but not set can
/// enforce that in the callback itself.
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
    use aterm_policy::{
        Defaults, OriginTag, Policy, Profile, Response, Rule, SCHEMA_VERSION, profiles,
    };

    fn policy_with_rule(sequence: &str, response: Response) -> Policy {
        Policy {
            schema_version: SCHEMA_VERSION,
            profile: Profile::Standard,
            defaults: Defaults {
                unmatched: Response::Drop,
                shell_integration_require_nonce: false,
            },
            rules: vec![Rule {
                sequence: sequence.to_owned(),
                origin_min: OriginTag::Pty,
                response,
                rate_limit: None,
                prompt_id: None,
            }],
            rate_limits: vec![],
        }
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
            auth.try_mint_write_capability_with_engine(Some(&engine), OriginTag::Pty)
                .is_none()
        );
        assert!(
            auth.try_mint_query_capability_with_engine(Some(&engine), OriginTag::Pty)
                .is_none()
        );
    }

    #[test]
    fn explicit_osc52_set_rule_still_allows_revoked_write_access() {
        let auth = ClipboardAuth::new();
        let engine = PolicyEngine::new(policy_with_rule("OSC 52 set", Response::Execute));

        assert!(
            auth.try_mint_write_capability_with_engine(Some(&engine), OriginTag::Pty)
                .is_some()
        );
    }

    #[test]
    fn explicit_osc52_query_rule_still_allows_revoked_query_access() {
        let auth = ClipboardAuth::new();
        let engine = PolicyEngine::new(policy_with_rule("OSC 52 query", Response::Execute));

        assert!(
            auth.try_mint_query_capability_with_engine(Some(&engine), OriginTag::Pty)
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

    /// #8001 ceremony: `ClipboardWriteCapability::as_host_auth_token`
    /// produces a token that lifts Pty-origin data to Host-origin.
    #[test]
    fn write_as_host_auth_token_lifts_pty_to_host() {
        use aterm_provenance::{OriginTag, Provenance, authorize_pty_to_host};

        let mut auth = ClipboardAuth::new();
        auth.authorize_write();
        let cap = auth.try_mint_write_capability().expect("write authorized");
        let tok = cap.as_host_auth_token();
        let pty: Provenance<String, aterm_provenance::Pty> =
            Provenance::from_pty("hello".to_string());
        let host = authorize_pty_to_host(pty, tok);
        assert_eq!(host.tag(), OriginTag::Host);
        assert_eq!(host.as_ref(), "hello");
    }

    /// #8001 ceremony: `ClipboardQueryCapability::as_host_auth_token`
    /// produces a token that lifts Pty-origin data to Host-origin.
    #[test]
    fn query_as_host_auth_token_lifts_pty_to_host() {
        use aterm_provenance::{OriginTag, Provenance, authorize_pty_to_host};

        let mut auth = ClipboardAuth::new();
        auth.authorize_query();
        let cap = auth.try_mint_query_capability().expect("query authorized");
        let tok = cap.as_host_auth_token();
        let pty: Provenance<Vec<ClipboardSelection>, aterm_provenance::Pty> =
            Provenance::from_pty(vec![ClipboardSelection::Clipboard]);
        let host = authorize_pty_to_host(pty, tok);
        assert_eq!(host.tag(), OriginTag::Host);
        assert_eq!(host.as_ref().len(), 1);
    }
}
