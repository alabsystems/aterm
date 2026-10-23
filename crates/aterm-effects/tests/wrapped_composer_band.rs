// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The band in a wrapped composer is one run, on every frame.

use crate::common::{
    LIT_COV, caret_sample, coverage, holes_of, note_blink, observe_caret_row, observe_ribbon_rows,
    rainbow_cfg, sync_scroll,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use std::time::{Duration, Instant};

const ROWS: usize = 14;
const COLS: usize = 80;
const CW: usize = 15;
const CH: usize = 28;
const W: usize = COLS - 4;
const PROMPT: &str = "> ";
const CONT: &str = "  ";
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 8;
const WRITE_GAP_MS: u64 = 3;

const PARA: &str = "there is a bug that when backspacing in this build the rainbow cursor trail breaks apart and I also see some awkward transitions when going to a new line still with the rainbow please audit that logic";
const TYPED: usize = 110;

struct RowCensus {
    ms: u64,
    row: u16,
    map: String,
    holes: Vec<usize>,
}

impl RowCensus {
    fn worst(&self) -> usize {
        self.holes.iter().copied().max().unwrap_or(0)
    }
}

struct Composer {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    scroll: Option<ContentScrollState>,
    line: String,
    caret: usize,
    prev_rows: usize,
    census: Vec<RowCensus>,
}

impl Composer {
    fn new() -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        term.process(format!("\x1b[{ROWS};1H").as_bytes());
        let mut c = Self {
            term,
            glow,
            cfg: rainbow_cfg(),
            g: crate::common::geom(ROWS, COLS, CW, CH),
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            scroll: None,
            line: String::new(),
            caret: 0,
            prev_rows: 0,
            census: Vec::new(),
        };
        c.paint();
        c.frame();
        c
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn frame(&mut self) {
        sync_scroll(&self.term, &mut self.glow, &mut self.scroll, self.now);
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        self.record();
    }

    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        self.frame();
    }

    fn idle_to(&mut self, t: Instant) {
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.step();
        }
    }

    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = t;
        self.last_event = t;
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.idle_to(t);
    }

    fn wrap(&self) -> Vec<String> {
        if self.line.is_empty() {
            return vec![String::new()];
        }
        self.line
            .as_bytes()
            .chunks(W)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect()
    }

    fn paint(&mut self) {
        let rows = self.wrap();
        let n = rows.len();
        let w = |c: &mut Self, bytes: &[u8]| {
            c.term.process(bytes);
            let t = c.now + Duration::from_millis(WRITE_GAP_MS);
            c.idle_to(t);
            c.now = t;
        };
        if n > self.prev_rows && self.prev_rows > 0 {
            w(self, format!("\x1b[{ROWS};1H").as_bytes());
            w(self, "\n".repeat(n - self.prev_rows).as_bytes());
        }
        let top = ROWS - n + 1;
        w(self, format!("\x1b[{top};1H\x1b[0J").as_bytes());
        let body: Vec<String> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| format!("{}{}", if i == 0 { PROMPT } else { CONT }, r))
            .collect();
        w(self, body.join("\r\n").as_bytes());
        let crow = top + self.caret / W;
        let ccol = PROMPT.len() + self.caret % W + 1;
        self.term.process(format!("\x1b[{crow};{ccol}H").as_bytes());
        self.prev_rows = n;
    }

    fn key(&mut self, ch: char) {
        self.schedule(KEY_MS);
        self.glow.note_typed_cells(self.now, 1);
        self.line.insert(self.caret, ch);
        self.caret += 1;
        self.paint();
        self.frame();
    }

    fn backspace(&mut self, ms: u64) {
        self.schedule(ms);
        self.glow.note_typed_cells(self.now, 1);
        if self.caret > 0 {
            self.caret -= 1;
            self.line.remove(self.caret);
        }
        self.paint();
        self.frame();
    }

    fn word_left(&mut self, ms: u64) {
        self.schedule(ms);
        self.glow.note_motion(self.now);
        let b = self.line.as_bytes();
        let mut i = self.caret;
        while i > 0 && b[i - 1] == b' ' {
            i -= 1;
        }
        while i > 0 && b[i - 1] != b' ' {
            i -= 1;
        }
        self.caret = i;
        self.paint();
        self.frame();
    }

    fn record(&mut self) {
        let ms = self.ms();
        for r in 0..ROWS as u16 {
            let row = coverage(&self.glow, self.g, r);
            if row.iter().filter(|&&v| v >= LIT_COV).count() < 2 {
                continue;
            }
            self.census.push(RowCensus {
                ms,
                row: r,
                map: row
                    .iter()
                    .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                    .collect(),
                holes: holes_of(&row).into_iter().map(|(_, len)| len).collect(),
            });
        }
    }

    fn broken(&self) -> Vec<&RowCensus> {
        self.census.iter().filter(|c| !c.holes.is_empty()).collect()
    }

    fn report(&self, what: &str) -> String {
        let broken = self.broken();
        let worst = broken.iter().map(|c| c.worst()).max().unwrap_or(0);
        let mut s = format!(
            "{what}: {} of {} row-frames carry an interior dark run, worst {worst} cells",
            broken.len(),
            self.census.len()
        );
        for c in broken.iter().take(6) {
            s.push_str(&format!("\n  t={:6} r={:2} {}", c.ms, c.row, c.map));
        }
        s
    }

    fn assert_band_was_laid(&self, least_rows: usize, least_span: usize) {
        let lit_rows: std::collections::BTreeSet<u16> = self.census.iter().map(|c| c.row).collect();
        assert!(
            lit_rows.len() >= least_rows,
            "the take lit {} rows, wanted at least {least_rows}: the paragraph never wrapped",
            lit_rows.len()
        );
        let widest = self
            .census
            .iter()
            .map(|c| c.map.chars().filter(|&ch| ch == '#').count())
            .max()
            .unwrap_or(0);
        assert!(
            widest >= least_span,
            "the widest band was {widest} cells, wanted at least {least_span}"
        );
    }
}

fn typed_paragraph() -> Composer {
    let mut c = Composer::new();
    for ch in PARA.chars().take(TYPED) {
        c.key(ch);
    }
    c
}

#[test]
fn a_backspace_run_mid_word_never_punches_a_hole_in_the_wrapped_band() {
    let mut c = typed_paragraph();
    c.idle(400);
    for _ in 0..3 {
        c.word_left(180);
    }
    c.idle(300);
    for _ in 0..8 {
        c.backspace(140);
    }
    c.idle(1500);
    c.assert_band_was_laid(2, 24);
    assert!(c.broken().is_empty(), "{}", c.report("c8_bs_mid"));
}

#[test]
fn a_backspace_run_at_the_end_never_punches_a_hole_in_the_wrapped_band() {
    let mut c = typed_paragraph();
    c.idle(300);
    for _ in 0..12 {
        c.backspace(120);
    }
    c.idle(1500);
    c.assert_band_was_laid(2, 24);
    assert!(c.broken().is_empty(), "{}", c.report("c2_bs"));
}

#[test]
fn an_insert_mid_line_never_punches_a_hole_in_the_wrapped_band() {
    let mut c = typed_paragraph();
    c.idle(400);
    for _ in 0..3 {
        c.word_left(180);
    }
    c.idle(300);
    for ch in "INSERTED ".chars() {
        c.key(ch);
    }
    c.idle(1500);
    c.assert_band_was_laid(2, 24);
    assert!(c.broken().is_empty(), "{}", c.report("c4_edit"));
}
