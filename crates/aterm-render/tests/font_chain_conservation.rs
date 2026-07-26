// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for the glyph-resolution chain (the `❯` U+276F tofu class): the
//! SHIPPING [`aterm_render::font_chain::resolve_chain`] laws, enumerated over the
//! COMPLETE input space, plus a prove-and-catch mutant and a real-renderer
//! binding.
//!
//! ## Three tiers, one property set
//!
//! * **Tier-0 (abstract, `ty` + the in-process interpreter)** —
//!   `aterm_spec::derive::{font_chain_seal_independence_model,
//!   font_key_recovery_lane_model, fallback_convergence_model}`, discharged by
//!   `cargo test -p aterm-spec --test derived_ring_ty`.
//! * **Tier-1 (concrete, this file)** — the chain's inputs are 7 coverage bits ×
//!   2 lazy-parse bits × 3 policy bools = 4096 cases. That is not a sample: it is
//!   the entire domain, so this is a complete proof of P1–P4 for the real policy.
//! * **Tier-2 (symbolic, trust-mc)** — `aterm_render::font_chain::kani_proofs`,
//!   discharged by `KANI_CRATE=aterm-render scripts/verify-kani-proofs.sh`.
//!
//! The real-renderer section at the bottom binds the policy to actual ink, which
//! no pure proof can do: a key that NAMES a face still has to rasterize.

use aterm_render::font_chain::{
    BitProbe, ChainPolicy, ChainProbe, DecisionCells, Resolution, RuntimeLane, Tier, io_free_mask,
    reachable_mask, resolve_chain, slot_for_source,
};
use aterm_render::{FaceId, Renderer, Theme};

/// Every policy in the domain (2^3).
fn all_policies() -> impl Iterator<Item = ChainPolicy> {
    (0u8..8).map(|b| ChainPolicy {
        runtime_discovery: b & 1 == 1,
        sealed: (b >> 1) & 1 == 1,
        wants_emoji: (b >> 2) & 1 == 1,
    })
}

/// Every (coverage, lazy-parse) input in the domain (2^7 × 2^2).
fn all_facts() -> impl Iterator<Item = (u8, u8)> {
    (0u8..(1 << Tier::COUNT)).flat_map(|covered| (0u8..4).map(move |pending| (covered, pending)))
}

/// THE MUTANT — the shipped pre-fix resolver, in which sealing a generation also
/// set `runtime_discovery = false` (one merged flag instead of two independent
/// gates). Kept here, not in the library, so the gate proves the property CATCHES
/// the real regression rather than being vacuously true.
fn resolve_chain_prefix<P: ChainProbe + ?Sized>(probe: &mut P, policy: ChainPolicy) -> Resolution {
    resolve_chain(
        probe,
        ChainPolicy {
            runtime_discovery: policy.runtime_discovery && !policy.sealed,
            sealed: false,
            ..policy
        },
    )
}

#[test]
fn chain_laws_hold_over_the_complete_input_space() {
    let mut notdef = 0usize;
    let mut provisional = 0usize;
    let mut runtime_pathname = 0usize;
    let mut runtime_embedded = 0usize;

    for (covered, pending) in all_facts() {
        for policy in all_policies() {
            let mut probe = BitProbe::new(covered, pending);
            let r = resolve_chain(&mut probe, policy);

            // P2 TOTALITY / ORDERING.
            assert!(
                probe.ordered,
                "P2: a tier was re-probed or probed out of order \
                 (covered={covered:07b} pending={pending:02b} {policy:?})"
            );
            assert!(
                probe.calls as usize <= Tier::COUNT,
                "P2: {} probes exceeds the {} declared tiers",
                probe.calls,
                Tier::COUNT
            );

            // P1 CONSERVATION + the give-up/lazy-parse lemma.
            if r == Resolution::Notdef {
                notdef += 1;
                assert_eq!(
                    covered & reachable_mask(policy),
                    0,
                    "P1: gave up on a code point a reachable tier covers \
                     (covered={covered:07b} {policy:?})"
                );
                assert_eq!(
                    pending, 0,
                    "P1': memoizable give-up while a lazy face was still parsing"
                );
            }
            if r == Resolution::Provisional {
                provisional += 1;
                assert_ne!(pending, 0, "P1': provisional without a parse in flight");
            }

            // P3 KEY/RASTER AGREEMENT.
            if let Some(slot) = r.recovery_slot() {
                assert_eq!(
                    slot_for_source(r.key_source()),
                    Some(slot),
                    "P3: {r:?} wrote {slot:?} but its key's face reads a different record"
                );
            }

            match r {
                Resolution::Runtime(RuntimeLane::Decisions) => runtime_pathname += 1,
                Resolution::Runtime(RuntimeLane::EmbeddedDecisions) => runtime_embedded += 1,
                _ => {}
            }

            // P4 SEAL MONOTONICITY, both forms.
            let unsealed = ChainPolicy {
                sealed: false,
                ..policy
            };
            let sealed = ChainPolicy {
                sealed: true,
                ..policy
            };
            let before = resolve_chain(&mut BitProbe::new(covered, pending), unsealed);
            let after = resolve_chain(&mut BitProbe::new(covered, pending), sealed);
            if after == Resolution::Notdef {
                assert_eq!(
                    covered & io_free_mask(sealed),
                    0,
                    "P4a: sealing hid an I/O-free tier (covered={covered:07b} {policy:?})"
                );
                assert!(
                    before == Resolution::Notdef
                        || before == Resolution::Runtime(RuntimeLane::Decisions),
                    "P4b: sealing downgraded {before:?} to `.notdef` \
                     (covered={covered:07b} {policy:?})"
                );
            }
        }
    }

    // NON-VACUITY: every outcome class is actually reachable, so none of the
    // implications above is trivially true.
    assert!(notdef > 0, "the honest give-up is unreachable");
    assert!(provisional > 0, "the provisional path is unreachable");
    assert!(runtime_pathname > 0, "the pathname lane is unreachable");
    assert!(
        runtime_embedded > 0,
        "the I/O-FREE bundled backstop is unreachable — this is the tofu bug"
    );
    eprintln!(
        "font-chain lattice: notdef={notdef} provisional={provisional} \
         runtime_pathname={runtime_pathname} runtime_embedded={runtime_embedded}"
    );
}

