// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// `EffectsPipeline::apply` — the per-frame DRIVER the web embedders
// (aterm-wasm / aterm-gpu-web) run every presented frame: the one function
// that ticks every engine, folds the frame's theme/cursor colours, flushes
// straggler keystrokes into the typing cadence, and splices 18 overlay
// channels into the host's resident `RenderInput`. Every other bench in this
// crate prices an ENGINE; none of them constructs the pipeline, so the
// driver's own per-frame overhead — the `enabled_any()` early-out, the
// unconditional `ignite()` + two `TypingCadence` `powf` decays (driver-01),
// the disabled-glow dark teardown (driver-02), the ~520-byte `GlowConfig`
// copy at pipeline.rs:1144 (driver-05), the 12 `mem::swap`s — has never been
// measured. This file is that instrument, and the gate any pipeline-level fix
// must stand before.
//
// WHAT IS TIMED, EXACTLY. `apply` AND NOTHING ELSE. One iteration is:
//
//     arm(&mut f);                 // UNTIMED: PTY bytes, note_keystroke,
//                                  //          advance(dt), cell_frame_into
//     let t0 = Instant::now();     //  ── timed span opens
//     black_box(f.apply());        //  the driver, and only the driver
//     total += t0.elapsed();       //  ── timed span closes
//
// The arm reproduces the host frame contract in the host's exact order
// (aterm-wasm `render`): key events land between rAFs (`term.process` +
// `note_keystroke`), the rAF callback calls `advance(dt)`, `render` refills
// the RESIDENT scratch via `cell_frame_into`, then calls `apply`. Two details
// of that contract are load-bearing and a naive bench gets both wrong:
//
//   * RESIDENT `RenderInput`. `apply` hands its scratch buffers to the caller
//     by `mem::swap`, so a fresh `RenderInput` per frame would throw away
//     every pooled capacity and measure the allocator, not the driver.
//     `cell_frame_into` on the kept scratch is the shipping pattern (wasm E8).
//   * `cell_frame_into` stamps `default_bg`/`default_fg`/`cursor_color` and
//     `snapshot_seq = damage_epoch()` from the live terminal, so the theme
//     fold at pipeline.rs:1068, the auto-colour follow, and rain's
//     torn-snapshot gate all run their REAL branches, not the sentinel
//     short-circuits.
//
// The arm's own cost (dominated by the 80x24 `cell_frame_into`) is priced
// separately in `effects_pipeline_host_seams`, and the `Instant::now()` pair
// inside every reported number is priced by `effects_pipeline/timer_floor` —
// subtract it for an absolute cost; it cancels in an A/B.
//
// THE CLOCK IS INJECTED. The pipeline is clockless by contract (`t0` captured
// at construction, time advanced only by `advance(dt_ms)`); every workload
// advances it by exactly 1000/60 ms per frame and never samples the wall
// (criterion's clock stays entirely separate). Same dt stream + same PTY
// bytes => identical frames, so the volume bounds below are a modest margin
// around the MEASURED peak, not an order-of-magnitude envelope.
//
// WHAT EACH WORKLOAD PRICES, keyed to the audit findings (driver.json):
//
//   tick_all_off      Nothing ever enabled. `apply` must take the
//                     `enabled_any()` early-out: ~21 channel clears and
//                     return 0. THE driver-off number — the floor every other
//                     number is read against, and the pipeline half of the
//                     "effects off must cost nothing" contract.
//
//   tick_one_on       ONE engine on (sparkle), grid full of NON-lexicon text,
//                     no damage between frames. THE probe for driver-01 and
//                     driver-02: `enabled_any()` is a single OR, so this
//                     frame pays the full per-frame entry of THREE disabled
//                     engines — the ungated `ignite()` + two `powf` cadence
//                     decays (driver-01; the cadence was heated once during
//                     warm-up, so `last` is `Some` forever — the finding's
//                     "paid for the rest of the session" state), the
//                     disabled-glow dark teardown of ~60-80 idempotent stores
//                     (driver-02), the ~520-byte `GlowConfig` copy at :1144
//                     (driver-05's pipeline half — `Option<TrailParams>` is
//                     inline, so the copy is full-size even with no pack
//                     armed), and the 12 swaps. Both fixes land here or
//                     nowhere: this number falling is their proof.
//
//   apply_idle        Wake enabled (glow + trail), engines SETTLED: cursor
//                     still, cadence cold, nothing to draw. The "effect on,
//                     user reading" frame — and still a driver-01 frame: the
//                     two `powf` decays run against a `Some(last)` cadence
//                     every frame of a session's idle hours.
//
//   apply_typing      The realistic keystroke frame: one key per 60 fps frame
//                     (held-key autorepeat), live lumen aurora + comet trail.
//                     What a typing user actually pays per present, and the
//                     A/B baseline for apply_typing_pack.
//
//   apply_typing_pack The same frame with a compiled Trail Pack armed — the
//                     configuration where driver-05's per-frame `GlowConfig`
//                     copies (pipeline :1144 + the engine's `last_cfg` store)
//                     carry a POPULATED ~440-byte `TrailParams`, and the
//                     configuration the finding's refactor risk lives in.
//                     The gate for that fix.
//
//   tick_all_on       Everything on at once — glow + trail + sparkle (live
//                     lexicon matches) + PHOSPHOR rain — in the shipped
//                     agent-session shape: the user typed a prompt (cadence
//                     armed for life), submitted (TurnStart lifts rain's
//                     composer freeze), and now the agent STREAMS one byte
//                     per frame while the user watches. The grid damages
//                     every frame (sparkle rescans + rain re-scans occupancy
//                     every frame, the session scrolls at steady state) and
//                     the rain weather ladder sees the sustained content
//                     deltas it climbs to a WORKING downpour on. The headline
//                     worst-case presented frame. Output bytes are NOT armed
//                     as keystrokes — that distinction is load-bearing: a
//                     stream mis-armed as typing pins rain at CALM with its
//                     material sampler frozen, and this workload measured
//                     ZERO rain quads in that shape before it was fixed.
//
// EVERY WORKLOAD IS GUARDED BEFORE IT IS TIMED, with TWO-SIDED bounds. The
// dark workloads' zeros are proven meaningful by CONTROLS (the identical
// script through a pipeline missing only the off switch / the no-match grid,
// which must light up), because "emitted nothing" is also what a dead engine
// reports. The early-out question — did `apply` take the `enabled_any()`
// return? — is settled exactly, not statistically: `enabled_any()` is public
// and pure, and the early-out is its literal negation, so asserting it TRUE
// proves every sampled frame ran the full driver body (the code driver-01/02/
// 05 live in), and asserting it FALSE proves the off number measured the
// early-out and nothing else.
//
// DRIVER-01's ARITHMETIC IS PINNED FROM OUTSIDE. The only externally visible
// product of the `intensity()`/`warmth()` `powf` pair is the ignited trail
// colour published through `input.cursor_trail_color` (`heat_color(base, q)`).
// `apply_typing` saturates the cadence (one key per frame >> the 220 ms
// half-life), so intensity is EXACTLY 1.0 at every apply instant (the
// advance-replay lands the last key at the apply `now`, dt = 0) and the
// published colour must equal `heat_color(TRAIL_BASE, 1.0)` — precomputed
// below as TRAIL_HOT — on EVERY sampled frame. `apply_idle` and
// `tick_all_on` decay the same cadence >= 10 s (>> 45 half-lives), so
// intensity is exactly 0.0 and the published colour must be EXACTLY the
// configured base on every frame — once with the cursor still, once with it
// in full streaming motion. The pins bracket the decay computation a
// driver-01 fix must reproduce bit-for-bit: hoisting the redundant decay
// cannot move any of them, but a botched merge of the two accessors moves
// one and this file goes red.
//
// EMITTED VOLUME IS A MEASUREMENT, NOT A PRINTOUT (the cursor_glow_volume
// idiom): every stream a workload proves non-empty has its verified peak and
// window total handed to criterion as counts-as-nanoseconds benchmarks in
// `effects_pipeline_volume`, so a count regression is stored, baselined and
// A/B'd by the same tooling that catches a time regression.
//
// WHAT THE FIRST FULL RUN MEASURED (m21, 2026-08-19, --warm-up-time 1
// --measurement-time 2; recorded so the numbers below are read with their
// shape, not re-derived from scratch):
//
//   timer_floor        13.0 ns   the Instant pair inside every number below
//   tick_all_off       15.1 ns   => the early-out itself is ~2 ns of driver
//   tick_one_on        54.2 ns   => ~41 ns of pure disabled-engine entry —
//                                 the driver-01/02/05 surface, ~20x the
//                                 early-out, with NOTHING drawn. This is the
//                                 number those fixes must collapse.
//   apply_idle        115.7 ns   an ENABLED wake with nothing left to draw
//   apply_typing      19.98 µs   engine-dominated (peak 4 515 glow quads)
//   apply_typing_pack 21.77 µs   the same frame carrying the resolved pack
//   tick_all_on       27.05 µs   the headline frame (glow 2 362 + ink 96 +
//                                 rain 177 peak items, rescan every frame)
//   typing_arm seam   10.9 µs    the UNTIMED host half (cell_frame_into);
//   idle_arm seam    191.9 ns    excluded from every apply number above
//
// Read tick_one_on against tick_all_off, not against zero: the gap IS the
// per-frame price of `enabled_any()` being a single OR over four masters.

