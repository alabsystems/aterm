// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W9 phase 2 — THE SEAM'S CONSTANT RECIPE, EXECUTED.
//!
//! # The gap this closes
//!
//! `vendor/winit/src/platform_impl/macos/aterm_objc_seam.rs`'s `consts` module
//! carries 42 AppKit and Foundation values, and its header says exactly the
//! right thing about how they are checked:
//!
//! ```text
//! Every value below is `_Static_assert`ed against the SDK on BOTH arches
//!   $ cc -arch arm64  -fsyntax-only /tmp/c.m   # passes
//!   $ cc -arch x86_64 -fsyntax-only /tmp/c.m   # passes
//! ```
//!
//! **It was a comment.** Nothing in the tree ran it, nothing re-ran it when a
//! row was added, and nothing would have noticed if a future edit made it
//! false. `aterm-gui`'s `consts` module has a real test
//! (`appkit::consts_tests`) that diffs every row against the `objc2` expression
//! it replaced — but that oracle is only available while `objc2-app-kit` is a
//! dependency of `aterm-gui`, and it says so itself. The vendored fork has no
//! equivalent: `winit` is a path dependency and NOT a workspace member, so
//! `cargo test --workspace` never compiles a test inside it.
//!
//! This file is the recipe, run. It lives here because `aterm-objc` IS a
//! workspace member and because these constants exist to feed this crate's
//! sends.
//!
//! # Why the campaign cares about exactly this shape
//!
//! `NS_TEXT_ALIGNMENT_CENTER` shipped as `2` — which is RIGHT alignment —
//! because its `#if TARGET_ABI_USES_IOS_VALUES` guard was read as "iOS vs
//! macOS" when on Apple Silicon it reduces to `!TARGET_CPU_X86_64`. It
//! compiled, every test passed, every encoding read back correct, and only an
//! A/B pixel capture caught it. The defence adopted afterwards was the recipe
//! above. A defence that has never been executed is a claim.
//!
//! # Non-vacuity, three ways
//!
//! * The table must cover the seam's `consts` module EXACTLY — every constant
//!   parsed out of the file must have a row here, and every row must name a
//!   constant that is really there. A value added to the seam without a row
//!   fails this test rather than slipping past it.
//! * The assertion uses the RUST literal verbatim, so the thing checked is what
//!   `view.rs` and `window_delegate.rs` actually compile against, not a second
//!   transcription of the SDK.
//! * A deliberately inverted copy must FAIL TO COMPILE on both arches. If the
//!   probe stopped seeing AppKit, or `_Static_assert` stopped meaning anything,
//!   the green arm would go green for the wrong reason and this arm catches it.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

use crate::sdk_consts;
use crate::sdk_consts::{as_c_literal, green_verdict, inverted_verdict, parse_const_decl};

