// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::drive_reflow_job;
use aterm_core::grid::ReflowStep;
use aterm_core::terminal::Terminal;
use std::sync::atomic::AtomicBool;

fn term_with_history(rows: u16, cols: u16, lines: usize) -> Terminal {
    let sb = aterm_scrollback::Scrollback::new(64, 512, 8_000_000);
    let mut t = Terminal::with_scrollback(rows, cols, 8, sb);
    let fill = "x".repeat((cols as usize).saturating_sub(8));
    let mut buf = Vec::new();
    for i in 0..lines {
        buf.extend_from_slice(format!("L{i}-{fill}\r\n").as_bytes());
    }
    t.process(&buf);
    t
}

#[test]
fn cancelled_mid_flight_job_is_dropped_at_the_next_step() {
    let mut t = term_with_history(24, 80, 800);
    let before = t.grid().scrollback_lines();
    assert!(before > 100, "precondition: deep tiered history ({before})");

    let job = t
        .resize_offloading_scrollback(24, 40)
        .expect("width change with tiered history detaches a job");
    let ReflowStep::InProgress(half) = job.reflow_step(5) else {
        panic!("an 800-line history cannot complete in one 5-line step");
    };
    let cancel = AtomicBool::new(true); // Session::drop raced in
    assert!(
        drive_reflow_job(half, &cancel, 5).is_none(),
        "the worker loop must observe the cancel at the next step boundary"
    );
}
