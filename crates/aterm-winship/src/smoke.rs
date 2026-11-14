// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `smoke <dist dir>`: run a packaged build's own images headless, in an
//! isolated environment, and judge what its control socket answers.
//!
//! The rows are the lab's Windows lane's (`crates/aterm-lab/src/windows_smoke.rs`),
//! run on the host against the release images a person will install: the
//! instance starts, its first tab's shell answers a typed line, the grid
//! resizes, the metrics and `sys` views answer and one frame is drawn. No
//! window opens (`--headless`), and nothing of the owner's is read or written
//! (`crate::env`).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::env::{self, Roots};
use crate::pointer::Build;
use crate::proc::{self, Job, Place, Ran};

/// One row of the report: its name, and what it found or why it failed.
pub type Row = (String, Result<String, String>);

/// The PowerShell line typed into the first tab. Its output is [`ECHO`], which
/// the line itself does not contain, so only a shell that ran it shows it.
pub const TYPED: &str = "Write-Output ('LANE-' + 6*7)";
/// What [`TYPED`] prints.
pub const ECHO: &str = "LANE-42";
/// The `sys` views the smoke reads, `""` being the overview.
pub const SYS_VIEWS: [&str; 6] = ["", "mem", "disk", "net", "cores", "aterm"];
/// The grid the smoke resizes the first tab to.
pub const RESIZED: (u32, u32) = (30, 100);
/// A wait for something that must happen: a hang detector, never a budget.
pub const HANG: Duration = Duration::from_secs(60);
/// The line aterm-gui writes when a window's GPU surface did not attach.
pub const SURFACE_FAILED: &str = "GPU surface creation failed";

/// What `smoke` was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The dist folder: `aterm.exe`, `aterm-gui.exe`, the aliases.
    pub dist: PathBuf,
    /// The scratch root to create and delete; a fresh one under the system
    /// temporary directory when `None`.
    pub scratch: Option<PathBuf>,
    /// Leave the scratch root in place.
    pub keep: bool,
    /// Copy the frame here.
    pub frame: Option<PathBuf>,
}

/// How a smoke ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It ran: every row, passed or failed.
    Ran(Vec<Row>),
    /// It could not run, and why. Nothing was judged.
    NotRun(String),
}

impl Outcome {
    /// The process exit code: 0 every row passed, 1 a row failed, 3 not run.
    #[must_use]
    pub fn code(&self) -> i32 {
        match self {
            Self::Ran(rows) if rows.iter().all(|(_, v)| v.is_ok()) => crate::PASS,
            Self::Ran(_) => crate::FAILED,
            Self::NotRun(_) => crate::NOT_RUN,
        }
    }
}

/// A row as its one report line.
#[must_use]
pub fn line(row: &Row) -> String {
    match &row.1 {
        Ok(detail) => format!("PASS {}: {detail}", row.0),
        Err(why) => format!("FAIL {}: {why}", row.0),
    }
}

/// `text` on one line, at most `max` characters.
#[must_use]
pub fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

/// The value of `key=` in a control reply.
#[must_use]
pub fn field<'a>(reply: &'a str, key: &str) -> Option<&'a str> {
    reply
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// What an image says of itself on `--version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The first line's first word: `aterm`, `aterm-gui`, `aterm-ctl`.
    pub name: String,
    /// The first line's second word: the version.
    pub version: String,
    /// The `build:` line's number, where the image prints one.
    pub build: Option<u64>,
}

/// Reads a `--version` answer.
#[must_use]
pub fn identity(text: &str) -> Option<Identity> {
    let mut first = text.lines().next()?.split_whitespace();
    let (name, version) = (first.next()?, first.next()?);
    if !version.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(Identity {
        name: name.to_string(),
        version: version.to_string(),
        build: text
            .lines()
            .find_map(|l| l.trim().strip_prefix("build:")?.trim().parse().ok()),
    })
}

