// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Response capability: a structural gate on the `send_response` sink.

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
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub(super) fn mint_for_dispatch_with_engine(
        engine: Option<&aterm_policy::engine::PolicyEngine>,
        origin: aterm_policy::OriginTag,
        kind: ProbeKind,
    ) -> Option<Self> {
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
#[inline]
pub(super) fn probe_for(kind: ProbeKind) -> aterm_policy::selector::DispatchedSequence {
    let ProbeKind::Dcs { final_byte } = kind;
    let suffix = std::str::from_utf8(&[final_byte])
        .map(str::to_owned)
        .unwrap_or_default();
    aterm_policy::selector::DispatchedSequence::dcs(&suffix)
}
