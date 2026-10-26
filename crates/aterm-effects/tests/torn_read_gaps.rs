// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Torn PTY read and stalled repaint tests for the rainbow kitty ribbon band.

use crate::common::{
    LIT_COV, Rec, caret_sample, class_of, coverage, holes_of, is_key, note_blink,
    observe_caret_row, observe_ribbon_rows, rainbow_cfg, recording,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::ribbon::{RETIRE_MELT_S, RETRACT_DUR_S, RETRACT_FADE_S};
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COLS: usize = 140;
const ROW: u16 = (ROWS - 3) as u16;
const COL0: usize = 4;
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
const SPINNER_MS: u64 = 100;
const PTY_READ: usize = 1024;

const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN!!!!";
const AT_TO: usize = 101;

const FILLER: [&str; 8] = [
    "⏺ I'll start by reading the harness I'm told to copy, then the ribbon module doc.",
    "⏺ Bash(ls -la crates/aterm-effects/tests/ && wc -l crates/aterm-effects/tests/scrub_gaps.rs)",
    "  ⎿  total 1560 — 48 entries, 789 lines in scrub_gaps.rs, 486 in wrapped_composer_band.rs",
    "⏺ Read(crates/aterm-effects/src/rainbow_kitty/ribbon.rs)",
    "  ⎿  Read 420 lines: the module ledger of every prior gap defect and the law that closed it",
    "⏺ The event handler shows the insert path routes through Event::Rewrite and the witness.",
    "⏺ Bash(TRUST_NO_MIGRATE_WARN=1 targo --unverified test -p aterm-effects --test scrub_gaps)",
    "  ⎿  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; finished in 0.21s",
];
const SPIN: [char; 4] = ['✻', '✶', '✳', '✢'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Split {
    Whole,
    TextCol(usize),
    Chunks,
}

fn geom() -> Geom {
    crate::common::geom(ROWS, COLS, CW, CH)
}

type Holes = Vec<(usize, usize)>;

type HoleFrame = (u64, String, Holes);

#[derive(Clone, Debug)]
struct Census {
    ms: u64,
    label: String,
    text: String,
    lit: String,
    holes: Vec<(usize, usize)>,
    retiring: Vec<u16>,
    released: Vec<u16>,
    live: Vec<u16>,
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    text: String,
    caret: usize,
    label: String,
    spinner: Option<Instant>,
    spin_i: usize,
    key_split: Split,
    spin_split: Split,
    pad: usize,
    torn: Vec<usize>,
    census: Vec<Census>,
}

impl Host {
    fn new(spinner: bool) -> Self {
        Self::with_pad(spinner, 0)
    }

    fn with_pad(spinner: bool, pad: usize) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, COLS);
        term.process(b"\x1b[?1049h\x1b[2J");
        let mut h = Self {
            term,
            glow,
            cfg: rainbow_cfg(),
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            text: String::new(),
            caret: 0,
            label: "start".into(),
            spinner: spinner.then_some(now + Duration::from_millis(SPINNER_MS)),
            spin_i: 0,
            key_split: Split::Whole,
            spin_split: Split::Whole,
            pad,
            torn: Vec::new(),
            census: Vec::new(),
        };
        h.repaint(Split::Whole);
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
        self.record(ROW);
    }

    fn record(&mut self, row: u16) {
        let cov = coverage(&self.glow, geom(), row);
        self.term.row_cols_into(usize::from(row), &mut self.row_buf);
        let text: String = self
            .row_buf
            .iter()
            .map(|&c| if c == '\0' { '_' } else { c })
            .collect();
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let on_row = || rib.cells().iter().filter(|c| c.row == row);
        let mut retiring: Vec<u16> = on_row()
            .filter(|c| c.retire_at.is_some())
            .map(|c| c.col)
            .collect();
        let mut released: Vec<u16> = on_row()
            .filter(|c| c.released_at.is_some())
            .map(|c| c.col)
            .collect();
        let mut live: Vec<u16> = on_row().filter(|c| !c.leaving()).map(|c| c.col).collect();
        for v in [&mut retiring, &mut released, &mut live] {
            v.sort_unstable();
            v.dedup();
        }
        self.census.push(Census {
            ms: self.ms(),
            label: self.label.clone(),
            text: text.trim_end().to_string(),
            lit: cov
                .iter()
                .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                .collect(),
            holes: holes_of(&cov),
            retiring,
            released,
            live,
        });
    }

    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        self.frame();
    }

    fn idle_to(&mut self, t: Instant) {
        loop {
            let next = self.now + Duration::from_millis(FRAME_MS);
            if let Some(due) = self.spinner
                && due <= t
                && due <= next
            {
                self.now = self.now.max(due);
                self.spinner = Some(due + Duration::from_millis(SPINNER_MS));
                self.spin_i += 1;
                let keep = std::mem::replace(&mut self.label, "spinner".into());
                self.repaint(self.spin_split);
                self.label = keep;
                continue;
            }
            if next > t {
                break;
            }
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
        self.now = self.now.max(t);
        self.last_event = self.now;
    }

    fn frame_bytes(&self) -> (Vec<u8>, usize) {
        let mut rows: Vec<String> = FILLER.iter().map(|s| (*s).to_string()).collect();
        rows.push(format!(
            "{} Clauding… ({}s · ↓ 2.3k tokens)",
            SPIN[self.spin_i % SPIN.len()],
            40 + self.spin_i / 10
        ));
        rows.push(String::new());
        rows.push(format!("╭{}╮", "─".repeat(COLS - 2)));
        let text_row = rows.len();
        let pad = COLS.saturating_sub(6 + self.text.len());
        rows.push(format!("│ > {}{} │", self.text, " ".repeat(pad)));
        rows.push(format!("╰{}╯", "─".repeat(COLS - 2)));
        rows.push("  ? for shortcuts".to_string());
        let top = ROWS - rows.len() + 1;
        let mut s = String::from("\x1b[?2026h\x1b[?25l");
        s.push_str(&sgr_pad(self.pad));
        s.push_str(&format!("\x1b[{top};1H"));
        let mut text_off = 0;
        for (i, r) in rows.iter().enumerate() {
            s.push_str("\x1b[2K");
            if i == text_row {
                text_off = s.len() + "│ > ".len();
            }
            s.push_str(r);
            if i + 1 < rows.len() {
                s.push_str("\r\n");
            }
        }
        s.push_str(&format!(
            "\x1b[{};{}H\x1b[?25h\x1b[?2026l",
            usize::from(ROW) + 1,
            COL0 + self.caret + 1
        ));
        (s.into_bytes(), text_off)
    }

    fn repaint(&mut self, split: Split) {
        let (bytes, text_off) = self.frame_bytes();
        let mut cuts: Vec<usize> = Vec::new();
        match split {
            Split::Whole => {}
            Split::TextCol(j) => cuts.push(text_off + j.min(self.text.len())),
            Split::Chunks => {
                cuts.extend((1..).map(|k| k * PTY_READ).take_while(|&p| p < bytes.len()))
            }
        }
        let mut start = 0;
        for &c in &cuts {
            self.term.process(&bytes[start..c]);
            if c > text_off && c < text_off + self.text.len() {
                self.torn.push(self.census.len());
            }
            self.frame();
            self.now += Duration::from_millis(1);
            start = c;
        }
        self.term.process(&bytes[start..]);
        self.frame();
    }

    fn key_after(&mut self, ch: char, ms: u64) {
        self.schedule(ms);
        self.label = format!("key {ch:?}");
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.repaint(self.key_split);
        assert!(self.term.cursor_visible(), "the repaint shows the caret");
    }

    fn hop_after(&mut self, left: bool, ms: u64, split: Split) {
        self.schedule(ms);
        self.label = if left { "opt-left" } else { "opt-right" }.into();
        self.glow.note_motion(self.now);
        let b = self.text.as_bytes();
        let mut i = self.caret;
        if left {
            while i > 0 && b[i - 1] == b' ' {
                i -= 1;
            }
            while i > 0 && b[i - 1] != b' ' {
                i -= 1;
            }
        } else {
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            while i < b.len() && b[i] != b' ' {
                i += 1;
            }
        }
        self.caret = i;
        self.repaint(split);
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key_after(ch, KEY_MS);
        }
    }

    fn program(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
        self.frame();
    }

    fn hole_frames(&self) -> Vec<HoleFrame> {
        self.census
            .iter()
            .filter(|c| !c.holes.is_empty())
            .map(|c| (c.ms, c.label.clone(), c.holes.clone()))
            .collect()
    }

    fn dump(&self, i: usize) -> String {
        let c = &self.census[i];
        let w = (COL0 + self.text.len() + 3).min(COLS);
        format!(
            "  f{i:<4} t={:>6} ms {:<12} holes={:?} retiring={:?} released={:?}\n    text |{}\n    lit  |{}",
            c.ms,
            c.label,
            c.holes,
            c.retiring,
            c.released,
            c.text.chars().take(w).collect::<String>(),
            c.lit.chars().take(w).collect::<String>()
        )
    }

    fn story(&self) -> String {
        let first = self.census.iter().position(|c| !c.holes.is_empty());
        let mut s = format!(
            "{} frames, {} with a hole; torn presents at {:?}\n",
            self.census.len(),
            self.hole_frames().len(),
            self.torn
        );
        if let Some(i) = first {
            for k in i.saturating_sub(3)..(i + 4).min(self.census.len()) {
                s.push_str(&self.dump(k));
                s.push('\n');
            }
        }
        s
    }
}

