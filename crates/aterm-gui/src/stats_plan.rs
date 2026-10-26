// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layout planner for the stats deck.
use crate::deck_face::FaceMetrics;
use crate::stats_deck::{FOLDED_PT, FULL_TABLE_PT};
use crate::stats_paint::Bar;

/// Stats deck layout form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckForm {
    /// Two blocks of four rows in 62 pt.
    #[default]
    Folded,
    /// Header line and seven rows in full height, only where `stats_deck_size` is full.
    Full,
}

/// Configured `[sys] stats_deck_size` preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckSizePref {
    /// `Folded` whatever the window's height (the owner, 2026-10-24: the Full form had too
    /// many details).
    #[default]
    Auto,
    /// Always use `Full`.
    Full,
    /// Always use `Folded`.
    Folded,
}

impl DeckSizePref {
    /// All preferences in menu order.
    pub(crate) const ALL: [Self; 3] = [Self::Auto, Self::Full, Self::Folded];

    /// Parses `[sys] stats_deck_size`, defaulting to `Auto`.
    pub(crate) fn parse(value: Option<&str>) -> Self {
        value.and_then(Self::from_word).unwrap_or_default()
    }

    /// Returns the config string for this preference.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Full => "full",
            Self::Folded => "folded",
        }
    }

    /// Returns the menu title for this preference.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Full => "Full",
            Self::Folded => "Folded",
        }
    }

    /// Parses a strict config word into a [`DeckSizePref`].
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        let word = word.trim();
        Self::ALL
            .into_iter()
            .find(|pref| pref.word().eq_ignore_ascii_case(word))
    }
}

/// The [`DeckForm`] `pref` gives: Full only where chosen, whatever the window's height.
pub(crate) const fn deck_form_for(pref: DeckSizePref) -> DeckForm {
    match pref {
        DeckSizePref::Full => DeckForm::Full,
        DeckSizePref::Auto | DeckSizePref::Folded => DeckForm::Folded,
    }
}

/// Returns the extra height in device pixels required by the full deck.
pub(crate) fn full_delta_px(scale: f64, cell_px: usize) -> usize {
    if !(scale.is_finite() && scale > 0.0) {
        return 0;
    }
    let need = ((FULL_TABLE_PT - FOLDED_PT) * scale).round() as usize;
    let cell = cell_px.max(1);
    need.div_ceil(cell) * cell
}

const X0: f64 = 12.0;
const MIN_TABLE_W: f64 = 160.0;
const DETAILS_MIN: f64 = 120.0;
const DETAILS_MAX: f64 = 560.0;
const GROW_GAP_MAX: f64 = 16.0;
const SLIM_AMOUNT_MIN: f64 = 92.0;
const AMOUNT_DRAWN_MIN: f64 = 40.0;
const BLOCK_GUTTER: f64 = 29.0;
/// The widest Folded block: fwide's columns (480 pt on Helvetica Neue since the glance narrowed
/// them; 528 held 0.134's), the gap and the widest Details. Wider decks centre the table.
const B_MAX: f64 = 480.0 + 10.0 + DETAILS_MAX;
const CORES_BLOCK_W: f64 = 68.0;
/// The amount column where the Full deck shows its glance (wide, mid): `16.5 GiB`, `18 of 200 KB/s
/// peak` (the refined deck, 2026-10-24; 120 pt held 0.134's longer words).
const GLANCE_AMOUNT_W: f64 = 96.0;
/// The aterm and Tab columns there, each: a bare figure (`2.2`, `0.1 MB/s`); 76 pt held 0.134's
/// `0.46 GiB` and `48 processes`.
const GLANCE_SCOPE_W: f64 = 56.0;
/// The strain gauge at the Folded deck's top left (the refined deck's Folded form, 2026-10-24):
/// its dial's radius, pt. The dial is 54 pt across (Full's is 62) and sits 4 pt inside the
/// 62 pt band at its top and its foot, the largest the band holds with room for the bezel's
/// bloom.
pub(crate) const FOLD_GAUGE_R: f64 = 27.0;
/// The dial's centre, pt below the deck's top: the middle of the 62 pt band.
pub(crate) const FOLD_GAUGE_CY: f64 = 31.0;
/// From the dial's right edge to its divider, and from the divider to block A, pt (Full's slot
/// leaves 11 and 9).
const FOLD_GAUGE_GAP: f64 = 9.0;
/// What the gauge takes from the blocks' width: the dial, its divider and the two gaps.
const FOLD_GAUGE_W: f64 = 2.0 * FOLD_GAUGE_R + 2.0 * FOLD_GAUGE_GAP;
const ROW_NAMES: [&str; 8] = [
    "CPU",
    "GPU",
    "Memory",
    "Disk",
    "Network",
    "Tokens",
    "Processes",
    "Cores",
];
pub(crate) const HEADER_PT: f64 = 9.5;
/// What a strip's header keeps clear at its right end for the now marker and its beat
/// ([`crate::stats_live`]), pt.
pub(crate) const MARKER_ROOM: f64 = 8.0;
/// The time scale's ticks after the middle of the span, how long ago each is, most wanted
/// first: the last minute, two minutes, half a minute, five minutes ([`tick_ages`]).
const TICK_AGES: [u32; 4] = [60, 120, 30, 300];
/// A strip under a minute's ticks after the middle of its span: twenty seconds, then ten.
const TICK_AGES_SHORT: [u32; 2] = [20, 10];
/// A Folded strip's column, seconds: one reading's second, at the focused window's cadence of a
/// reading a second, so each reading slides the strip one whole cell left, a step a person sees
/// at a glance (`docs/DESIGN-stats-deck-live-2026-10-24.md` §12). The cells are v2's own (20 on
/// 3 or 4 pt, 30 on 5 pt), so a Folded strip spans 20 or 30 seconds.
pub(crate) const FOLD_SLOT_S: u32 = 1;
/// The most slots a Folded strip draws (fwide's 30), and the one its slide brings in.
pub(crate) const FOLD_SLOTS_MAX: usize = 30;
/// The least room between a tick's words and the words beside them, pt.
const TICK_GAP: f64 = 6.0;
/// How far a tick's words may stand off centre over it, as a share of their width: the tick
/// stays under their middle half, so "–5 min" never reads as a minute later.
const TICK_SLACK: f64 = 0.25;
/// What the header keeps clear before a strip's left end, pt: a key span over the names ends
/// here, and a time label standing beside the strip ends here.
pub(crate) const STRIP_LEAD: f64 = 6.0;
const DETAILS_LONG: &str = "Details · point at a row for more";
const SWATCH_ADVANCE: f64 = 0.62 * HEADER_PT + 3.0;
const MARK_ADVANCE: f64 = 1.1 * HEADER_PT;
pub(crate) const KEY_GAP: f64 = 10.0;

fn name_width(base: f64, pt: f64, m: &FaceMetrics) -> f64 {
    let widest = ROW_NAMES
        .iter()
        .map(|name| m.width(name, pt, true))
        .fold(0.0, f64::max);
    base.max((widest + 1.5).ceil())
}

const fn whole_width(w_pt: f64) -> f64 {
    if w_pt.is_finite() {
        w_pt.max(0.0).floor()
    } else {
        0.0
    }
}

/// Horizontal column span in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Col {
    pub(crate) x: f64,
    pub(crate) w: f64,
}

impl Col {
    const fn new(x: f64, w: f64) -> Self {
        Self { x, w }
    }

    /// Right edge (`x + w`) in points.
    pub(crate) fn end(self) -> f64 {
        self.x + self.w
    }
}

struct Pen(f64);

impl Pen {
    fn col(&mut self, gap: f64, w: f64) -> Col {
        let col = Col::new(self.0 + gap, w);
        self.0 = col.end();
        col
    }
}

/// Heat strip geometry: `cols` columns of `period_s` on a `pitch`, the last of them now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Strip {
    pub(crate) x: f64,
    pub(crate) cols: u32,
    pub(crate) period_s: u32,
    pub(crate) pitch: f64,
    pub(crate) cell: f64,
}

impl Strip {
    /// What a Full strip spans: ten minutes, its cells coarsening as it narrows (v2 §5.5).
    const FULL_SPAN_S: u32 = 600;

    /// A Full strip: ten minutes over `cols` columns.
    fn new(x: f64, cols: u32, pitch: f64) -> Self {
        Self::of(x, cols, Self::FULL_SPAN_S / cols, pitch)
    }

    /// A Folded strip: `cols` slots of [`FOLD_SLOT_S`] on `pitch`, a reading's second each, so it
    /// spans `cols` seconds and each reading slides it one pitch left.
    const fn folded(x: f64, cols: u32, pitch: f64) -> Self {
        Self::of(x, cols, FOLD_SLOT_S, pitch)
    }

    const fn of(x: f64, cols: u32, period_s: u32, pitch: f64) -> Self {
        Self {
            x,
            cols,
            period_s,
            pitch,
            cell: pitch - 1.0,
        }
    }

    /// How far back the strip reaches, seconds: its left end's age when its right end is now.
    pub(crate) const fn span_s(self) -> u32 {
        self.cols.saturating_mul(self.period_s)
    }

    /// How fast its columns glide left, pt a second: one pitch a period (the painter measures
    /// it in the device px it draws, `Deck::live_edge`).
    #[cfg(test)]
    pub(crate) fn pt_per_s(self) -> f64 {
        self.pitch / f64::from(self.period_s.max(1))
    }

    /// Right edge of the last strip cell.
    pub(crate) fn end(self) -> f64 {
        self.x + f64::from(self.cols.saturating_sub(1)) * self.pitch + self.cell
    }

    fn after(self) -> f64 {
        self.x + f64::from(self.cols) * self.pitch
    }
}

/// Vertical row geometry and font sizes for a deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rows {
    /// Baseline of the header line.
    pub(crate) header_baseline: f64,
    /// Top edge of row 0.
    pub(crate) top0: f64,
    /// Vertical pitch between rows.
    pub(crate) pitch: f64,
    /// Height of a strip cell.
    pub(crate) cell: f64,
    /// Total number of rows.
    pub(crate) count: u32,
    /// Text baseline offset from a row's top.
    pub(crate) baseline: f64,
    /// Font size for row names and percentages.
    pub(crate) name_pt: f64,
    /// Font size for resource amounts.
    pub(crate) amount_pt: f64,
    /// Font size for `aterm` and `Tab` figures.
    pub(crate) figure_pt: f64,
    /// Scope bar height.
    pub(crate) bar_h: f64,
}

impl Rows {
    const fn full(kind: FullKind) -> Self {
        let (pitch, cell, count, baseline, bar_h) = match kind {
            FullKind::Wide | FullKind::Mid => (12.0, 9.0, 7, 8.0, 6.0),
            FullKind::Narrow | FullKind::Slim => (10.5, 8.0, 8, 7.0, 5.0),
        };
        let (name_pt, amount_pt, figure_pt) = match kind {
            FullKind::Wide | FullKind::Mid => (10.5, 10.0, 10.0),
            FullKind::Narrow => (10.0, 10.0, 10.0),
            FullKind::Slim => (9.5, 9.5, 9.5),
        };
        Self {
            header_baseline: 11.5,
            top0: 16.0,
            pitch,
            cell,
            count,
            baseline,
            name_pt,
            amount_pt,
            figure_pt,
            bar_h,
        }
    }

    const fn folded(kind: BlockKind) -> Self {
        Self {
            header_baseline: 9.5,
            top0: 14.0,
            pitch: 11.5,
            cell: 8.5,
            count: 4,
            baseline: 7.5,
            name_pt: match kind {
                BlockKind::Wide | BlockKind::Mid => 10.0,
                BlockKind::Narrow | BlockKind::Slim => 9.5,
            },
            amount_pt: 9.5,
            figure_pt: 9.5,
            bar_h: 5.0,
        }
    }

    /// Returns the top y-coordinate of row `r`.
    pub(crate) fn top(&self, r: u32) -> f64 {
        self.top0 + f64::from(r) * self.pitch
    }
}

