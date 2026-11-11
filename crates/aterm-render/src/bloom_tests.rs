// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn frame(w: usize, h: usize, bg: u32) -> Frame {
    Frame {
        width: w,
        height: h,
        pixels: vec![bg; w * h],
    }
}

/// After-land lows W5: a bloom lays no dark gap between the copies of its light at any
/// radius the Bloom size knob reaches. The kernel lays its light as shifted copies, one a
/// tap; at the knob's top, 8, the 25-tap kernel's taps stood 16 px apart, wider than a
/// caret's cell, and the copies of one bright cell stood apart as vertical bands right of
/// the caret (composed r3's access judge: `bloomwide/comet-dark.t0.png` x395-430), where
/// at 6, its taps 12 px apart, they overlap (the judge read it smooth). Read along the
/// light's middle row from its edge out, the bloom never dips under half the light it
/// climbs back to. The control keeps the 25-tap kernel at 8.
#[test]
fn a_wide_bloom_lays_no_gap_between_its_copies() {
    let profile = |radius: f32, forced: i32| -> Vec<i32> {
        FORCE_TAPS.with(|c| c.set(forced));
        let (w, h) = (320usize, 96usize);
        let mut f = frame(w, h, 0);
        let light = [GlowQuad::flat(0, 100, 34, 14, 26, 0x00E0_F0FF, 0)];
        let params = BloomParams {
            strength: BLOOM_STRENGTH,
            radius,
        };
        apply(&mut f, &light, 1, params).unwrap();
        FORCE_TAPS.with(|c| c.set(0));
        let y = 47;
        (114..220)
            .map(|x| {
                let p = f.pixels[y * w + x];
                (((p >> 16) & 0xff).max((p >> 8) & 0xff).max(p & 0xff)) as i32
            })
            .collect()
    };
    // The deepest gap: the most a pixel lies under half the light the row climbs back to.
    let gap = |p: &[i32]| {
        (0..p.len())
            .map(|j| {
                let ahead = p[j..].iter().copied().max().unwrap_or(0);
                if ahead < 4 { 0 } else { ahead / 2 - p[j] }
            })
            .max()
            .unwrap_or(0)
    };
    for radius in [BLOOM_RADIUS, 4.0, 6.0, 6.5, 7.0, 7.5, 8.0] {
        let p = profile(radius, 0);
        assert!(
            p[0] > 8,
            "radius {radius}: the bloom shows past its light ({p:?})"
        );
        assert!(
            gap(&p) <= 0,
            "radius {radius}: the bloom's copies stand apart: {p:?}"
        );
    }
    let control = profile(8.0, 2);
    assert!(
        gap(&control) > 0,
        "control: the 25-tap kernel at radius 8 bands: {control:?}"
    );
    assert_eq!(
        profile(6.0, 0),
        profile(6.0, 2),
        "to a radius of 6 the kernel is the 25-tap one"
    );
}

/// The bloom spreads the glow's light past its own rect, falls off with
/// distance, and leaves the frame untouched beyond its reach; the halo of
/// a red quad is red. NEGATIVE CONTROL: no glow, no change.
#[test]
fn the_bloom_spreads_light_past_the_glow_and_no_further_than_its_reach() {
    let mut f = frame(80, 40, 0);
    let glow = [GlowQuad::flat(1, 30, 16, 8, 8, 0x00E0_2020, 0)];
    apply(&mut f, &glow, 3, BloomParams::default()).unwrap();
    let at = |f: &Frame, x: usize, y: usize| f.pixels[y * 80 + x];
    let red = |p: u32| (p >> 16) & 0xff;
    assert!(red(at(&f, 34, 20)) > red(at(&f, 26, 20)));
    assert!(red(at(&f, 26, 20)) > 0, "light past the quad's own rect");
    assert!(red(at(&f, 26, 20)) > (at(&f, 26, 20) & 0xff), "a red halo");
    let reach = reach_px(BLOOM_RADIUS) as usize;
    assert_eq!(at(&f, 30 - reach - 1, 20), 0, "nothing past the reach");
    let mut bare = frame(80, 40, 0);
    apply(&mut bare, &[], 3, BloomParams::default()).unwrap();
    assert!(bare.pixels.iter().all(|&p| p == 0));
}

