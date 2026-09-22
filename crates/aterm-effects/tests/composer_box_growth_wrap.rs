// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **CLAUDE CODE'S COMPOSER WRAP, WITH THE REAL BYTES** (2026-09-21) — the
//! owner, on v0.90.0 with Claude Code in the window: *"incorrect blending
//! after a new line — a hard vertical colour edge between blue and purple a
//! few cells into the row the composer wrapped onto"*, and *"when typing
//! wraps to a new line the previous row's rainbow vanishes suddenly while
//! the new row populates — I want a beautiful animation where the previous
//! row's rainbow flows in the direction of typing while the rainbow
//! continues on the next line"*.
//!
//! What Claude Code's composer writes at the wrap was CAPTURED
//! (`docs/measured/claude-code-composer-wrap-bytes-2026-09-21.md`: Claude
//! Code v2.1.278 under a Python pty, 90×30, one key per 60 ms) and no
//! fixture in the repo modelled it: per ordinary key ONE glyph addressed
//! from home; a Space a caret move only; and at the wrap key ONE chunk that
//! repaints the bottom-anchored box ONE ROW HIGHER WITHOUT A SCROLL — the
//! rule row up a row, the text row rewritten a row up minus the word the
//! wrap moved, the continuation row holding that word beside a glyph Ink's
//! diff reused, and the caret making a same-row backward move.
//!
//! Every law below runs at the HOST seam — a real
//! `aterm_core::terminal::Terminal` driven byte for byte, its rows sampled
//! exactly as `app_render.rs`'s LOCK A samples them, fed through
//! `CursorGlow::observe_row` / `observe_ribbon_row` / `ribbon_rows` and
//! ticked through `CursorGlow::tick` at 16.7 ms, keys at 60 ms — and reads
//! the rows' COVERAGE CENSUS frame by frame, the helpers copied from
//! `tests/new_line_fade.rs` (`row_quads`, `coverage`, `dark_at`,
//! `worst_step`, `max_rise`). Each law says whether it was RED on the
//! unmodified tree; the new symbols the laws read (`FLOW_TOTAL_S`,
//! `Cohort::flow`, `Status::followed`) do not exist there, so the redness
//! is by the mechanism the doc above names, restated at each law.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::{FLOW_TOTAL_S, RETIRE_MELT_S, WALK_FAST_CELLS};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

/// The owner's window: 90 columns, 30 rows, the retina cell 15×28 device px.
const ROWS: usize = 30;
const COLS: usize = 90;
const CW: usize = 15;
const CH: usize = 28;

/// The frame train and the hand's cadence.
const FRAME_US: u64 = 16_667;
const KEY_MS: u64 = 60;

/// The composer's rows, 1-based as the escapes carry them: the rule on
/// 27, the text row on 28, the status on 29–30 — before the box grows.
const RULE_ROW: usize = 27;
const TEXT_ROW: usize = 28;
/// …and 0-based, as the engine reads them.
const OLD_ROW: u16 = (TEXT_ROW - 1) as u16;
const NEW_ROW: u16 = OLD_ROW - 1;

/// **THE SYNCHRONIZED-OUTPUT BRACKET.** Ink wraps every repaint in DEC
/// 2026 (`tests/ink_wrap_line_fill_repro.rs` records it on glass:
/// sync-begin, hide, the rewrite, show, sync-end), and aterm's seam reads
/// the hide INSIDE that bracket as the "repaint blink" that admits an
/// alt-screen re-anchor (`Terminal::repaint_blink_epoch`,
/// `CursorGlow::blink_fresh`). The pty capture carries NO bracket: nothing
/// answered Claude Code's DECRQM 2026 query there, while aterm does. The
/// bytes below are the capture's with the bracket the window sees put
/// back; without it the seam classes the wrap as a keyless park and never
/// lays the wrap key's own glyph — a premise the doc records.
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

/// The first text row up to the wrap: keys 0..=82 land on 1-based columns
/// 3..=85 (the engine's 2..=84); the Space at 86, `W` at 87, `H` at 88,
/// and the `Y` wraps.
const LINE1: &str =
    "HEY!!! [Image #1] in the other tab in aterm, I got this confirmation message. AUDIT";
/// The keys after the wrap, typed on the continuation row.
const AFTER: &str = " ATERM claude code h";

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

/// The owner's setup: rainbow kitty at intensity 0.70 on the default dark
/// theme (fg `0xD0D0D0`, bg `0x111318`).
fn cfg() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00D0_D0D0,
        theme_bg: 0x0011_1318,
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

fn rule() -> String {
    let mut s: String = "\u{2500}".repeat(78);
    s.push_str("\x1b[80Gworkspace\x1b[90G\u{2500}");
    s
}

