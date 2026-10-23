// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/hint/edges.rs and modified by the
// aterm project in 2026: the hint-plan recorder dropped; its tests
// need fontations' test fonts and are not carried.
// See ../../../LICENSE-FONTATIONS for the upstream license.

//! Edge hinting.

use super::super::{
    metrics::{
        Scale, ScaleFlags, ScaledAxisMetrics, ScaledWidth, fixed_mul_div, pix_floor, pix_round,
    },
    style::ScriptGroup,
    topo::{Axis, Dimension, Edge, TopoFlags},
};

/// Main Latin grid-fitting routine.
pub(crate) fn hint_edges(
    axis: &mut Axis,
    metrics: &ScaledAxisMetrics,
    group: ScriptGroup,
    scale: &Scale,
    mut top_to_bottom_hinting: bool,
) {
    if axis.dim != Dimension::Vertical {
        top_to_bottom_hinting = false;
    }
    let anchor_ix = align_edges_to_blues(axis, metrics, group, scale);
    let (serif_count, anchor_ix) = align_stem_edges(
        axis,
        metrics,
        group,
        scale,
        top_to_bottom_hinting,
        anchor_ix,
    );
    let edges = axis.edges.as_mut_slice();
    if axis.dim == Dimension::Horizontal && (edges.len() == 6 || edges.len() == 12) {
        hint_lowercase_m(edges, group);
    }
    if serif_count > 0 || anchor_ix.is_none() {
        align_remaining_edges(axis, group, top_to_bottom_hinting, serif_count, anchor_ix);
    }
}

fn align_edges_to_blues(
    axis: &mut Axis,
    metrics: &ScaledAxisMetrics,
    group: ScriptGroup,
    scale: &Scale,
) -> Option<usize> {
    let mut anchor_ix = None;
    if group == ScriptGroup::Default && axis.dim != Dimension::Vertical {
        return anchor_ix;
    }
    for edge_ix in 0..axis.edges.len() {
        let mut linked_edge_to_align = None;
        {
            let edges = axis.edges.as_mut_slice();
            let edge = &edges[edge_ix];
            if edge.flags.contains(TopoFlags::DONE) {
                continue;
            }
            let edge2_ix = edge.link_ix.map(|x| x as usize);
            let edge2 = edge2_ix.map(|ix| &edges[ix]);
            if let (true, Some(edge2)) = (edge.blue_edge.is_some(), edge2) {
                if edge2.blue_edge.is_some() {
                    let skip_ix = if edge2.flags.contains(TopoFlags::NEUTRAL) {
                        edge2_ix
                    } else if edge.flags.contains(TopoFlags::NEUTRAL) {
                        Some(edge_ix)
                    } else {
                        None
                    };
                    if let Some(skip_ix) = skip_ix {
                        let skip_edge = &mut edges[skip_ix];
                        skip_edge.blue_edge = None;
                        skip_edge.flags &= !TopoFlags::NEUTRAL;
                    }
                }
            }
            let blue = edges[edge_ix].blue_edge;
            let (blue, edge1_ix, edge2_ix) = if let Some(blue) = blue {
                (blue, Some(edge_ix), edge2_ix)
            } else if let Some(edge2_blue) = edge2_ix.and_then(|ix| edges[ix].blue_edge) {
                (edge2_blue, edge2_ix, Some(edge_ix))
            } else {
                (Default::default(), None, None)
            };
            let Some(edge1_ix) = edge1_ix else {
                continue;
            };
            if edges[edge1_ix].flags.contains(TopoFlags::DONE) {
                continue;
            }
            let edge1 = &mut edges[edge1_ix];
            edge1.pos = blue.fitted;
            edge1.flags |= TopoFlags::DONE;
            if let Some(edge2_ix) = edge2_ix {
                if edges[edge2_ix].blue_edge.is_none() {
                    edges[edge2_ix].flags |= TopoFlags::DONE;
                    linked_edge_to_align = Some((edge1_ix, edge2_ix));
                }
            }
        }
        if let Some((edge1_ix, edge2_ix)) = linked_edge_to_align {
            align_linked_edge(axis, metrics, group, scale, edge1_ix, edge2_ix);
        }
        if anchor_ix.is_none() {
            anchor_ix = Some(edge_ix);
        }
    }
    anchor_ix
}

