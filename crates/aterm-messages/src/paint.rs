// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Band row painter mapping [`Presentation`] and [`BandMotion`] frames onto cells and pixel tones.

use crate::animate::{Anim, BandMotion, FineTone, ROW, RowMotion, Surface, Tone};
use crate::center::EchoKind;
use crate::glass::{CapsuleLayout, CapsuleRole, Presentation, RowKind, RowLayout};
use crate::model::{ActionIndex, Severity};
use crate::{FAILED_WORD, STALLED_WORD};

/// Window pixel geometry for mapping a band meter across the window width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Geometry {
    /// Full window width in pixels.
    pub win_w: usize,
    /// Window x-coordinate of column 0's left edge.
    pub cells_x: usize,
    /// Width of one cell in pixels.
    pub cell_w: usize,
}

impl Geometry {
    /// Geometry with unit cells and no gutters.
    #[cfg(test)]
    #[inline]
    #[must_use]
    pub const fn cells_only(cols: usize) -> Self {
        Self {
            win_w: cols,
            cells_x: 0,
            cell_w: 1,
        }
    }

    /// Cell column under window pixel `x`, saturating at the row ends.
    #[inline]
    #[must_use]
    pub const fn cell_at(self, x: usize) -> usize {
        let cw = if self.cell_w > 1 { self.cell_w } else { 1 };
        x.saturating_sub(self.cells_x) / cw
    }
}

/// Window-pixel mapping for a `cols`-wide metered row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeterSpan {
    /// Window geometry.
    pub geom: Geometry,
    /// Row width in columns.
    pub cols: usize,
}

impl MeterSpan {
    /// Window-pixel span `[x0, x1)` for column `x` (including adjacent gutters at the ends).
    #[inline]
    #[must_use]
    pub(crate) fn px(self, x: usize) -> (u64, u64) {
        let g = self.geom;
        let cw = g.cell_w.max(1);
        let x0 = if x == 0 { 0 } else { g.cells_x + x * cw };
        let x1 = if x + 1 >= self.cols {
            g.win_w.max(g.cells_x + self.cols * cw)
        } else {
            g.cells_x + (x + 1) * cw
        };
        (x0 as u64, x1 as u64)
    }

    /// Total window width in pixels.
    #[inline]
    #[must_use]
    pub(crate) fn win_w(self) -> u64 {
        let g = self.geom;
        g.win_w.max(g.cells_x + self.cols * g.cell_w.max(1)) as u64
    }

    /// Byte-quantized [`Tone`] for column `x` on `surface`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn tone(self, surface: &Surface, x: usize) -> Tone {
        let (x0, x1) = self.px(x);
        surface.span(x0, x1, self.win_w())
    }

    /// Sub-step [`FineTone`] (1/256 step) for column `x` on `surface`.
    #[inline]
    #[must_use]
    pub fn fine(self, surface: &Surface, x: usize) -> FineTone {
        let (x0, x1) = self.px(x);
        surface.span_fine(x0, x1, self.win_w())
    }
}

/// Width in pixels of a pastel fill's darker edge line for cell width `cell_w`:
/// an eighth of the cell, rounded half up, and at least one pixel.
#[inline]
#[must_use]
pub(crate) const fn edge_line_px(cell_w: usize) -> usize {
    // `(cell_w + 4) / 8` without the sum, which passes `usize::MAX` for the widest cells.
    let w = if cell_w % 8 >= 4 {
        cell_w / 8 + 1
    } else {
        cell_w / 8
    };
    if w > 1 { w } else { 1 }
}

/// Hovered element within a band row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HoverTarget {
    /// Row body (opens Details).
    Body,
    /// Action capsule at the given index.
    Capsule(ActionIndex),
}

/// Per-window band hover state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hover {
    /// Band row index (0 = topmost).
    pub row: u8,
    /// Hovered target within the row.
    pub target: HoverTarget,
}

/// Cache key for painted band rows.
pub type BandKey = (
    u64,
    usize,
    crate::ink::BandInks,
    bool,
    Option<Hover>,
    Geometry,
    u64,
);

