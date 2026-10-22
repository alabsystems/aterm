// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared support for this crate's integration tests and benches. `effects_suite`, the
//! crate's one test binary, mounts it once (its files reach it as `crate::common`); a bench
//! mounts it (or a suite file) by path. An item some binary leaves unused carries
//! `#[allow(dead_code)]` (a re-export, `#[allow(unused_imports)]`).
//!
//! It holds the budget-scale knob of the release perf gates (`benches/cursor_bench.rs` and
//! `benches/rainbow_kitty_v2_frame_cost.rs`); the ribbon host's pieces: the rainbow-kitty
//! config, a grid's geometry, the present's DEC 2026 hold, and the frame hold's samples as
//! `app_render.rs` takes them from a real `Terminal` after the last `process`, one free
//! function each so a host keeps its own order; the plan-coverage census of a row; the
//! `lab ptyrec` recording parse the real-byte replays read; and the replay pool
//! ([`par_map`]).

use aterm_core::grid::Cursor;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

// The budget-scale knob.

/// The knob's name, shared by both gates.
#[allow(dead_code)]
pub const BUDGET_SCALE_VAR: &str = "RK_FRAME_COST_BUDGET_SCALE";

/// `RK_FRAME_COST_BUDGET_SCALE=x` (a positive float, default `1.0`) multiplies the absolute
/// wall-clock budgets of the two release-only perf gates and nothing else: §18's p50 400 µs /
/// p90 600 µs pair in `rainbow_kitty_v2_frame_cost` and the p90/median bounds in `cursor_bench`
/// (500, 500 and 1 000 µs). §18 (docs/design/RAINBOW-KITTY-V2.md) names no reference machine,
/// so the knob is for a host unlike it. Nothing structural is scaled (zero-alloc, idle → zero,
/// quad caps and shares, the fixture-emits checks): a red there is about the engine, not the
/// host.
#[allow(dead_code)]
pub fn budget_scale() -> f64 {
    parse_budget_scale(std::env::var(BUDGET_SCALE_VAR).ok().as_deref())
}

/// The scale's parse: unset, empty, unparsable, non-finite and non-positive spellings all mean
/// `1.0`, so a typo widens nothing.
#[allow(dead_code)]
fn parse_budget_scale(raw: Option<&str>) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0)
}

/// The absolute budget on this host: the reference times the scale, rounded to the microsecond
/// and floored at 1 µs, so `1.0` gives the reference exactly and no scale turns a gate into
/// `p50 ≤ 0 µs`.
#[allow(dead_code)]
pub fn scaled_budget_us(reference_us: u64, scale: f64) -> u64 {
    (reference_us as f64 * scale).round().max(1.0) as u64
}

// The ribbon host: config and geometry.

/// The config most takes ran: rainbow kitty, tall body, dark, 240 ms, intensity 0.7. A take
/// that ran another spells only what differs: `GlowConfig { intensity: 1.0, ..rainbow_cfg() }`.
#[allow(dead_code)]
pub fn rainbow_cfg() -> GlowConfig {
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
    }
}

/// A `rows × cols` grid of `cw × ch` px cells filling its window from the
/// origin.
#[allow(dead_code)]
pub fn geom(rows: usize, cols: usize, cw: usize, ch: usize) -> Geom {
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

// The ribbon host: the present's DEC 2026 hold.

/// `app_render.rs`'s `SYNC_HOLD_CAP`: how long a dirty DEC 2026 bracket holds
/// the present; past it the frame is presented as it stands.
#[allow(dead_code)]
pub const SYNC_HOLD_CAP: Duration = Duration::from_millis(150);

/// The present's DEC 2026 hold, `app_render.rs`'s `sync_frame_hold` over the
/// pane's sync level, close counter and open-dirty bit, its timeout
/// [`SYNC_HOLD_CAP`]: armed on the rising edge, held while armed and
/// unexpired, re-armed when one bracket closed and another opened (held then
/// only while the new one is dirty), released when sync ends or the deadline
/// passes.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameHold {
    was_active: bool,
    deadline: Option<Instant>,
    armed_seq: u64,
}

