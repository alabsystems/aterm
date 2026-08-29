// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// EMBERFORGE "dark glyph cores" CPU/GPU byte parity — the two P6 consumer
// streams end-to-end: `RenderInput.glow_under` (flame-BODY One/One additive
// light drawn BETWEEN the cell background fill and the glyph ink: the GPU
// parts its base pass around a Unorm glow_under pass, the CPU runs phase B3
// between the bg/sprite phases and the fg phase) and `RenderInput.char_fg`
// (per-cell FINAL glyph-ink overrides charring engulfed letterforms toward
// ember-black at the shared ink fg seam, before the legibility floors).
//
// Covered:
//   * base frames WITHOUT the streams are byte-exact CPU==GPU (delta 0), so
//     the measured deltas are effect-only;
//   * char_fg ALONE over block text is byte-exact UNGATED (a pure fg
//     substitution through the ordinary glyph path — no additive involved);
//   * a SYNTHETIC glow_under field over CHARRED block text — varied colours,
//     multiple rows, quads clipped at the grid edges — is BYTE-EXACT, and the
//     silhouette law holds on both backends (the charred stroke is darker
//     than the lit background beside it);
//   * the damaged/cached path: a body quad MOVING while the charring sweeps
//     must miss the GPU dirty gate and re-render byte-exactly (the prev∪cur
//     row discipline);
//   * char_fg FOLLOWS INTO LINE DECORATIONS on both backends: a charred
//     underline / undercurl / strike / overline is byte-identical to the same
//     text recoloured via SGR truecolor fg, WITHIN each backend;
//   * an emptied pair (`clear_overlays`) restores the bare GPU frame.
//
// Gated: no GPU or no font -> the tests no-op (return), like the other parity
// gates. Byte-exact additive gates additionally skip on downlevel
// (sRGB-offscreen) adapters via `additive_is_byte_exact`, the glow idiom.

use aterm_core::render::{CharFg, GlowQuad};
use aterm_core::terminal::Terminal;
use aterm_render::{Theme, WindowCpu};

mod common;
use common::{backends, bb, gg, max_channel_delta, rr};

fn luma(p: u32) -> i32 {
    rr(p) + gg(p) + bb(p)
}

/// One legal flame-body quad: a grid-interior rect clamped to the grid and to
/// its single row band (the GlowQuad producer contract). Returns `None` for a
/// fully-clipped rect.
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
        // ADDITIVE light (see `GlowQuad::alpha`).
        alpha: 0,
    })
}

/// THE glow_under + char_fg parity pin. The base frame is procedural
/// full-block glyphs — byte-exact CPU==GPU (delta 0) — so the deltas below are
/// the streams' alone. char_fg alone must stay byte-exact UNGATED (ordinary
/// glyph path); the combined engulfed frame must be byte-exact wherever the
/// additive contract is (native Unorm offscreen), and the silhouette law must
/// hold on BOTH backends.
#[test]
fn glow_under_char_fg_field_is_byte_exact_over_text() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Hidden cursor + procedural block rows: a byte-exact base with TEXT under
    // the fire (blocks render identically on both backends — delta 0).
    term.process("\x1b[?25l".as_bytes());
    for r in 1..8usize {
        term.process(format!("\x1b[{};1H{}", r + 1, "█".repeat(30)).as_bytes());
    }
    let (cw, ch) = cpu.cell_size();
    let grid_w = cols * cw;

    // (a) Base (no streams): the effect-only premise — CPU==GPU exactly.
    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the stream deltas are effect-only"
    );

    // (b) char_fg ALONE: charred glyph ink through the ordinary glyph path —
    // byte-exact UNGATED (no additive stream involved). Sorted by (row, col).
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

    // (c) The engulfed frame: a multi-row flame-body field (varied colours,
    // left/right edge-clipped quads) UNDER the charred text.
    let mut quads = Vec::new();
    for (r, x0, x1, color) in [
        (2usize, -20i64, (8 * cw) as i64, 0x0080_4010u32), // clipped LEFT
        (3, (6 * cw) as i64, (16 * cw) as i64, 0x0060_3018),
        (4, (2 * cw) as i64, (12 * cw) as i64, 0x0044_5522),
        (5, -4, (5 * cw) as i64, 0x0090_5008),
        (7, (36 * cw) as i64, grid_w as i64 + 60, 0x0030_60c0), // clipped RIGHT
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
    // Byte-exact additive holds only on native (plain-Unorm offscreen); the
    // downlevel sRGB offscreen folds the add into linear — the glow idiom.
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "glow_under under the glyph ink must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP byte-exact glow_under gate: downlevel sRGB offscreen (linear add)");
    }

    // (d) THE SILHOUETTE LAW, on both backends: inside the row-2 body quad the
    // charred block stroke (col 3) is darker than the lit background beside
    // the text (row 2 has blocks only up to col 29; col 2 is an uncharred
    // block, so sample the charred cell vs the SAME row's lit block one cell
    // over — the charred core must be the darkest thing in the fire).
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

