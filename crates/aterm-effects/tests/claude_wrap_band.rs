// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A WORD CARRIED DOWN BY A SOFT-WRAPPED CARET KEEPS ITS BAND, AND ONLY
//! ITS BAND** (2026-09-23 — the owner's screenshot, Claude Code's composer:
//! `› the other aterm window had this prompt appear asking for permission.
//! however, your aterm claude code harness should have` filling row 1 and
//! `  already been runnignin █[Image #1] in the` on row 2, the words typed
//! INTO the line at the fold. The band under `already…` starts under the
//! `l`, on a fresh red walk; the `a` at column 2 is dark; a detached stub
//! sits in the blank indent at columns 0–1).
//!
//! Every take here is REAL Claude Code (v2.1.280, the fullscreen composer on
//! the alternate screen) recorded under `docs/design/repro/ptyrec.py` inside a
//! headless aterm 0.91.0 at 40×120 (7×14 px cells), driven through the
//! keyboard seam with `aterm ctl key`; nothing was ever submitted. The line
//! is typed, the caret is walked back (Left, or Alt+Left/Alt+Right word
//! hops), and ` already been runnignin` is typed at 8–12 cps:
//!
//! * `row1-end` — THE OWNER'S SHAPE. The caret after `have`, the last word of
//!   row 1. The Space moves the caret to column 117, the last text column.
//!   The `a` is written AT (36,117) and the caret ALONE soft-wraps to the
//!   continuation row's indent: `ESC[H CR ESC[117C ESC[36B a … ESC[38;3H`,
//!   caret (37,2). The `l` erases the `a` from row 1 and writes `al` from
//!   the indent: `ESC[117C ESC[36B ESC[K CR ESC[2C ESC[1B al [image #1]…
//!   ESC[38;5H`, caret (37,4). The partial word sat at the end of row 1 for
//!   one key, then the whole word went down.
//! * `row1-end-six` — the same with nine columns of room: `alread` is typed
//!   on row 1, its `d` soft-wraps the caret to (37,2), and the `y` moves the
//!   whole word to (37,2..8), caret (37,9).
//! * `hops`, `hops-fast` — `row1-end` reached the owner's way: Alt+Left word
//!   hops to the start of row 1, Alt+Right word hops back to `have` (250 ms
//!   and 140 ms apart; the fast take types at once, while the hops' wake
//!   still stands on row 1).
//! * `eol-caret` — the caret before `[image`, which stood at the END of row 1
//!   (caret (36,112)): the first key erases row 1 from 112 and writes
//!   `a[image #1]` from (37,2), caret (37,3) — a one-row-down typed move whose
//!   landing row holds the key's glyph. A separate root cause: the seam held
//!   that move as a background footer park and swallowed the whole insert
//!   (row 2 live `[]` throughout, every mode); it is judged at once now
//!   (`CursorGlow::key_pushed_text_down`).
//! * `row2-start` (control) — the caret before `[image`, which already stood
//!   at the start of row 2: an in-place insert from (37,2), no row change.
//! * `end` (control) — typing at the end of the text; the box grows a row
//!   on the Space after `[image` (the 2.1.278 box-growth shape
//!   `composer_box_growth_wrap.rs` models) and `#1]` lands on row 2.
//!
//! **THE MECHANISM.** The engine modelled a composer wrap only as the text
//! moving WITH the caret: the 2.1.278 end-of-text wrap puts the wrap key's
//! glyph at `landing − 1` on the new row. For the soft-wrapped caret that
//! cell is the blank indent. The wrap key's move (36,117)→(37,2) matched
//! the coalesced fold: the seam swept `(37,1)`, the key's `Typed` replay laid
//! the same cell, and its one credit was spent there — THE STUB — while its
//! real glyph at (36,117) got nothing. The next key's hop (37,2)→(37,4) is two
//! cells for one press, so the press budget refused it (`no-credits` on the
//! trail ring), and the ribbon's re-wrap relay measured the word from the
//! last typed Space without the wrap key's own glyph: `(37,2)` never got a
//! cell — THE GAP, with the band starting under `l`.
//!
//! Now the seam reads the probes (`CursorGlow::soft_wrapped_caret`: the
//! landing row blank up to the landing, the origin cell blank in the last
//! probe and a glyph now) and lays the key's one cell at the origin; the
//! ribbon keeps the word the key ends (`Ribbon::settle_carry`) and, when the
//! next key's echo advances past it, relays its lit cells from row 1 to the
//! indent on the walk they had, with the reflowing key's own glyph.
//!
//! The replay is `codex_particle_replay.rs`'s: one real `Terminal`, sampled
//! as `app_render.rs`'s LOCK A samples it (content scroll, repaint blink,
//! the caret's row probe with its trust class and neighbours, the print
//! anchor, every ribbon row), `CursorGlow` ticked on an 8 ms train that holds
//! while a `?2026` bracket is open, with the hints the app stamps for each
//! key (`note_typed_expected` per printable byte; an arrow clears the typed
//! bank and stamps `note_motion`). Each owner-shape take is replayed under
//! every [`Present`] mode — the key's echo in the frame that sees its press,
//! a frame between the two (the key's `Typed` HELD, its cell from the seam's
//! sweep alone), the wrap key after a pause (judged a typed RE-ANCHOR, not
//! a coalesced fold), and the first and the third again with TYPEAHEAD
//! across the wrap (the next key pressed before the wrap key's echo, so one
//! frame holds both keys and the wrap); the controls under the first two
//! (`row2-start` has no wrap key to pause before). The census reads the
//! ribbon's own CELLS per column of the two text rows: a one-cell hole
//! between two cells is feathered over by the plan's coverage, so only the
//! cells show it.
//!
//! Measured before the fix: `row1-end` laid a stub at (37,1) from 15813 ms
//! for 497 frames and left (37,2) dark from 16097 ms (live `[1,3,4,…]`); with
//! a frame between press and echo (37,2) AND (37,3) stayed dark; after the
//! pause the re-anchor's landing sweep laid the same stub; `six` left (37,2)
//! dark under a relay of `[3,9)`. `hops` and `hops-fast` read as `row1-end`.
//! Measured before the typeahead hold (fix/trail-land review): with the
//! next key pressed before the wrap key's echo, `row1-end`, `row1-end-six`,
//! `hops` and `hops-fast` each laid the stub again under both typeahead
//! modes — that key's `Typed`, replayed at the frame's caret, laid
//! `landing − 1`, the blank indent (`Engine::replay_events` now holds it at
//! the landing, and the reflow lays it).
//!
//! Diagnostics: `targo --unverified test -p aterm-effects --test
//! claude_wrap_band census -- --ignored --nocapture` prints every take's
//! verdict; `CLAUDE_WRAP_ONLY=<take>` narrows it, `CLAUDE_WRAP_DUMP=1` prints
//! the two rows frame by frame, and `CLAUDE_WRAP_RING=<ms>:<ms>` prints every
//! admission verdict and every ribbon cell in that window.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle, ProbeTrust};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::{FLOW_TOTAL_S, RETRACT_FADE_S};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

