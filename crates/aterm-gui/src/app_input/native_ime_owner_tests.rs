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

fn password_composition_after_focus_round_trip(
    leave_and_return: impl FnOnce(&mut App, WindowId, ViewId),
) {
    let (mut app, wid, instance, view, _) = page(false);
    focus(&mut app, wid, view, "system-state/password");
    app.on_ime_commit(wid, "kept".into());
    let before = values(&app, instance);
    app.on_ime_preedit(wid, COMPOSED.into(), None);
    leave_and_return(&mut app, wid, view);

    // No platform event arrived while another field or tab owned input. The
    // empty preedit and Commit still belong to the old composition.
    commit(&mut app, wid);
    assert_eq!(
        values(&app, instance),
        before,
        "returning to a field cannot revive its retired composition"
    );
    assert!(app.windows[&wid].preedit.is_empty());

    // Retirement ends with that sequence, not with the field: a subsequent
    // standalone commit and a new composition both reach the current field.
    app.on_ime_commit(wid, "new".into());
    assert_eq!(values(&app, instance), (ADDRESS.into(), "•".repeat(7)));
    app.on_ime_preedit(wid, COMPOSED.into(), None);
    commit(&mut app, wid);
    assert_eq!(
        values(&app, instance),
        (ADDRESS.into(), "•".repeat(7 + COMPOSED.chars().count()))
    );
}

#[test]
fn ime_focus_round_trip_does_not_revive_a_password_composition() {
    password_composition_after_focus_round_trip(|app, wid, view| {
        focus(app, wid, view, "system-state/url");
        focus(app, wid, view, "system-state/password");
    });
}

#[test]
fn ime_focus_round_trip_through_a_tab_does_not_revive_a_password_composition() {
    password_composition_after_focus_round_trip(|app, wid, view| {
        app.switch_tab_in(wid, 0);
        assert!(app.active_native_view(wid).is_none());
        assert!(app.open_system_state_tab());
        assert_eq!(
            app.active_native_view(wid).map(|(_, active)| active),
            Some(view)
        );
    });
}

#[test]
fn ime_focus_round_trip_unchanged_field_keeps_its_password_composition() {
    let (mut app, wid, instance, view, _) = page(false);
    focus(&mut app, wid, view, "system-state/password");
    app.on_ime_preedit(wid, COMPOSED.into(), None);
    focus(&mut app, wid, view, "system-state/password");
    commit(&mut app, wid);
    assert_eq!(
        values(&app, instance),
        (ADDRESS.into(), "•".repeat(COMPOSED.chars().count()))
    );
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

fn url_review(known: bool) -> (App, WindowId, crate::native_app::AppInstanceId, ViewId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 90;
    window.cols = 160;
    window.a11y_active = true;
    app.set_system_state_handlers_for_test(
        crate::system_state_host::InitialSetupHandlers::inert_for_test(),
    );
    assert!(app.open_system_state_tab());
    if known {
        app.publish_system_state(crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            ..crate::SetupStatus::default()
        });
    }
    let (instance, view) = app.active_native_view(wid).unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new("system-state/row/secrets/disclose"),
            value: None,
        }),
    )
    .unwrap();
    focus(&mut app, wid, view, "system-state/url");
    (app, wid, instance, view)
}

#[test]
fn a_url_composition_survives_status_hydration_and_same_selection_vault_updates() {
    let source = aterm_types::macos_setup::Plan::default();
    let updated = aterm_types::macos_setup::Plan {
        vault_url: "http://newer.invalid/v1/team".into(),
        ..source.clone()
    };

    // A passive form still follows the saved vault when no draft is being typed.
    let (mut passive, _, instance, _) = url_review(true);
    passive.publish_system_state(crate::SetupStatus {
        plan: Some(updated.clone()),
        ..crate::SetupStatus::default()
    });
    assert_eq!(values(&passive, instance).0, updated.vault_url);

    for known in [false, true] {
        let (mut app, wid, instance, _) = url_review(known);
        app.on_ime_preedit(wid, COMPOSED.into(), Some((0, COMPOSED.len())));
        let Some(NativeApp::SystemState(model)) = app.native_runtime.app(instance) else {
            panic!("the URL's review stays live");
        };
        let draft = model.field_projection("system-state/url").unwrap();
        assert!(draft.preedit.is_some());
        assert_eq!(draft.text, format!("{}{COMPOSED}", source.vault_url));
        app.publish_system_state(crate::SetupStatus {
            plan: Some(updated.clone()),
            ..crate::SetupStatus::default()
        });
        let Some(NativeApp::SystemState(model)) = app.native_runtime.app(instance) else {
            panic!("the URL's review stays live");
        };
        assert_eq!(
            model.field_projection("system-state/url").unwrap(),
            draft,
            "a saved vault update must preserve the current marked URL draft"
        );
        commit(&mut app, wid);
        assert_eq!(values(&app, instance).0, draft.text);
        let Some(NativeApp::SystemState(model)) = app.native_runtime.app(instance) else {
            panic!("the URL's review stays live");
        };
        assert!(
            model
                .field_projection("system-state/url")
                .unwrap()
                .preedit
                .is_none()
        );
        assert!(app.native_runtime.take_system_state_action().is_none());
    }
}

