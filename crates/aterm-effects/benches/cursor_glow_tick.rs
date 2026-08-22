// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// `CursorGlow::tick` — the per-presented-frame cost of the crate's largest
// engine (35k lines, ~17 style-gated emitters, six output streams). Until this
// file the crate had NO bench target at all: the only instruments were
// `#[ignore]`d perf gates in `tests/`, and the biggest one of those
// (`tests/cursor_bench.rs::saturated*`) measures a state the shipping engine
// never reaches — see "WHAT THE OLD FIXTURES MISS" below.
//
// WHAT IS TIMED, EXACTLY. `tick` AND NOTHING ELSE. One iteration of a workload
// in the `cursor_glow_tick` group is:
//
//     arm(&mut f);                 // UNTIMED: clock += dt, note_typed/note_kill,
//                                  //          type_one, observe_row +
//                                  //          observe_neighbor_rows, note_context
//     let t0 = Instant::now();     //  ── timed span opens
//     black_box(f.tick());         //  the engine, and only the engine
//     total += t0.elapsed();       //  ── timed span closes
//
// The host seams are ARMED between the timed spans, so the script still drives
// the engine frame by frame (the state under test is sustained for the whole
// measurement) but the host's own cost is not folded into the engine's number.
// That cost is NOT free and NOT small — `probe()` re-copies a 190-char row and
// its two neighbours every frame — so it is priced on its own, under names that
// cannot be mistaken for `tick`: the `cursor_glow_host_seams` group.
//
// HOW MUCH THAT MATTERED, measured: `off_disabled` reports 21.6 ns over a
// 12.2 ns timer floor — ~9 ns of engine — while `typing_arm_190col`, the HOST
// half of that very same frame, is 51 ns. Timed as one unit, as it was, that
// workload reported 61.6 ns and five sixths of the number was the host, not the
// early return it was named for.
//
// THE PRICE OF THAT SEPARATION is one `Instant::now()` pair per iteration, which
// lands INSIDE every reported number in this group as a constant additive
// offset. `cursor_glow_tick/timer_floor` measures exactly that offset with an
// empty timed span, so an absolute cost is `reported - timer_floor`; for an A/B
// the offset cancels. It matters only for the `off_*` workloads, whose whole
// point is that they are cheap.
//
// WHAT `tick` COSTS. Cost scales with the LIVE LIGHT population, not with the
// grid: `sparks` (capped at MAX_SPARKS = 512, or RAINBOW_MAX_CELLS = 240 for
// rainbow kitty), `particles`, `ink_pops` (FRESH_INK_CAP = 32), and the emitted
// quad volume (MAX_QUADS = 16_384 per stream). So a workload is defined by the
// STATE it drives the engine into, and every state below is built by driving
// the public host seams — a cursor move, an injected clock, a typed hint, a row
// probe — never by reaching into private fields (there are none to reach).
//
// WHAT THE OLD FIXTURES MISS, and what this file therefore does differently:
//
//   * A FROZEN CLOCK. `tests/cursor_bench.rs::saturated` re-ticks ONE `Instant`,
//     so `dt == 0`, the whole lazy-decay block is skipped, `rainbow.disp` stays
//     0 and only the COLD 1-strip ribbon is measured. Every workload here
//     advances the injected clock by a FIXED dt per frame (`Fixture::dt`), so
//     the ~10 `exp` integrators, the momentum spine and the phase all run. The
//     measurement clock (criterion's) stays entirely separate from the injected
//     one, which is never sampled from the wall.
//
//   * `beam: false`. The shipped fixture hardcodes it, so `emit_comet` — and
//     with it the whole shared `aterm_render::layered_beam_quads` path that
//     Comet/Lumen/Laser/Beam/Phaser/Fire funnel through — is NEVER entered.
//     `beam_tube_jump` sets it and PROVES the entry with a `beam: false`
//     control on the identical script: measured, the control emits a THIRTEENTH
//     of this workload's items over the window (a fortieth of its `out` stream;
//     the guard demands at least a 10x separation on the window total).
//
//   * NO TYPED HINT. `note_typed` is the only thing that births
//     `rainbow.ink_pops`, and `emit_fresh_ink`, `emit_fresh_ink_glyphs` and
//     `emit_rainbow_wake` all return immediately on an empty pop ring. Every
//     typing workload here arms it per keystroke, and the rainbow workload
//     proves it matters with an unhinted control.
//
//   * NO ROW PROBE. Without `observe_row`/`observe_neighbor_rows`,
//     `probed_cell_glyph` answers `None` and every text-first star/ember/crown
//     gate takes the cheap fail-open branch instead of the real GUI branch.
//     Every workload here feeds the probe every frame, exactly as the host does.
//
// THE "OFF" NUMBERS ARE THE POINT. Most users have most effects off most of the
// time, so the four `off_*` workloads price what a frame costs when there is
// nothing to draw: the master-off early return, the zero-amplitude (unfocus)
// return, and an ENABLED engine with an empty light population — the ~17
// emitter early-outs plus the six fingerprint folds over empty vecs. Each is
// paired with a CONTROL: the identical script through an engine missing only
// the off switch, which must light up. An "off" cost measured on a script that
// would have drawn nothing anyway is not a measurement.
//
// EMITTED VOLUME IS A MEASUREMENT, NOT A PRINTOUT. `verify_reaches_target`
// samples all six streams (`out`, `under_quads`, `halos`, `patches`, `charred`,
// `halo_cells`) over the verification window, asserts a bound on every peak, and
// then hands those counts to criterion as their OWN benchmarks — the
// `cursor_glow_volume` group, where the reported "time" in nanoseconds IS the
// item count (1 ns == 1 item). So a regression in quad COUNT is caught, stored
// and A/B'd by exactly the tooling that catches a regression in per-quad COST,
// instead of scrolling past in stdout. (The human-readable `VOLUME …` lines are
// still printed; they carry the per-frame averages and the state scalars.)
//
// WHAT A VOLUME BOUND CAN AND CANNOT CATCH. Every bound is two-sided: the min
// proves the workload reached the emitter it claims (`>= 0` passes on a dead
// engine), the max catches a count regression a timing number alone would hide.
// TWO STREAMS ARE THE EXCEPTION, and they say so at their declaration:
//
//   * `halos` is CAPPED at MAX_HALOS = 512 and the truncation happens inside
//     `tick`, so a saturated halo stream reads 512 whatever was pushed. The six
//     workloads that saturate it (`rainbow_typing_{retina,1x,light}`,
//     `rainbow_jump_bursts`, `beam_tube_jump`, `style_crossfade`) therefore
//     assert `AT_HALO_CAP` — an explicit SATURATION guard, live only in the
//     "fell off the cap" direction. The two-sided halo guards live on the
//     workloads that stay under it: fire (423), laser (144), erase (100),
//     water and the custom pack (3 each, the crown alone).
//   * `out` is capped at MAX_QUADS = 16_384 the same way, and exactly one
//     workload (`water_wake_saturated`) is deliberately pinned there — see its
//     note. Every other workload's `out` is inside its budget.
//
// WHAT EACH WORKLOAD WAS CONFIRMED TO REACH. The guards below prove the STATE
// from outside; the function names were confirmed once, out of band, by
// sampling the bench binary built with `--profile profiling` (release codegen,
// symbols kept) and mapping the inline call-site lines back to source. Share of
// samples inside the timed loop, per workload:
//
//   rainbow_typing_retina   emit_rainbow 36 %, emit_particles 18 %,
//                           emit_rainbow_wake 3 %, rainbow_momentum_bands 2.6 %
//                           (the six-HSV-round-trip site itself), emit_rainbow_jumps
//   rainbow_typing_light    emit_particles 33 %, push_rainbow_streak_over 14 %,
//                           emit_rainbow 13 %, push_halo_over 8 %,
//                           emit_fresh_ink 2 %, light_ink_bold 1.8 %
//   water_wake_saturated    comet_beam 44 %, emit_water 26 %, emit_aa_slab 11 %
//   fire_blaze_sustained    comet_beam 29 %, emit_comet 16 %, layered_beam_quads
//                           15 %, emit_particles 11 %, emit_vapor 4 %, emit_flames 3 %
//   beam_tube_jump          comet_beam 34 %, emit_comet 18 %, layered_beam_quads 18 %
//   laser_bolt_jump         comet_beam 44 %, emit_bolts 12 %, emit_comet 9 %
//   rainbow_jump_bursts     comet_beam 34 %, emit_particles 19 %,
//                           emit_rainbow_jumps 10 %, emit_rainbow_starburst 1 %
//   erase_poof_run          emit_particles 56 %, push_halo 10 %, poof_scan 2 %,
//                           spawn_erase_sparkles 0.4 %
//   style_crossfade         tick_fades 38 % (the ghost's whole nested tick),
//                           emit_comet 6 %, emit_rainbow 5 %, emit_flames 2 %
//   custom_trail_pack       comet_beam 36 %, emit_custom 24 %, layered_beam_quads 20 %
//
// (Most emitters inline into `tick` under fat LTO; the percentages above are
// keyed on the inlined call-site line, which is why they name the emitter
// rather than `tick`. `push_twinkle_over` and `stacked_ink_alpha` are inlined
// further, inside the light rails / glitter frames above. `rainbow_typing_1x`
// runs `rainbow_typing_retina`'s code at 1x and was not sampled separately;
// `water_wake_glide` POST-DATES that sampling run and carries no share of it —
// its reach rests on its own bounds, see its note.)
//
// ONE THING THIS BENCH FOUND WHILE BEING BUILT, recorded because it changes how
// its own numbers should be read: `note_typed` is not a small detail of the
// rainbow workloads, it is the difference between two different programs. The
// hint is the only thing that advances the canonical typing-momentum metric, so
// without it the eased `rainbow.disp` spine never leaves 0, `rainbow_momentum_bands`
// early-returns its constant array, and the ribbon renders COLD. Measured on the
// identical script: momentum 1.00 vs 0.00, and 16_477 vs 4_156 emitted items per
// frame — a 4x difference in emitted volume alone. The shipped
// `bench_cursor_rainbow_hot_ribbon_worstcase` arms no typed hint, so despite its
// name it does not measure the hot ribbon either.
//
// DETERMINISM IS FREE, AND THE BOUNDS SPEND IT. `frand` is a xorshift over
// `self.rng`, which `tick` seeds to 0x9E37_79B9 on the first live tick; the
// injected clock is one wall sample plus a fixed integer dt per frame, and every
// age the engine reads is a DIFFERENCE of those, so nothing in a workload
// depends on when it ran. A fixed move script therefore reproduces every
// burst/nova/star roll — and every emitted count — exactly. That is why the
// volume bounds below are a modest margin around the MEASURED peak rather than
// an order-of-magnitude envelope: a count that moves at all is a change in the
// rendered frame, and this crate's rule is that a rendered pixel may not move.
// The returned `u64` frame fingerprint is the module's own golden-identity
// instrument; it is checked for "nonzero on a lit frame / exactly zero on a dark
// one" here, and is what a bit-exactness claim about any future optimisation
// should be pinned with.