/// The take's grid and cell: a headless instance at 40×120, 7×14 px.
const ROWS: usize = 40;
const COLS: usize = 120;
const CW: usize = 7;
const CH: usize = 14;
/// The composer's two text rows once the line has wrapped (0-based): the
/// box is bottom-anchored, so row 1 of the text is 36 and row 2 is 37.
const ROW1: u16 = 36;
const ROW2: u16 = 37;
/// The wrapped row's indent: Claude Code writes its continuation from
/// column 2 (`❯ ` on row 1, two blanks on every later row).
const INDENT: u16 = 2;
/// The frame train.
const FRAME_MS: u64 = 8;
/// A column is LIT at this planned coverage (the glass census's 20/255).
const LIT_COV: u8 = 20;
/// `app_render.rs`'s `BLINK_RECENT_MAX`.
const BLINK_RECENT: Duration = Duration::from_secs(1);
/// [`Present::PauseBeforeWrap`]'s pause: past the seam's typing rhythm
/// (`PHASER_CHAIN_GAP_MAX`, 0.75 s), which the coalesced fold needs.
const WRAP_PAUSE_MS: u64 = 1_000;
/// Row 1 must hold no band this long after the last key.
const STRANDED_AFTER_MS: u64 = 2_500;
// The row a word leaves flows into the fold and is gone by `FLOW_TOTAL_S`:
// the stranded law is only a law if its window starts after that.
const _: () = assert!(FLOW_TOTAL_S * 1000.0 < STRANDED_AFTER_MS as f32);

