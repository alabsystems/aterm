// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `windows window [--arch A]...`: WB.3's runtime smoke (docs/AUDIT-third-
//! party-2026-10-15.md, section 7), a windowed `aterm-gui` in the runner's
//! desktop session of the lane's VM ([`crate::windows`]), driven as a person
//! drives it: by the VM's USB keyboard and tablet through QMP, so every key
//! and click goes through Windows' own input stack, focus, IME and window
//! manager. It is what the windows-sys 0.61 facade over `aterm-win32` and the
//! winit fork's handle conversions are judged by at runtime.
//!
//! Once per run, the display: the virtio-gpu's monitor
//! ([`crate::windows::VIRTIO_GPU`], 1920x1200, display scaling 100-200 %)
//! made primary at (0,0) at 100 %, ramfb's 800x600 to its right. Then per
//! arch (x86_64 under Prism):
//!
//! 1. `cargo build -p aterm-gui -p aterm-ctl`;
//! 2. launch: one `aterm-gui` (scratch config, automatic lanes off), its
//!    window of class `aterm` per-monitor DPI aware at 96 dpi on the
//!    primary, `geometry=window`, drawing on the GPU on WARP;
//! 3. type: clicked, [`TYPED`] typed on the keyboard, its echo awaited;
//! 4. IME: Win+Space to Microsoft's Japanese IME, the IME opened, `nihon`
//!    composed, converted (Space) and committed (Enter) into a PowerShell
//!    line that prints `IME-日本` only when the committed text reached it;
//! 5. resize: the window's right edge dragged 180 px left, the grid narrower,
//!    and PowerShell's `[Console]::WindowWidth` printing the new width;
//! 6. DPI: the primary monitor's scaling set to 150 % while aterm runs: its
//!    scale 1.50 and its window at 144 dpi, still rendering, still typing;
//! 7. monitor: Win+Shift+Right to the 96-dpi second monitor (scale 1.00),
//!    Win+Shift+Left back (1.50);
//! 8. close: Alt+F4, exit status 0, no panic;
//! 9. launch at 150 %: a second `aterm-gui` started on the scaled monitor at
//!    1.50 and 144 dpi, closed the same way. With `--cpu`, before it closes,
//!    its window is lifted until 120 client rows are past the screen's top
//!    edge, PowerShell clears its screen and prints a line there, and the
//!    window is moved back down onto the screen ([`Run::exposed`]): the rows
//!    that come back, which Windows did not draw, must be painted.
//!
//! The monitor's scaling is put back to 100 % at the end. Exit 0 every row
//! passed, 1 a row failed, 3 NOT RUN. Report and screenshots:
//! `$LAB_STATE/windows/`.
//!
//! `--cpu` runs the same rows on aterm's CPU presenter (`aterm-gui --cpu`,
//! the cursor still and the stats Deck off, as the Linux lane's `--cpu` rows
//! run): the launch row requires `backend=cpu`, and the launch, type, IME,
//! resize, DPI and monitor rows each hold the frame aterm presented
//! (`image`) to the window's own pixels, which the desk helper reads back
//! from its DC (`readback`: `BitBlt` into a bitmap, then `GetDIBits`). WARP only: the VM has no GPU, so a real GPU's
//! driver, and real x86_64 hardware, are m6's to run.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::linux_proof::{
    WestonView, bands_held, compare_rgb, drew_on_gpu, field, held_at, parse_ppm, present_summary,
    reply,
};
use crate::paths::paths;
use crate::windows::{self, GPU_ID, Layout, Up, lane_lock, not_run, run_on_desktop, task_name};
use crate::windows_desk::{
    self as desk, ATERM_CLASS, Monitor, VIRTIO_ADAPTER, Window, helper_line, instance_dir,
};
use crate::windows_guest::{self as guest, Arch, Desktop};
use crate::windows_smoke::{TYPED, WARP, bin_dir};
use crate::{Fail, LAB, Res};

/// How long something that must happen may take: a hang detector, never a
/// latency budget.
const WITHIN: Duration = Duration::from_secs(60);

/// How many client rows [`Run::exposed`] lifts past the screen's top edge:
/// PowerShell's first three lines at 150 %.
const LIFT: i32 = 120;

