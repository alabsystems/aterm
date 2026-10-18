// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The walk a jump reads is the walk the band shows. Two laws at the host
//! seam (`docs/design/RAINBOW-KITTY-V2.md` §35, §41, §42), each with a control:
//!
//! 1. **A jump's landing marks wear the band.** `Ribbon::walk_origin_at`
//!    (→ `Ctx::caret_walk` → `meteor::LandingWalk`: the burst, jets and fan)
//!    reads the newest pool cell at the landing and answers with its cohort's
//!    walk; the field index (→ `Ctx::caret_t` → the meteor's `t_land`: pin and
//!    train) takes `Ribbon::build`'s owner (a born `Over` cell, else the newest
//!    `Base`, an unborn `Over` last). On a jump's own tick the corridor's
//!    landing cell is an unborn `Over`, so the two name different cells; that
//!    is harmless only while both are on one walk. One row holds one walk
//!    (below), and the shift pass re-reads each pushed cell's stop at its new
//!    column after a mid-line insert (`Ribbon::shift_run`), so a ⌥F onto the
//!    pushed tail lands on the band.
//!    [`a_jump_s_landing_marks_wear_the_band_they_land_on`].
//! 2. **A wake keeps the colours of the band it covers.** `Ribbon::wake_origin`
//!    gives the corridor the freshest cohort's `(anchor_col, t0, d0)`. A row can
//!    hold two runs when a word no key laid sits between them (a yank, Alt-. /
//!    Ctrl-Y, whose echo no key licenses); `Ribbon::join_cohort` mints the
//!    second run beside a live run on that run's walk by position,
//!    `walk_at(col)` (one row, one walk), so a Ctrl-A across the line or a ⌥B
//!    back onto the yanked word stays on the row's one walk. The price: ` /tmp`
//!    after the yank starts at `cp `'s walk at column 22, `walk_t(20)`, not one
//!    step past `cp `'s end. [`a_wake_keeps_the_colours_of_the_band_it_covers`].
//!
//! The host seam is `tests/midline_insert_band.rs`'s: a real
//! `aterm_core::terminal::Terminal` driven byte for byte, the frame hold's rows
//! fed to `CursorGlow`, ticked on a 16 ms train, keys stamped as the host
//! stamps them (`note_typed_glyph`; a navigation key `clear_typed_keeping_hand`
//! then `note_motion`; a yank nothing, since `app_input.rs` stamps no hint for a
//! chord that is not a navigation, typed, Tab, paste or kill key). The landing
//! is read as `Engine::tick` reads it on the jump's own tick
//! (`field_at(landing)` and `walk_origin_at(landing)` after the tick that
//! replayed the move), and the band as the owner the geometry draws from,
//! folded through `meteor::tri` as the ribbon and landing fold it.

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
/// 12 cps, the owner's pace.
const KEY_MS: u64 = 83;
/// A word motion's cadence.
const HOP_MS: u64 = 150;
const FRAME_MS: u64 = 16;
/// Two readings of one colour agree to this (folded arc units).
const SAME: f32 = 1e-4;
/// The widest step the walk ever takes between neighbouring cells: one
/// cell of the fast leg, `1/16`.
const WALK_STEP_MAX: f32 = 1.0 / WALK_FAST_CELLS;

fn geom() -> Geom {
    crate::common::geom(ROWS, COLS, CW, CH)
}

/// How the program repaints its line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// A shell at a visible caret (readline): the edit column on reprinted,
    /// then `CUB` back to the caret; a motion is a bare `CUB`/`CUF`.
    Shell,
    /// Claude Code's bottom-pinned composer: the diff frame inside a DEC 2026
    /// bracket with the caret hidden, a `CUP` to the hint row, the caret's
    /// `CUP`, shown.
    Claude,
}