const FIXTURES: [(&str, &str); 7] = [
    (
        "row1-end",
        include_str!("fixtures/claude-wrap-2026-09-23-row1-end.ptylog"),
    ),
    (
        "row1-end-six",
        include_str!("fixtures/claude-wrap-2026-09-23-row1-end-six.ptylog"),
    ),
    (
        "hops",
        include_str!("fixtures/claude-wrap-2026-09-23-hops.ptylog"),
    ),
    (
        "hops-fast",
        include_str!("fixtures/claude-wrap-2026-09-23-hops-fast.ptylog"),
    ),
    (
        "eol-caret",
        include_str!("fixtures/claude-wrap-2026-09-23-eol-caret.ptylog"),
    ),
    (
        "row2-start",
        include_str!("fixtures/claude-wrap-2026-09-23-row2-start.ptylog"),
    ),
    (
        "end",
        include_str!("fixtures/claude-wrap-2026-09-23-end.ptylog"),
    ),
];

/// How the host presents a key relative to its echo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Present {
    /// The echo lands before the next frame (Claude Code echoes 1–3 ms after
    /// the key): the key's `Typed` is replayed in the frame that observes
    /// its move.
    EchoInFrame,
    /// A frame between each key's press and its echo, as a 120 Hz window
    /// renders whenever one falls between the two: the key's `Typed` is
    /// HELD on the press frame and its cell comes from the seam's sweep.
    FrameBeforeEcho,
    /// [`Present::EchoInFrame`] with the hand pausing a second before the
    /// key that soft-wraps the caret — the first key after arriving at the
    /// fold. Past the typing rhythm the move is not a coalesced fold; the
    /// seam judges it a typed RE-ANCHOR, whose landing sweep is the other
    /// path that laid the stub.
    PauseBeforeWrap,
    /// [`Present::EchoInFrame`] with TYPEAHEAD across the wrap: the key
    /// after the wrap key is pressed a millisecond after it, before its
    /// echo (a lagging composer, a fast hand), so one frame holds both
    /// keys' `Typed` and the soft wrap's `Move`. The second key has no echo
    /// yet — the reflow that carries the word down is its echo.
    TypeaheadAcrossWrap,
    /// [`Present::PauseBeforeWrap`] with the same typeahead: the wrap judged
    /// a typed re-anchor.
    TypeaheadAfterPause,
}

impl Present {
    const ALL: [Self; 5] = [
        Self::EchoInFrame,
        Self::FrameBeforeEcho,
        Self::PauseBeforeWrap,
        Self::TypeaheadAcrossWrap,
        Self::TypeaheadAfterPause,
    ];

    fn pauses(self) -> bool {
        matches!(self, Self::PauseBeforeWrap | Self::TypeaheadAfterPause)
    }

    fn typeahead(self) -> bool {
        matches!(self, Self::TypeaheadAcrossWrap | Self::TypeaheadAfterPause)
    }
}

fn fixture(name: &str) -> &'static str {
    FIXTURES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| *s)
        .expect("a fixture")
}

enum Rec {
    Out(Vec<u8>),
    In(Vec<u8>),
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

/// `(ms from the first line, event)`; `ptyrec.py` stamps nanoseconds.
fn recording(src: &str) -> Vec<(u64, Rec)> {
    let mut t0 = None;
    src.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, ' ');
            let ns: u64 = it.next().expect("stamp").parse().expect("stamp");
            let kind = it.next().expect("kind");
            let bytes = unhex(it.next().expect("bytes"));
            let t0 = *t0.get_or_insert(ns);
            let ms = (ns - t0) / 1_000_000;
            match kind {
                "O" => (ms, Rec::Out(bytes)),
                "I" => (ms, Rec::In(bytes)),
                k => panic!("bad kind {k}"),
            }
        })
        .collect()
}

/// The owner's config: rainbow kitty, tall body, default intensity, dark.
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
        duration: Duration::from_millis(260),
        length: 18,
        intensity: 1.0,
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

/// The host's `typed_class_for` and its shiftedness.
fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        c => ("~!@#$%^&*()_+{}|:\"<>?".contains(c), TypedClass::Glyph),
    }
}

fn is_key(b: &[u8]) -> bool {
    matches!(b, [c] if (0x20..0x7f).contains(c))
}

fn is_arrow(b: &[u8]) -> bool {
    b.starts_with(b"\x1b[") && matches!(b.last(), Some(b'A' | b'B' | b'C' | b'D'))
}

/// Claude Code's kitty-keyboard Ctrl+C: the take's exit.
const CTRL_C: &[u8] = b"\x1b[99;5u";

