// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_c_name_is_the_name() {
    assert_eq!(CHANGED_C.to_str(), Ok(CHANGED));
}

#[test]
fn a_post_reaches_notifyd() {
    assert!(aterm_sysprobe::notify::post(
        c"com.aterm.aterm.acaffeine.test.unobserved"
    ));
}

#[test]
fn the_change_is_observed_only_on_the_main_thread() {
    assert!(aterm_objc::MainThread::new().is_none());
    assert!(observe_changed(|| {}).is_none());
}
