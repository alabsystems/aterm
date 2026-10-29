// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end [`CursorGlow`] coverage tests when a composer relayout moves the bottom-anchored box.
//! Covers keys whose echo rides a relayout, stalled frame trains across relayouts, and keyless relayouts.

use crate::common::{Ground, ground_or};
use crate::trail_host::{Case, MF, Outcome, case, holds_but, holds_where};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CURSOR_WITNESS_ROWS, CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::TypedClass;
use std::time::{Duration, Instant};

const ROWS: usize = 30;
const COLS: usize = 90;
const CW: usize = 15;
const CH: usize = 28;
const FRAME_US: u64 = 16_667;
const KEY_MS: u64 = 90;
const PTY_READ: usize = 1024;
const LIT_COV: u8 = 20;
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

/// 1-based composer text row before the relayout.
const TEXT_ROW: usize = 22;
/// 0-based grid column of the first text glyph (`❯` + NBSP precede it).
const COL0: usize = 2;

const BEFORE: &str = "also in parallel, ch";
const RIDER: char = 'e';
const AFTER: &str = "ck why the ";

const TRANSCRIPT: [&str; 6] = [
    "⏺ Agent \"Lint pass: the parser crate\" failed: Agent terminated early",
    "  ⎿  Backgrounded agent (↓ to manage · ctrl+o to expand)",
    "⏺ Goal paused · usage limit reached · continues automatically when it resets",
    "✻ Cogitated for 1d 11h 24m · done 4:01 PM · 8 shells still running",
    "❯ keep going, and check the other tab when you can.",
    "✽ Whirlpooling… (37s · ↓ 2.5k tokens · thinking with xhigh effort)",
];

const AGENTS: [&str; 6] = [
    "  ⏺ main",
    "  ◯ general-purpose  Checking fix lines in the parser",
    "  ◯ general-purpose  Merge the two lanes into main",
    "  ◯ general-purpose  Measure the render path, then decide",
    "  ◯ general-purpose  Close the last open review item",
    "  ◯ general-purpose  Read the metadata the importer trusts",
];

fn geom() -> Geom {
    Geom {
        cw: CW,
        ch: CH,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * CW) as u16,
        win_h: (ROWS * CH) as u16,
        head: 0,
    }
}

/// This file's own dark ground; an [`crate::common::on_ground`] lays another.
const GROUND: Ground = Ground {
    name: "Echo",
    fg: 0x00D0_D0D0,
    bg: 0x0011_1318,
};

fn cfg() -> GlowConfig {
    ground_or(GROUND).lay(GlowConfig {
        theme_fg: GROUND.fg,
        theme_bg: GROUND.bg,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        ..Default::default()
    })
}

fn rule() -> String {
    "\u{2500}".repeat(COLS)
}

/// Planned coverage per column of `row`.
fn coverage(glow: &CursorGlow, row: u16) -> Vec<u8> {
    let mut cov = vec![0u8; COLS];
    let rib = glow.v2_ribbon().expect("rainbow kitty owns the frame");
    for sg in rib.plan_segments() {
        let r = ((sg.spine / CH as f32) - 0.5).floor();
        let c = (sg.x / CW as f32).floor();
        if r >= 0.0 && c >= 0.0 && r as usize == usize::from(row) && (c as usize) < COLS {
            let i = c as usize;
            cov[i] = cov[i].max(sg.cov);
        }
    }
    cov
}

