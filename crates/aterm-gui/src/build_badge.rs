// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Top-right paint-only build/version pill (`v{version} · {build}`).

#[cfg(test)]
use aterm_render::Theme;

use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::row_baseline;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// Returns the `v{version} · {build}` caption.
pub(crate) fn badge_text() -> String {
    format!(
        "v{} \u{00b7} {}",
        crate::build_info::version_display(),
        crate::build_info::build_number()
    )
}

/// Non-zero repaint fingerprint when enabled, or `0` when disabled.
pub(crate) fn fingerprint(enabled: bool) -> u64 {
    if !enabled {
        return 0;
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    crate::build_info::version_display().hash(&mut h);
    crate::build_info::build_number().hash(&mut h);
    h.finish() | 1
}

/// Builds the top-right badge tray and its bounding card. Its type floor, corner radius cap and
/// border are points, scaled by [`SettingsGeom::px_per_pt`], so a Retina badge is the 1x badge at
/// twice the pixels.
pub(crate) fn badge_tray(
    g: &SettingsGeom,
    theme: impl Into<crate::chrome_band::ChromePalette>,
) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let pt = g.px_per_pt;
    let tray_w = g.cols as f32 * cw;

    let text = badge_text();
    let size = TypeStep::Caption.px_clamped(px, 9.0 * pt, f32::INFINITY);
    let tw = text_w(&text, size.get());

    let pad_x = 0.55 * cw;
    let pad_y = 0.22 * ch;
    let pill_w = tw + 2.0 * pad_x;
    let pill_h = size.get() + 2.0 * pad_y;
    let margin_r = 0.6 * cw;
    let margin_t = 0.28 * ch;
    let x = (tray_w - pill_w - margin_r).max(0.0);
    let y = margin_t;
    let radius = (pill_h * 0.5).min(11.0 * pt);

    let prims: Vec<DrawPrim> = vec![
        DrawPrim::Panel {
            x,
            y,
            w: pill_w,
            h: pill_h,
            radius,
            fill: rgba(r.elevated, 0xC8),
        },
        DrawPrim::Stroke {
            x,
            y,
            w: pill_w,
            h: pill_h,
            radius,
            width: pt,
            color: rgba(r.separator, 0x80),
        },
        text_prim(
            x + pad_x,
            row_baseline(y, pill_h, size.get()),
            text,
            size,
            TextWeight::Regular,
            TextFace::Mono,
            rgba(r.text_secondary, 0xE0),
        ),
    ];

    TrayInput {
        prims,
        card: (x, y, pill_w, pill_h),
    }
}

#[cfg(test)]
#[path = "build_badge_tests.rs"]
mod tests;
