// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole frames only (the observation kernel's properties 3 and 4): a row predicate never reads
//! a frame the program has not finished, and `await gone` never latches on a redraw split
//! across reads. The bytes are MEASURED: the lab's casts of Claude Code 2.1.308, agy 1.4.0 and
//! Gemini CLI 0.46.0 replying the stub's tall landing report on a 144x50 tab, three tabs at once
//! (`docs/measured/half-drawn-frame-2026-10-22.md`). A cast event is one burst of output as the
//! PTY reader delivered it; `half.bin` is a burst that ended partway through a redraw,
//! `rest.bin` the next burst, which finishes it (Claude Code 0.17 ms later, agy 1.2 ms, Gemini
//! CLI 0.42 ms), `before.bin` the whole frames before and `end.bin` the frames to the reply's
//! end. Claude Code and agy bracket every redraw in DEC 2026, so their half frame is an
//! unfinished bracket; Gemini CLI brackets nothing and clears and redraws its whole screen per
//! frame, so its half frame is whole to the kernel. Each half frame shows no busy row (Claude
//! Code's `esc to interrupt` footer, agy's and Gemini CLI's spinner rows): it reads `gone`,
//! which is the defect (`await gone esc.to.interrupt` latched mid-reply, fl8 seq 19029, fl9 seq
//! 2061, the lab's hd1 Gemini CLI's `await gone` in 24 of 24 replies of the gm3-b
//! rerun). MUTANTS: the same rows read as a frame the program closed (`Frame::CLOSED`) latch
//! `gone`. NEGATIVE CONTROLS: the reply's real end latches (at once on a closed frame, after
//! `GONE_SETTLE` on an unbracketed one); a bracket with nothing written in it is no frame.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_core::terminal::{
    ClockReading, FRAME_HANG, Frame, GONE_SETTLE, RowMatch, RowRange, RowVerdict, SurfaceSource,
    Terminal, WatcherSet, WatcherSpec,
};

/// The instant a test mints and feeds in: the engine itself reads no clock.
#[expect(
    clippy::disallowed_methods,
    reason = "test code: the one instant a test mints and feeds in"
)]
fn minted_now() -> Instant {
    Instant::now()
}

fn at(base: Instant, off_us: u64) -> Instant {
    base + Duration::from_micros(off_us)
}

fn clock(base: Instant, off_us: u64) -> ClockReading {
    ClockReading {
        monotonic: at(base, off_us),
        wall_ms: Some(off_us / 1000),
    }
}

/// Claude Code's busy footer, as the supervisor's busy guard spells it (`esc to interrupt`).
#[derive(Debug)]
struct Contains(&'static str);
impl RowMatch for Contains {
    fn verdict(&self, row: &str) -> RowVerdict {
        row.contains(self.0).into()
    }
}

/// agy's busy row, as its reader's busy guard spells it (`^[⠀-⣿]\s\s\S`): a braille spinner
/// at the row's start, two spaces, then a word.
#[derive(Debug)]
struct AgySpinner;
impl RowMatch for AgySpinner {
    fn verdict(&self, row: &str) -> RowVerdict {
        let mut c = row.chars();
        let spin = c
            .next()
            .is_some_and(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch));
        let gap =
            c.next().is_some_and(char::is_whitespace) && c.next().is_some_and(char::is_whitespace);
        (spin && gap && c.next().is_some_and(|ch| !ch.is_whitespace())).into()
    }
}

/// Gemini CLI's status row, as its reader's busy guard spells it (`^\s?[⠀-⣿]\s\S`): a
/// braille spinner in column 0 or 1, a blank, then a word.
#[derive(Debug)]
struct GeminiSpinner;
impl RowMatch for GeminiSpinner {
    fn verdict(&self, row: &str) -> RowVerdict {
        let row = row.strip_prefix(' ').unwrap_or(row);
        let mut c = row.chars();
        let spin = c
            .next()
            .is_some_and(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch));
        let gap = c.next().is_some_and(char::is_whitespace);
        (spin && gap && c.next().is_some_and(|ch| !ch.is_whitespace())).into()
    }
}

/// The rows a supervisor's `await gone` watches: the live zone of a 50-row read.
const LIVE: RowRange = RowRange::Span { start: 10, end: 49 };