/// Judges `aterm.exe --version` against the folder's `BUILD.json` (`None`: a
/// dist older than the record, which is reported and not failed).
///
/// # Errors
/// The answer is not an identity with a build number, or differs from the record.
pub fn version_row(answer: &str, record: Option<&Build>) -> Result<String, String> {
    let id = identity(answer)
        .filter(|id| id.name == "aterm")
        .ok_or_else(|| format!("aterm.exe --version answered {:?}", one_line(answer, 120)))?;
    let build = id.build.ok_or_else(|| {
        format!(
            "aterm.exe --version names no build: {:?}",
            one_line(answer, 120)
        )
    })?;
    let Some(record) = record else {
        return Ok(format!("aterm {} build {build}; no BUILD.json", id.version));
    };
    if id.version != record.version || build != record.build_number {
        return Err(format!(
            "aterm.exe says {} build {build}, BUILD.json says {} build {}",
            id.version, record.version, record.build_number
        ));
    }
    Ok(format!(
        "aterm {} build {build} commit {} ({}, {}{}) as BUILD.json says",
        id.version,
        record.commit,
        record.arch,
        record.toolchain,
        if record.dirty { ", dirty" } else { "" }
    ))
}

/// A `dims` reply's grid and cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dims {
    /// Grid rows.
    pub rows: u32,
    /// Grid columns.
    pub cols: u32,
    /// Cell width in pixels.
    pub cell_w: u32,
    /// Cell height in pixels.
    pub cell_h: u32,
    /// Font size as the reply spells it.
    pub font_px: String,
}

impl std::fmt::Display for Dims {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}x{} cells of {}x{} px, font {} px",
            self.rows, self.cols, self.cell_w, self.cell_h, self.font_px
        )
    }
}

/// Reads a `dims` reply: `OK <rows> <cols> … cell_w= cell_h= font_px=`.
///
/// # Errors
/// The reply is not that, or a grid or cell dimension is zero.
pub fn dims(reply: &str) -> Result<Dims, String> {
    let bad = || format!("dims answered {:?}", one_line(reply, 120));
    let mut words = reply.split_whitespace();
    if words.next() != Some("OK") {
        return Err(bad());
    }
    let mut next = || words.next().and_then(|w| w.parse::<u32>().ok());
    let (rows, cols) = (next().ok_or_else(bad)?, next().ok_or_else(bad)?);
    let px = |key: &str| field(reply, key).and_then(|v| v.parse::<u32>().ok());
    let d = Dims {
        rows,
        cols,
        cell_w: px("cell_w").ok_or_else(bad)?,
        cell_h: px("cell_h").ok_or_else(bad)?,
        font_px: field(reply, "font_px").ok_or_else(bad)?.to_string(),
    };
    if d.rows == 0 || d.cols == 0 || d.cell_w == 0 || d.cell_h == 0 {
        return Err(format!("a dimension is zero: {d}"));
    }
    Ok(d)
}

/// Reads a `metrics percentiles` reply into `n_present` and the present
/// percentiles, the figures latency is quoted in.
///
/// # Errors
/// The reply is not `OK …`, or one of the four is missing or not a number.
pub fn percentiles(reply: &str) -> Result<String, String> {
    if !reply.starts_with("OK ") {
        return Err(format!(
            "metrics percentiles answered {:?}",
            one_line(reply, 120)
        ));
    }
    let get = |key: &str| {
        field(reply, key)
            .filter(|v| v.parse::<f64>().is_ok_and(f64::is_finite))
            .ok_or_else(|| format!("metrics percentiles has no {key}"))
    };
    let n = get("n_present")?;
    let summary = format!(
        "n_present={n} present_p50/p95/p99_ms={}/{}/{}",
        get("present_p50_ms")?,
        get("present_p95_ms")?,
        get("present_p99_ms")?
    );
    Ok(if n == "0" {
        format!("{summary} (headless: no window presents)")
    } else {
        summary
    })
}

