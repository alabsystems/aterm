// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! zsh's Ctrl-R incremental search (`bck-i-search`) lays no band on the match row or under
//! zle's `_` cursor.
//!
//! zle draws the search's status row below the line and walks the caret back up onto the
//! match; every key's redisplay is one write, the caret never hidden:
//!
//! ```text
//! c  ESC[4m c ESC[24m laude --dangerously-skip-permissions ESC[1B ESC[54D c_ ESC[A ESC[15C
//! l  ESC[4m c ESC[4m l ESC[24m ESC[1B ESC[18D l_ ESC[A ESC[14C
//! a  ESC[4m c ESC[4m l ESC[4m a ESC[24m ESC[1B ESC[18D a_ ESC[A ESC[13C
//! d  BEL ESC[24m c ESC[24m l ESC[24m a ESC[1B CR failing bck-i-search: clad_ ESC[A ESC[4C
//! ```
//!
//! The caret the host samples never leaves row R, but the print anchor ends on row R+1 one
//! past `_`: the parked visible-caret shape `CursorGlow::echo_anchor_pass` lights from the
//! anchor. zle also puts the caret on the newest entry's rightmost occurrence, so a match off
//! the entry's first column walks the visible caret along row R as the keys narrow it (`ls -la`
//! on `l` is a four-cell hop): a credit-starved typed re-anchor onto history text nobody typed.
//!
//! Two gates, one witness: a key's light is laid only on a cell holding a live press's own
//! glyph (`note_typed_expected`). On a visible caret parked off the anchor's row, the anchored
//! lane lays an echo only where the run ends on such a glyph; the visible lane's typed
//! re-anchor lays its landing only where the caret row's probe holds one. zle's echoes end on
//! `_` and its hops land on the match's own text, so each is refused `program-row` and its
//! press forgotten. The run's last glyph comes from the host (`Terminal::print_anchor_glyph`
//! through `observe_print_anchor_glyph`, as `app_render.rs` does), else from the caret row's
//! probe and its two flanking rows; under a wrapped match the status row is two rows below the
//! caret, where only the host's glyph sees it. A glyph-less press (a wide glyph, an IME
//! commit) or an empty pool is unknown and keeps each lane as it was. The single-pane window
//! hands over a DECTCEM-hidden caret too, marked hidden (`observe_caret_drawn`), so a
//! hidden-caret TUI keeps its anchored lane.
//!
//! The initial search display establishes that the drawn caret is parked away from its echo
//! row with no hand typing there. A later partial redisplay cannot claim that parked row
//! merely by printing a key's own glyph: it stays dark, however late the continuation arrives.
//! Established typing stays immediate; classified navigation, shell Return or a generic input
//! boundary lets the next echo establish a new typing row. A row the hand's own input landed on
//! (a key's echo, a paste's) is its own until the next boundary, wherever the hand types
//! meanwhile.
//!
//! Each take in `fixtures/` (`<µs> O|I <hex>`, one line per read) is replayed through a real
//! [`Terminal`], sampled in `app_render.rs`'s order (scroll sync, the ribbon's witness rows,
//! the row probe and its neighbours, the print anchor and its glyph, whether the caret is
//! drawn, the tick), with the hints `app_input.rs` stamps per key, at 120 Hz and on the host's
//! wake schedule. The census reads the ink on glass (the ribbon's under-ink quads and the
//! overlay stream folded to cells) beside the live ribbon cells, the glyph under each, and the
//! admission ring.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{
    AdmissionRecord, CursorGlow, Geom, GlowConfig, GlowStyle, InsertWidth, ProbeTrust,
};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// The headless instance's cell, 7×14 px.
const CW: usize = 7;
const CH: usize = 14;
/// A 120 Hz window's frame train.
const FRAME_US: u64 = 8_333;
/// A pixel is ink at this level (premultiplied channel or source-over opacity,
/// 0..255), below the glass census's 20/255 so a faint stub counts.
const INK: u8 = 8;
/// Idle after the last key by which every life has ended: the longest swoosh
/// is a 1.5 s rest plus 0.79 s (`review_real_bytes.rs`'s `IDLE_CEILING_MS`).
const IDLE_CEILING_MS: u64 = 2_500;

/// One recorded take and the grid it was recorded at.
#[derive(Clone, Copy)]
struct Take {
    name: &'static str,
    src: &'static str,
    rows: usize,
    cols: usize,
    /// Command lines pre-fed so a python-pty take's prompt sits mid-grid (the headless takes
    /// carry their own history).
    preroll: usize,
}

const fn headless(name: &'static str, src: &'static str) -> Take {
    Take {
        name,
        src,
        rows: 24,
        cols: 100,
        preroll: 0,
    }
}

const G150: Take = headless(
    "g150",
    include_str!("fixtures/zsh-isearch-2026-09-23.ptylog"),
);
const G60: Take = headless(
    "g60",
    include_str!("fixtures/zsh-isearch-2026-09-23-g60.ptylog"),
);
const BURST: Take = headless(
    "burst",
    include_str!("fixtures/zsh-isearch-2026-09-23-burst.ptylog"),
);
const CAP120: Take = headless(
    "cap120",
    include_str!("fixtures/zsh-isearch-2026-09-23-cap120.ptylog"),
);
const PLAIN: Take = headless(
    "plain",
    include_str!("fixtures/zsh-isearch-2026-09-23-plain.ptylog"),
);
const CONTINUE: Take = Take {
    name: "continue",
    src: include_str!("fixtures/zsh-isearch-2026-09-23-continue.ptylog"),
    rows: 30,
    cols: 100,
    preroll: 5,
};
const WRAPPED: Take = Take {
    name: "wrapped",
    src: include_str!("fixtures/zsh-isearch-2026-09-23-wrapped.ptylog"),
    rows: 30,
    cols: 100,
    preroll: 0,
};

/// The Ctrl-R `clad` gesture at four cadences.
const OWNER_TAKES: [Take; 4] = [G150, G60, BURST, CAP120];

const HOP: Take = Take {
    name: "hop",
    src: include_str!("fixtures/zsh-isearch-2026-09-23-hop.ptylog"),
    rows: 30,
    cols: 100,
    preroll: 5,
};
const HOP2: Take = Take {
    name: "hop2",
    src: include_str!("fixtures/zsh-isearch-2026-09-23-hop2.ptylog"),
    rows: 30,
    cols: 100,
    preroll: 5,
};
const HOP2_BOTTOM: Take = Take {
    name: "hop2-bottom",
    src: include_str!("fixtures/zsh-isearch-2026-09-23-hop2-bottom.ptylog"),
    rows: 30,
    cols: 100,
    preroll: 0,
};

const BACKHOP: Take = Take {
    name: "backhop",
    src: include_str!("fixtures/zsh-isearch-2026-09-24-backhop.ptylog"),
    rows: 30,
    cols: 100,
    preroll: 5,
};

/// Searches whose match is not at the entry's first column, so zle walks the
/// caret along the match row: forward, and (the last) back.
const HOP_TAKES: [Take; 4] = [HOP, HOP2, HOP2_BOTTOM, BACKHOP];

const MULTILINE: Take = Take {
    name: "multiline",
    src: include_str!("fixtures/zsh-isearch-2026-09-24-multiline.ptylog"),
    rows: 30,
    cols: 100,
    preroll: 5,
};
const MULTILINE_SLOW: Take = Take {
    name: "multiline-slow",
    src: include_str!("fixtures/zsh-isearch-2026-09-24-multiline-slow.ptylog"),
    rows: 30,
    cols: 100,
    preroll: 5,
};

/// bash 3.2's reverse-i-search: the query is echoed inside the prompt row's
/// `(reverse-i-search)`…`': ` and the caret walks the match on that row.
const BASH: Take = Take {
    name: "bash",
    src: include_str!("fixtures/bash-isearch-2026-09-24.ptylog"),
    rows: 30,
    cols: 100,
    preroll: 5,
};

/// A composer take from another program (the recordings other suites replay:
/// `review_real_bytes.rs`, `codex_particle_replay.rs`), its clock and grid.
struct Composer {
    name: &'static str,
    src: &'static str,
    clock: Clock,
    rows: usize,
    cols: usize,
}

const fn claude(name: &'static str, src: &'static str) -> Composer {
    Composer {
        name,
        src,
        clock: Clock::Ns,
        rows: 40,
        cols: 120,
    }
}

const fn codex_wrapped(name: &'static str, src: &'static str) -> Composer {
    Composer {
        name,
        src,
        clock: Clock::Ms,
        rows: 56,
        cols: 137,
    }
}

/// Claude Code's four composer takes, Codex's four, and Claude Code's re-wrap
/// takes (the seven `claude_wrap_band.rs` replays and `wrap_code_reflow.rs`'s
/// recording): every shape whose re-anchor lays a cell other than
/// `landing − 1`.
const COMPOSERS: [Composer; 16] = [
    claude(
        "claude-insert",
        include_str!("fixtures/claude-composer-2026-09-21.ptylog"),
    ),
    claude(
        "claude-end",
        include_str!("fixtures/claude-composer-2026-09-21-end.ptylog"),
    ),
    claude(
        "claude-scrub3",
        include_str!("fixtures/claude-composer-2026-09-21-scrub3.ptylog"),
    ),
    claude(
        "claude-popup",
        include_str!("fixtures/claude-composer-2026-09-21-popup.ptylog"),
    ),
    Composer {
        name: "codex-particles",
        src: include_str!("fixtures/codex-particles-2026-09-16.ptylog"),
        clock: Clock::Ms,
        rows: 32,
        cols: 100,
    },
    codex_wrapped(
        "codex-wrapped",
        include_str!("fixtures/codex-wrapped-composer-2026-09-22.ptylog"),
    ),
    codex_wrapped(
        "codex-stalled",
        include_str!("fixtures/codex-wrapped-composer-stalled-2026-09-22.ptylog"),
    ),
    codex_wrapped(
        "codex-first-frame",
        include_str!("fixtures/codex-wrapped-composer-first-frame-2026-09-22.ptylog"),
    ),
    claude(
        "claude-wrap-row1-end",
        include_str!("fixtures/claude-wrap-2026-09-23-row1-end.ptylog"),
    ),
    claude(
        "claude-wrap-row1-end-six",
        include_str!("fixtures/claude-wrap-2026-09-23-row1-end-six.ptylog"),
    ),
    claude(
        "claude-wrap-hops",
        include_str!("fixtures/claude-wrap-2026-09-23-hops.ptylog"),
    ),
    claude(
        "claude-wrap-hops-fast",
        include_str!("fixtures/claude-wrap-2026-09-23-hops-fast.ptylog"),
    ),
    claude(
        "claude-wrap-eol-caret",
        include_str!("fixtures/claude-wrap-2026-09-23-eol-caret.ptylog"),
    ),
    claude(
        "claude-wrap-row2-start",
        include_str!("fixtures/claude-wrap-2026-09-23-row2-start.ptylog"),
    ),
    claude(
        "claude-wrap-end",
        include_str!("fixtures/claude-wrap-2026-09-23-end.ptylog"),
    ),
    Composer {
        name: "claude-wrap-code-reflow",
        src: include_str!("fixtures/wrap_code-cc-chip-reflow-2026-09-23.ptylog"),
        clock: Clock::Us,
        rows: 20,
        cols: 64,
    },
];

