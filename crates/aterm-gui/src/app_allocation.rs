// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Placement and publication of the read-only Allocation application.

use crate::native_app::{AppKind, AppViewState, DamageRegion, NativeApp};
use crate::tab_model::{TabId, View, ViewId};
use crate::{App, WindowId};

impl App {
    pub(crate) fn allocation_visible(&self) -> bool {
        self.windows.values().any(|window| !window.occluded && window.tab_set.active().is_some_and(|tab| {
            tab.root.leaves().into_iter().any(|view| matches!(self.view_store.get(view), Some(View::Native(native))
                if self.native_runtime.app(native.instance).is_some_and(|app| app.kind() == AppKind::Allocation)
                && (!tab.zoomed || tab.focus == view)))
        }))
    }

    pub(crate) fn open_allocation(&mut self, split: bool) -> Result<(), String> {
        let wid = self.frontmost_window.ok_or("No window for Allocation")?;
        let shared_terminal = self
            .focused_session_id(wid)
            .is_some_and(|session| self.pool.views(session).is_some_and(|views| views > 1));
        let split = split
            && !shared_terminal
            && self.focused_pane_cells(wid).is_some_and(|(rows, cols)| {
                crate::pane::split_fits_in(crate::pane::SplitDir::Vertical, rows, cols)
            });
        let instance = match self.native_runtime.instance_by_kind(AppKind::Allocation) {
            Some(instance) => instance,
            None => self
                .native_runtime
                .insert_instance(NativeApp::Allocation(Box::default()))
                .map_err(|error| format!("Allocation could not open: {error:?}"))?,
        };
        if split {
            self.split_active_with_native(
                wid,
                crate::tab_model::SplitAxis::Horizontal,
                instance,
                AppViewState::Allocation(Box::default()),
            )
            .map_err(|error| format!("Allocation split could not open: {error:?}"))?;
        } else {
            let existing = self.windows.get(&wid).and_then(|window| window.tab_set.tabs().iter().find_map(|tab| {
                tab.root.leaves().into_iter().find_map(|view| matches!(self.view_store.get(view), Some(View::Native(native)) if native.instance == instance).then_some((tab.id, view)))
            }));
            let (tab, view) = match existing {
                Some(found) => found,
                None => self
                    .stage_allocation_tab(wid)
                    .map_err(|error| format!("Allocation could not open: {error:?}"))?,
            };
            let window = self
                .windows
                .get_mut(&wid)
                .ok_or("Allocation window closed")?;
            window.tab_set.switch_to(tab);
            if let Some(tab) = window.tab_set.active_mut() {
                tab.set_focus(view);
            }
            self.resync_active_or_window(wid);
        }
        let window = self
            .windows
            .get_mut(&wid)
            .ok_or("Allocation window closed")?;
        crate::app_tabs::cancel_pending_close(window);
        window.last_present = None;
        self.publish_allocation();
        self.sync_active_session();
        Ok(())
    }

    pub(crate) fn stage_allocation_tab(
        &mut self,
        wid: WindowId,
    ) -> Result<(TabId, ViewId), crate::app_tabs::NativeTabInstallError> {
        let state = AppViewState::Allocation(Box::default());
        let presentation = crate::tab_model::TabPresentation {
            title: "Allocation".into(),
            icon: Some(crate::tab_model::TabIconKind::SystemState),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some("Read-only resource allocation observations".into()),
        };
        if let Some(instance) = self.native_runtime.instance_by_kind(AppKind::Allocation) {
            self.install_native_tab(wid, instance, state, presentation)
        } else {
            self.install_new_native_tab(
                wid,
                NativeApp::Allocation(Box::default()),
                state,
                presentation,
            )
            .map(|(_, tab, view)| (tab, view))
        }
    }

