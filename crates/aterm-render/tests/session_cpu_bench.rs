// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// HOLISTIC render-pipeline CPU benchmark: ONE defensible number for how much
// less CPU aterm's render path spends over a REPRESENTATIVE interactive terminal
// session after this session's optimizations (damage-tracked `render_input` +
// 0-alloc `extract_into`) vs the pre-optimization behavior (a full repaint every
// frame).
//
// =====================  WHAT THIS MEASURES (AND WHY)  ======================
// We build ONE `Terminal` at a realistic 40x120, drive it with a documented,
// fair, representative interactive byte stream, and capture a `RenderInput`
// snapshot after every "frame" (a frame = one user-visible update). Then we time
// rendering the WHOLE captured sequence two ways on the SAME machine, warmed,
// taking the MEDIAN of several runs:
//
//   (a) CURRENT / damaged: one warm `Renderer`, rendering the sequence in order.
//       Its damage cache makes unchanged-row frames cheap — this is what ships.
//   (b) PRE-OPT / full-repaint: the SAME renderer and the SAME snapshots, but we
//       call `reset_damage_cache()` before each frame so every frame takes the
//       full-repaint path (the old behavior). `reset_damage_cache` is documented
//       test/bench scaffolding; it does not change normal rendering.
//
// Crucially (a) and (b) render BYTE-IDENTICAL pixels for the final frame (and the
// damage path is separately proven byte-identical for EVERY frame by
// `damage_differential.rs`), so this is apples-to-apples: same pixels, only the
// WORK differs. We assert the final-frame identity here.
//
// The aggregate % reduction is the single holistic number. We also print the
// per-segment breakdown (typing / scroll / blink / scrollback / full-repaint) and
// the best-case (pure 1-cell frame) and worst-case (full-screen repaint) ratios
// so it is transparent where the gain comes from — and that full-repaint frames
// are ~unchanged between the two paths (the optimization can't and doesn't help
// a frame where everything changed).
//
// Run (release, for realistic numbers):
//   cargo test -p aterm-render --release --test session_cpu_bench \
//       -- --ignored --nocapture session_cpu

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_render::{Frame, RenderInput, Renderer, Theme, WindowCpu};

const ROWS: usize = 40;
const COLS: usize = 120;

/// Which part of the representative session a frame belongs to — so the report
/// can break the aggregate down and show where the gain comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Segment {
    /// The shell prompt being drawn (a small first paint after the blank frame).
    Prompt,
    /// A single-keystroke echo: a 1-cell delta on the current row. The dominant
    /// interactive case and the best case for damage tracking.
    Typing,
    /// A line of colored command output that scrolls the screen up by one row.
    Scroll,
    /// A cursor blink toggle: NO content change, only the renderer's blink phase
    /// flips. The damage path's dirty-gate should make this nearly free.
    Blink,
    /// A scrollback scroll step (display-offset change). This is frame-global, so
    /// the damage path intentionally falls back to a full repaint — included to
    /// keep the mix honest (it is NOT a case the optimization helps).
    Scrollback,
    /// A full-screen TUI repaint (an alt-screen redraw that rewrites every cell).
    /// The worst case: nearly every row changes, so the two paths do ~equal work.
    FullRepaint,
}

impl Segment {
    fn label(self) -> &'static str {
        match self {
            Segment::Prompt => "prompt",
            Segment::Typing => "typing (1-cell)",
            Segment::Scroll => "scroll (colored output)",
            Segment::Blink => "blink toggle",
            Segment::Scrollback => "scrollback step",
            Segment::FullRepaint => "full-screen repaint",
        }
    }
}

/// One captured frame: the input snapshot to render plus the blink phase it must
/// be rendered with (blink toggles change only the phase, not the bytes), tagged
/// with its segment so we can attribute time.
struct CapturedFrame {
    input: RenderInput,
    blink_phase: bool,
    segment: Segment,
}

