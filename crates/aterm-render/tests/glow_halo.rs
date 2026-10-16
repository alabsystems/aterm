// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Cursor-effect radial light on the CPU renderer (`RenderInput.glow_halo`, the Emberforge
// embers and crown). The empty-stream no-op is the `glow_halo` row of
// `tests/empty_channels.rs`. Pinned here: radial falloff that only brightens; the dirty gate
// (settled gate-hits, moved marks prev∪cur rows); no ghosting on the damaged path; and
// `HaloMode::Over`, the light-theme veil: it darkens white where `Add` is invisible, is a
// no-op without coverage, and composites after every Add quad (the GPU's per-mode order).

use aterm_core::render::{HaloMode, RainHalo};
use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, DirtyDecision, Renderer, Theme, WindowCpu, compute_dirty_rows};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// A single-row-band halo centred on its quad (the producer splits row-spanning halos into
/// per-row quads sharing one centre).
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
        mode: HaloMode::Add,
    }
}

/// Summed-RGB luminance proxy (monotone in every channel under additive light).
fn luma(p: u32) -> u32 {
    ((p >> 16) & 0xff) + ((p >> 8) & 0xff) + (p & 0xff)
}

/// Luminance decreases monotonically from the centre toward the ellipse edge along the
/// centre scanline, and additive light only brightens the base.
#[test]
fn glow_halo_luminance_decreases_radially_from_centre() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25l"); // glyph-free: the halo's own gradient, unperturbed
    let base = rend.render_input(&term.cell_frame(3, 10)).pixels.clone();

    // One bright halo filling row 1's band, centre mid-band.
    let q = halo(
        1,
        cw as u16,
        ch as u16,
        (4 * cw) as u16,
        ch as u16,
        0x00C0_FFC0,
    );
    let mut input = term.cell_frame(3, 10);
    input.glow_halo = vec![q];
    let f = rend.render_input(&input);

    let pad = (f.width - 10 * cw) / 2;
    let (cx, cy) = (pad + q.cx as usize, pad + q.cy as usize);
    let centre = luma(f.pixels[cy * f.width + cx]);
    let edge = luma(f.pixels[cy * f.width + cx + (q.rx as usize - 1)]);
    let bg = luma(base[cy * f.width + cx]);
    assert!(
        centre > edge,
        "halo centre (luma {centre}) must outshine its edge (luma {edge})"
    );
    assert!(
        edge >= bg && centre > bg,
        "the halo must brighten over the base (bg {bg}, edge {edge}, centre {centre})"
    );
    // Monotone along the centre scanline: each step outward never brightens.
    let mut prev = centre;
    for dx in 1..q.rx as usize {
        let cur = luma(f.pixels[cy * f.width + cx + dx]);
        assert!(
            cur <= prev,
            "radial falloff must be monotone (x+{dx}: {cur} > {prev})"
        );
        prev = cur;
    }
    // And nothing outside the quad's row band was touched.
    for (i, (&b, &p)) in base.iter().zip(f.pixels.iter()).enumerate() {
        let y = i / f.width;
        if !(pad + ch..pad + 2 * ch).contains(&y) {
            assert_eq!(b, p, "pixel outside the halo's row band changed at y={y}");
        }
        for sh in [16, 8, 0] {
            assert!(
                (p >> sh) & 0xff >= (b >> sh) & 0xff,
                "additive light must only brighten"
            );
        }
    }
}

/// Settled halos (equal, non-empty) gate-hit with nothing marked; a moved halo sets
/// `glow_halo_changed` and marks exactly its prev∪cur rows.
#[test]
fn glow_halo_dirty_gate_marks_prev_and_cur_rows() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set

    let mut frame = |halos: &[RainHalo]| {
        let mut input = term.cell_frame(6, 8);
        input.glow_halo = halos.to_vec();
        input
    };
    let marked = |dirty: &[bool]| -> Vec<usize> {
        dirty
            .iter()
            .enumerate()
            .filter_map(|(r, &b)| b.then_some(r))
            .collect()
    };

    let settled = [halo(1, 0, 16, 8, 16, 0x0010_2010)];
    let prev = frame(&settled);
    let cur = frame(&settled);
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.glow_halo_changed,
        "settled halos must not set glow_halo_changed"
    );
    assert!(d.is_gate_hit(), "settled glow_halo must gate-hit");
    assert!(dirty.iter().all(|&b| !b), "settled halos must mark no rows");

    // The halo steps row 1 → row 4: prev and cur rows repaint, nothing else.
    let cur = frame(&[halo(4, 0, 64, 8, 16, 0x0010_2010)]);
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.glow_halo_changed,
        "a moved halo must set glow_halo_changed"
    );
    assert!(!d.is_gate_hit(), "a changed glow_halo must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 4],
        "a moved halo must mark exactly its prev∪cur rows"
    );
}

