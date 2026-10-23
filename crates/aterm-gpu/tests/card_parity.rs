// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU/GPU byte-parity tests for `harness_card` fx layers: delta 0, or one
//! LSB where `common::assert_byte_exact` names the GPU. No GPU: COULD NOT RUN.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_core::render::{FxLayer, FxSlot, PxRect, RenderInput};
use aterm_core::terminal::Terminal;
use aterm_effects::harness_card::{
    CardFrame, CardFrames, CardPalette, CardScene, FRAME_MS, HarnessCard, dark_ground,
};
use aterm_render::{Renderer, Theme};

use crate::common::{self, max_channel_delta};

const ROWS: usize = 12;
const COLS: usize = 80;

/// The card's box.
const BOX_ROWS: u16 = 4;
const BOX_COLS: u16 = 51;
const TRAIL: (u16, u16) = (22, 30);
const KITTY_COL: u16 = 30;

/// The card's name in the frame's layers.
const CARD: u64 = 7;

/// A wait for something that must happen (an episode's end) is a hang detector, never a budget.
const FRAMES_MAX: usize = 400;

/// Flow steps compared after the intro: the card flows on while it may animate.
const FLOW_STEPS: usize = 3;

/// A hidden-cursor terminal whose every cell is the ground `bg` (`None` uses the theme's).
fn ground_term(bg: Option<&str>) -> Terminal {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    t.process(b"\x1b[?25l");
    if let Some(bg) = bg {
        for row in 0..ROWS {
            t.process(
                format!("\x1b[{};1H\x1b[{bg}m{}\x1b[0m", row + 1, " ".repeat(COLS)).as_bytes(),
            );
        }
    }
    t
}

/// The card's scene with its banner on grid row `top`.
fn scene(cpu: &Renderer, top: u16, dark: bool, ground: u32) -> CardScene {
    let (cw, ch) = cpu.cell_size();
    let px = |v: usize| i32::try_from(v).expect("a pixel");
    let (pad, grid_top) = (px(cpu.pad()), px(cpu.grid_top()));
    let u16_of = |v: usize| u16::try_from(v).expect("a cell size");
    CardScene {
        origin_x: pad,
        origin_y: grid_top,
        cw: u16_of(cw),
        ch: u16_of(ch),
        row_base: 0,
        pad_left: u16_of(cpu.pad()),
        pad_above: u16_of(ch),
        pad_below: u16_of(ch),
        top,
        rows: BOX_ROWS,
        cols: BOX_COLS,
        clip: (0, grid_top, pad + px(COLS * cw), grid_top + px(ROWS * ch)),
        trail: Some(TRAIL),
        kitty_col: Some(KITTY_COL),
        kitty_rows: 3,
        palette: CardPalette::Cruise,
        dark,
        ground,
        serious: false,
        coat: 0x00E8_A060,
        eye: 0x0040_C080,
        wand: None,
    }
}

/// `frame`'s two layers as the app lays them for `scene`.
fn layers(frame: &CardFrame, scene: &CardScene, cpu: &Renderer) -> Vec<FxLayer> {
    let (x0, y0, x1, y1) = scene.clip;
    let grid = (
        i32::try_from(cpu.pad()).expect("a pixel"),
        i32::try_from(cpu.grid_top()).expect("a pixel"),
    );
    frame
        .layers(CARD, PxRect::new(x0, y0, x1, y1), grid)
        .to_vec()
}

fn input_with(t: &mut Terminal, layers: Vec<FxLayer>, occluders: Vec<PxRect>) -> RenderInput {
    let mut i = t.cell_frame(ROWS, COLS);
    i.fx_layers = layers;
    i.fx_occluders = occluders;
    i
}

/// The card's frame on the GPU is byte-exact with the CPU's
/// ([`common::assert_byte_exact`]: delta 0, or the one LSB a GPU whose blend
/// unit rounds each product on its own may miss a source-over pixel by, where
/// that module names the GPU).
#[track_caller]
fn exact(
    gpu: &aterm_gpu::GpuRenderer,
    what: &str,
    c: &aterm_render::Frame,
    g: &aterm_render::Frame,
    which: std::fmt::Arguments<'_>,
) {
    let delta = max_channel_delta(&c.pixels, &g.pixels);
    common::assert_byte_exact(
        "the_cards_layers_are_byte_exact_on_the_gpu",
        &format!("{what}: {which}"),
        gpu,
        delta,
        format_args!("{what}: {which}: CPU vs GPU max per-channel delta {delta}"),
    );
}

