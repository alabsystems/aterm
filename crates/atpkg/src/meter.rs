// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The one-line progress meter a person sees under a typed `aterm pkg` verb.
//!
//! A typed install or update printed its first line and then nothing for the minutes a
//! multi-gigabyte download took. The meter is ONE line on stderr, redrawn about eight
//! times a second: from the live pass ([`crate::progress::live_snapshot`]) — which
//! program, what it is doing, the bytes, the rate and a rough time left — or, with no
//! pass live, a spinner and a sentence ([`Show::Busy`]). [`render_line`] builds the line
//! and is pure; [`TtyMeter`] owns the thread that draws it.
//!
//! It draws only for a person ([`wanted`]): stderr a terminal, `TERM` not `dumb`, on
//! Unix. A process the window spawns has pipes for stdout and stderr and carries
//! `--progress-file`, so nothing here ever reaches it, and the meter never writes to
//! stdout at all.
//!
//! Output and meter never share a line. atpkg's `println!`, `eprintln!` and `print!` are
//! this module's [`println`], [`eprintln`] and [`print`] (the macros in `lib.rs`): with no
//! meter live each is the standard macro, byte for byte and panic for panic; with one live
//! the meter's line is cleared first and the next tick draws it again below. A line that does not end (a question waiting for an answer)
//! stops the drawing until one does, and [`hold`] stops it while a child owns the
//! terminal (`sudo` asking for a password). The line is cleared when the meter stops —
//! on every return, and on Ctrl-C, whose default action is kept: the line is cleared,
//! then the signal is raised again.

use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::progress::{Phase, ProgramProgress, ProgressFile};

/// How often the meter redraws.
const TICK: Duration = Duration::from_millis(125);

/// The span the download rate is measured over.
const RATE_WINDOW: Duration = Duration::from_secs(5);

/// How long bytes must have been moving before a rate and a time left are shown: the
/// first seconds of a download are a TCP ramp, and a rate read off them is a guess.
const RATE_SETTLE: Duration = Duration::from_secs(3);

/// What the line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Show {
    /// A pass: the program `focus` names, or — `None` — the one the pass is working on;
    /// `idle` when the pass has no such program yet (it is still deciding what to do).
    Pass {
        file: ProgressFile,
        focus: Option<String>,
        idle: String,
    },
    /// A spinner and a sentence: a wait, or work with no numbers to show.
    Busy(String),
    /// Nothing: the line is cleared.
    Nothing,
}

/// How the line is drawn: the terminal's width, the spinner's frame, and whether the
/// terminal takes UTF-8 (the braille spinner and the block bar) or only ASCII.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Look {
    pub cols: usize,
    pub frame: usize,
    pub utf8: bool,
}

/// The program the line features: `focus` if the pass has a row for it, else the one in
/// a working phase — downloading, checking or unpacking before one only being set up.
fn featured<'a>(
    file: &'a ProgressFile,
    focus: Option<&str>,
) -> Option<(&'a str, &'a ProgramProgress)> {
    if let Some(name) = focus {
        return file
            .programs
            .get_key_value(name)
            .map(|(k, row)| (k.as_str(), row));
    }
    let working = |phases: &[Phase]| {
        file.programs
            .iter()
            .find(|(_, row)| phases.contains(&row.phase))
            .map(|(k, row)| (k.as_str(), row))
    };
    working(&[Phase::Download, Phase::Verify, Phase::Extract]).or_else(|| working(&[Phase::Link]))
}

/// A program name from a progress snapshot, fit for a terminal: the file is untrusted
/// input (see [`crate::progress`]), so a name the store would never admit, or one with
/// anything but the characters program names are spelled with, is not echoed.
fn display_name(name: &str) -> &str {
    if crate::store::ToolName::new(name).is_some()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
    {
        name
    } else {
        "a program"
    }
}

/// The spinner's glyph for `frame`.
fn spinner(frame: usize, utf8: bool) -> char {
    const BRAILLE: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    const ASCII: [char; 4] = ['|', '/', '-', '\\'];
    if utf8 {
        BRAILLE[frame % BRAILLE.len()]
    } else {
        ASCII[frame % ASCII.len()]
    }
}

