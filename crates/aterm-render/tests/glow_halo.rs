// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GLOW-HALO cursor-effect radial light on the CPU renderer
// (`RenderInput.glow_halo`: the EMBERFORGE round embers / crown stream). The
// contract under test:
//   * an empty `glow_halo` (never touched, explicitly emptied, and
//     pushed-then-cleared) is byte-identical to the pre-halo path, also after
//     `clear_overlays` (the `image plain` contract) — the no-op law;
//   * a halo renders with RADIALLY DECREASING luminance: the centre pixel is
//     the brightest, samples toward the ellipse edge monotonically dim, and
//     the light only ever brightens the base (premultiplied `add_sat`);
//   * dirty gate: a settled stream (equal, non-empty) gate-hits; a moved halo
//     sets `glow_halo_changed` and marks exactly its prev∪cur rows;
//   * damaged path: an animating halo re-renders with no ghosting
//     (cached == fresh, byte-for-byte) — the aurora discipline;
//   * `HaloMode::Over` (EMBERFORGE P7, the light-theme veil): an Over halo
//     DARKENS a white background at its centre where the same Add halo is
//     INVISIBLE (the smoke-on-light-theme law); an Over halo with no falloff
//     coverage is a byte no-op; an Add-only stream lands byte-for-byte on the
//     historical `add_sat(premul_rgb(..))` math (the no-regression law); and
//     a mixed stream composites every Add quad BEFORE every Over quad (the
//     GPU's per-mode split order — `over_rgb(add_sat(..))`).

use aterm_core::render::{HaloMode, RainHalo};
use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, DirtyDecision, Renderer, Theme, WindowCpu, compute_dirty_rows};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

/// A single-row-band halo whose falloff centre is the quad centre — the
/// simplest legal emission (the producer splits row-spanning halos into
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

/// The luminance proxy used by the radial samples: the summed RGB channels
/// (monotone in every channel under additive light, so ordering is exact).
fn luma(p: u32) -> u32 {
    ((p >> 16) & 0xff) + ((p >> 8) & 0xff) + (p & 0xff)
}

/// NO-OP LAW: empty `glow_halo` — untouched, explicitly emptied, and
/// pushed-then-cleared — must be byte-identical to the pre-halo path, and
/// `clear_overlays` strips a populated stream back to the bare frame.
#[test]
fn empty_glow_halo_is_byte_identical_to_before() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25lember forge");

    // Pre-feature frame: the snapshot as built, glow_halo never mentioned.
    let base = rend.render_input(&term.cell_frame(3, 12)).pixels.clone();

    // Explicit empty, and pushed-then-cleared (the emptied-stream path).
    let mut input = term.cell_frame(3, 12);
    input.glow_halo = Vec::new();
    let explicit = rend.render_input(&input).pixels.clone();
    assert_eq!(base, explicit, "explicit empty glow_halo must be a no-op");
    input
        .glow_halo
        .push(halo(1, 0, ch as u16, cw as u16, ch as u16, 0x0040_8040));
    input.glow_halo.clear();
    let emptied = rend.render_input(&input).pixels.clone();
    assert_eq!(base, emptied, "an emptied glow_halo must leave no residue");

    // A populated stream paints; `clear_overlays` restores the bare frame.
    let mut with_halo = term.cell_frame(3, 12);
    with_halo.glow_halo = vec![halo(
        1,
        0,
        ch as u16,
        (2 * cw) as u16,
        ch as u16,
        0x0040_8040,
    )];
    let painted = rend.render_input(&with_halo).pixels.clone();
    assert_ne!(base, painted, "a non-empty glow_halo must paint something");
    with_halo.clear_overlays();
    assert!(
        with_halo.glow_halo.is_empty(),
        "clear_overlays must strip glow_halo (it IS bling)"
    );
    let stripped = rend.render_input(&with_halo).pixels.clone();
    assert_eq!(base, stripped, "clear_overlays must restore the bare frame");
}

/// RADIAL LAW: the halo's luminance decreases monotonically from the centre
/// toward the ellipse edge (sampled along the centre scanline), and additive
/// light only ever brightens the base.
#[test]
fn glow_halo_luminance_decreases_radially_from_centre() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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

/// DIRTY GATE: settled halos (equal, non-empty) gate-hit with nothing marked;
/// a moved halo sets `glow_halo_changed` and marks exactly its prev∪cur rows
/// — the aurora's prev∪cur discipline, radial edition.
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

    // The halo steps row 1 → row 4: prev AND cur rows must repaint (vacated
    // light rebuilt fresh, new light landed), nothing else.
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

