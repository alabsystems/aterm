// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet inventory placement and the handoff from a machine to its underlying table row.

use crate::native_app::{AppKind, AppViewState, NativeApp};
use crate::native_registry::{RegistryApp, RegistryViewState};
use crate::qos::Role;
use crate::tab_model::{AppInstanceId, TabId, TabPresentation, ViewId};
use crate::tables_store::{FAMILIES, Paths};

impl crate::App {
    pub(crate) fn open_registry_tab(&mut self) -> Result<(), String> {
        self.open_singleton_native_tab(AppKind::Registry, |app, wid| {
            app.stage_registry_tab(wid, None, None, Role::Responsive)
        })
    }

    pub(crate) fn stage_registry_tab(
        &mut self,
        target: impl Into<crate::app_tabs::NativeInstallTarget>,
        paths: Option<Paths>,
        selected: Option<String>,
        initial_role: Role,
    ) -> Result<(TabId, ViewId), String> {
        let target = target.into();
        let kind = AppKind::Registry;
        let presentation = TabPresentation {
            title: kind.display_name().into(),
            icon: Some(kind.icon()),
            indicators: Default::default(),
            conn: None,
            closable: true,
            tooltip: Some(kind.display_name().into()),
        };
        let mut view = RegistryViewState::default();
        view.select(selected);
        let state = AppViewState::Registry(Box::new(view));
        let placed = if let Some(instance) = self.native_runtime.instance_by_kind(kind) {
            self.install_native_tab(target, instance, state, presentation)
        } else {
            let proxy = self.proxy.clone();
            let model = RegistryApp::new(paths, initial_role, move || {
                if let Some(proxy) = &proxy {
                    let _ = proxy.send_event(crate::Wake::RegistryChanged);
                }
            });
            self.install_new_native_tab(
                target,
                NativeApp::Registry(Box::new(model)),
                state,
                presentation,
            )
            .map(|(_, tab, view)| (tab, view))
        };
        placed.map_err(|error| format!("aRegistry could not be opened: {error:?}"))
    }

    pub(crate) fn on_registry_changed(&mut self) {
        if self.native_runtime.poll_registry() {
            for mut window in self.windows.values_mut() {
                window.last_present = None;
            }
            self.request_redraw_all_windows();
        }
    }

    pub(crate) fn open_registry_machine_in_tables(
        &mut self,
        wid: crate::WindowId,
        instance: AppInstanceId,
        key: &str,
    ) -> Result<(), String> {
        let Some(NativeApp::Registry(registry)) = self.native_runtime.app(instance) else {
            return Err("The aRegistry tab closed".into());
        };
        let paths = registry.paths();
        let view = self.open_singleton_native_tab_in(wid, AppKind::Tables, |app, wid| {
            app.stage_tables_tab(wid, paths, FAMILIES)
        })?;
        self.reveal_table_record(view, "machine", key)
    }
}

#[cfg(all(test, unix))]
#[path = "app_registry_tests.rs"]
mod tests;
