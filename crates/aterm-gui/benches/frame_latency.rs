// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The aterm-gui FRAME PATH — keystroke-to-echo and PTY-flood-to-present — had
// no bench target at all before this file: the crate was believed bin-only and
// unbenchable, while the audits kept filing per-presented-frame findings
// against it (CF-6's driver half, PET-03's gui half) that nothing could price.
// It is in fact a LIBRARY whose unit tests build full headless `App` fixtures;
// this bench drives those same fixtures through the feature-gated
// `bench_support` seam (the `test-open-probe` precedent — no shipping build
// compiles it).
//
// WHAT IS TIMED, EXACTLY. The engine path AND NOTHING ELSE, per workload. One
// iteration of a `frame_latency` workload is:
//
//     arm(..);                     // UNTIMED: clock += dt, cadence pulse,
//                                  //          keystroke, 8 KiB PTY feed
//     let t0 = Instant::now();     //  ── timed span opens
//     black_box(<frame call>);     //  tick_fx / compose / present_frame
//     total += t0.elapsed();       //  ── timed span closes
//
// The arms are priced SEPARATELY, under names that cannot be mistaken for the
// frame (`frame_latency_seams`), exactly as the aterm-effects benches price
// their host seams. `frame_latency/timer_floor` measures the `Instant::now()`
// pair every number in the group carries: subtract it for an absolute cost,
// ignore it for an A/B (it cancels).
//
// WHAT A HEADLESS FRAME CAN MODEL, and what each workload calls:
//
//   * `tick_fx` — `App::tick_cursor_fx`, the per-frame cursor-effect driver,
//     extracted VERBATIM from the windowed present and explicitly built for
//     two callers, one of them headless (`splice_cursor_fx`). The CF-6 finding
//     lives entirely inside it.
//   * `compose` — `App::redraw_compose`, the split/composed present (per-pane
//     locks + extraction, decorations, the compose-path cursor effects with
//     their own cadence `ignite`, the PET ink feed + brain tick, the
//     RepaintKey gate). A REAL shipping path (the video recorder's), and the
//     path the split-sparkle unit tests drive headless. The PET-03 gui half
//     lives on it.
//   * `present_frame` — the single-pane present modeled as the scout mapped
//     it: LOCK-A extract (`cell_frame_into` + `take_damage` under one lock),
//     `tick_cursor_fx`, then a REAL CPU raster (`Renderer::render_input`) —
//     actual pixels, headless.
//
// WHAT IS OUT OF REACH (cut honestly, not stubbed): the OS present itself —
// softbuffer damage-rect blit / GPU swapchain present, frame pacing, EDR/scale
// binding, tab-strip + native chrome, and `redraw_window`'s own inline
// single-pane compose (its present-target match bails headless before LOCK A;
// `present_frame` above is the model of it, not a call into it). Those halves
// need glass and stay unpriced here rather than mispriced.
//
// THE WORKLOADS, in the campaign's priority order:
//
//   1. effects_off_frame  — CF-6's price: `tick_cursor_fx` on a frame with
//      EVERY cursor effect off. The driver still resolves policies + configs
//      and runs the TypingCadence triple + `ignite` before any `cfg.enabled`
//      is consulted; the guard PROVES that from outside (see below).
//   2. pet_invisible_frame — PET-03's price: a composed frame with the pet
//      configured but provably invisible; the unconditional per-frame
//      `pet_ink()` + `sense_ink()` pair still runs. Its exact pair is also
//      priced alone in the seams group (`pet_ink_feed`), because inside a
//      full compose frame the pair is nanoseconds under microseconds.
//   3. flood_present      — HEAVY LOAD: an 8 KiB PTY-shaped chunk ingested
//      between frames, the compose timed while the terminal is hot. The
//      perceived-responsiveness headline.
//   4. keystroke_echo     — the latency-critical path: a real `App::input`
//      keystroke (encode, policy, predictive echo, cadence pulse) as the arm,
//      and the frame that echoes it (extract + effects + raster) timed.
//   5. many_tabs_idle/N   — N resident tabs, nothing changing: the
//      steady-state cost of the settled compose frame as state scales.
//
// ONE FINDING THIS BENCH MADE WHILE BEING BUILT (FL-1), recorded because it
// changes how the settled-frame numbers must be read: a SETTLED composed
// window never takes the RepaintKey early-out. With decorations dead, nothing
// typed, and `has_damage()` false on every pane both before and after the
// frame, consecutive keys still differ — a temporary field-by-field key diff
// showed `damage_epoch` as the ONLY moving term, advancing once per composed
// frame, so the advance happens inside the compose itself. Every scheduled
// compose of unchanged content therefore repaints the full window. The
// `pet_invisible_frame` and `many_tabs_idle/N` guards PIN that measured
// reality two-sided (presented == 100%); if the early-out is ever repaired,
// those pins flip to 0 and the flip is the fix's own measured proof.
//
// EVERY WORKLOAD IS VERIFIED BEFORE IT IS TIMED, template: the aterm-effects
// cursor_glow_tick bench. Two-sided guards prove the workload reaches the
// state it claims — an "off" cost measured on a script that would have drawn
// nothing anyway is not a measurement — and OFF arms carry DarkUnless-style
// controls: the identical script through a fixture missing only the off
// switch, which must light up.
//
// HOW THE CF-6 REACH IS PROVED FROM OUTSIDE. `ignite` heat-blends the
// returned `trail_color` from the cadence intensity/warmth pair
// UNCONDITIONALLY — before any enabled bit is consulted — so on an all-off
// frame the returned colour still moves with typing heat. The guard pins:
// hot-cadence trail_color != cold-cadence trail_color while every fingerprint
// and fill stays dark. That inequality IS the observable proof the cadence
// reads ran and were consumed on the off frame (pre-fix reality). The
// shared-sample adoption (`TypingCadence::sample`, already landed engine-side)
// keeps it bit-identical; an early-out fix that stops blending on all-off
// frames must revisit this guard — at which point the whole span this
// workload times is the win being claimed.
//
// REACH WAS ALSO CONFIRMED ONCE, OUT OF BAND, with temporary counters at the
// audit sites themselves (added, observed, removed — the campaign's
// prove-reach discipline): over 300 all-off `tick_fx` frames the cadence
// `rainbow_energy` read and the `ignite` pair each ran exactly 300 times
// (CF-6), and over 300 settled composed frames the compose-path
// `pet_ink`+`sense_ink` feed ran exactly 300 times while the single-pane twin
// ran 0 (it lives behind the glass-only present — headless-unreachable, as
// documented above). The standing guards keep the STATE honest from outside:
// the map is real (rows inked, live edge present), the pet is configured
// (`trail_is_kitty_pet`), and it cannot draw (`glow_cfg.enabled == false` is
// one of `pet_visible`'s AND terms).
//
// DETERMINISM. Every workload advances an INJECTED clock by a fixed dt per
// frame (one wall sample per fixture, at its origin); the measurement clock
// (criterion's) is entirely separate. Two exceptions are documented where
// they occur: `App::input` samples the wall internally (the real input seam
// is being driven, not modeled), and `RepaintKey` folds one wall read for a
// notice fingerprint on compose frames — neither feeds any guard.
//
// OBSERVABLE COUNTS ARE MEASUREMENTS, NOT PRINTOUTS. The volume group hands
// the guard-asserted counts (flood bytes/frame, the pet ink map's size) to
// criterion as their own benchmarks — 1 ns == 1 item, the aterm-effects
// convention — so a count regression is stored and A/B'd by the same tooling
// that catches a time regression.

