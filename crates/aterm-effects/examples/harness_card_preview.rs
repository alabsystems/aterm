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
//! card's three rows sit, Claude's block mascot left of them (the measured
//! 2.1.303 glyphs, drawn as the font draws block elements: whole cells and
//! their quadrants), plus the line under the card one block per letter in
//! its ink. The card sits on the panel's second row, a blank row over it and
//! one under the line, so the border stands in the box's air (§10.6.8) as
//! on the fullscreen renderer, and the air over and under the box can be
//! judged against the left side's, between the stroke and the mascot.
//! Written to `<out_dir>`:
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
//! * `ground_<theme>.png` (1×, drawn at a retina cell of [`HI_CW`] ×
//!   [`HI_CH`] device pixels, as the glass draws it), one per built-in ground
//!   the card is judged on — Aterm Night for reference, then every light
//!   built-in ([`GROUNDS`]) — each on the theme's own background, the card's
//!   words in real glyphs (the bundled JetBrains Mono) in the theme's own
//!   foreground, and the line's letters, each in the ink
//!   `HarnessCard::line_ink` gives it, led by the drawn `✦`
//!   (`DecoGlyph::Star4`, the host's): the tall kitty settled, the two-row
//!   kitty settled, the burst as the ride ends, the tall kitty mid-shimmer
//!   (the sheen halfway along the trail, the burst gone), Land settled and
//!   Land mid-shimmer.
//! * `zoom_<theme>.png` (3×): the trail, the kitty and its stars settled,
//!   on the ride (its sparkles shed), as the burst opens and a third of its
//!   way out, and mid-shimmer, then the line's first words.
//! * `grounds_zoom.png` (2×): the trail's last cells and the kitty, Cruise
//!   settled, the burst and Land settled across, one ground a row in the
//!   order above.
//! * `line_contrast.tsv`: the line's letters measured on each ground, per
//!   palette: the least WCAG contrast any letter has on any frame of the
//!   episode (and when, and which letter), the settled line's least and
//!   greatest, and the settled letters with their inks and contrasts.
//!
//! ```text
//! targo --unverified run -p aterm-effects --example harness_card_preview -- [out_dir]
//! (default out_dir: target/harness_card_preview)
//! ```

use std::path::Path;
use std::process::ExitCode;

use aterm_effects::cat_baker::ResolvedFills;
use aterm_effects::harness_card::{
    CardPalette, CardScene, EPISODE_MS, FRAME_MS, HarnessCard, RIDE_MS, kitty_cols,
};
use aterm_time::Instant;
use aterm_types::scheme::{Appearance, builtin};
use std::time::Duration;

/// One panel's size in pixels, before the 2× scale: the grid's six rows (a
/// blank row, the card's three, the line's, a blank row) and the origin's
/// margin either side.
const PANEL_W: usize = 620;
const PANEL_H: usize = ORIGIN + 6 * CH + ORIGIN;

/// The cell the panels are laid out on.
const CW: usize = 9;
const CH: usize = 18;

/// The grid's origin inside a panel.
const ORIGIN: usize = 8;

/// The card's banner row: a blank row over it (the box's air) and, under the
/// line, another.
const TOP: usize = 1;

/// Claude's mascot as the measured 2.1.303 card draws it (columns 0 to 8 of
/// its three rows), and its orange.
const MASCOT: [&str; 3] = [
    " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}",
    "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}",
    " \u{259D}\u{259D}   \u{259D}\u{259D}",
];
const MASCOT_RGB: u32 = 0x00D7_7757;

/// The generic sheets' grounds: a dark one, and white paper.
const DARK_BG: u32 = 0x001A_1B26;
const LIGHT_BG: u32 = 0x00FA_FAF8;

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

/// The built-in themes the ground sheets lay the card on: the dark
/// reference, then every light built-in — the four the owner judges it on
/// first, then the three spot-checked.
const GROUNDS: [&str; 8] = [
    "Aterm Night",
    "Aterm Day",
    "Solarized Light",
    "Linen",
    "Gruvbox Light",
    "Default Light",
    "Catppuccin Latte",
    "GitHub Light",
];

/// The mid-shimmer moment (ms in): the sheen's centre halfway along the
/// trail (`harness_card`'s `SHIMMER_MS` sweep at half a pass), the ride
/// and the burst over, the step wave still running.
const MID_SHIMMER_MS: u64 = 1500 + 750;

