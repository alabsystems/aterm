// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_hidden_windows_clocks_plan_nothing_unless_it_is_recorded() {
    assert!(clocks_have_glass(true, false, false), "a visible window");
    assert!(
        !clocks_have_glass(true, true, false),
        "a hidden one plans none"
    );
    assert!(clocks_have_glass(true, true, true), "unless it is recorded");
    for (occluded, recorded) in [(false, false), (true, false), (true, true)] {
        assert!(
            !clocks_have_glass(false, occluded, recorded),
            "no OS window, no glass"
        );
    }
}

#[test]
fn an_off_glass_window_parks_and_a_covered_one_paces_unless_its_present_is_awaited() {
    let t0 = Instant::now();
    let soon = t0 + COVERED_FRAME_PERIOD - Duration::from_millis(1);
    let later = t0 + COVERED_FRAME_PERIOD;
    assert_eq!(Sight::of(false, false), Sight::Visible);
    assert_eq!(Sight::of(false, true), Sight::Visible, "visible again wins");
    assert_eq!(Sight::of(true, false), Sight::Covered);
    assert_eq!(Sight::of(true, true), Sight::OffGlass);
    for paced in [None, Some(t0)] {
        for awaited in [false, true] {
            assert_eq!(
                hidden_frame(Sight::Visible, awaited, paced, soon),
                HiddenFrame::Draws
            );
        }
        assert_eq!(
            hidden_frame(Sight::Covered, true, paced, soon),
            HiddenFrame::Draws,
            "a capture's frame draws at once"
        );
        assert_eq!(
            hidden_frame(Sight::OffGlass, true, paced, soon),
            HiddenFrame::Draws
        );
        assert_eq!(
            hidden_frame(Sight::OffGlass, false, paced, later),
            HiddenFrame::Parks,
            "a minimised window never draws on its own"
        );
    }
    // A covered window draws its first frame of a spell at once, then at most one a period.
    assert_eq!(
        hidden_frame(Sight::Covered, false, None, t0),
        HiddenFrame::Paced
    );
    assert_eq!(
        hidden_frame(Sight::Covered, false, Some(t0), soon),
        HiddenFrame::Parks
    );
    assert_eq!(
        hidden_frame(Sight::Covered, false, Some(t0), later),
        HiddenFrame::Paced
    );
}
