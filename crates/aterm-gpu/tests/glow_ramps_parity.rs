// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lane B5's law: a glow stream's ramps draw byte-identical to the quads they
//! stand for. The Rainbow Kitty ribbon and meteor trains, laid as ramps and
//! read so by the letter guard, draw on the CPU and on the GPU arm exactly
//! the frames their legacy stream
//! (every row a quad, `CursorGlow::set_glow_ramps(false)`) draws, damage-cached
//! frame after frame, with and without bloom, on a dark and a light page;
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
//! bytes on the CPU and on every GPU arm and route, laid under any cost.

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
use aterm_render::{BeamRows, GlowQuad, GlowRamps, RampCost, RampSink, Theme, WindowCpu};
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
            &CursorStep {
                input: CursorFxInput {
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
                glow: cfg,
                trail: TrailConfig {
                    enabled: false,
                    duration: cfg.duration,
                    max_len: cfg.length,
                    color: cfg.color,
                    intensity: cfg.intensity,
                    warmth: 0.0,
                },
                animate: true,
                row_probe: Some(RowProbe {
                    row,
                    caret: col,
                    trust: ProbeTrust::Full,
                    cols: &self.row,
                    flanks: Some((
                        (row > 0).then_some(self.above.as_slice()),
                        below.then_some(self.below.as_slice()),
                    )),
                }),
                frame_cells: Some(base.cells.as_slice()),
                frame_cells_at: (0, 0),
                pending_keys: 0,
                print_anchor,
                print_anchor_glyph,
                caret_drawn: true,
            },
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
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: aterm_render::theme_is_dark(theme.bg),
        theme_fg: theme.fg,
        theme_bg: theme.bg,
        style,
        color: 0x00d0_d0d0,
        accent: 0x0048_c9ff,
        duration: Duration::from_millis(650),
        length: usize::MAX,
        intensity: 1.0,
        audible: false,
        radius: 0.4,
        ring: true,
        beam: aterm_effects::cursor_glow::style_has_beam_of(style, ""),
        head_dx: 0.5,
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
    /// The frames whose bloom read the additive stream's ramps (its item
    /// list), and the presents whose crown did.
    bloomed: usize,
    crowned: usize,
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
        self.crowned += o.crowned;
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

/// Run `script` in `style` (and its classic face) on `theme` with bloom
/// `bloom` over `page`, every frame drawn by each arm's CPU and GPU from the
/// ramps and from the legacy stream, and carrying the fx art of [`art`]
/// beside its glow where `with_art`.
fn scene(
    theme: Theme,
    bloom: bool,
    (script, page): (Script, Page),
    style: (GlowStyle, bool),
    with_art: bool,
) -> Seen {
    let mut arms = common::arms(PX, theme);
    let (cw, ch) = arms[0].1.cell_size();
    for (_, cpu, gpu) in &mut arms {
        cpu.debug_block_on_lazy_fallbacks();
        gpu.set_bloom(bloom);
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
                // The presents of the ramps and of the legacy stream.
                [aterm_gpu::WindowGpu::new(), aterm_gpu::WindowGpu::new()],
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
        seen.bloomed += usize::from(bloom && items);
        for ((arm, cpu, gpu), (cwins, gwins, pwins)) in arms.iter_mut().zip(&mut wins) {
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
            if items {
                gpu.set_ramps_fx_for_test(None);
                let [pr, pl] = pwins;
                let mut present = |win: &mut aterm_gpu::WindowGpu, input: &RenderInput| {
                    gpu.reset_glow_ease_for_test(win);
                    assert!(
                        gpu.present_virtual(win, input, false, None, None),
                        "{arm} frame {tick}: the virtual present cannot drop"
                    );
                    gpu.virtual_presented_snapshot_current(
                        win,
                        win.resident_input_epoch(),
                        tick as u64 + 1,
                    )
                    .and_then(aterm_gpu::video_tap::RawCapture::convert)
                    .expect("the presented frame")
                    .rgba
                };
                let (a, b) = (present(pr, r), present(pl, l));
                assert!(
                    a == b,
                    "{arm} frame {tick}: the present's bloom and crown draw the ramps as their \
                     quads (first diff byte {:?})",
                    a.iter().zip(&b).position(|(x, y)| x != y)
                );
                seen.crowned += 1;
            }
        }
    }
    seen
}

/// Run `script` in `style` on both pages with and without bloom, returning
/// what the scenes showed ([`Seen`], summed).
fn holds(script: Script, what: &str, style: (GlowStyle, bool)) -> Seen {
    holds_over(Page::Listing, script, what, style)
}

/// [`holds`] over `page`.
fn holds_over(page: Page, script: Script, what: &str, style: (GlowStyle, bool)) -> Seen {
    holds_beside(page, script, what, style, false)
}

/// [`holds_over`], every frame carrying fx art beside its glow where
/// `with_art` ([`art`]).
fn holds_beside(
    page: Page,
    script: Script,
    what: &str,
    style: (GlowStyle, bool),
    with_art: bool,
) -> Seen {
    let mut all = Seen::default();
    for theme in [Theme::default(), LIGHT] {
        for bloom in [false, true] {
            let seen = scene(theme, bloom, (script, page), style, with_art);
            eprintln!(
                "{what} (bg {:06x}, bloom {bloom}): {} frames, up to {} ramps ({} runs, {} under \
                 the measured cost) a frame, {} guarded, {} streams drawn apart, {} in order, \
                 {} shimmered, measured ramps {} times in fx, {} times as quads, {} bloomed \
                 and {} crowned from the ramps, {} drawn in fx beside art, {} \
                 measured beside art as quads, {} in fx",
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
                seen.beside_art,
                seen.art_shut,
                seen.art_opened
            );
            all += seen;
        }
    }
    assert!(all.ramps > 0, "{what}: the scenes laid ramps");
    all
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
        "typing: the bloom read the ribbon's ramps"
    );
    assert!(seen.crowned > 0, "typing: the present's crown read them");
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

/// The streaks' beds under the letters (Comet's and Laser's on the dark page,
/// their ink bodies on the light one) and the classic face's mono bed, typed
/// and leapt.
#[test]
fn streak_bed_ramps_draw_their_quads_on_every_arm() {
    for (style, what) in [
        ((GlowStyle::Comet, false), "comet"),
        ((GlowStyle::Laser, false), "laser"),
        ((GlowStyle::Classic, true), "classic mono"),
    ] {
        for (script, how) in [(Script::Type, "typed"), (Script::Hop, "leapt")] {
            holds(script, &format!("{what} {how}"), style);
        }
    }
}

/// The comet slabs' columns, a run each core: the comet, the laser's beam and
/// bolts, the classic comet and the fire streak with its meteors, typed and
/// leapt, the guard cutting the comet's and the laser's runs.
#[test]
fn comet_slab_runs_draw_their_quads_on_every_arm() {
    for (style, what, guarded) in [
        ((GlowStyle::Comet, false), "comet", true),
        ((GlowStyle::Laser, false), "laser", true),
        ((GlowStyle::Classic, false), "classic", false),
        ((GlowStyle::Fire, false), "fire", false),
    ] {
        let mut seen = Seen::default();
        for (script, how) in [(Script::Type, "typed"), (Script::Hop, "leapt")] {
            seen += holds(script, &format!("{what} {how}"), style);
        }
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
    let mut seen = Seen::default();
    for (script, how) in [(Script::Type, "typed"), (Script::Hop, "leapt")] {
        seen += holds_over(
            Page::Open,
            script,
            &format!("fire {how}, open page"),
            (GlowStyle::Fire, false),
        );
    }
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
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"\x1b[?25l");
        let base = term.cell_frame(ROWS, COLS);
        for (arm, cpu, gpu) in &mut arms {
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
                let l = carrying((&legacy[..], &GlowRamps::default()));
                let r = carrying((&always.0[..], &always.1));
                let m = carrying((&measured.0[..], &measured.1));
                let f = carrying((&flat.0[..], &flat.1));
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
                    "{what}: the flat twin draws other bytes on the CPU, or no end's own \
                     opacity was drawn"
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
                        "{what}: the flat twin draws other bytes on the GPU ({how})"
                    );
                    if bloom {
                        // The bloom is the GPU's alone: its routes agree above.
                        continue;
                    }
                    let delta = common::max_channel_delta(&cpu_l, &gpu_r);
                    let delta_flat = common::max_channel_delta(&cpu_f, &gpu_f);
                    eprintln!(
                        "{what}, ramps {how}: CPU vs GPU max per-channel delta {delta}, flat \
                         twin {delta_flat} ({} quads, {} ramps; {fx_r} fx instances)",
                        legacy.len(),
                        always.1.items.len()
                    );
                    if exact.holds(gpu, &what) {
                        assert_byte_exact_both(TEST, &what, gpu, (delta, delta_flat), how);
                    }
                }
                gpu.set_bloom(false);
                gpu.set_ramps_fx_for_test(None);
            }
        }
    }
}

