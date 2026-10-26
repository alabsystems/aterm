// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! New-line fade tests: when Enter clears the composer, the blanked band fades out through its
//! retract swoosh (`RETRACT_DUR_S + RETRACT_FADE_S`) rather than the 120 ms replaced-text melt
//! (`RETIRE_MELT_S`).

use crate::common::{
    caret_sample, note_blink, observe_caret_row, observe_ribbon_rows, rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::ribbon::{
    RETIRE_MELT_S, RETRACT_DUR_S, RETRACT_FADE_S, SWOOSH_TOTAL_S,
};
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
    /// The clock of the last typed key, for the grace-window guards.
    last_key: Instant,
}

impl Host {
    fn at_row(row: u16) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(format!("\x1b[{};1H", row + 1).as_bytes());
        let now = Instant::now();
        let mut h = Self {
            term,
            glow: crate::common::quads_glow(),
            cfg: rainbow_cfg(),
            g: crate::common::geom(ROWS, COLS, CW, CH),
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            last_key: now,
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
    }

    fn idle(&mut self, ms: u64) {
        let end = self.now + Duration::from_millis(ms);
        while self.now < end {
            self.now += Duration::from_millis(16);
            self.frame();
        }
    }

    fn key(&mut self, bytes: &[u8], cells: u16) {
        self.now += Duration::from_millis(90);
        self.last_key = self.now;
        self.glow.note_typed_cells(self.now, cells);
        self.term.process(bytes);
        self.frame();
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            let mut buf = [0u8; 4];
            self.key(ch.encode_utf8(&mut buf).as_bytes(), 1);
        }
    }

    fn ret(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(90);
        self.glow.note_return(self.now);
        self.term.process(bytes);
        self.frame();
    }

    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

    fn cells(&self, row: u16) -> Vec<(u16, bool)> {
        let mut v: Vec<(u16, bool)> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
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

    fn leaving(&self, row: u16) -> Vec<u16> {
        self.cells(row)
            .into_iter()
            .filter(|&(_, leaving)| leaving)
            .map(|(col, _)| col)
            .collect()
    }

    fn cohorts(&self, row: u16) -> Vec<(bool, bool)> {
        self.glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cohorts()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| (c.abandoned, c.phase.is_retracting()))
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

    fn census(&mut self, row: u16, ms: u64, step: u64) -> Vec<Sample> {
        let start = self.now;
        let mut out = vec![Sample {
            t: 0.0,
            cov: self.coverage(row),
            resident: self.cells(row).len(),
        }];
        let end = self.now + Duration::from_millis(ms);
        while self.now < end {
            self.now += Duration::from_millis(step);
            self.frame();
            out.push(Sample {
                t: self.now.saturating_duration_since(start).as_secs_f32(),
                cov: self.coverage(row),
                resident: self.cells(row).len(),
            });
        }
        out
    }

    fn retired(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.retired)
    }

    fn followed(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.followed)
    }

    fn since_key(&self) -> f32 {
        self.now
            .saturating_duration_since(self.last_key)
            .as_secs_f32()
    }
}

