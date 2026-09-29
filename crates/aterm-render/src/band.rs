// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MESSAGE BAND ON THE RENDERER'S FRAME — the one composition every host
//! runs to turn the engine's resolved band rows (`aterm_messages::ink`,
//! design ruling 324 of docs/DESIGN-unified-messages-2026-09-21.md) into this
//! renderer's input: the [`RenderCell`] write ([`cell`], [`rows`]), each row's
//! pixel raster placed on the frame ([`on_frame`], with [`band_icon`] and the
//! private `rail_px`), the composed stack's seams ([`seal_stack`],
//! [`DIVIDER_ALPHA`], [`floor_rings`], the private `give_seam_to_rails`), the
//! gutters' chrome bleed
//! ([`band_bleed`], [`band_row_edges`]) and the splice that puts it all above
//! the grid ([`compose_band`], over the engine's
//! [`RenderInput::prepend_host_rows`]).
//!
//! ONE TABLE, BOTH HOSTS (ruling 331, amending 326 (e) and (h), which called
//! this host code while there was one host). The macOS/Linux/Windows window
//! (`aterm-gui`) and the web module (`aterm-wasm`) render through the SAME CPU
//! rasterizer, so the code that feeds it lives beside it: moved verbatim from
//! `aterm-gui`'s `chrome_band`, `message_band` and `app_render`, which keep
//! their old names as re-exports and thin wrappers. What stays in a host is
//! what only that host has: the High Contrast latch (the Windows writer), the
//! Linux CSD base, the presence row, the palette read and the pointer.
//!
//! Pure: no clock, no platform, no state. Rows RESERVE space above the grid —
//! nothing here overlays a terminal cell.

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
        // A band whose cells all share one ink needs no seam colour: the
        // overline already paints in `fg`. A band that varies its ink sets this
        // per row (see `link_target::caption_row`), because a seam is a
        // STRUCTURAL edge and must not brighten under the words it happens to
        // pass beneath.
        overline_color: None,
    }
}

/// A `cols`-wide row of blank chrome cells in `fg` on `bg` (with a top seam
/// when `seam`).
#[must_use]
pub fn blank_row(cols: usize, fg: [u8; 3], bg: [u8; 3], seam: bool) -> Vec<RenderCell> {
    vec![cell(' ', fg, bg, false, seam); cols]
}

/// One EMPTY band row, in the same colours a painted row uses. The compose
/// pads with this when the geometry has committed more rows than the cache
/// holds, so a reserved row is never a hole the terminal's own top row shows
/// through.
#[must_use]
pub fn blank_band_row(cols: usize, inks: &BandInks) -> Vec<RenderCell> {
    blank_row(cols, inks.label, inks.bar_bg, false)
}

/// The renderer's drawn icon for the engine's (ruling 322: one set in two
/// tables — the engine's [`Icon`] and the core's [`BandIcon`] — held together
/// by `band_icon_ids_match_the_renderers`). A `const fn`, not a `From`: the
/// orphan rule forbids an impl between two foreign types here.
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

/// The rail band's height on a `cell_h`-pixel row: about an eighth of it —
/// two pixels on a 1x cell, three on a 24-pixel one, never under two — in
/// the row's lowest pixels (design ruling 243). A level's rail is at most
/// this: the renderer fits it, with its words lifted clear, into the room
/// the face leaves ([`RowRaster::clear_rail`], ruling 248); a bar's glint
/// drawn there sits under its words' descenders as before.
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
        clear_rail: r.clear_rail && rail_h > 0,
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
/// [`RenderCell`]s, keeping each metered row's gutter edges and its raster.
/// No row carries the hairline that closes the chrome against the terminal:
/// only the COMPOSED stack knows which row is last ([`seal_stack`]).
#[must_use]
pub fn rows(resolved: Vec<Resolved>) -> PaintedBand {
    let n = resolved.len();
    let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(n);
    let mut edges = Vec::with_capacity(n);
    let mut rasters = Vec::with_capacity(n);
    for r in resolved {
        // The gutters continue the cells ACTUALLY painted at the row's two
        // edges — a chip the width law put at column 0 (a degenerate narrow
        // row) wears its own fill, not the meter's, and an echo's fade mixes
        // both — so a tone changes only together with its edge cell: the
        // renderer's no-epoch contract (`ChromeBleed::row_edges`).
        // A busy row's comet is the same surface, so its gutters light as it
        // enters and leaves. The pixel raster, where there is one, is drawn
        // over both (and dirties its row on its own).
        let row: Vec<RenderCell> = r
            .cells
            .iter()
            .map(|k| {
                let mut out = cell(k.ch, k.fg, k.bg, k.bold, false);
                out.text_presentation = k.text_presentation;
                out
            })
            .collect();
        edges.push((r.metered && !row.is_empty()).then(|| (row[0].bg, row[row.len() - 1].bg)));
        rows.push(row);
        rasters.push(r.raster);
    }
    (rows, edges, rasters)
}