/// [`common::assert_byte_exact`] of a ramp frame and its flat twin drawn
/// `how`.
fn assert_byte_exact_both(
    test: &str,
    what: &str,
    gpu: &aterm_gpu::GpuRenderer,
    (delta, delta_flat): (i32, i32),
    how: &str,
) {
    common::assert_byte_exact(
        test,
        what,
        gpu,
        delta,
        format_args!(
            "{what}: ramp rows whose ends differ in opacity, drawn {how}, must be BYTE-EXACT \
             CPU==GPU (got {delta})"
        ),
    );
    common::assert_byte_exact(
        test,
        &format!("{what}, flat twin"),
        gpu,
        delta_flat,
        format_args!(
            "{what}: the flat twin, drawn {how}, must be BYTE-EXACT CPU==GPU (got {delta_flat})"
        ),
    );
}

/// The typist's ribbon (its ramps under the ink) and the comet (its slabs'
/// additive ramps, read by the bloom and its crowns on the dark page) beside
/// the frame's own fx art: ramps drawn in the fx stream the art opens draw
/// the legacy stream's bytes on every arm, typed and leapt, with and without
/// bloom, on both pages; and on Metal the measured rule draws them as the
/// glow instances they stand for.
#[test]
fn ramps_beside_fx_art_draw_their_quads_on_every_arm() {
    for (style, what) in [(KITTY, "typing"), ((GlowStyle::Comet, false), "comet")] {
        let mut seen = Seen::default();
        for (script, how) in [(Script::Type, "typed"), (Script::Hop, "leapt")] {
            seen += holds_beside(
                Page::Listing,
                script,
                &format!("{what} {how} beside art"),
                style,
                true,
            );
        }
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
        assert!(
            seen.crowned > 0,
            "{what}: the present read the ramps beside art"
        );
    }
}
