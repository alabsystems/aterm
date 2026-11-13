// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::ffi::OsStr;

/// [`aterm_log::env::scoped`], primed, in a test run alone ([`moves_alone`]).
#[track_caller]
pub(crate) fn scoped<T>(
    key: impl AsRef<OsStr>,
    value: impl AsRef<OsStr>,
    body: impl FnOnce() -> T,
) -> T {
    moves_alone(key.as_ref());
    super::prime_test_update_root();
    aterm_log::env::scoped(key, value, body)
}

/// [`aterm_log::env::scoped_unset`], primed, in a test run alone ([`moves_alone`]).
#[track_caller]
pub(crate) fn scoped_unset<T>(key: impl AsRef<OsStr>, body: impl FnOnce() -> T) -> T {
    moves_alone(key.as_ref());
    super::prime_test_update_root();
    aterm_log::env::scoped_unset(key, body)
}

/// The lib-test process's environment never moves: a test that moves a variable runs alone
/// in a child of the test binary (`if !crate::test_alone::alone() { return; }`). The scope
/// holds ENV_LOCK, but only other writers take it: code that reads a state, config or data
/// path reads the live environment, and in the lib-test process another test's scope
/// starting or ending between two of its reads splits them.
#[track_caller]
fn moves_alone(key: &OsStr) {
    assert!(
        crate::test_alone::in_child(),
        "{}: a test that moves the environment runs alone in a child (`test_alone::alone`)",
        key.display()
    );
}

/// A move of the environment is refused in the lib-test process, before it moves anything,
/// and made in a test run alone.
#[test]
fn the_environment_moves_only_in_a_test_run_alone() {
    const KEY: &str = "ATERM_TEST_ENV_MOVED_ALONE";
    if !crate::test_alone::in_child() {
        let moved = std::panic::catch_unwind(|| scoped(KEY, "moved", || ()));
        assert!(moved.is_err(), "the lib-test process's environment moved");
        assert_eq!(std::env::var_os(KEY), None, "the refusal moved it first");
    }
    if !crate::test_alone::alone() {
        return;
    }
    assert_eq!(
        scoped(KEY, "moved", || std::env::var(KEY).ok()),
        Some("moved".to_owned())
    );
}
