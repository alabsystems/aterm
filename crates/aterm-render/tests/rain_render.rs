// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// PHOSPHOR digital rain on the CPU renderer (`RenderInput.rain_quads` +
// `rain_atlas` + `rain_add`). The contract under test:
//   * empty rain channels (never touched AND explicitly emptied — and quads
//     empty WITH an atlas set) are byte-identical to the pre-rain path, also
//     after `clear_overlays` (the `image plain` contract);
//   * the rain stamp shares the cat regime: NEAREST 1:1, `mul8` tint/alpha,
//     `flip_x` mirroring, and every painted pixel stays inside the quad's
//     one-row cell band;
//   * pass-1c z-order: rain draws UNDER the row's glyphs and UNDER `cat_quads`
//     (cats walk on rain), matching the GPU's
//     `bg → scene_over → RainUnder → cat_over → glyphs` stream order;
//   * `rain_add` is a radial, premultiplied `add_sat` post-pass: byte-exact
//     against hand-computed integer falloff and monotonically brightening;
//   * damaged path: animating rain (moved rows, a mutation-tick all-rows
//     change, an atlas-version rebake) re-renders with no ghosting
//     (cached == fresh, byte-for-byte);
//   * dirty gate: settled rain gate-hits with a byte-stable framebuffer; the
//     per-row sorted-slice merge-diff marks ONLY the rows whose slice differs
//     (prev∪cur); an atlas-version bump alone marks all quad rows; a moved
//     halo marks its prev∪cur rows.

use std::sync::Arc;

use aterm_core::render::{RainHalo, SceneAtlas, SpriteQuad};
use aterm_core::terminal::Terminal;
use aterm_render::{
    DamageOutcome, DirtyDecision, Frame, Renderer, Theme, WindowCpu, compute_dirty_rows, rgb_to_u32,
};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

/// A deterministic, fully-opaque patterned RGBA atlas (per-texel distinct
/// colours so a wrong NEAREST index or a missed `flip_x` mirror shows up).
fn patterned_atlas(w: u32, h: u32, version: u64) -> SceneAtlas {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 53) as u8,
                (x * 29 + y * 3) as u8,
                255,
            ]);
        }
    }
    SceneAtlas {
        width: w,
        height: h,
        rgba,
        version,
    }
}

/// A solid single-colour atlas with a chosen per-texel alpha.
fn solid_atlas(w: u32, h: u32, rgb: [u8; 3], alpha: u8, version: u64) -> SceneAtlas {
    let rgba = (0..w * h)
        .flat_map(|_| [rgb[0], rgb[1], rgb[2], alpha])
        .collect();
    SceneAtlas {
        width: w,
        height: h,
        rgba,
        version,
    }
}

/// Opaque untinted quad: `dest = [x, y, w, h]`, `src = [ax, ay, aw, ah]`.
fn quad(row: u16, dest: [u16; 4], src: [u16; 4]) -> SpriteQuad {
    SpriteQuad {
        row,
        x: dest[0],
        y: dest[1],
        w: dest[2],
        h: dest[3],
        ax: src[0],
        ay: src[1],
        aw: src[2],
        ah: src[3],
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }
}

/// A rain quad occupying its row's band at `(col·cw, row·ch)` for one cell,
/// sourced 1:1 from atlas texel `(ax, ay)` — the engine's bake==dest contract.
fn band_quad(row: u16, x: u16, cw: u16, ch: u16, ax: u16, ay: u16) -> SpriteQuad {
    quad(row, [x, row * ch, cw, ch], [ax, ay, cw, ch])
}

fn halo(row: u16, x: u16, y: u16, w: u16, h: u16, color: u32) -> RainHalo {
    RainHalo {
        row,
        x,
        y,
        w,
        h,
        color,
        cx: x + w / 2,
        cy: y + h / 2,
        rx: (w / 2).max(1),
        ry: (h / 2).max(1),
        // Defaulted `mode: HaloMode::Add` — the historical light.
        ..Default::default()
    }
}

/// The stamp's channel math, restated independently: round-half 8-bit multiply.
fn mul8(c: u32, f: u32) -> u32 {
    (c * f + 127) / 255
}

