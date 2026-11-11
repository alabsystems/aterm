// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::cat_glyphs_gen::GLYPH_IDS;

const fn vk(coat: u8) -> BakeKeyV4 {
    BakeKeyV4 {
        variant: CatGlyphId::S100,
        accessory: None,
        coat,
        iris: 3,
        colors: CatColorKey {
            accent: 12,
            background: 0,
        },
        w: 40,
        h: 24,
        eyes: EyesFrame::Open,
    }
}

#[test]
fn a_slot_served_this_frame_is_never_evicted_under_its_own_quad() {
    const CH: u16 = 90;
    let slots = CatBaker::slot_count(CH);
    assert_eq!(slots, 8, "fixture: a small-atlas geometry");

    let mut baker = CatBaker::default();
    let mut filled = 0usize;
    while filled < slots {
        baker.begin_frame(CH / 2, CH);
        for coat in 0..slots as u8 {
            if baker.get_v4(&vk(coat)).is_some() {
                filled = filled.max(usize::from(coat) + 1);
            }
        }
    }

    baker.begin_frame(CH / 2, CH);
    let mut served = Vec::new();
    for coat in 0..slots as u8 {
        let tile = baker
            .get_v4(&vk(coat))
            .expect("a resident key is an exact hit and never needs a bake");
        served.push(tile.art.id());
    }
    let stranger = baker.get_v4(&vk(200));

    assert!(
        stranger.is_none(),
        "every slot was served this frame, so the bake must be REFUSED \
         (callers hold a last-good tile and retry); got {stranger:?}"
    );
    for coat in 0..slots as u8 {
        assert_eq!(
            baker.peek_v4(&vk(coat)).map(|t| t.art.id()),
            Some(served[usize::from(coat)]),
            "the stranger re-baked over a slot already drawn this frame"
        );
    }
}

#[test]
fn slot_count_is_structural() {
    assert_eq!(CatBaker::slot_count(0), 0);
    assert_eq!(CatBaker::slot_count(256), 1);
    assert_eq!(CatBaker::slot_count(257), 0);
    for ch in [0u16, 1, 14, 20, 40, 56, 90, 256, 257, 1_024, u16::MAX] {
        let bytes =
            (CatBaker::slot_count(ch) as u64) * (4 * u64::from(ch)) * (2 * u64::from(ch)) * 4;
        assert!(bytes <= MAX_ATLAS_BYTES as u64, "ch={ch}: {bytes} > 2 MiB");
    }
    let mut oversize = CatBaker::default();
    oversize.begin_frame(512, u16::MAX);
    assert!(oversize.get_v4(&vk(0)).is_none());
}

#[test]
fn lru_evicts_oldest_and_version_bumps() {
    let mut b = CatBaker::default();
    b.begin_frame(10, 20);
    let cap = b.slots.len();
    assert_eq!(cap, 32);
    let v0 = b.version();
    for i in 0..cap as u8 {
        b.begin_frame(10, 20);
        assert!(b.get_v4(&vk(i)).is_some(), "slot {i} bakes");
    }
    assert_eq!(b.version(), v0 + cap as u64, "one bump per bake");
    b.begin_frame(10, 20);
    assert!(b.get_v4(&vk(0)).is_some());
    b.begin_frame(10, 20);
    assert!(b.get_v4(&vk(200)).is_some(), "cap-full insert evicts");
    assert_eq!(b.slots.iter().flatten().count(), cap, "cap never exceeded");
    assert!(
        !b.slots
            .iter()
            .flatten()
            .any(|s| s.key == SlotKey::Cat(vk(1))),
        "the least-recently-used entry was evicted"
    );
    let v1 = b.version();
    b.clear();
    assert_eq!(b.version(), v1 + 1);
    assert!(b.is_empty(), "a cleared baker holds no tile");
}

