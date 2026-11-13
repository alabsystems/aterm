// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/event.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::consts::{
    NS_EVENT_MODIFIER_FLAG_COMMAND as COMMAND, NS_EVENT_MODIFIER_FLAG_CONTROL as CONTROL,
    NS_EVENT_MODIFIER_FLAG_OPTION as OPTION, NS_EVENT_MODIFIER_FLAG_SHIFT as SHIFT,
};
use super::{option_is_alt, NX_DEVICELALTKEYMASK, NX_DEVICERALTKEYMASK};
use crate::platform::macos::OptionAsAlt;

const LEFT: usize = OPTION | NX_DEVICELALTKEYMASK;
const RIGHT: usize = OPTION | NX_DEVICERALTKEYMASK;

/// Which held Option a setting makes an Alt, and that a chord with Control or Command
/// keeps AppKit's own reading (its text is already read without Option).
#[test]
fn an_option_is_alt_only_where_the_setting_names_it_and_never_under_control_or_command() {
    use OptionAsAlt::{Both, None, OnlyLeft, OnlyRight};
    let rows: &[(OptionAsAlt, usize, bool)] = &[
        (None, LEFT, false),
        (None, RIGHT, false),
        (Both, LEFT, true),
        (Both, RIGHT, true),
        (Both, LEFT | SHIFT, true),
        (Both, SHIFT, false),
        (Both, 0, false),
        (OnlyLeft, LEFT, true),
        (OnlyLeft, RIGHT, false),
        (OnlyRight, RIGHT, true),
        (OnlyRight, LEFT, false),
        (Both, LEFT | CONTROL, false),
        (Both, RIGHT | COMMAND, false),
        (OnlyLeft, LEFT | CONTROL | SHIFT, false),
    ];
    for &(setting, flags, alt) in rows {
        assert_eq!(option_is_alt(setting, flags), alt, "{setting:?} flags {flags:#x}");
    }
}
