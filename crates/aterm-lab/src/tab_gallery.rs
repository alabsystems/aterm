// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `tab-gallery`: the tab strip as a person sees it, per theme, with its application tiles measured from the pixels.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::instance::{self, Instance, Launch, Reply, World, take_flag, take_value};
use crate::stop;
use crate::system_state_sheet::{self as sheet, Tile};
use crate::trail_contrast::{Image, PxBox, contrast, hex, mode, most_contrasting};
use crate::{Fail, Res};

/// The verb's own help (`tab-gallery --help`).
pub const HELP: &str = "\
usage: targo --unverified lab tab-gallery [--aterm PATH] [--themes NAME,...|all] [--quick] <out_dir>

One windowed instance per theme, one at a time, ended by PID: a lone System State tab, then two terminal
tabs beside it and Settings, About, Mail, Editor and Markdown, each made the selected tab and captured with
`window`, then a narrow strip, then a headless `tab_strip_rows = 1` frame of the cell strip.
Writes <theme>.<state>.png, <theme>.sheet.png and report.txt, and measures from the pixels and `chrome`'s
tab-frame lines: a selected tile against its chip and a glyph against its tile (3:1), a quiet tile against
its ground (1.30:1), selected against quiet. The Windows and Linux raster band is not reachable here.

exit: 0; 1 a check failed or a frame could not be taken; 2 a usage error
";

/// Custom themes with nothing to lend: grey throughout, then an accent all but lost in its ground. `(name, appearance, bg, fg, cursor, ansi)`.
const CUSTOM: [(&str, &str, &str, &str, &str, &str); 3] = [
    (
        "Lab Mono Dark",
        "dark",
        "#161616",
        "#c8c8c8",
        "#a0a0a0",
        "#909090",
    ),
    (
        "Lab Mono Light",
        "light",
        "#f4f4f4",
        "#333333",
        "#666666",
        "#707070",
    ),
    (
        "Lab Low Accent",
        "dark",
        "#20242c",
        "#b8c0cc",
        "#262c3a",
        "#262c3a",
    ),
];

/// A theme file of [`CUSTOM`].
fn custom_theme_text(theme: &(&str, &str, &str, &str, &str, &str)) -> String {
    let (name, appearance, bg, fg, cursor, ansi) = theme;
    let mut text = format!(
        "name = {name}\nappearance = {appearance}\nforeground = {fg}\nbackground = {bg}\ncursor = {cursor}\nselection = {ansi}\nselection_foreground = {fg}\n"
    );
    for slot in 0..16 {
        text.push_str(&format!("color{slot} = {ansi}\n"));
    }
    text
}

const QUICK: [&str; 6] = [
    "Default",
    "Dracula",
    "Solarized Dark",
    "Solarized Light",
    "GitHub Light",
    "Lab Mono Dark",
];
const ALL: [&str; 20] = [
    "Default",
    "Aterm Night",
    "Hearth",
    "Dracula",
    "Nord",
    "Tokyo Night",
    "Catppuccin Mocha",
    "Gruvbox Dark",
    "Solarized Dark",
    "One Dark",
    "Aterm Day",
    "Linen",
    "Default Light",
    "Solarized Light",
    "Gruvbox Light",
    "Catppuccin Latte",
    "GitHub Light",
    CUSTOM[0].0,
    CUSTOM[1].0,
    CUSTOM[2].0,
];

/// The window the strip is judged at (backing pixels, as `resize px` takes them).
const WINDOW: (u32, u32) = (3000, 800);
const NARROW: (u32, u32) = (1500, 600);
const SETTLE: Duration = Duration::from_millis(600);
/// The tabs of the wide window, in order: is each an application? (System State, two terminals, then the rest.)
const WIDE_APPS: [bool; 8] = [true, false, false, true, true, true, true, true];
const FLOOR_SOLID: f64 = 3.0;
const FLOOR_QUIET: f64 = 1.30;
const FLOOR_APART: f64 = 1.5;

/// One `tab-frame` line of `chrome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chip {
    pub app: bool,
    pub selected: bool,
    pub frame: PxBox,
    pub icon: Option<PxBox>,
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

const fn px_box(x: i64, y: i64, w: i64, h: i64) -> PxBox {
    PxBox {
        x0: x,
        y0: y,
        x1: x + w,
        y1: y + h,
    }
}