fn align_stem_edges(
    axis: &mut Axis,
    metrics: &ScaledAxisMetrics,
    group: ScriptGroup,
    scale: &Scale,
    top_to_bottom_hinting: bool,
    mut anchor_ix: Option<usize>,
) -> (usize, Option<usize>) {
    let mut serif_count = 0;
    let mut last_stem_pos = None;
    let mut delta = 0;
    for edge_ix in 0..axis.edges.len() {
        let (edge_flags, edge_link_ix, edge_pos, edge2_pos_for_cjk, edge2_has_blue) = {
            let edges = axis.edges.as_slice();
            let edge = &edges[edge_ix];
            let link_ix = edge.link_ix.map(|ix| ix as usize);
            let (edge2_pos, edge2_has_blue) = link_ix
                .map(|ix| (edges[ix].pos, edges[ix].blue_edge.is_some()))
                .unwrap_or((0, false));
            (edge.flags, link_ix, edge.pos, edge2_pos, edge2_has_blue)
        };
        if edge_flags.contains(TopoFlags::DONE) {
            continue;
        }
        let Some(edge2_ix) = edge_link_ix else {
            serif_count += 1;
            continue;
        };
        if group != ScriptGroup::Default {
            if let Some(last_pos) = last_stem_pos {
                if edge_pos < last_pos + 64 || edge2_pos_for_cjk < last_pos + 64 {
                    serif_count += 1;
                    continue;
                }
            }
        }
        if edge2_has_blue {
            align_linked_edge(axis, metrics, group, scale, edge2_ix, edge_ix);
            axis.edges[edge_ix].flags |= TopoFlags::DONE;
            continue;
        }
        let edges = axis.edges.as_mut_slice();
        if group == ScriptGroup::Default {
            if let Some(anchor_ix) = anchor_ix {
                let anchor = &edges[anchor_ix];
                let edge = edges[edge_ix];
                let edge2 = edges[edge2_ix];
                let original_pos = anchor.pos + (edge.opos - anchor.opos);
                let original_len = edge2.opos - edge.opos;
                let original_center = original_pos + (original_len >> 1);
                let cur_len = stem_width(
                    metrics,
                    group,
                    scale,
                    original_len,
                    0,
                    edge.flags,
                    edge2.flags,
                );
                if edge2.flags.contains(TopoFlags::DONE) {
                    let new_pos = edge2.pos - cur_len;
                    edges[edge_ix].pos = new_pos;
                } else if cur_len < 96 {
                    let cur_pos1 = pix_round(original_center);
                    let (u_off, d_off) = if cur_len <= 64 { (32, 32) } else { (38, 26) };
                    let delta1 = (original_center - (cur_pos1 - u_off)).abs();
                    let delta2 = (original_center - (cur_pos1 + d_off)).abs();
                    let cur_pos1 = if delta1 < delta2 {
                        cur_pos1 - u_off
                    } else {
                        cur_pos1 + d_off
                    };
                    edges[edge_ix].pos = cur_pos1 - cur_len / 2;
                    edges[edge2_ix].pos = cur_pos1 + cur_len / 2;
                } else {
                    let cur_pos1 = pix_round(original_pos);
                    let delta1 = (cur_pos1 + (cur_len >> 1) - original_center).abs();
                    let cur_pos2 = pix_round(original_pos + original_len) - cur_len;
                    let delta2 = (cur_pos2 + (cur_len >> 1) - original_center).abs();
                    let new_pos = if delta1 < delta2 { cur_pos1 } else { cur_pos2 };
                    let new_pos2 = new_pos + cur_len;
                    edges[edge_ix].pos = new_pos;
                    edges[edge2_ix].pos = new_pos2;
                }
                edges[edge_ix].flags |= TopoFlags::DONE;
                edges[edge2_ix].flags |= TopoFlags::DONE;
                if edge_ix > 0 {
                    adjust_link(edges, edge_ix, LinkDir::Prev, top_to_bottom_hinting);
                }
            } else {
                let edge = edges[edge_ix];
                let edge2 = edges[edge2_ix];
                let original_len = edge2.opos - edge.opos;
                let cur_len = stem_width(
                    metrics,
                    group,
                    scale,
                    original_len,
                    0,
                    edge.flags,
                    edge2.flags,
                );
                let (u_off, d_off) = if cur_len <= 64 { (32, 32) } else { (38, 26) };
                if cur_len < 96 {
                    let original_center = edge.opos + (original_len >> 1);
                    let mut cur_pos1 = pix_round(original_center);
                    let error1 = (original_center - (cur_pos1 - u_off)).abs();
                    let error2 = (original_center - (cur_pos1 + d_off)).abs();
                    if error1 < error2 {
                        cur_pos1 -= u_off;
                    } else {
                        cur_pos1 += d_off;
                    }
                    let edge_pos = cur_pos1 - cur_len / 2;
                    edges[edge_ix].pos = edge_pos;
                    edges[edge2_ix].pos = edge_pos + cur_len;
                } else {
                    edges[edge_ix].pos = pix_round(edge.opos);
                }
                edges[edge_ix].flags |= TopoFlags::DONE;
                align_linked_edge(axis, metrics, group, scale, edge_ix, edge2_ix);
                anchor_ix = Some(edge_ix);
            }
        } else {
            if edge2_ix < edge_ix {
                last_stem_pos = Some(edge_pos);
                edges[edge_ix].flags |= TopoFlags::DONE;
                align_linked_edge(axis, metrics, group, scale, edge2_ix, edge_ix);
                continue;
            }
            if axis.dim != Dimension::Vertical && anchor_ix.is_none() {
                delta = hint_normal_stem_cjk(axis, metrics, group, scale, edge_ix, edge2_ix, delta);
            } else {
                hint_normal_stem_cjk(axis, metrics, group, scale, edge_ix, edge2_ix, delta);
            }
            anchor_ix = Some(edge_ix);
            axis.edges[edge_ix].flags |= TopoFlags::DONE;
            let edge2 = &mut axis.edges[edge2_ix];
            edge2.flags |= TopoFlags::DONE;
            last_stem_pos = Some(edge2.pos);
        }
    }
    (serif_count, anchor_ix)
}

