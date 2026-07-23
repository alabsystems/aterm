// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// EMBERFORGE GLYPH CONTRAST-HALO — the text-legibility layer for the fire
// cursor effect. Where a bright flame sits behind a glyph the letterform would
// wash out; the renderer draws a dark warm DILATION of the glyph's own coverage
// (`aterm_render::HALO_DILATE_OFFSETS` stamps in `HALO_IN_FIRE_RGB`) OVER the
// flame and UNDER the glyph ink, so the letter always separates from the fire.
// The halo keys on the COLOUR-FREE `fire_halo` strength stream — the ink is
// NEVER recoloured (the no-recolor law; the owner vetoed ink recolouring
// twice, v0.41/v0.42). The contract under test:
//   * an engulfed glyph (a `fire_halo` cell in a `fire_patch` frame) is ringed
//     by pixels DARKER than both the glyph ink AND the surrounding fire — the
//     legibility guarantee, verified as a WCAG contrast ratio at full blaze;
//   * the halo appears ONLY where fire is present: `fire_halo` WITHOUT a live
//     `fire_patch` draws no halo — and neither does `char_fg` (the retired
//     recolour stream keeps working as pure fg substitution but never keys
//     the ring) — and rows the fire never touches are byte-identical to a
//     plain frame (no global text-shadow regression);
//   * the halo's alpha SCALES with the engulfment strength — a lick barely
//     rims, the wall rims firmly;
//   * the halo DECAYS with the fire — removing the fire field removes the ring;
//   * dirty gate: settled strengths gate-hit; a strength change sets
//     `fire_halo_changed` and marks exactly its prev∪cur rows.

use aterm_core::render::{CharFg, FireHaloCell, FireMode, FirePatch};
use aterm_core::terminal::Terminal;
use aterm_render::{DirtyDecision, Renderer, Theme, compute_dirty_rows};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
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

/// A single bright single-row fire patch filling `row`'s band across a column
/// span — the parity harness's `emit_burn`, distilled to one hot band that
/// stays INSIDE its own row (so neighbouring rows are provably fire-free).
fn bright_fire(row: u16, ch: usize, x: usize, w: usize, cov_cap: u8) -> FirePatch {
    FirePatch {
        row,
        x: x as u16,
        y: (row as usize * ch) as u16,
        w: w as u16,
        h: ch as u16,
        // Root at the band's BOTTOM; a tall peak fills the whole band with the
        // hot root palette, so the band is reliably, brightly lit.
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

/// A full-engulfment halo cell — at strength 255 the stamp alpha is exactly
/// the historical `HALO_IN_FIRE_ALPHA` ceiling.
fn halo(row: u16, col: u16, strength: u8) -> FireHaloCell {
    FireHaloCell { row, col, strength }
}

/// The heat-glow ink the RETIRED recolour stream would substitute (hot gold) —
/// kept to pin that `char_fg` still works as pure fg substitution and never
/// keys the halo ring.
const HEAT_GLOW_FG: u32 = 0x00FF_C87A;

/// THE LEGIBILITY GUARANTEE. A full-block glyph engulfed by a BRIGHT flame is
/// ringed by a dark warm halo: pixels around the strokes that are darker than
/// both the (plain, never-recoloured) glyph ink and the flame beside them —
/// and that ring survives at FULL blaze (`cov_cap == 255`) with a defensible
/// WCAG contrast ratio between the glyph stroke and its immediate surround.
#[test]
fn halo_separates_engulfed_glyph_from_bright_fire() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // A full-block glyph at row 1, col 4 (dense coverage, spaces around it so
    // the ring lands in the neighbouring cells' fire), cursor hidden.
    term.process(b"\x1b[?25l\x1b[2;5H\xe2\x96\x88");

    let fire = bright_fire(1, ch, cw, 8 * cw, 255);

    // (a) FIRE ONLY (no fire_halo → no halo): the ring band shows raw flame.
    let mut fire_only = term.cell_frame(rows, cols);
    fire_only.fire_patch = vec![fire];
    let fo = rend.render_input(&fire_only).pixels.clone();

    // (b) FIRE + fire_halo → the halo rings the engulfed glyph. The ink is
    // PLAIN theme fg — never recoloured (the no-recolor law).
    let mut haloed = term.cell_frame(rows, cols);
    haloed.fire_patch = vec![fire];
    haloed.fire_halo = vec![halo(1, 4, 255)];
    let f = rend.render_input(&haloed);
    let px = &f.pixels;
    let stride = f.width;
    let pad = (f.width - cols * cw) / 2;

    // The block cell rect (col 4, row 1) plus a 3 px margin — the region the
    // ring lives in. Middle-height band avoids the row boundaries.
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
            // The RING is the fire-covered margin OUTSIDE the glyph cell (no
            // glyph ink there): the halo darkens the flame, isolating its
            // footprint from the glyph's own cell.
            let in_margin = x < bx0 || x >= bx0 + cw;
            if in_margin && luma(h) + 90 < luma(fo[i]) {
                darkened += 1;
            }
        }
    }
    // Contrast at the seam: the brightest stroke pixel vs the darkest halo pixel
    // directly beside a bright stroke pixel (the immediate surround).
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

