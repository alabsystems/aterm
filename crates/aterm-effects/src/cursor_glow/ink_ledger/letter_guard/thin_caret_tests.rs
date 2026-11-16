// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn lum(c: u32) -> f32 {
    crate::color_math::relative_luminance_with(crate::color_math::relative_luminance_table(), c)
}

/// A caret's colour along its path keeps its hue while it lightens (dark) or darkens
/// (paper), each step at least as far from the page as the last, and ends at white or
/// black; a fill that reads already is left as it is.
#[test]
fn a_thin_caret_moves_in_lightness_alone() {
    for (fill, page) in [
        (0x0050_FA7B, 0x0011_1318),
        (0x0009_69DA, 0x00FF_FFFF),
        (0x00E8_6D18, 0x0011_1318),
    ] {
        let dark = aterm_render::theme_is_dark(page);
        let (h0, _, _) = crate::color_math::rgb2hsv(fill);
        let mut last = lum(fill);
        for k in 1..=40 {
            let c = lifted(fill, dark, 2.0 * k as f32 / 40.0);
            let l = lum(c);
            assert!(
                if dark {
                    l >= last - 1e-4
                } else {
                    l <= last + 1e-4
                },
                "{fill:06x} step {k}: {c:06x}"
            );
            last = l;
            let (h, s, _) = crate::color_math::rgb2hsv(c);
            if s > 0.15 {
                let d = (h - h0).abs();
                assert!(
                    d.min(360.0 - d) <= 6.0,
                    "{fill:06x} step {k}: hue {h:.0} of {h0:.0} ({c:06x})"
                );
            }
        }
        assert_eq!(lifted(fill, dark, 2.0), if dark { 0x00FF_FFFF } else { 0 });
    }
    assert_eq!(
        lifted_to(0x0050_FA7B, 0x0011_1318, 0x0011_1318, 2.0),
        Some(0x0050_FA7B)
    );
    // The theme's bar on a pink ribbon's paper tint is darkened to read it.
    let c = lifted_to(0x0009_69DA, 0x00FF_FFFF, 0x00EE_BADD, 2.0).expect("darkened, it reads");
    assert!(
        crate::color_math::contrast_ratio(c, 0x00EE_BADD) >= THIN_CARET_MIN,
        "{c:06x}"
    );
    // White on Laser's lavender head reads under the bar: no lift does it.
    assert_eq!(lifted_to(0x0050_FA7B, 0x0011_1318, 0x00CC_B3FF, 2.0), None);
}

/// The flash law's turns of a luminance series, as `trail-film`'s `Turns` counts them: a change
/// of 0.10 or more from the last extreme, the darker state below 0.80.
fn turns(lums: &[f32]) -> usize {
    let (swing, dark) = (0.10f32, 0.80f32);
    let (mut lo, mut hi, mut dir) = (lums[0], lums[0], 0i8);
    let mut n = 0;
    for &v in &lums[1..] {
        let turned = match dir {
            0 => {
                (lo, hi) = (lo.min(v), hi.max(v));
                if hi - lo >= swing && lo < dark {
                    if v > lo { 1 } else { -1 }
                } else {
                    0
                }
            }
            1 if v > hi => {
                hi = v;
                0
            }
            1 if hi - v >= swing && v < dark => -1,
            -1 if v < lo => {
                lo = v;
                0
            }
            -1 if v - lo >= swing && lo < dark => 1,
            _ => 0,
        };
        if turned != 0 {
            (dir, lo, hi) = (turned, v, v);
            n += 1;
        }
    }
    n
}

/// A hood of one line pixel whose ring is one pixel lit `ring` ([`ThinHood`]), on `page`.
fn one_pixel_hood(ring: u32, page: u32) -> ThinHood {
    let mut hood = ThinHood {
        px: vec![HoodPx {
            lit: ring,
            base: page,
            ground: page,
            ..HoodPx::default()
        }],
        line: vec![(0, 0, 0, 1)],
        ring: vec![0],
        cell: page,
        bar: true,
        ..ThinHood::default()
    };
    hood.read_rings(&mut [Vec::new(), Vec::new(), Vec::new()]);
    hood
}

