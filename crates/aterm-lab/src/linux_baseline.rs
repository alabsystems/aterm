// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux lane's baseline (`linux baseline`; step LW.4 of
//! `docs/AUDIT-third-party-2026-10-15.md`): what today's Linux stack (the
//! winit fork's X11 and Wayland backends, softbuffer, x11rb, sctk, the AT-SPI
//! publisher) does for one scripted aterm-gui session, recorded
//! per architecture and screen ([`guest::Screen::BASELINE`]), and replayed
//! against that record. The steps that replace the stack (LW.5-7, A11Y.2-5,
//! GW.2-4) run the replay, and their commits diff the recordings.
//!
//! One session, in order, each item recording what happened (an item that
//! fails records its failure, and the session goes on):
//!
//! 1. an AT-SPI listener (`lane-bus atspi-listen`, crates/aterm-dbus) on
//!    at-spi2-core's bus, with `org.a11y.Status.IsEnabled` on for the
//!    session, so aterm's AT-SPI publisher publishes from its first frame;
//!    aterm-gui starts once the listener's match is in place;
//! 2. aterm-gui in a scratch world (a prompt of its own, `lane$ `), `ready`;
//!    `dims` (grid, scale, cell, font, frame, surface, padding, bands and
//!    crop);
//! 3. a typed `echo` whose output must appear; the AT-SPI tree walked
//!    (`lane-bus atspi-walk`);
//! 4. resizes from the control socket (30x100, 24x80, and a one-row flap),
//!    and on X11 one from the window system (xdotool's `windowsize`), each
//!    with `dims`; then `status` (`render=`, `resizes=`), the `resizes`
//!    ledger and `cast drift`;
//! 5. CLIPBOARD and PRIMARY, both ways, through a second client: on X11
//!    xclip, with PRIMARY made by a real mouse drag and pasted by a real
//!    middle click (xdotool's XTEST); under Mutter wl-clipboard, with the
//!    keys, and PRIMARY's drag and middle click (since LW.7), from Mutter's
//!    RemoteDesktop virtual keyboard and pointer (`lane-bus seat`), the only
//!    input a headless compositor can be given here;
//! 6. key repeat: a key held 1.5 s (XTEST on X11, the virtual keyboard
//!    under Mutter), and how many characters it typed, with the server's
//!    repeat settings as a stack line;
//! 7. on X11, compose (a dead key through XTEST) and a file dropped by XDND
//!    (`lane-bus xdnd`, a second X client as the drag source: dragged over
//!    the window and away, then dropped), with the drop highlight read from
//!    frames; after the session, XIM through ibus;
//! 8. the AT-SPI tree again, then the event stream the listener recorded,
//!    its kinds and whether its text changes rebuild the end walk's text;
//! 9. the present and input percentiles (`metrics percentiles`, reset after
//!    `ready`), on llvmpipe: software rendering, never a GPU's figure;
//! 10. the stack: the guest's packages, the crates the lock resolves for the
//!     windowing and accessibility stack, the server's globals or
//!     extensions, the GPU.
//!
//! Every wait for something that must happen is a one-minute hang detector:
//! a paste is awaited on the typed line before the rest of the line is
//! sent, a selection before it is read, a seat's keys before it lets go of
//! the keyboard, and the AT-SPI listener's match before aterm starts.
//!
//! A recording is lines `<class> <key> <value>`:
//!
//! * `=` behaviour: the replay compares it exactly, and a line that moved,
//!   came or went is a FAIL;
//! * `~` measured (counts and latencies that move from run to run, and the
//!   raw AT-SPI event stream, whose batching follows frame timing): shown
//!   when it moves, never a verdict;
//! * `@` the stack: shown as the stack's diff, never a verdict;
//! * `#` a comment.
//!
//! The recordings live in `crates/aterm-lab/linux-baseline/<arch>-<screen>.txt`.
//! `linux baseline` records afresh and compares (exit 0 the same behaviour,
//! 1 it moved, 3 NOT RUN); `--record` writes the fresh ones there. An item
//! this session does not drive is a line `= not-driven.<item> <token>`, the
//! token a short word for why (`no-seat`, `no-keycode`, …), and the reason in
//! prose a `#` comment, which the replay never compares: the line moves (and
//! the replay says so) the day the item is driven, never when its
//! explanation is reworded.
//!
//! STOCK EXCEPTION: the guest builds with rustup's stock toolchain, as the
//! proof does ([`crate::linux_proof`]).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::linux::{self, Out, Up};
use crate::linux_guest::{self as guest, Arch, Display, Screen, Selection};
use crate::linux_proof::{self as proof, Gui};
use crate::{Fail, LAB, Res};

/// Where the recordings are committed, under the checkout.
pub const DIR: &str = "crates/aterm-lab/linux-baseline";

/// The application name aterm-gui's AT-SPI root carries (aterm-a11y's
/// publisher names it after the executable, as accesskit_unix did).
pub const APP: &str = "aterm-gui";

/// The compose check: `echo X-` sent, `x` typed and seen (the typed line
/// shows `echo X-x`, its output never does: the shell has not run it yet),
/// `dead_acute` then `e` typed, `-Y` sent.
pub const COMPOSE_BEFORE: &str = "echo X-";
pub const COMPOSE_LIVE: &str = "echo X-x";
pub const COMPOSE_AFTER: &str = "-Y";
pub const COMPOSED: &str = "X-x\u{e9}-Y";

/// ibus's engine for the XIM check: its simple engine, which composes dead
/// keys (an engine must be set, or ibus passes every key through untouched).
pub const XIM_ENGINE: &str = "xkb:us::eng";

/// How long a key is held for the repeat measurement.
pub const HOLD_MS: u64 = 1500;

/// The XDND check's file: created in the guest before the drag (the target
/// resolves the path it is given), with a space in its name, which the uri
/// carries percent-encoded, the target decodes, and aterm types
/// shell-escaped.
pub const DND_DIR: &str = "/tmp/aterm-lane-dnd";
pub const DND_FILE: &str = "dropped file.txt";
pub const DND_URI: &str = "file:///tmp/aterm-lane-dnd/dropped%20file.txt";

/// What the drop types that no earlier row shows: awaited on the typed line.
pub const DND_STEM: &str = "aterm-lane-dnd";

/// A window pixel, `(x, y)`.
type Px = (i32, i32);

/// A decoded frame: RGBA pixels, row-major.
struct Frame {
    rgba: Vec<u8>,
    w: usize,
    h: usize,
}

/// How much of frame `b` differs from `a`, as a class a replay compares:
/// `at most a cell` (at most 1% of the pixels: no change, or a cursor's
/// blink), `most pixels` (over half: a wash over the window), or `part`;
/// and the count, which is measured.
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

/// The text-input-v3 part of a client's Wayland wire log
/// (`WAYLAND_DEBUG=client`): the manager's version as bound, then each
/// request (`->`) and event the client logged on a `zwp_text_input*`
/// object, by interface and name, once each, in the order they first came.
/// Object ids and arguments are left out (a cursor rectangle follows where
/// the prompt was when focus came, which is timing), except a content
/// type's, which says what the client declares itself.
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

/// The window's own frame as aterm-gui's `chrome` verb reports it (its
/// `strip-caption` line, LW.5): whether the window draws its own frame
/// (`frame=client`, a Wayland compositor that leaves the client undecorated)
/// or the window system decorates it (`frame=system`), and the header's
/// buttons as the strip laid them out, by name and strip columns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caption {
    pub client: bool,
    pub buttons: Vec<(String, u16, u16)>,
    pub maximized: bool,
    pub edges: bool,
    pub menu: bool,
}

impl Caption {
    /// The middle strip column of the button named `name`.
    #[must_use]
    pub fn middle(&self, name: &str) -> Option<u16> {
        self.buttons
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|&(_, s, e)| s + (e - s) / 2)
    }

    /// The buttons' names, in strip order, `none` without any.
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

/// The `strip-caption` line of a `chrome` reply, read; `None` without one.
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