#[allow(dead_code)]
impl FrameHold {
    /// Whether the present at `now` is withheld; one step of the machine.
    pub fn withholds(&mut self, term: &Terminal, now: Instant) -> bool {
        let active = term.modes().synchronized_output();
        let end_seq = term.sync_end_seq();
        let (deadline, hold) = if !active {
            (None, false)
        } else if self.was_active && end_seq != self.armed_seq {
            (Some(now + SYNC_HOLD_CAP), term.sync_open_dirty())
        } else {
            let armed = if self.was_active {
                self.deadline
            } else {
                Some(now + SYNC_HOLD_CAP)
            };
            let hold = armed.is_some_and(|d| now < d);
            (armed.filter(|_| hold), hold)
        };
        *self = Self {
            was_active: active,
            deadline,
            armed_seq: end_seq,
        };
        hold
    }
}

// The ribbon host: the frame hold's samples.

/// The host's content-scroll seam (`sync_cursor_effect_scroll`): the delta
/// since the state the last frame saw (`seen`) — a translate or band moves
/// the ribbon with the text and drops the row probe, an unexplained step
/// curtains — then `seen` is this frame's state.
#[allow(dead_code)]
pub fn sync_scroll(
    term: &Terminal,
    glow: &mut CursorGlow,
    seen: &mut Option<ContentScrollState>,
    now: Instant,
) {
    let scroll = term.content_scroll_state();
    match ContentScrollState::delta_since(*seen, scroll) {
        ContentScrollDelta::Baseline | ContentScrollDelta::Unchanged => {}
        ContentScrollDelta::Translate(rows) => {
            glow.note_scroll(rows);
            glow.drop_row_probe();
        }
        ContentScrollDelta::Bands { first_seq, count } => {
            for i in 0..u64::from(count) {
                let m = scroll.band(first_seq + i);
                glow.note_band_move(m.top, m.bottom, m.delta);
            }
            glow.drop_row_probe();
        }
        ContentScrollDelta::Invalidate => glow.curtain(now),
    }
    *seen = Some(scroll);
}

/// The caret, and the frame's cursor sample: `(row, col)` while it is
/// drawn, `None` while hidden.
#[allow(dead_code)]
pub fn caret_sample(term: &Terminal) -> (Cursor, Option<(u16, u16)>) {
    let c = term.cursor();
    (c, term.cursor_visible().then_some((c.row, c.col)))
}

/// The repaint blink: the terminal's `repaint_blink_epoch` diffed against
/// the last one seen — an advance is the hide-inside-DEC-2026 repaint
/// bracket, noted at `now`. Returns whether it advanced.
#[allow(dead_code)]
pub fn note_blink(term: &Terminal, glow: &mut CursorGlow, seen: &mut u64, now: Instant) -> bool {
    let epoch = term.repaint_blink_epoch();
    if epoch == *seen {
        return false;
    }
    *seen = epoch;
    glow.note_repaint_blink(now);
    true
}

/// The caret row's probe, read into `buf`: observed as the caret's row, and
/// as a ribbon row (the caret row rides the probe the host already holds).
#[allow(dead_code)]
pub fn observe_caret_row(
    term: &Terminal,
    glow: &mut CursorGlow,
    buf: &mut Vec<char>,
    c: Cursor,
    now: Instant,
) {
    term.row_cols_into(usize::from(c.row), buf);
    glow.observe_row(c.row, c.col, buf, now);
    glow.observe_ribbon_row(c.row, buf);
}

/// The caret row's neighbours on a `rows`-tall screen, read into `bufs`
/// (`[above, below]`) and handed over — `None` past the screen's edge.
#[allow(dead_code)]
pub fn observe_neighbor_rows(
    term: &Terminal,
    glow: &mut CursorGlow,
    bufs: &mut [Vec<char>; 2],
    row: u16,
    rows: usize,
) {
    let r = usize::from(row);
    let [above, below] = bufs;
    if r > 0 {
        term.row_cols_into(r - 1, above);
    }
    if r + 1 < rows {
        term.row_cols_into(r + 1, below);
    }
    glow.observe_neighbor_rows(
        (r > 0).then_some(above.as_slice()),
        (r + 1 < rows).then_some(below.as_slice()),
    );
}

