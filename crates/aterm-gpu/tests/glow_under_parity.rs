// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::render::{CharFg, GlowBlend, GlowQuad};
use aterm_core::terminal::Terminal;
use aterm_render::{BeamClip, RibbonVertex, Theme, WindowCpu};

use crate::common::ByteExact;
use crate::common::{assert_byte_exact, backends, bb, block_text_term, gg, max_channel_delta, rr};

fn luma(p: u32) -> i32 {
    rr(p) + gg(p) + bb(p)
}

/// One flame-body quad clamped to the grid and its single row band, or `None`
/// when fully clipped.
fn emit_under(
    row: usize,
    x0: i64,
    x1: i64,
    ch: usize,
    grid_w: usize,
    color: u32,
) -> Option<GlowQuad> {
    let cx0 = x0.max(0).min(grid_w as i64);
    let cx1 = x1.max(0).min(grid_w as i64);
    if cx0 >= cx1 {
        return None;
    }
    Some(GlowQuad {
        row: row as u16,
        x: cx0 as u16,
        y: (row * ch) as u16,
        w: (cx1 - cx0) as u16,
        h: ch as u16,
        color,
        alpha: 0,
        color2: color,
        alpha2: 0,
    })
}

/// glow_under and char_fg parity over procedural full-block glyphs (a delta-0
/// base): char_fg alone is byte-exact on any adapter; the combined frame is
/// byte-exact where the additive contract holds; and the charred core is the
/// darkest thing in the fire on both backends.
#[test]
fn glow_under_char_fg_field_is_byte_exact_over_text() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = block_text_term(rows, cols, 1..8);
    let (cw, ch) = cpu.cell_size();
    let grid_w = cols * cw;

    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the stream deltas are effect-only"
    );

    let chars: Vec<CharFg> = vec![
        CharFg {
            row: 2,
            col: 3,
            fg: 0x0010_0804,
        },
        CharFg {
            row: 2,
            col: 4,
            fg: 0x0014_0a05,
        },
        CharFg {
            row: 3,
            col: 10,
            fg: 0x000c_0603,
        },
        CharFg {
            row: 5,
            col: 0,
            fg: 0x0018_0c06,
        },
    ];
    let mut char_input = term.cell_frame(rows, cols);
    char_input.char_fg = chars.clone();
    let cpu_c = cpu.render_input(&char_input);
    let gpu_c = gpu.render_input(&mut win, &char_input, None);
    assert_ne!(
        cpu_c.pixels, cpu_base.pixels,
        "char_fg must actually char glyphs (non-vacuous)"
    );
    assert_eq!(
        max_channel_delta(&cpu_c.pixels, &gpu_c.pixels),
        0,
        "char_fg alone is a plain fg substitution and must be byte-exact CPU==GPU"
    );

    let mut quads = Vec::new();
    for (r, x0, x1, color) in [
        (2usize, -20i64, (8 * cw) as i64, 0x0080_4010u32),
        (3, (6 * cw) as i64, (16 * cw) as i64, 0x0060_3018),
        (4, (2 * cw) as i64, (12 * cw) as i64, 0x0044_5522),
        (5, -4, (5 * cw) as i64, 0x0090_5008),
        (7, (36 * cw) as i64, grid_w as i64 + 60, 0x0030_60c0),
    ] {
        quads.extend(emit_under(r, x0, x1, ch, grid_w, color));
    }
    assert!(
        quads.len() >= 5,
        "the synthetic field must be multi-quad, multi-row"
    );
    let mut input = term.cell_frame(rows, cols);
    input.glow_under = quads;
    input.char_fg = chars;
    let cpu_f = cpu.render_input(&input);
    let gpu_f = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_f.pixels, cpu_c.pixels,
        "the flame body must actually paint (non-vacuous)"
    );
    let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
    eprintln!(
        "glow_under+char_fg engulfed-frame GPU vs CPU max per-channel delta = {delta} ({} quads)",
        input.glow_under.len()
    );
    if exact.holds(&gpu, "byte-exact glow_under gate") {
        assert_eq!(
            delta, 0,
            "glow_under under the glyph ink must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    }

    for (name, f) in [("CPU", &cpu_f), ("GPU", &gpu_f)] {
        let pad = (f.width - cols * cw) / 2;
        let y_mid = pad + 2 * ch + ch / 2;
        let stroke = f.pixels[y_mid * f.width + pad + 3 * cw + cw / 2];
        let beside = f.pixels[y_mid * f.width + pad + 6 * cw + cw / 2];
        assert!(
            luma(stroke) < luma(beside),
            "{name}: charred stroke (luma {}) must be darker than the lit \
             glyph beside it (luma {}) — the dark-core silhouette",
            luma(stroke),
            luma(beside)
        );
    }
}

/// Cached-path parity via `render_input_cached`: frame A engulfs row 1, frame
/// B moves the fire to row 4, which must miss the GPU dirty gate, repaint the
/// previous and current rows, and match on both backends.
#[test]
fn damaged_path_glow_under_char_fg_parity_cpu_matches_gpu() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());
    term.process(format!("\x1b[2;1H{}", "█".repeat(12)).as_bytes());
    term.process(format!("\x1b[5;1H{}", "█".repeat(12)).as_bytes());
    let (cw, ch) = cpu.cell_size();

    let fire_at = |row: u16| -> (GlowQuad, Vec<CharFg>) {
        (
            GlowQuad {
                row,
                x: (2 * cw) as u16,
                y: (row as usize * ch) as u16,
                w: (8 * cw) as u16,
                h: ch as u16,
                color: 0x0070_3810,
                alpha: 0,
                color2: 0x0070_3810,
                alpha2: 0,
            },
            vec![
                CharFg {
                    row,
                    col: 3,
                    fg: 0x0010_0804,
                },
                CharFg {
                    row,
                    col: 4,
                    fg: 0x000e_0703,
                },
            ],
        )
    };

    let mut in_a = term.cell_frame(rows, cols);
    let (qa, ca) = fire_at(1);
    in_a.glow_under.push(qa);
    in_a.char_fg = ca;
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    let mut in_b = term.cell_frame(rows, cols);
    let (qb, cb) = fire_at(4);
    in_b.glow_under.push(qb);
    in_b.char_fg = cb;
    let misses_before = gpu.gate_misses();
    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_b = gpu
        .render_input_cached(&mut win_gpu, &in_b)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "a moved glow_under/char_fg must MISS the GPU dirty gate (real re-render)"
    );

    let cpu_fresh = cpu.render_input(&in_b).pixels.clone();
    assert_eq!(
        cpu_b, cpu_fresh,
        "CPU cached-damaged engulfed frame must equal a fresh full render"
    );
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path glow_under+char_fg CPU vs GPU max per-channel delta = {delta}");
    if exact.holds(&gpu, "damaged-path byte-exact glow_under gate") {
        assert_eq!(
            delta, 0,
            "engulfed frame via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    }
}

/// char_fg reaches line decorations on both backends. The substituted colour
/// is derived at three sites (CPU pass 3, CPU pass 3b undercurl, the GPU deco
/// loop).
#[test]
fn char_fg_follows_into_line_decorations_on_both_backends() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (2usize, 12usize);
    let charred_fg = 0x007C_C8FFu32;

    let mut term_a = Terminal::new(rows as u16, cols as u16);
    term_a.process(
        "\x1b[?25l\x1b[4mx\x1b[24m \x1b[4:3mw\x1b[24m \x1b[9ms\x1b[29m \
\x1b[53mo\x1b[55m\r\n\x1b[4m\x1b[58;2;10;20;30m \x1b[59m\x1b[24m \x1b[4m \x1b[24m \
\x1b[4:3m \x1b[24m"
            .as_bytes(),
    );
    let mut charred_in = term_a.cell_frame(rows, cols);
    charred_in.char_fg = [(0u16, 0u16), (0, 2), (0, 4), (0, 6), (1, 0), (1, 2), (1, 4)]
        .into_iter()
        .map(|(row, col)| CharFg {
            row,
            col,
            fg: charred_fg,
        })
        .collect();

    let mut term_b = Terminal::new(rows as u16, cols as u16);
    term_b.process(
        "\x1b[?25l\x1b[38;2;124;200;255m\x1b[4mx\x1b[24m\x1b[39m \x1b[38;2;124;200;255m\
\x1b[4:3mw\x1b[24m\x1b[39m \x1b[38;2;124;200;255m\x1b[9ms\x1b[29m\x1b[39m \
\x1b[38;2;124;200;255m\x1b[53mo\x1b[55m\x1b[39m\r\n\x1b[4m\x1b[58;2;10;20;30m \
\x1b[59m\x1b[24m \x1b[38;2;124;200;255m\x1b[4m \x1b[24m\x1b[39m \x1b[38;2;124;200;255m\
\x1b[4:3m \x1b[24m\x1b[39m"
            .as_bytes(),
    );
    let recolored_in = term_b.cell_frame(rows, cols);
    let plain_in = term_a.cell_frame(rows, cols);

    let (cw, ch) = cpu.cell_size();
    let cell = |f: &aterm_render::Frame, row: usize, col: usize| -> Vec<u32> {
        let pad = (f.width - cols * cw) / 2;
        let mut out = Vec::with_capacity(cw * ch);
        for y in pad + row * ch..(pad + row * ch + ch).min(f.height) {
            for x in pad + col * cw..(pad + col * cw + cw).min(f.width) {
                out.push(f.pixels[y * f.width + x]);
            }
        }
        out
    };

    let cpu_charred = cpu.render_input(&charred_in);
    let cpu_recolored = cpu.render_input(&recolored_in);
    let cpu_plain = cpu.render_input(&plain_in);
    let gpu_charred = gpu.render_input(&mut win, &charred_in, None);
    let gpu_recolored = gpu.render_input(&mut win, &recolored_in, None);
    let gpu_plain = gpu.render_input(&mut win, &plain_in, None);

    for (name, charred, recolored, plain) in [
        ("CPU", &cpu_charred, &cpu_recolored, &cpu_plain),
        ("GPU", &gpu_charred, &gpu_recolored, &gpu_plain),
    ] {
        assert_eq!(
            charred.pixels, recolored.pixels,
            "{name}: char_fg must substitute for the cell fg at EVERY deco \
             consult site (underline, undercurl, strike, overline) — \
             byte-identically to the SGR truecolor recolour"
        );
        assert_ne!(
            cell(plain, 1, 2),
            cell(charred, 1, 2),
            "{name}: the solid underline of a charred SPACE is the only ink in \
             that cell, so it must change colour"
        );
        assert_ne!(
            cell(plain, 1, 4),
            cell(charred, 1, 4),
            "{name}: the AA undercurl is a separate draw with its own re-derived \
             base_fg, and it must follow char_fg too"
        );
        assert_eq!(
            cell(plain, 1, 0),
            cell(charred, 1, 0),
            "{name}: an explicit SGR 58 underline colour still wins over char_fg"
        );
    }

    let delta = max_channel_delta(&cpu_charred.pixels, &gpu_charred.pixels);
    eprintln!("char_fg-on-decorations GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "charred decorated frame CPU/GPU diverge: delta {delta} > 8"
    );
}

