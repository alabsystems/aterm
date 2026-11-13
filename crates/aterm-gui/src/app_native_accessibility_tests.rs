// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_a11y::model::{Action, ActionRequest, TreeId};
use aterm_models_native::composite_accessibility_route_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

use super::*;
use crate::native_accessibility::{
    PublishedNativeAccessibility, compose_native_accessibility, project_native_accessibility,
    project_native_accessibility_for_view_in_container, stable_node_id, stable_node_id_for_view,
};
use crate::native_app::{AppKind, AppViewState};
use crate::native_settings::SettingsRoute;
use crate::native_ui::UiKey;

fn set_a11y_active(app: &mut App, wid: WindowId, active: bool) {
    app.windows
        .get_mut(&wid)
        .expect("headless window")
        .a11y_active = active;
}

#[test]
fn native_staging_is_idle_until_an_accessibility_client_attaches() {
    let wid = WindowId(0);
    let mut single = App::headless_for_test();
    assert!(single.open_settings_tab(SettingsRoute::About));
    assert!(single.prepare_native_input_scratch(wid));
    assert!(
        single.windows[&wid].native_a11y_staged.is_none(),
        "an inactive adapter must not build a staged tree"
    );
    let (_, view) = single.active_native_view(wid).unwrap();
    let compiled = single.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("the visual native frame was still prepared")
        .compiled
        .clone();
    assert!(
        !compiled.semantics.is_empty(),
        "negative control: this frame would build a non-empty accessibility tree"
    );
    let plan = single.active_visible_leaf_plan(wid).unwrap();
    set_a11y_active(&mut single, wid, true);
    single.stage_native_accessibility(wid, view, &plan);
    assert_eq!(
        single.windows[&wid]
            .native_a11y_staged
            .as_ref()
            .expect("attachment enables staging")
            .owners
            .len(),
        1
    );
    set_a11y_active(&mut single, wid, false);
    single.stage_native_accessibility(wid, view, &plan);
    assert!(
        single.windows[&wid].native_a11y_staged.is_none(),
        "deactivation clears an older stage"
    );

    let mut composite = App::headless_for_test();
    assert!(composite.open_settings_tab(SettingsRoute::Home));
    let (instance, _) = composite.active_native_view(wid).unwrap();
    composite
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            instance,
            AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
                &composite.config,
            ))),
        )
        .unwrap();
    assert!(composite.prepare_heterogeneous_input_scratch(wid).is_some());
    assert!(composite.windows[&wid].native_a11y_staged.is_none());
    assert_eq!(
        composite.windows[&wid]
            .leaf_render_cache
            .values()
            .filter(|cache| cache.native.is_some())
            .count(),
        2,
        "negative control: both native leaves were visually prepared"
    );
    let plan = composite.active_visible_leaf_plan(wid).unwrap();
    set_a11y_active(&mut composite, wid, true);
    composite.stage_visible_native_accessibility(wid, &plan);
    assert_eq!(
        composite.windows[&wid]
            .native_a11y_staged
            .as_ref()
            .expect("attachment enables composite staging")
            .owners
            .len(),
        2
    );
}

#[derive(Clone, Copy)]
struct RouteProjection {
    owner_one_generation: i64,
    owner_two_generation: i64,
    published_one_generation: i64,
    published_two_generation: i64,
    focus_owner: i64,
    pending: i64,
    target_owner: i64,
    target_generation: i64,
    dispatched_owner: i64,
    dispatched_generation: i64,
    cross_dispatch: i64,
    stale_dispatch: i64,
}

impl Default for RouteProjection {
    fn default() -> Self {
        Self {
            owner_one_generation: 1,
            owner_two_generation: 1,
            published_one_generation: 0,
            published_two_generation: 0,
            focus_owner: 2,
            pending: 0,
            target_owner: 0,
            target_generation: 0,
            dispatched_owner: 0,
            dispatched_generation: 0,
            cross_dispatch: 0,
            stale_dispatch: 0,
        }
    }
}

fn project_route(model: &Model, projection: RouteProjection) -> State {
    let mut state = model.init_state();
    state.insert("owner_one_generation", projection.owner_one_generation);
    state.insert("owner_two_generation", projection.owner_two_generation);
    state.insert(
        "published_one_generation",
        projection.published_one_generation,
    );
    state.insert(
        "published_two_generation",
        projection.published_two_generation,
    );
    state.insert("focus_owner", projection.focus_owner);
    state.insert("pending", projection.pending);
    state.insert("target_owner", projection.target_owner);
    state.insert("target_generation", projection.target_generation);
    state.insert("dispatched_owner", projection.dispatched_owner);
    state.insert("dispatched_generation", projection.dispatched_generation);
    state.insert("cross_dispatch", projection.cross_dispatch);
    state.insert("stale_dispatch", projection.stale_dispatch);
    state
}

fn assert_route_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(
        model.successors(action, before).as_slice(),
        std::slice::from_ref(after),
        "shipping route transition must conform specifically to {action}"
    );
    assert_eq!(admits(model, before, after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "post-state violates {}::{}: {after:?}",
            model.name,
            invariant.name
        );
    }
}

#[test]
fn presented_snapshot_publishes_its_exact_projection_and_generation() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(SettingsRoute::About));
    set_a11y_active(&mut app, wid, true);
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    let generation = app
        .native_runtime
        .view_generation(view)
        .expect("live view generation");
    let compiled = app.compiled_native_ui(wid).expect("compiled native UI");
    let focus = compiled
        .focus_order
        .first()
        .cloned()
        .expect("focusable Settings control");
    app.native_runtime
        .view_state_mut(view)
        .expect("live view")
        .common_mut()
        .last_focus = Some(focus.clone());
    assert!(app.prepare_native_input_scratch(wid));
    let compiled = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("retained native frame")
        .compiled
        .clone();
    let transform = app
        .native_accessibility_transform(wid)
        .expect("live window transform");
    let owner = AccessibilityOwner {
        view,
        generation,
        review_revision: 0,
    };
    let leaf = project_native_accessibility_for_view_in_container(
        &compiled,
        Some(&focus),
        transform,
        owner,
    )
    .expect("expected leaf accessibility projection");
    let leaf_root = leaf.update().tree.as_ref().expect("leaf tree").root;
    let expected = compose_native_accessibility(
        vec![(owner, leaf)],
        Some(view),
        app.native_accessibility_window_bounds(wid)
            .expect("window bounds"),
        app.windows
            .get(&wid)
            .map(|ws| ws.current_title.as_str())
            .unwrap_or_default(),
    )
    .expect("expected accessibility projection")
    .into_update();

    let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
    app.stage_native_accessibility(wid, view, &plan);
    let (actual, published) = app
        .take_native_accessibility_update(wid)
        .expect("active native view")
        .expect("native accessibility update");

    assert_eq!(actual, expected);
    assert_eq!(actual.nodes.len(), compiled.semantics.len() + 1);
    assert_eq!(actual.focus, stable_node_id_for_view(view, &focus));
    let root = actual.tree.as_ref().expect("full tree").root;
    assert_eq!(
        actual
            .nodes
            .iter()
            .find(|(node, _)| *node == leaf_root)
            .expect("leaf root node")
            .1
            .transform(),
        Some(&transform)
    );
    assert_eq!(
        actual
            .nodes
            .iter()
            .find(|(node, _)| *node == root)
            .expect("window root")
            .1
            .role(),
        aterm_a11y::model::Role::Window
    );
    assert_eq!(published.view, view);
    assert_eq!(published.generation, generation);
    assert!(
        published
            .route(stable_node_id_for_view(view, &focus))
            .is_some()
    );
    assert!(
        app.windows
            .get(&wid)
            .expect("headless window")
            .native_a11y_staged
            .is_none(),
        "a presented projection is consumed exactly once"
    );
}

#[test]
fn title_advanced_after_staging_is_authoritative_in_the_same_publish() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(SettingsRoute::About));
    set_a11y_active(&mut app, wid, true);
    app.windows.get_mut(&wid).unwrap().current_title = "before redraw".into();
    assert!(app.prepare_native_input_scratch(wid));

    let staged = app.windows[&wid]
        .native_a11y_staged
        .as_ref()
        .expect("native preparation stages accessibility");
    let staged_root = staged
        .projection
        .update()
        .tree
        .as_ref()
        .expect("staged window tree")
        .root;
    assert_eq!(
        staged
            .projection
            .update()
            .nodes
            .iter()
            .find(|(node, _)| *node == staged_root)
            .unwrap()
            .1
            .label(),
        Some("before redraw"),
        "negative control: staging really captured the preceding title"
    );

    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    app.windows
        .get_mut(&wid)
        .and_then(|window| window.leaf_render_cache.get_mut(&view))
        .and_then(|cache| cache.native.as_mut())
        .expect("retained native raster")
        .presented = true;
    app.windows.get_mut(&wid).unwrap().current_title = "same-frame title".into();

    let (update, _) = app
        .take_native_accessibility_update(wid)
        .expect("active native view")
        .expect("same-frame projection");
    let root = update.tree.as_ref().expect("published window tree").root;
    assert_eq!(
        update
            .nodes
            .iter()
            .find(|(node, _)| *node == root)
            .unwrap()
            .1
            .label(),
        Some("same-frame title"),
        "publish must reject the stale staged label and recompose from the retained frame"
    );
}

#[test]
fn editor_publish_retains_visible_text_coordinates_and_canonical_source_bytes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let dir = std::env::temp_dir().join(format!("aterm-native-a11y-editor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("unicode.txt");
    std::fs::write(&path, "alpha\naé\tz\nomega\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).expect("a file uri");
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    set_a11y_active(&mut app, wid, true);

    let (instance, view) = app.active_native_view(wid).expect("native editor view");
    let document = app
        .native_runtime
        .document_id(instance)
        .expect("editor document");
    let snapshot = app.document_store.snapshot(document).unwrap();
    assert!(app.prepare_native_input_scratch(wid));
    let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
    app.stage_native_accessibility(wid, view, &plan);
    let (update, published) = app
        .take_native_accessibility_update(wid)
        .expect("native update")
        .expect("valid update");

    let target = published
        .virtual_text()
        .first()
        .expect("published virtual editor text");
    assert!(!target.lines.is_empty());
    assert_eq!(
        target.visible_range,
        target.lines.first().unwrap().source.start..target.lines.last().unwrap().source.end
    );
    for line in &target.lines {
        let source =
            usize::try_from(line.source.start).unwrap()..usize::try_from(line.source.end).unwrap();
        assert_eq!(&snapshot.text[source], line.text);
        assert_eq!(
            line.character_positions.len() + 1,
            line.character_source_offsets.len()
        );
        assert_eq!(line.character_positions.len(), line.character_widths.len());
        assert!(update.nodes.iter().any(|(node, access)| {
            *node == line.node
                && access.role() == aterm_a11y::model::Role::TextRun
                && access.value() == Some(line.text.as_str())
        }));
    }
    assert_eq!(
        target.primary_selection,
        Some(crate::native_accessibility::VirtualTextSelection {
            anchor_byte: 0,
            focus_byte: 0,
        })
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The published accessibility press on the Mac settings line's switch, which turns the
/// line off.
fn publish_leave(app: &mut App, wid: WindowId, view: ViewId) -> ActionRequest {
    set_a11y_active(app, wid, true);
    assert!(app.prepare_native_input_scratch(wid));
    let (update, published) = app.take_native_accessibility_update(wid).unwrap().unwrap();
    let node = stable_node_id_for_view(view, &UiKey::new("system-state/line/mac/switch"));
    let context = update
        .nodes
        .iter()
        .find(|(id, _)| *id == node)
        .expect("published Mac settings switch")
        .1
        .action_context();
    assert!(context.is_some());
    app.windows.get_mut(&wid).unwrap().native_a11y_published = Some(published);
    ActionRequest {
        context,
        action: Action::Click,
        target_tree: TreeId::ROOT,
        target_node: node,
        data: None,
    }
}

fn vault_page_with_draft() -> (App, WindowId, ViewId, crate::native_app::AppInstanceId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 90;
    window.cols = 160;
    assert!(app.open_system_state_tab());
    app.set_system_state_handlers_for_test(
        crate::system_state_host::InitialSetupHandlers::inert_for_test(),
    );
    app.publish_system_state(crate::SetupStatus {
        plan: Some(aterm_types::macos_setup::Plan::default()),
        ..crate::SetupStatus::default()
    });
    let (instance, view) = app.active_native_view(wid).unwrap();
    // The fields of another vault, under the open Secrets & identity line.
    for key in [
        "system-state/line/secrets/disclose",
        "system-state/row/secrets/disclose",
    ] {
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new(key),
                value: None,
            }),
        )
        .unwrap();
    }
    for (key, text) in [
        ("system-state/url", "http://draft.invalid:7484/v1/team"),
        ("system-state/password", "inert-password"),
    ] {
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new(key),
                value: Some(crate::native_app::SemanticInput::Text(text.into())),
            }),
        )
        .unwrap();
    }
    (app, wid, view, instance)
}

fn publish_vault_action(
    app: &mut App,
    wid: WindowId,
    view: ViewId,
    key: &str,
    value: Option<&str>,
) -> ActionRequest {
    let action = if value.is_some() {
        Action::SetValue
    } else {
        Action::Click
    };
    app.accessibility_request_for_test(
        wid,
        view,
        key,
        action,
        value.map(|text| aterm_a11y::model::ActionData::Value(text.into())),
    )
}

