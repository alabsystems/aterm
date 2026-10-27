// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail_dump`'s scripts and printer (`docs/design/PRISM-GLASS.md` §15.1 P0.4, §15.7.2): the
//! `.trail` grammar, frozen now with every directive a later phase needs, and the dump of the
//! frames a script drives through the headless App its includer names `super::rig`.
//! `examples/trail_dump.rs` runs it on `aterm_gui::TrailRig`; aterm-gui's tests include this
//! file and run it on the crate's own.
//!
//! A script is a statement a line; a line starting `#` is a comment. Setup directives say how the window is
//! set up, and each `run <until_ms>` first applies what they changed, in one order (the config,
//! the grid, the panes and their focus, the panel rate, the ramp cost), at the instant the last
//! run ended. It then draws a frame every present interval up to `until_ms`, each after the
//! timed events (`at <ms> …`) due by its instant, at their own instants.
//! `foreach-scheme dark|light { … }` runs the script once per built-in and pack theme of that
//! polarity (`aterm_render::theme_is_dark` of its background), each on a fresh rig.
//!
//! What a frame prints, every instant in milliseconds after the script's start:
//!
//! ```text
//! F <n> t=<ms> light=<bits> arms=<arm>:<stream>+<stream>,…|-
//!   streams glow_under=<len>:<hash> glow_under_ramps=… cursor_glow_add=… …
//!   px <w>x<h>:<hash>
//!   deadline glow=<ms>|- pet=<ms>|- words=<ms>|-
//!   sounds <n>:<hash>|-
//!   state n=<words> <engine>=<hash> … bed=<hash>
//!   light n=<words>
//! ```
//!
//! The `streams` line names every `RenderInput` stream an effect writes, each a light arm may
//! declare (`aterm_effects::state_words::Streams::NAMED`, in its order; `cursor_ink` joins at
//! Phase 3). Every hash is FNV-1a/64 over named words, never `Debug`: a stream element's
//! (`stream_words`), a sound cue's (`aterm_effects::state_words::sound_event_words`) and the
//! bed's ink table's (`BedInkLut::state_words`).
//!
//! A frame a synchronized update holds prints `F <n> t=<ms> held`, then its `state` and `light`
//! words: it draws nothing, so it has no streams, pixels or deadline, and its cues stay for the
//! frame that draws. `--full --frame <n>` also prints every element and word of frame `n`
//! (`W …` lines). Exit codes: 0 dumped, 1 a run failed, 2 a usage or script error.

use super::rig::{TrailFrame, TrailRig};
use aterm_effects::rainbow_kitty::ribbon::BedInkLut;
use aterm_effects::state_words::{Fold, StateWords, Words};
use std::fmt::Write as _;
use std::io::Write;
use std::time::{Duration, Instant};

#[path = "stream_words.rs"]
mod stream_words;

/// Exit code: a usage or script error.
pub(crate) const EXIT_USAGE: i32 = 2;
/// Exit code: a run failed.
pub(crate) const EXIT_FAILED: i32 = 1;

/// The font every dump draws: the repo's fixture face, so glyph pixels never move with the host.
pub(crate) const FIXTURE_FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
);

/// The background `page translucent` asks for: a translucent page, not a choice of opacity.
const TRANSLUCENT_OPACITY: f32 = 0.8;

/// A setup directive.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Setup {
    /// `grid <rows> <cols>`: the window's grid.
    Grid { rows: u16, cols: u16 },
    /// `font <px>`: recorded, not honoured. Every dump draws the fixture face at the App's own
    /// size; the P0.6 corpus wires a size when a script first needs one.
    Font(f32),
    /// `style <name…>`: `cursor_trail_style`.
    Style(String),
    /// `pet on <seed>|off`: the pet, on the launch kitty `seed` mints.
    Pet(Option<u64>),
    /// `words <style>|off`: Sparkle Words (`sparkle_words.enabled`). A profanity style
    /// ([`PROFANITY_STYLES`]) is `[sparkle_words.profanity] style`; any other style is recorded,
    /// not honoured: no config key names one yet, and the phase that adds it wires it here.
    Words(Option<String>),
    /// `theme <fg> <bg>`: the configured colours (`foreground`, `background`).
    Theme { fg: u32, bg: u32 },
    /// `panes <n>`: split the window until it shows `n` panes, side by side.
    Panes(usize),
    /// `pane <i> theme <fg> <bg>`: recorded, not honoured; a pane's own session theme lands at
    /// 1.3 with the configured-colour seams.
    PaneTheme { pane: usize, fg: u32, bg: u32 },
    /// `focus-pane <i>`: focus pane `i`, counted from the first.
    FocusPane(usize),
    /// `hz <n>`: the panel's refresh rate.
    Hz(u32),
    /// `ramp never|always`: the glow's ramp cost, in place of the shipped one.
    Ramp(Ramp),
    /// `page opaque|translucent|wallpaper`: the page under the grid. `wallpaper` is recorded,
    /// not honoured: the corpus's wallpaper fixture lands with P0.6, its reader at 1.9.
    Page(Page),
    /// `intensity <x>`: `cursor_trail_intensity`.
    Intensity(f32),
    /// `live off|glass|lip|marks|vanish`: recorded, not honoured until `force_live` lands at 1.3.
    Live(Live),
}