use std::time::Duration;
use std::time::Instant as WallInstant;

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_effects::pipeline::EffectsPipeline;
use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, black_box, criterion_group, criterion_main,
};

// ---------------------------------------------------------------- geometry --

/// A classic 80x24 session at 1x metrics. The driver overhead under test is
/// O(1) in the grid (swaps, clears, the config copy, the cadence decays); the
/// O(rows*cols) costs that DO scale (sparkle rescan, rain occupancy) have
/// their own engine benches with their own grid sweeps, so one honest shape
/// suffices here and keeps every workload comparable against the same floor.
const ROWS: usize = 24;
const COLS: usize = 80;
const CELL_W: usize = 10;
const CELL_H: usize = 20;

/// 60 fps rAF delta — the dt every workload advances the injected clock by.
/// Typing workloads note one keystroke per frame: 60 keys/s is held-key
/// autorepeat, the shipped worst realistic cadence (and what pins the cadence
/// at its `knee_hi` saturation so the TRAIL_HOT colour pin is exact).
const FRAME_MS: f64 = 1000.0 / 60.0;

// ------------------------------------------------------------------ config --

/// The theme cursor handed to every setter (the docs' green).
const THEME_CURSOR: u32 = 0x0050_FA7B;

/// EXPLICIT trail colour (`Some`), so `trail_color_from_cursor` is false and
/// the per-frame auto-follow can never rebind the base under the pin below.
const TRAIL_BASE: u32 = 0x0050_FA7B;

