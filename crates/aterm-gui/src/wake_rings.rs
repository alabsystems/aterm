// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the window's event sources ring besides the window itself: each background wait that
//! rests long on an idle Mac (ATERM-KI-1108-2) and must not sleep through the event.

use crate::services_watch;
use crate::window_presence::{self, Presence};
use aterm_uds::park_bell::ParkBell;
use atpkg::vendor_direct::watch::Ring;

/// The package lane's wall ring ([`atpkg::vendor_direct::watch::Rest::wall_ringer`]), set once its
/// rest starts (`crate::spawn_pkg_update_check`).
pub(crate) static PKG_LANE: std::sync::OnceLock<Ring> = std::sync::OnceLock::new();

/// The Mac woke from sleep or the wall clock stepped (the watchdog's wall watch,
/// `crate::watchdog::start`): the window's presence writes ([`window_presence::PRESENCE`]), the
/// services watch reads ([`services_watch::BELL`]) and the package lane judges the wake
/// ([`PKG_LANE`]), since none of their rests runs while the Mac sleeps; then `send` tells the
/// window.
pub(crate) fn wall_moved(send: impl FnOnce()) {
    wall_moved_on(
        &window_presence::PRESENCE,
        &services_watch::BELL,
        PKG_LANE.get(),
        send,
    );
}

/// [`wall_moved`] on `presence`, the services watch's `services` bell and the package lane's
/// `pkg_lane` ring.
fn wall_moved_on(
    presence: &Presence,
    services: &ParkBell<bool>,
    pkg_lane: Option<&Ring>,
    send: impl FnOnce(),
) {
    presence.ring();
    services_watch::ring_on(services);
    if let Some(ring) = pkg_lane {
        ring.ring();
    }
    send();
}

/// The kernel's memory pressure moved (`crate::memory_pressure::install`'s handler): the services
/// watch reads now ([`services_watch::BELL`]), so a calm rest does not hide the strain; then
/// `send` tells the window.
#[cfg(target_os = "macos")]
pub(crate) fn memory_pressure_moved(send: impl FnOnce()) {
    memory_pressure_moved_on(&services_watch::BELL, send);
}

/// [`memory_pressure_moved`] on the services watch's `services` bell.
#[cfg(any(target_os = "macos", test))]
fn memory_pressure_moved_on(services: &ParkBell<bool>, send: impl FnOnce()) {
    services_watch::ring_on(services);
    send();
}

#[cfg(test)]
#[path = "wake_rings_tests.rs"]
mod tests;
