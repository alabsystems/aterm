// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab halfdraw scan`: replays an asciicast v2 into a terminal and classifies busy-row gaps.

use aterm_core::terminal::{ClockReading, GONE_SETTLE, SurfaceSource, Terminal};
use aterm_phase::reader::{Program, ScreenReader};

use crate::{Fail, LAB, Res};

/// How soon the busy row must come back for its absence to be a gap in a running turn rather
/// than the turn's end, in the cast's seconds: the stub's tall reply streams a line every 20 ms
/// and every program measured redraws within 0.25 s of its last frame.
const GAP_WINDOW_S: f64 = 2.0;

/// One output event of a cast.
pub(crate) struct Event {
    pub(crate) at: f64,
    pub(crate) data: String,
}

pub(crate) struct Cast {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) events: Vec<Event>,
}

pub(crate) fn parse_cast(text: &str) -> Result<Cast, String> {
    let mut lines = text.lines();
    let head: aterm_json::Value = aterm_json::from_str(lines.next().ok_or("empty cast")?)
        .map_err(|e| format!("cast header: {e}"))?;
    let dim = |k: &str| {
        head.get(k)
            .and_then(aterm_json::Value::as_u64)
            .and_then(|v| u16::try_from(v).ok())
            .filter(|&v| v > 0)
            .ok_or_else(|| format!("cast header: no {k}"))
    };
    let (cols, rows) = (dim("width")?, dim("height")?);
    let mut events = Vec::new();
    for (n, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: aterm_json::Value =
            aterm_json::from_str(line).map_err(|e| format!("cast line {}: {e}", n + 2))?;
        let a = v
            .as_array()
            .ok_or_else(|| format!("cast line {}: no array", n + 2))?;
        let (Some(at), Some(kind), Some(data)) = (
            a.first().and_then(aterm_json::Value::as_f64),
            a.get(1).and_then(aterm_json::Value::as_str),
            a.get(2).and_then(aterm_json::Value::as_str),
        ) else {
            return Err(format!("cast line {}: not [t, kind, data]", n + 2));
        };
        if !at.is_finite() || !(0.0..=1e9).contains(&at) {
            return Err(format!("cast line {}: bad timestamp", n + 2));
        }
        if kind == "o" {
            events.push(Event {
                at,
                data: data.to_string(),
            });
        }
    }
    Ok(Cast { cols, rows, events })
}

/// What one replayed state shows.
struct State {
    at: f64,
    open: bool,
    /// The screen is a frame the program closed, nothing drawn since.
    closed: bool,
    /// The busy row ([`ScreenReader::busy_guard`]) is drawn: the read `await gone <guard>`
    /// answers on, which reads no title.
    row: bool,
    /// The title runs and the reading beside it ([`ScreenReader::read_titled`]) is Busy: the
    /// harness's read of a turn whose screen draws no busy row, a reply under a status line.
    held: bool,
    torn: bool,
    phase: String,
    composer: bool,
}

