// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Effects harness: the `effects` floor of `verify perf` (lane B0 of
// docs/DESIGN-card-engine-2026-10-19.md, §5 and §12). What a frame of today's rectangle effects
// costs, measured on scripted scenes through the real emitters, the CPU rasterizer and the GPU
// presenter, so the gated lanes (B5 ramps, B6 pass economy) are judged on numbers and the fx
// layers (B1-B3) have a before.
//
//   targo --unverified run --release -q -p aterm-bench --features dev-seams --example effects_harness
//   -> {"gpu_clock":"metal","cpu_counter":"instructions",...,"episode_gpu_rel":...,
//       "episode_raster_per_ginsn":...,"episode_gpu_p95_us":...,...}
//
// `dev-seams` compiles in the pass-economy ablation (`GpuRenderer::set_unorm_ablation`) and the
// GPU clock's pause (`gpu_time::set_paused`); a release build without it refuses to run rather
// than report half a lane. On macOS, `--features dev-seams,wgpu-oracle` and `--wgpu` draw on the
// wgpu arm (the Windows and BSD presenter, its timestamp-query clock) instead of Metal; that run
// is a record, never the lane's (the lane builds without the oracle).
//
// The scenes, at the desk's cell (JetBrains Mono at 30 px: 18x40) on a 26x190 grid (the
// 3420x1040 drawable §5's pass costs are stated at), on the shipped dark theme with bloom and
// shimmer on:
//   episode  the harness card's intro (`HarnessCard::draw` on its 33 ms clock, the ride-in
//            included), every frame the card asks for, the flow's first frame (the one the
//            intro hands over to) last;
//   flowing  the card flowing on after its intro while a key a twelfth of a second lands in
//            the composer under it: a frame at each key and at each of the flow's 128 ms steps
//            (`HarnessCard::deadline`), a step drawing its palette on the fixed rim art and a
//            key between two steps replaying that step's kept frame (`replay_step`); REPORTED,
//            not gated: the lane's floors are the baseline its box recorded, and gating it
//            waits for a record there (docs/DESIGN-card-engine-2026-10-19.md §4.8.1);
//   settled  the held card (`animate` false: Reduce Motion, or its window out of focus)
//            replayed while a key a frame lands in the composer under it, its first held frame
//            drawn unmeasured;
//   typing   the shipped cursor glow (Rainbow Kitty) ticked at 120 Hz while 12 keys a second
//            land at a shell prompt, then its run-out; a tick whose fingerprint did not move
//            presents nothing, as in the app;
//   scroll   inline streaming under a settled card: a reply's lines, each its own, a line a
//            frame, scroll the screen and the card with it (since lane B3 the settled frame
//            replayed at its new origin: the same two fx contents, a new layer offset).
// Every card scene carries the card's line in the box's last row, as the app writes it under
// a card (`claude_card::make_line`): its text, and its ink and its `✦` laid on the frame by
// the host (`claude_card::ink_lines`), the ink re-made only where the line moved or the card
// is not still.
// and, reported but not gated, the other producers the B5 rule names, each a shorter typing run:
//   bolt     the Laser style's lightning bolts;
//   comet    the Comet style's slabs;
//   classic  the classic wake (`classic_wake`, its comet beams);
//   meteor   the Rainbow Kitty's meteors (`ribbon_beam`) on its nav flights: an 80-cell
//            Ctrl-A / Ctrl-E ping-pong every 200 ms (the frame-cost suite's §21 scenario);
//   flood    the Rainbow Kitty's ribbon at its `RIBBON_QUAD_BUDGET`: 400 keys at 120 a second,
//            a hot line wrapped over three rows, the frame the budget caps.
//
// Per frame, each on its own clock:
//   emit     the producer: the card's replay or draw and its line's ink, or the glow's
//            witness feed and tick;
//   splice   the host's put: the card's two fx layers (taken back by id, laid at its origin,
//            clipped to the pane) and its line's ink and `✦`, or the glow's quads into the
//            frame's streams, and the effect-damage refresh;
//   raster   the CPU presenter: `Renderer::render_input_cached` on the frame;
//   encode   the GPU presenter's present work up to its last commit
//            (`WindowGpu::last_present_encode_ns`), with its `build` (instances and atlas) and
//            `stage` (instance upload) parts from the frame shape;
//   gpu      the device's time for that present (`aterm_gpu::gpu_time`: Metal command-buffer
//            clocks, wgpu timestamp queries; absent, with `gpu_clock` "none", on an arm with no
//            clock); `present` is encode + gpu.
//
// The GPU presenter draws every frame four times, each on its own window of the one renderer
// and in an order that rotates frame by frame: under the pass-economy seam
// (`aterm_gpu::UnormAblation`) `off` (shipped), `thin` (every pass of the plan, each Unorm
// item drawing one instance) and `drop` (no Unorm group: one pass), and `untimed`, the shipped
// plan with the GPU clock paused. Paired per frame, `pass_cost` = thin - drop is what the extra
// passes themselves cost, the figure the B6 go rule reads as `pass_cost_pct` (its p50 over the
// present's p95), `unorm_draw` = off - thin is what the One/One draws cost, which merging the
// passes would not save, and `clock` = off - untimed is what timing a present costs its encode
// (the completion handlers on Metal; the timestamp writes, the deferred resolve and the
// readback harvest on wgpu). `build_stage_pct` (instance build and upload, p50 over the
// present's p95) is the B5 rule's.
//
// RUNS runs follow WARMUP unmeasured ones, each scene on fresh windows. A key's p50/p95 pools
// every run's frames; its `_spread` is the slowest run's p50 over the fastest's.
//
// On a shared desk the absolute figures move together by 2-4x between invocations (another
// session's build takes the cores; the GPU's clock follows its load), so two kinds of key are
// built to hold still:
//   `_rel`  CALIBRATED: every REF_EVERY frames the harness also draws a reference frame (a full
//           repaint of a screen of plain text, alternating two so every row changes) on both
//           presenters, and a `_rel` key is the reference's p50 over the scene's, per run, the
//           median run reported: bigger is cheaper. The GPU one divides the device clock and
//           holds; the CPU ones divide thread CPU time (`cpu_clock` "thread") and do not hold as
//           well (the reference's 14.6 MB repaint follows the memory contention, a scene's
//           small kernels do not). The reference shares the presenter's text work with every
//           scene, so a regression there moves both sides of a scene's ratio; `ref_text_gpu_rel`
//           calibrates the reference itself, against a blank full repaint (cell backgrounds, no
//           glyph) drawn beside it: the text's cost over the frame every present pays.
//   work    `_kins` is the instructions the process retired per frame in the producer, the CPU
//           raster and the GPU present call (`cpu_counter` "instructions": Apple's
//           `proc_pid_rusage`, the read's own cost taken out; "none" elsewhere), and
//           `_per_ginsn` (frames per 10^9 instructions, bigger is cheaper) its gated form. The
//           desk's load does not move it.
// Which keys gate is the `effects` lane's call (`aterm_verify::perf::floors::EFFECTS_LANE`). The
// `_us` figures are the record, and the reference's own (`ref_*`) say what the desk was doing.
//
// Fail closed: a refused present, a GPU frame that fails or never settles, a paused clock that
// timed a present, an ablation the plan did not take (read off the plan the arm built: its mode,
// a Unorm draw left by `drop` or an instance past one by `thin`, other passes or instances), or
// a scene that does not do what it says (a glow that drew nothing, a card that did not scroll,
// a flood that never reached the ribbon's budget) ends the run with exit 1.

use std::fmt::Write as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

use aterm_core::render::{DecoBlend, DecoGlyph, FxSlot, InkCell, PxRect, WordDecoration};
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CURSOR_WITNESS_ROWS, CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::harness_card::{
    CardFrame, CardFrames, CardPalette, CardScene, EPISODE_MS, FRAME_MS, HarnessCard, LineAt,
    card_layer_id, dark_ground,
};
use aterm_gpu::gpu_time::{self, Settled};
use aterm_gpu::{FrameShape, GpuContext, GpuRenderer, UnormAblation, WindowGpu};
use aterm_render::{GlowQuad, RenderInput, Renderer, Theme, WindowCpu};

/// The desk's cell: JetBrains Mono at 30 px is 18x40, the "2x Retina" cell the effects frame
/// costs are stated at (`aterm-effects/tests/cursor_bench.rs`).
const FONT_PX: f32 = 30.0;
const ROWS: usize = 26;
const COLS: usize = 190;
/// The window's interior padding, as the app's tests set it.
const PAD: usize = 12;

/// Unmeasured runs first (atlas, pipelines, the GPU's clocks up), then the measured ones.
const WARMUP: usize = 1;
const RUNS: usize = 5;

