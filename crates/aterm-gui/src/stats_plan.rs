// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layout planner for the stats deck (`DeckForm`, column coordinates, core cluster modes, and header key placement).
use aterm_render::font::Font;

use crate::stats_deck::{FOLDED_PT, FULL_TABLE_PT};
use crate::stats_paint::Bar;
use crate::tray_raster::UnicodeChars;

/// Stats deck layout form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckForm {
    /// Two blocks of four rows in 62 pt.
    #[default]
    Folded,
    /// Header line and seven rows in full height.
    Full,
}

/// Configured `[sys] stats_deck_size` preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckSizePref {
    /// Choose `Full` or `Folded` from the window height.
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

/// Auto turns a Folded window Full once its avail reaches this (pt)...
const FULL_FROM_AVAIL: f64 = 870.0;
/// ...and keeps a Full window Full down to this, so a window resized across
/// one line does not flap between the forms.
const FULL_DOWN_TO_AVAIL: f64 = 850.0;

/// Chooses the [`DeckForm`] for `avail_pt` with hysteresis from `prev`.
pub(crate) fn deck_form_for(avail_pt: f64, prev: DeckForm, pref: DeckSizePref) -> DeckForm {
    match pref {
        DeckSizePref::Full => DeckForm::Full,
        DeckSizePref::Folded => DeckForm::Folded,
        DeckSizePref::Auto => {
            let least = match prev {
                DeckForm::Folded => FULL_FROM_AVAIL,
                DeckForm::Full => FULL_DOWN_TO_AVAIL,
            };
            if avail_pt >= least {
                DeckForm::Full
            } else {
                DeckForm::Folded
            }
        }
    }
}