fn launch_offer_ax_navigation(action: Action) {
    use crate::workstation_setup::{Request, WorkstationSetupDecision};
    use aterm_types::macos_setup::Plan;
    use std::sync::mpsc;
    use std::time::Instant;

    // Nobody is at the screen: the page's first tick answers the offer all the same
    // (init-on-by-default design §6.1), and navigation before it answers nothing.
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // A window 270 points tall (a headless window's scale is 1), whatever this platform's
    // face makes a row: the page's head shows and its accounts line lies below it.
    let (_, cell_h) = app.win_cell_size(wid);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = u16::try_from(270 / cell_h).unwrap();
    window.cols = 90;
    window.os_window_for_test = true;
    assert!(app.open_system_state_tab());
    app.set_system_state_handlers_for_test(crate::InitialSetupHandlers::inert_for_test());
    let (_, view) = app.active_native_view(wid).unwrap();
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    // The real offer entry, with only the display's presence stood in:
    // every platform port remains the headless App's inert one.
    app.headless = false;
    app.offer_system_state(request.clone());
    app.headless = true;
    assert!(request.pending());
    assert!(app.commit_glassless_native_frame(wid));
    let start = Instant::now();

    let key = UiKey::new(if action == Action::ScrollIntoView {
        "system-state/line/accounts"
    } else {
        "system-state/line/mac/switch"
    });
    assert_eq!(
        app.compiled_native_ui(wid)
            .unwrap()
            .semantic(&key)
            .unwrap()
            .offscreen,
        action == Action::ScrollIntoView,
        "premise: the accounts line lies below the window, the Mac line within it"
    );
    set_a11y_active(&mut app, wid, true);
    assert!(app.prepare_native_input_scratch(wid));
    let (update, published) = app.take_native_accessibility_update(wid).unwrap().unwrap();
    let target_node = stable_node_id_for_view(view, &key);
    let control = &update
        .nodes
        .iter()
        .find(|(id, _)| *id == target_node)
        .expect("the offer's page publishes the target row or control")
        .1;
    assert!(control.supports_action(action));
    if action == Action::ScrollIntoView {
        assert!(control.is_offscreen());
    }
    let context = control.action_context();
    assert!(context.is_some());
    app.windows.get_mut(&wid).unwrap().native_a11y_published = Some(published);
    let scroll = |app: &App| match app.native_runtime.view_state(view).unwrap() {
        AppViewState::SystemState(state) => state.list_scroll().0,
        _ => panic!("the page remains System State"),
    };
    let before = scroll(&app);
    app.on_native_accessibility_action(
        wid,
        ActionRequest {
            context,
            action,
            target_tree: TreeId::ROOT,
            target_node,
            data: None,
        },
    );
    if action == Action::ScrollIntoView {
        assert!(scroll(&app) > before, "Show item really moves the list");
        assert!(app.commit_glassless_native_frame(wid));
        assert!(
            !app.compiled_native_ui(wid)
                .unwrap()
                .semantic(&key)
                .unwrap()
                .offscreen,
            "the requested row is now visible"
        );
    } else {
        assert_eq!(scroll(&app), before);
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .last_focus,
            Some(key)
        );
    }
    assert!(request.pending(), "navigation answers nothing");
    // The first tick answers it with nobody here: navigation held nothing (design
    // revision 4).
    app.tick_system_state(start);
    let mut answer = receive
        .try_recv()
        .expect("the page answers at its first tick");
    assert_eq!(
        answer.decision,
        WorkstationSetupDecision::Begin(Plan::default())
    );
    assert_eq!(
        answer.receipt.as_ref().unwrap().admission(),
        crate::system_state::Admission::Automatic
    );
    answer.receipt.take().unwrap().accept();
    app.resolve_system_state_apply();
    assert!(!request.pending());
    assert!(receive.try_recv().is_err());
    assert!(app.structural_invariants_ok());
}

#[test]
fn launch_offer_ax_show_item_moves_the_list_and_answers_nothing() {
    launch_offer_ax_navigation(Action::ScrollIntoView);
}

#[test]
fn launch_offer_ax_focus_answers_nothing() {
    launch_offer_ax_navigation(Action::Focus);
}

#[cfg(target_os = "macos")]
mod exact_card_open {
    use super::*;
    use aterm_types::macos_setup::{Access, Standing, Subject};

    thread_local! {
        static OPENED: std::cell::RefCell<Vec<Access>> = const {
            std::cell::RefCell::new(Vec::new())
        };
    }

    fn reading(access: Option<Access>, busy: bool) -> crate::SetupStatus {
        let mut status = crate::system_state::fixture_status("set-up").unwrap();
        status.busy = busy;
        if let Some(access) = access {
            let row = status
                .observed
                .iter_mut()
                .find(|row| row.subject == Subject::Access(access))
                .unwrap();
            row.standing = Standing::NeedsYou;
            row.now = Some("Off".into());
        }
        status
    }

    fn page(access: Access) -> (App, WindowId, ViewId) {
        use crate::devtools_helper::{Lane, test_arms};
        use crate::system_state_access::StepsReach;
        use aterm_containment::consent::{FdaProbe, FdaState, ProbeLabel};
        OPENED.with(|opened| opened.borrow_mut().clear());
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        assert!(app.open_system_state_tab());
        let (_, view) = app.active_native_view(wid).unwrap();
        app.set_system_state_handlers_for_test(
            crate::system_state_host::InitialSetupHandlers::inert_for_test(),
        );
        // Exercise the windowed hand-over through inert ports, with no
        // actual OS surface, probe, pane or process-wide dialog guard.
        assert!(!app.apprt.probe_preferences);
        assert!(app.windows[&wid].os_window.is_none());
        app.headless = false;
        app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(FdaProbe {
            state: FdaState::Denied,
            label: ProbeLabel::OpenEperm,
        });
        let mut fda = test_arms::fda_opening();
        fda.open_pane = || {
            OPENED.with(|opened| opened.borrow_mut().push(Access::FullDiskAccess));
            true
        };
        app.devtools_helper =
            Lane::windowed_with_fda_for_test(test_arms::opening(), fda, test_arms::showing());
        app.access_steps.set_reach(StepsReach {
            open_pane: |_, access| {
                OPENED.with(|opened| opened.borrow_mut().push(access));
                true
            },
            ..StepsReach::INERT
        });
        app.publish_system_state(reading(Some(access), false));
        (app, wid, view)
    }

    fn published_open(app: &mut App, wid: WindowId, view: ViewId, access: Access) -> ActionRequest {
        use crate::native_ui::SemanticRole;
        assert!(app.prepare_native_input_scratch(wid));
        let key = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .unwrap()
            .compiled
            .semantics
            .iter()
            .find(|node| {
                node.role == SemanticRole::Button
                    && node.key.as_str().starts_with("system-state/row/")
                    && node.key.as_str().ends_with("/open")
                    && node.label.contains(access.name())
            })
            .expect("the card's button names the permission this press opens")
            .key
            .clone();
        publish_vault_action(app, wid, view, key.as_str(), None)
    }

    #[test]
    fn a_queued_card_open_cannot_change_which_permission_the_person_pressed() {
        let (mut app, wid, view) = page(Access::FullDiskAccess);
        let old = published_open(&mut app, wid, view, Access::FullDiskAccess);
        let review = old.context;
        // A background grant reading moves the headline to another row,
        // but its new frame has not replaced the old platform action.
        app.publish_system_state(reading(Some(Access::Accessibility), false));
        app.on_native_accessibility_action(wid, old);
        assert!(
            OPENED.with(|opened| opened.borrow().is_empty()),
            "an FDA press must not become an Accessibility press"
        );

        let current = published_open(&mut app, wid, view, Access::Accessibility);
        assert_eq!(current.context, review, "the plan and review are unchanged");
        app.on_native_accessibility_action(wid, current);
        assert_eq!(
            OPENED.with(|opened| opened.borrow().clone()),
            [Access::Accessibility],
            "a fresh press opens its own current target"
        );
    }

    #[test]
    fn a_queued_card_open_does_nothing_once_the_grant_is_in_place() {
        let (mut app, wid, view) = page(Access::FullDiskAccess);
        let old = published_open(&mut app, wid, view, Access::FullDiskAccess);
        app.publish_system_state(reading(None, false));
        app.on_native_accessibility_action(wid, old);
        assert!(OPENED.with(|opened| opened.borrow().is_empty()));
    }

    #[test]
    fn a_queued_card_open_keeps_its_permission_during_a_run() {
        for busy in [false, true] {
            for access in [Access::FullDiskAccess, Access::Accessibility] {
                let (mut app, wid, view) = page(access);
                let current = published_open(&mut app, wid, view, access);
                app.publish_system_state(reading(Some(access), busy));
                app.on_native_accessibility_action(wid, current);
                assert_eq!(
                    OPENED.with(|opened| opened.borrow().clone()),
                    [access],
                    "the current permission can unblock a busy run"
                );
            }
        }
    }
}

fn setup_start_reading(standing: aterm_types::macos_setup::Standing) -> crate::SetupStatus {
    use aterm_types::macos_setup::{Setting, Standing, Subject};
    let mut status = crate::system_state::fixture_status("set-up").unwrap();
    let repositories = status
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Repositories))
        .unwrap();
    repositories.standing = standing;
    match standing {
        Standing::Differs | Standing::Failed => repositories.now = Some("3 of 12".into()),
        Standing::Waiting => {
            repositories.now = None;
            repositories.note = Some("Waits for the vault to finish syncing this Mac".into());
        }
        _ => {}
    }
    // A Stop holds the changes to make: the one time Set up now is offered, in the footer
    // (with nothing holding them they start by themselves).
    status.run = Some(crate::InitialSetupRunPolicy {
        revision: 1,
        allowed: false,
    });
    status
}

fn setup_start_failed_result(retrying: bool) -> crate::SetupStatus {
    use aterm_types::macos_setup::{Setting, Standing};
    crate::SetupStatus {
        retrying,
        rows: vec![crate::SetupStatusRow {
            name: Setting::Repositories.name().into(),
            status: "error".into(),
            detail: String::new(),
        }],
        ..setup_start_reading(Standing::Failed)
    }
}

fn assert_deferred_setup_start(current: crate::SetupStatus, should_start: bool) {
    use aterm_types::macos_setup::{Plan, Standing};
    thread_local! {
        static APPLIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    APPLIED.with(|applied| applied.set(0));
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 90;
    window.cols = 160;
    assert!(app.open_system_state_tab());
    let (_, view) = app.active_native_view(wid).unwrap();
    app.set_system_state_handlers_for_test(crate::system_state_host::InitialSetupHandlers {
        apply: |plan, typed, password, receipt| {
            assert_eq!(plan, Plan::default());
            assert!(!typed);
            assert!(password.is_none());
            APPLIED.with(|applied| applied.set(applied.get() + 1));
            receipt.accept();
            true
        },
        ..crate::system_state_host::InitialSetupHandlers::inert_for_test()
    });
    app.publish_system_state(setup_start_reading(Standing::Differs));
    let queued = publish_vault_action(&mut app, wid, view, "system-state/headline/start", None);
    let review = app
        .native_runtime
        .view_state(view)
        .unwrap()
        .common()
        .review_revision;
    let plan = current.plan.as_ref().unwrap();
    assert_eq!(
        crate::system_state::headline(
            &current,
            &crate::system_state::rows(&current, plan.selection),
            Some(plan),
        )
        .start,
        should_start
    );
    // The status reached the model, but its next native frame and AX
    // tree have not replaced the enabled action already queued by macOS.
    app.publish_system_state(current);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .review_revision,
        review,
        "a same-plan reading does not replace the review"
    );
    app.on_native_accessibility_action(wid, queued.clone());
    assert_eq!(
        APPLIED.with(std::cell::Cell::get),
        usize::from(should_start)
    );
    app.on_native_accessibility_action(wid, queued);
    assert_eq!(
        APPLIED.with(std::cell::Cell::get),
        usize::from(should_start),
        "an eligible action starts exactly once"
    );
}

#[test]
fn deferred_setup_start_does_not_run_after_same_plan_readings_settle() {
    assert_deferred_setup_start(
        setup_start_reading(aterm_types::macos_setup::Standing::InPlace),
        false,
    );
}

#[test]
fn deferred_setup_start_does_not_run_when_the_same_plan_only_waits() {
    assert_deferred_setup_start(
        setup_start_reading(aterm_types::macos_setup::Standing::Waiting),
        false,
    );
}

#[test]
fn deferred_setup_start_does_not_run_when_the_same_plan_needs_a_person() {
    use aterm_types::macos_setup::{Setting, Standing, Subject};
    let mut current = setup_start_reading(Standing::Waiting);
    let vault = current
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Setting(Setting::Secrets))
        .unwrap();
    vault.standing = Standing::NeedsYou;
    vault.now = Some("Awaiting code".into());
    vault.note = Some("On an admin Mac: aterm secrets approve INERT-CODE".into());
    assert_deferred_setup_start(current, false);
}

#[test]
fn deferred_setup_start_runs_once_for_current_changes() {
    use aterm_types::macos_setup::Standing;
    assert_deferred_setup_start(setup_start_reading(Standing::Differs), true);
}