/// Keys typed into the composer under the settled card.
const SETTLED_KEYS: usize = 120;
/// Keys typed into the composer under the flowing card, and their pace (the typist's, 12 a
/// second): a key falls between two of the flow's 128 ms steps or on one.
const FLOWING_KEYS: usize = 48;
const FLOWING_KEYS_HZ: u32 = 12;
/// The glow's frame clock and the typist's pace: a key every 10 ticks at 120 Hz is 12 a
/// second (the Rainbow Kitty frame-cost gate's prose pace), then a second of run-out.
const TYPING_HZ: u32 = 120;
const TYPING_KEYS: usize = 48;
const KEY_TICKS: usize = 10;
const RUNOUT_TICKS: usize = 120;
/// Keys typed in each of the other producers' scenes (reported, not gated).
const PRODUCER_KEYS: usize = 24;
/// The flood: a key every tick, long enough to wrap the hot line over three rows.
const FLOOD_KEYS: usize = 400;

/// The meteor scene's flights: hops between two columns 80 apart, one every 200 ms.
const HOPS: usize = 15;
const HOP_TICKS: usize = 24;
const HOP_COLS: (u16, u16) = (20, 100);
/// Launches per scroll run: each scrolls a fresh card from near the bottom to the top row.
const SCROLL_CYCLES: usize = 4;

/// A reference frame is drawn after every this many scene frames.
const REF_EVERY: usize = 8;

/// A wait for something that must happen (a GPU frame settling) is a hang detector.
const HANG: Duration = Duration::from_secs(60);

/// The launch card as Claude Code prints it: the mascot's block elements and two lines of
/// text, the longest ending in column 31.
const CARD: [&str; 3] = [
    " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.304",
    "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.5 \u{00B7} Claude Max",
    "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/src/aterm",
];
/// The card's box: its three rows and the line's, 62 columns, the trail from past the text to
/// the kitty, which stands three rows tall (the third card row is blank across both).
const BOX_ROWS: u16 = 4;
const BOX_COLS: u16 = 62;
const TRAIL: (u16, u16) = (33, 41);
const KITTY_COL: u16 = 41;
/// The card's line, in the box's last row: its `✦` in the card's text column and its words
/// two columns on (`claude_card::LINE_LEAD`), as the app writes it under the card.
const LINE_ROW: u16 = 3;
const LINE_STAR_COL: u16 = 12;
const LINE_COL: u16 = LINE_STAR_COL + 2;
const LINE_TEXT: &str = "Cruise: the kitty keeps watch while the reply streams";

/// Where the card sits for the episode and settled scenes, and where a scroll launch starts it.
const CARD_TOP: u16 = 2;
const SCROLL_TOP: u16 = 20;

const PROSE: &[u8] = b"The quick brown fox jumps over the lazy dog, and keeps on going. ";
const STREAMED: &str = "Reading the renderer's frame plan: each stream group draws in its own view, \
                        so a frame with additive light opens another pass over the drawable.";

fn fail(why: impl std::fmt::Display) -> ! {
    eprintln!("effects_harness: FAILED: {why}");
    std::process::exit(1);
}

fn elapsed_ns(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// This thread's CPU time: what a stretch of work cost the core, with the time the scheduler
/// gave the core to someone else left out.
#[cfg(unix)]
fn thread_cpu_ns() -> u64 {
    let mut stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `stamp` is initialized and writable for this one call.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut stamp) } != 0 {
        fail("the thread CPU clock is unreadable");
    }
    u64::try_from(stamp.tv_sec).unwrap_or(0) * 1_000_000_000
        + u64::try_from(stamp.tv_nsec).unwrap_or(0)
}

/// No thread CPU clock: the wall clock stands in (`cpu_clock` "wall").
#[cfg(not(unix))]
fn thread_cpu_ns() -> u64 {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    elapsed_ns(*ORIGIN.get_or_init(Instant::now))
}

/// The process's instructions retired so far, user and kernel, every thread: Apple's
/// `proc_pid_rusage` (`ri_instructions`, read from the cores' fixed counters).
#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    let pid = libc::c_int::try_from(std::process::id()).ok()?;
    // SAFETY: `info` is writable for one `rusage_info_v4`, the flavor asked for; the call fills it or fails, and a zeroed one is a valid value either way.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    // SAFETY: zeroed at creation, so initialized whatever the call did.
    (rc == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

/// No instruction counter this harness can read here: the CPU work keys are absent.
#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<u64> {
    None
}

/// The instructions a [`Stopwatch`] counts around nothing at all (its own reads), taken out of
/// every reading.
static COUNTER_OVERHEAD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Whether the instruction counter counts here: it reads, and a loop of known length moves it
/// (a counter that reads a constant would report every frame as free).
fn calibrate_counter() -> bool {
    let empty = (0..64).filter_map(|_| Stopwatch::start().stop().ins).min();
    let Some(empty) = empty else {
        return false;
    };
    let _ = COUNTER_OVERHEAD.set(empty);
    let watch = Stopwatch::start();
    let mut acc = 0u64;
    for i in 0..100_000u64 {
        acc = black_box(acc.wrapping_add(i));
    }
    black_box(acc);
    watch.stop().ins.is_some_and(|n| n >= 100_000)
}

/// A stretch of work on this thread: its wall time, its CPU time and the process's
/// instructions retired across it (`None` with no counter).
#[derive(Clone, Copy, Debug, Default)]
struct Spent {
    wall_ns: u64,
    cpu_ns: u64,
    ins: Option<u64>,
}

struct Stopwatch {
    wall: Instant,
    cpu: u64,
    ins: Option<u64>,
}

impl Stopwatch {
    /// The counter first and the wall last, so each clock spans the reads inside it the same
    /// way every time (a constant the overhead calibration takes out).
    fn start() -> Self {
        Self {
            ins: instructions(),
            cpu: thread_cpu_ns(),
            wall: Instant::now(),
        }
    }

    /// The instructions since `start`, the stopwatch's own reads included.
    fn raw_ins(&self) -> Option<u64> {
        instructions()?.checked_sub(self.ins?)
    }

    fn stop(&self) -> Spent {
        let wall_ns = elapsed_ns(self.wall);
        let cpu_ns = thread_cpu_ns().saturating_sub(self.cpu);
        let overhead = COUNTER_OVERHEAD.get().copied().unwrap_or(0);
        Spent {
            wall_ns,
            cpu_ns,
            ins: self.raw_ins().map(|n| n.saturating_sub(overhead)),
        }
    }
}

/// Why a release build without `dev-seams` refuses to run.
const NO_SEAM: &str =
    "built without the ablation seam: run it with `--features dev-seams` (the `effects` lane does)";

/// The pass-economy seam, compiled in by `dev-seams` (or any debug build).
#[cfg(any(debug_assertions, feature = "dev-seams"))]
fn set_ablation(gpu: &mut GpuRenderer, ablation: UnormAblation) {
    gpu.set_unorm_ablation(ablation);
}

#[cfg(not(any(debug_assertions, feature = "dev-seams")))]
fn set_ablation(_: &mut GpuRenderer, _: UnormAblation) {
    fail(NO_SEAM);
}

/// The GPU clock's pause, the same seam's.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
fn set_clocked(clocked: bool) {
    gpu_time::set_paused(!clocked);
}

#[cfg(not(any(debug_assertions, feature = "dev-seams")))]
fn set_clocked(_: bool) {
    fail(NO_SEAM);
}

/// One way the GPU presenter draws a frame: a plan of the pass-economy seam, timed or with the
/// clock paused.
#[derive(Clone, Copy, Debug)]
struct Way {
    ablation: UnormAblation,
    clocked: bool,
    name: &'static str,
}

/// The four ways every frame is drawn, in [`Sample::arms`] order.
const WAYS: [Way; 4] = [
    Way {
        ablation: UnormAblation::Off,
        clocked: true,
        name: "off",
    },
    Way {
        ablation: UnormAblation::Thin,
        clocked: true,
        name: "thin",
    },
    Way {
        ablation: UnormAblation::Drop,
        clocked: true,
        name: "drop",
    },
    Way {
        ablation: UnormAblation::Off,
        clocked: false,
        name: "untimed",
    },
];

/// The host-owned effect streams the scenes fill.
fn clear_streams(input: &mut RenderInput) {
    input.glow_under.clear();
    input.nova_add.clear();
    input.cursor_glow_add.clear();
    input.glow_halo.clear();
    input.fire_patch.clear();
    input.char_fg.clear();
    input.fire_halo.clear();
}

fn fx_quads(input: &RenderInput) -> u64 {
    [
        input.glow_under.len(),
        input.nova_add.len(),
        input.cursor_glow_add.len(),
        input.glow_halo.len(),
        input.fire_patch.len(),
        input.char_fg.len(),
        input.fire_halo.len(),
    ]
    .into_iter()
    .map(|n| n as u64)
    .sum()
}

/// One GPU present of a frame under one plan.
#[derive(Clone, Copy, Debug, Default)]
struct Arm {
    encode_ns: u64,
    /// This thread's CPU time in the present call: the encode's, the waits costing none.
    call_cpu_ns: u64,
    /// The instructions the process retired in the present call.
    call_ins: Option<u64>,
    gpu_ns: Option<u64>,
    shape: FrameShape,
}

