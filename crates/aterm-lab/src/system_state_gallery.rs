// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `system-state-gallery`: System State's fixtures photographed through fresh private headless
//! aterms ([`crate::instance`]'s machinery, as `trail-gallery` uses it: a scratch HOME and XDG
//! dirs, the automatic lanes off, a private control socket, a lifeline; ended by its process
//! group, never by name), one instance at a time, across looks, sizes and motion times; each
//! frame judged by [`crate::system_state_measure`] (init-on-by-default design §4.9), and every
//! look and size laid out on one contact sheet.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::instance::{self, Instance, Launch, Reply, World, take_flag, take_value, take_values};
use crate::paths::{from_caller, paths};
use crate::pyjson::J;
use crate::stop::{self, Stopped};
use crate::system_state_measure::{self as measure, Check, Layout};
use crate::system_state_sheet::{self as sheet, Tile};
use crate::trail_contrast::{Image, round_to};
use crate::trail_gallery::{ReplyShape, checkouts, image_bytes, relay_reply, resolve_out};
use crate::{Fail, Res};

/// The verb's own help (`system-state-gallery --help`).
pub const HELP: &str = "\
usage: targo --unverified lab system-state-gallery [--aterm PATH] [--fixtures NAME[,NAME]]... [--quick]
         [--looks LOOK[,LOOK]]... [--sizes SIZE[,SIZE]]... [--at MS[,MS]]... [--reduce-motion] [--keep] <out_dir>
       targo --unverified lab system-state-gallery --measure <out_dir>