/// Reads an `image` reply: `OK <width> <height> <path>`.
///
/// # Errors
/// The reply is not that.
pub fn image_reply(reply: &str) -> Result<(u32, u32, PathBuf), String> {
    let bad = || format!("image answered {:?}", one_line(reply, 160));
    let rest = reply.trim().strip_prefix("OK ").ok_or_else(bad)?;
    let (w, rest) = rest.split_once(' ').ok_or_else(bad)?;
    let (h, path) = rest.split_once(' ').ok_or_else(bad)?;
    let (w, h) = (w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?);
    if path.trim().is_empty() {
        return Err(bad());
    }
    Ok((w, h, PathBuf::from(path.trim())))
}

/// Judges a decoded frame: it has the size the instance said and more than one
/// colour (a frame of one flat colour is a renderer that drew nothing).
///
/// # Errors
/// An empty frame, another size, a short buffer, or one flat colour.
pub fn frame(rgba: &[u8], width: usize, height: usize, said: (u32, u32)) -> Result<String, String> {
    if width == 0 || height == 0 {
        return Err("the frame is empty".to_string());
    }
    if (width as u64, height as u64) != (u64::from(said.0), u64::from(said.1)) {
        return Err(format!(
            "the frame is {width}x{height}, the instance said {}x{}",
            said.0, said.1
        ));
    }
    if rgba.len() != width * height * 4 {
        return Err(format!(
            "{} bytes are not {width}x{height} RGBA",
            rgba.len()
        ));
    }
    let mut colours: Vec<&[u8; 4]> = Vec::new();
    for px in rgba.as_chunks::<4>().0 {
        if !colours.contains(&px) {
            colours.push(px);
            if colours.len() == 16 {
                break;
            }
        }
    }
    match colours.len() {
        1 => Err(format!(
            "the frame is one flat colour, rgba {:?}",
            colours.first().copied().copied().unwrap_or_default()
        )),
        n => Ok(format!(
            "{width}x{height}, {n}{} colours",
            if n == 16 { "+" } else { "" }
        )),
    }
}

/// Judges a `sys` view's answer: the header names the view and its row count,
/// and that many rows follow. Reports how many values read `unavailable(`
/// and why ([`unavailable`]); the count never fails the row.
///
/// # Errors
/// No `OK <n> sys v=1 view=<view>` header, or no rows.
pub fn sys_view(text: &str, view: &str) -> Result<String, String> {
    let name = if view.is_empty() { "overview" } else { view };
    let want = format!("view={name}");
    let header = text
        .lines()
        .find_map(|l| {
            let l = l
                .trim()
                .strip_prefix("aterm-ctl:")
                .map_or(l.trim(), str::trim);
            l.strip_prefix("OK ").filter(|r| r.contains(" sys v=1 "))
        })
        .ok_or_else(|| format!("sys {view} gave no header: {:?}", one_line(text, 160)))?;
    if !header.split_whitespace().any(|w| w == want) {
        return Err(format!(
            "sys {view} answered another view: {:?}",
            one_line(header, 120)
        ));
    }
    let rows = header
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("sys {view} has no rows: {:?}", one_line(header, 120)))?;
    Ok(format!("{rows} rows, {}", unavailable(text)))
}

/// How many values of a `sys` answer read `unavailable(<reason>)`, and why:
/// the count, then each reason (the word before a `:` detail) with its own
/// count. The reason is what tells a reader that is not wired on this platform
/// (`not_sampled`) from a value this machine does not have (`unsupported`: the
/// speed of a link that is down) and from one not yet measured (`warming`).
#[must_use]
pub fn unavailable(text: &str) -> String {
    let mut reasons: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for after in text.split("unavailable(").skip(1) {
        let reason = after.split([')', ':']).next().unwrap_or_default().trim();
        *reasons
            .entry(if reason.is_empty() { "?" } else { reason })
            .or_default() += 1;
    }
    let total: usize = reasons.values().sum();
    if total == 0 {
        return "0 values unavailable".to_string();
    }
    let why: Vec<String> = reasons.iter().map(|(r, n)| format!("{r} {n}")).collect();
    format!("{total} values unavailable ({})", why.join(", "))
}

