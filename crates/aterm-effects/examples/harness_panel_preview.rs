// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `harness_panel_preview`: pictures of Claude Code's agents panel as the
//! harness wears it (`aterm_effects::harness_panel`,
//! `docs/DESIGN-harness-autopilot-2026-10-15.md` §10.6.13) for visual
//! iteration and the owner's eye — pictures to look at, not checks.
//!
//! Each panel is drawn by the real renderer (`aterm_render::Renderer`, the
//! bundled JetBrains Mono with DejaVu Sans Mono and Noto Sans Symbols 2
//! behind it) from a real engine (`aterm_core::terminal::Terminal`) fed the
//! panel as Claude Code 2.1.304 draws it — the mode row, a blank row, `⏺
//! main` bold in the default ink, each agent's `◯ <name>  <activity>` and
//! its right-aligned `<elapsed> · ↓ <N> tokens` in Claude's panel grey
//! (`#999999` dark, `#666666` light), a workflow's `▰▰▱` bar after a slot
//! row — with the emitter's ink and light laid on the frame as the host
//! lays them (`aterm-gui`'s `claude_panel::make_look`). One row each:
//! running hot (1000 tok/s, level 1), running light (4 tok/s, level 0.18),
//! quiet for 90 s (freshness 0.54), waiting four minutes on its own Bash
//! call inside that call's timeout (in its tool: fresh, level 0, dimmed
//! light), stuck for minutes (freshness 0.10, its breath at its peak),
//! awaiting approval, done (Claude's success green
//! bullet), failed (its error red), and a workflow running at 116 tok/s
//! (level 0.66) crested mid-bar. Each row's activity text names its state.
//! Written to `<out_dir>`:
//!
//! * `panel_dark.png`, `panel_light.png`, `panel_linen.png`: Cruise, Land
//!   and Off on Aterm Night, on Aterm Day, then on Linen.
//! * `panel_serious.png`: Cruise in serious mode, dark then light (the state
//!   ink kept, every light dropped).
//! * `dots_zoom.png` (4×): the dots and names of Cruise dark and Cruise
//!   light, cropped.
//! * `breath.png` (4×): the stuck row alone at its breath's floor, halfway
//!   and at its peak, dark then light.
//! * `crest.png` (3×): the workflow row alone at six moments two ripple
//!   steps (256 ms) apart, the crest passing along the filled cells.
//!
//! ```text
//! targo --unverified run -p aterm-effects --example harness_panel_preview -- [out_dir]
//! (default out_dir: target/harness_panel_preview)
//! ```

use std::path::Path;
use std::process::ExitCode;

use aterm_core::render::InkCell;
use aterm_core::terminal::Terminal;
use aterm_effects::harness_card::CardPalette;
use aterm_effects::harness_panel::{self as glass, BREATH_PERIOD_MS, DotMood, DotPlace, PanelMark};
use aterm_effects::ripple::FLOW_STEP_MS;
use aterm_render::{Renderer, Theme};
use aterm_types::scheme::builtin;

/// The pane the panel is drawn in.
const COLS: usize = 104;
const ROWS: usize = 14;
/// The renderer's padding, every side.
const PAD: usize = 8;
/// The bar's crest moment: five ripple steps in, its front mid-bar.
const CREST_MS: u64 = 5 * FLOW_STEP_MS;

/// Claude's panel grey, dark then light (2.1.304).
const GREY: [[u8; 3]; 2] = [[0x99, 0x99, 0x99], [0x66, 0x66, 0x66]];

/// One agent row of the panel: its name, activity, right-hand meta, what
/// the harness's fold says of it, and the status colour Claude marked its
/// bullet in, dark then light (done, failed), if any.
struct Agent {
    name: &'static str,
    activity: &'static str,
    meta: &'static str,
    mood: DotMood,
    level: f32,
    sat: f32,
    done: Option<[u32; 2]>,
}