#[test]
fn bake_budget_is_two_per_frame() {
    let mut b = CatBaker::default();
    b.begin_frame(10, 20);
    assert!(b.get_v4(&vk(1)).is_some());
    assert!(b.get_v4(&vk(2)).is_some());
    assert!(
        b.get_v4(&vk(3)).is_none(),
        "third bake this frame is deferred"
    );
    assert!(b.get_v4(&vk(1)).is_some(), "a HIT still resolves");
    b.begin_frame(10, 20);
    assert!(b.get_v4(&vk(3)).is_some());
}

#[test]
fn metric_change_clears_and_bumps() {
    let mut b = CatBaker::default();
    b.begin_frame(10, 20);
    assert!(b.get_v4(&vk(0)).is_some());
    assert!(!b.is_empty());
    let v = b.version();
    b.begin_frame(12, 24);
    assert!(b.version() > v, "metric change bumps the version");
    assert!(b.is_empty(), "metric change clears");
}

#[test]
fn get_v4_rejects_art_wider_than_slot_minus_strip() {
    let mut b = CatBaker::default();
    b.begin_frame(10, 20);
    let mut k = vk(0);
    k.w = b.slot_w - PATCH_STRIP + 1;
    assert!(b.get_v4(&k).is_none(), "w + strip > slot_w ⇒ None");
}

#[test]
fn a_hit_hands_back_the_held_art_and_only_a_bake_bumps_the_version() {
    let mut b = CatBaker::default();
    b.begin_frame(10, 20);
    let a1 = b.get_v4(&vk(0)).expect("bake").art;
    let v1 = b.version();
    let a2 = b.get_v4(&vk(0)).expect("hit").art;
    assert!(Arc::ptr_eq(&a1, &a2), "a hit hands back the baked art");
    assert_eq!(b.version(), v1, "a hit bakes nothing");
    b.begin_frame(10, 20);
    let a3 = b.get_v4(&vk(1)).expect("bake 2").art;
    assert!(b.version() > v1, "a bake bumps the version");
    assert_ne!(a3.id(), a1.id(), "another key bakes another art");
}

#[test]
fn a_host_tile_is_the_hosts_texels_at_its_size() {
    let mut b = CatBaker::default();
    b.begin_frame(10, 20);
    let px: Vec<u8> = (0..6 * 5 * 4).map(|i| (i * 7 % 251) as u8).collect();
    let art = b.host_tile(0x51, 6, 5, &px).expect("fits").art;
    assert_eq!(
        (art.w(), art.h(), art.texels()),
        (6, 5, FxTexels::Rgba8Straight)
    );
    assert_eq!(art.data(), &px[..], "the art is the host's bytes");
    let held = Arc::clone(&art);
    for coat in 0..40u8 {
        b.begin_frame(10, 20);
        let _ = b.get_v4(&vk(coat));
    }
    assert!(b.host_peek(0x51).is_none(), "fixture: the tile was evicted");
    assert_eq!(
        held.data(),
        &px[..],
        "an evicted art a holder keeps is never rewritten"
    );
}

fn fills(coat: u8, iris: u8, dark: bool) -> ResolvedFills {
    ResolvedFills::from_indices(coat, iris, dark)
}

fn bake_reflection_control(
    id: CatGlyphId,
    fills: &ResolvedFills,
    w: u32,
    h: u32,
    eyes: EyesFrame,
    reflections: bool,
) -> Tile {
    let mut tile = Tile::new(w + u32::from(PATCH_STRIP), h);
    if w == 0 || h == 0 {
        return tile;
    }
    paint_glyph(
        &mut tile,
        id,
        fills,
        PathTransform::fit(w, h),
        eyes,
        reflections,
        &mut None,
    );
    let r = f32::from(PATCH_STRIP) * 0.5;
    tile.disc(w as f32 + r, r, r.max(1.0), rgb(CATCH_LIGHT), 1.0);
    tile
}