/// `heat_color(TRAIL_BASE, 1.0)` — the comet-core blend at full ignition,
/// precomputed by the same arithmetic (t = 1.0 * HEAT_COLOR_MAX = 0.5, per
/// channel `round(a + (HOT - a) * t)` toward HOT = 0x00FF_E6B0):
///   R: 0x50 = 80  -> round(80  + 175 * 0.5) = 168 = 0xA8
///   G: 0xFA = 250 -> round(250 -  20 * 0.5) = 240 = 0xF0
///   B: 0x7B = 123 -> round(123 +  53 * 0.5) = 150 = 0x96
/// The saturated-typing publish must equal this on EVERY sampled frame — the
/// external witness that the driver-01 `powf` pair is live and hot.
const TRAIL_HOT: u32 = 0x00A8_F096;

/// Explicit glow colour (`Some`) for the same reason as TRAIL_BASE: the
/// auto-colour follow at pipeline.rs:1082 stays configured-off, so the frame
/// stream is bit-stable however the terminal stamps its cursor colour.
const GLOW_COLOR: u32 = 0x00D0_D0D0;

/// The compiled Trail Pack for apply_typing_pack: the shipped synthwave
/// asset, so the ~520-byte copies under test carry a real resolved pack.
const SYNTHWAVE: &str = include_str!("../assets/trail-packs/synthwave.toml");

// ------------------------------------------------------------------ script --

/// One line of the tick_all_on PTY stream, emitted one byte per frame: three
/// sparkle surfaces (profanity + two feline) in the left half, blank right
/// half so rain has empty cells to occupy, CRLF so the session scrolls at
/// steady state (rescan-every-frame + scroll is the honest streaming shape).
const STREAM_LINE: &[u8] = b"fuck cat dog kitty aaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n";

/// A full-width line with NO lexicon surface on it, for the tick_one_on grid:
/// the sparkle engine must scan it and conclude there is nothing to decorate.
const NOISE_LINE: &[u8] =
    b"qwrt yuio psdf ghjk lzxc vbnm qwrt yuio psdf ghjk lzxc vbnm qwrt yuio psdf\r\n";

/// The tick_one_on CONTROL's grid line: the same shape, saturated with
/// lexicon matches — what proves the engine in tick_one_on was alive.
const MATCH_LINE: &[u8] = b"cat dog fuck kitty cat dog fuck kitty cat dog fuck kitty\r\n";

/// Frames sampled by `verify_reaches_target` — 10 s of session, several full
/// cycles of the longest periodic script (the 49-frame STREAM_LINE).
const SAMPLE_FRAMES: usize = 600;

// ----------------------------------------------------------------- fixture --

/// One workload's whole world: the pipeline, its terminal, the RESIDENT
/// render-input scratch (the buffer-pooling contract above), and the script
/// cursor. Built once, warmed once, then stepped — by the verifier first and
/// by criterion after, so the timed run continues from the verified state.
struct Fixture {
    p: EffectsPipeline,
    term: Terminal,
    input: RenderInput,
    /// Byte cursor into STREAM_LINE for the streaming arm.
    si: usize,
}

impl Fixture {
    fn new(p: EffectsPipeline) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let input = term.cell_frame(ROWS, COLS);
        Fixture {
            p,
            term,
            input,
            si: 0,
        }
    }

    /// Seed the grid with `ROWS - 1` copies of `line` (cursor parks on the
    /// last row), then let the FIRST warm frame consume the damage — after
    /// that the no-write arms animate over a stable epoch, which is exactly
    /// the "no damage between frames" state tick_one_on claims.
    fn seed_grid(&mut self, line: &[u8]) {
        for _ in 0..ROWS - 1 {
            self.term.process(line);
        }
    }

    /// THE TIMED UNIT. Nothing here but the driver call.
    fn apply(&mut self) -> u64 {
        self.p
            .apply(&mut self.term, &mut self.input, CELL_W, CELL_H)
    }
}

// --------------------------------------------------------------------- arm --

/// The HOST-SIDE half of one frame, in the host's exact order (see header).
type Arm = fn(&mut Fixture);

/// One whole frame — arm, then apply. What `warm`/`run` drive; NOT what the
/// `effects_pipeline` group times.
fn step(f: &mut Fixture, arm: Arm) -> u64 {
    arm(f);
    f.apply()
}

/// A held key at autorepeat: the glyph echoes (cursor advances, grid damages),
/// the keystroke is noted for the cadence, the clock advances one frame, the
/// resident snapshot refills.
fn arm_typing(f: &mut Fixture) {
    f.term.process(b"a");
    f.p.note_keystroke();
    f.p.advance(FRAME_MS);
    f.term.cell_frame_into(&mut f.input, ROWS, COLS);
}

