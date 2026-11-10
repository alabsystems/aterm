// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the Workers tab (`docs/DESIGN-workers-tab-2026-11-02.md`): each window's parked
//! worker tabs, the [`crate::native_workers`] page that stands for them, and the rows it shows.

use std::time::{Duration, Instant};

use crate::App;
use crate::WindowId;
use crate::app_tabs::{align_terminal_projection_to_active, reorder_with_projection};
use crate::native_app::{AppViewState, DamageRegion, NativeApp};
use crate::native_workers::{
    ATERM_GROUP, WORKERS_TITLE, WorkerRow, WorkersAction, WorkersViewState, chip_title_of,
    result_to_read, row_state, why_words,
};
use crate::tab_model::{
    AppInstanceId, Tab, TabIconKind, TabId, TabIndicators, TabPresentation, View, ViewId,
};

/// The refusal of a close of the Workers tab while workers remain.
pub(crate) const WORKERS_TAB_STAYS: &str =
    "the Workers tab stays while workers remain: stop, close or keep them as tabs first";

/// The refusal of `open app workers` and its menu row where no window has a worker.
pub(crate) const NO_WORKERS: &str = "no workers are running";

/// How often a list on screen reads its workers' last words again.
const WORKERS_READ_STEP: Duration = Duration::from_secs(2);

/// Whether an empty Workers list is all the window holds: a list stands alone only as the one
/// shown tab with no worker behind it ([`App::settle_workers_tab`]).
#[cfg(target_os = "macos")]
pub(crate) fn workers_list_stands_alone(ws: &crate::WindowState) -> bool {
    ws.tab_set.home().is_some() && ws.tab_set.parked().is_empty() && ws.tab_set.shown_len() == 1
}

