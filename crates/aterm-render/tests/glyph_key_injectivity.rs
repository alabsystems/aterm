// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for the W12 GLYPH-KEY INJECTIVITY law: pixel size is part of
//! every [`aterm_render::GlyphKey`] by construction, so a glyph rasterized at one
//! size can NEVER collide in the shared cache/atlas with the SAME glyph rasterized
//! at another size.
//!
//! This is the enabler behind retiring aterm's single shared-backend font size: a
//! laptop window at 1× and an external-monitor window at 2× draw the SAME glyphs at
//! DIFFERENT physical pixel sizes THROUGH ONE shared glyph cache. That is only sound
//! because two keys differing only in `px_q` are distinct — otherwise the 2× raster
//! would be served to the 1× window (and vice versa).
//!
//! ## Two-tier proof
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `KeyInjectivity` derived model (`aterm_spec::derive::key_injectivity_model`)
//!   carries the `NoCollision` invariant. `cargo test -p aterm-spec`
//!   (`derived_key_injectivity_proves_and_catches_size_collision`) runs the REAL
//!   `ty` binary over the whole bounded space: it PROVES the invariant at `Buggy=0`
//!   and CATCHES a px-dropping key at `Buggy=1` (the two sizes alias) → counterexample.
//! * **Tier-1 (concrete, this file)** — the SAME `NoCollision` invariant checked
//!   directly against the shipping `GlyphKey` `Eq`/`Hash`: over a dense lattice of
//!   real keys (every `FaceId` × `GlyphClass` × style × sample code point / glyph id)
//!   at the 1× and 2× physical sizes, two keys that agree on every field EXCEPT px
//!   are never equal and never share a `HashMap` slot — with a non-vacuity control
//!   (the differ-only-in-px branch is genuinely exercised) and a negative control
//!   reproducing the pre-enabler defect (a key that drops px DOES collide).

use aterm_render::{FaceId, GlyphClass, GlyphKey, StyleBits};
use std::collections::HashMap;

/// Every `FaceId` variant — a glyph can key from any source.
const FACES: &[FaceId] = &[
    FaceId::Primary,
    FaceId::BoldPrimary,
    FaceId::Fallback,
    FaceId::SymbolFallback,
    FaceId::Procedural,
    FaceId::ColorEmoji,
    FaceId::ColorEmojiMono,
    FaceId::RuntimeFallback,
];

/// Every pixel class.
const CLASSES: &[GlyphClass] = &[
    GlyphClass::Mono,
    GlyphClass::Rgba,
    GlyphClass::RgbaGid,
    GlyphClass::MonoGid,
];

/// The style-bit combinations that participate in a key's identity.
const STYLES: &[StyleBits] = &[
    StyleBits::REGULAR,
    StyleBits::BOLD,
    StyleBits::ITALIC,
    StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
];

/// A spread of code points / glyph ids: ASCII, a wide CJK scalar, an emoji scalar,
/// a shade-phase-folded id (above the scalar range), and low glyph ids.
const IDS: &[u32] = &[0x41, 0x4E2D, 0x1F680, 0x0100_2591, 0, 1, 42, 0xFFFF];

/// Build a key with the given fields at a specific 26.6-quantized px.
fn key(
    source: FaceId,
    glyph_class: GlyphClass,
    ch_or_id: u32,
    style: StyleBits,
    px_q: u32,
) -> GlyphKey {
    GlyphKey {
        source,
        glyph_class,
        ch_or_id,
        style,
        px_q,
    }
}

