// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/topo/edges.rs and modified by the
// aterm project in 2026: the blue provenance hint plans record dropped; its
// tests need fontations' test fonts and are not carried (the TF.1 hinting
// corpus replays the whole hinter).
// See ../../../LICENSE-FONTATIONS for the upstream license.

//! Edge detection from outline segments, property assignment, and blue zone association.

use super::{
    super::{
        metrics::{Scale, ScaledAxisMetrics, ScaledBlue, UnscaledBlue, fixed_div, fixed_mul},
        outline::Direction,
        style::ScriptGroup,
    },
    Axis, Dimension, Edge, TopoFlags,
};

/// Links segments to edges, using feature analysis for selection.
pub(crate) fn compute_edges(
    axis: &mut Axis,
    metrics: &ScaledAxisMetrics,
    top_to_bottom_hinting: bool,
    y_scale: i32,
    group: ScriptGroup,
) {
    axis.edges.clear();
    let scale = metrics.scale;
    let top_to_bottom_hinting =
        if axis.dim == Dimension::Horizontal || group != ScriptGroup::Default {
            false
        } else {
            top_to_bottom_hinting
        };
    // Ignore horizontal segments less than 1 pixel in length.
    let segment_length_threshold = if axis.dim == Dimension::Horizontal {
        fixed_div(64, y_scale)
    } else {
        0
    };
    // Ignore segments with a width delta larger than 0.5 pixels.
    let segment_width_threshold = fixed_div(32, scale);
    // Cap edge distance threshold at 0.25 pixels.
    let initial_threshold = metrics.width_metrics.edge_distance_threshold;
    const EDGE_DISTANCE_THRESHOLD_MAX: i32 = 64 / 4;
    let edge_distance_threshold = if group == ScriptGroup::Default {
        fixed_div(
            fixed_mul(initial_threshold, scale).min(EDGE_DISTANCE_THRESHOLD_MAX),
            scale,
        )
    } else {
        let threshold = fixed_mul(initial_threshold, scale);
        if threshold > EDGE_DISTANCE_THRESHOLD_MAX {
            fixed_div(EDGE_DISTANCE_THRESHOLD_MAX, scale)
        } else {
            initial_threshold
        }
    };
    for segment_ix in 0..axis.segments.len() {
        let segment = &axis.segments[segment_ix];
        if group == ScriptGroup::Default {
            // Ignore segments that are too short, too wide or direction-less
            if (segment.height as i32) < segment_length_threshold
                || (segment.delta as i32 > segment_width_threshold)
                || segment.dir == Direction::None
            {
                continue;
            }
            // Ignore serif edges smaller than 1.5 pixels.
            if segment.serif_ix.is_some()
                && (2 * segment.height as i32) < (3 * segment_length_threshold)
            {
                continue;
            }
        }
        let mut best_dist = i32::MAX;
        let mut best_edge_ix = None;
        for edge_ix in 0..axis.edges.len() {
            let edge = &axis.edges[edge_ix];
            let dist = (segment.pos as i32 - edge.fpos as i32).abs();
            if dist < edge_distance_threshold && edge.dir == segment.dir && dist < best_dist {
                if group == ScriptGroup::Default {
                    best_edge_ix = Some(edge_ix);
                    break;
                }
                if let Some(link) = segment.link(&axis.segments).copied() {
                    // Check whether all linked segments of the candidate edge can form a single
                    // edge.
                    let first_ix = edge.first_ix as usize;
                    let mut seg1 = &axis.segments[first_ix];
                    let mut dist2 = 0;
                    loop {
                        if let Some(link1) = seg1.link(&axis.segments).copied() {
                            dist2 = (link.pos as i32 - link1.pos as i32).abs();
                            if dist2 >= edge_distance_threshold {
                                break;
                            }
                        }
                        if seg1.edge_next_ix == Some(first_ix as u16) {
                            break;
                        }
                        if let Some(next) = seg1.next_in_edge(&axis.segments) {
                            seg1 = next;
                        } else {
                            break;
                        }
                    }
                    if dist2 >= edge_distance_threshold {
                        continue;
                    }
                }
                best_dist = dist;
                best_edge_ix = Some(edge_ix);
            }
        }
        if let Some(edge_ix) = best_edge_ix {
            axis.append_segment_to_edge(segment_ix, edge_ix);
        } else {
            let opos = fixed_mul(segment.pos as i32, scale);
            let edge = Edge {
                fpos: segment.pos,
                opos,
                pos: opos,
                dir: segment.dir,
                first_ix: segment_ix as u16,
                last_ix: segment_ix as u16,
                ..Default::default()
            };
            axis.insert_edge(edge, top_to_bottom_hinting);
            axis.segments[segment_ix].edge_next_ix = Some(segment_ix as u16);
        }
    }
    if group == ScriptGroup::Default {
        // Associate directionless single-point segments with coinciding edges.
        for segment_ix in 0..axis.segments.len() {
            let segment = &axis.segments[segment_ix];
            if segment.dir != Direction::None {
                continue;
            }
            if let Some(edge_ix) = axis
                .edges
                .iter()
                .enumerate()
                .filter_map(|(ix, edge)| {
                    ((segment.pos as i32 - edge.fpos as i32).abs() < edge_distance_threshold)
                        .then_some(ix)
                })
                .next()
            {
                axis.append_segment_to_edge(segment_ix, edge_ix);
            }
        }
    }
    link_segments_to_edges(axis);
    compute_edge_properties(axis);
}

