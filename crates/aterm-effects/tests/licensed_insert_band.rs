// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A LICENSED INSERT IS LAID, HOWEVER ITS FRAME IS TORN** (2026-09-22).
//! The owner, on the shipped 0.89, rainbow kitty under a Claude Code
//! composer: *"There is a rainbow trail gap in 0.89. I'm not sure what is
//! causing it but it seems like some kind of back cursor movement bug."*
//! The torn read (`torn_read_gaps.rs`) and the corridor over a fading band
//! (`jump_over_fading_band.rs`) were most of it. This is the third shape the
//! reproducers found: cells the PROGRAM writes at the caret after a Tab or a
//! paste — a completion, a pasted word — laid by nobody, and the hand's next
//! key past them minting a second band with a dark gap between.
//!
//! The take: the hand types the owner's line to `…BEST WORK IN` at 12 cps,
//! the program inserts `TO ` at the caret (a Tab's completion, a three-cell
//! paste, or no key at all), and the hand types `MAIN!!!!`. The insert's
//! licence is stamped as the host stamps it: a Tab is `note_user_gesture` and
//! `note_insert_armed(Unknown)` at the press (`app_input.rs`); a ⌘V is the
//! gesture stamped and revoked at the enqueue and
//! `note_insert_delivered_from(Cells(3))` at the delivery edge (`input_paste`,
//! `apply_delivery`). The repaint is put on the glass in one of six shapes
//! ([`Shape`]): `inkish.py`'s split whole-line redraw, the same bytes in one
//! read, the same bytes with the hide alone in the first read, Claude Code's
//! own diff frame (the recorded `?2026h ?25l … ?25h ?2026l` shape) whole or
//! torn after its hide, and a shell's print at a visible caret.
//!
//! **Measured before the fix** (this tree, the torn-read and corridor fixes
//! in): every licensed insert whose frame reached the glass TORN BEFORE ITS
//! GLYPHS — the hide alone in the first 1024-byte read, the `?2026` hold
//! capped — was laid by nobody. The next present shows the caret the
//! insert's width on, with the print it made arriving in the same present:
//! the anchored lane judges a print only under a HIDDEN caret, and the hide
//! bridge admitted only a typed or nav reach or the unpaid presses' echo. It
//! refused the reappearance as `hidden-relocation` (the Tab) or silently
//! (the paste, whose gesture the enqueue had revoked), and the licence lapsed
//! unspent. The hand's `M` then landed three cells past the band's end:
//! `join_cohort` refused it (`col <= c.col1`) and minted a second cohort
//! beside the first, and the one-finger hold kept both lit, so `O` and ` `
//! of `INTO ` stayed dark for 73 frames, to the end of the take. Every other
//! licensed shape was already laid, by the insert arm on the visible lane or
//! by the anchored lane under the hidden caret. The reproducer that reported
//! the Tab and paste red on the split redraw (folded into this file) never
//! fed the print anchor, which the GUI feeds on every frame
//! (`app_render.rs`). With the anchor fed, those takes were already clean.
//!
//! **The fix** (`CursorGlow::hidden_bridge_source`): a hidden→visible
//! reappearance that a fresh insert licence lays is bridged, with the insert's
//! own reach and whatever the age of the hide, as the unpaid presses' echo is
//! bridged. The predicate is exactly the one `spawn` lays the insert by
//! (`visible_insert_echo`), so the bridge admits nothing the insert arm then
//! refuses. Nothing it cannot buy is admitted: a reappearance wider than the
//! paste, a Tab's echo after its one-second window, and program output with
//! no licence all stay dark.
//!
//! **What is NOT changed: program output with no licence.** T1 and v3's X1
//! keep the program's cells dark (pinned here). The hand's next key past them
//! still mints a second band, and the one-finger hold keeps the band left of
//! the insert lit beside it, with the gap between, for as long as the hand
//! types. v3's S7 ("no hole in a phrase") would move that band out of the open
//! phrase so it leaves on its own clock. That shortens the gap to 0.8–1.1 s
//! after the key, but it also takes the hand's own line dark while the hand
//! is still typing. Choosing between those is the owner's ruling, not this
//! file's.
//!
//! The census is the PLAN's coverage on the composer row every frame: `#` at
//! ≥ 20 coverage, `.` dark. A HOLE is a dark column strictly between two lit
//! ones. A licensed take allows none that stands longer than
//! [`HOLE_FRAMES_MAX`] frames, from the first key to the end of the take.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle, InsertWidth};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COLS: usize = 140;
/// 12 cps, the owner's pace.
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
/// `inkish.py`'s pause between a redraw's two reads.
const INK_SPLIT_MS: u64 = 30;
/// A column is LIT at this planned coverage (the glass census's 20/255).
const LIT_COV: u8 = 20;
/// A hole standing longer than this many frames is the defect.
const HOLE_FRAMES_MAX: usize = 3;

