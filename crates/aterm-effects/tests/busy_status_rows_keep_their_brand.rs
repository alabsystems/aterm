// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A status tick never forgets a hesitated key's press.

use crate::common::{
    note_blink, observe_neighbor_rows, observe_ribbon_rows_where, rainbow_cfg, sync_scroll,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CURSOR_WITNESS_ROWS, CursorGlow, Geom, GlowConfig, ProbeTrust};
use aterm_effects::rainbow_kitty::TypedClass;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

const ROWS: usize = 30;
const COLS: usize = 100;
const CW: usize = 8;
const CH: usize = 16;

const BULLET: u16 = 3;
const TOOL: u16 = 5;
const SPINNER: u16 = 8;
const COMPOSER: u16 = 12;
const WORKFLOW: u16 = 16;
const TEXT_COL: u16 = 2;
const SPINNER_GLYPH_COL: u16 = 10;
const SPINNER_DIGIT_COL: u16 = 17;
const WORKFLOW_TIMER_COL: u16 = 13;
const WORKFLOW_TOKENS_COL: u16 = 30;
const WORKFLOW_TOKENS: &str = "12.3k";

/// What the hand types, and where it hesitates.
#[derive(Clone, Copy, Debug)]
struct Take {
    /// The presses, one per char: a glyph, `\u{8}` a Backspace, `←` / `→`
    /// the arrows.
    keys: &'static str,
    /// The press that follows 1.1 s of thought.
    pause_at: usize,
    /// The hesitated press: pressed 322 ms after the press before it echoed, the status tick
    /// printed 3 ms after it.
    hesitated: usize,
    /// The glyph the spinner's tick prints inside the hesitated press's
    /// window (`None`: its elapsed-seconds digit, as the clock has it).
    tick_glyph: Option<char>,
}

const OWNER: Take = Take {
    keys: "how did we get to this side quest",
    pause_at: 23,
    hesitated: 24,
    tick_glyph: None,
};

const BACKSPACE: Take = Take {
    keys: "how did we get to this sx\u{8}ide quest",
    pause_at: 23,
    hesitated: 26,
    tick_glyph: None,
};

const ARROWS: Take = Take {
    keys: "how did we get to this s←→ide quest",
    pause_at: 23,
    hesitated: 26,
    tick_glyph: None,
};

const DIGIT: Take = Take {
    keys: "once you COMPLETELY LAUNCH v0.100, ok",
    pause_at: 30,
    hesitated: 31,
    tick_glyph: Some('0'),
};

/// A key the hand presses.
#[derive(Clone, Copy, Debug)]
enum Press {
    Glyph(char),
    Backspace,
    Left,
    Right,
}

impl Press {
    fn of(ch: char) -> Self {
        match ch {
            '\u{8}' => Self::Backspace,
            '←' => Self::Left,
            '→' => Self::Right,
            ch => Self::Glyph(ch),
        }
    }

    fn echo(self) -> (Vec<u8>, i16) {
        match self {
            Self::Glyph(ch) => (ch.to_string().into_bytes(), 1),
            Self::Backspace => (b"\x08\x1b[K".to_vec(), -1),
            Self::Left => (b"\x1b[D".to_vec(), -1),
            Self::Right => (b"\x1b[C".to_vec(), 1),
        }
    }
}

const STREAM_ROWS: [u16; 21] = [
    0, 1, 2, 4, 6, 7, 9, 10, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29,
];

fn cfg() -> GlowConfig {
    GlowConfig {
        theme_fg: 0x00D8_DEE9,
        theme_bg: 0x002E_3440,
        intensity: 1.0,
        ..rainbow_cfg()
    }
}

enum Ev {
    /// Program output: one whole write.
    Out(Vec<u8>),
    /// A key pressed.
    Key(Press),
}

/// Where a transcript line scrolls the whole screen up one row.
#[derive(Clone, Copy, Debug)]
enum ScrollAt {
    /// `ms` before the press after the pause (`s`), inside the 1.1 s pause.
    BeforeS(u64),
    /// `ms` after the echo of the press before the hesitated one (`s`), before the hesitated press.
    AfterSEcho(u64),
}