/// Drawn vector icon replacing the glyph cell's font character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    /// `ℹ`.
    Info,
    /// `✓` (and `✔`).
    Success,
    /// `⚠`.
    Warn,
    /// `✕` (and `✖`, `✗`).
    Error,
    /// `⇣` (and `↓`).
    Download,
    /// `↻`.
    Update,
    /// `↑`.
    Upload,
    /// `⏸`.
    Pause,
    /// `✦`.
    Sparkle,
    /// `!`.
    Alert,
    /// `·`.
    Dot,
    /// `…`.
    More,
    /// `⊖`.
    Remove,
}

impl Icon {
    /// Drawn [`Icon`] corresponding to band glyph `ch`, if any.
    #[inline]
    #[must_use]
    pub const fn for_char(ch: char) -> Option<Self> {
        Some(match ch {
            '\u{2139}' => Self::Info,
            '\u{2713}' | '\u{2714}' => Self::Success,
            '\u{26a0}' => Self::Warn,
            '\u{2715}' | '\u{2716}' | '\u{2717}' => Self::Error,
            '\u{21e3}' | '\u{2193}' => Self::Download,
            '\u{21bb}' => Self::Update,
            '\u{2191}' => Self::Upload,
            '\u{23f8}' => Self::Pause,
            '\u{2726}' => Self::Sparkle,
            '!' => Self::Alert,
            '\u{00b7}' => Self::Dot,
            '\u{2026}' => Self::More,
            '\u{2296}' => Self::Remove,
            _ => return None,
        })
    }
}

/// Semantic word-ink slot before contrast flooring.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ink {
    /// The band's quiet ink: an excerpt, stats, elapsed words.
    Label,
    /// The band's words: a title, a remaining time.
    Value,
    /// A warning's words and glyph, a stall, a Fault's `failed`.
    Warn,
    /// An error's words and glyph.
    Error,
    /// The glyph of a Success or Info row (the cursor accent).
    Accent,
}

/// Visual style of a capsule chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChipForm {
    /// Its own fill: the band, the track or the accent.
    Solid,
    /// A Primary on a metered row: an accent ring with the band's ground inside, its
    /// label in the accent.
    Outlined,
    /// Under an OS-forced palette: `[label]` in system inks.
    Forced,
}

/// Capsule cell face parameters resolved by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChipFace {
    /// The chip's role.
    pub role: CapsuleRole,
    /// Under the pointer.
    pub lit: bool,
    /// On a row with a surface.
    pub metered: bool,
    /// How it is drawn.
    pub form: ChipForm,
}

/// Cell ink and background source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Face {
    /// The band's label ink on the ground under the cell, never floored:
    /// a cell nothing wrote.
    Blank,
    /// A word in an ink slot on the ground under the cell — floored by the
    /// row's ink plan on a metered row, raw on a plain one.
    Word(Ink),
    /// A chip's own face (its ink and fill, whatever the ground).
    Chip(ChipFace),
}

/// One painted band cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cell {
    /// The character (one per cell: the band's measure).
    pub ch: char,
    /// What its ink and ground resolve from.
    pub face: Face,
    /// Bold.
    pub bold: bool,
    /// Text presentation (the glyph cell: `⚠` and `✓` have emoji forms).
    pub text_presentation: bool,
}

impl Cell {
    const BLANK: Cell = Cell {
        ch: ' ',
        face: Face::Blank,
        bold: false,
        text_presentation: false,
    };
}

/// Meter fill palette slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Fill {
    /// The meter ink (the cursor accent).
    Meter,
    /// The warn ink.
    Warn,
}

/// Surface kind used by the row's word-ink contrast plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RowSurface {
    /// No surface: the band's own inks.
    Plain,
    /// A busy row's comet (not under a forced palette): every word floored
    /// once against the hottest tone the comet reaches and its track.
    Busy {
        /// The still look's unlit track: no comet to floor against.
        still: bool,
    },
    /// A determinate row (or any metered row under a forced palette): the
    /// crisp ink over the fill, a floored role ink over the track.
    Bar {
        /// The fill is the stall's dim slate ([`Tone::STALLED`]).
        stalled: bool,
    },
}

/// Per-column mean [`FineTone`] and fill-side flag on a metered row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Column {
    /// The mean tone over the column's window pixels.
    pub tone: FineTone,
    /// On the fill's side; always `false` on a busy row off a forced palette (its comet
    /// keeps its words' side).
    pub on_fill: bool,
}

