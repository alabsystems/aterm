// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The message band on the renderer's frame: the one composition every host runs to turn
//! the engine's resolved band rows (`aterm_messages::ink`, see
//! docs/DESIGN-unified-messages-2026-09-21.md) into this renderer's input. That is the
//! [`RenderCell`] write ([`rows`]), each row's pixel raster on the frame ([`on_frame`]),
//! the stack's seams ([`seal_stack`], [`floor_rings`], [`RAIL_SEAM_MIN_CELL_H`]), the
//! gutters' chrome bleed ([`band_bleed`], [`band_row_edges`]) and the splice above the
//! grid ([`compose_band`]).
//!
//! It lives beside the CPU rasterizer because the native window (`aterm-gui`) and the web
//! module (`aterm-wasm`) both render through it. A host keeps only what it alone has: the
//! High Contrast latch, the Linux CSD base, the palette read and the pointer.
//!
//! Pure: no clock, no platform, no state. Rows reserve space above the grid; nothing here
//! overlays a terminal cell.

use aterm_core::render::{HostRowPixels, RenderInput};
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_messages::ink::{BandInks, Resolved, RowRaster, mix3};
use aterm_messages::paint::Icon;

use crate::{
    BandIcon, CHROME_ROW_EDGES, ChromeBleed, ChromeIcon, ChromeRaster, ChromeRing, ChromeRowEdges,
    InkSplit, chrome_ring_floor_cols,
};

/// Build one render cell for compact chrome.
#[must_use]
pub fn cell(ch: char, fg: [u8; 3], bg: [u8; 3], bold: bool, seam: bool) -> RenderCell {
    RenderCell {
        ch,
        fg,
        bg,
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: seam,
        underline_color: None,
        // One ink per band needs no seam colour: the overline paints in `fg`. A band
        // that varies its ink sets this per row (`link_target::caption_row`), because a
        // structural edge must not brighten under the words.
        overline_color: None,
        script: aterm_core::terminal::Script::Normal,
    }
}

/// A `cols`-wide row of blank chrome cells in `fg` on `bg` (with a top seam when `seam`).
#[must_use]
pub fn blank_row(cols: usize, fg: [u8; 3], bg: [u8; 3], seam: bool) -> Vec<RenderCell> {
    vec![cell(' ', fg, bg, false, seam); cols]
}

/// One empty band row in a painted row's colours: [`compose_band`] pads with it.
#[must_use]
pub fn blank_band_row(cols: usize, inks: &BandInks) -> Vec<RenderCell> {
    blank_row(cols, inks.label, inks.bar_bg, false)
}

/// The renderer's drawn icon for the engine's ([`Icon`] and [`BandIcon`] are one set in two
/// tables, held together by `band_icon_ids_match_the_renderers`). A `const fn`, not a
/// `From`: the orphan rule forbids an impl between two foreign types here.
#[must_use]
pub const fn band_icon(icon: Icon) -> BandIcon {
    use BandIcon as B;
    match icon {
        Icon::Info => B::Info,
        Icon::Success => B::Success,
        Icon::Warn => B::Warn,
        Icon::Error => B::Error,
        Icon::Download => B::Download,
        Icon::Update => B::Update,
        Icon::Upload => B::Upload,
        Icon::Pause => B::Pause,
        Icon::Sparkle => B::Sparkle,
        Icon::Alert => B::Alert,
        Icon::Dot => B::Dot,
        Icon::More => B::More,
        Icon::Remove => B::Remove,
    }
}

/// The rail band's height on a `cell_h`-pixel row: about an eighth of it, never under two
/// pixels, in the row's lowest pixels, where a bar's glint sits under its words'
/// descenders (ruling 242).
#[must_use]
fn rail_px(cell_h: usize) -> usize {
    ((cell_h + 4) / 8).max(2)
}