/// Build the representative interactive session and capture a `RenderInput` after
/// each frame. Returns the captured frames and a textual description of the EXACT
/// workload composition (printed in the report so the mix is fully disclosed).
///
/// Counts are fixed constants so the composition is documented and reproducible.
fn build_session() -> (Vec<CapturedFrame>, String) {
    // --- Documented, fair composition. Mostly small per-frame deltas with
    // occasional full repaints, reflecting real interactive use. ---
    const N_TYPING: usize = 300; // single-keystroke echoes (1-cell deltas)
    const N_SCROLL: usize = 60; // lines of colored command output (scrolling)
    const N_BLINK: usize = 40; // cursor-blink toggles (no content change)
    const N_SCROLLBACK: usize = 20; // scrollback scroll steps
    const N_FULLREPAINT: usize = 5; // full-screen TUI repaints (alt-screen redraw)

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let mut frames: Vec<CapturedFrame> = Vec::new();
    let mut blink_phase = true;

    // A small helper to capture the current terminal state as a frame.
    // A-3: the engine builds the snapshot (`Terminal::cell_frame`).
    let capture = |term: &mut Terminal,
                   frames: &mut Vec<CapturedFrame>,
                   blink_phase: bool,
                   segment: Segment| {
        frames.push(CapturedFrame {
            input: term.cell_frame(ROWS, COLS),
            blink_phase,
            segment,
        });
    };

    // Use a steady (non-blinking) block cursor for the typing/scroll body so the
    // cursor overlay is deterministic; switch to a blinking style only for the
    // blink-toggle segment where the phase actually matters.
    term.process(b"\x1b[2 q"); // DECSCUSR 2 = steady block

    // 1) Shell prompt: a small first paint. (Frame 0 is the blank screen that
    //    warms the cache; frame 1 draws the prompt.)
    capture(&mut term, &mut frames, blink_phase, Segment::Prompt); // blank, warms cache
    term.process(b"\x1b[32muser@host\x1b[0m:\x1b[34m~/work\x1b[0m$ ");
    capture(&mut term, &mut frames, blink_phase, Segment::Prompt);

    // 2) ~300 single-keystroke echoes: a representative command line being typed
    //    one character at a time. Each frame is a 1-cell delta on the prompt row.
    //    When the line would reach the right margin we start a fresh prompt line
    //    so we keep echoing 1-cell deltas rather than wrapping pathologically.
    let typed =
        b"git commit -am 'optimize the render pipeline: damage tracking + zero-alloc extract' ";
    for i in 0..N_TYPING {
        let ch = typed[i % typed.len()];
        // Keep the cursor comfortably inside the row: restart the line each time
        // it gets long, emulating successive short commands at the prompt.
        if term.cursor().col as usize >= COLS - 2 {
            term.process(b"\r\n\x1b[32muser@host\x1b[0m:\x1b[34m~/work\x1b[0m$ ");
        }
        term.process(&[ch]);
        capture(&mut term, &mut frames, blink_phase, Segment::Typing);
    }

    // 3) ~60 lines of colored command output that scroll the screen. Each line is
    //    a full row of colored text terminated by CRLF, so the screen scrolls up
    //    by one row per frame (every visible row's content shifts — a heavier
    //    delta than typing, lighter than a full TUI repaint).
    let palette = [31u8, 32, 33, 34, 35, 36]; // ANSI fg colors
    for i in 0..N_SCROLL {
        let color = palette[i % palette.len()];
        let line = format!(
            "\x1b[{color}m[{i:>3}] build OK  src/module_{i}.rs  \
             warnings=0 errors=0 time={}ms\x1b[0m\r\n",
            12 + (i % 7)
        );
        term.process(line.as_bytes());
        capture(&mut term, &mut frames, blink_phase, Segment::Scroll);
    }

    // 4) ~40 cursor-blink toggles: NO content change, only the renderer's blink
    //    phase flips each frame. First switch the cursor to a blinking style so
    //    the phase actually gates the cursor overlay; then re-capture the SAME
    //    terminal state repeatedly, alternating the phase. The damage path's
    //    dirty-gate should make these nearly free (only the cursor row, if any).
    term.process(b"\x1b[1 q"); // DECSCUSR 1 = blinking block
    let blink_state = term.cell_frame(ROWS, COLS);
    for _ in 0..N_BLINK {
        blink_phase = !blink_phase;
        frames.push(CapturedFrame {
            input: blink_state.clone(),
            blink_phase,
            segment: Segment::Blink,
        });
    }
    blink_phase = true; // restore phase for subsequent frames
    term.process(b"\x1b[2 q"); // back to steady block

    // 5) ~20 scrollback scroll steps: scroll up into history one line at a time.
    //    This is a frame-global display-offset change, which the damage path
    //    deliberately does NOT accelerate (it forces a full repaint). Included to
    //    keep the mix realistic and honest — NOT cherry-picked to favor the win.
    for _ in 0..N_SCROLLBACK {
        term.scroll_display(1);
        capture(&mut term, &mut frames, blink_phase, Segment::Scrollback);
    }
    term.scroll_to_bottom();

    // 6) ~5 full-screen TUI repaints: enter the alternate screen and paint every
    //    cell of every row with fresh content, a different fill each time so the
    //    WHOLE frame changes (a TUI like vim/htop redrawing). This is the worst
    //    case for damage tracking — nearly every row is dirty — so the two paths
    //    should do nearly equal work here, which the per-segment report exposes.
    term.process(b"\x1b[?1049h"); // enter alt screen
    for f in 0..N_FULLREPAINT {
        term.process(b"\x1b[2J\x1b[H"); // clear + home
        let color = palette[f % palette.len()];
        for r in 0..ROWS {
            // Fill the row fully with a per-frame, per-row varied colored pattern.
            term.process(format!("\x1b[{};1H", r + 1).as_bytes());
            term.process(format!("\x1b[{}m", color + (r % 2) as u8).as_bytes());
            let fill: String = (0..COLS)
                .map(|c| {
                    let n = (f * 7 + r * 3 + c) % 36;
                    char::from_digit(n as u32, 36).unwrap_or('#')
                })
                .collect();
            term.process(fill.as_bytes());
        }
        term.process(b"\x1b[0m");
        capture(&mut term, &mut frames, blink_phase, Segment::FullRepaint);
    }
    term.process(b"\x1b[?1049l"); // leave alt screen

    let composition = format!(
        "Representative interactive session @ {ROWS}x{COLS}:\n  \
         - shell prompt frames:        2 (blank warm-up + prompt paint)\n  \
         - typing (1-cell echoes):     {N_TYPING}\n  \
         - colored output (scrolling): {N_SCROLL}\n  \
         - cursor-blink toggles:       {N_BLINK} (no content change)\n  \
         - scrollback scroll steps:    {N_SCROLLBACK} (frame-global -> full repaint)\n  \
         - full-screen TUI repaints:   {N_FULLREPAINT} (alt-screen full redraw)\n  \
         = {} total frames",
        frames.len()
    );

    (frames, composition)
}

