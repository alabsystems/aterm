// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ABANDONED BAND (2026-09-12) — the owner, on the shipped 0.83.0, with a
//! screenshot of Claude Code's input box: *"fix this ugly new-line bug for
//! the rainbow trail. the cursor trail should not paint like that in a
//! 'static way'"* — a long line wrapped, the caret typing on the second row,
//! and the FIRST row still carrying a flat, full-spectrum, full-brightness
//! band to the right edge, frozen. Earlier: *"the stray rainbows are the
//! cursor trails that sometimes get 'lost' or abandoned in complex CLI tools
//! like codex"*.
//!
//! Since `1e3e4a31b` an erase invalidates no host coordinate (D2 — a prompt
//! redraw must not wipe the ribbon), and v2 had no content-aware retirement
//! at all: the ribbon is an absolute-cell record of the keys, and when a TUI
//! re-laid its input box the TEXT moved and the band stayed. The screenshot
//! adds the plainest case of all — no relocation, just a WRAP: the one-finger
//! law held every cohort alive while any key was live, so the row the caret
//! had left was renewed by every key typed below it and kept full light for
//! as long as the hand typed. Four mechanisms, each closed here at the HOST
//! seam — a real `aterm_core::terminal::Terminal` driven byte for byte, its
//! rows sampled exactly as `app_render.rs`'s frame hold samples them
//! (`row_cols_into` under the term lock, after the batch is applied), fed to
//! `CursorGlow::observe_row` / `observe_ribbon_row` and ticked through
//! `CursorGlow::tick` — the seam `tick_cursor_fx` drives:
//!
//! 1. the one-finger law renewed a row the caret had left from the new row
//!    — now renewal is ROW-SCOPED (`Ribbon::place`): the earlier row keeps
//!    the clock it had and leaves through its own swoosh;
//! 2. an unlicensed relocation stranded the band on the abandoned row
//!    (`declined no-fresh-hint (41,2)->(40,2)`) — now the CONTENT WITNESS
//!    retires a cell whose glyph has changed or gone (`rk::witness`);
//! 3. the caret MIRROR was written only by a licensed move, so the next key
//!    after a declined relocation laid at the stale mirror — now the mirror
//!    follows an observed ROW change (`Engine::observe_caret`), composed
//!    with the echo ledger (a same-row hop stays the ledger's to pay);
//! 4. the box-growth wrap (`licensed (41,116)->(41,7)`, a typed re-anchor):
//!    the text moved up a row while the caret stayed — the witness sees the
//!    row's glyphs go.
//!
//! Every scenario below was RED on main at the assertion its comment names.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::ribbon::{
    FLOW_TOTAL_S, PHRASE_REST_MIN_S, RETRACT_DUR_S, RETRACT_FADE_S, SWOOSH_TOTAL_S, WALK_LAY_RATE,
};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;

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

/// The host: one terminal, one glow engine, one clock.
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
    /// The content-scroll state the last frame saw (`app_render.rs`'s
    /// `sync_cursor_effect_scroll`), so a fold on the screen's last row
    /// translates the ribbon with the text (2026-09-23).
    scroll: Option<ContentScrollState>,
}

impl Host {
    /// A fresh host with the caret parked at 0-based `(row, 0)` and the
    /// anchor seeded by one frame.
    fn at_row(row: u16) -> Self {
        Self::sized(ROWS as u16, COLS as u16, row)
    }

    /// A fresh `rows × cols` host with the caret parked at 0-based `(row,
    /// 0)` and the anchor seeded by one frame.
    fn sized(rows: u16, cols: u16, row: u16) -> Self {
        let mut term = Terminal::new(rows, cols);
        term.process(format!("\x1b[{};1H", row + 1).as_bytes());
        let now = Instant::now();
        let mut h = Self {
            term,
            glow: CursorGlow::default(),
            cfg: cfg(),
            g: Geom {
                rows: usize::from(rows),
                cols: usize::from(cols),
                win_w: cols * CW as u16,
                win_h: rows * CH as u16,
                ..geom()
            },
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            last_key: now,
            scroll: None,
        };
        h.frame();
        h
    }

    /// EXACTLY the frame hold, then the tick: the host's scroll sync
    /// (`sync_cursor_effect_scroll`, as `tests/codex_particle_replay.rs`
    /// drives it), then sample the cursor, the repaint blink, the caret
    /// row's probe, and the rows the resident ribbon occupies — all from
    /// the terminal AFTER the last `process` — then advance the engine one
    /// frame.
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
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        // The caret row rides the probe the host already holds…
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        // …and the ribbon's own rows are read beside it.
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            self.glow.observe_ribbon_row(r, &self.row_buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
    }

    /// Idle frames at 16 ms until `ms` have passed.
    fn idle(&mut self, ms: u64) {
        let end = self.now + Duration::from_millis(ms);
        while self.now < end {
            self.now += Duration::from_millis(16);
            self.frame();
        }
    }

    /// One keypress 90 ms after the last: the app_input seam arms the typed
    /// hint (priced at `cells`), the PTY echoes `bytes`, the frame presents.
    fn key(&mut self, bytes: &[u8], cells: u16) {
        self.now += Duration::from_millis(90);
        self.last_key = self.now;
        self.glow.note_typed_cells(self.now, cells);
        self.term.process(bytes);
        self.frame();
    }

    /// Type `s` a key at a time. The harness types ASCII and CJK only, so
    /// a non-ASCII char is priced at its two cells — the host's own price
    /// for a wide glyph (`note_typed_cells`).
    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            let cells = if ch.is_ascii() { 1 } else { 2 };
            let mut buf = [0u8; 4];
            self.key(ch.encode_utf8(&mut buf).as_bytes(), cells);
        }
    }

    /// One NAVIGATION keypress 90 ms after the last (an arrow, Home/End):
    /// the app_input seam arms the nav hint, the PTY echoes the caret move
    /// `bytes`, the frame presents. Not a typed key: it renews nothing.
    fn nav(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(90);
        self.glow.note_navigation(self.now);
        self.term.process(bytes);
        self.frame();
    }

    /// A PTY batch with no key behind it, presented on one frame.
    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

    /// Resident ribbon cells on `row`: `(col, leaving)`.
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

    /// Every resident cell at one position — there can be MORE than one
    /// (one owner per cell holds per cohort, and a key typed back over a
    /// cell an abandoned cohort still owns mints a second): `(born,
    /// leaving)`, oldest first.
    fn cells_at(&self, row: u16, col: u16) -> Vec<(Instant, bool)> {
        let mut v: Vec<(Instant, bool)> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .iter()
            .filter(|c| c.row == row && c.col == col)
            .map(|c| (c.born, c.leaving()))
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

    /// Whether any BODY quad covers the CENTRE of `row`'s pixel band this
    /// frame. The centre, not the damage-row tag: the tall body reaches
    /// `1.10 ch` above its spine, a tenth of a cell into the row above,
    /// and that overshoot is tagged with the row it touches — a band on row
    /// 6 must not read as light on row 5.
    fn lit(&self, row: u16) -> bool {
        let centre = f32::from(self.g.origin_y) + (f32::from(row) + 0.5) * CH as f32;
        self.glow.under_quads().iter().any(|q| {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            q.w > 0 && q.alpha > 0 && y0 <= centre && centre < y1
        })
    }

    fn retired(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.retired)
    }

    /// `ribbon_followed=`: the cells the follow pass carried to another
    /// row with their text (2026-09-21).
    fn followed(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.followed)
    }

    /// Seconds since the last key — the guards below use it to prove the
    /// band was still inside its own grace (no swoosh could have taken it).
    fn since_key(&self) -> f32 {
        self.now
            .saturating_duration_since(self.last_key)
            .as_secs_f32()
    }
}

const GRACE_S: f32 = PHRASE_REST_MIN_S;
const TEXT: &str = "hello world";