/// Source-over and additive glow quads on one pipeline: the rainbow bed uses
/// `GlowQuad::alpha > 0` (premultiplied source-over), everything else
/// `alpha == 0` (One/One).
#[test]
fn source_over_glow_under_is_byte_exact_and_leaves_the_additive_half_alone() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = block_text_term(rows, cols, 1..8);
    let (cw, ch) = cpu.cell_size();
    let grid_w = cols * cw;

    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the stream deltas are effect-only"
    );

    let mut add_only: Vec<GlowQuad> = Vec::new();
    for (r, x0, x1, color) in [
        (2usize, (30 * cw) as i64, (38 * cw) as i64, 0x0080_4010u32),
        (3, (31 * cw) as i64, grid_w as i64 + 60, 0x0060_3018),
    ] {
        add_only.extend(emit_under(r, x0, x1, ch, grid_w, color));
    }
    let mut add_input = term.cell_frame(rows, cols);
    add_input.glow_under = add_only.clone();
    let cpu_add = cpu.render_input(&add_input);

    let mut mixed = add_only.clone();
    for (r, x0, x1, color, alpha) in [
        (
            4usize,
            (30 * cw) as i64,
            (39 * cw) as i64,
            0x0044_5522u32,
            120u8,
        ),
        (5, (30 * cw) as i64, (37 * cw) as i64, 0x0090_5008, 61),
        (6, (32 * cw) as i64, (40 * cw) as i64, 0x0012_0d05, 1),
        (7, (30 * cw) as i64, grid_w as i64 + 60, 0x0030_60c0, 255),
    ] {
        mixed.extend(emit_under(r, x0, x1, ch, grid_w, color).map(|q| GlowQuad {
            alpha,
            alpha2: alpha,
            ..q
        }));
    }
    assert!(
        mixed.iter().filter(|q| q.alpha > 0).count() >= 4,
        "the mixed field must carry a real source-over half"
    );
    let mut input = term.cell_frame(rows, cols);
    input.glow_under = mixed;
    let cpu_mix = cpu.render_input(&input);
    let gpu_mix = gpu.render_input(&mut win, &input, None);

    let add_band = 2 * ch..4 * ch;
    assert_eq!(
        cpu_mix.pixels[add_band.start * cpu_mix.width..add_band.end * cpu_mix.width],
        cpu_add.pixels[add_band.start * cpu_add.width..add_band.end * cpu_add.width],
        "the additive rows must be untouched by the source-over rows beside them"
    );
    assert_ne!(
        cpu_mix.pixels, cpu_base.pixels,
        "the mixed field must actually paint (non-vacuous)"
    );

    let same_but_additive: Vec<GlowQuad> = input
        .glow_under
        .iter()
        .map(|q| GlowQuad {
            alpha: 0,
            alpha2: 0,
            ..*q
        })
        .collect();
    let mut twin = term.cell_frame(rows, cols);
    twin.glow_under = same_but_additive;
    let cpu_twin = cpu.render_input(&twin);
    assert_ne!(
        cpu_mix.pixels, cpu_twin.pixels,
        "a build that ignored GlowQuad::alpha would pass every other clause here"
    );
    let over_band = 4 * ch..8 * ch;
    let (mut lower, mut sampled) = (0usize, 0usize);
    for y in over_band {
        for x in 0..cpu_mix.width {
            let (a, b) = (
                cpu_mix.pixels[y * cpu_mix.width + x],
                cpu_twin.pixels[y * cpu_twin.width + x],
            );
            if a != b {
                sampled += 1;
                if luma(a) < luma(b) {
                    lower += 1;
                }
            }
        }
    }
    assert!(
        sampled > 400,
        "the comparison must walk a real field: {sampled}"
    );
    assert_eq!(
        lower, sampled,
        "every source-over pixel must sit at or under its additive twin — it \
         DISPLACES the ground where the twin ADDS to it ({lower} of {sampled})"
    );

    let delta = max_channel_delta(&cpu_mix.pixels, &gpu_mix.pixels);
    eprintln!(
        "mixed additive + source-over glow_under GPU vs CPU max per-channel delta = {delta} \
         ({} quads, {} of them source-over)",
        input.glow_under.len(),
        input.glow_under.iter().filter(|q| q.alpha > 0).count()
    );
    if exact.holds(&gpu, "byte-exact source-over gate") {
        assert_byte_exact(
            "source_over_glow_under_is_byte_exact_and_leaves_the_additive_half_alone",
            "source-over glow_under field",
            &gpu,
            delta,
            format_args!(
                "a source-over glow_under field must be BYTE-EXACT CPU==GPU (got {delta})"
            ),
        );
    }
}