/// Width tier for the `Full` deck form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FullKind {
    /// Cores block, 60-cell strip with variable pitch, and Details.
    Wide,
    /// Cores block, 60-cell strip at 4 pt pitch, and optional Details.
    Mid,
    /// Cores row, no scope bar, amount, and both figures.
    Narrow,
    /// Cores row, compact scope bar, and amount.
    Slim,
}

impl FullKind {
    /// All full kinds from widest to narrowest.
    pub(crate) const ALL: [Self; 4] = [Self::Wide, Self::Mid, Self::Narrow, Self::Slim];
}

/// Placement of CPU cores in the `Full` deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CoresAt {
    /// The slot left of the rows, past a divider: it holds the strain gauge (the refined deck,
    /// 2026-10-24), and a click in it opens the CPU lens, which keeps the per-core view.
    Block { at: Col, divider: f64 },
    /// Eighth row spanning from the strip to the right margin.
    Row(Col),
}

/// Column layout plan for the `Full` deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FullPlan {
    /// Deck width in whole points.
    pub(crate) w: f64,
    pub(crate) kind: FullKind,
    /// Whether the deck is too narrow to draw anything besides its background.
    pub(crate) ground_only: bool,
    /// Extra width added to each gap right of the strip in wide windows.
    pub(crate) gap_grow: f64,
    /// Horizontal centering offset applied to all columns.
    pub(crate) offset: f64,
    pub(crate) rows: Rows,
    pub(crate) cores: CoresAt,
    pub(crate) name: Col,
    pub(crate) well: Col,
    pub(crate) strip: Strip,
    /// Right-aligned percentage column.
    pub(crate) pct: Option<Col>,
    pub(crate) bar: Option<Col>,
    pub(crate) amount: Option<Col>,
    pub(crate) aterm: Option<Col>,
    pub(crate) tab: Option<Col>,
    pub(crate) details: Option<Col>,
}

impl FullPlan {
    /// Chooses the Cores row's [`CoresMode`] for `clusters`: narrow and slim have the row;
    /// wide and mid draw the strain gauge in [`CoresAt::Block`] and have none.
    pub(crate) fn cores_mode(
        &self,
        clusters: &[Cluster<'_>],
        m: &FaceMetrics,
    ) -> Option<CoresMode> {
        let CoresAt::Row(room) = self.cores else {
            return None;
        };
        Some(cores_row_mode(
            clusters,
            room.w,
            self.rows.cell,
            &[CoresMode::Stack, CoresMode::Tiny],
            CORES_CAPTION_PT,
            m,
        ))
    }

    fn shift(&mut self, dx: f64) {
        self.cores = match self.cores {
            CoresAt::Block { at, divider } => CoresAt::Block {
                at: Col::new(at.x + dx, at.w),
                divider: divider + dx,
            },
            CoresAt::Row(room) => CoresAt::Row(Col::new(room.x + dx, room.w)),
        };
        self.name.x += dx;
        self.well.x += dx;
        self.strip.x += dx;
        for col in [
            &mut self.pct,
            &mut self.bar,
            &mut self.amount,
            &mut self.aterm,
            &mut self.tab,
            &mut self.details,
        ]
        .into_iter()
        .flatten()
        {
            col.x += dx;
        }
    }

    fn grow(&mut self, extra: f64) {
        let grow = (extra / 6.0).min(GROW_GAP_MAX).floor();
        let mut moved = 0.0;
        for col in [
            &mut self.pct,
            &mut self.bar,
            &mut self.amount,
            &mut self.aterm,
            &mut self.tab,
            &mut self.details,
        ]
        .into_iter()
        .flatten()
        {
            moved += grow;
            col.x += moved;
        }
        self.gap_grow = grow;
        self.offset = ((extra - 6.0 * grow) / 2.0).floor();
        self.shift(self.offset);
    }
}

/// Computes the [`FullPlan`] for deck width `w_pt`.
pub(crate) fn plan_full(w_pt: f64, m: &FaceMetrics) -> FullPlan {
    let w = whole_width(w_pt);
    if let Some(plan) = FullKind::ALL
        .into_iter()
        .map(|kind| lay_full(w, kind, m))
        .find_map(|(plan, fits)| fits.then_some(plan))
    {
        return plan;
    }
    let right = w - X0;
    let (mut plan, _) = lay_full(w, FullKind::Slim, m);
    plan.amount = plan.amount.filter(|amount| amount.w >= AMOUNT_DRAWN_MIN);
    plan.bar = plan.bar.filter(|bar| bar.end() <= right);
    plan.pct = plan.pct.filter(|pct| pct.end() <= right);
    plan.ground_only = w < MIN_TABLE_W || plan.strip.end() > right;
    plan
}

fn lay_full(w: f64, kind: FullKind, m: &FaceMetrics) -> (FullPlan, bool) {
    let right = w - X0;
    let rows = Rows::full(kind);
    let (name_x, name_base) = match kind {
        FullKind::Wide | FullKind::Mid => (97.0, 56.0),
        FullKind::Narrow => (X0, 52.0),
        FullKind::Slim => (X0, 48.0),
    };
    let name = Col::new(name_x, name_width(name_base, rows.name_pt, m));
    let well = Col::new(name.end(), 10.0);
    let strip_x = well.end() + if kind == FullKind::Slim { 3.0 } else { 4.0 };
    let strip = match kind {
        FullKind::Wide => {
            let pitch = 5.0 + ((w - 1100.0) / 300.0).floor().clamp(0.0, 5.0);
            Strip::new(strip_x, 60, pitch)
        }
        FullKind::Mid => Strip::new(strip_x, 60, 4.0),
        FullKind::Narrow => Strip::new(strip_x, if w >= 640.0 { 30 } else { 20 }, 5.0),
        FullKind::Slim => Strip::new(strip_x, 20, 4.0),
    };
    let cores = match kind {
        FullKind::Wide | FullKind::Mid => CoresAt::Block {
            at: Col::new(X0, CORES_BLOCK_W),
            divider: 88.0,
        },
        FullKind::Narrow | FullKind::Slim => {
            CoresAt::Row(Col::new(strip.x, (right - strip.x).max(0.0)))
        }
    };
    let mut plan = FullPlan {
        w,
        kind,
        ground_only: false,
        gap_grow: 0.0,
        offset: 0.0,
        rows,
        cores,
        name,
        well,
        strip,
        pct: None,
        bar: None,
        amount: None,
        aterm: None,
        tab: None,
        details: None,
    };
    let mut pen = Pen(strip.after());
    let fits = match kind {
        FullKind::Wide | FullKind::Mid => {
            plan.pct = Some(pen.col(6.0, 30.0));
            let bar_w = if kind == FullKind::Wide { 96.0 } else { 80.0 };
            plan.bar = Some(pen.col(8.0, bar_w));
            plan.amount = Some(pen.col(8.0, GLANCE_AMOUNT_W));
            plan.aterm = Some(pen.col(8.0, GLANCE_SCOPE_W));
            plan.tab = Some(pen.col(8.0, GLANCE_SCOPE_W));
            let rest = right - (pen.0 + 10.0);
            if rest >= DETAILS_MIN {
                plan.details = Some(Col::new(pen.0 + 10.0, rest.min(DETAILS_MAX)));
            }
            let fits = pen.0 <= right && (kind == FullKind::Mid || plan.details.is_some());
            if fits && kind == FullKind::Wide && rest > DETAILS_MAX {
                plan.grow(rest - DETAILS_MAX);
            }
            fits
        }
        FullKind::Narrow => {
            plan.pct = Some(pen.col(6.0, 30.0));
            let amount_w = (right - (pen.0 + 8.0) - (8.0 + 76.0 + 8.0 + 76.0)).min(160.0);
            plan.amount = Some(pen.col(8.0, amount_w));
            plan.aterm = Some(pen.col(8.0, 76.0));
            plan.tab = Some(pen.col(8.0, 76.0));
            let rest = right - (pen.0 + 10.0);
            if amount_w >= 160.0 && rest >= DETAILS_MIN {
                plan.details = Some(Col::new(pen.0 + 10.0, rest.min(DETAILS_MAX)));
            }
            amount_w >= 120.0
        }
        FullKind::Slim => {
            plan.pct = Some(pen.col(4.0, 26.0));
            plan.bar = Some(pen.col(6.0, 24.0));
            let amount_x = pen.0 + 6.0;
            plan.amount = Some(Col::new(amount_x, right - amount_x));
            right - amount_x >= SLIM_AMOUNT_MIN
        }
    };
    (plan, fits)
}

/// Width tier for each block of the `Folded` deck form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BlockKind {
    /// 30-cell strip, amount, both figures, and optional Details.
    Wide,
    /// 20-cell strip, compact scope bar, and amount.
    Mid,
    /// Compact strip, amount, and optional scope bar.
    Narrow,
    /// Strip and percentage only, with state in the header.
    Slim,
}

impl BlockKind {
    /// All block kinds from widest to narrowest.
    pub(crate) const ALL: [Self; 4] = [Self::Wide, Self::Mid, Self::Narrow, Self::Slim];

    /// Candidate [`CoresMode`]s for the Cores row in priority order.
    pub(crate) const fn cores_ladder(self) -> &'static [CoresMode] {
        match self {
            Self::Wide | Self::Mid => &[CoresMode::Big, CoresMode::Stack, CoresMode::Tiny],
            Self::Narrow => &[CoresMode::Stack, CoresMode::Tiny],
            Self::Slim => &[CoresMode::Tiny],
        }
    }

    /// Font size for Cores row captions.
    pub(crate) const fn cores_caption_pt(self) -> f64 {
        match self {
            Self::Slim => 9.0,
            Self::Wide | Self::Mid | Self::Narrow => CORES_CAPTION_PT,
        }
    }
}

/// Column layout plan for one block of the `Folded` deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BlockPlan {
    pub(crate) kind: BlockKind,
    /// Horizontal span of the block.
    pub(crate) at: Col,
    pub(crate) rows: Rows,
    pub(crate) name: Col,
    pub(crate) well: Col,
    pub(crate) strip: Strip,
    /// Right-aligned percentage column, if fitted.
    pub(crate) pct: Option<Col>,
    pub(crate) bar: Option<Col>,
    pub(crate) amount: Option<Col>,
    pub(crate) aterm: Option<Col>,
    pub(crate) tab: Option<Col>,
    pub(crate) details: Option<Col>,
    /// Horizontal span available for Block B's Cores row.
    pub(crate) cores: Col,
}

impl BlockPlan {
    /// Chooses the [`CoresMode`] for Block B's Cores row.
    pub(crate) fn cores_mode(&self, clusters: &[Cluster<'_>], m: &FaceMetrics) -> CoresMode {
        cores_row_mode(
            clusters,
            self.cores.w,
            self.rows.cell,
            self.kind.cores_ladder(),
            self.kind.cores_caption_pt(),
            m,
        )
    }
}

/// Where the strain gauge sits in a Folded deck: at its top left, block A moved right of it
/// (the refined deck, 2026-10-24).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FoldedGauge {
    /// The dial's span across, [`FOLD_GAUGE_R`] each side of its centre.
    pub(crate) at: Col,
    /// X-coordinate of the divider between the dial and block A.
    pub(crate) divider: f64,
}

impl FoldedGauge {
    /// The dial's centre, pt.
    pub(crate) fn centre(&self) -> (f64, f64) {
        (self.at.x + self.at.w / 2.0, FOLD_GAUGE_CY)
    }

    /// The dial's box, pt: `x0, y0, x1, y1` (the painter notes the same box from [`Self::centre`]).
    #[cfg(test)]
    pub(crate) fn dial(&self) -> [f64; 4] {
        let (cx, cy) = self.centre();
        let r = FOLD_GAUGE_R;
        [cx - r, cy - r, cx + r, cy + r]
    }
}

/// Column layout plan for the `Folded` deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FoldedPlan {
    /// Deck width in whole points.
    pub(crate) w: f64,
    /// Whether the deck is too narrow to draw anything besides its background.
    pub(crate) ground_only: bool,
    /// X-coordinate of the vertical divider between the two blocks.
    pub(crate) divider: f64,
    /// The strain gauge at the top left; `None` where not even two slim blocks fit beside it.
    pub(crate) gauge: Option<FoldedGauge>,
    /// Layout plans for Block A and Block B.
    pub(crate) blocks: [BlockPlan; 2],
}