fn hello(row: u16) -> Host {
    let mut h = Host::at_row(row);
    h.type_str(TEXT);
    let want: Vec<u16> = (0..TEXT.len() as u16).collect();
    assert_eq!(h.live(row), want, "the typed band is laid under the text");
    assert!(h.lit(row));
    h
}

/// **THE SCREENSHOT, flavour (a): a plain shell wrap.** A line typed past
/// the right margin at 90 ms a key; the caret advances to the next row and
/// the hand keeps typing there. The first row's band must stop being renewed
/// the moment the caret leaves it and go out on ITS OWN clock — the swoosh,
/// `SWOOSH_TOTAL_S` after its last key — while the second row's band stays
/// under the hand. RED before 2026-09-12 at "row 5 is dark by …": every key
/// on row 6 renewed row 5's cohort, so the first row held a flat full band
/// for as long as the hand typed below it.
///
/// THE RULING RECORDED (merged 2026-09-14). Rainbow Path v3 §2.3 (S6) would
/// hold the whole wrapped line as ONE PHRASE, and the step-5 lane re-pinned
/// this test that way. The merge does not take that half: the row scope is
/// model-checked (`aterm-spec`'s `ribbon_row_hold_model`, invariant
/// `OnlyOwnerRenews`, whose named falsifier is a global hold) and it is the
/// shipped answer to the owner's 0.83.0 screenshot. What v3 DOES move here
/// is the clock the bound is read against: the grace is the phrase rest
/// (0.90 s at the floor, not a 0.75 s literal), so the swoosh is
/// `SWOOSH_TOTAL_S` = 1.69 s, and both numbers are derived rather than
/// written down.
///
/// THE FOLD FLOW (re-pinned 2026-09-21 — the owner: *"the previous row's
/// rainbow vanishes suddenly … a beautiful animation where the previous
/// row's rainbow flows in the direction of typing"*). The row the hand
/// wrapped off no longer holds still through its grace: `leave_row` marks
/// it flowing (`Cohort::flow`) and it drifts into the fold point from its
/// own last key, fading as it goes — gone by `FLOW_TOTAL_S` (1.50 s since
/// the drift of 2026-09-23), moving throughout. The row scope is untouched: no key on row 6
/// renews it, and the bound below is the flow's own span.
#[test]
fn a_wrapped_line_s_first_row_goes_out_on_its_own_clock_while_the_hand_types_on_the_second() {
    let mut h = Host::at_row(5);
    // Fill row 5 to the margin: the 80th key lands at column 79 and the
    // caret stays there with the wrap pending; the 81st glyph opens row 6.
    for i in 0..COLS {
        h.key(&[b'a' + (i % 26) as u8], 1);
    }
    let c = h.term.cursor();
    assert_eq!(c.row, 5, "eighty keys: the caret is still on row 5");
    assert!(h.lit(5), "row 5 is lit under the hand");
    let left_row_5 = h.now;
    // Twenty-six more keys on row 6 — 90 ms apart, well past the swoosh.
    let mut row_5_dark_at: Option<f32> = None;
    let mut row_5_lit_at_grace = false;
    for i in 0..26u16 {
        h.key(&[b'a' + (i % 26) as u8], 1);
        let c = h.term.cursor();
        assert_eq!(c.row, 6, "the wrap: the caret is on row 6");
        let since = h.now.saturating_duration_since(left_row_5).as_secs_f32();
        assert!(h.lit(6), "row 6 is lit under the hand at +{since:.2} s");
        if since < (RETRACT_DUR_S + RETRACT_FADE_S) * 0.9 {
            // The wrap sets the row the caret left FLOWING (`leave_row`,
            // 2026-09-21): it slides into the fold from its own last key
            // and must keep light on the glass the whole way down — no
            // early cut — for well past the old retract's span.
            assert!(
                h.lit(5),
                "row 5 keeps its light through the flow's first half (+{since:.2} s)"
            );
            row_5_lit_at_grace = true;
        }
        if row_5_dark_at.is_none() && h.cells(5).is_empty() && !h.lit(5) {
            row_5_dark_at = Some(since);
        }
    }
    assert!(row_5_lit_at_grace, "the flow's first half was observed");
    let dark = row_5_dark_at.expect(
        "row 5 must go dark while the hand is still typing on row 6 — a global hold          keeps it at full light for as long as any key is live",
    );
    // THE BOUND (re-pinned 2026-09-21, the fold flow): the row's own flow,
    // `FLOW_TOTAL_S` (1.50 s since the drift of 2026-09-23) from the wrap
    // key (one key's cadence after the row's last key), and not a frame
    // later than one key's cadence past it (the read is once per key, 90 ms
    // apart). Until 2026-09-21 this read `RETRACT_DUR_S + RETRACT_FADE_S
    // ..= SWOOSH_TOTAL_S + 0.1` (0.64 .. 1.79 s): the folded row held still
    // for its 0.90 s grace and then left through the swoosh — the owner's
    // "vanishes suddenly".
    assert!(
        (FLOW_TOTAL_S..=FLOW_TOTAL_S + 0.09 + 0.1).contains(&dark),
        "row 5 went out at +{dark:.2} s after the caret left it; the bound is the \
         fold flow's own span, {FLOW_TOTAL_S} s from the wrap key, and never past \
         one key's cadence after it"
    );
    // THE ABSOLUTE CEILING, AND WHY IT MOVED. This read 1.6 s while the swoosh
    // was a 1.54 s literal. The phrase rest is now the melody's — 0.90 s at 2.8
    // characters a second or faster, where it was a 0.75 s literal — and the
    // swoosh is DERIVED from it (rest + 0.79 s = 1.69 s at the floor rest), so
    // the old number would fail on arithmetic rather than on any regression.
    // The ceiling is restated at the new derived value plus the same margin the
    // old one carried, and it is still an absolute bound on how long a row the
    // caret has left may hold light: it is not a function of the constant it
    // guards, so shortening the rest cannot silently relax it.
    assert!(
        (SWOOSH_TOTAL_S * 1000.0) as u64 <= 1750,
        "the pinned bound: a row the caret left is dark inside 1.75 s"
    );
    assert!(h.lit(6), "…and the live row's band is untouched");
    assert!(
        h.live(6).len() >= 20,
        "the live row holds the keys typed on it: {:?}",
        h.live(6)
    );
}

/// `(col, t)` of every live (not leaving) cell on `row`, by column.
fn walk_on(h: &Host, row: u16) -> Vec<(u16, f32)> {
    let mut v: Vec<(u16, f32)> = h
        .glow
        .v2_ribbon()
        .expect("rainbow kitty owns the frame")
        .cells()
        .iter()
        .filter(|c| c.row == row && !c.leaving())
        .map(|c| (c.col, c.t))
        .collect();
    v.sort_unstable_by_key(|&(col, _)| col);
    v
}

/// Row 5 typed to the pane's right edge (80 keys at 90 ms; the 80th glyph
/// stands at column 79 under a pending wrap), and the walk's pace at the
/// row's end — asserted to be `WALK_LAY_RATE`, the premise both fold laws
/// below stand on: 80 cells from the row's first is far past the walk's
/// `d/16` fast phase.
fn typed_to_the_margin() -> (Host, f32) {
    let mut h = Host::at_row(5);
    for i in 0..COLS {
        h.key(&[b'a' + (i % 26) as u8], 1);
    }
    assert_eq!(h.term.cursor().row, 5, "eighty keys: the caret is on row 5");
    let above = walk_on(&h, 5);
    assert!(above.len() >= 60, "row 5 is laid under the text: {above:?}");
    let n = above.len();
    let (c_prev, t_prev) = above[n - 2];
    let (c_last, t_last) = above[n - 1];
    let pace = (t_last - t_prev) / f32::from(c_last - c_prev);
    assert!(
        (pace - WALK_LAY_RATE).abs() < 1e-4,
        "the premise: row 5 lays at WALK_LAY_RATE at its end — {t_prev} @{c_prev}, \
         {t_last} @{c_last}"
    );
    (h, pace)
}

