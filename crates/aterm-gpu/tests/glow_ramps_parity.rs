// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lane B5's law: a glow stream's ramps draw byte-identical to the quads they
//! stand for. The Rainbow Kitty ribbon and meteor trains, laid as ramps and
//! read so by the letter guard, draw on the CPU and on the GPU arm exactly
//! the frames their legacy stream
//! (every row a quad, `CursorGlow::set_glow_ramps(false)`) draws, damage-cached
//! frame after frame, with the bloom on and, on each frame it reads, off
//! ([`Bloom`]), on a dark and a light page;
//! each stream counts every lit ramp row as its quad and marks the same rows.
//! A stream whose lights from its first ramp on all add draws its quads ahead
//! of its ramps on the GPU, and one with an `over` light among them in order;
//! the meteor scenes show both. The present draws them alike too: its bloom
//! and its SDR crown read the
//! additive stream's ramps as an item list, rows unexpanded, and present the
//! legacy stream's bytes. A comet slab's column rides a ramp, its core
//! the ramp's run (one light as tall as its rows): the comet, laser (its
//! bolts with it), classic and fire streaks lay them. Over fire, the GPU's
//! heat shimmer reads the additive stream's ramps for its hot box and heat
//! bands, its phase pinned so both streams' frames meet the same haze.
//! Beside a frame's own fx art (exact layers at Under, Glow and Over and a
//! linear one at Over, the fx streams the pet's and the word icons' frames
//! open), the ramps drawn in that fx stream draw the quads' bytes too, and
//! the measured rule draws them on Metal as the glow instances they stand
//! for. A ramp row whose two ends differ in opacity (a faint end beside a
//! near-opaque one, an additive end beside an opaque one) draws the same
//! bytes on the CPU and on every GPU arm and route, laid under any cost, and
//! so does a frame of the light glass's wedge, its flat runs beside ramps
//! from zero opacity, and a frame of the glass typed on Default Light with
//! its wedge rows, its yield and a selection under it.

use crate::common;
use aterm_core::render::RenderInput;
use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, FxSprite, FxTexels,
    PxRect,
};
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_fx::CursorFxInput;
use aterm_effects::cursor_glow::{CURSOR_WITNESS_ROWS, Geom, GlowConfig, GlowStyle, ProbeTrust};
use aterm_effects::cursor_trail::TrailConfig;
use aterm_effects::host::motion_print_anchor;
use aterm_effects::pipeline::{CursorStep, EffectsPipeline, RowProbe};
use aterm_gpu::RampsFx;
use aterm_render::{
    BeamRows, GlowBlend, GlowQuad, GlowRamps, RampCost, RampSink, Theme, WindowCpu,
};
use aterm_time::Instant;
use std::sync::Arc;
use std::time::Duration;

const PX: f32 = 18.0;
const ROWS: usize = 12;
const COLS: usize = 72;
const HZ: u32 = 120;
const PROSE: &[u8] = b"the quick brown fox jumps over the lazy dog, then naps ";
/// Keys typed, a key every this many frames, and the frames after.
const KEYS: usize = 20;
const KEY_TICKS: usize = 8;
const RUNOUT: usize = 48;
/// Hops between two columns of the prompt's row (each a meteor), a hop every
/// this many frames.
const HOPS: usize = 6;
const HOP_TICKS: usize = 20;
const HOP_COLS: (u16, u16) = (16, 60);

const LIGHT: Theme = Theme {
    fg: 0x0020_2020,
    bg: 0x00F4_F1EA,
    cursor: 0x0050_FA7B,
    selection: 0x0033_415E,
};

#[derive(Clone, Copy)]
enum Script {
    Type,
    Hop,
}

/// What the page above the prompt holds: a listing to its row, or nothing,
/// the open rows the heat shimmer's haze rises through.
#[derive(Clone, Copy)]
enum Page {
    Listing,
    Open,
}

/// The heat shimmer's phase, pinned on every arm (its one wall-clock input).
const SHIMMER_PHASE: f32 = 0.37;
/// The SDR crown's strength (`cursor_glow_sdr_boost`'s default), its ease
/// reset before each present so both streams meet the same budget.
const CROWN: f32 = 0.25;

/// One effects pipeline and the frame it publishes into.
struct Side {
    fx: EffectsPipeline,
    input: RenderInput,
    row: Vec<char>,
    above: Vec<char>,
    below: Vec<char>,
    witness: Vec<char>,
}

impl Side {
    /// A pipeline laying its beams' runs under `cost` ([`RampCost::NEVER`]:
    /// the legacy stream; `None`: the measured cost), guarded or not.
    fn new(cost: Option<RampCost>, guard: bool) -> Self {
        let mut fx = EffectsPipeline::for_window();
        match cost {
            Some(cost) => fx.cursor_mut().glow.set_ramp_cost(cost),
            None => fx.cursor_mut().glow.set_glow_ramps(true),
        }
        fx.set_letter_guard(guard);
        Self {
            fx,
            input: RenderInput::empty(),
            row: Vec::new(),
            above: Vec::new(),
            below: Vec::new(),
            witness: Vec::new(),
        }
    }

    /// One frame: the cursor step over `base` (the terminal's frame), then
    /// its streams published into [`Self::input`].
    fn frame(
        &mut self,
        term: &Terminal,
        base: &RenderInput,
        cfg: &GlowConfig,
        geom: Geom,
        now: Instant,
    ) {
        let caret = (base.cursor_row as u16, base.cursor_col as u16);
        let (row, col) = caret;
        let below = usize::from(row) + 1 < ROWS;
        term.row_cols_into(usize::from(row), &mut self.row);
        if row > 0 {
            term.row_cols_into(usize::from(row) - 1, &mut self.above);
        }
        if below {
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
        let _ = self.fx.cursor_step(
            &CursorStep::new(
                CursorFxInput {
                    now,
                    cur: Some(caret),
                    live_viewport: true,
                    cursor_style: term.cursor_style(),
                    blink_phase: true,
                    live_cursor: cfg.color,
                    cursor_color_pinned: false,
                    default_bg: cfg.theme_bg,
                    cursor_cell_bg: None,
                    geom,
                    focused: true,
                    shed_envelope: 1.0,
                    trail_brightness: 1.0,
                    body_allowed: true,
                    momentum_glow: true,
                    user_tinted: false,
                    accent_pinned: false,
                },
                cfg,
                TrailConfig {
                    enabled: false,
                    duration: cfg.duration,
                    max_len: cfg.length,
                    color: cfg.color,
                    intensity: cfg.intensity,
                    warmth: 0.0,
                },
            )
            .with_row_probe(Some(RowProbe {
                row,
                caret: col,
                trust: ProbeTrust::Full,
                cols: &self.row,
                flanks: Some((
                    (row > 0).then_some(self.above.as_slice()),
                    below.then_some(self.below.as_slice()),
                )),
            }))
            .with_frame_cells(Some(base.cells.as_slice()))
            .with_frame_cells_cols(base.cols)
            .with_frame_marks(&base.combining)
            .with_print_anchor(print_anchor)
            .with_print_anchor_glyph(print_anchor_glyph),
            std::iter::empty(),
        );
        self.input.clone_from(base);
        self.fx.publish_cursor(&mut self.input);
        self.input.refresh_cursor_effect_damage();
    }
}

fn config(theme: Theme, (style, classic_mono): (GlowStyle, bool)) -> GlowConfig {
    GlowConfig {
        classic_mono,
        dark_theme: aterm_render::theme_is_dark(theme.bg),
        theme_fg: theme.fg,
        theme_bg: theme.bg,
        style,
        color: 0x00d0_d0d0,
        accent: 0x0048_c9ff,
        duration: Duration::from_millis(650),
        length: usize::MAX,
        radius: 0.4,
        ring: true,
        beam: aterm_effects::cursor_glow::style_has_beam_of(style, ""),
        ..Default::default()
    }
}

/// The rows a frame's glow streams mark dirty.
fn marked(input: &RenderInput) -> Vec<bool> {
    let mut rows = vec![false; 2 * ROWS];
    input.cursor_glow_add_damage.mark_rows(&mut rows[..ROWS]);
    input.glow_under_damage.mark_rows(&mut rows[ROWS..]);
    rows
}

/// What a scene showed: the frames whose ramps the guard cut, the most ramps
/// and runs a frame carried, the streams with ramps whose quads the GPU
/// moves ahead of them past a ramp (every light from the first ramp on adds)
/// or keeps in order (an `over` light among them), and the frames whose
/// glow the heat shimmer read while the stream carried ramps.
#[derive(Default)]
struct Seen {
    frames: usize,
    ramps: usize,
    /// The most ramps a frame laid under the measured cost.
    measured: usize,
    /// The GPU frames whose measured-cost ramps drew as their quads, opening
    /// no fx stream the legacy frame did not, and those that opened one.
    shut: usize,
    opened: usize,
    runs: usize,
    guarded: usize,
    apart: usize,
    ordered: usize,
    shimmered: usize,
    /// The presents of the additive stream's ramps (its item list) the bloom changed: the same
    /// frame with the bloom off ([`Bloom::Toggled`]) presented other bytes.
    bloomed: usize,
    /// Whether the crown changed a present of those ramps: the same frame with the crown off
    /// presented other bytes. A scene stops asking at the first, so this does not say on how many
    /// it drew: one more present of every such frame cost 11.6% of this file. A dark page gives the
    /// crown its budget; on [`LIGHT`] the additive stream carried no ramps in any scene here.
    crowned: bool,
    /// The frames also drawn with the bloom off ([`Bloom::Toggled`]).
    toggled: usize,
    /// The GPU frames whose ramps rode an fx stream that also drew the
    /// frame's art, and those whose measured-cost ramps beside art drew as
    /// their quads or rode it.
    beside_art: usize,
    art_shut: usize,
    art_opened: usize,
}

impl std::ops::AddAssign for Seen {
    fn add_assign(&mut self, o: Self) {
        self.frames += o.frames;
        self.ramps = self.ramps.max(o.ramps);
        self.measured = self.measured.max(o.measured);
        self.shut += o.shut;
        self.opened += o.opened;
        self.runs = self.runs.max(o.runs);
        self.guarded += o.guarded;
        self.apart += o.apart;
        self.ordered += o.ordered;
        self.shimmered += o.shimmered;
        self.bloomed += o.bloomed;
        self.crowned |= o.crowned;
        self.toggled += o.toggled;
        self.beside_art += o.beside_art;
        self.art_shut += o.art_shut;
        self.art_opened += o.art_opened;
    }
}

impl Seen {
    fn route(&mut self, quads: &[GlowQuad], ramps: &GlowRamps) {
        let Some(first) = ramps.items.first() else {
            return;
        };
        let after = &quads[(first.at as usize).min(quads.len())..];
        let adds = after.iter().all(|q| q.alpha == 0 && q.alpha2 == 0)
            && ramps.ends.iter().all(|&e| aterm_render::end_alpha(e) == 0);
        if !adds {
            self.ordered += 1;
        } else if !after.is_empty() {
            self.apart += 1;
        }
    }
}

/// What a GPU frame drew and uploaded: its instances a stream, their bytes,
/// and its fx instances and upload bytes ([`aterm_gpu::FrameShape`]).
#[derive(Debug, PartialEq, Eq)]
struct Drawn {
    instances: Vec<u32>,
    bytes: u64,
    fx: (u32, u64),
}

fn drawn(shape: &aterm_gpu::FrameShape) -> Drawn {
    Drawn {
        instances: shape.instances.to_vec(),
        bytes: shape.instance_bytes,
        fx: (shape.fx_instances, shape.fx_upload_bytes),
    }
}

/// Which measured route costs the arm `arm` names draws by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Costs {
    /// Metal's: ramps open the fx stream from about 1,460 lit rows, and
    /// beside art they draw as the glow instances they stand for.
    Metal,
    /// None measured: every ramp draws as the glow instances it stands for.
    Unmeasured,
}

