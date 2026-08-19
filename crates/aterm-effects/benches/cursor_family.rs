// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The CURSOR TRAIL engine (`cursor_trail.rs`) — the one cursor body driven in
// production ONLY by bin-only aterm-gui, which is why no bench has ever ticked
// it. Three audited findings live here and this file is their instrument:
//
//   CF-2  `CursorTrail::tick`'s per-frame cell dedup is `out.iter_mut().find(..)`
//         per live spark — O(live_sparks × deduped_cells), quadratic when the
//         cells are distinct, up to MAX_SPARKS = 512 total live sparks. The
//         `dedup_scaling/{16,64,128,256,512}` sweep drives the population to
//         each N with DISTINCT cells and times the steady-state tick, so the
//         curve's shape (not just one point) convicts or acquits the fix.
//   CF-5  `line_cells` heap-allocates a fresh path Vec sized by the JUMP
//         distance on every `spawn`, inside `tick`. `typing_1cell`,
//         `jump_24cell` and `jump_full_width` all keep the spawn INSIDE the
//         timed span; `jump_full_width` (a 200-column move capped to
//         max_len = 24) is the arm where the allocation is 201 tuples and the
//         survivors 24 — the oversized `with_capacity` isolated.
//   CF-6  (in-crate half) the driver evaluates the cadence decay THREE times
//         for one `frame_started`: `typing_cadence.intensity()` at
//         app_render.rs:9984, then `intensity()` + `warmth()` again inside the
//         `ignite` call at 10189-10193 — three `powf` where one decayed heat
//         serves all three. The `typing_cadence` group prices exactly that
//         driver-shaped triple (`driver_triple_ignite`) against the one-decay
//         floor (`single_decay`), so a shared `sample()` fix has its A/B.
//
//         THE BOUNDARY, stated precisely: what this crate can price is the
//         PER-CALL cost of `intensity`/`warmth`/`ignite` and therefore the
//         cost of the triple itself. What it CANNOT price is the driver's
//         half of the finding — that `App::tick_cursor_fx` runs the triple
//         unconditionally on every presented frame BEFORE any `cfg.enabled`
//         is consulted (app_render.rs:13087 → 9937 → 9984/10189), i.e. even
//         with every cursor effect off. That call site is bin-only aterm-gui;
//         no bench in this crate can reach it, so "the disabled user still
//         pays three powf per frame" stays unpriceable here and is reported
//         as such. The early-out fix's win is still derivable: it is exactly
//         one `driver_triple_ignite` per frame, measured below.
//
// THE DRIVER'S CALL PATTERN, reconstructed from aterm-gui (read-only) and
// reproduced by the arms here so the engine is driven the way production
// drives it:
//
//   per keystroke (app_input.rs):   typing_cadence.on_keystroke(now)  (:1993,
//                                   nav/kill chords earn NO heat)
//                                   cursor_trail.note_typed(now)      (:2021,
//                                   :2080 — typed forward AND backspace)
//                                   cursor_trail.note_navigation(now) (:2205,
//                                   nav chords)
//   per frame (app_render.rs):      trail_cfg = self.trail_config()   (:9670 —
//                                   REBUILT fresh each frame; `ignite` heats
//                                   `color` in place, so reusing one config
//                                   would compound the heat)
//                                   cursor_trail::ignite(&mut trail_cfg,
//                                     cadence.intensity(frame_started),
//                                     cadence.warmth(frame_started))  (:10189)
//                                   cursor_trail.tick(cur, frame_started,
//                                     &trail_cfg, &mut ws.trail_scratch)
//                                                                     (:10196)
//                                   cursor_trail.note_context(alt)    (:12685)
//
// WHAT IS TIMED, EXACTLY (the cursor_glow_tick discipline): `tick` AND NOTHING
// ELSE. The arm — clock += dt, the keystroke seams, the per-frame config
// rebuild + `ignite` — runs OUTSIDE the timed span, so the trail numbers
// cannot be contaminated by the cadence triple (which the `typing_cadence`
// group prices under its own name) and an A/B on a CF-2/CF-5 fix reads pure.
// The price of that separation is one `Instant::now()` pair per iteration;
// `timer_floor` in each timing group measures exactly that offset with an
// empty span. Subtract it for an absolute cost; for an A/B it cancels.
//
// DETERMINISM. `CursorTrail` has no RNG; the injected clock is one wall sample
// plus a fixed integer dt per frame, and every age the engine reads is a
// difference of those. The `dedup_scaling` fixtures additionally bypass the
// cadence entirely (fixed `intensity: 1.0` config, no `powf` anywhere in the
// pipeline), so their emitted counts are BYTE-DETERMINISTIC and their guards
// are exact. The cadence-driven workloads (`typing_1cell`) sit one `powf` ulp
// away from integer math, so their bounds carry a small margin instead.
//
// THE POPULATION MATH the dedup sweep is built on. `spawn` lays exactly
// `J = 16` sparks per frame (a 16-column same-row jump: 17 path cells, the
// destination popped, `max_len = 16`), and `tick`'s retain keeps a spark for
// `ceil(life/dt)` frames, so the steady-state population is
//
//     live = J x gens,   gens = number of frame-ages strictly under `life`
//
// with `life = duration x (1 + 0.6 x intensity)` at intensity 1, warmth 0
// (IGNITED_LIFE_MULT = 1.6). dt = 16 ms throughout, so:
//
//     N =  16: duration  10 ms -> life   16 ms -> 1 gen
//     N =  64: duration  40 ms -> life   64 ms -> 4 gens
//     N = 128: duration  80 ms -> life  128 ms -> 8 gens
//     N = 256: duration 160 ms -> life  256 ms -> 16 gens
//     N = 512: duration 2000 ms -> life 3200 ms -> pinned AT the MAX_SPARKS
//              cap: spawn's own drain drops 16 oldest per frame, retain never
//              fires. This is a SATURATION arm (the AT_HALO_CAP idiom): the
//              guard proves the population sits at exactly 512 and cannot
//              witness growth past it — the four below-cap arms carry the real
//              two-sided count guards.
//
// Intensity is pinned at 1.0 in these fixtures for a second reason besides
// determinism: it keeps every live spark's resolved alpha above zero (the
// oldest generation's time-fade is 255/gens, and the dimmest born alpha is
// ~45), so the `alpha == 0 -> continue` skip never fires and EVERY live spark
// pays the dedup scan — the guard `out.len() == N` is then simultaneously the
// proof that N sparks reached the scan.
//
// THE SERPENTINE keeps the swept cells DISTINCT within any lifetime: the
// caret walks J columns per frame across a 4096-column row, then drops to the
// next of 64 rows — a 16384-frame cycle against lifetimes of at most ~200
// frames. Distinct cells are the load-bearing half of CF-2: overlapping cells
// keep `out` short and the scan linear (which is exactly what
// `jump_full_width`'s ping-pong demonstrates — 400+ live sparks stacked on
// ~48 cells, dedup cheap, allocation expensive). At each row wrap the single
// long unhinted move lays one capped comet whose cells collide with the next
// few frames' spawns, dipping `out` by at most ~17 below N for a few frames
// per 256 — the trough bounds absorb exactly that and no more.
//
// EMITTED VOLUME IS A MEASUREMENT (the cursor_glow_volume idiom): every lit
// workload's peak `out` length and window total are handed to criterion as
// counts-as-nanoseconds benchmarks in `cursor_family_volume`, so a count
// regression is stored and A/B'd by the same tooling that catches a time
// regression. Every volume guard below is TWO-SIDED except the declared
// saturation arm: the min proves the workload reached the state it claims (a
// `>= 0` bound passes on a dead engine — the broken-guard mistake this crate
// has made and caught twice), the max catches a count regression.
//
// WHAT WAS MEASURED WHEN THIS FILE LANDED (Apple Silicon, --profile bench),
// recorded so the reach claims are auditable. dedup_scaling 16/64/128/256/512
// ticked at 0.20/1.13/3.88/13.57/49.9 us: the PER-SPARK cost — t/N =
// 12.5/17.7/30.3/53.0/97.5 ns — itself grows ~8x across the 32x population,
// which is the quadratic signature CF-2 predicts (a linear engine holds t/N
// flat; every doubling here multiplies total time ~3.5x, not 2x). The dedup
// fix's acceptance bar is that curve flattening while every count guard
// still passes. typing_1cell 2.46 us at ~100 cells; jump_24cell 28.9 us at
// ~380 deduped cells over 408 live sparks (the flood frame a caret-walking
// TUI pays today); jump_full_width 4.55 us with `out` pinned at 48 cells
// under the same 408 sparks — the dedup-cheap arm whose remaining cost IS
// the spawn path (the 201-tuple alloc + drain, CF-5). off_disabled 15.3 ns
// and idle_settled 15.2 ns over a 12.5 ns timer floor: ~3 ns of engine, the
// early-outs intact. Cadence: driver_triple_ignite 25.7 ns vs single_decay
// 15.5 ns vs cold_untouched 13.9 ns over a 13.9 ns floor in an isolated
// run; a full-file run read 40.8/20.9/13.8 over 14.0 — the ABSOLUTE offsets
// ride cache pressure, but the triple sits 3-4x over one decay after floor
// subtraction in both, and that within-run gap (which the floor cancels out
// of) is the stable signal a shared `sample()` fix will close.

