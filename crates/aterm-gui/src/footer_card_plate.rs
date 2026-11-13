// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The frame a footer card wears, drawn in pixels under its cells
//! (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4): a rounded line round the card's
//! cell rect in the static arc of the glass it wears ([`GlassInks::border_at`]), a drop shadow
//! round its outline, the card's ground inside the line (opaque, so a card reads the same on a
//! wallpaper or a see-through window as on a plain one), a hairline on each rule row, and the
//! pills a row stands on ([`PlatePill`]): a slider's track and a warn note's wash.
//!
//! The card's words, marks and grounds stay cells the grid draws ([`crate::autopilot_menu`]).
//! The frame is one retained fx layer under their glyphs ([`plate_layer`]): four strips rastered
//! once per geometry ([`plate_frame`]), the ground between them and the rules as rects and the
//! pills as rounded bars ([`plate_content`]), kept beside the open card ([`CardPlate`]) so a
//! card at rest builds
//! nothing. The wash of the row under the pointer or the keys is a second layer over it
//! ([`CardWash`]), so a hover seals a pill's few items and rasters nothing.

use std::sync::Arc;

use aterm_core::render::{
    FX_ART_MAX_SIDE, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace,
    FxSprite, FxTexels, PxRect,
};
use aterm_effects::glass_inks::{CELL_ASPECT, Edge, GlassInks, perimeter_at};
use aterm_render::rounded_bar::{CapArts, RoundedBar};

use crate::tray_raster::RasterRect;
use crate::widget::{DrawPrim, rgba};

#[cfg(test)]
#[path = "footer_card_plate_tests.rs"]
pub(crate) mod tests;

/// The slices a cell of the card's edge is cut into along the arc: each is stroked in the
/// arc's ink at its own middle, so the hue runs smoothly through a cell.
const ARC_STEPS: usize = 4;

/// The most points a card's corner radius takes, as a list overlay's
/// ([`crate::palette::overlay_card_frame`]).
const RADIUS_MAX_PT: f32 = 14.0;

/// The share of a cell's height a card's corner radius takes where that is less: inside the
/// card's blank first row.
const RADIUS_OF_CELL: f32 = 0.6;

/// The points a card's shadow reaches past its sides and below it
/// ([`crate::palette::overlay_card_shadow`]).
const SHADOW_SIDE_PT: f32 = 3.0;
const SHADOW_BELOW_PT: f32 = 8.0;

/// The points a slider's track is tall ([`PillShape::Track`]).
const TRACK_PT: f32 = 2.0;

/// How the frame's line is inked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlateLine {
    /// The static arc of the glass the card wears ([`GlassInks::border_at`]).
    Arc(GlassInks),
    /// One ink all round (`0x00RRGGBB`): a card with no palette, Serious Mode, a forced palette.
    Flat(u32),
}

/// What a card's frame is rastered from: a frame of an equal key is the same pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlateKey {
    /// The card's columns and rows, in cells.
    pub(crate) cells: (u16, u16),
    /// A cell's width and height, in device pixels.
    pub(crate) cell: (u16, u16),
    /// The device pixels a point takes, in 64ths.
    pub(crate) pt64: u16,
    /// The card's ground (`0x00RRGGBB`).
    pub(crate) ground: u32,
    /// The line's ink.
    pub(crate) line: PlateLine,
    /// Whether the card casts a shadow: not under a forced palette.
    pub(crate) shadow: bool,
}

impl PlateKey {
    /// The key of a card `cells` (columns, rows) of `cell` device pixels at `scale` device
    /// pixels a point.
    #[must_use]
    pub(crate) fn new(
        cells: (usize, usize),
        cell: (usize, usize),
        scale: f64,
        ground: u32,
        line: PlateLine,
        shadow: bool,
    ) -> Self {
        let short = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
        let scale = if scale.is_finite() { scale } else { 1.0 };
        Self {
            cells: (short(cells.0), short(cells.1)),
            cell: (short(cell.0), short(cell.1)),
            pt64: (scale.clamp(0.25, 16.0) * 64.0).round() as u16,
            ground: ground & 0x00FF_FFFF,
            line,
            shadow,
        }
    }