/// A bar `width` cells wide, `done` of `total` full — in eighths of a cell with UTF-8.
fn bar(done: u64, total: u64, width: usize, utf8: bool) -> String {
    let total = total.max(1);
    let done = done.min(total);
    let eighths = usize::try_from(u128::from(done) * (width as u128) * 8 / u128::from(total))
        .unwrap_or(width * 8);
    let mut out = String::new();
    if utf8 {
        const PARTIAL: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
        out.push('▕');
        let full = eighths / 8;
        out.extend(std::iter::repeat_n('█', full));
        if full < width {
            out.push(PARTIAL[eighths % 8]);
            out.extend(std::iter::repeat_n(' ', width - full - 1));
        }
        out.push('▏');
    } else {
        let full = (eighths + 4) / 8;
        out.push('[');
        out.extend(std::iter::repeat_n('#', full.min(width)));
        out.extend(std::iter::repeat_n(' ', width.saturating_sub(full)));
        out.push(']');
    }
    out
}

/// "about 3 min left" / "40 s left" for `secs` remaining.
fn time_left(secs: u64) -> String {
    if secs < 60 {
        // Rounded up to the next 5 s: a countdown that ticks every second reads as a
        // promise, and this is an estimate.
        format!("{} s left", secs.div_ceil(5).max(1) * 5)
    } else if secs < 100 * 60 {
        format!("about {} min left", secs.div_ceil(60))
    } else {
        format!("about {} h left", secs.div_ceil(3600))
    }
}

/// The meter's line for `show` — PURE, so every shape is pinned by a test. `rate` is the
/// download rate in bytes per second, `None` until it has settled ([`Rate`]). The line is
/// cut to one short of `look.cols`, so it never wraps (a wrapped line is not redrawn in
/// place). Empty for [`Show::Nothing`].
pub(crate) fn render_line(show: &Show, rate: Option<f64>, look: &Look) -> String {
    let body = match show {
        Show::Nothing => return String::new(),
        Show::Busy(text) => text.clone(),
        Show::Pass { file, focus, idle } => match featured(file, focus.as_deref()) {
            Some((name, row)) => pass_body(file, name, row, focus.is_none(), rate, look),
            None => idle.clone(),
        },
    };
    let mut line = String::new();
    line.push(spinner(look.frame, look.utf8));
    line.push(' ');
    line.push_str(&body);
    fit(line, look.cols)
}

/// The words after the spinner for one program of a pass. `whole_pass` when the line
/// follows the pass rather than one program in it ([`Show::Pass`]'s `focus`): only then
/// are the pass's position (`3 of 12`) and its whole download's time left about the
/// program shown — a pending stub's program may still be queued behind the others.
fn pass_body(
    file: &ProgressFile,
    name: &str,
    row: &ProgramProgress,
    whole_pass: bool,
    rate: Option<f64>,
    look: &Look,
) -> String {
    let verb = match row.phase {
        Phase::Queued => "Waiting to install",
        Phase::Download => "Downloading",
        Phase::Verify => "Checking",
        Phase::Extract => "Unpacking",
        Phase::Link => "Setting up",
        Phase::Done => "Installed",
        Phase::Failed => "Could not install",
        Phase::Skipped => "Skipped",
    };
    let dot = if look.utf8 { " · " } else { " - " };
    let head = format!("{verb} {}", display_name(name));
    let total = file.overall.programs_total;
    let mut position = if whole_pass && total > 1 {
        let n = file.overall.programs_done.saturating_add(1).min(total);
        format!(" ({n} of {total})")
    } else {
        String::new()
    };
    let metered = matches!(row.phase, Phase::Download | Phase::Extract) && row.bytes_total > 0;
    if !metered {
        let mut s = head + &position;
        if matches!(
            row.phase,
            Phase::Download | Phase::Verify | Phase::Extract | Phase::Link
        ) {
            s.push('…');
        }
        return s;
    }
    let (amount, mut per_second, left) = if row.phase == Phase::Extract {
        let pct = row.bytes_done.min(row.bytes_total).saturating_mul(100) / row.bytes_total;
        (format!(" {pct}%"), String::new(), String::new())
    } else {
        let amount = format!(
            " {} of {}",
            crate::cost::human_bytes(row.bytes_done.min(row.bytes_total)),
            crate::cost::human_bytes(row.bytes_total)
        );
        match rate.filter(|r| r.is_finite() && *r >= 1.0) {
            Some(rate) => {
                // The time left for the whole pass's downloads when the line follows a pass
                // that planned them, else for this one program.
                let remaining = if whole_pass && file.overall.bytes_total > 0 {
                    file.overall
                        .bytes_total
                        .saturating_sub(file.overall.bytes_done)
                } else {
                    row.bytes_total.saturating_sub(row.bytes_done)
                };
                let left = if remaining > 0 {
                    format!(
                        "{dot}{}",
                        time_left((remaining as f64 / rate).ceil() as u64)
                    )
                } else {
                    String::new()
                };
                (
                    amount,
                    format!("{dot}{}/s", crate::cost::human_bytes(rate as u64)),
                    left,
                )
            }
            None => (amount, String::new(), String::new()),
        }
    };
    // THE TIME LEFT SURVIVES A NARROW TERMINAL. What does not fit goes in this order —
    // the rate, then the pass position — and the bar takes the room the words leave (up
    // to 16 cells, left out under 4): at 80 columns the cut used to land on "about 2 min
    // left", the one figure a person waiting reads.
    let room = look.cols.max(20) - 1 - 2;
    let words = |position: &str, per_second: &str| {
        [
            head.as_str(),
            position,
            amount.as_str(),
            per_second,
            left.as_str(),
        ]
        .iter()
        .map(|p| p.chars().count())
        .sum::<usize>()
    };
    if words(&position, &per_second) > room {
        per_second.clear();
    }
    if words(&position, &per_second) > room {
        position.clear();
    }
    let width = room
        .saturating_sub(words(&position, &per_second) + 4)
        .min(16);
    let drawn = if width >= 4 {
        format!(
            "  {}",
            bar(row.bytes_done, row.bytes_total, width, look.utf8)
        )
    } else {
        String::new()
    };
    format!("{head}{position}{drawn}{amount}{per_second}{left}")
}

