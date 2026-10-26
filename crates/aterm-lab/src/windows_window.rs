// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `windows window`: Windows VM desktop smoke test driven via QMP USB keyboard and tablet.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::linux_proof::{
    WestonView, bands_held, compare_rgb, drew_on_gpu, field, held_at, input_summary, parse_ppm,
    present_summary, reply,
};
use crate::paths::paths;
use crate::windows::{self, GPU_ID, Layout, Up, not_run, run_on_desktop, task_name};
use crate::windows_desk::{
    self as desk, ATERM_CLASS, Monitor, VIRTIO_ADAPTER, Window, helper_line, instance_dir,
};
use crate::windows_guest::{self as guest, Arch, Desktop};
use crate::windows_smoke::{TYPED, WARP, bin_dir};
use crate::{Fail, LAB, Res};

/// How long something that must happen may take.
const WITHIN: Duration = Duration::from_secs(60);

/// How many client rows [`Run::exposed`] lifts past the screen's top edge.
const LIFT: i32 = 120;

/// What [`Run::exposed`] has PowerShell do while the window's top rows are past the screen's edge.
pub const EXPOSED_TYPED: &str = "Clear-Host; Write-Output ('ROWS-' + 6*7)";
pub const EXPOSED_ECHO: &str = "ROWS-42";

/// Puts in `window` (a readback, RGB, `cw` x `ch`), at each client pixel on none of `on` (the client
/// area's parts on the monitors, [`desk::Place::on_monitors`]), what a hold expects there: the pixel
/// of `frame` (RGBA, its top-left at `at` in the client area) nearest it. Windows draws no window
/// pixel off every monitor, past the virtual screen's edge or in a part of it no monitor covers, and
/// the readback finds there what the window held when the pixel was last on a monitor, or white where
/// it never was. Answers how many pixels it put and the rectangle they span, in client coordinates.
fn off_monitors_as_framed(
    window: &mut [u8],
    (cw, ch): (usize, usize),
    on: &[desk::Rect],
    frame: &(Vec<u8>, usize, usize),
    (ox, oy): (i64, i64),
) -> (usize, Option<desk::Rect>) {
    let (f, w, h) = (&frame.0, frame.1, frame.2);
    if w == 0 || h == 0 || f.len() < w * h * 4 || window.len() < cw * ch * 3 {
        return (0, None);
    }
    let (mut put, mut span) = (0usize, None::<desk::Rect>);
    for y in 0..ch {
        for x in 0..cw {
            let (Ok(xi), Ok(yi)) = (i32::try_from(x), i32::try_from(y)) else {
                continue;
            };
            if on.iter().any(|r| r.holds((xi, yi))) {
                continue;
            }
            let nx = (x as i64 - ox).clamp(0, w as i64 - 1) as usize;
            let ny = (y as i64 - oy).clamp(0, h as i64 - 1) as usize;
            let (at, want) = ((y * cw + x) * 3, (ny * w + nx) * 4);
            window[at..at + 3].copy_from_slice(&f[want..want + 3]);
            put += 1;
            let s = span.get_or_insert(desk::Rect {
                l: xi,
                t: yi,
                r: xi + 1,
                b: yi + 1,
            });
            (s.l, s.t, s.r, s.b) = (s.l.min(xi), s.t.min(yi), s.r.max(xi + 1), s.b.max(yi + 1));
        }
    }
    (put, span)
}

/// `l,t,r,b` for each of `rects`.
fn rects_text(rects: &[desk::Rect]) -> String {
    let each: Vec<String> = rects
        .iter()
        .map(|r| format!("{},{},{},{}", r.l, r.t, r.r, r.b))
        .collect();
    each.join(" ")
}

/// What the IME row types around the composition.
pub const IME_BEFORE: &str = "Write-Output ('IME-' + '";
pub const IME_READING: &str = "nihon";
pub const IME_AFTER: &str = "')\n";
pub const IME_ECHO: &str = "IME-日本";

/// Expected `chrome` `ime` composition after each key of [`IME_READING`] and after Space.
pub const IME_STEPS: [&str; 5] = ["ｎ", "に", "にｈ", "にほ", "にほｎ"];
pub const IME_COMPOSING: &str = IME_STEPS[4];
pub const IME_CONVERTED: &str = "日本";

/// IME conversion mode mask and hiragana value required by the reading.
pub const CMODE_MASK: u32 = 0x1 | 0x2 | 0x8;
pub const CMODE_HIRAGANA: u32 = 0x1 | 0x8;

/// What a row types to read the console width PowerShell sees, after `tag`.
#[must_use]
pub fn width_typed(tag: &str) -> String {
    format!("Write-Output ('{tag}' + [Console]::WindowWidth)\n")
}

/// The tags the resize, DPI change and monitor rows read the width under, each its own.
pub const WIDTH_TAGS: [&str; 4] = ["W=", "DPI-", "RIGHT-", "LEFT-"];

/// How far the resize row drags the window's right edge left, in pixels.
pub const DRAG: i32 = 180;
pub const EDGE_IN: i32 = 4;

/// The scaling steps the DPI rows set: 150 % (each step is 25 %).
pub const SCALED: i32 = 2;

/// Microsoft's Japanese IME's keyboard layout language, English (US), and German (Germany).
pub const JAPANESE: &str = "0411";
pub const ENGLISH: &str = "0409";
pub const GERMAN: &str = "0407";

/// What the dead-key row types on the US layout around its German keys.
pub const DEAD_BEFORE: &str = "Write-Output ('DK-' + '";
pub const DEAD_AFTER: &str = "')\n";

/// The dead-key row's German keys, as the USB keyboard's keys (qcodes) at their places: a dead
/// key's chord, then the key after it. `´` is the key right of `ß` (the US `=`), `^` the key left of
/// `1` (the US `` ` ``), and `` ` `` is Shift with `´`.
pub const DEAD_KEYS: [(&[&str], &str); 5] = [
    (&["equal"], "e"),
    (&["grave_accent"], "o"),
    (&["equal"], "spc"),
    (&["equal"], "x"),
    (&["shift", "equal"], "a"),
];

/// What [`DEAD_KEYS`] type on German: `´ e` é, `^ o` ô, `´ Space` the accent alone, `´ x` the accent
/// and the `x` it does not compose with, `` ` a`` à.
pub const DEAD_TYPED: &str = "éô´´xà";
pub const DEAD_ECHO: &str = "DK-éô´´xà";

/// What the AltGr row types on the US layout around its German keys.
pub const ALTGR_BEFORE: &str = "Write-Output ('AG-' + '";

/// The keys pressed with AltGr (the right Alt, `alt_r`) on German, and what they type.
pub const ALTGR_KEYS: [&str; 4] = ["q", "e", "8", "9"];
pub const ALTGR_TYPED: &str = "@€[]";
pub const ALTGR_ECHO: &str = "AG-@€[]";

/// What a new aterm window's style words and DWM state are, as the launch rows read them: recorded
/// on aterm-window before WC.5's Windows half folded the attributes no aterm crate sets into the
/// backend's creation (tree 06b9d62a8c, both architectures, both launch rows), which must leave
/// them as they were. `WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN | WS_CLIPSIBLINGS | WS_VISIBLE`;
/// `WS_EX_APPWINDOW | WS_EX_WINDOWEDGE | WS_EX_ACCEPTFILES`; DWM answers backdrop 1
/// (`DWMSBT_NONE`) for the `DWMSBT_AUTO` upstream wrote, and dark mode on (aterm's dark theme).
pub const LAUNCH_STYLES: Styles = Styles {
    style: 0x16cf_0000,
    exstyle: 0x0004_0110,
    backdrop: Some(1),
    dark: Some(1),
};

/// A window's style words and DWM state ([`desk::Window`]'s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Styles {
    pub style: u32,
    pub exstyle: u32,
    pub backdrop: Option<i32>,
    pub dark: Option<i32>,
}

impl Styles {
    fn of(w: &desk::Window) -> Self {
        Self {
            style: w.style,
            exstyle: w.exstyle,
            backdrop: w.backdrop,
            dark: w.dark,
        }
    }
}

/// How far the modal resize row drags the window's right edge right, in pixels, holding the button.
pub const HELD_DRAG: i32 = 150;

/// The tag PowerShell prints its console width under while the button is held.
pub const HELD_TAG: &str = "HELD-";

/// What the drop row types before and after the dropped file's path, and the file it drops (a
/// space in its name, so aterm quotes it).
pub const DROP_BEFORE: &str = "Write-Output ('DND-' + (Split-Path -Leaf ";
pub const DROP_AFTER: &str = "))\n";
pub const DROP_FILE: &str = "drop 42.txt";
pub const DROP_ECHO: &str = "DND-drop 42.txt";

/// Where the drag stands, from the drag source's log: its last `over effect=<e> at=<x>,<y>` line
/// (desk.ps1 `dragsource` writes one each time OLE's `GiveFeedback` brings a new effect or cursor
/// position) when that is Copy with the cursor on `target` and off `source`. That is the answer
/// aterm's window gave OLE's `DragOver` where the button is about to be released.
/// # Errors
/// No line yet, or the last one is another effect, or elsewhere.
fn accepted_over(
    log: &str,
    target: &desk::Rect,
    source: &desk::Rect,
) -> Result<(i32, i32), String> {
    let last = log
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("over "))
        .ok_or_else(|| "no drop target has answered the drag yet".to_string())?;
    let word = |key: &str| {
        last.split_whitespace()
            .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
    };
    let at = word("at")
        .and_then(|p| p.split_once(','))
        .and_then(|(x, y)| Some((x.parse::<i32>().ok()?, y.parse::<i32>().ok()?)));
    match (word("effect"), at) {
        (Some("Copy"), Some(at)) if target.holds(at) && !source.holds(at) => Ok(at),
        _ => Err(format!("the last answer: over {last}")),
    }
}

/// The display a run drives.
#[derive(Clone, Debug)]
struct Display {
    primary: Monitor,
    second: Monitor,
}

/// Run [`desk::HELPER`] with `args` in the runner's desktop, windowless.
fn desk(up: &Up, args: &[&str]) -> Result<String, String> {
    let out = run_on_desktop(
        up,
        &task_name("desk"),
        &helper_line(args),
        Desktop::Hidden,
        false,
    );
    if out.ok() {
        Ok(out.text)
    } else {
        Err(format!(
            "desk.ps1 {} exited {:?}: {}",
            args.first().unwrap_or(&""),
            out.code,
            out.tail(6)
        ))
    }
}

fn monitors(up: &Up) -> Result<Vec<Monitor>, String> {
    let text = desk(up, &["monitors"])?;
    let m = desk::parse_monitors(&text);
    if m.is_empty() {
        return Err(format!("desk.ps1 monitors named none: {}", text.trim()));
    }
    Ok(m)
}

