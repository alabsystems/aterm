// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Policy-engine bridge for capability gates.

use aterm_policy::{OriginTag, Response, engine::PolicyEngine, selector::DispatchedSequence};

/// Verdict of [`engine_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BridgeDecision {
    /// A rule matched with [`Response::Execute`].
    Allow,
    /// A rule matched with a non-execute response.
    Deny,
    /// No rule matched, or no engine is installed: the legacy `allow_*` bool decides.
    Fallback,
}

impl BridgeDecision {
    /// Resolve against the caller's legacy boolean.
    #[inline]
    #[must_use]
    pub(super) const fn resolve(self, legacy_allow: bool) -> bool {
        match self {
            Self::Allow => true,
            Self::Deny => false,
            Self::Fallback => legacy_allow,
        }
    }
}

/// Consult the installed [`PolicyEngine`] (if any) for `sequence` at `origin`.
#[inline]
#[must_use]
pub(super) fn engine_decision(
    engine: Option<&PolicyEngine>,
    sequence: &DispatchedSequence,
    origin: OriginTag,
) -> BridgeDecision {
    engine_decision_with_wildcard_execute_fallback(engine, sequence, origin, false)
}

/// [`engine_decision`] for deny-by-default gates where a universal wildcard `Execute` falls back to the host bool.
#[inline]
#[must_use]
pub(super) fn engine_decision_deny_by_default_capability(
    engine: Option<&PolicyEngine>,
    sequence: &DispatchedSequence,
    origin: OriginTag,
) -> BridgeDecision {
    engine_decision_with_wildcard_execute_fallback(engine, sequence, origin, true)
}

#[cfg(test)]
thread_local! {
    static ENGINE_DECISIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Read and reset the per-dispatch policy-gate evaluation count.
#[cfg(test)]
pub(super) fn take_engine_decisions() -> usize {
    ENGINE_DECISIONS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

#[inline]
fn engine_decision_with_wildcard_execute_fallback(
    engine: Option<&PolicyEngine>,
    sequence: &DispatchedSequence,
    origin: OriginTag,
    wildcard_execute_falls_back: bool,
) -> BridgeDecision {
    #[cfg(test)]
    ENGINE_DECISIONS.with(|c| c.set(c.get() + 1));
    let Some(engine) = engine else {
        return BridgeDecision::Fallback;
    };
    let decision = engine.evaluate(sequence, origin);
    let Some(rule_idx) = decision.matched_rule else {
        return BridgeDecision::Fallback;
    };
    if decision.response == Response::Execute {
        if wildcard_execute_falls_back
            && engine
                .policy()
                .rules
                .get(rule_idx)
                .is_some_and(|rule| is_universal_wildcard_rule(&rule.sequence))
        {
            return BridgeDecision::Fallback;
        }
        BridgeDecision::Allow
    } else {
        BridgeDecision::Deny
    }
}

#[inline]
fn is_universal_wildcard_rule(sequence: &str) -> bool {
    let trimmed = sequence.trim();
    if trimmed == "*" {
        return true;
    }
    let mut parts = trimmed.split_ascii_whitespace();
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && first.eq_ignore_ascii_case("response")
        && second.eq_ignore_ascii_case("any")
}

/// Build a Standard-profile policy with one rule (`sequence` -> `response` at `origin_min`) and `unmatched` default.
#[cfg(test)]
pub(crate) fn one_rule_policy(
    sequence: &str,
    response: Response,
    origin_min: OriginTag,
    unmatched: Response,
) -> aterm_policy::Policy {
    use aterm_policy::{Defaults, Policy, Profile, Rule, SCHEMA_VERSION};
    Policy {
        schema_version: SCHEMA_VERSION,
        profile: Profile::Standard,
        defaults: Defaults {
            unmatched,
            shell_integration_require_nonce: false,
        },
        rules: vec![Rule {
            sequence: sequence.to_owned(),
            origin_min,
            response,
            rate_limit: None,
            prompt_id: None,
        }],
        rate_limits: vec![],
    }
}

#[cfg(test)]
#[path = "policy_bridge_tests.rs"]
mod tests;
