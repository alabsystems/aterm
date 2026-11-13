// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use super::{RunEnd, settle_cancelled_video_export};

/// The run's end runs once however often it is reached: the loop's exit inside AppKit runs it,
/// and `main`, which a panic's stop returns to after it, reaches it again; an App that never
/// ran a loop owes none.
///
/// NEGATIVE CONTROL: read `run_end` in place of taking it and the second call runs the
/// shutdown again (two runs).
#[test]
fn the_runs_end_runs_once() {
    let mut app = crate::App::headless_for_test();
    app.end_run();
    assert!(app.run_end.is_none());

    let ran = Rc::new(Cell::new(0));
    let mut end = RunEnd::new(aterm_session::SessionId::new("s-root"));
    end.counted = Some(Rc::clone(&ran));
    app.run_end = Some(end);
    app.end_run();
    app.end_run();
    assert_eq!(ran.get(), 1);
    assert!(app.run_end.is_none());
}

/// A cancelled video export that never cleans up holds the run's end only to its bound,
/// which then goes on; one that is idle holds it not at all.
///
/// NEGATIVE CONTROL: wait on it until it is idle (the loop before 2026-11-07, 120 s at a
/// time with no end) and this test hangs: a logout under the quit, which macOS holds until
/// the run's end is done, hangs with it.
#[test]
fn a_stuck_video_export_holds_the_runs_end_only_to_its_bound() {
    let export = std::sync::Arc::new(crate::VideoExportState::default());
    assert!(settle_cancelled_video_export(&export, Duration::ZERO));
    let permit = export
        .try_begin(crate::VideoCancellation::new())
        .expect("the export starts");
    let (done, settled) = std::sync::mpsc::channel();
    let waiting = std::sync::Arc::clone(&export);
    std::thread::spawn(move || {
        let _ = done.send(settle_cancelled_video_export(
            &waiting,
            Duration::from_millis(1),
        ));
    });
    assert_eq!(
        settled.recv_timeout(Duration::from_secs(60)),
        Ok(false),
        "the bounded wait returns, unsettled"
    );
    drop(permit);
    assert!(settle_cancelled_video_export(&export, Duration::ZERO));
}
