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
// `dev-seams` compiles in the pass-economy ablation (`GpuRenderer::set_unorm_ablation`); a
// release build without it refuses to run rather than report half a lane.
//
// The scenes, at the desk's cell (JetBrains Mono at 30 px: 18x40) on a 26x190 grid (the
// 3420x1040 drawable §5's pass costs are stated at), on the shipped dark theme with bloom and
// shimmer on:
//   episode  the harness card's arm-to-settle episode (`HarnessCard::draw_kept` on its 33 ms
//            clock, the ride-in included), every frame the card asks for, the settled one last;
//   settled  the settled card replayed while a key a frame lands in the composer under it;
//   typing   the shipped cursor glow (Rainbow Kitty) ticked at 120 Hz while 12 keys a second
//            land at a shell prompt, then its run-out; a tick whose fingerprint did not move
//            presents nothing, as in the app;
//   scroll   inline streaming under a settled card: a line a frame scrolls the screen and the
//            card with it (today a settled redraw, and a full repaint: a frame carrying card
//            light refuses the scroll blit).
//
// Per frame, each on its own clock:
//   emit     the producer: the card's replay or draw, or the glow's witness feed and tick;
//   splice   the host's copy of the produced quads into the frame's streams (the kitty's
//            nova shift included) and the effect-damage refresh;
//   raster   the CPU presenter: `Renderer::render_input_cached` on the frame;
//   encode   the GPU presenter's present work up to its last commit
//            (`WindowGpu::last_present_encode_ns`), with its `build` (instances and atlas) and
//            `stage` (instance upload) parts from the frame shape;
//   gpu      the device's time for that present (`aterm_gpu::gpu_time`: Metal command-buffer
//            clocks, wgpu timestamp queries; absent, with `gpu_clock` "none", on an arm with no
//            clock); `present` is encode + gpu.
//
// The GPU presenter draws every frame three times, each on its own window of the one renderer
// and in an order that rotates frame by frame, under the pass-economy seam
// (`aterm_gpu::UnormAblation`): `off` (shipped), `thin` (every pass of the plan, each Unorm
// item drawing one instance) and `drop` (no Unorm group: one pass). Paired per frame,
// `pass_cost` = thin - drop is what the extra passes themselves cost, the figure the B6 go rule
// reads as `pass_cost_pct` (its p50 over the present's p95), and `unorm_draw` = off - thin is
// what the One/One draws cost, which merging the passes would not save. `build_stage_pct`
// (instance build and upload, p50 over the present's p95) is the B5 rule's.
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
//           small kernels do not).
//   work    `_kins` is the instructions the process retired per frame in the producer, the CPU
//           raster and the GPU present call (`cpu_counter` "instructions": Apple's
//           `proc_pid_rusage`, the read's own cost taken out; "none" elsewhere), and
//           `_per_ginsn` (frames per 10^9 instructions, bigger is cheaper) its gated form. The
//           desk's load does not move it.
// Which keys gate is the `effects` lane's call (`aterm_verify::perf::floors::EFFECTS_LANE`). The
// `_us` figures are the record, and the reference's own (`ref_*`) say what the desk was doing.
//
// Fail closed: a refused present, a GPU frame that fails or never settles, an ablation the plan
// did not take (or that changed the instances built), or a scene that does not do what it says
// ends the run with exit 1.

