// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every growth of Claude Code's composer box follows and flows: the line the box carries a row up keeps its rainbow, which drifts and fades rather than vanishing, and the new row continues the old row's walk.

use crate::common::{
    caret_sample, note_blink, observe_caret_row, observe_ribbon_rows, rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::meteor::tri;
use aterm_effects::rainbow_kitty::ribbon::{FLOW_TOTAL_S, RETIRE_MELT_S, walk_t};
use aterm_models_effects::rainbow_short_wrap_park_model;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

const FRAME_US: u64 = 16_667;
const FRAME_MS: f32 = FRAME_US as f32 / 1000.0;
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
/// The census window after each wrap: past `FLOW_TOTAL_S` with room.
const WINDOW_MS: u64 = 1600;
/// Keys typed after a wrap before the new row's walk is read.
const SNAP_KEYS: usize = 12;
/// The largest step one column may take in the folded spectrum (`tri(t)`, as
/// `Ink::sample` reads it) on a row past its walk's knee;
/// [`WALK_LAY_RATE`](aterm_effects::rainbow_kitty::ribbon::WALK_LAY_RATE) is
/// `1/36 ≈ 0.028`.
const FOLDED_STEP_MAX: f32 = 0.03;
/// When a stopped-hand census reads the new line: after the held park's flush,
/// which replays the wrap key's echo and the relay no later than 0.25 s on.
const STOPPED_SNAP_MS: u64 = 400;

#[derive(Clone, Copy, Debug)]
struct Shape {
    rows: usize,
    cols: usize,
    cw: usize,
    ch: usize,
    key_ms: u64,
}

/// The capture's window: 90×30, the retina cell, keys at 60 ms.
const OWNER_90: Shape = Shape {
    rows: 30,
    cols: 90,
    cw: 15,
    ch: 28,
    key_ms: 60,
};

/// A glass take: 60 columns, keys at 130 ms.
const GLASS_60: Shape = Shape {
    rows: 30,
    cols: 60,
    cw: 15,
    ch: 28,
    key_ms: 130,
};

/// A narrow composer, 20 columns (16 text cells) at 130 ms: words are long
/// against the row, so a wrap can move more letters than it keeps.
const NARROW_20: Shape = Shape {
    rows: 30,
    cols: 20,
    cw: 15,
    ch: 28,
    key_ms: 130,
};

/// Whether the hand types on through each wrap's census window or stops at the
/// censused wrap's key (the only way to census a wrap whose successor falls
/// inside its window).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hand {
    TypesOn,
    Stops,
}

/// Text long enough for three wraps at 90 columns. `…AUDIT WH|Y` puts `WHY`
/// under `HEY`, a twin.
const NATURAL: &str = "HEY!!! [Image #1] in the other tab in aterm, I got this confirmation message. AUDIT WHY ATERM claude code harness introspection interface keeps losing the rainbow when the composer grows to a third row and then a fourth row while I keep typing at a steady pace so please make the old line flow and drift and fade away beautifully instead of vanishing at once like it does now";

/// `tests/composer_box_growth_wrap.rs`'s config: rainbow kitty at 0.70 on
/// the default dark theme.
fn cfg() -> GlowConfig {
    GlowConfig {
        theme_fg: 0x00D0_D0D0,
        theme_bg: 0x0011_1318,
        ..rainbow_cfg()
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

/// Greedy word wrap of one appended char at `width` text cells; returns whether
/// the line count grew.
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

/// `text` with every Space that would land on a wrap dropped (the premise of [`append`]), so
/// the words either side of it join; a no-op on a text that never puts a Space there.
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

/// The control text: letters and spaces only, each char's case set by the
/// parity of the final line it wraps onto, so no glyph stands under the same
/// glyph on the line above (no `dr = −1` twins).
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

/// `text` (already fitted) with one glyph of final line `k`, nearest the line's
/// middle, changed to the glyph above it, arming exactly one `dr = −1` twin
/// inside the block.
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
    /// …on the row the text left (the caret's row).
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
    /// The `dr = −1` twins of the moving line: columns where it stood under the
    /// same glyph on the row above when armed, first and last columns included.
    twins: Vec<u16>,
    /// The two rows' walks [`SNAP_KEYS`] keys after the wrap (hand typing on) or
    /// [`STOPPED_SNAP_MS`] after it (hand stopped).
    snap: Option<Snap>,
    /// The walk the moved word's first letter had on the row it came off
    /// ([`OldWalk`]); `None` when the wrap moved no word.
    first: Option<OldWalk>,
    /// The moving line's cells: `(col, born)`.
    tracked: HashSet<(u16, Instant)>,
    old_row: u16,
    target_row: u16,
    counts0: (u64, u64, u64),
    counts1: (u64, u64, u64),
    frames: Vec<FrameRec>,
}

/// The walks of the new line and of the row the text went to, read after a
/// wrap: `(col, t)` of each column's newest standing cell.
#[derive(Clone, Debug)]
struct Snap {
    new_row: Vec<(u16, f32)>,
    above: Vec<(u16, f32)>,
    /// Cells the new line must show lit (two may be missing): the keys typed
    /// after the wrap, or with the hand stopped, the moved word and the wrap
    /// key's glyph.
    expect: usize,
    /// The hand typed on to this reading (it stopped at the wrap otherwise).
    typed_on: bool,
}

/// The walk the moved word had, read off the old row just before the wrapping
/// key: the `t` of the cell under its first letter, the walk distance there
/// (`Cohort::walk_d`) and the walk's zero (`Cohort::walk_zero`).
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
    /// How long after its press a key's PTY bytes are processed (`0` unless a
    /// test sets it).
    lag_ms: u64,
}