/// A THIN CARET'S LIFT IS GIVEN BACK SLOWLY ([`CursorGlow::thin_caret_fill`],
/// [`RELEASE_PER_S`]): a ring that flips every frame of a 30 Hz trail between Dracula's page,
/// where the green bar reads, and a grey only white reads on, turns the bar no more than 4 times
/// a second: it is lifted at once and eases back no faster than the release, so it reads on
/// every frame and never flashes; once the ring stays dark, it is back to its own green within
/// two seconds. NEGATIVE CONTROL ([`CursorGlow::set_thin_caret_release`] off): each frame's
/// least lift turns the bar on every frame, 30 times a second.
#[test]
fn a_thin_caret_s_lift_is_given_back_slowly() {
    let (page, fill, grey) = (0x0028_2A36, 0x0050_FA7B, 0x0064_6464);
    assert!(lifted_to(fill, page, page, 2.0) == Some(fill));
    assert!(lifted_to(fill, page, grey, 2.0).is_some_and(|c| c != fill));
    let caret = ThinCaret {
        cell: (3, 4),
        bar: true,
        page,
        pin: None,
    };
    let run = |release: bool| {
        let mut glow = CursorGlow::default();
        glow.set_thin_caret_release(release);
        let t0 = Instant::now();
        let mut lums = Vec::new();
        for k in 0..60u32 {
            let ring = if k % 2 == 0 { grey } else { page };
            glow.thin_hood = one_pixel_hood(ring, page);
            glow.thin_caret_read = Some((caret, page));
            let at = t0 + std::time::Duration::from_micros(33_333 * u64::from(k));
            let drawn = glow.thin_caret_fill(fill, at).unwrap_or(fill);
            assert!(
                glow.thin_hood.reads_all(drawn, page, true),
                "release {release} frame {k}: {drawn:06x} does not read on {ring:06x}"
            );
            lums.push(lum(drawn));
        }
        // The ring stays the page: the lift is given back.
        let mut last = 0;
        for k in 60..130u32 {
            glow.thin_hood = one_pixel_hood(page, page);
            glow.thin_caret_read = Some((caret, page));
            let at = t0 + std::time::Duration::from_micros(33_333 * u64::from(k));
            last = glow.thin_caret_fill(fill, at).unwrap_or(fill);
        }
        (turns(&lums), last, glow.thin_caret_releasing())
    };
    let (n, last, releasing) = run(true);
    assert!(n <= 8, "{n} turns in two seconds");
    assert_eq!(last, fill, "the lift is given back");
    assert!(!releasing);
    let (n, ..) = run(false);
    assert!(n >= 50, "control: {n} turns in two seconds");
}

/// A thin caret's line reads row by row against its ring, as the reader reads it: the lower
/// median of each pixel row's readings ([`ThinHood::reads_all`]). A bar two pixels wide whose
/// left pixel's ring is a bright band reads under the bar on that row; one whose rows read
/// apart from a single pixel of an underline's eight still reads.
#[test]
fn a_thin_caret_reads_its_ring_row_by_row() {
    let page = 0x0028_2A36;
    let band = 0x00C0_C0C0;
    let bar_hood = |left: u32| {
        let mut hood = ThinHood {
            px: vec![
                HoodPx {
                    lit: left,
                    base: page,
                    ground: page,
                    ..HoodPx::default()
                },
                HoodPx {
                    lit: page,
                    base: page,
                    ground: page,
                    ..HoodPx::default()
                },
            ],
            line: vec![(0, 0, 0, 1), (0, 0, 1, 2)],
            ring: vec![0, 1],
            cell: page,
            bar: true,
            ..ThinHood::default()
        };
        hood.read_rings(&mut [Vec::new(), Vec::new(), Vec::new()]);
        hood
    };
    assert!(bar_hood(page).reads_all(0x0050_FA7B, page, true));
    assert!(!bar_hood(band).reads_all(0x0050_FA7B, page, true));
    assert!(
        bar_hood(band).reads_all(0x0050_FA7B, page, false),
        "control: no ring"
    );
}

/// A THIN CARET'S CELL IS READ WITH ITS LETTER (CT-51 Rule A, M4;
/// [`CursorGlow::thin_caret_hood`]): a bar on a wide `l` under a light laid over its cell reads
/// the cell's ground as the reader does, the median of every pixel off the line with the
/// letter's laid as the renderer blends text, here the letter's colour over the light. NEGATIVE
/// CONTROL ([`CursorGlow::set_thin_caret_text`] off): read with the letter left out, as composed
/// r2's was, the ground is the light alone.
#[test]
fn a_thin_caret_s_cell_is_read_with_its_letter() {
    use aterm_render::glyph_ink::GlyphCoverage;
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 20,
        origin_x: 0,
        origin_y: 0,
        win_w: 160,
        win_h: 96,
        head: 0,
    };
    let (page, fg, light) = (0x0028_2A36, 0x00F8_F8F2, 0x0060_3060);
    let cell = aterm_core::terminal::RenderCell {
        fg: [0xF8, 0xF8, 0xF2],
        bg: [0x28, 0x2A, 0x36],
        ..aterm_core::terminal::RenderCell::default()
    };
    let mut cells = vec![vec![cell; g.cols]; g.rows];
    cells[2][4].ch = 'l';
    // A wide `l`: columns 3 to 7 of its cell, every row.
    let at = usize::from(aterm_render::GlyphInk::index('l', false));
    let mut regular: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    let bold: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    regular[at].max = 255;
    regular[at].cell_box = Some([3, 0, 8, 16]);
    let mut rasters = vec![None; 190];
    rasters[at] = Some(aterm_render::GlyphRaster {
        x: 3,
        y: 0,
        w: 5,
        cov: vec![255; 5 * 16].into_boxed_slice(),
    });
    let face = std::sync::Arc::new(
        aterm_render::GlyphInk::from_coverage(regular, bold, false).with_rasters(rasters),
    );
    let caret = ThinCaret {
        cell: (2, 4),
        bar: true,
        page,
        pin: None,
    };
    let read = |text: bool| {
        let mut glow = CursorGlow::default();
        glow.set_thin_caret_text(text);
        glow.observe_frame_text(Some((2, 4)), &cells, Instant::now());
        glow.observe_glyph_ink(Some(face.clone()));
        glow.under_out = vec![GlowQuad {
            row: 2,
            x: 24,
            y: 24,
            w: 32,
            h: 32,
            color: light,
            alpha: 0xFF,
            color2: light,
            alpha2: 0xFF,
        }];
        let mut over = Vec::new();
        let _ = glow.hold_light_for_thin_caret(
            &mut over,
            Some((caret, page)),
            (g, GlowStyle::RainbowKitty, Instant::now()),
            (&[], 0),
        );
        glow.thin_caret_read.map(|(_, ground)| ground)
    };
    let lit = crate::cursor_fx::light_at(
        page,
        (
            &[GlowQuad {
                row: 2,
                x: 24,
                y: 24,
                w: 32,
                h: 32,
                color: light,
                alpha: 0xFF,
                color2: light,
                alpha2: 0xFF,
            }],
            &[],
        ),
        (36, 40),
    );
    let lettered =
        aterm_render::TextLevel::new(fg, page, 255, face.corrected()).over(lit) & 0x00FF_FFFF;
    assert_ne!(lit, lettered);
    assert_eq!(read(true), Some(lettered), "the cell reads as its letter's");
    assert_eq!(read(false), Some(lit), "control: the light alone");
}

