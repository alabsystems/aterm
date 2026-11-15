// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The go-ahead reader (`alab_policy::go_ahead`) over a turn as this crate's screen readers draw
//! it: what the reader parks on is the turn's words those readers hand it.

use alab_policy::go_ahead::parked;

#[test]
fn claudes_own_task_notice_leaves_a_later_park_answered() {
    use crate::supervise::prompt::fixtures::{composer, rows};
    let tail = "NEEDS YOU: say \"rerun the checks\" so I can rebuild and push my branch to main.";
    let mut screen = rows(&[
        "✻ Worked for 2m 10s",
        "",
        "⏺ Background command \"Build and tag the release\" completed (exit code 0)",
        "",
        &format!("⏺ {tail}"),
        "",
    ]);
    screen.extend(composer("  ? for shortcuts"));
    let earlier = aterm_phase::turn_said_before(&screen);
    let last = aterm_phase::said_tail(&screen).expect("the turn's last reply");
    assert_eq!(earlier, None);
    assert_eq!(
        parked(earlier.as_deref().unwrap_or(""), &last)
            .as_ref()
            .and_then(|p| p.token.as_deref()),
        Some("rerun the checks")
    );

    let mut planned = rows(&[
        "✻ Worked for 2m 10s",
        "",
        "⏺ Next I'll build and tag the release.",
        "",
        &format!("⏺ {tail}"),
        "",
    ]);
    planned.extend(composer("  ? for shortcuts"));
    let earlier = aterm_phase::turn_said_before(&planned);
    let last = aterm_phase::said_tail(&planned).expect("the turn's last reply");
    assert_eq!(
        earlier.as_deref(),
        Some("Next I'll build and tag the release.")
    );
    assert!(
        parked(earlier.as_deref().unwrap_or(""), &last).is_none(),
        "an outward plan in the agent's own earlier prose still blocks the go-ahead"
    );
}
