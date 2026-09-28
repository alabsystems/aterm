// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A WORD TYPED INTO CLAUDE CODE'S COMPOSER KEEPS ITS BAND WHILE THE BOX
//! RE-WRAPS IT** (2026-09-23 — the owner, Claude Code's composer: row 1
//! `› the other aterm window … harness should have`, row 2 `  already been
//! runnignin █[Image #1] in the`, the words typed INTO the line before the
//! image chip). `claude_wrap_band.rs` pins the owner's own picture, the
//! soft-wrapped caret. This file pins the NEIGHBOURING shapes of the same
//! insert, each with a root cause of its own.
//!
//! WHAT CLAUDE CODE WRITES, MEASURED. Claude Code v2.1.280 under a PTY
//! recorder inside a headless aterm (64×20, alt screen, DEC 2026 answered),
//! driven with `aterm ctl key` (the typed hint the app stamps): the text
//! `… you did|do have [Image #1] in the` sent as program text, the caret
//! walked back with Left (the image chip is ONE Left), then `already been
//! runnignin ` typed. Every key is ONE home-addressed cell diff inside
//! `?2026h ?25l … ?25h ?2026l` — no `CSI 2K`: the typed glyph plus every
//! cell the insert shifted, `ESC[K` where a row got shorter, the caret last.
//! The typed letters GLUE to the chip's first half (`a[Image` is one word to
//! Ink's wrap), so the word being typed re-wraps between row 1 and row 2 as
//! it grows, and back up when a Space splits it off the chip.
//! `fixtures/wrap_code-cc-chip-reflow-2026-09-23.ptylog` is that recording
//! (µs stamps); the synthetic composer below is modelled on it cell for cell
//! (64 columns, text columns 2..=61, continuation indent 2, the chip atomic
//! under Left).
//!
//! * **A — THE KEY THAT PUSHES THE WORD DOWN, HELD AS A FOOTER PARK (the
//!   whole insert dark).** `have [Image` fills row 1, and the FIRST key
//!   sends `a[Image` down: Ink erases row 1 from the caret with `ESC[K`,
//!   writes `a[Image #1]` from the indent, and the caret goes `(16,56) →
//!   (17,3)`. Row 1's prefix is untouched, so the seam's cross-row park
//!   (a background footer repaint while a key is in flight must never spend
//!   it) held the move, and `spawn` swallowed every later move for the
//!   park's ten-second patience: 23 keys, no verdict, nothing laid. The same
//!   take LIVE on the shipped 0.91.0 logged no `trail` verdict at all. Now
//!   the move is judged at once when the glass shows the key carried the
//!   text down (`CursorGlow::key_pushed_text_down`): the source row's tail
//!   erased from the caret AND the in-flight key's own glyph left of the
//!   landing. Either alone keeps the footer's custody (the two negative
//!   controls).
//! * **B1 — THE MOVED LETTERS MEASURED FROM A SPACE THE HAND NEVER TYPED
//!   (the first letter dark, a scrap left on row 1).** One column more room:
//!   `a` fits and `l` moves `al[Image` down, `(16,56) → (17,4)`. The ribbon
//!   measured the word a re-wrap moved from the hand's last TYPED Space on
//!   the row (`Ribbon::moved_word_len`); the hand had navigated into program
//!   text, so there was none. `w = 0`: nothing relaid (the `a` at (17,2)
//!   dark for its band's life, the band starting under `l`) and the old `a`
//!   cell left flowing in place on row 1's blank tail.
//! * **B2 — A STALE TYPED SPACE (a stub in the indent).** When the hand
//!   typed row 1 but the Space after its last word came with a paste, its
//!   last typed Space was the one before `have`, and nothing had cleared it
//!   when the hand navigated. The word was over-measured and relaid down to
//!   the pane's first column: cells on the continuation row's blank indent,
//!   columns 0–1. B1 and B2 are one fix: the measure starts at the LATER of
//!   the last typed Space and where the hand last ARRIVED (`Ribbon::arrived`).
//! * **B3 — AN ARROW INSIDE THE WORD BEING TYPED (the moved word's head
//!   dark).** Every arrow is an arrival, and one that walks back over the
//!   hand's own live letters — a typo fixed mid-word, a scrub — cut the
//!   measure there: the word a re-wrap then moved measured only what was
//!   typed after the arrow (`0` for the typo's insert), its head dark on
//!   row 2 and its old light flowing on row 1's blank tail for ~1 s. The
//!   arrival now reaches back over the contiguous live typed cells left of
//!   it (`Ribbon::moved_word_len`); the typed Space still bounds it.
//! * **B4 — A TOKEN THE BOX HARD-BREAKS (row 1's band vanishing).** A token
//!   begun at row 1's first text column cannot go down whole; Ink's
//!   wrap-ansi `hard` breaks it at the row's end, the key that fills the
//!   last column is drawn there over the glyph it pushed on, and the caret
//!   alone wraps. The soft-wrap verdict read only a blank origin, so the
//!   move was the fold's (a cell on the indent, the key's glyph dark) and
//!   the arrival measured a 59-letter "moved word": row 1's whole band
//!   fast-retracted. The verdict now also takes an origin the key's own
//!   fresh glyph replaced (`CursorGlow::soft_wrapped_caret`). Modelled on
//!   wrap-ansi, not recorded.
//! * **C — THE LIFTED WORD (the word unlit on row 1).** With `already`
//!   typed on row 2, the Space splits `already[Image` and `already` fits
//!   row 1 again: Ink writes it at row 1's end and the rest of row 2 from
//!   the indent, the caret staying before `[Image` at (17,2). That is a
//!   same-row backward typed move. The seam held it as Ink's park, and the
//!   ribbon then drained the word's light under the chip's text while the
//!   word stood unlit on row 1. The follow pass carries runs vertically, at
//!   their own columns, never to other ones. Now the seam reads the lift off
//!   the probes (`CursorGlow::lifted_word`) and judges it at once, and the
//!   ribbon relays the word's lit cells under its glyphs on row 1, on the
//!   walk they had (`Ribbon::lift_word`).
//! * **D — A SOFT-WRAPPED WORD COMES DOWN ONLY WITH ITS TEXT.** The ribbon
//!   relayed the word a soft-wrapped caret left on row 1 on any echo from
//!   the landing that hopped past it — also a burst or a paste after a
//!   one-letter word, which leaves the word standing: its light went dark
//!   under a glyph that never moved. The relay now needs the seam's reading
//!   of the glass, the word's columns blank on its row
//!   (`CursorGlow::carried_word_left`).
//!
//! NOT REPRODUCED: the owner's two scraps in the MIDDLE of row 1 (under the
//! space after `aterm` and under `had`); no ribbon cell lands there in any
//! take below.
//!
//! Every take drives the real seam: an `aterm_core` [`Terminal`] fed the
//! bytes, sampled the way `app_render.rs`'s LOCK A samples it (the caret's
//! row probe and its neighbours, the print anchor, every ribbon row; the
//! differences are listed on [`Host`]), and [`CursorGlow`] ticked on a
//! 16 ms train with the hints the app stamps for each key. The census reads
//! the PLAN's coverage per column of the two composer rows. Measured on the
//! unfixed tree (`e61aabbdb`): A — all 20 typed glyph columns dark, and no
//! verdict for the fold, in the synthetic and in the recording's take 3.
//! B1 — (17,2) dark in 42 frames and (16,55) lit for ~0.7 s, and the same
//! in take 4. B2 — the indent lit in 38 frames. C — the lifted `already`
//! dark on row 1 after the Space, (16,56..=61), in the synthetic and in
//! take 4 ((16,55) held only B1's scrap). Measured on `2ac0e6aab`: B3 — the
//! typo's row-2 columns 2..=16 dark in 24 frames and row 1's 51..=61 lit to
//! ~1.3 s; the scrub's 2..=17 dark. B4 — row 1's last six glyphs dark in 4
//! to 41 frames each, the indent lit in 100. D — the standing `I` without
//! a live cell in every frame of the 100 ms after the burst.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::RETRACT_FADE_S;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

/// The recording: Claude Code v2.1.280 in a 64×20 headless aterm.
const REC: &str = include_str!("fixtures/wrap_code-cc-chip-reflow-2026-09-23.ptylog");
const ROWS: usize = 20;
const COLS: usize = 64;
const CW: usize = 7;
const CH: usize = 14;
const FRAME_MS: u64 = 16;
/// A column is LIT at this planned coverage (the glass census's 20/255).
const LIT_COV: u8 = 20;

