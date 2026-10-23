// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Structural capability gate on XTWINOPS (`CSI t`) host callback dispatch.

/// Proof that the caller may invoke the window callback for an XTWINOPS dispatch.
#[derive(Debug)]
pub(super) struct WindowOpsCapability {
    /// Private seal: no construction outside this module.
    _seal: super::Seal,
}

/// Minting authority for [`WindowOpsCapability`].
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

    /// Mint a [`WindowOpsCapability`] when the policy gate allows, or on fallback when
    /// `allow_window_ops` does.
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

/// Build the XTWINOPS (`CSI Ps t`) policy selector probe.
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

    #[test]
    fn disallowed_policy_mints_no_capability() {
        let auth = WindowMintAuthority::new();
        assert!(auth.try_mint_with_engine(Fallback, false).is_none());
    }

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
