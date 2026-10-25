// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layout planner for the stats deck.
use crate::deck_face::FaceMetrics;
use crate::stats_deck::{FOLDED_PT, FULL_TABLE_PT};
use crate::stats_paint::Bar;

/// Stats deck layout form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckForm {
    /// v2: two blocks of four rows in 62 pt. Merged: row A alone.
    #[default]
    Folded,
    /// v2: header line and seven rows in full height, only where `stats_deck_size` is full.
    /// Merged: its three rows.
    Full,
}

/// Configured `[sys] stats_deck_size` preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckSizePref {
    /// v2: `Folded` whatever the window's height (the owner, 2026-10-24: the Full form had too
    /// many details). Merged: row A alone under 10 grid rows, its three rows from 11, and at
    /// 10 the form it had.
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

/// The merged deck keeps row A alone while the grid would hold fewer rows than this under its
/// three rows (spec §3, "under about 10 terminal rows"), and takes the three rows back at
/// [`THREE_ROWS_FROM`] or more, so a resize back and forth across one row never flips it.
const ROW_A_UNDER: u16 = 10;
const THREE_ROWS_FROM: u16 = 11;

/// What a window has room for, as its deck's style reads it ([`deck_form_for`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DeckRoom {
    /// v2: nothing; its form never follows the window.
    Unread,
    /// Merged: the rows the grid would hold under the three-row deck, whatever the form now,
    /// so the fold cannot feed back into the decision.
    Rows(u16),
}