/// The ribbon's own rows: every row `CursorGlow::ribbon_rows` names into a
/// [`WITNESS_ROWS`]-slot list, read into `buf` and observed.
#[allow(dead_code)]
pub fn observe_ribbon_rows(term: &Terminal, glow: &mut CursorGlow, buf: &mut Vec<char>) {
    observe_ribbon_rows_where::<WITNESS_ROWS>(term, glow, buf, |_| true);
}

/// [`observe_ribbon_rows`] into an `N`-slot list, reading only the rows
/// `keep` admits (a host that skips the caret row it already sampled, or
/// drops the far rows).
#[allow(dead_code)]
pub fn observe_ribbon_rows_where<const N: usize>(
    term: &Terminal,
    glow: &mut CursorGlow,
    buf: &mut Vec<char>,
    keep: impl Fn(u16) -> bool,
) {
    let mut rows = [0u16; N];
    let n = glow.ribbon_rows(&mut rows);
    for &r in rows[..n].iter().filter(|&&r| keep(r)) {
        term.row_cols_into(usize::from(r), buf);
        glow.observe_ribbon_row(r, buf);
    }
}

// The ribbon host: the plan's census.

/// A cell counts as lit at this planned coverage; the pixel census on glass calls a cell lit
/// at 20% of the bed.
#[allow(dead_code)]
pub const LIT_COV: u8 = 20;

/// Plan coverage per column of `row`.
#[allow(dead_code)]
pub fn coverage(glow: &CursorGlow, g: Geom, row: u16) -> Vec<u8> {
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

/// Interior dark runs of a coverage lane, as `(first col, len)`: the unlit
/// runs between its first and last [`LIT_COV`] column.
#[allow(dead_code)]
pub fn holes_of(cov: &[u8]) -> Vec<(usize, usize)> {
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

// The ribbon host: keys and recordings.

/// The host's `typed_class_for` and `glyph_shifted && !spacebar`.
#[allow(dead_code)]
pub fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        _ => (false, TypedClass::Glyph),
    }
}

/// A printable one-byte key.
#[allow(dead_code)]
pub fn is_key(b: &[u8]) -> bool {
    matches!(b, [c] if (0x20..0x7f).contains(c))
}

/// An arrow key, plain or with a modifier (`ESC [ … A|B|C|D`).
#[allow(dead_code)]
pub fn is_arrow(b: &[u8]) -> bool {
    b.starts_with(b"\x1b[") && matches!(b.last(), Some(b'A' | b'B' | b'C' | b'D'))
}

/// One recorded event of a `targo --unverified lab ptyrec` take.
#[allow(dead_code)]
pub enum Rec {
    /// Bytes the program wrote to the terminal.
    Out(Vec<u8>),
    /// Bytes the keyboard sent to the program.
    In(Vec<u8>),
}

#[allow(dead_code)]
pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

/// `(ms from the first line, event)` of a take whose lines read `<stamp>
/// <O|I> <hex>`; `per_ms` stamps make a millisecond (`1_000_000` for
/// `lab ptyrec`'s nanoseconds, `1_000` for microseconds).
#[allow(dead_code)]
pub fn recording(src: &str, per_ms: u64) -> Vec<(u64, Rec)> {
    let mut t0 = None;
    src.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, ' ');
            let stamp: u64 = it.next().expect("stamp").parse().expect("stamp");
            let kind = it.next().expect("kind");
            let bytes = unhex(it.next().expect("bytes"));
            let t0 = *t0.get_or_insert(stamp);
            let ms = (stamp - t0) / per_ms;
            match kind {
                "O" => (ms, Rec::Out(bytes)),
                "I" => (ms, Rec::In(bytes)),
                k => panic!("bad kind {k}"),
            }
        })
        .collect()
}

// The replay pool: the library's, so the unit tests and this suite share one.

#[allow(unused_imports)]
pub use aterm_effects::test_pool::par_map;
