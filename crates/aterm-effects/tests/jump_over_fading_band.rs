// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration tests verifying that cursor jumps and word hops over a fading band leave no holes.

use crate::common::{
    LIT_COV, caret_sample, coverage, holes_of, note_blink, observe_caret_row, observe_ribbon_rows,
    rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::ribbon::{Layer, WAKE_MAX_CELLS};
use std::time::{Duration, Instant};

const COLS: usize = 140;
const CW: usize = 8;
const CH: usize = 16;
const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN";
const TAIL: &str = "!!!!";
const KEY_MS: u64 = 83;
const HOP_MS: u64 = 150;
const FRAME_MS: u64 = 16;
const INK_SPLIT_MS: u64 = 30;
const SPINNER_MS: u64 = 100;
const FADE_END_PAUSES_MS: [u64; 6] = [1481, 1483, 1485, 1487, 1489, 1491];

/// Which program echoes the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Composer {
    /// `inkish.py`: 30 rows, the line on row 3, prefix-less.
    Ink,
    /// Claude Code's shape.
    Claude,
}

impl Composer {
    fn rows(self) -> usize {
        match self {
            Self::Ink => 30,
            Self::Claude => 60,
        }
    }
    fn row(self) -> u16 {
        match self {
            Self::Ink => 2,
            Self::Claude => 57,
        }
    }
    fn col0(self) -> u16 {
        match self {
            Self::Ink => 0,
            Self::Claude => 2,
        }
    }
    fn spinner_row(self) -> u16 {
        match self {
            Self::Ink => 0,
            Self::Claude => 54,
        }
    }
}

/// Which way the word keys move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordKeys {
    /// `inkish.py`: ⌥→ to the next word's start (the end past the last word).
    Inkish,
    /// readline / Claude Code: ⌥→ to the end of the current-or-next word.
    Readline,
}

struct RowFrame {
    ms: u64,
    /// What the host was doing when this frame presented.
    label: String,
    /// `#`/`.` per column of the line (`col0`-relative).
    map: String,
    /// Dark runs strictly inside the lit span, `(first col, width)`.
    holes: Vec<(usize, usize)>,
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
    composer: Composer,
    words: WordKeys,
    text: String,
    caret: usize,
    label: String,
    spinner_n: u32,
    next_spinner: Instant,
    frames: Vec<RowFrame>,
    /// Every same-row move's replay frame and span.
    moves: Vec<(usize, usize, usize, Vec<usize>)>,
}