#[test]
fn v4_bake_variant_is_deterministic() {
    let a = bake_variant(CatGlyphId::S100, &fills(5, 3, false), 96, 64);
    let b = bake_variant(CatGlyphId::S100, &fills(5, 3, false), 96, 64);
    assert_eq!(a.pixels(), b.pixels(), "same key ⇒ byte-identical tile");
    assert!(a.pixels().iter().any(|&x| x != 0), "actually painted");
    assert_eq!(
        a.width(),
        96 + u32::from(PATCH_STRIP),
        "art + patch strip wide"
    );

    let key = BakeKeyV4 {
        variant: CatGlyphId::S100,
        accessory: None,
        coat: 5,
        iris: 3,
        colors: CatColorKey {
            accent: 12,
            background: 3,
        },
        w: 96,
        h: 64,
        eyes: EyesFrame::Open,
    };
    assert_eq!(key.bake().pixels(), a.pixels(), "key bake == direct bake");
}

#[test]
fn v4_coat_recolor_produces_distinct_tiles() {
    let idx = [0u8, 8, 15];
    let tiles: Vec<Vec<u8>> = idx
        .iter()
        .map(|&c| {
            bake_variant(CatGlyphId::S100, &fills(c, 3, false), 96, 64)
                .pixels()
                .to_vec()
        })
        .collect();
    for i in 0..tiles.len() {
        for j in (i + 1)..tiles.len() {
            assert_ne!(
                tiles[i], tiles[j],
                "coat ramp {} vs {} must bake distinct",
                idx[i], idx[j]
            );
        }
    }
}

#[test]
fn v4_fixed_patterned_variant_is_coat_invariant() {
    let def = &GLYPHS[CatGlyphId::SpecTuxedo as usize];
    assert!(
        def.layers.iter().all(|l| l.recolor != Recolor::Coat),
        "tuxedo must carry no Recolor::Coat layer (patterned coat is fixed)"
    );
    let a = bake_variant(CatGlyphId::SpecTuxedo, &fills(0, 3, false), 96, 96);
    let b = bake_variant(CatGlyphId::SpecTuxedo, &fills(15, 3, false), 96, 96);
    assert_eq!(a.pixels(), b.pixels(), "fixed patches ⇒ coat-invariant");
    assert!(a.pixels().iter().any(|&x| x != 0), "actually painted");
}

#[test]
fn dark_coats_are_lifted_on_dark_backgrounds_only() {
    let lum = |c: (f32, f32, f32)| relative_luminance(packed_rgb(c));
    let dark_ctx = CatColorKey {
        accent: 12,
        background: 0,
    };
    let light_ctx = CatColorKey {
        accent: 12,
        background: 3,
    };
    let band1_ctx = CatColorKey {
        accent: 12,
        background: 1,
    };
    for coat in 0..4u8 {
        let rescued = ResolvedFills::from_context(coat, 4, dark_ctx);
        assert!(
            lum(rescued.coat) >= COAT_MIN_LUM_DARK_BG - 0.005,
            "coat {coat} clears the floor on band 0, got {}",
            lum(rescued.coat)
        );
        let unlifted = ResolvedFills::from_context(coat, 4, light_ctx);
        assert!(
            lum(unlifted.coat) < COAT_MIN_LUM_DARK_BG,
            "coat {coat} keeps its authored darkness on light bands"
        );
        let band1 = ResolvedFills::from_context(coat, 4, band1_ctx);
        let raw = rgb(mix_rgb(
            COAT_RAMP[usize::from(coat)],
            CONTEXT_ACCENTS[12],
            0.12,
        ));
        assert_eq!(band1.coat, raw, "coat {coat} is bit-exact on band 1");
    }
    let pale = ResolvedFills::from_context(13, 4, dark_ctx);
    let raw = rgb(mix_rgb(COAT_RAMP[13], CONTEXT_ACCENTS[12], 0.12));
    assert_eq!(pale.coat, raw, "coats above the floor are bit-exact");
    let charcoal = ResolvedFills::from_context(2, 4, dark_ctx);
    let (h_before, ..) = rgb2hsv(mix_rgb(COAT_RAMP[2], CONTEXT_ACCENTS[12], 0.12));
    let (h_after, ..) = rgb2hsv(packed_rgb(charcoal.coat));
    assert!(
        lum(charcoal.coat) >= COAT_MIN_LUM_DARK_BG - 0.005,
        "coat 2 was lifted"
    );
    assert!(
        (h_before - h_after).abs() < 12.0,
        "the rescue preserves the coat's hue family ({h_before}° → {h_after}°)"
    );
    let lifted: Vec<_> = (0..4u8)
        .map(|c| ResolvedFills::from_context(c, 4, dark_ctx).coat)
        .map(packed_rgb)
        .collect();
    for i in 0..lifted.len() {
        for j in i + 1..lifted.len() {
            assert_ne!(lifted[i], lifted[j], "coats {i} and {j} stay distinct");
        }
    }
}