use std::time::Duration;
use std::time::Instant as WallInstant;

use aterm_effects::cursor_glow::{
    CursorGlow, Geom, GlowConfig, GlowStyle, RAINBOW_WAKE_PERSIST, TrailParams,
};
use aterm_effects::trail_pack::{HaloChannel, ParticlePop, RampParams};
use aterm_render::GlowQuad;
use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, black_box, criterion_group, criterion_main,
};

// ---------------------------------------------------------------- geometry --

/// 2x retina metrics on a maximized window — the shape the hot ribbon costs
/// most in: its per-cell work is DPI-dependent (`strips = cw.div_ceil(4)
/// .clamp(1, 3)`, `row_px = 2`), so a 1x-only measurement understates the
/// per-cell strip walk by ~3x.
const RETINA: Geom = Geom {
    cw: 18,
    ch: 40,
    rows: 26,
    cols: 190,
    origin_x: 0,
    origin_y: 0,
    win_w: 3420,
    win_h: 1040,
    head: 0,
};

/// The same window at 1x. Paired with `RETINA` so a change to the per-strip
/// walk shows up as a CHANGED RATIO between the two, not just a slower number.
const LODPI: Geom = Geom {
    cw: 9,
    ch: 20,
    rows: 26,
    cols: 190,
    origin_x: 0,
    origin_y: 0,
    win_w: 1710,
    win_h: 520,
    head: 0,
};

// ------------------------------------------------------------------ clocks --

/// 8 ms per keystroke — the injected dt of the typing workloads. Fast human /
/// key-repeat cadence: it keeps the ribbon's momentum spine pinned hot and the
/// resident spark population at its steady-state worst case, which is the
/// frame a user actually watches while typing.
const TYPE_DT: Duration = Duration::from_millis(8);

/// 60 fps — the dt of workloads whose script is paced in FRAMES rather than in
/// keystrokes (jumps, glides, crossfades).
const FRAME_DT: Duration = Duration::from_millis(16);

/// Fire's own cadence: ~17 keys/s sustained, which is what climbs `coal` and
/// `disp_t` toward 1 and holds the blaze there (measured: `blaze()` never
/// leaves 0.98-1.00 across the whole window).
const FIRE_DT: Duration = Duration::from_millis(60);

/// A held delete key's autorepeat (~25/s). Faster than the POOF_MIN_GAP (0.14
/// s) rate gate, so the poof fires on roughly every fourth frame while the
/// particles from the previous one are still in flight — which is the cadence a
/// real held delete produces, and the reason this workload's per-frame average
/// sits below its peak.
const ERASE_DT: Duration = Duration::from_millis(40);

// ------------------------------------------------------------------ script --

/// Frames of script run before anything is sampled or timed — long enough for
/// every workload to reach its steady state (the shipped gate uses the same
/// 1_200 to saturate the resident spark store).
const WARM_FRAMES: usize = 1_200;

/// Frames sampled by `verify_reaches_target`. Covers several full cycles of
/// every periodic script (the longest is the 189-column typing line).
const SAMPLE_FRAMES: usize = 600;

/// Frames between cursor jumps in the jump workloads: at `FRAME_DT` that is ~5
/// jumps/s, i.e. held-arrow / word-motion navigation. The beam and the ZOOM
/// from each jump outlive the gap (spark life 650 ms = 40 frames), so the beam
/// path runs on EVERY frame — the gap only controls how often a fresh strike
/// is laid.
const JUMP_PERIOD: u64 = 12;

/// How far a jump travels: a word motion, not a screen crossing. Deliberately
/// NOT the full 190 columns — a full-width jump drives the beam and the ZOOM
/// streak straight into the MAX_QUADS = 16_384 truncation, and a stream pinned
/// at its cap can no longer witness a COUNT regression (it also measures the
/// truncate path rather than the emitters it is supposed to price). 16 columns
/// keeps every stream inside its budget with room to grow.
const JUMP_COLS: u16 = 16;
const JUMP_ROWS: u16 = 3;

/// Frames between style flips in the crossfade workload. At `FRAME_DT` that is
/// 128 ms — inside `FADE_OUT_S` (250 ms), so a fade is always live and the
/// frame runs the whole emit chain twice (the ghost's `tick` plus this one).
const FLIP_PERIOD: u64 = 8;

/// The span the SATURATING water sweep covers. Each move lays the whole swept
/// line, so the resident spark store reaches its MAX_SPARKS = 512 cap within a
/// few frames whatever this is; what it controls is how much of the grid the
/// wake covers.
const SWEEP_COLS: u16 = 60;

