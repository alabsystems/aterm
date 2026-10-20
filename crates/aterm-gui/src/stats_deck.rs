// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATS DECK: the three bars, Global, aterm and Tab, drawn as pixel
//! graphics under the titlebar (`docs/DESIGN-system-awareness-2026-10-16.md`
//! §12.2; on by default since the owner's 2026-10-19 "make sure that this
//! feature is enabled on BY DEFAULT so that I can SEE IT").
//!
//! The Deck is a band of the window's chrome headroom: [`deck_px`] device
//! pixels added to `MetricsView::head` on a window with glass, so every
//! geometry law that reads the headroom (the grid fit, the frame size, the
//! pointer map, the effects box) counts it with no list of its own. A headless
//! window never has one. It is painted from [`crate::widget::DrawPrim`]s
//! through the shared tray rasterizer, identical on the CPU and GPU paths and
//! in `image` captures, into the window's paint-only `deck_card`, which is
//! folded under any other card (`App::splice_stats_deck`).
//!
//! What it draws, one grammar for tiles and bars (§12.1):
//! * **Cores**: one square per core, grouped by cluster as the OS names them,
//!   fill height and the load ramp for the level, a dotted ghost at the 20 s
//!   average, a warm sliver and a notch when rising, a pale band when falling,
//!   a warm halo round what is rising, a crisp or feathered edge for steady or
//!   jittery. The GPU is one tank beside them.
//! * **Lanes**: per scope, CPU on one cores-worth scale (so Tab ≤ aterm ≤
//!   Global reads as geometry) and memory on one physical-memory scale, with
//!   the same five channels along each bar; a two-minute sparkline of the
//!   scope's CPU; the figures in tabular type.
//!
//! Rich does not mean animated (§12.5, O34): nothing here schedules a frame.
//! A new snapshot re-lays the prims; positions are whole device pixels, so a
//! frame is presented only when a drawn pixel moves (`WindowState::deck_fp`).

use aterm_vitals::Motion;
use aterm_vitals::ink::{self, HaloMode, Rgb, VitalsInks};
use aterm_vitals::model::{ScopeView, Snapshot, TabView};
use aterm_vitals::trend::TrendView;
use aterm_vitals::words;

use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, Rgba, TextFace, TextWeight, rgba, text_prim};

/// The Deck's height in points: three 16 pt bars, 3 pt apart, 4 pt margins
/// (124 device px at 2×, §12.2).
pub(crate) const DECK_PT: f64 = 62.0;

/// Which form the stats bars take (`[sys] stats_bars`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatsBars {
    /// The three bars at full size under the titlebar (the default).
    Deck,
    /// No bars.
    Off,
}

impl StatsBars {
    /// Parse the config value: `"deck"` (default), `"off"`. `"sill"` (the
    /// compact form in the bottom pad, §12.2) is not built yet and reads as
    /// the Deck, so a person who picked it still sees the bars. Unknown values
    /// read as the default.
    pub(crate) fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("false") => {
                Self::Off
            }
            _ => Self::Deck,
        }
    }

    /// The config word for this form.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Deck => "deck",
            Self::Off => "off",
        }
    }

    /// The form a config word names, or `None` for a word that is not one.
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        let word = word.trim();
        [Self::Deck, Self::Off]
            .into_iter()
            .find(|f| f.word().eq_ignore_ascii_case(word))
    }
}

/// The Deck's height in device pixels at `scale`, or 0 when it is off.
pub(crate) fn deck_px(form: StatsBars, scale: f64) -> usize {
    match form {
        StatsBars::Deck if scale.is_finite() && scale > 0.0 => (DECK_PT * scale).round() as usize,
        _ => 0,
    }
}

/// An attached window's chrome headroom in device pixels: the titlebar band
/// (`head_pts`, measured from AppKit or the synthetic strip band) plus the
/// Deck. Every site that derives `MetricsView::head` for a window with glass
/// goes through here, so the Deck is counted by the grid fit, the frame size
/// and the pointer map alike; `head_pts` keeps only the titlebar's part.
pub(crate) fn chrome_head_px(head_pts: f64, scale: f64, form: StatsBars) -> usize {
    (head_pts * scale).round().max(0.0) as usize + deck_px(form, scale)
}

/// Headless Deck seam (development only, [`aterm_types::dev_seam!`]):
/// `$ATERM_STATS_DECK_HEADLESS=1` draws the Deck on a window with no glass
/// when its headroom holds it (`$ATERM_HEADROOM_PX`), so a lab instance's
/// `image` shows the bars. Always off in a shipped binary. Parsed once.
pub(crate) fn deck_on_headless() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_STATS_DECK_HEADLESS")
            .is_some_and(|v| v.to_str().is_some_and(|v| v.trim() == "1"))
    })
}

impl crate::App {
    /// The stats bars' form from the live config.
    pub(crate) fn stats_bars_form(&self) -> StatsBars {
        self.config.stats_bars_or_default()
    }
}

/// Where the Deck is drawn: its card's size in device pixels, the window's
/// side pad (so the bars align with the grid), the scale and the chrome text
/// size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DeckGeom {
    /// Card width (the frame's width).
    pub(crate) w: f32,
    /// Card height ([`deck_px`]).
    pub(crate) h: f32,
    /// The window's side pad.
    pub(crate) pad: f32,
    /// Device pixels per point.
    pub(crate) scale: f32,
    /// The terminal font's pixel size (the chrome text base).
    pub(crate) font_px: f32,
}

/// What the Deck shows: the snapshot, which tab is this window's, and the
/// inks solved for the Deck's ground.
pub(crate) struct DeckInput<'a> {
    /// The latest snapshot, `None` before the first.
    pub(crate) snap: Option<&'a Snapshot>,
    /// This window's active tab's key (its session id).
    pub(crate) tab_key: Option<u64>,
    /// Its label, `Tab 2`.
    pub(crate) tab_label: String,
    /// Inks for the ground.
    pub(crate) inks: VitalsInks,
    /// A hairline between the Deck and the grid.
    pub(crate) seam: Rgb,
}

