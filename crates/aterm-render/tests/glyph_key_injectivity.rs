// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for glyph-key injectivity: pixel size is part of every
//! [`aterm_render::GlyphKey`], so one glyph rasterized at two sizes never collides in the
//! shared cache or atlas. Windows at 1× and 2× share one glyph cache on the strength of this.
//!
//! Tier-0 is the `KeyInjectivity` model (`aterm_spec::derive::key_injectivity_model`,
//! invariant `NoCollision`), proved by `ty` in aterm-spec's
//! `derived_key_injectivity_proves_and_catches_size_collision`. This file checks the same
//! invariant against the shipping `GlyphKey` `Eq`/`Hash` over a dense lattice of real keys.

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
    FaceId::DisplayMix,
    FaceId::RuntimeFallback,
];

/// Every pixel class.
const CLASSES: &[GlyphClass] = &[
    GlyphClass::Mono,
    GlyphClass::Rgba,
    GlyphClass::RgbaGid,
    GlyphClass::RgbaCluster,
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
        cell_span: 0,
        px_q,
    }
}

/// `NoCollision` over every key in the lattice: the same glyph at 1× (13 px) and 2× (26 px)
/// is two keys, distinct by `Eq` and by `Hash`.
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

                    // Equality is not trivially false, which would make the check vacuous.
                    assert_eq!(a, a, "a key must equal itself");
                    assert_eq!(a, key(source, glyph_class, id, style, px_1x));

                    // NoCollision: same glyph, two sizes -> two distinct keys.
                    assert_ne!(
                        a, b,
                        "keys differing only in px must not be equal \
                         (source={source:?} class={glyph_class:?} id={id:#x} style={style:?})"
                    );

                    // Hash-distinct: the two sizes occupy independent slots.
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
                    // A re-lookup at each size returns its own entry.
                    assert_eq!(cache.get(&a), Some(&(serial - 2)));
                    assert_eq!(cache.get(&b), Some(&(serial - 1)));
                    compared += 1;
                }
            }
        }
    }

    // Non-vacuity: the differ-only-in-px branch ran over the whole field product.
    assert!(
        compared >= FACES.len() * CLASSES.len() * STYLES.len() * IDS.len(),
        "the lattice must exercise every (face, class, style, id) at both sizes ({compared} points)"
    );
    // No cross-field collision either.
    assert_eq!(
        cache.len(),
        compared * 2,
        "every lattice key must be distinct"
    );
}

/// A one-unit px_q step (the finest 26.6 step) is already a distinct key: quantization does
/// not coalesce nearby sizes into one raster.
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
    // Keys of one glyph order by px_q, so atlas packing is stable frame to frame.
    assert!(a < b, "keys of the same glyph order by ascending px_q");
}

/// The materialized cell span is part of the key: after an ambiguous-width reload one scalar
/// can coexist at one and two cells at the same px, and the two need separate slots.
#[test]
fn glyph_key_separates_materialized_fallback_spans() {
    let narrow = GlyphKey {
        cell_span: 1,
        ..key(
            FaceId::Fallback,
            GlyphClass::Mono,
            0x00B7,
            StyleBits::REGULAR,
            GlyphKey::quantize_px(16.0),
        )
    };
    let wide = GlyphKey {
        cell_span: 2,
        ..narrow
    };
    assert_ne!(narrow, wide);
    let mut cache = HashMap::new();
    cache.insert(narrow, "one-cell");
    cache.insert(wide, "two-cell");
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.get(&narrow), Some(&"one-cell"));
    assert_eq!(cache.get(&wide), Some(&"two-cell"));
}