/// Computes the available grid height in points under the folded deck.
pub(crate) fn avail_pt(frame_h_px: f64, titlebar_px: f64, scale: f64) -> f64 {
    if !(scale.is_finite() && scale > 0.0) {
        return 0.0;
    }
    (frame_h_px - titlebar_px - (FOLDED_PT * scale).round()) / scale
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

/// The chars a [`FaceMetrics`] table holds, in char order: printable ASCII
/// and the punctuation the deck's text uses (§10).
fn deck_glyphs() -> impl Iterator<Item = char> {
    (' '..='~').chain(['·', '×', '–', '’', '…'])
}

/// The units per em of [`HELVETICA_NEUE`]'s advances.
#[cfg(test)]
const HELVETICA_NEUE_UPEM: f64 = 1000.0;

/// Helvetica Neue's advances in font units, `(char, regular, Medium)`: the
/// macOS chrome's `TextFace::Ui` and `TextFace::UiBold`
/// (`/System/Library/Fonts/HelveticaNeue.ttc` faces 0 and 10), for every
/// char of [`deck_glyphs`].
#[cfg(test)]
const HELVETICA_NEUE: [(char, u16, u16); 100] = [
    (' ', 278, 278),
    ('!', 259, 278),
    ('"', 426, 444),
    ('#', 556, 556),
    ('$', 556, 556),
    ('%', 1000, 1000),
    ('&', 630, 648),
    ('\'', 278, 278),
    ('(', 259, 278),
    (')', 259, 278),
    ('*', 352, 370),
    ('+', 600, 600),
    (',', 278, 278),
    ('-', 389, 389),
    ('.', 278, 278),
    ('/', 333, 352),
    ('0', 556, 556),
    ('1', 556, 556),
    ('2', 556, 556),
    ('3', 556, 556),
    ('4', 556, 556),
    ('5', 556, 556),
    ('6', 556, 556),
    ('7', 556, 556),
    ('8', 556, 556),
    ('9', 556, 556),
    (':', 278, 278),
    (';', 278, 278),
    ('<', 600, 600),
    ('=', 600, 600),
    ('>', 600, 600),
    ('?', 556, 556),
    ('@', 800, 800),
    ('A', 648, 667),
    ('B', 685, 704),
    ('C', 722, 722),
    ('D', 704, 722),
    ('E', 611, 630),
    ('F', 574, 593),
    ('G', 759, 759),
    ('H', 722, 722),
    ('I', 259, 278),
    ('J', 519, 537),
    ('K', 667, 685),
    ('L', 556, 574),
    ('M', 871, 889),
    ('N', 722, 722),
    ('O', 760, 760),
    ('P', 648, 667),
    ('Q', 760, 760),
    ('R', 685, 704),
    ('S', 648, 648),
    ('T', 574, 593),
    ('U', 722, 722),
    ('V', 611, 611),
    ('W', 926, 944),
    ('X', 611, 648),
    ('Y', 648, 648),
    ('Z', 611, 630),
    ('[', 259, 296),
    ('\\', 333, 352),
    (']', 259, 296),
    ('^', 600, 600),
    ('_', 500, 500),
    ('`', 222, 241),
    ('a', 537, 556),
    ('b', 593, 611),
    ('c', 537, 556),
    ('d', 593, 611),
    ('e', 537, 556),
    ('f', 296, 315),
    ('g', 574, 593),
    ('h', 556, 574),
    ('i', 222, 241),
    ('j', 222, 241),
    ('k', 519, 537),
    ('l', 222, 241),
    ('m', 853, 870),
    ('n', 556, 574),
    ('o', 574, 593),
    ('p', 593, 611),
    ('q', 593, 611),
    ('r', 333, 352),
    ('s', 500, 519),
    ('t', 315, 333),
    ('u', 556, 574),
    ('v', 500, 519),
    ('w', 758, 778),
    ('x', 518, 537),
    ('y', 500, 519),
    ('z', 480, 500),
    ('{', 333, 296),
    ('|', 222, 222),
    ('}', 333, 296),
    ('~', 600, 600),
    ('·', 278, 278),
    ('×', 600, 600),
    ('–', 500, 500),
    ('’', 278, 278),
    ('…', 1000, 1000),
];

/// Glyph advance and kerning metrics for the chrome UI regular and bold faces.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FaceMetrics {
    /// `(char, regular, bold)` advances per em, in char order.
    advances: Vec<(char, f64, f64)>,
    /// `((left, right), regular, bold)` kerning per em of the pairs whose
    /// kerning is not zero, in pair order.
    kerns: Vec<((char, char), f64, f64)>,
    /// What the pen adds to each advance of a char the table has, per em
    /// (`tray_raster::UI_TRACKING_EM`).
    tracking: f64,
    /// What a char the table lacks measures: its widest advance, and never
    /// less than the pen's 0.6 em placeholder, so a word in other glyphs is
    /// not planned short.
    missing: f64,
}

impl FaceMetrics {
    fn from_rows(rows: impl Iterator<Item = (char, f64, f64)>) -> Self {
        let mut advances: Vec<_> = rows.collect();
        advances.sort_by_key(|&(ch, _, _)| ch);
        let missing = advances
            .iter()
            .map(|&(_, regular, bold)| regular.max(bold))
            .fold(0.6, f64::max);
        Self {
            advances,
            kerns: Vec::new(),
            tracking: 0.0,
            missing,
        }
    }

    /// Returns embedded metrics for macOS Helvetica Neue.
    #[cfg(test)]
    pub(crate) fn helvetica_neue() -> Self {
        Self::from_rows(HELVETICA_NEUE.iter().map(|&(ch, regular, bold)| {
            (
                ch,
                f64::from(regular) / HELVETICA_NEUE_UPEM,
                f64::from(bold) / HELVETICA_NEUE_UPEM,
            )
        }))
    }

