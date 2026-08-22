// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GPU SCISSORED DIRTY-ROW REPAINT — the byte-identity gate.
//
// `GpuRenderer::present_input` re-encodes ONLY the rows that differ from the
// previous presented frame: LoadOp::Load over the persistent offscreen (which
// still holds the prior frame), a scissor over the dirty rows' bounding band,
// and instances built for the dirty rows only. The dirty set is the SHARED
// `aterm_render::compute_dirty_rows` — the SAME one the CPU damage path uses, so
// the GPU and CPU cannot diverge. The hard contract: the scissored offscreen
// must be BYTE-IDENTICAL to a fresh full GPU render of the same input.
//
// This test drives ONE reused GpuRenderer at FIXED dims through a multi-frame
// sequence (prompt, single-keystroke typing, blink toggles, cursor moves, wide
// CJK, combining marks, DECDWL, DECDHL, selection set/clear, scrollback scroll,
// full-screen TUI repaint). After EACH frame it reads back the scissored
// offscreen (`present_input_readback`, the exact present-path encode + a
// readback) and asserts the pixels `==` a FRESH full GPU render of that input on
// a SEPARATE GpuRenderer (the oracle pattern from `dirty_gate.rs`). It also
// asserts, via the scissor/full counters, that:
//   * the scissor path is ACTUALLY taken on the typing/cursor/wide/combining/
//     DECDWL frames AND on selection set/clear (row-level selection damage —
//     only rows whose selected span changed repaint), and
//   * the DECDHL / scroll frames correctly FELL BACK to full repaint (the
//     conservative always-correct path), so a re-shaded double-height /
//     scrollback frame can never leak a seam.
//
// Gated: no GPU or no system font ⇒ the test no-ops (returns).

// PHOSPHOR rain fixture (a real MatrixRain drive) for the animating-rain
// scissor test below; each consuming binary uses its own subset.
#[allow(dead_code)]
mod rain_common;

use std::time::Instant;

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_effects::matrix_rain::RainVisibility;
use aterm_gpu::GpuRenderer;
use aterm_render::{GlowQuad, RenderInput, Theme, premul_rgb};
use rain_common::RainScene;

const ROWS: usize = 10;
const COLS: usize = 32;

/// A fresh GpuRenderer (or skip-marker) at the suite's standard px/theme.
/// Blocks on the lazy fallback parses so the reused renderer and every fresh
/// oracle rasterize the CJK steps identically (no provisional `.notdef` race).
fn fresh_gpu() -> Option<GpuRenderer> {
    match GpuRenderer::new(18.0, Theme::default()) {
        Ok(mut g) => {
            g.debug_block_on_lazy_fallbacks();
            // The heat shimmer (bloom parity class) is wall-clock at present, so
            // two renders of one input never byte-agree with it on. Disable it
            // for this suite's byte-identity gates; its own scissor-path
            // identity is pinned in `heat_shimmer.rs` with a pinned phase.
            g.set_shimmer(false);
            Some(g)
        }
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            None
        }
    }
}

/// The ground truth: a brand-new GpuRenderer renders `input` with a FULL repaint
/// (Clear + every row) and reads it back. No scissor, no prior frame — exactly
/// the pixels the scissored offscreen must match.
fn fresh_render(input: &RenderInput, blink: bool, override_: Option<CursorStyle>) -> Vec<u32> {
    let mut g = fresh_gpu().expect("GPU was available a moment ago");
    let mut win = aterm_gpu::WindowGpu::new();
    g.set_cursor_blink_phase(blink);
    g.set_cursor_style_override(override_);
    g.render_input(&mut win, input, None).pixels
}

/// What a step expects of the repaint path for THAT frame.
#[derive(Clone, Copy, PartialEq)]
enum Path {
    /// Must take the SCISSORED dirty-row path.
    Scissor,
    /// Must FALL BACK to a full Clear+all-rows repaint.
    Full,
    /// Must take the E7 WHOLE-ROW SCROLL-BLIT RESCUE: `compute_dirty_rows` says
    /// `FullRepaint` (display_offset AND the absolute anchor both moved), but
    /// `scroll_blit_plan` recognises a rigid integer-row slide, so the offscreen's
    /// grid band is shifted and only the newly-exposed strip re-encodes. Strictly
    /// stronger than [`Path::Scissor`]: it asserts the scissor AND that the rescue
    /// — not a lucky small dirty set — is what produced it.
    ScrollRescue,
    /// Don't assert the path (e.g. the first frame, or a blink toggle whose
    /// hit/miss depends on the terminal's DECSCUSR default).
    Any,
}

struct Step {
    desc: &'static str,
    act: Box<dyn Fn(&mut Terminal)>,
    blink: bool,
    override_: Option<CursorStyle>,
    path: Path,
}

fn step(
    desc: &'static str,
    act: impl Fn(&mut Terminal) + 'static,
    blink: bool,
    override_: Option<CursorStyle>,
    path: Path,
) -> Step {
    Step {
        desc,
        act: Box::new(act),
        blink,
        override_,
        path,
    }
}

