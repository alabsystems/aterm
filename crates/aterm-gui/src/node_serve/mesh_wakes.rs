// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the node service's mesh hears of this Mac's sleep and network (LT-B1): a wake from sleep
//! (`aterm_uds::power::listen`, the first wake after a will-sleep: `WakeEdge`) and each change
//! of the path Network.framework's monitor reports (`aterm_objc::path_monitor::PathMonitor`,
//! every update after its first, which is the path in use as it started: [`PathChanges`]) are
//! handed to the overlay's [`aterm_mesh::Waker::woke`], which asks every link to re-prove within
//! 5 s, rings the overlay, kicks every relay and clears every seat's advance on presence's board.
//! These subscriptions only feed that one function, which the mesh's own tests drive with an
//! injected event. Elsewhere than macOS nothing is heard: the links' keepalive and re-proof bound
//! a dead neighbour there.

use aterm_mesh::{Waker, Woke};
#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::{AtomicBool, Ordering};

/// Which path readings are changes: every one after the first, which is the path in use when the
/// monitor started (`PathMonitor::start`: "each later update is a change of that path").
#[cfg(any(target_os = "macos", test))] // heard on macOS alone (Network.framework)
#[derive(Debug, Default)]
pub(super) struct PathChanges {
    started: AtomicBool,
}

#[cfg(any(target_os = "macos", test))]
impl PathChanges {
    /// One reading heard: whether it is a change of the path.
    pub(super) fn read(&self) -> bool {
        self.started.swap(true, Ordering::AcqRel)
    }
}

/// The subscriptions feeding one generation's overlay: held with its links, and dropped (each
/// ending at once) before they are torn down.
pub(super) struct MeshWakes {
    #[cfg(target_os = "macos")]
    _power: Option<aterm_uds::power::Listening>,
    #[cfg(target_os = "macos")]
    _path: Option<aterm_objc::path_monitor::PathMonitor>,
}

impl std::fmt::Debug for MeshWakes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshWakes").finish_non_exhaustive()
    }
}

/// Hand `waker` this Mac's wakes and network path changes while the returned subscriptions live.
pub(super) fn hear(waker: &Waker) -> MeshWakes {
    #[cfg(target_os = "macos")]
    {
        let woke = waker.clone();
        let mut edge = crate::harness_netprobe::WakeEdge::default();
        let power = aterm_uds::power::listen(Box::new(move |announced| {
            if edge.hear(announced) {
                let _ = woke.woke(Woke::Wake);
            }
        }));
        let moved = waker.clone();
        let changes = PathChanges::default();
        let path =
            aterm_objc::path_monitor::PathMonitor::start(c"aterm.mesh.net-path", move |_| {
                if changes.read() {
                    let _ = moved.woke(Woke::PathChanged);
                }
            });
        if power.is_none() || path.is_none() {
            aterm_log::info!(
                "node: the mesh hears {}{}; its links' keepalive and re-proof bound a dead \
                 neighbour without it",
                if power.is_none() { "no wake" } else { "wakes" },
                if path.is_none() {
                    " and no network path change"
                } else {
                    " and network path changes"
                }
            );
        }
        MeshWakes {
            _power: power,
            _path: path,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (waker, Woke::Wake);
        MeshWakes {}
    }
}

#[cfg(test)]
#[path = "mesh_wakes_tests.rs"]
mod tests;