/// **THE SCREENSHOT OF 2026-09-23, flavour (a): a plain shell fold keeps the
/// walk's PACE.** The owner, on v0.91.0: *"fix this rainbow cursor issue
/// where the spectrum is smooshed on the next line. I want smooth continuous
/// rainbow"* — a paragraph wrapped, and the new row's first ~22 cells carried
/// the whole arc. His was Claude Code's composer
/// (`tests/composer_box_growth_wrap.rs`, law 3b); this is the same defect
/// with no composer at all: 80 keys fill row 5 to the pane's right edge and
/// the terminal itself wraps the 81st onto column 0 of row 6, where the hand
/// types 30 more.
///
/// Row 5's walk ran 80 cells from its first, so at its end it was laying at
/// `WALK_LAY_RATE` a cell ([`typed_to_the_margin`]'s premise). Row 6's
/// cohort is minted by `join_cohort` on the continued `t`, so the new row
/// must continue the PACE too: every adjacent step on row 6, from column 0
/// through column 29 (past the walk's sixteen-cell fast phase), is the step
/// row 5 ended on, to 1e-4.
///
/// RED on the unmodified tree by the mechanism: the new cohort's
/// `anchor_col` is its own first column, and `Cohort::t_at` reads
/// `t0 + walk_t(col − anchor_col)` — the walk's distance restarts at zero,
/// so row 6's first sixteen cells step `1/16` a cell, 2.25× row 5's `1/36`:
/// one full sweep of the arc squeezed into the sixteen cells after the fold.
#[test]
fn a_shell_fold_continues_the_walk_at_the_pace_the_row_above_had_reached() {
    let (mut h, pace) = typed_to_the_margin();
    for i in 0..30u16 {
        h.key(&[b'a' + (i % 26) as u8], 1);
    }
    assert_eq!(h.term.cursor().row, 6, "the fold: the caret is on row 6");
    let below = walk_on(&h, 6);
    assert!(
        below.len() >= 24 && below[0].0 == 0,
        "row 6 is laid from column 0 past the walk's fast phase: {below:?}"
    );
    for w in below.windows(2) {
        let (c0, t0) = w[0];
        let (c1, t1) = w[1];
        assert!(
            c1 == c0 + 1 && (t1 - t0 - pace).abs() < 1e-4,
            "row 6's step from column {c0} to {c1} is {} — row 5 laid at {pace} a cell \
             at its end; the walk restarted its pace at the fold: {below:?}",
            t1 - t0
        );
    }
}

/// **THE FOLD'S SEAM IS ONE STEP, NOT A REPEATED STOP** (2026-09-23, found
/// while pinning the pace law above). On the frame the 81st key folds onto
/// row 6, BOTH its cell `(6,0)` and the 80th key's pending-wrap cell
/// `(5,79)` are laid — and row 6's first cell must be row 5's last glyph
/// continued ONE step, `t(5,79) + WALK_LAY_RATE`: the colour crosses the
/// fold exactly as it crosses any two adjacent cells on a row.
///
/// RED on the unmodified tree by a SECOND mechanism, independent of the
/// pace: `(6,0)` is laid FIRST on that tick (pool order), so `join_cohort`
/// mints row 6's cohort from row 5's `t_at(col1)` while `col1` is still 79
/// — and then `(5,79)` joins row 5's cohort at that same `t_at(79)`. The
/// two glyphs either side of the fold carry the SAME stop (measured `2.75`
/// and `2.75`): a zero step. The walk odometer alone (`Cohort::d0`) does
/// not change `t0`, and left this law red; `Ribbon::seat_fold_successor`
/// closes it by re-seating row 6's cohort when the glyph above it lands.
#[test]
fn a_shell_fold_s_first_cell_is_the_row_above_s_last_glyph_continued_one_step() {
    let (mut h, pace) = typed_to_the_margin();
    h.key(b"a", 1);
    assert_eq!(h.term.cursor().row, 6, "the fold: the caret is on row 6");
    let above = walk_on(&h, 5);
    let below = walk_on(&h, 6);
    let (c_last, t_last) = *above.last().expect("row 5 is laid");
    let (c_first, t_first) = *below.first().expect("row 6's first cell is laid");
    assert_eq!(
        (c_last, c_first),
        (COLS as u16 - 1, 0),
        "the fold's two glyphs are laid: row 5 {above:?}, row 6 {below:?}"
    );
    let want = t_last + pace;
    assert!(
        (t_first - want).abs() < 1e-4,
        "the colour crosses the fold one step at a time: row 6's first cell is \
         {t_first}, row 5's last glyph ({t_last} @{c_last}) continued one step is {want}"
    );
}

/// Whether a cohort on `row` is flowing (`Cohort::flow`: the fold drift).
fn flowing(h: &Host, row: u16) -> bool {
    h.glow
        .v2_ribbon()
        .expect("rainbow kitty owns the frame")
        .cohorts()
        .iter()
        .any(|k| k.row == row && k.flow.is_some())
}

/// Frames at 16 ms, the hand parked, until `row` is dark: the ms from
/// `since` to the first frame with no light on it — `None` if it is still
/// lit after 3 s. `flowed` is set if any frame on the way found a cohort on
/// the row flowing.
fn dark_after(h: &mut Host, row: u16, since: Instant, flowed: &mut bool) -> Option<u64> {
    let end = h.now + Duration::from_millis(3000);
    while h.now < end {
        h.now += Duration::from_millis(16);
        h.frame();
        *flowed |= flowing(h, row);
        if !h.lit(row) {
            return Some(h.now.saturating_duration_since(since).as_millis() as u64);
        }
    }
    None
}

/// **A BACKSPACE BACK UP THROUGH A SHELL FOLD HOLDS THE ROW UNDER THE PARKED
/// HAND** (2026-09-25, the second review of the drift). Row 5 typed to the
/// margin, `x` folds onto `(6, 0)` — row 5 starts its drift — then two
/// Backspaces 90 ms apart: the first erases the `x` (`\b ESC[K`), the
/// second goes up through the fold (the shell's `ESC[A ESC[80G ESC[K`, the
/// caret at `(5, 79)`), and the hand parks on row 5. The row the hand came
/// back to must stop flowing on the frame the echo lands and stay lit at
/// least as long after that Backspace as a row that never folded does after
/// the same Backspace (the control) — whether the echo lands with its key
/// (the engine then replays the erase ON row 5 and its hold re-wets it) or
/// a frame later (the erase is replayed at `(6, 0)`, and only the hand's
/// arrival on row 5 can take it back, `Ribbon::take_row_back`), and
/// whether the run begins at once or after the phrase rest (the first
/// Backspace, on row 6, then mints a phrase and adopts row 6 alone, so the
/// second's erase ON row 5 holds a phrase row 5's cohort is not in).
///
/// Measured by the review on the tree before the take-back: the control
/// dark +1680 ms after its Backspace; the fold with the echo in the key's
/// frame dark +1680 (+1360 with the hold's re-wet removed), a frame late
/// still flowing and dark +1152 — about 0.53 s early, under the parked
/// caret. RED without the take-back for the late echo, and for both echo
/// shapes after the rest (row 5 flowing at the echo's frame).
#[test]
fn a_backspace_back_up_through_a_shell_fold_holds_the_row_under_the_parked_hand() {
    for pause_ms in [0u64, 1000] {
        // The control: a row that never folded, the same Backspace after
        // the same idle since the row's last key.
        let control = {
            let mut h = Host::at_row(5);
            for i in 0..79u8 {
                h.key(&[b'a' + i % 26], 1);
            }
            h.now += Duration::from_millis(180 + pause_ms);
            let bs = h.now;
            h.glow.note_backspace(h.now);
            h.term.process(b"\x08\x1b[K");
            h.frame();
            let mut flowed = false;
            let dark =
                dark_after(&mut h, 5, bs, &mut flowed).expect("the control goes dark inside 3 s");
            assert!(!flowed, "the control never flows");
            dark
        };
        for late in [false, true] {
            let what = format!(
                "{} ms pause, echo {}",
                pause_ms,
                if late { "a frame late" } else { "with its key" }
            );
            let (mut h, _) = typed_to_the_margin();
            h.key(b"x", 1);
            assert_eq!(h.term.cursor().row, 6, "{what}: `x` folds onto row 6");
            let bs1 = h.now + Duration::from_millis(90 + pause_ms);
            while h.now + Duration::from_millis(16) < bs1 {
                h.now += Duration::from_millis(16);
                h.frame();
            }
            assert!(
                flowing(&h, 5) && h.lit(5),
                "{what}: the premise: row 5 is flowing, and lit, when the hand comes back"
            );
            let rest = h.glow.v2_ribbon().expect("the ribbon").rest_s();
            assert!(
                pause_ms == 0 || (90 + pause_ms) as f32 >= rest * 1000.0,
                "{what}: the premise: the run begins past the {rest} s rest"
            );
            h.now = bs1;
            h.glow.note_backspace(h.now);
            h.term.process(b"\x08\x1b[K");
            h.frame();
            assert_eq!(
                (h.term.cursor().row, h.term.cursor().col),
                (6, 0),
                "{what}: the first Backspace erased the `x`"
            );
            h.now += Duration::from_millis(90);
            let bs2 = h.now;
            h.glow.note_backspace(h.now);
            if late {
                h.frame();
                h.now += Duration::from_millis(16);
            }
            h.term.process(b"\x1b[A\x1b[80G\x1b[K");
            h.frame();
            assert_eq!(
                (h.term.cursor().row, h.term.cursor().col),
                (5, 79),
                "{what}: the second Backspace went up through the fold"
            );
            assert!(
                !flowing(&h, 5),
                "{what}: row 5 still flows on the frame the hand came back to it"
            );
            let mut flowed = false;
            let dark = dark_after(&mut h, 5, bs2, &mut flowed);
            assert!(!flowed, "{what}: row 5 flowed again under the parked hand");
            let dark = dark.expect("row 5 goes dark inside 3 s");
            assert!(
                dark >= control,
                "{what}: row 5 went dark +{dark} ms after the hand came back to it; the \
                 never-folded control holds to +{control} ms"
            );
        }
    }
}