use std::time::{Duration, Instant};

use aterm_gui::bench_support::BenchApp;
use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, black_box, criterion_group, criterion_main,
};

// ------------------------------------------------------------------ clocks --

/// 60 fps — the dt of frame-paced workloads (flood, pet, idle).
const FRAME_DT: Duration = Duration::from_millis(16);

/// 8 ms per keystroke — fast human / key-repeat cadence, the same dial the
/// effects benches use for their typing scripts. It keeps the cadence heat
/// saturated, which is the state the CF-6 triple costs most in (a never-typed
/// tracker short-circuits its lazy decay entirely).
const TYPE_DT: Duration = Duration::from_millis(8);

// ------------------------------------------------------------------ script --

/// Frames of script run before anything is sampled or timed. Long enough for
/// every fixture to reach steady state: the decoration engines' longest
/// episode (a cat peek, ~600 ms) is ~40 frames, and the pet's fade/settle
/// envelope shorter — 400 frames (6.4 s injected) covers them with margin.
const WARM_FRAMES: usize = 400;

/// Frames sampled by each workload's verify pass.
const SAMPLE_FRAMES: usize = 300;

/// The flood chunk: the reader thread's 8 KiB slicing shape.
const FLOOD_CHUNK: usize = 8 * 1024;

/// Keystrokes between the echo script's line wraps (see `keystroke_echo`).
const ECHO_LINE: u32 = 60;

