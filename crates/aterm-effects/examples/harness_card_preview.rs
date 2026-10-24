// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renders preview PNG panels of `aterm_effects::harness_card` across [`GROUNDS`].

use std::path::Path;
use std::process::ExitCode;

use aterm_core::render::{FxLayer, PxRect};
use aterm_effects::cat_baker::ResolvedFills;
use aterm_effects::harness_card::{
    CardFrame, CardFrames, CardPalette, CardScene, EPISODE_MS, FRAME_MS, HarnessCard, LineAt,
    RIDE_MS, kitty_cols, line_star,
};
use aterm_effects::ripple::{FLOW_STEP_MS, GLINT_PERIOD_MS};
use aterm_time::Instant;
use aterm_types::scheme::{Appearance, builtin};
use std::time::Duration;

const PANEL_W: usize = 620;
const PANEL_H: usize = ORIGIN + 6 * CH + ORIGIN;

const CW: usize = 9;
const CH: usize = 18;

const ORIGIN: usize = 8;

const TOP: usize = 1;

const MASCOT: [&str; 3] = [
    " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}",
    "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}",
    " \u{259D}\u{259D}   \u{259D}\u{259D}",
];
const MASCOT_RGB: u32 = 0x00D7_7757;

const DARK_BG: u32 = 0x001A_1B26;
const LIGHT_BG: u32 = 0x00FA_FAF8;

const SCALE: usize = 2;

const KITTY_SCALE: usize = 5;

const BLINK_2_MS: u64 = 1250 + 2500 + 10;
const FLICK_2_MS: u64 = 400 + 2500 + 10;

const RIDE_AT: [u64; 3] = [300, 800, 1200];

const REARM_AT: [u64; 3] = [0, 300, 1200];

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

const MID_SHIMMER_MS: u64 = 1500 + 750;

const BURST_AT_MS: u64 = RIDE_MS + 150;

const ZOOM_SCALE: usize = 2;

const FLOW_AT: [u64; 4] = [
    2 * GLINT_PERIOD_MS,
    2 * GLINT_PERIOD_MS + 6 * FLOW_STEP_MS,
    2 * GLINT_PERIOD_MS + 12 * FLOW_STEP_MS,
    2 * GLINT_PERIOD_MS + 18 * FLOW_STEP_MS,
];

const HERO: std::ops::Range<u16> = 12..17;

/// When a panel shows the card.
#[derive(Clone, Copy)]
enum At {
    /// Held: the settled frame.
    Settled,
    /// This many ms after the arm.
    Ms(u64),
}

impl At {
    fn ms(self) -> (u64, bool) {
        match self {
            Self::Settled => (EPISODE_MS + 1, false),
            Self::Ms(ms) => (ms, true),
        }
    }
}

fn tick_to(
    card: &mut HarnessCard,
    t0: Instant,
    s: &CardScene,
    at: At,
) -> (Instant, bool, CardFrame) {
    let (ms, animate) = at.ms();
    let mut frames = CardFrames::default();
    if animate && ms >= EPISODE_MS {
        for then in [0, EPISODE_MS - 5] {
            card.draw(t0 + Duration::from_millis(then), s, true, &mut frames);
        }
    }
    let now = t0 + Duration::from_millis(ms);
    let frame = card.draw(now, s, animate, &mut frames);
    (now, animate, frame)
}

const GROUND_ZOOM_SCALE: usize = 3;

const HI_CW: usize = 18;
const HI_CH: usize = 36;

const HI_FONT_PX: f32 = 30.0;

const HI_ORIGIN: usize = 16;

const HI_TOP: usize = TOP;

const HI_TEXT_COL: usize = 11;

const HI_ROWS: [&str; 3] = [
    "Claude Code v2.1.302",
    "Opus 5.5 \u{b7} API Usage Billing",
    "/private/tmp/at.dFeo/proj",
];
const HI_LONG_PATH: &str =
    "/private/tmp/at.dFeo/proj/a-working-directory-whose-name-runs-on-past-the-trail";

const LINE_CRUISE: &str =
    "enhanced by aterm harness v0.106.3866+gd243414 \u{b7} DEV \u{b7} autopilot \u{221e} cruise";
