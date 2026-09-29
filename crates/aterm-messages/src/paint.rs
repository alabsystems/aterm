// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BAND'S PAINTER — its STRUCTURE (design ruling 319; the owner's
//! architecture ask, ruling 140: *"design the logic in aterm core and then
//! keep the osx layer lightweight so that we can make this cross platform"*).
//!
//! `paint` turns one [`Presentation`] under one [`BandMotion`] frame into
//! one `RowPaint` per band row: which character goes in which cell, in
//! which word-ink SLOT (`Ink`) or chip form (`ChipFace`), bold or not,
//! with which drawn [`Icon`]; which of the row's columns lie on the fill's
//! side, which cell the fill's edge splits, which cells a chip keeps for its
//! own ground or ring; and every surface tone as a [`FineTone`] mapped onto
//! the window's pixels through its [`Geometry`] ([`MeterSpan`], ruling 55 as
//! amended by 138 and 242). Nothing here is a colour: the host RESOLVES each
//! slot and tone against its palette — the contrast floors, the crisp ink,
//! the side holds, the fade, the chip inks — and writes its own cells. So a
//! second host (the web) paints the same band from the same structure.
//!
//! The cut (ruling 319): anything decided by an RGB value stays with the
//! resolver — a glint drawn in the rail because it would not read at full
//! height, the pastel edge line (which needs the fill to BE the palette's
//! meter ink), a Fault wash's side hold, every floor. This module decides
//! nothing gated on a colour.
//!
//! # The grammar
//!
//! Every cell starts `Face::Blank` (the band's label ink on the ground,
//! never floored), then the row is written in one order — glyph, title, the
//! ` · ` joint, excerpt, percent, elapsed and ETA slots, stats, load words,
//! capsules — each write minting a fresh cell (`Cell::text_presentation`
//! false but for the glyph's). A time slot clears its whole width to its
//! ink. The band's measure is one `char` per cell (the host's
//! `settings::write_str` measure), so a `Cell` carries no width. The
//! painter's structure is the crate's own (ruling 328): a host reads the
//! band through `ink::paint_band`, which paints and resolves it, and keeps
//! from this module only the window [`Geometry`] (with [`Geometry::cell_at`]
//! for its hit test), the [`Hover`], the drawn [`Icon`], the [`MeterSpan`]
//! and the band's repaint key ([`band_fp`], [`BandKey`]).

use crate::animate::{Anim, BandMotion, FineTone, ROW, RowMotion, Surface, Tone};
use crate::center::EchoKind;
use crate::glass::{CapsuleLayout, CapsuleRole, Presentation, RowKind, RowLayout};
use crate::model::{ActionIndex, Severity};
use crate::{FAILED_WORD, STALLED_WORD};

/// Where a window's band cells sit on its glass, in device px — what maps a
/// meter's fill onto the WINDOW's width rather than the grid's. (Field order
/// is part of the host's repaint key: it hashes this value.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Geometry {
    /// The window's full width (the swapchain / softbuffer surface), px.
    pub win_w: usize,
    /// Window x of column 0's left edge: the frame's left gutter plus the
    /// leading remainder band.
    pub cells_x: usize,
    /// One cell's width, px.
    pub cell_w: usize,
}

impl Geometry {
    /// Unit cells and no gutters: the fill maps onto the columns alone. The
    /// geometry of a caller that has no window (painter tests).
    #[must_use]
    pub const fn cells_only(cols: usize) -> Self {
        Self {
            win_w: cols,
            cells_x: 0,
            cell_w: 1,
        }
    }

    /// The cell column under pixel `x`, measured in the same space as
    /// [`Geometry::cells_x`] (ruling 328: the host's hit test reads it on the
    /// frame's own lattice). A pixel left of column 0 — the leading gutter —
    /// is column 0, a zero cell width counts as one pixel, and a pixel past
    /// the last column is not clamped: the row's layout knows its width.
    #[must_use]
    pub fn cell_at(self, x: usize) -> usize {
        x.saturating_sub(self.cells_x) / self.cell_w.max(1)
    }
}

/// THE MAPPING (ruling 55, amended by ruling 138): which window pixels a
/// `cols`-wide metered row's column `x` stands for. Column `x` covers its
/// own cell, `[cells_x + x·cell_w, cells_x + (x+1)·cell_w)` — and the FIRST
/// column also the left gutter and leading remainder band before it, the
/// LAST the ones after it, out to the window's edges. So the edge cells'
/// tones ARE the gutters', and the whole window, gutters included, is the
/// meter: 0 % is an empty track, 100 % lights the window edge to edge, and
/// 50 % is its middle — the one cell under the fill's edge takes
/// `mix(track, fill, coverage)` of its span ([`Surface::span`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeterSpan {
    /// The window the row is mapped onto.
    pub geom: Geometry,
    /// The row's width in columns.
    pub cols: usize,
}

impl MeterSpan {
    /// Column `x`'s window-pixel span `[x0, x1)` (see the type doc).
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

    /// The window's width the fractions are mapped onto.
    #[must_use]
    pub(crate) fn win_w(self) -> u64 {
        let g = self.geom;
        g.win_w.max(g.cells_x + self.cols * g.cell_w.max(1)) as u64
    }

    /// Column `x`'s tone on `surface`, in bytes (the tests' reading; the
    /// painter reads [`MeterSpan::fine`]).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn tone(self, surface: &Surface, x: usize) -> Tone {
        let (x0, x1) = self.px(x);
        surface.span(x0, x1, self.win_w())
    }

    /// Column `x`'s tone on `surface` to 1/256 of a step — what a host mixes
    /// from, rounding each cell's colour once (design ruling 157).
    #[must_use]
    pub fn fine(self, surface: &Surface, x: usize) -> FineTone {
        let (x0, x1) = self.px(x);
        surface.span_fine(x0, x1, self.win_w())
    }
}

/// The width of a pastel fill's darker EDGE line on a window whose cells are
/// `cell_w` pixels wide (design ruling 264): about an eighth of a cell — one
/// pixel on a 1x cell, two on a Retina one — never under one.
#[must_use]
pub(crate) fn edge_line_px(cell_w: usize) -> usize {
    ((cell_w + 4) / 8).max(1)
}