/// Wait (a hang detector) until `probe` holds: its value, or what it last saw.
fn within<T>(what: &str, mut probe: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let t0 = Instant::now();
    loop {
        match probe() {
            Ok(v) => return Ok(v),
            Err(last) if t0.elapsed() >= WITHIN => {
                return Err(format!(
                    "{what}: not within {} s (a hang detector); last: {last}",
                    WITHIN.as_secs()
                ));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// Put the display as the run wants it.
fn set_display(up: &Up) -> Result<Display, String> {
    let all = monitors(up)?;
    let virtio = all
        .iter()
        .find(|m| m.adapter.contains(VIRTIO_ADAPTER))
        .ok_or_else(|| {
            format!(
                "no monitor on the {VIRTIO_ADAPTER} adapter (provisioning installs virtio-win's display driver): {all:?}"
            )
        })?
        .name
        .clone();
    let _ = desk(up, &["primary", &virtio])?;
    let _ = desk(up, &["scale", &virtio, "0"])?;
    let all = monitors(up)?;
    let primary = all
        .iter()
        .find(|m| m.name == virtio)
        .cloned()
        .ok_or("the virtio-gpu's monitor went away")?;
    let second = all.iter().find(|m| m.name != virtio).cloned();
    match second {
        Some(second)
            if primary.primary
                && (primary.rect.l, primary.rect.t) == (0, 0)
                && primary.dpi == 96
                && primary
                    .scale
                    .is_some_and(|(_, cur, max)| cur == 0 && max >= SCALED)
                && second.rect.l == primary.rect.r =>
        {
            Ok(Display { primary, second })
        }
        _ => Err(format!(
            "the display is not the virtio-gpu's monitor primary at (0,0), 96 dpi, scalable to +{SCALED}, \
             with a second monitor to its right: {all:?}"
        )),
    }
}

fn display_detail(d: &Display) -> String {
    let (p, s) = (&d.primary, &d.second);
    format!(
        "{} {}x{} primary at (0,0), {} dpi, scaling steps {}; {} {}x{} at ({},{}), {} dpi",
        p.adapter,
        p.rect.width(),
        p.rect.height(),
        p.dpi,
        p.scale.map_or("none".into(), |(lo, cur, hi)| format!(
            "{lo}..{hi} (at {cur})"
        )),
        s.adapter,
        s.rect.width(),
        s.rect.height(),
        s.rect.l,
        s.rect.t,
        s.dpi
    )
}

fn qmp(l: &Layout, command: String) -> Result<(), String> {
    let answers = windows::qmp(l, &[command])?;
    match answers.first() {
        Some(a) if a.get("error").is_none() => Ok(()),
        other => Err(format!("QMP answered {other:?}")),
    }
}

/// Press the chord `keys` (qcodes) on the USB keyboard.
fn chord(l: &Layout, keys: &[&str]) -> Result<(), String> {
    qmp(l, desk::key_command(keys, 60))?;
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

/// Type `text` on the USB keyboard, key by key.
fn type_text(l: &Layout, text: &str) -> Result<(), String> {
    for c in text.chars() {
        let keys = desk::qcodes(c).ok_or_else(|| format!("no US key types {c:?}"))?;
        qmp(l, desk::key_command(&keys, 30))?;
        std::thread::sleep(Duration::from_millis(40));
    }
    Ok(())
}

/// Click the tablet's left button at `(x, y)` on the primary monitor.
fn click(l: &Layout, d: &Display, (x, y): (i32, i32)) -> Result<(), String> {
    let r = d.primary.rect;
    qmp(
        l,
        desk::tablet_move(x - r.l, y - r.t, r.width(), r.height()),
    )?;
    std::thread::sleep(Duration::from_millis(150));
    qmp(l, desk::tablet_button(true))?;
    std::thread::sleep(Duration::from_millis(80));
    qmp(l, desk::tablet_button(false))?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}

/// Drag with the tablet's left button from `from` to `to` on the primary monitor, in steps.
fn drag(l: &Layout, d: &Display, from: (i32, i32), to: (i32, i32)) -> Result<(), String> {
    press_to(l, d, from, to, 30)?;
    release(l)
}

/// Press the tablet's left button at `from` and move it to `to` in steps `pause_ms` apart, holding
/// the button.
fn press_to(
    l: &Layout,
    d: &Display,
    from: (i32, i32),
    to: (i32, i32),
    pause_ms: u64,
) -> Result<(), String> {
    let r = d.primary.rect;
    let at = |(x, y): (i32, i32)| desk::tablet_move(x - r.l, y - r.t, r.width(), r.height());
    qmp(l, at(from))?;
    std::thread::sleep(Duration::from_millis(300));
    qmp(l, desk::tablet_button(true))?;
    std::thread::sleep(Duration::from_millis(200));
    let steps = 20;
    for i in 1..=steps {
        qmp(
            l,
            at((
                from.0 + (to.0 - from.0) * i / steps,
                from.1 + (to.1 - from.1) * i / steps,
            )),
        )?;
        std::thread::sleep(Duration::from_millis(pause_ms));
    }
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

/// Release the tablet's left button where it is.
fn release(l: &Layout) -> Result<(), String> {
    qmp(l, desk::tablet_button(false))?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}

/// A screenshot of `device`'s screen (the virtio-gpu's by default; `None` is ramfb's) at `path`.
fn shot(l: &Layout, path: &Path, device: Option<&str>) -> String {
    let p = path.to_string_lossy();
    match qmp(l, desk::screendump_command(&p, device)) {
        Ok(()) => format!("screenshot {}", path.display()),
        Err(e) => format!("no screenshot: {e}"),
    }
}

struct Session<'a> {
    up: &'a Up,
    arch: Arch,
    dir: String,
    task: String,
    pid: u32,
}

/// The config a `--cpu` run's instance takes.
fn cpu_config() -> String {
    format!(
        "cursor_blink = false\ncursor_trail = false\n{}{}",
        crate::instance::CONFIG_OFF,
        crate::linux_proof::DECK_OFF
    )
}

impl<'a> Session<'a> {
    /// Starts instance `n` of `arch` in the runner's desktop and waits for `ready`.
    fn launch(up: &'a Up, arch: Arch, n: u32, cpu: bool) -> Result<Session<'a>, String> {
        let dir = instance_dir(arch, n);
        let task = task_name(&format!("window-{}-{n}", arch.name()));
        let cleared = up.ssh.run(&format!(
            "(if exist {dir} rmdir /s /q {dir}) & if exist {dir} (echo left)"
        ));
        if cleared.text.contains("left") {
            return Err(format!(
                "{dir} is held (an earlier instance still running?): {}",
                cleared.tail(3)
            ));
        }
        let config = if cpu {
            cpu_config()
        } else {
            crate::instance::CONFIG_OFF.to_string()
        };
        let config =
            aterm_codec::base64::encode(config.as_bytes()).map_err(|e| format!("base64: {e:?}"))?;
        let bin = bin_dir(arch);
        let mut words = vec!["launch", &bin, &dir, &config];
        if cpu {
            words.push("--cpu");
        }
        let started = run_on_desktop(up, &task, &helper_line(&words), Desktop::Started, false);
        if !started.ok() {
            return Err(format!("its task did not start: {}", started.tail(5)));
        }
        let pid = within("aterm-gui's pid", || {
            let out = up.ssh.run(&format!("type {dir}\\pid"));
            out.text.trim().parse::<u32>().map_err(|_| out.tail(2))
        });
        let mut s = Session {
            up,
            arch,
            dir,
            task,
            pid: 0,
        };
        match pid {
            Ok(pid) => s.pid = pid,
            Err(e) => {
                s.end();
                return Err(e);
            }
        }
        let sock = format!("{}\\aterm.sock", s.dir);
        let ready = within("its control socket", || {
            let out = up.ssh.run(&format!("if exist {sock} (echo yes)"));
            if out.text.trim() == "yes" || s.exit().is_some() {
                Ok(())
            } else {
                Err("no socket yet".into())
            }
        })
        .and_then(|()| {
            if let Some(code) = s.exit() {
                return Err(format!("aterm-gui exited {code}: {}", s.stderr()));
            }
            s.ctl(&["ready", "60000"])
        });
        if let Err(e) = ready {
            s.end();
            return Err(e);
        }
        Ok(s)
    }

    /// `aterm-ctl` (over SSH, as the runner) to this instance.
    fn ctl(&self, words: &[&str]) -> Result<String, String> {
        self.ask(words, true)
    }

    /// Wait (up to a minute) until a visible row holds `text` as written ([`literal`]).
    fn await_text(&self, text: &str) -> Result<String, String> {
        self.ctl(&["await", "match", &literal(text), "timeout=60000"])
    }

    /// `aterm-ctl` to this instance.
    fn ask(&self, words: &[&str], ok: bool) -> Result<String, String> {
        let quoted: Vec<String> = words.iter().map(|w| guest::cmd_quote(w)).collect();
        let out = self.up.ssh.run(&format!(
            "{}\\aterm-ctl.exe --sock {}\\aterm.sock --timeout 90 {}",
            bin_dir(self.arch),
            self.dir,
            quoted.join(" ")
        ));
        let r = reply(&out.text).to_string();
        if out.ok() && (!ok || r.starts_with("OK")) {
            Ok(r)
        } else {
            Err(format!(
                "`{}` answered {:?} (exit {:?})",
                words.join(" "),
                crate::util::one_line(&r, 300),
                out.code
            ))
        }
    }

    /// Its window of class [`ATERM_CLASS`].
    fn window(&self) -> Result<Window, String> {
        let text = desk(self.up, &["windows", &self.pid.to_string()])?;
        desk::parse_windows(&text)
            .into_iter()
            .find(|w| w.class == ATERM_CLASS)
            .ok_or_else(|| {
                format!(
                    "pid {} has no visible {ATERM_CLASS} window: {}",
                    self.pid,
                    text.trim()
                )
            })
    }

    fn dims(&self) -> Result<Dims, String> {
        let r = self.ctl(&["dims"])?;
        Dims::parse(&r).ok_or_else(|| format!("`dims` answered {r:?}"))
    }

    fn stderr(&self) -> String {
        self.up
            .ssh
            .run(&format!("type {}\\stderr.log", self.dir))
            .text
    }

    /// Its exit status, once the helper wrote it.
    fn exit(&self) -> Option<i32> {
        let out = self.up.ssh.run(&format!("type {}\\exit", self.dir));
        if out.ok() {
            out.text.trim().parse().ok()
        } else {
            None
        }
    }

    /// Its task stopped and gone, and aterm-gui with it.
    fn end(&self) {
        let _ = self.up.admin.ps(&guest::desktop_stop(&self.task));
        if self.pid != 0 && self.exit().is_none() {
            let _ = self
                .up
                .admin
                .run(&format!("taskkill /F /T /PID {}", self.pid));
        }
    }
}

/// What `dims` says about the grid and the window.
#[derive(Clone, Debug, PartialEq)]
pub struct Dims {
    pub rows: u32,
    pub cols: u32,
    pub scale: String,
    pub font_px: String,
    pub window_cols: u32,
    pub window_rows: u32,
    pub frame: (u32, u32),
    pub geometry: String,
}

impl Dims {
    /// A `dims` reply (`OK <rows> <cols> … scale=… window_cols=…`).
    #[must_use]
    pub fn parse(r: &str) -> Option<Dims> {
        let mut w = r.split_whitespace();
        if w.next() != Some("OK") {
            return None;
        }
        let num = |k: &str| field(r, k)?.parse::<u32>().ok();
        Some(Dims {
            rows: w.next()?.parse().ok()?,
            cols: w.next()?.parse().ok()?,
            scale: field(r, "scale")?.to_string(),
            font_px: field(r, "font_px")?.to_string(),
            window_cols: num("window_cols")?,
            window_rows: num("window_rows")?,
            frame: (num("frame_w")?, num("frame_h")?),
            geometry: field(r, "geometry")?.to_string(),
        })
    }

    fn brief(&self) -> String {
        format!(
            "scale {} font_px {} grid {}x{} frame {}x{}",
            self.scale, self.font_px, self.cols, self.rows, self.frame.0, self.frame.1
        )
    }
}

/// The machine `arch`'s process runs as.
#[must_use]
pub const fn machine(arch: Arch) -> &'static str {
    match arch {
        Arch::Aarch64 => "aa64",
        Arch::X86_64 => "8664",
    }
}

/// A number field of a reply.
fn count(r: &str, key: &str) -> Option<u64> {
    field(r, key)?.parse().ok()
}

/// Formats `frames`, dropped presents, and the `dims` surface for a row's evidence.
fn presents(s: &Session<'_>) -> String {
    let metrics = s.ctl(&["metrics"]).ok();
    let read = |key: &str| {
        metrics
            .as_deref()
            .and_then(|m| field(m, key))
            .map_or_else(|| "?".to_string(), str::to_string)
    };
    let n = read("frames");
    let drops = format!(
        "present_drops={} last {} parked={}",
        read("present_drops"),
        read("last_present_drop_reason"),
        read("last_present_drop_parked")
    );
    let surface = s
        .ctl(&["dims"])
        .ok()
        .and_then(|d| {
            Some(format!(
                "{}x{}",
                field(&d, "surface_w")?,
                field(&d, "surface_h")?
            ))
        })
        .unwrap_or_else(|| "?".to_string());
    format!("frames={n} {drops} surface={surface}")
}

/// How long [`Watch`] watches: the IME row ends well within it.
const WATCH_MS: u32 = 120_000;

/// Session input watcher ([`desk::HELPER`]'s `watch`) running alongside a row.
struct Watch<'a> {
    up: &'a Up,
    arch: Arch,
    task: String,
    file: String,
}

impl<'a> Watch<'a> {
    fn start(up: &'a Up, arch: Arch, pid: u32) -> Result<Watch<'a>, String> {
        let task = task_name(&format!("watch-{}", arch.name()));
        let file = format!("{}\\{}-ime-watch.txt", desk::DESK, arch.name());
        let _ = up.ssh.run(&format!("if exist {file} del /q {file}"));
        let started = run_on_desktop(
            up,
            &task,
            &helper_line(&[
                "watch",
                &pid.to_string(),
                ATERM_CLASS,
                &WATCH_MS.to_string(),
                &file,
            ]),
            Desktop::Started,
            false,
        );
        if !started.ok() {
            return Err(format!("its task did not start: {}", started.tail(5)));
        }
        within("the watcher's log", || {
            let out = up.ssh.run(&format!("if exist {file} (echo yes)"));
            if out.text.trim() == "yes" {
                Ok(())
            } else {
                Err("no log yet".into())
            }
        })
        .inspect_err(|_| {
            let _ = up.admin.ps(&guest::desktop_stop(&task));
        })?;
        Ok(Watch {
            up,
            arch,
            task,
            file,
        })
    }

    /// Stopped, its log copied to `results`.
    fn keep(self, results: &Path) -> String {
        let _ = self.up.admin.ps(&guest::desktop_stop(&self.task));
        let local = results.join(format!("{}-window-ime-watch.txt", self.arch.name()));
        let got = self.up.ssh.get(&self.file.replace('\\', "/"), &local);
        if got.ok() {
            format!("input watched: {}", local.display())
        } else {
            format!("no watch log: {}", got.tail(2))
        }
    }
}

/// One row: its name, and a pass's detail or a failure's.
type Row = (String, Result<String, String>);

/// A row run on the instance: a pass's detail or a failure's.
type Step<'a> = dyn Fn(&Session<'_>) -> Result<String, String> + 'a;

/// The rows one arch's run went through, in order.
pub const ROWS: [&str; 12] = [
    "launch",
    "type",
    "IME",
    "dead keys",
    "AltGr",
    "resize",
    "modal resize",
    "drop",
    "DPI change",
    "second monitor",
    "close",
    "launch at 150 %",
];

struct Run<'a> {
    up: &'a Up,
    arch: Arch,
    display: Display,
    results: PathBuf,
    /// Whether the IME row runs [`Watch`].
    watch: bool,
    /// `--cpu`: the instances present on the CPU.
    cpu: bool,
}

impl Run<'_> {
    const fn l(&self) -> &Layout {
        &self.up.layout
    }

    /// What this run's files are named after: `window`, or `window-cpu`.
    const fn tag(&self) -> &'static str {
        if self.cpu { "window-cpu" } else { "window" }
    }

    fn shot(&self, what: &str, device: Option<&str>) -> String {
        std::thread::sleep(Duration::from_secs(1));
        shot(
            self.l(),
            &self
                .results
                .join(format!("{}-{}-{what}.png", self.arch.name(), self.tag())),
            device,
        )
    }

    /// With `--cpu`, `detail` and then what [`Run::held`] says of the window's pixels now.
    fn and_held(&self, s: &Session<'_>, what: &str, detail: String) -> Result<String, String> {
        if !self.cpu {
            return Ok(detail);
        }
        let held = self.held(s, what).map_err(|why| {
            let now = presents(s);
            std::thread::sleep(Duration::from_secs(1));
            format!("{why}; then {now}, a second later {}", presents(s))
        })?;
        Ok(format!("{detail}; {held}"))
    }

    /// Compares the frame `image` captured with the window DC pixels read back by the helper.
    fn held(&self, s: &Session<'_>, what: &str) -> Result<String, String> {
        let stem = format!("{}-{}-{what}", self.arch.name(), self.tag());
        let image = s.ctl(&["image", &format!("{stem}-frame.png")])?;
        let guest_png = image
            .split_whitespace()
            .nth(3)
            .ok_or_else(|| format!("`image` answered {image}"))?
            .to_string();
        let guest_ppm = format!("{}\\{stem}-window.ppm", s.dir);
        let text = desk(
            self.up,
            &["readback", &s.pid.to_string(), ATERM_CLASS, &guest_ppm],
        )?;
        let place = desk::parse_place(&text)
            .ok_or_else(|| format!("desk.ps1 readback answered {}", text.trim()))?;
        let client = place.client;
        let png = self.results.join(format!("{stem}-frame.png"));
        let ppm = self.results.join(format!("{stem}-window.ppm"));
        for (from, to) in [(&guest_png, &png), (&guest_ppm, &ppm)] {
            let got = self.up.ssh.get(&from.replace('\\', "/"), to);
            if !got.ok() {
                return Err(format!("{from} not copied out: {}", got.tail(2)));
            }
        }
        let presented = std::fs::read(&png)
            .ok()
            .and_then(|b| aterm_png::decode_rgba8(&b, 8192))
            .ok_or_else(|| format!("{}: not a PNG this lab decodes", png.display()))?;
        let window = std::fs::read(&ppm)
            .ok()
            .and_then(|b| parse_ppm(&b))
            .ok_or_else(|| format!("{}: not a PPM this lab reads", ppm.display()))?;
        let (w, h) = (presented.1, presented.2);
        let (cw, ch) = (window.1, window.2);
        if (cw, ch) != client {
            return Err(format!(
                "the readback is {cw}x{ch}, its client area {}x{}",
                client.0, client.1
            ));
        }
        let files = format!("frame {}, window {}", png.display(), ppm.display());
        let monitors = [self.display.primary.rect, self.display.second.rect];
        let on = place.on_monitors(&monitors);
        if on.is_empty() {
            return Err(format!(
                "the window's client area ({cw}x{ch} at {:?}) is on no monitor ({})",
                place.at,
                rects_text(&monitors)
            ));
        }
        let (ox, oy) = if (cw, ch) == (w, h) {
            (0, 0)
        } else {
            let d = s.ctl(&["dims"])?;
            let num = |k: &str| field(&d, k).and_then(|v| v.parse::<i64>().ok());
            if (num("surface_w"), num("surface_h"))
                != (i64::try_from(cw).ok(), i64::try_from(ch).ok())
            {
                return Err(format!(
                    "the window's client area is {cw}x{ch}, the frame {w}x{h}, and `dims` names \
                     another surface: {d} ({files})"
                ));
            }
            (num("offset_x").unwrap_or(0), num("offset_y").unwrap_or(0))
        };
        let mut window = window;
        let (put, span) =
            off_monitors_as_framed(&mut window.0, (cw, ch), &on, &presented, (ox, oy));
        let off = span.map_or_else(String::new, |r| {
            format!(
                "; its {put} pixels on no monitor (columns {}..{}, rows {}..{}, the client area at \
                 {:?} and the monitors {}), which Windows does not draw, are not held",
                r.l,
                r.r,
                r.t,
                r.b,
                place.at,
                rects_text(&monitors)
            )
        });
        if (cw, ch) == (w, h) {
            compare_rgb(&presented.0, w, h, &window, 0..0)
                .map_err(|why| format!("{why}{off} ({files})"))?;
            return Ok(format!(
                "the window's {cw}x{ch} client pixels (GetDIBits) equal the presented frame \
                 exactly{off}"
            ));
        }
        let rgba = (
            window
                .0
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 0xff])
                .collect::<Vec<u8>>(),
            cw,
            ch,
        );
        let view = WestonView {
            x: 0,
            y: 0,
            width: cw,
            height: ch,
            format: "XRGB8888".into(),
        };
        held_at(&rgba, &presented, ox, oy, 0..0).map_err(|why| format!("{why}{off} ({files})"))?;
        let n = bands_held(&rgba, &presented, &view, (ox, oy))
            .map_err(|why| format!("{why}{off} ({files})"))?;
        Ok(format!(
            "the window's {cw}x{ch} client pixels (GetDIBits) hold the presented {w}x{h} frame \
             exactly at ({ox},{oy}), and its {n} band pixels each the frame's nearest{off}"
        ))
    }

    /// Where `s`'s window's client area is ([`desk::Place`]).
    fn place(&self, s: &Session<'_>, change: Option<(&str, i32)>) -> Result<desk::Place, String> {
        let pid = s.pid.to_string();
        let n = change.map(|(_, n)| n.to_string());
        let args = match (change, &n) {
            (Some((verb, _)), Some(n)) => vec![verb, &pid, ATERM_CLASS, n],
            _ => vec!["place", &pid, ATERM_CLASS],
        };
        let text = desk(self.up, &args)?;
        desk::parse_place(&text)
            .ok_or_else(|| format!("desk.ps1 {} answered {}", args[0], text.trim()))
    }

    /// Checks what `aterm` presented while rows of its window were off-screen.
    fn exposed(&self, s: &Session<'_>, what: &str) -> Result<String, String> {
        let at = self.place(s, None)?;
        let ch = i32::try_from(at.client.1).map_err(|_| format!("{at:?}: no height"))?;
        if ch <= LIFT || ch > at.screen.height() {
            return Err(format!(
                "the client area ({}x{ch} at {:?}) does not fit the virtual screen {:?} with {LIFT} \
                 rows lifted",
                at.client.0, at.at, at.screen
            ));
        }
        let lifted = self.place(s, Some(("lift", LIFT)))?;
        if lifted.on_screen().map(|r| (r.t, r.b)) != Some((LIFT, ch)) {
            return Err(format!(
                "lifted, the client area is not {LIFT} rows above the screen: {lifted:?}"
            ));
        }
        s.ctl(&["send", EXPOSED_TYPED])?;
        s.ctl(&["key", "enter"])?;
        s.await_text(EXPOSED_ECHO)?;
        std::thread::sleep(Duration::from_secs(1));
        let above = self.held(s, &format!("{what}-cut"))?;
        let top = (at.screen.b - ch).min(at.at.1).max(at.screen.t);
        let back = self.place(s, Some(("move", top)))?;
        if back.on_screen().map(|r| (r.t, r.b)) != Some((0, ch)) {
            return Err(format!(
                "moved back down, the client area is still cut: {back:?}"
            ));
        }
        std::thread::sleep(Duration::from_secs(1));
        let whole = self.held(s, &format!("{what}-exposed"))?;
        Ok(format!(
            "lifted from {:?} to {:?}, {LIFT} rows past the screen's top, where PowerShell cleared \
             the screen and printed {EXPOSED_ECHO}: {above}; moved back to {:?}, all on the screen: \
             {whole}",
            at.at, lifted.at, back.at
        ))
    }

    fn scale(&self, steps: i32) -> Result<Monitor, String> {
        let text = desk(
            self.up,
            &["scale", &self.display.primary.name, &steps.to_string()],
        )?;
        desk::parse_monitors(&text)
            .into_iter()
            .find(|m| m.name == self.display.primary.name)
            .filter(|m| m.scale.is_some_and(|(_, cur, _)| cur == steps))
            .ok_or_else(|| {
                format!(
                    "{} did not take scaling step {steps}: {}",
                    self.display.primary.name,
                    text.trim()
                )
            })
    }

    fn launch(&self, s: &Session<'_>, want_scale: &str, want_dpi: u32) -> Result<String, String> {
        let w = within("its window", || s.window())?;
        let machine = machine(self.arch);
        if w.machine != machine {
            return Err(format!(
                "its process runs {} code, not {machine} ({}): {w:?}",
                w.machine,
                self.arch.name()
            ));
        }
        if w.awareness != 2 {
            return Err(format!(
                "its window is not per-monitor DPI aware (DPI_AWARENESS {}): {w:?}",
                w.awareness
            ));
        }
        if w.dpi != want_dpi || !self.display.primary.rect.holds(w.rect.centre()) {
            return Err(format!(
                "its window is not at {want_dpi} dpi on the primary monitor: {w:?}"
            ));
        }
        if Styles::of(&w) != LAUNCH_STYLES {
            return Err(format!(
                "its window's {}, not the recorded {}",
                styles(&w),
                styles_of(LAUNCH_STYLES)
            ));
        }
        let dims = s.dims()?;
        if dims.scale != want_scale || dims.geometry != "window" {
            return Err(format!(
                "`dims` says scale={} geometry={}, not {want_scale} and window",
                dims.scale, dims.geometry
            ));
        }
        let backend = if self.cpu { "cpu" } else { "gpu" };
        let metrics = within(&format!("a frame with backend={backend}"), || {
            let m = s.ctl(&["metrics"])?;
            if field(&m, "backend") == Some(backend) {
                Ok(m)
            } else {
                Err(format!("backend={}", field(&m, "backend").unwrap_or("?")))
            }
        })?;
        let device = if self.cpu {
            "the CPU (backend=cpu)".to_string()
        } else {
            let stderr = s.stderr();
            let device = drew_on_gpu(&stderr, field(&metrics, "backend"))?
                .unwrap_or("(no device line)")
                .to_string();
            if !device.contains(WARP) {
                return Err(format!("drew on {device}, not on WARP ({WARP})"));
            }
            device
        };
        Ok(format!(
            "pid {} (machine {}, {}), window class {ATERM_CLASS} {}x{} client at {} dpi, per-monitor aware, geometry=window, \
             {}; {}; drew on {device}",
            s.pid,
            w.machine,
            self.arch.runs(),
            w.client.0,
            w.client.1,
            w.dpi,
            dims.brief(),
            styles(&w)
        ))
    }

    fn typed(&self, s: &Session<'_>) -> Result<String, String> {
        let w = s.window()?;
        let before = count(&s.ctl(&["metrics"])?, "n_input").unwrap_or(0);
        click(self.l(), &self.display, w.rect.centre())?;
        type_text(self.l(), &format!("{TYPED}\n"))?;
        s.await_text(crate::linux_proof::ECHO)?;
        let metrics = s.ctl(&["metrics"])?;
        let after = count(&metrics, "n_input").unwrap_or(0);
        if after <= before {
            return Err(format!(
                "n_input {before} -> {after}: aterm counted no input"
            ));
        }
        self.and_held(
            s,
            "typed",
            format!(
                "clicked into, `{TYPED}` typed on the USB keyboard, {} echoed; n_input {before} -> {after}",
                crate::linux_proof::ECHO
            ),
        )
    }

    /// Aterm's composition as `chrome`'s `ime` line shows it (its text and caret).
    fn composition(s: &Session<'_>) -> Result<String, String> {
        let chrome = s.ask(&["chrome"], false)?;
        crate::linux_baseline::ime_line(&chrome)
            .map(|l| crate::linux_baseline::ime_composition(l).to_string())
            .ok_or_else(|| "`chrome` has no ime line".to_string())
    }

    /// What aterm holds when a row fails: its last screen line and its composition.
    fn holds(s: &Session<'_>) -> String {
        let line = s.ask(&["text"], false).map_or_else(
            |e| e,
            |t| {
                t.lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim_end()
                    .to_string()
            },
        );
        let composing = Self::composition(s).unwrap_or_else(|e| e);
        format!("aterm's line {line:?}, {composing}")
    }

    /// Wait (a hang detector) until aterm's composition is `want`.
    fn composing(s: &Session<'_>, want: &str, what: &str) -> Result<(), String> {
        let prefix = format!("ime preedit={want:?} ");
        within(what, || {
            let c = Self::composition(s)?;
            if c.starts_with(&prefix) {
                Ok(())
            } else {
                Err(c)
            }
        })
    }

    fn ime(&self, s: &Session<'_>) -> Result<String, String> {
        let watch = self.watch.then(|| Watch::start(self.up, self.arch, s.pid));
        let verdict = self
            .ime_steps(s)
            .map_err(|e| format!("{e}; {}", Self::holds(s)));
        let restored = match &verdict {
            Ok(_) => String::new(),
            Err(_) => match self.to_layout(s, ENGLISH) {
                Ok(_) => "; input back to English".to_string(),
                Err(e) => format!("; input not put back to English: {e}"),
            },
        };
        let kept = match watch {
            None => String::new(),
            Some(Ok(w)) => format!("; {}", w.keep(&self.results)),
            Some(Err(e)) => format!("; no watch: {e}"),
        };
        match verdict {
            Ok(d) => Ok(format!("{d}{kept}")),
            Err(e) => Err(format!("{e}{restored}{kept}")),
        }
    }

    /// The window's keyboard layout made `want` (`0411`, `0409`, `0407`): Win+Space, each press the
    /// next of the session's input languages ([`guest::LANGUAGES`]), until it is.
    fn to_layout(&self, s: &Session<'_>, want: &str) -> Result<bool, String> {
        let mut now = s.window()?.layout;
        if now == want {
            return Ok(false);
        }
        let mut seen = vec![now.clone()];
        for _ in 0..guest::LANGUAGES.len() {
            chord(self.l(), &["meta_l", "spc"])?;
            let from = now.clone();
            now = within(
                &format!("Win+Space from layout {from} toward {want}"),
                || {
                    let w = s.window()?;
                    if w.layout == from {
                        Err(format!("layout {}", w.layout))
                    } else {
                        Ok(w.layout)
                    }
                },
            )?;
            if now == want {
                return Ok(true);
            }
            seen.push(now.clone());
        }
        Err(format!(
            "Win+Space went through layouts {} and not to {want}",
            seen.join(" ")
        ))
    }

    fn ime_steps(&self, s: &Session<'_>) -> Result<String, String> {
        type_text(self.l(), IME_BEFORE)?;
        let switched = if self.to_layout(s, JAPANESE)? {
            "Win+Space to"
        } else {
            "already on"
        };
        let mut w = s.window()?;
        let mut opened = "open";
        if w.ime_open != Some(true) {
            chord(self.l(), &["alt", "grave_accent"])?;
            w = within("Alt+` opening the IME", || {
                let w = s.window()?;
                if w.ime_open == Some(true) {
                    Ok(w)
                } else {
                    Err(format!("ime open {:?}", w.ime_open))
                }
            })?;
            opened = "opened with Alt+`";
        }
        let conversion = w.conversion;
        if conversion.is_none_or(|c| c & CMODE_MASK != CMODE_HIRAGANA) {
            return Err(format!(
                "the IME is open in conversion mode {conversion:?}, not hiragana ({CMODE_HIRAGANA:#x} under \
                 {CMODE_MASK:#x}): the reading would not compose as `{IME_COMPOSING}`"
            ));
        }
        for (c, want) in IME_READING.chars().zip(IME_STEPS) {
            type_text(self.l(), &c.to_string())?;
            Self::composing(
                s,
                want,
                &format!("`{c}` of `{IME_READING}` in aterm's composition"),
            )?;
        }
        let composing = self.shot("ime-composing", Some(GPU_ID));
        chord(self.l(), &["spc"])?;
        let converted_in = Self::composing(s, IME_CONVERTED, "Space's conversion in aterm");
        let converted = self.shot("ime-converted", Some(GPU_ID));
        converted_in.map_err(|e| format!("{e}; {converted}"))?;
        chord(self.l(), &["ret"])?;
        Self::composing(s, "", "Enter's commit ending the composition")?;
        let back = if self.to_layout(s, ENGLISH)? {
            "Win+Space back to"
        } else {
            "already back on"
        };
        type_text(self.l(), IME_AFTER)?;
        s.await_text(IME_ECHO)?;
        self.and_held(s, "ime", format!(
            "{switched} Microsoft IME (layout {JAPANESE}), IME {opened} (conversion mode {}), `{IME_READING}` composed \
             key by key ({} in aterm), converted (Space, {IME_CONVERTED:?}) and committed (Enter) into PowerShell's line, \
             which printed {IME_ECHO}; {back} English ({ENGLISH}); {composing}; {converted}",
            conversion.map_or("-".to_string(), |c| format!("{c:#x}")),
            IME_STEPS.join(" ")
        ))
    }

    /// German's keys typed between `before` and PowerShell's `')`, by `keys`, awaiting `echo`: the
    /// input goes back to English whatever happens.
    fn german(
        &self,
        s: &Session<'_>,
        before: &str,
        keys: &dyn Fn(&Layout) -> Result<(), String>,
        echo: &str,
    ) -> Result<String, String> {
        let w = s.window()?;
        if !w.foreground {
            click(self.l(), &self.display, w.rect.centre())?;
        }
        type_text(self.l(), before)?;
        let typed = self.to_layout(s, GERMAN).and_then(|switched| {
            keys(self.l())?;
            Ok(switched)
        });
        let back = self.to_layout(s, ENGLISH);
        let switched = typed.map_err(|e| format!("{e}; {}", Self::holds(s)))?;
        back.map_err(|e| format!("input not put back to English: {e}"))?;
        type_text(self.l(), DEAD_AFTER)?;
        s.await_text(echo)
            .map_err(|e| format!("{e}; {}", Self::holds(s)))?;
        Ok(if switched {
            "Win+Space to German (layout 0407) and back to English (0409)".to_string()
        } else {
            "already on German (layout 0407), Win+Space back to English (0409)".to_string()
        })
    }

    fn dead_keys(&self, s: &Session<'_>) -> Result<String, String> {
        let keys = |l: &Layout| {
            for (dead, next) in DEAD_KEYS {
                chord(l, dead)?;
                chord(l, &[next])?;
            }
            Ok(())
        };
        let how = self.german(s, DEAD_BEFORE, &keys, DEAD_ECHO)?;
        let detail = format!(
            "{how}; German's dead keys `´ e`, `^ o`, `´ Space`, `´ x` and `` ` a`` typed {DEAD_TYPED:?} into \
             PowerShell's line, which printed {DEAD_ECHO}; {}",
            self.shot("dead-keys", Some(GPU_ID))
        );
        self.and_held(s, "dead-keys", detail)
    }

    fn altgr(&self, s: &Session<'_>) -> Result<String, String> {
        let keys = |l: &Layout| {
            for k in ALTGR_KEYS {
                chord(l, &["alt_r", k])?;
            }
            Ok(())
        };
        let how = self.german(s, ALTGR_BEFORE, &keys, ALTGR_ECHO)?;
        let detail = format!(
            "{how}; AltGr (the right Alt) with {} typed {ALTGR_TYPED:?}, not Ctrl+Alt chords, into \
             PowerShell's line, which printed {ALTGR_ECHO}; {}",
            ALTGR_KEYS.join(", "),
            self.shot("altgr", Some(GPU_ID))
        );
        self.and_held(s, "altgr", detail)
    }

    /// The window's right edge dragged right with the button held: while Windows' modal size loop
    /// runs, aterm takes the resize, PowerShell prints its new width and aterm presents it.
    fn modal_resize(&self, s: &Session<'_>) -> Result<String, String> {
        let w = s.window()?;
        let before = s.dims()?;
        let y = w.rect.centre().1;
        let edge = w.rect.r - EDGE_IN;
        press_to(
            self.l(),
            &self.display,
            (edge, y),
            (edge + HELD_DRAG, y),
            50,
        )?;
        let verdict = self.held_in_loop(s, &before);
        let released = release(self.l());
        let ended = within("Windows' size loop ending at the release", || {
            let w = s.window()?;
            if w.move_size {
                Err("the window's thread is still in its size loop".into())
            } else {
                Ok(())
            }
        });
        let detail = verdict?;
        released?;
        ended?;
        let status = s.ctl(&["status"])?;
        if field(&status, "render") != Some("ok") {
            return Err(format!("after the release `status` says {status}"));
        }
        Ok(format!("{detail}; released, the loop ended, render=ok"))
    }

    /// What [`Run::modal_resize`] holds while the button is down.
    fn held_in_loop(&self, s: &Session<'_>, before: &Dims) -> Result<String, String> {
        let in_loop = || {
            let w = s.window()?;
            if w.move_size {
                Ok(w)
            } else {
                Err("the window's thread is not in Windows' size loop".to_string())
            }
        };
        within("Windows' size loop on aterm's window", in_loop)?;
        let during = within("the grid wider with the button held", || {
            let d = s.dims()?;
            if d.window_cols >= before.window_cols + 10 {
                Ok(d)
            } else {
                Err(format!("window_cols {}", d.window_cols))
            }
        })?;
        let frames = count(&s.ctl(&["metrics"])?, "frames").unwrap_or(0);
        s.ctl(&[
            "send",
            &format!("Write-Output ('{HELD_TAG}' + [Console]::WindowWidth)"),
        ])?;
        s.ctl(&["key", "enter"])?;
        let echo = format!("{HELD_TAG}{}", during.cols);
        s.await_text(&echo)
            .map_err(|e| format!("{e}; {}", Self::holds(s)))?;
        let after = within("a frame presented after the echo", || {
            let n = count(&s.ctl(&["metrics"])?, "frames").unwrap_or(0);
            if n > frames {
                Ok(n)
            } else {
                Err(format!("frames {n}"))
            }
        })?;
        let held = self.and_held(s, "modal-resize", String::new())?;
        let w = in_loop().map_err(|e| format!("{e} after the echo, with the button held"))?;
        Ok(format!(
            "right edge pressed and dragged {HELD_DRAG} px right, the button held: the window's thread in \
             Windows' size loop (GUI_INMOVESIZE), grid {}x{} -> {}x{} and client {}x{} while held; PowerShell \
             printed {echo} and aterm presented it in the loop (frames {frames} -> {after}){held}; {}",
            before.cols,
            before.rows,
            during.cols,
            during.rows,
            w.client.0,
            w.client.1,
            self.shot("modal-resize", Some(GPU_ID))
        ))
    }

    /// A file dragged by OLE from a window of the desk helper's onto aterm's: aterm pastes its path,
    /// quoted, and PowerShell reads it.
    fn drop(&self, s: &Session<'_>) -> Result<String, String> {
        let w = s.window()?;
        if !w.foreground {
            click(self.l(), &self.display, w.rect.centre())?;
        }
        type_text(self.l(), DROP_BEFORE)?;
        let file = format!("{}\\{DROP_FILE}", s.dir);
        let log = format!("{}\\dragsource.txt", s.dir);
        let p = self.display.primary.rect;
        let (x, y) = (p.r - 320, p.b - 280);
        let task = task_name(&format!("drag-{}", self.arch.name()));
        let started = run_on_desktop(
            self.up,
            &task,
            &helper_line(&["dragsource", &file, &x.to_string(), &y.to_string(), &log]),
            Desktop::Started,
            false,
        );
        if !started.ok() {
            return Err(format!(
                "the drag source did not start: {}",
                started.tail(5)
            ));
        }
        let verdict = self.dragged(s, &w, &log);
        let _ = self.up.admin.ps(&guest::desktop_stop(&task));
        let (source, over, effect) = verdict?;
        let w = s.window()?;
        if !w.foreground {
            click(self.l(), &self.display, w.rect.centre())?;
        }
        type_text(self.l(), DROP_AFTER)?;
        s.await_text(DROP_ECHO)
            .map_err(|e| format!("{e}; {}", Self::holds(s)))?;
        let detail = format!(
            "`{DROP_FILE}` dragged by OLE (CF_HDROP) from a window at ({},{}) onto aterm's window at ({},{}), \
             which answered Copy under the cursor at ({},{}) before the release, effect {effect}; aterm \
             pasted its path quoted and PowerShell printed {DROP_ECHO}; {}",
            source.l,
            source.t,
            w.rect.l,
            w.rect.t,
            over.0,
            over.1,
            self.shot("drop", Some(GPU_ID))
        );
        self.and_held(s, "drop", detail)
    }

    /// The drag itself: the source's label found, pressed, the tablet moved onto aterm's client
    /// area, and released once aterm's window has answered OLE's `DragOver` there with Copy
    /// ([`accepted_over`]); the source's rectangle, where that answer came, and the drag's effect.
    fn dragged(
        &self,
        s: &Session<'_>,
        w: &Window,
        log: &str,
    ) -> Result<(desk::Rect, (i32, i32), String), String> {
        let read = || self.up.ssh.run(&format!("type \"{log}\"")).text;
        let source = within("the drag source's window", || {
            read()
                .lines()
                .find_map(|l| l.trim().strip_prefix("source at="))
                .and_then(|r| {
                    let mut it = r.split(',').map(|n| n.trim().parse::<i32>().ok());
                    Some(desk::Rect {
                        l: it.next()??,
                        t: it.next()??,
                        r: it.next()??,
                        b: it.next()??,
                    })
                })
                .ok_or_else(|| "not shown yet".to_string())
        })?;
        if w.rect.holds(source.centre()) || source.holds(w.rect.centre()) {
            return Err(format!(
                "the drag source {source:?} overlaps aterm's window {:?}",
                w.rect
            ));
        }
        press_to(
            self.l(),
            &self.display,
            source.centre(),
            w.rect.centre(),
            60,
        )?;
        let over = within("aterm's window answering the drag under the cursor", || {
            accepted_over(&read(), &w.rect, &source)
        })
        .map_err(|e| format!("{e}; {}", Self::holds(s)))?;
        release(self.l())?;
        let effect = within("the drag's end", || {
            read()
                .lines()
                .find_map(|l| l.trim().strip_prefix("dropped effect=").map(str::to_string))
                .ok_or_else(|| "the source has not ended its drag".to_string())
        })
        .map_err(|e| format!("{e}; {}", Self::holds(s)))?;
        if effect != "Copy" {
            return Err(format!(
                "the drop ended with effect {effect}, not Copy: aterm refused it"
            ));
        }
        Ok((source, over, effect))
    }

    /// Type [`width_typed`]`(tag)` and await PowerShell's `<tag><cols>`.
    fn shell_width(&self, s: &Session<'_>, tag: &str, cols: u32) -> Result<String, String> {
        type_text(self.l(), &width_typed(tag))?;
        let echo = format!("{tag}{cols}");
        s.await_text(&echo)
            .map_err(|e| format!("{e}; {}", Self::holds(s)))?;
        Ok(echo)
    }

    fn resize(&self, s: &Session<'_>) -> Result<String, String> {
        let w = s.window()?;
        let before = s.dims()?;
        let resizes_before = count(&s.ctl(&["status"])?, "resizes").unwrap_or(0);
        let y = w.rect.centre().1;
        let edge = w.rect.r - EDGE_IN;
        drag(self.l(), &self.display, (edge, y), (edge - DRAG, y))?;
        let after = within("the grid narrower after the drag", || {
            let d = s.dims()?;
            if d.window_cols + 10 < before.window_cols {
                Ok(d)
            } else {
                Err(format!("window_cols {}", d.window_cols))
            }
        })?;
        let status = s.ctl(&["status"])?;
        let resizes = count(&status, "resizes").unwrap_or(0);
        if field(&status, "render") != Some("ok") || resizes <= resizes_before {
            return Err(format!(
                "after the drag `status` says render={} resizes={resizes} (was {resizes_before})",
                field(&status, "render").unwrap_or("?")
            ));
        }
        let echo = self.shell_width(s, WIDTH_TAGS[0], after.cols)?;
        let w2 = s.window()?;
        let detail = format!(
            "right edge dragged {DRAG} px left: window {}x{} -> {}x{}, grid {}x{} -> {}x{}, resizes {resizes_before} -> \
             {resizes}, render=ok; PowerShell's [Console]::WindowWidth printed {echo}; {}",
            w.client.0,
            w.client.1,
            w2.client.0,
            w2.client.1,
            before.cols,
            before.rows,
            after.cols,
            after.rows,
            self.shot("resized", Some(GPU_ID))
        );
        self.and_held(s, "resized", detail)
    }

    fn dpi_change(&self, s: &Session<'_>) -> Result<String, String> {
        let before = s.dims()?;
        let w0 = s.window()?;
        let m = self.scale(SCALED)?;
        let after = within("aterm at scale 1.50", || {
            let d = s.dims()?;
            if d.scale == "1.50" {
                Ok(d)
            } else {
                Err(format!("scale {}", d.scale))
            }
        })?;
        let w = within("its window at the monitor's dpi", || {
            let w = s.window()?;
            if w.dpi == m.dpi {
                Ok(w)
            } else {
                Err(format!("window dpi {} (monitor {})", w.dpi, m.dpi))
            }
        })?;
        let status = s.ctl(&["status"])?;
        if field(&status, "render") != Some("ok") {
            return Err(format!("after the change `status` says {status}"));
        }
        let echo = self.shell_width(s, WIDTH_TAGS[1], after.cols)?;
        let detail = format!(
            "primary monitor's scaling to step {SCALED} ({} dpi) while running: {} -> {}; window {} -> {} dpi, \
             client {}x{} -> {}x{}; render=ok; typed after it, [Console]::WindowWidth printed {echo}; {}",
            m.dpi,
            before.brief(),
            after.brief(),
            w0.dpi,
            w.dpi,
            w0.client.0,
            w0.client.1,
            w.client.0,
            w.client.1,
            self.shot("dpi-150", Some(GPU_ID))
        );
        self.and_held(s, "dpi-150", detail)
    }

    fn second_monitor(&self, s: &Session<'_>) -> Result<String, String> {
        let w = s.window()?;
        if !w.foreground {
            click(self.l(), &self.display, w.rect.centre())?;
        }
        chord(self.l(), &["meta_l", "shift", "right"])?;
        let second = &self.display.second;
        let (w2, d2) = within("Win+Shift+Right onto the second monitor", || {
            let w = s.window()?;
            let d = s.dims()?;
            if second.rect.holds(w.rect.centre()) && w.dpi == second.dpi && d.scale == "1.00" {
                Ok((w, d))
            } else {
                Err(format!(
                    "window {:?} at {} dpi, scale {}",
                    w.rect, w.dpi, d.scale
                ))
            }
        })?;
        let right = self.shell_width(s, WIDTH_TAGS[2], d2.cols)?;
        let there = self.shot("second-monitor", None);
        let there = self.and_held(s, "second-monitor", there)?;
        chord(self.l(), &["meta_l", "shift", "left"])?;
        let (w3, d3) = within("Win+Shift+Left back to the scaled primary", || {
            let w = s.window()?;
            let d = s.dims()?;
            if w.rect.l < second.rect.l && w.dpi > second.dpi && d.scale == "1.50" {
                Ok((w, d))
            } else {
                Err(format!(
                    "window {:?} at {} dpi, scale {}",
                    w.rect, w.dpi, d.scale
                ))
            }
        })?;
        let status = s.ctl(&["status"])?;
        if field(&status, "render") != Some("ok") {
            return Err(format!("after the moves `status` says {status}"));
        }
        let left = self.shell_width(s, WIDTH_TAGS[3], d3.cols)?;
        let back = self.and_held(s, "back", String::new())?;
        Ok(format!(
            "Win+Shift+Right: window at ({},{}) on {} at {} dpi, {}, [Console]::WindowWidth printed {right}; \
             Win+Shift+Left: back at {} dpi, {}, printed {left}{back}; render=ok; {there}",
            w2.rect.l,
            w2.rect.t,
            second.name,
            w2.dpi,
            d2.brief(),
            w3.dpi,
            d3.brief()
        ))
    }

    fn close(&self, s: &Session<'_>) -> Result<String, String> {
        let w = s.window()?;
        if !w.foreground {
            click(self.l(), &self.display, w.rect.centre())?;
        }
        let metrics = s.ctl(&["metrics", "percentiles"])?;
        let summary = format!("{}; {}", present_summary(&metrics), input_summary(&metrics));
        chord(self.l(), &["alt", "f4"])?;
        let code = within("aterm-gui's exit after Alt+F4", || {
            s.exit().ok_or_else(|| "still running".to_string())
        })?;
        let stderr = s.stderr();
        if code != 0 {
            return Err(format!(
                "aterm-gui exited {code}: {}",
                crate::util::one_line(&stderr, 400)
            ));
        }
        if let Some(line) = stderr.lines().find(|l| l.contains("panicked")) {
            return Err(format!("aterm-gui panicked: {}", line.trim()));
        }
        Ok(format!(
            "Alt+F4: aterm-gui exited 0, no panic on stderr; {summary}"
        ))
    }
}