fn assert_no_hole(h: &Host, what: &str) {
    let holes = h.hole_frames();
    assert!(
        holes.is_empty(),
        "{what}: the band carried an interior hole on {} frames, first {:?}\n{}",
        holes.len(),
        holes.first(),
        h.story()
    );
}

fn assert_whole(h: &Host, what: &str) {
    assert_no_hole(h, what);
    let melted = h.census.iter().position(|c| !c.retiring.is_empty());
    assert!(
        melted.is_none(),
        "{what}: text that came straight back was retired on the melt\n{}",
        {
            let i = melted.unwrap_or(0);
            (i.saturating_sub(3)..(i + 2).min(h.census.len()))
                .map(|k| h.dump(k))
                .collect::<Vec<_>>()
                .join("\n")
        }
    );
}

#[test]
fn the_owners_line_torn_under_to_of_into_keeps_its_band_whole() {
    let mut h = Host::new(false);
    h.type_str(&LINE[..AT_TO + 3]);
    h.key_split = Split::TextCol(AT_TO);
    h.type_str(&LINE[AT_TO + 3..]);
    h.idle(400);
    assert!(!h.torn.is_empty(), "the torn read was presented");
    let torn = h.torn[0];
    assert!(
        h.census[torn]
            .released
            .contains(&(COL0 as u16 + AT_TO as u16)),
        "`T` was released on the torn present, not melted\n{}",
        h.story()
    );
    assert!(
        h.census[torn + 1].released.is_empty(),
        "…and lifted by the identical text on the next present\n{}",
        h.story()
    );
    assert_whole(&h, "the owner's line torn under TO");
}

