// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **EVERY GROWTH OF THE COMPOSER FOLLOWS AND FLOWS** (2026-09-23) — the
//! owner: *"the existing line rainbow should beautifully flow and drift and
//! fade away, not simply abruptly vanish"*. On glass (a headless instance,
//! 60 columns, 146 keys at 130 ms, Claude Code's composer wrapped twice, no
//! Enter) `trail status` read `ribbon_followed=54` AND `ribbon_retired=58`:
//! one row followed its text and flowed, the other was RETIRED on the
//! content witness's 0.12 s melt.
//!
//! The mechanism, measured by this harness before the fix (the forensics
//! lane, `flow_forensics.rs`, whose Host this file keeps): the follow pass
//! (`Witness::follow_runs`) counted a TWIN — a glyph armed while the row
//! above already held the same glyph at the same column — as NOT FOUND,
//! and one such record inside the run broke the one-block rule, so the run
//! was refused at every offset and melted. At the first growth the row
//! above is the rule, so it followed; at every later growth the row above
//! is the paragraph's previous line, and a letter under the same letter is
//! ordinary English — the owner's own capture puts `WHY` under `HEY`. With
//! his text at 90 columns wraps 2 and 3 followed `+0`, retired `+86`, and
//! the row was gone at 140 ms; the same text with no twin followed `+77`
//! and flowed 1.3 s; ONE changed letter flipped it.
//!
//! **AND THE NEW ROW CONTINUES THE OLD ROW'S WALK** (2026-09-24, the
//! re-review: *"F1 is still open where a wrap moves a word at least as long
//! as the line it keeps"*). A wrap whose moved word is at least as long as
//! the line it keeps relays that word over the column of the Space the wrap
//! ate; that Space's cell stood in the old line's run, and the new row was
//! laid into it from red on the fast leg — the row above's stops column for
//! column. [`read_new_row`] and [`assert_new_row_continues`] read the new
//! row after the relay, the hand stopped or typing on, and require it to be
//! the old row's walk continued, colour and pace; the censuses with the
//! hand stopped read it too.
//!
//! Every test drives the composer at the HOST seam — a real
//! `aterm_core::terminal::Terminal`, its rows sampled exactly as
//! `app_render.rs`'s frame hold samples them (`tests/composer_box_growth_wrap.rs`
//! `Host::frame`, verbatim), frames on a 16.7 ms train — and reads, for
//! each wrap, the counters and the moving line's own cells tracked by
//! identity `(col, born)`, frame by frame for 1.6 s.
//!
//! **WHAT IS ASSUMED about the wraps after the first** (only the first was
//! captured — `docs/measured/claude-code-composer-wrap-bytes-2026-09-21.md`):
//! the box grows one row UP per wrap and is repainted in ONE chunk inside
//! the DEC 2026 bracket, in the captured first-wrap shape generalised to
//! `n` text rows: `ESC[?25l ESC[H CR ESC[<rule−1>B <rule>`, then per text
//! row `CR ESC[1B <prefix><line> ESC[K` (prefix `❯`+NBSP on the first row,
//! two spaces on every continuation row), then `ESC[<rows>;1H ESC[<text
//! row>;<caret>H ESC[?25h`. Every row is written IN FULL: the capture's
//! cell diff skipped one unchanged cell (the reused `Y`), and a skipped
//! cell leaves the grid byte-identical to a rewritten one, so nothing the
//! witness samples (the grid after the batch) can differ. The line breaks
//! are a greedy word wrap at `cols − 4` text cells (the capture wrapped
//! `…AUDIT WH|Y` at 86 cells of a 90-column pane): a glyph that would
//! overflow moves the last line's trailing word (after its last space)
//! down with it, the space itself dropped. Ordinary keys and Spaces use
//! the captured per-key shapes on the bottom text row.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::meteor::tri;
use aterm_effects::rainbow_kitty::ribbon::{FLOW_TOTAL_S, RETIRE_MELT_S, walk_t};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use aterm_spec::derive::rainbow_short_wrap_park_model;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

const FRAME_US: u64 = 16_667;
const FRAME_MS: f32 = FRAME_US as f32 / 1000.0;
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
/// The census window after each wrap: past `FLOW_TOTAL_S` with room.
const WINDOW_MS: u64 = 1600;
/// Keys typed after a wrap before the new row's walk is read (the review's
/// reading of the HOW/HIS text: 12 to 16 keys on).
const SNAP_KEYS: usize = 12;
/// The worst step one column may take in the FOLDED spectrum (`tri(t)`, as
/// `Ink::sample` reads it) on a row that continues a walk past its knee:
/// [`WALK_LAY_RATE`](aterm_effects::rainbow_kitty::ribbon::WALK_LAY_RATE) is
/// `1/36 ≈ 0.028`; the review measured `0.194` where a new line captured the
/// row above's walk.
const FOLDED_STEP_MAX: f32 = 0.03;
/// When a census with the hand STOPPED at the wrap reads the new line: past
/// the held park's flush, which replays the wrap key's echo — and with it
/// the relay of the moved word — no later than 0.25 s after the key.
const STOPPED_SNAP_MS: u64 = 400;

#[derive(Clone, Copy, Debug)]
struct Shape {
    rows: usize,
    cols: usize,
    cw: usize,
    ch: usize,
    key_ms: u64,
}

/// The owner's window of the capture: 90×30, the retina cell, keys at 60 ms.
const OWNER_90: Shape = Shape {
    rows: 30,
    cols: 90,
    cw: 15,
    ch: 28,
    key_ms: 60,
};

/// The lead's glass take: 60 columns, keys at 130 ms.
const GLASS_60: Shape = Shape {
    rows: 30,
    cols: 60,
    cw: 15,
    ch: 28,
    key_ms: 130,
};

/// A narrow composer, 20 columns (16 text cells) at 130 ms: words are long
/// against the row, so a wrap can move more letters than it keeps (the
/// owner's narrow-window reports, the review of the half-gate).
const NARROW_20: Shape = Shape {
    rows: 30,
    cols: 20,
    cw: 15,
    ch: 28,
    key_ms: 130,
};

/// Whether the hand types on through each wrap's census window, or stops at
/// the censused wrap's key (the idle hand; the only way to census a wrap
/// whose successor comes inside its window).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hand {
    TypesOn,
    Stops,
}

/// The capture's text, continued — long enough for three wraps at 90
/// columns. It is the owner's own text: `…AUDIT WH|Y` puts `WHY` under
/// `HEY`, the very coincidence Ink's diff reused the `Y` for.
const NATURAL: &str = "HEY!!! [Image #1] in the other tab in aterm, I got this confirmation message. AUDIT WHY ATERM claude code harness introspection interface keeps losing the rainbow when the composer grows to a third row and then a fourth row while I keep typing at a steady pace so please make the old line flow and drift and fade away beautifully instead of vanishing at once like it does now";

fn geom(s: Shape) -> Geom {
    Geom {
        cw: s.cw,
        ch: s.ch,
        rows: s.rows,
        cols: s.cols,
        origin_x: 0,
        origin_y: 0,
        win_w: (s.cols * s.cw) as u16,
        win_h: (s.rows * s.ch) as u16,
        head: 0,
    }
}

/// `tests/composer_box_growth_wrap.rs`'s config: rainbow kitty at 0.70 on
/// the default dark theme.
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

/// The rule row with its label, `workspace`, ten columns from the right.
fn rule(cols: usize) -> String {
    let mut s: String = "\u{2500}".repeat(cols - 12);
    let _ = write!(s, "\x1b[{}Gworkspace\x1b[{}G\u{2500}", cols - 10, cols);
    s
}

/// The composer's bottom text row, 1-based (the status rows below it).
fn text_row(s: Shape) -> usize {
    s.rows - 2
}

/// Greedy word wrap of one appended char at `width` text cells: returns
/// whether the line count grew. A glyph that overflows takes the last
/// line's trailing word down with it; the space before that word is
/// dropped.
fn append(lines: &mut Vec<String>, ch: char, width: usize) -> bool {
    let last = lines.last_mut().expect("a line");
    if last.chars().count() < width {
        last.push(ch);
        return false;
    }
    assert!(
        ch != ' ',
        "harness premise: the text puts no Space on a wrap (line {last:?})"
    );
    let moved = match last.rfind(' ') {
        Some(i) => {
            let m = last[i + 1..].to_string();
            last.truncate(i);
            m
        }
        None => String::new(),
    };
    lines.push(format!("{moved}{ch}"));
    true
}

/// `text` with every Space that would land ON a wrap dropped (the premise
/// of [`append`]), so the words either side of it join; a no-op on a text
/// that never puts a Space there.
fn fit(text: &str, width: usize) -> String {
    let mut lines = vec![String::new()];
    let mut out = String::new();
    for ch in text.chars() {
        if ch == ' ' && lines.last().expect("line").chars().count() >= width {
            continue;
        }
        append(&mut lines, ch, width);
        out.push(ch);
    }
    out
}

/// The final line layout `text` wraps to at `width`: for each final line,
/// the indices of `text`'s chars on it.
fn layout(text: &str, width: usize) -> Vec<Vec<usize>> {
    let mut lines: Vec<String> = vec![String::new()];
    let mut idx: Vec<Vec<usize>> = vec![Vec::new()];
    for (i, ch) in text.chars().enumerate() {
        if append(&mut lines, ch, width) {
            let moved_n = lines.last().expect("line").chars().count() - 1;
            let prev = idx.last_mut().expect("line");
            // The moved word's indices go down; the dropped space too.
            let keep = lines[lines.len() - 2].chars().count();
            let tail: Vec<usize> = prev.split_off(keep);
            let mut next: Vec<usize> = tail[tail.len() - moved_n..].to_vec();
            next.push(i);
            idx.push(next);
        } else {
            idx.last_mut().expect("line").push(i);
        }
    }
    idx
}