/// The grid every fixture presents at (the headless fixture's stub terminal).
const ROWS: usize = 24;

// ------------------------------------------------------------------ report --

/// The one human-readable line each verify pass prints: what state the
/// workload reached, in the numbers its guards assert on.
fn report(name: &str, detail: &str) {
    println!("REACH {name:<22} | {detail}");
}

// ---------------------------------------------------------------- fixtures --

/// CF-6 fixture: every cursor effect off, cadence kept hot by the arm.
fn f_effects_off() -> BenchApp {
    let mut b = BenchApp::headless();
    b.effects_all_off();
    b
}

/// The off arm's lit control: rainbow kitty at full — the one config knob the
/// off fixture is missing.
fn f_rainbow_on() -> BenchApp {
    let mut b = BenchApp::headless();
    b.effects_rainbow_on();
    b
}

/// Neutral, non-lexicon text: it must put INK on the rows (the pet map's
/// input) without summoning sparkle-word decorations, whose multi-second
/// episodes would keep the fixtures from ever settling.
fn bland_line(i: usize) -> Vec<u8> {
    format!("row {i:02} 0123456789 abcdef 0123456789 abcdef\r\n").into_bytes()
}

/// PET-03 fixture: 2-pane split, pet CONFIGURED (`rainbow kitty pet`) but
/// PROVABLY invisible (glow master off), sparkle scanner at its default ON so
/// the per-row ink map is real, both panes carrying enough text that the map
/// has a live edge and inked rows to scan.
fn f_pet_invisible() -> (BenchApp, Instant) {
    let mut b = BenchApp::headless();
    b.pet_configured_glow_off();
    let sid = b.split_stub();
    let t0 = Instant::now();
    b.mark_deco_birth(t0);
    // Establishing compose: a pane the scanner has never seen SPENDS whatever
    // was already on it (the refocus-storm rule) — the rows that must be
    // inked are the ones written after the first frame, like the unit fixture.
    b.compose(t0);
    for s in [0u64, sid] {
        for i in 0..20 {
            b.feed(s, &bland_line(i));
        }
    }
    (b, t0)
}

/// Flood fixture: DEFAULT config (the shipped shape — sparkle on, cursor
/// trail at its default), 2-pane split, the focused (new) pane about to take
/// 8 KiB per frame.
fn f_flood() -> (BenchApp, u64, Instant) {
    let mut b = BenchApp::headless();
    let sid = b.split_stub();
    let t0 = Instant::now();
    b.mark_deco_birth(t0);
    b.compose(t0);
    (b, sid, t0)
}

/// One 8 KiB PTY-shaped chunk: whole build-log-ish lines, exactly
/// `FLOOD_CHUNK` bytes, ~100 lines — a hard scroll every frame.
fn flood_chunk() -> Vec<u8> {
    let mut out = Vec::with_capacity(FLOOD_CHUNK);
    let mut i = 0usize;
    while out.len() < FLOOD_CHUNK {
        let line = format!(
            "[ {:3}%] compiling module_{i:05}.o  warnings: 0  time: {:4} ms\r\n",
            i % 100,
            (i * 37) % 1000
        );
        out.extend_from_slice(line.as_bytes());
        i += 1;
    }
    out.truncate(FLOOD_CHUNK);
    out
}

/// Keystroke-echo fixture: single pane, default config, a prompt on screen.
fn f_echo() -> (BenchApp, Instant) {
    let mut b = BenchApp::headless();
    let t0 = Instant::now();
    b.feed(0, b"user@host demo $ ");
    b.present_frame(t0);
    (b, t0)
}

