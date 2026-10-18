// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Torn reads on real bytes, through the host seam: a real `aterm_core` [`Terminal`] fed the
//! bytes, its rows sampled as `app_render.rs`'s frame hold samples them (the caret's row, every
//! row the ribbon names, the print anchor), its content-scroll clock read as
//! `sync_cursor_effect_scroll` reads it, fed to [`CursorGlow`] with the hints `app_input.rs`
//! stamps for each key.
//!
//! * Real bytes at every read size: the Claude Code composer takes and Codex's particle take,
//!   each burst presented whole, then re-cut into 1024- and 512-byte reads with a present after
//!   each. No interior dark run on the composer row from the first typed key to the last, and
//!   nothing drawn once the take has rested past the longest swoosh. Claude takes run with the
//!   `?2026` hold lapsed (past `SYNC_HOLD_CAP`, 150 ms), the torn-read condition; Codex's with
//!   the hold held (SYNC-1).
//! * A typed line into the swoosh: 12 cps into an Ink-shaped composer, every key's repaint torn
//!   at the same text column, then a rest through the whole exit. Neither the plan's coverage
//!   (what a pixel census reads) nor the ribbon's live cells (whose one-cell gaps the plan
//!   feathers over) may show a gap.

use std::time::{Duration, Instant};

use crate::common::{
    LIT_COV, Rec, caret_sample, class_of, coverage, geom, holes_of, is_key, note_blink,
    observe_caret_row, observe_ribbon_rows, rainbow_cfg, sync_scroll, unhex,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::ribbon::Ribbon;

// Shared.

/// Idle after the last gesture before T6 is read, as in
/// `rainbow_kitty_v2_frame_cost.rs`: longer than the longest swoosh (a 1.5 s
/// rest plus 0.79 s at the slowest tempo).
const IDLE_CEILING_MS: u64 = 2_500;

/// The line the swoosh test types.
const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN!!!!";

fn lit_map(cov: &[u8]) -> String {
    cov.iter()
        .map(|&v| if v >= LIT_COV { '#' } else { '.' })
        .collect()
}

/// Interior dark runs of one row as `(first col, len)`.
type Holes = Vec<(usize, usize)>;
/// One frame with a hole: the instant in ms, the lit map, the holes.
type HoleFrame = (u64, String, Holes);
/// The same for a composer row: the instant, the row, the take's label, the
/// lit map, the holes.
type RowHoleFrame = (u64, u16, String, String, Holes);

/// The columns no live cell owns between two that do — the ribbon's own
/// account of its row, which the plan's feathering hides.
fn live_gaps(rib: &Ribbon, row: u16) -> Vec<u16> {
    let mut live: Vec<u16> = rib
        .cells()
        .iter()
        .filter(|c| c.row == row && !c.leaving())
        .map(|c| c.col)
        .collect();
    live.sort_unstable();
    live.dedup();
    live.windows(2)
        .filter(|w| w[1] > w[0] + 1)
        .flat_map(|w| (w[0] + 1)..w[1])
        .collect()
}

/// What T6 (idle → zero) reads once the take has rested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rest {
    cells: u32,
    stars: u32,
    meteors: u32,
    lit_segments: usize,
    fp: u64,
    cadence: bool,
    deadline: bool,
}

impl Rest {
    const ZERO: Rest = Rest {
        cells: 0,
        stars: 0,
        meteors: 0,
        lit_segments: 0,
        fp: 0,
        cadence: false,
        deadline: false,
    };
}

fn rest_of(glow: &CursorGlow, fp: u64, now: Instant) -> Rest {
    let st = glow.v2_status().unwrap_or_default();
    Rest {
        cells: st.cells,
        stars: st.stars,
        meteors: st.meteors,
        lit_segments: glow
            .v2_ribbon()
            .map_or(0, |r| r.lit_segments().filter(|s| s.cov > 0).count()),
        fp,
        cadence: glow.needs_frame_cadence(),
        deadline: glow
            .next_change_deadline(now, Duration::from_millis(8))
            .is_some(),
    }
}

// 1. Real bytes at every read size.

struct Take {
    name: &'static str,
    src: &'static str,
    rows: usize,
    cols: usize,
    cw: usize,
    ch: usize,
    /// SYNC-1: withhold the present while a `?2026` bracket is open with
    /// content in it, as the host does up to its 150 ms cap.
    hold: bool,
}

