// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/hint/mod.rs and modified by the
// aterm project in 2026: the hint-plan recorder dropped.
// See ../../../LICENSE-FONTATIONS for the upstream license.

//! Entry point to hinting algorithm.

mod edges;
mod outline;

use super::{
    metrics::{Scale, ScaleFlags, UnscaledStyleMetrics, scale_style_metrics},
    outline::Outline,
    style::{GlyphStyle, ScriptGroup},
    topo::{self, Axis, Dimension},
};

/// Adjusted horizontal scale and outer edge positions for horizontal metrics adjustments.
#[derive(Copy, Clone, PartialEq, Default, Debug)]
pub(crate) struct EdgeMetrics {
    pub left_opos: i32,
    pub left_pos: i32,
    pub right_opos: i32,
    pub right_pos: i32,
}

#[derive(Copy, Clone, PartialEq, Default, Debug)]
pub(crate) struct HintedMetrics {
    pub x_scale: i32,
    /// This will be `None` when we've identified fewer than two horizontal edges in the outline.
    pub edge_metrics: Option<EdgeMetrics>,
}

/// Applies the complete hinting process to a latin outline.
pub(crate) fn hint_outline(
    outline: &mut Outline,
    metrics: &UnscaledStyleMetrics,
    scale: &Scale,
    glyph_style: Option<GlyphStyle>,
) -> HintedMetrics {
    let scaled_metrics = scale_style_metrics(metrics, *scale);
    let scale = &scaled_metrics.scale;
    let mut axis = Axis::default();
    let hint_top_to_bottom = metrics.style_class().script.hint_top_to_bottom;
    outline.scale(&scaled_metrics.scale);
    let mut hinted_metrics = HintedMetrics {
        x_scale: scale.x_scale,
        ..Default::default()
    };
    let group = metrics.style_class().script.group;
    if group == ScriptGroup::Default && scaled_metrics.axes[1].blues.is_empty() {
        return hinted_metrics;
    }
    for dim in [Dimension::Horizontal, Dimension::Vertical] {
        if (dim == Dimension::Horizontal && scale.flags.contains(ScaleFlags::NO_HORIZONTAL))
            || (dim == Dimension::Vertical && scale.flags.contains(ScaleFlags::NO_VERTICAL))
        {
            continue;
        }
        axis.reset(dim, outline.orientation);
        topo::compute_segments(outline, &mut axis, group);
        topo::link_segments(
            outline,
            &mut axis,
            scaled_metrics.axes[dim].scale,
            group,
            metrics.axes[dim].max_width(),
        );
        topo::compute_edges(
            &mut axis,
            &scaled_metrics.axes[dim],
            hint_top_to_bottom,
            scaled_metrics.scale.y_scale,
            group,
        );
        if dim == Dimension::Vertical {
            if group != ScriptGroup::Default
                || glyph_style
                    .map(|style| !style.is_non_base())
                    .unwrap_or(true)
            {
                topo::compute_blue_edges(
                    &mut axis,
                    scale,
                    &metrics.axes[dim].blues,
                    &scaled_metrics.axes[dim].blues,
                    group,
                );
            }
        } else {
            hinted_metrics.x_scale = scaled_metrics.axes[0].scale;
        }
        edges::hint_edges(
            &mut axis,
            &scaled_metrics.axes[dim],
            group,
            scale,
            hint_top_to_bottom,
        );
        outline::align_edge_points(outline, &axis, group, scale);
        outline::align_strong_points(outline, &mut axis);
        outline::align_weak_points(outline, dim);
        if dim == Dimension::Horizontal && axis.edges.len() > 1 {
            if let (Some(left), Some(right)) = (axis.edges.first(), axis.edges.last()) {
                hinted_metrics.edge_metrics = Some(EdgeMetrics {
                    left_pos: left.pos,
                    left_opos: left.opos,
                    right_pos: right.pos,
                    right_opos: right.opos,
                });
            }
        }
    }
    hinted_metrics
}