/// Many-tabs fixture: `n` resident tabs (stub sessions), the ACTIVE one
/// (`push_stub_tab` switches to each new tab, so the last pushed — session
/// `n-1` — is active) showing a screenful of neutral text, then idle.
fn f_many_tabs(n: usize) -> (BenchApp, u64, Instant) {
    let mut b = BenchApp::headless();
    assert!(n >= 1);
    b.push_stub_tabs(n - 1);
    // Sessions are minted 0..n in order, and the compose presents the ACTIVE
    // tab — feed that one, or the fed control below would damage a session no
    // visible pane folds into the RepaintKey.
    let active_sid = (n - 1) as u64;
    let t0 = Instant::now();
    for i in 0..12 {
        b.feed(active_sid, &bland_line(i));
    }
    b.compose(t0);
    (b, active_sid, t0)
}

// ------------------------------------------------------------------ verify --

/// PROVE the CF-6 workload's state, then hand back the warmed fixture and its
/// clock so the timed run continues from the verified state.
///
/// The guard set, all two-sided:
///   * DARK: every sampled frame's `glow_fp == 0 && trail_fp == 0`, no fill,
///     no bolt/twinkle override, and `glow_cfg.enabled == false` — the
///     "every cursor effect off" claim, read from the very config the
///     engines were ticked with.
///   * HOT: the cadence intensity stays >= 0.9 across the window — the
///     pulsed arm really is sustaining the state whose per-frame decay cost
///     CF-6 prices (a cold tracker short-circuits before the `powf`).
///   * CONSUMED (the CF-6 witness): the returned `trail_color` on the hot
///     fixture differs from a COLD control's — `ignite`'s heat blend ran on
///     an all-off frame, observed from outside. Pre-fix reality; see the
///     file header for what each fix shape does to this guard.
///   * LIT CONTROL (DarkUnless): the identical script through a fixture
///     missing only the off switch (rainbow kitty on, cursor moving) must
///     light `glow_fp` on most frames — otherwise the off arm's zero would
///     prove nothing.
fn verify_effects_off() -> (BenchApp, Instant) {
    let mut b = f_effects_off();
    let mut now = Instant::now();
    for _ in 0..WARM_FRAMES {
        now += TYPE_DT;
        b.pulse_typing(now);
        b.tick_fx(now);
    }
    let mut dark = 0usize;
    let mut off = 0usize;
    let mut hot_color = None;
    let mut int_min = f32::MAX;
    for _ in 0..SAMPLE_FRAMES {
        now += TYPE_DT;
        b.pulse_typing(now);
        let fx = b.tick_fx(now);
        dark += usize::from(
            fx.glow_fp == 0
                && fx.trail_fp == 0
                && !fx.any_fill
                && !fx.bolt_cursor
                && !fx.twinkle_cursor,
        );
        off += usize::from(!fx.glow_enabled);
        match hot_color {
            None => hot_color = Some(fx.trail_color),
            Some(c) => assert_eq!(
                c, fx.trail_color,
                "effects_off_frame: a saturated cadence must blend one steady colour"
            ),
        }
        int_min = int_min.min(b.cadence_intensity(now));
    }
    let hot_color = hot_color.expect("sampled");
    report(
        "effects_off_frame",
        &format!(
            "dark {dark}/{SAMPLE_FRAMES} | glow disabled {off}/{SAMPLE_FRAMES} | \
             cadence intensity min {int_min:.3} | trail_color {hot_color:#08x}"
        ),
    );
    assert_eq!(
        dark, SAMPLE_FRAMES,
        "effects_off_frame: a lit frame on the off arm"
    );
    assert_eq!(
        off, SAMPLE_FRAMES,
        "effects_off_frame: glow_cfg.enabled must be false"
    );
    assert!(
        int_min >= 0.9,
        "effects_off_frame: cadence went cold ({int_min}) — the arm is not sustaining \
         the state CF-6 prices"
    );

    // COLD control: the identical fixture, never typed. Its trail_color is the
    // unblended base — the two-sided half of the CONSUMED witness.
    let mut cold = f_effects_off();
    let mut cnow = Instant::now();
    let mut cold_color = None;
    for _ in 0..64 {
        cnow += TYPE_DT;
        let fx = cold.tick_fx(cnow);
        assert_eq!(
            cold.cadence_intensity(cnow),
            0.0,
            "cold control must stay cold"
        );
        cold_color = Some(fx.trail_color);
    }
    let cold_color = cold_color.expect("sampled");
    report(
        "effects_off.cold",
        &format!("trail_color {cold_color:#08x} (unblended base)"),
    );
    // FLIPPED WITNESS (the guard this replaces asserted hot != cold). The
    // pre-fix driver ran the cadence triple + ignite blend on every presented
    // frame even with all cursor effects off, so a typed-hot fixture's
    // trail_color moved with cadence heat — and this guard pinned that waste
    // by asserting the difference. The CF-6 early-out fix makes the off frame
    // skip the blend entirely, so hot and cold must now match: an off frame's
    // colour must NOT depend on typing history. If this ever fails again in
    // the hot != cold direction, the driver has regressed to paying the
    // cadence on off frames.
    assert_eq!(
        hot_color, cold_color,
        "CF-6 witness (post-fix): an all-effects-off frame's trail_color moved \
         with cadence heat — the early-out regressed and the driver is paying \
         the ignite blend on off frames again."
    );

    // LIT control (DarkUnless): only the off switch differs; the script adds a
    // cursor walk because a parked cursor never spawns and a control that
    // would have drawn nothing proves nothing.
    let mut lit = f_rainbow_on();
    let mut lnow = Instant::now();
    let mut lit_frames = 0usize;
    for i in 0..(WARM_FRAMES + SAMPLE_FRAMES) {
        lnow += TYPE_DT;
        lit.pulse_typing(lnow);
        let col = u16::try_from(2 + (i % 40)).expect("small col");
        let fx = lit.tick_fx_at(lnow, (4, col));
        if i >= WARM_FRAMES {
            lit_frames += usize::from(fx.glow_fp != 0);
        }
    }
    report(
        "effects_off.control",
        &format!("rainbow-on twin lit {lit_frames}/{SAMPLE_FRAMES}"),
    );
    assert!(
        lit_frames > SAMPLE_FRAMES / 2,
        "effects_off_frame: the CONTROL (identical script, off switch removed) drew \
         almost nothing ({lit_frames}/{SAMPLE_FRAMES}) — the off arm's zero would be \
         measuring a script with no light to suppress"
    );
    (b, now)
}