use std::time::Duration;
use std::time::Instant as WallInstant;

use aterm_effects::cursor_trail::{CursorTrail, TrailConfig, TypingCadence, ignite};
use aterm_render::TrailCell;
use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, black_box, criterion_group, criterion_main,
};

// ------------------------------------------------------------------ clocks --

/// 60 fps — the dt of every app-driven workload (jumps, the dedup sweep, the
/// idle arm). The presented-frame cadence `tick_cursor_fx` runs at.
const FRAME_DT: Duration = Duration::from_millis(16);

/// 8 ms per keystroke for the typing workload — fast human / key-repeat
/// cadence, the same TYPE_DT the cursor_glow bench types at. One keystroke ==
/// one frame, which pins the cadence at full ignition (heat capped at
/// knee_hi) so the typing arm exercises the IGNITED spawn path: heated
/// colour, 1.6x + warmth-chained lifetimes, the brightest alphas.
const TYPE_DT: Duration = Duration::from_millis(8);

/// The cadence group's frame dt, with a keystroke every [`KEY_EVERY`] frames:
/// 8 ms x 5 = a 40 ms inter-key gap — the sustained touch-typist cadence the
/// in-crate tests use to prove ignition (`fast_sustained_burst_ignites`).
const CAD_DT: Duration = Duration::from_millis(8);
const KEY_EVERY: u64 = 5;

