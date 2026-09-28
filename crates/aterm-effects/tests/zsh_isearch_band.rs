// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ZSH'S Ctrl-R SEARCH LAYS NO BAND** (2026-09-23 — the owner's
//! screenshot, the shipped default Rainbow Kitty: `user@… aterm % ` with the
//! history entry `claude --dangerously-skip-permissions`, Ctrl-R, `clad`
//! typed, the search failing at `d`. Row R shows the prompt line with the
//! match, the block caret on the `c` of `claude`; row R+1 shows `failing
//! bck-i-search: clad_`. A dithered band slab sat on row R+1 right of `clad`
//! — over zle's `_` and the blank after it, reaching a little below the row —
//! and a thin stub on row R under the `rm` of `aterm`, directly above it,
//! where nothing was typed).
//!
//! THE BYTES. zle draws the incremental search's STATUS row below the line
//! and walks the caret back up onto the MATCH. Every key's redisplay is ONE
//! write — one PTY read per key, the caret never hidden:
//!
//! ```text
//! c  ESC[4m c ESC[24m laude --dangerously-skip-permissions ESC[1B ESC[54D c_ ESC[A ESC[15C
//! l  ESC[4m c ESC[4m l ESC[24m ESC[1B ESC[18D l_ ESC[A ESC[14C
//! a  ESC[4m c ESC[4m l ESC[4m a ESC[24m ESC[1B ESC[18D a_ ESC[A ESC[13C
//! d  BEL ESC[24m c ESC[24m l ESC[24m a ESC[1B CR failing bck-i-search: clad_ ESC[A ESC[4C
//! ```
//!
//! THE MECHANISM. The caret the host samples never leaves row R, but the
//! terminal's print anchor ends on row R+1, one past zle's FAKE cursor `_` —
//! the PARKED visible-caret shape `CursorGlow::echo_anchor_pass` lights from
//! the anchor, laying the run's end advance as the key's echo. So every key
//! laid a cell under `_` (never under the key), and the failing `d` — the
//! whole status row rewritten with `failing ` prepended, the run's end
//! jumping nine cells on one press credit — was declined `no-credits` with
//! its credit-starved re-anchor landing laid under `_` anyway: the owner's
//! slab. The stub was that slab's own top edge: the tall body and its hot
//! edge reach about a tenth of a cell above the status row, under `rm`. Only
//! whole reads do this; nothing about a torn read is needed.
//!
//! THE MATCH THAT HOPS (the review's shape). The owner's match sits at the
//! entry's first column, so the caret never moves. zle puts the caret on the
//! newest entry's RIGHTMOST occurrence, so a match anywhere else walks the
//! VISIBLE caret along row R as the keys narrow it — `ls -la` on `l` is a
//! four-cell hop to its second `l`. That hop is the visible lane's: one
//! credit for four cells, a credit-starved typed re-anchor whose landing was
//! laid at the caret's left, under `-`; the next hop's under `hello`'s `l`,
//! and the ribbon walked the cells between — band on history text nobody
//! typed, the same landing law as the slab, one row up.
//!
//! THE FIX. Two gates, one witness: a key's light is laid only on a cell that
//! holds a live press's own glyph (`note_typed_expected`, the key the host
//! banked). On a VISIBLE caret parked off the anchor's row the anchored lane
//! lays an echo only where the run ends on such a glyph; zle's runs end on
//! `_`, so each echo is refused `program-row` and its presses are forgotten.
//! And the visible lane's typed re-anchor lays its landing only where the
//! caret row's probe holds such a glyph; zle's hops land on the match's own
//! text, so each is declined `program-row` before the ribbon sees it, its
//! press forgotten. The run's last glyph comes from the host —
//! `Terminal::print_anchor_glyph`, handed over with the anchor through
//! `observe_print_anchor_glyph` as `app_render.rs` does — or, from a host
//! that does not hand it over, from the caret row's own probe and its two
//! flanking rows. A WRAPPED history match puts the status row under the
//! match's last line, two rows below the caret, where only the host's glyph
//! can see it. A glyph-less press (a wide glyph, an IME commit) or an empty
//! pool is unknown and keeps each lane as it was. VISIBLE is the host's
//! word: the single-pane window hands a DECTCEM-hidden caret over too (for
//! the pet) and says it is hidden (`observe_caret_drawn`), so a hidden-caret
//! TUI keeps the anchored lane it has on every other path.
//!
//! THE TAKES — `fixtures/zsh-isearch-2026-09-23*.ptylog`, one line per read:
//! `<µs> O|I <hex>`, `O` one PTY read of program output, `I` one read of
//! keyboard input. The bare take and its `-g60`, `-burst` and `-cap120`
//! siblings are the owner's gesture (keys 150 ms, 60 ms and ~25 ms apart, and
//! one `aterm ctl image` capture per key — the take that showed the slab live)
//! recorded under a PTY wrapper running the owner's login zsh with aterm's
//! shell integration inside a headless aterm v0.91.0 (24×100); `-plain` is the
//! same four keys at the plain prompt, the band's normal schedule. `-continue`
//! (the search carried on: Backspace, `ude` to match again, Right to leave it,
//! ` x` typed on the prompt row), `-wrapped` (an 85-character history entry
//! whose match wraps, keys `git c`), `-hop`, `-hop2` and `-hop2-bottom` (the
//! match that hops; the last with the prompt on a full screen's last row) are
//! zsh 5.9 under a python pty at 30×100 with the owner's 31-column prompt, and
//! so are the fix/trail-land review's `2026-09-24-backhop` (the match walking
//! BACK along its row, keys `lo`) and `-multiline`/`-multiline-slow` (an entry
//! switch shaped like a soft-wrapped caret, keys `ad` 150 and 600 ms apart);
//! `bash-isearch-2026-09-24` is bash 3.2's reverse-i-search under it. The
//! recording host's name appears only as the same-length placeholder
//! `hostmachine0000` (replaced byte for byte where a take recorded it).
//! Claude Code's and Codex's composer takes,
//! recorded for `review_real_bytes.rs` and `codex_particle_replay.rs`, are
//! replayed here as the gates' blast radius.
//!
//! Every take is replayed through a real [`Terminal`], sampled in
//! `app_render.rs`'s order (scroll sync, the ribbon's witness rows, the row
//! probe and its neighbours, the print anchor and its glyph, whether the caret
//! is drawn, the tick), with the hints `app_input.rs` stamps for each key, on
//! a 120 Hz frame train and on the host's own wake schedule; the caret is the
//! single-pane window's (handed over hidden or not), or, for the controls that
//! say so, the composed paths' (drawn carets only). The harness probes every
//! frame, where the single-pane host skips the probe on a frame whose scroll
//! changed — a probe not taken is unknown to both gates, which then keep the
//! lane. The census is the INK on
//! glass — the ribbon's under-ink quads and the overlay stream (`tick`'s
//! `out`: the hot edge, sparks), per pixel, folded to cells — beside the live
//! ribbon cells, the glyph under each, and the admission ring.
//!
//! PINNED: the owner's whole-read shape (no ink on the match row, nothing
//! under or right of the `_`, each echo refused `program-row`), the match
//! that hops, forward or back (no ink on its row, each hop refused
//! `program-row`), the entry switch shaped like a soft wrap, bash's retreat,
//! the fewest-bytes minibuffer, the longer search and the forget that keeps
//! its keys from paying for the move after it, the wrapped match fed as the
//! host feeds it, and typeahead into the minibuffer; with their controls — plain
//! prompt typing lights and retires on schedule, typing after leaving the
//! search lights each key's own cell, a parked caret's own echo and a wide
//! glyph's (glyph-less, so unknown) still light, a hidden caret's echo is
//! judged alike on every path, Claude Code's and Codex's composers are
//! refused nothing, and nothing outlives the idle ceiling. Measured before
//! the minibuffer gate (`ac5b4c144`, the host feeding the anchor alone): the
//! bare take laid cells under `_` at (R+1, 15..17) and (R+1, 26), inked at
//! or right of the `_` on 488 of its frames, and put hot edge on row R at
//! cols 15-17 and 24-26; each echo was `licensed key` and the `d`
//! `no-credits`; the longer search laid cells under `_` at six columns of
//! its status row; the wrapped take laid (2, 17..19) under `_` — and still
//! does with the gate but without the host's glyph; the minimal minibuffer
//! laid its `_` cells. Measured before the landing gate (`06abe1777`): every
//! hop take laid (R, 34) under `-`, and `hop2` also (R, 38) under `hello`'s
//! `l` and (R, 35..37) between, each hop judged `no-credits`. Measured on
//! `8c1c2f52d` (both gates, before the flushed park's witness and the soft
//! wrap's prefix): `backhop`'s held retreat flushed `licensed key` and laid
//! (R, 38) under `hello`'s first `l`, bash's laid (R, 31); `multiline`'s
//! switch passed as a soft wrap, `licensed key`, and laid (R, 35) under
//! `cd /d`'s `d`. The two torn-read laws stay ignored: they need a read
//! boundary inside ONE zle redisplay, which a local whole read never
//! presents.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{
    AdmissionRecord, CursorGlow, Geom, GlowConfig, GlowStyle, ProbeTrust,
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
/// A pixel is INK at this level (premultiplied channel or source-over
/// opacity, 0..255) — below the glass census's 20/255 on purpose, so a faint
/// stub counts.
const INK: u8 = 8;
/// Idle after the last key by which every life has ended: the longest swoosh
/// is a 1.5 s rest plus 0.79 s at the slowest tempo (`review_real_bytes.rs`'s
/// `IDLE_CEILING_MS`).
const IDLE_CEILING_MS: u64 = 2_500;

