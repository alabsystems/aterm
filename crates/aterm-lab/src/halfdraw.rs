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

    /// The same MEASURED split under the running title Claude Code 2.1.308 sets mid-turn (its
    /// `end.bin` runs `◑`/`◐`): `await gone <guard>` reads no title, so the half frame is still
    /// the one open gap, though the reading beside the title calls it busy. NEGATIVE CONTROL: the
    /// split joined under the same title leaves no gap.
    #[test]
    fn a_running_title_hides_no_busy_row_gap() {
        let title = "\x1b]0;\u{25D0} Claude Code\x07\x1b[47;3H";
        let split = scan_claude(&[title.as_bytes(), BEFORE, HALF, REST, END]);
        assert!(
            split.contains("busy gaps: 1 (open 1, settle 0, torn 0, UNGUARDED 0"),
            "{split}"
        );
        assert!(split.contains("gap at event 2 "), "{split}");
        assert!(split.contains(" open phase=busy "), "{split}");
        let joined: Vec<u8> = [HALF, REST].concat();
        let whole = scan_claude(&[title.as_bytes(), BEFORE, &joined, END]);
        assert!(whole.contains("busy gaps: 0 "), "CONTROL: {whole}");
    }

    /// Claude Code 2.1.311's spinner row, drawn above a status line's composer with no
    /// `esc to interrupt`, is no busy row; under a running title the reading beside it is busy
    /// all the same, so a spinner frame between title-held reply frames is no gap and a cast of
    /// spinner frames alone is not blind. NEGATIVE CONTROL: the titles stripped, no read is busy.
    #[test]
    fn a_spinner_under_a_running_title_is_busy() {
        use aterm_phase::prompt::fixtures::{STATUSLINE_MIDREPLY_2_1_311, screen as rows_of};
        let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
        let (top, _) = aterm_phase::phase::composer_rules(&mid).expect("a composer");
        let mut spinning = mid.clone();
        spinning.insert(
            top,
            "\u{2736} Deliberating\u{2026} (4s \u{00B7} \u{2193} 120 tokens)".to_string(),
        );
        let (a, b) = (Some("\u{25D0} Claude Code"), Some("\u{25D1} Claude Code"));
        let between = scan_status_line(&[(&mid, a), (&spinning, b), (&mid, a)]);
        assert!(between.contains("\nbusy states: 3\n"), "{between}");
        assert!(between.contains("busy gaps: 0 "), "{between}");
        let alone = scan_status_line(&[(&spinning, a), (&spinning, b), (&spinning, a)]);
        assert!(alone.contains("\nbusy states: 3\n"), "{alone}");
        let stripped = scan_status_line(&[(&mid, None), (&spinning, None), (&mid, None)]);
        assert!(
            stripped.contains("\nbusy states: 0 (blind: "),
            "CONTROL: {stripped}"
        );
    }

    /// Under a status line the turn's one sign is the reading beside its running title: a frame
    /// mid-reply that it does not read busy (here the composer cut, so the title holds nothing)
    /// is a gap, the read the harness takes there. NEGATIVE CONTROL: the composer kept, no gap.
    #[test]
    fn under_a_status_line_a_gap_is_the_titled_busy_read_gone() {
        use aterm_phase::prompt::fixtures::{STATUSLINE_MIDREPLY_2_1_311, screen as rows_of};
        let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
        let (top, _) = aterm_phase::phase::composer_rules(&mid).expect("a composer");
        let cut: Vec<String> = mid[..top].to_vec();
        let (a, b) = (Some("\u{25D0} Claude Code"), Some("\u{25D1} Claude Code"));
        let gap = scan_status_line(&[(&mid, a), (&cut, b), (&mid, a)]);
        assert!(gap.contains("busy gaps: 1 "), "{gap}");
        assert!(gap.contains("gap at event 1 "), "{gap}");
        let kept = scan_status_line(&[(&mid, a), (&mid, b), (&mid, a)]);
        assert!(kept.contains("busy gaps: 0 "), "CONTROL: {kept}");
    }

    /// `frames_cast` of `frames` scanned as Claude Code's.
    fn scan_status_line(frames: &[(&Vec<String>, Option<&str>)]) -> String {
        let frames: Vec<(&[String], Option<&str>)> =
            frames.iter().map(|(r, t)| (r.as_slice(), *t)).collect();
        let cast = parse_cast(&frames_cast(&frames)).expect("a cast");
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

    /// Claude Code 2.1.311's status-line reply (lab `mr`, MEASURED frames) draws no busy row: the
    /// scan counts the reads its running title holds busy and finds no gap. NEGATIVE CONTROL: the
    /// same cast with its titles stripped shows no busy state, and the scan says it was blind.
    #[test]
    fn a_status_line_reply_is_seen_busy_by_its_title() {
        use aterm_phase::prompt::fixtures::{
            STATUSLINE_MIDREPLY_2_1_311, STATUSLINE_REPLY_DONE_2_1_311, screen as rows_of,
        };
        let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
        let done = rows_of(STATUSLINE_REPLY_DONE_2_1_311);
        let scan_of = |titles: [Option<&str>; 3]| {
            let text = frames_cast(&[
                (mid.as_slice(), titles[0]),
                (mid.as_slice(), titles[1]),
                (done.as_slice(), titles[2]),
            ]);
            let cast = parse_cast(&text).expect("a cast");
            let reader = Program::Claude.reader();
            let guard = reader.busy_guard().expect("a busy guard");
            scan(
                &cast,
                reader,
                &aterm_regex::Regex::new(&guard).expect("the guard"),
                0,
                None,
            )
        };
        let titled = scan_of([
            Some("\u{25D0} Claude Code"),
            Some("\u{25D1} Claude Code"),
            Some("\u{2733} Claude Code"),
        ]);
        assert!(titled.contains("\nbusy states: 2\n"), "{titled}");
        assert!(titled.contains("busy gaps: 0 "), "{titled}");
        let stripped = scan_of([None, None, None]);
        assert!(
            stripped.contains("\nbusy states: 0 (blind: "),
            "CONTROL: {stripped}"
        );
    }

    /// With no running title a read is busy by its busy row alone, as before the scan read titles:
    /// a frame that keeps Claude Code's spinner row but has lost its footer's `esc to interrupt`
    /// is a busy gap, the read `await gone esc.to.interrupt` answers on, though the reader still
    /// reads the spinner as a live turn. NEGATIVE CONTROL: the footer kept, no gap.
    #[test]
    fn with_no_running_title_a_gap_is_the_busy_row_gone() {
        use aterm_phase::prompt::fixtures::{STATUSLINE_MIDREPLY_2_1_311, screen as rows_of};
        let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
        let (top, _) = aterm_phase::phase::composer_rules(&mid).expect("a composer");
        let mut torn = mid.clone();
        torn.insert(
            top,
            "\u{2736} Deliberating\u{2026} (4s \u{00B7} \u{2193} 120 tokens)".to_string(),
        );
        let reader = Program::Claude.reader();
        assert_eq!(
            reader.phase(&torn),
            aterm_phase::phase::Phase::Busy,
            "the reader reads the spinner"
        );
        let mode = torn
            .iter()
            .position(|r| r.contains("auto mode on"))
            .expect("the mode row");
        let mut shown = torn.clone();
        shown[mode].push_str(" \u{00B7} esc to interrupt");
        let scan_of = |middle: &[String]| {
            let text = frames_cast(&[
                (shown.as_slice(), None),
                (middle, None),
                (shown.as_slice(), None),
            ]);
            let cast = parse_cast(&text).expect("a cast");
            let guard = reader.busy_guard().expect("a busy guard");
            scan(
                &cast,
                reader,
                &aterm_regex::Regex::new(&guard).expect("the guard"),
                0,
                None,
            )
        };
        let gap = scan_of(&torn);
        assert!(gap.contains("\nbusy states: 2\n"), "{gap}");
        assert!(gap.contains("busy gaps: 1 "), "{gap}");
        assert!(gap.contains("gap at event 1 "), "{gap}");
        let kept = scan_of(&shown);
        assert!(kept.contains("busy gaps: 0 "), "CONTROL: {kept}");
    }

    #[test]
    fn a_cast_keeps_its_output_events_and_refuses_a_bad_line() {
        let text = "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.5, \"o\", \"a\"]\n\
                    [0.6, \"i\", \"k\"]\n[0.7, \"o\", \"b\"]\n";
        let cast = parse_cast(text).expect("a cast");
        assert_eq!((cast.cols, cast.rows, cast.events.len()), (80, 24, 2));
        assert_eq!(cast.events[1].data, "b");
        assert!(parse_cast("{\"version\": 2}\n").is_err(), "no size");
        assert!(parse_cast("{\"width\": 0, \"height\": 24}\n").is_err());
        assert!(parse_cast("{\"width\": 80, \"height\": 0}\n").is_err());
        assert!(parse_cast("{\"width\": 1, \"height\": 1}\n[1, \"o\"]\n").is_err());
        assert!(parse_cast("{\"width\": 1, \"height\": 1}\n[-0.1, \"o\", \"a\"]\n").is_err());
        assert!(parse_cast("{\"width\": 1, \"height\": 1}\n[1e100, \"o\", \"a\"]\n").is_err());
    }
}