/// Row `row` of a `rows × cols` host typed to the pane's right edge, one
/// key per frame at 90 ms: `(host, t, pace)` — the stop of the column
/// BEFORE the margin (the margin glyph's own key is held under the pending
/// wrap, and comes with the fold's sweep) and the walk's pace there, read
/// off its last two cells.
fn typed_to_the_margin_of(rows: u16, cols: u16, row: u16) -> (Host, f32, f32) {
    let mut h = Host::sized(rows, cols, row);
    for i in 0..cols {
        h.key(&[b'a' + (i % 26) as u8], 1);
    }
    assert_eq!(h.term.cursor().row, row, "the caret is still on row {row}");
    let above = walk_on(&h, row);
    let n = above.len();
    assert!(
        n >= usize::from(cols) - 2 && above[n - 1].0 == cols - 2,
        "row {row} is laid up to the column before the margin: {above:?}"
    );
    let (c_prev, t_prev) = above[n - 2];
    let (c_last, t_last) = above[n - 1];
    (h, t_last, (t_last - t_prev) / f32::from(c_last - c_prev))
}

/// **THE FOLD'S SEAM HOLDS WHEN TWO KEYS' ECHOES SHARE A FRAME** (2026-09-23,
/// the audit of `Ribbon::seat_fold_successor`). The sibling of
/// [`a_shell_fold_s_first_cell_is_the_row_above_s_last_glyph_continued_one_step`],
/// which types one key per frame: here the fold key and the key after it
/// reach the glass together — pressed 6 ms apart and echoed on one frame
/// (a fast hand), or the fold key's echo lagging 40 ms until the next
/// key's lands with it (a slow link). The frame then replays the LATER
/// key's `Typed` at the frame's caret, `(r + 1, 2)`, which lays `(r + 1,
/// 1)` first and mints the new row's run there from the row above's end
/// one column short of the glyph still to come — and the seat that fixes
/// the one-key shape only looked at a successor anchored at the pane's
/// first column. The new row's first cell must still be the row above's
/// last glyph continued ONE step, and its second one more: at 80 columns
/// mid-screen, at 80 columns on the screen's LAST row (the fold scrolls
/// the screen, and the host's scroll sync carries the ribbon), and at 12
/// columns, where the walk is still on its `1/16` fast leg.
///
/// RED before 2026-09-23 (measured by the audit, and here): every fold
/// stepped BACKWARD one stop, `−1/36` (`−1/16` at 12 columns) — row `r +
/// 1`'s first two cells took the stops of row `r`'s last two.
#[test]
fn a_fold_s_seam_holds_when_two_keys_echoes_land_on_one_frame() {
    for (rows, cols, row) in [(24u16, 80u16, 5u16), (24, 80, 23), (24, 12, 5)] {
        for lagged in [false, true] {
            let what = format!(
                "{cols} columns on row {row}, {}",
                if lagged {
                    "the fold key's echo lagging to the next key's"
                } else {
                    "two keys 6 ms apart on one frame"
                }
            );
            let (mut h, t_before, pace) = typed_to_the_margin_of(rows, cols, row);
            // The margin glyph continues the row one step.
            let t_last = t_before + pace;
            h.now += Duration::from_millis(90);
            h.last_key = h.now;
            h.glow.note_typed_cells(h.now, 1);
            if lagged {
                // The fold key's echo is late: two frames with nothing new.
                h.now += Duration::from_millis(16);
                h.frame();
                h.now += Duration::from_millis(16);
                h.frame();
                h.now += Duration::from_millis(8);
            } else {
                h.term.process(b"a");
                h.now += Duration::from_millis(6);
            }
            h.last_key = h.now;
            h.glow.note_typed_cells(h.now, 1);
            h.term.process(if lagged { b"ab" } else { b"b" });
            h.frame();
            // The fold scrolled the screen on the last row: the row the
            // glyphs were typed on is one up now.
            let (above_row, below_row) = if row + 1 == rows {
                (row - 1, row)
            } else {
                (row, row + 1)
            };
            assert_eq!(h.term.cursor().row, below_row, "{what}: the fold");
            for _ in 0..4u16 {
                h.key(b"c", 1);
            }
            let above = walk_on(&h, above_row);
            let below = walk_on(&h, below_row);
            let &(c_above, t_above) = above.last().expect("the row above is laid");
            assert!(
                c_above == cols - 1 && (t_above - t_last).abs() < 1e-4,
                "{what}: the margin glyph continues its row one step, {t_last}: {above:?}"
            );
            assert!(
                below.len() >= 4 && below[0].0 == 0 && below[1].0 == 1,
                "{what}: the new row is laid from its first column: {below:?}"
            );
            // Four cells: at 12 columns they are distance 12..=15, still
            // on the fast leg the margin's pace was read on.
            for (k, &(col, t)) in below.iter().take(4).enumerate() {
                let want = t_last + (k as f32 + 1.0) * pace;
                assert!(
                    (t - want).abs() < 1e-4,
                    "{what}: the new row's column {col} is {t} — the margin glyph {t_last} \
                     continued {} steps of {pace} is {want}: above {above:?}, below {below:?}",
                    k + 1
                );
            }
        }
    }
}

