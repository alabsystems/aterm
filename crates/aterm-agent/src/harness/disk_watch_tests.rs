// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn reported_session_directories_are_actual_or_unknown() {
    assert_eq!(
        reported_cwd("OK /tmp/a%20b"),
        Some(PathBuf::from("/tmp/a b"))
    );
    for bad in [
        "OK ",
        "OK relative",
        "ERR busy",
        "OK /tmp/%ZZ",
        "OK /tmp/%FF",
        "OK /tmp/%00",
    ] {
        assert_eq!(reported_cwd(bad), None, "{bad}");
    }
}

/// THE ENTRY-BOUND STEP IS ADMITTED ONCE AN HOUR WHILE REMOVAL IS ON. A look is admitted where
/// none was before, refused a second short of the hour and admitted at the hour; with
/// `auto_free_gib = 0` it is refused and stamps nothing, so removal turned on again runs at the
/// next look. NEGATIVE CONTROLS: without the hourly rule the look at 3,599 s is admitted;
/// without the removal switch the first look is; and a refusal that stamped would leave `last`
/// at the refused look.
#[test]
fn the_entry_bound_step_is_admitted_hourly_while_removal_is_on() {
    let t = 1_790_000_000;
    let mut bound = BoundWatch::new();
    assert!(!bound.admits(0, t), "removal is off");
    assert_eq!(bound.last, None, "a refused look stamps nothing");
    assert!(bound.admits(1, t));
    assert!(!bound.admits(1, t + 3_599));
    assert!(bound.admits(1, t + 3_600));
    assert!(!bound.admits(0, t + 7_200), "removal turned off");
    assert_eq!(bound.last, Some(t + 3_600));
    assert!(bound.admits(1, t + 7_200));
}
