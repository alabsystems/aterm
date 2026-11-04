// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A registered stale variable leaves every command built after the registration, in a binary of
//! its own because the registration is process-wide. Unix only: the hook is Unix's `spawnfd`.
#![cfg(unix)]

use std::ffi::OsStr;

const PROBE: &str = "ATERM_UDS_STALE_INHERITED_PROBE";

fn stale() -> Option<&'static str> {
    Some(PROBE)
}

#[test]
fn a_registered_stale_variable_leaves_each_new_command_and_a_set_still_wins() {
    // NEGATIVE CONTROL: with nothing registered, a new command edits no variable.
    let before = aterm_uds::spawnfd::command("/usr/bin/true");
    assert_eq!(before.get_envs().count(), 0);

    aterm_uds::spawnfd::register_stale_inherited_var(stale);
    let after = aterm_uds::spawnfd::command("/usr/bin/true");
    assert_eq!(
        after.get_envs().collect::<Vec<_>>(),
        [(OsStr::new(PROBE), None)]
    );

    // A command that names the variable itself still sets it.
    let mut set = aterm_uds::spawnfd::command("/usr/bin/true");
    set.env(PROBE, "kept");
    assert_eq!(
        set.get_envs().collect::<Vec<_>>(),
        [(OsStr::new(PROBE), Some(OsStr::new("kept")))]
    );
}