/// PROVE the PET-03 workload's state: pet configured, pet invisible, the ink
/// map REAL (sized to the grid, mostly inked, live edge present), and the
/// settled-recompose steady state pinned (FL-1). Returns the warmed fixture +
/// clock + the ink-map counts for the volume group.
fn verify_pet_invisible() -> (BenchApp, Instant, usize, usize) {
    let (mut b, t0) = f_pet_invisible();
    let mut now = t0;
    for _ in 0..WARM_FRAMES {
        now += FRAME_DT;
        b.compose(now);
    }
    assert!(
        b.pet_mode(),
        "pet_invisible_frame: the pet style is not configured"
    );
    assert!(
        !b.glow_enabled(),
        "pet_invisible_frame: glow enabled — the pet could be visible and the \
         hoist under price would gate nothing"
    );
    let (map_rows, inked_rows, live) = b.pet_ink_probe();
    report(
        "pet_invisible_frame",
        &format!("ink map rows {map_rows} | inked {inked_rows} | live edge {live:?}"),
    );
    assert!(
        (1..=ROWS).contains(&map_rows) && map_rows >= 20,
        "pet_invisible_frame: ink map has {map_rows} rows — the scanner is not \
         feeding the map this workload's O(rows) claim is about"
    );
    assert!(
        inked_rows >= 10,
        "pet_invisible_frame: only {inked_rows} inked rows — the fed text never \
         reached the scanner"
    );
    assert!(live.is_some(), "pet_invisible_frame: no live output edge");
    let mut presented = 0usize;
    for _ in 0..SAMPLE_FRAMES {
        now += FRAME_DT;
        presented += usize::from(b.compose(now));
    }
    report(
        "pet_invisible.settled",
        &format!("presented {presented}/{SAMPLE_FRAMES} (settled content still recomposes — FL-1)"),
    );
    // MEASURED REALITY, pinned two-sided (bench finding FL-1, full story in
    // the file header): a SETTLED composed window still repaints on EVERY
    // frame, so this workload prices the FULL settled recompose — which is
    // precisely the per-frame fixed overhead PET-03's unconditional pet feed
    // belongs to. If a fix ever lets settled composes early-out, this pin
    // flips to 0 and the workload becomes the early-out price; both states
    // are meaningful, and the flip itself is the fix's measured proof.
    assert_eq!(
        presented, SAMPLE_FRAMES,
        "pet_invisible_frame: the settled-compose behavior changed (frames now \
         early-out) — re-pin this guard and re-read the FL-1 note"
    );
    // The fed CONTROL: fresh bytes must also present (the trivial direction,
    // kept so a future early-out fix cannot silently kill presentation).
    b.feed(0, b"x");
    now += FRAME_DT;
    assert!(
        b.compose(now),
        "pet_invisible_frame: the fed control did not present"
    );
    // Re-settle before timing so the timed run continues from the verified
    // settled state.
    for _ in 0..8 {
        now += FRAME_DT;
        b.compose(now);
    }
    (b, now, map_rows, inked_rows)
}

