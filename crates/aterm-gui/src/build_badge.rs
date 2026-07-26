// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The subtle top-right BUILD/VERSION badge: a tiny, faint [`DrawPrim`] pill showing
//! `v{version} · {build}` so "which build am I on" is answerable at a glance without
//! opening About. Default OFF, toggleable via `show_build_badge` or the native Settings
//! tab ▸ Window ▸ "Show build/version badge". It is NON-INTERACTIVE (paint only) — it never
//! captures the mouse — so it
//! lives in its OWN paint-only `badge_card` slot rather than the modal `settings_card`,
//! and the composite picks the modal FIRST (`settings_card.or(badge_card)`): an About or
//! palette overlay covers the badge while open, and it returns when that overlay
//! closes. Built from the SAME [`DrawPrim`] chrome vocabulary as the About dialog (a
//! rounded pill, hairline border, dim text) so it reads as native window chrome, not
//! terminal grid cells. ONE structured source ([`crate::build_info`]) drives it.

use aterm_render::Theme;

use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::row_baseline;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// The badge caption, e.g. `v0.59 · 1783203308` — the shared app/source display
/// version plus the build number (a release's `RELEASES.ledger` claim; HEAD's
/// committer epoch on a dev build — see `build_info::BUILD_NUMBER`). Compact by
/// design — a glanceable identity, not the full provenance (compiler flavor /
/// commit / signature live in About).
pub(crate) fn badge_text() -> String {
    format!(
        "v{} \u{00b7} {}",
        crate::build_info::version_display(),
        crate::build_info::BUILD_NUMBER
    )
}

/// A fingerprint of everything the badge paints, folded into the frame's `RepaintKey`
/// (`badge_fp`) so toggling the setting — or a version/build bump on a live swap —
/// forces exactly one present. `0` is the DISABLED/absent sentinel (matching the overlay
/// `0`-is-closed convention), so an enabled badge is never `0`.
pub(crate) fn fingerprint(enabled: bool) -> u64 {
    if !enabled {
        return 0;
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    crate::build_info::version_display().hash(&mut h);
    crate::build_info::BUILD_NUMBER.hash(&mut h);
    h.finish() | 1
}

/// Build the badge tray: a small rounded pill pinned to the TOP-RIGHT of the tray with
/// the `v{version} · {build}` caption. `card` is the pill's own bounds (top-right), so
/// the shared rasterizer only paints that small region — not a full-frame canvas. PURE
/// DrawPrims, same vocabulary as [`crate::about`].
pub(crate) fn badge_tray(g: &SettingsGeom, theme: Theme) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let tray_w = g.cols as f32 * cw;

    let text = badge_text();
    // Caption step (px*0.72 snaps to the 0.8 Caption factor), floored at 9px.
    let size = TypeStep::Caption.px_clamped(px, 9.0, f32::INFINITY);
    let tw = text_w(&text, size.get());

    // Pill geometry: hug the text, pinned to the top-right with a small margin.
    let pad_x = 0.55 * cw;
    let pad_y = 0.22 * ch;
    let pill_w = tw + 2.0 * pad_x;
    let pill_h = size.get() + 2.0 * pad_y;
    let margin_r = 0.6 * cw;
    let margin_t = 0.28 * ch;
    let x = (tray_w - pill_w - margin_r).max(0.0);
    let y = margin_t;
    let radius = (pill_h * 0.5).min(11.0);

    // Faint pill: a mostly-transparent surface fill + hairline, so the badge floats over
    // whatever terminal content is beneath it while staying legible — subtle, not a
    // solid chrome bar.
    let prims: Vec<DrawPrim> = vec![
        DrawPrim::Panel {
            x,
            y,
            w: pill_w,
            h: pill_h,
            radius,
            fill: rgba(r.elevated, 0xC8),
            blur: false,
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
        // The badge shows the shared app/source display version + the build
        // number. Compiler provenance is not encoded here; it has its own
        // About/control metadata rows.
        assert!(
            t.contains(crate::build_info::version_display()),
            "has display version: {t}"
        );
        assert!(
            t.contains(crate::build_info::BUILD_NUMBER),
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
        // Pinned right: the gap on the right is smaller than the gap on the left.
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
