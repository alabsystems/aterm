// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_app::{
    ActionInvocation, AppEffect, AppViewState, NativeApp, NativeRuntime, ViewId,
};
use crate::native_ui::{CompiledUi, LogicalRect};
use crate::registry_machines::Observation;

fn machine(key: &str, name: &str) -> Machine {
    Machine {
        key: key.into(),
        name: name.into(),
        status: "active".into(),
        assignments: Vec::new(),
        observations: Vec::new(),
    }
}

fn model() -> RegistryApp {
    RegistryApp {
        paths: None,
        wake: Arc::new(|| {}),
        pending: None,
        rows: None,
        revision: 0,
        endings: BTreeMap::new(),
        error: None,
    }
}

fn scene(app: &RegistryApp, view: &RegistryViewState, width: f32) -> CompiledUi {
    app.page(view, width, 760.0)
        .compile(LogicalRect::new(0.0, 0.0, width, 760.0))
        .unwrap()
}

fn label(ui: &CompiledUi, key: &str) -> String {
    ui.semantic(&UiKey::new(key)).unwrap().label.clone()
}

#[test]
fn selected_machine_follows_its_key_across_rename_and_reordered_reads() {
    let mut view = RegistryViewState::default();
    view.select(Some("east".into()));
    let mut app = model();
    let (tx, rx) = mpsc::channel();
    app.pending = Some(rx);
    let starting = scene(&app, &view, 360.0);
    assert!(label(&starting, "registry/status").contains("Reading"));
    assert!(starting.semantic(&UiKey::new(OPEN)).is_none());

    let mut east = machine("east", "Mill");
    east.assignments = vec![("account".into(), "Drafting".into())];
    east.observations = vec![
        Observation {
            writer: "writer-a".into(),
            at_ms: None,
            summary: "2 cores".into(),
        },
        Observation {
            writer: "writer-b".into(),
            at_ms: Some(1_700_000_000_000),
            summary: "4 cores".into(),
        },
    ];
    tx.send(Ok(vec![machine("west", "Mill"), east])).unwrap();
    assert!(app.poll());
    let narrow = scene(&app, &view, 360.0);
    assert_eq!(
        label(&narrow, "registry/detail/account/account"),
        "Drafting"
    );
    assert_eq!(label(&narrow, "registry/detail/observation/0"), "2 cores");
    assert_eq!(label(&narrow, "registry/detail/observation/1"), "4 cores");
    assert!(label(&narrow, "registry/detail/writer/0").contains("writer-a"));
    assert!(label(&narrow, "registry/detail/writer/0").contains("time unavailable"));
    assert!(
        label(&narrow, "registry/detail/writer/1")
            .contains(&crate::native_tables::stamp_words(1_700_000_000_000))
    );
    assert!(
        narrow
            .semantic(&UiKey::new("registry/machine/west"))
            .is_none()
    );
    assert_eq!(label(&narrow, MACHINES), "All machines");

    let (tx, rx) = mpsc::channel();
    app.pending = Some(rx);
    tx.send(Ok(vec![machine("east", "Router"), machine("west", "Mill")]))
        .unwrap();
    assert!(app.poll());
    let wide = scene(&app, &view, 960.0);
    assert_eq!(label(&wide, "registry/detail/name"), "Router");
    assert!(
        wide.semantic(&UiKey::new("registry/machine/east"))
            .unwrap()
            .state
            .is_some_and(|state| state.selected)
    );
    assert_eq!(label(&wide, OPEN), "Open in Tables");

    let (tx, rx) = mpsc::channel();
    app.pending = Some(rx);
    tx.send(Err(ReadError::Unavailable("Store unavailable".into())))
        .unwrap();
    assert!(app.poll());
    let failed = scene(&app, &view, 960.0);
    assert!(label(&failed, "registry/status").contains("Store unavailable"));
    assert_eq!(label(&failed, "registry/detail/name"), "Router");
}

#[test]
fn open_in_tables_requires_the_selected_machine_and_sends_its_key() {
    let mut app = model();
    app.rows = Some(vec![machine("east", "Mill")]);
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Registry(Box::new(app)))
        .unwrap();
    let mut state = RegistryViewState::default();
    state.select(Some("missing".into()));
    let view = ViewId::from_stored(711);
    runtime
        .attach_view(view, instance, AppViewState::Registry(Box::new(state)))
        .unwrap();
    let mut act = |id: &str| {
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(id),
                    value: None,
                }),
            )
            .unwrap()
    };
    assert_eq!(act(OPEN).result, EventResult::Bubble);
    assert_eq!(act("registry/machine/east").result, EventResult::Handled);
    assert!(act(OPEN).effects.iter().any(
        |effect| matches!(effect, AppEffect::OpenRegistryMachineInTables(key) if key == "east")
    ));
    assert_eq!(act(MACHINES).result, EventResult::Handled);
    assert_eq!(act(OPEN).result, EventResult::Bubble);
}