/// Hand-computed per-channel saturating add (the additive contract, restated
/// independently of `aterm_render::add_sat`).
fn sat_add(dst: u32, premul: u32) -> u32 {
    let ch = |sh: u32| (((dst >> sh) & 0xff) + ((premul >> sh) & 0xff)).min(255);
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Empty rain channels — untouched, explicitly emptied, or atlas-only — must be
/// byte-identical to the pre-rain path; `clear_overlays` restores the bare
/// frame (the `image plain` capture contract).
#[test]
fn rain_disabled_bytes_identical() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25lrainy planet");

    // Pre-feature frame: the snapshot as built, rain never mentioned.
    let base = rend.render_input(&term.cell_frame(3, 12)).pixels.clone();

    // The SAME frame with every rain channel set to its explicit empty state.
    let mut input = term.cell_frame(3, 12);
    input.rain_quads = Vec::new();
    input.rain_atlas = None;
    input.rain_add = Vec::new();
    let explicit = rend.render_input(&input).pixels.clone();
    assert_eq!(
        base, explicit,
        "explicit empty rain channels must not change any pixel"
    );

    // Empty quads WITH an atlas set: the atlas alone draws nothing.
    input.rain_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
    let atlas_only = rend.render_input(&input).pixels.clone();
    assert_eq!(
        base, atlas_only,
        "a rain atlas with no quads must draw nothing"
    );

    // `clear_overlays` strips rain like every other bling layer: both quad
    // Vecs cleared AND the atlas Arc nulled.
    let mut with_rain = term.cell_frame(3, 12);
    with_rain.rain_atlas = Some(Arc::new(patterned_atlas(32, 32, 1)));
    with_rain.rain_quads = vec![band_quad(1, 0, cw as u16, ch.min(32) as u16, 0, 0)];
    with_rain.rain_add = vec![halo(1, 0, ch as u16, cw as u16, 2, 0x0020_4020)];
    let painted = rend.render_input(&with_rain).pixels.clone();
    assert_ne!(base, painted, "non-empty rain must paint something");
    with_rain.clear_overlays();
    assert!(
        with_rain.rain_quads.is_empty(),
        "clear_overlays must strip rain quads"
    );
    assert!(
        with_rain.rain_atlas.is_none(),
        "clear_overlays must null the rain atlas Arc"
    );
    assert!(
        with_rain.rain_add.is_empty(),
        "clear_overlays must strip rain halos"
    );
    let stripped = rend.render_input(&with_rain).pixels.clone();
    assert_eq!(base, stripped, "clear_overlays must restore the bare frame");
}