fn costs(arm: &str) -> Costs {
    if !cfg!(target_os = "macos") {
        Costs::Unmeasured
    } else {
        assert_eq!(arm, "native", "the one arm a parity test runs on");
        Costs::Metal
    }
}

/// A frame's own fx art over the rows the beams cross (the prompt's and the
/// one above it), window pixels from `origin` in cells of `(cw, ch)`: exact
/// rects at Under, Glow and Over, which share the fx streams the glow
/// ramps ride, and a linear sprite at Over, a stream of its own (the pet's
/// and the word icons' sprites are linear layers).
fn art(origin: (usize, usize), (cw, ch): (usize, usize), prompt_row: usize) -> Vec<FxLayer> {
    let px = |v: usize| i32::try_from(v).expect("a frame within i32");
    let at = |k: usize| {
        (
            px(origin.0 + (6 + 9 * k) * cw),
            px(origin.1 + (prompt_row - 1 + k % 2) * ch + 3),
        )
    };
    let (w, h) = (
        u16::try_from(5 * cw).expect("a cell within u16"),
        u16::try_from(ch / 2).expect("a cell within u16"),
    );
    let layer = |id: u64, slot: FxSlot, blend: FxBlend, items: Vec<FxItem>, ink: FxInk| {
        FxLayer::new(
            0x4152_5400 + id,
            Arc::new(FxContent::new(items, vec![ink; 2])),
            slot,
            FxSpace::Window,
            blend,
        )
    };
    let rects = |from: usize| {
        (from..from + 4)
            .map(|k| {
                let (x, y) = at(k);
                FxItem::Rect(FxRect { x, y, w, h, ink: 0 })
            })
            .collect()
    };
    let texels: Vec<u8> = (0..16u8)
        .flat_map(|k| {
            [
                0x30 + 9 * k,
                0xC0 - 7 * k,
                0x60,
                [0, 0x50, 0xB0, 0xFF][usize::from(k % 4)],
            ]
        })
        .collect();
    let sprite = Arc::new(FxArt::new(4, 4, FxTexels::Rgba8Straight, texels).expect("sized"));
    let sprites = (3..7)
        .map(|k| {
            let (x, y) = at(k);
            FxItem::Sprite(FxSprite {
                x,
                y,
                dw: w,
                dh: h,
                flip_x: k % 2 == 1,
                art: Arc::clone(&sprite),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })
        })
        .collect();
    vec![
        layer(
            0,
            FxSlot::Under,
            FxBlend::Exact,
            rects(0),
            FxInk::over(0x0030_80C0, 0xA0),
        ),
        layer(
            1,
            FxSlot::Glow,
            FxBlend::Exact,
            rects(1),
            FxInk::add(0x0040_2010),
        ),
        layer(
            2,
            FxSlot::Over,
            FxBlend::Exact,
            rects(2),
            FxInk::over(0x00C0_4080, 0x60),
        ),
        layer(
            3,
            FxSlot::Over,
            FxBlend::Linear,
            sprites,
            FxInk::over(0x00FF_FFFF, 0xFF),
        ),
    ]
}

/// How a scene holds the GPU's bloom. The switch reaches a frame only through
/// `enable_bloom && bloom_source_live(input)` (the renderer's readback and present, on every
/// arm), so a frame with no live source draws one thing either way, and a CPU draw or an effect
/// pipeline reads no switch at all.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bloom {
    /// On throughout, every assert judged there; and on each frame whose source is live, off for
    /// one more pair of draws and presents on windows of their own, where the ramps must draw as
    /// their quads. Off, the measured cost's ramps, the ramps drawn as glow instances and the
    /// `drawn` shapes are not judged: a fault only those would show with the bloom off, and no
    /// halo masking it, goes unseen.
    Toggled,
    /// Off from the first frame, every assert judged there: renderer state that builds up only
    /// under a switch never on, which [`Self::Toggled`]'s frames cannot see. The cheapest case
    /// holds it ([`typed_ribbon_ramps_draw_their_quads_on_every_arm`]). It cannot say the switch
    /// kept the bloom out: `bloom_source_live_for_test` reads the frame and never the switch, and
    /// no present here has the bloom on to compare with. [`Self::Toggled`]'s frames read that
    /// ([`Seen::bloomed`]).
    NeverOn,
}

/// The RGBA of `input` through `win`'s virtual present, its ease reset.
fn presented(
    gpu: &mut aterm_gpu::GpuRenderer,
    win: &mut aterm_gpu::WindowGpu,
    input: &RenderInput,
    (arm, tick): (&str, usize),
) -> Vec<u8> {
    gpu.reset_glow_ease_for_test(win);
    assert!(
        gpu.present_virtual(win, input, false, None, None),
        "{arm} frame {tick}: the virtual present cannot drop"
    );
    gpu.virtual_presented_snapshot_current(win, win.resident_input_epoch(), tick as u64 + 1)
        .and_then(aterm_gpu::video_tap::RawCapture::convert)
        .expect("the presented frame")
        .rgba
}