impl Host {
    fn new(composer: Composer, words: WordKeys) -> Self {
        let mut term = Terminal::new(composer.rows() as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, COLS);
        term.process(b"\x1b[?1049h\x1b[2J");
        term.process(format!("\x1b[{};{}H", composer.row() + 1, composer.col0() + 1).as_bytes());
        let mut h = Self {
            term,
            glow,
            cfg: rainbow_cfg(),
            g: crate::common::geom(composer.rows(), COLS, CW, CH),
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            composer,
            words,
            text: String::new(),
            caret: 0,
            label: "start".into(),
            spinner_n: 0,
            next_spinner: now + Duration::from_millis(SPINNER_MS),
            frames: Vec::new(),
            moves: Vec::new(),
        };
        if composer == Composer::Claude {
            let r = composer.row();
            let rule: String = "─".repeat(COLS);
            let s = format!(
                "\x1b[?25l\x1b[{r};1H{rule}\x1b[{};1H{rule}\x1b[{};1H  ? for shortcuts\x1b[{};{}H\x1b[?25h",
                r + 2,
                r + 3,
                r + 1,
                composer.col0() + 1
            );
            h.term.process(s.as_bytes());
        }
        h.frame();
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink_seen, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        self.record();
    }

    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        if self.composer == Composer::Claude && self.now >= self.next_spinner {
            self.next_spinner += Duration::from_millis(SPINNER_MS);
            self.spinner_n += 1;
            let s = format!(
                "\x1b[?25l\x1b[{};1H\x1b[2K{}\x1b[{};{}H\x1b[?25h",
                self.composer.spinner_row() + 1,
                self.spinner(),
                self.composer.row() + 1,
                usize::from(self.composer.col0()) + self.caret + 1
            );
            self.term.process(s.as_bytes());
        }
        self.frame();
    }

    fn spinner(&self) -> String {
        let glyph = ['✻', '✽', '✶', '✳', '✢', '·'][(self.spinner_n as usize) % 6];
        format!("{glyph} Clauding… ({}s · ↓ 2.3k tokens)", self.ms() / 1000)
    }

    fn idle_to(&mut self, t: Instant) {
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.step();
        }
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.idle_to(t);
    }

    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = t;
        self.last_event = t;
    }

    fn redraw(&mut self) {
        let r = self.composer.row() + 1;
        let c0 = usize::from(self.composer.col0());
        let prefix = if self.composer == Composer::Claude {
            "> "
        } else {
            ""
        };
        let mut head = String::from("\x1b[?25l");
        if self.composer == Composer::Claude {
            head.push_str(&format!(
                "\x1b[{};1H\x1b[2K{}",
                self.composer.spinner_row() + 1,
                self.spinner()
            ));
        }
        head.push_str(&format!("\x1b[{r};1H\x1b[2K{prefix}{}", self.text));
        self.term.process(head.as_bytes());
        self.frame();
        self.now += Duration::from_millis(INK_SPLIT_MS);
        let tail = format!("\x1b[{r};{}H\x1b[?25h", c0 + self.caret + 1);
        self.term.process(tail.as_bytes());
        self.frame();
    }

    fn key_after(&mut self, ch: char, ms: u64) {
        self.schedule(ms);
        self.label = format!("key '{ch}'");
        self.glow.note_typed_cells(self.now, 1);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.redraw();
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key_after(ch, KEY_MS);
        }
    }

    fn nav_after(&mut self, what: &str, col: usize, ms: u64) {
        self.schedule(ms);
        self.label = format!("{what} -> {col}");
        self.glow.note_motion(self.now);
        let from = self.caret;
        self.caret = col;
        self.redraw();
        assert_eq!(
            usize::from(self.term.cursor().col),
            usize::from(self.composer.col0()) + col,
            "the nav key landed the caret"
        );
        let row = self.composer.row();
        let c0 = usize::from(self.composer.col0());
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let uncovered: Vec<usize> = (from.min(col)..=from.max(col))
            .filter(|&i| {
                !rib.cells()
                    .iter()
                    .any(|c| c.row == row && usize::from(c.col) == c0 + i && c.layer == Layer::Over)
            })
            .collect();
        self.moves
            .push((self.frames.len() - 1, from, col, uncovered));
    }

    fn word_left(&mut self) {
        let b = self.text.as_bytes();
        let mut i = self.caret;
        while i > 0 && b[i - 1] == b' ' {
            i -= 1;
        }
        while i > 0 && b[i - 1] != b' ' {
            i -= 1;
        }
        self.nav_after("opt-left", i, HOP_MS);
    }

    fn word_right(&mut self) {
        let b = self.text.as_bytes();
        let n = b.len();
        let mut i = self.caret;
        let col = match self.words {
            WordKeys::Inkish => (i + 1..n)
                .find(|&j| b[j - 1] == b' ' && b[j] != b' ')
                .unwrap_or(n),
            WordKeys::Readline => {
                while i < n && b[i] == b' ' {
                    i += 1;
                }
                while i < n && b[i] != b' ' {
                    i += 1;
                }
                i
            }
        };
        self.nav_after("opt-right", col, HOP_MS);
    }

    fn record(&mut self) {
        let row = self.composer.row();
        let c0 = usize::from(self.composer.col0());
        let n = self.text.len().max(LINE.len() + TAIL.len());
        let cov = coverage(&self.glow, self.g, row);
        let lit = |i: usize| cov[c0 + i] >= LIT_COV;
        let map: String = (0..n).map(|i| if lit(i) { '#' } else { '.' }).collect();
        let holes = holes_of(&cov[c0..c0 + n]);
        self.frames.push(RowFrame {
            ms: self.ms(),
            label: self.label.clone(),
            map,
            holes,
        });
    }

    fn longest_hole_run(&self, from: usize) -> usize {
        let (mut run, mut best) = (0, 0);
        for f in self.frames.iter().skip(from) {
            run = if f.holes.is_empty() { 0 } else { run + 1 };
            best = best.max(run);
        }
        best
    }

    fn first_labelled(&self, what: &str) -> usize {
        self.frames
            .iter()
            .position(|f| f.label.starts_with(what))
            .unwrap_or(0)
    }

    fn hole_frames_from(&self, from: usize) -> Vec<usize> {
        self.frames
            .iter()
            .enumerate()
            .skip(from)
            .filter(|(_, f)| !f.holes.is_empty())
            .map(|(i, _)| i)
            .collect()
    }

    fn holes_from(&self, from: usize) -> Vec<String> {
        let text: String = self.text.clone();
        self.frames
            .iter()
            .enumerate()
            .skip(from)
            .filter(|(_, f)| !f.holes.is_empty())
            .map(|(i, f)| {
                format!(
                    "frame {i:>4} t={:>6} ms [{}] holes={:?}\n      text |{text}|\n      lit  |{}|",
                    f.ms, f.label, f.holes, f.map
                )
            })
            .collect()
    }
}

