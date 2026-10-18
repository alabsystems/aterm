// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GPU scissored dirty-row repaint: the byte-identity gate.
// `GpuRenderer::present_input` re-encodes only rows that differ from the
// previous presented frame (`LoadOp::Load` over the persistent offscreen, a
// scissor over the dirty band, instances for dirty rows only), with the dirty
// set from the shared `aterm_render::compute_dirty_rows`. Every scissored
// offscreen must be byte-identical to a fresh full GPU render on a separate
// renderer, and each frame's declared path (scissor, full, scroll rescue) must
// be the one taken.
//
// Without a GPU it ends as could-not-run (`common::gpu`).

use crate::common;

use crate::rain_common::RainScene;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_effects::matrix_rain::RainVisibility;
use aterm_gpu::GpuRenderer;
use aterm_render::{GlowQuad, RenderInput, Theme, premul_rgb};

const ROWS: usize = 10;
const COLS: usize = 32;

/// A fresh renderer (no GPU: COULD NOT RUN) at the suite's px/theme, blocked on the lazy
/// fallback parses so reused and oracle renderers rasterize CJK identically.
fn fresh_gpu() -> GpuRenderer {
    let mut g = common::gpu(18.0, Theme::default());
    g.debug_block_on_lazy_fallbacks();
    // The heat shimmer is wall-clock at present, so it is off for these
    // byte-identity gates; `heat_shimmer.rs` pins its scissor path at a fixed phase.
    g.set_shimmer(false);
    g
}

/// Ground truth: a new renderer renders `input` with a full repaint and reads
/// it back.
fn fresh_render(input: &RenderInput, blink: bool, override_: Option<CursorStyle>) -> Vec<u32> {
    let mut g = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();
    g.set_cursor_blink_phase(blink);
    g.set_cursor_style_override(override_);
    g.render_input(&mut win, input, None).pixels
}

/// What a step expects of the repaint path for that frame.
#[derive(Clone, Copy, PartialEq)]
enum Path {
    /// Must take the scissored dirty-row path.
    Scissor,
    /// Must fall back to a full repaint.
    Full,
    /// Must take the whole-row scroll-blit rescue: `compute_dirty_rows` says
    /// `FullRepaint` but `scroll_blit_plan` sees a rigid integer-row slide, so
    /// the band is shifted and only the exposed strip re-encodes. Stronger than
    /// [`Path::Scissor`].
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
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = Terminal::new(ROWS as u16, COLS as u16);