    /// Extracts [`FaceMetrics`] from installed regular and bold [`Font`]s.
    pub(crate) fn of_faces(regular: &Font, bold: &Font) -> Self {
        let mut m = Self::advances_of(regular, bold);
        let glyphs: Vec<(char, u16, u16)> = m
            .advances
            .iter()
            .map(|&(ch, _, _)| {
                (
                    ch,
                    regular.unicode_glyph_index(ch),
                    bold.unicode_glyph_index(ch),
                )
            })
            .collect();
        for &(left, lr, lb) in &glyphs {
            for &(right, rr, rb) in &glyphs {
                let (r, b) = (em_kern(regular, lr, rr), em_kern(bold, lb, rb));
                if r != 0.0 || b != 0.0 {
                    m.kerns.push(((left, right), r, b));
                }
            }
        }
        m.tracking = f64::from(crate::tray_raster::UI_TRACKING_EM);
        m
    }

    /// A face pair's own advances alone, with no kerning or tracking.
    fn advances_of(regular: &Font, bold: &Font) -> Self {
        Self::from_rows(
            deck_glyphs()
                .filter(|&ch| regular.maps(ch) && bold.maps(ch))
                .map(|ch| (ch, em_advance(regular, ch), em_advance(bold, ch))),
        )
    }

    /// Returns placeholder metrics with 0.6 em fixed advances.
    pub(crate) fn placeholder() -> Self {
        Self::from_rows(std::iter::empty())
    }

    /// Returns metrics for the embedded DejaVu Sans Mono test font.
    #[cfg(test)]
    pub(crate) fn test_stack() -> Self {
        let (_, face) = aterm_render::shared_parsed_face(aterm_render::embedded_font(), 0)
            .expect("the embedded DejaVu Sans Mono parses");
        Self::advances_of(&face, &face)
    }

    /// Measures the advance width of `s` at `pt` in regular or bold.
    pub(crate) fn width(&self, s: &str, pt: f64, bold: bool) -> f64 {
        let mut em = 0.0;
        let mut prev: Option<char> = None;
        for ch in s.chars() {
            match self.advance(ch, bold) {
                Some(advance) => {
                    em += advance + self.tracking;
                    if let Some(left) = prev {
                        em += self.kern(left, ch, bold);
                    }
                    prev = Some(ch);
                }
                None => {
                    em += self.missing;
                    prev = None;
                }
            }
        }
        em * pt
    }

    fn advance(&self, ch: char, bold: bool) -> Option<f64> {
        self.advances
            .binary_search_by_key(&ch, |&(c, _, _)| c)
            .ok()
            .and_then(|at| self.advances.get(at))
            .map(|&(_, regular, heavy)| if bold { heavy } else { regular })
    }

    fn kern(&self, left: char, right: char, bold: bool) -> f64 {
        self.kerns
            .binary_search_by_key(&(left, right), |&(pair, _, _)| pair)
            .ok()
            .and_then(|at| self.kerns.get(at))
            .map_or(
                0.0,
                |&(_, regular, heavy)| if bold { heavy } else { regular },
            )
    }

    /// Returns the first candidate in `forms` that fits `room`, or truncates the last with `…`.
    pub(crate) fn fit(&self, forms: &[&str], room: f64, pt: f64, bold: bool) -> String {
        if let Some(form) = forms
            .iter()
            .find(|form| self.width(form, pt, bold) <= room + 0.01)
        {
            return (*form).to_owned();
        }
        let Some(last) = forms.last() else {
            return String::new();
        };
        let cuts = last.char_indices().map(|(at, _)| at).rev();
        cuts.map(|at| last.get(..at).unwrap_or_default().trim_end())
            .filter(|head| !head.is_empty())
            .map(|head| format!("{head}…"))
            .find(|cut| self.width(cut, pt, bold) <= room)
            .unwrap_or_default()
    }
}

/// `ch`'s advance per em in `font`, measured at the face's own units per em
/// so the f32 arithmetic is exact.
fn em_advance(font: &Font, ch: char) -> f64 {
    let upem = (1.0 / font.scale_factor(1.0)).round();
    f64::from(font.char_metrics(ch, upem).advance_width) / f64::from(upem)
}