fn hint_lowercase_m(edges: &mut [Edge], group: ScriptGroup) {
    let (edge1_ix, edge2_ix, edge3_ix) = if edges.len() == 6 {
        (0, 2, 4)
    } else {
        (1, 5, 9)
    };
    let edge1 = &edges[edge1_ix];
    let edge2 = &edges[edge2_ix];
    let edge3 = &edges[edge3_ix];
    let dist1 = edge2.opos - edge1.opos;
    let dist2 = edge3.opos - edge2.opos;
    let span = (dist1 - dist2).abs();
    if group != ScriptGroup::Default {
        for (edge, ix) in [(edge1, edge1_ix), (edge2, edge2_ix), (edge3, edge3_ix)] {
            if edge.link_ix != Some((ix + 1) as u16) {
                return;
            }
        }
    }
    if span < 8 {
        let delta = edge3.pos - (2 * edge2.pos - edge1.pos);
        let link_ix = edge3.link_ix.map(|ix| ix as usize);
        let edge3 = &mut edges[edge3_ix];
        edge3.pos -= delta;
        edge3.flags |= TopoFlags::DONE;
        if let Some(link_ix) = link_ix {
            let link = &mut edges[link_ix];
            link.pos -= delta;
            link.flags |= TopoFlags::DONE;
        }
        if edges.len() == 12 {
            edges[8].pos -= delta;
            edges[11].pos -= delta;
        }
    }
}