/// The burst's moment (ms in, on the card's first showing): the ride over,
/// the ring a sixth of its way out.
const BURST_AT_MS: u64 = RIDE_MS + 150;

/// The zoom sheet's scale.
const ZOOM_SCALE: usize = 2;

/// The per-ground zoom's scale.
const GROUND_ZOOM_SCALE: usize = 3;

/// The hi-fi ground sheets' cell, in device pixels: a retina cell, as the
/// glass draws the card (the emitter works in the stream's pixels).
const HI_CW: usize = 18;
const HI_CH: usize = 36;

/// Their glyphs' size (px): JetBrains Mono's advance is 0.6 em, so 30 px
/// fills an [`HI_CW`] cell.
const HI_FONT_PX: f32 = 30.0;

/// Their grid's origin inside a panel.
const HI_ORIGIN: usize = 16;

/// Their card's banner row: a blank row over it (the box's air, as the lab's
/// pane has over Claude Code's card) and, under the line, another.
const HI_TOP: usize = TOP;

/// The card's text column (Claude Code's mascot sits left of it).
const HI_TEXT_COL: usize = 11;

/// The card's three rows as Claude Code 2.1.302 writes them, and the two-row
/// kitty's third row: a working directory whose path runs on past the
/// trail's start.
const HI_ROWS: [&str; 3] = [
    "Claude Code v2.1.302",
    "Opus 5.5 \u{b7} API Usage Billing",
    "/private/tmp/at.dFeo/proj",
];
const HI_LONG_PATH: &str =
    "/private/tmp/at.dFeo/proj/a-working-directory-whose-name-runs-on-past-the-trail";

/// The line as a dev build writes it (`claude_card.rs`'s `make_line_texts`).
const LINE_CRUISE: &str =
    "enhanced by aterm harness v0.106.3866+gd243414 \u{b7} DEV \u{b7} autopilot \u{221e} cruise";
const LINE_LAND: &str =
    "enhanced by aterm harness v0.106.3866+gd243414 \u{b7} DEV \u{b7} autopilot \u{2913} land";

/// The `✦` and the space after it, ahead of the line's words
/// (`claude_card.rs`'s `LINE_LEAD`).
const LINE_LEAD: usize = 2;

/// The `✦`'s opacity on a light ground (`claude_card.rs`'s `make_line`):
/// opaque ink, as the line's letters are.
const STAR_INK_ALPHA: u8 = 255;

/// The trail's longest run and the kitty's right margin, in cells
/// (`claude_card.rs`'s `TRAIL_MAX` and `KITTY_MARGIN`): the lab's 120-column
/// pane has room for the longest trail.
const TRAIL_MAX: usize = 20;
const KITTY_MARGIN: usize = 4;

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
    let mut pictures = vec![
        ("harness_card.png".to_string(), PANEL_W, SCALE, sheet()),
        ("ride.png".to_string(), PANEL_W, SCALE, ride_sheet()),
        ("rearm.png".to_string(), PANEL_W, SCALE, rearm_sheet()),
        (
            "kitty_frames_dark.png".to_string(),
            kw,
            KITTY_SCALE,
            kitty_dark,
        ),
        (
            "kitty_frames_light.png".to_string(),
            kw,
            KITTY_SCALE,
            kitty_light,
        ),
    ];
    let grounds: Vec<Ground> = GROUNDS.iter().filter_map(|n| Ground::of(n)).collect();
    if grounds.len() != GROUNDS.len() {
        eprintln!("harness_card_preview: a ground in {GROUNDS:?} is not a built-in theme");
        return ExitCode::FAILURE;
    }
    let fonts = match Fonts::load() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("harness_card_preview: the bundled fonts: {e}");
            return ExitCode::FAILURE;
        }
    };
    for g in &grounds {
        let (w, sheet, zoom_w, zoom) = ground_sheet(g, &fonts);
        pictures.push((format!("ground_{}.png", g.slug), w, 1, sheet));
        pictures.push((
            format!("zoom_{}.png", g.slug),
            zoom_w,
            GROUND_ZOOM_SCALE,
            zoom,
        ));
    }
    let (zw, zoom) = zoom_sheet(&grounds, &fonts);
    pictures.push(("grounds_zoom.png".to_string(), zw, ZOOM_SCALE, zoom));
    let inks_path = out.join("line_contrast.tsv");
    if let Err(e) = std::fs::write(&inks_path, line_report(&grounds)) {
        eprintln!("harness_card_preview: {}: {e}", inks_path.display());
        return ExitCode::FAILURE;
    }
    println!("{}", inks_path.display());
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

