// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow Kitty v2 frame-cost gate (`docs/design/RAINBOW-KITTY-V2.md` §18, §21).
//!
//! Drives the v2 [`Engine`] alone, and `CursorGlow` (style `RainbowKitty`, v2 through the real
//! seam), through the §21 and §27 gestures at 120 Hz and asserts:
//!
//! * zero allocation on every steady tick after warm-up and through the idle run-out (a
//!   counting global allocator); through the seam, every eighth measured frame withholds one
//!   row the witness asked for;
//! * idle → zero: after the last key, `next_change_deadline == None`,
//!   `needs_frame_cadence == false`, `fingerprint == 0` and a tick writes nothing;
//! * the stream caps (`MAX_QUADS` 16 384, `MAX_HALOS` 512) on every tick.
//!
//! `benches/rainbow_kitty_v2_frame_cost.rs` mounts this file and holds §18's CPU budget
//! (p50 ≤ 0.4 ms, p90 ≤ 0.6 ms per tick) for the engine alone and through the seam.
//!
//! ```sh
//! # the deterministic laws (zero-alloc, idle → zero, caps) — every run:
//! targo --unverified test -p aterm-effects --test effects_suite -- rainbow_kitty_v2_frame_cost:: --nocapture
//! # the CPU budget gate — release only, like benches/cursor_bench.rs:
//! targo --unverified bench -p aterm-effects --bench rainbow_kitty_v2_frame_cost
//! # on a host with no §18 measurement on record, scale the two budgets (nothing else; the
//! # knob is shared with benches/cursor_bench.rs, see `common::budget_scale`):
//! RK_FRAME_COST_BUDGET_SCALE=2 targo --unverified bench -p aterm-effects \
//!     --bench rainbow_kitty_v2_frame_cost
//! ```
//!
//! The suite's allocator (`alloc_count`) counts only the thread that switched it
//! on, while a driver call is on its stack, and keeps its tally on that thread
//! too, so the laws' twelve (scenario x engine) cells run on
//! [`common::par_map`]'s pool, beside every other test of `effects_suite`.

use std::time::{Duration, Instant};

use crate::alloc_count::{SIZE_SLOTS, allocations_during, assert_counter_is_per_thread};
use crate::common::par_map;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle, SoundCue, band_pos};
use aterm_effects::rainbow_kitty::{
    CaretSeam, Config, Dir, Engine, Event, Frame, Licence, TypedClass, witness::WITNESS_ROWS,
};
use aterm_render::{BeamVertex, GlowQuad, RainHalo};

// The fixture: one geometry, one config, both engines.

const HZ: u64 = 120;

/// One tick, 8.333 ms.
fn period() -> Duration {
    Duration::from_nanos(1_000_000_000 / HZ)
}

/// Ticks in `ms` of scenario time, rounded.
fn ticks_in_ms(ms: u64) -> usize {
    (ms * HZ).div_ceil(1000) as usize
}

/// The family's worst-case fixture (tests/cursor_bench.rs, "2× retina cell
/// metrics"), the geometry §18's ribbon numbers are stated at.
fn geometry() -> Geom {
    Geom {
        cw: 18,
        ch: 40,
        rows: 26,
        cols: 190,
        origin_x: 0,
        origin_y: 0,
        win_w: 3420,
        win_h: 1040,
        head: 0,
    }
}

/// The shipped-default dark rainbow kitty config, as tests/cursor_bench.rs
/// builds it.
fn glow_config() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style: GlowStyle::RainbowKitty,
        color: 0x00d0_d0d0,
        accent: 0x0048_c9ff,
        duration: Duration::from_millis(650),
        length: usize::MAX,
        intensity: 1.0,
        audible: true,
        radius: 0.4,
        ring: true,
        beam: false,
        head_dx: 0.5,
    }
}

/// The row every scenario types on.
const ROW: u16 = 12;

// The script: one vocabulary, replayed to both engines.

/// One host gesture.
#[derive(Clone, Copy, Debug)]
enum Action {
    /// One typed glyph echoed forward one cell.
    Typed(char),
    /// A keyed same-row navigation of `dcol` cells (Ctrl-A/E, word motions).
    Nav(i32),
    /// One Backspace: the caret retreats one cell.
    Backspace,
    /// A row-band move the host replays before the tick (the band path): screen
    /// rows `top..=bottom` moved by `delta` (Codex's inline viewport sliding
    /// down per streamed line, its transcript archiving up). The caret rides
    /// its band (`band_pos`) as the host sees it on the next present.
    BandMove(u16, u16, i16),
    /// A sing-along bar (§27): `n` stars fanned from the caret, with the
    /// shockwave ring on every fourth. The one hand-off that stages a `Sow`
    /// without being a cursor event, so it checks the `SOW_SCRATCH`
    /// reservation.
    Party(u8, bool),
}

/// A gesture on a tick.
#[derive(Clone, Copy, Debug)]
struct Step {
    tick: usize,
    action: Action,
}

/// One §21 scenario.
pub(crate) struct Scenario {
    pub(crate) name: &'static str,
    start: (u16, u16),
    ticks: usize,
    /// Ticks before this are set-up and not measured.
    measure_from: usize,
    steps: Vec<Step>,
}

impl Scenario {
    /// The tick of the last gesture — idle is measured from here.
    fn last_event_tick(&self) -> usize {
        self.steps.iter().map(|s| s.tick).max().unwrap_or(0)
    }
}

const PROSE: &str = "The quick brown fox jumps over the lazy dog! And keeps going on ";