/// The prompt `owner0@hostmachine0000 aterm % `: its width is the column where
/// the line and the match start in the python-pty takes.
const PROMPT_COLS: u16 = 31;

#[derive(Clone)]
enum Ev {
    Out(Vec<u8>),
    In(Vec<u8>),
    /// A controller `turn` fence (Codex's takes): the app clears the typed
    /// licence.
    Clear,
}

/// The unit a fixture's stamps are in.
#[derive(Clone, Copy)]
enum Clock {
    /// µs from the recorder's start (the zsh takes).
    Us,
    /// ms from the recorder's start (Codex's takes).
    Ms,
    /// Absolute ns (Claude Code's takes).
    Ns,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn events(fixture: &str) -> Vec<(u64, Ev)> {
    events_in(fixture, Clock::Us)
}

/// `(µs, event)`; an absolute clock is taken from its first stamp.
fn events_in(fixture: &str, clock: Clock) -> Vec<(u64, Ev)> {
    let mut t0 = None;
    fixture
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, ' ');
            let stamp: u64 = it.next().expect("stamp").parse().expect("stamp");
            let kind = it.next().expect("kind");
            let bytes = unhex(it.next().unwrap_or(""));
            let us = match clock {
                Clock::Us => stamp,
                Clock::Ms => stamp * 1_000,
                Clock::Ns => (stamp - *t0.get_or_insert(stamp)) / 1_000,
            };
            (
                us,
                match kind {
                    "O" => Ev::Out(bytes),
                    "I" => Ev::In(bytes),
                    "C" => Ev::Clear,
                    k => panic!("bad kind {k}"),
                },
            )
        })
        .collect()
}

/// Cut one PTY read into the pieces a torn read can stop between: every escape
/// sequence (CSI to its final byte, OSC to BEL/ST, two-byte ESC), every C0
/// control, every printable run.
fn pieces(b: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        match b[i] {
            0x1b if i + 1 < b.len() && b[i + 1] == b'[' => {
                i += 2;
                while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
                    i += 1;
                }
                i = (i + 1).min(b.len());
            }
            0x1b if i + 1 < b.len() && b[i + 1] == b']' => {
                i += 2;
                while i < b.len() && b[i] != 0x07 && !(b[i] == 0x1b && b.get(i + 1) == Some(&b'\\'))
                {
                    i += 1;
                }
                i = if i < b.len() && b[i] == 0x1b {
                    (i + 2).min(b.len())
                } else {
                    (i + 1).min(b.len())
                };
            }
            0x1b => i = (i + 2).min(b.len()),
            c if c < 0x20 || c == 0x7f => i += 1,
            _ => {
                while i < b.len() && b[i] >= 0x20 && b[i] != 0x7f {
                    i += 1;
                }
            }
        }
        out.push(&b[start..i]);
    }
    out
}

/// The shipped default: Rainbow Kitty, tall body, intensity 1.0, dark.
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
    }
}

/// How the reads are presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tear {
    /// Every read processed whole, a present after it.
    Whole,
    /// A present after every piece of every read from the Ctrl-R on.
    Every,
    /// Exactly one read (the `n`th output read from the Ctrl-R on) torn once,
    /// after its first `k` pieces.
    One { n: usize, k: usize },
}

/// One replay's presentation.
#[derive(Clone, Copy, Debug)]
struct Present {
    tear: Tear,
    /// Present only when the host would (on PTY output, a key, and the wake the
    /// engine asks for: `needs_frame_cadence`, else `next_change_deadline`)
    /// instead of every 120 Hz frame.
    host_wake: bool,
    /// Hand the engine the print anchor's glyph with the anchor, as
    /// `app_render.rs` does; `false` feeds the anchor only, and the engine reads
    /// the caret row's probes.
    anchor_glyph: bool,
    /// Hand over the caret whether or not DECTCEM draws it, as the single-pane
    /// window does; `false` is the composed, focus and headless paths (drawn
    /// carets only). zle never hides it.
    window_caret: bool,
}

/// What the app does in its default single-pane window.
const APP: Present = Present {
    tear: Tear::Whole,
    host_wake: false,
    anchor_glyph: true,
    window_caret: true,
};

/// Every whole-read presentation.
const WHOLE: [Present; 4] = [
    APP,
    Present {
        host_wake: true,
        ..APP
    },
    Present {
        anchor_glyph: false,
        ..APP
    },
    Present {
        host_wake: true,
        anchor_glyph: false,
        ..APP
    },
];

/// One inked cell's life across the replay.
#[derive(Clone, Copy, Debug)]
struct Life {
    first_us: u64,
    last_us: u64,
    /// The most ink pixels the cell carried on one frame.
    peak_px: u32,
}

/// One admission-ring row.
#[derive(Clone, Copy, Debug)]
struct Verdict {
    us: u64,
    reason: &'static str,
    licence: &'static str,
    origin: (u16, u16),
    target: (u16, u16),
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    present: Present,
    t0: Instant,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    above: Vec<char>,
    below: Vec<char>,
    blink: u64,
    scroll: Option<ContentScrollState>,
    /// From when the census runs (the Ctrl-R, or a control's first key), and the caret row
    /// then: a search's match row R.
    census_from: Option<(u64, u16)>,
    /// Printable keys `(µs, glyph)`.
    keys: Vec<(u64, char)>,
    /// Under-ink (the band's body and rail) per cell since the census began.
    lives: BTreeMap<(u16, u16), Life>,
    /// The overlay stream (`out`: the hot edge, sparks) per cell.
    over_lives: BTreeMap<(u16, u16), Life>,
    /// Live (not leaving) ribbon cells and the glyph under each, first/last
    /// µs seen.
    cells: BTreeMap<(u16, u16, char), (u64, u64)>,
    /// When the search ended (Ctrl-G, Ctrl-C, or an arrow leaving it).
    search_end_us: Option<u64>,
    /// Every row the i-search status row stood on.
    status_rows: BTreeSet<u16>,
    /// Live ribbon cells on the status row while it was the status row, with
    /// the glyph under each.
    status_cells: BTreeSet<(u16, u16, char)>,
    /// Band ink on the status row at or right of zle's `_`: `(µs, col, ink
    /// pixels)`.
    beyond: Vec<(u64, u16, u32)>,
    /// Live ribbon cells on the status row at or right of zle's `_`, with
    /// the glyph under each.
    beyond_cells: Vec<(u16, u16, char)>,
    ring: Vec<Verdict>,
    ring_seen: u64,
    /// `(visible caret, print anchor)` on every frame a new print ended on a row other than
    /// the visible caret's.
    parked_prints: Vec<((u16, u16), (u16, u16))>,
    /// Of those, the frames whose caret DECTCEM hid — handed over only by
    /// the single-pane window ([`Present::window_caret`]).
    hidden_parked_prints: usize,
    /// Every visible caret move along the match row while the search ran: zle walking the
    /// caret to the match's new place.
    match_hops: Vec<((u16, u16), (u16, u16))>,
    /// The visible caret the last frame saw.
    last_cur: Option<(u16, u16)>,
    anchor_seq: Option<u64>,
    /// The wake the last frame asked for.
    wake: Option<Instant>,
    /// The cells the last presented frame inked — on glass until the next.
    on_glass: Vec<(u16, u16)>,
    /// Frames after which the engine asked for no wake with its ink on glass.
    idle_with_ink: Vec<(u64, usize)>,
    ink: Vec<u8>,
}

