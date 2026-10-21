// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A key typed inside a lit line leaves no colour seam at the tail it pushes: in Claude Code's
//! composer the rainbow stays continuous (`tests/midline_insert_band.rs` covers the band staying
//! lit).
//!
//! The shift pass (`Ribbon::shift_run`) re-reads each pushed cell's stop at the column it moved
//! to and leaves the walk's anchor where it is: the colour stays with the screen column and the
//! light rides the glyph. Carrying the pushed cells' `t` instead would restart the tail `k − 1`
//! stops back after `k` keys, a growing backward step at the seam.
//!
//! The census is each cell's walk position (`Cell::t`, what the plan resolves to colour) on every
//! text column of the composer row, on a settled frame after each key, beside the planned
//! segment's `t` at the column (what the quads draw). The law: along one phrase the walk position
//! never steps backward between neighbouring lit glyphs and never forward by more than one walk
//! step (`1/16`, the fast leg's pace). The frame before the first inserted key satisfies it, so
//! the metric reads a real walk.

use crate::common::{
    LIT_COV, caret_sample, class_of, geom, note_blink, observe_caret_row, observe_ribbon_rows,
    rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, GlowConfig};
use aterm_effects::rainbow_kitty::meteor::tri;
use aterm_effects::rainbow_kitty::ribbon::WALK_FAST_CELLS;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COLS: usize = 140;
/// 12 cps.
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
/// The Ink-shaped composer's pause between a redraw's two reads.
const INK_SPLIT_MS: u64 = 30;
/// A settled frame: this long after a mid key's echo, inside the key gap.
const SETTLE_MS: u64 = 70;
/// Float slack on a walk step (every stop is a multiple of `1/144`).
const EPS: f32 = 1e-4;

/// The line the hand types first.
const LINE: &str = "hello there this is a rainbow line";
/// What it types inside the line once it has walked back into it.
const MID: &str = "abcdef";

/// How the composer's repaint reaches the glass — the six shapes of
/// `tests/midline_insert_band.rs`, byte for byte but for the readline
/// shape's repaint, which writes from the insert point here
/// ([`Host::repaint`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// The Ink-shaped redraw: hide, home, clear, the whole line | present |
    /// 30 ms | the caret placed and shown | present.
    InkSplit,
    /// The same bytes in one read, one present.
    InkWhole,
    /// The hide alone in the first read | present | 30 ms | the rest.
    InkHideFirst,
    /// Claude Code's diff frame: `?2026h ?25l`, the changed glyphs on the
    /// composer row, a CUP to the hint row, the caret's CUP, `?25h ?2026l`.
    ClaudeWhole,
    /// The same frame torn after `?2026h ?25l`.
    ClaudeTorn,
    /// readline at a visible caret: the edit column on reprinted, `CUB`.
    Visible,
}

const SHAPES: [Shape; 6] = [
    Shape::InkSplit,
    Shape::InkWhole,
    Shape::InkHideFirst,
    Shape::ClaudeWhole,
    Shape::ClaudeTorn,
    Shape::Visible,
];

/// One text column of the composer row on one frame.
#[derive(Clone, Copy, Debug)]
struct Col {
    /// Grid column.
    col: u16,
    /// The newest live (not leaving) cell's walk position, and its cohort.
    cell: Option<(f32, u32)>,
    /// The planned segment's `t` at the column (the strongest one), and its
    /// coverage.
    plan: Option<(f32, u8)>,
}

/// One settled frame's colour census of the text.
#[derive(Clone, Debug)]
struct Snap {
    label: String,
    text: String,
    caret: usize,
    cols: Vec<Col>,
    /// `(id, anchor_col, t0, d0, col0, col1)` of every cohort on the row.
    cohorts: Vec<(u32, u16, f32, f32, u16, u16)>,
    /// Every live cell on the row as `(col, t, its cohort's t_at(col))`, for C2
    /// (`ribbon.rs`, "The walk"): a resident cell carries its cohort's walk at
    /// its column.
    walk: Vec<(u16, f32, f32)>,
}