/// The owner's line.
const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN!!!!";
/// Where the hand's typing stops before the insert.
const TYPED_TO: &str = "WORK IN";
/// What the program inserts at the caret.
const INSERT: &str = "TO ";
/// What the hand types after it.
const SUFFIX: &str = "MAIN!!!!";

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

/// The owner's config: rainbow kitty, tall body, intensity 0.70, dark.
fn cfg() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style: GlowStyle::RainbowKitty,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    }
}

/// The host's `typed_class_for` and `glyph_shifted && !spacebar`.
fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        _ => (false, TypedClass::Glyph),
    }
}

/// How the composer's repaint reaches the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// `inkish.py`: hide, home, clear, the whole line | present | 30 ms |
    /// the caret placed and shown | present.
    InkSplit,
    /// The same bytes in one read, one present.
    InkWhole,
    /// The hide alone in the first read | present | 30 ms | the line, the
    /// caret, the show | present: a frame torn before its glyphs.
    InkHideFirst,
    /// Claude Code's diff frame, the recorded shape: `?2026h ?25l`, the
    /// changed glyphs on the composer row, a CUP to the hint row, the
    /// caret's CUP, `?25h ?2026l` — one read, one present.
    ClaudeWhole,
    /// The same frame torn after `?2026h ?25l`: the hold capped, the rest
    /// a read later.
    ClaudeTorn,
    /// A shell completing at a visible caret: the glyphs printed where the
    /// caret stands, no hide.
    Visible,
}

/// The licence the host stamps for the insert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lic {
    /// Program output: no key behind it.
    None,
    /// A bare Tab.
    Tab,
    /// A ⌘V priced at `n` cells.
    Paste(u16),
}

/// One frame's census of the composer row.
#[derive(Clone, Debug)]
struct Census {
    ms: u64,
    label: &'static str,
    text: String,
    lit: String,
    holes: Vec<u16>,
    /// Columns of the row that carry a cell of any cohort, leaving or not.
    cells: Vec<u16>,
}

/// The host: one terminal, the kitty, one clock, the composer's state.
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
    /// The composer's row, and the grid column of its text's first glyph.
    row: u16,
    col0: usize,
    text: String,
    caret: usize,
    label: &'static str,
    census: Vec<Census>,
}