/// A geometry of 8 by 16 px cells, 20 by 6, from the window's corner.
fn hold_geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 20,
        origin_x: 0,
        origin_y: 0,
        win_w: 160,
        win_h: 96,
        head: 0,
    }
}

/// A light over the window px box `(x, y, w, h)`.
fn light_box(x: u16, y: u16, w: u16, h: u16, color: u32) -> GlowQuad {
    GlowQuad {
        row: 2,
        x,
        y,
        w,
        h,
        color,
        alpha: 0xFF,
        color2: color,
        alpha2: 0xFF,
    }
}

/// A THIN CARET'S HOLD IS GIVEN BACK SLOWLY ([`ThinHeld`], [`RELEASE_PER_S`]): a light that
/// stays on the pixel over a bar's top, where white reads under the bar, is held only on the
/// frames the ring's median fails, which a light flickering beside the bar at 30 Hz decides; held
/// at once and given back no faster than the release, that pixel turns no more than 4 times a
/// second, and every frame reads; once the flicker stops, the pixel has its whole light back
/// within four seconds and the hold stops asking for frames. NEGATIVE CONTROL
/// ([`CursorGlow::set_thin_caret_release`] off): held and let go on alternate frames, the pixel
/// turns on every frame (dark backspace's seam over the bar, 8 turns a second).
#[test]
fn a_thin_caret_s_hold_is_given_back_slowly() {
    let g = hold_geom();
    let page = 0x0028_2A36;
    let caret = ThinCaret {
        cell: (2, 4),
        bar: true,
        page,
        pin: None,
    };
    // The bar's line is x 32..34, y 32..48: the pixel over its top is (32, 31).
    let (top, flicker) = (
        light_box(32, 31, 1, 1, 0x00C0_C0C0),
        light_box(31, 31, 1, 3, 0x00C0_C0C0),
    );
    let run = |release: bool| {
        let mut glow = CursorGlow::default();
        glow.set_thin_caret_release(release);
        let t0 = Instant::now();
        let at = |k: u32| t0 + std::time::Duration::from_micros(33_333 * u64::from(k));
        let frame = |glow: &mut CursorGlow, k: u32, lights: Vec<GlowQuad>| {
            glow.under_out = lights;
            let mut over = Vec::new();
            let _ = glow.hold_light_for_thin_caret(
                &mut over,
                Some((caret, page)),
                (g, GlowStyle::RainbowKitty, at(k)),
                (&[], 0),
            );
            let hood = &glow.thin_hood;
            let (bx0, by0, bw, _) = hood.at;
            let p = hood.px[((31 - by0) * bw + (32 - bx0)) as usize];
            (lum(p.lit), hood.reads_all(0x00FF_FFFF, page, true))
        };
        let mut lums = Vec::new();
        for k in 0..60u32 {
            let lights = if k % 2 == 0 {
                vec![top, flicker]
            } else {
                vec![top]
            };
            let (l, reads) = frame(&mut glow, k, lights);
            assert!(reads, "release {release} frame {k}: white does not read");
            lums.push(l);
        }
        let mut last = 0.0;
        for k in 60..190u32 {
            last = frame(&mut glow, k, vec![top]).0;
        }
        (turns(&lums), last, glow.thin_caret_releasing())
    };
    let whole = crate::cursor_fx::light_at(page, (&[top], &[]), (32, 31));
    let (n, last, releasing) = run(true);
    assert!(n <= 8, "{n} turns in two seconds");
    assert!(
        (last - lum(whole)).abs() < 1.0e-3,
        "the hold is given back: {last} of {}",
        lum(whole)
    );
    assert!(!releasing, "a hold given back asks for no more frames");
    let (n, ..) = run(false);
    assert!(n >= 50, "control: {n} turns in two seconds");
}