/// **THE SCREENSHOT, flavour (b): Claude Code's box-growth wrap, then typing
/// on.** The key that grows the box re-lays line 1 a row UP while the caret
/// stays on its row and jumps left (`licensed (41,116)->(41,7)`); the hand
/// keeps typing on the caret row. The old band's cells are under blanks now
/// and go on the melt; the row the text moved TO was never typed on and
/// stays dark; and the live row never holds a static full band — only the
/// keys typed on it since. RED on main at "the old band's cells, now blank,
/// are retired": the whole band stayed on the caret row while its text lived
/// a row above, and every later key renewed it.
///
/// **THE PARK DEFERS THE VERDICT, NOT THE RETIREMENT** (`ca54aaadd`, merged
/// under this port): a same-row backward typed-paired move is HELD for one
/// stamp window in case it is Ink's park-and-return, so the wrap's own
/// `Move` — and with it the caret mirror the key's `Typed` replay folds
/// back from — arrives one observed move (or ≤ 0.25 s) later. The
/// RETIREMENT does not wait on it: the witness reads the host's rows on the
/// growth frame itself, so the band is on the melt at once — which is the
/// owner's defect. What the flush owes is the landing, and since the
/// re-anchor's one-cell sweep it pays it: the next key flushes the park and
/// the growth key's own glyph lights beside it.
///
/// **RE-PINNED 2026-09-21 — THE BAND FOLLOWS ITS TEXT.** The growth key's
/// rewrite is exactly the shape the follow pass (`rk::witness`,
/// `Engine::follow_rows`) was built for: every glyph of `hello world` is
/// gone from row 6 and stands one row up at its own column. So the band is
/// no longer melted where its text WAS; it is TRANSLATED to row 5 under
/// its text on the frame the box grew, with every clock intact and nothing
/// retired, and — the text having left the caret's row, which is the fold
/// — it FLOWS there: it drifts into the fold point and fades as it goes,
/// gone by `FLOW_TOTAL_S`, while the hand
/// types on the caret row. The park's own laws are unchanged: the wrap's
/// verdict is still held and flushed by the next key, which lays the
/// landing. The owner's vanish (2026-09-21) was this fixture's old
/// expectation on his glass.
#[test]
fn the_box_growth_wrap_carries_the_band_up_with_its_text_and_the_flush_pays_its_landing() {
    let mut h = hello(6);
    // The key that grows the box: line 1 is re-laid a row up, line 2 (this
    // key's glyph) on the caret's row, the caret after it.
    h.key(b"\x1b[6;1Hhello world\x1b[7;1H\x1b[2Kx", 1);
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (6, 1),
        "the caret stayed on its row and re-anchored left"
    );
    let want: Vec<u16> = (0..11).collect();
    assert!(
        h.cells(6).is_empty(),
        "the band left the caret row with its text, unstamped: {:?}",
        h.cells(6)
    );
    assert_eq!(
        h.live(5),
        want,
        "…and stands under its text one row up, every cell and the space between"
    );
    assert_eq!(h.retired(), 0, "nothing was retired by content");
    assert_eq!(h.followed(), 11, "every cell followed its text");
    assert!(h.lit(5), "the row the text moved to carries the band");
    assert_eq!(
        h.glow.in_flight_tally().park_flushed,
        0,
        "…held, not judged: nothing flushed yet"
    );
    let flowing = h
        .glow
        .v2_ribbon()
        .expect("rainbow kitty owns the frame")
        .cohorts()
        .iter()
        .filter(|k| k.row == 5)
        .all(|k| k.flow.is_some());
    assert!(
        flowing,
        "the run the text carried away from the caret's row flows"
    );
    // Typing on: the first key FLUSHES the park — the wrap's landing is laid
    // at the key's own clock — and then lays its own cell; each key after it
    // renews only the live run, and the followed band keeps flowing.
    h.type_str("yz");
    assert_eq!(
        h.glow.in_flight_tally().park_flushed,
        1,
        "the next key flushed the park"
    );
    assert_eq!(
        h.live(6),
        vec![0, 1, 2],
        "the wrap's own landing and the keys typed since, and only them"
    );
    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    assert_eq!(
        h.cells(6),
        vec![(0, false), (1, false), (2, false)],
        "the typed cells stand on the caret row"
    );
    assert!(h.lit(5), "the followed band is still flowing 0.4 s on");
    assert!(h.lit(6));
    // …and it is gone by the flow's end, the hand still on row 6.
    h.idle((FLOW_TOTAL_S * 1000.0) as u64);
    assert!(
        h.cells(5).is_empty() && !h.lit(5),
        "the followed band flowed out inside FLOW_TOTAL_S: {:?}",
        h.cells(5)
    );
    assert_eq!(h.retired(), 0, "…and still nothing was retired by content");
}