pub(crate) fn screen(t: &Terminal) -> Vec<String> {
    (0..SurfaceSource::rows(t))
        .map(|i| {
            let mut r = SurfaceSource::row_text(t, i).unwrap_or_default();
            r.truncate(r.trim_end().len());
            r
        })
        .collect()
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// Runs `lab halfdraw scan`.
pub fn cmd_halfdraw(args: &[String]) -> Res {
    let usage = || {
        Fail::usage(format!(
            "usage: {LAB} halfdraw scan <cast> --program claude|agy|muse [--show N] [--event I]"
        ))
    };
    if args.first().map(String::as_str) != Some("scan") {
        return Err(usage());
    }
    let (mut file, mut program, mut show, mut event) = (None, None, 0usize, None);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--program" => {
                let name = it.next().ok_or_else(usage)?;
                program = Some(
                    Program::ALL
                        .into_iter()
                        .find(|p| p.name() == name)
                        .ok_or_else(|| Fail::usage(format!("halfdraw: no program '{name}'")))?,
                );
            }
            "--show" => {
                show = it
                    .next()
                    .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(usage)?;
            }
            "--event" => {
                event = Some(
                    it.next()
                        .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
                        .and_then(|n| n.parse().ok())
                        .ok_or_else(usage)?,
                );
            }
            f if file.is_none() && !f.starts_with('-') => file = Some(f.to_string()),
            _ => return Err(usage()),
        }
    }
    let (Some(file), Some(program)) = (file, program) else {
        return Err(usage());
    };
    let text = crate::util::read_regular_text(std::path::Path::new(&file))
        .map_err(|e| Fail::die(format!("{file}: {e}")))?;
    let cast = parse_cast(&text).map_err(|e| Fail::die(format!("{file}: {e}")))?;
    let reader = program.reader();
    let guard = reader
        .busy_guard()
        .ok_or_else(|| Fail::die(format!("halfdraw: {} has no busy guard", program.name())))?;
    let busy_re = aterm_regex::Regex::new(&guard)
        .map_err(|e| Fail::die(format!("halfdraw: busy guard {guard}: {e}")))?;
    print!("{}", scan(&cast, reader, &busy_re, show, event));
    Ok(0)
}

/// Returns the base instant to which cast event timestamps are added.
pub(crate) fn base_instant() -> std::time::Instant {
    std::time::Instant::now()
}

pub(crate) fn event_instant(base: std::time::Instant, at: f64) -> std::time::Instant {
    std::time::Duration::try_from_secs_f64(at.max(0.0))
        .ok()
        .and_then(|d| base.checked_add(d))
        .unwrap_or(base)
}

