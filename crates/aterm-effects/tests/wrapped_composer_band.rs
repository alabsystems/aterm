// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE BAND IN A WRAPPED COMPOSER IS ONE RUN, ON EVERY FRAME** (2026-09-16
//! — the owner, on the 0.87 candidate: *"I still am sometimes seeing bugs and
//! gaps"*, and earlier *"when backspacing in 0.86 the rainbow cursor trail
//! breaks a part"*).
//!
//! [`Ribbon::retract_suffix`] states the law this file measures, and states it
//! as a law: *"CONTIGUOUS at every frame … nothing is ever removed from the
//! MIDDLE"*. Every path that takes light off a row by GEOMETRY — the
//! Backspace's retract, the kill, the insert's rewrite, the exit swoosh —
//! obeys it by construction. The CONTENT WITNESS is the one path that takes
//! light off a row by NAME, one cell at a time, and a name is not a shape.
//!
//! Reproduced at the HOST seam, like `scrub_gaps.rs`: a real
//! `aterm_core::terminal::Terminal` driven byte for byte, its rows sampled
//! exactly as `app_render.rs`'s LOCK A samples them, its content-scroll clock
//! read exactly as `app_render::sync_cursor_effect_scroll` reads it, fed to
//! `CursorGlow` and ticked on a frame train. The program is
//! `scratch/composer.py`'s BOTTOM-PINNED WRAPPING BOX — the shape of Claude
//! Code's and Codex's composer, which is what the owner types into:
//!
//! * one logical input that WRAPS at the box's inner width;
//! * the box is pinned to the last row and GROWS UPWARD, so a new wrap row
//!   scrolls the screen and the paragraph's first row moves up under the band;
//! * every repaint homes to the box's top row, clears to the end of the
//!   screen, rewrites every row and parks the cursor on the caret — in FIVE
//!   separate writes, because `composer.py`'s `w()` flushes on every call, so
//!   a present can land between any two of them.
//!
//! The census is of the PLAN — the coverage the emitter will put in each cell
//! — because that, and not the live-cell set, is what a pixel census on glass
//! reads: a retracting cell is still light until it has spent to zero.
//!
//! **What this catches.** Before the fix, the owner's own gesture — the caret
//! walked back three words into the wrapped paragraph, then a Backspace run
//! mid-word — left the band in pieces. The witness compares one column's
//! recorded glyph with the one standing there now; a mid-line Backspace
//! shifts every column from the edit to the end of the line, and wherever the
//! shifted text repeats a letter the comparison answers "unchanged" and keeps
//! that one cell while naming both its neighbours. Measured here, the named
//! set came out a COMB over `…and I alsosee some a` — columns 26, 28, 30, 31,
//! 32, 33 and 35 retired, 27, 29 and 34 kept — and 120 ms later, when the
//! fast melt had taken the named ones, row 13 read
//! `..#############################...##` : a solid head and two detached
//! specks, 16 frames of a 12 s take with interior dark runs up to 4 cells
//! wide. `Witness::walk`'s span closure is what makes that impossible.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

const ROWS: usize = 14;
const COLS: usize = 80;
const CW: usize = 15;
const CH: usize = 28;
/// `composer.py`'s inner width: the terminal's columns less its box chrome.
const W: usize = COLS - 4;
const PROMPT: &str = "> ";
const CONT: &str = "  ";
/// 12 cps, the rate the owner's takes were driven at.
const KEY_MS: u64 = 83;
/// The frame train.
const FRAME_MS: u64 = 8;
/// Milliseconds between a repaint's five writes. `composer.py` flushes five
/// times per paint; on glass the host presents between them.
const WRITE_GAP_MS: u64 = 3;
/// A cell counts as LIT at this planned coverage — `ribscan2.py`'s pixel
/// census calls a cell lit at 20% of the bed.
const LIT_COV: u8 = 20;

/// The owner's own paragraph, from `drive2.py`.
const PARA: &str = "there is a bug that when backspacing in this build the rainbow cursor trail breaks apart and I also see some awkward transitions when going to a new line still with the rainbow please audit that logic";
/// How much of it the takes type: enough to wrap onto a second row.
const TYPED: usize = 110;

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