/// Run `script` in `style` (and its classic face) on `theme`, its bloom held as `bloom` says,
/// over `page`, every frame drawn by each arm's CPU and GPU from the ramps and from the legacy
/// stream, and carrying the fx art of [`art`] beside its glow where `with_art`.
fn scene(
    theme: Theme,
    bloom: Bloom,
    (script, page): (Script, Page),
    style: (GlowStyle, bool),
    with_art: bool,
) -> Seen {
    let mut arms = common::arms(PX, theme);
    let (cw, ch) = arms[0].1.cell_size();
    for (_, cpu, gpu) in &mut arms {
        cpu.debug_block_on_lazy_fallbacks();
        gpu.set_bloom(bloom == Bloom::Toggled);
        gpu.set_shimmer_phase_for_test(Some(SHIMMER_PHASE));
        gpu.set_sdr_glow_boost(CROWN);
    }
    let (frame_w, frame_h) = arms[0].1.frame_size(ROWS, COLS);
    let px = |v: usize| u16::try_from(v).expect("a geometry within u16");
    let geom = Geom {
        cw,
        ch,
        rows: ROWS,
        cols: COLS,
        origin_x: px(arms[0].1.pad()),
        origin_y: px(arms[0].1.grid_top()),
        win_w: px(frame_w),
        win_h: px(frame_h),
        head: 0,
    };
    let cfg = config(theme, style);
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    for i in 0..ROWS - 3 {
        match page {
            Page::Listing => term.process(
                format!("drwxr-xr-x  12 user  staff   384 Oct 19 09:{i:02} crate-{i}\r\n")
                    .as_bytes(),
            ),
            Page::Open => term.process(b"\r\n"),
        }
    }
    term.process(b"~/src/aterm $ ");
    let prompt_row = ROWS as u16 - 3;
    let layers = if with_art {
        art(
            (usize::from(geom.origin_x), usize::from(geom.origin_y)),
            (cw, ch),
            usize::from(prompt_row),
        )
    } else {
        Vec::new()
    };
    let mut base = RenderInput::empty();
    // Every run a ramp, however short, so each producer's runs ride them;
    // the runs the measured cost lays as ramps; the legacy stream.
    let (mut ramps, mut measured, mut legacy) = (
        Side::new(Some(RampCost::ALWAYS), true),
        Side::new(None, true),
        Side::new(Some(RampCost::NEVER), true),
    );
    // The guard's negative control: the same ramps, unguarded.
    let mut bare = Side::new(Some(RampCost::ALWAYS), false);
    let mut wins: Vec<_> = arms
        .iter()
        .map(|_| {
            (
                [WindowCpu::new(), WindowCpu::new(), WindowCpu::new()],
                [
                    aterm_gpu::WindowGpu::new(),
                    aterm_gpu::WindowGpu::new(),
                    aterm_gpu::WindowGpu::new(),
                    aterm_gpu::WindowGpu::new(),
                ],
                // The presents of the ramps and of the legacy stream, and the ramps' with the crown
                // off.
                [
                    aterm_gpu::WindowGpu::new(),
                    aterm_gpu::WindowGpu::new(),
                    aterm_gpu::WindowGpu::new(),
                ],
                // The ramps' and the legacy stream's draws and presents with the bloom off.
                [
                    aterm_gpu::WindowGpu::new(),
                    aterm_gpu::WindowGpu::new(),
                    aterm_gpu::WindowGpu::new(),
                    aterm_gpu::WindowGpu::new(),
                ],
            )
        })
        .collect();
    let (events, every) = match script {
        Script::Type => (KEYS, KEY_TICKS),
        Script::Hop => (HOPS, HOP_TICKS),
    };
    let t0 = Instant::now();
    let period = Duration::from_secs(1) / HZ;
    let mut seen = Seen::default();
    for tick in 0..events * every + RUNOUT {
        let now = t0 + period * tick as u32;
        if tick % every == 0 && tick / every < events {
            let k = tick / every;
            match script {
                Script::Type => term.process(&[PROSE[k % PROSE.len()]]),
                Script::Hop => {
                    let col = if k % 2 == 0 { HOP_COLS.1 } else { HOP_COLS.0 };
                    term.process(format!("\x1b[{};{}H", prompt_row + 1, col + 1).as_bytes());
                }
            }
            for side in [&mut ramps, &mut measured, &mut legacy, &mut bare] {
                let glow = &mut side.fx.cursor_mut().glow;
                match script {
                    Script::Type => glow.note_synthetic_typed(now, 1),
                    Script::Hop => glow.note_synthetic_move(now),
                }
            }
        }
        let _ = term.cell_frame_damage_scoped_into(&mut base, ROWS, COLS);
        for side in [&mut ramps, &mut measured, &mut legacy, &mut bare] {
            side.frame(&term, &base, &cfg, geom, now);
            side.input.fx_layers.clone_from(&layers);
        }
        let (r, m, l) = (&ramps.input, &measured.input, &legacy.input);
        assert!(
            l.glow_under_ramps.is_empty() && l.cursor_glow_add_ramps.is_empty(),
            "frame {tick}: the legacy stream lays no ramp"
        );
        for (x, what) in [(r, "every run a ramp"), (m, "the measured cost")] {
            assert_eq!(
                (
                    x.glow_under_ramps.laid(x.glow_under.len()),
                    x.cursor_glow_add_ramps.laid(x.cursor_glow_add.len())
                ),
                (l.glow_under.len(), l.cursor_glow_add.len()),
                "frame {tick} ({what}): every lit ramp row counts as its quad"
            );
            assert_eq!(
                marked(x),
                marked(l),
                "frame {tick} ({what}): the streams mark the same rows"
            );
        }
        seen.measured = seen
            .measured
            .max(m.glow_under_ramps.items.len() + m.cursor_glow_add_ramps.items.len());
        let carried = r.glow_under_ramps.items.len() + r.cursor_glow_add_ramps.items.len();
        seen.ramps = seen.ramps.max(carried);
        let runs = [&r.glow_under_ramps, &r.cursor_glow_add_ramps]
            .iter()
            .flat_map(|ramps| &ramps.items)
            .filter(|ramp| ramp.joined > 0)
            .count();
        seen.runs = seen.runs.max(runs);
        seen.frames += 1;
        seen.route(&r.glow_under, &r.glow_under_ramps);
        seen.route(&r.cursor_glow_add, &r.cursor_glow_add_ramps);
        let b = &bare.input;
        if (&b.glow_under_ramps, &b.cursor_glow_add_ramps)
            != (&r.glow_under_ramps, &r.cursor_glow_add_ramps)
        {
            seen.guarded += 1;
        }
        let haze = arms[0].2.shimmer_region_for_test(r);
        assert_eq!(
            haze,
            arms[0].2.shimmer_region_for_test(l),
            "frame {tick}: the heat shimmer reads the ramps as their quads"
        );
        if haze.is_some() && !r.cursor_glow_add_ramps.is_empty() {
            seen.shimmered += 1;
        }
        let items = !r.cursor_glow_add_ramps.is_empty();
        let toggled =
            bloom == Bloom::Toggled && aterm_gpu::GpuRenderer::bloom_source_live_for_test(r);
        for ((arm, cpu, gpu), (cwins, gwins, pwins, owins)) in arms.iter_mut().zip(&mut wins) {
            let [cr, cm, cl] = cwins;
            let a = cpu.render_input_cached(cr, r).pixels().to_vec();
            let am = cpu.render_input_cached(cm, m).pixels().to_vec();
            let b = cpu.render_input_cached(cl, l).pixels().to_vec();
            assert!(
                a == b,
                "{arm} frame {tick}: the CPU draws the ramps as their quads"
            );
            assert!(
                am == b,
                "{arm} frame {tick}: the CPU draws the measured cost's ramps as their quads"
            );
            // Every run's ramps in the fx stream, the same ramps as the
            // glow instances their rows stand for, the measured cost's
            // ramps where the measured rule routes them, and the legacy
            // stream.
            let [gr, ge, gm, gl] = gwins;
            gpu.set_ramps_fx_for_test(Some(RampsFx::Always));
            let a = gpu.render_input(gr, r, None).pixels;
            let drawn_a = drawn(&gpu.frame_shape());
            common::assert_arm(arm, gpu);
            gpu.set_ramps_fx_for_test(Some(RampsFx::Never));
            let e = gpu.render_input(ge, r, None).pixels;
            let drawn_e = drawn(&gpu.frame_shape());
            gpu.set_ramps_fx_for_test(None);
            let am = gpu.render_input(gm, m, None).pixels;
            let drawn_m = drawn(&gpu.frame_shape());
            let b = gpu.render_input(gl, l, None).pixels;
            let drawn_l = drawn(&gpu.frame_shape());
            assert!(
                a == b,
                "{arm} frame {tick}: the GPU draws the ramps as their quads"
            );
            assert!(
                e == b,
                "{arm} frame {tick}: the GPU draws the ramps' rows as their quads"
            );
            assert!(
                am == b,
                "{arm} frame {tick}: the GPU draws the measured cost's ramps as their quads"
            );
            assert_eq!(
                drawn_e, drawn_l,
                "{arm} frame {tick}: ramps drawn as their quads draw the legacy frame's \
                 instances and open no fx stream"
            );
            let art_fx = drawn_l.fx.0 > 0;
            seen.beside_art += usize::from(art_fx && drawn_a.fx.0 > drawn_l.fx.0);
            let m_ramps = !(m.glow_under_ramps.is_empty() && m.cursor_glow_add_ramps.is_empty());
            match costs(arm) {
                Costs::Metal if art_fx && m_ramps => assert_eq!(
                    drawn_m, drawn_l,
                    "{arm} frame {tick}: on Metal the measured rule draws ramps beside art as \
                     the glow instances they stand for"
                ),
                Costs::Unmeasured => assert_eq!(
                    drawn_m, drawn_l,
                    "{arm} frame {tick}: an arm whose route costs were not measured draws the \
                     measured cost's ramps as the glow instances they stand for"
                ),
                Costs::Metal => {}
            }
            if !m_ramps {
                assert_eq!(
                    drawn_m, drawn_l,
                    "{arm} frame {tick}: a frame with no ramp draws the legacy frame"
                );
            } else if drawn_m.fx == drawn_l.fx {
                assert_eq!(
                    drawn_m, drawn_l,
                    "{arm} frame {tick}: ramps that open no fx stream draw the legacy frame"
                );
                seen.shut += usize::from(drawn_l.fx.0 == 0);
                seen.art_shut += usize::from(drawn_l.fx.0 > 0);
            } else {
                seen.opened += 1;
                seen.art_opened += usize::from(drawn_l.fx.0 > 0);
            }
            let mut present = None;
            if items {
                gpu.set_ramps_fx_for_test(None);
                let [pr, pl, pc] = pwins;
                let a = presented(gpu, pr, r, (arm, tick));
                let b = presented(gpu, pl, l, (arm, tick));
                assert!(
                    a == b,
                    "{arm} frame {tick}: the present's bloom and crown draw the ramps as their \
                     quads (first diff byte {:?})",
                    a.iter().zip(&b).position(|(x, y)| x != y)
                );
                if !seen.crowned {
                    gpu.set_sdr_glow_boost(0.0);
                    seen.crowned = presented(gpu, pc, r, (arm, tick)) != a;
                    gpu.set_sdr_glow_boost(CROWN);
                }
                present = Some(a);
            }
            if toggled {
                gpu.set_bloom(false);
                let [ora, orb, opa, opb] = owins;
                gpu.set_ramps_fx_for_test(Some(RampsFx::Always));
                let a = gpu.render_input(ora, r, None).pixels;
                gpu.set_ramps_fx_for_test(None);
                let b = gpu.render_input(orb, l, None).pixels;
                assert!(
                    a == b,
                    "{arm} frame {tick}: with the bloom off the GPU draws the ramps as their quads"
                );
                if items {
                    let a = presented(gpu, opa, r, (arm, tick));
                    let b = presented(gpu, opb, l, (arm, tick));
                    assert!(
                        a == b,
                        "{arm} frame {tick}: with the bloom off the present's crown draws the \
                         ramps as their quads (first diff byte {:?})",
                        a.iter().zip(&b).position(|(x, y)| x != y)
                    );
                    seen.bloomed += usize::from(present.as_ref().is_some_and(|on| *on != a));
                }
                seen.toggled += 1;
                gpu.set_bloom(true);
            }
        }
    }
    seen
}

/// [`Seen`]s summed, as `+=` adds two.
impl std::iter::Sum for Seen {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |mut all, seen| {
            all += seen;
            all
        })
    }
}

/// The two scripts a style runs, typed and leapt.
const TYPED_AND_LEAPT: [(Script, &str); 2] = [(Script::Type, "typed"), (Script::Hop, "leapt")];

/// One case of [`scene`], named `what`: `script` in `style` over `page`,
/// every frame carrying fx art beside its glow where `with_art` ([`art`]).
struct Case {
    page: Page,
    script: Script,
    what: String,
    style: (GlowStyle, bool),
    with_art: bool,
}

impl Case {
    /// `style`'s cases over `page`, typed and leapt, each named `what`, its
    /// script and `tail`.
    fn typed_and_leapt(
        page: Page,
        what: &str,
        tail: &str,
        style: (GlowStyle, bool),
        with_art: bool,
    ) -> [Self; 2] {
        TYPED_AND_LEAPT.map(|(script, how)| Self {
            page,
            script,
            what: format!("{what} {how}{tail}"),
            style,
            with_art,
        })
    }
}