const AGENTS: [Agent; 8] = [
    Agent {
        name: "general-purpose",
        activity: "running hot (1000 tok/s)",
        meta: "21s \u{B7} \u{2193} 6.2k tokens",
        mood: DotMood::Running,
        level: 1.0,
        sat: 1.0,
        done: None,
    },
    Agent {
        name: "Explore",
        activity: "running light (4 tok/s)",
        meta: "1m 3s \u{B7} \u{2193} 18.4k tokens",
        mood: DotMood::Running,
        level: 0.18,
        sat: 1.0,
        done: None,
    },
    Agent {
        name: "code-reviewer",
        activity: "quiet for 90 s",
        meta: "2m 10s \u{B7} \u{2193} 3.1k tokens",
        mood: DotMood::Quiet,
        level: 0.05,
        sat: 0.54,
        done: None,
    },
    Agent {
        name: "builder",
        activity: "in Bash 4m12s (its 400 s timeout)",
        meta: "4m 33s \u{B7} \u{2193} 6.2k tokens",
        mood: DotMood::Tool,
        level: 0.0,
        sat: 1.0,
        done: None,
    },
    Agent {
        name: "test-runner",
        activity: "stuck: no work for 3 min",
        meta: "7m 41s \u{B7} \u{2193} 9.9k tokens",
        mood: DotMood::Stuck,
        level: 0.0,
        sat: 0.10,
        done: None,
    },
    Agent {
        name: "planner",
        activity: "plan ready",
        meta: "awaiting approval",
        mood: DotMood::Awaiting,
        level: 0.0,
        sat: 1.0,
        done: None,
    },
    Agent {
        name: "docs-writer",
        activity: "done",
        meta: "4m 2s \u{B7} \u{2193} 12.0k tokens",
        mood: DotMood::Still,
        level: 0.0,
        sat: 1.0,
        done: Some(glass::CLAUDE_SUCCESS),
    },
    Agent {
        name: "migrator",
        activity: "failed",
        meta: "52s \u{B7} \u{2193} 2.4k tokens",
        mood: DotMood::Still,
        level: 0.0,
        sat: 1.0,
        done: Some(glass::CLAUDE_ERROR),
    },
];

/// The workflow row: its name, bar (filled of cells), meta and work.
const FLOW_NAME: &str = "sweep";
const FLOW_FILLED: usize = 9;
const FLOW_CELLS: usize = 12;
const FLOW_META: &str = "3/4 \u{B7} 12m 5s \u{B7} \u{2193} 88.1k tokens";
const FLOW_LEVEL: f32 = 0.66;

/// The panel's rows on the pane: a label row, the mode row, a blank, main,
/// the agents, the slot row, the workflow.
const MODE_ROW: usize = 1;
const MAIN_ROW: usize = 3;
const FIRST_AGENT: usize = 4;
const FLOW_ROW: usize = FIRST_AGENT + AGENTS.len() + 1;
/// The bullet's and the name's columns.
const BULLET: usize = 2;
const NAME: usize = 4;

/// What one picture is drawn with.
#[derive(Clone, Copy)]
struct Scene {
    palette: CardPalette,
    /// The built-in theme whose ink and ground the pane wears.
    theme: &'static str,
    dark: bool,
    serious: bool,
    breath: f32,
    crest_ms: u64,
    /// Only this pane row of the panel drawn (a close-up's), or all.
    solo: Option<usize>,
}

impl Scene {
    fn shows(&self, row: usize) -> bool {
        self.solo.is_none_or(|s| s == row)
    }
}

impl Scene {
    fn of(palette: CardPalette, dark: bool) -> Self {
        Self::on(
            palette,
            if dark { "Aterm Night" } else { "Aterm Day" },
            dark,
        )
    }

    fn on(palette: CardPalette, theme: &'static str, dark: bool) -> Self {
        Self {
            palette,
            theme,
            dark,
            serious: false,
            breath: 1.0,
            crest_ms: CREST_MS,
            solo: None,
        }
    }
}

/// A rendered frame: `w` pixels a row.
struct Picture {
    w: usize,
    px: Vec<u32>,
}

