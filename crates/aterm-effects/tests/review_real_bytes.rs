// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE TORN-READ FIX ON REAL BYTES, AND WHAT IT COSTS** (2026-09-22, the
//! review of the owner's gap: *"There is a rainbow trail gap in 0.89. I'm not
//! sure what is causing it but it seems like some kind of back cursor
//! movement bug."*).
//!
//! Everything here drives the HOST SEAM: a real `aterm_core` [`Terminal`] fed
//! the bytes, its rows sampled exactly as `app_render.rs`'s LOCK A samples
//! them (the caret's row, every row the ribbon names, the print anchor), its
//! content-scroll clock read as `sync_cursor_effect_scroll` reads it, fed to
//! [`CursorGlow`] with the hints `app_input.rs` stamps for each key.
//!
//! * **REAL BYTES AT EVERY READ SIZE.** The four Claude Code composer takes
//!   and Codex's particle take, each program burst presented whole, then
//!   re-cut into 1024- and 512-byte reads with a present after EVERY read.
//!   No interior dark run on the composer row from the first typed key to
//!   the last, and nothing on glass once the take has rested past the longest
//!   swoosh. The Claude takes are read with the `?2026` hold LAPSED (the
//!   host's cap is 150 ms, `SYNC_HOLD_CAP`), which is the condition the torn
//!   read needs; Codex's take is read with the hold HELD, as SYNC-1 holds it
//!   — presented torn, Codex's own frames park a VISIBLE caret 90 columns
//!   from the text mid-frame and the typed licence pays there, a stray-light
//!   defect that measures the same before and after this fix (the review
//!   reports it; it is not this fix's lane).
//! * **THE OWNER'S LINE INTO THE SWOOSH.** The line typed at 12 cps into an
//!   Ink-shaped composer with every key's repaint torn at the same text
//!   column — the owner's own shape — then the hand rests through the whole
//!   exit. Two censuses: the PLAN's coverage (what a pixel census on glass
//!   reads) and the ribbon's own LIVE CELLS (the census
//!   `codex_particle_replay.rs` reads, because the plan feathers across a
//!   one-cell gap and hides it until the band drains). Measured on
//!   `e05d7860e`: 130 frames with an interior dark run and 156 frames with a
//!   gap in the live cells; here, none of either.
//! * **THE COST** of [`Witness::walk`] and of the wake per call, and of the
//!   whole seam frame, printed by a release-only twin — the before / after
//!   against `e05d7860e` is the review's; the twin pins only that no single
//!   call eats §18's 400 µs frame budget.

use std::time::{Duration, Instant};

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::ribbon::{Cell, Layer, Ribbon};
use aterm_effects::rainbow_kitty::witness::{RowSample, WITNESS_ROWS, Witness};
use aterm_effects::rainbow_kitty::{Config, Ctx, Dir, Event, Flow, Licence, TypedClass};

// ===========================================================================
// Shared
// ===========================================================================

/// A column is LIT at this planned coverage (the glass census's 20/255).
const LIT_COV: u8 = 20;

/// Idle after the last gesture before T6 is read: every v2 life is shorter
/// (the longest swoosh is a 1.5 s rest plus 0.79 s at the slowest tempo —
/// `rainbow_kitty_v2_frame_cost.rs`'s own ceiling).
const IDLE_CEILING_MS: u64 = 2_500;

/// The owner's line.
const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN!!!!";

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