/// What happens around the hesitated key.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// The base shape: the spinner's digit ticks inside `i`'s pre-echo window.
    Owner,
    /// The base shape with one transcript line scrolling the screen first: the
    /// lane's row memory (every brand) goes with the old coordinates, and the
    /// spinner's digit is its first advance since.
    Scroll(ScrollAt),
    /// The base shape with a transcript streaming onto a new row every `ms` from 1.5 s before the
    /// first key ([`STREAM_ROWS`]).
    Stream(u64),
    /// The tick inside `i`'s window is the workflow row's token counter, a
    /// footer below the composer whose brand a scroll wiped.
    Footer(FooterScroll),
}

/// Where the scroll lands for [`Shape::Footer`]: after the footer's last keyless advance, 20 ms
/// before one more print of its timer (alone in its frame, so the lane samples it), which gives
/// the footer its endpoint in the new coordinates.
#[derive(Clone, Copy, Debug)]
enum FooterScroll {
    /// During the pause, 310 ms before the press after it (`s`).
    InPause,
    /// 100 ms after the echo of the press before the hesitated one (`s`, a Backspace's, an
    /// arrow's).
    AfterSEcho,
}

fn status_write(row: u16, col: u16, text: &str, crow: u16, caret: u16) -> Vec<u8> {
    format!(
        "\x1b[?2026h\x1b[{};{}H{text}\x1b[{};{}H\x1b[?2026l",
        row + 1,
        col + 1,
        crow + 1,
        caret + 1
    )
    .into_bytes()
}

struct Script {
    events: Vec<(u64, Ev)>,
    i_press: u64,
    i_echo: u64,
    tick: ((u16, u16), (u16, u16)),
    shift: u16,
}

