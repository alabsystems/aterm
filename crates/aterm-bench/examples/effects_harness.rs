// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::fmt::Write as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

use aterm_core::config::TerminalConfig;
use aterm_core::render::{
    DecoGlyph, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, InkCell,
    PxRect, WordDecoration,
};
use aterm_core::terminal::{Rgb, Terminal};
use aterm_effects::cursor_fx::{
    CursorFxInput, GlowKnobs, GlowSpelling, TrailColor, follow_live_cursor, head_dx_for,
    resolve_glow_config,
};
use aterm_effects::cursor_glow::{
    CURSOR_WITNESS_ROWS, CursorGlow, Geom, GlowConfig, GlowStyle, ProbeTrust,
};
use aterm_effects::cursor_trail::TrailConfig;
use aterm_effects::harness_card::{
    CardFrame, CardFrames, CardPalette, CardScene, EPISODE_MS, FRAME_MS, HarnessCard, LineAt,
    card_layer_id, dark_ground, line_star,
};
use aterm_effects::host::motion_print_anchor;
use aterm_effects::pipeline::{CursorStep, EffectsPipeline, RowProbe};
use aterm_effects::sound_policy::fold_window_audibility;
use aterm_gpu::gpu_time::{self, Settled};
use aterm_gpu::{
    FetchProbe, FrameShape, GpuContext, GpuRenderer, UnormAblation, UnormRoute, WindowGpu,
};
use aterm_render::{RenderInput, Renderer, Theme, WindowCpu};
use aterm_types::scheme::ColorScheme;

const FONT_PX: f32 = 30.0;
const ROWS: usize = 26;
const COLS: usize = 190;
const PAD: usize = 12;

const WARMUP: usize = 1;
const RUNS: usize = 5;

const SETTLED_KEYS: usize = 120;
const FLOWING_KEYS: usize = 48;
const FLOWING_KEYS_HZ: u32 = 12;
const TYPING_HZ: u32 = 120;
const TYPING_KEYS: usize = 48;
const KEY_TICKS: usize = 10;
const RUNOUT_TICKS: usize = 120;
const PRODUCER_KEYS: usize = 24;
const FLOOD_KEYS: usize = 400;

const HOPS: usize = 15;
const HOP_TICKS: usize = 24;
const HOP_COLS: (u16, u16) = (20, 100);
const SCROLL_CYCLES: usize = 4;

const REF_EVERY: usize = 8;

const HANG: Duration = Duration::from_secs(60);

const CARD: [&str; 3] = [
    " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.304",
    "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.5 \u{00B7} Claude Max",
    "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/src/aterm",
];
const BOX_ROWS: u16 = 4;
const BOX_COLS: u16 = 62;
const TRAIL: (u16, u16) = (33, 41);
const KITTY_COL: u16 = 41;
const LINE_ROW: u16 = 3;
const LINE_STAR_COL: u16 = 12;
const LINE_COL: u16 = LINE_STAR_COL + 2;
const LINE_TEXT: &str = "Cruise: the kitty keeps watch while the reply streams";

/// The `composer` scene: Claude Code's prompt row, between its two rules, and the keys typed
/// into it at 10 a second (a key every [`COMPOSER_KEY_TICKS`] frames at [`TYPING_HZ`]), to the
/// 117-cell line the owner's composer holds.
const COMPOSER_ROW: u16 = ROWS as u16 - 4;
const COMPOSER_KEYS: usize = 117;
const COMPOSER_KEY_TICKS: usize = 12;

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

#[cfg(not(unix))]
fn thread_cpu_ns() -> u64 {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    elapsed_ns(*ORIGIN.get_or_init(Instant::now))
}

#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    let pid = libc::c_int::try_from(std::process::id()).ok()?;
    // SAFETY: `info` is writable for one `rusage_info_v4`, the flavor asked for.
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

#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<u64> {
    None
}

static COUNTER_OVERHEAD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

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
    fn start() -> Self {
        Self {
            ins: instructions(),
            cpu: thread_cpu_ns(),
            wall: Instant::now(),
        }
    }

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

const NO_SEAM: &str =
    "built without the ablation seam: run it with `--features dev-seams` (the `effects` lane does)";

#[cfg(any(debug_assertions, feature = "dev-seams"))]
fn set_ablation(gpu: &mut GpuRenderer, ablation: UnormAblation) {
    gpu.set_unorm_ablation(ablation);
}

#[cfg(not(any(debug_assertions, feature = "dev-seams")))]
fn set_ablation(_: &mut GpuRenderer, _: UnormAblation) {
    fail(NO_SEAM);
}

#[cfg(any(debug_assertions, feature = "dev-seams"))]
fn set_route(gpu: &mut GpuRenderer, route: UnormRoute) {
    gpu.set_unorm_route(route);
}

#[cfg(not(any(debug_assertions, feature = "dev-seams")))]
fn set_route(_: &mut GpuRenderer, _: UnormRoute) {
    fail(NO_SEAM);
}

#[cfg(any(debug_assertions, feature = "dev-seams"))]
fn set_clocked(clocked: bool) {
    gpu_time::set_paused(!clocked);
}

#[cfg(not(any(debug_assertions, feature = "dev-seams")))]
fn set_clocked(_: bool) {
    fail(NO_SEAM);
}

#[derive(Clone, Copy, Debug)]
struct Way {
    ablation: UnormAblation,
    clocked: bool,
    /// The route the way asks for: the shipped one, or the split a fetch-capable device skips.
    route: UnormRoute,
    name: &'static str,
}

/// How each frame is presented, in turn: the shipped plan, the two ablations, the shipped plan
/// with the GPU clock paused, and the shipped plan kept on the split (the Unorm-law light in
/// passes of its own), which prices the framebuffer-fetch route in place.
const WAYS: [Way; 5] = [
    Way {
        ablation: UnormAblation::Off,
        clocked: true,
        route: UnormRoute::Fetch,
        name: "off",
    },
    Way {
        ablation: UnormAblation::Thin,
        clocked: true,
        route: UnormRoute::Fetch,
        name: "thin",
    },
    Way {
        ablation: UnormAblation::Drop,
        clocked: true,
        route: UnormRoute::Fetch,
        name: "drop",
    },
    Way {
        ablation: UnormAblation::Off,
        clocked: false,
        route: UnormRoute::Fetch,
        name: "untimed",
    },
    Way {
        ablation: UnormAblation::Off,
        clocked: true,
        route: UnormRoute::Split,
        name: "split",
    },
];

fn clear_streams(input: &mut RenderInput) {
    input.glow_under.clear();
    input.glow_under_ramps.clear();
    input.nova_add.clear();
    input.cursor_glow_add.clear();
    input.cursor_glow_add_ramps.clear();
    input.glow_halo.clear();
    input.fire_patch.clear();
    input.char_fg.clear();
    input.fire_halo.clear();
}

/// The frame's fx lights, a glow stream's lit ramp rows each the quad it
/// stands for.
fn fx_quads(input: &RenderInput) -> u64 {
    [
        input.glow_under_ramps.laid(input.glow_under.len()),
        input.nova_add.len(),
        input
            .cursor_glow_add_ramps
            .laid(input.cursor_glow_add.len()),
        input.glow_halo.len(),
        input.fire_patch.len(),
        input.char_fg.len(),
        input.fire_halo.len(),
    ]
    .into_iter()
    .map(|n| n as u64)
    .sum()
}