// ------------------------------------------------------------------ script --

/// Frames of script before anything is sampled or timed. The longest-lived
/// state here is typing_1cell's ~806 ms spark life (~101 frames at 8 ms);
/// 600 frames is several full turnovers of every population.
const WARM_FRAMES: usize = 600;

/// Frames sampled by the verify pass — covers > 2 serpentine row-wraps for
/// every J, so the wrap-dip is inside every sampled window.
const SAMPLE_FRAMES: usize = 600;

/// The typing line: 190 columns, the house grid width. At the fold the typed
/// hint classifies the wrap as a re-anchor (dr = 1, raw_dist > 2) and lays no
/// comet — exactly what a hinted composer wrap does in production, and ~0.5 %
/// of frames.
const TYPING_COLS: u16 = 190;
const TYPING_ROWS: u16 = 64;

/// The serpentine plane for the jump/dedup arms. Cells revisit only after
/// SERP_COLS/J x SERP_ROWS frames (16384 at J = 16) — far beyond any spark
/// lifetime, so live cells are distinct by construction.
const SERP_COLS: u64 = 4096;
const SERP_ROWS: u64 = 64;

/// The dedup sweep's per-frame spawn: a 16-column jump under `max_len = 16`
/// lays exactly 16 sparks (17 Bresenham cells minus the popped destination).
const DEDUP_J: u16 = 16;

/// `jump_full_width`: a same-row ping-pong between these columns — a
/// 200-column move whose `line_cells` allocates 201 tuples and whose
/// `drain(0..177)` keeps 24. The two directions lay disjoint 24-cell runs
/// (cols ~181..205 rightward, ~6..29 leftward), so `out` stays ~48 cells
/// while ~408 sparks are live: the workload that prices the ALLOCATION with
/// the dedup deliberately kept cheap.
const PONG_LO: u16 = 5;
const PONG_HI: u16 = 205;

// ------------------------------------------------------------------ config --

/// `CursorTrail::MAX_SPARKS`, mirrored — the population cap the 512 sweep arm
/// is pinned at.
const SPARK_CAP: usize = 512;

/// The shipped-default config (the audit's CF-2 fixture: `cursor_trail_ms`
/// 260, `cursor_trail_length` 24, the comet default colour). `intensity` and
/// `warmth` start 0.0 — the arm stamps the live cadence via `ignite` each
/// frame, exactly as `trail_config()`'s doc demands of its caller.
fn shipped_cfg() -> TrailConfig {
    TrailConfig {
        enabled: true,
        duration: Duration::from_millis(260),
        max_len: 24,
        color: 0x0050_FA7B,
        intensity: 0.0,
        warmth: 0.0,
    }
}

/// A dedup-sweep config targeting a steady-state population (see the header
/// math): `duration` picks the generation count, `max_len = DEDUP_J` pins the
/// per-frame spawn, and the FIXED `intensity: 1.0` (never re-stamped —
/// `ignite` is deliberately not called on these fixtures) removes every
/// `powf` from the pipeline so the counts are byte-deterministic AND keeps
/// every live spark's alpha above the `continue` skip, so all N sparks pay
/// the dedup scan.
fn dedup_cfg(duration_ms: u64) -> TrailConfig {
    TrailConfig {
        enabled: true,
        duration: Duration::from_millis(duration_ms),
        max_len: DEDUP_J as usize,
        color: 0x0050_FA7B,
        intensity: 1.0,
        warmth: 0.0,
    }
}

// ----------------------------------------------------------------- fixture --

/// One workload's world: the engine, the cadence tracker (the driver keeps
/// one per window beside the trail), the per-frame-rebuilt config pair, the
/// INJECTED clock, and the script position. The `out` Vec is resident across
/// frames like the host's `trail_scratch` (`tick` clears it itself).
struct Fixture {
    trail: CursorTrail,
    cad: TypingCadence,
    /// What `trail_config()` returns — rebuilt (copied) fresh each frame
    /// before `ignite`, because `ignite` heats `color` IN PLACE and stamping
    /// one long-lived config would compound the heat frame over frame.
    base: TrailConfig,
    /// This frame's ignited config — what the timed `tick` reads.
    cfg: TrailConfig,
    now: web_time::Instant,
    dt: Duration,
    out: Vec<TrailCell>,
    row: u16,
    col: u16,
    n: u64,
}

impl Fixture {
    fn new(base: TrailConfig, dt: Duration) -> Self {
        Fixture {
            trail: CursorTrail::default(),
            cad: TypingCadence::default(),
            base,
            cfg: base,
            // ONE wall sample for the clock's origin; from here the clock is
            // advanced by a fixed dt and never read from the wall again.
            now: web_time::Instant::now(),
            dt,
            out: Vec::new(),
            row: 0,
            col: 0,
            n: 0,
        }
    }