#[test]
fn the_laws_catch_the_shipped_seal_regression() {
    // The mutant must violate P4 (and therefore P1) somewhere in the domain.
    let mut caught_p4a = 0usize;
    let mut caught_p4b = 0usize;
    for (covered, pending) in all_facts() {
        for policy in all_policies() {
            let sealed = ChainPolicy {
                sealed: true,
                ..policy
            };
            let unsealed = ChainPolicy {
                sealed: false,
                ..policy
            };
            let before = resolve_chain(&mut BitProbe::new(covered, pending), unsealed);
            let mutant = resolve_chain_prefix(&mut BitProbe::new(covered, pending), sealed);
            if mutant == Resolution::Notdef {
                if covered & io_free_mask(sealed) != 0 {
                    caught_p4a += 1;
                }
                if !(before == Resolution::Notdef
                    || before == Resolution::Runtime(RuntimeLane::Decisions))
                {
                    caught_p4b += 1;
                }
            }
        }
    }
    assert!(
        caught_p4a > 0,
        "P4a does not catch the pre-fix resolver — the property is too weak"
    );
    assert!(
        caught_p4b > 0,
        "P4b does not catch the pre-fix resolver — the property is too weak"
    );
    eprintln!("mutant counterexamples: P4a={caught_p4a} P4b={caught_p4b}");
}

#[test]
fn a_recorded_runtime_decision_is_recoverable_on_either_lane() {
    for lane in [RuntimeLane::Decisions, RuntimeLane::EmbeddedDecisions] {
        let mut cells = DecisionCells::default();
        cells.record(lane, Some(7));
        assert_eq!(
            cells.parts_for(),
            Some(7),
            "P3(b): a decision recorded on {lane:?} was unrecoverable — the key would \
             say RuntimeFallback while the rasterizer drew `.notdef`"
        );
    }
    // A cached MISS is recoverable as "no face", never as a stale index.
    let mut miss = DecisionCells::default();
    miss.record(RuntimeLane::EmbeddedDecisions, None);
    assert_eq!(miss.parts_for(), None);
}

// ---------------------------------------------------------------------------
// REAL-RENDERER BINDING — the policy above decides WHICH face; only a real
// rasterization proves the key turns into ink. Machine-independent by
// construction: `from_bytes` leaves every system-font path list empty, so the
// compiled-in Symbols Nerd Font is the only thing that can serve `NERD_PUA`.
// ---------------------------------------------------------------------------

fn dejavu() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/DejaVuSansMono.ttf"
    ))
    .expect("bundled DejaVu asset")
}

/// A Nerd Font PUA icon: covered by the bundled symbol face, absent from DejaVu.
const NERD_PUA: char = '\u{E0A0}';
/// A noncharacter no font covers — the honest `.notdef` control.
const NONCHARACTER: char = '\u{FDD2}';

fn renderer() -> Renderer {
    Renderer::from_bytes(&dejavu(), 18.0, Theme::default()).expect("fixture parses")
}

fn ink(r: &mut Renderer, ch: char) -> (FaceId, usize) {
    let key = r.glyph_key(ch);
    let img = r.glyph_image(key);
    (key.source, img.bytes().iter().filter(|&&b| b > 0).count())
}

#[test]
fn every_resolved_key_rasterizes_on_both_runtime_lanes() {
    // Pathname lane (unsealed) and I/O-free lane (sealed) must BOTH produce a key
    // whose face the rasterizer can recover — P3 end to end.
    for sealed in [false, true] {
        let mut r = renderer();
        if sealed {
            r.seal_admitted_font_sources();
        }
        let (source, ink_px) = ink(&mut r, NERD_PUA);
        assert_eq!(
            source,
            FaceId::RuntimeFallback,
            "sealed={sealed}: the bundled symbol face must serve U+{:04X}",
            NERD_PUA as u32
        );
        assert!(
            ink_px > 0,
            "sealed={sealed}: the key named {source:?} but rasterized empty — \
             a `.notdef` wearing a fallback face id"
        );
    }
}

#[test]
fn a_genuine_miss_still_gives_up() {
    let mut r = renderer();
    r.seal_admitted_font_sources();
    let (source, _) = ink(&mut r, NONCHARACTER);
    assert_eq!(
        source,
        FaceId::Primary,
        "the resolver must not invent coverage for a noncharacter"
    );
}