/// One frame's census of the two text rows.
#[derive(Clone, Debug)]
struct Frame {
    ms: u64,
    caret: (u16, u16, bool),
    /// Planned coverage per column of [`ROW1`] and [`ROW2`].
    cov: [Vec<u8>; 2],
    /// Columns owned by a live (not leaving) ribbon cell, per row.
    live: [Vec<u16>; 2],
    /// Columns owned by any ribbon cell (leaving included), per row.
    any: [Vec<u16>; 2],
    /// The rows' text.
    text: [String; 2],
}

impl Frame {
    fn lit(&self, r: usize) -> Vec<usize> {
        (0..COLS).filter(|&c| self.cov[r][c] >= LIT_COV).collect()
    }

    fn map(&self, r: usize) -> String {
        self.cov[r]
            .iter()
            .map(|&v| if v >= LIT_COV { '#' } else { '.' })
            .collect()
    }
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    t0: Instant,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    above: Vec<char>,
    below: Vec<char>,
    blink: u64,
    last_blink: Option<Instant>,
    scroll: Option<ContentScrollState>,
    frames: Vec<Frame>,
    /// `CLAUDE_WRAP_RING=<ms-from>:<ms-to>` prints every new admission
    /// verdict and every ribbon cell in that window.
    ring: Option<(u64, u64)>,
    ring_seen: u64,
}