    let steps = vec![
        // The first frame is always full.
        step("prompt", |t| t.process(b"$ "), true, None, Path::Full),
        // Zero dirty rows: Load + an empty band preserves the prior frame.
        step("idle (unchanged)", |_| {}, true, None, Path::Scissor),
        step("type 'l'", |t| t.process(b"l"), true, None, Path::Scissor),
        step("type 's'", |t| t.process(b"s"), true, None, Path::Scissor),
        step("type ' '", |t| t.process(b" "), true, None, Path::Scissor),
        step("type '-'", |t| t.process(b"-"), true, None, Path::Scissor),
        step("type 'a'", |t| t.process(b"a"), true, None, Path::Scissor),
        step("idle after type", |_| {}, true, None, Path::Scissor),
        // A blink toggle's path depends on the DECSCUSR style: only byte identity
        // is checked.
        step("blink off", |_| {}, false, None, Path::Any),
        step("blink on", |_| {}, true, None, Path::Any),
        // Cursor moves without content change dirty the old and new cursor rows.
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
        step(
            "wide cjk",
            |t| t.process("\x1b[4;1H日本語".as_bytes()),
            true,
            None,
            Path::Scissor,
        ),
        step(
            "combining é",
            |t| t.process("\x1b[5;1He\u{0301}".as_bytes()),
            true,
            None,
            Path::Scissor,
        ),
        // DECDWL stays within one row band, so the frame stays reusable.
        step(
            "decdwl",
            |t| t.process(b"\x1b[6;1H\x1b#6WIDE"),
            true,
            None,
            Path::Scissor,
        ),
        // A DECDHL glyph spans two row bands, so the frame is not reusable: full
        // repaint (the seam-safe fallback).
        step(
            "decdhl top",
            |t| t.process(b"\x1b[7;1H\x1b#3TALL"),
            true,
            None,
            Path::Full,
        ),
        // No per-row reuse while either frame has a double-height row.
        step("idle with decdhl", |_| {}, true, None, Path::Full),
        step(
            "clear decdhl",
            |t| t.process(b"\x1b[7;1H\x1b#5plain   "),
            true,
            None,
            Path::Full,
        ),
        step("idle no decdhl", |_| {}, true, None, Path::Scissor),
        // A cursor style override (unfocused HollowBlock) dirties the cursor row.
        step(
            "focus lost",
            |_| {},
            true,
            Some(CursorStyle::HollowBlock),
            Path::Scissor,
        ),
        step("focus gained", |_| {}, true, None, Path::Scissor),
        // Row-level selection damage: only rows whose selected span changed are
        // dirty.
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
        step("idle selected", |_| {}, true, None, Path::Scissor),
        step(
            "clear selection",
            |t| t.text_selection_mut().clear(),
            true,
            None,
            Path::Scissor,
        ),
        // History to scroll into.
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
        // Scrolling into history hides the live cursor, so the retained offscreen
        // is not a rigid band shift: full repaint.
        step(
            "scroll back",
            |t| t.scroll_display(3),
            true,
            None,
            Path::Full,
        ),
        step("idle scrolled", |_| {}, true, None, Path::Scissor),
        // Between history viewports the cursor is hidden on both sides, so these
        // are rigid slides: the rescue in both directions.
        step(
            "scroll farther back",
            |t| t.scroll_display(2),
            true,
            None,
            Path::ScrollRescue,
        ),
        step(
            "scroll forward in history",
            |t| t.scroll_display(-2),
            true,
            None,
            Path::ScrollRescue,
        ),
        // Back in live view the cursor shows again: the inverse transition, full.
        step(
            "scroll to bottom",
            |t| t.scroll_to_bottom(),
            true,
            None,
            Path::Full,
        ),
        // A full-screen TUI repaint is still reusable: a scissor over a large band.
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
        step("idle tui", |_| {}, true, None, Path::Scissor),
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

        // (a) Byte identity: scissored or not, the offscreen equals a fresh full
        // render of the same input and cursor state.
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

    // The scissor must be exercised and the full fallback reached.
    assert!(
        scissor_seen >= 10,
        "scissor path barely fired ({scissor_seen}) — not exercised"
    );
    // Full repaints: the first frame, the DECDHL trio, and the two
    // live/history cursor-visibility boundaries. History-to-history scrolls stay
    // rescues (`rescue_seen`).
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

/// A scissored frame followed by a one-cell change repaints correctly: the
/// result matches a fresh render of the changed input and untouched rows are
/// not corrupted.
#[test]
fn gpu_scissor_one_cell_change_preserves_other_rows() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"first row\r\nsecond row");
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let in1 = term.cell_frame(ROWS, COLS);
    let _ = gpu.present_input_readback(&mut win, &in1);
    assert_eq!(gpu.full_repaints(), 1, "first frame must be a full repaint");
    assert_eq!(gpu.scissor_taken(), 0);

    // One cell on row 0 changes ('first' -> 'First').
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

    // Row 1 was not dirty; its band must still equal the fresh render.
    let (cw, ch) = gpu.cell_size();
    let w = COLS * cw;
    let band1 = (ch * w)..(2 * ch * w);
    assert!(
        got2[band1.clone()] == oracle2[band1],
        "the untouched row-1 band was corrupted by the scissored repaint",
    );