/// `ramp`'s cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ramp {
    /// Every ramp row laid as the quad it stands for.
    Never,
    /// Every run a ramp, however short.
    Always,
}

/// `page`'s page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    /// An opaque page.
    Opaque,
    /// A translucent page ([`TRANSLUCENT_OPACITY`]).
    Translucent,
    /// A wallpaper under the page.
    Wallpaper,
}

/// `live`'s preview level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Live {
    Off,
    Glass,
    Lip,
    Marks,
    Vanish,
}

/// `motion`'s policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    Full,
    Reduced,
    /// The shipped default: full while focused, and the only one the load-shed latch reduces.
    Auto,
}

/// The `words` styles that are `[sparkle_words.profanity] style`.
pub(crate) const PROFANITY_STYLES: [&str; 3] = ["rainbow", "nova", "sparkle"];

/// A timed event, the `<event>` of `at <ms> <event>`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Event {
    /// `pty <hex|"text">`: the focused pane's terminal reads the bytes.
    Pty(Vec<u8>),
    /// `key <name> [mods=…]`: a key, spelled as the control socket's `key` verb takes it.
    Key(String),
    /// `env [amp <a>] [focus <0|1>] [motion full|reduced|auto]`: the environment. `amp` is
    /// recorded, not honoured: the audio amplitude feed lands at Phase 4.
    Env {
        amp: Option<f32>,
        focus: Option<bool>,
        motion: Option<Motion>,
    },
    /// `cost <ns>`: one present that cost `ns`, fed to the load-shed latch.
    Cost(u64),
    /// `pointer <x> <y>|off`: the pointer moves to window pixel `(x, y)`, or leaves.
    Pointer(Option<(f64, f64)>),
    /// `press <x> <y>`: a left click at window pixel `(x, y)`.
    Press(f64, f64),
    /// `select <r0> <c0> <r1> <c1>|off`: the focused pane's viewport cells selected, or cleared.
    Select(Option<((u16, u16), (u16, u16))>),
}

/// One statement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stmt {
    /// A setup directive.
    Setup(Setup),
    /// `at <ms> <event>`.
    At { ms: u64, event: Event },
    /// `run <until_ms>`.
    Run { until_ms: u64 },
    /// One expansion's theme ([`Script::runs`]); never parsed.
    Scheme(Scheme),
}

/// A theme `foreach-scheme` expands over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Scheme {
    /// A built-in theme, by name.
    Builtin(&'static str),
    /// A pack theme, by id, installed from its text.
    Pack {
        id: &'static str,
        text: &'static str,
    },
}

impl Scheme {
    fn name(&self) -> &'static str {
        match self {
            Self::Builtin(name) => name,
            Self::Pack { id, .. } => id,
        }
    }
}

/// A parsed script: the statements before a `foreach-scheme` block, its body and polarity, and
/// the statements after it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Script {
    pub(crate) head: Vec<Stmt>,
    pub(crate) each: Option<(bool, Vec<Stmt>)>,
    pub(crate) tail: Vec<Stmt>,
}

/// A script error at a line, counted from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParseError {
    pub(crate) line: usize,
    pub(crate) msg: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

impl Script {
    /// Each run the script makes: its label (`-`, or the theme `foreach-scheme` gave it) and its
    /// statements.
    pub(crate) fn runs(&self) -> Vec<(&'static str, Vec<Stmt>)> {
        let Some((dark, body)) = &self.each else {
            return vec![("-", self.head.clone())];
        };
        schemes(*dark)
            .into_iter()
            .map(|scheme| {
                let mut stmts = self.head.clone();
                let name = scheme.name();
                stmts.push(Stmt::Scheme(scheme));
                stmts.extend(body.iter().cloned());
                stmts.extend(self.tail.iter().cloned());
                (name, stmts)
            })
            .collect()
    }
}

/// Every built-in theme then every pack theme whose background is dark (`dark`) or light.
pub(crate) fn schemes(dark: bool) -> Vec<Scheme> {
    let builtins = aterm_types::scheme::builtin_names()
        .into_iter()
        .filter_map(|name| {
            let scheme = aterm_types::scheme::builtin(name)?;
            Some((Scheme::Builtin(name), scheme.background))
        });
    let packs = aterm_theme_pack::themes().iter().filter_map(|theme| {
        let scheme = aterm_types::scheme::parse_scheme_str(theme.text).ok()?;
        Some((
            Scheme::Pack {
                id: theme.id,
                text: theme.text,
            },
            scheme.background,
        ))
    });
    builtins
        .chain(packs)
        .filter(|(_, bg)| {
            let bg = u32::from(bg.r) << 16 | u32::from(bg.g) << 8 | u32::from(bg.b);
            aterm_render::theme_is_dark(bg) == dark
        })
        .map(|(scheme, _)| scheme)
        .collect()
}