#[test]
fn repeated_names_keep_a_distinguishing_ending_visible_and_the_full_key_accessible() {
    let first = "mc-0011abcd";
    let second = "mc-0022abcd";
    let long = "Machine with a long display name in both buildings";
    let mut app = model();
    let mut rows = vec![
        machine(first, long),
        machine(second, long),
        machine("mc-0033abcd", "Lathe"),
    ];
    for _ in 0..2 {
        let (tx, rx) = mpsc::channel();
        app.pending = Some(rx);
        tx.send(Ok(rows.clone())).unwrap();
        assert!(app.poll());
        let scene = scene(&app, &RegistryViewState::default(), 320.0);
        for (key, suffix) in [(first, "1abcd"), (second, "2abcd")] {
            let row = scene
                .semantic(&UiKey::new(format!("{MACHINE}{key}")))
                .unwrap();
            assert_eq!(row.label, format!("{long} · {suffix}"));
            assert_eq!(
                row.description.as_deref(),
                Some(format!("Machine key {key}").as_str())
            );
            let label = app.row_label(app.machine(key).unwrap(), 120.0);
            assert!(label.visual_label.unwrap().ends_with(suffix));
        }
        assert_eq!(label(&scene, "registry/machine/mc-0033abcd"), "Lathe");
        rows.reverse();
    }
}

#[test]
fn short_narrow_detail_keeps_its_actions_and_returns_focus_to_the_machine() {
    let mut app = model();
    let mut rows: Vec<_> = (0..8)
        .map(|i| machine(&format!("key-{i}"), &format!("Router {i}")))
        .collect();
    rows[7].assignments = (0..14)
        .map(|i| (format!("account-{i}"), format!("Assigned account {i}")))
        .collect();
    app.rows = Some(rows);
    let mut state = RegistryViewState::default();
    state.select(Some("key-7".into()));
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Registry(Box::new(app)))
        .unwrap();
    let view = ViewId::from_stored(712);
    runtime
        .attach_view(view, instance, AppViewState::Registry(Box::new(state)))
        .unwrap();
    let bounds = LogicalRect::new(0.0, 0.0, 320.0, 125.0);
    let draw = |runtime: &NativeRuntime| {
        let Some(NativeApp::Registry(app)) = runtime.app(instance) else {
            unreachable!()
        };
        let Some(AppViewState::Registry(state)) = runtime.view_state(view) else {
            unreachable!()
        };
        app.page(state, bounds.width, bounds.height)
            .compile(bounds)
            .unwrap()
    };
    let button_rect = |ui: &CompiledUi, key: &str| {
        let key = UiKey::new(key);
        assert!(ui.focus_order.contains(&key));
        let hit = ui.hits.iter().find(|hit| hit.key == key).unwrap();
        assert!(hit.rect.height >= button_height());
        hit.rect
    };
    let first = draw(&runtime);
    let open = button_rect(&first, OPEN);
    let back = button_rect(&first, MACHINES);
    runtime
        .dispatch(instance, view, AppEvent::StepFocus(1))
        .unwrap();
    let Some(AppViewState::Registry(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert!(state.detail_scroll.get() > 0.0);
    let scrolled = draw(&runtime);
    assert_eq!(button_rect(&scrolled, OPEN), open);
    assert_eq!(button_rect(&scrolled, MACHINES), back);
    let last = "registry/machine/key-7";
    let activate = |key| {
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(key),
            value: None,
        })
    };
    runtime
        .dispatch(instance, view, activate(MACHINES))
        .unwrap();
    let list = draw(&runtime);
    button_rect(&list, last);
    button_rect(&list, PREVIOUS);
    assert_eq!(
        runtime
            .view_state(view)
            .unwrap()
            .common()
            .last_focus
            .as_ref()
            .map(UiKey::as_str),
        Some(last)
    );
    runtime
        .dispatch(instance, view, AppEvent::StepFocus(-1))
        .unwrap();
    button_rect(&draw(&runtime), "registry/machine/key-6");
    runtime
        .dispatch(instance, view, activate("registry/machine/key-6"))
        .unwrap();
    let selected = draw(&runtime);
    button_rect(&selected, OPEN);
    assert_eq!(
        runtime
            .view_state(view)
            .unwrap()
            .common()
            .last_focus
            .as_ref()
            .map(UiKey::as_str),
        Some(OPEN)
    );
}