/// PROVE the flood workload's state: EVERY frame presents (8 KiB of fresh
/// grid damage per frame), sustained across the window.
fn verify_flood() -> (BenchApp, u64, Instant) {
    let (mut b, sid, t0) = f_flood();
    let chunk = flood_chunk();
    assert_eq!(chunk.len(), FLOOD_CHUNK);
    let mut now = t0;
    for _ in 0..WARM_FRAMES {
        now += FRAME_DT;
        b.feed(sid, &chunk);
        b.compose(now);
    }
    let mut presented = 0usize;
    for _ in 0..SAMPLE_FRAMES {
        now += FRAME_DT;
        b.feed(sid, &chunk);
        presented += usize::from(b.compose(now));
    }
    report(
        "flood_present",
        &format!(
            "presented {presented}/{SAMPLE_FRAMES} | {FLOOD_CHUNK} B/frame into the focused pane"
        ),
    );
    assert_eq!(
        presented, SAMPLE_FRAMES,
        "flood_present: a flooded frame did not present — the compose is not hot"
    );
    (b, sid, now)
}

/// PROVE the echo workload's state: every frame's rasterized pixels differ
/// from the previous frame's (the echo visibly landed), and the just-typed
/// glyph is on the presented grid one cell left of the live cursor.
fn verify_echo() -> (BenchApp, Instant, u32) {
    let (mut b, t0) = f_echo();
    let mut now = t0;
    let mut k = 0u32;
    let mut prev = 0u64;
    let mut checked = 0usize;
    for i in 0..(WARM_FRAMES + SAMPLE_FRAMES) {
        let c = echo_arm(&mut b, &mut now, &mut k);
        let sum = b.present_frame(now);
        if i >= WARM_FRAMES {
            assert_ne!(
                sum, prev,
                "keystroke_echo: two consecutive frames rasterized identically — \
                 the echo never reached the pixels"
            );
            if let Some(ch) = c {
                let (row, col) = b.cursor_pos();
                assert!(col >= 1, "echo cursor at column 0 after a printable echo");
                assert_eq!(
                    b.scratch_cell(usize::from(row), usize::from(col) - 1),
                    ch,
                    "keystroke_echo: the echoed glyph is not on the presented grid"
                );
                checked += 1;
            }
        }
        prev = sum;
    }
    report(
        "keystroke_echo",
        &format!("echo glyph verified on {checked}/{SAMPLE_FRAMES} frames (rest: line wraps)"),
    );
    assert!(
        checked > SAMPLE_FRAMES * 8 / 10,
        "keystroke_echo: too few verified echoes"
    );
    (b, now, k)
}

/// The keystroke arm: one HUMAN key through the real input seam, its echo fed
/// by hand (the loop a real PTY closes), a `\r\n` every `ECHO_LINE` strokes so
/// the line never hits the right margin. Returns the echoed char, `None` on
/// the wrap strokes (their frame moves the cursor, not a new glyph).
fn echo_arm(b: &mut BenchApp, now: &mut Instant, k: &mut u32) -> Option<char> {
    *now += TYPE_DT;
    *k += 1;
    if (*k).is_multiple_of(ECHO_LINE) {
        b.feed(0, b"\r\nuser@host demo $ ");
        return None;
    }
    let c = char::from(b'a' + u8::try_from(*k % 26).expect("mod 26"));
    assert!(b.keystroke(c), "keystroke rejected by the input seam");
    b.feed(0, &[c as u8]);
    Some(c)
}