/// A launch card at a 9 × 18 cell on the panel's second row: text to column
/// 31, the trail to the kitty at column 40, the box 62 columns wide, four
/// boxed rows (the card's three and the line's), a blank row over and under
/// them (its air).
fn base() -> CardScene {
    CardScene {
        origin_x: ORIGIN as i32,
        origin_y: ORIGIN as i32,
        cw: CW as u16,
        ch: CH as u16,
        row_base: 0,
        pad_left: 10,
        pad_above: CH as u16,
        pad_below: CH as u16,
        top: TOP as u16,
        rows: 4,
        cols: 62,
        clip: (0, 0, PANEL_W as i32, PANEL_H as i32),
        trail: Some((32, 40)),
        kitty_col: Some(40),
        kitty_rows: 3,
        palette: CardPalette::Cruise,
        dark: true,
        ground: DARK_BG,
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
    let (y0, y1) = (ORIGIN + TOP * CH - 4, ORIGIN + (TOP + 3) * CH + 4);
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

/// A built-in theme's ground: its background, its text, and whether it is
/// dark (the scene's `dark`, as the host decides it from the appearance).
struct Ground {
    slug: String,
    bg: u32,
    fg: u32,
    dark: bool,
}

impl Ground {
    /// The built-in theme `name`'s ground, read from the theme pack.
    fn of(name: &str) -> Option<Self> {
        let s = builtin(name)?;
        let px =
            |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        Some(Self {
            slug: name.to_ascii_lowercase().replace(' ', "_"),
            bg: px(s.background),
            fg: px(s.foreground),
            dark: s.appearance == Appearance::Dark,
        })
    }
}

/// The hi-fi panels' size in device pixels: the widest card (the two-row
/// kitty's, whose long path sets its box) and six rows — the air over the
/// card, its three, the line's and the air under it.
fn hi_panel() -> (usize, usize) {
    let widest = HI_TEXT_COL + HI_LONG_PATH.chars().count() + 2;
    (2 * HI_ORIGIN + widest * HI_CW, 2 * HI_ORIGIN + 6 * HI_CH)
}

/// The card's place on a hi-fi panel, as `claude_card.rs`'s `layout` lays
/// it on the lab's 120-column pane: the line under the card at its text
/// column, the trail from two cells past the first two rows' text at its
/// longest, the kitty after it, the box out to the kitty's margin or the
/// furthest text; the tall kitty where the third row is blank under it; the
/// border in the blank rows over and under the box (`Layout::air`).
fn hi_scene(g: &Ground, palette: CardPalette, tall: bool) -> CardScene {
    let cells = |s: &str| s.chars().count();
    let line = match palette {
        CardPalette::Cruise => LINE_CRUISE,
        CardPalette::Land => LINE_LAND,
    };
    let third = if tall { HI_ROWS[2] } else { HI_LONG_PATH };
    let ends = [
        HI_TEXT_COL + cells(HI_ROWS[0]),
        HI_TEXT_COL + cells(HI_ROWS[1]),
        HI_TEXT_COL + cells(third),
    ];
    let line_end = HI_TEXT_COL + LINE_LEAD + cells(line);
    let start = ends[0].max(ends[1]) + 2;
    let kitty = start + TRAIL_MAX;
    let kc = usize::from(kitty_cols(HI_CW as u16, HI_CH as u16));
    let text_end = ends.into_iter().chain([line_end]).max().unwrap_or(0);
    let cols = (kitty + kc + KITTY_MARGIN).max(text_end);
    let (w, h) = hi_panel();
    let u16_of = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
    CardScene {
        origin_x: HI_ORIGIN as i32,
        origin_y: HI_ORIGIN as i32,
        cw: HI_CW as u16,
        ch: HI_CH as u16,
        row_base: 0,
        pad_left: HI_ORIGIN as u16,
        pad_above: HI_CH as u16,
        pad_below: HI_CH as u16,
        top: HI_TOP as u16,
        rows: 4,
        cols: u16_of(cols),
        clip: (0, 0, w as i32, h as i32),
        trail: Some((u16_of(start), u16_of(kitty))),
        kitty_col: Some(u16_of(kitty)),
        kitty_rows: if tall { 3 } else { 2 },
        palette,
        dark: g.dark,
        ground: g.bg,
        serious: false,
        coat: 0x00E8_A060,
        eye: 0x0040_C080,
    }
}

/// The bundled faces the hi-fi panels set the card's words in: JetBrains
/// Mono regular and bold, and the symbol faces for a glyph it lacks.
struct Fonts {
    regular: aterm_render::font::Font,
    bold: aterm_render::font::Font,
    fallback: Vec<aterm_render::font::Font>,
}

impl Fonts {
    fn load() -> Result<Self, String> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-render/assets/bundled");
        let face = |name: &str| -> Result<aterm_render::font::Font, String> {
            let bytes = std::fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"))?;
            aterm_render::font::Font::from_bytes(bytes, aterm_render::font::FontSettings::default())
                .map_err(|e| format!("{name}: {e}"))
        };
        Ok(Self {
            regular: face("JetBrainsMono-Regular.ttf")?,
            bold: face("JetBrainsMono-Bold.ttf")?,
            fallback: vec![
                face("NotoSansMath-Regular.ttf")?,
                face("NotoSansSymbols2-Regular.ttf")?,
            ],
        })
    }

    /// The face that draws `ch` (`bold` asks for the bold face), and the
    /// baseline offset from a cell's top that centres JetBrains Mono's line
    /// in a [`HI_CH`] cell.
    fn face(&self, ch: char, bold: bool) -> &aterm_render::font::Font {
        let first = if bold { &self.bold } else { &self.regular };
        std::iter::once(first)
            .chain(&self.fallback)
            .find(|f| f.lookup_glyph_index(ch) != 0)
            .unwrap_or(first)
    }

    fn baseline(&self) -> f32 {
        let m = self.regular.horizontal_line_metrics(HI_FONT_PX);
        let (asc, desc) = m.map_or((HI_FONT_PX * 0.8, -HI_FONT_PX * 0.2), |m| {
            (m.ascent, m.descent)
        });
        (HI_CH as f32 - (asc - desc)) * 0.5 + asc
    }
}

/// One cell's glyph `ch` at grid cell `(row, col)` of a `w`-wide panel, in
/// `fg` over the ground `bg`, through the renderer's own text blend
/// (`text_blending`'s default, linear-corrected).
#[allow(
    clippy::too_many_arguments,
    reason = "the panel, its width, the faces, the cell, the glyph and its two colours"
)]
fn draw_glyph(
    img: &mut [u32],
    w: usize,
    fonts: &Fonts,
    (row, col): (usize, usize),
    ch: char,
    bold: bool,
    fg: u32,
    bg: u32,
) {
    if ch == ' ' {
        return;
    }
    let face = fonts.face(ch, bold);
    let (m, cov) = face.rasterize(ch, HI_FONT_PX);
    let pen = (HI_ORIGIN + col * HI_CW) as i32;
    let base = (HI_ORIGIN + row * HI_CH) as f32 + fonts.baseline();
    let x0 = pen + m.xmin;
    let y0 = base.round() as i32 - (m.ymin + m.height as i32);
    let h = img.len() / w;
    for gy in 0..m.height {
        for gx in 0..m.width {
            let t = cov[gy * m.width + gx];
            let (x, y) = (x0 + gx as i32, y0 + gy as i32);
            if t == 0 || x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                continue;
            }
            let px = &mut img[y as usize * w + x as usize];
            *px = aterm_render::blend_text(*px, fg, bg, t, true) & 0x00FF_FFFF;
        }
    }
}