impl Snap {
    /// Walk steps between neighbouring live cells in column order, as
    /// `(left col, right col, dt)`. A column with no live cell (the plan bridges
    /// it) is stepped over, and the pair across it is judged against its width.
    fn cell_steps(&self) -> Vec<(u16, u16, f32)> {
        let live: Vec<(u16, f32)> = self
            .cols
            .iter()
            .filter_map(|c| c.cell.map(|v| (c.col, v.0)))
            .collect();
        live.windows(2)
            .map(|w| (w[0].0, w[1].0, w[1].1 - w[0].1))
            .collect()
    }

    /// The most negative cell step (`dt = 0` when none is negative).
    fn worst_back(&self) -> (u16, u16, f32) {
        self.cell_steps()
            .into_iter()
            .fold((0, 0, 0.0), |m, s| if s.2 < m.2 { s } else { m })
    }

    /// The most negative step of the plan's `t` (the drawn colour) between neighbouring text
    /// columns (`0` when none is negative).
    fn plan_worst_back(&self) -> (u16, f32) {
        self.cols
            .windows(2)
            .filter_map(|w| match (w[0].plan, w[1].plan) {
                (Some((a, _)), Some((b, _))) => Some((w[0].col, b - a)),
                _ => None,
            })
            .fold((0, 0.0), |m, s| if s.1 < m.1 { s } else { m })
    }

    /// The largest forward cell step per column crossed: `(left, right, dt / (right − left))`.
    fn worst_jump(&self) -> (u16, u16, f32) {
        self.cell_steps()
            .into_iter()
            .map(|(a, b, dt)| (a, b, dt / f32::from(b - a)))
            .fold((0, 0, 0.0), |m, s| if s.2 > m.2 { s } else { m })
    }

    /// Text columns the plan leaves dark (coverage under [`LIT_COV`]) —
    /// the lit map of `tests/midline_insert_band.rs`.
    fn unlit(&self) -> Vec<u16> {
        self.cols
            .iter()
            .filter(|c| c.plan.is_none_or(|p| p.1 < LIT_COV))
            .map(|c| c.col)
            .collect()
    }

    /// The live cells off their cohort's walk: `(col, t, t_at(col))`.
    fn off_walk(&self) -> Vec<(u16, f32, f32)> {
        self.walk
            .iter()
            .copied()
            .filter(|w| (w.1 - w.2).abs() > EPS)
            .collect()
    }

    /// The newest live cell's `t` at grid column `col`.
    fn t_at(&self, col: usize) -> Option<f32> {
        self.cols
            .iter()
            .find(|c| usize::from(c.col) == col)
            .and_then(|c| c.cell)
            .map(|v| v.0)
    }

    /// How many text columns carry a live cell of their own.
    fn live(&self) -> usize {
        self.cols.iter().filter(|c| c.cell.is_some()).count()
    }

    fn table(&self) -> String {
        let mut s = format!(
            "  [{}] caret={} text |{}|\n  cohorts (id, anchor, t0, d0, col0, col1): {:?}\n",
            self.label, self.caret, self.text, self.cohorts
        );
        let ch: Vec<char> = self.text.chars().collect();
        let steps = self.cell_steps();
        for (i, c) in self.cols.iter().enumerate() {
            let step = steps
                .iter()
                .find(|s| s.1 == c.col)
                .map_or(String::from("      "), |s| format!("{:+.4}", s.2));
            let _ = writeln!(
                s,
                "    col {:>3} '{}' cell_t={} (hue {}) coh={} plan_t={} cov={} step={}{}",
                c.col,
                ch.get(i).copied().unwrap_or(' '),
                c.cell.map_or("  --  ".into(), |v| format!("{:.4}", v.0)),
                c.cell
                    .map_or("  --  ".into(), |v| format!("{:.4}", tri(v.0))),
                c.cell.map_or("--".into(), |v| v.1.to_string()),
                c.plan.map_or("  --  ".into(), |v| format!("{:.4}", v.0)),
                c.plan.map_or("--".into(), |v| v.1.to_string()),
                step,
                if step.starts_with('-') {
                    "  <== BACKWARD"
                } else {
                    ""
                },
            );
        }
        s
    }
}