impl Arm {
    /// Encode plus GPU time (encode alone on an arm with no clock).
    fn present_ns(&self) -> u64 {
        self.encode_ns + self.gpu_ns.unwrap_or(0)
    }
}

/// One presented frame's figures.
#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    emit: Spent,
    splice_ns: u64,
    raster: Spent,
    fx_quads: u64,
    /// The `glow_under` stream's quads: the ribbon's body, the card's border and trail.
    under_quads: u64,
    /// The GPU presents, indexed like [`WAYS`].
    arms: [Arm; 4],
}

impl Sample {
    fn off(&self) -> &Arm {
        &self.arms[0]
    }
    fn thin(&self) -> &Arm {
        &self.arms[1]
    }
    fn drop(&self) -> &Arm {
        &self.arms[2]
    }
    fn untimed(&self) -> &Arm {
        &self.arms[3]
    }
}

/// One reference frame's costs, the calibration the `_rel` keys divide by, and the GPU time of
/// the blank full repaint drawn beside it (the reference's own calibration).
#[derive(Clone, Copy, Debug, Default)]
struct RefSample {
    raster: Spent,
    encode_ns: u64,
    call_cpu_ns: u64,
    call_ins: Option<u64>,
    gpu_ns: Option<u64>,
    blank_gpu_ns: Option<u64>,
}

/// One scene's frames in one run, and the reference frames drawn among them.
#[derive(Debug, Default)]
struct Run {
    samples: Vec<Sample>,
    refs: Vec<RefSample>,
}

/// The calibration: two screens of plain text, each differing from the other in every row, so
/// each reference frame is a full repaint, on windows of their own; and two blank screens (cell
/// backgrounds of two shades, no glyph), each a full repaint of what every present pays.
struct Reference {
    inputs: [RenderInput; 2],
    blanks: [RenderInput; 2],
    cpu: WindowCpu,
    gpu: WindowGpu,
    blank_gpu: WindowGpu,
    next: usize,
}

impl Reference {
    fn new() -> Self {
        let screen = |n: usize| {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            for row in 0..ROWS {
                let line = format!(
                    "-rw-r--r--  1 user  staff  {:>6} Oct 19 {:02}:{:02} crates/aterm-{}/src/lib.rs  ",
                    (row + 1) * 977 + n * 13,
                    9 + n,
                    row,
                    ["gpu", "render", "core", "effects"][(row + n) % 4],
                );
                let line: String = line.chars().cycle().take(COLS - 1).collect();
                term.process(format!("{}{line}", cup(row as u16, 0)).as_bytes());
            }
            term.cell_frame(ROWS, COLS)
        };
        let blank = |n: usize| {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            let shade = [236, 238][n];
            let spaces = " ".repeat(COLS);
            for row in 0..ROWS {
                term.process(
                    format!("{}\x1b[48;5;{shade}m{spaces}", cup(row as u16, 0)).as_bytes(),
                );
            }
            term.cell_frame(ROWS, COLS)
        };
        Self {
            inputs: [screen(0), screen(1)],
            blanks: [blank(0), blank(1)],
            cpu: WindowCpu::new(),
            gpu: WindowGpu::new(),
            blank_gpu: WindowGpu::new(),
            next: 0,
        }
    }
}

/// One scene's windows: the CPU presenter's, and one GPU window per way in [`WAYS`].
struct Win {
    cpu: WindowCpu,
    gpu: [WindowGpu; 4],
}

impl Win {
    fn new() -> Self {
        Self {
            cpu: WindowCpu::new(),
            gpu: std::array::from_fn(|_| WindowGpu::new()),
        }
    }
}

/// Both presenters on one font and theme, and the frame's geometry.
struct Rig {
    cpu: Renderer,
    gpu: GpuRenderer,
    /// `metal`, `wgpu` or `none`.
    clock: &'static str,
    /// `instructions`, or `none` where no counter counts.
    counter: &'static str,
    theme: Theme,
    cw: u16,
    ch: u16,
    pad: u16,
    grid_top: u16,
    frame_w: usize,
    frame_h: usize,
    /// Frames presented: the GPU presents' order rotates with it.
    frames: usize,
    reference: Reference,
    /// The reference frames drawn since the scene last took them.
    refs: Vec<RefSample>,
}

/// `--wgpu`: draw on the wgpu arm, the oracle a macOS build carries under `wgpu-oracle`.
#[cfg(all(target_os = "macos", feature = "wgpu-oracle"))]
fn on_wgpu(gpu: &mut GpuRenderer) {
    gpu.disarm_metal_for_oracle();
}

#[cfg(not(all(target_os = "macos", feature = "wgpu-oracle")))]
fn on_wgpu(_: &mut GpuRenderer) {
    if cfg!(target_os = "macos") {
        fail("--wgpu needs the wgpu arm: build with `--features dev-seams,wgpu-oracle`");
    }
}

/// What `main` reports instead of a lane when the host has no GPU: the lane skips.
fn skip(why: impl std::fmt::Display) -> ! {
    let why = why.to_string().replace(['"', '\\'], "'");
    eprintln!("effects_harness: SKIP: {why}");
    println!("{{\"skip\":\"{why}\"}}");
    std::process::exit(0);
}