/// NO GLOBAL TEXT-SHADOW REGRESSION. The halo is gated on a LIVE fire field
/// AND keys ONLY on the `fire_halo` stream:
///   * a `fire_halo` cell with an EMPTY `fire_patch` draws NO halo — and since
///     the stream carries no colour, the whole frame is byte-identical to the
///     plain one (the strongest possible no-recolor pin);
///   * a `char_fg` cell never keys the halo — WITH or WITHOUT fire it stays a
///     pure fg substitution (the retired recolour mechanism kept intact);
///   * rows the fire never reaches are byte-identical to a plain frame — normal
///     text everywhere else is untouched.
#[test]
fn no_halo_without_fire_and_untouched_rows_byte_identical() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Text on all three rows; the fire (and halo) will touch ONLY row 1.
    term.process(b"\x1b[?25l\x1b[1;1Hplain row aa\x1b[2;1Hburning line\x1b[3;1Hplain row cc");

    // Plain reference: no fire, no fire_halo, no char_fg anywhere.
    let plain = rend
        .render_input(&term.cell_frame(rows, cols))
        .pixels
        .clone();

    // fire_halo on row 1 WITHOUT any fire: gated OFF, and the stream carries
    // no colour — so the ENTIRE frame is byte-identical to plain (the ink is
    // never recoloured; a decayed fire leaves zero residue).
    let mut no_fire = term.cell_frame(rows, cols);
    no_fire.fire_halo = (0..6).map(|c| halo(1, c, 255)).collect();
    let nf = rend.render_input(&no_fire).pixels.clone();
    assert_eq!(
        nf, plain,
        "fire_halo WITHOUT fire must be byte-identical to the plain frame \
         (no halo, no recolour — nothing)"
    );

    // char_fg on row 1 WITHOUT any fire: the retired recolour mechanism still
    // substitutes ink (kept intact), but must add NO halo — rows 0 and 2 stay
    // byte-identical to plain, and row 1 differs ONLY on glyph ink, never in
    // the inter-glyph gaps (no dark ring where a glyph is absent).
    let mut char_only = term.cell_frame(rows, cols);
    char_only.char_fg = (0..6)
        .map(|c| CharFg {
            row: 1,
            col: c,
            fg: HEAT_GLOW_FG,
        })
        .collect();
    let co = rend.render_input(&char_only).pixels.clone();

    // Fire + fire_halo on row 1 only — the halo lands, but confined to row 1.
    // char_fg rides too (it must keep working independently, and must still
    // key NO ring of its own — the ring below is measured in the inter-glyph
    // gaps of the HALOED cells only).
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

    // Rows 0 and 2 (bands OUTSIDE row 1) must be byte-identical to plain in
    // the char_fg-only and the lit frame: the halo/fire never leak off their row.
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

    // Within row 1, the no-fire char_fg frame must add NO dark ring: sample the
    // inter-glyph gaps (between chars) — with no fire they stay at the plain
    // background, never the dark halo.
    let y_mid = pad + ch + ch / 2;
    let mut halo_pixels_without_fire = 0usize;
    for c in 0..6usize {
        // The right edge of glyph `c` into the next cell — where a ring would sit.
        let x = pad + (c + 1) * cw - 1;
        let i = y_mid * width + x;
        // A halo pixel would be markedly darker than the plain background here.
        if luma(co[i]) + 60 < luma(plain[i]) {
            halo_pixels_without_fire += 1;
        }
    }
    assert_eq!(
        halo_pixels_without_fire, 0,
        "char_fg WITHOUT fire must draw NO halo (found {halo_pixels_without_fire} dark ring px)"
    );
}

