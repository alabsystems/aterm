// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/hint/outline.rs and modified by the
// aterm project in 2026: the hint-plan recorder dropped; its tests
// need fontations' test fonts and are not carried.
// See ../../../LICENSE-FONTATIONS for the upstream license.

//! Apply edge hints to an outline.

use super::super::{
    glyf::PointMarker,
    metrics::{Scale, ScaleFlags, fixed_div, fixed_mul},
    outline::{Outline, Point},
    style::ScriptGroup,
    topo::{Axis, Dimension},
};
use core::cmp::Ordering;

/// Align all points of an edge to the same coordinate value.
pub(crate) fn align_edge_points(
    outline: &mut Outline,
    axis: &Axis,
    group: ScriptGroup,
    scale: &Scale,
) -> Option<()> {
    let edges = axis.edges.as_slice();
    let segments = axis.segments.as_slice();
    let points = outline.points.as_mut_slice();
    let snap = group == ScriptGroup::Default
        || (axis.dim == Dimension::Horizontal && scale.flags.contains(ScaleFlags::HORIZONTAL_SNAP))
        || (axis.dim == Dimension::Vertical && scale.flags.contains(ScaleFlags::VERTICAL_SNAP));
    for segment in segments {
        let Some(edge) = segment.edge(edges) else {
            continue;
        };
        let delta = edge.pos - edge.opos;
        let mut point_ix = segment.first();
        let last_ix = segment.last();
        loop {
            let point = points.get_mut(point_ix)?;
            if axis.dim == Dimension::Horizontal {
                if snap {
                    point.x = edge.pos;
                } else {
                    point.x += delta;
                }
                point.flags.set_marker(PointMarker::TOUCHED_X);
            } else {
                if snap {
                    point.y = edge.pos;
                } else {
                    point.y += delta;
                }
                point.flags.set_marker(PointMarker::TOUCHED_Y);
            }
            if point_ix == last_ix {
                break;
            }
            point_ix = point.next();
        }
    }
    Some(())
}

/// Align the strong points; equivalent to the TrueType `IP` instruction.
pub(crate) fn align_strong_points(outline: &mut Outline, axis: &mut Axis) -> Option<()> {
    if axis.edges.is_empty() {
        return Some(());
    }
    let dim = axis.dim;
    let touch_flag = if dim == Dimension::Horizontal {
        PointMarker::TOUCHED_X
    } else {
        PointMarker::TOUCHED_Y
    };
    let points = outline.points.as_mut_slice();
    'points: for point in points.iter_mut() {
        if point
            .flags
            .has_marker(touch_flag | PointMarker::WEAK_INTERPOLATION)
        {
            continue;
        }
        let (u, ou) = if dim == Dimension::Vertical {
            (point.fy, point.oy)
        } else {
            (point.fx, point.ox)
        };
        let edges = axis.edges.as_mut_slice();
        let edge = edges.first()?;
        let delta = edge.fpos as i32 - u;
        if delta >= 0 {
            store_point(point, dim, edge.pos - (edge.opos - ou));
            continue;
        }
        let edge = edges.last()?;
        let delta = u - edge.fpos as i32;
        if delta >= 0 {
            store_point(point, dim, edge.pos + (ou - edge.opos));
            continue;
        }
        let min_ix = if edges.len() <= 8 {
            if let Some((min_ix, edge)) = edges
                .iter()
                .enumerate()
                .find(|(_ix, edge)| edge.fpos as i32 >= u)
            {
                if edge.fpos as i32 == u {
                    store_point(point, dim, edge.pos);
                    continue 'points;
                }
                min_ix
            } else {
                0
            }
        } else {
            let mut min_ix = 0;
            let mut max_ix = edges.len();
            while min_ix < max_ix {
                let mid_ix = (min_ix + max_ix) >> 1;
                let edge = &edges[mid_ix];
                let fpos = edge.fpos as i32;
                match u.cmp(&fpos) {
                    Ordering::Less => max_ix = mid_ix,
                    Ordering::Greater => min_ix = mid_ix + 1,
                    Ordering::Equal => {
                        store_point(point, dim, edge.pos);
                        continue 'points;
                    }
                }
            }
            min_ix
        };
        if let Some(before_ix) = min_ix.checked_sub(1) {
            let edge_before = edges.get(before_ix)?;
            let before_pos = edge_before.pos;
            let before_fpos = edge_before.fpos as i32;
            let scale = if edge_before.scale == 0 {
                let edge_after = edges.get(min_ix)?;
                let scale = fixed_div(
                    edge_after.pos - edge_before.pos,
                    edge_after.fpos as i32 - before_fpos,
                );
                edges[before_ix].scale = scale;
                scale
            } else {
                edge_before.scale
            };
            store_point(point, dim, before_pos + fixed_mul(u - before_fpos, scale));
        }
    }
    Some(())
}