/// Judges what the instance drew the frame on: `metrics` after the frame says
/// `backend=gpu` or the instance named its device, and no surface failed.
///
/// # Errors
/// A surface failed to attach, or the frame was drawn on the CPU.
pub fn renderer(metrics: &str, stderr: &str) -> Result<String, String> {
    if let Some(l) = stderr.lines().find(|l| l.contains(SURFACE_FAILED)) {
        return Err(format!("the GPU surface did not attach: {:?}", l.trim()));
    }
    let backend = field(metrics, "backend").unwrap_or("(none)");
    let device = stderr
        .lines()
        .find_map(|l| l.split_once("GPU rendering on ").map(|(_, r)| r.trim()));
    match (backend, device) {
        (_, Some(device)) => Ok(format!("backend={backend}; drew on {device}")),
        ("gpu", None) => Ok("backend=gpu (no device line)".to_string()),
        _ => Err(format!(
            "backend={backend} after the frame and no GPU device was named"
        )),
    }
}

/// Whether two files hold the same bytes.
fn same_bytes(a: &Path, b: &Path) -> std::io::Result<bool> {
    if std::fs::metadata(a)?.len() != std::fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            return Ok(true);
        }
        let (got, want) = (
            ba.get(..n).unwrap_or_default(),
            bb.get_mut(..n).unwrap_or_default(),
        );
        fb.read_exact(want)?;
        if got != want {
            return Ok(false);
        }
    }
}

/// The images `build.ps1` lays into a folder that are no alias of `aterm.exe`, lower case:
/// the front door itself, the windowed image, the console host, and get-aterm.ps1's
/// signature checker (`aterm_winship::handoff::VERIFIER_MEMBER`).
pub const OWN_IMAGES: [&str; 4] = [
    "aterm.exe",
    "aterm-gui.exe",
    "openconsole.exe",
    "aterm-bootstrap-verify.exe",
];

/// The images of `dist` that are aliases of `aterm.exe`: every `.exe` but
/// [`OWN_IMAGES`], so an image nothing names is judged as an alias and refused.
///
/// # Errors
/// The folder could not be listed.
pub fn aliases(dist: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dist)
        .map_err(|e| format!("{}: {e}", dist.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"))
                && !OWN_IMAGES.contains(&name.to_ascii_lowercase().as_str())
        })
        .collect();
    found.sort();
    Ok(found)
}

