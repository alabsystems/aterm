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
//! * **Coverage runs both ways off the SOURCE.** Every constant the parser finds
//!   in `consts` must have a row, and every row must name a constant that is
//!   really there.
//!
//! Why it matters: `NS_TEXT_ALIGNMENT_CENTER` once shipped as RIGHT alignment,
//! caught only by a pixel capture, and `NS_COLOR_RENDERING_INTENT_PERCEPTUAL`
//! was first written `1` (it is `3`) — wrong values no run-time test sees.
//!
//! [`winit_seam_constants.rs`](../winit_seam_constants.rs) is the twin over
//! `vendor/winit`'s seam: same parser grammar, same probe, same coverage. Two
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
use std::process::Command;

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
        // A constant with no row would index-panic here. It is named instead,
        // because a bare `KeyError`-shaped panic in the probe reads as a broken
        // test rather than as the finding it is —
        // `every_gui_constant_has_a_row_and_every_row_names_a_gui_constant` is
        // the test that OWNS this failure, and this message says so.
        let sdk = *sdk_of.get(rust.as_str()).unwrap_or_else(|| {
            panic!(
                "`consts` declares {rust} and ROWS has no SDK spelling for it, so \
                 the probe cannot check it; add the row (see \
                 `every_gui_constant_has_a_row_and_every_row_names_a_gui_constant`)"
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

/// THE TABLE AND THE MODULE AGREE, in both directions.
#[test]
fn every_gui_constant_has_a_row_and_every_row_names_a_gui_constant() {
    let found = gui_constants();
    let mut names: Vec<String> = found.iter().map(|(n, _, _)| n.clone()).collect();
    names.sort();
    names.dedup();
    let rows: Vec<String> = ROWS.iter().map(|(n, _)| (*n).to_owned()).collect();

    let missing: Vec<&String> = names.iter().filter(|n| !rows.contains(n)).collect();
    assert!(
        missing.is_empty(),
        "appkit.rs's `consts` gained {} constant(s) with no row here, so nothing \
         checks them against the SDK:\n  {:?}",
        missing.len(),
        missing
    );
    let stale: Vec<&String> = rows.iter().filter(|n| !names.contains(n)).collect();
    assert!(
        stale.is_empty(),
        "these rows name constants `consts` no longer has:\n  {stale:?}"
    );
    assert_eq!(names.len(), ROWS.len(), "one row per distinct constant");
    assert!(
        names.len() >= 31,
        "`consts` had 31 distinct constants when this was written; it now has {}",
        names.len()
    );

    // THE SCOPE IS REAL. A parser that silently found nothing — a renamed
    // module, a reformatted declaration — would satisfy every assertion above
    // by making both lists empty, and this is what says it did not.
    assert!(
        found.len() > names.len(),
        "exactly one constant is arch-split ({}), so the declaration count must \
         exceed the name count",
        "NS_TEXT_ALIGNMENT_CENTER"
    );
    let split: Vec<&(String, Gate, String)> = found
        .iter()
        .filter(|(_, g, _)| *g != Gate::Always)
        .collect();
    assert_eq!(
        split.len(),
        2,
        "the arch-split constant must be parsed as TWO gated declarations, not \
         one: {split:?}"
    );
    assert!(
        split
            .iter()
            .all(|(n, _, _)| n == "NS_TEXT_ALIGNMENT_CENTER"),
        "an unexpected constant is arch-gated: {split:?}"
    );
    assert!(
        split.iter().any(|(_, g, _)| *g == Gate::X86_64)
            && split.iter().any(|(_, g, _)| *g == Gate::NotX86_64),
        "both arms of the arch-split constant must be seen: {split:?}"
    );
}

/// THE ARCH GATE IS READ FROM THE SOURCE, and only an arch gate is.
///
/// If [`arch_gate`] answered `Some` for any attribute, an unrelated
/// `#[cfg(test)]` or `#[allow(…)]` above a constant would put its assertion
/// behind a `#if` that is false on both arches — the assertion would compile,
/// both arms would pass, and the row would be checked by nothing.
#[test]
fn only_an_arch_gate_is_read_as_one() {
    assert_eq!(
        arch_gate(r#"    #[cfg(target_arch = "x86_64")]"#),
        Some(Gate::X86_64)
    );
    assert_eq!(
        arch_gate(r#"#[cfg(not(target_arch = "x86_64"))]"#),
        Some(Gate::NotX86_64)
    );
    for other in [
        "#[cfg(test)]",
        "#[allow(dead_code)]",
        r#"#[cfg(target_os = "macos")]"#,
        r#"#[cfg(target_arch = "aarch64")]"#,
        "/// a doc comment",
        "",
    ] {
        assert_eq!(arch_gate(other), None, "{other:?} is not an arch gate");
    }
    assert_eq!(Gate::Always.c_if(), None);
    assert_eq!(Gate::X86_64.c_if(), Some("TARGET_CPU_X86_64"));
    assert_eq!(Gate::NotX86_64.c_if(), Some("!TARGET_CPU_X86_64"));
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

/// …and the same probe with ONE row inverted must fail on BOTH arches.
///
/// The row picked is `NS_COLOR_RENDERING_INTENT_PERCEPTUAL` — the row this
/// instrument caught on its first run, and the profile of every constant it
/// exists for: an implicit enumerator whose wrong value produces different
/// pixels rather than a failure.
#[test]
fn the_assertion_is_load_bearing_on_both_arches() {
    let body = probe(Some("NS_COLOR_RENDERING_INTENT_PERCEPTUAL"));
    for arch in ["arm64", "x86_64"] {
        inverted_verdict(
            arch,
            "NS_COLOR_RENDERING_INTENT_PERCEPTUAL",
            compile(arch, &body, "inverted"),
            &format!(
                "an INVERTED constant compiled cleanly on {arch}: this test is passing for \
                 the wrong reason and proves nothing about the other one"
            ),
        );
    }
}

/// …AND SO IS THE ARCH SPLIT, which the row above cannot show.
///
/// `NS_TEXT_ALIGNMENT_CENTER` is 1 on arm64 and 2 on x86_64. Inverting it must
/// fail on BOTH arches — which is only possible if each arm really is compiled
/// under its own `#if`. A probe that emitted both declarations unguarded would
/// fail on both arches whatever the values were (1 and 2 cannot both hold), and
/// a probe that emitted only the arm this box compiles would leave the x86_64
/// arm checked by nothing at all. This is the test that separates those.
#[test]
fn the_arch_split_row_is_checked_on_the_arch_it_belongs_to() {
    // Green first: both arms as written must compile on both arches.
    for arch in ["arm64", "x86_64"] {
        green_verdict(
            arch,
            "the arch-split arms disagree with the SDK",
            compile(arch, &probe(None), "split-green"),
        );
    }
    // …and inverted, both arms fail — each on its own arch.
    let body = probe(Some("NS_TEXT_ALIGNMENT_CENTER"));
    for arch in ["arm64", "x86_64"] {
        inverted_verdict(
            arch,
            "NS_TEXT_ALIGNMENT_CENTER",
            compile(arch, &body, "split-inverted"),
            &format!("the arch-split constant's {arch} arm is not really checked on {arch}"),
        );
    }
}

/// THE PARENTHESES ARE LOAD-BEARING, measured rather than argued.
///
/// `|` binds looser than `==` in C, so an unparenthesised compound literal makes
/// the assertion `(SDK == first) | rest` — non-zero whatever `SDK` is. This
/// builds the WRONG value for a real `|`-composed row both ways and requires the
/// unparenthesised form to be accepted (with only a warning, which is why
/// [`compile`] passes `-Werror -Wparentheses`) and the parenthesised form to be
/// rejected. Without this, the two masks in [`ROWS`] would be decoration.
#[test]
fn an_unparenthesised_compound_literal_would_be_vacuous() {
    let sdk =
        "(NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways | NSTrackingInVisibleRect)";
    // The real value is `0x01 | 0x80 | 0x200`; `0x02` is wrong in the first term.
    let wrong = "0x02 | 0x80 | 0x200";
    let bare = format!(
        "#import <Cocoa/Cocoa.h>\n_Static_assert({sdk} == {wrong}, \"\");\nint main(void){{return 0;}}\n"
    );
    let wrapped = format!(
        "#import <Cocoa/Cocoa.h>\n_Static_assert({sdk} == ({wrong}), \"\");\nint main(void){{return 0;}}\n"
    );
    // Unparenthesised: clang warns, and WITHOUT `-Werror` it would compile —
    // which is the vacuity. Compiled here with a plain `cc` to show that.
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-gui-consts-vacuity-")
        .tempdir()
        .expect("a scratch directory");
    let path = dir.path().join("c.m");
    std::fs::write(&path, &bare).expect("writable");
    let out = Command::new("cc")
        .args(["-arch", "arm64", "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("`cc` must be runnable");
    assert!(
        out.status.success(),
        "the unparenthesised form was rejected, so the premise of the \
         parenthesisation has changed — re-derive it:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("lower precedence"),
        "clang no longer warns about `==` before `|`; the only thing standing \
         behind these two rows is the parenthesisation, so say so here"
    );
    // Parenthesised: rejected, as a wrong value must be.
    assert!(
        compile("arm64", &wrapped, "vacuity-wrapped").is_err(),
        "a WRONG parenthesised value compiled: the assertion proves nothing"
    );
}
