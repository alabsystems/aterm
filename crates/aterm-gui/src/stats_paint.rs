// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared level-mark painter for the stats Deck, sill, and lens.

use aterm_vitals::Motion;
use aterm_vitals::ink::{HaloMode, Rgb, VitalsInks};
use aterm_vitals::model::Resource;
use aterm_vitals::trend::TrendView;
use aterm_vitals::words;

use crate::type_scale::{StepPx, TypeStep};
use crate::widget::{DrawPrim, Rgba, TextFace, TextWeight, rgba, text_prim};

/// `c` at `alpha` (`0..=1`).
pub(crate) fn a(c: Rgb, alpha: f32) -> Rgba {
    rgba(c, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// A filled rounded rect (negative sizes collapse to nothing).
pub(crate) const fn panel(x: f32, y: f32, w: f32, h: f32, radius: f32, fill: Rgba) -> DrawPrim {
    DrawPrim::Panel {
        x,
        y,
        w: w.max(0.0),
        h: h.max(0.0),
        radius,
        fill,
    }
}

/// A rectangle in device pixels: `(x, y, w, h)`.
pub(crate) type Rect = (f32, f32, f32, f32);

/// Whether `(px, py)` lies inside `r`.
pub(crate) fn contains(r: Rect, px: f64, py: f64) -> bool {
    let (x, y, w, h) = (
        f64::from(r.0),
        f64::from(r.1),
        f64::from(r.2),
        f64::from(r.3),
    );
    px >= x && px < x + w && py >= y && py < y + h
}

/// Which instrument a hit region names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Instrument {
    /// The CPU row, the cores block and the Cores row.
    Cpu,
    /// The GPU row.
    Gpu,
    /// The Memory row.
    Memory,
    /// The Disk row.
    Disk,
    /// The Network row.
    Network,
    /// The Tokens row.
    Tokens,
    /// The Processes row: the jobs engine's marks and the counts.
    Processes,
    /// The header line's "How to read" lens.
    HowToRead,
}

impl Instrument {
    /// The rows' instruments in the lens's ←/→ order.
    pub(crate) const ALL: [Self; 6] = [
        Self::Cpu,
        Self::Gpu,
        Self::Memory,
        Self::Disk,
        Self::Network,
        Self::Processes,
    ];

    /// The instrument of a deck row.
    pub(crate) const fn of(res: Resource) -> Self {
        match res {
            Resource::Cpu => Self::Cpu,
            Resource::Gpu => Self::Gpu,
            Resource::Memory => Self::Memory,
            Resource::Disk => Self::Disk,
            Resource::Network => Self::Network,
            Resource::Tokens => Self::Tokens,
            Resource::Processes => Self::Processes,
        }
    }

    /// The deck row it names; none for How to read.
    pub(crate) const fn resource(self) -> Option<Resource> {
        Some(match self {
            Self::Cpu => Resource::Cpu,
            Self::Gpu => Resource::Gpu,
            Self::Memory => Resource::Memory,
            Self::Disk => Resource::Disk,
            Self::Network => Resource::Network,
            Self::Tokens => Resource::Tokens,
            Self::Processes => Resource::Processes,
            Self::HowToRead => return None,
        })
    }

    /// Its name in words.
    pub(crate) const fn word(self) -> &'static str {
        match self.resource() {
            Some(res) => words::resource_name(res),
            None => "How to read",
        }
    }
}

/// Scope bar a hit region belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Bar {
    /// The machine.
    Global,
    /// This aterm.
    Aterm,
    /// The window's front tab.
    Tab,
}

impl Bar {
    /// The three bars, top to bottom (the owner's order).
    pub(crate) const ALL: [Self; 3] = [Self::Global, Self::Aterm, Self::Tab];
}

/// Target kind of a stats hit region.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HitKind {
    /// A row, or one of its scope cells.
    Row(Instrument, Bar),
    /// The cores block, or the Cores row: the lens on CPU, cores well.
    Cores,
    /// The header line: the How to read lens.
    Header,
}

/// A rectangle a pointer can rest on, and what it names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hit {
    /// In the card's own pixels.
    pub(crate) rect: Rect,
    pub(crate) kind: HitKind,
}

impl Hit {
    /// A row's region, or a scope cell's.
    pub(crate) const fn row(rect: Rect, instrument: Instrument, bar: Bar) -> Self {
        Self {
            rect,
            kind: HitKind::Row(instrument, bar),
        }
    }

    /// The cores' region.
    pub(crate) const fn cores(rect: Rect) -> Self {
        Self {
            rect,
            kind: HitKind::Cores,
        }
    }

    /// The header line's region.
    pub(crate) const fn header(rect: Rect) -> Self {
        Self {
            rect,
            kind: HitKind::Header,
        }
    }