fn a(c: Rgb, alpha: f32) -> Rgba {
    rgba(c, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn panel(x: f32, y: f32, w: f32, h: f32, radius: f32, fill: Rgba) -> DrawPrim {
    DrawPrim::Panel {
        x,
        y,
        w: w.max(0.0),
        h: h.max(0.0),
        radius,
        fill,
    }
}

/// The level, ghost and channels of one bar or tile, as fractions of its
/// capacity.
#[derive(Clone, Copy, Debug)]
struct Mark {
    /// Fast average, `0..=1` of capacity.
    level: f32,
    /// Slow average, `0..=1`.
    ghost: f32,
    /// Variance step: 0 crisp, 1 soft, 2 blurred.
    edge: u8,
    /// Rise glow step, 0–3.
    glow: u8,
    /// Which way it moves.
    motion: Motion,
}

impl Mark {
    /// A mark from a trend in `capacity` units with a motion gate in the same
    /// units.
    fn of(t: &TrendView, capacity: f32, gate: f32) -> Self {
        let cap = capacity.max(f32::EPSILON);
        let mad = t.mad / cap;
        Self {
            level: (t.level / cap).clamp(0.0, 1.0),
            ghost: (t.slow / cap).clamp(0.0, 1.0),
            edge: if mad < 0.04 {
                0
            } else if mad < 0.12 {
                1
            } else {
                2
            },
            glow: t.glow_step(gate),
            motion: t.motion(gate),
        }
    }
}

struct Painter<'a> {
    prims: Vec<DrawPrim>,
    inks: &'a VitalsInks,
    s: f32,
    text_px: crate::type_scale::StepPx,
    small_px: crate::type_scale::StepPx,
}

impl Painter<'_> {
    fn text(&mut self, x: f32, baseline: f32, s: String, color: Rgb, bold: bool, small: bool) {
        let size = if small { self.small_px } else { self.text_px };
        self.prims.push(text_prim(
            x.round(),
            baseline.round(),
            s,
            size,
            if bold {
                TextWeight::Bold
            } else {
                TextWeight::Regular
            },
            TextFace::Mono,
            rgba(color, 0xFF),
        ));
    }

    fn text_w(&self, s: &str, small: bool) -> f32 {
        let size = if small { self.small_px } else { self.text_px };
        crate::tray_raster::measure_text(s, size.get(), TextWeight::Regular).ceil()
    }

    /// A warm halo round `(x, y, w, h)`: rings of the rise ink fading out over
    /// `reach` px, clipped to `clip`. Light themes lay a veil (Over), dark
    /// themes the same rings brighter (the rasterizer has no additive mode,
    /// so Add is approximated by a stronger Over).
    fn halo(
        &mut self,
        rect: (f32, f32, f32, f32),
        radius: f32,
        reach: f32,
        step: u8,
        clip: (f32, f32, f32, f32),
    ) {
        if step == 0 || reach < 1.0 {
            return;
        }
        let strength = match (self.inks.rise_mode, step) {
            (HaloMode::Add, 1) => 0.25,
            (HaloMode::Add, 2) => 0.45,
            (HaloMode::Add, _) => 0.65,
            (HaloMode::Over, 1) => 0.15,
            (HaloMode::Over, 2) => 0.28,
            (HaloMode::Over, _) => 0.40,
        };
        let (x, y, w, h) = rect;
        self.prims.push(DrawPrim::ClipPush {
            x: clip.0,
            y: clip.1,
            w: clip.2,
            h: clip.3,
        });
        let rings = reach.round().max(1.0) as usize;
        for i in (1..=rings).rev() {
            let d = i as f32;
            // RainHalo's falloff, ((1 − t²)²), over the reach.
            let t = d / (reach + 1.0);
            let k = (1.0 - t * t).powi(2) * strength / rings as f32 * 2.2;
            self.prims.push(panel(
                x - d,
                y - d,
                w + 2.0 * d,
                h + 2.0 * d,
                radius + d,
                a(self.inks.rise, k),
            ));
        }
        self.prims.push(DrawPrim::ClipPop);
    }

    /// A dotted line from `(x0, y0)` to `(x1, y1)` (axis-aligned), 2 on 1 off
    /// device px scaled, in `c`.
    fn dotted(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thick: f32, c: Rgba) {
        let on = (2.0 * self.s / 2.0).max(1.0).round();
        let off = (1.0 * self.s / 2.0).max(1.0).round();
        if (x1 - x0).abs() >= (y1 - y0).abs() {
            let mut x = x0.min(x1);
            while x < x0.max(x1) {
                self.prims
                    .push(panel(x, y0, on.min(x0.max(x1) - x), thick, 0.0, c));
                x += on + off;
            }
        } else {
            let mut y = y0.min(y1);
            while y < y0.max(y1) {
                self.prims
                    .push(panel(x0, y, thick, on.min(y0.max(y1) - y), 0.0, c));
                y += on + off;
            }
        }
    }

    /// One square tile (≥ 32 px draws all five channels, §12.1).
    fn tile(&mut self, x: f32, y: f32, t: f32, m: Mark, gutter: f32) {
        let inks = *self.inks;
        let r = (t * 0.16).round().max(2.0);
        let fill_h = (m.level * t).round();
        let fill = inks.load_at(m.level);
        // Rise glow: an outside halo reaching half the gutter, clipped to the
        // tile's own cell so it never touches a neighbour.
        let half = (gutter / 2.0).floor();
        self.halo(
            (x, y, t, t),
            r,
            half,
            m.glow,
            (x - half, y - half, t + 2.0 * half, t + 2.0 * half),
        );
        // Track, then the fill from the bottom.
        self.prims
            .push(panel(x, y, t, t, r, rgba(inks.track, 0xFF)));
        self.prims.push(DrawPrim::ClipPush { x, y, w: t, h: t });
        if fill_h > 0.0 {
            self.prims
                .push(panel(x, y + t - fill_h, t, fill_h + r, r, rgba(fill, 0xFF)));
        }
        // Momentum: the band between the ghost and the tide.
        let ghost_y = (y + t - m.ghost * t).round();
        let tide_y = y + t - fill_h;
        match m.motion {
            Motion::Rising if (ghost_y - tide_y) >= 2.0 => {
                self.prims.push(panel(
                    x,
                    tide_y,
                    t,
                    ghost_y - tide_y,
                    0.0,
                    rgba(inks.sliver(fill), 0xFF),
                ));
            }
            Motion::Falling if (tide_y - ghost_y) >= 2.0 => {
                let pale = inks.load_at(m.ghost);
                let alpha = if inks.dark { 0.30 } else { 0.35 };
                self.prims
                    .push(panel(x, ghost_y, t, tide_y - ghost_y, 0.0, a(pale, alpha)));
            }
            _ => {}
        }
        // The tide: a line at the fill top.
        if fill_h > 0.0 {
            let th = (self.s).round().max(1.0);
            self.prims
                .push(panel(x, tide_y, t, th, 0.0, a(inks.tide, 0.85)));
        }
        // The ghost: dotted at the slow average, ≥ 2 px from the tide.
        if (ghost_y - tide_y).abs() >= 2.0 && m.ghost > 0.0 {
            let th = (self.s).round().max(1.0);
            self.dotted(
                x + 2.0,
                ghost_y,
                x + t - 2.0,
                ghost_y,
                th,
                a(inks.tide, 0.6),
            );
        }
        // Variance: feather the edge inward (soft 3 px, blurred 6 px).
        let feather = match m.edge {
            0 => 0.0,
            1 => (3.0 * self.s / 2.0).round(),
            _ => (6.0 * self.s / 2.0).round(),
        };
        if feather >= 1.0 {
            let steps = feather as usize;
            for i in 0..steps {
                let d = i as f32;
                let k = 0.5 * (1.0 - d / feather);
                self.prims.push(DrawPrim::Stroke {
                    x: x + d,
                    y: y + d,
                    w: t - 2.0 * d,
                    h: t - 2.0 * d,
                    radius: (r - d).max(0.0),
                    width: 1.0,
                    color: a(inks.ground, k),
                });
            }
        }
        self.prims.push(DrawPrim::ClipPop);
        // Rising: a notch cut into the top edge, a shape cue with no colour.
        if m.motion == Motion::Rising {
            let nw = (t * 0.22).round();
            let nh = (2.0 * self.s / 2.0).round().max(2.0);
            self.prims.push(panel(
                x + (t - nw) / 2.0,
                y - 1.0,
                nw,
                nh + 1.0,
                0.0,
                rgba(inks.ground, 0xFF),
            ));
        }
        // A crisp rim while steady.
        if m.edge == 0 {
            self.prims.push(DrawPrim::Stroke {
                x,
                y,
                w: t,
                h: t,
                radius: r,
                width: 1.0,
                color: a(inks.rim, 0.9),
            });
        }
    }

    /// One horizontal lane (a bar: all five channels along it).
    #[allow(clippy::too_many_arguments)]
    fn lane(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        m: Mark,
        ink_fill: Rgb,
        slot: (f32, f32, f32, f32),
    ) {
        let inks = *self.inks;
        let r = (h / 2.0).floor();
        let len = (m.level * w).round();
        let ghost_x = (x + m.ghost * w).round();
        let end = x + len;
        // Glow round the live end, clipped to the lane's slot.
        if len > 0.0 {
            let reach = if inks.dark { 4.0 } else { 6.0 } * self.s / 2.0;
            self.halo((end - h.min(len), y, h.min(len), h), r, reach, m.glow, slot);
        }
        self.prims
            .push(panel(x, y, w, h, r, rgba(inks.track, 0xFF)));
        self.prims.push(DrawPrim::ClipPush { x, y, w, h });
        if len > 0.0 {
            self.prims
                .push(panel(x - r, y, len + r, h, r, rgba(ink_fill, 0xFF)));
        }
        match m.motion {
            Motion::Rising if end - ghost_x >= 2.0 => {
                self.prims.push(panel(
                    ghost_x,
                    y,
                    end - ghost_x,
                    h,
                    0.0,
                    rgba(inks.sliver(ink_fill), 0xFF),
                ));
            }
            Motion::Falling if ghost_x - end >= 2.0 => {
                let alpha = if inks.dark { 0.30 } else { 0.35 };
                self.prims
                    .push(panel(end, y, ghost_x - end, h, 0.0, a(ink_fill, alpha)));
            }
            _ => {}
        }
        // Variance: the live end fades inward over 0, 6 or 14 px.
        let feather = match m.edge {
            0 => 0.0,
            1 => 6.0 * self.s / 2.0,
            _ => 14.0 * self.s / 2.0,
        }
        .min(len)
        .round();
        if feather >= 1.0 {
            let steps = feather as usize;
            for i in 0..steps {
                let k = 0.75 * (i as f32 + 1.0) / feather;
                self.prims.push(panel(
                    end - feather + i as f32,
                    y,
                    1.0,
                    h,
                    0.0,
                    a(inks.track, k),
                ));
            }
        }
        // The tide: a 1 pt end line.
        if len > 0.0 {
            let th = self.s.round().max(1.0);
            self.prims
                .push(panel(end - th, y, th, h, 0.0, a(inks.tide, 0.85)));
        }
        // The ghost tick, dotted, ≥ 2 px from the tide.
        if (ghost_x - end).abs() >= 2.0 && m.ghost > 0.0 {
            let th = self.s.round().max(1.0);
            self.dotted(
                ghost_x,
                y + 1.0,
                ghost_x,
                y + h - 1.0,
                th,
                a(inks.tide, 0.6),
            );
        }
        self.prims.push(DrawPrim::ClipPop);
        // Rising: a notch in the end cap.
        if m.motion == Motion::Rising && len > 0.0 {
            let nh = (h * 0.3).round().max(2.0);
            let nw = (2.0 * self.s / 2.0).round().max(2.0);
            self.prims.push(panel(
                end - nw,
                y - 1.0,
                nw,
                nh,
                0.0,
                rgba(inks.ground, 0xFF),
            ));
        }
    }

    /// A two-minute sparkline of `values` (each `0..=1` of capacity, `None`
    /// a gap), newest at the right, as whole-pixel columns.
    fn spark(&mut self, x: f32, y: f32, w: f32, h: f32, values: &[Option<f32>], ink_fill: Rgb) {
        let inks = *self.inks;
        self.prims
            .push(panel(x, y, w, h, (h * 0.18).round(), a(inks.track, 0.55)));
        if values.is_empty() || w < 4.0 {
            return;
        }
        let cols = w.floor() as usize;
        let cap = aterm_vitals::trend::HISTORY_LEN;
        // Column c covers samples newest-aligned at the right.
        for c in 0..cols {
            let t0 = c * cap / cols;
            let t1 = ((c + 1) * cap / cols).max(t0 + 1);
            let off = cap.saturating_sub(values.len());
            let mut peak: Option<f32> = None;
            for t in t0..t1 {
                if t < off {
                    continue;
                }
                if let Some(Some(v)) = values.get(t - off) {
                    peak = Some(peak.map_or(*v, |p: f32| p.max(*v)));
                }
            }
            if let Some(v) = peak {
                let vh = (v.clamp(0.0, 1.0) * (h - 2.0)).round();
                if vh >= 1.0 {
                    self.prims.push(panel(
                        x + c as f32,
                        y + h - 1.0 - vh,
                        1.0,
                        vh,
                        0.0,
                        a(ink_fill, 0.85),
                    ));
                }
            }
        }
    }
}