/// Cell crossed by the meter fill edge and its split pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Split {
    /// The cell.
    pub col: u16,
    /// The window pixel the ink changes at.
    pub x: u32,
    /// The window pixel whose ground the fill side's ink sits on.
    pub under_px: usize,
}

/// Window pixel range and per-pixel coverage for a pastel fill's edge line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EdgeLine {
    /// First window pixel.
    pub first: u32,
    /// One past the last.
    pub end: u32,
    /// `(pixel, share)` for every pixel the line touches.
    pub coverage: Vec<(usize, f64)>,
}

/// Painted band row at one motion frame prior to palette resolution.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts of one row the resolver reads — forced palette, busy, drawn to the pixel — each set once by the painter"
)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowPaint<'a> {
    /// Exactly the presentation's `cols` cells.
    pub cells: Vec<Cell>,
    /// The fill's ink.
    pub fill: Fill,
    /// The ink plan's kind.
    pub kind: RowSurface,
    /// An OS-forced palette (High Contrast) was up.
    pub forced: bool,
    /// Work with no fraction: the row's surface is a comet track.
    pub busy: bool,
    /// The window mapping.
    pub span: MeterSpan,
    /// The frame's surface.
    pub(crate) surface: &'a Surface,
    /// Each column's tone and side; `None` on a plain row.
    pub columns: Option<Vec<Column>>,
    /// The ground is drawn pixel by pixel (a graded surface).
    pub raster_ground: bool,
    /// The fill's edge in window pixels, on a drawn-to-the-pixel bar.
    pub edge_px: Option<u64>,
    /// The cell the edge splits — the glyph's, where the edge crosses its
    /// spilling icon — never over a chip that keeps its own ground or ring,
    /// and only on a bar.
    pub split: Option<Split>,
    /// Cell spans `[start, end)` whose chip keeps its own fill.
    pub owned: Vec<(u16, u16)>,
    /// Outlined chips: cell spans and whether each is lit.
    pub rings: Vec<(u16, u16, bool)>,
    /// The glyph cell's drawn icon.
    pub icon: Option<(u16, Icon)>,
    /// An echo's fade: 0 opaque … 255 gone.
    pub fade: u8,
}

impl RowPaint<'_> {
    /// Per-window-pixel `(FineTone, on_fill)` iterator across the row.
    pub(crate) fn pixels(&self) -> impl Iterator<Item = (FineTone, bool)> + '_ {
        let w = self.span.win_w();
        (0..w).map(move |xw| {
            let t = self.surface.span_fine(xw, xw + 1, w);
            (t, u32::from(t.fill) + u32::from(t.warn) >= 128 << 8)
        })
    }

    /// Pastel edge line pixel coverage when a pixel-drawn bar ends inside the row;
    /// `None` too for an edge whose position in 1/[`ROW`] pixels passes `u64` (a
    /// product that takes a window over 2^32 pixels wide), which no line is placed at.
    #[must_use]
    pub(crate) fn edge_line(&self) -> Option<EdgeLine> {
        let q = self.surface.edge?;
        if !self.raster_ground || self.busy || self.forced {
            return None;
        }
        let unit = u64::from(ROW);
        let win_w = self.span.win_w();
        let share = |v: u64| f64::from(u32::try_from(v).unwrap_or(u32::MAX)) / f64::from(ROW);
        let exact = u64::from(q).checked_mul(win_w)?;
        // A width past `u64` is wider than `exact`, so `lo` is 0 either way.
        let width = u64::try_from(edge_line_px(self.span.geom.cell_w))
            .unwrap_or(1)
            .saturating_mul(unit);
        let (lo, hi) = (exact.saturating_sub(width), exact);
        if hi <= lo {
            return None;
        }
        let first = lo / unit;
        let end = hi.div_ceil(unit).min(win_w);
        let mut coverage = Vec::new();
        for p in first..end {
            // `p < hi.div_ceil(unit)`, so `p * unit < hi`; `(p + 1) * unit` passes `u64` only
            // above `hi`, and both `min`s below read it as `hi`.
            let (p0, p1) = (p.saturating_mul(unit), (p + 1).saturating_mul(unit));
            let line = p1.min(hi).saturating_sub(p0.max(lo));
            let filled = p1.min(exact).saturating_sub(p0);
            if line == 0 || filled == 0 {
                continue;
            }
            let t = self.surface.span_fine(p, p + 1, win_w);
            let strength = (f64::from(t.fill) / (255.0 * 256.0) / share(filled)).clamp(0.0, 1.0);
            if let Ok(px) = usize::try_from(p) {
                coverage.push((px, share(line) * strength));
            }
        }
        Some(EdgeLine {
            first: u32::try_from(first).unwrap_or(u32::MAX),
            end: u32::try_from(end).unwrap_or(u32::MAX),
            coverage,
        })
    }
}