/// The composer's first text row (0-based) and its continuation row.
const ROW1: usize = 16;
const ROW2: usize = 17;
/// Text columns per row: 60 `x` fill columns 2..=61 (measured).
const TEXT_W: usize = 60;
/// `❯ ` on the first row, the continuation indent on the others.
const PREFIX: usize = 2;
/// Claude Code's image placeholder: glued to the letters typed before it.
const CHIP: &str = "[Image #1]";
/// What the owner typed before the chip.
const TYPED: &str = "already been runnignin ";
/// The owner's final geometry: `have [Image` fits row 1, `have a[Image`
/// does not — the FIRST key pushes the glued word down (A).
const ROW1_EXACT: &str = "the aterm window had this prompt appear. you did have";
/// One column more: `a` fits, `al[Image` wraps — the SECOND key moves the
/// first letter down (B1, B2) — and the Space after `already` lets it fit
/// row 1 again (C).
const ROW1_ONE_FITS: &str = "the aterm window had this prompt appear. you do have";
/// Where the lifted `already` stands on row 1 in that geometry.
const LIFTED: std::ops::RangeInclusive<usize> = 55..=61;

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

/// The host's `typed_class_for` and `glyph_shifted && !spacebar`.
fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        _ => (false, TypedClass::Glyph),
    }
}

/// Plan coverage per column of `row` (`tests/torn_read_gaps.rs`).
fn coverage(glow: &CursorGlow, row: u16) -> Vec<u8> {
    let mut cov = vec![0u8; COLS];
    let rib = glow.v2_ribbon().expect("rainbow kitty owns the frame");
    for sg in rib.plan_segments() {
        let r = ((sg.spine / CH as f32) - 0.5).floor();
        let c = (sg.x / CW as f32).floor();
        if r >= 0.0 && c >= 0.0 && r as usize == usize::from(row) && (c as usize) < COLS {
            let i = c as usize;
            cov[i] = cov[i].max(sg.cov);
        }
    }
    cov
}

fn map(lit: &[bool]) -> String {
    lit.iter().map(|&l| if l { '#' } else { '.' }).collect()
}

/// One frame's census of the composer's two rows.
#[derive(Clone, Debug)]
struct Shot {
    ms: u64,
    label: String,
    caret: (u16, u16),
    text: [String; 2],
    lit: [Vec<bool>; 2],
    /// Columns owned by a LIVE ribbon cell (not leaving), per row.
    live: [Vec<usize>; 2],
    /// Hand-typed glyph cells per row as `(col, typing order)`.
    typed: [Vec<(usize, u64)>; 2],
}

impl Shot {
    /// Live cells on the continuation row at or right of the caret, when
    /// the caret is on it: light over text no key of this insert has
    /// written yet (the chip the word was glued to).
    fn live_ahead_on_row2(&self) -> Vec<usize> {
        if usize::from(self.caret.0) != ROW2 {
            return Vec::new();
        }
        self.live[1]
            .iter()
            .copied()
            .filter(|&c| c >= usize::from(self.caret.1))
            .collect()
    }
}

/// A real terminal and the glow engine, sampled the way LOCK A samples them
/// (`app_render.rs`: the caret row's probe and its two neighbours, the
/// print anchor, every row the ribbon wants), a frame after every PTY read
/// and on a 16 ms train. Not host-exact: the probe is always
/// `ProbeTrust::Full` (the host drops to `ContentOnly` on the alt screen
/// with no recent repaint blink); there is no content-scroll sync or
/// `drop_row_probe` (no take here scrolls past its setup); a frame is
/// presented inside an open `?2026` bracket (the host withholds it); the
/// caret row's witness sample follows its neighbour probe (the host feeds
/// the witness rows first); an arrow stamps `note_motion` without the
/// host's `clear_typed`, a Backspace `note_backspace` rather than
/// `note_backspace_erasing`; and there is no `note_pane_rows`. A
/// host-faithful rewrite of this harness gave the A, B1, B2 and C verdicts
/// unchanged on the fixed and the unfixed tree (2026-09-23 review);
/// `claude_wrap_band.rs`'s harness is the faithful one.
struct Host {
    term: Terminal,
    glow: CursorGlow,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    above: Vec<char>,
    below: Vec<char>,
    blink: u64,
    t0: Instant,
    now: Instant,
}

impl Host {
    fn new() -> Self {
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        Self {
            term: Terminal::new(ROWS as u16, COLS as u16),
            glow,
            out: Vec::new(),
            row_buf: Vec::new(),
            above: Vec::new(),
            below: Vec::new(),
            blink: 0,
            t0: now,
            now,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink {
            self.blink = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        let r = usize::from(c.row);
        self.term.row_cols_into(r, &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        if r > 0 {
            self.term.row_cols_into(r - 1, &mut self.above);
        }
        if r + 1 < ROWS {
            self.term.row_cols_into(r + 1, &mut self.below);
        }
        self.glow.observe_neighbor_rows(
            (r > 0).then_some(self.above.as_slice()),
            (r + 1 < ROWS).then_some(self.below.as_slice()),
        );
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        self.glow.observe_print_anchor(self.term.print_anchor());
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &row in &rows[..n] {
            self.term.row_cols_into(usize::from(row), &mut self.row_buf);
            self.glow.observe_ribbon_row(row, &self.row_buf);
        }
        let cfg = cfg();
        self.glow.tick(cur, self.now, &cfg, geom(), &mut self.out);
    }

    /// The census of this frame: the caret, the two composer rows' text and
    /// lit columns.
    fn shot(&mut self, label: &str) -> Shot {
        let mut text = [String::new(), String::new()];
        let mut lit = [Vec::new(), Vec::new()];
        for (i, r) in [ROW1 as u16, ROW2 as u16].into_iter().enumerate() {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            text[i] = self
                .row_buf
                .iter()
                .map(|&c| if c == '\0' { ' ' } else { c })
                .collect();
            lit[i] = coverage(&self.glow, r)
                .iter()
                .map(|&v| v >= LIT_COV)
                .collect();
        }
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let live = [ROW1, ROW2].map(|r| {
            let mut cols: Vec<usize> = rib
                .cells()
                .iter()
                .filter(|c| usize::from(c.row) == r && !c.leaving())
                .map(|c| usize::from(c.col))
                .collect();
            cols.sort_unstable();
            cols.dedup();
            cols
        });
        let c = self.term.cursor();
        Shot {
            ms: self.ms(),
            label: label.to_string(),
            caret: (c.row, c.col),
            text,
            lit,
            live,
            typed: [Vec::new(), Vec::new()],
        }
    }

    /// Whether the admission ring holds a verdict for `origin -> target`.
    fn judged(&self, origin: (u16, u16), target: (u16, u16)) -> bool {
        self.glow
            .admission_log()
            .any(|a| a.origin == origin && a.target == target)
    }
}

// ---------------------------------------------------------------------------
// The real bytes.
// ---------------------------------------------------------------------------

enum Rec {
    Out(Vec<u8>),
    In(Vec<u8>),
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

/// `(ms from the first line, event)`; the stamps are microseconds.
fn recording(src: &str) -> Vec<(u64, Rec)> {
    let mut t0 = None;
    src.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, ' ');
            let us: u64 = it.next().expect("stamp").parse().expect("stamp");
            let kind = it.next().expect("kind");
            let bytes = unhex(it.next().expect("bytes"));
            let t0 = *t0.get_or_insert(us);
            let ms = (us - t0) / 1_000;
            match kind {
                "O" => (ms, Rec::Out(bytes)),
                "I" => (ms, Rec::In(bytes)),
                k => panic!("bad kind {k}"),
            }
        })
        .collect()
}

/// The typed keys of the take whose program text contains `marker`, as
/// `(key, ms of its echo)` — the echo is the next synchronized chunk.
fn take_keys(rec: &[(u64, Rec)], marker: &str) -> Vec<(char, u64)> {
    let start = rec
        .iter()
        .position(|(_, e)| {
            matches!(e, Rec::In(b) if b.windows(marker.len()).any(|w| w == marker.as_bytes()))
        })
        .expect("the take's setup text");
    let mut keys: Vec<(char, u64)> = Vec::new();
    for (i, (ms, e)) in rec.iter().enumerate().skip(start + 1) {
        let Rec::In(b) = e else { continue };
        if b.len() > 3 {
            break; // the next take's setup
        }
        let [c] = b.as_slice() else { continue };
        if !(0x20..0x7f).contains(c) {
            continue;
        }
        if keys
            .last()
            .is_some_and(|&(_, last)| ms.saturating_sub(last) > 5_000)
        {
            break; // a later probe, not this take's typing
        }
        let echo = rec[i + 1..]
            .iter()
            .find_map(|(m, e)| {
                matches!(e, Rec::Out(o) if o.starts_with(b"\x1b[?2026h")).then_some(*m)
            })
            .expect("an echo");
        keys.push((*c as char, echo));
    }
    keys
}

/// Replay the whole recording read by read, with the hints the app stamps
/// for each input (`note_typed_expected` for a printable key, `note_motion`
/// for an arrow or End, nothing for program text the driver sent), and
/// census the composer rows on every frame inside `window` (ms).
fn replay(window: (u64, u64)) -> Vec<Shot> {
    let mut h = Host::new();
    let mut shots = Vec::new();
    let shoot = |h: &mut Host, shots: &mut Vec<Shot>| {
        if (window.0..=window.1).contains(&h.ms()) {
            shots.push(h.shot(""));
        }
    };
    for (ms, ev) in recording(REC) {
        if ms > window.1 {
            break;
        }
        let t = h.t0 + Duration::from_millis(ms);
        while h.now + Duration::from_millis(FRAME_MS) <= t {
            h.now += Duration::from_millis(FRAME_MS);
            h.frame();
            shoot(&mut h, &mut shots);
        }
        h.now = h.now.max(t);
        match ev {
            Rec::In(b) => match b.as_slice() {
                [c] if (0x20..0x7f).contains(c) => {
                    let ch = *c as char;
                    let (shifted, class) = class_of(ch);
                    h.glow.note_typed_expected(h.now, 1, shifted, class, ch);
                }
                [0x7f] => h.glow.note_backspace(h.now),
                b"\x1b[D" | b"\x1b[C" | b"\x1b[F" | b"\x1b[H" => h.glow.note_motion(h.now),
                _ => {}
            },
            Rec::Out(bytes) => {
                h.term.process(&bytes);
                h.frame();
                shoot(&mut h, &mut shots);
            }
        }
    }
    shots
}

/// The shot at or just after `ms`.
fn at(shots: &[Shot], ms: u64) -> &Shot {
    shots
        .iter()
        .find(|s| s.ms >= ms)
        .unwrap_or_else(|| panic!("a frame at {ms} ms"))
}

// ---------------------------------------------------------------------------
// The synthetic composer: Claude Code's box, modelled on the capture.
// ---------------------------------------------------------------------------

/// How the composer breaks a word wider than its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wrap {
    /// Never: the word overflows (every take the recording measured).
    Word,
    /// wrap-ansi's `hard: true`, which Ink passes: a word wider than a row
    /// is broken at the row's end where moving it down would not save a
    /// row, so a token that began at the row's first text column stays and
    /// its overflow goes to the next row. Modelled, not recorded.
    Hard,
}

