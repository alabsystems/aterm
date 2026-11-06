// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet task board's keeper in the aterm serving this Mac (`aterm_ctl::board::keep`): one
//! thread per process, started by the window's control server and by the node job, of which the
//! one holding the board's keeper lock waits on its open rows and the others take over within
//! seconds of its exit (`docs/DESIGN-fleet-task-board-2026-11-04.md`).

use std::path::PathBuf;

/// Start this process's keeper of the board in `state` once; a later call does nothing. A test
/// build starts none: its boards are its own tests'.
pub(crate) fn start(state: PathBuf) {
    #[cfg(all(unix, not(test)))]
    {
        static STARTED: std::sync::Once = std::sync::Once::new();
        STARTED.call_once(move || {
            let spawned = crate::qos::spawn_lane_thread(
                "aterm-board-keeper",
                crate::qos::Role::Background,
                move || {
                    aterm_ctl::board::keep(
                        &state,
                        &aterm_ctl::board::Keeper {
                            drive: Some(aterm_agent::drive_fleet),
                            here: &|| aterm_ctl::resolve_sock_for(None).ok(),
                            own_node: &aterm_agent::fleet_node,
                        },
                    );
                },
            );
            if let Err(err) = spawned {
                aterm_log::warn!(
                    "the fleet task board's keeper could not start: {err}; its rows move only \
                     when a wait on this Mac reads them"
                );
            }
        });
    }
    #[cfg(not(all(unix, not(test))))]
    let _ = state;
}
