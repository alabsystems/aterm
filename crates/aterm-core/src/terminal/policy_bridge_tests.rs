// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_policy::{Defaults, Policy, Profile, SCHEMA_VERSION, profiles};

fn policy_with_rule(sequence: &str, response: Response, origin_min: OriginTag) -> Policy {
    one_rule_policy(sequence, response, origin_min, Response::Drop)
}

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
    let standard = Some(PolicyEngine::new(profiles::standard()));
    use BridgeDecision::{Allow, Deny, Fallback};
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