/// Where each char of `text` sits `(text row, grid col)`, plus the cell of
/// the caret at the end. Ink's greedy wrap as measured: a word that does
/// not fit goes down whole and the Space before it is eaten at the end of
/// the row it left; the chip's inner Space splits it like any other.
fn layout(text: &[char], wrap: Wrap) -> Vec<(usize, usize)> {
    let mut pos = Vec::with_capacity(text.len() + 1);
    let (mut row, mut len) = (0usize, 0usize);
    let hard = wrap == Wrap::Hard;
    let mut i = 0;
    while i < text.len() {
        if text[i] == ' ' {
            pos.push((row, PREFIX + len));
            len += 1;
            i += 1;
            continue;
        }
        let end = (i..text.len())
            .find(|&j| text[j] == ' ')
            .unwrap_or(text.len());
        let w = end - i;
        let down = if hard && w > TEXT_W {
            // wrap-ansi's own count: the breaks the word needs starting on
            // this row against starting on the next.
            let here = 1 + (w - (TEXT_W - len) - 1) / TEXT_W;
            let there = (w - 1) / TEXT_W;
            there < here
        } else {
            len + w > TEXT_W
        };
        if down && len > 0 {
            row += 1;
            len = 0;
        }
        for _ in i..end {
            if hard && len == TEXT_W {
                row += 1;
                len = 0;
            }
            pos.push((row, PREFIX + len));
            len += 1;
        }
        i = end;
    }
    pos.push((row, PREFIX + len));
    pos
}

fn render(text: &[char], wrap: Wrap) -> [Vec<char>; 2] {
    let pos = layout(text, wrap);
    let mut rows = [vec![' '; COLS], vec![' '; COLS]];
    rows[0][0] = '\u{276f}';
    rows[0][1] = '\u{a0}';
    for (k, &ch) in text.iter().enumerate() {
        let (r, c) = pos[k];
        if r < 2 && c < COLS {
            rows[r][c] = ch;
        }
    }
    rows
}

/// Claude Code shows the caret on the cell of the char it stands before.
fn caret_cell(text: &[char], caret: usize, wrap: Wrap) -> (u16, u16) {
    let (r, c) = layout(text, wrap)[caret];
    ((ROW1 + r) as u16, c as u16)
}

/// Claude Code's chunk for `old -> new`, as recorded: per changed row the
/// changed cell runs, the first addressed from home (`ESC[H CR ESC[nC
/// ESC[mB`), the rest by `ESC[nG` over unchanged cells, `EL` where the row
/// got shorter; the status park, the caret, inside the DEC 2026 bracket
/// with the caret hidden.
fn frame_bytes(old: &[Vec<char>; 2], new: &[Vec<char>; 2], caret: (u16, u16)) -> Vec<u8> {
    let mut s = String::from("\x1b[?2026h\x1b[?25l");
    for r in 0..2 {
        let (o, n) = (&old[r], &new[r]);
        let n_len = n.iter().rposition(|&c| c != ' ').map_or(0, |i| i + 1);
        let o_len = o.iter().rposition(|&c| c != ' ').map_or(0, |i| i + 1);
        let mut first = true;
        let goto = |s: &mut String, col: usize, first: &mut bool| {
            if *first {
                s.push_str("\x1b[H\r");
                if col > 0 {
                    s.push_str(&format!("\x1b[{col}C"));
                }
                s.push_str(&format!("\x1b[{}B", ROW1 + r));
                *first = false;
            } else {
                s.push_str(&format!("\x1b[{}G", col + 1));
            }
        };
        let mut c = 0;
        while c < n_len {
            if o[c] == n[c] {
                c += 1;
                continue;
            }
            let start = c;
            while c < n_len && o[c] != n[c] {
                c += 1;
            }
            goto(&mut s, start, &mut first);
            s.extend(n[start..c].iter());
        }
        if o_len > n_len {
            goto(&mut s, n_len, &mut first);
            s.push_str("\x1b[K");
        }
    }
    s.push_str(&format!(
        "\x1b[{ROWS};1H\x1b[{};{}H\x1b[?25h\x1b[?2026l",
        caret.0 + 1,
        caret.1 + 1
    ));
    s.into_bytes()
}

/// A take's faults, frame by frame.
#[derive(Debug, Default)]
struct Faults {
    /// `(ms, row, col)`: a glyph the hand typed 20..=800 ms ago, dark.
    dark: Vec<(u64, usize, usize)>,
    /// `(ms, col)`: light on the continuation row's blank indent.
    indent: Vec<(u64, usize)>,
    /// `(ms, row, col)`: light on a row's blank tail, one cell past its
    /// text (the band's head at a caret parked after the text is allowed)
    /// — a scrap the text left behind.
    stranded: Vec<(u64, usize, usize)>,
}

impl Faults {
    /// Grouped by cell: `(row, col) -> frames`.
    fn summary(&self) -> String {
        use std::collections::BTreeMap;
        let mut dark: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for &(_, r, c) in &self.dark {
            *dark.entry((r, c)).or_default() += 1;
        }
        let mut strand: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for &(_, r, c) in &self.stranded {
            *strand.entry((r, c)).or_default() += 1;
        }
        let mut ind: BTreeMap<usize, usize> = BTreeMap::new();
        for &(_, c) in &self.indent {
            *ind.entry(c).or_default() += 1;
        }
        format!("dark {dark:?} | indent {ind:?} | stranded {strand:?}")
    }
}

