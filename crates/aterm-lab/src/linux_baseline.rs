// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scripted Linux baseline session (`linux baseline`).
//! STOCK EXCEPTION: the guest builds with rustup's stock toolchain, as [`crate::linux_proof`] does.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::linux::{self, Out, Up};
use crate::linux_guest::{self as guest, Arch, Display, Screen, Seat, Selection};
use crate::linux_proof::{self as proof, Gui};
use crate::linux_xwire as xwire;
use crate::{Fail, LAB, Res};

mod deck;

pub(crate) use deck::{row as deck_row, row_name as deck_row_name};

/// Committed recordings directory under the checkout.
pub const DIR: &str = "crates/aterm-lab/linux-baseline";
/// AT-SPI root application name for `aterm-gui`.
pub const APP: &str = "aterm-gui";

/// Prefix sent before the compose check.
pub const COMPOSE_BEFORE: &str = "echo X-";
pub const COMPOSE_LIVE: &str = "echo X-x";
pub const COMPOSE_AFTER: &str = "-Y";
pub const COMPOSED: &str = "X-x\u{e9}-Y";

/// Expected shell output for the Mutter input-method compose check.
pub const IME_COMPOSED: &str = "X-\u{e9}-Y";

/// IBus engine for dead-key XIM composition.
pub const XIM_ENGINE: &str = "xkb:us::eng";
/// IBus CJK engine for pinyin XIM composition.
pub const XIM_CJK_ENGINE: &str = "libpinyin";
/// Pinyin keys typed to [`XIM_CJK_ENGINE`].
pub const XIM_CJK_KEYS: [&str; 5] = ["n", "i", "h", "a", "o"];
/// Committed first candidate for [`XIM_CJK_KEYS`].
pub const XIM_CJK_TEXT: &str = "\u{4f60}\u{597d}";

/// Key-hold duration in milliseconds for the repeat measurement.
pub const HOLD_MS: u64 = 1500;

/// Guest directory for the XDND drop test file.
pub const DND_DIR: &str = "/tmp/aterm-lane-dnd";
pub const DND_FILE: &str = "dropped file.txt";
pub const DND_URI: &str = "file:///tmp/aterm-lane-dnd/dropped%20file.txt";

/// Reason weston headless outside resize is not driven.
pub const RESIZE_OS_WESTON: &str = "weston's headless backend has no seat, so no binding of its own reaches the window, and no tool here resizes a Wayland client from outside (Mutter's is driven, resize.os.tiled-left, and weston-x11's, resize.os.maximized from its seat)";

/// Wayland global name for primary selection.
pub const PRIMARY_GLOBAL: &str = "zwp_primary_selection_device_manager_v1";

/// Command typed on the seat keyboard for the CPU presenter check.
pub const CPU_TYPED: &str = "echo lane-seat";
pub const CPU_ECHO: &str = "lane-seat";

/// Reason touch input is not driven on X11 and weston backends.
pub const TOUCH_WESTON_X11: &str = "weston's x11 backend gives its seat a pointer and a keyboard, no touch, and XTEST fakes no touchscreen (lane-bus xseat refuses a finger step); Mutter's is driven, through a RemoteDesktop finger";
pub const TOUCH_WESTON: &str = "weston's headless backend advertises no wl_seat, so no touch reaches a client; Mutter's is driven, through a RemoteDesktop finger";
pub const TOUCH_X11: &str = "the lane's X servers have no touch device, which XInput 2's touch events come from, and XTEST fakes no touchscreen; Mutter's is driven, through a RemoteDesktop finger";

/// Seat key steps that type `text` one character at a time.
#[must_use]
pub fn seat_taps(text: &str) -> Vec<String> {
    text.chars()
        .map(|c| match c {
            ' ' => "tap:space".to_string(),
            c => format!("tap:{c}"),
        })
        .collect()
}

/// Filename stem awaited on the typed line after a drop.
pub const DND_STEM: &str = "aterm-lane-dnd";

type Px = (i32, i32);

#[derive(Clone, Copy)]
enum Typist<'a> {
    X { display: &'a str, server: bool },
    Seat,
}

struct Frame {
    rgba: Vec<u8>,
    w: usize,
    h: usize,
}

fn frame_change(a: &Frame, b: &Frame) -> (String, String) {
    if (a.w, a.h) != (b.w, b.h) {
        let size = format!("{}x{} against {}x{}", b.w, b.h, a.w, a.h);
        return (format!("size {size}"), size);
    }
    let pixels = a.w * a.h;
    let (pa, _) = a.rgba.as_chunks::<4>();
    let (pb, _) = b.rgba.as_chunks::<4>();
    let differ = pa.iter().zip(pb).filter(|(x, y)| x != y).count();
    let class = if differ * 100 <= pixels {
        "at most a cell"
    } else if differ * 2 > pixels {
        "most pixels"
    } else {
        "part"
    };
    (class.into(), format!("{differ} of {pixels} pixels"))
}

/// Extract the deduplicated `zwp_text_input*` request/event sequence from a Wayland client wire log.
#[must_use]
pub fn text_input_wire(log: &str) -> String {
    let mut bound = Vec::new();
    let mut requests: Vec<String> = Vec::new();
    let mut events: Vec<String> = Vec::new();
    for line in log.lines() {
        if let Some(at) = line.find("\"zwp_text_input_manager_v3\", ") {
            let rest = &line[at + "\"zwp_text_input_manager_v3\", ".len()..];
            let version: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let entry = format!("zwp_text_input_manager_v3 v{version}");
            if line.contains(".bind(") && !bound.contains(&entry) {
                bound.push(entry);
            }
            continue;
        }
        let Some(at) = line.find("zwp_text_input") else {
            continue;
        };
        let rest = &line[at..];
        let iface: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let Some(after) = rest[iface.len()..].strip_prefix(['@', '#']) else {
            continue;
        };
        let after = after.trim_start_matches(|c: char| c.is_ascii_digit());
        let Some(after) = after.strip_prefix('.') else {
            continue;
        };
        let method: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let Some(args) = after[method.len()..].strip_prefix('(') else {
            continue;
        };
        let mut entry = format!("{iface}.{method}");
        if method == "set_content_type" {
            let _ = write!(entry, "({}", args.split(')').next().unwrap_or(""));
            entry.push(')');
        }
        let list = if line[..at].contains("->") {
            &mut requests
        } else {
            &mut events
        };
        if !list.contains(&entry) {
            list.push(entry);
        }
    }
    let show = |v: &[String]| {
        if v.is_empty() {
            "none".to_string()
        } else {
            v.join(" ")
        }
    };
    format!(
        "binds {}; requests {}; events {}",
        show(&bound),
        show(&requests),
        show(&events)
    )
}

/// Extract the deduplicated `wl_touch` request/event sequence from a Wayland client wire log.
#[must_use]
pub fn touch_wire(log: &str) -> String {
    let mut requests: Vec<String> = Vec::new();
    let mut events: Vec<String> = Vec::new();
    for line in log.lines() {
        let request = line.contains("->");
        let named = |iface: &str| -> Option<String> {
            let at = line.find(iface)?;
            let rest = line[at + iface.len()..].strip_prefix(['@', '#'])?;
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
            let name: String = rest
                .strip_prefix('.')?
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then(|| format!("{iface}.{name}"))
        };
        let entry = if request && line.contains(".get_touch(") {
            Some("wl_seat.get_touch".to_string())
        } else {
            named("wl_touch")
        };
        let Some(entry) = entry else {
            continue;
        };
        let list = if request { &mut requests } else { &mut events };
        if !list.contains(&entry) {
            list.push(entry);
        }
    }
    let show = |v: &[String]| {
        if v.is_empty() {
            "none".to_string()
        } else {
            v.join(" ")
        }
    };
    format!("requests {}; events {}", show(&requests), show(&events))
}

/// Extract the touch id and surface-local position of the first `wl_touch.down` in `log`.
#[must_use]
pub fn touch_down_at(log: &str) -> Option<String> {
    let line = log
        .lines()
        .find(|l| !l.contains("->") && l.contains("wl_touch") && l.contains(".down("))?;
    let args = line
        .split(".down(")
        .nth(1)?
        .trim_end()
        .trim_end_matches(')');
    let parts: Vec<&str> = args.split(", ").collect();
    (parts.len() == 6).then(|| format!("id {} at {},{}", parts[3], parts[4], parts[5]))
}

/// Match `want` (`(target, action)`) in `lane-wl drag` report lines after `seen`.
#[must_use]
pub fn source_heard(lines: &[&str], seen: usize, want: (&str, Option<&str>)) -> Option<String> {
    let new = lines.get(seen..).unwrap_or_default();
    let target = new.iter().rev().find_map(|l| l.strip_prefix("target "));
    let action = lines.iter().rev().find_map(|l| l.strip_prefix("action "));
    let new_action = new.iter().rev().find_map(|l| l.strip_prefix("action "));
    if target == Some(want.0) && want.1.is_none_or(|a| action == Some(a)) {
        let mut said = format!("target {}", want.0);
        if let Some(a) = new_action {
            let _ = write!(said, ", action {a}");
        }
        return Some(said);
    }
    (want == ("none", None) && target.is_none() && new.contains(&"action none"))
        .then(|| "action none".to_string())
}

/// Window frame mode (`client` or `system`) and header button spans from `chrome`'s `strip-caption` line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caption {
    pub client: bool,
    pub buttons: Vec<(String, u16, u16)>,
    pub maximized: bool,
    pub edges: bool,
    pub menu: bool,
}

impl Caption {
    /// Middle strip column of the button named `name`.
    #[must_use]
    pub fn middle(&self, name: &str) -> Option<u16> {
        self.buttons
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|&(_, s, e)| s + (e - s) / 2)
    }

    /// First strip column of the right-aligned header buttons on a `cols`-wide strip, or `cols`.
    #[must_use]
    pub fn right_end(&self, cols: u16) -> u16 {
        let mut slots: Vec<(u16, u16)> = self.buttons.iter().map(|&(_, s, e)| (s, e)).collect();
        slots.sort_unstable();
        let mut lead = 0;
        slots
            .into_iter()
            .find(|&(s, e)| {
                if s == lead {
                    lead = e;
                    false
                } else {
                    true
                }
            })
            .map_or(cols, |(s, _)| s)
    }

    /// Comma-separated button names in strip order, or `none`.
    #[must_use]
    pub fn names(&self) -> String {
        if self.buttons.is_empty() {
            return "none".into();
        }
        self.buttons
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Parse the `strip-caption` line of a `chrome` reply.
#[must_use]
pub fn parse_caption(chrome: &str) -> Option<Caption> {
    let line = chrome
        .lines()
        .find_map(|l| l.trim().strip_prefix("strip-caption "))?;
    let mut c = Caption {
        client: proof::field(line, "frame")? == "client",
        ..Caption::default()
    };
    if !c.client {
        return Some(c);
    }
    let flag = |k: &str| proof::field(line, k) == Some("true");
    c.maximized = flag("maximized");
    c.edges = flag("edges");
    c.menu = flag("menu");
    let buttons = proof::field(line, "buttons")?;
    if buttons != "none" {
        for b in buttons.split(',') {
            let (name, cols) = b.split_once(':')?;
            let (s, e) = cols.split_once("..")?;
            let (s, e) = (s.parse().ok()?, e.parse().ok()?);
            if e <= s {
                return None;
            }
            c.buttons.push((name.to_string(), s, e));
        }
    }
    Some(c)
}

/// Extract the deduplicated `xdg_toplevel` frame-gesture requests from a Wayland client wire log.
#[must_use]
pub fn frame_requests(log: &str) -> String {
    const EDGES: [(&str, &str); 8] = [
        ("1", "top"),
        ("2", "bottom"),
        ("4", "left"),
        ("5", "top_left"),
        ("6", "bottom_left"),
        ("8", "right"),
        ("9", "top_right"),
        ("10", "bottom_right"),
    ];
    let mut seen: Vec<String> = Vec::new();
    for line in log.lines() {
        let Some(at) = line.find("-> xdg_toplevel@") else {
            continue;
        };
        let rest = &line[at + "-> xdg_toplevel@".len()..];
        let Some(after) = rest
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .strip_prefix('.')
        else {
            continue;
        };
        let method: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let entry = match method.as_str() {
            "move" | "show_window_menu" | "set_maximized" | "unset_maximized" | "set_minimized" => {
                method
            }
            "resize" => {
                let edge = after
                    .rsplit(',')
                    .next()
                    .map(|e| e.trim_end_matches(')').trim())
                    .and_then(|e| EDGES.iter().find(|(n, _)| *n == e))
                    .map_or("?", |(_, name)| name);
                format!("resize({edge})")
            }
            _ => continue,
        };
        if !seen.contains(&entry) {
            seen.push(entry);
        }
    }
    if seen.is_empty() {
        "none".into()
    } else {
        seen.join(" ")
    }
}

/// Width in cells of one 34-logical-px header button slot at `scale` on `cell_w`-px cells.
#[must_use]
pub fn caption_slot_cols(cell_w: u32, scale: f64) -> u16 {
    if cell_w == 0 {
        return 0;
    }
    let px = (34.0 * scale).round();
    let cols = (px / f64::from(cell_w)).ceil();
    if cols.is_finite() && cols >= 1.0 {
        cols.min(f64::from(u16::MAX)) as u16
    } else {
        1
    }
}

fn slot_class(f: &Frame, x0: usize, x1: usize, y0: usize, y1: usize) -> (&'static str, usize) {
    let (x1, y1) = (x1.min(f.w), y1.min(f.h));
    if x0 >= x1 || y0 >= y1 {
        return ("bare", 0);
    }
    let px = |x: usize, y: usize| {
        let i = (y * f.w + x) * 4;
        f.rgba.get(i..i + 4)
    };
    let ground = px(x0, y0);
    let mut differ = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if px(x, y) != ground {
                differ += 1;
            }
        }
    }
    let class = if differ * 10 > (x1 - x0) * (y1 - y0) {
        "drawn"
    } else {
        "bare"
    };
    (class, differ)
}

/// Extract the `ime` line from a `chrome` reply.
#[must_use]
pub fn ime_line(chrome: &str) -> Option<&str> {
    chrome
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("ime preedit="))
}

/// Strip the `spot=` suffix from an `ime` line, keeping preedit text and caret.
#[must_use]
pub fn ime_composition(line: &str) -> &str {
    line.rsplit_once(" spot=")
        .map_or(line, |(composition, _spot)| composition)
}

/// Text typed after the last `lane$ ` prompt on `screen`.
#[must_use]
pub fn typed_line(screen: &str) -> String {
    screen
        .lines()
        .rev()
        .find_map(|l| {
            l.strip_prefix(proof::PROMPT)
                .map(|typed| typed.trim().to_string())
        })
        .unwrap_or_default()
}

/// Recording line class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// `=`: behaviour compared exactly on replay.
    Behaviour,
    /// `~`: measured figure shown when it moves.
    Measured,
    /// `@`: stack metadata.
    Stack,
}

impl Class {
    #[must_use]
    pub const fn mark(self) -> char {
        match self {
            Class::Behaviour => '=',
            Class::Measured => '~',
            Class::Stack => '@',
        }
    }

    const fn parse(c: char) -> Option<Class> {
        match c {
            '=' => Some(Class::Behaviour),
            '~' => Some(Class::Measured),
            '@' => Some(Class::Stack),
            _ => None,
        }
    }
}

/// One `<class> <key> <value>` line of a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub class: Class,
    pub key: String,
    pub value: String,
}

/// Ordered recording lines with unique keys and header comments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recording {
    pub comments: Vec<String>,
    pub lines: Vec<Line>,
}

impl Recording {
    /// Append a line, disambiguating duplicate keys with `.<n>`.
    pub fn push(&mut self, class: Class, key: &str, value: impl Into<String>) {
        let mut k = key.to_string();
        let mut n = 2;
        while self.lines.iter().any(|l| l.key == k) {
            k = format!("{key}.{n}");
            n += 1;
        }
        let value: String = value.into();
        self.lines.push(Line {
            class,
            key: k,
            value: one_line(&value),
        });
    }

    pub fn behaviour(&mut self, key: &str, value: impl Into<String>) {
        self.push(Class::Behaviour, key, value);
    }

    pub fn measured(&mut self, key: &str, value: impl Into<String>) {
        self.push(Class::Measured, key, value);
    }

    pub fn stack(&mut self, key: &str, value: impl Into<String>) {
        self.push(Class::Stack, key, value);
    }

    /// Record `= not-driven.<item> <token>` with `why` as a `#` comment.
    pub fn not_driven(&mut self, item: &str, token: &str, why: &str) {
        self.behaviour(&format!("not-driven.{item}"), token);
        self.comments
            .push(format!("not-driven.{item} {token}: {}", one_line(why)));
    }

    /// Format comments and lines as committed recording text.
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = String::new();
        for c in &self.comments {
            let _ = writeln!(s, "# {c}");
        }
        for l in &self.lines {
            let _ = writeln!(s, "{} {} {}", l.class.mark(), l.key, l.value);
        }
        s
    }

    /// Parse committed recording text.
    pub fn parse(text: &str) -> Result<Recording, String> {
        let mut r = Recording::default();
        for (i, raw) in text.lines().enumerate() {
            if raw.trim().is_empty() {
                continue;
            }
            if let Some(c) = raw.strip_prefix('#') {
                r.comments.push(c.trim().to_string());
                continue;
            }
            let mut parts = raw.splitn(3, ' ');
            let class = parts
                .next()
                .and_then(|m| m.chars().next().filter(|_| m.len() == 1))
                .and_then(Class::parse);
            let key = parts.next().filter(|k| !k.is_empty());
            match (class, key) {
                (Some(class), Some(key)) => r.lines.push(Line {
                    class,
                    key: key.to_string(),
                    value: parts.next().unwrap_or_default().to_string(),
                }),
                _ => {
                    return Err(format!(
                        "line {}: not `<=|~|@> <key> <value>`: {raw}",
                        i + 1
                    ));
                }
            }
        }
        Ok(r)
    }

    fn get(&self, key: &str) -> Option<&Line> {
        self.lines.iter().find(|l| l.key == key)
    }
}

fn one_line(s: &str) -> String {
    s.trim_end().replace('\n', "\\n").replace('\r', "\\r")
}

/// Per-class differences between a committed and a fresh [`Recording`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub behaviour: Vec<(String, Option<String>, Option<String>)>,
    pub measured: Vec<(String, Option<String>, Option<String>)>,
    pub stack: Vec<(String, Option<String>, Option<String>)>,
}

impl Diff {
    #[must_use]
    pub const fn same_behaviour(&self) -> bool {
        self.behaviour.is_empty()
    }
}

/// Diff `new` against `old`, classifying keys by `old` when present in both.
#[must_use]
pub fn compare(old: &Recording, new: &Recording) -> Diff {
    let mut d = Diff::default();
    let mut keys: Vec<&str> = old.lines.iter().map(|l| l.key.as_str()).collect();
    for l in &new.lines {
        if old.get(&l.key).is_none() {
            keys.push(&l.key);
        }
    }
    for key in keys {
        let (o, n) = (old.get(key), new.get(key));
        let class = o.or(n).map_or(Class::Behaviour, |l| l.class);
        let (ov, nv) = (o.map(|l| l.value.clone()), n.map(|l| l.value.clone()));
        if ov == nv && o.map(|l| l.class) == n.map(|l| l.class) {
            continue;
        }
        let entry = (key.to_string(), ov, nv);
        match class {
            Class::Behaviour => d.behaviour.push(entry),
            Class::Measured => d.measured.push(entry),
            Class::Stack => d.stack.push(entry),
        }
    }
    d
}

/// Format `d` for the report, capping measured diffs at `measured_cap`.
#[must_use]
pub fn render_diff(d: &Diff, measured_cap: usize) -> String {
    let mut s = String::new();
    let side = |v: &Option<String>| v.clone().unwrap_or_else(|| "(absent)".into());
    for (what, rows, cap) in [
        ("behaviour", &d.behaviour, usize::MAX),
        ("stack", &d.stack, usize::MAX),
        ("measured", &d.measured, measured_cap),
    ] {
        for (k, o, n) in rows.iter().take(cap) {
            let _ = writeln!(s, "{what} {k}:\n  - {}\n  + {}", side(o), side(n));
        }
        if rows.len() > cap {
            let _ = writeln!(s, "{what}: {} more moved", rows.len() - cap);
        }
    }
    s
}

/// Parsed CLI arguments for `linux baseline`.
#[derive(Debug, PartialEq, Eq)]
pub struct BaselineArgs {
    pub arches: Vec<Arch>,
    pub screens: Vec<Screen>,
    pub record: bool,
    pub keep_build: bool,
    pub keep_up: bool,
}

/// Parse `linux baseline` CLI arguments.
pub fn parse_baseline(args: &[String]) -> Result<BaselineArgs, Fail> {
    let mut b = BaselineArgs {
        arches: Vec::new(),
        screens: Vec::new(),
        record: false,
        keep_build: false,
        keep_up: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--arch" => {
                let w = it.next().map_or("", String::as_str);
                let arch = Arch::parse(w).ok_or_else(|| {
                    Fail::usage(format!("linux baseline: --arch {w}: aarch64 or x86_64"))
                })?;
                if !b.arches.contains(&arch) {
                    b.arches.push(arch);
                }
            }
            "--screen" => {
                let w = it.next().map_or("", String::as_str);
                let s = Screen::parse(w).ok_or_else(|| {
                    let names: Vec<String> = Screen::BASELINE.iter().map(|s| s.name()).collect();
                    Fail::usage(format!(
                        "linux baseline: --screen {w}: one of {}",
                        names.join(", ")
                    ))
                })?;
                if !b.screens.contains(&s) {
                    b.screens.push(s);
                }
            }
            "--record" => b.record = true,
            "--keep-build" => b.keep_build = true,
            "--keep-up" => b.keep_up = true,
            other => {
                return Err(Fail::usage(format!(
                    "linux baseline: unknown argument '{other}' ({LAB} linux help)"
                )));
            }
        }
    }
    b.arches = crate::linux_gpu::arches(std::mem::take(&mut b.arches), "baseline")?;
    if b.screens.is_empty() {
        b.screens = Screen::BASELINE.to_vec();
    }
    Ok(b)
}

/// Committed recording filename for `arch` and `screen`.
#[must_use]
pub fn file_name(arch: Arch, screen: Screen) -> String {
    format!("{}-{}.txt", arch.name(), screen.name())
}

const DIMS_FIELDS: [&str; 27] = [
    "scale",
    "cell_w",
    "cell_h",
    "font_px",
    "window_rows",
    "window_cols",
    "grid_w",
    "grid_h",
    "frame_w",
    "frame_h",
    "surface_w",
    "surface_h",
    "offset_x",
    "offset_y",
    "band_left",
    "band_right",
    "band_top",
    "band_bottom",
    "crop_left",
    "crop_right",
    "crop_top",
    "crop_bottom",
    "pad",
    "pad_top",
    "pad_bottom",
    "head",
    "tab_rows",
];