fn align_remaining_edges(
    axis: &mut Axis,
    group: ScriptGroup,
    top_to_bottom_hinting: bool,
    mut serif_count: usize,
    mut anchor_ix: Option<usize>,
) {
    if group == ScriptGroup::Default {
        for edge_ix in 0..axis.edges.len() {
            let (edge_flags, edge_opos, edge_serif_ix, delta) = {
                let edges = axis.edges.as_slice();
                let edge = &edges[edge_ix];
                let delta = edge
                    .serif(edges)
                    .map(|serif| (serif.opos - edge.opos).abs())
                    .unwrap_or(1000);
                (edge.flags, edge.opos, edge.serif_ix, delta)
            };
            if edge_flags.contains(TopoFlags::DONE) {
                continue;
            }
            if delta < 64 + 16 {
                let serif_ix = edge_serif_ix.unwrap() as usize;
                align_serif_edge(axis, serif_ix, edge_ix);
            } else if let Some(anchor_ix) = anchor_ix {
                let edges = axis.edges.as_mut_slice();
                let [before_ix, after_ix] = find_bounding_completed_edges(edges, edge_ix);
                if let Some((before_ix, after_ix)) = before_ix.zip(after_ix) {
                    let before = &edges[before_ix];
                    let after = &edges[after_ix];
                    let new_pos = if after.opos == before.opos {
                        before.pos
                    } else {
                        before.pos
                            + fixed_mul_div(
                                edge_opos - before.opos,
                                after.pos - before.pos,
                                after.opos - before.opos,
                            )
                    };
                    edges[edge_ix].pos = new_pos;
                } else {
                    let anchor = &edges[anchor_ix];
                    let new_pos = anchor.pos + ((edge_opos - anchor.opos + 16) & !31);
                    edges[edge_ix].pos = new_pos;
                }
            } else {
                anchor_ix = Some(edge_ix);
                let edges = axis.edges.as_mut_slice();
                let new_pos = pix_round(edge_opos);
                edges[edge_ix].pos = new_pos;
            }
            let edges = &mut axis.edges;
            edges[edge_ix].flags |= TopoFlags::DONE;
            adjust_link(edges, edge_ix, LinkDir::Prev, top_to_bottom_hinting);
            adjust_link(edges, edge_ix, LinkDir::Next, top_to_bottom_hinting);
        }
    } else {
        for edge_ix in 0..axis.edges.len() {
            let edge = &mut axis.edges[edge_ix];
            if edge.flags.contains(TopoFlags::DONE) {
                continue;
            }
            if let Some(serif_ix) = edge.serif_ix.map(|ix| ix as usize) {
                edge.flags |= TopoFlags::DONE;
                align_serif_edge(axis, serif_ix, edge_ix);
                serif_count = serif_count.saturating_sub(1);
            }
        }
        if serif_count == 0 {
            return;
        }
        for edge_ix in 0..axis.edges.len() {
            let edges = axis.edges.as_mut_slice();
            let edge = &edges[edge_ix];
            if edge.flags.contains(TopoFlags::DONE) {
                continue;
            }
            let [before_ix, after_ix] = find_bounding_completed_edges(edges, edge_ix);
            match (before_ix, after_ix) {
                (Some(before_ix), None) => {
                    align_serif_edge(axis, before_ix, edge_ix);
                }
                (None, Some(after_ix)) => {
                    align_serif_edge(axis, after_ix, edge_ix);
                }
                (Some(before_ix), Some(after_ix)) => {
                    let before = edges[before_ix];
                    let after = edges[after_ix];
                    if after.fpos == before.fpos {
                        edges[edge_ix].pos = before.pos;
                    } else {
                        edges[edge_ix].pos = before.pos
                            + fixed_mul_div(
                                edge.fpos as i32 - before.fpos as i32,
                                after.pos - before.pos,
                                after.fpos as i32 - before.fpos as i32,
                            );
                    }
                }
                _ => {}
            }
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
enum LinkDir {
    Prev,
    Next,
}

fn adjust_link(
    edges: &mut [Edge],
    edge_ix: usize,
    link_dir: LinkDir,
    top_to_bottom_hinting: bool,
) -> Option<()> {
    let edge = &edges[edge_ix];
    let (edge2, prev_edge) = if link_dir == LinkDir::Next {
        let edge2 = edges.get(edge_ix + 1)?;
        if !edge2.flags.contains(TopoFlags::DONE) {
            return None;
        }
        (edge2, edges.get(edge_ix.checked_sub(1)?)?)
    } else {
        let edge = edges.get(edge_ix.checked_sub(1)?)?;
        (edge, edge)
    };
    let pos1 = edge.pos;
    let pos2 = edge2.pos;
    let order_check = match (link_dir, top_to_bottom_hinting) {
        (LinkDir::Prev, true) | (LinkDir::Next, false) => pos1 > pos2,
        (LinkDir::Prev, false) | (LinkDir::Next, true) => pos1 < pos2,
    };
    if !order_check {
        return None;
    }
    let link = edge.link(edges)?;
    if (link.pos - prev_edge.pos).abs() > 16 {
        let new_pos = edge2.pos;
        edges[edge_ix].pos = new_pos;
    }
    Some(())
}

fn find_bounding_completed_edges(edges: &[Edge], ix: usize) -> [Option<usize>; 2] {
    let before_ix = edges
        .get(..ix)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(ix, edge)| edge.flags.contains(TopoFlags::DONE).then_some(ix))
        .next();
    let after_ix = edges
        .iter()
        .enumerate()
        .skip(ix + 1)
        .filter_map(|(ix, edge)| edge.flags.contains(TopoFlags::DONE).then_some(ix))
        .next();
    [before_ix, after_ix]
}

fn snap_width(widths: &[ScaledWidth], width: i32) -> i32 {
    let (_, ref_width) =
        widths
            .iter()
            .fold((64 + 32 + 2, width), |(best_dist, ref_width), candidate| {
                let dist = (width - candidate.scaled).abs();
                if dist < best_dist {
                    (dist, candidate.scaled)
                } else {
                    (best_dist, ref_width)
                }
            });
    let scaled = pix_round(ref_width);
    if width >= ref_width {
        if width < scaled + 48 {
            ref_width
        } else {
            width
        }
    } else if width > scaled - 48 {
        ref_width
    } else {
        width
    }
}

fn stem_width(
    metrics: &ScaledAxisMetrics,
    group: ScriptGroup,
    scale: &Scale,
    width: i32,
    base_delta: i32,
    base_flags: TopoFlags,
    stem_flags: TopoFlags,
) -> i32 {
    if !scale.flags.contains(ScaleFlags::STEM_ADJUST)
        || (group == ScriptGroup::Default && metrics.width_metrics.is_extra_light)
    {
        return width;
    }
    let is_vertical = metrics.dim == Dimension::Vertical;
    let sign = if width < 0 { -1 } else { 1 };
    let mut dist = width.abs();
    if (is_vertical && !scale.flags.contains(ScaleFlags::VERTICAL_SNAP))
        || (!is_vertical && !scale.flags.contains(ScaleFlags::HORIZONTAL_SNAP))
    {
        if group == ScriptGroup::Default {
            if stem_flags.contains(TopoFlags::SERIF) && is_vertical && (dist < 3 * 64) {
                return dist * sign;
            } else if base_flags.contains(TopoFlags::ROUND) {
                if dist < 80 {
                    dist = 64;
                }
            } else if dist < 56 {
                dist = 56;
            }
        }
        if !metrics.widths.is_empty() {
            let min_width = metrics.widths[0].scaled;
            let delta = (dist - min_width).abs();
            if delta < 40 {
                dist = min_width.max(48);
                return dist * sign;
            }
            if group == ScriptGroup::Default {
                if dist < 3 * 64 {
                    let delta = dist & 63;
                    dist &= -64;
                    if delta < 10 {
                        dist += delta;
                    } else if delta < 32 {
                        dist += 10;
                    } else if delta < 54 {
                        dist += 54;
                    } else {
                        dist += delta;
                    }
                } else {
                    let mut new_base_delta = 0;
                    if (width > 0 && base_delta > 0) || (width < 0 && base_delta < 0) {
                        if scale.size < 10.0 {
                            new_base_delta = base_delta;
                        } else if scale.size < 30.0 {
                            new_base_delta = (base_delta * (30.0 - scale.size) as i32) / 20;
                        }
                    }
                    dist = (dist - new_base_delta.abs() + 32) & !63;
                }
            }
        }
        if group != ScriptGroup::Default {
            if dist < 54 {
                dist += (54 - dist) / 2;
            } else if dist < 3 * 64 {
                let delta = dist & 63;
                dist &= -64;
                if delta < 10 {
                    dist += delta;
                } else if delta < 22 {
                    dist += 10;
                } else if delta < 42 {
                    dist += delta;
                } else if delta < 54 {
                    dist += 54;
                } else {
                    dist += delta;
                }
            }
        }
    } else {
        let original_dist = dist;
        dist = snap_width(&metrics.widths, dist);
        if is_vertical {
            if dist >= 64 {
                dist = (dist + 16) & !63;
            } else {
                dist = 64;
            }
        } else if scale.flags.contains(ScaleFlags::MONO) {
            if dist < 64 {
                dist = 64;
            } else {
                dist = (dist + 32) & !63;
            }
        } else {
            if dist < 48 {
                dist = (dist + 64) >> 1;
            } else if dist < 128 {
                dist = (dist + 22) & !63;
                if group == ScriptGroup::Default {
                    let delta = (dist - original_dist).abs();
                    if delta >= 16 {
                        dist = original_dist;
                        if dist < 48 {
                            dist = (dist + 64) >> 1;
                        }
                    }
                }
            } else {
                dist = (dist + 32) & !63;
            }
        }
    }
    dist * sign
}

fn align_linked_edge(
    axis: &mut Axis,
    metrics: &ScaledAxisMetrics,
    group: ScriptGroup,
    scale: &Scale,
    base_edge_ix: usize,
    stem_edge_ix: usize,
) {
    let edges = axis.edges.as_mut_slice();
    let base_edge = &edges[base_edge_ix];
    let stem_edge = &edges[stem_edge_ix];
    let width = stem_edge.opos - base_edge.opos;
    let base_delta = base_edge.pos - base_edge.opos;
    let fitted_width = stem_width(
        metrics,
        group,
        scale,
        width,
        base_delta,
        base_edge.flags,
        stem_edge.flags,
    );
    edges[stem_edge_ix].pos = base_edge.pos + fitted_width;
}

fn align_serif_edge(axis: &mut Axis, base_edge_ix: usize, serif_edge_ix: usize) {
    let edges = axis.edges.as_mut_slice();
    let base_edge = &edges[base_edge_ix];
    let serif_edge = &edges[serif_edge_ix];
    edges[serif_edge_ix].pos = base_edge.pos + (serif_edge.opos - base_edge.opos);
}

fn hint_normal_stem_cjk(
    axis: &mut Axis,
    metrics: &ScaledAxisMetrics,
    group: ScriptGroup,
    scale: &Scale,
    edge_ix: usize,
    edge2_ix: usize,
    anchor: i32,
) -> i32 {
    const MAX_HORIZONTAL_GAP: i32 = 9;
    const MAX_VERTICAL_GAP: i32 = 15;
    const MAX_DELTA_ABS: i32 = 14;
    let edge = axis.edges[edge_ix];
    let edge2 = axis.edges[edge2_ix];
    let do_stem_adjust = scale.flags.contains(ScaleFlags::STEM_ADJUST);
    let threshold_delta = if do_stem_adjust {
        0
    } else {
        let delta = if axis.dim == Dimension::Vertical {
            MAX_HORIZONTAL_GAP
        } else {
            MAX_VERTICAL_GAP
        };
        if edge.flags.contains(TopoFlags::ROUND) && edge2.flags.contains(TopoFlags::ROUND) {
            delta
        } else {
            delta / 3
        }
    };
    let threshold = 64 - threshold_delta;
    let original_len = edge2.opos - edge.opos;
    let cur_len = stem_width(
        metrics,
        group,
        scale,
        original_len,
        0,
        edge.flags,
        edge2.flags,
    );
    let original_center = (edge.opos + edge2.opos) / 2 + anchor;
    let cur_pos1 = original_center - cur_len / 2;
    let cur_pos2 = cur_pos1 + cur_len;
    let mut finish = |mut delta: i32| {
        if !do_stem_adjust {
            delta = delta.clamp(-MAX_DELTA_ABS, MAX_DELTA_ABS);
        }
        let adjustment = cur_pos1 + delta;
        if edge.opos < edge2.opos {
            axis.edges[edge_ix].pos = adjustment;
            axis.edges[edge2_ix].pos = adjustment + cur_len;
        } else {
            axis.edges[edge2_ix].pos = adjustment;
            axis.edges[edge_ix].pos = adjustment + cur_len;
        }
        delta
    };
    let mut d_off1 = cur_pos1 - pix_floor(cur_pos1);
    let mut d_off2 = cur_pos2 - pix_floor(cur_pos2);
    let mut delta = 0;
    if d_off1 == 0 || d_off2 == 0 {
        return finish(delta);
    }
    let mut u_off1 = 64 - d_off1;
    let mut u_off2 = 64 - d_off2;
    if cur_len <= threshold {
        if d_off2 < cur_len {
            delta = if u_off1 <= d_off2 { u_off1 } else { -d_off2 };
        }
        return finish(delta);
    }
    if threshold < 64
        && (d_off1 >= threshold
            || u_off1 >= threshold
            || d_off2 >= threshold
            || u_off2 >= threshold)
    {
        return finish(delta);
    }
    let mut offset = cur_len & 63;
    if offset < 32 {
        if u_off1 <= offset || d_off2 <= offset {
            return finish(delta);
        }
    } else {
        offset = 64 - threshold;
    }
    d_off1 = threshold - u_off1;
    u_off1 -= offset;
    u_off2 = threshold - d_off2;
    d_off2 -= offset;
    if d_off1 <= u_off1 {
        u_off1 = -d_off1;
    }
    if d_off2 <= u_off2 {
        u_off2 = -d_off2;
    }
    if u_off1.abs() <= u_off2.abs() {
        delta = u_off1;
    } else {
        delta = u_off2;
    }
    finish(delta)
}