fn rows(t: &Terminal) -> Vec<String> {
    (0..SurfaceSource::rows(t))
        .map(|i| SurfaceSource::row_text(t, i).unwrap_or_default())
        .collect()
}

fn shows(t: &Terminal, m: &dyn RowMatch) -> bool {
    rows(t)[10..].iter().any(|r| m.matches(r))
}

fn dump(t: &Terminal) -> String {
    rows(t)
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{i:2}|{}\n", r.trim_end()))
        .collect()
}

struct Capture {
    before: &'static [u8],
    half: &'static [u8],
    rest: &'static [u8],
    end: &'static [u8],
    /// Where the program's cursor rested before `before`: its frames move relative to it.
    cursor: &'static [u8],
}

const CLAUDE: Capture = Capture {
    before: include_bytes!("fixtures/half-drawn/claude-2.1.308/before.bin"),
    half: include_bytes!("fixtures/half-drawn/claude-2.1.308/half.bin"),
    rest: include_bytes!("fixtures/half-drawn/claude-2.1.308/rest.bin"),
    end: include_bytes!("fixtures/half-drawn/claude-2.1.308/end.bin"),
    // Each frame ends `\r\r\n\x1b[2C\x1b[3A`: the composer's caret, row 47 col 3.
    cursor: b"\x1b[47;3H",
};

const AGY: Capture = Capture {
    before: include_bytes!("fixtures/half-drawn/agy-1.4.0/before.bin"),
    half: include_bytes!("fixtures/half-drawn/agy-1.4.0/half.bin"),
    rest: include_bytes!("fixtures/half-drawn/agy-1.4.0/rest.bin"),
    end: include_bytes!("fixtures/half-drawn/agy-1.4.0/end.bin"),
    // Each frame ends `\r\x1b[2A\x1b[2C` from the last row: the composer's caret.
    cursor: b"\x1b[48;3H",
};

/// Gemini CLI's frames carry their own `ESC[2J ESC[3J ESC[H`: no cursor to set first.
const GEMINI: Capture = Capture {
    before: include_bytes!("fixtures/half-drawn/gemini-0.46.0/before.bin"),
    half: include_bytes!("fixtures/half-drawn/gemini-0.46.0/half.bin"),
    rest: include_bytes!("fixtures/half-drawn/gemini-0.46.0/rest.bin"),
    end: include_bytes!("fixtures/half-drawn/gemini-0.46.0/end.bin"),
    cursor: b"",
};

/// A 144x50 terminal holding `cap`'s whole frames before the split, at `base`.
fn before(cap: &Capture, base: Instant) -> Terminal {
    let mut t = Terminal::new(50, 144);
    t.process_at(cap.cursor, clock(base, 0));
    t.process_at(cap.before, clock(base, 1_000));
    t
}

/// The split read, the read that finishes it, and the reply's end, each one batch, with
/// `await gone <busy>` armed on the whole frame before: it holds through the half frame and
/// the whole busy frame after it, and latches at the reply's end.
fn gone_holds_through_the_split(cap: &Capture, busy: &'static dyn RowMatch) {
    let base = minted_now();
    let mut t = before(cap, base);
    assert!(!t.sync_frame_unfinished(), "whole before the split");
    assert!(shows(&t, busy), "busy before the split:\n{}", dump(&t));
    let busy_arc: Arc<dyn RowMatch> = Arc::new(Wrap(busy));
    let gone = t
        .watch(
            WatcherSpec::RowsClear {
                matcher: busy_arc.clone(),
                rows: LIVE,
            },
            at(base, 1_000),
        )
        .expect("arm gone");
    assert!(t.watch_poll(gone).is_none(), "busy at arm");

    t.process_at(cap.half, clock(base, 2_000));
    assert!(
        t.sync_frame_unfinished(),
        "the read ended inside the bracket"
    );
    assert!(t.frame_unfinished_at(at(base, 2_000)));
    // The defect's evidence: the half frame shows no busy row.
    assert!(!shows(&t, busy), "the half frame reads gone:\n{}", dump(&t));
    assert!(
        t.watch_poll(gone).is_none(),
        "gone latched on a half-drawn frame:\n{}",
        dump(&t)
    );
    // `await frame` armed on the half frame waits for the close.
    let frame = t
        .watch(WatcherSpec::FrameWhole, at(base, 2_000))
        .expect("arm frame");
    assert!(t.watch_poll(frame).is_none(), "the frame is open");
    assert_eq!(
        t.watch_next_deadline(),
        Some(at(base, 2_000) + FRAME_HANG),
        "the hang detector is the wait's deadline"
    );

    t.process_at(cap.rest, clock(base, 2_200));
    assert!(!t.sync_frame_unfinished(), "the close finished the frame");
    assert!(shows(&t, busy), "the whole frame is busy:\n{}", dump(&t));
    assert!(
        t.watch_poll(frame).is_some(),
        "await frame latches at the close"
    );
    assert!(
        t.watch_poll(gone).is_none(),
        "the scan the half frame held read the whole frame, still busy"
    );

    t.process_at(cap.end, clock(base, 3_000));
    let sat = t
        .watch_poll(gone)
        .unwrap_or_else(|| panic!("the reply's end latches gone:\n{}", dump(&t)));
    assert_eq!(sat.seq, t.content_seq());
    assert_eq!(sat.at, at(base, 3_000), "a closed frame needs no settle");
    assert!(
        t.screen_is_closed_frame(),
        "the reply ends on a closed frame"
    );
    assert!(!shows(&t, busy));
    assert!(!t.sync_frame_unfinished());
}