    /// THE TIMED UNIT: the engine call and nothing else. `spawn` — and with
    /// it CF-5's `line_cells` allocation — happens INSIDE `tick`, so it is
    /// inside every timed span that moves the cursor.
    fn tick(&mut self) -> u64 {
        self.trail.tick(
            black_box(Some((self.row, self.col))),
            self.now,
            black_box(&self.cfg),
            &mut self.out,
        )
    }
}

// --------------------------------------------------------------------- arm --

/// The HOST half of one frame, run OUTSIDE the timed span: the clock, the
/// keystroke seams, the config rebuild + `ignite`. Everything aterm-gui does
/// around the tick, in its order.
type Arm = fn(&mut Fixture);

/// The driver's per-frame cadence stamp (app_render.rs:10189-10193): rebuild
/// the config, read `intensity` + `warmth` at the SAME injected now, ignite.
/// (The driver's THIRD read — rainbow energy at :9984 — is priced by the
/// `typing_cadence` group, not folded in here, so the trail numbers stay
/// pure.)
fn stamp(f: &mut Fixture) {
    let mut c = f.base;
    ignite(&mut c, f.cad.intensity(f.now), f.cad.warmth(f.now));
    f.cfg = c;
}

/// Typing: one keystroke == one frame at TYPE_DT. Keystroke seams exactly as
/// app_input.rs arms them (cadence heat :1993, trail typed hint :2080), then
/// the per-frame context stamp (:12685) and the ignite stamp. The caret
/// advances one column, folding at the margin (the fold is a hinted
/// re-anchor — no comet, hint consumed — matching a composer wrap).
fn arm_typing(f: &mut Fixture) {
    f.n += 1;
    f.now += f.dt;
    f.cad.on_keystroke(f.now);
    f.trail.note_typed(f.now);
    f.trail.note_context(false);
    if f.col + 1 >= TYPING_COLS {
        f.col = 0;
        f.row = (f.row + 1) % TYPING_ROWS;
    } else {
        f.col += 1;
    }
    stamp(f);
}

/// An app-driven caret walk: J columns per frame along the serpentine, NO
/// hints armed — the audit's flood shape (a TUI repositioning its caret every
/// frame; only nav-hinted and typed-re-anchor moves are suppressed, and this
/// is neither). J is recovered from `base.max_len`, which every serpentine
/// fixture sets to its jump length — for `jump_24cell` that is the SHIPPED
/// default 24, so every frame allocates 25 tuples and keeps 24 (the
/// realistic heavy-TUI caret walk the CF-2 mechanism cites: 24 x 17 gens =
/// 408 live sparks, under the cap).
fn arm_serp(f: &mut Fixture) {
    f.n += 1;
    f.now += f.dt;
    f.trail.note_context(false);
    let j = f.base.max_len as u64;
    let steps_per_row = SERP_COLS / j;
    f.col = ((f.n % steps_per_row) * j) as u16;
    f.row = ((f.n / steps_per_row) % SERP_ROWS) as u16;
    stamp(f);
}

/// The full-width ping-pong: a 200-column unhinted move every frame.
fn arm_pingpong(f: &mut Fixture) {
    f.n += 1;
    f.now += f.dt;
    f.trail.note_context(false);
    f.col = if f.n.is_multiple_of(2) {
        PONG_LO
    } else {
        PONG_HI
    };
    f.row = 4;
    stamp(f);
}

/// The dedup sweep's arm: `arm_serp`'s walk with the FIXED config — no
/// `ignite`, no cadence, no `powf`; byte-deterministic counts (see
/// `dedup_cfg`).
fn arm_dedup(f: &mut Fixture) {
    f.n += 1;
    f.now += f.dt;
    f.trail.note_context(false);
    let j = DEDUP_J as u64;
    let steps_per_row = SERP_COLS / j;
    f.col = ((f.n % steps_per_row) * j) as u16;
    f.row = ((f.n / steps_per_row) % SERP_ROWS) as u16;
    f.cfg = f.base;
}

/// The cursor never moves: `tick` sees `last == cur`, spawns nothing, and
/// once the warm-up's residue decays the engine is empty — the "no live
/// sparks -> no animation timer" idle contract, priced.
fn arm_idle(f: &mut Fixture) {
    f.n += 1;
    f.now += f.dt;
    f.trail.note_context(false);
    stamp(f);
}

// ------------------------------------------------------------- observation --

/// What running a script for a window showed: the `out` population's peak,
/// trough and window total, the lit-frame count (fp != 0), and the cadence
/// intensity's range (the GATE scalar for the ignited typing arm — a spine
/// this hot is what makes the spawn path the IGNITED one).
struct Sampled {
    peak: usize,
    trough: usize,
    total: usize,
    lit: usize,
    frames: usize,
    cad: (f32, f32),
}

fn run(f: &mut Fixture, arm: Arm, frames: usize) -> Sampled {
    let mut s = Sampled {
        peak: 0,
        trough: usize::MAX,
        total: 0,
        lit: 0,
        frames,
        cad: (f32::MAX, f32::MIN),
    };
    for _ in 0..frames {
        arm(f);
        let fp = f.tick();
        s.peak = s.peak.max(f.out.len());
        s.trough = s.trough.min(f.out.len());
        s.total += f.out.len();
        s.lit += usize::from(fp != 0);
        let i = f.cad.intensity(f.now);
        s.cad.0 = s.cad.0.min(i);
        s.cad.1 = s.cad.1.max(i);
    }
    s
}