#[test]
fn failed_rows_publish_no_press_and_aterm_alone_runs_the_plan_again_once() {
    // A whole Workstation-stage error publishes one error per setting: two card items that
    // say aterm tries again by itself. The accessibility tree holds no press for either and
    // no Set up now, and the page's own retry is one run, a minute on, however many failed.
    use crate::system_state::SystemStateAction;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 90;
    window.cols = 160;
    assert!(app.open_system_state_tab());
    app.set_system_state_handlers_for_test(
        crate::system_state_host::InitialSetupHandlers::inert_for_test(),
    );
    let failed = crate::system_state::fixture_status("named-results-failed").unwrap();
    app.publish_system_state(failed);
    let compiled = app.compiled_native_ui(wid).unwrap();
    let items: Vec<&str> = compiled
        .semantics
        .iter()
        .filter(|node| {
            node.label
                .ends_with(crate::system_state::lines::TRIES_BY_ITSELF)
        })
        .map(|node| node.label.as_str())
        .collect();
    assert_eq!(items.len(), 2, "{items:?}");
    let presses: Vec<&str> = compiled
        .semantics
        .iter()
        .filter(|node| {
            node.key.as_str().starts_with("system-state/card/")
                && node.role == crate::native_ui::SemanticRole::Button
        })
        .map(|node| node.key.as_str())
        .collect();
    assert!(presses.is_empty(), "{presses:?}");
    assert!(
        compiled
            .semantic(&UiKey::new("system-state/headline/start"))
            .is_none()
    );
    let page = app.native_runtime.system_state_mut().unwrap();
    let now = aterm_time::Instant::now();
    page.tick_at(now);
    assert!(
        page.take_action().is_none(),
        "a failed run is not run again at once"
    );
    page.tick_at(now + std::time::Duration::from_secs(61));
    assert!(matches!(
        page.take_action(),
        Some(SystemStateAction::Apply(..))
    ));
    page.tick_at(now + std::time::Duration::from_secs(62));
    assert!(page.take_action().is_none(), "one run for both rows");
}

#[test]
fn deferred_setup_start_does_not_repeat_a_failure_retrying_by_itself() {
    assert_deferred_setup_start(setup_start_failed_result(true), false);
}

#[test]
fn deferred_setup_start_does_not_run_for_a_reading_that_failed_by_itself() {
    // Its note names its own remedy, which Set up now cannot give, retried or not.
    for retrying in [false, true] {
        assert_deferred_setup_start(
            crate::SetupStatus {
                retrying,
                ..setup_start_reading(aterm_types::macos_setup::Standing::Failed)
            },
            false,
        );
    }
}

/// With the model unmoved, a tree compiled live is the tree the frame published, node for node,
/// in every layout: a press's repaint takes the card that placed it, so the live read has to
/// work out the same place by itself.
#[test]
fn a_live_tree_is_the_presented_tree_while_the_model_stands_still() {
    for scale in [1.0_f64, 2.0] {
        for layout in ["single", "two native", "side by side", "mixed"] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.backend.set_pad((12.0 * scale).round() as usize);
            app.seed_headless_boot_metrics();
            {
                let window = app.windows.get_mut(&wid).unwrap();
                window.cols = 140;
                window.rows = 48;
                window.scale = scale;
            }
            assert!(app.open_settings_tab(SettingsRoute::About));
            let (instance, view) = app.active_native_view(wid).unwrap();
            match layout {
                "two native" | "side by side" => {
                    app.split_active_with_native(
                        wid,
                        if layout == "two native" {
                            crate::tab_model::SplitAxis::Vertical
                        } else {
                            crate::tab_model::SplitAxis::Horizontal
                        },
                        instance,
                        AppViewState::Settings(Box::new(
                            crate::native_settings::SettingsViewState::new(&app.config),
                        )),
                    )
                    .unwrap();
                }
                "mixed" => {
                    app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
                    assert!(
                        app.windows
                            .get_mut(&wid)
                            .and_then(|ws| ws.tab_set.active_mut())
                            .is_some_and(|tab| tab.set_focus(view))
                    );
                    app.sync_window(wid);
                }
                _ => {}
            }
            set_a11y_active(&mut app, wid, true);
            if layout == "single" {
                assert!(app.prepare_native_input_scratch(wid));
            } else {
                assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
            }
            let (framed, _) = app
                .take_native_accessibility_update(wid)
                .expect("a native tree")
                .expect("the frame's tree");

            app.invalidate_native_ui_cache(wid);
            app.windows.get_mut(&wid).unwrap().settings_card = None;
            let (live, _) = app
                .take_native_accessibility_update(wid)
                .expect("a native tree")
                .expect("the live tree");

            assert_eq!(live, framed, "{layout} at {scale}x");
        }
    }
}

/// The frame `wid`'s layout calls for, on glass, and the tree it stages.
fn framed_tree(app: &mut App, wid: WindowId) -> aterm_a11y::model::TreeUpdate {
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    if plan.leaves.len() == 1 {
        assert!(app.prepare_native_input_scratch(wid));
    } else {
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    }
    let window = app.windows.get_mut(&wid).unwrap();
    for leaf in &plan.leaves {
        if let Some(raster) = window
            .leaf_render_cache
            .get_mut(&leaf.view)
            .and_then(|cache| cache.native.as_mut())
        {
            raster.presented = true;
        }
    }
    app.take_native_accessibility_update(wid)
        .expect("a native tree")
        .expect("the frame's tree")
        .0
}

/// A window that moves before its frame must not leave a client holding the old layout: a
/// press goes to the page at the live viewport, and a control only the old one had is dropped.
/// So the tree read in that gap is the one the next frame stages, node for node. Each case
/// starts from a presented frame, and the move itself is what refuses that frame's read.
#[test]
fn a_live_tree_is_the_next_frame_when_the_layout_moves_under_it() {
    for scale in [1.0_f64, 2.0] {
        for change in [
            "narrower",
            "shorter",
            "a chrome row",
            "the terminal sibling closes",
            "a native sibling closes",
        ] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.backend.set_pad((12.0 * scale).round() as usize);
            app.seed_headless_boot_metrics();
            {
                let window = app.windows.get_mut(&wid).unwrap();
                window.cols = 140;
                window.rows = 48;
                window.scale = scale;
            }
            assert!(app.open_settings_tab(SettingsRoute::About));
            let (instance, view) = app.active_native_view(wid).unwrap();
            let tab = app.windows[&wid].tab_set.active_id().unwrap();
            let sibling = match change {
                "the terminal sibling closes" => {
                    app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
                    assert!(
                        app.windows
                            .get_mut(&wid)
                            .and_then(|ws| ws.tab_set.active_mut())
                            .is_some_and(|tab| tab.set_focus(view))
                    );
                    app.sync_window(wid);
                    let plan = app.active_visible_leaf_plan(wid).unwrap();
                    Some(
                        plan.leaves
                            .iter()
                            .find(|leaf| leaf.view != view)
                            .unwrap()
                            .view,
                    )
                }
                "a native sibling closes" => Some(
                    app.split_active_with_native(
                        wid,
                        crate::tab_model::SplitAxis::Vertical,
                        instance,
                        AppViewState::Settings(Box::new(
                            crate::native_settings::SettingsViewState::new(&app.config),
                        )),
                    )
                    .unwrap(),
                ),
                _ => None,
            };
            set_a11y_active(&mut app, wid, true);
            framed_tree(&mut app, wid);

            match change {
                "narrower" => app.windows.get_mut(&wid).unwrap().cols = 60,
                "shorter" => app.windows.get_mut(&wid).unwrap().rows = 20,
                "a chrome row" => {
                    app.tab_strip_rows = app.tab_strip_rows.saturating_add(1);
                    app.windows.get_mut(&wid).unwrap().rows = 47;
                }
                _ => app
                    .close_view_keeping_front(wid, tab, sibling.unwrap())
                    .unwrap(),
            }
            let moved = app.active_native_view(wid).unwrap().1;
            let compiles = crate::app_native::native_ui_compile_count();
            let (live, _) = app
                .take_native_accessibility_update(wid)
                .expect("a native tree")
                .expect("the live tree");
            assert!(
                crate::app_native::native_ui_compile_count() > compiles,
                "{change} refused the frame's read"
            );

            if change == "narrower" {
                let carries = |key: &str| {
                    let node = stable_node_id_for_view(moved, &UiKey::new(key));
                    live.nodes.iter().any(|(id, _)| *id == node)
                };
                assert!(
                    !carries("settings/nav/appearance"),
                    "a compact page has no rail to press"
                );
            }
            let framed = framed_tree(&mut app, wid);
            if change == "a chrome row" {
                // The strip's tabs come from the segments a frame lays out, so they join with
                // that frame; the page has already moved down for them.
                let root = live.tree.as_ref().unwrap().root;
                for node in live.nodes.iter().filter(|(id, _)| *id != root) {
                    assert!(framed.nodes.contains(node), "{change} at {scale}x");
                }
            } else {
                assert_eq!(live, framed, "{change} at {scale}x");
            }
        }
    }
}

/// The text of each terminal document in `update`.
fn terminal_text(update: &aterm_a11y::model::TreeUpdate) -> Vec<String> {
    update
        .nodes
        .iter()
        .filter(|(_, node)| node.role() == aterm_a11y::model::Role::Terminal)
        .filter_map(|(_, node)| node.value().map(str::to_string))
        .collect()
}

/// A real frame of a plain terminal window tells a client the words it composed.
#[test]
fn a_terminal_frame_publishes_the_words_it_composed() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().a11y_active = true;
    crate::scripted_redraw_fixture::feed(&app, b"spoken words");
    app.redraw_window(wid);
    let sent = app.windows[&wid]
        .a11y_sent
        .last()
        .expect("the frame's tree");
    assert!(
        terminal_text(sent)
            .iter()
            .any(|text| text.contains("spoken words")),
        "{:?}",
        terminal_text(sent)
    );
}

/// A real frame of a split window tells a client each pane's words.
#[test]
fn a_split_frame_publishes_each_panes_words() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    let second = app.split_active_stub_tab(wid);
    app.windows.get_mut(&wid).unwrap().a11y_active = true;
    for (session, words) in [(0, &b"left words"[..]), (second, &b"right words"[..])] {
        let term = app.pool.get(session).expect("the pane").term.clone();
        crate::term_lock(&term).process(words);
    }
    app.redraw_window(wid);
    let sent = app.windows[&wid]
        .a11y_sent
        .last()
        .expect("the frame's tree");
    let text = terminal_text(sent);
    for words in ["left words", "right words"] {
        assert!(text.iter().any(|t| t.contains(words)), "{words}: {text:?}");
    }
}

/// A window with a tab strip, a client and words on the terminal's top row, a frame presented.
fn presented_terminal_under_a_strip() -> (App, WindowId) {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.windows.get_mut(&wid).unwrap().a11y_active = true;
    crate::scripted_redraw_fixture::feed(&app, b"top row words");
    // Twice: on Linux and Windows the strip is a band set in the UI face, and a face that
    // lands while the first frame draws it asks one frame more and drops the frame's key
    // (`rearm_for_strip_band_landing`).
    app.redraw_window(wid);
    app.redraw_window(wid);
    assert!(
        app.windows[&wid].last_present.is_some(),
        "PRECONDITION: the window holds the frame it presented"
    );
    (app, wid)
}

/// The document a client holds in window `wid`, whole: every row, the top one first.
fn assert_the_whole_terminal(app: &App, wid: WindowId, why: &str) {
    let sent = app.windows[&wid].a11y_sent.last().expect("a tree");
    let text = terminal_text(sent);
    assert_eq!(text.len(), 1, "{why}: {text:?}");
    assert!(text[0].starts_with("top row words"), "{why}: {text:?}");
    assert_eq!(
        text[0].lines().count(),
        usize::from(app.windows[&wid].rows),
        "{why}: {text:?}"
    );
}

/// A redraw that proves nothing changed leaves the cells without their strip, and a read after
/// it must still start at the terminal's top row.
#[test]
fn a_read_after_a_redraw_that_changed_nothing_keeps_the_top_row() {
    let (mut app, wid) = presented_terminal_under_a_strip();
    let proved = app.windows[&wid].redraws_proved_unchanged;

    app.redraw_window(wid);

    assert!(
        app.windows[&wid].redraws_proved_unchanged > proved,
        "CONTROL"
    );
    app.push_a11y_tree(wid);
    assert_the_whole_terminal(&app, wid, "after an unchanged redraw");
}

/// A frame held for a synchronized update stops before it composes, as above.
#[test]
fn a_read_during_a_synchronized_update_keeps_the_top_row() {
    let (mut app, wid) = presented_terminal_under_a_strip();
    crate::scripted_redraw_fixture::feed(&app, b"\x1b[?2026h");

    app.redraw_window(wid);
    app.push_a11y_tree(wid);

    assert_the_whole_terminal(&app, wid, "during a held frame");
}

/// A repeated document read owns its returned text and reuses extraction storage.
#[test]
fn document_reads_allocate_only_the_returned_text_once_warm() {
    let app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).unwrap().session;
    let term = app.front_terminal(wid).unwrap().term.clone();
    for (rows, cols) in [(24usize, 80usize), (120, 240)] {
        {
            let mut term = crate::term_lock(&term);
            term.resize(rows as u16, cols as u16);
            term.process(b"\x1b[2J\x1b[H");
            for row in 0..rows {
                term.process(
                    format!("\x1b[{};1Hrow {row} {}", row + 1, "x".repeat(cols - 10)).as_bytes(),
                );
            }
        }
        for _ in 0..3 {
            let _ = app
                .terminal_document_for_test(wid, session, rows, cols, true)
                .unwrap();
        }
        app.windows[&wid].a11y_document_frames.borrow_mut().clear();
        let (cold, cold_allocations) = aterm_heap_count::allocations_in(|| {
            app.terminal_document_for_test(wid, session, rows, cols, true)
                .unwrap()
        });
        let (document, allocations) = aterm_heap_count::allocations_in(|| {
            app.terminal_document_for_test(wid, session, rows, cols, true)
                .unwrap()
        });
        assert!(document.snap.value().starts_with("row 0 "));
        assert_eq!(
            document.snap, cold.snap,
            "retaining storage must preserve the document"
        );
        assert!(
            cold_allocations > 1,
            "CONTROL: discarding scratch must lose its reuse"
        );
        assert_eq!(
            allocations, 1,
            "{rows}x{cols}: only returned text needs ownership"
        );
        crate::term_lock(&term).process(b"\x1b[Hchanged");
        let (changed, changed_allocations) = aterm_heap_count::allocations_in(|| {
            app.terminal_document_for_test(wid, session, rows, cols, true)
                .unwrap()
        });
        assert!(
            changed.snap.value().starts_with("changed"),
            "every read must refill from terminal state"
        );
        assert_eq!(changed_allocations, 1);
        println!(
            "rows={rows} cols={cols} cold_allocations={cold_allocations} warm_allocations={allocations} changed_allocations={changed_allocations}"
        );
    }
}

