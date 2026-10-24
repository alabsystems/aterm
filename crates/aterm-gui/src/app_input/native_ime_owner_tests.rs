// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_accessibility::stable_node_id_for_view;
use crate::native_app::{ActionInvocation, AppEvent, AppViewState, NativeApp, SemanticInput};
use crate::native_ui::{ActionId, UiKey};
use crate::tab_model::{SplitAxis, ViewId};
use aterm_a11y::model::{Action, ActionRequest, TreeId};

const ADDRESS: &str = "http://vault.invalid/v1/team";
const COMPOSED: &str = "inert-密碼";

fn page(
    split: bool,
) -> (
    App,
    WindowId,
    crate::native_app::AppInstanceId,
    ViewId,
    ViewId,
) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 90;
    window.cols = if split { 240 } else { 160 };
    window.a11y_active = true;
    assert!(app.open_system_state_tab());
    app.set_system_state_handlers_for_test(
        crate::system_state_host::InitialSetupHandlers::inert_for_test(),
    );
    app.publish_system_state(crate::SetupStatus {
        plan: Some(aterm_types::macos_setup::Plan::default()),
        ..crate::SetupStatus::default()
    });
    let (instance, first) = app.active_native_view(wid).unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new("system-state/url"),
            value: Some(SemanticInput::Text(ADDRESS.into())),
        }),
    )
    .unwrap();
    let second = if split {
        app.split_active_with_native(
            wid,
            SplitAxis::Horizontal,
            instance,
            AppViewState::SystemState(Default::default()),
        )
        .unwrap();
        app.active_native_view(wid).unwrap().1
    } else {
        first
    };
    (app, wid, instance, first, second)
}

fn focus(app: &mut App, wid: WindowId, view: ViewId, key: &str) {
    if app.prepare_heterogeneous_input_scratch(wid).is_none() {
        assert!(app.prepare_native_input_scratch(wid));
    }
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    app.stage_visible_native_accessibility(wid, &plan);
    let (update, published) = app.take_native_accessibility_update(wid).unwrap().unwrap();
    let node = stable_node_id_for_view(view, &UiKey::new(key));
    let control = &update.nodes.iter().find(|(id, _)| *id == node).unwrap().1;
    assert!(control.supports_action(Action::Focus));
    let context = control.action_context();
    assert!(context.is_some());
    app.windows.get_mut(&wid).unwrap().native_a11y_published = Some(published);
    app.on_native_accessibility_action(
        wid,
        ActionRequest {
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: node,
            context,
            data: None,
        },
    );
    assert_eq!(app.active_native_view(wid).unwrap().1, view);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .last_focus,
        Some(UiKey::new(key))
    );
}

fn values(app: &App, instance: crate::native_app::AppInstanceId) -> (String, String) {
    let Some(NativeApp::SystemState(page)) = app.native_runtime.app(instance) else {
        panic!("System State is open");
    };
    (
        page.field_projection("system-state/url").unwrap().text,
        page.field_projection("system-state/password").unwrap().text,
    )
}

fn commit(app: &mut App, wid: WindowId) {
    // AppKit emits the empty preedit before Commit. Ownership must survive it.
    app.on_ime_preedit(wid, String::new(), None);
    app.on_ime_commit(wid, COMPOSED.into());
}

#[test]
fn a_password_composition_cannot_follow_ax_focus_into_an_address() {
    for split in [false, true] {
        // A same-field composition must still commit normally, including the
        // platform's empty-preedit event immediately before its commit.
        let (mut app, wid, instance, first, _) = page(split);
        focus(&mut app, wid, first, "system-state/password");
        app.on_ime_preedit(wid, COMPOSED.into(), Some((COMPOSED.len(), COMPOSED.len())));
        assert!(
            app.windows[&wid].preedit.is_empty(),
            "native password composition never reaches the terminal overlay"
        );
        commit(&mut app, wid);
        assert_eq!(
            values(&app, instance),
            (ADDRESS.into(), "•".repeat(COMPOSED.chars().count()))
        );

        for another_preedit in [false, true] {
            let (mut app, wid, instance, first, destination) = page(split);
            focus(&mut app, wid, first, "system-state/password");
            app.on_ime_preedit(wid, COMPOSED.into(), Some((COMPOSED.len(), COMPOSED.len())));
            focus(&mut app, wid, destination, "system-state/url");
            if another_preedit {
                app.on_ime_preedit(wid, COMPOSED.into(), None);
                assert_eq!(
                    values(&app, instance).0,
                    ADDRESS,
                    "an update still belongs to the password composition"
                );
            }
            commit(&mut app, wid);
            assert_eq!(
                values(&app, instance),
                (ADDRESS.into(), String::new()),
                "password composition must not move to a public field or sibling view"
            );

            // A fresh composition at the new focus is independent of the retired one.
            app.on_ime_preedit(wid, COMPOSED.into(), None);
            commit(&mut app, wid);
            assert_eq!(values(&app, instance).0, format!("{ADDRESS}{COMPOSED}"));
        }
    }
}

#[test]
fn a_replaced_setup_plan_retires_its_composition_even_with_the_same_field_focused() {
    let (mut app, wid, instance, first, _) = page(false);
    focus(&mut app, wid, first, "system-state/password");
    app.on_ime_preedit(wid, COMPOSED.into(), None);
    let mut next = aterm_types::macos_setup::Plan::default();
    next.selection
        .set(aterm_types::macos_setup::Setting::Dock, false);
    app.publish_system_state(crate::SetupStatus {
        plan: Some(next),
        ..crate::SetupStatus::default()
    });
    commit(&mut app, wid);
    assert_eq!(values(&app, instance), (ADDRESS.into(), String::new()));
    app.on_ime_commit(wid, COMPOSED.into());
    assert_eq!(
        values(&app, instance).1,
        "•".repeat(COMPOSED.chars().count()),
        "a subsequent standalone commit is independent of the retired sequence"
    );
}

#[test]
fn a_native_composition_retires_its_old_field_projection_after_focus_moves() {
    let (mut app, wid, instance, first, _) = page(false);
    focus(&mut app, wid, first, "system-state/url");
    app.on_ime_preedit(wid, COMPOSED.into(), None);
    assert_eq!(values(&app, instance).0, format!("{ADDRESS}{COMPOSED}"));
    focus(&mut app, wid, first, "system-state/password");
    commit(&mut app, wid);
    assert_eq!(
        values(&app, instance),
        (ADDRESS.into(), String::new()),
        "neither the old preedit nor its commit belongs to the new field"
    );
}