/// Elapsed seconds, row coverage, and resident cell count for one frame.
#[derive(Clone, Copy, Debug)]
struct Sample {
    t: f32,
    cov: f32,
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

fn is_monotone(curve: &[Sample]) -> bool {
    max_rise(curve) <= RISE_TOL
}

fn fmt(curve: &[Sample]) -> String {
    curve
        .iter()
        .step_by(4)
        .map(|s| format!("{:.2}:{:.3}/{}", s.t, s.cov, s.resident))
        .collect::<Vec<_>>()
        .join(" ")
}

const GLASS_DARK_FLOOR_S: f32 = 0.5;
const POOL_EMPTY_FLOOR_S: f32 = 0.6;
const RETRACT_SPAN_S: f32 = RETRACT_DUR_S + RETRACT_FADE_S + 0.04;

const GRACE_S: f32 = 0.75;
const TEXT: &str = "hello world";

fn composed(row: u16) -> Host {
    let mut h = Host::at_row(row);
    h.program(b"> ");
    h.type_str(TEXT);
    let want: Vec<u16> = (2..2 + TEXT.len() as u16).collect();
    assert_eq!(h.live(row), want, "the typed band is laid under the text");
    assert!(h.lit(row));
    h
}

fn hello(row: u16) -> Host {
    let mut h = Host::at_row(row);
    h.type_str(TEXT);
    let want: Vec<u16> = (0..TEXT.len() as u16).collect();
    assert_eq!(h.live(row), want, "the typed band is laid under the text");
    assert!(h.lit(row));
    h
}

const SUBMIT_SAME_ROW: &[u8] = b"\x1b[?25l\x1b[6;1H\x1b[2K> \x1b[6;3H\x1b[?25h";

const SUBMIT_TWO_ROWS_DOWN: &[u8] = b"\x1b[?25l\x1b[6;1H\x1b[2K\x1b[8;1H> \x1b[8;3H\x1b[?25h";

fn shell_enter_control() -> Vec<Sample> {
    let mut c = composed(5);
    c.ret(b"\r\n> ");
    let cur = c.term.cursor();
    assert_eq!((cur.row, cur.col), (6, 2), "the shell opened the next row");
    assert!(c.leaving(5).is_empty(), "a shell's Enter melts nothing");
    c.census(5, 1200, 16)
}

#[test]
fn a_submit_that_clears_the_composer_lets_the_band_leave_through_its_own_swoosh_not_a_cut() {
    let control = shell_enter_control();
    let control_dark = dark_at(&control).expect("the control goes out");

    let mut h = composed(5);
    let peak = h.coverage(5);
    assert!(peak > 0.0);
    h.ret(SUBMIT_SAME_ROW);
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (5, 2),
        "the composer was cleared and re-prompted"
    );
    assert!(
        h.leaving(5).is_empty(),
        "no cell of the blanked band took the fast melt: {:?}",
        h.cells(5)
    );
    let curve = h.census(5, 1200, 16);
    assert_eq!(
        h.cohorts(5),
        vec![],
        "…and the row is out of the pool at the end of the census"
    );
    let dark = dark_at(&curve).expect("the band goes out");
    let empty = empty_at(&curve).expect("the cells leave the pool");
    eprintln!(
        "submit:  dark +{dark:.3} s, empty +{empty:.3} s, worst step {:.4}, max rise {:.5}\n  {}",
        worst_step(&curve),
        max_rise(&curve),
        fmt(&curve)
    );
    eprintln!(
        "control: dark +{control_dark:.3} s, empty +{:.3} s, worst step {:.4}, max rise {:.5}\n  {}",
        empty_at(&control).unwrap_or(f32::NAN),
        worst_step(&control),
        max_rise(&control),
        fmt(&control)
    );
    assert!(
        empty >= POOL_EMPTY_FLOOR_S,
        "the cells live the retract's own span — on main they were out of the pool at \
         +{empty:.2} s (the {RETIRE_MELT_S} s melt for stale light over replaced text): {}",
        fmt(&curve)
    );
    assert!(
        dark >= GLASS_DARK_FLOOR_S,
        "the glass decays over at least {GLASS_DARK_FLOOR_S} s — main cut it at +{dark:.2} s: {}",
        fmt(&curve)
    );
    assert!(
        dark <= RETRACT_SPAN_S && empty <= RETRACT_SPAN_S,
        "…and through the swoosh's own retract, not a lingering natural expiry: \
         dark +{dark:.2} s, empty +{empty:.2} s"
    );
    assert!(
        is_monotone(&curve),
        "never brightens past the census's quantisation: rose {:.5} (control {:.5}): {}",
        max_rise(&curve),
        max_rise(&control),
        fmt(&curve)
    );
    assert!(
        worst_step(&curve) <= worst_step(&control) * 1.05 + 1e-3,
        "no frame-to-frame drop larger than the shell Enter's own worst step: \
         {:.4} against {:.4}\nsubmit  {}\ncontrol {}",
        worst_step(&curve),
        worst_step(&control),
        fmt(&curve),
        fmt(&control)
    );
    assert!(
        (dark - control_dark).abs() <= 0.05,
        "the same exit as the shell's Enter: dark at +{dark:.2} s against +{control_dark:.2} s"
    );
}

