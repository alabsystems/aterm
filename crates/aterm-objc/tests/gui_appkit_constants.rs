// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-gui`'s AppKit constants, `_Static_assert`ed against the SDK on BOTH
//! arches.
//!
//! * **The authority is Apple's header**, not a third-party transcription.
//! * **`x86_64` is covered.** Every row is a compile-time value, so compiling
//!   the assertion for an arch is the measurement; no binary for that arch has
//!   to run (this box cannot execute the compat slice). `NS_TEXT_ALIGNMENT_CENTER`'s
//!   two `#[cfg]` arms are BOTH checked, each under the matching `#if`, and the
//!   arm the gate selects is read out of the SOURCE.
//! * **Coverage comes from the SOURCE.** The probe walks every constant the
//!   parser finds in `consts`, and one with no SDK row fails the probe by name.
//!
//! Why it matters: `NS_TEXT_ALIGNMENT_CENTER` once shipped as RIGHT alignment,
//! caught only by a pixel capture, and `NS_COLOR_RENDERING_INTENT_PERCEPTUAL`
//! was first written `1` (it is `3`) — wrong values no run-time test sees.
//!
//! [`winit_seam_constants.rs`](../winit_seam_constants.rs) is the twin over
//! `crates/aterm-window`'s seam: same parser grammar, same probe. Two
//! differences:
//!
//! 1. **The literal is PARENTHESISED in the assertion.** `|` binds LOOSER than
//!    `==` in C, so `_Static_assert(SDK == 0x01 | 0x80, "")` is
//!    `(SDK == 0x01) | 0x80` and a wrong value compiles (clang only warns
//!    `-Wparentheses`). `NS_TRACKING_HOVER_IN_VISIBLE_RECT` and
//!    `NS_VIEW_MIN_X_MARGIN_MAX_Y_MARGIN` are `|`-composed. The seam's probe is
//!    parenthesised too, so it stays non-vacuous if one is added there.
//! 2. **`#[cfg(target_arch)]` is part of the grammar.** Its gate is read from
//!    the attribute above the declaration and turned into the matching `#if`,
//!    so swapping the two arms fails here.
//!
//! It lives in `aterm-objc` beside its twin: it reads its subject as TEXT and
//! shells out to `clang`, so it need not link the `aterm-gui` library.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

use crate::sdk_consts;
use crate::sdk_consts::{as_c_literal, green_verdict, inverted_verdict, parse_const_decl};