/// The frame a row with no motion row of its own reads.
static STILL: RowMotion = RowMotion {
    surface: Surface {
        stops: Vec::new(),
        flat: false,
        edge: None,
    },
    readout: None,
    eta: None,
    fade: 0,
    glyph: None,
    anim: Anim::None,
};

/// Paint all committed rows of `p` at `motion` for `geom` and `hover`.
#[must_use]
pub(crate) fn paint<'a>(
    p: &Presentation,
    hover: Option<Hover>,
    geom: Geometry,
    motion: &'a BandMotion,
    forced: bool,
) -> Vec<RowPaint<'a>> {
    p.rows
        .iter()
        .enumerate()
        .map(|(i, layout)| {
            let lit = hover.filter(|h| usize::from(h.row) == i).map(|h| h.target);
            let rm = motion.rows.get(i).unwrap_or(&STILL);
            paint_row(p.cols, layout, rm, forced, lit, geom)
        })
        .collect()
}

/// The ink a time slot's words wear: a stall, and a Fault echo's `failed`, warn; a
/// remaining time (it always ends in `left`) the value; how long work has run the label.
fn slot_ink(words: &str) -> Ink {
    if words == STALLED_WORD || words == FAILED_WORD {
        Ink::Warn
    } else if words.ends_with(" left") {
        Ink::Value
    } else {
        Ink::Label
    }
}

/// A row's cells as they are written.
struct Cells {
    cells: Vec<Cell>,
}

impl Cells {
    /// `s` from `col`, one char per cell, clipped at the row's end.
    fn write(&mut self, col: usize, s: &str, face: Face, bold: bool) {
        let cols = self.cells.len();
        for (k, ch) in s.chars().enumerate() {
            let x = col + k;
            if x >= cols {
                break;
            }
            self.cells[x] = Cell {
                ch,
                face,
                bold,
                text_presentation: false,
            };
        }
    }

    /// One cell, when it is on the row.
    fn put(&mut self, x: usize, ch: char, face: Face) {
        if let Some(cell) = self.cells.get_mut(x) {
            *cell = Cell {
                ch,
                face,
                bold: false,
                text_presentation: false,
            };
        }
    }

    /// Words in a `width`-cell slot from `col`, left-aligned, the rest of the
    /// slot cleared in the same ink — a time slot's words change length from
    /// frame to frame, and the cells they leave must not keep old glyphs.
    fn slot(&mut self, (col, width): (usize, usize), words: &str) {
        let face = Face::Word(slot_ink(words));
        let mut chars = words.chars();
        let end = (col + width).min(self.cells.len()).max(col);
        for cell in self.cells.iter_mut().take(end).skip(col) {
            let ch = chars.next().unwrap_or(' ');
            *cell = Cell {
                ch,
                face,
                bold: false,
                text_presentation: false,
            };
        }
    }
}

/// What a painted chip asks of the row's pixel raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chip {
    /// No fill of its own: it rides the surface under it like a word.
    Rides,
    /// Its own fill on a metered row: the raster leaves its cells alone.
    Owns,
    /// Outlined: the raster's ground runs under it and a ring is drawn over that.
    Rings,
}

/// What a chip's paint reads of its row.
#[derive(Clone, Copy)]
struct ChipRow {
    forced: bool,
    metered: bool,
    /// Outlined chips are possible: the row is drawn to the pixel.
    outline: bool,
}