/// Run every case on both pages ([`Bloom::Toggled`]), its scenes on every
/// core ([`aterm_spec::check::each`]; each scene builds its own arms,
/// pipelines and windows), returning what each case's scenes showed
/// ([`Seen`], summed), in case order. Each scene that finished prints its
/// summary line, in scene order, before a scene's panic is raised again.
fn holds_all(cases: &[Case]) -> Vec<Seen> {
    let scenes: Vec<(usize, Theme)> = (0..cases.len())
        .flat_map(|c| [Theme::default(), LIGHT].map(|theme| (c, theme)))
        .collect();
    let shown = aterm_spec::check::each(&scenes, |&(c, theme)| {
        let case = &cases[c];
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scene(
                theme,
                Bloom::Toggled,
                (case.script, case.page),
                case.style,
                case.with_art,
            )
        }))
    });
    let mut all: Vec<Seen> = cases.iter().map(|_| Seen::default()).collect();
    let mut failed = None;
    for (&(c, theme), seen) in scenes.iter().zip(shown) {
        let seen = match seen {
            Ok(seen) => seen,
            Err(panic) => {
                eprintln!("{} (bg {:06x}) failed", cases[c].what, theme.bg);
                failed.get_or_insert(panic);
                continue;
            }
        };
        eprintln!(
            "{} (bg {:06x}): {} frames, up to {} ramps ({} runs, {} under \
             the measured cost) a frame, {} guarded, {} streams drawn apart, {} in order, \
             {} shimmered, measured ramps {} times in fx, {} times as quads, {} bloomed from \
             the ramps, crown drawn: {}, {} also drawn with the bloom off, {} drawn in fx \
             beside art, {} measured beside art as quads, {} in fx",
            cases[c].what,
            theme.bg,
            seen.frames,
            seen.ramps,
            seen.runs,
            seen.measured,
            seen.guarded,
            seen.apart,
            seen.ordered,
            seen.shimmered,
            seen.opened,
            seen.shut,
            seen.bloomed,
            seen.crowned,
            seen.toggled,
            seen.beside_art,
            seen.art_shut,
            seen.art_opened
        );
        all[c] += seen;
    }
    if let Some(panic) = failed {
        std::panic::resume_unwind(panic);
    }
    for (case, all) in cases.iter().zip(&all) {
        assert!(all.ramps > 0, "{}: the scenes laid ramps", case.what);
        assert!(
            all.toggled > 0,
            "{}: the bloom was off for a frame whose source is live",
            case.what
        );
        assert!(
            all.crowned,
            "{}: the crown changed the bytes of a present of the ramps",
            case.what
        );
    }
    all
}

/// Run `script` in `style` on both pages, returning what the scenes showed
/// ([`Seen`], summed).
fn holds(script: Script, what: &str, style: (GlowStyle, bool)) -> Seen {
    let case = Case {
        page: Page::Listing,
        script,
        what: what.to_owned(),
        style,
        with_art: false,
    };
    holds_all(&[case]).pop().expect("the case's scenes")
}

const KITTY: (GlowStyle, bool) = (GlowStyle::RainbowKitty, false);

/// The typist's ribbon over a listing, guarded.
#[test]
fn typed_ribbon_ramps_draw_their_quads_on_every_arm() {
    let seen = holds(Script::Type, "typing", KITTY);
    assert!(seen.guarded > 0, "typing: the guard cut the ribbon's ramps");
    assert!(seen.measured > 0, "typing: the measured cost laid ramps");
    assert!(
        seen.shut > 0,
        "typing: ramps whose rows do not pay for the fx stream drew as their quads, opening none"
    );
    assert_eq!(
        seen.opened > 0,
        cfg!(target_os = "macos"),
        "typing: ramps whose rows pay for the fx stream opened it, on Metal, the one arm whose \
         route costs were measured"
    );
    assert!(
        seen.ordered > 0,
        "typing: the ribbon's over light drew in order"
    );
    assert!(
        seen.bloomed > 0,
        "typing: the bloom read the ribbon's ramps, and the switch off took it out of the present"
    );
    let never = scene(
        Theme::default(),
        Bloom::NeverOn,
        (Script::Type, Page::Listing),
        KITTY,
        false,
    );
    assert!(
        never.ramps > 0 && never.crowned,
        "typing with the bloom never on: the ramps were laid, and the crown changed the bytes of \
         a present of them"
    );
}

/// Meteors hopping along the prompt's row.
#[test]
fn meteor_ramps_draw_their_quads_on_every_arm() {
    let seen = holds(Script::Hop, "meteor", KITTY);
    assert!(seen.guarded > 0, "meteor: the guard cut the scene's ramps");
    assert!(
        seen.apart > 0,
        "meteor: the dark trains' stream drew its quads first"
    );
    assert!(
        seen.ordered > 0,
        "meteor: a stream with over light drew in order"
    );
}

/// The classic face's mono bed under the letters, typed and leapt. The comet's
/// and the laser's beds (on the dark page, and their ink bodies on the light
/// one) ride the scenes of [`comet_slab_runs_draw_their_quads_on_every_arm`],
/// which checks each of them for ramps, runs and the guard's cuts.
#[test]
fn streak_bed_ramps_draw_their_quads_on_every_arm() {
    holds_all(&Case::typed_and_leapt(
        Page::Listing,
        "classic mono",
        "",
        (GlowStyle::Classic, true),
        false,
    ));
}

/// The comet slabs' columns, a run each core: the comet, the laser's beam and
/// bolts and the classic comet, typed and leapt, the guard cutting the comet's
/// and the laser's runs.
/// The same scenes lay the comet's and the laser's streak beds under the
/// letters (their ink bodies on the light page), each checked for ramps.
/// The fire streak's runs ride [`fire_heat_shimmer_reads_the_ramps_on_every_arm`]'s open page
/// instead. Over a page of letters it is not drawn in this file: a fire-only difference between
/// a guard-cut ramp and its quad, its heat shimmer's haze over a page of letters, and its leaps
/// across rows of letters go unseen.
#[test]
fn comet_slab_runs_draw_their_quads_on_every_arm() {
    let styles = [
        ((GlowStyle::Comet, false), "comet", true),
        ((GlowStyle::Laser, false), "laser", true),
        ((GlowStyle::Classic, false), "classic", false),
    ];
    let cases: Vec<Case> = styles
        .iter()
        .flat_map(|&(style, what, _)| Case::typed_and_leapt(Page::Listing, what, "", style, false))
        .collect();
    let mut shown = holds_all(&cases).into_iter();
    for (_, what, guarded) in styles {
        let seen: Seen = shown.by_ref().take(TYPED_AND_LEAPT.len()).sum();
        assert!(seen.runs > 0, "{what}: the slabs' cores rode runs");
        assert!(
            !guarded || seen.guarded > 0,
            "{what}: the guard cut the scene's ramps"
        );
    }
}

/// The fire streak typed and leapt over an open page: the heat shimmer's haze
/// rises over the hot glow, whose slabs ride ramps, and reads them for its
/// hot box and heat bands exactly as it reads their quads.
#[test]
fn fire_heat_shimmer_reads_the_ramps_on_every_arm() {
    let cases = Case::typed_and_leapt(
        Page::Open,
        "fire",
        ", open page",
        (GlowStyle::Fire, false),
        false,
    );
    let seen: Seen = holds_all(&cases).into_iter().sum();
    assert!(seen.runs > 0, "fire: the slabs' cores rode runs");
    assert!(
        seen.shimmered > 0,
        "fire: the heat shimmer read frames whose glow rode ramps"
    );
}

/// Light laid after a ramp, over it: an `over` quad does not commute with the
/// ramp it covers, so the GPU keeps that stream's order, and additive light
/// does, so it draws ahead of the ramp; both draw the bytes of the stream's
/// quads on every arm, in the glow and under-glow streams.
#[test]
fn over_light_after_a_ramp_keeps_its_order() {
    let mut arms = common::arms(PX, Theme::default());
    let (cw, ch) = arms[0].1.cell_size();
    let (x0, y0) = (arms[0].1.pad() + 4 * cw, arms[0].1.grid_top() + 2 * ch);
    let px = |v: usize| u16::try_from(v).expect("a geometry within u16");
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"~/src/aterm $ ls");
    let mut base = RenderInput::empty();
    let _ = term.cell_frame_damage_scoped_into(&mut base, ROWS, COLS);
    for alpha in [0u8, 0xC8] {
        let quad = |x: usize, y: usize, color: u32| GlowQuad {
            row: 2,
            x: px(x),
            y: px(y),
            w: px(3 * cw),
            h: px(ch / 2),
            color: aterm_render::premul_rgb(color, alpha.max(0x80)),
            alpha,
            color2: aterm_render::premul_rgb(color, 0x40),
            alpha2: alpha / 2,
        };
        let quads = vec![
            quad(x0, y0, 0x0030_60F0),
            quad(x0 + cw, y0 + 2, 0x00F0_8020),
        ];
        let mut ramps = GlowRamps::default();
        let end = |c: u32, a: u8| aterm_render::glow_end(aterm_render::premul_rgb(c, a), alpha);
        ramps.open(
            1,
            2,
            (px(x0 + cw / 2), px(y0), px(4 * cw)),
            (end(0x0020_E040, 0x80), end(0x00E0_20C0, 0x30)),
        );
        for k in 1..u8::try_from(ch / 2).expect("a cell height within u8") {
            ramps.extend_last(
                0,
                (end(0x0020_E040, 0x80 - 2 * k), end(0x00E0_20C0, 0x30 + k)),
            );
        }
        let mut r = base.clone();
        let mut l = base.clone();
        (r.cursor_glow_add, r.cursor_glow_add_ramps) = (quads.clone(), ramps.clone());
        (r.glow_under, r.glow_under_ramps) = (quads.clone(), ramps.clone());
        ramps.expand_into(&quads, &mut l.cursor_glow_add);
        ramps.expand_into(&quads, &mut l.glow_under);
        let mut seen = Seen::default();
        seen.route(&r.cursor_glow_add, &r.cursor_glow_add_ramps);
        assert_eq!(
            (seen.apart, seen.ordered),
            if alpha == 0 { (1, 0) } else { (0, 1) },
            "alpha {alpha}: the stream's route"
        );
        for (arm, cpu, gpu) in &mut arms {
            let a = cpu.render_input(&r).pixels;
            let b = cpu.render_input(&l).pixels;
            assert!(
                a == b,
                "{arm} alpha {alpha}: the CPU draws the ramp in its place"
            );
            gpu.set_ramps_fx_for_test(Some(RampsFx::Always));
            let a = gpu
                .render_input(&mut aterm_gpu::WindowGpu::new(), &r, None)
                .pixels;
            common::assert_arm(arm, gpu);
            let b = gpu
                .render_input(&mut aterm_gpu::WindowGpu::new(), &l, None)
                .pixels;
            assert!(
                a == b,
                "{arm} alpha {alpha}: the GPU draws the stream's bytes"
            );
        }
    }
}

/// The opacities of a ramp row's left and right ends, a row band each: a
/// faint end beside a near-opaque one either way round, and an additive end
/// beside an opaque one.
const END_OPACITIES: [(u8, u8); 5] = [(1, 236), (236, 1), (200, 1), (1, 200), (0, 255)];

/// The colours a slab's rows walk: row `r`'s left end is entry `r`, its
/// right end entry `r + 2`.
const END_INKS: [u32; 4] = [0x00FF_4020, 0x0020_E040, 0x0040_40FF, 0x00F0_C010];

