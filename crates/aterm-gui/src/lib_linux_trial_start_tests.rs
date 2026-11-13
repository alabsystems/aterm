// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{LinuxTrialStart, linux_trial_start};

#[test]
fn only_a_launch_that_can_open_a_window_is_a_window_start() {
    assert_eq!(linux_trial_start(false, true), LinuxTrialStart::Window);
    assert_eq!(linux_trial_start(true, true), LinuxTrialStart::Session);
    assert_eq!(linux_trial_start(true, false), LinuxTrialStart::Session);
    assert_eq!(
        linux_trial_start(false, false),
        LinuxTrialStart::WindowOnceOpen
    );
}