/// One recorded take and the grid it was recorded at.
#[derive(Clone, Copy)]
struct Take {
    name: &'static str,
    src: &'static str,
    rows: usize,
    cols: usize,
    /// Command lines pre-fed so a python-pty take's prompt sits mid-grid, as
    /// the owner's did (the headless takes carry their own history).
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

/// The owner's gesture at four cadences.
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

/// The searches whose match is NOT at the entry's first column, so zle walks
/// the caret along the match row as the keys narrow it — forward, and (the
/// last) back.
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

/// Claude Code's four composer takes (2026-09-21) and Codex's four
/// (2026-09-16 and the wrapped composer of 2026-09-22); and Claude Code
/// 2.1.280's re-wrap takes (2026-09-23, fix/trail-soft-wrap): the seven
/// `claude_wrap_band.rs` replays (the soft-wrapped caret, the key that pushes
/// a word down, the controls) and `wrap_code_reflow.rs`'s recording (the
/// lifted word, the hard-broken token, the typo fixed mid-word) — every
/// shape whose re-anchor lays a cell other than `landing − 1`.
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

/// The owner's prompt, `user@hostmachine0000 aterm % `: its width is the
/// column the line (and the i-search's match) starts at in the python-pty
/// takes.
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

/// Cut one PTY read into the pieces a torn read can stop between: every
/// escape sequence (CSI to its final byte, OSC to BEL/ST, a two-byte ESC),
/// every C0 control, every run of printable text.
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
        pack: None,
    }
}

