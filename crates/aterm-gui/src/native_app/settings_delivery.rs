// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retain process facts independently of views. Unchanged delivery visits no view;
//! attachment seeds a new view before it can render or accept an action.

use super::{AppViewState, NativeRuntime, ViewId};
use crate::native_settings::{SessionRuntime, SettingsViewState};
use std::sync::Arc;

type Families = aterm_render::font_catalog::MonospaceFamilies;
type Refusals = Vec<crate::native_font_catalog::FontRefusal>;

fn copy_refusals(refusals: &[crate::native_font_catalog::FontRefusal]) -> Refusals {
    #[cfg(test)]
    crate::app_settings::FONT_REFUSAL_COPIES.with(|n| n.set(n.get() + refusals.len()));
    refusals.to_vec()
}

#[derive(Default)]
pub(super) struct Retained {
    families: Option<Arc<Families>>,
    refusals: Arc<Refusals>,
    drawn: Option<aterm_render::DrawnFaces>,
    runtime: Option<SessionRuntime>,
}

impl Retained {
    pub(super) fn seed(&self, state: &mut SettingsViewState) {
        if let Some(families) = &self.families {
            state.replace_font_families(Arc::clone(families));
        }
        state.replace_font_refusals(copy_refusals(&self.refusals), false);
        state.replace_drawn_faces(self.drawn.clone());
        state.session_runtime = self.runtime;
    }
}

impl NativeRuntime {
    fn changed_settings_views(
        &mut self,
        mut change: impl FnMut(&mut SettingsViewState) -> bool,
    ) -> Vec<ViewId> {
        let views: Vec<_> = self
            .views
            .iter()
            .filter_map(|(&view, state)| matches!(state, AppViewState::Settings(_)).then_some(view))
            .collect();
        views
            .into_iter()
            .filter(|&view| {
                self.change_view_state(view, |state| match state {
                    AppViewState::Settings(state) => change(state),
                    _ => unreachable!("the view kind cannot change during delivery"),
                })
            })
            .collect()
    }

    pub(crate) fn deliver_settings_families(&mut self, families: Arc<Families>) -> Vec<ViewId> {
        if self
            .settings_delivery
            .families
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &families))
        {
            return Vec::new();
        }
        self.settings_delivery.families = Some(Arc::clone(&families));
        self.changed_settings_views(|state| state.replace_font_families(Arc::clone(&families)))
    }

    pub(crate) fn deliver_settings_refusals(
        &mut self,
        refusals: Arc<Refusals>,
        announce: bool,
    ) -> Vec<ViewId> {
        if Arc::ptr_eq(&self.settings_delivery.refusals, &refusals) {
            return Vec::new();
        }
        self.settings_delivery.refusals = Arc::clone(&refusals);
        self.changed_settings_views(|state| {
            state.replace_font_refusals(copy_refusals(&refusals), announce)
        })
    }

    pub(crate) fn deliver_settings_drawn(
        &mut self,
        drawn: Option<&aterm_render::DrawnFaces>,
    ) -> Vec<ViewId> {
        if self.settings_delivery.drawn.as_ref() == drawn {
            return Vec::new();
        }
        let drawn = drawn.cloned();
        self.settings_delivery.drawn.clone_from(&drawn);
        self.changed_settings_views(|state| state.replace_drawn_faces(drawn.clone()))
    }

    pub(crate) fn deliver_settings_runtime(&mut self, runtime: SessionRuntime) -> Vec<ViewId> {
        if self.settings_delivery.runtime == Some(runtime) {
            return Vec::new();
        }
        self.settings_delivery.runtime = Some(runtime);
        self.changed_settings_views(|state| {
            if state.session_runtime == Some(runtime) {
                return false;
            }
            state.session_runtime = Some(runtime);
            true
        })
    }
}