    /// The instrument the lens opens on.
    pub(crate) const fn instrument(self) -> Instrument {
        match self.kind {
            HitKind::Row(instrument, _) => instrument,
            HitKind::Cores => Instrument::Cpu,
            HitKind::Header => Instrument::HowToRead,
        }
    }

    /// The scope the lens opens on.
    pub(crate) const fn bar(self) -> Bar {
        match self.kind {
            HitKind::Row(_, bar) => bar,
            HitKind::Cores | HitKind::Header => Bar::Global,
        }
    }
}

/// Tile detail rung determined by drawn pixel size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Rung {
    /// Under 10 px: the level only.
    Low,
    /// 10–31 px: level, momentum, the glow as a lit top rim, a crisp rim.
    Mid,
    /// 32 px and up: all five channels, the glow as an outside halo.
    Full,
}

impl Rung {
    /// The rung for a tile `t` device px across.
    pub(crate) fn of(t: f32) -> Self {
        if t >= 32.0 {
            Self::Full
        } else if t >= 10.0 {
            Self::Mid
        } else {
            Self::Low
        }
    }
}

/// The level, ghost, and channels of one bar or tile, as fractions of capacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Mark {
    /// Fast average, `0..=1` of capacity.
    pub(crate) level: f32,
    /// Slow average, `0..=1`.
    pub(crate) ghost: f32,
    /// Variance step: 0 crisp, 1 soft, 2 blurred.
    pub(crate) edge: u8,
    /// Rise glow step, 0–3.
    pub(crate) glow: u8,
    /// Which way it moves.
    pub(crate) motion: Motion,
}

impl Mark {
    /// A mark from a trend in `capacity` units with a motion gate in the same units.
    pub(crate) fn of(t: &TrendView, capacity: f32, gate: f32) -> Self {
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

    /// A still mark at `level` (no momentum, crisp).
    pub(crate) const fn still(level: f32) -> Self {
        Self {
            level: level.clamp(0.0, 1.0),
            ghost: level.clamp(0.0, 1.0),
            edge: 0,
            glow: 0,
            motion: Motion::Steady,
        }
    }
}

/// An attention mark drawn as a vector shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum WellMark {
    /// ⊘: a ring with a 45° slash, frost.
    Stuck,
    /// ☠ n: zombies, warn.
    Zombies,
    /// ◔: a ring filling as the suspicion grows, frost at 60 %.
    Quiet,
    /// ◇: waiting for you, the accent.
    Waiting,
    /// A drawn pause: a stopped job.
    Stopped,
    /// ◢: the disk low, filling or stuck.
    Low,
    /// ↑: sending.
    Up,
    /// ↓: receiving.
    Down,
    /// ●: memory pressure, a GPU reset.
    Dot,
    /// ∵: the cause aterm names, three dots (the faces lack the glyph).
    Cause,
}

impl WellMark {
    /// The word a text surface and a reader say for the mark.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Stuck => "stuck",
            Self::Zombies => "zombies",
            Self::Quiet => "quiet",
            Self::Waiting => "waiting for you",
            Self::Stopped => "stopped",
            Self::Low => "low",
            Self::Up => "up",
            Self::Down => "down",
            Self::Dot => "warn",
            Self::Cause => "cause",
        }
    }

    /// The mark the words name.
    pub(crate) const fn of(mark: words::Mark) -> Self {
        match mark {
            words::Mark::Low => Self::Low,
            words::Mark::Dot => Self::Dot,
            words::Mark::Stuck => Self::Stuck,
            words::Mark::Quiet => Self::Quiet,
            words::Mark::Zombie => Self::Zombies,
            words::Mark::Waiting => Self::Waiting,
            words::Mark::Stopped => Self::Stopped,
            words::Mark::Up => Self::Up,
            words::Mark::Down => Self::Down,
            words::Mark::Cause => Self::Cause,
        }
    }

    const fn ink(self, inks: &VitalsInks) -> Rgb {
        match self {
            Self::Stuck => inks.stuck,
            Self::Quiet => inks.quiet,
            Self::Waiting => inks.tab,
            Self::Zombies | Self::Low | Self::Dot => inks.warn,
            Self::Stopped | Self::Up | Self::Down | Self::Cause => inks.words2,
        }
    }
}

/// One attention mark the bars draw: its kind, count, and quiet arc.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StatsMark {
    pub(crate) kind: WellMark,
    pub(crate) count: u32,
    pub(crate) frac: f32,
}

impl StatsMark {
    /// The words a text surface and a reader say: `2 quiet`.
    pub(crate) fn words(self) -> String {
        format!("{} {}", self.count, self.kind.word())
    }
}