/// How the reads are presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tear {
    /// Every read processed whole, a present after it.
    Whole,
    /// A present after EVERY piece of every read from the Ctrl-R on.
    Every,
    /// Exactly one read (the `n`th output read from the Ctrl-R on) torn once,
    /// after its first `k` pieces.
    One { n: usize, k: usize },
}

/// One replay's presentation.
#[derive(Clone, Copy, Debug)]
struct Present {
    tear: Tear,
    /// Present only when the host would — on PTY output, on a key, and on the
    /// wake the engine asks for (`needs_frame_cadence`, else
    /// `next_change_deadline`) — instead of on every 120 Hz frame.
    host_wake: bool,
    /// Hand the engine the print anchor's glyph with the anchor, as
    /// `app_render.rs` does; `false` is a host that feeds only the anchor, and
    /// the engine reads the caret row's probes instead.
    anchor_glyph: bool,
    /// Hand the engine the caret whether or not DECTCEM draws it, as the
    /// single-pane window does (`app_render.rs`: a hidden cursor is still a
    /// caret, for the pet); `false` is the composed, focus and headless
    /// paths, which hand over only a drawn caret. zle never hides it.
    window_caret: bool,
}

/// What the app does in its default single-pane window.
const APP: Present = Present {
    tear: Tear::Whole,
    host_wake: false,
    anchor_glyph: true,
    window_caret: true,
};

