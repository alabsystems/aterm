// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/metrics/scale.rs and modified by the
// aterm project in 2026: the ahead-of-time quirks and the advance-only digit width dropped.
// See ../../../LICENSE-FONTATIONS for the upstream license.

//! Unscaled and scaled style metrics computation and blue zone activation.

use super::super::{
    fixed::F2Dot14,
    metrics::{
        BlueZones, Scale, ScaledAxisMetrics, ScaledBlue, ScaledStyleMetrics, ScaledWidth,
        UnscaledAxisMetrics, UnscaledBlue, UnscaledStyleMetrics, WidthMetrics, fixed_div,
        fixed_mul, fixed_mul_div, pix_round,
    },
    shape::Shaper,
    style::{ScriptGroup, StyleClass},
    topo::Dimension,
};

/// Computes unscaled metrics for the Latin writing system.
pub(crate) fn compute_unscaled_style_metrics(
    shaper: &Shaper,
    coords: &[F2Dot14],
    style: &StyleClass,
) -> UnscaledStyleMetrics {
    let charmap = shaper.charmap();
    if charmap.is_symbol() {
        return UnscaledStyleMetrics {
            class_ix: style.index as u16,
            axes: [
                UnscaledAxisMetrics {
                    dim: Dimension::Horizontal,
                    ..Default::default()
                },
                UnscaledAxisMetrics {
                    dim: Dimension::Vertical,
                    ..Default::default()
                },
            ],
        };
    }
    let [hwidths, vwidths] = super::widths::compute_widths(shaper, coords, style);
    let [hblues, vblues] = super::blues::compute_unscaled_blues(shaper, coords, style);
    UnscaledStyleMetrics {
        class_ix: style.index as u16,
        axes: [
            UnscaledAxisMetrics {
                dim: Dimension::Horizontal,
                blues: hblues,
                width_metrics: hwidths.0,
                widths: hwidths.1,
            },
            UnscaledAxisMetrics {
                dim: Dimension::Vertical,
                blues: vblues,
                width_metrics: vwidths.0,
                widths: vwidths.1,
            },
        ],
    }
}

/// Computes scaled metrics for the Latin or CJK writing system.
pub(crate) fn scale_style_metrics(
    unscaled_metrics: &UnscaledStyleMetrics,
    mut scale: Scale,
) -> ScaledStyleMetrics {
    let scale_axis_fn = if unscaled_metrics.style_class().script.group == ScriptGroup::Default {
        scale_default_axis_metrics
    } else {
        scale_cjk_axis_metrics
    };
    let mut scale_axis = |axis: &UnscaledAxisMetrics| {
        scale_axis_fn(
            axis.dim,
            &axis.widths,
            axis.width_metrics,
            &axis.blues,
            &mut scale,
        )
    };
    let axes = [
        scale_axis(&unscaled_metrics.axes[0]),
        scale_axis(&unscaled_metrics.axes[1]),
    ];
    ScaledStyleMetrics { scale, axes }
}

