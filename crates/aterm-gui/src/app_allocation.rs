// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Placement and publication of the read-only Allocation application.

use crate::native_app::{AppKind, AppViewState, DamageRegion, NativeApp};
use crate::tab_model::{TabId, View, ViewId};
use crate::{App, WindowId};

impl App {
    /// Whether an Allocation view is on screen ([`Self::view_on_screen`]).
    pub(crate) fn allocation_visible(&self) -> bool {
        self.view_on_screen(|view| {
            matches!(self.view_store.get(view), Some(View::Native(native))
                if self.native_runtime.app(native.instance).is_some_and(|app| app.kind() == AppKind::Allocation))
        })
    }

    pub(crate) fn open_allocation(&mut self, split: bool) -> Result<(), String> {
        let wid = self.frontmost_window.ok_or("No window for Allocation")?;
        // Beside a lone terminal only. A native view keeps its window's whole view, the one the
        // window's least size is measured for: split, a focused Settings stood in half its least
        // width, and a shared session is never split.
        let lone_terminal = self
            .focused_session_id(wid)
            .is_some_and(|session| self.pool.views(session).is_none_or(|views| views <= 1));
        let split = split
            && lone_terminal
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
            let existing = self.windows.get(&wid).and_then(|window| {
                window.tab_set.tabs().iter().find_map(|tab| {
                    let mut found = None;
                    tab.root.any_leaf(&mut |&view| {
                        if matches!(
                            self.view_store.get(view),
                            Some(View::Native(native)) if native.instance == instance
                        ) {
                            found = Some((tab.id, view));
                            true
                        } else {
                            false
                        }
                    });
                    found
                })
            });
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
        #[cfg(target_os = "macos")]
        let host = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .allocation_monitor();
        #[cfg(target_os = "macos")]
        let monitor = host.take_view();
        let Some(instance) = self.native_runtime.instance_by_kind(AppKind::Allocation) else {
            return;
        };
        #[cfg(target_os = "macos")]
        self.native_runtime
            .publish_allocation_monitor(instance, monitor);
        self.native_runtime.publish_allocation(
            instance,
            self.vitals.cpu_leases.clone(),
            self.vitals.snapshot.clone(),
            self.vitals.sampled_at,
        );
        let mut visible = Vec::new();
        for (&wid, window) in &self.windows {
            if window.occluded {
                continue;
            }
            if let Some(tab) = window.tab_set.active() {
                tab.root.visit(&mut |&view| {
                    if (!tab.zoomed || tab.focus == view)
                        && matches!(
                            self.view_store.get(view),
                            Some(View::Native(native)) if native.instance == instance
                        )
                    {
                        visible.push((wid, view));
                    }
                });
            }
        }
        for (wid, view) in visible {
            self.invalidate_native_view_cache(wid, view, DamageRegion::All);
            if let Some(window) = self.windows.get(&wid) {
                window.request_redraw();
            }
        }
    }
}

#[cfg(test)]
#[path = "app_allocation_tests.rs"]
mod tests;