#[test]
fn gpu_scissor_repaint_byte_identical() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = Terminal::new(ROWS as u16, COLS as u16);

    let steps = vec![
        // 1. First paint: a shell prompt. First frame always FULL (no prior).
        step("prompt", |t| t.process(b"$ "), true, None, Path::Full),
        // 2. Re-present the SAME frame — reusable, zero dirty rows ⇒ scissor path
        //    (Load + empty band; preserves the prior frame).
        step("idle (unchanged)", |_| {}, true, None, Path::Scissor),
        // 3-7. Single-keystroke typing — each changes ONE row ⇒ SCISSOR.
        step("type 'l'", |t| t.process(b"l"), true, None, Path::Scissor),
        step("type 's'", |t| t.process(b"s"), true, None, Path::Scissor),
        step("type ' '", |t| t.process(b" "), true, None, Path::Scissor),
        step("type '-'", |t| t.process(b"-"), true, None, Path::Scissor),
        step("type 'a'", |t| t.process(b"a"), true, None, Path::Scissor),
        // 8. Idle after typing — scissor (zero dirty rows).
        step("idle after type", |_| {}, true, None, Path::Scissor),
        // 9. Blink toggle (no content change). Don't assert path: whether the
        //    cursor's shown-ness flips depends on the DECSCUSR style. Always
        //    byte-identical though.
        step("blink off", |_| {}, false, None, Path::Any),
        step("blink on", |_| {}, true, None, Path::Any),
        // 11. Newline then a cursor MOVE without content change (CUP) — the old +
        //     new cursor rows are dirty ⇒ scissor.
        step(
            "newline + text",
            |t| t.process(b"\r\nrow two text"),
            true,
            None,
            Path::Scissor,
        ),
        step(
            "cursor home",
            |t| t.process(b"\x1b[1;1H"),
            true,
            None,
            Path::Scissor,
        ),
        step(
            "cursor r3c5",
            |t| t.process(b"\x1b[3;5H"),
            true,
            None,
            Path::Scissor,
        ),
        // 14. Wide CJK on a fresh row — changes one row ⇒ scissor.
        step(
            "wide cjk",
            |t| t.process("\x1b[4;1H日本語".as_bytes()),
            true,
            None,
            Path::Scissor,
        ),
        // 15. Combining mark (é = e + U+0301) — changes one row ⇒ scissor.
        step(
            "combining é",
            |t| t.process("\x1b[5;1He\u{0301}".as_bytes()),
            true,
            None,
            Path::Scissor,
        ),
        // 16. DECDWL double-WIDTH row. Double-width stays within ONE row band, so
        //     it is REUSABLE — the changed row is dirty ⇒ scissor (byte-identical).
        step(
            "decdwl",
            |t| t.process(b"\x1b[6;1H\x1b#6WIDE"),
            true,
            None,
            Path::Scissor,
        ),
        // 17. DECDHL double-HEIGHT row. A DECDHL glyph spans TWO row bands, so the
        //     whole frame is NOT reusable ⇒ FULL repaint (the seam-safe fallback).
        step(
            "decdhl top",
            |t| t.process(b"\x1b[7;1H\x1b#3TALL"),
            true,
            None,
            Path::Full,
        ),
        // 18. Still double-height present ⇒ idle also FULL (no per-row reuse while
        //     a double-height row exists in either frame).
        step("idle with decdhl", |_| {}, true, None, Path::Full),
        // 19. Clear the double-height: rewrite row 7 single-size. Prior frame had a
        //     double-height row ⇒ NOT reusable ⇒ FULL.
        step(
            "clear decdhl",
            |t| t.process(b"\x1b[7;1H\x1b#5plain   "),
            true,
            None,
            Path::Full,
        ),
        // 20. Idle now (no double-height anywhere) ⇒ scissor again.
        step("idle no decdhl", |_| {}, true, None, Path::Scissor),
        // 21. Style override (unfocused HollowBlock) — cursor style changes; the
        //     cursor row is dirty ⇒ scissor (byte-identical).
        step(
            "focus lost",
            |_| {},
            true,
            Some(CursorStyle::HollowBlock),
            Path::Scissor,
        ),
        step("focus gained", |_| {}, true, None, Path::Scissor),
        // 23. Selection set — only the rows whose selected span changed are
        //     dirty (row-level selection damage) ⇒ scissor, byte-identical.
        step(
            "select",
            |t| {
                let sel = t.text_selection_mut();
                sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
                sel.update_selection(2, 6, SelectionSide::Right);
                sel.complete_selection();
            },
            true,
            None,
            Path::Scissor,
        ),
        // 24. Idle WITH a selection — selection unchanged, reusable ⇒ scissor.
        step("idle selected", |_| {}, true, None, Path::Scissor),
        // 25. Clear selection — only the previously-selected rows dirty ⇒ scissor.
        step(
            "clear selection",
            |t| t.text_selection_mut().clear(),
            true,
            None,
            Path::Scissor,
        ),
        // 26. Generate scrollback so there is history to scroll into.
        step(
            "run output",
            |t| {
                for n in 0..30 {
                    t.process(format!("\r\nfile{n} contents here").as_bytes());
                }
            },
            true,
            None,
            Path::Any,
        ),
        // 27. Scroll back into history. `display_offset` AND the absolute anchor
        //     both move, so `compute_dirty_rows` returns `FullRepaint` — and the E7
        //     rescue turns that verdict into a band shift plus an exposed-strip
        //     scissor, exactly as the CPU backend has always done for this frame
        //     class. Byte-identity is asserted for every step regardless, so this
        //     line is about WHICH path pays for the frame, not whether it is right.
        step(
            "scroll back",
            |t| t.scroll_display(3),
            true,
            None,
            Path::ScrollRescue,
        ),
        // 28. Idle scrolled — offset unchanged ⇒ scissor.
        step("idle scrolled", |_| {}, true, None, Path::Scissor),
        // 29. Scroll to bottom — the same rigid slide in the OTHER direction (the
        //     overshoot-apron side), so the same rescue.
        step(
            "scroll to bottom",
            |t| t.scroll_to_bottom(),
            true,
            None,
            Path::ScrollRescue,
        ),
        // 30. Full-screen TUI repaint (clear + redraw): MANY rows change at once.
        //     Reusable (same dims/offset/selection, no double-height) ⇒ scissor
        //     over the (large) dirty band — still byte-identical.
        step(
            "full tui repaint",
            |t| {
                t.process(b"\x1b[2J\x1b[H");
                for r in 0..ROWS {
                    t.process(
                        format!("\x1b[{};1Hline {r:02} ::::::::::::::::::", r + 1).as_bytes(),
                    );
                }
            },
            true,
            None,
            Path::Scissor,
        ),
        // 31. Idle on the TUI screen ⇒ scissor.
        step("idle tui", |_| {}, true, None, Path::Scissor),
        // 32. One keystroke on the TUI ⇒ scissor (one row).
        step(
            "type on tui",
            |t| t.process(b"\x1b[1;1HX"),
            true,
            None,
            Path::Scissor,
        ),
    ];

    let mut scissor_seen = 0u64;
    let mut full_seen = 0u64;
    let mut rescue_seen = 0u64;

    for (i, s) in steps.iter().enumerate() {
        (s.act)(&mut term);
        gpu.set_cursor_blink_phase(s.blink);
        gpu.set_cursor_style_override(s.override_);

        let input = term.cell_frame(ROWS, COLS);

        let scissor_before = gpu.scissor_taken();
        let full_before = gpu.full_repaints();
        let rescues_before = gpu.scroll_rescues();

        // The scissored present-path encode + readback (the path under test).
        let got = gpu.present_input_readback(&mut win, &input).pixels;

        let took_scissor = gpu.scissor_taken() > scissor_before;
        let took_full = gpu.full_repaints() > full_before;
        let took_rescue = gpu.scroll_rescues() > rescues_before;
        assert!(
            took_scissor ^ took_full,
            "step {i} ({}): exactly one of scissor/full must be taken",
            s.desc
        );

        // (a) BYTE-IDENTITY — the cardinal contract. Whether this frame scissored
        // or fell back, the offscreen pixels MUST equal a fresh full GPU render of
        // the same input + cursor state. On a scissor this proves the dirty band
        // is bit-identical AND the untouched rows were preserved verbatim.
        let oracle = fresh_render(&input, s.blink, s.override_);
        assert_eq!(
            got.len(),
            oracle.len(),
            "step {i} ({}): pixel count differs",
            s.desc
        );
        assert!(
            got == oracle,
            "step {i} ({}): {} pixels are NOT byte-identical to a fresh GPU render",
            s.desc,
            if took_scissor {
                "SCISSORED"
            } else {
                "full-repaint"
            },
        );

        // (b) the path must be what the step declares.
        match s.path {
            Path::Scissor => assert!(
                took_scissor,
                "step {i} ({}): expected the SCISSOR path but it fell back to full",
                s.desc
            ),
            Path::Full => assert!(
                took_full,
                "step {i} ({}): expected a FULL repaint but it took the scissor",
                s.desc
            ),
            Path::ScrollRescue => {
                assert!(
                    took_rescue,
                    "step {i} ({}): expected the E7 scroll-blit rescue, but the \
                     frame did not consult/accept the plan",
                    s.desc
                );
                assert!(
                    took_scissor,
                    "step {i} ({}): a rescued frame must encode as a SCISSOR",
                    s.desc
                );
            }
            Path::Any => {}
        }

        if took_scissor {
            scissor_seen += 1;
        } else {
            full_seen += 1;
        }
        if took_rescue {
            rescue_seen += 1;
        }
        eprintln!(
            "step {i:2} {:<20} path={} (scissor={}, full={})",
            s.desc,
            if took_scissor { "SCISSOR" } else { "FULL   " },
            gpu.scissor_taken(),
            gpu.full_repaints(),
        );
    }

    // The optimisation must be EXERCISED (many scissor frames) and the fallback
    // must be REACHED (DECDHL / selection / scroll).
    assert!(
        scissor_seen >= 10,
        "scissor path barely fired ({scissor_seen}) — not exercised"
    );
    // The full-repaint fallback is now reached by the first frame and by the
    // DECDHL trio only: the two scroll steps that used to pad this count are the
    // E7 rescue's whole point, and `rescue_seen` below is the guard that they did
    // not silently fall back INTO this count instead.
    assert!(
        full_seen >= 4,
        "full-repaint fallback barely fired ({full_seen}) — not exercised"
    );
    assert!(
        rescue_seen >= 2,
        "the E7 scroll-blit rescue barely fired ({rescue_seen}) — not exercised"
    );
    assert_eq!(
        gpu.scissor_taken() + gpu.full_repaints(),
        steps.len() as u64,
        "every frame must be exactly one of scissor/full",
    );
    eprintln!(
        "scissor-repaint: {scissor_seen} scissor frames ({rescue_seen} of them \
         scroll-blit rescues), {full_seen} full repaints"
    );
}

