// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn geom() -> SettingsGeom {
    SettingsGeom {
        cw: 9.0,
        ch: 20.0,
        font_px: 14.0,
        cols: 120,
        panel_rows: 40,
        px_per_pt: 1.0,
    }
}

/// At 2 device pixels per point the badge is the 1x badge at twice the pixels: its type floor,
/// radius cap and border are points. A 10 px font puts the caption under its 9-point floor
/// and a 32 px cell rounds the pill past its 11-point cap, so both bind (§14.16).
#[test]
fn the_badge_wears_its_type_floor_radius_and_border_in_points() {
    let at = |k: f32| SettingsGeom {
        cw: 9.0 * k,
        ch: 32.0 * k,
        font_px: 10.0 * k,
        cols: 120,
        panel_rows: 40,
        px_per_pt: k,
    };
    let one = badge_tray(&at(1.0), Theme::default()).prims;
    let two = badge_tray(&at(2.0), Theme::default()).prims;
    let DrawPrim::Stroke { radius, .. } = one[1] else {
        panic!("the pill's border: {:?}", one[1]);
    };
    assert_eq!(radius, 11.0, "the 1x radius is at its cap");
    let DrawPrim::Text { px, .. } = one[2] else {
        panic!("the caption: {:?}", one[2]);
    };
    assert_eq!(px, 9.0, "the 1x caption is at its floor");
    // The pill's left edge and width follow the caption's measured advance.
    let measured = [("panel", 0), ("panel", 2), ("stroke", 0), ("stroke", 2)];
    crate::palette::assert_drawn_at_scale(&one, &two, 2.0, &measured, "badge");
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