#[test]
fn find_from_narrow_detail_opens_a_distant_machine_by_name_or_key() {
    let mut app = model();
    let mut rows: Vec<_> = (0..300)
        .map(|i| machine(&format!("node/{i:03}"), &format!("Router {i}")))
        .collect();
    rows[297].name = "Field Mill".into();
    app.rows = Some(rows);
    let mut state = RegistryViewState::default();
    state.select(Some("node/004".into()));
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Registry(Box::new(app)))
        .unwrap();
    let view = ViewId::from_stored(713);
    runtime
        .attach_view(view, instance, AppViewState::Registry(Box::new(state)))
        .unwrap();
    let draw = |runtime: &NativeRuntime| {
        let Some(NativeApp::Registry(app)) = runtime.app(instance) else {
            unreachable!()
        };
        let Some(AppViewState::Registry(state)) = runtime.view_state(view) else {
            unreachable!()
        };
        let bounds = LogicalRect::new(0.0, 0.0, 320.0, 125.0);
        app.page(state, bounds.width, bounds.height)
            .compile(bounds)
            .unwrap()
    };
    let action = |key, value| {
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(key),
            value,
        })
    };
    assert_eq!(label(&draw(&runtime), "registry/detail/name"), "Router 4");
    runtime
        .dispatch(instance, view, action(SEARCH, None))
        .unwrap();
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::TextInput(TextInputEvent::Commit("FIELD\r\nMILL".into())),
        )
        .unwrap();
    let matches = draw(&runtime);
    let target = UiKey::new("registry/machine/node/297");
    assert!(matches.focus_order.contains(&UiKey::new(SEARCH)));
    assert!(matches.focus_order.contains(&UiKey::new(CLEAR)));
    assert!(matches.focus_order.contains(&target));
    assert!(
        matches
            .semantic(&UiKey::new("registry/machine/node/004"))
            .is_none()
    );
    assert!(matches.semantic(&UiKey::new(OPEN)).is_none());
    let Some(AppViewState::Registry(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert_eq!(state.selected(), Some("node/004"));
    assert_eq!(
        RegistryApp::field_input(state, SEARCH).unwrap().value(),
        "FIELD MILL"
    );
    runtime
        .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Submit))
        .unwrap();
    assert_eq!(label(&draw(&runtime), "registry/detail/name"), "Field Mill");

    runtime
        .dispatch(instance, view, action(SEARCH, None))
        .unwrap();
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::TextInput(TextInputEvent::Commit("NODE/299".into())),
        )
        .unwrap();
    let by_key = draw(&runtime);
    assert_eq!(label(&by_key, "registry/machine/node/299"), "Router 299");
    assert!(by_key.semantic(&target).is_none());
    runtime
        .dispatch(
            instance,
            view,
            action(SEARCH, Some(SemanticInput::Text("absent".into()))),
        )
        .unwrap();
    assert_eq!(
        label(&draw(&runtime), "registry/list/empty"),
        "No matching machines."
    );
    runtime
        .dispatch(instance, view, action(CLEAR, None))
        .unwrap();
    let unfiltered = draw(&runtime);
    assert!(unfiltered.semantic(&UiKey::new(CLEAR)).is_none());
    assert!(unfiltered.focus_order.contains(&target));
}

#[test]
fn query_results_follow_refreshed_names_and_are_private_to_the_view() {
    let mut app = model();
    let mut search = RegistryViewState::default();
    search.select(Some("machine/b".into()));
    search.edit_search(TextInputEvent::Commit("café".into()));
    let initial = [machine("machine/a", "Café"), machine("machine/b", "Lathe")];
    let renamed = [machine("machine/b", "CAFÉ"), machine("machine/a", "Press")];
    for (inventory, matching, other) in [
        (initial, "machine/a", "Lathe"),
        (renamed, "machine/b", "CAFÉ"),
    ] {
        let (tx, rx) = mpsc::channel();
        app.pending = Some(rx);
        tx.send(Ok(inventory.into())).unwrap();
        assert!(app.poll());
        let filtered = scene(&app, &search, 960.0);
        let keys: Vec<_> = filtered
            .semantics
            .iter()
            .filter_map(|n| n.key.as_str().strip_prefix(MACHINE))
            .collect();
        assert_eq!(keys, [matching]);
        assert_eq!(label(&filtered, "registry/detail/name"), other);
        assert_eq!(search.selected(), Some("machine/b"));
        let unfiltered = scene(&app, &RegistryViewState::default(), 960.0);
        assert!(
            unfiltered
                .semantic(&UiKey::new("registry/machine/machine/a"))
                .is_some()
        );
        assert!(
            unfiltered
                .semantic(&UiKey::new("registry/machine/machine/b"))
                .is_some()
        );
    }
}