/// One ordinary key's echo, as captured: the glyph `ch` on 1-based column
/// `c` of the text row, addressed from home, the status row touched, the
/// caret put after it — inside a hide/show bracket.
fn glyph_bytes(ch: char, c: usize) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}C\x1b[{}B{ch}\x1b[30;1H\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        c - 1,
        TEXT_ROW - 1,
        c + 1
    )
    .into_bytes()
}

/// A Space's echo: the caret moves and nothing is written.
fn space_bytes(c: usize) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        c + 1
    )
    .into_bytes()
}

/// **THE WRAP CHUNK**, in the captured shape: the rule repainted one row up
/// (row 26), the first text row (`line1`) rewritten on row 27 without the
/// moved word plus `EL`, the continuation row holding `  WH` with the
/// reused `Y` left standing on column 5 and `EL` from column 6, the caret
/// to (28, 6).
fn wrap_bytes(line1: &str) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}B{}\r\x1b[1B\u{276f}\u{a0}{line1}\x1b[K\r\x1b[1B  WH\x1b[6G\x1b[K\x1b[30;1H\x1b[{TEXT_ROW};6H\x1b[?25h{SYNC_END}",
        RULE_ROW - 2,
        rule()
    )
    .into_bytes()
}

/// The host: one terminal, one glow engine, one clock, a frame train.
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
}

impl Host {
    /// Claude Code's screen as painted before the first key: the alt
    /// screen, the rule on row 27, the prompt marker on row 28, the status
    /// on row 29, the caret on (28, 3); one frame presented.
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
            g: geom(),
            now,
            next_frame: now,
            last_key: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            col: 3,
        };
        h.frame();
        h
    }

    /// EXACTLY LOCK A, then the tick: sample the cursor, the repaint blink,
    /// the caret row's probe, and the rows the resident ribbon wants — all
    /// from the terminal AFTER the last `process` — then advance the engine
    /// one frame. (`tests/new_line_fade.rs` `Host::frame`, verbatim.)
    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
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
        self.next_frame = self.now + Duration::from_micros(FRAME_US);
    }

    /// Run the frame train up to and including the last frame at or before
    /// `t`.
    fn advance_to(&mut self, t: Instant) {
        while self.next_frame <= t {
            self.now = self.next_frame;
            self.frame();
        }
        self.now = self.now.max(t);
    }

    /// Frames for `ms` more.
    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.advance_to(t);
    }

    /// One key, `KEY_MS` after the last: the frames until then run, the
    /// app_input seam arms the typed hint at the key, the PTY answers
    /// `bytes`, and the next frame of the train samples the echo — as the
    /// window does.
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

    /// An ordinary key on the text row: a glyph's echo or a Space's move.
    fn key(&mut self, ch: char) {
        let bytes = if ch == ' ' {
            space_bytes(self.col)
        } else {
            glyph_bytes(ch, self.col)
        };
        self.press(ch, &bytes);
        self.col += 1;
    }

    /// Type `s` a key at a time.
    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    /// The wrap key: the `Y` of `WHY`, answered by the chunk that repaints
    /// the box one row up with `line1` on the first text row; the next
    /// glyph lands on column 6 of the continuation row.
    fn wrap(&mut self, line1: &str) {
        self.press('Y', &wrap_bytes(line1));
        self.col = 6;
    }

    /// One more frame of the train, whenever it is due.
    fn next_frame(&mut self) {
        let t = self.next_frame;
        self.advance_to(t);
    }

    fn ribbon(&self) -> &aterm_effects::rainbow_kitty::ribbon::Ribbon {
        self.glow.v2_ribbon().expect("rainbow kitty owns the frame")
    }

    /// Resident ribbon cells on `row`: `(col, leaving)`, sorted.
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

    /// The bed's quads whose vertical extent covers the CENTRE of `row`'s
    /// pixel band this frame (`tests/new_line_fade.rs` `row_quads`).
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

    /// THE ROW'S COVERAGE CENSUS: the share of `row`'s pixel width the
    /// bed's quads cover this frame, each weighted by its source-over
    /// opacity (`tests/new_line_fade.rs` `coverage`).
    fn coverage(&self, row: u16) -> f32 {
        let sum: f32 = self
            .row_quads(row)
            .map(|q| f32::from(q.w) * f32::from(q.alpha) / 255.0)
            .sum();
        sum / (COLS * CW) as f32
    }

    /// The row's coverage over its LEFT 40 columns — the part of a line a
    /// Backspace run at its end does not reach.
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

    /// The opacity-weighted centroid of the row's lit pixels, in px from
    /// the row's left edge; `None` with nothing lit.
    fn centroid(&self, row: u16) -> Option<f32> {
        let (mut num, mut den) = (0.0f32, 0.0f32);
        for q in self.row_quads(row) {
            let w = f32::from(q.w) * f32::from(q.alpha) / 255.0;
            num += (f32::from(q.x) + f32::from(q.w) * 0.5) * w;
            den += w;
        }
        (den > 0.0).then(|| num / den)
    }

    /// The leftmost lit pixel on the row, or `None`.
    fn left_edge(&self, row: u16) -> Option<u16> {
        self.row_quads(row).map(|q| q.x).min()
    }

    fn retired(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.retired)
    }

    fn followed(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.followed)
    }

    /// Idle frames at the train's cadence for `ms`, censusing `row` after
    /// each — the frame the census began on first.
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
    resident: usize,
}