const LINE_LAND: &str =
    "enhanced by aterm harness v0.106.3866+gd243414 \u{b7} DEV \u{b7} autopilot \u{2913} land";
const LINE_OFF: &str =
    "enhanced by aterm harness v0.106.3866+gd243414 \u{b7} DEV \u{b7} autopilot \u{25cb} off";

fn line_of(palette: CardPalette) -> &'static str {
    match palette {
        CardPalette::Cruise => LINE_CRUISE,
        CardPalette::Land => LINE_LAND,
        CardPalette::Off => LINE_OFF,
    }
}

const fn wand_at(top: usize) -> Option<(u16, u16)> {
    Some((8, top as u16))
}

const LINE_LEAD: usize = 2;

const STAR_INK_ALPHA: u8 = 255;

const TRAIL_MAX: usize = 20;
const KITTY_MARGIN: usize = 4;

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
        ("flow.png".to_string(), PANEL_W, SCALE, flow_sheet()),
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
    let (gw, glints) = glint_sheet(&grounds);
    pictures.push(("glints.png".to_string(), gw, GLINT_SCALE, glints));
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
        wand: wand_at(TOP),
    }
}

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
    let off = CardScene {
        palette: CardPalette::Off,
        ..dark
    };
    let light_off = CardScene { dark: false, ..off };
    let panels = [
        (dark, At::Settled),
        (dark, At::Ms(RIDE_MS + 150)),
        (dark, At::Ms(1300)),
        (land, At::Settled),
        (light, At::Settled),
        (light, At::Ms(RIDE_MS + 200)),
        (light_land, At::Ms(2950)),
        (short, At::Settled),
        (short_light, At::Settled),
        (off, At::Settled),
        (light_off, At::Settled),
    ];
    let mut img = vec![0u32; PANEL_W * PANEL_H * panels.len()];
    for (i, (scene, at)) in panels.iter().enumerate() {
        let panel = &mut img[i * PANEL_W * PANEL_H..(i + 1) * PANEL_W * PANEL_H];
        draw_panel(panel, scene, *at);
    }
    img
}

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
    let panels: Vec<(CardScene, At)> = scenes
        .iter()
        .flat_map(|&s| RIDE_AT.map(|at| (s, At::Ms(at))))
        .collect();
    let mut img = vec![0u32; PANEL_W * PANEL_H * panels.len()];
    for (i, (scene, at)) in panels.iter().enumerate() {
        let panel = &mut img[i * PANEL_W * PANEL_H..(i + 1) * PANEL_W * PANEL_H];
        draw_panel(panel, scene, *at);
    }
    img
}

fn rearm_sheet() -> Vec<u32> {
    let dark = base();
    let scenes = [
        dark,
        CardScene {
            dark: false,
            ..dark
        },
    ];
    let panels: Vec<(CardScene, At)> = scenes
        .iter()
        .flat_map(|&s| REARM_AT.map(|at| (s, At::Ms(at))))
        .collect();
    let mut img = vec![0u32; PANEL_W * PANEL_H * panels.len()];
    for (i, (scene, at)) in panels.iter().enumerate() {
        let panel = &mut img[i * PANEL_W * PANEL_H..(i + 1) * PANEL_W * PANEL_H];
        draw_card(panel, scene, *at, true);
    }
    img
}

fn flow_sheet() -> Vec<u32> {
    let dark = base();
    let scenes = [
        dark,
        CardScene {
            palette: CardPalette::Land,
            ..dark
        },
        CardScene {
            palette: CardPalette::Off,
            ..dark
        },
        CardScene {
            dark: false,
            ..dark
        },
    ];
    let panels: Vec<(CardScene, At)> = scenes
        .iter()
        .flat_map(|&s| FLOW_AT.map(|at| (s, At::Ms(at))))
        .collect();
    let mut img = vec![0u32; PANEL_W * PANEL_H * panels.len()];
    for (i, (scene, at)) in panels.iter().enumerate() {
        let panel = &mut img[i * PANEL_W * PANEL_H..(i + 1) * PANEL_W * PANEL_H];
        draw_panel(panel, scene, *at);
    }
    img
}