/// NO-GHOSTING through the persistent damage cache: a halo animating across
/// rows (and finally draining) must leave the cached-damaged framebuffer equal
/// to a fresh full render after every step — byte-for-byte. The final drained
/// frame doubles as the gate-hit check on the steady state.
#[test]
fn damaged_path_no_ghosting_as_glow_halo_animates() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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

/// A calm LIGHT theme: white background, near-black ink — the frame every
/// additive (brighten-only) effect is invisible on, i.e. the frame
/// `HaloMode::Over` exists for.
fn light_theme() -> Theme {
    Theme {
        fg: 0x0020_2020,
        bg: 0x00FF_FFFF,
        ..Theme::default()
    }
}

/// The `RainHalo` integer falloff weight, restated independently of the
/// renderer (window-pixel coords): `nsq = (dx²·256)/rx² + (dy²·256)/ry²`,
/// `wt = clamp(((256 − nsq)²) / 256, 0, 255)`.
fn falloff_weight(q: &RainHalo, pad: usize, x: usize, y: usize) -> u8 {
    let dx = x as i32 - (pad as i32 + q.cx as i32);
    let dy = y as i32 - (pad as i32 + q.cy as i32);
    let nsq = (dx * dx * 256) / ((q.rx as i32) * (q.rx as i32))
        + (dy * dy * 256) / ((q.ry as i32) * (q.ry as i32));
    let wt = (256 - nsq).max(0);
    ((wt * wt) / 256).min(255) as u8
}

/// THE SMOKE-ON-LIGHT-THEME LAW: over a WHITE background an `Over` veil
/// DARKENS the frame — most at its centre (which lands the veil colour
/// EXACTLY: the falloff weight is 255 there, and `over_rgb` at full coverage
/// is the source colour), monotonically recovering toward white along the
/// scanline — while the SAME halo as `Add` is byte-invisible (you cannot
/// brighten white into smoke; `add_sat` saturates). Nothing outside the
/// veil's row band changes.
#[test]
fn over_halo_darkens_white_background_where_add_is_invisible() {
    let Some(mut rend) = Renderer::from_system(18.0, light_theme()) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    // Monotone recovery toward white along the centre scanline: each step
    // outward is no darker than the last, and never darker than the centre.
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

    // The FOIL: the same halo as Add-mode light is INVISIBLE on white —
    // `add_sat` cannot leave 0xFFFFFF — which is precisely why Over exists.
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

/// NO-COVERAGE NO-OP LAW: an `Over` quad whose rect lies entirely at (or
/// beyond) the falloff ellipse edge — weight 0 on every covered pixel —
/// leaves the frame byte-identical (the GPU twin blends `dst·(1−0)` there,
/// also a no-op).
#[test]
fn over_halo_with_zero_coverage_is_a_noop() {
    let Some(mut rend) = Renderer::from_system(18.0, light_theme()) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25lzero coverage");
    let base = rend.render_input(&term.cell_frame(3, 10)).pixels.clone();

    // Centre at the band's left edge with rx = cw; the quad starts at
    // x = cx + rx, so every covered pixel has dx >= rx -> nsq >= 256 -> wt 0.
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

/// THE NO-REGRESSION LAW: `HaloMode` defaults to `Add`, and an Add-only
/// stream lands byte-for-byte on the HISTORICAL math — every covered pixel is
/// exactly `add_sat(base, premul_rgb(color, wt))` with the integer falloff
/// weight restated independently here — so frames that never mention the new
/// field are byte-identical to the pre-`HaloMode` renderer.
#[test]
fn add_only_stream_matches_the_historical_math_byte_for_byte() {
    assert_eq!(
        HaloMode::default(),
        HaloMode::Add,
        "legacy streams stay Add"
    );
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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

/// MODE-SWEEP ORDER LAW: within one stream every Add quad composites BEFORE
/// every Over quad — REGARDLESS of emission order — matching the GPU's
/// per-mode split draws (the veil dims the ember, never vice versa). Pinned
/// with an Over veil emitted FIRST but sharing its geometry with an Add
/// ember: every covered pixel must equal `over_rgb(add_sat(base, ..), ..)`.
#[test]
fn mixed_stream_composites_add_before_over() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