/// THE CONTROL TEXT: `text` with everything but letters and spaces dropped,
/// and each char's case set by the parity of the FINAL line it wraps onto —
/// upper on even lines, lower on odd — so no glyph of a line stands at the
/// same column as the same glyph on the line above it (the witness's
/// `twins` for `dr = −1` are never set). Case changes no width, so the
/// wraps fall exactly where the letters-only text's do.
fn alternating_case(text: &str, width: usize) -> String {
    let letters: String = fit(
        &text
            .chars()
            .filter(|c| c.is_ascii_alphabetic() || *c == ' ')
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        width,
    );
    let chars: Vec<char> = letters.chars().collect();
    let mut out = chars.clone();
    for (k, line) in layout(&letters, width).iter().enumerate() {
        for &i in line {
            out[i] = if k % 2 == 0 {
                chars[i].to_ascii_uppercase()
            } else {
                chars[i].to_ascii_lowercase()
            };
        }
    }
    out.into_iter().collect()
}

/// `text` (already fitted, e.g. [`alternating_case`]'s) with ONE glyph of
/// final line `k` changed to the glyph the line above it has at the same
/// column — the column nearest the line's middle where both are letters —
/// so exactly one `dr = −1` twin is armed on line `k`, inside its block. A
/// letter for a letter: the wraps fall exactly where they did.
fn with_one_twin(text: &str, width: usize, k: usize) -> String {
    let lay = layout(text, width);
    let mut chars: Vec<char> = text.chars().collect();
    let (above, line) = (&lay[k - 1], &lay[k]);
    let mid = line.len() / 2;
    let mut order: Vec<usize> = (0..line.len().min(above.len())).collect();
    order.sort_by_key(|&j| j.abs_diff(mid));
    let j = order
        .into_iter()
        .find(|&j| chars[line[j]].is_ascii_alphabetic() && chars[above[j]].is_ascii_alphabetic())
        .expect("a column where both lines hold a letter");
    chars[line[j]] = chars[above[j]];
    chars.into_iter().collect()
}

/// One frame of a wrap's census.
#[derive(Clone, Copy, Debug)]
struct FrameRec {
    t_ms: f32,
    /// The target row's coverage: the share of its pixel width the bed's
    /// quads cover this frame, each weighted by its opacity.
    cov: f32,
    /// The moving line's cells still resident, anywhere.
    res: u16,
    /// …on the row the text LEFT (the caret's row).
    on_old: u16,
}

/// One wrap: what moved, and what became of it.
#[derive(Clone, Debug)]
struct Series {
    wrap_no: usize,
    at: Instant,
    /// The line the wrap carried a row up, minus the moved word.
    kept: usize,
    /// The moved word's length.
    w: usize,
    /// The moved word itself (the wrap key not included).
    moved: String,
    /// The `dr = −1` twins of the moving line: columns where it stood under
    /// the same glyph on the row above when it was armed — its first and
    /// last columns included (2026-09-23, the review: an EDGE twin whose
    /// glyph the moved word re-lays under it was the case the first cut of
    /// the premise could not see).
    twins: Vec<u16>,
    /// The two rows' walks [`SNAP_KEYS`] keys after the wrap, when the hand
    /// typed that far inside the window — or [`STOPPED_SNAP_MS`] after it,
    /// when the hand stopped at the wrap.
    snap: Option<Snap>,
    /// The walk the moved word's first letter had on the row it came off
    /// ([`OldWalk`]) — what the new line must continue; `None` when the wrap
    /// moved no word.
    first: Option<OldWalk>,
    /// The moving line's cells: `(col, born)`.
    tracked: HashSet<(u16, Instant)>,
    old_row: u16,
    target_row: u16,
    counts0: (u64, u64, u64),
    counts1: (u64, u64, u64),
    frames: Vec<FrameRec>,
}

/// The walks of the row the text left (the new line) and of the row it went
/// to, read [`SNAP_KEYS`] keys after a wrap (or [`STOPPED_SNAP_MS`] after
/// it, the hand stopped): `(col, t)` of each column's newest standing cell.
#[derive(Clone, Debug)]
struct Snap {
    new_row: Vec<(u16, f32)>,
    above: Vec<(u16, f32)>,
    /// How many cells the new line must show lit (two may be missing): the
    /// keys typed after the wrap, or — the hand stopped — the moved word and
    /// the wrap key's glyph.
    expect: usize,
    /// The hand typed on to this reading (it stopped at the wrap otherwise).
    typed_on: bool,
}

/// **THE WALK THE MOVED WORD HAD** (2026-09-24), read off the old row just
/// before the key that wraps: the `t` of the cell under the moved word's
/// first letter, the walk's distance there (its cohort's
/// `Cohort::walk_d`), and the walk's zero (`Cohort::walk_zero`). C2 — the
/// relay's own law — says the relaid word's first cell keeps exactly that
/// `t` and that distance, and every column after it continues the same
/// walk: new column `c` is `zero + walk_t(d + (c − 2))`, the colour AND the
/// pace the old row would have had there.
#[derive(Clone, Copy, Debug)]
struct OldWalk {
    t: f32,
    d: f32,
    zero: f32,
}

impl OldWalk {
    /// Read off `h`'s ribbon at `(row, col)`, the moved word's first letter.
    fn at(h: &Host, row: u16, col: u16) -> Option<Self> {
        let rib = h.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let cell = rib
            .cells()
            .iter()
            .filter(|c| c.row == row && c.col == col && !c.leaving())
            .max_by_key(|c| c.born)?;
        let coh = rib.cohorts().iter().find(|c| c.id == cell.cohort)?;
        Some(Self {
            t: cell.t,
            d: coh.walk_d(col),
            zero: coh.walk_zero(),
        })
    }

    /// The walk continued to column `col` of the new line (its text starts
    /// at column 2): `(t, distance)`.
    fn continued(self, col: u16) -> (f32, f32) {
        let d = self.d + f32::from(col) - 2.0;
        (self.zero + walk_t(d), d)
    }
}

impl Series {
    fn followed(&self) -> u64 {
        self.counts1.0 - self.counts0.0
    }

    fn retired(&self) -> u64 {
        self.counts1.1 - self.counts0.1
    }

    fn missed(&self) -> u64 {
        self.counts1.2 - self.counts0.2
    }