/// Row raster `r` on a frame whose column 0 starts `lo` window pixels in from
/// the window's left edge (the leading remainder band, `cells_x − pad`) and
/// which is `frame_w` pixels wide, for chrome row `row` whose band is `cell_h`
/// pixels tall: the renderer's [`ChromeRaster`]. A frame pixel past the
/// window's own columns repeats the edge one.
#[must_use]
pub fn on_frame(r: &RowRaster, row: u16, lo: usize, frame_w: usize, cell_h: usize) -> ChromeRaster {
    let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    let at = |v: &[[u8; 3]], x: usize| v[(x + lo).min(v.len().saturating_sub(1))];
    let ground: std::sync::Arc<[u32]> = if r.ground.is_empty() {
        std::sync::Arc::from(Vec::new())
    } else {
        (0..frame_w).map(|x| pack(at(&r.ground, x))).collect()
    };
    let rail: std::sync::Arc<[u32]> = if r.rail.is_empty() {
        std::sync::Arc::from(Vec::new())
    } else {
        (0..frame_w)
            .map(|x| r.rail[(x + lo).min(r.rail.len() - 1)].map_or(ChromeRaster::KEEP, pack))
            .collect()
    };
    let rail_h = if r.rail.is_empty() {
        0
    } else {
        u16::try_from(rail_px(cell_h)).unwrap_or(0)
    };
    ChromeRaster {
        row,
        ground,
        rail_h,
        clear_rail: false,
        rail,
        own: r.own.clone(),
        rings: r
            .rings
            .iter()
            .map(|&(start, end, ring, inner)| ChromeRing {
                start,
                end,
                ring: pack(ring),
                inner: pack(inner),
                // The seam is the composed stack's ([`floor_rings`]).
                seam: None,
            })
            .collect(),
        icons: r
            .icons
            .iter()
            .map(|&(col, icon)| ChromeIcon {
                col,
                icon: band_icon(icon),
            })
            .collect(),
        split: r.split.and_then(|(col, x, ink, bg)| {
            Some(InkSplit {
                col,
                x: u32::try_from(usize::try_from(x).ok()?.checked_sub(lo)?).ok()?,
                ink: pack(ink),
                bg: pack(bg),
            })
        }),
    }
}

/// One painted band: its rows; for each row the `(left, right)` gutter tones
/// of its meter (`None` on an unmetered row, whose gutters keep the band's
/// own tone); and each row's pixel raster (`None` where the cells say it all).
pub type PaintedBand = (
    Vec<Vec<RenderCell>>,
    Vec<Option<([u8; 3], [u8; 3])>>,
    Vec<Option<RowRaster>>,
);

/// Write the engine's resolved rows (`aterm_messages::ink::paint_band`) as
/// [`RenderCell`]s, keeping each metered row's gutter edges and its raster. No row carries
/// the closing hairline: only the composed stack knows which row is last ([`seal_stack`]).
#[must_use]
pub fn rows(resolved: Vec<Resolved>) -> PaintedBand {
    let n = resolved.len();
    let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(n);
    let mut edges = Vec::with_capacity(n);
    let mut rasters = Vec::with_capacity(n);
    for r in resolved {
        // The gutters continue the cells actually painted at the row's edges (a chip at
        // column 0 wears its own fill, an echo's fade mixes both), so a tone changes only
        // with its edge cell: the renderer's no-epoch contract (`ChromeBleed::row_edges`).
        // The pixel raster, if any, draws over both and dirties its own row.
        let row: Vec<RenderCell> = r
            .cells
            .iter()
            .map(|k| {
                let mut out = cell(k.ch, k.fg, k.bg, k.bold, false);
                out.text_presentation = k.text_presentation;
                // A wide glyph's right half (a CJK or emoji glyph in the
                // band's words): the renderer spans the lead cell across it.
                out.wide = k.continuation;
                out
            })
            .collect();
        edges.push((r.metered && !row.is_empty()).then(|| (row[0].bg, row[row.len() - 1].bg)));
        rows.push(row);
        rasters.push(r.raster);
    }
    (rows, edges, rasters)
}

/// Close a band's bottom edge in place across the whole row in the band's own ink, not
/// each cell's: a seam is a structural edge and must not brighten under the words above it.
fn seal_band_bottom(row: &mut [RenderCell], ink: [u8; 3]) {
    for cell in row.iter_mut() {
        cell.underline = UnderlineStyle::Single;
        cell.underline_color = Some(ink);
    }
}