/// MUTANT: the kernel minus the frame gate. The half frame's own rows, read as a whole frame,
/// latch `gone`; read as the unfinished frame they are, they do not, and the first whole pass
/// after scans what it was owed.
fn ungated_kernel_latches_on_the_half_frame(cap: &Capture, busy: &'static dyn RowMatch) {
    let base = minted_now();
    let mut t = before(cap, base);
    t.process_at(cap.half, clock(base, 2_000));
    let half: Vec<Option<String>> = rows(&t).into_iter().map(Some).collect();
    let seq = t.content_seq();
    let spec = || WatcherSpec::RowsClear {
        matcher: Arc::new(Wrap(busy)),
        rows: LIVE,
    };

    let mut mutant = WatcherSet::default();
    let id = mutant.arm(spec(), at(base, 2_000)).expect("arm");
    mutant.observe_screen(seq, false, 0, false, at(base, 2_000), &half, Frame::CLOSED);
    assert!(
        mutant.poll(id).is_some(),
        "MUTANT: a kernel that reads the half frame as whole latches gone"
    );

    let open = Frame {
        whole: false,
        hang: Some(at(base, 2_000) + FRAME_HANG),
        closed: false,
        drawn_at: Some(at(base, 2_000)),
    };
    let mut gated = WatcherSet::default();
    let id = gated.arm(spec(), at(base, 2_000)).expect("arm");
    gated.observe_screen(seq, false, 0, false, at(base, 2_000), &half, open);
    assert!(gated.poll(id).is_none(), "the gate holds the half frame");
    assert_eq!(gated.row_scan_hull(false, half.len()), Some((10, 49)));
    // The owed scan runs on the next whole pass, with no content advance (a lone `?2026l`).
    gated.observe_screen(seq, false, 0, false, at(base, 2_100), &half, Frame::CLOSED);
    assert!(gated.poll(id).is_some(), "owed scan ran on the whole pass");
}