/// Ticks per key at 12 cps: exactly 10 at 120 Hz.
const KEY_TICKS: usize = 10;

/// §21: 12 cps prose for 3 s.
fn prose_12cps() -> Scenario {
    let ticks = ticks_in_ms(3_000);
    let steps = PROSE
        .chars()
        .cycle()
        .enumerate()
        .map(|(k, ch)| Step {
            tick: k * KEY_TICKS,
            action: Action::Typed(ch),
        })
        .take_while(|s| s.tick < ticks)
        .collect();
    Scenario {
        name: "prose 12 cps, 3 s",
        start: (ROW, 4),
        ticks,
        measure_from: 0,
        steps,
    }
}

/// §21: a 12 cps burst with a same-row 40-cell nav meteor every 500 ms.
fn burst_with_nav_meteor() -> Scenario {
    let ticks = ticks_in_ms(3_000);
    let mut steps = Vec::new();
    let mut col: i32 = 20;
    let mut prose = PROSE.chars().cycle();
    let mut tick = 0usize;
    while tick < ticks {
        if tick.is_multiple_of(ticks_in_ms(500)) {
            // Ctrl-E-shaped hop of 40 cells, whichever way fits the row.
            let dcol = if col + 40 < 180 { 40 } else { -40 };
            col += dcol;
            steps.push(Step {
                tick,
                action: Action::Nav(dcol),
            });
        } else if tick.is_multiple_of(KEY_TICKS) {
            col += 1;
            steps.push(Step {
                tick,
                action: Action::Typed(prose.next().unwrap_or(' ')),
            });
        }
        tick += 1;
    }
    Scenario {
        name: "12 cps burst + 40-cell nav meteor / 500 ms",
        start: (ROW, 20),
        ticks,
        measure_from: 0,
        steps,
    }
}

/// §21: an 80-cell Ctrl-A / Ctrl-E ping-pong, a fresh flight every 200 ms, so
/// each lands (T = 120 ms) with its train still fading when the next retires it
/// (§6.9).
fn ctrl_e_ping_pong() -> Scenario {
    let ticks = ticks_in_ms(3_000);
    let every = ticks_in_ms(200);
    let steps = (0..ticks)
        .step_by(every)
        .enumerate()
        .map(|(k, tick)| Step {
            tick,
            action: Action::Nav(if k % 2 == 0 { -80 } else { 80 }),
        })
        .collect();
    Scenario {
        name: "80-cell ctrl-e ping-pong / 200 ms",
        start: (ROW, 100),
        ticks,
        measure_from: 0,
        steps,
    }
}

/// §21: a held Backspace at the macOS repeat rate (30/s) over a 60-cell line
/// laid at 12 cps first; only the hold is measured.
fn held_backspace() -> Scenario {
    let lay_keys = 60usize;
    let lay_ticks = lay_keys * KEY_TICKS;
    let hold_from = lay_ticks + ticks_in_ms(100);
    let repeat = ticks_in_ms(33);
    let mut steps: Vec<Step> = PROSE
        .chars()
        .cycle()
        .take(lay_keys)
        .enumerate()
        .map(|(k, ch)| Step {
            tick: k * KEY_TICKS,
            action: Action::Typed(ch),
        })
        .collect();
    for k in 0..(lay_keys - 2) {
        steps.push(Step {
            tick: hold_from + k * repeat,
            action: Action::Backspace,
        });
    }
    let ticks = steps.last().map_or(0, |s| s.tick) + 1;
    Scenario {
        name: "held backspace 30/s over a 60-cell line",
        start: (ROW, 4),
        ticks,
        measure_from: hold_from,
        steps,
    }
}

/// §27, Codex: 12 cps prose into the composer while the program streams an
/// answer. Phase A (~1.5 s): the inline viewport (from the row above the caret
/// to the bottom) slides down one row per streamed line every 120 ms and the
/// caret rides it from [`ROW`] to the penultimate row. Phase B: the composer is
/// pinned at the bottom and each streamed line archives rows `0..=caret−2` up
/// one row every 30 ms; the caret stands still.
fn codex_streaming_12cps() -> Scenario {
    let ticks = ticks_in_ms(3_000);
    let bottom = geometry().rows as u16 - 1;
    let line_a = ticks_in_ms(120);
    let line_b = ticks_in_ms(30);
    let phase_b_from = ticks_in_ms(1_700);
    let mut steps = Vec::new();
    let mut row = ROW;
    let mut prose = PROSE.chars().cycle();
    for tick in 0..ticks {
        // The host replays the batch before the tick's typed echo is judged,
        // so a band move on a key's tick is pushed first.
        if tick > 0 && tick < phase_b_from && tick.is_multiple_of(line_a) && row + 1 < bottom {
            steps.push(Step {
                tick,
                action: Action::BandMove(row - 1, bottom, 1),
            });
            row += 1;
        } else if tick >= phase_b_from && tick.is_multiple_of(line_b) && row >= 2 {
            steps.push(Step {
                tick,
                action: Action::BandMove(0, row - 2, -1),
            });
        }
        if tick.is_multiple_of(KEY_TICKS) {
            steps.push(Step {
                tick,
                action: Action::Typed(prose.next().unwrap_or(' ')),
            });
        }
    }
    Scenario {
        name: "codex streaming: 12 cps under a sliding then pinned viewport",
        start: (ROW, 4),
        ticks,
        measure_from: 0,
        steps,
    }
}