/// A scissored frame immediately followed by a ONE-CELL change must repaint
/// correctly: the dirty row is re-encoded over the preserved prior frame and the
/// result matches a fresh render of the CHANGED input — i.e. the offscreen does
/// not "stick" on the stale prior pixels, and untouched rows are not corrupted.
#[test]
fn gpu_scissor_one_cell_change_preserves_other_rows() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    // Two rows of content so we can prove the UNCHANGED row survives a scissor.
    term.process(b"first row\r\nsecond row");
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    // Frame 1: first paint — FULL (no prior frame).
    let in1 = term.cell_frame(ROWS, COLS);
    let _ = gpu.present_input_readback(&mut win, &in1);
    assert_eq!(gpu.full_repaints(), 1, "first frame must be a full repaint");
    assert_eq!(gpu.scissor_taken(), 0);

    // Frame 2: change ONE cell on row 0 ('first' → 'First'). Must SCISSOR and
    // match a fresh render of the changed input — not the stale frame.
    term.process(b"\x1b[1;1HF");
    let in2 = term.cell_frame(ROWS, COLS);
    let scissor_before = gpu.scissor_taken();
    let got2 = gpu.present_input_readback(&mut win, &in2).pixels;
    assert!(
        gpu.scissor_taken() > scissor_before,
        "one-cell change must take the scissor"
    );
    let oracle2 = fresh_render(&in2, true, None);
    assert!(
        got2 == oracle2,
        "scissored one-cell change diverges from a fresh render"
    );

    // Row 1 ("second row") was NOT dirty: prove its pixels survived the scissor
    // by checking they equal the fresh render's row-1 band exactly (they do, since
    // the whole frame matched — but assert the band explicitly for clarity).
    let (cw, ch) = gpu.cell_size();
    let w = COLS * cw;
    let band1 = (ch * w)..(2 * ch * w);
    assert!(
        got2[band1.clone()] == oracle2[band1],
        "the untouched row-1 band was corrupted by the scissored repaint",
    );

    // Frame 3: idle — zero dirty rows ⇒ scissor, byte-identical, re-presents the
    // exact prior frame.
    let in3 = term.cell_frame(ROWS, COLS);
    let got3 = gpu.present_input_readback(&mut win, &in3).pixels;
    assert!(
        got3 == got2,
        "idle scissor frame must re-present the prior frame verbatim"
    );
    assert!(
        got3 == fresh_render(&in3, true, None),
        "idle scissor diverges from a fresh render"
    );
}