#[derive(Clone, Copy, Debug, Default)]
struct Arm {
    encode_ns: u64,
    call_cpu_ns: u64,
    call_ins: Option<u64>,
    gpu_ns: Option<u64>,
    shape: FrameShape,
}

impl Arm {
    fn present_ns(&self) -> u64 {
        self.encode_ns + self.gpu_ns.unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    emit: Spent,
    splice_ns: u64,
    raster: Spent,
    fx_quads: u64,
    under_quads: u64,
    /// The ramps the glow streams carry (`glow_under`, `cursor_glow_add`).
    under_ramps: u64,
    add_ramps: u64,
    /// The lights the two streams' ramps carry, and the rows of ends they
    /// keep (a run's rows one), for the ramps' cost model.
    ramp_lights: u64,
    ramp_rows: u64,
    /// The pipeline scenes' sprites ([`sprite_items`]).
    sprites: u64,
    /// The texel bytes the frame uploaded, across its arms ([`pipeline_scene`]).
    uploads: u64,
    /// The frames in flight the frame's fx atlas syncs waited for before rewriting a region
    /// they may read, across its arms, and the nanoseconds they parked
    /// (`FrameShape::fx_reader_waits`).
    reader_waits: u64,
    reader_wait_ns: u64,
    dirty_rows: u64,
    output_fingerprint: Option<u64>,
    arms: [Arm; WAYS.len()],
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
    fn split(&self) -> &Arm {
        &self.arms[4]
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RefSample {
    raster: Spent,
    encode_ns: u64,
    call_cpu_ns: u64,
    call_ins: Option<u64>,
    gpu_ns: Option<u64>,
    blank_gpu_ns: Option<u64>,
}

#[derive(Debug, Default)]
struct Run {
    samples: Vec<Sample>,
    refs: Vec<RefSample>,
}

/// The full repaints every scene's GPU time is read against (`ref_*`). Their terminals stay
/// bare, unthemed ([`Rig::term`]): the yardstick is the same work on every tree.
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

struct Win {
    cpu: WindowCpu,
    gpu: [WindowGpu; WAYS.len()],
}

impl Win {
    fn new() -> Self {
        Self {
            cpu: WindowCpu::new(),
            gpu: std::array::from_fn(|_| WindowGpu::new()),
        }
    }
}

struct Rig {
    cpu: Renderer,
    gpu: GpuRenderer,
    clock: &'static str,
    counter: &'static str,
    theme: Theme,
    /// The name of the built-in theme [`Self::theme`] comes from ([`rig_scheme`]).
    theme_name: String,
    /// What a window gives each terminal it draws on [`Self::theme`] ([`engine_colors`]).
    colors: TerminalConfig,
    cw: u16,
    ch: u16,
    pad: u16,
    grid_top: u16,
    frame_w: usize,
    frame_h: usize,
    frames: usize,
    reference: Reference,
    refs: Vec<RefSample>,
    /// The sweep's cost for the beams' runs (`--ramp-cost`), in place of the
    /// measured one.
    ramp_cost: Option<aterm_render::RampCost>,
}

/// The cost sweep's knobs (`docs/measured/card-engine-b5-2026-10-24.md`, round 3): the beams'
/// runs folded under `--ramp-cost LIGHT,RAMP` (`never`: every row its quad) in place of the
/// measured cost, and their ramps drawn by `--ramps-fx fx` in the fx stream or by
/// `--ramps-fx instances` as the glow instances their rows stand for, in place of the measured
/// rule. Both are development seams; a harness run without them lays and draws what a shipped
/// build does.
struct Knobs {
    ramp_cost: Option<aterm_render::RampCost>,
    ramps_fx: Option<aterm_gpu::RampsFx>,
}

impl Knobs {
    fn from_args() -> Self {
        let value = |flag: &str| std::env::args().skip_while(|a| a != flag).nth(1);
        let ramp_cost = value("--ramp-cost").map(|v| {
            if v == "never" {
                return aterm_render::RampCost::NEVER;
            }
            let parts: Vec<u32> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            match parts[..] {
                [light, ramp] => aterm_render::RampCost { light, ramp },
                _ => fail(format!("--ramp-cost {v}: LIGHT,RAMP in ns, or never")),
            }
        });
        let ramps_fx = value("--ramps-fx").map(|v| match v.as_str() {
            "fx" => aterm_gpu::RampsFx::Always,
            "instances" => aterm_gpu::RampsFx::Never,
            _ => fail(format!("--ramps-fx {v}: fx or instances")),
        });
        Self {
            ramp_cost,
            ramps_fx,
        }
    }

    /// The knobs as the report names them (`measured` where unset).
    fn names(&self) -> (String, &'static str) {
        let cost = match self.ramp_cost {
            None => "measured".to_string(),
            Some(aterm_render::RampCost::NEVER) => "never".to_string(),
            Some(c) => format!("{},{}", c.light, c.ramp),
        };
        let route = match self.ramps_fx {
            None => "measured",
            Some(aterm_gpu::RampsFx::Always) => "fx",
            Some(aterm_gpu::RampsFx::Never) => "instances",
        };
        (cost, route)
    }
}

/// The theme `--theme NAME` or `--theme=NAME` names, if the command line has either; a
/// `--theme` with no name after it fails rather than leaving the rig on Default.
fn theme_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let name = if arg == "--theme" {
            args.next()
        } else if let Some(name) = arg.strip_prefix("--theme=") {
            Some(name.to_string())
        } else {
            continue;
        };
        return match name {
            Some(name) if !name.is_empty() && !name.starts_with("--") => Some(name),
            _ => fail("--theme names no theme: pass --theme NAME or --theme=NAME"),
        };
    }
    None
}

/// The rig's theme: Default, under `--light` Default Light (the shipped light theme, the card
/// in its light palette), or under `--theme NAME` that built-in theme, its ground deciding the
/// arm the trail draws as it does in a window.
fn rig_scheme() -> ColorScheme {
    let named = theme_arg();
    let light = std::env::args().skip(1).any(|a| a == "--light");
    let name = match (named.as_deref(), light) {
        (Some(_), true) => fail("--theme and --light both name the rig's theme"),
        (Some(name), false) => name,
        (None, true) => "Default Light",
        (None, false) => "Default",
    };
    aterm_types::scheme::builtin(name)
        .unwrap_or_else(|| fail(format!("{name} is not a built-in theme")))
}

/// The engine colours a window gives every terminal it draws on `scheme`, a theme its config
/// names (aterm-gui's `Config::applied_terminal_config_from`): the theme's ink and ground, its
/// cursor and selection, and its ANSI slots as the indexed palette. A bare `Terminal::new`
/// draws its own E5E5E5 on 000000 and xterm's slots, letters no window shows on the theme's
/// page, which the letter guard then judges under lights solved for the theme's.
fn engine_colors(scheme: &ColorScheme) -> TerminalConfig {
    let parts = scheme.to_theme_parts();
    let unpack = |c: u32| Rgb::new((c >> 16) as u8, (c >> 8) as u8, c as u8);
    TerminalConfig {
        default_foreground: scheme.foreground,
        default_background: scheme.background,
        cursor_color: Some(unpack(parts.cursor)),
        selection_background: Some(unpack(parts.selection)),
        selection_foreground: scheme.selection_fg,
        custom_palette: Some(scheme.to_color_palette()),
        color_scheme: Some(scheme.appearance),
        ..TerminalConfig::default()
    }
}

fn skip(why: impl std::fmt::Display) -> ! {
    let why = why.to_string().replace(['"', '\\'], "'");
    eprintln!("effects_harness: SKIP: {why}");
    println!("{{\"skip\":\"{why}\"}}");
    std::process::exit(0);
}

impl Rig {
    fn new(knobs: &Knobs) -> Self {
        let font = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
        );
        let scheme = rig_scheme();
        let theme = Theme::from(&scheme);
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
        gpu.set_ramps_fx_for_test(knobs.ramps_fx);
        let (cw, ch) = cpu.cell_size();
        if gpu.cell_size() != (cw, ch) {
            fail("the two presenters disagree on the cell");
        }
        let (frame_w, frame_h) = cpu.frame_size(ROWS, COLS);
        let clock = if gpu.gpu_time_supported() {
            "metal"
        } else {
            "none"
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
            colors: engine_colors(&scheme),
            theme_name: scheme.name,
            frames: 0,
            reference: Reference::new(),
            refs: Vec::new(),
            ramp_cost: knobs.ramp_cost,
        }
    }

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