#[test]
fn dark_average_footprint_keeps_the_pale_outline() {
    let dark_bg = 0x000A_0A10;
    let band2_bg = 0x007A_7A7A;
    assert_eq!(background_band(band2_bg), 2, "test constant is band 2");
    let mid_clip = CatColorKey::from_rgb_span(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0, 0, 2);
    assert_eq!(
        mid_clip.background, 0,
        "a decisively dark average with a mid clip keeps the pale outline"
    );
    let bright_clip = CatColorKey::from_rgb_span(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0, 0, 3);
    assert_eq!(
        bright_clip.background, 4,
        "a bright strip inside a dark average stays mixed"
    );
    let mixed = CatColorKey::from_rgb_span(band2_bg, 0x00C0_C8D0, 0x00A0_A8B0, 1, 3);
    assert_eq!(mixed.background, 4, "a true crossover span stays mixed");
    let uniform = CatColorKey::from_rgb_span(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0, 0, 1);
    assert_eq!(
        uniform,
        CatColorKey::from_rgb(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0),
        "uniform spans are unchanged"
    );
}

#[test]
fn context_palette_is_bounded_and_color_aware() {
    assert_eq!(std::mem::size_of::<CatColorKey>(), 2);
    let bg = 0x001A_1B26;
    let red_a = CatColorKey::from_rgb(bg, 0x00F0_6878, 0x00E8_7180);
    let red_b = CatColorKey::from_rgb(bg, 0x00EE_6A79, 0x00E5_7381);
    let blue = CatColorKey::from_rgb(bg, 0x0068_A8F0, 0x0071_B0E8);
    assert_eq!(red_a, red_b, "nearby colors reuse one atlas tile");
    assert_ne!(
        red_a.accent, blue.accent,
        "surrounding hue changes the art palette"
    );
    for color in [red_a, red_b, blue] {
        assert!(color.accent <= 12);
        assert!(color.background <= 3);
    }
    let red_fill = ResolvedFills::from_context(8, 4, red_a);
    let blue_fill = ResolvedFills::from_context(8, 4, blue);
    assert_ne!(red_fill.coat, blue_fill.coat);
    assert_ne!(red_fill.iris, blue_fill.iris);
}

#[test]
fn context_palette_default_is_neutral_and_hue_families_are_cardinal() {
    assert_eq!(
        CatColorKey::default(),
        CatColorKey {
            accent: 12,
            background: 0,
        },
        "an absent context must not invent a red tint"
    );
    assert_eq!(
        CatColorKey::from_rgb(0, 0x00CC_CCCC, 0x0088_8888).accent,
        12,
        "achromatic text selects the neutral family"
    );

    for (rgb, expected) in [
        (0x00FF_0000, 0),
        (0x00FF_8000, 1),
        (0x00FF_FF00, 2),
        (0x0000_FF00, 4),
        (0x0000_FFFF, 6),
        (0x0000_00FF, 8),
        (0x00FF_00FF, 10),
    ] {
        assert_eq!(
            CatColorKey::from_rgb(0, rgb, rgb).accent,
            expected,
            "cardinal color #{rgb:06X}"
        );
    }
    for red_edge in [0x00FF_0004, 0x00FF_0400] {
        assert_eq!(
            CatColorKey::from_rgb(0, red_edge, red_edge).accent,
            0,
            "the circular red seam must share one cache key"
        );
    }
}

