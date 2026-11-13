// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host integration for the Tables page ([`crate::native_tables`]): opening its one tab, and
//! taking its reader's answers when the reader wakes the loop.

use crate::native_app::{AppIcon, AppKind};
use crate::native_tables::{TABLES_TITLE, TablesApp};

impl crate::App {
    /// `aterm ctl open app tables`: focus the Tables tab, or open one in the front window.
    pub(crate) fn open_tables_tab(&mut self) -> Result<(), String> {
        self.open_tables_tab_at(None)
    }

    /// [`Self::open_tables_tab`], reading the registry at `paths` (this Mac's where `None`).
    pub(crate) fn open_tables_tab_at(
        &mut self,
        paths: Option<crate::tables_store::Paths>,
    ) -> Result<(), String> {
        let wid = self
            .frontmost_window
            .ok_or_else(|| "no window to open Tables in".to_string())?;
        let existing = self.windows.get(&wid).and_then(|window| {
            window.tab_set.tabs().iter().find_map(|tab| {
                let mut found = None;
                tab.root.any_leaf(&mut |&view| {
                    let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view)
                    else {
                        return false;
                    };
                    if self
                        .native_runtime
                        .app(native.instance)
                        .is_some_and(|app| app.kind() == AppKind::Tables)
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
            Some(placed) => placed,
            None => self.stage_tables_tab(wid, paths)?,
        };
        let window = self
            .windows
            .get_mut(&wid)
            .ok_or_else(|| "the window closed".to_string())?;
        if !window.tab_set.switch_to(tab)
            || !window
                .tab_set
                .active_mut()
                .is_some_and(|tab| tab.set_focus(view))
        {
            return Err("Tables could not be shown in the requesting window".into());
        }
        crate::app_tabs::cancel_pending_close(window);
        window.last_present = None;
        self.resync_active_or_window(wid);
        self.sync_active_session();
        self.request_redraw_all_windows();
        Ok(())
    }

    fn stage_tables_tab(
        &mut self,
        wid: crate::WindowId,
        paths: Option<crate::tables_store::Paths>,
    ) -> Result<(crate::tab_model::TabId, crate::tab_model::ViewId), String> {
        let proxy = self.proxy.clone();
        let presentation = crate::tab_model::TabPresentation {
            title: TABLES_TITLE.into(),
            icon: Some(AppIcon::Editor),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some(TABLES_TITLE.into()),
        };
        let state = crate::native_app::AppViewState::Tables(Box::default());
        let installed =
            if let Some(instance) = self.native_runtime.instance_by_kind(AppKind::Tables) {
                self.install_native_tab(wid, instance, state, presentation)
            } else {
                let model = TablesApp::new(paths, move || {
                    if let Some(proxy) = &proxy {
                        let _ = proxy.send_event(crate::Wake::TablesChanged);
                    }
                })?;
                self.install_new_native_tab(
                    wid,
                    crate::native_app::NativeApp::Tables(Box::new(model)),
                    state,
                    presentation,
                )
                .map(|(_, tab, view)| (tab, view))
            };
        installed.map_err(|error| format!("Tables could not be opened: {error:?}"))
    }

    pub(crate) fn on_tables_changed(&mut self) {
        if self.native_runtime.poll_tables() {
            for window in self.windows.values_mut() {
                window.last_present = None;
            }
            self.request_redraw_all_windows();
        }
    }
}