/// Computes the [`FoldedPlan`] for deck width `w_pt`. The gauge comes first: the blocks take the
/// widest kind that fits beside it, and only where not even slim fits beside it (under about
/// 417 pt) does the deck draw no gauge and give the blocks its room.
pub(crate) fn plan_folded(w_pt: f64, m: &FaceMetrics) -> FoldedPlan {
    let w = whole_width(w_pt);
    match lay_folded(w, true, m) {
        (plan, true) => plan,
        (_, false) => lay_folded(w, false, m).0,
    }
}

/// The Folded plan at `w` with the gauge or without it, and whether some block kind fits.
fn lay_folded(w: f64, gauge: bool, m: &FaceMetrics) -> (FoldedPlan, bool) {
    let g = if gauge { FOLD_GAUGE_W } else { 0.0 };
    let mut b = ((w - 2.0 * X0 - BLOCK_GUTTER - g) / 2.0).floor().max(0.0);
    let mut xa = X0 + g;
    if b > B_MAX {
        b = B_MAX;
        xa = X0 + g + ((w - 2.0 * X0 - (g + 2.0 * b + BLOCK_GUTTER)) / 2.0).floor();
    }
    let kind = BlockKind::ALL
        .into_iter()
        .find(|&kind| lay_block(xa, b, kind, m).1);
    let block = |x: f64| match kind {
        Some(kind) => lay_block(x, b, kind, m).0,
        None => {
            let (mut plan, _) = lay_block(x, b, BlockKind::Slim, m);
            plan.pct = None;
            plan
        }
    };
    let blocks = [block(xa), block(xa + b + BLOCK_GUTTER)];
    let plan = FoldedPlan {
        w,
        ground_only: w < MIN_TABLE_W || blocks.iter().any(|blk| blk.strip.end() > blk.at.end()),
        divider: xa + b + 14.0,
        gauge: gauge.then(|| FoldedGauge {
            at: Col::new(xa - g, 2.0 * FOLD_GAUGE_R),
            divider: xa - FOLD_GAUGE_GAP,
        }),
        blocks,
    };
    (plan, kind.is_some())
}

fn lay_block(x: f64, b: f64, kind: BlockKind, m: &FaceMetrics) -> (BlockPlan, bool) {
    let rows = Rows::folded(kind);
    let (name_base, well_w, strip_gap, cols, pitch) = match kind {
        BlockKind::Wide => (52.0, 10.0, 4.0, 30, 5.0),
        BlockKind::Mid => (52.0, 10.0, 4.0, 20, 4.0),
        BlockKind::Narrow => (48.0, 7.0, 3.0, 20, 3.0),
        BlockKind::Slim => (48.0, 7.0, 2.0, 20, 3.0),
    };
    let name = Col::new(x, name_width(name_base, rows.name_pt, m));
    let well = Col::new(name.end(), well_w);
    // The block's strip keeps v2's cells (`cols` on `pitch`, 60 to 150 pt), each a reading's
    // second (§12 of the live edge's design).
    let strip = Strip::folded(well.end() + strip_gap, cols, pitch);
    let e = strip.after();
    let end = x + b;
    let cores_x = if kind == BlockKind::Slim {
        name.x
    } else {
        strip.x
    };
    let mut plan = BlockPlan {
        kind,
        at: Col::new(x, b),
        rows,
        name,
        well,
        strip,
        pct: None,
        bar: None,
        amount: None,
        aterm: None,
        tab: None,
        details: None,
        cores: Col::new(cores_x, (end - cores_x).max(0.0)),
    };
    let fits = match kind {
        BlockKind::Wide => {
            // The glance's columns, as Full wide's: 117, 70 and 69 pt held 0.134's words.
            plan.pct = Some(Col::new(e + 6.0, 30.0));
            let amount = Col::new(e + 44.0, GLANCE_AMOUNT_W);
            plan.amount = Some(amount);
            let aterm = Col::new(amount.end() + 6.0, GLANCE_SCOPE_W);
            plan.aterm = Some(aterm);
            let tab = Col::new(aterm.end() + 6.0, GLANCE_SCOPE_W);
            plan.tab = Some(tab);
            let spare = end - tab.end();
            if spare >= 130.0 {
                plan.details = Some(Col::new(tab.end() + 10.0, DETAILS_MAX.min(spare - 10.0)));
            }
            spare >= 0.0
        }
        BlockKind::Mid => {
            plan.pct = Some(Col::new(e + 5.0, 28.0));
            plan.bar = Some(Col::new(e + 39.0, 28.0));
            let amount = Col::new(e + 73.0, (end - (e + 73.0)).min(160.0));
            plan.amount = Some(amount);
            amount.w >= 100.0
        }
        BlockKind::Narrow => {
            plan.pct = Some(Col::new(e + 4.0, 26.0));
            let mut amount_x = e + 35.0;
            if end - (amount_x + 29.0) >= SLIM_AMOUNT_MIN {
                plan.bar = Some(Col::new(amount_x, 24.0));
                amount_x += 29.0;
            }
            let amount = Col::new(amount_x, (end - amount_x).min(140.0));
            plan.amount = Some(amount);
            amount.w >= SLIM_AMOUNT_MIN
        }
        BlockKind::Slim => {
            let pct = Col::new(e + 3.0, 26.0);
            plan.pct = Some(pct);
            pct.end() <= end
        }
    };
    (plan, fits)
}

/// Minimum widths at which each full and folded kind fits on a face.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Breakpoints {
    /// Minimum deck widths for [`FullKind::ALL`].
    pub(crate) full: [f64; 4],
    /// Minimum block widths for [`BlockKind::ALL`].
    pub(crate) block: [f64; 4],
    /// Minimum deck widths (`2B + 53` and the gauge's 72) for [`BlockKind::ALL`].
    pub(crate) folded: [f64; 4],
    /// Minimum deck width (`2B + 53`) for slim blocks without the gauge.
    pub(crate) folded_bare: f64,
}

/// Scans whole-point widths to find the [`Breakpoints`] for `m`.
#[cfg(test)]
pub(crate) fn breakpoints(m: &FaceMetrics) -> Breakpoints {
    let first = |fits: &dyn Fn(f64) -> bool| {
        (0..=8192_u32)
            .map(f64::from)
            .find(|&w| fits(w))
            .unwrap_or(f64::INFINITY)
    };
    let full = FullKind::ALL.map(|kind| first(&|w| lay_full(w, kind, m).1));
    let block = BlockKind::ALL.map(|kind| first(&|b| lay_block(X0, b, kind, m).1));
    let folded = block.map(|b| 2.0 * b + 2.0 * X0 + BLOCK_GUTTER + FOLD_GAUGE_W);
    Breakpoints {
        full,
        block,
        folded,
        folded_bare: block[3] + block[3] + 2.0 * X0 + BLOCK_GUTTER,
    }
}

const CORES_CAPTION_PT: f64 = 9.5;

const CORES_NOT_READ: [&str; 2] = ["cores not read", "not read"];

/// CPU core cluster in OS performance order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cluster<'a> {
    pub(crate) count: usize,
    /// Cluster name (`"Performance"`), or `None` when unnamed.
    pub(crate) name: Option<&'a str>,
}

/// Display mode for CPU cores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CoresMode {
    /// Single row of cell-high squares at 10 pt pitch.
    Big,
    /// Two stacked rows of half-height squares.
    Stack,
    /// Three stacked rows of 2 pt squares at 2.5 pt pitch.
    Tiny,
    /// Text-only core count when no square layout fits.
    CountOnly,
}

fn total_cores(clusters: &[Cluster<'_>]) -> usize {
    clusters.iter().map(|c| c.count).sum()
}

/// Returns the width required by a Cores row in `mode`, or `f64::INFINITY` if it cannot fit.
pub(crate) fn cores_row_width(
    clusters: &[Cluster<'_>],
    mode: CoresMode,
    cell: f64,
    caption_pt: f64,
    m: &FaceMetrics,
) -> f64 {
    let squares = |count: usize| {
        let n = count as f64;
        match mode {
            CoresMode::Big => (n - 1.0) * 10.0 + cell,
            CoresMode::Stack => {
                let side = (cell - 0.5) / 2.0;
                let cols = (n / 2.0).ceil();
                cols * side + (cols - 1.0) * 0.5
            }
            CoresMode::Tiny => ((n / 3.0).ceil() - 1.0) * 2.5 + 2.0,
            CoresMode::CountOnly => f64::INFINITY,
        }
    };
    match mode {
        CoresMode::CountOnly => m.width(&count_words(clusters), caption_pt, false),
        CoresMode::Big | CoresMode::Stack | CoresMode::Tiny => {
            let each = clusters.iter().map(|cluster| {
                let count = format!("{} ", cluster.count);
                m.width(&count, caption_pt, true)
                    + cluster
                        .name
                        .map_or(0.0, |name| m.width(name, caption_pt, false))
                    + 3.0
                    + squares(cluster.count)
            });
            each.sum::<f64>() + 8.0 * clusters.len().saturating_sub(1) as f64
        }
    }
}

/// Chooses the first [`CoresMode`] in `ladder` that fits `room`, or `CountOnly`.
pub(crate) fn cores_row_mode(
    clusters: &[Cluster<'_>],
    room: f64,
    cell: f64,
    ladder: &[CoresMode],
    caption_pt: f64,
    m: &FaceMetrics,
) -> CoresMode {
    if total_cores(clusters) == 0 {
        return CoresMode::CountOnly;
    }
    ladder
        .iter()
        .copied()
        .find(|&mode| cores_row_width(clusters, mode, cell, caption_pt, m) <= room)
        .unwrap_or(CoresMode::CountOnly)
}

fn count_words(clusters: &[Cluster<'_>]) -> String {
    match total_cores(clusters) {
        0 => CORES_NOT_READ[1].to_owned(),
        total => format!("{total} cores"),
    }
}

/// Formats the text for a `CountOnly` Cores row within `room` points.
pub(crate) fn cores_count_text(
    clusters: &[Cluster<'_>],
    room: f64,
    caption_pt: f64,
    m: &FaceMetrics,
) -> String {
    if total_cores(clusters) == 0 {
        return m.fit(&CORES_NOT_READ, room, caption_pt, false);
    }
    let total = count_words(clusters);
    let named: Vec<String> = clusters
        .iter()
        .filter_map(|c| c.name.map(|name| format!("{} {name}", c.count)))
        .collect();
    if named.is_empty() {
        return m.fit(&[&total], room, caption_pt, false);
    }
    let each = format!("{total}: {}", named.join(", "));
    m.fit(&[&each, &total], room, caption_pt, false)
}

/// Kind of label placed on the header line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Label {
    /// How far back the strip reaches, `"10 min ago"` ([`time_words`]), over or beside it.
    Time,
    /// Right-aligned `"now"` over a percentage column.
    Now,
    /// Scope swatch and label for `Bar`.
    Scope(Bar),
    /// Details column header.
    Details,
    /// Worst state summary in an `fslim` block header.
    State,
}

/// Positioned label on the header line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeaderLabel {
    pub(crate) label: Label,
    /// Left edge (or right edge for [`Label::Now`]) in points.
    pub(crate) x: f64,
    pub(crate) text: String,
}

/// Dynamic text inputs for the header line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HeaderText<'a> {
    /// Active tab label (`"Tab 2"`).
    pub(crate) tab: &'a str,
    /// The machine's noun (`"this Mac"` or `"this computer"`): the Global label where the deck
    /// says the glance (Full wide and mid, and Folded), and nothing elsewhere.
    pub(crate) machine: &'a str,
    /// Whether a strip on screen draws an over mark. Where the deck says the glance (Full wide
    /// and mid, and Folded) the header keys "over max" only then, and keys nothing else (the
    /// refined deck, 2026-10-24: a legend learned once need not stay); Full narrow and slim key
    /// all three marks by rule K.
    pub(crate) over: bool,
}

/// Worst-state strings for an `fslim` block header.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StateLabel<'a> {
    /// Full `<Resource> <word> +N` string.
    pub(crate) full: &'a str,
    /// Bare `<Resource> <word>` fallback string.
    pub(crate) bare: &'a str,
}

/// Legend item in the header key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyItem {
    /// Heat ramp from `"cold"` to `"hot"`.
    Ramp,
    /// `"over max"` cell indicator.
    Over,
    /// `"not read"` cell indicator.
    NotRead,
}

