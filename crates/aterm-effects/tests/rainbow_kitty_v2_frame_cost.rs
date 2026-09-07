// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow Kitty v2 — the FRAME-COST GATE (design of record §18, §21).
//!
//! The owner's standing order (2026-09-05): "make sure that the typing and
//! effects continue to be extremely responsive and fast and efficient."
//! §18 states the budget: **p50 ≤ 0.4 ms / p90 ≤ 0.6 ms producer CPU per
//! tick at 120 Hz including the ribbon, zero allocation per steady frame,
//! idle → zero**. (Until §17.3 phase 7 this file also raced v1 through the
//! same harness and held v2 to "never slower"; v1 is deleted, so the budget
//! is now absolute — the numbers §18 records are the bar.)
//!
//! This file is a GATE, not a report. It drives the v2 [`Engine`] through the
//! §21 gestures — 12 cps prose, a burst with a same-row 40-cell nav meteor
//! every 500 ms, an 80-cell Ctrl-E ping-pong, and a held Backspace — at a
//! 120 Hz tick, and drives `CursorGlow` (style `RainbowKitty`, i.e. v2
//! through the real seam) through the SAME script in the same harness, then
//! asserts:
//!
//! * **zero allocation** on every steady-state tick after warm-up, through a
//!   counting global allocator (the one precedent is
//!   `tests/word_reflow_identity.rs`), for every scenario and for the idle
//!   run-out after it;
//! * **idle → zero**: after the last key, `next_change_deadline == None`,
//!   `needs_frame_cadence == false`, `fingerprint == 0` and a tick writes
//!   nothing — and it reports the exact ms after the last key at which each
//!   pool (stars, ribbon cells, meteors) and each idle law reached zero;
//! * the **stream caps** (`MAX_QUADS 16 384`, `MAX_HALOS 512`) on every tick;
//! * in the `--release --ignored` twin, the §18 **CPU budget** on every
//!   scenario (p50 and p90), for the engine alone and through the seam.
//!
//! ```sh
//! # the deterministic laws (zero-alloc, idle → zero, caps) — every run:
//! targo --unverified test -p aterm-effects --test rainbow_kitty_v2_frame_cost -- --nocapture
//! # the CPU budget gate — release only, like tests/cursor_bench.rs:
//! targo --unverified test -p aterm-effects --release --test rainbow_kitty_v2_frame_cost \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The allocator count is process-wide, so the two tests serialize on one
//! mutex and only count while a driver call is on the stack; run with
//! `--test-threads=1` when combining `--include-ignored`.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle, SoundCue};
use aterm_effects::rainbow_kitty::{
    CaretSeam, Config, Dir, Engine, Event, Frame, Licence, TypedClass,
};
use aterm_render::{BeamVertex, GlowQuad, RainHalo};

// ===========================================================================
// The counting allocator
// ===========================================================================

/// Counts every allocation, reallocation and zeroed allocation while
/// [`COUNTING`] is set. The first [`SIZE_SLOTS`] sizes are kept so a red
/// names the shape of what allocated, not just that something did.
struct CountingAllocator;

const SIZE_SLOTS: usize = 16;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATION_SIZES: [AtomicUsize; SIZE_SLOTS] = [const { AtomicUsize::new(0) }; SIZE_SLOTS];

