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
use std::process::Command;

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
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn seam() -> PathBuf {
    repo().join("vendor/winit/src/platform_impl/macos/aterm_objc_seam.rs")
}

/// A `const` declaration's body, whatever VISIBILITY it is spelled with.
///
/// # This function is the fourteenth pass's finding
///
/// It was `line.trim().strip_prefix("pub(crate) const ")`, and all 42 rows in
/// the seam happen to be spelled that way. **That armed the guard at one
/// SPELLING of its rule rather than at the rule.** `pub const` inside the
/// seam's private `consts` module is the same declaration with the same
/// visibility in practice — arguably the more idiomatic spelling, since the
/// module is already `pub(crate)` — and a `pub const` carrying a WRONG value
/// was planted here and passed all three tests in this file, including the
/// coverage test whose entire job is to say that every seam constant has a row.
///
/// That is F1's shape, recurring inside the fix for the constant-recipe gap:
/// the previous pass found a `load_borrowed` rebuilt with its lifetime behind a
/// `PhantomData` wrapper slipping through a guard armed at `&`. The question to
/// ask of any new guard is what the same defect looks like spelled differently,
/// so this parser is written against the GRAMMAR — `pub`, `pub(crate)`,
/// `pub(super)`, `pub(in path)` or nothing — and
/// [`the_parser_reads_every_visibility_spelling`] proves it reads all of them.
fn strip_const_decl(line: &str) -> Option<&str> {
    let mut s = line.trim();
    if let Some(rest) = s.strip_prefix("pub") {
        // `pub(crate)`, `pub(super)`, `pub(in crate::x)` — skip the qualifier,
        // then the whitespace before `const`.
        s = match rest.strip_prefix('(') {
            Some(q) => q.split_once(')')?.1,
            None => rest,
        };
        // A bare `pub` must be followed by whitespace, or this matched the
        // prefix of some other identifier (`pubfoo`).
        if !s.is_empty() && !s.starts_with(char::is_whitespace) {
            return None;
        }
        s = s.trim_start();
    }
    s.strip_prefix("const ")
}

/// `(name, literal)` for every `const` in the seam, at any visibility.
fn seam_constants() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(seam()).expect("the seam is readable");
    let mut out = Vec::new();
    for line in src.lines() {
        let Some(rest) = strip_const_decl(line) else {
            continue;
        };
        let (name, rest) = rest.split_once(':').expect("a typed const");
        let (_ty, value) = rest.split_once('=').expect("an initialised const");
        out.push((
            name.trim().to_owned(),
            value.trim().trim_end_matches(';').trim().to_owned(),
        ));
    }
    out
}

/// A Rust integer literal as C: only `_` separators differ (`0x7fff_ffff…`).
fn as_c_literal(rust: &str) -> String {
    rust.replace('_', "")
}

/// Compile `body` as Objective-C for `arch`, syntax only. `Ok(())` if clang
/// accepted it, `Err(stderr)` if it did not.
fn compile(arch: &str, body: &str, tag: &str) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("aterm-seam-consts-{tag}-{arch}"));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join("c.m");
    std::fs::write(&path, body).expect("the probe is writable");
    let out = Command::new("cc")
        .args(["-arch", arch, "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect(
            "`cc` must be runnable: this test IS the measurement the seam's \
             comment promised, and a measurement that cannot run is not a pass",
        );
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// The probe source: one `_Static_assert` per row, using the seam's own
/// literal. `invert` flips ONE row, for the non-vacuity arm.
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
            lit
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

/// THE PARSER READS EVERY VISIBILITY SPELLING, not just the one the seam uses.
///
/// Without this, [`strip_const_decl`] could regress to a single `strip_prefix`
/// and every test in this file would stay green, because all 42 live rows are
/// spelled `pub(crate) const`. The coverage test cannot notice a constant it
/// never parsed: a row it does not see is not "missing", it is invisible.
///
/// So the spellings are exercised against a synthetic source rather than
/// against the seam — the seam is allowed to use only one of them, and this
/// test is about the other four.
#[test]
fn the_parser_reads_every_visibility_spelling() {
    let cases = [
        ("const A: usize = 1;", Some(("A", "1"))),
        ("pub const B: usize = 2;", Some(("B", "2"))),
        ("    pub(crate) const C: usize = 3;", Some(("C", "3"))),
        ("pub(super) const D: usize = 4;", Some(("D", "4"))),
        ("pub(in crate::x) const E: usize = 5;", Some(("E", "5"))),
        // …and things that are NOT a constant declaration stay unparsed.
        ("let id = (r as *const T).cast_mut();", None),
        ("pub(super) static F: Id;", None),
        ("pubconst G: usize = 7;", None),
    ];
    for (line, want) in cases {
        let got = strip_const_decl(line).map(|rest| {
            let (name, rest) = rest.split_once(':').expect("a typed const");
            let (_ty, value) = rest.split_once('=').expect("an initialised const");
            (
                name.trim().to_owned(),
                value.trim().trim_end_matches(';').trim().to_owned(),
            )
        });
        let got = got.as_ref().map(|(n, v)| (n.as_str(), v.as_str()));
        assert_eq!(got, want, "the parser disagreed on {line:?}");
    }
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
        if let Err(stderr) = compile(arch, &body, "green") {
            panic!("a seam constant disagrees with the SDK on {arch}:\n{stderr}");
        }
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
        assert!(
            compile(arch, &body, "inverted").is_err(),
            "an INVERTED constant compiled cleanly on {arch}: this test is \
             passing for the wrong reason and proves nothing about the other one"
        );
    }
}