/// Keep the grid dimensions and layout fields from a `dims` reply.
#[must_use]
pub fn dims_summary(reply: &str) -> String {
    let mut w = reply.split_whitespace();
    if w.next() != Some("OK") {
        return format!("refused: {}", reply.trim());
    }
    let (rows, cols) = (w.next().unwrap_or("?"), w.next().unwrap_or("?"));
    let mut s = format!("{rows}x{cols}");
    for f in DIMS_FIELDS {
        let _ = write!(s, " {f}={}", proof::field(reply, f).unwrap_or("?"));
    }
    s
}

const RESIZE_BEHAVIOUR: [&str; 8] = ["ord", "from", "to", "alt", "sync", "site", "win", "chrome"];

/// Split a `resizes` ledger line into its behaviour fields and timing-sensitive measured fields.
#[must_use]
pub fn resize_line(line: &str) -> (String, String) {
    let words: Vec<&str> = line.split_whitespace().collect();
    let behaviour = words
        .iter()
        .take(1)
        .chain(words.iter().filter(|w| {
            w.split_once('=')
                .is_some_and(|(k, _)| RESIZE_BEHAVIOUR.contains(&k))
        }))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let measured = words
        .iter()
        .filter(|w| !w.starts_with("t=") && !w.starts_with("at="))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    (behaviour, measured)
}

const SUMMARY_BEHAVIOUR: [&str; 5] = ["total", "risks", "render", "unledgered", "lost"];

fn keep_fields(line: &str, keys: &[&str]) -> String {
    line.split_whitespace()
        .filter(|w| w.split_once('=').is_some_and(|(k, _)| keys.contains(&k)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Escape `text` as a single-word `await match` regex literal.
#[must_use]
pub fn regex_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            ' ' => out.push_str(r"\s"),
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

const DRIFT_VERDICT: [&str; 6] = [
    "verdict",
    "fidelity",
    "against",
    "drift_rows",
    "culprit",
    "geom",
];

/// Split a `cast drift` reply into its behaviour verdict and measured fields.
#[must_use]
pub fn drift_line(reply: &str) -> (String, String) {
    let first = reply.lines().next().unwrap_or_default();
    let verdict = DRIFT_VERDICT
        .iter()
        .map(|k| format!("{k}={}", proof::field(first, k).unwrap_or("?")))
        .collect::<Vec<_>>()
        .join(" ");
    let rest = first
        .split_whitespace()
        .filter(|w| !w.starts_with("anchor=") && !w.starts_with("ms="))
        .collect::<Vec<_>>()
        .join(" ");
    (verdict, rest)
}

/// Split a `lane-bus atspi-listen` line into `(pid, event_without_timestamp)`.
#[must_use]
pub fn event_parts(line: &str) -> Option<(&str, &str)> {
    let (_, rest) = line.split_once(' ')?;
    rest.split_once(' ')
}

/// Strip the timestamp and PID from an `atspi-listen` line.
#[must_use]
pub fn event_body(line: &str) -> &str {
    event_parts(line).map_or(line, |(_, body)| body)
}

/// Classify an AT-SPI event line as `<Iface.Member>:<detail>@<role>` using `roles`.
#[must_use]
pub fn event_kind(line: &str, roles: &[(String, String)]) -> String {
    let body = event_body(line);
    let mut words = body.split_whitespace();
    let what = words.next().unwrap_or("?");
    let path = words.next().unwrap_or("?");
    let detail = proof::field(body, "detail")
        .unwrap_or("\"\"")
        .trim_matches('"');
    let role = roles
        .iter()
        .find(|(p, _)| p == path)
        .map_or("?", |(_, r)| r.as_str());
    let mut kind = what.to_string();
    if !detail.is_empty() {
        let _ = write!(kind, ":{detail}");
    }
    if what == "Object.StateChanged" {
        let _ = write!(kind, "={}", proof::field(body, "d1").unwrap_or("?"));
    }
    let _ = write!(kind, "@{role}");
    kind
}

/// Split an `atspi-walk` node line into `(path, rest)`.
#[must_use]
pub fn node_parts(line: &str) -> Option<(&str, String)> {
    let rest = line.strip_prefix("node ")?;
    let path = proof::field(rest, "path")?;
    let without = rest
        .split(' ')
        .filter(|w| !w.starts_with("path="))
        .collect::<Vec<_>>()
        .join(" ");
    Some((path, without))
}

fn node_role(line: &str) -> Option<&str> {
    proof::field(line, "role")
}

/// Whether an `atspi-walk` snapshot captured the quiescent post-command prompt state:
/// the frame's accessible name is reset to `"~"` and the terminal's last non-empty row is
/// [`proof::PROMPT`].
fn walk_settled(text: &str) -> bool {
    let mut frame_settled = false;
    let mut terminal_settled = false;
    for line in text.lines() {
        let Some((_, rest)) = node_parts(line) else {
            continue;
        };
        match node_role(&rest) {
            Some("frame") => {
                frame_settled = quoted_field(line, "name").as_deref() == Some("~");
            }
            Some("terminal") => {
                terminal_settled = quoted_field(line, "text")
                    .and_then(|t| {
                        t.lines()
                            .rfind(|l| !l.trim().is_empty())
                            .map(|last| last.trim_end() == proof::PROMPT)
                    })
                    .unwrap_or(false);
            }
            _ => {}
        }
    }
    frame_settled && terminal_settled
}

/// Extract and unescape `key="…"` from `line`.
#[must_use]
pub fn quoted_field(line: &str, key: &str) -> Option<String> {
    let open = format!(" {key}=\"");
    let start = line.find(&open)? + open.len();
    let mut out = String::new();
    let mut chars = line.get(start..)?.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    if chars.next()? != '{' {
                        return None;
                    }
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// Verify that undoing `events` in reverse from `end_text` matches every insert/delete and the final caret.
#[must_use]
pub fn text_replay(
    events: &[&str],
    roles: &[(String, String)],
    end_text: &str,
    end_caret: Option<&str>,
) -> String {
    let terminal: Vec<&str> = roles
        .iter()
        .filter(|(_, r)| r == "terminal")
        .map(|(p, _)| p.as_str())
        .collect();
    let mut text: Vec<char> = end_text.chars().collect();
    let mut changes = 0usize;
    let mut last_caret: Option<String> = None;
    for line in events.iter().rev() {
        let body = event_body(line);
        let mut words = body.split_whitespace();
        let (what, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
        if !terminal.contains(&path) {
            continue;
        }
        if what == "Object.TextCaretMoved" && last_caret.is_none() {
            last_caret = Some(proof::field(body, "d1").unwrap_or("?").to_string());
        }
        if what != "Object.TextChanged" {
            continue;
        }
        let detail = proof::field(body, "detail").unwrap_or("").trim_matches('"');
        let number = |k: &str| proof::field(body, k).and_then(|v| v.parse::<usize>().ok());
        let (Some(at), Some(len), Some(any)) =
            (number("d1"), number("d2"), quoted_field(body, "any"))
        else {
            return format!("a TextChanged this lab cannot read: {body}");
        };
        let any: Vec<char> = any.chars().collect();
        let fits = any.len() == len;
        changes += 1;
        match detail {
            "insert" if fits && text.get(at..at + len) == Some(&any[..]) => {
                text.drain(at..at + len);
            }
            "delete" if fits && at <= text.len() => {
                text.splice(at..at, any);
            }
            "insert" | "delete" => {
                return format!(
                    "{detail} at {at} of {len} does not fit the text the later changes leave"
                );
            }
            other => return format!("a TextChanged of kind {other:?}"),
        }
    }
    let rebuilt = if changes == 0 {
        "no text change".to_string()
    } else {
        "the text changes undo cleanly from the end walk's text".to_string()
    };
    let caret = match (last_caret.as_deref(), end_caret) {
        (None, _) => "no caret event",
        (Some(a), Some(b)) if a == b => "the last caret event names the end walk's caret",
        (Some(_), _) => "the end walk's caret is not the last caret event's",
    };
    format!("{rebuilt}; {caret}")
}

/// Count consecutive `ch` characters after the last `lane$ ` prompt on `screen`.
#[must_use]
pub fn typed_after_prompt(screen: &str, ch: char) -> usize {
    screen
        .lines()
        .rev()
        .find_map(|l| l.split_once("$ ").map(|(_, typed)| typed.to_string()))
        .map_or(0, |typed| typed.chars().filter(|c| *c == ch).count())
}

/// Parse `delay=<ms> rate=<hz>` from `xset q` output.
#[must_use]
pub fn xset_repeat(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.contains("auto repeat delay:"))?;
    let nums: Vec<&str> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|w| !w.is_empty())
        .collect();
    match nums.as_slice() {
        [delay, rate, ..] => Some(format!("delay={delay} rate={rate}")),
        _ => None,
    }
}

/// Parse `delay=<ms> rate=<hz>` from `wayland-info` output.
#[must_use]
pub fn wayland_repeat(text: &str) -> Option<String> {
    let number = |key: &str| {
        text.lines().find_map(|l| {
            l.trim()
                .strip_prefix(key)
                .map(|v| v.trim_start_matches(':').trim().to_string())
                .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        })
    };
    Some(format!(
        "delay={} rate={}",
        number("keyboard repeat delay")?,
        number("keyboard repeat rate")?
    ))
}

/// Parse sorted `interface@version` globals from `wayland-info` output.
#[must_use]
pub fn wayland_globals(text: &str) -> String {
    let mut g: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("interface: '")?;
            let (name, rest) = rest.split_once('\'')?;
            let version = rest.split("version:").nth(1)?.split(',').next()?.trim();
            Some(format!("{name}@{version}"))
        })
        .collect();
    g.sort();
    g.dedup();
    g.join(",")
}

/// Parse layout and variant from `setxkbmap -query` output.
#[must_use]
pub fn keymap_summary(text: &str) -> String {
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string()))
            .unwrap_or_default()
    };
    format!("{} {}", get("layout:"), get("variant:"))
        .trim()
        .to_string()
}

/// Parse sorted X extension names from `xdpyinfo -queryExtensions` output.
#[must_use]
pub fn x_extensions(text: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for l in text.lines() {
        if l.starts_with("number of extensions:") {
            inside = true;
            continue;
        }
        if inside {
            if l.starts_with("    ") {
                let name = l.trim().split("  (").next().unwrap_or_default();
                out.push(name.to_string());
            } else {
                break;
            }
        }
    }
    out.sort();
    out.join(",")
}

/// Extract sorted `(crate, version)` pairs for the Linux windowing and accessibility stack from `Cargo.lock`.
#[must_use]
pub fn lock_stack(lock: &str) -> Vec<(String, String)> {
    const NAMES: [&str; 29] = [
        "accesskit",
        "accesskit_atspi_common",
        "accesskit_consumer",
        "accesskit_unix",
        "accesskit_winit",
        "aterm-dl",
        "aterm-wayland",
        "aterm-window",
        "aterm-x11",
        "aterm-xkb",
        "atspi",
        "calloop",
        "calloop-wayland-source",
        "rustix",
        "sctk-adwaita",
        "smithay-client-toolkit",
        "softbuffer",
        "tiny-xlib",
        "wayland-backend",
        "wayland-client",
        "wayland-cursor",
        "wayland-protocols",
        "winit",
        "x11-dl",
        "x11rb",
        "x11rb-protocol",
        "xkbcommon-dl",
        "zbus",
        "zvariant",
    ];
    let mut out = Vec::new();
    for block in lock.split("[[package]]").skip(1) {
        let get = |k: &str| {
            block.lines().find_map(|l| {
                l.strip_prefix(&format!("{k} = \""))
                    .and_then(|v| v.strip_suffix('"'))
            })
        };
        let (Some(name), Some(version)) = (get("name"), get("version")) else {
            continue;
        };
        if NAMES.contains(&name) {
            let v = if get("source").is_some() {
                version.to_string()
            } else {
                format!("{version} (path)")
            };
            out.push((name.to_string(), v));
        }
    }
    out.sort();
    out
}

/// Guest package names recorded for `arch`.
#[must_use]
pub fn guest_packages(arch: Arch) -> Vec<String> {
    let dpkg_arch = match arch {
        Arch::Aarch64 => "arm64",
        Arch::X86_64 => "amd64",
    };
    let own = [
        "xvfb",
        "xwayland",
        "weston",
        "mutter",
        "at-spi2-core",
        "dbus",
    ];
    let libs = [
        "libwayland-client0",
        "libwayland-cursor0",
        "libx11-6",
        "libx11-xcb1",
        "libxcb1",
        "libxcursor1",
        "libxi6",
        "libxkbcommon0",
        "libxkbcommon-x11-0",
        "libegl-mesa0",
        "libgl1-mesa-dri",
        "mesa-vulkan-drivers",
        "libvulkan1",
    ];
    own.iter()
        .map(|p| (*p).to_string())
        .chain(libs.iter().map(|p| format!("{p}:{dpkg_arch}")))
        .collect()
}

/// Driver state for one scripted baseline session.
struct Session<'a> {
    gui: Gui<'a>,
    arch: Arch,
    screen: Screen,
    bus: String,
    wl: String,
    results: PathBuf,
    rec: Recording,
    roles: Vec<(String, String)>,
    end_terminal: Option<(String, Option<String>)>,
    marks: Vec<(&'static str, f64)>,
}

/// Hang-detector timeout for conditions that must complete.
const HANG: Duration = Duration::from_secs(60);

struct SeatUnit {
    name: String,
    ready: String,
    done: String,
    stop: String,
}

impl SeatUnit {
    fn done(&self, s: &Session<'_>) -> Result<(), String> {
        if s.sh(&guest::wait_file(&self.done, 60)).ok() {
            Ok(())
        } else {
            Err(format!(
                "the seat's keys never went (60 s): {}",
                s.unit_journal(&self.name)
            ))
        }
    }

    fn stop(&self, s: &Session<'_>) {
        let _ = s.sh(&["touch".into(), self.stop.clone()]);
        let _ = s.sh(&guest::wait_unit_gone(&self.name, 60));
        s.stop_unit(&self.name);
    }
}