/// AN UNDERLINE IS READ ACROSS ITS LENGTH (CT-51 B.1; [`CursorGlow::thin_caret_hood`]): a grey
/// light hugging the right three of an underline's eight columns, above it and below, which
/// Dracula's green reads under the bar against and white reads over, lifts the underline, for each
/// of those columns is a slice of the line a reader sees break, as the judge reads it. NEGATIVE
/// CONTROL ([`CursorGlow::set_thin_caret_across`] off): read by pixel rows, each row of the line
/// keeps its lower median, the page's, and the green is left as it is.
#[test]
fn an_underline_is_read_across_its_length() {
    let g = hold_geom();
    let (page, fill) = (0x0028_2A36, 0x0050_FA7B);
    let caret = ThinCaret {
        cell: (2, 4),
        bar: false,
        page,
        pin: None,
    };
    let grey = 0x0060_6060;
    assert!(crate::color_math::contrast_ratio(fill, grey) < THIN_CARET_MIN);
    assert!(crate::color_math::contrast_ratio(0x00FF_FFFF, grey) >= THIN_CARET_MIN);
    let drawn = |across: bool| {
        let mut glow = CursorGlow::default();
        glow.set_thin_caret_across(across);
        let now = Instant::now();
        // The underline is x 32..40, y 46..48: the light over x 37..42 on the rows above and
        // below it.
        glow.under_out = vec![light_box(37, 45, 5, 1, grey), light_box(37, 48, 5, 1, grey)];
        let mut over = Vec::new();
        let _ = glow.hold_light_for_thin_caret(
            &mut over,
            Some((caret, page)),
            (g, GlowStyle::RainbowKitty, now),
            (&[], 0),
        );
        glow.thin_caret_fill(fill, now)
    };
    let lifted = drawn(true);
    assert!(
        lifted.is_some_and(|c| crate::color_math::contrast_ratio(c, grey) >= THIN_CARET_MIN),
        "the columns under the light lift the underline: {lifted:x?}"
    );
    assert_eq!(drawn(false), None, "control: by pixel rows it reads");
}

/// A THIN CARET'S RING READS A CELL IT CANNOT RASTER (CT-51 Rule A;
/// [`CursorGlow::thin_caret_hood`]): beside a bar, a cell whose ink the frame has no raster for
/// (an italic letter here, a wide Han letter on GitHub Light's cjk) is read as the reader reads
/// its twin's ground there, the light over the page: a light laid down that cell's edge beside
/// the bar lifts the bar. NEGATIVE CONTROL ([`CursorGlow::set_thin_caret_unrastered`] off): the
/// cell is never ring, the bar reads against the page past the light and keeps its green.
#[test]
fn a_thin_caret_s_ring_reads_a_cell_it_cannot_raster() {
    let g = hold_geom();
    let (page, fill) = (0x0028_2A36, 0x0050_FA7B);
    let caret = ThinCaret {
        cell: (2, 4),
        bar: true,
        page,
        pin: None,
    };
    let cell = aterm_core::terminal::RenderCell {
        fg: [0xF8, 0xF8, 0xF2],
        bg: [0x28, 0x2A, 0x36],
        ..aterm_core::terminal::RenderCell::default()
    };
    let mut cells = vec![vec![cell; g.cols]; g.rows];
    cells[2][3].ch = 'l';
    cells[2][3].italic = true;
    let drawn = |unrastered: bool| {
        let mut glow = CursorGlow::default();
        glow.set_thin_caret_unrastered(unrastered);
        let now = Instant::now();
        glow.observe_frame_text(Some((2, 4)), &cells, now);
        // The left cell's last two columns, down the bar's rows.
        glow.under_out = vec![light_box(30, 32, 2, 16, 0x0080_8080)];
        let mut over = Vec::new();
        let _ = glow.hold_light_for_thin_caret(
            &mut over,
            Some((caret, page)),
            (g, GlowStyle::RainbowKitty, now),
            (&[], 0),
        );
        glow.thin_caret_fill(fill, now)
    };
    assert!(
        drawn(true).is_some_and(|c| c != fill),
        "the light beside the bar lifts it: {:?}",
        drawn(true)
    );
    assert_eq!(drawn(false), None, "control: the cell is no ring");
}