/// `line` cut to at most `cols - 1` characters, with a visible `…` where it was cut.
fn fit(line: String, cols: usize) -> String {
    let max = cols.max(20) - 1;
    if line.chars().count() <= max {
        return line;
    }
    let mut out: String = line.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// The download rate over the last [`RATE_WINDOW`], from a pass's overall byte count.
#[derive(Debug, Default)]
pub(crate) struct Rate {
    samples: VecDeque<(Instant, u64)>,
    moving_since: Option<Instant>,
}

impl Rate {
    /// Record `bytes` (the pass's overall download count) at `now`; the rate in bytes per
    /// second once bytes have moved for [`RATE_SETTLE`], else `None`. A count that went
    /// DOWN is a new pass: the window starts again.
    pub(crate) fn note(&mut self, now: Instant, bytes: u64) -> Option<f64> {
        if self.samples.back().is_some_and(|(_, b)| bytes < *b) {
            self.reset();
        }
        if self.moving_since.is_none() && self.samples.back().is_some_and(|(_, b)| bytes > *b) {
            self.moving_since = Some(now);
        }
        self.samples.push_back((now, bytes));
        while self.samples.len() > 2
            && self
                .samples
                .front()
                .is_some_and(|(t, _)| now.saturating_duration_since(*t) > RATE_WINDOW)
        {
            self.samples.pop_front();
        }
        let since = self.moving_since?;
        if now.saturating_duration_since(since) < RATE_SETTLE {
            return None;
        }
        let (t0, b0) = *self.samples.front()?;
        let secs = now.saturating_duration_since(t0).as_secs_f64();
        (secs >= 0.5).then(|| bytes.saturating_sub(b0) as f64 / secs)
    }

    /// Forget every sample: no download is running.
    pub(crate) fn reset(&mut self) {
        self.samples.clear();
        self.moving_since = None;
    }
}

/// Whether this process may draw a meter: stderr is a person's terminal. Never in a test
/// binary, whose stderr is often the terminal that ran it.
#[must_use]
pub(crate) fn wanted() -> bool {
    use std::io::IsTerminal as _;
    !cfg!(test)
        && cfg!(unix)
        && std::io::stderr().is_terminal()
        && std::env::var_os("TERM").is_none_or(|t| t != "dumb")
}

/// Whether the terminal takes UTF-8, from the locale variables in their order of
/// precedence (`LC_ALL`, `LC_CTYPE`, `LANG`; the first one set decides).
fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
}