#[test]
fn complete_client_reads_reuse_frames_for_front_and_split_documents() {
    for split in [false, true] {
        let mut app = crate::scripted_redraw_fixture::scripted_app();
        let wid = WindowId(0);
        crate::scripted_redraw_fixture::feed(&app, b"client read allocation control");
        if split {
            app.split_active_stub_tab(wid);
            app.split_active_stub_tab(wid);
        }
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        for _ in 0..3 {
            let _ = app.client_reads(wid, Some(&plan));
        }
        app.windows[&wid].a11y_document_frames.borrow_mut().clear();
        let (cold, cold_allocations) =
            aterm_heap_count::allocations_in(|| app.client_reads(wid, Some(&plan)));
        let (warm, warm_allocations) =
            aterm_heap_count::allocations_in(|| app.client_reads(wid, Some(&plan)));
        assert_eq!(
            cold, warm,
            "reusing storage must preserve the complete client read"
        );
        assert!(
            cold_allocations > warm_allocations,
            "front/split={split}: extraction storage must survive a complete read pass"
        );
        println!(
            "split={split} cold_allocations={cold_allocations} warm_allocations={warm_allocations}"
        );
    }
}

#[test]
#[ignore = "measures complete accessibility tree allocations; run alone"]
fn complete_terminal_tree_allocation_cost() {
    for split in [false, true] {
        let mut app = crate::scripted_redraw_fixture::scripted_app();
        let wid = WindowId(0);
        crate::scripted_redraw_fixture::feed(
            &app,
            "tree text 中 e\u{301}\r\nsecond row".as_bytes(),
        );
        if split {
            app.split_active_stub_tab(wid);
            app.split_active_stub_tab(wid);
        }
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let control = app.grid_a11y_tree(wid, Some(&plan)).unwrap();
        for sample in 0..3 {
            let (tree, allocations) =
                aterm_heap_count::allocations_in(|| app.grid_a11y_tree(wid, Some(&plan)).unwrap());
            assert_eq!(tree, control);
            let nodes = tree.nodes.len();
            let (_, retained_bytes) = aterm_heap_count::bytes_freed_in(|| drop(tree));
            println!(
                "split={split} sample={sample} nodes={nodes} allocations={allocations} retained_bytes={retained_bytes}"
            );
        }
    }
}

#[test]
fn shared_geometry_panes_match_independent_document_reads() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    let first = app.front_terminal(wid).unwrap().session;
    let second = app.split_active_stub_tab(wid);
    let third = app.split_active_stub_tab(wid);
    for (index, session) in [first, second, third].into_iter().enumerate() {
        let term = app.pool.get(session).unwrap().term.clone();
        let mut term = crate::term_lock(&term);
        term.process(format!("pane {index}> ").as_bytes());
        if index == 0 {
            term.text_selection_mut().start_selection(
                0,
                0,
                SelectionSide::Left,
                SelectionType::Simple,
            );
            term.text_selection_mut()
                .update_selection(0, 3, SelectionSide::Right);
            term.text_selection_mut().complete_selection();
        } else if index == 1 {
            term.process(b"\x1b[?25l");
        }
    }
    app.on_ime_preedit(wid, "日λ".to_owned(), None);
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    assert_eq!(plan.leaves.len(), 3);
    let mut expected = Vec::new();
    for leaf in &plan.leaves {
        let session = app
            .view_store
            .get(leaf.view)
            .unwrap()
            .terminal_session()
            .unwrap();
        let rows = (leaf.rect.size.height.round() as usize).max(1);
        let cols = (leaf.rect.size.width.round() as usize).max(1);
        app.windows[&wid].a11y_document_frames.borrow_mut().clear();
        expected.push(
            app.terminal_document_for_test(wid, session, rows, cols, leaf.focused)
                .unwrap(),
        );
    }
    assert_ne!(
        expected[0].snap.text, expected[1].snap.text,
        "CONTROL: distinct terminal documents"
    );
    assert!(expected[0].selection.is_some());
    assert!(expected[1].snap.cursor.is_none());
    assert!(
        expected[2].snap.value().contains('日'),
        "CONTROL: focused composition was projected"
    );
    for _ in 0..3 {
        let reads = app.client_reads(wid, Some(&plan));
        assert_eq!(reads.panes.len(), expected.len());
        for (pane, fresh) in reads.panes.iter().zip(&expected) {
            assert_eq!(pane.snap, fresh.snap);
            assert_eq!(pane.selection, fresh.selection);
        }
    }
}

/// A terminal's document reads the rows on screen, its caret where it is drawn.
#[test]
fn a_document_reads_the_view_and_a_hidden_caret_is_none() {
    let app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).unwrap().session;
    let (rows, cols) = (usize::from(app.windows[&wid].rows), 40);
    for row in 0..rows + 3 {
        crate::scripted_redraw_fixture::feed(&app, format!("row {row}\r\n").as_bytes());
    }
    let read = |app: &App, focused| {
        app.terminal_document_for_test(wid, session, rows, cols, focused)
            .expect("the document")
            .snap
    };
    let live = read(&app, true);
    assert!(live.value().starts_with("row 4\n"), "{}", live.value());
    assert!(
        live.cursor.is_some(),
        "CONTROL: the live view has its caret"
    );
    assert_eq!(read(&app, false).cursor, None, "an unfocused pane has none");
    crate::scripted_redraw_fixture::feed(&app, b"abcde");
    assert!(
        read(&app, true).cursor.is_some(),
        "CONTROL: it fits this view"
    );
    let narrow = app
        .terminal_document_for_test(wid, session, rows, 3, true)
        .unwrap();
    assert_eq!(
        narrow.snap.cursor, None,
        "a caret past the view's columns is not drawn"
    );

    let term = app.front_terminal(wid).unwrap().term.clone();
    crate::term_lock(&term).scroll_display(2);
    let back = read(&app, true);
    assert!(back.value().starts_with("row 2\n"), "{}", back.value());
    assert_eq!(back.cursor, None, "the caret is below the rows in view");
    crate::term_lock(&term).scroll_display(-2);

    crate::scripted_redraw_fixture::feed(&app, b"\x1b[?25l");
    assert_eq!(read(&app, true).cursor, None, "a hidden caret");
}

/// A tab that comes to the front in a window that cannot present reaches its client whole, with
/// no empty document on the way, and the window has nothing more to send.
#[test]
fn a_tab_switch_in_a_window_that_cannot_present_reads_its_terminal() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    crate::scripted_redraw_fixture::feed(&app, b"text of the first tab");
    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));
    crate::term_lock(&app.pool.get(second).unwrap().term).process(b"text of the second tab");
    app.switch_tab_in(wid, 0);
    app.redraw_window(wid);
    let window = app.windows.get_mut(&wid).unwrap();
    window.a11y_active = true;
    window.bootstrap_present_retry_available = false;
    app.rearm_dropped_present(wid, crate::metrics::PresentDropReason::GpuOccluded);
    assert!(app.windows[&wid].present_retry.parked, "CONTROL");
    app.windows.get_mut(&wid).unwrap().a11y_sent.clear();

    app.switch_tab_in(wid, 1);
    app.push_a11y_tree(wid);
    app.rearm_dropped_present(wid, crate::metrics::PresentDropReason::GpuOccluded);
    app.redraw_window(wid);

    let sent = &app.windows[&wid].a11y_sent;
    assert_eq!(sent.len(), 1, "the press's publish and no other");
    let text = terminal_text(&sent[0]);
    assert_eq!(text.len(), 1, "{text:?}");
    assert!(text[0].contains("text of the second tab"), "{text:?}");
}

/// A window with a client and a terminal that holds `earlier words`, parked with nothing owed.
fn parked_terminal() -> (App, WindowId) {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    crate::scripted_redraw_fixture::feed(&app, b"earlier words");
    park(&mut app, wid);
    (app, wid)
}

fn park(app: &mut App, wid: WindowId) {
    app.redraw_window(wid);
    let window = app.windows.get_mut(&wid).unwrap();
    window.a11y_active = true;
    window.bootstrap_present_retry_available = false;
    app.push_a11y_tree(wid);
    app.rearm_dropped_present(wid, crate::metrics::PresentDropReason::GpuOccluded);
    assert!(app.windows[&wid].present_retry.parked, "CONTROL");
    app.windows.get_mut(&wid).unwrap().a11y_sent.clear();
}

/// Asks the parked window once its pace allows, and returns what its client was sent.
fn asked_at_the_pace(app: &mut App, wid: WindowId) -> Vec<aterm_a11y::model::TreeUpdate> {
    let at = app.windows[&wid]
        .a11y_parked_next
        .expect("parking read the window");
    app.publish_in_a_parked_window(wid, at);
    std::mem::take(&mut app.windows.get_mut(&wid).unwrap().a11y_sent)
}

/// A terminal that prints in a window that cannot present tells its client once.
#[test]
fn a_terminal_that_prints_in_a_parked_window_tells_its_client_once() {
    let (mut app, wid) = parked_terminal();
    assert!(
        asked_at_the_pace(&mut app, wid).is_empty(),
        "nothing printed"
    );

    crate::scripted_redraw_fixture::feed(&app, b" and later words");

    let sent = asked_at_the_pace(&mut app, wid);
    assert_eq!(sent.len(), 1, "told");
    let text = terminal_text(&sent[0]);
    assert!(
        text[0].contains("earlier words and later words"),
        "{text:?}"
    );
    assert!(asked_at_the_pace(&mut app, wid).is_empty(), "and only once");
}

/// No frame paces a parked window, so a stream is told at the pace, and what its last wake
/// could not tell falls due with nothing further to wake it.
#[test]
fn a_stream_in_a_parked_window_is_told_at_the_pace_to_its_last_bytes() {
    let (mut app, wid) = parked_terminal();
    crate::scripted_redraw_fixture::feed(&app, b" first");
    assert_eq!(asked_at_the_pace(&mut app, wid).len(), 1, "CONTROL");
    let next = app.windows[&wid].a11y_parked_next.unwrap();
    let soon = next - PARKED_TELL_PACE / 2;

    for bytes in [&b" second"[..], b" third", b" fourth"] {
        crate::scripted_redraw_fixture::feed(&app, bytes);
        app.publish_in_a_parked_window(wid, soon);
    }

    assert!(app.windows[&wid].a11y_sent.is_empty(), "held to the pace");
    assert_eq!(app.windows[&wid].a11y_parked_due, Some(next));
    app.serve_parked_tells(soon);
    assert!(
        app.windows[&wid].a11y_sent.is_empty(),
        "not before it is due"
    );

    app.serve_parked_tells(next);

    let sent = &app.windows[&wid].a11y_sent;
    assert_eq!(sent.len(), 1, "one tell");
    assert!(terminal_text(&sent[0])[0].contains("first second third fourth"));
    assert_eq!(app.windows[&wid].a11y_parked_due, None);
}

/// A frame ends a pace owed: what it publishes is the newest there is.
#[test]
fn a_window_that_presents_again_owes_no_parked_tell() {
    let (mut app, wid) = parked_terminal();
    crate::scripted_redraw_fixture::feed(&app, b" first");
    let at = app.windows[&wid].a11y_parked_next.unwrap();
    app.publish_in_a_parked_window(wid, at - PARKED_TELL_PACE / 2);
    let window = app.windows.get_mut(&wid).unwrap();
    assert!(window.a11y_parked_due.is_some(), "CONTROL");
    window.present_retry.parked = false;

    app.serve_parked_tells(at);

    assert!(app.windows[&wid].a11y_sent.is_empty());
    assert_eq!(app.windows[&wid].a11y_parked_due, None);
}

/// A screen part-way through a synchronized update is one no frame would show.
#[test]
fn a_parked_read_waits_out_a_synchronized_update() {
    let (mut app, wid) = parked_terminal();
    crate::scripted_redraw_fixture::feed(&app, b"\x1b[?2026h\x1b[2J");
    assert!(
        asked_at_the_pace(&mut app, wid).is_empty(),
        "the blank screen"
    );
    assert!(
        app.windows[&wid].a11y_parked_due.is_some(),
        "and it comes back"
    );

    crate::scripted_redraw_fixture::feed(&app, b"\x1b[Hrepainted\x1b[?2026l");

    let sent = asked_at_the_pace(&mut app, wid);
    assert_eq!(sent.len(), 1);
    assert!(terminal_text(&sent[0])[0].starts_with("repainted"));
}

/// As a frame does, a read stops waiting for an update that does not end.
#[test]
fn a_parked_read_tells_an_update_that_never_ends() {
    let (mut app, wid) = parked_terminal();
    crate::scripted_redraw_fixture::feed(&app, b"\x1b[?2026hstuck");
    assert!(asked_at_the_pace(&mut app, wid).is_empty(), "CONTROL");

    let sent = asked_at_the_pace(&mut app, wid);

    assert_eq!(sent.len(), 1);
    assert!(terminal_text(&sent[0])[0].contains("earlier wordsstuck"));
}

