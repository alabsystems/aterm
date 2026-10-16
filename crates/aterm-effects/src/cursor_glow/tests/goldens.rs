// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The byte-exact deletion and flat-spelling goldens.

use super::*;

/// Every deletion-script variant: the nine distinct built-ins dark
/// (`BUILTINS`' tenth entry is padding), then the kitty's light,
/// underline and reduced-motion rows — [`kitty_rows`]' own configs,
/// minus the dark row the built-ins loop already produced.
fn deletion_goldens() -> Vec<(String, u64)> {
    let g = geom();
    BUILTINS
        .iter()
        .take(9)
        .map(|style| {
            let c = cfg(*style, true);
            (format!("{style:?} dark"), deletion_script(&c, g, false))
        })
        .chain(
            kitty_rows(false)[1..]
                .iter()
                .map(|&(name, fp)| (name.to_string(), fp)),
        )
        .collect()
}

/// The four kitty rows of the deletion script — dark, light, underline,
/// reduced-motion — with the flat spelling on or off. The one fixture
/// for "the four kitty configs": [`deletion_goldens`] takes its last
/// three rows from here.
fn kitty_rows(flat: bool) -> [(&'static str, u64); 4] {
    let g = geom();
    let base = |tall: bool| {
        let mut c = cfg(GlowStyle::RainbowKitty, true);
        c.ribbon_tall = tall;
        c.ribbon_flat = flat;
        c
    };
    let dark = base(true);
    let mut light = base(true);
    light.dark_theme = false;
    light.theme_fg = 0x0033_3333;
    light.theme_bg = 0x00FA_FAFA;
    let under = base(false);
    [
        ("RainbowKitty dark", deletion_script(&dark, g, false)),
        ("RainbowKitty light", deletion_script(&light, g, false)),
        ("RainbowKitty underline", deletion_script(&under, g, false)),
        ("RainbowKitty reduced", deletion_script(&dark, g, true)),
    ]
}

/// The gate is byte-exact (§30): the `rainbow kitty flat` spelling folds every
/// kitty row to its own pinned number, and the default body folds none of them
/// to it (the comet moves all four; the reduced row keeps the shape and rail
/// and drops only the wipe).
#[test]
fn the_flat_spelling_collapses_every_comet_branch_byte_for_byte() {
    assert!(
        cfg_for_style_name("rainbow kitty flat", true).ribbon_flat,
        "the spelling reaches the flag"
    );
    assert!(!cfg_for_style_name("rainbow kitty", true).ribbon_flat);
    let flat = kitty_rows(true);
    let mut moved = Vec::new();
    for ((name, got), (want_name, want)) in flat.iter().zip(FLAT_GOLDENS) {
        assert_eq!(*name, want_name);
        // The four flat rows are Apple-silicon exact bits like the deletion
        // goldens (`arm64_pin`): asserted everywhere but x86_64 macOS, where a
        // miss is reported and the render's determinism is asserted instead.
        if crate::arm64_pin::moved(
            "cursor_glow::tests::goldens::the_flat_spelling_collapses_every_comet_branch_byte_for_byte",
            name,
            *got,
            want,
        ) {
            moved.push(format!("{name}: got {got} want {want}"));
        }
    }
    assert!(
        moved.is_empty(),
        "the flat spelling is no longer the flat body:\n{}",
        moved.join("\n")
    );
    crate::arm64_pin::deterministic_on_x86_64("the four flat kitty rows", &flat, || {
        kitty_rows(true)
    });
    for ((name, got), (_, pre)) in kitty_rows(false).iter().zip(FLAT_GOLDENS) {
        assert_ne!(
            *got, pre,
            "{name}: the comet must move this row, or the flat pin is vacuous"
        );
    }
}

/// Every built-in style except rainbow kitty emits its pinned bytes: v2
/// never engages for them.
#[test]
fn the_other_nine_styles_are_byte_identical_with_v2_unconditional() {
    let rows = deletion_goldens();
    assert_eq!(
        rows,
        deletion_goldens(),
        "the deletion script is nondeterministic"
    );
    for ((name, got), (want_name, want)) in rows.iter().zip(DELETION_GOLDENS) {
        assert_eq!(name, want_name);
        if name.starts_with("RainbowKitty") {
            continue;
        }
        crate::arm64_pin::assert_pinned(
            "cursor_glow::tests::goldens::the_other_nine_styles_are_byte_identical_with_v2_unconditional",
            name,
            *got,
            want,
            format_args!("{name}: the deletion moved a byte"),
        );
    }
}