/// The synthetic take: the box on a real terminal, the composer model
/// driving it, a shot per frame.
struct Cc {
    h: Host,
    text: Vec<char>,
    /// Per char: the order the hand typed it in, `None` for program text.
    typed: Vec<Option<u64>>,
    keys: u64,
    /// The ms each typed key's echo was painted, by typing order.
    echoed: Vec<u64>,
    /// The typing order of the first key under census.
    burst: u64,
    caret: usize,
    shown: [Vec<char>; 2],
    label: String,
    shots: Vec<Shot>,
    /// PTY latency of every echo.
    echo_ms: u64,
    /// How the box breaks a word wider than its row.
    wrap: Wrap,
}

impl Cc {
    /// The box painted with `text` as program output (a paste, a recall:
    /// nothing typed), the caret at `caret`.
    fn new(text: &str, caret: usize) -> Self {
        Self::with_wrap(text, caret, Wrap::Word)
    }

    /// [`Cc::new`] with the box breaking over-wide words as `wrap` says.
    fn with_wrap(text: &str, caret: usize, wrap: Wrap) -> Self {
        let mut h = Host::new();
        h.term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
        let rule = "\u{2500}".repeat(COLS);
        h.term.process(
            format!(
                "\x1b[{ROW1};1H{rule}\x1b[{};1H{rule}\x1b[{ROWS};3H\u{23f5}\u{23f5} auto mode on",
                ROW2 + 2
            )
            .as_bytes(),
        );
        let text: Vec<char> = text.chars().collect();
        let blank = [vec![' '; COLS], vec![' '; COLS]];
        let shown = render(&text, wrap);
        h.term
            .process(&frame_bytes(&blank, &shown, caret_cell(&text, caret, wrap)));
        let typed = vec![None; text.len()];
        let mut me = Self {
            h,
            text,
            typed,
            keys: 0,
            echoed: Vec::new(),
            burst: 0,
            caret,
            shown,
            label: "start".into(),
            shots: Vec::new(),
            echo_ms: 2,
            wrap,
        };
        me.frame();
        me.idle(400);
        me
    }

    fn frame(&mut self) {
        self.h.frame();
        let mut shot = self.h.shot(&self.label);
        let pos = layout(&self.text, self.wrap);
        for (k, &ch) in self.text.iter().enumerate() {
            if let (Some(order), false) = (self.typed[k], ch == ' ') {
                let (r, c) = pos[k];
                if r < 2 {
                    shot.typed[r].push((c, order));
                }
            }
        }
        self.shots.push(shot);
    }

    fn idle(&mut self, ms: u64) {
        let t = self.h.now + Duration::from_millis(ms);
        while self.h.now + Duration::from_millis(FRAME_MS) <= t {
            self.h.now += Duration::from_millis(FRAME_MS);
            self.frame();
        }
        self.h.now = self.h.now.max(t);
    }

    /// The box repainted to the model's state: one chunk, one frame.
    fn repaint(&mut self) {
        let new = render(&self.text, self.wrap);
        let bytes = frame_bytes(
            &self.shown,
            &new,
            caret_cell(&self.text, self.caret, self.wrap),
        );
        self.shown = new;
        self.h.term.process(&bytes);
        self.frame();
    }

    /// The hint the app stamps for a typed key (`app_input.rs`:
    /// `note_typed_expected`), `gap_ms` after the last event.
    fn press(&mut self, ch: char, gap_ms: u64) {
        self.idle(gap_ms);
        self.label = format!("key {ch:?}");
        let (shifted, class) = class_of(ch);
        self.h
            .glow
            .note_typed_expected(self.h.now, 1, shifted, class, ch);
    }

    /// The pressed key's echo `echo_ms` later: the model takes the glyph at
    /// the caret and the box is repainted.
    fn echo(&mut self, ch: char) {
        self.idle(self.echo_ms);
        self.text.insert(self.caret, ch);
        self.typed.insert(self.caret, Some(self.keys));
        self.keys += 1;
        self.caret += 1;
        self.echoed.push(self.h.ms());
        self.repaint();
    }

    /// One typed key `gap_ms` after the last event, and its echo.
    fn key(&mut self, ch: char, gap_ms: u64) {
        self.press(ch, gap_ms);
        self.echo(ch);
    }

    fn type_str(&mut self, s: &str, gap_ms: u64) {
        for ch in s.chars() {
            self.key(ch, gap_ms);
        }
    }

    /// Program text at the caret (a paste the host licensed nothing for).
    fn insert(&mut self, s: &str, gap_ms: u64) {
        self.idle(gap_ms);
        self.label = "insert".into();
        for ch in s.chars() {
            self.text.insert(self.caret, ch);
            self.typed.insert(self.caret, None);
            self.caret += 1;
        }
        self.repaint();
    }

    /// Program output outside the box model (a footer, an erase), one
    /// synchronized chunk, one frame.
    fn program(&mut self, bytes: &str) {
        self.label = "program".into();
        self.h
            .term
            .process(format!("\x1b[?2026h\x1b[?25l{bytes}\x1b[?25h\x1b[?2026l").as_bytes());
        self.frame();
    }

    /// A Left arrow: the nav hint, the caret's echo. The chip is one step.
    fn left(&mut self) {
        self.idle(120);
        self.label = "left".into();
        self.h.glow.note_motion(self.h.now);
        self.idle(self.echo_ms);
        let chip: Vec<char> = CHIP.chars().collect();
        let n = chip.len();
        self.caret -= if self.caret >= n && self.text[self.caret - n..self.caret] == chip[..] {
            n
        } else {
            1
        };
        self.repaint();
    }

    /// A Right arrow: the nav hint, the caret's echo.
    fn right(&mut self) {
        self.idle(120);
        self.label = "right".into();
        self.h.glow.note_motion(self.h.now);
        self.idle(self.echo_ms);
        self.caret += 1;
        self.repaint();
    }

    /// Walk the caret back with Left to just before the chip, pause, and
    /// type `typed` at `gap_ms`; returns the first shot of the typing.
    fn insert_before_chip(&mut self, typed: &str, gap_ms: u64) -> usize {
        let chip: Vec<char> = CHIP.chars().collect();
        while !self.text[self.caret..].starts_with(&chip) {
            self.left();
        }
        self.idle(1500);
        let from = self.shots.len();
        self.burst = self.keys;
        self.type_str(typed, gap_ms);
        self.idle(600);
        from
    }

    /// The first shot after the echo of the `k`-th key of the burst.
    fn after_key(&self, k: usize) -> &Shot {
        let echo = self.echoed[self.burst as usize + k];
        self.shots
            .iter()
            .find(|s| s.ms > echo)
            .expect("a frame after the key")
    }

    /// The two rows frame by frame, for the shots inside `ms` (a failure's
    /// picture).
    fn dump(&self, ms: (u64, u64)) -> String {
        let mut out = String::new();
        for s in self.shots.iter().filter(|s| (ms.0..=ms.1).contains(&s.ms)) {
            for r in 0..2 {
                out.push_str(&format!(
                    "{:>6} {:<9} r{} |{}|\n{:>16} r{} |{}|\n",
                    s.ms,
                    s.label,
                    ROW1 + r,
                    s.text[r].trim_end(),
                    "",
                    ROW1 + r,
                    map(&s.lit[r])
                ));
            }
        }
        out
    }

    /// What the take did wrong from shot `from` on.
    fn faults(&self, from: usize) -> Faults {
        let mut f = Faults::default();
        for s in &self.shots[from..] {
            for r in 0..2 {
                for &(col, order) in s.typed[r].iter().filter(|&&(_, o)| o >= self.burst) {
                    let age = s.ms.saturating_sub(self.echoed[order as usize]);
                    if !s.lit[r][col] && (20..=800).contains(&age) {
                        f.dark.push((s.ms, ROW1 + r, col));
                    }
                }
                let end = s.text[r].trim_end().chars().count();
                for c in end + 1..COLS {
                    if s.lit[r][c] {
                        f.stranded.push((s.ms, ROW1 + r, c));
                    }
                }
            }
            for c in 0..PREFIX {
                if s.lit[1][c] {
                    f.indent.push((s.ms, c));
                }
            }
        }
        f
    }
}

/// Row 1 painted as program text, the chip after it, the caret at the end.
fn painted(row1: &str) -> Cc {
    let full = format!("{row1} {CHIP} in the");
    Cc::new(&full, full.chars().count())
}

