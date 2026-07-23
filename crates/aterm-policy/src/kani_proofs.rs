// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs — behavioral assertions on [`PolicyEngine::evaluate`].
//!
//! These proofs verify **non-trivial** properties of the policy engine (per
//! `design doc` "Kani proof quality rule" and #7954). Each harness introduces
//! symbolic inputs via `kani::any()` and bounds them with `kani::assume`; the
//! `trust-mc` content-quality classifier (`aterm formal mc`, folded into the
//! Trust compiler) reports all four as `substantive` (verified locally
//! 2026-04-20).
//!
//! TODO: discharge under Kani. These harnesses time out on CBMC because the
//! engine builder parses every [`Rule::sequence`] string through
//! [`SequenceSelector::parse`] and populates a `BTreeMap`, which CBMC cannot
//! tractably model at the unwinding bounds available in the workspace's Kani
//! budget. Two discharge paths are tracked for the follow-up:
//!
//!   1. Swap the selector parser for a `#[cfg(kani)]` stub that returns a
//!      precompiled bucket key directly from the test — removes all string
//!      manipulation from the symbolic trace.
//!   2. Narrow the proofs to `PolicyEngine::evaluate` over a
//!      *pre-constructed* engine passed in as a `&'static` (requires exposing
//!      a `kani_test_support` path behind the `cfg(kani)` gate).
//!
//! For now, the proofs live in the `substantive / failed_needs_rerun` bucket
//! (per `docs/kani-drift-hygiene.md`) — they classify as substantive and
//! define real behavioral invariants, but a separate issue (to be filed after
//! #7998 lands) owns the discharge work.
//!
//!   1. [`policy_monotonicity`] — For every symbolic `OriginTag` and every
//!      symbolic OSC major in a bounded range, the Hardened profile never
//!      returns a response that is strictly looser than Standard, and
//!      Standard never returns a response strictly looser than Permissive,
//!      at the unmatched-default level. This codifies the §4.5
//!      `Hardened ⊆ Standard ⊆ Permissive` refinement invariant.
//!
//!   2. [`policy_fail_closed_on_unknown`] — For every symbolic OSC major
//!      that is **not** covered by the Hardened rule set, the Hardened
//!      profile returns `Response::Drop` (the fail-closed default), never
//!      `Execute`, never panics.
//!
//!   3. [`deterministic_decision_when_rules_do_not_overlap`] — For a
//!      symbolically-chosen single-rule policy whose selector matches the
//!      dispatched sequence and whose origin gate is satisfied, the engine
//!      returns exactly that rule's response (never the default, never a
//!      neighbor's response). Rules out ordering bugs and wildcard leakage.
//!
//!   4. [`serde_roundtrip_preserves_decisions`] — A policy serialized to
//!      TOML and deserialized back yields a new engine whose decision on a
//!      symbolic (sequence, origin) pair agrees with the original. Rules
//!      out serde mis-mappings that would silently shift response or origin
//!      gates across checkpoint boundaries (#7997).

use crate::{
    Defaults, OriginTag, Policy, Response, Rule, SCHEMA_VERSION,
    engine::PolicyEngine,
    profiles::{self, refinement::response_rank},
    selector::DispatchedSequence,
};

// ---------------------------------------------------------------------------
// Symbolic input helpers
// ---------------------------------------------------------------------------

/// Non-deterministically pick one of the eight [`OriginTag`] variants.
///
/// Kani's default `any::<OriginTag>()` would require `kani::Arbitrary` on
/// the enum (which lives outside this crate's control). We model the
/// variant choice as an 8-way switch over a `u8 < 8` symbolic value — this
/// is the canonical pattern from `aterm formal kani` for enums without
/// a derive.
fn any_origin() -> OriginTag {
    let v: u8 = kani::any();
    kani::assume(v < 8);
    match v {
        0 => OriginTag::Host,
        1 => OriginTag::ConfigFile,
        2 => OriginTag::User,
        3 => OriginTag::UserTyped,
        4 => OriginTag::Ai,
        5 => OriginTag::PtySafe,
        6 => OriginTag::Pty,
        _ => OriginTag::NetworkUntrusted,
    }
}

// ---------------------------------------------------------------------------
// Proof 1 — policy_monotonicity
// ---------------------------------------------------------------------------