/// What [`Run::exposed`] has PowerShell do while the window's top rows are
/// past the screen's edge: clear the screen and print [`EXPOSED_ECHO`] on its
/// first line, which no key typed shows.
pub const EXPOSED_TYPED: &str = "Clear-Host; Write-Output ('ROWS-' + 6*7)";
pub const EXPOSED_ECHO: &str = "ROWS-42";

/// The `r` part of a `w`-pixel-wide image of `n`-byte pixels: its pixels, width
/// and height.
fn crop(px: &[u8], w: usize, n: usize, r: desk::Rect) -> (Vec<u8>, usize, usize) {
    let col = |v: i32| usize::try_from(v).unwrap_or(0);
    let (l, t, rr, b) = (col(r.l), col(r.t), col(r.r), col(r.b));
    let mut out = Vec::with_capacity((rr - l) * (b - t) * n);
    for y in t..b {
        out.extend_from_slice(&px[(y * w + l) * n..(y * w + rr) * n]);
    }
    (out, rr - l, b - t)
}

/// What the IME row types around the composition, and what PowerShell
/// prints only when the IME's committed text (`日本`, from `nihon`) reached it.
pub const IME_BEFORE: &str = "Write-Output ('IME-' + '";
pub const IME_READING: &str = "nihon";
pub const IME_AFTER: &str = "')\n";
pub const IME_ECHO: &str = "IME-日本";

/// aterm's composition (`chrome`'s `ime` line) after each key of
/// [`IME_READING`] (a romaji letter that waits for the next key shows
/// full-width), and once Space converted it.
pub const IME_STEPS: [&str; 5] = ["ｎ", "に", "にｈ", "にほ", "にほｎ"];
pub const IME_COMPOSING: &str = IME_STEPS[4];
pub const IME_CONVERTED: &str = "日本";

/// The IME conversion mode bits the reading needs (`IME_CMODE_NATIVE`,
/// `IME_CMODE_KATAKANA`, `IME_CMODE_FULLSHAPE`) and their hiragana values:
/// native and full-width, not katakana.
pub const CMODE_MASK: u32 = 0x1 | 0x2 | 0x8;
pub const CMODE_HIRAGANA: u32 = 0x1 | 0x8;

/// What a row types to read the console width PowerShell sees, after
/// `tag`: ConPTY's view of a re-grid, which is what the shell lays its
/// lines out by, not aterm's own count.
#[must_use]
pub fn width_typed(tag: &str) -> String {
    format!("Write-Output ('{tag}' + [Console]::WindowWidth)\n")
}

/// The tags the resize, DPI change and monitor rows read the width under,
/// each its own, so no row's echo is an earlier row's.
pub const WIDTH_TAGS: [&str; 4] = ["W=", "DPI-", "RIGHT-", "LEFT-"];

/// How far the resize row drags the window's right edge left, in pixels,
/// from inside Windows' invisible resize border (GetWindowRect's right edge
/// minus [`EDGE_IN`]).
pub const DRAG: i32 = 180;
pub const EDGE_IN: i32 = 4;

/// The scaling steps the DPI rows set: 150 % (each step is 25 %).
pub const SCALED: i32 = 2;

/// Microsoft's Japanese IME's keyboard layout language, and English (US).
pub const JAPANESE: &str = "0411";
pub const ENGLISH: &str = "0409";

/// The display a run drives: the virtio-gpu's monitor (primary, the
/// tablet's) and ramfb's.
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

/// Wait (a hang detector) until `probe` holds: its value, or what it last
/// saw.
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

/// Put the display as the run wants it: the virtio-gpu's monitor primary at
/// (0,0) at its recommended scaling (100 %), ramfb's to its right.
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

// ---------------------------------------------------------------------------
// The VM's keyboard, tablet and screens

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

/// Drag with the tablet's left button from `from` to `to` on the primary
/// monitor, in steps, as a hand does.
fn drag(l: &Layout, d: &Display, from: (i32, i32), to: (i32, i32)) -> Result<(), String> {
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
        std::thread::sleep(Duration::from_millis(30));
    }
    std::thread::sleep(Duration::from_millis(200));
    qmp(l, desk::tablet_button(false))?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}

