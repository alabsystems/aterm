// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-gallery`: photographs of cursor trail styles, one fresh private headless aterm per style, theme and scenario, through [`crate::instance`]'s machinery (a scratch HOME and XDG dirs, the automatic lanes off, a private control socket, a lifeline; ended by its process group, never by name).

use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::RelayClient;
use aterm_uds::CtlStream;

use crate::instance::{
    self, Instance, Launch, Reply, World, count, take_flag, take_value, take_values,
};
use crate::paths::{from_caller, paths};
use crate::stop::{self, Stopped};
use crate::{Fail, Res};

/// The verb's own help (`trail-gallery --help`).
pub const HELP: &str = "\
usage: targo --unverified lab trail-gallery [--aterm PATH] [--theme dark|light|both] [--scenario NAME[,NAME]]...
         [--extra 'key = value']... [--columns N] [--lines N] [--font-px N] [--no-clean] [--keep] <out_dir> [style ...]

Photograph cursor trail styles: one fresh private headless aterm per style, theme and scenario (a scratch HOME and XDG
dirs, a private control socket, a lifeline; ended by its process group, never by name), a frame after every key
(headless advances the effects only when a frame renders), and each frame's sidecars for `trail-contrast`.

  <out_dir>     where the frames go, OUTSIDE the repository (refused inside this checkout or any of its worktrees);
                made if missing; same-named files are replaced
  style ...     trail styles, e.g. phaser \"rainbow kitty pet\" off; none: every style the Settings picker offers, read
                from a probe instance's `controls prefs` (an alias such as \"nyan rainbow\" or \"classic mono\"
                is not on it; name it to capture it)
  --aterm PATH  the aterm to run (default: the lab's pick, `targo --unverified lab paths`); a release build keeps the
                60 ms cadence (`targo --unverified build --release -p aterm`, then --aterm target/release/aterm)
  --theme       dark (the default theme), light (\"GitHub Light\") or both (the default)
  --scenario    typing (default), burst, context, steam, enter-bottom, idle10, redraw-stall, fold-rest, cjk, bidi,
                split, move-up, focus-roundtrip, settings, or all; repeatable
  --extra       a top-level aterm.toml line for every instance, replacing the base line of its key (the base lines:
                the style, motion = \"full\", trail_sounds = false, and on light the theme)
  --columns --lines --font-px   the grid (default 72x14 cells of 22 px; settings 110x40 of 16 px)
  --no-clean    skip each frame's `image clean` twin
  --keep        retain every instance's scratch world, including a roster probe's

Per instance <name> = <style-slug>-<theme>[-<scenario>] (typing has no suffix): <name>.<frame>.png and its
.clean.png, .cursor.txt and .text.txt; <name>.frames.tsv (frame, ms since the instance came up, @event rows);
<name>.dims.txt, .status.txt, .status-end.txt (`trail status`), .admissions.txt (`trail 40`), .gui.log; and
gallery.pids. Each terminal frame also keeps its .status.txt and image metadata. The open `redraw-stall`
frame has .read-notes.txt instead of cursor/text reads, which would wait for the very frame the fixture
has not yet closed. Its restored frame judges the final instance configuration (including --extra):
a requested rainbow that becomes unavailable is inconclusive, not a skip. Deliberately disabled
or other trails are not applicable. An inconclusive return is retained and exits 1, not counted
as a continuity pass.

exit: 0 every frame taken and every instance ended; 1 a frame or an instance failed, a redraw return was
inconclusive, or an instance outlived the run;
2 usage; 3 not run (no aterm, no roster)
";

/// The two themes a gallery photographs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    /// The default theme.
    Dark,
    /// "GitHub Light".
    Light,
}

impl Theme {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }

    /// The theme's own selection colour as an SGR truecolour triple
    /// (aterm-types scheme.rs: the renderer default #33415e, GitHub Light
    /// #b6e3ff): `context` PAINTS its selected row, since a keypress clears a
    /// real selection.
    const fn selection(self) -> &'static str {
        match self {
            Theme::Dark => "51;65;94",
            Theme::Light => "182;227;255",
        }
    }
}

/// What one instance is made to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Typing,
    /// A sustained line typed fast and long enough to earn the flying
    /// kitty's head ([`BURST_TEXT`] a key per [`BURST_KEY_MS`]): the run's
    /// start ([`BURST_START_KEYS`], `key3` and `key10`), `fly` while it flies
    /// (key [`BURST_FLY_KEY`]), `t0` at the last key, then [`BURST_AFTER_MS`]
    /// (`t120`, `t470` as the cat waits at the wall, `l1650` as its hop off
    /// the line's last letters lands, and `l3600` to `l4500` as it rests).
    Burst,
    Context,
    Steam,
    EnterBottom,
    Idle10,
    RedrawStall,
    /// Focus reports paint status away from an empty composer before typing resumes.
    FocusRoundtrip,
    FoldRest,
    Cjk,
    Bidi,
    /// Two panes, the caret's focused ([`SPLIT_PANES`]): the typing line
    /// (`mid`, `t0`, `t120`, `t470`), the line killed, then the burst's line
    /// on the same prompt ([`SPLIT_BURST_FRAMES`]), so the composed frames
    /// of a split window and the pet's veil inside its pane are photographed.
    Split,
    /// A program moves the caret up across lettered rows, as vim's `k` does
    /// ([`MOVE_UP_ROWS`]): `typed`, then `up` on the frame that first draws
    /// the move, `u120` and `u470`.
    MoveUp,
    Settings,
}

impl Scenario {
    /// Every scenario, in the order `all` runs them.
    pub const ALL: [Scenario; 14] = [
        Scenario::Typing,
        Scenario::Burst,
        Scenario::Context,
        Scenario::Steam,
        Scenario::EnterBottom,
        Scenario::Idle10,
        Scenario::RedrawStall,
        Scenario::FocusRoundtrip,
        Scenario::FoldRest,
        Scenario::Cjk,
        Scenario::Bidi,
        Scenario::Split,
        Scenario::MoveUp,
        Scenario::Settings,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Scenario::Typing => "typing",
            Scenario::Burst => "burst",
            Scenario::Context => "context",
            Scenario::Steam => "steam",
            Scenario::EnterBottom => "enter-bottom",
            Scenario::Idle10 => "idle10",
            Scenario::RedrawStall => "redraw-stall",
            Scenario::FocusRoundtrip => "focus-roundtrip",
            Scenario::FoldRest => "fold-rest",
            Scenario::Cjk => "cjk",
            Scenario::Bidi => "bidi",
            Scenario::Split => "split",
            Scenario::MoveUp => "move-up",
            Scenario::Settings => "settings",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Scenario> {
        Scenario::ALL.into_iter().find(|x| x.name() == s)
    }

    /// Whether its frames are of the terminal (they get the per-frame sidecars).
    fn terminal(self) -> bool {
        self != Scenario::Settings
    }

    /// Its default grid: columns, lines, font px.
    fn grid(self) -> (u32, u32, u32) {
        if self == Scenario::Settings {
            (110, 40, 16)
        } else {
            (72, 14, 22)
        }
    }
}

/// What `trail-gallery` was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub out: String,
    pub styles: Vec<String>,
    pub themes: Vec<Theme>,
    pub scenarios: Vec<Scenario>,
    pub extras: Vec<String>,
    pub columns: Option<u32>,
    pub lines: Option<u32>,
    pub font_px: Option<u32>,
    pub clean: bool,
    pub keep: bool,
    pub aterm: Option<String>,
}

fn usage(msg: impl AsRef<str>) -> Fail {
    Fail::usage(format!("trail-gallery: {}", msg.as_ref()))
}

