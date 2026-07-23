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
//! is [`WindowMintAuthority::try_mint`], a `pub(super)`-scoped function
//! that returns `Some(WindowOpsCapability)` iff the caller passed
//! `allow_window_ops = true`. The minting authority is itself a
//! zero-sized unit struct and has no runtime state — it exists solely
//! to funnel the bool through a single typed choke-point.
//!
//! Every call site that reaches
//! [`super::handler::TerminalHandler::invoke_window_callback`] must
//! receive a `&WindowOpsCapability`. Because the type's internal field
//! is private and the constructor is `pub(super)`, no code outside the
//! terminal module can name — much less construct — a capability. The
//! parser data path (`ActionSink` trait in [`crate::parser`]) cannot
//! reach [`WindowMintAuthority::try_mint`]: the trait only gives access
//! to `&mut dyn ActionSink`, which does not expose this type.
//!
//! # Semantics preserved
//!
//! This is a refactor, not a behavior change. The existing
//! `allow_window_ops` boolean remains authoritative and is the only
//! input to [`WindowMintAuthority::try_mint`]. When the flag is true,
//! a capability is minted and XTWINOPS subcommands 1–21 proceed per
//! the existing handler logic. When the flag is false,
//! [`WindowMintAuthority::try_mint`] returns `None` and the capability-
//! gated code paths (the `invoke_window_callback` call sites) are
//! structurally unreachable. The title-stack sub-operations (22/23),
//! which do not invoke the callback, continue to run regardless.
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
/// Minted only by [`WindowMintAuthority::try_mint`] when
/// `allow_window_ops = true`. Consumers outside the terminal module
/// cannot construct one; the type's internal field is private.
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

impl WindowOpsCapability {
    /// Provenance ceremony: lift this window-ops capability into a
    /// [`aterm_provenance::HostAuthorizationToken`] borrowed for the
    /// capability's lifetime.
    ///
    /// Part of the #8001 `authorize_*` wiring (design §6 migration table).
    /// Holding a `WindowOpsCapability` proves that the host's
    /// `allow_window_ops` policy bit was set when the XTWINOPS dispatch
    /// minted the capability; the returned token lets downstream
    /// `authorize_pty_to_host` consumers lift Pty-origin XTWINOPS
    /// arguments (rows/cols from `CSI t`) into `Host`-origin values.
    ///
    /// The token is borrowed by reference against `&self`, so it cannot
    /// outlive the XTWINOPS dispatch frame.
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

/// Zero-sized minting authority for [`WindowOpsCapability`].
///
/// Held implicitly by the terminal module (no field on
/// [`super::Terminal`] is required since the authority has no state).
/// Its [`Self::try_mint`] is the single entry point through which a
/// capability can come into existence; by limiting the constructors
/// to this location, the audit surface for "who can talk to the
/// window callback" collapses to one function.
///
/// The authority is itself a ZST to emphasize that it does not hold
/// policy — it *consults* policy (the `allow_window_ops` bool passed
/// in) and produces a capability iff that policy says yes.
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

    /// Attempt to mint a [`WindowOpsCapability`] given the current value
    /// of the host's `allow_window_ops` policy bit.
    ///
    /// Returns `Some` iff `allow_window_ops` is `true`. The bool is the
    /// one authoritative input: preserving CF-008's refactor contract,
    /// this function does not introduce any additional policy logic.
    ///
    /// # Structural guarantee
    ///
    /// Because this is the only public constructor of
    /// [`WindowOpsCapability`] and it is `pub(super)` (reachable only
    /// from the terminal module), no PTY-origin byte and no external
    /// crate can produce a capability. The parser's `ActionSink` trait
    /// does not expose this method; adding a new XTWINOPS handler that
    /// forgets to consult [`Self::try_mint`] produces a compile error
    /// at the `invoke_window_callback` call site.
    #[inline]
    #[must_use]
    pub(super) fn try_mint(&self, allow_window_ops: bool) -> Option<WindowOpsCapability> {
        let _ = self;
        if allow_window_ops {
            Some(WindowOpsCapability { _seal: () })
        } else {
            None
        }
    }