/// Adapts a `'static` matcher reference to the kernel's `Arc<dyn RowMatch>`.
#[derive(Debug)]
struct Wrap(&'static dyn RowMatch);
impl RowMatch for Wrap {
    fn verdict(&self, row: &str) -> RowVerdict {
        self.0.verdict(row)
    }
}

static ESC_TO_INTERRUPT: Contains = Contains("esc to interrupt");
static AGY_SPINNER: AgySpinner = AgySpinner;
static GEMINI_SPINNER: GeminiSpinner = GeminiSpinner;

#[test]
fn claude_code_gone_holds_through_a_split_frame() {
    gone_holds_through_the_split(&CLAUDE, &ESC_TO_INTERRUPT);
}

#[test]
fn agy_gone_holds_through_a_split_frame() {
    gone_holds_through_the_split(&AGY, &AGY_SPINNER);
}

#[test]
fn mutant_claude_code_half_frame_reads_gone_without_the_gate() {
    ungated_kernel_latches_on_the_half_frame(&CLAUDE, &ESC_TO_INTERRUPT);
}

#[test]
fn mutant_agy_half_frame_reads_gone_without_the_gate() {
    ungated_kernel_latches_on_the_half_frame(&AGY, &AGY_SPINNER);
}

/// The measured half frame of Claude Code is the fl8 shape: the composer's bottom rule cut
/// at 109 of 144 cells on the grid's last row, the busy footer not yet under it.
#[test]
fn claude_code_half_frame_is_the_fl8_shape() {
    let base = minted_now();
    let mut t = before(&CLAUDE, base);
    t.process_at(CLAUDE.half, clock(base, 2_000));
    let r = rows(&t);
    let last = r[49].trim_end();
    assert_eq!(last.chars().count(), 109, "{}", dump(&t));
    assert!(last.chars().all(|c| c == '─'), "{}", dump(&t));
    t.process_at(CLAUDE.rest, clock(base, 2_200));
    let r = rows(&t);
    let rule = r
        .iter()
        .rposition(|l| l.starts_with('─'))
        .expect("a bottom rule");
    assert_eq!(r[rule].trim_end().chars().count(), 144, "{}", dump(&t));
    assert!(r[rule + 1].contains("esc to interrupt"), "{}", dump(&t));
}

/// An update park holds the hang detector, as it holds the idle clocks: the frame's close may be
/// waiting unread, so the detector neither fires nor names a deadline until `watch_expire`.
#[test]
fn an_update_park_holds_the_hang_detector() {
    let base = minted_now();
    let mut t = before(&CLAUDE, base);
    t.process_at(CLAUDE.half, clock(base, 2_000));
    let frame = t
        .watch(WatcherSpec::FrameWhole, at(base, 2_500))
        .expect("arm");
    let hang = at(base, 2_000) + FRAME_HANG;
    assert_eq!(t.watch_next_deadline(), Some(hang), "NEGATIVE CONTROL");
    let parked = hang + Duration::from_secs(5);
    t.watch_restart_idle(parked);
    assert_eq!(
        t.watch_next_deadline(),
        None,
        "a detector gone by while parked is no deadline"
    );
    assert!(t.watch_poll(frame).is_none(), "nor is the frame read");
    assert!(t.watch_expire(parked), "the first look that reads fires it");
    assert!(t.watch_poll(frame).is_some());
    assert_eq!(t.watch_next_deadline(), None, "and nothing is owed after");
}

/// The hang detector: a program that opens a frame, writes into it and never closes it holds
/// a whole-frame wait FRAME_HANG past its first write, then the frame is read as it stands.
#[test]
fn a_frame_never_closed_is_read_as_it_stands_after_the_hang_detector() {
    let base = minted_now();
    let mut t = before(&CLAUDE, base);
    t.process_at(CLAUDE.half, clock(base, 2_000));
    let gone = t
        .watch_rows_gone(Arc::new(Wrap(&ESC_TO_INTERRUPT)), LIVE, at(base, 2_500))
        .expect("arm");
    let frame = t
        .watch(WatcherSpec::FrameWhole, at(base, 2_500))
        .expect("arm");
    let hang = at(base, 2_000) + FRAME_HANG;
    assert_eq!(t.frame_hang_deadline(), Some(hang), "from the first write");
    assert_eq!(t.watch_next_deadline(), Some(hang));
    let early = hang - Duration::from_micros(1);
    assert!(!t.watch_expire(early), "nothing fires before the hang");
    assert!(t.frame_unfinished_at(early));
    assert!(t.watch_poll(gone).is_none() && t.watch_poll(frame).is_none());
    assert!(t.watch_expire(hang), "the hang detector fires");
    assert!(!t.frame_unfinished_at(hang));
    let sat = t
        .watch_poll(frame)
        .expect("await frame answers at the hang");
    assert_eq!(sat.at, hang);
    // The frame as it stands shows no busy footer, and no close vouches for it: gone settles.
    assert!(t.watch_poll(gone).is_none(), "no closed frame: it settles");
    let settled = hang + GONE_SETTLE;
    assert_eq!(t.watch_next_deadline(), Some(settled));
    assert!(t.watch_expire(settled));
    assert_eq!(
        t.watch_poll(gone).map(|s| s.at),
        Some(settled),
        "the frame as it stands shows no busy footer"
    );
    // A later close is an ordinary frame again.
    t.process_at(CLAUDE.rest, clock(base, 61_000_000));
    assert!(!t.sync_frame_unfinished() && t.frame_hang_deadline().is_none());
}

/// A program that brackets nothing gives the kernel no frame boundary, so `gone` settles:
/// it latches GONE_SETTLE after the batch the row left in, at that computed instant, and a
/// batch that shows the row again before then cancels it.
#[test]
fn an_unbracketed_program_settles_gone() {
    let base = minted_now();
    let mut t = Terminal::new(50, 144);
    t.process_at(b"\x1b[49;1Hworking (esc to interrupt)", clock(base, 0));
    let gone = t
        .watch_rows_gone(Arc::new(Wrap(&ESC_TO_INTERRUPT)), LIVE, at(base, 0))
        .expect("arm");
    assert!(t.watch_poll(gone).is_none());
    t.process_at(b"\x1b[49;1H\x1b[2Kdone", clock(base, 1_000));
    assert!(t.watch_poll(gone).is_none(), "no frame: the range settles");
    assert!(!t.screen_is_closed_frame());
    let due = at(base, 1_000) + GONE_SETTLE;
    assert_eq!(t.watch_next_deadline(), Some(due));
    assert!(!t.watch_expire(due - Duration::from_micros(1)));
    assert!(t.watch_expire(due));
    let sat = t.watch_poll(gone).expect("settled");
    assert_eq!((sat.at, sat.seq), (due, t.content_seq()));
    assert_eq!(t.frame_hang_deadline(), None);

    // The row back before the settle ran out cancels it; a batch past the deadline that the
    // host's expiry did not see first still latches at the deadline, before what it drew.
    let gone = t
        .watch_rows_gone(Arc::new(Wrap(&ESC_TO_INTERRUPT)), LIVE, due)
        .expect("arm again");
    assert!(
        t.watch_poll(gone).is_some(),
        "clear at arm, the screen unchanged for GONE_SETTLE: at once"
    );
    t.process_at(
        b"\x1b[49;1Hworking (esc to interrupt)",
        clock(base, 200_000),
    );
    let gone = t
        .watch_rows_gone(Arc::new(Wrap(&ESC_TO_INTERRUPT)), LIVE, at(base, 200_000))
        .expect("arm on busy");
    t.process_at(b"\x1b[49;1H\x1b[2K", clock(base, 201_000));
    t.process_at(
        b"\x1b[49;1Hworking (esc to interrupt)",
        clock(base, 202_000),
    );
    assert!(
        !t.watch_expire(at(base, 201_000) + GONE_SETTLE),
        "back before the settle ran out"
    );
    t.process_at(b"\x1b[49;1H\x1b[2K", clock(base, 300_000));
    let seq_clear = t.content_seq();
    let settle_us = u64::try_from(GONE_SETTLE.as_micros()).expect("a settle in range");
    t.process_at(
        b"\x1b[49;1Hworking (esc to interrupt)",
        clock(base, 300_000 + settle_us + 100_000),
    );
    let sat = t
        .watch_poll(gone)
        .expect("latched at the deadline it outlived");
    assert_eq!(
        (sat.at, sat.seq),
        (at(base, 300_000) + GONE_SETTLE, seq_clear)
    );
}

/// A bracket opened with nothing written in it holds no frame, so `await frame` reads the
/// screen at once (a program idle between its `?2026h` and its first write); the screen it
/// leaves is no frame the program closed, so `gone` settles from its last change.
#[test]
fn an_open_bracket_with_nothing_written_is_whole() {
    let base = minted_now();
    let mut t = Terminal::new(50, 144);
    t.process_at(b"\x1b[49;1Hdone\x1b[?2026h", clock(base, 0));
    assert!(!t.sync_frame_unfinished());
    let frame = t.watch(WatcherSpec::FrameWhole, at(base, 0)).expect("arm");
    assert!(
        t.watch_poll(frame).is_some(),
        "await frame on a whole frame"
    );
    let gone = t
        .watch_rows_gone(Arc::new(Wrap(&ESC_TO_INTERRUPT)), LIVE, at(base, 50_000))
        .expect("arm");
    assert!(t.watch_poll(gone).is_none(), "drawn 50 ms before the arm");
    assert_eq!(t.watch_next_deadline(), Some(at(base, 0) + GONE_SETTLE));
    // A stray close with nothing of the program's written finishes no frame.
    t.process_at(b"\x1b[?2026l", clock(base, 60_000));
    assert!(
        !t.screen_is_closed_frame(),
        "an empty bracket closes nothing"
    );
    assert!(t.watch_poll(gone).is_none());
    assert!(t.watch_expire(at(base, 0) + GONE_SETTLE));
    assert!(t.watch_poll(gone).is_some());
}

/// THE HANG DETECTOR IS PER RUN, NOT PER FRAME. A program that closes one frame and opens the
/// next in the same read, so that every read ends inside a frame (aterm's reader behind a
/// program that streams bracketed frames), still reaches FRAME_HANG from the first read that
/// ended inside one: `await frame` answers there and `gone` reads the screen as it stands.
/// The same for one frame written into across many reads. A read that ends whole ends the run.
/// MUTANT (`note_batch_end` restarting the run at every batch, or the run restarting at each
/// frame's first write): the deadline moves with every read and neither wait ever answers.
#[test]
fn a_run_of_reads_ending_inside_frames_reaches_the_hang_detector() {
    for back_to_back in [true, false] {
        let base = minted_now();
        let mut t = before(&CLAUDE, base);
        let gone = t
            .watch_rows_gone(Arc::new(Wrap(&ESC_TO_INTERRUPT)), LIVE, at(base, 1_500))
            .expect("arm gone");
        t.process_at(CLAUDE.half, clock(base, 2_000));
        let frame = t
            .watch(WatcherSpec::FrameWhole, at(base, 2_000))
            .expect("arm frame");
        let hang = at(base, 2_000) + FRAME_HANG;
        let next: &[u8] = if back_to_back {
            b"\x1b[?2026l\x1b[?2026h\x1b[48;1Hx"
        } else {
            b"\x1b[48;1Hx"
        };
        for second in 1..=59u64 {
            t.process_at(next, clock(base, 2_000 + second * 1_000_000));
            assert!(t.sync_frame_unfinished(), "every read ends inside a frame");
            assert_eq!(
                t.frame_hang_deadline(),
                Some(hang),
                "second {second}, back to back {back_to_back}"
            );
            assert!(t.watch_poll(frame).is_none() && t.watch_poll(gone).is_none());
        }
        assert_eq!(t.watch_next_deadline(), Some(hang));
        assert!(t.watch_expire(hang), "back to back {back_to_back}");
        assert_eq!(t.watch_poll(frame).map(|s| s.at), Some(hang));
        assert!(t.watch_expire(hang + GONE_SETTLE));
        assert!(
            t.watch_poll(gone).is_some(),
            "the screen as it stands, settled"
        );
        // A read that ends whole ends the run; the next frame starts a new one.
        t.process_at(b"\x1b[?2026l", clock(base, 70_000_000));
        assert_eq!(t.frame_hang_deadline(), None);
        t.process_at(b"\x1b[?2026h\x1b[48;1Hy", clock(base, 71_000_000));
        assert_eq!(
            t.frame_hang_deadline(),
            Some(at(base, 71_000_000) + FRAME_HANG)
        );
    }
}

/// Gemini CLI 0.46.0 brackets nothing: each frame clears the screen and redraws it whole, and a
/// burst that ended 1024 bytes in left the top of the screen and no spinner (event 187 of the
/// gm3-a tab 1 cast), the rest 0.42 ms later (event 188). The kernel has no frame to hold, so
/// `gone` settles: the spinner back in the next burst cancels it, and the reply's real end
/// (events 232 and 233, 503 ms later) latches GONE_SETTLE after it.
#[test]
fn gemini_cli_gone_settles_through_a_split_redraw() {
    let base = minted_now();
    let mut t = Terminal::new(50, 144);
    t.process_at(GEMINI.before, clock(base, 0));
    assert!(shows(&t, &GEMINI_SPINNER), "busy before:\n{}", dump(&t));
    let gone = t
        .watch_rows_gone(Arc::new(Wrap(&GEMINI_SPINNER)), LIVE, at(base, 0))
        .expect("arm");
    assert!(t.watch_poll(gone).is_none(), "busy at arm");

    t.process_at(GEMINI.half, clock(base, 8_662));
    assert!(
        !t.sync_frame_unfinished(),
        "no bracket: nothing for the gate"
    );
    assert!(!t.screen_is_closed_frame());
    // The defect's evidence: the half redraw shows no spinner.
    assert!(
        !shows(&t, &GEMINI_SPINNER),
        "the half redraw reads gone:\n{}",
        dump(&t)
    );
    assert!(
        t.watch_poll(gone).is_none(),
        "gone latched on a half redraw:\n{}",
        dump(&t)
    );
    assert_eq!(t.watch_next_deadline(), Some(at(base, 8_662) + GONE_SETTLE));

    t.process_at(GEMINI.rest, clock(base, 9_078));
    assert!(shows(&t, &GEMINI_SPINNER), "whole again:\n{}", dump(&t));
    assert_eq!(t.watch_next_deadline(), None, "the settle is cancelled");
    assert!(!t.watch_expire(at(base, 8_662) + GONE_SETTLE));
    assert!(t.watch_poll(gone).is_none());

    t.process_at(GEMINI.end, clock(base, 512_317));
    assert!(
        !shows(&t, &GEMINI_SPINNER),
        "the reply's end:\n{}",
        dump(&t)
    );
    assert!(rows(&t).iter().any(|r| r.contains("Nothing left:")));
    assert!(t.watch_poll(gone).is_none(), "settling");
    let due = at(base, 512_317) + GONE_SETTLE;
    assert!(t.watch_expire(due));
    assert_eq!(t.watch_poll(gone).map(|s| s.at), Some(due));
}

/// MUTANT: the kernel minus the settle, believing every whole screen as a closed frame,
/// latches `gone` on Gemini CLI's half redraw; the settle does not, and the rest's spinner
/// cancels it.
#[test]
fn mutant_gemini_cli_half_redraw_reads_gone_without_the_settle() {
    let base = minted_now();
    let mut t = Terminal::new(50, 144);
    t.process_at(GEMINI.before, clock(base, 0));
    let busy_rows: Vec<Option<String>> = rows(&t).into_iter().map(Some).collect();
    t.process_at(GEMINI.half, clock(base, 8_662));
    let half: Vec<Option<String>> = rows(&t).into_iter().map(Some).collect();
    let spec = || WatcherSpec::RowsClear {
        matcher: Arc::new(Wrap(&GEMINI_SPINNER)),
        rows: LIVE,
    };
    let mut mutant = WatcherSet::default();
    let id = mutant.arm(spec(), at(base, 0)).expect("arm");
    mutant.observe_screen(1, false, 0, false, at(base, 0), &busy_rows, Frame::CLOSED);
    mutant.observe_screen(2, false, 0, false, at(base, 8_662), &half, Frame::CLOSED);
    assert!(
        mutant.poll(id).is_some(),
        "MUTANT: a kernel that believes the half redraw latches gone"
    );

    let unbracketed = |off| Frame::unbracketed(Some(at(base, off)));
    let mut settled = WatcherSet::default();
    let id = settled.arm(spec(), at(base, 0)).expect("arm");
    settled.observe_screen(1, false, 0, false, at(base, 0), &busy_rows, unbracketed(0));
    settled.observe_screen(
        2,
        false,
        0,
        false,
        at(base, 8_662),
        &half,
        unbracketed(8_662),
    );
    assert!(
        settled.poll(id).is_none(),
        "the settle holds the half redraw"
    );
    settled.observe_screen(
        3,
        false,
        0,
        false,
        at(base, 9_078),
        &busy_rows,
        unbracketed(9_078),
    );
    assert!(!settled.expire(at(base, 8_662) + GONE_SETTLE));
    assert!(settled.poll(id).is_none(), "the spinner back cancels it");
}

/// A match inside an unfinished frame waits for it too: `await match` reads whole frames.
#[test]
fn a_match_written_inside_an_open_frame_waits_for_its_close() {
    let base = minted_now();
    let mut t = Terminal::new(50, 144);
    let seen = t
        .watch_rows(Arc::new(Wrap(&ESC_TO_INTERRUPT)), LIVE, at(base, 0))
        .expect("arm");
    t.process_at(
        b"\x1b[?2026h\x1b[49;1Hwork (esc to interrupt)",
        clock(base, 1_000),
    );
    assert!(t.watch_poll(seen).is_none(), "inside the open frame");
    t.process_at(b"\x1b[?2026l", clock(base, 2_000));
    let sat = t.watch_poll(seen).expect("latches at the close");
    assert_eq!(sat.at, at(base, 2_000));
}
