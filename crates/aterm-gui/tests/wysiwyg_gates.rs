// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Source-scan gate: the THREE app-owned render paths (application-present
//! composition, SIGUSR1 `snapshot`, control-socket `image`) suppress the
//! transient effects — visual-bell invert, drag-drop wash, level-up glow —
//! behind the SAME predicate, `WindowState::overlay_open()`.
//!
//! # Why a source-scanning test
//!
//! The invariant tested here is narrower: application-present composition,
//! `snapshot`, and `image` use the same overlay-open suppression policy for
//! these three transient effects. The 2026-07 audit found mutually inconsistent
//! policies across those paths. The compiler cannot see that three distant code
//! sites implement one policy, so this structural test reads the committed
//! sources and fails when any transient-effect site stops consulting
//! `overlay_open`. It does not inspect WSI, compositor selection, or scanout.
//!
//! Each pattern below is matched on WHITESPACE-NORMALIZED source (newlines and
//! runs of spaces collapsed), so rustfmt churn cannot break it.

fn source(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Collapse all whitespace runs to single spaces so multi-line call sites match.
fn normalized(rel: &str) -> String {
    source(rel).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whitespace-normalized source between two stable declaration/arm markers.
///
/// Keeping assertions inside the actual call-chain region avoids a vacuous pass
/// merely because the opposite API appears elsewhere in the same source file.
fn normalized_section(rel: &str, start: &str, end: &str) -> String {
    let src = source(rel);
    let start_at = src
        .find(start)
        .unwrap_or_else(|| panic!("`{start}` missing from {rel}"));
    let tail = &src[start_at..];
    let end_at = tail
        .find(end)
        .unwrap_or_else(|| panic!("`{end}` missing after `{start}` in {rel}"));
    tail[..end_at]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn application_present_gates_all_three_transients_on_overlay_open() {
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
            "application-present path lost its overlay_open gate: `{needle}` not found in \
             app_render.rs — capture paths use overlay_open(), so app-present/capture policy \
             parity requires it here too"
        );
    }
    // The pre-audit policy must never come back.
    assert!(
        !src.contains("let settings_open = ws0.settings().is_some();"),
        "application-present path regressed to the Settings-only suppression gate"
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

/// API-closure guard for the two deliberately different pixel verbs.
///
/// `image` is the renderer-owned framebuffer path, including compiled native
/// surfaces, and is valid without an attached OS window or screen-capture grant.
/// `window` assembles platform-owned chrome around an exact successful
/// application-present client destination.
/// Both eventually encode PNGs, so checking only the encoder or reply shape is
/// vacuous; this follows each command through its distinct main-loop arm and then
/// checks the renderer leaf cannot acquire a platform-capture dependency.
#[test]
fn image_and_window_remain_disjoint_capture_apis() {
    let image_cmd = normalized_section(
        "src/control_media.rs",
        "pub(crate) fn cmd_image(",
        "fn image_metadata_fields(",
    );
    assert!(
        image_cmd.contains("push_back(ImageReq {"),
        "`image` must enqueue a renderer request"
    );
    assert!(
        image_cmd.contains("proxy.send_event(Wake::Control)"),
        "`image` must wake the renderer queue"
    );

    let image_dispatch = normalized_section(
        "src/lib.rs",
        "Wake::Control => {",
        "Wake::ReadChrome { reply } => {",
    );
    assert!(
        image_dispatch.contains("self.render_image(req);"),
        "Wake::Control must close over ImageReq through App::render_image"
    );

    let renderer_image = normalized_section(
        "src/app_introspect.rs",
        "pub(crate) fn render_image(&mut self, req: ImageReq)",
        "pub(crate) fn read_native_chrome",
    );
    let renderer_native = normalized_section(
        "src/app_introspect.rs",
        "fn render_native_image(",
        "fn native_image_metadata(",
    );
    assert!(
        renderer_image.contains("self.render_native_image("),
        "native and heterogeneous `image` routes must close over the compiled native renderer"
    );
    assert!(
        renderer_image.contains("backend.render_input_for_destination(")
            && renderer_native.contains("backend.render_input_for_destination("),
        "terminal and native `image` routes must both end in renderer framebuffers"
    );

    let forbidden_image_capture = [
        "Wake::CaptureWindow",
        "Wake::CaptureAuxWindow",
        "capture_window_pixels",
        "capture_window_rgba",
        "current_window_rgba_of",
        "window_rgba_of",
        "CGWindowListCreateImage",
        "PrintWindow",
    ];
    for (stage, body) in [
        ("control command", &image_cmd),
        ("main-loop dispatch", &image_dispatch),
        ("terminal/composite renderer", &renderer_image),
        ("native renderer", &renderer_native),
    ] {
        for forbidden in forbidden_image_capture {
            assert!(
                !body.contains(forbidden),
                "`image` {stage} acquired OS-window capture dependency `{forbidden}`"
            );
        }
    }

    // Why: the end marker only bounds `cmd_window`'s body. `newest_recording_with_index`
    // was the next fn when this gate was written; main has since replaced it, so the
    // bound follows to whatever now succeeds `cmd_window` — the assertions are unchanged.
    let window_cmd = normalized_section(
        "src/control_media.rs",
        "pub(crate) fn cmd_window(",
        "fn valid_recording_name(",
    );
    assert!(
        window_cmd.contains("Wake::CaptureWindow") && window_cmd.contains("Wake::CaptureAuxWindow"),
        "`window` must retain its explicit platform-window wake route"
    );
    // Why: markers match RAW source, so the brace-only prefix survives the rustfmt
    // split that adding the `cancel` (one-shot cancellation election) field forced.
    let window_dispatch = normalized_section(
        "src/lib.rs",
        "Wake::CaptureWindow {",
        "Wake::CaptureAuxWindow {",
    );
    assert!(
        window_dispatch.contains("self.capture_window(path, cancel, reply);"),
        "Wake::CaptureWindow must close over App::capture_window"
    );

    let mac_window_route = normalized_section(
        "src/app_introspect.rs",
        "fn capture_window_of(",
        "fn present_before_window_capture(",
    );
    assert!(
        mac_window_route.contains("self.current_window_rgba_of(")
            && mac_window_route.contains("self.window_rgba_of(None)"),
        "macOS `window` must close over its platform photograph route"
    );
    let mac_platform_leaf = normalized_section(
        "src/app_introspect.rs",
        "fn window_rgba_of(",
        "fn window_client_rect_of(",
    );
    assert!(
        mac_platform_leaf.contains("capture_window_pixels(window_number as u32)"),
        "macOS `window` must end in CoreGraphics capture"
    );
    let windows_window_route = normalized_section(
        "src/app_introspect.rs",
        "#[cfg(windows)]\n    pub(crate) fn capture_window(",
        "#[cfg(all(not(target_os = \"macos\"), not(windows)))]",
    );
    assert!(
        windows_window_route.contains("crate::platform_win::capture_window_rgba(window)"),
        "Windows `window` must close over its platform photograph route"
    );
    let windows_capture = normalized("src/platform_win.rs");
    assert!(
        windows_capture.contains("PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT)"),
        "Windows `window` lost its OS-chrome PrintWindow capture"
    );
    for fail_closed_gate in [
        "let print_succeeded = PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT) != 0;",
        "validate_window_capture_transfer(print_succeeded, lines, h)?;",
        "if copied_lines != expected_lines",
    ] {
        assert!(
            windows_capture.contains(fail_closed_gate),
            "Windows `window` lost its exact-transfer gate: `{fail_closed_gate}`"
        );
    }
    let restore = windows_capture
        .find("if SelectObject(mem, old) == 0")
        .expect("Windows capture must deselect its bitmap before readback");
    let readback = windows_capture
        .find("let lines = if print_succeeded { GetDIBits(")
        .expect("Windows capture must read the successfully printed bitmap");
    assert!(
        restore < readback,
        "GetDIBits requires the capture bitmap to be deselected from its DC first"
    );

    let ctl_docs = normalized("../aterm-ctl/src/lib.rs");
    for documented_boundary in [
        "current application-render artifact",
        "compiled native-app surfaces",
        "has no native OS chrome",
        "works headless",
        "screen-capture",
        "full-window artifact",
        "platform chrome",
        "exact client destination from a successful",
        "application present",
        "does not observe compositor selection",
        "Screen Recording permission",
    ] {
        assert!(
            ctl_docs.contains(documented_boundary),
            "aterm-ctl docs lost the image/window boundary: `{documented_boundary}`"
        );
    }
}