/// The label, CPU figure, memory figure and words for one scope row.
struct Row<'a> {
    label: String,
    scope: Option<&'a ScopeView>,
    cpu_ink: Rgb,
    mem_ink: Rgb,
    cpu_fig: String,
    mem_fig: String,
    extra: String,
}

/// Lay the Deck out and paint it. Pure: the same input gives the same prims,
/// at whole device pixels.
pub(crate) fn deck_prims(g: &DeckGeom, input: &DeckInput<'_>) -> Vec<DrawPrim> {
    let inks = input.inks;
    let s = g.scale.max(0.5);
    let mut p = Painter {
        prims: Vec::with_capacity(512),
        inks: &inks,
        s,
        text_px: TypeStep::Caption.px_clamped(g.font_px, 9.0 * s, 13.0 * s),
        small_px: TypeStep::Caption.px_clamped(g.font_px * 0.9, 8.0 * s, 11.0 * s),
    };
    let (w, h) = (g.w.round(), g.h.round());
    // The ground, and a hairline seam to the grid below.
    p.prims
        .push(panel(0.0, 0.0, w, h, 0.0, rgba(inks.ground, 0xFF)));
    let seam = s.round().max(1.0);
    p.prims
        .push(panel(0.0, h - seam, w, seam, 0.0, rgba(input.seam, 0xFF)));

    let margin_y = (4.0 * s).round();
    let left = g.pad.round().max((8.0 * s).round());
    let right = w - left;
    let Some(snap) = input.snap else {
        let base = crate::tray_raster::row_baseline(0.0, h, p.text_px.get());
        p.text(
            left,
            base,
            "Reading the machine\u{2026}".into(),
            inks.words2,
            false,
            false,
        );
        return p.prims;
    };
    let inner_h = h - 2.0 * margin_y - seam;
    let mut x = left;

    // ── Cores: one square per core, by cluster, in rows of two (more rows
    //    when the cores would take over a third of the width). ──
    if let Some(cpu) = snap.cpu.as_ref() {
        let n = cpu.total().max(1);
        let gap = (5.0 * s).round();
        let label_w = p.text_w("E\u{00D7}6", true) + (4.0 * s).round();
        let mut rows = 2usize;
        let tile = loop {
            let t = ((inner_h - gap * (rows as f32 - 1.0)) / rows as f32).floor();
            let cols: usize = cpu
                .clusters
                .iter()
                .map(|c| c.cores.len().div_ceil(rows))
                .sum();
            let need = cols as f32 * (t + gap) + cpu.clusters.len() as f32 * (label_w + gap * 2.0);
            if need <= (right - left) * 0.34 || rows >= 6 || t < 8.0 * s {
                break t;
            }
            rows += 1;
        };
        let _ = n;
        for cluster in &cpu.clusters {
            let k = cluster.cores.len();
            let cols = k.div_ceil(rows);
            // The cluster's label: its initial over its count.
            let lx = x;
            let top_base = crate::tray_raster::row_baseline(margin_y, tile, p.small_px.get());
            let bot_base =
                crate::tray_raster::row_baseline(margin_y + inner_h - tile, tile, p.small_px.get());
            p.text(lx, top_base, cluster.short.clone(), inks.words, true, true);
            p.text(
                lx,
                bot_base,
                format!("\u{00D7}{k}"),
                inks.words2,
                false,
                true,
            );
            x += label_w;
            for (i, &core) in cluster.cores.iter().enumerate() {
                let (col, row) = (i / rows, i % rows);
                let tx = x + col as f32 * (tile + gap);
                let ty = margin_y + row as f32 * (tile + gap);
                let view = cpu.cores.get(core).copied().unwrap_or_default();
                p.tile(
                    tx,
                    ty,
                    tile,
                    Mark::of(&view.load, 1.0, words::LOAD_GATE),
                    gap,
                );
            }
            x += cols as f32 * (tile + gap) + gap;
        }
        // The GPU: one tank, device-wide (Apple GPUs report device-wide use only).
        if let Some(gpu) = snap.gpu.as_ref() {
            let tw = (tile * 0.62).round();
            let th = inner_h - p.small_px.get() - (3.0 * s).round();
            let m = Mark::of(&gpu.load, 1.0, words::LOAD_GATE);
            let gx = x + p.text_w("GPU", true).max(tw) / 2.0 - tw / 2.0;
            p.halo(
                (gx, margin_y, tw, th),
                (tw * 0.25).round(),
                (gap / 2.0).floor(),
                m.glow,
                (gx - gap, margin_y - gap, tw + 2.0 * gap, th + 2.0 * gap),
            );
            p.prims.push(panel(
                gx,
                margin_y,
                tw,
                th,
                (tw * 0.25).round(),
                rgba(inks.track, 0xFF),
            ));
            let fh = (m.level * th).round();
            if fh > 0.0 {
                p.prims.push(DrawPrim::ClipPush {
                    x: gx,
                    y: margin_y,
                    w: tw,
                    h: th,
                });
                p.prims.push(panel(
                    gx,
                    margin_y + th - fh,
                    tw,
                    fh + tw,
                    (tw * 0.25).round(),
                    rgba(inks.load_at(m.level), 0xFF),
                ));
                p.prims.push(DrawPrim::ClipPop);
            }
            // Compartments: one hairline per GPU core group, so the tank reads
            // as N cores sharing one level.
            if let Some(cores) = gpu.cores.filter(|c| (2..=64).contains(c)) {
                let step = th / f32::from(cores);
                for i in 1..cores {
                    let ly = (margin_y + step * f32::from(i)).round();
                    p.prims.push(panel(
                        gx + 2.0,
                        ly,
                        tw - 4.0,
                        1.0,
                        0.0,
                        a(inks.ground, 0.55),
                    ));
                }
            }
            let pct = format!("{:.0}%", (gpu.load.raw * 100.0).round());
            let lbase = margin_y + inner_h;
            let label = format!("GPU {pct}");
            let lw = p.text_w(&label, true);
            p.text(
                gx + tw / 2.0 - lw / 2.0,
                lbase,
                label,
                inks.words2,
                false,
                true,
            );
            x += p.text_w("GPU 100%", true).max(tw) + gap * 2.0;
        }
        x += (6.0 * s).round();
    }

    // ── The three lanes. ──
    let total_cores = snap.total_cores().map_or(1.0, |n| n as f32).max(1.0);
    let mem_total = snap.mem.as_ref().map_or(0, |m| m.sample.total);
    let tab: Option<&TabView> = input.tab_key.and_then(|k| snap.tab(k));
    let cpu_motion = |t: &TrendView| t.motion(words::CORES_GATE);
    let mut rows: Vec<Row<'_>> = Vec::with_capacity(3);
    {
        let busy = snap.cpu.as_ref().map(|c| c.busy);
        let cpu_fig = busy.map_or_else(
            || "\u{2014}".to_string(),
            |b| words::cores_figure(b.level, snap.total_cores(), cpu_motion(&b)),
        );
        let mem_fig = snap.mem.as_ref().map_or_else(
            || "\u{2014}".to_string(),
            |m| {
                let mut f = format!(
                    "{}/{}G",
                    words::gib_figure(m.sample.used()),
                    words::gib_total(m.sample.total)
                );
                if m.sample.swap_used > 0 {
                    f.push_str(&format!(" swap {}", words::mem(m.sample.swap_used)));
                }
                f
            },
        );
        let extra = snap.volume.as_ref().map_or_else(String::new, |v| {
            let low = v.total > 0 && (v.available as f64) < v.total as f64 * 0.1;
            format!(
                "{}disk {} free",
                if low { "\u{25E2} " } else { "" },
                words::disk(v.available)
            )
        });
        rows.push(Row {
            label: "Global".into(),
            scope: Some(&snap.global),
            cpu_ink: busy.map_or(inks.load[0], |b| inks.load_at(b.level / total_cores)),
            mem_ink: inks.mem[0],
            cpu_fig,
            mem_fig,
            extra,
        });
    }
    {
        let at = snap.aterm.as_ref();
        let cpu_fig = at.map_or_else(
            || "measuring".to_string(),
            |sc| {
                let mut f = words::cores_figure(sc.cores.level, None, cpu_motion(&sc.cores));
                if let Some(b) = snap
                    .cpu
                    .as_ref()
                    .map(|c| c.busy.level)
                    .filter(|b| *b > 0.05)
                {
                    f.push_str(&format!(
                        " {:.0}%",
                        (sc.cores.level / b * 100.0).clamp(0.0, 100.0)
                    ));
                }
                f
            },
        );
        let mem_fig = at
            .and_then(|sc| sc.footprint)
            .map_or_else(String::new, |f| {
                let mut s = words::mem(f);
                if let Some(g) = at
                    .map(|sc| sc.footprint_gib.motion(words::FOOTPRINT_GATE_GIB))
                    .and_then(Motion::glyph)
                {
                    s.push(g);
                }
                s
            });
        let extra = at.map_or_else(String::new, |sc| {
            let mut e = format!("{} proc", sc.procs);
            if let Some((name, c)) = sc.top.as_ref().filter(|(_, c)| *c >= 0.05) {
                e.push_str(&format!(" \u{00B7} {name} {}c", words::cores(*c)));
            }
            e
        });
        rows.push(Row {
            label: "aterm".into(),
            scope: at,
            cpu_ink: inks.aterm,
            mem_ink: inks.aterm,
            cpu_fig,
            mem_fig,
            extra,
        });
    }
    {
        let sc = tab.map(|t| &t.scope);
        let label = tab.map_or_else(|| input.tab_label.clone(), |t| t.label.clone());
        let cpu_fig = sc.map_or_else(
            || "measuring".to_string(),
            |sc| words::cores_figure(sc.cores.level, None, cpu_motion(&sc.cores)),
        );
        let mem_fig = sc
            .and_then(|sc| sc.footprint)
            .map_or_else(String::new, words::mem);
        let extra = tab.map_or_else(String::new, |t| {
            let mut e = t.program.clone();
            if !e.is_empty() {
                e.push_str(" \u{00B7} ");
            }
            e.push_str(&format!("{} proc", t.scope.procs));
            if let Some((name, c)) = t
                .scope
                .top
                .as_ref()
                .filter(|(n, c)| *c >= 0.05 && *n != t.program)
            {
                e.push_str(&format!(" \u{00B7} {name} {}c", words::cores(*c)));
            }
            e
        });
        rows.push(Row {
            label,
            scope: sc,
            cpu_ink: inks.tab,
            mem_ink: inks.tab,
            cpu_fig,
            mem_fig,
            extra,
        });
    }

    let row_gap = (3.0 * s).round();
    let row_h = ((inner_h - 2.0 * row_gap) / 3.0).floor();
    let lane_h = (row_h * 0.46).round().max(4.0);
    let gap = (10.0 * s).round();
    let label_w = rows
        .iter()
        .map(|r| p.text_w(&r.label, false))
        .fold(0.0f32, f32::max)
        + gap;
    let cpu_fig_w = p.text_w("10.0c/10\u{25B4} 100%", false) + gap;
    // The memory figure's column holds the widest figure this machine can
    // show now: swap widens it only while swap is in use.
    let swapping = snap.mem.as_ref().is_some_and(|m| m.sample.swap_used > 0);
    let mem_fig_w = p.text_w(
        if swapping {
            "88.8/128G swap 88.8 GiB"
        } else {
            "888.8 MiB\u{25B4}"
        },
        false,
    ) + gap;
    let room = (right - x - label_w - cpu_fig_w - mem_fig_w).max(0.0);
    // The words (the disk, the busiest process, the tab's program) keep a
    // column of their own, a fixed share of the room so the lanes never
    // move when a name changes; they leave only when the room is too small.
    let words_w = if room > 300.0 * s / 2.0 {
        (room * 0.32)
            .max(p.text_w("disk 888 GB free", false))
            .min(p.text_w(&"m".repeat(44), false))
            .floor()
    } else {
        0.0
    };
    let avail = room - words_w;
    // Width shares of the rest: the CPU lane, the sparkline, the memory lane.
    let (cpu_w, spark_w, mem_w) = if avail > 600.0 * s / 2.0 {
        let spark = (avail * 0.22).floor().min(240.0 * s / 2.0);
        let rest = avail - spark;
        ((rest * 0.54).floor(), spark, (rest * 0.46).floor())
    } else if avail > 180.0 * s / 2.0 {
        ((avail * 0.55).floor(), 0.0, (avail * 0.45).floor())
    } else {
        (avail.floor(), 0.0, 0.0)
    };
    for (i, row) in rows.iter().enumerate() {
        let ry = margin_y + i as f32 * (row_h + row_gap);
        let base = crate::tray_raster::row_baseline(ry, row_h, p.text_px.get());
        let ly = (ry + (row_h - lane_h) / 2.0).round();
        let mut cx = x;
        p.text(
            cx,
            base,
            row.label.clone(),
            if i == 0 { inks.words } else { inks.words2 },
            i == 0,
            false,
        );
        cx += label_w;
        let slot = (
            cx - gap / 2.0,
            ry - row_gap / 2.0,
            cpu_w + gap,
            row_h + row_gap,
        );
        if cpu_w > 0.0 {
            match row.scope {
                Some(sc) => {
                    let m = Mark::of(&sc.cores, total_cores, words::CORES_GATE);
                    p.lane(cx, ly, cpu_w, lane_h, m, row.cpu_ink, slot);
                }
                None => p.prims.push(panel(
                    cx,
                    ly,
                    cpu_w,
                    lane_h,
                    (lane_h / 2.0).floor(),
                    a(inks.track, 0.6),
                )),
            }
            cx += cpu_w + gap / 2.0;
        }
        p.text(cx, base, row.cpu_fig.clone(), inks.words, false, false);
        cx += cpu_fig_w;
        if spark_w > 0.0 {
            if let Some(sc) = row.scope {
                let vals: Vec<Option<f32>> = sc
                    .history
                    .iter()
                    .map(|v| v.map(|c| c / total_cores))
                    .collect();
                p.spark(
                    cx,
                    (ry + row_h * 0.12).round(),
                    spark_w,
                    (row_h * 0.76).round(),
                    &vals,
                    row.cpu_ink,
                );
            }
            cx += spark_w + gap;
        }
        if mem_w > 0.0 {
            if i == 0 {
                if let Some(m) = snap.mem.as_ref() {
                    // The machine's memory by class: used, compressed, cached
                    // files, then free as the track.
                    let total = m.sample.total.max(1) as f32;
                    let r = (lane_h / 2.0).floor();
                    p.prims
                        .push(panel(cx, ly, mem_w, lane_h, r, rgba(inks.track, 0xFF)));
                    p.prims.push(DrawPrim::ClipPush {
                        x: cx,
                        y: ly,
                        w: mem_w,
                        h: lane_h,
                    });
                    let used_plain = m.sample.app.saturating_add(m.sample.wired) as f32;
                    let parts = [
                        (used_plain, inks.mem[0]),
                        (m.sample.compressed as f32, inks.mem[1]),
                        (m.sample.cached as f32, inks.mem[2]),
                    ];
                    let mut mx = cx;
                    for (k, (bytes, c)) in parts.iter().enumerate() {
                        let pw = (bytes / total * mem_w).round().min(cx + mem_w - mx);
                        if pw >= 1.0 {
                            let (px0, pw0) = if k == 0 { (mx - r, pw + r) } else { (mx, pw) };
                            p.prims.push(panel(
                                px0,
                                ly,
                                pw0,
                                lane_h,
                                if k == 0 { r } else { 0.0 },
                                rgba(*c, 0xFF),
                            ));
                            mx += pw;
                        }
                    }
                    p.prims.push(DrawPrim::ClipPop);
                    // The pressure cap: warn and critical take the end.
                    let cap = match m.sample.pressure {
                        aterm_vitals::Pressure::Warn => Some(inks.rise),
                        aterm_vitals::Pressure::Critical => Some(ink::oklch(0.62, 0.2, 28.0)),
                        _ => None,
                    };
                    if let Some(c) = cap {
                        let cw = (4.0 * s / 2.0).round().max(2.0);
                        p.prims
                            .push(panel(cx + mem_w - cw, ly, cw, lane_h, 0.0, rgba(c, 0xFF)));
                    }
                }
            } else if let (Some(sc), true) = (row.scope, mem_total > 0) {
                let mut t = sc.footprint_gib;
                let cap = aterm_vitals::model::gib(mem_total);
                if t.level == 0.0 && sc.footprint.is_some() {
                    t = TrendView::flat(aterm_vitals::model::gib(sc.footprint.unwrap_or(0)));
                }
                let m = Mark::of(&t, cap, words::FOOTPRINT_GATE_GIB);
                let slot = (
                    cx - gap / 2.0,
                    ry - row_gap / 2.0,
                    mem_w + gap,
                    row_h + row_gap,
                );
                p.lane(cx, ly, mem_w, lane_h, m, row.mem_ink, slot);
            }
            cx += mem_w + gap / 2.0;
        }
        if !row.mem_fig.is_empty() {
            p.text(cx, base, row.mem_fig.clone(), inks.words, false, false);
        }
        cx += mem_fig_w;
        if !row.extra.is_empty() && cx + p.text_w("disk", false) < right {
            // Elide the words to what fits.
            let mut words = row.extra.clone();
            while !words.is_empty() && cx + p.text_w(&words, false) > right {
                words.pop();
                while !words.is_char_boundary(words.len()) {
                    words.pop();
                }
            }
            if words.len() < row.extra.len() && !words.is_empty() {
                words.pop();
                words.push('\u{2026}');
            }
            p.text(cx, base, words, inks.words2, false, false);
        }
    }
    p.prims
}