/// The transposed ribbon (`ribbon_beam_v`) through the GPU, byte for byte:
/// one-pixel-wide columns as tall as a slab, split at cell-row edges into
/// row-tagged pieces.
#[test]
fn ribbon_beam_v_train_is_byte_exact_cpu_vs_gpu() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(17.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = block_text_term(rows, cols, 1..8);
    let (cw, ch) = cpu.cell_size();
    assert_ne!(ch % 4, 0, "the row height is no multiple of the stride");
    let clip = BeamClip::grid(cols * cw, rows * ch, ch);

    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the train's delta is the twin's alone"
    );

    let vert = |y: f32, x: f32, reach: f32, color: u32, cov: f32| RibbonVertex {
        x: y,
        spine: x,
        up: reach,
        dn: reach * 0.55,
        core_up: reach * 0.2,
        core_dn: reach * 0.11,
        color,
        cov,
        lift: 0.0,
        lift_span: 0.0,
    };
    let (cw_f, ch_f) = (cw as f32, ch as f32);
    let trains = |dy: f32| -> Vec<GlowQuad> {
        let mut quads = Vec::new();
        let vertical = [
            vert(6.5 * ch_f + dy, 34.5 * cw_f, 0.9 * cw_f, 0x00FF_3020, 120.0),
            vert(3.5 * ch_f + dy, 34.5 * cw_f, 0.7 * cw_f, 0x0030_FF60, 90.0),
            vert(0.5 * ch_f + dy, 34.5 * cw_f, 0.5 * cw_f, 0x0020_40FF, 60.0),
        ];
        assert!(aterm_render::ribbon_beam_v(
            &mut quads,
            clip,
            &vertical,
            0.55,
            4,
            100_000,
            GlowBlend::Over
        ));
        let diagonal = [
            vert(0.5 * ch_f + dy, 31.5 * cw_f, 0.8 * cw_f, 0x00FF_C020, 200.0),
            vert(6.5 * ch_f + dy, 37.5 * cw_f, 0.4 * cw_f, 0x0040_20FF, 110.0),
        ];
        assert!(aterm_render::ribbon_beam_v(
            &mut quads,
            clip,
            &diagonal,
            0.55,
            4,
            100_000,
            GlowBlend::Add
        ));
        quads
    };
    let quads = trains(0.0);
    assert!(quads.iter().all(|q| q.w == 1));
    assert!(
        quads.iter().any(|q| q.h > 1),
        "column pieces taller than a row"
    );
    assert!(
        quads.iter().any(|q| q.h < 4),
        "…and some split short of the stride"
    );
    assert!(quads.iter().any(|q| q.alpha > 0) && quads.iter().any(|q| q.alpha == 0));
    assert!(
        quads
            .iter()
            .map(|q| q.row)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            >= 6,
        "the trains must cross several row bands"
    );
    let mut input = term.cell_frame(rows, cols);
    input.glow_under = quads;
    let cpu_train = cpu.render_input(&input);
    let gpu_train = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_train.pixels, cpu_base.pixels,
        "the trains must actually paint (non-vacuous)"
    );
    let delta = max_channel_delta(&cpu_train.pixels, &gpu_train.pixels);
    eprintln!("ribbon_beam_v trains CPU vs GPU max per-channel delta = {delta}");
    if exact.holds(&gpu, "byte-exact ribbon_beam_v gate") {
        assert_byte_exact(
            "ribbon_beam_v_train_is_byte_exact_cpu_vs_gpu",
            "ribbon_beam_v trains",
            &gpu,
            delta,
            format_args!("a ribbon_beam_v train must be BYTE-EXACT CPU==GPU (got {delta})"),
        );
    }

    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let mut in_a = term.cell_frame(rows, cols);
    in_a.glow_under = trains(0.0);
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);
    let mut in_b = term.cell_frame(rows, cols);
    in_b.glow_under = trains(ch_f);
    let misses_before = gpu.gate_misses();
    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_b = gpu
        .render_input_cached(&mut win_gpu, &in_b)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "a moved train must MISS the GPU dirty gate (real re-render)"
    );
    let cpu_fresh = cpu.render_input(&in_b).pixels.clone();
    assert_eq!(
        cpu_b, cpu_fresh,
        "CPU cached-damaged train frame must equal a fresh full render"
    );
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path ribbon_beam_v CPU vs GPU max per-channel delta = {delta}");
    if exact.holds(&gpu, "damaged-path byte-exact ribbon_beam_v gate") {
        assert_byte_exact(
            "ribbon_beam_v_train_is_byte_exact_cpu_vs_gpu",
            "damaged-path ribbon_beam_v",
            &gpu,
            delta,
            format_args!(
                "a moved ribbon_beam_v train via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
            ),
        );
    }
}