impl KeyItem {
    const SEARCH: [Self; 3] = [Self::Over, Self::Ramp, Self::NotRead];
    const DRAW: [Self; 3] = [Self::Ramp, Self::Over, Self::NotRead];

    const fn weight(self) -> u32 {
        match self {
            Self::Over => 4,
            Self::Ramp => 2,
            Self::NotRead => 1,
        }
    }

    /// Returns the advance width of this key item on the header line.
    pub(crate) fn width(self, m: &FaceMetrics) -> f64 {
        let words = |s: &str| m.width(s, HEADER_PT, false);
        match self {
            Self::Ramp => words("cold") + 3.0 + 35.0 + 3.0 + words("hot"),
            Self::Over => 7.0 + words("over max"),
            Self::NotRead => 7.0 + words("not read"),
        }
    }
}

/// Free horizontal span on the header line available for key items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct KeySpan {
    pub(crate) x0: f64,
    pub(crate) x1: f64,
    /// Whether this span lies above a heat strip.
    pub(crate) strip: bool,
}

/// Placement of legend key items across free header spans.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KeyPlan {
    /// Free header spans from left to right.
    pub(crate) spans: Vec<KeySpan>,
    /// Placed key items and their x-coordinates from left to right.
    pub(crate) placed: Vec<(KeyItem, f64)>,
}

impl KeyPlan {
    /// Returns whether `item` was placed.
    #[cfg(test)]
    pub(crate) fn has(&self, item: KeyItem) -> bool {
        self.placed.iter().any(|&(placed, _)| placed == item)
    }
}

/// How well an assignment of the key's items does: more weight, then more
/// items over strips, then further left.
#[derive(Clone, Copy)]
struct KeyScore {
    weight: u32,
    over_strips: u32,
    x_sum: f64,
}

impl KeyScore {
    fn beats(self, other: Self) -> bool {
        (self.weight, self.over_strips)
            .cmp(&(other.weight, other.over_strips))
            .then_with(|| other.x_sum.total_cmp(&self.x_sum))
            .is_gt()
    }
}

/// Assigns the [`KeyItem`]s among `items` to free header `spans` to maximize priority weight.
pub(crate) fn place_key(mut spans: Vec<KeySpan>, items: &[KeyItem], m: &FaceMetrics) -> KeyPlan {
    spans.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let widths = KeyItem::SEARCH.map(|item| item.width(m));
    let narrowest = widths.iter().copied().fold(f64::INFINITY, f64::min);
    let choices: Vec<Option<usize>> = std::iter::once(None)
        .chain(
            (0..spans.len())
                .filter(|&i| spans.get(i).is_some_and(|s| s.x1 - s.x0 >= narrowest))
                .map(Some),
        )
        .collect();
    let score = |combo: [Option<usize>; 3]| {
        let mut used = vec![(0.0, 0_u32); spans.len()];
        let mut score = KeyScore {
            weight: 0,
            over_strips: 0,
            x_sum: 0.0,
        };
        for ((item, width), at) in KeyItem::SEARCH.into_iter().zip(widths).zip(combo) {
            let Some(at) = at else { continue };
            if !items.contains(&item) {
                return None;
            }
            let (span, (taken, n)) = (spans.get(at)?, used.get_mut(at)?);
            *taken += width + if *n > 0 { KEY_GAP } else { 0.0 };
            *n += 1;
            if *taken > span.x1 - span.x0 {
                return None;
            }
            score.weight += item.weight();
            score.over_strips += u32::from(span.strip);
            score.x_sum += span.x0;
        }
        Some(score)
    };
    let mut best: Option<(KeyScore, [Option<usize>; 3])> = None;
    for &over in &choices {
        for &ramp in &choices {
            for &not_read in &choices {
                let combo = [over, ramp, not_read];
                if let Some(s) = score(combo)
                    && best.is_none_or(|(b, _)| s.beats(b))
                {
                    best = Some((s, combo));
                }
            }
        }
    }
    let combo = best.map_or([None; 3], |(_, combo)| combo);
    let mut placed = Vec::new();
    for (at, span) in spans.iter().enumerate() {
        let mut x = span.x0;
        for item in KeyItem::DRAW {
            let k = KeyItem::SEARCH.iter().position(|&i| i == item).unwrap_or(0);
            if combo.get(k).copied().flatten() == Some(at) {
                placed.push((item, x));
                x += widths.get(k).copied().unwrap_or(0.0) + KEY_GAP;
            }
        }
    }
    KeyPlan { spans, placed }
}

/// One tick of the time scale over the strip that carries the time label: the strip's right
/// end is now, and a tick `age_s` back sits `age_s / period_s` pitches left of it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RulerTick {
    /// How long ago the tick is, seconds.
    pub(crate) age_s: u32,
    /// The tick's x, pt.
    pub(crate) x: f64,
    /// Its words' left edge, pt: centred on the tick, or pushed along until clear of the time
    /// label and the now marker, never so far that the tick leaves their middle half
    /// ([`TICK_SLACK`]).
    pub(crate) left: f64,
    /// Its words, `–5 min`, `–30 s`.
    pub(crate) text: String,
}

/// The words of a tick `age_s` back.
pub(crate) fn tick_words(age_s: u32) -> String {
    format!("\u{2013}{}", age_words(age_s))
}

/// How long `age_s` is, in a tick's or the time label's words: `10 min`, `2.5 min`, `80 s`.
fn age_words(age_s: u32) -> String {
    if age_s.is_multiple_of(60) {
        format!("{} min", age_s / 60)
    } else if age_s >= 120 && age_s.is_multiple_of(30) {
        format!("{}.5 min", age_s / 60)
    } else {
        format!("{age_s} s")
    }
}

/// The time label over (or beside) `strip`, long then short: how far back its left end is,
/// `10 min ago` and `–10 min` on a Full strip, `20 s ago` and `–20 s` on the default window's
/// Folded one.
pub(crate) fn time_words(strip: Strip) -> [String; 2] {
    let span = age_words(strip.span_s());
    [format!("{span} ago"), format!("\u{2013}{span}")]
}

/// The ages `strip`'s scale may mark, most wanted first: the middle of its span where that is
/// a round age (a whole minute, or a multiple of 10 s under one), then [`TICK_AGES`], and on a
/// strip under a minute [`TICK_AGES_SHORT`]; only ages inside the span, since its left end is the
/// label's.
pub(crate) fn tick_ages(strip: Strip) -> Vec<u32> {
    let span = strip.span_s();
    let mid = span / 2;
    let round =
        span.is_multiple_of(2) && (mid.is_multiple_of(60) || (mid < 60 && mid.is_multiple_of(10)));
    let short = TICK_AGES_SHORT.into_iter().filter(|_| span < 60);
    round
        .then_some(mid)
        .into_iter()
        .chain(TICK_AGES.into_iter().chain(short).filter(|&age| age != mid))
        .filter(|&age| age > 0 && age < span)
        .collect()
}

/// Where a tick `age_s` back on `strip` goes when its words, `w` wide, must stay inside
/// `lo..hi` with the tick under their middle half, or `None` when they cannot.
fn tick_slot(strip: Strip, age_s: u32, w: f64, (lo, hi): (f64, f64)) -> Option<(f64, f64)> {
    let x = strip.end() - f64::from(age_s) / f64::from(strip.period_s.max(1)) * strip.pitch;
    let centred = x - w / 2.0;
    let slack = w * TICK_SLACK;
    let (least, most) = (lo.max(centred - slack), (hi - w).min(centred + slack));
    (least <= most).then(|| (x, centred.clamp(least, most)))
}

/// Where the time label stands on the header line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeAt {
    /// Over the strip's left end, the scale's ticks after it.
    Over,
    /// Beside the strip, ending [`STRIP_LEAD`] short of its left end (where a tick marks
    /// it), in the free span over the names: the strip's whole header is then the scale's.
    Beside,
}

/// Where a time label can stand, and what it leaves.
struct TimeRoom {
    /// The label's left edge, pt.
    x: f64,
    /// The key's spans with the label's own place taken out of them.
    spans: Vec<KeySpan>,
    /// Where the strip's own free header starts, pt.
    from: f64,
    /// The room it leaves the ticks' words, pt.
    ticks: (f64, f64),
}

/// The time label and its scale as the planner settles them.
struct TimeScale {
    label: String,
    /// The label's left edge, pt.
    x: f64,
    ticks: Vec<RulerTick>,
    /// The tick at the strip's left end when the label stands beside it.
    end_tick: Option<f64>,
    /// The key's spans with what the label and the ticks leave of them.
    spans: Vec<KeySpan>,
}

/// Complete layout plan for the header line and its legend key.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HeaderPlan {
    pub(crate) labels: Vec<HeaderLabel>,
    pub(crate) key: KeyPlan,
    /// The time scale over the strip that carries the time label, left to right.
    pub(crate) ruler: Vec<RulerTick>,
    /// The tick at that strip's left end, pt, when the time label stands beside the strip
    /// rather than over it.
    pub(crate) time_tick: Option<f64>,
    /// The key's items this header may place (rule K searches only these).
    pub(crate) keys: &'static [KeyItem],
}

struct Header<'m> {
    m: &'m FaceMetrics,
    labels: Vec<HeaderLabel>,
    spans: Vec<KeySpan>,
    /// The strip whose header carries the time label and the scale.
    time: Option<Strip>,
    /// The key's items this header may place.
    keys: &'static [KeyItem],
}

impl<'m> Header<'m> {
    const fn new(m: &'m FaceMetrics) -> Self {
        Self {
            m,
            labels: Vec::new(),
            spans: Vec::new(),
            time: None,
            keys: &KeyItem::SEARCH,
        }
    }

    fn words(&self, s: &str) -> f64 {
        self.m.width(s, HEADER_PT, false)
    }

    fn label(&mut self, label: Label, x: f64, text: impl Into<String>) {
        self.labels.push(HeaderLabel {
            label,
            x,
            text: text.into(),
        });
    }

    fn span(&mut self, x0: f64, x1: f64, strip: bool) {
        self.spans.push(KeySpan { x0, x1, strip });
    }

    fn time(&mut self, strip: Strip) {
        self.time = Some(strip);
    }

    fn scope(&mut self, bar: Bar, x: f64, words: String) -> f64 {
        let w = SWATCH_ADVANCE + self.words(&words);
        self.label(Label::Scope(bar), x, words);
        w
    }

    fn scope_keys(&mut self, mut x: f64, limit: f64, global: &str, tab: &str) -> f64 {
        for (bar, words) in [
            (Bar::Global, global),
            (Bar::Aterm, "aterm"),
            (Bar::Tab, tab),
        ] {
            let room = limit - x - SWATCH_ADVANCE;
            let words = if bar == Bar::Tab {
                self.m.fit(&[words], room, HEADER_PT, false)
            } else if self.words(words) <= room {
                words.to_owned()
            } else {
                break;
            };
            if words.is_empty() {
                break;
            }
            x += self.scope(bar, x, words) + 8.0;
        }
        x
    }

    /// `base` (the key's spans) with the time strip's own: what lies from `from` to the strip's
    /// end that `ticks` leave, short of the now marker.
    fn spans_with(
        &self,
        base: &[KeySpan],
        strip: Strip,
        from: f64,
        ticks: &[RulerTick],
    ) -> Vec<KeySpan> {
        let mut spans = base.to_vec();
        let mut x0 = from;
        let mut by_x: Vec<&RulerTick> = ticks.iter().collect();
        by_x.sort_by(|a, b| a.left.total_cmp(&b.left));
        for tick in by_x {
            spans.push(KeySpan {
                x0,
                x1: tick.left - KEY_GAP,
                strip: true,
            });
            x0 = tick.left + self.words(&tick.text) + KEY_GAP;
        }
        spans.push(KeySpan {
            x0,
            x1: strip.end() - MARKER_ROOM,
            strip: true,
        });
        spans.retain(|s| s.x1 > s.x0);
        spans
    }

