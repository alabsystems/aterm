// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's paint acceptance matrix, run explicitly by `verify measure`.

#[cfg(unix)]
#[path = "support/quiet.rs"]
mod quiet;
#[cfg(unix)]
mod support;
#[cfg(unix)]
#[path = "support/telemetry.rs"]
mod telemetry;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::instance::CouldNotRun;
    use aterm_conformance::measure::paint::{
        Capture, Companion, Expect, PaintRow, Reach, Scene, Split, Style,
    };
    use aterm_conformance::measure::paint_probe::measure;
    use aterm_conformance::measure::starvation::{Disposition, Starvation};

    /// The mature run: 29 keys, no spaces (a space ends the mark). The
    /// traverse is per mark, so a ~10-key mark spans 2-3 of 12 hue buckets by
    /// design; at 29 keys the arc completes and the floors have margin.
    const MATURE_RUN: &str = "t,h,e,r,a,i,n,b,o,w,k,i,t,t,y,p,a,i,n,t,s,t,h,e,a,r,c,o,k";
    const OFF_PHRASE: &str = "r,a,i,n,b,o,w,space,o,n";

    /// A trail row: the `rainbow` theme with the pet off, no split, recording on.
    const fn trail(scene: Scene, keys: &'static str, expect: Expect, capture: Capture) -> PaintRow {
        PaintRow {
            scene,
            keys,
            expect,
            capture,
            style: Style::Rainbow,
            companion: None,
            split: None,
            reach: None,
            recording: true,
            rewound: false,
            hosted: false,
        }
    }

    /// `row` with recording off, as a person who turns Rewind off paints it.
    const fn recording_off(row: PaintRow) -> PaintRow {
        PaintRow {
            recording: false,
            ..row
        }
    }

    /// The prompt row's take: the matrix's flagship, recorded and pinned.
    const PROMPT_INK: PaintRow =
        trail(Scene::Prompt, MATURE_RUN, Expect::Ink, Capture::PinnedVideo);
    /// Its matched negative control.
    const PROMPT_OFF: PaintRow = PaintRow {
        style: Style::Off,
        ..trail(
            Scene::Prompt,
            "e,c,h,o,space,h,e,l,l,o",
            Expect::EffectOff,
            Capture::PinnedVideo,
        )
    };
    /// The shipped default cursor style's take.
    const SHIPPED_PET: PaintRow = PaintRow {
        style: Style::Default,
        companion: Some(Companion::Pet),
        ..trail(
            Scene::Prompt,
            MATURE_RUN,
            Expect::Ink,
            Capture::UnpinnedFocused,
        )
    };
    /// Cold output under a companion-free style: the dark control.
    const COLD_DARK: PaintRow = trail(
        Scene::ColdSpinner,
        "",
        Expect::Dark,
        Capture::UnpinnedFocused,
    );

    // Both arms: every row writes its recording (`temporal_recording`) either way, and its take
    // reads it back from `temporal status` before anything is staged. Recording is the shipped
    // default (aterm-gui `DEFAULT_TEMPORAL_RECORDING`); the rows write `true` anyway, so the
    // matrix keeps measuring what ships if that default moves. The off arm is a sample, not the
    // matrix again: the flagship prompt, its effect-off control and the cold dark control run
    // once more with recording off, as a person who turns Rewind off leaves them, because each
    // video take costs a window. Adding a row to the off arm is one `recording_off(…)` entry, and
    // no bound is loosened for either arm: an off-arm row is judged exactly as its recorded twin.
    // A build that records even under `temporal_recording = false` (throwaway, on main
    // c3682d2db0d, 2026-11-13) has every off-arm row refuse on the read-back, never pass.
    const ROWS: &[(&str, PaintRow)] = &[
        ("main_screen_prompt_typing_paints_trail_ink", PROMPT_INK),
        (
            "main_screen_prompt_with_effect_off_has_no_ribbon",
            PROMPT_OFF,
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
            SHIPPED_PET,
        ),
        (
            "the_rainbow_spelling_paints_its_band",
            trail(
                Scene::Prompt,
                MATURE_RUN,
                Expect::Ink,
                Capture::UnpinnedFocused,
            ),
        ),
        (
            "cold_output_paints_literally_nothing_for_a_companion_free_style",
            COLD_DARK,
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
        (
            "the_band_stands_within_its_reach_above_the_typed_row",
            PaintRow {
                reach: Some(Reach::Tall),
                ..trail(
                    Scene::Plane,
                    MATURE_RUN,
                    Expect::Ink,
                    Capture::UnpinnedFocused,
                )
            },
        ),
        // No effect is drawn over a rewound pane's past: the shipped default style (the resident
        // pet) over a recording prompt whose Rewind view is open, the matrix's mature run typed
        // under it. No effect ink, no companion and no ribbon may be on any frame; its control is
        // the shipped-default row above, whose same scene, live, draws the pet and the trail on
        // every take. Proven red 2026-11-13 on main c3682d2db0d's release build: with the
        // effects' rewound guards taken out (a throwaway build), the row failed by name
        // (total_ink=5274, the pet drawn on all 67 frames) where main's build passed (total_ink=0,
        // companion_frames=0, ribbon_claimed=0); the same red held on 5bf00deb90 on 2026-10-27.
        (
            "a_rewound_pane_draws_no_effect_over_its_past",
            PaintRow {
                rewound: true,
                keys: MATURE_RUN,
                expect: Expect::Past,
                companion: None,
                ..SHIPPED_PET
            },
        ),
        // The trail typed on the plane with the glow hosted by the release `aos` path (`aos
        // attach code=shipped`): every ink obligation, and the hosted task must stay attached,
        // keep its log whole and take every typed key. The plane's `sleep` leaves the tty echoing
        // the keys. Its control is the unhosted flagship, the first row above. Proven red
        // 2026-11-13 on main c3682d2db0d's release build: with the typed key withheld from an
        // attached glow's task (a throwaway build), the row failed by name (hosted_trace
        // attached=true keys=0 broken=false) where main's build passed (keys=29); the same red
        // held on 5bf00deb90 on 2026-10-27.
        (
            "a_hosted_glow_paints_the_trail_and_its_task_takes_every_key",
            PaintRow {
                hosted: true,
                ..trail(
                    Scene::Plane,
                    MATURE_RUN,
                    Expect::Ink,
                    Capture::UnpinnedFocused,
                )
            },
        ),
        // The recording-off arm.
        (
            "main_screen_prompt_typing_with_recording_off_paints_trail_ink",
            recording_off(PROMPT_INK),
        ),
        (
            "main_screen_prompt_with_recording_and_effect_off_has_no_ribbon",
            recording_off(PROMPT_OFF),
        ),
        (
            "cold_output_with_recording_off_paints_literally_nothing",
            recording_off(COLD_DARK),
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
            Disposition::Unproved => {
                // The driver kept time and the subject's frames did not: where that was measured
                // it was the GPU they render on, held by another process's long command buffers
                // with the load under its bound (QUIET-BURST).
                let advice = match report.starvation {
                    Starvation::CadenceBlown { .. } => {
                        "the driver kept time, so the subject's frames were held back: take it \
                         with the GPU quiet (the subject's slowest frame on the GPU is \
                         subject_gpu_max_us)"
                    }
                    _ => {
                        "take it on an unloaded machine, from an interactive shell or a \
                         LaunchAgent with ProcessType=App"
                    }
                };
                Outcome::Undecided(format!(
                    "PAINT UNPROVED [{scene}]: the take is not evidence about paint ({}); \
                     {advice}.\n  {report}",
                    report.starvation
                ))
            }
        }
    }

    pub fn run() -> std::process::ExitCode {
        let rows = match selected(std::env::args().skip(1)) {
            Ok(rows) => rows,
            Err(why) => {
                eprintln!("measure paint: {why}");
                return std::process::ExitCode::from(2);
            }
        };
        let rig = Rig::release(&["ATERM_PAINT_BIN"], aterm_tempfile::bin_exe("fake-tui"));
        support::run_with_telemetry(
            "paint",
            &rows,
            |row| take(&rig, row),
            Some(support::Telemetry {
                cpu: super::telemetry::cpu,
                events: super::telemetry::events,
            }),
        )
    }

    /// The rows each `--row <name>` among the arguments names, in matrix order, or every row
    /// when none is named; an unknown name is a usage error. `verify measure` names none.
    fn selected(
        args: impl Iterator<Item = String>,
    ) -> Result<Vec<(&'static str, PaintRow)>, String> {
        let mut named = Vec::new();
        let mut args = args;
        while let Some(arg) = args.next() {
            if arg == "--row" {
                let name = args.next().unwrap_or_default();
                if !ROWS.iter().any(|(n, _)| *n == name) {
                    return Err(format!("--row names no row of the matrix: `{name}`"));
                }
                named.push(name);
            }
        }
        Ok(ROWS
            .iter()
            .filter(|(n, _)| named.is_empty() || named.iter().any(|m| m == n))
            .copied()
            .collect())
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