/// A TOML line's key: the text before its first `=`, white space dropped.
fn toml_key(line: &str) -> String {
    line.split('=')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// The top-level keys the scratch world's own `aterm.toml` sets.
fn world_keys() -> Vec<String> {
    instance::CONFIG_OFF
        .lines()
        .take_while(|l| !l.trim_start().starts_with('['))
        .filter(|l| l.contains('='))
        .map(toml_key)
        .collect()
}

fn check_extra(line: &str) -> Result<(), Fail> {
    if !line.contains('=') {
        return Err(usage(format!(
            "--extra wants a top-level 'key = value' line, not '{line}'"
        )));
    }
    if line.trim_start().starts_with('[') {
        return Err(usage(format!(
            "--extra takes top-level keys, not a table: '{line}'"
        )));
    }
    if line.contains(['\n', '\r']) {
        return Err(usage(format!("--extra takes one line, not '{line}'")));
    }
    let key = toml_key(line);
    if world_keys().contains(&key) {
        return Err(usage(format!(
            "--extra cannot set {key}: the scratch world's own aterm.toml sets it"
        )));
    }
    Ok(())
}

fn check_style(style: &str) -> Result<(), Fail> {
    if style.is_empty() || style.contains(['"', '\\']) || style.chars().any(char::is_control) {
        return Err(usage(format!(
            "a style is a name with no quote, backslash or control character, not '{style}'"
        )));
    }
    Ok(())
}

/// Read `trail-gallery`'s arguments.
/// # Errors
/// A usage error (exit 2).
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let aterm = take_value(&mut a, "--aterm")?;
    let themes = match take_value(&mut a, "--theme")?.as_deref().unwrap_or("both") {
        "dark" => vec![Theme::Dark],
        "light" => vec![Theme::Light],
        "both" => vec![Theme::Dark, Theme::Light],
        other => {
            return Err(usage(format!(
                "--theme takes dark, light or both, not '{other}'"
            )));
        }
    };
    let mut scenarios: Vec<Scenario> = Vec::new();
    for spec in take_values(&mut a, "--scenario")? {
        for name in spec.split(',').filter(|n| !n.is_empty()) {
            let named = if name == "all" {
                Scenario::ALL.to_vec()
            } else {
                vec![Scenario::parse(name).ok_or_else(|| {
                    let all: Vec<&str> = Scenario::ALL.iter().map(|s| s.name()).collect();
                    usage(format!(
                        "unknown scenario '{name}' (one of: {}, all)",
                        all.join(", ")
                    ))
                })?]
            };
            for s in named {
                if !scenarios.contains(&s) {
                    scenarios.push(s);
                }
            }
        }
    }
    if scenarios.is_empty() {
        scenarios.push(Scenario::Typing);
    }
    let extras = take_values(&mut a, "--extra")?;
    for e in &extras {
        check_extra(e)?;
    }
    let mut dim = |flag: &str| -> Result<Option<u32>, Fail> {
        take_value(&mut a, flag)?
            .map(|v| count(flag, &v))
            .transpose()
    };
    let (columns, lines, font_px) = (dim("--columns")?, dim("--lines")?, dim("--font-px")?);
    let clean = !take_flag(&mut a, "--no-clean");
    let keep = take_flag(&mut a, "--keep");
    if let Some(flag) = a.iter().find(|x| x.starts_with('-') && x.len() > 1) {
        return Err(usage(format!(
            "unknown option '{flag}' (trail-gallery --help)"
        )));
    }
    let Some((out, styles)) = a.split_first() else {
        return Err(usage(
            "name an output directory outside the repository (trail-gallery --help)",
        ));
    };
    for s in styles {
        check_style(s)?;
    }
    Ok(Args {
        out: out.clone(),
        styles: styles.to_vec(),
        themes,
        scenarios,
        extras,
        columns,
        lines,
        font_px,
        clean,
        keep,
        aterm,
    })
}

/// The top-level `aterm.toml` lines of one instance: the base lines, less any key an `--extra` line sets, then the extras.
#[must_use]
pub fn config_lines(style: &str, theme: Theme, extras: &[String]) -> Vec<String> {
    let mut lines = vec![
        format!("cursor_trail_style = \"{style}\""),
        "motion = \"full\"".to_string(),
        "trail_sounds = false".to_string(),
    ];
    if theme == Theme::Light {
        lines.push("theme = \"GitHub Light\"".to_string());
    }
    lines.retain(|l| !extras.iter().any(|e| toml_key(e) == toml_key(l)));
    lines.extend(extras.iter().cloned());
    lines
}

/// A style's file-name form: lower case, a space or `:` a `-`, nothing else but `[a-z0-9-]`.
#[must_use]
pub fn slug(style: &str) -> String {
    style
        .chars()
        .filter_map(|c| match c {
            ' ' | ':' => Some('-'),
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
            '-' => Some('-'),
            _ => None,
        })
        .collect()
}

/// `<style-slug>-<theme>[-<scenario>]`.
#[must_use]
pub fn instance_name(style: &str, theme: Theme, scenario: Scenario) -> String {
    let base = format!("{}-{}", slug(style), theme.name());
    if scenario == Scenario::Typing {
        base
    } else {
        format!("{base}-{}", scenario.name())
    }
}

const PAINT_HEAD: &str = r#"LC_ALL=en_US.UTF-8; export LC_ALL
PS1='$ '; export PS1
cols=$(stty size 2>/dev/null | cut -d" " -f2); cols=${cols:-72}
row() { printf "%-${cols}s" "$1"; }
"#;

const PAINT_CONTEXT: &str = r#"printf '\033[2J\033[3H'
printf 'Above: Typography jumps; quick brown gyps (qwerty) _gy|\r\n'
printf '\033[30;42m ok \033[97;44m ~/aterm \033[30;43m main +2 \033[0;48;5;236m\033[K\033[0m\r\n'
printf '\r\n'
printf '\033[48;2;@SEL@m'; row 'src/trail.rs:412  a selected line under the prompt'; printf '\033[0m\r\n'
printf 'Below: every glyph here must stay legible! pqgyj {braces} [ok]\r\n'
printf '\033[31merror\033[0m: \033[1mcolour contrast\033[0m below the caret path\r\n'
printf '\033[5H'
on=$(printf '\033[30;46m'); off=$(printf '\033[0m')
# bash (macOS /bin/sh) needs \[…\] around a prompt's escapes to count its width; a plain sh prints them.
if [ -n "${BASH_VERSION:-}" ]; then PS1="\[$on\] aterm \[$off\] "; else PS1="$on aterm $off "; fi
export PS1
"#;

/// `move-up`'s page: lettered rows above the prompt, and `./k`, the
/// program that, a second after its Return, moves the caret up
/// [`MOVE_UP_ROWS`] rows into them and holds it there, as vim's `k` does.
/// It writes [`MOVE_UP_MARK`] on its row in the same write as the move, the
/// mark the gallery awaits (`@UP@` is the move's row count).
const PAINT_MOVE_UP: &str = r#"printf '\033[2J\033[H'
printf 'Above: Typography jumps; quick brown gyps (qwerty) _gy|\r\n'
printf 'fn main() {\r\n'
printf '    let fox = "the quick brown fox";\r\n'
printf '    println!("{fox} jumps over the lazy dog");\r\n'
printf '    // every glyph here must stay legible: pqgyj {braces} [ok]\r\n'
printf '}\r\n'
printf '\033[8H'
cat > ./k <<'K'
sleep 1
printf '[vim] NORMAL\033[@UP@A\033[12G'
sleep 4
K
chmod +x ./k
"#;

/// How many rows `move-up`'s program moves the caret up ([`PAINT_MOVE_UP`]):
/// from the row under its prompt to `println!`'s, past the comment, the
/// brace, a blank row and the prompt.
pub const MOVE_UP_ROWS: u16 = 5;

/// The mark `move-up`'s program writes with its move ([`PAINT_MOVE_UP`]).
pub const MOVE_UP_MARK: &str = "NORMAL";

/// The `split` scenario's panes: the window split top over bottom (`spawn
/// split=h`), the focus moved back up to the caret's pane (`pane up`), so
/// the burst's whole line still fits its row.
pub const SPLIT_PANES: [&str; 2] = ["split=h", "up"];

/// The `split` scenario's burst frames after its typing frames: `bfly` at
/// [`BURST_FLY_KEY`], `b0` at the last key, then `b120`, `b470` and `b1650`,
/// as the cat's hop off the line's last letters lands on its top
/// ([`SPLIT_BURST_AFTER_MS`]).
pub const SPLIT_BURST_FRAMES: [&str; 5] = ["bfly", "b0", "b120", "b470", "b1650"];

/// The ms after the last key of `split`'s `b120`, `b470` and `b1650`.
pub const SPLIT_BURST_AFTER_MS: [u64; 3] = [120, 470, 1650];

const PAINT_STEAM: &str = r"printf '\033[2J\033[3H'
printf 'Above: Typography jumps; quick brown gyps (qwerty) _gy|\r\n'
printf 'Above2: The lazy dog jogged by, every glyph legible.\r\n'
printf '\r\n'
printf 'Below: pqgyj Wide test {braces} [ok]\r\n'
printf '\033[5H'
";