/// A PINNED THIN CARET KEEPS ITS BYTES WHERE NOTHING LOWERS IT ([`ThinCaret::need`]): GitHub's
/// `#8250df` pinned as a bar on white reads 5.05 there, past the legibility bar, and with nothing
/// lit round it is drawn as itself. NEGATIVE CONTROL ([`CursorGlow::set_thin_caret_pin_need`]
/// off): asked the theme caret's bar, it was lifted on every focused frame.
#[test]
fn a_pinned_thin_caret_keeps_its_bytes_at_rest() {
    let g = hold_geom();
    let (page, pin) = (0x00FF_FFFF, 0x0082_50DF);
    let own = crate::color_math::contrast_ratio(pin, page);
    assert!((4.5..THIN_CARET_MIN).contains(&own), "{own}");
    let caret = ThinCaret {
        cell: (2, 4),
        bar: true,
        page,
        pin: Some(pin),
    };
    let drawn = |pin_need: bool| {
        let mut glow = CursorGlow::default();
        glow.set_thin_caret_pin_need(pin_need);
        let now = Instant::now();
        let mut over = Vec::new();
        let _ = glow.hold_light_for_thin_caret(
            &mut over,
            Some((caret, page)),
            (g, GlowStyle::RainbowKitty, now),
            (&[], 0),
        );
        glow.thin_caret_fill(pin, now)
    };
    assert_eq!(drawn(true), None, "the pin keeps its bytes");
    assert!(
        drawn(false).is_some_and(|c| c != pin),
        "control: asked the theme's bar, it is lifted"
    );
}

/// A PINNED THIN CARET IS HELD TO ITS OWN READING ([`ThinCaret::need`]): Dracula's red
/// `#ff5555` pinned as a bar on its own page reads 4.53, and its first leg ends at itself; beside
/// a grey light the hold keeps the light it can (some of it, never none) and the pin reads what it
/// reads on its page. NEGATIVE CONTROL ([`CursorGlow::set_thin_caret_pin_need`] off): asked the
/// theme caret's bar, which its end cannot read on the page itself, it is not read at all and
/// the light lowers it.
#[test]
fn a_pinned_thin_caret_is_held_to_its_own_reading() {
    let g = hold_geom();
    let (page, pin) = (0x0028_2A36, 0x00FF_5555);
    assert!(crate::color_math::contrast_ratio(pin, page) < THIN_CARET_MIN);
    let caret = ThinCaret {
        cell: (2, 4),
        bar: true,
        page,
        pin: Some(pin),
    };
    let grey = 0x0050_5050;
    let run = |pin_need: bool| {
        let mut glow = CursorGlow::default();
        glow.set_thin_caret_pin_need(pin_need);
        let now = Instant::now();
        // The left cell's last two columns, down the bar's rows.
        glow.under_out = vec![light_box(28, 32, 4, 16, grey)];
        let mut over = Vec::new();
        let _ = glow.hold_light_for_thin_caret(
            &mut over,
            Some((caret, page)),
            (g, GlowStyle::RainbowKitty, now),
            (&[], 0),
        );
        let kept: u32 = glow.under_out.iter().map(|q| q.color & 0xFF).sum();
        let reads = glow.thin_hood.reads_all(pin, page, true);
        (kept, reads, glow.thin_caret_fill(pin, now))
    };
    let (kept, reads, fill) = run(true);
    assert!(
        kept > 0,
        "the light is held to the pin's reading, not to nothing"
    );
    assert!(reads, "the pin reads its own reading beside the light");
    assert_eq!(fill, None, "the pin keeps its bytes");
    let (_, reads, _) = run(false);
    assert!(
        !reads,
        "control: asked the theme's bar, the pin is not read"
    );
}

/// What `glow`'s streams lay over `page` at each pixel of the window box `(x0, y0)..(x1, y1)`, and
/// what `light` alone laid there, as `((x, y), laid, uncut)`.
fn laid_beside(
    glow: &CursorGlow,
    over: &[GlowQuad],
    light: GlowQuad,
    page: u32,
    (x0, y0, x1, y1): (i32, i32, i32, i32),
) -> Vec<((i32, i32), u32, u32)> {
    let mut all = Vec::new();
    glow.under_ramps.expand_into(&glow.under_out, &mut all);
    all.extend_from_slice(over);
    let mut out = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let laid = crate::cursor_fx::light_at(page, (&all, &glow.halo_out), (x, y));
            let uncut = crate::cursor_fx::light_at(page, (&[light], &[]), (x, y));
            out.push(((x, y), laid, uncut));
        }
    }
    out
}

/// A HELD RING IS LAID OVER THE LIGHT AND CUTS NOTHING BESIDE IT
/// ([`CursorGlow::hold_light_for_thin_caret`], [`ThinHood::lay_over`]): a grey gradient laid down
/// the cell left of a bar on Dracula's page, which white reads under the bar against, is held
/// only in the column beside the bar, by the page laid over it there, and every other pixel of
/// the gradient keeps the very colour it laid. NEGATIVE CONTROL
/// ([`CursorGlow::thin_ring_cut`]): cut out of the stream, as before, the gradient's piece left of
/// the held column is fitted anew and some pixel of it lays another colour (the `w` beside glass
/// dark typing's bar read 4.58 where the guard laid 4.60).
#[test]
fn a_held_ring_is_laid_over_and_cuts_nothing_beside_it() {
    let g = hold_geom();
    let page = 0x0028_2A36;
    let caret = ThinCaret {
        cell: (2, 4),
        bar: true,
        page,
        pin: None,
    };
    let light = GlowQuad {
        row: 2,
        x: 16,
        y: 32,
        w: 16,
        h: 16,
        color: 0x0093_97A1,
        alpha: 0xFF,
        color2: 0x00B5_B0A3,
        alpha2: 0xFF,
    };
    let run = |cut: bool| {
        let mut glow = CursorGlow {
            thin_ring_cut: cut,
            ..CursorGlow::default()
        };
        let now = Instant::now();
        glow.under_out = vec![light];
        let mut over = Vec::new();
        let _ = glow.hold_light_for_thin_caret(
            &mut over,
            Some((caret, page)),
            (g, GlowStyle::RainbowKitty, now),
            (&[], 0),
        );
        let reads = glow.thin_hood.reads_all(0x00FF_FFFF, page, true);
        (
            laid_beside(&glow, &over, light, page, (14, 30, 34, 50)),
            reads,
        )
    };
    let (laid, reads) = run(false);
    assert!(reads, "white reads beside the held column");
    let moved: Vec<_> = laid.iter().filter(|p| p.1 != p.2).collect();
    assert!(!moved.is_empty(), "the column beside the bar is held");
    assert!(
        moved.iter().all(|p| (30..32).contains(&p.0.0)),
        "only the ring beside the bar is held: {moved:x?}"
    );
    let (laid, _) = run(true);
    assert!(
        laid.iter().any(|p| p.1 != p.2 && p.0.0 < 30),
        "control: cut out of the stream, the gradient left of the ring is laid anew"
    );
}