    /// Engine-consulting variant of [`Self::try_mint`] (#7994).
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
    /// When `engine` is `None`, behavior is identical to [`Self::try_mint`].
    ///
    /// See `terminal/policy_bridge.rs` for the decision tree.
    #[inline]
    #[must_use]
    pub(super) fn try_mint_with_engine(
        &self,
        engine: Option<&aterm_policy::engine::PolicyEngine>,
        origin: aterm_policy::OriginTag,
        ps: u16,
        allow_window_ops: bool,
    ) -> Option<WindowOpsCapability> {
        let _ = self;
        let seq = aterm_policy::selector::DispatchedSequence::csi(Some(u32::from(ps)), 't', []);
        let decision =
            super::policy_bridge::engine_decision_deny_by_default_capability(engine, &seq, origin);
        if decision.resolve(allow_window_ops) {
            Some(WindowOpsCapability { _seal: () })
        } else {
            None
        }
    }
}

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

    /// `try_mint(false)` returns `None`. This is the structural mirror
    /// of the existing `if !self.modes.allow_window_ops { return ... }`
    /// deny branch in [`super::handler_window`]: without the policy bit,
    /// no capability exists, so no call site can reach
    /// `invoke_window_callback`.
    #[test]
    fn disallowed_policy_mints_no_capability() {
        let auth = WindowMintAuthority::new();
        assert!(auth.try_mint(false).is_none());
    }

    /// `try_mint(true)` returns `Some`. When the host has opted into
    /// window operations, the capability is freely constructible — the
    /// gate is strictly an encoding of the existing boolean, not an
    /// additional runtime check.
    #[test]
    fn allowed_policy_mints_capability() {
        let auth = WindowMintAuthority::new();
        assert!(auth.try_mint(true).is_some());
    }

    /// The capability and authority are both zero-sized, so the
    /// capability argument threaded through `invoke_window_callback`
    /// adds no runtime cost — only a type-level obligation.
    #[test]
    fn capability_and_authority_are_zero_sized() {
        assert_eq!(std::mem::size_of::<WindowOpsCapability>(), 0);
        assert_eq!(std::mem::size_of::<WindowMintAuthority>(), 0);
    }

    /// Minting is a pure function of the policy bit: repeated calls
    /// with the same input produce the same outcome (either both
    /// `Some` or both `None`). This documents that
    /// [`WindowMintAuthority`] holds no hidden state — the only input
    /// is the explicit `allow_window_ops` argument.
    #[test]
    fn minting_is_deterministic_in_policy_bit() {
        let auth = WindowMintAuthority::new();
        assert_eq!(
            auth.try_mint(false).is_some(),
            auth.try_mint(false).is_some()
        );
        assert_eq!(auth.try_mint(true).is_some(), auth.try_mint(true).is_some());
    }

    #[test]
    fn standard_profile_wildcard_execute_does_not_overgrant_when_legacy_bool_is_false() {
        let auth = WindowMintAuthority::new();
        let engine = PolicyEngine::new(profiles::standard());

        assert!(
            auth.try_mint_with_engine(Some(&engine), OriginTag::Pty, 21, false)
                .is_none()
        );
    }

    #[test]
    fn explicit_xtwinops_rule_still_allows_when_legacy_bool_is_false() {
        let auth = WindowMintAuthority::new();
        let engine = PolicyEngine::new(policy_with_rule("CSI 21 t", Response::Execute));

        assert!(
            auth.try_mint_with_engine(Some(&engine), OriginTag::Pty, 21, false)
                .is_some()
        );
    }

    /// #8001 ceremony: a minted `WindowOpsCapability` lifts
    /// `Provenance<_, Pty>` to `Provenance<_, Host>`.
    #[test]
    fn as_host_auth_token_lifts_pty_to_host() {
        use aterm_provenance::{OriginTag, Provenance, authorize_pty_to_host};

        let auth = WindowMintAuthority::new();
        let cap = auth.try_mint(true).expect("policy allows window ops");
        let tok = cap.as_host_auth_token();
        let pty: Provenance<u32, aterm_provenance::Pty> = Provenance::from_pty(42);
        let host = authorize_pty_to_host(pty, tok);
        assert_eq!(host.tag(), OriginTag::Host);
        assert_eq!(*host.as_ref(), 42);
    }
}
