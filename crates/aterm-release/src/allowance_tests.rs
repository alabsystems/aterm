// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The engine's line of 2026-11-13 09:57 PT, as step 7's log held it.
const ENGINE: &str = "FAIL: cannot query publication engine origin/main: pub GitHub transport: \
    Agh exited 11: agh: rate_limited exit=11 verb=api repo=fixture-dev/publication as=dev \
    sub=pre_refused reset=1794594793 side_effect=none why=\"background core read leaves 1000 \
    requests for the release cutter until 1794594793\"";

#[test]
fn a_reset_is_read_only_from_aghs_rate_limited_refusal() {
    assert_eq!(reset_in([ENGINE]), Some(1_794_594_793));
    assert_eq!(
        reset_in([
            "could not read o/r v1 from GitHub's API (spent (agh: rate_limited exit=11 reset=1794594793))"
        ]),
        Some(1_794_594_793),
        "this tool's own client"
    );
    assert_eq!(reset_in([line(7).as_str()]), Some(7));
    assert_eq!(
        reset_in(["noise", ENGINE, &line(1_794_594_800)]),
        Some(1_794_594_800),
        "the latest of several"
    );
    // Negative controls: another refusal of agh's that carries a reset-shaped word, a
    // rate-limited one that names none, a reset no agh line holds, and one that is no number.
    for other in [
        "agh: forbidden exit=6 verb=api reset=1794594793 side_effect=none",
        "agh: rate_limited exit=11 verb=api retry_after=60 side_effect=none",
        "the allowance returns at reset=1794594793",
        "agh: rate_limited exit=11 reset=soon",
        "",
    ] {
        assert_eq!(reset_in([other]), None, "{other}");
    }
}

#[test]
fn a_run_waits_only_for_a_reset_within_githubs_hour() {
    let now = 1_794_592_628;
    assert_eq!(
        again_at(now + 2165, now),
        Some(now + 2165 + PAST_RESET_SECS)
    );
    assert_eq!(
        again_at(now + LONGEST_WAIT_SECS, now),
        Some(now + LONGEST_WAIT_SECS + PAST_RESET_SECS)
    );
    // A reset that just passed is run again at once, past its slack.
    assert_eq!(again_at(now - 5, now), Some(now - 5 + PAST_RESET_SECS));
    // Negative controls: further ahead than any window, and an old refusal's.
    assert_eq!(again_at(now + LONGEST_WAIT_SECS + 1, now), None);
    assert_eq!(again_at(now - 61, now), None);
}
