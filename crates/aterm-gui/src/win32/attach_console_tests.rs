// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn attach_reports_no_attach_when_the_parent_supplied_every_handle() {
    assert!(!super::attach_parent_console());
    assert!(!super::attached_a_console());
}

#[test]
fn an_attached_console_is_released_only_for_a_window() {
    assert!(super::attached_console_is_released_for(true, false));
    assert!(!super::attached_console_is_released_for(true, true));
    assert!(!super::attached_console_is_released_for(false, false));
    assert!(!super::attached_console_is_released_for(false, true));
}