/// Nothing changed: no bytes, no keys — just the rAF advance and the host's
/// unconditional snapshot refill (the wasm render loop refills every frame
/// while effects are enabled, damaged or not).
fn arm_idle(f: &mut Fixture) {
    f.p.advance(FRAME_MS);
    f.term.cell_frame_into(&mut f.input, ROWS, COLS);
}

/// A short burst of NON-ECHOING keys (warm-up only): arms the typing cadence
/// through its one public seam so `TypingCadence::last` becomes `Some` — the
/// state driver-01 says is then held for the rest of the session — without
/// writing a byte or moving the cursor.
fn arm_keys_no_echo(f: &mut Fixture) {
    f.p.note_keystroke();
    f.p.advance(FRAME_MS);
    f.term.cell_frame_into(&mut f.input, ROWS, COLS);
}

/// The streaming session: one STREAM_LINE byte per frame — an agent/shell
/// PRINTING while the user watches. Deliberately NO `note_keystroke`: output
/// is not typing, and the distinction is load-bearing twice over — keystrokes
/// pin the rain engine's weather at CALM and freeze its material sampler (the
/// composer gate), so a stream armed as keystrokes measures a rain engine
/// that can never rain; and the cold cadence is what lets tick_all_on pin the
/// driver-01 decay at its EXACT zero (TRAIL_BASE) while the cursor is in
/// full motion.
fn arm_stream(f: &mut Fixture) {
    let b = STREAM_LINE[f.si];
    f.si = (f.si + 1) % STREAM_LINE.len();
    f.term.process(&[b]);
    f.p.advance(FRAME_MS);
    f.term.cell_frame_into(&mut f.input, ROWS, COLS);
}

// -------------------------------------------------------------------- warm --

/// Typing warm long enough to pass the first full-screen SCROLL (80x24 fills
/// after 1920 typed glyphs), so the verified window and the timed run both
/// sit in the scrolled steady state rather than straddling the transition.
fn warm_typing(f: &mut Fixture) {
    for _ in 0..2_400 {
        step(f, arm_typing);
    }
}

/// The all-off warm: brief, because there is no state to converge — the
/// early-out is frame-one steady — but long enough to prove it stays taken
/// under sustained typing.
fn warm_brief(f: &mut Fixture) {
    for _ in 0..300 {
        step(f, arm_typing);
    }
}

/// Type, then stop: 300 typing frames light the wake, then 10 s of idle
/// frames let every spark expire (650 ms life) and the cadence decay through
/// at least 45 half-lives to exactly-0 intensity. The settled state
/// apply_idle claims — reached the way a real session reaches it.
fn warm_settle(f: &mut Fixture) {
    for _ in 0..300 {
        step(f, arm_typing);
    }
    for _ in 0..600 {
        step(f, arm_idle);
    }
}

/// The tick_one_on warm: consume the seed damage and let entrances finish
/// (240 idle frames), heat the cadence once (30 key frames — after this
/// `TypingCadence::last` is `Some` for the rest of the fixture's life, the
/// driver-01 session state), then 10 s idle so the sampled frames are the
/// steady "user watching a still screen" the workload names.
fn warm_watch(f: &mut Fixture) {
    for _ in 0..240 {
        step(f, arm_idle);
    }
    for _ in 0..30 {
        step(f, arm_keys_no_echo);
    }
    for _ in 0..600 {
        step(f, arm_idle);
    }
}

/// The streaming warm, in the shipped agent-session shape: the user TYPES the
/// prompt (30 echoed keystrokes — which also makes `TypingCadence::last`
/// `Some` for the rest of the fixture's life, the driver-01 session state),
/// the submit handler reports TurnStart (code 10 — the one signal that lifts
/// the rain material sampler's composer freeze), then the agent streams. 1600
/// output frames run past the first scroll (24 lines x 49 bytes = 1176
/// frames) AND give the rain weather ladder the sustained content deltas
/// (`note_activity` sees a fresh `content_seq` every apply) it climbs to a
/// WORKING downpour on — the same public-seam route tests/rain_bench.rs
/// documents.
fn warm_stream(f: &mut Fixture) {
    for _ in 0..30 {
        step(f, arm_typing);
    }
    f.p.note_matrix_rain_signal(aterm_effects::matrix_rain::RainSignal::TurnStart as u32, 1);
    for _ in 0..1_600 {
        step(f, arm_stream);
    }
}

// ------------------------------------------------------------- observation --

/// Every overlay channel `apply` owns, in RenderInput order. All 13 are
/// sampled every frame: the lit workloads bound the streams they claim and
/// prove the rest EMPTY (a stream lighting up unclaimed is a routing change),
/// and the dark workloads prove all 13 empty at once.
const STREAMS: [&str; 13] = [
    "trail",
    "glow_add",
    "glow_halo",
    "fire_patch",
    "glow_under",
    "char_fg",
    "fire_halo",
    "decos",
    "ink",
    "free_sprites",
    "nova_add",
    "rain_quads",
    "rain_add",
];

#[derive(Clone, Copy, Default)]
struct Volume([usize; 13]);