impl Rig {
    fn new() -> Self {
        let font = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
        );
        let theme = Theme::default();
        let ctx = GpuContext::new().unwrap_or_else(|e| skip(format!("no GPU: {e}")));
        let raster = |what: &str| {
            let mut r = Renderer::from_system_with_family(Some(font), FONT_PX, theme)
                .unwrap_or_else(|| fail(format!("{font} does not load for the {what}")));
            r.set_pad(PAD);
            r.debug_block_on_lazy_fallbacks();
            r
        };
        let cpu = raster("CPU presenter");
        let mut gpu =
            GpuRenderer::from_parts(ctx, raster("GPU presenter"), Some(font.into()), theme)
                .unwrap_or_else(|e| fail(format!("the GPU renderer did not assemble: {e}")));
        gpu.set_pad(PAD);
        gpu.set_bloom(true);
        gpu.set_shimmer(true);
        gpu.debug_block_on_lazy_fallbacks();
        let wgpu = std::env::args().skip(1).any(|a| a == "--wgpu");
        if wgpu {
            on_wgpu(&mut gpu);
        }
        let (cw, ch) = cpu.cell_size();
        if gpu.cell_size() != (cw, ch) {
            fail("the two presenters disagree on the cell");
        }
        let (frame_w, frame_h) = cpu.frame_size(ROWS, COLS);
        let clock = if !gpu.gpu_time_supported() {
            "none"
        } else if cfg!(target_os = "macos") && !wgpu {
            "metal"
        } else {
            "wgpu"
        };
        let counter = if calibrate_counter() {
            "instructions"
        } else {
            "none"
        };
        let px = |v: usize| u16::try_from(v).unwrap_or_else(|_| fail("a geometry past u16"));
        Self {
            cw: px(cw),
            ch: px(ch),
            pad: px(cpu.pad()),
            grid_top: px(cpu.grid_top()),
            frame_w,
            frame_h,
            cpu,
            gpu,
            clock,
            counter,
            theme,
            frames: 0,
            reference: Reference::new(),
            refs: Vec::new(),
        }
    }

    /// The card's scene with its banner on grid row `top`, as the app lays it for a pane that
    /// fills the window: window pixels, the left border in the padding, half a row of air over
    /// the banner and under the box, the pane as the clip.
    fn card_scene(&self, top: u16) -> CardScene {
        let origin_x = i32::from(self.pad);
        let origin_y = i32::from(self.grid_top);
        CardScene {
            origin_x,
            origin_y,
            cw: self.cw,
            ch: self.ch,
            row_base: 0,
            pad_left: self.pad,
            pad_above: self.ch,
            pad_below: self.ch,
            top,
            rows: BOX_ROWS,
            cols: BOX_COLS,
            clip: (
                0,
                origin_y,
                origin_x + COLS as i32 * i32::from(self.cw),
                origin_y + ROWS as i32 * i32::from(self.ch),
            ),
            trail: Some(TRAIL),
            kitty_col: Some(KITTY_COL),
            kitty_rows: 3,
            palette: CardPalette::Cruise,
            dark: dark_ground(self.theme.bg),
            ground: self.theme.bg,
            serious: false,
            coat: 0x00E8_A060,
            eye: 0x0040_C080,
            wand: None,
        }
    }

    /// The grid's origin in window pixels: a grid-space layer's shift.
    fn grid(&self) -> (i32, i32) {
        (i32::from(self.pad), i32::from(self.grid_top))
    }

    fn glow_geom(&self) -> Geom {
        let px = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
        Geom {
            cw: usize::from(self.cw),
            ch: usize::from(self.ch),
            rows: ROWS,
            cols: COLS,
            origin_x: self.pad,
            origin_y: self.grid_top,
            win_w: px(self.frame_w),
            win_h: px(self.frame_h),
            head: 0,
        }
    }

    /// The device time of the present this renderer just made: awaited (a hang detector, not a
    /// budget), `None` on an arm with no clock.
    fn gpu_time(&mut self) -> Option<u64> {
        if self.clock == "none" {
            return None;
        }
        let Some(serial) = self.gpu.last_gpu_time_serial() else {
            fail("a present on a GPU-clocked arm went untimed (gpu_time skipped it)");
        };
        let deadline = Instant::now() + HANG;
        loop {
            self.gpu.poll_gpu_time();
            match gpu_time::settled(serial) {
                Some(Settled::Timed(t)) => return Some(t.gpu_ns),
                Some(Settled::Failed) => fail(format!(
                    "GPU-time frame {serial} failed: a part did not complete"
                )),
                Some(Settled::Untimed) => fail(format!("GPU-time frame {serial} timed no part")),
                None => {}
            }
            if Instant::now() >= deadline {
                fail(format!(
                    "GPU-time frame {serial} never settled: a completion never reported"
                ));
            }
            std::thread::yield_now();
        }
    }

    /// Present `input` on the CPU presenter, then on the GPU presenter under each plan.
    fn present(
        &mut self,
        win: &mut Win,
        input: &RenderInput,
        emit: Spent,
        splice_ns: u64,
    ) -> Sample {
        let watch = Stopwatch::start();
        let view = self.cpu.render_input_cached(&mut win.cpu, input);
        black_box(&view);
        drop(view);
        let raster = watch.stop();

        let mut arms = [Arm::default(); 4];
        for k in 0..WAYS.len() {
            let i = (k + self.frames) % WAYS.len();
            let way = WAYS[i];
            set_ablation(&mut self.gpu, way.ablation);
            set_clocked(way.clocked);
            let watch = Stopwatch::start();
            if !self
                .gpu
                .present_virtual(&mut win.gpu[i], input, false, None, None)
            {
                fail(format!("the GPU presenter refused a frame ({})", way.name));
            }
            let call = watch.stop();
            set_clocked(true);
            let shape = self.gpu.frame_shape();
            let gpu_ns = if way.clocked {
                self.gpu_time()
            } else if self.gpu.last_gpu_time_serial().is_some() {
                fail("a present with the GPU clock paused was timed")
            } else {
                None
            };
            arms[i] = Arm {
                encode_ns: win.gpu[i].last_present_encode_ns(),
                call_cpu_ns: call.cpu_ns,
                call_ins: call.ins,
                gpu_ns,
                shape,
            };
        }
        set_ablation(&mut self.gpu, UnormAblation::Off);
        self.frames += 1;
        if self.frames.is_multiple_of(REF_EVERY) {
            let reference = self.reference_frame();
            self.refs.push(reference);
        }
        let [off, thin, dropped, untimed] = &arms;
        Self::check_ablation(&off.shape, &thin.shape, &dropped.shape);
        if (
            untimed.shape.ablation,
            untimed.shape.passes,
            untimed.shape.instances,
        ) != (off.shape.ablation, off.shape.passes, off.shape.instances)
        {
            fail("the untimed present drew another plan than the shipped one");
        }
        Sample {
            emit,
            splice_ns,
            raster,
            fx_quads: fx_quads(input),
            under_quads: input.glow_under.len() as u64,
            arms,
        }
    }

    /// The three plans of one frame did what their names say, read off the plans the arm built
    /// (`FrameShape::unorm_draws`, counted from the plan, and the mode it was built under), so
    /// an arm that ignored the seam ends the run instead of pricing the passes at nothing:
    /// dropped, no Unorm draw is left and the all-sRGB plan is one pass; thinned, every Unorm
    /// draw and every pass stays, each drawing at most one instance; and no plan built other
    /// instances than the shipped one.
    fn check_ablation(off: &FrameShape, thin: &FrameShape, dropped: &FrameShape) {
        for (shape, way) in [off, thin, dropped].into_iter().zip(WAYS) {
            if shape.ablation != way.ablation {
                fail(format!(
                    "the frame plan was built under {}, not the {} ablation",
                    shape.ablation.name(),
                    way.name
                ));
            }
        }
        if dropped.unorm_draws != 0 || dropped.passes != 1 {
            fail(format!(
                "the dropped plan kept {} Unorm draws in {} passes (want none, in one pass)",
                dropped.unorm_draws, dropped.passes
            ));
        }
        if thin.passes != off.passes
            || thin.unorm_draws != off.unorm_draws
            || thin.unorm_instances > u64::from(thin.unorm_draws)
        {
            fail(format!(
                "the thinned plan is not the shipped one at one instance a Unorm draw: {} passes \
                 and {} draws of {} instances, against {} passes and {} draws",
                thin.passes, thin.unorm_draws, thin.unorm_instances, off.passes, off.unorm_draws
            ));
        }
        if thin.instances != off.instances || dropped.instances != off.instances {
            fail("an ablated plan built other instances than the shipped one");
        }
    }

    /// Draw the next reference frame on both presenters.
    fn reference_frame(&mut self) -> RefSample {
        let reference = &mut self.reference;
        let input = &reference.inputs[reference.next];
        reference.next ^= 1;
        let watch = Stopwatch::start();
        let view = self.cpu.render_input_cached(&mut reference.cpu, input);
        black_box(&view);
        drop(view);
        let raster = watch.stop();
        let watch = Stopwatch::start();
        if !self
            .gpu
            .present_virtual(&mut reference.gpu, input, false, None, None)
        {
            fail("the GPU presenter refused the reference frame");
        }
        let call = watch.stop();
        let encode_ns = reference.gpu.last_present_encode_ns();
        let gpu_ns = self.gpu_time();
        let reference = &mut self.reference;
        let blank = &reference.blanks[reference.next];
        if !self
            .gpu
            .present_virtual(&mut reference.blank_gpu, blank, false, None, None)
        {
            fail("the GPU presenter refused the blank reference frame");
        }
        RefSample {
            raster,
            encode_ns,
            call_cpu_ns: call.cpu_ns,
            call_ins: call.ins,
            gpu_ns,
            blank_gpu_ns: self.gpu_time(),
        }
    }

    /// The scene's run: its frames, and the reference frames drawn since the last take.
    fn take_run(&mut self, samples: Vec<Sample>) -> Run {
        Run {
            samples,
            refs: std::mem::take(&mut self.refs),
        }
    }

    /// The frame `term` shows now, its effect streams emptied for the scene to fill.
    fn extract(term: &mut Terminal, input: &mut RenderInput) {
        let _ = term.cell_frame_damage_scoped_into(input, ROWS, COLS);
        clear_streams(input);
    }

    /// An unmeasured frame: fresh windows draw it in full, so the scene's measured frames are
    /// the ones the app would present next.
    fn settle_window(&mut self, win: &mut Win, input: &mut RenderInput) {
        input.refresh_cursor_effect_damage();
        let _ = self.present(win, input, Spent::default(), 0);
    }
}

/// The card's name in the frame's layers (the app names a card by its session).
const CARD_ID: u64 = 1;

/// The card's host: the app's replay-or-draw (`claude_card.rs`): the card's frames (its kept
/// settled or flow frame, the ring its episode frames are built in, its arts), the frame the last
/// emit made, which the splice lays as the card's two fx layers, and the card's line.
#[derive(Default)]
struct CardHost {
    card: HarnessCard,
    frames: CardFrames,
    frame: Option<CardFrame>,
    line: LineHost,
}

/// The card's line on the glass (`claude_card::make_line` and `ink_lines`): its ink, re-made
/// only where the line moved or the card is not still (the app keys the line by its row and
/// keeps a still card's settled gradient), and its `✦`, which the splice lays on the frame in
/// place of the last splice's.
#[derive(Default)]
struct LineHost {
    row: Option<u16>,
    settled: bool,
    ink: Vec<InkCell>,
    star: Option<WordDecoration>,
}

impl LineHost {
    /// Make the line of the card at `scene` for this frame, as the app does in the card's pass.
    fn make(&mut self, card: &HarnessCard, scene: &CardScene, now: Instant) {
        let row = scene.top + LINE_ROW;
        let still = card.is_still();
        if self.row == Some(row) && self.settled && still {
            return;
        }
        self.row = Some(row);
        self.ink.clear();
        let at = LineAt {
            row,
            col: LINE_COL,
            len: LINE_TEXT.chars().count() as u16,
            ..LineAt::default()
        };
        card.line_ink_into(
            (!still).then_some(now),
            &at,
            scene.ground,
            true,
            &mut self.ink,
        );
        self.settled = still;
        let [r, g, b] = self.ink.first().map_or([0xFF; 3], |c| c.color);
        self.star = Some(WordDecoration {
            row,
            col: LINE_STAR_COL,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Star4,
            blend: if scene.dark {
                DecoBlend::Add
            } else {
                DecoBlend::Over
            },
            color: u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
            alpha: u8::MAX,
        });
    }

