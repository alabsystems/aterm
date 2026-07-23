// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Response capability — structural gate on the `send_response` sink.
//!
//! # Why this module exists
//!
//! Privilege-conflation finding CF-003 (see
//! `reports/2026-04-18-privilege-conflation-audit.md`) identified 31
//! escape sequences (DA1/DA2/DA3, DSR/CPR, DECRQSS, XTGETTCAP, color
//! queries, title reports, XTWINOPS geometry reports, …) that each push
//! attacker-chosen bytes into the PTY writeback buffer. Each new response
//! site is one forgotten rate-limit or buffer-cap check away from a
//! DSR/DA amplification attack or fingerprint leak.
//!
//! Phase 1 (commit `0313b2561`) used the exact pattern below to close
//! the Terminal-class RCE on DCS 2000 p / 1000 p:
//! `ConductorActivationToken` / `TmuxActivationToken` are zero-sized,
//! unforgeable from outside the crate, and required as an explicit
//! argument on `SshConductorMode::activate` / `TmuxControlMode::activate`.
//! The parser data path cannot construct one, so the compiler itself
//! discharges the "PTY-origin activation" class. See
//! [`super::modal_auth`] for the template.
//!
//! # The structural gate
//!
//! [`ResponseCapability`] is a zero-sized token whose only constructor
//! ([`ResponseCapability::mint_for_dispatch`]) is `pub(super)` to the
//! terminal module and is called only from the `ActionSink` dispatch
//! entry points in `handler_actions.rs`. Every response-producing
//! handler receives `&ResponseCapability` as an explicit argument and
//! calls [`super::handler::TerminalHandler::send_response`] which takes
//! `&ResponseCapability`. A handler that forgets — or a future feature
//! that calls `send_response` without a capability — fails to compile.
//!
//! Because `ResponseCapability::mint_for_dispatch` is `pub(super)` to
//! the terminal module, the set of places that can originate a
//! response is bounded by the shape of the type.
//!
//! # Relation to rate limiting
//!
//! The capability is an *authorization* gate ("this call is inside a
//! parser-dispatched response-generating sequence"), not a *policy* gate.
//! Rate limiting continues to live on
//! [`super::response_rate_limiter::ResponseRateLimiter`] and the
//! [`super::MAX_RESPONSE_BUFFER_SIZE`] cap, both of which run inside
//! `send_response` regardless of whether a capability is present. The
//! capability cannot relax those — it only proves that a response *may*
//! be attempted at all.

/// Dispatch-kind hint used by
/// [`ResponseCapability::mint_for_dispatch_with_engine`] to select the
/// right [`aterm_policy::selector::DispatchedSequence`] probe shape
/// (#7994).
///
/// The enum lives here rather than in the policy crate because the
/// terminal module is the only producer and the policy crate should
/// not depend on our dispatch taxonomy.
#[derive(Debug, Clone, Copy)]
#[allow(
    dead_code,
    reason = "#7994 scaffolding: ProbeKind is consumed by the engine-consulting mint variant reserved for Release N+1 per-site wiring."
)]
pub(super) enum ProbeKind {
    /// CSI dispatch (ESC `[` ... `<final_byte>`).
    Csi { final_byte: char },
    /// ESC dispatch (ESC `<final_byte>`; no CSI).
    Esc { final_byte: char },
    /// OSC dispatch with parsed command number.
    Osc { command: u32 },
    /// DCS dispatch (ESC P ... `<final_byte>` ST).
    Dcs { final_byte: u8 },
}

impl ProbeKind {
    /// Catch-all probe for the single `send_response` sink. The response sink
    /// does not know the originating dispatch shape, so a `response any`
    /// wildcard `Drop` rule must be able to suppress every reply through it;
    /// the DCS wildcard bucket is exactly that catch-all (#7994).
    #[inline]
    #[must_use]
    pub(super) const fn response_sink() -> Self {
        Self::Dcs { final_byte: 0 }
    }
}