/// Align the weak points; equivalent to the TrueType `IUP` instruction.
pub(crate) fn align_weak_points(outline: &mut Outline, dim: Dimension) -> Option<()> {
    let touch_marker = if dim == Dimension::Horizontal {
        for point in &mut outline.points {
            point.u = point.x;
            point.v = point.ox;
        }
        PointMarker::TOUCHED_X
    } else {
        for point in &mut outline.points {
            point.u = point.y;
            point.v = point.oy;
        }
        PointMarker::TOUCHED_Y
    };
    for contour in &outline.contours {
        let points = outline.points.get_mut(contour.range())?;
        let Some(first_touched_ix) = points
            .iter()
            .position(|point| point.flags.has_marker(touch_marker))
        else {
            continue;
        };
        let last_ix = points.len() - 1;
        let mut point_ix = first_touched_ix;
        let mut last_touched_ix;
        'outer: loop {
            while point_ix < last_ix && points.get(point_ix + 1)?.flags.has_marker(touch_marker) {
                point_ix += 1;
            }
            last_touched_ix = point_ix;
            point_ix += 1;
            loop {
                if point_ix > last_ix {
                    break 'outer;
                }
                if points[point_ix].flags.has_marker(touch_marker) {
                    break;
                }
                point_ix += 1;
            }
            iup_interpolate(
                points,
                last_touched_ix + 1,
                point_ix - 1,
                last_touched_ix,
                point_ix,
            );
        }
        if last_touched_ix == first_touched_ix {
            iup_shift(points, 0, last_ix, first_touched_ix);
        } else {
            if last_touched_ix < last_ix {
                iup_interpolate(
                    points,
                    last_touched_ix + 1,
                    last_ix,
                    last_touched_ix,
                    first_touched_ix,
                );
            }
            if first_touched_ix > 0 {
                iup_interpolate(
                    points,
                    0,
                    first_touched_ix - 1,
                    last_touched_ix,
                    first_touched_ix,
                );
            }
        }
    }
    if dim == Dimension::Horizontal {
        for point in &mut outline.points {
            point.x = point.u;
        }
    } else {
        for point in &mut outline.points {
            point.y = point.u;
        }
    }
    Some(())
}

#[inline(always)]
fn store_point(point: &mut Point, dim: Dimension, u: i32) {
    if dim == Dimension::Horizontal {
        point.x = u;
        point.flags.set_marker(PointMarker::TOUCHED_X);
    } else {
        point.y = u;
        point.flags.set_marker(PointMarker::TOUCHED_Y);
    }
}

/// Shift coordinates in `p1_ix..=p2_ix` by the delta of the reference point at `ref_ix`.
fn iup_shift(points: &mut [Point], p1_ix: usize, p2_ix: usize, ref_ix: usize) -> Option<()> {
    let ref_point = points.get(ref_ix)?;
    let delta = ref_point.u - ref_point.v;
    if delta == 0 {
        return Some(());
    }
    for point in points.get_mut(p1_ix..ref_ix)? {
        point.u = point.v + delta;
    }
    for point in points.get_mut(ref_ix + 1..=p2_ix)? {
        point.u = point.v + delta;
    }
    Some(())
}

/// Interpolate coordinates in `p1_ix..=p2_ix` using reference points `ref1_ix` and `ref2_ix`.
fn iup_interpolate(
    points: &mut [Point],
    p1_ix: usize,
    p2_ix: usize,
    ref1_ix: usize,
    ref2_ix: usize,
) -> Option<()> {
    if p1_ix > p2_ix {
        return Some(());
    }
    let mut ref_point1 = points.get(ref1_ix)?;
    let mut ref_point2 = points.get(ref2_ix)?;
    if ref_point1.v > ref_point2.v {
        core::mem::swap(&mut ref_point1, &mut ref_point2);
    }
    let (u1, v1) = (ref_point1.u, ref_point1.v);
    let (u2, v2) = (ref_point2.u, ref_point2.v);
    let d1 = u1 - v1;
    let d2 = u2 - v2;
    if u1 == u2 || v1 == v2 {
        for point in points.get_mut(p1_ix..=p2_ix)? {
            point.u = if point.v <= v1 {
                point.v + d1
            } else if point.v >= v2 {
                point.v + d2
            } else {
                u1
            };
        }
    } else {
        let scale = fixed_div(u2 - u1, v2 - v1);
        for point in points.get_mut(p1_ix..=p2_ix)? {
            point.u = if point.v <= v1 {
                point.v + d1
            } else if point.v >= v2 {
                point.v + d2
            } else {
                u1 + fixed_mul(point.v - v1, scale)
            };
        }
    }
    Some(())
}