/// The RGB comparator must see complete frames, rather than a shared prefix.
fn same_extent(c: &aterm_render::Frame, g: &aterm_render::Frame) {
    assert_eq!((c.width, c.height), (g.width, g.height));
    assert_eq!(c.pixels.len(), g.pixels.len());
    let pixels = c
        .width
        .checked_mul(c.height)
        .expect("bounded fixture frame");
    assert_eq!(c.pixels.len(), pixels);
}

/// The renderers under test: the renderer's own arm (Metal on macOS, GL on
/// Linux) and, in the oracle build on macOS or Linux, the wgpu oracle, each
/// beside its own CPU twin.
fn arms(px: f32, theme: Theme) -> Vec<(&'static str, Renderer, aterm_gpu::GpuRenderer)> {
    let (cpu, gpu) = common::backends(px, theme);
    #[allow(unused_mut, reason = "the oracle build adds an arm")]
    let mut out = vec![("native", cpu, gpu)];
    #[cfg(all(wgpu_arm, any(target_os = "macos", target_os = "linux")))]
    {
        let (cpu, mut gpu) = common::backends(px, theme);
        common::disarm_for_oracle(&mut gpu);
        out.push(("wgpu-oracle", cpu, gpu));
    }
    out
}

/// After a frame: that it drew on the arm `arm` names (the oracle is
/// neither Metal nor GL).
fn assert_arm(arm: &str, gpu: &aterm_gpu::GpuRenderer, what: &str) {
    #[cfg(target_os = "macos")]
    assert_eq!(
        gpu.metal_render_armed(),
        arm == "native",
        "{what}: drawn on Metal"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        gpu.last_frame_on_gl(),
        arm == "native",
        "{what}: drawn on GL"
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = (arm, gpu, what);
}

/// THE CARD'S LAYERS ARE BYTE-EXACT ON THE GPU (design §4.9): its intro, its
/// flow, settled, replayed, scrolled and under a menu's occluder, on both
/// themes at two cell sizes, on every arm this build carries.
#[test]
fn the_cards_layers_are_byte_exact_on_the_gpu() {
    for px in [18.0, 30.0] {
        for (dark_theme, bg) in [(true, None), (false, Some("48;2;255;255;255"))] {
            let theme = Theme::default();
            for (arm, cpu, gpu) in arms(px, theme) {
                card_on_arm(arm, px, (dark_theme, bg), theme, cpu, gpu);
            }
        }
    }
}

/// [`the_cards_layers_are_byte_exact_on_the_gpu`] on one arm.
fn card_on_arm(
    arm: &str,
    px: f32,
    (dark_theme, bg): (bool, Option<&str>),
    theme: Theme,
    mut cpu: Renderer,
    mut gpu: aterm_gpu::GpuRenderer,
) {
    let mut win = aterm_gpu::WindowGpu::new();
    let mut t = ground_term(bg);
    let ground = if dark_theme { theme.bg } else { 0x00FF_FFFF };
    let dark = dark_ground(ground);
    assert_eq!(dark, dark_theme, "{px}: the ground's polarity");
    let what = format!("{arm}, {px} px, {}", if dark { "dark" } else { "light" });
    let base = t.cell_frame(ROWS, COLS);
    let (cb, gb) = (
        cpu.render_input(&base),
        gpu.render_input(&mut win, &base, None),
    );
    assert_arm(arm, &gpu, &what);
    same_extent(&cb, &gb);
    assert_eq!(
        max_channel_delta(&cb.pixels, &gb.pixels),
        0,
        "{what}: the ground must be byte-exact so any delta is the card's"
    );
    let top = 2;
    let at = scene(&cpu, top, dark, ground);
    let mut card = HarnessCard::default();
    let mut frames = CardFrames::default();
    let t0 = Instant::now();
    card.arm(t0, 0x5eed_ca4d);
    let mut now = t0;
    let (mut drawn, mut flowed) = (0, 0);
    loop {
        let frame = card.draw(now, &at, true, &mut frames);
        let input = input_with(&mut t, layers(&frame, &at, &cpu), Vec::new());
        let (c, g) = (
            cpu.render_input(&input),
            gpu.present_input_readback(&mut win, &input),
        );
        same_extent(&c, &g);
        exact(&gpu, &what, &c, &g, format_args!("episode frame {drawn}"));
        if drawn == 0 {
            assert_ne!(c.pixels, cb.pixels, "{what}: the card paints");
            assert!(
                gpu.frame_shape().fx_instances > 0,
                "{what}: the GPU built the card's instances"
            );
        }
        drawn += 1;
        assert!(drawn < FRAMES_MAX, "{what}: the intro never ended");
        if card.flowing(now) {
            flowed += 1;
            if flowed == FLOW_STEPS {
                break;
            }
        }
        match card.deadline(now) {
            Some(next) => now = next.max(now + Duration::from_millis(FRAME_MS)),
            None => break,
        }
    }
    assert!(drawn > 10, "{what}: an episode of {drawn} frames");
    assert_eq!(flowed, FLOW_STEPS, "{what}: the card flows after its intro");
    let settled = card.draw(now, &at, false, &mut frames);
    let settled_layers = layers(&settled, &at, &cpu);
    let input = input_with(&mut t, settled_layers.clone(), Vec::new());
    let (c, g) = (
        cpu.render_input(&input),
        gpu.present_input_readback(&mut win, &input),
    );
    same_extent(&c, &g);
    exact(&gpu, &what, &c, &g, format_args!("settled"));
    let replayed = card
        .replay(&at, false, &frames)
        .expect("a settled card replays");
    assert!(Arc::ptr_eq(&replayed.under, &settled.under));
    let input = input_with(&mut t, layers(&replayed, &at, &cpu), Vec::new());
    let (c, g) = (
        cpu.render_input(&input),
        gpu.present_input_readback(&mut win, &input),
    );
    same_extent(&c, &g);
    exact(&gpu, &what, &c, &g, format_args!("replayed"));
    assert_eq!(
        gpu.frame_shape().fx_upload_bytes,
        0,
        "{what}: a replayed card uploads nothing"
    );
    let up = scene(&cpu, top - 1, dark, ground);
    let moved = card
        .replay(&up, false, &frames)
        .expect("a scrolled card replays");
    assert!(
        Arc::ptr_eq(&moved.over, &settled.over),
        "{what}: the same contents"
    );
    let input = input_with(&mut t, layers(&moved, &up, &cpu), Vec::new());
    let (c, g) = (
        cpu.render_input(&input),
        gpu.present_input_readback(&mut win, &input),
    );
    same_extent(&c, &g);
    exact(&gpu, &what, &c, &g, format_args!("scrolled"));
    let (cw, ch) = cpu.cell_size();
    let (pad, grid_top) = (cpu.pad(), cpu.grid_top());
    let menu = PxRect::new(
        i32::try_from(pad + usize::from(KITTY_COL - 4) * cw).expect("a pixel"),
        i32::try_from(grid_top).expect("a pixel"),
        i32::try_from(pad + usize::from(KITTY_COL + 6) * cw).expect("a pixel"),
        i32::try_from(grid_top + 4 * ch).expect("a pixel"),
    );
    let input = input_with(&mut t, settled_layers.clone(), vec![menu]);
    let (c, g) = (
        cpu.render_input(&input),
        gpu.present_input_readback(&mut win, &input),
    );
    same_extent(&c, &g);
    exact(&gpu, &what, &c, &g, format_args!("occluded"));
    let mut nudged = settled_layers.clone();
    for l in nudged.iter_mut().filter(|l| l.slot == FxSlot::Over) {
        l.offset.0 += 1;
    }
    let input = input_with(&mut t, settled_layers.clone(), Vec::new());
    let c = cpu.render_input(&input);
    let input = input_with(&mut t, nudged, Vec::new());
    let g = gpu.present_input_readback(&mut win, &input);
    same_extent(&c, &g);
    assert!(
        max_channel_delta(&c.pixels, &g.pixels) > 1,
        "{what}: CONTROL, the comparator sees the kitty moved a pixel, past the one LSB a \
         waived GPU may miss by"
    );
}
