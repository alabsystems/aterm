// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Window-operations capability — structural gate on XTWINOPS dispatch.
//!
//! # Why this module exists
//!
//! Privilege-conflation finding CF-008 (see
//! [`reports/2026-04-18-privilege-conflation-audit.md`]) identified the
//! XTWINOPS (`CSI t`) handler as a sink that dispatches attacker-chosen
//! window geometry/state operations to a host-installed callback
//! ([`super::handler::TerminalHandler::invoke_window_callback`]). Whether
//! a given PTY-origin sequence is allowed to reach the callback is
//! distinguished only by the runtime boolean [`TerminalModes::allow_window_ops`].
//!
//! A point-patch in #7876 (CSI 20t / 21t bypass of the deny branch) showed
//! the antipattern from [design doc] — "a new sequence is one forgotten
//! `if !allow_window_ops` check away from reopening the class". The
//! structural fix is to make the *type* of `invoke_window_callback`
//! refuse to compile when the caller did not first prove the host is
//! authorizing window ops.
//!
//! # The structural gate
//!
//! [`WindowOpsCapability`] is a zero-sized token whose only constructor
//! is [`WindowMintAuthority::try_mint_with_engine`], a `pub(super)`-scoped
//! function that returns `Some(WindowOpsCapability)` iff the POLICY GATE it
//! is handed resolves open: an installed policy's verdict for the `CSI Ps t`
//! probe decides first (a sequence-specific `Execute` rule mints even with
//! `allow_window_ops` false; a rule with any other response refuses even with
//! it true), and `allow_window_ops` decides only when no rule matched, when
//! only a universal wildcard `Execute` rule did, or when no policy is
//! installed. The minting authority is itself a zero-sized unit struct and
//! has no runtime state — it exists solely to funnel that decision through a
//! single typed choke-point.
//!
//! Every call site that reaches
//! [`super::handler::TerminalHandler::invoke_window_callback`] must
//! receive a `&WindowOpsCapability`. Because the type's internal field
//! is private and the constructor is `pub(super)`, no code outside the
//! terminal module can name — much less construct — a capability. The
//! parser data path (`ActionSink` trait in [`crate::parser`]) cannot
//! reach [`WindowMintAuthority::try_mint_with_engine`]: the trait only gives access
//! to `&mut dyn ActionSink`, which does not expose this type.
//!
//! # Semantics
//!
//! The mint takes two inputs: the policy gate for the dispatched `Ps`
//! ([`super::policy_gates::PolicyState::xtwinops_gate`], resolved once per
//! installed policy) and the host's `allow_window_ops` boolean, which is the
//! fallback the gate resolves to when policy does not decide (see
//! [`WindowMintAuthority::try_mint_with_engine`] for the full table). With no
//! policy installed — the shipping default — the capability exists iff
//! `allow_window_ops` is true. When it is minted, XTWINOPS subcommands 1–21
//! proceed per the handler logic; when it is not, the capability-gated code
//! paths (the `invoke_window_callback` call sites) are structurally
//! unreachable. The title-stack sub-operations (22/23), which do not invoke
//! the callback, run regardless.
//!
//! Two consumers mint: the XTWINOPS dispatch itself (`handler_window.rs`,
//! gate for the dispatched `Ps`), and the XTSMGRAPHICS sixel-geometry read
//! (`Pi=2, Pa=1`, `handler_xtsmgraphics.rs`), which reports the same text-area
//! pixel size `CSI 14 t` does and so rides the `Ps = 14` gate: without the
//! capability it answers the maximum dimension.
//!
//! # Relation to other capabilities
//!
//! - [`super::modal_auth`] — nonce-gated activation tokens for DCS
//!   modal protocols (SSH conductor, tmux control). That module is the
//!   template for capability-as-argument; this module reuses the shape
//!   but without a nonce because `allow_window_ops` is a host-policy
//!   boolean (not a claim from the PTY).
//! - `response_capability` — dispatch-scoped token proving a caller
//!   is inside a parser-originated sequence that may produce a
//!   response. Orthogonal: XTWINOPS reports typically need both tokens.

/// Zero-sized proof that the calling context is authorized to invoke
/// the window callback for an XTWINOPS dispatch.
///
/// Minted only by [`WindowMintAuthority::try_mint_with_engine`] when the
/// policy gate resolves open (`allow_window_ops` when policy does not
/// decide). Consumers outside the terminal module cannot construct one; the
/// type's internal field is private.
///
/// Required by [`super::handler::TerminalHandler::invoke_window_callback`]
/// (after the CF-008 refactor). Passed by shared reference so multiple
/// `invoke_window_callback` calls in one `handle_xtwinops` dispatch can
/// share a single token without ownership transfer.
#[derive(Debug)]
pub(super) struct WindowOpsCapability {
    /// Private seal — prevents construction outside this module.
    ///
    /// Matches the pattern from `ConductorActivationToken` /
    /// `TmuxActivationToken` / `ResponseCapability`: a private unit
    /// field forces consumers to go through the module's
    /// visibility-gated constructor.
    _seal: (),
}