/// Lay into `out`, in the row band of each pair of [`END_OPACITIES`] (bands
/// 2 on), a slab per `(rows, w)` of `slabs`, side by side from cell column 4
/// and two pixels into the band, each row's ends at its pair's opacities, or,
/// for the `flat` twin, the right end at the left end's.
fn opacity_slabs(
    out: &mut impl BeamRows,
    (pad, top): (usize, usize),
    (cw, ch): (usize, usize),
    slabs: &[(usize, usize)],
    flat: bool,
) {
    let px = |v: usize| u16::try_from(v).expect("a geometry within u16");
    // An additive end adds light of its own; a source-over end carries its
    // colour premultiplied by its opacity.
    let end = |ink: usize, alpha: u8| {
        let cov = if alpha == 0 { 0x60 } else { alpha };
        (
            aterm_render::premul_rgb(END_INKS[ink % END_INKS.len()], cov),
            alpha,
        )
    };
    for (k, &(a0, a1)) in END_OPACITIES.iter().enumerate() {
        let band = px(2 + k);
        let mut x = pad + 4 * cw;
        for &(rows, w) in slabs {
            out.slab();
            for r in 0..rows {
                let y = top + usize::from(band) * ch + 2 + r;
                let right = (end(r + 2, a1).0, if flat { a0 } else { a1 });
                out.row(
                    i32::from(band),
                    band,
                    (px(x), px(y), px(w)),
                    end(r, a0),
                    right,
                );
            }
            x += w + cw;
        }
    }
}

/// A ramp row whose two ends differ in opacity (PRISM-GLASS §15.1 P0.8, the
/// rows the light wedge lays): the CPU's `draw_ramp` and the GPU's
/// `fx_ramp_row` rows draw the bytes of the quads they stand for and of each
/// other, on every arm, in the under-glow and glow streams over a dark and a
/// light page, laid under [`RampCost::ALWAYS`] and under the measured cost
/// (its short slabs left quads), on every GPU route. The negative control is
/// the flat twin, each row's right end at its left end's opacity: it draws
/// other bytes, so a renderer that read one end's opacity for both fails.
#[test]
fn a_ramp_row_whose_ends_differ_in_opacity_draws_the_same_bytes_on_cpu_and_gpu() {
    const TEST: &str =
        "a_ramp_row_whose_ends_differ_in_opacity_draws_the_same_bytes_on_cpu_and_gpu";
    let exact = common::ByteExact::default();
    for theme in [Theme::default(), LIGHT] {
        let mut arms = common::arms(PX, theme);
        let (cw, ch) = arms[0].1.cell_size();
        let origin = (arms[0].1.pad(), arms[0].1.grid_top());
        let floor = aterm_render::glow_ramps::RAMP_COST.floor() as usize;
        let tall = ch - 4;
        assert!(
            tall >= floor,
            "a tall slab's run pays under the measured cost"
        );
        // A row, a run the measured cost leaves quads, and a run that rides,
        // each narrow and wide.
        let slabs = [(1, 1), (1, 7), (3, 2), (3, 15), (tall, 3), (tall, 40)];
        let mut legacy: Vec<GlowQuad> = Vec::new();
        opacity_slabs(&mut legacy, origin, (cw, ch), &slabs, false);
        let laid = |cost: Option<RampCost>, flat: bool| {
            let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
            let sink = RampSink::new(&mut quads, &mut ramps);
            let mut sink = match cost {
                Some(cost) => sink.costed(cost),
                None => sink,
            };
            opacity_slabs(&mut sink, origin, (cw, ch), &slabs, flat);
            drop(sink);
            (quads, ramps)
        };
        let always = laid(Some(RampCost::ALWAYS), false);
        let measured = laid(None, false);
        let flat = laid(Some(RampCost::ALWAYS), true);
        let bands = END_OPACITIES.len();
        assert_eq!(
            (always.0.len(), always.1.items.len()),
            (0, bands * slabs.len()),
            "under RampCost::ALWAYS every slab rides a ramp"
        );
        let short: usize = slabs.iter().filter(|s| s.0 < floor).map(|s| s.0).sum();
        let riding = slabs.iter().filter(|s| s.0 >= floor).count();
        assert_eq!(
            (measured.0.len(), measured.1.items.len()),
            (bands * short, bands * riding),
            "under the measured cost a slab short of its floor stays quads"
        );
        for (x, what) in [
            (&always, "every slab a ramp"),
            (&measured, "the measured cost"),
        ] {
            assert_eq!(
                x.1.laid(x.0.len()),
                legacy.len(),
                "{what}: every lit ramp row counts as its quad"
            );
        }
        let opacities = |(_, ramps): &(Vec<GlowQuad>, GlowRamps)| {
            ramps
                .items
                .iter()
                .flat_map(|ramp| (0..ramp.h).map(move |r| ramp.row_ends(&ramps.ends, r)))
                .map(|(a, b)| (aterm_render::end_alpha(a), aterm_render::end_alpha(b)))
                .collect::<Vec<_>>()
        };
        assert!(
            opacities(&always).iter().all(|(a, b)| a != b),
            "every ramp row's ends differ in opacity"
        );
        assert!(
            opacities(&flat).iter().all(|(a, b)| a == b),
            "every flat twin row's ends share one"
        );
        let laid = Laid {
            legacy,
            always,
            measured,
            twin: flat,
        };
        draws_like_its_quads(TEST, &exact, &mut arms, theme, &laid, "the flat twin");
    }
}

/// The light glass's tails (PRISM-GLASS §2.2): one per band 2, 5 and 8 from
/// cell column 4 at one, two and three slabs a cell, its spine at its band's
/// bottom and nothing below it, `up` 0.96 ch at the hand eased in from
/// nothing at the oldest end over 2.5 cells, its core half of it, a vertex
/// at each cell edge and its hue walking, laid source-over as the wedge
/// lays them (`ribbon_wedge_into`), or for the `slab` twin as the slab
/// emission samples each slab at its centre (`ribbon_beam_into`).
fn glass_tails(
    out: &mut impl BeamRows,
    (pad, top): (usize, usize),
    (cw, ch): (usize, usize),
    slab: bool,
) {
    let px = |v: usize| i32::try_from(v).expect("a geometry within i32");
    let clip = aterm_render::BeamClip {
        x0: 0,
        y0: 0,
        x1: px(2 * pad + COLS * cw),
        y1: px(top + ROWS * ch),
        cell_h: px(ch),
        origin_y: px(top),
    };
    for (band, slabs) in [(2, 1), (5, 2), (8, 3)] {
        let spine = (top + (band + 1) * ch) as f32;
        let verts: Vec<aterm_render::RibbonVertex> = (0..=8u8)
            .map(|k| {
                let up = 0.96 * ch as f32 * aterm_render::smoothstep01(f32::from(k) / 2.5);
                aterm_render::RibbonVertex {
                    x: (pad + (4 + usize::from(k)) * cw) as f32 + 0.25,
                    spine,
                    up,
                    dn: 0.0,
                    core_up: 0.5 * up,
                    core_dn: 0.0,
                    color: END_INKS[usize::from(k) % END_INKS.len()],
                    cov: 255.0,
                    lift: 0.0,
                    lift_span: 0.0,
                }
            })
            .collect();
        let step = cw.div_ceil(slabs);
        let whole = if slab {
            aterm_render::ribbon_beam_into(out, clip, &verts, 1.0, step, 100_000, GlowBlend::Over)
        } else {
            aterm_render::ribbon_wedge_into(
                out,
                clip,
                &verts,
                1.0,
                step,
                100_000,
                GlowBlend::Over,
                0.5,
            )
        };
        assert!(whole, "band {band}: the tail is laid whole");
    }
}

/// A frame of the light glass's wedge (PRISM-GLASS §15.2 1.6, the P0.8
/// frame of its rows): the tails' flat runs and ramps whose ends differ in
/// opacity, some at zero, draw the same bytes on the CPU and on every GPU
/// arm and route, in the under-glow and glow streams over a dark and a
/// light page, laid under [`RampCost::ALWAYS`] and under the measured cost.
/// The negative control is the slab twin, the same tails as the slab
/// emission lays them, which draws other bytes.
#[test]
fn a_wedge_frame_draws_the_same_bytes_on_cpu_and_gpu() {
    const TEST: &str = "a_wedge_frame_draws_the_same_bytes_on_cpu_and_gpu";
    let exact = common::ByteExact::default();
    for theme in [Theme::default(), LIGHT] {
        let mut arms = common::arms(PX, theme);
        let cell = arms[0].1.cell_size();
        let origin = (arms[0].1.pad(), arms[0].1.grid_top());
        let _ = aterm_render::test_counters::take_wedge_slabs();
        let mut legacy: Vec<GlowQuad> = Vec::new();
        glass_tails(&mut legacy, origin, cell, false);
        assert!(
            aterm_render::test_counters::take_wedge_slabs() > 0,
            "the tails lay wedges"
        );
        let sunk = |cost: Option<RampCost>, slab: bool| {
            let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
            let sink = RampSink::new(&mut quads, &mut ramps);
            let mut sink = match cost {
                Some(cost) => sink.costed(cost),
                None => sink,
            };
            glass_tails(&mut sink, origin, cell, slab);
            drop(sink);
            (quads, ramps)
        };
        let ends = |zero: bool| {
            legacy
                .iter()
                .filter(|q| q.alpha != q.alpha2 && (q.alpha.min(q.alpha2) == 0) == zero)
                .count()
        };
        assert!(
            ends(false) > 0 && ends(true) > 0,
            "the wedge lays ramp rows whose ends differ in opacity, some from zero"
        );
        assert!(
            legacy
                .iter()
                .all(|q| (q.alpha > 0 || q.color == 0) && (q.alpha2 > 0 || q.color2 == 0)),
            "a zero-opacity end carries no colour"
        );
        let laid = Laid {
            always: sunk(Some(RampCost::ALWAYS), false),
            measured: sunk(None, false),
            twin: sunk(Some(RampCost::ALWAYS), true),
            legacy,
        };
        assert_eq!(
            laid.always.1.laid(laid.always.0.len()),
            laid.legacy.len(),
            "every lit ramp row counts as its quad"
        );
        draws_like_its_quads(TEST, &exact, &mut arms, theme, &laid, "the slab twin");
    }
}

/// Default Light, the shipped light theme the glass draws on (aterm-types' builtin): its
/// glass129 ships, so the glass draws from the first lit frame.
const DEFAULT_LIGHT: Theme = Theme {
    fg: 0x002B_2D33,
    bg: 0x00F6_F7F9,
    cursor: 0x001A_7F37,
    selection: 0x00BC_C9E0,
};