/// A halo animating across rows and then draining leaves the cached damaged framebuffer
/// equal to a fresh full render after every step; a repeat of the drained frame gate-hits.
#[test]
fn damaged_path_no_ghosting_as_glow_halo_animates() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let rows = 6usize;
    let mut term = Terminal::new(rows as u16, 12);
    term.process(b"\x1b[?25l");
    let (cw16, ch16) = (cw as u16, ch.min(48) as u16);

    let mut make = |halos: Vec<RainHalo>| {
        let mut input = term.cell_frame(rows, 12);
        input.glow_halo = halos;
        input
    };
    // A: an ember on row 1; B: it drifts to row 3 (vacating row 1); C: drained.
    let in_a = make(vec![halo(1, 2 * cw16, ch16, 2 * cw16, ch16, 0x0060_3010)]);
    let in_b = make(vec![halo(
        3,
        4 * cw16,
        3 * ch16,
        2 * cw16,
        ch16,
        0x0060_3010,
    )]);
    let in_c = make(Vec::new());

    let mut wc = WindowCpu::new();
    for (name, input) in [("A", &in_a), ("B", &in_b), ("C", &in_c)] {
        let cached = rend.render_input_cached(&mut wc, input).pixels().to_vec();
        let fresh = rend.render_input(input).pixels.clone();
        assert_eq!(
            cached, fresh,
            "cached-damaged frame {name} must equal a fresh full render \
             (no ghost at vacated rows, no missing halo at new rows)"
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

/// A light theme (white background, near-black ink): additive light is invisible on it, which
/// is what `HaloMode::Over` is for.
fn light_theme() -> Theme {
    Theme {
        fg: 0x0020_2020,
        bg: 0x00FF_FFFF,
        ..Theme::default()
    }
}

/// The `RainHalo` integer falloff weight, restated independently of the renderer
/// (window-pixel coords): `nsq = (dx²·256)/rx² + (dy²·256)/ry²`,
/// `wt = clamp(((256 − nsq)²) / 256, 0, 255)`.
fn falloff_weight(q: &RainHalo, pad: usize, x: usize, y: usize) -> u8 {
    let dx = x as i32 - (pad as i32 + q.cx as i32);
    let dy = y as i32 - (pad as i32 + q.cy as i32);
    let nsq = (dx * dx * 256) / ((q.rx as i32) * (q.rx as i32))
        + (dy * dy * 256) / ((q.ry as i32) * (q.ry as i32));
    let wt = (256 - nsq).max(0);
    ((wt * wt) / 256).min(255) as u8
}

/// Over a white background an `Over` veil darkens the frame: at its centre it lands the veil
/// colour exactly (weight 255, and `over_rgb` at full coverage is the source), recovering
/// monotonically toward white, while the same halo as `Add` is byte-invisible (`add_sat`
/// saturates). Nothing outside the veil's row band changes.
#[test]
fn over_halo_darkens_white_background_where_add_is_invisible() {
    let mut rend = Renderer::from_system(18.0, light_theme())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25l"); // glyph-free: the veil's own gradient, unperturbed
    let base = rend.render_input(&term.cell_frame(3, 10)).pixels.clone();

    // One grey-smoke veil filling row 1's band, centre mid-band.
    let smoke = 0x0020_2020;
    let q = RainHalo {
        mode: HaloMode::Over,
        ..halo(1, cw as u16, ch as u16, (4 * cw) as u16, ch as u16, smoke)
    };
    let mut input = term.cell_frame(3, 10);
    input.glow_halo = vec![q];
    let f = rend.render_input(&input);

    let pad = (f.width - 10 * cw) / 2;
    let (cx, cy) = (pad + q.cx as usize, pad + q.cy as usize);
    let centre = f.pixels[cy * f.width + cx];
    assert_eq!(
        centre, smoke,
        "the veil centre is fully opaque: over_rgb(white, smoke, 255) == smoke"
    );
    assert!(
        luma(centre) < luma(base[cy * f.width + cx]),
        "an Over veil must DARKEN a white background at its centre"
    );
    // Monotone recovery toward white along the centre scanline.
    let mut prev = luma(centre);
    for dx in 1..q.rx as usize {
        let cur = luma(f.pixels[cy * f.width + cx + dx]);
        assert!(
            cur >= prev,
            "veil opacity must fall off radially (x+{dx}: {cur} < {prev})"
        );
        prev = cur;
    }
    // Row-band containment, exactly like the Add stream.
    for (i, (&b, &p)) in base.iter().zip(f.pixels.iter()).enumerate() {
        let y = i / f.width;
        if !(pad + ch..pad + 2 * ch).contains(&y) {
            assert_eq!(b, p, "pixel outside the veil's row band changed at y={y}");
        }
    }

    // Foil: the same halo as Add is invisible on white, since `add_sat` cannot leave
    // 0xFFFFFF.
    let mut add_input = term.cell_frame(3, 10);
    add_input.glow_halo = vec![RainHalo {
        mode: HaloMode::Add,
        ..q
    }];
    assert_eq!(
        rend.render_input(&add_input).pixels,
        base,
        "additive smoke over a white background must be byte-invisible"
    );
}

/// An `Over` quad lying entirely at or beyond the falloff ellipse edge (weight 0 on every
/// covered pixel) leaves the frame byte-identical; the GPU blends `dst·(1−0)` there.
#[test]
fn over_halo_with_zero_coverage_is_a_noop() {
    let mut rend = Renderer::from_system(18.0, light_theme())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25lzero coverage");
    let base = rend.render_input(&term.cell_frame(3, 10)).pixels.clone();

    // Centre at the band's left edge with rx = cw; the quad starts at x = cx + rx, so every
    // covered pixel has dx >= rx -> nsq >= 256 -> wt 0.
    let q = RainHalo {
        row: 1,
        x: (2 * cw) as u16,
        y: ch as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: 0x0020_2020,
        cx: cw as u16,
        cy: (ch + ch / 2) as u16,
        rx: cw as u16,
        ry: (ch / 2).max(1) as u16,
        mode: HaloMode::Over,
    };
    let mut input = term.cell_frame(3, 10);
    input.glow_halo = vec![q];
    assert_eq!(
        rend.render_input(&input).pixels,
        base,
        "a zero-coverage Over veil must be a byte no-op"
    );
}

/// `HaloMode` defaults to `Add`, and an Add-only stream is exactly
/// `add_sat(base, premul_rgb(color, wt))` per covered pixel, with the falloff weight
/// restated independently here.
#[test]
fn add_only_stream_matches_the_historical_math_byte_for_byte() {
    assert_eq!(
        HaloMode::default(),
        HaloMode::Add,
        "legacy streams stay Add"
    );
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25l"); // glyph-free: bg-only base, exact expectations
    let base = rend.render_input(&term.cell_frame(3, 10)).pixels.clone();

    let q = halo(
        1,
        cw as u16,
        ch as u16,
        (4 * cw) as u16,
        ch as u16,
        0x0060_3010,
    );
    let mut input = term.cell_frame(3, 10);
    input.glow_halo = vec![q];
    let f = rend.render_input(&input);
    let pad = (f.width - 10 * cw) / 2;
    for (i, (&b, &p)) in base.iter().zip(f.pixels.iter()).enumerate() {
        let (x, y) = (i % f.width, i / f.width);
        let covered = (pad + q.x as usize..pad + (q.x + q.w) as usize).contains(&x)
            && (pad + q.y as usize..pad + (q.y + q.h) as usize).contains(&y);
        let expect = if covered {
            aterm_render::add_sat(
                b,
                aterm_render::premul_rgb(q.color, falloff_weight(&q, pad, x, y)),
            )
        } else {
            b
        };
        assert_eq!(
            p, expect,
            "Add-mode pixel at ({x},{y}) must equal the historical add_sat math"
        );
    }
}

/// Within one stream every Add quad composites before every Over quad, whatever the
/// emission order, matching the GPU's per-mode split draws: with an Over veil emitted first
/// over an Add ember's geometry, every covered pixel is `over_rgb(add_sat(base, ..), ..)`.
#[test]
fn mixed_stream_composites_add_before_over() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25l");
    let base = rend.render_input(&term.cell_frame(3, 10)).pixels.clone();

    let ember = halo(
        1,
        cw as u16,
        ch as u16,
        (4 * cw) as u16,
        ch as u16,
        0x00FF_6018,
    );
    let veil = RainHalo {
        color: 0x0018_1820,
        mode: HaloMode::Over,
        ..ember
    };
    let mut input = term.cell_frame(3, 10);
    input.glow_halo = vec![veil, ember]; // veil EMITTED first; must draw last
    let f = rend.render_input(&input);
    let pad = (f.width - 10 * cw) / 2;
    let mut veiled = 0usize;
    for (i, (&b, &p)) in base.iter().zip(f.pixels.iter()).enumerate() {
        let (x, y) = (i % f.width, i / f.width);
        let covered = (pad + ember.x as usize..pad + (ember.x + ember.w) as usize).contains(&x)
            && (pad + ember.y as usize..pad + (ember.y + ember.h) as usize).contains(&y);
        let expect = if covered {
            let wt = falloff_weight(&ember, pad, x, y);
            let lit = aterm_render::add_sat(b, aterm_render::premul_rgb(ember.color, wt));
            if wt == 0 {
                lit
            } else {
                veiled += 1;
                aterm_render::over_rgb(lit, veil.color, wt)
            }
        } else {
            b
        };
        assert_eq!(
            p, expect,
            "mixed-mode pixel at ({x},{y}) must be over_rgb(add_sat(base)) — Add then Over"
        );
    }
    assert!(veiled > 0, "the veil must actually cover lit pixels");
}
