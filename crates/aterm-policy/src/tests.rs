// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for the Phase 0 scaffold (#7991).
//!
//! These tests cover the three acceptance criteria from the techlead brief:
//!
//! 1. Round-trip: `hardened → TOML → parse → equal`.
//! 2. Schema validation: malformed TOML / wrong `schema_version` falls back to
//!    Hardened.
//! 3. Profile refinement ordering: `hardened ⊆ standard ⊆ permissive` over
//!    the unmatched-default response rank.
//!
//! A wider profile matrix against every concrete sequence lives with the
//! engine (#7992) once the decision tree exists.

use super::{
    Defaults, MirrorField, MirrorSnapshot, OriginTag, Policy, Profile, RateLimit, Response, Rule,
    SCHEMA_VERSION, aliases, engine::PolicyEngine, profiles, profiles::refinement::response_rank,
};

// ---------------------------------------------------------------------------
// Round-trip
// ---------------------------------------------------------------------------

#[test]
fn hardened_roundtrips_through_toml() {
    let original = profiles::hardened();
    let ser = original.to_toml().expect("hardened serializes");
    let parsed: Policy = toml::from_str(&ser).expect("hardened parses");
    assert_eq!(
        original, parsed,
        "hardened policy round-trip must be lossless"
    );
}

#[test]
fn standard_roundtrips_through_toml() {
    let original = profiles::standard();
    let ser = original.to_toml().expect("standard serializes");
    let parsed: Policy = toml::from_str(&ser).expect("standard parses");
    assert_eq!(original, parsed);
}

#[test]
fn permissive_roundtrips_through_toml() {
    let original = profiles::permissive();
    let ser = original.to_toml().expect("permissive serializes");
    let parsed: Policy = toml::from_str(&ser).expect("permissive parses");
    assert_eq!(original, parsed);
}

// ---------------------------------------------------------------------------
// Fail-closed loader
// ---------------------------------------------------------------------------

#[test]
fn malformed_toml_falls_back_to_hardened() {
    let (policy, fell_back) = Policy::from_toml_or_hardened("<<< not toml >>>");
    assert!(fell_back, "malformed TOML must signal fall-back");
    assert_eq!(policy, profiles::hardened());
}

#[test]
fn unknown_schema_version_falls_back_to_hardened() {
    // Build a valid-shape TOML but with schema_version = 9999.
    let mut future = profiles::standard();
    future.schema_version = 9_999;
    let raw = future.to_toml().expect("serialize future policy");
    let (policy, fell_back) = Policy::from_toml_or_hardened(&raw);
    assert!(fell_back, "unknown schema_version must fall back");
    assert_eq!(policy, profiles::hardened());
}

#[test]
fn unknown_field_rejected_via_deny_unknown_fields() {
    // `extra_field` is not in the Policy schema; serde(deny_unknown_fields)
    // must reject and the fallback kicks in.
    let toml_src = r#"
schema_version = 1
profile = "Standard"
extra_field = "boom"
[defaults]
unmatched = "warn"
"#;
    let (policy, fell_back) = Policy::from_toml_or_hardened(toml_src);
    assert!(fell_back, "unknown field must trigger fall-back");
    assert_eq!(policy, profiles::hardened());
}