/// One chip over its `width` cells from `col`: the pad cells carry the fill (or, forced,
/// the brackets), the text sits between them.
fn capsule(cells: &mut Cells, cap: &CapsuleLayout, lit: bool, row: ChipRow) -> Chip {
    if cap.width < 2 {
        return Chip::Rides;
    }
    let last = cap.col + cap.width - 1;
    let (open, close) = if row.forced { ('[', ']') } else { (' ', ' ') };
    let rides = !lit
        && (row.forced
            || cap.role == CapsuleRole::Details
            || (row.metered && cap.role == CapsuleRole::Secondary));
    if rides && row.metered {
        let ink = if row.forced || cap.role == CapsuleRole::Secondary {
            Ink::Value
        } else {
            Ink::Label
        };
        let bold = row.forced && cap.role == CapsuleRole::Primary;
        cells.put(cap.col, open, Face::Word(ink));
        cells.write(cap.col + 1, &cap.text, Face::Word(ink), bold);
        cells.put(last, close, Face::Word(ink));
        return Chip::Rides;
    }
    let ringed = !row.forced && row.metered && cap.role == CapsuleRole::Primary;
    let form = if row.forced {
        ChipForm::Forced
    } else if ringed {
        ChipForm::Outlined
    } else {
        ChipForm::Solid
    };
    let face = Face::Chip(ChipFace {
        role: cap.role,
        lit,
        metered: row.metered,
        form,
    });
    cells.put(cap.col, open, face);
    cells.write(
        cap.col + 1,
        &cap.text,
        face,
        cap.role == CapsuleRole::Primary,
    );
    cells.put(last, close, face);
    match (ringed && row.outline, row.metered) {
        (true, _) => Chip::Rings,
        (false, true) => Chip::Owns,
        (false, false) => Chip::Rides,
    }
}