    /// Lay the line's ink and `✦` on the frame. The bench's frames carry no other producer's,
    /// so the last splice's are taken back by clearing the two streams.
    fn lay(&self, input: &mut RenderInput) {
        input.ink.clone_from(&self.ink);
        input.word_decorations.clear();
        input.word_decorations.extend(self.star);
    }
}

impl CardHost {
    /// The producer, in the app's order: replayed (`true`) when the card is held at the settled
    /// frame it keeps for a scene of this shape (an inline scroll is the same shape at a new
    /// origin), or flows and keeps the frame of `now`'s step, else drawn on `now`.
    fn emit(&mut self, now: Instant, scene: &CardScene, animate: bool) -> (bool, Spent) {
        let watch = Stopwatch::start();
        let kept = self
            .card
            .replay(scene, animate, &self.frames)
            .or_else(|| self.card.replay_step(scene, animate, &self.frames, now));
        let (frame, replayed) = match kept {
            Some(frame) => (frame, true),
            None => (self.card.draw(now, scene, animate, &mut self.frames), false),
        };
        self.frame = Some(frame);
        self.line.make(&self.card, scene, now);
        (replayed, watch.stop())
    }

    /// The splice, as the app's (`claude_card.rs`) does it: the card's two layers from the last
    /// splice taken back by id, then this frame's laid at its origin and clipped to the pane
    /// (`scene.clip`, `CardFrame::layers`): two `Arc`s, nothing copied.
    fn splice(&mut self, scene: &CardScene, grid: (i32, i32), input: &mut RenderInput) -> u64 {
        let started = Instant::now();
        clear_streams(input);
        let ours = [
            card_layer_id(CARD_ID, FxSlot::Under),
            card_layer_id(CARD_ID, FxSlot::Over),
        ];
        input.fx_layers.retain(|l| !ours.contains(&l.id));
        if let Some(frame) = self.frame.as_ref().filter(|f| !f.is_empty()) {
            let (x0, y0, x1, y1) = scene.clip;
            input
                .fx_layers
                .extend(frame.layers(CARD_ID, PxRect::new(x0, y0, x1, y1), grid));
        }
        self.line.lay(input);
        input.refresh_cursor_effect_damage();
        elapsed_ns(started)
    }

    /// Whether the last frame drew the border and the kitty.
    fn drew_whole(&self) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|f| !f.under.items().is_empty() && !f.over.items().is_empty())
    }
}

/// The cursor glow's host: the content witness fed from the terminal's rows, then the tick, as
/// the app does under its frame hold.
struct GlowHost {
    glow: CursorGlow,
    cfg: GlowConfig,
    geom: Geom,
    out: Vec<GlowQuad>,
    row: Vec<char>,
    last_fp: u64,
}

impl GlowHost {
    /// The shipped dark-theme glow config (the Rainbow Kitty's, as its frame-cost gate builds
    /// it), on this theme's colours, in `style`.
    fn new(rig: &Rig, style: GlowStyle) -> Self {
        Self {
            glow: CursorGlow::default(),
            cfg: GlowConfig {
                classic_mono: false,
                ribbon_tall: true,
                ribbon_flat: false,
                enabled: true,
                dark_theme: true,
                theme_fg: rig.theme.fg,
                theme_bg: rig.theme.bg,
                style,
                color: 0x00d0_d0d0,
                accent: 0x0048_c9ff,
                duration: Duration::from_millis(650),
                length: usize::MAX,
                intensity: 1.0,
                audible: true,
                radius: 0.4,
                ring: true,
                beam: aterm_effects::cursor_glow::style_has_beam_of(style, ""),
                head_dx: 0.5,
            },
            geom: rig.glow_geom(),
            out: Vec::new(),
            row: Vec::new(),
            last_fp: 0,
        }
    }

    fn emit(&mut self, term: &Terminal, caret: (u16, u16), now: Instant) -> (u64, Spent) {
        let watch = Stopwatch::start();
        term.row_cols_into(usize::from(caret.0), &mut self.row);
        self.glow.observe_ribbon_row(caret.0, &self.row);
        let mut rows = [0u16; CURSOR_WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            if r != caret.0 && usize::from(r) < ROWS {
                term.row_cols_into(usize::from(r), &mut self.row);
                self.glow.observe_ribbon_row(r, &self.row);
            }
        }
        let fp = self
            .glow
            .tick(Some(caret), now, &self.cfg, self.geom, &mut self.out);
        (fp, watch.stop())
    }

    /// The app's stream map: aurora, halos, fire patches, under-light, charred glyphs and fire
    /// halo cells.
    fn splice(&self, input: &mut RenderInput) -> u64 {
        let started = Instant::now();
        clear_streams(input);
        input.cursor_glow_add.extend_from_slice(&self.out);
        input.glow_halo.extend_from_slice(self.glow.halos());
        input.fire_patch.extend_from_slice(self.glow.patches());
        input.glow_under.extend_from_slice(self.glow.under_quads());
        input.char_fg.extend_from_slice(self.glow.charred());
        input.fire_halo.extend_from_slice(self.glow.halo_cells());
        input.refresh_cursor_effect_damage();
        elapsed_ns(started)
    }
}

/// `CSI row;col H`, one-based.
fn cup(row: u16, col: u16) -> String {
    format!("\x1b[{};{}H", row + 1, col + 1)
}

/// The card's three rows from `top`, with a blank row over and two under (the line's row in the
/// box, its words as the app writes them there, then the air).
fn print_card(term: &mut Terminal, top: u16) {
    for (i, line) in CARD.iter().enumerate() {
        term.process(format!("{}{line}", cup(top + i as u16, 0)).as_bytes());
    }
    term.process(format!("{}{LINE_TEXT}", cup(top + LINE_ROW, LINE_COL)).as_bytes());
}

/// Whether grid row `row` of `term` shows the card's banner.
fn banner_at(term: &Terminal, row: u16, scratch: &mut Vec<char>) -> bool {
    term.row_cols_into(usize::from(row), scratch);
    let banner: Vec<char> = CARD[0].chars().collect();
    scratch.len() >= banner.len() && scratch[..banner.len()] == banner[..]
}

/// The intro, then the flowing card under typing, then the held card under typing, on one
/// launch.
fn card_scenes(rig: &mut Rig, seed: u64) -> (Run, Run, Run) {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ claude");
    print_card(&mut term, CARD_TOP);
    let composer = CARD_TOP + BOX_ROWS + 2;
    term.process(format!("{}> ", cup(composer, 0)).as_bytes());
    let mut input = RenderInput::empty();
    let mut win = Win::new();
    Rig::extract(&mut term, &mut input);
    rig.settle_window(&mut win, &mut input);
    rig.refs.clear();

    let mut host = CardHost::default();
    let scene = rig.card_scene(CARD_TOP);
    let t0 = Instant::now();
    host.card.arm(t0, seed);
    let mut episode = Vec::new();
    let mut now = t0;
    let most = (EPISODE_MS / FRAME_MS) as usize + 8;
    loop {
        Rig::extract(&mut term, &mut input);
        let (replayed, emit) = host.emit(now, &scene, true);
        if replayed {
            fail("an episode frame replayed: the card was not armed");
        }
        let splice_ns = host.splice(&scene, rig.grid(), &mut input);
        episode.push(rig.present(&mut win, &input, emit, splice_ns));
        if !host.card.intro_live() {
            break;
        }
        match host.card.deadline(now) {
            Some(next) if episode.len() < most => now = next,
            Some(_) => fail(format!("the intro still asks for frames after {most}")),
            None => fail("the intro stopped asking for frames before it was over"),
        }
    }
    if host.frames.kept().is_none_or(|k| k.step().is_none()) || !host.drew_whole() {
        fail("the intro did not hand over to the flow");
    }
    let episode = rig.take_run(episode);

    let key_every = Duration::from_secs(1) / FLOWING_KEYS_HZ;
    let typed_from = now;
    let mut flowing = Vec::with_capacity(FLOWING_KEYS * 2);
    let (mut keys, mut drawn, mut replays) = (0, 0, 0);
    while keys < FLOWING_KEYS {
        let key_at = typed_from + key_every * (keys as u32 + 1);
        let Some(step_at) = host.card.deadline(now) else {
            fail("the flowing card stopped asking for its steps");
        };
        if key_at <= step_at {
            now = key_at;
            term.process(&[PROSE[keys % PROSE.len()]]);
            keys += 1;
        } else {
            now = step_at;
        }
        Rig::extract(&mut term, &mut input);
        let (replayed, emit) = host.emit(now, &scene, true);
        if replayed {
            replays += 1;
        } else {
            drawn += 1;
        }
        let splice_ns = host.splice(&scene, rig.grid(), &mut input);
        flowing.push(rig.present(&mut win, &input, emit, splice_ns));
    }
    if drawn == 0 || replays == 0 {
        fail(format!(
            "the flowing card drew {drawn} and replayed {replays} of its frames: a step draws, a \
             key between two steps replays"
        ));
    }
    let flowing = rig.take_run(flowing);

    Rig::extract(&mut term, &mut input);
    let (replayed, _) = host.emit(now, &scene, false);
    if replayed || host.frames.kept().is_none_or(|k| k.step().is_some()) {
        fail("the held card did not draw its settled frame");
    }
    let _ = host.splice(&scene, rig.grid(), &mut input);
    rig.settle_window(&mut win, &mut input);
    rig.refs.clear();

    let mut settled = Vec::with_capacity(SETTLED_KEYS);
    for key in 0..SETTLED_KEYS {
        term.process(&[PROSE[key % PROSE.len()]]);
        Rig::extract(&mut term, &mut input);
        let (replayed, emit) = host.emit(now, &scene, false);
        if !replayed {
            fail("the settled card was drawn again rather than replayed");
        }
        let splice_ns = host.splice(&scene, rig.grid(), &mut input);
        settled.push(rig.present(&mut win, &input, emit, splice_ns));
    }
    (episode, flowing, rig.take_run(settled))
}

