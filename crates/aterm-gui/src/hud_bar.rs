// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bottom PERFORMANCE HUD — two stacked widgets plus the reusable, themed building
//! blocks that make aterm's HUDs a small framework:
//!
//! 1. **Resources** — the day-to-day view: *total system* vs *this terminal session*
//!    (aterm and every process it spawned) for CPU, memory, GPU, disk, and network. A
//!    three-row table — a labelled rule for separation, a `SYSTEM` row, and a `SESSION`
//!    row — with the columns aligned so the two scopes read at a glance. Health colour is
//!    SUBTLE: values stay the neutral text tone until they cross a threshold, then tint
//!    amber/red — so the bar is calm until something actually wants attention.
//! 2. **Engine** — the terminal's own runtime: render backend, fps, a frame-time
//!    sparkline, frame/present latency, aterm's resident memory, and any app-fed metric
//!    streams. On by default, like Resources (the performance GUI ships whole); the
//!    master `show_hud` key or the per-panel toggle turns it off.
//!
//! A HUD is rendered EXACTLY like the tab strip: rows of `aterm_core::terminal::RenderCell`s
//! spliced into the composed `RenderInput`, so it is WYSIWYG on glass AND visible to the
//! `image`/`snapshot` introspection, and goes through the same CPU/GPU renderer (parity
//! holds by construction). The sparkline uses the procedurally-synthesized block glyphs
//! `▁▂▃▄▅▆▇█` (cell-exact, font-independent).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_render::Theme;

// Dark/light classification is delegated to `tab_bar::bg_is_light` — the ONE
// classifier for all chrome, so HUD + tab strip + native toolbar can never disagree.
use crate::tab_bar::bg_is_light;

/// How many recent frames the engine sparkline / FPS window retains.
const RING_CAP: usize = 64;
/// Frame samples older than this are dropped (keeps FPS + rolling-max honest at idle).
const RING_TTL: Duration = Duration::from_secs(3);
/// Sparkline FLOOR full-scale: the bar auto-scales to `max(this, rolling-max)` so a
/// quiet workload still shows a low staircase (not a flat line) and a slow one stays
/// a *varied* staircase instead of pinning to a solid block. 16ms ≈ the 60fps budget.
const SPARK_FLOOR_MS: f32 = 16.0;

/// A poll-driven sample is rendered `n/a` once it is older than this (the OS probe
/// failed/stopped). Keeps the readout HONEST rather than freezing the last value forever.
const PANEL_STALE: Duration = Duration::from_secs(5);

// =============================================================================
// Streaming frame-sample ring — the Engine widget's per-frame state.
// =============================================================================

#[derive(Clone, Copy)]
struct Sample {
    at: Instant,
    render_ns: u64,
    present_ns: u64,
}

/// Rolling per-frame samples driving the Engine HUD's FPS, sparkline, and WINDOWED
/// maxima (the shared `crate::metrics` module holds only scalar all-time counters — the
/// streaming series + recent maxima live here so the HUD reflects *current* smoothness,
/// not a one-time startup spike).
pub(crate) struct HudSamples {
    ring: VecDeque<Sample>,
}

impl HudSamples {
    pub(crate) fn new() -> Self {
        Self {
            ring: VecDeque::with_capacity(RING_CAP),
        }
    }

    /// Record one presented/rendered frame. `present_ns` is 0 on the headless `image`
    /// path (no on-glass present). Called wherever `metrics::record_present` is.
    pub(crate) fn record(&mut self, render_ns: u64, present_ns: u64, now: Instant) {
        self.ring.push_back(Sample {
            at: now,
            render_ns,
            present_ns,
        });
        while self.ring.len() > RING_CAP {
            self.ring.pop_front();
        }
        while self.ring.front().is_some_and(|s| {
            now.checked_duration_since(s.at)
                .is_some_and(|a| a > RING_TTL)
        }) {
            self.ring.pop_front();
        }
    }

    fn within(&self, now: Instant, win: Duration) -> impl Iterator<Item = &Sample> {
        self.ring
            .iter()
            .filter(move |s| now.checked_duration_since(s.at).is_some_and(|a| a <= win))
    }

    /// Frames presented within the last second → rolling FPS.
    fn fps(&self, now: Instant) -> u32 {
        u32::try_from(self.within(now, Duration::from_secs(1)).count()).unwrap_or(u32::MAX)
    }

    fn last(&self) -> Option<&Sample> {
        self.ring.back()
    }

    fn max_render_ns(&self) -> u64 {
        self.ring.iter().map(|s| s.render_ns).max().unwrap_or(0)
    }

    /// Any real on-glass present recorded (vs the headless image path, all-zero)?
    fn any_present(&self) -> bool {
        self.ring.iter().any(|s| s.present_ns > 0)
    }

    /// The last `width` frame-render times as sparkline levels 0..=8, AUTO-SCALED to
    /// `max(SPARK_FLOOR, rolling-max)`, PLUS the aligned per-bar frame-ms so the painter
    /// can colour each bar by absolute frame health rather than bar height. Oldest→newest,
    /// left-padded with empties (0).
    fn spark(&self, width: usize) -> (Vec<u8>, Vec<f32>) {
        let ms: Vec<f64> = self
            .ring
            .iter()
            .map(|s| s.render_ns as f64 / 1.0e6)
            .collect();
        let levels = levels_autoscaled(&ms, f64::from(SPARK_FLOOR_MS), width);
        let mut ms_aligned = vec![0.0f32; width];
        let n = ms.len().min(width);
        for (i, &v) in ms.iter().rev().take(n).enumerate() {
            ms_aligned[width - 1 - i] = v as f32;
        }
        (levels, ms_aligned)
    }
}

/// Map a value series (oldest→newest) to sparkline levels 0..=8, AUTO-SCALED to
/// `max(floor, series-max)` so the staircase stays varied on any workload (never a
/// flat solid block). The newest `width` values land at the right; `0` values and
/// left-padding are level 0 (blank). Shared by every panel's sparkline.
pub(crate) fn levels_autoscaled(values: &[f64], floor: f64, width: usize) -> Vec<u8> {
    let mut out = vec![0u8; width];
    if width == 0 {
        return out;
    }
    // Fold only finite samples into the scale (a stray NaN/±Inf must not poison the
    // whole staircase), and clamp the floor up off zero so the divide is always safe.
    let scale = values
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(floor, f64::max)
        .max(f64::MIN_POSITIVE);
    let n = values.len().min(width);
    for (i, &v) in values.iter().rev().take(n).enumerate() {
        out[width - 1 - i] = if !v.is_finite() || v <= 0.0 {
            0
        } else {
            ((v / scale) * 8.0).round().clamp(1.0, 8.0) as u8
        };
    }
    out
}