/// A fingerprint of the prims, so an identical layout presents nothing.
/// Every position is a whole device pixel, so the Debug text is stable.
pub(crate) fn prims_fp(prims: &[DrawPrim]) -> u64 {
    let text = format!("{prims:?}");
    aterm_hash::fnv1a_64(text.as_bytes()) | 1
}

/// The Deck's inputs for a window, read off the App: the card geometry, the
/// tab, the inks. `None` when the window shows no Deck.
struct DeckFrame {
    geom: DeckGeom,
    dy: u32,
    tab_key: Option<u64>,
    tab_label: String,
    inks: VitalsInks,
    seam: Rgb,
}

impl crate::App {
    fn deck_frame(&self, wid: crate::WindowId) -> Option<DeckFrame> {
        let form = self.stats_bars_form();
        let ws = self.windows.get(&wid)?;
        if ws.os_window.is_none() && !deck_on_headless() {
            return None;
        }
        let deck = deck_px(form, ws.scale);
        let head = ws.metrics.head;
        if deck == 0 || head < deck || ws.cols == 0 {
            return None;
        }
        let (cw, _) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let w = ws.cols as usize * cw + 2 * pad;
        let palette = self.chrome_palette(crate::app_render::ChromeSurface::Header);
        let band = crate::chrome_band::band_colors(palette);
        let (tab_key, tab_label) = self.deck_tab(wid);
        Some(DeckFrame {
            geom: DeckGeom {
                w: w as f32,
                h: deck as f32,
                pad: pad as f32,
                scale: ws.scale as f32,
                font_px: self.win_font_px(wid),
            },
            dy: u32::try_from(head - deck).unwrap_or(0),
            tab_key,
            tab_label,
            inks: ink::vitals_inks(band.bar_bg, band.value, band.accent),
            seam: band.seam,
        })
    }

