// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Policy-engine bridge for the capability gates.
//!
//! A [`PolicyEngine`] on [`super::Terminal`] decides policy-bearing dispatches alongside the
//! `TerminalModes::allow_*` booleans through one decision tree, [`engine_decision`] (the fixed
//! gates read its verdicts as `policy_gates` compiled them once per policy):
//!
//! * no engine, or no matching rule (`defaults.unmatched`): [`BridgeDecision::Fallback`], and
//!   the `allow_*` bool decides;
//! * a matching rule with [`Response::Execute`]: [`BridgeDecision::Allow`];
//! * a matching rule with any other response (`Drop`, `Warn`, `Ask`, `Rewrite`):
//!   [`BridgeDecision::Deny`], whatever the bool says.
//!
//! Deny-by-default sinks that a broad wildcard allow must not reopen use
//! [`engine_decision_deny_by_default_capability`].

use aterm_policy::{OriginTag, Response, engine::PolicyEngine, selector::DispatchedSequence};

/// The verdict of [`engine_decision`] (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BridgeDecision {
    /// A rule matched with [`Response::Execute`]: the mint proceeds, whatever the legacy bool.
    Allow,
    /// A rule matched with any other response: the mint fails, whatever the legacy bool.
    Deny,
    /// No rule matched, or no engine is installed: the legacy `allow_*` / `authorized` bool
    /// decides.
    Fallback,
}

impl BridgeDecision {
    /// Resolve against the caller's legacy boolean: `true` when the mint should proceed.
    #[inline]
    #[must_use]
    pub(super) fn resolve(self, legacy_allow: bool) -> bool {
        match self {
            Self::Allow => true,
            Self::Deny => false,
            Self::Fallback => legacy_allow,
        }
    }
}

/// Consult the installed [`PolicyEngine`] (if any) for `sequence` at `origin`. Never panics or
/// allocates; `Fallback` when `engine` is `None`.
#[inline]
#[must_use]
pub(super) fn engine_decision(
    engine: Option<&PolicyEngine>,
    sequence: &DispatchedSequence,
    origin: OriginTag,
) -> BridgeDecision {
    engine_decision_with_wildcard_execute_fallback(engine, sequence, origin, false)
}

/// [`engine_decision`] for deny-by-default gates (OSC 52 clipboard, XTWINOPS,
/// Kitty external file/shared-memory transmission): a broad
/// `response any = Execute` rule yields [`BridgeDecision::Fallback`], so the
/// host's authorization bit still decides; wildcard non-`Execute` rules still
/// return [`BridgeDecision::Deny`].
#[inline]
#[must_use]
pub(super) fn engine_decision_deny_by_default_capability(
    engine: Option<&PolicyEngine>,
    sequence: &DispatchedSequence,
    origin: OriginTag,
) -> BridgeDecision {
    engine_decision_with_wildcard_execute_fallback(engine, sequence, origin, true)
}