/// The `xdg_toplevel` requests a client's wire log (`WAYLAND_DEBUG=client`)
/// shows for the frame's gestures, by name, once each, in the order they
/// first came: `move`, `resize(<edge>)` (xdg-shell's edge, by name),
/// `show_window_menu`, `set_maximized`, `unset_maximized`, `set_minimized`.
/// Serials and positions are left out: they follow the seat and the timing.
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

/// The cells one header button's slot takes at `scale` on `cell_w`-px cells:
/// aterm-gui's rule (`client_frame::CaptionSlot::measure`), 34 logical px
/// rounded up to whole cells; the lab reads a window the system decorates
/// through the same slots, so its negative control is the same pixels.
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

/// How a header slot `[x0, x1)` × `[0, y1)` of frame `f` reads: `drawn` when
/// more than a tenth of its pixels differ from the slot's top-left one (the
/// bare band above the button row), else `bare`; and the count.
fn slot_class(f: &Frame, x0: usize, x1: usize, y1: usize) -> (&'static str, usize) {
    let (x1, y1) = (x1.min(f.w), y1.min(f.h));
    if x0 >= x1 || y1 == 0 {
        return ("bare", 0);
    }
    let px = |x: usize, y: usize| {
        let i = (y * f.w + x) * 4;
        f.rgba.get(i..i + 4)
    };
    let ground = px(x0, 0);
    let mut differ = 0;
    for y in 0..y1 {
        for x in x0..x1 {
            if px(x, y) != ground {
                differ += 1;
            }
        }
    }
    let class = if differ * 10 > (x1 - x0) * y1 {
        "drawn"
    } else {
        "bare"
    };
    (class, differ)
}

/// What is typed after the screen's last prompt (`lane$ `).
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

/// A line's class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// `=`: compared exactly.
    Behaviour,
    /// `~`: shown when it moves.
    Measured,
    /// `@`: the stack.
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

    fn parse(c: char) -> Option<Class> {
        match c {
            '=' => Some(Class::Behaviour),
            '~' => Some(Class::Measured),
            '@' => Some(Class::Stack),
            _ => None,
        }
    }
}

/// One line of a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub class: Class,
    pub key: String,
    pub value: String,
}

/// A recording: a title, comments, and lines with unique keys, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recording {
    pub comments: Vec<String>,
    pub lines: Vec<Line>,
}

impl Recording {
    /// Add a line. A key already present gets `.<n>` appended, so every key
    /// stays unique and the order of repeats is kept.
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

    /// An item this session does not drive: `= not-driven.<item> <token>`,
    /// and `why` in a comment, which no replay compares.
    pub fn not_driven(&mut self, item: &str, token: &str, why: &str) {
        self.behaviour(&format!("not-driven.{item}"), token);
        self.comments
            .push(format!("not-driven.{item} {token}: {}", one_line(why)));
    }

    /// The text committed: the comments, then one line each.
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

    /// Read a committed recording. A line that is neither a comment nor
    /// `<class> <key> <value>` is an error, never skipped.
    ///
    /// # Errors
    /// The line, numbered from 1.
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

/// A value on one line: its newlines and carriage returns shown as `\n` and
/// `\r`. Nothing else is escaped, so a value lane-bus already escaped reads
/// as it printed it.
fn one_line(s: &str) -> String {
    s.trim_end().replace('\n', "\\n").replace('\r', "\\r")
}

/// What moved between a committed recording and a fresh one, by class: each
/// entry the key, the committed value and the fresh one (`None`: absent).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub behaviour: Vec<(String, Option<String>, Option<String>)>,
    pub measured: Vec<(String, Option<String>, Option<String>)>,
    pub stack: Vec<(String, Option<String>, Option<String>)>,
}

impl Diff {
    #[must_use]
    pub fn same_behaviour(&self) -> bool {
        self.behaviour.is_empty()
    }
}

/// Compare a fresh recording with the committed one. A key's class is the
/// committed line's when both have it; a line only one side has is judged
/// by its own class, so a behaviour line that appears or vanishes fails.
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

/// The diff, as the report prints it: behaviour first, every line; the
/// stack's and the measured lines' after, the measured ones capped.
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

/// What `baseline` was asked.
#[derive(Debug, PartialEq, Eq)]
pub struct BaselineArgs {
    pub arches: Vec<Arch>,
    pub screens: Vec<Screen>,
    pub record: bool,
    /// aterm-gui built with aterm-gpu's `wgpu-oracle`, so its windows draw
    /// on wgpu (Vulkan on the lane's lavapipe), the Linux renderer before
    /// the GL arm: GW.2's other arm, replayed against the shipped
    /// renderer's recordings and never recorded.
    pub wgpu_oracle: bool,
    pub keep_build: bool,
    pub keep_up: bool,
}

/// `baseline [--arch A]... [--screen S]... [--record | --wgpu-oracle]
/// [--keep-build] [--keep-up]`: every arch and screen when none is named.
///
/// # Errors
/// A usage error.
pub fn parse_baseline(args: &[String]) -> Result<BaselineArgs, Fail> {
    let mut b = BaselineArgs {
        arches: Vec::new(),
        screens: Vec::new(),
        record: false,
        wgpu_oracle: false,
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
            "--wgpu-oracle" => b.wgpu_oracle = true,
            "--keep-build" => b.keep_build = true,
            "--keep-up" => b.keep_up = true,
            other => {
                return Err(Fail::usage(format!(
                    "linux baseline: unknown argument '{other}' ({LAB} linux help)"
                )));
            }
        }
    }
    if b.record && b.wgpu_oracle {
        return Err(Fail::usage(
            "linux baseline: --wgpu-oracle replays only: the recordings are the shipped \
             renderer's"
                .to_string(),
        ));
    }
    if b.arches.is_empty() {
        b.arches = Arch::ALL.to_vec();
    }
    if b.screens.is_empty() {
        b.screens = Screen::BASELINE.to_vec();
    }
    Ok(b)
}

/// The committed recording's file name.
#[must_use]
pub fn file_name(arch: Arch, screen: Screen) -> String {
    format!("{}-{}.txt", arch.name(), screen.name())
}

/// The `dims` fields a recording keeps, in this order: every one that says
/// how the grid sits in the window (the head and padding a Wayland CSD
/// header moves, the bands and crop a resize leaves), none that is a count
/// of viewers or a present retry.
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

/// A `dims` reply as a recording keeps it: `<rows>x<cols>` and the fields
/// that say how the grid sits in the window.
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

/// The fields of a `resizes` ledger line that are behaviour: which resize,
/// from what grid to what, from where (`site=`), on which screen, and the
/// window's grid. How its rows moved (`trimmed=`, `appended=`, `restored=`,
/// `shift=`, …) and which run it joined (`run=`) follow what the shell had
/// drawn when it landed: the flap's halves straddle bash's SIGWINCH redraw or
/// not, which decides whether they join one run and whether the undo
/// restores the quiet flap (aterm-gui's resize_ledger.rs, `joins_run`).
const RESIZE_BEHAVIOUR: [&str; 8] = ["ord", "from", "to", "alt", "sync", "site", "win", "chrome"];

/// A `resizes` ledger line as a recording keeps it: its behaviour fields,
/// and everything but the time (`t=`) and the source line that booked it
/// (`at=`, which moves with every edit of the file) as a measured line.
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

/// The fields of the `resizes` summary that are behaviour: how many resizes,
/// risks and lost or unledgered ones, and the render verdict. How they fell
/// into runs and flaps is timing, as for each resize.
const SUMMARY_BEHAVIOUR: [&str; 5] = ["total", "risks", "render", "unledgered", "lost"];