    /// Whether the pointer is over the Deck's band, in the frame the
    /// presenters place (`frame_origin`).
    pub(crate) fn pointer_is_over_stats_deck(&self, wid: crate::WindowId) -> bool {
        let Some(f) = self.deck_frame(wid) else {
            return false;
        };
        let Some((x, y)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) else {
            return false;
        };
        let (ox, oy) = self.frame_origin(wid);
        let (fx, fy) = (x - ox as f64, y - oy as f64);
        let top = f64::from(f.dy);
        (top..top + f64::from(f.geom.h)).contains(&fy) && (0.0..f64::from(f.geom.w)).contains(&fx)
    }

    /// A cheap hash of everything the Deck's prims read: the snapshot's
    /// sequence, the geometry and placement, the tab and the inks.
    fn deck_input_key(&self, f: &DeckFrame) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.vitals.snapshot.as_ref().map(|s| s.seq).hash(&mut h);
        for v in [f.geom.w, f.geom.h, f.geom.pad, f.geom.scale, f.geom.font_px] {
            v.to_bits().hash(&mut h);
        }
        f.dy.hash(&mut h);
        f.tab_key.hash(&mut h);
        f.tab_label.hash(&mut h);
        format!("{:?}", f.inks).hash(&mut h);
        f.seam.hash(&mut h);
        h.finish() | 1
    }

    /// The Deck's prims, laid out only when their inputs moved
    /// (`WindowState::deck_memo` keeps the last input key and prims' print).
    fn deck_layout(&self, wid: crate::WindowId, f: &DeckFrame, key: u64) -> Vec<DrawPrim> {
        let input = DeckInput {
            snap: self.vitals.snapshot.as_deref(),
            tab_key: f.tab_key,
            tab_label: f.tab_label.clone(),
            inks: f.inks,
            seam: f.seam,
        };
        let prims = deck_prims(&f.geom, &input);
        if let Some(ws) = self.windows.get(&wid) {
            ws.deck_memo
                .set((key, prims_fp(&prims) ^ u64::from(f.dy).rotate_left(40)));
        }
        prims
    }

    /// The Deck's fingerprint for `wid` (`0` when it shows none), the
    /// `RepaintKey` term: whole-pixel prims, so a new snapshot that moves no
    /// drawn pixel presents nothing.
    pub(crate) fn stats_deck_fp(&self, wid: crate::WindowId) -> u64 {
        let Some(f) = self.deck_frame(wid) else {
            return 0;
        };
        let key = self.deck_input_key(&f);
        if let Some((k, fp)) = self.windows.get(&wid).map(|ws| ws.deck_memo.get())
            && k == key
        {
            return fp;
        }
        let _ = self.deck_layout(wid, &f, key);
        self.windows.get(&wid).map_or(0, |ws| ws.deck_memo.get().1)
    }

    /// Rasterize the Deck into `wid`'s paint-only `deck_card`, re-rasterizing
    /// only when its prims or placement moved. `None` when it shows none.
    pub(crate) fn splice_stats_deck(&mut self, wid: crate::WindowId) {
        let Some(f) = self.deck_frame(wid) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.deck_card = None;
                ws.deck_fold = None;
            }
            return;
        };
        let key = self.deck_input_key(&f);
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.deck_card.as_ref())
            .is_some_and(|c| c.geom == key)
        {
            return;
        }
        let prims = self.deck_layout(wid, &f, key);
        let fp = self.windows.get(&wid).map_or(0, |ws| ws.deck_memo.get().1);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(card) = ws.deck_card.as_mut()
            && card.fp == fp
        {
            // New inputs, the same pixels: keep the raster.
            card.geom = key;
            return;
        }
        let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
            &prims,
            f.geom.w as u32,
            f.geom.h as u32,
            1.0,
            [0, 0, 0, 0],
        );
        ws.deck_card = Some(crate::SettingsCard {
            scrim: None,
            rgba,
            pw,
            ph,
            dx: 0,
            dy: f.dy,
            fp,
            geom: key,
        });
    }

    /// Fold the Deck under the card the window would otherwise present (the
    /// backends take one tray surface), cached by both cards' identities.
    /// Runs after every other card's splice in each paint path.
    pub(crate) fn fold_stats_deck(&mut self, wid: crate::WindowId) {
        use std::hash::{Hash, Hasher};
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some(deck) = ws.deck_card.as_ref() else {
            ws.deck_fold = None;
            return;
        };
        let Some(top) = ws
            .route_card
            .as_ref()
            .or(ws.settings_card.as_ref())
            .or(ws.badge_card.as_ref())
        else {
            ws.deck_fold = None;
            return;
        };
        if top.scrim.is_some() {
            // A modal with a scrim covers the window, the Deck with it.
            ws.deck_fold = None;
            return;
        }
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        for card in [deck, top] {
            card.fp.hash(&mut identity);
            card.geom.hash(&mut identity);
            card.dx.hash(&mut identity);
            card.dy.hash(&mut identity);
            card.pw.hash(&mut identity);
            card.ph.hash(&mut identity);
        }
        let fp = identity.finish() | 1;
        if ws.deck_fold.as_ref().is_some_and(|c| c.fp == fp) {
            return;
        }
        let x0 = deck.dx.min(top.dx);
        let y0 = deck.dy.min(top.dy);
        let x1 = (deck.dx + deck.pw).max(top.dx + top.pw);
        let y1 = (deck.dy + deck.ph).max(top.dy + top.ph);
        let (pw, ph) = (x1 - x0, y1 - y0);
        let Some(len) = usize::try_from(u64::from(pw) * u64::from(ph) * 4).ok() else {
            ws.deck_fold = None;
            return;
        };
        let mut rgba = vec![0; len];
        let ok = crate::tray_raster::composite_rgba_surface(
            &mut rgba,
            (pw, ph),
            (x0, y0),
            &deck.rgba,
            (deck.pw, deck.ph),
            (deck.dx, deck.dy),
        ) && crate::tray_raster::composite_rgba_surface(
            &mut rgba,
            (pw, ph),
            (x0, y0),
            &top.rgba,
            (top.pw, top.ph),
            (top.dx, top.dy),
        );
        ws.deck_fold = ok.then_some(crate::SettingsCard {
            scrim: None,
            rgba,
            pw,
            ph,
            dx: x0,
            dy: y0,
            fp,
            geom: fp,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_vitals::model::{
        ClusterSpec, GpuSample, MemSample, Pressure, ProcSample, Sample, TabRoot, Ticks,
        VolumeSample,
    };

    fn snapshot() -> Snapshot {
        let mut v = aterm_vitals::Vitals::new();
        let tabs = vec![TabRoot {
            key: 7,
            shell_pid: 200,
            label: "Tab 1".into(),
            program: "claude".into(),
        }];
        let mk = |t: u64, busy: &[u64], cpu: &[u64]| Sample {
            at_ns: t * 1_000_000_000,
            cores: Some(
                busy.iter()
                    .map(|&b| Ticks {
                        busy: b,
                        total: t * 100,
                    })
                    .collect(),
            ),
            clusters: vec![
                ClusterSpec {
                    name: "Super".into(),
                    logical: vec![0, 1, 2, 3],
                },
                ClusterSpec {
                    name: "Efficiency".into(),
                    logical: vec![4, 5, 6, 7, 8, 9],
                },
            ],
            mem: Some(MemSample {
                total: 32 << 30,
                app: 12 << 30,
                wired: 4 << 30,
                compressed: 1 << 30,
                cached: 8 << 30,
                free: 7 << 30,
                swap_used: 0,
                swap_total: 0,
                pressure: Pressure::Normal,
            }),
            gpu: Some(GpuSample {
                device_pct: Some(12),
                cores: Some(10),
                name: None,
            }),
            volume: Some(VolumeSample {
                total: 2_000_000_000_000,
                free: 1_810_000_000_000,
                available: 1_810_000_000_000,
                mount: "/".into(),
            }),
            procs: Some(vec![
                ProcSample {
                    pid: 100,
                    ppid: 1,
                    cpu_ns: Some(cpu[0] * 1_000_000),
                    footprint: Some(300 << 20),
                    name: "aterm".into(),
                },
                ProcSample {
                    pid: 200,
                    ppid: 100,
                    cpu_ns: Some(0),
                    footprint: Some(5 << 20),
                    name: "zsh".into(),
                },
                ProcSample {
                    pid: 201,
                    ppid: 200,
                    cpu_ns: Some(cpu[1] * 1_000_000),
                    footprint: Some(400 << 20),
                    name: "claude".into(),
                },
            ]),
            self_pid: 100,
            tabs: tabs.clone(),
        };
        let _ = v.absorb(&mk(10, &[0; 10], &[0, 0]));
        let _ = v.absorb(&mk(11, &[20, 30, 10, 0, 5, 5, 5, 5, 5, 5], &[100, 400]));
        v.absorb(&mk(
            12,
            &[120, 130, 60, 10, 10, 10, 10, 10, 10, 10],
            &[200, 1200],
        ))
    }

    fn input(snap: &Snapshot) -> DeckInput<'_> {
        DeckInput {
            snap: Some(snap),
            tab_key: Some(7),
            tab_label: "Tab 1".into(),
            inks: aterm_vitals::ink::vitals_inks(
                [0x30, 0x31, 0x35],
                [0xF8, 0xF8, 0xF2],
                [0x50, 0xFA, 0x7B],
            ),
            seam: [0x50, 0x50, 0x55],
        }
    }

    const GEOM: DeckGeom = DeckGeom {
        w: 2238.0,
        h: 124.0,
        pad: 24.0,
        scale: 2.0,
        font_px: 26.0,
    };

    #[test]
    fn the_deck_is_62_points_tall_and_off_when_asked() {
        assert_eq!(deck_px(StatsBars::Deck, 2.0), 124);
        assert_eq!(deck_px(StatsBars::Deck, 1.0), 62);
        assert_eq!(deck_px(StatsBars::Off, 2.0), 0);
        assert_eq!(StatsBars::parse(None), StatsBars::Deck);
        assert_eq!(StatsBars::parse(Some("off")), StatsBars::Off);
        assert_eq!(StatsBars::parse(Some("sill")), StatsBars::Deck);
        assert_eq!(StatsBars::parse(Some("nonsense")), StatsBars::Deck);
    }

    #[test]
    fn the_deck_draws_every_core_and_all_three_scopes() {
        let snap = snapshot();
        let prims = deck_prims(&GEOM, &input(&snap));
        let texts: Vec<String> = prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { s, .. } => Some(s.clone()),
                _ => None,
            })
            .collect();
        for want in [
            "Global",
            "aterm",
            "Tab 1",
            "S",
            "\u{00D7}4",
            "E",
            "\u{00D7}6",
            "GPU 12%",
        ] {
            assert!(
                texts.iter().any(|t| t == want),
                "{want:?} missing from {texts:?}"
            );
        }
        assert!(texts.iter().any(|t| t.contains("/10")), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("17.0/32G")), "{texts:?}");
        assert!(
            texts.iter().any(|t| t.contains("disk 1.81 TB free")),
            "{texts:?}"
        );
        assert!(texts.iter().any(|t| t.contains("claude")), "{texts:?}");
        // Everything stays inside the card.
        for p in &prims {
            if let DrawPrim::Panel { x, y, w, h, .. } = p {
                assert!(
                    *x + *w <= GEOM.w + 40.0 && *y + *h <= GEOM.h + 40.0,
                    "{p:?}"
                );
            }
        }
    }

    #[test]
    fn the_same_snapshot_lays_the_same_pixels() {
        let snap = snapshot();
        let a = deck_prims(&GEOM, &input(&snap));
        let b = deck_prims(&GEOM, &input(&snap));
        assert_eq!(prims_fp(&a), prims_fp(&b));
    }

    #[test]
    fn before_the_first_snapshot_the_deck_says_it_is_reading() {
        let empty = Snapshot::default();
        let mut i = input(&empty);
        i.snap = None;
        let prims = deck_prims(&GEOM, &i);
        assert!(
            prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s.starts_with("Reading")))
        );
    }

    #[test]
    fn a_110_column_window_keeps_the_disk_and_the_tab_program() {
        // 110 columns at 2x, as a lab instance and a half-screen window are:
        // the lanes give up width before the words do.
        let snap = snapshot();
        let g = DeckGeom { w: 1698.0, ..GEOM };
        let texts: Vec<String> = deck_prims(&g, &input(&snap))
            .into_iter()
            .filter_map(|p| match p {
                DrawPrim::Text { s, x, .. } => {
                    assert!(x < g.w, "{s:?} at {x}");
                    Some(s)
                }
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("disk")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("claude")), "{texts:?}");
    }

    #[test]
    fn a_narrow_window_still_lays_out_inside_the_card() {
        let snap = snapshot();
        let g = DeckGeom { w: 700.0, ..GEOM };
        let prims = deck_prims(&g, &input(&snap));
        for p in &prims {
            if let DrawPrim::Text { x, .. } = p {
                assert!(*x < g.w, "{p:?}");
            }
        }
    }
}