impl App {
    /// Whether the window's Workers tab is `tab` and workers remain for it to stand for.
    pub(crate) fn workers_tab_holds(&self, wid: WindowId, tab: TabId) -> bool {
        self.windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_set.home() == Some(tab) && !ws.tab_set.parked().is_empty())
    }

    /// Whether a leaf of `tab` has a task record.
    pub(crate) fn tab_has_task(&self, tab: &Tab) -> bool {
        tab.root.any_leaf(&mut |view| {
            self.view_store
                .get(*view)
                .copied()
                .and_then(View::terminal_session)
                .and_then(|session| self.pool.get(session))
                .is_some_and(|session| {
                    session
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .task()
                        .is_some()
                })
        })
    }

    /// Whether window `wid` shows a parked worker in place of its list.
    pub(crate) fn worker_on_show(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_set.active_is_parked())
    }

    /// Whether `session` is a leaf of a parked tab.
    pub(crate) fn session_is_parked(&self, session: u64) -> bool {
        self.windows.values().any(|ws| {
            ws.tab_set
                .parked()
                .iter()
                .any(|tab| self.tab_holds_session(tab, session))
        })
    }

    /// Whether a leaf of `tab` shows `session`.
    pub(crate) fn tab_holds_session(&self, tab: &Tab, session: u64) -> bool {
        tab.root.any_leaf(&mut |view| {
            self.view_store
                .get(*view)
                .copied()
                .and_then(View::terminal_session)
                == Some(session)
        })
    }

    /// Open the window's Workers tab when it gets its first parked tab, behind what it shows, and
    /// remove it once its last goes and no Finished row of the window's is left
    /// ([`crate::retired_workers`]). Where it is the window's only tab it stays, saying no workers
    /// are running: a worker's end never takes a window away, and the person closes it.
    pub(crate) fn settle_workers_tab(&mut self, wid: WindowId) {
        let Some((parked, home, shown)) = self.windows.get(&wid).map(|ws| {
            (
                ws.tab_set.parked().len(),
                ws.tab_set.home(),
                ws.tab_set.shown_len(),
            )
        }) else {
            return;
        };
        let finished = self.finished_rows_of(wid).len();
        // Reading rows takes each worker's terminal lock, and a sync comes with every switch and
        // resize: a list behind a tab reads as it comes to the front, when a task moves, or when
        // which sessions it stands for moved, which its chip's marks and tooltip also say.
        match (parked, finished, home) {
            (0, 0, Some(home)) if shown > 1 => self.retire_workers_tab(wid, home),
            (1.., _, None) => {
                if self.install_workers_tab(wid) {
                    self.publish_workers(wid);
                }
            }
            (_, _, Some(_)) => {
                if self.workers_list_on_screen(wid) || self.worker_rows_stale(wid) {
                    self.publish_workers(wid);
                }
            }
            (0, _, None) => {}
        }
    }

    /// Window `wid`'s retired workers ([`crate::retired_workers`]), oldest first: its Workers
    /// page's Finished rows, each with the local id its row is keyed by.
    pub(crate) fn finished_rows_of(
        &self,
        wid: WindowId,
    ) -> Vec<(u64, crate::retired_workers::Retired)> {
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .retired_workers()
            .listed_on(wid)
            .map(|(local, row)| (local, row.clone()))
            .collect()
    }

    fn install_workers_tab(&mut self, wid: WindowId) -> bool {
        let keep = self.windows.get(&wid).and_then(|ws| ws.tab_set.active_id());
        keep.is_some() && self.install_workers_tab_keeping(wid, keep)
    }

    /// Opens the window's list and makes it the window's home for one. With `keep` it opens behind
    /// that tab; with none it comes to the front, as a restore of a lone list wants.
    pub(crate) fn install_workers_tab_keeping(
        &mut self,
        wid: WindowId,
        keep: Option<TabId>,
    ) -> bool {
        let presentation = TabPresentation {
            title: WORKERS_TITLE.to_owned(),
            icon: Some(TabIconKind::Workers),
            indicators: TabIndicators::default(),
            conn: None,
            closable: false,
            tooltip: Some(WORKERS_TITLE.to_owned()),
        };
        let installed = self.install_new_native_tab_keeping(
            wid,
            NativeApp::Workers(Box::default()),
            AppViewState::Workers(Box::<WorkersViewState>::default()),
            presentation,
            keep,
        );
        let tab = match installed {
            Ok((_, tab, _)) => tab,
            Err(error) => {
                aterm_log::warn!("workers tab: could not open one: {error:?}");
                return false;
            }
        };
        self.windows
            .get_mut(&wid)
            .is_some_and(|ws| ws.tab_set.set_home(Some(tab)))
    }

    fn retire_workers_tab(&mut self, wid: WindowId, home: TabId) {
        let views = &self.view_store;
        let Some(tab) = self.windows.get_mut(&wid).and_then(|ws| {
            let tab = ws.tab_set.remove(home)?;
            align_terminal_projection_to_active(ws, views);
            Some(tab)
        }) else {
            return;
        };
        self.remove_tab_views(&tab);
    }

    /// The window's Workers page: its tab, view and instance.
    pub(crate) fn workers_page(&self, wid: WindowId) -> Option<(TabId, ViewId, AppInstanceId)> {
        let ws = self.windows.get(&wid)?;
        let tab = ws.tab_set.get(ws.tab_set.home()?)?;
        let View::Native(native) = self.view_store.get(tab.focus).copied()? else {
            return None;
        };
        Some((tab.id, tab.focus, native.instance))
    }

    /// Each terminal leaf of the window's parked tabs, in tab order: a tab split by hand keeps a
    /// row for its worker and gains one for the new pane.
    fn parked_sessions(&self, wid: WindowId) -> Vec<u64> {
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let mut sessions = Vec::new();
        for tab in ws.tab_set.parked() {
            tab.root.visit(&mut |&view| {
                if let Some(session) = self
                    .view_store
                    .get(view)
                    .copied()
                    .and_then(View::terminal_session)
                {
                    sessions.push(session);
                }
            });
        }
        sessions
    }

    /// Whether the window's page lists other sessions than its parked tabs hold now, and its
    /// Finished rows after them.
    fn worker_rows_stale(&self, wid: WindowId) -> bool {
        self.workers_page(wid).is_some_and(|(_, _, instance)| {
            let mut listed = self.parked_sessions(wid);
            listed.extend(self.finished_rows_of(wid).iter().map(|(local, _)| *local));
            self.native_runtime.workers_listed(instance) != listed
        })
    }

    /// The window's parked workers as the page shows them, then its Finished rows.
    pub(crate) fn worker_rows(&self, wid: WindowId) -> Vec<WorkerRow> {
        let mut rows = self.live_worker_rows(wid);
        rows.extend(
            self.finished_rows_of(wid)
                .iter()
                .map(|(local, row)| finished_worker_row(*local, row)),
        );
        rows
    }

    /// The window's parked workers as the page shows them.
    fn live_worker_rows(&self, wid: WindowId) -> Vec<WorkerRow> {
        let sessions = self.parked_sessions(wid);
        let hosting = self.harness_hosting();
        // Off the store's lock before a row is read: a row reads the store again for its manager,
        // and a reader behind a waiting writer behind the guard held here would never wake.
        let handles: Vec<crate::session_store::SessionHandle> = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            sessions
                .iter()
                .filter_map(|session| store.by_local(*session).cloned())
                .collect()
        };
        handles
            .iter()
            .map(|handle| self.worker_row(handle, &hosting))
            .collect()
    }

    /// One row, from what `overview_chip` says of the session so the two surfaces cannot word it
    /// differently, and what only a row shows.
    pub(crate) fn worker_row(
        &self,
        handle: &crate::session_store::SessionHandle,
        hosting: &crate::harness_reach::HarnessHosting,
    ) -> WorkerRow {
        let chip = self.overview_chip(handle, hosting);
        let record = handle
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .task()
            .cloned();
        let state = row_state(record.as_ref(), chip.need.is_some());
        let manager = record
            .as_ref()
            .map(|task| task.from.clone())
            .filter(|from| from != crate::task_record::FROM_CTL);
        let group = manager.as_deref().map_or_else(
            || ATERM_GROUP.to_owned(),
            |from| self.worker_group_label(from),
        );
        let elapsed = record.as_ref().map_or_else(
            || chip.doing.clone(),
            |task| {
                let now = aterm_time::unix_millis().unwrap_or(0);
                if state.concluded() {
                    let age = crate::app_overview::coarse_age(task.since_ms());
                    if age == crate::app_overview::coarse_age(0) {
                        age
                    } else {
                        format!("{age} ago")
                    }
                } else {
                    crate::app_overview::coarse_age(now.saturating_sub(task.started_unix_ms))
                }
            },
        );
        // Once its agent has gone, a worker's tab title is its shell's folder: the row names what
        // it ran instead (its agent, or a program worker's program), unless a role or a pinned
        // title names the session.
        let gone = record
            .as_ref()
            .filter(|task| crate::native_workers::agent_gone(task))
            .and_then(crate::app_take_over::worker_label);
        let title = match gone {
            Some(name) if self.named_label_of_session(handle.local_id).is_none() => name,
            _ => chip.label,
        };
        WorkerRow {
            session: handle.local_id,
            group,
            manager,
            state,
            title,
            agent: record
                .as_ref()
                .and_then(|task| task.agent)
                .map(|program| program.name().to_owned()),
            elapsed,
            last_words: crate::survey::last_words(handle, state.concluded()),
            why: record.as_ref().and_then(why_words),
            has_result: record.as_ref().is_some_and(result_to_read),
            stoppable: chip.stoppable,
            finished: false,
        }
    }

    /// The name of the group a manager's workers sit under, as Overview's Manages names it.
    fn worker_group_label(&self, from: &str) -> String {
        let local = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(&aterm_session::SessionId::new(from))
            .map(|handle| handle.local_id);
        match local {
            Some(local) => self.tab_label_of_session(local),
            None => format!("{} (not here)", aterm_messages::presence::short_sid(from)),
        }
    }

    /// Give the window's page the rows as they stand, repainting only what moved.
    pub(crate) fn publish_workers(&mut self, wid: WindowId) {
        let Some((_, view, instance)) = self.workers_page(wid) else {
            return;
        };
        let rows = self.worker_rows(wid);
        if !self.native_runtime.publish_workers(instance, view, rows) {
            return;
        }
        self.invalidate_native_view_cache(wid, view, DamageRegion::All);
        self.refresh_native_presentation(wid, instance, view);
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
    }

    /// Every window's page: a task, a team or a label moved.
    pub(crate) fn publish_workers_all(&mut self) {
        let windows: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.tab_set.home().is_some())
            .map(|(wid, _)| *wid)
            .collect();
        for wid in windows {
            self.publish_workers(wid);
        }
    }

    pub(crate) fn workers_list_on_screen(&self, wid: WindowId) -> bool {
        self.windows.get(&wid).is_some_and(|ws| {
            ws.tab_set.home().is_some() && ws.tab_set.home() == ws.tab_set.active_id()
        })
    }

    /// A list on screen reads its workers again: their last words and their ages. One behind a
    /// tab reads when something moves and when it comes to the front.
    pub(crate) fn tick_workers(&mut self, now: Instant) {
        #[cfg(any(test, debug_assertions))]
        self.land_workers_fixture();
        if self
            .workers_read_at
            .is_some_and(|at| now.saturating_duration_since(at) < WORKERS_READ_STEP)
        {
            return;
        }
        let shown: Vec<WindowId> = self
            .windows
            .keys()
            .copied()
            .filter(|wid| self.workers_list_on_screen(*wid))
            .collect();
        if shown.is_empty() {
            self.workers_read_at = None;
            return;
        }
        self.workers_read_at = Some(now);
        for wid in shown {
            self.publish_workers(wid);
        }
    }

    /// The next read [`Self::tick_workers`] owes a list on screen.
    pub(crate) fn workers_wake(&self) -> Option<Instant> {
        self.workers_read_at
            .map(|at| at + WORKERS_READ_STEP)
            .or_else(|| {
                self.windows
                    .keys()
                    .any(|wid| self.workers_list_on_screen(*wid))
                    .then(Instant::now)
            })
    }

    /// Hand off what a press on window `wid`'s `instance` page asked for.
    pub(crate) fn drain_workers_actions(&mut self, wid: WindowId, instance: AppInstanceId) {
        while let Some(action) = self.native_runtime.take_workers_action(instance) {
            self.run_workers_action(wid, action);
        }
    }

    fn run_workers_action(&mut self, wid: WindowId, action: WorkersAction) {
        match action {
            // A Finished row has no tab to open: its press reads what it left.
            WorkersAction::Open(session) if self.finished_row(session).is_some() => {
                if let Err(why) = self.read_result(wid, session) {
                    aterm_log::info!("workers: open finished {session}: {why}");
                }
            }
            WorkersAction::Open(session) => {
                if !self.focus_session_preferring(Some(wid), session) {
                    aterm_log::info!("workers: session {session} is gone");
                }
            }
            WorkersAction::Stop(session) => {
                let peer = self.session_id_text(session);
                let tab = self.windows.get(&wid).and_then(|ws| ws.tab_set.home());
                if let (Some(peer), Some(tab)) = (peer, tab) {
                    self.dispatch_tab_menu_connection(
                        wid,
                        tab,
                        &aterm_session::SessionId::new(&peer),
                        crate::session_chrome::ConnVerb::StopWorker,
                    );
                }
            }
            WorkersAction::ReadResult(session) => {
                if let Err(why) = self.read_result(wid, session) {
                    aterm_log::info!("workers: read result {session}: {why}");
                }
            }
            WorkersAction::Keep(session) => {
                self.keep_as_tab(session);
            }
            WorkersAction::Dismiss(session) => self.dismiss_finished(session),
            WorkersAction::ClearFinished => self.clear_finished(wid),
        }
    }

    /// Keep as Tab: `session`'s worker becomes the person's tab ([`crate::tab_model::TabSet::keep`]):
    /// it leaves the parked tabs for the strip's end, keeping what the window shows, and loses its
    /// task-owned mark, so a claim task's execution budget no longer applies to it. Returns
    /// whether a tab was kept.
    pub(crate) fn keep_as_tab(&mut self, session: u64) -> bool {
        let found = self.windows.iter().find_map(|(wid, ws)| {
            ws.tab_set
                .tabs()
                .iter()
                .find(|tab| (tab.parked || tab.ephemeral) && self.tab_holds_session(tab, session))
                .map(|tab| (*wid, tab.id))
        });
        let Some((wid, tab)) = found else {
            return false;
        };
        // Each session the kept tab holds (a worker's split panes too) is the person's now.
        let mut held = Vec::new();
        if let Some(kept) = self.windows.get(&wid).and_then(|ws| ws.tab_set.get(tab)) {
            kept.root.visit(&mut |&view| {
                if let Some(crate::tab_model::View::Terminal(terminal)) =
                    self.view_store.get(view).copied()
                {
                    held.push(terminal.session);
                }
            });
        }
        let views = &self.view_store;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        if !reorder_with_projection(ws, views, tab, |tabs| tabs.keep(tab).is_some()) {
            return false;
        }
        let sid = self.session_id_text(session).unwrap_or_default();
        aterm_log::info!(
            "workers: @{sid} ({}) kept as a person's tab: its task no longer owns the tab",
            self.tab_label_of_session(session)
        );
        self.note_tab_keep(&held);
        self.session_chrome.remove(&session);
        self.resync_active_or_window(wid);
        true
    }

    /// The Finished row ([`crate::retired_workers`]) of the worker whose session was `session`.
    pub(crate) fn finished_row(&self, session: u64) -> Option<crate::retired_workers::Retired> {
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .retired_workers()
            .by_local(session)
            .cloned()
    }

    /// Dismiss: the Finished row of `session` leaves its page. What it left stays, so its manager
    /// still reads it, until its row ages out ([`crate::retired_workers`]).
    pub(crate) fn dismiss_finished(&mut self, session: u64) {
        let unlisted = self
            .store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retired_workers_mut()
            .unlist_local(session);
        if let Some(wid) = unlisted
            && self.windows.contains_key(&wid)
        {
            self.settle_workers_tab(wid);
        }
    }

    /// A closed worker's row goes at a plain quit, its results with it. A seamless update's Commit
    /// never reaches here (the outgoing process ends at it): its manifest carried the rows, whose
    /// results the successor keeps. A candidate that ends before its Commit leaves both: it only
    /// adopted its predecessor's rows, and the predecessor still answers from those files.
    pub(crate) fn forget_retired_workers_at_quit(&self) {
        if self.incoming_handoff_pending {
            return;
        }
        let retired = self
            .store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retired_workers_mut()
            .drop_all();
        crate::retired_workers::forget_results(&retired);
    }

    /// Clear Finished: window `wid`'s Finished rows leave its page, what they left kept as
    /// Dismiss keeps it.
    pub(crate) fn clear_finished(&mut self, wid: WindowId) {
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retired_workers_mut()
            .unlist_window(wid);
        self.settle_workers_tab(wid);
    }

    /// Agents ▸ Workers… and `open app workers`: the front window's list, else the first window
    /// with one.
    pub(crate) fn open_workers_tab(&mut self) -> Result<(), String> {
        let wid = self
            .frontmost_window
            .into_iter()
            .chain(self.windows.keys().copied())
            .find(|wid| {
                self.windows
                    .get(wid)
                    .is_some_and(|ws| ws.tab_set.home().is_some())
            })
            .ok_or_else(|| NO_WORKERS.to_owned())?;
        let index = self.windows.get(&wid).and_then(|ws| {
            let home = ws.tab_set.home()?;
            ws.tab_set.tabs().iter().position(|tab| tab.id == home)
        });
        let Some(index) = index else {
            return Err(NO_WORKERS.to_owned());
        };
        self.switch_tab_in(wid, index);
        self.raise_window(wid);
        Ok(())
    }

    /// A restore of a window that showed its list: the list, opened again for the parked tabs that
    /// came back ([`Self::settle_workers_tab`]), is brought to the front, as the tab the person
    /// looked at is for any other window. Where no parked tab came back there is no list to show.
    pub(crate) fn bring_workers_list_front(&mut self, wid: WindowId) {
        self.settle_workers_tab(wid);
        let index = self.windows.get(&wid).and_then(|ws| {
            let home = ws.tab_set.home()?;
            ws.tab_set.tabs().iter().position(|tab| tab.id == home)
        });
        if let Some(index) = index {
            self.switch_tab_in(wid, index);
        }
    }

    /// Agents ▸ Workers… and its palette row.
    pub(crate) fn open_workers_from_menu(&mut self) {
        if let Err(why) = self.open_workers_tab() {
            self.pending_action_refusal = Some(why);
        }
    }

    /// What the chip of `tab` reads while the worker it stands for shows in place of the list.
    pub(crate) fn workers_chip_title(&self, ws: &crate::WindowState, tab: &Tab) -> Option<String> {
        if ws.tab_set.home() != Some(tab.id) {
            return None;
        }
        let shown = ws.tab_set.active().filter(|active| active.parked)?;
        let session = self
            .view_store
            .get(shown.focus)
            .copied()
            .and_then(View::terminal_session)?;
        Some(chip_title_of(&self.tab_label_of_session(session)))
    }

    /// What a surface calls `tab`, at canonical `index`: `Tab 2`, or for a worker, which has no
    /// chip for a number to name, `Workers ▸ <label>`. Reads the store: no caller holds its lock.
    pub(crate) fn tab_name(&self, tab: &Tab, index: usize) -> String {
        if !tab.parked {
            return format!("Tab {}", index + 1);
        }
        self.view_store
            .get(tab.focus)
            .copied()
            .and_then(View::terminal_session)
            .map_or_else(
                || WORKERS_TITLE.to_owned(),
                |session| chip_title_of(&self.tab_label_of_session(session)),
            )
    }

    /// The Workers tab's words on a close it refuses: to whoever asked, and to a person's own
    /// gesture on the glass, which no reply reaches.
    pub(crate) fn workers_tab_close_refused(&mut self) {
        self.pending_action_refusal = Some(WORKERS_TAB_STAYS.to_owned());
        if !self.person_gesture_refused() {
            self.post_message(crate::message_reporters::workers_tab_stays(
                WORKERS_TAB_STAYS,
            ));
        }
    }

    /// The chrome line that reads the parked tabs, as `toolbar-tabs` reads the shown ones: `first=`
    /// is the first one's canonical index, so `tab <index>` reaches each.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn workers_chrome_line(&self) -> Option<String> {
        let wid = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied())?;
        let ws = self.windows.get(&wid)?;
        let parked = ws.tab_set.parked();
        if parked.is_empty() {
            return None;
        }
        let first = ws.tab_set.shown_len();
        let selected = ws
            .tab_set
            .active_index()
            .filter(|index| *index >= first)
            .map_or_else(|| "none".to_owned(), |index| index.to_string());
        let rows = self.worker_rows(wid);
        let labels: Vec<String> = parked
            .iter()
            .map(|tab| {
                self.view_store
                    .get(tab.focus)
                    .copied()
                    .and_then(View::terminal_session)
                    .map_or_else(
                        || tab.presentation.title.clone(),
                        |session| self.tab_label_of_session(session),
                    )
            })
            .collect();
        let states: Vec<Vec<&'static str>> = parked
            .iter()
            .map(|tab| {
                let mut states = Vec::new();
                if ws.tab_set.active_id() == Some(tab.id) {
                    states.push("selected");
                }
                let mut level = aterm_messages::presence::ChipLevel::Off;
                tab.root.any_leaf(&mut |view| {
                    if let Some(slot) = self
                        .view_store
                        .get(*view)
                        .copied()
                        .and_then(View::terminal_session)
                        .and_then(|session| self.presence.slot(session))
                    {
                        level = level.max(slot.chip());
                    }
                    false
                });
                states.extend(level.chrome_states());
                states
            })
            .collect();
        let tooltips: Vec<Option<String>> = parked
            .iter()
            .map(|tab| {
                let session = self
                    .view_store
                    .get(tab.focus)
                    .copied()
                    .and_then(View::terminal_session);
                match session {
                    Some(session) => self.presence_hover(session, tab.presentation.tooltip.clone()),
                    None => tab.presentation.tooltip.clone(),
                }
            })
            .collect();
        Some(format!(
            "toolbar-workers first={first} selected={selected} summary={:?} labels={labels:?} states={states:?} tooltips={tooltips:?}",
            crate::native_workers::summary_words(&rows),
        ))
    }
}