impl Host {
    fn new(rows: usize, cols: usize, present: Present) -> Self {
        let g = Geom {
            cw: CW,
            ch: CH,
            rows,
            cols,
            origin_x: 0,
            origin_y: 0,
            win_w: (cols * CW) as u16,
            win_h: (rows * CH) as u16,
            head: 0,
        };
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, cols);
        glow.note_pane_rows(0, rows);
        Self {
            term: Terminal::new(rows as u16, cols as u16),
            glow,
            cfg: cfg(),
            g,
            present,
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            above: Vec::new(),
            below: Vec::new(),
            blink: 0,
            scroll: None,
            census_from: None,
            search_end_us: None,
            status_rows: BTreeSet::new(),
            status_cells: BTreeSet::new(),
            beyond_cells: Vec::new(),
            keys: Vec::new(),
            lives: BTreeMap::new(),
            over_lives: BTreeMap::new(),
            cells: BTreeMap::new(),
            beyond: Vec::new(),
            ring: Vec::new(),
            ring_seen: 0,
            parked_prints: Vec::new(),
            hidden_parked_prints: 0,
            match_hops: Vec::new(),
            last_cur: None,
            anchor_seq: None,
            wake: None,
            on_glass: Vec::new(),
            idle_with_ink: Vec::new(),
            ink: vec![0; cols * CW * rows * CH],
        }
    }

    fn us(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_micros() as u64
    }

    fn glyph(&self, row: u16, col: u16) -> char {
        let mut buf = Vec::new();
        self.term.row_cols_into(usize::from(row), &mut buf);
        buf.get(usize::from(col)).copied().unwrap_or(' ')
    }

    fn text(&self, row: u16) -> String {
        let mut buf = Vec::new();
        self.term.row_cols_into(usize::from(row), &mut buf);
        buf.iter()
            .map(|&c| if c == '\0' { ' ' } else { c })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// The host's order: `sync_cursor_effect_scroll`, the frame hold's witness rows (the caret's
    /// row, then every row the ribbon names), the row probe with neighbours, the print anchor
    /// with its glyph, the tick.
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
        let drawn = self.term.cursor_visible();
        let cur = (self.present.window_caret || drawn).then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink {
            self.blink = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        let mut other = Vec::new();
        for &r in &rows[..n] {
            if r == c.row {
                continue;
            }
            self.term.row_cols_into(usize::from(r), &mut other);
            self.glow.observe_ribbon_row(r, &other);
        }
        self.glow
            .observe_row_with_trust(c.row, c.col, &self.row_buf, self.now, ProbeTrust::Full);
        let above = c.row > 0;
        let below = usize::from(c.row) + 1 < self.g.rows;
        if above {
            self.term
                .row_cols_into(usize::from(c.row) - 1, &mut self.above);
        }
        if below {
            self.term
                .row_cols_into(usize::from(c.row) + 1, &mut self.below);
        }
        self.glow.observe_neighbor_rows(
            above.then_some(self.above.as_slice()),
            below.then_some(self.below.as_slice()),
        );
        let anchor = self.term.print_anchor();
        if let (Some((ar, ac, seq)), Some((cr, cc))) = (anchor, cur)
            && self.anchor_seq != Some(seq)
            && ar != cr
        {
            self.parked_prints.push(((cr, cc), (ar, ac)));
            if !drawn {
                self.hidden_parked_prints += 1;
            }
        }
        self.anchor_seq = anchor.map(|(_, _, seq)| seq);
        if let (Some((_, r)), None, Some(from), Some(to)) =
            (self.census_from, self.search_end_us, self.last_cur, cur)
            && from != to
            && from.0 == r
            && to.0 == r
        {
            self.match_hops.push((from, to));
        }
        self.last_cur = cur;
        self.glow.observe_caret_drawn(drawn);
        // The engines get the anchor through the anchor law, as the shipping
        // host hands it over; the census above reads the terminal's own.
        let (law_anchor, law_glyph) = aterm_effects::host::motion_print_anchor(&self.term);
        if self.present.anchor_glyph {
            self.glow.observe_print_anchor_glyph(law_anchor, law_glyph);
        } else {
            self.glow.observe_print_anchor(law_anchor);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        self.record_ring();
        if self.census_from.is_some() {
            self.census();
        }
        let interval = Duration::from_micros(FRAME_US);
        self.wake = if self.glow.needs_frame_cadence() {
            Some(self.now + interval)
        } else {
            self.glow.next_change_deadline(self.now, interval)
        };
        if self.wake.is_none() && self.census_from.is_some() && !self.glow.under_quads().is_empty()
        {
            let at = self.us();
            self.idle_with_ink.push((at, self.on_glass.len()));
        }
    }

    fn record_ring(&mut self) {
        let floor = self.ring_seen;
        let us = self.us();
        for a in self.glow.admission_log().filter(|a| a.seq > floor) {
            self.ring_seen = self.ring_seen.max(a.seq);
            self.ring.push(Verdict {
                us,
                reason: a.reason,
                licence: a.licence,
                origin: a.origin,
                target: a.target,
            });
        }
    }

    /// This frame's ink, folded to cells, and the live ribbon cells.
    fn census(&mut self) {
        let us = self.us();
        self.close_glass();
        let frame_cells = fold(self.glow.under_quads(), self.g, &mut self.ink);
        self.on_glass = frame_cells.keys().copied().collect();
        let fake = self.fake_cursor();
        if let Some((status, fake)) = fake {
            for (&(r, c), &px) in &frame_cells {
                if r == status && c >= fake && px >= 20 {
                    self.beyond.push((us, c, px));
                }
            }
        }
        record(&mut self.lives, &frame_cells, us);
        let over = fold(&self.out, self.g, &mut self.ink);
        record(&mut self.over_lives, &over, us);
        let live: Vec<(u16, u16)> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .iter()
            .filter(|x| !x.leaving())
            .map(|x| (x.row, x.col))
            .collect();
        for (r, c) in live {
            let g = self.glyph(r, c);
            if let Some((status, fake)) = fake
                && r == status
            {
                self.status_cells.insert((r, c, g));
                if c >= fake {
                    self.beyond_cells.push((r, c, g));
                }
            }
            self.cells
                .entry((r, c, g))
                .and_modify(|e| e.1 = us)
                .or_insert((us, us));
        }
    }

    /// Where zle's fake cursor `_` stands: the status row (the first row below
    /// the match row holding `search:`; two down under a wrapped match) and its
    /// column.
    fn fake_cursor(&mut self) -> Option<(u16, u16)> {
        let (_, r) = self.census_from?;
        if self.search_end_us.is_some() {
            return None;
        }
        let rows = u16::try_from(self.g.rows).expect("rows fit");
        let status = (r + 1..(r + 4).min(rows)).find(|&s| self.text(s).contains("search:"))?;
        self.status_rows.insert(status);
        let text: Vec<char> = self.text(status).chars().collect();
        let fake = text.iter().rposition(|&c| c == '_')?;
        Some((status, u16::try_from(fake).expect("a column")))
    }

    /// The last presented frame's ink stayed on glass until now.
    fn close_glass(&mut self) {
        let us = self.us();
        for k in &self.on_glass {
            if let Some(l) = self.lives.get_mut(k) {
                l.last_us = l.last_us.max(us);
            }
        }
    }

    fn run_to(&mut self, us: u64) {
        let t = self.t0 + Duration::from_micros(us);
        let frame = Duration::from_micros(FRAME_US);
        if self.present.host_wake {
            while let Some(w) = self.wake.filter(|&w| w <= t) {
                self.now = self.now.max(w);
                if self.term.sync_open_dirty() {
                    self.wake = Some(self.now + frame);
                } else {
                    self.frame();
                }
            }
            self.now = self.now.max(t);
            return;
        }
        while self.now + frame <= t {
            self.now += frame;
            if !self.term.sync_open_dirty() {
                self.frame();
            }
        }
        self.now = self.now.max(t);
    }

    fn idle_ms(&mut self, ms: u64) {
        let to = self.us() + ms * 1000;
        self.run_to(to);
    }

    /// Program output: processed, then presented.
    fn feed(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
        if !self.term.sync_open_dirty() {
            self.frame();
        }
    }

    /// The hint `app_input.rs` stamps for what the keyboard sent: a printable
    /// key is a typed glyph (`note_typed_expected`); Enter is the Return
    /// licence; an arrow, ⌥←/⌥→, Home or End is navigation
    /// (`clear_typed_keeping_hand`, then `note_motion`); Backspace (same disarm)
    /// erases one priced glyph; Ctrl-R, Ctrl-G and Ctrl-C are the generic
    /// disarm (`clear_typed`); multi-byte `aterm ctl send` text stamps nothing.
    fn hint(&mut self, bytes: &[u8]) {
        match bytes {
            [b] if (0x20..0x7f).contains(b) => {
                let ch = char::from(*b);
                self.keys.push((self.us(), ch));
                let class = match ch {
                    ' ' => TypedClass::Space,
                    '!' => TypedClass::Bang,
                    c if c.is_uppercase() => TypedClass::Capital,
                    _ => TypedClass::Glyph,
                };
                self.glow.supersede_typed_press();
                self.glow
                    .note_typed_expected(self.now, 1, ch.is_uppercase(), class, ch);
            }
            b"\r" => {
                self.glow.supersede_typed_press();
                self.glow.note_return(self.now);
            }
            [0x7f] => {
                self.glow.clear_typed_keeping_hand(self.now);
                self.glow.note_backspace_erasing(self.now, Some(1));
            }
            b"\x1b[A" | b"\x1b[B" | b"\x1b[C" | b"\x1b[D" | b"\x1b[1;3C" | b"\x1b[1;3D"
            | b"\x1b[H" | b"\x1b[F" => {
                self.end_search();
                self.glow.clear_typed_keeping_hand(self.now);
                self.glow.note_motion(self.now);
            }
            [0x12] => {
                self.census_from = Some((self.us(), self.term.cursor().row));
                self.glow.clear_typed(self.now);
            }
            [0x07 | 0x03] => {
                self.end_search();
                self.glow.clear_typed(self.now);
            }
            _ => {}
        }
    }

    fn end_search(&mut self) {
        if self.census_from.is_some() && self.search_end_us.is_none() {
            self.search_end_us = Some(self.us());
        }
    }

    fn match_row(&self) -> u16 {
        self.census_from.expect("the take presses Ctrl-R").1
    }

    fn last_key_us(&self) -> u64 {
        self.keys.last().expect("a typed key").0
    }

    fn typed(&self) -> Vec<char> {
        self.keys.iter().map(|k| k.1).collect()
    }

    /// Live ribbon cells on `row`, `(col, glyph under it)`.
    fn cells_on(&self, row: u16) -> Vec<(u16, char)> {
        self.cells
            .keys()
            .filter(|(r, _, _)| *r == row)
            .map(|(_, c, g)| (*c, *g))
            .collect()
    }

    /// Cells whose ink is still on glass `IDLE_CEILING_MS` after the last key.
    fn outlived(&self) -> Vec<((u16, u16), u64)> {
        let cutoff = self.last_key_us() + IDLE_CEILING_MS * 1000;
        self.lives
            .iter()
            .filter(|(_, l)| l.last_us > cutoff)
            .map(|(k, l)| (*k, l.last_us.saturating_sub(self.last_key_us())))
            .collect()
    }

    /// The census, readable, relative to the Ctrl-R.
    fn report(&self, what: &str) -> String {
        let t0 = self.census_from.map_or(0, |(t, _)| t);
        let ms = |us: u64| us.saturating_sub(t0) as f64 / 1000.0;
        let mut s = format!("{what}: census row {}", self.match_row());
        for ((r, c), l) in &self.lives {
            s.push_str(&format!(
                "\n  under ({r},{c}) {:.1}..{:.1} ms, {} px",
                ms(l.first_us),
                ms(l.last_us),
                l.peak_px
            ));
        }
        for ((r, c), l) in &self.over_lives {
            s.push_str(&format!(
                "\n  over  ({r},{c}) {:.1}..{:.1} ms, {} px",
                ms(l.first_us),
                ms(l.last_us),
                l.peak_px
            ));
        }
        for ((r, c, g), (a, b)) in &self.cells {
            s.push_str(&format!(
                "\n  cell  ({r},{c}) under {g:?} {:.1}..{:.1} ms",
                ms(*a),
                ms(*b)
            ));
        }
        for v in &self.ring {
            s.push_str(&format!(
                "\n  ring {:.1} ms {} {} {:?}->{:?}",
                ms(v.us),
                v.reason,
                v.licence,
                v.origin,
                v.target
            ));
        }
        s
    }
}

/// Fold quads into per-cell ink pixels. `scratch` is a per-pixel max buffer,
/// cleared before returning; only touched pixels are visited.
fn fold(quads: &[GlowQuad], g: Geom, scratch: &mut [u8]) -> BTreeMap<(u16, u16), u32> {
    let w = g.cols * CW;
    let h = g.rows * CH;
    let level = |c: u32, a: u8| {
        let ch = ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff) as u8;
        ch.max(a)
    };
    let mut touched: Vec<usize> = Vec::new();
    for q in quads {
        let lv = level(q.color, q.alpha).max(level(q.color2, q.alpha2));
        if lv < INK {
            continue;
        }
        for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
            for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                let i = y * w + x;
                if scratch[i] == 0 {
                    touched.push(i);
                }
                scratch[i] = scratch[i].max(lv);
            }
        }
    }
    let mut cells: BTreeMap<(u16, u16), u32> = BTreeMap::new();
    for &i in &touched {
        let (y, x) = (i / w, i % w);
        *cells.entry(((y / CH) as u16, (x / CW) as u16)).or_insert(0) += 1;
        scratch[i] = 0;
    }
    cells
}