/// §27: a held key at key-repeat cadence (which arms the sing-along) with a bar
/// on every bar boundary, the ring on every fourth, the fan widening as
/// `kitty_sing::bar_fan` does. The busiest frame a party lands on: the held key
/// has filled the sky toward its cap, so the bar's `Sow` is staged beside a
/// full shed.
fn sing_along_bars() -> Scenario {
    let ticks = ticks_in_ms(3_000);
    // A 30 ms auto-repeat, and a bar every 1.6 s (4 beats at SING_BPM).
    let repeat_ticks = ticks_in_ms(30).max(1);
    let bar_ticks = ticks_in_ms(1_600).max(1);
    let mut steps: Vec<Step> = (0..)
        .map(|k| Step {
            tick: k * repeat_ticks,
            action: Action::Typed('a'),
        })
        .take_while(|s| s.tick < ticks)
        .collect();
    for bar in 0u64.. {
        let tick = (bar as usize + 1) * bar_ticks;
        if tick >= ticks {
            break;
        }
        let fan = aterm_effects::kitty_sing::bar_fan(bar);
        steps.push(Step {
            tick,
            action: Action::Party(fan.n, fan.ring),
        });
    }
    steps.sort_by_key(|s| s.tick);
    Scenario {
        name: "sing-along bars on a held key, 3 s",
        start: (ROW, 4),
        ticks,
        measure_from: 0,
        steps,
    }
}

pub(crate) fn scenarios() -> Vec<Scenario> {
    let only = std::env::var("RK_FRAME_COST_ONLY").unwrap_or_default();
    let needle = only.split(':').nth(1).unwrap_or("").to_string();
    vec![
        prose_12cps(),
        burst_with_nav_meteor(),
        ctrl_e_ping_pong(),
        held_backspace(),
        codex_streaming_12cps(),
        sing_along_bars(),
    ]
    .into_iter()
    .filter(|sc| needle.is_empty() || sc.name.contains(&needle))
    .collect()
}

/// `RK_FRAME_COST_ONLY="<engines>:<scenario substring>"` narrows a run to one
/// cell of the table for a profiler (`engines` a `+`-joined subset of `v2`, the
/// engine alone, and `v2s`, through `CursorGlow`'s seam);
/// `RK_FRAME_COST_REPEAT=n` repeats each measurement so a sampler can attach.
/// Neither changes what is asserted (`RK_FRAME_COST_BUDGET_SCALE` does; see
/// `common::budget_scale`).
pub(crate) fn engine_selected(tag: &str) -> bool {
    let only = std::env::var("RK_FRAME_COST_ONLY").unwrap_or_default();
    let engines = only.split(':').next().unwrap_or("");
    engines.is_empty() || engines.split('+').any(|e| e == tag)
}

fn repeat() -> usize {
    std::env::var("RK_FRAME_COST_REPEAT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1)
}

// The drivers: v2's Engine alone, and v2 behind CursorGlow's seam.

/// What one tick wrote.
#[derive(Clone, Copy, Debug, Default)]
struct Wrote {
    under: usize,
    out: usize,
    halos: usize,
    fp: u64,
}

impl Wrote {
    fn quads(self) -> usize {
        self.under + self.out
    }
}

/// The idle contract, read after a tick.
#[derive(Clone, Copy, Debug, Default)]
struct IdleProbe {
    stars: u32,
    cells: u32,
    meteors: u32,
    fp: u64,
    cadence: bool,
    deadline: bool,
}

impl IdleProbe {
    fn at_zero(self) -> bool {
        self.fp == 0 && !self.cadence && !self.deadline
    }
}

trait Driver {
    fn apply(&mut self, action: Action, now: Instant, caret: &mut (u16, u16));
    fn tick(&mut self, now: Instant, caret: (u16, u16)) -> Wrote;
    fn idle_probe(&self, now: Instant) -> IdleProbe;
    /// The streams the last tick wrote: `(under, out, halos)`.
    fn streams(&self) -> (&[GlowQuad], &[GlowQuad], &[RainHalo]);
    /// Rows handed to the content witness over this driver's life, the
    /// non-vacuity check for the seam. Zero for a driver with no host seam.
    fn witness_feeds(&self) -> u64 {
        0
    }
    /// The warm-up is over: the passes from here on are the measured ones.
    fn begin_measured_pass(&mut self) {}
    /// Frames on which the driver withheld a row the witness asked for,
    /// over its life — the non-vacuity witness for the withheld path.
    fn withheld_frames(&self) -> u64 {
        0
    }
}

/// An FNV-1a fold over every emitted field, timed separately in `run` to price
/// a per-frame content hash. The engine's own `fingerprint` is liveness only.
fn replica_fingerprint(under: &[GlowQuad], out: &[GlowQuad], halos: &[RainHalo]) -> u64 {
    if under.is_empty() && out.is_empty() && halos.is_empty() {
        return 0;
    }
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(PRIME);
    };
    for q in under.iter().chain(out.iter()) {
        mix(u64::from(q.row));
        mix(u64::from(q.x) << 16 | u64::from(q.y));
        mix(u64::from(q.w) << 16 | u64::from(q.h));
        mix(u64::from(q.color) << 8 | u64::from(q.alpha));
    }
    for a in halos {
        mix(u64::from(a.row));
        mix(u64::from(a.x) << 16 | u64::from(a.y));
        mix(u64::from(a.w) << 16 | u64::from(a.h));
        mix(u64::from(a.cx) << 16 | u64::from(a.cy));
        mix(u64::from(a.rx) << 16 | u64::from(a.ry));
        mix(u64::from(a.color));
    }
    h | 1
}

