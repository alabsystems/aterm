// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Install the subscriber's thread and child scheduling before its runtime starts.

use std::ffi::OsString;
use std::path::Path;

/// Enter only an exact front-door watch, before a worker, keyboard keeper, vault
/// callback or state operation. The owned service starts without a process-wide band;
/// its routine work stays at the background class and its lock holders may lift.
pub(super) fn enter(argv: &[OsString]) {
    enter_with(argv, install);
}

fn enter_with(argv: &[OsString], hold: impl FnOnce()) {
    let Some(argv0) = argv.first() else {
        return;
    };
    let name = Path::new(argv0)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if aterm_cli::alias_route(name, "secrets") != aterm_cli::AliasRoute::FrontDoor {
        return;
    }
    let args = argv.get(1..).unwrap_or_default();
    if args.len() == 2 && args[0] == "secrets" && args[1] == "watch" {
        hold();
    }
}

pub(super) fn install() {
    aterm_gui::hold_process_as_housekeeping();
    aterm_uds::spawnfd::start_children_in_background_band();
}

#[cfg(test)]
#[path = "subscriber_scheduling_tests.rs"]
mod tests;