/// CLOSE a band's content-facing BOTTOM edge, in place, across the WHOLE row and
/// in ONE tone — for chrome that sits ABOVE the terminal (the message band and
/// the presence row): after every write, in the band's own ink rather than each
/// cell's, because a seam is a STRUCTURAL edge and must not brighten under the
/// words it happens to pass beneath.
fn seal_band_bottom(row: &mut [RenderCell], ink: [u8; 3]) {
    for cell in row.iter_mut() {
        cell.underline = UnderlineStyle::Single;
        cell.underline_color = Some(ink);
    }
}

/// How strongly the divider between stacked band rows shows the seam's ink
/// over the band (design ruling 260): a hairline that separates, softer than
/// the seam that closes the stack.
pub const DIVIDER_ALPHA: f32 = 0.35;

/// CLOSE THE COMPOSED CHROME STACK against the terminal: the LAST row carries the
/// seam (the private `seal_band_bottom`, in `inks.label`), every row above it carries a
/// divider and no seam. `forced` says an OS-forced palette is up (a host's High
/// Contrast latch): the divider is then the seam's own ink.
///
/// THE SEAM BELONGS TO THE STACK, NOT TO THE PAINTER'S LAST ROW. The compose
/// PADS the cache with [`blank_band_row`] when the geometry has committed more
/// rows than are on the glass (every dismissal or retirement, for the
/// `SHRINK_QUIET` 1.5 s before the count follows — and for as long as a handoff
/// freeze holds it), and TRIMS it when the count is below it (the handoff
/// successor); a presence row may sit above it with no band row after it.
/// Sealing the stack after it is composed covers the painted, padded, trimmed
/// and presence-only shapes with one rule. No band or presence cell uses
/// underline for anything else, so clearing it above the last row erases
/// nothing but a stale seam.
pub fn seal_stack(rows: &mut [Vec<RenderCell>], inks: &BandInks, forced: bool) {
    let Some((last, above)) = rows.split_last_mut() else {
        return;
    };
    // STACKED ROWS STAY APART (design ruling 260): every row above the last
    // carries a 1 px DIVIDER, the seam's ink at [`DIVIDER_ALPHA`] over the
    // band — two rows of the same ground no longer run together into one
    // slab. A rail lit in a row's lowest pixels takes its place there, as it
    // takes the seam's ([`give_seam_to_rails`]).
    // Under an OS-forced palette every ink is a system colour: the divider is
    // the seam's own.
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

/// A RAIL (the strain gauge, ruling 243) runs in its row's lowest pixels,
/// where the stack's closing seam runs too when the row is the last: where
/// the rail is lit it IS the row's lower edge, so the seam gives way to it
/// cell by cell (a cell at least half lit). `cells` are the composed rows
/// (`rasters` keyed by their row), `cols` cells of `cell_w` px starting `pad`
/// px into the frame.
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

/// GIVE EACH OUTLINED CAPSULE ITS FLOOR (ruling 254): the cells between a
/// ring's two rounded ends ([`chrome_ring_floor_cols`]) carry a single
/// underline in the ring's colour, on whichever row the ring sits. The ring
/// builder stands the pill on the underline's rows and leaves its straight
/// floor to these cells, so the renderer's descender ink-skip carves the
/// floor round a `p` exactly as it carves the seam. On the stack's LAST row
/// the seam hands its two end cells to the ring ([`ChromeRing::seam`]): the
/// builder draws the seam there itself, meeting the pill's rounded foot
/// antialiased, and the pill's floor carries the line on in the ring's
/// colour — one edge, and no seam pixel crosses the pill's inside. Run after
/// [`seal_stack`] (and after the seam gives way to a lit rail), each frame the
/// stack is composed; `rasters` are the frame's chrome rasters, keyed by their
/// row. A band with no outlined capsule (every web row: no capsules there)
/// leaves the rows as they are.
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

/// The chrome bleed's per-row gutter tones for the message band's METERED
/// rows (ruling 55): band row `i` is chrome row `first + i`, and only the
/// rows the geometry has committed (`committed`) carry any — a cached row the
/// geometry has not committed is not on glass. Packed `0x00RRGGBB`, at most
/// [`CHROME_ROW_EDGES`] (the band's three rows).
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

/// The chrome bleed for `bars` band rows (the presence row counted) under
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
}

