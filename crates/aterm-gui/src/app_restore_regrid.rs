// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One final re-grid, resumed between planning tabs and applying individual leaves.

use std::collections::VecDeque;
use std::time::Instant;

use super::{App, RESTORE_SHOW_CAP, RESTORE_SHOW_SETTLE, RestoreRegridOwed, WindowId};
use crate::app_render::PlannedPaneResize;
use crate::tab_model::{TabSetRevision, ViewStoreRevision};

struct Geometry {
    tabs: TabSetRevision,
    views: ViewStoreRevision,
    rows: u16,
    cols: u16,
    chrome: u16,
    cell_px: Option<(u16, u16)>,
    active: Option<crate::tab_model::TabId>,
}

impl Geometry {
    fn read(app: &App, wid: WindowId) -> Option<Self> {
        let window = app.windows.get(&wid)?;
        Some(Self {
            tabs: window.tab_set.revision(),
            views: app.view_store.revision(),
            rows: window.rows,
            cols: window.cols,
            chrome: app.chrome_rows(wid),
            cell_px: app.spawn_cell_px(wid),
            active: window.tab_set.active_id(),
        })
    }

    fn current(&self, app: &App, wid: WindowId) -> bool {
        app.windows.get(&wid).is_some_and(|window| {
            self.tabs.is_current(&window.tab_set)
                && self.views.is_current(&app.view_store)
                && (self.rows, self.cols) == (window.rows, window.cols)
                && self.chrome == app.chrome_rows(wid)
                && self.cell_px == app.spawn_cell_px(wid)
                && self.active == window.tab_set.active_id()
        })
    }
}

struct Window {
    wid: WindowId,
    geometry: Option<Geometry>,
    next_tab: usize,
    targets: VecDeque<PlannedPaneResize>,
}

pub(super) enum Step {
    Advanced,
    Wait(Instant),
    Complete,
}

pub(super) struct Finalizing {
    pending: VecDeque<Window>,
    completed: Vec<Window>,
    finished: bool,
}

impl Finalizing {
    pub fn new(windows: impl Iterator<Item = WindowId>) -> Self {
        Self {
            pending: windows
                .map(|wid| Window {
                    wid,
                    geometry: None,
                    next_tab: 0,
                    targets: VecDeque::new(),
                })
                .collect(),
            completed: Vec::new(),
            finished: false,
        }
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn cancel(&mut self, wid: WindowId, all: bool) {
        self.pending.retain(|window| !all && window.wid != wid);
        self.completed.retain(|window| !all && window.wid != wid);
    }

    pub fn step(&mut self, app: &mut App) -> Step {
        if self.pending.is_empty() {
            // An input event may have changed a window already finished while another waited.
            if let Some(at) = self.completed.iter().position(|window| {
                app.windows.contains_key(&window.wid)
                    && !window
                        .geometry
                        .as_ref()
                        .is_some_and(|geometry| geometry.current(app, window.wid))
            }) {
                self.pending.push_back(self.completed.swap_remove(at));
            } else {
                self.finished = true;
                return Step::Complete;
            }
        }
        let mut window = self.pending.pop_front().expect("pending re-grid window");
        let wid = window.wid;
        if !app.windows.contains_key(&wid) {
            return Step::Advanced;
        }
        if !window
            .geometry
            .as_ref()
            .is_some_and(|geometry| geometry.current(app, wid))
        {
            window.geometry = None;
            window.targets.clear();
            window.next_tab = 0;
            let now = Instant::now();
            let fullscreen = app.carried_fullscreen_owed(wid);
            let unlanded = fullscreen || app.restore_size_unlanded(wid);
            let owed = app
                .windows
                .get(&wid)
                .and_then(|state| state.restore_regrid_owed);
            let ready = owed.is_some_and(|owed| {
                now.saturating_duration_since(owed.closed) >= RESTORE_SHOW_CAP
                    || (!fullscreen
                        && now.saturating_duration_since(owed.since) >= RESTORE_SHOW_SETTLE)
            });
            if unlanded && !ready {
                let owed = owed.unwrap_or(RestoreRegridOwed {
                    closed: now,
                    since: now,
                    fullscreen,
                });
                let owed = if fullscreen {
                    RestoreRegridOwed { since: now, ..owed }
                } else {
                    owed
                };
                app.windows
                    .get_mut(&wid)
                    .expect("live re-grid window")
                    .restore_regrid_owed = Some(owed);
                let deadline =
                    (owed.since + RESTORE_SHOW_SETTLE).min(owed.closed + RESTORE_SHOW_CAP);
                self.pending.push_front(window);
                return Step::Wait(deadline);
            }
            let mut state = app.windows.get_mut(&wid).expect("live re-grid window");
            state.restore_regrid_owed = None;
            state.restore_maximize_asked = false;
            // Marked only: the park pays it once the drain ends (`App::settle_dock_fall`).
            state.dock.restore_settled();
            drop(state);
            window.geometry = Geometry::read(app, wid);
        }
        if let Some(target) = window.targets.pop_front() {
            app.resize_restored_target(wid, target);
            self.pending.push_front(window);
            return Step::Advanced;
        }
        let state = app.windows.get(&wid).expect("live re-grid window");
        if let Some(tab) = state.tab_set.tab_at(window.next_tab) {
            let plan = app.plan_tab(tab, state.rows.max(1), state.cols.max(1));
            window.next_tab += 1;
            window.targets = plan
                .leaves
                .into_iter()
                .map(|leaf| {
                    let (rows, cols) = App::leaf_grid_cells(&leaf.rect);
                    PlannedPaneResize {
                        tab: plan.tab,
                        view: leaf.view,
                        rows,
                        cols,
                    }
                })
                .collect();
            self.pending.push_front(window);
        } else {
            self.completed.push(window);
        }
        Step::Advanced
    }
}