/// A page that owes a tell owes it the same screen a terminal would: a half-drawn pane beside
/// it is no reason to hurry.
#[test]
fn a_page_s_tell_waits_out_the_update_of_the_terminal_beside_it() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    park(&mut app, wid);
    assert_eq!(
        asked_at_the_pace(&mut app, wid).len(),
        1,
        "CONTROL: it owes"
    );

    crate::term_lock(&app.pool.get(session).unwrap().term).process(b"\x1b[?2026h\x1b[2J");

    assert!(asked_at_the_pace(&mut app, wid).is_empty());
}

/// A press that closes a menu changes nothing a terminal holds, and a client left with the row
/// it pressed would press it again to no effect.
#[test]
fn a_menu_that_closes_in_a_parked_window_leaves_its_client() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_tab_context_menu(wid, 0, 3, true));
    park(&mut app, wid);
    assert!(asked_at_the_pace(&mut app, wid).is_empty(), "CONTROL");

    assert!(app.close_tab_menu(wid));

    let sent = asked_at_the_pace(&mut app, wid);
    assert_eq!(sent.len(), 1);
    let menu = aterm_a11y::model::Role::Menu;
    assert!(sent[0].nodes.iter().all(|(_, node)| node.role() != menu));
}

/// The field holds the keys from the moment it opens, frame or no frame, and each key it takes
/// is one its client has to hear.
#[test]
fn a_session_field_in_a_parked_window_reaches_its_client_as_it_changes() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    park(&mut app, wid);
    let tab = app.windows[&wid].tab_set.active().unwrap().id;

    assert!(app.begin_session_rename(wid, tab));
    let sent = asked_at_the_pace(&mut app, wid);
    assert_eq!(sent.len(), 1, "it opened");

    let edit = app.windows.get_mut(&wid).unwrap().rename_edit.as_mut();
    edit.unwrap().text = "build".to_owned();
    let sent = asked_at_the_pace(&mut app, wid);
    assert_eq!(sent.len(), 1, "it changed");
    let field = aterm_a11y::model::Role::TextInput;
    let words = sent[0].nodes.iter().find(|(_, node)| node.role() == field);
    assert_eq!(words.expect("the field").1.value(), Some("build"));
}

#[test]
fn a_find_field_in_a_parked_window_reaches_its_client() {
    let (mut app, wid) = parked_terminal();
    let mut search = crate::app_search::SearchState::default();
    search.query = "ab".to_owned();
    app.windows.get_mut(&wid).unwrap().search = Some(search);

    assert_eq!(asked_at_the_pace(&mut app, wid).len(), 1);
}

/// A question stops the work until it is answered, and nothing on the terminal changes for it.
#[test]
fn a_question_in_a_parked_window_reaches_its_client() {
    let (mut app, wid) = parked_terminal();
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(0),
        "one\ntwo\n".to_owned(),
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));

    let sent = asked_at_the_pace(&mut app, wid);

    assert_eq!(sent.len(), 1);
    let asked = sent[0]
        .nodes
        .iter()
        .any(|(_, node)| node.role() == aterm_a11y::model::Role::AlertDialog);
    assert!(asked, "the question");
}

/// Two blank tabs read alike, and the client still has to hear that it moved.
#[test]
fn a_tab_that_reads_like_the_last_one_still_reaches_its_client() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));
    app.switch_tab_in(wid, 0);
    park(&mut app, wid);

    app.switch_tab_in(wid, 1);
    app.rearm_dropped_present(wid, crate::metrics::PresentDropReason::GpuOccluded);

    assert_eq!(asked_at_the_pace(&mut app, wid).len(), 1);
}

/// A wait that ended with a frame says nothing of the next time the window parks.
#[test]
fn a_frame_ends_a_parked_read_s_wait() {
    let (mut app, wid) = parked_terminal();
    crate::scripted_redraw_fixture::feed(&app, b"\x1b[?2026h\x1b[2J");
    assert!(asked_at_the_pace(&mut app, wid).is_empty(), "CONTROL");
    assert!(app.windows[&wid].a11y_parked_held, "CONTROL");

    app.push_a11y_tree(wid);

    assert!(!app.windows[&wid].a11y_parked_held);
}

/// A client holds a caret and a view as well as words: each reaches it with no cell printed.
#[test]
fn a_parked_client_is_told_what_moves_without_printing() {
    type Change = (&'static str, fn(&mut App, WindowId));
    let changes: [Change; 3] = [
        ("a caret that moves", |app, _| {
            crate::scripted_redraw_fixture::feed(app, b"\x1b[A");
        }),
        ("a view scrolled back", |app, wid| {
            let term = app.front_terminal(wid).unwrap().term.clone();
            crate::term_lock(&term).scroll_display(1);
        }),
        ("a selection", |app, wid| app.select_all_in(wid)),
    ];
    for (change, make) in changes {
        let mut app = crate::scripted_redraw_fixture::scripted_app();
        let wid = WindowId(0);
        let rows = usize::from(app.windows[&wid].rows);
        for row in 0..rows + 2 {
            crate::scripted_redraw_fixture::feed(&app, format!("row {row}\r\n").as_bytes());
        }
        park(&mut app, wid);

        make(&mut app, wid);

        assert_eq!(asked_at_the_pace(&mut app, wid).len(), 1, "{change}");
    }
}

/// The keyboard moving to another pane moves the client's caret with it.
#[test]
fn a_focus_move_in_a_parked_window_reaches_its_client() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    let _ = app.split_active_stub_tab(wid);
    park(&mut app, wid);

    assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));

    assert_eq!(asked_at_the_pace(&mut app, wid).len(), 1);
}

/// A window whose terminal tab composed last, with a Settings and terminal tab behind it:
/// the app, the window and that tab's index.
fn mixed_tab_behind_a_terminal_tab() -> (App, WindowId, usize) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 120;
        window.rows = 42;
    }
    let first = app.front_terminal(wid).unwrap().term.clone();
    crate::term_lock(&first).process(b"text of the front tab");
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    crate::term_lock(&app.pool.get(session).unwrap().term).process(b"text of the mixed tab");
    let mixed = app.windows[&wid].tab_set.active_index().unwrap();
    set_a11y_active(&mut app, wid, true);
    app.switch_tab_in(wid, 0);
    let terminal = app.prepare_terminal_capture_grid(wid);
    assert!(terminal.is_some(), "the front tab's frame composes");
    assert!(
        app.windows[&wid]
            .input_scratch
            .cells
            .iter()
            .flatten()
            .any(|cell| cell.ch == 't'),
        "the cells hold the front tab's text"
    );
    (app, wid, mixed)
}

/// The cells still hold the front tab's text. A tab that comes back reads its own terminal at
/// once, with no frame, beside its page.
#[test]
fn a_tab_brought_back_reads_its_own_terminal_at_once() {
    let (mut app, wid, mixed) = mixed_tab_behind_a_terminal_tab();

    app.switch_tab_in(wid, mixed);
    let (live, _) = app
        .take_native_accessibility_update(wid)
        .expect("a native tree")
        .expect("the page reads from the model");

    let text = terminal_text(&live);
    assert_eq!(text.len(), 1, "{text:?}");
    assert!(text[0].contains("text of the mixed tab"), "{text:?}");
    assert!(!text[0].contains("text of the front tab"), "{text:?}");
}

/// The same read serves a tab of terminals, which has no page to give at once.
#[test]
fn a_terminal_tab_brought_back_reads_its_own_terminal_at_once() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let first = app.front_terminal(wid).unwrap().term.clone();
    crate::term_lock(&first).process(b"text of the first tab");
    assert!(app.prepare_terminal_capture_grid(wid).is_some());
    let shown = app.grid_a11y_tree(wid, None).unwrap();
    assert!(terminal_text(&shown)[0].contains("text of the first tab"));

    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));
    crate::term_lock(&app.pool.get(second).unwrap().term).process(b"text of the second tab");

    let moved = app.grid_a11y_tree(wid, None).unwrap();
    let text = terminal_text(&moved);
    assert_eq!(text.len(), 1, "{text:?}");
    assert!(text[0].contains("text of the second tab"), "{text:?}");
    assert!(!text[0].contains("text of the first tab"), "{text:?}");
}

/// A layout no frame has composed still reads its terminal, at the leaf's own size.
#[test]
fn a_moved_layout_reads_its_terminal_at_once() {
    for change in ["narrower", "a chrome row"] {
        let (mut app, wid, mixed) = mixed_tab_behind_a_terminal_tab();
        app.switch_tab_in(wid, mixed);
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());

        match change {
            "narrower" => app.windows.get_mut(&wid).unwrap().cols = 100,
            _ => app.tab_strip_rows = app.tab_strip_rows.saturating_add(1),
        }
        let (live, _) = app
            .take_native_accessibility_update(wid)
            .expect("a native tree")
            .expect("the page reads from the model");

        let text = terminal_text(&live);
        assert_eq!(text.len(), 1, "{change}: {text:?}");
        assert!(
            text[0].contains("text of the mixed tab"),
            "{change}: {text:?}"
        );
    }
}

/// A lone page's frame leaves blank cells, which say nothing of the terminal beside it.
#[test]
fn a_tab_brought_back_from_a_lone_page_reads_its_terminal() {
    let (mut app, wid, mixed) = mixed_tab_behind_a_terminal_tab();
    app.switch_tab_in(wid, mixed);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let path = scratch.path().join("lone.txt");
    std::fs::write(&path, "text\n").unwrap();
    app.open_document_tab_in_window(
        wid,
        AppKind::Editor,
        &crate::native_document_host::path_to_file_uri(&path).unwrap(),
    )
    .unwrap();
    assert!(app.prepare_native_input_scratch(wid));

    app.switch_tab_in(wid, mixed);
    let (live, _) = app
        .take_native_accessibility_update(wid)
        .expect("a native tree")
        .expect("the page reads from the model");

    let text = terminal_text(&live);
    assert_eq!(text.len(), 1, "{text:?}");
    assert!(text[0].contains("text of the mixed tab"), "{text:?}");
}

/// A client's press in the page keeps the terminal beside it.
#[test]
fn a_focus_move_keeps_the_pane() {
    let (mut app, wid, mixed) = mixed_tab_behind_a_terminal_tab();
    app.switch_tab_in(wid, mixed);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let native = plan
        .leaves
        .iter()
        .find(|leaf| !leaf.focused)
        .expect("the unfocused page")
        .view;
    assert!(
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.tab_set.active_mut())
            .is_some_and(|tab| tab.set_focus(native))
    );
    app.sync_window(wid);
    app.invalidate_native_ui_cache(wid);

    let (live, _) = app
        .take_native_accessibility_update(wid)
        .expect("a native tree")
        .expect("the live tree");

    let text = terminal_text(&live);
    assert_eq!(text.len(), 1, "{text:?}");
    assert!(text[0].contains("text of the mixed tab"), "{text:?}");
}

/// A paint owed by one pane leaves the card, so a sibling whose frame still answers is read,
/// not compiled again. An effect that repaints drops the card, and then the sibling compiles.
#[test]
fn a_publish_compiles_only_the_pane_that_owes_a_paint() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 140;
        window.rows = 48;
    }
    assert!(app.open_settings_tab(SettingsRoute::About));
    let (instance, view) = app.active_native_view(wid).unwrap();
    app.split_active_with_native(
        wid,
        crate::tab_model::SplitAxis::Vertical,
        instance,
        AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
            &app.config,
        ))),
    )
    .unwrap();
    set_a11y_active(&mut app, wid, true);
    framed_tree(&mut app, wid);
    app.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
    let compiles = crate::app_native::native_ui_compile_count();

    app.take_native_accessibility_update(wid)
        .expect("a native tree")
        .expect("the tree");

    assert_eq!(crate::app_native::native_ui_compile_count(), compiles + 1);
}

/// A press that queues work retires the page's review, so its owners move with the press. The
/// tree goes out under the new owners, not as a root-only tree.
#[test]
fn a_press_that_retires_its_review_publishes_the_page() {
    let (mut app, wid, view, _) = vault_page_with_draft();
    let press = publish_vault_action(&mut app, wid, view, "system-state/vault/join", None);
    let owners = |app: &App| {
        let published = app.windows[&wid].native_a11y_published.as_ref();
        published.expect("the page routes").owners().to_vec()
    };
    let before = owners(&app);
    app.windows.get_mut(&wid).unwrap().a11y_sent.clear();

    app.on_native_accessibility_action(wid, press);

    assert_ne!(owners(&app), before, "the press retired the review");
    assert_ne!(
        app.windows[&wid].a11y_sent.last(),
        Some(&crate::a11y_tree::empty_tree())
    );
}

#[test]
fn deferred_vault_join_cannot_run_after_a_same_plan_busy_publication() {
    thread_local! {
        static APPLIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    for busy in [false, true] {
        APPLIED.with(|applied| applied.set(0));
        let (mut app, wid, view, _) = vault_page_with_draft();
        app.set_system_state_handlers_for_test(crate::system_state_host::InitialSetupHandlers {
            apply: |plan, typed, password, receipt| {
                assert!(typed);
                assert_eq!(plan.vault_url, "http://draft.invalid:7484/v1/team");
                assert_eq!(
                    password
                        .as_ref()
                        .and_then(aterm_secrets::SecretBytes::as_str),
                    Some("inert-password")
                );
                APPLIED.with(|applied| applied.set(applied.get() + 1));
                receipt.accept();
                true
            },
            ..crate::system_state_host::InitialSetupHandlers::inert_for_test()
        });
        let saved = publish_vault_action(&mut app, wid, view, "system-state/vault/join", None);
        let idle = crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            ..crate::SetupStatus::default()
        };
        app.publish_system_state(crate::SetupStatus {
            busy,
            ..idle.clone()
        });
        app.on_native_accessibility_action(wid, saved);
        assert_eq!(
            APPLIED.with(std::cell::Cell::get),
            usize::from(!busy),
            "only the idle saved Join is enabled"
        );
        if !busy {
            continue;
        }

        app.publish_system_state(idle);
        let current = publish_vault_action(&mut app, wid, view, "system-state/vault/join", None);
        app.on_native_accessibility_action(wid, current);
        assert_eq!(
            APPLIED.with(std::cell::Cell::get),
            1,
            "an idle current Join works"
        );
    }
}