#[test]
fn exterior_whiskers_follow_context_ink_but_internal_features_stay_fixed() {
    let fills = ResolvedFills::from_context(
        8,
        4,
        CatColorKey {
            accent: 6,
            background: 0,
        },
    );
    let layer = |role| Layer {
        role,
        recolor: Recolor::Fixed,
        fill: 0x0011_2233,
        paths: &[],
    };
    assert_eq!(
        resolve_layer(&layer(GlyphRole::Outline), &fills).0,
        fills.outline
    );
    assert_eq!(
        resolve_layer(&layer(GlyphRole::Whisker), &fills).0,
        fills.outline
    );
    assert_eq!(
        resolve_layer(&layer(GlyphRole::Eye), &fills).0,
        rgb(0x0011_2233),
        "internal face ink must preserve the authored expression"
    );
}

#[test]
fn context_outline_clears_wcag_contrast_at_every_band_edge() {
    const MIN_CONTRAST: f32 = 3.0;
    let bands = [
        (0.0, BG_BAND_DARKEST_MAX),
        (BG_BAND_DARKEST_MAX, BG_BAND_DARK_MAX),
        (BG_BAND_DARK_MAX, BG_BAND_MID_MAX),
        (BG_BAND_MID_MAX, 1.0),
    ];
    for (background, (lo, hi)) in bands.into_iter().enumerate() {
        for accent in 0..=12 {
            let outline = ResolvedFills::from_context(
                8,
                4,
                CatColorKey {
                    accent,
                    background: background as u8,
                },
            )
            .outline;
            let channel = |value: f32| (value * 255.0).round() as u32;
            let packed =
                (channel(outline.0) << 16) | (channel(outline.1) << 8) | channel(outline.2);
            let outline_luminance = relative_luminance(packed);
            for background_luminance in [lo, hi] {
                let contrast = (outline_luminance.max(background_luminance) + 0.05)
                    / (outline_luminance.min(background_luminance) + 0.05);
                assert!(
                    contrast >= MIN_CONTRAST,
                    "band {background}, accent {accent}, bg L={background_luminance:.3}, \
                     outline #{packed:06X} (L={outline_luminance:.3}) has {contrast:.3}:1"
                );
            }
        }
    }
}

#[test]
fn mixed_background_dual_keyline_covers_every_luminance() {
    const MIN_CONTRAST: f32 = 3.0;
    for accent in 0..=12 {
        let colors = CatColorKey::from_rgb_span(
            0x007F_7F7F,
            0x00EE_EEEE,
            0x0088_99AA,
            CatColorKey::background_band(0x0000_0000),
            CatColorKey::background_band(0x00FF_FFFF),
        );
        assert_eq!(colors.background, 4);
        let fills = ResolvedFills::from_context(8, 4, CatColorKey { accent, ..colors });
        let dark_luminance = relative_luminance(packed_rgb(fills.outline));
        let pale_luminance = relative_luminance(packed_rgb(
            fills
                .outline_underlay
                .expect("mixed context has a pale underlay"),
        ));
        for gray in 0..=255u32 {
            let background_luminance = relative_luminance((gray << 16) | (gray << 8) | gray);
            let contrast = |ink: f32| {
                (ink.max(background_luminance) + 0.05) / (ink.min(background_luminance) + 0.05)
            };
            let best = contrast(dark_luminance).max(contrast(pale_luminance));
            assert!(
                best >= MIN_CONTRAST,
                "mixed accent {accent}, gray {gray} (L={background_luminance:.3}): \
                 best dual-keyline contrast is {best:.3}:1"
            );
        }
    }
}

