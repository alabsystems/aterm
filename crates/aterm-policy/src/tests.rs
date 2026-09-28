// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for the policy data model, profiles and engine: TOML round-trips,
//! the `hardened ⊆ standard ⊆ permissive` refinement over every sequence the
//! profiles rule on, Hardened's fail-closed default, rule-vs-default
//! arbitration, and the exhaustive `OriginTag` lattice order.

use super::{
    Defaults, OriginTag, Policy, Profile, RateLimit, Response, Rule, SCHEMA_VERSION, aliases,
    engine::PolicyEngine, profiles, profiles::refinement::response_rank,
};

// ---------------------------------------------------------------------------
// Round-trip
// ---------------------------------------------------------------------------

#[test]
fn hardened_roundtrips_through_toml() {
    let original = profiles::hardened();
    let ser = original.to_toml().expect("hardened serializes");
    let parsed: Policy = aterm_toml::from_str(&ser).expect("hardened parses");
    assert_eq!(
        original, parsed,
        "hardened policy round-trip must be lossless"
    );
}

#[test]
fn standard_roundtrips_through_toml() {
    let original = profiles::standard();
    let ser = original.to_toml().expect("standard serializes");
    let parsed: Policy = aterm_toml::from_str(&ser).expect("standard parses");
    assert_eq!(original, parsed);
}

#[test]
fn permissive_roundtrips_through_toml() {
    let original = profiles::permissive();
    let ser = original.to_toml().expect("permissive serializes");
    let parsed: Policy = aterm_toml::from_str(&ser).expect("permissive parses");
    assert_eq!(original, parsed);
}

// ---------------------------------------------------------------------------
// Refinement invariant
// ---------------------------------------------------------------------------

#[test]
fn refinement_unmatched_default_is_monotone() {
    let h = response_rank(profiles::hardened().defaults.unmatched);
    let s = response_rank(profiles::standard().defaults.unmatched);
    let p = response_rank(profiles::permissive().defaults.unmatched);
    assert!(
        h >= s && s >= p,
        "refinement violated: hardened={h} standard={s} permissive={p} \
         (expected hardened >= standard >= permissive over strictness rank)"
    );
}