/// A typed line of words in upper half blocks, which the CPU and the GPU draw byte for byte (an
/// antialiased glyph's edge may round a last bit apart), so a delta is the glass's. Procedural
/// words have no mono-raster authority and yield; blank regions still exercise exact glass
/// parity. The supported-raster word positive below checks retained text separately.
/// Its fourth word is typed in an app's red.
const GLASS_LINE: &str = "▀▀▀▀ ▀▀ ▀▀▀ ▀▀▀▀▀▀▀ ▀▀▀▀▀ ▀▀▀▀ ▀▀▀▀▀ ▀▀▀ ▀▀▀ ▀▀▀▀▀▀▀";
/// The red word's keys in [`GLASS_LINE`], from its first (Default Light's ANSI red, #c4262e).
const RED_WORD: (usize, usize) = (12, 19);
/// The prompt the line is typed after, in the same blocks.
const GLASS_PROMPT: &str = "▀▀▀ ";
/// The key after which the cells [`GLASS_SELECTED`] behind the caret are selected.
const GLASS_SELECT_AT: usize = 30;
/// The selected cells, as offsets behind the caret: under the band's newest stretch.
const GLASS_SELECTED: (u16, u16) = (9, 3);

struct GlassParityScene {
    name: &'static str,
    px: f32,
    line: &'static str,
    prompt: &'static str,
    red_word: (usize, usize),
    select_at: usize,
    selected: (u16, u16),
    supported: bool,
}

impl GlassParityScene {
    fn procedural() -> Self {
        Self {
            name: "the procedural corpus",
            px: PX,
            line: GLASS_LINE,
            prompt: GLASS_PROMPT,
            red_word: RED_WORD,
            select_at: GLASS_SELECT_AT,
            selected: GLASS_SELECTED,
            supported: false,
        }
    }

    fn supported() -> Self {
        let line = "MMMM MMMM MMMM ";
        let words: Vec<_> = line
            .split_inclusive(' ')
            .scan(0, |col, word| {
                let start = *col;
                *col += word.chars().count();
                Some((start, start + word.trim_end_matches(' ').chars().count()))
            })
            .collect();
        let select_at = words[2].0 + 1;
        let caret = select_at + 1;
        // Select the first word's last two letters while the third word's second letter
        // arrives: selected ordinary ink, the red middle word and unselected ordinary ink
        // all lie in the same live tail. No position belongs to the procedural corpus.
        let selected = (
            u16::try_from(caret - (words[0].1 - 2)).expect("a short selection"),
            u16::try_from(caret - (words[0].1 - 1)).expect("a short selection"),
        );
        Self {
            name: "the actual 24px word corridor",
            px: 24.0,
            line,
            prompt: "MMM ",
            red_word: words[1],
            select_at,
            selected,
            supported: true,
        }
    }
}

#[derive(Debug, Default)]
struct GlassMixCensus {
    glass: usize,
    emitted: usize,
    unyielded_wedge: usize,
    wedge: usize,
    yielded: usize,
    selected: usize,
    mixed: usize,
    selected_pixels_changed: usize,
    font_page_delta: i32,
}

/// The glass's procedural corpus and an actual-font mixed frame (PRISM-GLASS §15.2 1.12):
/// Rainbow Kitty on Default Light at [`Live::Glass`], yielded as the heterogeneous route
/// yields it ([`EffectsPipeline::yield_glass_unworded`]). The original block words keep their
/// byte comparisons and have no raster authority. A separate 24px M corridor supplies the
/// retained wedge beside a red word and selected ordinary ink. Its actual font page can round
/// a channel apart between raster backends (the measured 24px page differs by one before any
/// glass), so its parity judge compares ramps with flattened quads within each backend. A
/// supported frame must mix actual emission, final wedge rows, a yield cut and selected cells;
/// its unyielded twin must change both backends, including pixels inside the selection. The
/// original procedural corpus keeps every exact cross-backend comparison, its unyielded
/// frames included; the font page delta remains in the census, never a global tolerance.
#[test]
fn procedural_glass_is_byte_exact_and_supported_glass_keeps_a_mixed_frame() {
    let procedural = glass_frame_parity(GlassParityScene::procedural());
    let supported = glass_frame_parity(GlassParityScene::supported());
    let glass_frames = procedural.glass + supported.glass;
    let mixed = procedural.mixed + supported.mixed;
    assert!(glass_frames > 0, "premise: the glass drew");
    assert!(
        mixed > 0,
        "a frame mixes the glass, its wedge rows, the yield and a selection \
         ({glass_frames} glass frames; procedural {procedural:?}; supported {supported:?})"
    );
    assert!(
        supported.mixed > 0 && supported.selected_pixels_changed > 0,
        "the actual word corridor retains the mixed frame and a visible selection cut: \
         {supported:?}"
    );
}

/// Keep procedural cross-backend byte parity and actual supported ink's per-backend
/// ramp/quad parity, where strict yielding can leave a body wedge beside its cuts.
fn glass_frame_parity(scene: GlassParityScene) -> GlassMixCensus {
    const TEST: &str = "procedural_glass_is_byte_exact_and_supported_glass_keeps_a_mixed_frame";
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_effects::cursor_glow::{Light, Live};
    let exact = common::ByteExact::default();
    let theme = DEFAULT_LIGHT;
    let mut arms = common::arms(scene.px, theme);
    for (_, _, gpu) in &arms {
        common::byte_exact_or_could_not_run(gpu, TEST);
    }
    let (cw, ch) = arms[0].1.cell_size();
    for (_, cpu, gpu) in &mut arms {
        cpu.debug_block_on_lazy_fallbacks();
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        gpu.set_ramps_fx_for_test(None);
    }
    let (frame_w, frame_h) = arms[0].1.frame_size(ROWS, COLS);
    let px = |v: usize| u16::try_from(v).expect("a geometry within u16");
    let geom = Geom {
        cw,
        ch,
        rows: ROWS,
        cols: COLS,
        origin_x: px(arms[0].1.pad()),
        origin_y: px(arms[0].1.grid_top()),
        win_w: px(frame_w),
        win_h: px(frame_h),
        head: 0,
    };
    let light = if scene.supported {
        let page = aterm_effects::host::PageFacts {
            thin_ink: arms[0].1.glyph_ink().thin_share(
                aterm_render::linear_luminance(theme.fg),
                aterm_render::linear_luminance(theme.bg),
            ),
            ..aterm_effects::host::PageFacts::OPAQUE
        };
        assert!(page.glass(), "the actual 24px fixture face admits glass");
        Light::at(Live::Glass).on_page(page.glass())
    } else {
        Light::at(Live::Glass)
    };
    let cfg = GlowConfig {
        light,
        ..config(theme, KITTY)
    };
    assert!(
        cfg.light.glass() && !cfg.dark_theme,
        "premise: a lit light page"
    );
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07");
    for _ in 0..ROWS - 3 {
        term.process(b"\r\n");
    }
    term.process(scene.prompt.as_bytes());
    let prompt_row = ROWS - 3;
    // The app's measured ramp cost, and every row a quad (where the wedge's rows show).
    let (mut app, mut rows) = (
        Side::new(None, true),
        Side::new(Some(RampCost::NEVER), true),
    );
    let mut base = RenderInput::empty();
    let t0 = Instant::now();
    let period = Duration::from_secs(1) / HZ;
    let line: Vec<char> = scene.line.chars().collect();
    let keys = line.len();
    let mut census = GlassMixCensus::default();
    let mut wins: Vec<_> = arms
        .iter()
        .map(|_| {
            (
                [WindowCpu::new(), WindowCpu::new()],
                [aterm_gpu::WindowGpu::new(), aterm_gpu::WindowGpu::new()],
            )
        })
        .collect();
    for tick in 0..keys * KEY_TICKS + RUNOUT {
        let now = t0 + period * tick as u32;
        if tick % KEY_TICKS == 0 && tick / KEY_TICKS < keys {
            let k = tick / KEY_TICKS;
            let mut key = Vec::new();
            if k == scene.red_word.0 {
                key.extend_from_slice(b"\x1b[38;2;196;38;46m");
            }
            if k == scene.red_word.1 {
                key.extend_from_slice(b"\x1b[39m");
            }
            key.extend_from_slice(line[k].encode_utf8(&mut [0; 4]).as_bytes());
            term.process(&key);
            if k == scene.select_at {
                let caret = term.cell_frame(ROWS, COLS).cursor_col as u16;
                let row = i32::try_from(prompt_row).expect("a row within i32");
                let selection = term.text_selection_mut();
                selection.start_selection(
                    row,
                    caret - scene.selected.0,
                    SelectionSide::Left,
                    SelectionType::Simple,
                );
                selection.update_selection(row, caret - scene.selected.1, SelectionSide::Right);
                selection.complete_selection();
            }
            for side in [&mut app, &mut rows] {
                side.fx.cursor_mut().glow.note_synthetic_typed(now, 1);
            }
        }
        let _ = term.cell_frame_damage_scoped_into(&mut base, ROWS, COLS);
        base.selection_bg = theme.selection;
        let _ = aterm_render::test_counters::take_glass_slabs();
        let ink = arms[0].1.glyph_ink_for(&base);
        let observed = ink
            .for_frame(&base, (cw, ch))
            .expect("the scene observes this exact frame");
        for (row, cells) in base.cells.iter().enumerate() {
            for (col, cell) in cells.iter().enumerate() {
                if cell.ch == '▀' {
                    assert!(
                        observed.coverage(row, col).is_none(),
                        "procedural blocks never gain mono-raster authority"
                    );
                } else if scene.supported && cell.ch == 'M' {
                    assert!(
                        observed
                            .coverage(row, col)
                            .is_some_and(|(_, ink)| ink.max > 0),
                        "the word corridor uses the actual supported M raster"
                    );
                }
            }
        }
        app.fx
            .cursor_mut()
            .glow
            .observe_glyph_ink(Some(Arc::clone(&ink)));
        rows.fx.cursor_mut().glow.observe_glyph_ink(Some(ink));
        app.frame(&term, &base, &cfg, geom, now);
        let glass_slabs = aterm_render::test_counters::take_glass_slabs();
        rows.frame(&term, &base, &cfg, geom, now);
        let mut unyielded = Vec::new();
        for side in [&mut app, &mut rows] {
            unyielded.push(side.input.clone());
            side.fx.yield_glass_unworded(&mut side.input);
        }
        let arm_names: Vec<&str> = app.fx.light_arms().iter().map(|(arm, _)| arm).collect();
        if !arm_names.contains(&"glass") {
            continue;
        }
        census.glass += 1;
        let wedge = rows.input.glow_under.iter().any(|q| q.alpha != q.alpha2);
        let selected =
            (0..COLS).any(|col| base.selection_hit(prompt_row, col, false, false).is_some());
        let yielded = arm_names.contains(&"yield");
        let mix = glass_slabs > 0 && wedge && yielded && selected;
        census.emitted += usize::from(glass_slabs > 0);
        census.unyielded_wedge +=
            usize::from(unyielded[1].glow_under.iter().any(|q| q.alpha != q.alpha2));
        census.wedge += usize::from(wedge);
        census.yielded += usize::from(yielded);
        census.selected += usize::from(selected);
        census.mixed += usize::from(mix);
        // The procedural page, selection and caret are byte-exact. Record the actual-font
        // page's separate AA delta without treating it as a glass-stream tolerance.
        let mut page = app.input.clone();
        page.glow_under.clear();
        page.cursor_glow_add.clear();
        page.glow_halo.clear();
        page.glow_under_ramps = Default::default();
        page.cursor_glow_add_ramps = Default::default();
        for ((arm, cpu, gpu), (cwins, gwins)) in arms.iter_mut().zip(&mut wins) {
            let what = format!("{}: {arm} frame {tick}, its page", scene.name);
            if exact.holds(gpu, &what) {
                let cpu_page = cpu.render_input(&page).pixels;
                let gpu_page = gpu
                    .render_input(&mut aterm_gpu::WindowGpu::new(), &page, None)
                    .pixels;
                let delta = common::max_channel_delta(&cpu_page, &gpu_page);
                if scene.supported {
                    census.font_page_delta = census.font_page_delta.max(delta);
                } else {
                    assert_eq!(
                        delta, 0,
                        "{what}: the page and its blocks draw byte-exact, so a delta is the glass's"
                    );
                }
            }
            let sides = [
                ("the measured cost", &app.input, &unyielded[0]),
                ("every row a quad", &rows.input, &unyielded[1]),
            ];
            for (i, (side, input, twin)) in sides.into_iter().enumerate() {
                let what = format!("{arm} frame {tick} ({side}, arms {arm_names:?})");
                let cpu_frame = cpu
                    .render_input_cached(&mut cwins[i], input)
                    .pixels()
                    .to_vec();
                let gpu_frame = gpu.render_input(&mut gwins[i], input, None).pixels;
                common::assert_arm(arm, gpu);
                let delta = common::max_channel_delta(&cpu_frame, &gpu_frame);
                if !scene.supported && exact.holds(gpu, &what) {
                    common::assert_byte_exact(
                        TEST,
                        &what,
                        gpu,
                        delta,
                        format_args!(
                            "{what}: the glass frame must be BYTE-EXACT CPU==GPU (got {delta})"
                        ),
                    );
                }
                if scene.supported {
                    let mut flat = input.clone();
                    flat.glow_under_ramps.flatten_into(&mut flat.glow_under);
                    let cpu_flat = cpu.render_input(&flat).pixels;
                    let gpu_flat = gpu
                        .render_input(&mut aterm_gpu::WindowGpu::new(), &flat, None)
                        .pixels;
                    common::assert_arm(arm, gpu);
                    assert_eq!(
                        cpu_frame, cpu_flat,
                        "{what}: supported-word ramps equal their quads on CPU"
                    );
                    assert_eq!(
                        gpu_frame, gpu_flat,
                        "{what}: supported-word ramps equal their quads on GPU"
                    );
                }
                // The protected corpus legitimately loses its final body wedges. Keep its
                // unyielded byte comparisons and cut control live on every emitted/cut frame.
                let cut_control = mix || !scene.supported && glass_slabs > 0 && yielded;
                if !cut_control {
                    continue;
                }
                let cpu_twin = cpu.render_input(twin).pixels;
                let gpu_twin = gpu
                    .render_input(&mut aterm_gpu::WindowGpu::new(), twin, None)
                    .pixels;
                assert!(
                    cpu_twin != cpu_frame && gpu_twin != gpu_frame,
                    "{what}, control: the frame unyielded draws other bytes on both backends"
                );
                if scene.supported {
                    for pixels in [&cpu_frame, &gpu_frame, &cpu_twin, &gpu_twin] {
                        assert_eq!(
                            pixels.len(),
                            frame_w * frame_h,
                            "{what}: the selected-cell comparison has the frame's geometry"
                        );
                    }
                    let changed = |lit: &[u32], twin: &[u32]| {
                        (0..COLS).any(|col| {
                            if base.selection_hit(prompt_row, col, false, false).is_none() {
                                return false;
                            }
                            let x0 = usize::from(geom.origin_x) + col * cw;
                            let y0 = usize::from(geom.origin_y) + prompt_row * ch;
                            (y0..(y0 + ch).min(frame_h)).any(|y| {
                                (x0..(x0 + cw).min(frame_w)).any(|x| {
                                    let at = y * frame_w + x;
                                    lit[at] & 0x00FF_FFFF != twin[at] & 0x00FF_FFFF
                                })
                            })
                        })
                    };
                    census.selected_pixels_changed += usize::from(
                        changed(&cpu_frame, &cpu_twin) && changed(&gpu_frame, &gpu_twin),
                    );
                }
                let delta_twin = common::max_channel_delta(&cpu_twin, &gpu_twin);
                if !scene.supported && exact.holds(gpu, &what) {
                    common::assert_byte_exact(
                        TEST,
                        &format!("{what}, unyielded"),
                        gpu,
                        delta_twin,
                        format_args!(
                            "{what}: the unyielded twin must be BYTE-EXACT CPU==GPU (got \
                             {delta_twin})"
                        ),
                    );
                }
            }
        }
    }
    assert!(census.glass > 0, "{}: premise: the glass drew", scene.name);
    assert!(census.emitted > 0, "{}: actual glass emitted", scene.name);
    eprintln!("{TEST}: {}: {census:?}", scene.name);
    census
}

