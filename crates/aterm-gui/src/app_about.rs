// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dedicated About tab placement; Settings keeps its own views and drafts.

use crate::native_app::{AppKind, AppViewState, NativeApp};
use crate::tab_model::{AppInstanceId, TabId, View, ViewId};
use crate::{App, WindowId};

impl App {
    pub(crate) fn open_about_tab(&mut self) -> bool {
        self.frontmost_window
            .is_some_and(|window| self.open_about_tab_in_window(window))
    }

    pub(crate) fn open_about_tab_in_window(&mut self, wid: WindowId) -> bool {
        let existing = self.windows.get(&wid).and_then(|window| {
            window.tab_set.tabs().iter().find_map(|tab| {
                let mut found = None;
                tab.root.any_leaf(&mut |&view| {
                    let Some(View::Native(native)) = self.view_store.get(view) else {
                        return false;
                    };
                    if self
                        .native_runtime
                        .app(native.instance)
                        .is_some_and(|app| app.kind() == AppKind::About)
                    {
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
            Some(existing) => existing,
            None => match self.stage_about_tab(wid) {
                Ok(placed) => placed,
                Err(_) => return false,
            },
        };
        let Some(window) = self.windows.get_mut(&wid) else {
            return false;
        };
        if !window.tab_set.switch_to(tab)
            || !window
                .tab_set
                .active_mut()
                .is_some_and(|tab| tab.set_focus(view))
        {
            return false;
        }
        crate::app_tabs::cancel_pending_close(window);
        window.last_present = None;
        self.resync_active_or_window(wid);
        self.sync_active_session();
        if let Some(window) = self
            .windows
            .get(&wid)
            .and_then(|window| window.os_window.as_ref())
        {
            window.focus_window();
            window.request_redraw();
        }
        true
    }

    /// Restore mints a fresh view even when the process already hosts About.
    pub(crate) fn stage_about_tab(
        &mut self,
        wid: WindowId,
    ) -> Result<(TabId, ViewId), crate::app_tabs::NativeTabInstallError> {
        let state = AppViewState::About(Box::default());
        let presentation = crate::tab_model::TabPresentation {
            title: "About aterm".into(),
            icon: Some(crate::tab_model::TabIconKind::About),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some("About aterm".into()),
        };
        if let Some(instance) = self.native_runtime.instance_by_kind(AppKind::About) {
            self.install_native_tab(wid, instance, state, presentation)
        } else {
            self.install_new_native_tab(
                wid,
                NativeApp::About(Box::new(crate::native_about::AboutApp::new())),
                state,
                presentation,
            )
            .map(|(_, tab, view)| (tab, view))
        }
    }

    pub(crate) fn open_about_from_menu(&mut self) {
        if !self.open_about_tab() {
            self.pending_action_refusal = Some("About could not be opened in this window".into());
        }
    }

    /// Compatibility inspection describes only the front window's visible About.
    pub(crate) fn front_about_view(&self) -> Option<(WindowId, AppInstanceId, ViewId)> {
        let wid = self.frontmost_window?;
        let (instance, view) = self.active_native_view(wid)?;
        (self.native_runtime.app(instance)?.kind() == AppKind::About)
            .then_some((wid, instance, view))
    }

    pub(crate) fn close_about_tabs(&mut self) {
        let mut targets = Vec::new();
        for (&wid, window) in &self.windows {
            for tab in window.tab_set.tabs() {
                tab.root.visit(&mut |&view| {
                    if let Some(View::Native(native)) = self.view_store.get(view)
                        && self
                            .native_runtime
                            .app(native.instance)
                            .is_some_and(|app| app.kind() == AppKind::About)
                    {
                        targets.push((wid, tab.id, view));
                    }
                });
            }
        }
        for (wid, tab, view) in targets {
            let _ = self.close_view_keeping_front(wid, tab, view);
        }
    }
}

#[cfg(test)]
#[path = "app_about_tests.rs"]
mod tests;