/// `(Rust constant, the SDK spelling it claims to equal)`.
///
/// The SDK name is written out rather than derived from the Rust name by
/// un-snake-casing it. A derivation would be a fourth guess — and it would be
/// wrong for `NS_APPKIT_VERSION_NUMBER_10_12`, `NS_CRITICAL_REQUEST` and
/// `NS_WINDOW_ABOVE`, none of which is the mechanical camel-case of its Rust
/// spelling.
//
// `rustfmt::skip`, as `winit_seam.rs`'s row table does and for the same reason:
// one row per line is what makes a lookup table checkable by eye against the
// seam it mirrors.
#[rustfmt::skip]
const ROWS: &[(&str, &str)] = &[
    // ---- NSWindowStyleMask ----
    ("NS_WINDOW_STYLE_MASK_BORDERLESS", "NSWindowStyleMaskBorderless"),
    ("NS_WINDOW_STYLE_MASK_TITLED", "NSWindowStyleMaskTitled"),
    ("NS_WINDOW_STYLE_MASK_CLOSABLE", "NSWindowStyleMaskClosable"),
    ("NS_WINDOW_STYLE_MASK_MINIATURIZABLE", "NSWindowStyleMaskMiniaturizable"),
    ("NS_WINDOW_STYLE_MASK_RESIZABLE", "NSWindowStyleMaskResizable"),
    ("NS_WINDOW_STYLE_MASK_FULL_SIZE_CONTENT_VIEW", "NSWindowStyleMaskFullSizeContentView"),
    // ---- NSApplicationPresentationOptions ----
    ("NS_APPLICATION_PRESENTATION_AUTO_HIDE_DOCK", "NSApplicationPresentationAutoHideDock"),
    ("NS_APPLICATION_PRESENTATION_HIDE_DOCK", "NSApplicationPresentationHideDock"),
    ("NS_APPLICATION_PRESENTATION_AUTO_HIDE_MENU_BAR", "NSApplicationPresentationAutoHideMenuBar"),
    ("NS_APPLICATION_PRESENTATION_HIDE_MENU_BAR", "NSApplicationPresentationHideMenuBar"),
    ("NS_APPLICATION_PRESENTATION_FULL_SCREEN", "NSApplicationPresentationFullScreen"),
    // ---- NSWindowButton ----
    ("NS_WINDOW_CLOSE_BUTTON", "NSWindowCloseButton"),
    ("NS_WINDOW_MINIATURIZE_BUTTON", "NSWindowMiniaturizeButton"),
    ("NS_WINDOW_ZOOM_BUTTON", "NSWindowZoomButton"),
    ("NS_WINDOW_FULL_SCREEN_BUTTON", "NSWindowFullScreenButton"),
    // ---- NSWindowSharingType ----
    ("NS_WINDOW_SHARING_NONE", "NSWindowSharingNone"),
    ("NS_WINDOW_SHARING_READ_ONLY", "NSWindowSharingReadOnly"),
    // ---- the singles ----
    ("NS_WINDOW_TITLE_HIDDEN", "NSWindowTitleHidden"),
    ("NS_WINDOW_TABBING_MODE_PREFERRED", "NSWindowTabbingModePreferred"),
    ("NS_WINDOW_ABOVE", "NSWindowAbove"),
    ("NS_WINDOW_OCCLUSION_STATE_VISIBLE", "NSWindowOcclusionStateVisible"),
    // ---- NSRequestUserAttentionType ----
    ("NS_CRITICAL_REQUEST", "NSCriticalRequest"),
    ("NS_INFORMATIONAL_REQUEST", "NSInformationalRequest"),
    // ---- the rest of W8's ----
    ("NS_BACKING_STORE_BUFFERED", "NSBackingStoreBuffered"),
    ("NS_DRAG_OPERATION_NONE", "NSDragOperationNone"),
    ("NS_DRAG_OPERATION_COPY", "NSDragOperationCopy"),
    ("NS_KEY_VALUE_OBSERVING_OPTION_NEW", "NSKeyValueObservingOptionNew"),
    ("NS_KEY_VALUE_OBSERVING_OPTION_OLD", "NSKeyValueObservingOptionOld"),
    ("NS_APPKIT_VERSION_NUMBER_10_12", "NSAppKitVersionNumber10_12"),
    // ---- W10, event.rs ----
    ("NS_EVENT_MODIFIER_FLAG_SHIFT", "NSEventModifierFlagShift"),
    ("NS_EVENT_MODIFIER_FLAG_CONTROL", "NSEventModifierFlagControl"),
    ("NS_EVENT_MODIFIER_FLAG_OPTION", "NSEventModifierFlagOption"),
    ("NS_EVENT_MODIFIER_FLAG_COMMAND", "NSEventModifierFlagCommand"),
    ("NS_EVENT_TYPE_APPLICATION_DEFINED", "NSEventTypeApplicationDefined"),
    ("NS_EVENT_SUBTYPE_WINDOW_EXPOSED", "NSEventSubtypeWindowExposed"),
    // ---- W9 phase 2, view.rs ----
    ("NS_EVENT_PHASE_BEGAN", "NSEventPhaseBegan"),
    ("NS_EVENT_PHASE_CHANGED", "NSEventPhaseChanged"),
    ("NS_EVENT_PHASE_ENDED", "NSEventPhaseEnded"),
    ("NS_EVENT_PHASE_CANCELLED", "NSEventPhaseCancelled"),
    ("NS_EVENT_PHASE_MAY_BEGIN", "NSEventPhaseMayBegin"),
    ("NS_NOT_FOUND", "NSNotFound"),
    ("NS_UTF8_STRING_ENCODING", "NSUTF8StringEncoding"),
    // ---- W12, app_state.rs's activation policy ----
    ("NS_APPLICATION_ACTIVATION_POLICY_REGULAR", "NSApplicationActivationPolicyRegular"),
    ("NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY", "NSApplicationActivationPolicyAccessory"),
    ("NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED", "NSApplicationActivationPolicyProhibited"),
    // ---- W12, app.rs's swizzled -sendEvent: ----
    //
    // The eleven `NSEventType` enumerators the fork matches on. `objc2-app-kit`
    // generated them as a Rust `enum`; the numbering is NOT dense across the
    // mouse types (1..7 then 25..27), which is exactly the kind of gap a
    // transcription gets wrong and a `_Static_assert` does not.
    ("NS_EVENT_TYPE_LEFT_MOUSE_DOWN", "NSEventTypeLeftMouseDown"),
    ("NS_EVENT_TYPE_LEFT_MOUSE_UP", "NSEventTypeLeftMouseUp"),
    ("NS_EVENT_TYPE_RIGHT_MOUSE_DOWN", "NSEventTypeRightMouseDown"),
    ("NS_EVENT_TYPE_RIGHT_MOUSE_UP", "NSEventTypeRightMouseUp"),
    ("NS_EVENT_TYPE_MOUSE_MOVED", "NSEventTypeMouseMoved"),
    ("NS_EVENT_TYPE_LEFT_MOUSE_DRAGGED", "NSEventTypeLeftMouseDragged"),
    ("NS_EVENT_TYPE_RIGHT_MOUSE_DRAGGED", "NSEventTypeRightMouseDragged"),
    ("NS_EVENT_TYPE_KEY_UP", "NSEventTypeKeyUp"),
    ("NS_EVENT_TYPE_OTHER_MOUSE_DOWN", "NSEventTypeOtherMouseDown"),
    ("NS_EVENT_TYPE_OTHER_MOUSE_UP", "NSEventTypeOtherMouseUp"),
    ("NS_EVENT_TYPE_OTHER_MOUSE_DRAGGED", "NSEventTypeOtherMouseDragged"),
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn seam() -> PathBuf {
    repo().join("vendor/winit/src/platform_impl/macos/aterm_objc_seam.rs")
}

/// `(name, literal)` for every `const` in the seam, at any visibility.
///
/// The visibility grammar is `sdk_consts::strip_const_decl`'s, shared with
/// `gui_appkit_constants.rs` — it is this file's fourteenth-pass finding, and
/// `sdk_consts::the_parser_reads_every_visibility_spelling` pins it.
fn seam_constants() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(seam()).expect("the seam is readable");
    src.lines().filter_map(parse_const_decl).collect()
}

