// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::render::{HaloMode, RainHalo};
use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, DirtyDecision, Renderer, Theme, WindowCpu, compute_dirty_rows};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
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
        mode: HaloMode::Add,
    }
}

fn luma(p: u32) -> u32 {
    ((p >> 16) & 0xff) + ((p >> 8) & 0xff) + (p & 0xff)
}

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
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.glow_halo_changed,
        "settled halos must not set glow_halo_changed"
    );
    assert!(d.is_gate_hit(), "settled glow_halo must gate-hit");
    assert!(dirty.iter().all(|&b| !b), "settled halos must mark no rows");

    let cur = frame(&[halo(4, 0, 64, 8, 16, 0x0010_2010)]);
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    ) else {
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

fn light_theme() -> Theme {
    Theme {
        fg: 0x0020_2020,
        bg: 0x00FF_FFFF,
        ..Theme::default()
    }
}

fn falloff_weight(q: &RainHalo, pad: usize, x: usize, y: usize) -> u8 {
    let dx = x as i32 - (pad as i32 + q.cx as i32);
    let dy = y as i32 - (pad as i32 + q.cy as i32);
    let nsq = (dx * dx * 256) / ((q.rx as i32) * (q.rx as i32))
        + (dy * dy * 256) / ((q.ry as i32) * (q.ry as i32));
    let wt = (256 - nsq).max(0);
    ((wt * wt) / 256).min(255) as u8
}

#[test]
fn over_halo_darkens_white_background_where_add_is_invisible() {
    let mut rend = Renderer::from_system(18.0, light_theme())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25l"); // glyph-free: the veil's own gradient, unperturbed
    let base = rend.render_input(&term.cell_frame(3, 10)).pixels.clone();

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
    let mut prev = luma(centre);
    for dx in 1..q.rx as usize {
        let cur = luma(f.pixels[cy * f.width + cx + dx]);
        assert!(
            cur >= prev,
            "veil opacity must fall off radially (x+{dx}: {cur} < {prev})"
        );
        prev = cur;
    }
    for (i, (&b, &p)) in base.iter().zip(f.pixels.iter()).enumerate() {
        let y = i / f.width;
        if !(pad + ch..pad + 2 * ch).contains(&y) {
            assert_eq!(b, p, "pixel outside the veil's row band changed at y={y}");
        }
    }

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