/// THE HALO DECAYS WITH THE FIRE. The halo's VISIBLE footprint is the ring of
/// fire it darkens around the glyph (measured against the same frame WITHOUT
/// the fire_halo cell, so the dark theme background — itself dark — never
/// confounds the count). That footprint scales with the fire: a full blaze
/// darkens a big bright ring; a weak ember darkens far less; and with the fire
/// gone there is nothing to darken (and no ring at all — the gate).
#[test]
fn halo_decays_when_fire_recedes() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l\x1b[2;5H\xe2\x96\x88");
    let engulfed = halo(1, 4, 255);

    // The halo footprint at fire coverage `cov_cap`: region pixels the halo
    // (fire_halo on) pulled well below the SAME fire without the halo
    // (fire_halo off). `cov_cap == 0` means "no fire" — then the reference is
    // the bare plain frame and the colour-free stream changes nothing, so the
    // darkened count is 0.
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
        // Scan the two 3 px margins OUTSIDE the block cell only — the ring,
        // never the glyph's own interior.
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

/// THE STRENGTH LAW: the halo's alpha scales with the cell's engulfment
/// strength (`fire_halo_alpha`: floor 90 → ceiling 235) — a lick barely rims,
/// the wall rims firmly. Measured as the total luma the ring REMOVES from the
/// same bright fire (the summed deficit is linear in the stamp alpha, so the
/// ordering is exact): full strength darkens strictly more than a weak lick,
/// and even the weak lick darkens something (the alpha floor — no pop-in).
#[test]
fn halo_alpha_scales_with_engulfment_strength() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l\x1b[2;5H\xe2\x96\x88");
    let fire = bright_fire(1, ch, cw, 8 * cw, 255);

    // Reference: the same bright fire with NO halo.
    let mut ref_in = term.cell_frame(rows, cols);
    ref_in.fire_patch = vec![fire];
    let rf = rend.render_input(&ref_in);
    let (px_ref, width) = (rf.pixels.clone(), rf.width);
    let pad = (width - cols * cw) / 2;
    let bx0 = pad + 4 * cw;
    let (by0, by1) = (pad + ch + ch / 4, pad + ch + (3 * ch) / 4);

    // Summed luma deficit the halo at `strength` carves out of the ring
    // margins (outside the block cell, where only fire+halo pixels live).
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

/// DIRTY GATE: settled fire_halo strengths (equal, non-empty) gate-hit with
/// nothing marked; a strength change (the swelling/decaying engulfment) sets
/// `fire_halo_changed` and marks exactly its prev∪cur rows — the
/// char_fg/ink prev∪cur discipline on the colour-free stream.
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

    // A STRENGTH-only change (same cell, the flame swelling): the row repaints.
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

    // The halo sweeps row 1 → row 4: prev AND cur rows must repaint (the
    // vacated ring restored fresh, the new ring landed), nothing else.
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