/// The host: one terminal, the kitty, one clock, the composer's state —
/// `tests/midline_insert_band.rs`'s host, reading colour instead of holes.
struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    shape: Shape,
    row: u16,
    col0: usize,
    text: String,
    caret: usize,
}

impl Host {
    fn new(shape: Shape) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        let (row, col0) = match shape {
            Shape::ClaudeWhole | Shape::ClaudeTorn => ((ROWS - 3) as u16, 4),
            _ => (2, 0),
        };
        term.process(format!("\x1b[?1049h\x1b[2J\x1b[{};1H", row + 1).as_bytes());
        if col0 > 0 {
            term.process(format!("│ > \x1b[{};{}H", row + 1, col0 + 1).as_bytes());
        }
        let mut h = Self {
            term,
            glow,
            cfg: rainbow_cfg(),
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            shape,
            row,
            col0,
            text: String::new(),
            caret: 0,
        };
        h.frame();
        h
    }

    /// The render path of `tests/midline_insert_band.rs`.
    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .observe_print_anchor(aterm_effects::host::motion_print_anchor(&self.term).0);
        let g = geom(ROWS, COLS, CW, CH);
        self.glow.tick(cur, self.now, &self.cfg, g, &mut self.out);
    }

    /// The colour census of the composer's text on the frame just drawn.
    fn snap(&self, label: String) -> Snap {
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let mut plan: Vec<Option<(f32, u8)>> = vec![None; COLS];
        for sg in rib.plan_segments() {
            let r = ((sg.spine / CH as f32) - 0.5).floor();
            let c = (sg.x / CW as f32).floor();
            if r >= 0.0 && c >= 0.0 && r as u16 == self.row && (c as usize) < COLS {
                let i = c as usize;
                if plan[i].is_none_or(|p| sg.cov > p.1) {
                    plan[i] = Some((sg.t, sg.cov));
                }
            }
        }
        let n = self.text.chars().count();
        let cols = (self.col0..self.col0 + n)
            .map(|c| {
                let col = c as u16;
                let cell = rib
                    .cells()
                    .iter()
                    .filter(|x| x.row == self.row && x.col == col && !x.leaving())
                    .max_by_key(|x| x.born)
                    .map(|x| (x.t, x.cohort));
                Col {
                    col,
                    cell,
                    plan: plan[c],
                }
            })
            .collect();
        let cohorts = rib
            .cohorts()
            .iter()
            .filter(|c| c.row == self.row)
            .map(|c| (c.id, c.anchor_col, c.t0, c.d0, c.col0, c.col1))
            .collect();
        let mut walk: Vec<(u16, f32, f32)> = rib
            .cells()
            .iter()
            .filter(|x| x.row == self.row && !x.leaving())
            .filter_map(|x| {
                rib.cohorts()
                    .iter()
                    .find(|k| k.id == x.cohort)
                    .map(|k| (x.col, x.t, k.t_at(x.col)))
            })
            .collect();
        walk.sort_by_key(|w| w.0);
        Snap {
            label,
            text: self.text.clone(),
            caret: self.caret,
            cols,
            cohorts,
            walk,
        }
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

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.idle_to(t);
    }

    /// The frames up to `ms` after the last event, then that instant.
    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = self.now.max(t);
        self.last_event = self.now;
    }

    fn cup(&self, col: usize) -> String {
        format!("\x1b[{};{}H", self.row + 1, self.col0 + col + 1)
    }

    fn ink_halves(&self, line: &str) -> (String, String) {
        (
            format!("\x1b[?25l\x1b[{};1H\x1b[2K{line}", self.row + 1),
            format!("{}\x1b[?25h", self.cup(self.caret)),
        )
    }

    /// The program's repaint after its text went from `before` to
    /// `self.text`, in this host's shape.
    fn repaint(&mut self, before: &str) {
        let first = before
            .chars()
            .zip(self.text.chars())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| before.len().min(self.text.len()));
        match self.shape {
            Shape::InkSplit => {
                let (head, tail) = self.ink_halves(&self.text);
                self.term.process(head.as_bytes());
                self.frame();
                self.now += Duration::from_millis(INK_SPLIT_MS);
                self.term.process(tail.as_bytes());
            }
            Shape::InkWhole => {
                let (head, tail) = self.ink_halves(&self.text);
                self.term.process(format!("{head}{tail}").as_bytes());
            }
            Shape::InkHideFirst => {
                let (head, tail) = self.ink_halves(&self.text);
                self.term.process(b"\x1b[?25l");
                self.frame();
                self.now += Duration::from_millis(INK_SPLIT_MS);
                self.term.process(format!("{head}{tail}").as_bytes());
            }
            Shape::ClaudeWhole | Shape::ClaudeTorn => {
                let body = format!(
                    "{}{}\x1b[K\x1b[{ROWS};1H{}\x1b[?25h\x1b[?2026l",
                    self.cup(first),
                    &self.text[first..],
                    self.cup(self.caret)
                );
                if self.shape == Shape::ClaudeTorn {
                    self.term.process(b"\x1b[?2026h\x1b[?25l");
                    self.frame();
                    self.now += Duration::from_millis(INK_SPLIT_MS);
                    self.term.process(body.as_bytes());
                } else {
                    self.term
                        .process(format!("\x1b[?2026h\x1b[?25l{body}").as_bytes());
                }
            }
            Shape::Visible => {
                // Readline writes from the terminal's cursor (the insert point)
                // to the line's end, so a key typed in front of its own letter
                // (`b` before `bow`) reaches the screen. (`midline_insert_band.rs`
                // and `licensed_insert_band.rs` keep the first-changed-column
                // repaint.)
                let at = usize::from(self.term.cursor().col).saturating_sub(self.col0);
                let mut s = self.text[at.min(first)..].to_string();
                let back = self.text.len() - self.caret;
                if back > 0 {
                    s.push_str(&format!("\x1b[{back}D"));
                }
                self.term.process(s.as_bytes());
            }
        }
        self.frame();
    }

    /// One typed key `KEY_MS` after the last event.
    fn key(&mut self, ch: char) {
        self.schedule(KEY_MS);
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        let before = self.text.clone();
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.repaint(&before);
    }

    /// One Left arrow `ms` after the last event, stamped as a navigation
    /// key (`clear_typed_keeping_hand` then `note_motion`).
    fn left(&mut self, ms: u64) {
        self.schedule(ms);
        self.glow.clear_typed_keeping_hand(self.now);
        self.glow.note_motion(self.now);
        let before = self.text.clone();
        self.caret -= 1;
        if self.shape == Shape::Visible {
            self.term.process(b"\x1b[D");
            self.frame();
        } else {
            self.repaint(&before);
        }
    }
    /// One Backspace inside the line `ms` after the last event, stamped as
    /// the host stamps it (`note_backspace_erasing`, one cell).
    fn backspace(&mut self, ms: u64) {
        self.schedule(ms);
        self.glow.note_backspace_erasing(self.now, Some(1));
        let before = self.text.clone();
        self.caret -= 1;
        self.text.remove(self.caret);
        if self.shape == Shape::Visible {
            // readline: BS, the rest of the line, a blank over the old
            // last glyph, CUB back to the caret.
            let s = format!(
                "\x08{} \x1b[{}D",
                &self.text[self.caret..],
                self.text.len() - self.caret + 1
            );
            self.term.process(s.as_bytes());
            self.frame();
        } else {
            self.repaint(&before);
        }
    }

    /// End: the caret to the line's end `ms` after the last event, stamped
    /// as a navigation key.
    fn end(&mut self, ms: u64) {
        self.schedule(ms);
        self.glow.clear_typed_keeping_hand(self.now);
        self.glow.note_motion(self.now);
        let before = self.text.clone();
        let n = self.text.len() - self.caret;
        self.caret = self.text.len();
        if self.shape == Shape::Visible {
            self.term.process(format!("\x1b[{n}C").as_bytes());
            self.frame();
        } else {
            self.repaint(&before);
        }
    }
}