    /// The device pixels a point takes.
    fn pt(&self) -> f32 {
        f32::from(self.pt64) / 64.0
    }

    /// The card's width and height, in device pixels.
    pub(crate) fn size(&self) -> (i32, i32) {
        (
            i32::from(self.cells.0) * i32::from(self.cell.0),
            i32::from(self.cells.1) * i32::from(self.cell.1),
        )
    }

    /// The line's width, in whole device pixels: one point, a pixel at least.
    pub(crate) fn line_px(&self) -> i32 {
        (self.pt().round() as i32).max(1)
    }

    /// The corner radius, in device pixels ([`RADIUS_OF_CELL`], [`RADIUS_MAX_PT`]), never past
    /// half the card.
    fn radius(&self) -> f32 {
        let (w, h) = self.size();
        (f32::from(self.cell.1) * RADIUS_OF_CELL)
            .min(RADIUS_MAX_PT * self.pt())
            .min(w.min(h) as f32 / 2.0)
    }

    /// The height of the top and of the bottom strip: a cell row, half the card at most.
    fn band(&self) -> i32 {
        i32::from(self.cell.1).min(self.size().1 / 2)
    }

    /// The device pixels the shadow may reach past the card's sides and below it, one clear
    /// pixel beyond its last; none with no shadow.
    pub(crate) fn margin(&self) -> (i32, i32) {
        if !self.shadow {
            return (0, 0);
        }
        let reach = |pt: f32| (pt * self.pt()).ceil() as i32 + 1;
        (reach(SHADOW_SIDE_PT), reach(SHADOW_BELOW_PT))
    }

    /// The pixels the frame may write, from the card's top left corner: the card and its
    /// shadow's margin.
    pub(crate) fn extent(&self) -> PxRect {
        let ((w, h), (side, below)) = (self.size(), self.margin());
        PxRect::new(-side, 0, w + side, h + below)
    }
}

/// One slice of the line: its clip, from the card's top left corner in device pixels
/// (`x`, `y`, `w`, `h`), and its ink.
type Slice = ((i32, i32, i32, i32), u32);

/// The line's slices round a card of `key` under `inks`, clockwise from the top left corner
/// as the arc runs ([`perimeter_at`], a cell [`CELL_ASPECT`] widths tall): the top and the
/// bottom edge cut by column, `deep` pixels of the card's first and of its last rows; the sides
/// between them cut by row, `wide` pixels in from each edge; [`ARC_STEPS`] to a cell, each on
/// whole device pixels and in the arc's ink at its own middle.
fn arc_slices(key: &PlateKey, inks: &GlassInks, deep: i32, wide: i32) -> Vec<Slice> {
    let (cols, rows) = (usize::from(key.cells.0), usize::from(key.cells.1));
    let (cw, ch) = (usize::from(key.cell.0), usize::from(key.cell.1));
    let (w, h) = key.size();
    let (across, down) = (cols as f32, rows as f32 * CELL_ASPECT);
    let ink = |edge: Edge, at: f32| inks.border_at(perimeter_at(edge, at, across, down));
    let px = |v: usize| i32::try_from(v).unwrap_or(i32::MAX);
    let mut out = Vec::with_capacity(2 * ARC_STEPS * (cols + rows));
    let cut = |steps: usize, size: usize| {
        (0..steps * ARC_STEPS)
            .map(move |k| (k * size / ARC_STEPS, (k + 1) * size / ARC_STEPS))
            .filter(|(a, b)| b > a)
    };
    for (a, b) in cut(cols, cw) {
        let x = (a + b) as f32 / (2.0 * cw.max(1) as f32);
        out.push(((px(a), 0, px(b - a), deep), ink(Edge::Top, x)));
        out.push(((px(a), h - deep, px(b - a), deep), ink(Edge::Bottom, x)));
    }
    for (a, b) in cut(rows, ch) {
        let (top, bottom) = (px(a).max(deep), px(b).min(h - deep));
        if bottom <= top {
            continue;
        }
        let y = (a + b) as f32 / (2.0 * ch.max(1) as f32) * CELL_ASPECT;
        out.push(((w - wide, top, wide, bottom - top), ink(Edge::Right, y)));
        out.push(((0, top, wide, bottom - top), ink(Edge::Left, y)));
    }
    out
}