/// Selection foreground/background changes recolor combining-mark ink without
/// changing the grid or selection span. The shared damage set must include the
/// selected row's predecessor because ordinary glyph rasters may overhang
/// upward into it; otherwise the GPU scissor preserves stale accent pixels.
#[test]
fn gpu_selection_color_scissor_covers_upward_combining_ink() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();
    let mut term = Terminal::new(ROWS as u16, COLS as u16);

    term.process(b"\x1b[?25l");
    term.process("\x1b[4;1HA\u{0302} selected text".as_bytes());
    term.process(
        b"\x1b]17;rgb:20/30/40\x1b\\\
          \x1b]19;rgb:d0/e0/f0\x1b\\",
    );
    {
        let selection = term.text_selection_mut();
        selection.start_selection(3, 0, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(3, 10, SelectionSide::Right);
        selection.complete_selection();
    }

    let first = term.cell_frame(ROWS, COLS);
    let _ = gpu.present_input_readback(&mut win, &first);
    assert_eq!(gpu.full_repaints(), 1, "first frame must repaint fully");

    // Stationary span, live OSC colors only: this must remain an optimized
    // scissored update while matching a fresh full render byte-for-byte.
    term.process(
        b"\x1b]17;rgb:60/20/70\x1b\\\
          \x1b]19;rgb:f8/a0/40\x1b\\",
    );
    let changed = term.cell_frame(ROWS, COLS);
    let scissor_before = gpu.scissor_taken();
    let got = gpu.present_input_readback(&mut win, &changed).pixels;
    assert!(
        gpu.scissor_taken() > scissor_before,
        "stationary selection color change must take the scissor path"
    );
    assert_eq!(
        got,
        fresh_render(&changed, true, None),
        "selection-color scissor left stale upward combining-mark ink"
    );
}