/// A screenshot of `device`'s screen (the virtio-gpu's by default; `None`
/// is ramfb's) at `path`: the window as the monitor shows it, chrome, IME
/// windows and all.
fn shot(l: &Layout, path: &Path, device: Option<&str>) -> String {
    let p = path.to_string_lossy();
    match qmp(l, desk::screendump_command(&p, device)) {
        Ok(()) => format!("screenshot {}", path.display()),
        Err(e) => format!("no screenshot: {e}"),
    }
}

// ---------------------------------------------------------------------------
// One aterm-gui in the desktop

struct Session<'a> {
    up: &'a Up,
    arch: Arch,
    dir: String,
    task: String,
    pid: u32,
}

/// The config a `--cpu` run's instance takes: the cursor still (no blink,
/// no trail) and the stats Deck off, so the frame `image` photographs is the
/// one the window keeps showing, as the Linux lane's `--cpu` rows hold it.
fn cpu_config() -> String {
    format!(
        "cursor_blink = false\ncursor_trail = false\n{}{}",
        crate::instance::CONFIG_OFF,
        crate::linux_proof::DECK_OFF
    )
}

impl<'a> Session<'a> {
    /// Start instance `n` of `arch` in the runner's desktop (on the CPU
    /// presenter with `cpu`) and wait until it answers `ready`.
    fn launch(up: &'a Up, arch: Arch, n: u32, cpu: bool) -> Result<Session<'a>, String> {
        let dir = instance_dir(arch, n);
        let task = task_name(&format!("window-{}-{n}", arch.name()));
        // A previous run's pid and exit files must be gone before this one
        // starts: the helper removes the directory too, but only once its
        // task runs, after this run may have read them.
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

    /// `aterm-ctl` (over SSH, as the runner) to this instance: its reply
    /// when it exited 0 and the reply is `OK …`.
    fn ctl(&self, words: &[&str]) -> Result<String, String> {
        self.ask(words, true)
    }

    /// `aterm-ctl` to this instance: its reply when it exited 0 (and, when
    /// `ok`, starts `OK`; `text` and `chrome` answer without it).
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

    /// Its task stopped and gone, and aterm-gui with it (the administrator
    /// ends it: the runner may not end a process of its desktop session from
    /// SSH).
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

/// The machine `arch`'s process runs as (`ProcessMachineTypeInfo`'s
/// `IMAGE_FILE_MACHINE_*`, as the desk helper prints it).
#[must_use]
pub fn machine(arch: Arch) -> &'static str {
    match arch {
        Arch::Aarch64 => "aa64",
        Arch::X86_64 => "8664",
    }
}

/// A number field of a reply.
fn count(r: &str, key: &str) -> Option<u64> {
    field(r, key)?.parse().ok()
}

/// `frames` from `metrics` (every successful present; `n_present` counts
/// only output-to-present samples), the presents it dropped and why the last
/// one was, and the surface `dims` names, as the row's evidence: `?` for what
/// aterm did not answer.
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

/// The session's input as a windowless watcher ([`desk::HELPER`]'s `watch`)
/// logs it while a row runs: the foreground window and focus, aterm's
/// keyboard layout, IME open status and conversion mode, the keys held, the
/// windows that come and go.
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
        // Started is a running task, not yet a log: the helper compiles first.
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

// ---------------------------------------------------------------------------
// The rows

/// One row: its name, and a pass's detail or a failure's.
type Row = (String, Result<String, String>);

/// A row run on the instance: a pass's detail or a failure's.
type Step<'a> = dyn Fn(&Session<'_>) -> Result<String, String> + 'a;

/// The rows one arch's run went through, in order.
pub const ROWS: [&str; 8] = [
    "launch",
    "type",
    "IME",
    "resize",
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
    /// `--cpu`: the instances present on the CPU, and the rows hold the
    /// window's pixels to the presented frame ([`Run::held`]).
    cpu: bool,
}

impl Run<'_> {
    fn l(&self) -> &Layout {
        &self.up.layout
    }