/// A pixel passed over for no lit texel within its taps' reach, and a tap passed over for four
/// dark texels or a tap row for two dark rows, take no light: over lights apart, in both modes,
/// at every radius, on both sides of the kernel's widening past 6 ([`taps_per_side`]), the
/// frame is the one each pixel's samples make, two a tap. NEGATIVE CONTROL: judged over a
/// quarter of the reach, a pixel the light reaches is passed over, so the comparison sees a
/// reach too short. NEGATIVE CONTROLS (2026-11-05): a dark tap read from three of its
/// texels, and a dark tap row read from the first of its two rows, each fail it ("a
/// passed-over pixel took light").
#[test]
fn a_quiet_pixel_passed_over_takes_no_light() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move |n: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % u64::from(n)) as u16
    };
    let (w, h) = (160, 96);
    let mut short_differs = false;
    for case in 0..48usize {
        let glow: Vec<GlowQuad> = (0..1 + case % 4)
            .map(|_| {
                let (x, y) = (next(150), next(88));
                GlowQuad {
                    row: y / 16,
                    x,
                    y,
                    w: 1 + next(9),
                    h: 1 + next(7),
                    color: (u32::from(next(0xFFFF)) << 8) | u32::from(next(256)),
                    alpha: [0, 255, 128][usize::from(next(3))],
                    color2: u32::from(next(0xFFFF)) << 8,
                    alpha2: next(256) as u8,
                }
            })
            .collect();
        let params = BloomParams {
            radius: [0.0, 0.6, 1.7, 3.0, 6.0, 6.5, 8.0, 12.0][case % 8],
            strength: [0.4, 1.0, 1.8][case % 3],
        };
        let ground = u32::from(next(0xFFFF)) * 0x101;
        let run = |quiet| {
            let mut f = frame(w, h, ground);
            bloom_kept_off(&mut f, &glow, 6, params, None, quiet).unwrap();
            f.pixels
        };
        let all = run(None);
        assert_eq!(
            run(Some(1.0)),
            all,
            "case {case}: a passed-over pixel took light"
        );
        short_differs |= run(Some(0.25)) != all;
    }
    assert!(
        short_differs,
        "a quarter of the reach passed over no lit pixel"
    );
}

/// The 49-tap kernel past a radius of 6 reaches as far as the 25-tap one, twice the radius, so
/// at the Bloom size knob's top the pass over quiet pixels keeps every pixel its outermost
/// taps light, and the smooth bloom W5's kernel lays is the frame it leaves. NEGATIVE CONTROL:
/// judged over seven eighths of the reach, a pixel only those taps light is passed over.
#[test]
fn the_pass_keeps_what_a_wide_kernels_outer_taps_light() {
    assert_eq!(taps_per_side(8.0), 3);
    let light = [GlowQuad::flat(0, 140, 60, 14, 26, 0x00E0_F0FF, 0)];
    let params = BloomParams {
        strength: BLOOM_STRENGTH,
        radius: 8.0,
    };
    let run = |quiet| {
        let mut f = frame(320, 160, 0);
        bloom_kept_off(&mut f, &light, 1, params, None, quiet).unwrap();
        f.pixels
    };
    let all = run(None);
    assert_eq!(run(Some(1.0)), all, "a passed-over pixel took light");
    assert_ne!(
        run(Some(0.875)),
        all,
        "seven eighths of the reach passed over no lit pixel"
    );
}