/// Parse a whole script, checking that time never runs back: every `at` falls at or after the
/// run before it ends and by the last run's end, and every `run` ends after the one before.
pub(crate) fn parse(text: &str) -> Result<Script, ParseError> {
    let mut script = Script {
        head: Vec::new(),
        each: None,
        tail: Vec::new(),
    };
    // 0: before the block, 1: inside it, 2: after it.
    let mut part = 0;
    let mut open_line = 0;
    let mut ran_to = 0_u64;
    // The latest event and its line.
    let mut last_at: Option<(usize, u64)> = None;
    let mut ran = false;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let err = |msg: String| ParseError { line, msg };
        let code = raw.trim();
        if code.is_empty() || code.starts_with('#') {
            continue;
        }
        if let Some(rest) = code.strip_prefix("foreach-scheme") {
            let words: Vec<&str> = rest.split_whitespace().collect();
            let dark = match words.as_slice() {
                ["dark", "{"] => true,
                ["light", "{"] => false,
                _ => return Err(err("foreach-scheme dark|light {".into())),
            };
            if part != 0 {
                return Err(err("one foreach-scheme block a script, never nested".into()));
            }
            script.each = Some((dark, Vec::new()));
            part = 1;
            open_line = line;
            continue;
        }
        if code == "}" {
            if part != 1 {
                return Err(err("} closes no foreach-scheme block".into()));
            }
            part = 2;
            continue;
        }
        let stmt = statement(code).map_err(err)?;
        match &stmt {
            Stmt::At { ms, .. } => {
                if ran && *ms < ran_to {
                    return Err(err(format!(
                        "at {ms} falls before the run that ended at {ran_to}"
                    )));
                }
                if last_at.is_none_or(|(_, latest)| *ms > latest) {
                    last_at = Some((line, *ms));
                }
            }
            Stmt::Run { until_ms } => {
                if ran && *until_ms <= ran_to {
                    return Err(err(format!(
                        "run {until_ms} ends by the run before ({ran_to})"
                    )));
                }
                ran = true;
                ran_to = *until_ms;
            }
            Stmt::Setup(_) | Stmt::Scheme(_) => {}
        }
        match part {
            0 => script.head.push(stmt),
            1 => {
                if let Some((_, body)) = script.each.as_mut() {
                    body.push(stmt);
                }
            }
            _ => script.tail.push(stmt),
        }
    }
    if part == 1 {
        return Err(ParseError {
            line: open_line,
            msg: "foreach-scheme block never closed".into(),
        });
    }
    if let Some((line, ms)) = last_at
        && (!ran || ms > ran_to)
    {
        return Err(ParseError {
            line,
            msg: format!("at {ms} falls after the last run"),
        });
    }
    Ok(script)
}

/// Parse one statement.
pub(crate) fn statement(code: &str) -> Result<Stmt, String> {
    let (head, rest) = code
        .split_once(char::is_whitespace)
        .map_or((code, ""), |(head, rest)| (head, rest.trim()));
    let args: Vec<&str> = rest.split_whitespace().collect();
    let setup = |s| Ok(Stmt::Setup(s));
    match head {
        "grid" => match args.as_slice() {
            [rows, cols] => setup(Setup::Grid {
                rows: positive(rows, "grid rows")?,
                cols: positive(cols, "grid cols")?,
            }),
            _ => Err("grid <rows> <cols>".into()),
        },
        "font" => match args.as_slice() {
            [px] => setup(Setup::Font(finite(px, "font px")?)),
            _ => Err("font <px>".into()),
        },
        "style" if !args.is_empty() => setup(Setup::Style(args.join(" "))),
        "style" => Err("style <name>".into()),
        "pet" => match args.as_slice() {
            ["on", seed] => setup(Setup::Pet(Some(number(seed, "pet seed")?))),
            ["off"] => setup(Setup::Pet(None)),
            _ => Err("pet on <seed>|off".into()),
        },
        "words" => match args.as_slice() {
            ["off"] => setup(Setup::Words(None)),
            [style] => setup(Setup::Words(Some((*style).to_string()))),
            _ => Err("words <style>|off".into()),
        },
        "theme" => match args.as_slice() {
            [fg, bg] => setup(Setup::Theme {
                fg: colour(fg)?,
                bg: colour(bg)?,
            }),
            _ => Err("theme <fg> <bg>".into()),
        },
        "panes" => match args.as_slice() {
            [n] => {
                let n: usize = number(n, "panes")?;
                if (1..=8).contains(&n) {
                    setup(Setup::Panes(n))
                } else {
                    Err(format!("panes {n}: 1 to 8"))
                }
            }
            _ => Err("panes <n>".into()),
        },
        "pane" => match args.as_slice() {
            [pane, "theme", fg, bg] => setup(Setup::PaneTheme {
                pane: number(pane, "pane")?,
                fg: colour(fg)?,
                bg: colour(bg)?,
            }),
            _ => Err("pane <i> theme <fg> <bg>".into()),
        },
        "focus-pane" => match args.as_slice() {
            [pane] => setup(Setup::FocusPane(number(pane, "focus-pane")?)),
            _ => Err("focus-pane <i>".into()),
        },
        "hz" => match args.as_slice() {
            [hz] => setup(Setup::Hz(positive(hz, "hz")?)),
            _ => Err("hz <n>".into()),
        },
        "ramp" => match args.as_slice() {
            ["never"] => setup(Setup::Ramp(Ramp::Never)),
            ["always"] => setup(Setup::Ramp(Ramp::Always)),
            _ => Err("ramp never|always".into()),
        },
        "page" => match args.as_slice() {
            ["opaque"] => setup(Setup::Page(Page::Opaque)),
            ["translucent"] => setup(Setup::Page(Page::Translucent)),
            ["wallpaper"] => setup(Setup::Page(Page::Wallpaper)),
            _ => Err("page opaque|translucent|wallpaper".into()),
        },
        "intensity" => match args.as_slice() {
            [x] => setup(Setup::Intensity(finite(x, "intensity")?)),
            _ => Err("intensity <x>".into()),
        },
        "live" => match args.as_slice() {
            ["off"] => setup(Setup::Live(Live::Off)),
            ["glass"] => setup(Setup::Live(Live::Glass)),
            ["lip"] => setup(Setup::Live(Live::Lip)),
            ["marks"] => setup(Setup::Live(Live::Marks)),
            ["vanish"] => setup(Setup::Live(Live::Vanish)),
            _ => Err("live off|glass|lip|marks|vanish".into()),
        },
        "at" => {
            let (ms, event) = rest
                .split_once(char::is_whitespace)
                .ok_or("at <ms> <event>")?;
            Ok(Stmt::At {
                ms: number(ms, "at ms")?,
                event: event_of(event.trim())?,
            })
        }
        "run" => match args.as_slice() {
            [until] => Ok(Stmt::Run {
                until_ms: number(until, "run until")?,
            }),
            _ => Err("run <until_ms>".into()),
        },
        other => Err(format!("unknown directive {other:?}")),
    }
}

