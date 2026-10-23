// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-seam tests for jump landing walk origins and wake colour continuity (`docs/design/RAINBOW-KITTY-V2.md` §35, §41, §42).

use crate::common::{
    LIT_COV, caret_sample, class_of, coverage, note_blink, observe_caret_row, observe_ribbon_rows,
    rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::meteor::{LandingWalk, tri};
use aterm_effects::rainbow_kitty::ribbon::{Cell, Layer, WALK_FAST_CELLS, walk_t};
use aterm_effects::rainbow_kitty::timing::{FLIGHT_MAX_MS, JUMP_MIN_CELLS};
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COLS: usize = 140;
/// 12 cps.
const KEY_MS: u64 = 83;
/// A word motion's cadence.
const HOP_MS: u64 = 150;
const FRAME_MS: u64 = 16;
/// Two readings of one colour agree to this (folded arc units).
const SAME: f32 = 1e-4;
/// The widest step the walk takes between neighbouring cells: one cell of the fast leg.
const WALK_STEP_MAX: f32 = 1.0 / WALK_FAST_CELLS;

fn geom() -> Geom {
    crate::common::geom(ROWS, COLS, CW, CH)
}

/// How the program repaints its line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Readline-style shell at a visible caret.
    Shell,
    /// Claude Code's bottom-pinned composer inside a DEC 2026 bracket.
    Claude,
}

/// Landing state read on the jump's tick and the band under it after the flight.
#[derive(Clone, Debug)]
struct Landing {
    col: u16,
    /// `Ctx::caret_t` for the move: the field index at the landing.
    t_land: f32,
    /// `Ctx::caret_walk` for the move: `walk_origin_at(landing)`.
    band: Option<(f32, f32)>,
    /// Cell taken by the field index at the landing (`(cohort, layer, born)`).
    field_owner: Option<(u32, Layer, bool)>,
    /// The cell `walk_origin_at` takes: the newest in pool order.
    walk_owner: Option<(u32, Layer, bool)>,
    /// Meteors live after the jump's tick.
    meteors: u32,
    /// Lit columns `(dx, folded_t)` within `|dx| <= 3` of the landing after corridor birth.
    drawn: Vec<(i32, f32)>,
}

impl Landing {
    fn walk(&self) -> LandingWalk {
        LandingWalk {
            t_land: self.t_land,
            band: self.band,
        }
    }

    /// Landing marks whose colour differs from the band under them by more than [`SAME`].
    fn off_band(&self) -> Vec<(&'static str, i32, f32, f32)> {
        let w = self.walk();
        let mut out = Vec::new();
        for &(dx, band) in &self.drawn {
            if dx == 0 && (tri(self.t_land) - band).abs() > SAME {
                out.push(("pin/train", 0, tri(self.t_land), band));
            }
            let burst = w.at(dx as f32);
            if (burst - band).abs() > SAME {
                out.push(("burst/jets", dx, burst, band));
            }
        }
        out
    }
}

/// One frame's census of the composer row (`(col, folded_t)`).
#[derive(Clone, Debug)]
struct Frame {
    ms: u64,
    lit: Vec<(u16, f32)>,
}

impl Frame {
    /// Widest colour step between two lit neighbouring columns (`(left_col, left_t, right_t)`).
    fn widest_step(&self) -> Option<(u16, f32, f32)> {
        self.lit
            .windows(2)
            .filter(|w| w[1].0 == w[0].0 + 1)
            .map(|w| (w[0].0, w[0].1, w[1].1))
            .max_by(|a, b| (a.2 - a.1).abs().total_cmp(&(b.2 - b.1).abs()))
    }
}