const GLINT_AT: [u64; 7] = [1500, 1700, 1900, 2100, 2300, 2500, 2700];

const GLINT_SCALE: usize = 2;

fn glint_sheet(grounds: &[Ground]) -> (usize, Vec<u32>) {
    let (pw, ph) = hi_panel();
    let grounds: Vec<&Ground> = grounds
        .iter()
        .filter(|g| g.slug == "aterm_night" || g.slug == "aterm_day")
        .collect();
    let Some(first) = grounds.first() else {
        return (0, Vec::new());
    };
    let probe = hi_scene(first, CardPalette::Cruise, true);
    let (t0, _) = probe.trail.unwrap_or((1, 1));
    let kitty = probe.kitty_col.map_or(0, usize::from);
    let kc = usize::from(kitty_cols(HI_CW as u16, HI_CH as u16));
    let x0 = HI_ORIGIN + (usize::from(t0) - 1) * HI_CW;
    let x1 = HI_ORIGIN + (kitty + kc + 1) * HI_CW;
    let (y0, y1) = (
        HI_ORIGIN + HI_TOP * HI_CH - 4,
        HI_ORIGIN + (HI_TOP + 3) * HI_CH + 4,
    );
    let (w, h) = (x1 - x0, y1 - y0);
    let cols: Vec<(&Ground, bool)> = grounds
        .iter()
        .flat_map(|&g| [(g, true), (g, false)])
        .collect();
    let rows = 1 + GLINT_AT.len();
    let gap = 4;
    let sheet_w = cols.len() * (w + gap) - gap;
    let mut img = vec![0x0080_8080; sheet_w * rows * (h + gap)];
    let mut panel = vec![0u32; pw * ph];
    for (c, &(g, tall)) in cols.iter().enumerate() {
        let s = hi_scene(g, CardPalette::Cruise, tall);
        for r in 0..rows {
            panel.fill(g.bg);
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            let mut frames = CardFrames::default();
            card.draw(t0, &s, false, &mut frames);
            card.arm(t0, 7);
            let at = r.checked_sub(1).map_or(EPISODE_MS + 1, |k| GLINT_AT[k]);
            let frame = card.draw(t0 + Duration::from_millis(at), &s, true, &mut frames);
            for layer in frame.layers(1, PxRect::EVERYWHERE, (0, 0)) {
                paint(&mut panel, pw, HI_CH, layer);
            }
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

fn kitty_sheet(dark: bool) -> (usize, Vec<u32>) {
    let s = CardScene { dark, ..base() };
    let col = |c: usize| ORIGIN + c * CW;
    let kc = usize::from(kitty_cols(CW as u16, CH as u16));
    let kitty = s.kitty_col.map_or(0, usize::from);
    let (x0, x1) = (col(kitty - 3), col(kitty + kc + 2));
    let (y0, y1) = (ORIGIN + TOP * CH - 4, ORIGIN + (TOP + 3) * CH + 4);
    let (w, h) = (x1 - x0, y1 - y0);
    let times = [At::Settled, At::Ms(BLINK_2_MS), At::Ms(FLICK_2_MS)];
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

fn pack_fill((r, g, b): (f32, f32, f32)) -> u32 {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

struct Ground {
    slug: String,
    bg: u32,
    fg: u32,
    dark: bool,
}

impl Ground {
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

fn hi_panel() -> (usize, usize) {
    let widest = HI_TEXT_COL + HI_LONG_PATH.chars().count() + 2;
    (2 * HI_ORIGIN + widest * HI_CW, 2 * HI_ORIGIN + 6 * HI_CH)
}

fn hi_scene(g: &Ground, palette: CardPalette, tall: bool) -> CardScene {
    let cells = |s: &str| s.chars().count();
    let line = line_of(palette);
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
        wand: wand_at(HI_TOP),
    }
}

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

/// One cell's glyph `ch` at grid cell `(row, col)` of a `w`-wide panel.
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

fn mix_px(a: u32, b: u32, t: f32) -> u32 {
    let ch = |sh: u32| {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        ((x + (y - x) * t).round() as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
}

fn line_colours(
    card: &HarnessCard,
    (now, animate): (Instant, bool),
    len: usize,
    g: &Ground,
) -> Vec<u32> {
    let at = LineAt {
        row: (HI_TOP + 3) as u16,
        col: u16::try_from(HI_TEXT_COL + LINE_LEAD).unwrap_or(0),
        len: u16::try_from(len).unwrap_or(u16::MAX),
        hero: HERO,
        wand: wand_at(HI_TOP),
    };
    let mut ink = Vec::new();
    card.line_ink_into(Some(now), &at, g.bg, animate, &mut ink);
    ink.iter()
        .map(|c| {
            (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2])
        })
        .collect()
}

fn draw_hi(img: &mut [u32], w: usize, fonts: &Fonts, s: &CardScene, g: &Ground, at: At) {
    img.fill(g.bg);
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    let (now, animate, frame) = tick_to(&mut card, t0, s, at);
    let moment = (now, animate);
    let [under, over] = frame.layers(1, PxRect::EVERYWHERE, (0, 0));
    paint(img, w, HI_CH, under);
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
    let line = line_of(s.palette);
    let inks = line_colours(&card, moment, line.chars().count(), g);
    for (i, ch) in line.chars().enumerate() {
        if let Some(&ink) = inks.get(i) {
            let cell = (HI_TOP + 3, HI_TEXT_COL + LINE_LEAD + i);
            let bold = u16::try_from(i).is_ok_and(|i| HERO.contains(&i));
            draw_glyph(img, w, fonts, cell, ch, bold, ink, g.bg);
        }
    }
    let (star, blend) = line_star(inks.first().copied().unwrap_or(g.fg), g.bg);
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
            *px = if blend == aterm_render::DecoBlend::Add {
                aterm_render::add_sat(*px, aterm_render::premul_rgb(star, cov as u8))
            } else {
                let a = (cov * u32::from(STAR_INK_ALPHA) / 255) as u8;
                aterm_render::over_premul(*px, aterm_render::premul_rgb(star, a), a)
            };
        }
    }
    paint(img, w, HI_CH, over);
}

fn paint(img: &mut [u32], w: usize, ch: usize, layer: FxLayer) {
    let h = img.len() / w;
    let frame = aterm_render::FxFrame {
        w,
        h,
        pad: 0,
        grid_top: 0,
        cell_h: ch,
        rows: h / ch + 1,
    };
    aterm_render::fx::paint_layers(img, &frame, &[layer], &[]);
}

const HI_SHOTS: [(CardPalette, bool, At); 8] = [
    (CardPalette::Cruise, true, At::Settled),
    (CardPalette::Cruise, false, At::Settled),
    (CardPalette::Cruise, true, At::Ms(BURST_AT_MS)),
    (CardPalette::Cruise, true, At::Ms(MID_SHIMMER_MS)),
    (CardPalette::Land, true, At::Settled),
    (CardPalette::Land, true, At::Ms(MID_SHIMMER_MS)),
    (CardPalette::Off, true, At::Settled),
    (CardPalette::Off, true, At::Ms(MID_SHIMMER_MS)),
];

const ZOOM_AT: [At; 5] = [
    At::Settled,
    At::Ms(800),
    At::Ms(BURST_AT_MS),
    At::Ms(RIDE_MS + 330),
    At::Ms(MID_SHIMMER_MS),
];

/// Copy the rectangle `(x0, y0, w, h)` of a `pw`-wide panel into a sheet `sw` wide at `(dx, dy)`.
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

fn ground_sheet(g: &Ground, fonts: &Fonts) -> (usize, Vec<u32>, usize, Vec<u32>) {
    let (w, h) = hi_panel();
    let mut img = vec![0u32; w * h * HI_SHOTS.len()];
    for (i, &(palette, tall, at)) in HI_SHOTS.iter().enumerate() {
        let s = hi_scene(g, palette, tall);
        draw_hi(&mut img[i * w * h..(i + 1) * w * h], w, fonts, &s, g, at);
    }
    let s = hi_scene(g, CardPalette::Cruise, true);
    let (t0, _) = s.trail.unwrap_or((0, 0));
    let x0 = HI_ORIGIN + (usize::from(t0) - 1) * HI_CW;
    let x1 = (HI_ORIGIN + (usize::from(s.cols) + 1) * HI_CW).min(w);
    let zw = x1 - x0;
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
    let (y0, y1) = (
        HI_ORIGIN + HI_TOP * HI_CH - HI_CH / 2 - 4,
        HI_ORIGIN + (HI_TOP + 3) * HI_CH + 4,
    );
    let (w, h) = (x1 - x0, y1 - y0);
    let gap = 3;
    let cols = 4;
    let sheet_w = cols * (w + gap) - gap;
    let mut img = vec![0x0080_8080; sheet_w * grounds.len() * (h + gap)];
    let mut panel = vec![0u32; pw * ph];
    for (r, g) in grounds.iter().enumerate() {
        let shots = [
            (CardPalette::Cruise, At::Settled),
            (CardPalette::Cruise, At::Ms(BURST_AT_MS)),
            (CardPalette::Land, At::Settled),
            (CardPalette::Off, At::Settled),
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

fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (
        aterm_effects::color_math::relative_luminance(a),
        aterm_effects::color_math::relative_luminance(b),
    );
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn line_report(grounds: &[Ground]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from(
        "ground\tbg\tpalette\tepisode_min\tat_ms\tletter\tsettled_min\tsettled_max\tsettled letters\n",
    );
    for g in grounds {
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            let s = hi_scene(g, palette, true);
            let line = line_of(palette);
            let letters: Vec<char> = line.chars().collect();
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let mut worst = (f32::MAX, 0u64, 0usize);
            let mut at = 0;
            while at <= EPISODE_MS + GLINT_PERIOD_MS {
                let now = t0 + Duration::from_millis(at);
                card.draw(now, &s, true, &mut CardFrames::default());
                for (i, ink) in line_colours(&card, (now, true), letters.len(), g)
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
            let now = t0 + Duration::from_millis(EPISODE_MS + GLINT_PERIOD_MS);
            card.draw(now, &s, false, &mut CardFrames::default());
            let settled = line_colours(&card, (now, false), letters.len(), g);
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

fn draw_panel(img: &mut [u32], s: &CardScene, at: At) {
    draw_card(img, s, at, false);
}

fn draw_card(img: &mut [u32], s: &CardScene, at: At, shown: bool) {
    let bg = if s.dark { DARK_BG } else { LIGHT_BG };
    let fg = if s.dark { 0x00C0_C4D0 } else { 0x0030_3030 };
    draw_card_on(img, s, at, shown, (bg, fg));
}

fn draw_card_on(img: &mut [u32], s: &CardScene, at: At, shown: bool, (bg, fg): (u32, u32)) {
    let s = &CardScene { ground: bg, ..*s };
    img.fill(bg);
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
        card.draw(t0, s, false, &mut CardFrames::default());
    }
    card.arm(t0, 7);
    let (now, animate, frame) = tick_to(&mut card, t0, s, at);
    for layer in frame.layers(1, PxRect::EVERYWHERE, (0, 0)) {
        paint(img, PANEL_W, CH, layer);
    }
    let line = TOP + 3;
    let at_line = LineAt {
        row: line as u16,
        col: 11,
        len: 50,
        hero: HERO,
        wand: s.wand,
    };
    let mut ink = Vec::new();
    card.line_ink_into(Some(now), &at_line, bg, animate, &mut ink);
    for (i, c) in ink.iter().enumerate() {
        let rgb =
            (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2]);
        let bold = u16::try_from(i).is_ok_and(|i| HERO.contains(&i));
        let (ya, yb) = if bold { (4, 14) } else { (5, 13) };
        let x0 = ORIGIN + usize::from(c.col) * CW;
        for x in x0 + 1..x0 + CW - 1 {
            for y in (ORIGIN + line * CH + ya)..(ORIGIN + line * CH + yb) {
                img[y * PANEL_W + x] = rgb;
            }
        }
    }
}

fn draw_mascot_on(img: &mut [u32], w: usize, (cw, ch): (usize, usize), origin: usize, top: usize) {
    for (r, row) in MASCOT.iter().enumerate() {
        for (c, glyph) in row.chars().enumerate() {
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