/// Computes scaled metrics for a single axis.
fn scale_default_axis_metrics(
    dim: Dimension,
    widths: &[i32],
    width_metrics: WidthMetrics,
    blues: &[UnscaledBlue],
    scale: &mut Scale,
) -> ScaledAxisMetrics {
    let mut axis = ScaledAxisMetrics {
        dim,
        ..Default::default()
    };
    if dim == Dimension::Horizontal {
        axis.scale = scale.x_scale;
        axis.delta = scale.x_delta;
    } else {
        axis.scale = scale.y_scale;
        axis.delta = scale.y_delta;
    };
    if let Some(blue_ix) = blues
        .iter()
        .position(|blue| blue.zones.contains(BlueZones::ADJUSTMENT))
    {
        let unscaled_blue = &blues[blue_ix];
        let scaled = fixed_mul(axis.scale, unscaled_blue.overshoot);
        let fitted = (scaled + 40) & !63;
        if scaled != fitted && dim == Dimension::Vertical {
            let new_scale = fixed_mul_div(axis.scale, fitted, scaled);
            let mut max_height = scale.units_per_em;
            for blue in blues {
                max_height = max_height.max(blue.ascender).max(-blue.descender);
            }
            let mut dist = fixed_mul(max_height, new_scale - axis.scale).abs();
            dist &= !127;
            if dist == 0 {
                axis.scale = new_scale;
                scale.y_scale = new_scale;
            }
        }
    }
    axis.width_metrics = width_metrics;
    for unscaled_width in widths {
        let scaled = fixed_mul(axis.scale, *unscaled_width);
        axis.widths.push(ScaledWidth {
            scaled,
            fitted: scaled,
        });
    }
    axis.width_metrics.is_extra_light =
        fixed_mul(axis.width_metrics.standard_width, axis.scale) < (32 + 8);
    if dim == Dimension::Vertical {
        for unscaled_blue in blues {
            let scaled_position = fixed_mul(axis.scale, unscaled_blue.position) + axis.delta;
            let scaled_overshoot = fixed_mul(axis.scale, unscaled_blue.overshoot) + axis.delta;
            let mut blue = ScaledBlue {
                position: ScaledWidth {
                    scaled: scaled_position,
                    fitted: scaled_position,
                },
                overshoot: ScaledWidth {
                    scaled: scaled_overshoot,
                    fitted: scaled_overshoot,
                },
                zones: unscaled_blue.zones,
                is_active: false,
            };
            let dist = fixed_mul(unscaled_blue.position - unscaled_blue.overshoot, axis.scale);
            if (-48..=48).contains(&dist) {
                let mut delta = dist.abs();
                if delta < 32 {
                    delta = 0;
                } else if delta < 48 {
                    delta = 32;
                } else {
                    delta = 64;
                }
                if dist < 0 {
                    delta = -delta;
                }
                blue.position.fitted = pix_round(blue.position.scaled);
                blue.overshoot.fitted = blue.position.fitted - delta;
                blue.is_active = true;
            }
            axis.blues.push(blue);
        }
        for blue_ix in 0..axis.blues.len() {
            let blue = axis.blues[blue_ix];
            if !blue.zones.is_sub_top() || !blue.is_active {
                continue;
            }
            for blue2 in &axis.blues {
                if blue2.zones.is_sub_top() || !blue2.is_active {
                    continue;
                }
                if blue2.position.fitted <= blue.overshoot.fitted
                    && blue2.overshoot.fitted >= blue.position.fitted
                {
                    axis.blues[blue_ix].is_active = false;
                    break;
                }
            }
        }
    }
    axis
}

/// Computes scaled metrics for a single axis for the CJK script group.
fn scale_cjk_axis_metrics(
    dim: Dimension,
    widths: &[i32],
    width_metrics: WidthMetrics,
    blues: &[UnscaledBlue],
    scale: &mut Scale,
) -> ScaledAxisMetrics {
    let mut axis = ScaledAxisMetrics {
        dim,
        ..Default::default()
    };
    axis.dim = dim;
    if dim == Dimension::Horizontal {
        axis.scale = scale.x_scale;
        axis.delta = scale.x_delta;
    } else {
        axis.scale = scale.y_scale;
        axis.delta = scale.y_delta;
    };
    let scale = axis.scale;
    for unscaled_blue in blues {
        let position = fixed_mul(unscaled_blue.position, scale) + axis.delta;
        let overshoot = fixed_mul(unscaled_blue.overshoot, scale) + axis.delta;
        let mut blue = ScaledBlue {
            position: ScaledWidth {
                scaled: position,
                fitted: position,
            },
            overshoot: ScaledWidth {
                scaled: overshoot,
                fitted: overshoot,
            },
            zones: unscaled_blue.zones,
            is_active: false,
        };
        let dist = fixed_mul(unscaled_blue.position - unscaled_blue.overshoot, scale);
        if (-48..=48).contains(&dist) {
            blue.position.fitted = pix_round(blue.position.scaled);
            let delta1 = fixed_div(blue.position.fitted, scale) - unscaled_blue.overshoot;
            let mut delta2 = fixed_mul(delta1.abs(), scale);
            if delta2 < 32 {
                delta2 = 0;
            } else {
                delta2 = pix_round(delta2);
            }
            if delta1 < 0 {
                delta2 = -delta2;
            }
            blue.overshoot.fitted = blue.position.fitted - delta2;
            blue.is_active = true;
        }
        axis.blues.push(blue);
    }
    for _ in 0..widths.len() {
        axis.widths.push(ScaledWidth::default());
    }
    axis.width_metrics = width_metrics;
    axis
}