/// Row 1 TYPED by the hand up to its last word, the Space after it and the
/// chip arriving as program text (a paste): the hand's last typed Space is
/// the one before `have`.
fn typed_but_the_last_space(row1: &str) -> Cc {
    let mut cc = Cc::new("", 0);
    cc.type_str(row1, 110);
    cc.insert(&format!(" {CHIP} in the"), 300);
    cc
}

/// A key pressed mid-line on row 1 — `x` before `prompt` — whose press is
/// answered FIRST by a program chunk that parks the caret one row down
/// after writing `glyph` there (a footer, a status line), erasing row 1
/// from the caret with it when `erase_tail`; the key's own echo follows
/// 60 ms later. Returns the take and the column the `x` lands at.
fn footer_before_the_echo(glyph: char, erase_tail: bool) -> (Cc, usize) {
    let text = "the aterm window had this prompt appear";
    let at = text.find("prompt").expect("the word");
    let mut cc = Cc::new(text, at);
    cc.press('x', 300);
    cc.idle(2);
    let erase = if erase_tail {
        format!("\x1b[{};{}H\x1b[K", ROW1 + 1, PREFIX + at + 1)
    } else {
        String::new()
    };
    cc.program(&format!("{erase}\x1b[{};30H{glyph}", ROW2 + 1));
    cc.idle(60);
    // Claude Code repaints its box whole: the row as the model has it.
    cc.shown = [vec![' '; COLS], vec![' '; COLS]];
    cc.echo_ms = 0;
    cc.echo('x');
    cc.idle(400);
    (cc, PREFIX + at)
}

// ---------------------------------------------------------------------------
// A — the key that pushes the word down is judged, not parked.
// ---------------------------------------------------------------------------

/// **THE FIRST KEY THAT PUSHES THE GLUED WORD DOWN IS JUDGED, NOT
/// PARKED.** The owner's final geometry: `have [Image` fills row 1, the
/// first `a` sends `a[Image` to row 2. The fold `(16,56) → (17,3)` reaches
/// the admission ring, and every glyph typed on row 2 is lit from 20 ms
/// after its echo.
///
/// RED before `CursorGlow::key_pushed_text_down`: the fold was held as a
/// cross-row footer park and `spawn` swallowed every move after it for the
/// park's patience — no verdict, every glyph dark.
#[test]
fn the_first_key_that_pushes_a_glued_word_down_is_judged_not_parked() {
    let mut cc = painted(ROW1_EXACT);
    let from = cc.insert_before_chip(&TYPED[..1], 110);
    assert!(
        cc.shots[from..]
            .iter()
            .any(|s| s.text[1].starts_with("  a[Image #1] in the")),
        "the take reached the measured fold"
    );
    // Read before the rest of the insert can push the fold's row out of
    // the admission ring.
    let judged = cc.h.judged((16, 56), (17, 3));
    let verdicts: Vec<_> =
        cc.h.glow
            .admission_log()
            .map(|a| (a.origin, a.target, a.reason))
            .collect();
    cc.type_str(&TYPED[1..], 110);
    cc.idle(600);
    let f = cc.faults(from);
    assert!(
        judged,
        "the fold (16,56) -> (17,3) never reached a verdict: {verdicts:?}\n{}",
        f.summary()
    );
    assert!(
        f.dark.is_empty() && f.indent.is_empty(),
        "{} dark typed-glyph frames — {}",
        f.dark.len(),
        f.summary()
    );
}

/// The same recorded composer fold with a delayed first echo. A key may
/// remain unpaid for ten seconds, but the exact-glyph witness used to
/// distinguish this fold from a foreign footer currently expires at 250 ms.
/// At 300 ms the fold is held as a cross-row park and every later key can
/// disappear behind its custody even though the source tail moved with the
/// key's glyph.
#[test]
fn a_delayed_first_key_still_carries_the_glued_word_down() {
    let mut cc = painted(ROW1_EXACT);
    cc.echo_ms = 300;
    let from = cc.insert_before_chip(&TYPED[..1], 110);
    assert!(
        cc.h.judged((16, 56), (17, 3)),
        "the 300 ms fold was held rather than judged: {:?}",
        cc.h.glow.admission_log().collect::<Vec<_>>()
    );
    let faults = cc.faults(from);
    assert!(
        faults.dark.is_empty(),
        "{}; verdicts {:?}",
        faults.summary(),
        cc.h.glow.admission_log().collect::<Vec<_>>()
    );
}

/// NEGATIVE CONTROL: a footer that happens to show the pressed key's own
/// glyph, with the input row untouched, is still a footer. The park keeps
/// custody of the press: the row below never lights, and the key's real
/// echo on row 1 is laid. Pins the conjunction: GREEN on the unfixed tree,
/// and RED (the footer lit, the echo dark) if the glyph witness alone
/// refused the park.
#[test]
fn a_footer_showing_the_keys_own_glyph_keeps_the_parks_custody() {
    let (cc, col) = footer_before_the_echo('x', false);
    let footer = cc.shots.iter().filter(|s| s.lit[1].contains(&true)).count();
    let key = cc.echoed[0];
    assert_eq!(
        footer,
        0,
        "the footer spent the key\n{}",
        cc.dump((key.saturating_sub(80), key + 32))
    );
    let last = cc.shots.last().expect("a frame");
    assert!(
        last.text[0].contains("had this xprompt") && last.lit[0][col],
        "the key's own echo went dark: r16 |{}|",
        map(&last.lit[0])
    );
}

/// NEGATIVE CONTROL: the input row's tail erased from the caret while the
/// caret parks one row down after a glyph the key did NOT stamp (a ghost
/// suggestion cleared, a status line drawn) is not the key's echo either:
/// the row below never lights. GREEN on the unfixed tree, and RED if the
/// erased tail alone refused the park.
#[test]
fn an_erased_tail_under_a_foreign_footer_glyph_keeps_the_parks_custody() {
    let (cc, col) = footer_before_the_echo('.', true);
    let footer = cc.shots.iter().filter(|s| s.lit[1].contains(&true)).count();
    assert_eq!(footer, 0, "the footer spent the key");
    let last = cc.shots.last().expect("a frame");
    assert!(
        last.lit[0][col],
        "the key's own echo went dark: r16 |{}|",
        map(&last.lit[0])
    );
}

/// A program footer can print the SAME glyph as the unpaid key while erasing
/// the input row's tail. Those two observations alone do not prove that the
/// typed word moved: the old tail must also follow the landing glyph.
#[test]
fn an_erased_tail_under_a_same_glyph_footer_keeps_the_parks_custody() {
    let (cc, col) = footer_before_the_echo('x', true);
    let footer = cc.shots.iter().filter(|s| s.lit[1].contains(&true)).count();
    assert_eq!(footer, 0, "the footer spent the key");
    let last = cc.shots.last().expect("a frame");
    assert!(last.lit[0][col], "the key's echo went dark");
}

// ---------------------------------------------------------------------------
// B1, B2 — the moved word is measured from where the hand's run began.
// ---------------------------------------------------------------------------

/// **B1: THE REFLOWED WORD KEEPS ITS FIRST GLYPH, AND THE ROW IT LEFT KEEPS
/// NO SCRAP.** `a` fits at row 1's end; `l` moves `al[Image` down. After
/// `l`'s echo the `a` at (17, 2) is lit for as long as its band lives, and
/// the old `a` cell on row 1 — its glyph gone — is dark once a retract fade
/// has passed.
///
/// RED before `Ribbon::arrived`: `moved_word_len` found no typed Space on
/// row 16, `w = 0`, nothing was relaid and the old cell flowed in place:
/// (17, 2) dark in every frame of its band's life, (16, 55) lit ~0.7 s.
#[test]
fn a_word_begun_at_a_navigated_caret_keeps_its_first_glyph_when_it_wraps_down() {
    let mut cc = painted(ROW1_ONE_FITS);
    let from = cc.insert_before_chip(TYPED, 110);
    let wrap = cc.echoed[cc.burst as usize + 1];
    let s = cc.after_key(1);
    assert!(
        s.text[1].starts_with("  al[Image #1] in the") && s.text[0].trim_end().ends_with("do have"),
        "the take reached the measured wrap: {:?}",
        s.text
    );
    let f = cc.faults(from);
    let first = f
        .dark
        .iter()
        .filter(|&&(_, r, c)| (r, c) == (ROW2, PREFIX))
        .count();
    assert_eq!(
        first,
        0,
        "the reflowed word's first glyph (17, 2) was dark in {first} frames — {}\n{}",
        f.summary(),
        cc.dump((wrap.saturating_sub(40), wrap + 80))
    );
    let late = wrap + (RETRACT_FADE_S * 1000.0) as u64 + 2 * FRAME_MS;
    let scraps: Vec<_> = f
        .stranded
        .iter()
        .filter(|&&(ms, r, _)| r == ROW1 && ms >= late)
        .collect();
    assert!(
        scraps.is_empty(),
        "light stayed on row 1's blank tail in {} frames after the word left it: {:?}",
        scraps.len(),
        &scraps[..scraps.len().min(6)]
    );
}