/// A row of 10 cells 8x16 at the frame's origin, `x` standing in cells `3..5`.
fn lettered_row() -> (aterm_core::render::RenderInput, LetterGrid) {
    let mut input = aterm_core::render::RenderInput::empty();
    input.rows = 1;
    input.cols = 10;
    let mut row = vec![crate::RenderCell::default(); 10];
    for cell in &mut row[3..5] {
        cell.ch = 'x';
    }
    input.cells = vec![row];
    let grid = LetterGrid {
        pad: 0,
        grid_top: 0,
        cell_w: 8,
        cell_h: 16,
    };
    (input, grid)
}

/// looks10 L1: the post passes' source keeps `shield` px off every lettered cell, and
/// keeps every other pixel of the light once, each at the colour and opacity the whole
/// light had there. NEGATIVE CONTROL: a light clear of the letters by more than the
/// shield is kept whole.
#[test]
fn spared_lights_keep_off_the_letters_and_keep_the_rest_whole() {
    let (input, grid) = lettered_row();
    let light = GlowQuad {
        color2: 0x0000_00FF,
        alpha2: 200,
        ..GlowQuad::flat(0, 0, 4, 80, 8, 0x00FF_0000, 40)
    };
    let shield = 3;
    let mut kept = vec![light];
    spare_letters(&mut kept, &input, grid, shield);
    let shielded = |x: usize, y: usize| {
        (24 - shield as usize..40 + shield as usize).contains(&x) && y < 16 + shield as usize
    };
    let mut covered = vec![0u8; 80 * 16];
    for q in &kept {
        for y in usize::from(q.y)..usize::from(q.y + q.h) {
            for x in usize::from(q.x)..usize::from(q.x + q.w) {
                assert!(!shielded(x, y), "a kept light at ({x}, {y}) by a letter");
                covered[y * 80 + x] += 1;
                let at = |c: u32, c2: u32, q: &GlowQuad, x: usize| {
                    crate::glow_lerp_rgb(c, c2, (x - usize::from(q.x)) as u32, u32::from(q.w))
                };
                let (whole, piece) = (
                    at(light.color, light.color2, &light, x),
                    at(q.color, q.color2, q, x),
                );
                for sh in [16, 8, 0] {
                    let d = ((whole >> sh) & 0xff).abs_diff((piece >> sh) & 0xff);
                    assert!(
                        d <= 1,
                        "a piece's colour at x {x}: {piece:06x} for {whole:06x}"
                    );
                }
            }
        }
    }
    for y in 4..12 {
        for x in 0..80 {
            let want = u8::from(!shielded(x, y));
            assert_eq!(
                covered[y * 80 + x],
                want,
                "({x}, {y}) kept {} times",
                covered[y * 80 + x]
            );
        }
    }
    let clear = GlowQuad::flat(0, 60, 4, 16, 8, 0x00FF_0000, 40);
    let mut whole = vec![clear];
    spare_letters(&mut whole, &input, grid, shield);
    assert_eq!(
        whole,
        vec![clear],
        "a light clear of the letters is kept whole"
    );
}

/// looks10 L1, composed r2 B1: the bloom of a light beside a letter, its source spared and
/// its composite kept off the letter ([`spared_sources`], [`apply_kept_off`]), lays nothing
/// on the letter's cell nor its ring; bloomed from the light whole (the control), it
/// lights the cell.
#[test]
fn a_spared_bloom_lays_no_light_on_a_letter() {
    let (input, grid) = lettered_row();
    let light = [GlowQuad::flat(0, 44, 0, 32, 16, 0x00E0_E0E0, 0)];
    let (mut crown, mut blur) = (Vec::new(), Vec::new());
    let mask = spared_sources(&light, &input, grid, BLOOM_RADIUS, &mut crown, &mut blur);
    let letter_lit = |glow: &[GlowQuad], mask: Option<&LetterMask>| {
        let mut f = frame(80, 16, 0);
        apply_kept_off(&mut f, glow, 1, BloomParams::default(), mask).unwrap();
        (0..16).any(|y| (22..42).any(|x| f.pixels[y * 80 + x] != 0))
    };
    assert!(
        letter_lit(&light, None),
        "control: the whole light blooms onto the letter"
    );
    assert!(
        !blur.is_empty() && mask.is_some(),
        "fixture: the light reaches past the bloom's shield"
    );
    assert!(
        !letter_lit(&blur, mask.as_ref()),
        "the spared bloom lights the letter"
    );
    assert_eq!(
        crown,
        light.to_vec(),
        "the crown keeps a light past its shield whole"
    );
}

