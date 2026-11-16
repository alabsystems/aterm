// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the window's event sources ring besides the window itself: each background wait that
//! rests long on an idle Mac (ATERM-KI-1108-2) and must not sleep through the event.

use crate::park_bell::ParkBell;
use crate::services_watch;
use crate::window_presence::{self, Presence};

/// The Mac woke from sleep or the wall clock stepped (the watchdog's wall watch,
/// `crate::watchdog::start`): the window's presence writes ([`window_presence::PRESENCE`]) and
/// the services watch reads ([`services_watch::BELL`]), since neither rest runs while the Mac
/// sleeps; then `send` tells the window.
pub(crate) fn wall_moved(send: impl FnOnce()) {
    wall_moved_on(&window_presence::PRESENCE, &services_watch::BELL, send);
}

/// [`wall_moved`] on `presence` and the services watch's `services` bell.
fn wall_moved_on(presence: &Presence, services: &ParkBell<bool>, send: impl FnOnce()) {
    presence.ring();
    services_watch::ring_on(services);
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