/// Every chip `chrome` names, in tab order.
#[must_use]
pub fn chips(chrome: &str) -> Vec<Chip> {
    chrome
        .lines()
        .filter(|line| line.starts_with("tab-frame "))
        .filter_map(|line| {
            let num = |k: &str| field(line, k)?.parse::<i64>().ok();
            let icon = field(line, "icon").and_then(|text| {
                let v: Vec<i64> = text.split(',').filter_map(|n| n.parse().ok()).collect();
                (v.len() == 4).then(|| px_box(v[0], v[1], v[2], v[3]))
            });
            Some(Chip {
                app: field(line, "surface")? == "app",
                selected: field(line, "selected")? == "true",
                frame: px_box(num("x")?, num("y")?, num("w")?, num("h")?),
                icon,
            })
        })
        .collect()
}

/// What one tile's pixels measure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileReading {
    pub ground: [u8; 3],
    pub fill: [u8; 3],
    pub glyph: [u8; 3],
}

/// Read a chip's tile: the fill is the commonest colour inside it, the glyph what stands furthest from that fill, and the ground the commonest colour of the chip's mid-height to the tile's trailing side, title strokes being far the fewer.
/// # Errors
/// The chip has no icon, or its boxes hold no pixels.
pub fn read_tile(frame: &Image, chip: &Chip) -> Result<TileReading, String> {
    let icon = chip.icon.ok_or("no tile laid out")?;
    let inset = (icon.x1 - icon.x0) / 7 + 1;
    // Clear of the rounded corners: the fill and the glyph, no ground.
    let inner = PxBox {
        x0: icon.x0 + inset,
        y0: icon.y0 + inset,
        x1: icon.x1 - inset,
        y1: icon.y1 - inset,
    };
    let px = frame.region(inner);
    let fill = mode(px.iter().copied()).ok_or("the tile holds no pixels")?;
    let glyph = most_contrasting(&px, fill).ok_or("the tile holds no pixels")?;
    // Between the pill's edge and the title's cap height there is only ground on both sides of the strokes.
    let side = PxBox {
        x0: icon.x1 + (icon.x1 - icon.x0),
        y0: icon.y0 - 1,
        x1: chip.frame.x1 - (icon.x1 - icon.x0),
        y1: icon.y1 + 1,
    };
    let ground = mode(frame.region(side)).ok_or("the chip holds no pixels")?;
    Ok(TileReading {
        ground,
        fill,
        glyph,
    })
}

/// The wide window must show [`WIDE_APPS`], each application with a tile and each terminal without one: a run that measured no tile has judged nothing.
/// # Errors
/// What differs.
pub fn wide_layout_holds(chips: &[Chip]) -> Result<(), String> {
    if chips.len() != WIDE_APPS.len() {
        return Err(format!("{} chips, not {}", chips.len(), WIDE_APPS.len()));
    }
    for (index, (chip, app)) in chips.iter().zip(WIDE_APPS).enumerate() {
        if chip.app != app || chip.icon.is_some() != app {
            return Err(format!("chip {index}: wanted app={app}, read {chip:?}"));
        }
    }
    Ok(())
}

/// The verdicts of one reading: `(word, passed)`.
#[must_use]
pub fn judge(reading: &TileReading, selected: bool) -> Vec<(String, bool)> {
    let tile = contrast(reading.fill, reading.ground);
    let glyph = contrast(reading.glyph, reading.fill);
    let floor = if selected { FLOOR_SOLID } else { FLOOR_QUIET };
    vec![
        (format!("tile {tile:.2}"), tile >= floor - 0.05),
        (format!("glyph {glyph:.2}"), glyph >= FLOOR_SOLID - 0.15),
    ]
}

fn slug(name: &str) -> String {
    name.to_lowercase().replace(' ', "-")
}

fn ok(reply: &Reply, what: &str) -> Result<(), String> {
    if reply.code == 0 {
        Ok(())
    } else {
        Err(format!(
            "{what}: {}",
            crate::trail_gallery::reply_text(reply).trim_end()
        ))
    }
}

struct Run {
    aterm: PathBuf,
    out: PathBuf,
    report: String,
    failed: bool,
}

impl Run {
    fn line(&mut self, text: &str) {
        println!("{text}");
        self.report.push_str(text);
        self.report.push('\n');
    }

    fn start(
        &self,
        theme: &str,
        headless: bool,
        extra: &[String],
    ) -> Result<(World, Instance), String> {
        let mut top = vec![format!("theme = {theme:?}")];
        top.extend_from_slice(extra);
        let world = World::new(&instance::config_with(&top))?;
        let themes = world.cfg.join("aterm/themes");
        std::fs::create_dir_all(&themes).map_err(|e| e.to_string())?;
        for custom in &CUSTOM {
            std::fs::write(
                themes.join(format!("{}.conf", custom.0)),
                custom_theme_text(custom),
            )
            .map_err(|e| e.to_string())?;
        }
        let inst = Instance::start(
            &world,
            Launch {
                aterm: self.aterm.clone(),
                headless,
                args: Vec::new(),
                env: world.env(),
                cwd: world.home.clone(),
                sandbox: None,
                reroute: false,
            },
        )?;
        Ok((world, inst))
    }