#[test]
fn mixed_background_bake_contains_both_keyline_inks() {
    let fills = ResolvedFills::from_context(
        8,
        4,
        CatColorKey {
            accent: 12,
            background: 4,
        },
    );
    let dark = packed_rgb(fills.outline);
    let pale = packed_rgb(fills.outline_underlay.expect("mixed underlay"));
    let tile = bake_variant(CatGlyphId::S100, &fills, 96, 96);
    let has_rgb = |wanted: u32| {
        tile.pixels().as_chunks::<4>().0.iter().any(|px| {
            px[3] > 0
                && (u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2])) == wanted
        })
    };
    assert!(has_rgb(dark), "mixed bake contains its dark keyline");
    assert!(has_rgb(pale), "mixed bake contains its pale halo");
}

#[test]
fn v4_accessory_overlay_changes_texels() {
    let bare = bake_variant(CatGlyphId::S100, &fills(5, 3, false), 96, 96);
    let bow = bake_variant_with(
        CatGlyphId::S100,
        Some(CatGlyphId::AccBow),
        &fills(5, 3, false),
        96,
        96,
        EyesFrame::Open,
    );
    assert_ne!(bare.pixels(), bow.pixels(), "overlay must change texels");
}

#[test]
fn eyes_frame_bakes_distinct_but_open_is_authored() {
    let f = fills(5, 3, false);
    let open = bake_variant_with(CatGlyphId::S100, None, &f, 96, 64, EyesFrame::Open);
    let happy = bake_variant_with(CatGlyphId::S100, None, &f, 96, 64, EyesFrame::Happy);
    let blink = bake_variant_with(CatGlyphId::S100, None, &f, 96, 64, EyesFrame::Blink);
    let plain = bake_variant(CatGlyphId::S100, &f, 96, 64);
    assert_eq!(open.pixels(), plain.pixels(), "Open == default open bake");
    assert_ne!(open.pixels(), happy.pixels(), "a squint changes texels");
    assert_ne!(open.pixels(), blink.pixels(), "a blink changes texels");
    assert_ne!(happy.pixels(), blink.pixels(), "squint and blink differ");
}

#[test]
fn flying_cursor_reflections_skip_happy_and_blink_eyes() {
    const W: u32 = 38;
    const H: u32 = 31;
    let fills = fills(8, 4, false);
    for state in [EyesFrame::Happy, EyesFrame::Blink] {
        let without = bake_reflection_control(CatGlyphId::S100, &fills, W, H, state, false);
        let with = bake_reflection_control(CatGlyphId::S100, &fills, W, H, state, true);
        assert_eq!(
            without.pixels(),
            with.pixels(),
            "{state:?} must never receive open-eye reflections"
        );
    }
}