/// A FOOT RAIL IN THE RING IS NEVER DIMMED ([`HoodPx::keep`]): the trail's underline laid down
/// the cell left of a bar, which white reads under the bar against, keeps every pixel of its
/// light, for a hold there only cuts the streams, whose cut leaves the rail whole: the trail is
/// never dimmed to buy the caret's reading. NEGATIVE CONTROL ([`CursorGlow::thin_keep_off`]):
/// held as any light there, the page is laid over the rail's column beside the bar.
#[test]
fn a_foot_rail_in_the_ring_is_never_dimmed() {
    let g = hold_geom();
    let page = 0x0028_2A36;
    let caret = ThinCaret {
        cell: (2, 4),
        bar: true,
        page,
        pin: None,
    };
    let rail = light_box(16, 32, 16, 16, 0x00B0_B0B0);
    let run = |keep: bool| {
        let mut glow = CursorGlow {
            thin_keep_off: !keep,
            ..CursorGlow::default()
        };
        let now = Instant::now();
        glow.under_out = vec![rail];
        glow.ink_ledger.note_foot_rail(0, 1);
        let mut over = Vec::new();
        let _ = glow.hold_light_for_thin_caret(
            &mut over,
            Some((caret, page)),
            (g, GlowStyle::RainbowKitty, now),
            (&[], 0),
        );
        laid_beside(&glow, &over, rail, page, (14, 30, 34, 50))
    };
    let laid = run(true);
    assert!(
        laid.iter().all(|p| p.1 == p.2),
        "the rail keeps its light: {:x?}",
        laid.iter().filter(|p| p.1 != p.2).collect::<Vec<_>>()
    );
    let laid = run(false);
    assert!(
        laid.iter().any(|p| p.1 != p.2 && p.0.0 == 31),
        "control: held as any light, the rail is dimmed beside the bar"
    );
}

// Share the integration suite's existing caller-thread allocator in this unit binary too.
#[path = "../../../../tests/alloc_count/mod.rs"]
mod thin_alloc_count;