    /// What this run's files are named after: `window`, or `window-cpu`.
    fn tag(&self) -> &'static str {
        if self.cpu { "window-cpu" } else { "window" }
    }

    fn shot(&self, what: &str, device: Option<&str>) -> String {
        // The virtio-gpu's screen is what the guest last flushed to it: give
        // the frame aterm just drew a moment to get there.
        std::thread::sleep(Duration::from_secs(1));
        shot(
            self.l(),
            &self
                .results
                .join(format!("{}-{}-{what}.png", self.arch.name(), self.tag())),
            device,
        )
    }

    /// With `--cpu`, `detail` and then what [`Run::held`] says of the
    /// window's pixels now; `detail` alone without it. A hold that fails
    /// says how many frames aterm has presented and at what surface size
    /// ([`presents`]), read once now and once a second later, which tells a
    /// window aterm stopped presenting to from one it presents wrong.
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

    /// The frame aterm presented (`image`, kept as
    /// `<arch>-window-cpu-<what>-frame.png`, apart from the row's screenshot)
    /// held to the window's own pixels, read back from its DC by the desk
    /// helper (`readback`, kept as `<arch>-window-cpu-<what>-window.ppm`):
    /// equal exactly when the frame is the client area's size, else the frame
    /// at `dims`' offset and each band pixel the frame's nearest, as the
    /// Linux lane holds a window the window system sized. What the row's
    /// detail says of it.
    ///
    /// Windows draws no window pixel outside the virtual screen: what the
    /// readback finds there is what the window held when that pixel was last
    /// on the screen, or white when it never was, not what aterm presented
    /// since. So a client area that reaches past the screen's edge is held on
    /// the part that is on it, and the detail names the part that is not.
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
        let seen = place.on_screen().ok_or_else(|| {
            format!(
                "the window's client area ({cw}x{ch} at {:?}) is off the virtual screen {:?}",
                place.at, place.screen
            )
        })?;
        let whole = desk::Rect {
            l: 0,
            t: 0,
            r: i32::try_from(cw).unwrap_or(i32::MAX),
            b: i32::try_from(ch).unwrap_or(i32::MAX),
        };
        if (cw, ch) == (w, h) && seen == whole {
            compare_rgb(&presented.0, w, h, &window, 0..0)
                .map_err(|why| format!("{why} ({files})"))?;
            return Ok(format!(
                "the window's {cw}x{ch} client pixels (GetDIBits) equal the presented frame exactly"
            ));
        }
        if (cw, ch) == (w, h) {
            let (rgba, vw, vh) = crop(&presented.0, w, 4, seen);
            let (rgb, _, _) = crop(&window.0, cw, 3, seen);
            compare_rgb(&rgba, vw, vh, &(rgb, vw, vh), 0..0).map_err(|why| {
                format!("on the screen's part {seen:?} of the client area: {why} ({files})")
            })?;
            return Ok(format!(
                "the window's {vw}x{vh} client pixels on the virtual screen (columns {}..{}, rows \
                 {}..{} of {cw}x{ch}; Windows draws none past its edge {:?}) equal the presented \
                 frame's exactly",
                seen.l, seen.r, seen.t, seen.b, place.screen
            ));
        }
        if seen != whole {
            return Err(format!(
                "the window's client area is {cw}x{ch}, the frame {w}x{h}, and only {seen:?} of \
                 the client area is on the virtual screen {:?}: not held ({files})",
                place.screen
            ));
        }
        let d = s.ctl(&["dims"])?;
        let num = |k: &str| field(&d, k).and_then(|v| v.parse::<i64>().ok());
        if (num("surface_w"), num("surface_h")) != (i64::try_from(cw).ok(), i64::try_from(ch).ok())
        {
            return Err(format!(
                "the window's client area is {cw}x{ch}, the frame {w}x{h}, and `dims` names another \
                 surface: {d} ({files})"
            ));
        }
        let (ox, oy) = (num("offset_x").unwrap_or(0), num("offset_y").unwrap_or(0));
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
        held_at(&rgba, &presented, ox, oy, 0..0).map_err(|why| format!("{why} ({files})"))?;
        let n = bands_held(&rgba, &presented, &view, (ox, oy))
            .map_err(|why| format!("{why} ({files})"))?;
        Ok(format!(
            "the window's {cw}x{ch} client pixels (GetDIBits) hold the presented {w}x{h} frame \
             exactly at ({ox},{oy}), and its {n} band pixels each the frame's nearest"
        ))
    }

    /// Where `s`'s window's client area is ([`desk::Place`]), after moving it
    /// (`move`, so the client area's top is at screen row `n`) or widening it
    /// (`grow`, by `n` pixels) when asked to.
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

    /// What aterm presented while rows of its window were past the screen's
    /// edge, painted once they come back. The window is lifted until its top
    /// [`LIFT`] client rows are above the virtual screen's top edge; there
    /// PowerShell clears the screen and prints [`EXPOSED_ECHO`] on those rows
    /// ([`EXPOSED_TYPED`], sent through `aterm-ctl`), which Windows does not
    /// draw, and the rows still on the screen are held. Then the window is
    /// moved back down until all of it is on the screen and held whole:
    /// Windows invalidates the rows that come back and erases them with the
    /// class's theme brush, and aterm must paint them although its frame has
    /// not changed since (`RedrawRequested` with the window owing a repaint,
    /// then the presenter's copy of the update rectangle). Rows that only
    /// moved, or that hold the theme's background, would come back right
    /// without aterm: the erase paints the background, and a row drawn before
    /// it went past the edge keeps its pixels.
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
        s.ctl(&["await", "match", EXPOSED_ECHO, "timeout=60000"])?;
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
             {}; drew on {device}",
            s.pid,
            w.machine,
            self.arch.runs(),
            w.client.0,
            w.client.1,
            w.dpi,
            dims.brief()
        ))
    }

    fn typed(&self, s: &Session<'_>) -> Result<String, String> {
        let w = s.window()?;
        let before = count(&s.ctl(&["metrics"])?, "n_input").unwrap_or(0);
        click(self.l(), &self.display, w.rect.centre())?;
        type_text(self.l(), &format!("{TYPED}\n"))?;
        s.ctl(&["await", "match", crate::linux_proof::ECHO, "timeout=60000"])?;
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

    /// aterm's composition as `chrome`'s `ime` line shows it (its text and
    /// caret).
    fn composition(s: &Session<'_>) -> Result<String, String> {
        let chrome = s.ask(&["chrome"], false)?;
        crate::linux_baseline::ime_line(&chrome)
            .map(|l| crate::linux_baseline::ime_composition(l).to_string())
            .ok_or_else(|| "`chrome` has no ime line".to_string())
    }

    /// What aterm holds when a row fails: its last screen line and its
    /// composition.
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
        // What aterm holds is read before English is put back: the switch
        // commits an open composition.
        let verdict = self
            .ime_steps(s)
            .map_err(|e| format!("{e}; {}", Self::holds(s)));
        // The session's input language is shared by every window of it: a
        // row that failed while Japanese was on puts English back, so the
        // instances after it (the other arch's, a later run's) start where
        // this one did.
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

    /// The window's keyboard layout made `want` (`0411`, `0409`): Win+Space,
    /// which steps through the session's two input languages, pressed only
    /// when it is not `want` already. Whether it was pressed.
    fn to_layout(&self, s: &Session<'_>, want: &str) -> Result<bool, String> {
        if s.window()?.layout == want {
            return Ok(false);
        }
        chord(self.l(), &["meta_l", "spc"])?;
        within(&format!("Win+Space to layout {want}"), || {
            let w = s.window()?;
            if w.layout == want {
                Ok(())
            } else {
                Err(format!("layout {}", w.layout))
            }
        })?;
        Ok(true)
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
        // Key by key, each awaited in aterm's composition: a key the IME
        // never got, or a composition the IME ended early, fails here with
        // the key it happened at.
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
        s.ctl(&["await", "match", IME_ECHO, "timeout=60000"])?;
        self.and_held(s, "ime", format!(
            "{switched} Microsoft IME (layout {JAPANESE}), IME {opened} (conversion mode {}), `{IME_READING}` composed \
             key by key ({} in aterm), converted (Space, {IME_CONVERTED:?}) and committed (Enter) into PowerShell's line, \
             which printed {IME_ECHO}; {back} English ({ENGLISH}); {composing}; {converted}",
            conversion.map_or("-".to_string(), |c| format!("{c:#x}")),
            IME_STEPS.join(" ")
        ))
    }

    /// Type [`width_typed`]`(tag)` and await PowerShell's `<tag><cols>`:
    /// the width the shell was told, after a re-grid to `cols` columns.
    fn shell_width(&self, s: &Session<'_>, tag: &str, cols: u32) -> Result<String, String> {
        type_text(self.l(), &width_typed(tag))?;
        let echo = format!("{tag}{cols}");
        s.ctl(&["await", "match", &echo, "timeout=60000"])
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
        // Windows gives a window that straddles two monitors the DPI of the
        // one it overlaps most, which need not hold its centre.
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
        let summary = present_summary(&s.ctl(&["metrics", "percentiles"])?);
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
    let mut env = guest::cargo_env(arch);
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
    // One instance through every row; a row that fails ends it (the next
    // rows need its state), and the rows after it are reported not run.
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
            let steps: [&Step<'_>; 7] = [
                &|s| {
                    run.launch(s, "1.00", display.primary.dpi)
                        .map(|d| format!("{d}; {}", run.shot("launch", Some(GPU_ID))))
                        .and_then(|d| run.and_held(s, "launch", d))
                },
                &|s| run.typed(s),
                &|s| run.ime(s),
                &|s| run.resize(s),
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
                // Launched on the monitor already at 150 %.
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
                push(ROWS[7], at150);
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
    /// `--cpu`: the rows on aterm's CPU presenter, each holding the window's
    /// pixels to the presented frame ([`Run::held`]).
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
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_window(args: &[String]) -> Res {
    let ask = parse_args(args)?;
    let l = Layout::find()?;
    let _lock = lane_lock(&l)?;
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
    // The tree the run built: the commit, `-dirty` when the checkout had
    // changes (`windows::build_facts`).
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The launch at 150 %'s readback, small: a frame whose last rows are
    /// past the screen's edge, read back white there.
    #[test]
    fn a_hold_on_the_screens_part_ignores_the_rows_past_its_edge_and_no_others() {
        let (w, h) = (3, 4);
        let frame: Vec<u8> = (0..w * h)
            .flat_map(|i| [i as u8, 0x10, 0x20, 0xff])
            .collect();
        let mut window: Vec<u8> = frame
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        // The last row past the edge, white.
        window[3 * w * 3..].fill(0xff);
        let place =
            desk::parse_place("readback client=3x4 at=10,7 screen=0,0,100,10").expect("a place");
        let seen = place.on_screen().expect("on the screen");
        assert_eq!((seen.t, seen.b, seen.l, seen.r), (0, 3, 0, 3));
        let (rgba, vw, vh) = crop(&frame, w, 4, seen);
        let (rgb, _, _) = crop(&window, w, 3, seen);
        assert_eq!((vw, vh), (3, 3));
        assert_eq!(compare_rgb(&rgba, vw, vh, &(rgb, vw, vh), 0..0), Ok(()));
        // The whole client area is not held: the white row differs.
        assert!(compare_rgb(&frame, w, h, &(window.clone(), w, h), 0..0).is_err());
        // A pixel wrong on the screen's part still fails.
        window[w * 3 + 1] ^= 1;
        let (rgb, _, _) = crop(&window, w, 3, seen);
        assert_eq!(
            compare_rgb(&rgba, vw, vh, &(rgb, vw, vh), 0..0),
            Err(
                "1 of 9 pixels differ; first at (0,1): presented [03, 10, 20], server [03, 11, 20]"
                    .into()
            )
        );
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
        // The IME's echo holds what only the IME commits: no key types it.
        assert!(IME_ECHO.chars().any(|c| desk::qcodes(c).is_none()));
        let typed = format!("{IME_BEFORE}{IME_READING}{IME_AFTER}");
        assert!(!typed.contains(IME_ECHO));
        // Without the IME the same keys print the reading, never the echo.
        assert!(format!("IME-{IME_READING}") != IME_ECHO);
        // The exposure's line prints its echo; the line itself never holds it.
        assert!(!EXPOSED_TYPED.contains(EXPOSED_ECHO));
        // A width's echo is its tag then digits, which no typed line holds,
        // and no tag is another's start.
        for (tag, typed) in WIDTH_TAGS.iter().zip(&widths) {
            assert!(
                !(0..10).any(|d| typed.contains(&format!("{tag}{d}"))),
                "{typed}"
            );
            for other in WIDTH_TAGS {
                assert!(other == *tag || !other.starts_with(tag), "{tag} {other}");
            }
        }
        // A step per key of the reading, each the last one grown or turned
        // into kana, never the conversion.
        assert_eq!(IME_STEPS.len(), IME_READING.chars().count());
        assert!(IME_STEPS.iter().all(|step| !step.contains(IME_CONVERTED)));
        assert!(IME_STEPS.windows(2).all(|w| w[0] != w[1]));
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
