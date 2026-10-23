// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for the policy profiles and engine.

use super::{
    Defaults, OriginTag, Policy, Profile, Response, Rule, SCHEMA_VERSION, engine::PolicyEngine,
    profiles, profiles::refinement::response_rank,
};

#[test]
fn refinement_holds_over_all_sequences_and_origins() {
    use super::selector::DispatchedSequence;
    let s = |x: &str| x.to_owned();
    let seqs: Vec<DispatchedSequence> = vec![
        DispatchedSequence::osc(52, [s("c"), s("SGVsbG8=")]), // clipboard set
        DispatchedSequence::osc(52, [s("c"), s("?")]),        // clipboard query
        DispatchedSequence::osc(4, [s("3"), s("?")]),         // palette query
        DispatchedSequence::osc(4, [s("3"), s("rgb:00/00/00")]), // palette set
        DispatchedSequence::osc(9, [s("hi")]),                // notification
        DispatchedSequence::osc(99, [s("i=1"), s("body")]),   // notification
        DispatchedSequence::osc(777, [s("notify"), s("t"), s("b")]),
        DispatchedSequence::osc(1337, [s("leet")]),
        DispatchedSequence::osc(8, [s(""), s("https://example.com")]), // hyperlink
        DispatchedSequence::csi(Some(20), 't', Vec::<String>::new()),  // window op
        DispatchedSequence::csi(Some(22), 't', [s("0")]),
        DispatchedSequence::csi(Some(11), 't', Vec::<String>::new()),
        DispatchedSequence::dcs("2000p"),
        DispatchedSequence::dcs("1000p"),
        DispatchedSequence::dcs("3000p"),
        DispatchedSequence::osc(0, [s("title")]),
        DispatchedSequence::osc(99999, [s("x")]),
        DispatchedSequence::csi(Some(1), 'm', Vec::<String>::new()),
        DispatchedSequence::csi(None, 'H', Vec::<String>::new()),
        DispatchedSequence::dcs("9999z"),
    ];

    let hardened = PolicyEngine::new(profiles::hardened());
    let standard = PolicyEngine::new(profiles::standard());
    let permissive = PolicyEngine::new(profiles::permissive());

    for seq in &seqs {
        for &origin in &ALL_ORIGINS {
            let rh = response_rank(hardened.evaluate(seq, origin).response);
            let rs = response_rank(standard.evaluate(seq, origin).response);
            let rp = response_rank(permissive.evaluate(seq, origin).response);
            assert!(
                rh >= rs && rs >= rp,
                "refinement violated for {seq:?} / {origin:?}: \
                 hardened={rh} standard={rs} permissive={rp} (need H>=S>=P strictness)"
            );
        }
    }
}

#[test]
fn hardened_fails_closed_on_unknown_for_untrusted_origins() {
    use super::selector::DispatchedSequence;
    let hardened = PolicyEngine::new(profiles::hardened());
    let ruled: std::collections::BTreeSet<u32> = [4, 8, 9, 52, 99, 777, 1337].into_iter().collect();
    let untrusted: Vec<OriginTag> = ALL_ORIGINS
        .into_iter()
        .filter(|&o| o != OriginTag::Host)
        .collect();
    for major in (0u32..=2200).chain([4000, 5000, 8888, 99_999, u32::MAX]) {
        if ruled.contains(&major) {
            continue;
        }
        let seq = DispatchedSequence::osc(major, [String::from("x")]);
        for &origin in &untrusted {
            let r = hardened.evaluate(&seq, origin).response;
            assert_eq!(
                r,
                Response::Drop,
                "Hardened must fail closed (Drop) on unknown OSC {major} from untrusted \
                 {origin:?}, got {r:?}"
            );
        }
    }
}

#[test]
fn hardened_rules_only_admit_host_configfile_or_user() {
    let h = profiles::hardened();
    for rule in &h.rules {
        match rule.origin_min {
            OriginTag::Host | OriginTag::ConfigFile | OriginTag::User => {}
            other => panic!(
                "Hardened rule for {:?} has origin_min = {:?}, expected Host | ConfigFile | User",
                rule.sequence, other
            ),
        }
    }
}

#[test]
fn hardened_requires_shell_integration_nonce() {
    assert!(
        profiles::hardened()
            .defaults
            .shell_integration_require_nonce
    );
}

#[test]
fn every_rate_limit_ref_resolves_in_each_profile() {
    for policy in [
        profiles::permissive(),
        profiles::standard(),
        profiles::hardened(),
    ] {
        let ids: Vec<&str> = policy.rate_limits.iter().map(|rl| rl.id.as_str()).collect();
        for rule in &policy.rules {
            if let Some(ref rl_ref) = rule.rate_limit {
                assert!(
                    ids.contains(&rl_ref.as_str()),
                    "profile {:?}: rule {:?} references unknown rate limit {rl_ref:?}",
                    policy.profile,
                    rule.sequence,
                );
            }
        }
    }
}

#[test]
fn single_matching_rule_decides_for_every_response_and_origin_gate() {
    use super::selector::DispatchedSequence;
    let responses = [
        Response::Drop,
        Response::Warn,
        Response::Execute,
        Response::Ask,
        Response::Rewrite,
    ];
    for response in responses {
        for origin_min in ALL_ORIGINS {
            let eng = PolicyEngine::new(Policy {
                schema_version: SCHEMA_VERSION,
                profile: Profile::Standard,
                defaults: Defaults {
                    unmatched: Response::Ask,
                    shell_integration_require_nonce: false,
                },
                rules: vec![Rule {
                    sequence: "OSC 9".to_owned(),
                    origin_min,
                    response,
                    rate_limit: None,
                    prompt_id: None,
                }],
                rate_limits: vec![],
            });
            let d = eng.evaluate(
                &DispatchedSequence::osc(9, [String::from("msg")]),
                OriginTag::Host,
            );
            assert_eq!(d.response, response, "origin_min {origin_min:?}");
            assert_eq!(d.matched_rule, Some(0), "origin_min {origin_min:?}");
        }
    }
}

const ALL_ORIGINS: [OriginTag; 8] = [
    OriginTag::Host,
    OriginTag::ConfigFile,
    OriginTag::User,
    OriginTag::UserTyped,
    OriginTag::Ai,
    OriginTag::PtySafe,
    OriginTag::Pty,
    OriginTag::NetworkUntrusted,
];

#[test]
fn trust_rank_is_a_bijection_onto_0_through_7() {
    let mut ranks: Vec<u8> = ALL_ORIGINS.iter().map(|o| o.trust_rank()).collect();
    ranks.sort_unstable();
    assert_eq!(
        ranks,
        (0..8).collect::<Vec<_>>(),
        "trust_rank must biject onto 0..8"
    );
}

#[test]
fn host_is_the_top_and_networkuntrusted_is_the_bottom() {
    for &o in &ALL_ORIGINS {
        assert!(
            OriginTag::Host.dominates(o),
            "Host must dominate {o:?} (top)"
        );
        assert!(
            o.dominates(OriginTag::NetworkUntrusted),
            "{o:?} must dominate NetworkUntrusted (bottom / allow-all floor)"
        );
    }
    assert!(OriginTag::Host.dominates(OriginTag::NetworkUntrusted));
    assert!(!OriginTag::NetworkUntrusted.dominates(OriginTag::Host));
    assert!(OriginTag::Pty.dominates(OriginTag::NetworkUntrusted));
    assert!(!OriginTag::NetworkUntrusted.dominates(OriginTag::Pty));
}