/// **B2: NOTHING IS LAID ON THE CONTINUATION ROW'S BLANK INDENT.** The hand
/// typed row 1, but the Space after its last word came with the pasted
/// chip, so its last TYPED Space is the one before `have` — stale by the
/// time it navigates back and types. No frame lights columns 0–1 of row 2.
///
/// RED before `Ribbon::arrived`: `w` was measured from the stale Space (5
/// for 1), the relocation went pending and settled `[glyph − w, from_col)`
/// down to the pane's first column: columns 0 and 1 lit.
#[test]
fn a_stale_typed_space_lays_nothing_on_the_continuation_indent() {
    let mut cc = typed_but_the_last_space(ROW1_ONE_FITS);
    let from = cc.insert_before_chip(TYPED, 110);
    let f = cc.faults(from);
    assert!(
        f.indent.is_empty(),
        "the continuation row's indent was lit in {} frames (first at {} ms) — {}",
        f.indent.len(),
        f.indent.first().map_or(0, |i| i.0),
        f.summary()
    );
    let first = f
        .dark
        .iter()
        .filter(|&&(_, r, c)| (r, c) == (ROW2, PREFIX))
        .count();
    assert_eq!(first, 0, "{}", f.summary());
}

// ---------------------------------------------------------------------------
// B3 — a move inside the hand's own lit word does not cut the measure.
// ---------------------------------------------------------------------------

/// Program text the hand types after: row 1's columns 2..=41.
const PROG: &str = "the aterm window had this prompt appear.";

/// Frames of a census, as `(ms after the event, columns)`.
type FrameCols = Vec<(u64, Vec<usize>)>;

/// A re-wrap's census from the wrap key's echo at `wrap` ms. `dark`: the
/// frames 30..=400 ms after it in which a column of the moved word's
/// letters on row 2 (`word`) is dark. `stranded`: the frames from two
/// retract fades after it (the moved cells' own fade, and one more) to
/// 1.5 s in which row 1 is lit past its text.
fn rewrap_faults(
    cc: &Cc,
    wrap: u64,
    word: std::ops::RangeInclusive<usize>,
) -> (FrameCols, FrameCols) {
    let late = wrap + 2 * (RETRACT_FADE_S * 1000.0) as u64 + 3 * FRAME_MS;
    let mut dark = Vec::new();
    let mut stranded = Vec::new();
    for s in cc.shots.iter().filter(|s| s.ms > wrap) {
        if (wrap + 30..=wrap + 400).contains(&s.ms) {
            let cols: Vec<usize> = word.clone().filter(|&c| !s.lit[1][c]).collect();
            if !cols.is_empty() {
                dark.push((s.ms - wrap, cols));
            }
        }
        if (late..=wrap + 1_500).contains(&s.ms) {
            let end = s.text[0].trim_end().chars().count();
            let cols: Vec<usize> = (end + 1..COLS).filter(|&c| s.lit[0][c]).collect();
            if !cols.is_empty() {
                stranded.push((s.ms - wrap, cols));
            }
        }
    }
    (dark, stranded)
}

/// Type `word` after [`PROG`], let `edit` walk and finish it, then press
/// `key`: the word no longer fits row 1 and Claude Code moves it whole to
/// row 2, the caret after `key`. Returns the take and the wrap key's echo.
fn word_then_wrap(word: &str, edit: impl FnOnce(&mut Cc), key: char) -> (Cc, u64) {
    let mut cc = Cc::new(PROG, PROG.chars().count());
    cc.burst = cc.keys;
    cc.type_str(word, 110);
    edit(&mut cc);
    let k = cc.echoed.len();
    cc.key(key, 110);
    let wrap = cc.echoed[k];
    cc.idle(1_600);
    let s = cc
        .shots
        .iter()
        .find(|s| s.ms > wrap)
        .expect("a frame after the wrap");
    assert!(
        s.text[1].starts_with("  crates/aterm-effects"),
        "the take reached the wrap: {:?}",
        s.text
    );
    (cc, wrap)
}

/// Assert [`rewrap_faults`] found nothing.
fn assert_rewrap_clean(cc: &Cc, wrap: u64, word: std::ops::RangeInclusive<usize>, what: &str) {
    let (dark, stranded) = rewrap_faults(cc, wrap, word);
    assert!(
        dark.is_empty() && stranded.is_empty(),
        "{what}: the moved word dark in {} frames {:?}; row 1 lit past its text in {} frames \
         {:?}\n{}",
        dark.len(),
        &dark[..dark.len().min(3)],
        stranded.len(),
        &stranded[..stranded.len().min(3)],
        cc.dump((wrap.saturating_sub(20), wrap + 120))
    );
}

/// **B3: A TYPO FIXED INSIDE THE WORD BEING TYPED DOES NOT CUT ITS
/// MEASURE.** ` crates/aterm-efects` typed after the program text fills
/// row 1 to its last column; Left ×4 walks back over the hand's own lit
/// letters and the missing `f` is inserted. `crates/aterm-effects` no longer
/// fits and goes down whole, the caret after the `f` at (17, 18). Every
/// letter of the moved word before the caret is lit from 30 ms after the
/// wrap, and row 1's blank tail is dark two retract fades later.
///
/// RED when `Ribbon::arrived` cut the measure at any deliberate move: the
/// Left ×4 left `arrived` at the insert's own column, so the word measured
/// `0` from it instead of 15 from the typed Space — row 2's columns 2..=16
/// dark for the whole window, and the old cells flowing on row 1's blank
/// tail to ~1.3 s instead of retracting with their glyphs.
#[test]
fn a_typo_fixed_inside_the_word_keeps_the_word_lit_when_it_wraps_down() {
    let (cc, wrap) = word_then_wrap(
        " crates/aterm-efects",
        |cc| {
            for _ in 0..4 {
                cc.left();
            }
            cc.idle(80);
        },
        'f',
    );
    assert_rewrap_clean(&cc, wrap, 2..=17, "typo");
}

/// **B3: A SCRUB OVER THE WORD BEING TYPED DOES NOT CUT ITS MEASURE.**
/// ` crates/aterm-eff` typed, Left ×2 and Right ×2 over the hand's own lit
/// letters, then `ect` and `s`: the word no longer fits and goes down whole.
/// Every letter of it on row 2 is lit, and row 1's blank tail goes dark.
///
/// RED when the Right ×2 set `arrived` at the word's typed end: the measure
/// started there, and row 2's columns 2..=17 stayed dark.
#[test]
fn a_scrub_over_the_word_keeps_the_word_lit_when_it_wraps_down() {
    let (cc, wrap) = word_then_wrap(
        " crates/aterm-eff",
        |cc| {
            cc.left();
            cc.left();
            cc.right();
            cc.right();
            cc.type_str("ect", 150);
        },
        's',
    );
    assert_rewrap_clean(&cc, wrap, 2..=21, "scrub");
}

/// CONTROL: the same word typed straight through after the hand's own
/// Space (the measure `moved_word_len` was written for) goes down lit.
#[test]
fn control_a_word_typed_straight_through_wraps_down_lit() {
    let (cc, wrap) = word_then_wrap(" crates/aterm-effect", |_| {}, 's');
    assert_rewrap_clean(&cc, wrap, 2..=21, "straight");
}

// ---------------------------------------------------------------------------
// B4 — a token the box hard-breaks moved nothing down.
// ---------------------------------------------------------------------------

