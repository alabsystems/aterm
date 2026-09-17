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
//! rows sampled exactly as `app_render.rs`'s LOCK A samples them
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
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::ribbon::{
    PHRASE_REST_MIN_S, RETIRE_MELT_S, RETRACT_DUR_S, RETRACT_FADE_S, SWOOSH_TOTAL_S,
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
}

impl Host {
    /// A fresh host with the caret parked at 0-based `(row, 0)` and the
    /// anchor seeded by one frame.
    fn at_row(row: u16) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(format!("\x1b[{};1H", row + 1).as_bytes());
        let now = Instant::now();
        let mut h = Self {
            term,
            glow: CursorGlow::default(),
            cfg: cfg(),
            g: geom(),
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            last_key: now,
        };
        h.frame();
        h
    }

    /// EXACTLY LOCK A, then the tick: sample the cursor, the repaint blink,
    /// the caret row's probe, and the rows the resident ribbon occupies —
    /// all from the terminal AFTER the last `process` — then advance the
    /// engine one frame.
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
            // The wrap ABANDONS the row the caret left (`leave_row`), so it
            // leaves on the retract's own span rather than its grace — and
            // it must keep its light the whole way down: no early cut.
            assert!(
                h.lit(5),
                "row 5 keeps the life it had through its grace (+{since:.2} s)"
            );
            row_5_lit_at_grace = true;
        }
        if row_5_dark_at.is_none() && h.cells(5).is_empty() && !h.lit(5) {
            row_5_dark_at = Some(since);
        }
    }
    assert!(row_5_lit_at_grace, "the grace was observed");
    let dark = row_5_dark_at.expect(
        "row 5 must go dark while the hand is still typing on row 6 — a global hold          keeps it at full light for as long as any key is live",
    );
    // THE BOUND: its own swoosh, and not a frame later than one key's cadence
    // past it (the read is once per key, 90 ms apart).
    assert!(
        (RETRACT_DUR_S + RETRACT_FADE_S..=SWOOSH_TOTAL_S + 0.1).contains(&dark),
        "row 5 went out at +{dark:.2} s after the caret left it; the bound is the \
         abandon's own retract, and never past its swoosh, {SWOOSH_TOTAL_S} s"
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
#[test]
fn the_box_growth_wrap_melts_the_old_band_at_once_and_the_flush_pays_its_landing() {
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
    assert_eq!(
        h.leaving(6),
        want,
        "the old band's cells, now blank or overwritten, are retired at once"
    );
    assert!(
        h.live(6).is_empty(),
        "the park holds the wrap's own verdict"
    );
    assert_eq!(
        h.glow.in_flight_tally().park_flushed,
        0,
        "…held, not judged: nothing flushed yet"
    );
    assert!(
        !h.lit(5),
        "the row the text moved to was never typed on here: dark"
    );
    assert_eq!(h.retired(), 11);
    // Typing on: the first key FLUSHES the park — the wrap's landing is laid
    // at the key's own clock — and then lays its own cell; each key after it
    // renews only the live run, and the retired cells keep melting.
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
        "the old band is gone inside 200 ms; the typed cells stand"
    );
    assert!(!h.lit(5));
    assert!(h.lit(6));
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
/// with it, no key. The move is DECLINED (no fresh hint); the witness sees
/// row 5's glyphs go and retires the band; row 7 — text nobody typed here —
/// stays dark. RED on main: row 5 stayed lit for its whole life (the band was
/// stranded) and `ribbon_retired` did not exist.
#[test]
fn a_relocated_input_box_retires_the_band_on_the_abandoned_row_and_lights_nothing_on_the_new_one() {
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
    assert_eq!(
        h.leaving(5),
        want,
        "every cell of the abandoned band is retired on the frame"
    );
    assert_eq!(h.retired(), 11);
    assert!(!h.lit(7), "row 7 holds text nobody typed: dark");
    h.idle(200);
    assert!(
        h.since_key() < GRACE_S,
        "inside the grace: the swoosh could not have taken it"
    );
    assert!(
        h.cells(5).is_empty(),
        "row 5 is out of the pool inside 200 ms"
    );
    assert!(!h.lit(5), "…and off the glass");
    assert!(!h.lit(7), "row 7 stays dark");
    assert!(
        (RETIRE_MELT_S * 1000.0) as u64 <= 150,
        "the melt is inside the 150 ms the ruling allows"
    );
}

/// (iii) The next key after the relocation lays on row 7, nothing on row 5:
/// the mirror followed the DECLINED row change. RED on main: the key laid at
/// the stale mirror, `(5, 11)`.
#[test]
fn the_next_key_after_a_declined_relocation_lays_at_the_caret_s_true_cell() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[2J\x1b[8;1Hhello world");
    h.idle(200);
    assert!(h.cells(5).is_empty());
    h.key(b"x", 1);
    assert_eq!(h.live(7), vec![11], "the key laid its own cell on row 7");
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
#[test]
fn the_box_growth_wrap_melts_the_band_at_once_and_a_silent_flush_lays_its_landing() {
    let mut h = hello(6);
    h.key(b"\x1b[6;1Hhello world\x1b[7;1H\x1b[2Kx", 1);
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (6, 1),
        "the caret stayed on its row and re-anchored left"
    );
    let want: Vec<u16> = (0..11).collect();
    assert_eq!(
        h.leaving(6),
        want,
        "the old band's cells, now blank or overwritten, are retired at once"
    );
    assert!(
        h.live(6).is_empty(),
        "the park holds the wrap's own verdict"
    );
    assert!(
        !h.lit(5),
        "the row the text moved to was never typed on here: dark"
    );
    assert_eq!(h.retired(), 11);
    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    assert!(
        h.cells(6).is_empty(),
        "the old band is gone inside 200 ms — and the park is still held"
    );
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
    assert!(!h.lit(5));
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
    let line = aterm_effects::cursor_glow::TrailStatus {
        style_raw: "rainbow kitty",
        style: GlowStyle::RainbowKitty,
        config_enabled: true,
        effective: true,
        focused: true,
        motion_stage: "full",
        motion_mode: "auto",
        shed: 1.0,
        intensity: 0.7,
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
    .line_v2(Some(status));
    assert!(
        line.ends_with(" ribbon_retired=11"),
        "the count is the last field: {line}"
    );
    assert!(
        line.contains(" v2_meteors=0 v2_bridged=0 ribbon_retired=11"),
        "{line}"
    );
}
