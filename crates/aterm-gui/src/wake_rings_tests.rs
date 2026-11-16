// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::time::Duration;

use super::*;
use crate::window_presence::Heard;

/// A WAKE RINGS EACH LONG REST THAT MUST NOT SLEEP THROUGH IT, AND THE WINDOW: the presence's
/// beat hears a wake (it writes now), the services watch's rest is rung (it reads now), and the
/// window is sent its wake. NEGATIVE CONTROL: leave either ring out of `wall_moved_on` and its
/// place in the assert fails (the presence's wait is due, the services rest unrung).
#[test]
fn a_wake_rings_the_presence_the_services_watch_and_the_window() {
    let presence = Presence::new();
    let services = ParkBell::new(false);
    let sent = Cell::new(false);
    wall_moved_on(&presence, &services, || sent.set(true));
    assert_eq!(
        (
            sent.get(),
            presence.wait(Duration::ZERO),
            services_watch::rest_on(&services, Duration::ZERO),
        ),
        (true, Heard::Woke, true),
        "(the window hears the wake, the presence writes, the services watch reads)"
    );
}

/// A MOVE OF THE KERNEL'S MEMORY PRESSURE ENDS THE SERVICES WATCH'S REST, AND THE WINDOW HEARS
/// IT. NEGATIVE CONTROL: leave the ring out of `memory_pressure_moved_on` and the rest is unrung.
#[test]
fn a_pressure_move_rings_the_services_watch_and_the_window() {
    let services = ParkBell::new(false);
    let sent = Cell::new(false);
    memory_pressure_moved_on(&services, || sent.set(true));
    assert!(sent.get(), "the window hears the pressure");
    assert!(
        services_watch::rest_on(&services, Duration::ZERO),
        "the services watch reads"
    );
}