/// A gradient quad (`GlowQuad::color2`) is byte-exact across backends: the CPU
/// ramps colour and opacity per column with `aterm_render::glow_lerp`'s integer
/// law and `fs_glow` computes the same law from the instance bytes.
#[test]
fn gradient_glow_under_is_byte_exact_cpu_vs_gpu() {
    gradient_glow_under_parity(&ByteExact::default());
}

fn gradient_glow_under_parity(exact: &ByteExact) {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (12usize, 40usize);
    let mut term = block_text_term(rows, cols, 1..11);
    let (cw, ch) = cpu.cell_size();
    let grid_w = cols * cw;

    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the stream deltas are effect-only"
    );

    const WIDTHS: [u16; 6] = [1, 2, 3, 7, 15, 40];
    let ramps: [(u32, u32, u8, u8); 8] = [
        (0x00FF_4020, 0x0020_40FF, 1, 236),
        (0x0020_40FF, 0x00FF_4020, 236, 1),
        (0x00FF_4020, 0x0020_40FF, 0, 0),
        (0x0010_FF80, 0x00FF_1080, 0, 0),
        (0x00FF_4020, 0x0020_40FF, 120, 120),
        (0x0040_FF40, 0x0040_40FF, 255, 255),
        (0x00FF_FFFF, 0x0000_0000, 200, 1),
        (0x0000_0000, 0x00FF_FFFF, 1, 200),
    ];
    let mut field: Vec<GlowQuad> = Vec::new();
    for (k, &(rgb0, rgb1, a0, a1)) in ramps.iter().enumerate() {
        let row = 2 + k;
        let mut x = (30 * cw) as u16;
        for &w in &WIDTHS {
            let (cov0, cov1) = if a0 == 0 && a1 == 0 {
                (150u8, 40u8)
            } else {
                (a0, a1)
            };
            let x1 = (x + w).min(grid_w as u16);
            if x1 > x {
                field.push(GlowQuad {
                    row: row as u16,
                    x,
                    y: (row * ch) as u16,
                    w: x1 - x,
                    h: ch as u16,
                    color: aterm_render::premul_rgb(rgb0, cov0),
                    alpha: a0,
                    color2: aterm_render::premul_rgb(rgb1, cov1),
                    alpha2: a1,
                });
            }
            x = x1 + 1;
        }
    }
    assert!(
        field.iter().any(|q| q.alpha > 0 && q.alpha != q.alpha2),
        "the field carries an opacity ramp"
    );
    assert!(
        field
            .iter()
            .any(|q| q.alpha == 0 && q.alpha2 == 0 && q.color != q.color2),
        "the field carries an additive colour ramp"
    );
    assert!(
        field.iter().any(|q| q.w == 40),
        "the widest quad fits the ground"
    );

    let mut input = term.cell_frame(rows, cols);
    input.glow_under = field.clone();
    let cpu_grad = cpu.render_input(&input);
    let gpu_grad = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_grad.pixels, cpu_base.pixels,
        "the gradient field must actually paint (non-vacuous)"
    );

    let flat: Vec<GlowQuad> = field
        .iter()
        .map(|q| GlowQuad {
            color2: q.color,
            alpha2: q.alpha,
            ..*q
        })
        .collect();
    let mut flat_input = term.cell_frame(rows, cols);
    flat_input.glow_under = flat;
    let cpu_flat = cpu.render_input(&flat_input);
    let gpu_flat = gpu.render_input(&mut win, &flat_input, None);
    assert_ne!(
        cpu_grad.pixels, cpu_flat.pixels,
        "a gradient field must differ from its flat twin, or no ramp was drawn"
    );

    let delta = max_channel_delta(&cpu_grad.pixels, &gpu_grad.pixels);
    let delta_flat = max_channel_delta(&cpu_flat.pixels, &gpu_flat.pixels);
    eprintln!(
        "gradient glow_under GPU vs CPU max per-channel delta = {delta} ({} \
         quads); flat twin = {delta_flat}",
        input.glow_under.len()
    );
    if exact.holds(&gpu, "byte-exact gradient gate") {
        assert_byte_exact(
            "gradient_glow_under_is_byte_exact_cpu_vs_gpu",
            "gradient glow_under field",
            &gpu,
            delta,
            format_args!("a gradient glow_under field must be BYTE-EXACT CPU==GPU (got {delta})"),
        );
        assert_byte_exact(
            "gradient_glow_under_is_byte_exact_cpu_vs_gpu",
            "flat twin of the gradient field",
            &gpu,
            delta_flat,
            format_args!("the flat twin must be BYTE-EXACT CPU==GPU (got {delta_flat})"),
        );
    }
}