/// A theme change re-themes the selection band / idle cursor / padding — pixels
/// that are NOT cell content, so the dirty-row diff alone would leave them stale
/// on an idle GPU pane. `WindowGpu::invalidate_present` (called by the gpu-web
/// `set_theme` path) must drop the prior-frame validity so the NEXT present is a
/// FULL repaint even for a frame that would otherwise scissor (zero dirty rows).
#[test]
fn gpu_invalidate_present_forces_full_repaint() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ themed prompt");
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    // Frame 1: first paint — FULL (no prior frame).
    let input = term.cell_frame(ROWS, COLS);
    let _ = gpu.present_input_readback(&mut win, &input);
    assert_eq!(gpu.full_repaints(), 1, "first frame must be a full repaint");

    // Frame 2: idle, unchanged ⇒ would normally SCISSOR (zero dirty rows).
    let scissor_before = gpu.scissor_taken();
    let _ = gpu.present_input_readback(&mut win, &input);
    assert!(
        gpu.scissor_taken() > scissor_before,
        "an unchanged idle frame must scissor without invalidation"
    );

    // Invalidate (the theme-change hook), then present the SAME idle frame: it
    // MUST fall back to a full repaint despite zero dirty rows, and the pixels
    // must still equal a fresh full render of that input.
    win.invalidate_present();
    let full_before = gpu.full_repaints();
    let got = gpu.present_input_readback(&mut win, &input).pixels;
    assert!(
        gpu.full_repaints() > full_before,
        "invalidate_present must force the next present to a FULL repaint"
    );
    assert!(
        got == fresh_render(&input, true, None),
        "the forced full repaint must be byte-identical to a fresh render"
    );
}