// =============================================================================
// Reusable themed render helpers (the "framework" layer the widgets share).
// =============================================================================

/// On-theme tones, all linear-blended from the active `Theme` (so HUDs track any
/// scheme), mirroring `tab_bar::strip_colors`.
#[derive(Clone, Copy)]
pub(crate) struct HudColors {
    pub bar_bg: [u8; 3],
    pub label: [u8; 3],
    pub value: [u8; 3],
    pub good: [u8; 3],
    pub warn: [u8; 3],
    pub hot: [u8; 3],
}

fn rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

fn blend(a: u32, b: u32, t: f32) -> [u8; 3] {
    mix3(rgb(a), rgb(b), t)
}

/// Linear blend of two packed-RGB tones `a` toward `b` by `t ∈ [0,1]`.
fn mix3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let mix = |x: u8, y: u8| (f32::from(x).mul_add(1.0 - t, f32::from(y) * t)).round() as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

/// WCAG relative-luminance contrast ratio between two tones (delegated to the single
/// implementation in `aterm-types`, same one `tab_bar`'s contrast test uses).
fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    aterm_types::Rgb::new(a[0], a[1], a[2]).contrast(aterm_types::Rgb::new(b[0], b[1], b[2]))
}

/// Darken/lighten `c` toward the higher-contrast pole (black on a light bar, white on
/// a dark one) JUST enough to clear `target` contrast against `bg`, preserving hue.
/// Falls back to the max-contrast pole if `target` is unreachable, so a health color
/// is NEVER invisible on any theme (the light-theme defect this fixes).
fn ensure_contrast(c: [u8; 3], bg: [u8; 3], target: f64) -> [u8; 3] {
    if contrast(c, bg) >= target {
        return c;
    }
    let anchor = if bg_is_light(bg) {
        [0, 0, 0]
    } else {
        [255, 255, 255]
    };
    let mut best = c;
    let mut best_ratio = contrast(c, bg);
    let mut step = 1u8;
    while step <= 10 {
        let m = mix3(c, anchor, f32::from(step) / 10.0);
        let r = contrast(m, bg);
        if r > best_ratio {
            best = m;
            best_ratio = r;
        }
        if r >= target {
            return m;
        }
        step += 1;
    }
    best // unreachable target → strongest available contrast
}

/// On-theme HUD tones. The neutral band/label/value linear-blend from the active
/// theme; the health colors (good/warn/hot) are appearance-aware semantic hues —
/// bright on dark backgrounds, deep on light — each guaranteed-readable against the
/// bar via [`ensure_contrast`]. Mirrors `tab_bar::strip_colors`' light/dark branch so
/// the HUD stays legible (and WCAG-AA, see the contrast test) on every scheme.
pub(crate) fn hud_colors(theme: Theme) -> HudColors {
    let light = bg_is_light(rgb(theme.bg));
    let bar_bg = blend(theme.bg, theme.fg, if light { 0.10 } else { 0.16 });
    // Semantic health hues per appearance. Dark: bright pastels + the theme cursor for
    // "good". Light: deep GitHub-style green/amber/red that read on a pale band.
    let (good_base, warn_base, hot_base) = if light {
        (rgb(0x0019_7A33), rgb(0x009A_6700), rgb(0x00CF_222E))
    } else {
        (rgb(theme.cursor), rgb(0x00F1_FA8C), rgb(0x00FF_6E67))
    };
    // AA for body text is 4.5:1; we aim there and let ensure_contrast fall back to the
    // best available so no value is ever unreadable.
    const AA: f64 = 4.5;
    HudColors {
        bar_bg,
        label: blend(theme.fg, theme.bg, if light { 0.40 } else { 0.48 }),
        value: ensure_contrast(rgb(theme.fg), bar_bg, AA),
        good: ensure_contrast(good_base, bar_bg, AA),
        warn: ensure_contrast(warn_base, bar_bg, AA),
        hot: ensure_contrast(hot_base, bar_bg, AA),
    }
}

/// Health grade by ascending-is-worse value against two thresholds → good/warn/hot.
pub(crate) fn grade_hi(v: f32, warn_at: f32, hot_at: f32, c: &HudColors) -> [u8; 3] {
    if v >= hot_at {
        c.hot
    } else if v >= warn_at {
        c.warn
    } else {
        c.good
    }
}

/// Health grade by descending-is-worse value (e.g. fps) → good/warn/hot.
pub(crate) fn grade_lo(v: f32, warn_below: f32, hot_below: f32, c: &HudColors) -> [u8; 3] {
    if v < hot_below {
        c.hot
    } else if v < warn_below {
        c.warn
    } else {
        c.good
    }
}

/// SUBTLE health grade: the neutral `value` tone while healthy, tinting `warn`/`hot`
/// only once the value crosses a threshold. This is the "tasteful feedback" rule — the
/// resource table stays calm and only lights up a figure that actually wants attention,
/// instead of painting every healthy cell a loud accent colour.
pub(crate) fn grade_quiet(v: f32, warn_at: f32, hot_at: f32, c: &HudColors) -> [u8; 3] {
    if v >= hot_at {
        c.hot
    } else if v >= warn_at {
        c.warn
    } else {
        c.value
    }
}

/// A HUD cell builder; `seam` draws a thin overline at the cell's top edge so the
/// whole bar reads as a band separated from the terminal content above.
pub(crate) fn cell(ch: char, fg: [u8; 3], bg: [u8; 3], bold: bool, seam: bool) -> RenderCell {
    RenderCell {
        ch,
        fg,
        bg,
        wide: false,
        emoji_presentation: false,
        bold,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: seam,
        underline_color: None,
    }
}

/// A bare HUD-background cell (fills the bar before segments are painted). The seam
/// overline is drawn in the dim label tone, giving a uniform thin top border across
/// the (majority blank) bar.
#[must_use]
pub fn blank_cell(theme: Theme) -> RenderCell {
    let c = hud_colors(theme);
    cell(' ', c.label, c.bar_bg, false, true)
}

/// A HUD-background cell WITHOUT the top seam — fills the interior rows of a widget so
/// only the band's separator row(s) draw a rule, keeping the band calm (not a grid). Also
/// used by the scene band, which is one continuous canvas: a per-row overline would be
/// drawn ON TOP of the scene by the text layer and show as a thin rule at every cell-row
/// boundary (a "grid" over the meadow), so every scene cell must be seam-free.
pub(crate) fn blank_cell_quiet(theme: Theme) -> RenderCell {
    let c = hud_colors(theme);
    cell(' ', c.label, c.bar_bg, false, false)
}

/// The sparkline glyph for a level 0..=8 (0 → space; 1..=8 → `▁`..`█`).
fn spark_glyph(level: u8) -> char {
    match level {
        0 => ' ',
        n => char::from_u32(0x2580 + u32::from(n.min(8))).unwrap_or('█'),
    }
}