/// One row at one motion frame.
#[allow(
    clippy::too_many_lines,
    reason = "one row's paint reads its layout, its frame and its window and writes its surface, its words and its chips in one order"
)]
fn paint_row<'a>(
    cols: usize,
    l: &RowLayout,
    rm: &'a RowMotion,
    forced: bool,
    hover: Option<HoverTarget>,
    geom: Geometry,
) -> RowPaint<'a> {
    let overflow = matches!(l.kind, RowKind::Overflow { .. });
    let alarm = matches!(l.severity, Severity::Warn | Severity::Error);
    let (word, accent) = if overflow && hover.is_some() {
        (Ink::Value, Ink::Value)
    } else if overflow {
        (Ink::Label, Ink::Label)
    } else if l.severity == Severity::Error {
        (Ink::Error, Ink::Error)
    } else if alarm {
        (Ink::Warn, Ink::Warn)
    } else if forced {
        (Ink::Value, Ink::Value)
    } else {
        (Ink::Value, Ink::Accent)
    };
    let surface = &rm.surface;
    let fill = if forced || !alarm {
        Fill::Meter
    } else {
        Fill::Warn
    };
    let busy = l.track.is_some();
    let comet_side = busy && !forced;
    let span = MeterSpan { geom, cols };
    let win_w = span.win_w();
    let raster_ground = !surface.is_empty() && !surface.flat && cols > 0;
    let edge_px = surface
        .edge
        .filter(|_| raster_ground && !busy)
        .map(|e| u64::from(e) * win_w / u64::from(ROW));
    let split_col = edge_px.and_then(|e| {
        (0..cols).find(|&x| {
            let (x0, x1) = span.px(x);
            x0 < e && e < x1
        })
    });
    let (gcol, gch) = l.glyph;
    let icon_split = edge_px
        .filter(|&e| {
            gcol > 0
                && gcol + 1 < cols
                && Icon::for_char(rm.glyph.unwrap_or(gch)).is_some()
                && span.px(gcol - 1).0 < e
                && e < span.px(gcol + 1).1
        })
        .map(|_| gcol);
    let columns: Option<Vec<Column>> = (!surface.is_empty() && cols > 0).then(|| {
        (0..cols)
            .map(|x| {
                let tone = span.fine(surface, x);
                let on = match (split_col, edge_px) {
                    _ if busy || forced => tone.fill >= 128 << 8,
                    _ if icon_split == Some(x) => false,
                    (Some(s), _) => x < s,
                    (None, Some(e)) => span.px(x).1 <= e,
                    (None, None) => tone.fill >= 128 << 8,
                };
                Column {
                    tone,
                    on_fill: on && !comet_side,
                }
            })
            .collect()
    });
    let kind = if surface.is_empty() {
        RowSurface::Plain
    } else if comet_side {
        RowSurface::Busy {
            still: matches!(rm.anim, Anim::Track),
        }
    } else {
        RowSurface::Bar {
            stalled: surface.stops.iter().any(|s| s.tone == Tone::STALLED),
        }
    };
    let mut cells = Cells {
        cells: vec![Cell::BLANK; cols],
    };
    let fault = matches!(
        rm.anim,
        Anim::Echo {
            kind: EchoKind::Fault,
            ..
        }
    );
    let mut icon = None;
    if gcol < cols {
        let glyph = rm.glyph.unwrap_or(gch);
        icon = Icon::for_char(glyph);
        cells.cells[gcol] = Cell {
            ch: glyph,
            face: Face::Word(if fault { Ink::Warn } else { accent }),
            bold: !overflow,
            text_presentation: true,
        };
    }
    let (tcol, title) = &l.title;
    cells.write(*tcol, title, Face::Word(word), !overflow);
    if let Some((dcol, d)) = &l.detail {
        cells.write(
            dcol.saturating_sub(2),
            "\u{00b7}",
            Face::Word(Ink::Label),
            false,
        );
        cells.write(*dcol, d, Face::Word(Ink::Label), false);
    }
    if let Some((pcol, pct)) = &l.pct {
        cells.write(*pcol, pct, Face::Word(word), false);
    }
    if let Some(col) = l.elapsed {
        cells.slot(
            (col, l.elapsed_width()),
            rm.readout.as_deref().unwrap_or(""),
        );
    }
    if let Some(col) = l.eta {
        cells.slot((col, l.eta_width()), rm.eta.as_deref().unwrap_or(""));
    }
    if let Some((scol, s)) = &l.stats {
        cells.write(*scol, s, Face::Word(Ink::Label), false);
    }
    if let Some((lcol, words)) = l.load {
        cells.write(lcol, words, Face::Word(Ink::Label), false);
    }
    let mut owned: Vec<(u16, u16)> = Vec::new();
    let mut rings: Vec<(u16, u16, bool)> = Vec::new();
    let chip_row = ChipRow {
        forced,
        metered: columns.is_some(),
        outline: raster_ground && !forced,
    };
    for cap in &l.capsules {
        let lit = match hover {
            Some(HoverTarget::Capsule(k)) => cap.action == k,
            Some(HoverTarget::Body) => cap.action.is_details(),
            None => false,
        };
        let cells_of = (
            u16::try_from(cap.col).unwrap_or(u16::MAX),
            u16::try_from((cap.col + cap.width).min(cols)).unwrap_or(u16::MAX),
        );
        match capsule(&mut cells, cap, lit, chip_row) {
            Chip::Rides => {}
            Chip::Owns => owned.push(cells_of),
            Chip::Rings => rings.push((cells_of.0, cells_of.1, lit)),
        }
    }
    let within = |x: usize, (a, b): (u16, u16)| (usize::from(a)..usize::from(b)).contains(&x);
    let split = match (icon_split.or(split_col), edge_px, kind) {
        (Some(col), Some(e), RowSurface::Bar { .. })
            if !owned.iter().any(|&s| within(col, s))
                && !rings.iter().any(|&(a, b, _)| within(col, (a, b))) =>
        {
            let exact = surface.edge.map_or(0, |q| u64::from(q) * win_w);
            let covered = exact % u64::from(ROW) * 2 >= u64::from(ROW);
            Some(Split {
                col: u16::try_from(col).unwrap_or(u16::MAX),
                x: u32::try_from(e + u64::from(covered)).unwrap_or(u32::MAX),
                under_px: usize::try_from(e.saturating_sub(1)).unwrap_or(0),
            })
        }
        _ => None,
    };
    RowPaint {
        cells: cells.cells,
        fill,
        kind,
        forced,
        busy,
        span,
        surface,
        columns,
        raster_ground,
        edge_px,
        split,
        owned,
        rings,
        icon: icon.zip(u16::try_from(gcol).ok()).map(|(i, c)| (c, i)),
        fade: rm.fade,
    }
}

#[cfg(test)]
#[path = "paint_tests.rs"]
mod tests;