/// How strongly the divider between stacked band rows shows the seam's ink over the band:
/// a hairline that separates, softer than the seam that closes the stack.
pub const DIVIDER_ALPHA: f32 = 0.35;

/// Close the composed chrome stack against the terminal: the last row carries the seam (in
/// `inks.label`), every row above it a divider. `forced` says an OS-forced palette is up (a
/// host's High Contrast latch): the divider is then the seam's own ink.
///
/// The seam belongs to the stack, not the painter's last row: the compose pads the cache
/// with [`blank_band_row`] when the geometry has committed more rows than it holds (after a
/// dismissal, for `SHRINK_QUIET` or while an echo runs, and through a handoff freeze), and
/// trims it when the count is below it. Sealing after composition covers every shape with
/// one rule. No band cell uses underline otherwise, so clearing it above the last row erases
/// only a stale seam.
pub fn seal_stack(rows: &mut [Vec<RenderCell>], inks: &BandInks, forced: bool) {
    let Some((last, above)) = rows.split_last_mut() else {
        return;
    };
    // A 1 px divider keeps two rows of one ground apart; a lit rail takes its place
    // ([`give_seam_to_rails`]).
    let divider = if forced {
        inks.label
    } else {
        mix3(inks.bar_bg, inks.label, DIVIDER_ALPHA)
    };
    for row in above {
        for cell in row.iter_mut() {
            cell.underline = UnderlineStyle::Single;
            cell.underline_color = Some(divider);
        }
    }
    seal_band_bottom(last, inks.label);
}

/// A rail (a bar's glint drawn low, ruling 242) runs in its row's lowest pixels, where the
/// closing seam runs on the last row: where the rail is lit it is the row's lower edge, so
/// the seam gives way cell by cell (a cell at least half lit). `cells` are the composed
/// rows, `rasters` keyed by row, `cols` cells of `cell_w` px starting `pad` px into the
/// frame.
fn give_seam_to_rails(
    cells: &mut [Vec<RenderCell>],
    rasters: &[ChromeRaster],
    cols: usize,
    pad: usize,
    cell_w: usize,
) {
    let cw = cell_w.max(1);
    let railed: Vec<(usize, Vec<usize>)> = rasters
        .iter()
        .filter(|m| !m.rail.is_empty())
        .map(|m| {
            let lit = (0..cols)
                .filter(|&c| {
                    let x0 = pad + c * cw;
                    let on = (x0..x0 + cw)
                        .filter(|&x| m.rail.get(x).is_some_and(|&v| v != ChromeRaster::KEEP))
                        .count();
                    on * 2 >= cw
                })
                .collect();
            (usize::from(m.row), lit)
        })
        .collect();
    for (r, lit) in railed {
        if let Some(row) = cells.get_mut(r) {
            for c in lit {
                if let Some(cell) = row.get_mut(c) {
                    cell.underline = UnderlineStyle::None;
                    cell.underline_color = None;
                }
            }
        }
    }
}

/// The smallest cell height, in px, at which the stack's seam is drawn as a rail. The rail
/// lifts the words by what a 1 px rail and its clear row need beyond the face's foot room
/// ([`crate::chrome_fit`]), never more than the face's head room. Measured with the macOS
/// default face (`band_seam_clearance.rs`): an 18 px cell (15 px type) leaves the clear
/// row; a 16 px cell has one pixel of head room, so the words would touch the rail. Below
/// this the seam stays the face's underline.
pub const RAIL_SEAM_MIN_CELL_H: usize = 18;

