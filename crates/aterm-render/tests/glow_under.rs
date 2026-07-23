// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// EMBERFORGE "dark glyph cores" on the CPU renderer — the two P6 consumer
// streams: `RenderInput.glow_under` (flame-BODY additive light drawn between
// the cell background fill and the glyph ink) and `RenderInput.char_fg`
// (per-cell FINAL glyph-ink overrides charring engulfed letterforms toward
// ember-black). The contract under test:
//   * an empty `glow_under` / an empty `char_fg` (never touched, explicitly
//     emptied, and pushed-then-cleared) is byte-identical to the pre-feature
//     path, also after `clear_overlays` (the `image plain` contract) — the
//     no-op laws;
//   * the SILHOUETTE LAW: with `glow_under` lighting a row band and `char_fg`
//     pinning a glyph near-black, the glyph stroke is DARKER than the lit
//     background beside it — the letter reads as a dark core INSIDE the fire,
//     because the light went UNDER the ink;
//   * `glow_under` light only ever brightens the pre-light frame and stays
//     inside its quads' row bands (additive containment);
//   * dirty gate: settled streams (equal, non-empty) gate-hit; a moved quad
//     sets `glow_under_changed` and marks exactly its prev∪cur rows; a
//     char_fg change sets `char_fg_changed` and marks its rows;
//   * damaged path: animating quad + charring sweep re-render with no
//     ghosting (cached == fresh, byte-for-byte) — the aurora discipline.

use aterm_core::render::{CharFg, GlowQuad};
use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, DirtyDecision, Renderer, Theme, WindowCpu, compute_dirty_rows};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

/// A single-row-band flame-body quad (the producer splits row-spanning bodies
/// into per-row quads — the GlowQuad invariant).
fn under(row: u16, x: u16, y: u16, w: u16, h: u16, color: u32) -> GlowQuad {
    GlowQuad {
        row,
        x,
        y,
        w,
        h,
        color,
    }
}

/// The luminance proxy: summed RGB channels (monotone in every channel under
/// additive light and under a darker substituted fg, so ordering is exact).
fn luma(p: u32) -> u32 {
    ((p >> 16) & 0xff) + ((p >> 8) & 0xff) + (p & 0xff)
}

/// NO-OP LAW (glow_under): empty — untouched, explicitly emptied, and
/// pushed-then-cleared — must be byte-identical to the pre-feature path, and
/// `clear_overlays` strips a populated stream back to the bare frame.
#[test]
fn empty_glow_under_is_byte_identical_to_before() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25lember forge");

    // Pre-feature frame: the snapshot as built, glow_under never mentioned.
    let base = rend.render_input(&term.cell_frame(3, 12)).pixels.clone();

    // Explicit empty, and pushed-then-cleared (the emptied-stream path).
    let mut input = term.cell_frame(3, 12);
    input.glow_under = Vec::new();
    let explicit = rend.render_input(&input).pixels.clone();
    assert_eq!(base, explicit, "explicit empty glow_under must be a no-op");
    input
        .glow_under
        .push(under(1, 0, ch as u16, cw as u16, ch as u16, 0x0040_2008));
    input.glow_under.clear();
    let emptied = rend.render_input(&input).pixels.clone();
    assert_eq!(base, emptied, "an emptied glow_under must leave no residue");

    // A populated stream paints; `clear_overlays` restores the bare frame.
    let mut with_under = term.cell_frame(3, 12);
    with_under.glow_under = vec![under(
        1,
        0,
        ch as u16,
        (3 * cw) as u16,
        ch as u16,
        0x0060_3010,
    )];
    let painted = rend.render_input(&with_under).pixels.clone();
    assert_ne!(base, painted, "a non-empty glow_under must paint something");
    with_under.clear_overlays();
    assert!(
        with_under.glow_under.is_empty(),
        "clear_overlays must strip glow_under (it IS bling)"
    );
    let stripped = rend.render_input(&with_under).pixels.clone();
    assert_eq!(base, stripped, "clear_overlays must restore the bare frame");
}