/// A retired worker's row on its window's page, under Finished: what it was, how its task ended,
/// and when its tab closed.
fn finished_worker_row(session: u64, row: &crate::retired_workers::Retired) -> WorkerRow {
    let record = &row.record;
    let now = aterm_time::unix_millis().unwrap_or(0);
    WorkerRow {
        session,
        group: crate::native_workers::FINISHED_GROUP.to_owned(),
        manager: Some(record.from.clone()).filter(|from| from != crate::task_record::FROM_CTL),
        state: row_state(Some(record), false),
        title: row.title.clone(),
        agent: record.agent.map(|program| program.name().to_owned()),
        elapsed: format!(
            "closed {} ago",
            crate::app_overview::coarse_age(now.saturating_sub(row.closed_unix_ms))
        ),
        last_words: row.last_words.clone(),
        why: why_words(record),
        has_result: result_to_read(record),
        stoppable: false,
        finished: true,
    }
}

/// The dev seam that lands plain shells as parked workers with seeded task rows, so the page can
/// be judged in frames with no agent running. `list` leaves the list on screen; `worker` opens the
/// first working row in its place.
#[cfg(any(test, debug_assertions))]
const FIXTURE_SEAM: &str = "ATERM_DEV_WORKERS_FIXTURE";

/// One fixture row: `(role, under the front session rather than aterm, state, why, exit, result
/// bytes, agent, age in minutes, what its shell prints)`.
#[cfg(any(test, debug_assertions))]
type FixtureRow = (
    &'static str,
    bool,
    crate::task_record::TaskState,
    Option<&'static str>,
    Option<i32>,
    u64,
    Option<aterm_phase::Program>,
    u64,
    &'static str,
);