/// Whether a band in a `cell_h` px cell can close its rows with rails that keep the words
/// one clear pixel row off them: the cell reaches [`RAIL_SEAM_MIN_CELL_H`] and, when the
/// host measured it ([`BandFrame::room`]), the face's `(head, foot)` room pays for a 1 px
/// rail plus its clear row, `head + foot >= 2` (the lift [`crate::chrome_fit`] makes is
/// `2 − foot`, never more than `head`).
///
/// The cell height alone is a proxy measured with the macOS default face. It fails for
/// Cascadia Mono PL (the Windows default until 2026-10-16), whose hinted `$` and capitals
/// reach the cell's top row: of the cells the proxy admits, 14 px (18 px cell) and 18 px
/// (23 px cell) measure `(0, 1)`. It fails for Iosevka Term Semibold, the Windows default
/// since then, at 14 px on the 1.1 line a Windows window draws (19 px cell, `(0, 1)`); its
/// default 17 px's 23 px cell is `(2, 2)`. Without the measure their descenders end on the
/// rail's own row. JetBrains Mono has at least a pixel each of head and foot room at 12 to
/// 40 px.
#[must_use]
pub fn seam_can_rail(cell_h: usize, room: Option<(usize, usize)>) -> bool {
    cell_h >= RAIL_SEAM_MIN_CELL_H && room.is_none_or(|(head, foot)| head + foot >= 2)
}

/// Whether a native host measures the face's room ([`BandFrame::room`]): Windows only, as
/// neither its default Iosevka Term Semibold nor Cascadia Mono PL (the default before it)
/// holds the cell-height proxy ([`seam_can_rail`]). macOS's default face is the one the
/// proxy was measured with, and Linux's JetBrains Mono has the room at every size measured.
/// Web hosts never measure.
pub const NATIVE_HOST_MEASURES_ROOM: bool = cfg!(windows);

/// The [`BandFrame::room`] a native host composes with: `measure`'s `(head, foot)`
/// ([`crate::Renderer::band_room`]) where [`NATIVE_HOST_MEASURES_ROOM`], else `None`. The
/// GUI's compose and the seam-clearance tests share it, so the tests check what ships.
pub fn native_host_room(measure: impl FnOnce() -> (usize, usize)) -> Option<(usize, usize)> {
    NATIVE_HOST_MEASURES_ROOM.then(measure)
}

/// Whether the composed stack's seams are drawn as rails: the cell and face can pay for
/// them ([`seam_can_rail`]) and no composed row's raster already owns the row's foot (a
/// glint rail, an outlined capsule, or `own` columns, under which the rail would leave
/// holes). One seam style per stack, never per row.
fn seam_rails_the_stack(
    rasters: &[ChromeRaster],
    composed: usize,
    cell_h: usize,
    room: Option<(usize, usize)>,
) -> bool {
    seam_can_rail(cell_h, room)
        && rasters
            .iter()
            .filter(|m| usize::from(m.row) < composed)
            .all(|m| m.rail.is_empty() && m.rail_h == 0 && m.rings.is_empty() && m.own.is_empty())
}

/// Draw the seams as rails. The face's underline runs through the descenders of `p`, `g`
/// and `y`; instead each seam becomes a 1 px rail on the row's last pixel row
/// ([`ChromeRaster::clear_rail`]) that lifts the words two pixels, leaving one clear row
/// and no glyph ink crossing any line. The last row gets the seam, every row above its
/// divider. `seal_stack`'s underlines are cleared here (no band cell uses underline for
/// anything else). Rails are `frame_w` px wide, edge to edge, so the gutters are covered
/// ([`crate::chrome_rail_covers_gutters`]).
fn rail_the_seams(
    rows: &mut [Vec<RenderCell>],
    rasters: &mut Vec<ChromeRaster>,
    inks: &BandInks,
    forced: bool,
    frame_w: usize,
) {
    let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    let divider = if forced {
        inks.label
    } else {
        mix3(inks.bar_bg, inks.label, DIVIDER_ALPHA)
    };
    let last = rows.len().saturating_sub(1);
    for (i, row) in rows.iter_mut().enumerate() {
        for cell in row.iter_mut() {
            cell.underline = UnderlineStyle::None;
            cell.underline_color = None;
        }
        let Ok(at) = u16::try_from(i) else { continue };
        let rail: std::sync::Arc<[u32]> =
            std::iter::repeat_n(pack(if i == last { inks.label } else { divider }), frame_w)
                .collect();
        if let Some(m) = rasters.iter_mut().find(|m| m.row == at) {
            m.rail = rail;
            m.rail_h = 1;
            m.clear_rail = true;
        } else {
            rasters.push(ChromeRaster {
                row: at,
                ground: std::sync::Arc::from(Vec::new()),
                rail,
                rail_h: 1,
                clear_rail: true,
                own: Vec::new(),
                split: None,
                rings: Vec::new(),
                icons: Vec::new(),
            });
        }
    }
}