fn warm(f: &mut Fixture, arm: Arm) {
    for _ in 0..WARM_FRAMES {
        arm(f);
        f.tick();
    }
}

// ------------------------------------------------------------------ guards --

/// The extra, decisive proof beyond the volume bounds.
enum Witness {
    /// The two-sided bounds are themselves decisive.
    Bounds,
    /// This workload must be completely DARK while a CONTROL — the identical
    /// script through a fixture missing only the off switch (or only the
    /// stillness) — lights up. What makes an "off" cost a measurement instead
    /// of a tautology.
    DarkUnless {
        what: &'static str,
        control: fn() -> Fixture,
        arm: Arm,
    },
}

struct Workload {
    name: &'static str,
    note: &'static str,
    build: fn() -> Fixture,
    arm: Arm,
    /// Two-sided bound on the PEAK `out` length over the window. The min side
    /// proves the population reached the state the workload claims; the max
    /// catches a count regression. `(N, N)` on the deterministic dedup arms.
    peak: (usize, usize),
    /// The window MINIMUM must not fall below this — the population is
    /// SUSTAINED, not a transient the timed run would decay out of. Sits at
    /// most one wrap-dip (~17 cells) under the peak by construction.
    trough: usize,
    /// Bounds on the percentage of sampled frames with fp != 0.
    lit_pct: (usize, usize),
    /// `(min >= .0, max <= .1)` on the cadence intensity over the window —
    /// the ignition GATE, pinned from outside. `(0.0, 0.0)` on the unignited
    /// app-walk arms proves their spawns took the GENTLE path; `(0.85, 1.0)`
    /// on typing proves the IGNITED one.
    cad: (f32, f32),
    witness: Witness,
}

fn report(name: &str, note: &str, s: &Sampled) {
    println!(
        "VOLUME {name:<22} | out peak {} trough {} | per-frame avg {} | \
         window total {} | intensity {:.2}-{:.2} | lit {}/{} | {note}",
        s.peak,
        if s.trough == usize::MAX { 0 } else { s.trough },
        s.total / s.frames,
        s.total,
        s.cad.0,
        s.cad.1,
        s.lit,
        s.frames
    );
}

/// Build the workload, run it to steady state, and PROVE it is in the state
/// it claims before a single nanosecond is timed. Returns the warmed fixture
/// (the timed run continues from the verified state) and what was observed.
fn verify_reaches_target(w: &Workload) -> (Fixture, Sampled) {
    let mut f = (w.build)();
    warm(&mut f, w.arm);
    let s = run(&mut f, w.arm, SAMPLE_FRAMES);
    report(w.name, w.note, &s);

    assert!(
        s.peak >= w.peak.0 && s.peak <= w.peak.1,
        "{}: peak out = {}, outside [{}, {}] — the workload is not in the \
         state it claims (a COUNT regression, a lost spawn, or a script that \
         stopped reaching its target)",
        w.name,
        s.peak,
        w.peak.0,
        w.peak.1
    );
    assert!(
        s.trough >= w.trough,
        "{}: window-min out = {} under the required {} — the population is \
         not SUSTAINED and the timed run would decay out of the verified state",
        w.name,
        s.trough,
        w.trough
    );
    let lit = s.lit * 100 / s.frames;
    assert!(
        lit >= w.lit_pct.0 && lit <= w.lit_pct.1,
        "{}: {lit}% of frames emitted (fp != 0), outside [{}, {}]%",
        w.name,
        w.lit_pct.0,
        w.lit_pct.1
    );
    assert!(
        s.cad.0 >= w.cad.0 && s.cad.1 <= w.cad.1,
        "{}: cadence intensity ranged [{:.3}, {:.3}], outside [{:.3}, {:.3}] \
         — the spawn path is not in the ignition state this workload claims",
        w.name,
        s.cad.0,
        s.cad.1,
        w.cad.0,
        w.cad.1
    );

    if let Witness::DarkUnless { what, control, arm } = w.witness {
        assert!(
            s.total == 0 && s.lit == 0,
            "{}: emitted light on a DARK workload (total {}, lit {})",
            w.name,
            s.total,
            s.lit
        );
        let mut c = (control)();
        warm(&mut c, arm);
        let cs = run(&mut c, arm, SAMPLE_FRAMES);
        report(&format!("{}.control", w.name), what, &cs);
        assert!(
            cs.total > 0 && cs.lit > 0,
            "{}: the CONTROL ({what}) drew nothing either, so this workload's \
             zero proves nothing — it would be measuring a script that had no \
             light to suppress",
            w.name
        );
    }
    (f, s)
}

// --------------------------------------------------------------- fixtures ---

fn f_typing() -> Fixture {
    Fixture::new(shipped_cfg(), TYPE_DT)
}

fn f_disabled() -> Fixture {
    let mut base = shipped_cfg();
    base.enabled = false;
    Fixture::new(base, TYPE_DT)
}

fn f_idle() -> Fixture {
    Fixture::new(shipped_cfg(), FRAME_DT)
}