/// `Ribbon::build`'s owner of `(row, col)` at `now` (born `Over`, then `Base`, then unborn `Over`).
fn owner_at(cells: &[Cell], row: u16, col: u16, now: Instant) -> Option<Cell> {
    let rank = |c: &Cell| match c.layer {
        Layer::Over if now < c.born => 2u8,
        Layer::Over => 0,
        Layer::Base => 1,
    };
    cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.row == row && c.col == col)
        .min_by(|a, b| rank(a.1).cmp(&rank(b.1)).then(b.0.cmp(&a.0)))
        .map(|(_, c)| *c)
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
    shape: Shape,
    row: u16,
    col0: usize,
    text: String,
    caret: usize,
    frames: Vec<Frame>,
    /// Index in [`Host::frames`] of the last motion's own frame.
    jump_at: usize,
}

impl Host {
    fn new(shape: Shape) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        let (row, col0, prefix) = match shape {
            Shape::Claude => ((ROWS - 3) as u16, 4, "│ > "),
            Shape::Shell => (2, 2, "$ "),
        };
        term.process(format!("\x1b[?1049h\x1b[2J\x1b[{};1H{prefix}", row + 1).as_bytes());
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
            shape,
            row,
            col0,
            text: String::new(),
            caret: 0,
            frames: Vec::new(),
            jump_at: 0,
        };
        h.frame();
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// Runs one render frame and records its lit-column census.
    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .observe_print_anchor(aterm_effects::host::motion_print_anchor(&self.term).0);
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
        let lit = self.lit_now();
        self.frames.push(Frame { ms: self.ms(), lit });
    }

    /// Lit columns on the composer row and their folded band colour at [`LIT_COV`].
    fn lit_now(&self) -> Vec<(u16, f32)> {
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let cov = coverage(&self.glow, geom(), self.row);
        (0..COLS as u16)
            .filter(|&c| cov[usize::from(c)] >= LIT_COV)
            .filter_map(|c| {
                owner_at(rib.cells(), self.row, c, self.now)
                    .filter(|o| o.layer == Layer::Base || o.born <= self.now)
                    .map(|o| (c, tri(o.t)))
            })
            .collect()
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

    fn gcol(&self, col: usize) -> u16 {
        (self.col0 + col) as u16
    }

    fn cup(&self, col: usize) -> String {
        format!("\x1b[{};{}H", self.row + 1, self.col0 + col + 1)
    }

    /// Repaints `self.text` after changing from `before` with the caret at `self.caret`.
    fn repaint(&mut self, before: &str) {
        let first = before
            .chars()
            .zip(self.text.chars())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| before.len().min(self.text.len()));
        match self.shape {
            Shape::Claude => {
                let body = format!(
                    "{}{}\x1b[K\x1b[{ROWS};1H{}\x1b[?25h\x1b[?2026l",
                    self.cup(first),
                    &self.text[first..],
                    self.cup(self.caret)
                );
                self.term
                    .process(format!("\x1b[?2026h\x1b[?25l{body}").as_bytes());
            }
            Shape::Shell => {
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

    /// Types `ch` `ms` after the last event, stamped as the host stamps it.
    fn key_after(&mut self, ch: char, ms: u64) {
        self.schedule(ms);
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        let before = self.text.clone();
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.repaint(&before);
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key_after(ch, KEY_MS);
        }
    }

    /// Moves the caret to text column `col` `ms` after the last event and returns the [`Landing`].
    fn nav(&mut self, col: usize, ms: u64) -> Landing {
        self.schedule(ms);
        self.glow.clear_typed_keeping_hand(self.now);
        self.glow.note_motion(self.now);
        let from = self.caret;
        self.caret = col;
        self.jump_at = self.frames.len();
        match self.shape {
            Shape::Shell => {
                let s = if col < from {
                    format!("\x1b[{}D", from - col)
                } else {
                    format!("\x1b[{}C", col - from)
                };
                self.term.process(s.as_bytes());
                self.frame();
            }
            Shape::Claude => {
                let before = self.text.clone();
                self.repaint(&before);
            }
        }
        assert_eq!(
            usize::from(self.term.cursor().col),
            self.col0 + col,
            "the motion landed the caret"
        );
        self.read_landing(self.gcol(col))
    }

    /// Readline's ⌥B: to the start of the previous word.
    fn word_left(&mut self, ms: u64) -> Landing {
        let b = self.text.as_bytes();
        let mut i = self.caret;
        while i > 0 && b[i - 1] == b' ' {
            i -= 1;
        }
        while i > 0 && b[i - 1] != b' ' {
            i -= 1;
        }
        self.nav(i, ms)
    }

    /// Readline's ⌥F: to the end of the current or next word.
    fn word_right(&mut self, ms: u64) -> Landing {
        let b = self.text.as_bytes();
        let n = b.len();
        let mut i = self.caret;
        while i < n && b[i] == b' ' {
            i += 1;
        }
        while i < n && b[i] != b' ' {
            i += 1;
        }
        self.nav(i, ms)
    }

    /// Writes `s` at the caret without a key stamp (`Shape::Shell` only).
    fn program_writes(&mut self, s: &str, ms: u64) {
        assert_eq!(self.shape, Shape::Shell);
        self.schedule(ms);
        self.term.process(s.as_bytes());
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
        self.frame();
    }

    /// Writes `s` after the caret and moves the caret back (`Shape::Shell` only).
    fn program_suggests(&mut self, s: &str, ms: u64) {
        assert_eq!(self.shape, Shape::Shell);
        self.schedule(ms);
        self.term
            .process(format!("{s}\x1b[{}D", s.len()).as_bytes());
        self.text.insert_str(self.caret, s);
        self.frame();
    }

    /// Reads the landing on the jump's tick and advances past `FLIGHT_MAX_MS` to sample the band under it.
    fn read_landing(&mut self, col: u16) -> Landing {
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let row = self.row;
        let now = self.now;
        let describe = |c: &Cell| (c.cohort, c.layer, now >= c.born);
        let field_owner = owner_at(rib.cells(), row, col, now).map(|c| describe(&c));
        let walk_owner = rib
            .cells()
            .iter()
            .rfind(|c| c.row == row && c.col == col)
            .map(describe);
        let t_land = rib
            .field_at(row, col)
            .unwrap_or_else(|| rib.field_at_caret());
        let band = rib.walk_origin_at(row, col);
        let meteors = self.glow.v2_status().map_or(0, |s| s.meteors);
        self.idle(FLIGHT_MAX_MS as u64 + 2 * FRAME_MS);
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        assert!(
            rib.cells()
                .iter()
                .all(|c| c.row != row || c.layer != Layer::Over || c.born <= self.now),
            "the flight is over: every corridor cell is born"
        );
        let lit = self.lit_now();
        let drawn = (-3i32..=3)
            .filter_map(|dx| {
                let c = (i32::from(col) + dx) as u16;
                lit.iter().find(|&&(lc, _)| lc == c).map(|&(_, t)| (dx, t))
            })
            .collect();
        Landing {
            col,
            t_land,
            band,
            field_owner,
            walk_owner,
            meteors,
            drawn,
        }
    }

    /// The lit map of the frames from `from` on, one line each.
    fn dump(&self, from: usize) -> String {
        let mut s = String::new();
        for f in self.frames.iter().skip(from) {
            let cells: Vec<String> = f.lit.iter().map(|(c, t)| format!("{c}:{t:.3}")).collect();
            s.push_str(&format!("  +{:>5} ms {}\n", f.ms, cells.join(" ")));
        }
        s
    }
}

/// How the middle of the line reached the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Middle {
    /// Yanked word with no key stamp between two typed runs.
    Yanked,
    /// Control: every glyph typed, one run.
    Typed,
    /// Control: autosuggested word accepted with a keyed jump (`Ctrl-E`).
    Suggested,
}