    /// A scene's terminal, in the rig's theme as a window draws it: the colours the app gives
    /// every session ([`Terminal::apply_config_colors`]).
    fn term(&self) -> Terminal {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.apply_config_colors(&self.colors);
        term
    }

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

    fn gpu_time(&mut self) -> Option<u64> {
        if self.clock == "none" {
            return None;
        }
        let Some(serial) = self.gpu.last_gpu_time_serial() else {
            fail("a present on a GPU-clocked arm went untimed (gpu_time skipped it)");
        };
        let deadline = Instant::now() + HANG;
        loop {
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
        let dirty_rows = match win.cpu.last_damage() {
            aterm_render::DamageOutcome::Full => ROWS as u64,
            aterm_render::DamageOutcome::GateHit => 0,
            _ => win.cpu.dirty_rows().iter().filter(|d| **d).count() as u64,
        };

        let mut arms = [Arm::default(); WAYS.len()];
        for k in 0..WAYS.len() {
            let i = (k + self.frames) % WAYS.len();
            let way = WAYS[i];
            set_ablation(&mut self.gpu, way.ablation);
            set_route(&mut self.gpu, way.route);
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
        set_route(&mut self.gpu, UnormRoute::Fetch);
        self.frames += 1;
        if self.frames.is_multiple_of(REF_EVERY) {
            let reference = self.reference_frame();
            self.refs.push(reference);
        }
        let [off, thin, dropped, untimed, split] = &arms;
        Self::check_ablation(&off.shape, &thin.shape, &dropped.shape);
        self.check_route(&off.shape, &split.shape);
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
            under_quads: input.glow_under_ramps.laid(input.glow_under.len()) as u64,
            under_ramps: input.glow_under_ramps.items.len() as u64,
            add_ramps: input.cursor_glow_add_ramps.items.len() as u64,
            ramp_lights: (input.glow_under_ramps.lit + input.cursor_glow_add_ramps.lit) as u64,
            ramp_rows: ((input.glow_under_ramps.ends.len()
                + input.cursor_glow_add_ramps.ends.len())
                / 2) as u64,
            sprites: 0,
            uploads: 0,
            reader_waits: arms.iter().map(|a| a.shape.fx_reader_waits).sum(),
            reader_wait_ns: arms.iter().map(|a| a.shape.fx_reader_wait_ns).sum(),
            dirty_rows,
            output_fingerprint: None,
            arms,
        }
    }

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

    /// The shipped way took the route the device's verdict allows, and the split way the split,
    /// drawing the same instances and Unorm-law draws.
    fn check_route(&self, off: &FrameShape, split: &FrameShape) {
        let want = if self.gpu.fetch_probe() == Some(FetchProbe::Passed) {
            UnormRoute::Fetch
        } else {
            UnormRoute::Split
        };
        if off.route != want || split.route != UnormRoute::Split {
            fail(format!(
                "the shipped way drew on the {} route and the split way on the {} route (want {} \
                 and split)",
                off.route.name(),
                split.route.name(),
                want.name()
            ));
        }
        if (split.instances, split.unorm_draws, split.unorm_instances)
            != (off.instances, off.unorm_draws, off.unorm_instances)
        {
            fail("the split way drew other instances or Unorm draws than the shipped one");
        }
    }

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

    fn take_run(&mut self, samples: Vec<Sample>) -> Run {
        Run {
            samples,
            refs: std::mem::take(&mut self.refs),
        }
    }

    fn extract(term: &mut Terminal, input: &mut RenderInput) {
        let _ = term.cell_frame_damage_scoped_into(input, ROWS, COLS);
        clear_streams(input);
    }

    fn settle_window(&mut self, win: &mut Win, input: &mut RenderInput) {
        input.refresh_cursor_effect_damage();
        let _ = self.present(win, input, Spent::default(), 0);
    }
}

const CARD_ID: u64 = 1;

#[derive(Default)]
struct CardHost {
    card: HarnessCard,
    frames: CardFrames,
    frame: Option<CardFrame>,
    line: LineHost,
}

#[derive(Default)]
struct LineHost {
    row: Option<u16>,
    settled: bool,
    ink: Vec<InkCell>,
    star: Option<WordDecoration>,
}

impl LineHost {
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
        let (color, blend) = line_star(
            u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
            scene.ground,
        );
        self.star = Some(WordDecoration {
            row,
            col: LINE_STAR_COL,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Star4,
            blend,
            color,
            alpha: u8::MAX,
        });
    }

    fn lay(&self, input: &mut RenderInput) {
        input.ink.clone_from(&self.ink);
        input.word_decorations.clear();
        input.word_decorations.extend(self.star);
    }
}

impl CardHost {
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

    fn drew_whole(&self) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|f| !f.under.items().is_empty() && !f.over.items().is_empty())
    }
}

/// The cursor family as a window presents it: [`EffectsPipeline::cursor_step`]
/// on the frame's cells, so every pass the shipped path runs (the letter guard
/// among them) is priced.
struct GlowHost {
    fx: EffectsPipeline,
    cfg: GlowConfig,
    geom: Geom,
    row: Vec<char>,
    witness: Vec<char>,
    above: Vec<char>,
    below: Vec<char>,
    last_fp: u64,
}