#[test]
fn a_submit_that_moves_the_cleared_composer_down_fades_the_old_row_the_same_way() {
    let control = shell_enter_control();
    let mut h = composed(5);
    h.ret(SUBMIT_TWO_ROWS_DOWN);
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (7, 2),
        "the empty composer moved two rows down"
    );
    assert!(
        h.leaving(5).is_empty(),
        "no cell took the fast melt: {:?}",
        h.cells(5)
    );
    assert!(!h.lit(7), "the new composer row holds no typed text: dark");
    let curve = h.census(5, 1200, 16);
    let dark = dark_at(&curve).expect("the band goes out");
    let empty = empty_at(&curve).expect("the cells leave the pool");
    eprintln!(
        "two rows down: dark +{dark:.3} s, empty +{empty:.3} s, worst step {:.4}, max rise {:.5}",
        worst_step(&curve),
        max_rise(&curve)
    );
    assert!(
        empty >= POOL_EMPTY_FLOOR_S && dark >= GLASS_DARK_FLOOR_S,
        "the retract's own span — main cut it: dark +{dark:.2} s, empty +{empty:.2} s: {}",
        fmt(&curve)
    );
    assert!(
        dark <= RETRACT_SPAN_S && empty <= RETRACT_SPAN_S,
        "+{dark:.2} s / +{empty:.2} s"
    );
    assert!(
        is_monotone(&curve),
        "never brightens past the census's quantisation: rose {:.5} (control {:.5}): {}",
        max_rise(&curve),
        max_rise(&control),
        fmt(&curve)
    );
    assert!(
        worst_step(&curve) <= worst_step(&control) * 1.05 + 1e-3,
        "{:.4} against the shell's {:.4}",
        worst_step(&curve),
        worst_step(&control)
    );
    assert!(!h.lit(7), "…and nothing was ever lit on the new row");
}

#[test]
fn a_submit_split_across_a_hidden_frame_still_leaves_through_the_swoosh() {
    let mut h = composed(5);
    h.ret(b"\x1b[?25l\x1b[6;1H\x1b[2K");
    assert!(
        h.leaving(5).is_empty(),
        "no cell took the fast melt on the hidden frame: {:?}",
        h.cells(5)
    );
    h.program(b"\x1b[8;1H> \x1b[8;3H\x1b[?25h");
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (7, 2));
    assert!(
        h.leaving(5).is_empty(),
        "no cell took the fast melt on the reappearance: {:?}",
        h.cells(5)
    );
    let curve = h.census(5, 1200, 16);
    let dark = dark_at(&curve).expect("the band goes out");
    let empty = empty_at(&curve).expect("the cells leave the pool");
    eprintln!(
        "split across a hidden frame: dark +{dark:.3} s, empty +{empty:.3} s, worst step {:.4}, max rise {:.5}",
        worst_step(&curve),
        max_rise(&curve)
    );
    assert!(
        dark >= GLASS_DARK_FLOOR_S - 0.016 && empty >= POOL_EMPTY_FLOOR_S - 0.016,
        "+{dark:.2} s / +{empty:.2} s: {}",
        fmt(&curve)
    );
    assert!(
        dark <= RETRACT_SPAN_S && empty <= RETRACT_SPAN_S,
        "+{dark:.2} s / +{empty:.2} s"
    );
    assert!(
        is_monotone(&curve),
        "never brightens past the census's quantisation: rose {:.5}: {}",
        max_rise(&curve),
        fmt(&curve)
    );
}