impl Host {
    /// Claude Code's screen before the first key: alt screen, rule above the
    /// text row, prompt marker, status row, caret at the inset; one frame
    /// presented.
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
            g: crate::common::geom(s.rows, s.cols, s.cw, s.ch),
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

    /// The frame hold then the tick (as `tests/composer_box_growth_wrap.rs`
    /// `Host::frame`), then the census while a wrap's window is open.
    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink_seen, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
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

    /// One ordinary key's echo as captured: the glyph at 1-based column `c` of
    /// the text row, addressed from home, the status row touched, the caret
    /// after it.
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
    /// captured first-wrap shape generalised to `n` text rows.
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

/// Type `text` with a census opened at each wrap in `census` (ascending) until
/// the last has had its window, typing on or stopping at the last censused
/// wrap's key.
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

/// Per wrap: the line carried a row up followed its text and flows there.
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
    let worst = sr
        .frames
        .windows(2)
        .map(|w| w[0].cov - w[1].cov)
        .fold(0.0f32, f32::max);
    assert!(
        cov0 > 0.0 && worst <= 0.1 * cov0 + 0.001,
        "{what}: the worst frame took {worst:.3} of a {cov0:.3} row — the cut: {curve}"
    );
    if let Some(snap) = &sr.snap {
        assert_the_new_line_continues(snap, sr.first, &what);
    }
}

/// The new line walks its own walk, read after the wrap (typing on or
/// stopped): no column carries the row above's stop at that column, and every
/// column continues the moved word's old walk ([`assert_the_walk_continues`]).
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

/// The new row is the old row's walk continued: every lit column carries exactly
/// [`OldWalk::continued`], the relaid first cell at its letter's old `t` and each later column
/// one step on along the same walk.
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

/// Type `text` to its `wrap`-th wrap without a census, read the moved word's
/// walk just before the wrapping key ([`OldWalk::at`]), press it, continue as
/// `then` says, and read the two rows.
fn read_new_row(s: Shape, text: &str, wrap: usize, then: Then) -> NewRow {
    read_new_row_lagged(s, text, wrap, then, 0)
}

/// [`read_new_row`] with every echo processed `lag_ms` after its press
/// ([`Host::lag_ms`]).
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

/// The new row law: the whole new line is lit (moved word, wrap key's glyph and
/// later keys) and continues the old row's walk ([`assert_the_walk_continues`]),
/// with no column wearing the row above's stop at that column.
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

/// 90 columns at 60 ms, three growths, typing on; wraps 2 and 3 carry twins
/// (asserted), `WHY` under `HEY` among them.
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

/// Each growth censused with the hand stopped at the wrapping key: the relay
/// arrives on the held park, and the carried line still follows and flows.
#[test]
fn every_growth_of_the_owner_s_composer_follows_and_flows_when_the_hand_stops_at_the_wrap() {
    crate::common::par_map(&[1, 2, 3], |&k| {
        let wraps = run_census(OWNER_90, NATURAL, &[k], Hand::Stops);
        assert_follows_and_flows(
            &wraps[0],
            "the owner's text at 90 columns, the hand stopped",
        );
    });
}

/// 60 columns at 130 ms, three growths; the first wrap twins against the rule's
/// label (the `o` of `got` under the `o` of `workspace`, asserted).
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