impl GlowHost {
    /// The config a focused window resolves for `style` on the rig's theme (aterm-gui's
    /// `App::glow_config_for`, then `fold_window_audibility`), from knobs a default window
    /// does not pass. Like a default window's, they set no `cursor_trail_color` or accent, so
    /// the colour is the style's automatic one against the theme's cursor and the accent its
    /// brightening, and the radius and ring reach only the styles that read them. Unlike a
    /// default window's, the duration is 650 ms, the length 512 and the radius 0.4, where a
    /// default config gives 260 ms, 24 and 0.6; each is a value a config can set. The
    /// brightness, 1.0, is every style's default but Classic's (0.7). Of those three knobs,
    /// Rainbow Kitty, the style of the typing, meteor, flood and composer scenes, reads only
    /// the duration.
    fn new(rig: &Rig, style: GlowStyle) -> Self {
        let mut cfg = resolve_glow_config(
            GlowKnobs {
                enabled: true,
                color: None,
                accent: None,
                duration_ms: 650,
                length: 512,
                intensity: 1.0,
                radius: 0.4,
                ring: true,
            },
            GlowSpelling {
                style: Some(style),
                beam: aterm_effects::cursor_glow::style_has_beam_of(style, ""),
                ribbon_tall: true,
                ribbon_flat: false,
                classic_mono: false,
            },
            rig.theme.cursor,
            aterm_render::theme_is_dark(rig.theme.bg),
            rig.theme.fg,
            rig.theme.bg,
            0.5,
        );
        fold_window_audibility(&mut cfg, 1.0, 1.0, true);
        let mut host = Self {
            fx: EffectsPipeline::for_window(),
            cfg,
            geom: rig.glow_geom(),
            row: Vec::new(),
            witness: Vec::new(),
            above: Vec::new(),
            below: Vec::new(),
            last_fp: 0,
        };
        if let Some(cost) = rig.ramp_cost {
            host.glow().set_ramp_cost(cost);
        }
        host
    }

    fn glow(&mut self) -> &mut CursorGlow {
        &mut self.fx.cursor_mut().glow
    }

    fn emit(
        &mut self,
        term: &Terminal,
        input: &RenderInput,
        caret: (u16, u16),
        now: Instant,
    ) -> (u64, Spent) {
        // What a window reads off its terminal each frame before the step (aterm-gui's
        // `App::cursor_frame`): the cursor colour it presents, which the automatic glow
        // colour and accent follow, the cursor's shape and the ground under it.
        let ink = term
            .cursor_color()
            .unwrap_or_else(|| term.default_foreground());
        let live_cursor = aterm_render::rgb_to_u32([ink.r, ink.g, ink.b]);
        let pinned = term.cursor_color_recoloured();
        let mut trail = TrailConfig {
            enabled: false,
            duration: self.cfg.duration,
            max_len: self.cfg.length,
            color: self.cfg.color,
            intensity: self.cfg.intensity,
            warmth: 0.0,
        };
        follow_live_cursor(
            &mut self.cfg,
            &mut trail,
            live_cursor,
            pinned,
            TrailColor::Automatic,
            false,
            None,
        );
        self.cfg.head_dx = head_dx_for(term.cursor_style());
        let (row, col) = caret;
        let cursor_cell_bg = input
            .cells
            .get(usize::from(row))
            .and_then(|cells| cells.get(usize::from(col)))
            .map(|cell| aterm_render::rgb_to_u32(cell.bg));
        let watch = Stopwatch::start();
        let below_row = usize::from(row) + 1 < ROWS;
        term.row_cols_into(usize::from(row), &mut self.row);
        if row > 0 {
            term.row_cols_into(usize::from(row) - 1, &mut self.above);
        }
        if below_row {
            term.row_cols_into(usize::from(row) + 1, &mut self.below);
        }
        let glow = &mut self.fx.cursor_mut().glow;
        glow.observe_ribbon_row(row, &self.row);
        let mut rows = [0u16; CURSOR_WITNESS_ROWS];
        let n = glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            if r != row && usize::from(r) < ROWS {
                term.row_cols_into(usize::from(r), &mut self.witness);
                glow.observe_ribbon_row(r, &self.witness);
            }
        }
        let (print_anchor, print_anchor_glyph) = motion_print_anchor(term);
        let step = self.fx.cursor_step(
            &CursorStep::new(
                CursorFxInput {
                    now,
                    cur: Some(caret),
                    live_viewport: true,
                    cursor_style: term.cursor_style(),
                    blink_phase: true,
                    live_cursor,
                    cursor_color_pinned: pinned,
                    default_bg: self.cfg.theme_bg,
                    cursor_cell_bg,
                    geom: self.geom,
                    focused: true,
                    shed_envelope: 1.0,
                    trail_brightness: 1.0,
                    body_allowed: true,
                    momentum_glow: true,
                    user_tinted: false,
                    accent_pinned: false,
                },
                &self.cfg,
                trail,
            )
            .with_row_probe(Some(RowProbe {
                row,
                caret: col,
                trust: ProbeTrust::Full,
                cols: &self.row,
                flanks: Some((
                    (row > 0).then_some(self.above.as_slice()),
                    below_row.then_some(self.below.as_slice()),
                )),
            }))
            .with_frame_cells(Some(input.cells.as_slice()))
            .with_frame_cells_cols(input.cols)
            .with_print_anchor(print_anchor)
            .with_print_anchor_glyph(print_anchor_glyph),
            std::iter::empty(),
        );
        (step.frame.glow_fp, watch.stop())
    }

    fn splice(&mut self, input: &mut RenderInput) -> u64 {
        let started = Instant::now();
        clear_streams(input);
        self.fx.publish_cursor(input);
        input.refresh_cursor_effect_damage();
        elapsed_ns(started)
    }
}

fn cup(row: u16, col: u16) -> String {
    format!("\x1b[{};{}H", row + 1, col + 1)
}

fn print_card(term: &mut Terminal, top: u16) {
    for (i, line) in CARD.iter().enumerate() {
        term.process(format!("{}{line}", cup(top + i as u16, 0)).as_bytes());
    }
    term.process(format!("{}{LINE_TEXT}", cup(top + LINE_ROW, LINE_COL)).as_bytes());
}

fn banner_at(term: &Terminal, row: u16, scratch: &mut Vec<char>) -> bool {
    term.row_cols_into(usize::from(row), scratch);
    let banner: Vec<char> = CARD[0].chars().collect();
    scratch.len() >= banner.len() && scratch[..banner.len()] == banner[..]
}