/// (i) D2, the control: an ED and the same text back at the same cells in
/// one PTY batch — every zsh prompt redraw, every Ctrl-L — keeps every
/// cell. The witness never fires because the host samples after the batch
/// is applied.
#[test]
fn a_prompt_redraw_that_puts_the_same_text_back_keeps_every_cell() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[2J\x1b[6;1Hhello world");
    assert!(
        h.since_key() < GRACE_S,
        "inside the grace: the swoosh is not the ending here"
    );
    let want: Vec<u16> = (0..11).collect();
    assert_eq!(
        h.live(5),
        want,
        "the band is untouched by a redraw of the same text"
    );
    assert!(h.leaving(5).is_empty(), "nothing is leaving");
    assert!(h.lit(5));
    assert_eq!(
        h.retired(),
        0,
        "the witness fired on a redraw of the same text"
    );
    h.idle(200);
    assert_eq!(h.live(5), want, "…and it stays");
    assert_eq!(h.retired(), 0);
}

/// (ii) The relocation: an ED, the text re-drawn two rows down, the caret
/// with it, no key. The move is DECLINED (no fresh hint).
///
/// RE-PINNED 2026-09-21 (the band follows its text — `rk::witness`'s follow
/// pass, `Engine::follow_rows`; RED on main before 2026-09-12 because row 5
/// stayed lit for its whole life with its text gone, the stranded band).
/// From 2026-09-12 to 2026-09-21 this pinned the melt: the witness saw row
/// 5's glyphs go and retired the band in `RETIRE_MELT_S`, and row 7 — "text
/// nobody typed here" — stayed dark. That was the wrong reading of a box
/// that moved WITH its text: the hand typed exactly those glyphs, and the
/// light belongs under them. Now the run's glyphs are found two rows down
/// at their own columns, gone from their own row, and the band is
/// TRANSLATED there with every clock intact — nothing stamped, nothing
/// retired, `ribbon_followed=` counting the cells. The caret went with the
/// text, so the run is not flowing: it keeps the clock its last key gave
/// it. A relocation whose text is NOT found still retires as before
/// (`a_band_overwritten_with_different_text_is_retired`, and the true
/// re-layout control in `composer_box_growth_wrap.rs`).
#[test]
fn a_relocated_input_box_carries_the_band_with_its_text_and_stamps_nothing() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[2J\x1b[8;1Hhello world");
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (7, 11), "the caret went with the text");
    assert_eq!(
        h.glow.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "the relocation had no key behind it"
    );
    let want: Vec<u16> = (0..11).collect();
    assert!(
        h.cells(5).is_empty(),
        "the band left row 5 with its text: {:?}",
        h.cells(5)
    );
    assert_eq!(h.live(7), want, "…and stands under it on row 7, unstamped");
    assert_eq!(h.retired(), 0, "nothing was retired by content");
    assert_eq!(h.followed(), 11, "every cell followed its text");
    assert!(h.lit(7), "row 7 is lit under the text the hand typed");
    assert!(!h.lit(5), "…and row 5 is dark");
    h.idle(200);
    assert!(
        h.since_key() < GRACE_S,
        "inside the grace: the swoosh could not have taken it"
    );
    assert_eq!(h.live(7), want, "the band stays under its text");
    assert!(h.cells(5).is_empty());
    assert!(h.lit(7));
    assert!(!h.lit(5));
}

/// **THE NEGATIVE CONTROL OF THE FOLLOW: A KILLED LINE UNDER ITS OWN
/// TWIN.** Row 4 holds the previous command, `$ cd ..`; the hand types the
/// same `cd ..` on row 5 and readline's Ctrl-U redraw (`\r$ \x1b[K`, no
/// key hint — a program's own repaint) blanks it. The run's glyphs are
/// gone from row 5, and row 4 holds the same glyphs at the same columns
/// as one block — but they were STANDING THERE before any of this run's
/// glyphs was laid: nothing moved, nothing arrived, and the follow pass
/// must not carry the band onto a line no key wrote this time. The band
/// is released into the swoosh, exactly as with any other line above
/// (the control: `$ ls -la`). RED on the integration tree before the fix
/// at `followed == 0` (`followed=5`, row 4 lit for 1.28 s).
#[test]
fn a_killed_line_under_an_identical_line_is_released_not_followed_onto_it() {
    for above in ["cd ..", "ls -la"] {
        let mut h = Host::at_row(4);
        h.program(format!("$ {above}\r\n$ ").as_bytes());
        h.type_str("cd ..");
        h.idle(64);
        assert_eq!(h.live(5), vec![2, 3, 4, 5, 6], "the band is under `cd ..`");
        assert!(!h.lit(4), "row 4 is dark before the kill");
        h.program(b"\r$ \x1b[K");
        assert_eq!(h.followed(), 0, "above `{above}`: nothing followed");
        let mut lit4 = 0;
        for _ in 0..90 {
            h.idle(16);
            lit4 += usize::from(h.lit(4));
        }
        assert_eq!(
            lit4, 0,
            "above `{above}`: the previous command's row never lights"
        );
        assert!(h.cells(4).is_empty(), "above `{above}`: {:?}", h.cells(4));
    }
}

/// **…AND WITH THE KILL KEY BEHIND IT.** The same line, killed by the hand:
/// `note_kill` precedes the erase's bytes (`\b`×5 and `EL`). The kill's own
/// retract must take the band on its own row — row 5 lit while it drains
/// — and the identical line above must stay dark. RED on the integration
/// tree before the fix: the follow pass ran before the kill replayed,
/// carried the band onto row 4, and row 5 was never lit.
#[test]
fn a_ctrl_u_under_an_identical_line_drains_on_its_own_row() {
    let mut h = Host::at_row(4);
    h.program(b"$ cd ..\r\n$ ");
    h.type_str("cd ..");
    h.idle(300);
    h.now += Duration::from_millis(90);
    h.glow.note_kill(h.now, true);
    h.term.process(b"\x08\x08\x08\x08\x08\x1b[K");
    h.frame();
    assert_eq!(h.followed(), 0, "nothing followed onto the line above");
    let (mut lit4, mut lit5) = (0, 0);
    for _ in 0..90 {
        h.idle(16);
        lit4 += usize::from(h.lit(4));
        lit5 += usize::from(h.lit(5));
    }
    assert_eq!(lit4, 0, "the previous command's row never lights");
    assert!(lit5 > 0, "the kill's swoosh drains on the killed row");
}

/// (iii) The next key after the relocation lays on row 7 BESIDE the band
/// that followed the text there, nothing on row 5: the mirror followed the
/// DECLINED row change. RED on main before 2026-09-12: the key laid at the
/// stale mirror, `(5, 11)`. RE-PINNED 2026-09-21: the band is on row 7 too
/// (see (ii)), so the key's cell joins it — one run, columns 0..=11.
#[test]
fn the_next_key_after_a_declined_relocation_lays_at_the_caret_s_true_cell() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[2J\x1b[8;1Hhello world");
    h.idle(200);
    assert!(h.cells(5).is_empty());
    h.key(b"x", 1);
    let want: Vec<u16> = (0..12).collect();
    assert_eq!(
        h.live(7),
        want,
        "the key laid its own cell on row 7 beside the band that followed"
    );
    assert!(
        h.cells(5).is_empty(),
        "…and nothing on the row the caret left"
    );
    assert!(h.lit(7));
}

/// (iv) The same cells overwritten with DIFFERENT text: glyph → different
/// glyph retires — and the space, which has no glyph of its own to witness,
/// goes with its run rather than lingering alone. RED on main: the band
/// stayed under text it was not typed over.
#[test]
fn a_band_overwritten_with_different_text_is_retired() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[6;1HHELLO WORLD");
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (5, 11), "the caret did not move");
    let want: Vec<u16> = (0..11).collect();
    assert_eq!(
        h.leaving(5),
        want,
        "every overwritten glyph's cell is retired, and the blank with its run"
    );
    assert!(h.live(5).is_empty());
    assert_eq!(h.retired(), 11);
    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    assert!(
        h.cells(5).is_empty(),
        "the retired band is out of the pool inside 200 ms"
    );
    assert!(!h.lit(5));
}

/// The screenshot's word-shaped gaps: a composer repaint changes one glyph
/// while spaces elsewhere in the live phrase remain between unchanged words.
/// Follow the real terminal/host seam through the retirement, not only its
/// first frame. A complete overwrite still removes the spaces with the run.
#[test]
fn a_composer_repaint_does_not_cut_a_live_ribbon_at_each_space() {
    const PHRASE: &str = "git commit and git pull and work on remote main";
    let mut h = Host::at_row(5);
    h.type_str(PHRASE);
    h.idle(64);
    let spaces: Vec<u16> = PHRASE
        .bytes()
        .enumerate()
        .filter_map(|(col, b)| (b == b' ').then_some(col as u16))
        .collect();
    assert_eq!(h.live(5).len(), PHRASE.len());
    assert_eq!(spaces.len(), 9, "exercise several word boundaries");

    // Clear and repaint in one batch, keeping the caret at the same column.
    let edited = PHRASE.replacen('g', "G", 1);
    h.program(format!("\x1b[6;1H\x1b[2K{edited}").as_bytes());
    assert_eq!(h.leaving(5), vec![0], "only the replaced glyph retires");
    for _ in 0..12 {
        let live = h.live(5);
        for &col in &spaces {
            assert!(live.contains(&col), "space {col} lost its ribbon");
            let x = (f32::from(col) + 0.5) * CW as f32;
            let y = 5.5 * CH as f32;
            assert!(
                h.glow.under_quads().iter().any(|q| {
                    q.alpha > 0
                        && f32::from(q.x) <= x
                        && x < f32::from(q.x) + f32::from(q.w)
                        && f32::from(q.y) <= y
                        && y < f32::from(q.y) + f32::from(q.h)
                }),
                "space {col} has a record but no painted ribbon"
            );
        }
        h.idle(16);
    }
    assert!(h.since_key() < GRACE_S);
    assert_eq!(h.retired(), 1);

    // Negative control: keeping every blank unconditionally would strand
    // isolated colored cells after all the words have been replaced.
    h.program(format!("\x1b[6;1H\x1b[2K{}", PHRASE.to_ascii_uppercase()).as_bytes());
    assert!(h.live(5).is_empty(), "a fully replaced run keeps no spaces");
    h.idle(200);
    assert!(h.cells(5).is_empty());
    assert!(!h.lit(5));
}

/// (v) A CJK glyph's two cells are one unit: overwrite its lead at the
/// run's END and both go on the same frame, its narrow neighbours untouched.
/// Overwrite it in the MIDDLE and neither goes: a change strictly inside a
/// standing run is not evidence (`Witness::shape_verdicts`, 2026-09-16 —
/// the owner: *"you need to be fixing in general"*), so the band stays
/// whole over the two new glyphs instead of carrying a two-cell hole.
#[test]
fn a_wide_glyph_s_two_cells_are_retired_as_one_unit() {
    let mut h = Host::at_row(5);
    h.type_str("ab\u{4f60}");
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (5, 4), "a + b + wide = four cells");
    assert_eq!(
        h.live(5),
        vec![0, 1, 2, 3],
        "both cells of the wide glyph are laid"
    );
    h.idle(400);
    // Two narrow glyphs over the wide one's cells, at the run's end.
    h.program(b"\x1b[6;3Hxy\x1b[6;5H");
    assert_eq!(
        h.leaving(5),
        vec![2, 3],
        "the wide glyph's two cells go together"
    );
    assert_eq!(h.live(5), vec![0, 1], "its neighbours are untouched");
    assert_eq!(h.retired(), 2);

    // And a DIFFERENT wide glyph over it: the same unit, the same verdict.
    let mut h = Host::at_row(5);
    h.type_str("ab\u{4f60}");
    h.idle(400);
    h.program("\x1b[6;3H\u{597d}\x1b[6;5H".as_bytes());
    assert_eq!(h.leaving(5), vec![2, 3]);
    assert_eq!(h.live(5), vec![0, 1]);

    // In the MIDDLE of a standing run the same overwrite is not evidence:
    // nothing leaves, the band is whole across the new glyphs.
    let mut h = Host::at_row(5);
    h.type_str("a\u{4f60}b");
    h.idle(400);
    h.program(b"\x1b[6;2Hxy\x1b[6;5H");
    assert_eq!(
        h.leaving(5),
        Vec::<u16>::new(),
        "an interior change stays lit"
    );
    assert_eq!(h.live(5), vec![0, 1, 2, 3]);
    assert_eq!(h.retired(), 0);
}