/// Parse a timed event.
fn event_of(code: &str) -> Result<Event, String> {
    let (head, rest) = code
        .split_once(char::is_whitespace)
        .map_or((code, ""), |(head, rest)| (head, rest.trim()));
    let args: Vec<&str> = rest.split_whitespace().collect();
    match head {
        "pty" if rest.starts_with('"') => Ok(Event::Pty(quoted(rest)?)),
        "pty" => match args.as_slice() {
            [hex] => Ok(Event::Pty(hex_bytes(hex)?)),
            _ => Err("pty <hex|\"text\">".into()),
        },
        "key" if TrailRig::key_parses(rest) => Ok(Event::Key(args.join(" "))),
        "key" => Err(format!(
            "key {rest:?}: not a key the control socket's key verb takes"
        )),
        "env" => {
            let (mut amp, mut focus, mut motion) = (None, None, None);
            let mut words = args.iter();
            while let Some(&word) = words.next() {
                let value = words
                    .next()
                    .ok_or_else(|| format!("env {word} needs a value"))?;
                match word {
                    "amp" => amp = Some(finite(value, "env amp")?),
                    "focus" => {
                        focus = Some(match *value {
                            "0" => false,
                            "1" => true,
                            _ => return Err("env focus 0|1".into()),
                        });
                    }
                    "motion" => {
                        motion = Some(match *value {
                            "full" => Motion::Full,
                            "reduced" => Motion::Reduced,
                            "auto" => Motion::Auto,
                            _ => return Err("env motion full|reduced|auto".into()),
                        });
                    }
                    _ => return Err(format!("env: unknown key {word:?}")),
                }
            }
            if amp.is_none() && focus.is_none() && motion.is_none() {
                return Err("env amp <a> focus <0|1> motion full|reduced|auto".into());
            }
            Ok(Event::Env { amp, focus, motion })
        }
        "cost" => match args.as_slice() {
            [ns] => Ok(Event::Cost(number(ns, "cost ns")?)),
            _ => Err("cost <ns>".into()),
        },
        "pointer" => match args.as_slice() {
            ["off"] => Ok(Event::Pointer(None)),
            [x, y] => Ok(Event::Pointer(Some((
                finite(x, "pointer x")?,
                finite(y, "pointer y")?,
            )))),
            _ => Err("pointer <x> <y>|off".into()),
        },
        "press" => match args.as_slice() {
            [x, y] => Ok(Event::Press(finite(x, "press x")?, finite(y, "press y")?)),
            _ => Err("press <x> <y>".into()),
        },
        "select" => match args.as_slice() {
            ["off"] => Ok(Event::Select(None)),
            [r0, c0, r1, c1] => Ok(Event::Select(Some((
                (number(r0, "select r0")?, number(c0, "select c0")?),
                (number(r1, "select r1")?, number(c1, "select c1")?),
            )))),
            _ => Err("select <r0> <c0> <r1> <c1>|off".into()),
        },
        other => Err(format!("unknown event {other:?}")),
    }
}

fn number<T: std::str::FromStr>(s: &str, what: &str) -> Result<T, String> {
    s.parse()
        .map_err(|_| format!("{what}: {s:?} is not a number"))
}

fn positive<T: std::str::FromStr + PartialEq + Default>(s: &str, what: &str) -> Result<T, String> {
    let n: T = number(s, what)?;
    if n == T::default() {
        return Err(format!("{what}: {s} is not positive"));
    }
    Ok(n)
}

fn finite<T: std::str::FromStr + Into<f64> + Copy>(s: &str, what: &str) -> Result<T, String> {
    let x: T = number(s, what)?;
    if x.into().is_finite() {
        Ok(x)
    } else {
        Err(format!("{what}: {s} is not finite"))
    }
}

/// `#rrggbb` or `rrggbb`.
fn colour(s: &str) -> Result<u32, String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 {
        return Err(format!("{s:?} is not #rrggbb"));
    }
    u32::from_str_radix(hex, 16).map_err(|_| format!("{s:?} is not #rrggbb"))
}

