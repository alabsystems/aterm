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
                tab.root.leaves().into_iter().find_map(|view| {
                    let View::Native(native) = self.view_store.get(view)? else {
                        return None;
                    };
                    (self.native_runtime.app(native.instance)?.kind() == AppKind::About)
                        .then_some((tab.id, view))
                })
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
                for view in tab.root.leaves() {
                    if let Some(View::Native(native)) = self.view_store.get(view)
                        && self
                            .native_runtime
                            .app(native.instance)
                            .is_some_and(|app| app.kind() == AppKind::About)
                    {
                        targets.push((wid, tab.id, view));
                    }
                }
            }
        }
        for (wid, tab, view) in targets {
            let _ = self.close_view_keeping_front(wid, tab, view);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_introspect::AuxTarget;
    use crate::native_settings::SettingsRoute;

    #[test]
    fn menu_about_is_independent_of_settings_and_its_file_capabilities() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(SettingsRoute::TextFonts));
        let settings = app.active_native_view(wid).unwrap();
        let initial_tabs = app.windows[&wid].tab_set.len();
        app.open_about_from_menu();
        let about = app.active_native_view(wid).unwrap();
        assert_ne!(about, settings);
        assert_eq!(
            app.native_runtime.app(about.0).unwrap().kind(),
            AppKind::About
        );
        assert_eq!(app.native_runtime.document_id(about.0), None);
        assert!(!app.can_find(wid));
        assert!(!app.can_find_again(wid));
        assert!(app.save_front_document().is_err());
        assert_eq!(app.windows[&wid].tab_set.len(), initial_tabs + 1);
        app.open_about_from_menu();
        assert_eq!(app.active_native_view(wid), Some(about));
        assert_eq!(app.windows[&wid].tab_set.len(), initial_tabs + 1);
        assert!(matches!(
            app.native_runtime.view_state(settings.1),
            Some(AppViewState::Settings(state)) if state.route == SettingsRoute::TextFonts
        ));

        assert!(app.prepare_native_input_scratch(wid));
        let frame = app.windows[&wid].native_ui_compiled.as_ref().unwrap();
        let expected = frame.compiled.controls_lines();
        let actual = app.read_aux_controls(AuxTarget::About);
        assert!(actual[0].contains("open=true"));
        assert!(actual[0].contains("app=about"));
        assert_eq!(
            &actual[2..],
            expected.as_slice(),
            "inspect the actual retained About frame"
        );
        assert!(
            frame
                .compiled
                .semantics
                .iter()
                .all(|node| !node.key.as_str().starts_with("settings/"))
        );
        assert_eq!(app.window_target_refusal(AuxTarget::About), None);
        assert!(app.window_target_refusal(AuxTarget::Prefs).is_some());
        app.close_aux_overlay(AuxTarget::About).unwrap();
        assert!(app.view_store.get(about.1).is_none());
        assert_eq!(app.active_native_view(wid), Some(settings));
        assert!(matches!(
            app.native_runtime.view_state(settings.1),
            Some(AppViewState::Settings(state)) if state.route == SettingsRoute::TextFonts
        ));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn about_views_share_read_only_information_and_keep_their_own_window() {
        let mut app = App::headless_for_test();
        let first = WindowId(0);
        assert!(app.open_about_tab_in_window(first));
        let about_first = app.active_native_view(first).unwrap();
        let session = app.next_session_id;
        let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
        assert_eq!(app.frontmost_window, Some(second));
        assert!(app.open_settings_tab(SettingsRoute::Home));
        let settings = app.active_native_view(second).unwrap();
        let lines = app.read_aux_controls(AuxTarget::About);
        assert!(lines[0].contains("open=false"));
        assert_eq!(
            lines.len(),
            1,
            "never borrow controls from the background About"
        );
        assert!(app.window_target_refusal(AuxTarget::About).is_some());

        assert!(app.open_about_tab());
        let about_second = app.active_native_view(second).unwrap();
        assert_eq!(about_first.0, about_second.0);
        assert_ne!(about_first.1, about_second.1);
        assert_eq!(app.active_native_view(first), Some(about_first));
        assert!(app.open_about_tab_in_window(first));
        assert_eq!(app.frontmost_window, Some(second));
        assert_eq!(app.active_native_view(second), Some(about_second));
        assert!(app.view_store.get(settings.1).is_some());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn about_tab_keys_visit_offscreen_actions_and_reveal_each_focus() {
        use crate::input::InputEvent;
        use crate::native_ui::UiKey;
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

        struct Scale(Option<f32>);
        impl Drop for Scale {
            fn drop(&mut self) {
                crate::native_appearance::inject_text_scale(self.0);
            }
        }
        let _scale = Scale(crate::native_appearance::injected_text_scale());
        crate::native_appearance::inject_text_scale(Some(1.0));
        let stops = ["about/open-site", "about/copy-build-info", "about/details"];
        let key = |backwards| InputEvent::Key {
            key: Key::Named(NamedKey::Tab),
            mods: if backwards {
                Modifiers::SHIFT
            } else {
                Modifiers::empty()
            },
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        };
        let mut failures = Vec::new();
        for short in [false, true] {
            for backwards in [false, true] {
                let mut app = App::headless_for_test();
                let wid = WindowId(0);
                assert!(app.open_settings_tab(SettingsRoute::TextFonts));
                let settings = app.active_native_view(wid).unwrap();
                let other_session = app.next_session_id;
                let other = app.insert_logical_window(crate::stub_session(other_session), 24, 80);
                assert!(app.open_settings_tab(SettingsRoute::Home));
                let other_settings = app.active_native_view(other).unwrap();
                assert!(app.open_about_tab_in_window(wid));
                app.window_focus_event(wid, true);
                let (instance, view) = app.active_native_view(wid).unwrap();
                let (cw, ch) = app.win_cell_size(wid);
                let pad = app.win_pad(wid);
                {
                    let window = app.windows.get_mut(&wid).unwrap();
                    window.scale = 1.0;
                    window.cols = u16::try_from(314usize.saturating_sub(pad * 2) / cw)
                        .unwrap()
                        .max(1);
                    window.rows = u16::try_from(84usize.saturating_sub(pad).div_ceil(ch))
                        .unwrap()
                        .max(1);
                }
                if !short {
                    // Keep the real grid compact enough to stack the actions. Its initial
                    // room ends just beyond Site; focusing Copy scrolls to a screen that
                    // shows Site and Copy while the logical next action remains below it.
                    let compiled = app.compiled_native_ui(wid).unwrap();
                    let site_bottom = compiled
                        .semantic(&UiKey::new(stops[0]))
                        .unwrap()
                        .rect
                        .bottom();
                    app.windows.get_mut(&wid).unwrap().rows =
                        ((site_bottom + 12.0 - pad as f32) / ch as f32).ceil() as u16;
                }
                let compiled = app.compiled_native_ui(wid).unwrap();
                assert_eq!(compiled.list_stops, stops.map(UiKey::new));
                for stop in stops {
                    assert!(
                        compiled
                            .semantic(&UiKey::new(stop))
                            .unwrap()
                            .state
                            .is_some_and(|state| state.enabled)
                    );
                }
                assert!(compiled.semantic(&UiKey::new(stops[2])).unwrap().offscreen);
                if short {
                    assert!(
                        compiled.focus_order.is_empty(),
                        "the fresh short screen shows no action"
                    );
                    assert!(
                        app.native_runtime
                            .view_state(view)
                            .unwrap()
                            .common()
                            .last_focus
                            .is_none()
                    );
                } else {
                    for expected in &stops[..2] {
                        assert!(app.native_input_event(wid, &key(false)));
                        assert_eq!(
                            app.native_runtime
                                .view_state(view)
                                .unwrap()
                                .common()
                                .last_focus
                                .as_ref()
                                .map(|key| key.as_str()),
                            Some(*expected)
                        );
                    }
                    let compiled = app.compiled_native_ui(wid).unwrap();
                    assert_eq!(
                        compiled.focus_order,
                        stops[..2]
                            .iter()
                            .map(|key| UiKey::new(*key))
                            .collect::<Vec<_>>()
                    );
                    assert!(compiled.semantic(&UiKey::new(stops[2])).unwrap().offscreen);
                }
                let owners = |app: &App| {
                    app.windows
                        .iter()
                        .map(|(&window, state)| {
                            (
                                window,
                                state.tab_set.active_id(),
                                state
                                    .tab_set
                                    .tabs()
                                    .iter()
                                    .map(|tab| (tab.id, tab.root.clone(), tab.focus))
                                    .collect::<Vec<_>>(),
                            )
                        })
                        .collect::<Vec<_>>()
                };
                let settings_state = |app: &App, view| {
                    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view)
                    else {
                        panic!("the retained Settings view");
                    };
                    (
                        state.route,
                        state.common.last_focus.clone(),
                        state.carried_field_drafts(),
                    )
                };
                let ownership = owners(&app);
                let settings_before = settings_state(&app, settings.1);
                let other_before = settings_state(&app, other_settings.1);
                let config = app.native_config_service.snapshot();
                let next_session = app.next_session_id;
                let terminal_views = app.pool.views(0);
                let other_terminal_views = app.pool.views(other_session);
                let start = if short {
                    if backwards { 2 } else { 0 }
                } else if backwards {
                    0
                } else {
                    2
                };
                for step in 0..6 {
                    let expected = stops[if backwards {
                        (start + 6 - step) % 3
                    } else {
                        (start + step) % 3
                    }];
                    assert!(app.native_input_event(wid, &key(backwards)));
                    let focus = app
                        .native_runtime
                        .view_state(view)
                        .unwrap()
                        .common()
                        .last_focus
                        .clone();
                    if focus.as_ref().map(|key| key.as_str()) != Some(expected) {
                        failures.push(format!("short={short} backwards={backwards} step={step}: expected {expected}, got {focus:?}"));
                    }
                    let compiled = app.compiled_native_ui(wid).unwrap();
                    if let Some(focus) = focus {
                        let node = compiled
                            .semantic(&focus)
                            .expect("the focused action remains in the page");
                        let viewport = app.native_ui_viewport_for(wid, view).unwrap();
                        if node.offscreen
                            || node.rect.y < viewport.y - 0.5
                            || node.rect.bottom() > viewport.bottom() + 0.5
                        {
                            failures.push(format!("short={short} backwards={backwards} step={step}: focused {focus:?} is not visible: {:?} in {viewport:?}", node.rect));
                        }
                    }
                    assert!(
                        !app.native_runtime
                            .presentation(instance, view)
                            .unwrap()
                            .indicators
                            .busy
                    );
                    assert!(compiled.semantic(&UiKey::new("about/feedback")).is_none());
                    assert_eq!(
                        compiled
                            .semantic(&UiKey::new(stops[2]))
                            .unwrap()
                            .state
                            .and_then(|state| state.expanded),
                        Some(false),
                        "Tab never activates Details"
                    );
                }
                assert_eq!(owners(&app), ownership);
                assert_eq!(app.frontmost_window, Some(wid));
                assert_eq!(app.active_native_view(wid), Some((instance, view)));
                assert_eq!(app.active_native_view(other), Some(other_settings));
                assert_eq!(settings_state(&app, settings.1), settings_before);
                assert_eq!(settings_state(&app, other_settings.1), other_before);
                assert_eq!(app.next_session_id, next_session);
                assert_eq!(app.pool.views(0), terminal_views);
                assert_eq!(app.pool.views(other_session), other_terminal_views);
                let after = app.native_config_service.snapshot();
                assert_eq!(after.revision, config.revision);
                assert_eq!(after.text, config.text);
                assert!(app.structural_invariants_ok());
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn about_scroll_before_its_details_frame_uses_the_target_views_current_layout() {
        use crate::native_app::{ActionInvocation, AppEvent};
        use crate::native_ui::ActionId;

        let shown = |unfocused: bool, drawn_first: bool| {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            {
                let window = app.windows.get_mut(&wid).unwrap();
                window.cols = 140;
                window.rows = 40;
            }
            assert!(app.open_about_tab());
            let (instance, view) = app.active_native_view(wid).unwrap();
            if unfocused {
                app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
                assert!(app.open_about_tab_in_window(wid));
            }
            assert!(app.commit_glassless_native_frame(wid));
            assert!(
                app.retained_native_leaf_artifact(wid, view, true).is_some(),
                "the original About leaf has a presented artifact; unfocused={unfocused}"
            );
            let render = |app: &App| {
                let viewport = app.native_ui_viewport_for(wid, view).unwrap();
                app.compiled_native_ui_for(wid, instance, view, viewport)
                    .unwrap()
            };
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("about/details"),
                    value: None,
                }),
            )
            .unwrap();
            assert!(
                app.retained_native_leaf_artifact(wid, view, true).is_none(),
                "Details invalidates the previous frame"
            );
            if unfocused {
                assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Down));
                assert!(
                    app.active_native_view(wid).is_none(),
                    "the terminal sibling owns keyboard focus"
                );
                assert!(
                    app.active_visible_leaf_plan(wid)
                        .unwrap()
                        .leaf(view)
                        .is_some()
                );
            }
            let tab = app.windows[&wid].tab_set.active().unwrap();
            let owner = (tab.id, tab.root.clone(), tab.focus);
            let next_session = app.next_session_id;
            let terminal_views = app.pool.views(0);
            let config = app.native_config_service.snapshot();
            if drawn_first {
                render(&app);
            }
            if unfocused {
                app.scroll_native_view(wid, view, 8, None).unwrap();
            } else {
                app.dispatch_native_view_event(wid, view, AppEvent::ScrollPage(1))
                    .unwrap();
            }
            let Some(AppViewState::About(state)) = app.native_runtime.view_state(view) else {
                unreachable!()
            };
            let offset = state.scroll_offset();
            let compiled = render(&app);
            let visible = compiled
                .semantics
                .iter()
                .filter(|node| !node.offscreen && node.key.as_str().starts_with("about/"))
                .map(|node| (node.key.clone(), node.rect))
                .collect::<Vec<_>>();
            let tab = app.windows[&wid].tab_set.active().unwrap();
            assert_eq!((tab.id, tab.root.clone(), tab.focus), owner);
            assert_eq!(app.frontmost_window, Some(wid));
            assert_eq!(app.next_session_id, next_session);
            assert_eq!(app.pool.views(0), terminal_views);
            let after = app.native_config_service.snapshot();
            assert_eq!(after.revision, config.revision);
            assert_eq!(after.text, config.text);
            assert!(app.structural_invariants_ok());
            (offset, visible, compiled.fingerprint())
        };

        let mut failures = Vec::new();
        for unfocused in [false, true] {
            let actual = shown(unfocused, false);
            let drawn = shown(unfocused, true);
            assert!(
                drawn.0 > 0.0,
                "the drawn control has enough details to scroll"
            );
            assert!(
                !drawn.1.is_empty(),
                "the drawn control presents About semantics"
            );
            if actual.0 != drawn.0 {
                failures.push(format!(
                    "unfocused={unfocused}: offset {} before layout, {} after layout",
                    actual.0, drawn.0
                ));
            }
            if actual.1 != drawn.1 || actual.2 != drawn.2 {
                failures.push(format!("unfocused={unfocused}: the scrolled semantic frame differs before and after layout"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