fn arch_rows(
    ask: &Ask,
    up: &Up,
    arch: Arch,
    repo: &Path,
    display: &Display,
    results: &Path,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let tag = if ask.cpu { "window --cpu" } else { "window" };
    let name = |what: &str| format!("{} {tag} {what}", arch.name());
    let mut env = up.cargo_env(arch);
    env.extend(windows::build_facts(repo));
    let build = ["cargo", "build", "-p", "aterm-gui", "-p", "aterm-ctl"].map(String::from);
    println!("windows: {} ({}) build …", arch.name(), arch.runs());
    let out = up.ssh.streamed(
        &guest::exec_command(guest::SRC, &env, &build),
        &format!("windows: {}: ", arch.name()),
    );
    if !out.ok() {
        rows.push((
            name("build"),
            Err(format!("cargo build failed: {}", out.tail(10))),
        ));
        return rows;
    }
    rows.push((
        name("build"),
        Ok(format!("aterm-gui, aterm-ctl built ({:.0} s)", out.secs)),
    ));
    let run = Run {
        up,
        arch,
        display: display.clone(),
        results: results.to_path_buf(),
        watch: ask.watch,
        cpu: ask.cpu,
    };
    let mut push = |row: &str, verdict: Result<String, String>| {
        let ok = verdict.is_ok();
        let line = (name(row), verdict);
        match &line.1 {
            Ok(d) => println!("PASS {}: {d}", line.0),
            Err(e) => println!("FAIL {}: {e}", line.0),
        }
        rows.push(line);
        ok
    };
    println!(
        "windows: {} windowed aterm-gui in lane's desktop …",
        arch.name()
    );
    let mut left: &[&str] = &ROWS;
    match Session::launch(up, arch, 1, ask.cpu) {
        Err(e) => {
            push(ROWS[0], Err(e));
            left = &ROWS[1..];
        }
        Ok(s) => {
            let steps: [&Step<'_>; 11] = [
                &|s| {
                    run.launch(s, "1.00", display.primary.dpi)
                        .map(|d| format!("{d}; {}", run.shot("launch", Some(GPU_ID))))
                        .and_then(|d| run.and_held(s, "launch", d))
                },
                &|s| run.typed(s),
                &|s| run.ime(s),
                &|s| run.dead_keys(s),
                &|s| run.altgr(s),
                &|s| run.resize(s),
                &|s| run.modal_resize(s),
                &|s| run.drop(s),
                &|s| run.dpi_change(s),
                &|s| run.second_monitor(s),
                &|s| run.close(s),
            ];
            let mut failed = false;
            for (i, step) in steps.iter().enumerate() {
                left = &ROWS[i + 1..];
                if !push(ROWS[i], step(&s)) {
                    failed = true;
                    break;
                }
            }
            s.end();
            if !failed {
                let at150 = Session::launch(up, arch, 2, ask.cpu).and_then(|s2| {
                    let verdict = run.launch(&s2, "1.50", 144).and_then(|d| {
                        let shot = run.shot("launch-150", Some(GPU_ID));
                        let d = run.and_held(&s2, "launch-150", format!("{d}; {shot}"))?;
                        let d = if run.cpu {
                            format!("{d}; {}", run.exposed(&s2, "launch-150")?)
                        } else {
                            d
                        };
                        run.close(&s2).map(|c| format!("{d}; {c}"))
                    });
                    s2.end();
                    verdict
                });
                push(ROWS[11], at150);
                left = &[];
            }
        }
    }
    for row in left {
        rows.push((name(row), Err("not run: a row before it failed".into())));
    }
    if let Err(e) = run.scale(0) {
        eprintln!("aterm-lab: windows: the primary monitor's scaling was not put back: {e}");
    }
    rows
}

