// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The linchpin correctness gate for the damage-tracking fast path in
// `Renderer::render_input_cached`. The optimization reuses the previous
// frame's pixel buffer and only re-renders changed rows (plus the cursor
// rows), returning the cached frame untouched when nothing changed. THE
// OUTPUT MUST BE BYTE-IDENTICAL to a full repaint for every input — this
// test proves it.
//
// Method: drive ONE Terminal through a long sequence of mutations (typing,
// backspace, cursor moves, SGR/colour changes, wide CJK, combining marks,
// DECDWL double-width, DECDHL double-height, scrollback display-offset changes,
// selection set/extend/clear, blink-phase toggles, cursor-style override,
// resize). After EACH mutation, render the extracted `RenderInput` two ways:
//   - through a PERSISTENT renderer PLUS its persistent per-window
//     `WindowCpu` damage cache, via `render_input_cached` — the exact entry
//     the shipping CPU present drives (`present_input_scratch` →
//     `render_input_cached(&mut ws.cpu_cache, ..)`), so the cache is warm
//     and the fast path is truly exercised — and
//   - through a FRESH renderer that has never rendered before (always the
//     full repaint path), via the owned `render_input` entry.
// Then assert `damaged.pixels == full.pixels`, pixel for pixel. Any divergence
// is a visual regression and fails the build.
//
// VACUOUSNESS PIN (the regression this warm arm repairs): when the damage
// cache was externalized into `WindowCpu`, `render_input` became
// throwaway-cache-per-call — unconditionally a full repaint — and this
// test's warm arm, still calling it, silently degraded to FULL-vs-FULL:
// byte-parity asserts that could never fail, a speedup print reading ~1.0x,
// and the fast path it documents untested here. The warm arm now drives
// `render_input_cached` over a persistent `WindowCpu`, and each test PINS
// via the `DamageRig` outcome tally that the row-scoped path, the dirty
// gate, and the full fallback all actually ran — so this gate cannot
// silently go vacuous again.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_render::{DamageOutcome, Frame, Renderer, Theme, WindowCpu};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default()).map(|mut r| {
        // Deterministic differential: block on the lazy fallback parses so the
        // warm-cache renderer and every fresh oracle rasterize the CJK step
        // identically (no provisional `.notdef` race; the global intern makes
        // every block after the first instant).
        r.debug_block_on_lazy_fallbacks();
        r
    })
}

/// The WARM ARM's persistent state: the renderer PLUS its per-window damage
/// cache (`WindowCpu`) — the exact pair the shipping CPU present holds per
/// window — and a tally of the [`DamageOutcome`]s the walk actually took, so
/// each test can PIN that the fast paths were exercised (see the header's
/// vacuousness note: without the pins, a refactor that reroutes the warm arm
/// back through a throwaway cache turns every parity assert into a
/// tautology).
struct DamageRig {
    r: Renderer,
    wc: WindowCpu,
    full_frames: usize,
    gate_frames: usize,
    rows_frames: usize,
    scroll_frames: usize,
}

fn rig() -> Option<DamageRig> {
    renderer().map(|r| DamageRig {
        r,
        wc: WindowCpu::new(),
        full_frames: 0,
        gate_frames: 0,
        rows_frames: 0,
        scroll_frames: 0,
    })
}

/// The renderer-owned state a frame is drawn with. Both the persistent and the
/// fresh renderer are configured with the SAME state before each render, since
/// `render_input` reads blink phase + cursor-style override off the renderer
/// (they are NOT in `RenderInput`).
#[derive(Clone, Copy)]
struct RState {
    blink_phase: bool,
    cursor_override: Option<CursorStyle>,
}

impl Default for RState {
    fn default() -> Self {
        RState {
            blink_phase: true,
            cursor_override: None,
        }
    }
}

