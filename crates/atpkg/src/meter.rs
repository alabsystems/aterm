// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The one-line progress meter a person sees under a typed `aterm pkg` verb.

use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// The program the line features: `focus` if the pass has a row for it, else one that is
/// moving (downloading, checking or unpacking before setting up), and only then a download
/// that is in and waits for the pass ([`ProgramProgress::downloaded`]).
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
    let working = |phases: &[Phase], waiting: bool| {
        file.programs
            .iter()
            .find(|(_, row)| phases.contains(&row.phase) && row.downloaded() == waiting)
            .map(|(k, row)| (k.as_str(), row))
    };
    working(&[Phase::Download, Phase::Verify, Phase::Extract], false)
        .or_else(|| working(&[Phase::Link], false))
        .or_else(|| working(&[Phase::Download], true))
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
const fn spinner(frame: usize, utf8: bool) -> char {
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
        format!("{} s left", secs.div_ceil(5).max(1) * 5)
    } else if secs < 100 * 60 {
        format!("about {} min left", secs.div_ceil(60))
    } else {
        format!("about {} h left", secs.div_ceil(3600))
    }
}

/// The meter's line for `show`, pure so every shape is pinned by a test.
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

/// The words after the spinner for one program of a pass.
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
    /// second once bytes have moved for [`RATE_SETTLE`], else `None`.
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

/// Whether this process may draw a meter: stderr is a person's terminal.
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
        .and_then(|c| {
            let c = c.trim();
            (!c.starts_with('+'))
                .then(|| c.parse::<usize>().ok())
                .flatten()
        })
        .filter(|c| *c > 0)
        .unwrap_or(80)
}

/// What is on the terminal: whether the meter's line is drawn.
struct Screen {
    drawn: bool,
}

static SCREEN: Mutex<Screen> = Mutex::new(Screen { drawn: false });

/// Whether a [`TtyMeter`] is running in this process: one at a time.
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

/// Draw `line` in place of the meter's last one (clearing it when `line` is empty).
fn draw(line: &str) {
    let mut s = screen_lock();
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

/// Run `write` — output that ends its line — with the meter's line out of its way: under the
/// screen's lock whether a meter is live or not, so a meter that starts between a look at
/// [`LIVE`] and the write — the store-lock wait's spinner two seconds in, beside a pass
/// edge's `[machine]` line — draws after the line, never on the row it is still filling.
/// Nothing drawn, the lock is all it costs.
pub(crate) fn around<R>(write: impl FnOnce() -> R) -> R {
    let mut s = screen_lock();
    clear(&mut s);
    write()
}

/// `println!`, the meter's line out of the way (the crate's `println!` is this).
pub(crate) fn println(args: std::fmt::Arguments<'_>) {
    around(|| aterm_uds::stdout::println(args));
}

/// `eprintln!`, the meter's line out of the way (the crate's `eprintln!` is this).
pub(crate) fn eprintln(args: std::fmt::Arguments<'_>) {
    around(|| std::eprintln!("{args}"));
}

/// The meter: a thread that draws [`render_line`] of what its source shows, every
/// [`TICK`], until it is dropped — which clears the line.
pub(crate) struct TtyMeter {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TtyMeter {
    /// Start drawing what `source` shows.
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
#[cfg(unix)]
mod sigint {
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            #[expect(
                clippy::disallowed_methods,
                reason = "re-raises SIGINT with its default action, which dumps no core"
            )]
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
#[path = "meter_tests.rs"]
mod tests;