/// Unlike the procedural parity corpus, these ordinary words have actual font rasters.
/// The final yielded stream must retain glass beneath one, draw there on both backends,
/// and keep ramp/quad parity within each backend. The unchanged same-frame letter judges
/// live in render_suite's supported-word control and Settings' 2x card control.
#[test]
fn a_supported_word_keeps_final_glass_on_both_raster_backends() {
    use aterm_effects::cursor_glow::{Light, Live};
    let theme = DEFAULT_LIGHT;
    let mut arms = common::arms(24.0, theme);
    for (_, _, gpu) in &arms {
        common::byte_exact_or_could_not_run(gpu, "supported-word glass census");
    }
    let (cw, ch) = arms[0].1.cell_size();
    let (frame_w, frame_h) = arms[0].1.frame_size(ROWS, COLS);
    let geom = Geom {
        cw,
        ch,
        rows: ROWS,
        cols: COLS,
        origin_x: arms[0].1.pad() as u16,
        origin_y: arms[0].1.grid_top() as u16,
        win_w: frame_w as u16,
        win_h: frame_h as u16,
        head: 0,
    };
    for (_, cpu, gpu) in &mut arms {
        cpu.debug_block_on_lazy_fallbacks();
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
    }
    let page = aterm_effects::host::PageFacts {
        thin_ink: arms[0].1.glyph_ink().thin_share(
            aterm_render::linear_luminance(theme.fg),
            aterm_render::linear_luminance(theme.bg),
        ),
        ..aterm_effects::host::PageFacts::OPAQUE
    };
    assert!(page.glass(), "the actual 24px fixture face admits glass");
    let cfg = GlowConfig {
        light: Light::at(Live::Glass).on_page(page.glass()),
        ..config(theme, KITTY)
    };
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07\x1b[10;5H");
    let mut side = Side::new(None, true);
    let text = b"MMMM MMMM MMMM ";
    let t0 = Instant::now();
    for tick in 0..text.len() * KEY_TICKS + RUNOUT {
        let now = t0 + (Duration::from_secs(1) / HZ) * tick as u32;
        if tick % KEY_TICKS == 0 && tick / KEY_TICKS < text.len() {
            side.fx.cursor_mut().glow.note_synthetic_typed(now, 1);
            term.process(&text[tick / KEY_TICKS..tick / KEY_TICKS + 1]);
        }
        let base = term.cell_frame(ROWS, COLS);
        let observed = arms[0].1.glyph_ink_for(&base);
        side.fx
            .cursor_mut()
            .glow
            .observe_glyph_ink(Some(Arc::clone(&observed)));
        side.frame(&term, &base, &cfg, geom, now);
        let no_glass = aterm_effects::cursor_glow::glass_yield::without_body_glass_for_test(
            &side.input,
            side.fx.cursor().glow.glass_laid(),
        );
        side.fx.yield_glass_unworded(&mut side.input);
        if !side.fx.light_arms().iter().any(|(arm, _)| arm == "glass") {
            continue;
        }
        let frame = observed.for_frame(&base, (cw, ch)).unwrap();
        let mut final_quads = Vec::new();
        side.input
            .glow_under_ramps
            .expand_into(&side.input.glow_under, &mut final_quads);
        let retained: Vec<_> = base
            .cells
            .iter()
            .enumerate()
            .flat_map(|(row, cells)| {
                cells
                    .iter()
                    .enumerate()
                    .filter_map(|(col, cell)| {
                        let ink = frame
                            .coverage(row, col)
                            .is_some_and(|(_, coverage)| coverage.max > 0);
                        let (x0, y0) = (
                            usize::from(geom.origin_x) + col * cw,
                            usize::from(geom.origin_y) + row * ch,
                        );
                        (cell.ch == 'M'
                            && ink
                            && final_quads.iter().any(|q| {
                                let lo = x0.max(usize::from(q.x));
                                let hi = (x0 + cw).min(usize::from(q.x) + usize::from(q.w));
                                usize::from(q.y) < y0 + ch
                                    && y0 < usize::from(q.y) + usize::from(q.h)
                                    && (lo..hi).any(|x| {
                                        aterm_render::glow_lerp(
                                            u32::from(q.alpha),
                                            u32::from(q.alpha2),
                                            (x - usize::from(q.x)) as u32,
                                            u32::from(q.w),
                                        ) > 0
                                    })
                            }))
                        .then_some((x0, y0))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        if retained.is_empty() {
            continue;
        }
        let mut flat = side.input.clone();
        flat.glow_under_ramps.flatten_into(&mut flat.glow_under);
        for (arm, cpu, gpu) in &mut arms {
            let cpu_lit = cpu.render_input(&side.input);
            let cpu_flat = cpu.render_input(&flat);
            let cpu_bare = cpu.render_input(&no_glass);
            assert_eq!(
                (cpu_lit.width, cpu_lit.height),
                (cpu_flat.width, cpu_flat.height),
                "{arm}: CPU ramps and quads have the same geometry"
            );
            assert_eq!(
                (cpu_lit.width, cpu_lit.height),
                (cpu_bare.width, cpu_bare.height),
                "{arm}: CPU body-only twin has the actual frame's geometry"
            );
            assert_eq!(
                cpu_lit.pixels, cpu_flat.pixels,
                "{arm}: final supported-word ramps equal their quads on CPU"
            );
            let gpu_lit = gpu.render_input(&mut aterm_gpu::WindowGpu::new(), &side.input, None);
            common::assert_arm(arm, gpu);
            let gpu_flat = gpu.render_input(&mut aterm_gpu::WindowGpu::new(), &flat, None);
            let gpu_bare = gpu.render_input(&mut aterm_gpu::WindowGpu::new(), &no_glass, None);
            assert_eq!(
                (gpu_lit.width, gpu_lit.height),
                (gpu_flat.width, gpu_flat.height),
                "{arm}: GPU ramps and quads have the same geometry"
            );
            assert_eq!(
                (gpu_lit.width, gpu_lit.height),
                (gpu_bare.width, gpu_bare.height),
                "{arm}: GPU body-only twin has the actual frame's geometry"
            );
            assert_eq!(
                gpu_lit.pixels, gpu_flat.pixels,
                "{arm}: final supported-word ramps equal their quads on GPU"
            );
            for (lit, bare) in [(&cpu_lit, &cpu_bare), (&gpu_lit, &gpu_bare)] {
                assert!(
                    retained.iter().any(|&(x0, y0)| {
                        final_quads.iter().any(|q| {
                            if usize::from(q.row) >= side.input.rows {
                                return false;
                            }
                            let left = x0.max(usize::from(q.x));
                            let right = (x0 + cw)
                                .min(lit.width)
                                .min(usize::from(q.x) + usize::from(q.w));
                            let top = y0.max(usize::from(q.y));
                            let bottom = (y0 + ch)
                                .min(lit.height)
                                .min(usize::from(q.y) + usize::from(q.h));
                            (left..right).any(|x| {
                                aterm_render::glow_lerp(
                                    u32::from(q.alpha),
                                    u32::from(q.alpha2),
                                    (x - usize::from(q.x)) as u32,
                                    u32::from(q.w),
                                ) > 0
                                    && (top..bottom).any(|y| {
                                        let at = y * lit.width + x;
                                        lit.pixels[at] & 0x00FF_FFFF
                                            != bare.pixels[at] & 0x00FF_FFFF
                                    })
                            })
                        })
                    }),
                    "{arm}: final glass really changes a supported word's cell, not only blank regions"
                );
            }
        }
        return;
    }
    panic!("no frame retained final glass beneath an observed supported word");
}

/// A frame's glow rows laid four ways: the legacy stream (every row a
/// quad), every run a ramp ([`RampCost::ALWAYS`]), the measured cost, and a
/// twin that must draw other bytes (the negative control).
struct Laid {
    legacy: Vec<GlowQuad>,
    always: (Vec<GlowQuad>, GlowRamps),
    measured: (Vec<GlowQuad>, GlowRamps),
    twin: (Vec<GlowQuad>, GlowRamps),
}

/// `laid` drawn in the under-glow and glow streams over `theme`'s page, on
/// every arm: the CPU draws its ramps as the legacy stream's quads; every
/// GPU route draws them so too, with bloom off and on; `twin` (its twin)
/// draws other bytes on both; and with bloom off the GPU frame is
/// byte-exact with the CPU's.
fn draws_like_its_quads(
    test: &str,
    exact: &common::ByteExact,
    arms: &mut [(&'static str, aterm_render::Renderer, aterm_gpu::GpuRenderer)],
    theme: Theme,
    laid: &Laid,
    twin: &str,
) {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l");
    let base = term.cell_frame(ROWS, COLS);
    for (arm, cpu, gpu) in arms.iter_mut() {
        let page = cpu.render_input(&base).pixels;
        // The CPU draws no bloom and no heat shimmer: the frames compare
        // with both off, and the GPU's routes with its bloom on too.
        gpu.set_shimmer(false);
        gpu.set_bloom(false);
        gpu.set_ramps_fx_for_test(None);
        let gpu_page = gpu
            .render_input(&mut aterm_gpu::WindowGpu::new(), &base, None)
            .pixels;
        assert_eq!(
            common::max_channel_delta(&page, &gpu_page),
            0,
            "{arm} on bg {:06x}: the page draws byte-exact, so a delta is the ramps'",
            theme.bg
        );
        for (stream, under) in [("under-glow", true), ("glow", false)] {
            let what = format!("{arm} {stream} stream on bg {:06x}", theme.bg);
            let carrying = |(quads, ramps): (&[GlowQuad], &GlowRamps)| {
                let mut input = base.clone();
                let (q, r) = if under {
                    (&mut input.glow_under, &mut input.glow_under_ramps)
                } else {
                    (&mut input.cursor_glow_add, &mut input.cursor_glow_add_ramps)
                };
                (*q, *r) = (quads.to_vec(), ramps.clone());
                input
            };
            let l = carrying((&laid.legacy[..], &GlowRamps::default()));
            let r = carrying((&laid.always.0[..], &laid.always.1));
            let m = carrying((&laid.measured.0[..], &laid.measured.1));
            let f = carrying((&laid.twin.0[..], &laid.twin.1));
            let cpu_l = cpu.render_input(&l).pixels;
            assert!(cpu_l != page, "{what}: the slabs paint");
            assert!(
                cpu.render_input(&r).pixels == cpu_l,
                "{what}: the CPU draws the ramps as their quads"
            );
            assert!(
                cpu.render_input(&m).pixels == cpu_l,
                "{what}: the CPU draws the measured cost's ramps as their quads"
            );
            let cpu_f = cpu.render_input(&f).pixels;
            assert!(
                cpu_f != cpu_l,
                "{what}: {twin} draws other bytes on the CPU, or the pin proves nothing"
            );
            for (bloom, (route, how)) in [false, true].into_iter().flat_map(|bloom| {
                [
                    (Some(RampsFx::Always), "in the fx stream"),
                    (Some(RampsFx::Never), "as glow instances"),
                    (None, "by the measured rule"),
                ]
                .map(|route| (bloom, route))
            }) {
                let what = format!("{what}, bloom {bloom}");
                gpu.set_bloom(bloom);
                gpu.set_ramps_fx_for_test(route);
                let mut draw = |input: &RenderInput| {
                    let pixels = gpu
                        .render_input(&mut aterm_gpu::WindowGpu::new(), input, None)
                        .pixels;
                    common::assert_arm(arm, gpu);
                    (pixels, gpu.frame_shape().fx_instances)
                };
                let (gpu_l, fx_l) = draw(&l);
                let (gpu_r, fx_r) = draw(&r);
                let (gpu_m, _) = draw(&m);
                let (gpu_f, _) = draw(&f);
                assert_eq!(fx_l, 0, "{what}: the legacy stream opens no fx stream");
                if route == Some(RampsFx::Always) {
                    assert!(
                        fx_r > 0,
                        "{what}: the ramps drew in the fx stream, through fx_ramp_row"
                    );
                }
                assert!(
                    gpu_r == gpu_l,
                    "{what}: the GPU draws the ramps {how} as their quads"
                );
                assert!(
                    gpu_m == gpu_l,
                    "{what}: the GPU draws the measured cost's ramps {how} as their quads"
                );
                assert!(
                    gpu_f != gpu_r,
                    "{what}: {twin} draws other bytes on the GPU ({how})"
                );
                if bloom {
                    // The bloom is the GPU's alone: its routes agree above.
                    continue;
                }
                let delta = common::max_channel_delta(&cpu_l, &gpu_r);
                let delta_twin = common::max_channel_delta(&cpu_f, &gpu_f);
                eprintln!(
                    "{what}, ramps {how}: CPU vs GPU max per-channel delta {delta}, {twin} \
                     {delta_twin} ({} quads, {} ramps; {fx_r} fx instances)",
                    laid.legacy.len(),
                    laid.always.1.items.len()
                );
                if exact.holds(gpu, &what) {
                    assert_byte_exact_both(test, &what, gpu, (delta, delta_twin), (how, twin));
                }
            }
            gpu.set_bloom(false);
            gpu.set_ramps_fx_for_test(None);
        }
    }
}

/// [`common::assert_byte_exact`] of a ramp frame and its twin `twin`, drawn
/// `how`.
fn assert_byte_exact_both(
    test: &str,
    what: &str,
    gpu: &aterm_gpu::GpuRenderer,
    (delta, delta_twin): (i32, i32),
    (how, twin): (&str, &str),
) {
    common::assert_byte_exact(
        test,
        what,
        gpu,
        delta,
        format_args!(
            "{what}: the ramp rows, drawn {how}, must be BYTE-EXACT CPU==GPU (got {delta})"
        ),
    );
    common::assert_byte_exact(
        test,
        &format!("{what}, {twin}"),
        gpu,
        delta_twin,
        format_args!("{what}: {twin}, drawn {how}, must be BYTE-EXACT CPU==GPU (got {delta_twin})"),
    );
}

/// The typist's ribbon typed (its ramps under the ink, its over light in order) and the comet
/// leapt (its slabs' additive ramps, read by the bloom and its crowns on the dark page) beside
/// the frame's own fx art: ramps drawn in the fx stream the art opens draw the legacy stream's
/// bytes on every arm, on both pages; and on Metal the measured rule draws them as the glow
/// instances they stand for. Those two reach both glow streams and both draw routes; the route
/// reads whether the frame has fx instances and whether its ramps commute, not the style or the
/// script. A meteor's trains and a comet's typed slabs are not drawn beside art: a fault that
/// needs the dark trains' order and art in one frame goes unseen.
#[test]
fn ramps_beside_fx_art_draw_their_quads_on_every_arm() {
    let beside_art = |script, what: &str, style| Case {
        page: Page::Listing,
        script,
        what: format!("{what} beside art"),
        style,
        with_art: true,
    };
    let cases = [
        beside_art(Script::Type, "typing", KITTY),
        beside_art(Script::Hop, "comet leapt", (GlowStyle::Comet, false)),
    ];
    for (case, seen) in cases.iter().zip(holds_all(&cases)) {
        let what = &case.what;
        assert!(
            seen.beside_art > 0,
            "{what}: ramps rode the fx stream beside the art"
        );
        assert!(
            !cfg!(target_os = "macos") || seen.art_shut > 0,
            "{what}: on Metal the measured rule drew ramps beside art as their quads"
        );
        assert_eq!(
            seen.art_opened, 0,
            "{what}: no arm's measured costs send ramps beside art into fx"
        );
    }
}