/// [`sdk_consts::compile`] in this census's own scratch directory.
fn compile(arch: &str, body: &str, tag: &str) -> Result<(), String> {
    sdk_consts::compile("aterm-seam-consts", arch, body, tag)
}

/// The probe source: one `_Static_assert` per row, using the seam's own
/// literal. `invert` flips ONE row, for the non-vacuity arm.
///
/// # The literal is PARENTHESISED, and the day that starts mattering is the day
/// it would otherwise go silently vacuous
///
/// `|` binds LOOSER than `==` in C, so `_Static_assert(SDK == a | b, "")` parses
/// as `_Static_assert((SDK == a) | b, "")` — non-zero whatever `SDK` is, i.e. an
/// assertion that passes for every value including the wrong one. Clang warns
/// (`-Wparentheses`) and exits 0.
///
/// **No row in this seam is compound today**, so this probe is not vacuous and
/// never has been. It would become so the moment someone writes a mask as
/// `A | B` — which is exactly how `aterm-gui`'s twin of this file spells two of
/// its rows, where the shape was found and MEASURED
/// (`gui_appkit_constants::an_unparenthesised_compound_literal_would_be_vacuous`
/// builds a wrong value both ways and proves the bare form is accepted). The
/// parentheses and the `-Werror -Wparentheses` in [`compile`] are the fix
/// applied here BEFORE the row that needs it exists, because a guard that only
/// starts working after the defect lands is not a guard.
fn probe(invert: Option<&str>) -> String {
    let values: std::collections::HashMap<String, String> = seam_constants().into_iter().collect();
    let mut s = String::from("#import <Cocoa/Cocoa.h>\n");
    for (rust, sdk) in ROWS {
        let lit = as_c_literal(&values[*rust]);
        let lit = if invert == Some(rust) {
            // `+ 1` is enough and is arithmetic the compiler must evaluate; it
            // works for the `double` row as well as every integer one.
            format!("(({lit}) + 1)")
        } else {
            format!("({lit})")
        };
        s.push_str(&format!("_Static_assert({sdk} == {lit}, \"{rust}\");\n"));
    }
    s.push_str("int main(void){return 0;}\n");
    s
}

