// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_options_are_display_and_system_sleep_off_user_initiated() {
    assert_eq!(OPTIONS, 0x100_00FF_FFFF);
    assert_ne!(OPTIONS & (1 << 20), 0);
}

#[test]
fn an_activity_begins_and_ends() {
    let first = Awake::begin("aterm acaffeine test").expect("begin");
    let second = Awake::begin("aterm acaffeine test").expect("begin");
    assert_ne!(first.token.id(), second.token.id());
    drop(first);
    drop(second);
}

/// The assertion types the power manager lists under this pid named `reason`.
pub(crate) fn assertions_named(reason: &str) -> Vec<String> {
    let held = aterm_sysprobe::power::assertions_of(std::process::id())
        .expect("the power manager answers");
    crate::held::assertion_types(&held, reason)
}

#[test]
fn powerd_lists_both_assertions_while_held_and_none_after() {
    let reason = format!("aterm acaffeine assertion test {}", std::process::id());
    let held = Awake::begin(&reason).expect("begin");
    let mut while_held = assertions_named(&reason);
    while_held.sort();
    drop(held);
    let after = assertions_named(&reason);
    assert_eq!(
        while_held,
        ["PreventUserIdleDisplaySleep", "PreventUserIdleSystemSleep"]
    );
    assert_eq!(after, Vec::<String>::new());
}