// Test-only count of entries into the engine bridge. Fixed capability gates read verdicts
// compiled once per policy into `PolicyGates`, which agree with the live verdicts on every
// input, so only this count shows a gate that evaluates per dispatch again.
#[cfg(test)]
thread_local! {
    static ENGINE_DECISIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Take (read and reset) the per-dispatch policy-gate evaluation count.
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
        // Fell through to `defaults.unmatched`: the legacy bool decides.
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

/// Test fixture: a Standard-profile policy holding one rule, `sequence` →
/// `response` at `origin_min`, with `unmatched` as the default response.
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
mod tests {
    use super::*;
    use aterm_policy::{Defaults, Policy, Profile, SCHEMA_VERSION, profiles};

    fn policy_with_rule(sequence: &str, response: Response, origin_min: OriginTag) -> Policy {
        one_rule_policy(sequence, response, origin_min, Response::Drop)
    }

    /// The bridge's decision tree, for both bridges: no engine and no matching
    /// rule fall back to the host bool; a matching `Execute` allows and any
    /// other response denies; the deny-by-default bridge treats only a
    /// universal wildcard `Execute` as a fallback.
    #[test]
    fn the_bridge_decision_tree() {
        let set = DispatchedSequence::osc(52, ["c".to_owned(), "SGVsbG8=".to_owned()]);
        let query = DispatchedSequence::osc(52, ["c".to_owned(), "?".to_owned()]);
        let rule = |sequence, response| {
            Some(PolicyEngine::new(policy_with_rule(
                sequence,
                response,
                OriginTag::NetworkUntrusted,
            )))
        };
        let empty = Some(PolicyEngine::new(Policy {
            schema_version: SCHEMA_VERSION,
            profile: Profile::Standard,
            defaults: Defaults {
                unmatched: Response::Drop,
                shell_integration_require_nonce: false,
            },
            rules: vec![],
            rate_limits: vec![],
        }));
        // The standard profile's `response any` wildcard catches an OSC 52 set
        // whose own rule's origin gate (User or higher) a PTY fails.
        let standard = Some(PolicyEngine::new(profiles::standard()));
        use BridgeDecision::{Allow, Deny, Fallback};
        // (case, engine, sequence, plain bridge, deny-by-default bridge)
        let cases: [(&str, Option<PolicyEngine>, &DispatchedSequence, _, _); 8] = [
            ("no engine", None, &query, Fallback, Fallback),
            (
                "execute",
                rule("OSC 52 query", Response::Execute),
                &query,
                Allow,
                Allow,
            ),
            (
                "drop",
                rule("OSC 52 query", Response::Drop),
                &query,
                Deny,
                Deny,
            ),
            (
                "warn",
                rule("OSC 52 query", Response::Warn),
                &query,
                Deny,
                Deny,
            ),
            ("no rule", empty, &query, Fallback, Fallback),
            ("wildcard execute", standard, &set, Allow, Fallback),
            (
                "explicit execute",
                rule("OSC 52 set", Response::Execute),
                &set,
                Allow,
                Allow,
            ),
            (
                "wildcard drop",
                rule("response any", Response::Drop),
                &set,
                Deny,
                Deny,
            ),
        ];
        for (case, engine, seq, plain, deny_by_default) in cases {
            assert_eq!(
                engine_decision(engine.as_ref(), seq, OriginTag::Pty),
                plain,
                "{case}"
            );
            assert_eq!(
                engine_decision_deny_by_default_capability(engine.as_ref(), seq, OriginTag::Pty),
                deny_by_default,
                "{case}: deny-by-default"
            );
        }
    }

    #[test]
    fn resolve_overrides_the_bool_except_on_fallback() {
        assert!(BridgeDecision::Allow.resolve(false));
        assert!(!BridgeDecision::Deny.resolve(true));
        assert!(BridgeDecision::Fallback.resolve(true));
        assert!(!BridgeDecision::Fallback.resolve(false));
    }

    // Response suppression at the single `send_response` sink.

    /// `response any = Drop` suppresses every PTY-origin reply at the one `send_response` sink:
    /// a DA1 query's, and the Kitty `a=q` probe's (`i=1`, quiet 0), which goes through the
    /// same sink.
    #[test]
    fn drop_policy_suppresses_every_reply_at_the_sink() {
        use crate::terminal::Terminal;
        for (query, what) in [
            (&b"\x1b[c"[..], "DA1"),
            (&b"\x1b_Ga=q,i=1\x1b\\"[..], "Kitty a=q"),
        ] {
            let mut term = Terminal::new(24, 80);
            term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
                "response any",
                Response::Drop,
                OriginTag::NetworkUntrusted,
            )));
            term.process(query);
            assert!(
                term.take_response().is_none(),
                "`response any = Drop` must suppress the {what} reply at the send_response sink"
            );
        }
    }
}