/// Render `term` at `rows`x`cols` through the warm rig (`render_input_cached`
/// over its persistent `WindowCpu` — the damage fast path) and through a
/// brand-new renderer (always full repaint), under identical renderer state,
/// and assert the framebuffers are byte-for-byte equal. `label` names the
/// step. The warm arm's [`DamageOutcome`] is tallied onto the rig so each
/// test can pin which paths its walk exercised.
fn assert_identical(
    damaged: &mut DamageRig,
    rows: usize,
    cols: usize,
    term: &mut Terminal,
    st: RState,
    label: &str,
) {
    // A-3: the engine builds the snapshot; the renderer consumes the value.
    let input = term.cell_frame(rows, cols);

    damaged.r.set_cursor_blink_phase(st.blink_phase);
    damaged.r.set_cursor_style_override(st.cursor_override);
    // THE WARM ARM: the persistent per-window damage cache — the shipping CPU
    // present's entry. NOT `render_input`: that one full-repaints into a
    // throwaway `WindowCpu` per call, which is exactly the degradation the
    // header's vacuousness note records. The borrowed view is cloned into an
    // owned `Frame` only so the exclusive `wc` borrow ends before the outcome
    // tally reads it (a test-only copy; the shipping present keeps the
    // borrow).
    let dmg: Frame = {
        let view = damaged.r.render_input_cached(&mut damaged.wc, &input);
        Frame {
            width: view.width(),
            height: view.height(),
            pixels: view.pixels().to_vec(),
        }
    };
    match damaged.wc.last_damage() {
        DamageOutcome::Full => damaged.full_frames += 1,
        DamageOutcome::GateHit => damaged.gate_frames += 1,
        DamageOutcome::Rows => damaged.rows_frames += 1,
        DamageOutcome::Scroll { .. } => damaged.scroll_frames += 1,
    }

    let mut fresh = renderer().expect("font available (checked by caller)");
    fresh.set_cursor_blink_phase(st.blink_phase);
    fresh.set_cursor_style_override(st.cursor_override);
    let full: Frame = fresh.render_input(&input);

    assert_eq!(dmg.width, full.width, "width mismatch @ {label}");
    assert_eq!(dmg.height, full.height, "height mismatch @ {label}");
    assert_eq!(
        dmg.pixels.len(),
        full.pixels.len(),
        "pixel-count mismatch @ {label}"
    );

    if dmg.pixels != full.pixels {
        // Pinpoint the first divergent pixel for a useful failure.
        let mut first = None;
        for (i, (&a, &b)) in dmg.pixels.iter().zip(full.pixels.iter()).enumerate() {
            if a != b {
                let (x, y) = (i % dmg.width, i / dmg.width);
                first = Some((i, x, y, a, b));
                break;
            }
        }
        let n_diff = dmg
            .pixels
            .iter()
            .zip(full.pixels.iter())
            .filter(|(a, b)| a != b)
            .count();
        panic!(
            "DAMAGE != FULL @ {label}: {n_diff} differing pixels; first {first:?} \
             (index, x, y, damaged, full)"
        );
    }
}