/// The take of `tests/midline_insert_band.rs`: [`LINE`] at 12 cps, `back` ← at
/// `left_ms`, a 300 ms rest, then [`MID`] typed inside the line. Returns the
/// settled census before the first inserted key and after each one.
fn take(shape: Shape, back: usize, left_ms: u64) -> (Host, Snap, Vec<Snap>) {
    let mut h = Host::new(shape);
    for ch in LINE.chars() {
        h.key(ch);
    }
    for _ in 0..back {
        h.left(left_ms);
    }
    h.schedule(300);
    let before = h.snap("before the first inserted key".into());
    let mut after = Vec::new();
    for (k, ch) in MID.chars().enumerate() {
        h.key(ch);
        h.idle(SETTLE_MS);
        after.push(h.snap(format!("{} key(s) inserted", k + 1)));
    }
    (h, before, after)
}

/// The depths the three insert laws read: 30 back puts the insert at text
/// column 4 (the walk's fast leg), 15 at column 19 and 8 at column 26 (the
/// slow leg); [`LINE`]`.len()` at the line's first glyph (the C2 law only).
const BACKS: [usize; 4] = [8, 15, 30, LINE.len()];

/// The depths the walk law and the hop law read.
const HOP_BACKS: [usize; 3] = [8, 15, 30];

