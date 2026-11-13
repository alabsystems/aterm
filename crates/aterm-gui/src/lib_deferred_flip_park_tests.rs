// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use atpkg::quiet::{Due, DueFlip, RECHECK};

#[test]
fn a_held_flip_wakes_on_two_quiet_looks_or_at_the_ceiling() {
    let layout = atpkg::store::Layout {
        prefix: std::path::PathBuf::from("/nonexistent-prefix"),
    };
    let look = |why, checked| move |_: &atpkg::store::Layout| Some(DueFlip { why, checked });
    let mut watch = BumpWatch::default();
    let t0 = Instant::now();
    assert!(
        !deferred_flip_due(&layout, &mut watch, t0, look(Due::Quiet, 1)),
        "one quiet look"
    );
    assert!(
        !deferred_flip_due(&layout, &mut watch, t0 + Duration::from_secs(5), |_| {
            panic!("no second look inside the minute")
        }),
        "not looked at again inside the minute"
    );
    assert!(
        deferred_flip_due(&layout, &mut watch, t0 + RECHECK, look(Due::Quiet, 1)),
        "the second quiet look in a row wakes the pass"
    );
    for n in 2..5 {
        assert!(
            !deferred_flip_due(&layout, &mut watch, t0 + n * RECHECK, look(Due::Quiet, 1)),
            "look {n}"
        );
    }
    assert!(!deferred_flip_due(
        &layout,
        &mut watch,
        t0 + 5 * RECHECK,
        look(Due::Ceiling, 1)
    ));
    let t1 = t0 + 6 * RECHECK;
    assert!(!deferred_flip_due(
        &layout,
        &mut watch,
        t1,
        look(Due::Quiet, 2)
    ));
    assert!(!deferred_flip_due(
        &layout,
        &mut watch,
        t1 + RECHECK,
        |_| None
    ));
    assert!(
        !deferred_flip_due(&layout, &mut watch, t1 + 2 * RECHECK, look(Due::Quiet, 2)),
        "a busy look between two quiet ones starts the count over"
    );
    assert!(deferred_flip_due(
        &layout,
        &mut watch,
        t1 + 3 * RECHECK,
        look(Due::Quiet, 2)
    ));
    let mut fresh = BumpWatch::default();
    assert!(
        deferred_flip_due(&layout, &mut fresh, t0, look(Due::Ceiling, 3)),
        "the ceiling wakes at once"
    );
    assert_eq!(ParkEnd::FlipDue.pass_why(PassWhy::Walk), PassWhy::FlipDue);
}