/// The landing as `Engine::tick` read it on the jump's own tick, and the
/// band under it once the flight is over.
#[derive(Clone, Debug)]
struct Landing {
    col: u16,
    /// `Ctx::caret_t` for the move: the field index at the landing.
    t_land: f32,
    /// `Ctx::caret_walk` for the move: `walk_origin_at(landing)`.
    band: Option<(f32, f32)>,
    /// The cell the field index takes at the landing: `(cohort, layer,
    /// born yet)`.
    field_owner: Option<(u32, Layer, bool)>,
    /// The cell `walk_origin_at` takes: the newest in pool order.
    walk_owner: Option<(u32, Layer, bool)>,
    /// Meteors live after the jump's tick.
    meteors: u32,
    /// Once every corridor cell is born: `(dx, the band's folded t)` for
    /// every LIT column `dx` cells right of the landing, `|dx| <= 3`.
    drawn: Vec<(i32, f32)>,
}

impl Landing {
    fn walk(&self) -> LandingWalk {
        LandingWalk {
            t_land: self.t_land,
            band: self.band,
        }
    }

    /// Every landing mark against the band under it: pin and train at
    /// `tri(t_land)`, burst, jets and fan at `LandingWalk::at(dx)`, as
    /// `(mark, dx, mark's colour, band's colour)` for each differing by more
    /// than [`SAME`].
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

/// One frame's census of the composer row: `(col, folded t)` of every lit
/// column.
#[derive(Clone, Debug)]
struct Frame {
    ms: u64,
    lit: Vec<(u16, f32)>,
}

impl Frame {
    /// The widest colour step between two LIT neighbouring columns:
    /// `(left col, left t, right t)`.
    fn widest_step(&self) -> Option<(u16, f32, f32)> {
        self.lit
            .windows(2)
            .filter(|w| w[1].0 == w[0].0 + 1)
            .map(|w| (w[0].0, w[0].1, w[1].1))
            .max_by(|a, b| (a.2 - a.1).abs().total_cmp(&(b.2 - b.1).abs()))
    }
}

/// `Ribbon::build`'s owner of `(row, col)` at `now`: a born `Over` cell first,
/// then the newest `Base`, an unborn `Over` last; newest first within a rank.
/// Its `t` is `field_at(row, col)`.
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
    /// The index in [`Host::frames`] of the last motion's own frame — the
    /// tick that replayed the move.
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

    /// The render path: the frame hold's caret row and every ribbon row, the
    /// print anchor fed just before the tick, the tick, then the census.
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

    /// The composer row's lit columns and the colour the band draws there,
    /// folded: a column is lit at [`LIT_COV`] of planned coverage with a born
    /// owner ([`owner_at`]); the feathered column past a run's end, or under an
    /// unborn corridor cell, is its neighbour's light and carries no colour.
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

    /// The program's repaint after its text went from `before` to
    /// `self.text`, the caret at `self.caret`.
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
                // Readline writes from the terminal's cursor (the insert point)
                // to the line's end, so a key typed in front of its own letter
                // reaches the screen.
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

    /// One typed key `ms` after the last event, stamped as the host stamps
    /// it.
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

    /// A navigation key landing the caret at text column `col`, `ms` after the
    /// last event, stamped as the host stamps one (`clear_typed_keeping_hand`,
    /// then `note_motion`). Returns the landing as the jump's tick read it.
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

    /// The PROGRAM writes `s` at the caret and leaves the caret after it — no
    /// key behind it (a shell at a visible caret; `Shape::Shell` only).
    fn program_writes(&mut self, s: &str, ms: u64) {
        assert_eq!(self.shape, Shape::Shell);
        self.schedule(ms);
        self.term.process(s.as_bytes());
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
        self.frame();
    }

    /// The PROGRAM writes `s` after the caret and parks the caret back where
    /// it was — a suggestion shown ahead of the hand (`Shape::Shell` only).
    fn program_suggests(&mut self, s: &str, ms: u64) {
        assert_eq!(self.shape, Shape::Shell);
        self.schedule(ms);
        self.term
            .process(format!("{s}\x1b[{}D", s.len()).as_bytes());
        self.text.insert_str(self.caret, s);
        self.frame();
    }