/// NO-OP LAW (char_fg): empty — untouched, explicitly emptied, and
/// pushed-then-cleared — must be byte-identical to the pre-feature path, and
/// `clear_overlays` strips a populated stream back to the bare frame.
#[test]
fn empty_char_fg_is_byte_identical_to_before() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25lember forge");

    let base = rend.render_input(&term.cell_frame(3, 12)).pixels.clone();

    let mut input = term.cell_frame(3, 12);
    input.char_fg = Vec::new();
    let explicit = rend.render_input(&input).pixels.clone();
    assert_eq!(base, explicit, "explicit empty char_fg must be a no-op");
    input.char_fg.push(CharFg {
        row: 0,
        col: 0,
        fg: 0x0010_0804,
    });
    input.char_fg.clear();
    let emptied = rend.render_input(&input).pixels.clone();
    assert_eq!(base, emptied, "an emptied char_fg must leave no residue");

    // A populated override recolours a glyph; `clear_overlays` restores it.
    let mut with_char = term.cell_frame(3, 12);
    with_char.char_fg = vec![CharFg {
        row: 0,
        col: 0,
        fg: 0x0010_0804,
    }];
    let painted = rend.render_input(&with_char).pixels.clone();
    assert_ne!(base, painted, "a char_fg override must recolour its glyph");
    with_char.clear_overlays();
    assert!(
        with_char.char_fg.is_empty(),
        "clear_overlays must strip char_fg (it IS bling)"
    );
    let stripped = rend.render_input(&with_char).pixels.clone();
    assert_eq!(base, stripped, "clear_overlays must restore the bare frame");
}

/// THE SILHOUETTE LAW: flames engulf a line — `glow_under` lights the row band
/// UNDER the ink and `char_fg` pins the glyph near-black — so the stroke is
/// DARKER than the lit background beside it (the letter is the darkest thing
/// inside the fire). Plus the additive laws: the light only ever brightens the
/// pre-light frame, and nothing outside the quad's row band changes.
#[test]
fn silhouette_law_glyph_darker_than_lit_background() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    // A full-block glyph at row 1 col 2: full-coverage stroke pixels, so the
    // stroke sample is exact on any font. Cursor hidden.
    term.process(b"\x1b[?25l\x1b[2;3H\xe2\x96\x88");

    let charred = CharFg {
        row: 1,
        col: 2,
        fg: 0x0010_0804, // near-black ember char
    };
    // The flame body: one bright quad filling row 1's band across cols 0..6.
    let body = under(
        1,
        0,
        ch as u16,
        (6 * cw) as u16,
        ch as u16,
        0x0080_4010, // premultiplied orange light
    );

    // Pre-light frame: charred glyph, no flame body (the brighten-only base).
    let mut dark_in = term.cell_frame(3, 10);
    dark_in.char_fg = vec![charred];
    let dark = rend.render_input(&dark_in).pixels.clone();

    // Lit frame: the same char + the flame body underneath the ink.
    let mut lit_in = term.cell_frame(3, 10);
    lit_in.char_fg = vec![charred];
    lit_in.glow_under = vec![body];
    let f = rend.render_input(&lit_in);

    let pad = (f.width - 10 * cw) / 2;
    let y_mid = pad + ch + ch / 2;
    let stroke = f.pixels[y_mid * f.width + pad + 2 * cw + cw / 2];
    let beside = f.pixels[y_mid * f.width + pad + 4 * cw + cw / 2];
    assert!(
        luma(stroke) < luma(beside),
        "SILHOUETTE LAW: the charred stroke (luma {}) must be darker than the \
         lit background beside it (luma {})",
        luma(stroke),
        luma(beside)
    );
    // Anti-vacuity: without char_fg the glyph would NOT be a dark core.
    let mut plain_in = term.cell_frame(3, 10);
    plain_in.glow_under = vec![body];
    let plain = rend.render_input(&plain_in);
    let plain_stroke = plain.pixels[y_mid * f.width + pad + 2 * cw + cw / 2];
    assert!(
        luma(stroke) < luma(plain_stroke),
        "char_fg must actually char the stroke (charred {} vs plain {})",
        luma(stroke),
        luma(plain_stroke)
    );
    // Additive laws: brighten-only over the pre-light frame, and row-band
    // containment (rows 0 and 2 are untouched).
    for (i, (&d, &p)) in dark.iter().zip(f.pixels.iter()).enumerate() {
        let y = i / f.width;
        if !(pad + ch..pad + 2 * ch).contains(&y) {
            assert_eq!(d, p, "pixel outside the body's row band changed at y={y}");
        }
        for sh in [16, 8, 0] {
            assert!(
                (p >> sh) & 0xff >= (d >> sh) & 0xff,
                "under-glyph light must only brighten the pre-light frame"
            );
        }
    }
}