/// Give each outlined capsule its floor: the cells between a ring's rounded ends
/// ([`chrome_ring_floor_cols`]) carry a single underline in the ring's colour, so the
/// descender ink-skip carves the floor as it carves the seam. On the stack's last row the
/// seam hands its end cells to the ring ([`ChromeRing::seam`]), which draws it meeting the
/// pill's rounded foot, so no seam pixel crosses the pill's inside. Runs after
/// [`seal_stack`] and the rail hand-off; `rasters` are keyed by row.
pub fn floor_rings(rows: &mut [Vec<RenderCell>], rasters: &mut [ChromeRaster]) {
    for m in rasters {
        let Some(row) = rows.get_mut(usize::from(m.row)) else {
            continue;
        };
        for ring in &mut m.rings {
            let floor = chrome_ring_floor_cols(ring);
            let (start, end) = (
                usize::from(ring.start),
                usize::from(ring.end).min(row.len()),
            );
            if start >= end {
                continue;
            }
            // The seam, when every end cell carries it in one tone.
            let ends = || (start..end).filter(|c| !floor.contains(c));
            let tone = row[start].underline_color;
            let sealed = ends().all(|c| {
                row[c].underline == UnderlineStyle::Single && row[c].underline_color == tone
            });
            ring.seam = tone
                .filter(|_| sealed)
                .map(|[r, g, b]| (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b));
            if ring.seam.is_some() {
                for c in ends() {
                    row[c].underline = UnderlineStyle::None;
                    row[c].underline_color = None;
                }
            }
            let [_, r, g, b] = ring.ring.to_be_bytes();
            let fe = floor.end.min(end);
            for cell in row.get_mut(floor.start.min(fe)..fe).into_iter().flatten() {
                cell.underline = UnderlineStyle::Single;
                cell.underline_color = Some([r, g, b]);
            }
        }
    }
}

/// The chrome bleed's per-row gutter tones for the band's metered rows: band row `i` is
/// chrome row `first + i`, and only the `committed` rows carry any (an uncommitted cached
/// row is not on glass). Packed `0x00RRGGBB`, at most [`CHROME_ROW_EDGES`].
#[must_use]
pub fn band_row_edges(
    first: usize,
    committed: usize,
    edges: &[Option<([u8; 3], [u8; 3])>],
) -> [Option<ChromeRowEdges>; CHROME_ROW_EDGES] {
    let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    let mut out = [None; CHROME_ROW_EDGES];
    for (slot, (i, tones)) in out.iter_mut().zip(edges.iter().enumerate().take(committed)) {
        *slot = tones.map(|(left, right)| ChromeRowEdges {
            row: first + i,
            left: pack(left),
            right: pack(right),
        });
    }
    out
}

/// The chrome bleed for `bars` band rows under
/// `strip` chrome rows with no surface of their own: the band's rows reach
/// the window edges in the band's tone with the seam that closes it, and the
/// strip rows keep the padding ([`ChromeBleed::first`]). `row_edges` are the
/// metered rows' own gutter tones ([`band_row_edges`]).
#[must_use]
pub fn band_bleed(
    inks: &BandInks,
    strip: usize,
    bars: usize,
    row_edges: [Option<ChromeRowEdges>; CHROME_ROW_EDGES],
) -> ChromeBleed {
    let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    ChromeBleed {
        rows: strip + bars,
        first: strip,
        color: pack(inks.bar_bg),
        seam: Some(pack(inks.label)),
        top_extends_cells: false,
        row_edges,
    }
}