/// The BELOW-CAP water glide: columns the caret advances per frame, and how far
/// it travels before reversing. At `FRAME_DT` this is a smooth 240 cells/s drag
/// — a mouse selection or a held word motion — and it is what makes the
/// workload below-cap: each frame lays `GLIDE_COLS` FRESH ADJACENT cells, so the
/// resident wake is bounded by the SPAWN RATE (`GLIDE_COLS x life/dt`, ~160
/// samples at this dial) rather than by the 512-spark cap. MEASURED, that lands
/// the wake at 9_324 quads — 57 % of MAX_QUADS, so `emit_water`'s load-shed
/// return is never taken and every segment is rasterized. Adjacency is the other
/// half of the point: `emit_water` SKIPS any segment whose endpoints are more
/// than 1.6 cells apart, so a script that teleports the caret buys cheap
/// `continue`s, not per-segment work.
const GLIDE_COLS: u16 = 4;
const GLIDE_SPAN: u16 = 160;

/// The erase workload's line: `ERASE_LINE` columns of text with the caret
/// parked at `ERASE_CARET`, eaten from the caret rightwards.
const ERASE_LINE: usize = 150;
const ERASE_CARET: usize = 20;

/// The glyph the scripts type. Any non-blank works — `observe_row`'s FILL is
/// "one past the last non-blank column".
const GLYPH: char = 'a';

// ------------------------------------------------------------------ config --

/// The documented default DARK palette (a coherent pair — `fg == bg` reads as a
/// conceal-shaped theme and suppresses the tint entirely).
const DARK_FG: u32 = 0x00C8_D3F5;
const DARK_BG: u32 = 0x001A_1B26;

/// A real LIGHT palette: near-black ink on near-white paper. It has to be a
/// genuinely light pair, not the dark pair with `dark_theme` flipped —
/// `fresh_ink_glyph_palette` refuses an incoherent theme outright, and the
/// light workload's whole point is the light-only emitters (`push_twinkle_over`,
/// `fresh_ink_veil_tinted`, `emit_rainbow_rails_light`).
const LIGHT_FG: u32 = 0x002E_3440;
const LIGHT_BG: u32 = 0x00FA_FAFA;

/// Whether a style draws the shared additive beam. Mirrors the GUI resolver:
/// Water paints its own fluid wake and rainbow kitty its own banded ribbon, so
/// those two are beam-less; everything else shows the beam.
fn beam_for(style: GlowStyle) -> bool {
    !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty)
}

/// A shipped-shaped config at full intensity.
fn cfg_for(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        enabled: true,
        style,
        color: 0x00d0_d0d0,
        accent: 0x0048_c9ff,
        duration: Duration::from_millis(650),
        // The trail-length dial at its maximum, as the shipped water/rainbow
        // worst-case gates use: what bounds the live population is then the
        // spark cap and the lifetimes, not the dial.
        length: usize::MAX,
        intensity: 1.0,
        radius: 0.4,
        ring: true,
        dark_theme: true,
        theme_fg: DARK_FG,
        theme_bg: DARK_BG,
        beam: beam_for(style),
        head_dx: 0.5,
        pack: None,
        wake_persist_s: RAINBOW_WAKE_PERSIST,
    }
}

/// Laser/Beam at a SHORTER trail dial. The beam family lays one spark per
/// swept cell with no cell dedup and ignores `cfg.length` on a jump (the rod
/// spans the whole leap), so at the 650 ms worst-case dial its layered tube
/// runs `out` into the MAX_QUADS truncation on every frame — and a stream
/// pinned at its cap stops witnessing count regressions. 320 ms is inside the
/// shipped dial's range (the in-crate fixtures use 240 ms) and keeps the same
/// per-run `layered_beam_quads` walk inside the budget.
fn beam_family_cfg(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        duration: Duration::from_millis(320),
        ..cfg_for(style)
    }
}

/// A resolved Trail Pack with every channel a pack can drive turned ON: the
/// 4-layer beam stack, an interpolating colour ramp, the crown, the landing
/// ring and two particle populations. `emit_custom` is the OTHER whole arm of
/// `tick` (`cfg.pack.is_some()` bypasses every built-in emitter), and it has no
/// perf gate anywhere in the tree today.
fn full_pack() -> TrailParams {
    let mut p = TrailParams::defaults();
    p.pack_fp = 0x0BAD_F00D;
    let mut stops = [(0.0f32, 0u32); 8];
    stops[0] = (0.0, 0x0000_2040);
    stops[1] = (0.35, 0x0020_C0FF);
    stops[2] = (0.75, 0x00FF_D060);
    stops[3] = (1.0, 0x00FF_FFFF);
    p.ramp = RampParams::Stops {
        stops,
        n: 4,
        hue_step: 0.01,
    };
    p.channels.halo = HaloChannel::Add;
    p.ring.enabled = true;
    p.particles[0] = ParticlePop {
        spawn_weight: 1.0,
        vx: (-0.45, 0.45),
        vy: (-1.30, -0.30),
        gravity: 2.4,
        life: (0.25, 0.60),
        size: 0.18,
        typing_burst_max: 2,
        jump_burst_max: 8,
    };
    p.particles[1] = ParticlePop {
        spawn_weight: 0.6,
        vx: (-0.20, 0.20),
        vy: (-0.60, -0.10),
        gravity: 0.8,
        life: (0.40, 0.90),
        size: 0.12,
        typing_burst_max: 1,
        jump_burst_max: 4,
    };
    p.particle_count = 2;
    p
}

// ----------------------------------------------------------------- fixture --

/// One workload's whole world: the engine, its config/geometry, the INJECTED
/// clock, and the script's cursor + row-probe state. Built once, warmed once,
/// then stepped — by `verify_reaches_target` first and by criterion after.
struct Fixture {
    glow: CursorGlow,
    cfg: GlowConfig,
    geom: Geom,
    /// The injected clock. Advanced by exactly `dt` per step; never sampled
    /// from the wall (the wall clock belongs to criterion alone).
    now: web_time::Instant,
    dt: Duration,
    quads: Vec<GlowQuad>,
    row: u16,
    col: u16,
    /// Steps taken, for the periodic scripts (jump / flip / glide / kill cadence).
    n: u64,
    /// The cursor row's content, per COLUMN, as `Terminal::row_cols_into`
    /// hands it to the host: the typed prefix then blanks.
    text: Vec<char>,
    /// The row above (the previous line of the paragraph — full of glyphs) and
    /// the row below (blank). Both PROBED, so the star-landing gate answers
    /// `Some(true)` / `Some(false)` instead of `None` and the displaced-star
    /// placement takes its real branch in both directions.
    above: Vec<char>,
    below: Vec<char>,
    /// Erase script only: how many glyphs are still on the row.
    fill: usize,
}

impl Fixture {
    fn new(cfg: GlowConfig, geom: Geom, dt: Duration) -> Self {
        let mut above = vec![' '; geom.cols];
        for c in above.iter_mut().take(geom.cols * 2 / 3) {
            *c = GLYPH;
        }
        Fixture {
            glow: CursorGlow::default(),
            cfg,
            geom,
            // ONE wall sample, for the clock's origin only: from here the clock
            // is advanced by a fixed dt and never read from the wall again, so
            // the whole run is reproducible.
            now: web_time::Instant::now(),
            dt,
            quads: Vec::new(),
            row: 4,
            col: 0,
            n: 0,
            text: vec![' '; geom.cols],
            above,
            below: vec![' '; geom.cols],
            fill: 0,
        }
    }

    /// Feed the per-frame host probes exactly where the host does: immediately
    /// before `tick`, under what would be the terminal lock. THREE row copies
    /// and a reverse scan for the fill — the host cost the timed span excludes
    /// and `cursor_glow_host_seams/row_probe_3x190col` prices on its own.
    fn probe(&mut self) {
        self.glow
            .observe_row(self.row, self.col, &self.text, self.now);
        self.glow
            .observe_neighbor_rows(Some(&self.above), Some(&self.below));
        self.glow.note_context(false);
    }

    /// THE TIMED UNIT. Nothing here but the engine call and the frame counter
    /// the periodic scripts key off.
    fn tick(&mut self) -> u64 {
        self.n += 1;
        let cur = Some((self.row, self.col));
        self.glow.tick(
            black_box(cur),
            self.now,
            black_box(&self.cfg),
            black_box(self.geom),
            &mut self.quads,
        )
    }