    /// The ticks whose words fit `room` on `strip`, each alone, most wanted first.
    fn tick_room(&self, strip: Strip, room: (f64, f64)) -> Vec<RulerTick> {
        tick_ages(strip)
            .into_iter()
            .filter_map(|age_s| {
                let text = tick_words(age_s);
                let (x, left) = tick_slot(strip, age_s, self.words(&text), room)?;
                Some(RulerTick {
                    age_s,
                    x,
                    left,
                    text,
                })
            })
            .collect()
    }

    /// Where a time label `w` wide can stand `at` `strip`, or `None` when no free span over
    /// the names ends at the strip's lead to hold it beside the strip.
    fn time_room(&self, strip: Strip, w: f64, at: TimeAt) -> Option<TimeRoom> {
        let hi = strip.end() - MARKER_ROOM;
        match at {
            TimeAt::Over => Some(TimeRoom {
                x: strip.x,
                spans: self.spans.clone(),
                from: strip.x + w + KEY_GAP,
                ticks: (strip.x + w + TICK_GAP, hi),
            }),
            TimeAt::Beside => {
                let end = strip.x - STRIP_LEAD;
                let x = end - w;
                let at = self
                    .spans
                    .iter()
                    .position(|s| !s.strip && (s.x1 - end).abs() < 1e-9 && s.x0 <= x)?;
                let mut spans = self.spans.clone();
                if let Some(span) = spans.get_mut(at) {
                    span.x1 = x - KEY_GAP;
                }
                spans.retain(|s| s.x1 > s.x0);
                Some(TimeRoom {
                    x,
                    spans,
                    from: end + KEY_GAP,
                    ticks: (end + TICK_GAP, hi),
                })
            }
        }
    }

    /// What `spans` let the key keep: the items rule K places, and whether the heat ramp stands
    /// over a strip, the colours it explains.
    fn key_keeps(&self, spans: Vec<KeySpan>) -> (Vec<KeyItem>, bool) {
        let plan = place_key(spans, self.keys, self.m);
        let items = plan.placed.iter().map(|&(item, _)| item).collect();
        let ramp_over_strip = plan.placed.iter().any(|&(item, x)| {
            item == KeyItem::Ramp
                && plan
                    .spans
                    .iter()
                    .any(|s| s.strip && s.x0 <= x + 1e-9 && x < s.x1)
        });
        (items, ramp_over_strip)
    }

    /// The time label and its scale. The label stands over the strip's left end in its long
    /// form, else its short one, when that leaves a tick room; else beside the strip, long then
    /// short; else long and alone over the strip. The scale is weighed against the key the long
    /// label alone leaves (v2's own header, rule K): it takes ticks in order of want while the
    /// key keeps every item it places and the heat ramp over a strip; failing that, every item;
    /// failing that, every item but its least, "not read", since in rule K's order the scale
    /// ranks after the ramp and before "not read". The test is the items themselves, not their
    /// summed weights, which can hold while one item is traded for another. It never costs
    /// "over max" or the ramp a place; where every placement would, the label stands alone.
    fn ruler(&self, strip: Strip) -> TimeScale {
        let [long, short] = time_words(strip);
        let alone = strip.x + self.words(&long) + KEY_GAP;
        let (items, ramp) = self.key_keeps(self.spans_with(&self.spans, strip, alone, &[]));
        let but_not_read: Vec<KeyItem> = items
            .iter()
            .copied()
            .filter(|&item| item != KeyItem::NotRead)
            .collect();
        let placements = [
            (&long, TimeAt::Over),
            (&short, TimeAt::Over),
            (&long, TimeAt::Beside),
            (&short, TimeAt::Beside),
        ];
        let costs = [
            (items.as_slice(), ramp),
            (items.as_slice(), false),
            (but_not_read.as_slice(), false),
        ];
        let clear = |chosen: &[RulerTick], t: &RulerTick| {
            let w = self.words(&t.text);
            chosen.iter().all(|c| {
                let cw = self.words(&c.text);
                t.left + w + TICK_GAP <= c.left || c.left + cw + TICK_GAP <= t.left
            })
        };
        for (must, ramp) in costs {
            for (label, at) in placements {
                let Some(TimeRoom {
                    x,
                    spans: base,
                    from,
                    ticks: room,
                }) = self.time_room(strip, self.words(label), at)
                else {
                    continue;
                };
                let mut chosen: Vec<RulerTick> = Vec::new();
                for t in self.tick_room(strip, room) {
                    let mut trial = chosen.clone();
                    trial.push(t.clone());
                    let (kept, r) = self.key_keeps(self.spans_with(&base, strip, from, &trial));
                    if clear(&chosen, &t) && must.iter().all(|i| kept.contains(i)) && (r || !ramp) {
                        chosen = trial;
                    }
                }
                if chosen.is_empty() {
                    continue;
                }
                chosen.sort_by(|a, b| a.left.total_cmp(&b.left));
                return TimeScale {
                    label: label.clone(),
                    x,
                    spans: self.spans_with(&base, strip, from, &chosen),
                    ticks: chosen,
                    end_tick: (at == TimeAt::Beside).then_some(strip.x),
                };
            }
        }
        TimeScale {
            label: long,
            x: strip.x,
            spans: self.spans_with(&self.spans, strip, alone, &[]),
            ticks: Vec::new(),
            end_tick: None,
        }
    }

    fn finish(mut self) -> HeaderPlan {
        let (mut ruler, mut time_tick) = (Vec::new(), None);
        if let Some(strip) = self.time {
            let scale = self.ruler(strip);
            self.spans = scale.spans;
            self.label(Label::Time, scale.x, scale.label);
            ruler = scale.ticks;
            time_tick = scale.end_tick;
        }
        HeaderPlan {
            key: place_key(self.spans, self.keys, self.m),
            labels: self.labels,
            ruler,
            time_tick,
            keys: self.keys,
        }
    }
}

/// Lays out the header line and key for a `Full` deck plan.
pub(crate) fn header_full(plan: &FullPlan, text: HeaderText<'_>, m: &FaceMetrics) -> HeaderPlan {
    if plan.ground_only {
        return HeaderPlan::default();
    }
    let mut head = Header::new(m);
    head.time(plan.strip);
    // Wide and mid say the glance's header: "now" beside the live edge's dot (the owner's), the
    // scopes' names alone, no "Details" (its clauses say what they are), and a key for the over
    // mark only while a strip draws one.
    let long = matches!(plan.kind, FullKind::Wide | FullKind::Mid);
    if long {
        head.keys = if text.over { &[KeyItem::Over] } else { &[] };
    }
    if let Some(pct) = plan.pct {
        head.label(Label::Now, pct.end(), "now");
    }
    if let CoresAt::Row(_) = plan.cores {
        head.span(plan.name.x, plan.strip.x - STRIP_LEAD, false);
    }
    if plan.kind == FullKind::Slim {
        if let (Some(bar), Some(amount)) = (plan.bar, plan.amount) {
            let end = head.scope_keys(bar.x, plan.w - X0, "Global", text.tab);
            head.span(end + 2.0, amount.end(), false);
        } else if let Some(bar) = plan.bar {
            head.scope_keys(bar.x, plan.w - X0, "Global", text.tab);
        }
        return head.finish();
    }
    if let Some(amount) = plan.amount {
        let x = plan.bar.map_or(amount.x, |bar| bar.x);
        let global = if long {
            text.machine.to_owned()
        } else {
            "Global".to_owned()
        };
        let w = head.scope(Bar::Global, x, global);
        head.span(x + w + KEY_GAP, amount.end(), false);
    }
    if let (Some(aterm), Some(tab)) = (plan.aterm, plan.tab) {
        let aterm_forms: &[&str] = &["aterm"];
        let tab_forms: &[&str] = &[text.tab];
        let tab_gap = plan.details.map_or(X0, |details| details.x - tab.end());
        for (col, bar, forms, gap) in [
            (aterm, Bar::Aterm, aterm_forms, tab.x - aterm.end()),
            (tab, Bar::Tab, tab_forms, tab_gap),
        ] {
            let room = col.w + gap - 8.0;
            let words = forms
                .iter()
                .find(|form| SWATCH_ADVANCE + head.words(form) <= room)
                .map_or_else(
                    || {
                        let last = forms.last().copied().unwrap_or_default();
                        m.fit(&[last], room - SWATCH_ADVANCE, HEADER_PT, false)
                    },
                    |form| (*form).to_owned(),
                );
            let w = head.scope(bar, col.x, words);
            head.span(col.x + w + KEY_GAP, col.end(), false);
        }
    }
    if let Some(details) = plan.details {
        if long {
            head.span(details.x, details.end(), false);
        } else {
            let words = if head.words(DETAILS_LONG) <= details.w {
                DETAILS_LONG
            } else {
                "Details"
            };
            head.label(Label::Details, details.x, words);
            head.span(
                details.x + head.words(words) + KEY_GAP,
                details.end(),
                false,
            );
        }
    }
    head.finish()
}