/// Chooses the [`DeckForm`] for `room` under `pref`; the merged deck's fold keeps `prev` at 10
/// rows, so it has hysteresis.
pub(crate) fn deck_form_for(room: DeckRoom, prev: DeckForm, pref: DeckSizePref) -> DeckForm {
    match pref {
        DeckSizePref::Full => DeckForm::Full,
        DeckSizePref::Folded => DeckForm::Folded,
        DeckSizePref::Auto => {
            let full = match (room, prev) {
                (DeckRoom::Unread, _) => false,
                (DeckRoom::Rows(rows), DeckForm::Folded) => rows >= THREE_ROWS_FROM,
                (DeckRoom::Rows(rows), DeckForm::Full) => rows >= ROW_A_UNDER,
            };
            if full {
                DeckForm::Full
            } else {
                DeckForm::Folded
            }
        }
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
const B_MAX: f64 = 528.0 + 10.0 + DETAILS_MAX;
const CORES_BLOCK_W: f64 = 68.0;
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
const TIME_LABEL: &str = "10 min ago";
/// The time label's short form, when the long one leaves the strip no room for a tick.
const TIME_SHORT: &str = "\u{2013}10 min";
/// What a strip's header keeps clear at its right end for the now marker and its beat
/// ([`crate::stats_live`]), pt.
pub(crate) const MARKER_ROOM: f64 = 8.0;
/// The time scale's ticks, how long ago each is, most wanted first: the middle of the span, the
/// last minute, two minutes, half a minute.
const TICK_AGES: [u32; 4] = [300, 60, 120, 30];
/// The least room between a tick's words and the words beside them, pt.
const TICK_GAP: f64 = 6.0;
const DETAILS_LONG: &str = "Details · point at a row for more";
const SWATCH_ADVANCE: f64 = 0.62 * HEADER_PT + 3.0;
const MARK_ADVANCE: f64 = 1.1 * HEADER_PT;
const KEY_GAP: f64 = 10.0;

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

/// Ten-minute heat strip geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Strip {
    pub(crate) x: f64,
    pub(crate) cols: u32,
    pub(crate) period_s: u32,
    pub(crate) pitch: f64,
    pub(crate) cell: f64,
}

impl Strip {
    const SPAN_S: u32 = 600;

    fn new(x: f64, cols: u32, pitch: f64) -> Self {
        Self {
            x,
            cols,
            period_s: Self::SPAN_S / cols,
            pitch,
            cell: pitch - 1.0,
        }
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
    /// Dedicated block to the left of the rows.
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
    /// Chooses the [`CoresMode`] for `clusters` in a full deck of height `deck_pt`.
    pub(crate) fn cores_mode(
        &self,
        clusters: &[Cluster<'_>],
        deck_pt: f64,
        m: &FaceMetrics,
    ) -> CoresMode {
        match self.cores {
            CoresAt::Block { .. } => cores_block_mode(clusters, deck_pt),
            CoresAt::Row(room) => cores_row_mode(
                clusters,
                room.w,
                self.rows.cell,
                &[CoresMode::Stack, CoresMode::Tiny],
                CORES_CAPTION_PT,
                m,
            ),
        }
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
            plan.amount = Some(pen.col(8.0, 120.0));
            plan.aterm = Some(pen.col(8.0, 76.0));
            plan.tab = Some(pen.col(8.0, 76.0));
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

/// Column layout plan for the `Folded` deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FoldedPlan {
    /// Deck width in whole points.
    pub(crate) w: f64,
    /// Whether the deck is too narrow to draw anything besides its background.
    pub(crate) ground_only: bool,
    /// X-coordinate of the vertical divider between the two blocks.
    pub(crate) divider: f64,
    /// Layout plans for Block A and Block B.
    pub(crate) blocks: [BlockPlan; 2],
}

/// Computes the [`FoldedPlan`] for deck width `w_pt`.
pub(crate) fn plan_folded(w_pt: f64, m: &FaceMetrics) -> FoldedPlan {
    let w = whole_width(w_pt);
    let mut b = ((w - 2.0 * X0 - BLOCK_GUTTER) / 2.0).floor().max(0.0);
    let mut xa = X0;
    if b > B_MAX {
        b = B_MAX;
        xa = X0 + ((w - 2.0 * X0 - (2.0 * b + BLOCK_GUTTER)) / 2.0).floor();
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
    FoldedPlan {
        w,
        ground_only: w < MIN_TABLE_W || blocks.iter().any(|blk| blk.strip.end() > blk.at.end()),
        divider: xa + b + 14.0,
        blocks,
    }
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
    let strip = Strip::new(well.end() + strip_gap, cols, pitch);
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
            plan.pct = Some(Col::new(e + 6.0, 30.0));
            plan.amount = Some(Col::new(e + 44.0, 117.0));
            plan.aterm = Some(Col::new(e + 167.0, 70.0));
            let tab = Col::new(e + 243.0, 69.0);
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
    /// Minimum deck widths (`2B + 53`) for [`BlockKind::ALL`].
    pub(crate) folded: [f64; 4],
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
    let folded = block.map(|b| 2.0 * b + 2.0 * X0 + BLOCK_GUTTER);
    Breakpoints {
        full,
        block,
        folded,
    }
}

const CORES_CAPTION_PT: f64 = 9.5;

const CORES_BLOCK_SIZES: [(f64, f64); 5] =
    [(11.0, 9.0), (9.0, 7.0), (7.0, 5.5), (5.5, 4.0), (2.5, 2.0)];

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
    /// Grid of squares in the dedicated full-deck cores block.
    Block { pitch: f64, sq: f64, per_row: u32 },
    /// Single row of cell-high squares at 10 pt pitch.
    Big,
    /// Two stacked rows of half-height squares.
    Stack,
    /// Three stacked rows of 2 pt squares at 2.5 pt pitch.
    Tiny,
    /// Text-only core count when no square layout fits.
    CountOnly,
}

impl CoresMode {
    fn block((pitch, sq): (f64, f64)) -> Self {
        Self::Block {
            pitch,
            sq,
            per_row: (CORES_BLOCK_W / pitch).floor() as u32,
        }
    }
}

fn total_cores(clusters: &[Cluster<'_>]) -> usize {
    clusters.iter().map(|c| c.count).sum()
}

/// Chooses the [`CoresMode`] for the full-deck cores block at height `deck_pt`.
pub(crate) fn cores_block_mode(clusters: &[Cluster<'_>], deck_pt: f64) -> CoresMode {
    if total_cores(clusters) == 0 {
        return CoresMode::CountOnly;
    }
    CORES_BLOCK_SIZES
        .into_iter()
        .map(CoresMode::block)
        .find(|&mode| cores_block_bottom(clusters, mode) <= deck_pt - 6.0)
        .unwrap_or(CoresMode::CountOnly)
}

/// Formatted count and name caption for a cluster in the cores block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CoresCaption {
    /// Core count with trailing space in bold.
    pub(crate) count: String,
    /// Cluster name truncated to fit the 68 pt block width.
    pub(crate) name: String,
}

impl CoresCaption {
    /// Returns the advance width of the caption.
    #[cfg(test)]
    pub(crate) fn width(&self, m: &FaceMetrics) -> f64 {
        m.width(&self.count, CORES_CAPTION_PT, true) + m.width(&self.name, CORES_CAPTION_PT, false)
    }
}

/// Formats `cluster`'s caption for the 68 pt cores block, or `None` if unnamed.
pub(crate) fn cores_block_caption(cluster: &Cluster<'_>, m: &FaceMetrics) -> Option<CoresCaption> {
    let name = cluster.name?;
    let count = format!("{} ", cluster.count);
    let room = CORES_BLOCK_W - m.width(&count, CORES_CAPTION_PT, true);
    let name = m.fit(&[name], room, CORES_CAPTION_PT, false);
    Some(CoresCaption { count, name })
}

/// Returns the bottom y-coordinate of the last square in the cores block.
pub(crate) fn cores_block_bottom(clusters: &[Cluster<'_>], mode: CoresMode) -> f64 {
    let top = Rows::full(FullKind::Wide).top0;
    let CoresMode::Block { pitch, sq, per_row } = mode else {
        return top;
    };
    let mut y = top;
    let mut bottom = top;
    for cluster in clusters {
        let squares = y + if cluster.name.is_some() { 11.0 } else { 1.0 };
        let rows = cluster.count.div_ceil(per_row.max(1) as usize);
        bottom = squares + rows as f64 * pitch - (pitch - sq);
        y = bottom + 4.0;
    }
    bottom
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
            CoresMode::Block { .. } | CoresMode::CountOnly => f64::INFINITY,
        }
    };
    match mode {
        CoresMode::Block { .. } => f64::INFINITY,
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

/// Formats the header title for the full-deck cores block.
pub(crate) fn cores_block_title(clusters: &[Cluster<'_>], m: &FaceMetrics) -> String {
    let total = total_cores(clusters);
    if total == 0 {
        let [short, shortest] = CORES_NOT_READ;
        return m.fit(
            &["CPU cores not read", short, shortest],
            CORES_BLOCK_W,
            HEADER_PT,
            false,
        );
    }
    let long = format!("{total} CPU cores");
    m.fit(
        &[&long, &count_words(clusters)],
        CORES_BLOCK_W,
        HEADER_PT,
        false,
    )
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
    /// `"10 min ago"` over a strip.
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
    /// Machine noun after `"Global · "` (`"this Mac"` or `"this computer"`).
    pub(crate) machine: &'a str,
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

/// Assigns [`KeyItem`]s to free header `spans` to maximize priority weight.
pub(crate) fn place_key(mut spans: Vec<KeySpan>, m: &FaceMetrics) -> KeyPlan {
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
    /// label and the now marker, never so far that the words stop covering the tick.
    pub(crate) left: f64,
    /// Its words, `–5 min`, `–30 s`.
    pub(crate) text: String,
}

/// The words of a tick `age_s` back.
pub(crate) fn tick_words(age_s: u32) -> String {
    if age_s.is_multiple_of(60) {
        format!("\u{2013}{} min", age_s / 60)
    } else {
        format!("\u{2013}{age_s} s")
    }
}

/// Where a tick `age_s` back on `strip` goes when its words, `w` wide, must stay inside
/// `lo..hi`, or `None` when they cannot.
fn tick_slot(strip: Strip, age_s: u32, w: f64, (lo, hi): (f64, f64)) -> Option<(f64, f64)> {
    let x = strip.end() - f64::from(age_s) / f64::from(strip.period_s.max(1)) * strip.pitch;
    let (least, most) = (lo.max(x - w), (hi - w).min(x));
    (least <= most).then(|| (x, (x - w / 2.0).clamp(least, most)))
}

/// Complete layout plan for the header line and its legend key.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HeaderPlan {
    pub(crate) labels: Vec<HeaderLabel>,
    pub(crate) key: KeyPlan,
    /// The time scale over the strip that carries the time label, left to right.
    pub(crate) ruler: Vec<RulerTick>,
}

struct Header<'m> {
    m: &'m FaceMetrics,
    labels: Vec<HeaderLabel>,
    spans: Vec<KeySpan>,
    /// The strip whose header carries the time label and the scale.
    time: Option<Strip>,
}

impl<'m> Header<'m> {
    const fn new(m: &'m FaceMetrics) -> Self {
        Self {
            m,
            labels: Vec::new(),
            spans: Vec::new(),
            time: None,
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

    fn scope_keys(&mut self, mut x: f64, limit: f64, tab: &str) -> f64 {
        for (bar, words) in [
            (Bar::Global, "Global"),
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

    /// The key's spans with the time strip's own: what its label and `ticks` leave of it, short
    /// of the now marker.
    fn spans_with(&self, strip: Strip, label_w: f64, ticks: &[RulerTick]) -> Vec<KeySpan> {
        let mut spans = self.spans.clone();
        let mut x0 = strip.x + label_w + KEY_GAP;
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

    /// The ticks a time label `label_w` wide leaves room for on `strip`, each alone, most wanted
    /// first.
    fn tick_room(&self, strip: Strip, label_w: f64) -> Vec<RulerTick> {
        let room = (strip.x + label_w + TICK_GAP, strip.end() - MARKER_ROOM);
        TICK_AGES
            .iter()
            .filter_map(|&age_s| {
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

    /// The time label and its scale: the long label when some tick fits beside it, else the
    /// short one, else the long one alone. The scale takes ticks in order of want while none
    /// costs the key a weight it could place without them; when every tick costs some, the one
    /// costing least still goes, since the scale is what says how far back the strip reaches.
    fn ruler(&self, strip: Strip) -> (&'static str, Vec<RulerTick>) {
        let weight = |plan: &KeyPlan| -> u32 { plan.placed.iter().map(|(i, _)| i.weight()).sum() };
        for label in [TIME_LABEL, TIME_SHORT] {
            let label_w = self.words(label);
            let room = self.tick_room(strip, label_w);
            if room.is_empty() {
                continue;
            }
            let key_with = |ticks: &[RulerTick]| {
                weight(&place_key(self.spans_with(strip, label_w, ticks), self.m))
            };
            let clear = |chosen: &[RulerTick], t: &RulerTick| {
                let w = self.words(&t.text);
                chosen.iter().all(|c| {
                    let cw = self.words(&c.text);
                    t.left + w + TICK_GAP <= c.left || c.left + cw + TICK_GAP <= t.left
                })
            };
            let mut chosen: Vec<RulerTick> = Vec::new();
            let mut best = key_with(&[]);
            for t in &room {
                let mut trial = chosen.clone();
                trial.push(t.clone());
                if clear(&chosen, t) && key_with(&trial) == best {
                    chosen = trial;
                }
            }
            if chosen.is_empty() {
                let first = room
                    .iter()
                    .enumerate()
                    .max_by_key(|&(i, t)| (key_with(std::slice::from_ref(t)), usize::MAX - i))
                    .map(|(_, t)| t.clone());
                chosen.extend(first);
                best = key_with(&chosen);
                for t in &room {
                    let mut trial = chosen.clone();
                    trial.push(t.clone());
                    if clear(&chosen, t) && key_with(&trial) == best {
                        chosen = trial;
                    }
                }
            }
            chosen.sort_by(|a, b| a.left.total_cmp(&b.left));
            return (label, chosen);
        }
        (TIME_LABEL, Vec::new())
    }

    fn finish(mut self) -> HeaderPlan {
        let mut ruler = Vec::new();
        if let Some(strip) = self.time {
            let (label, ticks) = self.ruler(strip);
            self.spans = self.spans_with(strip, self.words(label), &ticks);
            self.label(Label::Time, strip.x, label);
            ruler = ticks;
        }
        HeaderPlan {
            key: place_key(self.spans, self.m),
            labels: self.labels,
            ruler,
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
    if let Some(pct) = plan.pct {
        head.label(Label::Now, pct.end(), "now");
    }
    if let CoresAt::Row(_) = plan.cores {
        head.span(plan.name.x, plan.strip.x - 6.0, false);
    }
    let long = matches!(plan.kind, FullKind::Wide | FullKind::Mid);
    if plan.kind == FullKind::Slim {
        if let (Some(bar), Some(amount)) = (plan.bar, plan.amount) {
            let end = head.scope_keys(bar.x, plan.w - X0, text.tab);
            head.span(end + 2.0, amount.end(), false);
        } else if let Some(bar) = plan.bar {
            head.scope_keys(bar.x, plan.w - X0, text.tab);
        }
        return head.finish();
    }
    if let Some(amount) = plan.amount {
        let x = plan.bar.map_or(amount.x, |bar| bar.x);
        let global = if long {
            format!("Global · {}", text.machine)
        } else {
            "Global".to_owned()
        };
        let w = head.scope(Bar::Global, x, global);
        head.span(x + w + KEY_GAP, amount.end(), false);
    }
    if let (Some(aterm), Some(tab)) = (plan.aterm, plan.tab) {
        let this_tab = format!("{} · this tab", text.tab);
        let aterm_forms: &[&str] = if long {
            &["aterm · all tabs", "aterm"]
        } else {
            &["aterm"]
        };
        let tab_forms: &[&str] = if long {
            &[&this_tab, text.tab]
        } else {
            &[text.tab]
        };
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
    head.finish()
}

/// Lays out the header line and key for a `Folded` deck plan.
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
    let mut free_strips: [Option<Strip>; 2] = [None; 2];
    let mut a_spilled = false;
    for (i, (block, state)) in plan.blocks.iter().zip(states).enumerate() {
        if let Some(pct) = block.pct {
            head.label(Label::Now, pct.end(), "now");
        }
        let name_room = block.strip.x - 6.0 - block.name.x;
        let mut strip_free = true;
        match state.filter(|_| block.amount.is_none()) {
            None => head.span(block.name.x, block.strip.x - 6.0, false),
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
                (amount, Bar::Global, "Global"),
                (aterm, Bar::Aterm, "aterm"),
                (tab, Bar::Tab, text.tab),
            ] {
                let words = m.fit(&[words], col.w - SWATCH_ADVANCE, HEADER_PT, false);
                let w = head.scope(bar, col.x, words);
                head.span(col.x + w + KEY_GAP, col.end(), false);
            }
        } else if let (Some(bar), Some(amount)) = (block.bar, block.amount) {
            let end = head.scope_keys(bar.x, block.at.end(), text.tab);
            head.span(end + 2.0, amount.end(), false);
        } else if let Some(amount) = block.amount {
            head.span(amount.x, amount.end(), false);
        }
        if let Some(details) = block.details {
            head.label(Label::Details, details.x, "Details");
            head.span(
                details.x + head.words("Details") + KEY_GAP,
                details.end(),
                false,
            );
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
        assert_eq!(p.amount, Some(Col::new(615.0, 120.0)));
        assert_eq!(p.aterm, Some(Col::new(743.0, 76.0)));
        assert_eq!(p.tab, Some(Col::new(827.0, 76.0)));
        assert_eq!(p.details, Some(Col::new(913.0, 194.0)));
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
        assert_eq!(p.details, Some(Col::new(973.0, 439.0)));

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
        assert_eq!(p.aterm.map(|a| a.x), Some(667.0));
        assert_eq!(p.tab.map(|t| t.x), Some(751.0));
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
        assert_eq!((p.gap_grow, p.offset), (16.0, 369.0));
        let CoresAt::Block { at: cores, divider } = p.cores else {
            panic!("wide has the cores block");
        };
        assert_eq!((cores.x, divider, p.name.x), (381.0, 457.0, 466.0));
        let details = p.details.expect("wide has Details");
        assert_eq!(details.w, 560.0);
        assert!((cores.x - (p.w - details.end())).abs() <= 1.0);
        assert_eq!([1700.0, 2000.0].map(|w| at(w).offset), [2.0, 119.0]);
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

        let p = at(1119.0);
        assert_eq!(kinds(&p), [BlockKind::Wide; 2]);
        assert_eq!(
            p.blocks.map(|b| b.at),
            [Col::new(12.0, 533.0), Col::new(574.0, 533.0)]
        );
        assert_eq!(p.divider, 559.0);
        assert_eq!(details(&p), [None, None]);
        let a = p.blocks[0];
        assert_eq!((a.strip.x, a.strip.cols, a.strip.pitch), (78.0, 30, 5.0));
        assert_eq!(a.pct, Some(Col::new(234.0, 30.0)));
        assert_eq!(a.amount, Some(Col::new(272.0, 117.0)));
        assert_eq!(
            (a.aterm, a.tab),
            (Some(Col::new(395.0, 70.0)), Some(Col::new(471.0, 69.0)))
        );

        let p = at(1424.0);
        assert_eq!(kinds(&p), [BlockKind::Wide; 2]);
        assert_eq!(
            details(&p),
            [Some(Col::new(550.0, 147.0)), Some(Col::new(1264.0, 147.0))]
        );

        let p = at(750.0);
        assert_eq!(kinds(&p), [BlockKind::Mid; 2]);
        assert_eq!(p.blocks[0].bar, Some(Col::new(197.0, 28.0)));
        assert_eq!(p.blocks[0].amount, Some(Col::new(231.0, 129.0)));

        let p = at(550.0);
        assert_eq!(kinds(&p), [BlockKind::Narrow; 2]);
        assert_eq!(p.blocks[0].bar, None);
        assert_eq!(p.blocks[0].amount, Some(Col::new(165.0, 95.0)));

        let p = at(640.0);
        assert_eq!(kinds(&p), [BlockKind::Narrow; 2]);
        assert_eq!(p.blocks[0].bar, Some(Col::new(165.0, 24.0)));
        assert_eq!(p.blocks[0].amount, Some(Col::new(194.0, 111.0)));

        let p = at(350.0);
        assert_eq!(kinds(&p), [BlockKind::Slim; 2]);
        assert_eq!(p.blocks[0].pct, Some(Col::new(132.0, 26.0)));
        assert_eq!(p.blocks[0].amount, None);

        let p = at(2560.0);
        assert_eq!(
            p.blocks.map(|b| b.at),
            [Col::new(167.0, 1098.0), Col::new(1294.0, 1098.0)]
        );
        assert_eq!(details(&p).map(|d| d.map(|d| d.w)), [Some(560.0); 2]);

        let p = at(320.0);
        assert_eq!(
            (kinds(&p), p.blocks.map(|b| b.pct)),
            ([BlockKind::Slim; 2], [None; 2])
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
        assert_eq!(helvetica.full, [1045.0, 839.0, 522.0, 323.0]);
        assert_eq!(helvetica.block, [528.0, 319.0, 245.0, 146.0]);
        assert_eq!(helvetica.folded, [1109.0, 691.0, 543.0, 345.0]);

        let stack = FaceMetrics::test_stack();
        assert_eq!(plan_full(1119.0, &stack).name.w, 59.0);
        assert_eq!(plan_full(550.0, &stack).name.w, 56.0);
        let dejavu = breakpoints(&stack);
        assert_eq!(dejavu.full, [1048.0, 842.0, 526.0, 328.0]);
        assert_eq!(dejavu.block, [532.0, 323.0, 250.0, 151.0]);
        assert_eq!(dejavu.folded, [1117.0, 699.0, 553.0, 355.0]);

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
                assert_eq!(plan_folded(w, m).blocks.map(|b| b.kind), [kind; 2]);
                let below = plan_folded(w - 1.0, m).blocks[0];
                if kind == BlockKind::Slim {
                    assert_eq!((below.kind, below.pct), (kind, None));
                } else {
                    assert_ne!(below.kind, kind);
                }
            }
        }
    }

    #[test]
    fn every_width_plans_inside_the_card() {
        for m in [hn(), FaceMetrics::test_stack()] {
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
                for block in &folded.blocks {
                    let period = if block.kind == BlockKind::Wide {
                        20
                    } else {
                        30
                    };
                    assert_eq!(
                        (block.strip.period_s, block.strip.cols * period),
                        (period, 600)
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
        use crate::stats_deck::{DeckStyle, StatsBars, deck_px};
        use DeckForm::{Folded, Full};
        let _: fn(DeckRoom, DeckForm, DeckSizePref) -> DeckForm = deck_form_for;
        let _: fn(StatsBars, DeckStyle, DeckForm, f64, usize) -> usize = deck_px;

        // v2 is Folded unless chosen Full, a window Full before included; merged follows rows.
        for prev in [Folded, Full] {
            assert_eq!(
                deck_form_for(DeckRoom::Unread, prev, DeckSizePref::Auto),
                Folded
            );
            for room in [DeckRoom::Unread]
                .into_iter()
                .chain([0, 9, 10, 11, 400].map(DeckRoom::Rows))
            {
                assert_eq!(deck_form_for(room, prev, DeckSizePref::Full), Full);
                assert_eq!(deck_form_for(room, prev, DeckSizePref::Folded), Folded);
            }
        }
        assert_eq!(
            deck_form_for(DeckRoom::Rows(400), Folded, DeckSizePref::Auto),
            Full,
            "CONTROL: merged's Automatic still takes its three rows"
        );

        assert_eq!(full_delta_px(2.0, 28), 84);
        assert_eq!(deck_px(StatsBars::Deck, DeckStyle::V2, Full, 2.0, 28), 208);
        for s in [1.0, 2.0] {
            let folded = (FOLDED_PT * s).round() as usize;
            let table = (FULL_TABLE_PT * s).round() as usize;
            for cell in 10..=40 {
                let at = format!("{cell} px cells at {s}×");
                assert_eq!(
                    deck_px(StatsBars::Deck, DeckStyle::V2, Folded, s, cell),
                    folded,
                    "{at}"
                );
                let full = deck_px(StatsBars::Deck, DeckStyle::V2, Full, s, cell);
                assert_eq!((full - folded) % cell, 0, "{at}");
                assert_eq!(full - folded, full_delta_px(s, cell), "{at}");
                assert!((table..table + cell).contains(&full), "{at}");
                for bars in [StatsBars::Sill, StatsBars::Off] {
                    for deck in [Folded, Full] {
                        assert_eq!(
                            deck_px(bars, DeckStyle::V2, deck, s, cell),
                            0,
                            "{bars:?} {at}"
                        );
                    }
                }
            }
        }
    }

    /// The merged deck folds to row A below 10 grid rows under its three rows and unfolds at
    /// 11, keeping its form at exactly 10: a resize back and forth across one row never flips it.
    #[test]
    fn the_merged_deck_folds_under_ten_rows_and_unfolds_at_eleven() {
        use DeckForm::{Folded, Full};
        let rows = [0, 1, 8, 9, 10, 11, 12, 400].map(DeckRoom::Rows);
        let auto = DeckSizePref::Auto;
        assert_eq!(
            rows.map(|r| deck_form_for(r, Full, auto)),
            [Folded, Folded, Folded, Folded, Full, Full, Full, Full]
        );
        assert_eq!(
            rows.map(|r| deck_form_for(r, Folded, auto)),
            [Folded, Folded, Folded, Folded, Folded, Full, Full, Full]
        );
        for prev in [Folded, Full] {
            for r in 1..30u16 {
                for there in [r - 1, r + 1] {
                    let at = deck_form_for(DeckRoom::Rows(r), prev, auto);
                    let went = deck_form_for(DeckRoom::Rows(there), at, auto);
                    let back = deck_form_for(DeckRoom::Rows(r), went, auto);
                    assert_eq!(
                        back, went,
                        "{prev:?} at {r} rows, to {there} and back: a flap"
                    );
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
                let total: usize = clusters.iter().map(|c| c.count).sum();
                let title = cores_block_title(clusters, &m);
                assert!(title.starts_with(&format!("{total} ")) && !title.ends_with('…'));
                let floor = FULL_TABLE_PT - 6.0;
                let mode = cores_block_mode(clusters, FULL_TABLE_PT);
                let CoresMode::Block { pitch, sq, per_row } = mode else {
                    panic!("{clusters:?} lost the block's grid: {mode:?}");
                };
                assert!(cores_block_bottom(clusters, mode) <= floor);
                for &size in CORES_BLOCK_SIZES.iter().take_while(|size| size.0 > pitch) {
                    let larger = CoresMode::block(size);
                    assert!(
                        cores_block_bottom(clusters, larger) > floor,
                        "{clusters:?} fits {larger:?}"
                    );
                }
                assert!(f64::from(per_row - 1) * pitch + sq <= CORES_BLOCK_W);
                for cluster in clusters {
                    let caption = cores_block_caption(cluster, &m);
                    assert_eq!(caption.is_some(), cluster.name.is_some());
                    if let Some(caption) = caption {
                        assert_eq!(caption.count, format!("{} ", cluster.count));
                        assert!(
                            !caption.name.is_empty() && caption.width(&m) <= CORES_BLOCK_W + 0.01,
                            "{caption:?} leaves the block"
                        );
                    }
                }
                for plan in &full_rows {
                    let CoresAt::Row(room) = plan.cores else {
                        panic!("{:?} has a Cores row", plan.kind);
                    };
                    let mode = plan.cores_mode(clusters, FULL_TABLE_PT, &m);
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
                let title = cores_block_title(clusters, &m);
                assert!(title.ends_with("not read"), "the title says {title:?}");
                assert_eq!(
                    cores_block_mode(clusters, FULL_TABLE_PT),
                    CoresMode::CountOnly
                );
                let rows = full_rows.iter().map(|plan| {
                    let CoresAt::Row(room) = plan.cores else {
                        panic!("{:?} has a Cores row", plan.kind);
                    };
                    (plan.cores_mode(clusters, FULL_TABLE_PT, &m), room.w)
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

        let huge = [Cluster {
            count: 1024,
            name: None,
        }];
        let smallest = CORES_BLOCK_SIZES[CORES_BLOCK_SIZES.len() - 1];
        assert!(cores_block_bottom(&huge, CoresMode::block(smallest)) > FULL_TABLE_PT - 6.0);
        assert_eq!(cores_block_mode(&huge, FULL_TABLE_PT), CoresMode::CountOnly);
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
            cores_block_mode(&m5, 104.0),
            CoresMode::Block {
                pitch: 11.0,
                sq: 9.0,
                per_row: 6
            }
        );
        assert_eq!(cores_block_title(&m5, &m), "18 CPU cores");
        assert_eq!(
            plan_full(550.0, &m).cores_mode(&m5, 104.0, &m),
            CoresMode::Stack
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
        let time_w = m.width(TIME_LABEL, HEADER_PT, false);

        // The time strip's header carries the time label, the scale's ticks (the live edge's
        // design) and the key between them; what the ticks leave no room for goes, by weight,
        // to the next free span.
        let plan = plan_full(1119.0, &m);
        let head = header_full(&plan, TEXT, &m);
        let after_time = plan.strip.x + time_w + KEY_GAP;
        let ticks: Vec<(u32, f64)> = head.ruler.iter().map(|t| (t.age_s, t.left)).collect();
        assert_eq!(ticks.iter().map(|t| t.0).collect::<Vec<_>>(), [300, 60]);
        assert_eq!(placed(&head, KeyItem::Over), Some(after_time));
        let mid_tick_end = ticks[0].1 + m.width(&tick_words(300), HEADER_PT, false) + KEY_GAP;
        assert_eq!(placed(&head, KeyItem::NotRead), Some(mid_tick_end));
        let global = label_x(&head, Label::Scope(Bar::Global)).unwrap_or_default();
        let global_end = global + SWATCH_ADVANCE + m.width("Global · this Mac", HEADER_PT, false);
        assert!(
            placed(&head, KeyItem::Ramp).is_some_and(|x| (x - (global_end + KEY_GAP)).abs() < 1e-9),
            "the ramp after the global scope: {:?}",
            head.key.placed
        );
        let scope = |bar| head.labels.iter().find(|l| l.label == Label::Scope(bar));
        assert_eq!(
            scope(Bar::Global).map(|l| l.text.as_str()),
            Some("Global · this Mac")
        );
        assert_eq!(
            scope(Bar::Aterm).map(|l| l.text.as_str()),
            Some("aterm · all tabs")
        );
        assert_eq!(
            scope(Bar::Tab).map(|l| l.text.as_str()),
            Some("Tab 2 · this tab")
        );
        assert_eq!(label_x(&head, Label::Now), plan.pct.map(Col::end));
        let details = head.labels.iter().find(|l| l.label == Label::Details);
        assert_eq!(details.map(|l| l.text.as_str()), Some(DETAILS_LONG));

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

        let plan = plan_folded(1119.0, &m);
        let head = header_folded(&plan, TEXT, [None, None], &m);
        let [a, b] = plan.blocks;
        assert_eq!(label_x(&head, Label::Time), Some(a.strip.x));
        assert_eq!(
            head.ruler.iter().map(|t| t.age_s).collect::<Vec<_>>(),
            [300, 60]
        );
        assert_eq!(placed(&head, KeyItem::Over), Some(a.name.x));
        assert_eq!(placed(&head, KeyItem::Ramp), Some(b.strip.x));
        let ramp_end = b.strip.x + KeyItem::Ramp.width(&m) + KEY_GAP;
        assert_eq!(placed(&head, KeyItem::NotRead), Some(ramp_end));
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
        assert_eq!(label_x(&head, Label::Time), Some(b.strip.x));

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
        assert_eq!(label_x(&head, Label::Time), Some(b.strip.x));
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