/// The kerning per em `font` puts between the glyphs `left` and `right`, as
/// the pen reads it ([`UnicodeChars::char_kern`], by the same glyph ids);
/// zero for a pair it does not kern.
fn em_kern(font: &Font, left: u16, right: u16) -> f64 {
    let upem = (1.0 / font.scale_factor(1.0)).round();
    font.horizontal_kern_indexed(left, right, upem)
        .map_or(0.0, |k| f64::from(k) / f64::from(upem))
}

/// The table's side margins: it runs from `X0` to `W − X0` (§3.2).
const X0: f64 = 12.0;
/// Under this width the deck draws its ground only (§3.2).
const MIN_TABLE_W: f64 = 160.0;
/// Details' least width, and its greatest (§3.2).
const DETAILS_MIN: f64 = 120.0;
const DETAILS_MAX: f64 = 560.0;
/// The most each gap right of the strip grows in a very wide window (§3.3).
const GROW_GAP_MAX: f64 = 16.0;
/// The least amount column of slim and of fnarrow (§3.2, §4.1).
const SLIM_AMOUNT_MIN: f64 = 92.0;
/// Below its fit, slim's amount is not drawn under this width (§3.2).
const AMOUNT_DRAWN_MIN: f64 = 40.0;
/// The gutter between the Folded blocks; the divider sits 14 pt into it.
const BLOCK_GUTTER: f64 = 29.0;
/// A Folded block's greatest width: fwide's columns, then Details at its
/// greatest (§4.1).
const B_MAX: f64 = 528.0 + 10.0 + DETAILS_MAX;
/// The cores block's width (§3.4).
const CORES_BLOCK_W: f64 = 68.0;
/// The labels a name column must hold (§3.2).
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
/// The size of the header's words and of the key's (§3.1).
const HEADER_PT: f64 = 9.5;
/// The time label over a strip (§3.5).
const TIME_LABEL: &str = "10 min ago";
/// The Details header when it fits, else its first word (§3.5).
const DETAILS_LONG: &str = "Details · point at a row for more";
/// A scope swatch's advance before its words: a Panel 0.62 em wide, then
/// 3 pt of air (§3.5).
const SWATCH_ADVANCE: f64 = 0.62 * HEADER_PT + 3.0;
/// A mark's advance before its words (§10).
const MARK_ADVANCE: f64 = 1.1 * HEADER_PT;
/// The gap a key item keeps from the label before it and the item after it.
const KEY_GAP: f64 = 10.0;

/// A name column at least `base` wide that holds every row name in UiBold
/// at `pt`, with 1.5 pt to spare: wider on a wider face (§3.2).
fn name_width(base: f64, pt: f64, m: &FaceMetrics) -> f64 {
    let widest = ROW_NAMES
        .iter()
        .map(|name| m.width(name, pt, true))
        .fold(0.0, f64::max);
    base.max((widest + 1.5).ceil())
}

/// The deck's width in whole points.
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

/// Lays columns out left to right, each after a gap.
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
    /// Every strip spans ten minutes: cells coarsen instead (§5.5).
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

    /// Where the cells and their gaps end: the next column's room starts here.
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

    /// Moves the whole table right by `dx`.
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

    /// A very wide window (§3.3): Details has `extra` pt more than its
    /// greatest.
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

/// The `kind` columns at `w`, and whether they fit it.
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

/// A `kind` block at `x`, `b` wide, and whether it fits.
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

/// The size of a Cores row's captions and of the count (§3.4).
const CORES_CAPTION_PT: f64 = 9.5;

/// The cores block's square sizes, `(pitch, square)`, largest first (§3.4).
const CORES_BLOCK_SIZES: [(f64, f64); 5] =
    [(11.0, 9.0), (9.0, 7.0), (7.0, 5.5), (5.5, 4.0), (2.5, 2.0)];