/// Stamp correctness: tint and alpha flow through the shared `mul8` math
/// (opaque texel × alpha 255 ⇒ the exact `mul8(texel, tint)` product per
/// channel — blend at coverage 255 is the identity), the alpha path uses the
/// same rounding (`mul8(127, 1) == 0` paints nothing; `mul8(128, 1) == 1`
/// paints), `flip_x` mirrors the source columns, and NOTHING lands outside the
/// quad's one-row cell band.
#[test]
fn rain_stamp_mul8_tint_alpha_flip_and_band_containment() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(4, 12);
    term.process(b"\x1b[?25l");
    let base = rend.render_input(&term.cell_frame(4, 12)).pixels.clone();

    // Tint through mul8 at a non-endpoint: solid (200,200,200) opaque texels,
    // tint 0x80_40_C0, alpha 255 ⇒ every painted pixel is exactly
    // (mul8(200,0x80), mul8(200,0x40), mul8(200,0xC0)).
    let mut input = term.cell_frame(4, 12);
    input.rain_atlas = Some(Arc::new(solid_atlas(64, 64, [200, 200, 200], 255, 1)));
    let mut q = band_quad(1, 0, cw as u16, ch.min(64) as u16, 0, 0);
    q.tint = 0x0080_40C0;
    input.rain_quads = vec![q];
    let f = rend.render_input(&input);
    let want = (mul8(200, 0x80) << 16) | (mul8(200, 0x40) << 8) | mul8(200, 0xC0);
    let band_h = ch.min(64);
    for dy in 0..band_h {
        for dx in 0..cw {
            let got = f.pixels[(ch + dy) * f.width + dx];
            assert_eq!(
                got, want,
                "tinted opaque stamp must be the exact mul8 product at ({dx},{dy})"
            );
        }
    }
    // Band containment: every changed pixel lies inside row 1's pixel band.
    for (i, (&b, &p)) in base.iter().zip(f.pixels.iter()).enumerate() {
        if b != p {
            let y = i / f.width;
            assert!(
                (ch..ch + band_h).contains(&y),
                "rain stamp must stay inside its cell band: pixel row {y} changed"
            );
        }
    }

    // Alpha through mul8, pinned at the rounding boundary: quad alpha 1 over
    // texel alpha 127 ⇒ mul8 == 0 ⇒ NOTHING painted; texel alpha 128 ⇒
    // mul8 == 1 ⇒ the band changes.
    let mut input = term.cell_frame(4, 12);
    input.rain_atlas = Some(Arc::new(solid_atlas(64, 64, [255, 255, 255], 127, 2)));
    let mut q = band_quad(1, 0, cw as u16, ch.min(64) as u16, 0, 0);
    q.alpha = 1;
    input.rain_quads = vec![q];
    let f = rend.render_input(&input);
    assert_eq!(
        base, f.pixels,
        "mul8(127, 1) == 0: a fully-rounded-away alpha must paint nothing"
    );
    let mut input = term.cell_frame(4, 12);
    input.rain_atlas = Some(Arc::new(solid_atlas(64, 64, [255, 255, 255], 128, 3)));
    input.rain_quads = vec![q];
    let f = rend.render_input(&input);
    assert_ne!(
        base, f.pixels,
        "mul8(128, 1) == 1: one unit of coverage must survive the rounding"
    );

    // flip_x mirrors the source columns: at 1:1, dest (dx,dy) reads texel
    // (aw-1-dx, dy). Opaque untinted patterned atlas ⇒ exact byte compare.
    let atlas = patterned_atlas(64, 64, 4);
    let (aw, ah) = (cw.min(64) as u16, ch.min(64) as u16);
    let mut input = term.cell_frame(4, 12);
    input.rain_atlas = Some(Arc::new(patterned_atlas(64, 64, 4)));
    let mut q = quad(2, [3, 2 * ch as u16, aw, ah], [5, 7, aw, ah]);
    q.flip_x = true;
    input.rain_quads = vec![q];
    let f = rend.render_input(&input);
    for dy in 0..ah as usize {
        for dx in 0..aw as usize {
            let sx = 5 + (aw as usize - 1 - dx);
            let i = ((7 + dy) * 64 + sx) * 4;
            let want = ((atlas.rgba[i] as u32) << 16)
                | ((atlas.rgba[i + 1] as u32) << 8)
                | atlas.rgba[i + 2] as u32;
            let got = f.pixels[(2 * ch + dy) * f.width + 3 + dx];
            assert_eq!(
                got, want,
                "flip_x must mirror the source column at ({dx},{dy})"
            );
        }
    }
}

/// Pass-1c z-order: rain draws UNDER the row's glyphs (a full-block cell stays
/// pure fg) and UNDER `cat_quads` (a cat covering the rain wins) — the
/// `scene_over → rain → cat → glyphs` order; cats walk on rain.
#[test]
fn rain_under_text_and_under_cats() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(2, 8);
    term.process("\x1b[?25l█".as_bytes());

    let rain_green = 0x0020_C840;
    let cat_red = 0x00C8_2020;
    let mut input = term.cell_frame(2, 8);
    input.rain_atlas = Some(Arc::new(solid_atlas(8, 8, [0x20, 0xC8, 0x40], 255, 1)));
    // One row-0 rain quad across cells 0..3 (band-wide, opaque, untinted).
    input.rain_quads = vec![quad(0, [0, 0, (3 * cw) as u16, ch as u16], [0, 0, 8, 8])];
    // A cat over cell 2 of the same row.
    input.cat_atlas = Some(Arc::new(solid_atlas(8, 8, [0xC8, 0x20, 0x20], 255, 1)));
    input.cat_quads = vec![quad(
        0,
        [(2 * cw) as u16, 0, cw as u16, ch as u16],
        [0, 0, 8, 8],
    )];

    let f = rend.render_input(&input);
    let fg = rgb_to_u32(input.cells[0][0].fg);
    assert!(
        cell_pixels(&f, cw, ch, 0, 0).iter().all(|&p| p == fg),
        "the full-block glyph must draw OVER the rain (cell 0 pure fg)"
    );
    assert!(
        cell_pixels(&f, cw, ch, 0, 1)
            .iter()
            .all(|&p| p == rain_green),
        "the uncovered rain must be the exact rain colour (cell 1 pure green)"
    );
    assert!(
        cell_pixels(&f, cw, ch, 0, 2).iter().all(|&p| p == cat_red),
        "the cat must draw OVER the rain (cell 2 pure red — cats walk on rain)"
    );
}