#[test]
fn refinement_holds_over_all_sequences_and_origins() {
    // The refinement invariant in FULL: for EVERY (sequence, origin), the
    // stricter profile is never looser than the looser one, i.e.
    // rank(hardened) >= rank(standard) >= rank(permissive) (higher rank = stricter;
    // see `response_rank`). The test above only covers the unmatched default.
    // Verify it here over a comprehensive CONCRETE grid:
    // every sequence the default profiles carry a rule for, plus several unmatched
    // fall-throughs, evaluated under all 8 origins.
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
        // Unmatched → each profile's default response.
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
    // Fail-closed invariant: a strict profile must never let an UNTRUSTED origin execute a
    // sequence it does not explicitly rule on. `Host` (the trusted host app) is
    // intentionally allowed to execute via the Host-gated `response any` wildcard
    // rule, so it is excluded; every OTHER origin must fall through to the Drop
    // default for an unknown sequence. Sweep the OSC-major space (dense low range +
    // sparse high points), excluding the majors any default profile rules on.
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
fn hardened_denies_unmatched() {
    assert_eq!(
        profiles::hardened().defaults.unmatched,
        Response::Drop,
        "Hardened MUST drop on unmatched (§4.1 + §7.3)"
    );
}

// ---------------------------------------------------------------------------
// Hardened-specific invariants from techlead brief
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Schema version + profile tag coherence
// ---------------------------------------------------------------------------

#[test]
fn all_builtin_profiles_stamp_current_schema_version() {
    for p in [
        profiles::permissive(),
        profiles::standard(),
        profiles::hardened(),
    ] {
        assert_eq!(p.schema_version, SCHEMA_VERSION);
    }
}

#[test]
fn builtin_profiles_carry_correct_profile_tag() {
    assert_eq!(profiles::permissive().profile, Profile::Permissive);
    assert_eq!(profiles::standard().profile, Profile::Standard);
    assert_eq!(profiles::hardened().profile, Profile::Hardened);
}

// ---------------------------------------------------------------------------
// Rate-limit references resolve
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Alias table
// ---------------------------------------------------------------------------

#[test]
fn alias_table_has_expected_entries() {
    // Design §3.4 — 7 canonical aliases.
    assert_eq!(aliases::count(), 7);
    assert!(aliases::lookup("OSC 52 set").is_some());
    assert!(aliases::lookup("OSC 52 query").is_some());
    assert!(aliases::lookup("response any").is_some());
}

#[test]
fn alias_lookup_is_case_sensitive() {
    assert!(aliases::lookup("OSC 52 set").is_some());
    assert!(aliases::lookup("osc 52 set").is_none());
}

// ---------------------------------------------------------------------------
// Rule-vs-default arbitration
// ---------------------------------------------------------------------------

/// A single-rule policy whose selector matches and whose origin gate is met
/// returns exactly that rule's response — for EVERY response and EVERY
/// `origin_min` (5 × 8, the whole domain). Rules out wildcard leakage (the `*`
/// bucket firing ahead of the specific one), default leakage (`unmatched`
/// winning over a matching rule — the default here is `Ask`, so a leak is
/// observable for every other response) and a misreported rule index.
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
            // `Host` dominates every origin, so the gate always admits the rule.
            let d = eng.evaluate(
                &DispatchedSequence::osc(9, [String::from("msg")]),
                OriginTag::Host,
            );
            assert_eq!(d.response, response, "origin_min {origin_min:?}");
            assert_eq!(d.matched_rule, Some(0), "origin_min {origin_min:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// Hand-built Rule / RateLimit round-trip
// ---------------------------------------------------------------------------

#[test]
fn manually_built_policy_roundtrips() {
    let p = Policy {
        schema_version: SCHEMA_VERSION,
        profile: Profile::Standard,
        defaults: Defaults {
            unmatched: Response::Warn,
            shell_integration_require_nonce: true,
        },
        rules: vec![Rule {
            sequence: "OSC 52 set".to_owned(),
            origin_min: OriginTag::User,
            response: Response::Ask,
            rate_limit: Some("clipboard".to_owned()),
            prompt_id: Some("clipboard-write".to_owned()),
        }],
        rate_limits: vec![RateLimit {
            id: "clipboard".to_owned(),
            capacity_bytes: 16_384,
            refill_per_second: 1_024,
            per_sequence_max: 65_536,
        }],
    };

    let ser = p.to_toml().expect("serialize");
    let back: Policy = aterm_toml::from_str(&ser).expect("parse");
    assert_eq!(p, back);
}

// ---------------------------------------------------------------------------
// OriginTag trust lattice — exhaustive order proofs
// ---------------------------------------------------------------------------
//
// The lattice has only 8 elements, so enumerating all pairs/triples is a
// COMPLETE proof (not a sample) of its order-theoretic invariants. These pin the
// load-bearing structure the policy engine relies on — in particular that `Host`
// is the top (dominates every origin) and `NetworkUntrusted` is the bottom (the
// "allow from any origin" floor the default profiles gate on via
// `origin_min = NetworkUntrusted`, e.g. the permissive "response any" rule). Any
// accidental reordering, rank collision, or inversion fails HERE rather than
// silently changing which origins may run which escape sequences. (This ordering
// is deliberately distinct from `aterm_provenance::OriginTag`'s — a separate
// byte-taint lattice whose bottom is `Pty`; see the `OriginTag` type docs.)

/// Every `OriginTag` variant, most- to least-trusted.
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

/// Compile-time guard: adding an `OriginTag` variant without extending
/// [`ALL_ORIGINS`] (and the proofs below) is a hard error. The match is
/// exhaustive without a wildcard because these tests live in the defining crate,
/// and walking [`ALL_ORIGINS`] through it pins that the list names every variant
/// exactly once, in trust order.
#[test]
fn all_origins_names_every_variant_once_in_order() {
    for (index, &origin) in ALL_ORIGINS.iter().enumerate() {
        let position = match origin {
            OriginTag::Host => 0,
            OriginTag::ConfigFile => 1,
            OriginTag::User => 2,
            OriginTag::UserTyped => 3,
            OriginTag::Ai => 4,
            OriginTag::PtySafe => 5,
            OriginTag::Pty => 6,
            OriginTag::NetworkUntrusted => 7,
        };
        assert_eq!(position, index, "ALL_ORIGINS[{index}] is {origin:?}");
    }
}

#[test]
fn trust_rank_is_a_bijection_onto_0_through_7() {
    // Each variant has a distinct rank covering exactly 0..8 — no collisions, no
    // gaps. A collision would make two origins indistinguishable to `dominates`.
    let mut ranks: Vec<u8> = ALL_ORIGINS.iter().map(|o| o.trust_rank()).collect();
    ranks.sort_unstable();
    assert_eq!(
        ranks,
        (0..8).collect::<Vec<_>>(),
        "trust_rank must biject onto 0..8"
    );
}

#[test]
fn dominates_agrees_with_trust_rank_everywhere() {
    // The whole contract: `a dominates b  ⟺  rank(a) <= rank(b)`. Exhaustive 8×8.
    for &a in &ALL_ORIGINS {
        for &b in &ALL_ORIGINS {
            assert_eq!(
                a.dominates(b),
                a.trust_rank() <= b.trust_rank(),
                "dominates disagreed with trust_rank for {a:?} vs {b:?}"
            );
        }
    }
}

#[test]
fn dominance_is_a_reflexive_antisymmetric_transitive_total_order() {
    for &a in &ALL_ORIGINS {
        assert!(a.dominates(a), "reflexivity: {a:?}");
        for &b in &ALL_ORIGINS {
            assert!(a.dominates(b) || b.dominates(a), "totality: {a:?},{b:?}");
            if a.dominates(b) && b.dominates(a) {
                assert_eq!(a, b, "antisymmetry: {a:?},{b:?}");
            }
        }
    }
    for &a in &ALL_ORIGINS {
        for &b in &ALL_ORIGINS {
            for &c in &ALL_ORIGINS {
                if a.dominates(b) && b.dominates(c) {
                    assert!(a.dominates(c), "transitivity: {a:?},{b:?},{c:?}");
                }
            }
        }
    }
}

#[test]
fn host_is_the_top_and_networkuntrusted_is_the_bottom() {
    // Host dominates every origin (top); every origin dominates NetworkUntrusted
    // (bottom). The bottom is LOAD-BEARING: default profiles use
    // `origin_min = NetworkUntrusted` as the "allow from any origin" floor. If this
    // inverts, those rules silently change meaning — this is the tripwire.
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
    // The lattice is non-trivial: the top strictly dominates the bottom.
    assert!(OriginTag::Host.dominates(OriginTag::NetworkUntrusted));
    assert!(!OriginTag::NetworkUntrusted.dominates(OriginTag::Host));
}

#[test]
fn pty_default_is_more_trusted_than_explicit_network() {
    // Pins the deliberate threat-model choice (the one that LOOKS like an
    // inversion vs aterm-provenance but is intentional here): an unshaped local
    // PTY byte (`Pty`) is MORE trusted than an explicitly remote one
    // (`NetworkUntrusted`), because the escape-policy floor treats known-remote as
    // the least-trusted origin. Provenance's byte-taint lattice makes the opposite
    // (Pty-as-catch-all-bottom) choice for its own purpose; the two are separate.
    assert!(OriginTag::Pty.dominates(OriginTag::NetworkUntrusted));
    assert!(!OriginTag::NetworkUntrusted.dominates(OriginTag::Pty));
}