/// What the pointer is on within one band row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HoverTarget {
    /// The row body: a press opens Details, so the `Details ›` chip lights.
    Body,
    /// One capsule, by its action.
    Capsule(ActionIndex),
}

/// The per-window hover state a paint reads: which row, and what on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hover {
    /// The band row (0 = topmost), narrow on purpose — the band has three.
    pub row: u8,
    /// Body or capsule.
    pub target: HoverTarget,
}

/// What one view's painted band rows were built from — `(center
/// fingerprint at cols, cols, inks key, hover, window geometry, motion
/// fingerprint)` — the paint cache key of [`crate::drive::View::paint`],
/// which reads the same terms [`band_fp`] folds. The inks key hashes the
/// resolved `BandInks` and the forced-palette flag the rows are painted
/// with (ruling 337: a key on the theme alone missed a High Contrast toggle).
/// The motion term is [`BandMotion::fingerprint`]: 0 with nothing moving or
/// indicating, and moved only by a frame that draws something new (ruling
/// 140).
pub type BandKey = (u64, usize, u64, Option<Hover>, Geometry, u64);

/// The band's repaint term: the center's fingerprint at `cols` ⊕ the
/// window's hover ⊕ the window geometry a full-width meter (or a busy row's
/// comet) is mapped onto (ruling 55: a resize that keeps the column count
/// still moves its lit cells) ⊕ the motion frame's fingerprint
/// ([`BandMotion::fingerprint`], ruling 140). **Exactly `0` with no row
/// committed** (the center's own FL-1 term; the hover is `None` and the
/// motion 0 then by construction, and the geometry is not folded), so an idle
/// key is byte-identical to the no-band path; else nonzero, and moved by a
/// hover change so the lit chip re-presents, and by a motion frame that draws
/// something new — the key and the splice's cache key ([`BandKey`]) read the
/// same motion term (design §3.2). The motion term is folded only when
/// nonzero, so a motion-free band's key does not move with the clock. Moved
/// from the macOS host with its values (ruling 328): [`Geometry`]'s derived
/// `Hash` and the std hasher's fixed keys, so a key is the same number it was.
#[must_use]
pub fn band_fp(center_fp: u64, hover: Option<Hover>, geom: Geometry, motion_fp: u64) -> u64 {
    if center_fp == 0 {
        return 0;
    }
    let center_fp = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        center_fp.hash(&mut h);
        geom.hash(&mut h);
        h.finish()
    };
    let term = match hover {
        None => 0u64,
        Some(Hover { row, target }) => {
            let t = match target {
                HoverTarget::Body => 0x100,
                HoverTarget::Capsule(k) => 0x200 | u64::from(k.0),
            };
            // The presence bit sits BELOW the target (ruling 330): OR-ed into
            // the target itself, it made Capsule(2j) and Capsule(2j+1) on one
            // row the same key, so a hover moving between them drew nothing.
            (u64::from(row) << 16) | (t << 1) | 1
        }
    };
    let fp = center_fp ^ term.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let fp = if motion_fp == 0 {
        fp
    } else {
        fp ^ motion_fp
            .wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            .rotate_left(17)
    };
    fp | 1
}

/// One of the band's DRAWN icons (ruling 251): the renderer draws it in the
/// cell's ink instead of a font's glyph, the character kept in the cell. A
/// mirror of the renderer's own table (`aterm_core::render::BandIcon`, which
/// this crate does not link); the host's parity test holds the two together
/// over every `char`.
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
    /// The icon that stands in for `ch`, the band's glyph; `None` for any
    /// character outside the band's closed set (it keeps its font glyph).
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

/// A word's ink SLOT (ruling 320): which of the band's inks it wears before
/// the host floors it against the ground under it. The choice of slot is
/// the painter's logic (a warning's words, a stall, the overflow link's lit
/// state); the host maps each slot 1:1 onto its palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ink {
    /// The band's quiet ink: an excerpt, stats, elapsed words.
    Label,
    /// The band's words: a title, a remaining time.
    Value,
    /// A warning's words and glyph, a stall, a Fault's `failed`.
    Warn,
    /// An error's words and glyph (ruling 265).
    Error,
    /// The glyph of a Success or Info row (the cursor accent).
    Accent,
}

/// How a chip with its own face is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChipForm {
    /// Its own fill: the band, the track or the accent.
    Solid,
    /// A Primary on a metered row (ruling 249): an accent ring with the
    /// band's ground inside, its label in the accent.
    Outlined,
    /// Under an OS-forced palette: `[label]` in system inks.
    Forced,
}

/// A chip cell's face: what the host resolves to its `(ink, fill)`.
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

/// What a cell's ink and ground resolve from.
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

/// One painted cell.
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

/// Which ink a row's fill wears: the meter's, or `warn` on a Warn/Error row
/// (under an OS-forced palette always the meter's, its HIGHLIGHT).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Fill {
    /// The meter ink (the cursor accent, ruling 137).
    Meter,
    /// The warn ink.
    Warn,
}

/// The row's surface as its words' ink plan reads it (ruling 242).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RowSurface {
    /// No surface: the band's own inks.
    Plain,
    /// A measured LEVEL (ruling 243): the rail under the words, which sit on
    /// the band's own ground.
    Rail,
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

/// One column of a metered row: its span's mean tone (the lift as the
/// surface has it) and whether it lies on the fill's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Column {
    /// The mean tone over the column's window pixels.
    pub tone: FineTone,
    /// On the fill's side — always `false` on a busy row off a forced
    /// palette (its comet keeps its words' side, ruling 152).
    pub on_fill: bool,
}

/// The one cell the fill's edge crosses (ruling 242): its glyph drawn in the
/// fill side's ink left of `x` and its own right of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Split {
    /// The cell.
    pub col: u16,
    /// The window pixel the ink changes at.
    pub x: u32,
    /// The window pixel whose ground the fill side's ink sits on.
    pub under_px: usize,
}

