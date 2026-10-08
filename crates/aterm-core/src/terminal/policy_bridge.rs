// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Policy-engine bridge for the capability-module mint paths.
//!
//! A [`PolicyEngine`] on [`super::Terminal`] evaluates policy-bearing
//! dispatches alongside the `TerminalModes::allow_*` booleans. Every capability
//! module (`response_capability`, `clipboard_auth`, `window_auth`,
//! `shell_integration_auth`) uses the same decision tree, factored into
//! [`engine_decision`]:
//!
//! * no engine: [`BridgeDecision::Fallback`] (the `allow_*` bool decides);
//! * a matching rule with [`Response::Execute`]: [`BridgeDecision::Allow`];
//! * a matching rule with any other response (`Drop`, `Warn`, `Ask`,
//!   `Rewrite`): [`BridgeDecision::Deny`], whatever the bool says;
//! * no matching rule (`defaults.unmatched`): [`BridgeDecision::Fallback`].
//!
//! Deny-by-default sinks that a broad wildcard allow must not reopen use
//! [`engine_decision_deny_by_default_capability`].

use aterm_policy::{OriginTag, Response, engine::PolicyEngine, selector::DispatchedSequence};

/// Decision returned by [`engine_decision`]. Mirrors the three mutually
/// exclusive branches of the bridge decision tree described in the
/// module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BridgeDecision {
    /// The engine matched a rule whose response was [`Response::Execute`].
    /// The mint may proceed regardless of the legacy bool.
    Allow,
    /// The engine matched a rule whose response was **not**
    /// [`Response::Execute`] (i.e. `Drop` / `Warn` / `Ask` / `Rewrite`).
    /// The mint must fail, regardless of the legacy bool.
    Deny,
    /// No rule matched, or no engine was installed. The caller should
    /// consult the legacy `allow_*` / `authorized` boolean.
    Fallback,
}

impl BridgeDecision {
    /// Resolve the bridge decision against the caller's legacy boolean.
    ///
    /// Returns `true` when the mint should proceed, `false` when it
    /// should fail. This is the canonical one-line consumer for sites
    /// that do not need to distinguish the three branches explicitly.
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

/// Consult the installed [`PolicyEngine`] (if any) for `sequence` at `origin`
/// and return the bridge decision; the one way capability modules consult the
/// engine. Never panics or allocates; `Fallback` when `engine` is `None`.
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

// Test-only count of entries into the engine bridge. Fixed capability gates
// read verdicts compiled once per policy into `PolicyGates`; the compiled
// and live verdicts agree on every input, so only this count can show a gate
// that evaluates per dispatch again.
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