fn record_allocation(size: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        let index = ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        if let Some(slot) = ALLOCATION_SIZES.get(index) {
            slot.store(size, Ordering::Relaxed);
        }
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            record_allocation(new_size);
        }
        new_ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` came from this delegating allocator.
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// The two tests share one process-wide counter; they take turns.
static SERIAL: Mutex<()> = Mutex::new(());

/// Count the allocations `f` makes. Returns the count and the first sizes.
fn allocations_during<T>(f: impl FnOnce() -> T) -> (T, usize, [usize; SIZE_SLOTS]) {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    for slot in &ALLOCATION_SIZES {
        slot.store(0, Ordering::Relaxed);
    }
    COUNTING.store(true, Ordering::Release);
    let out = f();
    COUNTING.store(false, Ordering::Release);
    (
        out,
        ALLOCATIONS.load(Ordering::Relaxed),
        std::array::from_fn(|index| ALLOCATION_SIZES[index].load(Ordering::Relaxed)),
    )
}

// ===========================================================================
// The fixture — one geometry, one config, both engines
// ===========================================================================

/// The frame clock: 120 Hz.
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
/// metrics") — the same geometry §18's "measured today" ribbon numbers were
/// taken at, so the rows compare like for like across releases.
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
        radius: 0.4,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
        wake_persist_s: aterm_effects::cursor_glow::RAINBOW_WAKE_PERSIST,
    }
}

/// The row every scenario types on.
const ROW: u16 = 12;

// ===========================================================================
// The script — one vocabulary, replayed to both engines
// ===========================================================================

/// One host gesture.
#[derive(Clone, Copy, Debug)]
enum Action {
    /// One typed glyph echoed forward one cell.
    Typed(char),
    /// A keyed same-row navigation of `dcol` cells (Ctrl-A/E, word motions).
    Nav(i32),
    /// One Backspace: the caret retreats one cell.
    Backspace,
}

/// A gesture on a tick.
#[derive(Clone, Copy, Debug)]
struct Step {
    tick: usize,
    action: Action,
}

/// One §21 scenario.
struct Scenario {
    name: &'static str,
    /// Where the caret starts.
    start: (u16, u16),
    /// Total ticks the script runs.
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

/// §21: an 80-cell Ctrl-A / Ctrl-E ping-pong — a fresh flight every 200 ms,
/// so each one lands (T = 120 ms) with its train still fading when the next
/// retires it (§6.9's retire-previous).
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

/// §21: a held Backspace at the macOS repeat rate (30 / s) over a 60-cell
/// line laid at 12 cps first; only the hold is measured.
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

fn scenarios() -> Vec<Scenario> {
    let only = std::env::var("RK_FRAME_COST_ONLY").unwrap_or_default();
    let needle = only.split(':').nth(1).unwrap_or("").to_string();
    vec![
        prose_12cps(),
        burst_with_nav_meteor(),
        ctrl_e_ping_pong(),
        held_backspace(),
    ]
    .into_iter()
    .filter(|sc| needle.is_empty() || sc.name.contains(&needle))
    .collect()
}

/// `RK_FRAME_COST_ONLY="<engines>:<scenario substring>"` narrows a run to
/// one cell of the table for a profiler — `engines` is a `+`-joined subset of
/// `v2` (the engine alone) and `v2s` (v2 through `CursorGlow`'s seam);
/// `RK_FRAME_COST_REPEAT=n` repeats each measurement so the process stays
/// alive long enough to sample. Neither changes what is asserted.
fn engine_selected(tag: &str) -> bool {
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

// ===========================================================================
// The drivers — v2's Engine alone, and v2 behind CursorGlow's seam
// ===========================================================================

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
}

/// A replica of `rainbow_kitty::fingerprint` (private to the engine): the
/// FNV-1a fold over every emitted field the engine runs on EVERY frame. Timed
/// separately in `run` so the fixer knows what share of a frame the fold is,
/// measured rather than estimated.
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
/// scratch by contract — reusing it is what makes §18's zero-alloc claim
/// testable rather than aspirational).
struct V2 {
    eng: Engine,
    cfg: Config,
    geom: Geom,
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
        // §5.4's hard gate: no sky star is born over a row the host has not
        // probed. Blank rows either side of the typing row, so the sky is
        // the worst case (every deal may be born), exactly as a host that
        // probes the caret's row and its two neighbours would report.
        let blank = vec![false; geom.cols];
        for r in [ROW - 1, ROW, ROW + 1] {
            eng.probe_mut().probe_row(i32::from(r), &blank);
        }
        // `Frame`'s streams are HOST scratch by contract (§17.1, §18): the
        // shipping host hands v2 the same resident `under` / `out` / `halos`
        // the other nine styles fill, held at the stream caps. Reserving them
        // here is what makes a red a PRODUCER allocation and not the
        // harness's own vector doubling on a new halo peak.
        Self {
            eng,
            cfg: Config::from_glow(&glow_config(), false),
            geom,
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
            Action::Backspace => {
                caret.1 = caret.1.saturating_sub(1);
                self.eng.on_event(mv(from, *caret, Licence::Typed), now);
                self.eng.on_event(Event::Erase, now);
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

/// `CursorGlow` in the `RainbowKitty` style, driven through the same
/// scripted seams tests/cursor_bench.rs uses: **v2 through the real seam**
/// (D14's twelve delegation points inside `CursorGlow::tick`), which is what
/// a typed key actually costs the host. Note the seam's glyph probe reads the host's
/// row probes, which this terminal-less harness never supplies, so the
/// seam run bears no sky stars — its cost is a floor for the sky's share,
/// not a substitute for the direct `V2` driver above.
struct Host {
    glow: CursorGlow,
    cfg: GlowConfig,
    geom: Geom,
    quads: Vec<GlowQuad>,
    last_fp: u64,
}

impl Host {
    fn seam() -> Self {
        Self {
            glow: CursorGlow::default(),
            cfg: glow_config(),
            geom: geometry(),
            quads: Vec::with_capacity(MAX_QUADS),
            last_fp: 0,
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
        }
    }

    fn tick(&mut self, now: Instant, caret: (u16, u16)) -> Wrote {
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
}

// ===========================================================================
// The run
// ===========================================================================

/// Idle run-out ceiling after the last gesture. Every v2 life is shorter:
/// meteor `T + 320 ≤ 440 ms`, star ≤ 460 ms, ribbon swoosh
/// `SWOOSH_TOTAL_S = 1.54 s`.
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
    /// The first tick at which all three laws held at once.
    zero: Option<u64>,
    /// The scratch was empty on the tick after `zero`.
    wrote_nothing_after: bool,
}

/// What one measured pass produced.
#[derive(Clone, Debug, Default)]
struct Report {
    us: Vec<u64>,
    /// The replica fingerprint fold's own µs per measured tick.
    fp_us: Vec<u64>,
    /// Device pixels covered by the `out` (over-ink) stream per measured
    /// tick — what the host's §3.4 ledger prices per pixel.
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
    /// Allocations per WARM-UP pass, cold pass first — the one-time pool
    /// growth a fixed pool would not pay (see `measure`).
    warmup_allocs: Vec<usize>,
}

impl Report {
    fn pct(&self, p: usize) -> u64 {
        let n = self.us.len();
        if n == 0 {
            return 0;
        }
        self.us[(n * p / 100).min(n - 1)]
    }
    fn p50(&self) -> u64 {
        self.pct(50)
    }
    fn p90(&self) -> u64 {
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

/// One pass of the script through `d`, from `t0`. `measured` turns on the
/// timers and the allocation counter. Returns the report and the instant
/// the pass ended (idle included).
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
    (rep, now)
}

/// Warm-up passes before the measured one, at most. The cold pass grows
/// every demand-grown pool; a second pass can still grow the ribbon (the
/// spine's resume floor lengthens cell lives); the gate then measures a pass
/// after one that allocated NOTHING, so a red is a per-frame allocation and
/// not one-time pool growth — which is reported separately as
/// [`Report::warmup_allocs`] and held to "settles to zero".
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
    for _ in 0..repeat() {
        let (r, end) = run(d, sc, t, true);
        rep = r;
        t = end + Duration::from_secs(1);
    }
    rep.warmup_allocs = warmup;
    rep
}

/// One table row set: the engine alone and v2 through the seam. An engine
/// `RK_FRAME_COST_ONLY` leaves out is not run and reports empty (zero
/// everywhere), which the gates below treat as "nothing to hold".
fn measure_all(sc: &Scenario) -> (Report, Report) {
    let r2 = if engine_selected("v2") {
        let mut v2 = V2::new();
        let r = measure(&mut v2, sc);
        print_row("v2 ", sc, &r);
        r
    } else {
        Report::default()
    };
    let rs = if engine_selected("v2s") {
        let mut seam = Host::seam();
        let r = measure(&mut seam, sc);
        print_row("v2s", sc, &r);
        r
    } else {
        Report::default()
    };
    if engine_selected("v2") {
        print_idle(sc, &r2);
    }
    (r2, rs)
}

fn fmt_ms(t: Option<u64>) -> String {
    t.map_or_else(|| "NEVER".to_string(), |ms| format!("{ms} ms"))
}

fn print_row(engine: &str, sc: &Scenario, r: &Report) {
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

fn print_header(mode: &str) {
    println!();
    println!(
        "rainbow kitty v2 frame cost — {mode} — {HZ} Hz tick, geometry 18×40 px, 190×26 cells"
    );
    println!(
        "| {:<44} | eng | p50µs | p90µs |  maxµs | fpµs | quads max (mean; under / out, out px max)                  | hal | all | warm-up allocs / pass |",
        "scenario"
    );
}

fn print_idle(sc: &Scenario, r: &Report) {
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

// ===========================================================================
// The gates
// ===========================================================================

/// §18's deterministic laws, every run: zero allocation on every steady
/// frame and on every idle frame, idle → exactly zero within the longest
/// life, the stream caps, and a live fixture (the scenario actually drew).
/// Timing is printed for information; the debug build's numbers are not the
/// budget's (see the release twin below).
#[test]
fn rainbow_kitty_v2_steady_frames_allocate_nothing_and_idle_to_exactly_zero() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mode = if cfg!(debug_assertions) {
        "DEBUG build (timing informational)"
    } else {
        "release build"
    };
    print_header(mode);
    let mut reds: Vec<String> = Vec::new();
    for sc in scenarios() {
        let (r2, rs) = measure_all(&sc);
        print_allocs(&sc, &r2);
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

/// §18's CPU budget — release only, like tests/cursor_bench.rs: p50 ≤ 400 µs,
/// p90 ≤ 600 µs per tick including the ribbon, on every §21 scenario, for
/// the engine alone and through the seam, measured in the same harness on
/// the same script.
#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture --test-threads=1"]
fn bench_rainbow_kitty_v2_frame_cost_budget() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cfg!(debug_assertions) {
        panic!("the CPU budget is a release-build number; run with --release");
    }
    const P50_BUDGET_US: u64 = 400;
    const P90_BUDGET_US: u64 = 600;
    print_header("release build — §18 budget p50 ≤ 400 µs / p90 ≤ 600 µs");
    let mut reds: Vec<String> = Vec::new();
    for sc in scenarios() {
        let (r2, rs) = measure_all(&sc);
        if engine_selected("v2") && r2.p50() > P50_BUDGET_US {
            reds.push(format!(
                "[{}] v2 p50 {} µs > {P50_BUDGET_US} µs",
                sc.name,
                r2.p50()
            ));
        }
        if engine_selected("v2") && r2.p90() > P90_BUDGET_US {
            reds.push(format!(
                "[{}] v2 p90 {} µs > {P90_BUDGET_US} µs",
                sc.name,
                r2.p90()
            ));
        }
        // The shipping path: v2 THROUGH `CursorGlow`'s seam is held to the
        // same absolute budget as the engine alone.
        if engine_selected("v2s") && rs.p50() > P50_BUDGET_US {
            reds.push(format!(
                "[{}] v2-via-seam p50 {} µs > {P50_BUDGET_US} µs",
                sc.name,
                rs.p50()
            ));
        }
        if engine_selected("v2s") && rs.p90() > P90_BUDGET_US {
            reds.push(format!(
                "[{}] v2-via-seam p90 {} µs > {P90_BUDGET_US} µs",
                sc.name,
                rs.p90()
            ));
        }
    }
    assert!(
        reds.is_empty(),
        "rainbow kitty v2 CPU budget RED:\n  {}",
        reds.join("\n  ")
    );
}