    /// The landing as `Engine::tick` read it on the tick that replayed the move
    /// (`move_ctx.caret_t = field_at(to)`, `move_ctx.caret_walk =
    /// walk_origin_at(to)`); then the frames to the end of the flight, and the
    /// band under the landing's marks.
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
        // The flight: every corridor cell is born by `FLIGHT_MAX_MS`.
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

// ---------------------------------------------------------------------------
// The takes
// ---------------------------------------------------------------------------

/// How the middle of the line reached the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Middle {
    /// The take: the shell yanks the word (Alt-. `insert-last-word`, or Ctrl-Y),
    /// writing it and leaving the caret after it. Neither chord is a
    /// navigation, typed, Tab or paste key, so no key licenses the echo and
    /// nothing is laid under it; the next key mints a second run on the row on
    /// the first run's walk at its own column.
    Yanked,
    /// CONTROL: every glyph typed — one run.
    Typed,
    /// Control: the program suggests the word ahead of the caret (a zsh
    /// autosuggestion) and the hand takes it with Ctrl-E, a keyed jump whose
    /// corridor bridges the word on the first run's walk and which the next key
    /// joins.
    Suggested,
}

/// `cp `, the middle ([`Middle`]) and ` /tmp` after it: the hand's two
/// runs with the word between them. The caret ends the line.
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

/// The word between the runs: one readline word, longer than
/// `JUMP_MIN_CELLS`, so a ⌥B onto it from past it is a jump.
const WORD: &str = "rainbowkittytrail";

/// [`cp_line`], a rest into the swoosh (so the jump is a jump, not a scrub),
/// then Ctrl-A across the line. Returns the host, the landing and the jump's
/// frame.
fn cp_then_home(middle: Middle) -> (Host, Landing, usize) {
    let mut h = cp_line(middle);
    let landing = h.nav(0, 1100);
    let jump_at = h.jump_at;
    (h, landing, jump_at)
}

/// A mid-line edit (`tests/midline_insert_band.rs`) in Claude Code's composer:
/// the line typed, ⌥B ×4 back to `documentation`, a rest, `ins` typed there
/// (each key an insert pushing the tail on). Empty `ins` is the control.
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

// ---------------------------------------------------------------------------
// (1) The landing's walk and the band under it
// ---------------------------------------------------------------------------

/// (1) Every mark of a jump's landing wears the band it lands on. Read once the
/// flight is over, pin and train (`tri(t_land)`) are the band's colour at the
/// landing, and burst, jets and fan (`LandingWalk::at(dx)`) the band's colour
/// `dx` cells along, wherever the band is lit within three cells.
///
/// * Ctrl-A across a yanked word ([`cp_then_home`]): on the jump's tick the
///   field index takes the first run's `Base` under the unborn corridor cell
///   while `walk_origin_at` takes the corridor's cohort; with one walk per row
///   they agree. The typed control makes the same two picks on one walk.
/// * ⌥F over a word an insert pushed ([`midline`]): a scrub, both readers take
///   the same `Base` cell, and the pushed cells sit on their cohort's walk, so
///   burst, jets and fan match the band at every column.
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

// ---------------------------------------------------------------------------
// (2) The corridor over a band
// ---------------------------------------------------------------------------

/// What a corridor did to the light it covered, from the frame before the jump
/// to the last frame read: the widest recolour of a column lit before and after
/// `(col, before, after)`, and the widest step between lit neighbours on any
/// frame from the jump on `(frame ms, left col, left t, right t)`.
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

/// (2) A wake keeps the colours of the band it covers (`Ribbon::wake`: the wake
/// shares the band's walk origin, so an overlay over a draining band reads as
/// one corridor). From the jump to the last frame read, no column lit before
/// and after changes colour, and no two lit neighbours step wider than the
/// walk's widest step (`1/16`).
///
/// On the yanked line ([`cp_line`]):
///
/// * Ctrl-A across the line ([`cp_then_home`]) must not repaint `cp ` on
///   another walk;
/// * ⌥B ⌥B back onto the word (a hop to `tmp`, then a jump across ` /`) lands
///   the corridor beside `cp `'s last cell on the same walk.
///
/// Both controls lay every corridor on the row's one walk.
#[test]
fn a_wake_keeps_the_colours_of_the_band_it_covers() {
    let mut fails = Vec::new();
    for gesture in ["Ctrl-A", "⌥B ⌥B"] {
        for middle in [Middle::Typed, Middle::Suggested, Middle::Yanked] {
            let (h, l, jump_at) = if gesture == "Ctrl-A" {
                cp_then_home(middle)
            } else {
                // Straight back onto the word to edit it: the first ⌥B a hop
                // to `tmp`, the second a jump across ` /` onto the word.
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