#[test]
fn a_shell_s_enter_leaves_the_typed_row_s_band_to_its_own_swoosh() {
    let mut h = hello(5);
    h.ret(b"\r\n$ ");
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (6, 2));
    assert!(h.leaving(5).is_empty(), "nothing is stamped");
    assert_eq!(h.retired(), 0, "nothing was retired by content");
    let curve = h.census(5, 1200, 16);
    let dark = dark_at(&curve).expect("the band goes out");
    let empty = empty_at(&curve).expect("the cells leave the pool");
    assert!(
        (GLASS_DARK_FLOOR_S..=RETRACT_SPAN_S).contains(&dark)
            && (POOL_EMPTY_FLOOR_S..=RETRACT_SPAN_S).contains(&empty),
        "the retract's own span: dark +{dark:.2} s, empty +{empty:.2} s: {}",
        fmt(&curve)
    );
    assert!(
        is_monotone(&curve),
        "never brightens past the census's quantisation: rose {:.5}: {}",
        max_rise(&curve),
        fmt(&curve)
    );
    assert_eq!(h.retired(), 0);
}

#[test]
fn a_declined_hidden_relocation_with_the_old_row_s_text_intact_moves_only_the_mirror() {
    let mut h = hello(5);
    let want: Vec<u16> = (0..TEXT.len() as u16).collect();
    h.program(b"\x1b[?25l");
    h.program(b"\x1b[10;1H\x1b[?25h");
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (9, 0),
        "the caret reappeared four rows down"
    );
    assert!(
        h.leaving(5).is_empty(),
        "no cell of the intact row is stamped: {:?}",
        h.cells(5)
    );
    assert_eq!(h.live(5), want, "the band stands under its text");
    assert_eq!(h.retired(), 0, "nothing was retired by content");
    assert!(h.lit(5));
    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    assert_eq!(
        h.live(5),
        want,
        "…and still stands 200 ms on, inside its grace"
    );
    assert!(h.lit(5));
    h.key(b"x", 1);
    assert_eq!(h.live(9), vec![0], "the key laid its own cell on row 9");
    assert_eq!(h.cells(5).len(), TEXT.len(), "…and nothing new on row 5");
    assert!(h.lit(9), "the hand's row is lit");
    h.idle((SWOOSH_TOTAL_S * 1000.0) as u64 + 100);
    assert!(
        h.cells(5).is_empty(),
        "row 5 went out through its own swoosh"
    );
    assert!(!h.lit(5));
}

#[test]
fn a_declined_hidden_relocation_whose_text_went_with_it_carries_the_band_with_it() {
    let mut h = hello(5);
    h.program(b"\x1b[?25l");
    h.program(b"\x1b[2J\x1b[8;1Hhello world\x1b[?25h");
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (7, 11), "the caret went with the text");
    let want: Vec<u16> = (0..TEXT.len() as u16).collect();
    assert!(
        h.leaving(5).is_empty() && h.cells(5).is_empty(),
        "the band left row 5 with its text, unstamped: {:?}",
        h.cells(5)
    );
    assert_eq!(h.live(7), want, "…and stands under it on row 7");
    assert_eq!(h.retired(), 0, "nothing was retired by content");
    assert_eq!(
        h.followed(),
        TEXT.len() as u64,
        "every cell of the band followed its text"
    );
    assert!(h.lit(7), "row 7 is lit under the text the hand typed");
    assert!(!h.lit(5));
    h.idle(200);
    assert!(
        h.since_key() < GRACE_S,
        "inside the grace: the swoosh could not have taken it"
    );
    assert_eq!(h.live(7), want, "…and still stands 200 ms on");
    assert!(h.cells(5).is_empty());
    assert!(h.lit(7));
}

const SPINNER: &[u8] = b"* Thinking about it (esc to interrupt)";