/// `rain_add` is a radial premultiplied saturating add: byte-exact against the
/// independently restated integer falloff and `min(255, dst + src)` channel
/// math over the rendered base frame, and it can only ever brighten a pixel.
#[test]
fn rain_add_is_saturating_add_and_only_brightens() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25lglow");
    let base = rend.render_input(&term.cell_frame(3, 10)).pixels.clone();

    // A halo overlapping both glyph pixels and bare background on row 0, plus
    // a near-saturating halo on row 1 to exercise the clamp.
    let dim = 0x0018_3010;
    let hot = 0x00F0_F0F0;
    let mut input = term.cell_frame(3, 10);
    input.rain_add = vec![
        halo(0, 2, 1, (3 * cw) as u16, (ch - 2) as u16, dim),
        halo(1, 0, ch as u16, (2 * cw) as u16, ch as u16, hot),
    ];
    let f = rend.render_input(&input);

    // Frame geometry: the grid is padded; recover pad from the frame dims.
    let pad = (f.width - 10 * cw) / 2;
    let mut expected = base.clone();
    for q in &input.rain_add {
        let x0 = pad + q.x as usize;
        let y0 = pad + q.y as usize;
        let cx = (pad + q.cx as usize) as i32;
        let cy = (pad + q.cy as usize) as i32;
        let rx2 = i32::from(q.rx) * i32::from(q.rx);
        let ry2 = i32::from(q.ry) * i32::from(q.ry);
        for y in y0..(y0 + q.h as usize).min(f.height) {
            let dy = y as i32 - cy;
            let ny = dy * dy * 256 / ry2;
            for x in x0..(x0 + q.w as usize).min(f.width) {
                let dx = x as i32 - cx;
                let nsq = dx * dx * 256 / rx2 + ny;
                let weight = (256 - nsq).max(0);
                let weight = (weight * weight / 256).min(255) as u32;
                let premul = (mul8((q.color >> 16) & 0xff, weight) << 16)
                    | (mul8((q.color >> 8) & 0xff, weight) << 8)
                    | mul8(q.color & 0xff, weight);
                let i = y * f.width + x;
                expected[i] = sat_add(expected[i], premul);
            }
        }
    }
    assert_eq!(
        f.pixels, expected,
        "rain_add must be byte-exact radial saturating add over the base frame"
    );
    for (&b, &p) in base.iter().zip(f.pixels.iter()) {
        for sh in [16, 8, 0] {
            assert!(
                (p >> sh) & 0xff >= (b >> sh) & 0xff,
                "additive light must only brighten"
            );
        }
    }
}