/// The slices the rastered line is stroked in ([`arc_slices`]): the card's first and last
/// row deep, a cell wide at the sides, wide enough to hold a corner's arc.
fn raster_slices(key: &PlateKey, inks: &GlassInks) -> Vec<Slice> {
    let side = i32::from(key.cell.0)
        .min(key.size().0 / 2)
        .max(key.line_px());
    arc_slices(key, inks, key.band(), side)
}

/// The frame of `key` as tray prims in device pixels, the card's top left corner at
/// (`key.margin().0`, 0): the shadow, whole; over it the card's ground inside the line,
/// opaque; and the line, one point wide inside the rect's edge, stroked slice by slice in the
/// arc's inks ([`arc_slices`]) or whole in a flat one. So the shadow shows wherever the card's
/// rounded outline does not cover it, the wedge between a corner's arc and the rect's corner
/// included, as a list overlay's does ([`crate::palette::overlay_card_frame`]); it never
/// reaches the rect's two top corners, which stay clear.
#[must_use]
pub(crate) fn plate_prims(key: &PlateKey) -> Vec<DrawPrim> {
    let ((w, h), (side, _)) = (key.size(), key.margin());
    let (x0, wf, hf) = (side as f32, w as f32, h as f32);
    let (pt, radius, line) = (key.pt(), key.radius(), key.line_px() as f32);
    let mut prims = Vec::new();
    if key.shadow {
        prims.extend(crate::palette::overlay_card_shadow(
            (x0, 0.0, wf, hf),
            pt,
            radius,
            true,
        ));
    }
    // The rect the line is centred on: the card's, inset by half the line.
    let (ix, iy, iw, ih) = (x0 + line / 2.0, line / 2.0, wf - line, hf - line);
    let inner = (radius - line / 2.0).max(0.0);
    prims.push(DrawPrim::Panel {
        x: ix,
        y: iy,
        w: iw,
        h: ih,
        radius: inner,
        fill: rgba(unpack(key.ground), 0xFF),
    });
    let stroke = |ink: u32| DrawPrim::Stroke {
        x: ix,
        y: iy,
        w: iw,
        h: ih,
        radius: inner,
        width: line,
        color: rgba(unpack(ink), 0xFF),
    };
    match key.line {
        PlateLine::Flat(ink) => prims.push(stroke(ink)),
        PlateLine::Arc(inks) => {
            for ((x, y, w, h), ink) in raster_slices(key, &inks) {
                prims.push(DrawPrim::ClipPush {
                    x: x0 + x as f32,
                    y: y as f32,
                    w: w as f32,
                    h: h as f32,
                });
                prims.push(stroke(ink));
                prims.push(DrawPrim::ClipPop);
            }
        }
    }
    prims
}