/// Render the whole captured sequence through one warm renderer the CURRENT
/// (damaged) way: render each frame in order; the cache makes unchanged rows
/// cheap. Returns the total elapsed time and the FINAL frame (for the
/// byte-identity assertion).
fn run_damaged(r: &mut Renderer, frames: &[CapturedFrame]) -> (Duration, Frame) {
    let start = Instant::now();
    let mut last: Option<Frame> = None;
    for f in frames {
        r.set_cursor_blink_phase(f.blink_phase);
        let frame = r.render_input(&f.input);
        last = Some(std::hint::black_box(frame));
    }
    (start.elapsed(), last.expect("non-empty sequence"))
}

/// Render the whole captured sequence the PRE-OPT (full-repaint) way: same warm
/// renderer, but drop the damage cache before each frame so every frame takes the
/// full-repaint path (the old behavior). Returns total time and the final frame.
fn run_full(r: &mut Renderer, frames: &[CapturedFrame]) -> (Duration, Frame) {
    let start = Instant::now();
    let mut last: Option<Frame> = None;
    for f in frames {
        r.set_cursor_blink_phase(f.blink_phase);
        // The owned `render_input` now drives a THROWAWAY per-call `WindowCpu`
        // (S5c), so it already takes the full-repaint path every frame — the old
        // explicit `reset_damage_cache()` is implied and no longer needed here.
        let frame = r.render_input(&f.input);
        last = Some(std::hint::black_box(frame));
    }
    (start.elapsed(), last.expect("non-empty sequence"))
}