fn f_jump24() -> Fixture {
    Fixture::new(shipped_cfg(), FRAME_DT)
}

fn f_full_width() -> Fixture {
    Fixture::new(shipped_cfg(), FRAME_DT)
}

fn f_dedup_16() -> Fixture {
    Fixture::new(dedup_cfg(10), FRAME_DT)
}

fn f_dedup_64() -> Fixture {
    Fixture::new(dedup_cfg(40), FRAME_DT)
}

fn f_dedup_128() -> Fixture {
    Fixture::new(dedup_cfg(80), FRAME_DT)
}

fn f_dedup_256() -> Fixture {
    Fixture::new(dedup_cfg(160), FRAME_DT)
}

fn f_dedup_512() -> Fixture {
    Fixture::new(dedup_cfg(2000), FRAME_DT)
}

// --------------------------------------------------------------- workloads --

/// "Not what this workload is about" — intensity is clamped to 0..=1, so this
/// pair can never fail.
const ANY_CAD: (f32, f32) = (0.0, 1.0);

fn workloads() -> Vec<Workload> {
    vec![
        // ---- the OFF/IDLE floor every other number is read against --------
        Workload {
            name: "off_disabled",
            note: "enabled = false, full typing script — the :305 early-out",
            build: f_disabled,
            arm: arm_typing,
            peak: (0, 0),
            trough: 0,
            lit_pct: (0, 0),
            // The cadence itself still ignites (the typing script heats it);
            // only the ENGINE must stay dark — which is exactly the CF-6
            // point: the cadence work is not gated by the trail's switch.
            cad: ANY_CAD,
            witness: Witness::DarkUnless {
                what: "the same typing script with enabled = true",
                control: f_typing,
                arm: arm_typing,
            },
        },
        Workload {
            name: "idle_settled",
            note: "enabled, cursor still, population decayed to empty",
            build: f_idle,
            arm: arm_idle,
            peak: (0, 0),
            trough: 0,
            lit_pct: (0, 0),
            cad: (0.0, 0.0),
            witness: Witness::DarkUnless {
                what: "the same engine with the caret walking",
                control: f_jump24,
                arm: arm_serp,
            },
        },
        // ---- CF-5: the spawn allocation, three real move shapes -----------
        Workload {
            name: "typing_1cell",
            note: "ignited typing: 1-cell spawn + hint + chained lifetimes",
            build: f_typing,
            arm: arm_typing,
            // ~101 generations of one spark (life 260 x (1 + 1.5 + 0.6) ~=
            // 806 ms at 8 ms/frame), minus the oldest spark whose alpha
            // rounds to 0 (measured: peak 100, trough 99, steady). The
            // margin covers the cadence powf ulp — the one float feeding
            // these counts.
            peak: (97, 103),
            trough: 95,
            lit_pct: (100, 100),
            // The GATE: a spine this hot is what routes `spawn` through the
            // ignited alphas, the heated colour and the warmth-chained life.
            cad: (0.85, 1.0),
            witness: Witness::Bounds,
        },
        Workload {
            name: "jump_24cell",
            note: "unhinted 24-cell caret walk: 25-tuple alloc per frame",
            build: f_jump24,
            arm: arm_serp,
            // 408 live sparks (24 x 17 gens at the shipped 260 ms), of which
            // the oldest generations' dimmest cells round to alpha 0 and
            // drop out of `out` — measured steady state: peak 383, trough
            // 359 (the trough is the wrap-dip, one full 24-cell generation
            // colliding). Deterministic: the cadence is cold (`last: None`),
            // so intensity is exactly 0.0 and no powf feeds these counts.
            peak: (375, 391),
            trough: 350,
            lit_pct: (100, 100),
            cad: (0.0, 0.0),
            witness: Witness::Bounds,
        },
        Workload {
            name: "jump_full_width",
            note: "200-col ping-pong: 201-tuple alloc, 24 survivors, out ~48",
            build: f_full_width,
            arm: arm_pingpong,
            // The two directions lay two disjoint ~24-cell runs; ~408 live
            // sparks STACK on those cells, so `out` stays under ~50 and the
            // dedup is deliberately cheap — what this arm prices is the
            // oversized `with_capacity` + `drain`, isolated.
            peak: (40, 50),
            trough: 34,
            lit_pct: (100, 100),
            cad: (0.0, 0.0),
            witness: Witness::Bounds,
        },
        // ---- CF-2: the dedup sweep (see the header math) -------------------
        Workload {
            name: "dedup_scaling/16",
            note: "16 live sparks, distinct cells (1 gen x 16/frame)",
            build: f_dedup_16,
            arm: arm_dedup,
            peak: (16, 16),
            trough: 16,
            lit_pct: (100, 100),
            cad: (0.0, 0.0),
            witness: Witness::Bounds,
        },
        Workload {
            name: "dedup_scaling/64",
            note: "64 live sparks, distinct cells (4 gens x 16/frame)",
            build: f_dedup_64,
            arm: arm_dedup,
            peak: (64, 64),
            trough: 47,
            lit_pct: (100, 100),
            cad: (0.0, 0.0),
            witness: Witness::Bounds,
        },
        Workload {
            name: "dedup_scaling/128",
            note: "128 live sparks, distinct cells (8 gens x 16/frame)",
            build: f_dedup_128,
            arm: arm_dedup,
            peak: (128, 128),
            trough: 111,
            lit_pct: (100, 100),
            cad: (0.0, 0.0),
            witness: Witness::Bounds,
        },
        Workload {
            name: "dedup_scaling/256",
            note: "256 live sparks, distinct cells (16 gens x 16/frame)",
            build: f_dedup_256,
            arm: arm_dedup,
            peak: (256, 256),
            trough: 239,
            lit_pct: (100, 100),
            cad: (0.0, 0.0),
            witness: Witness::Bounds,
        },
        Workload {
            name: "dedup_scaling/512",
            note: "AT the MAX_SPARKS cap: spawn's drain pins the population",
            build: f_dedup_512,
            arm: arm_dedup,
            // SATURATION arm (the AT_HALO_CAP idiom): live only in the "fell
            // off the cap" direction — a population regression PAST 512
            // cannot be witnessed here, which is why the four below-cap arms
            // above carry the real two-sided growth guards.
            peak: (SPARK_CAP, SPARK_CAP),
            trough: SPARK_CAP - 17,
            lit_pct: (100, 100),
            cad: (0.0, 0.0),
            witness: Witness::Bounds,
        },
    ]
}