#[test]
fn deferred_vault_values_cannot_edit_after_a_same_plan_busy_publication() {
    use crate::native_app::NativeApp;
    for busy in [false, true] {
        let (mut app, wid, view, instance) = vault_page_with_draft();
        let values = |app: &App| {
            let Some(NativeApp::SystemState(page)) = app.native_runtime.app(instance) else {
                panic!("the page is open");
            };
            (
                page.field_projection("system-state/url").unwrap().text,
                page.field_projection("system-state/password").unwrap().text,
            )
        };
        let before = values(&app);
        let idle = crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            ..crate::SetupStatus::default()
        };
        // Each request is minted on an idle page and delivered past the status. The first
        // field's publish replaces the routes, so the second mints on its own idle page.
        for (key, text) in [
            ("system-state/url", "http://later.invalid:7484/v1/team"),
            ("system-state/password", "different-inert-password"),
        ] {
            app.publish_system_state(idle.clone());
            let request = publish_vault_action(&mut app, wid, view, key, Some(text));
            app.publish_system_state(crate::SetupStatus {
                busy,
                ..idle.clone()
            });
            app.on_native_accessibility_action(wid, request);
        }
        let expected = if busy {
            before
        } else {
            (
                "http://later.invalid:7484/v1/team".into(),
                "•".repeat("different-inert-password".len()),
            )
        };
        assert_eq!(values(&app), expected, "only idle saved fields may change");

        app.publish_system_state(idle);
        for (key, text) in [
            ("system-state/url", "http://current.invalid:7484/v1/team"),
            ("system-state/password", "current-inert-password"),
        ] {
            let current = publish_vault_action(&mut app, wid, view, key, Some(text));
            app.on_native_accessibility_action(wid, current);
        }
        assert_eq!(
            values(&app),
            (
                "http://current.invalid:7484/v1/team".into(),
                "•".repeat("current-inert-password".len()),
            )
        );
    }
}

#[test]
fn queued_accessibility_choice_cannot_answer_a_replacement_setup_offer() {
    use crate::workstation_setup::Request;
    use aterm_types::macos_setup::{Group, Plan, Setting};
    thread_local! {
        static APPLIED: std::cell::RefCell<Vec<Plan>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    for (by_offer, republish) in [(true, false), (true, true), (false, false), (false, true)] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        assert!(app.open_system_state_tab());
        app.set_system_state_handlers_for_test(crate::system_state_host::InitialSetupHandlers {
            apply: |plan, _, _, receipt| {
                APPLIED.with(|applied| applied.borrow_mut().push(plan));
                receipt.accept();
                true
            },
            ..crate::system_state_host::InitialSetupHandlers::inert_for_test()
        });
        let (_, view) = app.active_native_view(wid).unwrap();
        // The worker's first read names the plan in force: until it does, no line shows an
        // on or off to press.
        app.publish_system_state(crate::SetupStatus {
            plan: Some(Plan::default()),
            ..crate::SetupStatus::default()
        });
        let generation = app.native_runtime.view_generation(view).unwrap();
        let old_press = publish_leave(&mut app, wid, view);

        let mut replacement = Plan {
            vault_url: "http://new-review.invalid:7484/v1/team".into(),
            ..Plan::default()
        };
        replacement.selection.set(Setting::Dock, false);
        let (send, _receive) = std::sync::mpsc::channel();
        if by_offer {
            assert!(
                app.native_runtime
                    .offer_system_state(Request::new(replacement.clone(), send))
            );
        } else {
            app.native_runtime.publish_system_state(crate::SetupStatus {
                plan: Some(replacement.clone()),
                ..crate::SetupStatus::default()
            });
        }
        assert_eq!(app.native_runtime.view_generation(view), Some(generation));
        if republish {
            let _fresh = publish_leave(&mut app, wid, view);
        }
        app.on_native_accessibility_action(wid, old_press);
        assert!(
            APPLIED.with(|applied| applied.borrow().is_empty()),
            "an old press cannot answer a different offer, before or after its new frame publishes"
        );

        let fresh = publish_leave(&mut app, wid, view);
        // A tick answers an offer up at once (init-on-by-default design §6.1), so only the
        // page with no offer up is ticked here.
        if !by_offer {
            app.native_runtime.tick_system_state();
            app.native_runtime.tick_system_state();
        }
        app.native_runtime.publish_system_state(crate::SetupStatus {
            plan: Some(replacement.clone()),
            ..crate::SetupStatus::default()
        });
        app.dispatch_native_view_event(wid, view, AppEvent::FocusChanged(None))
            .unwrap();
        let after_repaint = publish_leave(&mut app, wid, view);
        assert_eq!(
            fresh.context, after_repaint.context,
            "ticks, status and focus do not retire the current page"
        );
        app.on_native_accessibility_action(wid, fresh.clone());
        let mut expected = replacement.clone();
        // The Mac settings line governs the Input and Desktop groups.
        for setting in Group::Input
            .settings()
            .iter()
            .chain(Group::Desktop.settings())
        {
            expected.selection.set(*setting, false);
        }
        // The current offer's switch press answers it at once, as a press on the page does
        // with no offer up.
        assert_eq!(
            APPLIED.with(|applied| std::mem::take(&mut *applied.borrow_mut())),
            [expected.clone()],
            "the current page must remain actionable"
        );
        app.native_runtime
            .accept_system_state_apply(expected.clone());
        app.native_runtime.publish_system_state(crate::SetupStatus {
            plan: Some(expected),
            ..crate::SetupStatus::default()
        });
        app.on_native_accessibility_action(wid, fresh);
        assert!(
            APPLIED.with(|applied| applied.borrow().is_empty()),
            "a press already taken cannot submit a second time"
        );
    }
}

#[test]
fn a_check_intent_retires_a_queued_accessibility_choice_before_it_can_run() {
    thread_local! {
        static CHECKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 90;
    window.cols = 160;
    assert!(app.open_system_state_tab());
    app.publish_system_state(crate::SetupStatus {
        plan: Some(aterm_types::macos_setup::Plan::default()),
        ..crate::SetupStatus::default()
    });
    app.set_system_state_handlers_for_test(crate::system_state_host::InitialSetupHandlers {
        apply: |_, _, _, _| panic!("an old accessibility choice cannot follow Check"),
        check: |_, _| {
            CHECKS.with(|checks| checks.set(checks.get() + 1));
            crate::system_state_host::CheckStarted {
                reading: true,
                check: true,
            }
        },
        ..crate::system_state_host::InitialSetupHandlers::inert_for_test()
    });
    let (_, view) = app.active_native_view(wid).unwrap();
    let old_press = publish_leave(&mut app, wid, view);
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("system-state/check"),
            value: None,
        }),
    )
    .unwrap();
    app.on_native_accessibility_action(wid, old_press);
    assert_eq!(CHECKS.with(std::cell::Cell::get), 1);
    assert!(
        app.native_runtime.take_system_state_action().is_none(),
        "the old press cannot follow a Check"
    );
}

#[test]
fn platform_focus_gives_the_editor_visible_focus_and_an_ime_anchor() {
    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let path = scratch.path().join("focus.txt");
    let source = "aéz\nsecond λ\n";
    std::fs::write(&path, source).unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab_in_window(
        wid,
        AppKind::Editor,
        &crate::native_document_host::path_to_file_uri(&path).unwrap(),
    )
    .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::EditorSetSelection { anchor: 1, head: 3 },
    )
    .unwrap();
    app.reconcile_active_editor_viewport(wid).unwrap();
    let editor_state = |app: &App| {
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("the Editor view remains attached")
        };
        state.as_ref().clone()
    };
    let document_state = |app: &App| {
        let snapshot = app.document_store.snapshot(document).unwrap();
        (
            snapshot.seq,
            snapshot.text,
            app.document_store.dirty(document),
            app.document_store.checkpoint_seq(document),
        )
    };
    let before = editor_state(&app);
    let document_before = document_state(&app);
    let input_before = before.common.input_lifetime();
    assert!(before.common.last_focus.is_none());
    assert!(!before.common.focus_visible);
    let key = UiKey::new("editor/buffer");
    let compiled = app.compiled_native_ui(wid).unwrap();
    assert!(compiled.focus_order.contains(&key));
    let anchor_before = compiled
        .ime_caret_rect()
        .expect("the quiet editor already owns text input");
    let projection = project_native_accessibility(&compiled, None).unwrap();
    let node = projection.id_for_key(&key).unwrap();
    let (_, routes) = projection.into_update_and_routes();
    let generation = app.native_runtime.view_generation(view).unwrap();
    app.windows.get_mut(&wid).unwrap().native_a11y_published =
        Some(PublishedNativeAccessibility::new(view, generation, routes));
    let request = ActionRequest {
        context: Some(aterm_a11y::model::ActionContext::new(
            view.get(),
            generation,
        )),
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: node,
        data: None,
    };
    let mut stale = request.clone();
    stale.context = Some(aterm_a11y::model::ActionContext::new(
        view.get(),
        generation.saturating_add(1),
    ));
    app.on_native_accessibility_action(wid, stale);
    assert_eq!(editor_state(&app), before);
    assert!(std::sync::Arc::ptr_eq(
        &input_before,
        &editor_state(&app).common.input_lifetime()
    ));

    app.on_native_accessibility_action(wid, request.clone());
    let focused = editor_state(&app);
    assert_eq!(focused.common.last_focus.as_ref(), Some(&key));
    let input_focused = focused.common.input_lifetime();
    assert!(!std::sync::Arc::ptr_eq(&input_before, &input_focused));
    let mut failures = Vec::new();
    if !focused.common.focus_visible {
        failures.push("platform Focus did not make the Editor field visibly focused");
    }
    let painted = app.compiled_native_ui(wid).unwrap();
    let viewport = painted
        .paint
        .iter()
        .find_map(|paint| {
            if paint.key != key {
                return None;
            }
            match &paint.content {
                crate::native_ui::UiContent::TextViewport(spec) => Some(spec),
                _ => None,
            }
        })
        .unwrap();
    if !viewport.focused {
        failures.push("platform Focus did not reach the focused viewport projection");
    }
    if painted.ime_caret_rect() != Some(anchor_before) {
        failures.push("platform Focus moved the existing text target's IME anchor");
    }
    app.on_native_accessibility_action(wid, request);
    assert!(std::sync::Arc::ptr_eq(
        &input_focused,
        &editor_state(&app).common.input_lifetime()
    ));
    assert_eq!(editor_state(&app).buffer, before.buffer);
    assert_eq!(document_state(&app), document_before);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    assert!(app.structural_invariants_ok());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn platform_focus_routes_to_exact_live_view_and_rejects_stale_generation() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(SettingsRoute::About));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    let generation = app
        .native_runtime
        .view_generation(view)
        .expect("live view generation");
    let compiled = app.compiled_native_ui(wid).expect("compiled native UI");
    let key = compiled
        .focus_order
        .first()
        .cloned()
        .expect("focusable Settings control");
    let node = stable_node_id(&key);
    let (_, routes) = project_native_accessibility(&compiled, None)
        .expect("native accessibility projection")
        .into_update_and_routes();
    app.windows
        .get_mut(&wid)
        .expect("headless window")
        .native_a11y_published = Some(PublishedNativeAccessibility::new(view, generation, routes));
    app.native_runtime
        .view_state_mut(view)
        .expect("live view")
        .common_mut()
        .last_focus = None;
    let request = ActionRequest {
        context: Some(aterm_a11y::model::ActionContext::new(
            view.get(),
            generation,
        )),
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: node,
        data: None,
    };

    app.on_native_accessibility_action(wid, request.clone());
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .expect("live view")
            .common()
            .last_focus,
        Some(key.clone())
    );
    let visible_focus = app
        .compiled_native_ui(wid)
        .unwrap()
        .semantic(&key)
        .and_then(|node| node.state)
        .is_some_and(|state| state.focus_visible);

    app.native_runtime
        .view_state_mut(view)
        .unwrap()
        .common_mut()
        .last_focus = None;
    let winit_id = aterm_window::window::WindowId::from(17u64);
    app.winit_to_window.insert(winit_id, wid);
    let mut delayed = request.clone();
    delayed.context = Some(aterm_a11y::model::ActionContext::new(
        view.get(),
        generation.saturating_sub(1),
    ));
    app.on_accessibility_event(crate::a11y_adapter::Event {
        window_id: winit_id,
        window_event: crate::a11y_adapter::WindowEvent::ActionRequested(delayed),
    });
    assert!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .last_focus
            .is_none(),
        "Wake delivery must not restamp an old action with the current lifecycle"
    );

    let (_, routes) = project_native_accessibility(&compiled, None)
        .expect("native accessibility projection")
        .into_update_and_routes();
    app.windows
        .get_mut(&wid)
        .expect("headless window")
        .native_a11y_published = Some(PublishedNativeAccessibility::new(
        view,
        generation + 1,
        routes,
    ));
    app.native_runtime
        .view_state_mut(view)
        .expect("live view")
        .common_mut()
        .last_focus = None;

    app.on_native_accessibility_action(wid, request);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .expect("live view")
            .common()
            .last_focus,
        None,
        "a route stamped for another lifecycle must not dispatch"
    );
    assert!(
        app.windows
            .get(&wid)
            .expect("headless window")
            .native_a11y_published
            .is_none(),
        "stale routes are discarded fail-closed"
    );
    assert!(
        visible_focus,
        "platform Focus exposes the Settings focus indicator"
    );
}

