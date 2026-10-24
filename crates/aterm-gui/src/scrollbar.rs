// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The overlay scrollbar (`scrollbar = true`, the Windows default): a Fluent panning indicator in each terminal pane's right gutter.

use aterm_render::GlowQuad;

/// Thumb opacity while the view rests in history (out of 255).
pub(crate) const ALPHA_REST: u8 = 92;
/// Thumb opacity while the pane is scrolling.
pub(crate) const ALPHA_ACTIVE: u8 = 132;
/// Thumb opacity under the pointer.
pub(crate) const ALPHA_HOVER: u8 = 156;
/// Thumb opacity while dragged.
pub(crate) const ALPHA_DRAG: u8 = 196;
/// Track opacity under the pointer.
pub(crate) const ALPHA_TRACK: u8 = 22;

/// The bar's sizes for one cell geometry, in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Metrics {
    /// The resting and scrolling thumb's width (the Classic chrome).
    pub thin: u32,
    /// The resting and scrolling thumb's width under the Fluent chrome: WinUI's panning indicator (`ScrollBar`'s `VerticalPanningThumb`, `Width="2"`), two device pixels at the 9 px Windows cell, three at 150%, four at 200%.
    pub fine: u32,
    /// The thumb's width under the pointer or while dragged.
    pub wide: u32,
    /// The gap kept between the thumb and the window edge.
    pub margin: u32,
    /// The gap kept above and below the track.
    pub inset: u32,
    /// The shortest thumb.
    pub min_len: u32,
    /// The divider hairline's thickness: the renderer's light box-drawing stroke for this cell ([`hairline_stroke`]), so a thumb or a focus line placed against the hairline lands on the pixels the `│` glyph draws.
    pub stroke: u32,
}

impl Metrics {
    /// Sizes scaled from the cell, so a 2x display draws a 2x bar: at the 9x23 Windows default a 3 px thumb (2 px under the Fluent chrome) that widens to 6 px, 2 px from the edge.
    #[must_use]
    pub(crate) fn for_cell(cell_w: u32, cell_h: u32) -> Self {
        let unit = (cell_w / 9).max(1);
        Metrics {
            thin: (cell_w / 3).clamp(2, 6),
            fine: (cell_w.saturating_mul(2).saturating_add(4) / 9).max(1),
            wide: (cell_w * 2 / 3).clamp(4, 14),
            margin: 2 * unit,
            inset: 2 * unit,
            min_len: cell_h.max(16),
            stroke: hairline_stroke(cell_w, cell_h),
        }
    }
}

/// The light box-drawing stroke of a `cell_w × cell_h` cell, in device pixels: `max(1, (min(w, h) + 4) / 8)`, `aterm_render`'s procedural rounding rule.
#[must_use]
pub(crate) fn hairline_stroke(cell_w: u32, cell_h: u32) -> u32 {
    (cell_w.min(cell_h).saturating_add(4) / 8).max(1)
}

/// Where a light `│` (or `─`) of `stroke` px starts across a cell `extent` px wide (or tall): `(extent − stroke) / 2`, the extra pixel of an odd remainder going right (down), as the renderer places it.
#[must_use]
pub(crate) const fn hairline_start(extent: u32, stroke: u32) -> u32 {
    extent.saturating_sub(stroke) / 2
}

/// Where one pane's bar may draw, in terminal-grid pixels (the grid's own origin, chrome rows above it excluded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Lane {
    /// The pixel column the gutter ends at (exclusive): the window's right edge, or the divider gap's right edge.
    pub right: u32,
    /// How much room the gutter has left of `right`: the right padding, or the whole divider gap (one cell), whose hairline the bar is centred on.
    pub room: u32,
    /// The track's top and length.
    pub top: u32,
    pub len: u32,
    /// The lane is the window's own padding (pages on a track press), not a divider gap (which keeps the rest of its width for resizing).
    pub edge: bool,
}

/// The lane of the pane whose content occupies cells `(row, col, rows, cols)` in a `win_cols`-wide grid, or `None` where there is no gutter to draw in.
#[must_use]
pub(crate) fn lane(
    (row, col, rows, cols): (usize, usize, usize, usize),
    win_cols: usize,
    (cell_w, cell_h): (usize, usize),
    pad: usize,
    m: Metrics,
) -> Option<Lane> {
    let px = |v: usize| u32::try_from(v).ok();
    let x0 = px((col + cols).checked_mul(cell_w)?)?;
    let edge = col + cols >= win_cols;
    let (right, room) = if edge {
        (x0.checked_add(px(pad)?)?, px(pad)?)
    } else {
        let gap = px(cell_w)?;
        (x0.checked_add(gap)?, gap)
    };
    let top = px(row.checked_mul(cell_h)?)?.checked_add(m.inset)?;
    let len = px(rows.checked_mul(cell_h)?)?.checked_sub(2 * m.inset)?;
    (room > m.margin && len > 0).then_some(Lane {
        right,
        room,
        top,
        len,
        edge,
    })
}