/// What `window` was asked.
#[derive(Debug, PartialEq, Eq)]
struct Ask {
    arches: Vec<Arch>,
    /// `--watch`: the IME row's input watched ([`Watch`]), its log kept.
    watch: bool,
    /// `--cpu`: run on the CPU presenter and compare window pixels with the presented frame.
    cpu: bool,
}

fn parse_args(args: &[String]) -> Result<Ask, Fail> {
    let mut arches = Vec::new();
    let mut watch = false;
    let mut cpu = false;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--arch" => {
                let w = args.get(i + 1).map_or("", String::as_str);
                arches.push(Arch::parse(w).ok_or_else(|| {
                    Fail::usage(format!("windows window: --arch {w}: aarch64 or x86_64"))
                })?);
                i += 2;
            }
            "--watch" => {
                watch = true;
                i += 1;
            }
            "--cpu" => {
                cpu = true;
                i += 1;
            }
            other => {
                return Err(Fail::usage(format!(
                    "windows window: unknown argument '{other}' ({LAB} windows help)"
                )));
            }
        }
    }
    if arches.is_empty() {
        arches = vec![Arch::Aarch64, Arch::X86_64];
    }
    Ok(Ask { arches, watch, cpu })
}

/// `windows window [--arch A]... [--watch] [--cpu]`.
/// # Errors
/// A usage error, or the lane could not be brought up (not run).
pub fn cmd_window(args: &[String]) -> Res {
    let ask = parse_args(args)?;
    let l = Layout::find()?;
    let _lock = windows::boot_lane(&l)?;
    let results = paths().need_state()?.join("windows");
    std::fs::create_dir_all(&results)
        .map_err(|e| Fail::die(format!("{}: {e}", results.display())))?;
    let up = windows::bring_up(false)?;
    let repo = crate::paths::physical(&paths().repo);
    if let Err(e) = windows::copy_tree(&up.layout, &up.ssh, &repo) {
        up.release().map_err(Fail::die)?;
        return Err(not_run(format!("the checkout did not copy: {e}")));
    }
    let helper = up.layout.run().join("desk.ps1");
    let put = std::fs::write(&helper, desk::HELPER)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            let made = up
                .ssh
                .run(&format!("if not exist {d} mkdir {d}", d = desk::DESK));
            let put = up.ssh.put(&helper, &desk::HELPER_PATH.replace('\\', "/"));
            if made.ok() && put.ok() {
                Ok(())
            } else {
                Err(format!("{} {}", made.tail(2), put.tail(2)))
            }
        });
    let _ = std::fs::remove_file(&helper);
    if let Err(e) = put {
        up.release().map_err(Fail::die)?;
        return Err(not_run(format!("the desk helper did not copy: {e}")));
    }
    let mut rows: Vec<Row> = Vec::new();
    let t0 = Instant::now();
    match set_display(&up) {
        Err(e) => rows.push(("display".into(), Err(e))),
        Ok(display) => {
            rows.push(("display".into(), Ok(display_detail(&display))));
            for &arch in &ask.arches {
                rows.extend(arch_rows(&ask, &up, arch, &repo, &display, &results));
            }
        }
    }
    let cleanup = up.release();
    let tree = windows::build_facts(&repo)
        .into_iter()
        .find(|(k, _)| k == "ATERM_BUILD_GIT_COMMIT")
        .map_or_else(|| "unknown".to_string(), |(_, v)| v);
    let mut report = format!("tree {tree}\n");
    let mut failed = 0;
    for (row, verdict) in &rows {
        let line = match verdict {
            Ok(d) => format!("PASS {row}: {d}"),
            Err(e) => {
                failed += 1;
                format!("FAIL {row}: {e}")
            }
        };
        report.push_str(&line);
        report.push('\n');
    }
    print!("{report}");
    let path = results.join(if ask.cpu {
        "window-cpu.txt"
    } else {
        "window.txt"
    });
    let _ = std::fs::write(&path, &report);
    println!(
        "windows: {} rows, {failed} failed, {:.0} s, tree {tree}; report {}",
        rows.len(),
        t0.elapsed().as_secs_f64(),
        path.display()
    );
    windows::released(i32::from(failed > 0), cleanup)
}