/// Zero-sized proof that the calling context is allowed to push bytes
/// into the terminal response buffer.
///
/// Minted only by [`Self::mint_for_dispatch`], which is `pub(super)` to
/// the terminal module and invoked from the `ActionSink` dispatch
/// entry points in `handler_actions.rs` (CSI / ESC / OSC / DCS).
/// Consumers outside the terminal module cannot construct one; the
/// type's internal field is private.
///
/// Required by [`super::handler::TerminalHandler::send_response`]. The
/// capability is passed by shared reference so multiple response sites
/// in one dispatch can share a single token without ownership transfer.
#[derive(Debug)]
pub(super) struct ResponseCapability {
    /// Private seal — prevents construction outside this module.
    ///
    /// Matches the pattern from `ConductorActivationToken` /
    /// `TmuxActivationToken`: a private unit field forces consumers to
    /// go through the module's visibility-gated constructor.
    _seal: (),
}

impl ResponseCapability {
    /// Mint a new response capability for the current dispatch frame.
    ///
    /// Called exclusively from `ActionSink` dispatch methods in
    /// `handler_actions.rs` at the start of a parser-originated
    /// sequence that may legitimately produce a response (CSI, ESC,
    /// OSC, DCS). The returned capability is bound to the calling
    /// scope and borrowed by the downstream handlers for the duration
    /// of that dispatch.
    ///
    /// # Why this is `pub(super)`
    ///
    /// The constructor is `pub(super)`, which restricts it to the
    /// `terminal` module. The dispatch entry points live in
    /// `handler_actions.rs` inside the same module; no other caller
    /// can reach this function. The parser crate (`aterm-parser`) has
    /// no access: it only sees the [`ActionSink`] trait, whose methods
    /// take `&mut dyn ActionSink` and cannot name this type.
    ///
    /// [`ActionSink`]: crate::parser::ActionSink
    #[inline]
    #[must_use]
    pub(super) const fn mint_for_dispatch() -> Self {
        Self { _seal: () }
    }

    /// Engine-consulting variant of [`Self::mint_for_dispatch`] (#7994).
    ///
    /// Consults the [`aterm_policy::engine::PolicyEngine`] with a probe
    /// for the dispatched sequence (CSI / ESC / OSC / DCS, identified by
    /// `kind` and `major`) at `origin`. Returns `None` if the engine
    /// denies the sequence — callers skip response emission for this
    /// dispatch frame. When the engine is absent or falls through, the
    /// capability is always minted, preserving the pre-engine behavior
    /// (response_capability has no legacy boolean to fall back to;
    /// "allow by default at fallthrough" matches the existing always-
    /// mint semantics and design §6.3 Release N backward-compat).
    ///
    /// Hosts that want to suppress all response-producing sequences
    /// (fingerprint mitigation, DECRQSS/DSR hardening) can attach a
    /// policy with the `response any` wildcard rule set to `Drop`.
    ///
    /// Called from the ActionSink dispatch entry points in
    /// `handler_actions.rs`. The `kind` parameter selects the probe
    /// shape:
    ///
    /// * `ProbeKind::Csi { final_byte }` — CSI dispatch
    /// * `ProbeKind::Esc { final_byte }` — ESC dispatch
    /// * `ProbeKind::Osc { command }` — OSC dispatch (command is the
    ///   OSC command number from params[0])
    /// * `ProbeKind::Dcs { final_byte }` — DCS dispatch
    ///
    /// For OSC dispatch specifically, callers pass the parsed OSC
    /// command number; for dispatches where the parameters are not
    /// yet parsed (e.g. OSC mint before the command byte is read)
    /// callers should use [`mint_for_dispatch`] and defer engine
    /// consultation to the specific capability module (clipboard,
    /// notification, etc.) which has a full parameter view.
    #[inline]
    #[must_use]
    pub(super) fn mint_for_dispatch_with_engine(
        engine: Option<&aterm_policy::engine::PolicyEngine>,
        origin: aterm_policy::OriginTag,
        kind: ProbeKind,
    ) -> Option<Self> {
        // Default no-policy fast path (#7994 perf): with no engine
        // installed (the common case — `Terminal::new` attaches none),
        // `engine_decision` returns `Fallback` and `Fallback.resolve(true)`
        // is `true`, so this function always mints `Some`. Skip building the
        // `DispatchedSequence` probe — the probe is read only when an engine
        // is present, and the Dcs response-sink arm below would otherwise
        // burn two heap `String` allocations per reply (the 1-byte
        // `str::to_owned` plus the `Into<String>` clone in
        // `DispatchedSequence::dcs`). Behavior-identical to falling through.
        if engine.is_none() {
            return Some(Self::mint_for_dispatch());
        }
        let seq = match kind {
            ProbeKind::Csi { final_byte } => aterm_policy::selector::DispatchedSequence::csi(
                None,
                final_byte,
                std::iter::empty::<String>(),
            ),
            ProbeKind::Esc { final_byte } => aterm_policy::selector::DispatchedSequence::csi(
                None,
                final_byte,
                std::iter::empty::<String>(),
            ),
            ProbeKind::Osc { command } => aterm_policy::selector::DispatchedSequence::osc(
                command,
                std::iter::empty::<String>(),
            ),
            ProbeKind::Dcs { final_byte } => {
                // DCS has no major code; the "response any" catch-all
                // covers it via the wildcard bucket in the standard
                // profile.
                let suffix = std::str::from_utf8(&[final_byte])
                    .map(str::to_owned)
                    .unwrap_or_default();
                aterm_policy::selector::DispatchedSequence::dcs(&suffix)
            }
        };
        let decision = super::policy_bridge::engine_decision(engine, &seq, origin);
        if decision.resolve(true) {
            Some(Self::mint_for_dispatch())
        } else {
            None
        }
    }

