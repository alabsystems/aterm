// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab halfdraw scan`: a tab's cast (`aterm ctl @<sid> cast`, asciicast v2) replayed into the
//! engine aterm draws with, one event per batch, and every state a turn-end reader could read
//! judged: the half-drawn frames of `docs/measured/half-drawn-frame-2026-10-22.md`.
//!
//! A cast's `o` event is one BURST of output as the PTY reader delivered it, not one read: the
//! recorder coalesces the reads of a burst, and the live kernel observes after each of the
//! reader's holds inside it (`aterm-gui`'s `cast.rs`). So the replay sees no finer than the
//! burst. For a program that brackets its redraws in DEC 2026 that loses nothing (any read
//! boundary inside a bracketed redraw lies inside the bracket); for one that brackets nothing,
//! the gaps counted are a lower bound on what the live kernel could see.
//!
//! Per cast it says how many DEC 2026 brackets the program opened and closed, how many events
//! ended inside an open bracket (a frame split across bursts), and every BUSY GAP: a state
//! whose busy row ([`ScreenReader::busy_guard`], over the live zone a supervisor's `await gone`
//! watches) is gone while the state before showed it and a later state shows it again, within
//! [`GAP_WINDOW_S`] — the read a bare row scan latched `gone <busy>` on mid-reply. Each gap says
//! how soon the busy row was back (`back=`), and is classed by what reads it today:
//!
//! - `open`: the program's frame is unfinished there (`Terminal::frame_unfinished_at`), so the
//!   observation kernel holds `await gone`, `text --json` says `"frame":"open"` and the
//!   supervisor waits for the frame (`await frame`) before it reads;
//! - `settle`: whole to the kernel, but no frame the program closed vouches for it
//!   (`Terminal::screen_is_closed_frame`), and the busy row is back within
//!   [`GONE_SETTLE`]: `await gone` settles there and does not latch;
//! - `torn`: neither, and its reader judges the frame not whole ([`ScreenReader::frame_torn`]);
//! - `UNGUARDED`: none of these — a gap `await gone` latches on and a turn-end reader would
//!   read as the screen shows it. Its phase follows (`idle` there is the defect).
//!
//! `--show N` prints the first N gaps' screens; `--event I` prints the screen after event I.

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
    busy: bool,
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

/// The verb.
pub fn cmd_halfdraw(args: &[String]) -> Res {
    let usage = || {
        Fail::usage(format!(
            "usage: {LAB} halfdraw scan <cast> --program claude|agy|muse|gemini [--show N] [--event I]"
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
            "--show" => show = it.next().and_then(|n| n.parse().ok()).ok_or_else(usage)?,
            "--event" => event = Some(it.next().and_then(|n| n.parse().ok()).ok_or_else(usage)?),
            f if file.is_none() && !f.starts_with('-') => file = Some(f.to_string()),
            _ => return Err(usage()),
        }
    }
    let (Some(file), Some(program)) = (file, program) else {
        return Err(usage());
    };
    let text = std::fs::read_to_string(&file).map_err(|e| Fail::die(format!("{file}: {e}")))?;
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

/// The replay's base instant: every event's clock is this plus its cast time, so the replay
/// reads no wall clock after it.
pub(crate) fn base_instant() -> std::time::Instant {
    std::time::Instant::now()
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
        let at = base + std::time::Duration::from_secs_f64(e.at.max(0.0));
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
        let reading = reader.read(&rows, None);
        states.push(State {
            at: e.at,
            open,
            closed: t.screen_is_closed_frame(),
            busy: rows[zone..].iter().any(|r| busy.is_match(r)),
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
        let s = &states[i];
        if s.busy || !states[i - 1].busy {
            continue;
        }
        let Some(back) = states[i + 1..]
            .iter()
            .take_while(|n| n.at - s.at <= GAP_WINDOW_S)
            .find(|n| n.busy)
            .map(|n| n.at - s.at)
        else {
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
            "event {i} t={:.6} open={} closed={} busy={} torn={} phase={}",
            s.at, s.open, s.closed, s.busy, s.torn, s.phase
        );
        for (n, r) in rows.iter().enumerate() {
            let _ = writeln!(out, "  {n:2}|{r}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308";
    const BEFORE: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/before.bin");
    const HALF: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/half.bin");
    const REST: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/rest.bin");
    const END: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/end.bin");

    /// A cast of `reads`, one event each, a millisecond apart, as `aterm ctl cast` writes it.
    fn cast_text(reads: &[&[u8]]) -> String {
        let mut out = String::from("{\"version\": 2, \"width\": 144, \"height\": 50}\n");
        for (i, r) in reads.iter().enumerate() {
            let data = std::str::from_utf8(r).unwrap_or_else(|e| panic!("{DIR}: {e}"));
            let quoted = aterm_json::to_string(&data).expect("a JSON string");
            out.push_str(&format!("[{:.6}, \"o\", {quoted}]\n", i as f64 * 0.001));
        }
        out
    }

    fn scan_claude(reads: &[&[u8]]) -> String {
        let cast = parse_cast(&cast_text(reads)).expect("a cast");
        let reader = Program::Claude.reader();
        let guard = reader.busy_guard().expect("a busy guard");
        scan(
            &cast,
            reader,
            &aterm_regex::Regex::new(&guard).expect("the guard"),
            0,
            None,
        )
    }

    /// Claude Code 2.1.308's MEASURED split (aterm-core's `half-drawn` fixtures): one read ends
    /// inside a frame, and its busy gap is the frame the kernel holds (`open`). NEGATIVE
    /// CONTROL: the same reads with the split joined end inside no frame and leave no gap.
    #[test]
    fn the_measured_split_is_one_open_gap() {
        let split = scan_claude(&[b"\x1b[47;3H", BEFORE, HALF, REST, END]);
        assert!(split.contains(", 1 ending inside an open frame"), "{split}");
        assert!(
            split.contains("busy gaps: 1 (open 1, settle 0, torn 0, UNGUARDED 0"),
            "{split}"
        );
        assert!(split.contains("gap at event 2 "), "{split}");
        let joined: Vec<u8> = [HALF, REST].concat();
        let whole = scan_claude(&[b"\x1b[47;3H", BEFORE, &joined, END]);
        assert!(whole.contains(", 0 ending inside an open frame"), "{whole}");
        assert!(whole.contains("busy gaps: 0 "), "{whole}");
    }

    #[test]
    fn a_cast_keeps_its_output_events_and_refuses_a_bad_line() {
        let text = "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.5, \"o\", \"a\"]\n\
                    [0.6, \"i\", \"k\"]\n[0.7, \"o\", \"b\"]\n";
        let cast = parse_cast(text).expect("a cast");
        assert_eq!((cast.cols, cast.rows, cast.events.len()), (80, 24, 2));
        assert_eq!(cast.events[1].data, "b");
        assert!(parse_cast("{\"version\": 2}\n").is_err(), "no size");
        assert!(parse_cast("{\"width\": 1, \"height\": 1}\n[1, \"o\"]\n").is_err());
    }
}