/// PROVE the many-tabs workload's state: N tabs resident, the settled
/// recompose steady state pinned (FL-1), the fed control presenting.
fn verify_many_tabs(n: usize) -> (BenchApp, Instant) {
    let (mut b, active_sid, t0) = f_many_tabs(n);
    let mut now = t0;
    for _ in 0..WARM_FRAMES {
        now += FRAME_DT;
        b.compose(now);
    }
    assert_eq!(
        b.tab_count(),
        n,
        "many_tabs_idle: fixture staged the wrong tab count"
    );
    let mut presented = 0usize;
    for _ in 0..SAMPLE_FRAMES {
        now += FRAME_DT;
        presented += usize::from(b.compose(now));
    }
    report(
        &format!("many_tabs_idle/{n}"),
        &format!("tabs {n} | presented {presented}/{SAMPLE_FRAMES} (settled recompose — FL-1)"),
    );
    // Same FL-1 pin as pet_invisible_frame: a settled window recomposes every
    // frame, so this workload prices the steady-state frame as tab count
    // scales. Two-sided on purpose — a future early-out fix flips it to 0,
    // and that flip is the fix's own measured proof.
    assert_eq!(
        presented, SAMPLE_FRAMES,
        "many_tabs_idle: the settled-compose behavior changed (frames now \
         early-out) — re-pin this guard and re-read the FL-1 note"
    );
    b.feed(active_sid, b"y");
    now += FRAME_DT;
    assert!(
        b.compose(now),
        "many_tabs_idle: the fed control did not present"
    );
    for _ in 0..8 {
        now += FRAME_DT;
        b.compose(now);
    }
    (b, now)
}

// ---------------------------------------------------------------- counting --

/// Record a COUNT as a criterion measurement — 1 ns == 1 item, verbatim the
/// aterm-effects convention (see cursor_glow_tick.rs for why the spin loop
/// and the `k % 4` jitter are not ceremony: the spin keeps criterion's
/// wall-clock warm-up from doubling `iters` forever, and the jitter keeps the
/// sample distribution non-degenerate so the PDF plot never divides by zero).
fn bench_count(g: &mut BenchmarkGroup<'_, WallTime>, id: &str, count: usize) {
    assert!(
        count > 0,
        "{id}: a zero count cannot be recorded as a duration"
    );
    let n = count as u64;
    let mut k = 0u64;
    g.bench_function(BenchmarkId::from_parameter(id), |b| {
        b.iter_custom(|iters| {
            let mut spin = 0u64;
            for i in 0..iters {
                spin = spin.wrapping_add(black_box(i));
            }
            black_box(spin);
            k = k.wrapping_add(1);
            Duration::from_nanos(n.saturating_mul(iters).saturating_add(k % 4))
        });
    });
}

// -------------------------------------------------------------- the groups --