use std::fmt::Write as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CURSOR_WITNESS_ROWS, CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::harness_card::{
    CardPalette, CardScene, EPISODE_MS, FRAME_MS, HarnessCard, SettledFrame, dark_ground,
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
/// The glow's frame clock and the typist's pace: a key every 10 ticks at 120 Hz is 12 a
/// second (the Rainbow Kitty frame-cost gate's prose pace), then a second of run-out.
const TYPING_HZ: u32 = 120;
const TYPING_KEYS: usize = 48;
const KEY_TICKS: usize = 10;
const RUNOUT_TICKS: usize = 120;
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
/// `proc_pid_rusage` (`ri_instructions`, read from the cores' fixed counters). The work a
/// stretch of code does, which the desk's load does not move the way it moves time.
#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    let pid = libc::c_int::try_from(std::process::id()).ok()?;
    // SAFETY: `info` is writable for one `rusage_info_v4`, the flavor asked for; the call fills
    // it or fails, and a zeroed one is a valid value either way.
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
/// every reading. Set once by [`calibrate_counter`].
static COUNTER_OVERHEAD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Whether the instruction counter counts here: it reads, and a loop of known length moves it
/// (a counter that reads a constant would report every frame as free). Measures the
/// stopwatch's own reads on the way.
fn calibrate_counter() -> bool {
    // Uncalibrated, `stop` takes nothing out: an empty stretch reads the reads themselves.
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

/// The three plans every frame is drawn under, in [`Sample::arms`] order.
const MODES: [UnormAblation; 3] = [UnormAblation::Off, UnormAblation::Thin, UnormAblation::Drop];

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
    /// The GPU presents, indexed like [`MODES`].
    arms: [Arm; 3],
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
}

/// One reference frame's costs, the calibration the `_rel` keys divide by.
#[derive(Clone, Copy, Debug, Default)]
struct RefSample {
    raster: Spent,
    encode_ns: u64,
    call_cpu_ns: u64,
    call_ins: Option<u64>,
    gpu_ns: Option<u64>,
}

/// One scene's frames in one run, and the reference frames drawn among them.
#[derive(Debug, Default)]
struct Run {
    samples: Vec<Sample>,
    refs: Vec<RefSample>,
}

/// The calibration: two screens of plain text, each differing from the other in every row, so
/// each reference frame is a full repaint, on windows of their own.
struct Reference {
    inputs: [RenderInput; 2],
    cpu: WindowCpu,
    gpu: WindowGpu,
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
        Self {
            inputs: [screen(0), screen(1)],
            cpu: WindowCpu::new(),
            gpu: WindowGpu::new(),
            next: 0,
        }
    }
}

/// One scene's windows: the CPU presenter's, and one GPU window per plan in [`MODES`].
struct Win {
    cpu: WindowCpu,
    gpu: [WindowGpu; 3],
}