/// Keep the `key=value` words of `line` whose key is in `keys`, in order.
fn keep_fields(line: &str, keys: &[&str]) -> String {
    line.split_whitespace()
        .filter(|w| w.split_once('=').is_some_and(|(k, _)| keys.contains(&k)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` as an `await match` pattern that matches it literally: the regex
/// metacharacters escaped, and each space as `\s`, since aterm-gui splits
/// the verb's words on whitespace (a space would end the pattern there, and
/// `echo X-x` would wait for `echo`, which an earlier row already shows).
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

/// The fields of a `cast drift` reply that are its verdict: what it found
/// and against what. How many runs and candidates it replayed follows how
/// the resizes fell into runs, which is timing (measured: two runs of the
/// same script gave `runs=1` and `runs=2`).
const DRIFT_VERDICT: [&str; 6] = [
    "verdict",
    "fidelity",
    "against",
    "drift_rows",
    "culprit",
    "geom",
];

/// A `cast drift` reply's verdict, and the rest of its first line without
/// the fields that move with timing (`anchor=`, `ms=`).
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

/// One AT-SPI event as `lane-bus atspi-listen` writes it
/// (`<ms> <pid> <Iface.Member> <path> <body>`): the process that emitted it,
/// and the event without its time.
#[must_use]
pub fn event_parts(line: &str) -> Option<(&str, &str)> {
    let (_, rest) = line.split_once(' ')?;
    rest.split_once(' ')
}

/// The event without its time and process.
#[must_use]
pub fn event_body(line: &str) -> &str {
    event_parts(line).map_or(line, |(_, body)| body)
}

/// The kind of an event, by the role its path has in the walks
/// (`roles`: path → role): `<Iface.Member>:<detail>@<role>`, and for a
/// state change the new value too.
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

/// A walk's node line split into its path and the rest (the path is
/// the publisher's numbering, not behaviour a reader sees).
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

/// The role a node line names.
fn node_role(line: &str) -> Option<&str> {
    proof::field(line, "role")
}

/// The value of ` key="…"` in `line`, unescaped as lane-bus quotes a string
/// (`\"`, `\\`, `\n`, `\t`, `\r`, `\u{…}`).
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

/// Whether the event stream's text changes rebuild the end walk's text, and
/// whether its last caret event names the end walk's caret. Undone in
/// reverse from the end walk's text, each insert must find its own text at
/// its offset and each delete must fit; that holds however the changes were
/// batched, so the line is behaviour, where the stream itself is measured.
/// `events` are this aterm's, in order; `roles` maps paths to roles.
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
    // A caret event comes only while the terminal has focus (none at all
    // under weston, none after Mutter's keyboard goes), so a caret moved
    // since is behaviour too; where it moved to follows the key repeat's
    // count, which is timing.
    let caret = match (last_caret.as_deref(), end_caret) {
        (None, _) => "no caret event",
        (Some(a), Some(b)) if a == b => "the last caret event names the end walk's caret",
        (Some(_), _) => "the end walk's caret is not the last caret event's",
    };
    format!("{rebuilt}; {caret}")
}

/// How many times `ch` follows the prompt on the screen's last prompt row:
/// the characters a held key typed.
#[must_use]
pub fn typed_after_prompt(screen: &str, ch: char) -> usize {
    screen
        .lines()
        .rev()
        .find_map(|l| l.split_once("$ ").map(|(_, typed)| typed.to_string()))
        .map_or(0, |typed| typed.chars().filter(|c| *c == ch).count())
}

/// `xset q`'s repeat settings: `delay=<ms> rate=<hz>`.
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

/// wayland-info's keyboard repeat settings (`wl_keyboard.repeat_info`, which
/// it prints under a seat that has a keyboard): `delay=<ms> rate=<hz>`.
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

/// wayland-info's globals: `name@version`, sorted.
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

/// `setxkbmap -query`'s layout and variant: `us intl`.
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

/// xdpyinfo's extension names, sorted.
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
                // `NAME  (opcode: N, …)` with -queryExtensions: the name.
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

/// The crates the lock resolves for the windowing and accessibility stack,
/// `name version` (with `(path)` for a first-party path crate), one per
/// version, sorted. Read from the checkout's `Cargo.lock`. The first-party
/// crates that replaced the registry stack (LW.6, LW.7) are named too, so a
/// recording's stack says what took the registry crates' place.
#[must_use]
pub fn lock_stack(lock: &str) -> Vec<(String, String)> {
    const NAMES: [&str; 31] = [
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
        "wgpu",
        "wgpu-hal",
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

/// The guest packages a recording names for `arch`: the servers and the
/// session's own, then the libraries aterm-gui loads, in `arch`'s build.
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

/// One session's driver: an aterm-gui under `screen`, its probes, and the
/// recording it fills.
struct Session<'a> {
    gui: Gui<'a>,
    arch: Arch,
    screen: Screen,
    /// lane-bus in the guest.
    bus: String,
    /// Where the run's report and fresh recordings go on this Mac (frames
    /// too).
    results: PathBuf,
    rec: Recording,
    /// path → role, from the walks.
    roles: Vec<(String, String)>,
    /// The terminal's text and caret in the end walk.
    end_terminal: Option<(String, Option<String>)>,
    /// Where each scripted item began on aterm's metrics clock
    /// (`metrics_now_ms`), for naming the item the slowest present ended in.
    marks: Vec<(&'static str, f64)>,
}

/// A wait that must end: a one-minute hang detector, never a budget.
const HANG: Duration = Duration::from_secs(60);

/// A `lane-bus seat` running as a lane unit ([`Session::seat_unit`]), and
/// its markers in the guest.
struct SeatUnit {
    name: String,
    ready: String,
    done: String,
    stop: String,
}

impl SeatUnit {
    /// Wait (a hang detector) until the seat's steps have gone.
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

    /// Let the seat go: its stop marker, then the unit's end (a hang
    /// detector), then a stop in case it never came.
    fn stop(&self, s: &Session<'_>) {
        let _ = s.sh(&["touch".into(), self.stop.clone()]);
        let _ = s.sh(&guest::wait_unit_gone(&self.name, 60));
        s.stop_unit(&self.name);
    }
}

impl Session<'_> {
    fn up(&self) -> &Up {
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

    /// A control verb's reply (trailing whitespace dropped), or why not.
    fn ctl(&self, verb: &[&str]) -> Result<String, String> {
        let out = self.gui.ctl(verb);
        let reply = proof::reply(&out.text).to_string();
        if out.ok() {
            Ok(reply)
        } else {
            Err(format!(
                "`{}` answered {}",
                verb.join(" "),
                crate::util::one_line(&reply, 160)
            ))
        }
    }

    /// Run `argv` in the guest.
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

    /// A lane unit's main pid.
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

    /// aterm-gui's main pid: the AT-SPI application to read, the X window.
    fn pid(&self) -> Option<String> {
        self.unit_pid(&Gui::unit_name())
    }

    /// aterm-gui's X window, by its pid, once it is viewable (a hang
    /// detector). `ready` reads the shell, not the window, and X refuses the
    /// focus to a window not yet mapped (`BadMatch` from `SetInputFocus`):
    /// with the guest's CPUs busy, a fresh aterm-gui's window was found and
    /// focused before it was mapped, the focus failed, and the keys typed
    /// next reached no window (measured, FLAKES).
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

    /// Give the X window `win` the keyboard focus, or say why not: a key
    /// typed without it reaches no window.
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

    /// Wait for the screen to stop changing: two equal `text` reads 300 ms
    /// apart, within the hang detector.
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

    /// `dims` once it stops moving: the same reply twice, 600 ms apart,
    /// within the hang detector.
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

    /// Poll `dims` until `done` holds, within the hang detector.
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

    /// Type a line through the control socket and wait for `want`.
    fn run_line(&self, typed: &str, want: &str) -> Result<(), String> {
        self.ctl(&["send", typed])?;
        self.ctl(&["key", "enter"])?;
        let r = self.ctl(&["await", "match", want, "timeout=60000"])?;
        if r.starts_with("OK match") {
            Ok(())
        } else {
            Err(format!("`{want}` never appeared: {r}"))
        }
    }

    /// The AT-SPI tree, as `atspi.walk.<phase>.<n>` lines.
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
        let out = self.sh(&argv);
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
        // A one-row flap, its halves back to back in one guest command (two
        // round trips from this Mac would land a variable 100-300 ms apart,
        // either side of the ledger's run gap): the case the resize undo
        // exists for.
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

    /// A resize the window system makes (X11: the window's size set from
    /// outside, as a window manager would), then the size put back.
    fn os_resize(&mut self) {
        let Some(display) = self.x_display() else {
            self.rec.not_driven(
                "resize.os",
                "no-resize-tool",
                "no headless Wayland compositor here resizes a client from outside (no window manager tooling)",
            );
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
                // A flap is two resizes in one run: timing, as below.
                self.rec
                    .measured("status.flaps", proof::field(&s, "flaps").unwrap_or("?"));
            }
            Err(e) => self.rec.behaviour("status", format!("failed: {e}")),
        }
        // The ledger: aterm-ctl prints its summary line on stderr, after.
        let out = self.gui.ctl(&["resizes"]);
        for line in out.text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("resize ") {
                let n = rest.split_whitespace().next().unwrap_or("?").to_string();
                let (behaviour, measured) = resize_line(rest);
                self.rec.behaviour(&format!("resizes.{n}"), behaviour);
                self.rec.measured(&format!("resizes.{n}.rows"), measured);
            } else if let Some(rest) = line.strip_prefix("run ") {
                // How resizes group into runs is timing; each resize is kept above.
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

    /// The echo row's pixel centre in X window coordinates, and the x where a
    /// drag across its text ends.
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
        // CLIPBOARD out: the echo row selected and copied, read by xclip.
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
        // CLIPBOARD in: xclip owns it; aterm's Paste.
        let r = self.paste_from_x(display, Selection::Clipboard, "XCLIP-42", &|s| {
            s.ctl(&["invoke", "unread=ok", "Paste"]).map(drop)
        });
        self.rec.behaviour("clipboard.in", r);
        // PRIMARY out: a real drag across the echo row (copy-on-select).
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
            // aterm takes PRIMARY once it has handled the button release: wait
            // for the echo there (a hang detector), then read what is there,
            // which a drag that set nothing leaves as before.
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
        // PRIMARY in: xclip owns it; a real middle click over the grid.
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

    /// Wait (a hang detector) until a row matches the regex `pattern`.
    /// aterm-ctl exits non-zero on `OK timeout`, so the reply decides.
    fn matched(&self, pattern: &str) -> bool {
        let r = self.gui.ctl(&["await", "match", pattern, "timeout=60000"]);
        r.text.trim_start().starts_with("OK match")
    }

    /// Clear what is typed at the prompt, if anything, and wait (a hang
    /// detector) for the bare prompt. An empty line is left alone: readline
    /// rings the bell for Ctrl+U on one, and aterm's visual bell inverts the
    /// frames that follow.
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

    /// Wait (a hang detector) for `typed` on the screen: the line being
    /// typed shows what a paste or a key put there, before the rest is sent.
    fn typed(&self, typed: &str) -> Result<(), String> {
        if self.matched(&regex_literal(typed)) {
            Ok(())
        } else {
            Err(format!("`{typed}` never reached the typed line (60 s)"))
        }
    }

    /// Wait (a hang detector) for `want` on the screen.
    fn appeared(&self, want: &str) -> Result<(), String> {
        if self.matched(&regex_literal(want)) {
            Ok(())
        } else {
            Err(format!("{want} never appeared (60 s)"))
        }
    }

    /// xclip owns `sel` with `text`; `paste` pastes it after `echo X-`, and
    /// once the typed line shows it, ` | tr A-Z a-z` follows; the lowered
    /// echo must appear.
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

    /// `wl-paste` (of PRIMARY with `primary`) until it reads `text` (a hang
    /// detector), then once more: what it read, or why not.
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

    fn mutter_selections(&mut self, wayland: &str) {
        // CLIPBOARD out: the echo row selected, copied by Ctrl+Shift+C on the
        // virtual keyboard, read by wl-paste while the keyboard lives (a
        // client without a keyboard cannot read a selection on Mutter).
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
        // CLIPBOARD in: wl-copy (a lane unit, serving in the foreground) takes
        // it while a keyboard lives; then a second keyboard, once it has the
        // window's focus again, pastes with Ctrl+Shift+V.
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
        self.mutter_primary(wayland);
    }

    /// PRIMARY under Mutter, from the RemoteDesktop pointer (LW.7 recorded it
    /// on the sctk backend before switching): out, a real drag across the
    /// echo row (copy-on-select) read by `wl-paste --primary`; in, `wl-copy
    /// --primary`'s text pasted by a real middle click over the grid. The
    /// seat moves only relatively, so each aim is made from where the window
    /// reads its pointer (`pointer status`), as the header gestures aim.
    fn mutter_primary(&mut self, wayland: &str) {
        let out = self.mutter_primary_out(wayland);
        self.rec.behaviour(
            "primary.out",
            out.unwrap_or_else(|e| format!("failed: {e}")),
        );
        let text = "WLPRIMARY-42";
        let want = "x-wlprimary-42";
        let r = self.mutter_primary_in(wayland, text, want);
        self.stop_unit(&format!("{}wlcopy", guest::UNIT_PREFIX));
        self.rec.behaviour(
            "primary.in",
            match r {
                Ok(()) => format!("wl-copy --primary's {text} pasted by a middle click: {want}"),
                Err(e) => format!("failed: {e}"),
            },
        );
    }

    /// The window pixels of the echo row's first and last cells (their
    /// left and right edges, 2 px in, at mid-row), from `dims`.
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

    /// The window's scale (`dims`), 1 when it says none.
    fn dims_scale(&self) -> f64 {
        self.ctl(&["dims"])
            .ok()
            .and_then(|d| proof::field(&d, "scale").and_then(|v| v.parse::<f64>().ok()))
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0)
    }

    fn mutter_primary_out(&mut self, wayland: &str) -> Result<String, String> {
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
            // From the top-left corner, where `lane-bus seat` parked its
            // pointer before the window took the keyboard, onto the window.
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
            // aterm takes PRIMARY once it has handled the release: read it
            // while the seat's keyboard lives (a client without a keyboard
            // cannot read a selection on Mutter).
            let read = self.wl_paste_until(wayland, proof::ECHO, true)?;
            Ok(format!("drag; wl-paste --primary read {read:?}"))
        })();
        let done = if r.is_ok() { seat.done(self) } else { Ok(()) };
        seat.stop(self);
        let out = r?;
        done?;
        Ok(out)
    }

    fn mutter_primary_in(&mut self, wayland: &str, text: &str, want: &str) -> Result<(), String> {
        // wl-copy takes PRIMARY while a keyboard lives.
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
        // A middle click over the grid: the pointer centred, then aimed at
        // the grid's middle from where the window reads it.
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

    /// `lane-bus seat <steps>` as a lane unit, so a keyboard lives while
    /// another client acts: it touches `ready` once its keyboard has the
    /// window's focus, runs `steps`, touches `done`, and lets go when
    /// [`SeatUnit::stop`] says so. Returned once `ready` is there (a hang
    /// detector).
    fn seat_unit(&self, steps: &[&str]) -> Result<SeatUnit, String> {
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
        let mut argv = vec![
            self.bus.clone(),
            "seat".to_string(),
            self.unit_pid(&format!("{}mutter", guest::UNIT_PREFIX))
                .unwrap_or_else(|| "0".into()),
            APP.into(),
            self.pid().unwrap_or_else(|| "0".into()),
            format!("touch:{}", seat.ready),
        ];
        argv.extend(steps.iter().map(|s| (*s).to_string()));
        argv.push(format!("touch:{}", seat.done));
        argv.push(format!("wait:{}", seat.stop));
        let unit = guest::Unit {
            name: seat.name.clone(),
            env: vec![(
                "DBUS_SESSION_BUS_ADDRESS".into(),
                format!("unix:path={}/bus", self.up().runtime),
            )],
            argv,
            socket: String::new(),
        };
        if !self.sh(&guest::run_unit(&unit, None)).ok() {
            return Err("lane-bus seat did not start".into());
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

    /// The last lines a lane unit logged, on one line.
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

    fn key_repeat(&mut self) {
        let held = match (self.x_display(), self.screen.display) {
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
            (None, Display::Mutter) => {
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
                    "weston's headless backend advertises no wl_seat, so no key reaches a client (weston's x11 backend, nested in Xvfb, has a seat XTEST could drive: not tried)",
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
        // Under Mutter the keyboard's repeat settings exist only while a
        // keyboard does: read them now, before it goes.
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

    /// The IME events aterm has counted (`metrics percentiles`' `n_ime`).
    fn ime_events(&self) -> Option<u64> {
        self.ctl(&["metrics", "percentiles"])
            .ok()
            .and_then(|m| proof::field(&m, "n_ime").and_then(|v| v.parse().ok()))
    }

    /// A plain key, then a dead key and a letter, typed through the X
    /// `display` between `echo X-` and `-Y`: what the shell printed, and
    /// whether aterm counted IME events for it. The plain key must reach the
    /// screen before the dead key goes, so the whole input path (an XIM
    /// server's input context included) is live when it does: typed at once
    /// after the window took focus, the dead key was sometimes lost under
    /// ibus (measured). Through an XIM `server` the letter is held down
    /// until the composed letter is on the line ([`Self::held_letter`]).
    fn composed(&self, display: &str, server: bool) -> String {
        let before = self.ime_events();
        let r = (|| -> Result<String, String> {
            self.clear_line();
            self.ctl(&["send", COMPOSE_BEFORE])?;
            let plain = self.x(display, &["xdotool", "key", "x"]);
            if !plain.ok() {
                return Err(format!("the plain key: {}", plain.tail(2)));
            }
            self.typed(COMPOSE_LIVE)
                .map_err(|_| format!("a plain key never reached the screen ({COMPOSE_LIVE})"))?;
            // The keys go through the window system; the typed suffix must
            // follow them, so the composed letter is awaited on the typed
            // line first. Keys that compose nothing pass the hang detector,
            // and the line then shows what they did.
            let composed = format!("{COMPOSE_LIVE}\u{e9}");
            if server {
                self.held_letter(display, &composed)?;
            } else {
                let typed = self.x(display, &["xdotool", "key", "dead_acute", "e"]);
                if !typed.ok() {
                    return Err(format!("the keys: {}", typed.tail(2)));
                }
                let _ = self.typed(&composed);
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

    /// `dead_acute`, then `e` pressed through the XIM-served X `display` and
    /// let go only once `composed` is on the typed line (a hang detector),
    /// with the server's autorepeat off while it is held, so how long that
    /// takes changes nothing.
    ///
    /// libX11 hands its client each event the input method sends back (a key
    /// the method did not use, or a commit's carrier: a KeyPress with keycode
    /// 0) with `XPutBackEvent`, at the head of the queue, and marks it with
    /// one flag per input method, which the next key the client filters
    /// takes (`MARK_FABRICATED`, libX11 1.8.7's `modules/im/ximcp`). A
    /// synchronous XIM request dispatches whatever else it reads while it
    /// waits for its reply (`_XimRead`; aterm makes them, `XSetICValues` for
    /// the spot among them), so a second such event can be put back before
    /// aterm has read the first, and ahead of it. Typed as one burst, the
    /// letter's release came back right behind the commit and took the
    /// flag; aterm filtered the carrier as a new key, ibus swallowed it, and
    /// the shell printed `X-x-Y` (measured with aterm-gui stopped across the
    /// keys, and with the guest's CPUs busy). Held, the carrier has been read
    /// before anything can come back after it; what came back before the
    /// commit (the dead key's release) is put back behind it.
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

    /// Compose: `dead_acute` then `e` through the window system, with no
    /// input-method server: Xlib's own compose on X11. (On Wayland the winit
    /// fork composes with xkbcommon, which LW.7's aterm-xkb takes over, but no
    /// headless keyboard here can type a dead key.)
    fn compose(&mut self) {
        let r = match (self.x_display(), self.screen.display) {
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
                self.composed(&display, false)
            }
            (None, Display::Mutter) => {
                self.rec.not_driven(
                    "compose",
                    "no-keycode",
                    "a bare Mutter's keymap is plain us, with no dead key (GNOME Shell sets the keymap on a desktop): RemoteDesktop finds no keycode for dead_acute (measured: \"No keycode found for keyval fe51\")",
                );
                return;
            }
            (None, _) => {
                self.rec.not_driven(
                    "compose",
                    "no-seat",
                    "weston's headless backend advertises no wl_seat, so no key reaches a client (weston's x11 backend, nested in Xvfb, has a seat XTEST could drive: not tried)",
                );
                return;
            }
        };
        self.rec.behaviour("compose", r);
    }

    /// A frame of the window (`image`), copied to this Mac beside the report
    /// and decoded: its pixels, or why not.
    fn frame(&self, name: &str) -> Result<Frame, String> {
        // `plain`: without the Scene's animated sprites and the cursor's
        // trail, which move from frame to frame.
        let reply = self.ctl(&["image", "plain", name])?;
        let guest_path = reply
            .split_whitespace()
            .nth(3)
            .ok_or_else(|| format!("`image` answered {reply}"))?;
        let file = format!("{}-{}-{name}", self.arch.name(), self.screen.name());
        let to = self.results.join(&file);
        let copied = self.up().lima.copy_out(guest_path, &to);
        if !copied.ok() {
            return Err(format!("{guest_path} not copied out: {}", copied.tail(2)));
        }
        let bytes = std::fs::read(&to).map_err(|e| format!("{}: {e}", to.display()))?;
        aterm_png::decode_rgba8(&bytes, 8192)
            .map(|(rgba, w, h)| Frame { rgba, w, h })
            .ok_or_else(|| format!("{}: not a PNG this lab decodes", to.display()))
    }

    /// XDND (X11): `lane-bus xdnd`, a second X client acting as the drag
    /// source, drags a file over aterm's window and away, then over it and
    /// drops it. What the target answered (`XdndAware`, each `XdndStatus`,
    /// the data it asked for, `XdndFinished`), the drop highlight read from
    /// frames taken at rest, while hovered and after the leave, and what the
    /// drop typed at the prompt.
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
            // A clean prompt, at rest.
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

    /// The drag source's report, a behaviour line per answer.
    fn record_dnd_report(&mut self, report: &str) {
        let out = self.sh(&["cat".into(), report.into()]);
        for line in out.text.lines() {
            if let Some((key, value)) = line.split_once(' ') {
                self.rec.behaviour(&format!("dnd.{key}"), value);
            }
        }
    }

    /// XIM (X11): a second aterm-gui started with `XMODIFIERS=@im=ibus` while
    /// ibus-daemon serves XIM with its `xkb:us::eng` engine (ibus's simple
    /// engine, which composes dead keys), then the same dead key and letter,
    /// the letter held down until it composed ([`Self::held_letter`]).
    /// The session's own aterm has stopped by now. ibus reads its settings
    /// from a scratch keyfile ([`guest::IBUS_KEYFILE`]) that keeps it off the
    /// X keymap: picking the engine would otherwise set the keymap to plain
    /// `us`, where xdotool types `dead_acute` by binding a spare keycode
    /// around each half of the key, and ibus, which reads the key's symbol
    /// when its XIM server handles it, read keyval 0 once the binding had gone
    /// and swallowed the key (measured: `X-xe-Y`, seen on x86_64 under
    /// Rosetta, where aterm hands the key on later).
    fn xim(&mut self) {
        let Some(display) = self.x_display() else {
            return;
        };
        let keyfile = format!("{}/glib-2.0/settings/keyfile", guest::IBUS_CONFIG);
        let config = format!("XDG_CONFIG_HOME={}", guest::IBUS_CONFIG);
        let ibus = guest::Unit {
            name: format!("{}ibus", guest::UNIT_PREFIX),
            env: vec![
                ("DISPLAY".into(), display.clone()),
                ("GSETTINGS_BACKEND".into(), "keyfile".into()),
                ("XDG_CONFIG_HOME".into(), guest::IBUS_CONFIG.into()),
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
        let r = (|| -> Result<String, String> {
            let fresh = self.sh(&[
                "sh".into(),
                "-c".into(),
                "rm -rf \"$1\" && install -d \"${2%/*}\" && printf '%s' \"$3\" > \"$2\"".into(),
                "ibus-config".into(),
                guest::IBUS_CONFIG.into(),
                keyfile.clone(),
                guest::IBUS_KEYFILE.into(),
            ]);
            if !fresh.ok() {
                return Err(format!("{keyfile}: {}", fresh.tail(2)));
            }
            if !self.sh(&guest::run_unit(&ibus, None)).ok() {
                return Err("ibus-daemon did not start".into());
            }
            // The engine can be set once the daemon answers; XIM serves once
            // the root window names it. Hang detectors, both.
            // ibus finds its daemon by a file named after the display, under
            // the daemon's XDG_CONFIG_HOME.
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
            Ok(self.composed(&display, true))
        })();
        self.gui.stop();
        self.stop_unit(&ibus.name);
        self.rec.behaviour(
            "xim",
            match r {
                Ok(what) => format!("ibus XIM, engine {XIM_ENGINE}: {what}"),
                Err(e) => format!("failed: {e}"),
            },
        );
    }

    /// text-input-v3 (Mutter): a second aterm-gui, its Wayland wire logged
    /// (`WAYLAND_DEBUG=client`, stderr to a file), given a keyboard's focus
    /// and then out of it by `lane-bus seat`. No input method composes under
    /// a bare Mutter, so this is the client's half: what it binds, and what
    /// it asks and hears of text-input-v3 as focus comes and goes. The
    /// session's own aterm has stopped by now.
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
            // The client places the input method's cursor rectangle once it
            // has redrawn with focus, a few ms after `enable`: the keyboard
            // stays until the log shows it (a hang detector; a client that
            // never sends one leaves after it, and its line says so).
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
            // The seat ends once the window has lost its focus again.
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

    /// The front window's `strip-caption` (LW.5), or why not.
    fn caption(&self) -> Result<Caption, String> {
        let chrome = self.ctl(&["chrome"])?;
        parse_caption(&chrome).ok_or_else(|| {
            format!(
                "`chrome` has no strip-caption line: {}",
                crate::util::one_line(&chrome, 200)
            )
        })
    }

    /// Poll `chrome` until the frame says `done`, within the hang detector.
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

    /// Click the header button `name` over the control socket: the pointer on
    /// its middle column of the strip (the chrome row that IS the strip: a
    /// message band may sit between it and the grid), then
    /// `pointer click`, which runs what a press and release run.
    fn click_button(&self, caption: &Caption, name: &str) -> Result<(), String> {
        let col = caption
            .middle(name)
            .ok_or_else(|| format!("no {name} button (buttons={})", caption.names()))?
            .to_string();
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

    /// LW.5: the window's own frame, where the band is the header. On X11 (the
    /// window manager's frame: this lane runs none) and under a compositor that
    /// grants server-side decorations, `frame=system` and no buttons; under
    /// weston and Mutter, which leave a Wayland client to decorate itself, the
    /// band's buttons, their pixels (each slot holds a drawn button; read
    /// through the same slots, a system-framed window's band is bare there,
    /// which is the negative control), and maximize and restore driven
    /// through those buttons over the control socket.
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

    /// The header's three rightmost button slots, from one `image plain`: each
    /// `drawn` (a button) or `bare` (band), read the same way whatever frame
    /// the window has.
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
                let (class, n) = slot_class(&f, x(s), x(e), band as usize);
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

    /// Maximize through the header's maximize button, then restore through
    /// the button that takes its place: the grid each settles at, and the
    /// buttons the header shows while maximized.
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

    /// Write `text` to `path` in the guest.
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

    /// The window pixel `pointer status` reads back, or why not.
    fn pointer_px(&self) -> Result<(i32, i32), String> {
        let at = self.ctl(&["pointer", "status"])?;
        proof::field(&at, "px")
            .and_then(|p| p.split_once(','))
            .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))
            .ok_or_else(|| format!("the window holds no pointer ({at})"))
    }

    /// The pointer's window pixel once it stops moving: the same reading
    /// twice, 300 ms apart, within the hang detector (while a compositor grab
    /// holds the pointer the window holds none).
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

    /// Wait (a hang detector) until the window reads its pointer at `want`,
    /// give or take `slack` pixels; the reading, or where it was instead.
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

    /// LW.5 under Mutter: the frame's gestures from a real seat, which the
    /// control socket cannot make (a compositor move, resize or window menu
    /// takes a press's serial): a second aterm-gui with `WAYLAND_DEBUG=client`
    /// and a RemoteDesktop pointer, aimed from what the window reads back
    /// (`pointer status`'s `px=`). A press on the bare band dragged 40,30
    /// after the move starts (the window follows the pointer, so the pointer
    /// ends where it started on the window), a press on the right edge
    /// dragged 60 px right (the window widens), and a right press on the band
    /// (the window menu); with the `xdg_toplevel` requests the wire log shows.
    fn header_gestures(&mut self) {
        if self.screen.display != Display::Mutter {
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

    /// Wait (a hang detector) until the seat touches `<marks>/<name>`.
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

    /// [`Self::header_gestures`]'s session: its lines (`true` for behaviour),
    /// or why it could not run.
    fn header_gestures_run(&self) -> Result<Vec<(bool, String, String)>, String> {
        let log = format!("{}/wayland-debug-frame.txt", self.gui.dir);
        let marks = format!("{}/frame", self.gui.dir);
        let mark = |name: &str| format!("{marks}/{name}");
        // After the scratch world is made afresh (`prepare` empties the
        // session's directory), so the seat's markers have a place.
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
        let d = self.settled_dims()?;
        let num = |k: &str| proof::field(&d, k).and_then(|v| v.parse::<i32>().ok());
        let scale = proof::field(&d, "scale")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0);
        let (w, h) = (num("frame_w").unwrap_or(0), num("frame_h").unwrap_or(0));
        let band_y =
            num("pad_top").unwrap_or(0) + num("head").unwrap_or(0) + num("cell_h").unwrap_or(0) / 2;
        // Bare band: four cells left of the first button, right of a lone
        // tab and its `+`.
        let caption = self.caption()?;
        let (cw, pad) = (num("cell_w").unwrap_or(0), num("pad").unwrap_or(0));
        let first = caption.buttons.first().map_or(0, |&(_, s, _)| i32::from(s));
        let band_x = pad + (first - 4).max(0) * cw;
        // The seat moves in logical pixels and the window reads back device
        // pixels: one seat pixel is `scale` window pixels.
        let logical = |px: i32| (f64::from(px) / scale).round() as i32;
        let device = |lp: i32| (f64::from(lp) * scale).round() as i32;
        let slack = scale.ceil() as i32;
        let steps = [
            // From the top-left corner, where `lane-bus seat` parked its
            // pointer before the window took the keyboard, onto the window.
            "move:640,400".to_string(),
            format!("touch:{}", mark("centred")),
            format!("wait:{}", mark("aim-band")),
            format!("move:@{}", mark("to-band")),
            format!("touch:{}", mark("at-band")),
            format!("wait:{}", mark("drag-band")),
            "press:left".to_string(),
            "move:6,0".to_string(),
            "sleep:300".to_string(),
            "move:40,30".to_string(),
            "sleep:300".to_string(),
            "release:left".to_string(),
            format!("touch:{}", mark("moved")),
            format!("wait:{}", mark("aim-edge")),
            format!("move:@{}", mark("to-edge")),
            format!("touch:{}", mark("at-edge")),
            format!("wait:{}", mark("drag-edge")),
            "press:left".to_string(),
            "sleep:300".to_string(),
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
        // Aim the seat from `from` to `to` (window pixels): the motion in
        // seat pixels, and the window pixel it lands on.
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
            // Each aim is checked where the window reads it back before the
            // seat presses there: a motion reaches the window a moment after
            // RemoteDesktop takes it.
            let band = aim("to-band", (x, y), (band_x, band_y))?;
            go("aim-band");
            self.seat_reached(&seat, &marks, "at-band")?;
            let (bx, by) = self
                .px_at(band, slack)
                .map_err(|e| format!("on the band: {e}"))?;
            go("drag-band");
            self.seat_reached(&seat, &marks, "moved")?;
            let (mx, my) = self
                .settled_px()
                .map_err(|e| format!("after the move: {e}"))?;
            // The window followed: the pointer ends on the window where the
            // press was (Mutter anchors the move at the press), give or take
            // the 6 px that started it; had the window stayed, 46,30 off.
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
            // The right edge's handle: 2 px in, at mid-height.
            let edge = aim("to-edge", (mx, my), (w - 2, h / 2))?;
            go("aim-edge");
            self.seat_reached(&seat, &marks, "at-edge")?;
            self.px_at(edge, slack)
                .map_err(|e| format!("on the edge: {e}"))?;
            go("drag-edge");
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
            // Where the pointer is now, in window pixels: the right edge's
            // handle plus the 60 seat pixels it was dragged (the window's left
            // edge did not move). The window snaps its width to whole cells, so
            // the pointer may be just outside it, and the window then reads none.
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
        // The wire log is kept beside the report whatever happened: it is
        // what says where a gesture went.
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

    /// What a later step can compare against for input it could not drive.
    fn not_driven(&mut self) {
        match self.screen.display {
            Display::Mutter => {
                self.rec.not_driven(
                    "text-input-v3.compose",
                    "no-im-bridge",
                    "the client's half of text-input-v3 is recorded (text-input-v3.client), but no input method composes through it: one reaches Mutter's zwp_text_input_manager_v3 only through GNOME Shell's IBus bridge, which a bare headless Mutter does not run (by reading Mutter 46 and GNOME Shell, not a run), and its keymap has no dead key to compose",
                );
                self.rec.not_driven(
                    "dnd",
                    "no-pointer-motion",
                    "a Wayland drag needs a pointer that moves over the window with a button held, from a client with a seat serial; Mutter's RemoteDesktop pointer moves absolutely only within a screencast stream, which the lane does not open (not tried)",
                );
            }
            Display::Weston => {
                self.rec.not_driven(
                    "text-input-v3",
                    "v1-only",
                    "weston 13 offers zwp_text_input_manager_v1 only, which the winit fork does not bind",
                );
                self.rec.not_driven(
                    "header.gestures",
                    "no-seat",
                    "the frame's move, resize and window menu take a press's serial from a seat, and weston's headless backend has none (header.maximize is driven: maximize takes no serial)",
                );
                self.rec.not_driven(
                    "dnd",
                    "no-seat",
                    "weston's headless backend advertises no wl_seat, and a drag starts from a seat's pointer (weston's x11 backend, nested in Xvfb, has a seat XTEST could drive: not tried)",
                );
            }
            Display::Xvfb | Display::Xwayland => {
                self.rec.not_driven(
                    "header.gestures",
                    "system-frame",
                    "an X11 window's frame is the window manager's (this lane runs none), so aterm draws no header and its band is not caption",
                );
            }
        }
    }

    fn percentiles(&mut self, journal: &str) {
        let gpu = proof::renderer(journal).unwrap_or("?").to_string();
        self.rec.stack("gpu", &gpu);
        match self.ctl(&["metrics", "percentiles"]) {
            Ok(m) => {
                let get = |k: &str| proof::field(&m, k).unwrap_or("?").to_string();
                self.rec.measured(
                    "present.software",
                    format!("software rendering ({gpu}): {}", proof::present_summary(&m)),
                );
                self.rec.measured(
                    "input.software",
                    format!(
                        "n_input={} input_p50/p95/p99_ms={}/{}/{}",
                        get("n_input"),
                        get("input_p50_ms"),
                        get("input_p95_ms"),
                        get("input_p99_ms")
                    ),
                );
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

    /// Note where `item` begins on aterm's metrics clock; a reply without the
    /// clock leaves no mark, and the slowest present is then named against
    /// the marks there are.
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

/// Record one session: the recording, or why the session could not run.
fn record_session(
    up: &Up,
    arch: Arch,
    screen: Screen,
    bin: &str,
    bus: &str,
    lock: &str,
    results: &Path,
) -> Result<Recording, String> {
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
    };
    gui.prepare().map_err(|e| format!("scratch world: {e}"))?;
    linux::start_screen(up, screen)?;
    if let Some(step) = screen.baseline_keymap() {
        let out = up.lima.shell(&up.home, &step.argv, None);
        if !out.ok() {
            up.stop_units();
            return Err(format!("{}: {}", step.label, out.tail(3)));
        }
    }
    let mut s = Session {
        gui,
        arch,
        screen,
        bus: bus.to_string(),
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
    // The listener first, so it hears aterm's adapter from its first event.
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
    // lane-bus creates its output once its match rule is in place: from then
    // on the bus routes it every event, aterm's first included.
    if !up.sh(&guest::wait_file(&events, 60)).ok() {
        let why = s.unit_journal(&listener.name);
        up.stop_units();
        return Err(format!(
            "the AT-SPI listener never subscribed (60 s): {why}"
        ));
    }
    let invocation = s.gui.start(Some(screen.display), &[])?;
    // A session that never got going decided nothing: NOT RUN, with aterm's
    // journal tail and any crash report it wrote kept beside the report.
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
        // The window's first configures can still be landing at `ready` (an
        // output's scale arrives with the surface's enter): the grid it
        // settles at. Both startup re-grids the lane once booked, a scaled
        // weston's and Mutter's, are traced in
        // docs/measured/linux-gpu-rows-2026-10-19.md.
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
    match (s.x_display(), screen.display) {
        (Some(x), _) => s.x11_selections(&x),
        (None, Display::Mutter) => {
            let w = s.wayland_display().unwrap_or_default();
            s.mutter_selections(&w);
        }
        (None, _) => s.rec.not_driven(
            "clipboard",
            "no-seat",
            "weston's headless backend advertises no wl_seat, and a Wayland selection is set with a seat's serial (weston's x11 backend, nested in Xvfb, has a seat XTEST could drive: not tried)",
        ),
    }
    s.mark("key-repeat");
    s.key_repeat();
    s.mark("compose");
    s.compose();
    if let Some(x) = s.x_display() {
        s.mark("dnd");
        s.dnd(&x);
    }
    s.mark("header");
    s.header();
    s.not_driven();
    s.mark("walk.end");
    s.walk("end");
    let journal = s.gui.journal(&invocation);
    s.percentiles(&journal);
    let pid = s.pid().unwrap_or_default();
    // The event stream: the listener stopped, its file read.
    let _ = up.sh(&["touch".into(), stop]);
    let _ = up.sh(&guest::wait_unit_gone(&listener.name, 60));
    let stream = up.sh(&["cat".into(), events]);
    // This aterm's events only: the guest's bus may carry another lane's.
    let lines: Vec<&str> = stream
        .text
        .lines()
        .filter(|l| event_parts(l).is_some_and(|(p, _)| p == pid))
        .collect();
    // The frame's name follows the window title, which the shell sets to the
    // running command and back; whether a short command's title is ever
    // published follows frame timing (measured: present on x86_64, absent on
    // aarch64, for the same script). The end walk keeps the name it settles at.
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
    s.text_input_v3();
    s.header_gestures();
    up.stop_units();
    Ok(s.rec)
}

/// The session's slowest present, from a `metrics` summary: its
/// output-to-present latency, the scripted item it ended in (the last of
/// `marks` that began before it) and how far in, the gap since the present
/// before it, and the frame and main-loop maxima beside it, which say
/// whether the frame's own work or a wait made it slow.
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

/// What every baseline session of one lane run shares: lane-bus, built once
/// natively (a probe, not under test) while accessibility is on; the
/// checkout's lock, for the stack lines; where the recordings are committed;
/// where the fresh ones go; and whether this run records or replays.
pub struct Baseline {
    bus: String,
    lock: String,
    committed: PathBuf,
    results: PathBuf,
    record: bool,
    /// The sessions' aterm-gui draws on the wgpu oracle (`--wgpu-oracle`).
    pub wgpu_oracle: bool,
}

/// Set at-spi2-core's `org.a11y.Status.IsEnabled`: aterm's AT-SPI
/// publisher (aterm-a11y) publishes only while it is on.
fn accessibility(up: &Up, on: bool) -> Out {
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
    /// Build lane-bus, and see that at-spi2-core can turn accessibility on
    /// (it is off again until a session starts: the proof's own rows and the
    /// guest's other aterms run without it).
    ///
    /// # Errors
    /// The NOT RUN row that says which of the two failed.
    pub fn begin(up: &Up, results: &Path, record: bool) -> Result<Baseline, proof::Row> {
        let repo = linux::repo();
        let t0 = Instant::now();
        let not_run = |detail: String| proof::Row {
            name: "baseline: lane-bus and the accessibility bus".into(),
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
        let env = guest::cargo_env(Arch::Aarch64, &format!("{lane}/target"), false);
        println!("\n=== lane-bus (aarch64, the baseline's probe) ===");
        let built = up.lima.shell_streamed(
            &up.home,
            &guest::with_env(
                &env,
                &[
                    "cargo".into(),
                    "build".into(),
                    "--locked".into(),
                    "--manifest-path".into(),
                    format!("{repo}/Cargo.toml"),
                    "-p".into(),
                    "aterm-dbus".into(),
                    "--bin".into(),
                    "lane-bus".into(),
                ],
            ),
            "  | ",
        );
        if !built.ok() {
            return Err(not_run(format!(
                "lane-bus build: {}",
                proof::tally(&built.text)
                    .error
                    .unwrap_or_else(|| built.tail(3))
            )));
        }
        Ok(Baseline {
            bus: format!(
                "{}/lane-bus",
                Arch::Aarch64.bin_dir(&format!("{lane}/target"))
            ),
            lock,
            committed: PathBuf::from(&repo).join(DIR),
            results: results.to_path_buf(),
            record,
            wgpu_oracle: false,
        })
    }

    /// Turn accessibility off, whatever a session left.
    pub fn end(&self, up: &Up) {
        let _ = accessibility(up, false);
    }

    /// One session of `arch` under `screen`, from the aterm-gui and
    /// aterm-ctl built in `bin`: recorded, then replayed against the
    /// committed recording, or (`--record`) written over it. Its row.
    pub fn session(&self, up: &Up, arch: Arch, screen: Screen, bin: &str) -> proof::Row {
        let name = format!(
            "{} ({}) baseline {}{}",
            arch.name(),
            arch.runs(),
            screen.name(),
            if self.wgpu_oracle {
                " (wgpu oracle)"
            } else {
                ""
            }
        );
        println!("\n=== {name} ===");
        let started = Instant::now();
        // Accessibility is on for the session alone: aterm's AT-SPI
        // publisher publishes only while it is.
        let enabled = accessibility(up, true);
        let recorded = if enabled.ok() {
            record_session(up, arch, screen, bin, &self.bus, &self.lock, &self.results)
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

    /// Keep the fresh recording beside the report, then compare it with the
    /// committed one, or (`record`) write it over the committed one.
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

/// `linux baseline …`.
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_baseline(args: &[String]) -> Res {
    let ask = parse_baseline(args)?;
    crate::stop::install();
    let _lock = linux::LaneLock::take()?;
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
        Ok(mut baseline) => {
            baseline.wgpu_oracle = ask.wgpu_oracle;
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
                let env = guest::cargo_env(arch, &format!("{lane}/target"), false);
                let oracle = if ask.wgpu_oracle {
                    " (wgpu oracle)"
                } else {
                    ""
                };
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
                if ask.wgpu_oracle {
                    argv.extend(["--features".into(), "aterm-gpu/wgpu-oracle".into()]);
                }
                println!("\n=== {tag}: build aterm-gui{oracle}, aterm-ctl ===");
                let built = up
                    .lima
                    .shell_streamed(&up.home, &guest::with_env(&env, &argv), "  | ");
                if !built.ok() {
                    rows.push(proof::Row {
                        name: format!("{tag} build aterm-gui{oracle}"),
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
    if ask.keep_up {
        up.stop_units();
    } else {
        up.release();
    }
    let report = proof::render(&rows, &[], t0.elapsed().as_secs_f64()).replacen(
        "LINUX LANE PROOF",
        if ask.record {
            "LINUX LANE BASELINE (record)"
        } else if ask.wgpu_oracle {
            "LINUX LANE BASELINE (replay, aterm-gui on the wgpu oracle)"
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

    /// The replay's negative control: a behaviour line that moves, appears or
    /// vanishes fails it; a measured or stack line that moves never does.
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
        assert!(one.record && !one.wgpu_oracle);
        assert!(
            parse_baseline(&s(&["--wgpu-oracle"]))
                .expect("parses")
                .wgpu_oracle
        );
        assert_eq!(
            parse_baseline(&s(&["--wgpu-oracle", "--record"]))
                .map_err(|f| f.code)
                .err(),
            Some(2),
            "the oracle replays; the recordings stay the shipped renderer's"
        );
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
                "xwayland"
            ]
        );
        assert_eq!(
            file_name(Arch::Aarch64, Screen::BASELINE[5]),
            "aarch64-mutter-scale1.5.txt"
        );
    }

    /// The slowest present is named by the item it ended in: the last mark
    /// that began before it, however late in that item it came.
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

    /// Shapes measured in the guest on 2026-10-16.
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

    /// `await match` takes one word, a regex: text with a space or a
    /// metacharacter still matches only itself.
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

    /// The stream's text changes, undone from the end walk's text, and its
    /// last caret event; the negative controls: an insert whose text is not
    /// where it says, and a caret that moved after its last event.
    #[test]
    fn the_text_changes_rebuild_the_end_walks_text() {
        let roles = vec![("/t".to_string(), "terminal".to_string())];
        // From "lane$ \n": the echo typed, the blank row taken, its output
        // and the next prompt put in its place.
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

    /// A wire log's lines as libwayland prints them (measured under Mutter
    /// 46, 2026-10-16).
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

    /// The `strip-caption` line as aterm-gui's `chrome` prints it (LW.5).
    #[test]
    fn the_caption_line_reads_back_the_frame_and_its_buttons() {
        let chrome = "toolbar (none)\nstrip-caption frame=client buttons=minimize:68..72,maximize:72..76,close:76..80 hovered=none maximized=false edges=true menu=true\nmenu \"File\": New Tab";
        let c = parse_caption(chrome).expect("a caption line");
        assert!(c.client && c.edges && c.menu && !c.maximized);
        assert_eq!(c.names(), "minimize,maximize,close");
        assert_eq!(c.middle("maximize"), Some(74));
        assert_eq!(c.middle("restore"), None);
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

    /// A wire log's `xdg_toplevel` requests as libwayland prints them.
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

    /// The slot rule is aterm-gui's: 34 logical px in whole cells.
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
        // 8x4 frame: band grey, a 3x3 block of ink in the right half.
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
        assert_eq!(slot_class(&f, 4, 8, 4), ("drawn", 9));
        assert_eq!(slot_class(&f, 0, 4, 4), ("bare", 0));
        assert_eq!(slot_class(&f, 8, 9, 4), ("bare", 0), "past the frame");
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
}