/// The scan's report, as text.
fn scan(
    cast: &Cast,
    reader: &dyn ScreenReader,
    busy: &aterm_regex::Regex,
    show: usize,
    event: Option<usize>,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let base = base_instant();
    let mut t = Terminal::new(cast.rows, cast.cols);
    let mut states = Vec::with_capacity(cast.events.len());
    let mut screens = Vec::new();
    let (mut bsu, mut esu, mut splits) = (0usize, 0usize, 0usize);
    for (i, e) in cast.events.iter().enumerate() {
        let at = event_instant(base, e.at);
        t.process_at(
            e.data.as_bytes(),
            ClockReading {
                monotonic: at,
                wall_ms: None,
            },
        );
        bsu += count(&e.data, "\x1b[?2026h");
        esu += count(&e.data, "\x1b[?2026l");
        let open = t.frame_unfinished_at(at);
        splits += usize::from(t.sync_frame_unfinished());
        let rows = screen(&t);
        let zone = reader.busy_zone_start(&rows).min(rows.len());
        // The program's title read with the rows, as the harness reads them: under a status
        // line Claude Code draws its reply with no busy row, and its title's run glyph is the
        // turn's one sign.
        let runs = reader.runs(Some(t.title()));
        let reading = reader.read_titled(&rows, None, runs);
        states.push(State {
            at: e.at,
            open,
            closed: t.screen_is_closed_frame(),
            row: rows[zone..].iter().any(|r| busy.is_match(r)),
            held: runs == Some(true) && reading.phase == aterm_phase::phase::Phase::Busy,
            torn: reader.frame_torn(&rows),
            phase: reading.phase.name().to_string(),
            composer: reading.composer.is_some(),
        });
        screens.push(if event == Some(i) || show > 0 {
            Some(rows)
        } else {
            None
        });
    }
    let _ = writeln!(
        out,
        "{} {}x{}: {} events, {bsu} ?2026h, {esu} ?2026l, {splits} ending inside an open frame",
        reader.program().name(),
        cast.cols,
        cast.rows,
        cast.events.len()
    );
    let (mut open, mut settle, mut torn, mut unguarded, mut idle) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut shown = 0usize;
    for i in 1..states.len() {
        let (s, before) = (&states[i], &states[i - 1]);
        let back_of = |sign: fn(&State) -> bool| {
            states[i + 1..]
                .iter()
                .take_while(|n| n.at - s.at <= GAP_WINDOW_S)
                .find(|n| sign(n))
                .map(|n| n.at - s.at)
        };
        // A gap is the busy row gone mid-turn, the read `await gone <guard>` answers on whatever
        // the title says; or, with no busy row drawn, the titled busy read gone mid-turn, the
        // harness's read under a status line. Each comes back by its own sign. A cast no title
        // runs over holds nothing, so its busy row alone decides, as for every cast before.
        let row_gap = (before.row && !s.row).then(|| back_of(|n| n.row)).flatten();
        let held_gap = (before.held && !s.held && !s.row)
            .then(|| back_of(|n| n.held))
            .flatten();
        let Some(back) = row_gap.or(held_gap) else {
            continue;
        };
        let class = if s.open {
            open += 1;
            "open"
        } else if !s.closed && back < GONE_SETTLE.as_secs_f64() {
            settle += 1;
            "settle"
        } else if s.torn {
            torn += 1;
            "torn"
        } else {
            unguarded += 1;
            idle += usize::from(s.phase == "idle");
            "UNGUARDED"
        };
        let _ = writeln!(
            out,
            "gap at event {i} t={:.6} bytes={} back=+{:.3}ms {class} phase={} composer={}",
            s.at,
            cast.events[i].data.len(),
            back * 1000.0,
            s.phase,
            if s.composer { "yes" } else { "no" },
        );
        if shown < show {
            shown += 1;
            if let Some(rows) = &screens[i] {
                for (n, r) in rows.iter().enumerate() {
                    let _ = writeln!(out, "  {n:2}|{r}");
                }
            }
        }
    }
    let busy_states = states.iter().filter(|s| s.row || s.held).count();
    let _ = writeln!(
        out,
        "busy states: {busy_states}{}",
        if busy_states == 0 {
            " (blind: no read showed the busy row or read busy under a running title, so no gap could be seen)"
        } else {
            ""
        }
    );
    let _ = writeln!(
        out,
        "busy gaps: {} (open {open}, settle {settle}, torn {torn}, UNGUARDED {unguarded}, of them idle {idle})",
        open + settle + torn + unguarded
    );
    if let Some(i) = event
        && let Some(Some(rows)) = screens.get(i)
    {
        let s = &states[i];
        let _ = writeln!(
            out,
            "event {i} t={:.6} open={} closed={} row={} held={} torn={} phase={}",
            s.at, s.open, s.closed, s.row, s.held, s.torn, s.phase
        );
        for (n, r) in rows.iter().enumerate() {
            let _ = writeln!(out, "  {n:2}|{r}");
        }
    }
    out
}

/// A cast of whole frames, one event each a millisecond apart: each frame's rows drawn from
/// the top, the cursor on its composer's caret, under the window title given (none set where
/// it is `None`).
#[cfg(test)]
pub(crate) fn frames_cast(frames: &[(&[String], Option<&str>)]) -> String {
    let mut out = String::from("{\"version\": 2, \"width\": 144, \"height\": 20}\n");
    for (i, (rows, title)) in frames.iter().enumerate() {
        let mut data = title.map_or_else(String::new, |t| format!("\x1b]2;{t}\x07"));
        data.push_str("\x1b[2J");
        for (n, row) in rows.iter().enumerate() {
            data.push_str(&format!("\x1b[{};1H{row}", n + 1));
        }
        if let Some((caret, _)) = aterm_phase::phase::composer_draft(rows) {
            data.push_str(&format!("\x1b[{};3H", caret + 1));
        }
        let quoted = aterm_json::to_string(&data).expect("a JSON string");
        out.push_str(&format!("[{:.6}, \"o\", {quoted}]\n", i as f64 * 0.001));
    }
    out
}

#[cfg(test)]
#[path = "halfdraw_tests.rs"]
mod tests;