impl Win {
    fn new() -> Self {
        Self {
            cpu: WindowCpu::new(),
            gpu: [WindowGpu::new(), WindowGpu::new(), WindowGpu::new()],
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
        // The shipped rendering defaults.
        gpu.set_bloom(true);
        gpu.set_shimmer(true);
        gpu.debug_block_on_lazy_fallbacks();
        let (cw, ch) = cpu.cell_size();
        if gpu.cell_size() != (cw, ch) {
            fail("the two presenters disagree on the cell");
        }
        let (frame_w, frame_h) = cpu.frame_size(ROWS, COLS);
        let clock = if !gpu.gpu_time_supported() {
            "none"
        } else if cfg!(target_os = "macos") {
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
            // The pane's ground is the theme's background, read as the app reads it.
            dark: dark_ground(self.theme.bg),
            ground: self.theme.bg,
            serious: false,
            coat: 0x00E8_A060,
            eye: 0x0040_C080,
        }
    }

    /// The nova stream's shift: its pixels are grid-interior.
    fn nova_shift(&self) -> (u16, u16) {
        (self.pad, self.grid_top)
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

        let mut arms = [Arm::default(); 3];
        // The order rotates so no plan always runs first, on a GPU the CPU raster left idle.
        for k in 0..MODES.len() {
            let i = (k + self.frames) % MODES.len();
            set_ablation(&mut self.gpu, MODES[i]);
            let watch = Stopwatch::start();
            if !self
                .gpu
                .present_virtual(&mut win.gpu[i], input, false, None, None)
            {
                fail(format!(
                    "the GPU presenter refused a frame ({})",
                    MODES[i].name()
                ));
            }
            let call = watch.stop();
            let shape = self.gpu.frame_shape();
            if shape.ablation != MODES[i] {
                fail(format!(
                    "the frame plan did not take the {} ablation",
                    MODES[i].name()
                ));
            }
            arms[i] = Arm {
                encode_ns: win.gpu[i].last_present_encode_ns(),
                call_cpu_ns: call.cpu_ns,
                call_ins: call.ins,
                gpu_ns: self.gpu_time(),
                shape,
            };
        }
        set_ablation(&mut self.gpu, UnormAblation::Off);
        self.frames += 1;
        if self.frames.is_multiple_of(REF_EVERY) {
            let reference = self.reference_frame();
            self.refs.push(reference);
        }
        let [off, thin, dropped] = &arms;
        if thin.shape.passes != off.shape.passes || dropped.shape.passes > off.shape.passes {
            fail(format!(
                "the ablated plans' passes are off: {} shipped, {} thinned, {} dropped",
                off.shape.passes, thin.shape.passes, dropped.shape.passes
            ));
        }
        if thin.shape.instances != off.shape.instances
            || dropped.shape.instances != off.shape.instances
        {
            fail("an ablated plan built other instances than the shipped one");
        }
        Sample {
            emit,
            splice_ns,
            raster,
            fx_quads: fx_quads(input),
            arms,
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
        RefSample {
            raster,
            encode_ns: reference.gpu.last_present_encode_ns(),
            call_cpu_ns: call.cpu_ns,
            call_ins: call.ins,
            gpu_ns: self.gpu_time(),
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

/// The card's host: the app's replay-or-draw (`claude_card.rs`), the kept settled frame, and
/// the nova stream's shifted copy of the kitty and stars.
#[derive(Default)]
struct CardHost {
    card: HarnessCard,
    kept: SettledFrame,
    under: Vec<GlowQuad>,
    over: Vec<GlowQuad>,
    nova: Vec<GlowQuad>,
    /// `nova` is the shifted copy of `kept.over()`.
    nova_kept: bool,
}

impl CardHost {
    /// The producer: replayed (`true`) when the card holds its kept settled frame for this very
    /// scene, else drawn on `now`.
    fn emit(&mut self, now: Instant, scene: &CardScene) -> (bool, Spent) {
        let watch = Stopwatch::start();
        let replayed = self.card.replay(scene, true, &self.kept);
        if !replayed {
            self.under.clear();
            self.over.clear();
            self.card.draw_kept(
                now,
                scene,
                true,
                &mut self.kept,
                &mut self.under,
                &mut self.over,
            );
        }
        (replayed, watch.stop())
    }

    /// The splice: the border and trail into `glow_under` (window pixels), the kitty and stars
    /// shifted into `nova_add` (grid-interior pixels).
    fn splice(&mut self, replayed: bool, shift: (u16, u16), input: &mut RenderInput) -> u64 {
        let started = Instant::now();
        clear_streams(input);
        if replayed {
            if !self.nova_kept {
                nova_of(self.kept.over(), shift, &mut self.nova);
                self.nova_kept = true;
            }
            input.glow_under.extend_from_slice(self.kept.under());
        } else {
            nova_of(&self.over, shift, &mut self.nova);
            self.nova_kept = !self.kept.over().is_empty();
            input.glow_under.extend_from_slice(&self.under);
        }
        input.nova_add.extend_from_slice(&self.nova);
        input.refresh_cursor_effect_damage();
        elapsed_ns(started)
    }
}

/// `claude_card::nova_of`: window pixels to grid-interior ones, a quad left of or above the
/// grid dropped.
fn nova_of(over: &[GlowQuad], shift: (u16, u16), out: &mut Vec<GlowQuad>) {
    out.clear();
    out.extend(over.iter().filter_map(|quad| {
        Some(GlowQuad {
            x: quad.x.checked_sub(shift.0)?,
            y: quad.y.checked_sub(shift.1)?,
            ..*quad
        })
    }));
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
    fn new(rig: &Rig) -> Self {
        Self {
            glow: CursorGlow::default(),
            // The shipped dark-theme Rainbow Kitty, as the frame-cost gate builds it, on this
            // theme's colours.
            cfg: GlowConfig {
                classic_mono: false,
                ribbon_tall: true,
                ribbon_flat: false,
                enabled: true,
                dark_theme: true,
                theme_fg: rig.theme.fg,
                theme_bg: rig.theme.bg,
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
/// box, then the air).
fn print_card(term: &mut Terminal, top: u16) {
    for (i, line) in CARD.iter().enumerate() {
        term.process(format!("{}{line}", cup(top + i as u16, 0)).as_bytes());
    }
}

/// Whether grid row `row` of `term` shows the card's banner.
fn banner_at(term: &Terminal, row: u16, scratch: &mut Vec<char>) -> bool {
    term.row_cols_into(usize::from(row), scratch);
    let banner: Vec<char> = CARD[0].chars().collect();
    scratch.len() >= banner.len() && scratch[..banner.len()] == banner[..]
}

/// The episode, then the settled card under typing, on one launch.
fn card_scenes(rig: &mut Rig, seed: u64) -> (Run, Run) {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ claude");
    print_card(&mut term, CARD_TOP);
    // The composer under the card, the caret in it.
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
        let (replayed, emit) = host.emit(now, &scene);
        if replayed {
            fail("an episode frame replayed: the card was not armed");
        }
        let splice_ns = host.splice(replayed, rig.nova_shift(), &mut input);
        episode.push(rig.present(&mut win, &input, emit, splice_ns));
        match host.card.deadline(now) {
            Some(next) if episode.len() < most => now = next,
            Some(_) => fail(format!("the episode still asks for frames after {most}")),
            None => break,
        }
    }
    if host.kept.under().is_empty() || host.kept.over().is_empty() {
        fail("the episode ended without a settled frame");
    }
    let episode = rig.take_run(episode);

    let mut settled = Vec::with_capacity(SETTLED_KEYS);
    for key in 0..SETTLED_KEYS {
        term.process(&[PROSE[key % PROSE.len()]]);
        Rig::extract(&mut term, &mut input);
        let (replayed, emit) = host.emit(now, &scene);
        if !replayed {
            fail("the settled card was drawn again rather than replayed");
        }
        let splice_ns = host.splice(replayed, rig.nova_shift(), &mut input);
        settled.push(rig.present(&mut win, &input, emit, splice_ns));
    }
    (episode, rig.take_run(settled))
}

/// The cursor glow under a typist at a shell prompt.
fn typing_scene(rig: &mut Rig) -> Run {
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

    let mut host = GlowHost::new(rig);
    let period = Duration::from_secs(1) / TYPING_HZ;
    let t0 = Instant::now();
    let mut samples = Vec::new();
    for tick in 0..TYPING_KEYS * KEY_TICKS + RUNOUT_TICKS {
        let now = t0 + period * tick as u32;
        let key =
            (tick % KEY_TICKS == 0 && tick / KEY_TICKS < TYPING_KEYS).then_some(tick / KEY_TICKS);
        if let Some(k) = key {
            term.process(&[PROSE[k % PROSE.len()]]);
            host.glow.note_synthetic_typed(now, 1);
        }
        Rig::extract(&mut term, &mut input);
        let caret = (
            u16::try_from(input.cursor_row).unwrap_or(u16::MAX),
            u16::try_from(input.cursor_col).unwrap_or(u16::MAX),
        );
        let (fp, emit) = host.emit(&term, caret, now);
        if key.is_none() && fp == host.last_fp {
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
        // The caret on the last row: every streamed line scrolls the screen by one.
        term.process(cup(ROWS as u16 - 1, 0).as_bytes());
        let mut input = RenderInput::empty();
        let mut win = Win::new();
        // An unarmed card draws its settled frame: the launch's episode has played out.
        let mut host = CardHost::default();
        let now = Instant::now();
        Rig::extract(&mut term, &mut input);
        let (replayed, _) = host.emit(now, &rig.card_scene(SCROLL_TOP));
        let _ = host.splice(replayed, rig.nova_shift(), &mut input);
        let _ = rig.present(&mut win, &input, Spent::default(), 0);
        let mut top = SCROLL_TOP;
        while top > 1 {
            term.process(format!("{STREAMED}\r\n").as_bytes());
            top -= 1;
            Rig::extract(&mut term, &mut input);
            if !banner_at(&term, top, &mut row) {
                fail(format!(
                    "the streamed line did not scroll the card to row {top}"
                ));
            }
            let (replayed, emit) = host.emit(now, &rig.card_scene(top));
            if replayed {
                fail("a scrolled card replayed: its scene did not move");
            }
            let splice_ns = host.splice(replayed, rig.nova_shift(), &mut input);
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
    /// that happened to succeed. Check before printing any report keys.
    fn require_cpu_samples(&self) -> Result<(), String> {
        for (run_index, run) in self.runs.iter().enumerate() {
            let frames_complete = run.samples.iter().all(|sample| {
                sample.emit.ins.is_some()
                    && sample.raster.ins.is_some()
                    && sample.off().call_ins.is_some()
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
        // The timed costs, p50 and p95 in microseconds; `gpu` ones only on a clocked arm.
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
            // Paired per frame: the extra passes' own encode.
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
        // The pass economy, paired per frame (encode + GPU): what the passes cost, and what the
        // One/One draws in them cost.
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
                    // Over the p50 too: a loaded desk's preemption lands in the p95.
                    let present_p50 = pct(&mut present, 50.0).max(1.0);
                    r.num(format!("{s}_pass_cost_pct_p50"), 100.0 * cost / present_p50);
                }
            }
        }
        let mut build_stage =
            self.pooled(|x| Some(ns(x.off().shape.build_ns + x.off().shape.stage_ns)));
        r.num(
            format!("{s}_build_stage_pct"),
            100.0 * pct(&mut build_stage, 50.0) / present_p95,
        );
        // The frame's shape.
        let counts: [(&str, Figure); 5] = [
            ("passes", |x| Some(f64::from(x.off().shape.passes))),
            ("drop_passes", |x| Some(f64::from(x.drop().shape.passes))),
            (
                "instances",
                |x| Some(x.off().shape.total_instances() as f64),
            ),
            ("instance_bytes", |x| Some(ns(x.off().shape.instance_bytes))),
            ("fx_quads", |x| Some(ns(x.fx_quads))),
        ];
        for (what, f) in counts {
            let mut v = self.pooled(f);
            r.int(format!("{s}_{what}_p50"), pct(&mut v, 50.0) as u64);
            r.int(format!("{s}_{what}_max"), pct(&mut v, 100.0) as u64);
        }
        // Run-to-run spread, and the gated keys: the scene against the reference frames drawn
        // among its own, per run, the median run.
        // The CPU figures are thread CPU time (the core's work, not the scheduler's), the GPU's
        // the device clock.
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
        // The work: instructions retired per frame, which the desk's load does not move the
        // way it moves time. `_per_ginsn` (frames per 10^9 instructions, bigger is cheaper) is
        // the gated form of the p50.
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
}

fn main() {
    if !cfg!(any(debug_assertions, feature = "dev-seams")) {
        fail(NO_SEAM);
    }
    fn noop(_: gpu_time::GpuFrameTime) {}
    if !gpu_time::install_sink(noop) && !gpu_time::sink_installed() {
        fail("could not install the GPU-time sink");
    }
    // The app renders on a user-interactive thread; a default-QoS harness on a busy machine is
    // the first thread the scheduler moves to an efficiency core.
    aterm_pty::declare_interactive_thread();
    let mut rig = Rig::new();
    let clocked = rig.clock != "none";
    let counted = rig.counter != "none";
    eprintln!(
        "effects_harness: {}x{} cells of {}x{} px ({}x{} frame), GPU clock {}, CPU counter {}",
        COLS, ROWS, rig.cw, rig.ch, rig.frame_w, rig.frame_h, rig.clock, rig.counter
    );
    let mut scenes = [
        Scene {
            name: "episode",
            runs: Vec::new(),
            rate_emit: true,
        },
        Scene {
            name: "settled",
            runs: Vec::new(),
            rate_emit: false,
        },
        Scene {
            name: "typing",
            runs: Vec::new(),
            rate_emit: true,
        },
        Scene {
            name: "scroll",
            runs: Vec::new(),
            rate_emit: true,
        },
    ];
    for run in 0..WARMUP + RUNS {
        let (episode, settled) = card_scenes(&mut rig, 0x00C0_FFEE + run as u64);
        let typing = typing_scene(&mut rig);
        let scroll = scroll_scene(&mut rig);
        if run < WARMUP {
            continue;
        }
        for (scene, run) in scenes.iter_mut().zip([episode, settled, typing, scroll]) {
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
        "effects_harness: reference n={} raster {} us | encode {} us | gpu {} us (p50)",
        report.get("ref_n"),
        report.get("ref_raster_p50_us"),
        report.get("ref_encode_p50_us"),
        report.get("ref_gpu_p50_us"),
    );
    for scene in &scenes {
        scene.report(&mut report, clocked, counted);
        let get = |k: &str| report.get(&format!("{}_{k}", scene.name)).to_string();
        eprintln!(
            "effects_harness: {:<8} n={} emit {} us | raster {} us | encode {}/{} us | gpu {}/{} us \
             | passes {} (drop {}) | pass cost {} us = {}% of present p95 {} us | unorm draws {} us \
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