/// A left-packing cursor over a HUD row that writes themed, optionally-colored
/// segments and a color-graded sparkline, never overflowing `row`. Shared by both
/// widgets so they look identical. [`RowWriter::seek`] lets a caller jump to an
/// absolute column for the aligned resource table.
pub(crate) struct RowWriter<'a> {
    row: &'a mut [RenderCell],
    col: usize,
    c: HudColors,
    bar_bg: [u8; 3],
    /// Whether cells this writer paints carry the top-seam overline (the band's rule).
    /// Interior rows use a quiet writer so the band reads as one panel, not a table grid.
    seam: bool,
    /// Reused per-field formatting buffer so the fixed-width numeric fields cost zero
    /// heap allocations per paint (the panel sits on the measured present path).
    scratch: String,
}

impl<'a> RowWriter<'a> {
    pub(crate) fn new(row: &'a mut [RenderCell], theme: Theme) -> Self {
        Self::with_seam(row, theme, true)
    }

    /// A writer whose cells carry NO top seam — for a widget's interior rows.
    pub(crate) fn new_quiet(row: &'a mut [RenderCell], theme: Theme) -> Self {
        Self::with_seam(row, theme, false)
    }

    fn with_seam(row: &'a mut [RenderCell], theme: Theme, seam: bool) -> Self {
        let c = hud_colors(theme);
        let bar_bg = c.bar_bg;
        Self {
            row,
            col: 1, // leading inset
            c,
            bar_bg,
            seam,
            scratch: String::with_capacity(16),
        }
    }

    pub(crate) fn colors(&self) -> &HudColors {
        &self.c
    }

    pub(crate) fn room(&self) -> usize {
        self.row.len().saturating_sub(self.col)
    }

    /// Move the write cursor to absolute column `col` (clamped to the row length) — for
    /// the aligned resource table, where each column starts at a fixed x on every row.
    pub(crate) fn seek(&mut self, col: usize) {
        self.col = col.min(self.row.len());
    }

    /// Write `s` in `fg` (bold optional). Stops at the row edge.
    pub(crate) fn put(&mut self, s: &str, fg: [u8; 3], bold: bool) {
        for ch in s.chars() {
            if self.col >= self.row.len() {
                break;
            }
            self.row[self.col] = cell(ch, fg, self.bar_bg, bold, self.seam);
            self.col += 1;
        }
    }

    /// Like [`put`] for a `format_args!` value, formatting into the reused `scratch`
    /// buffer (no per-field `String` allocation). The `scratch`/`row` borrows are
    /// disjoint fields, so the char loop and the cell writes don't conflict.
    pub(crate) fn put_num(&mut self, args: std::fmt::Arguments<'_>, fg: [u8; 3], bold: bool) {
        use std::fmt::Write as _;
        self.scratch.clear();
        let _ = self.scratch.write_fmt(args);
        for ch in self.scratch.chars() {
            if self.col >= self.row.len() {
                break;
            }
            self.row[self.col] = cell(ch, fg, self.bar_bg, bold, self.seam);
            self.col += 1;
        }
    }

    /// A quiet dim separator between groups: ` · ` — lighter than a full pipe so the
    /// engine row reads as a calm strip, not the old cramped bar.
    pub(crate) fn sep(&mut self) {
        self.put(" \u{00b7} ", self.c.label, false);
    }

    /// A sparkline whose bar HEIGHTS come from `levels` but whose COLORS are supplied
    /// per-bar (e.g. graded by absolute frame-ms health, so fast frames stay green
    /// even when the auto-scaled bar is tall). `colors` aligns 1:1 with `levels`.
    pub(crate) fn sparkline_graded(&mut self, levels: &[u8], colors: &[[u8; 3]]) {
        for (i, &lvl) in levels.iter().enumerate() {
            if self.col >= self.row.len() {
                break;
            }
            let col = colors.get(i).copied().unwrap_or(self.c.good);
            self.row[self.col] = cell(spark_glyph(lvl), col, self.bar_bg, false, self.seam);
            self.col += 1;
        }
    }
}

// =============================================================================
// Human-readable formatters.
// =============================================================================

/// Human-readable short number: `850`, `12.0k`, `3.4M`, `1.2G`.
fn fmt_short(v: f64) -> String {
    if !v.is_finite() {
        return "--".to_string();
    }
    if v >= 1.0e9 {
        format!("{:.1}G", v / 1.0e9)
    } else if v >= 1.0e6 {
        format!("{:.1}M", v / 1.0e6)
    } else if v >= 1.0e3 {
        format!("{:.1}k", v / 1.0e3)
    } else {
        format!("{v:.0}")
    }
}

/// Human-readable byte size: `512B`, `318M`, `31.2G` (1 decimal only below 100 of a
/// unit, so `137G` not `137.4G` stays compact). Used for memory + disk totals.
fn fmt_bytes(v: f64) -> String {
    if !v.is_finite() {
        return "--".to_string();
    }
    let (val, unit) = if v >= 1.0e9 {
        (v / 1.0e9, 'G')
    } else if v >= 1.0e6 {
        (v / 1.0e6, 'M')
    } else if v >= 1.0e3 {
        (v / 1.0e3, 'K')
    } else {
        return format!("{v:.0}B");
    };
    if val < 100.0 {
        format!("{val:.1}{unit}")
    } else {
        format!("{val:.0}{unit}")
    }
}

/// Human-readable byte rate: `0/s`, `340K/s`, `1.2M/s`.
fn fmt_rate(bps: f64) -> String {
    if !bps.is_finite() {
        return "--".to_string();
    }
    if bps >= 1.0e9 {
        format!("{:.1}G/s", bps / 1.0e9)
    } else if bps >= 1.0e6 {
        format!("{:.1}M/s", bps / 1.0e6)
    } else if bps >= 1.0e3 {
        format!("{:.0}K/s", bps / 1.0e3)
    } else {
        format!("{bps:.0}/s")
    }
}

/// Fixed-width (right-aligned) byte size for the MEM `used/total` cell: pad `used` to a
/// constant column so the `/total` that follows sits at a STABLE x and never slides as
/// `used`'s formatted width changes tick to tick. 5 covers the compact range
/// (`512B`..`31.2G`..`137G`).
fn fmt_bytes_w(v: f64) -> String {
    format!("{:>5}", fmt_bytes(v))
}

/// Fixed-width (right-aligned) byte rate for the NET `↓rx ↑tx` and DISK cells: pad the
/// leading rate to a constant column so the `↑tx` that follows it stays put. 7 covers
/// the common range (`0/s`..`999K/s`..`1.2G/s`); a rare `>100M/s` reading is one wider.
fn fmt_rate_w(bps: f64) -> String {
    format!("{:>7}", fmt_rate(bps))
}