    /// Capture the whole window; report and return its strip crop and chips.
    fn shot(&mut self, inst: &Instance, name: &str) -> Result<(Image, Vec<Chip>), String> {
        stop::sleep(SETTLE).map_err(|s| format!("stopped by signal {}", s.0))?;
        let path = self.out.join(format!("{name}.png"));
        let bare = format!("{name}.png");
        let shot = inst.ctl(&["window", &bare]);
        ok(&shot, "window")?;
        let written = shot
            .stdout
            .split_whitespace()
            .nth(3)
            .ok_or("window named no file")?;
        std::fs::copy(written, &path).map_err(|e| format!("{written}: {e}"))?;
        let chrome = inst.ctl(&["chrome"]);
        ok(&chrome, "chrome")?;
        std::fs::write(self.out.join(format!("{name}.chrome.txt")), &chrome.stdout)
            .map_err(|e| e.to_string())?;
        let frame = Image::read(&path)?;
        let chips = chips(&chrome.stdout);
        let bottom = chips
            .iter()
            .map(|c| c.frame.y1)
            .max()
            .ok_or("no tab-frame line")?;
        let bottom = bottom.clamp(1, i64::try_from(frame.height).unwrap_or(1));
        let strip = frame.crop(PxBox {
            x0: 0,
            y0: 0,
            x1: i64::try_from(frame.width).map_err(|e| e.to_string())?,
            y1: bottom,
        })?;
        Ok((strip, self.measured(name, &frame, chips)?))
    }

    fn measured(
        &mut self,
        name: &str,
        frame: &Image,
        chips: Vec<Chip>,
    ) -> Result<Vec<Chip>, String> {
        let mut selected = None;
        let mut quiet = None;
        for (i, chip) in chips.iter().enumerate() {
            if !chip.app || chip.icon.is_none() {
                continue;
            }
            let reading = read_tile(frame, chip)?;
            let verdicts = judge(&reading, chip.selected);
            let state = if chip.selected { "selected" } else { "quiet" };
            let words: Vec<String> = verdicts
                .iter()
                .map(|(word, pass)| format!("{word}{}", if *pass { "" } else { " FAIL" }))
                .collect();
            self.failed |= verdicts.iter().any(|(_, pass)| !pass);
            self.line(&format!(
                "{name} chip={i} {state} ground={} fill={} glyph={} {}",
                hex(reading.ground),
                hex(reading.fill),
                hex(reading.glyph),
                words.join(" ")
            ));
            if chip.selected {
                selected = Some(reading.fill);
            } else {
                quiet = Some(reading.fill);
            }
        }
        if let (Some(a), Some(b)) = (selected, quiet) {
            let apart = contrast(a, b);
            let pass = apart >= FLOOR_APART;
            self.failed |= !pass;
            self.line(&format!(
                "{name} selected-vs-quiet {apart:.2}{}",
                if pass { "" } else { " FAIL" }
            ));
        }
        Ok(chips)
    }

    fn theme(&mut self, theme: &str, scratch: &Path) -> Result<(), String> {
        let stem = slug(theme);
        let mut tiles = Vec::new();
        let (world, mut inst) = self.start(theme, false, &[])?;
        let taken = self.windowed(theme, &stem, &inst, scratch, &mut tiles);
        inst.stop();
        drop(world);
        taken?;
        let (world, mut inst) = self.start(theme, true, &["tab_strip_rows = 1".to_string()])?;
        let cells = self.cells(&stem, &inst, &mut tiles);
        inst.stop();
        drop(world);
        cells?;
        sheet::write(
            &self.out.join(format!("{stem}.sheet.png")),
            theme,
            "the strip, each app tab selected in turn",
            &tiles,
        )
    }

    /// Refuse a frame of the wrong theme: a name that misses draws Default without complaint.
    fn drawn(&mut self, inst: &Instance, theme: &str) -> Result<(), String> {
        let status = inst.ctl(&["theme", "status"]);
        ok(&status, "theme status")?;
        let line = status.first();
        if field(&line, "miss") != Some("-") || !line.contains(&format!("label={theme:?}")) {
            return Err(format!("{theme} is not what is drawn: {line}"));
        }
        self.line(&format!(
            "{theme} drawn: {}",
            field(&line, "bg").unwrap_or("?")
        ));
        Ok(())
    }