/// NO-GHOSTING: animate rain through the persistent damage cache — quads
/// moving between rows (with a halo in tow), then a mutation-tick-style frame
/// where EVERY row's slice changes, then an atlas-version rebake with
/// byte-equal quads. After every step the cached-damaged framebuffer must
/// equal a fresh full render of the same input, byte-for-byte.
#[test]
fn damaged_path_no_ghosting_as_rain_animates() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    // Glyph-free terminal (all background), like the cat/glow damaged-path
    // tests: glyph AA overhang across bands is a pre-existing snapshot quirk
    // this test must not trip on. The rain contract is the row bands it owns.
    let rows = 6usize;
    let mut term = Terminal::new(rows as u16, 12);
    term.process(b"\x1b[?25l");
    let (cw16, ch16) = (cw as u16, ch.min(48) as u16);

    let mut make = |quads: Vec<SpriteQuad>, add: Vec<RainHalo>, atlas: &Arc<SceneAtlas>| {
        let mut input = term.cell_frame(rows, 12);
        input.rain_atlas = Some(atlas.clone());
        input.rain_quads = quads;
        input.rain_add = add;
        input
    };
    let atlas_v1 = Arc::new(patterned_atlas(64, 64, 1));

    // A: two columns of rain on rows 1..=2 + a bright-head halo on row 2.
    let in_a = make(
        vec![
            band_quad(1, 0, cw16, ch16, 0, 0),
            band_quad(1, 3 * cw16, cw16, ch16, 8, 0),
            band_quad(2, 3 * cw16, cw16, ch16, 8, 16),
        ],
        vec![halo(2, 3 * cw16, 2 * ch16, cw16, ch16, 0x0030_6030)],
        &atlas_v1,
    );
    // B: the heads step down a row (rows 2..=3), the halo follows.
    let in_b = make(
        vec![
            band_quad(2, 0, cw16, ch16, 0, 0),
            band_quad(2, 3 * cw16, cw16, ch16, 8, 16),
            band_quad(3, 3 * cw16, cw16, ch16, 8, 32),
        ],
        vec![halo(3, 3 * cw16, 3 * ch16, cw16, ch16, 0x0030_6030)],
        &atlas_v1,
    );
    // C: mutation tick — a quad on EVERY row, all glyph tiles reselected (every
    // row's slice differs from B's).
    let in_c = make(
        (0..rows as u16)
            .map(|r| band_quad(r, cw16, cw16, ch16, 16 + r, 8))
            .collect(),
        vec![halo(5, cw16, 5 * ch16, cw16, ch16, 0x0018_4018)],
        &atlas_v1,
    );
    // D: rebake — byte-equal quads, new atlas version AND new texel content
    // (distinct texels so a missed rebake repaint is visible, not just versioned).
    let atlas_v2 = {
        let mut a = patterned_atlas(64, 64, 2);
        for px in a.rgba.as_chunks_mut::<4>().0 {
            px[0] = px[0].wrapping_add(64);
        }
        Arc::new(a)
    };
    let in_d = make(in_c.rain_quads.clone(), in_c.rain_add.clone(), &atlas_v2);

    let mut wc = WindowCpu::new();
    for (name, input) in [("A", &in_a), ("B", &in_b), ("C", &in_c), ("D", &in_d)] {
        let cached = rend.render_input_cached(&mut wc, input).pixels().to_vec();
        let fresh = rend.render_input(input).pixels.clone();
        assert_eq!(
            cached, fresh,
            "cached-damaged frame {name} must equal a fresh full render \
             (no ghost at vacated rows, no missing stamp at new rows)"
        );
    }
}

