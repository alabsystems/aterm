// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Host integration for the native Mail tab and its worker completions.
use crate::native_app::{AppIcon, AppKind};
use crate::native_mail::MailApp;

impl crate::App {
    pub(crate) fn open_mail_tab(&mut self) -> bool {
        let Some(wid) = self.frontmost_window else {
            return false;
        };
        let existing = self.windows.get(&wid).and_then(|window| {
            window.tab_set.tabs().iter().find_map(|tab| {
                tab.root.leaves().into_iter().find_map(|view| {
                    let crate::tab_model::View::Native(native) = self.view_store.get(view)? else {
                        return None;
                    };
                    (self.native_runtime.app(native.instance)?.kind() == AppKind::Mail)
                        .then_some((tab.id, view))
                })
            })
        });
        let placed = existing.map(Ok).unwrap_or_else(|| self.stage_mail_tab(wid));
        let Ok((tab, view)) = placed else {
            return false;
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
        self.request_redraw_all_windows();
        true
    }

    pub(crate) fn stage_mail_tab(
        &mut self,
        wid: crate::WindowId,
    ) -> Result<(crate::tab_model::TabId, crate::tab_model::ViewId), String> {
        let proxy = self.proxy.clone();
        let presentation = crate::tab_model::TabPresentation {
            title: "Mail".into(),
            icon: Some(AppIcon::Mail),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some("Mail".into()),
        };
        let state = crate::native_app::AppViewState::Mail(Box::default());
        let installed = if let Some(instance) = self.native_runtime.instance_by_kind(AppKind::Mail)
        {
            self.install_native_tab(wid, instance, state, presentation)
        } else {
            let model = MailApp::new(move || {
                if let Some(proxy) = &proxy {
                    let _ = proxy.send_event(crate::Wake::MailChanged);
                }
            })?;
            self.install_new_native_tab(
                wid,
                crate::native_app::NativeApp::Mail(Box::new(model)),
                state,
                presentation,
            )
            .map(|(_, tab, view)| (tab, view))
        };
        installed.map_err(|error| format!("Mail could not be opened: {error:?}"))
    }

    pub(crate) fn on_mail_changed(&mut self) {
        if self.native_runtime.poll_mail() {
            for window in self.windows.values_mut() {
                window.last_present = None;
            }
            self.request_redraw_all_windows();
        }
    }
}
