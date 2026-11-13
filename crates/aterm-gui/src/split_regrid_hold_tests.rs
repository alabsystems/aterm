// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const C: RelayCause = RelayCause::Collapse;
const O: RelayCause = RelayCause::Other;

#[test]
fn a_collapse_take_back_inside_the_quiet_is_withheld_and_owes_one_wake() {
    let mut holds = SplitRegridHolds::default();
    let t0 = Instant::now();
    holds.arm(7, (24, 80), t0);
    assert_eq!(holds.deadline(), None, "an unconsulted hold owes no wake");
    assert_eq!(holds.judge(7, (11, 80), C, t0), HoldVerdict::Apply);
    assert_eq!(holds.get(7), None);

    holds.arm(7, (24, 80), t0);
    assert_eq!(holds.judge(7, (24, 80), C, t0), HoldVerdict::Withhold);
    assert_eq!(holds.judge(7, (24, 80), O, t0), HoldVerdict::Withhold);
    assert_eq!(holds.deadline(), Some(t0 + SPLIT_REGRID_QUIET));
    assert!(holds.take_due(t0).is_empty(), "not due before the quiet");
    assert_eq!(holds.take_due(t0 + SPLIT_REGRID_QUIET), vec![7]);
    assert_eq!(holds.deadline(), None);
}

#[test]
fn no_collapse_no_hold_even_on_the_pre_split_grid() {
    let mut holds = SplitRegridHolds::default();
    let t0 = Instant::now();
    holds.arm(7, (24, 80), t0);
    assert_eq!(holds.judge(7, (24, 80), O, t0), HoldVerdict::Apply);
    assert_eq!(holds.get(7), None, "a non-collapse pass ends the hold");
    assert_eq!(holds.deadline(), None);
}

#[test]
fn any_other_target_or_an_ended_quiet_applies_at_once() {
    let mut holds = SplitRegridHolds::default();
    let t0 = Instant::now();
    holds.arm(7, (24, 80), t0);
    assert_eq!(holds.judge(7, (30, 100), C, t0), HoldVerdict::Apply);
    assert_eq!(holds.get(7), None, "another target ends the hold");

    holds.arm(7, (24, 80), t0);
    assert_eq!(holds.judge(7, (24, 80), C, t0), HoldVerdict::Withhold);
    assert_eq!(
        holds.judge(7, (30, 100), O, t0),
        HoldVerdict::Apply,
        "a withheld take-back yields to any other target"
    );
    assert_eq!(holds.get(7), None);

    holds.arm(7, (24, 80), t0);
    assert_eq!(
        holds.judge(7, (24, 80), C, t0 + SPLIT_REGRID_QUIET),
        HoldVerdict::Apply
    );
    assert_eq!(holds.get(7), None);

    holds.arm(7, (24, 80), t0);
    assert_eq!(holds.judge(9, (24, 80), C, t0), HoldVerdict::Apply);
    holds.forget(7);
    assert_eq!(holds.get(7), None);
    assert!(holds.take_due(t0 + SPLIT_REGRID_QUIET).is_empty());
}