/// Bytes as hex pairs.
fn hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    if s.is_empty() || !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("pty {s:?}: hex pairs or \"text\""));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// A `"…"` string's bytes, with `\n`, `\r`, `\t`, `\e` (ESC), `\\`, `\"` and `\xHH`.
fn quoted(s: &str) -> Result<Vec<u8>, String> {
    let body = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .ok_or_else(|| format!("{s}: one \"text\" and nothing after it"))?;
    let mut out = Vec::with_capacity(body.len());
    let mut bytes = body.bytes();
    while let Some(b) = bytes.next() {
        if b == b'"' {
            return Err(format!("{s}: an unescaped \" inside the text"));
        }
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(b'n') => out.push(b'\n'),
            Some(b'r') => out.push(b'\r'),
            Some(b't') => out.push(b'\t'),
            Some(b'e') => out.push(0x1b),
            Some(b'\\') => out.push(b'\\'),
            Some(b'"') => out.push(b'"'),
            Some(b'x') => {
                let pair = [bytes.next(), bytes.next()];
                let [Some(hi), Some(lo)] = pair else {
                    return Err(format!("{s}: \\x needs two hex digits"));
                };
                let hex = [hi, lo];
                let hex = std::str::from_utf8(&hex).map_err(|e| e.to_string())?;
                out.push(
                    u8::from_str_radix(hex, 16).map_err(|_| format!("{s}: \\x{hex} is not hex"))?,
                );
            }
            other => {
                return Err(format!(
                    "{s}: unknown escape \\{}",
                    other.map_or(String::new(), |b| char::from(b).to_string())
                ));
            }
        }
    }
    Ok(out)
}

/// What one dump prints beyond each frame's summary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Detail {
    /// `--full --frame <n>`: every element and word of frame `n`.
    pub(crate) full_frame: Option<u64>,
}

/// Run `trail_dump <args>`: the exit code, every dump on `out` and every error on `err`.
pub(crate) fn dump_main(args: &[String], out: &mut impl Write, err: &mut impl Write) -> i32 {
    const USAGE: &str = "usage: trail_dump [--full --frame <n>] <script.trail>...";
    let mut full = false;
    let mut detail = Detail::default();
    let mut paths = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--full" => full = true,
            "--frame" => {
                let Some(n) = args.next().and_then(|n| n.parse().ok()) else {
                    let _ = writeln!(err, "trail_dump: --frame <n>\n{USAGE}");
                    return EXIT_USAGE;
                };
                detail.full_frame = Some(n);
            }
            flag if flag.starts_with("--") => {
                let _ = writeln!(err, "trail_dump: unknown flag {flag}\n{USAGE}");
                return EXIT_USAGE;
            }
            path => paths.push(path),
        }
    }
    if full != detail.full_frame.is_some() || paths.is_empty() {
        let _ = writeln!(err, "{USAGE}");
        return EXIT_USAGE;
    }
    let mut scripts = Vec::with_capacity(paths.len());
    for path in paths {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                let _ = writeln!(err, "trail_dump: {path}: {error}");
                return EXIT_USAGE;
            }
        };
        match parse(&text) {
            Ok(script) => scripts.push((path, script)),
            Err(error) => {
                let _ = writeln!(err, "trail_dump: {path}: {error}");
                return EXIT_USAGE;
            }
        }
    }
    for (path, script) in scripts {
        if let Err(error) = dump(path, &script, detail, out) {
            let _ = writeln!(err, "trail_dump: {path}: {error}");
            return EXIT_FAILED;
        }
    }
    0
}

