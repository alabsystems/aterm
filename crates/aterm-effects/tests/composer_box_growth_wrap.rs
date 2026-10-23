// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for composer line-wrap rainbow flow and walk continuity.

use crate::common::{
    caret_sample, note_blink, observe_caret_row, observe_ribbon_rows, rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::{
    FLOW_TAIL_S, FLOW_TOTAL_S, HOT_EDGE_CELLS_MAX, RETIRE_MELT_S, WALK_FAST_CELLS, WALK_LAY_RATE,
};
use std::time::{Duration, Instant};

const ROWS: usize = 30;
const COLS: usize = 90;
const CW: usize = 15;
const CH: usize = 28;

const FRAME_US: u64 = 16_667;
const KEY_MS: u64 = 60;

const RULE_ROW: usize = 27;
const TEXT_ROW: usize = 28;
const OLD_ROW: u16 = (TEXT_ROW - 1) as u16;
const NEW_ROW: u16 = OLD_ROW - 1;

const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

const LINE1: &str =
    "HEY!!! [Image #1] in the other tab in aterm, I got this confirmation message. AUDIT";

fn cfg() -> GlowConfig {
    GlowConfig {
        theme_fg: 0x00D0_D0D0,
        theme_bg: 0x0011_1318,
        ..rainbow_cfg()
    }
}

fn rule() -> String {
    let mut s: String = "\u{2500}".repeat(78);
    s.push_str("\x1b[80Gworkspace\x1b[90G\u{2500}");
    s
}

fn glyph_bytes(row: usize, ch: char, c: usize) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}C\x1b[{}B{ch}\x1b[30;1H\x1b[{row};{}H\x1b[?25h{SYNC_END}",
        c - 1,
        row - 1,
        c + 1
    )
    .into_bytes()
}

fn space_bytes(row: usize, c: usize) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[{row};{}H\x1b[?25h{SYNC_END}",
        c + 1
    )
    .into_bytes()
}

fn wrap_bytes(line1: &str) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}B{}\r\x1b[1B\u{276f}\u{a0}{line1}\x1b[K\r\x1b[1B  WH\x1b[6G\x1b[K\x1b[30;1H\x1b[{TEXT_ROW};6H\x1b[?25h{SYNC_END}",
        RULE_ROW - 2,
        rule()
    )
    .into_bytes()
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    now: Instant,
    next_frame: Instant,
    last_key: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
    /// The 1-based column the next glyph lands on.
    col: usize,
    /// The 1-based row the next glyph lands on: the text row, or the row a
    /// top-anchored box grew down onto.
    row: usize,
}

impl Host {
    fn new() -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
        term.process(
            format!(
                "\x1b[{RULE_ROW};1H{}\x1b[{TEXT_ROW};1H\u{276f}\u{a0}\x1b[29;1H  auto mode on\x1b[{TEXT_ROW};3H",
                rule()
            )
            .as_bytes(),
        );
        let now = Instant::now();
        let mut h = Self {
            term,
            glow: CursorGlow::default(),
            cfg: cfg(),
            g: crate::common::geom(ROWS, COLS, CW, CH),
            now,
            next_frame: now,
            last_key: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            col: 3,
            row: TEXT_ROW,
        };
        h.frame();
        h
    }

    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink_seen, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        self.next_frame = self.now + Duration::from_micros(FRAME_US);
    }

    fn advance_to(&mut self, t: Instant) {
        while self.next_frame <= t {
            self.now = self.next_frame;
            self.frame();
        }
        self.now = self.now.max(t);
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.advance_to(t);
    }

    fn press(&mut self, ch: char, bytes: &[u8]) {
        let t = self.last_key + Duration::from_millis(KEY_MS);
        self.advance_to(t);
        self.now = t;
        self.last_key = t;
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        self.glow.note_typed_glyph(t, 1, false, class);
        self.term.process(bytes);
    }

    fn key(&mut self, ch: char) {
        let bytes = if ch == ' ' {
            space_bytes(self.row, self.col)
        } else {
            glyph_bytes(self.row, ch, self.col)
        };
        self.press(ch, &bytes);
        self.col += 1;
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    fn wrap(&mut self, line1: &str) {
        self.press('Y', &wrap_bytes(line1));
        self.col = 6;
    }

    fn next_frame(&mut self) {
        let t = self.next_frame;
        self.advance_to(t);
    }

    fn ribbon(&self) -> &aterm_effects::rainbow_kitty::ribbon::Ribbon {
        self.glow.v2_ribbon().expect("rainbow kitty owns the frame")
    }

    fn cells(&self, row: u16) -> Vec<(u16, bool)> {
        let mut v: Vec<(u16, bool)> = self
            .ribbon()
            .cells()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| (c.col, c.leaving()))
            .collect();
        v.sort_unstable();
        v
    }

    fn live(&self, row: u16) -> Vec<u16> {
        self.cells(row)
            .into_iter()
            .filter(|&(_, leaving)| !leaving)
            .map(|(col, _)| col)
            .collect()
    }

    fn row_quads(&self, row: u16) -> impl Iterator<Item = &GlowQuad> {
        let centre = f32::from(self.g.origin_y) + (f32::from(row) + 0.5) * CH as f32;
        self.glow.under_quads().iter().filter(move |q| {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            q.w > 0 && q.alpha > 0 && y0 <= centre && centre < y1
        })
    }

    fn lit(&self, row: u16) -> bool {
        self.row_quads(row).next().is_some()
    }

    fn coverage(&self, row: u16) -> f32 {
        let sum: f32 = self
            .row_quads(row)
            .map(|q| f32::from(q.w) * f32::from(q.alpha) / 255.0)
            .sum();
        sum / (COLS * CW) as f32
    }

    fn left_coverage(&self, row: u16) -> f32 {
        let edge = (40 * CW) as f32;
        let sum: f32 = self
            .row_quads(row)
            .map(|q| {
                let x0 = f32::from(q.x);
                let x1 = (x0 + f32::from(q.w)).min(edge);
                (x1 - x0).max(0.0) * f32::from(q.alpha) / 255.0
            })
            .sum();
        sum / edge
    }

    fn centroid(&self, row: u16) -> Option<f32> {
        let (mut num, mut den) = (0.0f32, 0.0f32);
        for q in self.row_quads(row) {
            let w = f32::from(q.w) * f32::from(q.alpha) / 255.0;
            num += (f32::from(q.x) + f32::from(q.w) * 0.5) * w;
            den += w;
        }
        (den > 0.0).then(|| num / den)
    }

    fn far_tail(&self, row: u16, from_col: u16) -> f32 {
        let cw = CW as f32;
        let from_x = f32::from(self.g.origin_x) + f32::from(from_col) * cw;
        let head_x = f32::from(self.g.origin_x) + f32::from(self.term.cursor().col) * cw;
        let y_lo = f32::from(self.g.origin_y) + (f32::from(row) - 0.5) * CH as f32;
        let y_hi = y_lo + CH as f32;
        self.out
            .iter()
            .filter(|q| {
                let (x, y) = (f32::from(q.x), f32::from(q.y));
                q.w > 0
                    && y >= y_lo
                    && y < y_hi
                    && x >= from_x
                    && x < head_x - (HOT_EDGE_CELLS_MAX + 1.0) * cw
            })
            .map(|q| {
                let lum = (q.color >> 16 & 0xFF) + (q.color >> 8 & 0xFF) + (q.color & 0xFF);
                f32::from(q.w) * lum as f32
            })
            .sum()
    }

    fn left_edge(&self, row: u16) -> Option<u16> {
        self.row_quads(row).map(|q| q.x).min()
    }

    fn retired(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.retired)
    }

    fn followed(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.followed)
    }

    fn census(&mut self, row: u16, ms: u64) -> Vec<Sample> {
        let start = self.now;
        let mut out = vec![self.sample(row, start)];
        let end = start + Duration::from_millis(ms);
        while self.next_frame <= end {
            self.next_frame();
            out.push(self.sample(row, start));
        }
        out
    }

    fn sample(&self, row: u16, start: Instant) -> Sample {
        Sample {
            t: self.now.saturating_duration_since(start).as_secs_f32(),
            cov: self.coverage(row),
            centroid: self.centroid(row),
            left: self.left_edge(row),
            resident: self.cells(row).len(),
        }
    }
}

/// One frame of a row's census.
#[derive(Clone, Copy, Debug)]
struct Sample {
    t: f32,
    cov: f32,
    centroid: Option<f32>,
    /// The row's leftmost lit pixel — the band's far edge on a row above
    /// the caret.
    left: Option<u16>,
    resident: usize,
}

fn dark_at(curve: &[Sample]) -> Option<f32> {
    let last_lit = curve.iter().rposition(|s| s.cov > 0.0)?;
    curve.get(last_lit + 1).map(|s| s.t)
}

fn empty_at(curve: &[Sample]) -> Option<f32> {
    curve.iter().find(|s| s.resident == 0).map(|s| s.t)
}

fn worst_step(curve: &[Sample]) -> f32 {
    curve
        .windows(2)
        .map(|w| w[0].cov - w[1].cov)
        .fold(0.0, f32::max)
}

fn max_rise(curve: &[Sample]) -> f32 {
    curve
        .windows(2)
        .map(|w| w[1].cov - w[0].cov)
        .fold(0.0, f32::max)
}

const RISE_TOL: f32 = 2e-3;

fn worst_far_edge_step(curve: &[Sample], floor: f32) -> u16 {
    curve
        .windows(2)
        .filter(|w| w[0].cov >= floor && w[1].cov >= floor)
        .filter_map(|w| Some(w[0].left?.saturating_sub(w[1].left?)))
        .max()
        .unwrap_or(0)
}

fn fmt(curve: &[Sample]) -> String {
    curve
        .iter()
        .step_by(6)
        .map(|s| {
            format!(
                "{:.2}:{:.3}/{}@{}L{}",
                s.t,
                s.cov,
                s.resident,
                s.centroid.map_or(-1.0, |c| c.round()),
                s.left.map_or(-1, i32::from)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const FRAME_S: f32 = FRAME_US as f32 / 1e6;

fn typed_to_the_wrap() -> (Host, f32, usize, f32) {
    let mut h = Host::new();
    h.type_str(LINE1);
    h.type_str(" WH");
    h.next_frame();
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (OLD_ROW, 88), "the caret stands after `WH`");
    let live = h.live(OLD_ROW);
    assert!(
        live.len() >= 80 && live[0] == 2 && *live.last().expect("laid") == 87,
        "the band is laid under the text: {live:?}"
    );
    let pre_cov = h.coverage(OLD_ROW);
    assert!(
        pre_cov > 0.5,
        "the row is well lit before the wrap: {pre_cov}"
    );
    let line_end = 2 + LINE1.len() as u16 - 1;
    let n_pre = live.iter().filter(|&&c| c <= line_end).count();
    let t_w = h
        .ribbon()
        .cells()
        .iter()
        .find(|c| c.row == OLD_ROW && c.col == 86 && !c.leaving())
        .map(|c| c.t)
        .expect("the `W`'s cell");
    (h, pre_cov, n_pre, t_w)
}

#[test]
fn one_frame_after_the_wrap_the_band_stands_under_its_text_a_row_up_and_is_not_melted() {
    let (mut h, pre_cov, n_pre, _) = typed_to_the_wrap();
    h.wrap(LINE1);
    h.next_frame();
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (OLD_ROW, 5),
        "the caret re-anchored left on its row"
    );
    let live = h.live(NEW_ROW);
    let line_end = 2 + LINE1.len() as u16 - 1;
    assert!(
        live.len() == n_pre && live[0] == 2 && *live.last().expect("followed") == line_end,
        "the band followed its text one row up, every cell 2..={line_end}: {live:?}"
    );
    assert_eq!(
        h.followed(),
        n_pre as u64,
        "`ribbon_followed=` counts exactly the cells the text carried"
    );
    assert!(
        h.retired() <= 3,
        "`ribbon_retired=` counts none of the followed cells (at most the moved word's \
         two old cells and the space the wrap ate, which the witness finds re-laid on \
         the continuation row): {}",
        h.retired()
    );
    let cov = h.coverage(NEW_ROW);
    assert!(
        cov >= 0.6 * pre_cov,
        "one frame after the wrap the followed band carries the light it had: {cov} \
         against {pre_cov} before"
    );
    let melt = ((RETIRE_MELT_S + 0.03) * 1000.0) as u64;
    h.idle(melt);
    let cov = h.coverage(NEW_ROW);
    assert!(
        cov >= 0.5 * pre_cov,
        "at +{:.2} s the followed band is not melted: {cov} against {pre_cov}",
        RETIRE_MELT_S + 0.03
    );
    assert!(
        h.live(NEW_ROW).len() >= n_pre - 2,
        "…and its cells stand, unstamped: {:?}",
        h.cells(NEW_ROW)
    );
}

#[test]
fn the_followed_row_flows_into_the_fold_and_is_dark_by_the_flows_end() {
    let (mut h, pre_cov, _, _) = typed_to_the_wrap();
    h.wrap(LINE1);
    let left_before = h.left_edge(OLD_ROW).expect("lit before the wrap");
    h.next_frame();
    let curve = h.census(NEW_ROW, 1800);
    let shown = fmt(&curve);
    let at = |t: f32| {
        curve
            .iter()
            .find(|s| s.t >= t)
            .copied()
            .unwrap_or_else(|| panic!("a sample at +{t} s"))
    };
    assert!(
        at(0.5).cov > 0.0 && at(0.5).resident > 0,
        "the followed row is still lit at +0.5 s: {shown}"
    );
    let far: Vec<(f32, Option<u16>)> = curve.iter().map(|s| (s.t, s.left)).collect();
    assert!(
        worst_far_edge_step(&curve, 0.05 * curve[0].cov) <= 1,
        "the band's far edge moves only toward the fold (right) while it holds 5 % of its \
         light: {far:?}"
    );
    let last_lit = curve.iter().rposition(|s| s.cov > 0.0).expect("lit");
    let (c0, c1) = (
        curve[0].centroid.expect("lit at the wrap"),
        curve[last_lit].centroid.expect("lit at the end"),
    );
    assert!(
        c1 > c0 + 2.0 * CW as f32,
        "…and it MOVES: the centroid went from {c0} px to {c1} px over the flow: {shown}"
    );
    assert!(
        max_rise(&curve) <= RISE_TOL,
        "coverage never rises after the hand left the row: {shown}"
    );
    assert!(
        worst_step(&curve) <= 0.1 * pre_cov,
        "never the 120 ms cut: the worst frame-to-frame drop is {} of a {pre_cov} \
         band: {shown}",
        worst_step(&curve)
    );
    h = typed_to_the_wrap().0;
    h.wrap(LINE1);
    h.next_frame();
    h.idle(300);
    let left_after = h.left_edge(NEW_ROW).expect("still lit at +0.3 s");
    assert!(
        left_after > left_before + CW as u16,
        "the drain is left-first: the left edge moved from {left_before} to {left_after} px"
    );
    let dark = dark_at(&curve).expect("the row goes dark");
    let empty = empty_at(&curve).expect("the pool empties");
    assert!(
        dark <= FLOW_TOTAL_S + 2.0 * FRAME_S && empty <= FLOW_TOTAL_S + 2.0 * FRAME_S,
        "the flow is over by FLOW_TOTAL_S + a frame: dark at {dark}, empty at {empty}: {shown}"
    );
    assert!(
        dark >= FLOW_TOTAL_S - FLOW_TAIL_S,
        "…and not before the flow's last breath (its body thins out of the row's centre only \
         then): dark at {dark}"
    );
}

const AFTER_LONG: &str = " ATERM claude code how is the rainbow now";

#[test]
fn the_new_row_continues_the_old_rows_walk_at_the_pace_it_had_reached() {
    let (mut h, _, _, t_w) = typed_to_the_wrap();
    let old = |h: &Host, col: u16| {
        h.ribbon()
            .cells()
            .iter()
            .find(|c| c.row == OLD_ROW && c.col == col && !c.leaving())
            .map(|c| c.t)
            .unwrap_or_else(|| panic!("the old row's cell at {col}"))
    };
    let (t_space, t_h) = (old(&h, 85), old(&h, 87));
    let old_pace = t_h - t_w;
    assert!(
        (old_pace - WALK_LAY_RATE).abs() < 1e-4 && (t_w - t_space - WALK_LAY_RATE).abs() < 1e-4,
        "the premise: the old row lays at WALK_LAY_RATE at its end — {t_space} @85, \
         {t_w} @86, {t_h} @87"
    );
    h.wrap(LINE1);
    h.next_frame();
    h.type_str(AFTER_LONG);
    h.next_frame();
    h.idle(100);
    let rib = h.ribbon();
    let last = 4 + AFTER_LONG.len() as u16;
    let ts: Vec<(u16, f32)> = (2..=last)
        .map(|col| {
            let t = rib.field_at(OLD_ROW, col).unwrap_or_else(|| {
                panic!(
                    "column {col} of the new row is laid: {:?}",
                    h.cells(OLD_ROW)
                )
            });
            (col, t)
        })
        .collect();
    assert!(
        ts.len() as f32 > WALK_FAST_CELLS + 4.0,
        "the new row is laid past the walk's fast phase: {} cells",
        ts.len()
    );
    for w in ts.windows(2) {
        let (c0, t0) = w[0];
        let (c1, t1) = w[1];
        assert!(
            (t1 - t0 - old_pace).abs() < 1e-4,
            "the new row's step from column {c0} to {c1} is {} — the old row laid at \
             {old_pace} a cell at its end; the walk restarted its pace at the fold: {ts:?}",
            t1 - t0
        );
    }
    let (_, t_first) = ts[0];
    let (_, t_second) = ts[1];
    assert!(
        (t_first - t_w).abs() < 1e-4 && (t_second - t_h).abs() < 1e-4,
        "the relaid `WH` keeps the t it had on the old row: `W` {t_first} against \
         {t_w}, `H` {t_second} against {t_h}: {ts:?}"
    );
    for &(col, t) in &ts {
        let want = t_w + f32::from(col - 2) * WALK_LAY_RATE;
        assert!(
            (t - want).abs() < 1e-3,
            "column {col} of the new row is {t}, the old row's walk continued is {want}: \
             {ts:?}"
        );
    }
}

#[test]
fn a_true_re_layout_with_different_text_a_row_up_still_melts_the_old_band() {
    let (mut h, _, n_pre, _) = typed_to_the_wrap();
    let other: String = LINE1
        .chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            c => c,
        })
        .collect();
    h.wrap(&other);
    h.next_frame();
    assert_eq!(
        h.followed(),
        0,
        "nothing followed: the text is not the run's"
    );
    assert!(
        h.retired() >= n_pre as u64,
        "the old band is retired by content, as before: {}",
        h.retired()
    );
    assert!(
        h.live(NEW_ROW).is_empty(),
        "the row the other text went to is dark: {:?}",
        h.cells(NEW_ROW)
    );
    let leaving = h.cells(OLD_ROW).iter().filter(|&&(_, l)| l).count();
    assert!(
        leaving >= n_pre,
        "the old cells are on the melt: {:?}",
        h.cells(OLD_ROW)
    );
    h.idle(((RETIRE_MELT_S + 0.05) * 1000.0) as u64);
    assert!(
        h.cells(OLD_ROW).iter().all(|&(c, _)| c < 6),
        "…and gone inside the melt, the caret's own cells aside: {:?}",
        h.cells(OLD_ROW)
    );
    assert!(!h.lit(NEW_ROW));
}

fn unwrap_bytes(line: &str) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[{};1H\x1b[K\x1b[{RULE_ROW};1H{}\x1b[{TEXT_ROW};1H\u{276f}\u{a0}{line}\x1b[K\x1b[30;1H\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        RULE_ROW - 1,
        rule(),
        3 + line.chars().count()
    )
    .into_bytes()
}

#[test]
fn a_backspace_back_through_the_fold_rewets_the_row_the_hand_returned_to() {
    fn backspace_run(h: &mut Host, from_col: usize, n: usize) -> Vec<(u64, f32)> {
        let start = h.now;
        let mut out = Vec::new();
        for k in 0..n {
            let t = h.last_key + Duration::from_millis(KEY_MS);
            h.advance_to(t);
            h.now = t;
            h.last_key = t;
            h.glow.note_backspace(t);
            let c = from_col - 1 - k;
            h.term
                .process(format!("\x1b[{TEXT_ROW};{c}H\x1b[K").as_bytes());
            out.push((
                h.now.saturating_duration_since(start).as_millis() as u64,
                h.left_coverage(OLD_ROW),
            ));
        }
        out
    }
    let line = format!("{LINE1} WH");
    let (mut c, _, _, _) = typed_to_the_wrap();
    c.idle(160);
    let ctl = backspace_run(&mut c, 3 + line.chars().count(), 30);
    let floor = |v: &[(u64, f32)]| v[6..].iter().map(|s| s.1).fold(f32::MAX, f32::min);
    let want = floor(&ctl);
    for late in [false, true] {
        let (mut h, _, _, _) = typed_to_the_wrap();
        h.wrap(LINE1);
        h.next_frame();
        h.idle(100);
        let t = h.last_key + Duration::from_millis(KEY_MS);
        h.advance_to(t);
        h.now = t;
        h.last_key = t;
        h.glow.note_backspace(t);
        if late {
            h.next_frame();
        }
        let before = h.followed();
        h.term.process(&unwrap_bytes(&line));
        h.next_frame();
        assert!(
            h.followed() > before,
            "late={late}: the follow pass carried the band back down with its text"
        );
        assert!(
            h.ribbon()
                .cohorts()
                .iter()
                .filter(|c| c.row == OLD_ROW)
                .all(|c| c.flow.is_none()),
            "late={late}: nothing on the hand's row is flowing"
        );
        let run = backspace_run(&mut h, 3 + line.chars().count(), 30);
        let got = floor(&run);
        assert!(
            got >= 0.6 * want,
            "late={late}: the row the hand came back to holds its light under the \
             Backspace run like the control: floor {got} against {want}\nround trip \
             {run:?}\ncontrol {ctl:?}\ncells {:?}",
            h.cells(OLD_ROW)
        );
    }
}

#[test]
fn an_unwrap_takes_the_seam_off_the_line_the_hand_came_back_to() {
    const TAIL_FROM: u16 = 12;
    fn backspaces(h: &mut Host, from_col: usize, n: usize, until: Instant) -> f32 {
        let (mut k, mut worst) = (0usize, 0.0f32);
        while h.next_frame <= until {
            let due = h.last_key + Duration::from_millis(KEY_MS);
            if k < n && h.next_frame > due {
                h.advance_to(due);
                h.now = due;
                h.last_key = due;
                h.glow.note_backspace(due);
                let col = from_col - 1 - k;
                h.term
                    .process(format!("\x1b[{TEXT_ROW};{col}H\x1b[K").as_bytes());
                k += 1;
            }
            h.next_frame();
            worst = worst.max(h.far_tail(OLD_ROW, TAIL_FROM));
        }
        worst
    }
    let line = format!("{LINE1} WH");
    let from_col = 3 + line.chars().count();
    let (mut c, _, _, _) = typed_to_the_wrap();
    let until = c.now + Duration::from_millis(1600);
    let ctl = backspaces(&mut c, from_col, 8, until);
    for idle in [40u64, 100, 300] {
        let (mut h, _, _, _) = typed_to_the_wrap();
        h.wrap(LINE1);
        h.next_frame();
        let wrapped = h.last_key;
        h.idle(idle);
        let t = h.now.max(h.last_key + Duration::from_millis(KEY_MS));
        h.advance_to(t);
        h.now = t;
        h.last_key = t;
        h.glow.note_backspace(t);
        let before = h.followed();
        h.term.process(&unwrap_bytes(&line));
        h.next_frame();
        assert!(
            h.followed() > before,
            "unwrap {idle} ms after the wrap: the follow pass carried line 1 back down"
        );
        let got = backspaces(&mut h, from_col, 8, wrapped + Duration::from_millis(1600));
        assert!(
            got <= 2.0 * ctl + 200.0,
            "unwrap {idle} ms after the wrap: the new-line seam stayed on the line the hand \
             came back to — its far tail reads {got} lum·px in a frame, where the line that \
             never wrapped reads {ctl}"
        );
    }
}

fn cohorts_on(h: &Host, row: u16) -> Vec<(u16, f32, f32, u16, u16, bool)> {
    h.ribbon()
        .cohorts()
        .iter()
        .filter(|k| k.row == row)
        .map(|k| (k.anchor_col, k.t0, k.d0, k.col0, k.col1, k.abandoned))
        .collect()
}

#[test]
fn after_a_wrap_whose_old_row_melts_the_next_row_continues_the_walk_at_its_pace() {
    let (mut h, _, _, t_w) = typed_to_the_wrap();
    let other: String = LINE1
        .chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            c => c,
        })
        .collect();
    h.wrap(&other);
    h.next_frame();
    assert_eq!(
        h.followed(),
        0,
        "the premise: nothing followed, the old row melts"
    );
    h.type_str(AFTER_LONG);
    h.next_frame();
    h.idle(100);
    let rib = h.ribbon();
    let last = 4 + AFTER_LONG.len() as u16;
    let ts: Vec<(u16, f32)> = (2..=last)
        .map(|col| {
            let t = rib.field_at(OLD_ROW, col).unwrap_or_else(|| {
                panic!(
                    "column {col} of the new row is laid: {:?}",
                    h.cells(OLD_ROW)
                )
            });
            (col, t)
        })
        .collect();
    for &(col, t) in &ts {
        let want = t_w + f32::from(col - 2) * WALK_LAY_RATE;
        assert!(
            (t - want).abs() < 1e-3,
            "column {col} of the row after a melted wrap is {t}, the old row's walk \
             continued is {want}: {ts:?}\ncohorts {:?}",
            cohorts_on(&h, OLD_ROW)
        );
    }
}

const LINE_A: &str = "abcdefghijklmnopqrstuvwxyzabcd";

fn newline_up_bytes(line: &str) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}B{}\r\x1b[1B\u{276f}\u{a0}{line}\x1b[K\r\x1b[1B\x1b[K\x1b[30;1H\x1b[{TEXT_ROW};3H\x1b[?25h{SYNC_END}",
        RULE_ROW - 2,
        rule()
    )
    .into_bytes()
}

fn newline_down_bytes() -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[29;1H  \x1b[K\x1b[30;1H  auto mode on\x1b[K\x1b[29;3H\x1b[?25h{SYNC_END}"
    )
    .into_bytes()
}

#[test]
fn a_shift_enter_newline_starts_the_walk_again_in_a_bottom_anchored_box_too() {
    for bottom in [true, false] {
        let what = if bottom {
            "bottom-anchored (Claude Code)"
        } else {
            "top-anchored (the control)"
        };
        let mut h = Host::new();
        h.type_str(LINE_A);
        h.next_frame();
        let old = *h
            .ribbon()
            .cohorts()
            .iter()
            .find(|k| k.row == OLD_ROW && k.anchor_col == 2)
            .expect("the old line's run");
        assert_eq!(
            old.col1, 32,
            "{what}: the premise, thirty cells from column 2"
        );
        let t_next = old.t_at(32);
        let t = h.last_key + Duration::from_millis(KEY_MS);
        h.advance_to(t);
        h.now = t;
        h.last_key = t;
        h.glow.note_newline_break(t);
        h.glow.note_typed_glyph(t, 1, true, TypedClass::Glyph);
        if bottom {
            h.term.process(&newline_up_bytes(LINE_A));
            h.col = 3;
        } else {
            h.term.process(&newline_down_bytes());
            h.row = TEXT_ROW + 1;
            h.col = 3;
        }
        h.next_frame();
        let row = if bottom { OLD_ROW } else { OLD_ROW + 1 };
        let c = h.term.cursor();
        assert_eq!(
            (c.row, c.col),
            (row, 2),
            "{what}: the caret at the new line's inset"
        );
        h.type_str(&LINE_A[..20]);
        h.next_frame();
        h.idle(100);
        let line = h
            .ribbon()
            .cohorts()
            .iter()
            .filter(|k| k.row == row && k.anchor_col == 2 && k.born > t)
            .max_by_key(|k| k.born)
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "{what}: the new line is a run of its own: {:?}",
                    cohorts_on(&h, row)
                )
            });
        assert!(
            line.d0 == 0.0 && (line.t0 - t_next).abs() < 1e-5,
            "{what}: the new line continues the colour ({} against {t_next}) and starts its \
             walk (d0 {}): {:?}",
            line.t0,
            line.d0,
            cohorts_on(&h, row)
        );
        let ts: Vec<(u16, f32)> = (2..=18u16)
            .map(|col| {
                let t = h
                    .ribbon()
                    .field_at(row, col)
                    .unwrap_or_else(|| panic!("{what}: column {col} of the new line is laid"));
                (col, t)
            })
            .collect();
        for w in ts.windows(2) {
            let ((c0, t0), (c1, t1)) = (w[0], w[1]);
            assert!(
                (t1 - t0 - 1.0 / WALK_FAST_CELLS).abs() < 1e-4,
                "{what}: the new line's step from column {c0} to {c1} is {} — a new line walks \
                 the fast leg: {ts:?}",
                t1 - t0
            );
        }
    }
}

#[test]
fn a_shift_enter_newline_starts_the_walk_again_however_soon_the_first_key_follows() {
    for bottom in [true, false] {
        for (mode, lag_ms) in [(1u8, 0u64), (2, 20), (2, 45)] {
            let when = if mode == 1 {
                "half-way to the next frame".to_string()
            } else {
                format!("{lag_ms} ms after the chord")
            };
            let what = format!(
                "{}, mode {mode}, key {when}",
                if bottom {
                    "bottom-anchored (Claude Code)"
                } else {
                    "top-anchored (the control)"
                }
            );
            let mut h = Host::new();
            h.type_str(LINE_A);
            h.next_frame();
            let old = *h
                .ribbon()
                .cohorts()
                .iter()
                .find(|k| k.row == OLD_ROW && k.anchor_col == 2)
                .expect("the old line's run");
            assert_eq!(old.col1, 32, "{what}: the premise, thirty cells");
            let t_next = old.t_at(32);
            let t = h.last_key + Duration::from_millis(KEY_MS);
            h.advance_to(t);
            h.now = t;
            h.last_key = t;
            h.glow.note_newline_break(t);
            h.glow.note_typed_glyph(t, 1, true, TypedClass::Glyph);
            let repaint = if bottom {
                newline_up_bytes(LINE_A)
            } else {
                newline_down_bytes()
            };
            if mode == 1 {
                h.term.process(&repaint);
            }
            let k = if mode == 1 {
                t + (h.next_frame - t) / 2
            } else {
                t + Duration::from_millis(lag_ms)
            };
            h.advance_to(k);
            h.now = k;
            h.last_key = k;
            h.glow.note_typed_glyph(k, 1, false, TypedClass::Glyph);
            if mode == 2 {
                h.term.process(&repaint);
            }
            h.row = if bottom { TEXT_ROW } else { TEXT_ROW + 1 };
            h.col = 3;
            h.next_frame();
            let row = if bottom { OLD_ROW } else { OLD_ROW + 1 };
            let c = h.term.cursor();
            assert_eq!(
                (c.row, c.col),
                (row, 2),
                "{what}: the caret at the new line's inset"
            );
            h.term.process(&glyph_bytes(h.row, 'a', h.col));
            h.col += 1;
            h.next_frame();
            h.type_str(&LINE_A[1..20]);
            h.next_frame();
            h.idle(100);
            let line = h
                .ribbon()
                .cohorts()
                .iter()
                .filter(|q| q.row == row && q.anchor_col == 2 && q.born > t)
                .max_by_key(|q| q.born)
                .copied()
                .unwrap_or_else(|| {
                    panic!(
                        "{what}: the new line is a run of its own: {:?}",
                        cohorts_on(&h, row)
                    )
                });
            assert!(
                line.d0 == 0.0 && (line.t0 - t_next).abs() < 1e-5,
                "{what}: the new line continues the colour ({} against {t_next}) and starts \
                 its walk (d0 {}): {:?}",
                line.t0,
                line.d0,
                cohorts_on(&h, row)
            );
            let ts: Vec<(u16, f32)> = (2..=18u16)
                .map(|col| {
                    let t = h
                        .ribbon()
                        .field_at(row, col)
                        .unwrap_or_else(|| panic!("{what}: column {col} of the new line is laid"));
                    (col, t)
                })
                .collect();
            for w in ts.windows(2) {
                let ((c0, t0), (c1, t1)) = (w[0], w[1]);
                assert!(
                    (t1 - t0 - 1.0 / WALK_FAST_CELLS).abs() < 1e-4,
                    "{what}: the new line's step from column {c0} to {c1} is {} — a new line \
                     walks the fast leg: {ts:?}",
                    t1 - t0
                );
            }
        }
    }
}