/// What the caret does in a glow scene, on the glow's 120 Hz clock, before the run-out.
#[derive(Clone, Copy)]
enum Script {
    /// `keys` keys typed, one every `every` ticks.
    Type { keys: usize, every: usize },
    /// `hops` nav moves between [`HOP_COLS`] (a Ctrl-A or Ctrl-E), one every `every` ticks.
    Hop { hops: usize, every: usize },
}

impl Script {
    /// Its events and their ticks.
    fn steps(self) -> (usize, usize) {
        match self {
            Self::Type { keys, every } | Self::Hop { hops: keys, every } => (keys, every),
        }
    }
}

/// The flood measures the frame the ribbon's budget caps, or it measures nothing it names: its
/// fullest frame spends the budget, give or take the hand's two quads.
fn at_the_ribbon_budget(flood: &Run) {
    use aterm_effects::rainbow_kitty::ribbon::RIBBON_QUAD_BUDGET;
    let fullest = flood
        .samples
        .iter()
        .map(|s| s.under_quads)
        .max()
        .unwrap_or(0);
    if fullest + 200 < RIBBON_QUAD_BUDGET as u64 {
        fail(format!(
            "the flood never brought the ribbon to its budget: {fullest} of \
             {RIBBON_QUAD_BUDGET} under quads"
        ));
    }
}

/// The cursor glow in `style` at a shell prompt under `script`, then its run-out.
fn typing_scene(rig: &mut Rig, style: GlowStyle, script: Script) -> Run {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    for i in 0..10 {
        term.process(
            format!("drwxr-xr-x  12 user  staff   384 Oct 19 09:{i:02} crate-{i}\r\n").as_bytes(),
        );
    }
    term.process(b"~/src/aterm $ ");
    let mut input = RenderInput::empty();
    let mut win = Win::new();
    Rig::extract(&mut term, &mut input);
    rig.settle_window(&mut win, &mut input);
    rig.refs.clear();

    let mut host = GlowHost::new(rig, style);
    let period = Duration::from_secs(1) / TYPING_HZ;
    let t0 = Instant::now();
    let mut samples = Vec::new();
    let (events, every) = script.steps();
    let prompt_row = u16::try_from(term.cell_frame(ROWS, COLS).cursor_row).unwrap_or(0);
    for tick in 0..events * every + RUNOUT_TICKS {
        let now = t0 + period * tick as u32;
        let event = (tick % every == 0 && tick / every < events).then_some(tick / every);
        match (event, script) {
            (Some(k), Script::Type { .. }) => {
                term.process(&[PROSE[k % PROSE.len()]]);
                host.glow.note_synthetic_typed(now, 1);
            }
            (Some(k), Script::Hop { .. }) => {
                let col = if k % 2 == 0 { HOP_COLS.1 } else { HOP_COLS.0 };
                term.process(cup(prompt_row, col).as_bytes());
                host.glow.note_synthetic_move(now);
            }
            (None, _) => {}
        }
        Rig::extract(&mut term, &mut input);
        let caret = (
            u16::try_from(input.cursor_row).unwrap_or(u16::MAX),
            u16::try_from(input.cursor_col).unwrap_or(u16::MAX),
        );
        let (fp, emit) = host.emit(&term, caret, now);
        if event.is_none() && fp == host.last_fp {
            // Nothing moved: the app presents nothing.
            continue;
        }
        host.last_fp = fp;
        let splice_ns = host.splice(&mut input);
        samples.push(rig.present(&mut win, &input, emit, splice_ns));
    }
    if samples.iter().all(|s| s.fx_quads == 0) {
        fail("the cursor glow drew nothing under the typist");
    }
    rig.take_run(samples)
}

/// Inline streaming that scrolls a settled card up the screen, launch after launch.
fn scroll_scene(rig: &mut Rig) -> Run {
    let mut samples = Vec::new();
    let mut row = Vec::new();
    rig.refs.clear();
    for _ in 0..SCROLL_CYCLES {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        for i in 0..SCROLL_TOP - 2 {
            term.process(format!("{} {i}\r\n", &STREAMED[..60]).as_bytes());
        }
        term.process(b"$ claude");
        print_card(&mut term, SCROLL_TOP);
        term.process(cup(ROWS as u16 - 1, 0).as_bytes());
        let mut input = RenderInput::empty();
        let mut win = Win::new();
        let mut host = CardHost::default();
        let now = Instant::now();
        Rig::extract(&mut term, &mut input);
        let scene = rig.card_scene(SCROLL_TOP);
        let (replayed, _) = host.emit(now, &scene, true);
        if replayed || !host.drew_whole() {
            fail("the unarmed card's first frame was not drawn whole");
        }
        let _ = host.splice(&scene, rig.grid(), &mut input);
        let _ = rig.present(&mut win, &input, Spent::default(), 0);
        let mut top = SCROLL_TOP;
        let mut line = 0usize;
        while top > 1 {
            line += 1;
            term.process(format!("{STREAMED} {line}\r\n").as_bytes());
            top -= 1;
            Rig::extract(&mut term, &mut input);
            if !banner_at(&term, top, &mut row) {
                fail(format!(
                    "the streamed line did not scroll the card to row {top}"
                ));
            }
            let scene = rig.card_scene(top);
            let (replayed, emit) = host.emit(now, &scene, true);
            if !replayed {
                fail("a scrolled settled card was drawn again: it is the same frame moved");
            }
            let splice_ns = host.splice(&scene, rig.grid(), &mut input);
            samples.push(rig.present(&mut win, &input, emit, splice_ns));
        }
    }
    rig.take_run(samples)
}

/// Nearest-rank percentile of `v` (sorted in place); 0 for an empty set.
fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable_by(f64::total_cmp);
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    v[rank.clamp(1, v.len()) - 1]
}

/// A per-frame figure in nanoseconds (signed: a paired difference can be negative), `None`
/// where the frame has none (no GPU clock).
type Figure = fn(&Sample) -> Option<f64>;

/// The same for a reference frame.
type RefFigure = fn(&RefSample) -> Option<f64>;

fn ns(v: u64) -> f64 {
    v as f64
}

/// One scene's runs, folded into its report keys.
struct Scene {
    name: &'static str,
    runs: Vec<Run>,
    /// The producer is a replay: well under a microsecond and about 500 instructions, beside
    /// the counter's own read of about 6,200 it is taken from, so it is reported, not rated.
    rate_emit: bool,
}

/// The flat JSON report, in insertion order.
#[derive(Default)]
struct Report(Vec<(String, String)>);

impl Report {
    fn num(&mut self, key: impl Into<String>, v: f64) {
        self.0.push((key.into(), format!("{v:.3}")));
    }
    fn int(&mut self, key: impl Into<String>, v: u64) {
        self.0.push((key.into(), v.to_string()));
    }
    fn text(&mut self, key: impl Into<String>, v: &str) {
        self.0.push((key.into(), format!("\"{v}\"")));
    }
    fn get(&self, key: &str) -> &str {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map_or("-", |(_, v)| v.as_str())
    }
    fn json(&self) -> String {
        let mut s = String::from("{");
        for (i, (k, v)) in self.0.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "\"{k}\":{v}");
        }
        s.push('}');
        s
    }
}

impl Scene {
    /// A counted report describes every frame, not just the counter reads
    /// that happened to succeed.
    fn require_cpu_samples(&self) -> Result<(), String> {
        for (run_index, run) in self.runs.iter().enumerate() {
            let frames_complete = run.samples.iter().all(|sample| {
                sample.emit.ins.is_some()
                    && sample.raster.ins.is_some()
                    && sample.arms.iter().all(|arm| arm.call_ins.is_some())
            });
            let refs_complete = run
                .refs
                .iter()
                .all(|sample| sample.raster.ins.is_some() && sample.call_ins.is_some());
            if !frames_complete || !refs_complete {
                return Err(format!(
                    "{} run {} has incomplete CPU instruction samples; refusing a partial report",
                    self.name,
                    run_index + 1,
                ));
            }
        }
        Ok(())
    }

    fn pooled(&self, f: Figure) -> Vec<f64> {
        self.runs
            .iter()
            .flat_map(|run| &run.samples)
            .filter_map(f)
            .collect()
    }

