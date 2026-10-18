// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shipping [`aterm_render::font_chain::resolve_chain`] laws P1–P4 (stated in
//! `font_chain.rs`), checked over the complete input space: 8 coverage bits × 2 lazy-parse
//! bits × 4 policy bools = 16384 cases. A real-renderer section checks that a key naming a
//! face actually rasterizes.

use aterm_render::font_chain::{
    BitProbe, ChainPolicy, Resolution, RuntimeLane, Tier, io_free_mask, reachable_mask,
    resolve_chain, slot_for_source,
};
use aterm_render::{FaceId, Renderer, Theme};

/// Every policy in the domain (2^4).
fn all_policies() -> impl Iterator<Item = ChainPolicy> {
    (0u8..16).map(|b| ChainPolicy {
        runtime_discovery: b & 1 == 1,
        sealed: (b >> 1) & 1 == 1,
        wants_emoji: (b >> 2) & 1 == 1,
        // The PUA reorder (a symbol face outranks the broad chain for a private-use
        // point) is policy, so the laws must hold over it too.
        prefers_symbol: (b >> 3) & 1 == 1,
    })
}

/// Every (coverage, lazy-parse) input in the domain (2^8 × 2^2).
fn all_facts() -> impl Iterator<Item = (u8, u8)> {
    (0u16..(1 << Tier::COUNT))
        .map(|covered| u8::try_from(covered).expect("Tier::COUNT <= 8"))
        .flat_map(|covered| (0u8..4).map(move |pending| (covered, pending)))
}

#[test]
fn chain_laws_hold_over_the_complete_input_space() {
    let mut notdef = 0usize;
    let mut provisional = 0usize;
    let mut runtime_pathname = 0usize;
    let mut runtime_embedded = 0usize;
    let mut synthetic = 0usize;

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
                // Last-resort synthesis without the box-drawing bit is reached only when
                // no font tier has a real glyph; the colour face's silhouette of a text
                // point is the one font answer it outranks.
                Resolution::Face(FaceId::Procedural) if covered & Tier::Procedural.bit() == 0 => {
                    synthetic += 1;
                    assert_ne!(covered & Tier::Synthetic.bit(), 0);
                    let configured_fonts =
                        Tier::Primary.bit() | Tier::Fallback.bit() | Tier::Symbol.bit();
                    let runtime = reachable_mask(policy)
                        & (Tier::RuntimeDecisions.bit() | Tier::RuntimeEmbedded.bit());
                    let silhouette_only = covered & Tier::Color.bit() != 0 && !policy.wants_emoji;
                    assert!(
                        covered & configured_fonts == 0
                            && (silhouette_only || covered & (Tier::Color.bit() | runtime) == 0),
                        "SYNTHETIC outranked a real glyph \
                         (covered={covered:08b} {policy:?})"
                    );
                }
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

    // Non-vacuity: every outcome class is reachable.
    assert!(notdef > 0, "the honest give-up is unreachable");
    assert!(provisional > 0, "the provisional path is unreachable");
    assert!(runtime_pathname > 0, "the pathname lane is unreachable");
    assert!(
        runtime_embedded > 0,
        "the I/O-FREE bundled backstop is unreachable — this is the tofu bug"
    );
    assert!(
        synthetic > 0,
        "the fontless symbol synthesis is unreachable — this is the U+23F5 tofu"
    );
    eprintln!(
        "font-chain lattice: notdef={notdef} provisional={provisional} \
         runtime_pathname={runtime_pathname} runtime_embedded={runtime_embedded} \
         synthetic={synthetic}"
    );
}

// Real-renderer binding: only a real rasterization proves the key turns into ink.
// `from_bytes` leaves every system-font path list empty, so the compiled-in Symbols Nerd
// Font is the only face that can serve `NERD_PUA`.

fn dejavu() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/DejaVuSansMono.ttf"
    ))
    .expect("bundled DejaVu asset")
}

/// A Nerd Font PUA icon: covered by the bundled symbol face, absent from DejaVu.
#[cfg(feature = "embedded-font")]
const NERD_PUA: char = '\u{E0A0}';
/// A noncharacter no font covers — the honest `.notdef` control.
const NONCHARACTER: char = '\u{FFFF}';

fn renderer() -> Renderer {
    Renderer::from_bytes(&dejavu(), 18.0, Theme::default()).expect("fixture parses")
}

fn ink(r: &mut Renderer, ch: char) -> (FaceId, usize) {
    let key = r.glyph_key(ch);
    let img = r.glyph_image(key);
    (key.source, img.bytes().iter().filter(|&&b| b > 0).count())
}

#[cfg(feature = "embedded-font")]
#[test]
fn every_resolved_key_rasterizes_on_both_runtime_lanes() {
    // Both the pathname lane (unsealed) and the I/O-free lane (sealed) produce a key the
    // rasterizer can recover (P3 end to end).
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

/// A private-use point prefers the dedicated symbol face over the broad chain: a broad face
/// that maps a PUA point (arial maps U+F301) is following some private convention, not
/// necessarily the author's. When the symbol face lacks the point, the broad chain's
/// coverage still beats tofu.
#[test]
fn a_private_use_point_prefers_the_symbol_face_over_the_broad_chain() {
    let bit = |t: Tier| 1u8 << (t as u8);
    let both = bit(Tier::Fallback) | bit(Tier::Symbol);

    let text = ChainPolicy {
        runtime_discovery: true,
        sealed: false,
        wants_emoji: false,
        prefers_symbol: false,
    };
    let pua = ChainPolicy {
        prefers_symbol: true,
        ..text
    };

    // Both faces cover it: an assigned point takes the broad chain, a private-use point
    // the symbol face.
    assert_eq!(
        resolve_chain(&mut BitProbe::new(both, 0), text),
        Resolution::Face(FaceId::Fallback),
        "an assigned code point still takes the broad chain first"
    );
    assert_eq!(
        resolve_chain(&mut BitProbe::new(both, 0), pua),
        Resolution::Face(FaceId::SymbolFallback),
        "a private-use point takes the dedicated symbol face"
    );

    // Only the broad chain has it: a private-use point still resolves there.
    assert_eq!(
        resolve_chain(&mut BitProbe::new(bit(Tier::Fallback), 0), pua),
        Resolution::Face(FaceId::Fallback),
        "with no symbol coverage the broad chain still answers a private-use point"
    );

    // Only the symbol face has it: unchanged either way.
    for policy in [text, pua] {
        assert_eq!(
            resolve_chain(&mut BitProbe::new(bit(Tier::Symbol), 0), policy),
            Resolution::Face(FaceId::SymbolFallback),
        );
    }
}
