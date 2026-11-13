// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    CONSOLE_BITS, CREATE_NEW_CONSOLE, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, Console,
    DETACHED_PROCESS, creation_flags, shows_a_console_window,
};

/// A quiet child is windowless only when this process has no console, the explicit
/// choice ignores whether it has one, and the console bits come from the choice alone.
#[test]
fn the_console_is_decided_by_the_choice_and_whether_this_process_has_one() {
    let group = CREATE_NEW_PROCESS_GROUP;
    for (console, extra, have, want) in [
        (Console::Quiet, 0, false, CREATE_NO_WINDOW),
        (Console::Quiet, group, false, CREATE_NO_WINDOW | group),
        (Console::Quiet, 0, true, 0),
        (Console::Quiet, group, true, group),
        (Console::Detached, 0, false, DETACHED_PROCESS),
        (Console::Detached, 0, true, DETACHED_PROCESS),
        (Console::Detached, group, true, DETACHED_PROCESS | group),
        (Console::Quiet, CREATE_NEW_CONSOLE | group, true, group),
        (Console::Quiet, DETACHED_PROCESS, false, CREATE_NO_WINDOW),
        (Console::Quiet, CREATE_NO_WINDOW, true, 0),
        (Console::Detached, CREATE_NO_WINDOW, false, DETACHED_PROCESS),
        (
            Console::Detached,
            CONSOLE_BITS | group,
            true,
            DETACHED_PROCESS | group,
        ),
    ] {
        let got = creation_flags(console, extra, have);
        assert_eq!(
            got, want,
            "{console:?} extra {extra:#x} have_console {have}"
        );
        assert!(
            !shows_a_console_window(got, have),
            "{console:?} extra {extra:#x} have_console {have} must never show a window"
        );
    }
    assert!(shows_a_console_window(0, false));
    assert!(!shows_a_console_window(0, true));
    assert!(shows_a_console_window(CREATE_NEW_CONSOLE, true));
    assert!(shows_a_console_window(
        CREATE_NEW_CONSOLE | CREATE_NO_WINDOW,
        false
    ));
}
