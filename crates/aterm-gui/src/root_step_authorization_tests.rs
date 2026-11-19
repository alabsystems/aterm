// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A RUN WHOSE STEP NEVER STARTED NEVER RAN: a mistyped password (sudo asked again, the second
/// request cancelled the run, sudo exited 1) reads "the password was not accepted", not a step
/// that ran and failed; a step that printed its started line and exited 1 did run.
/// CONTROL: the same exit with the started line reads as the step's own failure, a withdrawn
/// request and an unanswered one keep their words, and sudo's 0 is the step finished.
#[test]
fn a_mistyped_password_is_a_step_that_never_ran() {
    let step = Step::Power;
    assert_eq!(
        verdict(Some(1), false, true, true, step),
        Ended::NotRun("the password was not accepted".to_owned())
    );
    assert_eq!(
        verdict(Some(1), false, true, false, step),
        Ended::NotRun("the password was not accepted".to_owned()),
        "a sudo that allows one try refuses without asking again"
    );
    assert!(
        matches!(verdict(Some(1), true, true, false, step), Ended::NotApplied(why) if why.contains("exit 1")),
        "CONTROL: the step ran"
    );
    assert_eq!(
        verdict(Some(1), false, false, true, step),
        Ended::NotRun("the password request was withdrawn".to_owned()),
        "CONTROL: withdrawn"
    );
    assert_eq!(
        verdict(Some(1), false, false, false, step),
        Ended::NotRun("no password was given".to_owned()),
        "CONTROL: unanswered"
    );
    assert_eq!(
        verdict(None, true, true, false, step),
        Ended::NotApplied("the step ended by a signal".to_owned())
    );
    assert_eq!(verdict(Some(0), true, true, false, step), Ended::Applied);
}

/// The started line is read off the step's output wherever it falls, and an output that ends
/// without it says so.
/// CONTROL: a line that only resembles it is no start.
#[test]
fn the_started_line_is_read_off_the_steps_output() {
    let read = |text: String| {
        let rx = watch_started(std::io::Cursor::new(text.into_bytes())).expect("the reader");
        rx.recv_timeout(Duration::from_mins(1)).expect("an answer")
    };
    assert!(read(format!("{}\n", root_only::STARTED)));
    assert!(read(format!("noise\n{}\nmore\n", root_only::STARTED)));
    assert!(!read(String::new()));
    assert!(
        !read(format!("{} later\n", root_only::STARTED)),
        "CONTROL: not the line"
    );
}