#[cfg(any(test, debug_assertions))]
fn fixture_rows() -> Vec<FixtureRow> {
    use crate::task_record::TaskState::{Done, Exited, Stalled, Working};
    use aterm_phase::Program::{Claude, Codex};
    vec![
        (
            "disk clean-up",
            false,
            Working,
            None,
            None,
            0,
            Some(Claude),
            4,
            "Reclaimed 18 GB from the shared build cache so far",
        ),
        (
            "credit claim",
            false,
            Done,
            Some("landed"),
            None,
            2_048,
            Some(Claude),
            27,
            "Claimed the weekly credit and recorded the receipt",
        ),
        (
            "render sweep",
            true,
            Working,
            None,
            None,
            0,
            Some(Claude),
            12,
            "Comparing the third theme's frames against the baseline",
        ),
        (
            "lab forever",
            true,
            Stalled,
            Some("startup"),
            None,
            0,
            Some(Codex),
            6,
            "Waiting for the composer",
        ),
        (
            "parity run",
            true,
            Exited,
            Some("exit"),
            Some(101),
            0,
            Some(Claude),
            41,
            "thread main panicked: the certificate does not check",
        ),
        (
            "docs prune",
            true,
            Done,
            Some("landed"),
            None,
            5_120,
            Some(Codex),
            63,
            "Pruned 41 files and pushed to main",
        ),
    ]
}