/// **B4: A TOKEN THE BOX HARD-BREAKS MOVED NOTHING DOWN.** The hand walks
/// Left to the start of row 1, over program text (a real arrival at
/// (16, 2)), and types one unbroken token glued to `hello`. The 60th key
/// fills row 1's last text column and the box breaks the token there
/// ([`Wrap::Hard`]): the key's glyph is drawn at (16, 61), over the `h` it
/// pushed on, and the caret ALONE goes to (17, 2), before `hello` — the
/// soft-wrapped caret, over a cell that held a glyph rather than a blank.
/// Every typed glyph stays lit for its band's life (the row flows into the
/// fold), and nothing lights the continuation row's indent.
///
/// RED before: the soft-wrap verdict read only a blank origin, so the move
/// was the coalesced fold's. Its one cell was laid on the indent at
/// (17, 1) and the key's glyph at (16, 61) stayed dark (as on `ac5b4c144`);
/// and with `Ribbon::arrived`, the re-wrap relay measured a 59-letter moved
/// word from the arrival and fast-retracted row 1's whole band — 0 of 60
/// lit 400 ms after the wrap — for a relocation that could never settle.
#[test]
fn a_token_the_box_hard_breaks_keeps_its_band_on_the_row_it_fills() {
    let mut cc = Cc::with_wrap("hello", 5, Wrap::Hard);
    for _ in 0..5 {
        cc.left();
    }
    cc.idle(1_500);
    let from = cc.shots.len();
    cc.burst = cc.keys;
    let token: String = (b'a'..=b'z').cycle().take(59).map(char::from).collect();
    cc.type_str(&token, 110);
    let k = cc.echoed.len();
    cc.key('q', 110);
    let wrap = cc.echoed[k];
    cc.idle(1_600);
    let s = cc
        .shots
        .iter()
        .find(|s| s.ms > wrap)
        .expect("a frame after the wrap");
    assert!(
        s.caret == (ROW2 as u16, PREFIX as u16)
            && s.text[0].trim_end().ends_with("gq")
            && s.text[1].starts_with("  hello"),
        "the take reached the hard break: caret {:?} {:?}",
        s.caret,
        s.text
    );
    let f = cc.faults(from);
    assert!(
        f.dark.is_empty() && f.indent.is_empty() && f.stranded.is_empty(),
        "{}\n{}",
        f.summary(),
        cc.dump((wrap.saturating_sub(20), wrap + 64))
    );
}

// ---------------------------------------------------------------------------
// C — the lifted word goes up with its light.
// ---------------------------------------------------------------------------

/// **C: A SPACE THAT LETS THE WORD FIT THE ROW ABOVE AGAIN CARRIES ITS BAND
/// UP.** `already` typed on row 2 before the chip, then the Space: Ink
/// writes `already` at row 1's end and the caret stays before `[Image`. On
/// the frame after, every glyph of the lifted word on row 1 is lit; from
/// the Space on, no live cell stands on row 2 at or right of the caret (the
/// word's old cells leave with it, from under the chip's text); and the
/// indent stays dark throughout.
///
/// RED before `CursorGlow::lifted_word`: the move was held as a park and
/// then drained as a same-row re-anchor — (16,55..=61) dark.
#[test]
fn a_space_that_lets_the_word_fit_the_row_above_carries_its_band_up() {
    let mut cc = painted(ROW1_ONE_FITS);
    let from = cc.insert_before_chip(TYPED, 110);
    let space = TYPED.find(' ').expect("the Space after `already`");
    let lift = cc.echoed[cc.burst as usize + space];
    let s = cc.after_key(space);
    assert!(
        s.text[0].trim_end().ends_with("do have already")
            && s.text[1].starts_with("  [Image #1] in the"),
        "the take reached the measured lift: {:?}",
        s.text
    );
    let s = at(&cc.shots, lift + 2 * FRAME_MS);
    let dark: Vec<usize> = LIFTED.filter(|&c| !s.lit[0][c]).collect();
    assert!(
        dark.is_empty(),
        "the lifted word is dark at {dark:?} {} ms after the Space\n{}",
        s.ms - lift,
        cc.dump((lift.saturating_sub(20), lift + 2 * FRAME_MS))
    );
    let ahead: Vec<(u64, Vec<usize>)> = cc
        .shots
        .iter()
        .filter(|s| s.ms >= lift && s.ms <= lift + 400)
        .map(|s| (s.ms, s.live_ahead_on_row2()))
        .filter(|(_, cols)| !cols.is_empty())
        .collect();
    assert!(
        ahead.is_empty(),
        "a live cell stayed on row 2 at or right of the caret, over the chip the word \
         left: {:?}",
        &ahead[..ahead.len().min(4)]
    );
    let f = cc.faults(from);
    assert!(f.indent.is_empty(), "{}", f.summary());
}

// ---------------------------------------------------------------------------
// D — a soft-wrapped word comes down only when its text did.
// ---------------------------------------------------------------------------

/// Program text to row 1's column 59; ` I` typed after it puts the `I` on
/// row 1's last text column.
const PROG_58: &str = "the aterm window had this prompt appear. you do have a lot";

/// The soft-wrapped caret `claude_wrap_band.rs` records, in the synthetic
/// box: ` I` typed after [`PROG_58`], the `I` drawn AT (16, 61) and the
/// caret alone wrapped to the continuation row's indent, (17, 2). Then
/// `keys` pressed 4 ms apart and ONE chunk `echo` (after the box's own
/// `?2026` bracket) answering them all. Returns the take and the ms of
/// that chunk.
fn soft_wrap_then(keys: &str, echo: &str) -> (Cc, u64) {
    let mut cc = Cc::new(PROG_58, PROG_58.chars().count());
    cc.key(' ', 110);
    cc.press('I', 110);
    cc.idle(2);
    cc.program(&format!(
        "\x1b[{};{}HI\x1b[{};{}H",
        ROW1 + 1,
        PREFIX + TEXT_W,
        ROW2 + 1,
        PREFIX + 1
    ));
    let s = cc.shots.last().expect("a frame");
    assert!(
        s.caret == (ROW2 as u16, PREFIX as u16) && s.text[0].trim_end().ends_with("a lot I"),
        "the take reached the soft-wrapped caret: {:?} {:?}",
        s.caret,
        s.text
    );
    let mut gap = 110;
    for ch in keys.chars() {
        cc.press(ch, gap);
        gap = 4;
    }
    cc.idle(2);
    cc.program(echo);
    let ms = cc.h.ms();
    cc.idle(300);
    (cc, ms)
}

/// **D: THE KEY THAT REFLOWS THE SOFT-WRAPPED WORD CARRIES ITS LIGHT
/// DOWN** (`claude_wrap_band.rs`'s owner shape, in the synthetic box). The
/// `b` after the soft-wrapped `I` makes `Ib` one word: Claude Code erases
/// the `I` from row 1's end and writes `Ib` from the indent, caret
/// (17, 4). The `I`'s light leaves row 1 with its glyph and is relaid under
/// it at (17, 2) (`Ribbon::settle_carry`).
#[test]
fn a_soft_wrapped_word_the_next_key_reflows_comes_down_with_its_light() {
    let echo = format!(
        "\x1b[{};{}H\x1b[K\x1b[{};{}HIb\x1b[{};{}H",
        ROW1 + 1,
        PREFIX + TEXT_W,
        ROW2 + 1,
        PREFIX + 1,
        ROW2 + 1,
        PREFIX + 3
    );
    let (cc, ms) = soft_wrap_then("b", &echo);
    let s = cc
        .shots
        .iter()
        .find(|s| s.ms >= ms)
        .expect("the echo's frame");
    assert!(
        s.text[1].starts_with("  Ib") && s.caret == (ROW2 as u16, PREFIX as u16 + 2),
        "the take reached the reflow: {:?} {:?}",
        s.caret,
        s.text
    );
    assert!(
        s.live[1].contains(&PREFIX) && !s.live[0].contains(&(PREFIX + TEXT_W - 1)),
        "the `I`'s light did not come down with it: row 1 live {:?}, row 2 live {:?}",
        s.live[0],
        s.live[1]
    );
}