/// The bar's horizontal span `(x, w)` in `lane` at `width`.
#[must_use]
pub(crate) fn span(lane: Lane, width: u32, m: Metrics) -> (u32, u32) {
    if lane.edge {
        let fits = lane.room.saturating_sub(m.margin).max(1);
        let w = width.min(fits);
        let right = lane.right.saturating_sub(m.margin);
        return (right.saturating_sub(w), w);
    }
    let gap_x = lane.right.saturating_sub(lane.room);
    let w = width.clamp(1, lane.room.max(1));
    let line = gap_x + hairline_start(lane.room, m.stroke);
    let x = (2 * line + m.stroke).saturating_sub(w) / 2;
    (x.clamp(gap_x, lane.right.saturating_sub(w)), w)
}

/// The resting (and scrolling) thumb's width, for [`rest_span`]: under the Fluent chrome (`fluent`, Windows) WinUI's thin panning indicator, [`Metrics::fine`], at the window's edge and in a divider gap alike; under the Classic chrome the [`Metrics::thin`] thumb.
#[must_use]
pub(crate) const fn rest_width(m: Metrics, fluent: bool) -> u32 {
    if fluent { m.fine } else { m.thin }
}

/// The opacity a resting (or scrolling) thumb of `alpha` paints at: under the Fluent chrome twice `alpha` (opaque at most), so the thin indicator carries the ink of WinUI's (its `ControlStrongFillColorDefault`, about 55% white on dark) and stands off the divider's hairline it covers in a gap, where the plain resting opacity laid almost the hairline's own tone; the scrolling and parked steps stay apart.
#[must_use]
pub(crate) const fn rest_alpha(alpha: u8, fluent: bool) -> u8 {
    if fluent {
        alpha.saturating_mul(2)
    } else {
        alpha
    }
}

/// Which side of a divider gap's hairline a focus-ring stroke runs on beside a lane's track (`crate::pane_chrome::focus_ring_rects`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RingSide {
    /// Left of the hairline: the pane on the gap's left has the keyboard.
    Left,
    /// Right of the hairline: the pane on the gap's right has the keyboard.
    Right,
}

/// The resting (or scrolling) thumb's span `(x, w)` in `lane` at `width` ([`rest_width`]): [`span`], except in a divider gap where the focus ring runs beside the hairline (`ring`).
#[must_use]
pub(crate) fn rest_span(lane: Lane, width: u32, m: Metrics, ring: Option<RingSide>) -> (u32, u32) {
    let Some(side) = ring.filter(|_| !lane.edge) else {
        return span(lane, width, m);
    };
    let gap_x = lane.right.saturating_sub(lane.room);
    let w = width.clamp(1, lane.room.max(1));
    let line = gap_x + hairline_start(lane.room, m.stroke);
    let x = match side {
        RingSide::Left => line,
        RingSide::Right => (line + m.stroke).saturating_sub(w),
    };
    (x.clamp(gap_x, lane.right.saturating_sub(w)), w)
}

/// The widened (hovered or dragged) bar's track span `(x, w)` in `lane`: [`span`]'s centred track, `m.wide + 2` wide, except in a divider gap where the focus ring runs beside the hairline (`ring`).
#[must_use]
pub(crate) fn wide_span(lane: Lane, m: Metrics, ring: Option<RingSide>) -> (u32, u32) {
    let width = m.wide + 2;
    let Some(side) = ring.filter(|_| !lane.edge) else {
        return span(lane, width, m);
    };
    let gap_x = lane.right.saturating_sub(lane.room);
    let line = gap_x + hairline_start(lane.room, m.stroke);
    let (from, to) = match side {
        RingSide::Left => (line, lane.right.saturating_sub(m.stroke)),
        RingSide::Right => (gap_x + m.stroke, line + m.stroke),
    };
    let w = to.saturating_sub(from).clamp(1, width);
    match side {
        RingSide::Left => (from, w),
        RingSide::Right => (to.saturating_sub(w), w),
    }
}

