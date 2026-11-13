// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ResampleCause, resample_wants_repair_frame};

#[test]
fn only_a_settings_burst_buys_the_repair_frame() {
    assert!(
        !resample_wants_repair_frame(ResampleCause::Poll),
        "focus-gain must not repaint every window"
    );
    assert_eq!(
        resample_wants_repair_frame(ResampleCause::SettingsBurst),
        cfg!(windows),
        "the repair frame exists to undo winit's Win32 non-client re-theme, \
         and only Windows has one"
    );
}