/// The v2 engine plus the host scratch it borrows (`Frame` is borrowed host
/// scratch by contract, which makes the zero-alloc claim testable).
struct V2 {
    eng: Engine,
    cfg: Config,
    geom: Geom,
    /// A blank probe row minted once: the host re-probes the caret's rows after
    /// every band move (the band path drops the sky's probe) without allocating.
    blank: Vec<bool>,
    under: Vec<GlowQuad>,
    out: Vec<GlowQuad>,
    halos: Vec<RainHalo>,
    beams: Vec<BeamVertex>,
    cues: Vec<SoundCue>,
}

impl V2 {
    fn new() -> Self {
        let geom = geometry();
        let mut eng = Engine::new();
        eng.set_engaged(true);
        // §5.4: no sky star is born over an unprobed row. Blank rows either side
        // of the typing row make the sky the worst case, as a host probing the
        // caret row and both neighbours reports.
        let blank = vec![false; geom.cols];
        for r in [ROW - 1, ROW, ROW + 1] {
            eng.probe_mut().probe_row(i32::from(r), &blank);
        }
        // `Frame`'s streams are host scratch (§17.1, §18) held at the stream
        // caps; reserving them here makes any allocation a producer's.
        Self {
            eng,
            cfg: Config::from_glow(&glow_config(), false),
            geom,
            blank,
            under: Vec::with_capacity(MAX_QUADS),
            out: Vec::with_capacity(MAX_QUADS),
            halos: Vec::with_capacity(MAX_HALOS),
            beams: Vec::with_capacity(HOST_BEAM_SCRATCH),
            cues: Vec::with_capacity(HOST_CUE_SCRATCH),
        }
    }
}

fn class_of(ch: char) -> TypedClass {
    if ch == ' ' {
        TypedClass::Space
    } else if ch == '!' {
        TypedClass::Bang
    } else if ch.is_uppercase() {
        TypedClass::Capital
    } else {
        TypedClass::Glyph
    }
}

fn mv(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
    Event::Move {
        from,
        to,
        licence,
        dir: Dir::of(
            i32::from(to.1) - i32::from(from.1),
            i32::from(to.0) - i32::from(from.0),
        ),
    }
}

impl Driver for V2 {
    fn apply(&mut self, action: Action, now: Instant, caret: &mut (u16, u16)) {
        let from = *caret;
        match action {
            Action::Typed(ch) => {
                caret.1 += 1;
                self.eng.on_event(mv(from, *caret, Licence::Typed), now);
                self.eng.on_event(
                    Event::Typed {
                        cells: 1,
                        shifted: ch.is_uppercase() || ch == '!',
                        class: class_of(ch),
                    },
                    now,
                );
            }
            Action::Nav(dcol) => {
                caret.1 = (i32::from(caret.1) + dcol).clamp(0, i32::from(u16::MAX)) as u16;
                self.eng.on_event(mv(from, *caret, Licence::Nav), now);
            }
            Action::Party(n, ring) => self.eng.party(now, n, ring),
            Action::Backspace => {
                caret.1 = caret.1.saturating_sub(1);
                self.eng.on_event(mv(from, *caret, Licence::Typed), now);
                self.eng.on_event(Event::Erase, now);
            }
            Action::BandMove(top, bottom, delta) => {
                self.eng.translate_band(
                    top,
                    bottom,
                    delta,
                    self.geom.ch as u16,
                    self.geom.origin_y,
                );
                caret.0 = band_pos(caret.0, top, bottom, delta);
                // The host re-probes the caret's rows before the next deal.
                for r in [caret.0.saturating_sub(1), caret.0, caret.0 + 1] {
                    self.eng.probe_mut().probe_row(i32::from(r), &self.blank);
                }
            }
        }
    }

    fn tick(&mut self, now: Instant, _caret: (u16, u16)) -> Wrote {
        // The host clears its scratch every frame (`CursorGlow::tick` does
        // `out.clear()`); the engine appends.
        self.under.clear();
        self.out.clear();
        self.halos.clear();
        self.cues.clear();
        let mut fr = Frame {
            under: &mut self.under,
            out: &mut self.out,
            halos: &mut self.halos,
            beams: &mut self.beams,
            cues: &mut self.cues,
            caret: CaretSeam::default(),
            companion: None,
            fp: 0,
        };
        self.eng.tick(now, self.geom, &self.cfg, &mut fr);
        let fp = fr.fp;
        // A host takes the impulse every frame; leaving it pending would be
        // a different steady state from the shipping one.
        let _ = self.eng.take_companion_impulse();
        Wrote {
            under: self.under.len(),
            out: self.out.len(),
            halos: self.halos.len(),
            fp,
        }
    }

    fn idle_probe(&self, now: Instant) -> IdleProbe {
        let st = self.eng.status();
        IdleProbe {
            stars: st.stars,
            cells: st.cells,
            meteors: st.meteors,
            fp: self.eng.fingerprint(),
            cadence: self.eng.needs_frame_cadence(),
            deadline: self.eng.next_change_deadline(now).is_some(),
        }
    }

    fn streams(&self) -> (&[GlowQuad], &[GlowQuad], &[RainHalo]) {
        (&self.under, &self.out, &self.halos)
    }
}

