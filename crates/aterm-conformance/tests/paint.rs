// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The paint matrix: the release binary's pixel proof of the bytes it ships.
//!
//! Each row boots the release `aterm` headless, stages a scene, types real
//! keys over the control socket, captures the take and judges its pixels
//! (`aterm_conformance::measure`). The rows are ignored in an ordinary test
//! run; the `measure` check runs them with `-- --ignored` and the release
//! bundle's `Contents/MacOS/aterm` in `ATERM_PAINT_BIN`. Without it a row is
//! red, never "not measured". `ATERM_PAINT_KEEP=1` keeps a take's scratch
//! world for a diagnostic read.
//!
//! What each verdict demands is `measure::paint::judge`; the instrument's
//! account of its own run (`measure::starvation`) can only take a green away,
//! so a take it cannot stand behind is red as unproved. A row is never
//! re-taken. Run verdict-bearing sweeps on a quiet machine: a red whose
//! sampling hole is normal is a rendered gap, a real defect candidate.
//!
//! What each capture perturbs (the observer rule) is in `measure::paint_probe`.
#![cfg(unix)]

mod measuring {
    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::paint::{
        Capture, Companion, Expect, PaintRow, Scene, Split, Style,
    };
    use aterm_conformance::measure::paint_probe::measure;
    use aterm_conformance::measure::starvation::Disposition;

    /// The mature run: 29 keys, no spaces (a space ends the mark). The
    /// traverse is per mark, so a ~10-key mark spans 2-3 of 12 hue buckets by
    /// design; at 29 keys the arc completes and the floors have margin.
    const MATURE_RUN: &str = "t,h,e,r,a,i,n,b,o,w,k,i,t,t,y,p,a,i,n,t,s,t,h,e,a,r,c,o,k";
    /// One word, short of a mature claim for the long styles.
    const WORD: &str = "r,a,i,n,b,o,w,k,i,t,t,y";
    /// Four words: a sustained high-band run (48 keys at four a second), the
    /// only run that earns the flying head.
    const SUSTAINED: &str = concat!(
        "r,a,i,n,b,o,w,k,i,t,t,y,",
        "r,a,i,n,b,o,w,k,i,t,t,y,",
        "r,a,i,n,b,o,w,k,i,t,t,y,",
        "r,a,i,n,b,o,w,k,i,t,t,y"
    );
    const OFF_PHRASE: &str = "r,a,i,n,b,o,w,space,o,n";

    /// A trail row: the companion-free `flying` style, no split.
    const fn trail(scene: Scene, keys: &'static str, expect: Expect, capture: Capture) -> PaintRow {
        PaintRow {
            scene,
            keys,
            expect,
            capture,
            style: Style::Flying,
            companion: None,
            split: None,
        }
    }

    /// Opens every take that decided nothing, so the verify gate records the
    /// machine (COULD NOT RUN), not a paint finding.
    const COULD_NOT_RUN: &str = "aterm-gate: COULD NOT RUN";

    fn run(row: PaintRow) {
        let rig = Rig::release(&["ATERM_PAINT_BIN"], env!("CARGO_BIN_EXE_fake-tui"));
        let scene = row.scene.name();
        let report = match measure(&rig, &row) {
            Ok(report) => report,
            Err(why) => panic!(
                "{COULD_NOT_RUN} — PAINT COULD NOT RUN [{scene}]: the take decided nothing, which \
                 is not a pass: {why}"
            ),
        };
        match report.disposition {
            Disposition::Pass => eprintln!("{report}"),
            Disposition::Fail => panic!(
                "PAINT CONFORMANCE FAILED [{scene}]: expected {:?}: {}.\n  {report}",
                row.expect,
                row.meaning()
            ),
            Disposition::Unproved => panic!(
                "{COULD_NOT_RUN} — PAINT UNPROVED [{scene}]: the take is not evidence about paint ({}); take it on an \
                 unloaded machine, from an interactive shell or a LaunchAgent with \
                 ProcessType=Interactive.\n  {report}",
                report.starvation
            ),
        }
    }