    /// Each run's p50 of `f`.
    fn run_p50s(&self, f: Figure) -> Vec<f64> {
        self.runs
            .iter()
            .map(|run| {
                pct(
                    &mut run.samples.iter().filter_map(f).collect::<Vec<_>>(),
                    50.0,
                )
            })
            .collect()
    }

    /// Each run's reference p50 over its scene p50 of `f`; `None` for a run with no reference
    /// frame or no reading.
    fn run_rels(&self, f: Figure, reference: RefFigure) -> Vec<Option<f64>> {
        self.runs
            .iter()
            .map(|run| {
                let mut refs: Vec<f64> = run.refs.iter().filter_map(reference).collect();
                let mut scene: Vec<f64> = run.samples.iter().filter_map(f).collect();
                (!refs.is_empty() && !scene.is_empty())
                    .then(|| pct(&mut refs, 50.0) / pct(&mut scene, 50.0).max(1.0))
            })
            .collect()
    }

    fn report(&self, r: &mut Report, clocked: bool, counted: bool) {
        let s = self.name;
        let us = |v: f64| v / 1_000.0;
        r.int(
            format!("{s}_n"),
            self.runs.iter().map(|run| run.samples.len()).sum::<usize>() as u64,
        );
        let timed: [(&str, Figure); 16] = [
            ("emit", |x| Some(ns(x.emit.wall_ns))),
            ("emit_cpu", |x| Some(ns(x.emit.cpu_ns))),
            ("splice", |x| Some(ns(x.splice_ns))),
            ("raster", |x| Some(ns(x.raster.wall_ns))),
            ("raster_cpu", |x| Some(ns(x.raster.cpu_ns))),
            ("encode", |x| Some(ns(x.off().encode_ns))),
            ("encode_cpu", |x| Some(ns(x.off().call_cpu_ns))),
            ("build", |x| Some(ns(x.off().shape.build_ns))),
            ("stage", |x| Some(ns(x.off().shape.stage_ns))),
            ("gpu", |x| x.off().gpu_ns.map(ns)),
            ("present", |x| Some(ns(x.off().present_ns()))),
            ("thin_encode", |x| Some(ns(x.thin().encode_ns))),
            ("thin_gpu", |x| x.thin().gpu_ns.map(ns)),
            ("drop_encode", |x| Some(ns(x.drop().encode_ns))),
            ("drop_gpu", |x| x.drop().gpu_ns.map(ns)),
            ("pass_encode", |x| {
                Some(ns(x.thin().encode_ns) - ns(x.drop().encode_ns))
            }),
        ];
        for (what, f) in timed {
            if !clocked && (what.ends_with("gpu") || what == "present") {
                continue;
            }
            let mut v = self.pooled(f);
            r.num(format!("{s}_{what}_p50_us"), us(pct(&mut v, 50.0)));
            r.num(format!("{s}_{what}_p95_us"), us(pct(&mut v, 95.0)));
        }
        let mut present = self.pooled(|x| Some(ns(x.off().present_ns())));
        let present_p95 = pct(&mut present, 95.0).max(1.0);
        if clocked {
            let paired: [(&str, Figure); 3] = [
                ("pass_gpu", |x| {
                    Some(ns(x.thin().gpu_ns?) - ns(x.drop().gpu_ns?))
                }),
                ("pass_cost", |x| {
                    Some(ns(x.thin().present_ns()) - ns(x.drop().present_ns()))
                }),
                ("unorm_draw", |x| {
                    Some(ns(x.off().present_ns()) - ns(x.thin().present_ns()))
                }),
            ];
            for (what, f) in paired {
                let mut v = self.pooled(f);
                r.num(format!("{s}_{what}_p50_us"), us(pct(&mut v, 50.0)));
                r.num(format!("{s}_{what}_p95_us"), us(pct(&mut v, 95.0)));
                if what == "pass_cost" {
                    let cost = pct(&mut v, 50.0);
                    r.num(format!("{s}_pass_cost_pct"), 100.0 * cost / present_p95);
                    let present_p50 = pct(&mut present, 50.0).max(1.0);
                    r.num(format!("{s}_pass_cost_pct_p50"), 100.0 * cost / present_p50);
                }
            }
            let mut clock =
                self.pooled(|x| Some(ns(x.off().encode_ns) - ns(x.untimed().encode_ns)));
            let clock_p50 = pct(&mut clock, 50.0);
            r.num(format!("{s}_clock_encode_p50_us"), us(clock_p50));
            r.num(
                format!("{s}_clock_encode_p95_us"),
                us(pct(&mut clock, 95.0)),
            );
            r.num(format!("{s}_clock_pct"), 100.0 * clock_p50 / present_p95);
            let mut untimed = self.pooled(|x| Some(ns(x.untimed().encode_ns)));
            r.num(
                format!("{s}_untimed_encode_p50_us"),
                us(pct(&mut untimed, 50.0)),
            );
            r.num(
                format!("{s}_untimed_encode_p95_us"),
                us(pct(&mut untimed, 95.0)),
            );
        }
        let mut build_stage =
            self.pooled(|x| Some(ns(x.off().shape.build_ns + x.off().shape.stage_ns)));
        r.num(
            format!("{s}_build_stage_pct"),
            100.0 * pct(&mut build_stage, 50.0) / present_p95,
        );
        let counts: [(&str, Figure); 9] = [
            ("passes", |x| Some(f64::from(x.off().shape.passes))),
            ("drop_passes", |x| Some(f64::from(x.drop().shape.passes))),
            (
                "instances",
                |x| Some(x.off().shape.total_instances() as f64),
            ),
            ("unorm_instances", |x| {
                Some(ns(x.off().shape.unorm_instances))
            }),
            ("instance_bytes", |x| Some(ns(x.off().shape.instance_bytes))),
            ("fx_quads", |x| Some(ns(x.fx_quads))),
            ("under_quads", |x| Some(ns(x.under_quads))),
            ("fx_instances", |x| {
                Some(f64::from(x.off().shape.fx_instances))
            }),
            ("fx_upload_bytes", |x| {
                Some(ns(x.off().shape.fx_upload_bytes))
            }),
        ];
        for (what, f) in counts {
            let mut v = self.pooled(f);
            r.int(format!("{s}_{what}_p50"), pct(&mut v, 50.0) as u64);
            r.int(format!("{s}_{what}_max"), pct(&mut v, 100.0) as u64);
        }
        let rated: [(&str, Figure, RefFigure); 4] = [
            (
                "emit",
                |x| Some(ns(x.emit.cpu_ns)),
                |r| Some(ns(r.raster.cpu_ns)),
            ),
            (
                "raster",
                |x| Some(ns(x.raster.cpu_ns)),
                |r| Some(ns(r.raster.cpu_ns)),
            ),
            (
                "encode",
                |x| Some(ns(x.off().call_cpu_ns)),
                |r| Some(ns(r.call_cpu_ns)),
            ),
            ("gpu", |x| x.off().gpu_ns.map(ns), |r| r.gpu_ns.map(ns)),
        ];
        for (what, f, reference) in rated {
            if !clocked && what == "gpu" {
                continue;
            }
            let p50s = self.run_p50s(f);
            let fastest = p50s.iter().copied().fold(f64::INFINITY, f64::min).max(1.0);
            let slowest = p50s.iter().copied().fold(0.0, f64::max);
            r.num(format!("{s}_{what}_spread"), slowest / fastest);
            if what == "emit" && !self.rate_emit {
                continue;
            }
            let Some(mut rels) = self
                .run_rels(f, reference)
                .into_iter()
                .collect::<Option<Vec<f64>>>()
            else {
                fail(format!(
                    "a {s} run drew no reference frame to calibrate {what} against"
                ));
            };
            r.num(format!("{s}_{what}_rel"), pct(&mut rels, 50.0));
            let lo = rels.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = rels.iter().copied().fold(0.0, f64::max);
            r.num(
                format!("{s}_{what}_rel_spread"),
                hi / lo.max(f64::MIN_POSITIVE),
            );
        }
        if !counted {
            return;
        }
        let work: [(&str, Figure); 3] = [
            ("emit", |x| x.emit.ins.map(ns)),
            ("raster", |x| x.raster.ins.map(ns)),
            ("encode", |x| x.off().call_ins.map(ns)),
        ];
        for (what, f) in work {
            let mut v = self.pooled(f);
            if v.is_empty() {
                fail(format!("the {s} frames counted no {what} instructions"));
            }
            let p50 = pct(&mut v, 50.0);
            r.num(format!("{s}_{what}_kins_p50"), p50 / 1_000.0);
            r.num(format!("{s}_{what}_kins_p95"), pct(&mut v, 95.0) / 1_000.0);
            let p50s = self.run_p50s(f);
            let fastest = p50s.iter().copied().fold(f64::INFINITY, f64::min).max(1.0);
            let slowest = p50s.iter().copied().fold(0.0, f64::max);
            r.num(format!("{s}_{what}_ins_spread"), slowest / fastest);
            if what == "emit" && !self.rate_emit {
                continue;
            }
            r.num(format!("{s}_{what}_per_ginsn"), 1e9 / p50.max(1.0));
        }
        if clocked {
            let mut clock =
                self.pooled(|x| Some(ns(x.off().call_ins?) - ns(x.untimed().call_ins?)));
            r.num(
                format!("{s}_clock_kins_p50"),
                pct(&mut clock, 50.0) / 1_000.0,
            );
            r.num(
                format!("{s}_clock_kins_p95"),
                pct(&mut clock, 95.0) / 1_000.0,
            );
        }
    }
}