#[test]
fn flying_reflection_roster_is_contained_and_nonvacuous() {
    let mut failures = Vec::new();
    let mut eligible = 0usize;
    let mut exercised = 0usize;
    let mut preview_exercised = false;
    let mut live_expression_exercised = false;
    for &id in GLYPH_IDS {
        let def = &GLYPHS[id as usize];
        for h in [16u32, 26, 31, 48, 120] {
            let w = (h as f32 * f32::from(def.aspect_x1000) / 1000.0)
                .round()
                .max(1.0) as u32;
            let xform = PathTransform::fit(w, h);
            let mut eligible_indices = Vec::new();
            if let (Some(eyes), Some(catch_lights)) = (
                def.layers.iter().find(|layer| layer.role == GlyphRole::Eye),
                def.layers
                    .iter()
                    .find(|layer| layer.role == GlyphRole::CatchLight),
            ) {
                for catch_light in catch_lights.paths {
                    let Some((cx0, cy0, cx1, cy1)) = path_bounds(catch_light) else {
                        continue;
                    };
                    let (pcx, pcy) = ((cx0 + cx1) * 0.5, (cy0 + cy1) * 0.5);
                    let Some((eye, bounds)) = closest_eye_path(eyes, pcx, pcy) else {
                        continue;
                    };
                    if open_eye_reflection_eligible(bounds, xform)
                        && !eligible_indices.contains(&eye)
                    {
                        eligible_indices.push(eye);
                    }
                }
            }

            let mut eligible_masks = Vec::new();
            if let Some(eyes) = def.layers.iter().find(|layer| layer.role == GlyphRole::Eye) {
                for &eye in &eligible_indices {
                    let mut mask = Tile::new(w, h);
                    fill_path_fixed(
                        &mut mask,
                        std::slice::from_ref(&eyes.paths[eye]),
                        (0.0, 0.0, 0.0),
                        1.0,
                        xform,
                    );
                    eligible_masks.push((eye, mask));
                }
            }

            for dark_bg in [false, true] {
                let fills = fills(8, 4, dark_bg);
                let plain = bake_reflection_control(id, &fills, w, h, EyesFrame::Open, false);
                let glossy = bake_reflection_control(id, &fills, w, h, EyesFrame::Open, true);
                let tile_w = plain.width() as usize;
                let changed: Vec<usize> = plain
                    .pixels()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(glossy.pixels().as_chunks::<4>().0)
                    .enumerate()
                    .filter(|(pixel, (a, b))| pixel % tile_w < w as usize && a != b)
                    .map(|(pixel, _)| pixel)
                    .collect();

                if eligible_masks.is_empty() && !changed.is_empty() {
                    failures.push(format!(
                        "{} h={h} dark_bg={dark_bg}: ineligible glyph changed at {:?}",
                        def.id, changed
                    ));
                }
                for &pixel in &changed {
                    let x = pixel % tile_w;
                    let y = pixel / tile_w;
                    let mi = (y * w as usize + x) * 4;
                    let owners: Vec<usize> = eligible_masks
                        .iter()
                        .filter_map(|(eye, mask)| (mask.pixels()[mi + 3] != 0).then_some(*eye))
                        .collect();
                    let before = &plain.pixels()[pixel * 4..pixel * 4 + 4];
                    if owners.is_empty() || before[3] != 255 || !before[..3].iter().all(|&c| c < 96)
                    {
                        failures.push(format!(
                            "{} h={h} dark_bg={dark_bg}: changed ({x},{y}) \
                             owners={owners:?}, before={before:?}",
                            def.id
                        ));
                    }
                }

                for (eye, mask) in &eligible_masks {
                    eligible += 1;
                    let local: Vec<usize> = changed
                        .iter()
                        .copied()
                        .filter(|pixel| {
                            let x = pixel % tile_w;
                            let y = pixel / tile_w;
                            mask.pixels()[(y * w as usize + x) * 4 + 3] != 0
                        })
                        .collect();
                    if local.is_empty() {
                        continue;
                    }
                    exercised += 1;
                    preview_exercised |= id == CatGlyphId::S100 && h == 31;
                    live_expression_exercised |= id == CatGlyphId::S121 && h == 31;
                    let dark_count = |tile: &Tile| {
                        mask.pixels()
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .enumerate()
                            .filter(|(pixel, mask_px)| {
                                if mask_px[3] == 0 {
                                    return false;
                                }
                                let x = pixel % w as usize;
                                let y = pixel / w as usize;
                                let i = (y * tile_w + x) * 4;
                                let px = &tile.pixels()[i..i + 4];
                                px[3] == 255 && px[..3].iter().all(|&c| c < 96)
                            })
                            .count()
                    };
                    let before = dark_count(&plain);
                    let after = dark_count(&glossy);
                    if after == 0 || after * 2 < before {
                        failures.push(format!(
                            "{} h={h} dark_bg={dark_bg}: eye {eye} lost its \
                             dark core, before={before}, after={after}",
                            def.id
                        ));
                    }
                }
            }
        }
    }
    assert!(
        eligible > 0,
        "the roster sweep must find eligible open eyes"
    );
    assert!(
        exercised > 0 && preview_exercised && live_expression_exercised,
        "the roster sweep must affect both S100 preview and live S121 cursor heads"
    );
    assert!(
        failures.is_empty(),
        "the generic flying reflection pass regressed:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn accessory_overlay_preserves_each_authored_aspect() {
    let (w, h) = (96, 72);
    for accessory in [
        CatGlyphId::AccBow,
        CatGlyphId::AccCrown,
        CatGlyphId::AccBell,
    ] {
        let transform = accessory_transform(accessory, w, h);
        let authored = f32::from(GLYPHS[accessory as usize].aspect_x1000) / 1000.0;
        let rendered = transform.scale_x / transform.scale_y;
        assert!(
            (rendered - authored).abs() <= 0.001,
            "{accessory:?}: overlay aspect {rendered:.3} != authored {authored:.3}"
        );
        let (_, _, scale) = accessory_attach(accessory);
        assert!(transform.scale_x <= scale * w as f32 + f32::EPSILON);
        assert!(transform.scale_y <= scale * h as f32 + f32::EPSILON);
    }
}

#[test]
fn get_v4_reuses_scratch_tiles_and_matches_direct_bake() {
    let mut baker = CatBaker::default();
    baker.begin_frame(10, 20);
    let first = BakeKeyV4 {
        variant: CatGlyphId::S121,
        eyes: EyesFrame::Open,
        ..vk(2)
    };
    let t0 = baker.get_v4(&first).expect("first bake fits");
    let scratch_ptr = baker
        .scratch
        .as_ref()
        .expect("scratch tile retained")
        .pixels()
        .as_ptr();
    let mask_ptr = baker
        .eye_mask_scratch
        .as_ref()
        .expect("eye mask scratch retained")
        .pixels()
        .as_ptr();

    for (coat, eyes, w) in [
        (2, EyesFrame::Happy, 40),
        (2, EyesFrame::Blink, 40),
        (7, EyesFrame::Open, 40),
        (9, EyesFrame::Open, 34),
    ] {
        baker.begin_frame(10, 20);
        let key = BakeKeyV4 {
            variant: CatGlyphId::S121,
            coat,
            w,
            eyes,
            ..vk(coat)
        };
        let placed = baker.get_v4(&key).expect("subsequent bake fits");
        assert_eq!(
            baker.scratch.as_ref().unwrap().pixels().as_ptr(),
            scratch_ptr,
            "same-or-smaller bake must reuse destination scratch tile buffer"
        );
        assert_eq!(
            baker.eye_mask_scratch.as_ref().unwrap().pixels().as_ptr(),
            mask_ptr,
            "open-eye reflection mask scratch buffer must stay resident and reused"
        );
        let direct = key.bake();
        let tw = direct.width() as usize;
        let aw = usize::from(key.w);
        assert_eq!(
            (placed.art.w(), placed.art.h()),
            (key.w, key.h),
            "the art is the design box"
        );
        for row in 0..usize::from(key.h) {
            let d = row * aw * 4;
            let s = row * tw * 4;
            assert_eq!(
                &placed.art.data()[d..d + aw * 4],
                &direct.pixels()[s..s + aw * 4],
                "reused scratch bake must match fresh key.bake() texels (coat={coat}, eyes={eyes:?}, w={w}, row={row})"
            );
        }
    }
    let _ = t0;
    baker.clear();
    assert!(baker.scratch.is_none() && baker.eye_mask_scratch.is_none());
}