    /// Row 1: a human typing at a main-screen `/bin/sh` prompt leaves trail ink.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn main_screen_prompt_typing_paints_trail_ink() {
        run(trail(
            Scene::Prompt,
            MATURE_RUN,
            Expect::Ink,
            Capture::PinnedVideo,
        ));
    }

    /// Row 1's matched control: the same prompt, focus and recording with the
    /// off style keeps real cursor and echo pixels and no ribbon witness, so
    /// row 1 cannot pass on cursor motion or echoed text. The static window's
    /// frame gaps are read against the recorder's tick ledger.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn main_screen_prompt_with_effect_off_has_no_ribbon() {
        run(PaintRow {
            style: Style::Off,
            ..trail(
                Scene::Prompt,
                "e,c,h,o,space,h,e,l,l,o",
                Expect::EffectOff,
                Capture::PinnedVideo,
            )
        });
    }

    /// Row 2: typing into the fake-Claude alt-screen scene leaves trail ink.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn alt_screen_fake_claude_typing_paints_trail_ink() {
        run(trail(
            Scene::FakeClaude,
            MATURE_RUN,
            Expect::Ink,
            Capture::PinnedVideo,
        ));
    }

    /// Row 3, the dark control: an alt-screen spinner repainting on its own
    /// with nothing typed earns no trail, which makes the other rows
    /// falsifiable.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn alt_screen_cold_spinner_paints_zero_ink() {
        run(trail(
            Scene::ColdSpinner,
            "",
            Expect::TrailDark,
            Capture::PinnedVideo,
        ));
    }

    /// Row 4: typing beside an ESC7/ESC8 token streamer (unowned batches away
    /// from the caret) leaves trail ink, continuity judged in wall-clock time
    /// inside the driven window.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn alt_screen_esc7_esc8_streamer_typing_paints_trail_ink() {
        run(trail(
            Scene::Streamer,
            MATURE_RUN,
            Expect::Ink,
            Capture::PinnedVideo,
        ));
    }

    /// Row 5, the row that can go red on the motion gate: an unfocused window
    /// typed into, captured unpinned. Hue spread is the witness: without the
    /// typed-wake term the trail collapses to one bucket.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn unfocused_typed_window_paints_its_trail_under_an_unpinned_capture() {
        run(trail(
            Scene::FakeClaude,
            MATURE_RUN,
            Expect::Ink,
            Capture::UnpinnedUnfocused,
        ));
    }

    /// Row 5's effect-off twin on the same path.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn unfocused_typed_window_with_effect_off_has_no_ribbon() {
        run(PaintRow {
            style: Style::Off,
            ..trail(
                Scene::FakeClaude,
                OFF_PHRASE,
                Expect::EffectOff,
                Capture::UnpinnedUnfocused,
            )
        });
    }

    /// Row 6, row 5's control: the same window and capture with nothing typed
    /// is not scored as ink. `Quiet`, not `Dark`: a capture inside the
    /// client's bracket sees one hidden-caret cell differ.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn unfocused_idle_window_paints_nothing_under_an_unpinned_capture() {
        run(trail(
            Scene::ColdSpinner,
            "",
            Expect::Quiet,
            Capture::UnpinnedUnfocused,
        ));
    }

    /// Row 6a: focused, unrecorded, typing into a TUI that repaints on its
    /// own: the configuration users are actually in.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn focused_alt_screen_typing_paints_trail_ink_without_a_recording_pin() {
        run(trail(
            Scene::FakeClaude,
            MATURE_RUN,
            Expect::Ink,
            Capture::UnpinnedFocused,
        ));
    }

    /// Row 6a's effect-off twin.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn focused_typed_window_with_effect_off_has_no_ribbon() {
        run(PaintRow {
            style: Style::Off,
            ..trail(
                Scene::FakeClaude,
                OFF_PHRASE,
                Expect::EffectOff,
                Capture::UnpinnedFocused,
            )
        });
    }

    /// Row 6b: the same beside a token streamer; carries the ribbon bind (a
    /// driven mature claim held against the raster over at least two frames).
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn focused_streamer_typing_paints_trail_ink_without_a_recording_pin() {
        run(trail(
            Scene::Streamer,
            MATURE_RUN,
            Expect::Ink,
            Capture::UnpinnedFocused,
        ));
    }

    /// Row 6c, the dark control for 6a/6b.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn focused_cold_spinner_paints_zero_ink_without_a_recording_pin() {
        run(trail(
            Scene::ColdSpinner,
            "",
            Expect::TrailDark,
            Capture::UnpinnedFocused,
        ));
    }

    /// Row 7, the shipped default: the resident pet is whole in the first
    /// still, present in every paired frame, and rides a rainbow while a word
    /// is typed.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn shipped_default_first_still_and_typing_carry_the_resident_pet() {
        run(PaintRow {
            style: Style::Default,
            companion: Some(Companion::Pet),
            ..trail(Scene::Prompt, WORD, Expect::Ink, Capture::UnpinnedFocused)
        });
    }

    /// The explicit underline spelling resolves to the rainbow engine through
    /// the shipped config loader, reports underline geometry, and paints.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn explicit_underline_spelling_paints_an_underline_ribbon() {
        run(PaintRow {
            style: Style::Underline,
            ..trail(
                Scene::Prompt,
                MATURE_RUN,
                Expect::Ink,
                Capture::UnpinnedFocused,
            )
        });
    }

    /// The explicit `rainbow kitty tall` spelling is inside the validator's
    /// domain: it resolves and paints.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn explicit_tall_short_word_paints_four_hues() {
        run(PaintRow {
            style: Style::Tall,
            ..trail(Scene::Prompt, WORD, Expect::Ink, Capture::UnpinnedFocused)
        });
    }

    /// The tall row's exact off twin.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn explicit_tall_short_word_control_has_no_ribbon_when_effect_is_off() {
        run(PaintRow {
            style: Style::Off,
            ..trail(
                Scene::Prompt,
                WORD,
                Expect::EffectOff,
                Capture::UnpinnedFocused,
            )
        });
    }

    /// The absolute-zero control: a companion-free style, cold output, and
    /// literally no ink in any frame.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn cold_output_paints_literally_nothing_for_a_companion_free_style() {
        run(trail(
            Scene::ColdSpinner,
            "",
            Expect::Dark,
            Capture::UnpinnedFocused,
        ));
    }

    /// Row 8: a sustained run earns the flying head, which must be claimed
    /// four times and drawn whole in the three isolated final stills, with no
    /// resident claimed.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn flying_style_sustained_typing_earns_and_paints_the_flying_kitty() {
        run(PaintRow {
            companion: Some(Companion::Cat),
            ..trail(
                Scene::Prompt,
                SUSTAINED,
                Expect::Ink,
                Capture::UnpinnedFocused,
            )
        });
    }

    /// Row 9: a cold stream walking a shown caret with nothing typed, the shape
    /// only the keyboard licence keeps dark. The caret is verified visible and
    /// moving first.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn alt_screen_cold_token_streamer_with_a_walking_caret_paints_zero_ink() {
        run(trail(
            Scene::ColdStreamer,
            "",
            Expect::TrailDark,
            Capture::PinnedVideo,
        ));
    }

    /// Row 10: the `"rainbow kitty"` line carries the resident pet, not the
    /// flying head.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn the_owners_rainbow_kitty_spelling_carries_the_resident_pet() {
        run(PaintRow {
            style: Style::Classic,
            companion: Some(Companion::Pet),
            ..trail(Scene::Prompt, WORD, Expect::Ink, Capture::UnpinnedFocused)
        });
    }

    /// Row 11: one companion body per frame on the `"rainbow kitty"` line, over
    /// a run long enough for both companions to be active at once.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn the_owners_spelling_never_draws_two_companions_in_one_frame() {
        run(PaintRow {
            style: Style::Classic,
            companion: Some(Companion::Pet),
            ..trail(
                Scene::Prompt,
                SUSTAINED,
                Expect::Ink,
                Capture::UnpinnedFocused,
            )
        });
    }

    /// Row 12: a split window being typed into paints through the composed
    /// path in the new focused pane.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn split_window_typing_paints_the_rainbow_body_in_the_focused_pane() {
        run(PaintRow {
            split: Some(Split::V),
            ..trail(
                Scene::Prompt,
                MATURE_RUN,
                Expect::Ink,
                Capture::UnpinnedFocused,
            )
        });
    }

    /// Row 12's off twin: the divider, the second pane and the echo are no
    /// ribbon.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_PAINT_BIN"]
    fn split_window_typing_with_effect_off_has_no_ribbon() {
        run(PaintRow {
            style: Style::Off,
            split: Some(Split::V),
            ..trail(
                Scene::Prompt,
                MATURE_RUN,
                Expect::EffectOff,
                Capture::UnpinnedFocused,
            )
        });
    }
}
