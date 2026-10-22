// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Window-operations capability: a structural gate on XTWINOPS dispatch.
//!
//! XTWINOPS (`CSI t`) hands attacker-chosen window operations to a host callback
//! ([`super::handler::TerminalHandler::invoke_window_callback`]) that takes a
//! `&WindowOpsCapability`, so a handler that skips the policy check has no token to pass
//! and does not compile. The only constructor is
//! [`WindowMintAuthority::try_mint_with_engine`], which the parser's `ActionSink` path
//! cannot reach. The title-stack sub-operations (22/23) do not invoke the callback and
//! always run.
//!
//! Two consumers mint: the XTWINOPS dispatch (`handler_window.rs`) and the XTSMGRAPHICS
//! sixel-geometry read (`Pi=2, Pa=1`, `handler_xtsmgraphics.rs`), which reports the same
//! pixel size as `CSI 14 t` and rides the `Ps = 14` gate. XTWINOPS reports usually also
//! need a `response_capability` token.

/// Zero-sized proof that the calling context may invoke the window callback for an
/// XTWINOPS dispatch. Passed by reference, so several callback calls in one dispatch
/// share one token.
#[derive(Debug)]
pub(super) struct WindowOpsCapability {
    /// Private seal: no construction outside this module.
    _seal: super::Seal,
}

/// Zero-sized minting authority for [`WindowOpsCapability`]: holds no policy, and
/// [`Self::try_mint_with_engine`] is the one place a capability comes into existence.
#[derive(Debug, Default)]
pub(super) struct WindowMintAuthority {
    _seal: (),
}

impl WindowMintAuthority {
    /// Construct the authority: a namespace for the mint, not shared state.
    #[inline]
    #[must_use]
    pub(super) const fn new() -> Self {
        Self { _seal: () }
    }

    /// Mint a [`WindowOpsCapability`] from the resolved policy gate:
    ///
    /// * a sequence-specific rule with response `Execute` mints;
    /// * a rule with any other response refuses, whatever `allow_window_ops` says;
    /// * only a universal wildcard `Execute` rule (`response any`), no matching rule, or
    ///   no policy ([`super::policy_bridge::BridgeDecision::Fallback`]) falls back to
    ///   `allow_window_ops`, so broad profiles cannot reopen this deny-by-default sink.
    ///
    /// The caller passes a decision, not an engine: the probe ([`probe_xtwinops`]) varies
    /// only in `ps` and the origin is always `OriginTag::Pty`, so
    /// [`super::policy_gates::PolicyState::xtwinops_gate`] resolves every `ps` once per
    /// installed policy and no dispatch allocates or walks rule buckets.
    #[inline]
    #[must_use]
    pub(super) const fn try_mint_with_engine(
        &self,
        gate: super::policy_bridge::BridgeDecision,
        allow_window_ops: bool,
    ) -> Option<WindowOpsCapability> {
        let _ = self;
        if gate.resolve(allow_window_ops) {
            Some(WindowOpsCapability { _seal: super::Seal })
        } else {
            None
        }
    }
}

/// The XTWINOPS (`CSI Ps t`) policy probe, shared by the compiled gate table in
/// [`super::policy_gates`] and its out-of-range live fallback so the two cannot disagree
/// about which rule bucket a `Ps` lands in.
#[must_use]
#[inline]
pub(super) fn probe_xtwinops(ps: u16) -> aterm_policy::selector::DispatchedSequence {
    aterm_policy::selector::DispatchedSequence::csi(Some(u32::from(ps)), 't', [])
}

#[cfg(test)]
mod tests {
    use super::super::policy_bridge::BridgeDecision::Fallback;
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

    /// With no policy rule, `allow_window_ops = false` mints nothing.
    #[test]
    fn disallowed_policy_mints_no_capability() {
        let auth = WindowMintAuthority::new();
        assert!(auth.try_mint_with_engine(Fallback, false).is_none());
    }

    /// With no policy rule, `allow_window_ops = true` mints.
    #[test]
    fn allowed_policy_mints_capability() {
        let auth = WindowMintAuthority::new();
        assert!(auth.try_mint_with_engine(Fallback, true).is_some());
    }

    #[test]
    fn standard_profile_wildcard_execute_does_not_overgrant_when_legacy_bool_is_false() {
        let auth = WindowMintAuthority::new();
        let engine = PolicyEngine::new(profiles::standard());

        assert!(
            auth.try_mint_with_engine(
                super::super::policy_gates::xtwinops_verdict(Some(&engine), 21),
                false
            )
            .is_none()
        );
    }

    #[test]
    fn explicit_xtwinops_rule_still_allows_when_legacy_bool_is_false() {
        let auth = WindowMintAuthority::new();
        let engine = PolicyEngine::new(policy_with_rule("CSI 21 t", Response::Execute));

        assert!(
            auth.try_mint_with_engine(
                super::super::policy_gates::xtwinops_verdict(Some(&engine), 21),
                false
            )
            .is_some()
        );
    }
}