#[test]
fn a_url_composition_rejects_an_incoming_automatic_offer_without_losing_its_commit() {
    let (mut app, wid, instance, _) = url_review(true);
    app.on_ime_preedit(wid, COMPOSED.into(), None);
    let draft = values(&app, instance);
    let (send, receive) = std::sync::mpsc::channel();
    let request = crate::workstation_setup::Request::new(
        aterm_types::macos_setup::Plan {
            vault_url: "http://offered.invalid/v1/team".into(),
            ..aterm_types::macos_setup::Plan::default()
        },
        send,
    );
    // Exercise the actual windowed offer preflight without an OS surface or probe.
    assert!(!app.apprt.probe_preferences);
    assert!(app.windows[&wid].os_window.is_none());
    app.headless = false;
    app.offer_system_state(request.clone());
    assert_eq!(
        values(&app, instance),
        draft,
        "an automatic offer cannot replace a composing URL"
    );
    assert_eq!(
        receive.try_recv().unwrap().decision,
        crate::WorkstationSetupDecision::Unavailable
    );
    assert!(!request.pending());
    commit(&mut app, wid);
    assert_eq!(values(&app, instance), draft);
    assert!(app.native_runtime.take_system_state_action().is_none());
}

#[test]
fn a_password_composition_rejects_an_incoming_automatic_offer_without_losing_its_commit() {
    let (mut app, wid, instance, view) = url_review(true);
    focus(&mut app, wid, view, "system-state/password");
    let source = values(&app, instance).0;
    app.on_ime_preedit(wid, COMPOSED.into(), None);
    assert_eq!(values(&app, instance), (source.clone(), String::new()));
    assert!(app.windows[&wid].preedit.is_empty());
    let (send, receive) = std::sync::mpsc::channel();
    let request = crate::workstation_setup::Request::new(
        aterm_types::macos_setup::Plan {
            vault_url: "http://offered.invalid/v1/team".into(),
            ..aterm_types::macos_setup::Plan::default()
        },
        send,
    );
    assert!(!app.apprt.probe_preferences);
    assert!(app.windows[&wid].os_window.is_none());
    app.headless = false;
    app.offer_system_state(request.clone());
    assert_eq!(
        values(&app, instance),
        (source.clone(), String::new()),
        "an automatic offer cannot replace a composing password's vault"
    );
    assert_eq!(
        receive.try_recv().unwrap().decision,
        crate::WorkstationSetupDecision::Unavailable
    );
    assert!(!request.pending());
    commit(&mut app, wid);
    assert_eq!(
        values(&app, instance),
        (source, "•".repeat(COMPOSED.chars().count()))
    );
    assert!(app.windows[&wid].preedit.is_empty());
    assert!(app.native_runtime.take_system_state_action().is_none());
}

#[test]
fn a_password_composition_survives_saved_plan_hydration_and_vault_updates() {
    let source = aterm_types::macos_setup::Plan::default();
    let updated = aterm_types::macos_setup::Plan {
        vault_url: "http://newer.invalid/v1/team".into(),
        ..source.clone()
    };
    for known in [false, true] {
        let (mut app, wid, instance, view) = url_review(known);
        focus(&mut app, wid, view, "system-state/password");
        app.on_ime_preedit(wid, COMPOSED.into(), None);
        app.publish_system_state(crate::SetupStatus {
            plan: Some(updated.clone()),
            ..crate::SetupStatus::default()
        });
        assert_eq!(
            values(&app, instance),
            (source.vault_url.clone(), String::new()),
            "a saved vault update must preserve the composing password's review"
        );
        commit(&mut app, wid);
        assert_eq!(
            values(&app, instance),
            (
                source.vault_url.clone(),
                "•".repeat(COMPOSED.chars().count())
            )
        );
        assert!(app.windows[&wid].preedit.is_empty());
        assert!(app.native_runtime.take_system_state_action().is_none());
    }
}