/// The first instant a census reads zero light and never reads light again
/// (`tests/new_line_fade.rs` `dark_at`).
fn dark_at(curve: &[Sample]) -> Option<f32> {
    let last_lit = curve.iter().rposition(|s| s.cov > 0.0)?;
    curve.get(last_lit + 1).map(|s| s.t)
}

/// The first instant the row has no resident cell.
fn empty_at(curve: &[Sample]) -> Option<f32> {
    curve.iter().find(|s| s.resident == 0).map(|s| s.t)
}

/// The largest frame-to-frame DROP in coverage.
fn worst_step(curve: &[Sample]) -> f32 {
    curve
        .windows(2)
        .map(|w| w[0].cov - w[1].cov)
        .fold(0.0, f32::max)
}

/// The largest frame-to-frame RISE in coverage.
fn max_rise(curve: &[Sample]) -> f32 {
    curve
        .windows(2)
        .map(|w| w[1].cov - w[0].cov)
        .fold(0.0, f32::max)
}

/// The census's quantisation noise: a one-pixel boundary rounding at full
/// alpha over the 1350 px row is 7.4e-4 (`tests/new_line_fade.rs`
/// `RISE_TOL`).
const RISE_TOL: f32 = 2e-3;

/// The largest frame-to-frame LEFTWARD move of the centroid, px.
fn worst_left_step(curve: &[Sample]) -> f32 {
    curve
        .windows(2)
        .filter_map(|w| Some(w[0].centroid? - w[1].centroid?))
        .fold(0.0, f32::max)
}