    fn windowed(
        &mut self,
        theme: &str,
        stem: &str,
        inst: &Instance,
        scratch: &Path,
        tiles: &mut Vec<Tile>,
    ) -> Result<(), String> {
        await_up(inst)?;
        self.drawn(inst, theme)?;
        let px = |(w, h): (u32, u32)| (w.to_string(), h.to_string());
        let (w, h) = px(WINDOW);
        ok(
            &inst.ctl(&["open", "app", "system-state"]),
            "open system-state",
        )?;
        ok(&inst.ctl(&["tab", "close", "0"]), "close the terminal")?;
        ok(&inst.ctl(&["resize", "px", &w, &h]), "resize")?;
        let (strip, chips) = self.shot(inst, &format!("{stem}.lone"))?;
        if !matches!(
            chips.as_slice(),
            [Chip {
                app: true,
                icon: Some(_),
                ..
            }]
        ) {
            return Err(format!("the lone window is not one app tile: {chips:?}"));
        }
        tiles.push(Tile {
            frame: strip,
            caption: format!("{theme}: lone System State"),
            verdicts: Vec::new(),
        });
        let note = scratch.join(format!("{stem}.md"));
        std::fs::write(&note, "# note\n").map_err(|e| e.to_string())?;
        let code = scratch.join(format!("{stem}.rs"));
        std::fs::write(&code, "fn main() {}\n").map_err(|e| e.to_string())?;
        let uri = |p: &Path| {
            std::fs::canonicalize(p).map_or_else(
                |_| format!("file://{}", p.display()),
                |real| format!("file://{}", real.display()),
            )
        };
        for _ in 0..2 {
            ok(&inst.ctl(&["tab", "new"]), "tab new")?;
        }
        for args in [
            vec!["open", "app", "settings"],
            vec!["open", "app", "about"],
            vec!["open", "app", "mail"],
        ] {
            ok(&inst.ctl(&args), args[2])?;
        }
        let (note, code) = (uri(&note), uri(&code));
        ok(&inst.ctl(&["open", "app", "editor", &code]), "open editor")?;
        ok(
            &inst.ctl(&["open", "app", "markdown", &note]),
            "open markdown",
        )?;
        for index in 0..WIDE_APPS.len() {
            let n = index.to_string();
            ok(&inst.ctl(&["tab", &n]), "tab")?;
            let (strip, chips) = self.shot(inst, &format!("{stem}.tab{index}"))?;
            if let Err(why) = wide_layout_holds(&chips) {
                self.failed = true;
                self.line(&format!("{stem}.tab{index} FAIL {why}"));
            }
            if chips.get(index).is_some_and(|c| c.selected) {
                tiles.push(Tile {
                    frame: strip,
                    caption: format!("{theme}: tab {index} selected"),
                    verdicts: Vec::new(),
                });
            } else {
                self.failed = true;
                self.line(&format!("{stem}.tab{index} the wanted tab is not selected"));
            }
        }
        let (w, h) = px(NARROW);
        ok(&inst.ctl(&["resize", "px", &w, &h]), "resize narrow")?;
        let (strip, _) = self.shot(inst, &format!("{stem}.narrow"))?;
        tiles.push(Tile {
            frame: strip,
            caption: format!("{theme}: narrow strip"),
            verdicts: Vec::new(),
        });
        Ok(())
    }

    /// The cell strip: `lab instance --image`'s own capture, the lane macOS's windows do not show.
    fn cells(&mut self, stem: &str, inst: &Instance, tiles: &mut Vec<Tile>) -> Result<(), String> {
        await_up(inst)?;
        ok(
            &inst.ctl(&["open", "app", "system-state"]),
            "open system-state",
        )?;
        ok(&inst.ctl(&["tab", "new"]), "tab new")?;
        stop::sleep(SETTLE).map_err(|s| format!("stopped by signal {}", s.0))?;
        let path = self.out.join(format!("{stem}.cells.png"));
        let shot = inst.ctl(&["image", "frame.png"]);
        ok(&shot, "image")?;
        let from = shot
            .stdout
            .split_whitespace()
            .nth(3)
            .ok_or("image named no file")?;
        std::fs::copy(from, &path).map_err(|e| e.to_string())?;
        let frame = Image::read(&path)?;
        let strip = frame.crop(PxBox {
            x0: 0,
            y0: 0,
            x1: i64::try_from(frame.width).map_err(|e| e.to_string())?,
            y1: 90.min(i64::try_from(frame.height).map_err(|e| e.to_string())?),
        })?;
        tiles.push(Tile {
            frame: strip,
            caption: "cell strip".to_string(),
            verdicts: Vec::new(),
        });
        Ok(())
    }
}