// ------------------------------------------------------------ volume group --

/// Record a COUNT as a criterion measurement — 1 ns == 1 emitted item — so a
/// count regression is stored, baselined and A/B'd by exactly the tooling
/// that catches a time regression (the cursor_glow_volume idiom, carried over
/// verbatim). The spin loop keeps criterion's wall-clock warm-up from
/// doubling `iters` forever on an instant-return routine; the `k % 4`
/// nanoseconds keep the sample distribution non-degenerate (a zero-variance
/// PDF divides by a zero bandwidth: NaN, assert).
fn bench_count(g: &mut BenchmarkGroup<'_, WallTime>, id: &str, count: usize) {
    assert!(
        count > 0,
        "{id}: a zero count cannot be recorded as a duration — only lit \
         workloads reach this"
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

// ----------------------------------------------------------- cadence group --

/// The cadence tracker's world: the tracker, the injected clock, the frame
/// counter the keystroke cadence keys off, and the base config the ignite
/// stamp copies (as `trail_config()` rebuilds it).
struct CadFixture {
    cad: TypingCadence,
    base: TrailConfig,
    now: web_time::Instant,
    n: u64,
}

impl CadFixture {
    fn new() -> Self {
        CadFixture {
            cad: TypingCadence::default(),
            base: shipped_cfg(),
            now: web_time::Instant::now(),
            n: 0,
        }
    }
}

/// The hot arm: 8 ms frames, a keystroke every 5th (a 40 ms sustained
/// touch-typist — the cadence the in-crate ignition tests pin). Keystrokes
/// land OUTSIDE the timed span, so the timed numbers are pure reads.
fn cad_arm_hot(f: &mut CadFixture) {
    f.n += 1;
    f.now += CAD_DT;
    if f.n.is_multiple_of(KEY_EVERY) {
        f.cad.on_keystroke(f.now);
    }
}

/// The cold arm: the clock runs, no key is ever struck. `last` stays `None`,
/// so `intensity` takes the no-decay branch — the cheapest read the tracker
/// has, and the idle floor the hot numbers are read against.
fn cad_arm_cold(f: &mut CadFixture) {
    f.n += 1;
    f.now += CAD_DT;
}

fn typing_cadence_group(c: &mut Criterion) {
    // PROVE FIRST: the hot tracker must actually be ignited over a sustained
    // window (two-sided — an idle tracker reads 0.0 and cannot pass the
    // floor), and the cold tracker must read exactly 0.0 throughout.
    let mut hot = CadFixture::new();
    for _ in 0..WARM_FRAMES {
        cad_arm_hot(&mut hot);
    }
    let (mut i_min, mut i_max) = (f32::MAX, f32::MIN);
    let (mut w_min, mut w_max) = (f32::MAX, f32::MIN);
    for _ in 0..SAMPLE_FRAMES {
        cad_arm_hot(&mut hot);
        let i = hot.cad.intensity(hot.now);
        let w = hot.cad.warmth(hot.now);
        i_min = i_min.min(i);
        i_max = i_max.max(i);
        w_min = w_min.min(w);
        w_max = w_max.max(w);
    }
    println!(
        "VOLUME cadence_hot            | intensity {i_min:.3}-{i_max:.3} | \
         warmth {w_min:.3}-{w_max:.3} | 40 ms sustained typing"
    );
    assert!(
        i_min >= 0.85 && i_max <= 1.0,
        "cadence_hot: intensity [{i_min:.3}, {i_max:.3}] outside [0.85, 1.0] \
         — the tracker is not ignited and the timed reads would price a cold \
         decay"
    );
    assert!(
        w_min >= 0.99 && w_max <= 1.0,
        "cadence_hot: warmth [{w_min:.3}, {w_max:.3}] not pinned at 1.0"
    );
    let mut cold = CadFixture::new();
    for _ in 0..WARM_FRAMES {
        cad_arm_cold(&mut cold);
        assert_eq!(
            cold.cad.intensity(cold.now),
            0.0,
            "cadence_cold: an untouched tracker must read exactly 0.0"
        );
    }

    let mut group = c.benchmark_group("typing_cadence");
    // The floor under this group's numbers — these routines are tens of
    // nanoseconds, so the empty-span `Instant::now()` pair is a visible
    // additive offset. Subtract for absolutes; it cancels in the A/B a
    // `sample()` fix will run.
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
    // THE DRIVER'S TRIPLE, verbatim: `intensity(frame_started)` for the
    // rainbow energy (app_render.rs:9984), then the config rebuild +
    // `ignite(intensity(frame_started), warmth(frame_started))` (:10189-93).
    // Three decays of the same heat at the same instant — two `powf`-bearing
    // `intensity` calls, one `warmth` — plus `heat_color`. This is the
    // per-frame cadence cost the driver pays UNCONDITIONALLY (before any
    // cfg.enabled check), and the number a shared `sample()` must beat.
    //
    // THE `black_box(&f.cad)` BETWEEN CALLS IS LOAD-BEARING. `intensity` and
    // `warmth` are pure, so in a tight loop LLVM CSE's the duplicate calls
    // into ONE decay — measured, the un-boxed form of this triple timed
    // FASTER than one call (19.6 vs 22.3 ns), i.e. the bench had optimized
    // away the very defect it prices. The driver's duplicate calls are
    // separated by hundreds of lines of `&mut ws` engine ticks LLVM cannot
    // see through, so the opacity is the FAITHFUL modelling, not a trick:
    // each `black_box` stands in for the opaque code between the real call
    // sites and forces each read to redo its decay, exactly as shipped.
    group.bench_function("driver_triple_ignite", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                cad_arm_hot(&mut hot);
                let t0 = WallInstant::now();
                let energy = black_box(&hot.cad).intensity(hot.now); // :9984
                let mut cfg = hot.base; // trail_config(), rebuilt (:9670)
                let i = black_box(&hot.cad).intensity(hot.now); // :10191
                let w = black_box(&hot.cad).warmth(hot.now); // :10192
                ignite(&mut cfg, i, w); // :10189
                black_box((energy, cfg.intensity, cfg.warmth, cfg.color));
                total += t0.elapsed();
            }
            total
        });
    });
    // ONE decay: the floor a `TypingCadence::sample(now) -> (f32, f32)` fix
    // approaches (sample adds warmth's knee arithmetic — subtract, divide,
    // clamp — but only ONE `powf`). triple - floor vs single - floor is the
    // honest 3-vs-1 ratio. Same opacity as the triple, so the two arms
    // differ by exactly the extra decays.
    group.bench_function("single_decay", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                cad_arm_hot(&mut hot);
                let t0 = WallInstant::now();
                black_box(black_box(&hot.cad).intensity(hot.now));
                total += t0.elapsed();
            }
            total
        });
    });
    // The untouched tracker: `last == None`, no decay at all — what a
    // freshly-opened window pays until its first keystroke.
    group.bench_function("cold_untouched", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                cad_arm_cold(&mut cold);
                let t0 = WallInstant::now();
                black_box(black_box(&cold.cad).intensity(cold.now));
                total += t0.elapsed();
            }
            total
        });
    });
    group.finish();
}