fn timeline(take: Take, lat: u64, shape: Shape) -> Script {
    let keys: Vec<Press> = take.keys.chars().map(Press::of).collect();
    let mut ev: Vec<(u64, Ev)> = Vec::new();
    let mut presses = Vec::new();
    let mut t = 2_000;
    for i in 0..keys.len() {
        if i == take.pause_at {
            t += 1_100;
        } else if i == take.hesitated {
            t = presses[i - 1] + lat + 322;
        } else if i > 0 {
            t += 150;
        }
        presses.push(t);
    }
    let i_press = presses[take.hesitated];
    let i_echo = i_press + lat;
    let s_press = presses[take.pause_at];
    let s_echo = presses[take.hesitated - 1] + lat;
    let end = presses[presses.len() - 1] + 600;
    let tick_at = i_press + 3;
    let footer = matches!(shape, Shape::Footer(_));
    let scroll_at = match shape {
        Shape::Scroll(ScrollAt::BeforeS(d)) => Some(s_press - d),
        Shape::Scroll(ScrollAt::AfterSEcho(d)) => Some(s_echo + d),
        Shape::Footer(FooterScroll::InPause) => Some(s_press - 310),
        Shape::Footer(FooterScroll::AfterSEcho) => Some(s_echo + 100),
        Shape::Owner | Shape::Stream(_) => None,
    };
    let sh = |ms: u64| -> u16 { u16::from(scroll_at.is_some_and(|at| ms >= at)) };
    ev.push((
        0,
        Ev::Out(
            format!(
                "\x1b[2J\x1b[{};1H> \x1b[{};{}H",
                COMPOSER + 1,
                COMPOSER + 1,
                TEXT_COL + 1
            )
            .into_bytes(),
        ),
    ));
    let mut moves = Vec::new();
    for (i, &key) in keys.iter().enumerate() {
        let (echo, moved) = key.echo();
        ev.push((presses[i], Ev::Key(key)));
        ev.push((presses[i] + lat, Ev::Out(echo)));
        moves.push(moved);
    }
    let caret_at = |ms: u64| -> u16 {
        let moved: i16 = presses
            .iter()
            .zip(&moves)
            .filter(|&(&p, _)| p + lat <= ms)
            .map(|(_, &m)| m)
            .sum();
        TEXT_COL.checked_add_signed(moved).expect("a column")
    };
    let write = |ms: u64, row: u16, col: u16, text: &str| -> (u64, Ev) {
        (
            ms,
            Ev::Out(status_write(
                row - sh(ms),
                col,
                text,
                COMPOSER - sh(ms),
                caret_at(ms),
            )),
        )
    };
    if let Some(at) = scroll_at {
        ev.push((
            at,
            Ev::Out(
                format!(
                    "\x1b[?2026h\x1b[{ROWS};1H\n\x1b[{};1H  transcript line\x1b[{};{}H\x1b[?2026l",
                    ROWS - 1,
                    COMPOSER,
                    caret_at(at) + 1
                )
                .into_bytes(),
            ),
        ));
    }
    let glyphs = ['✻', '✽', '✶', '✢'];
    let mut k = 0usize;
    let mut g = 17;
    while g < end {
        let glyph = glyphs[k % glyphs.len()].to_string();
        ev.push(write(g, SPINNER, SPINNER_GLYPH_COL, &glyph));
        k += 1;
        g += 120;
    }
    let digit_tick = if footer { tick_at + 500 } else { tick_at };
    let mut d = digit_tick % 1_000;
    while d < end {
        let digit = match take.tick_glyph {
            Some(glyph) if d == tick_at => glyph,
            _ => char::from(b'0' + u8::try_from((d / 1_000) % 10).expect("a digit")),
        };
        ev.push(write(d, SPINNER, SPINNER_DIGIT_COL, &digit.to_string()));
        d += 1_000;
    }
    let mut b = 90;
    while b < end {
        let glyph = if (b / 500) % 2 == 0 { "●" } else { " " };
        ev.push(write(b, BULLET, 0, glyph));
        b += 500;
    }
    let mut s = 430;
    while s < end {
        ev.push(write(s, TOOL, 15, &format!("{:02}", 10 + s / 1_000)));
        s += 1_000;
    }
    let mut w = 770;
    while w < end {
        let secs = format!("{:02}", 40 + w / 1_000);
        ev.push(write(w, WORKFLOW, WORKFLOW_TIMER_COL, &secs));
        w += 1_000;
    }
    if footer {
        let mut x = tick_at % 1_000;
        while x < end {
            ev.push(write(x, WORKFLOW, WORKFLOW_TOKENS_COL, WORKFLOW_TOKENS));
            x += 1_000;
        }
        let timer_at = scroll_at.expect("a footer shape scrolls") + 20;
        ev.push(write(
            timer_at,
            WORKFLOW,
            WORKFLOW_TIMER_COL,
            &format!("{:02}", 40 + timer_at / 1_000),
        ));
    }
    if let Shape::Stream(ms) = shape {
        let mut x = presses[0] - 1_500;
        let mut k = 0usize;
        while x < end {
            if !(x + 5 > tick_at && x < i_echo + 5) {
                let row = STREAM_ROWS[k % STREAM_ROWS.len()];
                ev.push(write(x, row, 0, &format!("line {k:04} of streamed prose")));
            }
            k += 1;
            x += ms;
        }
    }
    ev.sort_by_key(|(at, _)| *at);
    assert!(
        !ev.iter()
            .any(|(at, e)| matches!(e, Ev::Out(_)) && *at > tick_at && *at < i_echo),
        "PREMISE: nothing is written between the tick and `i`'s echo"
    );
    let shift = sh(tick_at);
    let tick = if footer {
        (
            (WORKFLOW - shift, WORKFLOW_TIMER_COL + 2),
            (WORKFLOW - shift, WORKFLOW_TOKENS_COL + 5),
        )
    } else {
        (
            (SPINNER - shift, SPINNER_GLYPH_COL + 1),
            (SPINNER - shift, SPINNER_DIGIT_COL + 1),
        )
    };
    Script {
        events: ev,
        i_press,
        i_echo,
        tick,
        shift: sh(end),
    }
}

struct Verdict {
    us: u64,
    reason: &'static str,
    origin: (u16, u16),
    target: (u16, u16),
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    other: Vec<char>,
    /// The caret row's neighbours: `[above, below]`.
    nbrs: [Vec<char>; 2],
    blink: u64,
    scroll: Option<ContentScrollState>,
    ring: Vec<Verdict>,
    ring_seen: u64,
}