/// The terminal's width in columns: stderr's window size, else `$COLUMNS`, else 80.
fn columns() -> usize {
    #[cfg(unix)]
    {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `TIOCGWINSZ` writes one `winsize` into the struct handed to it and
        // reads nothing else; fd 2 is this process's own stderr.
        let rc = unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) };
        if rc == 0 && ws.ws_col > 0 {
            return usize::from(ws.ws_col);
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.trim().parse::<usize>().ok())
        .filter(|c| *c > 0)
        .unwrap_or(80)
}

/// What is on the terminal: whether the meter's line is drawn, and how many reasons
/// there are not to draw it (a line left open, a [`hold`]).
struct Screen {
    drawn: bool,
    open_line: bool,
    holds: usize,
}

static SCREEN: Mutex<Screen> = Mutex::new(Screen {
    drawn: false,
    open_line: false,
    holds: 0,
});

/// Whether a [`TtyMeter`] is running in this process — one load on every print.
static LIVE: AtomicBool = AtomicBool::new(false);

fn screen_lock() -> std::sync::MutexGuard<'static, Screen> {
    SCREEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Clear the meter's line if it is drawn.
fn clear(s: &mut Screen) {
    if s.drawn {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(b"\r\x1b[2K");
        let _ = err.flush();
        s.drawn = false;
    }
}

