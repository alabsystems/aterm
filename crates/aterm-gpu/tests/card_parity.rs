// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness card's fx layers on the GPU against the CPU
//! (`docs/DESIGN-card-engine-2026-10-19.md` §7 "B, the card": byte
//! comparisons in-process, one `RenderInput` through the CPU and the GPU
//! arm): a card's two layers — the border and the trail Under in window
//! space, the kitty, its rim and the stars Over in grid space, as
//! `harness_card::CardFrame::layers` lays them — are byte-exact (delta 0)
//! over a background-filled ground, at two cell sizes, on a dark ground
//! (additive light) and a light one (source-over ink), through every frame
//! of an episode (the ride-in, the burst) and the settled frame; the
//! settled frame replayed is byte-exact with no fx byte uploaded, and so is
//! the settled frame moved a row up by an inline scroll and the frame under
//! a tab menu's occluder. CONTROL: the comparator sees the kitty moved by one
//! pixel on the GPU's input alone.
//!
//! No GPU: COULD NOT RUN.

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

/// The card's box: its three rows and the line's, the trail from past the
/// text to the kitty, which stands three rows tall.
const BOX_ROWS: u16 = 4;
const BOX_COLS: u16 = 51;
const TRAIL: (u16, u16) = (22, 30);
const KITTY_COL: u16 = 30;

/// The card's name in the frame's layers.
const CARD: u64 = 7;

/// A wait for something that must happen (an episode's end) is a hang
/// detector, never a budget: more frames than any episode has.
const FRAMES_MAX: usize = 400;

/// A hidden-cursor terminal whose every cell is the ground `bg` (an SGR
/// background, `None`: the theme's): background fills only, byte-equal on
/// both renderers, so any delta is the card's.
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

/// The card's scene with its banner on grid row `top`, as the app lays it
/// for a pane that fills the window.
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

#[test]
fn the_cards_layers_are_byte_exact_on_the_gpu() {
    for px in [18.0, 30.0] {
        for (dark_theme, bg) in [(true, None), (false, Some("48;2;255;255;255"))] {
            let theme = Theme::default();
            let (mut cpu, mut gpu) = common::backends(px, theme);
            let mut win = aterm_gpu::WindowGpu::new();
            let mut t = ground_term(bg);
            let ground = if dark_theme { theme.bg } else { 0x00FF_FFFF };
            let dark = dark_ground(ground);
            assert_eq!(dark, dark_theme, "{px}: the ground's polarity");
            let what = format!("{px} px, {}", if dark { "dark" } else { "light" });
            let base = t.cell_frame(ROWS, COLS);
            let (cb, gb) = (
                cpu.render_input(&base),
                gpu.render_input(&mut win, &base, None),
            );
            #[cfg(target_os = "macos")]
            assert!(gpu.metal_render_armed(), "{what}: drawn on Metal");
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
            let mut drawn = 0;
            // The episode, frame by frame, then the settled frame.
            loop {
                let frame = card.draw(now, &at, true, &mut frames);
                let input = input_with(&mut t, layers(&frame, &at, &cpu), Vec::new());
                let (c, g) = (
                    cpu.render_input(&input),
                    gpu.present_input_readback(&mut win, &input),
                );
                same_extent(&c, &g);
                assert_eq!(
                    max_channel_delta(&c.pixels, &g.pixels),
                    0,
                    "{what}: episode frame {drawn}"
                );
                if drawn == 0 {
                    assert_ne!(c.pixels, cb.pixels, "{what}: the card paints");
                    assert!(
                        gpu.frame_shape().fx_instances > 0,
                        "{what}: the GPU built the card's instances"
                    );
                }
                drawn += 1;
                assert!(drawn < FRAMES_MAX, "{what}: the episode never ended");
                match card.deadline(now) {
                    Some(next) => now = next.max(now + Duration::from_millis(FRAME_MS)),
                    None => break,
                }
            }
            assert!(drawn > 10, "{what}: an episode of {drawn} frames");
            let settled = card.draw(now, &at, true, &mut frames);
            let settled_layers = layers(&settled, &at, &cpu);
            let input = input_with(&mut t, settled_layers.clone(), Vec::new());
            let (c, g) = (
                cpu.render_input(&input),
                gpu.present_input_readback(&mut win, &input),
            );
            same_extent(&c, &g);
            assert_eq!(
                max_channel_delta(&c.pixels, &g.pixels),
                0,
                "{what}: settled"
            );
            // Replayed: the same contents, nothing uploaded.
            let replayed = card
                .replay(&at, true, &frames)
                .expect("a settled card replays");
            assert!(Arc::ptr_eq(&replayed.under, &settled.under));
            let input = input_with(&mut t, layers(&replayed, &at, &cpu), Vec::new());
            let (c, g) = (
                cpu.render_input(&input),
                gpu.present_input_readback(&mut win, &input),
            );
            same_extent(&c, &g);
            assert_eq!(
                max_channel_delta(&c.pixels, &g.pixels),
                0,
                "{what}: replayed"
            );
            assert_eq!(
                gpu.frame_shape().fx_upload_bytes,
                0,
                "{what}: a replayed card uploads nothing"
            );
            // An inline scroll: the settled frame a row up, replayed.
            let up = scene(&cpu, top - 1, dark, ground);
            let moved = card
                .replay(&up, true, &frames)
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
            assert_eq!(
                max_channel_delta(&c.pixels, &g.pixels),
                0,
                "{what}: scrolled"
            );
            // Under a tab menu's occluder over the kitty and the border.
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
            assert_eq!(
                max_channel_delta(&c.pixels, &g.pixels),
                0,
                "{what}: occluded"
            );
            // CONTROL: the kitty a pixel right on the GPU's input alone.
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
                max_channel_delta(&c.pixels, &g.pixels) > 0,
                "{what}: CONTROL, the comparator sees the kitty moved a pixel"
            );
        }
    }
}