impl Volume {
    fn of(input: &RenderInput) -> Self {
        Volume([
            input.cursor_trail.len(),
            input.cursor_glow_add.len(),
            input.glow_halo.len(),
            input.fire_patch.len(),
            input.glow_under.len(),
            input.char_fg.len(),
            input.fire_halo.len(),
            input.word_decorations.len(),
            input.ink.len(),
            input.free_sprites.len(),
            input.nova_add.len(),
            input.rain_quads.len(),
            input.rain_add.len(),
        ])
    }

    fn max_with(&mut self, other: Self) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a = (*a).max(b);
        }
    }

    fn add(&mut self, other: Self) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a += b;
        }
    }

    fn total(&self) -> usize {
        self.0.iter().sum()
    }
}

/// What running a script for a while showed.
struct Sampled {
    /// Per-frame peak of each channel.
    peak: Volume,
    /// Sum over the window (per-frame averages + the `total` count bench).
    sum: Volume,
    frames: usize,
    /// Frames whose folded fingerprint was non-zero. `apply` returns 0 from
    /// the early-out AND from a fully settled enabled frame, so this
    /// separates "drew something" from "dark" without a channel read.
    lit: usize,
    /// Frames on which `input.cursor_trail_color` equalled the workload's
    /// pinned value (see TRAIL_HOT / TRAIL_BASE) — the external witness on
    /// the driver-01 cadence arithmetic.
    pin_hits: usize,
}

fn run(f: &mut Fixture, arm: Arm, pin: Option<u32>, frames: usize) -> Sampled {
    let mut s = Sampled {
        peak: Volume::default(),
        sum: Volume::default(),
        frames,
        lit: 0,
        pin_hits: 0,
    };
    for _ in 0..frames {
        let fp = step(f, arm);
        let v = Volume::of(&f.input);
        s.peak.max_with(v);
        s.sum.add(v);
        s.lit += usize::from(fp != 0);
        if let Some(c) = pin {
            s.pin_hits += usize::from(f.input.cursor_trail_color == c);
        }
    }
    s
}

// ------------------------------------------------------------------ guards --

/// Inclusive `[min, max]` bounds on a channel's PEAK length over the sample
/// window. Both sides are load-bearing: the min proves the workload reached
/// the emitter it claims (`>= 0` passes on a dead engine — the mistake this
/// crate's bench discipline exists to prevent), the max catches a count
/// regression a timing number alone would hide. The maxima are a ~+12 %
/// margin over the measured peak; the engines are deterministic under the
/// injected clock, so the measured peak is the same number on every machine.
type Range = (usize, usize);

/// Every channel must stay exactly empty.
const DARK: [Range; 13] = [(0, 0); 13];

/// The extra, decisive proof a workload carries beyond its volume bounds.
enum Witness {
    /// The bounds themselves are decisive.
    Bounds,
    /// This workload must be completely DARK while a CONTROL — the identical
    /// script through a fixture missing only the thing that keeps this one
    /// dark — lights up. What makes a zero a measurement instead of a
    /// tautology.
    DarkUnless {
        what: &'static str,
        build: fn() -> Fixture,
        warm: fn(&mut Fixture),
        arm: Arm,
    },
}

struct Workload {
    name: &'static str,
    /// One line for the report: what state this workload is in.
    note: &'static str,
    build: fn() -> Fixture,
    warm: fn(&mut Fixture),
    arm: Arm,
    /// Master-switch truth asserted straight after `build`. `enabled_any()`
    /// is the EXACT early-out condition (public + pure), so these assertions
    /// settle the "which apply body ran" question structurally, per workload,
    /// for every frame — not statistically.
    assert_cfg: fn(&Fixture),
    bounds: [Range; 13],
    /// Bounds on the % of sampled frames with a non-zero fingerprint.
    lit_pct: Range,
    /// Exact `input.cursor_trail_color` required on EVERY sampled frame
    /// (`None` = no claim). The driver-01 arithmetic pins.
    trail_pin: Option<u32>,
    witness: Witness,
}

/// Build the workload, run it to steady state, and PROVE it is in the state
/// it claims before a single nanosecond is timed. Returns the warmed fixture
/// (the timed run continues from the verified state) and what was observed.
fn verify_reaches_target(w: &Workload) -> (Fixture, Sampled) {
    let mut f = (w.build)();
    (w.assert_cfg)(&f);
    (w.warm)(&mut f);
    let s = run(&mut f, w.arm, w.trail_pin, SAMPLE_FRAMES);

    report(w.name, w.note, &s);
    for (i, (&got, &(lo, hi))) in s.peak.0.iter().zip(w.bounds.iter()).enumerate() {
        assert!(
            got >= lo && got <= hi,
            "{}: peak {} = {got}, outside [{lo}, {hi}] — the workload is not in \
             the state it claims (a COUNT regression, a lost emitter, or a \
             script that stopped reaching its target)",
            w.name,
            STREAMS[i]
        );
    }
    let lit = s.lit * 100 / s.frames;
    assert!(
        lit >= w.lit_pct.0 && lit <= w.lit_pct.1,
        "{}: {lit}% of frames emitted (fingerprint != 0), outside [{}, {}]%",
        w.name,
        w.lit_pct.0,
        w.lit_pct.1
    );
    if w.trail_pin.is_some() {
        assert_eq!(
            s.pin_hits, s.frames,
            "{}: the published cursor_trail_color matched its pinned value on \
             only {} of {} frames — the TypingCadence decay the driver-01 fix \
             must preserve is not in the state this workload claims",
            w.name, s.pin_hits, s.frames
        );
    }

    match w.witness {
        Witness::Bounds => {}
        Witness::DarkUnless {
            what,
            build,
            warm,
            arm,
        } => {
            assert_eq!(
                s.peak.total(),
                0,
                "{}: emitted overlay items on a workload that must stay dark",
                w.name
            );
            assert_eq!(
                s.lit, 0,
                "{}: a non-zero fingerprint on a dark workload",
                w.name
            );
            let mut c = (build)();
            (warm)(&mut c);
            let cs = run(&mut c, arm, None, SAMPLE_FRAMES);
            report(&format!("{}.control", w.name), what, &cs);
            assert!(
                cs.lit > 0 && cs.sum.total() > 0,
                "{}: the CONTROL ({what}) drew nothing either, so this \
                 workload's zero proves nothing — it would be measuring a \
                 script that had no light to suppress",
                w.name
            );
        }
    }
    (f, s)
}