/// The cores' words before the OS's topology is read, longest first. The
/// cluster list is empty then, and unknown is never zero (§5.1), so no form
/// says "0 cores".
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
    /// The block's squares at `(pitch, sq)`, as many to a row as its 68 pt
    /// hold.
    fn block((pitch, sq): (f64, f64)) -> Self {
        Self::Block {
            pitch,
            sq,
            per_row: (CORES_BLOCK_W / pitch).floor() as u32,
        }
    }
}

/// How many cores the clusters count: 0 before the topology is read.
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

/// The count's shortest words: the total ("128 cores"), or "not read".
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
    /// The order the search assigns them in, most important first.
    const SEARCH: [Self; 3] = [Self::Over, Self::Ramp, Self::NotRead];
    /// The order they draw in within one span.
    const DRAW: [Self; 3] = [Self::Ramp, Self::Over, Self::NotRead];

    /// What placing it is worth.
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

/// Complete layout plan for the header line and its legend key.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HeaderPlan {
    pub(crate) labels: Vec<HeaderLabel>,
    pub(crate) key: KeyPlan,
}

/// A header line being laid out: its labels, and the spans they leave free.
struct Header<'m> {
    m: &'m FaceMetrics,
    labels: Vec<HeaderLabel>,
    spans: Vec<KeySpan>,
}