/// `CursorGlow` in the `RainbowKitty` style, v2 through the real seam in `CursorGlow::tick`: the
/// cost of a typed key to the host. This harness supplies no glyph probes, so the seam run has
/// no sky stars; its cost is a floor, not a substitute for the `V2` driver.
struct Host {
    glow: CursorGlow,
    cfg: GlowConfig,
    geom: Geom,
    quads: Vec<GlowQuad>,
    last_fp: u64,
    /// The content witness's row as the frame hold hands it over, fed before
    /// each tick. Every column holds a glyph that never moves: the witness's
    /// worst case (every ribbon cell arms a record, walked each frame) with
    /// nothing retiring (`tests/abandoned_ribbon.rs` covers retirement).
    witness_row: Vec<char>,
    witness_feeds: u64,
    /// After warm-up, every [`WITHHOLD_EVERY`]th frame withholds one asked-for
    /// row (the first that is not the caret's, as the composed host drops its
    /// second read), so the walk scans the records beside it
    /// (`Witness::find_deferred_runs`). Warm-up withholds nothing, so only
    /// scratch reserved up front keeps these frames from allocating.
    withhold: bool,
    frames: u64,
    withheld_frames: u64,
}

/// A measured pass withholds one asked-for row every this many frames.
const WITHHOLD_EVERY: u64 = 8;

impl Host {
    fn seam() -> Self {
        Self {
            glow: CursorGlow::default(),
            cfg: glow_config(),
            geom: geometry(),
            quads: Vec::with_capacity(MAX_QUADS),
            last_fp: 0,
            witness_row: vec!['x'; geometry().cols],
            witness_feeds: 0,
            withhold: false,
            frames: 0,
            withheld_frames: 0,
        }
    }
}

impl Driver for Host {
    fn apply(&mut self, action: Action, now: Instant, caret: &mut (u16, u16)) {
        match action {
            Action::Typed(_) => {
                caret.1 += 1;
                self.glow.note_synthetic_typed(now, 1);
            }
            Action::Nav(dcol) => {
                caret.1 = (i32::from(caret.1) + dcol).clamp(0, i32::from(u16::MAX)) as u16;
                self.glow.note_synthetic_move(now);
            }
            Action::Backspace => {
                caret.1 = caret.1.saturating_sub(1);
                self.glow.note_backspace(now);
            }
            Action::BandMove(top, bottom, delta) => {
                self.glow.note_band_move(top, bottom, delta);
                caret.0 = band_pos(caret.0, top, bottom, delta);
            }
            // The seam run does not replay the bar; the `V2` driver prices it.
            Action::Party(..) => {}
        }
    }

    fn tick(&mut self, now: Instant, caret: (u16, u16)) -> Wrote {
        // The frame hold's witness feed in order: the caret row rides the probe
        // the host holds, then the rows the ribbon names. The engine consumes
        // them inside the tick below.
        {
            self.frames += 1;
            let withhold = self.withhold && self.frames.is_multiple_of(WITHHOLD_EVERY);
            let Self {
                glow, witness_row, ..
            } = self;
            glow.observe_ribbon_row(caret.0, witness_row);
            let mut rows = [0u16; WITNESS_ROWS];
            let n = glow.ribbon_rows(&mut rows);
            let mut fed = 1u64;
            let mut withheld = None;
            for &r in &rows[..n] {
                if r != caret.0 && usize::from(r) < geometry().rows {
                    if withhold && withheld.is_none() {
                        withheld = Some(r);
                        continue;
                    }
                    glow.observe_ribbon_row(r, witness_row);
                    fed += 1;
                }
            }
            self.witness_feeds += fed;
            self.withheld_frames += u64::from(withheld.is_some());
        }
        let fp = self
            .glow
            .tick(Some(caret), now, &self.cfg, self.geom, &mut self.quads);
        self.last_fp = fp;
        Wrote {
            under: self.glow.under_quads().len(),
            out: self.quads.len(),
            halos: self.glow.halos().len(),
            fp,
        }
    }

    fn idle_probe(&self, now: Instant) -> IdleProbe {
        let st = self.glow.v2_status().unwrap_or_default();
        IdleProbe {
            stars: st.stars,
            cells: st.cells,
            meteors: st.meteors,
            fp: self.last_fp,
            cadence: self.glow.needs_frame_cadence(),
            deadline: self.glow.next_change_deadline(now, period()).is_some(),
        }
    }

    fn streams(&self) -> (&[GlowQuad], &[GlowQuad], &[RainHalo]) {
        (self.glow.under_quads(), &self.quads, self.glow.halos())
    }

    fn witness_feeds(&self) -> u64 {
        self.witness_feeds
    }

    fn begin_measured_pass(&mut self) {
        self.withhold = true;
    }

    fn withheld_frames(&self) -> u64 {
        self.withheld_frames
    }
}

// The run.

/// Idle run-out ceiling after the last gesture; every v2 life is shorter
/// (meteor `T + FLIGHT_OFF_GLASS_MS`, star ≤ 460 ms, the ribbon swoosh
/// `SWOOSH_TOTAL_S`).
const IDLE_CEILING_MS: u64 = 2_500;

/// The §18 stream caps, restated because a `tests/` target cannot see the
/// private `CursorGlow::MAX_QUADS` / `MAX_HALOS`.
const MAX_QUADS: usize = 16_384;
const MAX_HALOS: usize = 512;
/// The host's polyline scratch: the ring's 49 vertices or the hot edge's
/// handful, never more than a few hundred.
const HOST_BEAM_SCRATCH: usize = 512;
/// The host's cue sink: a frame mints a handful of cues at most.
const HOST_CUE_SCRATCH: usize = 64;