/// One [`take`] at 150 ms arrows: the edited text, the census before the first inserted key and
/// after each, and (at [`HOP_BACKS`]) the End hop over the pushed tail that
/// [`a_hop_over_the_pushed_tail_does_not_repaint_it`] reads (its echo frame and the four after,
/// or the panic message, so a panicking hop fails the hop law alone).
struct Shared {
    text: String,
    col0: usize,
    before: Snap,
    after: Vec<Snap>,
    hop: Option<Result<Vec<Snap>, String>>,
}

/// The take of `(shape, back)`, replayed once per process: the walk, hop and C2 laws read the
/// same takes, each a pure function of `(shape, back)` on its own terminal, engine and clock. The
/// hop continues the take's own host, so it is replayed right after it. A take that panics leaves
/// its cell empty, so every law that reads it replays it and fails on its own.
fn shared(shape: Shape, back: usize) -> &'static Shared {
    static TAKES: [OnceLock<Shared>; SHAPES.len() * BACKS.len()] =
        [const { OnceLock::new() }; SHAPES.len() * BACKS.len()];
    let s = SHAPES.iter().position(|&x| x == shape).expect("a shape");
    let b = BACKS.iter().position(|&x| x == back).expect("a depth");
    TAKES[s * BACKS.len() + b].get_or_init(|| replay_shared(shape, back))
}

/// [`take`] at 150 ms arrows and, at [`HOP_BACKS`], the End hop after it.
fn replay_shared(shape: Shape, back: usize) -> Shared {
    let (mut h, before, after) = take(shape, back, 150);
    let (text, col0) = (h.text.clone(), h.col0);
    let hop = HOP_BACKS.contains(&back).then(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            h.end(KEY_MS);
            (0..5)
                .map(|f| {
                    if f > 0 {
                        h.step();
                    }
                    h.snap(format!("End echo +{}ms", f as u64 * FRAME_MS))
                })
                .collect()
        }))
        .map_err(|panic| {
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_default()
        })
    });
    Shared {
        text,
        col0,
        before,
        after,
        hop,
    }
}

