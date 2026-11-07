// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's idle-wake acceptance matrix, run explicitly by `verify measure`.

#[cfg(unix)]
#[path = "support/quiet.rs"]
mod quiet;
#[cfg(unix)]
mod support;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use std::time::Duration;

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::instance::CouldNotRun;
    use aterm_conformance::measure::spin::{
        EFFECT_LANE, LIVE_LANE, NEEDLE_LANE, SpinRow, SpinScene, measure,
    };
    use aterm_conformance::measure::starvation::ShapeVerdict;

    const MAX_ARMS: u64 = 100;
    const RESTING_MAX_WAKES: u64 = 60;
    const RESTING_MAX_FRAMES: u64 = 20;

    // `max_wakes` bounds every loop wake in the window (`event_wakes`), not only its timers: a loop
    // that wakes itself as it parks takes wait-cancelled wakes and turns, which neither
    // `past_deadline_arms` nor `timer_wakes` sees. Each row's comment gives what three takes or
    // more measured, on debug builds of 71b865e7c4 unless it names another build (the rig takes any
    // aterm executable), and its bound's headroom over that. Proven red
    // 2026-10-25 on the headless rows by a debug build of 71b865e7c4 whose display-free loop
    // signalled its own wake source as it parked (a mutant, reverted): the three rows measured
    // 682,944, 507,404 and 833,204 wakes with at most 1 arm and 2 timer wakes, which the arm and
    // timer bounds alone passed.
    const ROWS: &[(&str, SpinRow)] = &[
        // The repainter writes every 166.7 ms, about 66 output wakes in the window: 75 and 76
        // wakes measured, bounded at twice that.
        (
            "an_idle_claude_shaped_client_banks_no_past_deadline_arms",
            SpinRow {
                scene: SpinScene::ClaudeShaped,
                window: Duration::from_secs(10),
                max_arms: MAX_ARMS,
                max_timer_wakes: Some(12),
                max_owner_fires: &[],
                needle_owed: false,
                max_wakes: 150,
                max_frames: None,
            },
        ),
        // No timer fired and no event came: 0 wakes measured in each take, bounded at one a second.
        (
            "a_bare_idle_instance_banks_no_past_deadline_arms",
            SpinRow {
                scene: SpinScene::Idle,
                window: Duration::from_secs(6),
                max_arms: MAX_ARMS,
                max_timer_wakes: None,
                max_owner_fires: &[],
                needle_owed: false,
                max_wakes: 6,
                max_frames: None,
            },
        ),
        // A recording prompt frozen in an open Rewind view (design §14.5 R0 item 7). Proven red
        // 2026-10-24: a release build of c4d3f61702 measured timer_wakes=0 frames=0 over 10 s, and the
        // same build with a 100 ms refresh deadline armed while any view is open measured
        // timer_wakes=99 and failed this row. 1 wake measured (the view's own Rewind wake), bounded
        // as its timers are.
        (
            "an_idle_open_rewind_view_arms_no_timer",
            SpinRow {
                scene: SpinScene::Rewound,
                window: Duration::from_secs(10),
                max_arms: MAX_ARMS,
                max_timer_wakes: Some(12),
                max_owner_fires: &[],
                needle_owed: false,
                max_wakes: 12,
                max_frames: None,
            },
        ),
        // A bare prompt in a window: AppKit's loop, which no headless row runs. Under
        // `motion = "reduced"`, so neither a focused window's Deck live edge (30 timer wakes in
        // 10 s, a frame each) nor its awake pet (35 to 37 cursor-effect timer wakes in a fresh
        // window's first 10 s at the shipped motion, of 79 to 81 timer wakes and 71 frames in all,
        // on a release build of 921fbd893d) makes the bound hang on a focus the rig does not
        // decide: the cursor blink (19 timer wakes), the vitals (10 timer, 10 event) and each
        // frame's surface-ready wake, 72 and 73 wakes measured (74, 75 and 98 on that release
        // build), bounded at 200. Proven red 2026-10-25: a debug build of 71b865e7c4 with
        // 5db5aa8c37^'s event_loop.rs, whose proxy clone (taken by `aos_watch` in every
        // `about_to_wait`) made a run loop source and woke the loop, measured 456,021 wakes
        // (455,992 wait-cancelled) with 25 past-deadline arms and 29 timer wakes, and the three
        // headless rows passed on it. Red again 2026-10-26 through the scene code it shares with
        // the pet row below: the release build of 921fbd893d with a 5 ms cursor blink (a mutant,
        // reverted) measured 3,755 wakes and 1,805 blink fires.
        (
            "an_idle_window_parks_until_its_next_deadline",
            SpinRow {
                scene: SpinScene::Windowed,
                window: Duration::from_secs(10),
                max_arms: MAX_ARMS,
                max_timer_wakes: None,
                max_owner_fires: &[],
                needle_owed: false,
                max_wakes: 200,
                max_frames: None,
            },
        ),
        // The window above at full motion (what the shipped `auto` resolves to on a Mac whose
        // Reduce Motion is off) under the shipped cursor style, the rainbow kitty pet, measured
        // from 40 s after its prompt went quiet. A pet in deep sleep takes no frame and offers no
        // deadline, so the cursor-effect lane is bounded at 0. A window without the focus asks its
        // pet for none either, so a take that does not open and end focused, at full motion with
        // the pet past its breath window (`pet_deep`), is a refusal, never a pass (`PET_READY`).
        // A window that loses the focus and regains it fades its pet back in over 0.30 s, about 18
        // frames at a 120 Hz panel's effect interval: the best fit, by a reading of the code, for
        // one take of main's dev bundle on 2026-10-28 that fired the lane 18 times with the pet
        // asleep at both edges (it recorded no window events), so a take whose window took any
        // event other than a redraw (`os_window_events`, `os_events_unidle`), or whose pet still
        // asks for frames as the window opens (a fade, a pointer's warmth), is a refusal too, and
        // the owners' fires count from the metrics read after that check. A release build of
        // 921fbd893d
        // measured, focused with the pet asleep (`pet_sleep_0`) in each of three takes, 0
        // cursor-effect fires, 42 timer wakes (the Deck's live edge 30, the vitals 10, 2 devtools
        // hand-overs), 40 frames and 92 wakes, bounded at 200 as the row above. Proven red
        // 2026-10-26: that build with a pet that asks for frames whenever it sleeps (a mutant,
        // reverted) measured 570 cursor-effect fires, 607 timer wakes and 698 wakes, focused and
        // asleep, and the four rows above passed on it. The window's Folded deck draws the dial,
        // whose needle sweeps on its deck clock (`deck_clock`) after a reading that moves its
        // tip a device pixel or more (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §4.3). The
        // dial reads the machine's own CPU, so how often it sweeps is the Mac's load: on main's
        // dev bundle of 2026-10-28, three takes asked 46, 19 and 34 needle frames at loads of
        // 8.4, 6.2 and 3.7, and a fixed bound of 20 failed two of them. The app counts the frames
        // each reading's episode owes the windows that animate the dial, on each window's own
        // shortest deck frame interval (`needle_frames_owed`), and what the episode in flight as
        // the window opens has left to fire (`needle_episode_left`), so the row bounds the needle's
        // fires by those, each counted between the window's opening and closing metrics: a needle
        // that fires more than its readings owed fails, whatever the
        // Mac's load. The needle's own wakes, two a fire, are left out of the 200, which the
        // rest of the window kept to 91 or 92 in each of those takes. Since the window's rest
        // (`idle_rest_secs`, ATERM-KI-1027-4) the row's head turns it off (`idle_rest_secs = 0`),
        // so it stays the awake deck's row; a take whose window rests anyway is a refusal.
        // Those takes predate the deck clock (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md`
        // S2): a Mac with a display link fires `deck_clock` only to resume its paused link a few
        // periods before a frame due, and the link's vsyncs while it runs are window events,
        // which the 200-wake bound counts. This row has not been measured at the desk since.
        (
            "a_pet_in_deep_sleep_fires_no_effect_wake",
            SpinRow {
                scene: SpinScene::PetAsleep,
                window: Duration::from_secs(10),
                max_arms: MAX_ARMS,
                max_timer_wakes: None,
                max_owner_fires: &[(EFFECT_LANE, 0)],
                needle_owed: true,
                max_wakes: 200,
                max_frames: None,
            },
        ),
        // The row above with the window's rest on (`idle_rest_secs = 30`, ATERM-KI-1027-4; the
        // rest is off unless set), measured over 20 s from 40 s after its prompt went quiet, past
        // the rest and the pet's deep sleep. A resting window's deck draws as an unfocused one's:
        // no live-edge frame, no needle sweep, a reading every 5 s and a frame for each, so the
        // live-edge, needle and cursor-effect lanes are bounded at 0, and its frames and wakes
        // too, which no lane above bounds. A take whose window does not read `resting=true` as it
        // opens and as it ends is a refusal (`rest_unready`). Two release builds of this branch
        // (2026-10-29 and 2026-10-30, the rest then on by default) measured 4 frames, 6 timer
        // wakes and 18 wakes (the vitals 4, the crash journal 2) in each, and a debug build of
        // 569781e613 (off by default, on in this head) 4, 5 and 17, bounded at 20 frames and 60
        // wakes. Proven red by the row above on the same builds: the window with its rest off
        // drew 69, 60 and 47 frames in half the time, with 30 live-edge fires each and 29, 20 and
        // 7 needle fires, which the lane, frame and wake bounds here each fail.
        (
            "a_resting_window_takes_no_live_frame",
            SpinRow {
                scene: SpinScene::Resting,
                window: Duration::from_secs(20),
                max_arms: MAX_ARMS,
                max_timer_wakes: None,
                max_owner_fires: &[(EFFECT_LANE, 0), (NEEDLE_LANE, 0), (LIVE_LANE, 0)],
                needle_owed: false,
                max_wakes: RESTING_MAX_WAKES,
                max_frames: Some(RESTING_MAX_FRAMES),
            },
        ),
    ];

    fn take(rig: &Rig, row: &SpinRow) -> Outcome {
        let report = match measure(rig, row) {
            Ok(report) => report,
            Err(CouldNotRun::Unanswered(why)) => return Outcome::Unanswered(why),
            Err(why) => {
                return Outcome::CouldNotRun(format!(
                    "SPIN COULD NOT RUN [{:?}]: the probe decided nothing: {why}",
                    row.scene
                ));
            }
        };
        if report.verdict == ShapeVerdict::Pass {
            Outcome::Pass(report.to_string())
        } else {
            Outcome::Fail(format!(
                "SPIN CONFORMANCE FAILED [{:?}]: an idle instance armed wakes past its own \
                 observation gate, or woke for timers or at all far past its quiet rate.\n  \
                 {report}",
                row.scene
            ))
        }
    }

    /// Set by `verify --no-windows`: the rows that open a window are left out (verify names
    /// their skip).
    const NO_WINDOWS: &str = "ATERM_MEASURE_NO_WINDOWS";

    pub fn run() -> std::process::ExitCode {
        let rig = Rig::release(
            &["ATERM_SPIN_BIN", "ATERM_PAINT_BIN"],
            aterm_tempfile::bin_exe("fake-tui"),
        );
        let windows = std::env::var_os(NO_WINDOWS).is_none();
        let rows: Vec<(&str, SpinRow)> = ROWS
            .iter()
            .copied()
            .filter(|(name, row)| {
                let kept = windows || !row.scene.windowed();
                if !kept {
                    eprintln!("measure spin: {name} left out ({NO_WINDOWS}: it opens a window)");
                }
                kept
            })
            .collect();
        support::run_classified(
            "spin",
            &rows,
            |row| support::Kind::Count {
                windowed: row.scene.windowed(),
            },
            |row| take(&rig, row),
        )
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        quiet::run("spin", measuring::run)
    }
    #[cfg(not(unix))]
    {
        eprintln!("measure: COULD NOT RUN — spin requires a Unix host");
        std::process::ExitCode::from(3)
    }
}