/// Exponential-moving-average smoothing factor for the resource readout's streamed
/// values (CPU/GPU/disk/net rates). At the 300 ms HUD cadence, α=0.3 settles to ~95%
/// of a step in ~2 s — calm enough that the digits don't visibly bounce every tick
/// (the "jumpy numbers" complaint) while still tracking real changes within ~1 s.
const RES_EMA_ALPHA: f64 = 0.3;

/// One EMA step toward `sample`. `None` (no prior value) seeds with the raw sample so
/// the first reading is exact and subsequent readings are smoothed — a persistent,
/// glanceable value instead of a per-tick jitter.
fn res_ema(prev: Option<f64>, sample: f64) -> f64 {
    match prev {
        Some(p) => p + RES_EMA_ALPHA * (sample - p),
        None => sample,
    }
}

/// Is a poll-driven sample stamped at `at` still fresh as of `now`?
fn fresh(at: Option<Instant>, now: Instant) -> bool {
    at.is_some_and(|t| now.saturating_duration_since(t) <= PANEL_STALE)
}

// =============================================================================
// The Panel framework — a stack of themed, MULTI-ROW HUD widgets. Adding one is:
// define a struct, impl `Panel` (rows + paint + optional on_present/poll), register
// it in `App::panels`. Both widgets share the chrome above (RowWriter / colors /
// grade / sparkline / seam), so they look identical and track the theme.
// =============================================================================

/// Stable identity for a HUD widget (config keys, menu toggles, registry lookup).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PanelId {
    /// System-vs-session resource table (CPU / mem / GPU / disk / net).
    Resources,
    /// aterm's own engine: render speed, memory, introspection.
    Engine,
}

impl PanelId {
    /// Every widget id, in registry/stack order (top → bottom). The single source for a
    /// generic surface (the Performance control panel, config-reload sync, introspection)
    /// to iterate widgets without hardcoding the set.
    pub(crate) const ALL: [PanelId; 2] = [PanelId::Resources, PanelId::Engine];

    /// The `aterm.toml` / `Config` key that enables this widget — the single source shared
    /// by config load/reload, the Performance control panel's persist, and introspection.
    pub(crate) fn config_key(self) -> &'static str {
        match self {
            PanelId::Resources => "show_resources_hud",
            PanelId::Engine => "show_engine_hud",
        }
    }

    /// A human label for this widget — shared by the Performance control panel checkboxes,
    /// the View-menu items, and the `controls perf` introspection dump.
    pub(crate) fn label(self) -> &'static str {
        match self {
            PanelId::Resources => "Resources (system vs session)",
            PanelId::Engine => "aterm engine (render / memory)",
        }
    }
}

/// One stackable HUD widget, occupying [`Panel::rows`] bottom rows. `paint` is pure (no
/// I/O); state is fed either at PRESENT time (`on_present`, for the frame-coupled Engine)
/// or on the HUD refresh tick (`poll`, for OS-sampled metrics). Both default to no-op.
pub(crate) trait Panel {
    fn id(&self) -> PanelId;
    fn enabled(&self) -> bool;
    fn set_enabled(&mut self, on: bool);
    /// How many bottom rows this widget reserves when enabled.
    fn rows(&self) -> u16;
    /// Paint into `rows` pre-blanked rows (`rows.len() == self.rows()`, each `cols` wide);
    /// never resize them.
    fn paint(&self, rows: &mut [Vec<RenderCell>], theme: Theme);
    /// Frame-coupled feed (render/present timing). Default: ignored.
    fn on_present(&mut self, _render_ns: u64, _present_ns: u64, _now: Instant) {}
    /// Interval-driven sampling on the HUD tick. Default: ignored.
    fn poll(&mut self, _now: Instant) {}
}

// =============================================================================
// Resources widget — total system vs this terminal session.
// =============================================================================

/// The resource table's columns, left→right, as `(header, width)`. Widths are sized to
/// the widest value the column shows; a column whose start falls past the row edge is
/// simply skipped (graceful right-to-left narrowing on a small window).
const RES_COLS: [(&str, usize); 5] = [
    ("CPU", 7),
    ("MEM", 14),
    ("GPU", 6),
    ("DISK", 10),
    ("NET", 17),
];
/// Leading inset before the scope label.
const SCOPE_X: usize = 1;
/// Width of the `SYSTEM`/`SESSION` scope-label gutter.
const SCOPE_W: usize = 9;

/// Absolute start column of each resource column (shared by the header rule + both scope
/// rows so the table aligns vertically).
fn res_col_x() -> [usize; RES_COLS.len()] {
    let mut xs = [0usize; RES_COLS.len()];
    let mut x = SCOPE_X + SCOPE_W;
    for (i, (_, w)) in RES_COLS.iter().enumerate() {
        xs[i] = x;
        x += w;
    }
    xs
}

/// Which scope a value row reports.
#[derive(Clone, Copy)]
enum Scope {
    System,
    Session,
}

/// Whole-machine vs this-session resource usage, refreshed on the HUD tick. The cheap
/// system figures (CPU / memory / network) are READ from the unified
/// [`crate::metrics_service`] snapshot sampled once earlier in the same tick; the slow
/// probes (IOKit GPU/disk, the session process subtree) come from the BACKGROUND
/// sampler in [`crate::sysmetrics`], so no multi-millisecond syscall ever runs on the
/// event-loop thread. GPU and network have no public per-process counter on macOS, so
/// their SESSION cells read `—`.
pub(crate) struct ResourcePanel {
    enabled: bool,
    ncpu: f64,
    total_mem: Option<u64>,
    // Previous cumulative counters + the WORKER timestamp they were sampled at:
    // rates diff over the sampler's own timestamps, never the poll tick, so a
    // lagged/repeated background sample cannot distort a rate.
    prev_disk: Option<((u64, u64), Instant)>,
    /// Per-pid prior `(cpu_ns, disk_read, disk_write)` so each process in the session
    /// subtree is diffed independently — a child that starts/exits between samples
    /// never spikes or undercounts the session rate.
    prev_procs: HashMap<i32, (u64, u64, u64)>,
    prev_procs_at: Option<Instant>,
    // Latest derived values (`None` until a probe / diff is available → painted `·`).
    cpu_sys: Option<f64>,
    cpu_ses: Option<f64>,
    mem_sys: Option<f64>,
    mem_ses: Option<u64>,
    gpu_sys: Option<f64>,
    disk_sys: Option<f64>,
    disk_ses: Option<f64>,
    net_rx: Option<f64>,
    net_tx: Option<f64>,
    /// Whether the per-session probe is available at all (false off macOS).
    ses_ok: bool,
    /// Last successful poll, for stale → `n/a` decay.
    at: Option<Instant>,
}