impl Host {
    fn new() -> Self {
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        glow.note_pane_rows(0, ROWS);
        Self {
            term: Terminal::new(ROWS as u16, COLS as u16),
            glow,
            cfg: cfg(),
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            above: Vec::new(),
            below: Vec::new(),
            blink: 0,
            last_blink: None,
            scroll: None,
            frames: Vec::new(),
            ring: std::env::var("CLAUDE_WRAP_RING").ok().and_then(|w| {
                let (a, b) = w.split_once(':')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            }),
            ring_seen: 0,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn debug(&mut self) {
        let Some((a, b)) = self.ring else { return };
        let ms = self.ms();
        if ms < a || ms > b {
            return;
        }
        for rec in self.glow.admission_log() {
            if rec.seq > self.ring_seen {
                eprintln!("  t={ms} ring: {}", rec.line(self.now));
                self.ring_seen = rec.seq;
            }
        }
        let tally = self.glow.in_flight_tally();
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let mut cells: Vec<(u16, u16, bool)> = rib
            .cells()
            .iter()
            .map(|c| (c.row, c.col, c.leaving()))
            .collect();
        cells.sort_unstable();
        eprintln!(
            "  t={ms} credits={} cells(row,col,leaving)={cells:?}",
            tally.credits
        );
    }

    /// The host's own order: the content-scroll sync, the blink edge, the
    /// context, the caret's row probe (trust and neighbours), the print
    /// anchor, every ribbon row, then the tick.
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
        let visible = self.term.cursor_visible();
        let cur = visible.then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink {
            self.blink = epoch;
            self.last_blink = Some(self.now);
            self.glow.note_repaint_blink(self.now);
        }
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        let blink_recent = self
            .last_blink
            .is_some_and(|t| self.now.saturating_duration_since(t) <= BLINK_RECENT);
        let trust = if !alt || blink_recent {
            ProbeTrust::Full
        } else {
            ProbeTrust::ContentOnly
        };
        let r = usize::from(c.row);
        self.term.row_cols_into(r, &mut self.row_buf);
        self.glow
            .observe_row_with_trust(c.row, c.col, &self.row_buf, self.now, trust);
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
        self.glow.observe_print_anchor(self.term.print_anchor());
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &rr in &rows[..n] {
            if rr == c.row {
                continue;
            }
            self.term.row_cols_into(usize::from(rr), &mut self.row_buf);
            self.glow.observe_ribbon_row(rr, &self.row_buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
        self.record((c.row, c.col, visible));
        self.debug();
    }

    fn record(&mut self, caret: (u16, u16, bool)) {
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let mut cov = [vec![0u8; COLS], vec![0u8; COLS]];
        for sg in rib.plan_segments() {
            let r = ((sg.spine / CH as f32) - 0.5).floor();
            let c = (sg.x / CW as f32).floor();
            if r < 0.0 || c < 0.0 || (c as usize) >= COLS {
                continue;
            }
            for (i, row) in [ROW1, ROW2].into_iter().enumerate() {
                if r as usize == usize::from(row) {
                    let k = c as usize;
                    cov[i][k] = cov[i][k].max(sg.cov);
                }
            }
        }
        let cols_of = |row: u16, live_only: bool| {
            let mut v: Vec<u16> = rib
                .cells()
                .iter()
                .filter(|c| c.row == row && (!live_only || !c.leaving()))
                .map(|c| c.col)
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        let live = [cols_of(ROW1, true), cols_of(ROW2, true)];
        let any = [cols_of(ROW1, false), cols_of(ROW2, false)];
        let mut text = [String::new(), String::new()];
        for (i, row) in [ROW1, ROW2].into_iter().enumerate() {
            self.term.row_cols_into(usize::from(row), &mut self.row_buf);
            text[i] = self
                .row_buf
                .iter()
                .map(|&c| if c == '\0' { '_' } else { c })
                .collect::<String>()
                .trim_end()
                .to_string();
        }
        self.frames.push(Frame {
            ms: self.ms(),
            caret,
            cov,
            live,
            any,
            text,
        });
    }

    /// The frame train up to `ms`; the host WITHHOLDS the present while a
    /// `?2026` bracket is open (SYNC-1), so LOCK A never samples a torn frame.
    fn run_to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.now += Duration::from_millis(FRAME_MS);
            self.present();
        }
        self.now = self.now.max(t);
    }

    fn present(&mut self) {
        if !self.term.sync_open_dirty() {
            self.frame();
        }
    }

    /// The hint the app stamps for what the keyboard sent: a printable byte
    /// is a typed key (`note_typed_expected`, the host's single-cell arm), an
    /// arrow is navigation (the typed bank cleared, then `note_motion`), a
    /// Backspace erases one glyph. Terminal replies stamp nothing.
    fn hint(&mut self, bytes: &[u8]) {
        match bytes {
            [b] if (0x20..0x7f).contains(b) => {
                let ch = char::from(*b);
                let (shifted, class) = class_of(ch);
                self.glow
                    .note_typed_expected(self.now, 1, shifted && ch != ' ', class, ch);
            }
            [0x7f] => self.glow.note_backspace_erasing(self.now, Some(1)),
            b if is_arrow(b) => {
                self.glow.clear_typed(self.now);
                self.glow.note_motion(self.now);
            }
            _ => {}
        }
    }
}

/// One take, replayed: its frames and its insert (the printable keys after
/// the last arrow, up to the exit chord).
struct Take {
    name: &'static str,
    present: Present,
    frames: Vec<Frame>,
    first_key: u64,
    last_key: u64,
    keys: String,
}

impl Take {
    fn window(&self, from: u64, to: u64) -> impl Iterator<Item = &Frame> {
        self.frames
            .iter()
            .filter(move |f| f.ms >= from && f.ms <= to)
    }

    fn at(&self, ms: u64) -> &Frame {
        self.frames
            .iter()
            .find(|f| f.ms >= ms)
            .unwrap_or_else(|| self.frames.last().expect("a frame"))
    }

    /// The two rows frame by frame (`CLAUDE_WRAP_DUMP=1`), one line per
    /// change of the plan.
    fn story(&self) -> String {
        let mut s = format!(
            "{} {:?}: insert {:?} at {}..{} ms\n",
            self.name, self.present, self.keys, self.first_key, self.last_key
        );
        let mut last = (String::new(), String::new());
        for f in self.window(self.first_key.saturating_sub(300), self.last_key + 2_500) {
            let m = (f.map(0), f.map(1));
            if m == last {
                continue;
            }
            s.push_str(&format!(
                "t={:6} caret={:?}\n  r36 {}\n      {}\n  r37 {}\n      {}\n  live36={:?}\n  live37={:?}\n",
                f.ms, f.caret, m.0, f.text[0], m.1, f.text[1], f.live[0], f.live[1],
            ));
            last = m;
        }
        s
    }
}

/// The insert's WRAP KEY: the key whose echo first carries the caret from
/// row 1 to row 2 — measured on the bytes alone, a terminal with no host.
fn wrap_key_ms(rec: &[(u64, Rec)], from_ms: u64) -> Option<u64> {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let mut key = None;
    let mut row = None;
    for (ms, ev) in rec {
        match ev {
            Rec::In(b) if *ms >= from_ms && is_key(b) => key = Some(*ms),
            Rec::In(_) => {}
            Rec::Out(bytes) => {
                term.process(bytes);
                if term.sync_open_dirty() {
                    continue;
                }
                let now = term.cursor().row;
                if key.is_some() && row == Some(ROW1) && now == ROW2 {
                    return key;
                }
                row = Some(now);
            }
        }
    }
    None
}

fn replay(name: &'static str, present: Present) -> Take {
    let mut rec = recording(fixture(name));
    let exit = rec
        .iter()
        .find(|(_, e)| matches!(e, Rec::In(b) if b.as_slice() == CTRL_C))
        .map_or(u64::MAX, |(ms, _)| *ms);
    rec.retain(|(ms, _)| *ms < exit);
    let last_arrow = rec
        .iter()
        .filter(|(_, e)| matches!(e, Rec::In(b) if is_arrow(b)))
        .map(|(ms, _)| *ms)
        .next_back()
        .unwrap_or(0);
    if present.pauses() || present.typeahead() {
        let wrap = wrap_key_ms(&rec, last_arrow + 1)
            .unwrap_or_else(|| panic!("{name}: no key soft-wraps the caret to row 2"));
        if present.typeahead() {
            // The key after the wrap key, pressed a millisecond after it and
            // before its echo: everything after the wrap key is delayed two
            // (the echo lands 1–3 ms after its key, often inside the same
            // millisecond), and the next key moved in between — on its own
            // clock, as a real press is.
            let wk = rec
                .iter()
                .position(|(ms, e)| *ms == wrap && matches!(e, Rec::In(b) if is_key(b)))
                .expect("the wrap key");
            let next = rec
                .iter()
                .skip(wk + 1)
                .position(|(_, e)| matches!(e, Rec::In(b) if is_key(b)))
                .map(|i| wk + 1 + i)
                .unwrap_or_else(|| panic!("{name}: a key follows the wrap key"));
            for (ms, _) in &mut rec[wk + 1..] {
                *ms += 2;
            }
            rec[next].0 = wrap + 1;
            rec.sort_by_key(|(ms, _)| *ms);
        }
        if present.pauses() {
            for (ms, _) in &mut rec {
                if *ms >= wrap {
                    *ms += WRAP_PAUSE_MS;
                }
            }
        }
    }
    let keys: Vec<(u64, char)> = rec
        .iter()
        .filter_map(|(ms, e)| match e {
            Rec::In(b) if *ms > last_arrow && is_key(b) => Some((*ms, char::from(b[0]))),
            _ => None,
        })
        .collect();
    let first_key = keys.first().expect("a typed key").0;
    let last_key = keys.last().expect("a typed key").0;
    let mut h = Host::new();
    for (ms, ev) in rec {
        h.run_to(ms);
        match ev {
            Rec::In(b) => {
                h.hint(&b);
                if present == Present::FrameBeforeEcho {
                    h.present();
                }
            }
            Rec::Out(bytes) => {
                h.term.process(&bytes);
                h.present();
            }
        }
    }
    h.run_to(last_key + STRANDED_AFTER_MS + 500);
    let take = Take {
        name,
        present,
        frames: h.frames,
        first_key,
        last_key,
        keys: keys.iter().map(|k| k.1).collect(),
    };
    if std::env::var_os("CLAUDE_WRAP_DUMP").is_some() {
        eprintln!("{}", take.story());
    }
    take
}

/// One take's census against the laws.
#[derive(Debug, Default)]
struct Verdict {
    /// When the wrapped row first held inserted text under a caret past it.
    landed_ms: Option<u64>,
    /// Frames with a ribbon cell (or lit coverage) in the indent of the
    /// wrapped row: `(ms, columns)`.
    stub: Vec<(u64, Vec<u16>)>,
    /// Frames where a column of typed text left of the caret on the wrapped
    /// row has no live cell: `(ms, caret col, missing columns)`.
    gaps: Vec<(u64, u16, Vec<u16>)>,
    /// Frames from the landing to the last key with a live cell on the
    /// wrapped row AT or right of the caret, over the text the reflow moved
    /// down with the word: `(ms, caret col, columns)`.
    ahead: Vec<(u64, u16, Vec<u16>)>,
    /// Row-1 cells and lit columns still there long after the last key.
    stranded: Vec<(u64, Vec<u16>, Vec<usize>)>,
    /// Frames from a retract fade after the landing to the last key with a
    /// LIVE row-1 cell right of the cell after row 1's text: `(ms, columns)`.
    tail: Vec<(u64, Vec<u16>)>,
}

impl Verdict {
    fn of(t: &Take) -> Self {
        let mut v = Self::default();
        for f in t.window(t.first_key, u64::MAX) {
            let mut stub: Vec<u16> = f.any[1]
                .iter()
                .copied()
                .filter(|&c| c < INDENT)
                .chain((0..INDENT).filter(|&c| f.cov[1][usize::from(c)] >= LIT_COV))
                .collect();
            stub.sort_unstable();
            stub.dedup();
            if !stub.is_empty() {
                v.stub.push((f.ms, stub));
            }
            // The wrapped row holds the inserted text once the composer's
            // first row is row 1 and the caret stands on row 2 past the
            // indent's first column.
            if v.landed_ms.is_none()
                && f.text[0].starts_with('❯')
                && f.caret.0 == ROW2
                && f.caret.1 > INDENT + 1
            {
                v.landed_ms = Some(f.ms);
            }
            if let Some(landed) = v.landed_ms
                && f.caret.0 == ROW2
                && f.ms <= t.last_key + 100
            {
                let missing: Vec<u16> = (INDENT..f.caret.1)
                    .filter(|c| !f.live[1].contains(c))
                    .collect();
                if !missing.is_empty() {
                    v.gaps.push((f.ms, f.caret.1, missing));
                }
                // A cell past the one after row 1's text is over the blank
                // the word left: its light should have gone with it. The
                // typed Space's own cell, right after the text, stays.
                let end = u16::try_from(f.text[0].chars().count()).unwrap_or(u16::MAX);
                let past: Vec<u16> = f.live[0].iter().copied().filter(|&c| c > end).collect();
                let fade_ms = (RETRACT_FADE_S * 1000.0) as u64 + FRAME_MS;
                if !past.is_empty() && f.ms >= landed + fade_ms {
                    v.tail.push((f.ms, past));
                }
                let ahead: Vec<u16> = f.live[1]
                    .iter()
                    .copied()
                    .filter(|&c| c >= f.caret.1)
                    .collect();
                if !ahead.is_empty() {
                    v.ahead.push((f.ms, f.caret.1, ahead));
                }
            }
            if f.ms >= t.last_key + STRANDED_AFTER_MS {
                let lit = f.lit(0);
                if !f.any[0].is_empty() || !lit.is_empty() {
                    v.stranded.push((f.ms, f.any[0].clone(), lit));
                }
            }
        }
        v
    }

    fn report(&self, t: &Take) -> String {
        let mut s = format!(
            "{} {:?}: insert {}..{} ms, landed on row 2 at {:?} ms\n  stub frames {} {:?}\n  gap frames {} {:?}\n  ahead-of-caret frames {} {:?}\n  stranded frames {} {:?}\n  row-1 tail frames {} {:?}\n",
            t.name,
            t.present,
            t.first_key,
            t.last_key,
            self.landed_ms,
            self.stub.len(),
            self.stub.first(),
            self.gaps.len(),
            self.gaps.first(),
            self.ahead.len(),
            self.ahead.first(),
            self.stranded.len(),
            self.stranded.first(),
            self.tail.len(),
            self.tail.first(),
        );
        if let Some(&(ms, _, _)) = self.gaps.first() {
            let f = t.at(ms);
            s.push_str(&format!(
                "  at {ms} ms: caret {:?}\n    row2 text  {}\n    row2 plan  {}\n    row2 live  {:?}\n",
                f.caret,
                f.text[1],
                f.map(1),
                f.live[1],
            ));
        }
        s
    }

    /// The laws, as the first one broken.
    fn broken(&self) -> Option<&'static str> {
        if self.landed_ms.is_none() {
            // Precondition: the take reached the shape at all.
            Some("the insert never reached the wrapped row")
        } else if !self.stub.is_empty() {
            // (1) NOTHING ON THE INDENT: Claude Code's continuation row
            // writes from column 2; columns 0-1 hold no glyph anyone typed.
            Some("a band cell stands in the wrapped row's indent")
        } else if !self.gaps.is_empty() {
            // (2) EVERY TYPED GLYPH OF THE WORD THAT CAME DOWN IS LIT, ITS
            // FIRST LETTER INCLUDED: from the frame the word landed on row 2
            // (the relay lands on the reflowing key's own echo, that same
            // frame) to the last key, every column from the indent to the
            // caret owns a live cell.
            Some("typed text on the wrapped row has no band under it")
        } else if !self.ahead.is_empty() {
            // (3) ONLY WHAT WAS TYPED: the `[image #1] in the` the reflow
            // moved down with the word, right of the caret, is the app's
            // text and stays dark.
            Some("a band cell stands at or right of the caret")
        } else if !self.stranded.is_empty() {
            // (4) NOTHING STRANDED ON THE ROW THE WORD LEFT: long after the
            // last key (the fold flow is gone by FLOW_TOTAL_S) row 1 owns no
            // cell and no lit column.
            Some("band left stranded on row 1")
        } else if !self.tail.is_empty() {
            // (5) THE WORD'S OLD LIGHT LEAVES ROW 1 WITH IT: from a retract
            // fade after the landing to the last key, no LIVE row-1 cell
            // stands past the cell after row 1's text. (4) cannot see this:
            // by its window the fold flow has taken every row-1 cell, a
            // stranded one included. Cells, not coverage: the typed Space's
            // live cell right after the text lights the column past it.
            Some("the moved word's old light stayed on row 1's blank tail")
        } else {
            None
        }
    }
}

/// Assert the laws on one take under each of `modes`, naming every mode
/// that broke one.
fn assert_laws(name: &'static str, modes: &[Present]) {
    let failures: Vec<String> = modes
        .iter()
        .filter_map(|&present| {
            let t = replay(name, present);
            let v = Verdict::of(&t);
            v.broken()
                .map(|law| format!("{name} {present:?}: {law}\n{}", v.report(&t)))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// **THE OWNER'S SHAPE.** The caret after the last word of row 1, two
/// columns from its end; ` already` typed: the `a` is written at the end of
/// row 1 while the caret soft-wraps to the continuation row's indent, and
/// the `l` carries `al` down to column 2.
#[test]
fn a_partial_word_carried_down_by_the_next_key_keeps_its_first_letter_and_no_indent_stub() {
    assert_laws("row1-end", &Present::ALL);
}

/// The same with more room: `alread` is laid on row 1, its `d` soft-wraps
/// the caret, and the `y` carries the whole word down.
#[test]
fn a_six_letter_word_carried_down_keeps_every_letter_and_no_indent_stub() {
    assert_laws("row1-end-six", &Present::ALL);
}

/// The owner's navigation: Alt+Left to the start of row 1, Alt+Right word
/// by word back to the end of it, then the same insert.
#[test]
fn the_reflowed_word_after_word_hops_keeps_its_first_letter() {
    assert_laws("hops", &Present::ALL);
}

/// The same with the hops 140 ms apart and the insert typed at once, while
/// the hops' wake still stands on row 1.
#[test]
fn the_reflowed_word_after_fast_word_hops_keeps_its_first_letter() {
    assert_laws("hops-fast", &Present::ALL);
}

/// CONTROLS: an insert that begins at the continuation row's indent (no
/// word changes row, no caret soft-wraps), and typing at the end of the
/// text while `#1]` wraps with the box's growth.
#[test]
fn controls_an_insert_on_the_wrapped_row_and_an_end_of_text_wrap() {
    let modes = [Present::EchoInFrame, Present::FrameBeforeEcho];
    assert_laws("row2-start", &modes);
    assert_laws("end", &modes);
}

/// The caret before `[image`, which stood at the end of row 1: the first key
/// erases row 1 from the caret, writes `a[image #1]` at the continuation
/// row's indent and moves the caret down to column 3. No caret soft-wraps
/// here; the text moved with it. The seam used to hold that move as a
/// background footer park (row 1's prefix untouched, a key in flight) and
/// swallow every later move for the park's patience — the whole insert dark,
/// live on 0.91.0 too. The key's own glyph left of the landing and row 1's
/// erased tail now judge it at once (`CursorGlow::key_pushed_text_down`;
/// `wrap_code_reflow.rs` pins the footer's custody beside it).
#[test]
fn an_insert_whose_first_key_carries_the_caret_down_is_lit() {
    assert_laws("eol-caret", &Present::ALL);
}

/// The diagnostic: every take's verdict with the echo in the frame and with
/// a frame before it, printed (see the module doc for the variables that
/// narrow and deepen it).
#[test]
#[ignore = "diagnostic; run with --ignored --nocapture"]
fn census() {
    let only = std::env::var("CLAUDE_WRAP_ONLY").ok();
    for (name, _) in FIXTURES {
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        for present in [Present::EchoInFrame, Present::FrameBeforeEcho] {
            let t = replay(name, present);
            let v = Verdict::of(&t);
            eprintln!("{:?}\n{}", v.broken(), v.report(&t));
        }
    }
}
