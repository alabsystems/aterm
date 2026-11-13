// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{TITLEBAR_BAND_SANITY_CAP_PTS, accept_titlebar_band_pts, titlebar_band_decision};

#[test]
fn a_titlebar_band_sample_is_accepted_only_when_windowed_decorated_and_sane() {
    let inflated = TITLEBAR_BAND_SANITY_CAP_PTS + 1.0;
    for (label, sample, fullscreen, decorated, prev, expected) in [
        ("fullscreen 38", 38.0, true, true, 55.0, 0.0),
        ("fullscreen 0", 0.0, true, true, 55.0, 0.0),
        ("fullscreen 900", 900.0, true, true, 55.0, 0.0),
        ("windowed 0", 0.0, false, true, 55.0, 55.0),
        ("windowed -3", -3.0, false, true, 55.0, 55.0),
        ("windowed past the cap", inflated, false, true, 55.0, 55.0),
        ("windowed 700", 700.0, false, true, 55.0, 55.0),
        ("windowed 38", 38.0, false, true, 55.0, 38.0),
        ("windowed 62, no prev", 62.0, false, true, 0.0, 62.0),
        ("undecorated 0", 0.0, false, false, 55.0, 0.0),
        ("undecorated -1", -1.0, false, false, 55.0, 0.0),
    ] {
        assert_eq!(
            accept_titlebar_band_pts(sample, fullscreen, decorated, prev),
            expected,
            "{label}"
        );
    }
}

#[test]
fn band_memory_survives_a_fullscreen_stay_and_restores_on_exit_artifact() {
    let (applied, memory) = titlebar_band_decision(55.0, false, true, 0.0);
    assert_eq!((applied, memory), (55.0, 55.0));

    let (applied, memory) = titlebar_band_decision(700.0, true, true, memory);
    assert_eq!((applied, memory), (0.0, 55.0));

    let (applied, memory) = titlebar_band_decision(0.0, true, true, memory);
    assert_eq!((applied, memory), (0.0, 55.0));

    let (applied, memory) = titlebar_band_decision(0.0, false, true, memory);
    assert_eq!((applied, memory), (55.0, 55.0));

    assert_eq!(
        titlebar_band_decision(0.0, false, false, 55.0),
        (0.0, 55.0),
        "undecorated applies 0 and keeps the memory slot"
    );
}