/// The human-readable per-workload volume line: peak and per-frame average of
/// every channel plus the lit-frame count. The peaks and window total also
/// become criterion measurements (`effects_pipeline_volume`); this line is
/// what a human reads while the run scrolls by.
fn report(name: &str, note: &str, s: &Sampled) {
    let peaks: Vec<String> = s.peak.0.iter().map(ToString::to_string).collect();
    let avgs: Vec<String> = s.sum.0.iter().map(|v| (v / s.frames).to_string()).collect();
    println!(
        "VOLUME {name:<22} | peak {} {} | avg {} | lit {}/{} | {note}",
        STREAMS.join("/"),
        peaks.join("/"),
        avgs.join("/"),
        s.lit,
        s.frames
    );
}

// --------------------------------------------------------------- fixtures ---

/// Arm the LUMEN aurora + the opaque comet trail with explicit colours (so
/// the auto-follow branch never rebinds them under the pins) at the shipped
/// worst-case dials (650 ms / 512, the in-crate gates' pair).
fn arm_wake(p: &mut EffectsPipeline) {
    p.set_cursor_glow(
        true,
        "lumen",
        Some(GLOW_COLOR),
        None,
        650,
        512,
        1.0,
        0.9,
        true,
        THEME_CURSOR,
    );
    p.set_cursor_trail(true, 650, 512, Some(TRAIL_BASE), THEME_CURSOR);
}

/// Arm sparkle words exactly as bench_design's state setup does: all shipped
/// classes but orca, the rainbow profanity style with a supernova chance, and
/// animated ink — the full resolved config a real session runs.
fn arm_sparkle(p: &mut EffectsPipeline) {
    p.set_sparkle_enabled(true);
    p.set_sparkle_classes(true, true, false, true);
    p.set_sparkle_profanity("rainbow", 8, 900, 3, 1.0, true, 10);
    p.set_sparkle_ink(true, 1.0, 1200, false);
}

/// Arm PHOSPHOR rain at the bench_design knobs (30 fps engine cadence,
/// density 6, output material on, fixed seed).
fn arm_rain(p: &mut EffectsPipeline) {
    p.set_matrix_rain(
        30,
        6,
        5,
        5,
        None,
        None,
        "matrix",
        None,
        400,
        30,
        false,
        true,
        true,
        true,
        0x00C0_FFEE,
        0x0011_1318,
        0x00C8_D3F5,
    );
    p.set_matrix_rain_enabled(true);
}

fn f_all_off() -> Fixture {
    let mut f = Fixture::new(EffectsPipeline::new());
    f.seed_grid(NOISE_LINE);
    f
}

fn f_wake() -> Fixture {
    let mut p = EffectsPipeline::new();
    arm_wake(&mut p);
    Fixture::new(p)
}

fn f_wake_pack() -> Fixture {
    let mut p = EffectsPipeline::new();
    arm_wake(&mut p);
    let err = p.set_cursor_trail_pack(Some(SYNTHWAVE.to_string()));
    assert!(
        err.is_none(),
        "synthwave trail pack failed to compile: {err:?} — apply_typing_pack \
         would silently price the built-in lumen path instead of the pack copies"
    );
    Fixture::new(p)
}

fn f_sparkle_no_match() -> Fixture {
    let mut p = EffectsPipeline::new();
    arm_sparkle(&mut p);
    let mut f = Fixture::new(p);
    f.seed_grid(NOISE_LINE);
    f
}

fn f_sparkle_matches() -> Fixture {
    let mut p = EffectsPipeline::new();
    arm_sparkle(&mut p);
    let mut f = Fixture::new(p);
    f.seed_grid(MATCH_LINE);
    f
}

fn f_all_on() -> Fixture {
    let mut p = EffectsPipeline::new();
    arm_wake(&mut p);
    arm_sparkle(&mut p);
    arm_rain(&mut p);
    Fixture::new(p)
}

// --------------------------------------------------------------- workloads --

fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "tick_all_off",
            note: "nothing enabled; the enabled_any() early-out, typing",
            build: f_all_off,
            warm: warm_brief,
            arm: arm_typing,
            assert_cfg: |f| {
                assert!(
                    !f.p.enabled_any(),
                    "tick_all_off: a master switch is on — this would time the \
                     full driver body and call it the off number"
                );
            },
            bounds: DARK,
            lit_pct: (0, 0),
            trail_pin: None,
            witness: Witness::DarkUnless {
                what: "the same typing script with the wake enabled",
                build: f_wake,
                warm: warm_typing,
                arm: arm_typing,
            },
        },
        Workload {
            name: "tick_one_on",
            note: "sparkle on, zero matches, no damage; 3 disabled engines' entry",
            build: f_sparkle_no_match,
            warm: warm_watch,
            arm: arm_idle,
            assert_cfg: |f| {
                // enabled_any() TRUE is the exact negation of the early-out:
                // every sampled frame provably runs the full driver body —
                // the ungated ignite()+2 powf (driver-01), the disabled-glow
                // dark teardown (driver-02) and the :1144 GlowConfig copy
                // (driver-05) — while glow/trail/rain were never enabled.
                assert!(f.p.enabled_any() && f.p.sparkle_enabled());
                assert!(!f.p.matrix_rain_enabled());
            },
            bounds: DARK,
            lit_pct: (0, 0),
            trail_pin: None,
            witness: Witness::DarkUnless {
                what: "the same watch script over a grid of lexicon matches",
                build: f_sparkle_matches,
                warm: warm_watch,
                arm: arm_idle,
            },
        },
        Workload {
            name: "apply_idle",
            note: "wake enabled, engines settled, cadence cold (base pin)",
            build: f_wake,
            warm: warm_settle,
            arm: arm_idle,
            assert_cfg: |f| {
                assert!(f.p.enabled_any());
                assert!(!f.p.sparkle_enabled() && !f.p.matrix_rain_enabled());
            },
            bounds: DARK,
            lit_pct: (0, 0),
            // >= 45 half-lives of decay: intensity exactly 0.0, so ignite()
            // must publish the UNTOUCHED configured base — while the powf
            // pair still runs every frame (last is Some forever).
            trail_pin: Some(TRAIL_BASE),
            witness: Witness::DarkUnless {
                what: "the same fixture with the typing resumed",
                build: f_wake,
                warm: warm_settle,
                arm: arm_typing,
            },
        },
        Workload {
            name: "apply_typing",
            note: "held-key typing, lumen + comet live (hot pin)",
            build: f_wake,
            warm: warm_typing,
            arm: arm_typing,
            assert_cfg: |f| {
                assert!(f.p.enabled_any());
                assert!(!f.p.sparkle_enabled() && !f.p.matrix_rain_enabled());
            },
            bounds: [
                (69, 89),       // trail
                (3_970, 5_060), // glow_add
                (1, 3),         // glow_halo
                (0, 0),         // fire_patch
                (0, 0),         // glow_under
                (0, 0),         // char_fg
                (0, 0),         // fire_halo
                (0, 0),         // decos
                (0, 0),         // ink
                (0, 0),         // free_sprites
                (0, 0),         // nova_add
                (0, 0),         // rain_quads
                (0, 0),         // rain_add
            ],
            lit_pct: (100, 100),
            // Saturated cadence: intensity exactly 1.0 at every apply instant,
            // so the publish must be exactly heat_color(base, 1.0).
            trail_pin: Some(TRAIL_HOT),
            witness: Witness::Bounds,
        },
        Workload {
            name: "apply_typing_pack",
            note: "the same typing frame with the synthwave pack armed",
            build: f_wake_pack,
            warm: warm_typing,
            arm: arm_typing,
            assert_cfg: |f| {
                assert!(f.p.enabled_any());
                assert!(!f.p.sparkle_enabled() && !f.p.matrix_rain_enabled());
            },
            bounds: [
                (69, 89),       // trail
                (2_330, 2_970), // glow_add
                (1, 3),         // glow_halo
                (0, 0),         // fire_patch
                (0, 0),         // glow_under
                (0, 0),         // char_fg
                (0, 0),         // fire_halo
                (0, 0),         // decos
                (0, 0),         // ink
                (0, 0),         // free_sprites
                (0, 0),         // nova_add
                (0, 0),         // rain_quads
                (0, 0),         // rain_add
            ],
            lit_pct: (100, 100),
            trail_pin: Some(TRAIL_HOT),
            witness: Witness::Bounds,
        },
        Workload {
            name: "tick_all_on",
            note: "all four engines live: agent stream, rescan every frame, rain",
            build: f_all_on,
            warm: warm_stream,
            arm: arm_stream,
            assert_cfg: |f| {
                assert!(
                    f.p.enabled_any() && f.p.sparkle_enabled() && f.p.matrix_rain_enabled(),
                    "tick_all_on: a master switch is off"
                );
            },
            bounds: [
                (42, 54),       // trail
                (2_080, 2_650), // glow_add
                (1, 3),         // glow_halo
                (0, 0),         // fire_patch
                (0, 0),         // glow_under
                (0, 0),         // char_fg
                (0, 0),         // fire_halo
                // The legacy decoration stream stays EMPTY in the rainbow-ink
                // era: sparkle output rides `ink` (per-cell fg overrides) —
                // a non-empty `decos` here would mean a routing change.
                (0, 0),    // decos
                (84, 108), // ink
                // Peeking cats did not fire in this streaming window (the
                // occurrences are young and scroll away before an idle
                // one-shot rolls); a sprite appearing is a cadence change.
                (0, 0),     // free_sprites
                (0, 0),     // nova_add
                (155, 199), // rain_quads
                (7, 15),    // rain_add
            ],
            lit_pct: (100, 100),
            // The cadence is COLD here (last keystroke was the prompt, > 26 s
            // of stream ago) so ignite() must publish the untouched base —
            // the driver-01 zero pin under a cursor in full motion.
            trail_pin: Some(TRAIL_BASE),
            witness: Witness::Bounds,
        },
    ]
}