    /// One typed glyph lands in the caret's cell and the caret advances one
    /// column; at the right margin the line FOLDS to the next row.
    ///
    /// The fold is a real typewriter wrap (`cr == pr + 1`, landing at column 0,
    /// launched from within two columns of the last) — `classify_move`'s
    /// `shape_wrap`, which is CONTINUED TYPING, not a jump: it lays no meteor,
    /// no ZOOM and no swept jump sparks, and the rainbow ribbon follows the
    /// typing through it instead of being retired. Running out of ROWS is the
    /// one move in these scripts that cannot be typing, so it is hinted as
    /// navigation (Ctrl-Home) — it costs ~1.5 % of frames on a 26x190 grid.
    fn type_one(&mut self) {
        self.text[self.col as usize] = GLYPH;
        if self.col as usize + 2 >= self.geom.cols {
            self.col = 0;
            self.text.fill(' '); // the fold lands on a fresh, empty row
            if self.row as usize + 1 < self.geom.rows {
                self.row += 1;
            } else {
                self.glow.note_synthetic_move(self.now);
                self.row = 0;
            }
        } else {
            self.col += 1;
        }
    }

    /// A word-motion cursor jump, alternating direction so the swept vector
    /// never degenerates into a repeat of one line.
    fn jump(&mut self) {
        let (r, c) = if self.n % (2 * JUMP_PERIOD) < JUMP_PERIOD {
            (2 + JUMP_ROWS, 6 + JUMP_COLS)
        } else {
            (2, 6)
        };
        self.row = r;
        self.col = c;
        self.text.fill(' ');
        for ch in self.text.iter_mut().take(c as usize) {
            *ch = GLYPH;
        }
    }
}

// --------------------------------------------------------------------- arm --

/// The HOST-SIDE half of one frame: advance the injected clock and drive
/// whatever host seams the script models — everything a presenting host does
/// before `tick` and OUTSIDE the engine. Kept separate from `tick` for one
/// reason: so the timed span can be `tick` and only `tick`. `probe()` alone
/// re-copies three 190-char rows per frame, which on the `off_*` scripts is
/// comparable to the whole engine call.
type Arm = fn(&mut Fixture);

/// One whole frame — arm, then tick. What `warm`/`run` drive; NOT what the
/// `cursor_glow_tick` group times.
fn step(f: &mut Fixture, arm: Arm) -> u64 {
    arm(f);
    f.tick()
}

/// Typing: the typed hint armed per keystroke (the ONLY thing that births
/// `rainbow.ink_pops`), one glyph laid, the row probe fed.
fn arm_typing(f: &mut Fixture) {
    f.now += f.dt;
    f.glow.note_synthetic_typed(f.now, 1);
    f.type_one();
    f.probe();
}

/// `arm_typing` MINUS the typed hint — the control that isolates the fresh-ink
/// family. For a strictly 1-cell forward move the hint changes exactly one
/// thing: `typed_pair`, and with it the ink-pop births (`re_anchor` needs
/// `raw_dist > 2`, `echo_run` needs `> 1`, and the fold takes `shape_wrap`'s
/// first arm, which reads no hint). So the difference between this and
/// `arm_typing` IS `emit_fresh_ink` + `emit_fresh_ink_glyphs` +
/// `emit_rainbow_wake`.
fn arm_typing_unhinted(f: &mut Fixture) {
    f.now += f.dt;
    f.type_one();
    f.probe();
}

/// The cursor never moves: `tick` sees `last == cur`, never calls `spawn`, and
/// every collection stays empty. The probe is still fed every frame, because a
/// host that is presenting frames probes whether or not anything is lit.
fn arm_idle(f: &mut Fixture) {
    f.now += f.dt;
    f.probe();
}

/// Typing with a word-motion cursor jump every `JUMP_PERIOD` frames.
fn arm_jump(f: &mut Fixture) {
    f.now += f.dt;
    if f.n.is_multiple_of(JUMP_PERIOD) {
        // This scripted jump represents deliberate word motion. The shipping
        // engine now fails closed on unproven cursor deltas, so benchmark the
        // authored ZOOM/starburst path with the same explicit provenance a
        // real navigation dispatch supplies.
        f.glow.note_synthetic_move(f.now);
        f.jump();
    } else {
        f.glow.note_synthetic_typed(f.now, 1);
        f.type_one();
    }
    f.probe();
}

/// A same-row TELEPORT between two far columns, every frame. Each move lays the
/// whole swept line, so the resident spark store reaches MAX_SPARKS = 512 in a
/// few frames and STAYS there under a warm clock — the saturated state the
/// frozen-clock fixtures reach only by never expiring anything. 512 sparks is
/// also far more wake than MAX_QUADS can hold, which is the point of
/// `water_wake_saturated` and the reason it is not the workload that prices
/// `emit_water`'s per-segment walk (see `arm_glide`).
fn arm_sweep(f: &mut Fixture) {
    f.now += f.dt;
    f.col = if f.n.is_multiple_of(2) {
        5
    } else {
        5 + SWEEP_COLS
    };
    f.probe();
}

/// A SMOOTH same-row glide, `GLIDE_COLS` cells per frame, ping-ponging across
/// `GLIDE_SPAN` columns. Every cell it lays is adjacent to the last, and the
/// population it sustains (`GLIDE_COLS x life/dt`) sits well under MAX_SPARKS —
/// so unlike `arm_sweep` the whole wake fits inside the quad budget and every
/// segment of it is rasterized instead of being shed by the load-shed return.
fn arm_glide(f: &mut Fixture) {
    f.now += f.dt;
    let leg_frames = u64::from(GLIDE_SPAN / GLIDE_COLS);
    let k = (f.n % leg_frames) as u16 * GLIDE_COLS;
    f.col = if (f.n / leg_frames).is_multiple_of(2) {
        5 + k
    } else {
        5 + GLIDE_SPAN - k
    };
    f.probe();
}

/// A held STATIONARY kill run — forward Delete / Ctrl-K / Alt-D — with the
/// probed row shrinking to match. `note_kill(now, false)` is the arm whose echo
/// does NOT move the caret, and that is the whole point of choosing it here:
/// with no cursor move there is no `spawn` and no trail at all, so every quad
/// this workload emits comes from `poof_scan` -> `spawn_poof` ->
/// `spawn_erase_sparkles` / `spawn_poof_smoke` and the particle + vapor
/// emitters.
///
/// WHAT IT DOES NOT WITNESS. `poof_scan` ORs `kill_hint` and `bs_poof_hint`
/// into one `fresh_kill` gate, but the two are NOT the same path downstream:
/// the caret-anchored fallback carries an extra `(!bs_only || erasure_proven ||
/// bs_erased)` condition that only the Backspace arm can fail, and a Backspace
/// run also moves the caret per keystroke, which would drag a ribbon through
/// the measurement. So this workload prices the KILL-CHORD poof, and nothing
/// here proves anything about the Backspace one; a Backspace workload would be
/// a separate script with a separate guard.
///
/// The RATE GATE is real: POOF_MIN_GAP is 0.14 s, so at 40 ms/frame the poof
/// fires on roughly every fourth frame while the particles from the last one
/// are still in flight — which is why this workload's per-frame average sits
/// below its peak.
fn arm_erase(f: &mut Fixture) {
    f.now += f.dt;
    let n = if f.n % 8 == 7 { 4 } else { 1 };
    if f.fill <= ERASE_CARET + n {
        // The tail is gone — the shell reprints the rest of the line. The caret
        // does not move (it is already where the reprint starts), so this frame
        // spawns nothing at all; it just refills the probe.
        for c in f.text.iter_mut().take(ERASE_LINE) {
            *c = GLYPH;
        }
        f.fill = ERASE_LINE;
        f.probe();
        return;
    }
    f.glow.note_kill(f.now, false);
    f.fill -= n;
    for c in f.text[f.fill..f.fill + n].iter_mut() {
        *c = ' ';
    }
    f.probe();
}