impl ResourcePanel {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ncpu: f64::from(crate::sysmetrics::ncpu()).max(1.0),
            total_mem: crate::sysmetrics::mem_total(),
            prev_disk: None,
            prev_procs: HashMap::new(),
            prev_procs_at: None,
            cpu_sys: None,
            cpu_ses: None,
            mem_sys: None,
            mem_ses: None,
            gpu_sys: None,
            disk_sys: None,
            disk_ses: None,
            net_rx: None,
            net_tx: None,
            ses_ok: false,
            at: None,
        }
    }

    /// Paint the labelled separator rule (also the column header): a full-width `─` line
    /// in the dim tone with the column names lifted out of it. This single hairline is the
    /// band's whole visual break from the terminal content above — the interior rows carry
    /// no rule of their own, so the band reads as one calm panel rather than a grid.
    fn paint_rule(&self, row: &mut [RenderCell], theme: Theme) {
        let c = hud_colors(theme);
        // Fill the whole row with the rule glyph in the dim label tone.
        for cellv in row.iter_mut() {
            *cellv = cell('\u{2500}', c.label, c.bar_bg, false, true);
        }
        let len = row.len();
        let mut w = RowWriter::new(row, theme);
        // Column headers, each lifted out of the rule with a space either side. The scope
        // gutter is left as bare rule (the SYSTEM/SESSION rows below label themselves).
        let xs = res_col_x();
        for (i, (lbl, _)) in RES_COLS.iter().enumerate() {
            let x = xs[i];
            if x + lbl.len() + 1 >= len {
                continue;
            }
            w.seek(x.saturating_sub(1));
            w.put(" ", c.label, false);
            w.put(lbl, c.value, true);
            w.put(" ", c.label, false);
        }
    }

    /// Paint one scope row (`SYSTEM` or `SESSION`), each value at its column's fixed x so
    /// the two rows align. Unavailable values render a dim `·`; the session GPU/net cells
    /// (no per-process counter on macOS) render a dim `—`.
    fn paint_scope(&self, row: &mut [RenderCell], theme: Theme, scope: Scope, fresh_now: bool) {
        let xs = res_col_x();
        // Interior row: no top seam (only the rule row draws a line), so re-fill the
        // pre-blanked (seamed) cells with quiet blanks before writing.
        for cellv in row.iter_mut() {
            *cellv = blank_cell_quiet(theme);
        }
        let mut w = RowWriter::new_quiet(row, theme);
        let c = *w.colors();
        let stale = !fresh_now;
        // The "·" placeholder shown before the first sample / a failed probe.
        let dot = |w: &mut RowWriter, label: [u8; 3]| w.put("\u{00b7}", label, false);

        // Scope label — both rows the same dim weight (the bold values carry the emphasis,
        // so the superset SYSTEM never reads weaker than the subset SESSION it contains).
        w.seek(SCOPE_X);
        match scope {
            Scope::System => w.put("SYSTEM", c.label, false),
            Scope::Session => w.put("SESSION", c.label, false),
        }

        // CPU — percent of all cores (left-aligned under the column header).
        w.seek(xs[0]);
        let cpu = match scope {
            Scope::System => self.cpu_sys,
            Scope::Session => self.cpu_ses,
        };
        match (cpu, stale) {
            (Some(f), false) => {
                let col = grade_quiet(f as f32, 0.70, 0.90, &c);
                w.put_num(format_args!("{:.0}%", f * 100.0), col, true);
            }
            _ => dot(&mut w, c.label),
        }

        // MEM — system shows used/total, session shows resident bytes.
        w.seek(xs[1]);
        match scope {
            Scope::System => match (self.mem_sys, self.total_mem, stale) {
                (Some(f), Some(total), false) => {
                    let col = grade_quiet(f as f32, 0.80, 0.92, &c);
                    let used = f * total as f64;
                    w.put(&fmt_bytes_w(used), col, true);
                    w.put("/", c.label, false);
                    w.put(&fmt_bytes(total as f64), c.label, false);
                }
                _ => dot(&mut w, c.label),
            },
            Scope::Session => match (self.mem_ses, stale) {
                (Some(b), false) => {
                    let frac = self.total_mem.map_or(0.0, |t| b as f32 / t as f32);
                    let col = grade_quiet(frac, 0.25, 0.50, &c);
                    w.put(&fmt_bytes(b as f64), col, true);
                }
                _ => dot(&mut w, c.label),
            },
        }

        // GPU — system utilization; no per-process GPU on macOS → session shows `—`.
        w.seek(xs[2]);
        match scope {
            Scope::System => match (self.gpu_sys, stale) {
                (Some(f), false) => {
                    let col = grade_quiet(f as f32, 0.60, 0.85, &c);
                    w.put_num(format_args!("{:.0}%", f * 100.0), col, true);
                }
                _ => dot(&mut w, c.label),
            },
            Scope::Session => w.put("\u{2014}", c.label, false),
        }

        // DISK — throughput (read+write), neutral tone (no obvious health threshold).
        w.seek(xs[3]);
        let disk = match scope {
            Scope::System => self.disk_sys,
            Scope::Session => self.disk_ses,
        };
        match (disk, stale) {
            (Some(bps), false) => w.put(&fmt_rate_w(bps), c.value, false),
            _ => dot(&mut w, c.label),
        }

        // NET — system rx/tx; no per-process net on macOS → session shows `—`.
        w.seek(xs[4]);
        match scope {
            Scope::System => match (self.net_rx, self.net_tx, stale) {
                (Some(rx), Some(tx), false) => {
                    // Rate + unit (`/s`), matching DISK, so throughput is unambiguous.
                    // Fixed-width rates so `↑tx` never slides as `rx`'s width changes.
                    w.put("\u{2193}", c.label, false);
                    w.put(&fmt_rate_w(rx), c.value, false);
                    w.put(" \u{2191}", c.label, false);
                    w.put(&fmt_rate_w(tx), c.value, false);
                }
                _ => dot(&mut w, c.label),
            },
            Scope::Session => w.put("\u{2014}", c.label, false),
        }
    }
}