/// Dirty gating and the per-row sorted-slice merge-diff:
///   * settled rain (equal quads + halos, same atlas version) gate-hits with
///     zero rows marked and a byte-stable framebuffer (zero repaint work);
///   * moving quads between rows marks ONLY the rows whose per-row slice
///     differs (prev∪cur) — unchanged rows stay clean;
///   * an atlas-version bump alone marks ALL quad rows;
///   * a moved halo sets `rain_add_changed` and marks its prev∪cur rows.
#[test]
fn rain_dirty_gate_and_merge_diff_row_marking() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let atlas_v1 = Arc::new(patterned_atlas(16, 16, 1));
    // Row-sorted, row 2 carrying TWO quads (a slice, not a single element).
    let settled = vec![
        quad(1, [0, 16, 8, 16], [0, 0, 8, 16]),
        quad(2, [0, 32, 8, 16], [0, 0, 8, 16]),
        quad(2, [24, 32, 8, 16], [8, 0, 8, 16]),
        quad(3, [8, 48, 8, 16], [0, 0, 8, 16]),
    ];
    // Halo BELOW the quad rows so its motion subtest's dirty band [4, 5]
    // contains no settled quad (the band fill would legitimately mark one).
    let halos = vec![halo(4, 0, 64, 8, 16, 0x0010_2010)];

    let mut frame = |quads: &[SpriteQuad], add: &[RainHalo], atlas: &Arc<SceneAtlas>| {
        let mut input = term.cell_frame(6, 8);
        input.rain_atlas = Some(atlas.clone());
        input.rain_quads = quads.to_vec();
        input.rain_add = add.to_vec();
        input
    };
    let marked = |dirty: &[bool]| -> Vec<usize> {
        dirty
            .iter()
            .enumerate()
            .filter_map(|(r, &b)| b.then_some(r))
            .collect()
    };

    // Settled: equal non-empty channels + same atlas ⇒ gate hit, nothing marked.
    let prev = frame(&settled, &halos, &atlas_v1);
    let cur = frame(&settled, &halos, &atlas_v1);
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(!d.rain_changed, "settled rain must not set rain_changed");
    assert!(
        !d.rain_add_changed,
        "settled halos must not set rain_add_changed"
    );
    assert!(
        d.is_gate_hit(),
        "settled rain must gate-hit: steady state is free"
    );
    assert!(dirty.iter().all(|&b| !b), "settled rain must mark no rows");

    // Merge-diff: row 2 loses a quad, row 4 gains one; rows 1 and 3 keep
    // byte-equal slices. The merge-diff itself marks only {2, 4}; the
    // unconditional-overlay scissor-band fill then ALSO marks row 3, whose
    // settled quad sits INSIDE the [2, 4] bounding band (the GPU redraws the
    // rain stream ungated inside the scissor, so its row must rebuild or the
    // translucent stamp re-blends over its own cached pixels). Row 1 — a
    // settled slice OUTSIDE the band — must stay CLEAN: that is the merge-diff
    // win over the cat arm's mark-all-prev∪cur-rows.
    let moved = vec![
        settled[0],
        settled[1],
        settled[3],
        quad(4, [8, 64, 8, 16], [8, 0, 8, 16]),
    ];
    let cur = frame(&moved, &halos, &atlas_v1);
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.rain_changed, "moved quads must set rain_changed");
    assert!(!d.is_gate_hit(), "changed rain must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![2, 3, 4],
        "merge-diff marks the differing rows; the band fill adds only the \
         settled in-band row — the out-of-band settled row 1 stays clean"
    );

    // Within-row change: same rows, one quad's tile reselected ⇒ only its row.
    let mut mutated = settled.clone();
    mutated[3].ax = 8;
    let cur = frame(&mutated, &halos, &atlas_v1);
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.rain_changed);
    assert_eq!(
        marked(&dirty),
        vec![3],
        "a within-row slice change must mark exactly that row"
    );

    // Atlas-version bump with byte-equal quads: a rebake repaints ALL quad rows.
    let cur = frame(&settled, &halos, &Arc::new(patterned_atlas(16, 16, 2)));
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.rain_changed,
        "an atlas-version bump alone must set rain_changed (a rebake repaints)"
    );
    assert!(!d.is_gate_hit(), "a rebaked atlas must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 2, 3],
        "the rebake must mark every (unmoved) quad row"
    );

    // Halo motion: quads settled, the bright head's halo steps row 4 → row 5.
    // The [4, 5] band holds no settled quad, so the marks are the pure
    // prev∪cur halo rows.
    let cur = frame(&settled, &[halo(5, 0, 80, 8, 16, 0x0010_2010)], &atlas_v1);
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(!d.rain_changed, "settled quads must not set rain_changed");
    assert!(d.rain_add_changed, "a moved halo must set rain_add_changed");
    assert!(!d.is_gate_hit());
    assert_eq!(
        marked(&dirty),
        vec![4, 5],
        "a moved halo must mark exactly its prev∪cur rows"
    );
}

/// Unchanged rain through the REAL cached path: the second frame is a
/// dirty-gate hit ([`DamageOutcome::GateHit`] — zero repaint work) and the
/// framebuffer is byte-stable.
#[test]
fn unchanged_rain_gate_hits_with_byte_stable_framebuffer() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(4, 12);
    term.process(b"\x1b[?25l");
    let (cw16, ch16) = (cw as u16, ch.min(48) as u16);
    let atlas = Arc::new(patterned_atlas(64, 64, 1));

    let mut make = || {
        let mut input = term.cell_frame(4, 12);
        input.rain_atlas = Some(atlas.clone());
        input.rain_quads = vec![
            band_quad(1, 0, cw16, ch16, 0, 0),
            band_quad(2, 4 * cw16, cw16, ch16, 8, 8),
        ];
        input.rain_add = vec![halo(1, 0, ch16, cw16, ch16, 0x0020_4020)];
        input
    };

    let mut wc = WindowCpu::new();
    let first = rend.render_input_cached(&mut wc, &make()).pixels().to_vec();
    let second = rend.render_input_cached(&mut wc, &make()).pixels().to_vec();
    assert_eq!(
        wc.last_damage(),
        DamageOutcome::GateHit,
        "unchanged rain (no other damage) must dirty-gate: zero repaint work"
    );
    assert_eq!(first, second, "a gate-hit frame must be byte-stable");
}