#[test]
fn a_cleared_password_composition_allows_an_offer_without_leaking_its_text() {
    for cancelled in [false, true] {
        let (mut app, wid, instance, view) = url_review(true);
        focus(&mut app, wid, view, "system-state/password");
        if cancelled {
            app.on_ime_preedit(wid, COMPOSED.into(), None);
            app.on_ime_enablement_changed(wid);
        } else {
            app.on_ime_preedit(wid, String::new(), None);
        }
        let offered = aterm_types::macos_setup::Plan {
            vault_url: "http://offered.invalid/v1/team".into(),
            ..aterm_types::macos_setup::Plan::default()
        };
        let (send, receive) = std::sync::mpsc::channel();
        let request = crate::workstation_setup::Request::new(offered.clone(), send);
        app.headless = false;
        app.offer_system_state(request.clone());
        assert!(request.pending(), "no composition still owns a draft");
        assert!(receive.try_recv().is_err());
        assert_eq!(
            values(&app, instance),
            (offered.vault_url.clone(), String::new())
        );
        commit(&mut app, wid);
        assert_eq!(
            values(&app, instance),
            (offered.vault_url, String::new()),
            "the retired password must never reach the offered URL or password"
        );
        assert!(app.windows[&wid].preedit.is_empty());
        request.finish(crate::WorkstationSetupDecision::Unavailable);
    }
}

#[test]
fn a_retired_password_composition_preserves_committed_text_and_clears_its_draft() {
    let (mut app, wid, instance, view) = url_review(true);
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new("system-state/password"),
            value: Some(SemanticInput::Text("kept".into())),
        }),
    )
    .unwrap();
    let before = values(&app, instance);
    app.on_ime_preedit(wid, COMPOSED.into(), None);
    focus(&mut app, wid, view, "system-state/url");
    commit(&mut app, wid);
    assert_eq!(values(&app, instance), before);
    assert!(
        !app.native_runtime.cancel_preedit(
            instance,
            view,
            Some(&UiKey::new("system-state/password")),
        ),
        "recipient retirement already cleared the password composition"
    );
    assert!(app.windows[&wid].preedit.is_empty());
}

fn close_linked_composition(field: &str, close_owner: bool) {
    let (mut app, wid, instance, owner) = url_review(true);
    app.windows.get_mut(&wid).unwrap().cols = 240;
    let sibling = app
        .split_active_with_native(
            wid,
            SplitAxis::Horizontal,
            instance,
            AppViewState::SystemState(Default::default()),
        )
        .unwrap();
    focus(&mut app, wid, owner, field);
    let before = values(&app, instance);
    app.on_ime_preedit(wid, COMPOSED.into(), None);
    let composing = values(&app, instance);
    let closing = if close_owner { owner } else { sibling };
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(closing)
    );
    app.close_focused_mixed_leaf(wid).unwrap();
    assert!(app.view_store.get(closing).is_none());
    let survivor = if close_owner { sibling } else { owner };
    assert_eq!(app.active_native_view(wid), Some((instance, survivor)));
    assert!(app.structural_invariants_ok());
    let Some(NativeApp::SystemState(model)) = app.native_runtime.app(instance) else {
        panic!("the linked page survives");
    };
    if close_owner {
        assert_eq!(
            values(&app, instance),
            before,
            "the closed owner's marked text is retired"
        );
        assert!(
            model.can_offer(),
            "no orphaned composition can hold the surviving review"
        );
        commit(&mut app, wid);
        assert_eq!(
            values(&app, instance),
            before,
            "a late commit cannot move to the surviving view"
        );
    } else {
        assert_eq!(values(&app, instance), composing);
        assert!(
            !model.can_offer(),
            "the surviving owner's draft still belongs to it"
        );
        commit(&mut app, wid);
        let expected = if field.ends_with("password") {
            (before.0, "•".repeat(COMPOSED.chars().count()))
        } else {
            (format!("{}{COMPOSED}", before.0), before.1)
        };
        assert_eq!(values(&app, instance), expected);
    }
    assert!(app.windows[&wid].preedit.is_empty());
}

#[test]
fn linked_composition_close_retires_the_url_owner() {
    close_linked_composition("system-state/url", true);
}

#[test]
fn linked_composition_close_retires_the_password_owner() {
    close_linked_composition("system-state/password", true);
}

#[test]
fn linked_composition_close_preserves_the_surviving_owner() {
    for field in ["system-state/url", "system-state/password"] {
        close_linked_composition(field, false);
    }
}
