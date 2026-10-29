// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verify `aterm-gui`'s AppKit constants (`appkit.rs`) against Apple's SDK headers on both architectures.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

use crate::sdk_consts;
use crate::sdk_consts::{as_c_literal, green_verdict, inverted_verdict, parse_const_decl};

/// `(Rust constant, the SDK spelling it claims to equal)`. The SDK name is written out, not
/// derived: some (`NS_MODAL_RESPONSE_OK`, `CG_NULL_WINDOW_ID`, the `|`-composed masks) are not
/// the camel-case of their Rust spelling, and the masks make the parenthesisation in [`probe`]
/// load-bearing.
#[rustfmt::skip]
const ROWS: &[(&str, &str)] = &[
    ("NS_VARIABLE_STATUS_ITEM_LENGTH",        "NSVariableStatusItemLength"),
    ("NS_EVENT_MODIFIER_FLAG_SHIFT",          "NSEventModifierFlagShift"),
    ("NS_EVENT_MODIFIER_FLAG_CONTROL",        "NSEventModifierFlagControl"),
    ("NS_EVENT_MODIFIER_FLAG_COMMAND",        "NSEventModifierFlagCommand"),
    ("NS_MODAL_RESPONSE_OK",                  "NSModalResponseOK"),
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
    ("NS_EVENT_MASK_KEY_DOWN",                "NSEventMaskKeyDown"),
    ("NS_ALERT_FIRST_BUTTON_RETURN",          "NSAlertFirstButtonReturn"),
    ("NS_ALERT_SECOND_BUTTON_RETURN",         "NSAlertSecondButtonReturn"),
    ("NS_WINDOW_TOOLBAR_STYLE_AUTOMATIC",     "NSWindowToolbarStyleAutomatic"),
    ("NS_WINDOW_TOOLBAR_STYLE_EXPANDED",      "NSWindowToolbarStyleExpanded"),
    ("NS_WINDOW_TOOLBAR_STYLE_PREFERENCE",    "NSWindowToolbarStylePreference"),
    ("NS_WINDOW_TOOLBAR_STYLE_UNIFIED",       "NSWindowToolbarStyleUnified"),
    ("NS_TOOLBAR_DISPLAY_MODE_DEFAULT",       "NSToolbarDisplayModeDefault"),
    ("NS_TOOLBAR_DISPLAY_MODE_ICON_AND_LABEL","NSToolbarDisplayModeIconAndLabel"),
    ("NS_TOOLBAR_DISPLAY_MODE_LABEL_ONLY",    "NSToolbarDisplayModeLabelOnly"),
    ("NS_BITMAP_IMAGE_FILE_TYPE_PNG",         "NSBitmapImageFileTypePNG"),
    ("NS_COLOR_RENDERING_INTENT_PERCEPTUAL",  "NSColorRenderingIntentPerceptual"),
    (
        "NS_ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP",
        "NSActivityUserInitiatedAllowingIdleSystemSleep",
    ),
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
    (
        "CG_WINDOW_LIST_ALL_NON_DESKTOP",
        "(kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements)",
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
    /// The C preprocessor condition that selects the same arch; `None` for [`Gate::Always`].
    fn c_if(self) -> Option<&'static str> {
        match self {
            Gate::Always => None,
            Gate::X86_64 => Some("TARGET_CPU_X86_64"),
            Gate::NotX86_64 => Some("!TARGET_CPU_X86_64"),
        }
    }
}

/// The arch gate an attribute line states; `None` for any other line (`#[cfg(test)]`, a lint
/// allow, a doc comment), which is not the same as [`Gate::Always`].
fn arch_gate(line: &str) -> Option<Gate> {
    match line.trim() {
        r#"#[cfg(target_arch = "x86_64")]"# => Some(Gate::X86_64),
        r#"#[cfg(not(target_arch = "x86_64"))]"# => Some(Gate::NotX86_64),
        _ => None,
    }
}

/// `(name, gate, literal)` for every `const` in `appkit.rs`'s `consts` module, at any
/// visibility. Scoped to the module by brace depth: the file's other constants are not AppKit
/// values.
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
            let opens = trimmed.matches('{').count() as i32;
            let closes = trimmed.matches('}').count() as i32;
            if let Some(g) = arch_gate(line) {
                gate = g;
            } else if let Some((name, value)) = parse_const_decl(line) {
                out.push((name, gate, value));
                gate = Gate::Always;
            } else if !trimmed.is_empty() && !trimmed.starts_with("//") {
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

/// The probe source: one `_Static_assert` per constant on `appkit.rs`'s own literal, each
/// arch-gated constant inside the matching `#if`, the literal parenthesised (see the module
/// doc). `invert` flips one row, for the non-vacuity arm.
fn probe(invert: Option<&str>) -> String {
    let mut s = String::from("#import <Cocoa/Cocoa.h>\n#include <TargetConditionals.h>\n");
    let sdk_of: std::collections::HashMap<&str, &str> = ROWS.iter().copied().collect();
    for (rust, gate, value) in gui_constants() {
        let sdk = *sdk_of.get(rust.as_str()).unwrap_or_else(|| {
            panic!(
                "`consts` declares {rust} and ROWS has no SDK spelling for it, so \
                 the probe cannot check it; add the row"
            )
        });
        let lit = as_c_literal(&value);
        let lit = if invert == Some(rust.as_str()) {
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

/// The oracle, on both arches.
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

/// One row inverted fails on both arches, for a scalar row and a `|`-composed mask:
/// `NS_COLOR_RENDERING_INTENT_PERCEPTUAL` is an implicit enumerator whose wrong value gives
/// different pixels rather than a failure, and the mask's wrong value must stay grouped as
/// `SDK == (a | b | c)` rather than become a truthy `(SDK == a) | b | c`.
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