impl Panel for ResourcePanel {
    fn id(&self) -> PanelId {
        PanelId::Resources
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }
    fn rows(&self) -> u16 {
        3
    }
    fn poll(&mut self, now: Instant) {
        // System CPU / memory / network: READ the unified `metrics_service` snapshot
        // sampled once earlier in the SAME tick (main.rs samples, panels read) — one
        // cpu_ticks/host-VM/getifaddrs probe per tick shared by every consumer instead
        // of a per-panel re-probe. Rates arrive pre-diffed; `Unavailable` (first tick /
        // failed probe) leaves the prior value in place, matching the old diff gating.
        // EMA-smoothed so the readout is a calm, persistent value, not a tick bounce.
        if let Some(g) = crate::metrics_service::global_snapshot() {
            self.mem_sys = g.mem_system.value().map(|s| res_ema(self.mem_sys, s));
            if let Some(f) = g.cpu_system.value() {
                self.cpu_sys = Some(res_ema(self.cpu_sys, f));
            }
            if let (Some(rx), Some(tx)) = (g.net_rx_bps.value(), g.net_tx_bps.value()) {
                self.net_rx = Some(res_ema(self.net_rx, rx));
                self.net_tx = Some(res_ema(self.net_tx, tx));
            }
        }

        // GPU / whole-machine disk / the session process subtree: read the BACKGROUND
        // slow-probe sampler's latest pass — the IOKit registry walks and the
        // per-process rusage sweep are multi-millisecond and must never run on this
        // (event-loop) thread. Rates diff over the WORKER's sample timestamps, never
        // the poll tick, so a lagged or repeated sample cannot distort a rate; a
        // parked/stalled worker fails the TTL filter and decays to honest `n/a`.
        let slow = crate::sysmetrics::slow_probes_latest()
            .filter(|s| now.saturating_duration_since(s.at) <= PANEL_STALE);
        if let Some(s) = slow {
            self.gpu_sys = s.gpu.map(|v| res_ema(self.gpu_sys, v));

            // This session — aterm's process subtree, diffed PER PID so a child that
            // starts or exits between samples never spikes/undercounts the rate (a
            // compile churns hundreds of short-lived processes; a sum-then-diff would
            // read garbage).
            if let Some(procs) = &s.procs {
                self.ses_ok = true;
                // Footprint is instantaneous → a straight sum over the live tree.
                self.mem_ses = Some(procs.iter().map(|p| p.footprint).sum());
                let dt = self
                    .prev_procs_at
                    .map(|p| s.at.saturating_duration_since(p).as_secs_f64())
                    .filter(|&d| d > 0.0);
                if let Some(dt) = dt {
                    let (mut dcpu_ns, mut dio) = (0u64, 0u64);
                    for p in procs {
                        if let Some(&(pc, pr, pw)) = self.prev_procs.get(&p.pid) {
                            dcpu_ns += p.cpu_ns.saturating_sub(pc);
                            dio += p.disk_read.saturating_sub(pr) + p.disk_write.saturating_sub(pw);
                        }
                    }
                    // CPU as a fraction of ALL cores, comparable to the system column.
                    let dcpu = dcpu_ns as f64 / 1.0e9; // cpu-seconds
                    self.cpu_ses = Some(res_ema(
                        self.cpu_ses,
                        ((dcpu / dt) / self.ncpu).clamp(0.0, 1.0),
                    ));
                    self.disk_ses = Some(res_ema(self.disk_ses, dio as f64 / dt));
                }
                // Re-seed the prior table only for a NEW worker sample; re-reading the
                // same sample (worker mid-pass) keeps the settled rates.
                if self.prev_procs_at != Some(s.at) {
                    self.prev_procs = procs
                        .iter()
                        .map(|p| (p.pid, (p.cpu_ns, p.disk_read, p.disk_write)))
                        .collect();
                    self.prev_procs_at = Some(s.at);
                }
            } else {
                self.ses_ok = false;
            }

            // System disk — cumulative bytes diff over the worker timestamps.
            if let Some((r, wr)) = s.disk {
                if let Some(((pr, pw), pat)) = self.prev_disk {
                    let ddt = s.at.saturating_duration_since(pat).as_secs_f64();
                    if ddt > 0.0 {
                        let dio = r.saturating_sub(pr) + wr.saturating_sub(pw);
                        self.disk_sys = Some(res_ema(self.disk_sys, dio as f64 / ddt));
                    }
                }
                self.prev_disk = Some(((r, wr), s.at));
            }
        } else {
            // No fresh background sample (first tick after arming, or the worker is
            // parked/stalled): honest `n/a` for the figures the worker owns.
            self.gpu_sys = None;
            self.ses_ok = false;
        }

        self.at = Some(now);
    }
    fn paint(&self, rows: &mut [Vec<RenderCell>], theme: Theme) {
        let fresh_now = fresh(self.at, Instant::now());
        if let Some(row) = rows.get_mut(0) {
            self.paint_rule(row, theme);
        }
        if let Some(row) = rows.get_mut(1) {
            self.paint_scope(row, theme, Scope::System, fresh_now);
        }
        if let Some(row) = rows.get_mut(2) {
            self.paint_scope(row, theme, Scope::Session, fresh_now && self.ses_ok);
        }
    }
}

// =============================================================================
// Engine widget — aterm's own render speed, memory, and introspection.
// =============================================================================

/// aterm's runtime health on one row: render backend, fps, a frame-time sparkline,
/// frame/present latency, the engine's resident memory, slow-frame count, and any
/// app-fed metric streams (`aterm-ctl metric …`). Frame timing is fed at present; memory
/// + feed are sampled on the HUD tick.
pub(crate) struct EnginePanel {
    enabled: bool,
    samples: HudSamples,
    /// aterm's own physical memory footprint (bytes), sampled on `poll`.
    footprint: Option<u64>,
    /// App-fed metric streams, refreshed on `poll` (so `paint` never takes the store lock).
    feed: Vec<crate::app_fed::StreamView>,
}

impl EnginePanel {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            samples: HudSamples::new(),
            footprint: None,
            feed: Vec::new(),
        }
    }
}

