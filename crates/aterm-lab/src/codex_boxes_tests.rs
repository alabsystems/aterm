// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

/// A box's mark, its step, the mark that ends it and the leave: met, closed, read.
#[test]
fn a_blocked_mark_is_met_by_its_standing_claim_step() {
    let s = box_steps(&lines(
        "10 open t1 turn par=r1 src=engine conf=claim t=1\n\
         11 mark r1 program state=blocked ask=permission src=engine conf=claim t=2\n\
         12 open p1 approval par=t1 box=other src=engine conf=claim t=2\n\
         13 mark r1 program state=working src=engine conf=claim t=3\n\
         14 close p1 approval out=passed dur_ms=1 by=ctl src=engine conf=claim t=3\n\
         15 mark r1 program state=idle src=engine conf=claim t=4\n\
         16 close t1 turn out=done dur_ms=3 steps=1 appr=1 src=engine conf=claim t=4",
    ));
    assert_eq!(s.unmet, Vec::<String>::new());
    assert_eq!(s.open(), Vec::<&str>::new());
    assert_eq!(
        s.steps.iter().map(ClaimStep::said).collect::<Vec<_>>(),
        ["p1 approval out=passed by=ctl"]
    );
    assert_eq!(s.steps[0].boxed.as_deref(), Some("other"));
    assert_eq!(s.counts, ["t1 appr=1 qs=0"]);
    assert_eq!(s.tally(false), "passed:1");
}

/// NEGATIVE CONTROL: a blocked mark no step answered, a step of the other kind, and a
/// screen's step (`src=screen`) are each unmet.
#[test]
fn a_blocked_mark_without_its_claim_step_is_unmet() {
    let s = box_steps(&lines(
        "11 mark r1 program state=blocked ask=permission src=engine conf=claim t=2\n\
         12 mark r1 program state=working src=engine conf=claim t=3\n\
         13 mark r1 program state=blocked ask=question src=engine conf=claim t=4\n\
         14 open p1 approval par=r1 box=other src=engine conf=claim t=4\n\
         15 mark r1 program state=working src=engine conf=claim t=5\n\
         16 close p1 approval out=passed dur_ms=1 src=engine conf=claim t=5\n\
         17 mark r1 program state=blocked ask=permission src=engine conf=claim t=6\n\
         18 open p2 approval par=r1 box=edit src=screen conf=strong t=6\n\
         19 mark r1 program state=working src=engine conf=claim t=7",
    ));
    assert_eq!(
        s.unmet,
        ["11 ask=permission", "13 ask=question", "17 ask=permission"]
    );
    assert_eq!(
        s.open(),
        Vec::<&str>::new(),
        "a screen's step is no claim step"
    );
}

/// The sweep feeds the turn's end edge before the mark that shows the rest: the turn's close
/// ends the box's step before the next mark, and the box is met.
#[test]
fn a_turn_close_before_the_next_mark_ends_its_box() {
    let s = box_steps(&lines(
        "10 open t1 turn par=r1 src=engine conf=claim t=1\n\
         11 mark r1 program state=working src=engine conf=claim t=1\n\
         12 mark r1 program state=blocked ask=permission src=engine conf=claim t=2\n\
         13 open p1 approval par=t1 box=other src=engine conf=claim t=2\n\
         14 close p1 approval out=passed dur_ms=1 by=ctl src=engine conf=claim t=3\n\
         15 close t1 turn out=done dur_ms=2 steps=1 appr=1 src=engine conf=claim t=3\n\
         16 mark r1 program state=done src=engine conf=claim t=4",
    ));
    assert_eq!(s.unmet, Vec::<String>::new());
    assert_eq!(s.open(), Vec::<&str>::new());
}

/// NEGATIVE CONTROL: a step that closed before the next mark with no turn close ending it
/// (another turn's close, or none) does not meet the mark.
#[test]
fn a_step_closed_before_the_next_mark_by_nothing_is_unmet() {
    let s = box_steps(&lines(
        "10 open t1 turn par=r1 src=engine conf=claim t=1\n\
         11 mark r1 program state=blocked ask=permission src=engine conf=claim t=2\n\
         12 open p1 approval par=t1 box=other src=engine conf=claim t=2\n\
         13 close p1 approval out=passed dur_ms=1 src=engine conf=claim t=3\n\
         14 close t9 turn out=done dur_ms=2 src=engine conf=claim t=3\n\
         15 mark r1 program state=working src=engine conf=claim t=4",
    ));
    assert_eq!(s.unmet, ["11 ask=permission"]);
}

/// Two boxes back to back, the second of the other kind: the first step stands until the
/// mark that ends it, the second opens under it; the last mark's step stands past the end.
#[test]
fn back_to_back_boxes_each_have_their_step() {
    let s = box_steps(&lines(
        "11 mark r1 program state=blocked ask=permission src=engine conf=claim t=2\n\
         12 open p1 approval par=r1 box=other src=engine conf=claim t=2\n\
         13 mark r1 program state=blocked ask=question src=engine conf=claim t=3\n\
         14 close p1 approval out=passed dur_ms=1 src=engine conf=claim t=3\n\
         15 open p2 question par=r1 src=engine conf=claim t=3",
    ));
    assert_eq!(s.unmet, Vec::<String>::new());
    assert_eq!(s.open(), ["p2"]);
    assert_eq!(
        s.steps.iter().map(ClaimStep::said).collect::<Vec<_>>(),
        ["p1 approval out=passed by=-", "p2 question open"]
    );
}

/// A step under another run never meets a run's mark.
#[test]
fn a_step_meets_only_its_own_runs_marks() {
    let s = box_steps(&lines(
        "10 open t1 turn par=r2 src=engine conf=claim t=1\n\
         11 mark r1 program state=blocked ask=permission src=engine conf=claim t=2\n\
         12 open p1 approval par=t1 box=other src=engine conf=claim t=2\n\
         13 mark r1 program state=working src=engine conf=claim t=3",
    ));
    assert_eq!(s.unmet, ["11 ask=permission"]);
}