/// Default `◔` arc for a quiet row or job whose `φ` was not read.
pub(crate) const QUIET_ARC: f32 = 0.25;

/// How far a quiet job's `◔` arc has filled (`0.0` at `φ = 3`, `1.0` at `φ = 8`).
pub(crate) fn quiet_arc(phi_milli: u64) -> f32 {
    use aterm_vitals::jobs::{PHI_QUIET_MILLI, PHI_STUCK_MILLI};
    (phi_milli.saturating_sub(PHI_QUIET_MILLI) as f32
        / PHI_STUCK_MILLI.saturating_sub(PHI_QUIET_MILLI).max(1) as f32)
        .clamp(0.0, 1.0)
}

/// Attention marks from the jobs view in priority order (`⊘ > ☠ > ◔ > ◇ > stopped`).
pub(crate) fn marks_of(view: &aterm_vitals::jobs::JobsView) -> Vec<StatsMark> {
    use aterm_vitals::jobs::Verdict;
    let count = |v: Verdict| {
        u32::try_from(view.rows.iter().filter(|r| r.verdict == v).count()).unwrap_or(u32::MAX)
    };
    let zombies: u32 = view
        .findings
        .zombies
        .iter()
        .filter(|z| z.tab.is_some())
        .map(|z| z.count)
        .sum();
    let quiet_frac = view
        .rows
        .iter()
        .filter(|r| r.verdict == Verdict::Quiet)
        .filter_map(|r| r.phi_milli)
        .map(quiet_arc)
        .fold(0.0f32, f32::max);
    [
        (WellMark::Stuck, count(Verdict::Stuck), 1.0),
        (WellMark::Zombies, zombies, 1.0),
        (WellMark::Quiet, count(Verdict::Quiet), quiet_frac),
        (WellMark::Waiting, count(Verdict::WaitingYou), 1.0),
        (WellMark::Stopped, count(Verdict::Parked), 1.0),
    ]
    .into_iter()
    .filter(|(_, n, _)| *n > 0)
    .map(|(kind, count, frac)| StatsMark { kind, count, frac })
    .collect()
}

/// Type sizes for figures and labels on the stats surfaces, in device px.
pub(crate) fn stats_type(font_px: f32, s: f32) -> (StepPx, StepPx) {
    (
        TypeStep::Body.px_clamped(font_px * 0.92, 10.5 * s, 11.5 * s),
        TypeStep::Caption.px_clamped(font_px, 9.5 * s, 10.5 * s),
    )
}

/// The painter: a prim list and the inks, scale and type it draws with.
pub(crate) struct Painter<'a> {
    pub(crate) prims: Vec<DrawPrim>,
    pub(crate) inks: &'a VitalsInks,
    /// Device px per point.
    pub(crate) s: f32,
    /// Figures (tabular, mono).
    pub(crate) text_px: StepPx,
    /// Labels and small figures.
    pub(crate) small_px: StepPx,
    /// Differentiate Without Color: falling marks gain a shape too.
    pub(crate) dwc: bool,
}

impl<'a> Painter<'a> {
    /// A painter at scale `s` for a terminal font of `font_px`.
    pub(crate) fn new(inks: &'a VitalsInks, s: f32, font_px: f32, dwc: bool) -> Self {
        let (text_px, small_px) = stats_type(font_px, s);
        Self {
            prims: Vec::with_capacity(512),
            inks,
            s,
            text_px,
            small_px,
            dwc,
        }
    }

    /// `pt` points in whole device px.
    pub(crate) fn px(&self, pt: f32) -> f32 {
        (pt * self.s).round()
    }

    /// One device hairline: a point's worth at 1×, never under a pixel.
    pub(crate) fn hair(&self) -> f32 {
        (self.s / 2.0).round().max(1.0)
    }

    /// A figure in the terminal's tabular mono face.
    pub(crate) fn figure(&mut self, x: f32, baseline: f32, s: String, color: Rgb, small: bool) {
        let size = if small { self.small_px } else { self.text_px };
        self.prims.push(text_prim(
            x.round(),
            baseline.round(),
            s,
            size,
            TextWeight::Regular,
            TextFace::Mono,
            rgba(color, 0xFF),
        ));
    }

    /// A figure's width.
    pub(crate) fn figure_w(&self, s: &str, small: bool) -> f32 {
        let size = if small { self.small_px } else { self.text_px };
        crate::tray_raster::measure_text(s, size.get(), TextWeight::Regular).ceil()
    }