/// Types `"cp "`, inserts [`WORD`] via `middle`, and types `" /tmp"`.
fn cp_line(middle: Middle) -> Host {
    let mut h = Host::new(Shape::Shell);
    h.type_str("cp ");
    match middle {
        Middle::Yanked => h.program_writes(WORD, 200),
        Middle::Typed => h.type_str(WORD),
        Middle::Suggested => {
            h.program_suggests(WORD, 200);
            let end = h.text.len();
            h.nav(end, 400);
        }
    }
    h.idle(100);
    h.type_str(" /tmp");
    h
}

/// Readline word longer than `JUMP_MIN_CELLS` between the two typed runs.
const WORD: &str = "rainbowkittytrail";

/// Runs [`cp_line`] followed by `Ctrl-A` back to column 0.
fn cp_then_home(middle: Middle) -> (Host, Landing, usize) {
    let mut h = cp_line(middle);
    let landing = h.nav(0, 1100);
    let jump_at = h.jump_at;
    (h, landing, jump_at)
}

/// Types a sentence in Claude Code's composer, hops 4 words left, and inserts `ins`.
fn midline(ins: &str) -> Host {
    let mut h = Host::new(Shape::Claude);
    h.type_str("please update the documentation for this module");
    for _ in 0..4 {
        h.word_left(HOP_MS);
    }
    assert_eq!(
        &h.text[h.caret..h.caret + 4],
        "docu",
        "⌥B ×4 reached the word"
    );
    h.schedule(300);
    h.type_str(ins);
    h
}

