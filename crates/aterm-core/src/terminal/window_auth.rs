// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Window-operations capability: a structural gate on XTWINOPS dispatch.
//!
//! XTWINOPS (`CSI t`) dispatches attacker-chosen window geometry/state
//! operations to a host callback
//! ([`super::handler::TerminalHandler::invoke_window_callback`]). That function
//! takes a `&WindowOpsCapability`, so a handler that forgets the policy check
//! has no token to pass and does not compile.
//!
//! [`WindowOpsCapability`] is a zero-sized token whose only constructor is
//! [`WindowMintAuthority::try_mint_with_engine`] (`pub(super)`). It mints iff the
//! policy gate for the dispatched `Ps`
//! ([`super::policy_gates::PolicyState::xtwinops_gate`]) resolves open: a
//! sequence-specific `Execute` rule mints even with `allow_window_ops` false, a
//! rule with any other response refuses even with it true, and
//! [`TerminalModes::allow_window_ops`] decides when no rule matched, only a
//! universal wildcard `Execute` rule did, or no policy is installed (the
//! default). The parser's `ActionSink` path cannot reach the mint.
//!
//! Without the capability the callback call sites are unreachable; the
//! title-stack sub-operations (22/23) do not invoke the callback and always run.
//! Two consumers mint: the XTWINOPS dispatch (`handler_window.rs`) and the
//! XTSMGRAPHICS sixel-geometry read (`Pi=2, Pa=1`, `handler_xtsmgraphics.rs`),
//! which reports the same pixel size as `CSI 14 t` and rides the `Ps = 14` gate.
//!
//! `response_capability` is orthogonal: it proves a caller is inside a
//! parser-originated sequence that may respond, and XTWINOPS reports usually
//! need both tokens.

/// Zero-sized proof that the calling context may invoke the window callback
/// for an XTWINOPS dispatch.
///
/// Minted only by [`WindowMintAuthority::try_mint_with_engine`]; the private
/// field prevents construction elsewhere. Passed by reference so several
/// callback calls in one dispatch share one token.
#[derive(Debug)]
pub(super) struct WindowOpsCapability {
    /// Private seal: prevents construction outside this module.
    _seal: (),
}

/// Zero-sized minting authority for [`WindowOpsCapability`].
///
/// Holds no policy; [`Self::try_mint_with_engine`] consults the gate and the
/// `allow_window_ops` fallback passed in, and is the one place a capability
/// comes into existence.
#[derive(Debug, Default)]
pub(super) struct WindowMintAuthority {
    _seal: (),
}

impl WindowMintAuthority {
    /// Construct the authority (`pub(super)`: terminal module only). It is a
    /// namespace for the mint, not shared state.
    #[inline]
    #[must_use]
    pub(super) const fn new() -> Self {
        Self { _seal: () }
    }

    /// Mint a [`WindowOpsCapability`] from the resolved policy gate:
    ///
    /// * a sequence-specific rule with response `Execute` mints;
    /// * only a universal wildcard `Execute` rule (`response any`) falls back
    ///   to `allow_window_ops`, so broad profiles cannot reopen this
    ///   deny-by-default sink;
    /// * a rule with any other response refuses, whatever `allow_window_ops` says;
    /// * no matching rule (or no policy installed:
    ///   [`super::policy_bridge::BridgeDecision::Fallback`]) falls back to
    ///   `allow_window_ops`.
    ///
    /// The caller passes a decision rather than an engine because the probe
    /// ([`probe_xtwinops`]) varies only in `ps` and the origin is always
    /// `OriginTag::Pty`, so [`super::policy_gates::PolicyState::xtwinops_gate`]
    /// resolves every `ps` once per installed policy and no dispatch allocates
    /// or walks rule buckets. `terminal/policy_bridge.rs` has the decision tree.
    #[inline]
    #[must_use]
    pub(super) fn try_mint_with_engine(
        &self,
        gate: super::policy_bridge::BridgeDecision,
        allow_window_ops: bool,
    ) -> Option<WindowOpsCapability> {
        let _ = self;
        if gate.resolve(allow_window_ops) {
            Some(WindowOpsCapability { _seal: () })
        } else {
            None
        }
    }
}

/// Probe sequence used by the XTWINOPS (`CSI Ps t`) policy lookup.
///
/// The single definition, shared by the compiled gate table in
/// [`super::policy_gates`] and by its out-of-range live fallback, so the two
/// cannot disagree about which rule bucket a `Ps` lands in.
#[must_use]
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

    /// The capability and authority are zero-sized: the gate costs nothing at runtime.
    #[test]
    fn capability_and_authority_are_zero_sized() {
        assert_eq!(std::mem::size_of::<WindowOpsCapability>(), 0);
        assert_eq!(std::mem::size_of::<WindowMintAuthority>(), 0);
    }

    /// Minting is a pure function of the gate and the fallback bool.
    #[test]
    fn minting_is_deterministic_in_policy_bit() {
        let auth = WindowMintAuthority::new();
        assert_eq!(
            auth.try_mint_with_engine(Fallback, false).is_some(),
            auth.try_mint_with_engine(Fallback, false).is_some()
        );
        assert_eq!(
            auth.try_mint_with_engine(Fallback, true).is_some(),
            auth.try_mint_with_engine(Fallback, true).is_some()
        );
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