/// Typing with the style flipped every `FLIP_PERIOD` frames. Each flip moves
/// the in-flight light into a boxed ghost animator that keeps ticking under its
/// SNAPSHOTTED old config, so the frame runs the whole emit chain twice (up to
/// FADE_CAP = 2 ghosts) and merges six streams. Structurally the worst frame
/// the module can produce.
fn arm_crossfade(f: &mut Fixture) {
    if f.n.is_multiple_of(FLIP_PERIOD) {
        f.cfg.style = if matches!(f.cfg.style, GlowStyle::RainbowKitty) {
            GlowStyle::Fire
        } else {
            GlowStyle::RainbowKitty
        };
        f.cfg.beam = beam_for(f.cfg.style);
    }
    arm_typing(f);
}

// ------------------------------------------------------------- observation --

/// The six emitted streams' lengths for one frame.
#[derive(Clone, Copy, Default)]
struct Volume([usize; 6]);

const STREAMS: [&str; 6] = ["out", "under", "halos", "patches", "charred", "halo_cells"];

impl Volume {
    fn of(f: &Fixture) -> Self {
        Volume([
            f.quads.len(),
            f.glow.under_quads().len(),
            f.glow.halos().len(),
            f.glow.patches().len(),
            f.glow.charred().len(),
            f.glow.halo_cells().len(),
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

/// The engine STATE scalars the emitters' own gates key off, sampled every
/// frame as `(min, max)` over the window. These pin the GATE rather than the
/// output, which is what a volume number cannot do:
///
///   * `typing_momentum` IS the value the eased `rainbow.disp` spine chases,
///     and `rainbow_momentum_bands` (the six-HSV-round-trip hot spot) returns
///     its constant array unchanged whenever that spine is under 0.005. A cold
///     spine means the ribbon is the COLD 1-strip path no matter how many
///     quads came out. Only a `note_typed`-paired forward move advances it.
///   * `blaze` is the raw fire heat the flame field's height and the FirePatch
///     band walk ride.
///   * `erase_momentum` is advanced by exactly `note_backspace`/`note_kill`, so
///     it is the licence the erase poof needs, observed from outside.
#[derive(Clone, Copy)]
struct State([(f32, f32); 3]);

const SCALARS: [&str; 3] = ["momentum", "blaze", "erase_mom"];

impl State {
    fn of(f: &Fixture) -> [f32; 3] {
        [
            f.glow.typing_momentum(f.now),
            f.glow.blaze(),
            f.glow.erase_momentum(f.now),
        ]
    }

    fn fold(&mut self, v: [f32; 3]) {
        for (a, b) in self.0.iter_mut().zip(v) {
            a.0 = a.0.min(b);
            a.1 = a.1.max(b);
        }
    }
}

impl Default for State {
    fn default() -> Self {
        State([(f32::MAX, f32::MIN); 3])
    }
}

/// What running a script for a while showed.
struct Sampled {
    /// Per-frame peak of each stream.
    peak: Volume,
    /// Sum over the window, for the per-frame average in the report and for the
    /// `total` volume benchmark (the most sensitive single count this file has:
    /// it moves when ANY frame's emission moves, not just the peak one).
    sum: Volume,
    frames: usize,
    /// Frames whose fingerprint was non-zero — i.e. that emitted anything at
    /// all. `tick` returns 0 from both dark early-returns and from an idle
    /// frame, so this separates "lit" from "dark" without a stream read.
    lit: usize,
    /// Frames on which `patches()` was non-empty while the live style was NOT
    /// Fire. `patch_out` is cleared at the top of every tick and written by
    /// exactly one emitter (`emit_flames`, Fire-gated), so the only other way
    /// it can be non-empty is `tick_fades` merging a Fire GHOST's stream — the
    /// external witness that a crossfade really ran the emit chain twice.
    ghost: usize,
    state: State,
}

fn run(f: &mut Fixture, arm: Arm, frames: usize) -> Sampled {
    let mut s = Sampled {
        peak: Volume::default(),
        sum: Volume::default(),
        frames,
        lit: 0,
        ghost: 0,
        state: State::default(),
    };
    for _ in 0..frames {
        let fp = step(f, arm);
        let v = Volume::of(f);
        s.peak.max_with(v);
        s.sum.add(v);
        s.lit += usize::from(fp != 0);
        s.ghost += usize::from(v.0[3] > 0 && !matches!(f.cfg.style, GlowStyle::Fire));
        s.state.fold(State::of(f));
    }
    s
}

fn warm(f: &mut Fixture, arm: Arm) {
    for _ in 0..WARM_FRAMES {
        step(f, arm);
    }
}

// ------------------------------------------------------------------ guards --

/// Inclusive `[min, max]` bounds on a stream's PEAK length over the sample
/// window. Both sides are load-bearing: the min proves the workload reached the
/// emitter it claims (`>= 0` passes on a dead engine), the max catches a
/// regression in quad COUNT that a timing number alone would hide.
///
/// The maxima below are a MODEST MARGIN over the measured peak (~+12 %), not a
/// number chosen so it can never fail. The engine is deterministic under a
/// fixed script and an injected clock, so the measured peak is the same number
/// on every machine and every build; an envelope 60x wide would let a stream
/// grow by an order of magnitude in silence.
type Range = (usize, usize);

/// `CursorGlow::MAX_HALOS`, mirrored: the halo stream is truncated to this
/// INSIDE `tick`, so a saturated halo stream reads exactly this whatever was
/// pushed into it.
const HALO_CAP: usize = 512;

/// "This workload SATURATES the halo stream." Not a two-sided count guard and
/// not pretending to be one: it is live only in the "fell off the cap"
/// direction (a lost emitter, a shrunken ring), because a count INCREASE cannot
/// be observed past the truncation. The workloads that keep a real two-sided
/// halo bound are the ones whose halo stream stays under the cap — fire, laser,
/// erase, water, custom pack.
const AT_HALO_CAP: Range = (HALO_CAP, HALO_CAP);

/// `CursorGlow::MAX_QUADS`, mirrored — the same story for the `out` stream.
const QUAD_CAP: usize = 16_384;

/// "This workload SATURATES the quad budget." Exactly one workload declares it.
const AT_QUAD_CAP: Range = (QUAD_CAP, QUAD_CAP);

/// The extra, decisive proof a workload carries beyond its volume bounds.
enum Witness {
    /// Nothing beyond the bounds — the bounds themselves are decisive.
    Bounds,
    /// This workload must be completely DARK while a CONTROL — the identical
    /// script through an engine missing only the off switch — lights up. What
    /// makes an "off" cost a measurement instead of a tautology.
    DarkUnless {
        what: &'static str,
        control: fn() -> Fixture,
        arm: Arm,
    },
    /// A CONTROL missing exactly one piece of the target state must emit
    /// dramatically less: `control_total * ratio < workload_total`. What proves
    /// the workload's own state is what reaches the code under test.
    Dimmer {
        what: &'static str,
        control: fn() -> Fixture,
        arm: Arm,
        ratio: usize,
    },
    /// A stream only the OUTGOING style's emitter can write was non-empty while
    /// the live style could not have written it, on `.0 ..= .1` of the sampled
    /// frames. Two-sided for the same reason the volume bounds are: the count
    /// is deterministic, so a drift in either direction is a change.
    Ghost(Range),
}

struct Workload {
    name: &'static str,
    /// One line for the report: what state this workload is in.
    note: &'static str,
    build: fn() -> Fixture,
    arm: Arm,
    bounds: [Range; 6],
    /// `(min-over-window >= .0, max-over-window <= .1)` for each engine state
    /// scalar. `(0.0, 1.0)` makes no claim (every scalar is clamped to 0..=1).
    state: [(f32, f32); 3],
    /// Bounds on the fraction of sampled frames that emitted anything, in
    /// percent. `(0, 0)` for the dark workloads; a positive lower bound for
    /// every workload that claims to draw.
    lit_pct: Range,
    witness: Witness,
}

/// Build the workload, run it to steady state, and PROVE it is in the state it
/// claims before a single nanosecond is timed. Returns the warmed fixture (the
/// timed run continues from the verified state) and what was observed.
fn verify_reaches_target(w: &Workload) -> (Fixture, Sampled) {
    let mut f = (w.build)();
    warm(&mut f, w.arm);
    let s = run(&mut f, w.arm, SAMPLE_FRAMES);

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
    for (i, (&(min, max), &(lo, hi))) in s.state.0.iter().zip(w.state.iter()).enumerate() {
        assert!(
            min >= lo && max <= hi,
            "{}: {} ranged [{min:.3}, {max:.3}] over the window, outside the \
             required [{lo:.3}, {hi:.3}] — the emitter GATE this workload aims \
             at is not in the state it claims",
            w.name,
            SCALARS[i]
        );
    }

    match w.witness {
        Witness::Bounds => {}
        Witness::DarkUnless { what, control, arm } => {
            assert_eq!(
                s.peak.total(),
                0,
                "{}: emitted light on an OFF workload — nothing may reach the \
                 streams here",
                w.name
            );
            assert_eq!(
                s.lit, 0,
                "{}: a non-zero fingerprint on an OFF workload",
                w.name
            );
            let mut c = (control)();
            warm(&mut c, arm);
            let cs = run(&mut c, arm, SAMPLE_FRAMES);
            report(&format!("{}.control", w.name), what, &cs);
            assert!(
                cs.peak.total() > 0 && cs.lit > 0,
                "{}: the CONTROL ({what}) drew nothing either, so this \
                 workload's zero proves nothing about the off path — it would \
                 be measuring a script that had no light to suppress",
                w.name
            );
        }
        Witness::Dimmer {
            what,
            control,
            arm,
            ratio,
        } => {
            let mut c = (control)();
            warm(&mut c, arm);
            let cs = run(&mut c, arm, SAMPLE_FRAMES);
            let (mine, theirs) = (s.sum.total(), cs.sum.total());
            report(&format!("{}.control", w.name), what, &cs);
            assert!(
                theirs * ratio < mine,
                "{}: the control ({what}) emitted {theirs} vs this workload's \
                 {mine} — less than the {ratio}x separation that proves the \
                 state under test is what reaches the code under test",
                w.name
            );
        }
        Witness::Ghost((lo, hi)) => {
            println!(
                "VOLUME {}.ghost | fire ghost merged on {} of {} frames",
                w.name, s.ghost, s.frames
            );
            // Only the ~half of the window whose LIVE style is not Fire can
            // witness anything (on a Fire frame the live emitter owns the
            // stream), so ~300 of 600 is the ceiling and anything near it means
            // essentially every non-Fire frame carried a ghost.
            assert!(
                s.ghost >= lo && s.ghost <= hi,
                "{}: {} of {} frames merged a FIRE ghost's patch stream while a \
                 non-Fire style was live, outside [{lo}, {hi}] — the crossfade \
                 is not running the emit chain twice at the cadence it claims",
                w.name,
                s.ghost,
                s.frames
            );
        }
    }
    (f, s)
}

/// The human-readable per-workload volume line: peak and per-frame average of
/// every emitted stream, plus the state scalars and the lit-frame count. The
/// PEAKS and the window total also become criterion measurements (see
/// `bench_counts`) so a count regression is stored and A/B'd like a time
/// regression; this line is what a human reads while the run scrolls by, and it
/// is the only place the per-frame averages appear.
fn report(name: &str, note: &str, s: &Sampled) {
    let avg = |i: usize| s.sum.0[i] / s.frames;
    println!(
        "VOLUME {name:<22} | peak out/under/halos/patches/charred/halo_cells \
         {}/{}/{}/{}/{}/{} | per-frame avg {}/{}/{}/{}/{}/{} | momentum {:.2}-{:.2} \
         blaze {:.2}-{:.2} erase {:.2}-{:.2} | lit {}/{} | {note}",
        s.peak.0[0],
        s.peak.0[1],
        s.peak.0[2],
        s.peak.0[3],
        s.peak.0[4],
        s.peak.0[5],
        avg(0),
        avg(1),
        avg(2),
        avg(3),
        avg(4),
        avg(5),
        s.state.0[0].0,
        s.state.0[0].1,
        s.state.0[1].0,
        s.state.0[1].1,
        s.state.0[2].0,
        s.state.0[2].1,
        s.lit,
        s.frames
    );
}

// --------------------------------------------------------------- fixtures ---

fn f_rainbow_retina() -> Fixture {
    Fixture::new(cfg_for(GlowStyle::RainbowKitty), RETINA, TYPE_DT)
}

fn f_rainbow_lodpi() -> Fixture {
    Fixture::new(cfg_for(GlowStyle::RainbowKitty), LODPI, TYPE_DT)
}

/// The light-theme arm — a genuinely light palette, not the dark pair with the
/// flag flipped (`fresh_ink_glyph_palette` refuses an incoherent theme and the
/// glyph-tint stream would silently stay empty).
fn f_rainbow_light() -> Fixture {
    let mut cfg = cfg_for(GlowStyle::RainbowKitty);
    cfg.dark_theme = false;
    cfg.theme_fg = LIGHT_FG;
    cfg.theme_bg = LIGHT_BG;
    Fixture::new(cfg, RETINA, TYPE_DT)
}

fn f_rainbow_jumps() -> Fixture {
    Fixture::new(cfg_for(GlowStyle::RainbowKitty), RETINA, FRAME_DT)
}

fn f_disabled() -> Fixture {
    let mut cfg = cfg_for(GlowStyle::RainbowKitty);
    cfg.enabled = false;
    Fixture::new(cfg, RETINA, TYPE_DT)
}

fn f_unfocused() -> Fixture {
    let mut cfg = cfg_for(GlowStyle::RainbowKitty);
    cfg.intensity = 0.0;
    Fixture::new(cfg, RETINA, TYPE_DT)
}

fn f_custom() -> Fixture {
    let mut cfg = cfg_for(GlowStyle::Custom);
    cfg.pack = Some(full_pack());
    cfg.beam = true;
    Fixture::new(cfg, RETINA, FRAME_DT)
}

/// Water at 1x, driven by the SATURATING teleport sweep. The fluid wake's quad
/// count is driven by its SEGMENT count (one per live spark, 512 at the cap)
/// times the per-segment pixel span, and 511 segments overrun the 16_384 budget
/// at any real cell size. 1x keeps the identical 511-segment walk; what it
/// cannot do is keep it inside the budget — see `water_wake_saturated`.
fn f_water() -> Fixture {
    Fixture::new(cfg_for(GlowStyle::Water), LODPI, TYPE_DT)
}

/// Water at 1x, driven by the BELOW-CAP glide: the same wake, a population the
/// quad budget can actually hold, so every segment runs.
fn f_water_glide() -> Fixture {
    Fixture::new(cfg_for(GlowStyle::Water), LODPI, FRAME_DT)
}

fn f_fire() -> Fixture {
    Fixture::new(cfg_for(GlowStyle::Fire), RETINA, FIRE_DT)
}

/// BEAM — the clean tube: no particles, no scintillation, no lightning. That is
/// exactly what makes it the honest probe for the shared beam rasterizer: with
/// `beam: false` this style emits essentially nothing, so the paired control
/// isolates `emit_comet` -> `aterm_render::layered_beam_quads` and nothing else.
fn f_beam() -> Fixture {
    Fixture::new(beam_family_cfg(GlowStyle::Beam), RETINA, FRAME_DT)
}

fn f_beam_off() -> Fixture {
    let mut f = f_beam();
    f.cfg.beam = false;
    f
}

/// Laser — the same layered-beam path PLUS `emit_bolts`, the lightning strike
/// no other style lays. At 1x for the same reason the water wake is: the
/// strike's quad count at 2x pins `out` at MAX_QUADS on every frame, and a
/// capped stream cannot witness a count regression.
fn f_laser() -> Fixture {
    Fixture::new(beam_family_cfg(GlowStyle::Laser), LODPI, FRAME_DT)
}

fn f_erase() -> Fixture {
    let mut f = Fixture::new(cfg_for(GlowStyle::RainbowKitty), RETINA, ERASE_DT);
    for c in f.text.iter_mut().take(ERASE_LINE) {
        *c = GLYPH;
    }
    f.fill = ERASE_LINE;
    f.col = ERASE_CARET as u16;
    f
}

fn f_crossfade() -> Fixture {
    Fixture::new(cfg_for(GlowStyle::RainbowKitty), RETINA, FRAME_DT)
}

// --------------------------------------------------------------- workloads --

/// "This scalar is not what this workload is about" — every scalar is clamped
/// to 0..=1, so this pair can never fail.
const ANY: (f32, f32) = (0.0, 1.0);

/// Every stream must stay exactly empty.
const DARK: [Range; 6] = [(0, 0); 6];

fn workloads() -> Vec<Workload> {
    vec![
        // ---- the OFF costs: what a frame costs when nothing is drawn -------
        Workload {
            name: "off_disabled",
            note: "master switch off, cursor typing",
            build: f_disabled,
            arm: arm_typing,
            bounds: DARK,
            state: [ANY; 3],
            lit_pct: (0, 0),
            witness: Witness::DarkUnless {
                what: "the same typing script with enabled = true",
                control: f_rainbow_retina,
                arm: arm_typing,
            },
        },
        Workload {
            name: "off_unfocused",
            note: "intensity 0 (unfocus), cursor typing",
            build: f_unfocused,
            arm: arm_typing,
            bounds: DARK,
            state: [ANY; 3],
            lit_pct: (0, 0),
            witness: Witness::DarkUnless {
                what: "the same typing script at intensity 1",
                control: f_rainbow_retina,
                arm: arm_typing,
            },
        },
        Workload {
            name: "off_idle_rainbow",
            note: "enabled, nothing live, cursor still",
            build: f_rainbow_retina,
            arm: arm_idle,
            bounds: DARK,
            // The spine must be stone cold: an idle engine that still carried
            // momentum would be running the warm integrators, not the idle path.
            state: [(0.0, 0.0), (0.0, 0.0), ANY],
            lit_pct: (0, 0),
            witness: Witness::DarkUnless {
                what: "the same engine with the cursor moving",
                control: f_rainbow_retina,
                arm: arm_typing,
            },
        },
        Workload {
            name: "off_idle_custom_pack",
            note: "enabled Trail Pack, nothing live, cursor still",
            build: f_custom,
            arm: arm_idle,
            bounds: DARK,
            state: [(0.0, 0.0), (0.0, 0.0), ANY],
            lit_pct: (0, 0),
            witness: Witness::DarkUnless {
                what: "the same pack with the cursor moving",
                control: f_custom,
                arm: arm_typing,
            },
        },
        // ---- the ON costs --------------------------------------------------
        Workload {
            name: "rainbow_typing_retina",
            note: "2x hot ribbon + ink pops + wake, dark",
            build: f_rainbow_retina,
            arm: arm_typing,
            bounds: [
                (5_350, 6_500),
                (9_900, 12_000),
                AT_HALO_CAP,
                (0, 0),
                (0, 0),
                (0, 0),
            ],
            // THE GATE, not the output: a spine this warm is what takes
            // `rainbow_momentum_bands` past its `disp < 0.005` early return and
            // puts the ribbon on the 3-strip hot path.
            state: [(0.90, 1.0), ANY, (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Dimmer {
                what: "the same script with no note_typed: no ink pops, and a spine \
                       that never leaves zero",
                control: f_rainbow_retina,
                arm: arm_typing_unhinted,
                ratio: 3,
            },
        },
        Workload {
            name: "rainbow_typing_1x",
            note: "1x hot ribbon + ink pops + wake, dark",
            build: f_rainbow_lodpi,
            arm: arm_typing,
            bounds: [
                (3_280, 4_000),
                (5_200, 6_350),
                AT_HALO_CAP,
                (0, 0),
                (0, 0),
                (0, 0),
            ],
            state: [(0.90, 1.0), ANY, (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Bounds,
        },
        Workload {
            name: "rainbow_typing_light",
            note: "2x light rails/veils/twinkles + glyph tint",
            build: f_rainbow_light,
            arm: arm_typing,
            bounds: [
                (370, 455),
                // The light body is source-over VEIL RAILS in `halos`, not
                // additive quads in `under`: an empty under-stream here is the
                // theme fork working, and a non-empty one would mean the dark
                // arm ran.
                (0, 0),
                AT_HALO_CAP,
                (0, 0),
                // `charred` is written by exactly one emitter,
                // `emit_fresh_ink_glyphs`, which is light-theme AND live-pop
                // gated — so a non-empty glyph-tint stream is a two-in-one
                // witness: the light arm ran, and `note_typed` really did birth
                // ink pops. Its ceiling is FRESH_INK_CAP = 32 (the pop ring),
                // and the measured peak is 28: the bound is the structural cap.
                (25, 32),
                (0, 0),
            ],
            state: [(0.90, 1.0), ANY, (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Bounds,
        },
        Workload {
            name: "rainbow_jump_bursts",
            note: "ZOOM streaks + landing starbursts",
            build: f_rainbow_jumps,
            arm: arm_jump,
            bounds: [
                (6_400, 7_800),
                (7_690, 9_370),
                AT_HALO_CAP,
                (0, 0),
                (0, 0),
                (0, 0),
            ],
            state: [ANY, ANY, (0.0, 0.0)],
            lit_pct: (98, 100),
            witness: Witness::Bounds,
        },
        Workload {
            name: "water_wake_saturated",
            note: "512-spark fluid wake OVER budget: the load-shed path",
            build: f_water,
            arm: arm_sweep,
            // `out` is pinned AT MAX_QUADS here and only here, and what that
            // measures is the LOAD SHED, not the whole wake: `emit_water` walks
            // its segments newest-first and RETURNS the moment `out` is full,
            // so past the cap the per-segment `water_ramp(0.34)` +
            // `comet_beam` work simply stops running. This workload prices the
            // frame a pathological wake actually costs (the vertex walk over
            // all 512 sparks, then as many segments as the budget holds, then
            // the truncate); `water_wake_glide` is the one that prices
            // `emit_water`'s per-segment walk to completion.
            bounds: [AT_QUAD_CAP, (0, 0), (3, 8), (0, 0), (0, 0), (0, 0)],
            state: [(0.0, 0.0), ANY, (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Bounds,
        },
        Workload {
            name: "water_wake_glide",
            note: "below-cap fluid wake: every segment rasterized",
            build: f_water_glide,
            arm: arm_glide,
            // BELOW the cap by construction (see `GLIDE_COLS`), so this `out`
            // bound is a real two-sided count guard on the wake — the one thing
            // the saturated workload above structurally cannot give.
            bounds: [(8_580, 10_440), (0, 0), (3, 8), (0, 0), (0, 0), (0, 0)],
            state: [(0.0, 0.0), ANY, (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Bounds,
        },
        Workload {
            name: "fire_blaze_sustained",
            note: "sustained blaze: patch field, halo cells, forge",
            build: f_fire,
            arm: arm_typing,
            bounds: [
                (1_720, 2_100),
                (0, 0),
                (388, 474),
                (56, 69),
                // Fire's charring is retired ("NO CHAR, NO VEIL" —
                // `emit_flames` takes `_charred` and never writes it), so an
                // empty stream here is the CORRECT state, and a non-empty one
                // means charring came back without this bound being revisited.
                (0, 0),
                (41, 51),
            ],
            // A blaze this hot is what the flame field's height, its coverage
            // ceiling and the FirePatch band walk all ride.
            state: [ANY, (0.55, 1.0), (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Bounds,
        },
        Workload {
            name: "beam_tube_jump",
            note: "layered_beam_quads via emit_comet, beam = true",
            build: f_beam,
            arm: arm_jump,
            bounds: [
                (10_220, 12_440),
                (0, 0),
                AT_HALO_CAP,
                (0, 0),
                (0, 0),
                (0, 0),
            ],
            state: [ANY, ANY, (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Dimmer {
                what: "the same jumps with beam = false — what tests/cursor_bench.rs \
                       hardcodes, and why emit_comet has never been measured",
                control: f_beam_off,
                arm: arm_jump,
                // Measured separation is 12.8x on the six-stream window total
                // (the beam-less control still lays a saturated halo stream,
                // which is why this is not the 40x the `out` stream alone
                // suggests).
                ratio: 10,
            },
        },
        Workload {
            name: "laser_bolt_jump",
            note: "emit_bolts lightning + the pooled comet_verts arm",
            build: f_laser,
            arm: arm_jump,
            bounds: [(9_230, 11_240), (0, 0), (132, 162), (0, 0), (0, 0), (0, 0)],
            state: [ANY, ANY, (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Bounds,
        },
        Workload {
            name: "erase_poof_run",
            note: "stationary kill run: the poof and nothing else",
            build: f_erase,
            arm: arm_erase,
            // `under` is the rainbow ribbon's stream, and a caret that never
            // moves lays no ribbon — so an EMPTY under-stream is the proof that
            // what is being timed is the poof and only the poof.
            bounds: [(44, 54), (0, 0), (92, 112), (0, 0), (0, 0), (0, 0)],
            // The erase metric is advanced by exactly `note_backspace` and
            // `note_kill`, so a run this hard IS the poof's licence, observed
            // from outside; and a typing spine at zero proves the light being
            // measured is the erase path's, not a typing ribbon's.
            state: [(0.0, 0.0), ANY, (0.60, 1.0)],
            lit_pct: (95, 100),
            witness: Witness::Bounds,
        },
        Workload {
            name: "style_crossfade",
            note: "live ghost: the emit chain runs twice per frame",
            build: f_crossfade,
            arm: arm_crossfade,
            bounds: [
                (1_890, 2_300),
                (620, 760),
                AT_HALO_CAP,
                (89, 109),
                (0, 0),
                (76, 93),
            ],
            state: [ANY, ANY, (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Ghost((272, 310)),
        },
        Workload {
            name: "custom_trail_pack",
            note: "the emit_custom arm: beam, ramp, crown, ring, 2 pops",
            build: f_custom,
            arm: arm_jump,
            bounds: [(11_320, 13_790), (0, 0), (3, 8), (0, 0), (0, 0), (0, 0)],
            state: [ANY, ANY, (0.0, 0.0)],
            lit_pct: (100, 100),
            witness: Witness::Bounds,
        },
    ]
}

// ------------------------------------------------------------ volume group --

/// Record a COUNT as a criterion measurement. The reported "time" in
/// NANOSECONDS **is** the item count — 1 ns == 1 emitted item, so a peak of
/// 5_823 quads reads as `5.8230 µs` and never moves by so much as a nanosecond
/// unless the count moved. That is the whole point: counts now land in
/// `target/criterion`, get baselines, get `--save-baseline`/`--baseline`
/// comparisons, and print `Performance has regressed` — exactly like the
/// timings — instead of scrolling past in stdout for someone to diff by hand.
///
/// TWO THINGS HERE ARE NOT CEREMONY.
///
///   * The spin loop. Criterion's warm-up phase measures the WALL time a
///     routine takes — only the SAMPLE VALUE comes from `iter_custom`'s return
///     — so a routine that returned instantly would double `iters` forever.
///     The counter burns time proportional to `iters` and is discarded.
///   * The `k % 4` nanoseconds. A perfectly constant sample has zero variance,
///     and criterion's PDF plot divides by the kernel bandwidth it derives from
///     that variance: 0/0, NaN, `assertion failed: !x.is_nan()`. Three spare
///     nanoseconds spread over a whole SAMPLE (hundreds of thousands of
///     iterations) make the distribution non-degenerate while moving the
///     reported per-iteration value by ~1e-5 ns — five orders of magnitude
///     below the one-item resolution this benchmark exists to report.
fn bench_count(g: &mut BenchmarkGroup<'_, WallTime>, id: &str, count: usize) {
    assert!(
        count > 0,
        "{id}: a zero count cannot be recorded as a duration — only streams \
         with a positive lower bound may be recorded, and that bound is \
         asserted before this runs"
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

/// Every count a workload's guards assert, handed to criterion as its own
/// benchmark: each stream's PEAK (the number the bound is written against) and
/// the window TOTAL across all six streams (the most sensitive one — it moves
/// when any frame's emission moves, not only the peak frame).
///
/// Streams whose asserted lower bound is 0 are skipped: they are the ones a
/// workload proves EMPTY, a zero cannot be encoded as a duration, and the
/// `(0, 0)` bound already catches anything appearing there.
fn bench_counts(g: &mut BenchmarkGroup<'_, WallTime>, w: &Workload, s: &Sampled) {
    for (i, (&peak, &(lo, _))) in s.peak.0.iter().zip(w.bounds.iter()).enumerate() {
        if lo > 0 {
            bench_count(g, &format!("{}/{}", w.name, STREAMS[i]), peak);
        }
    }
    if s.sum.total() > 0 {
        bench_count(g, &format!("{}/total", w.name), s.sum.total());
    }
    if matches!(w.witness, Witness::Ghost(_)) {
        bench_count(g, &format!("{}/ghost_frames", w.name), s.ghost);
    }
}

// -------------------------------------------------------------- the groups --

fn cursor_glow_tick(c: &mut Criterion) {
    // PROVE FIRST, TIME SECOND. Every workload is built, warmed and verified
    // before a single nanosecond is measured; the warmed fixture and what was
    // observed are carried forward so the timed run continues from the verified
    // state and the volume group records the verified counts.
    let mut verified: Vec<(Workload, Fixture, Sampled)> = workloads()
        .into_iter()
        .map(|w| {
            let (f, s) = verify_reaches_target(&w);
            (w, f, s)
        })
        .collect();

    {
        let mut group = c.benchmark_group("cursor_glow_tick");
        // THE FLOOR UNDER EVERY NUMBER IN THIS GROUP. Timing `tick` alone means
        // one `Instant::now()` pair per iteration inside the measured span;
        // this is that pair with an empty span between them. Subtract it for an
        // absolute cost; ignore it for an A/B (it cancels).
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
                // state, with the script continuing to drive the engine — so
                // the state under test is sustained for the whole measurement
                // instead of decaying away after the first few iterations. The
                // script's own cost is armed OUTSIDE the timed span; what is
                // timed is `tick`.
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
        // WHAT THE ENGINE NUMBERS EXCLUDE, priced under names that cannot be
        // mistaken for `tick`. `row_probe_3x190col` is the per-frame host probe
        // — three 190-char row copies plus the reverse fill scan — and
        // `typing_arm_190col` is the whole armed frame minus the engine
        // (clock += dt, `note_typed`, one glyph, that same probe). Add the
        // latter to a `cursor_glow_tick` number, subtract `timer_floor`, and
        // you have the cost of a whole presented frame.
        let mut group = c.benchmark_group("cursor_glow_host_seams");
        let mut probed = f_rainbow_retina();
        for ch in probed.text.iter_mut().take(120) {
            *ch = GLYPH;
        }
        probed.col = 120;
        group.bench_function("row_probe_3x190col", |b| {
            b.iter(|| black_box(&mut probed).probe());
        });
        let mut armed = f_rainbow_retina();
        group.bench_function("typing_arm_190col", |b| {
            b.iter(|| arm_typing(black_box(&mut armed)));
        });
        group.finish();
    }

    {
        // The counts, as measurements. These cost no real time to produce (they
        // were sampled during verification), so the group runs at criterion's
        // floor rather than at the timing group's budget.
        let mut group = c.benchmark_group("cursor_glow_volume");
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

criterion_group!(benches, cursor_glow_tick);
criterion_main!(benches);