    // Idle, zero dirty rows: re-presents the prior frame.
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

/// Selection fg/bg changes recolour combining-mark ink without changing the
/// grid or selection span. The damage set must include the selected row's
/// predecessor, since glyphs may overhang upward into it.
#[test]
fn gpu_selection_color_scissor_covers_upward_combining_ink() {
    let mut gpu = fresh_gpu();
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

    // Stationary span, live OSC colours only: still a scissored update.
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

/// A theme change re-themes non-content pixels (selection band, idle cursor,
/// padding), so `WindowGpu::invalidate_present` must make the next present a
/// full repaint even with zero dirty rows.
#[test]
fn gpu_invalidate_present_forces_full_repaint() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ themed prompt");
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let input = term.cell_frame(ROWS, COLS);
    let _ = gpu.present_input_readback(&mut win, &input);
    assert_eq!(gpu.full_repaints(), 1, "first frame must be a full repaint");

    let scissor_before = gpu.scissor_taken();
    let _ = gpu.present_input_readback(&mut win, &input);
    assert!(
        gpu.scissor_taken() > scissor_before,
        "an unchanged idle frame must scissor without invalidation"
    );

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

/// The bloom halo composites on every bloom frame, keystroke echoes
/// (`input_hot`) included: hot and settle frames of the same content present
/// identical pixels, so the halo never blinks while typing.
#[test]
fn input_hot_presents_the_same_halo_as_settle() {
    let mut gpu = fresh_gpu();
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
            // Additive light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: premul_rgb(0x0050_FA7B, (120 + c * 10).min(230) as u8),
            alpha2: 0,
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

    let mut settle = base.clone();
    settle.input_hot = false;
    let mut win_s = aterm_gpu::WindowGpu::new();
    let px_settle = gpu.present_input_readback(&mut win_s, &settle).pixels;

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
    // Non-vacuity: a bloom-off render of the same content is strictly dimmer.
    gpu.set_bloom(false);
    let mut win_n = aterm_gpu::WindowGpu::new();
    let lum_nobloom = lum(&gpu.present_input_readback(&mut win_n, &settle).pixels);
    assert!(
        lum_nobloom < lum(&px_settle),
        "bloom-off must be dimmer (nobloom {lum_nobloom} vs settle {})",
        lum(&px_settle)
    );
}

/// The bloom halo composites at present time over a copy of the offscreen, so
/// it does not widen the dirty set: a comet near the bottom plus a keystroke at
/// the top rebuilds only the changed rows, far fewer instances than a
/// full-grid repaint.
#[test]
#[cfg(wgpu_arm)]
fn bloom_scissor_dirty_band_stays_proportional() {
    let mut gpu = fresh_gpu();
    assert!(gpu.bloom_enabled(), "bloom must be ON (the default) here");
    let (rows, cols) = (40usize, 80usize);
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);
    let (cw, ch) = gpu.cell_size();

    // Fill every row so a full repaint builds a large instance set.
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
                // Additive light (see `GlowQuad::alpha`).
                alpha: 0,
                color2: premul_rgb(0x0050_FA7B, 200),
                alpha2: 0,
            })
            .collect()
    };

    // Frame 0: prime (the full first frame) with a comet near the bottom.
    let mut in0 = term.cell_frame(rows, cols);
    in0.cursor_glow_add = comet(35);
    gpu.present_encode_poll(&mut win, &in0);

    // Frame 1: one keystroke at the top + the comet moves one row down. Only rows
    // {0, 35, 36} differ.
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

    let mut full = fresh_gpu();
    full.set_cursor_blink_phase(true);
    let mut full_win = aterm_gpu::WindowGpu::new();
    let _ = full.render_input(&mut full_win, &in1, None);
    let full_inst = full.last_instances();

    // About 3 of 40 rows changed.
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

/// Animating rain (real `MatrixRain` emissions) rides the scissored path (rain
/// damage is row-scoped and the band fill rebuilds every quad in the band) and
/// stays byte-identical to a fresh full render on every frame, including a
/// keystroke, the settled frame and the drain to empty.
#[test]
fn gpu_scissor_animating_rain_byte_identical() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l$ phosphor"); // hidden cursor: no blink in the mix
    let base = term.cell_frame(ROWS, COLS);
    let mut scene = RainScene::new(ROWS, COLS, gpu.cell_size(), &base);
    scene.drive_until_raining();

    let mut in0 = term.cell_frame(ROWS, COLS);
    scene.apply(&mut in0);
    let mut prev_px = gpu.present_input_readback(&mut win, &in0).pixels;
    assert_eq!(gpu.full_repaints(), 1, "first frame must be a full repaint");
    assert!(
        prev_px == fresh_render(&in0, true, None),
        "the priming rain frame diverges from a fresh full render"
    );

    // Every tick takes the scissor and matches a fresh render; tick 4 also types.
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
    // Non-vacuity: the field animated, and halos rode the scissored path.
    assert!(
        moved >= 5,
        "the rain barely animated ({moved}/10 changed frames)"
    );
    assert!(
        haloed >= 1,
        "no frame carried bright-head halos (additive path unexercised)"
    );

    // Settled frame: re-emitting the same tick gives zero dirty rows and a
    // verbatim re-present.
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