/// `text` as an `await match` pattern that matches it as written: `await match` takes a regular
/// expression, so `text` is escaped (the AltGr row's `[]` is syntax unescaped), and the control
/// protocol splits its words on whitespace, so a space is `\s` (the drop row's `DND-drop 42.txt`).
fn literal(text: &str) -> String {
    text.split(' ')
        .map(aterm_regex::escape)
        .collect::<Vec<_>>()
        .join("\\s")
}

/// A window's style words and what DWM answers for its backdrop and dark mode, as the launch rows
/// report them (`-` where DWM did not answer).
fn styles(w: &desk::Window) -> String {
    styles_of(Styles::of(w))
}

fn styles_of(s: Styles) -> String {
    let dwm = |v: Option<i32>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
    format!(
        "style {:08x} exstyle {:08x}, DWM backdrop {} dark {}",
        s.style,
        s.exstyle,
        dwm(s.backdrop),
        dwm(s.dark)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w` x `h` RGBA frame whose pixels all differ, and the RGB readback of a window that shows it.
    fn frame_and_window(w: usize, h: usize) -> (Vec<u8>, Vec<u8>) {
        let frame: Vec<u8> = (0..w * h)
            .flat_map(|i| [i as u8, 0x10, 0x20, 0xff])
            .collect();
        let window = frame
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        (frame, window)
    }

    #[test]
    fn a_hold_on_the_screens_part_ignores_the_rows_past_its_edge_and_no_others() {
        let (w, h) = (3, 4);
        let (frame, mut window) = frame_and_window(w, h);
        window[3 * w * 3..].fill(0xff);
        let place =
            desk::parse_place("readback client=3x4 at=10,7 screen=0,0,100,10").expect("a place");
        let on = place.on_monitors(&[place.screen]);
        assert_eq!(
            on,
            vec![desk::Rect {
                l: 0,
                t: 0,
                r: 3,
                b: 3
            }]
        );
        assert!(compare_rgb(&frame, w, h, &(window.clone(), w, h), 0..0).is_err());
        let framed = (frame.clone(), w, h);
        let mut held = window.clone();
        let (put, span) = off_monitors_as_framed(&mut held, (w, h), &on, &framed, (0, 0));
        assert_eq!(
            (put, span),
            (
                3,
                Some(desk::Rect {
                    l: 0,
                    t: 3,
                    r: 3,
                    b: 4
                })
            )
        );
        assert_eq!(compare_rgb(&frame, w, h, &(held, w, h), 0..0), Ok(()));
        window[w * 3 + 1] ^= 1;
        let mut held = window;
        off_monitors_as_framed(&mut held, (w, h), &on, &framed, (0, 0));
        assert_eq!(
            compare_rgb(&frame, w, h, &(held, w, h), 0..0),
            Err(
                "1 of 12 pixels differ; first at (0,1): presented [03, 10, 20], server [03, 11, 20]"
                    .into()
            )
        );
    }

    /// The second monitor row's place on 2026-10-25: back on the primary, the client area reached
    /// past its right edge and below the smaller second monitor beside it. Scaled down, a 10x10
    /// primary with a 4x5 monitor to its right, and a 6x8 client area at (6,1).
    #[test]
    fn a_hold_ignores_the_pixels_between_the_monitors_and_no_others() {
        let monitors = [
            desk::Rect {
                l: 0,
                t: 0,
                r: 10,
                b: 10,
            },
            desk::Rect {
                l: 10,
                t: 0,
                r: 14,
                b: 5,
            },
        ];
        let place =
            desk::parse_place("readback client=6x8 at=6,1 screen=0,0,14,10").expect("a place");
        assert_eq!(
            place.on_screen(),
            Some(desk::Rect {
                l: 0,
                t: 0,
                r: 6,
                b: 8
            }),
            "all of it is on the virtual screen"
        );
        let on = place.on_monitors(&monitors);
        assert_eq!(
            on,
            vec![
                desk::Rect {
                    l: 0,
                    t: 0,
                    r: 4,
                    b: 8
                },
                desk::Rect {
                    l: 4,
                    t: 0,
                    r: 6,
                    b: 4
                }
            ]
        );
        let (w, h) = (6, 8);
        let (frame, mut window) = frame_and_window(w, h);
        let framed = (frame.clone(), w, h);
        // What Windows left between the monitors: the theme's background, not the frame.
        for y in 4..8 {
            for x in 4..6 {
                window[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[0x11, 0x13, 0x18]);
            }
        }
        assert!(compare_rgb(&frame, w, h, &(window.clone(), w, h), 0..0).is_err());
        let mut held = window.clone();
        let (put, span) = off_monitors_as_framed(&mut held, (w, h), &on, &framed, (0, 0));
        assert_eq!(
            (put, span),
            (
                8,
                Some(desk::Rect {
                    l: 4,
                    t: 4,
                    r: 6,
                    b: 8
                })
            )
        );
        assert_eq!(compare_rgb(&frame, w, h, &(held, w, h), 0..0), Ok(()));
        // A wrong pixel beside the gap, on the primary, and one on the second monitor above it.
        for at in [(3, 6), (5, 3)] {
            let mut planted = window.clone();
            planted[(at.1 * w + at.0) * 3] ^= 1;
            off_monitors_as_framed(&mut planted, (w, h), &on, &framed, (0, 0));
            let got = compare_rgb(&frame, w, h, &(planted, w, h), 0..0);
            assert!(
                got.as_ref()
                    .is_err_and(|e| e.contains(&format!("first at ({},{})", at.0, at.1))),
                "{at:?}: {got:?}"
            );
        }
    }

    /// A frame smaller than the client area sits at `dims`' offset and its bands hold its nearest
    /// pixels; a pixel on no monitor is put to what that rule expects there, and no other.
    #[test]
    fn a_hold_off_the_monitors_puts_the_frames_nearest_pixel_at_an_offset() {
        let (w, h) = (4, 6);
        let (cw, ch) = (6, 6);
        let (frame, _) = frame_and_window(w, h);
        let framed = (frame.clone(), w, h);
        let (ox, oy) = (1, 0);
        let nearest = |x: usize, y: usize| {
            let nx = (x as i64 - ox).clamp(0, w as i64 - 1) as usize;
            let ny = (y as i64 - oy).clamp(0, h as i64 - 1) as usize;
            [
                frame[(ny * w + nx) * 4],
                frame[(ny * w + nx) * 4 + 1],
                frame[(ny * w + nx) * 4 + 2],
            ]
        };
        let mut window: Vec<u8> = (0..ch)
            .flat_map(|y| (0..cw).map(move |x| (x, y)))
            .flat_map(|(x, y)| nearest(x, y))
            .collect();
        let on = [
            desk::Rect {
                l: 0,
                t: 0,
                r: 4,
                b: 6,
            },
            desk::Rect {
                l: 4,
                t: 0,
                r: 6,
                b: 3,
            },
        ];
        for y in 3..6 {
            for x in 4..6 {
                window[(y * cw + x) * 3..(y * cw + x) * 3 + 3].fill(0xff);
            }
        }
        let rgba = |px: &[u8]| -> (Vec<u8>, usize, usize) {
            (
                px.as_chunks::<3>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2], 0xff])
                    .collect(),
                cw,
                ch,
            )
        };
        let view = WestonView {
            x: 0,
            y: 0,
            width: cw,
            height: ch,
            format: "XRGB8888".into(),
        };
        assert!(held_at(&rgba(&window), &framed, ox, oy, 0..0).is_err());
        let mut held = window.clone();
        let (put, span) = off_monitors_as_framed(&mut held, (cw, ch), &on, &framed, (ox, oy));
        assert_eq!(
            (put, span),
            (
                6,
                Some(desk::Rect {
                    l: 4,
                    t: 3,
                    r: 6,
                    b: 6
                })
            )
        );
        assert_eq!(held_at(&rgba(&held), &framed, ox, oy, 0..0), Ok((4, 6)));
        assert_eq!(bands_held(&rgba(&held), &framed, &view, (ox, oy)), Ok(12));
        // The band column 0 is on the primary: a wrong pixel there still fails.
        held[(2 * cw) * 3] ^= 1;
        assert!(bands_held(&rgba(&held), &framed, &view, (ox, oy)).is_err());
    }

    #[test]
    fn dims_are_read_from_a_windowed_reply() {
        let r = "OK 23 83 1079 828 session=0 cell_w=13 cell_h=36 font_px=26.00 scale=1.50 window=0 \
                 window_rows=23 window_cols=83 composed_rows=24 grid_w=1079 grid_h=828 frame_w=1115 frame_h=996 \
                 surface_w=1116 surface_h=996 geometry=window";
        let d = Dims::parse(r).unwrap_or_else(|| unreachable!());
        assert_eq!((d.rows, d.cols), (23, 83));
        assert_eq!((d.scale.as_str(), d.font_px.as_str()), ("1.50", "26.00"));
        assert_eq!((d.window_rows, d.window_cols), (23, 83));
        assert_eq!(d.frame, (1115, 996));
        assert_eq!(d.geometry, "window");
        assert_eq!(Dims::parse("ERR no session"), None);
        assert_eq!(Dims::parse("OK 23 83 scale=1.50"), None);
    }

    #[test]
    fn what_the_rows_type_has_keys_and_never_holds_its_echo() {
        let widths = WIDTH_TAGS.map(width_typed);
        for text in [TYPED, IME_BEFORE, IME_READING, IME_AFTER]
            .into_iter()
            .chain(widths.iter().map(String::as_str))
        {
            assert!(text.chars().all(|c| desk::qcodes(c).is_some()), "{text}");
        }
        assert!(IME_ECHO.chars().any(|c| desk::qcodes(c).is_none()));
        let typed = format!("{IME_BEFORE}{IME_READING}{IME_AFTER}");
        assert!(!typed.contains(IME_ECHO));
        assert!(format!("IME-{IME_READING}") != IME_ECHO);
        assert!(!EXPOSED_TYPED.contains(EXPOSED_ECHO));
        for (tag, typed) in WIDTH_TAGS.iter().zip(&widths) {
            assert!(
                !(0..10).any(|d| typed.contains(&format!("{tag}{d}"))),
                "{typed}"
            );
            for other in WIDTH_TAGS {
                assert!(other == *tag || !other.starts_with(tag), "{tag} {other}");
            }
        }
        for (before, after, typed, echo) in [
            (DEAD_BEFORE, DEAD_AFTER, DEAD_TYPED, DEAD_ECHO),
            (ALTGR_BEFORE, DEAD_AFTER, ALTGR_TYPED, ALTGR_ECHO),
        ] {
            assert!(
                format!("{before}{after}")
                    .chars()
                    .all(|c| desk::qcodes(c).is_some())
            );
            assert!(!format!("{before}{typed}{after}").contains(echo), "{echo}");
            assert!(echo.ends_with(typed), "{echo}");
        }
        assert!(DEAD_TYPED.chars().any(|c| desk::qcodes(c).is_none()));
        assert_eq!(ALTGR_KEYS.len(), ALTGR_TYPED.chars().count());
        assert!(
            format!("{DROP_BEFORE}{DROP_AFTER}")
                .chars()
                .all(|c| desk::qcodes(c).is_some())
        );
        let pasted =
            format!("{DROP_BEFORE}\"C:\\lane\\desk\\aarch64-1\\{DROP_FILE}\" {DROP_AFTER}");
        assert!(!pasted.contains(DROP_ECHO));
        assert!(DROP_ECHO.ends_with(DROP_FILE) && DROP_FILE.contains(' '));
        assert!(
            !format!("Write-Output ('{HELD_TAG}' + [Console]::WindowWidth)")
                .contains(&format!("{HELD_TAG}8"))
        );
        assert_eq!(IME_STEPS.len(), IME_READING.chars().count());
        assert!(IME_STEPS.iter().all(|step| !step.contains(IME_CONVERTED)));
        assert!(IME_STEPS.windows(2).all(|w| w[0] != w[1]));
    }

    #[test]
    fn every_echo_is_awaited_as_written() {
        let widths = WIDTH_TAGS.map(|tag| format!("{tag}80"));
        for echo in [
            EXPOSED_ECHO,
            crate::linux_proof::ECHO,
            IME_ECHO,
            DEAD_ECHO,
            ALTGR_ECHO,
            DROP_ECHO,
            &format!("{HELD_TAG}80"),
        ]
        .into_iter()
        .chain(widths.iter().map(String::as_str))
        {
            let pattern = literal(echo);
            assert!(!pattern.contains(char::is_whitespace), "{pattern}");
            let re = aterm_regex::Regex::new(&pattern).unwrap_or_else(|e| panic!("{echo}: {e:?}"));
            assert!(
                re.is_match(&format!("PS C:\\Users\\lane> {echo}")),
                "{echo}"
            );
        }
        // Negative controls: unescaped, the AltGr echo is no expression at all (the lane's
        // `ERR badregex`), escaped alone the drop echo keeps its space (which the wire splits, the
        // lane's next `ERR badregex`), and the drop's `.` matches a byte it does not hold.
        assert!(aterm_regex::Regex::new(ALTGR_ECHO).is_err());
        assert!(aterm_regex::escape(DROP_ECHO).contains(' '));
        let dot = aterm_regex::Regex::new(DROP_ECHO).unwrap_or_else(|e| panic!("{e:?}"));
        let near = DROP_ECHO.replace('.', "x");
        assert!(dot.is_match(&near));
        let escaped =
            aterm_regex::Regex::new(&literal(DROP_ECHO)).unwrap_or_else(|e| panic!("{e:?}"));
        assert!(!escaped.is_match(&near));
    }

    #[test]
    fn the_drop_is_released_only_on_aterms_copy_under_the_cursor() {
        let aterm = desk::Rect {
            l: 100,
            t: 50,
            r: 1200,
            b: 950,
        };
        let source = desk::Rect {
            l: 1600,
            t: 900,
            r: 1860,
            b: 1060,
        };
        let start = "source at=1600,900,1860,1060\n";
        let crossing = "over effect=None at=1700,950\nover effect=Copy at=1500,700\n";
        let on_aterm = "over effect=Copy at=650,500\n";
        assert_eq!(
            accepted_over(&format!("{start}{crossing}{on_aterm}"), &aterm, &source),
            Ok((650, 500))
        );
        // Negative controls: no answer yet; Copy from a window the path crossed (the desktop);
        // aterm's window refusing; and the source's own label, where it overlaps aterm's.
        assert!(accepted_over(start, &aterm, &source).is_err());
        assert!(accepted_over(&format!("{start}{crossing}"), &aterm, &source).is_err());
        assert!(
            accepted_over(
                &format!("{start}{crossing}over effect=None at=650,500\n"),
                &aterm,
                &source
            )
            .is_err()
        );
        let over_aterm = desk::Rect {
            l: 600,
            t: 450,
            r: 860,
            b: 610,
        };
        assert!(accepted_over(&format!("{start}{on_aterm}"), &aterm, &over_aterm).is_err());
        // The last answer is the one that stands: the cursor left aterm's window after it.
        assert!(
            accepted_over(
                &format!("{start}{on_aterm}over effect=None at=1700,950\n"),
                &aterm,
                &source
            )
            .is_err()
        );
        assert!(desk::HELPER.contains("\"over effect={0} at={1},{2}\""));
    }

    #[test]
    fn the_launch_rows_report_the_style_words_and_dwm() {
        let w = desk::parse_windows(
            "window class=aterm rect=0,0,10,10 client=8x8 dpi=96 foreground=1 awareness=2 layout=0409 ime=0 \
             machine=aa64 conv=0 movesize=0 style=16cf0000 exstyle=00200100 backdrop=0 dark=-",
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(
            styles(&w[0]),
            "style 16cf0000 exstyle 00200100, DWM backdrop 0 dark -"
        );
        // The recorded words hold the window they were read from (the before-run's launch line) and
        // not another: this one's extended style, backdrop and dark mode differ.
        let recorded = desk::parse_windows(
            "window class=aterm rect=0,0,10,10 client=8x8 dpi=96 foreground=1 awareness=2 layout=0409 ime=0 \
             machine=aa64 conv=0 movesize=0 style=16cf0000 exstyle=00040110 backdrop=1 dark=1",
        );
        assert_eq!(Styles::of(&recorded[0]), LAUNCH_STYLES);
        assert_ne!(Styles::of(&w[0]), LAUNCH_STYLES);
        assert_eq!(
            styles_of(LAUNCH_STYLES),
            "style 16cf0000 exstyle 00040110, DWM backdrop 1 dark 1"
        );
    }

    #[test]
    fn window_takes_arches_and_nothing_else() {
        assert_eq!(
            parse_args(&[]).ok(),
            Some(Ask {
                arches: vec![Arch::Aarch64, Arch::X86_64],
                watch: false,
                cpu: false
            })
        );
        assert_eq!(
            parse_args(&["--arch".into(), "x86_64".into(), "--watch".into()]).ok(),
            Some(Ask {
                arches: vec![Arch::X86_64],
                watch: true,
                cpu: false
            })
        );
        assert_eq!(
            parse_args(&["--cpu".into()]).ok(),
            Some(Ask {
                arches: vec![Arch::Aarch64, Arch::X86_64],
                watch: false,
                cpu: true
            })
        );
        assert!(parse_args(&["--arch".into(), "mips".into()]).is_err());
        assert!(parse_args(&["--vnc".into()]).is_err());
        let r = desk::Rect {
            l: 0,
            t: 0,
            r: 1920,
            b: 1200,
        };
        assert!(r.holds((1919, 1199)) && !r.holds((1920, 0)));
    }
}