/// When (ms after the last gesture) each pool and each idle law hit zero.
#[derive(Clone, Copy, Debug, Default)]
struct IdleTimes {
    stars: Option<u64>,
    cells: Option<u64>,
    meteors: Option<u64>,
    fp: Option<u64>,
    cadence: Option<u64>,
    deadline: Option<u64>,
    /// When all three idle laws first held at once.
    zero: Option<u64>,
    /// The scratch was empty on the tick after `zero`.
    wrote_nothing_after: bool,
}

/// What one measured pass produced.
#[derive(Clone, Debug, Default)]
pub(crate) struct Report {
    us: Vec<u64>,
    /// The replica fingerprint fold's own µs per measured tick.
    fp_us: Vec<u64>,
    /// Device pixels covered by the `out` (over-ink) stream per measured tick,
    /// which the host's §3.4 ledger prices per pixel.
    out_px: Vec<u64>,
    quads_max: usize,
    under_max: usize,
    out_max: usize,
    halos_max: usize,
    quads_sum: usize,
    live_ticks: usize,
    /// Allocations over the measured ticks, and the ticks that allocated:
    /// `(tick, count, sizes, pools at that tick)`.
    allocs: usize,
    alloc_ticks: Vec<(usize, usize, [usize; SIZE_SLOTS], IdleProbe)>,
    idle_allocs: usize,
    idle: IdleTimes,
    /// Allocations per warm-up pass, cold pass first: one-time pool growth.
    warmup_allocs: Vec<usize>,
    /// Rows the driver handed the content witness over the pass.
    witness_feeds: u64,
    /// Frames on which the driver withheld an asked-for row.
    withheld_frames: u64,
}

impl Report {
    fn pct(&self, p: usize) -> u64 {
        let n = self.us.len();
        if n == 0 {
            return 0;
        }
        self.us[(n * p / 100).min(n - 1)]
    }
    pub(crate) fn p50(&self) -> u64 {
        self.pct(50)
    }
    pub(crate) fn p90(&self) -> u64 {
        self.pct(90)
    }
    fn max(&self) -> u64 {
        self.us.last().copied().unwrap_or(0)
    }
    fn quads_mean(&self) -> usize {
        self.quads_sum / self.us.len().max(1)
    }
    fn fp_p50(&self) -> u64 {
        let n = self.fp_us.len();
        if n == 0 { 0 } else { self.fp_us[n / 2] }
    }
    fn out_px_max(&self) -> u64 {
        self.out_px.last().copied().unwrap_or(0)
    }
}

/// One pass of the script through `d` from `t0`. `measured` turns on timers and
/// the allocation counter. Returns the report and the instant the pass ended
/// (idle included).
fn run(d: &mut dyn Driver, sc: &Scenario, t0: Instant, measured: bool) -> (Report, Instant) {
    let mut rep = Report::default();
    let n_measured = sc.ticks.saturating_sub(sc.measure_from);
    rep.us.reserve(n_measured);
    rep.alloc_ticks.reserve(SIZE_SLOTS);
    let mut caret = sc.start;
    let mut next_step = 0usize;
    let mut now = t0;
    for tick in 0..sc.ticks {
        now = t0 + period() * tick as u32;
        let measure = measured && tick >= sc.measure_from;
        let steps = &sc.steps;
        let body = |d: &mut dyn Driver, caret: &mut (u16, u16), next_step: &mut usize| {
            while let Some(s) = steps.get(*next_step)
                && s.tick == tick
            {
                d.apply(s.action, now, caret);
                *next_step += 1;
            }
            d.tick(now, *caret)
        };
        let wrote = if measure {
            let start = Instant::now();
            let (wrote, allocs, sizes) = allocations_during(|| body(d, &mut caret, &mut next_step));
            rep.us.push(start.elapsed().as_micros() as u64);
            let (under, out, halos) = d.streams();
            let start = Instant::now();
            let fp = replica_fingerprint(under, out, halos);
            rep.fp_us.push(start.elapsed().as_micros() as u64);
            rep.out_px
                .push(out.iter().map(|q| u64::from(q.w) * u64::from(q.h)).sum());
            assert!(
                (fp == 0) == (wrote.quads() == 0 && wrote.halos == 0),
                "replica fold disagrees with the streams"
            );
            if allocs > 0 {
                rep.allocs += allocs;
                if rep.alloc_ticks.len() < SIZE_SLOTS {
                    rep.alloc_ticks
                        .push((tick, allocs, sizes, d.idle_probe(now)));
                }
            }
            wrote
        } else {
            body(d, &mut caret, &mut next_step)
        };
        if measure {
            rep.quads_max = rep.quads_max.max(wrote.quads());
            rep.under_max = rep.under_max.max(wrote.under);
            rep.out_max = rep.out_max.max(wrote.out);
            rep.halos_max = rep.halos_max.max(wrote.halos);
            rep.quads_sum += wrote.quads();
            if wrote.fp != 0 {
                rep.live_ticks += 1;
            }
            assert!(
                wrote.quads() <= MAX_QUADS,
                "{}: tick {tick} wrote {} quads > MAX_QUADS {MAX_QUADS}",
                sc.name,
                wrote.quads()
            );
            assert!(
                wrote.halos <= MAX_HALOS,
                "{}: tick {tick} wrote {} halos > MAX_HALOS {MAX_HALOS}",
                sc.name,
                wrote.halos
            );
        }
    }
    // Idle run-out: keep ticking with no gestures until every law holds.
    let last = t0 + period() * sc.last_event_tick() as u32;
    let idle_ticks = ticks_in_ms(IDLE_CEILING_MS);
    let mut zero_seen = false;
    for k in 1..=idle_ticks {
        now = t0 + period() * (sc.ticks - 1 + k) as u32;
        let ms = now.saturating_duration_since(last).as_millis() as u64;
        let wrote = if measured {
            let (wrote, allocs, sizes) = allocations_during(|| d.tick(now, caret));
            if allocs > 0 {
                rep.idle_allocs += allocs;
                if rep.alloc_ticks.len() < SIZE_SLOTS {
                    rep.alloc_ticks
                        .push((sc.ticks - 1 + k, allocs, sizes, d.idle_probe(now)));
                }
            }
            wrote
        } else {
            d.tick(now, caret)
        };
        if zero_seen {
            rep.idle.wrote_nothing_after = wrote.quads() == 0 && wrote.halos == 0 && wrote.fp == 0;
            break;
        }
        let p = d.idle_probe(now);
        let idle = &mut rep.idle;
        let first = |slot: &mut Option<u64>, hit: bool| {
            if hit && slot.is_none() {
                *slot = Some(ms);
            }
        };
        first(&mut idle.stars, p.stars == 0);
        first(&mut idle.cells, p.cells == 0);
        first(&mut idle.meteors, p.meteors == 0);
        first(&mut idle.fp, p.fp == 0 && wrote.fp == 0);
        first(&mut idle.cadence, !p.cadence);
        first(&mut idle.deadline, !p.deadline);
        if p.at_zero() && wrote.fp == 0 {
            idle.zero = Some(ms);
            zero_seen = true;
        }
    }
    rep.us.sort_unstable();
    rep.fp_us.sort_unstable();
    rep.out_px.sort_unstable();
    rep.witness_feeds = d.witness_feeds();
    rep.withheld_frames = d.withheld_frames();
    (rep, now)
}

