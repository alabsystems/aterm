// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Response capability: a structural gate on the `send_response` sink.
//!
//! Dozens of sequences (DA1/DA2/DA3, DSR/CPR, DECRQSS, XTGETTCAP, color
//! queries, title and XTWINOPS reports, …) push bytes into the PTY writeback
//! buffer. [`ResponseCapability`] is a zero-sized token whose only constructor
//! ([`ResponseCapability::mint_for_dispatch`], `pub(super)`) is called from the
//! `ActionSink` dispatch entry points in `handler_actions.rs`; every
//! response-producing handler takes `&ResponseCapability` and passes it to
//! [`super::handler::TerminalHandler::send_response`]. A handler without one
//! does not compile, and the parser crate cannot name the type.
//!
//! The capability authorizes an attempt; it does not relax rate limiting.
//! [`super::response_rate_limiter::ResponseRateLimiter`] and the
//! [`super::MAX_RESPONSE_BUFFER_SIZE`] cap still run inside `send_response`.

/// Dispatch kind that selects the [`aterm_policy::selector::DispatchedSequence`]
/// probe shape. Lives here because the terminal module is its only producer.
#[derive(Debug, Clone, Copy)]
pub(super) enum ProbeKind {
    /// CSI dispatch (ESC `[` ... `<final_byte>`). Production probes only the
    /// response sink ([`Self::response_sink`]); the per-shape probes exist for
    /// the reference-implementation cross-check in the tests.
    #[cfg(test)]
    Csi { final_byte: char },
    /// ESC dispatch (ESC `<final_byte>`; no CSI).
    #[cfg(test)]
    Esc { final_byte: char },
    /// OSC dispatch with parsed command number.
    #[cfg(test)]
    Osc { command: u32 },
    /// DCS dispatch (ESC P ... `<final_byte>` ST).
    Dcs { final_byte: u8 },
}

impl ProbeKind {
    /// Catch-all probe for the single `send_response` sink. The response sink
    /// does not know the originating dispatch shape, so a `response any`
    /// wildcard `Drop` rule must be able to suppress every reply through it;
    /// the DCS wildcard bucket is exactly that catch-all.
    #[inline]
    #[must_use]
    pub(super) const fn response_sink() -> Self {
        Self::Dcs { final_byte: 0 }
    }
}

/// Zero-sized proof that the calling context may push bytes into the terminal
/// response buffer.
///
/// Minted only by [`Self::mint_for_dispatch`] from the `ActionSink` dispatch
/// entry points (CSI / ESC / OSC / DCS); the private field prevents
/// construction elsewhere. Passed by reference so several response sites in
/// one dispatch share one token.
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
    /// Mint a response capability for the current dispatch frame.
    ///
    /// `pub(super)`: called only from the `ActionSink` dispatch methods in
    /// `handler_actions.rs`. The parser crate sees only the [`ActionSink`]
    /// trait and cannot name this type.
    ///
    /// [`ActionSink`]: crate::parser::ActionSink
    #[inline]
    #[must_use]
    pub(super) const fn mint_for_dispatch() -> Self {
        Self { _seal: () }
    }

    /// Engine-consulting variant of [`Self::mint_for_dispatch`].
    ///
    /// Probes the [`aterm_policy::engine::PolicyEngine`] with the dispatched
    /// sequence (`kind` and `major`) at `origin` and returns `None` when the
    /// engine denies it. With no engine, or no matching rule, it always mints.
    /// A host can suppress every response with a `response any` rule set to
    /// `Drop`.
    ///
    /// `kind` selects the probe: `ProbeKind::Csi { final_byte }`,
    /// `Esc { final_byte }`, `Osc { command }` (the OSC number from
    /// `params[0]`), or `Dcs { final_byte }`. When parameters are not yet
    /// parsed, use [`mint_for_dispatch`] and let the specific capability module
    /// consult the engine.
    ///
    /// Not on the dispatch path: the `send_response` sink's probe and origin
    /// are constants, so [`super::policy_gates`] compiles its verdict once per
    /// installed policy. This is the reference implementation that table is
    /// checked against (`compiled_gate_matches_the_per_dispatch_mint`).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub(super) fn mint_for_dispatch_with_engine(
        engine: Option<&aterm_policy::engine::PolicyEngine>,
        origin: aterm_policy::OriginTag,
        kind: ProbeKind,
    ) -> Option<Self> {
        // No engine (the default): `engine_decision` would return `Fallback`,
        // which resolves to allow, so skip building the probe and its heap
        // strings.
        if engine.is_none() {
            return Some(Self::mint_for_dispatch());
        }
        let seq = probe_for(kind);
        let decision = super::policy_bridge::engine_decision(engine, &seq, origin);
        if decision.resolve(true) {
            Some(Self::mint_for_dispatch())
        } else {
            None
        }
    }
}

/// Build the [`aterm_policy::selector::DispatchedSequence`] a [`ProbeKind`]
/// stands for.
///
/// The single definition of every dispatch probe, shared by
/// `ResponseCapability::mint_for_dispatch_with_engine` and
/// [`super::policy_gates::PolicyGates`], so the compiled verdict is the one the
/// per-dispatch call would produce.
#[must_use]
pub(super) fn probe_for(kind: ProbeKind) -> aterm_policy::selector::DispatchedSequence {
    match kind {
        #[cfg(test)]
        ProbeKind::Csi { final_byte } | ProbeKind::Esc { final_byte } => {
            aterm_policy::selector::DispatchedSequence::csi(
                None,
                final_byte,
                std::iter::empty::<String>(),
            )
        }
        #[cfg(test)]
        ProbeKind::Osc { command } => {
            aterm_policy::selector::DispatchedSequence::osc(command, std::iter::empty::<String>())
        }
        ProbeKind::Dcs { final_byte } => {
            // DCS has no major code; the "response any" catch-all covers it via
            // the wildcard bucket in the standard profile.
            let suffix = std::str::from_utf8(&[final_byte])
                .map(str::to_owned)
                .unwrap_or_default();
            aterm_policy::selector::DispatchedSequence::dcs(&suffix)
        }
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
        // Zero-sized: the capability costs nothing at runtime.
        assert_eq!(std::mem::size_of_val(&cap), 0);
    }

    /// The seal field is `()`: no payload, no discriminant, no heap.
    #[test]
    fn capability_is_zero_sized() {
        assert_eq!(std::mem::size_of::<ResponseCapability>(), 0);
    }

    /// With no policy engine the engine-consulting mint always yields `Some`,
    /// for every `ProbeKind`, matching the probe-building path.
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