/// DAMAGED/CACHED-PATH parity: the per-frame presentation hot path
/// (`render_input_cached` on persistent renderer+window per backend). Frame A
/// engulfs row 1 (body quad + charred cells, priming both caches); frame B
/// MOVES the fire to row 4 — a real change that must MISS the GPU dirty gate,
/// repaint the prev∪cur rows (`glow_under_changed` + `char_fg_changed`), and
/// land byte-exact on both backends.
#[test]
fn damaged_path_glow_under_char_fg_parity_cpu_matches_gpu() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Block text on rows 1 and 4 so the charring recolours real glyphs.
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
                // ADDITIVE light (see `GlowQuad::alpha`).
                alpha: 0,
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

    // Frame A: fire on row 1 — primes both caches.
    let mut in_a = term.cell_frame(rows, cols);
    let (qa, ca) = fire_at(1);
    in_a.glow_under.push(qa);
    in_a.char_fg = ca;
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    // Frame B: the fire CLIMBS to row 4 — a genuine content change.
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

    // Ground truth: the damaged frame must equal a FRESH full render (no light
    // ghost / stale charred glyph at the vacated row 1, the fire landed at 4) ...
    let cpu_fresh = cpu.render_input(&in_b).pixels.clone();
    assert_eq!(
        cpu_b, cpu_fresh,
        "CPU cached-damaged engulfed frame must equal a fresh full render"
    );
    // ... and byte-exact across backends.
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path glow_under+char_fg CPU vs GPU max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "engulfed frame via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP damaged-path byte-exact glow_under gate: downlevel sRGB offscreen");
    }
}

/// char_fg FOLLOWS INTO LINE DECORATIONS, on BOTH backends. A decoration is a
/// decoration OF the glyph's ink, and the substituted colour is re-derived at
/// three independent sites — CPU pass 3 (solid rects), CPU pass 3b (the AA
/// undercurl, a separate pass) and the GPU deco loop. Until this fixture every
/// char_fg test drew blocks and flames with no SGR styling and every decoration
/// test drew SGR styling with no overlay stream, so the `None`-with-char_fg arm
/// at a decorated cell was never taken on either backend — the same shape that
/// produced the confirmed ink-vs-curl divergence one stream over.
///
/// The proof is the `ink_gpu_matches_cpu` idiom, WITHIN each backend: a charred
/// frame must be byte-identical to the same text recoloured via SGR 38;2. That
/// form is deliberately not a cross-backend byte gate — real glyph AA lives
/// under the suite's <=8 delta bar, not at 0 — but a char_fg-only difference
/// must add exactly zero on each side, which is the property at risk here.
///
/// Row 1 is the per-site isolate: its cells are SPACES, so the decoration is
/// the only ink in them. Col 2 (solid underline) and col 4 (undercurl) must
/// each change when char_fg lands; col 0's explicit SGR 58 underline colour
/// must not move (`deco_inks`'s explicit arms ignore the operand).
#[test]
fn char_fg_follows_into_line_decorations_on_both_backends() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    // Deterministic parity: neither backend may compare a provisional `.notdef`
    // frame against a real glyph (the ink_gpu_matches_cpu discipline).
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (2usize, 12usize);
    let charred_fg = 0x007C_C8FFu32;

    // Row 0: underlined x, curly-underlined w, struck s, overlined o.
    // Row 1: the deco-only isolates — SGR 58 underlined space (col 0), plain
    // underlined space (col 2), curly-underlined space (col 4).
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

    // The same text via SGR 38;2 — no char_fg. Row 1 col 0 stays un-recoloured:
    // its SGR 58 underline colour wins in both frames (the precedence pin).
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
        // Non-vacuity, per site: the deco-only cells of row 1 must move.
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

    // Cross-backend: the charred decorated frame stays inside the suite's
    // glyph-AA budget (this frame has real glyphs, so it is NOT a byte gate).
    let delta = max_channel_delta(&cpu_charred.pixels, &gpu_charred.pixels);
    eprintln!("char_fg-on-decorations GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "charred decorated frame CPU/GPU diverge: delta {delta} > 8"
    );
}

