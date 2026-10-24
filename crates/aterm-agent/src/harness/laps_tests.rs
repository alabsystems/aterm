// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the timed runs' marks.

use super::*;

/// A RUN KEEPS ITS OWN MARKS, IN ORDER: a mark inside a run is kept in it, a nested run keeps its
/// own and leaves the outer's alone, and a mark outside any run keeps nothing. Negative control: the
/// mark taken before the run opened is in none of them.
#[test]
fn a_run_keeps_its_own_marks_in_order() {
    lap("before");
    let ((inner, ()), outer) = within(|| {
        lap("connect");
        let inner = within(|| {
            lap("table");
            lap("table");
        });
        lap("roster");
        (inner, ())
    });
    let names = |laps: &Laps| laps.marks().iter().map(|(n, _)| *n).collect::<Vec<_>>();
    assert_eq!(names(&outer), ["connect", "roster"]);
    assert_eq!(names(&inner.1), ["table", "table"]);
    assert!(outer.took("before").is_none(), "a mark outside the run");
    assert!(
        outer.took("table").is_none(),
        "the nested run's marks are its own"
    );
    assert!(inner.1.took("table").is_some());
    let (_, empty) = within(|| ());
    assert!(empty.marks().is_empty());
    lap("after");
    assert!(OPEN.with(|o| o.borrow().is_empty()), "every run closed");
}

/// A RUN'S TIME IS SAID WHERE IT WENT: the mark after a slow call carries that call's time, the
/// total covers it, and an ended run's total stops growing.
#[test]
fn a_runs_time_is_said_where_it_went() {
    let ((), laps) = within(|| {
        lap("quick");
        std::thread::sleep(Duration::from_millis(30));
        lap("slow");
    });
    let slow = laps.took("slow").expect("marked");
    assert!(slow >= Duration::from_millis(30), "{laps}");
    assert!(laps.total() >= slow + laps.took("quick").expect("marked"));
    let total = laps.total();
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(laps.total(), total, "an ended run");
    let said = laps.to_string();
    assert!(said.starts_with("quick "), "{said}");
    assert!(said.contains(", slow "), "{said}");
    assert!(said.ends_with(" ms"), "{said}");
}

/// A RUN THAT UNWINDS IS CLOSED: a panic inside it leaves no run open on the thread.
#[test]
fn a_run_that_unwinds_is_closed() {
    let caught = std::panic::catch_unwind(|| {
        within(|| {
            lap("read");
            panic!("a read that panics");
        })
    });
    assert!(caught.is_err());
    assert!(OPEN.with(|o| o.borrow().is_empty()));
}