/// `text` from grid cell `(row, col)`, one char a cell.
#[allow(
    clippy::too_many_arguments,
    reason = "the panel, its width, the faces, the place, the words and their colours"
)]
fn draw_text(
    img: &mut [u32],
    w: usize,
    fonts: &Fonts,
    (row, col): (usize, usize),
    text: &str,
    bold: bool,
    fg: u32,
    bg: u32,
) {
    for (i, ch) in text.chars().enumerate() {
        draw_glyph(img, w, fonts, (row, col + i), ch, bold, fg, bg);
    }
}

/// `a` mixed toward `b` by `t`, per channel.
fn mix_px(a: u32, b: u32, t: f32) -> u32 {
    let ch = |sh: u32| {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        ((x + (y - x) * t).round() as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
}

/// The card's line, letter by letter, at `now`: each letter's ink as
/// `HarnessCard::line_ink` gives it for this ground.
fn line_colours(card: &HarnessCard, now: Instant, len: usize, g: &Ground) -> Vec<u32> {
    let col = u16::try_from(HI_TEXT_COL + LINE_LEAD).unwrap_or(0);
    let len = u16::try_from(len).unwrap_or(u16::MAX);
    card.line_ink(now, (HI_TOP + 3) as u16, col, len, g.bg, true)
        .iter()
        .map(|c| {
            (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2])
        })
        .collect()
}

/// One hi-fi panel: the ground, the card `at` ms into its first episode
/// stacked as the renderers stack it — its `under` stream (the border, the
/// trail, the rim), then the glyphs (Claude's mascot, the card's words in
/// the theme's foreground, the banner's name bold and the rest dimmed as
/// Claude Code dims them, and the line: the drawn `✦` and its letters in
/// their inks), then its `over` stream (the kitty and its stars).
fn draw_hi(img: &mut [u32], w: usize, fonts: &Fonts, s: &CardScene, g: &Ground, at: u64) {
    img.fill(g.bg);
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    let now = t0 + Duration::from_millis(at);
    let (mut under, mut over) = (Vec::new(), Vec::new());
    card.tick(now, s, true, &mut under, &mut over);
    composite(img, w, under.iter());
    draw_mascot_on(img, w, (HI_CW, HI_CH), HI_ORIGIN, HI_TOP);
    let dim = mix_px(g.fg, g.bg, 0.45);
    let tall = s.kitty_rows >= 3;
    let third = if tall { HI_ROWS[2] } else { HI_LONG_PATH };
    let words: [(usize, usize, &str, bool, u32); 4] = [
        (0, 0, "Claude Code", true, g.fg),
        (0, 12, "v2.1.302", false, dim),
        (1, 0, HI_ROWS[1], false, dim),
        (2, 0, third, false, dim),
    ];
    for (row, col, text, bold, fg) in words {
        let cell = (HI_TOP + row, HI_TEXT_COL + col);
        draw_text(img, w, fonts, cell, text, bold, fg, g.bg);
    }
    let line = match s.palette {
        CardPalette::Cruise => LINE_CRUISE,
        CardPalette::Land => LINE_LAND,
    };
    let inks = line_colours(&card, now, line.chars().count(), g);
    for (i, ch) in line.chars().enumerate() {
        if let Some(&ink) = inks.get(i) {
            let cell = (HI_TOP + 3, HI_TEXT_COL + LINE_LEAD + i);
            draw_glyph(img, w, fonts, cell, ch, false, ink, g.bg);
        }
    }
    // The `✦`: the host's drawn star in the line's first ink, added light
    // on dark, source-over ink at its opacity on light.
    let star = inks.first().copied().unwrap_or(g.fg);
    let mask =
        aterm_render::procedural::deco_coverage(aterm_render::DecoGlyph::Star4, HI_CW, HI_CH);
    let (sx, sy) = (
        HI_ORIGIN + HI_TEXT_COL * HI_CW,
        HI_ORIGIN + (HI_TOP + 3) * HI_CH,
    );
    for y in 0..HI_CH {
        for x in 0..HI_CW {
            let cov = u32::from(mask[y * HI_CW + x]);
            if cov == 0 {
                continue;
            }
            let px = &mut img[(sy + y) * w + sx + x];
            *px = if g.dark {
                aterm_render::add_sat(*px, aterm_render::premul_rgb(star, cov as u8))
            } else {
                let a = (cov * u32::from(STAR_INK_ALPHA) / 255) as u8;
                aterm_render::over_premul(*px, aterm_render::premul_rgb(star, a), a)
            };
        }
    }
    composite(img, w, over.iter());
}

/// Composite `quads` onto a `w`-wide panel with the renderers' own gradient
/// law and blends.
fn composite<'q>(
    img: &mut [u32],
    w: usize,
    quads: impl Iterator<Item = &'q aterm_render::GlowQuad>,
) {
    let h = img.len() / w;
    for q in quads {
        for dy in 0..usize::from(q.h) {
            for dx in 0..usize::from(q.w) {
                let (x, y) = (usize::from(q.x) + dx, usize::from(q.y) + dy);
                if x >= w || y >= h {
                    continue;
                }
                let qw = u32::from(q.w);
                let c = aterm_render::glow_lerp_rgb(q.color, q.color2, dx as u32, qw);
                let a =
                    aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), dx as u32, qw)
                        as u8;
                let px = &mut img[y * w + x];
                *px = if a == 0 {
                    aterm_render::add_sat(*px, c)
                } else {
                    aterm_render::over_premul(*px, c, a)
                };
            }
        }
    }
}

