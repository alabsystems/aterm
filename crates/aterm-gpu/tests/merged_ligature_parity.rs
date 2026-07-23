// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// CPU==GPU parity for M4 Cascadia N:1 MERGED ligatures (`admit_collapsed`): a run
// whose OpenType shaping COLLAPSES several cells into ONE wide glyph is sliced
// into per-cell tiles, and the GPU must key + place those tiles byte-for-byte
// with the CPU (each slice is an ordinary cell-local `mono_gid` slice glyph both
// backends share via the atlas). aterm bundles no merged-ligature font, so this
// DISCOVERS one (any font with the classic Latin `f_i`/`f_l`/`ffi` ligatures
// collapses `fi`/`fl`/`ffi` N:1) and SKIPs cleanly when none is present or no GPU
// is available. Its OWN test binary so the $ATERM_FONT it sets never races the
// other parity suites; within this binary the set is hoisted behind a OnceLock
// (see merged_ligature_font) so the parallel #[test] threads never race it
// either.

use aterm_core::terminal::Terminal;
use aterm_render::{Frame, LigatureMode, Renderer, TextShapingConfig, Theme};

fn rr(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn gg(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn bb(p: u32) -> i32 {
    (p & 0xff) as i32
}
fn max_channel_delta(a: &Frame, b: &Frame) -> i32 {
    let mut m = 0;
    for (&pa, &pb) in a.pixels.iter().zip(b.pixels.iter()) {
        m = m.max((rr(pa) - rr(pb)).abs());
        m = m.max((gg(pa) - gg(pb)).abs());
        m = m.max((bb(pa) - bb(pb)).abs());
    }
    m
}

/// A font path whose `fi` collapses N:1 under `liga`/`calt`. Order:
/// $ATERM_MERGED_FONT, then well-known macOS faces carrying Latin `f`-ligatures.
/// `None` -> the caller SKIPs.
fn merged_ligature_font_path() -> Option<std::path::PathBuf> {
    let feats = aterm_render::ligature_shaping::build_feature_list(&[], true);
    let collapses = |path: &std::path::Path| -> bool {
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        matches!(
            aterm_render::ligature_shaping::shape_ligature_run(
                &bytes,
                0,
                "fi",
                &['f', 'i'],
                true,
                true,
                &feats,
                &[],
            ),
            Some(aterm_render::ShapedRun::Collapsed { .. })
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

/// Discovery + the SINGLE point where $ATERM_FONT is exported to both renderers.
/// Both #[test] fns in this binary want the SAME discovered font, but libtest
/// runs them on PARALLEL threads — a per-test set_var would race the sibling
/// test's renderer construction (C-side getenv/setenv under concurrent mutation
/// is dangling-pointer UB). `get_or_init` parks every caller until the closure
/// returns, so the ONE write is complete before any renderer in this process is
/// built — the same guarantee glow_parity.rs gets from its Once. (Bonus: the
/// shape-probing discovery scan now runs once, not once per test.) `None` -> the
/// caller SKIPs.
fn merged_ligature_font() -> Option<&'static std::path::Path> {
    static FONT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let found = merged_ligature_font_path()?;
        // SAFETY: set exactly once per process (OnceLock init), before any renderer
        // is constructed — every concurrent caller is parked in get_or_init until
        // this write completes, so no getenv can observe it mid-mutation. (set_var
        // is unsafe in edition 2024.)
        unsafe { std::env::set_var("ATERM_FONT", &found) };
        Some(found)
    })
    .as_deref()
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

    // Discovers the font AND points BOTH renderers at it via $ATERM_FONT, once
    // per process (see merged_ligature_font).
    if merged_ligature_font().is_none() {
        eprintln!("SKIP: no merged-ligature font (set ATERM_MERGED_FONT)");
        return;
    }

    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system font");
        return;
    };
    // A CPU renderer with the merge DECLINED, to prove the sliced frame is not
    // vacuously equal (the font really collapses `fi`/`fl`/`ffi`).
    let Some(mut cpu_off) = Renderer::from_system(px, theme) else {
        return;
    };
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

/// CPU==GPU parity with a BLOCK cursor parked on a merged-ligature cell: the
/// cut-out re-colours only the cursor cell's slice tile, and the GPU quad slicing
/// must reproduce it. Non-vacuous: the cursor frame differs from the hidden-cursor
/// frame.
#[test]
fn merged_ligature_cursor_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    // Same once-per-process discovery + $ATERM_FONT export as the sibling test.
    if merged_ligature_font().is_none() {
        eprintln!("SKIP: no merged-ligature font");
        return;
    }
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system font");
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