/// An emptied pair is byte-identical on the GPU: a populated
/// `glow_under`+`char_fg` frame must paint, and `clear_overlays` must restore
/// the bare frame — the introspection-capture (`image plain`) contract, GPU
/// side (also pins that a glow_under-free frame opens NO extra passes: the
/// fused base pass reproduces the bare bytes).
#[test]
fn glow_under_disabled_bytes_identical_on_gpu() {
    let theme = Theme::default();
    let Some((cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l$ embers off");
    let (cw, ch) = cpu.cell_size();

    let base_input = term.cell_frame(rows, cols);
    assert!(base_input.glow_under.is_empty() && base_input.char_fg.is_empty());
    let base = gpu.render_input(&mut win, &base_input, None).pixels;

    let mut cleared = term.cell_frame(rows, cols);
    cleared.glow_under.push(GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: (10 * cw) as u16,
        h: ch as u16,
        color: 0x0060_3010,
        // ADDITIVE light (see `GlowQuad::alpha`).
        alpha: 0,
    });
    cleared.char_fg.push(CharFg {
        row: 0,
        col: 2,
        fg: 0x0010_0804,
    });
    let painted = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_ne!(
        base, painted,
        "a live glow_under+char_fg frame must paint on the GPU"
    );
    cleared.clear_overlays();
    assert!(
        cleared.glow_under.is_empty() && cleared.char_fg.is_empty(),
        "clear_overlays must strip both streams"
    );
    let stripped = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_eq!(
        base, stripped,
        "clear_overlays must restore the bare GPU frame (both streams ARE bling)"
    );
}

/// **THE SOURCE-OVER BED, BYTE FOR BYTE.** The rainbow bed composites
/// `GlowQuad::alpha > 0` — premultiplied SOURCE-OVER, `src + dst·(1 − a)` — while
/// every other flat stream on this pipeline keeps `alpha == 0` and stays
/// `One`/`One`. This is the gate that says the two modes ride ONE pipeline
/// without either one moving: a MIXED field of both, over real text, in one
/// frame.
///
/// Three claims, and the second is the one a delta-only test would miss.
///
/// 1. **PARITY.** The CPU's [`aterm_render::over_premul`] and the GPU's
///    `One`/`OneMinusSrcAlpha` land the identical byte, on the same
///    plain-Unorm offscreen and under the same gate the additive streams take.
/// 2. **THE ADDITIVE HALF DID NOT MOVE.** The same additive-only field renders
///    byte-identically to what it renders with the source-over quads removed —
///    so `alpha == 0` really is `One`/`One`, and switching the pipeline's blend
///    state cost the historical streams nothing.
/// 3. **THE MODES ARE DISTINGUISHABLE.** A source-over quad and an additive
///    quad carrying the SAME premultiplied colour over the same ground must
///    produce DIFFERENT pixels (the source-over one darker, by exactly the
///    ground it displaced), or the test would pass on a build that ignored
///    `alpha` entirely.
#[test]
fn source_over_glow_under_is_byte_exact_and_leaves_the_additive_half_alone() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());
    for r in 1..8usize {
        term.process(format!("\x1b[{};1H{}", r + 1, "█".repeat(30)).as_bytes());
    }
    let (cw, ch) = cpu.cell_size();
    let grid_w = cols * cw;

    // The base premise: without any stream the two backends are byte-exact, so
    // every delta below is the stream's.
    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the stream deltas are effect-only"
    );

    // **RIGHT OF THE TEXT, DELIBERATELY.** The procedural rows are full-block
    // glyphs out to column 29, and the bed draws UNDER the glyph pass — a quad
    // beneath a solid block is completely hidden, and every clause below would
    // pass vacuously on pixels the mark never reached. Columns 30..40 are bare
    // ground on every row, which is where a bed is actually visible and where
    // the mode difference can be read.
    //
    // The ADDITIVE half, alone. Rows 2-3, `alpha == 0`.
    let mut add_only: Vec<GlowQuad> = Vec::new();
    for (r, x0, x1, color) in [
        (2usize, (30 * cw) as i64, (38 * cw) as i64, 0x0080_4010u32),
        (3, (31 * cw) as i64, grid_w as i64 + 60, 0x0060_3018), // clipped RIGHT
    ] {
        add_only.extend(emit_under(r, x0, x1, ch, grid_w, color));
    }
    let mut add_input = term.cell_frame(rows, cols);
    add_input.glow_under = add_only.clone();
    let cpu_add = cpu.render_input(&add_input);

    // …and the SOURCE-OVER half beside it, on rows the additive half does not
    // touch, at opacities spanning the bed's real range (the shipped ceiling is
    // `RAINBOW_UNDER_COV_CAP`, 120 of 255) plus the two ends of the byte.
    let mut mixed = add_only.clone();
    for (r, x0, x1, color, alpha) in [
        (4usize, (30 * cw) as i64, (39 * cw) as i64, 0x0044_5522u32, 120u8),
        (5, (30 * cw) as i64, (37 * cw) as i64, 0x0090_5008, 61),
        (6, (32 * cw) as i64, (40 * cw) as i64, 0x0012_0d05, 1),
        (7, (30 * cw) as i64, grid_w as i64 + 60, 0x0030_60c0, 255),
    ] {
        mixed.extend(emit_under(r, x0, x1, ch, grid_w, color).map(|q| GlowQuad { alpha, ..q }));
    }
    assert!(
        mixed.iter().filter(|q| q.alpha > 0).count() >= 4,
        "the mixed field must carry a real source-over half"
    );
    let mut input = term.cell_frame(rows, cols);
    input.glow_under = mixed;
    let cpu_mix = cpu.render_input(&input);
    let gpu_mix = gpu.render_input(&mut win, &input, None);

    // 2. THE ADDITIVE HALF DID NOT MOVE. Rows 2-3 carry only `alpha == 0`
    //    quads, and they must read exactly what they read with the source-over
    //    quads absent — the pixels, not a tolerance.
    // `glow_under` is a WINDOW-ABSOLUTE stream and `emit_under` writes
    // `y = row * ch` into it, so the bands below are window rows and take NO
    // pad offset — the quads land exactly where the emitter put them.
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

    // 3. THE MODES ARE DISTINGUISHABLE. Same premultiplied colour, same
    //    geometry, one with `alpha` and one without: the source-over frame must
    //    differ, and differ DOWNWARD, by the ground it displaced.
    let same_but_additive: Vec<GlowQuad> = input
        .glow_under
        .iter()
        .map(|q| GlowQuad { alpha: 0, ..*q })
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
    assert!(sampled > 400, "the comparison must walk a real field: {sampled}");
    assert_eq!(
        lower, sampled,
        "every source-over pixel must sit at or under its additive twin — it \
         DISPLACES the ground where the twin ADDS to it ({lower} of {sampled})"
    );

    // 1. PARITY, on the same gate the additive streams take: byte-exact holds on
    //    a plain-Unorm offscreen; the downlevel sRGB offscreen folds the blend
    //    into linear, which is the glow idiom's accepted approximation and
    //    applies to `OneMinusSrcAlpha` for exactly the same reason it applies to
    //    `One`.
    let delta = max_channel_delta(&cpu_mix.pixels, &gpu_mix.pixels);
    eprintln!(
        "mixed additive + source-over glow_under GPU vs CPU max per-channel delta = {delta} \
         ({} quads, {} of them source-over)",
        input.glow_under.len(),
        input.glow_under.iter().filter(|q| q.alpha > 0).count()
    );
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "a source-over glow_under field must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP byte-exact source-over gate: downlevel sRGB offscreen (linear blend)");
    }
}