const INS: &str = "new ";

fn describe(l: &Landing) -> String {
    format!(
        "landing col {}: on the jump's tick the field took {:?} (t_land {:.4}) and \
         walk_origin_at took {:?} (walk {:?} = t {:.4}); at arrival the band reads \
         {:?}; marks off it (mark, dx, mark, band): {:?}",
        l.col,
        l.field_owner,
        l.t_land,
        l.walk_owner,
        l.band,
        l.band.map_or(f32::NAN, |(d, z)| z + walk_t(d)),
        l.drawn,
        l.off_band()
    )
}

/// Landing marks (`pin/train` and `burst/jets/fan`) match the band colours under them within 3 cells.
#[test]
fn a_jump_s_landing_marks_wear_the_band_they_land_on() {
    let mut fails = Vec::new();
    for middle in [Middle::Typed, Middle::Suggested, Middle::Yanked] {
        let (h, l, jump_at) = cp_then_home(middle);
        let what = format!("Ctrl-A across the line, middle {middle:?}");
        assert!(l.meteors > 0, "{what}: the jump flew a meteor");
        assert!(l.drawn.len() >= 3, "{what}: the landing is on lit band");
        println!("{what}: {}", describe(&l));
        let off = l.off_band();
        match middle {
            Middle::Yanked => {
                if !off.is_empty() {
                    fails.push(format!("{what}: {}", describe(&l)));
                }
            }
            _ => {
                if middle == Middle::Typed {
                    assert!(
                        matches!(l.field_owner, Some((_, Layer::Base, true)))
                            && matches!(l.walk_owner, Some((_, Layer::Over, false))),
                        "{what}: the premise — on the jump's tick the field takes the \
                         band's Base cell and walk_origin_at the corridor's unborn \
                         Over cell: {}",
                        describe(&l)
                    );
                }
                assert!(
                    off.is_empty(),
                    "{what}: the CONTROL's landing wears its band: {}\n{}",
                    describe(&l),
                    h.dump(jump_at - 1)
                );
            }
        }
    }
    for ins in ["", INS] {
        let mut h = midline(ins);
        let from = h.gcol(h.caret);
        let l = h.word_right(HOP_MS);
        let what = format!("⌥F over a pushed word (ins {ins:?})");
        assert!(
            l.col.abs_diff(from) >= JUMP_MIN_CELLS,
            "{what}: ⌥F is a jump ({} cells)",
            l.col.abs_diff(from)
        );
        assert!(l.meteors > 0, "{what}: the jump flew a meteor");
        assert!(l.drawn.len() >= 3, "{what}: the landing is on lit band");
        println!("{what}: {}", describe(&l));
        let off = l.off_band();
        if ins.is_empty() {
            assert!(
                off.is_empty(),
                "{what}: the CONTROL's landing wears its band: {}",
                describe(&l)
            );
        } else if !off.is_empty() {
            fails.push(format!("{what}: {}", describe(&l)));
        }
    }
    assert!(
        fails.is_empty(),
        "a landing's marks do not wear the band they land on:\n{}",
        fails.join("\n")
    );
}