    /// Every ninth frame, `t:cov/resident/on-old` — the failure message.
    fn curve(&self) -> String {
        self.frames
            .iter()
            .step_by(9)
            .map(|f| format!("{:.0}:{:.3}/{}/{}", f.t_ms, f.cov, f.res, f.on_old))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

struct Host {
    s: Shape,
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
    /// The box's text lines.
    lines: Vec<String>,
    wraps: usize,
    /// The wraps a census is opened for; every wrap when empty.
    census: Vec<usize>,
    /// Keys pressed since the last wrap.
    keys_since_wrap: usize,
    active: Option<Series>,
    done: Vec<Series>,
    /// **THE ECHO'S LAG** (2026-09-24): how long after its press a key's
    /// PTY bytes are processed — `0`, the key and its echo on one instant,
    /// unless a test sets it. Every lag used here is shorter than the key
    /// cadence, so the bytes still arrive in press order between frames, as
    /// a PTY delivers them, and the frames between the press and the echo
    /// see the key's typed stamp with nothing echoed yet.
    lag_ms: u64,
}

impl Host {
    /// Claude Code's screen before the first key: the alt screen, the rule
    /// above the text row, the prompt marker, the status row, the caret at
    /// the inset; one frame presented.
    fn new(s: Shape) -> Self {
        let mut term = Terminal::new(s.rows as u16, s.cols as u16);
        let tr = text_row(s);
        term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
        term.process(
            format!(
                "\x1b[{};1H{}\x1b[{tr};1H\u{276f}\u{a0}\x1b[{};1H  auto mode on\x1b[{tr};3H",
                tr - 1,
                rule(s.cols),
                tr + 1
            )
            .as_bytes(),
        );
        let now = Instant::now();
        let mut h = Self {
            s,
            term,
            glow: CursorGlow::default(),
            cfg: cfg(),
            g: geom(s),
            now,
            next_frame: now,
            last_key: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            lines: vec![String::new()],
            wraps: 0,
            census: Vec::new(),
            keys_since_wrap: 0,
            active: None,
            done: Vec::new(),
            lag_ms: 0,
        };
        h.frame();
        h
    }

    /// EXACTLY the frame hold, then the tick (`tests/composer_box_growth_wrap.rs`
    /// `Host::frame`, verbatim) — then the census, while a wrap's window is
    /// open.
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
        self.record();
    }

    fn advance_to(&mut self, t: Instant) {
        while self.next_frame <= t {
            self.now = self.next_frame;
            self.frame();
        }
        self.now = self.now.max(t);
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.advance_to(t);
    }

    /// `(ribbon_followed=, ribbon_retired=, ribbon_follow_missed=)`.
    fn counts(&self) -> (u64, u64, u64) {
        self.glow
            .v2_status()
            .map_or((0, 0, 0), |s| (s.followed, s.retired, s.follow_missed))
    }

    /// The target row's coverage census (`tests/composer_box_growth_wrap.rs`
    /// `coverage`): the bed's quads whose vertical extent covers the row's
    /// centre, weighted by opacity, over the row's pixel width.
    fn coverage(&self, row: u16) -> f32 {
        let centre = f32::from(self.g.origin_y) + (f32::from(row) + 0.5) * self.s.ch as f32;
        let sum: f32 = self
            .glow
            .under_quads()
            .iter()
            .filter(|q| {
                let y0 = f32::from(q.y);
                q.w > 0 && q.alpha > 0 && y0 <= centre && centre < y0 + f32::from(q.h)
            })
            .map(|q| f32::from(q.w) * f32::from(q.alpha) / 255.0)
            .sum();
        sum / (self.s.cols * self.s.cw) as f32
    }

    fn record(&mut self) {
        let Some(sr) = self.active.as_ref() else {
            return;
        };
        if self.now < sr.at {
            return;
        }
        let since = self.now.saturating_duration_since(sr.at);
        if since > Duration::from_millis(WINDOW_MS) {
            let mut sr = self.active.take().expect("open");
            sr.counts1 = self.counts();
            self.done.push(sr);
            return;
        }
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let (mut res, mut on_old) = (0u16, 0u16);
        for c in rib.cells() {
            if sr.tracked.contains(&(c.col, c.born)) {
                res += 1;
                if c.row == sr.old_row {
                    on_old += 1;
                }
            }
        }
        let rec = FrameRec {
            t_ms: since.as_secs_f32() * 1000.0,
            cov: self.coverage(sr.target_row),
            res,
            on_old,
        };
        self.active.as_mut().expect("open").frames.push(rec);
    }

    /// One key `key_ms` after the last: the frames until then run, the
    /// typed hint is armed at the key, the PTY answers `bytes`.
    fn press(&mut self, ch: char, bytes: &[u8]) {
        let t = self.last_key + Duration::from_millis(self.s.key_ms);
        self.advance_to(t);
        if self.keys_since_wrap == SNAP_KEYS {
            self.snap(true);
        }
        self.keys_since_wrap += 1;
        self.now = t;
        self.last_key = t;
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        self.glow.note_typed_glyph(t, 1, false, class);
        if self.lag_ms > 0 {
            self.advance_to(t + Duration::from_millis(self.lag_ms));
        }
        self.term.process(bytes);
    }

    /// The open census's [`Snap`]: the newest standing cell of each column
    /// on the row the text left and on the row it went to.
    fn snap(&mut self, typed_on: bool) {
        let Some(sr) = self.active.as_ref() else {
            return;
        };
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let walk = |row: u16| {
            let mut v: Vec<(u16, Instant, f32)> = rib
                .cells()
                .iter()
                .filter(|c| c.row == row && !c.leaving())
                .map(|c| (c.col, c.born, c.t))
                .collect();
            v.sort_unstable_by_key(|c| (c.0, std::cmp::Reverse(c.1)));
            v.dedup_by_key(|c| c.0);
            v.into_iter()
                .map(|(col, _, t)| (col, t))
                .collect::<Vec<_>>()
        };
        let snap = Snap {
            new_row: walk(sr.old_row),
            above: walk(sr.target_row),
            expect: if typed_on {
                SNAP_KEYS
            } else {
                self.lines.last().expect("line").chars().count()
            },
            typed_on,
        };
        self.active.as_mut().expect("open").snap = Some(snap);
    }

    /// One ordinary key's echo, as captured: the glyph on 1-based column
    /// `c` of the text row, addressed from home, the status row touched,
    /// the caret put after it.
    fn glyph_bytes(&self, ch: char, c: usize) -> Vec<u8> {
        let tr = text_row(self.s);
        format!(
            "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}C\x1b[{}B{ch}\x1b[{};1H\x1b[{tr};{}H\x1b[?25h{SYNC_END}",
            c - 1,
            tr - 1,
            self.s.rows,
            c + 1
        )
        .into_bytes()
    }

    /// A Space's echo: the caret moves and nothing is written.
    fn space_bytes(&self, c: usize) -> Vec<u8> {
        let tr = text_row(self.s);
        format!(
            "{SYNC_BEGIN}\x1b[?25l\x1b[{tr};{}H\x1b[?25h{SYNC_END}",
            c + 1
        )
        .into_bytes()
    }

    /// The whole box repainted with `self.lines`, bottom-anchored, in the
    /// captured first-wrap shape generalised to `n` text rows (module doc).
    fn box_bytes(&self) -> Vec<u8> {
        let tr = text_row(self.s);
        let rule_row = tr - self.lines.len();
        let mut s = format!(
            "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}B{}",
            rule_row - 1,
            rule(self.s.cols)
        );
        for (i, l) in self.lines.iter().enumerate() {
            let prefix = if i == 0 { "\u{276f}\u{a0}" } else { "  " };
            let _ = write!(s, "\r\x1b[1B{prefix}{l}\x1b[K");
        }
        let caret = 3 + self.lines.last().expect("line").chars().count();
        let _ = write!(
            s,
            "\x1b[{};1H\x1b[{tr};{caret}H\x1b[?25h{SYNC_END}",
            self.s.rows
        );
        s.into_bytes()
    }

    /// One key of the text; a key that wraps opens a wrap's census.
    fn key(&mut self, ch: char) {
        let width = self.s.cols - 4;
        let before = self.lines.last().expect("line").clone();
        if !append(&mut self.lines, ch, width) {
            let len = self.lines.last().expect("line").chars().count();
            let bytes = if ch == ' ' {
                self.space_bytes(3 + len - 1)
            } else {
                self.glyph_bytes(ch, 3 + len - 1)
            };
            self.press(ch, &bytes);
            return;
        }
        // THE WRAP: the line on the caret row moves one row up, minus the
        // moved word.
        self.wraps += 1;
        self.keys_since_wrap = 0;
        assert!(
            self.active.is_none(),
            "harness premise: no wrap inside an open census window ({WINDOW_MS} ms)"
        );
        if !self.census.is_empty() && !self.census.contains(&self.wraps) {
            let bytes = self.box_bytes();
            self.press(ch, &bytes);
            return;
        }
        let old_row = (text_row(self.s) - 1) as u16;
        let kept = self.lines[self.lines.len() - 2].chars().count();
        let last = self.lines.last().expect("line");
        let w = last.chars().count() - 1;
        let moved: String = last.chars().take(w).collect();
        let first = (w > 0)
            .then(|| OldWalk::at(self, old_row, (2 + kept + 1) as u16))
            .flatten();
        // The twins for dr = −1: every cell of the line was armed after
        // the previous wrap's chunk (or, for the first, after the first
        // paint), and the row above has not changed since — so the grid
        // NOW is the grid at arm time.
        let (mut own, mut above) = (Vec::new(), Vec::new());
        self.term.row_cols_into(usize::from(old_row), &mut own);
        self.term
            .row_cols_into(usize::from(old_row - 1), &mut above);
        let kept_cols = 2..(2 + kept) as u16;
        let twins: Vec<u16> = own
            .iter()
            .enumerate()
            .filter(|&(i, &c)| {
                kept_cols.contains(&(i as u16)) && c != ' ' && c != '\0' && above.get(i) == Some(&c)
            })
            .map(|(i, _)| i as u16)
            .collect();
        assert_eq!(
            before.chars().count(),
            width,
            "harness premise: the line wraps full"
        );
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let tracked: HashSet<(u16, Instant)> = rib
            .cells()
            .iter()
            .filter(|c| c.row == old_row && !c.leaving() && kept_cols.contains(&c.col))
            .map(|c| (c.col, c.born))
            .collect();
        assert!(
            tracked.len() + 2 >= kept,
            "harness premise: the moving line is laid under its text: {} of {kept}",
            tracked.len()
        );
        let bytes = self.box_bytes();
        let at = self.last_key + Duration::from_millis(self.s.key_ms);
        self.active = Some(Series {
            wrap_no: self.wraps,
            at,
            kept,
            w,
            moved,
            twins,
            snap: None,
            first,
            tracked,
            old_row,
            target_row: old_row - 1,
            counts0: self.counts(),
            counts1: (0, 0, 0),
            frames: Vec::new(),
        });
        self.press(ch, &bytes);
    }
}

/// Type `text` until `wraps` wraps have happened and each has had its
/// window — the hand typing on through every window.
fn run(s: Shape, text: &str, wraps: usize) -> Vec<Series> {
    let census: Vec<usize> = (1..=wraps).collect();
    run_census(s, text, &census, Hand::TypesOn)
}

/// Type `text` with a census opened at each wrap in `census` (ascending)
/// until the last of them has had its window — the hand typing on through
/// every window, or stopping at the last censused wrap's key.
fn run_census(s: Shape, text: &str, census: &[usize], hand: Hand) -> Vec<Series> {
    let last = *census.last().expect("a wrap to census");
    let mut h = Host::new(s);
    h.census = census.to_vec();
    for ch in text.chars() {
        h.key(ch);
        if h.wraps == last && (hand == Hand::Stops || h.active.is_none()) {
            break;
        }
    }
    if hand == Hand::Stops {
        // The stopped hand's reading of the new line (2026-09-24): the
        // relay lands at the held park's flush, not on a next key.
        h.idle(STOPPED_SNAP_MS);
        h.snap(false);
        h.idle(WINDOW_MS + 100 - STOPPED_SNAP_MS);
    } else {
        h.idle(WINDOW_MS + 100);
    }
    if let Some(mut sr) = h.active.take() {
        sr.counts1 = h.counts();
        h.done.push(sr);
    }
    assert_eq!(
        h.done.iter().map(|sr| sr.wrap_no).collect::<Vec<_>>(),
        census,
        "harness premise: the text wraps {last} times at {} columns",
        s.cols
    );
    h.done
}

/// **THE LAW, PER WRAP**: the line the box carried a row up FOLLOWED its
/// text and FLOWS there — never the melt.
///
/// * `ribbon_followed=` grew by the line (all of it but two cells), and
///   `ribbon_retired=` by no more than the moved word's old cells, the
///   space the wrap ate and a little slack — never by the row;
///   `ribbon_follow_missed=` did not grow;
/// * from the follow frame on, none of the line's own cells is left on the
///   row the text left;
/// * the row the text went to is lit well past `RETIRE_MELT_S` — still lit
///   at +800 ms — and dark by `FLOW_TOTAL_S` and two frames;
/// * no frame takes more than a tenth of the wrap frame's light: the melt
///   takes a quarter a frame.
fn assert_follows_and_flows(sr: &Series, what: &str) {
    let what = format!("{what}, wrap {}", sr.wrap_no);
    let curve = sr.curve();
    assert!(
        sr.followed() + 2 >= sr.kept as u64,
        "{what}: ribbon_followed= grew {} for a {}-cell line — the row did not follow its \
         text (retired {}, missed {}): {curve}",
        sr.followed(),
        sr.kept,
        sr.retired(),
        sr.missed()
    );
    assert!(
        sr.retired() <= (sr.w + 1 + 3) as u64,
        "{what}: ribbon_retired= grew {} — more than the moved word's {} cells, the eaten \
         space and slack: the row melted: {curve}",
        sr.retired(),
        sr.w
    );
    assert_eq!(
        sr.missed(),
        0,
        "{what}: ribbon_follow_missed= grew: {curve}"
    );
    assert!(
        sr.frames.iter().all(|f| f.on_old == 0),
        "{what}: the line's cells stayed on the row its text left: {curve}"
    );
    let lit_at = |ms: f32| sr.frames.iter().find(|f| f.t_ms >= ms).map(|f| f.cov);
    assert!(
        lit_at(800.0).is_some_and(|cov| cov > 0.0),
        "{what}: the row the text went to is still lit at +800 ms — {:.0} ms past the \
         melt: {curve}",
        800.0 - RETIRE_MELT_S * 1000.0
    );
    let last_lit = sr
        .frames
        .iter()
        .rposition(|f| f.cov > 0.0)
        .expect("lit at the wrap");
    let dark = sr
        .frames
        .get(last_lit + 1)
        .map(|f| f.t_ms)
        .expect("the row goes dark inside the window");
    assert!(
        dark <= FLOW_TOTAL_S * 1000.0 + 2.0 * FRAME_MS,
        "{what}: dark at {dark:.0} ms, past the flow's {:.0} ms: {curve}",
        FLOW_TOTAL_S * 1000.0
    );
    let cov0 = sr.frames.first().map_or(0.0, |f| f.cov);
    // Every frame, the last one too, takes at most a tenth of the row: the
    // content melt took 12.7-15.4 % a frame for eight frames. (The drift's
    // body leaves by its SHAPE over `FLOW_TAIL_S`, 2026-09-23 — there is no
    // step at the end to exempt.)
    let worst = sr
        .frames
        .windows(2)
        .map(|w| w[0].cov - w[1].cov)
        .fold(0.0f32, f32::max);
    // A one-cell kept row covers just 0.015 of this narrow window. Its
    // integer-alpha step can exceed one tenth of that tiny initial area by
    // about 0.001; a whole-cell cut (0.015) is still decisively red.
    assert!(
        cov0 > 0.0 && worst <= 0.1 * cov0 + 0.001,
        "{what}: the worst frame took {worst:.3} of a {cov0:.3} row — the cut: {curve}"
    );
    if let Some(snap) = &sr.snap {
        assert_the_new_line_continues(snap, sr.first, &what);
    }
}

/// **THE NEW LINE WALKS ITS OWN WALK** (2026-09-23, the review of the edge
/// twin — the owner: *"fix this rainbow cursor issue where the spectrum is
/// smooshed on the next line. I want smooth continuous rainbow"*), read
/// [`SNAP_KEYS`] keys after the wrap, or [`STOPPED_SNAP_MS`] after it with
/// the hand stopped (2026-09-24): no column of the new line carries the
/// stop the row above has at that column (the new line continues the walk
/// from the moved word; the row above is the walk BEFORE it), and every
/// column is the moved word's old walk continued, colour and pace
/// ([`assert_the_walk_continues`]). Typed on, no two adjacent columns step
/// more than [`FOLDED_STEP_MAX`] in the folded spectrum (those readings are
/// all past the walk's knee). The review measured a kept line's first cell
/// left on the caret row capturing the new line: row 27's columns 4..14 at
/// the row above's own stops, a folded step of 0.194 between columns 3 and
/// 4.
fn assert_the_new_line_continues(snap: &Snap, first: Option<OldWalk>, what: &str) {
    assert!(
        snap.new_row.len() + 2 >= snap.expect,
        "{what}: premise: the new line is laid under the hand: {:?}",
        snap.new_row
    );
    if let Some(first) = first {
        assert_the_walk_continues(&snap.new_row, first, what);
    }
    for &(col, t) in &snap.new_row {
        if let Some(&(_, above)) = snap.above.iter().find(|a| a.0 == col) {
            assert!(
                (t - above).abs() > 1e-4,
                "{what}: column {col} of the new line wears the row above's stop {above}: new \
                 {:?} / above {:?}",
                snap.new_row,
                snap.above
            );
        }
    }
    if !snap.typed_on {
        return;
    }
    let worst = snap
        .new_row
        .windows(2)
        .filter(|w| w[1].0 == w[0].0 + 1)
        .map(|w| (tri(w[1].1) - tri(w[0].1)).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst <= FOLDED_STEP_MAX,
        "{what}: the new line steps {worst:.3} of the folded spectrum between two columns: {:?}",
        snap.new_row
    );
}

/// **THE NEW ROW IS THE OLD ROW'S WALK CONTINUED** (2026-09-24 — the
/// owner: *"the spectrum is smooshed on the next line. I want smooth
/// continuous rainbow"*). Every lit column `c` of the new line (`(col, t)`)
/// carries exactly [`OldWalk::continued`]: the relaid word's first cell the
/// `t` its letter had on the old row, and each column after it one step on
/// along the SAME walk — the pace the old row had reached there, never a
/// walk restarted, never the old row's own stops re-read from its anchor.
fn assert_the_walk_continues(new_row: &[(u16, f32)], first: OldWalk, what: &str) {
    let want: Vec<(u16, f32)> = new_row
        .iter()
        .map(|&(col, _)| (col, first.continued(col).0))
        .collect();
    if let Some(&(col, t)) = new_row.first() {
        assert!(
            col == 2 && (t - first.t).abs() < 1e-4,
            "{what}: the relaid word's first cell is not the `t` {} its letter had on the old \
             row (distance {}): new {new_row:?}",
            first.t,
            first.d
        );
    }
    for (&(col, t), &(_, w)) in new_row.iter().zip(&want) {
        assert!(
            (t - w).abs() < 1e-4,
            "{what}: column {col} of the new line is {t}, not the old walk continued ({w}) — \
             the walk from `t` {} at distance {}: new {new_row:?} / continued {want:?}",
            first.t,
            first.d
        );
    }
}

/// How the hand goes on after the wrap a [`NewRow`] is read at.
#[derive(Clone, Copy, Debug)]
enum Then {
    /// It lifts at the key that wraps; the rows are read [`STOPPED_SNAP_MS`]
    /// on, past the held park's flush that replays the relay.
    Stops,
    /// It types this many more keys at its cadence, none of them wrapping
    /// again; the rows are read 300 ms after the last (its echo replayed).
    TypesOn(usize),
}

/// One wrap's two rows, read after the hand has stopped or typed on
/// ([`read_new_row`]).
#[derive(Clone, Debug)]
struct NewRow {
    /// The line the wrap carried a row up, minus the moved word.
    kept: usize,
    /// The moved word's length.
    w: usize,
    /// The walk the moved word had.
    first: OldWalk,
    /// The new line's text cells at the reading.
    len: usize,
    /// `(col, t)` of the newest standing cell of each column: the row the
    /// text left (the new line) and the row it went to.
    new_row: Vec<(u16, f32)>,
    above: Vec<(u16, f32)>,
    /// The cohort of each of `new_row`'s cells, and its walk's distance
    /// there (`Cohort::walk_d`).
    new_walk: Vec<(u32, f32)>,
}

/// Type `text` to its `wrap`-th wrap (no census is opened), read the walk
/// the moved word had just before the key that wraps ([`OldWalk::at`]),
/// press it, go on as `then` says, and read the two rows.
fn read_new_row(s: Shape, text: &str, wrap: usize, then: Then) -> NewRow {
    read_new_row_lagged(s, text, wrap, then, 0)
}

/// [`read_new_row`] with every key's echo processed `lag_ms` after its
/// press ([`Host::lag_ms`], 2026-09-24). A wrap that moves no word (`w ==
/// 0`: the line ended on the Space the wrap ate) has no moved letter to
/// read the walk off; its new line continues the walk from one past that
/// Space — two steps past the kept line's last glyph, C2's law for the
/// relay too ([`OldWalk::continued`] skips the eaten Space's stop).
fn read_new_row_lagged(s: Shape, text: &str, wrap: usize, then: Then, lag_ms: u64) -> NewRow {
    let mut h = Host::new(s);
    h.census = vec![usize::MAX];
    h.lag_ms = lag_ms;
    let width = s.cols - 4;
    let old_row = (text_row(s) - 1) as u16;
    let mut chars = text.chars();
    let (kept, w, first) = loop {
        let ch = chars
            .next()
            .expect("premise: the text reaches the wrap it is read at");
        let mut lines = h.lines.clone();
        if append(&mut lines, ch, width) && h.wraps + 1 == wrap {
            let kept = lines[lines.len() - 2].chars().count();
            let w = lines.last().expect("line").chars().count() - 1;
            let first = if w > 0 {
                OldWalk::at(&h, old_row, (2 + kept + 1) as u16)
                    .expect("premise: the moved word's first letter is lit on the old row")
            } else {
                let last = OldWalk::at(&h, old_row, (2 + kept - 1) as u16)
                    .expect("premise: the kept line's last glyph is lit on the old row");
                let d = last.d + 2.0;
                OldWalk {
                    t: last.zero + walk_t(d),
                    d,
                    zero: last.zero,
                }
            };
            h.key(ch);
            break (kept, w, first);
        }
        h.key(ch);
    };
    match then {
        Then::Stops => h.idle(STOPPED_SNAP_MS),
        Then::TypesOn(k) => {
            for _ in 0..k {
                let ch = chars.next().expect("premise: text to type on");
                let wraps = h.wraps;
                h.key(ch);
                assert_eq!(h.wraps, wraps, "premise: no key typed on wraps again");
            }
            h.idle(300);
        }
    }
    let rib = h.glow.v2_ribbon().expect("rainbow kitty owns the frame");
    let newest = |row: u16| {
        let mut v: Vec<_> = rib
            .cells()
            .iter()
            .filter(|c| c.row == row && !c.leaving())
            .copied()
            .collect();
        v.sort_unstable_by_key(|c| (c.col, std::cmp::Reverse(c.born)));
        v.dedup_by_key(|c| c.col);
        v
    };
    let new_cells = newest(old_row);
    let new_walk = new_cells
        .iter()
        .map(|c| {
            let coh = rib
                .cohorts()
                .iter()
                .find(|k| k.id == c.cohort)
                .expect("a cell's cohort");
            (c.cohort, coh.walk_d(c.col))
        })
        .collect();
    NewRow {
        kept,
        w,
        first,
        len: h.lines.last().expect("line").chars().count(),
        new_row: new_cells.iter().map(|c| (c.col, c.t)).collect(),
        above: newest(old_row - 1).iter().map(|c| (c.col, c.t)).collect(),
        new_walk,
    }
}

/// **THE LAW OF THE NEW ROW** (2026-09-24): the whole new line is lit —
/// the moved word, the wrap key's glyph and every key typed after it — and
/// it is the old row's walk continued ([`assert_the_walk_continues`]): the
/// relaid word's first cell at exactly the `t` AND the walk's distance its
/// letter had on the old row (C2), every column after it one step on at
/// the walk's own pace, and no column wearing the stop the row above has
/// at that column.
fn assert_new_row_continues(nr: &NewRow, what: &str) {
    let cols: Vec<u16> = nr.new_row.iter().map(|c| c.0).collect();
    let line: Vec<u16> = (2..2 + nr.len as u16).collect();
    assert_eq!(
        cols, line,
        "{what}: premise: the new line's {} cells are lit and nothing else stands on its row: \
         {:?}",
        nr.len, nr.new_row
    );
    assert_the_walk_continues(&nr.new_row, nr.first, what);
    let (cohort, d) = nr.new_walk[0];
    assert!(
        (d - nr.first.d).abs() < 1e-3,
        "{what}: the relaid word's first cell is at walk distance {d} (cohort {cohort}), not \
         the {} its letter had on the old row",
        nr.first.d
    );
    for &(col, t) in &nr.new_row {
        if let Some(&(_, above)) = nr.above.iter().find(|a| a.0 == col) {
            assert!(
                (t - above).abs() > 1e-4,
                "{what}: column {col} of the new line wears the row above's stop {above}: new \
                 {:?} / above {:?}",
                nr.new_row,
                nr.above
            );
        }
    }
}

/// **THE OWNER'S TEXT, 90 COLUMNS AT 60 MS, THREE GROWTHS, THE HAND TYPING
/// ON** — the capture's geometry and cadence, carried to a fourth text
/// row. Wraps 2 and 3 carry twins (the premise, asserted): `WHY` under
/// `HEY` among them.
///
/// RED before 2026-09-23, measured: wraps 2 and 3 followed `+0` and retired
/// `+86` each, the row gone at 140 and 134 ms.
#[test]
fn every_growth_of_the_owner_s_composer_follows_its_text_and_flows() {
    let wraps = run(OWNER_90, NATURAL, 3);
    for sr in &wraps[1..] {
        assert!(
            !sr.twins.is_empty(),
            "premise: wrap {} of the owner's text carries a twin on its line",
            sr.wrap_no
        );
    }
    for sr in &wraps {
        assert_follows_and_flows(sr, "the owner's text at 90 columns");
    }
}

/// **…AND WITH THE HAND STOPPED AT THE WRAP** (2026-09-23, the flow spec's
/// 13a idle variant, restored by the review): each growth of the owner's
/// text censused on its own, the hand lifting at the key that wraps — the
/// moved word's relay then arrives on the held park, not on the next key's
/// echo, and the line carried up must follow and flow all the same.
#[test]
fn every_growth_of_the_owner_s_composer_follows_and_flows_when_the_hand_stops_at_the_wrap() {
    for k in 1..=3 {
        let wraps = run_census(OWNER_90, NATURAL, &[k], Hand::Stops);
        assert_follows_and_flows(
            &wraps[0],
            "the owner's text at 90 columns, the hand stopped",
        );
    }
}

/// **THE LEAD'S GLASS GEOMETRY, 60 COLUMNS AT 130 MS, THREE GROWTHS** (two
/// until the review restored the flow spec's 13a matrix). Its first wrap
/// twins against the rule's label: the `o` of `got` stands under the `o`
/// of `workspace` (the premise, asserted).
///
/// RED before 2026-09-23, measured: wrap 1 followed `+0`, retired `+56`,
/// gone at 140 ms — the glass take's `ribbon_retired=58`.
#[test]
fn every_growth_at_the_glass_take_s_geometry_follows_its_text_and_flows() {
    let text = fit(NATURAL, GLASS_60.cols - 4);
    let wraps = run(GLASS_60, &text, 3);
    assert!(
        !wraps[0].twins.is_empty(),
        "premise: wrap 1 at 60 columns twins against the rule's label"
    );
    for sr in &wraps {
        assert_follows_and_flows(sr, "the owner's text at 60 columns, 130 ms");
    }
}

/// **THE CONTROL: THE SAME WRAPS WITH NO TWIN** — letters only, each line's
/// case alternating, so no glyph of a line stands under the same glyph on
/// the line above (asserted). GREEN before the fix: the follow pass was
/// never the problem where nothing twinned.
#[test]
fn the_no_twin_control_follows_and_flows_at_every_growth() {
    let text = alternating_case(NATURAL, OWNER_90.cols - 4);
    let wraps = run(OWNER_90, &text, 3);
    for sr in &wraps {
        assert!(
            sr.twins.is_empty(),
            "premise: the control's wrap {} has no twin: {:?}",
            sr.wrap_no,
            sr.twins
        );
        assert_follows_and_flows(sr, "the no-twin control");
    }
}

/// **ONE COINCIDENT LETTER DOES NOT CHANGE THE EXIT** — the A/B the
/// forensics proved the mechanism with. A is the no-twin control; B is the
/// same text with ONE letter of its second line changed to the letter the
/// first line has at that column: one interior twin at wrap 2, nothing
/// else different — the same lengths, wraps and cadence. Every wrap of
/// both follows and flows, and each of B's carries within two cells of
/// A's — three growths since the review restored the flow spec's 13a
/// matrix (the letter B changed is on the row above wrap 3's line too).
///
/// RED before 2026-09-23, measured: A's wrap 2 followed `+77` and flowed;
/// B's followed `+0`, retired `+86`, and was gone at 140 ms.
#[test]
fn one_coincident_letter_does_not_change_the_exit() {
    let width = OWNER_90.cols - 4;
    let a = alternating_case(NATURAL, width);
    let b = with_one_twin(&a, width, 1);
    let diff = a.chars().zip(b.chars()).filter(|(x, y)| x != y).count();
    assert_eq!(diff, 1, "premise: A and B differ in exactly one glyph");
    let wa = run(OWNER_90, &a, 3);
    let wb = run(OWNER_90, &b, 3);
    assert!(
        wa.iter().all(|sr| sr.twins.is_empty()) && wb[1].twins.len() == 1,
        "premise: one twin at B's wrap 2 and none in A: {:?} / {:?}",
        wa.iter().map(|sr| &sr.twins).collect::<Vec<_>>(),
        wb[1].twins
    );
    for (sa, sb) in wa.iter().zip(&wb) {
        assert_follows_and_flows(sa, "A, no twin");
        assert_follows_and_flows(sb, "B, one twin");
        assert!(
            sa.followed().abs_diff(sb.followed()) <= 2,
            "one letter changed the exit at wrap {}: A followed {}, B followed {}",
            sa.wrap_no,
            sa.followed(),
            sb.followed()
        );
    }
}

/// **THREE ROWS THAT START ALIKE** (2026-09-23, the review of the edge
/// twin). The owner's text with `WHY` → `HOW` and `the composer` → `HIS
/// composer`: its rows start `HEY` / `HOW` / the moved `HIS`, at wrap 2
/// the kept line's FIRST glyph stands under the same glyph (an edge twin,
/// the premise) and the moved word that Ink re-lays at the caret row's
/// first text column starts with it too, so that record reads as still
/// standing on its own row. The line follows WHOLE — none of its cells is
/// left on the caret row — and the new line walks its own walk
/// ([`assert_the_new_line_continues`]).
///
/// RED before the review's fix (the edge twin rode only while its glyph
/// read as gone), measured by the review at 90 columns and 60 ms: wrap 2
/// followed 82 of 83, one cell on the old row on every frame, and the new
/// row's columns 4..14 re-walked row 26's stops (a folded step of 0.194).
#[test]
fn a_line_whose_first_glyph_twins_above_and_below_follows_whole() {
    let text = NATURAL
        .replace("AUDIT WHY ATERM", "AUDIT HOW ATERM")
        .replace("when the composer", "when HIS composer");
    let wraps = run(OWNER_90, &text, 3);
    let sr = &wraps[1];
    assert!(
        sr.twins.first() == Some(&2) && sr.moved.starts_with('H'),
        "premise: wrap 2's line twins at its first column and the moved word `{}` starts \
         with the same glyph: twins {:?}",
        sr.moved,
        sr.twins
    );
    for sr in &wraps {
        assert!(
            sr.snap.is_some(),
            "premise: the hand typed on past wrap {}",
            sr.wrap_no
        );
        assert_follows_and_flows(sr, "rows that start alike, 90 columns");
    }
}

/// **A WRAP THAT MOVES MORE THAN IT KEEPS** (2026-09-23, the review of the
/// half-gate). At 20 columns (16 text cells) the owner's text wraps 5
/// keeps `this` (4 letters, the premise) and moves `confirmatio` (11): the
/// moved word's records are holes a row up only because the wrap relaid
/// them at the caret row's start, and counted against the four that
/// arrived they refused the follow (`2·4 < 15`). The line follows and
/// flows. The hand stops at the wrap: the next wrap would fall inside the
/// census window.
///
/// RED before the review's fix, measured by the review (at 600 ms keys and,
/// the census opened at wrap 5 alone, at 60–300 ms): followed `0`,
/// retired `16`, `ribbon_follow_missed=` `0`, the kept cells gone from the
/// old row at 134 ms and the row above never lit.
#[test]
fn a_narrow_composer_s_wrap_that_moves_more_than_it_keeps_follows() {
    let text = fit(NATURAL, NARROW_20.cols - 4);
    let wraps = run_census(NARROW_20, &text, &[5], Hand::Stops);
    let sr = &wraps[0];
    assert!(
        sr.kept == 4 && sr.moved == "confirmatio",
        "premise: wrap 5 keeps `this` and moves `confirmatio`: kept {}, moved `{}`",
        sr.kept,
        sr.moved
    );
    assert_follows_and_flows(sr, "the owner's text at 20 columns, wrap 5");
}

/// **…AND AT 60 COLUMNS, A PATH** (2026-09-23, the review of the half-gate):
/// `see` and then a 60-letter path typed at 130 ms — the wrap keeps `see`
/// and moves 52 letters of the path. RED before the review's fix, measured
/// by the review: the three kept cells melted on the old row at 140 ms,
/// `ribbon_follow_missed=0`.
#[test]
fn a_wrap_that_carries_a_long_path_down_keeps_its_short_line_following() {
    let text = "see crates/aterm-effects/src/rainbow_kitty/witness_follow_runs.rs";
    let wraps = run_census(GLASS_60, text, &[1], Hand::Stops);
    let sr = &wraps[0];
    assert!(
        sr.kept == 3 && sr.w == 52,
        "premise: the wrap keeps `see` and moves 52 letters: kept {}, w {}",
        sr.kept,
        sr.w
    );
    assert_follows_and_flows(sr, "`see` and a path at 60 columns");
}

/// The review's path shape (2026-09-24): `see` and a 61-letter path at 60
/// columns, 130 ms keys — the wrap keeps `see` (3) and moves 52 letters.
const PATH_60: &str = "see crates/aterm-effects/src/rainbow_kitty/witness_follow_runs.rs";

/// The review's shape at the owner's width (2026-09-24): `please read`
/// and an 83-letter path at 90 columns, 60 ms keys — the wrap keeps 11 and
/// moves 74 letters.
const PATH_90: &str = "please read crates/aterm-effects/src/rainbow_kitty/witness_follow_runs_and_the_relaid_suffix.rs now";

/// **A WRAP THAT MOVES A WORD AT LEAST AS LONG AS THE LINE IT KEEPS — THE NEW
/// ROW CONTINUES THE OLD ROW'S WALK** (2026-09-24, the re-review of the
/// landing: *"F1 is still open where a wrap moves a word at least as long
/// as the line it keeps"*). Whenever `kept ≤ w` the relaid word covers the
/// column of the Space the wrap ate. That cell was never armed (a blank),
/// and the follow split left it on the caret row in the old line's run;
/// the witness then ARMED it under the relaid glyph in the very walk that
/// melted its run, so it stood — and `join_cohort` kept laying the new row
/// into that run from its own anchor: red again on the `1/16` fast leg,
/// the row above's stops column for column (the owner's smoosh). With the
/// hand stopped the relay also came too late for its own origin: the held
/// park replays it at ≤ 0.25 s, and the moved word's old cells had melted
/// at 0.12 s.
///
/// RED before the fix, measured by the review with this harness: the path
/// at 60 columns laid cols 2.. at `0.000, 0.062, 0.125, …` in cohort 0 (the
/// row above `see` reads `0.000, 0.0625, 0.125`), where its letter had
/// `0.250` at distance 4.
#[test]
fn a_long_word_s_wrap_continues_the_old_row_s_walk_when_the_hand_stops() {
    let nr = read_new_row(GLASS_60, PATH_60, 1, Then::Stops);
    assert!(
        nr.kept == 3 && nr.w == 52,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(&nr, "`see` and a path at 60 columns, the hand stopped");
}

/// …the same wrap, the hand typing three keys on. RED before the fix,
/// measured: the relay minted on its walk, but the eaten Space's cell kept
/// the old stop `0.188` at column 5 between `0.375` and `0.500`, and the
/// wrap key and the keys after it joined the old line's run at its own
/// stops.
#[test]
fn a_long_word_s_wrap_continues_the_old_row_s_walk_as_the_hand_types_on() {
    let nr = read_new_row(GLASS_60, PATH_60, 1, Then::TypesOn(3));
    assert!(
        nr.kept == 3 && nr.w == 52,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(&nr, "`see` and a path at 60 columns, typed on");
}

/// The narrow composer (20 columns, 130 ms): wrap 5 keeps `this` (4) and
/// moves `confirmatio` (11). RED before the fix, measured by the review:
/// the eaten Space at column 6 survived in cohort 11 and `confirmation`
/// was laid into it at `1.944, 1.972, 2.000, …` — row 26's `this` stops.
#[test]
fn a_narrow_wrap_continues_the_old_row_s_walk_when_the_hand_stops() {
    let text = fit(NATURAL, NARROW_20.cols - 4);
    let nr = read_new_row(NARROW_20, &text, 5, Then::Stops);
    assert!(
        nr.kept == 4 && nr.w == 11,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(
        &nr,
        "the owner's text at 20 columns, wrap 5, the hand stopped",
    );
}

/// …typing ` mes` on (the line fills to 16 cells, no wrap). RED before the
/// fix, measured by the review: column 6 kept `2.0556` between `2.1667`
/// and `2.2222`, and the wrap key and ` mes` joined cohort 11 at
/// `2.2500..2.3611`.
#[test]
fn a_narrow_wrap_continues_the_old_row_s_walk_as_the_hand_types_on() {
    let text = fit(NATURAL, NARROW_20.cols - 4);
    let nr = read_new_row(NARROW_20, &text, 5, Then::TypesOn(4));
    assert!(
        nr.kept == 4 && nr.w == 11,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(&nr, "the owner's text at 20 columns, wrap 5, typed on");
}

/// The owner's width (90 columns, 60 ms): `please read` and a 74-letter
/// run of a path moved. RED before the fix, measured by the review: column
/// 13 survived and the new row was laid into cohort 0 from `t` 0.000.
#[test]
fn a_path_s_wrap_at_the_owner_s_width_continues_the_old_row_s_walk_when_the_hand_stops() {
    let nr = read_new_row(OWNER_90, PATH_90, 1, Then::Stops);
    assert!(
        nr.kept == 11 && nr.w == 74,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(
        &nr,
        "`please read` and a path at 90 columns, the hand stopped",
    );
}

/// …typing three keys on. RED before the fix, measured by the review:
/// column 13 kept the old stop `0.6875` between `1.1667` and `1.2222`, and
/// the keys after the relaid path joined cohort 0 at `2.6111, 2.6389, …`.
#[test]
fn a_path_s_wrap_at_the_owner_s_width_continues_the_old_row_s_walk_as_the_hand_types_on() {
    let nr = read_new_row(OWNER_90, PATH_90, 1, Then::TypesOn(3));
    assert!(
        nr.kept == 11 && nr.w == 74,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(&nr, "`please read` and a path at 90 columns, typed on");
}

/// **THE CONTROL: A WRAP THAT KEEPS MORE THAN IT MOVES** — the owner's own
/// capture, `…AUDIT WH|Y` at 90 columns: `kept` 84 against `w` 2, so the
/// relaid word never reaches the eaten Space's column and the moved word's
/// old cells go blank (released, still resident when the relay reads
/// them). GREEN before the fix, both hands: the law above is not stricter
/// than the shape that always worked.
#[test]
fn the_control_a_wrap_that_keeps_more_than_it_moves_continues_the_walk() {
    for then in [Then::Stops, Then::TypesOn(12)] {
        let nr = read_new_row(OWNER_90, NATURAL, 1, then);
        assert!(nr.kept > nr.w, "premise: kept {} > w {}", nr.kept, nr.w);
        assert_new_row_continues(&nr, &format!("the owner's capture at 90 columns, {then:?}"));
    }
}

/// **TWO SPACES BEFORE THE MOVED WORD** (2026-09-24): `see`, two Spaces and
/// the path at 60 columns, the hand stopped. The kept line's own trailing
/// Space stays behind with the eaten one, so the copy the follow pass
/// carried up ends two cells short of the moved word. The relay comes at
/// the held park's flush, after the word's old cells melted; with no cell
/// to read the word's walk off, it continued whatever run was freshest.
/// RED before the relay read the split run's walk off its copy, measured:
/// the relaid word from `0.250` (distance 4) where its first letter had
/// `0.3125` (distance 5) — every letter one step behind, wearing its
/// predecessor's colour.
#[test]
fn two_spaces_before_the_moved_word_continue_the_walk_when_the_hand_stops() {
    let text = PATH_60.replacen(' ', "  ", 1);
    let nr = read_new_row(GLASS_60, &text, 1, Then::Stops);
    assert!(
        nr.kept == 4 && nr.w == 51,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(
        &nr,
        "`see`, two Spaces and a path at 60 columns, the hand stopped",
    );
}

/// A narrow pane of ODD width: 21 columns, 17 text cells, 130 ms keys.
const NARROW_21: Shape = Shape {
    rows: 30,
    cols: 21,
    cw: 15,
    ch: 28,
    key_ms: 130,
};

/// **A WRAP THAT MOVES EXACTLY AS MUCH AS IT KEEPS** (2026-09-24): at an
/// odd width `kept == w` is possible — `friendly` (8) kept, `neighbor` (8)
/// moved — and then no relaid LETTER covers the eaten Space's column: the
/// moved word's old cells go blank (the word, found relaid on its own row,
/// is moved text, so its run is still retired) and the wrap key's own
/// glyph is what lands on the Space. RED without the witness's landed
/// rule, measured: the new row laid from `0.0` on the old run's own walk,
/// the Space's column at its old stop `0.5`, where `neighbor`'s first
/// letter had `0.5625` at distance 9.
#[test]
fn a_wrap_that_moves_exactly_as_much_as_it_keeps_continues_the_walk() {
    for then in [Then::Stops, Then::TypesOn(3)] {
        let nr = read_new_row(NARROW_21, "friendly neighbors wave", 1, then);
        assert!(
            nr.kept == 8 && nr.w == 8,
            "premise: kept {} w {}",
            nr.kept,
            nr.w
        );
        assert_new_row_continues(
            &nr,
            &format!("`friendly neighbor|s` at 21 columns, {then:?}"),
        );
    }
}

/// [`NARROW_21`] at the owner's cadence, 60 ms keys.
const NARROW_21_60: Shape = Shape {
    key_ms: 60,
    ..NARROW_21
};

/// **THE NEW LINE NEVER HEALS INTO THE DEAD REMNANT OF THE OLD ONE**
/// (2026-09-24, the review of the landing). The follow split leaves the
/// old line's cohort on the caret row holding the eaten Space and the moved
/// word's old cells — every one of them stamped to melt, resident for
/// `RETIRE_MELT_S`. A new line that reaches the remnant's first column while
/// it is still resident abuts it, and `Ribbon::heal_seams` merged any two
/// live cohorts that abut: both are anchored at the text inset, so the
/// OLDER remnant's origin survived and every new-line cell was re-stamped
/// on the old walk — the row above's stops, red on the fast leg. At 60 ms
/// keys the new line reaches it on the relay itself (`kept == w`) or one or
/// two keys on (`kept − w` of 2 or 3).
///
/// RED before the fix, measured: `friendly neighbor|s` at 21 columns laid
/// `(2, 0.0) (3, 0.0625) … (9, 0.4375) (10, 1.0278) (11, 1.0556) (12,
/// 0.625)`, where `neighbor`'s first letter had `0.5625` at distance 9.
#[test]
fn a_new_line_that_reaches_the_old_line_s_melting_remnant_keeps_its_own_walk() {
    let nr = read_new_row(NARROW_21_60, "friendly neighbors wave", 1, Then::TypesOn(3));
    assert!(
        nr.kept == 8 && nr.w == 8,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(&nr, "`friendly neighbor|s` at 21 columns, 60 ms, typed on");
}

/// …at 24 columns, `hello there wonderfu|l` (kept 11, w 8): the second key
/// after the wrap reaches the remnant. RED before the fix, measured: the new
/// row laid `0.0, 0.0625 … 0.875` over columns 2..16, where `w` had `0.75`
/// at distance 12.
#[test]
fn a_new_line_two_keys_short_of_the_remnant_keeps_its_own_walk() {
    let s = Shape {
        rows: 30,
        cols: 24,
        cw: 15,
        ch: 28,
        key_ms: 60,
    };
    let nr = read_new_row(
        s,
        "hello there wonderful day to you all",
        1,
        Then::TypesOn(6),
    );
    assert!(
        nr.kept == 11 && nr.w == 8,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(
        &nr,
        "`hello there wonderfu|l` at 24 columns, 60 ms, typed on",
    );
}

/// …at 40 columns, English (kept 19, w 16). RED before the fix, measured:
/// the new row from `0.0` to `1.1667`, where the moved word had `1.1111` at
/// distance 20.
#[test]
fn a_new_line_three_keys_short_of_the_remnant_at_forty_columns_keeps_its_own_walk() {
    let s = Shape {
        rows: 30,
        cols: 40,
        cw: 15,
        ch: 28,
        key_ms: 60,
    };
    let nr = read_new_row(
        s,
        "please use the apis internationalization now",
        1,
        Then::TypesOn(6),
    );
    assert!(
        nr.kept == 19 && nr.w == 16,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(&nr, "kept 19 / moved 16 at 40 columns, 60 ms, typed on");
}

/// …at the owner's width (90 columns, 60 ms), a path (kept 44, w 41). RED
/// before the fix, measured: the new row from `0.0`, where the path's first
/// letter had `1.8056` at distance 45.
#[test]
fn a_new_line_three_keys_short_of_the_remnant_at_the_owner_s_width_keeps_its_own_walk() {
    let text = "please read the whole of this long file nows crates/aterm-effects/src/rainbow_kitty/ribbon_follow_the_text.rs now";
    let nr = read_new_row(OWNER_90, text, 1, Then::TypesOn(4));
    assert!(
        nr.kept == 44 && nr.w == 41,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(&nr, "kept 44 / moved 41 at 90 columns, 60 ms, typed on");
}

/// …and at 130 ms, the owner's own text at 21 columns, wrap 4 (`got this` /
/// `confirma`, kept == w). RED before the fix, measured: the relaid word
/// read `1.833..2.028`, the row above's values, columns 10..11 `2.306 /
/// 2.333`, and column 12 dropped back to `2.111`.
#[test]
fn the_owner_s_text_at_21_columns_keeps_its_own_walk_past_the_remnant() {
    let text = fit(NATURAL, NARROW_21.cols - 4);
    let nr = read_new_row(NARROW_21, &text, 4, Then::TypesOn(2));
    assert!(
        nr.kept == 8 && nr.w == 8,
        "premise: kept {} w {}",
        nr.kept,
        nr.w
    );
    assert_new_row_continues(&nr, "the owner's text at 21 columns, wrap 4, typed on");
}

/// **A SPACE TYPED BEFORE THE WRAP'S ECHO IS NOT THE WORD'S SPACE**
/// (2026-09-24, the review of the landing). The owner's text at 90 columns,
/// 60 ms keys, each echo `lag` ms behind its press: the key after each of
/// the three wrap keys is a Space, and a frame falls between its press and
/// its echo. The Space is held, not laid (`Ribbon::hold_typed`), and it was
/// remembered as the row's last Space at the mirror — still the pre-wrap
/// caret, because the wrap's own backward move was held by the park. The
/// relay then measured the moved word from that Space: zero cells, and the
/// word was never relaid.
///
/// RED before the fix, measured: at `lag` 20 the new row was dark under
/// `WH` (columns 2..3), `th` and `stead` at +300 ms, the bed's quads at
/// zero alpha there; lit from the wrap key's glyph on.
#[test]
fn a_space_typed_before_the_wrap_s_echo_lands_leaves_the_moved_word_relaid() {
    for lag in [10, 20, 40] {
        for wrap in 1..=3 {
            let nr = read_new_row_lagged(OWNER_90, NATURAL, wrap, Then::TypesOn(12), lag);
            assert!(nr.w > 0, "premise: wrap {wrap} moves a word");
            assert_new_row_continues(
                &nr,
                &format!("the owner's text at 90 columns, echo lag {lag} ms, wrap {wrap}"),
            );
        }
    }
}

/// …THE CONTROL: the same wraps with a LETTER after the first wrap key
/// (`WHYX`) — green before the fix, at every lag.
#[test]
fn the_control_a_letter_typed_before_the_wrap_s_echo_lands() {
    let text = NATURAL.replacen("AUDIT WHY ATERM", "AUDIT WHYX ATERM", 1);
    for lag in [10, 20, 40] {
        let nr = read_new_row_lagged(OWNER_90, &text, 1, Then::TypesOn(12), lag);
        assert!(nr.w == 2, "premise: wrap 1 moves `WH`: w {}", nr.w);
        assert_new_row_continues(&nr, &format!("`WHYX` at 90 columns, echo lag {lag} ms"));
    }
}

/// **A WRAP THAT MOVES NO WORD CONTINUES ONE PAST THE SPACE IT ATE**
/// (2026-09-24, the review of the landing). The owner's text at 60 columns
/// and 60 ms keys wraps first on `…I got this ` + `c`: the line ends on the
/// Space the wrap eats (kept 55, w 0), and the follow split leaves that
/// Space's cell on the caret row as a remnant with the same clock as the
/// copy it carried up. With the hand stopped and each echo a frame or more
/// behind its press, the wrap key's cell is minted on the continuation arm
/// of `Ribbon::join_cohort`, whose freshest-cohort pick TIED between the
/// two and took the copy — the walk at the eaten Space's own stop, one step
/// behind.
///
/// RED before the fix, measured: new column 2 at `2.0833` (distance 55)
/// with a 20 ms lag, where lag 0 lays `2.1111` (distance 56).
#[test]
fn a_wrap_that_moves_no_word_continues_one_past_the_eaten_space_whatever_the_echo_lag() {
    let s = Shape {
        key_ms: 60,
        ..GLASS_60
    };
    for lag in [0, 20, 40] {
        let nr = read_new_row_lagged(s, NATURAL, 1, Then::Stops, lag);
        assert!(
            nr.kept == 55 && nr.w == 0,
            "premise: kept {} w {}",
            nr.kept,
            nr.w
        );
        assert_new_row_continues(
            &nr,
            &format!("`…I got this |c` at 60 columns, echo lag {lag} ms, the hand stopped"),
        );
    }
    // …and at the owner's width: a line of eighty-five cells and the Space
    // that fills it, then a key.
    let text = format!("{}flows next line of text", "rainbow ".repeat(10));
    for lag in [0, 20] {
        let nr = read_new_row_lagged(OWNER_90, &text, 1, Then::Stops, lag);
        assert!(
            nr.kept == 85 && nr.w == 0,
            "premise: kept {} w {}",
            nr.kept,
            nr.w
        );
        assert_new_row_continues(
            &nr,
            &format!("a full line at 90 columns, echo lag {lag} ms, the hand stopped"),
        );
    }
}

/// `to introspection|s` at 20 columns: the wrap keeps TWO cells.
const KEPT_2: &str = "to introspections are fun";

/// `I introspections|s` at 20 columns: the wrap keeps ONE cell.
const KEPT_1: &str = "I introspectionss are fun";

/// **A WRAP THAT KEEPS ONE OR TWO CELLS IS STILL A WRAP** (2026-09-24, the
/// review of the landing). The composer wrap's same-row caret move is
/// exactly `kept` cells back, and the ribbon took a same-row backward typed
/// move as a re-anchor only from `RE_ANCHOR_MIN_CELLS` (3) back — the
/// host's own floor, which exists for the flushed one-cell park. Under it
/// there was no relay: the moved word's old cells melted in
/// `RETIRE_MELT_S` and its cells on the new row were never lit.
///
/// RED before the fix, measured: kept 2 at 20 columns lit none of the new
/// row's 14 cells with the hand stopped and only `(16, 1.0)` typed on; `cd`
/// and a path at 90 columns lit 2 of 86.
#[test]
fn a_wrap_that_keeps_two_cells_relays_the_moved_word() {
    for then in [Then::Stops, Then::TypesOn(1)] {
        let nr = read_new_row(NARROW_20, KEPT_2, 1, then);
        assert!(
            nr.kept == 2 && nr.w == 13,
            "premise: kept {} w {}",
            nr.kept,
            nr.w
        );
        assert_new_row_continues(
            &nr,
            &format!("`to introspection|s` at 20 columns, {then:?}"),
        );
    }
    let text = "cd crates/aterm-effects/src/rainbow_kitty/witness_follow_runs_and_the_relaid_suffix_of_every_wrap.rs now";
    let nr = read_new_row(OWNER_90, text, 1, Then::TypesOn(2));
    assert!(nr.kept == 2, "premise: kept {} w {}", nr.kept, nr.w);
    assert_new_row_continues(&nr, "`cd` and a path at 90 columns, typed on");
}

/// …AND ONE CELL (2026-09-24): `I introspections|s` keeps `I`. RED before
/// the fix, measured: every cell of the line was stamped to melt on the
/// wrap frame — the follow pass needs two found records — the row above
/// never lit, and the new row stayed dark: the abrupt vanish.
#[test]
fn a_wrap_that_keeps_one_cell_relays_the_moved_word() {
    for then in [Then::Stops, Then::TypesOn(1)] {
        let nr = read_new_row(NARROW_20, KEPT_1, 1, then);
        assert!(
            nr.kept == 1 && nr.w == 14,
            "premise: kept {} w {}",
            nr.kept,
            nr.w
        );
        assert_new_row_continues(
            &nr,
            &format!("`I introspections|s` at 20 columns, {then:?}"),
        );
    }
}

/// The repaint may arrive 120 ms after its key, just before the next key's
/// 130 ms press. The content proof must still reach the held park and its
/// delayed flush, including when that next key types onto the new row.
#[test]
fn a_short_wrap_relay_survives_a_delayed_echo() {
    for (text, kept) in [(KEPT_1, 1), (KEPT_2, 2)] {
        for then in [Then::Stops, Then::TypesOn(1)] {
            let nr = read_new_row_lagged(NARROW_20, text, 1, then, 120);
            assert_eq!(nr.kept, kept);
            assert_new_row_continues(&nr, &format!("kept {kept}, 120 ms lag, {then:?}"));
        }
    }
}

/// …and the line it keeps FOLLOWS and FLOWS, one cell or two
/// ([`assert_follows_and_flows`], the hand stopped at the wrap). RED before
/// the fix, measured at kept 1: followed `0`, retired `16`, the row above
/// dark through +493 ms.
#[test]
fn a_wrap_that_keeps_one_or_two_cells_follows_and_flows() {
    for (text, kept) in [(KEPT_1, 1), (KEPT_2, 2)] {
        let wraps = run_census(NARROW_20, text, &[1], Hand::Stops);
        let sr = &wraps[0];
        assert!(
            sr.kept == kept,
            "premise: the wrap keeps {kept}: kept {}",
            sr.kept
        );
        assert!(
            sr.followed() >= kept as u64,
            "kept {kept}: ribbon_followed= grew {} — the kept line did not follow its text: {}",
            sr.followed(),
            sr.curve()
        );
        assert_follows_and_flows(sr, &format!("a wrap that keeps {kept} at 20 columns"));
    }
}

/// A real one-cell wrap proves its held park and relays the moved word.
/// Within 300 ms, a later typed hint with a one-cell caret retreat but no
/// content move is still an ordinary false park: the earlier proof cannot
/// be borrowed to drain or relay the new row.
#[test]
fn a_proved_short_wrap_does_not_reanchor_a_later_false_park() {
    // Tier-1: drive the real terminal/host, and project its held, followed,
    // flushed and relayed observations onto the derived park protocol.
    let model = rainbow_short_wrap_park_model();
    let held = model.successors("Hold", &model.init_state())[0].clone();
    let mut h = Host::new(NARROW_20);
    h.census = vec![usize::MAX];
    for ch in KEPT_1.chars() {
        h.key(ch);
        if h.wraps == 1 {
            break;
        }
    }
    let old_row = (text_row(NARROW_20) - 1) as u16;
    h.idle(20);
    assert!(h.counts().0 >= 1, "the short row followed its content");
    assert_eq!(h.glow.in_flight_tally().park_flushed, 0);
    let exact = model.successors("FollowExact", &held)[0].clone();
    assert!(!model.action_enabled("CancelSameEnd", &exact));
    h.idle(240);
    let standing = |h: &Host| {
        let rib = h.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let mut cols: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|c| c.row == old_row && !c.leaving())
            .map(|c| c.col)
            .collect();
        cols.sort_unstable();
        cols.dedup();
        cols
    };
    let before = standing(&h);
    assert_eq!(
        before,
        (2..=16).collect::<Vec<_>>(),
        "the relay lit every glyph"
    );
    let flushed = h.glow.in_flight_tally().park_flushed;
    assert_eq!(flushed, 1, "the proved wrap was judged");
    let queued = model.successors("FlushLicensed", &exact)[0].clone();
    assert_eq!(queued.get("armed"), Some(&1));
    let relayed = model.successors("Replay", &queued)[0].clone();
    assert_eq!(relayed.get("relayed"), Some(&1));

    // The new park starts while the old wrap's evidence would still be
    // inside a 300 ms row cache. Its row content has not moved at all.
    let at = h.now + Duration::from_millis(1);
    h.advance_to(at);
    h.glow.note_typed_glyph(at, 1, false, TypedClass::Glyph);
    let cur = h.term.cursor();
    assert_eq!(cur.row, old_row);
    assert!(cur.col > 2);
    h.term
        .process(format!("\x1b[{};{}H", cur.row + 1, cur.col).as_bytes());
    h.frame();
    assert_eq!(h.glow.in_flight_tally().park_flushed, flushed);
    let false_park = model.successors("Hold", &relayed)[0].clone();
    assert!(model.action_enabled("CancelSameEnd", &false_park));
    h.idle(260);
    assert_eq!(h.glow.in_flight_tally().park_flushed, flushed + 1);
    let unproved = model.successors("FlushLicensed", &false_park)[0].clone();
    assert_eq!(unproved.get("armed"), Some(&0));
    assert_eq!(
        standing(&h),
        before,
        "the false park drained or relaid the row"
    );
}