/// Every take of `shapes × backs`, in that order, replayed on the binary's threads
/// ([`crate::common::par_map`]) where no law has replayed it yet. A take that panicked raises its
/// panic only when the law reaches it, after the takes before it are judged.
fn takes_of(backs: &[usize]) -> impl Iterator<Item = (Shape, usize, &'static Shared)> {
    let keys: Vec<(Shape, usize)> = SHAPES
        .iter()
        .flat_map(|&shape| backs.iter().map(move |&back| (shape, back)))
        .collect();
    let takes = crate::common::par_map(&keys, |&(shape, back)| {
        std::panic::catch_unwind(|| shared(shape, back))
    });
    keys.into_iter().zip(takes).map(|((shape, back), take)| {
        let take = take.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (shape, back, take)
    })
}

/// One walk step at most between neighbouring glyphs: the fast leg's pace.
fn max_step() -> f32 {
    1.0 / WALK_FAST_CELLS + EPS
}

/// The law: on every shape, at depths on the walk's fast and slow legs, every
/// settled frame after a key typed inside a lit line reads one continuous walk
/// along the phrase: no live cell steps backward from its live neighbour, none
/// jumps past one walk step a column.
#[test]
fn typing_inside_a_lit_line_keeps_the_walk_continuous_at_the_pushed_tail() {
    let mut report = String::new();
    let mut failures = 0usize;
    for (shape, back, t) in takes_of(&HOP_BACKS) {
        let (before, after) = (&t.before, &t.after);
        let what = format!("{shape:?} back={back}");
        let at = LINE.len() - back;
        assert_eq!(
            t.text,
            format!("{}{MID}{}", &LINE[..at], &LINE[at..]),
            "{what}: the composer holds the edited line"
        );
        // Premise: the line before the edit is lit on every glyph, has a
        // live cell on every glyph, and walks monotone at no more than one
        // step a cell.
        assert!(
            before.unlit().is_empty() && before.live() == LINE.len(),
            "{what}: premise — the line is lit before the edit\n{}",
            before.table()
        );
        let (ba, bb, bdt) = before.worst_back();
        let (ja, jb, jdt) = before.worst_jump();
        assert!(
            bdt >= -EPS && jdt <= max_step(),
            "{what}: premise — the unedited line walks continuously \
             (worst back {bdt:+.4} at {ba}->{bb}, worst jump {jdt:+.4} at {ja}->{jb})\n{}",
            before.table()
        );
        for s in after {
            // Premise: the band covers the edited text
            // (`tests/midline_insert_band.rs`) and all but a bridged column
            // or two carry a cell, so the census is not empty.
            assert!(
                s.unlit().is_empty() && s.live() + 2 >= s.cols.len(),
                "{what}: premise — the edited text is lit, and carries cells\n{}",
                s.table()
            );
            let (ba, bb, bdt) = s.worst_back();
            let (ja, jb, jdt) = s.worst_jump();
            let (pa, pdt) = s.plan_worst_back();
            let bad = bdt < -EPS || jdt > max_step();
            let _ = writeln!(
                report,
                "{what:<24} [{}]: worst back {bdt:+.4} at col {ba}->{bb}; \
                 worst forward {jdt:+.4}/col at {ja}->{jb}; \
                 plan's worst back {pdt:+.4} at col {pa}->{}{}",
                s.label,
                pa + 1,
                if bad { "  FAIL" } else { "" }
            );
            if bad {
                failures += 1;
            }
        }
        if matches!(
            (shape, back),
            (Shape::ClaudeWhole, 15) | (Shape::ClaudeWhole, 30)
        ) {
            let _ = writeln!(report, "\n--- the census, {what} ---");
            report.push_str(&before.table());
            for k in [0usize, 2, 5] {
                report.push_str(&after[k].table());
            }
            report.push('\n');
        }
    }
    assert!(
        failures == 0,
        "{failures} settled frames read the walk discontinuous at the pushed tail:\n{report}"
    );
}