// ------------------------------------------------------------ volume group --

/// Record a COUNT as a criterion measurement: the reported "time" in
/// nanoseconds IS the item count (1 ns == 1 item), so counts land in
/// target/criterion, get baselines and print regressions exactly like the
/// timings. The spin loop and the `k % 4` jitter are NOT ceremony — see the
/// cursor_glow_tick bench this idiom is copied from (warm-up needs wall time
/// proportional to `iters`; a zero-variance sample NaNs criterion's PDF).
fn bench_count(g: &mut BenchmarkGroup<'_, WallTime>, id: &str, count: usize) {
    assert!(
        count > 0,
        "{id}: a zero count cannot be recorded as a duration — only streams \
         with a positive asserted lower bound are recorded"
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

/// Every count a workload's guards assert, handed to criterion: each claimed
/// stream's PEAK (the number its bound is written against) and the window
/// total across all channels (the most sensitive single count — it moves when
/// ANY frame's emission moves). Streams whose asserted lower bound is 0 are
/// skipped: they are the ones the workload proves empty or leaves unclaimed,
/// and their `(0, 0)`/ceiling bounds already police them.
fn bench_counts(g: &mut BenchmarkGroup<'_, WallTime>, w: &Workload, s: &Sampled) {
    for (i, (&peak, &(lo, _))) in s.peak.0.iter().zip(w.bounds.iter()).enumerate() {
        if lo > 0 {
            bench_count(g, &format!("{}/{}", w.name, STREAMS[i]), peak);
        }
    }
    if s.sum.total() > 0 {
        bench_count(g, &format!("{}/total", w.name), s.sum.total());
    }
}

// -------------------------------------------------------------- the groups --

fn effects_pipeline(c: &mut Criterion) {
    // PROVE FIRST, TIME SECOND: every workload is built, warmed and verified
    // before a single nanosecond is measured; the warmed fixture is carried
    // forward so the timed run continues from the verified state.
    let mut verified: Vec<(Workload, Fixture, Sampled)> = workloads()
        .into_iter()
        .map(|w| {
            let (f, s) = verify_reaches_target(&w);
            (w, f, s)
        })
        .collect();

    {
        let mut group = c.benchmark_group("effects_pipeline");
        // THE FLOOR UNDER EVERY NUMBER IN THIS GROUP: the Instant::now() pair
        // that timing `apply` alone puts inside each iteration, measured with
        // an empty span. Subtract for absolutes; it cancels in an A/B.
        group.bench_function("timer_floor", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let t0 = WallInstant::now();
                    black_box(0u64);
                    total += t0.elapsed();
                }
                total
            });
        });
        for (w, f, _) in verified.iter_mut() {
            let arm = w.arm;
            group.bench_function(BenchmarkId::from_parameter(w.name), |b| {
                // ONE presented frame per iteration, from the verified steady
                // state, the script still driving the pipeline so the state
                // under test is SUSTAINED across the whole measurement. The
                // arm runs outside the timed span; what is timed is `apply`.
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        arm(f);
                        let t0 = WallInstant::now();
                        black_box(f.apply());
                        total += t0.elapsed();
                    }
                    total
                });
            });
        }
        group.finish();
    }

    {
        // WHAT THE DRIVER NUMBERS EXCLUDE, priced under names that cannot be
        // mistaken for `apply`: the host half of one frame (PTY byte +
        // note_keystroke + advance + the 80x24 `cell_frame_into` refill) and
        // the idle variant (advance + refill only). Add one of these to an
        // `effects_pipeline` number, subtract `timer_floor`, and you have a
        // whole presented frame minus the renderer.
        let mut group = c.benchmark_group("effects_pipeline_host_seams");
        let mut typing = f_wake();
        group.bench_function("typing_arm_80x24", |b| {
            b.iter(|| arm_typing(black_box(&mut typing)));
        });
        let mut idle = f_wake();
        group.bench_function("idle_arm_80x24", |b| {
            b.iter(|| arm_idle(black_box(&mut idle)));
        });
        group.finish();
    }

    {
        // The counts, as measurements (sampled during verification, so this
        // group runs at criterion's floor rather than the timing budget).
        let mut group = c.benchmark_group("effects_pipeline_volume");
        group
            .warm_up_time(Duration::from_millis(1))
            .measurement_time(Duration::from_millis(10))
            .sample_size(10);
        for (w, _, s) in verified.iter() {
            bench_counts(&mut group, w, s);
        }
        group.finish();
    }
}

criterion_group!(benches, effects_pipeline);
criterion_main!(benches);