/// §15.2 1.9, §4 rule 1: a selected word under the light glass shows its selection on both
/// backends. The glass laid across a Default Light row whose word `two` is selected yields
/// there (`aterm_effects::cursor_glow::glass_yield`): each selected cell draws as it draws with
/// no glass at all, on the CPU and on the GPU, while the theme's words beside it keep their
/// glass. Negative control: the glass laid whole tints the selection.
#[test]
fn a_selected_word_under_the_light_glass_shows_its_selection() {
    use aterm_core::render::GlowRamps;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_effects::cursor_glow::glass_yield::{GlassLaid, GlassYield};
    let (fg, bg) = (0x002B_2D33, 0x00F6_F7F9);
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    gpu.set_bloom(false);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (3usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07one two three");
    let selection = term.text_selection_mut();
    selection.start_selection(0, 4, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(0, 6, SelectionSide::Right);
    selection.complete_selection();
    let mut base = term.cell_frame(rows, cols);
    base.cursor_visible = false;
    base.selection_bg = 0x00B4_D5FE;
    let glass = GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: (cols * cw) as u16,
        h: ch as u16,
        color: 0x005A_4B00,
        alpha: 90,
        color2: 0x005A_4B00,
        alpha2: 90,
    };
    let mut whole = base.clone();
    whole.glow_under = vec![glass];
    let mut yielded = whole.clone();
    let laid = GlassLaid::new(
        std::time::Instant::now(),
        (vec![glass], GlowRamps::default()),
        (fg, bg),
        (0, 0, cw as i32, ch as i32),
        None,
    );
    let report = GlassYield::default().apply(&mut yielded, &laid, &[]);
    assert!(report.cut(), "premise: the selection yields the glass");
    let cell = |f: &aterm_render::Frame, col: usize| -> Vec<u32> {
        (0..ch)
            .flat_map(|y| (col * cw..(col + 1) * cw).map(move |x| f.pixels[y * f.width + x]))
            .collect()
    };
    for (name, plain, yielded_frame, whole_frame) in [
        (
            "CPU",
            cpu.render_input(&base),
            cpu.render_input(&yielded),
            cpu.render_input(&whole),
        ),
        (
            "GPU",
            gpu.render_input(&mut win, &base, None),
            gpu.render_input(&mut win, &yielded, None),
            gpu.render_input(&mut win, &whole, None),
        ),
    ] {
        for col in 4..7 {
            assert_eq!(
                cell(&yielded_frame, col),
                cell(&plain, col),
                "{name}: the selected cell {col} draws as it does with no glass"
            );
            assert_ne!(
                cell(&whole_frame, col),
                cell(&plain, col),
                "{name}, control: the whole glass tints the selected cell {col}"
            );
        }
        for col in [0, 1, 2, 9, 10] {
            assert_eq!(
                cell(&yielded_frame, col),
                cell(&whole_frame, col),
                "{name}: the theme's cell {col} keeps its glass"
            );
        }
    }
}