fn geom(rows: usize, cols: usize, cw: usize, ch: usize) -> Geom {
    Geom {
        cw,
        ch,
        rows,
        cols,
        origin_x: 0,
        origin_y: 0,
        win_w: (cols * cw) as u16,
        win_h: (rows * ch) as u16,
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

/// Plan coverage per column of `row`.
fn coverage(glow: &CursorGlow, g: Geom, row: u16) -> Vec<u8> {
    let mut cov = vec![0u8; g.cols];
    let rib = glow.v2_ribbon().expect("rainbow kitty owns the frame");
    for sg in rib.plan_segments() {
        let r = ((sg.spine / g.ch as f32) - 0.5).floor();
        let c = (sg.x / g.cw as f32).floor();
        if r >= 0.0 && c >= 0.0 && r as usize == usize::from(row) && (c as usize) < g.cols {
            let i = c as usize;
            cov[i] = cov[i].max(sg.cov);
        }
    }
    cov
}

/// Interior dark runs of a coverage lane, as `(first col, len)`.
fn holes_of(cov: &[u8]) -> Holes {
    let lit: Vec<usize> = (0..cov.len()).filter(|&i| cov[i] >= LIT_COV).collect();
    let mut holes = Vec::new();
    if lit.len() < 2 {
        return holes;
    }
    let mut start = None;
    for (i, &v) in cov
        .iter()
        .enumerate()
        .take(lit[lit.len() - 1] + 1)
        .skip(lit[0])
    {
        if v >= LIT_COV {
            if let Some(s) = start.take() {
                holes.push((s, i - s));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    holes
}

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

/// What T6 reads once the take has rested.
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

// ===========================================================================
// 1. Real bytes at every read size
// ===========================================================================

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

/// The Claude Code takes (`resize 40 120`, 7×14 px cells, recorded
/// 2026-09-21 under a PTY wrapper inside a headless aterm: the owner's line
/// at 12 cps, ⌥← and `INTO ` inserted, ⌥→ / End back, three hops each way,
/// an `@`-mention popup) and Codex's particle take (`resize 32 100`, 7×14 px,
/// 2026-09-16: a line, ⌥← ×3, a word typed inside it, ⌥→ ×3, two letters).
const TAKES: [Take; 5] = [
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
];

/// How a program burst reaches LOCK A.
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

enum Rec {
    Out(Vec<u8>),
    In(Vec<u8>),
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

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

fn is_key(b: &[u8]) -> bool {
    matches!(b, [c] if (0x20..0x7f).contains(c))
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
            cfg: cfg(),
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

    /// The host's own order: `sync_cursor_effect_scroll`, LOCK A's samples
    /// (the caret's row, every ribbon row, the print anchor), the tick.
    /// Returns false when SYNC-1 withheld the present.
    fn frame(&mut self) -> bool {
        if self.term.sync_open_dirty() {
            if self.hold {
                return false;
            }
            self.torn_presents += 1;
        }
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
        self.glow.observe_print_anchor(self.term.print_anchor());
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
    let census =
        |h: &ReplayHost, row: Option<u16>, holes: &mut Vec<HoleFrame>, lit_max: &mut usize| {
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
        };
    for (ms, ev) in rec {
        if ms >= exit {
            break;
        }
        let t = h.t0 + Duration::from_millis(ms);
        while h.now + Duration::from_millis(REPLAY_FRAME_MS) <= t {
            h.now += Duration::from_millis(REPLAY_FRAME_MS);
            if h.frame() {
                census(&h, composer_row, &mut holes, &mut lit_max);
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
                        census(&h, composer_row, &mut holes, &mut lit_max);
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

/// **CLAUDE CODE'S BYTES, EVERY READ SIZE (the insert take).** The owner's
/// line typed into the real composer, ⌥← into it, `INTO ` inserted, ⌥→ back:
/// presented whole, and in 1024- and 512-byte reads with a present after
/// each. No interior hole while typing; nothing on glass at rest.
#[test]
fn claude_code_s_insert_take_holds_whole_and_torn_at_every_read_size() {
    assert_take_holds(&TAKES[0]);
}

/// The End take: the insert, then End back to the end of the line.
#[test]
fn claude_code_s_end_take_holds_whole_and_torn_at_every_read_size() {
    assert_take_holds(&TAKES[1]);
}

/// The scrub take: three word hops each way inside the typed line.
#[test]
fn claude_code_s_scrub_take_holds_whole_and_torn_at_every_read_size() {
    assert_take_holds(&TAKES[2]);
}

/// The popup take: an `@`-mention popup open under the composer while
/// typing.
#[test]
fn claude_code_s_popup_take_holds_whole_and_torn_at_every_read_size() {
    assert_take_holds(&TAKES[3]);
}

/// **CODEX'S BYTES, EVERY READ SIZE.** Codex paints braille particles into
/// the composer's blank cells on every ~150 ms frame inside `?2026`, and its
/// reads are 1 KiB; re-cut at 1024 and 512 bytes, with SYNC-1 holding the
/// present while a bracket is open, as the host holds it. The band under the
/// typed line, the backward edit and the forward hops stays one run, and
/// nothing is left on glass at rest.
#[test]
fn codex_s_particle_take_holds_whole_and_torn_at_every_read_size() {
    assert_take_holds(&TAKES[4]);
}

// ===========================================================================
// 2. The owner's line, torn, into the swoosh
// ===========================================================================

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
/// 12 cps, the owner's pace.
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
    timing: bool,
    frames: usize,
    /// Per-frame cost of the glow calls alone, µs.
    frame_us: Vec<u64>,
    /// `(ms, row, label, lit map, holes)` per frame with an interior dark
    /// run in the PLAN.
    holes: Vec<RowHoleFrame>,
    /// `(ms, row, columns)` per frame with an interior gap in the LIVE
    /// cells.
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
            cfg: cfg(),
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
            timing: false,
            frames: 0,
            frame_us: Vec::new(),
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

    /// LOCK A and the tick, timed; then the census, outside the timer.
    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        let now = self.now;
        let mut spent = Duration::ZERO;
        if epoch != self.blink {
            self.blink = epoch;
            self.glow.note_repaint_blink(now);
        }
        let alt = self.term.is_alternate_screen();
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        let mut rows = [0u16; WITNESS_ROWS];
        let t = Instant::now();
        self.glow.note_context(alt);
        self.glow.observe_row(c.row, c.col, &self.row_buf, now);
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let n = self.glow.ribbon_rows(&mut rows);
        spent += t.elapsed();
        for &r in &rows[..n] {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            let t = Instant::now();
            self.glow.observe_ribbon_row(r, &self.row_buf);
            spent += t.elapsed();
        }
        let anchor = self.term.print_anchor();
        let t = Instant::now();
        self.glow.observe_print_anchor(anchor);
        self.glow.tick(cur, now, &self.cfg, self.g, &mut self.out);
        spent += t.elapsed();
        if self.timing {
            self.frame_us.push(spent.as_micros() as u64);
        }
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

/// **THE OWNER'S OWN SHAPE, READ TO THE END OF THE SWOOSH.** The line typed
/// at 12 cps into the composer, every key's repaint from `…WORK IN` on torn
/// at that same text column (the 1 KiB read boundary of a frame whose head
/// does not move), then the hand rests through the whole exit. Nothing may
/// carry an interior dark run — neither the PLAN (what a pixel census reads)
/// nor the ribbon's own LIVE CELLS, whose one-cell gaps the plan feathers
/// over while the band is whole and the drain exposes as a comb. The control
/// is the same line with no tear at all. Measured on `e05d7860e`: the torn
/// take carried 130 frames with a dark run and 156 with a live-cell gap, the
/// first at `t=8715` under the hand; here, none of either.
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

// ===========================================================================
// 3. The cost (release-only)
// ===========================================================================

fn median_ns(mut v: Vec<u64>) -> (u64, u64, u64) {
    v.sort_unstable();
    let n = v.len().max(1);
    (v[n / 2], v[(n * 9 / 10).min(n - 1)], v[0])
}

fn samples(rows: &[(u16, Vec<char>)]) -> Vec<RowSample<'_>> {
    rows.iter()
        .map(|(row, cols)| RowSample { row: *row, cols })
        .collect()
}

/// One walk on a fresh copy of `w0` per sample.
fn time_walk(w0: &Witness, cells: &[Cell], rows: &[RowSample<'_>], n: usize) -> (u64, u64, u64) {
    let mut retire = Vec::with_capacity(4096);
    let mut release = Vec::with_capacity(4096);
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let mut w = w0.clone();
        let t = Instant::now();
        std::hint::black_box(w.walk(cells, rows, &mut retire, &mut release));
        v.push(t.elapsed().as_nanos() as u64);
    }
    median_ns(v)
}

/// One same-row keyed move (the wake and the abandon behind it) on a fresh
/// copy of `rib` per sample.
fn time_move(
    rib: &Ribbon,
    cfg: &Config,
    g: Geom,
    at: Instant,
    from: (u16, u16),
    to: (u16, u16),
    n: usize,
) -> (u64, u64, u64) {
    let ctx = Ctx {
        now: at,
        geom: g,
        cfg,
        disp: 0.6,
        birth_disp: 0.6,
        phase: 0.0,
        caret: to,
        caret_t: rib.field_at(to.0, to.1).unwrap_or(0.0),
        caret_walk: rib.walk_origin_at(to.0, to.1),
        mend: None,
        surge: 0.0,
        flow: Flow::default(),
    };
    let ev = Event::Move {
        from,
        to,
        licence: Licence::Nav,
        dir: if to.1 < from.1 { Dir::Left } else { Dir::Right },
    };
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let mut r = rib.clone();
        let t = Instant::now();
        r.on_event(&ev, at, &ctx);
        std::hint::black_box(&r);
        v.push(t.elapsed().as_nanos() as u64);
    }
    median_ns(v)
}

/// §18's frame budget, p50.
const FRAME_BUDGET_US: u64 = 400;

/// **THE COST, PRINTED.** Release-only (`--ignored`): the whole seam frame
/// over the owner's line typed with every key torn; [`Witness::walk`] per
/// call on inputs any build holds identically (the owner's line at 140
/// columns, and a three-row band at 177 — every column a cell — whole and
/// torn four ways); and the wake per call on a band laid by plain typing and
/// left to its swoosh (a Home jump and a five-cell word hop). Each line is
/// `COST <what> p50 p90 min`, ns unless it says µs. The review compares them
/// against the same file on `e05d7860e`; the file itself pins only that no
/// single call reaches §18's 400 µs frame budget.
#[test]
#[ignore = "release-only cost report; run with --release -- --ignored --nocapture"]
fn the_walk_and_the_wake_fit_the_frame_budget_on_the_owner_s_line() {
    let n = 400;
    let mut worst = Vec::new();
    let rk_cfg = Config::from_glow(&cfg(), false);
    {
        let mut h = Composer::new(140);
        h.timing = true;
        h.type_line(LINE, |k| if k > 8 { Cut::After(k - 2) } else { Cut::Whole });
        let v = h.frame_us.clone();
        let fmax = v.iter().copied().max().unwrap_or(0);
        let (p50, p90, _) = median_ns(v);
        println!("\nCOST seam-frame-us/owner-line-torn p50={p50} p90={p90} max={fmax}");
    }
    let t0 = Instant::now();
    for (name, cols, rows, tears) in [
        (
            "owner-140x1",
            140usize,
            1usize,
            vec![("torn-under-TO", 101usize), ("torn-tail-2", 109)],
        ),
        (
            "band-177x3",
            177,
            3,
            vec![
                ("torn-tail-rows-2-3", 183),
                ("torn-all-but-5", 5),
                ("torn-last-2", 3 * 173 - 2),
            ],
        ),
    ] {
        let w = cols - 4;
        let text: Vec<char> = LINE
            .chars()
            .chain(" ".chars())
            .cycle()
            .take(rows * w)
            .collect();
        let cells: Vec<Cell> = (0..rows * w)
            .map(|i| {
                let born = t0 + Duration::from_millis(i as u64 * 60);
                Cell {
                    row: (R0 + i / w) as u16,
                    col: (2 + i % w) as u16,
                    cohort: (1 + i / w) as u32,
                    t: i as f32 * 0.01,
                    born,
                    attack_at: born,
                    life_s: 1.7,
                    cov0: 0.8,
                    typing: true,
                    retract_at: None,
                    retire_at: None,
                    birth_disp: 0.5,
                    edge_cells: 0.0,
                    layer: Layer::Base,
                    rearm: None,
                    released_at: None,
                }
            })
            .collect();
        let row_of = |cut: usize| -> Vec<(u16, Vec<char>)> {
            (0..rows)
                .map(|r| {
                    let mut v = vec![' '; cols];
                    v[0] = if r == 0 { '>' } else { ' ' };
                    for c in 0..w {
                        let i = r * w + c;
                        if i < cut {
                            v[2 + c] = text[i];
                        }
                    }
                    ((R0 + r) as u16, v)
                })
                .collect()
        };
        let whole = row_of(usize::MAX);
        let sw = samples(&whole);
        let mut w0 = Witness::new();
        let (mut ret, mut rel) = (Vec::new(), Vec::new());
        w0.walk(&cells, &sw, &mut ret, &mut rel);
        w0.walk(&cells, &sw, &mut ret, &mut rel);
        let (p50, p90, min) = time_walk(&w0, &cells, &sw, n);
        println!(
            "COST walk-ns/{name}/whole cells={} p50={p50} p90={p90} min={min}",
            cells.len()
        );
        worst.push((format!("walk/{name}/whole"), p50));
        for (what, cut) in tears {
            let torn = row_of(cut);
            let st = samples(&torn);
            let mut probe = w0.clone();
            probe.walk(&cells, &st, &mut ret, &mut rel);
            let (p50, p90, min) = time_walk(&w0, &cells, &st, n);
            println!(
                "COST walk-ns/{name}/{what} retire={} release={} p50={p50} p90={p90} min={min}",
                ret.len(),
                rel.len()
            );
            worst.push((format!("walk/{name}/{what}"), p50));
        }
    }
    for (name, cols, typed) in [
        ("owner-140x1", 140usize, LINE.len()),
        ("band-177x2", 177, 173 + 60),
    ] {
        let mut h = Composer::new(cols);
        let text: String = LINE
            .chars()
            .chain(" ".chars())
            .cycle()
            .take(typed)
            .collect();
        h.type_line(&text, |_| Cut::Whole);
        h.rest(1_300);
        let rib = h.glow.v2_ribbon().expect("ribbon").clone();
        let (row, col) = h.at_index(h.caret);
        let at = h.now + Duration::from_millis(4);
        for (what, to) in [("home-jump", (row, 2u16)), ("hop-5", (row, col - 5))] {
            let (p50, p90, min) = time_move(&rib, &rk_cfg, h.g, at, (row, col), to, n);
            println!(
                "COST wake-ns/{name}/{what} span={} cells={} p50={p50} p90={p90} min={min}",
                col - to.1,
                rib.cells().len()
            );
            worst.push((format!("wake/{name}/{what}"), p50));
        }
    }
    let over: Vec<_> = worst
        .iter()
        .filter(|(_, p50)| *p50 > FRAME_BUDGET_US * 1_000)
        .collect();
    assert!(
        over.is_empty(),
        "a single call takes the whole §18 frame budget: {over:?}"
    );
}