/// (vi) The box-growth wrap (the owner's ring: `licensed (41,116)->(41,7)`,
/// a typed re-anchor): the text moves UP a row while the caret stays on its
/// row and jumps left. The old band's cells now show the continuation row's
/// blanks and go on the melt AT ONCE — the witness reads the host's rows on
/// the growth frame. RED on main: the whole band stayed on the caret row
/// while its text lived a row above.
///
/// And with NO key after it the park is judged on the tick past its own
/// window (`ca54aaadd`: the held park, ≤ `TYPE_HINT_FRESH`): the flush's
/// re-anchor lays the wrap's landing at the key's clock, so the growth key's
/// own glyph stands alone on the row the melt emptied. RED before the
/// re-anchor's one-cell sweep: the park had deferred the mirror past the
/// key's `Typed` replay, the glyph landed on the abandoned band instead and
/// the landing was dark for good.
///
/// RE-PINNED 2026-09-21 (the band follows its text — see the sibling
/// above): the band is carried up under its text on the growth frame and
/// flows there, nothing is retired, and the silent flush still lays the
/// landing on the caret row as before.
#[test]
fn the_box_growth_wrap_carries_the_band_up_and_a_silent_flush_lays_its_landing() {
    let mut h = hello(6);
    h.key(b"\x1b[6;1Hhello world\x1b[7;1H\x1b[2Kx", 1);
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (6, 1),
        "the caret stayed on its row and re-anchored left"
    );
    let want: Vec<u16> = (0..11).collect();
    assert!(
        h.cells(6).is_empty(),
        "the band left the caret row with its text: {:?}",
        h.cells(6)
    );
    assert_eq!(h.live(5), want, "…and stands under it one row up");
    assert_eq!(h.retired(), 0);
    assert_eq!(h.followed(), 11);
    assert!(h.lit(5), "the row the text moved to carries the band");
    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    assert!(
        h.cells(6).is_empty(),
        "nothing on the caret row yet — the park is still held"
    );
    assert!(h.lit(5), "the followed band is flowing");
    assert_eq!(h.glow.in_flight_tally().park_flushed, 0);
    // Past the park's own window: the flush judges the wrap as the re-anchor
    // it always was and lays the one cell it spent its credit on.
    h.idle(150);
    assert_eq!(h.glow.in_flight_tally().park_flushed, 1);
    assert!(h.since_key() < GRACE_S);
    assert_eq!(
        h.cells(6),
        vec![(0, false)],
        "the flush's re-anchor lays the key's own cell at the landing"
    );
    assert!(h.lit(6), "…and it is on the glass");
    assert!(h.lit(5), "the followed band is still flowing at +0.35 s");
    h.idle((FLOW_TOTAL_S * 1000.0) as u64);
    assert!(
        h.cells(5).is_empty() && !h.lit(5),
        "…and gone by the flow's end: {:?}",
        h.cells(5)
    );
}

/// **A RE-ANCHOR ONTO THE PANE'S FIRST COLUMN LIGHTS NOTHING IN THE PANE
/// BESIDE IT.** The seam's re-anchor sweep lays the one cell the verdict
/// spends its credit on — the landing, one cell back from the caret. That
/// fold is the RIBBON's fold, and `Ribbon::lay` states its edge: "a fold
/// wraps at the FOCUSED PANE's edges (`set_pane`), not the grid's … the cell
/// before its first column is the previous row's LAST pane cell — never the
/// neighbouring pane's." `Ribbon::sweep` clamps to the grid alone, so a
/// re-anchor landing on the pane's own first column swept `cc - 1` verbatim:
/// a live cell in the split beside it, under content the hand is not typing
/// in. RED at "nothing is lit outside the focused pane" before the sweep's
/// guard read the PANE's first column instead of the grid's.
#[test]
fn a_re_anchor_onto_the_pane_s_first_column_lights_nothing_in_the_pane_beside_it() {
    const PANE_COL0: u16 = 40;
    let mut h = Host::at_row(6);
    h.glow.note_pane_columns(PANE_COL0, 40);
    // The hand types in the RIGHT-HAND split: a band at row 6, columns 40..51.
    h.term.process(b"\x1b[7;41H");
    h.frame();
    h.type_str(TEXT);
    let want: Vec<u16> = (PANE_COL0..PANE_COL0 + TEXT.len() as u16).collect();
    assert_eq!(
        h.live(6),
        want,
        "the band is laid under the pane's own text"
    );
    let retired_before = h.retired();
    // The re-anchor: one typed key whose echo drops the caret back onto the
    // pane's FIRST column and leaves the row's text exactly as it was, so the
    // content witness fires on nothing. The licensed backward re-anchor now
    // explicitly drains the old line toward its landing; it still cannot
    // lay a cell in the neighboring pane.
    h.key(b"\x1b[7;41H", 1);
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (6, PANE_COL0),
        "the caret re-anchored onto the pane's first column"
    );
    // Past the park's own window, so the held verdict is judged and the
    // re-anchor's landing sweep — if it lays anything — has run.
    h.idle(400);
    assert_eq!(
        h.glow.in_flight_tally().park_flushed,
        1,
        "the park was judged"
    );
    assert!(
        h.since_key() < GRACE_S,
        "inside the idle grace: this is the explicit re-anchor drain"
    );
    let strays: Vec<(u16, bool)> = h
        .cells(6)
        .into_iter()
        .filter(|&(col, _)| col < PANE_COL0)
        .collect();
    assert!(
        strays.is_empty(),
        "nothing is lit outside the focused pane; the re-anchor's landing put \
         {strays:?} in the split beside it"
    );
    assert!(
        h.live(6).is_empty(),
        "the old line is draining, not renewed"
    );
    assert_eq!(
        h.retired(),
        retired_before,
        "unchanged glyphs do not fire the content witness"
    );
    let draining = h.cells(6);
    assert!(
        !draining.is_empty(),
        "the explicit drain fades rather than dropping the whole band"
    );
    assert!(
        draining
            .iter()
            .all(|&(col, leaving)| want.contains(&col) && leaving),
        "only the original pane-local cells remain on their drain: {draining:?}"
    );
    // Past the 0.40 s stagger plus 0.24 s melt, even if the held verdict
    // used the flush instant: every old cell must have left the pool.
    h.idle(700);
    assert!(
        h.cells(6).is_empty(),
        "the re-anchored line finishes its bounded drain"
    );
    // The edge withholds a landing cell, not the next genuine typed glyph.
    h.key(b"Z", 1);
    assert_eq!(
        h.live(6),
        vec![PANE_COL0],
        "fresh typing lights its own pane's first cell"
    );
}