/// The hi-fi moments, top to bottom: `(palette, tall kitty, ms in)`.
const HI_SHOTS: [(CardPalette, bool, u64); 6] = [
    (CardPalette::Cruise, true, EPISODE_MS + 1),
    (CardPalette::Cruise, false, EPISODE_MS + 1),
    (CardPalette::Cruise, true, BURST_AT_MS),
    (CardPalette::Cruise, true, MID_SHIMMER_MS),
    (CardPalette::Land, true, EPISODE_MS + 1),
    (CardPalette::Land, true, MID_SHIMMER_MS),
];

/// The per-ground zoom's moments (ms into the card's first episode): settled,
/// the ride with its sparkles shed, the burst opening and a third of its
/// way out, and mid-shimmer.
const ZOOM_AT: [u64; 5] = [
    EPISODE_MS + 1,
    800,
    BURST_AT_MS,
    RIDE_MS + 330,
    MID_SHIMMER_MS,
];

/// Copy the rectangle `(x0, y0, w, h)` of a `pw`-wide panel into a sheet
/// `sw` wide at `(dx, dy)`.
#[allow(
    clippy::too_many_arguments,
    reason = "source and its width, the crop, the sheet and its width, the place"
)]
fn blit(
    panel: &[u32],
    pw: usize,
    (x0, y0, w, h): (usize, usize, usize, usize),
    sheet: &mut [u32],
    sw: usize,
    (dx, dy): (usize, usize),
) {
    for r in 0..h {
        let src = (y0 + r) * pw + x0;
        let dst = (dy + r) * sw + dx;
        sheet[dst..dst + w].copy_from_slice(&panel[src..src + w]);
    }
}