#[test]
fn platform_click_dispatches_the_published_key_action_pair() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = 140;
        window.rows = 50;
    }
    assert!(app.open_settings_tab(SettingsRoute::About));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    let generation = app
        .native_runtime
        .view_generation(view)
        .expect("live view generation");
    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(AppViewState::Settings(settings)) if settings.route == SettingsRoute::About
    ));

    let compiled = app.compiled_native_ui(wid).expect("compiled native UI");
    let key = UiKey::new("settings/nav/appearance");
    let semantic = compiled.semantic(&key).expect("Appearance navigation node");
    assert_eq!(
        semantic.action.as_ref().map(|action| action.as_str()),
        Some("settings/route/appearance")
    );
    let projection =
        project_native_accessibility(&compiled, None).expect("native accessibility projection");
    let node = projection
        .id_for_key(&key)
        .expect("stable Appearance node id");
    let (_, routes) = projection.into_update_and_routes();
    app.windows
        .get_mut(&wid)
        .expect("headless window")
        .native_a11y_published = Some(PublishedNativeAccessibility::new(view, generation, routes));

    app.on_native_accessibility_action(
        wid,
        ActionRequest {
            context: Some(aterm_a11y::model::ActionContext::new(
                view.get(),
                generation,
            )),
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: node,
            data: None,
        },
    );

    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(AppViewState::Settings(settings))
            if settings.route == SettingsRoute::Appearance
    ));
}

fn assert_native_sibling_geometry(scale: f64, axis: crate::tab_model::SplitAxis) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.backend.set_pad((12.0 * scale).round() as usize);
    app.seed_headless_boot_metrics();
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 140;
        window.rows = 48;
        window.scale = scale;
    }
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (instance, first) = app.active_native_view(wid).expect("first Settings view");
    let second = app
        .split_active_with_native(
            wid,
            axis,
            instance,
            AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
                &app.config,
            ))),
        )
        .expect("second Settings view");
    assert_eq!(app.active_native_view(wid), Some((instance, second)));
    set_a11y_active(&mut app, wid, true);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());

    let (frame_x, frame_y) = app.frame_origin(wid);
    let (card_x, card_y) = {
        let card = app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("presented composite card");
        (i64::from(card.dx), i64::from(card.dy))
    };
    let root_keys = [first, second].map(|view| {
        app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("retained native raster")
            .compiled
            .semantics
            .iter()
            .find(|semantic| semantic.parent.is_none())
            .expect("semantic root")
            .key
            .clone()
    });
    let focus_key = app.windows[&wid].leaf_render_cache[&first]
        .native
        .as_ref()
        .unwrap()
        .compiled
        .focus_order
        .first()
        .cloned()
        .expect("focusable first Settings control");
    let (update, published) = app
        .take_native_accessibility_update(wid)
        .expect("visible native siblings")
        .expect("valid composite tree");
    assert_eq!(published.owners().len(), 2);
    let window_root = update.tree.as_ref().expect("window tree").root;
    let window_node = update
        .nodes
        .iter()
        .find(|(id, _)| *id == window_root)
        .expect("window root");
    let roots = [
        stable_node_id_for_view(first, &root_keys[0]),
        stable_node_id_for_view(second, &root_keys[1]),
    ];
    assert_ne!(
        roots[0], roots[1],
        "identical sibling keys are view-qualified"
    );
    assert_eq!(window_node.1.children(), roots);

    for (view, root) in [first, second].into_iter().zip(roots) {
        let raster = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("retained presented leaf");
        let x = frame_x + card_x + i64::from(raster.presented_x);
        let y = frame_y + card_y + i64::from(raster.presented_y);
        let expected =
            aterm_a11y::model::Affine::translate(aterm_a11y::model::Vec2::new(x as f64, y as f64))
                * aterm_a11y::model::Affine::scale(scale);
        assert_eq!(
            update
                .nodes
                .iter()
                .find(|(id, _)| *id == root)
                .expect("native leaf root")
                .1
                .transform(),
            Some(&expected),
            "accessibility geometry must equal the retained raster placement at {scale}x"
        );
        let owner = published.route_owner(root).expect("root route owner");
        assert_eq!(owner.view, view);
        assert_eq!(
            owner.generation,
            app.native_runtime.view_generation(view).unwrap()
        );
        if view == second {
            let omitted_pad = aterm_a11y::model::Affine::translate(aterm_a11y::model::Vec2::new(
                (frame_x + i64::from(raster.presented_x)) as f64,
                y as f64,
            )) * aterm_a11y::model::Affine::scale(scale);
            assert_ne!(
                expected, omitted_pad,
                "omitting the compositor card's win_pad destination must fail at {scale}x"
            );
            match axis {
                crate::tab_model::SplitAxis::Horizontal => {
                    assert!(raster.presented_x > 0);
                    let omitted_leaf = aterm_a11y::model::Affine::translate(
                        aterm_a11y::model::Vec2::new((frame_x + card_x) as f64, y as f64),
                    ) * aterm_a11y::model::Affine::scale(scale);
                    assert_ne!(
                        expected, omitted_leaf,
                        "omitting the retained leaf x destination must fail at {scale}x"
                    );
                }
                crate::tab_model::SplitAxis::Vertical => {
                    assert!(raster.presented_y > 0);
                    let omitted_leaf = aterm_a11y::model::Affine::translate(
                        aterm_a11y::model::Vec2::new(x as f64, (frame_y + card_y) as f64),
                    ) * aterm_a11y::model::Affine::scale(scale);
                    assert_ne!(
                        expected, omitted_leaf,
                        "omitting the retained leaf y destination must fail at {scale}x"
                    );
                }
            }
        }
    }
    assert_eq!(
        update.focus,
        stable_node_id_for_view(second, &root_keys[1]),
        "the focused native sibling owns accessibility focus"
    );

    if scale == 1.0 {
        let target = stable_node_id_for_view(first, &focus_key);
        app.windows.get_mut(&wid).unwrap().native_a11y_published = Some(published);
        app.on_native_accessibility_action(
            wid,
            ActionRequest {
                context: Some(aterm_a11y::model::ActionContext::new(
                    first.get(),
                    app.native_runtime.view_generation(first).unwrap(),
                )),
                action: Action::Focus,
                target_tree: TreeId::ROOT,
                target_node: target,
                data: None,
            },
        );
        assert_eq!(
            app.active_visible_leaf_plan(wid).unwrap().focused,
            first,
            "an action on a visible sibling focuses that exact pane"
        );
        assert_eq!(
            app.native_runtime
                .view_state(first)
                .unwrap()
                .common()
                .last_focus,
            Some(focus_key)
        );
    }
}

#[test]
fn native_native_composite_matches_presented_geometry_at_one_and_two_x() {
    for scale in [1.0, 2.0] {
        assert_native_sibling_geometry(scale, crate::tab_model::SplitAxis::Horizontal);
        assert_native_sibling_geometry(scale, crate::tab_model::SplitAxis::Vertical);
    }
}

#[test]
fn retained_pointer_hit_is_view_exact_and_rejects_staged_stale_or_torn_artifacts() {
    for scale in [1.0, 2.0] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.cols = 140;
            window.rows = 48;
            window.scale = scale;
        }
        assert!(app.open_settings_tab(SettingsRoute::Home));
        let (instance, first) = app.active_native_view(wid).unwrap();
        let second = app
            .split_active_with_native(
                wid,
                crate::tab_model::SplitAxis::Horizontal,
                instance,
                AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
                    &app.config,
                ))),
            )
            .unwrap();
        assert_eq!(app.active_visible_leaf_plan(wid).unwrap().focused, second);
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        assert!(
            app.retained_native_leaf_artifact(wid, first, true)
                .is_none(),
            "a merely staged raster must not accept pointer input"
        );
        for cache in app
            .windows
            .get_mut(&wid)
            .unwrap()
            .leaf_render_cache
            .values_mut()
        {
            if let Some(raster) = cache.native.as_mut() {
                raster.presented = true;
            }
        }

        let (raw_x, raw_y, expected_key) = {
            let artifact = app
                .retained_native_leaf_artifact(wid, first, true)
                .expect("exact first sibling artifact");
            let key = artifact
                .compiled
                .focus_order
                .first()
                .cloned()
                .expect("focusable control");
            let rect = artifact.compiled.semantic(&key).unwrap().rect;
            (
                artifact.device_x as f64 + f64::from(rect.x + rect.width * 0.5) * scale,
                artifact.device_y as f64 + f64::from(rect.y + rect.height * 0.5) * scale,
                key,
            )
        };
        let (artifact, local_x, local_y) = app
            .retained_native_leaf_at_pointer(wid, raw_x, raw_y)
            .expect("presented first sibling hit");
        assert_eq!(
            artifact.view, first,
            "pointer ownership comes from the retained destination, not focused sibling"
        );
        assert_eq!(
            artifact.compiled.hit_test(local_x, local_y).unwrap().key,
            expected_key
        );
        assert_eq!(app.active_visible_leaf_plan(wid).unwrap().focused, second);

        let original_stamp = app.windows[&wid].leaf_render_cache[&first]
            .native
            .as_ref()
            .unwrap()
            .stamp;
        app.windows
            .get_mut(&wid)
            .unwrap()
            .leaf_render_cache
            .get_mut(&first)
            .unwrap()
            .native
            .as_mut()
            .unwrap()
            .stamp
            .generation = original_stamp.generation.saturating_add(1);
        assert!(
            app.retained_native_leaf_at_pointer(wid, raw_x, raw_y)
                .is_none(),
            "a stale lifecycle generation fails closed"
        );
        app.windows
            .get_mut(&wid)
            .unwrap()
            .leaf_render_cache
            .get_mut(&first)
            .unwrap()
            .native
            .as_mut()
            .unwrap()
            .stamp = original_stamp;
        app.windows
            .get_mut(&wid)
            .unwrap()
            .leaf_render_cache
            .get_mut(&first)
            .unwrap()
            .native
            .as_mut()
            .unwrap()
            .stamp
            .geometry ^= 1;
        assert!(
            app.retained_native_leaf_at_pointer(wid, raw_x, raw_y)
                .is_none(),
            "a stale geometry stamp fails closed"
        );
        let card_width = app.windows[&wid].settings_card.as_ref().unwrap().pw;
        {
            let raster = app
                .windows
                .get_mut(&wid)
                .unwrap()
                .leaf_render_cache
                .get_mut(&first)
                .unwrap()
                .native
                .as_mut()
                .unwrap();
            raster.stamp = original_stamp;
            raster.presented_x = card_width;
        }
        assert!(
            app.retained_native_leaf_at_pointer(wid, raw_x, raw_y)
                .is_none(),
            "a torn card/leaf placement fails closed"
        );
    }
}

#[test]
fn retained_native_pointer_and_accessibility_survive_a_signed_surface_crop() {
    use aterm_window::dpi::PhysicalSize;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    assert!(app.prepare_native_input_scratch(wid));
    app.windows
        .get_mut(&wid)
        .and_then(|window| window.leaf_render_cache.get_mut(&view))
        .and_then(|cache| cache.native.as_mut())
        .expect("retained native raster")
        .presented = true;

    let (rows, cols) = {
        let window = &app.windows[&wid];
        (
            u16::try_from(window.input_scratch.cells.len()).unwrap(),
            window.cols,
        )
    };
    let full = app.frame_px(rows, cols);
    app.windows.get_mut(&wid).unwrap().win_px = Some(PhysicalSize::new(
        full.width.saturating_sub(8).max(1),
        full.height.saturating_sub(8).max(1),
    ));
    let origin = app.frame_origin(wid);
    assert!(origin.0 < 0, "fixture crops horizontally (centred)");
    if cfg!(target_os = "linux") {
        assert_eq!(origin.1, 0, "Linux pins the frame top");
    } else {
        assert!(origin.1 < 0, "fixture is a centred crop");
    }

    let artifact = app
        .retained_native_leaf_artifact(wid, view, true)
        .expect("a partially visible retained leaf remains authoritative");
    assert!(
        artifact.device_x < 0 || artifact.device_y < 0,
        "retained destination preserves signed window coordinates"
    );
    let (hit, local_x, local_y) = app
        .retained_native_leaf_at_pointer(wid, 0.5, 0.5)
        .expect("visible intersection routes pointer input");
    assert_eq!(hit.view, view);
    assert!(local_x >= 0.0 && local_y >= 0.0);

    let transform = app
        .native_accessibility_transform(wid)
        .expect("partially visible retained scene remains publishable");
    let expected = aterm_a11y::model::Affine::translate(aterm_a11y::model::Vec2::new(
        artifact.device_x as f64,
        artifact.device_y as f64,
    )) * aterm_a11y::model::Affine::scale(artifact.scale);
    assert_eq!(transform, expected);
}

