// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared test and benchmark helpers for `aterm-effects`.

use aterm_core::grid::Cursor;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

/// A glow whose ticks fold its ramps into its quads
/// (`CursorGlow::fold_glow_ramps`): the suites here read its streams as quads.
#[cfg(debug_assertions)]
#[allow(dead_code)]
pub fn quads_glow() -> CursorGlow {
    let mut glow = CursorGlow::default();
    glow.fold_glow_ramps(true);
    glow
}

/// Environment variable scaling the wall-clock budgets of the release perf gates.
#[allow(dead_code)]
pub const BUDGET_SCALE_VAR: &str = "RK_FRAME_COST_BUDGET_SCALE";

/// Read `RK_FRAME_COST_BUDGET_SCALE` as a positive finite multiplier, defaulting to `1.0`.
#[allow(dead_code)]
pub fn budget_scale() -> f64 {
    parse_budget_scale(std::env::var(BUDGET_SCALE_VAR).ok().as_deref())
}

#[allow(dead_code)]
fn parse_budget_scale(raw: Option<&str>) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0)
}

/// Scale `reference_us` by `scale`, rounded to whole microseconds and floored at `1`.
#[allow(dead_code)]
pub fn scaled_budget_us(reference_us: u64, scale: f64) -> u64 {
    (reference_us as f64 * scale).round().max(1.0) as u64
}

/// Default dark-theme [`GlowStyle::RainbowKitty`] configuration (240 ms, intensity 0.7).
#[allow(dead_code)]
pub fn rainbow_cfg() -> GlowConfig {
    GlowConfig {
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        ..Default::default()
    }
}

/// [`rainbow_cfg`] on `ground` ([`Ground::lay`]).
#[allow(dead_code)]
pub fn rainbow_cfg_on(ground: Ground) -> GlowConfig {
    ground.lay(rainbow_cfg())
}

/// A scheme's ground as the host hands it to the trail (`app_render.rs`'s
/// frame glow config): its default fg and bg, with `dark_theme` read off the
/// bg by `theme_is_dark`.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ground {
    /// The ground's name in a verdict.
    pub name: &'static str,
    pub fg: u32,
    pub bg: u32,
}

#[allow(dead_code)]
impl Ground {
    /// [`rainbow_cfg`]'s dark ground.
    pub const TOKYO: Self = Self {
        name: "Tokyo",
        fg: 0x00C8_D3F5,
        bg: 0x001A_1B26,
    };

    /// The built-in scheme `name`'s ground.
    pub fn scheme(name: &'static str) -> Self {
        let s = aterm_types::scheme::builtin(name)
            .unwrap_or_else(|| panic!("{name:?} is no built-in scheme"));
        let rgb = |c: aterm_types::Rgb| aterm_render::rgb_to_u32([c.r, c.g, c.b]);
        Self {
            name,
            fg: rgb(s.foreground),
            bg: rgb(s.background),
        }
    }

    /// The host's polarity rule: no dark theme on a light bg.
    pub fn dark(self) -> bool {
        aterm_render::theme_is_dark(self.bg)
    }

    /// `cfg` with this ground's colours and polarity, as the host lays them.
    pub fn lay(self, cfg: GlowConfig) -> GlowConfig {
        GlowConfig {
            dark_theme: self.dark(),
            theme_fg: self.fg,
            theme_bg: self.bg,
            ..cfg
        }
    }
}

thread_local! {
    static GROUND: std::cell::Cell<Option<Ground>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with every host this thread builds standing on `ground` instead of
/// the one its test names ([`ground_or`]), so a family reads on a light scheme
/// as it reads on its own. A panic in `f` restores the host's own ground.
#[allow(dead_code)]
pub fn on_ground<T>(ground: Ground, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Ground>);
    impl Drop for Restore {
        fn drop(&mut self) {
            GROUND.with(|g| g.set(self.0));
        }
    }
    let _restore = Restore(GROUND.with(|g| g.replace(Some(ground))));
    f()
}

/// The ground a host stands on: [`on_ground`]'s, else its own.
#[allow(dead_code)]
pub fn ground_or(own: Ground) -> Ground {
    GROUND.with(std::cell::Cell::get).unwrap_or(own)
}

/// Construct a `rows × cols` grid geometry with `cw × ch` pixel cells at the origin.
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

/// Maximum duration a dirty DEC 2026 bracket withholds presentation.
#[allow(dead_code)]
pub const SYNC_HOLD_CAP: Duration = Duration::from_millis(150);

/// DEC 2026 synchronized-output frame hold state machine capped at [`SYNC_HOLD_CAP`].
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameHold {
    was_active: bool,
    deadline: Option<Instant>,
    armed_seq: u64,
}

#[allow(dead_code)]
impl FrameHold {
    /// Advance the hold state at `now` and return whether presentation is withheld.
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

/// Apply content-scroll deltas from `term` since `seen` to `glow`.
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

/// Return the terminal cursor and its visible `(row, col)` sample.
#[allow(dead_code)]
pub fn caret_sample(term: &Terminal) -> (Cursor, Option<(u16, u16)>) {
    let c = term.cursor();
    (c, term.cursor_visible().then_some((c.row, c.col)))
}

/// Forward any advance in `term.repaint_blink_epoch()` since `seen` to `glow`.
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

/// Sample `c.row` from `term` into `buf` and observe it as both the caret row and a ribbon row.
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

/// Sample the rows immediately above and below `row` on a `rows`-tall grid into `bufs` and pass them to `glow`.
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

/// Observe every row reported by `CursorGlow::ribbon_rows` up to [`WITNESS_ROWS`].
#[allow(dead_code)]
pub fn observe_ribbon_rows(term: &Terminal, glow: &mut CursorGlow, buf: &mut Vec<char>) {
    observe_ribbon_rows_where::<WITNESS_ROWS>(term, glow, buf, |_| true);
}

/// Observe up to `N` ribbon rows from `glow` that satisfy `keep`.
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

/// Minimum planned coverage (`20 / 255`) at which a cell counts as lit.
#[allow(dead_code)]
pub const LIT_COV: u8 = 20;

/// Planned coverage per column of `row`.
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

/// Interior unlit (`< LIT_COV`) runs `(first_col, len)` between the first and last lit columns.
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

/// Return `(shifted, TypedClass)` for a typed character `ch`.
#[allow(dead_code)]
pub fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        _ => (false, TypedClass::Glyph),
    }
}

/// Return whether `b` is a single printable ASCII byte.
#[allow(dead_code)]
pub fn is_key(b: &[u8]) -> bool {
    matches!(b, [c] if (0x20..0x7f).contains(c))
}

/// Return whether `b` is a CSI arrow-key sequence (`ESC [ … A|B|C|D`).
#[allow(dead_code)]
pub fn is_arrow(b: &[u8]) -> bool {
    b.starts_with(b"\x1b[") && matches!(b.last(), Some(b'A' | b'B' | b'C' | b'D'))
}

/// One recorded event from a `lab ptyrec` trace.
#[allow(dead_code)]
pub enum Rec {
    /// Bytes written by the program to the terminal.
    Out(Vec<u8>),
    /// Bytes sent by the keyboard to the program.
    In(Vec<u8>),
}

#[allow(dead_code)]
pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

/// Parse `<stamp> <O|I> <hex>` lines into `(elapsed_ms, Rec)` using `per_ms` ticks per millisecond.
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

/// The replay pool, where the library builds it (a release bench that mounts
/// this module builds none).
#[cfg(debug_assertions)]
#[allow(unused_imports)]
#[cfg(debug_assertions)]
pub use aterm_effects::test_pool::par_map;