    fn clipped(&mut self, clip: Rect, f: impl FnOnce(&mut Self)) {
        if clip.2 <= 0.0 || clip.3 <= 0.0 {
            return;
        }
        self.prims.push(DrawPrim::ClipPush {
            x: clip.0,
            y: clip.1,
            w: clip.2,
            h: clip.3,
        });
        f(self);
        self.prims.push(DrawPrim::ClipPop);
    }

    /// A warm halo around `rect` fading out over `reach` px, clipped to `clip`.
    pub(crate) fn halo(&mut self, rect: Rect, radius: f32, reach: f32, step: u8, clip: Rect) {
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
        let rise = self.inks.rise;
        let (x, y, w, h) = rect;
        self.clipped(clip, |p| {
            let rings = reach.round().max(1.0) as usize;
            for i in (1..=rings).rev() {
                let d = i as f32;
                let t = d / (reach + 1.0);
                let k = (1.0 - t * t).powi(2) * strength / rings as f32 * 2.2;
                p.prims.push(panel(
                    x - d,
                    y - d,
                    w + 2.0 * d,
                    h + 2.0 * d,
                    radius + d,
                    a(rise, k),
                ));
            }
        });
    }

    /// A dotted axis-aligned line, 2 on 1 off (scaled), `thick` px, in `c`.
    pub(crate) fn dotted(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thick: f32, c: Rgba) {
        let on = self.s.round().max(1.0);
        let off = (self.s / 2.0).round().max(1.0);
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

    /// One square tile `t` px across at `(x, y)`, by its [`Rung`].
    pub(crate) fn tile(&mut self, x: f32, y: f32, t: f32, m: Mark, gutter: f32) {
        self.tile_rect(x, y, t, t, m, gutter, Some(self.inks.load_at(m.level)));
    }

    /// A tile of any aspect (`fill = None` uses the load ramp at `m.level`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tile_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        m: Mark,
        gutter: f32,
        fill: Option<Rgb>,
    ) {
        let inks = *self.inks;
        let rung = Rung::of(w.min(h));
        let r = match rung {
            Rung::Full => (w.min(h) * 0.16).round().max(3.0),
            Rung::Mid => (w.min(h) * 0.18).round().max(2.0),
            Rung::Low => 1.0,
        };
        let fill = fill.unwrap_or_else(|| inks.load_at(m.level));
        let fill_h = (m.level * h).round();
        let tide_y = y + h - fill_h;
        let ghost_y = (y + h - m.ghost * h).round();
        if rung == Rung::Full {
            let half = (gutter / 2.0).floor();
            self.halo(
                (x, y, w, h),
                r,
                half,
                m.glow,
                (x - half, y - half, w + 2.0 * half, h + 2.0 * half),
            );
        }
        self.prims
            .push(panel(x, y, w, h, r, rgba(inks.track, 0xFF)));
        let shape = |p: &mut Self, y0: f32, bh: f32, c: Rgba| {
            p.clipped((x, y0, w, bh), |p| p.prims.push(panel(x, y, w, h, r, c)));
        };
        if fill_h > 0.0 {
            shape(self, tide_y, fill_h, rgba(fill, 0xFF));
        }
        if rung >= Rung::Mid {
            match m.motion {
                Motion::Rising if ghost_y - tide_y >= 2.0 => {
                    shape(
                        self,
                        tide_y,
                        ghost_y - tide_y,
                        rgba(inks.sliver(fill), 0xFF),
                    );
                }
                Motion::Falling if tide_y - ghost_y >= 2.0 => {
                    let pale = inks.load_at(m.ghost);
                    let alpha = if inks.dark { 0.30 } else { 0.35 };
                    shape(self, ghost_y, tide_y - ghost_y, a(pale, alpha));
                }
                _ => {}
            }
        }
        let th = self.hair();
        if fill_h > 0.0 && rung >= Rung::Mid {
            shape(self, tide_y, th, a(inks.tide, 0.85));
        }
        if rung >= Rung::Mid && (ghost_y - tide_y).abs() >= 2.0 && m.ghost > 0.0 {
            let inset = r.max(2.0);
            self.dotted(
                x + inset,
                ghost_y,
                x + w - inset,
                ghost_y,
                th,
                a(inks.tide, 0.6),
            );
        }
        if rung == Rung::Full && m.edge > 0 {
            let feather = if m.edge == 1 {
                (1.5 * self.s).round()
            } else {
                (3.0 * self.s).round()
            };
            let steps = feather as usize;
            self.clipped((x, y, w, h), |p| {
                for i in 0..steps {
                    let d = i as f32;
                    let k = 0.5 * (1.0 - d / feather);
                    p.prims.push(DrawPrim::Stroke {
                        x: x + d,
                        y: y + d,
                        w: w - 2.0 * d,
                        h: h - 2.0 * d,
                        radius: (r - d).max(0.0),
                        width: 1.0,
                        color: a(inks.ground, k),
                    });
                }
            });
        }
        if rung == Rung::Mid && m.glow > 0 {
            let k = [0.35, 0.60, 0.85][usize::from(m.glow.min(3) - 1)];
            shape(self, y, th, a(inks.rise, k));
        }
        if rung >= Rung::Mid && m.motion == Motion::Rising {
            let nw = (w * 0.22).round().max(2.0);
            let nh = (self.s).round().max(2.0);
            self.prims.push(panel(
                x + ((w - nw) / 2.0).round(),
                y - 1.0,
                nw,
                nh + 1.0,
                0.0,
                rgba(inks.ground, 0xFF),
            ));
        }
        if self.dwc && rung >= Rung::Mid && m.motion == Motion::Falling {
            let nw = (w * 0.22).round().max(2.0);
            let nh = (self.s).round().max(2.0);
            self.prims.push(panel(
                x + ((w - nw) / 2.0).round(),
                y + h - nh,
                nw,
                nh + 1.0,
                0.0,
                rgba(inks.ground, 0xFF),
            ));
        }
        if rung >= Rung::Mid && m.edge == 0 {
            self.prims.push(DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius: r,
                width: 1.0,
                color: a(inks.rim, 0.9),
            });
        }
    }

    /// A dashed outline for an unmeasured or withdrawn core.
    pub(crate) fn tile_unmeasured(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let c = a(self.inks.words2, 0.7);
        let th = self.hair();
        self.dotted(x, y, x + w, y, th, c);
        self.dotted(x, y + h - th, x + w, y + h - th, th, c);
        self.dotted(x, y, x, y + h, th, c);
        self.dotted(x + w - th, y, x + w - th, y + h, th, c);
    }

    /// The GPU tank: one fill for the device with a hairline per core group.
    pub(crate) fn tank(&mut self, r: Rect, m: Mark, cores: Option<u16>, gutter: f32) {
        let (x, y, w, h) = r;
        self.tile_rect(x, y, w, h, m, gutter, None);
        if let Some(cores) = cores.filter(|c| (2..=64).contains(c)) {
            let step = h / f32::from(cores);
            if step >= 3.0 {
                let ground = self.inks.ground;
                for i in 1..cores {
                    let ly = (y + step * f32::from(i)).round();
                    self.prims
                        .push(panel(x + 2.0, ly, w - 4.0, 1.0, 0.0, a(ground, 0.45)));
                }
            }
        }
        if m.edge != 0 || Rung::of(w.min(h)) == Rung::Low {
            self.prims.push(DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius: (w.min(h) * 0.16).round().max(1.0),
                width: 1.0,
                color: a(self.inks.rim, 0.9),
            });
        }
    }

    /// One horizontal lane (`groove` draws the empty part as a thin groove).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn lane(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        m: Mark,
        ink_fill: Rgb,
        slot: Rect,
        groove: bool,
    ) {
        let inks = *self.inks;
        let r = (h / 2.0).floor();
        let len = (m.level * w).round();
        let ghost_x = (x + m.ghost * w).round();
        let end = x + len;
        if len > 0.0 && h >= 3.0 {
            let reach = (if inks.dark { 2.0 } else { 3.0 } * self.s).round();
            let gw = len.min(3.0 * h);
            self.halo((end - gw, y, gw, h), r, reach, m.glow, slot);
        }
        if groove {
            let gh = (h / 2.0).round().max(1.0);
            let gy = y + ((h - gh) / 2.0).round();
            self.prims.push(panel(
                x,
                gy,
                w,
                gh,
                (gh / 2.0).floor(),
                rgba(inks.track, 0xFF),
            ));
        } else {
            self.prims
                .push(panel(x, y, w, h, r, rgba(inks.track, 0xFF)));
        }
        if len <= 0.0 {
            return;
        }
        self.clipped((x, y - 1.0, len.max(1.0), h + 2.0), |p| {
            p.prims
                .push(panel(x, y, w.max(len), h, r, rgba(ink_fill, 0xFF)));
        });
        if h >= 2.0 {
            match m.motion {
                Motion::Rising if end - ghost_x >= 2.0 => {
                    self.clipped((ghost_x, y, end - ghost_x, h), |p| {
                        p.prims
                            .push(panel(x, y, len, h, r, rgba(inks.sliver(ink_fill), 0xFF)));
                    });
                }
                Motion::Falling if ghost_x - end >= 2.0 => {
                    let alpha = if inks.dark { 0.30 } else { 0.35 };
                    self.clipped((end, y, ghost_x - end, h), |p| {
                        p.prims.push(panel(x, y, w, h, r, a(ink_fill, alpha)));
                    });
                }
                _ => {}
            }
        }
        if h >= 3.0 && m.edge > 0 {
            let feather = if m.edge == 1 { 3.0 } else { 7.0 } * self.s;
            let feather = feather.min(len).round();
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
        let th = self.hair();
        self.prims
            .push(panel(end - th, y, th, h, 0.0, a(inks.tide, 0.85)));
        if h >= 2.0 && (ghost_x - end).abs() >= 2.0 && m.ghost > 0.0 {
            self.dotted(
                ghost_x,
                y - 1.0,
                ghost_x,
                y + h + 1.0,
                th,
                a(inks.tide, 0.6),
            );
        }
        if h >= 3.0 && m.motion == Motion::Rising {
            let nh = (h * 0.35).round().max(1.0);
            let nw = (self.s).round().max(2.0);
            self.prims.push(panel(
                end - nw,
                y - 1.0,
                nw,
                nh + 1.0,
                0.0,
                rgba(inks.ground, 0xFF),
            ));
        }
        if self.dwc && h >= 3.0 && m.motion == Motion::Falling && len >= h {
            let cap = h.min(len);
            self.prims.push(panel(
                end - cap + 1.0,
                y + 1.0,
                cap - 2.0 - th,
                h - 2.0,
                0.0,
                rgba(inks.track, 0xFF),
            ));
        }
    }

    /// A capacity bracket at a lane's end, highlighted when pinned.
    pub(crate) fn bracket(&mut self, x: f32, y: f32, h: f32, pinned: bool) {
        let w = self.hair().max((self.s * 0.75).round());
        let c = if pinned {
            rgba(self.inks.load[4], 0xFF)
        } else {
            a(self.inks.rim, 0.9)
        };
        self.prims.push(panel(x, y, w, h, 0.0, c));
    }

    /// An attention mark centred at `(cx, cy)` with radius `r`.
    pub(crate) fn mark(&mut self, kind: WellMark, cx: f32, cy: f32, r: f32, frac: f32) {
        let ink = kind.ink(self.inks);
        self.mark_in(kind, (cx, cy, r), frac, ink, (r * 0.28).max(1.0));
    }

    /// A mark at `(cx, cy, r)` in `ink`, its strokes `lw` px (§10's table).
    pub(crate) fn mark_in(
        &mut self,
        kind: WellMark,
        (cx, cy, r): (f32, f32, f32),
        frac: f32,
        ink: Rgb,
        lw: f32,
    ) {
        let c = rgba(ink, 0xFF);
        let line = |x1: f32, y1: f32, x2: f32, y2: f32| DrawPrim::Line {
            x1,
            y1,
            x2,
            y2,
            width: lw,
            color: c,
        };
        match kind {
            WellMark::Stuck => {
                self.prims.push(DrawPrim::Ring {
                    cx,
                    cy,
                    r_outer: r,
                    thickness: lw,
                    track: c,
                    frac: 0.0,
                    fill: c,
                });
                let d = (r - lw / 2.0) * std::f32::consts::FRAC_1_SQRT_2;
                self.prims.push(line(cx - d, cy + d, cx + d, cy - d));
            }
            WellMark::Quiet => {
                self.prims.push(DrawPrim::Ring {
                    cx,
                    cy,
                    r_outer: r,
                    thickness: lw,
                    track: c,
                    frac: 0.0,
                    fill: c,
                });
                self.prims.push(DrawPrim::Ring {
                    cx,
                    cy,
                    r_outer: r - lw,
                    thickness: r - lw,
                    track: rgba(ink, 0),
                    frac: frac.clamp(0.06, 1.0),
                    fill: c,
                });
            }
            WellMark::Waiting => {
                let pts = [(cx, cy - r), (cx + r, cy), (cx, cy + r), (cx - r, cy)];
                for i in 0..4 {
                    let (p0, p1) = (pts[i], pts[(i + 1) % 4]);
                    self.prims.push(line(p0.0, p0.1, p1.0, p1.1));
                }
            }
            WellMark::Zombies => {
                self.prims.push(DrawPrim::Dot {
                    cx,
                    cy: cy - r * 0.1,
                    r: r * 0.8,
                    color: c,
                });
                self.prims.push(panel(
                    cx - r * 0.45,
                    cy + r * 0.3,
                    r * 0.9,
                    r * 0.6,
                    r * 0.15,
                    c,
                ));
                for dx in [-0.32, 0.32] {
                    self.prims.push(DrawPrim::Dot {
                        cx: cx + r * dx,
                        cy: cy - r * 0.1,
                        r: r * 0.2,
                        color: rgba(self.inks.ground, 0xFF),
                    });
                }
            }
            WellMark::Stopped => {
                let bw = (r * 0.42).max(1.0);
                self.prims
                    .push(panel(cx - r * 0.6, cy - r * 0.7, bw, r * 1.4, 0.0, c));
                self.prims
                    .push(panel(cx + r * 0.18, cy - r * 0.7, bw, r * 1.4, 0.0, c));
            }
            WellMark::Low => {
                let (top, bottom, right) = ((cy - r).round(), (cy + r).round(), (cx + r).round());
                let mut y = top;
                while y < bottom {
                    let reach = ((y + 0.5 - (cy - r)) / (2.0 * r)).clamp(0.0, 1.0) * 2.0 * r;
                    let left = (right - reach).round();
                    if right > left {
                        self.prims.push(panel(left, y, right - left, 1.0, 0.0, c));
                    }
                    y += 1.0;
                }
            }
            WellMark::Up | WellMark::Down => {
                let (top, bottom, head) = (cy - r, cy + r, r * 0.24 / 0.36);
                self.prims.push(line(cx, top, cx, bottom));
                let (tip, back) = if kind == WellMark::Down {
                    (bottom, bottom - head)
                } else {
                    (top, top + head)
                };
                self.prims.push(line(cx - head, back, cx, tip));
                self.prims.push(line(cx + head, back, cx, tip));
            }
            WellMark::Dot => self.prims.push(DrawPrim::Dot {
                cx,
                cy,
                r: r * 0.33 / 0.36,
                color: c,
            }),
            WellMark::Cause => {
                let disc = r * 0.33 / 0.36;
                for (dx, dy) in [(-0.5, -0.42), (0.5, -0.42), (0.0, 0.5)] {
                    self.prims.push(DrawPrim::Dot {
                        cx: cx + disc * dx,
                        cy: cy + disc * dy,
                        r: disc * 0.28,
                        color: c,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use aterm_vitals::jobs::{
        Class, Consumer, JobKind, JobRow, JobsView, Tick, Verdict, ZombieParent,
    };

    /// A job of `verdict` in tab 7, quiet for `quiet_ms` at `phi_milli`.
    pub(crate) fn job_row(
        id: &str,
        verdict: Verdict,
        phi_milli: Option<u64>,
        quiet_ms: Option<u64>,
    ) -> JobRow {
        JobRow {
            id: id.into(),
            tab: 7,
            kind: JobKind::Shell,
            consumer: Consumer::default(),
            promoted_ms: None,
            task: None,
            pgid: 300,
            leader: (300, None),
            exe: "make".into(),
            members: 1,
            class: Class::Running,
            wait: None,
            named: None,
            stop: None,
            input: None,
            cpu_mc: None,
            zombies: None,
            standing: None,
            quiet_ms,
            phi_milli,
            mu_ms: None,
            mu_from: None,
            verdict,
            no_progress: false,
            episode: 0,
            exit: None,
            tick: Tick::default(),
        }
    }

    /// Zombies unreaped by `ppid`, in tab `tab` (or outside aterm).
    pub(crate) fn zombies(ppid: u32, count: u32, tab: Option<u64>) -> ZombieParent {
        ZombieParent {
            ppid,
            name: "node".into(),
            count,
            per_min_deci: 0,
            tab,
        }
    }

    #[test]
    fn the_marks_count_the_engines_verdicts_worst_first() {
        let calm = JobsView {
            rows: vec![
                job_row("j1", Verdict::Fine, None, None),
                job_row("j2", Verdict::Busy, Some(1_000), Some(4_000)),
            ],
            findings: aterm_vitals::jobs::Findings {
                zombies: vec![zombies(1, 4, None)],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(marks_of(&calm).is_empty(), "a calm machine has no marks");

        let busy = JobsView {
            rows: vec![
                job_row("j1", Verdict::Parked, None, None),
                job_row("j2", Verdict::Quiet, Some(4_000), Some(30_000)),
                job_row("j3", Verdict::Stuck, Some(9_000), Some(90_000)),
                job_row("j4", Verdict::WaitingYou, Some(0), Some(5_000)),
                job_row("j5", Verdict::Stuck, Some(12_000), Some(200_000)),
            ],
            findings: aterm_vitals::jobs::Findings {
                zombies: vec![zombies(1, 4, None), zombies(300, 3, Some(7))],
                ..Default::default()
            },
            ..Default::default()
        };
        let marks: Vec<(WellMark, u32)> =
            marks_of(&busy).iter().map(|m| (m.kind, m.count)).collect();
        assert_eq!(
            marks,
            [
                (WellMark::Stuck, 2),
                (WellMark::Zombies, 3),
                (WellMark::Quiet, 1),
                (WellMark::Waiting, 1),
                (WellMark::Stopped, 1),
            ],
            "only the zombies a tab's process holds count"
        );
    }

    #[test]
    fn the_quiet_arc_fills_from_the_quiet_line_to_the_stuck_line() {
        assert_eq!(quiet_arc(aterm_vitals::jobs::PHI_QUIET_MILLI), 0.0);
        assert_eq!(quiet_arc(aterm_vitals::jobs::PHI_STUCK_MILLI), 1.0);
        assert_eq!(quiet_arc(20_000), 1.0);
        assert_eq!(quiet_arc(0), 0.0);
        let view = JobsView {
            rows: vec![
                job_row("j1", Verdict::Quiet, Some(4_000), None),
                job_row("j2", Verdict::Quiet, Some(5_500), None),
            ],
            ..Default::default()
        };
        let quiet = marks_of(&view);
        assert_eq!(quiet.len(), 1);
        assert_eq!(quiet[0].frac, quiet_arc(5_500), "the worst φ sets the arc");
        assert!(quiet[0].frac > quiet_arc(4_000));
    }

    fn inks() -> VitalsInks {
        aterm_vitals::ink::vitals_inks([0x30, 0x31, 0x35], [0xF8, 0xF8, 0xF2], [0x50, 0xFA, 0x7B])
    }

    #[test]
    fn the_ladder_is_keyed_to_drawn_size() {
        assert_eq!(Rung::of(48.0), Rung::Full);
        assert_eq!(Rung::of(32.0), Rung::Full);
        assert_eq!(Rung::of(16.0), Rung::Mid);
        assert_eq!(Rung::of(10.0), Rung::Mid);
        assert_eq!(Rung::of(7.0), Rung::Low);
    }

    #[test]
    fn a_tiles_bands_keep_its_rounded_silhouette() {
        let inks = inks();
        let mut p = Painter::new(&inks, 2.0, 24.0, false);
        let m = Mark {
            level: 0.6,
            ghost: 0.3,
            edge: 0,
            glow: 2,
            motion: Motion::Rising,
        };
        p.tile(100.0, 10.0, 48.0, m, 10.0);
        let tile_panels: Vec<f32> = p
            .prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Panel {
                    x, y, w, h, radius, ..
                } if *x == 100.0 && *y == 10.0 && *w == 48.0 && *h == 48.0 => Some(*radius),
                _ => None,
            })
            .collect();
        assert!(
            tile_panels.len() >= 4,
            "track, fill, sliver, tide: {tile_panels:?}"
        );
        assert!(
            tile_panels
                .iter()
                .all(|r| *r == tile_panels[0] && *r >= 3.0)
        );
        for prim in &p.prims {
            if let DrawPrim::Panel { w, radius, y, .. } = prim
                && *radius == 0.0
                && *y >= 10.0
            {
                assert!(
                    *w < 48.0 - 2.0 * tile_panels[0] + 1.0 || *w < 16.0,
                    "{prim:?}"
                );
            }
        }
    }

    #[test]
    fn a_small_tile_shows_the_glow_as_a_top_rim_and_a_tiny_one_only_its_level() {
        let inks = inks();
        let m = Mark {
            level: 0.5,
            ghost: 0.2,
            edge: 2,
            glow: 3,
            motion: Motion::Rising,
        };
        let mut mid = Painter::new(&inks, 2.0, 24.0, false);
        mid.tile(0.0, 0.0, 16.0, m, 6.0);
        let rise = a(inks.rise, 0.85);
        assert!(
            mid.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Panel { fill, .. } if *fill == rise))
        );
        assert!(
            !mid.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Stroke { color, .. } if color[3] < 200)),
            "no feather at 16 px"
        );
        let mut low = Painter::new(&inks, 1.0, 12.0, false);
        low.tile(0.0, 0.0, 7.0, m, 3.0);
        let panels = low
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::Panel { .. }))
            .count();
        assert_eq!(panels, 2, "{:?}", low.prims);
    }

    #[test]
    fn differentiate_without_color_gives_falling_a_shape() {
        let inks = inks();
        let m = Mark {
            level: 0.2,
            ghost: 0.6,
            edge: 0,
            glow: 0,
            motion: Motion::Falling,
        };
        let count = |dwc: bool| {
            let mut p = Painter::new(&inks, 2.0, 24.0, dwc);
            p.tile(0.0, 0.0, 48.0, m, 10.0);
            p.lane(
                0.0,
                80.0,
                300.0,
                10.0,
                m,
                inks.aterm,
                (0.0, 70.0, 300.0, 30.0),
                false,
            );
            p.prims.len()
        };
        assert_eq!(
            count(true),
            count(false) + 2,
            "a bottom notch and a hollow cap"
        );
    }
}