/// Warm-up passes before the measured one, at most. The cold pass grows every
/// demand-grown pool and a second can still grow the ribbon, so the gate
/// measures after a pass that allocated nothing: a failure is a per-frame
/// allocation, not one-time growth.
const WARMUP_PASSES_MAX: usize = 3;

/// Warm the driver until a whole pass (script + idle run-out) allocates
/// nothing, then measure the next pass of the same script.
fn measure(d: &mut dyn Driver, sc: &Scenario) -> Report {
    let mut t = Instant::now();
    let mut warmup = Vec::with_capacity(WARMUP_PASSES_MAX);
    for _ in 0..WARMUP_PASSES_MAX {
        let (rep, end) = run(d, sc, t, true);
        let grew = rep.allocs + rep.idle_allocs;
        warmup.push(grew);
        t = end + Duration::from_secs(1);
        if grew == 0 {
            break;
        }
    }
    let mut rep = Report::default();
    d.begin_measured_pass();
    for _ in 0..repeat() {
        let (r, end) = run(d, sc, t, true);
        rep = r;
        t = end + Duration::from_secs(1);
    }
    rep.warmup_allocs = warmup;
    rep
}

/// One table cell: `"v2"` (the engine alone) or `"v2s"` (through the seam)
/// driven through `sc`, the driver built on this thread so its warm-up and
/// measured pass share one allocator tally.
pub(crate) fn measure_cell(tag: &str, sc: &Scenario) -> Report {
    match tag {
        "v2" => measure(&mut V2::new(), sc),
        _ => measure(&mut Host::seam(), sc),
    }
}

fn fmt_ms(t: Option<u64>) -> String {
    t.map_or_else(|| "NEVER".to_string(), |ms| format!("{ms} ms"))
}

pub(crate) fn print_row(engine: &str, sc: &Scenario, r: &Report) {
    println!(
        "| {:<44} | {engine} | {:>5} | {:>5} | {:>6} | {:>4} | {:>6} ({:>5} mean; under {:>5} / out {:>4}, out px {:>6}) | {:>3} | {:>3} | {:?} |",
        sc.name,
        r.p50(),
        r.p90(),
        r.max(),
        r.fp_p50(),
        r.quads_max,
        r.quads_mean(),
        r.under_max,
        r.out_max,
        r.out_px_max(),
        r.halos_max,
        r.allocs + r.idle_allocs,
        r.warmup_allocs,
    );
}

pub(crate) fn print_header(mode: &str) {
    println!();
    println!(
        "rainbow kitty v2 frame cost — {mode} — {HZ} Hz tick, geometry 18×40 px, 190×26 cells"
    );
    println!(
        "| {:<44} | eng | p50µs | p90µs |  maxµs | fpµs | quads max (mean; under / out, out px max)                  | hal | all | warm-up allocs / pass |",
        "scenario"
    );
}

pub(crate) fn print_idle(sc: &Scenario, r: &Report) {
    let i = &r.idle;
    println!(
        "  idle after last key [{}]: stars→0 {}, cells→0 {}, meteors→0 {}, fp→0 {}, cadence→false {}, deadline→None {}; ALL ZERO at {}; tick after wrote nothing: {}",
        sc.name,
        fmt_ms(i.stars),
        fmt_ms(i.cells),
        fmt_ms(i.meteors),
        fmt_ms(i.fp),
        fmt_ms(i.cadence),
        fmt_ms(i.deadline),
        fmt_ms(i.zero),
        i.wrote_nothing_after,
    );
}