fn record(lives: &mut BTreeMap<(u16, u16), Life>, frame: &BTreeMap<(u16, u16), u32>, us: u64) {
    for (&k, &px) in frame {
        let life = lives.entry(k).or_insert(Life {
            first_us: us,
            last_us: us,
            peak_px: 0,
        });
        life.last_us = us;
        life.peak_px = life.peak_px.max(px);
    }
}

fn replay(take: Take, present: Present) -> Host {
    let mut h = Host::new(take.rows, take.cols, present);
    // Scrollback above the prompt, so the prompt row is not the grid's top.
    for i in 0..take.preroll {
        h.term
            .process(format!("owner0@hostmachine0000 aterm % echo {i}\r\n{i}\r\n").as_bytes());
    }
    let mut nth_out = 0usize;
    // The Ctrl-R's own redisplay places the match row: a prompt on the
    // grid's last row scrolls up one to make room for the status row.
    let mut rebase = false;
    for (us, ev) in events(take.src) {
        h.run_to(us);
        match ev {
            Ev::In(b) => {
                rebase |= b.as_slice() == [0x12];
                h.hint(&b);
                if present.host_wake {
                    // A key requests a redraw (`app_input.rs`).
                    h.frame();
                }
            }
            Ev::Clear => h.glow.clear_typed(h.now),
            Ev::Out(b) => {
                let torn_here = h.census_from.is_some();
                let cut: Vec<&[u8]> = match present.tear {
                    Tear::Every if torn_here => pieces(&b),
                    Tear::One { n, k } if torn_here && nth_out == n => {
                        let p = pieces(&b);
                        let k = k.min(p.len());
                        let first: usize = p[..k].iter().map(|s| s.len()).sum();
                        vec![&b[..first], &b[first..]]
                    }
                    _ => vec![&b[..]],
                };
                if torn_here {
                    nth_out += 1;
                }
                let last = cut.len() - 1;
                for (i, piece) in cut.into_iter().enumerate() {
                    if piece.is_empty() {
                        continue;
                    }
                    h.feed(piece);
                    if i < last {
                        // A torn read's present is its own frame, a beat later.
                        h.now += Duration::from_micros(50);
                    }
                }
                if std::mem::take(&mut rebase)
                    && let Some((_, row)) = h.census_from.as_mut()
                {
                    *row = h.term.cursor().row;
                }
            }
        }
    }
    // Rest well past every life.
    h.idle_ms(3_000);
    h.close_glass();
    h
}

/// A composer take replayed up to its exit chord (Claude Code's
/// `\x1b[99;5u`, Codex's Ctrl-C), then rested past every life.
fn replay_composer(take: &Composer, present: Present) -> Host {
    let mut h = Host::new(take.rows, take.cols, present);
    for (us, ev) in events_in(take.src, take.clock) {
        h.run_to(us);
        match ev {
            Ev::In(b) if b == b"\x1b[99;5u" || b == b"\x03" => break,
            Ev::In(b) => h.hint(&b),
            Ev::Out(b) => h.feed(&b),
            Ev::Clear => h.glow.clear_typed(h.now),
        }
    }
    h.idle_ms(3_000);
    h
}

/// Every cell the ribbon put on the status row under a glyph the search did
/// not type (zle's `_` included), and every cell at or right of the `_`.
fn untyped_status_cells(h: &Host) -> Vec<(u16, u16, char)> {
    assert!(!h.status_rows.is_empty(), "the search drew its status row");
    let typed = h.typed();
    let mut bad: Vec<(u16, u16, char)> = h
        .status_cells
        .iter()
        .copied()
        .filter(|&(_, _, g)| !typed.contains(&g) || g == '_')
        .chain(h.beyond_cells.iter().copied())
        .collect();
    bad.sort_unstable();
    bad.dedup();
    bad
}

/// Every read presented whole (one zle redisplay is one write is one read), at 120 Hz and on the
/// host's wake, the anchor's glyph handed over or not. From the Ctrl-R on nothing is typed on
/// row R (zle only re-underlines the match), so no cell of it carries ribbon ink: neither
/// body/rail nor hot edge. On the status row: no ribbon cell under anything but the search's
/// typed glyphs, none under or right of the `_`, and no band ink at or right of the `_`.
#[test]
fn whole_reads_lay_no_band_on_the_match_row_or_under_zle_s_fake_cursor() {
    let mut bad = String::new();
    for take in OWNER_TAKES {
        for present in WHOLE {
            let h = replay(take, present);
            let r = h.match_row();
            let body: Vec<_> = h.lives.keys().filter(|(row, _)| *row == r).collect();
            let over: Vec<_> = h.over_lives.keys().filter(|(row, _)| *row == r).collect();
            if !body.is_empty() || !over.is_empty() {
                bad.push_str(&format!(
                    "\n{}/{present:?}: ribbon ink on the match row R: body {body:?} hot edge {over:?}\n{}",
                    take.name,
                    h.report(take.name)
                ));
            }
            let cells = untyped_status_cells(&h);
            if !cells.is_empty() || !h.beyond.is_empty() {
                let cols: BTreeSet<u16> = h.beyond.iter().map(|b| b.1).collect();
                bad.push_str(&format!(
                    "\n{}/{present:?}: band on the minibuffer: cells {cells:?}; ink at/right of `_` on {} frames, cols {cols:?}",
                    take.name,
                    h.beyond.len()
                ));
            }
        }
    }
    assert!(bad.is_empty(), "the owner's shape lit:{bad}");
}