/// Judges the other images: every alias is `aterm.exe` byte for byte (so it is
/// the build `aterm.exe --version` named), and the windowed image, which is
/// linked apart, names the same version.
fn aliases_row(dist: &Path, version: &str, place: Place<'_>) -> Result<String, String> {
    let front = dist.join("aterm.exe");
    let found = aliases(dist)?;
    if found.is_empty() {
        return Err("the folder holds no alias of aterm.exe".to_string());
    }
    for alias in &found {
        let same = same_bytes(&front, alias).map_err(|e| format!("{}: {e}", alias.display()))?;
        if !same {
            return Err(format!(
                "{} is not aterm.exe byte for byte",
                alias.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    let gui = proc::run(&dist.join("aterm-gui.exe"), &["--version"], place, HANG)?;
    match identity(&gui.stdout) {
        Some(id) if gui.ok() && id.name == "aterm-gui" && id.version == version => Ok(format!(
            "{} aliases are aterm.exe byte for byte; aterm-gui {}",
            found.len(),
            id.version
        )),
        _ => Err(format!(
            "aterm-gui.exe --version answered {:?} (exit {:?}), want aterm-gui {version}",
            one_line(&gui.text(), 120),
            gui.code
        )),
    }
}

/// A headless instance this process started, and the job that holds it.
struct Instance {
    child: aterm_uds::spawnfd::Child,
    job: Job,
    pid: u32,
}

impl Instance {
    fn start(
        dist: &Path,
        sock: &Path,
        roots: &Roots,
        env: &[(String, String)],
    ) -> Result<Self, String> {
        let log = |name: &str| {
            let path = roots.root.join(name);
            std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))
        };
        let gui = dist.join("aterm-gui.exe");
        let args: [&std::ffi::OsStr; 7] = [
            "--headless".as_ref(),
            "--control-sock".as_ref(),
            sock.as_os_str(),
            "-e".as_ref(),
            "powershell.exe".as_ref(),
            "-NoLogo".as_ref(),
            "-NoProfile".as_ref(),
        ];
        let place = Place {
            env: Some(env),
            cwd: Some(&roots.home),
        };
        let mut cmd = proc::command(&gui, &args, place);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log("gui-stdout.log")?))
            .stderr(Stdio::from(log("gui-stderr.log")?));
        // Born in the job: it runs no instruction, so starts no console host
        // and no shell, before the job holds it.
        let (child, job) = Job::start(&mut cmd)
            .map_err(|e| format!("{} did not start in a job: {e}", gui.display()))?;
        let pid = child.id();
        Ok(Self { child, job, pid })
    }

    /// Waits for the control socket to appear, the instance still running.
    fn serving(&mut self, sock: &Path) -> Result<Duration, String> {
        let began = Instant::now();
        loop {
            if sock.exists() {
                return Ok(began.elapsed());
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "the instance exited ({status}) before it served a control socket"
                ));
            }
            if began.elapsed() >= HANG {
                return Err(format!(
                    "no control socket at {} after {} s",
                    sock.display(),
                    HANG.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Ends the instance and everything it started, by the handles held here.
    fn end(mut self) -> Result<String, String> {
        let emptied = self
            .job
            .end(HANG)
            .map_err(|e| format!("the job did not end: {e}"))?;
        aterm_uds::deadline::kill_within(&mut self.child, HANG);
        match self.child.try_wait() {
            Ok(Some(_)) if emptied => Ok(format!(
                "instance pid {} and the processes it started ended by this tool's own handles",
                self.pid
            )),
            Ok(Some(_)) => Err(format!(
                "instance pid {} ended, but a process it started was still running after {} s",
                self.pid,
                HANG.as_secs()
            )),
            other => Err(format!("instance pid {} did not end: {other:?}", self.pid)),
        }
    }
}

/// The session's rows, in order, stopping at the first that the later ones
/// stand on.
struct Session<'a> {
    root: &'a Path,
    dist: &'a Path,
    sock: &'a Path,
    place: Place<'a>,
    rows: &'a mut Vec<Row>,
    say: &'a mut dyn FnMut(&str),
}

impl Session<'_> {
    fn ctl(&self, words: &[&str]) -> Result<Ran, String> {
        let mut args: Vec<&std::ffi::OsStr> = vec![
            "ctl".as_ref(),
            "--sock".as_ref(),
            self.sock.as_os_str(),
            "--timeout".as_ref(),
            "90".as_ref(),
        ];
        args.extend(words.iter().map(|w| std::ffi::OsStr::new(*w)));
        let ran = proc::run(
            &self.dist.join("aterm.exe"),
            &args,
            self.place,
            Duration::from_secs(120),
        )?;
        if ran.ok() {
            Ok(ran)
        } else {
            Err(format!(
                "`{}` answered {:?} (exit {:?})",
                words.join(" "),
                one_line(&ran.text(), 200),
                ran.code
            ))
        }
    }

    /// A verb whose reply is one `OK …` line on standard output.
    fn reply(&self, words: &[&str], begins: &str) -> Result<String, String> {
        let ran = self.ctl(words)?;
        let reply = ran.stdout.trim().to_string();
        if reply.starts_with(begins) {
            Ok(reply)
        } else {
            Err(format!(
                "`{}` answered {:?}",
                words.join(" "),
                one_line(&ran.text(), 200)
            ))
        }
    }

    fn row(&mut self, name: &str, verdict: Result<String, String>) -> bool {
        let ok = verdict.is_ok();
        emit(self.rows, self.say, (name.to_string(), verdict));
        ok
    }

    fn shell(&self) -> Result<String, String> {
        self.reply(&["send", TYPED], "OK")?;
        self.reply(&["key", "enter"], "OK")?;
        self.reply(&["await", "match", ECHO, "timeout=60000"], "OK match")?;
        Ok(format!(
            "typed a line in the first tab; its shell answered {ECHO}"
        ))
    }

    fn resize(&self) -> Result<String, String> {
        let (rows, cols) = RESIZED;
        self.reply(&["resize", &rows.to_string(), &cols.to_string()], "OK")?;
        let d = dims(&self.reply(&["dims"], "OK ")?)?;
        if (d.rows, d.cols) == RESIZED {
            Ok(format!("resized to {d}"))
        } else {
            Err(format!("asked for {rows}x{cols}, the grid is {d}"))
        }
    }

    fn image(&self, keep: Option<&Path>) -> Result<String, String> {
        let (w, h, path) = image_reply(&self.reply(&["image", "frame.png"], "OK ")?)?;
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (rgba, width, height) = aterm_png::decode_rgba8(&bytes, 1 << 15)
            .ok_or_else(|| format!("{} ({} bytes) is not a PNG", path.display(), bytes.len()))?;
        let detail = frame(&rgba, width, height, (w, h))?;
        match keep {
            Some(to) => {
                std::fs::write(to, &bytes).map_err(|e| format!("{}: {e}", to.display()))?;
                Ok(format!("{detail}; frame {}", to.display()))
            }
            None => Ok(detail),
        }
    }

    fn run(&mut self, frame_to: Option<&Path>, stderr_log: &Path) {
        let ready = self
            .reply(&["ready", "60000"], "OK ready")
            .map(|r| one_line(&r, 80));
        if !self.row("ready", ready) {
            return;
        }
        let status = self.reply(&["status"], "OK ").and_then(|r| {
            let get = |k: &str| field(&r, k).unwrap_or("?").to_string();
            if field(&r, "render") == Some("ok") {
                Ok(format!(
                    "render=ok conpty={} integration={}",
                    get("conpty"),
                    get("integration")
                ))
            } else {
                Err(format!("render={}, want ok", get("render")))
            }
        });
        self.row("status", status);
        let grid = self
            .reply(&["dims"], "OK ")
            .and_then(|r| dims(&r))
            .map(|d| d.to_string());
        self.row("dims", grid);
        let shell = self.shell();
        self.row("shell", shell);
        let resized = self.resize();
        self.row("resize", resized);
        let present = self
            .reply(&["metrics", "percentiles"], "OK ")
            .and_then(|r| percentiles(&r));
        self.row("metrics", present);
        let image = self.image(frame_to);
        self.row("image", image);
        let drew = self
            .reply(&["metrics"], "OK ")
            .and_then(|m| renderer(&m, &std::fs::read_to_string(stderr_log).unwrap_or_default()));
        self.row("renderer", drew);
        for view in SYS_VIEWS {
            let words: Vec<&str> = ["sys", view]
                .into_iter()
                .filter(|w| !w.is_empty())
                .collect();
            let name = if view.is_empty() { "overview" } else { view };
            let verdict = self.ctl(&words).and_then(|ran| {
                // Kept with the scratch root (`--keep`): the rows behind the count.
                let _ = std::fs::write(self.root.join(format!("sys-{name}.txt")), ran.text());
                sys_view(&ran.text(), view)
            });
            self.row(&format!("sys {name}"), verdict);
        }
    }
}

fn scratch_root(asked: Option<&Path>) -> Result<PathBuf, String> {
    let root = match asked {
        Some(dir) => std::path::absolute(dir).map_err(|e| format!("{}: {e}", dir.display()))?,
        None => std::env::temp_dir().join(format!("aterm-winship-{}", unique())),
    };
    // Never adopt a folder with anything in it: the smoke deletes its root.
    if std::fs::read_dir(&root).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(format!(
            "{} is not empty: the smoke deletes its scratch root",
            root.display()
        ));
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    Ok(root)
}

/// A name no other run of this tool on this machine is using.
fn unique() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    format!("{}-{nanos:08x}", std::process::id())
}

fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all.get(all.len().saturating_sub(lines)..)
        .unwrap_or_default()
        .join(" | ")
}

fn emit(rows: &mut Vec<Row>, say: &mut dyn FnMut(&str), row: Row) {
    say(&line(&row));
    rows.push(row);
}

/// Runs the smoke on `opts.dist` with `parent` as the caller's environment,
/// calling `say` with each row's line as it is judged.
pub fn smoke(opts: &Options, parent: &[(String, String)], say: &mut dyn FnMut(&str)) -> Outcome {
    let dist = match std::path::absolute(&opts.dist) {
        Ok(dist) => dist,
        Err(e) => return Outcome::NotRun(format!("{}: {e}", opts.dist.display())),
    };
    for image in ["aterm.exe", "aterm-gui.exe"] {
        if !dist.join(image).is_file() {
            return Outcome::NotRun(format!(
                "{} holds no {image}: not a dist folder",
                dist.display()
            ));
        }
    }
    let root = match scratch_root(opts.scratch.as_deref()) {
        Ok(root) => root,
        Err(e) => return Outcome::NotRun(e),
    };
    let roots = Roots::under(&root);
    let prepared = (|| {
        let env = env::isolated(parent, &roots)?;
        let (sock, extra) =
            env::sock_path(&root, &std::env::temp_dir(), &format!("aws-{}", unique()))?;
        for dir in roots.dirs().into_iter().chain(sock.parent()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok::<_, String>((env, sock, extra))
    })();
    let (env, sock, extra) = match prepared {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&root);
            return Outcome::NotRun(e);
        }
    };
    let place = Place {
        env: Some(&env),
        cwd: Some(&roots.home),
    };
    let mut rows: Vec<Row> = Vec::new();

    let record = Build::read(&dist);
    let answer = proc::run(&dist.join("aterm.exe"), &["--version"], place, HANG);
    let version = match (&record, &answer) {
        (Err(e), _) | (_, Err(e)) => Err(e.clone()),
        (Ok(record), Ok(ran)) => version_row(&ran.stdout, record.as_ref()),
    };
    emit(&mut rows, say, ("version".to_string(), version));
    // The other images are held to what aterm.exe said, whatever the record says.
    if let Some(id) = answer.as_ref().ok().and_then(|ran| identity(&ran.stdout)) {
        emit(
            &mut rows,
            say,
            (
                "aliases".to_string(),
                aliases_row(&dist, &id.version, place),
            ),
        );
    }

    let stderr_log = root.join("gui-stderr.log");
    match Instance::start(&dist, &sock, &roots, &env) {
        Err(e) => emit(&mut rows, say, ("start".to_string(), Err(e))),
        Ok(mut instance) => {
            let serving = instance.serving(&sock).map(|took| {
                format!(
                    "headless aterm-gui pid {} served its control socket in {} ms",
                    instance.pid,
                    took.as_millis()
                )
            });
            let up = serving.is_ok();
            emit(&mut rows, say, ("start".to_string(), serving));
            if up {
                let mut session = Session {
                    root: &root,
                    dist: &dist,
                    sock: &sock,
                    place,
                    rows: &mut rows,
                    say: &mut *say,
                };
                session.run(opts.frame.as_deref(), &stderr_log);
            }
            let state = roots.local.join("aterm");
            let isolation = if state.is_dir() {
                Ok(format!(
                    "the instance kept its state under the scratch root ({})",
                    state.display()
                ))
            } else {
                Err(format!(
                    "the instance wrote no state under {}: it used another root",
                    state.display()
                ))
            };
            emit(&mut rows, say, ("isolation".to_string(), isolation));
            emit(&mut rows, say, ("end".to_string(), instance.end()));
        }
    }
    if rows.iter().any(|(_, v)| v.is_err()) {
        let said = tail(&stderr_log, 12);
        if !said.is_empty() {
            say(&format!("aterm-gui stderr: {said}"));
        }
    }
    if opts.keep {
        say(&format!("scratch kept at {}", root.display()));
    } else {
        let swept = [Some(root.as_path()), extra.as_deref()]
            .into_iter()
            .flatten()
            .try_for_each(|dir| {
                std::fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
            })
            .map(|()| "scratch root deleted".to_string());
        emit(&mut rows, say, ("cleanup".to_string(), swept));
    }
    Outcome::Ran(rows)
}

#[cfg(test)]
#[path = "smoke_tests.rs"]
mod tests;
