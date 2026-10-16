// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seam's constants, `_Static_assert`ed against the SDK on BOTH arches.
//!
//! `crates/aterm-window/src/platform_impl/macos/aterm_objc_seam.rs`'s `consts` module
//! carries AppKit and Foundation values, and its header gives the recipe:
//!
//! ```text
//! Every value below is `_Static_assert`ed against the SDK on BOTH arches
//!   $ cc -arch arm64  -fsyntax-only /tmp/c.m   # passes
//!   $ cc -arch x86_64 -fsyntax-only /tmp/c.m   # passes
//! ```
//!
//! This file runs it. `winit` is a path dependency, NOT a workspace member, so
//! `targo test --workspace` never compiles a test inside it; `aterm-objc` is a
//! member, and these constants feed its sends.
//!
//! Why: `NS_TEXT_ALIGNMENT_CENTER` shipped as `2` (RIGHT alignment) because its
//! `#if TARGET_ABI_USES_IOS_VALUES` guard was read as "iOS vs macOS" when on
//! Apple Silicon it reduces to `!TARGET_CPU_X86_64`. Every test passed; only a
//! pixel capture caught it.
//!
//! # Non-vacuity, three ways
//!
//! * The table must cover the seam's `consts` module EXACTLY — every constant
//!   parsed out of the file must have a row here, and every row must name a
//!   constant that is really there.
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
/// un-snake-casing it. A derivation would be a second guess — and it would be
/// wrong for `NS_APPKIT_VERSION_NUMBER_10_12` and `NS_CRITICAL_REQUEST`, neither
/// of which is the mechanical camel-case of its Rust spelling.
//
// `rustfmt::skip`, as `winit_seam.rs`'s row table does and for the same reason:
// one row per line is what makes a lookup table checkable by eye against the
// seam it mirrors.
#[rustfmt::skip]
const ROWS: &[(&str, &str)] = &[
    // ---- NSWindowStyleMask ----
    ("NS_WINDOW_STYLE_MASK_TITLED", "NSWindowStyleMaskTitled"),
    ("NS_WINDOW_STYLE_MASK_CLOSABLE", "NSWindowStyleMaskClosable"),
    ("NS_WINDOW_STYLE_MASK_MINIATURIZABLE", "NSWindowStyleMaskMiniaturizable"),
    ("NS_WINDOW_STYLE_MASK_RESIZABLE", "NSWindowStyleMaskResizable"),
    // ---- the singles ----
    ("NS_WINDOW_OCCLUSION_STATE_VISIBLE", "NSWindowOcclusionStateVisible"),
    // ---- NSRequestUserAttentionType ----
    ("NS_CRITICAL_REQUEST", "NSCriticalRequest"),
    ("NS_INFORMATIONAL_REQUEST", "NSInformationalRequest"),
    // ---- window, drag, KVO, AppKit version ----
    ("NS_BACKING_STORE_BUFFERED", "NSBackingStoreBuffered"),
    ("NS_DRAG_OPERATION_NONE", "NSDragOperationNone"),
    ("NS_DRAG_OPERATION_COPY", "NSDragOperationCopy"),
    ("NS_KEY_VALUE_OBSERVING_OPTION_NEW", "NSKeyValueObservingOptionNew"),
    ("NS_KEY_VALUE_OBSERVING_OPTION_OLD", "NSKeyValueObservingOptionOld"),
    ("NS_APPKIT_VERSION_NUMBER_10_12", "NSAppKitVersionNumber10_12"),
    // ---- event.rs ----
    ("NS_EVENT_MODIFIER_FLAG_SHIFT", "NSEventModifierFlagShift"),
    ("NS_EVENT_MODIFIER_FLAG_CONTROL", "NSEventModifierFlagControl"),
    ("NS_EVENT_MODIFIER_FLAG_OPTION", "NSEventModifierFlagOption"),
    ("NS_EVENT_MODIFIER_FLAG_COMMAND", "NSEventModifierFlagCommand"),
    ("NS_EVENT_TYPE_APPLICATION_DEFINED", "NSEventTypeApplicationDefined"),
    ("NS_EVENT_SUBTYPE_WINDOW_EXPOSED", "NSEventSubtypeWindowExposed"),
    // ---- view.rs ----
    ("NS_EVENT_PHASE_BEGAN", "NSEventPhaseBegan"),
    ("NS_EVENT_PHASE_ENDED", "NSEventPhaseEnded"),
    ("NS_EVENT_PHASE_CANCELLED", "NSEventPhaseCancelled"),
    ("NS_EVENT_PHASE_MAY_BEGIN", "NSEventPhaseMayBegin"),
    ("NS_NOT_FOUND", "NSNotFound"),
    ("NS_UTF8_STRING_ENCODING", "NSUTF8StringEncoding"),
    // ---- app_state.rs's activation policy ----
    ("NS_APPLICATION_ACTIVATION_POLICY_REGULAR", "NSApplicationActivationPolicyRegular"),
    ("NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY", "NSApplicationActivationPolicyAccessory"),
    ("NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED", "NSApplicationActivationPolicyProhibited"),
    // ---- app.rs's swizzled -sendEvent: ----
    //
    // The `NSEventType` enumerator the override matches on. It matched eleven
    // until winit-core step 5 (2026-10-15) took its device-event dispatch, and
    // the ten mouse types with it.
    ("NS_EVENT_TYPE_KEY_UP", "NSEventTypeKeyUp"),
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn seam() -> PathBuf {
    repo().join("crates/aterm-window/src/platform_impl/macos/aterm_objc_seam.rs")
}

/// `(name, literal)` for every `const` in the seam, at any visibility.
///
/// The visibility grammar is `sdk_consts::strip_const_decl`'s, shared with
/// `gui_appkit_constants.rs`;
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
/// its rows, where the shape was found. The
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
    // A non-vacuity floor at the measured count: 42 when this was written, 39
    // since winit-core step 5 (2026-10-15) carved the backend code that read
    // seventeen of them, 34 since its window-control carve (2026-10-16) took the
    // borderless mask, the zoom button, both sharing types and the
    // parent-window ordering, and 29 since its exclusive-fullscreen carve took
    // the five presentation options.
    assert!(
        seam.len() >= 29,
        "the seam has {} constants; it had 29 when the floor was last measured",
        seam.len()
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