impl Host {
    fn new() -> Self {
        let g = crate::common::geom(ROWS, COLS, CW, CH);
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, COLS);
        glow.note_pane_rows(0, ROWS);
        Self {
            term: Terminal::new(ROWS as u16, COLS as u16),
            glow,
            cfg: cfg(),
            g,
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            other: Vec::new(),
            nbrs: [Vec::new(), Vec::new()],
            blink: 0,
            scroll: None,
            ring: Vec::new(),
            ring_seen: 0,
        }
    }

    fn us(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_micros() as u64
    }

    fn frame(&mut self) {
        if self.term.sync_open_dirty() {
            return;
        }
        sync_scroll(&self.term, &mut self.glow, &mut self.scroll, self.now);
        let c = self.term.cursor();
        let drawn = self.term.cursor_visible();
        let cur = Some((c.row, c.col));
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        observe_ribbon_rows_where::<CURSOR_WITNESS_ROWS>(
            &self.term,
            &mut self.glow,
            &mut self.other,
            |r| r != c.row,
        );
        self.glow
            .observe_row_with_trust(c.row, c.col, &self.row_buf, self.now, ProbeTrust::Full);
        observe_neighbor_rows(&self.term, &mut self.glow, &mut self.nbrs, c.row, ROWS);
        let (anchor, glyph) = aterm_effects::host::motion_print_anchor(&self.term);
        self.glow.observe_print_anchor_glyph(anchor, glyph);
        self.glow.observe_caret_drawn(drawn);
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        let floor = self.ring_seen;
        let us = self.us();
        for a in self.glow.admission_log().filter(|a| a.seq > floor) {
            self.ring_seen = self.ring_seen.max(a.seq);
            self.ring.push(Verdict {
                us,
                reason: a.reason,
                origin: a.origin,
                target: a.target,
            });
        }
    }

    fn key(&mut self, key: Press) {
        match key {
            Press::Glyph(ch) => {
                let class = match ch {
                    ' ' => TypedClass::Space,
                    c if c.is_uppercase() => TypedClass::Capital,
                    _ => TypedClass::Glyph,
                };
                self.glow.supersede_typed_press();
                self.glow
                    .note_typed_expected(self.now, 1, ch.is_uppercase(), class, ch);
            }
            Press::Backspace => {
                self.glow.clear_typed_keeping_hand(self.now);
                self.glow.note_backspace_erasing(self.now, Some(1));
            }
            Press::Left | Press::Right => {
                self.glow.clear_typed_keeping_hand(self.now);
                self.glow.note_motion(self.now);
            }
        }
    }

    fn off_row(&self, row: u16) -> Vec<(u16, u16)> {
        let mut v: Vec<(u16, u16)> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .iter()
            .filter(|c| c.row != row && !c.leaving())
            .map(|c| (c.row, c.col))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    fn live(&self, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .iter()
            .filter(|c| c.row == row && !c.leaving())
            .map(|c| c.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

fn holes(live: &[u16]) -> Vec<u16> {
    match (live.first(), live.last()) {
        (Some(&a), Some(&b)) => (a..b).filter(|c| !live.contains(c)).collect(),
        _ => Vec::new(),
    }
}

fn replay(take: Take, frame_us: u64, phase_us: u64, lat: u64, shape: Shape) -> String {
    let script = timeline(take, lat, shape);
    let mut h = Host::new();
    let mut bad = String::new();
    let mut next = 0usize;
    let end_us = (script.events.last().expect("events").0 + 100) * 1_000;
    let census_from_us = (script.i_echo + 1) * 1_000;
    let composer = COMPOSER - script.shift;
    let mut print_rows = BTreeSet::new();
    let mut frame_at = phase_us;
    while frame_at <= end_us {
        while let Some((ms, ev)) = script.events.get(next)
            && ms * 1_000 <= frame_at
        {
            h.now = h.t0 + Duration::from_millis(*ms);
            match ev {
                Ev::Out(b) => h.term.process(b),
                Ev::Key(key) => h.key(*key),
            }
            next += 1;
        }
        h.now = h.t0 + Duration::from_micros(frame_at);
        h.frame();
        if let Some((row, _, _)) = h.term.print_anchor()
            && frame_at < script.i_press * 1_000
        {
            print_rows.insert(row);
        }
        let stray = h.off_row(h.term.cursor().row);
        if !stray.is_empty() && !bad.contains("ink off the composer") {
            let _ = write!(
                bad,
                "ink off the composer {stray:?} (t={} ms) ",
                frame_at / 1_000
            );
        }
        if frame_at > census_from_us {
            let live = h.live(composer);
            let dark = holes(&live);
            if !dark.is_empty() && !bad.contains("hole at") {
                let _ = write!(
                    bad,
                    "hole at {dark:?} (t={} ms, live {live:?}) ",
                    frame_at / 1_000
                );
            }
        }
        frame_at += frame_us;
    }
    let verdicts: Vec<&'static str> = h
        .ring
        .iter()
        .filter(|v| {
            (v.origin, v.target) == script.tick
                && v.us >= script.i_press * 1_000
                && v.us < script.i_echo * 1_000
        })
        .map(|v| v.reason)
        .collect();
    if let Some(other) = verdicts
        .iter()
        .find(|&&r| r != CursorGlow::DECLINE_PROGRAM_ROW)
    {
        let _ = write!(bad, "the tick {:?} was judged `{other}` ", script.tick);
    }
    if verdicts.is_empty() {
        let _ = write!(
            bad,
            "VACUOUS: the tick {:?} was not judged in `i`'s window ",
            script.tick
        );
    }
    if matches!(shape, Shape::Stream(_)) && print_rows.len() <= 16 {
        let _ = write!(
            bad,
            "VACUOUS: frames ended their print on only {} rows ",
            print_rows.len()
        );
    }
    let forgotten = h.glow.in_flight_tally().forgotten;
    if forgotten != 0 {
        let _ = write!(bad, "forgotten={forgotten} ");
    }
    let refused: Vec<_> = h
        .ring
        .iter()
        .filter(|v| v.origin.0 == composer && v.reason == CursorGlow::DECLINE_NO_FRESH_HINT)
        .map(|v| (v.origin, v.target))
        .collect();
    if !refused.is_empty() {
        let _ = write!(bad, "no-fresh-hint on the composer {refused:?} ");
    }
    bad
}

fn grid() -> Vec<(u64, u64, u64)> {
    [
        (8_333u64, 0u64, 12u64),
        (8_333, 3, 16),
        (8_333, 1, 20),
        (8_333, 4, 12),
        (16_667, 0, 20),
        (16_667, 2, 20),
    ]
    .into_iter()
    .map(|(f, p, l)| (f, p * f / 5, l))
    .collect()
}

fn sweep(take: Take, shape: Shape) {
    let runs = grid();
    let n = runs.len();
    let read = crate::common::par_map(&runs, |&(frame_us, phase_us, lat)| {
        replay(take, frame_us, phase_us, lat, shape)
    });
    let broken: Vec<String> = runs
        .iter()
        .zip(read)
        .filter(|(_, bad)| !bad.is_empty())
        .map(|((frame_us, phase_us, lat), bad)| {
            format!("frame={frame_us}us phase={phase_us}us lat={lat}ms: {bad}")
        })
        .collect();
    assert!(
        broken.is_empty(),
        "{:?} {shape:?}: {} of {n} replays:\n{}",
        take.keys,
        broken.len(),
        broken.join("\n")
    );
}

#[test]
fn a_status_row_digit_inside_a_hesitated_keys_echo_window_leaves_no_hole() {
    sweep(OWNER, Shape::Owner);
}

#[test]
fn a_scroll_that_wipes_the_brands_leaves_no_hole() {
    for at in [
        ScrollAt::BeforeS(200),
        ScrollAt::BeforeS(50),
        ScrollAt::AfterSEcho(100),
    ] {
        sweep(OWNER, Shape::Scroll(at));
    }
}

#[test]
fn a_transcript_streaming_onto_new_rows_leaves_no_hole() {
    for ms in [100, 500] {
        sweep(OWNER, Shape::Stream(ms));
    }
}

#[test]
fn a_footer_below_the_composer_leaves_no_hole_after_a_scroll() {
    for at in [FooterScroll::InPause, FooterScroll::AfterSEcho] {
        sweep(OWNER, Shape::Footer(at));
    }
}

#[test]
fn a_line_editing_key_before_the_hesitated_key_leaves_no_hole() {
    for take in [BACKSPACE, ARROWS] {
        for at in [FooterScroll::InPause, FooterScroll::AfterSEcho] {
            sweep(take, Shape::Footer(at));
        }
        sweep(take, Shape::Scroll(ScrollAt::AfterSEcho(100)));
    }
}

#[test]
fn a_status_tick_ending_on_the_hesitated_key_s_own_glyph_takes_nothing() {
    sweep(DIGIT, Shape::Owner);
    for at in [
        ScrollAt::BeforeS(200),
        ScrollAt::BeforeS(50),
        ScrollAt::AfterSEcho(100),
    ] {
        sweep(DIGIT, Shape::Scroll(at));
    }
}