const fn unpack(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// A card's frame as fx items: its strips' sprites, or the straight line's rects, and the
/// palette those rects read.
#[derive(Clone, Debug, Default)]
pub(crate) struct PlateFrame {
    items: Vec<FxItem>,
    palette: Vec<FxInk>,
}

impl PlateFrame {
    /// The frame's items, from the card's top left corner.
    #[cfg(test)]
    pub(crate) fn items(&self) -> &[FxItem] {
        &self.items
    }
}

/// The four strips a frame is rastered in, from the card's top left corner (`x`, `y`, `w`,
/// `h`): the top row and the bottom row with the shadow beside and below them, and the two
/// sides between, each the line and the shadow beside it. The card's middle is never rastered:
/// its ground is a rect of the layer's ([`push_body`]).
fn strips(key: &PlateKey) -> [(i32, i32, i32, i32); 4] {
    let ((w, h), (side, below), band) = (key.size(), key.margin(), key.band());
    let edge = key.line_px() + side;
    [
        (-side, 0, w + 2 * side, band),
        (-side, h - band, w + 2 * side, band + below),
        (-side, band, edge, h - 2 * band),
        (w - key.line_px(), band, edge, h - 2 * band),
    ]
}

/// The frame of `key` ([`plate_prims`]) rastered into its strips ([`strips`]), each cut into
/// tiles of [`FX_ART_MAX_SIDE`] at most and interned ([`aterm_render::fx::bitmap`]); a tile
/// that draws nothing is no sprite. Where a tile cannot be built the frame is the straight
/// line instead ([`straight_frame`]).
#[must_use]
pub(crate) fn plate_frame(key: &PlateKey) -> PlateFrame {
    plate_frame_with(key, |w, h, texels| {
        aterm_render::fx::bitmap(FxTexels::Rgba8Premul, w, h, texels)
    })
}

/// [`plate_frame`] with the art builder `art` (a tile's width, height and premultiplied
/// bytes), which a test fails.
fn plate_frame_with(
    key: &PlateKey,
    art: impl Fn(u16, u16, Vec<u8>) -> Option<Arc<FxArt>>,
) -> PlateFrame {
    let (w, h) = key.size();
    if w <= 0 || h <= 0 {
        return PlateFrame::default();
    }
    let prims = plate_prims(key);
    let side = key.margin().0;
    let max = i32::from(FX_ART_MAX_SIDE);
    let mut items = Vec::new();
    for (sx, sy, sw, sh) in strips(key) {
        if sw <= 0 || sh <= 0 {
            continue;
        }
        for ty in (0..sh).step_by(usize::from(FX_ART_MAX_SIDE)) {
            for tx in (0..sw).step_by(usize::from(FX_ART_MAX_SIDE)) {
                let (tw, th) = ((sw - tx).min(max), (sh - ty).min(max));
                let (x, y) = (sx + tx, sy + ty);
                let region = RasterRect {
                    x: (x + side).unsigned_abs(),
                    y: y.unsigned_abs(),
                    width: tw.unsigned_abs(),
                    height: th.unsigned_abs(),
                };
                let mut texels =
                    crate::tray_raster::rasterize_tray_region(&prims, 1.0, [0, 0, 0, 0], region);
                if !premultiply(&mut texels) {
                    continue;
                }
                let (dw, dh) = (tw as u16, th as u16);
                let Some(art) = art(dw, dh, texels) else {
                    return straight_frame(key);
                };
                items.push(FxItem::Sprite(FxSprite {
                    x,
                    y,
                    dw,
                    dh,
                    flip_x: false,
                    art,
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                }));
            }
        }
    }
    PlateFrame {
        items,
        palette: Vec::new(),
    }
}

/// Premultiply straight `R, G, B, A` texels in place, returning whether any of them draws.
fn premultiply(texels: &mut [u8]) -> bool {
    let mut draws = false;
    for px in texels.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        if a == 0 {
            px[..3].fill(0);
            continue;
        }
        draws = true;
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * a + 127) / 255) as u8;
            }
        }
    }
    draws
}

/// The frame of `key` with no art: a straight line one point wide inside the rect's edge, in
/// the arc's inks slice by slice or whole in a flat one, with square corners and no shadow,
/// and the card's ground on its first and its last row inside the line.
fn straight_frame(key: &PlateKey) -> PlateFrame {
    let ((w, h), line, band) = (key.size(), key.line_px(), key.band());
    let mut frame = PlateFrame::default();
    let mut rect = |(x, y, w, h): (i32, i32, i32, i32), ink: u32| {
        let (Ok(w), Ok(h)) = (u16::try_from(w), u16::try_from(h)) else {
            return;
        };
        if w == 0 || h == 0 {
            return;
        }
        let base = u32::try_from(frame.palette.len()).unwrap_or(u32::MAX);
        frame.palette.extend([FxInk::over(ink, 255); 2]);
        frame.items.push(FxItem::Rect(FxRect {
            x,
            y,
            w,
            h,
            ink: base,
        }));
    };
    rect((line, line, w - 2 * line, band - line), key.ground);
    rect((line, h - band, w - 2 * line, band - line), key.ground);
    match key.line {
        PlateLine::Flat(ink) => {
            rect((0, 0, w, line), ink);
            rect((0, h - line, w, line), ink);
            rect((0, line, line, h - 2 * line), ink);
            rect((w - line, line, line, h - 2 * line), ink);
        }
        PlateLine::Arc(inks) => {
            // Each slice is its own rect: the edges a line deep, the sides a line wide and
            // whole between them.
            for (slice, ink) in arc_slices(key, &inks, line, line) {
                rect(slice, ink);
            }
        }
    }
    frame
}

