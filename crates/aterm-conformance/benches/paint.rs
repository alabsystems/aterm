// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's paint acceptance matrix, run explicitly by `verify measure`.

#[cfg(unix)]
#[path = "support/quiet.rs"]
mod quiet;
#[cfg(unix)]
mod support;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::instance::CouldNotRun;
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

    const ROWS: &[(&str, PaintRow)] = &[
        (
            "main_screen_prompt_typing_paints_trail_ink",
            trail(Scene::Prompt, MATURE_RUN, Expect::Ink, Capture::PinnedVideo),
        ),
        (
            "main_screen_prompt_with_effect_off_has_no_ribbon",
            PaintRow {
                style: Style::Off,
                ..trail(
                    Scene::Prompt,
                    "e,c,h,o,space,h,e,l,l,o",
                    Expect::EffectOff,
                    Capture::PinnedVideo,
                )
            },
        ),
        (
            "alt_screen_fake_claude_typing_paints_trail_ink",
            trail(
                Scene::FakeClaude,
                MATURE_RUN,
                Expect::Ink,
                Capture::PinnedVideo,
            ),
        ),
        (
            "alt_screen_cold_spinner_paints_zero_ink",
            trail(
                Scene::ColdSpinner,
                "",
                Expect::TrailDark,
                Capture::PinnedVideo,
            ),
        ),
        (
            "alt_screen_esc7_esc8_streamer_typing_paints_trail_ink",
            trail(
                Scene::Streamer,
                MATURE_RUN,
                Expect::Ink,
                Capture::PinnedVideo,
            ),
        ),
        (
            "unfocused_typed_window_paints_its_trail_under_an_unpinned_capture",
            trail(
                Scene::FakeClaude,
                MATURE_RUN,
                Expect::Ink,
                Capture::UnpinnedUnfocused,
            ),
        ),
        (
            "unfocused_typed_window_with_effect_off_has_no_ribbon",
            PaintRow {
                style: Style::Off,
                ..trail(
                    Scene::FakeClaude,
                    OFF_PHRASE,
                    Expect::EffectOff,
                    Capture::UnpinnedUnfocused,
                )
            },
        ),
        (
            "unfocused_idle_window_paints_nothing_under_an_unpinned_capture",
            trail(
                Scene::ColdSpinner,
                "",
                Expect::Quiet,
                Capture::UnpinnedUnfocused,
            ),
        ),
        (
            "focused_alt_screen_typing_paints_trail_ink_without_a_recording_pin",
            trail(
                Scene::FakeClaude,
                MATURE_RUN,
                Expect::Ink,
                Capture::UnpinnedFocused,
            ),
        ),
        (
            "focused_typed_window_with_effect_off_has_no_ribbon",
            PaintRow {
                style: Style::Off,
                ..trail(
                    Scene::FakeClaude,
                    OFF_PHRASE,
                    Expect::EffectOff,
                    Capture::UnpinnedFocused,
                )
            },
        ),
        (
            "focused_streamer_typing_paints_trail_ink_without_a_recording_pin",
            trail(
                Scene::Streamer,
                MATURE_RUN,
                Expect::Ink,
                Capture::UnpinnedFocused,
            ),
        ),
        (
            "focused_cold_spinner_paints_zero_ink_without_a_recording_pin",
            trail(
                Scene::ColdSpinner,
                "",
                Expect::TrailDark,
                Capture::UnpinnedFocused,
            ),
        ),
        (
            "shipped_default_first_still_and_typing_carry_the_resident_pet",
            PaintRow {
                style: Style::Default,
                companion: Some(Companion::Pet),
                ..trail(Scene::Prompt, WORD, Expect::Ink, Capture::UnpinnedFocused)
            },
        ),
        (
            "explicit_underline_spelling_paints_an_underline_ribbon",
            PaintRow {
                style: Style::Underline,
                ..trail(
                    Scene::Prompt,
                    MATURE_RUN,
                    Expect::Ink,
                    Capture::UnpinnedFocused,
                )
            },
        ),
        (
            "explicit_tall_short_word_paints_four_hues",
            PaintRow {
                style: Style::Tall,
                ..trail(Scene::Prompt, WORD, Expect::Ink, Capture::UnpinnedFocused)
            },
        ),
        (
            "explicit_tall_short_word_control_has_no_ribbon_when_effect_is_off",
            PaintRow {
                style: Style::Off,
                ..trail(
                    Scene::Prompt,
                    WORD,
                    Expect::EffectOff,
                    Capture::UnpinnedFocused,
                )
            },
        ),
        (
            "cold_output_paints_literally_nothing_for_a_companion_free_style",
            trail(
                Scene::ColdSpinner,
                "",
                Expect::Dark,
                Capture::UnpinnedFocused,
            ),
        ),
        (
            "flying_style_sustained_typing_earns_and_paints_the_flying_kitty",
            PaintRow {
                companion: Some(Companion::Cat),
                ..trail(
                    Scene::Prompt,
                    SUSTAINED,
                    Expect::Ink,
                    Capture::UnpinnedFocused,
                )
            },
        ),
        (
            "alt_screen_cold_token_streamer_with_a_walking_caret_paints_zero_ink",
            trail(
                Scene::ColdStreamer,
                "",
                Expect::TrailDark,
                Capture::PinnedVideo,
            ),
        ),
        (
            "the_owners_rainbow_kitty_spelling_carries_the_resident_pet",
            PaintRow {
                style: Style::Classic,
                companion: Some(Companion::Pet),
                ..trail(Scene::Prompt, WORD, Expect::Ink, Capture::UnpinnedFocused)
            },
        ),
        (
            "the_owners_spelling_never_draws_two_companions_in_one_frame",
            PaintRow {
                style: Style::Classic,
                companion: Some(Companion::Pet),
                ..trail(
                    Scene::Prompt,
                    SUSTAINED,
                    Expect::Ink,
                    Capture::UnpinnedFocused,
                )
            },
        ),
        (
            "split_window_typing_paints_the_rainbow_body_in_the_focused_pane",
            PaintRow {
                split: Some(Split::V),
                ..trail(
                    Scene::Prompt,
                    MATURE_RUN,
                    Expect::Ink,
                    Capture::UnpinnedFocused,
                )
            },
        ),
        (
            "split_window_typing_with_effect_off_has_no_ribbon",
            PaintRow {
                style: Style::Off,
                split: Some(Split::V),
                ..trail(
                    Scene::Prompt,
                    MATURE_RUN,
                    Expect::EffectOff,
                    Capture::UnpinnedFocused,
                )
            },
        ),
    ];

    fn take(rig: &Rig, row: &PaintRow) -> Outcome {
        let scene = row.scene.name();
        let report = match measure(rig, row) {
            Ok(report) => report,
            Err(CouldNotRun::Unanswered(why)) => return Outcome::Unanswered(why),
            Err(why) => {
                return Outcome::CouldNotRun(format!(
                    "PAINT COULD NOT RUN [{scene}]: the take decided nothing: {why}"
                ));
            }
        };
        match report.disposition {
            Disposition::Pass => Outcome::Pass(report.to_string()),
            Disposition::Fail => Outcome::Fail(format!(
                "PAINT CONFORMANCE FAILED [{scene}]: expected {:?}: {}.\n  {report}",
                row.expect,
                row.meaning()
            )),
            Disposition::Unproved => Outcome::CouldNotRun(format!(
                "PAINT UNPROVED [{scene}]: the take is not evidence about paint ({}); \
                 take it on an unloaded machine, from an interactive shell or a LaunchAgent \
                 with ProcessType=Interactive.\n  {report}",
                report.starvation
            )),
        }
    }

    pub fn run() -> std::process::ExitCode {
        let rig = Rig::release(&["ATERM_PAINT_BIN"], env!("CARGO_BIN_EXE_fake-tui"));
        support::run("paint", ROWS, |row| take(&rig, row))
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        quiet::run("paint", measuring::run)
    }
    #[cfg(not(unix))]
    {
        eprintln!("measure: COULD NOT RUN — paint requires a Unix host");
        std::process::ExitCode::from(3)
    }
}