// -------------------------------------------------------------- the groups --

fn cursor_family(c: &mut Criterion) {
    // PROVE FIRST, TIME SECOND: every workload is built, warmed and verified
    // before a nanosecond is measured; the warmed fixture is carried into the
    // timed run so the state under test continues, and the verified counts
    // become the volume group's measurements.
    let mut verified: Vec<(Workload, Fixture, Sampled)> = workloads()
        .into_iter()
        .map(|w| {
            let (f, s) = verify_reaches_target(&w);
            (w, f, s)
        })
        .collect();

    {
        let mut group = c.benchmark_group("cursor_trail_tick");
        // The empty-span `Instant::now()` pair inside every number in this
        // group. Subtract it for an absolute; it cancels in an A/B.
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
                // ONE presented frame per iteration, continuing from the
                // verified steady state: the arm (clock, keystroke seams,
                // config rebuild + ignite) outside the span, `tick` — with
                // its spawn, its retain, its dedup — alone inside it.
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        arm(f);
                        let t0 = WallInstant::now();
                        black_box(f.tick());
                        total += t0.elapsed();
                    }
                    total
                });
            });
        }
        group.finish();
    }

    {
        // The verified counts as measurements (1 ns == 1 cell). Dark
        // workloads are proven empty by their guards and skipped here.
        let mut group = c.benchmark_group("cursor_family_volume");
        group
            .warm_up_time(Duration::from_millis(1))
            .measurement_time(Duration::from_millis(10))
            .sample_size(10);
        for (w, _, s) in verified.iter() {
            if s.peak > 0 {
                bench_count(&mut group, &format!("{}/out_peak", w.name), s.peak);
            }
            if s.total > 0 {
                bench_count(&mut group, &format!("{}/total", w.name), s.total);
            }
        }
        group.finish();
    }

    typing_cadence_group(c);
}

criterion_group!(benches, cursor_family);
criterion_main!(benches);
