// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::Arc;

/// Book an in-flight tab to `wid` without starting the spawner thread.
#[cfg(unix)]
pub(crate) fn book(app: &mut App, wid: WindowId, then: TabSpawnThen) -> u64 {
    let id = app.next_session_id;
    app.next_session_id += 1;
    let parent = app.new_tab_parent(wid);
    let colors = app.engine_color_fanouts;
    let grid = app.spawn_grid(wid);
    app.tab_spawns.in_flight.push(InFlight {
        id,
        alt_id: None,
        window: wid,
        place: SpawnPlace::Tab,
        asked: Instant::now(),
        parent,
        colors,
        grid,
        then,
        admission: SpawnAdmission::Plain,
        background: false,
        parks: false,
        fleet: false,
        refuses_worker_split: false,
        unasked: false,
        completion: Arc::new(TabSpawnCompletion::default()),
        theme: None,
        acting: None,
    });
    id
}

/// The act the newest booking was made under ([`crate::tab_marks::acting`] as it booked),
/// which its tab's open names once it lands.
pub(crate) fn booked_act(app: &App) -> Option<crate::tab_marks::Acting> {
    app.tab_spawns
        .in_flight
        .last()
        .and_then(|t| t.acting.clone())
}

/// Book an in-flight tab nobody asked for ([`SpawnAsk::unasked`]), as [`book`] does.
#[cfg(unix)]
pub(crate) fn book_unasked(app: &mut App, wid: WindowId, then: TabSpawnThen) -> u64 {
    book_as(
        app,
        wid,
        &SpawnAsk {
            unasked: true,
            ..SpawnAsk::default()
        },
        then,
    )
}

/// Book an in-flight tab as `ask` would ([`SpawnAsk::unasked`] carried over), as [`book`]
/// does.
#[cfg(unix)]
pub(crate) fn book_as(app: &mut App, wid: WindowId, ask: &SpawnAsk, then: TabSpawnThen) -> u64 {
    let parent = app.spawn_parent(wid, ask);
    let id = book(app, wid, then);
    if let Some(booked) = app.tab_spawns.in_flight.iter_mut().find(|t| t.id == id) {
        booked.parent = parent;
        booked.unasked = ask.unasked;
        booked.background = ask.background;
        booked.parks = ask.parks();
        booked.fleet = ask.parks() && ask.fleet;
        booked.admission = ask.admission.clone();
    }
    id
}

/// Book an in-flight window without starting the spawner thread.
pub(crate) fn book_window(
    app: &mut App,
    parent: Option<aterm_session::SessionId>,
    admission: SpawnAdmission,
    then: TabSpawnThen,
) -> (u64, WindowId) {
    let (window, place, grid) = app.reserve_window();
    let id = app.next_session_id;
    app.next_session_id += 1;
    let colors = app.engine_color_fanouts;
    app.tab_spawns.in_flight.push(InFlight {
        id,
        alt_id: None,
        window,
        place,
        asked: Instant::now(),
        parent,
        colors,
        grid,
        then,
        admission,
        background: false,
        parks: false,
        fleet: false,
        refuses_worker_split: false,
        unasked: false,
        completion: Arc::new(TabSpawnCompletion::default()),
        theme: None,
        acting: None,
    });
    (id, window)
}

/// Test [`LandingOs`] without WindowServer access.
pub(crate) struct InertOs {
    pub(crate) exiting: bool,
    pub(crate) attaches: bool,
}

impl InertOs {
    /// A running loop whose windows attach.
    pub(crate) const ATTACHES: Self = Self {
        exiting: false,
        attaches: true,
    };
}

impl LandingOs for InertOs {
    fn exiting(&self) -> bool {
        self.exiting
    }

    fn attach(&self, _app: &mut App, _wid: WindowId) -> bool {
        self.attaches
    }
}