impl Host {
    fn new(shape: Shape) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        // Ink's composer at the top of the alt screen; Claude Code's in its
        // bottom-pinned box behind `│ > `.
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
            cfg: cfg(),
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
            label: "start",
            census: Vec::new(),
        };
        h.frame();
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// EXACTLY the render path: LOCK A's caret row and every ribbon row
    /// whether or not the caret is visible, the print anchor fed immediately
    /// before the tick (`app_render.rs`), the tick, then the census.
    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink {
            self.blink = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            self.glow.observe_ribbon_row(r, &self.row_buf);
        }
        self.glow.observe_print_anchor(self.term.print_anchor());
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
        self.record();
    }

    fn record(&mut self) {
        let row = self.row;
        let mut cov = [0u8; COLS];
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        for sg in rib.plan_segments() {
            let r = ((sg.spine / CH as f32) - 0.5).floor();
            let c = (sg.x / CW as f32).floor();
            if r >= 0.0 && c >= 0.0 && r as u16 == row && (c as usize) < COLS {
                let i = c as usize;
                cov[i] = cov[i].max(sg.cov);
            }
        }
        let lit: Vec<usize> = (0..COLS).filter(|&i| cov[i] >= LIT_COV).collect();
        let holes = if lit.len() >= 2 {
            (lit[0]..=lit[lit.len() - 1])
                .filter(|&i| cov[i] < LIT_COV)
                .map(|i| i as u16)
                .collect()
        } else {
            Vec::new()
        };
        let mut cells: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| c.col)
            .collect();
        cells.sort_unstable();
        cells.dedup();
        self.term.row_cols_into(usize::from(row), &mut self.row_buf);
        let text: String = self
            .row_buf
            .iter()
            .map(|&c| if c == '\0' { '_' } else { c })
            .collect();
        let lit: String = cov
            .iter()
            .map(|&v| if v >= LIT_COV { '#' } else { '.' })
            .collect();
        self.census.push(Census {
            ms: self.ms(),
            label: self.label,
            text: text.trim_end().to_string(),
            lit: lit.trim_end_matches('.').to_string(),
            holes,
            cells,
        });
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

    /// The CUP to text column `col` of the composer.
    fn cup(&self, col: usize) -> String {
        format!("\x1b[{};{}H", self.row + 1, self.col0 + col + 1)
    }

    /// The Ink redraw's two halves: hide, home, clear, `line`; the caret
    /// placed and shown.
    fn ink_halves(&self, line: &str) -> (String, String) {
        (
            format!("\x1b[?25l\x1b[{};1H\x1b[2K{line}", self.row + 1),
            format!("{}\x1b[?25h", self.cup(self.caret)),
        )
    }

    /// The program's repaint after its text went from `before` to
    /// `self.text` (the caret at `self.caret`), in this host's shape.
    fn repaint(&mut self, before: &str) {
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
                // The diff: from the first changed text column on.
                let first = before
                    .chars()
                    .zip(self.text.chars())
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| before.len().min(self.text.len()));
                let body = format!(
                    "{}{}\x1b[{ROWS};1H{}\x1b[?25h\x1b[?2026l",
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
                let tail = self.text[before.len()..].to_string();
                self.term.process(tail.as_bytes());
            }
        }
        self.frame();
    }

    /// One typed key `KEY_MS` after the last event, stamped as the host
    /// stamps it.
    fn key(&mut self, ch: char) {
        self.schedule(KEY_MS);
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        let before = self.text.clone();
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.repaint(&before);
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    /// The insert: its licence stamped `gap_ms` after the last key, the
    /// program's repaint of it `echo_ms` after that.
    fn insert(&mut self, s: &str, lic: Lic, gap_ms: u64, echo_ms: u64) {
        self.schedule(gap_ms);
        let press = self.now;
        match lic {
            Lic::None => {}
            Lic::Tab => {
                self.glow.note_user_gesture(press);
                self.glow.note_insert_armed(press, InsertWidth::Unknown);
            }
            Lic::Paste(_) => {
                self.glow.note_user_gesture(press);
                self.glow.revoke_input_hints_at(press);
            }
        }
        self.schedule(echo_ms);
        if let Lic::Paste(n) = lic {
            // The delivery edge, applied before the frame that shows the
            // echo.
            self.glow
                .note_insert_delivered_from(press, self.now, InsertWidth::Cells(n));
        }
        let before = self.text.clone();
        self.text.insert_str(self.caret, s);
        self.caret += s.chars().count();
        self.repaint(&before);
    }

    /// A key whose Ink redraw is TORN: the first present shows `shown` with
    /// the caret hidden, the second, 30 ms later, the whole text and the
    /// caret.
    fn torn_key(&mut self, ch: char, shown: &str) {
        self.schedule(KEY_MS);
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.torn_redraw(shown);
    }

    /// A program repaint `ms` after the last event, torn the same way.
    fn torn_repaint(&mut self, shown: &str, ms: u64) {
        self.schedule(ms);
        self.torn_redraw(shown);
    }

    fn torn_redraw(&mut self, shown: &str) {
        let (head, _) = self.ink_halves(shown);
        self.term.process(head.as_bytes());
        self.frame();
        self.now += Duration::from_millis(INK_SPLIT_MS);
        let (head, tail) = self.ink_halves(&self.text);
        self.term
            .process(format!("{}{tail}", &head[6..]).as_bytes());
        self.frame();
    }

    /// Runs of consecutive frames with a hole: `(first, last, widest)`.
    fn hole_runs(&self) -> Vec<(usize, usize, Vec<u16>)> {
        let mut runs: Vec<(usize, usize, Vec<u16>)> = Vec::new();
        for (i, f) in self.census.iter().enumerate() {
            if f.holes.is_empty() {
                continue;
            }
            match runs.last_mut() {
                Some(r) if r.1 + 1 == i => {
                    r.1 = i;
                    if f.holes.len() > r.2.len() {
                        r.2 = f.holes.clone();
                    }
                }
                _ => runs.push((i, i, f.holes.clone())),
            }
        }
        runs
    }

    fn dump(&self, from: usize, to: usize) -> String {
        let mut s = String::new();
        for (i, f) in self.census.iter().enumerate().take(to + 1).skip(from) {
            s.push_str(&format!(
                "f={i:04} t={:>6}ms [{}] holes={:?}\n  txt |{}|\n  lit |{}|\n",
                f.ms, f.label, f.holes, f.text, f.lit
            ));
        }
        s
    }

    /// No hole stands longer than [`HOLE_FRAMES_MAX`] frames; the frames
    /// around the longest when one does.
    fn assert_no_standing_hole(&self, what: &str) {
        let runs = self.hole_runs();
        if let Some((a, b, cols)) = runs.iter().max_by_key(|(a, b, _)| b - a) {
            assert!(
                b - a < HOLE_FRAMES_MAX,
                "{what}: a hole stood {} frames under {cols:?}\n{}",
                b - a + 1,
                self.dump(a.saturating_sub(2), (b + 2).min(a + 12))
            );
        }
    }

    /// The grid columns the insert took.
    fn insert_cols(&self) -> std::ops::Range<u16> {
        let first = self.col0 + typed_prefix().len();
        (first as u16)..((first + INSERT.len()) as u16)
    }
}