/// `(Rust constant, the SDK spelling it claims to equal)`.
///
/// The SDK name is written out rather than derived from the Rust name by
/// un-snake-casing it, for the reason the seam's twin gives: a derivation would
/// be a second guess, and it would be wrong for `NS_MODAL_RESPONSE_OK`,
/// `NS_NO_IMAGE`, `NS_WINDOW_ABOVE`, `NS_TRACKING_HOVER_IN_VISIBLE_RECT` and
/// `NS_VIEW_MIN_X_MARGIN_MAX_Y_MARGIN`, none of which is the mechanical
/// camel-case of its Rust spelling.
///
/// Five rows are EXPRESSIONS rather than single enumerators, because the Rust
/// constant is one: two `|`-composed masks, and three whose Rust literal is a
/// shift. Those are what make the parenthesisation in [`probe`] load-bearing.
//
// `rustfmt::skip`, as the seam's row table does and for the same reason: one
// row per line is what makes a lookup table checkable by eye against the
// module it mirrors.
#[rustfmt::skip]
const ROWS: &[(&str, &str)] = &[
    // ---- status item, modifiers, modal ----
    ("NS_VARIABLE_STATUS_ITEM_LENGTH",        "NSVariableStatusItemLength"),
    ("NS_EVENT_MODIFIER_FLAG_SHIFT",          "NSEventModifierFlagShift"),
    ("NS_EVENT_MODIFIER_FLAG_CONTROL",        "NSEventModifierFlagControl"),
    ("NS_EVENT_MODIFIER_FLAG_COMMAND",        "NSEventModifierFlagCommand"),
    ("NS_MODAL_RESPONSE_OK",                  "NSModalResponseOK"),
    // ---- the tab strip ----
    ("NS_LINE_CAP_STYLE_ROUND",               "NSLineCapStyleRound"),
    ("NS_TEXT_ALIGNMENT_LEFT",                "NSTextAlignmentLeft"),
    ("NS_TEXT_ALIGNMENT_CENTER",              "NSTextAlignmentCenter"),
    ("NS_NO_IMAGE",                           "NSNoImage"),
    (
        "NS_TRACKING_HOVER_IN_VISIBLE_RECT",
        "(NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways | NSTrackingInVisibleRect)",
    ),
    (
        "NS_VIEW_MIN_X_MARGIN_MAX_Y_MARGIN",
        "(NSViewMinXMargin | NSViewMaxYMargin)",
    ),
    ("NS_VIEW_WIDTH_SIZABLE",                 "NSViewWidthSizable"),
    ("NS_WINDOW_CLOSE_BUTTON",                "NSWindowCloseButton"),
    ("NS_WINDOW_MINIATURIZE_BUTTON",          "NSWindowMiniaturizeButton"),
    ("NS_WINDOW_ZOOM_BUTTON",                 "NSWindowZoomButton"),
    ("NS_WINDOW_ABOVE",                       "NSWindowAbove"),
    ("NS_TOOLBAR_DISPLAY_MODE_ICON_ONLY",     "NSToolbarDisplayModeIconOnly"),
    (
        "NS_WINDOW_TOOLBAR_STYLE_UNIFIED_COMPACT",
        "NSWindowToolbarStyleUnifiedCompact",
    ),
    ("NS_WINDOW_TITLE_HIDDEN",                "NSWindowTitleHidden"),
    ("NS_LINE_BREAK_BY_TRUNCATING_TAIL",      "NSLineBreakByTruncatingTail"),
    // ---- the modal-alert subsystem ----
    ("NS_EVENT_MASK_KEY_DOWN",                "NSEventMaskKeyDown"),
    ("NS_ALERT_FIRST_BUTTON_RETURN",          "NSAlertFirstButtonReturn"),
    ("NS_ALERT_SECOND_BUTTON_RETURN",         "NSAlertSecondButtonReturn"),
    // ---- the `chrome` introspection reader ----
    ("NS_WINDOW_TOOLBAR_STYLE_AUTOMATIC",     "NSWindowToolbarStyleAutomatic"),
    ("NS_WINDOW_TOOLBAR_STYLE_EXPANDED",      "NSWindowToolbarStyleExpanded"),
    ("NS_WINDOW_TOOLBAR_STYLE_PREFERENCE",    "NSWindowToolbarStylePreference"),
    ("NS_WINDOW_TOOLBAR_STYLE_UNIFIED",       "NSWindowToolbarStyleUnified"),
    ("NS_TOOLBAR_DISPLAY_MODE_DEFAULT",       "NSToolbarDisplayModeDefault"),
    ("NS_TOOLBAR_DISPLAY_MODE_ICON_AND_LABEL","NSToolbarDisplayModeIconAndLabel"),
    ("NS_TOOLBAR_DISPLAY_MODE_LABEL_ONLY",    "NSToolbarDisplayModeLabelOnly"),
    // ---- the titlebar snapshot ----
    ("NS_BITMAP_IMAGE_FILE_TYPE_PNG",         "NSBitmapImageFileTypePNG"),
    ("NS_COLOR_RENDERING_INTENT_PERCEPTUAL",  "NSColorRenderingIntentPerceptual"),
    // ---- the App Nap opt-out (Foundation's, reached through Cocoa.h) ----
    (
        "NS_ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP",
        "NSActivityUserInitiatedAllowingIdleSystemSleep",
    ),
    // ---- the Developer Tools helper panel ----
    (
        "NS_WINDOW_STYLE_MASK_UTILITY_PANEL",
        "(NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskUtilityWindow | NSWindowStyleMaskNonactivatingPanel)",
    ),
    ("NS_BACKING_STORE_BUFFERED",             "NSBackingStoreBuffered"),
    ("NS_FLOATING_WINDOW_LEVEL",              "NSFloatingWindowLevel"),
    (
        "NS_WINDOW_COLLECTION_BEHAVIOR_ALL_SPACES_AUXILIARY",
        "(NSWindowCollectionBehaviorCanJoinAllSpaces | NSWindowCollectionBehaviorFullScreenAuxiliary)",
    ),
    ("NS_DRAG_OPERATION_NONE",                "NSDragOperationNone"),
    ("NS_DRAG_OPERATION_COPY",                "NSDragOperationCopy"),
    ("NS_BEZEL_STYLE_PUSH",                   "NSBezelStylePush"),
    (
        "NS_APPLICATION_ACTIVATE_ALL_WINDOWS_IGNORING_OTHER_APPS",
        "(NSApplicationActivateAllWindows | NSApplicationActivateIgnoringOtherApps)",
    ),
    (
        "NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED",
        "NSApplicationActivationPolicyProhibited",
    ),
    (
        "CG_WINDOW_LIST_ON_SCREEN_NON_DESKTOP",
        "(kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements)",
    ),
    ("CG_NULL_WINDOW_ID",                     "kCGNullWindowID"),
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn appkit_rs() -> PathBuf {
    repo().join("crates/aterm-gui/src/appkit.rs")
}

/// Which arch a constant's `#[cfg(target_arch = …)]` selects it on, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// No `#[cfg(target_arch)]`: the declaration is compiled on every arch.
    Always,
    /// `#[cfg(target_arch = "x86_64")]`.
    X86_64,
    /// `#[cfg(not(target_arch = "x86_64"))]`.
    NotX86_64,
}