/// One ground's hi-fi panels, stacked ([`HI_SHOTS`]), and its zoom: the
/// trail, the kitty and its stars settled, at the burst and mid-shimmer,
/// then the line's first words: `(width, panels, zoom width, zoom)`.
fn ground_sheet(g: &Ground, fonts: &Fonts) -> (usize, Vec<u32>, usize, Vec<u32>) {
    let (w, h) = hi_panel();
    let mut img = vec![0u32; w * h * HI_SHOTS.len()];
    for (i, &(palette, tall, at)) in HI_SHOTS.iter().enumerate() {
        let s = hi_scene(g, palette, tall);
        draw_hi(&mut img[i * w * h..(i + 1) * w * h], w, fonts, &s, g, at);
    }
    // The zoom: the trail's start to the box's right edge over the card's
    // first three rows, for the settled, burst and mid-shimmer panels; then
    // the line's first 40 cells.
    let s = hi_scene(g, CardPalette::Cruise, true);
    let (t0, _) = s.trail.unwrap_or((0, 0));
    let x0 = HI_ORIGIN + (usize::from(t0) - 1) * HI_CW;
    let x1 = (HI_ORIGIN + (usize::from(s.cols) + 1) * HI_CW).min(w);
    let zw = x1 - x0;
    // The card's three rows and the top border in the air over them.
    let crop_y = HI_ORIGIN + HI_TOP * HI_CH - HI_CH / 2 - 4;
    let crop_h = 3 * HI_CH + HI_CH / 2 + 8;
    let line_w = 40 * HI_CW;
    let gap = 4;
    let zh = ZOOM_AT.len() * (crop_h + gap) + HI_CH + 8;
    let mut zoom = vec![0x0080_8080u32; zw * zh];
    let mut panel = vec![0u32; w * h];
    for (k, &at) in ZOOM_AT.iter().enumerate() {
        draw_hi(&mut panel, w, fonts, &s, g, at);
        blit(
            &panel,
            w,
            (x0, crop_y, zw, crop_h),
            &mut zoom,
            zw,
            (0, k * (crop_h + gap)),
        );
    }
    let shots = ZOOM_AT;
    let line_x = HI_ORIGIN + (HI_TEXT_COL - 1) * HI_CW;
    blit(
        &img[..w * h],
        w,
        (
            line_x,
            HI_ORIGIN + (HI_TOP + 3) * HI_CH - 4,
            line_w.min(zw),
            HI_CH + 8,
        ),
        &mut zoom,
        zw,
        (0, shots.len() * (crop_h + gap)),
    );
    (w, img, zw, zoom)
}

