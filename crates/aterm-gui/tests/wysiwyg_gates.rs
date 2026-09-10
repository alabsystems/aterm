// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Source-scan gate: the THREE app-owned render paths (application-present
//! composition, SIGUSR1 `snapshot`, control-socket `image`) suppress the
//! transient effects — visual-bell invert, drag-drop wash, level-up glow —
//! through the same overlay policy. Bell/drag and the landing celebration yield
//! to `WindowState::overlay_open()`; the functional charging rim stays visible.
//!
//! # Why a source-scanning test
//!
//! The invariant tested here is narrower: application-present composition,
//! `snapshot`, and `image` use the same phase-aware overlay policy for
//! these three transient effects. The 2026-07 audit found mutually inconsistent
//! policies across those paths. The compiler cannot see that three distant code
//! sites implement one policy, so this structural test reads the committed
//! sources and checks both the policy and capture's route to that policy.
//! It does not inspect WSI, compositor selection, or scanout.
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
fn application_present_uses_the_phase_aware_overlay_policy() {
    let src = normalized_section(
        "src/app_render.rs",
        "fn redraw_window_with_layout(",
        "pub(crate) fn redraw_tab_strip_state(",
    );
    for needle in [
        // bell invert + drag wash read the shared gate once...
        "let overlay_open = ws0.overlay_open();",
        "let invert = ws0.bell_flash.is_active(Instant::now()) && !overlay_open;",
        "let drag_hover = ws0.drag_hover && !overlay_open;",
        // ...and the surge consults the SAME local, with only its functional
        // charging phase admitted over a modal (LevelUp's policy below).
        "self.level_up .as_ref() .filter(|l| !overlay_open || l.paints_over_overlay())",
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
fn shared_visual_policy_keeps_only_the_charging_exception() {
    let src = normalized_section(
        "src/app_render.rs",
        "pub(crate) fn host_visual_state(",
        "pub(crate) fn finalize_successful_terminal_present_for_test(",
    );
    for needle in [
        "let overlay_open = window.overlay_open();",
        "let invert = window.bell_flash.is_active(now) && !overlay_open;",
        "if window.drag_hover && !overlay_open {",
        "self.level_up .as_ref() .filter(|l| !overlay_open || l.paints_over_overlay())",
    ] {
        assert!(src.contains(needle), "shared visual policy lost `{needle}`");
    }
    let phase = normalized_section(
        "src/level_up.rs",
        "pub(crate) const fn paints_over_overlay(",
        "pub(crate) const fn phase(",
    );
    assert!(
        phase.contains("matches!(self.phase, Phase::Charging)"),
        "the overlay exception belongs only to Charging, never the Landing celebration"
    );
}

/// Structural closure of the fallback capture policy, including its consumers.
/// Compact only for matching punctuation across optional rustfmt line breaks.
fn capture_uses_shared_visual_policy(src: &str, authority: &str) -> bool {
    let src = src.split_whitespace().collect::<String>();
    let mapping = ".map(|presented|crate::app_render::HostVisualState{\
        invert:presented.invert,overlay:presented.overlay,})";
    let fallback = ".unwrap_or_else(||self.host_visual_state(front,Instant::now()))";
    let retained_first = match authority {
        "presented_visuals" => {
            src.contains(&format!(
                "letpresented_visuals=presented.as_ref(){mapping};"
            )) && src.contains(&format!("letvisuals=presented_visuals{fallback};"))
        }
        "presented_authority" => src.contains(&format!(
            "letvisuals=presented_authority{mapping}{fallback};"
        )),
        _ => false,
    };
    retained_first
        && src.contains("apply_bell_invert(&mutframe,visuals.invert);")
        && src.contains("ifletSome(overlay)=visuals.overlay{")
        && ![
            "letlevel_up_glow=",
            "letlevel_up_style=",
            "ws.overlay_open()",
            "ws.bell_flash",
            "ws.drag_hover",
        ]
        .iter()
        .any(|needle| src.contains(*needle))
}

#[test]
fn snapshot_and_image_resolve_retained_or_shared_visuals_without_a_second_policy() {
    for (path, start, end, authority) in [
        (
            "snapshot",
            "pub(crate) fn snapshot(&mut self)",
            "pub(crate) fn submit_encode_job(",
            "presented_visuals",
        ),
        (
            "image",
            "pub(crate) fn render_image(&mut self, req: ImageReq)",
            "pub(crate) fn read_native_chrome",
            "presented_authority",
        ),
    ] {
        let src = normalized_section("src/app_introspect.rs", start, end);
        assert!(
            capture_uses_shared_visual_policy(&src, authority),
            "{path} must retain presented visuals when available, otherwise resolve \
             host_visual_state once and consume its invert/overlay without a local policy"
        );

        // These mutations replay the historical capture-policy divergence and
        // the opposite failure (capturing a default instead of the live rim).
        // Each path is checked independently, so the other path cannot mask it.
        let compact = src.split_whitespace().collect::<String>();
        for (mutation, replacement) in [
            (
                "self.host_visual_state(front,Instant::now())",
                "HostVisualState::default()",
            ),
            (
                "ifletSome(overlay)=visuals.overlay{",
                "if!ws.overlay_open()&&letSome(overlay)=visuals.overlay{",
            ),
            (
                "apply_bell_invert(&mutframe,visuals.invert);",
                "apply_bell_invert(&mutframe,ws.bell_flash.is_active(Instant::now()));",
            ),
        ] {
            assert!(
                !capture_uses_shared_visual_policy(
                    &compact.replace(mutation, replacement),
                    authority,
                ),
                "{path} gate accepted policy-divergence mutation `{replacement}`"
            );
        }
        assert!(
            !capture_uses_shared_visual_policy(
                &compact.replace(&format!("letvisuals={authority}"), "letvisuals=None"),
                authority,
            ),
            "{path} gate accepted dropping retained visual authority"
        );
    }
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
        window_dispatch.contains("self.capture_window(handoff, path, cancel, reply);"),
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