fn card_scenes(rig: &mut Rig, seed: u64) -> (Run, Run, Run) {
    let mut term = rig.term();
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

/// Two small items spanning eight row bands, recoloured each frame without moving their hull.
/// Every measured frame follows an already drawn frame; verification runs outside the timers.
fn sparse_fx_scene(rig: &mut Rig) -> Run {
    let mut term = rig.term();
    let mut input = term.cell_frame(ROWS, COLS);
    input.cursor_visible = false;
    let layers: [FxLayer; 2] = std::array::from_fn(|phase| {
        let items = [2, 9]
            .map(|row| {
                FxItem::Rect(FxRect {
                    x: 4 * i32::from(rig.cw),
                    y: row * i32::from(rig.ch) + 1,
                    w: rig.cw,
                    h: 3,
                    ink: 0,
                })
            })
            .to_vec();
        FxLayer::new(
            0x5350_4152_5345,
            std::sync::Arc::new(FxContent::new(
                items,
                vec![FxInk::add([0x0020_4080, 0x0080_2040][phase]); 2],
            )),
            FxSlot::Glow,
            FxSpace::Grid,
            FxBlend::Exact,
        )
    });
    let mut win = Win::new();
    input.fx_layers = vec![layers[0].clone()];
    rig.settle_window(&mut win, &mut input);
    rig.refs.clear();
    let mut samples = Vec::new();
    let mut previous = None;
    for step in 0..32 {
        let watch = Stopwatch::start();
        input.fx_layers[0] = layers[(step + 1) % 2].clone();
        let emit = watch.stop();
        let mut sample = rig.present(&mut win, &input, emit, 0);
        #[cfg(target_os = "macos")]
        if rig.clock == "metal" && !rig.gpu.metal_render_armed() {
            fail("the sparse FX Metal presenter is no longer armed");
        }
        // An owned full render has no retained cache. Compare actual pixels, then retain a
        // stable digest for the before/after runs; neither operation is in a measured timer.
        let cached = rig
            .cpu
            .render_input_cached(&mut win.cpu, &input)
            .pixels()
            .to_vec();
        let full = rig.cpu.render_input(&input);
        if cached != full.pixels {
            fail(format!(
                "sparse fx frame {step}: cached pixels differ from a full render"
            ));
        }
        let fingerprint = cached.iter().fold(0xcbf2_9ce4_8422_2325, |h, &p| {
            (h ^ u64::from(p)).wrapping_mul(0x100_0000_01b3)
        });
        if previous == Some(fingerprint) || sample.dirty_rows == 0 {
            fail("a recoloured sparse layer changed no pixels or marked no rows");
        }
        previous = Some(fingerprint);
        sample.output_fingerprint = Some(fingerprint);
        samples.push(sample);
    }
    let unchanged = rig.present(&mut win, &input, Spent::default(), 0);
    if unchanged.dirty_rows != 0 {
        fail("an unchanged sparse layer repainted rows");
    }
    rig.take_run(samples)
}

#[derive(Clone, Copy)]
enum Script {
    Type { keys: usize, every: usize },
    Hop { hops: usize, every: usize },
}

impl Script {
    fn steps(self) -> (usize, usize) {
        match self {
            Self::Type { keys, every } | Self::Hop { hops: keys, every } => (keys, every),
        }
    }
}

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

/// The page a typing scene's keys land on.
#[derive(Clone, Copy)]
enum Page {
    /// A directory listing over a shell prompt.
    Shell,
    /// Claude Code's composer under a reply: its prompt on [`COMPOSER_ROW`] between two `─`
    /// rules, its hint under them.
    Composer,
}

impl Page {
    fn paint(self, term: &mut Terminal) {
        match self {
            Self::Shell => {
                for i in 0..10 {
                    term.process(
                        format!("drwxr-xr-x  12 user  staff   384 Oct 19 09:{i:02} crate-{i}\r\n")
                            .as_bytes(),
                    );
                }
                term.process(b"~/src/aterm $ ");
            }
            Self::Composer => {
                for i in 0..COMPOSER_ROW - 2 {
                    let bullet = if i == 0 { "\u{23fa}" } else { " " };
                    term.process(format!("{bullet} {} {i}\r\n", &STREAMED[..72]).as_bytes());
                }
                let rule = format!("\x1b[90m{}\x1b[0m", "\u{2500}".repeat(COLS));
                term.process(
                    format!(
                        "{}{rule}{}{rule}{}\x1b[90m  ? for shortcuts\x1b[0m{}\u{276f} ",
                        cup(COMPOSER_ROW - 1, 0),
                        cup(COMPOSER_ROW + 1, 0),
                        cup(COMPOSER_ROW + 2, 0),
                        cup(COMPOSER_ROW, 0),
                    )
                    .as_bytes(),
                );
            }
        }
    }
}

fn typing_scene(rig: &mut Rig, style: GlowStyle, page: Page, script: Script) -> Run {
    let mut term = rig.term();
    page.paint(&mut term);
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
                host.glow().note_synthetic_typed(now, 1);
            }
            (Some(k), Script::Hop { .. }) => {
                let col = if k % 2 == 0 { HOP_COLS.1 } else { HOP_COLS.0 };
                term.process(cup(prompt_row, col).as_bytes());
                host.glow().note_synthetic_move(now);
            }
            (None, _) => {}
        }
        Rig::extract(&mut term, &mut input);
        let caret = (
            u16::try_from(input.cursor_row).unwrap_or(u16::MAX),
            u16::try_from(input.cursor_col).unwrap_or(u16::MAX),
        );
        let (fp, emit) = host.emit(&term, &input, caret, now);
        if event.is_none() && fp == host.last_fp {
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

fn scroll_scene(rig: &mut Rig) -> Run {
    let mut samples = Vec::new();
    let mut row = Vec::new();
    rig.refs.clear();
    for _ in 0..SCROLL_CYCLES {
        let mut term = rig.term();
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

/// One pipeline tick: the window's frame rate.
const PIPE_MS: f64 = 1000.0 / 30.0;
const RAIN_TICKS: usize = 180;
const CATS_TICKS: usize = 240;
const CATS_EVERY: usize = 40;
const PET_KEYS: usize = 48;
const PET_EVERY: usize = 10;

/// The sprites a frame's effects pipeline drew: the items of the matrix rain's rows and of the
/// companions' and word cats' sprite layers.
fn sprite_items(input: &RenderInput) -> u64 {
    input
        .fx_layers
        .iter()
        .filter(|l| {
            aterm_effects::matrix_rain::is_rain_row(l) || aterm_render::sprites::is_sprite_layer(l)
        })
        .map(|l| l.content.items().len() as u64)
        .sum()
}

/// A scene driven through the effects pipeline a window runs: `setup` configures it, `feed`
/// gives tick `k` its input (typing, output) and says whether it fed any; a tick that fed none
/// and folded the frame it folded before presents nothing, as a window would.
fn pipeline_scene(
    rig: &mut Rig,
    name: &str,
    ticks: usize,
    setup: impl FnOnce(&mut EffectsPipeline),
    mut feed: impl FnMut(usize, &mut EffectsPipeline, &mut Terminal, &RenderInput) -> bool,
) -> Run {
    let mut term = rig.term();
    for i in 0..4 {
        term.process(format!("{} {i}\r\n", &STREAMED[..72]).as_bytes());
    }
    term.process(b"~/src/aterm $ ");
    let mut p = EffectsPipeline::new();
    p.set_focused(true);
    setup(&mut p);
    if let Some(cost) = rig.ramp_cost {
        p.cursor_mut().glow.set_ramp_cost(cost);
    }
    let mut input = RenderInput::empty();
    let mut win = Win::new();
    Rig::extract(&mut term, &mut input);
    rig.settle_window(&mut win, &mut input);
    rig.refs.clear();
    let (mut samples, mut last_fp, mut drew) = (Vec::new(), 0, 0);
    let cell = (usize::from(rig.cw), usize::from(rig.ch));
    for tick in 0..ticks {
        let fed = feed(tick, &mut p, &mut term, &input);
        p.advance(PIPE_MS);
        Rig::extract(&mut term, &mut input);
        let watch = Stopwatch::start();
        let fp = p.apply(&mut term, &mut input, cell.0, cell.1);
        let emit = watch.stop();
        let sprites = sprite_items(&input);
        drew += sprites;
        if !fed && fp == last_fp {
            continue;
        }
        last_fp = fp;
        let mut sample = rig.present(&mut win, &input, emit, 0);
        sample.sprites = sprites;
        // One renderer presents every arm, and its fx atlas uploads an art once: the sum is
        // the texel bytes the frame uploaded (its instances and palette are in
        // `instance_bytes` and `fx_upload_bytes`).
        sample.uploads = sample
            .arms
            .iter()
            .map(|arm| arm.shape.fx_atlas_bytes)
            .sum::<u64>();
        samples.push(sample);
    }
    if drew == 0 {
        fail(format!("the {name} scene drew no sprite"));
    }
    rig.take_run(samples)
}

fn type_char(p: &mut EffectsPipeline, term: &mut Terminal, input: &RenderInput, b: u8) {
    let _ = p.note_committed_char(term, input, char::from(b));
    term.process(&[b]);
}

fn rain_scene(rig: &mut Rig, seed: u64) -> Run {
    let (bg, fg) = (rig.theme.bg, rig.theme.fg);
    pipeline_scene(
        rig,
        "rain",
        RAIN_TICKS,
        |p| {
            p.set_matrix_rain(
                30, 6, 4, 6, None, None, "matrix", None, 320, 12, false, true, true, false, seed,
                bg, fg,
            );
            p.set_matrix_rain_enabled(true);
        },
        |_, p, _, _| {
            p.note_keystroke();
            false
        },
    )
}

fn cats_scene(rig: &mut Rig) -> Run {
    pipeline_scene(
        rig,
        "cats",
        CATS_TICKS,
        |p| {
            p.set_sparkle_enabled(true);
            p.set_sparkle_classes(false, true, false);
            p.set_sparkle_feline("cat", true, true, false);
        },
        |tick, _, term, _| {
            if !tick.is_multiple_of(CATS_EVERY) {
                return false;
            }
            let k = tick / CATS_EVERY;
            term.process(
                format!("a kitty naps by the cat flap while the kitten {k} watches\r\n").as_bytes(),
            );
            true
        },
    )
}

fn pet_scene(rig: &mut Rig, seed: u64) -> Run {
    pipeline_scene(
        rig,
        "pet",
        PET_KEYS * PET_EVERY + RUNOUT_TICKS,
        |p| {
            p.set_cursor_glow(
                true,
                "rainbow kitty",
                aterm_effects::cursor_fx::TrailColor::Automatic,
                None,
                400,
                24,
                0.9,
                0.8,
                true,
                0x0050_FA7B,
            );
            p.set_cursor_pet(true, seed);
        },
        |tick, p, term, input| {
            if !tick.is_multiple_of(PET_EVERY) || tick / PET_EVERY >= PET_KEYS {
                return false;
            }
            type_char(p, term, input, PROSE[(tick / PET_EVERY) % PROSE.len()]);
            true
        },
    )
}

/// The icons scene's keys and cadence (pipeline ticks a key).
const ICON_KEYS: usize = 48;
const ICON_EVERY: usize = 3;
/// What the icons scene types: feline words, each drawn under its icon by
/// the word pass once typed.
const ICON_PROSE: &[u8] = b"the cat naps; a kitty purrs by the cat flap ";

/// The word icons' shade ([`CursorGlow::shade_word_icons`]) under the
/// ribbon: the pipeline types feline words under the rainbow kitty, the word
/// pass draws their icons on the typed row, and each frame the shade lifts
/// the ribbon's light off them, its ramps cut in place.
fn icons_scene(rig: &mut Rig) -> Run {
    pipeline_scene(
        rig,
        "icons",
        ICON_KEYS * ICON_EVERY + RUNOUT_TICKS,
        |p| {
            p.set_cursor_glow(
                true,
                "rainbow kitty",
                aterm_effects::cursor_fx::TrailColor::Automatic,
                None,
                400,
                24,
                0.9,
                0.8,
                true,
                0x0050_FA7B,
            );
            p.set_sparkle_enabled(true);
            p.set_sparkle_classes(false, true, false);
            p.set_sparkle_feline("cat", true, true, false);
        },
        |tick, p, term, input| {
            if !tick.is_multiple_of(ICON_EVERY) || tick / ICON_EVERY >= ICON_KEYS {
                return false;
            }
            type_char(
                p,
                term,
                input,
                ICON_PROSE[(tick / ICON_EVERY) % ICON_PROSE.len()],
            );
            true
        },
    )
}

/// `--frames PATH`: every measured frame of every scene, one tab-separated line each (its scene,
/// run and place in the run; the ramps and the lit rows they carry; its main present's
/// instances, fx instances, build, stage and encode CPU nanoseconds and GPU nanoseconds), the
/// points the ramps' route costs are fitted on (`docs/measured/card-engine-b5-2026-10-24.md`,
/// round 3): a scene's frames are the same frames in every process, so two processes drawing
/// them by two routes pair frame by frame.
fn write_frames(path: &str, scenes: &[Scene]) {
    let mut out = String::from(
        "scene\trun\tframe\tunder_ramps\tadd_ramps\tlit\tinstances\tfx_instances\tbuild_ns\t\
         stage_ns\tencode_ns\tgpu_ns\n",
    );
    for scene in scenes {
        for (r, run) in scene.runs.iter().enumerate() {
            for (k, x) in run.samples.iter().enumerate() {
                let a = x.off();
                let _ = writeln!(
                    out,
                    "{}\t{r}\t{k}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    scene.name,
                    x.under_ramps,
                    x.add_ramps,
                    x.ramp_lights,
                    a.shape.total_instances(),
                    a.shape.fx_instances,
                    a.shape.build_ns,
                    a.shape.stage_ns,
                    a.encode_ns,
                    a.gpu_ns.unwrap_or(0),
                );
            }
        }
    }
    if let Err(e) = std::fs::write(path, out) {
        fail(format!("--frames {path}: {e}"));
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable_by(f64::total_cmp);
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    v[rank.clamp(1, v.len()) - 1]
}

type Figure = fn(&Sample) -> Option<f64>;

type RefFigure = fn(&RefSample) -> Option<f64>;

/// The mean of `v` (`0` when empty): a cost model sums a frame's costs.
fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len().max(1) as f64
}

fn ns(v: u64) -> f64 {
    v as f64
}

struct Scene {
    name: &'static str,
    runs: Vec<Run>,
    rate_emit: bool,
    /// The texel bytes the warm-up runs uploaded: the cold cost of the scene's arts, which
    /// the measured runs find resident in the rig's one fx atlas.
    cold_uploads: u64,
}

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
        let timed: [(&str, Figure); 21] = [
            ("emit", |x| Some(ns(x.emit.wall_ns))),
            // The whole frame: its emission, splice and present.
            ("frame_present", |x| {
                Some(ns(x.emit.wall_ns + x.splice_ns + x.off().present_ns()))
            }),
            ("emit_cpu", |x| Some(ns(x.emit.cpu_ns))),
            ("splice", |x| Some(ns(x.splice_ns))),
            ("raster", |x| Some(ns(x.raster.wall_ns))),
            ("raster_cpu", |x| Some(ns(x.raster.cpu_ns))),
            ("encode", |x| Some(ns(x.off().encode_ns))),
            ("encode_cpu", |x| Some(ns(x.off().call_cpu_ns))),
            ("build", |x| Some(ns(x.off().shape.build_ns))),
            ("stage", |x| Some(ns(x.off().shape.stage_ns))),
            ("build_stage", |x| {
                Some(ns(x.off().shape.build_ns + x.off().shape.stage_ns))
            }),
            ("gpu", |x| x.off().gpu_ns.map(ns)),
            ("present", |x| Some(ns(x.off().present_ns()))),
            ("thin_encode", |x| Some(ns(x.thin().encode_ns))),
            ("thin_gpu", |x| x.thin().gpu_ns.map(ns)),
            ("drop_encode", |x| Some(ns(x.drop().encode_ns))),
            ("drop_gpu", |x| x.drop().gpu_ns.map(ns)),
            ("pass_encode", |x| {
                Some(ns(x.thin().encode_ns) - ns(x.drop().encode_ns))
            }),
            ("split_encode", |x| Some(ns(x.split().encode_ns))),
            ("split_gpu", |x| x.split().gpu_ns.map(ns)),
            ("split_present", |x| Some(ns(x.split().present_ns()))),
        ];
        for (what, f) in timed {
            if !clocked && (what.ends_with("gpu") || what.ends_with("present")) {
                continue;
            }
            let mut v = self.pooled(f);
            r.num(format!("{s}_{what}_p50_us"), us(pct(&mut v, 50.0)));
            r.num(format!("{s}_{what}_p95_us"), us(pct(&mut v, 95.0)));
            r.num(format!("{s}_{what}_p99_us"), us(pct(&mut v, 99.0)));
            r.num(format!("{s}_{what}_mean_us"), us(mean(&v)));
        }
        let mut present = self.pooled(|x| Some(ns(x.off().present_ns())));
        let present_p95 = pct(&mut present, 95.0).max(1.0);
        if clocked {
            let paired: [(&str, Figure); 4] = [
                ("pass_gpu", |x| {
                    Some(ns(x.thin().gpu_ns?) - ns(x.drop().gpu_ns?))
                }),
                ("pass_cost", |x| {
                    Some(ns(x.thin().present_ns()) - ns(x.drop().present_ns()))
                }),
                ("unorm_draw", |x| {
                    Some(ns(x.off().present_ns()) - ns(x.thin().present_ns()))
                }),
                ("route_gain", |x| {
                    Some(ns(x.split().present_ns()) - ns(x.off().present_ns()))
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
                if what == "route_gain" {
                    let gain = pct(&mut v, 50.0);
                    let mut split = self.pooled(|x| Some(ns(x.split().present_ns())));
                    let split_p95 = pct(&mut split, 95.0).max(1.0);
                    r.num(format!("{s}_route_gain_pct"), 100.0 * gain / split_p95);
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
        // `ramp_frames` and `fx_frames` are 1 on a frame carrying a glow ramp
        // or drawing an fx stream: their means are the shares of such frames,
        // what the fx stream's opening costs a scene is weighed by.
        let counts: [(&str, Figure); 19] = [
            ("passes", |x| Some(f64::from(x.off().shape.passes))),
            ("drop_passes", |x| Some(f64::from(x.drop().shape.passes))),
            ("split_passes", |x| Some(f64::from(x.split().shape.passes))),
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
            ("under_ramps", |x| Some(ns(x.under_ramps))),
            ("add_ramps", |x| Some(ns(x.add_ramps))),
            ("ramp_lights", |x| Some(ns(x.ramp_lights))),
            ("ramp_rows", |x| Some(ns(x.ramp_rows))),
            ("ramp_frames", |x| {
                Some(f64::from(u8::from(x.under_ramps + x.add_ramps > 0)))
            }),
            ("fx_frames", |x| {
                Some(f64::from(u8::from(x.off().shape.fx_instances > 0)))
            }),
            ("dirty_rows", |x| Some(ns(x.dirty_rows))),
            ("fx_instances", |x| {
                Some(f64::from(x.off().shape.fx_instances))
            }),
            ("fx_upload_bytes", |x| {
                Some(ns(x.off().shape.fx_upload_bytes))
            }),
            ("sprites", |x| Some(ns(x.sprites))),
            ("upload_bytes", |x| Some(ns(x.uploads))),
        ];
        for (what, f) in counts {
            let mut v = self.pooled(f);
            r.int(format!("{s}_{what}_p50"), pct(&mut v, 50.0) as u64);
            r.int(format!("{s}_{what}_max"), pct(&mut v, 100.0) as u64);
            r.num(format!("{s}_{what}_mean"), mean(&v));
        }
        let uploaded: u64 = self.pooled(|x| Some(ns(x.uploads))).iter().sum::<f64>() as u64;
        r.int(
            format!("{s}_upload_bytes_per_run"),
            uploaded / self.runs.len().max(1) as u64,
        );
        r.int(
            format!("{s}_upload_bytes_cold"),
            self.cold_uploads / WARMUP.max(1) as u64,
        );
        let waits: u64 = self
            .pooled(|x| Some(ns(x.reader_waits)))
            .iter()
            .sum::<f64>() as u64;
        r.int(
            format!("{s}_reader_waits_per_run"),
            waits / self.runs.len().max(1) as u64,
        );
        let mut parked = self.pooled(|x| Some(ns(x.reader_wait_ns)));
        r.int(
            format!("{s}_reader_wait_ns_per_run"),
            (parked.iter().sum::<f64>() / self.runs.len().max(1) as f64) as u64,
        );
        r.int(
            format!("{s}_reader_wait_ns_max"),
            pct(&mut parked, 100.0) as u64,
        );
        let dirty = self.pooled(|x| Some(ns(x.dirty_rows)));
        r.num(
            format!("{s}_dirty_rows_mean"),
            dirty.iter().sum::<f64>() / dirty.len().max(1) as f64,
        );
        let checked: Vec<u64> = self
            .runs
            .iter()
            .flat_map(|r| &r.samples)
            .filter_map(|sample| sample.output_fingerprint)
            .collect();
        if !checked.is_empty() {
            r.int(format!("{s}_verified_frames"), checked.len() as u64);
            let fingerprint = checked.iter().fold(0u64, |h, &p| h.rotate_left(7) ^ p);
            r.text(
                format!("{s}_output_fingerprint"),
                &format!("{fingerprint:016x}"),
            );
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
            r.num(format!("{s}_{what}_kins_mean"), mean(&v) / 1_000.0);
            r.num(format!("{s}_{what}_kins_p95"), pct(&mut v, 95.0) / 1_000.0);
            let p50s = self.run_p50s(f);
            let fastest = p50s.iter().copied().fold(f64::INFINITY, f64::min).max(1.0);
            let slowest = p50s.iter().copied().fold(0.0, f64::max);
            r.num(format!("{s}_{what}_ins_spread"), slowest / fastest);
            if what == "emit" && !self.rate_emit {
                continue;
            }
            r.num(format!("{s}_{what}_per_ginsn"), 1e9 / p50.max(1.0));
            // The frames over all their instructions: the rate of a scene whose frames are two
            // kinds by design, where the median frame sits on the edge between the kinds (the
            // flowing card's 32 draws and 32 replays a run: its median is its costliest replay).
            r.num(
                format!("{s}_{what}_mean_per_ginsn"),
                1e9 / mean(&v).max(1.0),
            );
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
    if std::env::args().skip(1).any(|a| a == "--help") {
        println!(
            "effects_harness [--light | --theme NAME | --theme=NAME] [--damage] [--only a,b,...] [--ramp-cost LIGHT,RAMP|never] [--ramps-fx fx|instances] [--frames PATH]\n  --damage: animated card scenes and recoloured sparse FX layers, with dirty rows and full-render pixel verification\n  --only: run only the named scenes (episode, flowing, settled, typing, scroll, bolt, comet, classic, meteor, flood, rain, cats, pet, icons; fx_sparse under --damage; composer only when named)\n  --light: every scene on Default Light\n  --theme NAME, --theme=NAME: every scene on the named built-in theme (\"Solarized Light\", \"Catppuccin Latte\", ...)"
        );
        return;
    }
    if !cfg!(any(debug_assertions, feature = "dev-seams")) {
        fail(NO_SEAM);
    }
    fn noop(_: gpu_time::GpuFrameTime) {}
    if !gpu_time::install_sink(noop) && !gpu_time::sink_installed() {
        fail("could not install the GPU-time sink");
    }
    aterm_pty::declare_interactive_thread();
    let knobs = Knobs::from_args();
    let mut rig = Rig::new(&knobs);
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
        cold_uploads: 0,
    };
    let damage = std::env::args().skip(1).any(|a| a == "--damage");
    let only: Option<Vec<String>> = std::env::args()
        .skip_while(|a| a != "--only")
        .nth(1)
        .map(|list| list.split(',').map(str::to_string).collect());
    let named = |name: &str| only.as_ref().is_some_and(|o| o.iter().any(|n| n == name));
    let wanted = |name: &str| only.is_none() || named(name);
    let cards = [("episode", true), ("flowing", true), ("settled", false)];
    let rest: &[(&str, bool)] = if damage {
        &[("fx_sparse", true)]
    } else {
        &[
            ("typing", true),
            ("scroll", true),
            ("bolt", true),
            ("comet", true),
            ("classic", true),
            ("meteor", true),
            ("flood", true),
            ("rain", true),
            ("cats", true),
            ("pet", true),
            ("icons", true),
        ]
    };
    // The scenes a bare run leaves out, so the `effects` lane's frames stay its own: each runs
    // only when `--only` names it (`composer`, the `trail-*-composer` lanes' scene).
    let named_only: &[(&str, bool)] = if damage { &[] } else { &[("composer", true)] };
    let mut scenes: Vec<Scene> = cards
        .iter()
        .chain(rest)
        .filter(|(name, _)| wanted(name))
        .chain(named_only.iter().filter(|(name, _)| named(name)))
        .map(|&(name, rate_emit)| scene(name, rate_emit))
        .collect();
    if scenes.is_empty() {
        fail("--only names no scene");
    }
    let names: Vec<&str> = scenes.iter().map(|s| s.name).collect();
    let wanted = |name: &str| names.contains(&name);
    for run in 0..WARMUP + RUNS {
        let seed = 0x00C0_FFEE + run as u64;
        let mut runs: Vec<(&str, Run)> = Vec::new();
        if ["episode", "flowing", "settled"].iter().any(|n| wanted(n)) {
            let (episode, flowing, settled) = card_scenes(&mut rig, seed);
            runs.extend([
                ("episode", episode),
                ("flowing", flowing),
                ("settled", settled),
            ]);
        }
        if wanted("fx_sparse") {
            runs.push(("fx_sparse", sparse_fx_scene(&mut rig)));
        }
        let typed = |keys| Script::Type {
            keys,
            every: KEY_TICKS,
        };
        if wanted("typing") {
            let typing = typing_scene(
                &mut rig,
                GlowStyle::RainbowKitty,
                Page::Shell,
                typed(TYPING_KEYS),
            );
            runs.push(("typing", typing));
        }
        if wanted("scroll") {
            runs.push(("scroll", scroll_scene(&mut rig)));
        }
        for (name, style) in [
            ("bolt", GlowStyle::Laser),
            ("comet", GlowStyle::Comet),
            ("classic", GlowStyle::Classic),
        ] {
            if wanted(name) {
                let run = typing_scene(&mut rig, style, Page::Shell, typed(PRODUCER_KEYS));
                runs.push((name, run));
            }
        }
        if wanted("meteor") {
            let hops = Script::Hop {
                hops: HOPS,
                every: HOP_TICKS,
            };
            runs.push((
                "meteor",
                typing_scene(&mut rig, GlowStyle::RainbowKitty, Page::Shell, hops),
            ));
        }
        if wanted("flood") {
            let flood = Script::Type {
                keys: FLOOD_KEYS,
                every: 1,
            };
            let flood = typing_scene(&mut rig, GlowStyle::RainbowKitty, Page::Shell, flood);
            at_the_ribbon_budget(&flood);
            runs.push(("flood", flood));
        }
        if wanted("composer") {
            let line = Script::Type {
                keys: COMPOSER_KEYS,
                every: COMPOSER_KEY_TICKS,
            };
            let composer = typing_scene(&mut rig, GlowStyle::RainbowKitty, Page::Composer, line);
            runs.push(("composer", composer));
        }
        if wanted("rain") {
            runs.push(("rain", rain_scene(&mut rig, seed)));
        }
        if wanted("cats") {
            runs.push(("cats", cats_scene(&mut rig)));
        }
        if wanted("pet") {
            runs.push(("pet", pet_scene(&mut rig, seed)));
        }
        if wanted("icons") {
            runs.push(("icons", icons_scene(&mut rig)));
        }
        if run < WARMUP {
            for (name, run) in runs {
                if let Some(scene) = scenes.iter_mut().find(|s| s.name == name) {
                    scene.cold_uploads += run.samples.iter().map(|x| x.uploads).sum::<u64>();
                }
            }
            continue;
        }
        for (name, run) in runs {
            if let Some(scene) = scenes.iter_mut().find(|s| s.name == name) {
                scene.runs.push(run);
            }
        }
        eprintln!("effects_harness: run {}/{RUNS} done", run + 1 - WARMUP);
    }
    if let Some(path) = std::env::args().skip_while(|a| a != "--frames").nth(1) {
        write_frames(&path, &scenes);
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
    report.text("theme", &rig.theme_name);
    report.text(
        "ground",
        if dark_ground(rig.theme.bg) {
            "dark"
        } else {
            "light"
        },
    );
    report.text(
        "fetch_probe",
        rig.gpu.fetch_probe().map_or("unminted", FetchProbe::name),
    );
    let (ramp_cost, ramps_fx) = knobs.names();
    report.text("ramp_cost", &ramp_cost);
    report.text("ramps_fx", ramps_fx);
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
             | passes {} (drop {}, split {}) | pass cost {} us = {}% of present p95 {} us \
             | split present p95 {} us, route gain {} us = {}% | unorm draws {} us \
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
            get("split_passes_max"),
            get("pass_cost_p50_us"),
            get("pass_cost_pct"),
            get("present_p95_us"),
            get("split_present_p95_us"),
            get("route_gain_p50_us"),
            get("route_gain_pct"),
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
        if get("sprites_max") != "0" {
            eprintln!(
                "effects_harness: {:<8} sprites p50 {} max {} | uploads p50 {} max {} per run {} \
                 cold {} bytes | fx instances p50 {} max {} | reader waits per run {} ({} ns, \
                 max {} ns a frame)",
                scene.name,
                get("sprites_p50"),
                get("sprites_max"),
                get("upload_bytes_p50"),
                get("upload_bytes_max"),
                get("upload_bytes_per_run"),
                get("upload_bytes_cold"),
                get("fx_instances_p50"),
                get("fx_instances_max"),
                get("reader_waits_per_run"),
                get("reader_wait_ns_per_run"),
                get("reader_wait_ns_max"),
            );
        }
    }
    println!("{}", report.json());
}