/// The zoom sheet: per ground (a row each), the trail's last cells and the
/// kitty with its stars, Cruise settled, the burst and Land settled across.
fn zoom_sheet(grounds: &[Ground], fonts: &Fonts) -> (usize, Vec<u32>) {
    let (pw, ph) = hi_panel();
    let Some(first) = grounds.first() else {
        return (0, Vec::new());
    };
    let s = hi_scene(first, CardPalette::Cruise, true);
    let kitty = s.kitty_col.map_or(0, usize::from);
    let kc = usize::from(kitty_cols(HI_CW as u16, HI_CH as u16));
    let x0 = HI_ORIGIN + (kitty - 8) * HI_CW;
    let x1 = HI_ORIGIN + (kitty + kc + 3) * HI_CW;
    // The card's three rows, and the top border in the air over them.
    let (y0, y1) = (
        HI_ORIGIN + HI_TOP * HI_CH - HI_CH / 2 - 4,
        HI_ORIGIN + (HI_TOP + 3) * HI_CH + 4,
    );
    let (w, h) = (x1 - x0, y1 - y0);
    let gap = 3;
    let cols = 3;
    let sheet_w = cols * (w + gap) - gap;
    let mut img = vec![0x0080_8080; sheet_w * grounds.len() * (h + gap)];
    let mut panel = vec![0u32; pw * ph];
    for (r, g) in grounds.iter().enumerate() {
        let shots = [
            (CardPalette::Cruise, EPISODE_MS + 1),
            (CardPalette::Cruise, BURST_AT_MS),
            (CardPalette::Land, EPISODE_MS + 1),
        ];
        for (c, &(palette, at)) in shots.iter().enumerate() {
            let s = hi_scene(g, palette, true);
            draw_hi(&mut panel, pw, fonts, &s, g, at);
            blit(
                &panel,
                pw,
                (x0, y0, w, h),
                &mut img,
                sheet_w,
                (c * (w + gap), r * (h + gap)),
            );
        }
    }
    (sheet_w, img)
}

/// WCAG 2 contrast of `a` against `b`.
fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (
        aterm_effects::color_math::relative_luminance(a),
        aterm_effects::color_math::relative_luminance(b),
    );
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The line's letters measured against each ground, both palettes (the
/// `✦` wears the first letter's ink): the least WCAG contrast any letter has
/// on any frame of the card's first episode, a frame every [`FRAME_MS`], and
/// when and which letter; the settled line's least and greatest; then the
/// settled line letter by letter (`char#ink:contrast`).
fn line_report(grounds: &[Ground]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from(
        "ground\tbg\tpalette\tepisode_min\tat_ms\tletter\tsettled_min\tsettled_max\tsettled letters\n",
    );
    for g in grounds {
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            let s = hi_scene(g, palette, true);
            let line = match palette {
                CardPalette::Cruise => LINE_CRUISE,
                CardPalette::Land => LINE_LAND,
            };
            let letters: Vec<char> = line.chars().collect();
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let mut worst = (f32::MAX, 0u64, 0usize);
            let mut at = 0;
            while at <= EPISODE_MS + FRAME_MS {
                let now = t0 + Duration::from_millis(at);
                card.tick(now, &s, true, &mut Vec::new(), &mut Vec::new());
                for (i, ink) in line_colours(&card, now, letters.len(), g)
                    .into_iter()
                    .enumerate()
                {
                    let k = contrast(ink, g.bg);
                    if k < worst.0 {
                        worst = (k, at, i);
                    }
                }
                at += FRAME_MS;
            }
            let now = t0 + Duration::from_millis(EPISODE_MS + FRAME_MS);
            let settled = line_colours(&card, now, letters.len(), g);
            let ks: Vec<f32> = settled.iter().map(|&ink| contrast(ink, g.bg)).collect();
            let (lo, hi) = ks
                .iter()
                .fold((f32::MAX, 0.0f32), |(lo, hi), &k| (lo.min(k), hi.max(k)));
            let which = letters.get(worst.2).copied().unwrap_or(' ');
            let _ = write!(
                out,
                "{}\t{:06X}\t{palette:?}\t{:.2}\t{}\t{}:{which}\t{lo:.2}\t{hi:.2}\t",
                g.slug, g.bg, worst.0, worst.1, worst.2
            );
            for ((ch, ink), k) in letters.iter().zip(&settled).zip(&ks) {
                let _ = write!(out, "{ch}#{ink:06X}:{k:.2} ");
            }
            out.push('\n');
        }
    }
    out
}