/// A key typed back over a cell an ABANDONED cohort still owns keeps its own
/// light. `hello world`, Up, Down, Left×3, `X` at `(5,8)`: the Up is a row
/// jump that abandons the band (its cohort rewinds into the retract and its
/// cells stay in the pool, not `leaving`, for ~0.64 s); the key back on the
/// row cannot join an abandoned cohort, so a SECOND live cell is minted at
/// `(5,8)`. The stale cell (`o` → `X`) is one changed glyph strictly inside a
/// run whose other ten letters stand, so the witness does not name it
/// (`Witness::shape_verdicts`, 2026-09-16): it leaves on the drain its
/// cohort is already on, and the key's own cell, born this frame, keeps its
/// light — `retire_cells` stamps by identity, and nothing is stamped here.
#[test]
fn a_key_typed_over_an_abandoned_cohorts_cell_keeps_its_own_light() {
    let mut h = hello(5);
    h.nav(b"\x1b[A");
    h.nav(b"\x1b[B");
    for _ in 0..3 {
        h.nav(b"\x1b[D");
    }
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (5, 8), "Up, Down, Left×3 from (5,11)");
    let before = h.cells_at(5, 8);
    // Since the per-row drain (Rainbow Path v3 §2.6, 2026-09-13) the band
    // the Up left BELOW the caret drains from its RIGHT end, so by the third
    // Left (+450 ms) the stale cell at (5,8) has spent to zero and the hop
    // has laid a fresh wake cell over it (a hop lights what the drain has
    // emptied). The stale cell is still resident and not `leaving`; it is
    // the OLDEST at the position.
    assert!(
        !before.is_empty() && !before[0].1,
        "the stale owner at (5,8) is resident and abandoned, not leaving: {before:?}"
    );
    let stale_born = before[0].0;
    let retired_before = h.retired();

    h.key(b"X", 1);
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (5, 9), "the key echoed at (5,8)");
    let at = h.cells_at(5, 8);
    assert!(
        at.len() >= 2,
        "the stale cell and the fresh one at (5,8): {at:?}"
    );
    assert_eq!(at[0].0, stale_born, "oldest first is the stale cell");
    let fresh = at[at.len() - 1];
    assert!(
        !fresh.1,
        "the key's own cell keeps its light on its birth frame: {at:?}"
    );
    let fresh_born = fresh.0;
    assert!(fresh_born > stale_born);
    assert_eq!(
        h.retired() - retired_before,
        0,
        "one glyph changed inside a standing run names nothing — not the stale cell, not the fresh one, not the run's blank"
    );
    // **THE BLANK STAYS WITH A RUN THAT IS STILL STANDING** (2026-09-15).
    // `hello world`'s other ten glyphs are unchanged, so the space at
    // (5,5) is still a space between lit letters: taking it with the one
    // replaced glyph punches a hole in a whole band, which is the owner's
    // word-block break-up. It leaves on the drain its run is already on.
    assert!(
        h.cells(5).contains(&(5, false)),
        "the run is standing and its blank kept its light: {:?}",
        h.cells(5)
    );

    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    let at = h.cells_at(5, 8);
    assert_eq!(
        at,
        vec![(fresh_born, false)],
        "200 ms on: the stale cell is out of the pool, the key's cell stands"
    );
    assert!(h.lit(5), "the key's light is on the glass");
}

/// The trail status row carries the count: appended after `v2_bridged=`,
/// nothing in front of it moves.
#[test]
fn trail_status_appends_ribbon_retired_after_the_v2_rows() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[6;1HHELLO WORLD");
    let status = h.glow.v2_status().expect("v2 owns the frame");
    assert_eq!(status.retired, 11);
    let line = trail_line(&h);
    // `ribbon_followed=` (2026-09-21) rides beside it: the cells the
    // follow pass carried WITH their text — none here, the text was
    // overwritten in place. `ribbon_follow_missed=` (2026-09-23) is the
    // tail's new last field, appended as every v2 row is (nothing in front
    // of it moves): no text arrived elsewhere, so nothing was missed.
    assert!(
        line.ends_with(" ribbon_retired=11 ribbon_followed=0 ribbon_follow_missed=0"),
        "the three content counts are the last fields: {line}"
    );
    assert!(
        line.contains(" v2_meteors=0 v2_bridged=0 ribbon_retired=11 ribbon_followed=0"),
        "{line}"
    );
}

/// `trail status`'s line for `h`, as the control socket prints it
/// (`TrailStatus::line_v2`).
fn trail_line(h: &Host) -> String {
    aterm_effects::cursor_glow::TrailStatus {
        style_raw: "rainbow kitty",
        style: GlowStyle::RainbowKitty,
        config_enabled: true,
        effective: true,
        focused: true,
        motion_stage: "full",
        motion_mode: "auto",
        shed: 1.0,
        intensity: 0.7,
        sound_seam: true,
        ribbon_look: "tall",
        tally: h.glow.admission_tally(),
        spawns: h.glow.spawns(),
        ribbon_segments: h.glow.ribbon_segments(),
        ribbon_hue_bands: h.glow.ribbon_hue_bands(),
        ribbon_drawn: h.glow.ribbon_drawn(),
        ribbon_curtain_ms: None,
        field: h.glow.rainbow_field(),
        sparks: h.glow.live_sparks(),
        momentum: 0.0,
        momentum_display: 0.0,
        momentum_glow: 0.0,
        glow_active: true,
        pet_active: false,
        pet_action: "none",
        pet_content: 0.0,
        pet_pending: 0,
        pet_focus: "none",
        pet_reason: "none",
        pet_anchor: None,
        pet_event_seq: 0,
        pet_pose: "none",
        pet_body: None,
        cat_active: false,
        block_fill: None,
        flow: h.glow.flow_status(),
        inserts: aterm_effects::cursor_glow::InsertTally::default(),
        in_flight: aterm_effects::cursor_glow::InFlightTally::default(),
    }
    .line_v2(h.glow.v2_status())
}

/// `ribbon_follow_missed=`, read from the engine's status.
fn follow_missed(h: &Host) -> u64 {
    h.glow.v2_status().map_or(0, |s| s.follow_missed)
}

/// **THE MISSED-FOLLOW COUNT REACHES `trail status`** (2026-09-23, the
/// review: the counter had no positive control above the witness unit —
/// replacing the engine's accumulation with a no-op left the whole suite
/// green, so `ribbon_follow_missed=0` could read forever while composer
/// rows melted). `hello world` typed on row 5, then one PTY batch blanks
/// the row and writes `hel   world` on row 4: eight of its ten glyphs
/// ARRIVED a row up at their own columns, but around a hole — not one
/// block — so the follow pass names nothing and counts the ten gone cells
/// missed, once, on the engine's status and on `trail status`'s last field.
/// The controls, each from the same `hello world`: the whole line arriving
/// is FOLLOWED (eleven cells, the space with them) and misses nothing, and
/// other text a row up arrived nothing and misses nothing.
///
/// RED with `Engine::follow_rows`' accumulation replaced by a no-op (the
/// review's mutant): `ribbon_follow_missed=0`.
#[test]
fn a_line_that_arrives_a_row_up_around_a_hole_is_counted_missed_on_trail_status() {
    for (now, missed, followed) in [
        ("hel   world", 10, 0),
        ("hello world", 0, 11),
        ("jumpy frogs", 0, 0),
    ] {
        let mut h = hello(5);
        h.idle(64);
        let (missed0, followed0) = (follow_missed(&h), h.followed());
        assert_eq!((missed0, followed0), (0, 0), "the premise: nothing yet");
        h.program(format!("\x1b[6;1H\x1b[2K\x1b[5;1H{now}").as_bytes());
        assert_eq!(
            (follow_missed(&h), h.followed()),
            (missed, followed),
            "`{now}` a row up: (missed, followed)"
        );
        h.idle(300);
        assert_eq!(
            follow_missed(&h),
            missed,
            "`{now}` a row up: counted once, not per frame"
        );
        let line = trail_line(&h);
        assert!(
            line.ends_with(&format!(" ribbon_follow_missed={missed}")),
            "`{now}` a row up: the count is trail status's last field: {line}"
        );
    }
}