/// What a card lays inside its frame ([`plate_content`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PlateLay {
    /// The hairlines.
    pub(crate) rules: Vec<PlateRule>,
    /// The pills no pointer moves.
    pub(crate) pills: Vec<PlatePill>,
    /// The cells that stand on a ground of their own (a drawn row's warn and busy cells), a
    /// row's in column order: the card's ground is not drawn over them.
    pub(crate) own: Vec<PlateRule>,
}

impl PlateLay {
    fn clear(&mut self) {
        self.rules.clear();
        self.pills.clear();
        self.own.clear();
    }
}

/// A span of a card's row, a hairline's or that of cells on a ground of their own: the row,
/// counted from the card's first, and the columns it spans, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlateRule {
    pub(crate) row: u16,
    pub(crate) from: u16,
    pub(crate) to: u16,
}

/// How tall a pill stands in its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PillShape {
    /// The row's height less a point above and below: the wash of a row or of a slider's stop
    /// under the pointer or the keys, and a warn note's.
    Wash,
    /// [`TRACK_PT`] at the row's middle: a slider's track.
    Track,
}

/// A pill on a card's row, its ends rounded to half its height: the row, counted from the
/// card's first, the half cells it runs from and to, counted from the card's left edge, and
/// its colour at `alpha`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlatePill {
    pub(crate) row: u16,
    pub(crate) from: u16,
    pub(crate) to: u16,
    pub(crate) shape: PillShape,
    pub(crate) rgb: [u8; 3],
    pub(crate) alpha: u8,
}

impl PlateKey {
    /// `pill` as a bar in device pixels from the card's top left corner; `None` for one of no
    /// width, height or opacity.
    #[must_use]
    pub(crate) fn pill_bar(&self, pill: &PlatePill) -> Option<RoundedBar> {
        let (cw, ch) = (u32::from(self.cell.0), u32::from(self.cell.1));
        let (x0, x1) = (u32::from(pill.from) * cw / 2, u32::from(pill.to) * cw / 2);
        let h = match pill.shape {
            PillShape::Wash => {
                ch.saturating_sub(2 * self.line_px().unsigned_abs().min(ch.saturating_sub(1) / 2))
            }
            PillShape::Track => ((TRACK_PT * self.pt()).round() as u32).max(2).min(ch),
        };
        (x1 > x0 && h > 0 && pill.alpha > 0).then(|| RoundedBar {
            x: x0,
            y: u32::from(pill.row) * ch + (ch - h) / 2,
            w: x1 - x0,
            h,
            rgb: pill.rgb,
            alpha: pill.alpha,
        })
    }
}

/// Append `pills` on a card of `key` to `items` and their inks to `palette`, each a rounded bar
/// ([`aterm_render::rounded_bar`]) whose cap art `caps` keeps, so a pill that moves or fades
/// builds none. A pill wider than an art may be ([`FX_ART_MAX_SIDE`]) is a square bar.
fn push_pills(
    key: &PlateKey,
    pills: &[PlatePill],
    caps: &mut CapArts,
    items: &mut Vec<FxItem>,
    palette: &mut Vec<FxInk>,
) {
    for bar in pills.iter().filter_map(|pill| key.pill_bar(pill)) {
        if bar.w <= u32::from(FX_ART_MAX_SIDE) {
            aterm_render::rounded_bar::push_items(bar, caps, items, palette);
            continue;
        }
        let pre = |c: u8| (u32::from(c) * u32::from(bar.alpha) + 127) / 255;
        let ink = FxInk::over(
            (pre(bar.rgb[0]) << 16) | (pre(bar.rgb[1]) << 8) | pre(bar.rgb[2]),
            bar.alpha,
        );
        let base = u32::try_from(palette.len()).unwrap_or(u32::MAX);
        palette.extend([ink; 2]);
        items.push(FxItem::Rect(FxRect {
            x: i32::try_from(bar.x).unwrap_or(i32::MAX),
            y: i32::try_from(bar.y).unwrap_or(i32::MAX),
            w: u16::try_from(bar.w).unwrap_or(u16::MAX),
            h: u16::try_from(bar.h).unwrap_or(u16::MAX),
            ink: base,
        }));
    }
}

