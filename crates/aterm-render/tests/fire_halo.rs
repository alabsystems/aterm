// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Fire-cursor contrast halo: where a bright flame sits behind a glyph, the renderer draws a
// dark warm dilation of the glyph's coverage (`HALO_DILATE_OFFSETS` stamps in
// `HALO_IN_FIRE_RGB`) over the flame and under the ink. It keys on the colour-free
// `fire_halo` strength stream; the ink is never recoloured. Pinned here: the ring is darker
// than both ink and fire (WCAG contrast at full blaze); no halo without a live `fire_patch`
// or from `char_fg`, and untouched rows stay byte-identical; alpha scales with strength and
// the ring vanishes with the fire; the dirty gate marks exactly prev∪cur rows.

use aterm_core::render::{CharFg, FireHaloCell, FireMode, FirePatch};
use aterm_core::terminal::Terminal;
use aterm_render::{DirtyDecision, Renderer, Theme, compute_dirty_rows};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// Summed-RGB luminance proxy (monotone per channel).
fn luma(p: u32) -> u32 {
    ((p >> 16) & 0xff) + ((p >> 8) & 0xff) + (p & 0xff)
}

/// sRGB channel → relative-luminance contribution (WCAG linearization).
fn lin(c: u32) -> f64 {
    let s = c as f64 / 255.0;
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance of a `0x00RRGGBB` pixel, 0..=1.
fn rel_luma(p: u32) -> f64 {
    0.2126 * lin((p >> 16) & 0xff) + 0.7152 * lin((p >> 8) & 0xff) + 0.0722 * lin(p & 0xff)
}

/// WCAG contrast ratio between two pixels (>= 1.0).
fn contrast(a: u32, b: u32) -> f64 {
    let (la, lb) = (rel_luma(a), rel_luma(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// One bright fire patch filling `row`'s band across a column span, staying inside its own
/// row so neighbouring rows are fire-free.
fn bright_fire(row: u16, ch: usize, x: usize, w: usize, cov_cap: u8) -> FirePatch {
    FirePatch {
        row,
        x: x as u16,
        y: (row as usize * ch) as u16,
        w: w as u16,
        h: ch as u16,
        // Root at the band's bottom; a tall peak fills the band with the hot root palette.
        base_y: ((row as usize + 1) * ch) as u16,
        peak_h: (3 * ch) as u16,
        phase: 4096,
        temp: 240,
        strength: 255,
        lean: 0,
        cov_cap,
        cell_h: ch as u16,
        mode: FireMode::Add,
    }
}

/// A halo cell; at strength 255 the stamp alpha is the `HALO_IN_FIRE_ALPHA` ceiling.
fn halo(row: u16, col: u16, strength: u8) -> FireHaloCell {
    FireHaloCell { row, col, strength }
}

/// Hot-gold `char_fg` ink, to pin that `char_fg` stays a pure fg substitution and never keys
/// the halo.
const HEAT_GLOW_FG: u32 = 0x00FF_C87A;

/// A full-block glyph engulfed by a bright flame is ringed by pixels darker than both its
/// (never recoloured) ink and the flame, with at least WCAG 4.5:1 between stroke and
/// surround at full blaze (`cov_cap == 255`).
#[test]
fn halo_separates_engulfed_glyph_from_bright_fire() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // A full block at row 1, col 4, with spaces around it so the ring lands in fire.
    term.process(b"\x1b[?25l\x1b[2;5H\xe2\x96\x88");

    let fire = bright_fire(1, ch, cw, 8 * cw, 255);

    // (a) Fire only: the ring band shows raw flame.
    let mut fire_only = term.cell_frame(rows, cols);
    fire_only.fire_patch = vec![fire];
    let fo = rend.render_input(&fire_only).pixels.clone();

    // (b) Fire + `fire_halo`: the halo rings the glyph; the ink stays theme fg.
    let mut haloed = term.cell_frame(rows, cols);
    haloed.fire_patch = vec![fire];
    haloed.fire_halo = vec![halo(1, 4, 255)];
    let f = rend.render_input(&haloed);
    let px = &f.pixels;
    let stride = f.width;
    let pad = (f.width - cols * cw) / 2;

    // The block cell plus a 3 px margin, at mid-height to avoid the row boundaries.
    let bx0 = pad + 4 * cw;
    let by0 = pad + ch + ch / 4;
    let by1 = pad + ch + (3 * ch) / 4;
    let (rx0, rx1) = (bx0 - 3, bx0 + cw + 3);

    let mut glyph_max = 0u32; // brightest pixel (the plain-ink stroke)
    let mut halo_min = u32::MAX; // darkest pixel (the halo ring)
    let mut darkened = 0usize; // MARGIN ring pixels the halo pulled below the fire
    let mut best_contrast = 0.0f64; // stroke vs an immediately-adjacent ring px
    for y in by0..by1 {
        for x in rx0..rx1 {
            let i = y * stride + x;
            let h = px[i];
            glyph_max = glyph_max.max(luma(h));
            halo_min = halo_min.min(luma(h));
            // The ring is the fire-covered margin outside the glyph cell.
            let in_margin = x < bx0 || x >= bx0 + cw;
            if in_margin && luma(h) + 90 < luma(fo[i]) {
                darkened += 1;
            }
        }
    }
    // Contrast: the brightest stroke pixel against the halo pixels just beside it.
    let mut brightest = (0u32, 0usize);
    for y in by0..by1 {
        for x in rx0..rx1 {
            let i = y * stride + x;
            if luma(px[i]) > brightest.0 {
                brightest = (luma(px[i]), i);
            }
        }
    }
    let bi = brightest.1;
    for (dx, dy) in [(-2i32, 0i32), (2, 0), (0, -2), (0, 2), (-3, 0), (3, 0)] {
        let j = (bi as i32 + dy * stride as i32 + dx) as usize;
        if j < px.len() {
            best_contrast = best_contrast.max(contrast(px[bi], px[j]));
        }
    }

    eprintln!(
        "contrast-halo: glyph_max_luma={glyph_max} halo_min_luma={halo_min} \
         darkened_ring_px={darkened} stroke-vs-surround WCAG contrast={best_contrast:.2}"
    );

    assert!(
        glyph_max > 400,
        "the plain-ink glyph must stay bright inside the flame (max luma {glyph_max})"
    );
    assert!(
        halo_min < 130,
        "a dark halo ring must exist around the glyph (min luma {halo_min})"
    );
    assert!(
        halo_min + 200 < glyph_max,
        "the halo ({halo_min}) must be clearly darker than the glyph ink ({glyph_max})"
    );
    assert!(
        darkened >= 12,
        "the halo must darken a real RING of fire around the glyph ({darkened} px)"
    );
    assert!(
        best_contrast >= 4.5,
        "LEGIBILITY GUARANTEE: the glyph stroke vs its immediate (fire+halo) \
         surround must clear a WCAG 4.5:1 contrast even at full blaze (got {best_contrast:.2})"
    );
}

/// The halo needs a live fire field and keys only on `fire_halo`: `fire_halo` with an
/// empty `fire_patch` leaves the frame byte-identical; `char_fg` never keys a ring, with or
/// without fire; rows the fire never reaches are byte-identical to a plain frame.
#[test]
fn no_halo_without_fire_and_untouched_rows_byte_identical() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Text on all three rows; fire and halo touch only row 1.
    term.process(b"\x1b[?25l\x1b[1;1Hplain row aa\x1b[2;1Hburning line\x1b[3;1Hplain row cc");

    let plain = rend
        .render_input(&term.cell_frame(rows, cols))
        .pixels
        .clone();

    // `fire_halo` without fire: the whole frame is byte-identical to plain.
    let mut no_fire = term.cell_frame(rows, cols);
    no_fire.fire_halo = (0..6).map(|c| halo(1, c, 255)).collect();
    let nf = rend.render_input(&no_fire).pixels.clone();
    assert_eq!(
        nf, plain,
        "fire_halo WITHOUT fire must be byte-identical to the plain frame \
         (no halo, no recolour — nothing)"
    );

    // `char_fg` without fire substitutes ink but adds no halo: other rows match plain and
    // the inter-glyph gaps in row 1 stay unringed.
    let mut char_only = term.cell_frame(rows, cols);
    char_only.char_fg = (0..6)
        .map(|c| CharFg {
            row: 1,
            col: c,
            fg: HEAT_GLOW_FG,
        })
        .collect();
    let co = rend.render_input(&char_only).pixels.clone();

    // Fire + `fire_halo` (+ `char_fg`) on row 1: the halo lands, confined to row 1.
    let mut lit = term.cell_frame(rows, cols);
    lit.fire_patch = vec![bright_fire(1, ch, 0, cols * cw, 200)];
    lit.fire_halo = (0..6).map(|c| halo(1, c, 255)).collect();
    lit.char_fg = (0..6)
        .map(|c| CharFg {
            row: 1,
            col: c,
            fg: HEAT_GLOW_FG,
        })
        .collect();
    let f = rend.render_input(&lit).pixels.clone();

    let width = rend.render_input(&term.cell_frame(rows, cols)).width;
    let pad = (width - cols * cw) / 2;

    // Rows 0 and 2 are byte-identical to plain in both frames.
    let r1_lo = pad + ch;
    let r1_hi = pad + 2 * ch;
    let mut changed_off_row = 0usize;
    for i in 0..plain.len() {
        let y = i / width;
        if (r1_lo..r1_hi).contains(&y) {
            continue;
        }
        assert_eq!(
            co[i], plain[i],
            "char_fg-only must not touch other rows (y={y})"
        );
        assert_eq!(f[i], plain[i], "fire+halo must stay inside row 1 (y={y})");
        if f[i] != plain[i] {
            changed_off_row += 1;
        }
    }
    assert_eq!(changed_off_row, 0, "nothing off row 1 may change");

    // In row 1 the no-fire `char_fg` frame leaves the inter-glyph gaps at plain background.
    let y_mid = pad + ch + ch / 2;
    let mut halo_pixels_without_fire = 0usize;
    for c in 0..6usize {
        let x = pad + (c + 1) * cw - 1;
        let i = y_mid * width + x;
        if luma(co[i]) + 60 < luma(plain[i]) {
            halo_pixels_without_fire += 1;
        }
    }
    assert_eq!(
        halo_pixels_without_fire, 0,
        "char_fg WITHOUT fire must draw NO halo (found {halo_pixels_without_fire} dark ring px)"
    );
}

/// The halo's footprint (ring pixels darkened against the same frame without `fire_halo`)
/// shrinks as the fire weakens and is zero with the fire gone.
#[test]
fn halo_decays_when_fire_recedes() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l\x1b[2;5H\xe2\x96\x88");
    let engulfed = halo(1, 4, 255);

    // Ring pixels the halo pulls well below the same fire without it; `cov_cap == 0` means
    // no fire, so the colour-free stream changes nothing.
    let mut footprint = |cov_cap: u8| -> usize {
        let with_fire = cov_cap > 0;
        let mut lit = term.cell_frame(rows, cols);
        let mut ref_in = term.cell_frame(rows, cols);
        if with_fire {
            let fire = bright_fire(1, ch, cw, 8 * cw, cov_cap);
            lit.fire_patch = vec![fire];
            ref_in.fire_patch = vec![fire];
        }
        lit.fire_halo = vec![engulfed];
        let lf = rend.render_input(&lit).pixels.clone();
        let rf = rend.render_input(&ref_in);
        let (px_ref, width) = (rf.pixels.clone(), rf.width);
        let pad = (width - cols * cw) / 2;
        let bx0 = pad + 4 * cw;
        let (by0, by1) = (pad + ch + ch / 4, pad + ch + (3 * ch) / 4);
        // Scan the two 3 px margins outside the block cell only.
        let mut darkened = 0usize;
        for y in by0..by1 {
            for x in ((bx0 - 3)..bx0).chain((bx0 + cw)..(bx0 + cw + 3)) {
                let i = y * width + x;
                if luma(lf[i]) + 70 < luma(px_ref[i]) {
                    darkened += 1;
                }
            }
        }
        darkened
    };

    let blaze = footprint(255);
    let ember = footprint(48);
    let gone = footprint(0);
    eprintln!("halo decay footprint: blaze={blaze}, ember={ember}, fire gone={gone}");
    assert!(
        blaze >= 12,
        "a full blaze must ring the glyph with a substantial dark halo ({blaze})"
    );
    assert!(
        ember < blaze,
        "the halo footprint must SHRINK as the fire weakens (ember {ember} vs blaze {blaze})"
    );
    assert_eq!(
        gone, 0,
        "with the fire gone the halo vanishes — no ring survives ({gone})"
    );
}

/// The halo alpha scales with engulfment strength (`fire_halo_alpha`: floor 90, ceiling
/// 235). The summed luma deficit in the ring is linear in alpha, so full strength removes
/// strictly more than a weak lick, and the lick still removes something (the floor).
#[test]
fn halo_alpha_scales_with_engulfment_strength() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l\x1b[2;5H\xe2\x96\x88");
    let fire = bright_fire(1, ch, cw, 8 * cw, 255);

    let mut ref_in = term.cell_frame(rows, cols);
    ref_in.fire_patch = vec![fire];
    let rf = rend.render_input(&ref_in);
    let (px_ref, width) = (rf.pixels.clone(), rf.width);
    let pad = (width - cols * cw) / 2;
    let bx0 = pad + 4 * cw;
    let (by0, by1) = (pad + ch + ch / 4, pad + ch + (3 * ch) / 4);

    // Summed luma deficit in the ring margins outside the block cell.
    let mut deficit = |strength: u8| -> u64 {
        let mut lit = term.cell_frame(rows, cols);
        lit.fire_patch = vec![fire];
        lit.fire_halo = vec![halo(1, 4, strength)];
        let lf = rend.render_input(&lit).pixels.clone();
        let mut sum = 0u64;
        for y in by0..by1 {
            for x in ((bx0 - 3)..bx0).chain((bx0 + cw)..(bx0 + cw + 3)) {
                let i = y * width + x;
                sum += u64::from(luma(px_ref[i]).saturating_sub(luma(lf[i])));
            }
        }
        sum
    };

    let wall = deficit(255);
    let lick = deficit(24);
    eprintln!("halo strength scaling: wall deficit={wall}, lick deficit={lick}");
    assert!(
        lick > 0,
        "even a weak lick rims a little (the 90-alpha floor — no pop-in)"
    );
    assert!(
        wall > lick,
        "the wall must rim strictly harder than a lick (wall {wall} vs lick {lick})"
    );
}

/// Settled `fire_halo` strengths gate-hit with nothing marked; a strength change or move
/// sets `fire_halo_changed` and marks exactly prev∪cur rows.
#[test]
fn fire_halo_dirty_gate_marks_prev_and_cur_rows() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set

    let mut frame = |cells: &[FireHaloCell]| {
        let mut input = term.cell_frame(6, 8);
        input.fire_halo = cells.to_vec();
        input
    };
    let marked = |dirty: &[bool]| -> Vec<usize> {
        dirty
            .iter()
            .enumerate()
            .filter_map(|(r, &b)| b.then_some(r))
            .collect()
    };

    let settled = [halo(1, 2, 160)];
    let prev = frame(&settled);
    let cur = frame(&settled);
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.fire_halo_changed,
        "settled fire_halo must not set fire_halo_changed"
    );
    assert!(d.is_gate_hit(), "settled fire_halo must gate-hit");
    assert!(dirty.iter().all(|&b| !b), "settled fire_halo marks no rows");

    // A strength-only change repaints its row.
    let cur = frame(&[halo(1, 2, 40)]);
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.fire_halo_changed,
        "a strength change must set fire_halo_changed"
    );
    assert!(!d.is_gate_hit(), "a changed fire_halo must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1],
        "a strength-only change marks exactly its row"
    );

    // The halo moves row 1 → row 4: both rows repaint, nothing else.
    let cur = frame(&[halo(4, 5, 160)]);
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.fire_halo_changed,
        "a moved fire_halo must set fire_halo_changed"
    );
    assert!(!d.is_gate_hit(), "a moved fire_halo must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 4],
        "a moved halo cell must mark exactly its prev∪cur rows"
    );
}
