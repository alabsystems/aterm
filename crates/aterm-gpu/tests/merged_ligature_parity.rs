// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// CPU==GPU parity for N:1 merged ligatures (`admit_collapsed`): a run whose
// shaping collapses several cells into one wide glyph is sliced into per-cell
// tiles (cell-local `mono_gid` slice glyphs shared via the atlas), and the GPU
// must place them exactly as the CPU does. No merged-ligature font is bundled,
// so one is discovered (Latin `f_i`/`f_l`/`ffi` ligatures collapse N:1); with
// none, or no GPU, the test skips. The font is handed to both renderers as
// their family (`--font`), so nothing writes the environment.

use crate::common;
use aterm_core::terminal::Terminal;
use aterm_render::{LigatureMode, Renderer, TextShapingConfig, Theme};

use crate::common::max_channel_delta_frame as max_channel_delta;

/// A font path whose `fi` collapses N:1 under `liga`/`calt` — the CPU row plan
/// slices it into [`aterm_render::ColumnGlyph::LigatedSlice`] columns. Order:
/// $ATERM_MERGED_FONT, then well-known macOS faces carrying Latin `f`-ligatures.
/// `None` -> the caller SKIPs.
fn merged_ligature_font_path() -> Option<std::path::PathBuf> {
    let collapses = |path: &std::path::Path| -> bool {
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        let Ok(mut r) = Renderer::from_bytes(&bytes, 18.0, Theme::default()) else {
            return false;
        };
        r.set_text_shaping(shaping(true));
        let mut term = Terminal::new(1, 4);
        term.process(b"fi");
        let mut plan = Vec::new();
        r.row_glyph_plan(&term.cell_frame(1, 4), 0, &[], &mut plan);
        matches!(
            plan.first(),
            Some(aterm_render::ColumnGlyph::LigatedSlice { .. })
        )
    };
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("ATERM_MERGED_FONT") {
        candidates.push(p.into());
    }
    candidates.extend(
        [
            "/System/Library/Fonts/Avenir Next Condensed.ttc",
            "/System/Library/Fonts/MuktaMahee.ttc",
            "/System/Library/Fonts/Supplemental/Georgia.ttf",
            "/System/Library/Fonts/Supplemental/Times New Roman.ttf",
        ]
        .iter()
        .map(std::path::PathBuf::from),
    );
    candidates.into_iter().find(|p| collapses(p))
}

/// Discovery, once per process: both #[test] fns want the SAME discovered font,
/// and the shape-probing scan is not free. `None` -> the caller SKIPs.
fn merged_ligature_font() -> Option<&'static std::path::Path> {
    static FONT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    FONT.get_or_init(merged_ligature_font_path).as_deref()
}

fn shaping(admit: bool) -> TextShapingConfig {
    TextShapingConfig {
        ligature_mode: LigatureMode::Enabled,
        admit_collapsed: admit,
        ..Default::default()
    }
}

#[test]
fn merged_ligature_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    // Discovers the font once per process (see merged_ligature_font) and builds
    // BOTH renderers on it, font-settled by the kit (`common::font_settled`).
    let Some(font) = merged_ligature_font() else {
        eprintln!("SKIP: no merged-ligature font (set ATERM_MERGED_FONT)");
        return;
    };
    let Some((mut cpu, mut gpu)) = common::backends_with_font(font, px, theme) else {
        return;
    };
    // A CPU renderer with the merge declined, proving the sliced frame is not
    // vacuously equal. Settled like the kit's pair, so a chain landing between
    // arms cannot fake or mask the difference.
    let Some(mut cpu_off) = font
        .to_str()
        .and_then(|family| Renderer::from_system_with_family(Some(family), px, theme))
    else {
        return;
    };
    cpu_off.debug_block_on_lazy_fallbacks();
    gpu.set_text_shaping(shaping(true));
    cpu.set_text_shaping(shaping(true));
    cpu_off.set_text_shaping(shaping(false));

    let (rows, cols) = (1usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // A row of collapsing clusters interleaved with plain text so slices sit next
    // to ordinary per-cell glyphs (exercising the run-break on both sides).
    term.process(b"\x1b[?25la fi b fl c ffi d");

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let cpu_off_frame = cpu_off.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );
    // Non-vacuous: with the merge admitted the sliced glyphs really changed the
    // ink vs the declined (per-cell) render.
    assert_ne!(
        cpu_frame.pixels, cpu_off_frame.pixels,
        "the merged ligatures did not slice — test would be vacuous (is this a merging font?)"
    );
    // The gate: GPU reproduces the CPU sliced frame within the blend tolerance,
    // because both keyed + placed the IDENTICAL per-cell slice tiles.
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("merged-ligature GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU merged-ligature pixels diverge: max per-channel delta {delta} > 8"
    );
}

/// CPU==GPU parity with a block cursor on a merged-ligature cell: the cut-out
/// re-colours only that cell's slice tile. Non-vacuous: the cursor frame
/// differs from the hidden-cursor frame.
#[test]
fn merged_ligature_cursor_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    // Same once-per-process discovery and font-settled pair as the sibling test
    // (this one renders the CPU twice and the GPU once).
    let Some(font) = merged_ligature_font() else {
        eprintln!("SKIP: no merged-ligature font");
        return;
    };
    let Some((mut cpu, mut gpu)) = common::backends_with_font(font, px, theme) else {
        return;
    };
    gpu.set_text_shaping(shaping(true));
    cpu.set_text_shaping(shaping(true));

    let (rows, cols) = (1usize, 8usize);
    let mut win = aterm_gpu::WindowGpu::new();

    // Cursor hidden baseline (CPU) for the non-vacuity check.
    let mut hidden = Terminal::new(rows as u16, cols as u16);
    hidden.process(b"\x1b[?25lfi");
    let cpu_hidden = cpu.render_input(&hidden.cell_frame(rows, cols));

    // Block cursor on cell 1 (the second half of the merged `fi`).
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[2 qfi\x1b[1;2H");
    let input = term.cell_frame(rows, cols);
    let cpu_cur = cpu.render_input(&input);
    let gpu_cur = gpu.render_input(&mut win, &input, None);

    assert_ne!(
        cpu_cur.pixels, cpu_hidden.pixels,
        "the block cursor over the merged ligature must change the CPU render"
    );
    assert_eq!(
        (gpu_cur.width, gpu_cur.height),
        (cpu_cur.width, cpu_cur.height),
        "dimensions differ"
    );
    let delta = max_channel_delta(&cpu_cur, &gpu_cur);
    eprintln!("merged-ligature cursor GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU merged-ligature cursor pixels diverge: max per-channel delta {delta} > 8"
    );
}
