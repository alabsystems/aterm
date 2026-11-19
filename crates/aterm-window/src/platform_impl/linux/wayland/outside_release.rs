// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13 to
// resolve tab drops after a Wayland pointer grab (see the repository NOTICE).

//! Resolve a captured pointer release using the compositor's next focus for that seat.

use aterm_hash::FxHashMap;

use super::state::WinitState;
use super::wire::Owned;
use super::WindowId;
use crate::dpi::PhysicalPosition;
use crate::event::WindowEvent;
use crate::window::WindowId as RootWindowId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ReleaseId {
    seat: u32,
    serial: u32,
}

struct Release {
    source: WindowId,
    awaiting_focus: bool,
    target: Option<(WindowId, PhysicalPosition<f64>)>,
    request: Option<([u64; 2], Owned)>,
}

impl Release {
    fn event(&self, token: [u64; 2]) -> WindowEvent {
        let target = self.target.map(|(target, point)| (RootWindowId(target), point));
        WindowEvent::DroppedLocal { token, target }
    }
}

#[derive(Default)]
pub(crate) struct OutsideReleases {
    /// The event currently being delivered to the application, independent of any later
    /// button events the compositor included in the same dispatch.
    pub(crate) dispatching: Option<ReleaseId>,
    pending: FxHashMap<ReleaseId, Release>,
}

impl OutsideReleases {
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(crate) fn released(&mut self, seat: u32, serial: u32, source: WindowId) -> ReleaseId {
        let id = ReleaseId { seat, serial };
        self.pending.insert(id, Release {
            source,
            awaiting_focus: true,
            target: None,
            request: None,
        });
        id
    }

    /// A new pointer action cannot supply the destination of an earlier release.
    pub(crate) fn stop_observing(&mut self, seat: u32) {
        for (id, release) in &mut self.pending {
            if id.seat == seat {
                release.awaiting_focus = false;
            }
        }
    }

    pub(crate) fn entered(&mut self, seat: u32, target: WindowId, position: PhysicalPosition<f64>) {
        for (id, release) in &mut self.pending {
            if id.seat == seat && release.awaiting_focus {
                release.target = Some((target, position));
                release.awaiting_focus = false;
            }
        }
    }

    /// Ordinary releases need no round trip or follow-up event.
    pub(crate) fn dispatched(&mut self, id: ReleaseId) {
        self.dispatching = None;
        if self.pending.get(&id).is_some_and(|release| release.request.is_none()) {
            self.pending.remove(&id);
        }
    }
}

impl WinitState {
    pub(crate) fn resolve_outside_release(&mut self, source: WindowId, token: [u64; 2]) -> bool {
        let Some(id) = self.outside_releases.dispatching else { return false };
        let Some(release) = self.outside_releases.pending.get_mut(&id) else { return false };
        if release.source != source || release.request.is_some() {
            return false;
        }
        if !release.awaiting_focus {
            // Release and enter (or a subsequent pointer action) arrived in one batch.
            self.events_sink.push_window_event(release.event(token), source);
        } else if let Some(barrier) = self.inbox.outside_release_barrier(&self.connection.queue, id)
        {
            release.request = Some((token, barrier));
        }
        // Even if the display could not create its barrier, consume the gesture:
        // no destination was established, so treating it as a desktop drop would tear it off.
        true
    }

    pub(crate) fn complete_outside_release(&mut self, id: ReleaseId) {
        let Some(release) = self.outside_releases.pending.remove(&id) else { return };
        if let Some((token, _barrier)) = &release.request {
            self.events_sink.push_window_event(release.event(*token), release.source);
        }
    }
}

#[cfg(test)]
#[path = "outside_release_tests.rs"]
mod tests;