#[allow(
    clippy::too_many_lines,
    reason = "one linear registry of verified workloads, the effects-bench shape"
)]
fn frame_latency(c: &mut Criterion) {
    // PROVE FIRST, TIME SECOND: every fixture is built, warmed and verified
    // before a nanosecond is measured; the timed run continues from the
    // verified state with the same arm sustaining it.
    let (mut fx_off, mut off_now) = verify_effects_off();
    let (mut pet, mut pet_now, pet_map_rows, pet_inked) = verify_pet_invisible();
    let (mut flood, flood_sid, mut flood_now) = verify_flood();
    let chunk = flood_chunk();
    let (mut echo, mut echo_now, mut echo_k) = verify_echo();
    let mut tabs: Vec<(usize, BenchApp, Instant)> = [2usize, 8, 32]
        .into_iter()
        .map(|n| {
            let (b, now) = verify_many_tabs(n);
            (n, b, now)
        })
        .collect();

    {
        let mut group = c.benchmark_group("frame_latency");
        // THE FLOOR UNDER EVERY NUMBER IN THIS GROUP: the `Instant::now()`
        // pair with an empty span between them.
        group.bench_function("timer_floor", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let t0 = Instant::now();
                    black_box(0u64);
                    total += t0.elapsed();
                }
                total
            });
        });
        // 1. CF-6: the whole per-frame cursor-effect driver on an all-off
        // frame — policy resolves, config folds, cadence triple, ignite,
        // every engine's own early-out. The arm keeps the cadence saturated.
        group.bench_function("effects_off_frame", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    off_now += TYPE_DT;
                    fx_off.pulse_typing(off_now);
                    let t0 = Instant::now();
                    black_box(fx_off.tick_fx(off_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 2. PET-03: a SETTLED composed frame with the pet configured but
        // invisible — the steady per-frame recompose (see the FL-1 pin in its
        // verify fn) whose fixed overhead includes the unconditional pet feed
        // the finding's hoist would delete.
        group.bench_function("pet_invisible_frame", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    pet_now += FRAME_DT;
                    let t0 = Instant::now();
                    black_box(pet.compose(pet_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 3. HEAVY LOAD: 8 KiB ingested (untimed arm), the hot compose timed.
        group.bench_function("flood_present", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    flood_now += FRAME_DT;
                    flood.feed(flood_sid, &chunk);
                    let t0 = Instant::now();
                    black_box(flood.compose(flood_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 4. The latency-critical path: keystroke armed, the echoing frame
        // (extract + effects + real CPU raster) timed.
        group.bench_function("keystroke_echo", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    echo_arm(&mut echo, &mut echo_now, &mut echo_k);
                    let t0 = Instant::now();
                    black_box(echo.present_frame(echo_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // 5. Steady state as resident state scales.
        for (n, b_app, now) in tabs.iter_mut() {
            group.bench_function(BenchmarkId::new("many_tabs_idle", *n), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        *now += FRAME_DT;
                        let t0 = Instant::now();
                        black_box(b_app.compose(*now));
                        total += t0.elapsed();
                    }
                    total
                });
            });
        }
        group.finish();
    }

    {
        // WHAT THE FRAME NUMBERS EXCLUDE — and the two findings' exact seams,
        // priced alone where a full-frame delta could not resolve them.
        let mut group = c.benchmark_group("frame_latency_seams");
        // CF-6 engine half, both sides of the A/B in one run: the driver's
        // PRE-FIX triple (intensity, intensity, warmth — three lazy decays for
        // one instant) vs the prepared shared sample (one decay). The gui
        // adoption replaces the former with the latter; the difference between
        // these two numbers is the engine-half win, measured on the same hot
        // tracker the effects_off arm sustains.
        group.bench_function("cadence_triple_hot", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    off_now += TYPE_DT;
                    fx_off.pulse_typing(off_now);
                    let t0 = Instant::now();
                    black_box(fx_off.cadence_triple(off_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        group.bench_function("cadence_sample_hot", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    off_now += TYPE_DT;
                    fx_off.pulse_typing(off_now);
                    let t0 = Instant::now();
                    black_box(fx_off.cadence_sample(off_now));
                    total += t0.elapsed();
                }
                total
            });
        });
        // PET-03's exact pair — producer scan + consumer copy — on the
        // verified real map. This is the number the hoist deletes from every
        // pet-invisible frame.
        group.bench_function("pet_ink_feed", |b| {
            b.iter(|| pet.pet_ink_feed());
        });
        // The echo workload's whole untimed arm (real input seam + hand-fed
        // echo), priced under a name that cannot be mistaken for the frame.
        group.bench_function("keystroke_arm", |b| {
            b.iter(|| echo_arm(&mut echo, &mut echo_now, &mut echo_k));
        });
        // The flood workload's untimed arm: 8 KiB through `Terminal::process`
        // under the session lock. Add it to `flood_present` and subtract the
        // timer floor for the cost of one whole flooded frame.
        group.bench_function("flood_feed_8kib", |b| {
            b.iter(|| flood.feed(flood_sid, black_box(&chunk)));
        });
        group.finish();
    }

    {
        // The guard-asserted counts as measurements (1 ns == 1 item), so a
        // count regression is stored and A/B'd like a time regression.
        let mut group = c.benchmark_group("frame_latency_volume");
        group
            .warm_up_time(Duration::from_millis(1))
            .measurement_time(Duration::from_millis(10))
            .sample_size(10);
        bench_count(&mut group, "flood_present/bytes_per_frame", FLOOD_CHUNK);
        bench_count(&mut group, "pet_invisible_frame/ink_map_rows", pet_map_rows);
        bench_count(&mut group, "pet_invisible_frame/inked_rows", pet_inked);
        group.finish();
    }
}

criterion_group!(benches, frame_latency);
criterion_main!(benches);