#[test]
fn a_released_band_that_different_text_lands_under_it_a_frame_later_melts_fast() {
    let want: Vec<u16> = (2..2 + TEXT.len() as u16).collect();
    let mut h = composed(5);
    h.ret(b"\x1b[?25l\x1b[6;1H\x1b[2K");
    assert!(
        h.leaving(5).is_empty(),
        "released on the hidden frame, unstamped: {:?}",
        h.cells(5)
    );
    assert_eq!(h.retired(), TEXT.len() as u64, "counted when released");
    h.program(&[b"\x1b[6;1H", SPINNER, b"\x1b[8;1H> \x1b[8;3H\x1b[?25h"].concat());
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (7, 2), "the empty composer two rows down");
    assert_eq!(
        h.leaving(5),
        want,
        "every cell of the released band is stamped on the frame the spinner landed: {:?}",
        h.cells(5)
    );
    assert_eq!(
        h.retired(),
        TEXT.len() as u64,
        "…and not counted again: the melt only changed their clock"
    );
    let curve = h.census(5, 400, 16);
    let dark = dark_at(&curve).expect("the band goes out");
    let empty = empty_at(&curve).expect("the cells leave the pool");
    eprintln!(
        "spinner under a released band: dark +{dark:.3} s, empty +{empty:.3} s, worst step {:.4}, max rise {:.5}\n  {}",
        worst_step(&curve),
        max_rise(&curve),
        fmt(&curve)
    );
    assert!(
        dark <= 0.15 && empty <= 0.15,
        "light under the wrong letters goes inside 150 ms: dark +{dark:.2} s, empty +{empty:.2} s: {}",
        fmt(&curve)
    );
    assert!(
        is_monotone(&curve),
        "the melt multiplies into the retract, never brightens: rose {:.5}: {}",
        max_rise(&curve),
        fmt(&curve)
    );
    assert!(!h.lit(7), "row 7 holds no typed text: dark");
}

const SUBMIT_GLYPH_ON_THE_SPACE: &[u8] = b"\x1b[?25l\x1b[6;1H\x1b[2K> \x1b[6;5H*\x1b[6;3H\x1b[?25h";

fn hole_frames(h: &mut Host, row: u16, ms: u64) -> Vec<f32> {
    let holed = |h: &Host| {
        let mut lit = vec![false; COLS * CW];
        for q in h.row_quads(row) {
            let x0 = usize::from(q.x).min(lit.len());
            let x1 = (usize::from(q.x) + usize::from(q.w)).min(lit.len());
            for px in &mut lit[x0..x1] {
                *px = true;
            }
        }
        let first = lit.iter().position(|&l| l);
        let last = lit.iter().rposition(|&l| l);
        matches!((first, last), (Some(a), Some(b)) if lit[a..=b].iter().any(|&l| !l))
    };
    let start = h.now;
    let mut out = Vec::new();
    let end = h.now + Duration::from_millis(ms);
    loop {
        if holed(h) {
            out.push(h.now.saturating_duration_since(start).as_secs_f32());
        }
        if h.now >= end {
            return out;
        }
        h.now += Duration::from_millis(16);
        h.frame();
    }
}

#[test]
fn a_glyph_the_clear_paints_on_a_typed_space_leaves_with_the_swoosh_not_ahead_of_it() {
    for (submit, what) in [
        (SUBMIT_SAME_ROW, "the plain submit (the control)"),
        (SUBMIT_GLYPH_ON_THE_SPACE, "a glyph on the Space's column"),
    ] {
        let mut h = Host::at_row(5);
        h.program(b"> ");
        h.type_str("ab cd");
        assert_eq!(h.live(5), vec![2, 3, 4, 5, 6], "{what}: the premise");
        h.ret(submit);
        let cells = h.cells(5);
        let holes = hole_frames(&mut h, 5, 800);
        assert!(
            holes.is_empty(),
            "{what}: the draining band is punched through on {} frames: {holes:?}",
            holes.len()
        );
        assert!(
            cells.iter().all(|&(_, leaving)| !leaving),
            "{what}: no cell of the released band is stamped for the melt: {cells:?}"
        );
    }
}
