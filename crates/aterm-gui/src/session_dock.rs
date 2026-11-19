// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session organizer projects live terminal views. A borrowed preview takes one
//! reference to the PTY in the other window and releases it when the preview leaves.

use crate::app_navigator::SessionPreview;
use crate::tab_model::{TabId, View, ViewId};
use crate::{App, WindowId};

/// A strip drop transfers its window's tab. A hidden row already belongs to the
/// organizer; reordering it among hidden rows leaves its other window open.
enum DragOrigin {
    Strip(WindowId),
    Row(WindowId),
}

impl App {
    /// Prefer the receiving window's real tab, then a real tab in another window.
    fn dock_source(&self, wid: WindowId, session: u64) -> Option<(WindowId, TabId, ViewId, bool)> {
        std::iter::once(wid)
            .chain(self.windows.keys().copied().filter(|id| *id != wid))
            .find_map(|id| {
                let ws = self.windows.get(&id)?;
                ws.tab_set.tabs().iter().find_map(|tab| {
                    if ws
                        .dock
                        .preview
                        .is_some_and(|p| p.borrowed && p.tab == tab.id)
                        && self.pool.views(session).is_some_and(|views| views > 1)
                    {
                        return None;
                    }
                    let mut found = None;
                    tab.root.any_leaf(&mut |&view| {
                        if self
                            .view_store
                            .get(view)
                            .copied()
                            .and_then(View::terminal_session)
                            == Some(session)
                        {
                            found = Some(view);
                            true
                        } else {
                            false
                        }
                    });
                    Some((id, tab.id, found?, tab.parked))
                })
            })
    }