/// Composed r2 B1: a frame whose bloom spares its letters lights more the larger its
/// radius, and lays nothing on a lettered cell or its ring at any radius: the bloom keeps
/// off the letters at its composite ([`LetterMask`]), so its source is cut by the ring
/// alone. NEGATIVE CONTROL ([`with_source_cut_by_reach`]): the source cut a whole blur's
/// reach off the letters, as looks10 kept it, lights less at the larger radii (the Bloom
/// size ran backwards) and nothing at all once the cut covers the light.
#[test]
fn a_spared_bloom_grows_with_its_radius_and_keeps_off_the_letters() {
    // Three rows of 30 cells 8x16, `x` in columns 3..9 of rows 0 and 2; a light along
    // the blank middle row and over the letters.
    let mut input = aterm_core::render::RenderInput::empty();
    input.rows = 3;
    input.cols = 30;
    let mut lettered = vec![crate::RenderCell::default(); 30];
    for cell in &mut lettered[3..9] {
        cell.ch = 'x';
    }
    input.cells = vec![
        lettered.clone(),
        vec![crate::RenderCell::default(); 30],
        lettered,
    ];
    let grid = LetterGrid {
        pad: 0,
        grid_top: 0,
        cell_w: 8,
        cell_h: 16,
    };
    let light = [GlowQuad::flat(1, 16, 20, 88, 8, 0x00D0_E0F0, 0)];
    let (w, h) = (240, 48);
    let ring = |x: usize, y: usize| {
        let (x, y) = (x as i64, y as i64);
        (22..74).contains(&x) && ((-2..18).contains(&y) || (30..50).contains(&y))
    };
    let bloomed = |radius: f32| -> (usize, bool) {
        let (mut crown, mut blur) = (Vec::new(), Vec::new());
        let mask = spared_sources(&light, &input, grid, radius, &mut crown, &mut blur);
        let mut f = frame(w, h, 0);
        let params = BloomParams {
            radius,
            ..BloomParams::default()
        };
        apply_kept_off(&mut f, &blur, 3, params, mask.as_ref()).unwrap();
        let lit = f.pixels.iter().filter(|&&p| p != 0).count();
        let on_letter = (0..h).any(|y| (0..w).any(|x| ring(x, y) && f.pixels[y * w + x] != 0));
        (lit, on_letter)
    };
    let radii = [0.5f32, BLOOM_RADIUS, 8.0];
    let lit: Vec<(usize, bool)> = radii.iter().map(|&r| bloomed(r)).collect();
    for (r, (_, on)) in radii.iter().zip(&lit) {
        assert!(!on, "radius {r}: the bloom lit a letter's cell or its ring");
    }
    assert!(
        lit[0].0 < lit[1].0 && lit[1].0 < lit[2].0,
        "the lit area grows with the radius: {lit:?}"
    );
    let cut: Vec<(usize, bool)> =
        with_source_cut_by_reach(|| radii.iter().map(|&r| bloomed(r)).collect());
    assert!(
        !(cut[0].0 < cut[1].0 && cut[1].0 < cut[2].0),
        "control: the source cut by the blur's reach lights less as the radius grows: {cut:?}"
    );
}