/// The reference frames' own costs, pooled over every scene and run: what the desk was doing.
fn report_reference(r: &mut Report, scenes: &[Scene], clocked: bool, counted: bool) {
    let all = || {
        scenes
            .iter()
            .flat_map(|scene| &scene.runs)
            .flat_map(|run| &run.refs)
    };
    let us = |v: f64| v / 1_000.0;
    r.int("ref_n", all().count() as u64);
    let figures: [(&str, RefFigure); 5] = [
        ("raster", |x| Some(ns(x.raster.wall_ns))),
        ("raster_cpu", |x| Some(ns(x.raster.cpu_ns))),
        ("encode", |x| Some(ns(x.encode_ns))),
        ("encode_cpu", |x| Some(ns(x.call_cpu_ns))),
        ("gpu", |x| x.gpu_ns.map(ns)),
    ];
    for (what, f) in figures {
        if !clocked && what == "gpu" {
            continue;
        }
        let mut v: Vec<f64> = all().filter_map(f).collect();
        r.num(format!("ref_{what}_p50_us"), us(pct(&mut v, 50.0)));
        r.num(format!("ref_{what}_p95_us"), us(pct(&mut v, 95.0)));
    }
    if counted {
        let work: [(&str, RefFigure); 2] = [
            ("raster", |x| x.raster.ins.map(ns)),
            ("encode", |x| x.call_ins.map(ns)),
        ];
        for (what, f) in work {
            let mut v: Vec<f64> = all().filter_map(f).collect();
            r.num(format!("ref_{what}_kins_p50"), pct(&mut v, 50.0) / 1_000.0);
        }
    }
    if !clocked {
        return;
    }
    let mut blank: Vec<f64> = all().filter_map(|x| x.blank_gpu_ns.map(ns)).collect();
    r.num("ref_blank_gpu_p50_us", us(pct(&mut blank, 50.0)));
    r.num("ref_blank_gpu_p95_us", us(pct(&mut blank, 95.0)));
    let mut rels: Vec<f64> = scenes
        .iter()
        .flat_map(|scene| &scene.runs)
        .filter_map(|run| {
            let mut text: Vec<f64> = run.refs.iter().filter_map(|x| x.gpu_ns.map(ns)).collect();
            let mut blank: Vec<f64> = run
                .refs
                .iter()
                .filter_map(|x| x.blank_gpu_ns.map(ns))
                .collect();
            (!text.is_empty() && !blank.is_empty())
                .then(|| pct(&mut blank, 50.0) / pct(&mut text, 50.0).max(1.0))
        })
        .collect();
    if rels.is_empty() {
        fail("no run drew a reference frame to calibrate the reference against");
    }
    r.num("ref_text_gpu_rel", pct(&mut rels, 50.0));
    let lo = rels.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = rels.iter().copied().fold(0.0, f64::max);
    r.num("ref_text_gpu_rel_spread", hi / lo.max(f64::MIN_POSITIVE));
}

fn main() {
    if !cfg!(any(debug_assertions, feature = "dev-seams")) {
        fail(NO_SEAM);
    }
    fn noop(_: gpu_time::GpuFrameTime) {}
    if !gpu_time::install_sink(noop) && !gpu_time::sink_installed() {
        fail("could not install the GPU-time sink");
    }
    aterm_pty::declare_interactive_thread();
    let mut rig = Rig::new();
    let clocked = rig.clock != "none";
    let counted = rig.counter != "none";
    eprintln!(
        "effects_harness: {}x{} cells of {}x{} px ({}x{} frame), GPU clock {}, CPU counter {}",
        COLS, ROWS, rig.cw, rig.ch, rig.frame_w, rig.frame_h, rig.clock, rig.counter
    );
    let scene = |name, rate_emit| Scene {
        name,
        runs: Vec::new(),
        rate_emit,
    };
    let mut scenes = [
        scene("episode", true),
        scene("flowing", true),
        scene("settled", false),
        scene("typing", true),
        scene("scroll", true),
        scene("bolt", true),
        scene("comet", true),
        scene("classic", true),
        scene("meteor", true),
        scene("flood", true),
    ];
    for run in 0..WARMUP + RUNS {
        let (episode, flowing, settled) = card_scenes(&mut rig, 0x00C0_FFEE + run as u64);
        let typed = |keys| Script::Type {
            keys,
            every: KEY_TICKS,
        };
        let typing = typing_scene(&mut rig, GlowStyle::RainbowKitty, typed(TYPING_KEYS));
        let scroll = scroll_scene(&mut rig);
        let bolt = typing_scene(&mut rig, GlowStyle::Laser, typed(PRODUCER_KEYS));
        let comet = typing_scene(&mut rig, GlowStyle::Comet, typed(PRODUCER_KEYS));
        let classic = typing_scene(&mut rig, GlowStyle::Classic, typed(PRODUCER_KEYS));
        let hops = Script::Hop {
            hops: HOPS,
            every: HOP_TICKS,
        };
        let meteor = typing_scene(&mut rig, GlowStyle::RainbowKitty, hops);
        let flood = Script::Type {
            keys: FLOOD_KEYS,
            every: 1,
        };
        let flood = typing_scene(&mut rig, GlowStyle::RainbowKitty, flood);
        at_the_ribbon_budget(&flood);
        if run < WARMUP {
            continue;
        }
        let runs = [
            episode, flowing, settled, typing, scroll, bolt, comet, classic, meteor, flood,
        ];
        for (scene, run) in scenes.iter_mut().zip(runs) {
            scene.runs.push(run);
        }
        eprintln!("effects_harness: run {}/{RUNS} done", run + 1 - WARMUP);
    }
    if counted {
        for scene in &scenes {
            scene.require_cpu_samples().unwrap_or_else(|why| fail(why));
        }
    }
    let mut report = Report::default();
    report.text("gpu_clock", rig.clock);
    report.text("cpu_clock", if cfg!(unix) { "thread" } else { "wall" });
    report.text("cpu_counter", rig.counter);
    report.int("runs", RUNS as u64);
    report.int("warmup", WARMUP as u64);
    report.int("rows", ROWS as u64);
    report.int("cols", COLS as u64);
    report.int("cell_w", u64::from(rig.cw));
    report.int("cell_h", u64::from(rig.ch));
    report.int("frame_w", rig.frame_w as u64);
    report.int("frame_h", rig.frame_h as u64);
    report.int("ref_every", REF_EVERY as u64);
    report_reference(&mut report, &scenes, clocked, counted);
    eprintln!(
        "effects_harness: reference n={} raster {} us | encode {} us | gpu {} us (p50) | blank \
         gpu {} us | text rel {}",
        report.get("ref_n"),
        report.get("ref_raster_p50_us"),
        report.get("ref_encode_p50_us"),
        report.get("ref_gpu_p50_us"),
        report.get("ref_blank_gpu_p50_us"),
        report.get("ref_text_gpu_rel"),
    );
    for scene in &scenes {
        scene.report(&mut report, clocked, counted);
        let get = |k: &str| report.get(&format!("{}_{k}", scene.name)).to_string();
        eprintln!(
            "effects_harness: {:<8} n={} emit {} us | raster {} us | encode {}/{} us | gpu {}/{} us \
             | passes {} (drop {}) | pass cost {} us = {}% of present p95 {} us | unorm draws {} us \
             | build+stage {}% | instances max {} (under {}) | clock {} us, {} kins \
             | rel emit {} raster {} encode {} gpu {} | kins emit {} raster {} encode {}",
            scene.name,
            get("n"),
            get("emit_p50_us"),
            get("raster_p50_us"),
            get("encode_p50_us"),
            get("encode_p95_us"),
            get("gpu_p50_us"),
            get("gpu_p95_us"),
            get("passes_max"),
            get("drop_passes_max"),
            get("pass_cost_p50_us"),
            get("pass_cost_pct"),
            get("present_p95_us"),
            get("unorm_draw_p50_us"),
            get("build_stage_pct"),
            get("instances_max"),
            get("under_quads_max"),
            get("clock_encode_p50_us"),
            get("clock_kins_p50"),
            get("emit_rel"),
            get("raster_rel"),
            get("encode_rel"),
            get("gpu_rel"),
            get("emit_kins_p50"),
            get("raster_kins_p50"),
            get("encode_kins_p50"),
        );
    }
    println!("{}", report.json());
}