fn main() -> ExitCode {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/harness_panel_preview".to_string());
    let out = Path::new(&out);
    if let Err(e) = std::fs::create_dir_all(out) {
        eprintln!("harness_panel_preview: {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    let (Some(night), Some(day), Some(linen)) = (
        builtin("Aterm Night"),
        builtin("Aterm Day"),
        builtin("Linen"),
    ) else {
        eprintln!("harness_panel_preview: Aterm Night, Aterm Day and Linen are built in");
        return ExitCode::FAILURE;
    };
    let mut dark = match renderer(Theme::from(&night)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("harness_panel_preview: the bundled fonts: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut light = match renderer(Theme::from(&day)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("harness_panel_preview: the bundled fonts: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut paper = match renderer(Theme::from(&linen)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("harness_panel_preview: the bundled fonts: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (cw, ch) = dark.cell_size();
    let palettes = [
        (CardPalette::Cruise, "Cruise"),
        (CardPalette::Land, "Land"),
        (CardPalette::Off, "Off"),
    ];
    let mut draw = |scene: Scene, label: &str| {
        let rend = match scene.theme {
            "Linen" => &mut paper,
            _ if scene.dark => &mut dark,
            _ => &mut light,
        };
        panel(rend, scene, label)
    };
    let dark_sheet: Vec<Picture> = palettes
        .iter()
        .map(|&(p, name)| draw(Scene::of(p, true), &format!("{name} \u{B7} Aterm Night")))
        .collect();
    let light_sheet: Vec<Picture> = palettes
        .iter()
        .map(|&(p, name)| draw(Scene::of(p, false), &format!("{name} \u{B7} Aterm Day")))
        .collect();
    let linen_sheet: Vec<Picture> = palettes
        .iter()
        .map(|&(p, name)| {
            draw(
                Scene::on(p, "Linen", false),
                &format!("{name} \u{B7} Linen"),
            )
        })
        .collect();
    let serious: Vec<Picture> = [true, false]
        .iter()
        .map(|&d| {
            let scene = Scene {
                serious: true,
                ..Scene::of(CardPalette::Cruise, d)
            };
            draw(scene, "Cruise \u{B7} serious mode")
        })
        .collect();
    let dot_cols = (0, NAME + 16);
    let agent_rows = (MAIN_ROW, FLOW_ROW + 1);
    let zoom: Vec<Picture> = [&dark_sheet[0], &light_sheet[0]]
        .iter()
        .map(|p| crop(p, (cw, ch), dot_cols, agent_rows))
        .collect();
    let stuck = FIRST_AGENT
        + AGENTS
            .iter()
            .position(|a| a.mood == DotMood::Stuck)
            .expect("a stuck row");
    let mut breaths = Vec::new();
    for d in [true, false] {
        for ms in [0, BREATH_PERIOD_MS / 4, BREATH_PERIOD_MS / 2] {
            let scene = Scene {
                breath: glass::breath(Some(ms)),
                solo: Some(stuck),
                ..Scene::of(CardPalette::Cruise, d)
            };
            let p = draw(scene, "breath");
            breaths.push(crop(&p, (cw, ch), (0, NAME + 12), (stuck, stuck + 1)));
        }
    }
    let mut crests = Vec::new();
    for step in 0..6u64 {
        let scene = Scene {
            crest_ms: step * 2 * FLOW_STEP_MS,
            solo: Some(FLOW_ROW),
            ..Scene::of(CardPalette::Cruise, true)
        };
        let p = draw(scene, "crest");
        crests.push(crop(
            &p,
            (cw, ch),
            (0, NAME + FLOW_NAME.len() + 2 + FLOW_CELLS + 1),
            (FLOW_ROW, FLOW_ROW + 1),
        ));
    }
    let pictures = [
        ("panel_dark.png", stack(&dark_sheet), 1),
        ("panel_light.png", stack(&light_sheet), 1),
        ("panel_linen.png", stack(&linen_sheet), 1),
        ("panel_serious.png", stack(&serious), 1),
        ("dots_zoom.png", stack(&zoom), 4),
        ("breath.png", stack(&breaths), 4),
        ("crest.png", stack(&crests), 3),
    ];
    for (name, pic, scale) in &pictures {
        let path = out.join(name);
        if let Err(e) = write_png(&path, pic, *scale) {
            eprintln!("harness_panel_preview: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("{}", path.display());
    }
    ExitCode::SUCCESS
}

/// The bundled face with the bundled fallbacks behind it, padded.
fn renderer(theme: Theme) -> Result<Renderer, String> {
    let mut rend = Renderer::from_bytes(
        include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf"),
        18.0,
        theme,
    )?;
    rend.set_fallback_bytes(include_bytes!(
        "../../aterm-render/assets/DejaVuSansMono.ttf"
    ))?;
    rend.set_symbol_fallback_bytes(include_bytes!(
        "../../aterm-render/assets/bundled/NotoSansSymbols2-Regular.ttf"
    ))?;
    rend.set_pad(PAD);
    Ok(rend)
}

/// `\x1b[38;2;r;g;bm`.
fn sgr(rgb: [u8; 3]) -> String {
    format!("\x1b[38;2;{};{};{}m", rgb[0], rgb[1], rgb[2])
}

fn pack(rgb: [u8; 3]) -> u32 {
    (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
}

fn unpack(rgb: u32) -> [u8; 3] {
    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]
}

/// Text `s` at `(row, col)` in `ink`.
fn at(row: usize, col: usize, ink: &str, s: &str) -> String {
    format!("\x1b[{};{}H{ink}{s}\x1b[0m", row + 1, col + 1)
}

/// Right-aligned meta, ending two cells short of the pane's edge.
fn meta_col(meta: &str) -> usize {
    COLS - 2 - meta.chars().count()
}

/// One panel, as Claude draws it and the harness wears it.
fn panel(rend: &mut Renderer, scene: Scene, label: &str) -> Picture {
    let li = usize::from(!scene.dark);
    let grey = GREY[li];
    let g = sgr(grey);
    let theme = Theme::from(&builtin(scene.theme).expect("a built-in theme"));
    // The engine's default ink and ground are the theme's, as a pane's are.
    let mut screen = format!(
        "\x1b]10;#{:06x}\x07\x1b]11;#{:06x}\x07\x1b[?25l\x1b[2J",
        theme.fg, theme.bg
    );
    if scene.solo.is_none() {
        screen += &at(
            0,
            0,
            "",
            &format!("\u{2500}\u{2500} {label} \u{2500}\u{2500}"),
        );
        screen += &at(
            MODE_ROW,
            2,
            &g,
            "\u{23F8} manual mode on \u{B7} ? for shortcuts \u{B7} \u{2193} to manage",
        );
        screen += &at(MAIN_ROW, BULLET, "\x1b[1m", "\u{23FA} main");
    }
    for (i, a) in AGENTS.iter().enumerate() {
        let row = FIRST_AGENT + i;
        if !scene.shows(row) {
            continue;
        }
        let bullet = a.done.map_or_else(|| g.clone(), |ink| sgr(unpack(ink[li])));
        screen += &at(row, BULLET, &bullet, "\u{25EF}");
        screen += &at(row, NAME, &g, &format!("{}  {}", a.name, a.activity));
        screen += &at(row, meta_col(a.meta), &g, a.meta);
    }
    screen += &at(FLOW_ROW - 1, BULLET, "", " ");
    let bar: String = "\u{25B0}".repeat(FLOW_FILLED) + &"\u{25B1}".repeat(FLOW_CELLS - FLOW_FILLED);
    if scene.shows(FLOW_ROW) {
        screen += &at(FLOW_ROW, BULLET, &g, "\u{25EF}");
        screen += &at(
            FLOW_ROW,
            NAME,
            &g,
            &format!("{FLOW_NAME}  {bar}  {FLOW_META}"),
        );
    }
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(screen.as_bytes());
    let mut input = term.cell_frame(ROWS, COLS);
    lay(
        &mut input,
        rend.cell_size(),
        scene,
        grey,
        (theme.fg, theme.bg),
    );
    let frame = rend.render_input(&input);
    Picture {
        w: frame.width,
        px: frame.pixels.iter().map(|p| p & 0x00FF_FFFF).collect(),
    }
}

/// The emitter's ink and light on `input`, as the host lays them.
fn lay(
    input: &mut aterm_core::render::RenderInput,
    (cw, ch): (usize, usize),
    scene: Scene,
    grey: [u8; 3],
    (fg, bg): (u32, u32),
) {
    let Scene {
        palette,
        dark,
        serious,
        breath,
        crest_ms,
        theme: _,
        solo: _,
    } = scene;
    let mut ink = Vec::new();
    let clip = (PAD as u16, (PAD + ROWS * ch) as u16);
    let mut put = |row: usize, col: usize, rgb: u32| {
        ink.push(InkCell {
            row: row as u16,
            col: col as u16,
            color: unpack(rgb),
        });
    };
    let name = |put: &mut dyn FnMut(usize, usize, u32), row, k, len, cell_fg, sat| {
        let row_t = glass::row_hue(palette, k, dark);
        for j in 0..len {
            let x = glass::name_x(row_t, j, len);
            put(
                row,
                NAME + j,
                glass::panel_ink(palette, PanelMark::Name, x, dark, cell_fg, sat, 0.0),
            );
        }
    };
    // Main: its name along its stripe at Claude's (bold default) lightness.
    if scene.solo.is_none() {
        name(&mut put, MAIN_ROW, 0, 4, fg, 1.0);
    }
    let mut dots = Vec::new();
    for (i, a) in AGENTS.iter().enumerate() {
        let (row, k) = (FIRST_AGENT + i, 1 + i);
        if !scene.shows(row) {
            continue;
        }
        if a.done.is_none() {
            name(&mut put, row, k, a.name.chars().count(), pack(grey), a.sat);
        }
        let mood = if a.done.is_some() {
            DotMood::Still
        } else {
            a.mood
        };
        dots.push((row, k, mood, a.level, a.sat));
    }
    let k = 1 + AGENTS.len();
    let bar0 = NAME + FLOW_NAME.len() + 2;
    if scene.shows(FLOW_ROW) {
        name(&mut put, FLOW_ROW, k, FLOW_NAME.len(), pack(grey), 1.0);
        dots.push((FLOW_ROW, k, DotMood::Running, FLOW_LEVEL, 1.0));
    }
    for i in (0..FLOW_CELLS).filter(|_| scene.shows(FLOW_ROW)) {
        let x = glass::bar_x(i, FLOW_CELLS);
        let rgb = if i < FLOW_FILLED {
            let crest = glass::crest(i as f32, crest_ms);
            glass::panel_ink(palette, PanelMark::BarFill, x, dark, pack(grey), 1.0, crest)
        } else {
            glass::panel_ink(palette, PanelMark::BarEmpty, x, dark, pack(grey), 1.0, 0.0)
        };
        put(FLOW_ROW, bar0 + i, rgb);
    }
    for (row, k, mood, level, sat) in dots {
        let look = glass::dot_look(mood, level, sat, palette, k, dark, bg, breath, serious);
        if let Some(rgb) = look.ink {
            put(row, BULLET, rgb);
        }
        if let Some(light) = look.light {
            let place = DotPlace {
                cx: (PAD + BULLET * cw + cw / 2) as u16,
                cy: (PAD + row * ch + ch / 2) as u16,
                cw: cw as u16,
                ch: ch as u16,
                row: row as u16,
                clip,
            };
            glass::dot_light_quads(&light, dark, place, &mut input.glow_halo);
        }
    }
    ink.sort_unstable_by_key(|c| (c.row, c.col));
    ink.dedup_by_key(|c| (c.row, c.col));
    input.ink = ink;
}

/// Columns `cols` and rows `rows` of `p`'s grid (half-open), with half a
/// cell of air round them.
fn crop(
    p: &Picture,
    (cw, ch): (usize, usize),
    cols: (usize, usize),
    rows: (usize, usize),
) -> Picture {
    let h = p.px.len() / p.w;
    let x0 = (PAD + cols.0 * cw).saturating_sub(cw / 2);
    let x1 = (PAD + cols.1 * cw + cw / 2).min(p.w);
    let y0 = (PAD + rows.0 * ch).saturating_sub(ch / 2);
    let y1 = (PAD + rows.1 * ch + ch / 2).min(h);
    let w = x1 - x0;
    let mut px = Vec::with_capacity(w * (y1 - y0));
    for y in y0..y1 {
        px.extend_from_slice(&p.px[y * p.w + x0..y * p.w + x1]);
    }
    Picture { w, px }
}

/// `pics` one above the next, a 4-pixel mid-grey rule between, all as wide
/// as the widest.
fn stack(pics: &[Picture]) -> Picture {
    let w = pics.iter().map(|p| p.w).max().unwrap_or(1);
    let mut px = Vec::new();
    for (i, p) in pics.iter().enumerate() {
        if i > 0 {
            px.extend(std::iter::repeat_n(0x0080_8080, w * 4));
        }
        for row in p.px.chunks(p.w) {
            px.extend_from_slice(row);
            px.extend(std::iter::repeat_n(0x0080_8080, w - p.w));
        }
    }
    Picture { w, px }
}

/// Write `p` at `scale`× as an RGB PNG.
fn write_png(path: &Path, p: &Picture, scale: usize) -> std::io::Result<()> {
    let (w, h) = (p.w * scale, p.px.len() / p.w * scale);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let v = p.px[(y / scale) * p.w + x / scale];
            rgb.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
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