fn home_end(composer: Composer, settle_ms: u64) -> (Host, usize) {
    let mut h = Host::new(composer, WordKeys::Inkish);
    h.type_str(LINE);
    let home = h.frames.len();
    h.nav_after("home", 0, settle_ms);
    h.nav_after("end", LINE.len(), 300);
    h.key_after('!', 200);
    h.type_str(&TAIL[1..]);
    h.idle(1500);
    (h, home)
}

fn scrub_pause_hop(composer: Composer, words: WordKeys, pause_ms: u64) -> (Host, usize) {
    let mut h = Host::new(composer, words);
    h.type_str(LINE);
    h.word_left();
    h.word_left();
    let t = h.now + Duration::from_millis(pause_ms);
    h.idle_to(t);
    h.now = t;
    h.last_event = t;
    let hop = h.frames.len();
    h.word_right();
    h.word_right();
    if h.caret < h.text.len() {
        let n = h.text.len();
        h.nav_after("end", n, HOP_MS);
    }
    h.type_str(TAIL);
    h.idle(700);
    (h, hop)
}

const BAND_EXIT_FRAMES: usize = 16;

#[test]
fn home_then_end_over_a_fading_band_then_typing_leaves_no_hole_in_the_line() {
    let mut takes = Vec::new();
    for composer in [Composer::Ink, Composer::Claude] {
        for settle in [900u64, 1500, 2200, 4000] {
            takes.push((composer, settle));
        }
    }
    let bad: Vec<String> = crate::common::par_map(&takes, |&(composer, settle)| {
        let mut bad = Vec::new();
        let (h, home) = home_end(composer, settle);
        let jump = h.first_labelled("home").max(home);
        let late = h.hole_frames_from(jump + BAND_EXIT_FRAMES);
        if !late.is_empty() {
            bad.push(format!(
                "{composer:?} settle={settle}: {} frame(s) with a hole more than \
                 {BAND_EXIT_FRAMES} frames after the Home\n{}",
                late.len(),
                h.holes_from(jump + BAND_EXIT_FRAMES)
                    .iter()
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        for i in h.hole_frames_from(jump) {
            let f = &h.frames[i];
            let lit = f.map.chars().filter(|&c| c == '#').count();
            let head = f.map.find('#').unwrap_or(0);
            let run = f.map[head..].chars().take_while(|&c| c == '#').count();
            if lit != run + 1 {
                bad.push(format!(
                    "{composer:?} settle={settle}: frame {i} is not one corridor plus \
                     one departing cell\n{}",
                    h.holes_from(i).first().cloned().unwrap_or_default()
                ));
                break;
            }
        }
        bad
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

#[test]
fn the_end_back_over_dark_ground_lays_one_corridor_out_to_the_island_it_re_owns() {
    for composer in [Composer::Ink, Composer::Claude] {
        let (h, _) = home_end(composer, 4000);
        let (home, end) = (&h.moves[h.moves.len() - 2], &h.moves[h.moves.len() - 1]);
        assert_eq!(
            (home.1, home.2, end.1, end.2),
            (LINE.len(), 0, 0, LINE.len()),
            "{composer:?}: the setup: the Home and the End were traced"
        );
        let cap = usize::from(WAKE_MAX_CELLS);
        assert_eq!(
            home.3,
            (cap..=LINE.len()).collect::<Vec<_>>(),
            "{composer:?}: the Home over dark ground lays the cap at its landing and nothing past it"
        );
        assert!(
            end.3.is_empty(),
            "{composer:?}: the End's corridor is one interval from the Home's island to its \
             landing; uncovered columns {:?}",
            end.3
        );
    }
}

#[test]
fn a_word_hop_landing_in_the_last_twenty_ms_of_the_swoosh_lays_its_corridor_whole() {
    let mut takes = Vec::new();
    for composer in [Composer::Ink, Composer::Claude] {
        for words in [WordKeys::Inkish, WordKeys::Readline] {
            for pause in FADE_END_PAUSES_MS {
                takes.push((composer, words, pause));
            }
        }
    }
    let bad: Vec<String> = crate::common::par_map(&takes, |&(composer, words, pause)| {
        let mut bad = Vec::new();
        let (h, hop) = scrub_pause_hop(composer, words, pause);
        let first = h
            .moves
            .iter()
            .find(|m| m.1 < m.2)
            .expect("the first opt-right was traced");
        if !first.3.is_empty() {
            bad.push(format!(
                "{composer:?} {words:?} pause={pause}: the hop {}->{} left corridor \
                 columns {:?} with no Over cell on its replay frame {}",
                first.1, first.2, first.3, first.0
            ));
        }
        let holes = h.holes_from(hop);
        if !holes.is_empty() {
            bad.push(format!(
                "{composer:?} {words:?} pause={pause}: {} frame(s) with a hole after the hop\n{}",
                holes.len(),
                holes.iter().take(4).cloned().collect::<Vec<_>>().join("\n")
            ));
        }
        bad
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

#[test]
fn a_word_hop_over_a_retracting_band_leaves_nothing_to_the_band_s_clock() {
    let mut bad = Vec::new();
    for pause in [1000u64, 1100, 1200] {
        let (h, hop) = scrub_pause_hop(Composer::Ink, WordKeys::Inkish, pause);
        let first = h
            .moves
            .iter()
            .find(|m| m.1 < m.2)
            .expect("the first opt-right was traced");
        if !first.3.is_empty() {
            bad.push(format!(
                "pause={pause}: the hop {}->{} left corridor columns {:?} with no Over cell",
                first.1, first.2, first.3
            ));
        }
        let run = h.longest_hole_run(hop);
        if run > 3 {
            bad.push(format!(
                "pause={pause}: a hole stood for {run} frames after the hop\n{}",
                h.holes_from(hop).join("\n")
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

#[test]
fn a_word_hop_over_a_live_band_is_a_scrub_and_lays_nothing() {
    for composer in [Composer::Ink, Composer::Claude] {
        let (h, hop) = scrub_pause_hop(composer, WordKeys::Inkish, 300);
        let first = h
            .moves
            .iter()
            .find(|m| m.1 < m.2)
            .expect("the first opt-right was traced");
        assert_eq!(
            first.3.len(),
            first.2 - first.1 + 1,
            "{composer:?}: a scrub over the live band lays no Over cell over it: uncovered {:?}",
            first.3
        );
        let holes = h.holes_from(hop);
        assert!(holes.is_empty(), "{composer:?}:\n{}", holes.join("\n"));
    }
}
