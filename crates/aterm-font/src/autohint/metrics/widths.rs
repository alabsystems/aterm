// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/metrics/widths.rs and modified by the
// aterm project in 2026: reading outlines through aterm-font; its tests need
// fontations' test fonts and are not carried.
// See ../../../LICENSE-FONTATIONS for the upstream license.

//! Latin standard stem width computation.

use super::super::{
    derived_constant,
    fixed::F2Dot14,
    metrics::{self, MAX_WIDTHS, UnscaledWidths, WidthMetrics},
    outline::Outline,
    shape::{ShapedCluster, Shaper},
    style::{ScriptGroup, StyleClass},
    topo::{Axis, Dimension, compute_segments, link_segments},
};

/// Compute all stem widths and initialize standard width and height for the
/// given script.
///
/// Returns width metrics and unscaled widths for each dimension.
///
/// See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/aflatin.c#L54>
pub(super) fn compute_widths(
    shaper: &Shaper,
    coords: &[F2Dot14],
    style: &StyleClass,
) -> [(WidthMetrics, UnscaledWidths); 2] {
    let mut result: [(WidthMetrics, UnscaledWidths); 2] = Default::default();
    let font = shaper.font();
    let units_per_em = font.units_per_em();
    let mut outline = Outline::default();
    let mut axis = Axis::default();
    let mut cluster_shaper = shaper.cluster_shaper(style);
    let mut shaped_cluster = ShapedCluster::default();
    // We take the first available glyph from the standard character set.
    let glyph = style
        .script
        .std_chars
        .split(' ')
        .filter_map(|cluster| {
            cluster_shaper.shape(cluster, &mut shaped_cluster);
            // Reject input that maps to more than a single glyph
            // See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/aflatin.c#L128>
            match shaped_cluster.as_slice() {
                [glyph] if glyph.id.to_u32() != 0 => font.outline(glyph.id),
                _ => None,
            }
        })
        .next();
    if let Some(glyph) = glyph {
        if outline.fill(font, &glyph, coords).is_ok() && !outline.points.is_empty() {
            // Now process each dimension
            for (dim, (_metrics, widths)) in [Dimension::Horizontal, Dimension::Vertical]
                .into_iter()
                .zip(result.iter_mut())
            {
                axis.reset(dim, outline.orientation);
                // Segment computation for widths always uses the default
                // script group
                compute_segments(&mut outline, &mut axis, ScriptGroup::Default);
                link_segments(&outline, &mut axis, 0, ScriptGroup::Default, None);
                let segments = axis.segments.as_slice();
                for (segment_ix, segment) in segments.iter().enumerate() {
                    let segment_ix = segment_ix as u16;
                    let Some(link_ix) = segment.link_ix else {
                        continue;
                    };
                    let link = &segments[link_ix as usize];
                    if link_ix > segment_ix && link.link_ix == Some(segment_ix) {
                        let dist = (segment.pos as i32 - link.pos as i32).abs();
                        if widths.len() < MAX_WIDTHS {
                            widths.push(dist);
                        } else {
                            break;
                        }
                    }
                }
                // FreeTypes `af_sort_and_quantize_widths()` has the side effect
                // of always updating the width count to 1 when we don't find
                // any...
                // See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afhints.c#L121>
                if widths.is_empty() {
                    widths.push(0);
                }
                // The value 100 is heuristic
                metrics::sort_and_quantize_widths(widths, units_per_em / 100);
            }
        }
    }
    for (metrics, widths) in result.iter_mut() {
        // Now set derived values
        let stdw = widths
            .first()
            .copied()
            .unwrap_or_else(|| derived_constant(units_per_em, 50));
        // Heuristic value of 20% of the smallest width
        metrics.edge_distance_threshold = stdw / 5;
        metrics.standard_width = stdw;
        metrics.is_extra_light = false;
    }
    result
}