#[cfg(any(test, debug_assertions))]
impl App {
    /// Land the seam's rows once, on the front window.
    fn land_workers_fixture(&mut self) {
        if self.workers_fixture_landed {
            return;
        }
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let manager = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .and_then(|tab| self.view_store.get(tab.focus).copied())
            .and_then(View::terminal_session)
            .and_then(|session| self.session_id_text(session));
        let Some(manager) = manager else {
            return;
        };
        self.workers_fixture_landed = true;
        let Some(mode) = aterm_types::dev_seam!(FIXTURE_SEAM) else {
            return;
        };
        let open_one = mode.to_str() == Some("worker");
        let rows = fixture_rows();
        let last = rows.len() - 1;
        for (index, row) in rows.into_iter().enumerate() {
            let prints = row.8;
            let ask = crate::tab_spawn::SpawnAsk {
                exec_command: Some(
                    [
                        "/bin/sh",
                        "-c",
                        "printf '%s\\n' \"$0\"; exec /bin/cat",
                        prints,
                    ]
                    .map(std::ffi::OsString::from)
                    .to_vec(),
                ),
                task: Some(crate::task_launch::TaskSeed::new(
                    prints.as_bytes().to_vec(),
                )),
                background: true,
                unasked: true,
                ..crate::tab_spawn::SpawnAsk::default()
            };
            let manager = manager.clone();
            let then = crate::tab_spawn::TabSpawnThen::Run(Box::new(move |app, landed| {
                let Ok(sid) = landed else {
                    aterm_log::warn!("{FIXTURE_SEAM}: row {index} did not land");
                    return;
                };
                app.seed_workers_fixture_row(&sid, &manager, row);
                if !open_one && index == last {
                    let _ = app.open_workers_tab();
                }
                if open_one && index == 2 {
                    let local = app
                        .store
                        .read()
                        .unwrap_or_else(|p| p.into_inner())
                        .by_sid(&aterm_session::SessionId::new(&sid))
                        .map(|handle| handle.local_id);
                    if let Some(local) = local {
                        app.focus_session_preferring(Some(wid), local);
                    }
                }
                app.publish_workers_all();
            }));
            self.open_off_loop(wid, crate::tab_spawn::SpawnPlace::Tab, ask, then);
        }
    }

