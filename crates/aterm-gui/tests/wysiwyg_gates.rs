// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Source-scan gate: the THREE render paths (glass present, SIGUSR1 `snapshot`,
//! control-socket `image`) suppress the transient effects — visual-bell invert,
//! drag-drop wash, level-up glow — behind the SAME predicate,
//! `WindowState::overlay_open()`.
//!
//! # Why a source-scanning test
//!
//! The SACRED introspection invariant (app_introspect.rs header) is that a
//! capture is byte-identical to the presented frame. The 2026-07 audit found
//! three mutually inconsistent suppression policies: glass gated the invert and
//! wash on `settings().is_some()` only (About/Palette/Update overlays kept
//! inverting) and never gated the glow; `snapshot` gated all three on
//! `overlay_open()`; `image` gated nothing — so a capture during a bell flash,
//! a drag, or a level-up celebration LIED about the glass whenever the policies
//! disagreed. The compiler cannot see that three distant code sites implement
//! one policy, so this structural test does: it reads the committed sources and
//! fails when any transient-effect site stops consulting `overlay_open`.
//!
//! Each pattern below is matched on WHITESPACE-NORMALIZED source (newlines and
//! runs of spaces collapsed), so rustfmt churn cannot break it.

/// Collapse all whitespace runs to single spaces so multi-line call sites match.
fn normalized(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn glass_present_gates_all_three_transients_on_overlay_open() {
    let src = normalized("src/app_render.rs");
    for needle in [
        // bell invert + drag wash read the shared gate once...
        "let overlay_open = ws0.overlay_open();",
        "let invert = ws0.bell_flash.is_active(Instant::now()) && !overlay_open;",
        "let drag_hover = ws0.drag_hover && !overlay_open;",
        // ...and the level-up glow arm consults the SAME local.
        "self.level_up .as_ref() .filter(|_| !overlay_open)",
    ] {
        assert!(
            src.contains(needle),
            "glass present lost its overlay_open gate: `{needle}` not found in app_render.rs \
             — the capture paths gate on overlay_open(), so glass MUST too (SACRED WYSIWYG)"
        );
    }
    // The pre-audit policy must never come back.
    assert!(
        !src.contains("let settings_open = ws0.settings().is_some();"),
        "glass present regressed to the Settings-only suppression gate"
    );
}

#[test]
fn snapshot_and_image_gate_all_three_transients_on_overlay_open() {
    let src = normalized("src/app_introspect.rs");
    let gated_invert = "ws.bell_flash.is_active(Instant::now()) && !ws.overlay_open(),";
    let gated_drag = "if ws.drag_hover && !ws.overlay_open() {";
    let gated_glow = "} else if !ws.overlay_open() && let Some((wash_a, border_a)) = level_up_glow";
    // Two capture paths (snapshot + render_image) — each must carry each gate.
    for (needle, what) in [
        (gated_invert, "bell-invert"),
        (gated_drag, "drag-wash"),
        (gated_glow, "level-up glow"),
    ] {
        let n = src.matches(needle).count();
        assert!(
            n >= 2,
            "expected the {what} overlay_open gate in BOTH capture paths of \
             app_introspect.rs (snapshot + render_image), found {n} of `{needle}`"
        );
    }
    // No capture path may invert unconditionally (the pre-audit `image` policy).
    assert!(
        !src.contains("apply_bell_invert(&mut frame, ws.bell_flash.is_active(Instant::now()));"),
        "a capture path applies the bell invert without the overlay_open gate"
    );
}