/// Draw `line` in place of the meter's last one (clearing it when `line` is empty),
/// unless something owns the line now.
fn draw(line: &str) {
    let mut s = screen_lock();
    if s.open_line || s.holds > 0 {
        return;
    }
    if line.is_empty() {
        clear(&mut s);
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(b"\r\x1b[2K");
    let _ = err.write_all(line.as_bytes());
    let _ = err.flush();
    s.drawn = true;
}

/// Run `write` — output that `ends_line` or leaves its line open — with the meter's line
/// out of its way. With no meter live, just `write`.
pub(crate) fn around<R>(ends_line: bool, write: impl FnOnce() -> R) -> R {
    if !LIVE.load(Ordering::Acquire) {
        return write();
    }
    let mut s = screen_lock();
    clear(&mut s);
    let out = write();
    s.open_line = !ends_line;
    out
}

/// `println!`, the meter's line out of the way (the crate's `println!` is this).
pub(crate) fn println(args: std::fmt::Arguments<'_>) {
    if !LIVE.load(Ordering::Acquire) {
        std::println!("{args}");
        return;
    }
    let text = args.to_string();
    around(true, || std::println!("{text}"));
}

/// `eprintln!`, the meter's line out of the way (the crate's `eprintln!` is this).
pub(crate) fn eprintln(args: std::fmt::Arguments<'_>) {
    if !LIVE.load(Ordering::Acquire) {
        std::eprintln!("{args}");
        return;
    }
    let text = args.to_string();
    around(true, || std::eprintln!("{text}"));
}

/// `print!`, the meter's line out of the way (the crate's `print!` is this).
pub(crate) fn print(args: std::fmt::Arguments<'_>) {
    if !LIVE.load(Ordering::Acquire) {
        std::print!("{args}");
        return;
    }
    let text = args.to_string();
    around(text.ends_with('\n'), || std::print!("{text}"));
}

/// The meter stays off the terminal while this is alive: a child that owns the terminal
/// (`sudo` asking for a password, an installer's own output) must not be drawn over.
pub(crate) struct Hold(bool);

/// Clear the meter's line and keep it off until the [`Hold`] drops.
#[must_use]
pub(crate) fn hold() -> Hold {
    if !LIVE.load(Ordering::Acquire) {
        return Hold(false);
    }
    let mut s = screen_lock();
    clear(&mut s);
    s.holds += 1;
    Hold(true)
}

impl Drop for Hold {
    fn drop(&mut self) {
        if self.0 {
            let mut s = screen_lock();
            s.holds = s.holds.saturating_sub(1);
            // The child's last line ended wherever it ended; the next draw starts over.
            s.open_line = false;
        }
    }
}

/// The meter: a thread that draws [`render_line`] of what its source shows, every
/// [`TICK`], until it is dropped — which clears the line.
pub(crate) struct TtyMeter {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TtyMeter {
    /// Start drawing what `source` shows. `None` when this process may not draw
    /// ([`wanted`]), or a meter is already running in it.
    pub(crate) fn start(mut source: impl FnMut() -> Show + Send + 'static) -> Option<Self> {
        if !wanted() || LIVE.swap(true, Ordering::AcqRel) {
            return None;
        }
        sigint::arm();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("atpkg-meter".into())
            .spawn(move || {
                let utf8 = utf8_locale();
                let mut rate = Rate::default();
                let mut frame = 0usize;
                while !stop2.load(Ordering::Acquire) {
                    let show = source();
                    let downloading = match &show {
                        Show::Pass { file, focus, .. } => featured(file, focus.as_deref())
                            .is_some_and(|(_, row)| row.phase == Phase::Download)
                            .then_some(file.overall.bytes_done),
                        _ => None,
                    };
                    let bytes_per_sec = match downloading {
                        Some(bytes) => rate.note(Instant::now(), bytes),
                        None => {
                            rate.reset();
                            None
                        }
                    };
                    let look = Look {
                        cols: columns(),
                        frame,
                        utf8,
                    };
                    draw(&render_line(&show, bytes_per_sec, &look));
                    frame = frame.wrapping_add(1);
                    crate::progress::tick_or_stop(&stop2, TICK);
                }
            })
            .ok();
        if handle.is_none() {
            sigint::disarm();
            LIVE.store(false, Ordering::Release);
            return None;
        }
        Some(Self { stop, handle })
    }
}

impl Drop for TtyMeter {
    fn drop(&mut self) {
        crate::progress::stop_and_join(&self.stop, self.handle.take());
        clear(&mut screen_lock());
        LIVE.store(false, Ordering::Release);
        sigint::disarm();
    }
}

/// Ctrl-C while the meter is drawn: clear its line, then die of the signal as before.
/// atpkg installs no handler of its own anywhere else (`crate::noindex` relies on the
/// default action), so this one exists only while a meter runs, and a SIGINT this process
/// was started ignoring (`nohup`) stays ignored.
#[cfg(unix)]
mod sigint {
    use super::{AtomicUsize, Ordering};

    /// The disposition this process had before [`arm`]; `usize::MAX` = not armed.
    static PREVIOUS: AtomicUsize = AtomicUsize::new(usize::MAX);

    extern "C" fn on_sigint(sig: libc::c_int) {
        const CLEAR: &[u8] = b"\r\x1b[2K";
        // SAFETY: `write`, `signal` and `raise` are async-signal-safe; the buffer is a
        // static. The signal is blocked while this runs, so the raise is delivered — with
        // the default action — as the handler returns.
        unsafe {
            libc::write(2, CLEAR.as_ptr().cast(), CLEAR.len());
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    pub(super) fn arm() {
        let handler = on_sigint as extern "C" fn(libc::c_int);
        // SAFETY: installs a handler that only makes async-signal-safe calls; the
        // previous disposition is kept to be restored by `disarm`.
        let previous = unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) };
        if previous == libc::SIG_IGN {
            // SAFETY: puts back the disposition that was there a moment ago.
            unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
            return;
        }
        PREVIOUS.store(previous, Ordering::Release);
    }

    pub(super) fn disarm() {
        let previous = PREVIOUS.swap(usize::MAX, Ordering::AcqRel);
        if previous != usize::MAX {
            // SAFETY: restores the disposition `arm` replaced.
            unsafe { libc::signal(libc::SIGINT, previous) };
        }
    }
}

#[cfg(not(unix))]
mod sigint {
    pub(super) fn arm() {}
    pub(super) fn disarm() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::Overall;
    use std::collections::BTreeMap;

    fn row(phase: Phase, done: u64, total: u64) -> ProgramProgress {
        ProgramProgress {
            phase,
            bytes_done: done,
            bytes_total: total,
            build: None,
            bumped: false,
            bumped_with: None,
            error: None,
        }
    }

    fn file(rows: &[(&str, ProgramProgress)], overall: Overall) -> ProgressFile {
        ProgressFile {
            v: crate::progress::PROGRESS_VERSION,
            pid: Some(1),
            pass: "net".into(),
            started_unix: 0,
            heartbeat_unix: 0,
            overall,
            queue: Vec::new(),
            programs: rows
                .iter()
                .map(|(n, r)| ((*n).to_string(), r.clone()))
                .collect::<BTreeMap<_, _>>(),
            ended_unix: None,
        }
    }

    fn pass(file: ProgressFile) -> Show {
        Show::Pass {
            file,
            focus: None,
            idle: "Checking for updates…".into(),
        }
    }

    const WIDE: Look = Look {
        cols: 120,
        frame: 2,
        utf8: true,
    };

    const GIB: u64 = 1 << 30;

    /// The download line: the program, where it is in the pass, the bar, the bytes, the
    /// rate and the time left — in that order, on one line.
    #[test]
    fn a_download_shows_program_position_bar_bytes_rate_and_time_left() {
        let f = file(
            &[
                ("trust", row(Phase::Download, GIB, 2 * GIB)),
                ("ay", row(Phase::Queued, 0, 0)),
            ],
            Overall {
                programs_done: 2,
                programs_total: 12,
                bytes_done: GIB,
                bytes_total: 3 * GIB,
            },
        );
        let line = render_line(&pass(f), Some(18.0 * 1024.0 * 1024.0), &WIDE);
        assert_eq!(
            line,
            "⠹ Downloading trust (3 of 12)  ▕████████        ▏ 1.0 GiB of 2.0 GiB · 18.0 MiB/s · \
             about 2 min left"
        );
    }

    /// At 80 columns — a terminal's default — the time left is never the part cut: the
    /// rate goes first, then the bar narrows.
    #[test]
    fn the_time_left_survives_an_eighty_column_terminal() {
        let f = file(
            &[("trust", row(Phase::Download, GIB, 2 * GIB))],
            Overall {
                programs_done: 2,
                programs_total: 12,
                bytes_done: GIB,
                bytes_total: 3 * GIB,
            },
        );
        let eighty = Look {
            cols: 80,
            frame: 2,
            utf8: true,
        };
        let line = render_line(&pass(f), Some(18.0 * 1024.0 * 1024.0), &eighty);
        assert_eq!(
            line,
            "⠹ Downloading trust (3 of 12)  ▕████    ▏ 1.0 GiB of 2.0 GiB · about 2 min left"
        );
        assert!(line.chars().count() < 80, "{line}");
    }

    /// No rate yet (it has not settled): the bytes, and no guess at the time left.
    #[test]
    fn a_download_without_a_settled_rate_shows_no_time_left() {
        let f = file(
            &[("ty", row(Phase::Download, 123 << 20, 457 << 20))],
            Overall::default(),
        );
        let line = render_line(&pass(f), None, &WIDE);
        assert_eq!(
            line,
            "⠹ Downloading ty  ▕████▎           ▏ 123.0 MiB of 457.0 MiB"
        );
        assert!(!line.contains("left"), "{line}");
    }

    /// Unpacking shows a percentage; checking and setting up show the word alone.
    #[test]
    fn unpacking_checking_and_setting_up() {
        let unpack = file(
            &[("trust", row(Phase::Extract, 3, 4))],
            Overall {
                programs_done: 0,
                programs_total: 2,
                ..Overall::default()
            },
        );
        assert_eq!(
            render_line(&pass(unpack), None, &WIDE),
            "⠹ Unpacking trust (1 of 2)  ▕████████████    ▏ 75%"
        );
        let verify = file(&[("trust", row(Phase::Verify, 0, 0))], Overall::default());
        assert_eq!(render_line(&pass(verify), None, &WIDE), "⠹ Checking trust…");
        let link = file(&[("clean", row(Phase::Link, 0, 0))], Overall::default());
        assert_eq!(render_line(&pass(link), None, &WIDE), "⠹ Setting up clean…");
    }

    /// A program still downloading outranks one being set up; a pass with nothing in
    /// flight says its idle sentence.
    #[test]
    fn the_line_features_the_program_in_flight_else_the_idle_sentence() {
        let f = file(
            &[
                ("ay", row(Phase::Link, 0, 0)),
                ("trust", row(Phase::Verify, 0, 0)),
                ("zz", row(Phase::Done, 0, 0)),
            ],
            Overall::default(),
        );
        assert_eq!(render_line(&pass(f), None, &WIDE), "⠹ Checking trust…");
        let idle = file(&[("zz", row(Phase::Done, 0, 0))], Overall::default());
        assert_eq!(
            render_line(&pass(idle), None, &WIDE),
            "⠹ Checking for updates…"
        );
        assert_eq!(
            render_line(&Show::Busy("Waiting…".into()), None, &WIDE),
            "⠹ Waiting…"
        );
        assert_eq!(render_line(&Show::Nothing, None, &WIDE), "");
    }

    /// A FOCUS names the program a pending stub waits for, whatever else is in flight —
    /// and never the pass's position or its whole download's time left, which are not
    /// that program's.
    #[test]
    fn a_focus_features_its_own_program() {
        let overall = Overall {
            programs_done: 2,
            programs_total: 12,
            bytes_done: GIB,
            bytes_total: 20 * GIB,
        };
        let f = file(
            &[
                ("trust", row(Phase::Download, 1, 2)),
                ("ty", row(Phase::Queued, 0, 0)),
            ],
            overall,
        );
        let show = Show::Pass {
            file: f,
            focus: Some("ty".into()),
            idle: String::new(),
        };
        assert_eq!(render_line(&show, None, &WIDE), "⠹ Waiting to install ty");
        let f = file(&[("ty", row(Phase::Download, 1 << 20, 2 << 20))], overall);
        let show = Show::Pass {
            file: f,
            focus: Some("ty".into()),
            idle: String::new(),
        };
        assert_eq!(
            render_line(&show, Some(f64::from(1u32 << 20)), &WIDE),
            "⠹ Downloading ty  ▕████████        ▏ 1.0 MiB of 2.0 MiB · 1.0 MiB/s · 5 s left"
        );
    }

    /// ASCII terminals get ASCII glyphs; narrow ones lose the bar, then get cut with a
    /// visible ellipsis — never a line that wraps.
    #[test]
    fn ascii_and_narrow_terminals() {
        let f = file(&[("trust", row(Phase::Download, 1, 2))], Overall::default());
        let ascii = Look {
            cols: 80,
            frame: 1,
            utf8: false,
        };
        assert_eq!(
            render_line(&pass(f.clone()), None, &ascii),
            "/ Downloading trust  [########        ] 1 B of 2 B"
        );
        let narrow = Look {
            cols: 30,
            frame: 0,
            utf8: true,
        };
        let line = render_line(&pass(f), None, &narrow);
        assert_eq!(line.chars().count(), 29, "{line}");
        assert!(line.ends_with('…'), "{line}");
        assert!(!line.contains('▕'), "{line}");
    }

    /// An untrusted name the store would never admit is not echoed to the terminal.
    #[test]
    fn a_hostile_program_name_is_not_echoed() {
        let f = file(
            &[("\u{1b}]0;pwned\u{7}", row(Phase::Verify, 0, 0))],
            Overall::default(),
        );
        assert_eq!(render_line(&pass(f), None, &WIDE), "⠹ Checking a program…");
    }

    #[test]
    fn time_left_rounds_up_and_scales() {
        assert_eq!(time_left(0), "5 s left");
        assert_eq!(time_left(41), "45 s left");
        assert_eq!(time_left(61), "about 2 min left");
        assert_eq!(time_left(7_200), "about 2 h left");
    }

    #[test]
    fn the_bar_fills_in_eighths_and_never_overflows() {
        assert_eq!(bar(0, 8, 4, true), "▕    ▏");
        assert_eq!(bar(1, 8, 4, true), "▕▌   ▏");
        assert_eq!(bar(8, 8, 4, true), "▕████▏");
        assert_eq!(bar(99, 8, 4, true), "▕████▏");
        assert_eq!(bar(4, 8, 4, false), "[##  ]");
        assert_eq!(bar(0, 0, 4, false), "[    ]");
    }

    /// The rate waits for bytes to move for three seconds, averages over the last five,
    /// and starts over when the count goes down (a new pass).
    #[test]
    fn the_rate_settles_then_averages_over_the_window() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut rate = Rate::default();
        assert_eq!(rate.note(at(0), 0), None);
        assert_eq!(rate.note(at(1), 100), None, "moving, not settled");
        assert_eq!(rate.note(at(3), 300), None, "two seconds of movement");
        let r = rate.note(at(4), 400).expect("settled");
        assert!((r - 100.0).abs() < 1e-9, "{r}");
        // Older samples leave the window: 10 s later at 1000 B/s.
        for s in 5..=14 {
            rate.note(at(s), 400 + (s - 4) * 1000);
        }
        let r = rate.note(at(15), 400 + 11 * 1000).expect("settled");
        assert!((r - 1000.0).abs() < 1.0, "{r}");
        assert_eq!(rate.note(at(16), 10), None, "a new pass starts over");
    }
}