fn await_up(inst: &Instance) -> Result<(), String> {
    let end = Instant::now() + instance::BOOT;
    while Instant::now() < end {
        if inst.ctl(&["ready"]).code == 0 {
            return Ok(());
        }
        stop::sleep(Duration::from_millis(200))
            .map_err(|s| format!("stopped by signal {}", s.0))?;
    }
    Err("the instance never answered ready".to_string())
}

/// `tab-gallery`.
/// # Errors
/// A usage error, or an output that cannot be written.
pub fn cmd(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    if take_flag(&mut a, "--help") || take_flag(&mut a, "-h") {
        print!("{HELP}");
        return Ok(0);
    }
    let quick = take_flag(&mut a, "--quick");
    let themes = take_value(&mut a, "--themes")?;
    let aterm = instance::resolve_aterm(take_value(&mut a, "--aterm")?)?;
    let [out] = a.as_slice() else {
        return Err(Fail::usage(HELP));
    };
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let out = std::fs::canonicalize(&out).map_err(|e| e.to_string())?;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("tabs")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .map_err(|e| e.to_string())?;
    let names: Vec<String> = match themes.as_deref() {
        None if quick => QUICK.iter().map(ToString::to_string).collect(),
        None | Some("all") => ALL.iter().map(ToString::to_string).collect(),
        Some(list) => list.split(',').map(str::to_string).collect(),
    };
    let mut run = Run {
        aterm,
        out: out.clone(),
        report: String::new(),
        failed: false,
    };
    for theme in &names {
        if stop::check().is_err() {
            run.failed = true;
            run.line("stopped by a signal before the remaining themes");
            break;
        }
        run.line(&format!("== {theme}"));
        if let Err(e) = run.theme(theme, scratch.path()) {
            run.failed = true;
            run.line(&format!("{theme} COULD NOT RUN: {e}"));
        }
    }
    let verdict = if run.failed { "FAILED" } else { "ok" };
    run.line(&format!(
        "tab-gallery {verdict}: {} themes in {}",
        names.len(),
        out.display()
    ));
    std::fs::write(out.join("report.txt"), &run.report).map_err(|e| e.to_string())?;
    Ok(i32::from(run.failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chrome_reply_names_each_chip() {
        let got = chips(
            "toolbar-tabs count=2\n\
             tab-frame index=0 surface=terminal selected=false x=100 y=10 w=200 h=60 icon=-\n\
             tab-frame index=1 surface=app selected=true x=300 y=10 w=200 h=60 icon=340,26,28,28\n",
        );
        assert_eq!(got.len(), 2);
        assert!(!got[0].app && got[0].icon.is_none());
        assert!(got[1].app && got[1].selected);
        assert_eq!(got[1].icon, Some(px_box(340, 26, 28, 28)));
    }

    #[test]
    fn a_strip_with_no_tile_fails_the_layout_check() {
        let chip = |app, icon: bool| Chip {
            app,
            selected: false,
            frame: px_box(0, 0, 10, 10),
            icon: icon.then(|| px_box(1, 1, 5, 5)),
        };
        let good: Vec<Chip> = WIDE_APPS.iter().map(|app| chip(*app, *app)).collect();
        assert!(wide_layout_holds(&good).is_ok());
        let bare: Vec<Chip> = WIDE_APPS.iter().map(|app| chip(*app, false)).collect();
        assert!(wide_layout_holds(&bare).is_err());
        assert!(wide_layout_holds(&[]).is_err());
    }

    #[test]
    fn a_tile_is_judged_by_its_own_state() {
        let reading = TileReading {
            ground: [0x20, 0x20, 0x20],
            fill: [0x3c, 0x3c, 0x3c],
            glyph: [0xe0, 0xe0, 0xe0],
        };
        assert!(judge(&reading, false).iter().all(|(_, pass)| *pass));
        assert!(
            !judge(&reading, true)[0].1,
            "a selected tile is held to 3:1"
        );
    }

    #[test]
    fn every_theme_name_makes_a_file_name() {
        assert_eq!(slug("Solarized Dark"), "solarized-dark");
        assert!(ALL.iter().all(|t| !slug(t).contains(' ')));
    }

    #[test]
    fn a_custom_theme_parses_as_the_theme_it_names() {
        for custom in &CUSTOM {
            let parsed = aterm_types::scheme::parse_theme_str(&custom_theme_text(custom));
            assert!(parsed.is_ok(), "{}", custom.0);
        }
    }
}