/// Warm the hood's geometry and pixel/channel scratch before installing a light stream.
fn thin_stream_fixture(case: &str) -> (CursorGlow, Vec<GlowQuad>, ThinCaret, Geom) {
    use aterm_core::render::glow_end;
    use aterm_render::glyph_ink::GlyphCoverage;
    assert!(matches!(
        case,
        "none" | "local_mixed" | "distant_ramps" | "distant_quads"
    ));
    let g = hold_geom();
    let page = 0x0028_2A36;
    let caret = ThinCaret {
        cell: (2, 4),
        bar: true,
        page,
        pin: None,
    };
    let cell = aterm_core::terminal::RenderCell {
        fg: [0xF8, 0xF8, 0xF2],
        bg: [0x28, 0x2A, 0x36],
        ..aterm_core::terminal::RenderCell::default()
    };
    let mut cells = vec![vec![cell; g.cols]; g.rows];
    cells[2][4].ch = 'l';
    let at = usize::from(aterm_render::GlyphInk::index('l', false));
    let mut regular: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    let bold: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    regular[at].max = 255;
    regular[at].cell_box = Some([3, 0, 8, 16]);
    let mut rasters = vec![None; 190];
    rasters[at] = Some(aterm_render::GlyphRaster {
        x: 3,
        y: 0,
        w: 5,
        cov: vec![255; 5 * 16].into_boxed_slice(),
    });
    let face = std::sync::Arc::new(
        aterm_render::GlyphInk::from_coverage(regular, bold, false).with_rasters(rasters),
    );
    let mut glow = CursorGlow::default();
    glow.observe_frame_text(Some((2, 4)), &cells, Instant::now());
    glow.observe_glyph_ink(Some(face));
    glow.thin_caret_hood(&[], caret, (g, (&[], 0)), true);
    let mut over = Vec::new();
    if case != "none" {
        glow.under_out.push(GlowQuad {
            row: 2,
            x: 16,
            y: 24,
            w: 48,
            h: 40,
            color: 0x0010_2030,
            alpha: 128,
            color2: 0x0060_2040,
            alpha2: 208,
        });
        glow.under_out.push(GlowQuad {
            row: 2,
            x: 30,
            y: 28,
            w: 20,
            h: 26,
            color: 0x0018_0420,
            alpha: 0,
            color2: 0x0004_1020,
            alpha2: 0,
        });
        over.push(GlowQuad {
            row: 2,
            x: 24,
            y: 22,
            w: 40,
            h: 38,
            color: 0x0028_1808,
            alpha: 96,
            color2: 0x0008_1838,
            alpha2: 176,
        });
        over.push(GlowQuad {
            row: 2,
            x: 31,
            y: 27,
            w: 14,
            h: 30,
            color: 0x0002_0408,
            alpha: 0,
            color2: 0x0008_0402,
            alpha2: 0,
        });
        let ends = (glow_end(0x0018_1020, 112), glow_end(0x0030_1808, 160));
        glow.under_ramps.open_run(1, 2, (20, 20, 52), 16, ends);
        // Keep the joined row run whole when the hood starts inside it.
        assert!(glow.under_ramps.extend_run(0, 1, ends));
        for i in 0u16..24 {
            assert!(glow.under_ramps.extend_run(
                0,
                1,
                (glow_end(0x0010_1808 + u32::from(i), 96), ends.1)
            ));
        }
        glow.out_ramps.open_run(
            0,
            2,
            (22, 24, 44),
            32,
            (glow_end(0x0004_1008, 0), glow_end(0x0014_0410, 80)),
        );
    }
    if case == "distant_ramps" {
        let before = glow.under_ramps.lit;
        glow.under_ramps.open(
            glow.under_out.len(),
            0,
            (0, 1000, 160),
            (0x8010_1820, 0xC020_1008),
        );
        for i in 1u16..4096 {
            assert!(glow.under_ramps.extend_run(
                0,
                1,
                (0x8010_1820 + u32::from(i % 64), 0xC020_1008)
            ));
        }
        assert_eq!(glow.under_ramps.lit - before, 4096);
    }
    if case == "distant_quads" {
        for i in 0u16..4096 {
            glow.under_out.push(GlowQuad {
                row: 0,
                x: 800 + (i % 32) * 8,
                y: 800 + i / 32,
                w: 8,
                h: 1,
                color: 0x0010_1820,
                alpha: 128,
                color2: 0x0020_1008,
                alpha2: 192,
            });
        }
    }
    (glow, over, caret, g)
}

#[test]
fn thin_hood_scratch_allocation_is_local() {
    let observations = ["none", "local_mixed", "distant_ramps", "distant_quads"].map(|case| {
        let (mut glow, over, caret, g) = thin_stream_fixture(case);
        let (_, requests, sizes) = thin_alloc_count::allocations_during(|| {
            glow.thin_caret_hood(&over, caret, (g, (&[], 0)), false)
        });
        assert!(
            requests <= thin_alloc_count::SIZE_SLOTS,
            "all sizes must be retained"
        );
        (case, requests, sizes)
    });
    for (case, requests, sizes) in &observations {
        eprintln!("THIN_ALLOC case={case} requests={requests} sizes={sizes:?}");
    }
    assert!(
        observations.iter().all(|(_, requests, _)| *requests == 0),
        "off-region lights must not allocate warmed hood scratch: {observations:?}"
    );
    // Keep the profiling test registry identical to the frozen baseline: these independent
    // semantic oracles run through this guard, not as extra profile-process test entries.
    bounded_stream_composition_matches_pointwise_lights_without_refitting_gradients();
    bounded_stream_composition_keeps_pixels_outside_a_negative_origin_unlit();
    channel_selection_matches_upper_sorted_medians();
}

#[test]
#[ignore = "manual finite thin-hood native instruction comparison"]
fn thin_hood_stream_cost() {
    let case = std::env::var("ATERM_THIN_STREAM_CASE").expect("profile case");
    let (mut glow, over, caret, g) = thin_stream_fixture(&case);
    let expected = glow.thin_caret_hood(&over, caret, (g, (&[], 0)), true);
    let snapshot = format!("{:?}", glow.thin_hood);
    for _ in 0..512 {
        let cell = std::hint::black_box(&mut glow).thin_caret_hood(
            std::hint::black_box(&over),
            caret,
            (g, (&[], 0)),
            false,
        );
        assert_eq!(cell, expected);
    }
    assert_eq!(format!("{:?}", glow.thin_hood), snapshot);
    eprintln!("THIN_PROFILE case={case} iterations=512 cell={expected} hood={snapshot}");
}

