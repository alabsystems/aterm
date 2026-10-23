// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seam's constants.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

use crate::sdk_consts;
use crate::sdk_consts::{as_c_literal, green_verdict, inverted_verdict, parse_const_decl};

/// `(Rust constant, the SDK spelling it claims to equal)`. The SDK name is written out, not
/// derived: `NS_APPKIT_VERSION_NUMBER_10_12` and `NS_CRITICAL_REQUEST` are not the mechanical
/// camel-case of their Rust spelling.
#[rustfmt::skip]
const ROWS: &[(&str, &str)] = &[
    ("NS_WINDOW_STYLE_MASK_TITLED", "NSWindowStyleMaskTitled"),
    ("NS_WINDOW_STYLE_MASK_CLOSABLE", "NSWindowStyleMaskClosable"),
    ("NS_WINDOW_STYLE_MASK_MINIATURIZABLE", "NSWindowStyleMaskMiniaturizable"),
    ("NS_WINDOW_STYLE_MASK_RESIZABLE", "NSWindowStyleMaskResizable"),
    ("NS_WINDOW_OCCLUSION_STATE_VISIBLE", "NSWindowOcclusionStateVisible"),
    ("NS_CRITICAL_REQUEST", "NSCriticalRequest"),
    ("NS_INFORMATIONAL_REQUEST", "NSInformationalRequest"),
    ("NS_BACKING_STORE_BUFFERED", "NSBackingStoreBuffered"),
    ("NS_DRAG_OPERATION_NONE", "NSDragOperationNone"),
    ("NS_DRAG_OPERATION_COPY", "NSDragOperationCopy"),
    ("NS_KEY_VALUE_OBSERVING_OPTION_NEW", "NSKeyValueObservingOptionNew"),
    ("NS_KEY_VALUE_OBSERVING_OPTION_OLD", "NSKeyValueObservingOptionOld"),
    ("NS_APPKIT_VERSION_NUMBER_10_12", "NSAppKitVersionNumber10_12"),
    ("NS_EVENT_MODIFIER_FLAG_SHIFT", "NSEventModifierFlagShift"),
    ("NS_EVENT_MODIFIER_FLAG_CONTROL", "NSEventModifierFlagControl"),
    ("NS_EVENT_MODIFIER_FLAG_OPTION", "NSEventModifierFlagOption"),
    ("NS_EVENT_MODIFIER_FLAG_COMMAND", "NSEventModifierFlagCommand"),
    ("NS_EVENT_TYPE_APPLICATION_DEFINED", "NSEventTypeApplicationDefined"),
    ("NS_EVENT_SUBTYPE_WINDOW_EXPOSED", "NSEventSubtypeWindowExposed"),
    ("NS_EVENT_PHASE_BEGAN", "NSEventPhaseBegan"),
    ("NS_EVENT_PHASE_ENDED", "NSEventPhaseEnded"),
    ("NS_EVENT_PHASE_CANCELLED", "NSEventPhaseCancelled"),
    ("NS_EVENT_PHASE_MAY_BEGIN", "NSEventPhaseMayBegin"),
    ("NS_NOT_FOUND", "NSNotFound"),
    ("NS_UTF8_STRING_ENCODING", "NSUTF8StringEncoding"),
    ("NS_APPLICATION_ACTIVATION_POLICY_REGULAR", "NSApplicationActivationPolicyRegular"),
    ("NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY", "NSApplicationActivationPolicyAccessory"),
    ("NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED", "NSApplicationActivationPolicyProhibited"),
    ("NS_EVENT_TYPE_KEY_UP", "NSEventTypeKeyUp"),
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn seam() -> PathBuf {
    repo().join("crates/aterm-window/src/platform_impl/macos/aterm_objc_seam.rs")
}

/// `(name, literal)` for every `const` in the seam, at any visibility.
fn seam_constants() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(seam()).expect("the seam is readable");
    src.lines().filter_map(parse_const_decl).collect()
}

/// [`sdk_consts::compile`] in this census's own scratch directory.
fn compile(arch: &str, body: &str, tag: &str) -> Result<(), String> {
    sdk_consts::compile("aterm-seam-consts", arch, body, tag)
}

/// The probe source: one `_Static_assert` per seam constant, using the seam's
/// own literal.
fn probe(invert: Option<&str>) -> String {
    let sdk_of: std::collections::HashMap<&str, &str> = ROWS.iter().copied().collect();
    let mut s = String::from("#import <Cocoa/Cocoa.h>\n");
    for (rust, value) in seam_constants() {
        let sdk = *sdk_of.get(rust.as_str()).unwrap_or_else(|| {
            panic!(
                "the seam declares {rust} and ROWS has no SDK spelling for it, so \
                 the probe cannot check it; add the row"
            )
        });
        let lit = as_c_literal(&value);
        let lit = if invert == Some(rust.as_str()) {
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

/// The recipe, run on both arches.
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

/// The same probe with one row inverted fails on both arches.
#[test]
fn the_assertion_is_load_bearing_on_both_arches() {
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