/// The quad `(rgb, alpha)` that paints `ink` at `alpha` over `ground` in ONE tone, whatever lies under it (a divider gap's hairline): from `solid` up, the ink precomposed over the ground and drawn opaque; below it, the `solid` tone fading, so a thumb fading out gives the hairline back.
#[must_use]
pub(crate) fn over_ground(ink: [u8; 3], ground: [u8; 3], alpha: u8, solid: u8) -> ([u8; 3], u8) {
    let solid = solid.max(1);
    let mix = |a: u8| {
        let a = u32::from(a);
        std::array::from_fn(|i| {
            ((u32::from(ground[i]) * (255 - a) + u32::from(ink[i]) * a + 127) / 255) as u8
        })
    };
    if alpha >= solid {
        (mix(alpha), 255)
    } else {
        let fade = (u32::from(alpha) * 255 + u32::from(solid) / 2) / u32::from(solid);
        (mix(solid), fade.min(255) as u8)
    }
}

/// The thumb's `(top, len)` on `lane` for a view of `view_rows` over `history` lines scrolled back `offset` lines, or `None` with no history: [`crate::scroll_motion::pill_geometry`]'s proven laws (containment, the length floor, a monotone position with exact ends) over the lane's track.
#[must_use]
pub(crate) fn thumb(
    lane: Lane,
    view_rows: u32,
    history: u32,
    offset: u32,
    m: Metrics,
) -> Option<(u32, u32)> {
    crate::scroll_motion::pill_geometry(lane.len, m.min_len, view_rows, history, offset)
        .map(|(y, len)| (lane.top + y, len))
}

/// The scroll offset that puts the thumb's top at `top` (track pixels from the lane's top, clamped), the inverse of [`thumb`]: the track's top is the top of history, its bottom the live view.
#[must_use]
pub(crate) fn offset_for_top(
    lane: Lane,
    view_rows: u32,
    history: u32,
    top: f64,
    m: Metrics,
) -> u32 {
    let Some((_, len)) = thumb(lane, view_rows, history, 0, m) else {
        return 0;
    };
    let travel = f64::from(lane.len.saturating_sub(len));
    if travel <= 0.0 {
        return 0;
    }
    let along = (top - f64::from(lane.top)).clamp(0.0, travel);
    let from_top = (along * f64::from(history) / travel).round();
    history.saturating_sub(from_top as u32)
}

/// What a pointer at terminal-grid pixel `(x, y)` is over in `lane`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Hit {
    /// The thumb, `grab` pixels below its top.
    Thumb { grab: f64 },
    /// The track above (`true`) or below (`false`) the thumb, in the window's padding only: an inner pane's gap belongs to the divider off the thumb.
    Track { above: bool },
}

/// Hit-test `(x, y)` against `lane` with the thumb at `(top, len)`.
#[must_use]
pub(crate) fn hit(lane: Lane, thumb: (u32, u32), x: f64, y: f64, m: Metrics) -> Option<Hit> {
    let (top, len) = thumb;
    let (left, right) = if lane.edge {
        (
            f64::from(lane.right.saturating_sub(lane.room)),
            f64::from(lane.right),
        )
    } else {
        let (sx, sw) = span(lane, m.wide, m);
        (f64::from(sx), f64::from(sx + sw))
    };
    if x < left || x >= right {
        return None;
    }
    let (t0, t1) = (f64::from(lane.top), f64::from(lane.top + lane.len));
    if y < t0 || y >= t1 {
        return None;
    }
    let (y0, y1) = (f64::from(top), f64::from(top + len));
    if y >= y0 && y < y1 {
        return Some(Hit::Thumb { grab: y - y0 });
    }
    lane.edge.then_some(Hit::Track { above: y < y0 })
}

/// Append a rounded bar `(x, y, w, h)` in frame pixels (grid-interior, the chrome rows included) of `rgb` at `alpha` to `out`, under the fx layer id `id`: its body and its antialiased caps draw as one fx layer of [`aterm_render::rounded_bar`] items ([`crate::pane_chrome`] splices it). A bar of no area or opacity draws nothing.
pub(crate) fn push_bar(
    out: &mut Vec<(u64, aterm_render::rounded_bar::RoundedBar)>,
    id: u64,
    (x, y, w, h): (u32, u32, u32, u32),
    rgb: [u8; 3],
    alpha: u8,
) {
    if w == 0 || h == 0 || alpha == 0 {
        return;
    }
    out.push((
        id,
        aterm_render::rounded_bar::RoundedBar {
            x,
            y,
            w,
            h,
            rgb,
            alpha,
        },
    ));
}

