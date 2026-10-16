// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The top-right build/version badge: a faint [`DrawPrim`] pill showing `v{version} · {build}`.
//! Off by default (`show_build_badge`, Settings "Show floating version pill"). It is paint-only,
//! so it lives in its own `badge_card` slot behind the modal cards: an open overlay hides it, and
//! a native route composes it into its own tray (`App::compose_native_route_card`).

use aterm_render::Theme;

use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::row_baseline;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// The badge caption, e.g. `v0.59 · 1783203308`: the shared display version and
/// `build_info::build_number()`. Full provenance (compiler, commit, signature) is in About.
pub(crate) fn badge_text() -> String {
    format!(
        "v{} \u{00b7} {}",
        crate::build_info::version_display(),
        crate::build_info::build_number()
    )
}

/// A fingerprint of everything the badge paints, folded into the frame's `RepaintKey`
/// (`badge_fp`) so toggling the setting, or a version/build change on a live swap, forces
/// exactly one present. `0` means disabled, so an enabled badge is never `0`.
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

/// Build the badge tray: a small rounded pill pinned to the top-right with the
/// `v{version} · {build}` caption. `card` is the pill's own bounds, so the shared rasterizer
/// paints only that region.
pub(crate) fn badge_tray(g: &SettingsGeom, theme: Theme) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let tray_w = g.cols as f32 * cw;

    let text = badge_text();
    let size = TypeStep::Caption.px_clamped(px, 9.0, f32::INFINITY);
    let tw = text_w(&text, size.get());

    let pad_x = 0.55 * cw;
    let pad_y = 0.22 * ch;
    let pill_w = tw + 2.0 * pad_x;
    let pill_h = size.get() + 2.0 * pad_y;
    let margin_r = 0.6 * cw;
    let margin_t = 0.28 * ch;
    let x = (tray_w - pill_w - margin_r).max(0.0);
    let y = margin_t;
    let radius = (pill_h * 0.5).min(11.0);

    // Faint pill: a mostly-transparent fill and hairline, legible over whatever terminal
    // content is beneath it.
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
            width: 1.0,
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
mod tests {
    use super::*;

    fn geom() -> SettingsGeom {
        SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 120,
            panel_rows: 40,
        }
    }

    #[test]
    fn text_has_version_and_build() {
        let t = badge_text();
        assert!(t.starts_with('v'), "leads with v: {t}");
        // Compiler provenance is not on the badge; About and the control metadata carry it.
        assert!(
            t.contains(crate::build_info::version_display()),
            "has display version: {t}"
        );
        assert!(
            t.contains(crate::build_info::build_number()),
            "has build: {t}"
        );
        assert!(!t.contains('+'), "no provenance suffix on the badge: {t}");
    }

    #[test]
    fn fingerprint_zero_iff_disabled() {
        assert_eq!(fingerprint(false), 0, "disabled is the 0 sentinel");
        assert_ne!(fingerprint(true), 0, "enabled is never 0");
    }

    #[test]
    fn pill_hugs_top_right_within_tray() {
        let g = geom();
        let t = badge_tray(&g, Theme::default());
        let (x, y, w, h) = t.card;
        let tray_w = g.cols as f32 * g.cw;
        assert!(x >= 0.0 && x + w <= tray_w, "inside the tray horizontally");
        assert!(y >= 0.0, "top margin non-negative");
        assert!(x + w <= tray_w, "right-aligned");
        assert!(tray_w - (x + w) < x, "hugs the right edge");
        assert!(h < 3.0 * g.ch, "compact height");
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s.starts_with('v'))),
            "paints the caption"
        );
    }
}