/// A hop over the pushed tail does not repaint it: after the same take, End
/// walks the caret over the tail to the line's end. The hop lays its corridor on
/// the band's own walk (`Ribbon::wake_origin`), the walk at each column, which
/// the pushed tail already shows, so no tail glyph changes colour on the End's
/// echo frame.
#[test]
fn a_hop_over_the_pushed_tail_does_not_repaint_it() {
    let mut report = String::new();
    let mut failures = 0usize;
    for (shape, back, t) in takes_of(&HOP_BACKS) {
        let what = format!("{shape:?} back={back}");
        let settled = &t.after[MID.len() - 1];
        // The tail: the glyphs right of the six keys, which stand still
        // from here on.
        let first = t.col0 + LINE.len() - back + MID.len();
        let tail: Vec<usize> = (first + 1..t.col0 + t.text.len()).collect();
        let was: Vec<Option<f32>> = tail.iter().map(|&c| settled.t_at(c)).collect();
        // Premise: the tail is lit and carries cells before the hop.
        assert!(
            was.iter().filter(|t| t.is_some()).count() + 1 >= tail.len(),
            "{what}: premise — the tail carries cells\n{}",
            settled.table()
        );
        // The End hop over the tail, its echo frame and the four after
        // (replayed with the take, [`shared`]).
        let hop = match t
            .hop
            .as_ref()
            .expect("the hop is replayed at every hop depth")
        {
            Ok(hop) => hop,
            Err(panic) => panic!("{what}: the End hop panicked: {panic}"),
        };
        let mut worst = (0usize, 0.0f32);
        let mut compared = 0usize;
        let mut shown = None;
        for s in hop {
            for (k, &c) in tail.iter().enumerate() {
                if let (Some(a), Some(b)) = (was[k], s.t_at(c)) {
                    compared += 1;
                    if (b - a).abs() > worst.1.abs() {
                        worst = (c, b - a);
                        shown.get_or_insert(s.clone());
                    }
                }
            }
        }
        // Premise: the tail was read on every frame after the hop.
        assert!(
            compared >= 5 * (tail.len() - 1),
            "{what}: premise — the tail was compared ({compared})"
        );
        let bad = worst.1.abs() > EPS;
        let _ = writeln!(
            report,
            "{what:<24}: worst repaint of a still tail glyph {:+.4} at col {}{}",
            worst.1,
            worst.0,
            if bad { "  FAIL" } else { "" }
        );
        if bad {
            failures += 1;
            if shape == Shape::ClaudeWhole && back == 15 {
                report.push_str(&settled.table());
                if let Some(s) = shown {
                    report.push_str(&s.table());
                }
            }
        }
    }
    assert!(
        failures == 0,
        "{failures} takes repainted the still, pushed tail when the caret hopped over it:\n{report}"
    );
}