    pub(crate) fn publish_allocation(&mut self) {
        let Some(instance) = self.native_runtime.instance_by_kind(AppKind::Allocation) else {
            return;
        };
        self.native_runtime.publish_allocation(
            instance,
            self.vitals.cpu_leases.clone(),
            self.vitals.snapshot.clone(),
            self.vitals.sampled_at,
        );
        let visible: Vec<_> = self.windows.iter().filter(|(_, window)| !window.occluded)
            .flat_map(|(wid, window)| window.tab_set.active().into_iter().flat_map(move |tab| {
                tab.root.leaves().into_iter().filter(move |view| !tab.zoomed || tab.focus == *view)
                    .map(move |view| (*wid, view))
            }))
            .filter(|(_, view)| matches!(self.view_store.get(*view), Some(View::Native(native)) if native.instance == instance))
            .collect();
        for (wid, view) in visible {
            self.invalidate_native_view_cache(wid, view, DamageRegion::All);
            if let Some(window) = self.windows.get(&wid) {
                window.request_redraw();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_open_cancels_a_deferred_last_tab_window_close() {
        for split in [false, true] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let terminal_view = app.windows[&wid].tab_set.active().unwrap().focus;
            let progress = {
                let _closing = crate::session_store::CloseAttribution::enter(
                    crate::session_store::ExitReason::CtlClose,
                    crate::session_store::ExitActor::Unknown,
                );
                app.close_session_by_id(0).unwrap()
            };
            assert_eq!(progress, crate::app_tabs::CloseProgress::Deferred(wid));
            assert!(app.windows[&wid].pending_close);
            assert!(app.windows[&wid].pending_close_attribution.is_some());
            app.open_allocation(split).unwrap();
            assert!(!app.windows[&wid].pending_close);
            assert!(app.windows[&wid].pending_close_attribution.is_none());
            assert!(app.pool.get(0).is_some());
            assert!(app.view_store.get(terminal_view).is_some());
            let (instance, _) = app.active_native_view(wid).unwrap();
            assert_eq!(
                app.native_runtime.app(instance).unwrap().kind(),
                AppKind::Allocation
            );
            assert!(app.structural_invariants_ok());
        }
    }

    #[test]
    fn allocation_split_preserves_terminal_identity_and_reports_its_real_grid() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let next = app.next_session_id;
        let original_tab = app.windows[&wid].tab_set.active_id();
        app.open_allocation(true).unwrap();
        assert_eq!(
            app.next_session_id, next,
            "a native split must not spawn a PTY"
        );
        assert_eq!(app.windows[&wid].tab_set.active_id(), original_tab);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        assert_eq!(plan.leaves.len(), 2);
        let terminal = plan
            .leaves
            .iter()
            .find_map(|leaf| {
                View::terminal_session(*app.view_store.get(leaf.view)?)
                    .map(|session| (leaf, session))
            })
            .unwrap();
        let session = app.pool.get(terminal.1).unwrap();
        let term = crate::term_lock(&session.term);
        assert_eq!(
            (term.rows(), term.cols()),
            (
                (terminal.0.rect.size.height.round() as u16).max(1),
                (terminal.0.rect.size.width.round() as u16).max(1)
            )
        );
        drop(term);
        app.open_allocation(false).unwrap();
        assert_eq!(app.windows[&wid].tab_set.active_id(), original_tab);
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn a_narrow_allocation_request_opens_a_tab_without_splitting_the_terminal() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows.get_mut(&wid).unwrap().cols = 20;
        let next = app.next_session_id;
        let old_tab = app.windows[&wid].tab_set.active_id().unwrap();
        app.open_allocation(true).unwrap();
        assert_eq!(app.next_session_id, next);
        assert_eq!(
            app.windows[&wid]
                .tab_set
                .get(old_tab)
                .unwrap()
                .root
                .leaves()
                .len(),
            1
        );
        assert_ne!(app.windows[&wid].tab_set.active_id(), Some(old_tab));
        assert!(app.structural_invariants_ok());
    }
}