/// One end-to-end differential walk. Returns early (test passes vacuously) if no
/// system font is present, matching the other renderer tests' SKIP convention.
#[test]
fn damage_path_is_byte_identical_to_full_repaint() {
    let Some(mut dmg) = rig() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };

    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let st = RState::default();

    // Step 0: blank screen, cursor home (the first frame — full path warms cache).
    assert_identical(&mut dmg, rows, cols, &mut term, st, "blank");

    // --- Typing, char by char (each adds a 1-cell change on one row). ---
    for (i, ch) in "hello world".bytes().enumerate() {
        term.process(&[ch]);
        assert_identical(&mut dmg, rows, cols, &mut term, st, &format!("type[{i}]"));
    }

    // --- Backspace + overwrite (cursor moves left, content changes). ---
    term.process(b"\x08\x08X"); // back over 'd''l'... rewrite
    assert_identical(&mut dmg, rows, cols, &mut term, st, "backspace+overwrite");

    // --- Newline then more typing on a different row. ---
    term.process(b"\r\nsecond line");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "second row");

    // --- Cursor moves WITHOUT content change (CUP). ---
    term.process(b"\x1b[1;1H"); // home
    assert_identical(&mut dmg, rows, cols, &mut term, st, "cursor home");
    term.process(b"\x1b[3;5H"); // row 3 col 5
    assert_identical(&mut dmg, rows, cols, &mut term, st, "cursor move r3c5");

    // --- SGR / colour change (rewrites cells with new fg/bg). ---
    term.process(b"\x1b[31;42mRED-ON-GREEN\x1b[0m");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "sgr colour");

    // --- Bold + italic + underline + strikethrough + overline decorations. ---
    term.process(b"\x1b[1;3;4mBI\x1b[0m\x1b[9mS\x1b[0m\x1b[53mO\x1b[0m");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "decorations");

    // --- Wide CJK (each glyph occupies 2 cells). ---
    term.process(b"\x1b[5;1H");
    term.process("日本語".as_bytes());
    assert_identical(&mut dmg, rows, cols, &mut term, st, "wide cjk");

    // --- Combining mark: 'e' + U+0301 (combining acute) => é. ---
    term.process(b"\x1b[5;10H");
    term.process("e\u{0301}".as_bytes());
    assert_identical(&mut dmg, rows, cols, &mut term, st, "combining é");

    // --- DECDWL double-width row (ESC # 6 on the current row). ---
    term.process(b"\x1b[6;1H");
    term.process(b"\x1b#6DOUBLEWIDE");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "decdwl");

    // --- Blink-phase toggle (no content change; affects only Blinking* cursor). ---
    // First make the cursor a blinking style so the phase matters.
    term.process(b"\x1b[1 q"); // DECSCUSR 1 = blinking block
    term.process(b"\x1b[1;1H");
    let st_off = RState {
        blink_phase: false,
        ..st
    };
    assert_identical(&mut dmg, rows, cols, &mut term, st_off, "blink off");
    let st_on = RState {
        blink_phase: true,
        ..st
    };
    assert_identical(&mut dmg, rows, cols, &mut term, st_on, "blink on");
    // Toggle again from the warm cache to exercise the gate's phase tracking.
    assert_identical(&mut dmg, rows, cols, &mut term, st_off, "blink off 2");

    // --- Re-render with NO change at all (the dirty-gate fast return). ---
    assert_identical(&mut dmg, rows, cols, &mut term, st_on, "no-op gate");
    assert_identical(&mut dmg, rows, cols, &mut term, st_on, "no-op gate 2");

    // --- Cursor-style override (frontend forces HollowBlock while unfocused). ---
    let st_hollow = RState {
        cursor_override: Some(CursorStyle::HollowBlock),
        ..st_on
    };
    assert_identical(
        &mut dmg,
        rows,
        cols,
        &mut term,
        st_hollow,
        "override hollow",
    );
    // Back to no override.
    assert_identical(&mut dmg, rows, cols, &mut term, st_on, "override cleared");

    // --- Steady block cursor over a glyph (block "cut-out" path). ---
    term.process(b"\x1b[2 q\x1b[1;1H"); // steady block at home, over 'h' (or current)
    assert_identical(
        &mut dmg,
        rows,
        cols,
        &mut term,
        st,
        "steady block over glyph",
    );

    // --- DECTCEM hide / show cursor. ---
    term.process(b"\x1b[?25l"); // hide
    assert_identical(&mut dmg, rows, cols, &mut term, st, "cursor hidden");
    term.process(b"\x1b[?25h"); // show
    assert_identical(&mut dmg, rows, cols, &mut term, st, "cursor shown");

    // --- Selection set / extend / clear (frame-global: forces full fallback). ---
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 2, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    assert_identical(&mut dmg, rows, cols, &mut term, st, "selection set");
    {
        let sel = term.text_selection_mut();
        sel.extend_selection(1, 4, SelectionSide::Right); // extend onto row 1
    }
    assert_identical(&mut dmg, rows, cols, &mut term, st, "selection extend");
    {
        let sel = term.text_selection_mut();
        sel.clear();
    }
    assert_identical(&mut dmg, rows, cols, &mut term, st, "selection clear");

    // --- Scrollback display-offset change (frame-global: forces full fallback). ---
    // Generate scrollback first so there is something to scroll into.
    for i in 0..20 {
        term.process(format!("\r\nscrollback row {i}").as_bytes());
    }
    assert_identical(&mut dmg, rows, cols, &mut term, st, "after scroll content");
    term.scroll_display(5); // scroll up into history
    assert_identical(&mut dmg, rows, cols, &mut term, st, "display_offset=5");
    term.scroll_display(-2);
    assert_identical(&mut dmg, rows, cols, &mut term, st, "display_offset=3");
    term.scroll_to_bottom();
    assert_identical(&mut dmg, rows, cols, &mut term, st, "display_offset=0");

    // --- Type a single char after scrollback returns (warm 1-cell change). ---
    term.process(b"Z");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "1-cell after scroll");

    // --- Resize (dims change: forces full fallback + cache rebuild). ---
    let (rows2, cols2) = (8usize, 30usize);
    term.resize(rows2 as u16, cols2 as u16);
    term.process(b"after resize");
    assert_identical(&mut dmg, rows2, cols2, &mut term, st, "after resize");
    // A 1-cell change at the new size goes through the (now-rebuilt) damage path.
    term.process(b"!");
    assert_identical(&mut dmg, rows2, cols2, &mut term, st, "1-cell after resize");

    // --- Shrink resize back down. ---
    term.resize(rows as u16, cols as u16);
    assert_identical(&mut dmg, rows, cols, &mut term, st, "shrink resize");
    term.process(b"Q");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "1-cell after shrink");

    // ANTI-VACUOUSNESS PINS (see the header): the walk must have exercised
    // every class of the warm arm's damage machinery, or the parity asserts
    // above proved nothing about the fast path.
    assert!(
        dmg.rows_frames > 0,
        "warm arm never took the row-scoped damage path — the differential is vacuous"
    );
    assert!(
        dmg.gate_frames > 0,
        "warm arm never dirty-gate-hit (the no-op steps must gate) — the differential is vacuous"
    );
    assert!(
        dmg.full_frames > 0,
        "warm arm never full-repainted (first frame + resizes must) — the fallback lost coverage"
    );
    eprintln!(
        "damage outcomes over the walk: rows {} | gate {} | full {} | scroll {}",
        dmg.rows_frames, dmg.gate_frames, dmg.full_frames, dmg.scroll_frames
    );
}