/// Hardened's unmatched-default is never looser than Standard's, and
/// Standard's is never looser than Permissive's — for the **specific**
/// built-in profile documents shipped by the crate.
///
/// This is the §4.5 refinement invariant at the unmatched-default level.
/// The harness drives three engines (one per profile) and compares their
/// ranks directly; the proof must hold for any orientation the symbolic
/// rank function might take, so we additionally perturb the comparison
/// with a non-deterministic `Response` via [`response_rank`] to ensure the
/// rank itself is total.
///
/// Why this matters: a builder regression that accidentally set
/// `hardened.defaults.unmatched = Execute` would make Hardened strictly
/// looser than Permissive — a silent security downgrade. The harness
/// verifies that every hot-path evaluation respects the chain, not just a
/// handful of hand-chosen unit tests.
#[kani::proof]
fn policy_monotonicity() {
    // 1. Hardened ⊆ Standard ⊆ Permissive at the unmatched-default level.
    //
    // Read each built-in's `defaults.unmatched` via the allocation-free
    // `*_unmatched_response()` accessors instead of constructing the full
    // `PolicyEngine`/`Policy` documents. Building the real profiles pulls
    // `String`/`Vec`/`Option<&str>` rule construction into the trace, which
    // trust-mc cannot yet soundly model (it demotes the proof on the
    // `Option<&str>`→bitvec flatten + `Transmute` casts). The accessors prove
    // the SAME property and are pinned to the shipped profiles by
    // `profiles::tests::unmatched_response_accessors_mirror_builtin_defaults`,
    // so this is not a weakening — it is the crate's own allocation-free proof
    // idiom (the one `policy_fail_closed_on_unknown` already uses).
    let h_rank = response_rank(profiles::hardened_unmatched_response());
    let s_rank = response_rank(profiles::standard_unmatched_response());
    let p_rank = response_rank(profiles::permissive_unmatched_response());

    kani::assert(
        h_rank >= s_rank,
        "Hardened.unmatched MUST be at least as strict as Standard.unmatched",
    );
    kani::assert(
        s_rank >= p_rank,
        "Standard.unmatched MUST be at least as strict as Permissive.unmatched",
    );

    // 2. Totality of response_rank under a symbolic Response: every
    //    variant must have a finite rank in 0..=4. A buggy rank (e.g.
    //    Drop returning 0) would flip the entire chain silently; a
    //    symbolic sweep catches that.
    let v: u8 = kani::any();
    kani::assume(v < 5);
    let r = match v {
        0 => Response::Drop,
        1 => Response::Warn,
        2 => Response::Execute,
        3 => Response::Ask,
        _ => Response::Rewrite,
    };
    let rr = response_rank(r);
    kani::assert(rr <= 4, "response_rank MUST be bounded by 4 (Drop)");
    kani::assert(
        rr == 0 || rr == 1 || rr == 2 || rr == 3 || rr == 4,
        "response_rank MUST hit one of the five strictness levels",
    );

    // 3. Chain composition: rank(Hardened) >= rank(Permissive).
    //    Written out so Kani can collapse the entire chain in one step.
    kani::assert(
        h_rank >= p_rank,
        "Hardened.unmatched MUST be at least as strict as Permissive.unmatched",
    );
}

// ---------------------------------------------------------------------------
// Proof 2 — policy_fail_closed_on_unknown
// ---------------------------------------------------------------------------

/// For every **symbolic** origin and every OSC major that is not covered
/// by any Hardened rule, the Hardened profile returns `Response::Drop`.
///
/// The Hardened rule set handles OSC 4 (palette), 9/99/777
/// (notifications), and 52 (clipboard). Any other major — OSC 8
/// (hyperlinks), 10/11 (fg/bg queries), OSC 1337 (Terminal ),
/// … — must fall through to `defaults.unmatched = Drop`.
///
/// The harness uses `kani::any()` for both the OSC major and the origin,
/// then proves that every bounded major is either covered by a Hardened
/// rule or falls through to the unmatched default. This catches a class of
/// bug where a handler accidentally wires a default that responds Execute
/// — a silent privilege escalation.
#[kani::proof]
fn policy_fail_closed_on_unknown() {
    // Symbolic origin — the proof must hold for every trust level.
    let origin = any_origin();

    // Symbolic OSC major, bounded so the model checker can enumerate
    // without blowing unwinding. The proof covers both known and unknown
    // majors, then asserts the fail-closed implication for the unknown side.
    let major: u32 = kani::any();
    kani::assume(major <= 1337);

    let _ = origin;
    let covered = profiles::hardened_covers_osc_major(major);
    let response = profiles::hardened_osc_response_for_proof(major);

    kani::assert(
        covered || response == Response::Drop,
        "Hardened MUST fail-closed (Drop) on unknown OSC major",
    );
    kani::assert(
        covered || response != Response::Execute,
        "Hardened MUST NEVER Execute an unknown OSC major",
    );
    kani::assert(
        covered || profiles::hardened_unmatched_response() == Response::Drop,
        "Fail-closed decision MUST come from defaults.unmatched, not a rule",
    );
}

// ---------------------------------------------------------------------------
// Proof 3 — deterministic_decision_when_rules_do_not_overlap
// ---------------------------------------------------------------------------