/// Where the band lands on the frame [`compose_band`] composes onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandFrame {
    /// The band's width in cells (the terminal's columns).
    pub cols: usize,
    /// One cell's width, px.
    pub cell_w: usize,
    /// One cell's height, px.
    pub cell_h: usize,
    /// The frame's side padding, px: column 0 starts here.
    pub pad: usize,
    /// Where the window's column 0 sits left of the frame's (the leading
    /// remainder band, `cells_x − pad`; `0` when the frame is the window).
    pub lo: usize,
    /// The frame's width, px (`cols·cell_w + 2·pad`).
    pub frame_w: usize,
    /// The grid-interior top before the prepend (`pad_top + head`), px.
    pub grid_top: usize,
    /// The face's `(head, foot)` room around the band's words at this size
    /// ([`crate::Renderer::band_room`]), when the host measured it; `None`
    /// keeps the cell-height rule alone ([`seam_can_rail`]).
    pub room: Option<(usize, usize)>,
}

/// Splice the band onto the just-composed frame `dst`, above the grid
/// ([`RenderInput::prepend_host_rows`]): the painted `rows` and `rasters` trimmed or padded
/// to the `committed` count, each raster placed ([`on_frame`]), the stack sealed
/// ([`seal_stack`]), the seams drawn as rails where the stack allows it
/// ([`seam_can_rail`]), and each outlined capsule floored ([`floor_rings`]). Returns the
/// rows prepended (`0`: `dst` untouched).
///
/// Under-supply is padded with [`blank_band_row`]: the window was sized for `committed`
/// rows, and composing fewer would show the terminal's top row under the band.
#[allow(
    clippy::too_many_arguments,
    reason = "the one compose both hosts call: every input is a plain value of the frame it composes"
)]
pub fn compose_band(
    dst: &mut RenderInput,
    rows: &[Vec<RenderCell>],
    rasters: &[Option<RowRaster>],
    committed: usize,
    inks: &BandInks,
    forced: bool,
    at: BandFrame,
    pool: &mut Vec<Vec<RenderCell>>,
    pixels: HostRowPixels,
) -> usize {
    if committed == 0 {
        return 0;
    }
    let padded: Vec<Vec<RenderCell>>;
    let rows: &[Vec<RenderCell>] = if rows.len() > committed {
        &rows[..committed]
    } else if rows.len() < committed {
        padded = rows
            .iter()
            .cloned()
            .chain(std::iter::repeat_with(|| blank_band_row(at.cols, inks)))
            .take(committed)
            .collect();
        &padded
    } else {
        rows
    };
    let composed = rows.len();
    // The band owns the frame's pixel-resolution chrome rows: none carry
    // over from the scratch's last use.
    dst.chrome_rasters.clear();
    dst.prepend_host_rows(
        composed,
        rows.iter().map(Vec::as_slice),
        at.cell_h,
        at.grid_top,
        pool,
        pixels,
    );
    // Each painted row's raster at its composed index; a strip prepended later shifts it
    // with every other per-row channel.
    for (i, raster) in rasters.iter().take(rows.len()).enumerate() {
        if let Some(r) = raster
            && let Ok(row) = u16::try_from(i)
        {
            dst.chrome_rasters
                .push(on_frame(r, row, at.lo, at.frame_w, at.cell_h));
        }
    }
    // The seam goes on the composed stack's last row (painted or padded), never the
    // painter's last row. Rows `0..composed` are exactly this stack.
    seal_stack(&mut dst.cells[..composed], inks, forced);
    give_seam_to_rails(
        &mut dst.cells[..composed],
        &dst.chrome_rasters,
        at.cols,
        at.pad,
        at.cell_w,
    );
    // Rails where the stack allows them: the words keep one clear pixel row above.
    if seam_rails_the_stack(&dst.chrome_rasters, composed, at.cell_h, at.room) {
        rail_the_seams(
            &mut dst.cells[..composed],
            &mut dst.chrome_rasters,
            inks,
            forced,
            at.frame_w,
        );
    }
    // An outlined capsule's floor is its cells' underline in the ring's colour; on the
    // last row the seam runs up to the pill's foot instead of through its inside.
    floor_rings(&mut dst.cells[..composed], &mut dst.chrome_rasters);
    composed
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_messages::ink::{BarBase, ThemeInks};

    fn inks() -> BandInks {
        BandInks::derive(
            ThemeInks {
                bg: [0x1e, 0x1e, 0x2e],
                fg: [0xcd, 0xd6, 0xf4],
                cursor: [0xf5, 0xe0, 0xdc],
            },
            None,
            BarBase::Blend,
        )
    }

    /// A resolved cell's continuation is the render cell's `wide`, so the renderer draws a
    /// CJK lead cell across both.
    #[test]
    fn a_continuation_is_the_render_cells_wide_half() {
        let c = inks();
        let k = |ch, continuation| aterm_messages::ink::InkedCell {
            ch,
            fg: c.value,
            bg: c.bar_bg,
            bold: false,
            text_presentation: false,
            continuation,
        };
        let (rows, _, _) = rows(vec![Resolved {
            cells: vec![k('\u{62c5}', false), k(' ', true), k('x', false)],
            metered: false,
            raster: None,
        }]);
        let wide: Vec<bool> = rows[0].iter().map(|r| r.wide).collect();
        assert_eq!(wide, [false, true, false]);
        assert_eq!(crate::materialized_cell_span(&rows[0], 0), 2);
        assert_eq!(crate::materialized_cell_span(&rows[0], 2), 1);
    }

    /// Every char the renderer draws as a band icon is the engine's icon of that char, and
    /// no other char is an engine icon.
    #[test]
    fn band_icon_ids_match_the_renderers() {
        for ch in (0..=0x10_ffffu32).filter_map(char::from_u32) {
            assert_eq!(
                BandIcon::for_char(ch),
                Icon::for_char(ch).map(band_icon),
                "{ch:?}"
            );
        }
        for icon in BandIcon::ALL {
            assert_eq!(Icon::for_char(icon.ch()).map(band_icon), Some(icon));
        }
    }

    /// No committed row leaves the frame untouched; a short cache is padded and a long one
    /// trimmed, and only the composed stack's last row carries the seam.
    #[test]
    fn compose_pads_trims_and_seals_the_stack_it_composes() {
        let c = inks();
        let at = BandFrame {
            cols: 4,
            cell_w: 8,
            cell_h: 16,
            pad: 2,
            lo: 0,
            frame_w: 36,
            grid_top: 2,
            room: None,
        };
        let engine = || RenderInput {
            rows: 2,
            cols: 4,
            cells: vec![vec![cell('x', [1, 1, 1], [2, 2, 2], false, false); 4]; 2],
            ..Default::default()
        };
        let row = |ch| vec![cell(ch, c.value, c.bar_bg, false, false); 4];
        let pixels = HostRowPixels::Translate;
        let mut f = engine();
        assert_eq!(
            compose_band(
                &mut f,
                &[row('a')],
                &[None],
                0,
                &c,
                false,
                at,
                &mut Vec::new(),
                pixels
            ),
            0
        );
        assert!(f == engine(), "no committed row: the frame is untouched");
        let mut f = engine();
        let n = compose_band(
            &mut f,
            &[row('a')],
            &[None],
            3,
            &c,
            false,
            at,
            &mut Vec::new(),
            pixels,
        );
        assert_eq!((n, f.rows), (3, 5), "one painted row padded to three");
        assert_eq!(f.cells[1], {
            let mut r = blank_band_row(4, &c);
            for k in &mut r {
                k.underline = UnderlineStyle::Single;
                k.underline_color = Some(mix3(c.bar_bg, c.label, DIVIDER_ALPHA));
            }
            r
        });
        assert!(
            f.cells[2]
                .iter()
                .all(|k| k.underline_color == Some(c.label)),
            "the seam"
        );
        assert_eq!(f.cells[3][0].ch, 'x', "the terminal starts under the band");
        let mut f = engine();
        let n = compose_band(
            &mut f,
            &[row('a'), row('b'), row('c')],
            &[None, None, None],
            2,
            &c,
            true,
            at,
            &mut Vec::new(),
            pixels,
        );
        assert_eq!(
            (n, f.cells[1][0].ch),
            (2, 'b'),
            "three painted rows trimmed to two"
        );
        assert_eq!(
            f.cells[0][0].underline_color,
            Some(c.label),
            "under a forced palette the divider is the seam's own ink"
        );
    }
}