/// Composed r2 O2(a): the post passes keep off a bar or underline caret's cell as off a
/// letter's, past the end of its row's ink too, so the bar reads against the light the
/// frame's streams lay in its cell ([`thin_caret_cell`]). NEGATIVE CONTROL: a block caret
/// paints its whole cell, and the bloom and the crown are laid over it.
#[test]
fn the_post_passes_keep_off_a_thin_caret_s_cell() {
    use aterm_core::terminal::CursorStyle;
    let mut input = aterm_core::render::RenderInput::empty();
    input.rows = 1;
    input.cols = 10;
    input.cells = vec![vec![crate::RenderCell::default(); 4]];
    input.cursor_visible = true;
    input.cursor_col = 6;
    let grid = LetterGrid {
        pad: 0,
        grid_top: 0,
        cell_w: 8,
        cell_h: 16,
    };
    let light = [GlowQuad::flat(0, 24, 4, 56, 8, 0x00E0_E0E0, 0)];
    let cell_lit = |style: CursorStyle| {
        let mut input = input.clone();
        input.cursor_style = style;
        let (mut crown, mut blur) = (Vec::new(), Vec::new());
        let mask = spared_sources(&light, &input, grid, BLOOM_RADIUS, &mut crown, &mut blur);
        let mut f = frame(80, 16, 0);
        apply_kept_off(&mut f, &blur, 1, BloomParams::default(), mask.as_ref()).unwrap();
        sdr_crown(&mut f, &crown, 1, 0.4).unwrap();
        (0..16).any(|y| (46..58).any(|x| f.pixels[y * 80 + x] != 0))
    };
    for style in [CursorStyle::SteadyBar, CursorStyle::BlinkingUnderline] {
        assert!(
            !cell_lit(style),
            "{style:?}: the post passes lit the caret's cell"
        );
    }
    assert!(
        cell_lit(CursorStyle::SteadyBlock),
        "control: a block caret's cell takes the bloom and the crown"
    );
}

/// The SDR crown screens each quad once at its colour times the budget.
#[test]
fn the_sdr_crown_screens_the_glow_at_its_budget() {
    let mut f = frame(20, 20, 0x0010_1010);
    sdr_crown(
        &mut f,
        &[GlowQuad::flat(0, 2, 2, 4, 4, 0x00FF_0000, 0)],
        2,
        0.4,
    )
    .unwrap();
    let p = f.pixels[3 * 20 + 3];
    assert_eq!((p >> 16) & 0xff, 112);
    assert_eq!(p & 0xff, 0x10, "a channel the quad lacks keeps the ground");
    assert_eq!(f.pixels[0], 0x0010_1010, "outside the quad nothing changes");
}

#[test]
fn rejected_inputs_leave_pixels_intact_and_valid_retries_still_draw() {
    let glow = [GlowQuad::flat(0, 2, 2, 4, 4, 0x00FF_0000, 0)];
    for (width, height, len) in [
        (usize::MAX, 1, 1),
        (0, usize::MAX, 0),
        (8193, 8192, 1),
        (2, 2, 3),
        (1, 1, 2),
    ] {
        let mut f = Frame {
            width,
            height,
            pixels: vec![0x0010_1010; len],
        };
        let before = f.pixels.clone();
        assert!(apply(&mut f, &glow, 1, BloomParams::default()).is_err());
        assert_eq!(f.pixels, before);
        assert!(sdr_crown(&mut f, &glow, 1, 0.4).is_err());
        assert_eq!(f.pixels, before);
    }
    let mut f = frame(20, 20, 0x0010_1010);
    let before = f.pixels.clone();
    for (strength, radius) in [
        (f32::NAN, BLOOM_RADIUS),
        (f32::INFINITY, BLOOM_RADIUS),
        (BLOOM_STRENGTH, f32::NAN),
        (BLOOM_STRENGTH, f32::INFINITY),
        (BLOOM_STRENGTH, f32::NEG_INFINITY),
        (BLOOM_STRENGTH, -1.0),
    ] {
        assert!(apply(&mut f, &glow, 1, BloomParams { strength, radius }).is_err());
        assert_eq!(f.pixels, before);
    }
    assert!(sdr_crown(&mut f, &glow, 1, f32::NAN).is_err());
    assert_eq!(f.pixels, before);
    apply(&mut f, &glow, 1, BloomParams::default()).unwrap();
    assert_ne!(f.pixels, before);
    let mut empty = frame(0, 0, 0);
    apply(&mut empty, &glow, 1, BloomParams::default()).unwrap();
    sdr_crown(&mut empty, &glow, 1, 0.4).unwrap();
}