impl Gate {
    /// The C preprocessor condition that selects the same arch, or `None` for
    /// [`Gate::Always`].
    fn c_if(self) -> Option<&'static str> {
        match self {
            Gate::Always => None,
            Gate::X86_64 => Some("TARGET_CPU_X86_64"),
            Gate::NotX86_64 => Some("!TARGET_CPU_X86_64"),
        }
    }
}

/// The `#[cfg]` an attribute line states, if it is an arch gate.
///
/// Anything else — `#[cfg(test)]`, a doc attribute, a lint allow — answers
/// `None`, which is NOT the same as [`Gate::Always`]: `None` means "this line is
/// not a gate", so the caller keeps looking rather than resetting.
fn arch_gate(line: &str) -> Option<Gate> {
    match line.trim() {
        r#"#[cfg(target_arch = "x86_64")]"# => Some(Gate::X86_64),
        r#"#[cfg(not(target_arch = "x86_64"))]"# => Some(Gate::NotX86_64),
        _ => None,
    }
}

/// `(name, gate, literal)` for every `const` inside `appkit.rs`'s `consts`
/// module, at any visibility.
///
/// SCOPED TO THE MODULE BY BRACE DEPTH, not by "every const in the file". The
/// file has other constants elsewhere and will grow more; a walk that took them
/// all would demand SDK rows for things that are not AppKit values at all.
fn gui_constants() -> Vec<(String, Gate, String)> {
    let src = std::fs::read_to_string(appkit_rs()).expect("appkit.rs is readable");
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut inside = false;
    let mut gate = Gate::Always;
    for line in src.lines() {
        let trimmed = line.trim();
        if !inside && trimmed.ends_with("mod consts {") {
            inside = true;
            depth = 0;
        }
        if inside {
            // Count braces BEFORE parsing, so the module's own opening brace
            // takes the depth to 1 and its closing brace back to 0.
            let opens = trimmed.matches('{').count() as i32;
            let closes = trimmed.matches('}').count() as i32;
            if let Some(g) = arch_gate(line) {
                gate = g;
            } else if let Some((name, value)) = parse_const_decl(line) {
                out.push((name, gate, value));
                gate = Gate::Always;
            } else if !trimmed.is_empty() && !trimmed.starts_with("//") {
                // A non-comment, non-const line ends any pending gate: an
                // attribute applies to the item that FOLLOWS it, and if that
                // item is not a const then the gate was not a const's.
                gate = Gate::Always;
            }
            depth += opens - closes;
            if depth <= 0 && !trimmed.ends_with("mod consts {") {
                break;
            }
        }
    }
    out
}