Photograph System State's fixtures and judge each frame (init-on-by-default design §4.9): one fresh private headless
aterm at a time per fixture, look, scale and motion time (a scratch HOME and XDG dirs, a private control socket, a
lifeline; ended by its process group, never by name), showing the page under ATERM_DEV_SYSTEM_STATE_FIXTURE.

  <out_dir>      where the frames, sheets and report go, OUTSIDE the repository; made if missing; a rerun replaces
                 the frames it takes
  --fixtures     fixtures to take, by name (default: every one the seam accepts, the FIXTURES list of this checkout's
                 crates/aterm-gui/src/system_state.rs, read when the lab was built); repeatable
  --quick        a builder's subset: the fixtures checking, running, needs-you, failed and set-up (those of them the
                 list has; --fixtures overrides), the looks dark and light, the sizes regular and compact
  --looks        dark (the default theme), light (\"GitHub Light\"), hc-dark, hc-light (High Contrast),
                 rt-dark, rt-light (Reduce Transparency); high-contrast and reduce-transparency name both of theirs;
                 all (the default); repeatable. High Contrast and Reduce Transparency come from the instance's
                 ATERM_DEV_ACCESSIBILITY seam, never the Mac's own settings
  --sizes        regular (900x644 pt at 1x), regular-2x (900x644 pt at 2x), compact (46 columns of the regular
                 window's grid at 1x); all (the default); repeatable
  --at MS        motion times: one frame per time, the instance's painters pinned there by ATERM_DEV_MOTION_AT_MS
                 (refused, exit 3, by a build without that seam); none: one settled frame
  --reduce-motion  every instance with Reduce Motion on (ATERM_DEV_ACCESSIBILITY)
  --aterm PATH   the aterm to run (default: the lab's pick, `targo --unverified lab paths`); a debug or dev-seams build
  --keep         retain every instance's scratch world
  --measure      take nothing: judge the frames already in <out_dir> again and rewrite its sheets and report

Each frame is <fixture>.<look>.<size>[.t<ms>]: .png, .layout.txt (the page's own layout, `aterm ctl inspect app/v1 view
<view> audit`) and .image-meta.txt; one sheet.<look>.<size>[.t<ms>].png per look, size and time; report.txt and
report.json. Where the text, glyphs and cards are is read from the layout, never guessed from pixels; their colours
from the PNG:

  text    every node whose words the layout audits: its ink (the most contrasting colour two or more pixels share)
          against its background (its rect's most common colour) >= 4.5:1; a disabled control's words, and words
          a scrolling list cuts or fades out at its edge, are skipped
  glyph   every painter-drawn mark (a custom node, not a hairline): its ink against its wash (the most common colour
          1-3 px around it) and against the ground (the card it sits on, else the page) >= 3:1; a mark that paints
          nothing is blank, and one a list cuts or fades out, not judged
  card    every group or painted surface the layout names a card (surface=card, hero or code): its colour 2-5 pt
          inside its edge against the page's 2-6 pt outside it >= 1.2:1, both read above its list's fade; a card
          inside a larger one is not judged
  clip    no visible words ellipsised, wanting more room than they have, or cut by a clip or scroll edge; words
          scrolled wholly out of view, or cut where a list fades its rows out (its cue that more lies below), pass
  rhythm  every visible node's top (from the page's top) and height on the 4 pt grid; hairlines and full-height
          backdrops are not judged

exit: 0 every frame taken and every check passed; 1 a check failed, a frame could not be taken, or an instance
outlived the run; 2 usage; 3 not run (no aterm, no fixture list, no motion seam for --at)
";

/// The fixture list's source, read when the lab is built: the one list the seam accepts.
const FIXTURE_SOURCE: &str = include_str!("../../aterm-gui/src/system_state.rs");

/// The seam that pins the painters' motion clock (MOT-1, design §5.1).
const MOTION_SEAM: &str = "ATERM_DEV_MOTION_AT_MS";

/// `--quick`'s fixtures: the phases of design §4.3 (checking, running, needs you, failed,
/// finished).
const QUICK_FIXTURES: [&str; 5] = ["checking", "running", "needs-you", "failed", "set-up"];

/// The regular window, in points (design §4.1).
const REGULAR_PT: (u32, u32) = (900, 644);
/// The compact window's columns of the regular window's grid.
const COMPACT_COLUMNS: u32 = 46;

/// How long a page may take to settle before its frame is taken as it stands: a hang detector.
const SETTLE_LIMIT: Duration = Duration::from_secs(60);
/// Two reads of the page's layout this far apart that agree: it has settled.
const SETTLE_GAP: Duration = Duration::from_millis(250);

/// The fixtures the seam accepts: the `FIXTURES` list of `source`, in its order.
/// # Errors
/// The list is missing, or holds a different count of names than its type declares.
pub fn parse_fixtures(source: &str) -> Result<Vec<&str>, String> {
    const HEAD: &str = "const FIXTURES: [&str; ";
    let at = source
        .find(HEAD)
        .ok_or("crates/aterm-gui/src/system_state.rs declares no `const FIXTURES: [&str; N]`")?;
    let rest = &source[at + HEAD.len()..];
    let (count, rest) = rest
        .split_once("] = [")
        .ok_or("FIXTURES is not a `[&str; N] = [...]` array")?;
    let count: usize = count
        .trim()
        .parse()
        .map_err(|_| format!("FIXTURES declares no count: {count:?}"))?;
    let body = rest.split_once("];").ok_or("FIXTURES never closes")?.0;
    let names: Vec<&str> = body.split('"').skip(1).step_by(2).collect();
    if names.len() != count {
        return Err(format!(
            "FIXTURES declares {count} names and lists {}",
            names.len()
        ));
    }
    Ok(names)
}

/// A look: the theme, and the accessibility setting the instance is put in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Dark,
    Light,
    HcDark,
    HcLight,
    RtDark,
    RtLight,
}

impl Look {
    pub const ALL: [Look; 6] = [
        Look::Dark,
        Look::Light,
        Look::HcDark,
        Look::HcLight,
        Look::RtDark,
        Look::RtLight,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Look::Dark => "dark",
            Look::Light => "light",
            Look::HcDark => "hc-dark",
            Look::HcLight => "hc-light",
            Look::RtDark => "rt-dark",
            Look::RtLight => "rt-light",
        }
    }

    /// The looks a `--looks` word names.
    fn parse(word: &str) -> Option<Vec<Look>> {
        match word {
            "all" => Some(Look::ALL.to_vec()),
            "high-contrast" => Some(vec![Look::HcDark, Look::HcLight]),
            "reduce-transparency" => Some(vec![Look::RtDark, Look::RtLight]),
            _ => Look::ALL
                .into_iter()
                .find(|l| l.name() == word)
                .map(|l| vec![l]),
        }
    }

    const fn light(self) -> bool {
        matches!(self, Look::Light | Look::HcLight | Look::RtLight)
    }

    /// Its top-level `aterm.toml` line: the light theme's name (the default theme is dark).
    fn config(self) -> Vec<String> {
        if self.light() {
            vec!["theme = \"GitHub Light\"".to_string()]
        } else {
            Vec::new()
        }
    }

    /// The `ATERM_DEV_ACCESSIBILITY` word it turns on.
    const fn accessibility(self) -> Option<&'static str> {
        match self {
            Look::HcDark | Look::HcLight => Some("high-contrast"),
            Look::RtDark | Look::RtLight => Some("reduce-transparency"),
            Look::Dark | Look::Light => None,
        }
    }
}

/// A window size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Regular,
    Regular2x,
    Compact,
}

impl Size {
    pub const ALL: [Size; 3] = [Size::Regular, Size::Regular2x, Size::Compact];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Size::Regular => "regular",
            Size::Regular2x => "regular-2x",
            Size::Compact => "compact",
        }
    }

    fn parse(word: &str) -> Option<Vec<Size>> {
        if word == "all" {
            return Some(Size::ALL.to_vec());
        }
        Size::ALL
            .into_iter()
            .find(|s| s.name() == word)
            .map(|s| vec![s])
    }

    /// Device pixels per point.
    const fn scale(self) -> u32 {
        match self {
            Size::Regular2x => 2,
            Size::Regular | Size::Compact => 1,
        }
    }
}

/// What `system-state-gallery` was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub out: String,
    /// `None`: every fixture (or `--quick`'s).
    pub fixtures: Option<Vec<String>>,
    pub quick: bool,
    pub looks: Vec<Look>,
    pub sizes: Vec<Size>,
    /// Motion times in ms; empty: one settled frame.
    pub at: Vec<u64>,
    pub reduce_motion: bool,
    pub aterm: Option<String>,
    pub keep: bool,
    pub measure_only: bool,
}

fn usage(msg: impl AsRef<str>) -> Fail {
    Fail::usage(format!("system-state-gallery: {}", msg.as_ref()))
}

/// Every comma-separated word of a repeatable flag's values.
fn words(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse the command line.
/// # Errors
/// An unknown look, size, time or flag, or no `<out_dir>`.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let aterm = take_value(&mut a, "--aterm")?;
    let fixtures = words(&take_values(&mut a, "--fixtures")?);
    let looks = words(&take_values(&mut a, "--looks")?);
    let sizes = words(&take_values(&mut a, "--sizes")?);
    let at = words(&take_values(&mut a, "--at")?);
    let quick = take_flag(&mut a, "--quick");
    let reduce_motion = take_flag(&mut a, "--reduce-motion");
    let keep = take_flag(&mut a, "--keep");
    let measure_only = take_flag(&mut a, "--measure");
    if let Some(flag) = a.iter().find(|x| x.starts_with("--")) {
        return Err(usage(format!("unknown flag {flag} (--help)")));
    }
    let out = match a.as_slice() {
        [out] => out.clone(),
        [] => return Err(usage("name an <out_dir> (--help)")),
        [_, extra, ..] => return Err(usage(format!("unexpected '{extra}' (--help)"))),
    };
    let mut parsed_looks = Vec::new();
    for word in &looks {
        for look in Look::parse(word).ok_or_else(|| usage(format!("no look '{word}' (--help)")))? {
            if !parsed_looks.contains(&look) {
                parsed_looks.push(look);
            }
        }
    }
    if parsed_looks.is_empty() {
        parsed_looks = if quick {
            vec![Look::Dark, Look::Light]
        } else {
            Look::ALL.to_vec()
        };
    }
    let mut parsed_sizes = Vec::new();
    for word in &sizes {
        for size in Size::parse(word).ok_or_else(|| usage(format!("no size '{word}' (--help)")))? {
            if !parsed_sizes.contains(&size) {
                parsed_sizes.push(size);
            }
        }
    }
    if parsed_sizes.is_empty() {
        parsed_sizes = if quick {
            vec![Size::Regular, Size::Compact]
        } else {
            Size::ALL.to_vec()
        };
    }
    let at = at
        .iter()
        .map(|t| {
            t.parse::<u64>()
                .map_err(|_| usage(format!("--at takes milliseconds, not '{t}'")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Args {
        out,
        fixtures: (!fixtures.is_empty()).then_some(fixtures),
        quick,
        looks: parsed_looks,
        sizes: parsed_sizes,
        at,
        reduce_motion,
        aterm,
        keep,
        measure_only,
    })
}

/// The fixtures a run takes: `--fixtures` (each one the list holds), else `--quick`'s that the
/// list holds, else the whole list.
/// # Errors
/// A named fixture the list does not hold.
pub fn chosen_fixtures(all: &[&str], a: &Args) -> Result<Vec<String>, Fail> {
    if let Some(named) = &a.fixtures {
        if let Some(unknown) = named.iter().find(|n| !all.contains(&n.as_str())) {
            return Err(usage(format!(
                "no fixture '{unknown}'; the seam accepts: {}",
                all.join(" ")
            )));
        }
        return Ok(named.clone());
    }
    if a.quick {
        let quick: Vec<String> = QUICK_FIXTURES
            .iter()
            .filter(|q| all.contains(q))
            .map(|q| (*q).to_string())
            .collect();
        if !quick.is_empty() {
            return Ok(quick);
        }
    }
    Ok(all.iter().map(|n| (*n).to_string()).collect())
}

/// The name of one frame's files: `<fixture>.<look>.<size>[.t<ms>]`.
#[must_use]
pub fn frame_name(fixture: &str, look: Look, size: Size, at: Option<u64>) -> String {
    let time = at.map_or_else(String::new, |ms| format!(".t{ms}"));
    format!("{fixture}.{}.{}{time}", look.name(), size.name())
}

/// A frame's name read back: fixture, look, size and time.
#[must_use]
pub fn parse_frame_name(name: &str) -> Option<(String, Look, Size, Option<u64>)> {
    let mut parts: Vec<&str> = name.split('.').collect();
    let at = match parts.last() {
        Some(last) if last.starts_with('t') && last[1..].parse::<u64>().is_ok() => {
            let ms = last[1..].parse::<u64>().ok();
            parts.pop();
            ms
        }
        _ => None,
    };
    let [fixture, look, size] = parts.as_slice() else {
        return None;
    };
    if fixture.is_empty() {
        return None;
    }
    let look = Look::ALL.into_iter().find(|l| l.name() == *look)?;
    let size = Size::ALL.into_iter().find(|s| s.name() == *size)?;
    Some(((*fixture).to_string(), look, size, at))
}

/// Whether the executable at `aterm` names the seam `seam`: a build that reads it carries its
/// name.
/// # Errors
/// The executable could not be read, which says nothing of its seams.
fn names_seam(aterm: &Path, seam: &str) -> std::io::Result<bool> {
    let bytes = crate::util::read_program(aterm)?;
    let needle = seam.as_bytes();
    Ok(bytes.windows(needle.len()).any(|w| w == needle))
}

/// One judged frame.
struct Judged {
    name: String,
    fixture: String,
    look: Look,
    size: Size,
    at: Option<u64>,
    /// The frame's pixels and the page's viewport in points.
    width: usize,
    height: usize,
    viewport: measure::Rect,
    checks: Vec<Check>,
    /// Why the frame could not be judged (no surfaces in the layout, a fixture the build lacks).
    notes: Vec<String>,
}

impl Judged {
    fn pass(&self) -> bool {
        self.checks.iter().all(Check::pass)
    }

    fn line(&self) -> String {
        let time = self.at.map_or_else(String::new, |ms| format!(" t{ms}"));
        let checks: Vec<String> = self.checks.iter().map(Check::phrase).collect();
        format!(
            "{:<26} {:<8} {:<10}{time}  {}",
            self.fixture,
            self.look.name(),
            self.size.name(),
            checks.join(" | ")
        )
    }

    fn details(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .notes
            .iter()
            .map(|n| format!("    note: {n}"))
            .collect();
        for check in &self.checks {
            for f in check.failures.iter().take(12) {
                out.push(format!(
                    "    {} {} {}: {}",
                    check.name, f.value, f.key, f.detail
                ));
            }
            if check.failures.len() > 12 {
                out.push(format!(
                    "    {} … {} more (report.json)",
                    check.name,
                    check.failures.len() - 12
                ));
            }
        }
        out
    }

    fn json(&self) -> J {
        J::obj([
            ("frame", J::s(self.name.as_str())),
            ("fixture", J::s(self.fixture.as_str())),
            ("look", J::s(self.look.name())),
            ("size", J::s(self.size.name())),
            ("scale", J::Int(i64::from(self.size.scale()))),
            (
                "at_ms",
                self.at.and_then(|ms| i64::try_from(ms).ok()).into(),
            ),
            ("png_px", J::s(format!("{}x{}", self.width, self.height))),
            (
                "viewport_pt",
                J::s(format!(
                    "{},{},{},{}",
                    round_to(self.viewport.x, 1),
                    round_to(self.viewport.y, 1),
                    round_to(self.viewport.w, 1),
                    round_to(self.viewport.h, 1)
                )),
            ),
            ("pass", J::Bool(self.pass())),
            (
                "checks",
                J::Obj(
                    self.checks
                        .iter()
                        .map(|c| (c.name.to_string(), c.json()))
                        .collect(),
                ),
            ),
            (
                "notes",
                J::Arr(self.notes.iter().map(|n| J::s(n.as_str())).collect()),
            ),
        ])
    }

    /// Its sheet tile: the caption and each check's short verdict.
    fn tile(&self, frame: Image) -> Tile {
        let verdicts = self
            .checks
            .iter()
            .map(|c| {
                let word = match c.name {
                    "clip" | "rhythm" => format!("{} {}", c.name, c.failures.len()),
                    name => format!(
                        "{name} {}",
                        c.worst
                            .map_or_else(|| "-".to_string(), |w| format!("{w:.2}"))
                    ),
                };
                (word, c.pass())
            })
            .collect();
        Tile {
            frame,
            caption: self.fixture.clone(),
            verdicts,
        }
    }
}

/// Judge the frame `<dir>/<name>.png` by its layout `<dir>/<name>.layout.txt`.
fn judge(dir: &Path, name: &str) -> Result<(Judged, Image), String> {
    let (fixture, look, size, at) =
        parse_frame_name(name).ok_or_else(|| format!("{name}: not a gallery frame's name"))?;
    let png = dir.join(format!("{name}.png"));
    let frame = Image::read(&png)?;
    let text = crate::util::read_regular_text(&dir.join(format!("{name}.layout.txt")))
        .map_err(|e| format!("{name}.layout.txt: {e}"))?;
    let layout: Layout = measure::parse_layout(&text).map_err(|e| format!("{name}: {e}"))?;
    let mut notes = Vec::new();
    if !measure::names_surfaces(&layout) {
        notes.push(
            "the build's audit names no group surface (`surface=`): no card was found to judge"
                .to_string(),
        );
    }
    let checks = measure::measure(&frame, &layout);
    Ok((
        Judged {
            name: name.to_string(),
            fixture,
            look,
            size,
            at,
            width: frame.width,
            height: frame.height,
            viewport: layout.viewport,
            checks,
            notes,
        },
        frame,
    ))
}

/// A running instance and its scratch world: dropped, the instance is stopped (its process
/// group) before the world is removed (fields drop in order).
struct Live {
    inst: Instance,
    _world: World,
}

/// A run: where it writes, what it runs, and what it has taken.
struct Gallery {
    out: PathBuf,
    aterm: PathBuf,
    keep: bool,
    reduce_motion: bool,
    pids: Option<std::fs::File>,
    launched: Vec<(i32, String, String)>,
    /// Frames taken, by name, in order.
    taken: Vec<String>,
    /// Frames that could not be taken, and why.
    missed: Vec<(String, String)>,
}

impl Gallery {
    fn launch(
        &mut self,
        fixture: &str,
        look: Look,
        scale: u32,
        at: Option<u64>,
    ) -> Result<Live, String> {
        let mut world = World::new(&instance::config_with(&look.config()))?;
        if self.keep {
            world.keep();
        }
        let mut env = world.env();
        env.push((
            "ATERM_DEV_SYSTEM_STATE_FIXTURE".to_string(),
            fixture.to_string(),
        ));
        let accessibility: Vec<&str> = look
            .accessibility()
            .into_iter()
            .chain(self.reduce_motion.then_some("reduce-motion"))
            .collect();
        if !accessibility.is_empty() {
            env.push((
                "ATERM_DEV_ACCESSIBILITY".to_string(),
                accessibility.join(","),
            ));
        }
        if let Some(ms) = at {
            env.push((MOTION_SEAM.to_string(), ms.to_string()));
        }
        let inst = Instance::start(
            &world,
            Launch {
                aterm: self.aterm.clone(),
                headless: true,
                args: vec![
                    "--columns".to_string(),
                    "110".to_string(),
                    "--lines".to_string(),
                    "36".to_string(),
                    "--scale".to_string(),
                    scale.to_string(),
                ],
                env,
                cwd: world.home.clone(),
                sandbox: None,
                reroute: false,
            },
        )?;
        let sock = inst.sock.to_string_lossy().into_owned();
        let tag = format!("{fixture}.{}.{scale}x", look.name());
        if let Some(f) = self.pids.as_mut() {
            let _ = writeln!(f, "{}\t{sock}\t{tag}", inst.pid);
        }
        self.launched.push((inst.pid, sock, tag));
        Ok(Live {
            inst,
            _world: world,
        })
    }

    /// Take every size of `sizes` (all at one scale) of one fixture, look and time.
    fn take(
        &mut self,
        fixture: &str,
        look: Look,
        sizes: &[Size],
        at: Option<u64>,
    ) -> Result<(), Stopped> {
        let Some(scale) = sizes.first().map(|s| s.scale()) else {
            return Ok(());
        };
        let names: Vec<String> = sizes
            .iter()
            .map(|&s| frame_name(fixture, look, s, at))
            .collect();
        for name in &names {
            crate::trail_gallery::remove_named(&self.out, name);
        }
        let mut live = match self.launch(fixture, look, scale, at) {
            Ok(live) => live,
            Err(e) => {
                for name in names {
                    self.miss(name, e.clone());
                }
                return Ok(());
            }
        };
        let shown = show(&mut live.inst, look, scale);
        for (&size, name) in sizes.iter().zip(&names) {
            let taken = match &shown {
                Err(miss) => Err(miss.clone()),
                Ok(view) => self.frame(&live.inst, view, size, name),
            };
            match taken {
                Ok(()) => self.taken.push(name.clone()),
                Err(Miss::Stopped(s)) => return Err(s),
                Err(Miss::Failed(e)) => self.miss(name.clone(), e),
            }
        }
        if let Ok(log) = crate::util::read_regular_text(&live.inst.log)
            && log.contains("names no System State scenario")
        {
            for name in &names {
                self.miss(
                    name.clone(),
                    format!("this aterm has no fixture {fixture:?} (its log says so)"),
                );
            }
        }
        live.inst.stop();
        let _ = std::fs::copy(
            &live.inst.log,
            self.out
                .join(format!("{fixture}.{}.{scale}x.gui.log", look.name())),
        );
        Ok(())
    }

    fn miss(&mut self, name: String, why: String) {
        eprintln!("system-state-gallery: [{name}] {why}");
        self.taken.retain(|n| n != &name);
        if !self.missed.iter().any(|(n, _)| n == &name) {
            self.missed.push((name, why));
        }
    }

    /// Put the shown page at `size`, let it settle, and keep its frame and layout.
    fn frame(&self, inst: &Instance, view: &View, size: Size, name: &str) -> Result<(), Miss> {
        let scale = size.scale();
        let resize = match size {
            Size::Regular | Size::Regular2x => format!(
                "resize px {} {}",
                REGULAR_PT.0 * scale,
                REGULAR_PT.1 * scale
            ),
            Size::Compact => format!("resize {} {COMPACT_COLUMNS}", view.regular_rows()?),
        };
        let words: Vec<&str> = resize.split(' ').collect();
        let r = inst.ctl(&words);
        if r.code != 0 {
            return Err(Miss::Failed(format!("{resize}: {}", r.first())));
        }
        let (png, meta, layout) = settled_frame(inst, view.id)?;
        let write = |suffix: &str, bytes: &[u8]| {
            let path = self.out.join(format!("{name}.{suffix}"));
            std::fs::write(&path, bytes)
                .map_err(|e| Miss::Failed(format!("{}: {e}", path.display())))
        };
        write("png", &png)?;
        write("image-meta.txt", format!("{meta}\n").as_bytes())?;
        write("layout.txt", layout.as_bytes())
    }

    /// Every launched instance must be gone: one still running (its pid checked against its
    /// own socket in its arguments, so a recycled pid is never mistaken) is reported and
    /// killed by pid.
    fn survivors(&self) -> u32 {
        let mut alive = 0;
        for (pid, sock, name) in &self.launched {
            if crate::procs::command_line(*pid).contains(&format!("--control-sock {sock}")) {
                eprintln!(
                    "system-state-gallery: [{name}] pid {pid} is still running; killing it by pid"
                );
                let _ = crate::sys::kill(*pid, libc::SIGKILL);
                alive += 1;
            }
        }
        alive
    }
}

/// The System State view an instance shows, and its window's grid.
struct View {
    id: u64,
    /// The `inspect tabs` reply it was found in.
    tabs: String,
}

impl View {
    /// The window's rows once at the regular size: the compact window keeps them.
    fn regular_rows(&self) -> Result<String, String> {
        self.tabs
            .lines()
            .find_map(|l| {
                l.trim_start().strip_prefix("window ").and_then(|rest| {
                    measure::fields(rest)
                        .into_iter()
                        .find(|(k, _)| k == "rows")
                        .map(|(_, v)| v)
                })
            })
            .ok_or_else(|| "inspect tabs names no window rows".to_string())
    }
}

/// Why a frame was not taken: an interrupt (the run ends), or a failure (the run goes on).
#[derive(Clone, Debug)]
enum Miss {
    Stopped(Stopped),
    Failed(String),
}

impl From<String> for Miss {
    fn from(why: String) -> Miss {
        Miss::Failed(why)
    }
}

impl From<Stopped> for Miss {
    fn from(s: Stopped) -> Miss {
        Miss::Stopped(s)
    }
}

/// Bring `inst` up, put its OS appearance with `look`, and open System State.
fn show(inst: &mut Instance, look: Look, scale: u32) -> Result<View, Miss> {
    inst.await_socket(instance::BOOT)
        .map_err(|f| match f.stopped {
            Some(sig) => Miss::Stopped(Stopped(sig)),
            None => Miss::Failed(f.msg.unwrap_or_default()),
        })?;
    // The OS appearance the theme stands for, as a person in that mode has it (a development
    // build injects it; a refusal leaves the headless default, the theme still chosen).
    let _ = inst.ctl(&["appearance", if look.light() { "light" } else { "dark" }]);
    let opened = inst.ctl(&["open", "app", "system-state"]);
    if opened.code != 0 {
        return Err(Miss::Failed(format!(
            "open app system-state: {}",
            crate::trail_gallery::reply_text(&opened).trim_end()
        )));
    }
    // Fit the regular window first, so a compact frame keeps the regular window's rows.
    let r = inst.ctl(&[
        "resize",
        "px",
        &(REGULAR_PT.0 * scale).to_string(),
        &(REGULAR_PT.1 * scale).to_string(),
    ]);
    if r.code != 0 {
        return Err(Miss::Failed(format!("resize px: {}", r.first())));
    }
    // The window takes its new size through the resize throttle: the view is shown once two
    // reads of the topology agree.
    let end = Instant::now() + SETTLE_LIMIT;
    let mut last = String::new();
    loop {
        let tabs = inst.ctl(&["inspect", "app/v1", "tabs"]);
        let topology = tabs.stdout.lines().skip(1).collect::<Vec<_>>().join("\n");
        if let Some(id) = system_state_view(&tabs.stdout)
            && topology == last
        {
            return Ok(View {
                id,
                tabs: tabs.stdout,
            });
        }
        if Instant::now() >= end {
            return Err(Miss::Failed(format!(
                "no settled System State view in `inspect app/v1 tabs`: {}",
                crate::trail_gallery::reply_text(&tabs).trim_end()
            )));
        }
        last = topology;
        stop::sleep(SETTLE_GAP).map_err(Miss::Stopped)?;
    }
}

/// The view id of the System State leaf an `inspect tabs` reply names.
#[must_use]
pub fn system_state_view(tabs: &str) -> Option<u64> {
    tabs.lines().find_map(|line| {
        let line = line.trim_start().strip_prefix("leaf ")?;
        let f = measure::fields(line);
        let get = |k: &str| f.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        (get("kind") == Some("app") && get("app") == Some("system-state"))
            .then(|| get("view")?.parse().ok())
            .flatten()
    })
}

/// The layout's shape: its node and fit lines, which a painter that moves (the village walking)
/// leaves as they are, though it changes the compiled UI's fingerprint every tick.
#[must_use]
pub fn layout_shape(audit: &str) -> String {
    audit
        .lines()
        .filter(|l| l.starts_with("paint-node ") || l.starts_with("paint-text "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The page's frame once it has settled: two reads of its layout [`SETTLE_GAP`] apart with the
/// same shape, then a frame, then a read with that shape still, so the layout kept beside the
/// frame is the one it was painted from (a moving painter's tick between them changes no rect).
/// A frame is rendered before each read: a headless page paints only when asked.
fn settled_frame(inst: &Instance, view: u64) -> Result<(Vec<u8>, String, String), Miss> {
    let mut wire = inst.connect().ok();
    let mut request = |words: &[&str], shape: ReplyShape| -> Reply {
        if let Some(w) = wire.as_mut()
            && let Ok(reply) = relay_reply(w, words, shape)
        {
            return reply;
        }
        wire = None;
        inst.ctl(words)
    };
    let view = view.to_string();
    let audit_words = ["inspect", "app/v1", "view", view.as_str(), "audit"];
    let end = Instant::now() + SETTLE_LIMIT;
    let mut last: Option<String> = None;
    loop {
        stop::check()?;
        let _ = request(&["image", "--bytes"], ReplyShape::Counted);
        let audit = request(&audit_words, ReplyShape::Counted);
        if audit.code != 0 {
            return Err(Miss::Failed(format!(
                "inspect app/v1 view {view} audit: {}",
                crate::trail_gallery::reply_text(&audit).trim_end()
            )));
        }
        let mut shape = layout_shape(&audit.stdout);
        if !shape.is_empty() && last.as_deref() == Some(shape.as_str()) {
            let shot = request(&["image", "--meta", "--bytes"], ReplyShape::Counted);
            if shot.code != 0 {
                return Err(Miss::Failed(format!(
                    "image: {}",
                    crate::trail_gallery::reply_text(&shot).trim_end()
                )));
            }
            let (png, meta) = image_bytes(&shot.stdout)?;
            let meta = meta.to_string();
            let after = request(&audit_words, ReplyShape::Counted);
            let held = layout_shape(&after.stdout);
            if after.code == 0 && held == shape {
                return Ok((png, meta, after.stdout));
            }
            shape = held;
        }
        if Instant::now() >= end {
            return Err(Miss::Failed(format!(
                "the page never settled in {} s (its layout kept changing)",
                SETTLE_LIMIT.as_secs()
            )));
        }
        last = Some(shape);
        stop::sleep(SETTLE_GAP)?;
    }
}

/// The frames in `dir`, by name: each `.layout.txt` with its `.png`.
fn frames_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let name = file.strip_suffix(".layout.txt")?.to_string();
            (dir.join(format!("{name}.png")).is_file() && parse_frame_name(&name).is_some())
                .then_some(name)
        })
        .collect();
    names.sort();
    names
}

/// The motion note a sheet and the report carry.
fn motion_note(at: &[u64], seam: bool) -> String {
    if !at.is_empty() {
        format!(
            "motion: frames at {} ms ({MOTION_SEAM})",
            at.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
        )
    } else if seam {
        format!("motion: one settled frame (pass --at MS,... for motion times, {MOTION_SEAM})")
    } else {
        format!("motion: one settled frame: this build has no {MOTION_SEAM} seam yet (MOT-1)")
    }
}

/// Judge the frames `names` in `out` on every core, in `names`' order: each frame's verdict and
/// pixels, or why it could not be judged.
fn judge_all(out: &Path, names: &[String]) -> Vec<Result<(Judged, Image), String>> {
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .clamp(1, names.len().max(1));
    let chunk = names.len().div_ceil(workers).max(1);
    std::thread::scope(|s| {
        let running: Vec<_> = names
            .chunks(chunk)
            .map(|part| {
                let judging = s.spawn(move || part.iter().map(|n| judge(out, n)).collect());
                (part, judging)
            })
            .collect();
        running
            .into_iter()
            .flat_map(|(part, judging)| {
                judging.join().unwrap_or_else(|_| {
                    part.iter()
                        .map(|n| Err(format!("{n}: judging it panicked")))
                        .collect::<Vec<_>>()
                })
            })
            .collect()
    })
}

type SheetGroup = ((Look, Size, Option<u64>), Vec<String>);

/// Judge every frame named, write the sheets, report.txt and report.json, print the report;
/// whether every frame passed. One sheet's frames at a time are held in memory.
fn report(
    out: &Path,
    names: &[String],
    missed: &[(String, String)],
    motion: &str,
    aterm: &str,
) -> Result<bool, String> {
    let mut unjudged: Vec<(String, String)> = missed.to_vec();
    // One sheet per look, size and time, fixtures in the order they were taken.
    let mut groups: Vec<SheetGroup> = Vec::new();
    for name in names {
        let Some((_, look, size, at)) = parse_frame_name(name) else {
            unjudged.push((name.clone(), "not a gallery frame's name".to_string()));
            continue;
        };
        match groups.iter_mut().find(|(key, _)| *key == (look, size, at)) {
            Some((_, members)) => members.push(name.clone()),
            None => groups.push(((look, size, at), vec![name.clone()])),
        }
    }
    let mut verdicts: std::collections::HashMap<String, Judged> = std::collections::HashMap::new();
    let mut sheets = Vec::new();
    for ((look, size, at), members) in &groups {
        let mut tiles: Vec<Tile> = Vec::new();
        for (name, judged) in members.iter().zip(judge_all(out, members)) {
            match judged {
                Ok((j, frame)) => {
                    tiles.push(j.tile(frame));
                    verdicts.insert(name.clone(), j);
                }
                Err(e) => unjudged.push((name.clone(), e)),
            }
        }
        if tiles.is_empty() {
            continue;
        }
        let time = at.map_or_else(String::new, |ms| format!(".t{ms}"));
        let file = format!("sheet.{}.{}{time}.png", look.name(), size.name());
        let (w, h) = REGULAR_PT;
        let window = match size {
            Size::Regular => format!("{w}x{h} pt at 1x"),
            Size::Regular2x => format!("{w}x{h} pt at 2x"),
            Size::Compact => format!("{COMPACT_COLUMNS} columns at 1x"),
        };
        let title = format!(
            "system state · {} · {} {window}{}",
            look.name(),
            size.name(),
            at.map_or_else(String::new, |ms| format!(" · t={ms} ms"))
        );
        sheet::write(&out.join(&file), &title, motion, &tiles)?;
        sheets.push(file);
    }
    let judged: Vec<Judged> = names.iter().filter_map(|n| verdicts.remove(n)).collect();
    let mut text = Vec::new();
    text.push(format!("system-state-gallery: {aterm}"));
    text.push(motion.to_string());
    for j in &judged {
        println!("{}", j.line());
        text.push(j.line());
        text.extend(j.details());
    }
    for (name, why) in &unjudged {
        let line = format!("{name}: NOT TAKEN: {why}");
        println!("{line}");
        text.push(line);
    }
    let totals = totals(&judged, unjudged.len());
    println!("{totals}");
    text.push(totals.clone());
    std::fs::write(out.join("report.txt"), text.join("\n") + "\n")
        .map_err(|e| format!("report.txt: {e}"))?;
    let json = J::obj([
        ("aterm", J::s(aterm)),
        ("motion", J::s(motion)),
        (
            "thresholds",
            J::obj([
                ("text", J::Float(measure::TEXT_MIN)),
                ("glyph", J::Float(measure::GLYPH_MIN)),
                ("card", J::Float(measure::CARD_MIN)),
                ("rhythm_pt", J::Float(measure::RHYTHM_PT)),
            ]),
        ),
        ("frames", J::Arr(judged.iter().map(Judged::json).collect())),
        (
            "not_taken",
            J::Arr(
                unjudged
                    .iter()
                    .map(|(n, why)| {
                        J::obj([("frame", J::s(n.as_str())), ("why", J::s(why.as_str()))])
                    })
                    .collect(),
            ),
        ),
        (
            "sheets",
            J::Arr(sheets.iter().map(|s| J::s(s.as_str())).collect()),
        ),
        ("totals", J::s(totals.as_str())),
    ]);
    std::fs::write(
        out.join("report.json"),
        crate::trail_contrast::render(&json, 0) + "\n",
    )
    .map_err(|e| format!("report.json: {e}"))?;
    Ok(unjudged.is_empty() && judged.iter().all(Judged::pass))
}

/// The totals line: frames, and each check's passes, failures and worst number.
fn totals(judged: &[Judged], not_taken: usize) -> String {
    let mut parts = vec![format!(
        "totals: {} frame(s), {} pass, {} fail, {not_taken} not taken",
        judged.len(),
        judged.iter().filter(|j| j.pass()).count(),
        judged.iter().filter(|j| !j.pass()).count(),
    )];
    for name in ["text", "glyph", "card", "clip", "rhythm"] {
        let checks: Vec<&Check> = judged
            .iter()
            .filter_map(|j| j.checks.iter().find(|c| c.name == name))
            .collect();
        let failed = checks.iter().filter(|c| !c.pass()).count();
        let nodes: usize = checks.iter().map(|c| c.failures.len()).sum();
        let worst = match name {
            "clip" | "rhythm" => checks
                .iter()
                .filter_map(|c| c.worst)
                .fold(None, |m: Option<f64>, w| Some(m.map_or(w, |m| m.max(w)))),
            _ => checks
                .iter()
                .filter_map(|c| c.worst)
                .fold(None, |m: Option<f64>, w| Some(m.map_or(w, |m| m.min(w)))),
        };
        let worst = worst.map_or_else(|| "-".to_string(), |w| format!("{w:.2}"));
        parts.push(format!(
            "{name} {} pass/{failed} fail ({nodes} node(s), worst {worst})",
            checks.len() - failed
        ));
    }
    parts.join(" · ")
}

/// `system-state-gallery`.
/// # Errors
/// Usage (2), not run (3), or interrupted (128 + the signal).
pub fn cmd_system_state_gallery(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    stop::install();
    let a = parse_args(args)?;
    let out = resolve_out(&from_caller(&a.out), &checkouts(&paths().repo)).map_err(usage)?;
    if a.measure_only {
        let names = frames_in(&out);
        if names.is_empty() {
            return Err(usage(format!("no frames in {} to judge", out.display())));
        }
        // The run that took the frames named its aterm and motion in report.txt's head.
        let earlier = crate::util::read_regular_text(&out.join("report.txt")).unwrap_or_default();
        let mut head = earlier.lines();
        let aterm = head
            .next()
            .and_then(|l| l.strip_prefix("system-state-gallery: "))
            .map_or_else(|| "(frames judged again)".to_string(), str::to_string);
        let motion = head
            .next()
            .filter(|l| l.starts_with("motion: "))
            .unwrap_or("motion: as taken")
            .to_string();
        let passed = report(&out, &names, &[], &motion, &aterm)
            .map_err(|e| Fail::die(format!("system-state-gallery: {e}")))?;
        return Ok(i32::from(!passed));
    }
    let all = parse_fixtures(FIXTURE_SOURCE)
        .map_err(|e| Fail::exit(3, format!("system-state-gallery: {e}")))?;
    let fixtures = chosen_fixtures(&all, &a)?;
    let aterm = instance::resolve_aterm(a.aterm.clone()).map_err(|e| Fail::exit(3, e))?;
    let seam = names_seam(&aterm, MOTION_SEAM).map_err(|e| {
        Fail::exit(
            3,
            format!("system-state-gallery: cannot read {}: {e}", aterm.display()),
        )
    })?;
    if !a.at.is_empty() && !seam {
        return Err(Fail::exit(
            3,
            format!(
                "system-state-gallery: {} has no {MOTION_SEAM} seam (MOT-1 adds it), so --at cannot pin a motion time",
                aterm.display()
            ),
        ));
    }
    std::fs::create_dir_all(&out).map_err(|e| {
        Fail::exit(
            3,
            format!("system-state-gallery: cannot make {}: {e}", out.display()),
        )
    })?;
    let pids = std::fs::File::create(out.join("gallery.pids")).ok();
    let mut g = Gallery {
        out: out.clone(),
        aterm: aterm.clone(),
        keep: a.keep,
        reduce_motion: a.reduce_motion,
        pids,
        launched: Vec::new(),
        taken: Vec::new(),
        missed: Vec::new(),
    };
    let times: Vec<Option<u64>> = if a.at.is_empty() {
        vec![None]
    } else {
        a.at.iter().copied().map(Some).collect()
    };
    // One instance per fixture, look, scale and time: a 1x one takes the regular frame, then the
    // compact one.
    let one_x: Vec<Size> = a.sizes.iter().copied().filter(|s| s.scale() == 1).collect();
    let two_x: Vec<Size> = a.sizes.iter().copied().filter(|s| s.scale() == 2).collect();
    let instances = fixtures.len()
        * a.looks.len()
        * times.len()
        * (usize::from(!one_x.is_empty()) + usize::from(!two_x.is_empty()));
    println!(
        "system-state-gallery: {} fixture(s) x {} look(s) x {} size(s) x {} time(s): {instances} instance(s), one at a time -> {}",
        fixtures.len(),
        a.looks.len(),
        a.sizes.len(),
        times.len(),
        out.display()
    );
    let started = Instant::now();
    let mut run = || -> Result<(), Stopped> {
        for fixture in &fixtures {
            for &look in &a.looks {
                for &at in &times {
                    for sizes in [&one_x, &two_x] {
                        g.take(fixture, look, sizes, at)?;
                    }
                }
            }
        }
        Ok(())
    };
    let ran = run();
    let alive = g.survivors();
    if let Err(Stopped(sig)) = ran {
        return Err(Fail {
            msg: Some(format!("system-state-gallery: interrupted (signal {sig})")),
            code: 128 + sig,
            stopped: Some(sig),
        });
    }
    let passed = report(
        &out,
        &g.taken,
        &g.missed,
        &motion_note(&a.at, seam),
        &aterm.display().to_string(),
    )
    .map_err(|e| Fail::die(format!("system-state-gallery: {e}")))?;
    println!(
        "system-state-gallery: {} frame(s) in {} s, {alive} instance(s) left running -> {}",
        g.taken.len(),
        started.elapsed().as_secs(),
        out.display()
    );
    Ok(i32::from(!passed || alive > 0))
}

#[cfg(test)]
#[path = "system_state_gallery_tests.rs"]
mod tests;