#[test]
fn a_released_tail_is_restored_although_the_key_is_laid_before_the_restore() {
    let line = "the quick brown fox jumps over the lazy dog while seven wizards quietly hex jumbo pyramids";
    let mut h = Host::new(false);
    h.type_str(&line[..60]);
    h.key_split = Split::TextCol(50);
    h.type_str(&line[60..]);
    h.idle(300);
    assert!(
        h.torn.len() >= 20,
        "every key from col 60 on was torn: {:?}",
        h.torn
    );
    for &t in &h.torn {
        assert!(
            !h.census[t].released.is_empty(),
            "the torn present released the blanked tail at f{t}\n{}",
            h.story()
        );
        assert!(
            h.census[t + 1].released.is_empty(),
            "and the complete present lifted all of it though the key's cell joined first (f{})\n{}",
            t + 1,
            h.story()
        );
    }
    assert_whole(&h, "a unique tail torn under every key");
}

fn sgr_pad(n: usize) -> String {
    assert!(n == 0 || n >= 3, "an SGR pad is 0 or at least 3 bytes");
    if n == 0 {
        return String::new();
    }
    let mut s = "\x1b[m".repeat(n / 3 - 1);
    s.push_str(["\x1b[m", "\x1b[0m", "\x1b[00m"][n % 3]);
    debug_assert_eq!(s.len(), n);
    s
}

fn pad_for_boundary_at(j: usize) -> usize {
    let (_, text_off) = Host::new(false).frame_bytes();
    let n = (PTY_READ - (text_off + j) % PTY_READ) % PTY_READ;
    if n < 3 { n + PTY_READ } else { n }
}

#[test]
fn claude_code_frames_in_1024_byte_reads_keep_the_band_whole_while_the_hand_types() {
    for j in [30usize, 60, 90, AT_TO] {
        let mut h = Host::with_pad(true, pad_for_boundary_at(j));
        h.key_split = Split::Chunks;
        h.spin_split = Split::Chunks;
        h.type_str(LINE);
        h.idle(500);
        assert!(
            h.torn.len() > 5,
            "the boundary at {j} tore the row: {:?}",
            h.torn
        );
        assert_whole(&h, &format!("1024-byte reads, a boundary at text col {j}"));
    }
}

#[test]
fn an_insert_through_word_hops_under_torn_reads_keeps_the_band_whole() {
    let head = &LINE[..99];
    let tail = &LINE[104..];
    let mut takes = Vec::new();
    for j in [30usize, 60, 90, 98, 99, 100, 101, 102, 103] {
        takes.push((format!("text col {j}"), Split::TextCol(j), 0usize, false));
    }
    for j in [99usize, 100, 101] {
        for spinner in [false, true] {
            takes.push((
                format!("1024-byte reads, boundary at {j}, spinner {spinner}"),
                Split::Chunks,
                pad_for_boundary_at(j),
                spinner,
            ));
        }
    }
    crate::common::par_map(&takes, |(what, split, pad, spinner)| {
        let (split, pad, spinner) = (*split, *pad, *spinner);
        let mut h = Host::with_pad(spinner, pad);
        h.type_str(head);
        h.type_str(tail);
        h.hop_after(true, 400, Split::Whole);
        assert_eq!(h.caret, head.len(), "the hop landed at the start of MAIN");
        h.key_split = split;
        for (k, ch) in "INTO ".chars().enumerate() {
            h.key_after(ch, if k == 0 { 400 } else { KEY_MS });
        }
        assert_eq!(h.text, LINE);
        h.hop_after(false, 400, split);
        assert_eq!(h.caret, LINE.len(), "the hop landed at the end");
        h.idle(600);
        assert_no_hole(&h, &format!("insert through hops, {what}"));
    });
}

#[test]
fn a_spinner_repaint_torn_under_to_with_no_key_in_flight_keeps_the_band_whole() {
    let mut h = Host::new(true);
    h.spin_split = Split::TextCol(AT_TO);
    h.type_str(LINE);
    h.idle(600);
    assert!(
        h.torn.len() >= 5,
        "the spinner's repaints were torn: {:?}",
        h.torn
    );
    assert_whole(&h, "the spinner torn under TO");
}