/// Zero-sized minting authority for [`WindowOpsCapability`].
///
/// Held implicitly by the terminal module (no field on
/// [`super::Terminal`] is required since the authority has no state).
/// Its [`Self::try_mint_with_engine`] is the single entry point through which a
/// capability can come into existence; by limiting the constructors
/// to this location, the audit surface for "who can talk to the
/// window callback" collapses to one function.
///
/// The authority is itself a ZST to emphasize that it does not hold
/// policy — it *consults* policy (the gate and the `allow_window_ops`
/// fallback passed in) and produces a capability iff that policy says yes.
#[derive(Debug, Default)]
pub(super) struct WindowMintAuthority {
    _seal: (),
}

impl WindowMintAuthority {
    /// Construct the authority.
    ///
    /// `pub(super)` so only code in the terminal module can obtain an
    /// authority. In practice callers construct a fresh authority inside
    /// the XTWINOPS dispatch frame — the authority is a namespace for
    /// the mint operation, not shared state.
    #[inline]
    #[must_use]
    pub(super) const fn new() -> Self {
        Self { _seal: () }
    }

    /// Mint a [`WindowOpsCapability`] (#7994) — the one mint, so a new
    /// XTWINOPS-class handler that forgets to consult it has no capability to
    /// hand `invoke_window_callback` and does not compile.
    ///
    /// Consults the [`aterm_policy::engine::PolicyEngine`] first with a
    /// `CSI t` (XTWINOPS) probe at the given `origin`. Behavior:
    ///
    /// * Engine matches a sequence-specific rule whose response is
    ///   `Execute` → capability minted.
    /// * Engine matches only a universal wildcard `Execute` rule
    ///   (`response any`) → falls back to the legacy `allow_window_ops`
    ///   bool so broad profiles cannot silently reopen this deny-by-default
    ///   sink.
    /// * Engine matches a rule with any other response → returns `None`,
    ///   regardless of the legacy `allow_window_ops` bool (fail-closed).
    /// * Engine falls through to `defaults.unmatched` → falls back to the
    ///   legacy `allow_window_ops` bool. This is the design-§6.3 Release N
    ///   backward-compat guarantee.
    ///
    /// With no policy installed, `gate` is [`super::policy_bridge::BridgeDecision::Fallback`]
    /// and the capability exists iff `allow_window_ops` is `true`.
    ///
    /// # Why the caller passes a decision instead of an engine
    ///
    /// The probe is [`probe_xtwinops`], whose only variable is `ps`, and the
    /// production origin is the literal `OriginTag::Pty`. The verdict for every
    /// `ps` xterm defines is therefore resolved once per installed policy by
    /// [`super::policy_gates::PolicyState::xtwinops_gate`], which builds it from
    /// THIS module's probe constructor. That kills a `char::to_string()` heap
    /// allocation and two bucket walks per `CSI t` dispatch — a cost the old
    /// code paid unconditionally, before even testing whether an engine was
    /// installed and before the `allow_window_ops` bool that drops ps 1..=21 on
    /// the floor in the shipping default configuration.
    ///
    /// See `terminal/policy_bridge.rs` for the decision tree.
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

    /// With no policy rule, `allow_window_ops = false` mints nothing. This is the structural mirror
    /// of the existing `if !self.modes.allow_window_ops { return ... }`
    /// deny branch in [`super::handler_window`]: without the policy bit,
    /// no capability exists, so no call site can reach
    /// `invoke_window_callback`.
    #[test]
    fn disallowed_policy_mints_no_capability() {
        let auth = WindowMintAuthority::new();
        assert!(auth.try_mint_with_engine(Fallback, false).is_none());
    }

    /// With no policy rule, `allow_window_ops = true` mints. When the host has opted into
    /// window operations, the capability is freely constructible — the
    /// gate is strictly an encoding of the existing boolean, not an
    /// additional runtime check.
    #[test]
    fn allowed_policy_mints_capability() {
        let auth = WindowMintAuthority::new();
        assert!(auth.try_mint_with_engine(Fallback, true).is_some());
    }

    /// The capability and authority are both zero-sized, so the
    /// capability argument threaded through `invoke_window_callback`
    /// adds no runtime cost — only a type-level obligation.
    #[test]
    fn capability_and_authority_are_zero_sized() {
        assert_eq!(std::mem::size_of::<WindowOpsCapability>(), 0);
        assert_eq!(std::mem::size_of::<WindowMintAuthority>(), 0);
    }

    /// Minting is a pure function of its explicit inputs (the policy gate
    /// and the `allow_window_ops` fallback): repeated calls with the same
    /// inputs produce the same outcome (either both `Some` or both `None`).
    /// This documents that [`WindowMintAuthority`] holds no hidden state.
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