/// THE TABLE AND THE SEAM AGREE, in both directions.
#[test]
fn every_seam_constant_has_a_row_and_every_row_names_a_seam_constant() {
    let seam: Vec<String> = seam_constants().into_iter().map(|(n, _)| n).collect();
    let rows: Vec<String> = ROWS.iter().map(|(n, _)| (*n).to_owned()).collect();

    let missing: Vec<&String> = seam.iter().filter(|n| !rows.contains(n)).collect();
    assert!(
        missing.is_empty(),
        "the seam gained {} constant(s) with no row here, so nothing checks \
         them against the SDK:\n  {:?}",
        missing.len(),
        missing
    );
    let stale: Vec<&String> = rows.iter().filter(|n| !seam.contains(n)).collect();
    assert!(
        stale.is_empty(),
        "these rows name constants the seam no longer has:\n  {stale:?}"
    );
    assert_eq!(seam.len(), ROWS.len());
    assert!(
        seam.len() >= 42,
        "the seam had 42 constants when this was written"
    );
}

/// THE RECIPE, RUN — both arches, and this box cannot execute one of them.
///
/// `-fsyntax-only` is why the x86_64 arm is possible at all: every row is a
/// compile-time value, so COMPILING the assertion for an arch is the whole
/// measurement and no binary for that arch has to run.
#[test]
fn every_seam_constant_equals_the_sdk_on_both_arches() {
    let body = probe(None);
    for arch in ["arm64", "x86_64"] {
        green_verdict(
            arch,
            "a seam constant disagrees with the SDK",
            compile(arch, &body, "green"),
        );
    }
}

/// …and the same probe with ONE row inverted must fail on BOTH arches.
#[test]
fn the_assertion_is_load_bearing_on_both_arches() {
    // The row picked is deliberately one whose VALUE could never expose a
    // mistake at runtime: `NSEventSubtypeWindowExposed` is 0, so the fork
    // passes the same bits whether the constant is right or wrong. It is the
    // profile of every constant this instrument exists for.
    let body = probe(Some("NS_EVENT_SUBTYPE_WINDOW_EXPOSED"));
    for arch in ["arm64", "x86_64"] {
        inverted_verdict(
            arch,
            "NS_EVENT_SUBTYPE_WINDOW_EXPOSED",
            compile(arch, &body, "inverted"),
            &format!(
                "an INVERTED constant compiled cleanly on {arch}: this test is passing for \
                 the wrong reason and proves nothing about the other one"
            ),
        );
    }
}