/// The retired path of [`push_bar`], kept as the differential oracle its fx layer is held to byte for byte: a rounded bar `(x, y, w, h)` in frame pixels as source-over [`GlowQuad`]s of `rgb` at `alpha`, the body one quad per cell-row band, the two semicircular caps per pixel row at their antialiased coverage.
#[cfg(test)]
pub(crate) fn legacy_bar_quads(
    out: &mut Vec<GlowQuad>,
    (x, y, w, h): (u32, u32, u32, u32),
    rgb: [u8; 3],
    alpha: u8,
    cell_h: u32,
) {
    if w == 0 || h == 0 || alpha == 0 || cell_h == 0 {
        return;
    }
    let radius = (f64::from(w) / 2.0).min(f64::from(h) / 2.0);
    let cap = (radius.ceil() as u32).min(h / 2);
    let (b0, b1) = (y + cap, y + h - cap);
    let mut row_y = b0;
    while row_y < b1 {
        let band_end = (row_y / cell_h + 1) * cell_h;
        let end = band_end.min(b1);
        push_quad(out, (x, row_y, w, end - row_y), rgb, alpha, cell_h);
        row_y = end;
    }
    let (fx, fy, fw, fh) = (f64::from(x), f64::from(y), f64::from(w), f64::from(h));
    let inside = |px: f64, py: f64| -> bool {
        if px < fx || px >= fx + fw || py < fy || py >= fy + fh {
            return false;
        }
        let cx = px.clamp(fx + radius, fx + fw - radius);
        let cy = py.clamp(fy + radius, fy + fh - radius);
        let (dx, dy) = (px - cx, py - cy);
        dx * dx + dy * dy <= radius * radius
    };
    const SS: u32 = 4;
    let coverage = |px: u32, py: u32| -> u8 {
        let mut hits = 0u32;
        for sy in 0..SS {
            for sx in 0..SS {
                let sample_x = f64::from(px) + (f64::from(sx) + 0.5) / f64::from(SS);
                let sample_y = f64::from(py) + (f64::from(sy) + 0.5) / f64::from(SS);
                hits += u32::from(inside(sample_x, sample_y));
            }
        }
        ((u32::from(alpha) * hits + SS * SS / 2) / (SS * SS)) as u8
    };
    for py in (y..y + cap).chain((y + h - cap)..(y + h)) {
        let mut start = x;
        let mut run: Option<u8> = None;
        for px in x..x + w {
            let a = coverage(px, py);
            match run {
                Some(r) if r == a => {}
                Some(r) => {
                    push_quad(out, (start, py, px - start, 1), rgb, r, cell_h);
                    (start, run) = (px, Some(a));
                }
                None => (start, run) = (px, Some(a)),
            }
        }
        if let Some(r) = run {
            push_quad(out, (start, py, x + w - start, 1), rgb, r, cell_h);
        }
    }
}

/// Append a 1-device-pixel-scaled rectangular outline `(x, y, w, h)` (frame pixels, within one cell-row band) as four source-over quads: the hollow cursor of an unfocused pane.
pub(crate) fn push_outline(
    out: &mut Vec<GlowQuad>,
    (x, y, w, h): (u32, u32, u32, u32),
    t: u32,
    rgb: [u8; 3],
    alpha: u8,
    cell_h: u32,
) {
    if w <= 2 * t || h <= 2 * t {
        push_quad(out, (x, y, w, h), rgb, alpha, cell_h);
        return;
    }
    push_quad(out, (x, y, w, t), rgb, alpha, cell_h);
    push_quad(out, (x, y + h - t, w, t), rgb, alpha, cell_h);
    push_quad(out, (x, y + t, t, h - 2 * t), rgb, alpha, cell_h);
    push_quad(out, (x + w - t, y + t, t, h - 2 * t), rgb, alpha, cell_h);
}

/// Append the rectangle `(x, y, w, h)` (frame pixels, any number of row bands tall) as flat source-over [`GlowQuad`]s, one per cell-row band it crosses: the Fluent split's focus line.
pub(crate) fn push_rect(
    out: &mut Vec<GlowQuad>,
    (x, y, w, h): (u32, u32, u32, u32),
    rgb: [u8; 3],
    alpha: u8,
    cell_h: u32,
) {
    if w == 0 || h == 0 || cell_h == 0 {
        return;
    }
    let mut row_y = y;
    let end = y.saturating_add(h);
    while row_y < end {
        let band_end = (row_y / cell_h + 1) * cell_h;
        let stop = band_end.min(end);
        push_quad(out, (x, row_y, w, stop - row_y), rgb, alpha, cell_h);
        row_y = stop;
    }
}