/// The LUMEN aurora with GPU BLOOM enabled (both defaults) must ride the
/// SCISSORED dirty-row path, not force a full-grid repaint — and stay
/// byte-identical to a fresh full render. The bloom halo is additive light
/// composited over the offscreen: on a scissored frame the dirty set is widened
/// to every row the halo can touch (glow rows ± the blur margin, gap-filled) and
/// the composite is clipped to the band, so the halo can neither accumulate on
/// Load-preserved rows nor be clipped visibly (outside the widened band it is
/// exactly zero). Three present-path frames prove it: glow appears (full — first
/// frame), glow MOVES + one keystroke (must SCISSOR, bytes == fresh render), glow
/// FADES to empty (must SCISSOR, the halo fringe fully erased).
#[test]
fn bloom_glow_rides_the_scissor_path_byte_identical() {
    let Some(mut gpu) = fresh_gpu() else { return };
    assert!(gpu.bloom_enabled(), "bloom must be ON (the default) here");
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ prompt");
    term.process(b"\x1b[5;1Hmiddle row text");
    term.process(b"\x1b[9;1Hbottom row text");
    term.process(b"\x1b[1;9H"); // park the cursor after the prompt
    let (cw, ch) = gpu.cell_size();
    let comet = |row: usize, cols: std::ops::Range<usize>| -> Vec<GlowQuad> {
        cols.map(|c| GlowQuad {
            row: row as u16,
            x: (c * cw) as u16,
            y: (row * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(0x0050_FA7B, (120 + c * 10).min(230) as u8),
        })
        .collect()
    };

    // Frame A: first paint with a glowing comet on row 4 — FULL (no prior frame).
    let mut in_a = term.cell_frame(ROWS, COLS);
    in_a.cursor_glow_add = comet(4, 2..8);
    let _ = gpu.present_input_readback(&mut win, &in_a);
    assert_eq!(gpu.full_repaints(), 1, "first frame must be a full repaint");

    // Frame B: one keystroke + the comet moves down a row — the default typing
    // tick. PRE-FIX this forced a FULL repaint whenever bloom was enabled.
    term.process(b"x");
    let mut in_b = term.cell_frame(ROWS, COLS);
    in_b.cursor_glow_add = comet(5, 4..10);
    let scissor_before = gpu.scissor_taken();
    let got_b = gpu.present_input_readback(&mut win, &in_b).pixels;
    assert!(
        gpu.scissor_taken() > scissor_before,
        "typing with the bloom aurora alive must take the SCISSOR path"
    );
    assert!(
        got_b == fresh_render(&in_b, true, None),
        "scissored bloom frame is NOT byte-identical to a fresh full render"
    );

    // Frame C: the comet fades out (empty glow). The prior frame's halo — which
    // bled BEYOND the glow rows — must be fully erased by the widened band.
    let mut in_c = term.cell_frame(ROWS, COLS);
    in_c.cursor_glow_add.clear();
    let scissor_before = gpu.scissor_taken();
    let got_c = gpu.present_input_readback(&mut win, &in_c).pixels;
    assert!(
        gpu.scissor_taken() > scissor_before,
        "the glow fade-out frame must take the SCISSOR path"
    );
    assert!(
        got_c == fresh_render(&in_c, true, None),
        "halo residue survived the fade-out scissor (or the band diverged)"
    );

    // Frame D: idle with no glow — back to the cheapest scissor (zero dirty rows).
    let got_d = gpu.present_input_readback(&mut win, &in_c).pixels;
    assert!(
        got_d == got_c,
        "idle after fade-out must re-present the prior frame verbatim"
    );
}

/// NO-FLASH LAW (TYPING-2 deferral retired): the present-time bloom halo
/// composites on EVERY bloom frame — `input_hot` keystroke echoes included. The
/// old deferral skipped the halo on exactly the echo frame, which was invisible
/// while bloom was an opt-in rarity but BLINKED the halo off once per keystroke
/// with bloom default-on (the reported "little flash when I type"). Same content,
/// one flag: hot and settle frames must now present IDENTICAL pixels — the
/// per-keystroke luminance dip is structurally impossible. Gated: no GPU/font ⇒
/// no-op.
#[test]
fn input_hot_presents_the_same_halo_as_settle() {
    let Some(mut gpu) = fresh_gpu() else { return };
    assert!(gpu.bloom_enabled(), "bloom must be ON (the default) here");
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);
    let (cw, ch) = gpu.cell_size();
    let comet = |row: usize, cols: std::ops::Range<usize>| -> Vec<GlowQuad> {
        cols.map(|c| GlowQuad {
            row: row as u16,
            x: (c * cw) as u16,
            y: (row * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(0x0050_FA7B, (120 + c * 10).min(230) as u8),
        })
        .collect()
    };
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ prompt");
    term.process(b"\x1b[1;9H");
    let mut base = term.cell_frame(ROWS, COLS);
    base.cursor_glow_add = comet(4, 2..8);

    let lum = |px: &[u32]| -> u64 {
        px.iter()
            .map(|&p| ((p & 0xff) + ((p >> 8) & 0xff) + ((p >> 16) & 0xff)) as u64)
            .sum()
    };

    // Settle frame (input_hot = false): the halo is composited.
    let mut settle = base.clone();
    settle.input_hot = false;
    let mut win_s = aterm_gpu::WindowGpu::new();
    let px_settle = gpu.present_input_readback(&mut win_s, &settle).pixels;

    // Hot frame (input_hot = true): identical content — and now an IDENTICAL frame.
    let mut hot = base.clone();
    hot.input_hot = true;
    let mut win_h = aterm_gpu::WindowGpu::new();
    let px_hot = gpu.present_input_readback(&mut win_h, &hot).pixels;

    assert!(lum(&px_hot) > 0, "the comet still presents on a hot frame");
    assert!(
        px_hot == px_settle,
        "hot and settle frames must present byte-identical pixels — a keystroke \
         must never blink the halo (hot lum {} vs settle lum {})",
        lum(&px_hot),
        lum(&px_settle)
    );
    // NON-VACUITY: the halo is really in both — a bloom-off render of the same
    // content is strictly dimmer than the haloed frames above.
    gpu.set_bloom(false);
    let mut win_n = aterm_gpu::WindowGpu::new();
    let lum_nobloom = lum(&gpu.present_input_readback(&mut win_n, &settle).pixels);
    assert!(
        lum_nobloom < lum(&px_settle),
        "bloom-off must be dimmer (nobloom {lum_nobloom} vs settle {})",
        lum(&px_settle)
    );
}

/// LOAD ROBUSTNESS (the point of the change): the GPU comet bloom halo now
/// composites at PRESENT time over a throwaway copy of the clean offscreen, so a
/// scissored frame's dirty set is NO LONGER widened by the halo penumbra and
/// band-FILLED across the whole span it touches. A moving comet near the BOTTOM +
/// one keystroke at the TOP therefore rebuilds only the few actually-changed rows,
/// not the ~full-grid band the old in-offscreen bloom forced every aurora tick.
/// The gate: the scissored encode builds FAR fewer instances than a full-grid
/// repaint of the same frame (pre-fix they were ~equal — the band-fill spanned the
/// keystroke row through the comet row). Gated: no GPU/font ⇒ no-op.
#[test]
fn bloom_scissor_dirty_band_stays_proportional() {
    let Some(mut gpu) = fresh_gpu() else { return };
    assert!(gpu.bloom_enabled(), "bloom must be ON (the default) here");
    let (rows, cols) = (40usize, 80usize);
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);
    let (cw, ch) = gpu.cell_size();

    // Fill EVERY row so a full repaint builds a large instance set.
    let mut term = Terminal::new(rows as u16, cols as u16);
    for r in 0..rows {
        term.process(
            format!(
                "\x1b[{};1Hrow {r:02} the quick brown fox jumps over the lazy dog",
                r + 1
            )
            .as_bytes(),
        );
    }
    let comet = |row: usize| -> Vec<GlowQuad> {
        (2..8)
            .map(|c| GlowQuad {
                row: row as u16,
                x: (c * cw) as u16,
                y: (row * ch) as u16,
                w: cw as u16,
                h: ch as u16,
                color: premul_rgb(0x0050_FA7B, 200),
            })
            .collect()
    };

    // Frame 0: prime (FULL — first frame) with a comet near the BOTTOM.
    let mut in0 = term.cell_frame(rows, cols);
    in0.cursor_glow_add = comet(35);
    gpu.present_encode_poll(&mut win, &in0);

    // Frame 1: one keystroke at the TOP + the comet moves one row down. Only rows
    // {0, 35, 36} differ — NOT the whole 0..=36 span the old halo band-fill forced.
    term.process(b"\x1b[1;1HX");
    let mut in1 = term.cell_frame(rows, cols);
    in1.cursor_glow_add = comet(36);
    let scissor_before = gpu.scissor_taken();
    gpu.present_encode_poll(&mut win, &in1);
    assert!(
        gpu.scissor_taken() > scissor_before,
        "a moving comet + keystroke with bloom ON must take the SCISSOR path"
    );
    let scissored_inst = gpu.last_instances();

    // The whole-grid instance count for the SAME frame (a fresh FULL render).
    let mut full = fresh_gpu().expect("GPU available a moment ago");
    full.set_cursor_blink_phase(true);
    let mut full_win = aterm_gpu::WindowGpu::new();
    let _ = full.render_input(&mut full_win, &in1, None);
    let full_inst = full.last_instances();

    // ~3 changed rows out of 40 ⇒ the scissored encode builds a small fraction of a
    // full-grid rebuild. (Pre-fix the bloom band-fill spanned rows 0..=36, so the
    // scissored count was ~the full count and typing under a live comet was O(grid).)
    assert!(
        scissored_inst.saturating_mul(4) < full_inst,
        "scissored bloom frame built {scissored_inst} instances vs {full_inst} full — \
         the dirty band was NOT kept proportional (halo band-fill regressed?)"
    );
    eprintln!(
        "bloom scissor proportionality: scissored={scissored_inst} instances, full={full_inst} \
         ({:.1}x fewer)",
        full_inst as f64 / scissored_inst.max(1) as f64,
    );
}