/// One panel: the ground, stand-in text bars, the card `at` ms into its
/// first episode, and the line under the card.
fn draw_panel(img: &mut [u32], s: &CardScene, at: u64) {
    draw_card(img, s, at, false);
}

/// [`draw_panel`], `at` ms into an episode armed on a card fresh or, with
/// `shown`, one whose settled frame was on the glass first.
fn draw_card(img: &mut [u32], s: &CardScene, at: u64, shown: bool) {
    let bg = if s.dark { DARK_BG } else { LIGHT_BG };
    let fg = if s.dark { 0x00C0_C4D0 } else { 0x0030_3030 };
    draw_card_on(img, s, at, shown, (bg, fg));
}

/// [`draw_card`] on a given ground: `(background, text)`.
fn draw_card_on(img: &mut [u32], s: &CardScene, at: u64, shown: bool, (bg, fg): (u32, u32)) {
    // The scene on this ground, as the host passes the pane's.
    let s = &CardScene { ground: bg, ..*s };
    img.fill(bg);
    // Stand-in text: dashed bars where the card's three rows sit — the
    // third, the working directory's, run on past the trail's start where
    // the scene stands the kitty two rows tall (the host's reason to).
    for r in 0..3 {
        let end = if r == 2 && s.kitty_rows < 3 { 50 } else { 31 };
        let band = ORIGIN + (TOP + r) * CH;
        for x in (ORIGIN + CW * 11)..(ORIGIN + CW * end) {
            for y in (band + 5)..(band + 13) {
                if (x / 3) % 3 != 0 {
                    img[y * PANEL_W + x] = fg;
                }
            }
        }
    }
    draw_mascot_on(img, PANEL_W, (CW, CH), ORIGIN, TOP);
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
    let line = TOP + 3;
    for c in card.line_ink(now, line as u16, 11, 50, bg, true) {
        let ink =
            (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2]);
        let x0 = ORIGIN + usize::from(c.col) * CW;
        for x in x0 + 1..x0 + CW - 1 {
            for y in (ORIGIN + line * CH + 5)..(ORIGIN + line * CH + 13) {
                img[y * PANEL_W + x] = ink;
            }
        }
    }
}

/// Claude's mascot ([`MASCOT`]) on the card's three rows, from row `top`
/// of a `w`-wide panel whose grid is `(cw, ch)` cells from `origin`, each
/// block element filling the quadrants of its cell the font fills (a cell's
/// left and right halves split at `cw / 2`, its top and bottom at `ch / 2`).
fn draw_mascot_on(img: &mut [u32], w: usize, (cw, ch): (usize, usize), origin: usize, top: usize) {
    for (r, row) in MASCOT.iter().enumerate() {
        for (c, glyph) in row.chars().enumerate() {
            // Upper left, upper right, lower left, lower right.
            let quads: [bool; 4] = match glyph {
                '\u{2588}' => [true; 4],
                '\u{2580}' => [true, true, false, false],
                '\u{2590}' => [false, true, false, true],
                '\u{259B}' => [true, true, true, false],
                '\u{259C}' => [true, true, false, true],
                '\u{259D}' => [false, true, false, false],
                _ => [false; 4],
            };
            let (x0, y0) = (origin + c * cw, origin + (top + r) * ch);
            for (k, _) in quads.iter().enumerate().filter(|(_, on)| **on) {
                let (xa, xb) = if k % 2 == 0 {
                    (0, cw / 2)
                } else {
                    (cw / 2, cw)
                };
                let (ya, yb) = if k < 2 { (0, ch / 2) } else { (ch / 2, ch) };
                for y in y0 + ya..y0 + yb {
                    img[y * w + x0 + xa..y * w + x0 + xb].fill(MASCOT_RGB);
                }
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