/// Append the card's ground between its first and its last row, inside the line, to `items`
/// and its ink to `palette`: opaque rects of the ground, whole rows together, cut round each
/// span of `own`. The cells there have the pane's own ground, which a renderer leaves clear
/// over a wallpaper and see-through in a translucent window; the rects make the card's body as
/// opaque as its first and last row, which the strips draw ([`plate_prims`]).
fn push_body(key: &PlateKey, own: &[PlateRule], items: &mut Vec<FxItem>, palette: &mut Vec<FxInk>) {
    let (rows, (cw, ch)) = (key.cells.1, (i32::from(key.cell.0), i32::from(key.cell.1)));
    let ((w, _), line) = (key.size(), key.line_px());
    if rows < 3 || w <= 2 * line {
        return;
    }
    let base = u32::try_from(palette.len()).unwrap_or(u32::MAX);
    let before = items.len();
    let mut rect = |x: (i32, i32), y: (i32, i32)| {
        if x.1 > x.0 && y.1 > y.0 {
            items.push(FxItem::Rect(FxRect {
                x: x.0,
                y: y.0,
                w: u16::try_from(x.1 - x.0).unwrap_or(u16::MAX),
                h: u16::try_from(y.1 - y.0).unwrap_or(u16::MAX),
                ink: base,
            }));
        }
    };
    let across = (line, w - line);
    // The first row of the whole rows not yet drawn.
    let mut whole = 1;
    for r in 1..rows - 1 {
        if !own.iter().any(|span| span.row == r) {
            continue;
        }
        let y = (i32::from(r) * ch, i32::from(r + 1) * ch);
        rect(across, (i32::from(whole) * ch, y.0));
        whole = r + 1;
        let mut x = across.0;
        for span in own.iter().filter(|span| span.row == r) {
            rect((x, (i32::from(span.from) * cw).min(across.1)), y);
            x = x.max(i32::from(span.to) * cw);
        }
        rect((x, across.1), y);
    }
    rect(across, (i32::from(whole) * ch, i32::from(rows - 1) * ch));
    if items.len() > before {
        palette.extend([FxInk::over(key.ground, 255); 2]);
    }
}

/// The layer content of `frame` on a card of `key`: the card's ground between its first and its
/// last row ([`push_body`]), cut round `lay.own`; a hairline of `ink` on each of `lay.rules`,
/// one line's width tall ([`PlateKey::line_px`]) at the middle of its row, over the columns it
/// spans; then each of `lay.pills` ([`push_pills`]). `None` where nothing is drawn.
#[must_use]
pub(crate) fn plate_content(
    key: &PlateKey,
    frame: &PlateFrame,
    lay: &PlateLay,
    ink: u32,
    caps: &mut CapArts,
) -> Option<Arc<FxContent>> {
    let mut items = frame.items.clone();
    let mut palette = frame.palette.clone();
    push_body(key, &lay.own, &mut items, &mut palette);
    let (base, before) = (
        u32::try_from(palette.len()).unwrap_or(u32::MAX),
        items.len(),
    );
    let (cw, ch) = (i32::from(key.cell.0), i32::from(key.cell.1));
    let line = key.line_px().min(ch);
    for rule in lay.rules.iter().filter(|rule| rule.to > rule.from) {
        let Ok(w) = u16::try_from(i32::from(rule.to - rule.from) * cw) else {
            continue;
        };
        items.push(FxItem::Rect(FxRect {
            x: i32::from(rule.from) * cw,
            y: i32::from(rule.row) * ch + (ch - line) / 2,
            w,
            h: line as u16,
            ink: base,
        }));
    }
    if items.len() > before {
        palette.extend([FxInk::over(ink & 0x00FF_FFFF, 255); 2]);
    }
    push_pills(key, &lay.pills, caps, &mut items, &mut palette);
    (!items.is_empty()).then(|| Arc::new(FxContent::new(items, palette)))
}