    #[test]
    fn decision_none_engine_yields_fallback() {
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "?".to_owned()]);
        assert_eq!(
            engine_decision(None, &seq, OriginTag::Pty),
            BridgeDecision::Fallback,
        );
    }

    #[test]
    fn decision_matching_execute_rule_allows() {
        let policy = policy_with_rule(
            "OSC 52 query",
            Response::Execute,
            OriginTag::NetworkUntrusted,
        );
        let engine = PolicyEngine::new(policy);
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "?".to_owned()]);
        assert_eq!(
            engine_decision(Some(&engine), &seq, OriginTag::Pty),
            BridgeDecision::Allow,
        );
    }

    #[test]
    fn decision_matching_drop_rule_denies() {
        let policy = policy_with_rule("OSC 52 query", Response::Drop, OriginTag::NetworkUntrusted);
        let engine = PolicyEngine::new(policy);
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "?".to_owned()]);
        assert_eq!(
            engine_decision(Some(&engine), &seq, OriginTag::Pty),
            BridgeDecision::Deny,
        );
    }

    #[test]
    fn decision_matching_warn_rule_denies() {
        let policy = policy_with_rule("OSC 52 query", Response::Warn, OriginTag::NetworkUntrusted);
        let engine = PolicyEngine::new(policy);
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "?".to_owned()]);
        assert_eq!(
            engine_decision(Some(&engine), &seq, OriginTag::Pty),
            BridgeDecision::Deny,
        );
    }

    #[test]
    fn decision_no_rule_match_yields_fallback() {
        // Empty rule set → engine always falls through to `defaults.unmatched`.
        let policy = Policy {
            schema_version: SCHEMA_VERSION,
            profile: Profile::Standard,
            defaults: Defaults {
                unmatched: Response::Drop,
                shell_integration_require_nonce: false,
            },
            rules: vec![],
            rate_limits: vec![],
        };
        let engine = PolicyEngine::new(policy);
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "?".to_owned()]);
        assert_eq!(
            engine_decision(Some(&engine), &seq, OriginTag::Pty),
            BridgeDecision::Fallback,
        );
    }

    #[test]
    fn decision_origin_gate_failure_falls_through_to_wildcard() {
        // Standard profile has a `response any` wildcard with
        // origin_min=NetworkUntrusted → every origin matches it → Execute.
        // An OSC 52 set probe from Pty fails the OSC 52 set rule's
        // origin gate (User-or-higher) but the wildcard catches it →
        // the engine returns a matched decision with Execute.
        let engine = PolicyEngine::new(profiles::standard());
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "SGVsbG8=".to_owned()]);
        assert_eq!(
            engine_decision(Some(&engine), &seq, OriginTag::Pty),
            BridgeDecision::Allow,
        );
    }

    #[test]
    fn deny_by_default_bridge_treats_wildcard_execute_as_fallback() {
        let engine = PolicyEngine::new(profiles::standard());
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "SGVsbG8=".to_owned()]);
        assert_eq!(
            engine_decision_deny_by_default_capability(Some(&engine), &seq, OriginTag::Pty),
            BridgeDecision::Fallback,
        );
    }

    #[test]
    fn deny_by_default_bridge_keeps_explicit_execute_authoritative() {
        let policy = policy_with_rule("OSC 52 set", Response::Execute, OriginTag::NetworkUntrusted);
        let engine = PolicyEngine::new(policy);
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "SGVsbG8=".to_owned()]);
        assert_eq!(
            engine_decision_deny_by_default_capability(Some(&engine), &seq, OriginTag::Pty),
            BridgeDecision::Allow,
        );
    }

    #[test]
    fn deny_by_default_bridge_keeps_wildcard_drop_authoritative() {
        let policy = policy_with_rule("response any", Response::Drop, OriginTag::NetworkUntrusted);
        let engine = PolicyEngine::new(policy);
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "SGVsbG8=".to_owned()]);
        assert_eq!(
            engine_decision_deny_by_default_capability(Some(&engine), &seq, OriginTag::Pty),
            BridgeDecision::Deny,
        );
    }

    #[test]
    fn resolve_allow_overrides_false_bool() {
        assert!(BridgeDecision::Allow.resolve(false));
    }

    #[test]
    fn resolve_deny_overrides_true_bool() {
        assert!(!BridgeDecision::Deny.resolve(true));
    }

    #[test]
    fn resolve_fallback_defers_to_bool() {
        assert!(BridgeDecision::Fallback.resolve(true));
        assert!(!BridgeDecision::Fallback.resolve(false));
    }

    // --- Response-injection suppression at the single `send_response` sink ---

    /// A host installing `response any = Drop` suppresses every PTY-origin reply
    /// (DA1/DSR/DECRQSS/...) at the one sink, via the engine-consulting mint gate.
    #[test]
    fn drop_policy_suppresses_response_at_the_sink() {
        use crate::terminal::Terminal;
        let mut term = Terminal::new(24, 80);
        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "response any",
            Response::Drop,
            OriginTag::NetworkUntrusted,
        )));
        term.process(b"\x1b[c"); // DA1 primary device-attributes query
        assert!(
            term.take_response().is_none(),
            "`response any = Drop` must suppress the DA1 reply at the send_response sink (P4)"
        );
    }

    /// With no policy engine the reply is emitted unchanged (default-allow).
    #[test]
    fn no_engine_still_replies() {
        use crate::terminal::Terminal;
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[c"); // DA1; no engine installed
        assert!(
            term.take_response().is_some(),
            "with no policy engine the DA1 reply must be emitted unchanged (P4 default-allow)"
        );
    }

    /// The Kitty `a=q` support-probe reply flows through the same
    /// `send_response` sink as every other PTY reply (policy, rate and
    /// buffer-cap gates), so `response any = Drop` suppresses it.
    #[test]
    fn drop_policy_suppresses_kitty_query_reply() {
        use crate::terminal::Terminal;
        let mut term = Terminal::new(24, 80);
        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "response any",
            Response::Drop,
            OriginTag::NetworkUntrusted,
        )));
        // APC Kitty graphics support probe: a=q (query), i=1, quiet=0 (default).
        term.process(b"\x1b_Ga=q,i=1\x1b\\");
        assert!(
            term.take_response().is_none(),
            "`response any = Drop` must suppress the Kitty a=q OK reply at the send_response sink"
        );
    }

    /// Sanity: with no policy engine the a=q probe still replies (default-allow),
    /// proving the reply is produced and only the Drop gate suppresses it.
    #[test]
    fn no_engine_kitty_query_replies() {
        use crate::terminal::Terminal;
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b_Ga=q,i=1\x1b\\");
        assert!(
            term.take_response().is_some(),
            "with no policy engine the Kitty a=q reply must be emitted (default-allow)"
        );
    }
}