/// Every whole-read presentation of the owner's shape.
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
    /// From when the census runs (the Ctrl-R, or a control's first key), and
    /// the caret row then — the MATCH row R of a search.
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
    /// Every row the i-search STATUS row stood on.
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
    /// `(visible caret, print anchor)` on every frame a NEW print ended on a
    /// row other than the visible caret's.
    parked_prints: Vec<((u16, u16), (u16, u16))>,
    /// Of those, the frames whose caret DECTCEM hid — handed over only by
    /// the single-pane window ([`Present::window_caret`]).
    hidden_parked_prints: usize,
    /// Every visible caret move ALONG the match row while the search ran:
    /// zle walking the caret to the match's new place.
    match_hops: Vec<((u16, u16), (u16, u16))>,
    /// The visible caret the last frame saw.
    last_cur: Option<(u16, u16)>,
    anchor_seq: Option<u64>,
    /// The wake the last frame asked for.
    wake: Option<Instant>,
    /// The cells the last presented frame inked — on glass until the next.
    on_glass: Vec<(u16, u16)>,
    /// Frames after which the engine asked for NO wake with its ink on glass.
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

    /// The host's order: `sync_cursor_effect_scroll`, LOCK A's witness rows
    /// (the caret's row, then every row the ribbon names), then
    /// `tick_cursor_fx`'s row probe with its neighbours, the print anchor
    /// with its glyph, and the tick.
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
        if self.present.anchor_glyph {
            self.glow
                .observe_print_anchor_glyph(anchor, self.term.print_anchor_glyph());
        } else {
            self.glow.observe_print_anchor(anchor);
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

    /// Where zle's fake cursor `_` stands now: the i-search STATUS row — the
    /// first row below the match row whose text holds `search:` (one row
    /// down; under a wrapped match, two) — and the `_`'s column on it.
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

    /// The hint `app_input.rs` stamps for what the keyboard sent. A printable
    /// key is a typed glyph (`note_typed_expected`); Enter is the Return
    /// licence; an arrow, ⌥←/⌥→, Home or End is navigation (the generic
    /// disarm, then `note_motion`); Backspace erases one priced glyph; Ctrl-R, Ctrl-G and
    /// Ctrl-C are the generic different-class disarm (`clear_typed`); a
    /// multi-byte `aterm ctl send` text is not typing and stamps nothing.
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
                self.glow.clear_typed(self.now);
                self.glow.note_backspace_erasing(self.now, Some(1));
            }
            b"\x1b[A" | b"\x1b[B" | b"\x1b[C" | b"\x1b[D" | b"\x1b[1;3C" | b"\x1b[1;3D"
            | b"\x1b[H" | b"\x1b[F" => {
                self.end_search();
                self.glow.clear_typed(self.now);
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

/// Fold quads into per-cell ink pixels; `scratch` is a per-pixel max buffer,
/// cleared again before returning. Only the pixels the quads touch are
/// visited.
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
            .process(format!("user@hostmachine0000 aterm % echo {i}\r\n{i}\r\n").as_bytes());
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

/// **NO INK ON THE MATCH ROW — THE OWNER'S STUB.** Every read presented
/// whole (one zle redisplay is one write is one read), at 120 Hz and on the
/// host's own wake schedule, the anchor's glyph handed over or not. From the
/// Ctrl-R on nothing is typed on row R — zle only re-underlines the match
/// there — so no cell of it may carry the ribbon's ink: neither its
/// body/rail nor its hot edge.
#[test]
fn whole_reads_lay_no_ink_on_the_match_row() {
    let mut bad = String::new();
    for take in OWNER_TAKES {
        for present in WHOLE {
            let h = replay(take, present);
            let r = h.match_row();
            let body: Vec<_> = h.lives.keys().filter(|(row, _)| *row == r).collect();
            let over: Vec<_> = h.over_lives.keys().filter(|(row, _)| *row == r).collect();
            if !body.is_empty() || !over.is_empty() {
                bad.push_str(&format!(
                    "\n{}/{present:?}: body {body:?} hot edge {over:?}\n{}",
                    take.name,
                    h.report(take.name)
                ));
            }
        }
    }
    assert!(bad.is_empty(), "ribbon ink on the match row R:{bad}");
}

/// **NO INK ON THE MATCH ROW WHEN THE MATCH HOPS.** A match that is not at
/// the history entry's first column: zle puts the caret on the newest
/// entry's rightmost occurrence, so a key walks the VISIBLE caret along row
/// R — `hop` (history `git log --stat` then `ls -la`, keys `log`: the `l`
/// hops four cells onto `ls -la`'s last `l`) and `hop2` (`echo hello world`
/// then `ls -la`, keys `lo w`: the `l` hops the same four cells, the `o`
/// four more onto `hello`'s `lo`), the latter again with the prompt on a
/// full screen's last row, so the Ctrl-R scrolls it up one. Each hop is four
/// cells on one credit and lands under a glyph its key never typed (`-`,
/// then `hello`'s `l`); nothing is typed on row R, so no cell of it may carry
/// the ribbon's ink, and each hop is refused `program-row`. The first hop's
/// press is forgotten with its refusal: kept banked, the `l` press matched
/// the `l` of `hello` under the second hop's landing, and that landing was
/// laid (RED with the forget dropped: (R, 38) under `hello`'s `l`, judged
/// `no-credits`).
/// Measured before the fix, every presentation: the `l`'s credit-starved
/// landing laid under `-` at (R, 34), `hop2`'s `o` laid under `hello`'s `l`
/// at (R, 38), and the ribbon then walked (R, 35..37) between them.
///
/// `backhop` (`echo hello world`, keys `lo`) walks the match BACK: `l` puts
/// the caret on `world`'s `l` (R, 45), `lo` retreats it six cells to
/// `hello`'s `lo` (R, 39). A same-row retreat with a press in flight is held
/// as Ink's park and judged at its own clock, where no probe is that
/// frame's; the landing gate now reads the glyph the landing held when the
/// park was held (`HeldPark::landing_cell`). RED before, every presentation:
/// the flush was `licensed key (R, 45)->(R, 39)` and laid (R, 38) under
/// `hello`'s first `l`, with ink at (R, 38..39).
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

/// **AN ENTRY SWITCH SHAPED LIKE A SOFT-WRAPPED CARET LAYS NOTHING.** zsh
/// Ctrl-R, `a` then `d` (150 ms apart, and 600 ms in `multiline-slow`):
/// `a` puts the caret on `cat a`'s `a` (R, 35); `ad` switches to an older
/// two-line entry whose first line holds `d` at that very column and whose
/// indented second line starts with the match, caret (R+1, 2). That is
/// exactly Claude Code 2.1.280's soft-wrapped caret — the landing row blank
/// up to the landing, the origin changed to the fresh key's glyph — and the
/// soft wrap moves the landing gate's witness to the origin, where `d`
/// passed it. What tells them apart is the origin row left of the origin:
/// the composer's key goes in AT the fold and leaves it as it was; zle
/// rewrote `cat ` as `cd /`. RED before `soft_wrapped_caret` read that
/// prefix: `licensed key (R, 35)->(R+1, 2)`, a cell at (R, 35) under the
/// `d` of `cd /d` and body ink at (R, 35) and (R+1, 35), in every
/// presentation of both takes.
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

/// **BASH'S REVERSE-I-SEARCH RETREAT LAYS NOTHING UNDER THE MATCH.** bash
/// 3.2, history `echo hello world`, Ctrl-R, `lo w`: `l` hops the caret onto
/// `world`'s `l` (R, 37); `lo` retreats it to `hello`'s `lo` (R, 32) — the
/// same held-park retreat as zsh's `backhop`, its landing cell `hello`'s
/// first `l`. It is refused `program-row` and (R, 31) never gets a cell.
/// RED before the flushed park's witness: `licensed key (R, 37)->(R, 32)`,
/// its landing laid at (R, 31). Not pinned here: each later query key inserts
/// itself inside `(reverse-i-search)` and shifts the line one cell right
/// (`ESC[1@`), a typed `+1` whose cell is the match's own text — the
/// ordinary one-cell echo, which no witness reads.
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

/// **NO BAND UNDER zle'S FAKE CURSOR — THE OWNER'S SLAB.** The same replays:
/// no ribbon cell on the status row under anything but the search's typed
/// glyphs, none under or right of the `_`, and no band ink at or right of
/// the `_` on any frame.
#[test]
fn the_status_row_carries_no_band_under_zle_s_fake_cursor() {
    let mut bad = String::new();
    for take in OWNER_TAKES {
        for present in WHOLE {
            let h = replay(take, present);
            let cells = untyped_status_cells(&h);
            if !cells.is_empty() || !h.beyond.is_empty() {
                let cols: BTreeSet<u16> = h.beyond.iter().map(|b| b.1).collect();
                bad.push_str(&format!(
                    "\n{}/{present:?}: cells {cells:?}; ink at/right of `_` on {} frames, cols {cols:?}",
                    take.name,
                    h.beyond.len()
                ));
            }
        }
    }
    assert!(bad.is_empty(), "band on the minibuffer:{bad}");
}

/// **THE MINIBUFFER'S ECHOES ARE REFUSED, NOT LAID.** The mechanism, on the
/// owner's take as the app presents it: each of the four keys' status-row
/// echoes reaches the anchored lane (four verdicts naming the status row) and
/// every one is the `program-row` refusal — no `licensed` row lays a cell
/// there, and the failing `d` leaves no `no-credits` landing behind.
#[test]
fn the_minibuffer_echoes_are_refused_program_row() {
    let h = replay(G150, APP);
    let status = h.match_row() + 1;
    assert_eq!(h.status_rows, BTreeSet::from([status]));
    let end = h.search_end_us.expect("Ctrl-G ends the search");
    let there: Vec<Verdict> = h
        .ring
        .iter()
        .filter(|v| v.target.0 == status && v.us < end)
        .copied()
        .collect();
    assert_eq!(
        there.len(),
        4,
        "one verdict per minibuffer echo:{}",
        h.report("g150")
    );
    assert!(
        there
            .iter()
            .all(|v| v.reason == CursorGlow::DECLINE_PROGRAM_ROW
                && v.licence == AdmissionRecord::LICENCE_NONE),
        "every minibuffer echo is refused `program-row`: {there:?}"
    );
    assert!(
        h.status_cells.is_empty(),
        "nothing laid on the status row: {:?}",
        h.status_cells
    );
}

/// **THE SHAPE WITH THE FEWEST BYTES.** A visible caret parked after a
/// 3-column prompt that never moves; per key the program drops a row, prints
/// `<glyph>_` on the status row and climbs back — zle's i-search refresh
/// without the match text. Nothing is laid under the fake cursor, whether the
/// glyph comes from the host or from the caret row's probes.
#[test]
fn a_minibuffer_echo_below_a_stationary_caret_lays_nothing_under_its_fake_cursor() {
    for anchor_glyph in [true, false] {
        let mut h = Host::new(
            24,
            100,
            Present {
                anchor_glyph,
                ..APP
            },
        );
        h.feed(b"\x1b[5;1Hp% ");
        let r = h.term.cursor().row;
        // Ctrl-R: the status row appears, the caret returns.
        h.hint(&[0x12]);
        h.feed(b"\r\r\nsearch: _\x1b[K\x1b[A\x1b[6D");
        for (i, k) in "abc".chars().enumerate() {
            h.idle_ms(120);
            h.hint(&[k as u8]);
            // Down to (r+1, 3), right onto the fake cursor, `k_`, up, back.
            h.feed(format!("\x1b[1B\x1b[{}C{k}_\x1b[A\x1b[{}D", 5 + i, 7 + i).as_bytes());
            assert_eq!(
                (h.term.cursor().row, h.term.cursor().col),
                (r, 3),
                "the caret is back after the prompt"
            );
        }
        h.idle_ms(1_200);
        assert_eq!(h.text(r + 1), "search: abc_");
        assert_eq!(
            h.parked_prints.len(),
            4,
            "non-vacuity: every echo ended off the caret's row"
        );
        assert_eq!(
            untyped_status_cells(&h),
            vec![],
            "anchor glyph {anchor_glyph}:\n{}",
            h.report("minimal")
        );
    }
}

/// **THE SAME LAW OVER A LONGER SEARCH.** The search carried on past the
/// failing `d`: Backspace, `ude` to match again, Right to leave the search,
/// then ` x` typed on the prompt row. While the search runs, the status row
/// carries no band under a glyph nobody typed and the prompt row carries
/// none at all; after it, the prompt row carries only the Right hop's own
/// cells and the keys typed there (a Right over text lays its hop at any
/// prompt) — never the prompt's text.
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
}