fn typed_prefix() -> &'static str {
    let i = LINE.find(TYPED_TO).expect("the prefix is in the line") + TYPED_TO.len();
    &LINE[..i]
}

fn completed() -> String {
    format!("{}{INSERT}", typed_prefix())
}

/// The take: the hand types to `…WORK IN`, the insert lands, the hand types
/// `MAIN!!!!`, then 600 ms idle.
fn take(shape: Shape, lic: Lic, gap_ms: u64, echo_ms: u64) -> Host {
    let mut h = Host::new(shape);
    h.label = "type";
    h.type_str(typed_prefix());
    h.label = "insert";
    h.insert(INSERT, lic, gap_ms, echo_ms);
    h.label = "suffix";
    h.type_str(SUFFIX);
    h.label = "idle";
    h.idle(600);
    assert_eq!(h.text, LINE, "the composer ends with the owner's line");
    h
}

/// A licensed take is laid as the insert, once, and leaves no hole; the
/// frames around the insert when it does not.
fn assert_laid_whole(h: &Host, what: &str) {
    let at = h
        .census
        .iter()
        .position(|f| f.label == "insert")
        .unwrap_or(0);
    let around = h.dump(at.saturating_sub(1), at + 6);
    let tally = h.glow.admission_tally();
    assert_eq!(
        h.glow.insert_tally().lit,
        1,
        "{what}: the insert's echo is laid as the insert, once (last decline {:?})\n{around}",
        tally.last_decline_reason
    );
    assert_eq!(
        tally.declined, 0,
        "{what}: nothing is declined (last {:?})\n{around}",
        tally.last_decline_reason
    );
    h.assert_no_standing_hole(what);
}