fn fmt(curve: &[Sample]) -> String {
    curve
        .iter()
        .step_by(6)
        .map(|s| {
            format!(
                "{:.2}:{:.3}/{}@{}",
                s.t,
                s.cov,
                s.resident,
                s.centroid.map_or(-1.0, |c| c.round())
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One frame's worth of slack on a bound read at the train's cadence.
const FRAME_S: f32 = FRAME_US as f32 / 1e6;

/// The first text row typed up to the wrap key: the band under it, the
/// pre-wrap census recorded.
fn typed_to_the_wrap() -> (Host, f32, usize, f32) {
    let mut h = Host::new();
    h.type_str(LINE1);
    h.type_str(" WH");
    // The last echo is sampled on the next frame.
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
    // The cells the text carries: the first row's glyphs, columns 2..=84
    // (`LINE1`); the Space the wrap ate at 85 and the moved word at 86..=87
    // stay behind for the re-anchor.
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

/// **(1) THE BAND FOLLOWS ITS TEXT, AND (4) THE COUNTS SAY SO.** One frame
/// after the wrap the band stands on the row the text moved to — the row
/// above — under its own glyphs, at no less than 60 % of the row's
/// pre-wrap coverage, and it is NOT melted at +0.15 s. `ribbon_followed=`
/// counts every cell the text carried; `ribbon_retired=` counts none of
/// them (at most the moved word's two old cells, which the witness finds
/// re-laid on the continuation row).
///
/// RED on the unmodified tree by the mechanism: the witness sampled only
/// the rows holding ribbon cells, saw the old row's glyphs replaced, and
/// melted the run in `RETIRE_MELT_S` — row 27 (0-based 26) was never lit
/// and `ribbon_retired=` counted the whole band.
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
    // Not melted: well past `RETIRE_MELT_S`, the row still carries most of
    // its light.
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

/// **(2) THE FOLDED ROW FLOWS INTO THE FOLD.** From the wrap frame the
/// followed row's light moves in the direction of typing: its lit-column
/// centroid never steps left, its coverage never rises, it is still lit at
/// +0.5 s, its far (left) end goes first, and it is dark by
/// `FLOW_TOTAL_S` plus a frame — never the 120 ms cut.
///
/// RED on the unmodified tree by the mechanism: the row was melted where
/// it stood (the 0.12 s cut, `worst_step` a fifth of the coverage a frame)
/// and nothing was lit at +0.5 s.
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
    assert!(
        worst_left_step(&curve) <= 0.5,
        "the light's centroid moves monotonically toward the fold (right), never left \
         by more than half a pixel: {shown}"
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
    // The far end goes first: by +0.3 s the left edge has moved right.
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
        dark >= 0.9 * FLOW_TOTAL_S,
        "…and not sooner than the flow's own span: dark at {dark}"
    );
}

/// **(3) THE NEW ROW'S WALK HAS NO STEP.** After the wrap and a few more
/// keys, the continuation row's cells — the relaid `WH`, the reused `Y`,
/// and the keys typed since — carry a `t` that is monotone at the walk's
/// pace (no backward step; adjacent forward steps at most a sixteenth),
/// and the relaid word's first cell has exactly the `t` its column had on
/// the old row.
///
/// RED on the unmodified tree by the mechanism: the wrap key's glyph
/// minted a cohort at its own column and the relaid word walked UP from
/// that cohort's `t0` — a backward step of `w/16` at column `inset + w`,
/// the owner's hard edge.
#[test]
fn the_new_rows_cells_carry_a_monotone_walk_and_the_relaid_word_keeps_the_t_it_had() {
    let (mut h, _, _, t_w) = typed_to_the_wrap();
    h.wrap(LINE1);
    h.next_frame();
    h.type_str(AFTER);
    h.next_frame();
    h.idle(100);
    let rib = h.ribbon();
    let last = 4 + AFTER.len() as u16;
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
    for w in ts.windows(2) {
        let (c0, t0) = w[0];
        let (c1, t1) = w[1];
        assert!(
            t1 >= t0 - 1e-5,
            "a backward step from {t0} at column {c0} to {t1} at column {c1}: {ts:?}"
        );
        assert!(
            t1 - t0 <= 1.0 / WALK_FAST_CELLS + 1e-4,
            "a step wider than the walk's pace from {t0} at column {c0} to {t1} at column \
             {c1}: {ts:?}"
        );
    }
    let (_, t_first) = ts[0];
    assert!(
        (t_first - t_w).abs() < 1e-5,
        "the relaid word's first cell has exactly the t its column had on the old row: \
         {t_first} against {t_w}: {ts:?}"
    );
}

/// **(5) THE NEGATIVE CONTROL: A TRUE RE-LAYOUT STILL MELTS.** The same
/// bytes, but the text row is rewritten one row up with DIFFERENT text: the
/// old run's glyphs are found nowhere, nothing follows, and the old cells
/// retire on the melt exactly as before — `ribbon_retired=` counts the
/// band, `ribbon_followed=` stays 0, and the row the text went to is dark.
/// GREEN on the unmodified tree and green here: the stray fix is intact.
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

/// **THE UNWRAP CHUNK**, modelled on the wrap's own shape (not captured):
/// the Backspace that erases the continuation row's last glyph lets the
/// line fit one row again, and the box is repainted one row LOWER — the
/// row the rule stood on cleared, the rule back on row 27, the text row 28
/// holding the whole line, the caret after it.
fn unwrap_bytes(line: &str) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[{};1H\x1b[K\x1b[{RULE_ROW};1H{}\x1b[{TEXT_ROW};1H\u{276f}\u{a0}{line}\x1b[K\x1b[30;1H\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        RULE_ROW - 1,
        rule(),
        3 + line.chars().count()
    )
    .into_bytes()
}

/// **(6) A BACKSPACE BACK THROUGH THE FOLD RE-WETS THE ROW THE HAND CAME
/// BACK TO.** After the wrap, a Backspace erases the continuation row and
/// the box is repainted one row lower with the whole line on the text row
/// again: the follow pass carries the flowing band back DOWN onto the
/// caret's row, and the hand keeps backspacing there. That row is the
/// hand's again — it must hold its light while the hand works on it, as
/// the same Backspace run does on a line that never wrapped (the control),
/// and must not drain away as the flowing orphan it was a row up. Run with
/// the unwrap's echo on the Backspace's own frame and one frame late.
///
/// RED on the integration tree before 2026-09-22: on the same frame the
/// follow pass placed the band on the caret's row BEFORE the Backspace was
/// replayed at the continuation row's column 5, whose suffix retract then
/// stamped 83 of the 86 cells (the row's left 40 columns fell 0.43 → 0.034
/// against the control's 0.395 floor); one frame late the band arrived
/// still FLOWING under the hand and slid away into a fold that was gone.
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
    // The control: the same line, the same Backspace run, no round trip.
    let (mut c, _, _, _) = typed_to_the_wrap();
    c.idle(160);
    let ctl = backspace_run(&mut c, 3 + line.chars().count(), 30);
    let floor = |v: &[(u64, f32)]| v[6..].iter().map(|s| s.1).fold(f32::MAX, f32::min);
    let want = floor(&ctl);
    for late in [false, true] {
        // The fold's round trip: wrap, then the Backspace that unwraps.
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