/// The layer that draws `content` under the glyphs of a card whose top left corner is at
/// `origin` (grid pixels), inside `clip`, standing on the card's own `chrome`
/// ([`FxLayer::chrome`]): the occluder the card pushed cuts every other layer and not this one.
#[must_use]
pub(crate) fn plate_layer(
    id: u64,
    content: Arc<FxContent>,
    origin: (i32, i32),
    clip: PxRect,
    chrome: Option<PxRect>,
) -> FxLayer {
    let mut layer = FxLayer::new(id, content, FxSlot::Under, FxSpace::Grid, FxBlend::Exact);
    layer.offset = origin;
    layer.clip = clip;
    layer.chrome = chrome;
    layer
}

/// An open card's frame as the last frame drew it: the strips rastered for its key and the
/// content sealed of them and of what the card lays inside ([`PlateLay`]), handed on again
/// while none of them moves, so a card at rest rasters, seals and allocates nothing.
#[derive(Clone, Debug, Default)]
pub(crate) struct CardPlate {
    key: Option<(PlateKey, u32)>,
    frame: PlateFrame,
    /// What this frame lays, refilled in place, and what the content draws.
    lay: PlateLay,
    drawn: PlateLay,
    caps: CapArts,
    content: Option<Arc<FxContent>>,
}

impl CardPlate {
    /// The content of the frame of `key` with what `lay` writes inside it, its hairlines in
    /// `ink`: the last frame's where the key, the ink and what is laid are unchanged.
    pub(crate) fn content(
        &mut self,
        key: PlateKey,
        ink: u32,
        lay: impl FnOnce(&mut PlateLay),
    ) -> Option<Arc<FxContent>> {
        self.lay.clear();
        lay(&mut self.lay);
        let framed = self.key.is_some_and(|(was, _)| was == key);
        if !framed {
            self.frame = plate_frame(&key);
        }
        if !framed || self.key != Some((key, ink)) || self.lay != self.drawn {
            self.content = plate_content(&key, &self.frame, &self.lay, ink, &mut self.caps);
            self.drawn.clone_from(&self.lay);
        }
        self.key = Some((key, ink));
        self.content.clone()
    }
}

/// The wash of an open card's row under the pointer or the keys, as the last frame drew it:
/// the pills of its own layer over the frame's ([`plate_layer`]), sealed again only where one
/// moved or faded, its cap arts kept, so a hover rasters nothing and a card at rest seals
/// nothing.
#[derive(Clone, Debug, Default)]
pub(crate) struct CardWash {
    cell: Option<((u16, u16), u16)>,
    /// This frame's pills, refilled in place.
    pills: Vec<PlatePill>,
    /// The pills the content draws.
    drawn: Vec<PlatePill>,
    caps: CapArts,
    content: Option<Arc<FxContent>>,
}

impl CardWash {
    /// The content of the pills `lay` writes on a card of `key`: the last frame's where the
    /// cell, the scale and the pills are unchanged; `None` with no pill.
    pub(crate) fn content(
        &mut self,
        key: &PlateKey,
        lay: impl FnOnce(&mut Vec<PlatePill>),
    ) -> Option<Arc<FxContent>> {
        self.pills.clear();
        lay(&mut self.pills);
        let cell = Some((key.cell, key.pt64));
        if self.cell != cell || self.pills != self.drawn {
            let (mut items, mut palette) = (Vec::new(), Vec::new());
            push_pills(key, &self.pills, &mut self.caps, &mut items, &mut palette);
            self.content = (!items.is_empty()).then(|| Arc::new(FxContent::new(items, palette)));
            self.drawn.clone_from(&self.pills);
            self.cell = cell;
        }
        self.content.clone()
    }
}