#[test]
fn enormous_finite_radii_sample_clamped_edges_without_overflow() {
    assert_eq!(reach_px(BLOOM_RADIUS), 12);
    assert_eq!(reach_px(f32::MAX), u32::MAX);
    assert_eq!(reach_px(f32::INFINITY), u32::MAX);
    let glow = [GlowQuad::flat(0, 2, 2, 4, 4, 0x00FF_0000, 0)];
    let mut large = frame(20, 20, 0x0010_1010);
    let mut saturated = large.clone();
    for (frame, radius) in [(&mut large, 1000.0), (&mut saturated, f32::MAX)] {
        apply(
            frame,
            &glow,
            1,
            BloomParams {
                radius,
                ..BloomParams::default()
            },
        )
        .unwrap();
    }
    assert_eq!(large.pixels, saturated.pixels);
    assert!(large.pixels.iter().any(|&p| p != 0x0010_1010));
}

/// A stream for the item list's law: quads before, between and after
/// its ramps (a tall one, a one-pixel-wide one, an over light, an empty
/// one, one past the frame's rows), ramps with and without a run, dark
/// rows inside one and at both its ends, a dark ramp, a one-pixel-wide
/// ramp and a ramp past the frame's rows.
fn stream() -> (Vec<GlowQuad>, GlowRamps, Vec<GlowQuad>) {
    let quad = |row, x, y, w, h, c: u32, a| GlowQuad {
        color2: c.rotate_left(8) & 0x00FF_FFFF,
        alpha2: a,
        ..GlowQuad::flat(row, x, y, w, h, c, a)
    };
    let mut glow = vec![
        quad(0, 3, 2, 6, 3, 0x0030_2010, 0),
        quad(1, 9, 20, 1, 1, 0x00FF_8040, 0),
        quad(1, 40, 21, 0, 1, 0x0011_1111, 0),
    ];
    let mut ramps = GlowRamps::default();
    let mut lay = |glow: &Vec<GlowQuad>, row, x, y, w, rows: &[(u32, u32)], run: (u16, u16)| {
        let ink = ramps.ends.len() as u32;
        for &(a, b) in rows {
            ramps.ends.extend([a, b]);
        }
        ramps.items.push(GlowRamp {
            at: glow.len() as u32,
            row,
            x,
            y,
            w,
            h: rows.len() as u16 + run.1,
            join: run.0,
            joined: run.1,
            ink,
        });
    };
    let lit = |k: u32| (0x0010_2030 + k * 0x0003_0507, 0x0050_4030 - k * 0x0001_0203);
    let rows: Vec<_> = (0..7).map(lit).collect();
    lay(&glow, 1, 12, 17, 9, &rows, (0, 0));
    glow.push(quad(1, 30, 18, 4, 2, 0x8020_4060, 0x80));
    lay(&glow, 1, 14, 18, 7, &rows[..5], (2, 3));
    let mut dark = rows.clone();
    for k in [0, 3, 6] {
        dark[k] = (0, 0);
    }
    lay(&glow, 2, 5, 33, 11, &dark, (0, 0));
    lay(&glow, 2, 7, 34, 4, &[(0, 0), (0, 0)], (0, 0));
    lay(&glow, 2, 21, 35, 1, &rows[..4], (1, 2));
    glow.push(quad(2, 2, 40, 5, 0, 0x0001_0101, 0));
    lay(&glow, 9, 1, 150, 6, &rows, (0, 0));
    glow.push(quad(9, 1, 160, 5, 1, 0x0001_0101, 0));
    glow.push(quad(2, 50, 44, 2, 1, 0x00F0_F0F0, 0));
    ramps.lit = ramps.count_lit();
    let extra = vec![
        quad(1, 60, 19, 3, 1, 0x0000_FF00, 0),
        quad(2, 61, 36, 8, 4, 0x00FF_0000, 0),
    ];
    (glow, ramps, extra)
}