/// PHOSPHOR rain through the SCISSORED present path (design §10): an
/// ANIMATING field — genuine `MatrixRain` emissions, tick after tick — must
/// ride the dirty-row scissor (rain damage is row-scoped: the per-row
/// merge-diff marks changed rows, the band fill rebuilds every rain quad in
/// the band, so the incremental frame can never ghost or double-blend) and
/// stay BYTE-IDENTICAL to a fresh full render on EVERY frame, including a
/// mid-stream keystroke, the settled (fp-stable) frame, and the drain to
/// empty (the vacated pixels fully erased).
#[test]
fn gpu_scissor_animating_rain_byte_identical() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l$ phosphor"); // hidden cursor: no blink in the mix
    let base = term.cell_frame(ROWS, COLS);
    let mut scene = RainScene::new(ROWS, COLS, gpu.cell_size(), &base);
    scene.drive_until_raining();

    // Frame 0: first present — FULL (no prior frame), byte-identical.
    let mut in0 = term.cell_frame(ROWS, COLS);
    scene.apply(&mut in0);
    let mut prev_px = gpu.present_input_readback(&mut win, &in0).pixels;
    assert_eq!(gpu.full_repaints(), 1, "first frame must be a full repaint");
    assert!(
        prev_px == fresh_render(&in0, true, None),
        "the priming rain frame diverges from a fresh full render"
    );

    // Animating frames: each engine tick moves the field. Every frame must
    // take the SCISSOR (rain-only damage is reusable) and match a fresh full
    // render byte-for-byte. Step 4 types a character so a combined
    // text+rain change rides the same scissored frame.
    let mut prev_quads = in0.rain_quads.clone();
    let mut moved = 0usize;
    let mut haloed = 0usize;
    for i in 0..10 {
        scene.tick();
        if i == 4 {
            term.process(b"x");
        }
        let mut input = term.cell_frame(ROWS, COLS);
        scene.apply(&mut input);
        let scissor_before = gpu.scissor_taken();
        let got = gpu.present_input_readback(&mut win, &input).pixels;
        assert!(
            gpu.scissor_taken() > scissor_before,
            "animating rain frame {i} must take the SCISSOR path"
        );
        assert!(
            got == fresh_render(&input, true, None),
            "animating rain frame {i} is NOT byte-identical to a fresh full render"
        );
        if input.rain_quads != prev_quads {
            moved += 1;
        }
        if !input.rain_add.is_empty() {
            haloed += 1;
        }
        prev_quads.clone_from(&input.rain_quads);
        prev_px = got;
    }
    // Non-vacuity: the field genuinely animated, and the additive halos rode
    // the scissored path on at least some frames.
    assert!(
        moved >= 5,
        "the rain barely animated ({moved}/10 changed frames)"
    );
    assert!(
        haloed >= 1,
        "no frame carried bright-head halos (additive path unexercised)"
    );

    // Settled frame: the same engine tick re-emits the identical field —
    // zero dirty rows, scissor, verbatim re-present.
    scene.emit();
    let mut in_s = term.cell_frame(ROWS, COLS);
    scene.apply(&mut in_s);
    let scissor_before = gpu.scissor_taken();
    let got_s = gpu.present_input_readback(&mut win, &in_s).pixels;
    assert!(
        gpu.scissor_taken() > scissor_before,
        "the settled rain frame must take the scissor (zero dirty rows)"
    );
    assert!(
        got_s == prev_px,
        "the settled rain frame must re-present the prior frame verbatim"
    );

    // Drain to empty (hidden pane): the vacated rain rows must be re-cleared
    // through the scissor — the no-ghosting rule for the per-row slice diff.
    scene.engine.set_visibility(RainVisibility::Hidden);
    let fp = scene.tick();
    assert_eq!(fp, 0, "hidden visibility must drain the field to empty");
    let mut in_d = term.cell_frame(ROWS, COLS);
    scene.apply(&mut in_d);
    let scissor_before = gpu.scissor_taken();
    let got_d = gpu.present_input_readback(&mut win, &in_d).pixels;
    assert!(
        gpu.scissor_taken() > scissor_before,
        "the rain fade-out frame must take the scissor path"
    );
    assert!(
        got_d == fresh_render(&in_d, true, None),
        "rain residue survived the fade-out scissor (ghost pixels in the vacated band)"
    );
    assert!(
        got_d != got_s,
        "draining must visibly change the frame (non-vacuous)"
    );
}