/// Assigns edge indices to segments after edge sorting stabilizes.
fn link_segments_to_edges(axis: &mut Axis) {
    let segments = axis.segments.as_mut_slice();
    for edge_ix in 0..axis.edges.len() {
        let edge = &axis.edges[edge_ix];
        let mut ix = edge.first_ix as usize;
        let last_ix = edge.last_ix as usize;
        loop {
            let segment = &mut segments[ix];
            segment.edge_ix = Some(edge_ix as u16);
            if ix == last_ix {
                break;
            }
            ix = segment
                .edge_next_ix
                .map(|ix| ix as usize)
                .unwrap_or(last_ix);
        }
    }
}

/// Compute the edge properties based on the series of segments that make
/// up the edge.
fn compute_edge_properties(axis: &mut Axis) {
    let edges = axis.edges.as_mut_slice();
    let segments = axis.segments.as_slice();
    for edge_ix in 0..edges.len() {
        let mut roundness = 0;
        let mut straightness = 0;
        let edge = edges[edge_ix];
        let mut segment_ix = edge.first_ix as usize;
        let last_segment_ix = edge.last_ix as usize;
        loop {
            let edge = edges[edge_ix];
            let segment = &segments[segment_ix];
            let next_segment_ix = segment.edge_next_ix;
            if segment.flags.contains(TopoFlags::ROUND) {
                roundness += 1;
            } else {
                straightness += 1;
            }
            let is_serif = if let Some(serif_ix) = segment.serif_ix {
                let serif = &segments[serif_ix as usize];
                serif.edge_ix.is_some() && serif.edge_ix != Some(edge_ix as u16)
            } else {
                false
            };
            if is_serif
                || (segment.link_ix.is_some()
                    && segments[segment.link_ix.unwrap() as usize]
                        .edge_ix
                        .is_some())
            {
                let (edge2_ix, segment2_ix) = if is_serif {
                    (edge.serif_ix, segment.serif_ix)
                } else {
                    (edge.link_ix, segment.link_ix)
                };
                let edge2_ix = if let (Some(edge2_ix), Some(segment2_ix)) = (edge2_ix, segment2_ix)
                {
                    let edge2 = &edges[edge2_ix as usize];
                    let edge_delta = (edge.fpos as i32 - edge2.fpos as i32).abs();
                    let segment2 = &segments[segment2_ix as usize];
                    let segment_delta = (segment.pos as i32 - segment2.pos as i32).abs();
                    if segment_delta < edge_delta {
                        segment2.edge_ix
                    } else {
                        Some(edge2_ix)
                    }
                } else if let Some(segment2_ix) = segment2_ix {
                    segments[segment2_ix as usize].edge_ix
                } else {
                    edge2_ix
                };
                if is_serif {
                    edges[edge_ix].serif_ix = edge2_ix;
                    edges[edge2_ix.unwrap() as usize].flags |= TopoFlags::SERIF;
                } else {
                    edges[edge_ix].link_ix = edge2_ix;
                }
            }
            if segment_ix == last_segment_ix {
                break;
            }
            segment_ix = next_segment_ix
                .map(|ix| ix as usize)
                .unwrap_or(last_segment_ix);
        }
        let edge = &mut edges[edge_ix];
        edge.flags = TopoFlags::NORMAL;
        if roundness > 0 && roundness >= straightness {
            edge.flags |= TopoFlags::ROUND;
        }
        if edge.serif_ix.is_some() && edge.link_ix.is_some() {
            edge.serif_ix = None;
        }
    }
}

