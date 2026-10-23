// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exhaustive [`aterm_render::font_chain::resolve_chain`] law checks (16,384 cases) and real-renderer rasterization.

use aterm_render::font_chain::{
    BitProbe, ChainPolicy, Resolution, RuntimeLane, Tier, io_free_mask, reachable_mask,
    resolve_chain, slot_for_source,
};
use aterm_render::{FaceId, Renderer, Theme};

fn all_policies() -> impl Iterator<Item = ChainPolicy> {
    (0u8..16).map(|b| ChainPolicy {
        runtime_discovery: b & 1 == 1,
        sealed: (b >> 1) & 1 == 1,
        wants_emoji: (b >> 2) & 1 == 1,
        prefers_symbol: (b >> 3) & 1 == 1,
    })
}

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

    assert_eq!(
        resolve_chain(&mut BitProbe::new(bit(Tier::Fallback), 0), pua),
        Resolution::Face(FaceId::Fallback),
        "with no symbol coverage the broad chain still answers a private-use point"
    );

    for policy in [text, pua] {
        assert_eq!(
            resolve_chain(&mut BitProbe::new(bit(Tier::Symbol), 0), policy),
            Resolution::Face(FaceId::SymbolFallback),
        );
    }
}