/// The item list draws the extract set and the crown's lights: instance
/// by instance its lit rows are the stream's lights, the crown's in
/// their order and the extract's widened as its copies are, with the
/// extra lights after them, and both bound alike.
#[test]
fn the_item_list_draws_its_streams_extract_set() {
    let (glow, ramps, extra) = stream();
    let rows = 4;
    let drawn = |set: &[GlowQuad]| -> Vec<GlowQuad> {
        set.iter()
            .filter(|q| q.w > 0 && q.h > 0)
            .map(|q| GlowQuad {
                row: 0,
                color: q.color & 0x00FF_FFFF,
                color2: q.color2 & 0x00FF_FFFF,
                ..*q
            })
            .collect()
    };
    let union = |set: &[GlowQuad]| -> Option<[u32; 4]> {
        drawn(set)
            .iter()
            .map(|q| {
                let (x, y) = (u32::from(q.x), u32::from(q.y));
                [x, y, x + u32::from(q.w), y + u32::from(q.h)]
            })
            .reduce(|a, b| {
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ]
            })
    };
    let mut items = BloomItems::default();
    for extra in [&[][..], &extra[..]] {
        let mut all = Vec::new();
        ramps.expand_into(&glow, &mut all);
        all.extend_from_slice(extra);
        let mut set = Vec::new();
        let (bbox, first) = source_quads_into(&all, rows, &mut set);
        let n = if first > 0 { first } else { set.len() };
        if extra.is_empty() {
            let mut walked = Vec::new();
            assert_eq!(
                source_stream_into(&glow, &ramps, rows, &mut walked),
                (bbox, first)
            );
            assert_eq!(walked, set);
        }
        assert!(first > 0, "the stream has thin lights to widen");
        let got = items_into(&glow, &ramps, extra, rows, &mut items);
        assert_eq!(
            got, bbox,
            "the list bounds its lights as the extract set does"
        );
        let read = |widen| -> Vec<GlowQuad> {
            (0..items.instances() + 3)
                .filter_map(|i| items.light(i, widen))
                .collect()
        };
        assert_eq!(read(false), drawn(&set[..n]), "the crown's lights");
        assert_eq!(read(true), drawn(&set[first..]), "the extract's copies");
        assert_eq!(items.footprint(), union(&set[..n]), "the crown's footprint");
        assert!(items.len() < drawn(&set[..n]).len(), "ramps ride whole");
    }
    // Each ramp alone: its first and last lit rows bound it.
    for ramp in &ramps.items {
        let alone = GlowRamps {
            items: vec![GlowRamp { at: 0, ..*ramp }],
            ends: ramps.ends.clone(),
            lit: 0,
        };
        let mut set = Vec::new();
        let (bbox, _) = source_stream_into(&[], &alone, rows, &mut set);
        assert_eq!(
            items_into(&[], &alone, &[], rows, &mut items),
            bbox,
            "{ramp:?}"
        );
        let mut lit = Vec::new();
        alone.expand_into(&[], &mut lit);
        lit.retain(|q| usize::from(q.row) < rows);
        assert_eq!(items.footprint(), union(&lit), "{ramp:?}");
    }
    let skipped = ramps
        .items
        .iter()
        .filter(|r| usize::from(r.row) >= rows)
        .count();
    assert!(skipped > 0 && ramps.items.iter().any(|r| r.joined > 0));
    // A stream with nothing to bloom lists nothing and bounds nothing.
    assert_eq!(
        items_into(&[], &GlowRamps::default(), &[], rows, &mut items),
        None
    );
    assert!(items.is_empty() && items.instances() == 0 && items.light(0, true).is_none());
    assert_eq!(items.footprint(), None);
}