    fn seed_workers_fixture_row(&mut self, sid: &str, manager: &str, row: FixtureRow) {
        use crate::task_record::TaskMark;
        let (role, managed, state, why, exit, result_bytes, agent, minutes, prints) = row;
        let handle = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            store.by_sid(&aterm_session::SessionId::new(sid)).cloned()
        };
        let Some(handle) = handle else {
            return;
        };
        handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .role = Some(role.to_owned());
        let mut mark = TaskMark::new(state);
        mark.from = Some(if managed {
            manager.to_owned()
        } else {
            crate::task_record::FROM_CTL.to_owned()
        });
        mark.why = why;
        mark.exit = exit;
        mark.result_bytes = result_bytes;
        if result_bytes > 0
            && let Some(folder) = crate::task_record::tasks_dir()
            && std::fs::create_dir_all(&folder).is_ok()
        {
            // A real snapshot, so Read Result's document and the `result` verb read one body.
            let stem = format!("fixture-{}", handle.local_id);
            let body = format!("# {role}\n\nThe fixture's result: {prints}.\n");
            if let Ok(snapshot) = aterm_agent::harness::result::snapshot_screen(
                &folder,
                &stem,
                "fixture",
                &body,
                aterm_agent::harness::result::DoneBy::Turn,
            ) {
                mark.result_bytes = snapshot.bytes;
                mark.result_path = Some(snapshot.path);
            }
        }
        if crate::task_record::mark_session_task_within(
            &handle.ctx,
            handle.local_id,
            &mark,
            handle.ctx.fabric.fence_wait(),
        )
        .is_err()
        {
            return;
        }
        let mut timeline = handle
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(program) = agent {
            timeline.set_task_worker(program, None);
        }
        if let Some(mut task) = timeline.task().cloned() {
            let age = Duration::from_secs(minutes * 60);
            let age_ms = minutes * 60_000;
            task.started_unix_ms = task.started_unix_ms.saturating_sub(age_ms);
            task.started = task.started.checked_sub(age).unwrap_or(task.started);
            if state != crate::task_record::TaskState::Working {
                let half = age / 2;
                task.since_unix_ms = task.since_unix_ms.saturating_sub(age_ms / 2);
                task.since = task.since.checked_sub(half).unwrap_or(task.since);
            }
            timeline.seed_task(task);
        }
    }
}