impl Session<'_> {
    const fn up(&self) -> &Up {
        self.gui.up
    }

    fn x_display(&self) -> Option<String> {
        (self.screen.display.protocol() == "X11")
            .then(|| self.screen.display.client_env(&self.up().runtime).1)
    }

    fn wayland_display(&self) -> Option<String> {
        (self.screen.display.protocol() == "Wayland")
            .then(|| self.screen.display.client_env(&self.up().runtime).1)
    }

    fn ctl(&self, verb: &[&str]) -> Result<String, String> {
        self.gui.expect(verb, &|_| true)
    }

    fn sh(&self, argv: &[String]) -> Out {
        self.up().sh(argv)
    }

    fn x(&self, display: &str, args: &[&str]) -> Out {
        let mut argv = vec!["env".to_string(), format!("DISPLAY={display}")];
        argv.extend(args.iter().map(|s| (*s).to_string()));
        self.sh(&argv)
    }

    fn wl(&self, display: &str, args: &[&str]) -> Out {
        let mut argv = vec!["env".to_string(), format!("WAYLAND_DISPLAY={display}")];
        argv.extend(args.iter().map(|s| (*s).to_string()));
        self.sh(&argv)
    }

    fn unit_pid(&self, unit: &str) -> Option<String> {
        let out = self.sh(&[
            "systemctl".into(),
            "--user".into(),
            "show".into(),
            "--property=MainPID".into(),
            "--value".into(),
            unit.into(),
        ]);
        let pid = out.text.trim().to_string();
        (out.ok() && !pid.is_empty() && pid != "0").then_some(pid)
    }

    fn pid(&self) -> Option<String> {
        self.unit_pid(&Gui::unit_name())
    }

    fn x_window(&self, display: &str) -> Option<String> {
        let pid = self.pid()?;
        let out = self.x(
            display,
            &[
                "timeout",
                "60",
                "xdotool",
                "search",
                "--sync",
                "--onlyvisible",
                "--pid",
                &pid,
            ],
        );
        out.text
            .lines()
            .next()
            .map(str::trim)
            .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_string)
    }

    fn focus_x(&self, display: &str, win: &str) -> Result<(), String> {
        let out = self.x(display, &["xdotool", "windowfocus", "--sync", win]);
        if out.ok() {
            Ok(())
        } else {
            Err(format!(
                "the window refused the focus: {}",
                crate::util::one_line(&out.text, 200)
            ))
        }
    }

    fn settled_text(&self) -> Result<String, String> {
        let t0 = Instant::now();
        let mut last = self.ctl(&["text"])?;
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let now = self.ctl(&["text"])?;
            if now == last {
                return Ok(now);
            }
            if t0.elapsed() > HANG {
                return Err("the screen never settled (60 s)".into());
            }
            last = now;
        }
    }

    fn settled_dims(&self) -> Result<String, String> {
        let t0 = Instant::now();
        let mut last = self.ctl(&["dims"])?;
        loop {
            std::thread::sleep(Duration::from_millis(600));
            let now = self.ctl(&["dims"])?;
            if now == last {
                return Ok(now);
            }
            if t0.elapsed() > HANG {
                return Err("dims never settled (60 s)".into());
            }
            last = now;
        }
    }

    fn dims_until(&self, done: &dyn Fn(&str) -> bool) -> Result<String, String> {
        let t0 = Instant::now();
        loop {
            let d = self.ctl(&["dims"])?;
            if done(&d) {
                return Ok(d);
            }
            if t0.elapsed() > HANG {
                return Err(format!("dims never moved (60 s): {}", dims_summary(&d)));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn run_line(&self, typed: &str, want: &str) -> Result<(), String> {
        self.ctl(&["send", typed])?;
        self.ctl(&["key", "enter"])?;
        let r = self.ctl(&["await", "match", want, "timeout=60000"])?;
        if !r.starts_with("OK match") {
            return Err(format!("`{want}` never appeared: {r}"));
        }
        self.await_prompt()
    }

    fn walk(&mut self, phase: &str) {
        let argv = vec![
            "env".to_string(),
            format!(
                "DBUS_SESSION_BUS_ADDRESS=unix:path={}/bus",
                self.up().runtime
            ),
            self.bus.clone(),
            "atspi-walk".into(),
            APP.into(),
            self.pid().unwrap_or_else(|| "0".into()),
        ];
        let t0 = Instant::now();
        let out = loop {
            let out = self.sh(&argv);
            if !out.ok() || walk_settled(&out.text) || t0.elapsed() > HANG {
                break out;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        if !out.ok() {
            self.rec.behaviour(
                &format!("atspi.walk.{phase}"),
                format!("failed (exit {:?}): {}", out.code, out.tail(2)),
            );
            return;
        }
        let mut n = 0;
        for line in out.text.lines() {
            if let Some((path, rest)) = node_parts(line) {
                n += 1;
                if let Some(role) = node_role(&rest) {
                    let entry = (path.to_string(), role.to_string());
                    if !self.roles.contains(&entry) {
                        self.roles.push(entry);
                    }
                    if role == "terminal" && phase == "end" {
                        self.end_terminal = quoted_field(line, "text")
                            .map(|t| (t, proof::field(line, "caret").map(str::to_string)));
                    }
                }
                self.rec.behaviour(&format!("atspi.walk.{phase}.{n}"), rest);
                self.rec.measured(&format!("atspi.path.{phase}.{n}"), path);
            } else if !line.trim().is_empty() {
                self.rec.behaviour(&format!("atspi.walk.{phase}"), line);
            }
        }
    }

    fn resizes(&mut self) -> Result<(), String> {
        self.ctl(&["resize", "30", "100"])?;
        let d = self.dims_until(&|r| proof::dims_are(r, 30, 100))?;
        self.rec.behaviour("resize.input.30x100", dims_summary(&d));
        self.ctl(&["resize", "24", "80"])?;
        let d = self.dims_until(&|r| proof::dims_are(r, 24, 80))?;
        self.rec.behaviour("resize.input.24x80", dims_summary(&d));
        let one = self.gui.ctl_argv(&["resize", "23", "80"]);
        let two = self.gui.ctl_argv(&["resize", "24", "80"]);
        let mut argv = vec![
            "sh".to_string(),
            "-c".into(),
            "\"$@\" resize 23 80 && \"$@\" resize 24 80".into(),
            "flap".into(),
        ];
        let base = one.len() - 3;
        debug_assert_eq!(one[..base], two[..base]);
        argv.extend(one[..base].iter().cloned());
        let flap = self.sh(&argv);
        if !flap.ok() {
            return Err(format!("the flap: {}", flap.tail(2)));
        }
        let d = self.dims_until(&|r| proof::dims_are(r, 24, 80))?;
        self.rec.behaviour("resize.input.flap", dims_summary(&d));
        Ok(())
    }

    fn os_resize(&mut self) {
        let Some(display) = self.x_display() else {
            match self.screen.display.seat() {
                Some(Seat::RemoteDesktop) => self.mutter_os_resize(),
                Some(Seat::Xtest) => self.weston_os_resize(),
                None => self
                    .rec
                    .not_driven("resize.os", "no-resize-tool", RESIZE_OS_WESTON),
            }
            return;
        };
        let Some(win) = self.x_window(&display) else {
            self.rec.behaviour(
                "resize.os",
                "failed: xdotool found no window for aterm-gui's pid",
            );
            return;
        };
        let geometry = self.x(&display, &["xdotool", "getwindowgeometry", "--shell", &win]);
        let get = |k: &str| {
            geometry
                .text
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string))
        };
        let (Some(w), Some(h)) = (get("WIDTH"), get("HEIGHT")) else {
            self.rec.behaviour(
                "resize.os",
                format!("failed: no geometry: {}", geometry.tail(2)),
            );
            return;
        };
        self.rec
            .behaviour("resize.os.window_before", format!("{w}x{h}"));
        let before = self.ctl(&["dims"]).unwrap_or_default();
        let grid = |r: &str| {
            let mut it = r.split_whitespace().skip(1);
            (it.next().map(str::to_string), it.next().map(str::to_string))
        };
        let was = grid(&before);
        let _ = self.x(
            &display,
            &["xdotool", "windowsize", "--sync", &win, "1000", "700"],
        );
        match self.dims_until(&|r| grid(r) != was) {
            Ok(d) => self.rec.behaviour("resize.os.1000x700", dims_summary(&d)),
            Err(e) => self
                .rec
                .behaviour("resize.os.1000x700", format!("failed: {e}")),
        }
        let _ = self.x(&display, &["xdotool", "windowsize", "--sync", &win, &w, &h]);
        match self.dims_until(&|r| grid(r) == was) {
            Ok(d) => self.rec.behaviour("resize.os.restored", dims_summary(&d)),
            Err(e) => self
                .rec
                .behaviour("resize.os.restored", format!("failed: {e}")),
        }
    }

    fn mutter_os_resize(&mut self) {
        self.binding_os_resize("Super_L+Left", "Super+Left", "resize.os.tiled-left");
    }

    fn weston_os_resize(&mut self) {
        self.binding_os_resize("Super_L+Shift_L+m", "Super+Shift+M", "resize.os.maximized");
    }

    fn binding_os_resize(&mut self, chord: &str, shown: &str, key: &str) {
        let grid = |r: &str| {
            let mut it = r.split_whitespace().skip(1);
            (it.next().map(str::to_string), it.next().map(str::to_string))
        };
        let was = grid(&self.ctl(&["dims"]).unwrap_or_default());
        let step = format!("chord:{chord}");
        let press = |s: &Self| {
            s.seat_keys(&[step.as_str()])
                .map_err(|e| format!("{shown}: {e}"))
        };
        let moved = press(self)
            .and_then(|()| self.dims_until(&|r| grid(r) != was))
            .and_then(|_| self.settled_dims());
        let moved_ok = moved.is_ok();
        self.rec.behaviour(
            key,
            match moved {
                Ok(d) => dims_summary(&d),
                Err(e) => format!("failed: {e}"),
            },
        );
        if !moved_ok {
            return;
        }
        let back = press(self)
            .and_then(|()| self.dims_until(&|r| grid(r) == was))
            .and_then(|_| self.settled_dims());
        self.rec.behaviour(
            "resize.os.restored",
            match back {
                Ok(d) => dims_summary(&d),
                Err(e) => format!("failed: {e}"),
            },
        );
    }

    fn ledger(&mut self) {
        match self.ctl(&["status"]) {
            Ok(s) => {
                self.rec.behaviour(
                    "status",
                    format!(
                        "render={} resizes={}",
                        proof::field(&s, "render").unwrap_or("?"),
                        proof::field(&s, "resizes").unwrap_or("?"),
                    ),
                );
                self.rec
                    .measured("status.flaps", proof::field(&s, "flaps").unwrap_or("?"));
            }
            Err(e) => self.rec.behaviour("status", format!("failed: {e}")),
        }
        let out = self.gui.ctl(&["resizes"]);
        for line in out.text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("resize ") {
                let n = rest.split_whitespace().next().unwrap_or("?").to_string();
                let (behaviour, measured) = resize_line(rest);
                self.rec.behaviour(&format!("resizes.{n}"), behaviour);
                self.rec.measured(&format!("resizes.{n}.rows"), measured);
            } else if let Some(rest) = line.strip_prefix("run ") {
                let n = rest.split_whitespace().next().unwrap_or("?").to_string();
                self.rec.measured(&format!("runs.{n}"), resize_line(rest).1);
            } else if let Some(rest) = line.strip_prefix("aterm-ctl: OK ") {
                let summary: Vec<&str> = rest.split_whitespace().skip(1).collect();
                let summary = summary.join(" ");
                self.rec
                    .behaviour("resizes.summary", keep_fields(&summary, &SUMMARY_BEHAVIOUR));
                self.rec.measured("resizes.summary.detail", summary);
            }
        }
        match self.ctl(&["cast", "drift"]) {
            Ok(r) => {
                let (verdict, rest) = drift_line(&r);
                self.rec.behaviour("cast.drift", verdict);
                self.rec.measured("cast.drift.detail", rest);
            }
            Err(e) => self.rec.behaviour("cast.drift", format!("failed: {e}")),
        }
    }

    fn x_echo_cell(
        &self,
        display: &str,
        win: &str,
        row: usize,
        len: usize,
    ) -> Option<(i64, i64, i64)> {
        let d = self.ctl(&["dims"]).ok()?;
        let num = |k: &str| proof::field(&d, k).and_then(|v| v.parse::<i64>().ok());
        let geometry = self.x(display, &["xdotool", "getwindowgeometry", "--shell", win]);
        let height = geometry
            .text
            .lines()
            .find_map(|l| l.strip_prefix("HEIGHT="))
            .and_then(|v| v.trim().parse::<i64>().ok())?;
        let (cell_w, cell_h) = (num("cell_w")?, num("cell_h")?);
        let (grid_h, pad, pad_bottom) = (num("grid_h")?, num("pad")?, num("pad_bottom")?);
        let top = height - pad_bottom - grid_h;
        let row = i64::try_from(row).ok()?;
        let len = i64::try_from(len).ok()?;
        let y = top + row * cell_h + cell_h / 2;
        Some((pad + 2, y, pad + len * cell_w - 2))
    }

    fn x11_selections(&mut self, display: &str) {
        let Some(win) = self.x_window(display) else {
            self.rec.behaviour(
                "clipboard",
                "failed: xdotool found no window for aterm-gui's pid",
            );
            return;
        };
        let out = (|| -> Result<String, String> {
            let screen = self.ctl(&["text"])?;
            let row = proof::echo_row(&screen).ok_or("no echo row on screen")?;
            self.ctl(&["select", "line", &row.to_string()])?;
            let copied = self.ctl(&["copy"])?;
            let read = self.sh(&guest::read_clipboard(display, Selection::Clipboard, 60));
            Ok(format!(
                "copy {copied}; xclip read {:?}",
                read.text.lines().next().unwrap_or_default()
            ))
        })();
        self.rec.behaviour(
            "clipboard.out",
            out.unwrap_or_else(|e| format!("failed: {e}")),
        );
        let r = self.paste_from_x(display, Selection::Clipboard, "XCLIP-42", &|s| {
            s.ctl(&["invoke", "unread=ok", "Paste"]).map(drop)
        });
        self.rec.behaviour("clipboard.in", r);
        let out = (|| -> Result<String, String> {
            let screen = self.ctl(&["text"])?;
            let row = proof::echo_row(&screen).ok_or("no echo row on screen")?;
            let (x0, y, x1) = self
                .x_echo_cell(display, &win, row, proof::ECHO.len())
                .ok_or("no geometry for the echo row")?;
            let (x0, y, x1) = (x0.to_string(), y.to_string(), x1.to_string());
            let drag = self.x(
                display,
                &[
                    "xdotool",
                    "mousemove",
                    "--window",
                    &win,
                    &x0,
                    &y,
                    "mousedown",
                    "1",
                    "mousemove",
                    "--window",
                    &win,
                    &x1,
                    &y,
                    "mouseup",
                    "1",
                ],
            );
            if !drag.ok() {
                return Err(format!("xdotool drag: {}", drag.tail(2)));
            }
            let _ = self.sh(&guest::wait_clipboard(
                display,
                Selection::Primary,
                proof::ECHO,
                60,
            ));
            let read = self.sh(&guest::read_clipboard(display, Selection::Primary, 60));
            Ok(format!(
                "drag; xclip read {:?}",
                read.text.lines().next().unwrap_or_default()
            ))
        })();
        self.rec.behaviour(
            "primary.out",
            out.unwrap_or_else(|e| format!("failed: {e}")),
        );
        let r = self.paste_from_x(display, Selection::Primary, "XPRIMARY-42", &|s| {
            let click = s.x(
                display,
                &[
                    "xdotool",
                    "mousemove",
                    "--window",
                    &win,
                    "200",
                    "200",
                    "click",
                    "2",
                ],
            );
            if click.ok() {
                Ok(())
            } else {
                Err(format!("xdotool click: {}", click.tail(2)))
            }
        });
        self.rec.behaviour("primary.in", r);
    }

    fn matched(&self, pattern: &str) -> bool {
        let r = self.gui.ctl(&["await", "match", pattern, "timeout=60000"]);
        r.text.trim_start().starts_with("OK match")
    }

    fn clear_line(&self) {
        let typed = self
            .ctl(&["text"])
            .map(|screen| typed_line(&screen))
            .unwrap_or_default();
        if !typed.is_empty() {
            let _ = self.ctl(&["key", "ctrl+u"]);
            let _ = self.matched(&format!("^{}\\s*$", regex_literal(proof::PROMPT)));
        }
    }

    fn typed(&self, typed: &str) -> Result<(), String> {
        if self.matched(&regex_literal(typed)) {
            Ok(())
        } else {
            Err(format!("`{typed}` never reached the typed line (60 s)"))
        }
    }

    fn await_prompt(&self) -> Result<(), String> {
        if self.matched(&format!("^{}\\s*$", regex_literal(proof::PROMPT))) {
            Ok(())
        } else {
            Err(format!("prompt `{}` never returned (60 s)", proof::PROMPT))
        }
    }

    fn appeared(&self, want: &str) -> Result<(), String> {
        if self.matched(&regex_literal(want)) {
            self.await_prompt()
        } else {
            Err(format!("{want} never appeared (60 s)"))
        }
    }

    fn paste_from_x(
        &self,
        display: &str,
        sel: Selection,
        text: &str,
        paste: &dyn Fn(&Self) -> Result<(), String>,
    ) -> String {
        let want = format!("x-{}", text.to_lowercase());
        let r = (|| -> Result<(), String> {
            let owner = guest::clipboard_owner(display, sel, text);
            if !self.sh(&guest::run_unit(&owner, None)).ok() {
                return Err("xclip as owner did not start".into());
            }
            if !self.sh(&guest::wait_clipboard(display, sel, text, 60)).ok() {
                return Err(format!("xclip never owned {} (60 s)", sel.xclip()));
            }
            self.ctl(&["send", proof::PASTE_BEFORE])?;
            paste(self)?;
            self.typed(&format!("{}{text}", proof::PASTE_BEFORE))?;
            self.ctl(&["send", proof::PASTE_AFTER])?;
            self.ctl(&["key", "enter"])?;
            self.appeared(&want)
        })();
        let _ = self.sh(&[
            "systemctl".into(),
            "--user".into(),
            "stop".into(),
            format!("{}xclip", guest::UNIT_PREFIX),
        ]);
        match r {
            Ok(()) => format!("xclip's {text} pasted: {want}"),
            Err(e) => format!("failed: {e}"),
        }
    }

    fn wl_paste_until(&self, wayland: &str, text: &str, primary: bool) -> Result<String, String> {
        let flags = if primary {
            "--primary --no-newline"
        } else {
            "--no-newline"
        };
        let read = self.wl(
            wayland,
            &[
                "timeout",
                "60",
                "sh",
                "-c",
                "until [ \"$(wl-paste $2 2>/dev/null)\" = \"$1\" ]; do sleep 0.2; done; wl-paste $2",
                "wait-paste",
                text,
                flags,
            ],
        );
        if read.ok() {
            Ok(read.text.trim().to_string())
        } else {
            Err(format!("wl-paste {flags} never read {text} (60 s)"))
        }
    }

    fn wl_selections(&mut self, wayland: &str) {
        let out = (|| -> Result<String, String> {
            let screen = self.ctl(&["text"])?;
            let row = proof::echo_row(&screen).ok_or("no echo row on screen")?;
            self.ctl(&["select", "line", &row.to_string()])?;
            let seat = self.seat_unit(&["chord:ctrl+shift+c"])?;
            let read = seat
                .done(self)
                .and_then(|()| self.wl_paste_until(wayland, proof::ECHO, false));
            seat.stop(self);
            Ok(format!("Ctrl+Shift+C; wl-paste read {:?}", read?))
        })();
        self.rec.behaviour(
            "clipboard.out",
            out.unwrap_or_else(|e| format!("failed: {e}")),
        );
        let text = "WLCOPY-42";
        let want = "x-wlcopy-42";
        let r = (|| -> Result<(), String> {
            let holder = self.seat_unit(&[])?;
            let copier = guest::Unit {
                name: format!("{}wlcopy", guest::UNIT_PREFIX),
                env: vec![("WAYLAND_DISPLAY".into(), wayland.into())],
                argv: vec!["wl-copy".into(), "--foreground".into(), text.into()],
                socket: String::new(),
            };
            let owned = if self.sh(&guest::run_unit(&copier, None)).ok() {
                self.wl_paste_until(wayland, text, false).map(drop)
            } else {
                Err("wl-copy did not start".into())
            };
            holder.stop(self);
            owned.map_err(|_| "wl-copy never owned the clipboard (60 s)".to_string())?;
            self.ctl(&["send", proof::PASTE_BEFORE])?;
            let seat = self.seat_unit(&["chord:ctrl+shift+v"])?;
            let pasted = seat
                .done(self)
                .and_then(|()| self.typed(&format!("{}{text}", proof::PASTE_BEFORE)));
            seat.stop(self);
            pasted?;
            self.ctl(&["send", proof::PASTE_AFTER])?;
            self.ctl(&["key", "enter"])?;
            self.appeared(want)
        })();
        self.stop_unit(&format!("{}wlcopy", guest::UNIT_PREFIX));
        self.rec.behaviour(
            "clipboard.in",
            match r {
                Ok(()) => format!("wl-copy's {text} pasted by Ctrl+Shift+V: {want}"),
                Err(e) => format!("failed: {e}"),
            },
        );
        self.wl_primary(wayland);
    }

    fn wl_primary(&mut self, wayland: &str) {
        let info = self.wl(wayland, &["wayland-info"]);
        let globals = wayland_globals(&info.text);
        if !info.ok() || globals.is_empty() {
            self.rec
                .behaviour("primary", format!("failed: wayland-info: {}", info.tail(2)));
            return;
        }
        if !globals.contains(PRIMARY_GLOBAL) {
            self.rec.behaviour(
                "primary",
                format!("none: the compositor offers no {PRIMARY_GLOBAL}"),
            );
            return;
        }
        let out = self.wl_primary_out(wayland);
        self.rec.behaviour(
            "primary.out",
            out.unwrap_or_else(|e| format!("failed: {e}")),
        );
        let text = "WLPRIMARY-42";
        let want = "x-wlprimary-42";
        let r = self.wl_primary_in(wayland, text, want);
        self.stop_unit(&format!("{}wlcopy", guest::UNIT_PREFIX));
        self.rec.behaviour(
            "primary.in",
            match r {
                Ok(()) => format!("wl-copy --primary's {text} pasted by a middle click: {want}"),
                Err(e) => format!("failed: {e}"),
            },
        );
    }

    fn echo_span(&self) -> Result<(Px, Px), String> {
        let screen = self.ctl(&["text"])?;
        let row = proof::echo_row(&screen).ok_or("no echo row on screen")?;
        let d = self.ctl(&["dims"])?;
        let num = |k: &str| {
            proof::field(&d, k)
                .and_then(|v| v.parse::<i32>().ok())
                .ok_or_else(|| format!("dims has no {k}"))
        };
        let (cell_w, cell_h, pad) = (num("cell_w")?, num("cell_h")?, num("pad")?);
        let top = num("frame_h")? - num("pad_bottom")? - num("grid_h")?;
        let row = i32::try_from(row).map_err(|_| "a row past i32")?;
        let len = i32::try_from(proof::ECHO.len()).map_err(|_| "a length past i32")?;
        let y = top + row * cell_h + cell_h / 2;
        Ok(((pad + 2, y), (pad + len * cell_w - 2, y)))
    }

    fn dims_scale(&self) -> f64 {
        self.ctl(&["dims"])
            .ok()
            .and_then(|d| proof::field(&d, "scale").and_then(|v| v.parse::<f64>().ok()))
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0)
    }

    fn wl_primary_out(&mut self, wayland: &str) -> Result<String, String> {
        let marks = format!("{}/primary", self.gui.dir);
        let mark = |name: &str| format!("{marks}/{name}");
        let fresh = self.sh(&[
            "sh".into(),
            "-c".into(),
            "rm -rf \"$1\" && install -d \"$1\"".into(),
            "marks".into(),
            marks.clone(),
        ]);
        if !fresh.ok() {
            return Err(format!("{marks}: {}", fresh.tail(2)));
        }
        self.clear_line();
        let scale = self.dims_scale();
        let logical = |px: i32| (f64::from(px) / scale).round() as i32;
        let device = |lp: i32| (f64::from(lp) * scale).round() as i32;
        let slack = scale.ceil() as i32;
        let (start, end) = self.echo_span()?;
        let steps = [
            "move:640,400".to_string(),
            format!("touch:{}", mark("centred")),
            format!("wait:{}", mark("aim-start")),
            format!("move:@{}", mark("to-start")),
            format!("touch:{}", mark("at-start")),
            format!("wait:{}", mark("drag")),
            "press:left".to_string(),
            "sleep:200".to_string(),
            format!("move:@{}", mark("to-end")),
            "sleep:300".to_string(),
            "release:left".to_string(),
            format!("touch:{}", mark("dragged")),
        ];
        let words: Vec<&str> = steps.iter().map(String::as_str).collect();
        let seat = self.seat_unit(&words)?;
        let go = |name: &str| {
            let _ = self.sh(&["touch".into(), mark(name)]);
        };
        let r = (|| -> Result<String, String> {
            self.seat_reached(&seat, &marks, "centred")?;
            let (x, y) = self.settled_px().map_err(|e| format!("centred: {e}"))?;
            let (dx, dy) = (logical(start.0 - x), logical(start.1 - y));
            self.put_file(&mark("to-start"), &format!("{dx} {dy}"))?;
            let at = (x + device(dx), y + device(dy));
            go("aim-start");
            self.seat_reached(&seat, &marks, "at-start")?;
            self.px_at(at, slack)
                .map_err(|e| format!("on the echo row: {e}"))?;
            self.put_file(&mark("to-end"), &format!("{} 0", logical(end.0 - start.0)))?;
            go("drag");
            self.seat_reached(&seat, &marks, "dragged")?;
            let read = self.wl_paste_until(wayland, proof::ECHO, true)?;
            Ok(format!("drag; wl-paste --primary read {read:?}"))
        })();
        let done = if r.is_ok() { seat.done(self) } else { Ok(()) };
        seat.stop(self);
        let out = r?;
        done?;
        Ok(out)
    }

    fn wl_primary_in(&mut self, wayland: &str, text: &str, want: &str) -> Result<(), String> {
        let holder = self.seat_unit(&[])?;
        let copier = guest::Unit {
            name: format!("{}wlcopy", guest::UNIT_PREFIX),
            env: vec![("WAYLAND_DISPLAY".into(), wayland.into())],
            argv: vec![
                "wl-copy".into(),
                "--primary".into(),
                "--foreground".into(),
                text.into(),
            ],
            socket: String::new(),
        };
        let owned = if self.sh(&guest::run_unit(&copier, None)).ok() {
            self.wl_paste_until(wayland, text, true).map(drop)
        } else {
            Err("wl-copy did not start".into())
        };
        holder.stop(self);
        owned.map_err(|_| "wl-copy never owned PRIMARY (60 s)".to_string())?;
        self.ctl(&["send", proof::PASTE_BEFORE])?;
        let marks = format!("{}/primary-in", self.gui.dir);
        let mark = |name: &str| format!("{marks}/{name}");
        let fresh = self.sh(&[
            "sh".into(),
            "-c".into(),
            "rm -rf \"$1\" && install -d \"$1\"".into(),
            "marks".into(),
            marks.clone(),
        ]);
        if !fresh.ok() {
            return Err(format!("{marks}: {}", fresh.tail(2)));
        }
        let scale = self.dims_scale();
        let logical = |px: i32| (f64::from(px) / scale).round() as i32;
        let d = self.ctl(&["dims"])?;
        let num = |k: &str| {
            proof::field(&d, k)
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(0)
        };
        let grid_mid = (
            num("pad") + num("grid_w") / 2,
            num("frame_h") - num("pad_bottom") - num("grid_h") / 2,
        );
        let steps = [
            "move:640,400".to_string(),
            format!("touch:{}", mark("centred")),
            format!("wait:{}", mark("aim")),
            format!("move:@{}", mark("to-grid")),
            "sleep:200".to_string(),
            "button:middle".to_string(),
        ];
        let words: Vec<&str> = steps.iter().map(String::as_str).collect();
        let seat = self.seat_unit(&words)?;
        let r = (|| -> Result<(), String> {
            self.seat_reached(&seat, &marks, "centred")?;
            let (x, y) = self.settled_px().map_err(|e| format!("centred: {e}"))?;
            let (dx, dy) = (logical(grid_mid.0 - x), logical(grid_mid.1 - y));
            self.put_file(&mark("to-grid"), &format!("{dx} {dy}"))?;
            let _ = self.sh(&["touch".into(), mark("aim")]);
            seat.done(self)?;
            self.typed(&format!("{}{text}", proof::PASTE_BEFORE))
        })();
        seat.stop(self);
        r?;
        self.ctl(&["send", proof::PASTE_AFTER])?;
        self.ctl(&["key", "enter"])?;
        self.appeared(want)
    }

    fn seat_unit(&self, steps: &[&str]) -> Result<SeatUnit, String> {
        self.seat_unit_of(&format!("{}mutter", guest::UNIT_PREFIX), APP, steps)
    }

    fn seat_unit_of(
        &self,
        compositor: &str,
        app: &str,
        steps: &[&str],
    ) -> Result<SeatUnit, String> {
        let dir = format!("{}/seat", self.gui.dir);
        let seat = SeatUnit {
            name: format!("{}seat", guest::UNIT_PREFIX),
            ready: format!("{dir}/ready"),
            done: format!("{dir}/done"),
            stop: format!("{dir}/stop"),
        };
        let fresh = self.sh(&[
            "sh".into(),
            "-c".into(),
            "rm -rf \"$1\" && install -d \"$1\"".into(),
            "seat-dir".into(),
            dir.clone(),
        ]);
        if !fresh.ok() {
            return Err(format!("{dir}: {}", fresh.tail(2)));
        }
        let mut env = vec![(
            "DBUS_SESSION_BUS_ADDRESS".to_string(),
            format!("unix:path={}/bus", self.up().runtime),
        )];
        let mut argv = vec![self.bus.clone()];
        match (self.screen.display.seat(), self.screen.display.input_x()) {
            (Some(Seat::Xtest), Some(x)) => {
                argv.push("xseat".into());
                env.push(("DISPLAY".into(), x.into()));
            }
            _ => {
                argv.push("seat".into());
                argv.push(self.unit_pid(compositor).unwrap_or_else(|| "0".into()));
            }
        }
        let verb = argv[1].clone();
        argv.extend([
            app.into(),
            self.pid().unwrap_or_else(|| "0".into()),
            format!("touch:{}", seat.ready),
        ]);
        argv.extend(steps.iter().map(|s| (*s).to_string()));
        argv.push(format!("touch:{}", seat.done));
        argv.push(format!("wait:{}", seat.stop));
        let unit = guest::Unit {
            name: seat.name.clone(),
            env,
            argv,
            socket: String::new(),
        };
        if !self.sh(&guest::run_unit(&unit, None)).ok() {
            return Err(format!("lane-bus {verb} did not start"));
        }
        if !self.sh(&guest::wait_file(&seat.ready, 60)).ok() {
            let why = self.unit_journal(&seat.name);
            seat.stop(self);
            return Err(format!(
                "the seat's keyboard never had the window's focus (60 s): {why}"
            ));
        }
        Ok(seat)
    }

    fn unit_journal(&self, name: &str) -> String {
        let out = self.sh(&[
            "journalctl".into(),
            "--user".into(),
            "--no-pager".into(),
            "-o".into(),
            "cat".into(),
            "-n".into(),
            "3".into(),
            "-u".into(),
            name.into(),
        ]);
        crate::util::one_line(&out.text, 300)
    }

    fn stop_unit(&self, name: &str) {
        let _ = self.sh(&[
            "systemctl".into(),
            "--user".into(),
            "stop".into(),
            name.into(),
        ]);
    }

    fn x11_wire(&mut self) {
        let Some(display) = self.x_display() else {
            return;
        };
        let r = self.x11_wire_run(&display);
        self.gui.stop();
        self.stop_unit(&xwire::unit_name());
        match r {
            Ok(lines) => {
                for (key, value) in lines {
                    self.rec.behaviour(key, value);
                }
            }
            Err(e) => self.rec.behaviour("x11.wire", format!("failed: {e}")),
        }
    }

    fn x11_wire_run(&self, display: &str) -> Result<Vec<(&'static str, String)>, String> {
        let log = format!("{}/xtrace.txt", self.gui.dir);
        let stderr = format!("{}/xtrace-gui.stderr", self.gui.dir);
        self.gui.prepare()?;
        let proxy = guest::Unit {
            name: xwire::unit_name(),
            env: Vec::new(),
            argv: xwire::proxy_argv(display, &log),
            socket: guest::x_socket(xwire::PROXY),
        };
        let _ = self.sh(&["rm".into(), "-f".into(), proxy.socket.clone()]);
        let started = self.sh(&guest::run_unit(&proxy, None));
        if !started.ok() {
            return Err(format!("could not start xtrace: {}", started.tail(3)));
        }
        if !self.sh(&guest::wait_socket(&proxy.socket, 60)).ok() {
            return Err(format!(
                "xtrace never listened (60 s): {}",
                self.unit_journal(&proxy.name)
            ));
        }
        let invocation = self
            .gui
            .start_logged_on(&[("DISPLAY".into(), xwire::PROXY.into())], &stderr)?;
        if !self.sh(&guest::wait_socket(&self.gui.sock(), 60)).ok() {
            return Err(format!(
                "aterm-gui never listened through xtrace (60 s): {}",
                crate::util::one_line(&self.gui.journal(&invocation), 300)
            ));
        }
        let step = |verb: &[&str], ok: &dyn Fn(&str) -> bool| -> Result<(), String> {
            let r = self.ctl(verb)?;
            if ok(&r) {
                Ok(())
            } else {
                Err(format!(
                    "`{}` answered {:?}",
                    verb.join(" "),
                    crate::util::one_line(&r, 200)
                ))
            }
        };
        step(&["ready", "60000"], &|r| r.starts_with("OK ready"))?;
        step(&["send", xwire::TYPED], &|r| r.starts_with("OK"))?;
        step(&["key", "enter"], &|r| r.starts_with("OK"))?;
        let awaited = format!("timeout={}", 60_000);
        step(&["await", "match", xwire::ECHO, &awaited], &|r| {
            r.starts_with("OK match")
        })?;
        step(&["resize", "30", "100"], &|r| r.starts_with("OK"))?;
        step(&["status"], &|r| proof::field(r, "render") == Some("ok"))?;
        step(&["invoke", "Quit"], &|r| r.starts_with("OK"))?;
        if !self.sh(&guest::wait_unit_gone(&Gui::unit_name(), 60)).ok() {
            return Err("aterm-gui still ran 60 s after its Quit".into());
        }
        self.stop_unit(&proxy.name);
        let wire = self.sh(&["cat".into(), log]);
        let _ = std::fs::write(
            self.results.join(format!(
                "{}-{}-xtrace.txt",
                self.arch.name(),
                self.screen.name()
            )),
            &wire.text,
        );
        if !wire.ok() || wire.text.trim().is_empty() {
            return Err(format!("xtrace logged nothing: {}", wire.tail(2)));
        }
        Ok(xwire::summary(&wire.text))
    }

    fn key_repeat(&mut self) {
        let held = match (self.x_display(), self.screen.display.seat()) {
            (Some(display), _) => {
                let Some(win) = self.x_window(&display) else {
                    self.rec
                        .behaviour("keyrepeat", "failed: xdotool found no window");
                    return;
                };
                let xset = self.x(&display, &["xset", "q"]);
                if let Some(r) = xset_repeat(&xset.text) {
                    self.rec
                        .stack("keyrepeat.server", format!("X server autorepeat {r}"));
                }
                if let Err(e) = self.focus_x(&display, &win) {
                    self.rec.behaviour("keyrepeat", format!("failed: {e}"));
                    return;
                }
                self.clear_line();
                let hold = format!("{:.1}", Duration::from_millis(HOLD_MS).as_secs_f64());
                let out = self.x(
                    &display,
                    &["xdotool", "keydown", "a", "sleep", &hold, "keyup", "a"],
                );
                if out.ok() {
                    Ok(None)
                } else {
                    Err(format!("the hold: {}", out.tail(2)))
                }
            }
            (None, Some(_)) => {
                self.clear_line();
                let hold = format!("sleep:{HOLD_MS}");
                self.seat_unit(&["down:a", &hold, "up:a"])
                    .and_then(|seat| match seat.done(self) {
                        Ok(()) => Ok(Some(seat)),
                        Err(e) => {
                            seat.stop(self);
                            Err(e)
                        }
                    })
            }
            (None, _) => {
                self.rec.not_driven(
                    "keyrepeat",
                    "no-seat",
                    "weston's headless backend advertises no wl_seat, so no key reaches a client (the weston-x11 screen drives it: weston's x11 backend, nested in Xvfb, has a seat that XTEST on the Xvfb drives)",
                );
                return;
            }
        };
        let seat = match held {
            Ok(seat) => seat,
            Err(e) => {
                self.rec.behaviour("keyrepeat", format!("failed: {e}"));
                return;
            }
        };
        if let (Some(w), Some(_)) = (self.wayland_display(), &seat) {
            let info = self.wl(&w, &["wayland-info"]);
            if let Some(r) = wayland_repeat(&info.text) {
                self.rec
                    .stack("keyrepeat.server", format!("wl_keyboard repeat_info {r}"));
            }
        }
        let settled = self.settled_text();
        if let Some(seat) = seat {
            seat.stop(self);
        }
        match settled {
            Ok(screen) => {
                let n = typed_after_prompt(&screen, 'a');
                self.rec.behaviour(
                    "keyrepeat",
                    match n {
                        0 => "nothing typed".to_string(),
                        1 => "one character: no repeat".to_string(),
                        _ => "repeats".to_string(),
                    },
                );
                self.rec.measured(
                    "keyrepeat.count",
                    format!("{n} characters for a {HOLD_MS} ms hold"),
                );
            }
            Err(e) => self.rec.behaviour("keyrepeat", format!("failed: {e}")),
        }
        self.clear_line();
    }

    fn ime_events(&self) -> Option<u64> {
        self.ctl(&["metrics", "percentiles"])
            .ok()
            .and_then(|m| proof::field(&m, "n_ime").and_then(|v| v.parse().ok()))
    }

    fn seat_keys(&self, steps: &[&str]) -> Result<(), String> {
        let seat = self.seat_unit(steps)?;
        let done = seat.done(self);
        seat.stop(self);
        done
    }

    fn composed(&self, typist: Typist<'_>) -> String {
        let before = self.ime_events();
        let r = (|| -> Result<String, String> {
            self.clear_line();
            self.ctl(&["send", COMPOSE_BEFORE])?;
            match typist {
                Typist::X { display, .. } => {
                    let plain = self.x(display, &["xdotool", "key", "x"]);
                    if !plain.ok() {
                        return Err(format!("the plain key: {}", plain.tail(2)));
                    }
                }
                Typist::Seat => self.seat_keys(&["tap:x"])?,
            }
            self.typed(COMPOSE_LIVE)
                .map_err(|_| format!("a plain key never reached the screen ({COMPOSE_LIVE})"))?;
            let composed = format!("{COMPOSE_LIVE}\u{e9}");
            match typist {
                Typist::X {
                    display,
                    server: true,
                } => self.held_letter(display, &composed)?,
                Typist::X { display, .. } => {
                    let typed = self.x(display, &["xdotool", "key", "dead_acute", "e"]);
                    if !typed.ok() {
                        return Err(format!("the keys: {}", typed.tail(2)));
                    }
                    let _ = self.typed(&composed);
                }
                Typist::Seat => {
                    self.seat_keys(&["tap:dead_acute", "tap:e"])?;
                    let _ = self.typed(&composed);
                }
            }
            self.ctl(&["send", COMPOSE_AFTER])?;
            self.ctl(&["key", "enter"])?;
            if self.appeared(COMPOSED).is_ok() {
                Ok(format!("{COMPOSED} printed"))
            } else {
                let screen = self.ctl(&["text"]).unwrap_or_default();
                let shown = screen
                    .lines()
                    .rev()
                    .find(|l| l.starts_with("X-"))
                    .unwrap_or("nothing")
                    .to_string();
                Ok(format!(
                    "{COMPOSED} never printed; the shell printed {shown:?}"
                ))
            }
        })();
        let ime = match (before, self.ime_events()) {
            (Some(b), Some(a)) if a > b => "IME events",
            (Some(_), Some(_)) => "no IME event",
            _ => "IME events unread",
        };
        match r {
            Ok(what) => format!("{what}; {ime}"),
            Err(e) => format!("failed: {e}"),
        }
    }

    fn held_letter(&self, display: &str, composed: &str) -> Result<(), String> {
        let off = self.x(display, &["xset", "r", "off"]);
        if !off.ok() {
            return Err(format!("autorepeat stayed on: {}", off.tail(2)));
        }
        let down = self.x(display, &["xdotool", "key", "dead_acute", "keydown", "e"]);
        if down.ok() {
            let _ = self.typed(composed);
        }
        let up = self.x(display, &["xdotool", "keyup", "e"]);
        let _ = self.x(display, &["xset", "r", "on"]);
        if !down.ok() {
            return Err(format!("the keys: {}", down.tail(2)));
        }
        if !up.ok() {
            return Err(format!("the letter's release: {}", up.tail(2)));
        }
        Ok(())
    }

    fn compose(&mut self) {
        let r = match (self.x_display(), self.screen.display.seat()) {
            (Some(display), _) => {
                let Some(win) = self.x_window(&display) else {
                    self.rec
                        .behaviour("compose", "failed: xdotool found no window");
                    return;
                };
                if let Err(e) = self.focus_x(&display, &win) {
                    self.rec.behaviour("compose", format!("failed: {e}"));
                    return;
                }
                self.composed(Typist::X {
                    display: &display,
                    server: false,
                })
            }
            (None, Some(_)) => self.composed(Typist::Seat),
            (None, _) => {
                self.rec.not_driven(
                    "compose",
                    "no-seat",
                    "weston's headless backend advertises no wl_seat, so no key reaches a client (the weston-x11 screen drives it: weston's x11 backend, nested in Xvfb, has a seat that XTEST on the Xvfb drives)",
                );
                return;
            }
        };
        self.rec.behaviour("compose", r);
    }

    fn frame(&self, name: &str) -> Result<Frame, String> {
        let reply = self.ctl(&["image", "plain", name])?;
        let guest_path = reply
            .split_whitespace()
            .nth(3)
            .ok_or_else(|| format!("`image` answered {reply}"))?;
        let file = format!("{}-{}-{name}", self.arch.name(), self.screen.name());
        let to = self.results.join(&file);
        let copied = self.up().guest.copy_out(guest_path, &to);
        if !copied.ok() {
            return Err(format!("{guest_path} not copied out: {}", copied.tail(2)));
        }
        let bytes = std::fs::read(&to).map_err(|e| format!("{}: {e}", to.display()))?;
        aterm_png::decode_rgba8(&bytes, 8192)
            .map(|(rgba, w, h)| Frame { rgba, w, h })
            .ok_or_else(|| format!("{}: not a PNG this lab decodes", to.display()))
    }

    fn dnd(&mut self, display: &str) {
        let marks = format!("{}/dnd", self.gui.dir);
        let report = format!("{marks}/report");
        let r = (|| -> Result<(), String> {
            let win = self
                .x_window(display)
                .ok_or("xdotool found no window for aterm-gui's pid")?;
            let made = self.sh(&[
                "sh".into(),
                "-c".into(),
                "rm -rf \"$1\" \"$2\" && install -d \"$1\" \"$2\" && : > \"$1/$3\"".into(),
                "dnd-files".into(),
                DND_DIR.into(),
                marks.clone(),
                DND_FILE.into(),
            ]);
            if !made.ok() {
                return Err(format!("the dropped file: {}", made.tail(2)));
            }
            self.clear_line();
            let rest = self.frame("dnd-rest.png")?;
            let source = guest::Unit {
                name: format!("{}xdnd", guest::UNIT_PREFIX),
                env: vec![("DISPLAY".into(), display.into())],
                argv: vec![
                    self.bus.clone(),
                    "xdnd".into(),
                    win,
                    DND_URI.into(),
                    marks.clone(),
                ],
                socket: String::new(),
            };
            if !self.sh(&guest::run_unit(&source, None)).ok() {
                return Err("lane-bus xdnd did not start".into());
            }
            let mut frames = Vec::new();
            for (mark, name) in [("hovering", "dnd-hover.png"), ("left", "dnd-left.png")] {
                if !self
                    .sh(&guest::wait_file(&format!("{marks}/{mark}"), 60))
                    .ok()
                {
                    self.record_dnd_report(&report);
                    let why = self.unit_journal(&source.name);
                    self.stop_unit(&source.name);
                    return Err(format!("the drag never reached `{mark}` (60 s): {why}"));
                }
                frames.push(self.frame(name));
                let _ = self.sh(&["touch".into(), format!("{marks}/{mark}.seen")]);
            }
            let ended = self.sh(&guest::wait_unit_gone(&source.name, 60)).ok();
            self.record_dnd_report(&report);
            if !ended {
                self.stop_unit(&source.name);
                return Err("the drag source never ended (60 s)".into());
            }
            let changes: Vec<(String, String)> = frames
                .into_iter()
                .map(|f| match f {
                    Ok(f) => frame_change(&rest, &f),
                    Err(e) => (format!("failed: {e}"), String::new()),
                })
                .collect();
            let (hover, left) = match changes.as_slice() {
                [hover, left] => (hover.clone(), left.clone()),
                _ => return Err("two frames".into()),
            };
            self.rec.behaviour(
                "dnd.highlight",
                format!(
                    "hovered against rest: {}; after the leave against rest: {}",
                    hover.0, left.0
                ),
            );
            self.rec.measured(
                "dnd.highlight.pixels",
                format!("hovered {}; after the leave {}", hover.1, left.1),
            );
            let dropped = self.typed(DND_STEM);
            let screen = self.ctl(&["text"])?;
            self.rec.behaviour(
                "dnd.typed",
                match dropped {
                    Ok(()) => typed_line(&screen),
                    Err(e) => format!("failed: {e}; the prompt line {:?}", typed_line(&screen)),
                },
            );
            Ok(())
        })();
        if let Err(e) = r {
            self.rec.behaviour("dnd", format!("failed: {e}"));
        }
        self.clear_line();
    }

    fn wl_dnd(&mut self, wayland: &str) {
        let marks = format!("{}/dnd", self.gui.dir);
        let report = format!("{marks}/report");
        let source = guest::Unit {
            name: format!("{}lane-wl", guest::UNIT_PREFIX),
            env: vec![("WAYLAND_DISPLAY".into(), wayland.into())],
            argv: vec![
                self.wl.clone(),
                "drag".into(),
                DND_URI.into(),
                marks.clone(),
            ],
            socket: String::new(),
        };
        let mark = |name: &str| format!("{marks}/{name}");
        let r = (|| -> Result<(), String> {
            let made = self.sh(&[
                "sh".into(),
                "-c".into(),
                "rm -rf \"$1\" \"$2\" && install -d \"$1\" \"$2\" && : > \"$1/$3\"".into(),
                "dnd-files".into(),
                DND_DIR.into(),
                marks.clone(),
                DND_FILE.into(),
            ]);
            if !made.ok() {
                return Err(format!("the dropped file: {}", made.tail(2)));
            }
            self.clear_line();
            let rest = self.frame("dnd-rest.png")?;
            let steps = [
                "move:640,400".to_string(),
                format!("touch:{}", mark("centred")),
                format!("wait:{}", mark("mapped")),
                "move:1,0".to_string(),
                "move:-1,0".to_string(),
                format!("wait:{}", mark("pointed")),
                "press:left".to_string(),
                format!("wait:{}", mark("dragging")),
                "move:1,1".to_string(),
                format!("touch:{}", mark("over")),
                format!("wait:{}", mark("hovered")),
                format!("move:@{}", mark("to-away")),
                format!("touch:{}", mark("away")),
                format!("wait:{}", mark("left")),
                format!("move:@{}", mark("to-back")),
                format!("touch:{}", mark("back")),
                format!("wait:{}", mark("returned")),
                "release:left".to_string(),
                format!("touch:{}", mark("released")),
            ];
            let words: Vec<&str> = steps.iter().map(String::as_str).collect();
            let seat = self.seat_unit(&words)?;
            let go = |name: &str| {
                let _ = self.sh(&["touch".into(), mark(name)]);
            };
            let mut frames = Vec::new();
            let mut heard = Vec::new();
            let dragged = (|| -> Result<(), String> {
                self.seat_reached(&seat, &marks, "centred")?;
                let (x, y) = self.settled_px().map_err(|e| format!("centred: {e}"))?;
                let scale = self.dims_scale();
                let logical = |px: i32| (f64::from(px) / scale).round() as i32;
                let (ax, ay) = (-logical(x) - 21, -logical(y) - 21);
                self.put_file(&mark("to-away"), &format!("{ax} {ay}"))?;
                self.put_file(&mark("to-back"), &format!("{} {}", -ax, -ay))?;
                if !self.sh(&guest::run_unit(&source, None)).ok() {
                    return Err("lane-wl drag did not start".into());
                }
                let mut seen = 0;
                for (point, answer, want, frame) in [
                    (
                        "over",
                        "hovered",
                        ("text/uri-list", Some("copy")),
                        Some("dnd-hover.png"),
                    ),
                    ("away", "left", ("none", None), Some("dnd-left.png")),
                    ("back", "returned", ("text/uri-list", Some("copy")), None),
                ] {
                    self.seat_reached(&seat, &marks, point)?;
                    let (new, lines) =
                        self.source_heard_until(&report, seen, want).map_err(|e| {
                            format!("{e}; the source: {}", self.unit_journal(&source.name))
                        })?;
                    seen = lines;
                    heard.push((point, new));
                    if let Some(name) = frame {
                        frames.push(self.frame(name));
                    }
                    go(answer);
                }
                self.seat_reached(&seat, &marks, "released")?;
                if !self.sh(&guest::wait_unit_gone(&source.name, 60)).ok() {
                    return Err(format!(
                        "the drag source never heard the drag end (60 s): {}",
                        self.unit_journal(&source.name)
                    ));
                }
                Ok(())
            })();
            let done = if dragged.is_ok() {
                seat.done(self)
            } else {
                Ok(())
            };
            seat.stop(self);
            self.stop_unit(&source.name);
            let events = self.sh(&["cat".into(), report.clone()]).text;
            for (point, new) in &heard {
                self.rec
                    .behaviour(&format!("dnd.source.{point}"), new.clone());
            }
            dragged?;
            done?;
            let tail: Vec<&str> = events
                .lines()
                .rev()
                .take_while(|l| !l.starts_with("action ") && !l.starts_with("target "))
                .collect();
            let mut drop_seen: Vec<&str> = tail.into_iter().rev().collect();
            drop_seen.dedup();
            self.rec.behaviour(
                "dnd.source.drop",
                if drop_seen.is_empty() {
                    "nothing".to_string()
                } else {
                    drop_seen.join(", ")
                },
            );
            self.rec
                .measured("dnd.source.events", one_line(events.trim_end()));
            let changes: Vec<(String, String)> = frames
                .into_iter()
                .map(|f| match f {
                    Ok(f) => frame_change(&rest, &f),
                    Err(e) => (format!("failed: {e}"), String::new()),
                })
                .collect();
            let [hover, left] = changes.as_slice() else {
                return Err("two frames".into());
            };
            self.rec.behaviour(
                "dnd.highlight",
                format!(
                    "hovered against rest: {}; after the leave against rest: {}",
                    hover.0, left.0
                ),
            );
            self.rec.measured(
                "dnd.highlight.pixels",
                format!("hovered {}; after the leave {}", hover.1, left.1),
            );
            let dropped = self.typed(DND_STEM);
            let screen = self.ctl(&["text"])?;
            self.rec.behaviour(
                "dnd.typed",
                match dropped {
                    Ok(()) => typed_line(&screen),
                    Err(e) => format!("failed: {e}; the prompt line {:?}", typed_line(&screen)),
                },
            );
            Ok(())
        })();
        if let Err(e) = r {
            self.rec.behaviour("dnd", format!("failed: {e}"));
        }
        self.stop_unit(&source.name);
        self.clear_line();
    }

    fn source_heard_until(
        &self,
        report: &str,
        seen: usize,
        want: (&str, Option<&str>),
    ) -> Result<(String, usize), String> {
        let t0 = Instant::now();
        loop {
            let text = self.sh(&["cat".into(), report.into()]).text;
            let lines: Vec<&str> = text.lines().collect();
            if let Some(said) = source_heard(&lines, seen, want) {
                return Ok((said, lines.len()));
            }
            if t0.elapsed() > HANG {
                return Err(format!(
                    "the drag source never heard target {}{} (60 s): it heard {:?}",
                    want.0,
                    want.1.map(|a| format!(", action {a}")).unwrap_or_default(),
                    lines.get(seen..).unwrap_or_default().join(", ")
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn record_dnd_report(&mut self, report: &str) {
        let out = self.sh(&["cat".into(), report.into()]);
        for line in out.text.lines() {
            if let Some((key, value)) = line.split_once(' ') {
                self.rec.behaviour(&format!("dnd.{key}"), value);
            }
        }
    }

    fn xim(&mut self) {
        let Some(display) = self.x_display() else {
            return;
        };
        let ibus_config = match self.up().config() {
            Ok(config) => config.ibus(),
            Err(e) => {
                self.rec.behaviour("xim", format!("failed: {e}"));
                return;
            }
        };
        let keyfile = format!("{ibus_config}/glib-2.0/settings/keyfile");
        let config = format!("XDG_CONFIG_HOME={ibus_config}");
        let ibus = guest::Unit {
            name: format!("{}ibus", guest::UNIT_PREFIX),
            env: vec![
                ("DISPLAY".into(), display.clone()),
                ("GSETTINGS_BACKEND".into(), "keyfile".into()),
                ("XDG_CONFIG_HOME".into(), ibus_config.clone()),
            ],
            argv: [
                "ibus-daemon",
                "--xim",
                "--replace",
                "--panel=disable",
                "--emoji-extension=disable",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            socket: String::new(),
        };
        type Cjk = (Option<String>, String);
        let r = (|| -> Result<(String, Cjk), String> {
            let fresh = self.sh(&[
                "sh".into(),
                "-c".into(),
                "rm -rf -- \"$1\"".into(),
                "ibus-config".into(),
                ibus_config,
            ]);
            if !fresh.ok() {
                return Err(format!("{keyfile}: {}", fresh.tail(2)));
            }
            self.up().write_config(&keyfile, guest::IBUS_KEYFILE)?;
            if !self.sh(&guest::run_unit(&ibus, None)).ok() {
                return Err("ibus-daemon did not start".into());
            }
            let engine = self.x(
                &display,
                &[
                    "GSETTINGS_BACKEND=keyfile",
                    &config,
                    "timeout",
                    "60",
                    "sh",
                    "-c",
                    "until ibus engine \"$1\" 2>/dev/null; do sleep 0.2; done",
                    "ibus-engine",
                    XIM_ENGINE,
                ],
            );
            if !engine.ok() {
                return Err(format!("ibus never took the engine {XIM_ENGINE} (60 s)"));
            }
            let served = self.x(
                &display,
                &[
                    "timeout",
                    "60",
                    "sh",
                    "-c",
                    "until xprop -root XIM_SERVERS | grep -q @server=ibus; do sleep 0.2; done",
                ],
            );
            if !served.ok() {
                return Err("the root window never named ibus in XIM_SERVERS (60 s)".into());
            }
            self.gui.prepare()?;
            let invocation = self.gui.start(
                Some(self.screen.display),
                &[("XMODIFIERS".into(), "@im=ibus".into())],
            )?;
            if !self.sh(&guest::wait_socket(&self.gui.sock(), 60)).ok() {
                return Err(format!(
                    "aterm-gui under XIM never listened (60 s): {}",
                    crate::util::one_line(&self.gui.journal(&invocation), 300)
                ));
            }
            let r = self.ctl(&["ready", "60000"])?;
            if !r.starts_with("OK ready") {
                return Err(format!("`ready` answered {r}"));
            }
            let win = self
                .x_window(&display)
                .ok_or("xdotool found no window for the XIM aterm-gui")?;
            self.focus_x(&display, &win)?;
            let composed = self.composed(Typist::X {
                display: &display,
                server: true,
            });
            Ok((composed, self.cjk(&display, &config)))
        })();
        self.gui.stop();
        self.stop_unit(&ibus.name);
        match r {
            Ok((composed, (preedit, committed))) => {
                self.rec
                    .behaviour("xim", format!("ibus XIM, engine {XIM_ENGINE}: {composed}"));
                self.rec.behaviour(
                    "xim.cjk",
                    format!("ibus XIM, engine {XIM_CJK_ENGINE}: {committed}"),
                );
                if let Some(preedit) = preedit {
                    self.rec.behaviour("xim.cjk.preedit", preedit);
                }
            }
            Err(e) => {
                self.rec.behaviour("xim", format!("failed: {e}"));
                self.rec.behaviour("xim.cjk", format!("failed: {e}"));
            }
        }
    }

    fn cjk(&self, display: &str, config: &str) -> (Option<String>, String) {
        let mut preedit = None;
        let before = self.ime_events();
        let r = (|| -> Result<String, String> {
            let engine = self.x(
                display,
                &[
                    "GSETTINGS_BACKEND=keyfile",
                    config,
                    "timeout",
                    "60",
                    "sh",
                    "-c",
                    "until ibus engine \"$1\" 2>/dev/null; do sleep 0.2; done",
                    "ibus-engine",
                    XIM_CJK_ENGINE,
                ],
            );
            if !engine.ok() {
                return Err(format!(
                    "ibus never took the engine {XIM_CJK_ENGINE} (60 s)"
                ));
            }
            self.clear_line();
            self.ctl(&["send", COMPOSE_BEFORE])?;
            self.typed(COMPOSE_BEFORE)?;
            let mut last = self.ime()?;
            for key in XIM_CJK_KEYS {
                let typed = self.x(display, &["xdotool", "key", key]);
                if !typed.ok() {
                    return Err(format!("the key {key}: {}", typed.tail(2)));
                }
                let t0 = Instant::now();
                loop {
                    let now = self.ime()?;
                    if ime_composition(&now) != ime_composition(&last) {
                        last = now;
                        break;
                    }
                    if t0.elapsed() > HANG {
                        let screen = self.ctl(&["text"]).unwrap_or_default();
                        return Err(format!(
                            "`{key}` never moved the composition (60 s): {last}; typed line {:?}",
                            typed_line(&screen)
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            let screen = self.ctl(&["text"])?;
            preedit = Some(format!("{last}; typed line {:?}", typed_line(&screen)));
            let committed = format!("{COMPOSE_BEFORE}{XIM_CJK_TEXT}");
            let off = self.x(display, &["xset", "r", "off"]);
            if !off.ok() {
                return Err(format!("autorepeat stayed on: {}", off.tail(2)));
            }
            let down = self.x(display, &["xdotool", "keydown", "space"]);
            if down.ok() {
                let _ = self.typed(&committed);
            }
            let up = self.x(display, &["xdotool", "keyup", "space"]);
            let _ = self.x(display, &["xset", "r", "on"]);
            if !down.ok() || !up.ok() {
                return Err(format!("space: {} {}", down.tail(2), up.tail(2)));
            }
            self.ctl(&["send", COMPOSE_AFTER])?;
            self.ctl(&["key", "enter"])?;
            let want = format!("X-{XIM_CJK_TEXT}{COMPOSE_AFTER}");
            if self.appeared(&want).is_ok() {
                Ok(format!("{want} printed"))
            } else {
                let screen = self.ctl(&["text"]).unwrap_or_default();
                let shown = screen
                    .lines()
                    .rev()
                    .find(|l| l.starts_with("X-"))
                    .unwrap_or("nothing")
                    .to_string();
                Ok(format!("{want} never printed; the shell printed {shown:?}"))
            }
        })();
        self.clear_line();
        let ime = match (before, self.ime_events()) {
            (Some(b), Some(a)) if a > b => "IME events",
            (Some(_), Some(_)) => "no IME event",
            _ => "IME events unread",
        };
        let committed = match r {
            Ok(what) => format!("{what}; {ime}"),
            Err(e) => format!("failed: {e}"),
        };
        (preedit, committed)
    }

    fn ime(&self) -> Result<String, String> {
        let chrome = self.ctl(&["chrome"])?;
        ime_line(&chrome).map(str::to_string).ok_or_else(|| {
            format!(
                "`chrome` has no ime line: {}",
                crate::util::one_line(&chrome, 200)
            )
        })
    }

    fn text_input_v3(&mut self) {
        if self.screen.display != Display::Mutter {
            return;
        }
        let log = format!("{}/wayland-debug.txt", self.gui.dir);
        let r = (|| -> Result<String, String> {
            self.gui.prepare()?;
            let invocation = self.gui.start_logged(
                Some(Display::Mutter),
                &[("WAYLAND_DEBUG".into(), "client".into())],
                &log,
            )?;
            if !self.sh(&guest::wait_socket(&self.gui.sock(), 60)).ok() {
                return Err(format!(
                    "aterm-gui never listened (60 s): {}",
                    crate::util::one_line(&self.gui.journal(&invocation), 300)
                ));
            }
            let r = self.ctl(&["ready", "60000"])?;
            if !r.starts_with("OK ready") {
                return Err(format!("`ready` answered {r}"));
            }
            let seat = self.seat_unit(&[])?;
            let done = seat.done(self);
            let _ = self.sh(&[
                "timeout".into(),
                "60".into(),
                "sh".into(),
                "-c".into(),
                "until grep -q 'zwp_text_input_v3.*set_cursor_rectangle' \"$1\"; do sleep 0.1; done"
                    .into(),
                "wait-cursor-rectangle".into(),
                log.clone(),
            ]);
            seat.stop(self);
            done?;
            self.gui.stop();
            let wire = self.sh(&["cat".into(), log.clone()]);
            let _ = std::fs::write(
                self.results.join(format!(
                    "{}-{}-wayland-debug.txt",
                    self.arch.name(),
                    self.screen.name()
                )),
                &wire.text,
            );
            Ok(text_input_wire(&wire.text))
        })();
        self.gui.stop();
        self.rec.behaviour(
            "text-input-v3.client",
            r.unwrap_or_else(|e| format!("failed: {e}")),
        );
    }

    fn wire_until(&self, log: &str, pattern: &str) -> Result<(), String> {
        let seen = self.sh(&[
            "timeout".into(),
            "60".into(),
            "sh".into(),
            "-c".into(),
            "until grep -qE -e \"$2\" \"$1\"; do sleep 0.1; done".into(),
            "wire-until".into(),
            log.into(),
            pattern.into(),
        ]);
        if seen.ok() {
            Ok(())
        } else {
            Err(format!("the wire never showed /{pattern}/ (60 s)"))
        }
    }

    fn logged_gui(&self, log: &str, extra: &[(String, String)]) -> Result<(), String> {
        self.gui.prepare()?;
        let mut env = vec![("WAYLAND_DEBUG".to_string(), "client".to_string())];
        env.extend(extra.iter().cloned());
        let invocation = self
            .gui
            .start_logged(Some(self.screen.display), &env, log)?;
        if !self.sh(&guest::wait_socket(&self.gui.sock(), 60)).ok() {
            return Err(format!(
                "aterm-gui never listened (60 s): {}",
                crate::util::one_line(&self.gui.journal(&invocation), 300)
            ));
        }
        let r = self.ctl(&["ready", "60000"])?;
        if r.starts_with("OK ready") {
            Ok(())
        } else {
            Err(format!("`ready` answered {r}"))
        }
    }

    fn keep_wire(&self, log: &str, what: &str) -> String {
        let wire = self.sh(&["cat".into(), log.into()]).text;
        let _ = std::fs::write(
            self.results.join(format!(
                "{}-{}-{what}.txt",
                self.arch.name(),
                self.screen.name()
            )),
            &wire,
        );
        wire
    }

    fn touch(&mut self) {
        if self.screen.display != Display::Mutter {
            return;
        }
        let log = format!("{}/touch-wayland-debug.txt", self.gui.dir);
        let marks = format!("{}/touch", self.gui.dir);
        let mark = |name: &str| format!("{marks}/{name}");
        let r = (|| -> Result<(String, String, String), String> {
            self.logged_gui(&log, &[])?;
            let fresh = self.sh(&[
                "sh".into(),
                "-c".into(),
                "rm -rf \"$1\" && install -d \"$1\"".into(),
                "touch-marks".into(),
                marks.clone(),
            ]);
            if !fresh.ok() {
                return Err(format!("{marks}: {}", fresh.tail(2)));
            }
            let before = self.settled_text()?;
            let steps = [
                "move:640,400".to_string(),
                format!("touch:{}", mark("centred")),
                format!("wait:{}", mark("aimed")),
                format!("finger-down:@{}", mark("at")),
                format!("finger-move:@{}", mark("to")),
                "finger-up:".to_string(),
                format!("touch:{}", mark("lifted")),
                format!("wait:{}", mark("heard")),
            ];
            let words: Vec<&str> = steps.iter().map(String::as_str).collect();
            let seat = self.seat_unit(&words)?;
            let mut pointer = String::new();
            let touched = (|| -> Result<(), String> {
                self.seat_reached(&seat, &marks, "centred")?;
                let (px, py) = self
                    .settled_px()
                    .map_err(|e| format!("the pointer on the window: {e}"))?;
                let scale = self.dims_scale();
                pointer = format!("{:.2},{:.2}", f64::from(px) / scale, f64::from(py) / scale);
                let (x, y) = (
                    (640.0 * scale).round() as i32,
                    (400.0 * scale).round() as i32,
                );
                self.put_file(&mark("at"), &format!("{x} {y}"))?;
                self.put_file(&mark("to"), &format!("{} {}", x + 15, y + 15))?;
                self.wire_until(&log, r"get_touch\(")?;
                let _ = self.sh(&["touch".into(), mark("aimed")]);
                self.seat_reached(&seat, &marks, "lifted")?;
                self.wire_until(&log, r"wl_touch[@#][0-9]+\.up\(")?;
                let _ = self.sh(&["touch".into(), mark("heard")]);
                Ok(())
            })();
            let done = if touched.is_ok() {
                seat.done(self)
            } else {
                Ok(())
            };
            seat.stop(self);
            touched?;
            done?;
            let after = self.settled_text()?;
            let screen = if after == before {
                "the screen as it was".to_string()
            } else {
                format!(
                    "the screen changed: {:?}",
                    after
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                )
            };
            self.gui.stop();
            let wire = self.keep_wire(&log, "touch-wayland-debug");
            let landed = format!(
                "touch {}; the pointer at {pointer}",
                touch_down_at(&wire).unwrap_or_else(|| "down not logged".into())
            );
            Ok((touch_wire(&wire), screen, landed))
        })();
        self.gui.stop();
        match r {
            Ok((wire, screen, landed)) => {
                self.rec.behaviour("touch.wire", wire);
                self.rec.behaviour("touch.screen", screen);
                self.rec.measured("touch.at", landed);
            }
            Err(e) => self.rec.behaviour("touch", format!("failed: {e}")),
        }
    }

    fn text_input_v3_ime(&mut self) {
        if self.screen.display != Display::Mutter {
            return;
        }
        let config = match self.up().config() {
            Ok(config) => config,
            Err(e) => {
                self.rec
                    .behaviour("text-input-v3.ime", format!("failed: {e}"));
                return;
            }
        };
        let Some(mut shell) = self.screen.gnome_shell(&self.up().runtime, config) else {
            return;
        };
        let shell_log = format!("{}/{}-wire.txt", self.up().runtime, shell.name);
        shell.env.push(("WAYLAND_DEBUG".into(), "server".into()));
        shell
            .env
            .push(("ATERM_LANE_STDERR".into(), shell_log.clone()));
        let mut argv = vec![
            "sh".to_string(),
            "-c".into(),
            "exec \"$0\" \"$@\" 2>\"$ATERM_LANE_STDERR\"".into(),
        ];
        argv.append(&mut shell.argv);
        shell.argv = argv;
        let log = format!("{}/ime-wayland-debug.txt", self.gui.dir);
        let marks = format!("{}/ime", self.gui.dir);
        let mark = |name: &str| format!("{marks}/{name}");
        for (pkg, key) in [("gnome-shell", "ime.gnome-shell"), ("ibus", "ime.ibus")] {
            let v = self.sh(&[
                "dpkg-query".into(),
                "-W".into(),
                "-f".into(),
                "${Version}".into(),
                pkg.into(),
            ]);
            if v.ok() {
                self.rec.stack(key, v.text.trim());
            }
        }
        let shell_said = || {
            let said = self.sh(&[
                "sh".into(),
                "-c".into(),
                "grep -v '^\\[' \"$1\" | tail -n 3".into(),
                "shell-said".into(),
                shell_log.clone(),
            ]);
            crate::util::one_line(&said.text, 300)
        };
        let before = std::cell::Cell::new(None);
        let r = (|| -> Result<String, String> {
            self.stop_unit(&format!("{}mutter", guest::UNIT_PREFIX));
            if !self.sh(&guest::run_unit(&shell, None)).ok() {
                return Err("gnome-shell did not start".into());
            }
            if !self.sh(&guest::wait_socket(&shell.socket, 60)).ok() {
                return Err(format!(
                    "gnome-shell never listened (60 s): {}",
                    shell_said()
                ));
            }
            let engine = self.sh(&[
                "timeout".into(),
                "60".into(),
                "sh".into(),
                "-c".into(),
                "cg=/sys/fs/cgroup$(systemctl --user show -p ControlGroup --value \"$1\") && \
                 until for p in $(cat \"$cg/cgroup.procs\"); do cat \"/proc/$p/comm\" 2>/dev/null; done \
                 | grep -q '^ibus-engine-sim'; do sleep 0.1; done"
                    .into(),
                "ibus-engine".into(),
                format!("{}.service", shell.name),
            ]);
            if !engine.ok() {
                return Err(format!(
                    "GNOME Shell's IBus never ran an engine (60 s): {}",
                    shell_said()
                ));
            }
            self.logged_gui(&log, &[("WAYLAND_DISPLAY".into(), shell.socket.clone())])?;
            before.set(self.ime_events());
            let fresh = self.sh(&[
                "sh".into(),
                "-c".into(),
                "rm -rf \"$1\" && install -d \"$1\"".into(),
                "ime-marks".into(),
                marks.clone(),
            ]);
            if !fresh.ok() {
                return Err(format!("{marks}: {}", fresh.tail(2)));
            }
            let steps = [
                "move:-4000,-4000".to_string(),
                format!("touch:{}", mark("parked")),
                format!("wait:{}", mark("bound")),
                "move:640,400".to_string(),
                "move:1,0".to_string(),
                "move:-1,0".to_string(),
                "press:left".to_string(),
                "release:left".to_string(),
                format!("touch:{}", mark("clicked")),
                format!("wait:{}", mark("focused")),
                "tap:x".to_string(),
                format!("touch:{}", mark("plain")),
                format!("wait:{}", mark("plain-seen")),
                "tap:BackSpace".to_string(),
                "tap:dead_acute".to_string(),
                "tap:e".to_string(),
                format!("touch:{}", mark("keyed")),
            ];
            let words: Vec<&str> = steps.iter().map(String::as_str).collect();
            let seat = self.seat_unit_of(&shell.name, "-", &words)?;
            let go = |name: &str| {
                let _ = self.sh(&["touch".into(), mark(name)]);
            };
            let stopped = std::cell::Cell::new(false);
            let keyed = (|| -> Result<(), String> {
                self.seat_reached(&seat, &marks, "parked")?;
                self.wire_until(&log, r"get_pointer\(")?;
                go("bound");
                self.seat_reached(&seat, &marks, "clicked")?;
                self.wire_until(&log, r"-> zwp_text_input_v3[@#][0-9]+\.enable\(")
                    .map_err(|e| format!("the click gave the window no focus: {e}"))?;
                self.ctl(&["send", COMPOSE_BEFORE])?;
                self.typed(COMPOSE_BEFORE)?;
                go("focused");
                self.seat_reached(&seat, &marks, "plain")?;
                self.typed(COMPOSE_LIVE).map_err(|_| {
                    format!("a plain key never reached the screen ({COMPOSE_LIVE})")
                })?;
                if !self.gui.signal("SIGSTOP") {
                    return Err("aterm-gui would not stop".into());
                }
                stopped.set(true);
                go("plain-seen");
                self.seat_reached(&seat, &marks, "keyed")?;
                self.wire_until(
                    &shell_log,
                    "-> zwp_text_input_v3[@#][0-9]+\\.commit_string\\(\"\u{e9}\"\\)",
                )
                .map_err(|e| format!("GNOME Shell never posted the commit: {e}"))?;
                self.wire_until(
                    &shell_log,
                    r"-> wl_keyboard[@#][0-9]+\.key\([0-9]+, [0-9]+, 18, 0\)",
                )
                .map_err(|e| format!("GNOME Shell never posted the e's release: {e}"))?;
                Ok(())
            })();
            let continued = !stopped.get() || self.gui.signal("SIGCONT");
            let done = if keyed.is_ok() {
                seat.done(self)
            } else {
                Ok(())
            };
            seat.stop(self);
            keyed?;
            if !continued {
                return Err("aterm-gui would not continue".into());
            }
            done?;
            let _ = self.typed(&format!("{COMPOSE_BEFORE}\u{e9}"));
            self.ctl(&["send", COMPOSE_AFTER])?;
            self.ctl(&["key", "enter"])?;
            if self.appeared(IME_COMPOSED).is_ok() {
                Ok(format!("{IME_COMPOSED} printed"))
            } else {
                let screen = self.ctl(&["text"]).unwrap_or_default();
                let shown = screen
                    .lines()
                    .rev()
                    .find(|l| l.starts_with("X-"))
                    .unwrap_or("nothing")
                    .to_string();
                Ok(format!(
                    "{IME_COMPOSED} never printed; the shell printed {shown:?}"
                ))
            }
        })();
        let ime = match (before.get(), self.ime_events()) {
            (Some(b), Some(a)) if a > b => "IME events",
            (Some(_), Some(_)) => "no IME event",
            _ => "IME events unread",
        };
        self.gui.stop();
        self.stop_unit(&shell.name);
        let posted = self.sh(&[
            "sh".into(),
            "-c".into(),
            "grep -E 'zwp_text_input_v3|wl_keyboard' \"$1\"; rm -f \"$1\"".into(),
            "shell-wire".into(),
            shell_log.clone(),
        ]);
        let _ = std::fs::write(
            self.results.join(format!(
                "{}-{}-ime-shell-wayland-debug.txt",
                self.arch.name(),
                self.screen.name()
            )),
            &posted.text,
        );
        match r {
            Ok(what) => {
                self.rec
                    .behaviour("text-input-v3.ime", format!("{what}; {ime}"));
                let wire = self.keep_wire(&log, "ime-wayland-debug");
                self.rec
                    .behaviour("text-input-v3.ime.wire", text_input_wire(&wire));
            }
            Err(e) => {
                let _ = self.keep_wire(&log, "ime-wayland-debug");
                self.rec
                    .behaviour("text-input-v3.ime", format!("failed: {e}"));
            }
        }
    }

    fn caption(&self) -> Result<Caption, String> {
        let chrome = self.ctl(&["chrome"])?;
        parse_caption(&chrome).ok_or_else(|| {
            format!(
                "`chrome` has no strip-caption line: {}",
                crate::util::one_line(&chrome, 200)
            )
        })
    }

    fn caption_until(&self, done: &dyn Fn(&Caption) -> bool) -> Result<Caption, String> {
        let t0 = Instant::now();
        loop {
            let c = self.caption()?;
            if done(&c) {
                return Ok(c);
            }
            if t0.elapsed() > HANG {
                return Err(format!(
                    "the frame never got there (60 s): buttons={} maximized={}",
                    c.names(),
                    c.maximized
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn click_button(&self, caption: &Caption, name: &str) -> Result<(), String> {
        let col = caption
            .middle(name)
            .ok_or_else(|| format!("no {name} button (buttons={})", caption.names()))?;
        self.click_strip(col)
    }

    fn click_strip(&self, col: u16) -> Result<(), String> {
        let col = col.to_string();
        let d = self.ctl(&["dims"])?;
        let num = |k: &str| proof::field(&d, k).and_then(|v| v.parse::<u32>().ok());
        let strip_bottom = num("pad_top").unwrap_or(0)
            + num("head").unwrap_or(0)
            + num("tab_rows").unwrap_or(1) * num("cell_h").unwrap_or(0);
        for up in 1..=4 {
            let up = format!("-{up}");
            let at = self.ctl(&["pointer", "move", &up, &col])?;
            let y = proof::field(&at, "px")
                .and_then(|p| p.split_once(','))
                .and_then(|(_, y)| y.parse::<u32>().ok());
            if y.is_some_and(|y| y < strip_bottom) {
                self.ctl(&["pointer", "click"])?;
                return Ok(());
            }
        }
        Err(format!("no chrome row above the grid is the strip ({d})"))
    }

    fn header(&mut self) {
        let caption = match self.caption() {
            Ok(c) => c,
            Err(e) => {
                self.rec.behaviour("header.frame", format!("failed: {e}"));
                return;
            }
        };
        self.rec.behaviour(
            "header.frame",
            if caption.client {
                format!(
                    "client buttons={} edges={} menu={}",
                    caption.names(),
                    caption.edges,
                    caption.menu
                )
            } else {
                "system".to_string()
            },
        );
        self.rec.measured(
            "header.cols",
            caption
                .buttons
                .iter()
                .map(|(n, s, e)| format!("{n}:{s}..{e}"))
                .collect::<Vec<_>>()
                .join(","),
        );
        self.header_pixels(&caption);
        if caption.client {
            self.header_maximize(&caption);
        }
    }

    fn header_pixels(&mut self, caption: &Caption) {
        let r = (|| -> Result<(String, String), String> {
            let d = self.ctl(&["dims"])?;
            let num = |k: &str| proof::field(&d, k).and_then(|v| v.parse::<u32>().ok());
            let scale = proof::field(&d, "scale")
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(1.0);
            let (cell_w, cell_h) = (num("cell_w").unwrap_or(0), num("cell_h").unwrap_or(0));
            let cols = num("window_cols").unwrap_or(0);
            let pad = num("pad").unwrap_or(0);
            let band = num("pad_top").unwrap_or(0)
                + num("head").unwrap_or(0)
                + num("tab_rows").unwrap_or(1) * cell_h;
            let slots: Vec<(u16, u16)> = if caption.buttons.is_empty() {
                let w = caption_slot_cols(cell_w, scale);
                let cols = u16::try_from(cols).unwrap_or(u16::MAX);
                (1..=3u16)
                    .rev()
                    .filter_map(|k| {
                        let s = cols.checked_sub(k * w)?;
                        Some((s, s + w))
                    })
                    .collect()
            } else {
                caption.buttons.iter().map(|&(_, s, e)| (s, e)).collect()
            };
            let f = self.frame("header")?;
            let x = |c: u16| (pad + u32::from(c) * cell_w) as usize;
            let (mut classes, mut counts) = (Vec::new(), Vec::new());
            for (s, e) in slots {
                let (class, n) = slot_class(&f, x(s), x(e), 0, band as usize);
                classes.push(class);
                counts.push(n.to_string());
            }
            Ok((classes.join(","), counts.join(",")))
        })();
        match r {
            Ok((classes, counts)) => {
                self.rec.behaviour("header.pixels", classes);
                self.rec.measured("header.pixels.differ", counts);
            }
            Err(e) => self.rec.behaviour("header.pixels", format!("failed: {e}")),
        }
    }

    fn header_maximize(&mut self, caption: &Caption) {
        let maximized = (|| -> Result<(Caption, String), String> {
            self.click_button(caption, "maximize")?;
            let c = self.caption_until(&|c| c.maximized)?;
            Ok((c, self.settled_dims()?))
        })();
        let c = match maximized {
            Ok((c, d)) => {
                self.rec.behaviour(
                    "header.maximize",
                    format!("buttons={} edges={}", c.names(), c.edges),
                );
                self.rec.behaviour("header.maximize.dims", dims_summary(&d));
                c
            }
            Err(e) => {
                self.rec
                    .behaviour("header.maximize", format!("failed: {e}"));
                return;
            }
        };
        let restored = (|| -> Result<(Caption, String), String> {
            self.click_button(&c, "restore")?;
            let c = self.caption_until(&|c| !c.maximized)?;
            Ok((c, self.dims_until(&|r| proof::dims_are(r, 24, 80))?))
        })();
        match restored {
            Ok((c, d)) => {
                self.rec
                    .behaviour("header.restore", format!("buttons={}", c.names()));
                self.rec.behaviour("header.restore.dims", dims_summary(&d));
            }
            Err(e) => self.rec.behaviour("header.restore", format!("failed: {e}")),
        }
    }

    fn put_file(&self, path: &str, text: &str) -> Result<(), String> {
        let out = self.sh(&[
            "sh".into(),
            "-c".into(),
            "printf '%s\\n' \"$1\" > \"$2\"".into(),
            "put".into(),
            text.into(),
            path.into(),
        ]);
        if out.ok() {
            Ok(())
        } else {
            Err(format!("{path}: {}", out.tail(2)))
        }
    }

    fn pointer_px(&self) -> Result<(i32, i32), String> {
        let at = self.ctl(&["pointer", "status"])?;
        proof::field(&at, "px")
            .and_then(|p| p.split_once(','))
            .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))
            .ok_or_else(|| format!("the window holds no pointer ({at})"))
    }

    fn settled_px(&self) -> Result<(i32, i32), String> {
        let t0 = Instant::now();
        let mut last = self.pointer_px().ok();
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let now = self.pointer_px().ok();
            if now.is_some() && now == last {
                return now.ok_or_else(String::new);
            }
            if t0.elapsed() > HANG {
                return Err(format!(
                    "the pointer never settled on the window (60 s): {}",
                    self.pointer_px().err().unwrap_or_default()
                ));
            }
            last = now;
        }
    }

    fn px_at(&self, want: (i32, i32), slack: i32) -> Result<(i32, i32), String> {
        let t0 = Instant::now();
        loop {
            let got = self.pointer_px();
            if let Ok(p) = got
                && (p.0 - want.0).abs() <= slack
                && (p.1 - want.1).abs() <= slack
            {
                return Ok(p);
            }
            if t0.elapsed() > HANG {
                return Err(format!(
                    "the pointer never reached {},{} (60 s): {}",
                    want.0,
                    want.1,
                    match got {
                        Ok((x, y)) => format!("px={x},{y}"),
                        Err(e) => e,
                    }
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn header_gestures(&mut self) {
        if self.screen.display.seat().is_none() {
            return;
        }
        let r = self.header_gestures_run();
        self.gui.stop();
        match r {
            Ok(lines) => {
                for (behaviour, key, value) in lines {
                    if behaviour {
                        self.rec.behaviour(&key, value);
                    } else {
                        self.rec.measured(&key, value);
                    }
                }
            }
            Err(e) => self
                .rec
                .behaviour("header.gestures", format!("failed: {e}")),
        }
    }

    fn seat_reached(&self, seat: &SeatUnit, marks: &str, name: &str) -> Result<(), String> {
        if self
            .sh(&guest::wait_file(&format!("{marks}/{name}"), 60))
            .ok()
        {
            Ok(())
        } else {
            Err(format!(
                "the seat never reached {name} (60 s): {}",
                self.unit_journal(&seat.name)
            ))
        }
    }

    /// The `wl_pointer` (`wl_pointer@N`) aterm-gui's wire log `log` names.
    fn wire_pointer(&self, log: &str) -> Result<String, String> {
        let wire = self.sh(&["cat".into(), log.into()]);
        let tag = "wl_pointer@";
        let at = wire
            .text
            .find(tag)
            .ok_or_else(|| format!("{log} names no wl_pointer"))?;
        let id: String = wire.text[at + tag.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        Ok(format!("{tag}{id}"))
    }

    /// How many times the wire log `log` shows `pointer` leaving a surface.
    fn wire_leaves(&self, log: &str, pointer: &str) -> usize {
        let leave = format!("{pointer}.leave(");
        let wire = self.sh(&["cat".into(), log.into()]);
        wire.text.lines().filter(|l| l.contains(&leave)).count()
    }

    /// Wait (a hang detector) until the wire log `log` shows the `request`
    /// (`move` or `resize`) and then one more leave of `pointer` than
    /// `leaves`: Mutter and weston take the pointer as a grab starts. The seat
    /// drags only after it, or a starved aterm-gui reads press, drag and
    /// release in one batch and asks with the release's serial, which the
    /// compositor refuses.
    fn await_grab(&self, log: &str, request: &str, pointer: &str, leaves: usize) {
        let asked = format!(".{request}(wl_seat@");
        if self.sh(&guest::wait_lines(log, &asked, 1, 60)).ok() {
            let left = format!("{pointer}.leave(");
            let _ = self.sh(&guest::wait_lines(log, &left, leaves + 1, 60));
        }
    }

    fn header_gestures_run(&self) -> Result<Vec<(bool, String, String)>, String> {
        let log = format!("{}/wayland-debug-frame.txt", self.gui.dir);
        let marks = format!("{}/frame", self.gui.dir);
        let mark = |name: &str| format!("{marks}/{name}");
        self.gui.prepare()?;
        let fresh = self.sh(&[
            "sh".into(),
            "-c".into(),
            "rm -rf \"$1\" && install -d \"$1\"".into(),
            "marks".into(),
            marks.clone(),
        ]);
        if !fresh.ok() {
            return Err(format!("{marks}: {}", fresh.tail(2)));
        }
        let invocation = self.gui.start_logged(
            Some(self.screen.display),
            &[("WAYLAND_DEBUG".into(), "client".into())],
            &log,
        )?;
        if !self.sh(&guest::wait_socket(&self.gui.sock(), 60)).ok() {
            return Err(format!(
                "aterm-gui never listened (60 s): {}",
                crate::util::one_line(&self.gui.journal(&invocation), 300)
            ));
        }
        let r = self.ctl(&["ready", "60000"])?;
        if !r.starts_with("OK ready") {
            return Err(format!("`ready` answered {r}"));
        }
        let d = self.settled_dims()?;
        let num = |k: &str| proof::field(&d, k).and_then(|v| v.parse::<i32>().ok());
        let scale = proof::field(&d, "scale")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0);
        let (w, h) = (num("frame_w").unwrap_or(0), num("frame_h").unwrap_or(0));
        let band_y =
            num("pad_top").unwrap_or(0) + num("head").unwrap_or(0) + num("cell_h").unwrap_or(0) / 2;
        let caption = self.caption()?;
        let (cw, pad) = (num("cell_w").unwrap_or(0), num("pad").unwrap_or(0));
        let cols = u16::try_from(num("window_cols").unwrap_or(0)).unwrap_or(0);
        let first = i32::from(caption.right_end(cols));
        let band_x = pad + (first - 4).max(0) * cw;
        let logical = |px: i32| (f64::from(px) / scale).round() as i32;
        let device = |lp: i32| (f64::from(lp) * scale).round() as i32;
        let slack = scale.ceil() as i32;
        let steps = [
            "move:640,400".to_string(),
            format!("touch:{}", mark("centred")),
            format!("wait:{}", mark("aim-band")),
            format!("move:@{}", mark("to-band")),
            format!("touch:{}", mark("at-band")),
            format!("wait:{}", mark("drag-band")),
            "press:left".to_string(),
            "move:6,0".to_string(),
            format!("wait:{}", mark("move-grabbed")),
            format!("move:@{}", mark("to-move")),
            "sleep:300".to_string(),
            "release:left".to_string(),
            format!("touch:{}", mark("moved")),
            format!("wait:{}", mark("aim-edge")),
            format!("move:@{}", mark("to-edge")),
            format!("touch:{}", mark("at-edge")),
            format!("wait:{}", mark("drag-edge")),
            "press:left".to_string(),
            format!("wait:{}", mark("resize-grabbed")),
            "move:60,0".to_string(),
            "sleep:300".to_string(),
            "release:left".to_string(),
            format!("touch:{}", mark("resized")),
            format!("wait:{}", mark("aim-menu")),
            format!("move:@{}", mark("to-menu")),
            format!("touch:{}", mark("at-menu")),
            format!("wait:{}", mark("press-menu")),
            "press:right".to_string(),
            "release:right".to_string(),
            "sleep:300".to_string(),
            "tap:Escape".to_string(),
        ];
        let words: Vec<&str> = steps.iter().map(String::as_str).collect();
        let seat = self.seat_unit(&words)?;
        let mut lines: Vec<(bool, String, String)> = Vec::new();
        let go = |name: &str| {
            let _ = self.sh(&["touch".into(), mark(name)]);
        };
        let aim = |name: &str, from: (i32, i32), to: (i32, i32)| -> Result<(i32, i32), String> {
            let (dx, dy) = (logical(to.0 - from.0), logical(to.1 - from.1));
            self.put_file(&mark(name), &format!("{dx} {dy}"))?;
            Ok((from.0 + device(dx), from.1 + device(dy)))
        };
        let gestures = (|| -> Result<(), String> {
            self.seat_reached(&seat, &marks, "centred")?;
            let (x, y) = self.settled_px().map_err(|e| format!("centred: {e}"))?;
            lines.push((
                false,
                "header.gesture.centred".into(),
                format!("px={x},{y}"),
            ));
            let band = aim("to-band", (x, y), (band_x, band_y))?;
            go("aim-band");
            self.seat_reached(&seat, &marks, "at-band")?;
            let (bx, by) = self
                .px_at(band, slack)
                .map_err(|e| format!("on the band: {e}"))?;
            let left = guest::SCREEN.0 / 2 - logical(x);
            let room = guest::SCREEN.0 - (left + 46 + logical(w));
            let dx = if room > 60 + 2 * slack { 40 } else { -40 };
            self.put_file(&mark("to-move"), &format!("{dx} 30"))?;
            let pointer = self.wire_pointer(&log)?;
            let leaves = self.wire_leaves(&log, &pointer);
            go("drag-band");
            self.await_grab(&log, "move", &pointer, leaves);
            go("move-grabbed");
            self.seat_reached(&seat, &marks, "moved")?;
            let (mx, my) = self
                .settled_px()
                .map_err(|e| format!("after the move: {e}"))?;
            let near = device(8);
            let followed = (mx - bx).abs() <= near && (my - by).abs() <= near;
            lines.push((
                true,
                "header.move".into(),
                if followed { "followed" } else { "stayed" }.into(),
            ));
            lines.push((
                false,
                "header.gesture.moved".into(),
                format!("px={mx},{my}"),
            ));
            let edge = aim("to-edge", (mx, my), (w - 2, h / 2))?;
            go("aim-edge");
            self.seat_reached(&seat, &marks, "at-edge")?;
            self.px_at(edge, slack)
                .map_err(|e| format!("on the edge: {e}"))?;
            let leaves = self.wire_leaves(&log, &pointer);
            go("drag-edge");
            self.await_grab(&log, "resize", &pointer, leaves);
            go("resize-grabbed");
            self.seat_reached(&seat, &marks, "resized")?;
            let d = self.settled_dims()?;
            let wider = proof::field(&d, "frame_w")
                .and_then(|v| v.parse::<i32>().ok())
                .is_some_and(|fw| fw > w);
            lines.push((
                true,
                "header.resize".into(),
                if wider { "wider" } else { "unchanged" }.into(),
            ));
            lines.push((false, "header.resize.dims".into(), dims_summary(&d)));
            let dragged = (edge.0 + device(60), edge.1);
            let menu = aim("to-menu", dragged, (band_x, band_y))?;
            go("aim-menu");
            self.seat_reached(&seat, &marks, "at-menu")?;
            self.px_at(menu, slack)
                .map_err(|e| format!("back on the band: {e}"))?;
            go("press-menu");
            Ok(())
        })();
        let done = if gestures.is_ok() {
            seat.done(self)
        } else {
            Ok(())
        };
        seat.stop(self);
        self.gui.stop();
        let wire = self.sh(&["cat".into(), log]);
        let _ = std::fs::write(
            self.results.join(format!(
                "{}-{}-wayland-debug-frame.txt",
                self.arch.name(),
                self.screen.name()
            )),
            &wire.text,
        );
        gestures?;
        done?;
        lines.push((true, "header.requests".into(), frame_requests(&wire.text)));
        Ok(lines)
    }

    fn cpu_seat(&mut self) {
        if self.screen.display.seat() != Some(Seat::Xtest) {
            return;
        }
        self.gui.cpu = true;
        let r = self.cpu_seat_run();
        self.gui.stop();
        self.gui.cpu = false;
        match r {
            Ok(lines) => {
                for (behaviour, key, value) in lines {
                    if behaviour {
                        self.rec.behaviour(&key, value);
                    } else {
                        self.rec.measured(&key, value);
                    }
                }
            }
            Err(e) => self.rec.behaviour("cpu.seat", format!("failed: {e}")),
        }
    }

    fn cpu_seat_run(&self) -> Result<Vec<(bool, String, String)>, String> {
        self.gui.prepare()?;
        let invocation = self.gui.start(Some(self.screen.display), &[])?;
        let mut lines: Vec<(bool, String, String)> = Vec::new();
        let r = (|| -> Result<(), String> {
            if !self.sh(&guest::wait_socket(&self.gui.sock(), 60)).ok() {
                return Err("aterm-gui --cpu never listened (60 s)".into());
            }
            let r = self.ctl(&["ready", "60000"])?;
            if !r.starts_with("OK ready") {
                return Err(format!("`ready` answered {r}"));
            }
            let start = self.settled_dims()?;
            lines.push((true, "cpu.start.dims".into(), dims_summary(&start)));
            let mut typed = vec!["move:0,0".to_string()];
            typed.extend(seat_taps(CPU_TYPED));
            typed.push("tap:Return".into());
            self.seat_keys(&typed.iter().map(String::as_str).collect::<Vec<_>>())?;
            if !self.matched(&format!("^{}\\s*$", regex_literal(CPU_ECHO))) {
                return Err(format!(
                    "`{CPU_TYPED}` typed on the seat, and {CPU_ECHO} never appeared (60 s)"
                ));
            }
            lines.push((
                true,
                "cpu.echo".into(),
                format!("typed `{CPU_TYPED}` on the seat, {CPU_ECHO} appeared"),
            ));
            let grid = |r: &str| {
                let mut it = r.split_whitespace().skip(1);
                (it.next().map(str::to_string), it.next().map(str::to_string))
            };
            let was = grid(&start);
            let binding = ["chord:Super_L+Shift_L+m"];
            self.seat_keys(&binding)
                .map_err(|e| format!("Super+Shift+M: {e}"))?;
            self.dims_until(&|r| grid(r) != was)?;
            let d = self.settled_dims()?;
            lines.push((true, "cpu.maximized".into(), dims_summary(&d)));
            lines.extend(self.cpu_held("maximized"));
            let more = seat_taps(proof::MORE);
            self.seat_keys(&more.iter().map(String::as_str).collect::<Vec<_>>())?;
            self.typed(proof::MORE)?;
            lines.extend(self.cpu_held("typed"));
            self.seat_keys(&binding)
                .map_err(|e| format!("Super+Shift+M again: {e}"))?;
            self.dims_until(&|r| grid(r) == was)?;
            let d = self.settled_dims()?;
            lines.push((true, "cpu.restored".into(), dims_summary(&d)));
            lines.extend(self.cpu_held("restored"));
            Ok(())
        })();
        if let Err(why) = r {
            let journal = self.gui.journal(&invocation);
            let kept = self.gui.keep_crash_reports(&self.results);
            return Err(format!(
                "{why}; aterm-gui's journal: {}{kept}",
                crate::util::one_line(&journal, 400)
            ));
        }
        Ok(lines)
    }

    fn cpu_held(&self, phase: &str) -> Vec<(bool, String, String)> {
        let render = self
            .ctl(&["status"])
            .ok()
            .and_then(|s| proof::field(&s, "render").map(str::to_string));
        let backend = self
            .ctl(&["metrics"])
            .ok()
            .and_then(|m| proof::field(&m, "backend").map(str::to_string));
        let drawn = format!(
            "render={} backend={}",
            render.as_deref().unwrap_or("?"),
            backend.as_deref().unwrap_or("?")
        );
        let held =
            self.gui
                .held_to_server(self.screen.display, &self.results, &format!("-cpu-{phase}"));
        let (pixels, detail) = match held {
            Ok(detail) => (
                "weston's output shows the presented frame exactly".to_string(),
                detail,
            ),
            Err(e) => (format!("failed: {e}"), String::new()),
        };
        let mut lines = vec![
            (true, format!("cpu.{phase}.drawn"), drawn),
            (true, format!("cpu.{phase}.pixels"), pixels),
        ];
        if !detail.is_empty() {
            lines.push((false, format!("cpu.{phase}.pixels.detail"), detail));
        }
        lines
    }

    fn not_driven(&mut self) {
        match self.screen.display {
            Display::Mutter => {}
            Display::WestonX11 => {
                self.rec.not_driven(
                    "text-input-v3",
                    "v1-only",
                    "weston 13 offers zwp_text_input_manager_v1 only, which aterm-window does not bind",
                );
                self.rec
                    .not_driven("touch", "no-touchscreen", TOUCH_WESTON_X11);
            }
            Display::Weston => {
                self.rec.not_driven(
                    "text-input-v3",
                    "v1-only",
                    "weston 13 offers zwp_text_input_manager_v1 only, which aterm-window does not bind",
                );
                self.rec.not_driven(
                    "header.gestures",
                    "no-seat",
                    "the frame's move, resize and window menu take a press's serial from a seat, and weston's headless backend has none (header.maximize is driven: maximize takes no serial; the weston-x11 screen drives the gestures from its x11 backend's seat)",
                );
                self.rec.not_driven(
                    "dnd",
                    "no-seat",
                    "weston's headless backend advertises no wl_seat, and a drag starts from a seat's pointer (the weston-x11 screen drives it: weston's x11 backend, nested in Xvfb, has a seat that XTEST on the Xvfb drives)",
                );
                self.rec.not_driven("touch", "no-seat", TOUCH_WESTON);
            }
            Display::Xvfb | Display::Xwayland => {
                self.rec.not_driven(
                    "header.gestures",
                    "system-frame",
                    "an X11 window's frame is the window manager's (this lane runs none), so aterm draws no header and its band is not caption",
                );
                self.rec.not_driven("touch", "no-touchscreen", TOUCH_X11);
            }
        }
    }

    fn percentiles(&mut self, journal: &str) {
        let gpu = proof::renderer(journal).unwrap_or("?").to_string();
        self.rec.stack("gpu", &gpu);
        // The GL arm's windows can draw on another driver than the probe `gpu` names.
        let windows = proof::window_renderer(journal);
        if let Some(w) = windows {
            self.rec.stack("gpu.window", w);
        }
        let drew = windows.unwrap_or(&gpu).to_string();
        match self.ctl(&["metrics", "percentiles"]) {
            Ok(m) => {
                self.rec.measured(
                    "present.software",
                    format!(
                        "{} ({drew}): {}",
                        rendering_kind(&drew),
                        proof::present_summary(&m)
                    ),
                );
                self.rec
                    .measured("input.software", proof::input_summary(&m));
            }
            Err(e) => self
                .rec
                .measured("present.software", format!("failed: {e}")),
        }
        match self.ctl(&["metrics"]) {
            Ok(m) => self
                .rec
                .measured("present.slowest", slowest_present(&m, &self.marks)),
            Err(e) => self.rec.measured("present.slowest", format!("failed: {e}")),
        }
    }

    fn mark(&mut self, item: &'static str) {
        if let Some(now) = self
            .ctl(&["metrics"])
            .ok()
            .and_then(|m| proof::field(&m, "metrics_now_ms")?.parse::<f64>().ok())
        {
            self.marks.push((item, now));
        }
    }

    fn server_stack(&mut self) {
        if let Some(w) = self.wayland_display() {
            let out = self.wl(&w, &["wayland-info"]);
            self.rec.stack("server.globals", wayland_globals(&out.text));
        }
        if let Some(x) = self.x_display() {
            let out = self.x(&x, &["xdpyinfo", "-queryExtensions"]);
            self.rec.stack("server.extensions", x_extensions(&out.text));
            let keymap = self.x(&x, &["setxkbmap", "-query"]);
            self.rec
                .stack("server.keymap", keymap_summary(&keymap.text));
        }
        let mut argv = vec![
            "dpkg-query".to_string(),
            "-W".into(),
            "-f".into(),
            "${Package}:${Architecture} ${Version}\\n".into(),
        ];
        argv.extend(guest_packages(self.arch));
        let out = self.sh(&argv);
        for line in out.text.lines() {
            if let Some((pkg, version)) = line.split_once(' ') {
                self.rec.stack(&format!("guest.{pkg}"), version);
            }
        }
    }
}

/// Run and record one baseline session for `arch` and `screen`.
fn record_session(
    up: &Up,
    arch: Arch,
    screen: Screen,
    bin: &str,
    probes: (&str, &str),
    lock: &str,
    results: &Path,
) -> Result<Recording, String> {
    let (bus, wl) = probes;
    let dir = format!(
        "/tmp/{}{}-{}",
        guest::UNIT_PREFIX,
        arch.name(),
        screen.name()
    );
    let gui = Gui {
        up,
        bin: bin.to_string(),
        dir: dir.clone(),
        name: format!("{}-{}", arch.name(), screen.name()),
        cpu: false,
        deck: false,
    };
    gui.prepare().map_err(|e| format!("scratch world: {e}"))?;
    linux::start_baseline_screen(up, screen)?;
    let mut s = Session {
        gui,
        arch,
        screen,
        bus: bus.to_string(),
        wl: wl.to_string(),
        results: results.to_path_buf(),
        rec: Recording::default(),
        roles: Vec::new(),
        end_terminal: None,
        marks: Vec::new(),
    };
    s.rec.comments = vec![
        format!(
            "aterm Linux lane baseline (LW.4): {} ({}) under {} ({})",
            arch.name(),
            arch.runs(),
            screen.name(),
            screen.display.protocol()
        ),
        "= behaviour, compared exactly by the replay; ~ measured, shown when it moves; @ the stack"
            .into(),
        format!(
            "replay: {LAB} linux baseline --arch {} --screen {} (add --record to rewrite this file)",
            arch.name(),
            screen.name()
        ),
    ];
    for (name, version) in lock_stack(lock) {
        s.rec.stack(&format!("crate.{name}"), version);
    }
    let events = format!("{dir}/atspi-events.txt");
    let stop = format!("{dir}/atspi-stop");
    let listener = guest::Unit {
        name: format!("{}atspi", guest::UNIT_PREFIX),
        env: vec![(
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path={}/bus", up.runtime),
        )],
        argv: vec![
            bus.to_string(),
            "atspi-listen".into(),
            APP.into(),
            events.clone(),
            stop.clone(),
        ],
        socket: String::new(),
    };
    let listening = up.sh(&guest::run_unit(&listener, None));
    if !listening.ok() {
        return Err(format!(
            "the AT-SPI listener did not start: {}",
            listening.tail(3)
        ));
    }
    if !up.sh(&guest::wait_file(&events, 60)).ok() {
        let why = s.unit_journal(&listener.name);
        up.stop_units();
        return Err(format!(
            "the AT-SPI listener never subscribed (60 s): {why}"
        ));
    }
    let invocation = s.gui.start(Some(screen.display), &[])?;
    let started = (|| -> Result<String, String> {
        if !up.sh(&guest::wait_socket(&s.gui.sock(), 60)).ok() {
            return Err("aterm-gui's control socket never listened (60 s)".into());
        }
        let r = s.ctl(&["ready", "60000"])?;
        if !r.starts_with("OK ready") {
            return Err(format!("`ready` answered {r}"));
        }
        let _ = s.ctl(&["metrics", "reset"]);
        s.mark("settle");
        s.settled_dims()
    })();
    let d = match started {
        Ok(d) => d,
        Err(why) => {
            let journal = s.gui.journal(&invocation);
            let kept = s.gui.keep_crash_reports(results);
            s.gui.stop();
            return Err(format!(
                "{why}; aterm-gui's journal: {}{kept}",
                crate::util::one_line(&journal, 400)
            ));
        }
    };
    s.rec.behaviour("start.dims", dims_summary(&d));
    s.mark("echo");
    let echo = s.run_line(proof::TYPED, proof::ECHO);
    s.rec.behaviour(
        "echo",
        match &echo {
            Ok(()) => format!("typed `{}`, {} appeared", proof::TYPED, proof::ECHO),
            Err(e) => format!("failed: {e}"),
        },
    );
    s.mark("walk.start");
    s.walk("start");
    s.mark("resizes");
    if let Err(e) = s.resizes() {
        s.rec.behaviour("resize.input", format!("failed: {e}"));
    }
    s.mark("os-resize");
    s.os_resize();
    s.mark("ledger");
    s.ledger();
    s.mark("selections");
    match (s.x_display(), screen.display.seat()) {
        (Some(x), _) => s.x11_selections(&x),
        (None, Some(_)) => {
            let w = s.wayland_display().unwrap_or_default();
            s.wl_selections(&w);
        }
        (None, _) => s.rec.not_driven(
            "clipboard",
            "no-seat",
            "weston's headless backend advertises no wl_seat, and a Wayland selection is set with a seat's serial (the weston-x11 screen drives it: weston's x11 backend, nested in Xvfb, has a seat that XTEST on the Xvfb drives)",
        ),
    }
    s.mark("key-repeat");
    s.key_repeat();
    s.mark("compose");
    s.compose();
    match (s.x_display(), screen.display.seat()) {
        (Some(x), _) => {
            s.mark("dnd");
            s.dnd(&x);
        }
        (None, Some(_)) => {
            s.mark("dnd");
            let w = s.wayland_display().unwrap_or_default();
            s.wl_dnd(&w);
        }
        (None, _) => {}
    }
    s.mark("header");
    s.header();
    s.not_driven();
    s.mark("walk.end");
    s.walk("end");
    let journal = s.gui.journal(&invocation);
    s.percentiles(&journal);
    let pid = s.pid().unwrap_or_default();
    let _ = up.sh(&["touch".into(), stop]);
    let _ = up.sh(&guest::wait_unit_gone(&listener.name, 60));
    let stream = up.sh(&["cat".into(), events]);
    let lines: Vec<&str> = stream
        .text
        .lines()
        .filter(|l| event_parts(l).is_some_and(|(p, _)| p == pid))
        .collect();
    let kinds: BTreeSet<String> = lines
        .iter()
        .map(|l| event_kind(l, &s.roles))
        .filter(|k| !k.starts_with("Object.PropertyChange:accessible-name@"))
        .collect();
    s.rec.behaviour(
        "atspi.events.kinds",
        if kinds.is_empty() {
            "none".to_string()
        } else {
            kinds.into_iter().collect::<Vec<_>>().join(" ")
        },
    );
    s.rec.behaviour(
        "atspi.events.text",
        match &s.end_terminal {
            Some((text, caret)) => text_replay(&lines, &s.roles, text, caret.as_deref()),
            None => "no terminal in the end walk".to_string(),
        },
    );
    s.rec
        .measured("atspi.events.count", lines.len().to_string());
    for (i, l) in lines.iter().enumerate() {
        let body = event_body(l);
        let mut shown = body.to_string();
        for (path, role) in &s.roles {
            shown = shown.replacen(path.as_str(), &format!("<{role}>"), 1);
        }
        s.rec.measured(&format!("atspi.event.{}", i + 1), shown);
    }
    s.server_stack();
    s.gui.stop();
    s.xim();
    s.x11_wire();
    s.text_input_v3();
    s.header_gestures();
    s.cpu_seat();
    s.touch();
    s.text_input_v3_ime();
    up.stop_units();
    Ok(s.rec)
}

/// What drew a session's frames, as its `present.software` line names it: Mesa's software
/// rasterizer (llvmpipe, which lavapipe and zink over lavapipe also name) or a GPU, as zink over
/// Venus is on the lane's GPU instance (`lab linux --gpu`); neither when the renderer was not
/// read. The line keeps its recorded name.
#[must_use]
pub fn rendering_kind(gpu: &str) -> &'static str {
    if gpu.contains("llvmpipe") || gpu.contains("softpipe") {
        "software rendering"
    } else if gpu.is_empty() || gpu == "?" {
        "rendering"
    } else {
        "GPU rendering"
    }
}

/// Summarize the slowest present in `metrics` against the item start timestamps in `marks`.
#[must_use]
pub fn slowest_present(metrics: &str, marks: &[(&str, f64)]) -> String {
    let get = |k: &str| proof::field(metrics, k).unwrap_or("?");
    let at = get("max_present_latency_at_ms").parse::<f64>().ok();
    let during = at
        .and_then(|at| {
            marks
                .iter()
                .rev()
                .find(|(_, began)| *began <= at)
                .map(|(item, began)| format!("{item} +{:.0} ms", at - began))
        })
        .unwrap_or_else(|| "before the first mark".into());
    format!(
        "{} ms, ending in {during}; gap {} ms; max_frame_render_ms={} max_redraw_total_ms={} \
         max_turn_ms={} max_turn_owner={}",
        get("max_present_latency_ms"),
        get("max_present_latency_gap_ms"),
        get("max_frame_render_ms"),
        get("max_redraw_total_ms"),
        get("max_turn_ms"),
        get("max_turn_owner")
    )
}

/// Shared state across all baseline sessions in one lane run.
pub struct Baseline {
    bus: String,
    wl: String,
    lock: String,
    committed: PathBuf,
    results: PathBuf,
    record: bool,
}

/// Set `org.a11y.Status.IsEnabled` on the guest AT-SPI bus.
pub(crate) fn accessibility(up: &Up, on: bool) -> Out {
    up.sh(&[
        "busctl".into(),
        "--user".into(),
        "set-property".into(),
        "org.a11y.Bus".into(),
        "/org/a11y/bus".into(),
        "org.a11y.Status".into(),
        "IsEnabled".into(),
        "b".into(),
        on.to_string(),
    ])
}

impl Baseline {
    /// Build guest probes (`lane-bus`, `lane-wl`) and verify `org.a11y.Status.IsEnabled`.
    pub fn begin(up: &Up, results: &Path, record: bool) -> Result<Baseline, proof::Row> {
        let repo = linux::repo();
        let t0 = Instant::now();
        let not_run = |detail: String| proof::Row {
            name: "baseline: lane-bus, lane-wl and the accessibility bus".into(),
            verdict: proof::Verdict::NotRun,
            secs: t0.elapsed().as_secs_f64(),
            detail,
        };
        let lock = std::fs::read_to_string(Path::new(&repo).join("Cargo.lock"))
            .map_err(|e| not_run(format!("{repo}/Cargo.lock: {e}")))?;
        let _ = up.sh(&guest::a11y_address());
        let enabled = accessibility(up, true);
        let _ = accessibility(up, false);
        if !enabled.ok() {
            return Err(not_run(format!("IsEnabled: {}", enabled.tail(2))));
        }
        let lane = up.lane_dir();
        let env = up.cargo_env(Arch::Aarch64, &format!("{lane}/target"), false);
        for (what, package, target) in [
            ("lane-bus", "aterm-dbus", ["--bin", "lane-bus"]),
            ("lane-wl", "aterm-wayland", ["--example", "lane-wl"]),
        ] {
            println!("\n=== {what} (aarch64, the baseline's probe) ===");
            let mut argv: Vec<String> = [
                "cargo",
                "build",
                "--locked",
                "--manifest-path",
                &format!("{repo}/Cargo.toml"),
                "-p",
                package,
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
            argv.extend(target.iter().map(|s| (*s).to_string()));
            let built = up
                .guest
                .shell_streamed(&up.home, &guest::with_env(&env, &argv), "  | ");
            if !built.ok() {
                return Err(not_run(format!(
                    "{what} build: {}",
                    proof::tally(&built.text)
                        .error
                        .unwrap_or_else(|| built.tail(3))
                )));
            }
        }
        let bin_dir = Arch::Aarch64.bin_dir(&format!("{lane}/target"));
        Ok(Baseline {
            bus: format!("{bin_dir}/lane-bus"),
            wl: format!("{bin_dir}/examples/lane-wl"),
            lock,
            committed: PathBuf::from(&repo).join(DIR),
            results: results.to_path_buf(),
            record,
        })
    }

    /// Disable `org.a11y.Status.IsEnabled` in the guest.
    pub fn end(&self, up: &Up) {
        let _ = accessibility(up, false);
    }

    /// Record one session for `arch` and `screen` and compare or write the baseline file.
    pub fn session(&self, up: &Up, arch: Arch, screen: Screen, bin: &str) -> proof::Row {
        let name = format!(
            "{} ({}) baseline {}",
            arch.name(),
            arch.runs(),
            screen.name()
        );
        println!("\n=== {name} ===");
        let started = Instant::now();
        let enabled = accessibility(up, true);
        let recorded = if enabled.ok() {
            record_session(
                up,
                arch,
                screen,
                bin,
                (&self.bus, &self.wl),
                &self.lock,
                &self.results,
            )
        } else {
            Err(format!("IsEnabled: {}", enabled.tail(2)))
        };
        let _ = accessibility(up, false);
        let row = match recorded {
            Err(why) => {
                up.stop_units();
                proof::Row {
                    name,
                    verdict: proof::Verdict::NotRun,
                    secs: started.elapsed().as_secs_f64(),
                    detail: why,
                }
            }
            Ok(rec) => self.judge(&rec, arch, screen, name, started),
        };
        println!(
            "  {} {}",
            row.verdict.label(),
            row.detail.lines().next().unwrap_or("")
        );
        row
    }

    fn judge(
        &self,
        rec: &Recording,
        arch: Arch,
        screen: Screen,
        name: String,
        started: Instant,
    ) -> proof::Row {
        let file = file_name(arch, screen);
        let fresh = rec.render();
        let _ = std::fs::write(self.results.join(&file), &fresh);
        let secs = started.elapsed().as_secs_f64();
        let target = self.committed.join(&file);
        let old = std::fs::read_to_string(&target)
            .ok()
            .map(|t| Recording::parse(&t));
        let row = |verdict, detail| proof::Row {
            name: name.clone(),
            verdict,
            secs,
            detail,
        };
        if self.record {
            let wrote = std::fs::create_dir_all(&self.committed)
                .and_then(|()| std::fs::write(&target, &fresh));
            let moved = match &old {
                Some(Ok(o)) => {
                    let d = compare(o, rec);
                    format!(
                        "; against the one it replaced: {} behaviour, {} stack, {} measured line(s) moved",
                        d.behaviour.len(),
                        d.stack.len(),
                        d.measured.len()
                    )
                }
                _ => String::new(),
            };
            return match wrote {
                Ok(()) => row(
                    proof::Verdict::Pass,
                    format!(
                        "recorded {} lines to {}{moved}",
                        rec.lines.len(),
                        target.display()
                    ),
                ),
                Err(e) => row(proof::Verdict::Fail, format!("{}: {e}", target.display())),
            };
        }
        match old {
            None => row(
                proof::Verdict::NotRun,
                format!(
                    "no committed recording {} to replay against (record one with --record)",
                    target.display()
                ),
            ),
            Some(Err(e)) => row(proof::Verdict::Fail, format!("{}: {e}", target.display())),
            Some(Ok(o)) => {
                let d = compare(&o, rec);
                let head = format!(
                    "{} behaviour line(s) moved; {} stack, {} measured",
                    d.behaviour.len(),
                    d.stack.len(),
                    d.measured.len()
                );
                row(
                    if d.same_behaviour() {
                        proof::Verdict::Pass
                    } else {
                        proof::Verdict::Fail
                    },
                    format!("{head}\n{}", render_diff(&d, 12)),
                )
            }
        }
    }
}

/// Entry point for `linux baseline`.
pub fn cmd_baseline(args: &[String]) -> Res {
    let ask = parse_baseline(args)?;
    crate::stop::install();
    let _lock = linux::LaneLock::boot()?;
    let repo = linux::repo();
    let results = linux::state_dir()?.join(format!(
        "baseline-{}",
        crate::util::compact_stamp(crate::util::now_secs())
    ));
    std::fs::create_dir_all(&results)
        .map_err(|e| Fail::die(format!("{}: {e}", results.display())))?;
    let t0 = Instant::now();
    let up = match linux::bring_up() {
        Ok(up) => up,
        Err(f) => {
            let _ = std::fs::remove_dir(&results);
            return Err(f);
        }
    };
    let lane = up.lane_dir();
    let mut rows: Vec<proof::Row> = Vec::new();
    let mut stopped = None;
    match Baseline::begin(&up, &results, ask.record) {
        Err(row) => rows.push(row),
        Ok(baseline) => {
            'arches: for &arch in &ask.arches {
                let tag = format!("{} ({})", arch.name(), arch.runs());
                if arch == Arch::X86_64 {
                    let reg = up.sh(&["cat".into(), guest::ROSETTA_BINFMT.into()]);
                    if !guest::rosetta_registered(&reg.text) {
                        rows.push(proof::Row {
                            name: format!("{tag} every screen"),
                            verdict: proof::Verdict::NotRun,
                            secs: reg.secs,
                            detail: "Rosetta's binfmt registration is not enabled in the guest"
                                .into(),
                        });
                        continue;
                    }
                }
                let env = up.cargo_env(arch, &format!("{lane}/target"), false);
                let mut argv = vec![
                    "cargo".to_string(),
                    "build".into(),
                    "--locked".into(),
                    "--manifest-path".into(),
                    format!("{repo}/Cargo.toml"),
                ];
                argv.extend(arch.target_args());
                for a in [
                    "-p",
                    "aterm-gui",
                    "--bin",
                    "aterm-gui",
                    "-p",
                    "aterm-ctl",
                    "--bin",
                    "aterm-ctl",
                ] {
                    argv.push(a.into());
                }
                println!("\n=== {tag}: build aterm-gui, aterm-ctl ===");
                let built =
                    up.guest
                        .shell_streamed(&up.home, &guest::with_env(&env, &argv), "  | ");
                if !built.ok() {
                    rows.push(proof::Row {
                        name: format!("{tag} build aterm-gui"),
                        verdict: proof::Verdict::NotRun,
                        secs: built.secs,
                        detail: proof::tally(&built.text)
                            .error
                            .unwrap_or_else(|| built.tail(5)),
                    });
                    continue;
                }
                let bin = arch.bin_dir(&format!("{lane}/target"));
                for &screen in &ask.screens {
                    rows.push(baseline.session(&up, arch, screen, &bin));
                    if let Err(s) = crate::stop::check() {
                        stopped = Some(s);
                        break 'arches;
                    }
                }
            }
            baseline.end(&up);
        }
    }
    if !ask.keep_build
        && let Err(e) = up.clean_build()
    {
        eprintln!("aterm-lab: linux: guest build output NOT removed: {e}");
    }
    up.end(ask.keep_up);
    let report = proof::render(&rows, &[], t0.elapsed().as_secs_f64()).replacen(
        "LINUX LANE PROOF",
        if ask.record {
            "LINUX LANE BASELINE (record)"
        } else {
            "LINUX LANE BASELINE (replay)"
        },
        1,
    );
    print!("\n{report}");
    let path = results.join("report.txt");
    if let Err(e) = std::fs::write(&path, &report) {
        eprintln!("aterm-lab: linux: {}: {e}", path.display());
    } else {
        println!("report and fresh recordings: {}", results.display());
    }
    if let Some(s) = stopped {
        return Err(s.into());
    }
    let code = proof::exit_code(&rows);
    if code == linux::NOT_RUN {
        let why = rows
            .iter()
            .find(|r| r.verdict == proof::Verdict::NotRun)
            .map_or("no session ran".to_string(), |r| {
                format!("{}: {}", r.name, r.detail)
            });
        return Err(linux::not_run(crate::util::one_line(&why, 300)));
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_present_line_names_software_only_for_mesa_s_software_rasterizer() {
        assert_eq!(
            rendering_kind("llvmpipe (LLVM 20.1.2, 128 bits) (Gl)"),
            "software rendering"
        );
        assert_eq!(
            rendering_kind(
                "zink Vulkan 1.4(llvmpipe (LLVM 20.1.2, 128 bits) (MESA_LLVMPIPE)) (Gl)"
            ),
            "software rendering"
        );
        assert_eq!(
            rendering_kind("zink Vulkan 1.3(Virtio-GPU Venus (Apple M4 Pro) (MESA_VENUS)) (Gl)"),
            "GPU rendering"
        );
        assert_eq!(
            rendering_kind("?"),
            "rendering",
            "an unread renderer claims neither"
        );
    }

    fn rec(lines: &[(Class, &str, &str)]) -> Recording {
        let mut r = Recording::default();
        for (c, k, v) in lines {
            r.push(*c, k, *v);
        }
        r
    }

    #[test]
    fn a_recording_reads_back_what_it_wrote() {
        let mut r = rec(&[
            (Class::Stack, "guest.weston", "13.0.0"),
            (Class::Behaviour, "echo", "typed\nthen shown"),
            (Class::Measured, "keyrepeat.count", "23"),
        ]);
        r.comments.push("a title".into());
        r.behaviour("echo", "again");
        assert_eq!(r.lines[3].key, "echo.2", "a repeated key gets a suffix");
        let text = r.render();
        assert!(text.starts_with("# a title\n@ guest.weston 13.0.0\n= echo typed\\nthen shown\n"));
        assert_eq!(Recording::parse(&text).expect("parses"), r);
        assert!(Recording::parse("= \n").is_err());
        assert!(Recording::parse("! key v\n").is_err());
        assert!(Recording::parse("==key v\n").is_err());
    }

    #[test]
    fn only_behaviour_decides_the_replay() {
        let old = rec(&[
            (Class::Behaviour, "start.dims", "24x80 scale=1.00"),
            (Class::Measured, "keyrepeat.count", "23"),
            (Class::Stack, "crate.x11rb", "0.13.2"),
        ]);
        let same = old.clone();
        assert_eq!(compare(&old, &same), Diff::default());
        let mut moved = old.clone();
        moved.lines[1].value = "22".into();
        moved.lines[2].value = "0.14.0".into();
        let d = compare(&old, &moved);
        assert!(d.same_behaviour(), "{d:?}");
        assert_eq!((d.measured.len(), d.stack.len()), (1, 1));
        let mut broke = old.clone();
        broke.lines[0].value = "24x80 scale=2.00".into();
        assert!(!compare(&old, &broke).same_behaviour());
        let mut gone = old.clone();
        gone.lines.remove(0);
        assert!(!compare(&old, &gone).same_behaviour());
        let mut came = old.clone();
        came.behaviour("not-driven.dnd", "now driven");
        let d = compare(&old, &came);
        assert_eq!(
            d.behaviour[0],
            ("not-driven.dnd".into(), None, Some("now driven".into()))
        );
        let mut reclassed = old.clone();
        reclassed.lines[0].class = Class::Measured;
        assert!(
            !compare(&old, &reclassed).same_behaviour(),
            "a class change is a move"
        );
        let text = render_diff(&compare(&old, &broke), 12);
        assert!(text.contains("behaviour start.dims:\n  - 24x80 scale=1.00\n  + 24x80 scale=2.00"));
    }

    #[test]
    fn every_arch_and_screen_unless_named() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        let all = parse_baseline(&[]).expect("parses");
        assert_eq!(all.arches, Arch::ALL);
        assert_eq!(all.screens, Screen::BASELINE);
        assert!(!all.record);
        let one = parse_baseline(&s(&[
            "--screen",
            "mutter-scale1.5",
            "--arch",
            "x86_64",
            "--record",
        ]))
        .expect("parses");
        assert_eq!(one.screens[0].name(), "mutter-scale1.5");
        assert_eq!(one.arches, [Arch::X86_64]);
        assert!(one.record);
        assert_eq!(
            parse_baseline(&s(&["--screen", "kwin"]))
                .map_err(|f| f.code)
                .err(),
            Some(2)
        );
        let names: Vec<String> = Screen::BASELINE.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            [
                "xvfb",
                "xvfb-dpi144",
                "weston",
                "weston-scale2",
                "mutter",
                "mutter-scale1.5",
                "xwayland",
                "weston-x11"
            ]
        );
        assert_eq!(
            file_name(Arch::Aarch64, Screen::BASELINE[5]),
            "aarch64-mutter-scale1.5.txt"
        );
    }

    #[test]
    fn a_drag_source_hears_the_window_take_and_leave_the_drag() {
        let over = ("text/uri-list", Some("copy"));
        let away = ("none", None);
        let mutter = [
            "target none",
            "action none",
            "target text/uri-list",
            "action copy",
            "target none",
            "target text/uri-list",
            "action copy",
        ];
        assert_eq!(
            source_heard(&mutter[..4], 0, over).as_deref(),
            Some("target text/uri-list, action copy")
        );
        assert_eq!(source_heard(&mutter[..4], 4, away), None, "nothing yet");
        assert_eq!(
            source_heard(&mutter[..5], 4, away).as_deref(),
            Some("target none")
        );
        assert_eq!(
            source_heard(&mutter[..6], 5, over).as_deref(),
            Some("target text/uri-list"),
            "the action wanted is the last one anywhere"
        );
        assert_eq!(
            source_heard(&mutter, 5, over).as_deref(),
            Some("target text/uri-list, action copy")
        );
        let weston = [
            "target text/uri-list",
            "action copy",
            "action none",
            "action copy",
        ];
        assert_eq!(
            source_heard(&weston[..2], 0, over).as_deref(),
            Some("target text/uri-list, action copy")
        );
        assert_eq!(
            source_heard(&weston, 2, away).as_deref(),
            Some("action none"),
            "weston says nothing at a leave: the next client's offer starts at none"
        );
        assert_eq!(
            source_heard(&["action none", "target text/uri-list"], 0, away),
            None,
            "a target past the point is no leave"
        );
        assert_eq!(source_heard(&["action copy"], 0, away), None);
    }

    #[test]
    fn the_slowest_present_is_named_by_the_item_it_ended_in() {
        let metrics = "OK backend=gpu max_present_latency_ms=301.40 max_frame_render_ms=297.10 \
                       max_redraw_total_ms=299.00 max_turn_ms=300.20 max_turn_owner=redraw \
                       max_present_latency_at_ms=5230.00 max_present_latency_gap_ms=410.00 \
                       metrics_now_ms=9000.00";
        let marks = [
            ("settle", 1000.0),
            ("echo", 1200.0),
            ("resizes", 5000.0),
            ("ledger", 6000.0),
        ];
        assert_eq!(
            slowest_present(metrics, &marks),
            "301.40 ms, ending in resizes +230 ms; gap 410.00 ms; max_frame_render_ms=297.10 \
             max_redraw_total_ms=299.00 max_turn_ms=300.20 max_turn_owner=redraw"
        );
        assert!(slowest_present(metrics, &marks[3..]).contains("before the first mark"));
        assert!(slowest_present("OK", &marks).starts_with("? ms, ending in before the first mark"));
    }

    #[test]
    fn the_replies_are_kept_without_what_moves_with_time_or_edits() {
        let dims = "OK 24 80 1120 720 session=0 cell_w=14 cell_h=30 font_px=23.00 scale=1.50 window=0 window_rows=24 window_cols=80 composed_rows=24 grid_w=1120 grid_h=720 frame_w=1156 frame_h=792 surface_w=1157 surface_h=792 offset_x=18 offset_y=54 band_left=0 band_right=0 band_top=0 band_bottom=0 crop_left=0 crop_right=0 crop_top=0 crop_bottom=0 pad=18 pad_top=2 pad_bottom=18 head=34 tab_rows=1 viewers=1 visible_viewers=1 geometry=settled present_retry_state=idle";
        assert_eq!(
            dims_summary(dims),
            "24x80 scale=1.50 cell_w=14 cell_h=30 font_px=23.00 window_rows=24 window_cols=80 grid_w=1120 grid_h=720 frame_w=1156 frame_h=792 surface_w=1157 surface_h=792 offset_x=18 offset_y=54 band_left=0 band_right=0 band_top=0 band_bottom=0 crop_left=0 crop_right=0 crop_top=0 crop_bottom=0 pad=18 pad_top=2 pad_bottom=18 head=34 tab_rows=1"
        );
        assert!(dims_summary("ERR busy").starts_with("refused"));
        assert_eq!(
            resize_line(
                "1 t=480 run=1 ord=1 from=80x24 to=80x22 alt=0 sync=0 trimmed=2 appended=0 restored=0 shift=+0 site=window at=crates/aterm-gui/src/app_config.rs:8062 win=80x22 chrome=1"
            ),
            (
                "1 ord=1 from=80x24 to=80x22 alt=0 sync=0 site=window win=80x22 chrome=1".to_string(),
                "1 run=1 ord=1 from=80x24 to=80x22 alt=0 sync=0 trimmed=2 appended=0 restored=0 shift=+0 site=window win=80x22 chrome=1".to_string()
            ),
            "how the rows moved and the run are measured; the time and source line dropped"
        );
        assert_eq!(
            keep_fields(
                "total=6 runs=5 flaps=1 risks=0 render=ok unledgered=0 lost=0 nudge=idle nudged=0",
                &SUMMARY_BEHAVIOUR
            ),
            "total=6 risks=0 render=ok unledgered=0 lost=0"
        );
        assert_eq!(
            drift_line("OK 1 verdict=clean fidelity=pass against=screen drift_rows=0 runs=1 culprit=- anchor=47.41 seed=primary geom=80x24 ms=0\nrun t=438.6 n=3"),
            (
                "verdict=clean fidelity=pass against=screen drift_rows=0 culprit=- geom=80x24".to_string(),
                "OK 1 verdict=clean fidelity=pass against=screen drift_rows=0 runs=1 culprit=- seed=primary geom=80x24".to_string()
            )
        );
        assert_eq!(
            xset_repeat("  auto repeat delay:  660    repeat rate:  25\n").as_deref(),
            Some("delay=660 rate=25")
        );
        assert_eq!(
            wayland_repeat("interface: 'wl_seat', version: 9, name: 13\n\tname: seat0\n\tcapabilities: pointer keyboard\n\tkeyboard repeat rate: 33\n\tkeyboard repeat delay: 500\n").as_deref(),
            Some("delay=500 rate=33")
        );
        assert_eq!(wayland_repeat("\tcapabilities: pointer\n"), None);
        assert_eq!(
            typed_after_prompt("lane$ echo x\nx\nlane$ aaaaa\n\n", 'a'),
            5
        );
        assert_eq!(typed_after_prompt("no prompt\n", 'a'), 0);
        assert_eq!(
            typed_line("lane$ echo x\nx\nlane$ /tmp/a\\ b.txt\n\n"),
            "/tmp/a\\ b.txt"
        );
        assert_eq!(typed_line("lane$ echo x\nx\nlane$\n"), "");
    }

    #[test]
    fn a_literal_becomes_one_word_of_regex() {
        assert_eq!(regex_literal("echo X-x"), r"echo\sX-x");
        assert_eq!(regex_literal("lane$"), r"lane\$");
        assert_eq!(
            regex_literal("echo LANE-$((6*7))"),
            r"echo\sLANE-\$\(\(6\*7\)\)"
        );
        assert!(!regex_literal("a b c").contains(' '));
    }

    #[test]
    fn text_is_typed_on_the_seat_a_key_each() {
        assert_eq!(
            seat_taps("echo a-1"),
            [
                "tap:e",
                "tap:c",
                "tap:h",
                "tap:o",
                "tap:space",
                "tap:a",
                "tap:-",
                "tap:1"
            ]
        );
        assert!(seat_taps("").is_empty());
    }

    #[test]
    fn the_text_changes_rebuild_the_end_walks_text() {
        let roles = vec![("/t".to_string(), "terminal".to_string())];
        let events = [
            r#"1 7 Object.TextChanged /t detail="insert" d1=6 d2=6 any="echo x""#,
            r#"2 7 Object.TextChanged /t detail="delete" d1=12 d2=1 any="\n""#,
            r#"3 7 Object.TextChanged /t detail="insert" d1=12 d2=8 any="\nx\nlane$""#,
            r#"4 7 Object.TextCaretMoved /t detail="" d1=20 d2=0 any=0"#,
            r#"5 7 Object.BoundsChanged /f detail="" d1=0 d2=0 any=(0,0,1,1)"#,
        ];
        let end = "lane$ echo x\nx\nlane$";
        assert_eq!(
            text_replay(&events, &roles, end, Some("20")),
            "the text changes undo cleanly from the end walk's text; the last caret event names the end walk's caret"
        );
        assert_eq!(
            text_replay(&events, &roles, "lane$ echo y\nx\nlane$", Some("20")),
            "insert at 6 of 6 does not fit the text the later changes leave",
            "an insert must find its own text"
        );
        assert!(
            text_replay(&events, &roles, end, Some("19"))
                .ends_with("the end walk's caret is not the last caret event's")
        );
        assert_eq!(
            text_replay(&events[4..], &roles, end, None),
            "no text change; no caret event"
        );
        assert_eq!(
            quoted_field(
                r#"node role=terminal text="a \"b\"\n\u{e9}" caret=3"#,
                "text"
            )
            .as_deref(),
            Some("a \"b\"\n\u{e9}")
        );
    }

    #[test]
    fn the_text_input_wire_keeps_names_and_the_content_type() {
        let log = r#"[1545378.526] wl_registry@2.global(24, "zwp_text_input_manager_v3", 1)
[1545378.743]  -> wl_registry@2.bind(24, "zwp_text_input_manager_v3", 1, new id [unknown]@19)
[1546509.728]  -> zwp_text_input_manager_v3@19.get_text_input(new id zwp_text_input_v3@79, wl_seat@8)
[1546509.991] wl_keyboard@81.enter(31, wl_surface@22, array[0])
[1546509.994] zwp_text_input_v3@79.enter(wl_surface@22)
[1546510.818]  -> zwp_text_input_v3@79.enable()
[1546510.823]  -> zwp_text_input_v3@79.set_content_type(0, 13)
[1546510.825]  -> zwp_text_input_v3@79.commit()
[1546514.163]  -> zwp_text_input_v3@79.set_cursor_rectangle(66, 36, 9, 20)
[1546514.176]  -> zwp_text_input_v3@79.commit()
[1546716.728]  -> zwp_text_input_v3@79.destroy()
"#;
        assert_eq!(
            text_input_wire(log),
            "binds zwp_text_input_manager_v3 v1; requests zwp_text_input_manager_v3.get_text_input zwp_text_input_v3.enable zwp_text_input_v3.set_content_type(0, 13) zwp_text_input_v3.commit zwp_text_input_v3.set_cursor_rectangle zwp_text_input_v3.destroy; events zwp_text_input_v3.enter"
        );
        assert_eq!(
            text_input_wire("[1.0] wl_keyboard@81.enter(31, wl_surface@22, array[0])\n"),
            "binds none; requests none; events none"
        );
    }

    #[test]
    fn the_touch_wire_keeps_names_and_the_first_down() {
        let log = "[1166175.811]  -> wl_seat@7.get_touch(new id wl_touch@33)
[1176533.604] wl_touch@33.down(57, 1285080, wl_surface@16, 736, 640.00000000, 400.00000000)
[1176533.614] wl_touch@33.frame()
[1176534.035] wl_touch@33.motion(1285080, 736, 650.00000000, 410.00000000)
[1176534.041] wl_touch@33.frame()
[1176534.043] wl_touch@33.up(58, 1285080, 736)
[1177545.446]  -> wl_touch@33.release()
";
        assert_eq!(
            touch_wire(log),
            "requests wl_seat.get_touch wl_touch.release; events wl_touch.down wl_touch.frame wl_touch.motion wl_touch.up"
        );
        assert_eq!(
            touch_down_at(log).as_deref(),
            Some("id 736 at 640.00000000,400.00000000")
        );
        let none = "[1.0] wl_keyboard@81.enter(31, wl_surface@22, array[0])\n";
        assert_eq!(touch_wire(none), "requests none; events none");
        assert_eq!(touch_down_at(none), None);
    }

    #[test]
    fn the_caption_line_reads_back_the_frame_and_its_buttons() {
        let chrome = "toolbar (none)\nstrip-caption frame=client buttons=minimize:68..72,maximize:72..76,close:76..80 hovered=none maximized=false edges=true menu=true\nmenu \"File\": New Tab";
        let c = parse_caption(chrome).expect("a caption line");
        assert!(c.client && c.edges && c.menu && !c.maximized);
        assert_eq!(c.names(), "minimize,maximize,close");
        assert_eq!(c.middle("maximize"), Some(74));
        assert_eq!(c.middle("restore"), None);
        assert_eq!(c.right_end(80), 68);
        let left = parse_caption(
            "strip-caption frame=client buttons=close:0..4,minimize:4..8,maximize:76..80 maximized=false",
        )
        .expect("a caption line");
        assert_eq!(
            left.right_end(80),
            76,
            "the left end's run is not the right end"
        );
        let only_left =
            parse_caption("strip-caption frame=client buttons=close:0..4 maximized=false")
                .expect("a caption line");
        assert_eq!(only_left.right_end(80), 80);
        let system = parse_caption("strip-caption frame=system").expect("a caption line");
        assert!(!system.client);
        assert_eq!(system.names(), "none");
        assert_eq!(parse_caption("toolbar (none)"), None, "no line");
        assert_eq!(
            parse_caption("strip-caption frame=client buttons=close:9..4 maximized=false"),
            None,
            "a slot that ends before it starts is not read"
        );
        let none = parse_caption(
            "strip-caption frame=client buttons=none hovered=none maximized=true edges=false menu=false",
        )
        .expect("a caption line");
        assert!(none.buttons.is_empty() && none.maximized);
    }

    #[test]
    fn the_ime_line_is_found_among_the_chrome_lines() {
        let chrome = "toolbar (none)\nstrip-caption frame=system\nime preedit=\"ni hao\" caret=6 spot=66,36,9x20\nmenu \"File\": New Tab";
        assert_eq!(
            ime_line(chrome),
            Some("ime preedit=\"ni hao\" caret=6 spot=66,36,9x20")
        );
        assert_eq!(ime_line("toolbar (none)\nmenu \"ime\": x"), None, "no line");
        assert_eq!(
            ime_composition("ime preedit=\"\" caret=- spot=174,76,9x20"),
            ime_composition("ime preedit=\"\" caret=- spot=165,76,9x20"),
            "a spot that moved is no composition"
        );
        assert_ne!(
            ime_composition("ime preedit=\"n\" caret=1 spot=174,76,9x20"),
            ime_composition("ime preedit=\"\" caret=- spot=174,76,9x20"),
        );
    }

    #[test]
    fn the_frame_requests_keep_names_and_the_resize_edge() {
        let log = "[1.0]  -> xdg_toplevel@17.set_title(\"~\")
[2.0]  -> xdg_toplevel@17.move(wl_seat@8, 1234)
[3.0]  -> xdg_toplevel@17.move(wl_seat@8, 1250)
[4.0]  -> xdg_toplevel@17.resize(wl_seat@8, 1260, 8)
[5.0] xdg_toplevel@17.configure(800, 528, array[4])
[6.0]  -> xdg_toplevel@17.show_window_menu(wl_seat@8, 1270, 300, 18)
[7.0]  -> xdg_toplevel@17.set_maximized()
";
        assert_eq!(
            frame_requests(log),
            "move resize(right) show_window_menu set_maximized"
        );
        assert_eq!(frame_requests("[1.0] xdg_toplevel@17.close()\n"), "none");
    }

    #[test]
    fn a_caption_slot_is_34_logical_px_in_whole_cells() {
        assert_eq!(caption_slot_cols(9, 1.0), 4);
        assert_eq!(caption_slot_cols(14, 1.5), 4);
        assert_eq!(caption_slot_cols(18, 2.0), 4);
        assert_eq!(caption_slot_cols(20, 1.0), 2);
        assert_eq!(caption_slot_cols(0, 1.0), 0);
    }

    #[test]
    fn a_slot_with_a_button_reads_drawn_and_bare_band_reads_bare() {
        let band = [48u8, 48, 48, 255];
        let ink = [200u8, 200, 200, 255];
        let mut rgba = Vec::new();
        for y in 0..4 {
            for x in 0..8 {
                let on = (5..8).contains(&x) && (1..4).contains(&y);
                rgba.extend_from_slice(if on { &ink } else { &band });
            }
        }
        let f = Frame { rgba, w: 8, h: 4 };
        assert_eq!(slot_class(&f, 4, 8, 0, 4), ("drawn", 9));
        assert_eq!(slot_class(&f, 0, 4, 0, 4), ("bare", 0));
        assert_eq!(slot_class(&f, 8, 9, 0, 4), ("bare", 0), "past the frame");
        assert_eq!(slot_class(&f, 4, 8, 1, 4), ("drawn", 9));
        assert_eq!(slot_class(&f, 4, 8, 2, 4), ("drawn", 6));
        assert_eq!(slot_class(&f, 4, 8, 4, 4), ("bare", 0), "no rows");
    }

    #[test]
    fn a_frame_change_is_classed_by_how_much_moved() {
        let frame = |px: &[[u8; 4]]| Frame {
            rgba: px.iter().flatten().copied().collect(),
            w: px.len(),
            h: 1,
        };
        let rest = frame(&[[0, 0, 0, 255]; 200]);
        let mut cell = rest.rgba.clone();
        cell[0] = 9;
        let cell = Frame {
            rgba: cell,
            w: 200,
            h: 1,
        };
        assert_eq!(frame_change(&rest, &rest).0, "at most a cell");
        assert_eq!(frame_change(&rest, &cell).0, "at most a cell");
        let washed = frame(&[[9, 9, 9, 255]; 200]);
        assert_eq!(frame_change(&rest, &washed).0, "most pixels");
        assert_eq!(frame_change(&rest, &washed).1, "200 of 200 pixels");
        let mut half = vec![[0, 0, 0, 255]; 150];
        half.extend([[9, 9, 9, 255]; 50]);
        assert_eq!(frame_change(&rest, &frame(&half)).0, "part");
        assert!(
            frame_change(&rest, &frame(&[[0, 0, 0, 255]; 3]))
                .0
                .starts_with("size")
        );
    }

    #[test]
    fn an_event_is_kept_by_kind_and_role() {
        let roles = vec![
            (
                "/org/a11y/atspi/accessible/0/0".to_string(),
                "frame".to_string(),
            ),
            (
                "/org/a11y/atspi/accessible/0/1".to_string(),
                "terminal".to_string(),
            ),
        ];
        let state = "510 77 Object.StateChanged /org/a11y/atspi/accessible/0/1 detail=\"focused\" d1=1 d2=0 any=0";
        assert_eq!(
            event_kind(state, &roles),
            "Object.StateChanged:focused=1@terminal"
        );
        let activate =
            "510 77 Window.Activate /org/a11y/atspi/accessible/0/0 detail=\"\" d1=0 d2=0 any=\"~\"";
        assert_eq!(event_kind(activate, &roles), "Window.Activate@frame");
        let text = "936 77 Object.TextChanged /x detail=\"insert\" d1=126 d2=2 any=\" a\"";
        assert_eq!(event_kind(text, &roles), "Object.TextChanged:insert@?");
        assert_eq!(
            event_body(text),
            "Object.TextChanged /x detail=\"insert\" d1=126 d2=2 any=\" a\""
        );
        assert_eq!(event_parts(text).map(|(pid, _)| pid), Some("77"));
        let node = "node depth=2 path=/org/a11y/atspi/accessible/0/1 role=terminal name=\"aterm terminal\"";
        let (path, rest) = node_parts(node).expect("a node");
        assert_eq!(path, "/org/a11y/atspi/accessible/0/1");
        assert_eq!(rest, "depth=2 role=terminal name=\"aterm terminal\"");
    }

    #[test]
    fn the_stack_is_read_from_the_lock_and_the_servers() {
        let lock = "[[package]]\nname = \"x11rb\"\nversion = \"0.13.2\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"winit\"\nversion = \"0.30.13\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+x\"\n";
        assert_eq!(
            lock_stack(lock),
            [
                ("winit".to_string(), "0.30.13 (path)".to_string()),
                ("x11rb".to_string(), "0.13.2".to_string())
            ]
        );
        let info = "interface: 'wl_seat',                                    version:  8, name: 14\n\tname: seat0\ninterface: 'wl_compositor',                              version:  5, name:  1\n";
        assert_eq!(wayland_globals(info), "wl_compositor@5,wl_seat@8");
        let xd = "name of display:    :91\nnumber of extensions:    3\n    XTEST  (opcode: 132)\n    RANDR  (opcode: 140, base event: 89, base error: 147)\n    BIG-REQUESTS\ndefault screen number:    0\n";
        assert_eq!(x_extensions(xd), "BIG-REQUESTS,RANDR,XTEST");
        assert_eq!(
            keymap_summary(
                "rules:      evdev\nmodel:      pc105\nlayout:     us\nvariant:    intl\n"
            ),
            "us intl"
        );
        assert!(guest_packages(Arch::X86_64).contains(&"libx11-6:amd64".to_string()));
        assert!(guest_packages(Arch::Aarch64).contains(&"weston".to_string()));
    }

    #[test]
    fn walk_settled_requires_idle_frame_title_and_trailing_prompt() {
        let snap = |title: &str, text: &str| {
            format!(
                "apps 1\n\
                 node depth=0 path=/org/a11y/atspi/accessible/root role=application name=\"aterm-gui\"\n\
                 node depth=1 path=/org/a11y/atspi/accessible/0/0 role=frame name=\"{title}\"\n\
                 node depth=2 path=/org/a11y/atspi/accessible/0/1 role=terminal name=\"aterm terminal\" text=\"{text}\"\n"
            )
        };
        assert!(walk_settled(&snap(
            "~",
            "lane$ echo LANE-$((6*7))\\nLANE-42\\nlane$\\n\\n\\n"
        )));
        assert!(!walk_settled(&snap(
            "echo LANE-$((6*7))",
            "lane$ echo LANE-$((6*7))\\nLANE-42\\nlane$\\n\\n\\n"
        )));
        assert!(!walk_settled(&snap(
            "~",
            "lane$ echo LANE-$((6*7))\\nLANE-42\\n\\n\\n"
        )));
        assert!(!walk_settled(&snap(
            "~",
            "lane$ echo LANE-$((6*7))\\nLANE-42\\nlane$ echo X-WLCOPY-42 | tr A-Z a-z\\nx-wlcopy-42\\n\\n"
        )));
    }
}