    pub(crate) fn select_dock_session(&mut self, wid: WindowId, session: u64) {
        let Some((source, tab, view, parked)) = self.dock_source(wid, session) else {
            return;
        };
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        if ws
            .dock
            .preview
            .is_some_and(|p| p.session == session && ws.tab_set.active_id() == Some(p.tab))
        {
            return;
        }
        let anchor = ws
            .dock
            .preview
            .and_then(|p| p.anchor)
            .filter(|id| ws.strip_index_of(*id).is_some())
            .or_else(|| {
                ws.tab_set
                    .active_id()
                    .filter(|id| ws.strip_index_of(*id).is_some())
            })
            .or_else(|| ws.strip_tabs().next().map(|(_, tab)| tab.id));
        // Switch away from any borrowed view before installing a replacement.
        let leave_borrowed = ws.dock.preview.filter(|p| p.borrowed).and_then(|old| {
            anchor
                .and_then(|id| self.tab_index_for_id(wid, id))
                .or_else(|| ws.tab_set.tabs().iter().position(|t| t.id != old.tab))
        });
        if let Some(index) = leave_borrowed {
            self.switch_tab_in(wid, index);
        }
        if source == wid {
            let Some(index) = self.tab_index_for_id(wid, tab) else {
                return;
            };
            if parked && let Some(mut ws) = self.windows.get_mut(&wid) {
                ws.dock.preview = Some(SessionPreview {
                    tab,
                    session,
                    borrowed: false,
                    anchor,
                });
            }
            self.switch_tab_in(wid, index);
            self.focus_session_pane(wid, session, view);
            self.navigator_mark_structure();
        } else {
            let layout = crate::pane::PaneTree::new(session);
            let Ok(mut mirror) =
                crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout)
            else {
                return;
            };
            mirror.parked = true;
            let tab = mirror.id;
            self.pool.attach(session);
            let mut ws = self.windows.get_mut(&wid).expect("receiving window exists");
            crate::app_tabs::land_terminal_tab(&mut ws, &self.view_store, layout, mirror)
                .expect("fresh tab id");
            ws.dock.preview = Some(SessionPreview {
                tab,
                session,
                borrowed: true,
                anchor,
            });
            drop(ws);
            self.resync_active_or_window(wid);
        }
    }

    /// Leave the live session parked. With no shown view left, the navigator itself
    /// gives the person a place to choose another session.
    pub(crate) fn dismiss_session_preview(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let Some(preview) = ws.active_session_preview() else {
            return false;
        };
        let next = preview
            .anchor
            .and_then(|id| ws.tab_set.shown().iter().position(|tab| tab.id == id))
            .or_else(|| ws.strip_tabs().next().map(|(index, _)| index))
            .or_else(|| (!ws.tab_set.shown().is_empty()).then_some(0));
        if let Some(index) = next {
            self.switch_tab_in(wid, index);
        } else if let Err(why) = self.open_navigator_in(wid, false) {
            aterm_log::info!("navigator: could not leave preview: {why}");
        }
        true
    }

    /// Drop a borrowed projection only while another view still owns the live session.
    pub(crate) fn settle_session_preview(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let Some(preview) = ws.dock.preview else {
            // A restored or externally selected hidden human tab has the same
            // input contract as a row selected directly in the organizer.
            if let Some(tab) = ws.tab_set.active().filter(|t| t.parked && !t.ephemeral)
                && let Some(session) = self
                    .view_store
                    .get(tab.focus)
                    .copied()
                    .and_then(View::terminal_session)
            {
                let restored = SessionPreview {
                    tab: tab.id,
                    session,
                    borrowed: false,
                    anchor: ws.strip_tabs().next().map(|(_, tab)| tab.id),
                };
                self.windows
                    .get_mut(&wid)
                    .expect("an active window")
                    .dock
                    .preview = Some(restored);
            }
            return;
        };
        if ws.tab_set.active_id() == Some(preview.tab)
            && ws.tab_set.get(preview.tab).is_some_and(|t| t.parked)
        {
            return;
        }
        let mut ws = self.windows.get_mut(&wid).expect("a window with a preview");
        ws.dock.preview = None;
        if !preview.borrowed || self.pool.views(preview.session).is_none_or(|v| v <= 1) {
            return;
        }
        let Some(index) =
            crate::app_tabs::terminal_projection_index(&ws.tab_set, &self.view_store, preview.tab)
        else {
            return;
        };
        ws.layouts.remove(index);
        crate::app_tabs::remove_terminal_projection(&mut ws, index);
        let tab = ws
            .tab_set
            .remove(preview.tab)
            .expect("projection has a tab");
        crate::app_tabs::align_terminal_projection_to_active(&mut ws, &self.view_store);
        drop(ws);
        self.remove_tab_views(&tab);
        self.detach_session_view(preview.session);
    }

    /// Keep the entire tab, including split siblings, and put its selected session at hand.
    pub(crate) fn keep_dock_session(&mut self, wid: WindowId, session: u64) -> bool {
        let Some((source, tab, view, parked)) = self.dock_source(wid, session) else {
            return false;
        };
        if source != wid && self.tab_home_window() != Some(wid) {
            return false;
        }
        if parked && !self.keep_tab_in(source, tab, session) {
            return false;
        }
        if source != wid && !self.drop_tab_on_strip(source, tab, wid, None) {
            return false;
        }
        let Some(index) = self.tab_index_for_id(wid, tab) else {
            return false;
        };
        self.switch_tab_in(wid, index);
        self.focus_session_pane(wid, session, view);
        true
    }

    fn drop_session_on_strip(
        &mut self,
        source: WindowId,
        tab: TabId,
        session: u64,
        target: WindowId,
        before: Option<TabId>,
    ) -> bool {
        let parked = self
            .windows
            .get(&source)
            .and_then(|ws| ws.tab_set.get(tab))
            .is_some_and(|t| t.parked);
        if source != target && self.tab_home_window() != Some(target) {
            return false;
        }
        if parked && !self.keep_tab_in(source, tab, session) {
            return false;
        }
        self.drop_tab_on_strip(source, tab, target, before)
    }

    fn dock_before(&self, wid: WindowId, session: u64) -> Option<TabId> {
        let (owner, tab, _, parked) = self.dock_source(wid, session)?;
        (owner == wid && !parked).then_some(tab)
    }

    pub(crate) fn drop_tab_in_session_dock(
        &mut self,
        source: WindowId,
        tab: TabId,
        target: WindowId,
        x: f64,
        y: f64,
    ) -> bool {
        let Some(session) = self
            .windows
            .get(&source)
            .and_then(|ws| ws.tab_set.get(tab))
            .and_then(|tab| self.view_store.get(tab.focus))
            .copied()
            .and_then(View::terminal_session)
        else {
            return false;
        };
        self.drop_session_in_dock(target, DragOrigin::Strip(source), tab, session, (x, y))
    }

    fn drop_session_in_dock(
        &mut self,
        target: WindowId,
        origin: DragOrigin,
        tab: TabId,
        session: u64,
        point: (f64, f64),
    ) -> bool {
        let source = match origin {
            DragOrigin::Strip(w) | DragOrigin::Row(w) => w,
        };
        let (x, y) = point;
        let Some((artifact, x, y)) = self.retained_native_leaf_at_pointer(target, x, y) else {
            return false;
        };
        if self.dock_view(target) != Some(artifact.view) {
            return false;
        }
        let Some(crate::native_app::AppViewState::Navigator(state)) =
            self.native_runtime.view_state(artifact.view)
        else {
            return false;
        };
        let order = &state.sessions.order;
        let shown = state.sessions.shown;
        let slot = artifact
            .compiled
            .hit_test(x, y)
            .and_then(|hit| {
                let s = crate::navigator::row_session(hit.key.as_str())?;
                let index = order.iter().position(|id| *id == s)?;
                let after = y >= hit.rect.y + hit.rect.height / 2.0;
                Some((index < shown, index + usize::from(after)))
            })
            .or_else(|| {
                // The section labels also accept a drop when their group is empty.
                let heading = artifact
                    .compiled
                    .semantic(&crate::native_ui::UiKey::new("navigator/machine"))?;
                let title = artifact
                    .compiled
                    .semantic(&crate::native_ui::UiKey::new("navigator/title"))?;
                if y >= title.rect.y && y <= title.rect.y + title.rect.height {
                    Some((true, 0))
                } else if y >= heading.rect.y && y <= heading.rect.y + heading.rect.height {
                    Some((false, shown))
                } else {
                    None
                }
            });
        let Some((visible, slot)) = slot else {
            return false;
        };
        if visible {
            let before = order.get(slot).and_then(|s| self.dock_before(target, *s));
            self.drop_session_on_strip(source, tab, session, target, before)
        } else {
            let mut hidden = order[shown..].to_vec();
            let next = hidden.get(slot.saturating_sub(shown)).copied();
            if next != Some(session) {
                hidden.retain(|s| *s != session);
                let at = next
                    .and_then(|id| hidden.iter().position(|s| *s == id))
                    .unwrap_or(hidden.len());
                hidden.insert(at, session);
            }
            let next_local = next
                .and_then(|s| self.dock_source(target, s))
                .filter(|(owner, _, _, parked)| *owner == target && *parked)
                .map(|(_, tab, _, _)| tab);
            let transfer = source != target && matches!(origin, DragOrigin::Strip(_));
            if transfer && !self.drop_tab_on_strip(source, tab, target, None) {
                return false;
            }
            let Some(mut ws) = self.windows.get_mut(&target) else {
                return false;
            };
            if transfer || source == target {
                if ws.tab_set.get(tab).is_some_and(|t| !t.parked) {
                    if !crate::app_tabs::reorder_with_projection(
                        &mut ws,
                        &self.view_store,
                        tab,
                        |tabs| tabs.hide(tab),
                    ) {
                        return false;
                    }
                    if ws.tab_set.active_id() == Some(tab) {
                        let anchor = ws.strip_tabs().next_back().map(|(_, tab)| tab.id);
                        ws.dock.preview = Some(SessionPreview {
                            tab,
                            session,
                            anchor,
                            borrowed: false,
                        });
                    }
                }
                if next_local != Some(tab) {
                    crate::app_tabs::reorder_with_projection(
                        &mut ws,
                        &self.view_store,
                        tab,
                        |tabs| {
                            let Some(from) = tabs.tabs().iter().position(|t| t.id == tab) else {
                                return false;
                            };
                            let before = next_local
                                .and_then(|id| tabs.tabs().iter().position(|t| t.id == id))
                                .unwrap_or(tabs.len());
                            tabs.reorder(tab, before - usize::from(from < before))
                        },
                    );
                }
            }
            ws.dock.hidden_order = hidden;
            drop(ws);
            self.resync_active_or_window(target);
            true
        }
    }

    pub(crate) fn finish_dock_session_drag(
        &mut self,
        wid: WindowId,
        session: u64,
        point: (f64, f64),
    ) {
        #[cfg(target_os = "macos")]
        if let Some((target, crate::tab_drop::TabDropSite::Strip(before))) =
            self.current_native_tab_drop_site()
        {
            if let Some((source, tab, _, _)) = self.dock_source(wid, session) {
                self.drop_session_on_strip(source, tab, session, target, before);
            }
            return;
        }
        self.finish_dock_session_drag_on(wid, wid, session, point);
    }

    pub(crate) fn finish_dock_session_drag_on(
        &mut self,
        origin: WindowId,
        wid: WindowId,
        session: u64,
        point: (f64, f64),
    ) {
        let (x, y) = point;
        let Some((source, tab, _, _)) = self.dock_source(origin, session) else {
            return;
        };
        if let Some(col) = self.strip_col_at(wid, x, y) {
            let before = self.windows.get(&wid).and_then(|ws| {
                let hit = crate::tab_bar::hit_test(&ws.tab_segments, col)?;
                if let crate::tab_bar::TabHit::Select(i) = hit {
                    ws.painted_tab_index(i)
                        .and_then(|at| ws.tab_set.tab_at(at))
                        .map(|t| t.id)
                } else {
                    None
                }
            });
            self.drop_session_on_strip(source, tab, session, wid, before);
        } else {
            self.drop_session_in_dock(wid, DragOrigin::Row(source), tab, session, (x, y));
        }
    }
}