fn bounded_stream_composition_matches_pointwise_lights_without_refitting_gradients() {
    use aterm_core::render::GlowRamps;
    for case in ["none", "local_mixed", "distant_ramps", "distant_quads"] {
        let (glow, over, _, _) = thin_stream_fixture(case);
        for (quads, ramps) in [
            (&glow.under_out, &glow.under_ramps),
            (&over, &glow.out_ramps),
        ] {
            let mut expanded = Vec::new();
            ramps.expand_into(quads, &mut expanded);
            for bounds in [(30, 30, 12, 20), (-2, -2, 12, 20), (0, 20, 12, 20)] {
                let (x, y, w, h) = bounds;
                let base: Vec<u32> = (0..w * h).map(|i| 0x0010_1820 + (i % 31) as u32).collect();
                let mut kept = expanded.clone();
                kept.retain(|q| {
                    q.w > 0
                        && q.h > 0
                        && i32::from(q.x) < x + w
                        && i32::from(q.x) + i32::from(q.w) > x
                        && i32::from(q.y) < y + h
                        && i32::from(q.y) + i32::from(q.h) > y
                });
                let expected: Vec<_> = base
                    .iter()
                    .enumerate()
                    .map(|(i, &c)| {
                        crate::cursor_fx::light_at(
                            c,
                            (&kept, &[]),
                            (x + i as i32 % w, y + i as i32 / w),
                        )
                    })
                    .collect();
                let mut actual = base.clone();
                paint_hood_lights(&mut actual, quads, ramps, bounds);
                assert_eq!(actual, expected, "{case} {bounds:?}");
                if case == "local_mixed" && bounds == (30, 30, 12, 20) {
                    kept.reverse();
                    let mut wrong_order = base.clone();
                    paint_hood_lights(&mut wrong_order, &kept, &GlowRamps::EMPTY, bounds);
                    assert_ne!(
                        wrong_order, expected,
                        "the fixture detects reversed draw order"
                    );
                }
            }
        }
    }
    let bounds = (30, 30, 12, 20);
    let gradient = GlowQuad {
        row: 0,
        x: 16,
        y: 20,
        w: 64,
        h: 40,
        color: 0x0010_2030,
        alpha: 96,
        color2: 0x0060_1020,
        alpha2: 208,
    };
    let mut whole = vec![0x0028_2A36; 240];
    paint_hood_lights(&mut whole, &[gradient], &GlowRamps::EMPTY, bounds);
    let mut refitted = whole.clone();
    refitted.fill(0x0028_2A36);
    paint_hood_lights(
        &mut refitted,
        &[GlowQuad {
            x: 30,
            w: 12,
            ..gradient
        }],
        &GlowRamps::EMPTY,
        bounds,
    );
    assert_ne!(
        whole, refitted,
        "the fixture detects refitted gradient coordinates"
    );
    let mut degenerate = vec![0x0028_2A36; 240];
    let before = degenerate.clone();
    paint_hood_lights(
        &mut degenerate,
        &[GlowQuad { w: 0, ..gradient }, GlowQuad { h: 0, ..gradient }],
        &GlowRamps::EMPTY,
        bounds,
    );
    assert_eq!(degenerate, before, "empty quads contribute no light");
}

fn bounded_stream_composition_keeps_pixels_outside_a_negative_origin_unlit() {
    use aterm_core::render::GlowRamps;
    let light = GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: 20,
        h: 20,
        color: 0x0010_2010,
        alpha: 64,
        color2: 0x0030_0818,
        alpha2: 160,
    };
    let mut actual = vec![0x0028_2A36; 240];
    paint_hood_lights(&mut actual, &[light], &GlowRamps::EMPTY, (-2, -2, 12, 20));
    let expected: Vec<_> = (0..240)
        .map(|i| {
            crate::cursor_fx::light_at(0x0028_2A36, (&[light], &[]), (-2 + i % 12, -2 + i / 12))
        })
        .collect();
    assert_eq!(actual, expected);
    assert!(
        actual.iter().any(|&p| p != 0x0028_2A36),
        "the fixture lays real light"
    );
    assert_eq!(actual[0], 0x0028_2A36, "the off-window corner stays unlit");
}

fn channel_selection_matches_upper_sorted_medians() {
    assert_eq!(upper_channel_median(&[]), 0);
    for a in 0u16..=255 {
        for b in 0u16..=255 {
            let (a, b) = (a as u8, b as u8);
            assert_eq!(upper_channel_median(&[a, b]), a.max(b));
            for c in 0u16..=255 {
                let c = c as u8;
                let sum = u16::from(a) + u16::from(b) + u16::from(c);
                let mid = sum - u16::from(a.min(b).min(c)) - u16::from(a.max(b).max(c));
                assert_eq!(u16::from(upper_channel_median(&[a, b, c])), mid);
            }
        }
    }
    for len in 1..=257 {
        let values: Vec<u8> = (0..len).map(|i| ((i * 37) % 256) as u8).collect();
        let mut sorted = values.clone();
        sorted.sort_unstable();
        assert_eq!(upper_channel_median(&values), sorted[len / 2]);
        let reverse: Vec<_> = values.iter().rev().copied().collect();
        assert_eq!(upper_channel_median(&reverse), sorted[len / 2]);
        assert_eq!(upper_channel_median(&vec![127; len]), 127);
    }
    assert_eq!(
        upper_channel_median(&[0, 0, 255, 255]),
        255,
        "even samples retain the upper, not lower, median"
    );
}
