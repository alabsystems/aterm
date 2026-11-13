// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::read_bounded_stdout;

#[cfg(unix)]
#[test]
fn the_bounded_read_returns_stdout_of_a_successful_child() {
    assert_eq!(
        read_bounded_stdout(
            "/bin/sh",
            &["-c", "printf false"],
            aterm_spec::wait::MUST_HAPPEN
        )
        .as_deref(),
        Some("false")
    );
    assert_eq!(
        read_bounded_stdout(
            "/bin/sh",
            &["-c", "printf false; exit 3"],
            aterm_spec::wait::MUST_HAPPEN
        ),
        None,
        "a failing exit is not a preference"
    );
    assert_eq!(
        read_bounded_stdout(
            "/nonexistent/wbgui-no-such-gsettings",
            &[],
            aterm_spec::wait::MUST_HAPPEN
        ),
        None,
        "a missing binary is not a preference"
    );
}

#[cfg(unix)]
#[test]
fn the_bounded_read_kills_a_child_that_outlives_the_bound() {
    let started = std::time::Instant::now();
    assert_eq!(
        read_bounded_stdout("/bin/sleep", &["600"], std::time::Duration::from_millis(50)),
        None
    );
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the read must not wait for the child: took {:?}",
        started.elapsed()
    );
}