/// **MUST NOT BREAK — TYPING AFTER THE SEARCH.** Once the search is left
/// (Right) the hand types ` x` on the prompt row, and each of those two keys
/// lights its OWN cell — the ribbon cell under its own glyph, born within
/// 100 ms of its own key: the fix darkens the minibuffer, never the prompt
/// row's own typing after it. (Keyed on the glyph and the key's clock: the
/// Right's hop has already laid a cell at the space's column, under the
/// `l` that stood there.)
#[test]
fn typing_on_the_prompt_row_after_leaving_the_search_still_lights_its_cells() {
    let h = replay(CONTINUE, APP);
    let r = h.match_row();
    // Right left the caret one past the match's `c`; ` x` went in there.
    let (space, x) = (PROMPT_COLS + 1, PROMPT_COLS + 2);
    assert_eq!(h.glyph(r, space), ' ');
    assert_eq!(h.glyph(r, x), 'x');
    let after = h.search_end_us.expect("Right leaves the search");
    let keys: Vec<(u64, char)> = h.keys.iter().copied().filter(|k| k.0 > after).collect();
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

/// **THE REFUSED PRESSES ARE FORGOTTEN.** Each refused minibuffer echo drops
/// the presses it was the echo of (the in-flight law): `u`, `d` and `e`
/// echoed on the status row, so nothing typed during the search may pay for
/// a move after it. The Right that leaves the search is licensed by its own
/// key, and no verdict from the Ctrl-R on is `licence=inflight`. RED with the
/// refusal kept but the forget dropped: the three presses stay banked and
/// the Right's hop reads `licensed inflight (10, 31)->(10, 32)`, paid by
/// keys that echoed elsewhere.
#[test]
fn the_refused_search_keys_pay_for_nothing_after_the_search() {
    let h = replay(CONTINUE, APP);
    let r = h.match_row();
    let from = h.census_from.expect("the take presses Ctrl-R").0;
    let end = h.search_end_us.expect("Right leaves the search");
    let right = h
        .ring
        .iter()
        .find(|v| v.us >= end && v.origin == (r, PROMPT_COLS) && v.target == (r, PROMPT_COLS + 1))
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
}

/// **THE WRAPPED MATCH.** An 85-character history entry after the 31-column
/// prompt wraps, so zle draws its status row under the match's LAST line.
/// The first key's match is the entry's last `g` (of `bug`), on that last
/// line, one row above the status row; from `gi` on the caret parks on the
/// match's first glyph, two rows above it — out of the caret row probe's
/// reach. Handed the anchor's glyph as the app hands it, the lane lays
/// nothing under the fake cursor there either.
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

/// **THE GATES' BLAST RADIUS — THE COMPOSERS.** Claude Code's and Codex's
/// composer takes fed as the app feeds them — each key banked with its
/// glyph, the anchor handed over with its glyph, the caret as the
/// single-pane window hands it over and as the composed paths do — print
/// off the caret's row again and again (the parked arm the minibuffer gate
/// sits on), and neither gate refuses anything there. Both gates log every
/// refusal `program-row`, and these takes have no other `program-row`
/// verdict, so none at all means neither fired and every cell these takes
/// lit is lit as before the gates.
#[test]
fn the_gates_refuse_nothing_in_claude_code_s_or_codex_s_composer() {
    let mut bad = String::new();
    let mut parked = 0usize;
    for take in &COMPOSERS {
        for window_caret in [true, false] {
            let h = replay_composer(
                take,
                Present {
                    window_caret,
                    ..APP
                },
            );
            assert!(h.keys.len() >= 5, "non-vacuity: {} types", take.name);
            let licensed = h
                .ring
                .iter()
                .filter(|v| v.licence != AdmissionRecord::LICENCE_NONE)
                .count();
            assert!(licensed >= 5, "non-vacuity: {} lights its keys", take.name);
            parked += h.parked_prints.len();
            let refused: Vec<Verdict> = h
                .ring
                .iter()
                .filter(|v| v.reason == CursorGlow::DECLINE_PROGRAM_ROW)
                .copied()
                .collect();
            if !refused.is_empty() {
                bad.push_str(&format!(
                    "\n{} (window caret {window_caret}): {refused:?}",
                    take.name
                ));
            }
        }
    }
    assert!(
        parked > 0,
        "non-vacuity: the composers print off the caret's row"
    );
    assert!(bad.is_empty(), "a gate refused a composer echo:{bad}");
}

/// **A HIDDEN CARET KEEPS ITS LANE ON EVERY PATH.** A TUI that hides the
/// caret, parks it on the row below its input and draws its OWN fake cursor
/// after each echoed key (`> a█`, `> ab█`, …): every print run ends on the
/// fake cursor, a glyph no key typed. The composed, focus and headless
/// paths hand over no caret while DECTCEM hides it; the single-pane window
/// hands it over (a hidden cursor is still a caret, for the pet) and says it
/// is hidden (`observe_caret_drawn`), so the minibuffer gate — a law about a
/// VISIBLE caret — stays out of it on both: the same cells, and no
/// `program-row` refusal. RED with the gate reading the window's hidden
/// caret as parked: the window path refuses all three echoes and lays
/// nothing.
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

/// **THE CONTROL — PLAIN TYPING.** The same four keys typed at the plain
/// prompt light their four cells and are gone by the idle ceiling: the
/// normal schedule, unchanged.
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

/// **A PARKED CARET'S OWN ECHO STILL LIGHTS.** The negative control for the
/// gate: the same stationary visible caret, but the program's run on the row
/// below ends on the KEY — the ordinary parked-caret echo the lane exists
/// for — so each key's cell is laid under its own glyph.
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

/// **TYPEAHEAD INTO THE MINIBUFFER.** Two keys pressed before the first
/// one's echo is presented: both echoes end on `_`, foreign to both live
/// presses, so the first is refused by the glyph gate, and the second finds
/// no credit (the first refusal forgot both presses; a stamp with no credit
/// behind it licenses no light) — nothing is laid under the fake cursor.
/// This law holds with or without that forget (the gate alone refuses both
/// echoes); `the_refused_search_keys_pay_for_nothing_after_the_search` is
/// the one that pins it.
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

/// **A GLYPH-LESS PRESS KEEPS THE LANE.** The host banks a wide glyph (and an
/// IME commit) by its cell count alone, so the gate cannot judge it: a CJK
/// key echoed on the row below a parked visible caret is laid exactly as
/// before, though the run ends on the glyph's continuation column.
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

/// **LATENT, NOT THE OWNER'S SHAPE: A PRESENT INSIDE ONE zle WRITE.** A
/// present after every piece of every redisplay: the caret visits every
/// intermediate cell, the match's re-underlined glyphs among them, and the
/// visible lane lays cells on row R. Every local take delivered each
/// redisplay as one read; a redisplay larger than the tty's output chunk (a
/// long wrapped match) or a remote zsh over ssh could arrive split, and then
/// this is what the glass would show.
#[test]
#[ignore = "latent: needs a read boundary inside one zle redisplay; local whole reads (spawn.rs, one term-lock hold per read under 8 KiB) never present one"]
fn torn_reads_lay_no_band_on_the_match_row() {
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
            let r = h.match_row();
            let body: Vec<_> = h.lives.keys().filter(|(row, _)| *row == r).collect();
            if !body.is_empty() {
                bad.push_str(&format!("\n{}/{host_wake}: {body:?}", take.name));
            }
        }
    }
    assert!(bad.is_empty(), "band body on the match row R:{bad}");
}