/// [`sdk_consts::compile`] in this census's own scratch directory.
fn compile(arch: &str, body: &str, tag: &str) -> Result<(), String> {
    sdk_consts::compile("aterm-gui-consts", arch, body, tag)
}

/// The probe source: one `_Static_assert` per constant, using `appkit.rs`'s own
/// literal, each arch-gated constant wrapped in the matching `#if`. `invert`
/// flips ONE row, for the non-vacuity arm.
///
/// THE LITERAL IS PARENTHESISED. See the module note: `|` binds looser than `==`
/// in C, so `SDK == 0x01 | 0x80 | 0x200` asserts `(SDK == 0x01) | …`, which is
/// non-zero whatever `SDK` is. Two rows here are `|`-composed.
fn probe(invert: Option<&str>) -> String {
    let mut s = String::from("#import <Cocoa/Cocoa.h>\n#include <TargetConditionals.h>\n");
    let sdk_of: std::collections::HashMap<&str, &str> = ROWS.iter().copied().collect();
    for (rust, gate, value) in gui_constants() {
        // A constant with no row is the coverage finding, named.
        let sdk = *sdk_of.get(rust.as_str()).unwrap_or_else(|| {
            panic!(
                "`consts` declares {rust} and ROWS has no SDK spelling for it, so \
                 the probe cannot check it; add the row"
            )
        });
        let lit = as_c_literal(&value);
        let lit = if invert == Some(rust.as_str()) {
            // `+ 1` is arithmetic the compiler must evaluate, and it works for
            // the one `double` row as well as every integer one.
            format!("(({lit}) + 1)")
        } else {
            format!("({lit})")
        };
        if let Some(cond) = gate.c_if() {
            s.push_str(&format!("#if {cond}\n"));
        }
        s.push_str(&format!("_Static_assert({sdk} == {lit}, \"{rust}\");\n"));
        if gate.c_if().is_some() {
            s.push_str("#endif\n");
        }
    }
    s.push_str("int main(void){return 0;}\n");
    s
}

/// THE ORACLE, RUN — both arches, and this box cannot execute one of them.
#[test]
fn every_gui_constant_equals_the_sdk_on_both_arches() {
    let body = probe(None);
    for arch in ["arm64", "x86_64"] {
        green_verdict(
            arch,
            "an appkit.rs constant disagrees with the SDK",
            compile(arch, &body, "green"),
        );
    }
}

/// Both scalar and compound-mask value errors must be rejected on both arches.
/// The mask arm exercises the same probe's grouping of `SDK == (a | b | c)`;
/// a wrong compound value must not turn into a truthy bitwise expression.
#[test]
fn the_assertion_is_load_bearing_on_both_arches() {
    for row in [
        "NS_COLOR_RENDERING_INTENT_PERCEPTUAL",
        "NS_TRACKING_HOVER_IN_VISIBLE_RECT",
    ] {
        let body = probe(Some(row));
        for arch in ["arm64", "x86_64"] {
            let result = compile(arch, &body, "inverted");
            if let Err(stderr) = &result {
                assert!(
                    stderr.contains(&format!("\"{row}\"")),
                    "the failed {arch} probe did not name its planted assertion {row}:\n{stderr}"
                );
                eprintln!("SDK rejected planted value error for {row} on {arch}:\n{stderr}");
            }
            inverted_verdict(
                arch,
                row,
                result,
                &format!("an inverted {row} compiled cleanly on {arch}"),
            );
        }
    }
}