#[test]
fn valid_minimal_toml_parses_cleanly() {
    let toml_src = r#"
schema_version = 1
profile = "Permissive"
[defaults]
unmatched = "execute"
"#;
    let (policy, fell_back) = Policy::from_toml_or_hardened(toml_src);
    assert!(!fell_back, "valid TOML must not fall back");
    assert_eq!(policy.schema_version, SCHEMA_VERSION);
    assert_eq!(policy.profile, Profile::Permissive);
    assert_eq!(policy.defaults.unmatched, Response::Execute);
    assert!(policy.rules.is_empty());
    assert!(policy.rate_limits.is_empty());
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
    // The §4.5 / TLA+ T2 invariant in FULL: for EVERY (sequence, origin), the
    // stricter profile is never looser than the looser one, i.e.
    // rank(hardened) >= rank(standard) >= rank(permissive) (higher rank = stricter;
    // see `response_rank`). The test above only covers the unmatched default; the
    // kani `policy_monotonicity` harness covers this symbolically but is not
    // discharged on this host. Verify it here over a comprehensive CONCRETE grid:
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
    // Fail-closed invariant (kani `policy_fail_closed_on_unknown`, not discharged
    // on this host): a strict profile must never let an UNTRUSTED origin execute a
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

#[test]
fn permissive_executes_unmatched() {
    assert_eq!(profiles::permissive().defaults.unmatched, Response::Execute);
}

#[test]
fn standard_warns_on_unmatched() {
    assert_eq!(profiles::standard().defaults.unmatched, Response::Warn);
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
    let back: Policy = toml::from_str(&ser).expect("parse");
    assert_eq!(p, back);
}

// ---------------------------------------------------------------------------
// Mirror-field invariant (§4.5 + §6.1)
// ---------------------------------------------------------------------------
//
// These tests exercise the read (`mirror_snapshot`) and write
// (`set_mirror_bool`) halves of the §6.1 invariant:
//
//     policy_engine.effective_response_pty(field) == Execute
//         iff  modes.allow_<field> == true
//
// Wiring into `Terminal` / `aterm-core` is out of scope for this crate; the
// follow-up issue tests the end-to-end consistency once handlers consult the
// engine. Here we verify the local algebra: snapshot matches probe, and
// `set → snapshot` round-trips in both directions.

/// Helper: assert that `snapshot.get(field) == (effective_response_pty(field)
/// == Execute)` for every mirror field. The body of the §6.1 invariant, run
/// as a check against any engine state.
fn assert_snapshot_matches_engine(eng: &PolicyEngine, label: &str) {
    let snap = eng.mirror_snapshot();
    for field in MirrorField::ALL {
        let rsp = eng.effective_response_pty(field);
        let bool_is_exec = snap.get(field);
        assert_eq!(
            bool_is_exec,
            rsp == Response::Execute,
            "{label}: mirror snapshot out of sync with engine for {field:?} \
             (snapshot = {bool_is_exec}, effective_response_pty = {rsp:?})"
        );
    }
}

#[test]
fn mirror_snapshot_matches_effective_response_on_permissive() {
    let eng = PolicyEngine::new(profiles::permissive());
    assert_snapshot_matches_engine(&eng, "permissive");
    // Permissive's wildcard `response any` catches every Pty-origin probe
    // with Execute, and nonce is not required.
    let snap = eng.mirror_snapshot();
    assert!(snap.allow_osc52_query);
    assert!(snap.allow_osc52_set);
    assert!(snap.allow_window_ops);
    assert!(snap.allow_notifications);
    assert!(snap.allow_palette_reconfigure);
    assert!(!snap.require_shell_integration_nonce);
}

#[test]
fn mirror_snapshot_matches_effective_response_on_standard() {
    let eng = PolicyEngine::standard();
    assert_snapshot_matches_engine(&eng, "standard");
    // Standard requires the nonce; every per-field allow_* bool is observed
    // as `true` at Pty origin because the rules' strict origin_min gates
    // fall through to the `response any` wildcard (see engine::tests
    // `standard_clipboard_set_from_pty_falls_through_to_unmatched`).
    let snap = eng.mirror_snapshot();
    assert!(snap.require_shell_integration_nonce);
}

#[test]
fn mirror_snapshot_matches_effective_response_on_hardened() {
    let eng = PolicyEngine::hardened();
    assert_snapshot_matches_engine(&eng, "hardened");
    // Hardened's unmatched-default is Drop; no rule fires at Pty origin so
    // every allow_* probe returns Drop → bool = false.
    let snap = eng.mirror_snapshot();
    assert!(!snap.allow_osc52_query);
    assert!(!snap.allow_osc52_set);
    assert!(!snap.allow_window_ops);
    assert!(!snap.allow_notifications);
    assert!(!snap.allow_palette_reconfigure);
    assert!(snap.require_shell_integration_nonce);
}

#[test]
fn policy_engine_standard_matches_new_of_standard_profile() {
    // `PolicyEngine::standard` is a convenience constructor; verify it is
    // observationally equivalent to `PolicyEngine::new(profiles::standard())`.
    let a = PolicyEngine::standard();
    let b = PolicyEngine::new(profiles::standard());
    assert_eq!(a.policy(), b.policy());
    assert_eq!(a.mirror_snapshot(), b.mirror_snapshot());
}

#[test]
fn set_mirror_bool_then_effective_response_roundtrips_true() {
    // For every field and every profile, setting the bool to `true` must
    // leave `effective_response_pty == Execute`.
    for starting in [
        profiles::permissive(),
        profiles::standard(),
        profiles::hardened(),
    ] {
        for field in MirrorField::ALL {
            let mut eng = PolicyEngine::new(starting.clone());
            eng.set_mirror_bool(field, true);
            assert_eq!(
                eng.effective_response_pty(field),
                Response::Execute,
                "field {field:?} on profile {:?} did not become Execute after set(true)",
                starting.profile,
            );
            assert!(
                eng.mirror_snapshot().get(field),
                "snapshot.get({field:?}) did not become true after set(true) on profile {:?}",
                starting.profile,
            );
        }
    }
}

#[test]
fn set_mirror_bool_then_effective_response_roundtrips_false() {
    // For every field and every profile, setting the bool to `false` must
    // leave `effective_response_pty != Execute`.
    for starting in [
        profiles::permissive(),
        profiles::standard(),
        profiles::hardened(),
    ] {
        for field in MirrorField::ALL {
            let mut eng = PolicyEngine::new(starting.clone());
            eng.set_mirror_bool(field, false);
            assert_ne!(
                eng.effective_response_pty(field),
                Response::Execute,
                "field {field:?} on profile {:?} stayed Execute after set(false)",
                starting.profile,
            );
            assert!(
                !eng.mirror_snapshot().get(field),
                "snapshot.get({field:?}) stayed true after set(false) on profile {:?}",
                starting.profile,
            );
        }
    }
}

#[test]
fn mirror_invariant_holds_for_every_field_after_arbitrary_sequence() {
    // Drive a field through true → false → true → false and verify the
    // §6.1 invariant at every step. Use Hardened as the starting point so
    // the initial state is all-false.
    let mut eng = PolicyEngine::hardened();
    for field in MirrorField::ALL {
        for value in [true, false, true, false] {
            eng.set_mirror_bool(field, value);
            assert_snapshot_matches_engine(&eng, &format!("after set({field:?}, {value})"));
            assert_eq!(eng.mirror_snapshot().get(field), value);
        }
    }
}

#[test]
fn set_mirror_bool_does_not_grow_rule_list_unboundedly() {
    // Calling set_mirror_bool repeatedly for the same field must not
    // accumulate stale mirror rules: each call removes the previous
    // mirror-tagged rule before inserting the new one.
    let mut eng = PolicyEngine::hardened();
    let starting_len = eng.policy().rules.len();

    // 10 alternating toggles for every field.
    for _ in 0..5 {
        for field in MirrorField::ALL {
            eng.set_mirror_bool(field, true);
            eng.set_mirror_bool(field, false);
        }
    }

    // At most one mirror rule per rule-gated field (5 of them — the nonce
    // field does not create a rule). So the final rule count is the
    // original plus at most five.
    let final_len = eng.policy().rules.len();
    assert!(
        final_len <= starting_len + 5,
        "rule list grew unboundedly: {starting_len} → {final_len}"
    );
}

#[test]
fn set_mirror_bool_require_nonce_updates_defaults_only() {
    // The require-nonce field is not a per-sequence rule (§6.4). Writing it
    // must change defaults.shell_integration_require_nonce and leave the
    // rule list intact.
    let mut eng = PolicyEngine::hardened();
    let rule_count_before = eng.policy().rules.len();

    eng.set_mirror_bool(MirrorField::RequireShellIntegrationNonce, false);
    assert!(!eng.policy().defaults.shell_integration_require_nonce);
    assert_eq!(eng.policy().rules.len(), rule_count_before);
    assert!(!eng.mirror_snapshot().require_shell_integration_nonce);

    eng.set_mirror_bool(MirrorField::RequireShellIntegrationNonce, true);
    assert!(eng.policy().defaults.shell_integration_require_nonce);
    assert_eq!(eng.policy().rules.len(), rule_count_before);
    assert!(eng.mirror_snapshot().require_shell_integration_nonce);
}

#[test]
fn mirror_fields_are_independent() {
    // Setting one field's bool must not flip any other field's bool.
    let mut eng = PolicyEngine::hardened();
    let baseline = eng.mirror_snapshot();

    eng.set_mirror_bool(MirrorField::AllowOsc52Set, true);
    let after = eng.mirror_snapshot();

    assert!(after.allow_osc52_set);
    // Every other field must match the baseline.
    for field in MirrorField::ALL {
        if field == MirrorField::AllowOsc52Set {
            continue;
        }
        assert_eq!(
            baseline.get(field),
            after.get(field),
            "setting AllowOsc52Set disturbed {field:?}"
        );
    }
}

#[test]
fn mirror_snapshot_default_is_all_false() {
    // The `Default` derive on MirrorSnapshot must produce all-false (a
    // fail-closed baseline). The `Terminal` wiring issue depends on this to
    // safely initialize before any policy is loaded.
    let snap = MirrorSnapshot::default();
    for field in MirrorField::ALL {
        assert!(!snap.get(field));
    }
}

#[test]
fn mirror_snapshot_set_then_get_roundtrips() {
    let mut snap = MirrorSnapshot::default();
    for field in MirrorField::ALL {
        snap.set(field, true);
        assert!(snap.get(field));
        snap.set(field, false);
        assert!(!snap.get(field));
    }
}

#[test]
fn mirror_rules_survive_toml_roundtrip() {
    // Mirror rules are stored in the policy's rule vector (tagged via
    // prompt_id). They MUST round-trip through TOML so that a policy
    // serialized after set_mirror_bool deserializes to an equivalent engine.
    let mut eng = PolicyEngine::hardened();
    eng.set_mirror_bool(MirrorField::AllowOsc52Set, true);
    eng.set_mirror_bool(MirrorField::AllowNotifications, true);

    let snap_before = eng.mirror_snapshot();
    let ser = eng.policy().to_toml().expect("serialize");
    let parsed: Policy = toml::from_str(&ser).expect("parse");
    let reloaded = PolicyEngine::new(parsed);
    assert_eq!(reloaded.mirror_snapshot(), snap_before);
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
/// exhaustive without a wildcard because these tests live in the defining crate.
#[allow(dead_code)]
fn origin_exhaustiveness_guard(o: OriginTag) {
    match o {
        OriginTag::Host
        | OriginTag::ConfigFile
        | OriginTag::User
        | OriginTag::UserTyped
        | OriginTag::Ai
        | OriginTag::PtySafe
        | OriginTag::Pty
        | OriginTag::NetworkUntrusted => {}
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
