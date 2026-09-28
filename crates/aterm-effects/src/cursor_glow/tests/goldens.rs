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

/// **THE GATE IS BYTE-EXACT** (§30): the `rainbow kitty flat` spelling
/// folds every kitty row to its own pinned number, and the default body
/// folds none of them to it — the comet moves all four, light and
/// reduced included (the comet is a shape on both grounds; the reduced
/// row keeps the shape and the rail and drops only the wipe).
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
        // goldens above them (`arm64_pin`): asserted everywhere but x86_64
        // macOS, where a miss is reported and the render's determinism is
        // what is asserted instead.
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

/// THE ONE HARD LAW OF THE DELETION (§17.3 phase 7): with v2
/// unconditional for the style and v1's rainbow arms gone, every
/// built-in style except rainbow kitty emits the same bytes it emitted
/// with v1 in the tree — v2 never engages for them, and the code the
/// deletion removed was never on their path.
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

/// v2 IS the rainbow kitty, and the deletion moved none of its bytes:
/// dark, light, underline and reduced-motion each fold to the seam-era
/// golden — the v1 bookkeeping that used to run beside v2 fed nothing
/// v2 draws, reads or schedules. The kitty rows are re-pinned whenever
/// v2 changes its own bytes — the re-bake law on [`DELETION_GOLDENS`];
/// this pin is what makes any later drift a decision instead of a
/// surprise.
///
/// RE-BAKED 2026-09-15, THE EDIT ROUND'S ANSWER LANE, AND THE WALK IS
/// THE WHOLE OF IT. All four kitty rows move, and what moved them is
/// ONE law: [`rk::ribbon::CROSS_PACE`], the re-pace of the walk across
/// the green→blue leg (the owner: *"some blending smoothness in green
/// to blue where the gradient sometimes looked blocky"*). The script
/// draws the band on every frame, so a change to WHICH STOP a walk
/// position resolves to reaches every one of them.
///
/// **DECOMPOSED, NOT ASSUMED** (measured 2026-09-15 by switching each
/// of the round's laws off in turn and re-reading this table):
///
/// * with [`rk::ribbon::walk_pace`] forced to the IDENTITY and every
///   other law of the round left in place, these four rows read
///   `4_461_815_233_714_909_726`, `15_435_435_340_954_993_666`,
///   `7_623_959_890_164_863_941` and `380_128_264_356_753_282` — the
///   bytes v0.86.0 shipped (`de9846727`), to the bit. The round's other
///   laws — the fold that keeps its row, the repaint's home, the
///   witness's blank cells, the erase (which is v0.86.0's own
///   `retract_suffix` again, the slide having been dropped after
///   measurement) — move NO byte of this script.
/// * the green→blue confinement is `rk::ribbon::tests::`
///   `the_crossing_is_smoothed_out_of_its_own_legs_cells_and_nothing_else_moves`:
///   every step the shipped walk placed outside green→blue is unchanged
///   to the bit (`moved_off_leg == 0`), all seven anchors keep their
///   residency, and cells 0–4 — the warm end the owner asked to see
///   more of — sit on exactly the stops they shipped with.
/// * THE EIGHT NON-KITTY ROWS ARE UNTOUCHED
///   (`the_other_nine_styles_are_byte_identical_with_v2_unconditional`
///   is green over them), and `licensed_typed_parity`'s nine-style
///   golden moved ONLY its entry 2: no other trail style moved.
///
/// The bake was taken AFTER a defect it would otherwise have pinned was
/// fixed. `Ribbon::typed_landing` — the round's "a key lays where the
/// hand is" — read [`Ribbon::caret`] as the hand, and that field is
/// seeded from the engine's caret MIRROR, which is `(0, 0)` until a
/// caret has been observed at all. Traced on this very script, the
/// FIRST key of the session (`caret=(2, 5)`, mirror `(0, 0)`) was
/// redirected to `(0, 1)`: a stray on a row the hand has never been on,
/// which is the defect the law exists to prevent. The law now requires
/// the hand's row to be a row this ribbon holds a cell on, and with
/// that guard it moves no byte here.
///
/// At the 0.86 candidate's final catch-up with main (2026-09-15) this
/// pin was read and NOT re-baked: main's incoming round moved none of
/// the four, so the numbers on [`DELETION_GOLDENS`] are the merged
/// tree's own — measured on its run, `checked == 4`. (The stray `)` a
/// previous hand-merge left on "surprise.)" is gone with it: main's
/// spelling of the sentence was the whole one.)
///
/// RE-BAKED 2026-09-16, THE EXHAUST (§33) — five laws, each landed as
/// its own commit and each decomposed by switching it off and reading
/// the previous number; the paragraph on [`DELETION_GOLDENS`] names them
/// and which rows each moved. The control that held throughout:
/// **reduced did not move one byte** — the exhaust's first gate is
/// `cfg.reduced_motion` and the slipstream is a position law `Star::pos`
/// ignores under it — and the light row moved by a DIFFERENT number
/// from dark, which is `light_keeps` thinning the population to
/// [`rk::stardust::LIGHT_COUNT_SCALE`]; a bake that moved light and dark
/// by one law would have been the tell that something other than the
/// exhaust moved. The eight non-kitty rows are untouched
/// (`the_other_nine_styles_are_byte_identical_with_v2_unconditional`).
#[test]
fn rainbow_kitty_is_byte_identical_to_the_seam_era_v2() {
    let rows = deletion_goldens();
    let mut checked = 0;
    // Every kitty row is read before any is judged, so one re-bake shows
    // all four moved numbers at once instead of one per run.
    let mut moved = Vec::new();
    for ((name, got), (want_name, want)) in rows.iter().zip(DELETION_GOLDENS) {
        assert_eq!(name, want_name);
        if !name.starts_with("RainbowKitty") {
            continue;
        }
        checked += 1;
        if crate::arm64_pin::moved(
            "cursor_glow::tests::goldens::rainbow_kitty_is_byte_identical_to_the_seam_era_v2",
            name,
            *got,
            want,
        ) {
            moved.push(format!("{name}: got {got} want {want}"));
        }
    }
    assert!(
        moved.is_empty(),
        "the deletion moved a v2 byte:\n{}",
        moved.join("\n")
    );
    assert_eq!(checked, 4);
}