fn print_allocs(sc: &Scenario, r: &Report) {
    for (tick, n, sizes, pools) in &r.alloc_ticks {
        let sizes: Vec<String> = sizes
            .iter()
            .take(*n)
            .filter(|s| **s > 0)
            .map(|s| s.to_string())
            .collect();
        println!(
            "  ALLOC [{}] measured tick {tick}: {n} allocation(s), sizes [{}] — pools: cells {}, stars {}, meteors {}",
            sc.name,
            sizes.join(", "),
            pools.cells,
            pools.stars,
            pools.meteors,
        );
    }
}

// The gates.

/// §18's deterministic laws, every run: zero allocation on every steady and
/// idle frame, idle → exactly zero within [`IDLE_CEILING_MS`], the stream caps,
/// and a live fixture. Timing printed here is informational.
#[test]
fn rainbow_kitty_v2_steady_frames_allocate_nothing_and_idle_to_exactly_zero() {
    assert_counter_is_per_thread();
    let mode = if cfg!(debug_assertions) {
        "DEBUG build (timing informational)"
    } else {
        "release build"
    };
    print_header(mode);
    // The cells are independent replays on a scripted clock and the tally is
    // per thread, so they run on the pool; rows print and laws are judged in
    // scenario order afterwards.
    let scs = scenarios();
    let cells: Vec<(usize, &str)> = (0..scs.len())
        .flat_map(|i| ["v2", "v2s"].map(|tag| (i, tag)))
        .filter(|&(_, tag)| engine_selected(tag))
        .collect();
    let measured = par_map(&cells, |&(i, tag)| measure_cell(tag, &scs[i]));
    let mut reports: Vec<(Report, Report)> = scs.iter().map(|_| Default::default()).collect();
    for (&(i, tag), report) in cells.iter().zip(measured) {
        match tag {
            "v2" => reports[i].0 = report,
            _ => reports[i].1 = report,
        }
    }
    let mut reds: Vec<String> = Vec::new();
    for (sc, (r2, rs)) in scs.iter().zip(&reports) {
        if engine_selected("v2") {
            print_row("v2 ", sc, r2);
        }
        if engine_selected("v2s") {
            print_row("v2s", sc, rs);
        }
        if engine_selected("v2") {
            print_idle(sc, r2);
        }
        print_allocs(sc, r2);
        if !engine_selected("v2") {
            continue;
        }

        if r2.live_ticks == 0 {
            reds.push(format!(
                "[{}] v2 fixture never drew (fp == 0 on every measured tick)",
                sc.name
            ));
        }
        if r2.allocs > 0 {
            reds.push(format!(
                "[{}] v2 allocated {} time(s) across the measured steady ticks (first ticks: {:?})",
                sc.name,
                r2.allocs,
                r2.alloc_ticks
                    .iter()
                    .map(|(t, n, _, _)| (*t, *n))
                    .collect::<Vec<_>>()
            ));
        }
        if r2.idle_allocs > 0 {
            reds.push(format!(
                "[{}] v2 allocated {} time(s) while idling out",
                sc.name, r2.idle_allocs
            ));
        }
        if engine_selected("v2s") && rs.witness_feeds == 0 {
            reds.push(format!(
                "[{}] the seam fed the CONTENT WITNESS nothing: this gate prices \
                 none of `observe_ribbon_row` / `Engine::witness_rows`",
                sc.name
            ));
        }
        if engine_selected("v2s") && rs.withheld_frames == 0 {
            reds.push(format!(
                "[{}] the seam withheld no row the witness asked for: this gate \
                 prices no frame of `Witness::find_deferred_runs`",
                sc.name
            ));
        }
        if rs.allocs + rs.idle_allocs > 0 {
            reds.push(format!(
                "[{}] v2 THROUGH THE SEAM allocated {} time(s) on steady ticks (first: {:?})",
                sc.name,
                rs.allocs + rs.idle_allocs,
                rs.alloc_ticks
                    .iter()
                    .map(|(t, n, sizes, _)| (*t, *n, sizes[0]))
                    .collect::<Vec<_>>()
            ));
        }
        if r2.warmup_allocs.last().copied().unwrap_or(1) != 0 {
            reds.push(format!(
                "[{}] v2 never reached a pass with zero allocations in {WARMUP_PASSES_MAX} warm-up passes: {:?}",
                sc.name, r2.warmup_allocs
            ));
        }
        match r2.idle.zero {
            Some(ms) if ms <= IDLE_CEILING_MS => {}
            Some(ms) => reds.push(format!("[{}] v2 reached idle-zero only at {ms} ms after the last key", sc.name)),
            None => reds.push(format!(
                "[{}] v2 never reached idle-zero within {IDLE_CEILING_MS} ms (stars {}, cells {}, meteors {}, fp {}, cadence {}, deadline {})",
                sc.name,
                fmt_ms(r2.idle.stars),
                fmt_ms(r2.idle.cells),
                fmt_ms(r2.idle.meteors),
                fmt_ms(r2.idle.fp),
                fmt_ms(r2.idle.cadence),
                fmt_ms(r2.idle.deadline),
            )),
        }
        if r2.idle.zero.is_some() && !r2.idle.wrote_nothing_after {
            reds.push(format!(
                "[{}] v2 wrote geometry on the tick after idle-zero",
                sc.name
            ));
        }
    }
    assert!(
        reds.is_empty(),
        "rainbow kitty v2 frame-cost laws RED:\n  {}",
        reds.join("\n  ")
    );
}