/// The PTY's script: a locale for wide input, a known prompt, then the scenario's paint, then an interactive shell.
#[must_use]
pub fn paint(scenario: Scenario, theme: Theme) -> String {
    let body = match scenario {
        Scenario::Context => PAINT_CONTEXT.replace("@SEL@", theme.selection()),
        Scenario::Steam => PAINT_STEAM.to_string(),
        Scenario::EnterBottom => "seq 1 40\n".to_string(),
        Scenario::MoveUp => PAINT_MOVE_UP.replace("@UP@", &MOVE_UP_ROWS.to_string()),
        _ => "printf '\\033[2J\\033[5H'\n".to_string(),
    };
    format!("{PAINT_HEAD}{body}exec /bin/sh -i\n")
}

/// One `key` name per character of a word (a space is `space`).
fn keys_of(word: &str) -> Vec<String> {
    word.chars()
        .map(|c| {
            if c == ' ' {
                "space".to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// The line `typing`, `context` and `steam` type, a key per
/// [`TYPING_KEY_MS`] unless a frame is slower.
pub const TYPING_TEXT: &str = "echo the quick brown fox";
/// See [`TYPING_TEXT`].
pub const TYPING_KEY_MS: u64 = 60;

/// The `burst` scenario's line: the longest a 72-column row holds after the
/// prompt with the caret still on it.
pub const BURST_TEXT: &str = "echo the quick brown fox jumps over the lazy dog as the kitty flies";

/// The `burst` scenario's pace, ms a key: a fast typist's, under the 60 ms
/// of every other burst. The flying kitty's head is earned by the typing
/// momentum (`aterm_effects::typing_momentum`, the cat's band held for its
/// dwell over at least its run of keys, `aterm_effects::kitty_cursor`): at
/// this pace about 2.2 s of the line, at any pace under 250 ms a key about
/// as long, so a slow frame only spends more keys on it.
pub const BURST_KEY_MS: u64 = 50;

/// The key (0-based, of [`BURST_TEXT`]) after which `burst` keeps its `fly`
/// frame: past the summon and the cat's fade in, at this pace or slower.
pub const BURST_FLY_KEY: usize = 56;

/// The keys (0-based, of [`BURST_TEXT`]) after which `burst` keeps a frame of
/// its run's start, `key<n>`: the first letters, `ech` and `echo the q`,
/// typed where a resting companion stood (looks8 round 2 caught the cat's
/// body on them).
pub const BURST_START_KEYS: [usize; 2] = [3, 10];

/// A frame label of `burst` after its key `key` ([`BURST_START_KEYS`]).
#[must_use]
pub fn burst_start_frame(key: usize) -> String {
    format!("key{key}")
}

/// The `burst` frames after its last key, ms: the cat waiting at the wall
/// for a wrap the hand may still type (`t120`, `t470`), its hop off the
/// line's last letters at its sit landing over the line (`l1650`, where
/// looks8 round 2 caught a wrap bound coming down through the line's
/// letters before r23 K6 kept the cat on the ground for a wrap that never
/// comes), then its rest (`l<ms>`) as the momentum decays to the cat's
/// grounding (about 3.9 s from a full run) and its exit fades it out.
pub const BURST_AFTER_MS: [u64; 7] = [120, 470, 1650, 3600, 3900, 4200, 4500];

/// A frame label of `burst` for `ms` after its last key ([`BURST_AFTER_MS`]).
#[must_use]
pub fn burst_frame(ms: u64) -> String {
    if ms < 1000 {
        format!("t{ms}")
    } else {
        format!("l{ms}")
    }
}

/// Of a `panes` reply's panes (`pane session=… rect=<row>,<col>,<rows>x<cols>
/// focused=<bool>`), in reading order (top to bottom, left to right), the
/// place of the focused one; `None` unless there are two or more, one of
/// them focused.
#[must_use]
pub fn panes_focus(reply: &str) -> Option<usize> {
    let mut panes: Vec<((u32, u32), bool)> = reply
        .lines()
        .filter(|l| l.starts_with("pane "))
        .filter_map(|l| {
            let field = |key: &str| {
                l.split_whitespace()
                    .find_map(|w| w.strip_prefix(key))
                    .map(str::to_string)
            };
            let rect = field("rect=")?;
            let mut at = rect.split(',');
            let row = at.next()?.parse().ok()?;
            let col = at.next()?.parse().ok()?;
            Some(((row, col), field("focused=")? == "true"))
        })
        .collect();
    if panes.len() < 2 {
        return None;
    }
    panes.sort_by_key(|&(at, _)| at);
    panes.iter().position(|&(_, focused)| focused)
}

/// A window's `dims` reply narrowed to the focused pane of a `panes`
/// reply: its rows and columns, and its grid origin moved to the pane's
/// (`head` and `pad` grow by the pane's row and column times the cell), so
/// `trail-contrast` reads the caret's pane, its caret and its text where
/// they are drawn; `None` unless the window has two or more panes, one
/// focused, and the dims name the cell. Judged on the window's grid, a
/// split's pane header put every row a row low: the caret was read on the
/// page above it (1.47:1 on dark, `off` included) and the typed line on the
/// row above its letters (looks8 round 1).
#[must_use]
pub fn pane_dims(dims: &str, panes: &str) -> Option<String> {
    panes_focus(panes)?;
    let (row, col, rows, cols) = panes
        .lines()
        .filter(|l| l.starts_with("pane ") && l.split_whitespace().any(|w| w == "focused=true"))
        .find_map(|l| {
            let rect = l.split_whitespace().find_map(|w| w.strip_prefix("rect="))?;
            let (at, size) = rect.rsplit_once(',')?;
            let (row, col) = at.split_once(',')?;
            let (rows, cols) = size.split_once('x')?;
            Some((
                row.parse::<i64>().ok()?,
                col.parse::<i64>().ok()?,
                rows.parse::<i64>().ok()?,
                cols.parse::<i64>().ok()?,
            ))
        })?;
    let mut tokens: Vec<String> = dims.split_whitespace().map(str::to_string).collect();
    if tokens.len() < 5 || tokens[0] != "OK" {
        return None;
    }
    let fact = |key: &str| {
        tokens[5..]
            .iter()
            .find_map(|t| t.strip_prefix(key)?.strip_prefix('=')?.parse::<i64>().ok())
    };
    let (cw, ch) = (fact("cell_w")?, fact("cell_h")?);
    let (pad, head) = (fact("pad")?, fact("head").unwrap_or(0));
    tokens[1] = rows.to_string();
    tokens[2] = cols.to_string();
    let mut has_head = false;
    for t in &mut tokens[5..] {
        if t.starts_with("pad=") {
            *t = format!("pad={}", pad + col * cw);
        } else if t.starts_with("head=") {
            *t = format!("head={}", head + row * ch);
            has_head = true;
        }
    }
    if !has_head {
        tokens.push(format!("head={}", row * ch));
    }
    Some(tokens.join(" "))
}

/// The `burst` scenario's keys (one per character of [`BURST_TEXT`]).
#[must_use]
pub fn burst_keys() -> Vec<String> {
    keys_of(BURST_TEXT)
}

/// The gallery's private PTY child: a stalled synchronized redraw, or bare echo for the fold probe.
pub fn cmd_redraw_fixture(args: &[String]) -> Res {
    use std::io::Read as _;

    let fold = args == ["fold"];
    let focus = args == ["focus"];
    if !args.is_empty() && !fold && !focus {
        return Err(Fail::usage(
            "the redraw fixture takes only an optional fold or focus mode",
        ));
    }
    let _raw = crate::sys::RawMode::enter(0)
        .ok_or_else(|| Fail::usage("the redraw fixture needs its private PTY"))?;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let io_error = |e| Fail::usage(format!("redraw fixture: {e}"));
    output
        .write_all(if focus {
            b"\x1b[?1049h\x1b[?1004h\x1b[2J\x1b[8;1Hfocus ready\x1b[5;1H> "
        } else if fold {
            b"\x1b[2J\x1b[8;1Hfold ready\x1b[5;1H> "
        } else {
            b"\x1b[2J\x1b[8;1Hredraw ready\x1b[5;1H> "
        })
        .map_err(io_error)?;
    output.flush().map_err(io_error)?;
    let mut text = String::new();
    let mut redraw_open = false;
    let mut restored = false;
    let mut key = [0u8; 1];
    while input.read_exact(&mut key).is_ok() {
        if focus && key[0] == 27 {
            let mut report = [0u8; 2];
            input.read_exact(&mut report).map_err(io_error)?;
            let label = match report {
                [b'[', b'I'] => "in",
                [b'[', b'O'] => "out",
                _ => continue,
            };
            write!(
                output,
                "\x1b[?2026h\x1b[8;1H\x1b[2Kfocus {label}\x1b[5;{}H\x1b[?2026l",
                text.len() + 3
            )
            .map_err(io_error)?;
            output.flush().map_err(io_error)?;
            continue;
        }
        if fold {
            match key[0] {
                3 | 4 => break,
                0x20..=0x7e => {
                    output.write_all(&key).map_err(io_error)?;
                    output.flush().map_err(io_error)?;
                }
                _ => {}
            }
            continue;
        }
        let redraw = key[0] == 7 && !restored && text == TYPING_TEXT;
        match key[0] {
            3 | 4 => break,
            7 if redraw => {}
            0x20..=0x7e if text.len() < 80 => text.push(char::from(key[0])),
            _ => continue,
        }
        if focus {
            output.write_all(&key).map_err(io_error)?;
            output.flush().map_err(io_error)?;
        } else if !redraw {
            write!(
                output,
                "\x1b[?2026h\x1b[?25l\x1b[5;1H\x1b[2K> {text}\x1b[5;{}H\x1b[?25h\x1b[?2026l",
                text.len() + 3
            )
            .map_err(io_error)?;
            output.flush().map_err(io_error)?;
        }
        if redraw && !redraw_open {
            output
                .write_all(b"\x1b[?2026h\x1b[?25l\x1b[5;9H\x1b[K")
                .map_err(io_error)?;
            output.flush().map_err(io_error)?;
            redraw_open = true;
        } else if redraw {
            write!(
                output,
                "\x1b[5;1H> {text}\x1b[8;1Hredraw restored\x1b[5;{}H\x1b[?25h\x1b[?2026l",
                text.len() + 3
            )
            .map_err(io_error)?;
            output.flush().map_err(io_error)?;
            restored = true;
        }
    }
    Ok(0)
}

/// `p` relative to `base` when it is not absolute (git's own files).
fn joined(base: &Path, p: &str) -> PathBuf {
    if p.starts_with('/') {
        PathBuf::from(p)
    } else {
        base.join(p)
    }
}

/// Every checkout of the repository `repo` belongs to: `repo`, the main worktree and each linked worktree git records (`worktrees/*/gitdir`), read from git's own files.
#[must_use]
pub fn checkouts(repo: &Path) -> Vec<PathBuf> {
    let mut out = vec![repo.to_path_buf()];
    let dotgit = repo.join(".git");
    let common = if dotgit.is_dir() {
        Some(dotgit)
    } else {
        std::fs::read_to_string(&dotgit)
            .ok()
            .and_then(|t| {
                let g = t.trim().strip_prefix("gitdir:")?.trim().to_string();
                Some(joined(repo, &g))
            })
            .and_then(|gitdir| {
                let c = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
                Some(joined(&gitdir, c.trim()))
            })
    };
    let Some(common) = common.and_then(|c| std::fs::canonicalize(c).ok()) else {
        return out;
    };
    if common.file_name().is_some_and(|n| n == ".git")
        && let Some(main) = common.parent()
    {
        out.push(main.to_path_buf());
    }
    if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
        for e in entries.flatten() {
            if let Ok(g) = std::fs::read_to_string(e.path().join("gitdir"))
                && let Some(wt) = joined(&e.path(), g.trim()).parent()
            {
                out.push(wt.to_path_buf());
            }
        }
    }
    out
}

/// `given` (absolute) made physical: its deepest existing directory resolved through every link, then the part still to be made, which may not climb (a `..` there would resolve only once it is made).
/// # Errors
/// The path climbs out of a directory that does not exist yet, or it is inside a checkout.
pub fn resolve_out(given: &str, checkouts: &[PathBuf]) -> Result<PathBuf, String> {
    let mut head = PathBuf::from(given);
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while !head.is_dir() {
        match head.components().next_back() {
            Some(Component::Normal(leaf)) => {
                rest.push(leaf.to_os_string());
                head.pop();
            }
            Some(Component::CurDir) => {
                head.pop();
            }
            Some(Component::ParentDir) => {
                return Err(format!(
                    "{given} climbs (..) out of a directory that does not exist yet; name it without .."
                ));
            }
            _ => return Err(format!("{given}: no part of it is a directory")),
        }
    }
    let dir = std::fs::canonicalize(&head)
        .map_err(|e| format!("cannot resolve {}: {e}", head.display()))?;
    let mut out = dir.clone();
    for leaf in rest.iter().rev() {
        out.push(leaf);
    }
    let ids: Vec<(u64, u64, &PathBuf)> = checkouts
        .iter()
        .filter_map(|c| std::fs::metadata(c).ok().map(|m| (m.dev(), m.ino(), c)))
        .collect();
    for at in dir.ancestors() {
        let Ok(m) = std::fs::metadata(at) else {
            continue;
        };
        if let Some((_, _, c)) = ids.iter().find(|(d, i, _)| *d == m.dev() && *i == m.ino()) {
            return Err(format!(
                "{} is inside the repository ({}); pick an output dir outside it",
                out.display(),
                c.display()
            ));
        }
    }
    Ok(out)
}

/// The styles of the Settings picker: the `options=[…]` list of the `cursor_trail_style` field of a `controls prefs` reply.
#[must_use]
pub fn roster_of(prefs: &str) -> Option<Vec<String>> {
    let line = prefs
        .lines()
        .find(|l| l.contains("field key=cursor_trail_style "))?;
    let list = line.split_once("options=[")?.1.split_once(']')?.0;
    let styles: Vec<String> = list
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (!styles.is_empty()).then_some(styles)
}

/// A running instance and its scratch world: dropped, the instance is stopped
/// (its process group) before the world is removed (fields drop in order).
struct Live {
    inst: Instance,
    _world: World,
}

/// A human-readable `ctl` diagnostic: its stdout, then its stderr.
fn reply_text(r: &Reply) -> String {
    format!("{}{}", r.stdout, r.stderr)
}

#[derive(Clone, Copy)]
enum ReplyShape {
    Line,
    Counted,
}

fn relay_reply<S: std::io::Read + std::io::Write>(
    wire: &mut RelayClient<S>,
    request: &[&str],
    shape: ReplyShape,
) -> std::io::Result<Reply> {
    let line = request.join(" ");
    let (header, body) = match shape {
        ReplyShape::Line => (wire.request_line(&line)?, None),
        ReplyShape::Counted => {
            let (header, body) = wire.request_counted(&line)?;
            (header, Some(body))
        }
    };
    let ok = header == "OK" || header.starts_with("OK ");
    Ok(match (ok, body) {
        (true, Some(body)) => Reply {
            code: 0,
            stdout: body,
            stderr: format!("{header}\n"),
        },
        (true, None) => Reply {
            code: 0,
            stdout: format!("{header}\n"),
            stderr: String::new(),
        },
        (false, _) => Reply {
            code: 1,
            stdout: String::new(),
            stderr: format!("{header}\n"),
        },
    })
}

/// The counted `image --meta --bytes` body; unlike a filename reply, it needs no artifact ACK.
fn image_bytes(body: &str) -> Result<(Vec<u8>, &str), String> {
    let limit = aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES;
    let mut lines = body.lines();
    let metadata = lines.next().ok_or("image metadata is missing")?;
    if metadata.len() > limit || !metadata.starts_with("image-meta ") {
        return Err("image metadata is missing or exceeds its bound".into());
    }
    let payload = lines.next().ok_or("image bytes are missing")?;
    if payload.len() > limit || lines.next().is_some() {
        return Err("image payload exceeds its line bound or has extra rows".into());
    }
    let mut fields = payload.split_whitespace();
    let number = |token: Option<&str>| token.and_then(|s| s.parse::<usize>().ok());
    let width = number(fields.next())
        .filter(|&n| n > 0)
        .ok_or("bad image width")?;
    let height = number(fields.next())
        .filter(|&n| n > 0)
        .ok_or("bad image height")?;
    let declared = number(fields.next())
        .filter(|&n| n <= limit)
        .ok_or("bad image length")?;
    let encoded = fields.next().ok_or("image base64 is missing")?;
    if fields.next().is_some() || aterm_codec::base64::encoded_len(declared) != Some(encoded.len())
    {
        return Err("image encoded length does not match its declaration".into());
    }
    let png = aterm_codec::base64::decode(encoded).map_err(|e| format!("image base64: {e}"))?;
    if png.len() != declared {
        return Err("image byte length does not match its declaration".into());
    }
    {
        let reader = aterm_png::Decoder::new(std::io::Cursor::new(&png))
            .read_info()
            .map_err(|e| format!("image PNG: {e}"))?;
        if usize::try_from(reader.info().width).ok() != Some(width)
            || usize::try_from(reader.info().height).ok() != Some(height)
        {
            return Err("image PNG dimensions do not match its declaration".into());
        }
    }
    Ok((png, metadata))
}

#[derive(Clone, Copy)]
enum FrameRead {
    Whole,
    Open,
    Returned,
}

struct FrameEvidence {
    status: Reply,
    cursor: Option<Reply>,
    text: Option<Reply>,
}

fn frame_evidence(
    phase: FrameRead,
    mut request: impl FnMut(&[&str], ReplyShape) -> Reply,
) -> FrameEvidence {
    let status = request(&["trail", "status"], ReplyShape::Counted);
    let (cursor, text) = if matches!(phase, FrameRead::Open) {
        (None, None)
    } else {
        (
            Some(request(&["cursor"], ReplyShape::Line)),
            Some(request(&["text"], ReplyShape::Counted)),
        )
    };
    FrameEvidence {
        status,
        cursor,
        text,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ReturnMeasurement {
    NotApplicable,
    LiveRibbon,
    Inconclusive,
}

/// Whether this instance's final config asks to measure a live rainbow ribbon.
fn expects_rainbow(config: &[String]) -> Result<bool, String> {
    use aterm_effects::cursor_glow::{GlowStyle, spellings};
    use aterm_effects::motion::MotionMode;
    use aterm_toml::Value;

    let config: Value = aterm_toml::from_str(&config.join("\n"))
        .map_err(|error| format!("invalid gallery config: {error}"))?;
    let string = |key: &str| match config.get(key) {
        Some(Value::String(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(format!("gallery config {key} must be a string")),
        None => Ok(None),
    };
    let boolean = |key: &str| match config.get(key) {
        Some(Value::Boolean(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("gallery config {key} must be a boolean")),
        None => Ok(None),
    };
    let raw = string("cursor_trail_style")?.ok_or("gallery config has no cursor_trail_style")?;
    let style =
        spellings::row(raw).ok_or_else(|| format!("unknown gallery cursor trail style {raw:?}"))?;
    let master = boolean("cursor_trail")?;
    let serious = boolean("serious_mode")?.unwrap_or(false);
    let motion = string("motion")?.map_or(MotionMode::Auto, MotionMode::parse);
    let intensity = match config.get("cursor_trail_intensity") {
        Some(Value::Float(value)) => Some(*value as f32),
        Some(Value::Integer(value)) => Some(*value as f32),
        Some(_) => return Err("gallery config cursor_trail_intensity must be a number".into()),
        None => None,
    };
    // These explicit choices suppress the trail in the app. Runtime focus,
    // shedding or a missing ribbon cannot turn a requested measurement into a skip.
    Ok(style.style == Some(GlowStyle::RainbowKitty)
        && master != Some(false)
        && !serious
        && motion != MotionMode::Reduced
        && intensity.is_none_or(|value| value.is_finite() && value > 0.0))
}

fn return_measurement(rainbow_expected: bool, status: &Reply) -> Result<ReturnMeasurement, String> {
    if status.code != 0 {
        return Err(format!(
            "could not read the returning frame's trail: {}",
            reply_text(status)
        ));
    }
    let field = |key: &str| {
        status
            .stdout
            .split_whitespace()
            .find_map(|word| {
                word.strip_prefix(key)
                    .and_then(|rest| rest.strip_prefix('='))
            })
            .ok_or_else(|| format!("trail status has no {key}"))
    };
    let boolean = |key| match field(key)? {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("trail status has invalid {key}")),
    };
    let resolved = field("resolved")?;
    if resolved != "off"
        && !aterm_effects::cursor_glow::spellings::TRAIL_STYLES
            .iter()
            .any(|row| row.style.is_some_and(|style| style.label() == resolved))
    {
        return Err("trail status has invalid resolved style".into());
    }
    let enabled = boolean("config_enabled")?;
    let effective = boolean("effective")?;
    let intensity = field("intensity")?
        .parse::<f32>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or("trail status has invalid intensity")?;
    let count = |key| {
        field(key)?
            .parse::<u64>()
            .map_err(|_| format!("trail status has invalid {key}"))
    };
    let segments = count("ribbon_segments")?;
    let drawn = count("ribbon_drawn")?;
    Ok(if !rainbow_expected {
        ReturnMeasurement::NotApplicable
    } else if resolved == "rainbow-kitty"
        && enabled
        && effective
        && intensity > 0.0
        && segments > 0
        && drawn > 0
    {
        ReturnMeasurement::LiveRibbon
    } else {
        ReturnMeasurement::Inconclusive
    })
}

/// One instance's capture: its clock, its held connection and its tallies.
struct Shot<'a> {
    inst: &'a Instance,
    /// Keys and ticks; `None` once it failed (then they are client runs).
    wire: Option<RelayClient<CtlStream>>,
    out: &'a Path,
    name: &'a str,
    tsv: Option<File>,
    start: Instant,
    mark: Instant,
    terminal: bool,
    rainbow_expected: bool,
    columns: u32,
    clean: bool,
    frames: u32,
    fails: u32,
    inconclusive: u32,
}

impl Shot<'_> {
    fn note(&self, msg: &str) {
        eprintln!("trail-gallery: [{}] {msg}", self.name);
    }

    fn path(&self, suffix: &str) -> PathBuf {
        self.out.join(format!("{}.{suffix}", self.name))
    }

    fn keep(&self, suffix: &str, r: &Reply) {
        let _ = std::fs::write(self.path(suffix), &r.stdout);
        if !r.stderr.is_empty() {
            let _ = std::fs::write(self.path(&format!("{suffix}.stderr")), &r.stderr);
        }
    }

    fn row(&mut self, line: &str) {
        if let Some(f) = self.tsv.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }

    fn since_start(&self, t: Instant) -> u128 {
        t.duration_since(self.start).as_millis()
    }

    fn request(&mut self, request: &[&str], shape: ReplyShape) -> Reply {
        if let Some(wire) = self.wire.as_mut() {
            match relay_reply(wire, request, shape) {
                Ok(reply) => return reply,
                Err(e) => {
                    self.wire = None;
                    self.note(&format!(
                        "the held control connection failed ({e}); using `ctl` client runs from here"
                    ));
                    self.row(&format!(
                        "@transport-client\t{}",
                        self.since_start(Instant::now())
                    ));
                }
            }
        }
        self.inst.ctl(request)
    }

    /// A frame rendered and thrown away, so time passes for the effects.
    fn tick(&mut self) {
        if let Some(w) = self.wire.as_mut() {
            match w.request_counted("image --bytes") {
                Ok(_) => return,
                Err(e) => {
                    self.wire = None;
                    self.note(&format!(
                        "the held control connection failed ({e}); ticking with `ctl image` from here"
                    ));
                }
            }
        }
        let _ = self.inst.ctl(&["image", "tick.png"]);
    }

    /// A named key through the `key` verb, the licensed press path.
    fn press(&mut self, key: &str) {
        let wired = self
            .wire
            .as_mut()
            .map(|w| w.request_line(&format!("key {key}")));
        let answer = match wired {
            Some(Ok(line)) => line,
            other => {
                if let Some(Err(e)) = other {
                    self.wire = None;
                    self.note(&format!(
                        "the held control connection failed ({e}); pressing with `ctl key` from here"
                    ));
                }
                let r = self.inst.ctl(&["key", key]);
                if r.code == 0 {
                    "OK".to_string()
                } else {
                    r.first()
                }
            }
        };
        if !answer.starts_with("OK") {
            self.note(&format!("key {key} refused: {answer}"));
            self.fails += 1;
        }
    }

    /// Capture inline PNG bytes without the filename form's artifact acknowledgement.
    fn grab(&mut self, clean: bool, dest: &Path) -> bool {
        let request = if clean {
            &["image", "clean", "--meta", "--bytes"][..]
        } else {
            &["image", "--meta", "--bytes"][..]
        };
        let reply = self.request(request, ReplyShape::Counted);
        let saved = (|| {
            if reply.code != 0 {
                return Err(reply_text(&reply));
            }
            let (png, metadata) = image_bytes(&reply.stdout)?;
            std::fs::write(dest, png).map_err(|e| format!("{}: {e}", dest.display()))?;
            let meta = dest.with_extension("image-meta.txt");
            std::fs::write(&meta, format!("{metadata}\n"))
                .map_err(|e| format!("{}: {e}", meta.display()))
        })();
        if let Err(error) = &saved {
            self.note(&format!(
                "{} failed: {}",
                request.join(" "),
                error.trim_end()
            ));
            self.fails += 1;
        }
        saved.is_ok()
    }

    /// A kept frame, its row in frames.tsv, and its sidecars.
    fn snap(&mut self, frame: &str) {
        self.snap_phase(frame, FrameRead::Whole);
    }

    fn snap_phase(&mut self, frame: &str, phase: FrameRead) {
        let t = Instant::now();
        let dest = self.path(&format!("{frame}.png"));
        if !self.grab(false, &dest) {
            return;
        }
        self.frames += 1;
        let ms = self.since_start(t);
        self.row(&format!("{frame}\t{ms}"));
        if !self.terminal {
            return;
        }
        let evidence = frame_evidence(phase, |request, shape| self.request(request, shape));
        self.keep(&format!("{frame}.status.txt"), &evidence.status);
        match evidence.cursor {
            Some(reply) => self.keep(&format!("{frame}.cursor.txt"), &reply),
            None => self.keep(&format!("{frame}.read-notes.txt"), &Reply {
                stdout: "cursor/text were not requested: DEC 2026 is deliberately open; whole-frame reads would delay its release. No later state is substituted.\n".into(),
                ..Reply::default()
            }),
        }
        if let Some(reply) = evidence.text {
            self.keep(&format!("{frame}.text.txt"), &reply);
        }
        if matches!(phase, FrameRead::Returned) {
            let note = match return_measurement(self.rainbow_expected, &evidence.status) {
                Ok(ReturnMeasurement::NotApplicable) => {
                    "not applicable: this instance's config did not request a live rainbow ribbon"
                }
                Ok(ReturnMeasurement::LiveRibbon) => {
                    "measurable: a live ribbon remains at return capture; continuity has not been judged"
                }
                Ok(ReturnMeasurement::Inconclusive) => {
                    self.inconclusive += 1;
                    self.note("inconclusive: no live ribbon at return capture");
                    "inconclusive: no live ribbon at return capture; expiry or another rendering cause cannot be distinguished by this frame"
                }
                Err(error) => {
                    self.fails += 1;
                    self.note(&error);
                    "unmeasured: the returning frame's trail status could not be validated"
                }
            };
            self.keep(
                &format!("{frame}.measurement.txt"),
                &Reply {
                    stdout: format!("{note}\n"),
                    ..Reply::default()
                },
            );
        }
        if self.clean {
            let dest = self.path(&format!("{frame}.clean.png"));
            self.grab(true, &dest);
        }
    }

    /// An event the frame labels count from, on the frames' clock.
    fn mark(&mut self, event: &str) {
        self.mark = Instant::now();
        let ms = self.since_start(self.mark);
        self.row(&format!("@{event}\t{ms}"));
    }

    /// Tick until `ms` after the mark, a frame every `every`.
    fn tick_until(&mut self, ms: u64, every: Duration) -> Result<(), Stopped> {
        while self.mark.elapsed() < Duration::from_millis(ms) {
            stop::sleep(every)?;
            self.tick();
        }
        Ok(())
    }

    /// Wait until `ms` after `start`; at once when that is past (a slow frame).
    fn pace(start: Instant, ms: u64) -> Result<(), Stopped> {
        let due = start + Duration::from_millis(ms);
        let now = Instant::now();
        if due > now {
            stop::sleep(due - now)?;
        }
        Ok(())
    }

    /// A typed burst, a key per 60 ms (~17 keys/s) unless a frame is slower,
    /// each followed by a frame: key `mid` is kept as `mid`.
    fn burst(&mut self, mid: Option<u64>, keys: &[String]) -> Result<(), Stopped> {
        match mid {
            Some(i) => self.paced(TYPING_KEY_MS, &[(i, "mid".to_string())], keys),
            None => self.paced(TYPING_KEY_MS, &[], keys),
        }
    }

    /// A typed burst, a key per `every` ms unless a frame is slower, each
    /// followed by a frame: each key `keep.0` is kept as `keep.1`.
    fn paced(
        &mut self,
        every: u64,
        keep: &[(u64, String)],
        keys: &[String],
    ) -> Result<(), Stopped> {
        self.mark("burst-start");
        let start = self.mark;
        for (i, k) in (0u64..).zip(keys) {
            if i > 0 {
                Shot::pace(start, i * every)?;
            }
            self.press(k);
            stop::sleep(Duration::from_millis(every.min(60) / 2))?;
            match keep.iter().find(|(at, _)| *at == i) {
                Some((_, frame)) => self.snap(frame),
                None => self.tick(),
            }
        }
        self.mark("burst-end");
        Ok(())
    }

    /// The audit gallery's fade: three frames 40 ms apart, then six 60 ms apart.
    fn fade(&mut self) -> Result<(), Stopped> {
        for _ in 0..3 {
            stop::sleep(Duration::from_millis(40))?;
            self.tick();
        }
        self.snap("t120");
        for _ in 0..6 {
            stop::sleep(Duration::from_millis(60))?;
            self.tick();
        }
        self.snap("t470");
        Ok(())
    }

    /// Split the window ([`SPLIT_PANES`]) and give the caret's pane the focus
    /// back, keeping each reply and the layout (`<name>.panes.txt`); half a
    /// second of frames lets the new pane's shell come up.
    fn split(&mut self) -> Result<(), Stopped> {
        let [how, back] = SPLIT_PANES;
        for request in [&["spawn", how][..], &["pane", back][..]] {
            let r = self.inst.ctl(request);
            if r.code != 0 {
                self.note(&format!(
                    "{}: {}",
                    request.join(" "),
                    reply_text(&r).trim_end()
                ));
                self.fails += 1;
            }
        }
        self.mark("split");
        self.tick_until(500, Duration::from_millis(60))?;
        let panes = self.inst.ctl(&["panes"]);
        self.keep("panes.txt", &panes);
        if !panes.stdout.lines().any(|l| l.starts_with("pane "))
            || panes_focus(&panes.stdout) != Some(0)
        {
            self.note("the split did not leave the caret's pane, the first, focused");
            self.fails += 1;
        }
        // The frames are judged on the caret's pane: its grid, not the
        // window's (the window's is kept as `window.dims.txt`).
        let window = std::fs::read_to_string(self.path("dims.txt")).unwrap_or_default();
        match pane_dims(&window, &panes.stdout) {
            Some(pane) => {
                let _ = std::fs::write(self.path("window.dims.txt"), &window);
                let _ = std::fs::write(self.path("dims.txt"), format!("{pane}\n"));
            }
            None => {
                self.note("the caret's pane's grid is not known; its frames keep the window's");
                self.fails += 1;
            }
        }
        Ok(())
    }

    fn settings_page(&mut self, route: &str, label: &str, at: &[u64]) -> Result<(), Stopped> {
        let r = self.inst.ctl(&["open", "app", "settings", route]);
        self.keep(&format!("{label}.open.txt"), &r);
        if r.code != 0 {
            self.note(&format!(
                "open app settings {route}: {}",
                reply_text(&r).trim_end()
            ));
            self.fails += 1;
            return Ok(());
        }
        self.mark(&format!("open-{label}"));
        for &ms in at {
            self.tick_until(ms, Duration::from_millis(100))?;
            self.snap(&format!("{label}-{ms:04}"));
        }
        let prefs = self.inst.ctl(&["controls", "prefs"]);
        self.keep(&format!("{label}.controls.txt"), &prefs);
        Ok(())
    }

    fn run(&mut self, scenario: Scenario) -> Result<(), Stopped> {
        let sleep = |ms| stop::sleep(Duration::from_millis(ms));
        let word = keys_of(TYPING_TEXT);
        match scenario {
            Scenario::Typing => {
                self.burst(Some(12), &word)?;
                self.snap("t0");
                self.fade()?;
                self.press("enter");
                sleep(100)?;
                self.tick();
                sleep(200)?;
                self.tick();
                self.press("ctrl+l");
                sleep(50)?;
                self.snap("jump");
                for _ in 0..8 {
                    sleep(120)?;
                    self.tick();
                }
                self.snap("rest");
            }
            Scenario::Burst => {
                let keep: Vec<(u64, String)> = BURST_START_KEYS
                    .iter()
                    .map(|&k| (k as u64, burst_start_frame(k)))
                    .chain([(BURST_FLY_KEY as u64, "fly".to_string())])
                    .collect();
                self.paced(BURST_KEY_MS, &keep, &burst_keys())?;
                self.snap("t0");
                for ms in BURST_AFTER_MS {
                    self.tick_until(ms, Duration::from_millis(40))?;
                    self.snap(&burst_frame(ms));
                }
            }
            Scenario::Context | Scenario::Cjk | Scenario::Bidi => {
                let (mid, keys) = match scenario {
                    Scenario::Cjk => (6, keys_of("echo 你好世界")),
                    Scenario::Bidi => (8, keys_of("echo שלום עולם")),
                    _ => (12, word),
                };
                self.burst(Some(mid), &keys)?;
                self.snap("t0");
                self.fade()?;
            }
            Scenario::Split => {
                self.split()?;
                self.burst(Some(12), &word)?;
                self.snap("t0");
                self.fade()?;
                self.press("ctrl+u");
                self.mark("killed");
                self.tick_until(800, Duration::from_millis(60))?;
                let [fly, b0, after @ ..] = SPLIT_BURST_FRAMES;
                self.paced(
                    BURST_KEY_MS,
                    &[(BURST_FLY_KEY as u64, fly.to_string())],
                    &burst_keys(),
                )?;
                self.snap(b0);
                for (label, ms) in after.into_iter().zip(SPLIT_BURST_AFTER_MS) {
                    self.tick_until(ms, Duration::from_millis(40))?;
                    self.snap(label);
                }
            }
            Scenario::MoveUp => {
                self.burst(None, &keys_of("./k"))?;
                self.snap("typed");
                self.press("enter");
                self.mark("enter");
                self.tick_until(900, Duration::from_millis(60))?;
                let moved = self.request(
                    &["await", "match", MOVE_UP_MARK, "timeout=60000"],
                    ReplyShape::Line,
                );
                if moved.code != 0 {
                    self.note("the move-up program did not write its mark");
                    self.fails += 1;
                }
                self.mark("up");
                self.snap("up");
                self.tick_until(120, Duration::from_millis(40))?;
                self.snap("u120");
                self.tick_until(470, Duration::from_millis(60))?;
                self.snap("u470");
            }
            Scenario::Steam => {
                self.burst(None, &word)?;
                self.snap("t0");
                self.mark("steam-start");
                let start = self.mark;
                for k in 1..=12u64 {
                    if k > 1 {
                        Shot::pace(start, (k - 1) * 50)?;
                    }
                    self.press("backspace");
                    sleep(20)?;
                    if k % 3 == 0 {
                        self.snap(&format!("bs{k:02}"));
                    } else {
                        self.tick();
                    }
                }
                self.mark("steam-end");
                self.fade()?;
            }
            Scenario::EnterBottom => {
                self.burst(None, &word)?;
                self.snap("typed");
                self.press("enter");
                self.mark("enter");
                sleep(30)?;
                self.snap("enter");
                self.tick_until(120, Duration::from_millis(40))?;
                self.snap("e120");
                self.tick_until(470, Duration::from_millis(60))?;
                self.snap("e470");
            }
            Scenario::Idle10 => {
                self.burst(Some(12), &word)?;
                self.snap("t0");
                for s in [1u64, 2, 5, 10] {
                    self.tick_until(s * 1000, Duration::from_millis(100))?;
                    self.snap(&format!("idle{s}"));
                }
            }
            Scenario::RedrawStall => {
                self.burst(None, &word)?;
                self.snap("typed");
                self.press("ctrl+g");
                self.mark("redraw-start");
                self.tick_until(500, Duration::from_millis(16))?;
                self.snap_phase("stalled", FrameRead::Open);
                self.press("ctrl+g");
                self.mark("redraw-return");
                let ready = self.request(
                    &["await", "match", "redraw.restored", "timeout=60000"],
                    ReplyShape::Line,
                );
                if ready.code != 0 {
                    self.note("the redraw fixture did not restore its text");
                    self.fails += 1;
                }
                self.snap_phase("restored", FrameRead::Returned);
            }
            Scenario::FocusRoundtrip => {
                self.tick();
                for (direction, expected) in [("out", "focus.out"), ("in", "focus.in")] {
                    let sent = self.request(&["focus", direction], ReplyShape::Line);
                    let seen = self.request(
                        &["await", "match", expected, "timeout=60000"],
                        ReplyShape::Line,
                    );
                    if sent.code != 0 || seen.code != 0 {
                        self.note("the focus report did not reach the fixture");
                        self.fails += 1;
                        return Ok(());
                    }
                    self.tick();
                }
                self.snap("focused");
                self.burst(None, &word)?;
                self.snap_phase("typed", FrameRead::Returned);
            }
            Scenario::FoldRest => {
                self.tick();
                let mut text: String =
                    "echo the quick brown fox walks along every line kitty types "
                        .chars()
                        .cycle()
                        .take(self.columns.saturating_sub(3) as usize)
                        .collect();
                text.extend("xyz".chars().skip(3 - self.columns.min(3) as usize));
                let keys = keys_of(&text);
                let split = keys.len().saturating_sub(3);
                self.burst(None, &keys[..split])?;
                self.mark("fold");
                for key in &keys[split..] {
                    self.press(key);
                }
                sleep(40)?;
                let ready =
                    self.request(&["await", "match", "z", "timeout=60000"], ReplyShape::Line);
                if ready.code != 0 {
                    self.note("the fold fixture did not echo its final glyph");
                    self.fails += 1;
                }
                self.snap("folded");
                for ms in [1100, 1250, 1400] {
                    self.tick_until(ms, Duration::from_millis(16))?;
                    self.snap(&format!("f{ms}"));
                }
                self.tick_until(2000, Duration::from_millis(16))?;
                self.snap("rest");
            }
            Scenario::Settings => {
                self.settings_page("/cursor-motion", "motion", &[300, 900, 1500, 2100])?;
                self.settings_page("/cursor-kitty", "kitty", &[300, 1200])?;
            }
        }
        Ok(())
    }
}

/// A whole run: where it writes, what it drives and what it counted.
struct Gallery {
    out: PathBuf,
    aterm: PathBuf,
    pids: Option<File>,
    /// pid, socket, name of every instance launched.
    launched: Vec<(i32, String, String)>,
    clean: bool,
    keep: bool,
    instances: u32,
    frames: u32,
    fails: u32,
    inconclusive: u32,
}

impl Gallery {
    /// A fresh world whose `aterm.toml` is `lines` over the lanes-off base,
    /// the scenario's paint, and the instance started in it (not yet ready).
    fn launch(
        &mut self,
        name: &str,
        scenario: Scenario,
        theme: Theme,
        (columns, lines, font_px): (u32, u32, u32),
        config: &[String],
    ) -> Result<Live, String> {
        let mut world = World::new(&instance::config_with(config))?;
        if self.keep {
            world.keep();
        }
        let script = world.dir.join("paint.sh");
        std::fs::write(&script, paint(scenario, theme))
            .map_err(|e| format!("cannot write {}: {e}", script.display()))?;
        let env = world
            .env()
            .into_iter()
            .filter(|(k, _)| k != "ENV")
            .collect();
        let mut args = vec![
            "--columns".to_string(),
            columns.to_string(),
            "--lines".to_string(),
            lines.to_string(),
            "--font-px".to_string(),
            font_px.to_string(),
        ];
        args.push("-e".to_string());
        if matches!(
            scenario,
            Scenario::RedrawStall | Scenario::FoldRest | Scenario::FocusRoundtrip
        ) {
            args.push(paths().exe.to_string_lossy().into_owned());
            args.push("__trail-redraw-fixture".to_string());
            if scenario == Scenario::FoldRest {
                args.push("fold".to_string());
            } else if scenario == Scenario::FocusRoundtrip {
                args.push("focus".to_string());
            }
        } else {
            args.push("/bin/sh".to_string());
            args.push(script.to_string_lossy().into_owned());
        }
        let inst = Instance::start(
            &world,
            Launch {
                aterm: self.aterm.clone(),
                headless: true,
                args,
                env,
                cwd: world.home.clone(),
                sandbox: None,
                reroute: false,
            },
        )?;
        let sock = inst.sock.to_string_lossy().into_owned();
        if let Some(f) = self.pids.as_mut() {
            let _ = writeln!(f, "{}\t{sock}\t{name}", inst.pid);
        }
        self.launched.push((inst.pid, sock, name.to_string()));
        Ok(Live {
            inst,
            _world: world,
        })
    }

    /// Stop it by its group and keep its log as `<name>.gui.log`; `--keep` retains its world.
    fn end(&self, mut live: Live, name: &str) {
        live.inst.stop();
        let _ = std::fs::copy(&live.inst.log, self.out.join(format!("{name}.gui.log")));
    }

    /// Every style the Settings picker offers, read from a probe instance.
    fn roster(&mut self) -> Result<Vec<String>, Fail> {
        let mut live = self
            .launch(
                "roster-probe",
                Scenario::Typing,
                Theme::Dark,
                Scenario::Settings.grid(),
                &[],
            )
            .map_err(|e| Fail::exit(3, format!("trail-gallery: the roster probe: {e}")))?;
        let mut found = None;
        match settle(&mut live.inst) {
            Err(f) if f.stopped.is_some() => return Err(f),
            Err(f) => eprintln!(
                "trail-gallery: the roster probe: {}",
                f.msg.unwrap_or_default()
            ),
            Ok(()) => {
                for page in ["/cursor-kitty", "/cursor-motion"] {
                    if live.inst.ctl(&["open", "app", "settings", page]).code != 0 {
                        continue;
                    }
                    stop::sleep(Duration::from_millis(300))?;
                    found = roster_of(&live.inst.ctl(&["controls", "prefs"]).stdout);
                    if found.is_some() {
                        break;
                    }
                }
            }
        }
        live.inst.stop();
        drop(live);
        found.ok_or_else(|| {
            Fail::exit(
                3,
                format!(
                    "trail-gallery: could not read the trail roster from {}; name the styles",
                    self.aterm.display()
                ),
            )
        })
    }

    /// One style, theme and scenario on a fresh instance.
    fn one(&mut self, style: &str, theme: Theme, scenario: Scenario, a: &Args) -> Result<(), Fail> {
        let name = instance_name(style, theme, scenario);
        remove_named(&self.out, &name);
        self.instances += 1;
        let before = (self.fails, self.inconclusive);
        let (c, l, f) = scenario.grid();
        let grid = (
            a.columns.unwrap_or(c),
            a.lines.unwrap_or(l),
            a.font_px.unwrap_or(f),
        );
        let config = config_lines(style, theme, &a.extras);
        let rainbow_expected = matches!(scenario, Scenario::RedrawStall | Scenario::FocusRoundtrip)
            && expects_rainbow(&config).map_err(usage)?;
        match self.launch(&name, scenario, theme, grid, &config) {
            Err(e) => {
                eprintln!("trail-gallery: [{name}] {e}");
                self.fails += 1;
            }
            Ok(mut live) => {
                match settle(&mut live.inst) {
                    Err(f) if f.stopped.is_some() => return Err(f),
                    Err(f) => {
                        eprintln!("trail-gallery: [{name}] {}", f.msg.unwrap_or_default());
                        self.fails += 1;
                    }
                    Ok(()) => {
                        self.capture(&live.inst, &name, scenario, grid.0, rainbow_expected)?;
                    }
                }
                self.end(live, &name);
            }
        }
        if (self.fails, self.inconclusive) == before {
            println!("[{name}] done");
        } else if self.fails == before.0 {
            println!("[{name}] INCONCLUSIVE");
        } else {
            println!("[{name}] FAILED");
        }
        Ok(())
    }

    fn capture(
        &mut self,
        inst: &Instance,
        name: &str,
        scenario: Scenario,
        columns: u32,
        rainbow_expected: bool,
    ) -> Result<(), Stopped> {
        let tsv_path = self.out.join(format!("{name}.frames.tsv"));
        let tsv = File::create(&tsv_path);
        let wire = inst.connect().map_err(|e| {
            eprintln!("trail-gallery: [{name}] no held control connection ({e}); keys and ticks are client runs");
        });
        let now = Instant::now();
        let mut shot = Shot {
            inst,
            wire: wire.ok(),
            out: &self.out,
            name,
            tsv: tsv.ok(),
            start: now,
            mark: now,
            terminal: scenario.terminal(),
            rainbow_expected,
            columns,
            clean: self.clean,
            frames: 0,
            fails: 0,
            inconclusive: 0,
        };
        if shot.tsv.is_none() {
            shot.note(&format!("cannot write {}", tsv_path.display()));
            shot.fails += 1;
        }
        let status = shot.request(&["trail", "status"], ReplyShape::Counted);
        shot.keep("status.txt", &status);
        if shot.terminal {
            let dims = shot.request(&["dims"], ReplyShape::Line);
            shot.keep("dims.txt", &dims);
        }
        let ran = shot.run(scenario);
        if ran.is_ok() {
            let status = shot.request(&["trail", "status"], ReplyShape::Counted);
            shot.keep("status-end.txt", &status);
            let admissions = shot.request(&["trail", "40"], ReplyShape::Counted);
            shot.keep("admissions.txt", &admissions);
        }
        self.frames += shot.frames;
        self.fails += shot.fails;
        self.inconclusive += shot.inconclusive;
        ran
    }

    /// Every launched instance must be gone: one still running (its pid
    /// checked against its own socket in its arguments, so a recycled pid is
    /// never mistaken) is reported and killed by pid.
    fn survivors(&self) -> u32 {
        let mut alive = 0;
        for (pid, sock, name) in &self.launched {
            if crate::procs::command_line(*pid).contains(&format!("--control-sock {sock}")) {
                eprintln!("trail-gallery: [{name}] pid {pid} is still running; killing it by pid");
                let _ = crate::sys::kill(*pid, libc::SIGKILL);
                alive += 1;
            }
        }
        alive
    }
}

/// Wait for a launched instance to answer and settle: its socket, `ready`,
/// then half a second with no output.
fn settle(inst: &mut Instance) -> Result<(), Fail> {
    inst.await_socket(instance::BOOT)?;
    let ready = inst.ctl(&["ready"]);
    if ready.code != 0 {
        return Err(Fail::die(format!("not ready: {}", ready.first())));
    }
    let _ = inst.ctl(&["await", "idle", "500", "timeout=60000"]);
    stop::check()?;
    Ok(())
}

/// Remove `<name>.*` from `dir`: a rerun replaces what an earlier one left.
fn remove_named(dir: &Path, name: &str) {
    let prefix = format!("{name}.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with(&prefix)
            && e.file_type().is_ok_and(|t| t.is_file())
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `trail-gallery`.
/// # Errors
/// Usage (2), not run (3), or interrupted (128 + the signal).
pub fn cmd_trail_gallery(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    stop::install();
    let a = parse_args(args)?;
    let out = resolve_out(&from_caller(&a.out), &checkouts(&paths().repo)).map_err(usage)?;
    let aterm = instance::resolve_aterm(a.aterm.clone()).map_err(|e| Fail::exit(3, e))?;
    std::fs::create_dir_all(&out).map_err(|e| {
        Fail::exit(
            3,
            format!("trail-gallery: cannot make {}: {e}", out.display()),
        )
    })?;
    let pids_path = out.join("gallery.pids");
    let pids = File::create(&pids_path).map_err(|e| {
        Fail::exit(
            3,
            format!("trail-gallery: cannot write {}: {e}", pids_path.display()),
        )
    })?;
    let mut g = Gallery {
        out,
        aterm,
        pids: Some(pids),
        launched: Vec::new(),
        clean: a.clean,
        keep: a.keep,
        instances: 0,
        frames: 0,
        fails: 0,
        inconclusive: 0,
    };
    let styles = if a.styles.is_empty() {
        let r = g.roster()?;
        println!("trail-gallery: roster: {}", r.join(" "));
        r
    } else {
        a.styles.clone()
    };
    for &scenario in &a.scenarios {
        for style in &styles {
            for &theme in &a.themes {
                g.one(style, theme, scenario, &a)?;
            }
        }
    }
    let alive = g.survivors();
    println!(
        "trail-gallery: {} instance(s), {} frame(s), {} failure(s), {} inconclusive return(s), {alive} left running -> {}",
        g.instances,
        g.frames,
        g.fails,
        g.inconclusive,
        g.out.display()
    );
    Ok(i32::from(alive > 0 || g.fails > 0 || g.inconclusive > 0))
}

#[cfg(test)]
#[path = "trail_gallery_tests.rs"]
mod tests;