    /// Provenance ceremony: lift this response capability into a
    /// [`HostAuthorizationToken`] borrowed for the capability's lifetime.
    ///
    /// Part of the #8001 `authorize_*` wiring (design §6 migration table).
    /// Holding a `ResponseCapability` proves that the caller is inside a
    /// parser-originated dispatch that may legitimately produce a host-
    /// directed response; the returned token lets downstream
    /// `authorize_pty_to_host` consumers lift Pty-origin response bytes
    /// into `Host`-origin so the rate-limited `send_response` pipeline can
    /// treat them as policy-approved.
    ///
    /// The token is borrowed by reference against `&self`, so it cannot
    /// outlive the dispatch frame.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The capability type exists and can be constructed by in-module code.
    /// The production compile-fail proof (that external code cannot construct
    /// one) lives in the capability compile-fail test matrix.
    #[test]
    fn in_module_construction_succeeds() {
        let cap = ResponseCapability::mint_for_dispatch();
        // Structural assertion: ZST occupies no bytes, confirming the
        // capability compiles to a type-level argument with no runtime cost.
        assert_eq!(std::mem::size_of_val(&cap), 0);
    }

    /// The seal field is `()` — no payload, no discriminant, no heap.
    /// Keeps the capability zero-cost at the ABI boundary.
    #[test]
    fn capability_is_zero_sized() {
        assert_eq!(std::mem::size_of::<ResponseCapability>(), 0);
    }

    /// #8001 ceremony: a `ResponseCapability` mints a
    /// `HostAuthorizationToken` that lifts `Provenance<_, Pty>` to
    /// `Provenance<_, Host>`.
    #[test]
    fn as_host_auth_token_lifts_pty_to_host() {
        use aterm_provenance::{OriginTag, Provenance, authorize_pty_to_host};

        let cap = ResponseCapability::mint_for_dispatch();
        let tok = cap.as_host_auth_token();
        let pty: Provenance<Vec<u8>, aterm_provenance::Pty> =
            Provenance::from_pty(b"\x1b[?1;2c".to_vec());
        let host = authorize_pty_to_host(pty, tok);
        assert_eq!(host.tag(), OriginTag::Host);
        assert_eq!(host.as_ref(), b"\x1b[?1;2c");
    }

    /// #7994 perf fast path: with no policy engine installed (the default),
    /// the engine-consulting mint still always yields `Some` — the early
    /// return skips building the `DispatchedSequence` probe but is
    /// behavior-identical to falling through `engine_decision` (which
    /// returns `Fallback`, and `Fallback.resolve(true) == true`). Exercised
    /// across every `ProbeKind` shape so the fast path cannot diverge from
    /// the probe-building path for any dispatch kind.
    #[test]
    fn no_engine_still_mints_some() {
        let origin = aterm_policy::OriginTag::Pty;
        for kind in [
            ProbeKind::response_sink(),
            ProbeKind::Csi { final_byte: 'c' },
            ProbeKind::Esc { final_byte: 'Z' },
            ProbeKind::Osc { command: 10 },
            ProbeKind::Dcs { final_byte: b'q' },
        ] {
            assert!(
                ResponseCapability::mint_for_dispatch_with_engine(None, origin, kind).is_some(),
                "engine=None must always mint Some for {kind:?}",
            );
        }
    }
}
