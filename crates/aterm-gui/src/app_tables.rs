// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host integration for the Tables page ([`crate::native_tables`]): opening its one tab from
//! the menu, the palette, `aterm ctl open app tables` or a restore, and taking its reader's
//! answers when the reader wakes the loop.

use crate::native_app::{AppIcon, AppKind};
use crate::native_tables::{TABLES_TITLE, TablesApp};

impl crate::App {
    /// `aterm ctl open app tables`: focus the Tables tab, or open one in the front window.
    pub(crate) fn open_tables_tab(&mut self) -> Result<(), String> {
        self.open_tables_tab_at(None)
    }

    /// aterm ▸ Tables… and its palette row (`MenuAction::Tables`).
    pub(crate) fn open_tables_from_menu(&mut self) {
        if let Err(why) = self.open_app(crate::app_control::OpenRequest::Tables) {
            self.pending_action_refusal = Some(why);
        }
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
        let mut route_guard_window = self
            .windows
            .get_mut(&wid)
            .ok_or_else(|| "the window closed".to_string())?;
        let window = &mut **route_guard_window;
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
        drop(route_guard_window);
        self.resync_active_or_window(wid);
        self.sync_active_session();
        self.request_redraw_all_windows();
        Ok(())
    }

    /// Mint a view of the Tables page at `target`: a tab of a window, or the leaf a restore is
    /// building. One page serves every view.
    pub(crate) fn stage_tables_tab(
        &mut self,
        target: impl Into<crate::app_tabs::NativeInstallTarget>,
        paths: Option<crate::tables_store::Paths>,
    ) -> Result<(crate::tab_model::TabId, crate::tab_model::ViewId), String> {
        let target = target.into();
        let proxy = self.proxy.clone();
        let presentation = crate::tab_model::TabPresentation {
            title: TABLES_TITLE.into(),
            icon: Some(AppIcon::Tables),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some(TABLES_TITLE.into()),
        };
        let state = crate::native_app::AppViewState::Tables(Box::default());
        let installed =
            if let Some(instance) = self.native_runtime.instance_by_kind(AppKind::Tables) {
                self.install_native_tab(target, instance, state, presentation)
            } else {
                let model = TablesApp::new(paths, move || {
                    if let Some(proxy) = &proxy {
                        let _ = proxy.send_event(crate::Wake::TablesChanged);
                    }
                })?;
                self.install_new_native_tab(
                    target,
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
            for mut route_guard_window in self.windows.values_mut() {
                let window = &mut **route_guard_window;
                window.last_present = None;
            }
            self.request_redraw_all_windows();
        }
    }
}