/// One flat source-over quad: `rgb` premultiplied by `alpha` (the stream's
/// contract, `out = color + dst·(255 − alpha)/255`).
fn push_quad(
    out: &mut Vec<GlowQuad>,
    (x, y, w, h): (u32, u32, u32, u32),
    rgb: [u8; 3],
    alpha: u8,
    cell_h: u32,
) {
    if alpha == 0 || w == 0 || h == 0 {
        return;
    }
    let to16 = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    let premul = |c: u8| (u32::from(c) * u32::from(alpha) + 127) / 255;
    let color = (premul(rgb[0]) << 16) | (premul(rgb[1]) << 8) | premul(rgb[2]);
    out.push(GlowQuad {
        row: to16(y / cell_h.max(1)),
        x: to16(x),
        y: to16(y),
        w: to16(w),
        h: to16(h),
        color,
        alpha,
        color2: color,
        alpha2: alpha,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (usize, usize) = (9, 23);

    fn m() -> Metrics {
        Metrics::for_cell(9, 23)
    }

    #[test]
    fn the_windows_default_cell_gets_a_three_pixel_thumb_that_widens_to_six() {
        let m = m();
        assert_eq!((m.thin, m.fine, m.wide, m.margin), (3, 2, 6, 2));
        let big = Metrics::for_cell(18, 46);
        assert_eq!(
            (big.thin, big.fine, big.wide, big.margin),
            (6, 4, 12, 4),
            "2x draws 2x"
        );
        assert_eq!(Metrics::for_cell(13, 33).fine, 3, "150%");
    }

    #[test]
    fn the_right_pane_draws_in_the_window_padding_and_an_inner_one_in_the_gap() {
        let edge = lane((1, 0, 30, 111), 111, CELL, 12, m()).expect("edge lane");
        assert_eq!((edge.right, edge.room, edge.edge), (111 * 9 + 12, 12, true));
        let (x, w) = span(edge, m().thin, m());
        assert!(x >= 111 * 9, "the resting thumb never covers a column: {x}");
        assert_eq!(x + w + m().margin, edge.right);
        let (wx, ww) = span(edge, m().wide, m());
        assert!(wx >= 111 * 9 && ww == 6, "and neither does the wide one");

        let inner = lane((1, 0, 30, 55), 111, CELL, 12, m()).expect("inner lane");
        assert_eq!((inner.right, inner.room, inner.edge), (56 * 9, 9, false));
    }

    /// THE THUMB IN A DIVIDER GAP sits on the hairline with the same air from
    /// the text on each side, at every scale, resting or widened, and never
    /// covers a column of either pane.
    #[test]
    fn a_gap_thumb_is_centred_on_the_hairline_clear_of_both_panes() {
        for (cw, ch) in [(9u32, 23u32), (14, 35), (18, 46), (10, 20), (7, 17)] {
            let m = Metrics::for_cell(cw, ch);
            let cell = (cw as usize, ch as usize);
            let inner = lane((0, 0, 30, 40), 81, cell, 12, m).expect("inner lane");
            let gap = (40 * cw, 41 * cw);
            let line = gap.0 + hairline_start(cw, m.stroke);
            for width in [m.thin, m.wide, m.wide + 2] {
                let (x, w) = span(inner, width, m);
                assert!(
                    x >= gap.0 && x + w <= gap.1,
                    "{cw}x{ch} w{width}: inside the gap: {x}+{w}"
                );
                assert!(
                    (2 * x + w).abs_diff(2 * line + m.stroke) <= 1,
                    "{cw}x{ch} w{width}: centred on the line at {line}: {x}+{w}"
                );
                assert!(
                    x <= line && x + w >= line + m.stroke,
                    "{cw}x{ch} w{width}: over the hairline"
                );
            }
            let (x, _) = span(inner, m.thin, m);
            assert!(
                x - gap.0 >= 2,
                "{cw}x{ch}: air from the text: {}",
                x - gap.0
            );
        }
        let m = m();
        let inner = lane((0, 0, 30, 40), 81, CELL, 12, m).unwrap();
        assert_eq!(
            span(inner, m.thin, m),
            (40 * 9 + 3, 3),
            "1x: 3 px each side"
        );
    }

    /// A FLUENT RESTING THUMB IN A GAP BESIDE THE RING covers the hairline and
    /// grows away from the ring, at every scale and on either side: a stroke
    /// of ground between it and the ring (which stands one stroke clear of the
    /// line on its pane's side), at least two pixels from the far pane's text,
    /// inside the gap.
    #[test]
    fn a_fluent_resting_thumb_beside_the_ring_grows_away_from_it() {
        for (cw, ch) in [
            (9u32, 23u32),
            (13, 33),
            (14, 35),
            (17, 44),
            (18, 46),
            (7, 17),
        ] {
            let m = Metrics::for_cell(cw, ch);
            let t = m.stroke;
            let cell = (cw as usize, ch as usize);
            let inner = lane((0, 0, 30, 40), 81, cell, 12, m).expect("inner lane");
            let gap = (40 * cw, 41 * cw);
            let line = gap.0 + hairline_start(cw, t);
            let ring_left = line - 2 * t;
            let ring_right = line + 2 * t;
            let w = rest_width(m, true);
            assert!(w > t, "{cw}x{ch}: wider than the line it covers");
            let mut crowded = false;
            for (side, ring_x) in [(RingSide::Left, ring_left), (RingSide::Right, ring_right)] {
                let (x, sw) = rest_span(inner, w, m, Some(side));
                assert_eq!(sw, w, "{cw}x{ch} {side:?}");
                assert!(
                    x <= line && x + sw >= line + t,
                    "{cw}x{ch} {side:?}: over the hairline: {x}+{sw}"
                );
                assert!(
                    x >= gap.0 + 2 && x + sw + 2 <= gap.1,
                    "{cw}x{ch} {side:?}: clear of both panes' text: {x}+{sw}"
                );
                let ground = |x: u32, sw: u32| {
                    if ring_x >= x + sw {
                        ring_x - (x + sw)
                    } else {
                        x.saturating_sub(ring_x + t)
                    }
                };
                assert!(
                    ground(x, sw) >= t,
                    "{cw}x{ch} {side:?}: a stroke of ground from the ring: {}",
                    ground(x, sw)
                );
                let (cx, csw) = span(inner, w, m);
                crowded |= ground(cx, csw) < t;
            }
            assert!(
                crowded,
                "{cw}x{ch}: CONTROL: centred, the thumb crowds one ring"
            );
            assert_eq!(rest_span(inner, w, m, None), span(inner, w, m));
            let edge = lane((0, 41, 30, 40), 81, cell, 12, m).expect("edge lane");
            assert_eq!(
                rest_span(edge, w, m, Some(RingSide::Left)),
                span(edge, w, m),
                "{cw}x{ch}: the padding"
            );
        }
    }

    /// THE WIDENED BAR IN A GAP BESIDE THE RING keeps to the ring's far side
    /// at every scale and on either side: it covers the hairline, a stroke of
    /// ground lies between it and the ring's stroke, it stays a stroke clear
    /// of the far pane's text, and it is no wider than the centred track.
    #[test]
    fn a_widened_gap_bar_beside_the_ring_keeps_off_it() {
        for (cw, ch) in [
            (9u32, 23u32),
            (13, 33),
            (14, 35),
            (17, 44),
            (18, 46),
            (7, 17),
        ] {
            let m = Metrics::for_cell(cw, ch);
            let t = m.stroke;
            let cell = (cw as usize, ch as usize);
            let inner = lane((0, 0, 30, 40), 81, cell, 12, m).expect("inner lane");
            let gap = (40 * cw, 41 * cw);
            let line = gap.0 + hairline_start(cw, t);
            let mut covers_ring = false;
            for (side, ring_x) in [
                (RingSide::Left, line - 2 * t),
                (RingSide::Right, line + 2 * t),
            ] {
                let (x, w) = wide_span(inner, m, Some(side));
                assert!(
                    x <= line && x + w >= line + t,
                    "{cw}x{ch} {side:?}: over the hairline: {x}+{w}"
                );
                assert!(w <= m.wide + 2, "{cw}x{ch} {side:?}: no wider than centred");
                assert!(
                    x >= gap.0 + t && x + w + t <= gap.1,
                    "{cw}x{ch} {side:?}: a stroke clear of both panes' text: {x}+{w}"
                );
                let ground = if ring_x >= x + w {
                    ring_x - (x + w)
                } else {
                    x.saturating_sub(ring_x + t)
                };
                assert!(
                    ground >= t,
                    "{cw}x{ch} {side:?}: a stroke of ground from the ring: {ground}"
                );
                let (cx, cw_) = span(inner, m.wide + 2, m);
                covers_ring |= cx <= ring_x && cx + cw_ >= ring_x + t;
            }
            assert!(
                covers_ring,
                "{cw}x{ch}: CONTROL: the centred track covers the ring"
            );
            assert_eq!(wide_span(inner, m, None), span(inner, m.wide + 2, m));
            let edge = lane((0, 41, 30, 40), 81, cell, 12, m).expect("edge lane");
            assert_eq!(
                wide_span(edge, m, Some(RingSide::Left)),
                span(edge, m.wide + 2, m)
            );
        }
    }

    /// ONE TONE OVER THE HAIRLINE: from `solid` up the thumb is opaque in the
    /// ink composed over the ground, so the hairline under it cannot show;
    /// below it the solid tone fades, and over the ground every step is the
    /// plain translucent ink's tone.
    #[test]
    fn a_thumb_over_the_ground_is_one_tone_over_the_hairline_too() {
        let (ink, ground, line) = ([0xf8, 0xf8, 0xf2], [0x28, 0x2a, 0x36], [0x5a, 0x5c, 0x68]);
        let over = |rgb: [u8; 3], alpha: u8, dst: [u8; 3]| -> [u8; 3] {
            std::array::from_fn(|i| {
                let a = u32::from(alpha);
                ((u32::from(rgb[i]) * a + u32::from(dst[i]) * (255 - a) + 127) / 255) as u8
            })
        };
        let solid = rest_alpha(ALPHA_REST, true);
        for alpha in [solid, 200, 255] {
            let (rgb, a) = over_ground(ink, ground, alpha, solid);
            assert_eq!(a, 255, "{alpha}: opaque");
            assert_eq!(
                over(rgb, a, line),
                over(rgb, a, ground),
                "{alpha}: one tone"
            );
            assert_eq!(over(rgb, a, ground), over(ink, alpha, ground), "{alpha}");
        }
        for alpha in [1u8, 40, 120, solid - 1] {
            let (rgb, a) = over_ground(ink, ground, alpha, solid);
            assert!(a < 255, "{alpha}: fading gives the hairline back");
            let want = over(ink, alpha, ground);
            let got = over(rgb, a, ground);
            assert!(
                (0..3).all(|i| want[i].abs_diff(got[i]) <= 1),
                "{alpha}: over the ground the plain ink's tone: {got:?} vs {want:?}"
            );
        }
        assert_ne!(
            over(ink, solid, line),
            over(ink, solid, ground),
            "CONTROL: translucent, two tones"
        );
    }

    /// ONE RESTING IDIOM UNDER THE FLUENT CHROME: the thin indicator, at the
    /// same width and the same (doubled) opacity whatever lane it rests in;
    /// the Classic chrome keeps its thin thumb at its own opacity.
    #[test]
    fn the_fluent_resting_thumb_is_one_width_and_one_ink() {
        let m = m();
        assert_eq!(rest_width(m, true), m.fine);
        assert_eq!(rest_width(m, false), m.thin);
        assert!(m.fine < m.thin && m.fine < m.wide, "thin, and widens");
        assert_eq!(rest_alpha(ALPHA_REST, true), 2 * ALPHA_REST);
        assert_eq!(rest_alpha(ALPHA_ACTIVE, true), 255, "opaque at most");
        assert_eq!(rest_alpha(ALPHA_REST, false), ALPHA_REST);
    }

    #[test]
    fn the_track_spans_the_pane_rows_inset() {
        let l = lane((2, 0, 10, 80), 80, CELL, 12, m()).unwrap();
        assert_eq!(l.top, 2 * 23 + 2);
        assert_eq!(l.len, 10 * 23 - 4);
    }

    #[test]
    fn no_history_no_thumb_and_the_ends_are_exact() {
        let l = lane((0, 0, 30, 80), 80, CELL, 12, m()).unwrap();
        assert_eq!(thumb(l, 30, 0, 0, m()), None);
        let (live_top, len) = thumb(l, 30, 300, 0, m()).unwrap();
        assert_eq!(
            live_top + len,
            l.top + l.len,
            "live bottom: flush with the track's end"
        );
        let (top, _) = thumb(l, 30, 300, 300, m()).unwrap();
        assert_eq!(top, l.top, "top of history: flush with the track's top");
    }

    #[test]
    fn dragging_the_thumb_to_a_place_reads_back_the_offset_that_put_it_there() {
        let l = lane((0, 0, 30, 80), 80, CELL, 12, m()).unwrap();
        for offset in [0u32, 1, 17, 150, 299, 300] {
            let (top, _) = thumb(l, 30, 300, offset, m()).unwrap();
            let back = offset_for_top(l, 30, 300, f64::from(top), m());
            let (_, len) = thumb(l, 30, 300, 0, m()).unwrap();
            let per_px = 300.0 / f64::from(l.len - len);
            assert!(
                (f64::from(back) - f64::from(offset)).abs() <= per_px.ceil(),
                "offset {offset} -> top {top} -> {back}"
            );
        }
        assert_eq!(
            offset_for_top(l, 30, 300, -50.0, m()),
            300,
            "above the track: the top"
        );
        assert_eq!(offset_for_top(l, 30, 300, 1e6, m()), 0, "below it: live");
    }

    #[test]
    fn the_edge_lane_takes_the_padding_and_the_gap_lane_only_its_thumb() {
        let l = lane((0, 0, 30, 80), 80, CELL, 12, m()).unwrap();
        let t = thumb(l, 30, 300, 150, m()).unwrap();
        let x = f64::from(80 * 9 + 6);
        assert_eq!(
            hit(l, t, x, f64::from(t.0 + 1), m()),
            Some(Hit::Thumb { grab: 1.0 })
        );
        assert_eq!(
            hit(l, t, x, f64::from(l.top + 1), m()),
            Some(Hit::Track { above: true })
        );
        assert_eq!(
            hit(l, t, f64::from(80 * 9 - 1), f64::from(t.0 + 1), m()),
            None,
            "text"
        );

        let inner = lane((0, 0, 30, 40), 80, CELL, 12, m()).unwrap();
        let t = thumb(inner, 30, 300, 150, m()).unwrap();
        let x = f64::from(40 * 9 + 4);
        assert!(matches!(
            hit(inner, t, x, f64::from(t.0 + 1), m()),
            Some(Hit::Thumb { .. })
        ));
        assert_eq!(
            hit(inner, t, x, f64::from(inner.top + 1), m()),
            None,
            "off the thumb, a divider gap stays the divider's"
        );
        assert_eq!(
            hit(inner, t, f64::from(40 * 9), f64::from(t.0 + 1), m()),
            None,
            "the gap's edge beside the text stays the divider's"
        );
    }

    #[test]
    fn a_rect_is_cut_at_every_row_band() {
        let mut out = Vec::new();
        push_rect(&mut out, (40, 10, 1, 60), [80, 250, 123], 255, 23);
        let bands: Vec<(u16, u16, u16)> = out.iter().map(|q| (q.row, q.y, q.h)).collect();
        assert_eq!(
            bands,
            vec![(0, 10, 13), (1, 23, 23), (2, 46, 23), (3, 69, 1)]
        );
        assert!(out.iter().all(|q| q.alpha == 255 && q.w == 1 && q.x == 40));
    }

    /// Every quad is source-over (alpha > 0), lies in one row band, and the
    /// bar's pixels are exactly its rectangle: corners shaved, the middle solid.
    #[test]
    fn a_bar_is_rounded_source_over_and_banded() {
        let mut out = Vec::new();
        legacy_bar_quads(&mut out, (100, 40, 6, 60), [200, 200, 200], 160, 23);
        assert!(!out.is_empty());
        let mut cover = std::collections::BTreeMap::new();
        for q in &out {
            assert!(q.alpha > 0, "a zero alpha would be additive light");
            assert_eq!(
                u32::from(q.y) / 23,
                u32::from(q.y + q.h - 1) / 23,
                "one band"
            );
            assert_eq!(u32::from(q.row), u32::from(q.y) / 23);
            for y in q.y..q.y + q.h {
                for x in q.x..q.x + q.w {
                    assert!(cover.insert((x, y), q.alpha).is_none(), "no pixel twice");
                }
            }
        }
        assert!(
            cover
                .keys()
                .all(|&(x, y)| (100..106).contains(&x) && (40..100).contains(&y))
        );
        assert_eq!(cover.get(&(103, 70)), Some(&160), "the body is solid");
        assert!(
            cover.get(&(100, 40)).is_none_or(|a| *a < 80),
            "a corner pixel is shaved"
        );
        assert!(
            cover.get(&(102, 40)).copied().unwrap_or(0) > 0,
            "the cap's middle shows"
        );
    }

    #[test]
    fn an_outline_is_four_edges_and_no_middle() {
        let mut out = Vec::new();
        push_outline(&mut out, (90, 46, 9, 23), 1, [80, 250, 123], 200, 23);
        assert_eq!(out.len(), 4);
        let inside = |x: u16, y: u16| {
            out.iter()
                .any(|q| x >= q.x && x < q.x + q.w && y >= q.y && y < q.y + q.h)
        };
        assert!(inside(90, 46) && inside(98, 68) && inside(90, 60) && inside(98, 50));
        assert!(!inside(94, 57), "hollow");
        assert!(out.iter().all(|q| q.row == 2));
    }
}