/// No ink on the match row when the match hops. `hop` (`git log --stat` then
/// `ls -la`, keys `log`: `l` hops four cells to `ls -la`'s last `l`) and `hop2`
/// (`echo hello world` then `ls -la`, keys `lo w`: `l` hops four cells, `o`
/// four more to `hello`'s `lo`), the latter also with the prompt on a full
/// screen's last row. Each hop is four cells on one credit and lands under a
/// glyph its key never typed, so it is refused `program-row`, and its press is
/// forgotten (else it would match `hello`'s `l` at the next landing).
///
/// `backhop` (`echo hello world`, keys `lo`) walks back: `l` to `world`'s `l`
/// (R, 45), `lo` six cells back to `hello`'s `lo` (R, 39). The retreat is held
/// as Ink's park and judged at its own clock, so the landing gate reads the
/// glyph held with the park (`HeldPark::landing_cell`).
#[test]
fn a_match_that_hops_along_its_row_lays_no_ink_there() {
    let mut bad = String::new();
    for take in HOP_TAKES {
        for present in WHOLE {
            let h = replay(take, present);
            let r = h.match_row();
            assert!(
                !h.match_hops.is_empty(),
                "non-vacuity: {} walks the caret along the match row",
                take.name
            );
            assert!(
                h.status_rows.contains(&(r + 1)),
                "{}: the status row sits under the match row: {:?}",
                take.name,
                h.status_rows
            );
            let from = h.census_from.expect("the take presses Ctrl-R").0;
            let end = h.search_end_us.expect("Ctrl-G ends the search");
            let hops: Vec<Verdict> = h
                .ring
                .iter()
                .filter(|v| v.origin.0 == r && v.target.0 == r && (from..end).contains(&v.us))
                .copied()
                .collect();
            let refused = hops.len() == h.match_hops.len()
                && hops.iter().all(|v| {
                    v.reason == CursorGlow::DECLINE_PROGRAM_ROW
                        && v.licence == AdmissionRecord::LICENCE_NONE
                });
            let cells = h.cells_on(r);
            let body: Vec<_> = h.lives.keys().filter(|(row, _)| *row == r).collect();
            let over: Vec<_> = h.over_lives.keys().filter(|(row, _)| *row == r).collect();
            if !refused || !cells.is_empty() || !body.is_empty() || !over.is_empty() {
                bad.push_str(&format!(
                    "\n{}/{present:?}: hops {:?} judged {hops:?}; cells {cells:?} body {body:?} hot edge {over:?}",
                    take.name, h.match_hops
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "the hopping match row R lit, or a hop along it not refused `program-row`:{bad}"
    );
}

/// An entry switch shaped like a soft-wrapped caret lays nothing. `a` puts the
/// caret on `cat a`'s `a` (R, 35); `ad` switches to an older two-line entry
/// whose first line holds `d` at that column and whose indented second line
/// starts with the match, caret (R+1, 2). That looks like Claude Code's
/// soft-wrapped caret, but zle rewrote the origin row left of the origin
/// (`cat ` to `cd /`), while a composer key goes in at the fold and leaves it,
/// so `soft_wrapped_caret` rejects it.
#[test]
fn an_entry_switch_shaped_like_a_soft_wrap_lays_nothing_on_the_match() {
    let mut bad = String::new();
    for take in [MULTILINE, MULTILINE_SLOW] {
        for present in WHOLE {
            let h = replay(take, present);
            let r = h.match_row();
            let from = h.census_from.expect("the take presses Ctrl-R").0;
            let end = h.search_end_us.expect("Ctrl-G ends the search");
            let switch: Vec<Verdict> = h
                .ring
                .iter()
                .filter(|v| v.origin.0 == r && v.target.0 == r + 1 && (from..end).contains(&v.us))
                .copied()
                .collect();
            assert!(
                !switch.is_empty(),
                "non-vacuity: {} switches the caret down to the entry's second line",
                take.name
            );
            let refused = switch.iter().all(|v| {
                v.reason == CursorGlow::DECLINE_PROGRAM_ROW
                    && v.licence == AdmissionRecord::LICENCE_NONE
            });
            let cells: Vec<_> = h
                .cells
                .keys()
                .filter(|(row, _, _)| (r..=r + 2).contains(row))
                .collect();
            let body: Vec<_> = h
                .lives
                .keys()
                .filter(|(row, _)| (r..=r + 1).contains(row))
                .collect();
            let over: Vec<_> = h
                .over_lives
                .keys()
                .filter(|(row, _)| (r..=r + 1).contains(row))
                .collect();
            if !refused || !cells.is_empty() || !body.is_empty() || !over.is_empty() {
                bad.push_str(&format!(
                    "\n{}/{present:?}: switch judged {switch:?}; cells {cells:?} body {body:?} hot edge {over:?}",
                    take.name
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "an entry switch lit the match, or was not refused `program-row`:{bad}"
    );
}

/// Bash's reverse-i-search retreat lays nothing under the match: bash 3.2,
/// `echo hello world`, Ctrl-R, `lo w`. `lo` retreats from `world`'s `l`
/// (R, 37) to `hello`'s `lo` (R, 32), the same held-park retreat as zsh's
/// `backhop`; it is refused `program-row` and (R, 31) gets no cell. Not pinned:
/// later query keys insert inside `(reverse-i-search)` (`ESC[1@`), an ordinary
/// one-cell echo.
#[test]
fn bash_s_reverse_search_retreat_lays_nothing_under_the_match() {
    for present in WHOLE {
        let h = replay(BASH, present);
        let r = h.match_row();
        let retreat = h
            .ring
            .iter()
            .find(|v| v.origin == (r, 37) && v.target == (r, 32))
            .copied();
        assert!(
            retreat.is_some_and(|v| v.reason == CursorGlow::DECLINE_PROGRAM_ROW
                && v.licence == AdmissionRecord::LICENCE_NONE),
            "{present:?}: the retreat is refused `program-row`: {retreat:?}\n{}",
            h.report("bash")
        );
        let under: Vec<_> = h
            .cells
            .keys()
            .filter(|&&(row, col, _)| row == r && col == 31)
            .collect();
        assert!(
            under.is_empty(),
            "{present:?}: a cell under the retreat's landing: {under:?}\n{}",
            h.report("bash")
        );
    }
}

/// A longer search: Backspace, `ude`, Right to leave, ` x` on the prompt row.
/// During the search the status row has no band under untyped glyphs and the
/// prompt row none at all. The refused presses (`u`, `d`, `e` echoed on the
/// status row) are forgotten, so nothing typed during the search pays for a
/// later move: the Right is licensed by its own key and no verdict from the
/// Ctrl-R on is `licence=inflight`. After the search each of ` x` lights its
/// own cell, under its glyph, within 100 ms of its key (keyed on glyph and
/// clock: the Right's hop already laid a cell at the space's column).
#[test]
fn a_longer_search_lays_no_band_on_cells_nobody_typed() {
    let h = replay(CONTINUE, APP);
    let r = h.match_row();
    assert_eq!(h.status_rows, BTreeSet::from([r + 1]));
    let wrong = untyped_status_cells(&h);
    assert!(
        wrong.is_empty(),
        "status-row cells under untyped glyphs: {wrong:?}\n{}",
        h.report("continue")
    );
    let from = h.census_from.expect("the take presses Ctrl-R").0;
    let left = h.search_end_us.expect("Right leaves the search");
    let during: Vec<(u16, char)> = h
        .cells
        .iter()
        .filter(|((row, _, _), (first, _))| *row == r && *first < left)
        .map(|((_, c, g), _)| (*c, *g))
        .collect();
    assert!(
        during.is_empty(),
        "prompt-row cells while the search ran: {during:?}\n{}",
        h.report("continue")
    );
    let prompt_text: Vec<(u16, char)> = h
        .cells_on(r)
        .into_iter()
        .filter(|&(c, _)| c < PROMPT_COLS)
        .collect();
    assert!(
        prompt_text.is_empty(),
        "cells on the prompt's own text: {prompt_text:?}"
    );

    // The refused presses are forgotten.
    let right = h
        .ring
        .iter()
        .find(|v| v.us >= left && v.origin == (r, PROMPT_COLS) && v.target == (r, PROMPT_COLS + 1))
        .copied();
    assert!(
        right.is_some_and(|v| v.licence == AdmissionRecord::LICENCE_KEY),
        "the Right's hop is its own key's: {right:?}\n{}",
        h.report("continue")
    );
    let paid_in_flight: Vec<Verdict> = h
        .ring
        .iter()
        .filter(|v| v.us >= from && v.licence == AdmissionRecord::LICENCE_IN_FLIGHT)
        .copied()
        .collect();
    assert!(
        paid_in_flight.is_empty(),
        "moves paid by presses in flight: {paid_in_flight:?}"
    );

    // Typing after the search: Right left the caret one past the match's `c`;
    // ` x` went in there.
    let (space, x) = (PROMPT_COLS + 1, PROMPT_COLS + 2);
    assert_eq!(h.glyph(r, space), ' ');
    assert_eq!(h.glyph(r, x), 'x');
    let keys: Vec<(u64, char)> = h.keys.iter().copied().filter(|k| k.0 > left).collect();
    assert_eq!(
        keys.iter().map(|k| k.1).collect::<String>(),
        " x",
        "the keys typed after the search"
    );
    for ((key_us, glyph), col) in keys.into_iter().zip([space, x]) {
        let first = h.cells.get(&(r, col, glyph)).map(|&(first, _)| first);
        assert!(
            first.is_some_and(|first| (key_us..=key_us + 100_000).contains(&first)),
            "{glyph:?} at ({r},{col}) lit under its own glyph within 100 ms of its key at {key_us} µs: {first:?}\n{}",
            h.report("continue")
        );
    }
}

/// The wrapped match: an 85-character entry after the 31-column prompt wraps,
/// so the status row sits under the match's last line. From `gi` on the caret
/// parks two rows above the status row, beyond the probe's reach; with the
/// anchor's glyph handed over, nothing is laid under the fake cursor.
#[test]
fn a_wrapped_match_lays_nothing_under_zle_s_fake_cursor() {
    let h = replay(WRAPPED, APP);
    let far = h
        .parked_prints
        .iter()
        .filter(|((cr, _), (ar, _))| ar.saturating_sub(*cr) == 2)
        .count();
    assert!(
        far >= 3,
        "non-vacuity: `t`, SPACE and `c` echo two rows below the caret: {:?}",
        h.parked_prints
    );
    assert!(
        h.status_rows.contains(&(h.match_row() + 2)),
        "the status row moves under the match's last line: {:?}",
        h.status_rows
    );
    let cells = untyped_status_cells(&h);
    assert!(
        cells.is_empty(),
        "cells under zle's fake cursor two rows down: {cells:?}\n{}",
        h.report("wrapped")
    );
    assert!(h.beyond.is_empty(), "ink at/right of `_`: {:?}", h.beyond);
}

/// The gates' blast radius: Claude Code's and Codex's composer takes, fed as
/// the app feeds them (window and composed carets), print off the caret's row
/// repeatedly, and neither gate refuses anything. Both gates log refusals as
/// `program-row`, and these takes have no other such verdict, so none at all
/// means neither fired.
#[test]
fn the_gates_refuse_nothing_in_claude_code_s_or_codex_s_composer() {
    // Each take under each caret is its own replay, run in parallel (serially they were this
    // binary's 25 s wall) and judged in order.
    let runs: Vec<(&Composer, bool)> = COMPOSERS
        .iter()
        .flat_map(|take| [(take, true), (take, false)])
        .collect();
    let read = crate::common::par_map(&runs, |&(take, window_caret)| {
        let h = replay_composer(
            take,
            Present {
                window_caret,
                ..APP
            },
        );
        let licensed = h
            .ring
            .iter()
            .filter(|v| v.licence != AdmissionRecord::LICENCE_NONE)
            .count();
        let refused: Vec<Verdict> = h
            .ring
            .iter()
            .filter(|v| v.reason == CursorGlow::DECLINE_PROGRAM_ROW)
            .copied()
            .collect();
        (h.keys.len(), licensed, h.parked_prints.len(), refused)
    });
    let mut bad = String::new();
    let mut parked = 0usize;
    for (&(take, window_caret), (keys, licensed, parked_prints, refused)) in runs.iter().zip(read) {
        assert!(keys >= 5, "non-vacuity: {} types", take.name);
        assert!(licensed >= 5, "non-vacuity: {} lights its keys", take.name);
        parked += parked_prints;
        if !refused.is_empty() {
            bad.push_str(&format!(
                "\n{} (window caret {window_caret}): {refused:?}",
                take.name
            ));
        }
    }
    assert!(
        parked > 0,
        "non-vacuity: the composers print off the caret's row"
    );
    assert!(bad.is_empty(), "a gate refused a composer echo:{bad}");
}

/// A hidden caret keeps its lane on every path: a TUI hides the caret, parks it
/// below its input and draws its own fake cursor after each echo (`> a█`), so
/// every run ends on a glyph no key typed. The composed paths hand over no
/// hidden caret; the single-pane window hands it over marked hidden
/// (`observe_caret_drawn`), so the visible-caret gate stays out: same cells,
/// no `program-row` refusal.
#[test]
fn a_hidden_caret_s_echo_is_judged_alike_on_every_path() {
    let lay = |window_caret| {
        let mut h = Host::new(
            24,
            100,
            Present {
                window_caret,
                ..APP
            },
        );
        h.feed("\x1b[?25l\x1b[5;1H> \x1b[6;1H".as_bytes());
        h.census_from = Some((h.us(), 4));
        for (i, k) in "abc".chars().enumerate() {
            h.idle_ms(120);
            h.hint(&[k as u8]);
            h.feed(format!("\x1b[5;{}H{k}\u{2588}\x1b[6;1H", 3 + i).as_bytes());
        }
        h.idle_ms(200);
        h
    };
    let window = lay(true);
    let composed = lay(false);
    assert_eq!(window.text(4), "> abc\u{2588}");
    assert_eq!(
        window.hidden_parked_prints, 4,
        "non-vacuity: the window hands over a hidden caret parked off every echo"
    );
    let laid = |h: &Host| h.cells_on(4);
    assert!(
        !laid(&composed).is_empty(),
        "the hidden-caret lane lights these echoes:\n{}",
        composed.report("composed")
    );
    assert_eq!(
        laid(&window),
        laid(&composed),
        "\n{}\n{}",
        window.report("window"),
        composed.report("composed")
    );
    for h in [&window, &composed] {
        let refused: Vec<&Verdict> = h
            .ring
            .iter()
            .filter(|v| v.reason == CursorGlow::DECLINE_PROGRAM_ROW)
            .collect();
        assert!(refused.is_empty(), "{refused:?}");
    }
}

/// Control: the same four keys at the plain prompt light four cells and are
/// gone by the idle ceiling.
#[test]
fn plain_prompt_typing_lights_and_retires_on_the_normal_schedule() {
    let mut h = Host::new(PLAIN.rows, PLAIN.cols, APP);
    // The control has no Ctrl-R: census from the first typed key.
    for (us, ev) in events(PLAIN.src) {
        h.run_to(us);
        match ev {
            Ev::In(b) => {
                if h.census_from.is_none() && matches!(b.as_slice(), [c] if c.is_ascii_lowercase())
                {
                    h.census_from = Some((h.us(), h.term.cursor().row));
                }
                h.hint(&b);
            }
            Ev::Out(b) => h.feed(&b),
            Ev::Clear => h.glow.clear_typed(h.now),
        }
    }
    h.idle_ms(3_000);
    h.close_glass();
    let r = h.match_row();
    let typed: Vec<(u16, char)> = h.cells_on(r);
    let glyphs: String = typed.iter().map(|(_, g)| *g).collect();
    assert_eq!(glyphs, "clad", "the four typed cells lit: {typed:?}");
    assert!(
        h.outlived().is_empty(),
        "the control outlived the idle ceiling: {:?}",
        h.outlived()
    );
}

/// A parked caret's own echo still lights: the program's run on the row below
/// ends on the key, so each key's cell is laid under its own glyph.
#[test]
fn a_parked_caret_s_own_echo_still_lights() {
    let mut h = Host::new(24, 100, APP);
    h.feed(b"\x1b[5;1Hp% ");
    let r = h.term.cursor().row;
    h.census_from = Some((h.us(), r + 1));
    h.feed(b"\r\r\n> \x1b[A\x1b[3G");
    for (i, k) in "abc".chars().enumerate() {
        h.idle_ms(120);
        h.hint(&[k as u8]);
        h.feed(format!("\x1b[1B\x1b[{}G{k}\x1b[A\x1b[3G", 3 + i).as_bytes());
        assert_eq!((h.term.cursor().row, h.term.cursor().col), (r, 2));
    }
    h.idle_ms(200);
    assert_eq!(h.text(r + 1), "> abc");
    let laid: Vec<(u16, char)> = h.cells_on(r + 1);
    assert_eq!(
        laid,
        vec![(2, 'a'), (3, 'b'), (4, 'c')],
        "each key laid under its own glyph:\n{}",
        h.report("parked")
    );
}

/// Enter hands the line over: `ls` at the prompt on row R, then Enter; the
/// program draws its input two rows down and parks its visible caret on row R,
/// echoing keys on the input row. Enter took the hand off row R, so the
/// hand's-row hold does not refuse the program's echoes: each key lays under
/// its glyph, and the first names the input row as the hand's.
#[test]
fn a_parked_caret_s_own_echo_lights_after_enter_on_the_caret_s_row() {
    let mut h = Host::new(24, 100, APP);
    h.feed(b"\x1b[5;1Hp% ");
    let r = h.term.cursor().row;
    h.census_from = Some((h.us(), r));
    for k in "ls".chars() {
        h.idle_ms(120);
        h.hint(&[k as u8]);
        h.feed(k.to_string().as_bytes());
    }
    h.idle_ms(120);
    assert_eq!(
        h.cells_on(r),
        vec![(3, 'l'), (4, 's')],
        "PRECONDITION: the hand typed on row R:\n{}",
        h.report("prompt")
    );
    h.hint(b"\r");
    h.feed(b"\r\n");
    // The program starts: its input row two rows down, the visible caret
    // parked back on row R.
    h.idle_ms(400);
    h.census_from = Some((h.us(), r + 2));
    h.feed(format!("\x1b[{};1H> \x1b[{};3H", r + 3, r + 1).as_bytes());
    for (i, k) in "abc".chars().enumerate() {
        h.idle_ms(120);
        h.hint(&[k as u8]);
        h.feed(format!("\x1b[{};{}H{k}\x1b[{};3H", r + 3, 3 + i, r + 1).as_bytes());
        assert_eq!(h.term.cursor().row, r, "the caret stays parked on row R");
    }
    h.idle_ms(200);
    assert_eq!(h.text(r + 2), "> abc");
    assert_eq!(
        h.cells_on(r + 2),
        vec![(2, 'a'), (3, 'b'), (4, 'c')],
        "each key laid under its own glyph:\n{}",
        h.report("after enter")
    );
}

/// Typeahead into the minibuffer: two keys pressed before the first echo is
/// presented both end on `_`; the first is refused by the glyph gate and the
/// second finds no credit, so nothing is laid under the fake cursor. The forget
/// itself is pinned by `a_longer_search_lays_no_band_on_cells_nobody_typed`.
#[test]
fn typeahead_into_the_minibuffer_lays_nothing() {
    let mut h = Host::new(24, 100, APP);
    h.feed(b"\x1b[5;1Hp% ");
    let r = h.term.cursor().row;
    h.hint(&[0x12]);
    h.feed(b"\r\r\nsearch: _\x1b[K\x1b[A\x1b[6D");
    h.idle_ms(120);
    h.hint(b"a");
    h.idle_ms(5);
    h.hint(b"b");
    h.idle_ms(12);
    h.feed(b"\x1b[1B\x1b[5Ca_\x1b[A\x1b[7D");
    h.idle_ms(16);
    h.feed(b"\x1b[1B\x1b[6Cb_\x1b[A\x1b[8D");
    h.idle_ms(1_200);
    assert_eq!(h.text(r + 1), "search: ab_");
    assert_eq!(h.parked_prints.len(), 3, "non-vacuity: both echoes parked");
    assert_eq!(
        untyped_status_cells(&h),
        vec![],
        "\n{}",
        h.report("typeahead")
    );
}

/// A glyph-less press keeps the lane: a wide glyph or IME commit is banked by
/// cell count alone, so a CJK key echoed below a parked visible caret is laid
/// as usual though the run ends on its continuation column.
#[test]
fn a_wide_glyph_echo_below_a_parked_caret_still_lights() {
    let mut h = Host::new(24, 100, APP);
    h.feed(b"\x1b[5;1Hp% ");
    let r = h.term.cursor().row;
    h.census_from = Some((h.us(), r + 1));
    h.feed(b"\r\r\n> \x1b[A\x1b[3G");
    for (i, k) in ['中', '文'].into_iter().enumerate() {
        h.idle_ms(120);
        h.glow.supersede_typed_press();
        h.glow.note_typed_glyph(h.now, 2, false, TypedClass::Glyph);
        h.feed(format!("\x1b[1B\x1b[{}G{k}\x1b[A\x1b[3G", 3 + 2 * i).as_bytes());
    }
    h.idle_ms(200);
    assert_eq!(
        h.term.print_anchor_glyph(),
        Some('\0'),
        "the run ends on a continuation"
    );
    let laid: Vec<u16> = h.cells_on(r + 1).into_iter().map(|(c, _)| c).collect();
    assert!(
        laid.contains(&2) && laid.contains(&4),
        "both wide keys laid: {laid:?}\n{}",
        h.report("wide")
    );
}

/// How long light laid on a torn redisplay may stay on the match row: the
/// caret's rest (`CursorGlow::TORN_REST_S`, 32 ms) and a frame to see it,
/// then the recall's retract (`12·n + 240` ms for the one or two cells laid)
/// and its last frame.
const RECALL_BOUND_MS: u64 = 330;

/// Band ink on the match row that outlived [`RECALL_BOUND_MS`] — from its
/// first frame when one read tore, from the search's last key when every
/// read did (each key then re-lays the match's glyph while the last lay is
/// still retracting) — and live ribbon cells left on it once the replay
/// rested.
fn match_row_kept(h: &Host) -> Vec<String> {
    let r = h.match_row();
    let every = h.present.tear == Tear::Every;
    let last_key = h.last_key_us();
    let mut kept: Vec<String> = h
        .lives
        .iter()
        .filter(|((row, _), l)| {
            let from = if every { last_key } else { l.first_us };
            *row == r && l.last_us.saturating_sub(from) > RECALL_BOUND_MS * 1000
        })
        .map(|((_, c), l)| {
            let from = if every { last_key } else { l.first_us };
            format!("ink ({r},{c}) {} ms", l.last_us.saturating_sub(from) / 1000)
        })
        .collect();
    kept.extend(live_cols(h, r).iter().map(|c| format!("live ({r},{c})")));
    kept
}

/// Every owner take with a present after every piece of every read, on the
/// 120 Hz train and on the host's wake: what `law` finds, per replay.
fn every_tear(law: fn(&Host) -> Vec<String>) -> String {
    let mut bad = String::new();
    for take in OWNER_TAKES {
        for host_wake in [false, true] {
            let h = replay(
                take,
                Present {
                    tear: Tear::Every,
                    host_wake,
                    ..APP
                },
            );
            let found = law(&h);
            if !found.is_empty() {
                bad.push_str(&format!(
                    "\n{}/{host_wake}: {found:?}\n{}",
                    take.name,
                    h.report(take.name)
                ));
            }
        }
    }
    bad
}

/// Each owner take's search reads torn at each of their piece boundaries,
/// one at a time, everything else whole: what `law` finds, per tear, with
/// how many tears were tried and how many program-row refusals the ring logged.
fn one_tear(law: fn(&Host) -> Vec<String>) -> (String, usize, usize) {
    let (mut bad, mut tried, mut refused) = (String::new(), 0, 0);
    for take in OWNER_TAKES {
        let evs = events(take.src);
        let from = evs
            .iter()
            .position(|(_, e)| matches!(e, Ev::In(b) if b.as_slice() == [0x12]))
            .expect("a Ctrl-R");
        let outs: Vec<usize> = evs[from..]
            .iter()
            .filter_map(|(_, e)| match e {
                Ev::Out(b) => Some(pieces(b).len()),
                Ev::In(_) | Ev::Clear => None,
            })
            .collect();
        // The Ctrl-R's read and the four keys' (a take whose `d` read its
        // BEL separately has one more).
        for (n, &np) in outs.iter().enumerate().take(6) {
            for k in 1..np {
                tried += 1;
                let h = replay(
                    take,
                    Present {
                        tear: Tear::One { n, k },
                        ..APP
                    },
                );
                refused += h
                    .ring
                    .iter()
                    .filter(|v| v.reason == CursorGlow::DECLINE_PROGRAM_ROW)
                    .count();
                let found = law(&h);
                if !found.is_empty() {
                    bad.push_str(&format!(
                        "\n{} read {n} torn after piece {k}: {found:?}",
                        take.name
                    ));
                }
            }
        }
    }
    (bad, tried, refused)
}

/// The existing retention ceiling also holds with a present after every
/// redisplay fragment. Strict transient-ink checks below cover admission.
#[test]
fn torn_reads_leave_no_band_on_the_match_row() {
    let bad = every_tear(match_row_kept);
    assert!(bad.is_empty(), "band kept on the match row R:{bad}");
}

/// Each possible single tear preserves the retention ceiling and exercises
/// program-row refusal, rather than passing with an empty fixture.
#[test]
fn one_torn_read_at_any_boundary_leaves_no_band_on_the_match_row() {
    let (bad, tried, refused) = one_tear(match_row_kept);
    assert!(tried > 100, "the sweep tried {tried} tears");
    assert!(refused > 0, "non-vacuity: some program rows were refused");
    assert!(
        bad.is_empty(),
        "a single torn read left band on the match row:{bad}"
    );
}

/// Any band ink on the match row, with how long each cell carried it.
fn match_row_inked(h: &Host) -> Vec<String> {
    let r = h.match_row();
    h.lives
        .iter()
        .filter(|((row, _), _)| *row == r)
        .map(|((_, c), l)| format!("({r},{c}) {} ms", (l.last_us - l.first_us) / 1000))
        .collect()
}

#[test]
fn torn_reads_lay_no_band_on_the_match_row() {
    let bad = every_tear(match_row_inked);
    assert!(bad.is_empty(), "band laid on the match row R:{bad}");
    // A PTY continuation may arrive after any rest timer or press expiry.
    // The parked row's ownership, rather than silence, must keep it dark.
    for delay_ms in [64, 1_500, 12_000] {
        let mut h = Host::new(24, 100, APP);
        let row = plain_prompt(&mut h);
        h.hint(&[0x12]);
        h.feed(b"\r\r\nbck-i-search: _\x1b[K\x1b[A\x1b[16C");
        h.hint(b"c");
        h.feed(b"\x1b[4mc");
        h.idle_ms(delay_ms);
        h.feed(b"\x1b[24mlaude --dangerously-skip-permissions\x1b[1B\x1b[54Dc_\x1b[A\x1b[15C");
        h.idle_ms(100);
        assert_eq!(h.match_row(), row);
        assert!(
            match_row_inked(&h).is_empty(),
            "delayed {delay_ms} ms continuation painted the match: {}",
            h.report("delayed redisplay")
        );
    }
}

#[test]
fn one_torn_read_at_any_boundary_lays_no_band_on_the_match_row() {
    let (bad, tried, _) = one_tear(match_row_inked);
    assert!(tried > 100, "the sweep tried {tried} tears");
    assert!(
        bad.is_empty(),
        "a single torn read painted the match row:{bad}"
    );
}

/// A footer does not displace an established typing row. After an unowned
/// parked row is refused, a classified navigation boundary restores ordinary
/// typing on that row; neither path waits for a rest timer.
#[test]
fn ordinary_typing_keeps_its_row_and_a_classified_boundary_restores_it() {
    let mut h = Host::new(24, 100, APP);
    let row = plain_prompt(&mut h);
    h.feed(b"\x1b[1B\rstatus\x1b[A\x1b[4G");
    h.hint(b"a");
    h.feed(b"a");
    assert_eq!(
        live_cols(&h, row),
        [3],
        "first ordinary echo lights immediately"
    );
    h.feed(b"\x1b[1B\rstatus\x1b[A\x1b[5G");
    h.hint(b"b");
    h.feed(b"b");
    assert_eq!(
        live_cols(&h, row),
        [3, 4],
        "the footer keeps the hand's row"
    );

    let navigation: [fn(&mut CursorGlow, Instant); 2] =
        [CursorGlow::note_motion, CursorGlow::note_navigation];
    for navigate in navigation {
        let mut h = Host::new(24, 100, APP);
        let row = plain_prompt(&mut h);
        h.glow.clear_typed(h.now);
        h.feed(b"\x1b[1B\rstatus\x1b[A\x1b[4G");
        h.hint(b"a");
        h.feed(b"a");
        assert!(
            live_cols(&h, row).is_empty(),
            "unowned parked row is ambiguous"
        );
        navigate(&mut h.glow, h.now);
        h.feed(b"\x1b[C");
        h.hint(b"b");
        h.feed(b"b");
        assert!(
            live_cols(&h, row).contains(&5),
            "navigation restores own-row typing"
        );
    }
}

/// A licensed insert's echo puts the hand on its row (`CursorGlow::lay_insert`). A click
/// (the generic boundary) leaves the caret's row unowned, a footer tick parks it, and a
/// paste (⌘V, which keeps whatever hand there is: none) lays its echo there. The paste's
/// bytes landing at the caret prove the row takes input, so the next key's echo lights, as
/// it does after a navigation boundary. RED without the insert taking the hand: `d` is
/// refused program-row, its press forgotten, and every key after it on that row stays dark
/// until some classified boundary (the gap after a paste into Claude Code's composer while
/// its footer ticks).
#[test]
fn a_licensed_insert_takes_the_hand_s_row() {
    let mut h = Host::new(24, 100, APP);
    let row = plain_prompt(&mut h);
    h.glow.clear_typed(h.now);
    h.feed(b"\x1b[1B\rstatus\x1b[A\x1b[4G");
    h.idle_ms(40);
    h.glow.clear_typed_keeping_hand(h.now);
    let press = h.now;
    h.idle_ms(2);
    h.glow
        .note_insert_delivered_from(press, h.now, InsertWidth::Cells(3));
    h.feed(b"abc");
    assert_eq!(
        live_cols(&h, row),
        [3, 4, 5],
        "the paste's echo lights: {}",
        h.report("paste")
    );
    h.feed(b"\x1b[1B\rstatus\x1b[A\x1b[7G");
    h.idle_ms(40);
    h.hint(b"d");
    h.feed(b"d");
    assert!(
        live_cols(&h, row).contains(&6),
        "the key after the paste lights: {}",
        h.report("key after paste")
    );
    assert!(
        h.ring
            .iter()
            .all(|v| v.reason != CursorGlow::DECLINE_PROGRAM_ROW),
        "nothing refused program-row: {}",
        h.report("key after paste")
    );
}

/// A recalled lay hands back the doubt it settled. After the Ctrl-R (the generic boundary)
/// and before zle shows the search, a read torn at the first key's glyph lays `c` on the
/// caret's row: that lay put the hand there and settled the row's doubt. zle then prints its
/// status row and rests the caret on the `c`, and the lay is recalled (`licence=recall`):
/// the hand goes back, and so must the doubt (`TornLay::unowned_before`), or the next key's
/// torn read lays `l` on the match row again where the parked row refuses it.
#[test]
fn a_recalled_lay_hands_back_the_row_s_doubt() {
    let mut h = Host::new(24, 100, APP);
    let row = plain_prompt(&mut h);
    h.hint(&[0x12]);
    h.idle_ms(40);
    h.hint(b"c");
    h.feed(b"c");
    assert_eq!(live_cols(&h, row), [3], "the torn read lays `c`");
    h.feed(b"\r\r\nbck-i-search: c_\x1b[K\x1b[A\x1b[4G");
    h.idle_ms(100);
    assert!(
        h.ring.iter().any(|v| v.licence == "recall"),
        "the lay is recalled: {}",
        h.report("recall")
    );
    h.hint(b"l");
    h.feed(b"l");
    h.idle_ms(20);
    assert!(
        h.cells_on(row).iter().all(|&(_, glyph)| glyph != 'l'),
        "the next torn read lays nothing on the match row: {}",
        h.report("next key")
    );
    assert_eq!(
        h.ring
            .iter()
            .filter(|v| v.reason == CursorGlow::DECLINE_PROGRAM_ROW)
            .count(),
        1,
        "the parked row refuses `l`: {}",
        h.report("next key")
    );
}

/// A row the hand put input on stays its own after the hand moves on. A click leaves the
/// caret's row unowned; the hand's input lands there (a key's echo, or a paste's); a composer
/// newline and a key put the hand on the next line; two Backspaces bring the caret back up; a
/// footer tick parks the caret on the first row. The first row took the hand's input, so the
/// park cannot disown it (`CursorGlow::hand_lands_on`): the next key there lights. RED before
/// (2026-10-20): `c` refused program-row, its press forgotten, the row dark from then on, for
/// a key (the typed echo put the hand on the row and left the click's doubt standing) and for
/// a paste (`lay_insert` cleared the doubt itself, so only the paste was spared).
#[test]
fn a_row_the_hand_put_input_on_stays_its_own_after_the_hand_moves_on() {
    for paste in [false, true] {
        let mut h = Host::new(24, 100, APP);
        let row = plain_prompt(&mut h);
        h.glow.clear_typed(h.now);
        h.idle_ms(40);
        if paste {
            h.glow.clear_typed_keeping_hand(h.now);
            let press = h.now;
            h.idle_ms(2);
            h.glow
                .note_insert_delivered_from(press, h.now, InsertWidth::Cells(1));
        } else {
            h.hint(b"a");
        }
        h.feed(b"a");
        assert_eq!(live_cols(&h, row), [3], "paste={paste}: the input lights");
        h.idle_ms(40);
        h.glow.supersede_typed_press();
        h.glow.note_newline_break(h.now);
        h.feed(b"\r\n");
        h.idle_ms(40);
        h.hint(b"b");
        h.feed(b"b");
        h.idle_ms(40);
        h.hint(&[0x7f]);
        h.feed(b"\x08\x1b[K");
        h.idle_ms(40);
        h.hint(&[0x7f]);
        h.feed(b"\x1b[A\x1b[5G");
        h.idle_ms(40);
        h.feed(b"\x1b[3B\rstatus\x1b[3A\x1b[5G");
        h.idle_ms(40);
        h.hint(b"c");
        h.feed(b"c");
        assert!(
            live_cols(&h, row).contains(&4),
            "paste={paste}: the key back on the first row lights: {}",
            h.report("back on the row")
        );
        assert!(
            h.ring
                .iter()
                .all(|v| v.reason != CursorGlow::DECLINE_PROGRAM_ROW),
            "paste={paste}: nothing refused program-row: {}",
            h.report("back on the row")
        );
    }
}

/// The recall's negative control. A highlighter's redraw (the key's echo, then back to the
/// word's start and the word reprinted in its new colour), torn after every piece, walks the
/// caret left of the echo mid-write but rests right of it: the key's cell stays. An arrow
/// pressed right after an echo walks the caret onto it behind a key: that cell stays too.
/// Nothing is recalled.
#[test]
fn a_caret_resting_right_of_the_echo_or_behind_a_key_keeps_it() {
    let mut h = Host::new(24, 100, APP);
    h.feed(b"\x1b[5;1Hp% ");
    let r = h.term.cursor().row;
    h.census_from = Some((h.us(), r));
    for k in "ls".chars() {
        h.idle_ms(120);
        h.hint(&[k as u8]);
        h.feed(k.to_string().as_bytes());
    }
    h.idle_ms(120);
    h.hint(b"x");
    for piece in [&b"x"[..], b"\x1b[3D", b"\x1b[32mlsx\x1b[m"] {
        h.feed(piece);
        h.now += Duration::from_micros(50);
    }
    h.idle_ms(120);
    h.hint(b"y");
    h.feed(b"y");
    h.idle_ms(20);
    h.hint(b"\x1b[D");
    h.feed(b"\x1b[D");
    h.idle_ms(100);
    assert_eq!(
        (h.term.cursor().row, h.term.cursor().col),
        (r, 6),
        "the arrow put the caret on `y`"
    );
    let live: Vec<u16> = h
        .glow
        .v2_ribbon()
        .expect("rainbow kitty owns the frame")
        .cells()
        .iter()
        .filter(|x| x.row == r && !x.leaving())
        .map(|x| x.col)
        .collect();
    assert!(
        [3, 4, 5, 6].iter().all(|c| live.contains(c)),
        "`lsxy` stays lit: {live:?}\n{}",
        h.report("rest right")
    );
    assert!(
        h.ring.iter().all(|v| v.licence != "recall"),
        "nothing recalled:\n{}",
        h.report("rest right")
    );
}

/// A prompt `p% ` on the fifth row, the census running from it; the row.
fn plain_prompt(h: &mut Host) -> u16 {
    h.feed(b"\x1b[5;1Hp% ");
    let r = h.term.cursor().row;
    h.census_from = Some((h.us(), r));
    r
}

/// The live (not leaving) ribbon cells on `row`.
fn live_cols(h: &Host, row: u16) -> Vec<u16> {
    h.glow
        .v2_ribbon()
        .expect("rainbow kitty owns the frame")
        .cells()
        .iter()
        .filter(|x| x.row == row && !x.leaving())
        .map(|x| x.col)
        .collect()
}

/// The recall's other negative controls, each walking the caret back onto the cell a key just
/// laid with no key behind the move. A highlighter's redraw split across reads (the echo, a step
/// left, 60 ms, the glyph recoloured), the same redraw torn into the next key's echo, and a
/// keyless step left 10 ms after the echo (vi-mode Escape: its key is no typed press) print
/// nothing off the echo's row; vim's `showmatch` does (the ruler), but the hand was already
/// typing on the row. And an earlier key's cell at or right of where the caret rests keeps the
/// lay whatever was printed. A recall needs both a print off the row and the hand off it.
#[test]
fn a_keyless_walk_back_onto_a_typed_echo_keeps_it() {
    let mut bad = String::new();
    // The highlighter's redraw, split 60 ms across reads.
    let mut h = Host::new(24, 100, APP);
    let r = plain_prompt(&mut h);
    h.idle_ms(120);
    h.hint(b"l");
    h.feed(b"l");
    h.feed(b"\x1b[D");
    h.idle_ms(60);
    h.feed(b"\x1b[32ml\x1b[m");
    h.idle_ms(100);
    let lit = live_cols(&h, r);
    if lit != [3] {
        bad.push_str(&format!("\nsplit redraw: {lit:?}\n{}", h.report("split")));
    }
    // …torn into the next key's echo.
    let mut h = Host::new(24, 100, APP);
    let r = plain_prompt(&mut h);
    h.idle_ms(120);
    h.hint(b"l");
    h.feed(b"l");
    h.feed(b"\x1b[D");
    h.idle_ms(16);
    h.hint(b"s");
    h.feed(b"\x1b[32ml\x1b[ms");
    h.idle_ms(100);
    let lit = live_cols(&h, r);
    if lit != [3, 4] {
        bad.push_str(&format!(
            "\ninto the next key: {lit:?}\n{}",
            h.report("next")
        ));
    }
    // A keyless step left 10 ms after the echo (vi-mode Escape).
    let mut h = Host::new(24, 100, APP);
    let r = plain_prompt(&mut h);
    h.idle_ms(120);
    h.hint(b"l");
    h.feed(b"l");
    h.idle_ms(10);
    h.glow.clear_typed(h.now);
    h.feed(b"\x1b[D");
    h.idle_ms(100);
    let lit = live_cols(&h, r);
    if lit != [3] {
        bad.push_str(&format!("\nvi escape: {lit:?}\n{}", h.report("escape")));
    }
    // An earlier key's cell right of the rest, after a print off the row.
    let mut h = Host::new(24, 100, APP);
    let r = plain_prompt(&mut h);
    for k in "ls".chars() {
        h.idle_ms(120);
        h.hint(&[k as u8]);
        h.feed(k.to_string().as_bytes());
    }
    h.feed(b"\x1b[1B\rstatus\x1b[A\x1b[4G");
    h.idle_ms(100);
    let lit = live_cols(&h, r);
    if lit != [3, 4] {
        bad.push_str(&format!("\nforeign cell: {lit:?}\n{}", h.report("foreign")));
    }
    // vim's `showmatch`: `)` typed on a row the hand is typing on, the
    // ruler printed below, the caret resting on the `(` a while.
    let mut h = Host::new(24, 100, APP);
    let r = plain_prompt(&mut h);
    h.idle_ms(120);
    h.hint(b"(");
    h.feed(b"(");
    h.idle_ms(6_000);
    h.hint(b")");
    h.feed(b")");
    h.feed(b"\x1b[24;90H1,5\x1b[5;4H");
    h.idle_ms(100);
    let lit = live_cols(&h, r);
    if lit != [4] {
        bad.push_str(&format!("\nshowmatch: {lit:?}\n{}", h.report("showmatch")));
    }
    assert!(bad.is_empty(), "a typed echo was recalled:{bad}");
}

/// Whatever the search's replays lit (whole, torn after every piece, or on the host's wake)
/// retires on the normal clock: no cell is inked `IDLE_CEILING_MS` after the last key, and no
/// replay's engine sleeps with ink on glass.
#[test]
fn isearch_ink_retires_within_the_idle_ceiling() {
    let mut bad = String::new();
    for take in OWNER_TAKES.into_iter().chain([CONTINUE, WRAPPED]) {
        for present in [
            APP,
            Present {
                host_wake: true,
                ..APP
            },
            Present {
                tear: Tear::Every,
                ..APP
            },
        ] {
            let h = replay(take, present);
            let left = h.outlived();
            if !left.is_empty() {
                bad.push_str(&format!(
                    "\n{}/{present:?}: inked until (cell, µs after the last key) {left:?}",
                    take.name
                ));
            }
            if let Some(&(us, n)) = h.idle_with_ink.first() {
                bad.push_str(&format!(
                    "\n{}/{present:?}: the engine asked for no wake with {n} cells on glass at {us} µs",
                    take.name
                ));
            }
        }
    }
    assert!(bad.is_empty(), "band ink outlived the idle ceiling:{bad}");
}