#[test]
fn a_tab_completion_whose_frame_is_torn_before_its_glyphs_is_laid_as_the_insert() {
    // Before the fix: declined `hidden-relocation`, `O` and ` ` of `INTO `
    // dark for 73 frames in every take.
    for shape in [Shape::InkHideFirst, Shape::ClaudeTorn] {
        for (gap, echo) in [(120, 0), (120, 40), (700, 200)] {
            let h = take(shape, Lic::Tab, gap, echo);
            assert_laid_whole(&h, &format!("{shape:?} Tab gap={gap} echo={echo}"));
        }
    }
}

#[test]
fn a_paste_whose_frame_is_torn_before_its_glyphs_is_laid_as_the_insert() {
    // Before the fix: refused silently (the enqueue revoked the paste's
    // gesture), the same 73-frame hole.
    for shape in [Shape::InkHideFirst, Shape::ClaudeTorn] {
        for (gap, echo) in [(120, 0), (120, 40), (700, 200)] {
            let h = take(shape, Lic::Paste(3), gap, echo);
            assert_laid_whole(&h, &format!("{shape:?} Paste(3) gap={gap} echo={echo}"));
        }
    }
}

/// The controls: every shape whose print or caret move reaches the glass
/// whole was laid before the fix, by the anchored lane (the split redraw's
/// print under the hidden caret) or by the insert arm (a visible hop), and
/// still is.
#[test]
fn a_licensed_insert_is_laid_whole_in_every_frame_shape_that_reaches_the_glass_whole() {
    for shape in [
        Shape::InkSplit,
        Shape::InkWhole,
        Shape::ClaudeWhole,
        Shape::Visible,
    ] {
        for lic in [Lic::Tab, Lic::Paste(3)] {
            let h = take(shape, lic, 120, 0);
            assert_laid_whole(&h, &format!("{shape:?} {lic:?}"));
        }
    }
}

/// T1 and v3's X1: program output at the caret lays no cell, with the caret
/// visible or hidden, torn or whole — the new bridge lane opens for a
/// licence and for nothing else. (The gap beside these cells when the hand
/// types on is not pinned either way: it is the owner's ruling, see the
/// module doc.)
#[test]
fn program_output_at_the_caret_lays_no_light_however_its_frame_reaches_the_glass() {
    for shape in [
        Shape::InkSplit,
        Shape::InkWhole,
        Shape::InkHideFirst,
        Shape::ClaudeWhole,
        Shape::ClaudeTorn,
        Shape::Visible,
    ] {
        let h = take(shape, Lic::None, 120, 0);
        let cols = h.insert_cols();
        let lit_under = h
            .census
            .iter()
            .enumerate()
            .find(|(_, f)| f.cells.iter().any(|c| cols.contains(c)));
        assert!(
            lit_under.is_none(),
            "{shape:?}: the program's `TO ` at {cols:?} carries a cell\n{}",
            lit_under.map_or(String::new(), |(i, _)| h.dump(i.saturating_sub(1), i + 2))
        );
        assert_eq!(h.glow.insert_tally().lit, 0, "{shape:?}: no insert laid");
    }
}