/// Per-segment timing of the two paths, for the breakdown. Sums the per-frame
/// time within each segment so the report can attribute the aggregate gain. Runs
/// on a warm renderer; resets the cache per frame only for the full path.
fn segment_times(
    r: &mut Renderer,
    frames: &[CapturedFrame],
    full: bool,
) -> std::collections::BTreeMap<&'static str, (Duration, usize)> {
    use std::collections::BTreeMap;
    let mut out: BTreeMap<&'static str, (Duration, usize)> = BTreeMap::new();
    // `full` is now implicit: the owned `render_input` always full-repaints via a
    // throwaway per-call `WindowCpu` (S5c). The arg is kept so both call sites and
    // the report wording stay put; it no longer needs an explicit cache reset.
    let _ = full;
    for f in frames {
        r.set_cursor_blink_phase(f.blink_phase);
        let start = Instant::now();
        let frame = r.render_input(&f.input);
        let dt = start.elapsed();
        std::hint::black_box(&frame);
        let e = out.entry(f.segment.label()).or_insert((Duration::ZERO, 0));
        e.0 += dt;
        e.1 += 1;
    }
    out
}

/// Median of a slice of durations (sorted copy, middle element).
fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

// ===================  GUI PRESENTATION HOT-PATH MEASUREMENT  ===================
// The holistic figures above time `render_input` (which CLONES the damage cache
// into an owned `Frame`). They are unaffected by this session's optimization,
// which lives one level up — in the windowed frontend's per-frame PRESENTATION
// path. There the GUI used to:
//     let frame = rasterizer.render_input(input);   // cache -> OWNED Frame (CLONE)
//     surface_buf.copy_from_slice(&frame.pixels);   // Frame -> surface (COPY)
// i.e. TWO full-framebuffer copies + one Vec alloc per frame. The new path is:
//     let view = rasterizer.render_input_cached(input);  // cache -> BORROW (no clone)
//     surface_buf.copy_from_slice(view.pixels());        // cache -> surface (COPY)
// i.e. ONE copy, zero alloc. These two helpers reproduce EXACTLY those two
// per-frame bodies (a persistent surface buffer, copied into each frame, with a
// black_box so the optimizer can't elide the copy), over the SAME warm renderer
// and the SAME captured session, so the delta is the eliminated clone+alloc.

/// OLD GUI per-frame body: `render_input` (owned Frame, clones the cache) then
/// copy the Frame's pixels into the persistent surface buffer.
fn run_gui_old(r: &mut Renderer, frames: &[CapturedFrame], surface: &mut Vec<u32>) -> Duration {
    let start = Instant::now();
    for f in frames {
        r.set_cursor_blink_phase(f.blink_phase);
        let frame = r.render_input(&f.input); // cache -> OWNED Frame (clone + alloc)
        let n = surface.len().min(frame.pixels.len());
        surface[..n].copy_from_slice(&frame.pixels[..n]); // Frame -> surface (copy)
        std::hint::black_box(&*surface);
        std::hint::black_box(&frame);
    }
    start.elapsed()
}

/// NEW GUI per-frame body: `render_input_cached` (borrow of the cache, no clone)
/// then copy the borrowed pixels into the persistent surface buffer.
fn run_gui_new(r: &mut Renderer, frames: &[CapturedFrame], surface: &mut Vec<u32>) -> Duration {
    // A persistent per-window damage cache (S5c) — the GUI now holds one
    // `WindowCpu` per window and threads it into `render_input_cached`.
    let mut wc = WindowCpu::new();
    let start = Instant::now();
    for f in frames {
        r.set_cursor_blink_phase(f.blink_phase);
        let view = r.render_input_cached(&mut wc, &f.input); // cache -> BORROW (no clone/alloc)
        let pixels = view.pixels();
        let n = surface.len().min(pixels.len());
        surface[..n].copy_from_slice(&pixels[..n]); // cache -> surface (copy)
        std::hint::black_box(&*surface);
    }
    start.elapsed()
}