/// One coincident letter does not change the exit. A is the no-twin control: letters only,
/// each line's case alternating, so no glyph stands under the same glyph on the line above
/// (asserted).
#[test]
fn one_coincident_letter_does_not_change_the_exit() {
    let width = OWNER_90.cols - 4;
    let a = alternating_case(NATURAL, width);
    let b = with_one_twin(&a, width, 1);
    let diff = a.chars().zip(b.chars()).filter(|(x, y)| x != y).count();
    assert_eq!(diff, 1, "premise: A and B differ in exactly one glyph");
    let [wa, wb]: [Vec<Series>; 2] =
        crate::common::par_map(&[&a, &b], |text| run(OWNER_90, text, 3))
            .try_into()
            .expect("two takes");
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

/// Three rows that start alike (`HEY` / `HOW` / moved `HIS`): at wrap 2 the kept
/// line's first glyph is an edge twin and the relaid word starts with it too.
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

/// The kept line follows its text and flows whatever the wrap's proportions,
/// the hand stopped at the wrap: a wrap that moves more than it keeps (20
/// columns, wrap 5 keeps `this` and moves `confirmatio`: the moved records are
/// holes a row up only because the wrap relaid them, and must not refuse the
/// follow), a long path carried down under a short line (`see` at 60 columns),
/// and a wrap that keeps only one or two cells.
#[test]
fn every_wrap_proportion_s_kept_line_follows_and_flows() {
    let narrow = fit(NATURAL, NARROW_20.cols - 4);
    let cases: [(&str, Shape, &str, usize, usize, usize); 4] = [
        ("20 cols, wrap 5", NARROW_20, &narrow, 5, 4, 11),
        ("`see` and a path, 60 cols", GLASS_60, PATH_60, 1, 3, 52),
        ("keeps 1, 20 cols", NARROW_20, KEPT_1, 1, 1, 14),
        ("keeps 2, 20 cols", NARROW_20, KEPT_2, 1, 2, 13),
    ];
    crate::common::par_map(&cases, |&(what, shape, text, wrap, kept, w)| {
        let wraps = run_census(shape, text, &[wrap], Hand::Stops);
        let sr = &wraps[0];
        assert!(
            sr.kept == kept && sr.w == w,
            "{what}: premise: kept {} w {} (moved `{}`)",
            sr.kept,
            sr.w,
            sr.moved
        );
        assert!(
            sr.followed() >= kept as u64,
            "{what}: ribbon_followed= grew {} — the kept line did not follow its text: {}",
            sr.followed(),
            sr.curve()
        );
        assert_follows_and_flows(sr, what);
    });
}

/// `see` and a 61-letter path at 60 columns, 130 ms keys: the wrap keeps `see`
/// (3) and moves 52 letters.
const PATH_60: &str = "see crates/aterm-effects/src/rainbow_kitty/witness_follow_runs.rs";

/// `please read` and an 83-letter path at 90 columns, 60 ms keys: the wrap
/// keeps 11 and moves 74 letters.
const PATH_90: &str = "please read crates/aterm-effects/src/rainbow_kitty/witness_follow_runs_and_the_relaid_suffix.rs now";

/// A narrow pane of odd width: 21 columns, 17 text cells, 130 ms keys.
const NARROW_21: Shape = Shape {
    rows: 30,
    cols: 21,
    cw: 15,
    ch: 28,
    key_ms: 130,
};

/// [`NARROW_21`] with 60 ms keys.
const NARROW_21_60: Shape = Shape {
    key_ms: 60,
    ..NARROW_21
};

/// The new row continues the moved word's walk, whatever the wrap's shape and
/// whether the hand stops at the wrap (the relay arrives at the park flush,
/// ≤ 0.25 s, and must still find its origin after the old cells melt at
/// 0.12 s) or types on (the eaten Space's cell and later keys continue the
/// relay's walk, not the old run's stops):
#[test]
fn every_wrap_shape_s_new_row_continues_the_old_row_s_walk() {
    let n20 = fit(NATURAL, NARROW_20.cols - 4);
    let n21 = fit(NATURAL, NARROW_21.cols - 4);
    let two_sp = PATH_60.replacen(' ', "  ", 1);
    let cols = |cols| Shape {
        rows: 30,
        cols,
        cw: 15,
        ch: 28,
        key_ms: 60,
    };
    let cd = "cd crates/aterm-effects/src/rainbow_kitty/witness_follow_runs_and_the_relaid_suffix_of_every_wrap.rs now";
    let long_90 = "please read the whole of this long file nows crates/aterm-effects/src/rainbow_kitty/ribbon_follow_the_text.rs now";
    let friendly = "friendly neighbors wave";
    let hello = "hello there wonderful day to you all";
    let apis = "please use the apis internationalization now";
    let (stops, on) = (Then::Stops, Then::TypesOn);
    let cases: Vec<(&str, Shape, &str, usize, Then, usize, usize)> = vec![
        ("long word, 60 cols", GLASS_60, PATH_60, 1, stops, 3, 52),
        ("long word, 60 cols", GLASS_60, PATH_60, 1, on(3), 3, 52),
        ("owner's text, 20 cols", NARROW_20, &n20, 5, stops, 4, 11),
        ("owner's text, 20 cols", NARROW_20, &n20, 5, on(4), 4, 11),
        ("path, 90 cols", OWNER_90, PATH_90, 1, stops, 11, 74),
        ("path, 90 cols", OWNER_90, PATH_90, 1, on(3), 11, 74),
        ("control: keeps more", OWNER_90, NATURAL, 1, stops, 83, 2),
        ("control: keeps more", OWNER_90, NATURAL, 1, on(12), 83, 2),
        ("two Spaces, 60 cols", GLASS_60, &two_sp, 1, stops, 4, 51),
        ("kept == w, 21 cols", NARROW_21, friendly, 1, stops, 8, 8),
        ("kept == w, 21 cols", NARROW_21, friendly, 1, on(3), 8, 8),
        ("remnant, 60 ms", NARROW_21_60, friendly, 1, on(3), 8, 8),
        ("remnant, 24 cols", cols(24), hello, 1, on(6), 11, 8),
        ("remnant, 40 cols", cols(40), apis, 1, on(6), 19, 16),
        ("remnant, 90 cols", OWNER_90, long_90, 1, on(4), 44, 41),
        ("remnant, owner, 21 cols", NARROW_21, &n21, 4, on(2), 8, 8),
        ("keeps 2, 20 cols", NARROW_20, KEPT_2, 1, stops, 2, 13),
        ("keeps 2, 20 cols", NARROW_20, KEPT_2, 1, on(1), 2, 13),
        ("keeps 2, path, 90 cols", OWNER_90, cd, 1, on(2), 2, 83),
        ("keeps 1, 20 cols", NARROW_20, KEPT_1, 1, stops, 1, 14),
        ("keeps 1, 20 cols", NARROW_20, KEPT_1, 1, on(1), 1, 14),
    ];
    crate::common::par_map(&cases, |&(what, shape, text, wrap, then, kept, w)| {
        let nr = read_new_row(shape, text, wrap, then);
        assert!(
            nr.kept == kept && nr.w == w,
            "{what}, {then:?}: premise: kept {} w {}",
            nr.kept,
            nr.w
        );
        assert_new_row_continues(&nr, &format!("{what}, {then:?}"));
    });
}

/// A Space typed before the wrap's echo is not the word's Space: at 90 columns
/// and 60 ms with each echo lagging its press, the key after each wrap key is a
/// held Space.
#[test]
fn a_space_typed_before_the_wrap_s_echo_lands_leaves_the_moved_word_relaid() {
    let takes: Vec<(u64, usize)> = [10, 20, 40]
        .into_iter()
        .flat_map(|lag| (1..=3).map(move |wrap| (lag, wrap)))
        .collect();
    crate::common::par_map(&takes, |&(lag, wrap)| {
        let nr = read_new_row_lagged(OWNER_90, NATURAL, wrap, Then::TypesOn(12), lag);
        assert!(nr.w > 0, "premise: wrap {wrap} moves a word");
        assert_new_row_continues(
            &nr,
            &format!("the owner's text at 90 columns, echo lag {lag} ms, wrap {wrap}"),
        );
    });
}

/// The control: the same wraps with a letter after the first wrap key (`WHYX`) at every lag.
#[test]
fn the_control_a_letter_typed_before_the_wrap_s_echo_lands() {
    let text = NATURAL.replacen("AUDIT WHY ATERM", "AUDIT WHYX ATERM", 1);
    crate::common::par_map(&[10, 20, 40], |&lag| {
        let nr = read_new_row_lagged(OWNER_90, &text, 1, Then::TypesOn(12), lag);
        assert!(nr.w == 2, "premise: wrap 1 moves `WH`: w {}", nr.w);
        assert_new_row_continues(&nr, &format!("`WHYX` at 90 columns, echo lag {lag} ms"));
    });
}

/// A wrap that moves no word continues one past the Space it ate: at 60 columns
/// the first wrap is `…I got this ` + `c` (kept 55, w 0).
#[test]
fn a_wrap_that_moves_no_word_continues_one_past_the_eaten_space_whatever_the_echo_lag() {
    let s = Shape {
        key_ms: 60,
        ..GLASS_60
    };
    let text = format!("{}flows next line of text", "rainbow ".repeat(10));
    let takes = [(false, 0), (false, 20), (false, 40), (true, 0), (true, 20)];
    crate::common::par_map(&takes, |&(owner_width, lag)| {
        if !owner_width {
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
            return;
        }
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
    });
}

/// `to introspection|s` at 20 columns: the wrap keeps two cells.
const KEPT_2: &str = "to introspections are fun";

/// `I introspections|s` at 20 columns: the wrap keeps one cell.
const KEPT_1: &str = "I introspectionss are fun";

/// The repaint may arrive 120 ms after its key, just before the next key's
/// 130 ms press.
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

/// A real one-cell wrap proves its held park and relays the moved word.
#[test]
fn a_proved_short_wrap_does_not_reanchor_a_later_false_park() {
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