impl<'m> Header<'m> {
    const fn new(m: &'m FaceMetrics) -> Self {
        Self {
            m,
            labels: Vec::new(),
            spans: Vec::new(),
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

    /// The time label over `strip`; the rest of the strip's header is a span.
    fn time(&mut self, strip: Strip) {
        self.label(Label::Time, strip.x, TIME_LABEL);
        self.span(
            strip.x + self.words(TIME_LABEL) + KEY_GAP,
            strip.end(),
            true,
        );
    }

    /// A scope's swatch and words at `x`; answers their width.
    fn scope(&mut self, bar: Bar, x: f64, words: String) -> f64 {
        let w = SWATCH_ADVANCE + self.words(&words);
        self.label(Label::Scope(bar), x, words);
        w
    }

    /// "■ Global ■ aterm ■ Tab 2" from `x`, 8 pt apart, none past `limit`
    /// (the tab's label cut to fit); answers where the run and its trailing
    /// gap end.
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

    fn finish(self) -> HeaderPlan {
        HeaderPlan {
            key: place_key(self.spans, self.m),
            labels: self.labels,
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
                        block.strip.end() - block.name.x - MARK_ADVANCE,
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
                head.span(b.x, b.end(), true);
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

    /// A device pixel: `P(v) = floor(v·s + 0.5)`.
    fn px(v: f64, s: f64) -> f64 {
        (v * s + 0.5).floor()
    }

    fn strip_col(strip: Strip) -> Col {
        Col::new(strip.x, strip.end() - strip.x)
    }

    /// A Full plan's columns, left to right.
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

    /// A Folded block's columns, left to right.
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

    /// Columns that keep their order, none overlapping the next, all inside
    /// `[x0, x1]`.
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

    /// Every edge lands on a whole device pixel at 1× and at 2×.
    fn assert_on_pixels(cols: &[Col], what: &str) {
        for s in [1.0, 2.0] {
            for edge in cols.iter().flat_map(|col| [col.x, col.end()]) {
                assert_eq!(edge * s, px(edge, s), "{what}: edge {edge} at {s}×");
            }
        }
    }

    /// T-G1, Full (§3.2, §3.3) on Helvetica Neue, at 1× and 2×. The plan
    /// reads the width in points (`W · s` px at either scale): every edge
    /// lands on a device pixel at both, and an odd px count at 2× plans as
    /// the whole point under it.
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

    /// T-G1, Folded (§4.1) on Helvetica Neue.
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

    /// §3.1 and §4.1's vertical tables, in device px at 2× and 1×.
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

    /// T-G1's breakpoints: Helvetica Neue's are the spec's; the test stack's
    /// wider face (DejaVu Sans Mono) widens the name columns and so starts
    /// every kind a few points later.
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

    /// §3.2, §3.3, §4.1 and §5.5 at every width from 160 to 5120 pt, on both
    /// faces: the columns keep their order inside the card (inside their
    /// block), each strip spans ten minutes in the kind's cells, and no
    /// deck draws more than 420 cells.
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

    /// T-G5: the form is a function of the window and the setting, with a
    /// 20 pt band of hysteresis, and Full adds whole cells.
    #[test]
    fn deck_height_is_a_function_of_the_window() {
        use crate::stats_deck::{StatsBars, deck_px};
        use DeckForm::{Folded, Full};
        let _: fn(f64, DeckForm, DeckSizePref) -> DeckForm = deck_form_for;
        let _: fn(f64, f64, f64) -> f64 = avail_pt;
        let _: fn(StatsBars, DeckForm, f64, usize) -> usize = deck_px;

        let auto = DeckSizePref::Auto;
        let avails = [849.0, 850.0, 869.0, 870.0];
        assert_eq!(
            avails.map(|a| deck_form_for(a, Folded, auto)),
            [Folded, Folded, Folded, Full]
        );
        assert_eq!(
            avails.map(|a| deck_form_for(a, Full, auto)),
            [Folded, Full, Full, Full]
        );
        for prev in [Folded, Full] {
            for avail in [0.0, 849.0, 894.0, 4000.0] {
                assert_eq!(deck_form_for(avail, prev, DeckSizePref::Full), Full);
                assert_eq!(deck_form_for(avail, prev, DeckSizePref::Folded), Folded);
            }
        }

        let owner = avail_pt(1992.0, 80.0, 2.0);
        assert_eq!(owner, 894.0);
        assert_eq!(deck_form_for(owner, Folded, auto), Full);

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

    /// Every machine T-G3 asks about: 4 to 128 cores in one unnamed cluster
    /// and in one to four named ones, split as evenly as the count allows.
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

    /// A Cores row that fits its room, in one of its modes or by the count,
    /// and never loses the count.
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

    /// T-G3: every machine's cores fit the Full block, the Full Cores row and
    /// the Folded Cores row at the narrowest width of every kind, in one of
    /// their modes, and the count is always there.
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

    /// The modes each width chooses for a familiar machine: 18 cores as 6
    /// Super and 12 Performance.
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

    /// §3.6's results on Helvetica Neue.
    #[test]
    fn the_key_goes_where_the_spec_says() {
        let m = hn();
        let time_w = m.width(TIME_LABEL, HEADER_PT, false);

        let plan = plan_full(1119.0, &m);
        let head = header_full(&plan, TEXT, &m);
        let after_time = plan.strip.x + time_w + KEY_GAP;
        assert_eq!(placed(&head, KeyItem::Ramp), Some(after_time));
        let ramp_end = after_time + KeyItem::Ramp.width(&m) + KEY_GAP;
        assert_eq!(placed(&head, KeyItem::Over), Some(ramp_end));
        assert!(
            placed(&head, KeyItem::NotRead).is_some_and(|x| x > ramp_end && x < plan.strip.end())
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
            placed(&head, KeyItem::Over),
            Some(a.strip.x + time_w + KEY_GAP)
        );
        assert_eq!(placed(&head, KeyItem::Ramp), Some(b.strip.x));
        let ramp_end = b.strip.x + KeyItem::Ramp.width(&m) + KEY_GAP;
        assert_eq!(placed(&head, KeyItem::NotRead), Some(ramp_end));
    }

    /// fslim names its block's worst state in the header (§4.4): over the
    /// name when it fits there, else spilling over the strip, which then
    /// lends the time label to the other block.
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

    /// T-G14: over every plan of the T-G4 sweep (W 160 to 5120 pt in steps of
    /// a device pixel at 2×, which covers 1× too; both faces; both forms),
    /// "over max" is drawn exactly when some free span holds it, which is
    /// always but in fslim with a state in both blocks; every item sits
    /// inside a span; the time label is drawn once.
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

    /// Rule F, as the header uses it.
    #[test]
    fn rule_f_takes_the_first_form_that_fits_else_cuts_the_last() {
        let m = hn();
        let wide = m.width("Tab 2 · this tab", HEADER_PT, false);
        assert_eq!(
            m.fit(&["Tab 2 · this tab", "Tab 2"], wide, HEADER_PT, false),
            "Tab 2 · this tab"
        );
        assert_eq!(
            m.fit(&["Tab 2 · this tab", "Tab 2"], wide - 1.0, HEADER_PT, false),
            "Tab 2"
        );
        let cut = m.fit(
            &["a forty character tab label, cut to fit"],
            60.0,
            HEADER_PT,
            false,
        );
        assert!(cut.ends_with('…') && m.width(&cut, HEADER_PT, false) <= 60.0);
        assert!(!cut.contains(" …"));
        assert_eq!(m.fit(&["Performance"], 1.0, HEADER_PT, false), "");
    }

    /// Prints [`HELVETICA_NEUE`] from the face the macOS chrome draws in and
    /// checks the embedded table against it: run it by name with
    /// `--ignored --nocapture` after a macOS font update, and paste the
    /// array it prints over the table.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "reads the system Helvetica Neue to print the embedded table"]
    fn print_the_helvetica_neue_table() {
        use crate::widget::TextFace;
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let regular = crate::tray_raster::ui_font_of(TextFace::Ui).expect("the chrome's UI face");
        let bold = crate::tray_raster::ui_font_of(TextFace::UiBold).expect("the chrome's bold");
        let upem = f64::from((1.0 / regular.scale_factor(1.0)).round());
        assert_eq!(upem, HELVETICA_NEUE_UPEM);
        let units = |font: &Font, ch: char| {
            assert!(font.maps(ch), "Helvetica Neue maps {ch:?}");
            (em_advance(font, ch) * upem).round() as u16
        };
        let rows: Vec<(char, u16, u16)> = deck_glyphs()
            .map(|ch| (ch, units(&regular, ch), units(&bold, ch)))
            .collect();
        println!(
            "const HELVETICA_NEUE: [(char, u16, u16); {}] = [",
            rows.len()
        );
        for (ch, regular, bold) in &rows {
            println!("    ({ch:?}, {regular}, {bold}),");
        }
        println!("];");
        assert_eq!(
            rows, HELVETICA_NEUE,
            "paste the printed table over HELVETICA_NEUE"
        );
    }

    /// The deck measures its words as the pen draws them: with this host's
    /// UI faces installed, a width is the pen's own, its kerning and
    /// tracking in it, so the Details separator after a clause sits where it
    /// was planned whatever the clause's length (Disk's dot drew 13 px after
    /// its clause and GPU's 9 when the measure left them out).
    #[test]
    fn the_deck_measures_its_words_as_the_pen_draws_them() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let m = crate::stats_deck::deck_metrics();
        for s in [
            "Tokens",
            "learning peak",
            "low, 41 GB free",
            "aterm freed 41 GB today",
            "AVATAR To We",
        ] {
            for (face, bold) in [
                (crate::widget::TextFace::Ui, false),
                (crate::widget::TextFace::UiBold, true),
            ] {
                let pen = f64::from(crate::tray_raster::ui_text_width_for(face, s, 19.0));
                let planned = m.width(s, 19.0, bold);
                assert!(
                    (pen - planned).abs() < 0.01,
                    "{s:?} bold {bold}: the pen draws {pen}, the deck plans {planned}"
                );
            }
        }
    }
}