impl Panel for EnginePanel {
    fn id(&self) -> PanelId {
        PanelId::Engine
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }
    fn rows(&self) -> u16 {
        1
    }
    fn on_present(&mut self, render_ns: u64, present_ns: u64, now: Instant) {
        self.samples.record(render_ns, present_ns, now);
    }
    fn poll(&mut self, now: Instant) {
        self.footprint =
            crate::sysmetrics::proc_usage(crate::sysmetrics::self_pid()).map(|u| u.footprint);
        self.feed = crate::app_fed::snapshot(now, 4);
    }
    fn paint(&self, rows: &mut [Vec<RenderCell>], theme: Theme) {
        let Some(row) = rows.get_mut(0) else {
            return;
        };
        let now = Instant::now();
        let m = crate::metrics::snapshot();
        let ms = |ns: u64| ns as f32 / 1.0e6;
        let mut w = RowWriter::new(row, theme);
        let c = *w.colors();

        // Identity + render backend.
        w.put("engine", c.label, true);
        w.sep();
        let (btxt, bcol) = if m.backend_gpu {
            ("gpu", c.good)
        } else {
            ("cpu", c.warn)
        };
        w.put(btxt, bcol, true);
        w.put(" ", c.label, false);
        let fps = self.samples.fps(now);
        w.put_num(
            format_args!("{fps:>3}"),
            grade_lo(fps as f32, 50.0, 30.0, &c),
            true,
        );
        w.put(" fps", c.label, false);
        w.sep();

        // Frame-time sparkline — only when the full trailing block still fits, so the
        // numbers below are never clipped to a stub. Bars are coloured by ABSOLUTE frame
        // health (fast frames stay good even when the auto-scaled bar is tall).
        const TRAILING: usize = 40;
        const MIN_SPARK: usize = 6;
        // Only reserve the sparkline region when there is an actual frame series — at
        // idle / headless (no presents) the row packs left instead of leaving a gap.
        if self.samples.last().is_some() && w.room() >= TRAILING + MIN_SPARK {
            let want = (w.room() - TRAILING).min(24);
            let (levels, spark_ms) = self.samples.spark(want);
            let colors: Vec<[u8; 3]> = spark_ms
                .iter()
                .map(|&x| {
                    if x <= 0.0 {
                        c.label
                    } else {
                        grade_hi(x, 8.0, 16.0, &c)
                    }
                })
                .collect();
            w.sparkline_graded(&levels, &colors);
            w.sep();
        }

        // Frame render time — last (graded) / max (dim).
        let last_frame = ms(self.samples.last().map_or(0, |s| s.render_ns));
        let max_frame = ms(self.samples.max_render_ns());
        w.put("frame ", c.label, false);
        w.put_num(
            format_args!("{last_frame:>5.1}"),
            grade_hi(last_frame, 8.0, 16.0, &c),
            true,
        );
        w.put("/", c.label, false);
        w.put_num(format_args!("{max_frame:>5.1}"), c.label, false);
        w.put(" ms", c.label, false);
        w.sep();

        // Present latency — honest `—` until a real on-glass present exists.
        w.put("lat ", c.label, false);
        if self.samples.any_present() {
            let lat = ms(self.samples.last().map_or(0, |s| s.present_ns));
            w.put_num(
                format_args!("{lat:>4.1}"),
                grade_hi(lat, 8.0, 16.0, &c),
                true,
            );
            w.put(" ms", c.label, false);
        } else {
            w.put("\u{2014}", c.label, false);
        }
        w.sep();

        // aterm's own physical memory footprint.
        w.put("mem ", c.label, false);
        match self.footprint {
            Some(b) => w.put(&fmt_bytes(b as f64), c.value, true),
            None => w.put("n/a", c.label, false),
        }

        // Slow frames — only when non-zero, hot.
        if m.slow_frames > 0 {
            w.sep();
            w.put_num(format_args!("!{} slow", m.slow_frames), c.hot, true);
        }

        // App-fed streams — compact tail, only if any exist and there is room: the
        // stream name, its latest value, the derived per-second rate, and a calm spark.
        if let Some(s) = self.feed.first()
            && w.room() > 14
        {
            w.sep();
            w.put("feed ", c.good, true);
            w.put(&s.name, c.value, false);
            w.put(&format!(" {}", fmt_short(s.last)), c.label, false);
            if s.rate > 0.0 {
                w.put(&format!(" {}/s", fmt_short(s.rate)), c.label, false);
            }
            if w.room() > s.spark.len() {
                w.put(" ", c.label, false);
                let colors = vec![c.good; s.spark.len()];
                w.sparkline_graded(&s.spark, &colors);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paint a fully-populated resource widget for layout assertions.
    fn resource_panel() -> ResourcePanel {
        let mut p = ResourcePanel::new(true);
        p.ncpu = 8.0;
        p.total_mem = Some(137 * 1_000_000_000);
        p.cpu_sys = Some(0.23);
        p.cpu_ses = Some(0.04);
        p.mem_sys = Some(0.21);
        p.mem_ses = Some(318 * 1_000_000);
        p.gpu_sys = Some(0.18);
        p.disk_sys = Some(1_200_000.0);
        p.disk_ses = Some(41_000.0);
        p.net_rx = Some(340_000.0);
        p.net_tx = Some(12_000.0);
        p.ses_ok = true;
        p.at = Some(Instant::now());
        p
    }

    fn paint_panel(p: &dyn Panel, cols: usize, theme: Theme) -> Vec<Vec<RenderCell>> {
        let mut rows: Vec<Vec<RenderCell>> = (0..p.rows() as usize)
            .map(|_| vec![blank_cell(theme); cols])
            .collect();
        p.paint(&mut rows, theme);
        rows
    }

    fn text_of(row: &[RenderCell]) -> String {
        row.iter().map(|c| c.ch).collect()
    }

    #[test]
    fn resource_widget_keeps_the_band_and_seams_only_the_rule_row() {
        let theme = Theme::default();
        let bar_bg = hud_colors(theme).bar_bg;
        let p = resource_panel();
        for cols in [1usize, 10, 40, 66, 80, 200] {
            let rows = paint_panel(&p, cols, theme);
            assert_eq!(rows.len(), 3, "resources widget is three rows");
            for row in &rows {
                assert_eq!(row.len(), cols, "paint must not resize a row");
                for cellv in row {
                    assert_eq!(cellv.bg, bar_bg, "every HUD cell keeps the themed bar bg");
                }
            }
            // Only the rule row (row 0) draws the separator line; the SYSTEM/SESSION rows
            // are seam-free so the band reads as one calm panel, not a grid.
            assert!(
                rows[0].iter().all(|c| c.overline),
                "rule row carries the seam across its full width"
            );
            assert!(
                rows[1].iter().chain(&rows[2]).all(|c| !c.overline),
                "interior scope rows carry no seam"
            );
        }
    }

    #[test]
    fn resource_widget_shows_both_scopes_and_all_columns() {
        let theme = Theme::default();
        let rows = paint_panel(&resource_panel(), 90, theme);
        let header = text_of(&rows[0]);
        let system = text_of(&rows[1]);
        let session = text_of(&rows[2]);
        for col in ["CPU", "MEM", "GPU", "DISK", "NET"] {
            assert!(header.contains(col), "header lists {col}, got {header:?}");
        }
        assert!(
            system.contains("SYSTEM"),
            "system row labelled, got {system:?}"
        );
        assert!(
            session.contains("SESSION"),
            "session row labelled, got {session:?}"
        );
        assert!(
            system.contains("23%"),
            "system CPU rendered, got {system:?}"
        );
        assert!(
            session.contains("4%"),
            "session CPU rendered, got {session:?}"
        );
        // The labelled rule is a real separator line of box-drawing dashes.
        assert!(
            rows[0].iter().filter(|c| c.ch == '\u{2500}').count() > 10,
            "header rule draws a divider line, got {header:?}"
        );
    }

    #[test]
    fn session_gpu_and_net_are_honest_em_dashes() {
        let theme = Theme::default();
        let rows = paint_panel(&resource_panel(), 90, theme);
        let session = text_of(&rows[2]);
        // No public per-process GPU/net on macOS — the session cells say so honestly.
        assert!(
            session.matches('\u{2014}').count() >= 2,
            "session GPU + net render an em-dash, got {session:?}"
        );
    }

    #[test]
    fn stale_resource_samples_decay_to_a_dot() {
        let theme = Theme::default();
        let mut p = resource_panel();
        // Pretend the last successful poll was long ago → every value is stale.
        p.at = Some(Instant::now() - PANEL_STALE - Duration::from_secs(1));
        let rows = paint_panel(&p, 90, theme);
        assert!(
            text_of(&rows[1]).contains('\u{00b7}'),
            "stale system row decays to a dot placeholder"
        );
    }

    #[test]
    fn engine_widget_is_one_row_with_render_stats() {
        let theme = Theme::default();
        let mut p = EnginePanel::new(true);
        let now = Instant::now();
        p.on_present(4_200_000, 800_000, now);
        p.footprint = Some(96 * 1_000_000);
        let rows = paint_panel(&p, 120, theme);
        assert_eq!(rows.len(), 1, "engine widget is one row");
        let line = text_of(&rows[0]);
        assert!(line.contains("engine"), "engine identity, got {line:?}");
        assert!(line.contains("fps"), "fps field, got {line:?}");
        assert!(line.contains("frame"), "frame-time field, got {line:?}");
        assert!(line.contains("mem"), "memory field, got {line:?}");
        assert!(
            line.contains("96"),
            "footprint value rendered, got {line:?}"
        );
    }

    #[test]
    fn engine_latency_is_honest_until_a_real_present() {
        let theme = Theme::default();
        let mut p = EnginePanel::new(true);
        // Headless path: present_ns == 0 → no real on-glass present.
        p.on_present(4_000_000, 0, Instant::now());
        let line = text_of(&paint_panel(&p, 120, theme)[0]);
        assert!(
            line.contains("lat \u{2014}"),
            "headless latency shows an em-dash, got {line:?}"
        );
    }

    #[test]
    fn res_ema_seeds_then_smooths_toward_samples() {
        // First reading is exact (no prior), so the readout starts truthful.
        assert!((res_ema(None, 0.8) - 0.8).abs() < 1e-9);
        // A step from a settled value moves only α of the way — the calm response
        // that stops the per-tick bounce (not a jump to the raw sample).
        let after_one = res_ema(Some(0.0), 1.0);
        assert!((after_one - RES_EMA_ALPHA).abs() < 1e-9, "one step moves α");
        // Repeated identical samples converge toward the value (monotone, bounded).
        let mut v = 0.0;
        for _ in 0..20 {
            v = res_ema(Some(v), 1.0);
        }
        assert!(
            v > 0.99 && v < 1.0,
            "converges toward the sample without overshoot"
        );
    }

    #[test]
    fn tray_multifield_values_are_fixed_width() {
        // The leading value of each multi-field cell is padded to a constant width so
        // the trailing field (MEM `/total`, NET `↑tx`) never slides as the value's
        // formatted width changes tick to tick.
        for &v in &[512.0, 318.0e6, 31.2e9, 137.0e9] {
            assert_eq!(
                fmt_bytes_w(v).chars().count(),
                5,
                "fmt_bytes_w pads to 5: {v}"
            );
        }
        for &r in &[0.0, 999.0e3, 1.2e6, 12.3e6] {
            assert_eq!(
                fmt_rate_w(r).chars().count(),
                7,
                "fmt_rate_w pads to 7: {r}"
            );
        }
    }

    #[test]
    fn poll_samples_decay_to_stale_after_the_ttl() {
        let now = Instant::now();
        assert!(!fresh(None, now), "never-sampled is not fresh");
        assert!(fresh(Some(now), now), "just-sampled is fresh");
        assert!(
            !fresh(Some(now), now + PANEL_STALE + Duration::from_secs(1)),
            "past the TTL is stale → n/a"
        );
    }

    #[test]
    fn sparkline_levels_map_to_block_glyphs() {
        assert_eq!(spark_glyph(0), ' ');
        assert_eq!(spark_glyph(1), '▁');
        assert_eq!(spark_glyph(8), '█');
        assert_eq!(spark_glyph(9), '█');
    }

    #[test]
    fn fmt_bytes_is_compact() {
        assert_eq!(fmt_bytes(318.0 * 1e6), "318M");
        assert_eq!(fmt_bytes(31.2 * 1e9), "31.2G");
        assert_eq!(fmt_bytes(137.0 * 1e9), "137G");
        assert_eq!(fmt_bytes(512.0), "512B");
    }

    #[test]
    fn sparkline_auto_scales_so_uniform_slow_frames_are_not_all_full_block() {
        let mut s = HudSamples::new();
        let now = Instant::now();
        for (i, &ns) in [40_000_000u64, 20_000_000, 40_000_000, 10_000_000]
            .iter()
            .enumerate()
        {
            s.record(ns, 0, now + Duration::from_millis(i as u64 * 10));
        }
        let (levels, _ms) = s.spark(4);
        assert!(
            levels.iter().any(|&l| l < 8),
            "auto-scaled sparkline must not pin every slow frame to a full block: {levels:?}"
        );
        assert!(
            levels.contains(&8),
            "the rolling-max frame should reach the top: {levels:?}"
        );
    }

    /// Every HUD health color (good/warn/hot) and the value tone must stay READABLE
    /// against the bar background on EVERY built-in scheme — dark AND light. The 3.0:1
    /// floor is the WCAG-AA large/bold-text + non-text-contrast threshold (values render
    /// bold), while `hud_colors` itself aims for 4.5:1.
    #[test]
    fn hud_colors_meet_wcag_aa_on_every_builtin_scheme() {
        for name in aterm_types::scheme::builtin_names() {
            let s = aterm_types::scheme::builtin(name).expect("builtin exists");
            let tp = s.to_theme_parts();
            let theme = Theme {
                fg: tp.fg,
                bg: tp.bg,
                cursor: tp.cursor,
                selection: tp.selection,
            };
            let c = hud_colors(theme);
            for (role, fg) in [
                ("good", c.good),
                ("warn", c.warn),
                ("hot", c.hot),
                ("value", c.value),
            ] {
                let ratio = contrast(fg, c.bar_bg);
                assert!(
                    ratio >= 3.0,
                    "{name}: HUD {role} contrast {ratio:.2} < 3.0:1 against the bar"
                );
            }
        }
    }
}
