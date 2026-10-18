// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `harness_card_preview`: pictures of the harness card
//! (`aterm_effects::harness_card`, `docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §3, §10.6) for visual iteration and the owner's eye — pictures to look at,
//! not checks.
//!
//! Each panel is the emitter's real quads composited with the renderer's own
//! gradient law and blend functions (`aterm_render::glow_lerp_rgb`,
//! `glow_lerp`, `add_sat`, `over_premul`) over stand-in text bars where the
//! card's three rows sit, plus the line under the card one block per letter
//! in its ink. Written to `<out_dir>`:
//!
//! * `harness_card.png` (2×): nine panels, one above the next — the tall
//!   kitty dark settled, the burst as the ride ends, a blink with the head
//!   passing, Land settled, light settled, the light burst, a light Land
//!   tail flick, then the two-row kitty (the path row taken) dark and light
//!   settled.
//! * `ride.png` (2×): the ride (§10.7), nine panels — the tall kitty dark,
//!   the tall kitty light and the two-row kitty dark, each 0.3 s, 0.8 s and
//!   1.2 s into the episode (on its way, nearly seated, seated with the
//!   burst opening).
//! * `rearm.png` (2×): a card the glass showed at rest, armed again (the
//!   window regained focus, the Autopilot changed): six panels — the tall
//!   kitty dark, then light, each at its start, 0.3 s and 1.2 s in — the
//!   kitty in its seat throughout, the trail whole, the burst at the start.
//! * `kitty_frames_dark.png`, `kitty_frames_light.png` (5×): the kitty on its
//!   trail, cropped, in three columns — eyes open (settled), the blink and
//!   the tail flick (the episode's second of each) — and six rows: a black,
//!   a grey and the launch kitty's orange coat, each two rows tall then
//!   three.
//!
//! ```text
//! targo --unverified run -p aterm-effects --example harness_card_preview -- [out_dir]
//! (default out_dir: target/harness_card_preview)
//! ```

use std::path::Path;
use std::process::ExitCode;

use aterm_effects::cat_baker::ResolvedFills;
use aterm_effects::harness_card::{
    CardPalette, CardScene, EPISODE_MS, HarnessCard, RIDE_MS, kitty_cols,
};
use aterm_time::Instant;
use std::time::Duration;

/// One panel's size in pixels, before the 2× scale.
const PANEL_W: usize = 620;
const PANEL_H: usize = 100;

/// The cell the panels are laid out on.
const CW: usize = 9;
const CH: usize = 18;

/// The grid's origin inside a panel.
const ORIGIN: usize = 8;

/// The output scale: each pixel drawn as a `SCALE × SCALE` block.
const SCALE: usize = 2;

/// The kitty sheets' scale.
const KITTY_SCALE: usize = 5;

/// The episode's second blink and second tail flick, ms in
/// (`harness_card`'s `BLINK_AT_MS + BLINK_PERIOD_MS` and
/// `FLICK_AT_MS + FLICK_PERIOD_MS`, ten ms into each): the kitty seated, the
/// burst long gone.
const BLINK_2_MS: u64 = 1250 + 2500 + 10;
const FLICK_2_MS: u64 = 400 + 2500 + 10;

/// The ride sheet's moments, ms into the episode.
const RIDE_AT: [u64; 3] = [300, 800, 1200];

/// The re-arm sheet's moments, ms into the episode armed again.
const REARM_AT: [u64; 3] = [0, 300, 1200];

/// The coats the kitty sheets show (`cat_baker::COAT_RAMP` stops): black,
/// grey, and the launch kitty's orange.
const COATS: [u8; 3] = [1, 4, 12];