/// Widest recolour and neighbour step across a jump corridor.
struct Corridor {
    recolour: Option<(u16, f32, f32)>,
    step: Option<(u64, u16, f32, f32)>,
    steady_step: Option<(u16, f32, f32)>,
}

impl Corridor {
    fn bad(&self) -> bool {
        self.recolour.is_some_and(|(_, a, b)| (b - a).abs() > SAME)
            || self
                .step
                .is_some_and(|(_, _, a, b)| (b - a).abs() > WALK_STEP_MAX + SAME)
    }
}

fn corridor(h: &Host, jump_frame: usize, lo: u16, hi: u16) -> Corridor {
    let before = &h.frames[jump_frame - 1];
    let after = h.frames.last().expect("frames");
    let recolour = before
        .lit
        .iter()
        .filter(|&&(c, _)| (lo..=hi).contains(&c))
        .filter_map(|&(c, t0)| {
            after
                .lit
                .iter()
                .find(|&&(ac, _)| ac == c)
                .map(|&(_, t1)| (c, t0, t1))
        })
        .max_by(|a, b| (a.2 - a.1).abs().total_cmp(&(b.2 - b.1).abs()));
    let step = h.frames[jump_frame..]
        .iter()
        .filter_map(|f| f.widest_step().map(|(c, a, b)| (f.ms, c, a, b)))
        .max_by(|a, b| (a.3 - a.2).abs().total_cmp(&(b.3 - b.2).abs()));
    Corridor {
        recolour,
        step,
        steady_step: before.widest_step(),
    }
}

/// `Ribbon::wake` preserves covered band colours and bounds adjacent steps by [`WALK_STEP_MAX`].
#[test]
fn a_wake_keeps_the_colours_of_the_band_it_covers() {
    let mut fails = Vec::new();
    for gesture in ["Ctrl-A", "⌥B ⌥B"] {
        for middle in [Middle::Typed, Middle::Suggested, Middle::Yanked] {
            let (h, l, jump_at) = if gesture == "Ctrl-A" {
                cp_then_home(middle)
            } else {
                let mut h = cp_line(middle);
                h.word_left(HOP_MS);
                let l = h.word_left(HOP_MS);
                let jump_at = h.jump_at;
                assert_eq!(
                    usize::from(l.col),
                    h.col0 + "cp ".len(),
                    "the second ⌥B landed on the word"
                );
                h.idle(400);
                (h, l, jump_at)
            };
            let what = format!("{gesture} across the line, middle {middle:?}");
            assert!(l.meteors > 0, "{what}: the jump flew a meteor");
            assert!(
                h.frames[jump_at - 1].lit.len() >= 8,
                "{what}: the band was lit before the jump"
            );
            let c = corridor(&h, jump_at, l.col, h.gcol(h.text.len()));
            println!(
                "{what}: recolour (col, before, after) {:?}; widest step from the jump \
                 (ms, col, left, right) {:?}; widest step before it {:?}",
                c.recolour, c.step, c.steady_step
            );
            if middle == Middle::Yanked {
                if c.bad() {
                    fails.push(format!(
                        "{what}: recolour (col, before, after) {:?}; widest step (ms, \
                         col, left, right) {:?}\n{}",
                        c.recolour,
                        c.step,
                        h.dump(jump_at - 1)
                    ));
                }
            } else {
                assert!(
                    !c.bad(),
                    "{what}: the CONTROL's corridor keeps its band: recolour {:?} step \
                     {:?}\n{}",
                    c.recolour,
                    c.step,
                    h.dump(jump_at - 1)
                );
            }
        }
    }
    assert!(
        fails.is_empty(),
        "a wake repaints the band it covers:\n{}",
        fails.join("\n")
    );
}