/// DIRTY GATE: settled streams (equal, non-empty) gate-hit with nothing
/// marked; a moved flame-body quad sets `glow_under_changed` and marks exactly
/// its prev∪cur rows; a char_fg change sets `char_fg_changed` and marks its
/// rows — the ink/aurora prev∪cur discipline.
#[test]
fn glow_under_and_char_fg_dirty_gate_marks_prev_and_cur_rows() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set

    let mut frame = |quads: &[GlowQuad], chars: &[CharFg]| {
        let mut input = term.cell_frame(6, 8);
        input.glow_under = quads.to_vec();
        input.char_fg = chars.to_vec();
        input
    };
    let marked = |dirty: &[bool]| -> Vec<usize> {
        dirty
            .iter()
            .enumerate()
            .filter_map(|(r, &b)| b.then_some(r))
            .collect()
    };

    let settled_q = [under(1, 0, 16, 8, 16, 0x0010_0803)];
    let settled_c = [CharFg {
        row: 1,
        col: 2,
        fg: 0x0010_0804,
    }];
    let prev = frame(&settled_q, &settled_c);
    let cur = frame(&settled_q, &settled_c);
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.glow_under_changed && !d.char_fg_changed,
        "settled streams must not set the changed flags"
    );
    assert!(d.is_gate_hit(), "settled streams must gate-hit");
    assert!(dirty.iter().all(|&b| !b), "settled streams mark no rows");

    // The quad steps row 1 → row 4: prev AND cur rows must repaint (vacated
    // light rebuilt fresh, new light landed), nothing else.
    let cur = frame(&[under(4, 0, 64, 8, 16, 0x0010_0803)], &settled_c);
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.glow_under_changed,
        "a moved glow_under quad must set glow_under_changed"
    );
    assert!(!d.is_gate_hit(), "a changed glow_under must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 4],
        "a moved quad must mark exactly its prev∪cur rows"
    );

    // The char sweep moves to row 3 (its colour also changes): prev row 1 and
    // cur row 3 must repaint, glow_under (settled) marks nothing.
    let cur = frame(
        &settled_q,
        &[CharFg {
            row: 3,
            col: 5,
            fg: 0x0008_0402,
        }],
    );
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.char_fg_changed,
        "a changed char_fg must set char_fg_changed"
    );
    assert!(
        !d.glow_under_changed,
        "the settled glow_under must stay unchanged"
    );
    assert!(!d.is_gate_hit(), "a changed char_fg must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 3],
        "a moved char_fg must mark exactly its prev∪cur rows"
    );
}

/// NO-GHOSTING through the persistent damage cache: a flame body animating
/// across rows while the charring sweeps (and finally both draining) must
/// leave the cached-damaged framebuffer equal to a fresh full render after
/// every step — byte-for-byte. The final drained frame doubles as the
/// gate-hit check on the steady state.
#[test]
fn damaged_path_no_ghosting_as_glow_under_and_char_fg_animate() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let rows = 6usize;
    let mut term = Terminal::new(rows as u16, 12);
    // Text on rows 1 and 3 so the charring recolours real glyphs.
    term.process(b"\x1b[?25l\x1b[2;1Hburning line\x1b[4;1Hburning line");
    let (cw16, ch16) = (cw as u16, ch.min(48) as u16);

    let mut make = |quads: Vec<GlowQuad>, chars: Vec<CharFg>| {
        let mut input = term.cell_frame(rows, 12);
        input.glow_under = quads;
        input.char_fg = chars;
        input
    };
    let char_at = |row: u16, col: u16, fg: u32| CharFg { row, col, fg };
    // A: fire on row 1 (chars 0..3 charred); B: it climbs to row 3 (row 1
    // vacated — its glyphs must restore); C: drained.
    let in_a = make(
        vec![under(1, 0, ch16, 8 * cw16, ch16, 0x0050_2808)],
        vec![
            char_at(1, 0, 0x0012_0904),
            char_at(1, 1, 0x0010_0804),
            char_at(1, 2, 0x000e_0703),
        ],
    );
    let in_b = make(
        vec![under(3, 2 * cw16, 3 * ch16, 8 * cw16, ch16, 0x0050_2808)],
        vec![char_at(3, 2, 0x0010_0804), char_at(3, 3, 0x000e_0703)],
    );
    let in_c = make(Vec::new(), Vec::new());

    let mut wc = WindowCpu::new();
    for (name, input) in [("A", &in_a), ("B", &in_b), ("C", &in_c)] {
        let cached = rend.render_input_cached(&mut wc, input).pixels().to_vec();
        let fresh = rend.render_input(input).pixels.clone();
        assert_eq!(
            cached, fresh,
            "cached-damaged frame {name} must equal a fresh full render \
             (no light ghost, no stale charred glyph at vacated rows)"
        );
    }
    // Steady state: a repeat of the drained frame is a gate hit (zero work).
    let again = rend.render_input_cached(&mut wc, &in_c).pixels().to_vec();
    assert_eq!(
        wc.last_damage(),
        DamageOutcome::GateHit,
        "an unchanged drained frame must dirty-gate"
    );
    assert_eq!(
        again,
        rend.render_input(&in_c).pixels,
        "a gate-hit frame must be byte-stable"
    );
}
