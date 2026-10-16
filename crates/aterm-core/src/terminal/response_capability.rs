// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Response capability: a structural gate on the `send_response` sink.
//!
//! Every response-producing handler (DA, DSR/CPR, DECRQSS, XTGETTCAP, color queries,
//! title and XTWINOPS reports, …) takes a `&ResponseCapability` and passes it to
//! [`super::handler::TerminalHandler::send_response`]. Its only constructor
//! ([`ResponseCapability::mint_for_dispatch`]) is called from the `ActionSink` dispatch
//! entry points in `handler_actions.rs`, and the parser crate cannot name the type.
//!
//! The capability authorizes an attempt; rate limiting
//! ([`super::response_rate_limiter::ResponseRateLimiter`]) and the
//! [`super::MAX_RESPONSE_BUFFER_SIZE`] cap still run inside `send_response`.

/// Dispatch kind that selects the [`aterm_policy::selector::DispatchedSequence`]
/// probe shape. Lives here because the terminal module is its only producer.
#[derive(Debug, Clone, Copy)]
pub(super) enum ProbeKind {
    /// DCS dispatch (ESC P ... `<final_byte>` ST).
    Dcs { final_byte: u8 },
}

impl ProbeKind {
    /// Catch-all probe for the single `send_response` sink, which does not know the
    /// dispatch shape: the DCS wildcard bucket, so a `response any` `Drop` rule suppresses
    /// every reply.
    #[inline]
    #[must_use]
    pub(super) const fn response_sink() -> Self {
        Self::Dcs { final_byte: 0 }
    }
}

/// Zero-sized proof that the calling context may push bytes into the terminal response
/// buffer. Minted only by [`Self::mint_for_dispatch`]; passed by reference so several
/// response sites in one dispatch share one token.
#[derive(Debug)]
pub(super) struct ResponseCapability {
    /// Private [`super::Seal`]: only [`Self::mint_for_dispatch`] builds the token, and it
    /// cannot derive `Clone` or `Default`.
    _seal: super::Seal,
}

impl ResponseCapability {
    /// Mint a response capability for the current dispatch frame. Outside tests, called
    /// only from the `ActionSink` dispatch methods in `handler_actions.rs`.
    #[inline]
    #[must_use]
    pub(super) const fn mint_for_dispatch() -> Self {
        Self { _seal: super::Seal }
    }

    /// Engine-consulting variant of [`Self::mint_for_dispatch`]: probes the
    /// [`aterm_policy::engine::PolicyEngine`] with `kind` at `origin` and returns `None`
    /// when it denies; with no engine or no matching rule it mints.
    ///
    /// Not on the dispatch path: the `send_response` sink's probe and origin are constants,
    /// so [`super::policy_gates`] compiles its verdict once per installed policy. This is
    /// the reference that table is checked against
    /// (`compiled_gate_matches_the_per_dispatch_mint`).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub(super) fn mint_for_dispatch_with_engine(
        engine: Option<&aterm_policy::engine::PolicyEngine>,
        origin: aterm_policy::OriginTag,
        kind: ProbeKind,
    ) -> Option<Self> {
        // No engine resolves to allow: skip building the probe and its heap strings.
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

/// The [`aterm_policy::selector::DispatchedSequence`] a [`ProbeKind`] stands for: the one
/// definition shared by `ResponseCapability::mint_for_dispatch_with_engine` and
/// [`super::policy_gates::PolicyGates`], so the compiled verdict is the per-dispatch one.
#[must_use]
pub(super) fn probe_for(kind: ProbeKind) -> aterm_policy::selector::DispatchedSequence {
    match kind {
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

    /// The token is zero-sized: no payload, no heap.
    #[test]
    fn capability_is_zero_sized() {
        assert_eq!(std::mem::size_of::<ResponseCapability>(), 0);
    }
}