/// SPLICE THE BAND onto the just-composed frame `dst`: the painted `rows`
/// (and their `rasters`) trimmed or padded to the `committed` count the
/// geometry owns, under an optional row `above` them (a host's presence
/// row), prepended above the grid ([`RenderInput::prepend_host_rows`] with
/// `pixels`), each raster placed at its composed row ([`on_frame`]), the stack
/// sealed ([`seal_stack`]), the seam given to a lit rail
/// (the private `give_seam_to_rails`) and each outlined capsule floored
/// ([`floor_rings`]). Returns how many rows it prepended (`0`: nothing to
/// compose, `dst` untouched).
///
/// SYMMETRIC: over-supply is trimmed, and UNDER-supply is padded with
/// [`blank_band_row`]. The window was SIZED for `committed` rows; composing
/// fewer makes every later row land one short, so the terminal's own top row
/// is drawn under the band — the row the geometry owns, with nothing in it, is
/// the honest picture.
#[allow(
    clippy::too_many_arguments,
    reason = "the one compose both hosts call: every input is a plain value of the frame it composes"
)]
pub fn compose_band(
    dst: &mut RenderInput,
    above: Option<&[RenderCell]>,
    rows: &[Vec<RenderCell>],
    rasters: &[Option<RowRaster>],
    committed: usize,
    inks: &BandInks,
    forced: bool,
    at: BandFrame,
    pool: &mut Vec<Vec<RenderCell>>,
    pixels: HostRowPixels,
) -> usize {
    if committed == 0 && above.is_none() {
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
    let composed = usize::from(above.is_some()) + rows.len();
    // The band owns the frame's pixel-resolution chrome rows: none carry
    // over from the scratch's last use.
    dst.chrome_rasters.clear();
    dst.prepend_host_rows(
        composed,
        above.into_iter().chain(rows.iter().map(Vec::as_slice)),
        at.cell_h,
        at.grid_top,
        pool,
        pixels,
    );
    // THE METER AT PIXEL RESOLUTION (ruling 242): each painted band row's
    // raster, placed on the frame (its column 0 is `lo` window pixels in)
    // at the row's composed index — a strip prepended later shifts it with
    // every other per-row channel.
    let above_n = usize::from(above.is_some());
    for (i, raster) in rasters.iter().take(rows.len()).enumerate() {
        if let Some(r) = raster
            && let Ok(row) = u16::try_from(above_n + i)
        {
            dst.chrome_rasters
                .push(on_frame(r, row, at.lo, at.frame_w, at.cell_h));
        }
    }
    // The closing seam goes on whichever row the COMPOSED stack ends with — a
    // painted row, a padded one, or the row above alone — never on the
    // painter's last row, which the pad and trim above can bury or cut off.
    // Rows `0..composed` are exactly this stack: a strip is prepended later.
    seal_stack(&mut dst.cells[..composed], inks, forced);
    give_seam_to_rails(
        &mut dst.cells[..composed],
        &dst.chrome_rasters,
        at.cols,
        at.pad,
        at.cell_w,
    );
    // An outlined capsule (ruling 249) stands on the row's underline and
    // its floor IS its cells' underline, in the ring's colour (ruling
    // 254): on the last row the seam runs up to the pill's foot (the
    // ring draws it across its end cells) and the floor carries it on,
    // instead of cutting through the pill's inside.
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

    /// Every `char` the renderer draws as a band icon is the engine's icon of
    /// that char, and no other char is an engine icon (ruling 322): the two
    /// tables this map joins, held together where both are linked.
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

    /// A frame with no committed row and nothing above it is not touched; a
    /// short cache is PADDED to the committed count and a long one TRIMMED,
    /// and only the composed stack's last row carries the seam, the rows above
    /// it the divider.
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
                None,
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
            None,
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
            None,
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