#[test]
fn a_really_cleared_tail_leaves_and_takes_only_its_own_spaces() {
    let mut h = Host::new(false);
    let typed = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES";
    h.type_str(typed);
    h.idle(200);
    let cut = typed.find("STOP").expect("the cut");
    let at = COL0 + cut;
    h.label = "clear".into();
    h.program(format!("\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, at + 1).as_bytes());
    let cleared = h.census.len() - 1;
    let released = &h.census[cleared].released;
    assert!(
        released.iter().all(|&c| usize::from(c) >= at),
        "only the cleared tail is released, never a space of the standing text: {released:?}\n{}",
        h.story()
    );
    assert!(
        released.contains(&(at as u16)),
        "the cleared tail was released: {released:?}"
    );
    h.program(format!("\x1b[{};{}H\x1b[?25h", ROW + 1, at + 1).as_bytes());
    let span_ms = ((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64;
    h.idle(span_ms + 3 * FRAME_MS);
    let last = h.census.last().expect("a frame");
    let lit_past: Vec<usize> = last
        .lit
        .char_indices()
        .filter(|&(i, c)| c == '#' && i > at)
        .map(|(i, _)| i)
        .collect();
    assert!(
        lit_past.is_empty(),
        "the cleared tail is gone within the retract, not held: lit {lit_past:?}\n{}",
        h.story()
    );
    let prefix: Vec<u16> = (COL0..at - 1).map(|c| c as u16).collect();
    for c in &h.census[cleared..] {
        let dark: Vec<_> = prefix.iter().filter(|p| !c.live.contains(p)).collect();
        assert!(
            dark.is_empty(),
            "the standing text keeps every cell, its spaces included, at t={}: {dark:?}",
            c.ms
        );
    }
    let holes = h.hole_frames();
    assert!(
        holes.is_empty(),
        "no comb in the standing text: {:?}\n{}",
        holes.first(),
        h.story()
    );
}

#[test]
fn a_stalled_repaint_restores_the_suffix_when_typing_resumes() {
    for stall_ms in [40, 300, 800] {
        let mut h = Host::new(false);
        h.type_str("hello brave new world");
        h.idle(200);
        let cut = COL0 + "hello ".len();
        h.label = "stalled repaint".into();
        h.program(format!("\x1b[?2026h\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, cut + 1).as_bytes());
        h.idle(stall_ms);
        h.label = "same text returned".into();
        h.repaint(Split::Whole);
        let returned = h.census.last().expect("returned frame");
        let missing: Vec<_> = (COL0..COL0 + h.text.len())
            .filter(|&col| !returned.live.contains(&(col as u16)))
            .collect();
        assert!(
            missing.is_empty(),
            "{stall_ms}ms repaint: restored text lost suffix cells {missing:?} before the next key\n{}",
            h.story()
        );
        let y = i32::from(ROW) * CH as i32 + CH as i32 / 2;
        for col in [cut + 3, cut + 6, COL0 + h.text.len() - 2] {
            let x = (col * CW + CW / 2) as i32;
            assert!(
                h.glow.under_quads().iter().any(|q| q.alpha >= 50
                    && i32::from(q.x) <= x
                    && x < i32::from(q.x) + i32::from(q.w)
                    && i32::from(q.y) <= y
                    && y < i32::from(q.y) + i32::from(q.h)),
                "{stall_ms}ms repaint: restored ownership was not painted on its first frame at col {col}"
            );
        }
        h.key_after('!', KEY_MS);
        h.idle(200);
        let restored = h.census.last().expect("restored frame");
        let missing: Vec<_> = (COL0..COL0 + h.text.len())
            .filter(|&col| !restored.live.contains(&(col as u16)))
            .collect();
        assert!(
            missing.is_empty(),
            "{stall_ms}ms repaint: same typing run lost suffix cells {missing:?}\n{}",
            h.story()
        );
    }
}

fn pending_repaint(
    stall_ms: u64,
    keys: usize,
    tick_pending: bool,
    exact: bool,
) -> (Host, Vec<aterm_effects::rainbow_kitty::ribbon::Cell>) {
    let mut h = Host::new(false);
    h.type_str("hello brave new world");
    h.idle(200);
    let original = h
        .glow
        .v2_ribbon()
        .expect("ribbon")
        .cells()
        .iter()
        .filter(|cell| cell.row == ROW)
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(original.len(), h.text.len(), "every seed glyph owns a cell");
    let cut = COL0 + "hello ".len();
    h.program(format!("\x1b[?2026h\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, cut + 1).as_bytes());
    h.idle(stall_ms);
    let ribbon = h.glow.v2_ribbon().expect("ribbon");
    for old in &original {
        let resident = ribbon
            .cells()
            .iter()
            .find(|cell| (cell.row, cell.col, cell.born) == (old.row, old.col, old.born))
            .expect("the original identity remains resident during the blank");
        let cohort = ribbon
            .cohorts()
            .iter()
            .find(|cohort| cohort.id == resident.cohort)
            .unwrap();
        assert!(
            !cohort.leaving_at(h.now),
            "{stall_ms}ms fixture must precede natural retraction"
        );
        if usize::from(old.col) >= cut {
            assert!(resident.released_at.is_some(), "the suffix is in custody");
            if stall_ms >= 300 {
                assert!(
                    h.now
                        .saturating_duration_since(resident.released_at.unwrap())
                        .as_secs_f32()
                        > RETRACT_FADE_S,
                    "the blank outlasts visual retraction"
                );
                assert!(
                    ribbon.field_at(ROW, old.col).is_none(),
                    "custody is dark before new keys"
                );
            }
        }
    }
    if stall_ms >= 300 {
        assert!(
            coverage(&h.glow, geom(), ROW)[cut + 1..COL0 + h.text.len()]
                .iter()
                .all(|&cov| cov == 0)
        );
    }
    let waiting = h.glow.admission_tally();
    for ch in "xyz".chars().take(keys) {
        h.now += Duration::from_millis(1);
        h.last_event = h.now;
        let (shifted, class) = class_of(ch);
        if exact {
            h.glow.note_typed_expected(h.now, 1, shifted, class, ch);
        } else {
            h.glow.note_typed_glyph(h.now, 1, shifted, class);
        }
        h.text.insert(h.caret, ch);
        h.caret += 1;
        if tick_pending {
            h.frame();
            assert_eq!(
                h.glow.admission_tally(),
                waiting,
                "pending keys have no visible echo to admit"
            );
            let ribbon = h.glow.v2_ribbon().unwrap();
            assert!(
                (cut..COL0 + original.len()).all(|col| ribbon.field_at(ROW, col as u16).is_none()),
                "{stall_ms}ms/{keys} keys: pending key {ch} relit the blank suffix"
            );
            assert!(
                coverage(&h.glow, geom(), ROW)[cut + 1..COL0 + original.len()]
                    .iter()
                    .all(|&cov| cov == 0)
            );
            assert!(
                coverage(&h.glow, geom(), ROW + 2)
                    .iter()
                    .all(|&cov| cov == 0)
            );
        }
    }
    (h, original)
}

#[test]
fn a_dark_partial_repaint_restores_on_the_first_frame_with_pending_keys() {
    for (stall_ms, keys, tick_pending, exact) in [
        (300, 0, false, true),
        (300, 1, false, true),
        (800, 3, false, true),
        (300, 1, true, true),
        (800, 3, true, true),
        (300, 3, false, false),
        (1, 1, false, true),
    ] {
        eprintln!(
            "pending return: stall={stall_ms}, keys={keys}, ticks={tick_pending}, exact={exact}"
        );
        let (mut h, original) = pending_repaint(stall_ms, keys, tick_pending, exact);
        let before = h.glow.admission_tally();
        h.now += Duration::from_millis(1);
        h.label = format!(
            "return after {stall_ms}ms, {keys} pending, ticks={tick_pending}, exact={exact}"
        );
        h.repaint(Split::Whole);
        let ribbon = h.glow.v2_ribbon().expect("ribbon");
        for old in &original {
            let cell = ribbon
                .cells()
                .iter()
                .find(|cell| (cell.row, cell.col, cell.born) == (old.row, old.col, old.born))
                .unwrap_or_else(|| {
                    panic!(
                        "{}: original cell {:?} disappeared; residents={:?}\n{}",
                        h.label,
                        (old.row, old.col, old.born),
                        ribbon
                            .cells()
                            .iter()
                            .map(|cell| (
                                cell.row,
                                cell.col,
                                cell.born,
                                cell.cohort,
                                cell.released_at
                            ))
                            .collect::<Vec<_>>(),
                        h.story()
                    )
                });
            assert_eq!(
                (cell.cohort, cell.t),
                (old.cohort, old.t),
                "{}: original cell {} keeps its hue walk",
                h.label,
                old.col
            );
            assert!(
                !cell.leaving() && cell.released_at.is_none(),
                "{}: cell {} restored",
                h.label,
                old.col
            );
            assert!(
                ribbon.field_at(ROW, old.col).is_some(),
                "{}: first-return field at {}",
                h.label,
                old.col
            );
        }
        for col in COL0..COL0 + h.text.len() {
            assert!(
                ribbon
                    .cells()
                    .iter()
                    .any(|cell| cell.row == ROW && usize::from(cell.col) == col && !cell.leaving()),
                "{}: expected cell {col}",
                h.label
            );
        }
        let cut = COL0 + "hello ".len();
        let y = i32::from(ROW) * CH as i32 + CH as i32 / 2;
        for col in [cut + 3, cut + 6, COL0 + original.len() - 2] {
            let x = (col * CW + CW / 2) as i32;
            assert!(
                h.glow.under_quads().iter().any(|q| q.alpha >= 50
                    && i32::from(q.x) <= x
                    && x < i32::from(q.x) + i32::from(q.w)
                    && i32::from(q.y) <= y
                    && y < i32::from(q.y) + i32::from(q.h)),
                "{}: first-return old-suffix geometry at {col}",
                h.label
            );
        }
        let after = h.glow.admission_tally();
        assert_eq!(
            after.licensed - before.licensed,
            u64::from(keys > 0),
            "{}: one coalesced echo move",
            h.label
        );
        assert_eq!(
            after.declined, before.declined,
            "{}: no refused echo",
            h.label
        );
        assert!(
            ribbon.cells().iter().all(|cell| cell.row == ROW),
            "{}: no footer identity",
            h.label
        );
        assert!((0..COLS).all(|col| ribbon.field_at(ROW + 2, col as u16).is_none()));
        assert!(
            coverage(&h.glow, geom(), ROW + 2)
                .iter()
                .all(|&cov| cov == 0)
        );

        let clocks = ribbon
            .cohorts()
            .iter()
            .map(|cohort| (cohort.id, cohort.clock()))
            .collect::<Vec<_>>();
        let expires = ribbon
            .cohorts()
            .iter()
            .map(|cohort| cohort.clock() + Duration::from_secs_f32(cohort.swoosh_total()))
            .max()
            .unwrap();
        h.step();
        h.repaint(Split::Whole);
        assert_eq!(
            h.glow.admission_tally(),
            after,
            "a repeated frame earns no new admission"
        );
        assert_eq!(
            h.glow
                .v2_ribbon()
                .unwrap()
                .cohorts()
                .iter()
                .map(|cohort| (cohort.id, cohort.clock()))
                .collect::<Vec<_>>(),
            clocks,
            "repainting does not renew the genuine keys' clocks"
        );
        h.idle_to(expires + Duration::from_millis(3 * FRAME_MS));
        assert!(
            h.glow.v2_ribbon().unwrap().at_rest(),
            "{}: natural retirement",
            h.label
        );
        assert!(
            h.glow
                .v2_ribbon()
                .unwrap()
                .next_change_deadline(h.now)
                .is_none()
        );
        h.repaint(Split::Whole);
        assert!(
            h.glow.v2_ribbon().unwrap().at_rest(),
            "a late identical repaint earns no light"
        );
    }
}

#[test]
fn stalled_repaint_custody_is_dark_and_expires_with_its_run() {
    for reduced_motion in [false, true] {
        let mut h = Host::new(false);
        h.glow.set_reduced_motion(reduced_motion);
        h.type_str("hello brave new world");
        h.idle(200);
        let cut = COL0 + "hello ".len();
        h.program(format!("\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, cut + 1).as_bytes());
        h.idle(300);
        assert!(
            h.glow
                .v2_ribbon()
                .expect("ribbon")
                .cells()
                .iter()
                .any(|cell| { usize::from(cell.col) >= cut && cell.released_at.is_some() }),
            "partial release still has exact identities in custody"
        );
        let cov = coverage(&h.glow, geom(), ROW);
        assert!(
            cov[cut + 1..COL0 + h.text.len()].iter().all(|&c| c == 0),
            "custody paints no light"
        );
        assert!(
            h.glow
                .v2_ribbon()
                .expect("ribbon")
                .field_at(ROW, cut as u16)
                .is_none(),
            "custody owns no field"
        );
        h.idle(3000);
        assert!(
            h.glow.v2_ribbon().expect("ribbon").at_rest(),
            "custody expires with the original run"
        );
        assert!(
            h.glow
                .v2_ribbon()
                .expect("ribbon")
                .next_change_deadline(h.now)
                .is_none(),
            "expired custody schedules no wake"
        );
        h.repaint(Split::Whole);
        assert!(
            h.glow.v2_ribbon().expect("ribbon").at_rest(),
            "a late same-glyph repaint earns no new light"
        );
    }
}

#[test]
fn abandoning_a_stalled_repaints_run_retires_its_custody() {
    let mut h = Host::new(false);
    h.type_str("hello brave new world");
    h.idle(200);
    let cut = COL0 + "hello ".len();
    h.program(format!("\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, cut + 1).as_bytes());
    h.idle(300);
    h.text.clear();
    h.caret = 0;
    h.repaint(Split::Whole);
    h.idle(((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64 + 3 * FRAME_MS);
    assert!(
        h.glow.v2_ribbon().expect("ribbon").at_rest(),
        "a whole clear releases all partial custody on its existing swoosh"
    );
    assert!(
        h.glow
            .v2_ribbon()
            .expect("ribbon")
            .next_change_deadline(h.now)
            .is_none(),
        "a cleared run schedules no custody wake"
    );
}

#[test]
fn replaced_glyphs_revoke_a_stalled_repaints_custody() {
    let mut h = Host::new(false);
    h.type_str("hello brave new world");
    h.idle(200);
    let cut = COL0 + "hello ".len();
    h.program(format!("\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, cut + 1).as_bytes());
    h.idle(300);
    let original = h.text.clone();
    h.text = format!("hello {}", "Z".repeat(original.len() - "hello ".len()));
    h.repaint(Split::Whole);
    h.idle((RETIRE_MELT_S * 1000.0) as u64 + 3 * FRAME_MS);
    assert!(
        h.glow
            .v2_ribbon()
            .expect("ribbon")
            .cells()
            .iter()
            .all(|cell| usize::from(cell.col) < cut),
        "replacement retires every released original glyph"
    );
    h.text = original;
    h.repaint(Split::Whole);
    assert!(
        h.glow
            .v2_ribbon()
            .expect("ribbon")
            .cells()
            .iter()
            .all(|cell| usize::from(cell.col) < cut),
        "a later same-glyph repaint cannot restore revoked custody"
    );
}

#[test]
fn a_stalled_repaints_dark_tail_follows_its_standing_prefix() {
    let mut h = Host::new(false);
    h.type_str("hello brave new world");
    h.idle(200);
    let cut = COL0 + "hello ".len();
    h.program(format!("\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, cut + 1).as_bytes());
    h.idle(300);
    h.program(
        format!(
            "\x1b[{};1H\x1b[2K\x1b[{};1H\x1b[2K│ > hello \x1b[{};{}H\x1b[?25h",
            ROW + 1,
            ROW,
            ROW,
            cut + 1
        )
        .as_bytes(),
    );
    let text = h.text.clone();
    h.program(
        format!(
            "\x1b[?25l\x1b[{};1H\x1b[2K│ > {text}\x1b[{};{}H\x1b[?25h",
            ROW,
            ROW,
            COL0 + text.len() + 1
        )
        .as_bytes(),
    );
    let ribbon = h.glow.v2_ribbon().expect("ribbon");
    let missing: Vec<_> = (COL0..COL0 + text.len())
        .filter(|&col| {
            !ribbon
                .cells()
                .iter()
                .any(|cell| cell.row == ROW - 1 && usize::from(cell.col) == col && !cell.leaving())
        })
        .collect();
    assert!(
        missing.is_empty(),
        "moved prefix stranded its dark tail: {missing:?}"
    );
}

#[test]
fn a_stalled_repaints_dark_tail_shifts_with_its_standing_prefix() {
    for insert_at in [0, 2] {
        let mut h = Host::new(false);
        h.type_str("hello brave new world");
        h.idle(200);
        let cut = COL0 + "hello ".len();
        h.program(format!("\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, cut + 1).as_bytes());
        h.idle(300);
        let prefix = "hello ";
        let shifted = format!("{}++{}", &prefix[..insert_at], &prefix[insert_at..]);
        h.program(
            format!(
                "\x1b[{};1H\x1b[2K│ > {shifted}\x1b[{};{}H\x1b[?25h",
                ROW + 1,
                ROW + 1,
                cut + 3
            )
            .as_bytes(),
        );
        let text = h.text.clone();
        let shifted = format!("{}++{}", &text[..insert_at], &text[insert_at..]);
        h.program(
            format!(
                "\x1b[?25l\x1b[{};1H\x1b[2K│ > {shifted}\x1b[{};{}H\x1b[?25h",
                ROW + 1,
                ROW + 1,
                COL0 + text.len() + 3
            )
            .as_bytes(),
        );
        let ribbon = h.glow.v2_ribbon().expect("ribbon");
        let missing: Vec<_> = (0..text.len())
            .map(|i| COL0 + i + if i >= insert_at { 2 } else { 0 })
            .filter(|&col| {
                !ribbon
                    .cells()
                    .iter()
                    .any(|cell| cell.row == ROW && usize::from(cell.col) == col && !cell.leaving())
            })
            .collect();
        assert!(
            missing.is_empty(),
            "insert at {insert_at}: shifted prefix stranded its dark tail: {missing:?}"
        );
    }
}

#[test]
fn moving_only_part_of_a_standing_prefix_does_not_guess_where_its_dark_tail_went() {
    let mut h = Host::new(false);
    h.type_str("hello brave new world");
    h.idle(200);
    let cut = COL0 + "hello brave".len();
    h.program(format!("\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, cut + 1).as_bytes());
    h.idle(300);
    h.program(
        format!(
            "\x1b[{};1H\x1b[2K│ > hello \x1b[{};{}Hbrave\x1b[{};{}H\x1b[?25h",
            ROW + 1,
            ROW,
            COL0 + "hello ".len() + 1,
            ROW,
            cut + 1
        )
        .as_bytes(),
    );
    let ribbon = h.glow.v2_ribbon().expect("ribbon");
    let held: Vec<_> = ribbon
        .cells()
        .iter()
        .filter(|cell| cell.released_at.is_some() && usize::from(cell.col) >= cut)
        .collect();
    assert!(!held.is_empty(), "the original run still has dark custody");
    assert!(
        held.iter().all(|cell| cell.row == ROW),
        "a partial visible split cannot choose a new owner for dark custody"
    );
    assert!(
        ribbon
            .cells()
            .iter()
            .any(|cell| cell.row == ROW - 1 && !cell.leaving()),
        "the witnessed partial move actually happened"
    );
}

#[test]
fn a_whole_run_re_laid_on_another_row_follows_its_text() {
    let mut h = Host::new(false);
    h.type_str("hello brave new world");
    h.idle(200);
    let text = h.text.clone();
    h.label = "moved".into();
    h.program(
        format!(
            "\x1b[?25l\x1b[{};1H\x1b[2K\x1b[{};1H\x1b[2K│ > {text}\x1b[{};{}H\x1b[?25h",
            ROW + 1,
            ROW - 1,
            ROW - 1,
            COL0 + text.len() + 1
        )
        .as_bytes(),
    );
    let moved = h.census.len() - 1;
    let c = &h.census[moved];
    assert!(
        c.live.is_empty() && c.retiring.is_empty(),
        "the old row keeps no cell: live {:?}, retiring {:?}",
        c.live,
        c.retiring
    );
    let up = coverage(&h.glow, geom(), ROW - 2);
    let lit_up: Vec<usize> = (0..COLS).filter(|&i| up[i] >= LIT_COV).collect();
    assert!(
        lit_up.len() >= text.len() / 2,
        "the band followed its text to the row above: lit {lit_up:?}\n{}",
        h.story()
    );
    h.idle((RETIRE_MELT_S * 1000.0) as u64 + 2 * FRAME_MS);
    let last = h.census.last().expect("a frame");
    assert!(
        !last.lit.contains('#'),
        "the old row is dark one melt later: {}",
        last.lit
    );
}

const FIXTURES: [(&str, &str); 4] = [
    (
        "insert",
        include_str!("fixtures/claude-composer-2026-09-21.ptylog"),
    ),
    (
        "end",
        include_str!("fixtures/claude-composer-2026-09-21-end.ptylog"),
    ),
    (
        "scrub3",
        include_str!("fixtures/claude-composer-2026-09-21-scrub3.ptylog"),
    ),
    (
        "popup",
        include_str!("fixtures/claude-composer-2026-09-21-popup.ptylog"),
    ),
];
const REC_ROWS: usize = 40;
const REC_COLS: usize = 120;
const REC_CW: usize = 7;
const REC_CH: usize = 14;
const REC_ROW: u16 = 37;

fn replay(src: &str, torn: bool) -> (Vec<HoleFrame>, usize) {
    let now0 = Instant::now();
    let mut term = Terminal::new(REC_ROWS as u16, REC_COLS as u16);
    let mut glow = crate::common::quads_glow();
    glow.note_pane_columns(0, REC_COLS);
    let g = crate::common::geom(REC_ROWS, REC_COLS, REC_CW, REC_CH);
    let cfg = rainbow_cfg();
    let mut out = Vec::new();
    let mut row_buf = Vec::new();
    let mut blink = 0u64;
    let mut now = now0;
    let rec = recording(src, 1_000_000);
    let first_key = rec
        .iter()
        .find(|(_, e)| matches!(e, Rec::In(b) if is_key(b)))
        .map(|(ms, _)| *ms)
        .expect("a typed key");
    let exit = rec
        .iter()
        .find(|(_, e)| matches!(e, Rec::In(b) if b.as_slice() == b"\x1b[99;5u"))
        .map_or(u64::MAX, |(ms, _)| *ms);
    let last_key = rec
        .iter()
        .rev()
        .find(|(ms, e)| *ms < exit && matches!(e, Rec::In(b) if is_key(b)))
        .map(|(ms, _)| *ms)
        .expect("a last key");
    let mut holes = Vec::new();
    let mut frames = 0usize;
    let mut frame = |term: &Terminal, glow: &mut CursorGlow, now: Instant, frames: &mut usize| {
        let (c, cur) = caret_sample(term);
        note_blink(term, glow, &mut blink, now);
        glow.note_context(term.is_alternate_screen());
        observe_caret_row(term, glow, &mut row_buf, c, now);
        observe_ribbon_rows(term, glow, &mut row_buf);
        glow.tick(cur, now, &cfg, g, &mut out);
        *frames += 1;
        let ms = now.saturating_duration_since(now0).as_millis() as u64;
        if ms >= first_key && ms <= last_key {
            let cov = coverage(glow, g, REC_ROW);
            let found = holes_of(&cov);
            if !found.is_empty() {
                let map: String = cov
                    .iter()
                    .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                    .collect();
                holes.push((ms, map, found));
            }
        }
    };
    for (ms, ev) in rec {
        if ms >= exit {
            break;
        }
        let t = now0 + Duration::from_millis(ms);
        while now + Duration::from_millis(FRAME_MS) <= t {
            now += Duration::from_millis(FRAME_MS);
            frame(&term, &mut glow, now, &mut frames);
        }
        now = now.max(t);
        match ev {
            Rec::In(b) => match b.as_slice() {
                [c] if (0x20..0x7f).contains(c) => {
                    let (shifted, class) = class_of(*c as char);
                    glow.note_typed_glyph(now, 1, shifted, class);
                }
                b"\x1b[1;3D" | b"\x1b[1;3C" | b"\x1b[F" => glow.note_motion(now),
                _ => {}
            },
            Rec::Out(bytes) => {
                let cut = if torn { bytes.len() / 2 } else { bytes.len() };
                term.process(&bytes[..cut]);
                frame(&term, &mut glow, now, &mut frames);
                if cut < bytes.len() {
                    now += Duration::from_millis(1);
                    term.process(&bytes[cut..]);
                    frame(&term, &mut glow, now, &mut frames);
                }
            }
        }
    }
    (holes, frames)
}

#[test]
fn claude_code_s_recorded_bytes_read_whole_or_torn_keep_the_band_whole_while_typing() {
    for (name, src) in FIXTURES {
        for torn in [false, true] {
            let (holes, frames) = replay(src, torn);
            assert!(frames > 500, "{name}: the replay presented {frames} frames");
            assert!(
                holes.is_empty(),
                "{name} (torn={torn}): {} frames with an interior hole while typing, first at {} ms {:?}\n{}",
                holes.len(),
                holes[0].0,
                holes[0].2,
                holes[0].1
            );
        }
    }
}