/// A pastel fill's darker EDGE line (ruling 264): the window pixels
/// `[first, end)` it is drawn over, and each pixel's share of it — the line's
/// coverage of the pixel times the fill's own strength there (0–1).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EdgeLine {
    /// First window pixel.
    pub first: u32,
    /// One past the last.
    pub end: u32,
    /// `(pixel, share)` for every pixel the line touches.
    pub coverage: Vec<(usize, f64)>,
}

/// One band row at one motion frame, before any colour.
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
    /// Each column's tone and side; `None` on a plain row and on a rail row.
    pub columns: Option<Vec<Column>>,
    /// The ground is drawn pixel by pixel (a graded surface, not a rail).
    pub raster_ground: bool,
    /// The fill's edge in window pixels, on a drawn-to-the-pixel bar.
    pub edge_px: Option<u64>,
    /// The cell the edge splits — the glyph's, where the edge crosses its
    /// spilling icon — never over a chip that keeps its own ground or ring,
    /// and only on a bar.
    pub split: Option<Split>,
    /// Cell spans `[start, end)` whose chip keeps its own fill.
    pub owned: Vec<(u16, u16)>,
    /// OUTLINED chips: cell spans and whether each is lit.
    pub rings: Vec<(u16, u16, bool)>,
    /// The glyph cell's drawn icon.
    pub icon: Option<(u16, Icon)>,
    /// An echo's fade: 0 opaque … 255 gone.
    pub fade: u8,
}

impl RowPaint<'_> {
    /// Every window pixel's tone, left to right, and whether it lies on the
    /// fill's side (a Fault's wash counts as fill) — the raster's pass.
    pub(crate) fn pixels(&self) -> impl Iterator<Item = (FineTone, bool)> + '_ {
        let w = self.span.win_w();
        (0..w).map(move |xw| {
            let t = self.surface.span_fine(xw, xw + 1, w);
            (t, u32::from(t.fill) + u32::from(t.warn) >= 128 << 8)
        })
    }

    /// The pastel edge line's pixels, where a bar drawn to the pixel ends
    /// inside the row — the host draws it where its fill wears a palette
    /// with an edge ink.
    #[must_use]
    pub(crate) fn edge_line(&self) -> Option<EdgeLine> {
        let q = self.surface.edge?;
        if !self.raster_ground || self.busy || self.forced {
            return None;
        }
        let unit = u64::from(ROW);
        let win_w = self.span.win_w();
        // A share of one pixel, `v` of `unit`.
        let share = |v: u64| f64::from(u32::try_from(v).unwrap_or(u32::MAX)) / f64::from(ROW);
        let exact = u64::from(q) * win_w;
        let width = u64::try_from(edge_line_px(self.span.geom.cell_w)).unwrap_or(1) * unit;
        let (lo, hi) = (exact.saturating_sub(width), exact);
        if hi <= lo {
            return None;
        }
        let first = lo / unit;
        let end = hi.div_ceil(unit).min(win_w);
        let mut coverage = Vec::new();
        for p in first..end {
            let (p0, p1) = (p * unit, (p + 1) * unit);
            let line = p1.min(hi).saturating_sub(p0.max(lo));
            let filled = p1.min(exact).saturating_sub(p0);
            if line == 0 || filled == 0 {
                continue;
            }
            // The fill's strength at this pixel: its coverage over the share
            // of the pixel left of the edge.
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
        rail: false,
    },
    readout: None,
    eta: None,
    fade: 0,
    glyph: None,
    anim: Anim::None,
};

/// Paint every committed row of `p`, top to bottom, at one motion frame:
/// `hover` lights the chip under the pointer (or the body's `Details ›`) on
/// its row, `geom` maps each surface onto the window, and `forced` says an
/// OS-forced palette is up (the host's High Contrast latch, read once per
/// paint — never derived from the look).
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