/// One frame's census of one row: the lit columns, as a map, with the span's
/// interior dark runs.
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

/// The host: one terminal, the kitty, one clock, and the composer's own state.
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
        // The shell's prompt is on the last row; the composer takes it.
        term.process(format!("\x1b[{ROWS};1H").as_bytes());
        let mut c = Self {
            term,
            glow,
            cfg: cfg(),
            g: geom(),
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

    /// EXACTLY the host's own order: `sync_cursor_effect_scroll`, then LOCK
    /// A's samples, then the tick.
    fn frame(&mut self) {
        let scroll = self.term.content_scroll_state();
        match ContentScrollState::delta_since(self.scroll, scroll) {
            ContentScrollDelta::Baseline | ContentScrollDelta::Unchanged => {}
            ContentScrollDelta::Translate(rows) => {
                self.glow.note_scroll(rows);
                self.glow.drop_row_probe();
            }
            ContentScrollDelta::Bands { first_seq, count } => {
                for i in 0..u64::from(count) {
                    let m = scroll.band(first_seq + i);
                    self.glow.note_band_move(m.top, m.bottom, m.delta);
                }
                self.glow.drop_row_probe();
            }
            ContentScrollDelta::Invalidate => self.glow.curtain(self.now),
        }
        self.scroll = Some(scroll);
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

    /// The next event's instant, `ms` after the last, with the train run to it.
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

    /// `composer.py`'s `paint()`, byte for byte and WRITE for write: the grow
    /// scroll, the home-and-clear, the body, the caret park — each its own
    /// flush, with the frame train running between them.
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
            // The box grew: scroll the screen so the bottom stays pinned.
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

    /// Alt-B: the caret back one word, as a navigation key.
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

    /// This frame's per-row census of the PLAN's coverage — what the glass
    /// shows, not what the pool holds.
    fn record(&mut self) {
        let mut cov = vec![0u8; ROWS * COLS];
        {
            let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
            for sg in rib.plan_segments() {
                let r = ((sg.spine / CH as f32) - 0.5).floor();
                let col = (sg.x / CW as f32).floor();
                if r < 0.0 || col < 0.0 {
                    continue;
                }
                let (r, col) = (r as usize, col as usize);
                if r < ROWS && col < COLS {
                    cov[r * COLS + col] = cov[r * COLS + col].max(sg.cov);
                }
            }
        }
        let ms = self.ms();
        for r in 0..ROWS {
            let row = &cov[r * COLS..(r + 1) * COLS];
            let lit: Vec<usize> = (0..COLS).filter(|&i| row[i] >= LIT_COV).collect();
            if lit.len() < 2 {
                continue;
            }
            let mut holes = Vec::new();
            let mut run = 0usize;
            for &v in &row[lit[0]..=lit[lit.len() - 1]] {
                if v >= LIT_COV {
                    if run > 0 {
                        holes.push(run);
                    }
                    run = 0;
                } else {
                    run += 1;
                }
            }
            self.census.push(RowCensus {
                ms,
                row: r as u16,
                map: row
                    .iter()
                    .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                    .collect(),
                holes,
            });
        }
    }

    /// Every row-frame of the take whose band had an interior dark run.
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

    /// The take must actually have LAID a band, or "no holes" is a statement
    /// about nothing — the shape this file is written against is a band with
    /// holes, and an empty row has none.
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

/// **THE OWNER'S OWN GESTURE** — the caret scrubbed back into the middle of a
/// wrapped line, then a Backspace run mid-word, every glyph right of the caret
/// shifting one column on every erase. RED before the span closure in
/// `Witness::walk`: 16 row-frames with interior dark runs up to 4 cells.
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

/// The same box, the Backspace run at the END of the text — the gesture the
/// owner first named ("when backspacing in 0.86 the rainbow cursor trail
/// breaks a part"). The band crosses the fold here: the box grows a row, the
/// screen scrolls under the paragraph, and the first row's light rides up with
/// its text while the hand types on the new one.
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

/// The third shape the owner described: the caret moved back into typed text
/// and MORE typed there, so every glyph right of the caret shifts one column
/// to the RIGHT on every key — the insert's mirror of the Backspace run.
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