/// Interior dark runs of a coverage lane as `(first col, len)`.
fn holes_of(cov: &[u8]) -> Vec<(usize, usize)> {
    let lit: Vec<usize> = (0..cov.len()).filter(|&i| cov[i] >= LIT_COV).collect();
    let mut holes = Vec::new();
    if lit.len() < 2 {
        return holes;
    }
    let mut start = None;
    for (i, &v) in cov
        .iter()
        .enumerate()
        .take(lit[lit.len() - 1] + 1)
        .skip(lit[0])
    {
        if v >= LIT_COV {
            if let Some(s) = start.take() {
                holes.push((s, i - s));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    holes
}

/// How the relayout frame reaches the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    /// Delivered in one read.
    Whole,
    /// Delivered in 1024-byte reads with a frame presented between each.
    Reads,
}

/// One frame's census of the text row.
#[derive(Clone, Debug)]
struct Census {
    ms: u64,
    label: String,
    row: u16,
    text: String,
    lit: String,
    holes: Vec<(usize, usize)>,
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    t0: Instant,
    now: Instant,
    next_frame: Instant,
    last_key: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
    text: String,
    /// 1-based text row.
    row: usize,
    agents: usize,
    tail: usize,
    label: String,
    census: Vec<Census>,
    stalled: bool,
    /// The redraw erases the whole screen first (ED 2), so its writes leave full damage.
    erase_all: bool,
    /// Whether the last relayout's writes left full damage.
    relaid_full: bool,
}

impl Host {
    fn new() -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, COLS);
        let mut h = Self {
            term,
            glow,
            cfg: cfg(),
            t0: now,
            now,
            next_frame: now,
            last_key: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            text: String::new(),
            row: TEXT_ROW,
            agents: AGENTS.len() - 1,
            tail: 0,
            label: "start".into(),
            census: Vec::new(),
            stalled: false,
            erase_all: false,
            relaid_full: false,
        };
        let screen = h.screen();
        h.term.process(&screen);
        h.frame();
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// Encode the bottom-anchored screen inside a DEC 2026 bracket with the caret after `self.text`.
    fn screen(&self) -> Vec<u8> {
        let mut rows: Vec<String> = TRANSCRIPT.iter().map(|s| (*s).to_string()).collect();
        rows.push(String::new());
        rows.push(rule());
        let text_i = rows.len();
        rows.push(format!("\u{276f}\u{a0}{}", self.text));
        rows.push(rule());
        rows.push("  ⏵⏵ bypass permissions on · 8 shells".to_string());
        rows.push(String::new());
        rows.extend(AGENTS[..self.agents].iter().map(|s| (*s).to_string()));
        rows.extend(std::iter::repeat_n(String::new(), self.tail));
        let top = self.row - text_i;
        assert!(top >= 1 && top + rows.len() - 1 <= ROWS, "the screen fits");
        let mut s = format!("{SYNC_BEGIN}\x1b[?25l\x1b[H");
        if self.erase_all {
            s.push_str("\x1b[2J");
        }
        for r in 1..top {
            s.push_str(&format!("\x1b[{r};1H\x1b[K"));
        }
        for (i, r) in rows.iter().enumerate() {
            s.push_str(&format!("\x1b[{};1H{r}\x1b[K", top + i));
        }
        for r in top + rows.len()..=ROWS {
            s.push_str(&format!("\x1b[{r};1H\x1b[K"));
        }
        s.push_str(&format!(
            "\x1b[{ROWS};1H\x1b[{};{}H\x1b[?25h{SYNC_END}",
            self.row,
            COL0 + self.text.chars().count() + 1
        ));
        s.into_bytes()
    }

    /// Sample rows, tick [`CursorGlow`], and record the text row's census.
    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; CURSOR_WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            self.glow.observe_ribbon_row(r, &self.row_buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
        self.next_frame = self.now + Duration::from_micros(FRAME_US);
        self.record();
    }

    fn record(&mut self) {
        let row = (self.row - 1) as u16;
        let cov = coverage(&self.glow, row);
        self.term.row_cols_into(usize::from(row), &mut self.row_buf);
        let text: String = self
            .row_buf
            .iter()
            .map(|&c| if c == '\0' { '_' } else { c })
            .collect();
        self.census.push(Census {
            ms: self.ms(),
            label: self.label.clone(),
            row,
            text: text.trim_end().to_string(),
            lit: cov
                .iter()
                .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                .collect(),
            holes: holes_of(&cov),
        });
    }

    fn advance_to(&mut self, t: Instant) {
        if self.stalled {
            self.now = self.now.max(t);
            return;
        }
        while self.next_frame <= t {
            self.now = self.next_frame;
            self.frame();
        }
        self.now = self.now.max(t);
    }

    /// Starve the frame train until [`Host::resume`].
    fn stall(&mut self) {
        self.stalled = true;
    }

    /// Resume the frame train so the next tick observes everything buffered during the stall.
    fn resume(&mut self) {
        self.stalled = false;
        self.next_frame = self.now;
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.advance_to(t);
    }

    /// Stamp a one-cell printable key `KEY_MS` after the previous key.
    fn press(&mut self, ch: char) {
        let t = self.last_key + Duration::from_millis(KEY_MS);
        self.advance_to(t);
        self.now = t;
        self.last_key = t;
        self.label = format!("key {ch:?}");
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        self.glow.note_typed_expected(t, 1, false, class, ch);
    }

    /// Echo one key in per-key shape (or a caret move for Space).
    fn key(&mut self, ch: char) {
        self.press(ch);
        let c = COL0 + self.text.chars().count() + 1;
        let bytes = if ch == ' ' {
            format!(
                "{SYNC_BEGIN}\x1b[?25l\x1b[{};{}H\x1b[?25h{SYNC_END}",
                self.row,
                c + 1
            )
        } else {
            format!(
                "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}C\x1b[{}B{ch}\x1b[{ROWS};1H\x1b[{};{}H\x1b[?25h{SYNC_END}",
                c - 1,
                self.row - 1,
                self.row,
                c + 1
            )
        };
        self.text.push(ch);
        self.term.process(bytes.as_bytes());
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    /// Echo `ch` inside a full-screen relayout that lifts the composer `rows` rows.
    fn relay_rows(&mut self, ch: char, rows: usize, delivery: Delivery) {
        self.press(ch);
        self.text.push(ch);
        self.shift_box(rows as isize);
        let bytes = self.screen();
        self.term.take_damage();
        match delivery {
            Delivery::Whole => self.term.process(&bytes),
            Delivery::Reads => {
                let mut reads = bytes.chunks(PTY_READ).peekable();
                while let Some(read) = reads.next() {
                    self.term.process(read);
                    if reads.peek().is_some() {
                        self.frame();
                        self.now += Duration::from_millis(1);
                    }
                }
            }
        }
        self.relaid_full = self.term.grid().damage().is_full();
        self.label = "after relayout".into();
    }

    fn backspace(&mut self) {
        let t = self.last_key + Duration::from_millis(KEY_MS);
        self.advance_to(t);
        self.now = t;
        self.last_key = t;
        self.label = "backspace".into();
        self.glow.note_backspace_erasing(t, Some(1));
        self.text.pop();
        let bytes = self.screen();
        self.term.process(&bytes);
    }

    /// Grow (positive `rows`) or shrink the box under the composer, lifting or lowering its text row.
    fn shift_box(&mut self, rows: isize) {
        for _ in 0..rows {
            if self.agents < AGENTS.len() {
                self.agents += 1;
            } else {
                self.tail += 1;
            }
        }
        for _ in rows..0 {
            if self.tail > 0 {
                self.tail -= 1;
            } else {
                self.agents -= 1;
            }
        }
        self.row = self
            .row
            .checked_add_signed(-rows)
            .expect("the box stays on the screen");
    }

    /// Perform a keyless relayout shifting the composer by `rows` (positive = up, negative = down).
    fn keyless_relay_rows(&mut self, rows: isize) {
        self.shift_box(rows);
        let bytes = self.screen();
        self.term.process(&bytes);
        self.label = "keyless relayout".into();
    }

    fn first_frame_labelled(&self, label: &str) -> usize {
        self.census
            .iter()
            .position(|c| c.label == label)
            .expect("the take reached the label")
    }

    fn dump(&self, i: usize) -> String {
        let c = &self.census[i];
        let w = (COL0 + self.text.chars().count() + 3).min(COLS);
        format!(
            "  f{i:<4} t={:>5} ms row {:>2} {:<16} holes={:?}\n    text |{}\n    lit  |{}",
            c.ms,
            c.row,
            c.label,
            c.holes,
            c.text.chars().take(w).collect::<String>(),
            c.lit.chars().take(w).collect::<String>()
        )
    }

    fn story(&self, from: usize) -> String {
        let first = self.census[from..]
            .iter()
            .position(|c| !c.holes.is_empty())
            .map(|k| k + from);
        let holed = self.census[from..]
            .iter()
            .filter(|c| !c.holes.is_empty())
            .count();
        let mut s = format!(
            "{} frames since the relayout, {holed} with a hole\n",
            self.census.len() - from
        );
        if let Some(i) = first {
            for k in i.saturating_sub(2)..(i + 3).min(self.census.len()) {
                s.push_str(&self.dump(k));
                s.push('\n');
            }
        }
        s.push_str("  …the last frame:\n");
        s.push_str(&self.dump(self.census.len() - 1));
        s
    }

    fn inflight_forgotten(&self) -> u64 {
        self.glow.in_flight_tally().forgotten
    }
}

/// Read whether from `label` onward the relaid text row has no holes, lights the rider's column, and forgets no press.
fn whole_from(h: &Host, label: &str, what: &str, forgotten_before: u64) -> Result<(), String> {
    let from = h.first_frame_labelled(label);
    let relaid = (h.row - 1) as u16;
    let holed = h.census[from..]
        .iter()
        .any(|c| c.row == relaid && !c.holes.is_empty());
    if holed {
        return Err(format!(
            "{what}: the key that rode the relayout left a hole in the band\n{}",
            h.story(from)
        ));
    }
    let rider_col = COL0 + BEFORE.chars().count();
    let last = h.census.last().expect("frames ran");
    assert_eq!(last.row, relaid, "the census reads the relaid text row");
    if last.lit.as_bytes()[rider_col] != b'#' {
        return Err(format!(
            "{what}: the column of the key that rode the relayout is dark\n{}",
            h.story(from)
        ));
    }
    if h.inflight_forgotten() != forgotten_before {
        return Err(format!(
            "{what}: a press the relayout carried was forgotten as an unexplained hop"
        ));
    }
    Ok(())
}

/// Assert [`whole_from`].
fn assert_whole_from(h: &Host, label: &str, what: &str, forgotten_before: u64) {
    if let Err(e) = whole_from(h, label, what, forgotten_before) {
        panic!("{e}");
    }
}

/// A take's reading as a law case: dark when the relaid line is not whole.
fn outcome(read: Result<(), String>) -> Outcome {
    let note = read.as_ref().err().cloned().unwrap_or_default();
    Outcome::mf(0, 0, u64::from(read.is_err())).with_note(note)
}

/// A key rides a relayout `rows` rows up, its frame redrawn row by row or, `erase_all`, under ED 2.
fn take(rows: usize, delivery: Delivery, erase_all: bool) -> Result<(), String> {
    let mut h = Host::new();
    h.erase_all = erase_all;
    h.type_str(BEFORE);
    let forgotten_before = h.inflight_forgotten();
    h.relay_rows(RIDER, rows, delivery);
    assert_eq!(
        h.relaid_full, erase_all,
        "premise: the relayout's writes leave full damage exactly under ED 2"
    );
    h.type_str(AFTER);
    h.idle(200);
    whole_from(
        &h,
        "after relayout",
        &format!("{rows} row(s) up, {delivery:?}, ED 2 {erase_all}"),
        forgotten_before,
    )
}

/// Verify that a key riding a 1-row relayout lights its glyph under whole and chunked delivery.
#[test]
fn a_key_whose_echo_rides_a_relayout_a_row_up_is_laid_under_its_glyph() {
    for delivery in [Delivery::Whole, Delivery::Reads] {
        if let Err(e) = take(1, delivery, false) {
            panic!("{e}");
        }
    }
}

/// The relayouts as law cases: `relaid.rides` and `relaid.stalled` lift the box two, three or
/// six rows (agents launched in one frame) under each delivery and every stall width around the
/// relayout, and `relaid.ed2` redraws it under ED 2 one to six rows up.
fn cases() -> Vec<Case> {
    let mut v = Vec::new();
    for rows in [2usize, 3, 6] {
        for delivery in [Delivery::Whole, Delivery::Reads] {
            case(
                &mut v,
                format!("relaid.rides/rows{rows}/{delivery:?}"),
                MF,
                move || outcome(take(rows, delivery, false)),
            );
        }
        for before in 0..=3 {
            for after in 0..=3 {
                case(
                    &mut v,
                    format!("relaid.stalled/rows{rows}/before{before}/after{after}"),
                    MF,
                    move || outcome(stalled_take(before, after, rows)),
                );
            }
        }
    }
    for rows in [1usize, 2, 3, 6] {
        for delivery in [Delivery::Whole, Delivery::Reads] {
            case(
                &mut v,
                format!("relaid.ed2/rows{rows}/{delivery:?}"),
                MF,
                move || outcome(take(rows, delivery, true)),
            );
        }
    }
    v
}

fn rows_of(id: &str) -> usize {
    let at = id.find("/rows").expect("a row count") + "/rows".len();
    id[at..]
        .split('/')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a row count")
}

/// Verify that a key riding a relayout two, three or six rows up lights its glyph under whole
/// and chunked delivery and under every stall width around it.
#[test]
fn a_key_whose_echo_rides_a_relayout_three_rows_up_is_laid_under_its_glyph() {
    holds_where(cases(), |id| {
        id.starts_with("relaid.rides/") || id.starts_with("relaid.stalled/")
    });
}

/// Verify that a relayout redrawn under ED 2, which leaves full damage, is followed as main
/// follows the row-by-row redraw, one to six rows up. The limit: delivered in reads with a frame
/// presented between them, the first read erases the hand's row and writes the line on its new
/// row two or more rows away, past the ±1 rows the witness samples beside a band
/// (`Engine::band_rows`), while the caret is hidden; the witness reads the run as gone and
/// retires it before the last read shows the caret there. One row up the new row is a sampled
/// neighbour and the run is followed on that frame; the row-by-row redraw writes the new row
/// before it overwrites the old one, so no frame shows the line gone.
#[test]
fn a_full_damage_relayout_is_followed_as_main() {
    holds_but(
        cases(),
        |id| id.starts_with("relaid.ed2/"),
        |id| id.ends_with("/Reads") && rows_of(id) >= 2,
    );
}

/// Stall the frame train across `before` keys, the relayout `rows` rows up, and `after` keys on the relaid row.
fn stalled_take(before: usize, after: usize, rows: usize) -> Result<(), String> {
    let mut h = Host::new();
    let n = BEFORE.chars().count();
    h.type_str(&BEFORE.chars().take(n - before).collect::<String>());
    h.idle(100);
    let forgotten_before = h.inflight_forgotten();
    h.stall();
    h.type_str(&BEFORE.chars().skip(n - before).collect::<String>());
    h.relay_rows(RIDER, rows, Delivery::Whole);
    h.type_str(&AFTER.chars().take(after).collect::<String>());
    h.label = "resumed".into();
    h.resume();
    h.type_str(&AFTER.chars().skip(after).collect::<String>());
    h.idle(200);
    whole_from(
        &h,
        "resumed",
        &format!(
            "a stall over {before} key(s), the relayout {rows} row(s) up and {after} key(s) after it"
        ),
        forgotten_before,
    )
}

/// Verify that every stall width around the relayout (`0..=3` keys before and after) lights every typed glyph.
#[test]
fn every_stall_width_around_the_relayout_lays_every_key() {
    for before in 0..=3 {
        for after in 0..=3 {
            if let Err(e) = stalled_take(before, after, 1) {
                panic!("{e}");
            }
        }
    }
}

/// Verify that a keyless relayout carries the band and subsequent keys join it without a hole.
#[test]
fn a_keyless_relayout_moves_the_band_and_the_next_keys_join_it() {
    let mut h = Host::new();
    h.type_str(BEFORE);
    h.idle(150);
    let forgotten_before = h.inflight_forgotten();
    h.shift_box(1);
    let bytes = h.screen();
    h.term.process(&bytes);
    h.label = "after relayout".into();
    h.idle(150);
    h.type_str(&format!("{RIDER}{AFTER}"));
    h.idle(200);
    assert_whole_from(&h, "after relayout", "a keyless relayout", forgotten_before);
}

/// Verify that the band laid before the relayout stays lit while the hand keeps typing.
#[test]
fn the_band_laid_before_the_relayout_stays_lit_under_the_hand() {
    for stalled in [false, true] {
        let mut h = Host::new();
        h.type_str(&BEFORE[..BEFORE.len() - 1]);
        if stalled {
            h.stall();
        }
        h.type_str(&BEFORE[BEFORE.len() - 1..]);
        h.relay_rows(RIDER, 1, Delivery::Whole);
        h.type_str(&AFTER[..2]);
        h.resume();
        h.type_str(&AFTER[2..]);
        h.type_str("aterm claude harness ");
        let last = h.census.last().expect("frames ran");
        let lit_before = last.lit.as_bytes()[COL0..COL0 + BEFORE.chars().count()]
            .iter()
            .filter(|&&b| b == b'#')
            .count();
        assert_eq!(
            lit_before,
            BEFORE.chars().count(),
            "stalled={stalled}: the band laid before the relayout went dark under a typing hand\n{}",
            h.dump(h.census.len() - 1)
        );
        assert!(
            last.holes.is_empty(),
            "stalled={stalled}: the relaid line carries a hole\n{}",
            h.dump(h.census.len() - 1)
        );
    }
}

/// Verify that a revived prefix survives Backspace and a keyless relayout without holes or drift.
#[test]
fn revived_prefix_survives_backspace_and_keyless_relayout_without_holes_or_drift() {
    for post_relay_idle_ms in [34, 350, 600] {
        let mut h = Host::new();
        h.type_str(" gaps ");
        h.idle(1_300);
        h.last_key = h.now;
        h.type_str("n");
        h.backspace();
        h.idle(34);
        h.keyless_relay_rows(1);
        h.idle(post_relay_idle_ms);
        h.last_key = h.now;
        h.type_str("and ");
        h.idle(17);
        let last = h.census.last().expect("frames ran");
        let total_chars = h.text.chars().count();
        let lit_all = last.lit.as_bytes()[COL0..COL0 + total_chars]
            .iter()
            .filter(|&&b| b == b'#')
            .count();
        assert_eq!(
            lit_all,
            total_chars,
            "idle={post_relay_idle_ms}ms: revived prefix or suffix went dark after backspace + keyless relayout\n{}",
            h.dump(h.census.len() - 1)
        );
        assert!(
            last.holes.is_empty(),
            "idle={post_relay_idle_ms}ms: band after backspace + keyless relayout carries a hole\n{}",
            h.dump(h.census.len() - 1)
        );
    }
}

/// Verify that a faded prefix carried across a keyless relayout revives when typing resumes.
#[test]
fn a_prefix_a_keyless_relayout_carried_revives_when_the_hand_resumes() {
    for lift in [0, 1, 2, 3, 6, -2] {
        let mut h = Host::new();
        h.type_str(" gaps");
        h.idle(2_200);
        h.keyless_relay_rows(lift);
        h.idle(100);
        h.last_key = h.now;
        h.type_str(" and ");
        h.idle(17);
        let last = h.census.last().expect("frames ran");
        let total = h.text.chars().count();
        let lit = last.lit.as_bytes()[COL0..COL0 + total]
            .iter()
            .filter(|&&b| b == b'#')
            .count();
        assert_eq!(
            lit,
            total,
            "lift={lift}: the carried prefix stayed dark beside the new keys\n{}",
            h.dump(h.census.len() - 1)
        );
        assert!(
            last.holes.is_empty(),
            "lift={lift}: the revived line carries a hole\n{}",
            h.dump(h.census.len() - 1)
        );
    }
}

#[test]
fn no_fresh_hint_flood_preserves_licensed_admissions_in_ring() {
    use aterm_effects::cursor_glow::AdmissionPhase;

    let mut h = Host::new();
    h.type_str("hello");
    h.idle(600);
    let licensed_before: Vec<u64> = h
        .glow
        .admission_log()
        .filter(|r| r.phase == AdmissionPhase::Licensed)
        .map(|r| r.seq)
        .collect();
    assert_eq!(licensed_before.len(), 5);

    for step in 0..40u16 {
        let bounce_row = if step % 2 == 0 { 1 } else { h.row as u16 };
        h.term.process(format!("\x1b[{bounce_row};3H").as_bytes());
        h.idle(20);
    }

    let licensed_after: Vec<u64> = h
        .glow
        .admission_log()
        .filter(|r| r.phase == AdmissionPhase::Licensed)
        .map(|r| r.seq)
        .collect();
    assert_eq!(
        licensed_after, licensed_before,
        "uncontested no-fresh-hint bounces must not evict Licensed records from the ring"
    );
}