/// The ink a time slot's words wear (design §10.7): a stall, and a Fault
/// echo's `failed`, warn; a remaining time (it always ends in `left`) the
/// value; how long work has run the label.
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
        for x in col..end {
            let ch = chars.next().unwrap_or(' ');
            self.cells[x] = Cell {
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
    /// OUTLINED (ruling 249): the raster's ground runs under it and a ring
    /// is drawn over that.
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

/// One chip over its `width` cells from `col`: the pad cells carry the fill
/// (or, forced, the brackets), the text sits between them.
///
/// On a METERED row a resting Secondary and every `Details ›` carry no fill
/// of their own and ride the meter like words (design ruling 260), and every
/// forced chip too (High Contrast separates surfaces with borders); a
/// Primary is OUTLINED where the row is drawn to the pixel (ruling 249 — the
/// owner: *"Outlined on meters"*), and keeps a ground of its own elsewhere on
/// a metered row. A lit chip keeps its hover fill: the pointer is on it.
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
    // Every form sets a Primary's words bold, and no other chip's.
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

/// One row at one motion frame (see the module doc's grammar).
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
    // Success and Info share the title ink, Warn wears `warn` and Error
    // `error` (§2.6, ruling 265); the overflow row is a link, not a message,
    // and reads in `label` — lifted to `value` while the pointer is on it
    // (ruling 259). Under a forced palette every ink is WINDOWTEXT: the glyph
    // too.
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
    // A measured LEVEL is a RAIL (ruling 243): its words sit on the band's
    // own ground.
    let rail = surface.rail && !surface.is_empty();
    let fill = if forced || !alarm {
        Fill::Meter
    } else {
        Fill::Warn
    };
    let busy = l.track.is_some();
    // A busy row off a forced palette keeps its words' side under its comet.
    let comet_side = busy && !forced && !rail;
    let span = MeterSpan { geom, cols };
    let win_w = span.win_w();
    // Every graded surface but a rail's is drawn pixel by pixel; the flat
    // look keeps whole-cell tones.
    let raster_ground = !surface.is_empty() && !surface.flat && !rail && cols > 0;
    // The fill's EDGE in window pixels, and the cell it falls in — the one
    // cell whose glyph is split (ruling 242).
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
    // A DRAWN ICON spills into the blank cells beside its own (ruling 258):
    // an edge anywhere in its three cells — on a boundary between them too —
    // crosses the icon, so the icon's cell takes the split.
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
    let columns: Option<Vec<Column>> = (!surface.is_empty() && cols > 0 && !rail).then(|| {
        (0..cols)
            .map(|x| {
                let tone = span.fine(surface, x);
                let on = match (split_col, edge_px) {
                    _ if busy || forced => tone.fill >= 128 << 8,
                    // The split icon's own ink is the track's.
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
    let kind = if rail {
        RowSurface::Rail
    } else if surface.is_empty() {
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
    // The glyph cell: the engine's glyph for this frame (an echo's ✓ / ⚠ —
    // a Fault echo's ⚠ in warn whatever the row's severity was), else the
    // row's own; a rail row's wears the rail's warn (ruling 243). Its
    // character stays in the cell; the renderer draws its icon (ruling 251).
    let mut icon = None;
    if gcol < cols {
        let glyph = rm.glyph.unwrap_or(gch);
        icon = Icon::for_char(glyph);
        cells.cells[gcol] = Cell {
            ch: glyph,
            face: Face::Word(if fault || rail { Ink::Warn } else { accent }),
            bold: !overflow,
            text_presentation: true,
        };
    }
    let (tcol, title) = &l.title;
    cells.write(*tcol, title, Face::Word(word), !overflow);
    if let Some((dcol, d)) = &l.detail {
        // The ` · ` joint occupies the three cells before the excerpt.
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
    // The load words at the TAIL of the word cluster, with no joint (ruling
    // 246): an empty reservation reads as track, never as a hole.
    if let Some((lcol, words)) = l.load {
        cells.write(lcol, words, Face::Word(Ink::Label), false);
    }
    let mut owned: Vec<(u16, u16)> = Vec::new();
    let mut rings: Vec<(u16, u16, bool)> = Vec::new();
    let chip_row = ChipRow {
        forced,
        metered: columns.is_some(),
        // A Primary on a row with a meter is OUTLINED (ruling 249) wherever
        // the row is drawn to the pixel: the bar is the row's only solid
        // accent.
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
            // The pixel the edge crosses goes to whichever side covers most
            // of it: the crisp ink's line is at its pixel boundary.
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
mod tests {
    use super::*;
    use crate::animate::{Look, Pace};
    use crate::center::{MessageCenter, Outcome};
    use crate::glass::Links;
    use crate::log::MessageLog;
    use crate::model::{Hold, Intent, Load, Message, Meter, WallStamp, tags};
    use crate::text::char_width;
    use crate::{Duration, Instant};

    const FLAT: Look = Look {
        pace: Pace::Moving,
        graded: false,
    };

    fn stamp() -> WallStamp {
        WallStamp { unix_ms: 1 }
    }

    fn bar(pm: u16) -> Message {
        Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
            .meter(Meter {
                fill_permille: Some(pm),
                stats: "31 MB / 74 MB".into(),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            })
    }

    fn busy(title: &str) -> Message {
        Message::new(tags::PACKAGES, Severity::Info, title)
            .line("detail")
            .meter(Meter::busy("~3 GB"))
            .hold(Hold::Live {
                stale_after: crate::STALE_TAILED,
            })
    }

    fn level(pm: u16) -> Message {
        Message::new(tags::SYSTEM, Severity::Info, "Typing slowed by 'yes'")
            .key(crate::STRAIN_KEY)
            .hold(Hold::Live {
                stale_after: crate::STALE_STRAIN,
            })
            .meter(Meter {
                load: Some(Load::Cpu),
                ..Meter::level(pm, "6 of 8 cores")
            })
    }

    fn with_capsules(m: Message) -> Message {
        m.action(Intent::ApplyUpdate { build: 7 })
            .action(Intent::OpenSettings {
                route: "/packages".into(),
            })
    }

    fn center(msgs: Vec<Message>, now: Instant) -> MessageCenter {
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        for mut m in msgs {
            m.reveal_after = None;
            c.post(m, stamp(), now);
        }
        c.commit_rows(now, 3);
        c
    }

    fn present(c: &MessageCenter, cols: usize) -> Presentation {
        c.presentation(cols, &char_width, None, Links::Painted)
    }

    fn window(cols: usize) -> Geometry {
        Geometry {
            win_w: cols * 16 + 16,
            cells_x: 8,
            cell_w: 16,
        }
    }

    /// Paint `msgs` at `cols` on a real window at `now + 2 s` in `look`.
    fn painted(
        msgs: Vec<Message>,
        cols: usize,
        look: Look,
        hover: Option<Hover>,
        forced: bool,
        each: impl FnOnce(&Presentation, &[RowPaint<'_>]),
    ) {
        let now = Instant::now();
        let c = center(msgs, now);
        let p = present(&c, cols);
        let m = c.motion(&p, now + Duration::from_secs(2), look);
        let rows = paint(&p, hover, window(cols), &m, forced);
        each(&p, &rows);
    }

    fn text(r: &RowPaint<'_>) -> String {
        r.cells.iter().map(|c| c.ch).collect()
    }

    fn fixtures() -> Vec<Vec<Message>> {
        vec![
            vec![with_capsules(bar(420))],
            vec![bar(0)],
            vec![bar(1000)],
            vec![busy("Installing Homebrew").action(Intent::NewWindow)],
            vec![level(640)],
            vec![Message::new(tags::CONFIG, Severity::Warn, "Misspelled setting").line("x")],
            vec![
                with_capsules(bar(333)),
                busy("Installing Homebrew"),
                Message::new(tags::CONFIG, Severity::Error, "Tests failed on main"),
                Message::new(tags::CONFIG, Severity::Info, "One more"),
            ],
        ]
    }

    /// Every painted row is exactly `cols` cells, and carries the layout's
    /// words where the layout put them.
    #[test]
    fn every_row_is_exactly_cols_wide_and_carries_its_title() {
        for msgs in fixtures() {
            for cols in [60usize, 80, 120, 160] {
                for look in [Look::MOVING, Look::STILL, FLAT] {
                    for forced in [false, true] {
                        painted(msgs.clone(), cols, look, None, forced, |p, rows| {
                            assert_eq!(rows.len(), p.rows.len());
                            for (r, l) in rows.iter().zip(&p.rows) {
                                assert_eq!(r.cells.len(), cols);
                                let t = text(r);
                                let (tcol, title) = &l.title;
                                let at: String =
                                    t.chars().skip(*tcol).take(title.chars().count()).collect();
                                assert_eq!(&at, title, "{t:?}");
                                if let Some(cols) = &r.columns {
                                    assert_eq!(cols.len(), r.cells.len());
                                }
                            }
                        });
                    }
                }
            }
        }
    }

    /// A time slot's blank cells are cleared to its ink, and a later write
    /// resets the glyph cell's text presentation.
    #[test]
    fn a_slot_is_cleared_to_its_width_in_its_ink() {
        let mut cells = Cells {
            cells: vec![Cell::BLANK; 20],
        };
        cells.cells[5].ch = 'x';
        cells.slot((3, 6), STALLED_WORD);
        let t: String = cells.cells.iter().map(|c| c.ch).collect();
        assert_eq!(&t[3..9], "stalle");
        cells.slot((3, 10), "35 s left");
        assert!(
            cells.cells[3..13]
                .iter()
                .all(|c| c.face == Face::Word(Ink::Value))
        );
        assert_eq!(cells.cells[12].ch, ' ');
        cells.slot((10, 4), "for 3m");
        assert!(
            cells.cells[10..14]
                .iter()
                .all(|c| c.face == Face::Word(Ink::Label))
        );
        cells.cells[0].text_presentation = true;
        cells.write(0, "a", Face::Word(Ink::Value), true);
        assert!(!cells.cells[0].text_presentation && cells.cells[0].bold);
        // Clipped at the row's end.
        cells.slot((18, 6), "abcdef");
        cells.write(19, "xyz", Face::Blank, false);
        assert_eq!(cells.cells.len(), 20);
    }

    /// The glyph cell is text presentation with its drawn icon.
    #[test]
    fn the_glyph_cell_is_text_presentation_with_its_icon() {
        painted(vec![bar(420)], 80, Look::MOVING, None, false, |p, rows| {
            let (gcol, ch) = p.rows[0].glyph;
            let cell = rows[0].cells[gcol];
            assert!(cell.text_presentation && cell.bold);
            assert_eq!(cell.ch, ch);
            assert_eq!(
                rows[0].icon,
                Some((gcol as u16, Icon::for_char(ch).unwrap()))
            );
            assert_eq!(
                rows[0].cells.iter().filter(|c| c.text_presentation).count(),
                1
            );
        });
    }

    /// Forced: brackets, riding chips on a meter, no ring, no outline.
    #[test]
    fn forced_chips_are_brackets_and_never_rings() {
        for msgs in [
            vec![with_capsules(bar(420))],
            vec![with_capsules(Message::new(
                tags::CONFIG,
                Severity::Info,
                "A",
            ))],
        ] {
            for look in [Look::MOVING, FLAT] {
                painted(msgs.clone(), 80, look, None, true, |p, rows| {
                    let r = &rows[0];
                    assert!(r.rings.is_empty() && r.owned.is_empty(), "{r:?}");
                    for cap in &p.rows[0].capsules {
                        assert_eq!(r.cells[cap.col].ch, '[');
                        assert_eq!(r.cells[cap.col + cap.width - 1].ch, ']');
                        if r.columns.is_none() {
                            assert!(matches!(
                                r.cells[cap.col].face,
                                Face::Chip(ChipFace {
                                    form: ChipForm::Forced,
                                    ..
                                })
                            ));
                        } else {
                            assert!(matches!(r.cells[cap.col].face, Face::Word(_)));
                        }
                    }
                });
            }
        }
    }

    /// On a meter a resting Secondary rides, and a Primary wears the outlined
    /// form — ringed only where the row is drawn to the pixel, else owning
    /// its cells.
    #[test]
    fn a_metered_secondary_rides_and_a_primary_rings_only_where_drawn() {
        for (look, rings) in [(Look::MOVING, true), (Look::STILL, true), (FLAT, false)] {
            painted(
                vec![with_capsules(bar(420))],
                80,
                look,
                None,
                false,
                |p, rows| {
                    let r = &rows[0];
                    let caps = &p.rows[0].capsules;
                    let primary = caps
                        .iter()
                        .find(|c| c.role == CapsuleRole::Primary)
                        .unwrap();
                    let secondary = caps
                        .iter()
                        .find(|c| c.role == CapsuleRole::Secondary)
                        .unwrap();
                    assert_eq!(
                        r.cells[secondary.col + 1].face,
                        Face::Word(Ink::Value),
                        "the Secondary rides"
                    );
                    assert_eq!(r.rings.len(), usize::from(rings), "{look:?}");
                    assert_eq!(r.owned.len(), usize::from(!rings), "{look:?}");
                    // Unringed (the flat look), it still wears the ring's
                    // inside and the accent label.
                    assert!(matches!(
                        r.cells[primary.col + 1].face,
                        Face::Chip(ChipFace {
                            form: ChipForm::Outlined,
                            metered: true,
                            ..
                        })
                    ));
                },
            );
        }
        // A lit Secondary wears its own face.
        let hover = Some(Hover {
            row: 0,
            target: HoverTarget::Capsule(ActionIndex(1)),
        });
        painted(
            vec![with_capsules(bar(420))],
            80,
            Look::MOVING,
            hover,
            false,
            |p, rows| {
                let sec = p.rows[0]
                    .capsules
                    .iter()
                    .find(|c| c.role == CapsuleRole::Secondary)
                    .unwrap();
                assert!(matches!(
                    rows[0].cells[sec.col + 1].face,
                    Face::Chip(ChipFace { lit: true, .. })
                ));
            },
        );
    }

    /// The split and the icon split are one cell, absent over a chip's own
    /// cells, and only on a bar drawn to the pixel.
    #[test]
    fn the_split_is_one_cell_never_under_a_chip() {
        let now = Instant::now();
        let mut seen = 0usize;
        for pm in (0..=1000u16).step_by(7) {
            let c = center(vec![with_capsules(bar(pm))], now);
            for cols in [60usize, 80, 120] {
                let p = present(&c, cols);
                for look in [Look::MOVING, Look::STILL, FLAT] {
                    let m = c.motion(&p, now + Duration::from_secs(2), look);
                    for g in [Geometry::cells_only(cols), window(cols)] {
                        let rows = paint(&p, None, g, &m, false);
                        let r = &rows[0];
                        let Some(s) = r.split else {
                            continue;
                        };
                        seen += 1;
                        assert!(r.raster_ground && r.edge_px.is_some());
                        assert!(matches!(r.kind, RowSurface::Bar { .. }));
                        let col = usize::from(s.col);
                        let within =
                            |&(a, b): &(u16, u16)| (usize::from(a)..usize::from(b)).contains(&col);
                        assert!(!r.owned.iter().any(within));
                        assert!(!r.rings.iter().any(|&(a, b, _)| within(&(a, b))));
                        let (x0, x1) = r.span.px(col);
                        let e = r.edge_px.unwrap();
                        let gcol = p.rows[0].glyph.0;
                        assert!(
                            (x0 < e && e < x1) || col == gcol,
                            "{pm} {cols}: the split is the edge's cell or the icon's"
                        );
                    }
                }
            }
        }
        // Non-vacuity: a sweep that met no split would pass on no assertion.
        assert!(seen > 0, "no paint in the sweep carried a split");
    }

    /// A plain row and a rail row have no columns; a busy row's columns are
    /// never on the fill's side off a forced palette.
    #[test]
    fn columns_are_absent_on_plain_and_rail_rows_and_busy_never_rides_the_fill() {
        painted(
            vec![Message::new(tags::CONFIG, Severity::Warn, "W")],
            80,
            Look::MOVING,
            None,
            false,
            |_, rows| {
                assert!(rows[0].columns.is_none());
                assert_eq!(rows[0].kind, RowSurface::Plain);
            },
        );
        painted(
            vec![level(700)],
            80,
            Look::MOVING,
            None,
            false,
            |_, rows| {
                assert!(rows[0].columns.is_none());
                assert_eq!(rows[0].kind, RowSurface::Rail);
            },
        );
        let now = Instant::now();
        let c = center(vec![busy("Installing Homebrew")], now);
        let p = present(&c, 80);
        for k in 0..90u32 {
            let m = c.motion(&p, now + crate::ANIM_FRAME * k, Look::MOVING);
            let rows = paint(&p, None, window(80), &m, false);
            let cols = rows[0].columns.as_ref().expect("a comet's track");
            assert!(cols.iter().all(|c| !c.on_fill));
            assert!(matches!(rows[0].kind, RowSurface::Busy { still: false }));
            let forced = paint(&p, None, window(80), &m, true);
            assert!(matches!(forced[0].kind, RowSurface::Bar { .. }));
        }
        let m = c.motion(&p, now, Look::STILL);
        let rows = paint(&p, None, window(80), &m, false);
        assert!(matches!(rows[0].kind, RowSurface::Busy { still: true }));
    }

    /// The pastel edge line lies inside the fill's last pixels, each share in
    /// 0..=1, ending at the edge.
    #[test]
    fn the_edge_line_ends_at_the_edge() {
        let now = Instant::now();
        for pm in [60u16, 420, 777, 993] {
            let c = center(vec![bar(pm)], now);
            let p = present(&c, 80);
            let m = c.motion(&p, now, Look::STILL);
            let rows = paint(&p, None, window(80), &m, false);
            let r = &rows[0];
            let line = r.edge_line().expect("a bar ending inside the row");
            let e = r.edge_px.unwrap();
            assert!(u64::from(line.end) >= e && u64::from(line.first) <= e);
            assert!(!line.coverage.is_empty());
            for &(px, k) in &line.coverage {
                assert!((0.0..=1.0).contains(&k), "{k}");
                assert!(px >= line.first as usize && px < line.end as usize);
            }
            assert!(
                paint(&p, None, window(80), &m, true)[0]
                    .edge_line()
                    .is_none()
            );
        }
    }

    /// The icon set is the band's closed glyph set.
    #[test]
    fn icons_are_exactly_the_bands_glyph_set() {
        let set = [
            '\u{2139}', '\u{2713}', '\u{2714}', '\u{26a0}', '\u{2715}', '\u{2716}', '\u{2717}',
            '\u{21e3}', '\u{2193}', '\u{21bb}', '\u{2191}', '\u{23f8}', '\u{2726}', '!',
            '\u{00b7}', '\u{2026}', '\u{2296}',
        ];
        for ch in set {
            assert!(Icon::for_char(ch).is_some(), "{ch:?}");
        }
        let n = (0..=0x10_ffffu32)
            .filter_map(char::from_u32)
            .filter(|&c| Icon::for_char(c).is_some())
            .count();
        assert_eq!(n, set.len());
    }

    /// An echo's fade is handed through, and a Fault echo's glyph wears warn.
    #[test]
    fn a_fault_echo_fades_under_a_warn_glyph() {
        let now = Instant::now();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        let id = c.post(bar(500), stamp(), now).id;
        c.commit_rows(now, 3);
        let at = now + Duration::from_millis(1500);
        assert!(c.resolve(id, Outcome::Warn, at));
        let p = present(&c, 80);
        let mut faded = false;
        for k in 0..20u64 {
            let m = c.motion(&p, at + Duration::from_millis(k * 40), Look::MOVING);
            let rows = paint(&p, None, window(80), &m, false);
            let g = p.rows[0].glyph.0;
            assert_eq!(rows[0].cells[g].face, Face::Word(Ink::Warn));
            faded |= rows[0].fade > 0;
        }
        assert!(faded);
    }

    // -- The mapping (moved from the host's painter tests, ruling 319). ----

    /// Each column's lit coverage of a bar at `fill` on `g`: the fill channel
    /// of its window-pixel span's mean tone (0 track … 255 full).
    fn coverage(fill: u16, cols: usize, g: Geometry) -> Vec<u8> {
        coverage_in(fill, cols, g, true)
    }

    /// [`coverage`] in the FLAT look (High Contrast): whole inks only.
    fn coverage_flat(fill: u16, cols: usize, g: Geometry) -> Vec<u8> {
        coverage_in(fill, cols, g, false)
    }

    fn coverage_in(fill: u16, cols: usize, g: Geometry, graded: bool) -> Vec<u8> {
        let surface = crate::animate::bar(fill, None, graded);
        let span = MeterSpan { geom: g, cols };
        (0..cols).map(|x| span.tone(&surface, x).fill).collect()
    }

    /// THE HONEST ENDS IN THE FLAT LOOK (ruling 55; review 2026-09-24).
    /// Under High Contrast every cell is whole — the fill or the track — and
    /// the first and last columns carry the gutters, so their CENTRES would
    /// leave a started pass an empty track (1 ‰ on an 80-column window) and
    /// light the whole window at 99.x %. Main's `lit.clamp(1, cols − 1)`
    /// holds in every look: any started fill lights the first column, only
    /// 1000 ‰ the last, and the lit columns are one run from the left.
    #[test]
    fn the_flat_meter_keeps_the_honest_ends() {
        for (g, cols) in [
            (
                Geometry {
                    win_w: 1296,
                    cells_x: 8,
                    cell_w: 16,
                },
                80,
            ),
            (
                Geometry {
                    win_w: 2000,
                    cells_x: 31,
                    cell_w: 17,
                },
                114,
            ),
        ] {
            assert!(coverage_flat(0, cols, g).iter().all(|&f| f == 0), "0 %");
            assert!(
                coverage_flat(1000, cols, g).iter().all(|&f| f == 255),
                "100 %: the whole window"
            );
            for fill in 1..1000u16 {
                let cov = coverage_flat(fill, cols, g);
                assert!(
                    cov.iter().all(|&f| f == 0 || f == 255),
                    "{fill}: whole inks"
                );
                assert_eq!(cov[0], 255, "{fill}: a started pass shows");
                assert_eq!(cov[cols - 1], 0, "{fill}: only 100 % fills the last");
                assert!(
                    cov.windows(2).all(|p| p[0] >= p[1]),
                    "{fill}: one run from the left"
                );
            }
        }
    }

    /// THE METER IS THE ROW, the MAPPING (ruling 55, 2026-09-23 — owner:
    /// *"make sure that 0% and 100% and similar concepts are mapped to the
    /// relative size of the screen with such top progress bars"* — amended by
    /// ruling 138: whole cells, smooth by TONE coverage). On a real 2000 px
    /// Retina window (pad 24, 17 px cells, 114 columns, a 14 px remainder
    /// split 7/7, so column 0 at x = 31): 0 ‰ lights nothing, 1000 ‰ every
    /// column fully — the first and last columns' spans run out to the
    /// window's edges, so through the gutter tones the window edge to edge —
    /// a started pass lights the first column by its coverage, an unfinished
    /// one never fills the last; at every fill the lit window pixels
    /// (coverage × span) add up to `fill · W` — 50 % at the window's exact
    /// middle — with at most ONE fractional cell, and no column ever runs
    /// backwards as the fill grows.
    #[test]
    fn the_meter_maps_the_fill_onto_the_window_edge_to_edge() {
        let g = Geometry {
            win_w: 2000,
            cells_x: 31,
            cell_w: 17,
        };
        let cols = 114;
        let span = MeterSpan { geom: g, cols };
        assert_eq!(span.px(0), (0, 48), "column 0 carries the left gutter");
        assert_eq!(
            span.px(cols - 1),
            (31 + 113 * 17, 2000),
            "the last, the right"
        );
        assert!(
            coverage(0, cols, g).iter().all(|&f| f == 0),
            "0 % lights nothing"
        );
        assert!(
            coverage(1000, cols, g).iter().all(|&f| f == 255),
            "100 % lights every column, edge to edge"
        );
        let started = coverage(1, cols, g);
        assert!(
            started[0] > 0 && started[1] == 0,
            "a started pass shows: {:?}",
            &started[..2]
        );
        let unfinished = coverage(999, cols, g);
        assert!(
            unfinished[cols - 1] < 255,
            "only a finished pass fills the last column"
        );
        assert!(unfinished[cols - 2] == 255);
        let half = coverage(500, cols, g);
        // Column 57 starts at x = 31 + 57·17 = 1000: the window's middle.
        assert!(half[..57].iter().all(|&f| f == 255) && half[57..].iter().all(|&f| f == 0));
        let mut prev = vec![0u8; cols];
        for fill in 0..=1000u16 {
            let cov = coverage(fill, cols, g);
            let lit: f64 = cov
                .iter()
                .enumerate()
                .map(|(x, &f)| {
                    let (x0, x1) = span.px(x);
                    f64::from(f) / 255.0 * (x1 - x0) as f64
                })
                .sum();
            let want = 2000.0 * f64::from(fill) / 1000.0;
            assert!(
                (lit - want).abs() <= 0.5,
                "fill {fill}: {lit:.2} px lit, the fill is at {want} px"
            );
            assert!(
                cov.iter().filter(|&&f| f > 0 && f < 255).count() <= 1,
                "fill {fill}: one edge cell at most"
            );
            assert!(
                cov.iter().zip(&prev).all(|(now, was)| now >= was),
                "fill {fill}: the meter ran backwards"
            );
            prev = cov;
        }
        // Degenerate widths answer without a panic and inside the row.
        for cols in 1..4 {
            for fill in [0u16, 1, 499, 500, 999, 1000] {
                assert_eq!(coverage(fill, cols, g).len(), cols);
            }
        }
        // THE WINDOW, NOT THE GRID: with the grid's own mapping a 10 % fill on
        // a 20-column grid inside a 400 px window with 100 px gutters would
        // light 2 columns (40 px from the grid's edge, 140 px from the
        // window's); mapped onto the window, 10 % of 400 px is 40 px — still
        // inside the left gutter, which column 0's span carries.
        let wide_gutters = Geometry {
            win_w: 400,
            cells_x: 100,
            cell_w: 10,
        };
        let cov = coverage(100, 20, wide_gutters);
        assert!(cov[0] > 0 && cov[0] < 255 && cov[1..].iter().all(|&f| f == 0));
        let cov = coverage(500, 20, wide_gutters);
        assert!(
            cov[..10].iter().all(|&f| f == 255) && cov[10..].iter().all(|&f| f == 0),
            "50 % of the window is the window's middle, not the grid's"
        );
    }

    /// THE COMET SWEEPS THE WINDOW EDGE TO EDGE, SOFTLY (rulings 55, 138):
    /// mapped onto real 2000 px and 1000 px windows through [`MeterSpan`], the comet
    /// lights the first column (the left gutter's) as it enters and the last
    /// (the right gutter's) as it leaves; while it is wholly inside, it is
    /// about a fifth of the WINDOW wide ([`crate::COMET_PERMILLE`]);
    /// and it has no straight edge anywhere — no two neighbouring cells step
    /// from full to empty.
    #[test]
    fn the_comet_sweeps_the_window_edge_to_edge() {
        for (win_w, cols, cells_x, cell_w) in
            [(2000usize, 114usize, 31usize, 17usize), (1000, 80, 20, 12)]
        {
            sweep_edge_to_edge(
                Geometry {
                    win_w,
                    cells_x,
                    cell_w,
                },
                cols,
            );
        }
    }

    /// [`the_comet_sweeps_the_window_edge_to_edge`] on one window.
    fn sweep_edge_to_edge(g: Geometry, cols: usize) {
        let span = MeterSpan { geom: g, cols };
        let (mut first, mut last) = (false, false);
        let period = crate::COMET_PERIOD;
        let steps = period.as_millis() / crate::ANIM_FRAME.as_millis();
        for k in 0..u32::try_from(steps).unwrap() {
            let since = crate::ANIM_FRAME * k;
            let surface = crate::animate::comet(since, true);
            let fill: Vec<u8> = (0..cols).map(|x| span.tone(&surface, x).fill).collect();
            first |= fill[0] > 0;
            last |= fill[cols - 1] > 0;
            for (x, pair) in fill.windows(2).enumerate() {
                assert!(
                    pair[0].abs_diff(pair[1]) <= 160,
                    "frame {k}: a hard edge at col {x}: {pair:?}"
                );
            }
            if fill[0] == 0 && fill[cols - 1] == 0 && fill.iter().any(|&f| f > 0) {
                let lit = fill.iter().filter(|&&f| f > 0).count();
                let fifth = cols as f64 * f64::from(crate::COMET_PERMILLE) / 1000.0;
                assert!(
                    (lit as f64) >= fifth * 0.8 && (lit as f64) <= fifth * 1.4,
                    "frame {k}: {lit} cells lit, a fifth of the window is {fifth:.1}"
                );
            }
        }
        assert!(first && last, "it enters and leaves through the gutters");
    }

    /// THE COMET ENTERS WITHOUT A STRAIGHT EDGE (review round 3, 2026-09-24,
    /// re-pinned on whole-cell tones by ruling 138): through its first frames
    /// the first column — the left gutter's — takes the entering head's
    /// COVERAGE, rising through intermediate tones frame by frame rather than
    /// switching on, and it never lights before the head has entered.
    #[test]
    fn the_comet_enters_without_a_straight_edge() {
        for (win_w, cols, cells_x, cell_w) in
            [(2000usize, 114usize, 31usize, 17usize), (1000, 80, 20, 12)]
        {
            let g = Geometry {
                win_w,
                cells_x,
                cell_w,
            };
            let span = MeterSpan { geom: g, cols };
            for base in [0u32, 1, 2] {
                let mut seen = Vec::new();
                for k in 0..24u32 {
                    let since = crate::COMET_PERIOD * base + crate::ANIM_FRAME * k;
                    let surface = crate::animate::comet(since, true);
                    seen.push(span.tone(&surface, 0).fill);
                }
                // The head is a peak, not a plateau: the first column's mean
                // rises while the head crosses it and falls as the tail
                // follows, never reaching the full fill (the column is wider
                // than the head). Entering is the run up to that peak.
                let first = seen.iter().position(|&f| f > 0).unwrap_or(seen.len());
                let peak = (first..seen.len())
                    .max_by_key(|&k| seen[k])
                    .unwrap_or(first);
                let rising = &seen[first..=peak.min(seen.len() - 1)];
                assert!(
                    rising.len() >= 3 && rising[0] < 128,
                    "{win_w} px crossing {base}: the first column switched on: {seen:?}"
                );
                assert!(
                    rising.windows(2).all(|w| w[1] >= w[0]),
                    "{win_w} px crossing {base}: the entering head ran backwards: {seen:?}"
                );
                assert!(
                    seen[peak..].windows(2).all(|w| w[1] <= w[0]),
                    "{win_w} px crossing {base}: the tail left the first column unevenly: {seen:?}"
                );
            }
        }
    }

    /// A PIXEL'S CELL (ruling 328, the host's hit test): the column under pixel
    /// `x` of a lattice whose column 0 starts at `cells_x` — the leading gutter
    /// is column 0, every cell's pixels map to it and its right neighbour starts
    /// at the next pixel, and a zero cell width never divides by zero.
    #[test]
    fn a_pixel_maps_to_the_cell_it_lies_in() {
        let g = Geometry {
            win_w: 8 * 10 + 2 * 4,
            cells_x: 4,
            cell_w: 8,
        };
        for x in 0..4 {
            assert_eq!(g.cell_at(x), 0, "px {x}: the gutter is column 0");
        }
        for col in 0..10 {
            for dx in 0..8 {
                assert_eq!(g.cell_at(4 + col * 8 + dx), col, "col {col} +{dx}");
            }
        }
        assert_eq!(
            g.cell_at(4 + 10 * 8),
            10,
            "past the last column: not clamped"
        );
        let zero = Geometry {
            win_w: 0,
            cells_x: 4,
            cell_w: 0,
        };
        assert_eq!(zero.cell_at(3), 0);
        assert_eq!(zero.cell_at(9), 5, "a zero cell is one pixel wide");
        assert_eq!(Geometry::cells_only(80).cell_at(79), 79);
    }
}