/// The Claude Code takes (`resize 40 120`, 7×14 px cells, recorded under a PTY
/// wrapper in a headless aterm: a line at 12 cps, ⌥← and `INTO ` inserted,
/// ⌥→ / End back, three hops each way, an `@`-mention popup), Codex's particle
/// take (`resize 32 100`, 7×14 px: a line, ⌥← ×3, a word typed inside it,
/// ⌥→ ×3, two letters), and the plain-arrow insert take (`resize 40 120`, under
/// a bare PTY wrapper against Claude Code 2.1.282: a line at 12 cps, ← ×12 at
/// 150 ms, `NEW words ` typed inside it, ← ×6 at 60 ms, `more`).
const TAKES: [Take; 6] = [
    Take {
        name: "claude-insert",
        src: include_str!("fixtures/claude-composer-2026-09-21.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        hold: false,
    },
    Take {
        name: "claude-end",
        src: include_str!("fixtures/claude-composer-2026-09-21-end.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        hold: false,
    },
    Take {
        name: "claude-scrub3",
        src: include_str!("fixtures/claude-composer-2026-09-21-scrub3.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        hold: false,
    },
    Take {
        name: "claude-popup",
        src: include_str!("fixtures/claude-composer-2026-09-21-popup.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        hold: false,
    },
    Take {
        name: "codex-particles",
        src: include_str!("fixtures/codex-particles-2026-09-16.ptylog"),
        rows: 32,
        cols: 100,
        cw: 7,
        ch: 14,
        hold: true,
    },
    Take {
        name: "claude-arrow-insert",
        src: include_str!("fixtures/claude-composer-2026-09-24-left.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        hold: false,
    },
];

/// How a program burst reaches the frame hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chunking {
    /// The burst in one read, one present.
    Whole,
    /// The burst in reads of this many bytes, a present after each.
    Reads(usize),
}

/// Consecutive program writes this close together, with no key between, are
/// one burst: one frame as the program wrote it.
const BURST_GAP_MS: u64 = 2;
/// The frame train between events.
const REPLAY_FRAME_MS: u64 = 8;

/// `(ms from the first line, event)`. The Claude takes are stamped in ns,
/// Codex's in ms.
fn recording(src: &str) -> Vec<(u64, Rec)> {
    let mut t0 = None;
    src.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, ' ');
            let stamp: u64 = it.next().expect("stamp").parse().expect("stamp");
            let kind = it.next().expect("kind");
            let bytes = unhex(it.next().expect("bytes"));
            let t0 = *t0.get_or_insert(stamp);
            let ms = if t0 > 1_000_000_000_000 {
                (stamp - t0) / 1_000_000
            } else {
                stamp - t0
            };
            match kind {
                "O" => (ms, Rec::Out(bytes)),
                "I" => (ms, Rec::In(bytes)),
                k => panic!("bad kind {k}"),
            }
        })
        .collect()
}

/// Program output coalesced into bursts, the keys kept where they fell.
fn bursts(rec: Vec<(u64, Rec)>) -> Vec<(u64, Rec)> {
    let mut out: Vec<(u64, Rec)> = Vec::new();
    let mut last_out_ms = None;
    for (ms, ev) in rec {
        match ev {
            Rec::Out(bytes) => {
                if let (Some((_, Rec::Out(buf))), Some(prev)) = (out.last_mut(), last_out_ms)
                    && ms.saturating_sub(prev) <= BURST_GAP_MS
                {
                    buf.extend_from_slice(&bytes);
                } else {
                    out.push((ms, Rec::Out(bytes)));
                }
                last_out_ms = Some(ms);
            }
            Rec::In(bytes) => {
                out.push((ms, Rec::In(bytes)));
                last_out_ms = None;
            }
        }
    }
    out
}

fn is_exit(b: &[u8]) -> bool {
    b == b"\x1b[99;5u" || b == b"\x03"
}

/// A keyboard byte string the app stamps a navigation hint for.
fn is_nav(b: &[u8]) -> bool {
    matches!(
        b,
        b"\x1b[1;3D"
            | b"\x1b[1;3C"
            | b"\x1b[F"
            | b"\x1b[H"
            | b"\x1b[A"
            | b"\x1b[B"
            | b"\x1b[C"
            | b"\x1b[D"
    )
}

struct Replay {
    frames: usize,
    /// Presents taken while a `?2026` bracket was open with content in it.
    torn_presents: usize,
    /// Frames with an interior dark run on the composer row while typing:
    /// `(ms, lit map, holes)`.
    holes: Vec<HoleFrame>,
    /// The widest lit run the composer row carried while typing.
    lit_max: usize,
    composer_row: u16,
    rest: Rest,
    /// Ms after the last gesture at which the ribbon planned nothing lit.
    dark_after_ms: Option<u64>,
    /// The composer row at the take's last census frame, while the hand is
    /// still typing: `(ms, text, lit map)`.
    last_row: Option<(u64, String, String)>,
}

struct ReplayHost {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    hold: bool,
    t0: Instant,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    scroll: Option<ContentScrollState>,
    fp: u64,
    frames: usize,
    torn_presents: usize,
}

impl ReplayHost {
    fn new(take: &Take) -> Self {
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, take.cols);
        Self {
            term: Terminal::new(take.rows as u16, take.cols as u16),
            glow,
            cfg: rainbow_cfg(),
            g: geom(take.rows, take.cols, take.cw, take.ch),
            hold: take.hold,
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            scroll: None,
            fp: 0,
            frames: 0,
            torn_presents: 0,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// The host's own order: `sync_cursor_effect_scroll`, the frame hold's samples
    /// (the caret's row, every ribbon row, the print anchor), the tick.
    /// Returns false when SYNC-1 withheld the present.
    fn frame(&mut self) -> bool {
        if self.term.sync_open_dirty() {
            if self.hold {
                return false;
            }
            self.torn_presents += 1;
        }
        sync_scroll(&self.term, &mut self.glow, &mut self.scroll, self.now);
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .observe_print_anchor(aterm_effects::host::motion_print_anchor(&self.term).0);
        self.fp = self
            .glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        self.frames += 1;
        true
    }

    fn hint(&mut self, b: &[u8]) {
        if is_key(b) {
            let (shifted, class) = class_of(b[0] as char);
            self.glow.note_typed_glyph(self.now, 1, shifted, class);
        } else if b == [0x7f] {
            self.glow.note_backspace_erasing(self.now, Some(1));
        } else if is_nav(b) {
            self.glow.note_motion(self.now);
        }
    }
}

fn replay(take: &Take, chunking: Chunking) -> Replay {
    let rec = bursts(recording(take.src));
    let first_key = rec
        .iter()
        .find(|(_, e)| matches!(e, Rec::In(b) if is_key(b)))
        .map(|(ms, _)| *ms)
        .expect("a typed key");
    let exit = rec
        .iter()
        .find(|(ms, e)| *ms > first_key && matches!(e, Rec::In(b) if is_exit(b)))
        .map_or(u64::MAX, |(ms, _)| *ms);
    let last_key = rec
        .iter()
        .rev()
        .find(|(ms, e)| *ms < exit && matches!(e, Rec::In(b) if is_key(b)))
        .map(|(ms, _)| *ms)
        .expect("a last key");
    let last_gesture = rec
        .iter()
        .rev()
        .find(|(ms, e)| {
            *ms < exit && matches!(e, Rec::In(b) if is_key(b) || is_nav(b) || b == &[0x7f])
        })
        .map(|(ms, _)| *ms)
        .expect("a last gesture");
    let mut h = ReplayHost::new(take);
    let mut composer_row = None;
    let mut holes = Vec::new();
    let mut lit_max = 0usize;
    let mut last_row = None;
    let census = |h: &ReplayHost,
                  row: Option<u16>,
                  holes: &mut Vec<HoleFrame>,
                  lit_max: &mut usize,
                  last_row: &mut Option<(u64, String, String)>| {
        let Some(row) = row else { return };
        let ms = h.ms();
        if ms < first_key || ms > last_key {
            return;
        }
        let cov = coverage(&h.glow, h.g, row);
        *lit_max = (*lit_max).max(cov.iter().filter(|&&v| v >= LIT_COV).count());
        let found = holes_of(&cov);
        if !found.is_empty() {
            holes.push((ms, lit_map(&cov), found));
        }
        let mut cols = Vec::new();
        h.term.row_cols_into(usize::from(row), &mut cols);
        let text = cols
            .iter()
            .map(|&c| if c == '\0' { ' ' } else { c })
            .collect();
        *last_row = Some((ms, text, lit_map(&cov)));
    };
    for (ms, ev) in rec {
        if ms >= exit {
            break;
        }
        let t = h.t0 + Duration::from_millis(ms);
        while h.now + Duration::from_millis(REPLAY_FRAME_MS) <= t {
            h.now += Duration::from_millis(REPLAY_FRAME_MS);
            if h.frame() {
                census(&h, composer_row, &mut holes, &mut lit_max, &mut last_row);
            }
        }
        h.now = h.now.max(t);
        match ev {
            Rec::In(b) => {
                if composer_row.is_none() && is_key(&b) {
                    composer_row = Some(h.term.cursor().row);
                }
                h.hint(&b);
            }
            Rec::Out(bytes) => {
                let step = match chunking {
                    Chunking::Whole => bytes.len().max(1),
                    Chunking::Reads(n) => n,
                };
                let mut chunks = bytes.chunks(step).peekable();
                while let Some(chunk) = chunks.next() {
                    h.term.process(chunk);
                    if h.frame() {
                        census(&h, composer_row, &mut holes, &mut lit_max, &mut last_row);
                    }
                    if chunks.peek().is_some() {
                        h.now += Duration::from_millis(1);
                    }
                }
            }
        }
    }
    // The take has ended: rest past the longest swoosh.
    let dark_from = h.t0 + Duration::from_millis(last_gesture);
    let end = dark_from + Duration::from_millis(IDLE_CEILING_MS);
    let mut dark_after_ms = None;
    while h.now + Duration::from_millis(REPLAY_FRAME_MS) <= end {
        h.now += Duration::from_millis(REPLAY_FRAME_MS);
        h.frame();
        let lit = h
            .glow
            .v2_ribbon()
            .map_or(0, |r| r.lit_segments().filter(|s| s.cov > 0).count());
        if lit == 0 && dark_after_ms.is_none() {
            dark_after_ms = Some(h.now.saturating_duration_since(dark_from).as_millis() as u64);
        } else if lit > 0 {
            dark_after_ms = None;
        }
    }
    h.now = h.now.max(end);
    h.frame();
    Replay {
        frames: h.frames,
        torn_presents: h.torn_presents,
        holes,
        lit_max,
        composer_row: composer_row.unwrap_or(u16::MAX),
        rest: rest_of(&h.glow, h.fp, h.now),
        dark_after_ms,
        last_row,
    }
}

fn assert_take_holds(take: &Take) {
    let mut report = String::new();
    let mut bad = Vec::new();
    for chunking in [Chunking::Whole, Chunking::Reads(1024), Chunking::Reads(512)] {
        let r = replay(take, chunking);
        report.push_str(&format!(
            "\n  {}/{chunking:?}: row {} frames {} torn presents {} widest {} hole frames {} \
             dark +{:?} ms rest {:?}",
            take.name,
            r.composer_row,
            r.frames,
            r.torn_presents,
            r.lit_max,
            r.holes.len(),
            r.dark_after_ms,
            r.rest
        ));
        for (ms, map, found) in r.holes.iter().take(4) {
            report.push_str(&format!("\n      t={ms:6} {found:?}\n      {map}"));
        }
        if r.frames < 500 || r.lit_max < 9 || !r.holes.is_empty() || r.rest != Rest::ZERO {
            bad.push(chunking);
        }
    }
    assert!(
        bad.is_empty(),
        "{}: the band broke, or never lit, or light was left at rest under {bad:?}:{report}",
        take.name
    );
}

/// Every take at every read size, each presented whole and in 1024- and
/// 512-byte reads: no interior hole while typing, nothing drawn at rest.
///
/// * Claude Code's bytes: the insert take (the line typed, ⌥← into it,
///   `INTO ` inserted, ⌥→ back); the End take (the insert, then End back to
///   the end of the line); the scrub take (three word hops each way inside
///   the typed line); the popup take (an `@`-mention popup open under the
///   composer while typing); the plain-arrow insert.
/// * Codex's bytes: braille particles painted into the composer's blank cells
///   on every ~150 ms frame inside `?2026`, with SYNC-1 holding the present
///   while a bracket is open. The band under the typed line, the backward
///   edit and the forward hops stays one run.
#[test]
fn every_take_holds_whole_and_torn_at_every_read_size() {
    crate::common::par_map(&TAKES, assert_take_holds);
}

/// The plain-arrow insert take: ← (one `CUP` per press) back into the line and
/// typing there, each key's diff frame rewriting the rest of the line one
/// column on. On the last typing frame the band is lit from the prompt to the
/// end of the text: a dark pushed tail is not an interior hole, so the hole
/// census alone cannot see it.
#[test]
fn claude_code_s_arrow_insert_keeps_the_pushed_tail_lit_at_every_read_size() {
    let take = &TAKES[5];
    for chunking in [Chunking::Whole, Chunking::Reads(1024), Chunking::Reads(512)] {
        let r = replay(take, chunking);
        let (ms, text, lit) = r.last_row.expect("the composer row was censused");
        let text = text.trim_end();
        let first = text.find("hello").expect("the typed line is on the row");
        let first = text[..first].chars().count();
        let end = text.chars().count();
        let dark: Vec<usize> = (first..end)
            .filter(|&c| lit.chars().nth(c) != Some('#'))
            .collect();
        assert!(
            dark.is_empty(),
            "{}/{chunking:?} t={ms}: the pushed tail went dark at {dark:?}\n  txt |{text}|\n  lit |{}|",
            take.name,
            lit.trim_end_matches('.')
        );
    }
}

// 2. A typed line, torn, into the swoosh.

const COMPOSER_ROWS: usize = 40;
const COMPOSER_CW: usize = 8;
const COMPOSER_CH: usize = 16;
/// The composer's first text row (0-based); it has three, as a wrapped Ink
/// box does.
const R0: usize = 30;
const TEXT_ROWS: usize = 3;
const SPIN_ROW: usize = R0 - 2;
const HINT_ROW: usize = R0 + TEXT_ROWS + 1;
const FRAME_MS: u64 = 16;
/// 12 cps.
const KEY_MS: u64 = 83;

/// Where a present lands inside one repaint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
    Whole,
    /// One present right after text index `j` was written: the rest of that
    /// row still blank from its `CSI 2K`, the rows under it still the last
    /// frame's, the caret hidden — a PTY read boundary.
    After(usize),
}

/// A Claude-Code-shaped composer: a spinner row, three `CSI 2K`-cleared text
/// rows, a hint row, all inside `?2026h` / `?25l`, the caret parked last.
struct Composer {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    cols: usize,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    text: String,
    caret: usize,
    spin_i: usize,
    label: String,
    frames: usize,
    /// `(ms, row, label, lit map, holes)` per frame with an interior dark run
    /// in the plan.
    holes: Vec<RowHoleFrame>,
    /// `(ms, row, columns)` per frame with an interior gap in the live cells.
    gaps: Vec<(u64, u16, Vec<u16>)>,
    rows_lit_max: usize,
}

impl Composer {
    fn new(cols: usize) -> Self {
        let mut term = Terminal::new(COMPOSER_ROWS as u16, cols as u16);
        term.process(b"\x1b[?1049h\x1b[2J");
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, cols);
        let mut h = Self {
            term,
            glow,
            cfg: rainbow_cfg(),
            g: geom(COMPOSER_ROWS, cols, COMPOSER_CW, COMPOSER_CH),
            cols,
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            text: String::new(),
            caret: 0,
            spin_i: 0,
            label: "start".into(),
            frames: 0,
            holes: Vec::new(),
            gaps: Vec::new(),
            rows_lit_max: 0,
        };
        h.repaint(Cut::Whole);
        h
    }

    /// Text cells per row (`> ` before it, two spare after).
    fn width(&self) -> usize {
        self.cols - 4
    }

    fn at_index(&self, i: usize) -> (u16, u16) {
        let w = self.width();
        ((R0 + i / w) as u16, (2 + i % w) as u16)
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// The frame hold and the tick, then the census.
    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        let now = self.now;
        note_blink(&self.term, &mut self.glow, &mut self.blink, now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        let (anchor, _) = aterm_effects::host::motion_print_anchor(&self.term);
        self.glow.observe_print_anchor(anchor);
        self.glow.tick(cur, now, &self.cfg, self.g, &mut self.out);
        self.frames += 1;
        self.census();
    }

    fn census(&mut self) {
        let ms = self.ms();
        let mut lit_rows = 0;
        for r in R0..R0 + TEXT_ROWS {
            let row = r as u16;
            let cov = coverage(&self.glow, self.g, row);
            if cov.iter().any(|&v| v >= LIT_COV) {
                lit_rows += 1;
            }
            let found = holes_of(&cov);
            if !found.is_empty() {
                let label = self.label.clone();
                self.holes.push((ms, row, label, lit_map(&cov), found));
            }
            let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
            let gaps = live_gaps(rib, row);
            if !gaps.is_empty() {
                self.gaps.push((ms, row, gaps));
            }
        }
        self.rows_lit_max = self.rows_lit_max.max(lit_rows);
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

    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = self.now.max(t);
        self.last_event = self.now;
    }

    fn rest(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.idle_to(t);
        self.last_event = self.now;
    }

    /// The frame's bytes and, per text index, the offset just past its
    /// glyph.
    fn frame_bytes(&self) -> (Vec<u8>, Vec<usize>) {
        let w = self.width();
        let mut s = String::from("\x1b[?2026h\x1b[?25l");
        s.push_str(&format!(
            "\x1b[{};1H\x1b[2K✻ Clauding… ({}s · ↓ 2.3k tokens)",
            SPIN_ROW + 1,
            40 + self.spin_i / 10
        ));
        let mut ends = Vec::with_capacity(self.text.len());
        let chars: Vec<char> = self.text.chars().collect();
        for r in 0..TEXT_ROWS {
            s.push_str(&format!("\x1b[{};1H\x1b[2K", R0 + r + 1));
            s.push_str(if r == 0 { "> " } else { "  " });
            for &ch in chars.iter().skip(r * w).take(w) {
                s.push(ch);
                ends.push(s.len());
            }
        }
        s.push_str(&format!("\x1b[{};1H\x1b[2K  ? for shortcuts", HINT_ROW + 1));
        let (row, col) = self.at_index(self.caret);
        s.push_str(&format!(
            "\x1b[{};{}H\x1b[?25h\x1b[?2026l",
            row + 1,
            col + 1
        ));
        (s.into_bytes(), ends)
    }

    fn repaint(&mut self, cut: Cut) {
        let (bytes, ends) = self.frame_bytes();
        let mut start = 0;
        if let Cut::After(j) = cut
            && let Some(&c) = ends.get(j)
        {
            self.term.process(&bytes[..c]);
            self.frame();
            self.now += Duration::from_millis(1);
            start = c;
        }
        self.term.process(&bytes[start..]);
        self.frame();
    }

    fn key(&mut self, ch: char, ms: u64, cut: Cut) {
        self.schedule(ms);
        self.label = format!("key {ch:?}");
        let (shifted, class) = class_of(ch);
        let now = self.now;
        self.glow.note_typed_glyph(now, 1, shifted, class);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.spin_i += 1;
        self.repaint(cut);
    }

    fn type_line(&mut self, text: &str, cut: impl Fn(usize) -> Cut) {
        for (k, ch) in text.chars().enumerate() {
            self.key(ch, KEY_MS, cut(k));
        }
    }

    fn story(&self, what: &str) -> String {
        let mut s = format!(
            "{what}: {} frames, {} with an interior dark run in the plan, {} with a gap in the \
             live cells",
            self.frames,
            self.holes.len(),
            self.gaps.len()
        );
        for (ms, row, label, map, found) in self.holes.iter().take(3) {
            s.push_str(&format!(
                "\n  t={ms:6} row {row} {label:12} {found:?}\n    {map}"
            ));
        }
        for (ms, row, cols) in self.gaps.iter().take(3) {
            s.push_str(&format!("\n  t={ms:6} row {row} live-cell gap at {cols:?}"));
        }
        s
    }
}

/// A line typed at 12 cps, every key's repaint from `…WORK IN` on torn at the same text column
/// (the 1 KiB read boundary), then a rest through the whole exit. Neither the plan nor the
/// ribbon's live cells may carry an interior dark run. The control is the same line untorn.
#[test]
fn the_owner_s_line_torn_at_the_same_column_stays_whole_into_the_swoosh() {
    /// The text index of the `T` of `INTO`: a read boundary here shows
    /// `…WORK IN` and blanks `TO MAIN!!!!`.
    const AT_TO: usize = 101;
    for (what, tear) in [("no tear", false), ("torn at TO", true)] {
        let mut h = Composer::new(140);
        h.type_line(LINE, |k| {
            if tear && k > AT_TO + 2 {
                Cut::After(AT_TO)
            } else {
                Cut::Whole
            }
        });
        assert!(
            h.rows_lit_max == 1,
            "{what}: the setup: the line lit {} rows",
            h.rows_lit_max
        );
        h.rest(IDLE_CEILING_MS + 100);
        assert!(h.holes.is_empty() && h.gaps.is_empty(), "{}", h.story(what));
        assert_eq!(
            rest_of(&h.glow, 0, h.now).cells,
            0,
            "{what}: the band is out {IDLE_CEILING_MS} ms after the last key"
        );
    }
}