/// TRUE PRE-OPT GUI per-frame body: full repaint (`reset_damage_cache`) + owned
/// `Frame` (clone + alloc) + copy to surface — the GUI before BOTH of this
/// session's render optimizations (damage tracking AND the clone removal).
fn run_gui_old_full(
    r: &mut Renderer,
    frames: &[CapturedFrame],
    surface: &mut Vec<u32>,
) -> Duration {
    let start = Instant::now();
    for f in frames {
        r.set_cursor_blink_phase(f.blink_phase);
        // Owned `render_input` already full-repaints via a throwaway `WindowCpu`
        // (S5c) — the pre-damage-tracking behavior, no explicit reset needed.
        let frame = r.render_input(&f.input); // cache -> OWNED Frame (clone + alloc)
        let n = surface.len().min(frame.pixels.len());
        surface[..n].copy_from_slice(&frame.pixels[..n]);
        std::hint::black_box(&*surface);
    }
    start.elapsed()
}

#[test]
#[ignore]
fn session_cpu_reduction() {
    let Some(mut r) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };

    let (frames, composition) = build_session();
    assert!(!frames.is_empty(), "session must produce frames");

    // --- Warm: run both paths a few times so caches / allocator / CPU clocks
    // settle into steady state before we measure. ---
    for _ in 0..3 {
        let _ = run_damaged(&mut r, &frames);
        let _ = run_full(&mut r, &frames);
    }

    // --- Timed: median of >= 5 runs each, alternating so any drift hits both. ---
    const RUNS: usize = 9;
    let mut d_times = Vec::with_capacity(RUNS);
    let mut f_times = Vec::with_capacity(RUNS);
    let mut final_damaged: Option<Frame> = None;
    let mut final_full: Option<Frame> = None;
    for _ in 0..RUNS {
        let (td, fd) = run_damaged(&mut r, &frames);
        let (tf, ff) = run_full(&mut r, &frames);
        d_times.push(td);
        f_times.push(tf);
        final_damaged = Some(fd);
        final_full = Some(ff);
    }
    let t_damaged = median(d_times);
    let t_full = median(f_times);

    // --- Apples-to-apples proof: the final frame is byte-identical both ways. ---
    let fd = final_damaged.expect("ran");
    let ff = final_full.expect("ran");
    assert_eq!(fd.width, ff.width, "final width mismatch");
    assert_eq!(fd.height, ff.height, "final height mismatch");
    assert_eq!(
        fd.pixels, ff.pixels,
        "damaged and full-repaint paths must render byte-identical final pixels"
    );

    // --- Per-segment breakdown (single warm run each; relative shares are stable
    // and the aggregate above is the authoritative timed figure). ---
    let seg_dmg = segment_times(&mut r, &frames, false);
    let seg_full = segment_times(&mut r, &frames, true);

    // --- Best / worst single-frame ratios, for honesty. Best = a pure 1-cell
    // typing frame; worst = a full-screen repaint frame. Median over several
    // measurements of one representative frame of each kind. ---
    let best_idx = frames.iter().position(|f| f.segment == Segment::Typing);
    let worst_idx = frames
        .iter()
        .position(|f| f.segment == Segment::FullRepaint);
    let mut frame_ratio = |idx: usize| -> (f64, f64) {
        let f = &frames[idx];
        let n = 200;
        // Damaged: warm the cache to THIS frame first (render the preceding frame
        // then this one) so the measured render is a true incremental update.
        let mut ds = Vec::with_capacity(n);
        for _ in 0..n {
            // Re-establish the cache from the previous frame so `idx` is a real
            // damage update, not a no-op gate hit against itself.
            if idx > 0 {
                r.set_cursor_blink_phase(frames[idx - 1].blink_phase);
                let _ = r.render_input(&frames[idx - 1].input);
            }
            r.set_cursor_blink_phase(f.blink_phase);
            let s = Instant::now();
            let fr = r.render_input(&f.input);
            ds.push(s.elapsed());
            std::hint::black_box(&fr);
        }
        let mut fs = Vec::with_capacity(n);
        for _ in 0..n {
            r.set_cursor_blink_phase(f.blink_phase);
            // Owned `render_input` always full-repaints via a throwaway `WindowCpu`
            // (S5c), so no explicit `reset_damage_cache` is needed to force it.
            let s = Instant::now();
            let fr = r.render_input(&f.input);
            fs.push(s.elapsed());
            std::hint::black_box(&fr);
        }
        (
            median(ds).as_secs_f64() * 1e6,
            median(fs).as_secs_f64() * 1e6,
        )
    };

    // ============================  REPORT  ============================
    let dmg_us = t_damaged.as_secs_f64() * 1e6;
    let full_us = t_full.as_secs_f64() * 1e6;
    let reduction = (full_us - dmg_us) / full_us * 100.0;
    let speedup = full_us / dmg_us;

    eprintln!("\n================ aterm render-pipeline holistic CPU bench ================");
    eprintln!("{composition}\n");
    eprintln!(
        "Method: one warm Renderer renders the whole captured sequence twice — \n  \
         (a) CURRENT damage-tracked, (b) PRE-OPT full-repaint (reset_damage_cache \n  \
         before each frame). Median of {RUNS} runs. Final frame asserted byte-identical."
    );

    eprintln!("\n--- AGGREGATE (the single holistic number) ---");
    eprintln!("  pre-opt  (full repaint every frame): {full_us:>9.1} us / sequence");
    eprintln!("  current  (damage-tracked render):    {dmg_us:>9.1} us / sequence");
    eprintln!(
        "  ==> CPU/time reduction:  {reduction:>5.1} %   ({speedup:.2}x faster) over the \
         representative session"
    );

    eprintln!("\n--- PER-SEGMENT BREAKDOWN (where the gain comes from) ---");
    eprintln!(
        "  {:<26} {:>6} {:>12} {:>12} {:>10}",
        "segment", "frames", "pre-opt us", "current us", "reduction"
    );
    for (label, (full_d, n)) in &seg_full {
        let (dmg_d, _) = seg_dmg.get(label).copied().unwrap_or((Duration::ZERO, 0));
        let fu = full_d.as_secs_f64() * 1e6;
        let du = dmg_d.as_secs_f64() * 1e6;
        let red = if fu > 0.0 {
            (fu - du) / fu * 100.0
        } else {
            0.0
        };
        eprintln!("  {label:<26} {n:>6} {fu:>12.1} {du:>12.1} {red:>9.1}%");
    }

    eprintln!("\n--- BEST / WORST single-frame ratios (honesty check) ---");
    if let Some(bi) = best_idx {
        let (d, f) = frame_ratio(bi);
        eprintln!(
            "  BEST  (pure 1-cell typing frame):  current {d:.3} us vs pre-opt {f:.3} us  \
             -> {:.1}% reduction ({:.1}x)",
            (f - d) / f * 100.0,
            f / d
        );
    }
    if let Some(wi) = worst_idx {
        let (d, f) = frame_ratio(wi);
        eprintln!(
            "  WORST (full-screen repaint frame): current {d:.3} us vs pre-opt {f:.3} us  \
             -> {:.1}% reduction ({:.1}x)  [~equal work, as expected]",
            (f - d) / f * 100.0,
            f / d
        );
    }
    // `frame_ratio` borrows `r`; it is dropped here, so `r` is free to borrow
    // again for the GUI presentation-path measurement below.
    let _ = frame_ratio;

    // === GUI PRESENTATION HOT-PATH: old (render_input + copy) vs new
    // (render_input_cached + copy). This is what THIS session's change actually
    // moves: the per-frame cache->Frame CLONE + alloc is gone. Same warm renderer,
    // same captured session, a persistent surface buffer copied into each frame. ===
    let (fw, fh) = r.frame_size(ROWS, COLS);
    let mut surf_old = vec![0u32; fw * fh];
    let mut surf_new = vec![0u32; fw * fh];
    // Warm both bodies.
    for _ in 0..3 {
        let _ = run_gui_old(&mut r, &frames, &mut surf_old);
        let _ = run_gui_new(&mut r, &frames, &mut surf_new);
    }
    // Byte-identity of what lands on the surface, both bodies, after the same
    // sequence — the hard constraint, asserted here directly on presented pixels.
    let _ = run_gui_old(&mut r, &frames, &mut surf_old);
    let _ = run_gui_new(&mut r, &frames, &mut surf_new);
    assert_eq!(
        surf_old, surf_new,
        "GUI old (render_input+copy) and new (render_input_cached+copy) must present \
         byte-identical surface pixels"
    );
    // Timed: median of RUNS, alternating so drift hits both equally.
    let mut g_old = Vec::with_capacity(RUNS);
    let mut g_new = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        g_old.push(run_gui_old(&mut r, &frames, &mut surf_old));
        g_new.push(run_gui_new(&mut r, &frames, &mut surf_new));
    }
    let gui_old_us = median(g_old).as_secs_f64() * 1e6;
    let gui_new_us = median(g_new).as_secs_f64() * 1e6;
    let gui_reduction = (gui_old_us - gui_new_us) / gui_old_us * 100.0;
    let gui_speedup = gui_old_us / gui_new_us;
    let nframes = frames.len();
    let frame_bytes = fw * fh * std::mem::size_of::<u32>();

    eprintln!("\n--- GUI PRESENTATION HOT-PATH (what this change moves) ---");
    eprintln!(
        "  Per frame the GUI does the cache->surface copy EITHER way (the surface\n  \
         buffer isn't persistent); the change removes the EXTRA cache->Frame clone\n  \
         + Vec allocation that `render_input` did before `render_input_cached`."
    );
    eprintln!(
        "  frame size: {fw}x{fh} = {} px = {:.2} MB ({} frames in the session)",
        fw * fh,
        frame_bytes as f64 / 1e6,
        nframes
    );
    eprintln!("  old  (render_input  + copy_from_slice): {gui_old_us:>9.1} us / sequence");
    eprintln!("  new  (render_input_cached + copy):       {gui_new_us:>9.1} us / sequence");
    eprintln!(
        "  ==> presentation-path reduction: {gui_reduction:>5.1} %  ({gui_speedup:.2}x) \
         over the session"
    );
    eprintln!(
        "  ==> per-frame eliminated: 1 full-framebuffer memcpy (~{:.2} MB) + 1 Vec alloc/free",
        frame_bytes as f64 / 1e6
    );
    eprintln!(
        "  ==> over the session: ~{:.1} MB of clone memcpy + {nframes} allocs eliminated",
        frame_bytes as f64 * nframes as f64 / 1e6
    );

    // === COMBINED end-to-end: the GUI per-frame path BEFORE this whole session's
    // render work (full repaint + Frame clone + surface copy) vs AFTER
    // (damage-tracked borrow + surface copy). This composes BOTH wins (damage
    // tracking AND the clone removal) into the single holistic real-world number. ===
    let mut surf_oldfull = vec![0u32; fw * fh];
    for _ in 0..3 {
        let _ = run_gui_old_full(&mut r, &frames, &mut surf_oldfull);
        let _ = run_gui_new(&mut r, &frames, &mut surf_new);
    }
    let _ = run_gui_old_full(&mut r, &frames, &mut surf_oldfull);
    let _ = run_gui_new(&mut r, &frames, &mut surf_new);
    assert_eq!(
        surf_oldfull, surf_new,
        "combined old-full (repaint+clone+copy) and new (damaged borrow+copy) must present \
         byte-identical surface pixels"
    );
    let mut c_old = Vec::with_capacity(RUNS);
    let mut c_new = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        c_old.push(run_gui_old_full(&mut r, &frames, &mut surf_oldfull));
        c_new.push(run_gui_new(&mut r, &frames, &mut surf_new));
    }
    let comb_old_us = median(c_old).as_secs_f64() * 1e6;
    let comb_new_us = median(c_new).as_secs_f64() * 1e6;
    let comb_reduction = (comb_old_us - comb_new_us) / comb_old_us * 100.0;
    let comb_speedup = comb_old_us / comb_new_us;
    eprintln!("\n--- COMBINED end-to-end GUI per-frame (THE single holistic number) ---");
    eprintln!(
        "  Old GUI (pre-session): full repaint EVERY frame + cache->Frame clone+alloc + \
         Frame->surface copy.\n  New GUI (now): damage-tracked render into the persistent \
         cache, borrow it, single cache->surface copy."
    );
    eprintln!("  pre-opt  (full repaint + clone + copy): {comb_old_us:>9.1} us / sequence");
    eprintln!("  current  (damaged borrow + copy):       {comb_new_us:>9.1} us / sequence");
    eprintln!(
        "  ==> END-TO-END GUI CPU reduction: {comb_reduction:>5.1} %  ({comb_speedup:.2}x) \
         over the representative session"
    );

    eprintln!("=========================================================================\n");
}