/// **D: A WORD THAT STAYED ON ITS ROW KEEPS ITS LIGHT THERE.** The same
/// soft-wrapped `I`, then a Space and a `b` answered by ONE chunk (a burst,
/// or a paste): `I` stays at row 1's end, the Space and the `b` are written
/// from the indent, and the caret hops (17, 2) → (17, 4) — past the word
/// plus one glyph, the hop the reflow makes. The `I`'s cell on row 1 stays
/// live: the glass shows its word never left.
///
/// RED when `Ribbon::settle_carry` read the hop alone: it retracted the
/// `I`'s light from row 1, under a glyph that never moved, and relaid it
/// under the Space at (17, 2).
#[test]
fn a_soft_wrapped_word_that_stays_on_its_row_keeps_its_light_there() {
    let echo = format!(
        "\x1b[{};{}H b\x1b[{};{}H",
        ROW2 + 1,
        PREFIX + 1,
        ROW2 + 1,
        PREFIX + 3
    );
    let (cc, ms) = soft_wrap_then(" b", &echo);
    let s = cc
        .shots
        .iter()
        .find(|s| s.ms >= ms)
        .expect("the echo's frame");
    assert!(
        s.text[0].trim_end().ends_with("a lot I")
            && s.text[1].starts_with("   b")
            && s.caret == (ROW2 as u16, PREFIX as u16 + 2),
        "the take reached the burst: {:?} {:?}",
        s.caret,
        s.text
    );
    let gone: Vec<u64> = cc
        .shots
        .iter()
        .filter(|s| s.ms >= ms && s.ms <= ms + 100)
        .filter(|s| !s.live[0].contains(&(PREFIX + TEXT_W - 1)))
        .map(|s| s.ms - ms)
        .collect();
    assert!(
        gone.is_empty(),
        "the `I` that stayed at (16, 61) lost its live cell {gone:?} ms after the burst\n{}",
        cc.dump((ms.saturating_sub(40), ms + 48))
    );
}

// ---------------------------------------------------------------------------
// The same shapes in Claude Code's own bytes.
// ---------------------------------------------------------------------------

/// **THE SAME FAULTS IN CLAUDE CODE'S OWN BYTES.** The recording's take 3
/// (the owner's geometry, `you did have`) and take 4 (one column more, `you
/// do have`), replayed read by read with the hints the app stamps.
///
/// RED on the unfixed tree: take 3 — all 20 typed glyph columns dark 100 ms
/// after the last key (A); take 4 — (17, 2) dark after `l`'s echo and
/// (16, 55) still lit a retract fade later (B1), and the lifted `already`
/// dark on row 1 after the Space (C).
#[test]
fn claude_codes_recorded_reflow_keeps_every_typed_glyph_lit_and_strands_nothing() {
    let rec = recording(REC);
    let exact = take_keys(&rec, "you did have");
    let one = take_keys(&rec, "you do have");
    assert_eq!(exact.len(), TYPED.len(), "take 3's keys: {exact:?}");
    assert_eq!(one.len(), 13, "take 4's keys: {one:?}");
    let shots = replay((exact[0].1, one.last().expect("keys").1 + 1_000));
    // Take 3: the typed text stands on row 2 at columns 2..=24.
    let end = at(&shots, exact.last().expect("keys").1 + 100);
    assert!(
        end.text[1].starts_with("  already been runnignin [Image #1] in the"),
        "take 3 reached the owner's screen: {:?}",
        end.text
    );
    let glyphs: Vec<usize> = TYPED
        .char_indices()
        .filter(|&(_, c)| c != ' ')
        .map(|(i, _)| PREFIX + i)
        .collect();
    let dark: Vec<usize> = glyphs.iter().copied().filter(|&c| !end.lit[1][c]).collect();
    // Take 4: `l` (the 2nd key) moved `a` down; the Space (the 8th) lifted
    // `already` back up.
    let wrap = one[1].1;
    let s = at(&shots, wrap + 50);
    let late = at(&shots, wrap + (RETRACT_FADE_S * 1000.0) as u64 + 50);
    assert!(
        s.text[1].starts_with("  al[Image #1] in the"),
        "take 4 reached the wrap: {:?}",
        s.text
    );
    let (space, lift) = one[7];
    assert_eq!(space, ' ', "take 4's 8th key");
    let lifted = at(&shots, lift + 50);
    assert!(
        lifted.text[0].trim_end().ends_with("do have already"),
        "take 4 reached the lift: {:?}",
        lifted.text
    );
    let unlit: Vec<usize> = LIFTED.filter(|&c| !lifted.lit[0][c]).collect();
    let faults = [
        (!dark.is_empty()).then(|| {
            format!(
                "take 3 (the owner's geometry): {} of {} typed glyphs dark 100 ms after the last \
                 key {dark:?}\n  r17 |{}|",
                dark.len(),
                glyphs.len(),
                map(&end.lit[1])
            )
        }),
        (!s.lit[1][PREFIX]).then(|| {
            format!(
                "take 4: the reflowed word's first glyph (17, 2) dark 50 ms after the wrap\n  \
                 r17 |{}|",
                map(&s.lit[1])
            )
        }),
        late.lit[0][55].then(|| {
            format!(
                "take 4: the moved `a`'s old cell (16, 55) still lit {} ms after its glyph left\n  \
                 r16 |{}|",
                late.ms - wrap,
                map(&late.lit[0])
            )
        }),
        (!unlit.is_empty()).then(|| {
            format!(
                "take 4: the lifted `already` dark at {unlit:?} 50 ms after the Space\n  r16 |{}|",
                map(&lifted.lit[0])
            )
        }),
    ];
    let faults: Vec<String> = faults.into_iter().flatten().collect();
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

// ---------------------------------------------------------------------------
// Controls: GREEN on the unfixed tree, and must stay green.
// ---------------------------------------------------------------------------

/// The same wrap with the hand's typed Space EXACTLY before the moved word
/// (the case `moved_word_len` was written for): the letter is relaid, the
/// indent stays dark, row 1 keeps no scrap past a retract fade.
#[test]
fn control_a_word_after_the_hands_own_space_is_relaid_exactly() {
    let mut cc = Cc::new("", 0);
    cc.type_str(&format!("{ROW1_ONE_FITS} "), 110);
    cc.insert(&format!("{CHIP} in the"), 300);
    let from = cc.insert_before_chip(TYPED, 110);
    let wrap = cc.echoed[cc.burst as usize + 1];
    let late = wrap + (RETRACT_FADE_S * 1000.0) as u64 + 2 * FRAME_MS;
    let f = cc.faults(from);
    assert!(
        !f.dark.iter().any(|&(ms, r, _)| r == ROW2 && ms > wrap),
        "{}",
        f.summary()
    );
    assert!(f.indent.is_empty(), "{}", f.summary());
    assert!(
        !f.stranded.iter().any(|&(ms, r, _)| r == ROW1 && ms >= late),
        "{}",
        f.summary()
    );
}

/// With the chip already on row 2 (row 1 too long for `[Image`), the whole
/// insert happens on row 2 with no fold and no lift: every glyph lit,
/// nothing stranded, nothing on the indent.
#[test]
fn control_an_insert_that_never_folds_keeps_its_band_whole() {
    let mut cc = painted("the aterm window had this prompt appear. you had to have");
    let from = cc.insert_before_chip(TYPED, 110);
    let f = cc.faults(from);
    assert!(
        f.dark.is_empty() && f.indent.is_empty() && f.stranded.is_empty(),
        "{}",
        f.summary()
    );
}

/// **DIAGNOSTIC** (ignored; `-- --ignored --nocapture`): the fault census
/// over geometries (0, 1, 2 and 5 letters of `already` fitting row 1 before
/// the wrap), how row 1 came to be (painted / typed but its last Space /
/// typed with its last Space) and the typing cadence. What it still prints
/// is by design: the moved cells' retract fade on row 1's blank tail right
/// after the wrap (one fade, `RETRACT_FADE_S`), and at the fast cadence the
/// lifted word flowing out of row 1 from its left end (the census counts a
/// flowing glyph dark inside its 800 ms window).
#[test]
#[ignore = "diagnostic census, prints; the laws above pin the faults"]
fn diagnostic_sweep() {
    for row1 in [
        ROW1_EXACT,
        ROW1_ONE_FITS,
        "the aterm window had this prompt appear. we do have",
        "the aterm window had this prompt appear. so have",
    ] {
        for how in ["painted", "typed-but-space", "typed-with-space"] {
            for gap in [110u64, 60] {
                let mut cc = match how {
                    "painted" => painted(row1),
                    "typed-but-space" => typed_but_the_last_space(row1),
                    _ => {
                        let mut cc = Cc::new("", 0);
                        cc.type_str(&format!("{row1} "), 110);
                        cc.insert(&format!("{CHIP} in the"), 300);
                        cc
                    }
                };
                let from = cc.insert_before_chip(TYPED, gap);
                println!(
                    "{:>2} {how:<16} gap={gap:<3} {}",
                    row1.len(),
                    cc.faults(from).summary()
                );
            }
        }
    }
}