/// The bridge admits only what the licence buys: a torn reappearance WIDER
/// than the paste's priced width, or a Tab's torn echo after its one-second
/// window (`INSERT_GESTURE_HINT_FRESH`), is refused as the visible lane
/// refuses it, and the program's cells stay dark.
#[test]
fn a_torn_reappearance_the_licence_cannot_buy_lays_nothing() {
    // A three-cell paste, then the program re-lays the line nine cells on.
    let mut h = Host::new(Shape::InkHideFirst);
    h.type_str(typed_prefix());
    h.insert("TO MAIN! ", Lic::Paste(3), 120, 0);
    let first = h.col0 + typed_prefix().len();
    let wide = (first as u16)..((first + 9) as u16);
    assert_eq!(
        h.glow.insert_tally().lit,
        0,
        "a 9-cell hop is not a 3-cell paste"
    );
    assert!(
        !h.census
            .iter()
            .any(|f| f.cells.iter().any(|c| wide.contains(c))),
        "the hop the paste could not buy lays nothing"
    );
    // A Tab whose completion lands 1.2 s after the press.
    for shape in [Shape::InkHideFirst, Shape::ClaudeTorn] {
        let mut h = Host::new(shape);
        h.type_str(typed_prefix());
        h.insert(INSERT, Lic::Tab, 120, 1200);
        let cols = h.insert_cols();
        assert_eq!(
            h.glow.insert_tally().lit,
            0,
            "{shape:?}: the Tab's window closed"
        );
        assert!(
            !h.census
                .iter()
                .any(|f| f.cells.iter().any(|c| cols.contains(c))),
            "{shape:?}: a lapsed Tab's echo lays nothing"
        );
    }
}

/// The reproducer's torn-frame half, kept as a guard (the torn-read fix
/// closed `b3` and `b7`; the rest were already clean): with the hand typing
/// `MAIN!!!!` after `INTO `, one Ink repaint shows the line damaged for a
/// frame and the next restores it. Every shape heals: three interior cells
/// blanked or replaced, the suffix from `TO ` cut off (a 1024-byte tear),
/// on the first key after `INTO ` (the owner's shape), or one cell short of
/// the caret — under a key, or between keys.
#[test]
fn a_one_frame_tear_of_the_completed_word_heals_while_the_hand_types() {
    fn blank_to(text: &str) -> String {
        let i = typed_prefix().len();
        let mut s = text.to_owned();
        s.replace_range(i..i + INSERT.len(), "   ");
        s
    }
    fn cut_at_to(text: &str) -> String {
        text[..typed_prefix().len()].to_owned()
    }
    fn other_glyph_to(text: &str) -> String {
        let i = typed_prefix().len();
        let mut s = text.to_owned();
        s.replace_range(i..i + 2, "XX");
        s
    }
    fn cut_last_two(text: &str) -> String {
        text[..text.len() - 2].to_owned()
    }
    type Tear = fn(&str) -> String;
    let tears: [(&str, Tear, bool); 7] = [
        ("b1 TO blanked, keyed", blank_to, true),
        ("b2 TO blanked, keyless", blank_to, false),
        ("b3 cut at TO, keyed", cut_at_to, true),
        ("b4 cut at TO, keyless", cut_at_to, false),
        ("b5 TO -> XX, keyed", other_glyph_to, true),
        ("b6 TO -> XX, keyless", other_glyph_to, false),
        ("b8 cut one short, keyed", cut_last_two, true),
    ];
    for (what, tear, keyed) in tears {
        let mut h = Host::new(Shape::InkSplit);
        h.type_str(&completed());
        h.type_str("MAI");
        h.label = "torn";
        if keyed {
            let mut text = h.text.clone();
            text.insert(h.caret, 'N');
            h.torn_key('N', &tear(&text));
        } else {
            let shown = tear(&h.text.clone());
            h.torn_repaint(&shown, 40);
            h.label = "type";
            h.key('N');
        }
        h.type_str("!!!!");
        h.idle(600);
        assert_eq!(h.text, LINE);
        h.assert_no_standing_hole(what);
    }
    // b7: the tear on the `M` key, the torn present showing `…WORK IN`.
    let mut h = Host::new(Shape::InkSplit);
    h.type_str(&completed());
    h.label = "torn";
    let mut text = h.text.clone();
    text.insert(h.caret, 'M');
    h.torn_key('M', &cut_at_to(&text));
    h.type_str("AIN!!!!");
    h.idle(600);
    assert_eq!(h.text, LINE);
    h.assert_no_standing_hole("b7 cut at TO on the M key");
}