/// Rough, machine-dependent timing: how much cheaper is a warm 1-cell change
/// (damage path) than a cold full repaint of the same frame? Ignored by default
/// (`cargo test -- --ignored bench_one_cell_speedup --nocapture`); reports a
/// ratio, asserts nothing — it is a measurement, not a gate.
#[test]
#[ignore]
fn bench_one_cell_speedup() {
    use std::time::Instant;
    let Some(mut dmg) = rig() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (rows, cols) = (40usize, 120usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Fill the screen with text so a full repaint is non-trivial.
    for r in 0..rows {
        term.process(format!("\x1b[{};1Hline {r} ", r + 1).as_bytes());
        term.process(b"the quick brown fox jumps over the lazy dog 0123456789");
    }
    let st = RState::default();
    dmg.r.set_cursor_blink_phase(st.blink_phase);
    dmg.r.set_cursor_style_override(st.cursor_override);

    // Warm the damage cache (the persistent rig — the fast path under test).
    let input0 = term.cell_frame(rows, cols);
    let _ = dmg.r.render_input_cached(&mut dmg.wc, &input0);

    let iters = 200u32;

    // Damage path: a 1-cell change each iter (toggle a single char), warm cache.
    let t_dmg = {
        let start = Instant::now();
        for i in 0..iters {
            let ch = if i % 2 == 0 { b'A' } else { b'B' };
            term.process(b"\x1b[1;1H");
            term.process(&[ch]);
            let input = term.cell_frame(rows, cols);
            let view = dmg.r.render_input_cached(&mut dmg.wc, &input);
            std::hint::black_box(view.pixels().as_ptr());
        }
        start.elapsed()
    };

    // Full path on a PERSISTENT renderer (no per-iter font parse to distort it):
    // toggling the display_offset every frame forces `full_render` each time
    // (scrollback change invalidates the cache), so this times the full repaint
    // of the same screen, render-only.
    let mut fullr = renderer().expect("font");
    fullr.set_cursor_blink_phase(st.blink_phase);
    fullr.set_cursor_style_override(st.cursor_override);
    let mut input_a = term.cell_frame(rows, cols);
    let mut input_b = input_a.clone();
    input_b.display_offset = 1; // a different offset -> forced full render
    let t_full = {
        let start = Instant::now();
        for i in 0..iters {
            let input = if i % 2 == 0 { &input_a } else { &input_b };
            let f = fullr.render_input(input);
            std::hint::black_box(&f);
        }
        start.elapsed()
    };
    std::hint::black_box((&mut input_a, &mut input_b));

    let per_dmg = t_dmg.as_secs_f64() / iters as f64 * 1e6;
    let per_full = t_full.as_secs_f64() / iters as f64 * 1e6;
    eprintln!(
        "1-cell damage: {per_dmg:.1} us/frame  |  full repaint: {per_full:.1} us/frame  \
         |  speedup: {:.1}x  ({rows}x{cols} grid)",
        per_full / per_dmg
    );
}

/// A second, tighter loop focused on the single-cell warm path: type many chars
/// in a row, asserting byte-identity at every keystroke once the cache is warm.
/// This is the dominant interactive case the optimization targets.
#[test]
fn warm_single_cell_typing_is_identical() {
    let Some(mut dmg) = rig() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (rows, cols) = (4usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let st = RState::default();

    assert_identical(&mut dmg, rows, cols, &mut term, st, "warm:blank");
    let text = "The quick brown fox jumps over the lazy dog 0123";
    let mut typed = 0usize;
    for (i, ch) in text.bytes().enumerate() {
        if i >= cols {
            break;
        }
        term.process(&[ch]);
        typed += 1;
        assert_identical(
            &mut dmg,
            rows,
            cols,
            &mut term,
            st,
            &format!("warm:type[{i}]"),
        );
    }

    // The dominant interactive case must RIDE the damage path (the header's
    // vacuousness note): only frame 0 (cache priming) may repaint fully, no
    // keystroke may bogusly gate-hit, and every keystroke must take the
    // row-scoped path.
    assert_eq!(
        dmg.full_frames, 1,
        "warm typing: only the cache-priming first frame may full-repaint"
    );
    assert_eq!(
        dmg.gate_frames, 0,
        "warm typing: a keystroke frame dirty-gate-hit — its glyph never reached the diff"
    );
    assert_eq!(
        dmg.rows_frames, typed,
        "warm typing: every keystroke must take the row-scoped damage path"
    );
}