fn main() -> ExitCode {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/harness_card_preview".to_string());
    let out = Path::new(&out);
    if let Err(e) = std::fs::create_dir_all(out) {
        eprintln!("harness_card_preview: {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    let (kw, kitty_dark) = kitty_sheet(true);
    let (_, kitty_light) = kitty_sheet(false);
    let pictures = [
        ("harness_card.png", PANEL_W, SCALE, sheet()),
        ("ride.png", PANEL_W, SCALE, ride_sheet()),
        ("rearm.png", PANEL_W, SCALE, rearm_sheet()),
        ("kitty_frames_dark.png", kw, KITTY_SCALE, kitty_dark),
        ("kitty_frames_light.png", kw, KITTY_SCALE, kitty_light),
    ];
    for (name, w, scale, img) in &pictures {
        let path = out.join(name);
        if let Err(e) = write_png(&path, img, *w, *scale) {
            eprintln!("harness_card_preview: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("{}", path.display());
    }
    ExitCode::SUCCESS
}

/// A launch card at a 9 × 18 cell at the panel's top-left: text to column
/// 31, the trail to the kitty at column 40, the box 62 columns wide, four
/// boxed rows (the card's three and the line's).
fn base() -> CardScene {
    CardScene {
        origin_x: ORIGIN as i32,
        origin_y: ORIGIN as i32,
        cw: CW as u16,
        ch: CH as u16,
        row_base: 0,
        pad_left: 10,
        top: 0,
        rows: 4,
        cols: 62,
        clip: (0, 0, PANEL_W as i32, PANEL_H as i32),
        trail: Some((32, 40)),
        kitty_col: Some(40),
        kitty_rows: 3,
        palette: CardPalette::Cruise,
        dark: true,
        serious: false,
        coat: 0x00E8_A060,
        eye: 0x0040_C080,
    }
}

/// The panels, `0x00RRGGBB` pixels, [`PANEL_W`] wide and stacked.
fn sheet() -> Vec<u32> {
    let dark = base();
    let land = CardScene {
        palette: CardPalette::Land,
        ..dark
    };
    let light = CardScene {
        dark: false,
        ..dark
    };
    let light_land = CardScene {
        dark: false,
        ..land
    };
    let short = CardScene {
        kitty_rows: 2,
        ..dark
    };
    let short_light = CardScene {
        dark: false,
        ..short
    };
    let panels = [
        (dark, EPISODE_MS + 1),
        (dark, RIDE_MS + 150),
        (dark, 1300),
        (land, EPISODE_MS + 1),
        (light, EPISODE_MS + 1),
        (light, RIDE_MS + 200),
        (light_land, 2950),
        (short, EPISODE_MS + 1),
        (short_light, EPISODE_MS + 1),
    ];
    let mut img = vec![0u32; PANEL_W * PANEL_H * panels.len()];
    for (i, (scene, at)) in panels.iter().enumerate() {
        let panel = &mut img[i * PANEL_W * PANEL_H..(i + 1) * PANEL_W * PANEL_H];
        draw_panel(panel, scene, *at);
    }
    img
}

/// The ride's panels, [`PANEL_W`] wide and stacked: the tall kitty dark,
/// light, and the two-row kitty dark, each at the [`RIDE_AT`] moments.
fn ride_sheet() -> Vec<u32> {
    let dark = base();
    let scenes = [
        dark,
        CardScene {
            dark: false,
            ..dark
        },
        CardScene {
            kitty_rows: 2,
            ..dark
        },
    ];
    let panels: Vec<(CardScene, u64)> = scenes
        .iter()
        .flat_map(|&s| RIDE_AT.map(|at| (s, at)))
        .collect();
    let mut img = vec![0u32; PANEL_W * PANEL_H * panels.len()];
    for (i, (scene, at)) in panels.iter().enumerate() {
        let panel = &mut img[i * PANEL_W * PANEL_H..(i + 1) * PANEL_W * PANEL_H];
        draw_panel(panel, scene, *at);
    }
    img
}

/// The re-arm's panels, [`PANEL_W`] wide and stacked: a card shown at rest
/// and armed again, the tall kitty dark then light, each at [`REARM_AT`].
fn rearm_sheet() -> Vec<u32> {
    let dark = base();
    let scenes = [
        dark,
        CardScene {
            dark: false,
            ..dark
        },
    ];
    let panels: Vec<(CardScene, u64)> = scenes
        .iter()
        .flat_map(|&s| REARM_AT.map(|at| (s, at)))
        .collect();
    let mut img = vec![0u32; PANEL_W * PANEL_H * panels.len()];
    for (i, (scene, at)) in panels.iter().enumerate() {
        let panel = &mut img[i * PANEL_W * PANEL_H..(i + 1) * PANEL_W * PANEL_H];
        draw_card(panel, scene, *at, true);
    }
    img
}

/// The kitty sheet on a dark or a light ground ([`COATS`] × both heights
/// down, open / blink / flick across): its width in pixels, and its pixels.
fn kitty_sheet(dark: bool) -> (usize, Vec<u32>) {
    let s = CardScene { dark, ..base() };
    let col = |c: usize| ORIGIN + c * CW;
    let kc = usize::from(kitty_cols(CW as u16, CH as u16));
    let kitty = s.kitty_col.map_or(0, usize::from);
    // The crop: two cells of trail left of the kitty to two right of it, the
    // card's three rows and a few pixels either side.
    let (x0, x1) = (col(kitty - 3), col(kitty + kc + 2));
    let (y0, y1) = (ORIGIN - 4, ORIGIN + 3 * CH + 4);
    let (w, h) = (x1 - x0, y1 - y0);
    let times = [EPISODE_MS + 1, BLINK_2_MS, FLICK_2_MS];
    let rows: Vec<(u8, u8)> = COATS
        .iter()
        .flat_map(|&coat| [(coat, 2u8), (coat, 3u8)])
        .collect();
    let gap = 2;
    let sheet_w = times.len() * (w + gap) - gap;
    let mut img =
        vec![if dark { 0x0008_0808 } else { 0x00D0_D0D0 }; sheet_w * rows.len() * (h + gap)];
    let mut panel = vec![0u32; PANEL_W * PANEL_H];
    for (r, &(coat, kitty_rows)) in rows.iter().enumerate() {
        let fills = ResolvedFills::from_indices(coat, 8, dark);
        let scene = CardScene {
            kitty_rows,
            coat: pack_fill(fills.coat),
            eye: pack_fill(fills.iris),
            ..s
        };
        for (c, &at) in times.iter().enumerate() {
            draw_panel(&mut panel, &scene, at);
            for dy in 0..h {
                let dst = (r * (h + gap) + dy) * sheet_w + c * (w + gap);
                let src = (y0 + dy) * PANEL_W + x0;
                img[dst..dst + w].copy_from_slice(&panel[src..src + w]);
            }
        }
    }
    (sheet_w, img)
}

/// A cat baker fill (`0..=1` a channel) as `0x00RRGGBB`, as the host packs
/// it into the scene.
fn pack_fill((r, g, b): (f32, f32, f32)) -> u32 {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

/// One panel: the ground, stand-in text bars, the card `at` ms into its
/// first episode, and the line under the card.
fn draw_panel(img: &mut [u32], s: &CardScene, at: u64) {
    draw_card(img, s, at, false);
}

/// [`draw_panel`], `at` ms into an episode armed on a card fresh or, with
/// `shown`, one whose settled frame was on the glass first.
fn draw_card(img: &mut [u32], s: &CardScene, at: u64, shown: bool) {
    let bg = if s.dark { 0x001A_1B26 } else { 0x00FA_FAF8 };
    let fg = if s.dark { 0x00C0_C4D0 } else { 0x0030_3030 };
    img.fill(bg);
    // Stand-in text: dashed bars where the card's three rows sit — the
    // third, the working directory's, run on past the trail's start where
    // the scene stands the kitty two rows tall (the host's reason to).
    for r in 0..3 {
        let end = if r == 2 && s.kitty_rows < 3 { 50 } else { 31 };
        for x in (ORIGIN + CW * 11)..(ORIGIN + CW * end) {
            for y in (ORIGIN + r * CH + 5)..(ORIGIN + r * CH + 13) {
                if (x / 3) % 3 != 0 {
                    img[y * PANEL_W + x] = fg;
                }
            }
        }
    }
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    if shown {
        card.tick(t0, s, false, &mut Vec::new(), &mut Vec::new());
    }
    card.arm(t0, 7);
    let now = t0 + Duration::from_millis(at);
    let (mut under, mut over) = (Vec::new(), Vec::new());
    card.tick(now, s, true, &mut under, &mut over);
    for q in under.iter().chain(&over) {
        for dy in 0..usize::from(q.h) {
            for dx in 0..usize::from(q.w) {
                let (x, y) = (usize::from(q.x) + dx, usize::from(q.y) + dy);
                if x >= PANEL_W || y >= PANEL_H {
                    continue;
                }
                // The renderers' own gradient law and blends.
                let qw = u32::from(q.w);
                let c = aterm_render::glow_lerp_rgb(q.color, q.color2, dx as u32, qw);
                let a =
                    aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), dx as u32, qw)
                        as u8;
                let px = &mut img[y * PANEL_W + x];
                *px = if a == 0 {
                    aterm_render::add_sat(*px, c)
                } else {
                    aterm_render::over_premul(*px, c, a)
                };
            }
        }
    }
    // The line, one block per letter in its ink.
    for c in card.line_ink(now, 3, 11, 50, s.dark, true) {
        let ink =
            (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2]);
        let x0 = ORIGIN + usize::from(c.col) * CW;
        for x in x0 + 1..x0 + CW - 1 {
            for y in (ORIGIN + 3 * CH + 5)..(ORIGIN + 3 * CH + 13) {
                img[y * PANEL_W + x] = ink;
            }
        }
    }
}

/// Write `img`, `width` pixels a row, at `scale`× as an RGB PNG.
fn write_png(path: &Path, img: &[u32], width: usize, scale: usize) -> std::io::Result<()> {
    let (w, h) = (width * scale, img.len() / width * scale);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let p = img[(y / scale) * width + x / scale];
            rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    let to_u32 = |n: usize| u32::try_from(n).map_err(std::io::Error::other);
    let file = std::fs::File::create(path)?;
    let mut enc = aterm_png::Encoder::new(std::io::BufWriter::new(file), to_u32(w)?, to_u32(h)?);
    enc.set_color(aterm_png::ColorType::Rgb);
    enc.set_depth(aterm_png::BitDepth::Eight);
    enc.write_header()?
        .write_image_data(&rgb)
        .map_err(std::io::Error::other)
}