/// Computes all edges that lie within blue zones.
pub(crate) fn compute_blue_edges(
    axis: &mut Axis,
    scale: &Scale,
    unscaled_blues: &[UnscaledBlue],
    blues: &[ScaledBlue],
    group: ScriptGroup,
) {
    if axis.dim != Dimension::Vertical && group == ScriptGroup::Default {
        return;
    }
    let axis_scale = if axis.dim == Dimension::Horizontal {
        scale.x_scale
    } else {
        scale.y_scale
    };
    let initial_best_dest = fixed_mul(scale.units_per_em / 40, axis_scale).min(64 / 2);
    for edge in &mut axis.edges {
        let mut best_blue = None;
        let mut best_is_neutral = false;
        let mut best_dist = initial_best_dest;
        for (unscaled_blue, blue) in unscaled_blues.iter().zip(blues) {
            if !blue.is_active {
                continue;
            }
            let is_top = blue.zones.is_top_like();
            let is_neutral = blue.zones.is_neutral();
            let is_major_dir = edge.dir == axis.major_dir;
            if is_top ^ is_major_dir || is_neutral {
                let (ref_pos, matching_blue) = if group == ScriptGroup::Default {
                    (unscaled_blue.position, blue.position)
                } else {
                    if (edge.fpos as i32 - unscaled_blue.position).abs()
                        > (edge.fpos as i32 - unscaled_blue.overshoot).abs()
                    {
                        (unscaled_blue.overshoot, blue.overshoot)
                    } else {
                        (unscaled_blue.position, blue.position)
                    }
                };
                let dist = fixed_mul((edge.fpos as i32 - ref_pos).abs(), axis_scale);
                if dist < best_dist {
                    best_dist = dist;
                    best_blue = Some(matching_blue);
                    best_is_neutral = is_neutral;
                }
                if group == ScriptGroup::Default {
                    // Now compare to overshoot position for the default script
                    // group.
                    if edge.flags.contains(TopoFlags::ROUND) && dist != 0 && !is_neutral {
                        let is_under_ref = (edge.fpos as i32) < unscaled_blue.position;
                        if is_top ^ is_under_ref {
                            let dist = fixed_mul(
                                (edge.fpos as i32 - unscaled_blue.overshoot).abs(),
                                axis_scale,
                            );
                            if dist < best_dist {
                                best_dist = dist;
                                best_blue = Some(blue.overshoot);
                                best_is_neutral = is_neutral;
                            }
                        }
                    }
                }
            }
        }
        if let Some(best_blue) = best_blue {
            edge.blue_edge = Some(best_blue);
            if best_is_neutral {
                edge.flags |= TopoFlags::NEUTRAL;
            }
        }
    }
}