#[test]
fn terminal_focus_keeps_visible_native_sibling_in_the_composite_tree() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = 120;
        window.rows = 42;
    }
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (_, native_view) = app.active_native_view(wid).expect("Settings view");
    let (session, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    crate::term_lock(&app.pool.get(session).expect("stub session").term)
        .process(b"mixed pane text");
    assert_eq!(app.active_native_view(wid), None);
    assert_eq!(
        app.active_visible_leaf_plan(wid).unwrap().focused,
        terminal_view
    );
    set_a11y_active(&mut app, wid, true);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let root_key = app.windows[&wid].leaf_render_cache[&native_view]
        .native
        .as_ref()
        .expect("retained native sibling")
        .compiled
        .semantics
        .iter()
        .find(|semantic| semantic.parent.is_none())
        .unwrap()
        .key
        .clone();
    let (update, published) = app
        .take_native_accessibility_update(wid)
        .expect("visible native sibling despite terminal focus")
        .expect("valid mixed tree");
    let node = |id| &update.nodes.iter().find(|(node, _)| *node == id).unwrap().1;
    let window_root = update.tree.as_ref().unwrap().root;
    let native_root = stable_node_id_for_view(native_view, &root_key);
    let children = node(window_root).children();
    assert_eq!(
        children.len(),
        2,
        "one child per visible leaf: {children:?}"
    );
    assert_eq!(children[0], native_root);
    let pane = children[1];
    assert_eq!(node(pane).role(), aterm_a11y::model::Role::Terminal);
    assert_eq!(
        node(pane).label(),
        Some(format!("{} — pane 2 of 2", crate::accessibility::LABEL).as_str())
    );
    assert!(
        node(pane)
            .value()
            .is_some_and(|text| text.contains("mixed pane text")),
        "the terminal leaf publishes its own text: {:?}",
        node(pane).value()
    );
    assert_eq!(
        update.focus, pane,
        "terminal focus is the terminal's own document, never a hidden native control"
    );
    assert!(published.route_owner(pane).is_none());
    assert_eq!(
        published.route_owner(native_root).unwrap().view,
        native_view
    );
}

#[test]
fn a_mixed_tab_s_native_focus_keeps_its_terminal_sibling_readable() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = 120;
        window.rows = 42;
    }
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (_, native_view) = app.active_native_view(wid).expect("Settings view");
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    crate::term_lock(&app.pool.get(session).expect("stub session").term).process(b"sibling text");
    assert!(
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.tab_set.active_mut())
            .is_some_and(|tab| tab.set_focus(native_view)),
        "the native leaf is live"
    );
    app.sync_window(wid);
    assert_eq!(
        app.active_native_view(wid).map(|(_, view)| view),
        Some(native_view)
    );
    set_a11y_active(&mut app, wid, true);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let (update, published) = app
        .take_native_accessibility_update(wid)
        .expect("visible native leaf")
        .expect("valid mixed tree");
    let node = |id| &update.nodes.iter().find(|(node, _)| *node == id).unwrap().1;
    let window_root = update.tree.as_ref().unwrap().root;
    let children = node(window_root).children();
    assert_eq!(
        children.len(),
        2,
        "one child per visible leaf: {children:?}"
    );
    let pane = children[1];
    assert!(
        node(pane)
            .value()
            .is_some_and(|text| text.contains("sibling text"))
    );
    assert!(
        published
            .route_owner(update.focus)
            .is_some_and(|owner| owner.view == native_view),
        "native focus stays on the native control"
    );
    assert!(
        node(pane).text_selection().is_none(),
        "no caret in an unfocused pane"
    );
}

#[test]
fn a_mixed_tab_s_terminal_text_moves_with_its_native_sibling_in_a_centred_frame() {
    use aterm_window::dpi::PhysicalSize;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = 120;
        window.rows = 42;
    }
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    crate::term_lock(&app.pool.get(session).expect("stub session").term).process(b"placed");
    set_a11y_active(&mut app, wid, true);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let full = {
        let window = &app.windows[&wid];
        app.frame_px(
            u16::try_from(window.input_scratch.cells.len()).unwrap(),
            window.cols,
        )
    };
    let placed = |app: &mut App, extra: u32| {
        app.windows.get_mut(&wid).unwrap().win_px = Some(PhysicalSize::new(
            full.width.saturating_add(extra),
            full.height.saturating_add(extra),
        ));
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let (update, _) = app
            .take_native_accessibility_update(wid)
            .expect("visible native leaf")
            .expect("valid mixed tree");
        let node = |id| &update.nodes.iter().find(|(node, _)| *node == id).unwrap().1;
        let children = node(update.tree.as_ref().unwrap().root).children();
        assert_eq!(
            children.len(),
            2,
            "one child per visible leaf: {children:?}"
        );
        let native = node(children[0])
            .transform()
            .expect("a native leaf root is placed")
            .as_coeffs();
        let pane = node(children[1])
            .bounds()
            .expect("a terminal pane has bounds");
        ((native[4], native[5]), (pane.x0, pane.y0))
    };
    let (native_snug, pane_snug) = placed(&mut app, 0);
    let (native_wide, pane_wide) = placed(&mut app, 16);
    let moved = |wide: (f64, f64), snug: (f64, f64)| (wide.0 - snug.0, wide.1 - snug.1);
    let native_moved = moved(native_wide, native_snug);
    let centred_y = if cfg!(target_os = "linux") { 0.0 } else { 8.0 };
    assert_eq!(
        native_moved,
        (8.0, centred_y),
        "PRECONDITION: a larger surface centres the frame and its native leaf"
    );
    assert_eq!(
        moved(pane_wide, pane_snug),
        native_moved,
        "the terminal leaf's text moves with its native sibling"
    );
    assert!(
        pane_snug.0 > native_snug.0,
        "the terminal leaf sits right of its native sibling"
    );
}

#[test]
fn composite_route_conforms_and_rejects_wrong_owner_and_stale_generation() {
    let model = composite_accessibility_route_model();
    let initial_projection = RouteProjection::default();
    let initial = project_route(&model, initial_projection);
    assert_eq!(initial, model.init_state());

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (instance, first) = app.active_native_view(wid).unwrap();
    let second = app
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            instance,
            AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
                &app.config,
            ))),
        )
        .unwrap();
    set_a11y_active(&mut app, wid, true);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let first_key = app.windows[&wid].leaf_render_cache[&first]
        .native
        .as_ref()
        .unwrap()
        .compiled
        .focus_order
        .first()
        .cloned()
        .expect("first view focus route");
    let first_node = stable_node_id_for_view(first, &first_key);
    let (_, published) = app.take_native_accessibility_update(wid).unwrap().unwrap();
    let first_generation = app.native_runtime.view_generation(first).unwrap();
    let second_generation = app.native_runtime.view_generation(second).unwrap();
    assert_eq!(
        published.route_owner(first_node),
        Some(AccessibilityOwner {
            view: first,
            generation: first_generation,
            review_revision: 0,
        })
    );
    let mut published_projection = initial_projection;
    published_projection.published_one_generation = 1;
    published_projection.published_two_generation = 1;
    let published_state = project_route(&model, published_projection);
    assert_route_transition(&model, &initial, &published_state, "Publish");

    let mut requested_projection = published_projection;
    requested_projection.pending = 1;
    requested_projection.target_owner = 1;
    requested_projection.target_generation = 1;
    let requested_state = project_route(&model, requested_projection);
    assert_route_transition(&model, &published_state, &requested_state, "RequestOne");

    let mut wrong_owner = published.clone();
    assert!(wrong_owner.retag_route_for_test(
        first_node,
        AccessibilityOwner {
            view: second,
            generation: second_generation,
            review_revision: 0,
        },
    ));
    app.native_runtime
        .view_state_mut(first)
        .unwrap()
        .common_mut()
        .last_focus = None;
    app.native_runtime
        .view_state_mut(second)
        .unwrap()
        .common_mut()
        .last_focus = None;
    app.windows.get_mut(&wid).unwrap().native_a11y_published = Some(wrong_owner);
    app.on_native_accessibility_action(
        wid,
        ActionRequest {
            context: Some(aterm_a11y::model::ActionContext::new(
                first.get(),
                first_generation,
            )),
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: first_node,
            data: None,
        },
    );
    assert_eq!(app.active_visible_leaf_plan(wid).unwrap().focused, second);
    assert!(
        app.native_runtime
            .view_state(first)
            .unwrap()
            .common()
            .last_focus
            .is_none()
    );
    assert!(
        app.native_runtime
            .view_state(second)
            .unwrap()
            .common()
            .last_focus
            .is_none()
    );
    let mut cross_view_mutant = requested_projection;
    cross_view_mutant.pending = 0;
    cross_view_mutant.dispatched_owner = 2;
    cross_view_mutant.dispatched_generation = 1;
    cross_view_mutant.cross_dispatch = 1;
    let cross_view_mutant = project_route(&model, cross_view_mutant);
    assert_eq!(admits(&model, &requested_state, &cross_view_mutant), None);
    assert!(!model.check_invariant("NoCrossViewDispatch", &cross_view_mutant));

    app.windows.get_mut(&wid).unwrap().native_a11y_published = Some(published);
    app.on_native_accessibility_action(
        wid,
        ActionRequest {
            context: Some(aterm_a11y::model::ActionContext::new(
                first.get(),
                first_generation,
            )),
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: first_node,
            data: None,
        },
    );
    assert_eq!(app.active_visible_leaf_plan(wid).unwrap().focused, first);
    assert_eq!(
        app.native_runtime
            .view_state(first)
            .unwrap()
            .common()
            .last_focus,
        Some(first_key)
    );
    assert!(
        app.native_runtime
            .view_state(second)
            .unwrap()
            .common()
            .last_focus
            .is_none()
    );
    let mut routed_projection = requested_projection;
    routed_projection.pending = 0;
    routed_projection.focus_owner = 1;
    routed_projection.dispatched_owner = 1;
    routed_projection.dispatched_generation = 1;
    let routed_state = project_route(&model, routed_projection);
    assert_route_transition(&model, &requested_state, &routed_state, "Route");

    let mut stale_app = App::headless_for_test();
    assert!(stale_app.open_settings_tab(SettingsRoute::Home));
    let (stale_instance, stale_first) = stale_app.active_native_view(wid).unwrap();
    let stale_second = stale_app
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            stale_instance,
            AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
                &stale_app.config,
            ))),
        )
        .unwrap();
    assert!(stale_app.prepare_heterogeneous_input_scratch(wid).is_some());
    let (live_owners, focused_native) = stale_app.visible_native_accessibility_owners(wid).unwrap();
    let mut stale_owners = live_owners.clone();
    let stale_second_owner = stale_owners
        .iter_mut()
        .find(|owner| owner.view == stale_second)
        .unwrap();
    assert!(stale_second_owner.generation > 0);
    stale_second_owner.generation -= 1;
    let stale_second_owner = *stale_second_owner;
    let mut projections = Vec::new();
    for owner in &stale_owners {
        let compiled = stale_app.windows[&wid].leaf_render_cache[&owner.view]
            .native
            .as_ref()
            .unwrap()
            .compiled
            .clone();
        let focus = stale_app
            .native_runtime
            .view_state(owner.view)
            .and_then(|state| state.common().last_focus.as_ref());
        let transform = stale_app
            .native_accessibility_transform_for_view(wid, owner.view)
            .unwrap();
        projections.push((
            *owner,
            project_native_accessibility_for_view_in_container(&compiled, focus, transform, *owner)
                .unwrap(),
        ));
    }
    let projection = compose_native_accessibility(
        projections,
        focused_native,
        stale_app.native_accessibility_window_bounds(wid).unwrap(),
        stale_app
            .windows
            .get(&wid)
            .map(|ws| ws.current_title.as_str())
            .unwrap_or_default(),
    )
    .unwrap();
    let (_, routes, virtual_text) = projection.into_update_routes_and_virtual_text();
    let stale_published = PublishedNativeAccessibility::composite(
        stale_second_owner,
        stale_owners,
        routes,
        virtual_text,
    );
    let stale_key = stale_app.windows[&wid].leaf_render_cache[&stale_second]
        .native
        .as_ref()
        .unwrap()
        .compiled
        .focus_order
        .first()
        .cloned()
        .unwrap();
    let stale_node = stable_node_id_for_view(stale_second, &stale_key);
    stale_app
        .native_runtime
        .view_state_mut(stale_second)
        .unwrap()
        .common_mut()
        .last_focus = None;
    stale_app
        .windows
        .get_mut(&wid)
        .unwrap()
        .native_a11y_published = Some(stale_published);
    stale_app.on_native_accessibility_action(
        wid,
        ActionRequest {
            context: Some(aterm_a11y::model::ActionContext::new(
                stale_second.get(),
                stale_second_owner.generation,
            )),
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: stale_node,
            data: None,
        },
    );
    assert!(
        stale_app
            .native_runtime
            .view_state(stale_second)
            .unwrap()
            .common()
            .last_focus
            .is_none(),
        "a delayed lifecycle route must never dispatch"
    );
    assert!(
        stale_app.windows[&wid].native_a11y_published.is_none(),
        "the stale composite is discarded fail-closed"
    );

    let mut advanced_projection = published_projection;
    advanced_projection.owner_two_generation = 2;
    let advanced_state = project_route(&model, advanced_projection);
    assert_route_transition(&model, &published_state, &advanced_state, "AdvanceTwo");
    let mut stale_request_projection = advanced_projection;
    stale_request_projection.pending = 1;
    stale_request_projection.target_owner = 2;
    stale_request_projection.target_generation = 1;
    let stale_request_state = project_route(&model, stale_request_projection);
    assert_route_transition(&model, &advanced_state, &stale_request_state, "RequestTwo");
    let mut rejected_projection = stale_request_projection;
    rejected_projection.pending = 0;
    let rejected_state = project_route(&model, rejected_projection);
    assert_route_transition(&model, &stale_request_state, &rejected_state, "RejectStale");
    let mut stale_mutant = stale_request_projection;
    stale_mutant.pending = 0;
    stale_mutant.dispatched_owner = 2;
    stale_mutant.dispatched_generation = 1;
    stale_mutant.stale_dispatch = 1;
    let stale_mutant = project_route(&model, stale_mutant);
    assert_eq!(admits(&model, &stale_request_state, &stale_mutant), None);
    assert!(!model.check_invariant("NoStaleGenerationDispatch", &stale_mutant));
    assert!(live_owners.iter().any(|owner| owner.view == stale_first));
}