/// **LATENT: ONE TORN READ, EVERY BOUNDARY.** Each of the search's output
/// reads torn at each of its piece boundaries, one at a time, everything
/// else whole: the match row's band body stays dark.
#[test]
#[ignore = "latent: needs a read boundary inside one zle redisplay; local whole reads (spawn.rs, one term-lock hold per read under 8 KiB) never present one"]
fn one_torn_read_at_any_boundary_lays_no_band_on_the_match_row() {
    let mut bad = String::new();
    let mut tried = 0usize;
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
                let r = h.match_row();
                let cols: Vec<u16> = h
                    .lives
                    .keys()
                    .filter(|(row, _)| *row == r)
                    .map(|(_, c)| *c)
                    .collect();
                if !cols.is_empty() {
                    bad.push_str(&format!(
                        "\n{} read {n} torn after piece {k}: match-row cols {cols:?}",
                        take.name
                    ));
                }
            }
        }
    }
    assert!(tried > 100, "the sweep tried {tried} tears");
    assert!(
        bad.is_empty(),
        "a single torn read laid band on the match row:{bad}"
    );
}

/// **NOTHING PERSISTS.** Whatever the search's replays lit — whole, torn at
/// every boundary, or on the host's wake schedule — retires on the normal
/// clock: no cell is inked `IDLE_CEILING_MS` after the last key, and no
/// replay's engine goes to sleep with ink on glass.
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