/// Lays out the header line and key for a `Folded` deck plan. Folded says the glance (§6.8) at
/// every tier, so its header does as Full wide and mid's: "now" over each %, "this Mac" for
/// Global, no "Details", and a key for the over mark only while a strip draws one.
pub(crate) fn header_folded(
    plan: &FoldedPlan,
    text: HeaderText<'_>,
    states: [Option<StateLabel<'_>>; 2],
    m: &FaceMetrics,
) -> HeaderPlan {
    if plan.ground_only {
        return HeaderPlan::default();
    }
    let mut head = Header::new(m);
    head.keys = if text.over { &[KeyItem::Over] } else { &[] };
    let mut free_strips: [Option<Strip>; 2] = [None; 2];
    let mut a_spilled = false;
    for (i, (block, state)) in plan.blocks.iter().zip(states).enumerate() {
        if let Some(pct) = block.pct {
            head.label(Label::Now, pct.end(), "now");
        }
        let name_room = block.strip.x - STRIP_LEAD - block.name.x;
        let mut strip_free = true;
        match state.filter(|_| block.amount.is_none()) {
            None => head.span(block.name.x, block.strip.x - STRIP_LEAD, false),
            Some(state) => {
                let forms = [state.full, state.bare];
                let words = if MARK_ADVANCE + m.width(state.full, HEADER_PT, true) <= name_room {
                    state.full.to_owned()
                } else if !a_spilled {
                    strip_free = false;
                    a_spilled = i == 0;
                    m.fit(
                        &forms,
                        block.strip.end() - MARKER_ROOM - block.name.x - MARK_ADVANCE,
                        HEADER_PT,
                        true,
                    )
                } else {
                    m.fit(&forms, name_room - MARK_ADVANCE, HEADER_PT, true)
                };
                head.label(Label::State, block.name.x, words);
            }
        }
        if strip_free && let Some(free) = free_strips.get_mut(i) {
            *free = Some(block.strip);
        }
        if let (Some(amount), Some(aterm), Some(tab)) = (block.amount, block.aterm, block.tab) {
            for (col, bar, words) in [
                (amount, Bar::Global, text.machine),
                (aterm, Bar::Aterm, "aterm"),
                (tab, Bar::Tab, text.tab),
            ] {
                let words = m.fit(&[words], col.w - SWATCH_ADVANCE, HEADER_PT, false);
                let w = head.scope(bar, col.x, words);
                head.span(col.x + w + KEY_GAP, col.end(), false);
            }
        } else if let (Some(bar), Some(amount)) = (block.bar, block.amount) {
            // The bar's key once, over block A: block B's bar is the same three scopes on the
            // same line, and a legend learned once need not be said twice.
            if i == 0 {
                let end = head.scope_keys(bar.x, block.at.end(), text.machine, text.tab);
                head.span(end + 2.0, amount.end(), false);
            } else {
                head.span(bar.x, amount.end(), false);
            }
        } else if let Some(amount) = block.amount {
            head.span(amount.x, amount.end(), false);
        }
        if let Some(details) = block.details {
            head.span(details.x, details.end(), false);
        }
    }
    match free_strips {
        [Some(a), b] => {
            head.time(a);
            if let Some(b) = b {
                head.span(b.x, b.end() - MARKER_ROOM, true);
            }
        }
        [None, Some(b)] => head.time(b),
        [None, None] => {}
    }
    head.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: HeaderText<'static> = HeaderText {
        tab: "Tab 2",
        machine: "this Mac",
        over: true,
    };

    fn hn() -> FaceMetrics {
        FaceMetrics::helvetica_neue()
    }

    fn px(v: f64, s: f64) -> f64 {
        (v * s + 0.5).floor()
    }

    fn strip_col(strip: Strip) -> Col {
        Col::new(strip.x, strip.end() - strip.x)
    }

    fn full_cols(plan: &FullPlan) -> Vec<Col> {
        let mut cols = Vec::new();
        if let CoresAt::Block { at, divider } = plan.cores {
            cols.extend([at, Col::new(divider, 0.0)]);
        }
        cols.extend([plan.name, plan.well, strip_col(plan.strip)]);
        cols.extend(
            [
                plan.pct,
                plan.bar,
                plan.amount,
                plan.aterm,
                plan.tab,
                plan.details,
            ]
            .into_iter()
            .flatten(),
        );
        cols
    }

    fn block_cols(block: &BlockPlan) -> Vec<Col> {
        let mut cols = vec![block.name, block.well, strip_col(block.strip)];
        cols.extend(
            [
                block.pct,
                block.bar,
                block.amount,
                block.aterm,
                block.tab,
                block.details,
            ]
            .into_iter()
            .flatten(),
        );
        cols
    }

    fn assert_in_order(cols: &[Col], x0: f64, x1: f64, what: &str) {
        let mut edge = x0;
        for col in cols {
            assert!(
                col.w >= 0.0 && col.x >= edge,
                "{what}: {col:?} starts before {edge}"
            );
            edge = col.end();
        }
        assert!(edge <= x1, "{what}: the columns end at {edge}, past {x1}");
    }

    fn assert_on_pixels(cols: &[Col], what: &str) {
        for s in [1.0, 2.0] {
            for edge in cols.iter().flat_map(|col| [col.x, col.end()]) {
                assert_eq!(edge * s, px(edge, s), "{what}: edge {edge} at {s}×");
            }
        }
    }

    #[test]
    fn plan_golden_tables_full() {
        let m = hn();
        let at = |w: f64| {
            let plan = plan_full(w, &m);
            assert_eq!(plan_full((2.0 * w + 1.0) / 2.0, &m), plan);
            assert_on_pixels(&full_cols(&plan), &format!("W {w}"));
            plan
        };

        let p = at(1119.0);
        assert_eq!((p.kind, p.offset, p.gap_grow), (FullKind::Wide, 0.0, 0.0));
        assert_eq!(p.name, Col::new(97.0, 56.0));
        assert_eq!(
            (p.strip.x, p.strip.cols, p.strip.pitch, p.strip.cell),
            (167.0, 60, 5.0, 4.0)
        );
        assert_eq!(p.strip.end(), 466.0);
        assert_eq!(p.pct.map(Col::end), Some(503.0));
        assert_eq!(p.bar, Some(Col::new(511.0, 96.0)));
        assert_eq!(p.amount, Some(Col::new(615.0, 96.0)));
        assert_eq!(p.aterm, Some(Col::new(719.0, 56.0)));
        assert_eq!(p.tab, Some(Col::new(783.0, 56.0)));
        assert_eq!(p.details, Some(Col::new(849.0, 258.0)));
        assert_eq!(
            p.cores,
            CoresAt::Block {
                at: Col::new(12.0, 68.0),
                divider: 88.0
            }
        );
        assert_eq!([2.0, 1.0].map(|s| px(p.strip.x, s)), [334.0, 167.0]);
        assert_eq!([2.0, 1.0].map(|s| px(p.strip.cell, s)), [8.0, 4.0]);

        let p = at(1424.0);
        assert_eq!(
            (p.kind, p.strip.pitch, p.strip.end()),
            (FullKind::Wide, 6.0, 526.0)
        );
        assert_eq!(p.pct.map(Col::end), Some(563.0));
        assert_eq!(p.amount.map(|a| a.x), Some(675.0));
        assert_eq!(p.details, Some(Col::new(909.0, 503.0)));

        let p = at(900.0);
        assert_eq!(p.kind, FullKind::Mid);
        assert_eq!(
            (p.strip.x, p.strip.cols, p.strip.pitch, p.strip.cell),
            (167.0, 60, 4.0, 3.0)
        );
        assert_eq!(p.strip.end(), 406.0);
        assert_eq!(p.pct.map(Col::end), Some(443.0));
        assert_eq!(p.bar, Some(Col::new(451.0, 80.0)));
        assert_eq!(p.amount.map(|a| a.x), Some(539.0));
        assert_eq!(p.aterm.map(|a| a.x), Some(643.0));
        assert_eq!(p.tab.map(|t| t.x), Some(707.0));
        assert_eq!(p.details, None);

        let p = at(550.0);
        assert_eq!(p.kind, FullKind::Narrow);
        assert_eq!((p.name, p.well.x), (Col::new(12.0, 52.0), 64.0));
        assert_eq!((p.strip.x, p.strip.cols, p.strip.pitch), (78.0, 20, 5.0));
        assert_eq!(p.pct, Some(Col::new(184.0, 30.0)));
        assert_eq!((p.bar, p.details), (None, None));
        assert_eq!(p.amount, Some(Col::new(222.0, 148.0)));
        assert_eq!(p.aterm, Some(Col::new(378.0, 76.0)));
        assert_eq!(p.tab, Some(Col::new(462.0, 76.0)));
        assert_eq!(p.cores, CoresAt::Row(Col::new(78.0, 460.0)));

        let p = at(350.0);
        assert_eq!(p.kind, FullKind::Slim);
        assert_eq!((p.name, p.well.x), (Col::new(12.0, 48.0), 60.0));
        assert_eq!((p.strip.x, p.strip.cols, p.strip.pitch), (73.0, 20, 4.0));
        assert_eq!(p.pct, Some(Col::new(157.0, 26.0)));
        assert_eq!(p.bar, Some(Col::new(189.0, 24.0)));
        assert_eq!(p.amount, Some(Col::new(219.0, 119.0)));
        assert_eq!((p.aterm, p.tab, p.details), (None, None, None));

        let p = at(2560.0);
        assert_eq!((p.kind, p.strip.pitch), (FullKind::Wide, 9.0));
        assert_eq!((p.gap_grow, p.offset), (16.0, 401.0));
        let CoresAt::Block { at: cores, divider } = p.cores else {
            panic!("wide has the cores block");
        };
        assert_eq!((cores.x, divider, p.name.x), (413.0, 489.0, 498.0));
        let details = p.details.expect("wide has Details");
        assert_eq!(details.w, 560.0);
        assert!((cores.x - (p.w - details.end())).abs() <= 1.0);
        assert_eq!([1700.0, 2000.0].map(|w| at(w).offset), [31.0, 151.0]);
    }

    #[test]
    fn plan_golden_tables_folded() {
        let m = hn();
        let at = |w: f64| {
            let plan = plan_folded(w, &m);
            assert_eq!(plan_folded((2.0 * w + 1.0) / 2.0, &m), plan);
            for block in &plan.blocks {
                assert_on_pixels(&block_cols(block), &format!("W {w}"));
            }
            plan
        };
        let kinds = |plan: &FoldedPlan| plan.blocks.map(|b| b.kind);
        let details = |plan: &FoldedPlan| plan.blocks.map(|b| b.details);
        let gauge = |at: f64| FoldedGauge {
            at: Col::new(at, 54.0),
            divider: at + 63.0,
        };

        // The gauge at the top left, 54 pt across, its divider 9 pt past it and block A 9 pt
        // past that; the blocks share what is left.
        let p = at(1119.0);
        assert_eq!(p.gauge, Some(gauge(12.0)));
        assert_eq!(p.gauge.map(|g| g.dial()), Some([12.0, 4.0, 66.0, 58.0]));
        assert_eq!(kinds(&p), [BlockKind::Wide; 2]);
        assert_eq!(
            p.blocks.map(|b| b.at),
            [Col::new(84.0, 497.0), Col::new(610.0, 497.0)]
        );
        assert_eq!(p.divider, 595.0);
        assert_eq!(details(&p), [None, None]);
        let a = p.blocks[0];
        // Its strip keeps v2's 30 cells on 5 pt, each a reading's second: half a minute, sliding
        // a cell left at each reading (§12 of the live edge's design).
        assert_eq!(
            (a.strip.x, a.strip.cols, a.strip.period_s, a.strip.pitch),
            (150.0, 30, 1, 5.0)
        );
        assert_eq!((a.strip.end(), a.strip.span_s()), (299.0, 30));
        assert_eq!(a.pct, Some(Col::new(306.0, 30.0)));
        // The glance's columns, as Full wide's (§3.2).
        assert_eq!(a.amount, Some(Col::new(344.0, 96.0)));
        assert_eq!(
            (a.aterm, a.tab),
            (Some(Col::new(446.0, 56.0)), Some(Col::new(508.0, 56.0)))
        );

        let p = at(1424.0);
        assert_eq!(kinds(&p), [BlockKind::Wide; 2]);
        assert_eq!(
            details(&p),
            [Some(Col::new(574.0, 159.0)), Some(Col::new(1252.0, 159.0))]
        );

        // The owner's window: mid beside the gauge.
        let p = at(1039.0);
        assert_eq!(
            (p.gauge, kinds(&p)),
            (Some(gauge(12.0)), [BlockKind::Mid; 2])
        );
        assert_eq!(p.blocks[0].bar, Some(Col::new(269.0, 28.0)));
        assert_eq!(p.blocks[0].amount, Some(Col::new(303.0, 160.0)));

        let p = at(750.0);
        assert_eq!(kinds(&p), [BlockKind::Narrow; 2]);
        assert_eq!(p.blocks[0].bar, Some(Col::new(237.0, 24.0)));
        assert_eq!(p.blocks[0].amount, Some(Col::new(266.0, 130.0)));

        for w in [550.0, 450.0] {
            let p = at(w);
            assert_eq!(
                (p.gauge, kinds(&p)),
                (Some(gauge(12.0)), [BlockKind::Slim; 2])
            );
            assert_eq!(p.blocks[0].pct, Some(Col::new(204.0, 26.0)));
            assert_eq!(p.blocks[0].amount, None);
        }

        // Under 417 pt not even slim fits beside the gauge: the deck draws none, and the blocks
        // are 0.134's.
        let p = at(350.0);
        assert_eq!((p.gauge, kinds(&p)), (None, [BlockKind::Slim; 2]));
        assert_eq!(p.blocks[0].pct, Some(Col::new(132.0, 26.0)));
        assert_eq!(p.blocks[0].amount, None);

        // Past the cap the table is centred, the gauge leading it as Full's slot leads its table.
        let p = at(2560.0);
        assert_eq!(p.gauge, Some(gauge(179.0)));
        assert_eq!(
            p.blocks.map(|b| b.at),
            [Col::new(251.0, 1050.0), Col::new(1330.0, 1050.0)]
        );
        assert_eq!(details(&p).map(|d| d.map(|d| d.w)), [Some(560.0); 2]);

        let p = at(320.0);
        assert_eq!(
            (p.gauge, kinds(&p), p.blocks.map(|b| b.pct)),
            (None, [BlockKind::Slim; 2], [None; 2])
        );
        assert!(!p.ground_only);
    }

    #[test]
    fn rows_sit_where_the_vertical_tables_say() {
        let m = hn();
        let tops = |rows: &Rows, s: f64| {
            (0..rows.count)
                .map(|r| px(rows.top(r), s))
                .collect::<Vec<_>>()
        };

        let wide = plan_full(1119.0, &m).rows;
        assert_eq!(
            (wide.header_baseline, wide.count, wide.cell, wide.baseline),
            (11.5, 7, 9.0, 8.0)
        );
        assert_eq!(
            tops(&wide, 2.0),
            [32.0, 56.0, 80.0, 104.0, 128.0, 152.0, 176.0]
        );
        assert_eq!([2.0, 1.0].map(|s| px(wide.cell, s)), [18.0, 9.0]);
        assert_eq!(
            (wide.name_pt, wide.amount_pt, wide.figure_pt, wide.bar_h),
            (10.5, 10.0, 10.0, 6.0)
        );

        let narrow = plan_full(550.0, &m).rows;
        assert_eq!(
            (narrow.count, narrow.pitch, narrow.cell, narrow.baseline),
            (8, 10.5, 8.0, 7.0)
        );
        assert_eq!(
            (narrow.name_pt, narrow.amount_pt, narrow.figure_pt),
            (10.0, 10.0, 10.0)
        );
        let slim = plan_full(350.0, &m).rows;
        assert_eq!((slim.name_pt, slim.amount_pt, slim.bar_h), (9.5, 9.5, 5.0));

        let folded = plan_folded(1119.0, &m).blocks[0].rows;
        assert_eq!(
            (folded.header_baseline, folded.count, folded.baseline),
            (9.5, 4, 7.5)
        );
        assert_eq!(tops(&folded, 2.0), [28.0, 51.0, 74.0, 97.0]);
        assert_eq!(tops(&folded, 1.0), [14.0, 26.0, 37.0, 49.0]);
        assert_eq!([2.0, 1.0].map(|s| px(folded.cell, s)), [17.0, 9.0]);
        assert_eq!(
            (
                folded.name_pt,
                folded.amount_pt,
                folded.figure_pt,
                folded.bar_h
            ),
            (10.0, 9.5, 9.5, 5.0)
        );
        assert_eq!(plan_folded(350.0, &m).blocks[0].rows.name_pt, 9.5);
    }

    #[test]
    fn breakpoints_follow_the_face() {
        let helvetica = breakpoints(&hn());
        assert_eq!(helvetica.full, [981.0, 775.0, 522.0, 323.0]);
        assert_eq!(helvetica.block, [480.0, 319.0, 245.0, 146.0]);
        assert_eq!(helvetica.folded, [1085.0, 763.0, 615.0, 417.0]);
        assert_eq!(helvetica.folded_bare, 345.0);

        let stack = FaceMetrics::test_stack();
        assert_eq!(plan_full(1119.0, &stack).name.w, 59.0);
        assert_eq!(plan_full(550.0, &stack).name.w, 56.0);
        let dejavu = breakpoints(&stack);
        assert_eq!(dejavu.full, [984.0, 778.0, 526.0, 328.0]);
        assert_eq!(dejavu.block, [484.0, 323.0, 250.0, 151.0]);
        assert_eq!(dejavu.folded, [1093.0, 771.0, 625.0, 427.0]);
        assert_eq!(dejavu.folded_bare, 355.0);

        let drawn = |w: f64| {
            (
                !plan_full(w, &hn()).ground_only,
                !plan_folded(w, &hn()).ground_only,
            )
        };
        assert_eq!(
            [163.0, 164.0, 284.0, 285.0].map(drawn),
            [(false, false), (true, false), (true, false), (true, true)]
        );

        for (m, bp) in [(&hn(), helvetica), (&stack, dejavu)] {
            for (kind, w) in FullKind::ALL.into_iter().zip(bp.full) {
                assert_eq!(plan_full(w, m).kind, kind);
            }
            for (kind, w) in FullKind::ALL.into_iter().zip(bp.full).take(3) {
                assert_ne!(plan_full(w - 1.0, m).kind, kind);
            }
            for (kind, w) in BlockKind::ALL.into_iter().zip(bp.folded) {
                let at = plan_folded(w, m);
                assert_eq!(at.blocks.map(|b| b.kind), [kind; 2]);
                assert!(at.gauge.is_some(), "{kind:?} at {w} has the gauge");
                let below = plan_folded(w - 1.0, m);
                if kind == BlockKind::Slim {
                    assert_eq!((below.gauge, below.blocks[0].kind), (None, kind));
                    assert!(below.blocks[0].pct.is_some());
                } else {
                    assert_ne!(below.blocks[0].kind, kind);
                }
            }
            let bare = plan_folded(bp.folded_bare, m);
            assert_eq!((bare.gauge, bare.blocks[0].kind), (None, BlockKind::Slim));
            assert!(bare.blocks[0].pct.is_some());
            let below = plan_folded(bp.folded_bare - 1.0, m).blocks[0];
            assert_eq!((below.kind, below.pct), (BlockKind::Slim, None));
        }
    }

    #[test]
    fn every_width_plans_inside_the_card() {
        for m in [hn(), FaceMetrics::test_stack()] {
            let slim_beside = breakpoints(&m).folded[3];
            for w in (160..=5120).map(f64::from) {
                let full = plan_full(w, &m);
                let period = match full.kind {
                    FullKind::Wide | FullKind::Mid => 10,
                    FullKind::Narrow if w >= 640.0 => 20,
                    FullKind::Narrow | FullKind::Slim => 30,
                };
                assert_eq!(
                    (full.strip.period_s, full.strip.cols * period),
                    (period, 600),
                    "W {w}"
                );
                assert!(full.strip.cols * full.rows.count.min(7) <= 420);
                if !full.ground_only {
                    assert_in_order(&full_cols(&full), X0, w - X0, &format!("Full W {w}"));
                }
                if full.offset > 0.0
                    && let (CoresAt::Block { at, .. }, Some(details)) = (full.cores, full.details)
                {
                    assert!(
                        (at.x - (w - details.end())).abs() <= 1.0,
                        "W {w} is not centred"
                    );
                }

                let folded = plan_folded(w, &m);
                let [a, b] = folded.blocks;
                assert!(a.at.end() < folded.divider && folded.divider < b.at.x);
                if let Some(g) = folded.gauge {
                    let [x0, y0, x1, y1] = g.dial();
                    assert!(
                        x0 >= X0 && x1 < g.divider && g.divider < a.at.x && a.at.x == a.name.x,
                        "Folded W {w}: the gauge left of block A: {g:?} {:?}",
                        a.at
                    );
                    assert!(y0 >= 4.0 && y1 <= 58.0, "Folded W {w}: {g:?} in the band");
                }
                assert_eq!(folded.gauge.is_some(), w >= slim_beside, "Folded W {w}");
                for block in &folded.blocks {
                    // v2's cells over the strip's 60, 80 or 150 pt, each a reading's second: it
                    // spans 20 or 30 s and slides 3 to 5 pt at each reading.
                    let (width, span, pitch) = match block.kind {
                        BlockKind::Wide => (150, 30, 5.0),
                        BlockKind::Mid => (80, 20, 4.0),
                        BlockKind::Narrow | BlockKind::Slim => (60, 20, 3.0),
                    };
                    assert_eq!(
                        (
                            block.strip.period_s,
                            block.strip.span_s(),
                            block.strip.pitch
                        ),
                        (FOLD_SLOT_S, span, pitch),
                        "Folded W {w}"
                    );
                    assert!(block.strip.pt_per_s() >= 3.0, "Folded W {w}");
                    assert!(
                        (block.strip.end() + 1.0 - block.strip.x - f64::from(width)).abs() < 1e-9,
                        "Folded W {w}: the strip keeps its place"
                    );
                    if !folded.ground_only {
                        assert_in_order(
                            &block_cols(block),
                            block.at.x,
                            block.at.end(),
                            &format!("Folded W {w}"),
                        );
                    }
                }
                if !folded.ground_only {
                    assert!(a.at.x >= X0 && b.at.end() <= w - X0, "Folded W {w}");
                }
            }
        }
    }

    #[test]
    fn deck_height_is_a_function_of_the_window() {
        use crate::stats_deck::{StatsBars, deck_px};
        use DeckForm::{Folded, Full};
        let _: fn(DeckSizePref) -> DeckForm = deck_form_for;
        let _: fn(StatsBars, DeckForm, f64, usize) -> usize = deck_px;

        // Folded unless chosen Full: the form reads nothing of the window.
        assert_eq!(deck_form_for(DeckSizePref::Auto), Folded);
        assert_eq!(deck_form_for(DeckSizePref::Full), Full);
        assert_eq!(deck_form_for(DeckSizePref::Folded), Folded);

        assert_eq!(full_delta_px(2.0, 28), 84);
        assert_eq!(deck_px(StatsBars::Deck, Full, 2.0, 28), 208);
        for s in [1.0, 2.0] {
            let folded = (FOLDED_PT * s).round() as usize;
            let table = (FULL_TABLE_PT * s).round() as usize;
            for cell in 10..=40 {
                let at = format!("{cell} px cells at {s}×");
                assert_eq!(deck_px(StatsBars::Deck, Folded, s, cell), folded, "{at}");
                let full = deck_px(StatsBars::Deck, Full, s, cell);
                assert_eq!((full - folded) % cell, 0, "{at}");
                assert_eq!(full - folded, full_delta_px(s, cell), "{at}");
                assert!((table..table + cell).contains(&full), "{at}");
                for bars in [StatsBars::Sill, StatsBars::Off] {
                    for deck in [Folded, Full] {
                        assert_eq!(deck_px(bars, deck, s, cell), 0, "{bars:?} {at}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_deck_size_setting_reads_its_own_words() {
        for pref in DeckSizePref::ALL {
            assert_eq!(DeckSizePref::parse(Some(pref.word())), pref);
            assert_eq!(DeckSizePref::from_word(pref.word()), Some(pref));
            let loud = format!(" {} ", pref.word().to_uppercase());
            assert_eq!(DeckSizePref::parse(Some(&loud)), pref);
        }
        assert_eq!(DeckSizePref::parse(None), DeckSizePref::Auto);
        assert_eq!(DeckSizePref::parse(Some("huge")), DeckSizePref::Auto);
        assert_eq!(DeckSizePref::from_word("huge"), None);
    }

    fn machines() -> Vec<Vec<Cluster<'static>>> {
        const NAMES: [&[&str]; 4] = [
            &["Performance"],
            &["Performance", "Efficiency"],
            &["Super", "Performance", "Efficiency"],
            &["Super", "Performance", "Mid 2", "Efficiency"],
        ];
        let mut machines = Vec::new();
        for n in 4..=128_usize {
            machines.push(vec![Cluster {
                count: n,
                name: None,
            }]);
            for names in NAMES {
                let k = names.len();
                machines.push(
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, &name)| Cluster {
                            count: n / k + usize::from(i < n % k),
                            name: Some(name),
                        })
                        .collect(),
                );
            }
        }
        machines
    }

    fn assert_row_fits(
        clusters: &[Cluster<'_>],
        mode: CoresMode,
        ladder: &[CoresMode],
        room: f64,
        cell: f64,
        caption_pt: f64,
        m: &FaceMetrics,
    ) {
        let total = count_words(clusters);
        if mode == CoresMode::CountOnly {
            let words = cores_count_text(clusters, room, caption_pt, m);
            assert!(
                words == total
                    || (words.starts_with(&format!("{total}: ")) && !words.ends_with('…')),
                "{clusters:?} in {room} pt says {words:?}"
            );
            assert!(m.width(&words, caption_pt, false) <= room);
        } else {
            assert!(ladder.contains(&mode), "{mode:?} is not this row's");
            assert!(
                cores_row_width(clusters, mode, cell, caption_pt, m) <= room,
                "{clusters:?} in {room} pt"
            );
        }
    }

    #[test]
    fn cores_fit_every_machine() {
        let machines = machines();
        for m in [hn(), FaceMetrics::test_stack()] {
            let bp = breakpoints(&m);
            let full_rows = [bp.full[2], bp.full[3]].map(|w| plan_full(w, &m));
            let folded = bp.folded.map(|w| plan_folded(w, &m).blocks[1]);
            for clusters in &machines {
                for plan in &full_rows {
                    let CoresAt::Row(room) = plan.cores else {
                        panic!("{:?} has a Cores row", plan.kind);
                    };
                    let mode = plan.cores_mode(clusters, &m).expect("a Cores row");
                    let ladder = [CoresMode::Stack, CoresMode::Tiny];
                    assert_row_fits(
                        clusters,
                        mode,
                        &ladder,
                        room.w,
                        plan.rows.cell,
                        CORES_CAPTION_PT,
                        &m,
                    );
                }
                for block in &folded {
                    let mode = block.cores_mode(clusters, &m);
                    let caption_pt = block.kind.cores_caption_pt();
                    let ladder = block.kind.cores_ladder();
                    assert_row_fits(
                        clusters,
                        mode,
                        ladder,
                        block.cores.w,
                        block.rows.cell,
                        caption_pt,
                        &m,
                    );
                }
            }

            let unread = [Cluster {
                count: 0,
                name: None,
            }];
            for clusters in [&[][..], &unread] {
                let rows = full_rows.iter().map(|plan| {
                    let CoresAt::Row(room) = plan.cores else {
                        panic!("{:?} has a Cores row", plan.kind);
                    };
                    (plan.cores_mode(clusters, &m).expect("a Cores row"), room.w)
                });
                let blocks = folded
                    .iter()
                    .map(|block| (block.cores_mode(clusters, &m), block.cores.w));
                for (mode, room) in rows.chain(blocks) {
                    assert_eq!(mode, CoresMode::CountOnly);
                    let words = cores_count_text(clusters, room, CORES_CAPTION_PT, &m);
                    assert!(words.ends_with("not read"), "the row says {words:?}");
                }
            }
        }
    }

    #[test]
    fn an_m5_draws_its_cores_large_where_it_can() {
        let m = hn();
        let m5 = [
            Cluster {
                count: 6,
                name: Some("Super"),
            },
            Cluster {
                count: 12,
                name: Some("Performance"),
            },
        ];
        assert_eq!(
            plan_full(1119.0, &m).cores_mode(&m5, &m),
            None,
            "the gauge's slot"
        );
        assert_eq!(
            plan_full(550.0, &m).cores_mode(&m5, &m),
            Some(CoresMode::Stack)
        );
        assert_eq!(
            plan_folded(1119.0, &m).blocks[1].cores_mode(&m5, &m),
            CoresMode::Big
        );
        assert_eq!(
            cores_count_text(&m5, 400.0, CORES_CAPTION_PT, &m),
            "18 cores: 6 Super, 12 Performance"
        );
        assert_eq!(
            cores_count_text(&m5, 60.0, CORES_CAPTION_PT, &m),
            "18 cores"
        );
    }

    fn placed(head: &HeaderPlan, item: KeyItem) -> Option<f64> {
        head.key
            .placed
            .iter()
            .find(|&&(i, _)| i == item)
            .map(|&(_, x)| x)
    }

    fn label_x(head: &HeaderPlan, label: Label) -> Option<f64> {
        head.labels.iter().find(|l| l.label == label).map(|l| l.x)
    }

    #[test]
    fn the_key_goes_where_the_spec_says() {
        let m = hn();

        // The time strip's header carries the time label, the scale's ticks (the live edge's
        // design) and the key between them; what the ticks leave no room for goes, by weight,
        // to the next free span.
        let plan = plan_full(1119.0, &m);
        let [label, _] = time_words(plan.strip);
        assert_eq!(label, "10 min ago", "a Full strip spans ten minutes");
        let time_w = m.width(&label, HEADER_PT, false);
        let head = header_full(&plan, TEXT, &m);
        let after_time = plan.strip.x + time_w + KEY_GAP;
        let ticks: Vec<u32> = head.ruler.iter().map(|t| t.age_s).collect();
        assert_eq!(ticks, [300, 60]);
        assert_eq!(placed(&head, KeyItem::Over), Some(after_time));
        assert_eq!(
            (
                placed(&head, KeyItem::Ramp),
                placed(&head, KeyItem::NotRead)
            ),
            (None, None),
            "wide keys only the over mark"
        );
        let scope = |bar| {
            head.labels
                .iter()
                .find(|l| l.label == Label::Scope(bar))
                .map(|l| l.text.as_str())
        };
        assert_eq!(
            [Bar::Global, Bar::Aterm, Bar::Tab].map(scope),
            [Some("this Mac"), Some("aterm"), Some("Tab 2")]
        );
        assert_eq!(
            label_x(&head, Label::Now),
            plan.pct.map(Col::end),
            "\"now\" beside the live edge's dot"
        );
        assert_eq!(label_x(&head, Label::Details), None);
        // With no over mark on screen the key is empty, and the scale keeps what it had.
        let calm = header_full(
            &plan,
            HeaderText {
                over: false,
                ..TEXT
            },
            &m,
        );
        assert!(calm.key.placed.is_empty(), "{:?}", calm.key.placed);
        assert!(calm.ruler.len() >= head.ruler.len());

        let plan = plan_full(550.0, &m);
        let head = header_full(&plan, TEXT, &m);
        assert_eq!(placed(&head, KeyItem::Over), Some(plan.name.x));
        let global = label_x(&head, Label::Scope(Bar::Global));
        assert_eq!(global, plan.amount.map(|a| a.x));
        let global_end =
            global.unwrap_or_default() + SWATCH_ADVANCE + m.width("Global", HEADER_PT, false);
        let ramp = placed(&head, KeyItem::Ramp).expect("the ramp is placed");
        assert!(
            (ramp - (global_end + KEY_GAP)).abs() < 1e-9,
            "the ramp at {ramp}"
        );

        let plan = plan_full(350.0, &m);
        let head = header_full(&plan, TEXT, &m);
        assert_eq!(placed(&head, KeyItem::Over), Some(plan.name.x));
        assert_eq!(
            label_x(&head, Label::Scope(Bar::Global)),
            plan.bar.map(|b| b.x)
        );
        assert!(label_x(&head, Label::Scope(Bar::Tab)).is_some());

        // Folded says the glance: "over max" alone, over block B's strip (block A's carries the
        // time label and the scale), and only while a strip draws an over mark.
        let plan = plan_folded(1119.0, &m);
        let head = header_folded(&plan, TEXT, [None, None], &m);
        let [a, b] = plan.blocks;
        assert_eq!(label_x(&head, Label::Time), Some(a.strip.x));
        // Its 150 pt strip spans half a minute, a reading's second a cell (§12 of the live
        // edge's design): "30 s ago" over its left end, and twenty and ten seconds marked.
        assert_eq!(
            head.labels
                .iter()
                .find(|l| l.label == Label::Time)
                .map(|l| l.text.as_str()),
            Some("30 s ago")
        );
        assert_eq!(
            head.ruler.iter().map(|t| t.age_s).collect::<Vec<_>>(),
            [20, 10]
        );
        assert_eq!(placed(&head, KeyItem::Over), Some(b.strip.x));
        assert_eq!(
            (
                placed(&head, KeyItem::Ramp),
                placed(&head, KeyItem::NotRead)
            ),
            (None, None)
        );
        let calm = HeaderText {
            over: false,
            ..TEXT
        };
        assert!(
            header_folded(&plan, calm, [None, None], &m)
                .key
                .placed
                .is_empty()
        );
        let scopes = |head: &HeaderPlan| -> Vec<(f64, String)> {
            head.labels
                .iter()
                .filter(|l| matches!(l.label, Label::Scope(_)))
                .map(|l| (l.x, l.text.clone()))
                .collect()
        };
        assert_eq!(
            scopes(&head),
            [
                (a.amount.map_or(0.0, |c| c.x), "this Mac".to_owned()),
                (a.aterm.map_or(0.0, |c| c.x), "aterm".to_owned()),
                (a.tab.map_or(0.0, |c| c.x), "Tab 2".to_owned()),
                (b.amount.map_or(0.0, |c| c.x), "this Mac".to_owned()),
                (b.aterm.map_or(0.0, |c| c.x), "aterm".to_owned()),
                (b.tab.map_or(0.0, |c| c.x), "Tab 2".to_owned()),
            ],
            "wide heads each block's columns"
        );
        assert_eq!(label_x(&head, Label::Details), None);
        // Mid keys the bar once, over block A.
        let plan = plan_folded(1039.0, &m);
        let head = header_folded(&plan, TEXT, [None, None], &m);
        let bar = plan.blocks[0].bar.map_or(0.0, |c| c.x);
        assert_eq!(
            scopes(&head)
                .iter()
                .map(|(x, t)| (*x >= bar && *x < plan.divider, t.as_str()))
                .collect::<Vec<_>>(),
            [(true, "this Mac"), (true, "aterm"), (true, "Tab 2")]
        );
    }

    #[test]
    fn an_fslim_state_takes_the_header() {
        let m = hn();
        let plan = plan_folded(350.0, &m);
        let [a, b] = plan.blocks;
        let long = StateLabel {
            full: "Disk full ~34 min +1",
            bare: "Disk full ~34 min",
        };
        let head = header_folded(&plan, TEXT, [Some(long), None], &m);
        let states: Vec<_> = head
            .labels
            .iter()
            .filter(|l| l.label == Label::State)
            .collect();
        assert_eq!(states.len(), 1);
        assert_eq!(
            (states[0].x, states[0].text.as_str()),
            (a.name.x, "Disk full ~34 min +1")
        );
        assert!(time_on(&head, b.strip, &m), "{:?}", head.labels);

        let head = header_folded(&plan, TEXT, [Some(long), Some(long)], &m);
        let b_state = head
            .labels
            .iter()
            .find(|l| l.label == Label::State && l.x == b.name.x);
        let b_state = b_state.expect("block B names its state");
        assert!(b_state.text.ends_with('…'));
        assert!(
            MARK_ADVANCE + m.width(&b_state.text, HEADER_PT, true) <= b.strip.x - 6.0 - b.name.x
        );
        assert!(time_on(&head, b.strip, &m), "{:?}", head.labels);
    }

    /// Whether `head`'s time label stands on `strip`: over its left end, or beside it, ending
    /// [`STRIP_LEAD`] short of it with a tick marking that end.
    fn time_on(head: &HeaderPlan, strip: Strip, m: &FaceMetrics) -> bool {
        head.labels.iter().any(|l| {
            l.label == Label::Time
                && (l.x == strip.x && head.time_tick.is_none()
                    || (l.x + m.width(&l.text, HEADER_PT, false) - (strip.x - STRIP_LEAD)).abs()
                        < 1e-9
                        && head.time_tick == Some(strip.x))
        })
    }

    #[test]
    fn key_is_placed() {
        let long = StateLabel {
            full: "Disk emergency, out of file slots +3",
            bare: "Disk emergency, out of file slots",
        };
        let short = StateLabel {
            full: "Memory warn",
            bare: "Memory warn",
        };
        let check = |head: &HeaderPlan, m: &FaceMetrics, exempt: bool, what: &str| {
            let fits = head
                .key
                .spans
                .iter()
                .any(|s| s.x1 - s.x0 >= KeyItem::Over.width(m));
            assert_eq!(head.key.has(KeyItem::Over), fits, "{what}");
            assert!(exempt || fits, "{what}: no span holds over max");
            for &(item, x) in &head.key.placed {
                let inside = |s: &KeySpan| x >= s.x0 && x + item.width(m) <= s.x1;
                assert!(head.key.spans.iter().any(inside), "{what}: {item:?} at {x}");
            }
            let times = head
                .labels
                .iter()
                .filter(|l| l.label == Label::Time)
                .count();
            assert_eq!(times, 1, "{what}");
        };
        for m in [hn(), FaceMetrics::test_stack()] {
            for px2 in 320..=10_240 {
                let w = f64::from(px2) / 2.0;
                let full = plan_full(w, &m);
                if !full.ground_only {
                    check(
                        &header_full(&full, TEXT, &m),
                        &m,
                        false,
                        &format!("Full W {w}"),
                    );
                }
                let folded = plan_folded(w, &m);
                if folded.ground_only {
                    continue;
                }
                let fslim = folded.blocks[0].amount.is_none();
                let states: &[[Option<StateLabel<'_>>; 2]] = if fslim {
                    &[
                        [None, None],
                        [Some(long), None],
                        [None, Some(short)],
                        [Some(short), Some(long)],
                        [Some(long), Some(long)],
                    ]
                } else {
                    &[[Some(long), Some(long)]]
                };
                for &pair in states {
                    let both = fslim && pair.iter().all(Option::is_some);
                    let head = header_folded(&folded, TEXT, pair, &m);
                    check(&head, &m, both, &format!("Folded W {w}"));
                }
            }
        }
    }
}