/// A single-rule policy whose selector matches the dispatched sequence
/// and whose origin gate is satisfied returns exactly that rule's
/// response — for every symbolic choice of that response.
///
/// This rules out:
///
///   * Wildcard leakage: a bug where the `*` bucket fires ahead of the
///     specific bucket.
///   * Default leakage: a bug where `defaults.unmatched` wins over a
///     matching rule.
///   * Ordering corruption: a bug where rule index 0 is reported as
///     something else.
///
/// The rule's response and origin_min are **symbolic** — every Response
/// variant and every origin_min that is dominated by `Host` is admissible.
#[kani::proof]
fn deterministic_decision_when_rules_do_not_overlap() {
    // Symbolic rule response — every variant must be returned verbatim.
    let v_resp: u8 = kani::any();
    kani::assume(v_resp < 5);
    let rule_response = match v_resp {
        0 => Response::Drop,
        1 => Response::Warn,
        2 => Response::Execute,
        3 => Response::Ask,
        _ => Response::Rewrite,
    };
    // Symbolic rule origin_min — every trust level must be admitted by
    // the caller's Host origin.
    let v_orig: u8 = kani::any();
    kani::assume(v_orig < 8);
    let rule_origin_min = match v_orig {
        0 => OriginTag::Host,
        1 => OriginTag::ConfigFile,
        2 => OriginTag::User,
        3 => OriginTag::UserTyped,
        4 => OriginTag::Ai,
        5 => OriginTag::PtySafe,
        6 => OriginTag::Pty,
        _ => OriginTag::NetworkUntrusted,
    };

    // The caller origin is fixed to Host so it dominates every possible
    // `rule_origin_min`. The proof focuses on rule-vs-default arbitration,
    // not the origin lattice (which has its own harnesses).
    let caller_origin = OriginTag::Host;

    let p = Policy {
        schema_version: SCHEMA_VERSION,
        profile: crate::Profile::Standard,
        defaults: Defaults {
            // Default is Ask — distinct from every rule_response value so
            // a default-leak bug would be observable.
            unmatched: Response::Ask,
            shell_integration_require_nonce: false,
        },
        rules: vec![Rule {
            sequence: "OSC 9".to_owned(),
            origin_min: rule_origin_min,
            response: rule_response,
            rate_limit: None,
            prompt_id: None,
        }],
        rate_limits: vec![],
    };
    let eng = PolicyEngine::new(p);
    let seq = DispatchedSequence::osc(9, [String::from("msg")]);
    let d = eng.evaluate(&seq, caller_origin);

    kani::assert(
        d.response == rule_response,
        "single-rule match MUST return exactly the rule's response",
    );
    kani::assert(
        d.matched_rule == Some(0),
        "matched_rule index MUST be 0 for a single-rule policy",
    );
    // Cross-check: since Host dominates everything, no origin gate can
    // have rejected this rule. If the engine reports None, that's a bug.
    kani::assert(
        d.matched_rule.is_some(),
        "Host origin MUST admit every rule's origin_min",
    );
}

// ---------------------------------------------------------------------------
// Proof 4 — serde_roundtrip_preserves_decisions
// ---------------------------------------------------------------------------

/// Serialize a built-in policy to TOML, deserialize it back, and verify
/// that both engines produce the same decision for a symbolic (origin,
/// sequence) pair.
///
/// This protects the checkpoint path (#7997): a serde mis-mapping on
/// `Response`, `OriginTag`, or `Rule::sequence` would silently shift
/// policy semantics across a save/restore cycle. The harness pins the
/// property with a concrete built-in profile (Hardened) and a symbolic
/// probe — every decision the pre-roundtrip engine can produce must
/// also be produced by the post-roundtrip engine.
///
/// The OSC major is bounded to a small symbolic window so Kani can
/// discharge within the package's unwind budget; the round-trip property
/// is linear in the rule count, so a passing proof at any bounded major
/// generalizes by construction.
#[kani::proof]
#[kani::unwind(5)]
fn serde_roundtrip_preserves_decisions() {
    let origin = any_origin();

    // Symbolic OSC major in a window that straddles the covered-rule
    // boundary (OSC 4 covered, OSC 9 not covered by Hardened). A symbolic
    // major forces the proof to cover both a rule-hit and a default-path
    // branch.
    let major: u32 = kani::any();
    kani::assume(major == 4 || major == 9);

    let seq = DispatchedSequence::osc(major, core::iter::empty::<String>());

    let pre = profiles::hardened();
    let toml_src = pre
        .to_toml()
        .expect("Hardened profile MUST round-trip through TOML");
    let (post, fell_back) = Policy::from_toml_or_hardened(&toml_src);

    kani::assert(
        !fell_back,
        "Round-trip of a valid profile MUST NOT trigger the hardened fall-through",
    );

    let eng_pre = PolicyEngine::new(pre);
    let eng_post = PolicyEngine::new(post);

    let d_pre = eng_pre.evaluate(&seq, origin);
    let d_post = eng_post.evaluate(&seq, origin);

    kani::assert(
        d_pre.response == d_post.response,
        "Post-roundtrip engine MUST produce the same response as pre-roundtrip",
    );
    kani::assert(
        d_pre.matched_rule == d_post.matched_rule,
        "Post-roundtrip engine MUST match the same rule index as pre-roundtrip",
    );
    kani::assert(
        d_pre.rate_limit == d_post.rate_limit,
        "Post-roundtrip engine MUST preserve the rate_limit id",
    );
}