/// THE INVARIANT (the same `NoCollision` that `ty` model-checks abstractly in
/// aterm-spec): over every real key in the lattice, the SAME glyph at two different
/// physical sizes is two DISTINCT keys — distinct by `Eq`, distinct by `Hash`
/// (independent `HashMap` slots). Exhaustive over the field product at the concrete
/// 1× (13 px) and 2× (26 px) DPI pair — the exact mixed-DPI scenario.
#[test]
fn glyph_key_separates_sizes_never_collides() {
    // The two physical sizes a 1× and a 2× window rasterize the base 13 px font at.
    let px_1x = GlyphKey::quantize_px(13.0);
    let px_2x = GlyphKey::quantize_px(26.0);
    assert_ne!(px_1x, px_2x, "the two DPI sizes must quantize distinctly");

    let mut cache: HashMap<GlyphKey, u32> = HashMap::new();
    let mut compared = 0usize;
    let mut serial = 0u32;
    for &source in FACES {
        for &glyph_class in CLASSES {
            for &style in STYLES {
                for &id in IDS {
                    let a = key(source, glyph_class, id, style, px_1x);
                    let b = key(source, glyph_class, id, style, px_2x);

                    // Sanity: a key equals itself and its clone (equality is not
                    // trivially always-false, which would make injectivity vacuous).
                    assert_eq!(a, a, "a key must equal itself");
                    assert_eq!(a, key(source, glyph_class, id, style, px_1x));

                    // NoCollision: same glyph, two sizes -> two distinct keys.
                    assert_ne!(
                        a, b,
                        "keys differing only in px must not be equal \
                         (source={source:?} class={glyph_class:?} id={id:#x} style={style:?})"
                    );

                    // Hash-distinct: inserting both grows the map by exactly two, so
                    // the 2× raster never overwrites (is never served to) the 1× slot.
                    let before = cache.len();
                    cache.insert(a, serial);
                    serial += 1;
                    cache.insert(b, serial);
                    serial += 1;
                    assert_eq!(
                        cache.len(),
                        before + 2,
                        "the two sizes must occupy independent cache slots \
                         (source={source:?} class={glyph_class:?} id={id:#x} style={style:?})"
                    );
                    // And a re-lookup at each size returns ITS OWN entry, not the other's.
                    assert_eq!(cache.get(&a), Some(&(serial - 2)));
                    assert_eq!(cache.get(&b), Some(&(serial - 1)));
                    compared += 1;
                }
            }
        }
    }

    // NON-VACUITY: the differ-only-in-px branch was genuinely exercised across the
    // whole field product (mirrors the model's reachable Compute at Buggy=0).
    assert!(
        compared >= FACES.len() * CLASSES.len() * STYLES.len() * IDS.len(),
        "the lattice must exercise every (face, class, style, id) at both sizes ({compared} points)"
    );
    // Every key is distinct in the cache (no accidental cross-field collision either).
    assert_eq!(
        cache.len(),
        compared * 2,
        "every lattice key must be distinct"
    );
}

/// A change of px_q of ONE 26.6 unit (the finest representable step) is already a
/// distinct key — the quantization does not silently coalesce nearby sub-pixel
/// sizes into one raster.
#[test]
fn glyph_key_smallest_px_step_is_distinct() {
    let base = GlyphKey::quantize_px(13.0);
    let a = key(
        FaceId::Primary,
        GlyphClass::Mono,
        0x41,
        StyleBits::REGULAR,
        base,
    );
    let b = key(
        FaceId::Primary,
        GlyphClass::Mono,
        0x41,
        StyleBits::REGULAR,
        base + 1,
    );
    assert_ne!(a, b, "a one-unit px_q difference is a distinct key");
    // Ordering is total and size-monotone within a fixed glyph, so atlas packing is
    // stable frame to frame (the `Ord` derive the cache relies on).
    assert!(a < b, "keys of the same glyph order by ascending px_q");
}

/// NEGATIVE CONTROL: the pre-enabler world where the key did NOT carry px (one cache
/// could host only one size). Projecting a key onto its non-px fields makes the two
/// sizes COLLIDE — exactly the aliasing the `px_q` component prevents. This shows the
/// injectivity is load-bearing, not incidental.
#[test]
fn dropping_px_from_the_key_reproduces_the_collision() {
    let px_1x = GlyphKey::quantize_px(13.0);
    let px_2x = GlyphKey::quantize_px(26.0);
    let a = key(
        FaceId::Primary,
        GlyphClass::Mono,
        0x41,
        StyleBits::REGULAR,
        px_1x,
    );
    let b = key(
        FaceId::Primary,
        GlyphClass::Mono,
        0x41,
        StyleBits::REGULAR,
        px_2x,
    );

    // With px in the key: distinct (the fix).
    assert_ne!(a, b);

    // The pre-fix key: drop px_q, keeping only (source, class, ch_or_id, style).
    let project = |k: &GlyphKey| (k.source, k.glyph_class, k.ch_or_id, k.style);
    assert_eq!(
        project(&a),
        project(&b),
        "without px the two sizes alias -> the 2x raster would be served to the 1x window"
    );

    // A px-less cache would hold ONE entry for both -> a genuine collision.
    let mut pxless: HashMap<(FaceId, GlyphClass, u32, StyleBits), &str> = HashMap::new();
    pxless.insert(project(&a), "1x-raster");
    pxless.insert(project(&b), "2x-raster");
    assert_eq!(
        pxless.len(),
        1,
        "the px-less key collides the two sizes into one slot"
    );
    assert_eq!(
        pxless.get(&project(&a)),
        Some(&"2x-raster"),
        "the later (2x) insert clobbers the 1x raster — the defect W12 keys prevent"
    );
}