/// Dump every run of `script` (named `name`) to `out`.
pub(crate) fn dump(
    name: &str,
    script: &Script,
    detail: Detail,
    out: &mut impl Write,
) -> Result<(), String> {
    writeln!(out, "P {name}").map_err(|error| error.to_string())?;
    for (label, stmts) in script.runs() {
        let mut text = format!("S {label}\n");
        Runner::new(detail)?.run(&stmts, &mut text)?;
        out.write_all(text.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The config the setup directives build, admitted whole.
#[derive(Clone, Debug, PartialEq)]
struct Config {
    style: String,
    /// The pet's launch-kitty seed, when it is on.
    pet: Option<u64>,
    words: Option<bool>,
    /// `[sparkle_words.profanity] style`, when `words` names one.
    profanity: Option<&'static str>,
    scheme: Option<&'static str>,
    colours: Option<(u32, u32)>,
    intensity: Option<f32>,
    page: Option<Page>,
    motion: Motion,
}

impl Config {
    /// The owner's look: the rainbow kitty, its sounds on, motion full.
    fn new() -> Self {
        Self {
            style: "rainbow kitty".into(),
            pet: None,
            words: None,
            profanity: None,
            scheme: None,
            colours: None,
            intensity: None,
            page: None,
            motion: Motion::Full,
        }
    }

    /// The `aterm.toml` this config is.
    fn toml(&self) -> String {
        let mut t = String::from("trail_sounds = true\n");
        let style = match (self.pet, self.style.strip_suffix(" pet")) {
            (Some(_), None) => format!("{} pet", self.style),
            (None, Some(bare)) => bare.to_string(),
            _ => self.style.clone(),
        };
        let _ = writeln!(t, "cursor_trail_style = {style:?}");
        let motion = match self.motion {
            Motion::Full => "full",
            Motion::Reduced => "reduced",
            Motion::Auto => "auto",
        };
        let _ = writeln!(t, "motion = {motion:?}");
        if let Some(x) = self.intensity {
            let _ = writeln!(t, "cursor_trail_intensity = {x:?}");
        }
        match self.page {
            Some(Page::Opaque) => t.push_str("background_opacity = 1.0\n"),
            Some(Page::Translucent) => {
                let _ = writeln!(t, "background_opacity = {TRANSLUCENT_OPACITY:?}");
            }
            Some(Page::Wallpaper) | None => {}
        }
        if let Some(name) = self.scheme {
            let _ = writeln!(t, "theme = {name:?}");
        }
        if let Some((fg, bg)) = self.colours {
            let _ = writeln!(t, "foreground = \"#{fg:06x}\"\nbackground = \"#{bg:06x}\"");
        }
        if let Some(on) = self.words {
            let _ = writeln!(t, "[sparkle_words]\nenabled = {on}");
            if let Some(style) = self.profanity {
                let _ = writeln!(t, "[sparkle_words.profanity]\nstyle = {style:?}");
            }
        }
        t
    }
}

/// One run's rig and the setup it has applied and still owes.
struct Runner {
    rig: TrailRig,
    detail: Detail,
    config: Config,
    applied: Option<Config>,
    grid: Option<(u16, u16)>,
    grid_applied: Option<(u16, u16)>,
    panes: usize,
    focus_pane: Option<usize>,
    hz: Option<u32>,
    ramp: Option<Ramp>,
    /// Events not yet due, in time order (ties in script order).
    events: Vec<(u64, Event)>,
    /// Where the last run ended, in ms after `t0`.
    ran_to: u64,
    /// The next frame's instant after `t0`.
    next_frame: Duration,
    frame: u64,
}

impl Runner {
    fn new(detail: Detail) -> Result<Self, String> {
        Ok(Self {
            rig: TrailRig::new(std::path::Path::new(FIXTURE_FONT))?,
            detail,
            config: Config::new(),
            applied: None,
            grid: None,
            grid_applied: None,
            panes: 1,
            focus_pane: None,
            hz: None,
            ramp: None,
            events: Vec::new(),
            ran_to: 0,
            next_frame: Duration::ZERO,
            frame: 0,
        })
    }

    fn at(&self, after: Duration) -> std::time::Instant {
        self.rig.t0() + after
    }

    fn run(mut self, stmts: &[Stmt], out: &mut String) -> Result<(), String> {
        for stmt in stmts {
            match stmt {
                Stmt::Setup(setup) => self.setup(setup),
                Stmt::Scheme(scheme) => {
                    if let Scheme::Pack { id, text } = scheme {
                        self.rig.install_theme(id, text)?;
                    }
                    self.config.scheme = Some(scheme.name());
                }
                Stmt::At { ms, event } => {
                    let at = self.events.partition_point(|(due, _)| due <= ms);
                    self.events.insert(at, (*ms, event.clone()));
                }
                Stmt::Run { until_ms } => self.run_to(*until_ms, out)?,
            }
        }
        Ok(())
    }

    fn setup(&mut self, setup: &Setup) {
        match setup {
            Setup::Grid { rows, cols } => self.grid = Some((*rows, *cols)),
            Setup::Style(style) => self.config.style.clone_from(style),
            Setup::Pet(seed) => self.config.pet = *seed,
            Setup::Words(style) => {
                self.config.words = Some(style.is_some());
                self.config.profanity = PROFANITY_STYLES
                    .into_iter()
                    .find(|named| style.as_deref() == Some(*named));
            }
            Setup::Theme { fg, bg } => self.config.colours = Some((*fg, *bg)),
            Setup::Panes(n) => self.panes = *n,
            Setup::FocusPane(pane) => self.focus_pane = Some(*pane),
            Setup::Hz(hz) => self.hz = Some(*hz),
            Setup::Ramp(ramp) => self.ramp = Some(*ramp),
            Setup::Page(page) => self.config.page = Some(*page),
            Setup::Intensity(x) => self.config.intensity = Some(*x),
            // Recorded, not honoured: each variant's doc names the phase that wires it.
            Setup::Font(_) | Setup::PaneTheme { .. } | Setup::Live(_) => {}
        }
    }

    /// Apply what the setup changed since the last run, at `at`.
    fn apply_setup(&mut self, at: std::time::Instant) -> Result<(), String> {
        let config_changed = self.applied.as_ref() != Some(&self.config);
        if config_changed {
            if let Some(seed) = self.config.pet {
                self.rig.kitty_seed(seed);
            }
            self.rig.configure(&self.config.toml(), at)?;
            self.applied = Some(self.config.clone());
        }
        if self.grid != self.grid_applied
            && let Some((rows, cols)) = self.grid
        {
            self.rig.grid(rows, cols, at)?;
            self.grid_applied = self.grid;
        }
        if self.panes < self.rig.panes() {
            return Err(format!(
                "panes {}: the window already shows {}",
                self.panes,
                self.rig.panes()
            ));
        }
        while self.rig.panes() < self.panes {
            self.rig.split(at)?;
        }
        if let Some(pane) = self.focus_pane.take() {
            self.rig.focus_pane(pane, at)?;
        }
        if let Some(hz) = self.hz.take() {
            self.rig.panel_hz(hz);
        }
        if config_changed || self.ramp.is_some() {
            let ramp = self.ramp.map(|ramp| ramp == Ramp::Always);
            self.rig.ramp_cost(ramp);
        }
        Ok(())
    }

    fn run_to(&mut self, until_ms: u64, out: &mut String) -> Result<(), String> {
        self.apply_setup(self.at(Duration::from_millis(self.ran_to)))?;
        let until = Duration::from_millis(until_ms);
        while self.next_frame <= until {
            let now = self.next_frame;
            self.fire_until(now)?;
            self.draw(now, out)?;
            self.next_frame = now + self.rig.present_interval();
        }
        self.fire_until(until)?;
        self.ran_to = until_ms;
        Ok(())
    }

    /// Apply every event due by `now`, each at its own instant.
    fn fire_until(&mut self, now: Duration) -> Result<(), String> {
        let due = self
            .events
            .partition_point(|(ms, _)| Duration::from_millis(*ms) <= now);
        for (ms, event) in self.events.drain(..due).collect::<Vec<_>>() {
            let at = self.at(Duration::from_millis(ms));
            match event {
                Event::Pty(bytes) => self.rig.pty(&bytes, at),
                Event::Key(key) => self.rig.key(&key, at)?,
                Event::Env { amp, focus, motion } => {
                    // `amp` is recorded, not honoured until Phase 4's amplitude feed.
                    let _ = amp;
                    if let Some(motion) = motion {
                        self.config.motion = motion;
                        self.rig.configure(&self.config.toml(), at)?;
                        self.applied = Some(self.config.clone());
                    }
                    if let Some(focus) = focus {
                        self.rig.focus(focus, at);
                    }
                }
                Event::Cost(ns) => self.rig.present_cost(ns, at),
                Event::Pointer(to) => self.rig.pointer(to, at),
                Event::Press(x, y) => self.rig.press(x, y, at),
                Event::Select(span) => self.rig.select(span),
            }
        }
        Ok(())
    }

    fn draw(&mut self, after: Duration, out: &mut String) -> Result<(), String> {
        let n = self.frame;
        self.frame += 1;
        let now = self.at(after);
        let t = ms(after);
        let full = self.detail.full_frame == Some(n);
        let Some(frame) = self.rig.frame(now)? else {
            let _ = writeln!(out, "F {n} t={t} held");
            let reads = self.rig.reads(now);
            state(&reads.words, &reads.bed_ink, full, out);
            let _ = writeln!(out, "  light n=0");
            return Ok(());
        };
        let reads = self.rig.reads(now);
        let mut arms = String::new();
        for (arm, streams) in reads.arms.iter() {
            if !arms.is_empty() {
                arms.push(',');
            }
            let _ = write!(
                arms,
                "{arm}:{}",
                streams.names().collect::<Vec<_>>().join("+")
            );
        }
        if arms.is_empty() {
            arms.push('-');
        }
        // GlowConfig carries no light bits until Phase 1 (§15.0.4), so every frame's are 0.
        let _ = writeln!(out, "F {n} t={t} light=0 arms={arms}");
        self.streams(full, out);
        pixels(&frame, out);
        let t0 = self.rig.t0();
        let at = |deadline: Option<Instant>| {
            deadline.map_or_else(
                || "-".to_string(),
                |at| ms(at.saturating_duration_since(t0)),
            )
        };
        let _ = writeln!(
            out,
            "  deadline glow={} pet={} words={}",
            at(reads.glow_deadline),
            at(reads.pet_deadline),
            at(reads.words_deadline)
        );
        sounds(&frame, t0, full, out);
        state(&reads.words, &reads.bed_ink, full, out);
        // No engine writes light words until the phase that adds a light table (§15.1 P0.3).
        let _ = writeln!(out, "  light n=0");
        Ok(())
    }

    /// Each stream's length and hash: FNV-1a/64 over every element's words ([`stream_words`]),
    /// since a stream must equal main's word for word (§15.0.2(b)), in `Streams::NAMED`'s order.
    /// `cursor_ink` joins at Phase 3.
    fn streams(&self, full: bool, out: &mut String) {
        use stream_words as sw;
        let input = self.rig.input();
        let mut s = StreamLine::new(self.rig.t0(), full);
        s.put("glow_under", &input.glow_under, sw::glow_quad);
        s.put(
            "glow_under_ramps",
            std::iter::once(&input.glow_under_ramps),
            sw::glow_ramps,
        );
        s.put("cursor_glow_add", &input.cursor_glow_add, sw::glow_quad);
        s.put(
            "cursor_glow_add_ramps",
            std::iter::once(&input.cursor_glow_add_ramps),
            sw::glow_ramps,
        );
        s.put("glow_halo", &input.glow_halo, sw::rain_halo);
        s.put("fx_layers", &input.fx_layers, sw::fx_layer);
        s.put(
            "word_decorations",
            &input.word_decorations,
            sw::word_decoration,
        );
        s.put(
            "cursor_fill_override",
            &input.cursor_fill_override,
            |w, &v| {
                sw::whole(w, v);
            },
        );
        s.put("ink", &input.ink, sw::ink_cell);
        s.put("nova_add", &input.nova_add, sw::glow_quad);
        s.put("char_fg", &input.char_fg, sw::char_fg);
        s.put("fire_halo", &input.fire_halo, sw::fire_halo_cell);
        s.put("rain_add", &input.rain_add, sw::rain_halo);
        s.put("cursor_trail", &input.cursor_trail, sw::trail_cell);
        s.put("fire_patch", &input.fire_patch, sw::fire_patch);
        s.put(
            "bloom_gate",
            std::iter::once(&input.bloom_gate),
            |w, &gate| {
                w.put("gate", gate as u64);
            },
        );
        s.put(
            "cursor_effect_style_override",
            &input.cursor_effect_style_override,
            |w, &style| w.put("style", style as u64),
        );
        s.put(
            "cursor_trail_color",
            std::iter::once(&input.cursor_trail_color),
            |w, &v| {
                sw::whole(w, v);
            },
        );
        s.finish(out);
    }
}

/// The `streams` line as a frame builds it, and its `W` lines under `--full`.
struct StreamLine {
    t0: Instant,
    full: bool,
    line: String,
    words: String,
}

impl StreamLine {
    fn new(t0: Instant, full: bool) -> Self {
        Self {
            t0,
            full,
            line: String::from("  streams"),
            words: String::new(),
        }
    }

    /// One stream: how many elements it holds and the fold of their words, each element's
    /// written by `each`.
    fn put<'a, T: 'a>(
        &mut self,
        name: &str,
        items: impl IntoIterator<Item = &'a T>,
        each: impl Fn(&mut StateWords, &T),
    ) {
        let mut stream = Fold::new(self.t0);
        let mut len = 0_u64;
        for (i, item) in items.into_iter().enumerate() {
            let mut words = StateWords::new(self.t0);
            each(&mut words, item);
            stream.group("item", |f| {
                for &(n, v) in words.words() {
                    f.put(n, v);
                }
            });
            if self.full {
                let _ = write!(self.words, "  W {name}[{i}]");
                put_words(&mut self.words, &words);
            }
            len += 1;
        }
        stream.put("len", len);
        let _ = write!(self.line, " {name}={len}:{:016x}", stream.finish());
    }

    fn finish(mut self, out: &mut String) {
        self.line.push('\n');
        out.push_str(&self.line);
        out.push_str(&self.words);
    }
}

/// `words` as ` <name>=<hex>` pairs, then the line's end.
fn put_words(out: &mut String, words: &StateWords) {
    for &(n, v) in words.words() {
        let _ = write!(out, " {n}={v:x}");
    }
    out.push('\n');
}

/// The CPU frame's size and FNV-1a/64 over its pixels, little-endian.
fn pixels(frame: &TrailFrame, out: &mut String) {
    let h = frame.pixels.iter().fold(aterm_hash::FNV64_OFFSET, |h, &p| {
        aterm_hash::fnv1a_64_extend(h, &p.to_le_bytes())
    });
    let _ = writeln!(out, "  px {}x{}:{h:016x}", frame.width, frame.height);
}

/// The sound cues the frame pushed: how many and the fold of their words
/// ([`aterm_effects::state_words::sound_event_words`]).
fn sounds(frame: &TrailFrame, t0: Instant, full: bool, out: &mut String) {
    if frame.sounds.is_empty() {
        out.push_str("  sounds -\n");
        return;
    }
    let mut fold = Fold::new(t0);
    for cue in &frame.sounds {
        fold.group("cue", |f| {
            aterm_effects::state_words::sound_event_words(f, cue)
        });
    }
    let _ = writeln!(
        out,
        "  sounds {}:{:016x}",
        frame.sounds.len(),
        fold.finish()
    );
    if full {
        for (i, cue) in frame.sounds.iter().enumerate() {
            let mut words = StateWords::new(t0);
            aterm_effects::state_words::sound_event_words(&mut words, cue);
            let _ = write!(out, "  W sound[{i}]");
            put_words(out, &words);
        }
    }
}

/// The state words, folded per engine (the name before a word's first `.`) in the order the
/// engines wrote them, then the bed's ink table's words as `bed`.
fn state(words: &StateWords, bed: &BedInkLut, full: bool, out: &mut String) {
    let mut groups: Vec<(&str, u64)> = Vec::new();
    for &(name, v) in words.words() {
        let engine = name.split_once('.').map_or(name, |(engine, _)| engine);
        let h = match groups.iter_mut().find(|(e, _)| *e == engine) {
            Some((_, h)) => h,
            None => {
                groups.push((engine, aterm_hash::FNV64_OFFSET));
                &mut groups.last_mut().expect("just pushed").1
            }
        };
        *h = aterm_hash::fnv1a_64_extend(*h, name.as_bytes());
        *h = aterm_hash::fnv1a_64_extend(*h, &v.to_le_bytes());
    }
    let mut bed_fold = Fold::new(words.t0());
    bed.state_words(&mut bed_fold);
    let _ = write!(out, "  state n={}", words.words().len());
    for (engine, h) in &groups {
        let _ = write!(out, " {engine}={h:016x}");
    }
    let _ = writeln!(out, " bed={:016x}", bed_fold.finish());
    if full {
        for &(name, v) in words.words() {
            let _ = writeln!(out, "  W state {name} {v:016x}");
        }
    }
}

/// `after` in milliseconds, to the microsecond.
fn ms(after: Duration) -> String {
    let us = after.as_micros();
    format!("{}.{:03}", us / 1000, us % 1000)
}