/// Every cell an insert leaves is on its cohort's walk: C2 (`ribbon.rs`, "The
/// walk"), the root of both laws above. After an insert mid-line the tail's
/// cells take their new columns' stops; after an insert at the line's first
/// glyph the anchor stays put, so the keys laid there do not pile onto one
/// stop. Negative control: the frame before the insert passes the same census.
#[test]
fn every_cell_a_mid_line_insert_leaves_is_on_its_cohorts_walk() {
    let mut report = String::new();
    let mut failures = 0usize;
    for (shape, back, t) in takes_of(&BACKS) {
        let (before, after) = (&t.before, &t.after);
        let what = format!("{shape:?} back={back}");
        assert!(
            before.walk.len() >= LINE.len() && before.off_walk().is_empty(),
            "{what}: premise — the typed line is on its walk before the insert\n{:?}\n{}",
            before.off_walk(),
            before.table()
        );
        for s in after {
            assert!(
                s.walk.len() >= LINE.len(),
                "{what}: premise — the edited line carries cells\n{}",
                s.table()
            );
            let off = s.off_walk();
            if !off.is_empty() {
                failures += 1;
                let worst = off.iter().copied().fold((0u16, 0.0f32, 0.0f32), |m, w| {
                    if (w.1 - w.2).abs() > (m.1 - m.2).abs() {
                        w
                    } else {
                        m
                    }
                });
                let _ = writeln!(
                    report,
                    "{what:<24} [{}]: {} live cells off the walk; worst col {} t={:.4} walk={:.4} ({:+.4})",
                    s.label,
                    off.len(),
                    worst.0,
                    worst.1,
                    worst.2,
                    worst.1 - worst.2
                );
            }
        }
        if shape == Shape::ClaudeWhole && back == LINE.len() {
            let _ = writeln!(report, "\n--- the census, {what} ---");
            report.push_str(&after[MID.len() - 1].table());
            report.push('\n');
        }
    }
    assert!(
        failures == 0,
        "{failures} settled frames carry cells off their cohort's walk:\n{report}"
    );
}

/// A delete is not followed, by design (`Witness::shift_runs`): a mid-line
/// Backspace or kill pulls the tail left and the ribbon's erase law takes the
/// row's suffix from the caret on, so there is no colour seam to measure. After
/// one Backspace inside the lit line every cell of the pulled tail is leaving on
/// its settled frame and dark within 400 ms; later keys at the caret light only
/// themselves; the prefix keeps its walk.
#[test]
fn a_mid_line_backspace_takes_the_tail_s_light_and_leaves_no_seam() {
    // Each shape replays on its own host and clock, so the six run on the
    // binary's threads (`par_map`); the first failing shape's panic is raised.
    crate::common::par_map(&SHAPES, |&shape| {
        let what = format!("{shape:?}");
        let mut h = Host::new(shape);
        for ch in LINE.chars() {
            h.key(ch);
        }
        for _ in 0..15 {
            h.left(150);
        }
        h.schedule(300);
        let before = h.snap("before the Backspace".into());
        assert!(
            before.unlit().is_empty() && before.off_walk().is_empty(),
            "{what}: premise — the line is lit and on its walk\n{}",
            before.table()
        );
        h.backspace(KEY_MS);
        h.idle(SETTLE_MS);
        let s = h.snap("one Backspace inside the line".into());
        let caret = h.col0 + h.caret;
        let tail_live: Vec<u16> = s
            .cols
            .iter()
            .filter(|c| usize::from(c.col) >= caret && c.cell.is_some())
            .map(|c| c.col)
            .collect();
        assert!(
            tail_live.is_empty(),
            "{what}: the pulled tail is leaving (live at {tail_live:?})\n{}",
            s.table()
        );
        let (ba, bb, bdt) = s.worst_back();
        assert!(
            bdt >= -EPS && s.off_walk().is_empty(),
            "{what}: the prefix keeps its walk (back {bdt:+.4} at {ba}->{bb})\n{}",
            s.table()
        );
        h.idle(400);
        let s = h.snap("400 ms on".into());
        let lit_tail: Vec<u16> = s
            .cols
            .iter()
            // The plan feathers the band's edge one column past its last cell
            // (`tests/midline_insert_band.rs`): the band ending at the caret,
            // not light on the tail.
            .filter(|c| usize::from(c.col) > caret + 1 && c.plan.is_some_and(|p| p.1 >= LIT_COV))
            .map(|c| c.col)
            .collect();
        assert!(
            lit_tail.is_empty(),
            "{what}: the pulled tail is dark 400 ms on (lit at {lit_tail:?})\n{}",
            s.table()
        );
    });
}
