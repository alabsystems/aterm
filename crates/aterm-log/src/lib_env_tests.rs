// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Each test uses a distinct key so parallel tests do not race on values.
#[test]
fn take_reads_and_removes_in_one_step() {
    const KEY: &str = "ATERM_LOG_TEST_TAKE";
    super::env::set(KEY, "authority");
    assert_eq!(std::env::var_os(KEY).as_deref(), Some("authority".as_ref()));
    assert_eq!(super::env::take(KEY).as_deref(), Some("authority".as_ref()));
    assert_eq!(super::env::take(KEY), None);
    assert_eq!(std::env::var_os(KEY), None);
}

#[test]
fn scoped_restores_the_previous_value_and_the_previous_absence() {
    const KEY: &str = "ATERM_LOG_TEST_SCOPED";
    super::env::set(KEY, "outer");
    let seen = super::env::scoped(KEY, "inner", || std::env::var_os(KEY));
    assert_eq!(
        seen.as_deref(),
        Some("inner".as_ref()),
        "body sees the override"
    );
    assert_eq!(
        std::env::var_os(KEY).as_deref(),
        Some("outer".as_ref()),
        "and the previous value is back"
    );

    super::env::unset(KEY);
    let seen = super::env::scoped(KEY, "inner", || std::env::var_os(KEY));
    assert_eq!(seen.as_deref(), Some("inner".as_ref()));
    assert_eq!(
        std::env::var_os(KEY),
        None,
        "restoring an absent key means REMOVING it, not setting it empty"
    );
}

#[test]
fn scoped_unset_hides_a_set_key_for_exactly_the_body() {
    const KEY: &str = "ATERM_LOG_TEST_SCOPED_UNSET";
    super::env::set(KEY, "present");
    let seen = super::env::scoped_unset(KEY, || std::env::var_os(KEY));
    assert_eq!(seen, None, "the body sees the key as absent");
    assert_eq!(std::env::var_os(KEY).as_deref(), Some("present".as_ref()));
    super::env::unset(KEY);
}

/// A panicking body still restores: a leaked override would corrupt every
/// later test in the binary.
#[test]
fn scoped_restores_even_when_the_body_panics() {
    const KEY: &str = "ATERM_LOG_TEST_SCOPED_PANIC";
    super::env::set(KEY, "outer");
    let caught = std::panic::catch_unwind(|| {
        super::env::scoped(KEY, "inner", || panic!("body blew up"));
    });
    assert!(caught.is_err(), "the panic propagates to the caller");
    assert_eq!(
        std::env::var_os(KEY).as_deref(),
        Some("outer".as_ref()),
        "and the override did not leak past it"
    );
    super::env::unset(KEY);
}