/// Diagnostic (run with `--ignored --nocapture`): the changed-frame GPU
/// encode/instance-build cost for a 1-ROW change at 50x200 via the SCISSORED
/// present path vs a FULL repaint of the same frame. Both read the whole texture
/// back (constant cost), so the delta is the scissor's encode/fill saving. Not an
/// assertion — prints the reduction.
#[test]
#[ignore = "diagnostic benchmark; run with --ignored --nocapture"]
fn gpu_scissor_changed_frame_cost() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (50usize, 200usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Fill every row so a full repaint is non-trivial.
    for r in 0..rows {
        term.process(format!("\x1b[{};1Hline {r:02} ", r + 1).as_bytes());
        term.process(b"the quick brown fox jumps over the lazy dog 0123456789 abcdef");
    }
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    // Prime the present path (first frame is a full repaint, fills present_prev).
    let in0 = term.cell_frame(rows, cols);
    gpu.present_encode_poll(&mut win, &in0);

    const N: u32 = 500;

    // Measure the ENCODE + instance-build + GPU fill only (no readback — it is
    // scope-independent and would swamp the scissor's saving).
    //
    // SCISSORED 1-row change: toggle a single char on row 0 each iter — exactly
    // one dirty row ⇒ a one-band scissor + one row's instances.
    let scissor_before = gpu.scissor_taken();
    let t = Instant::now();
    for i in 0..N {
        let ch = if i % 2 == 0 { b'A' } else { b'B' };
        term.process(b"\x1b[1;1H");
        term.process(&[ch]);
        let input = term.cell_frame(rows, cols);
        gpu.present_encode_poll(&mut win, &input);
    }
    let scissor_us = t.elapsed().as_secs_f64() * 1e6 / f64::from(N);
    let scissor_inst = gpu.last_instances();
    assert_eq!(
        gpu.scissor_taken() - scissor_before,
        u64::from(N),
        "all iters should scissor"
    );

    // FULL repaint of the SAME 1-row-change frames on a SEPARATE renderer. Toggle
    // the display_offset every frame so `compute_dirty_rows` returns FullRepaint
    // (a scrollback change is never reusable) — this forces the full Clear+all-
    // rows encode for the SAME screen, isolating the repaint scope.
    let mut full = fresh_gpu().expect("GPU available");
    let mut win_full = aterm_gpu::WindowGpu::new();
    full.set_cursor_blink_phase(true);
    let mut input_a = term.cell_frame(rows, cols);
    let mut input_b = input_a.clone();
    input_b.display_offset = 1; // a different offset ⇒ forced full repaint
    // Prime with B so the loop's first frame (A) already differs ⇒ every
    // strictly-alternating frame's offset differs from the prior ⇒ all full.
    full.present_encode_poll(&mut win_full, &input_b);
    let full_before = full.full_repaints();
    let t = Instant::now();
    for i in 0..N {
        let input = if i % 2 == 0 { &input_a } else { &input_b };
        full.present_encode_poll(&mut win_full, input);
    }
    let full_us = t.elapsed().as_secs_f64() * 1e6 / f64::from(N);
    let full_inst = full.last_instances();
    std::hint::black_box((&mut input_a, &mut input_b));
    assert_eq!(
        full.full_repaints() - full_before,
        u64::from(N),
        "all iters should full-repaint"
    );

    eprintln!(
        "1-row change @ {rows}x{cols} (encode only, no readback): \
         SCISSOR present = {scissor_us:.1} us/frame, \
         FULL repaint = {full_us:.1} us/frame, reduction = {:.2}x",
        full_us / scissor_us.max(0.0001),
    );
    eprintln!(
        "instances built: SCISSOR (1 dirty row) = {scissor_inst}, FULL ({rows} rows) = {full_inst}, \
         reduction = {:.1}x",
        full_inst as f64 / scissor_inst.max(1) as f64,
    );
}
