// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that Settings controls accept presses while an earlier change is still saving.

use super::*;
use crate::native_app::{ActionInvocation, SemanticInput};
use crate::native_settings::SettingsRoute;
use crate::native_ui::{ActionId, SemanticValue, UiKey};

fn settings_app(text: &str, route: SettingsRoute) -> App {
    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::new(text.to_string()).unwrap();
    app.config = (*app.native_config_service.snapshot().config).clone();
    assert!(app.open_settings_tab(route));
    app.native_config_inflight = true;
    app
}

fn press(app: &mut App, action: &str, value: Option<SemanticInput>) {
    app.dispatch_native_event(
        WindowId(0),
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(action),
            value,
        }),
    )
    .unwrap();
}

fn control(app: &App, key: &str) -> (SemanticValue, bool) {
    let compiled = app.compiled_native_ui(WindowId(0)).unwrap();
    let node = compiled
        .semantic(&UiKey::new(key))
        .unwrap_or_else(|| panic!("{key} is drawn"));
    let state = node.state.expect("a control has a state");
    assert!(
        !state.busy,
        "{key} never reads as busy while it takes presses"
    );
    (node.value.clone(), state.enabled)
}

fn status(app: &App) -> Option<String> {
    let (_, _, _, state) = app.native_settings_view_target().unwrap();
    state.feedback.clone()
}

fn queued_follows(app: &App) -> Vec<Vec<crate::native_app::OperationId>> {
    app.native_config_pending
        .iter()
        .map(|request| match &request.origin {
            NativeConfigOrigin::View { follows, .. } => follows.clone(),
            _ => panic!("a Settings write"),
        })
        .collect()
}

fn land_next(app: &mut App, refuse: Option<&str>) -> ConfigPatchOutcome {
    let request = app
        .native_config_pending
        .pop_front()
        .expect("a queued write");
    if let Some(message) = refuse {
        let outcome = ConfigPatchOutcome::Rejected {
            message: message.to_string(),
        };
        app.publish_native_config_origin(request.origin, outcome.clone(), None, false, None);
        return outcome;
    }
    let (origin, result) = app.reduce_native_config_request(request);
    let result = result.unwrap_or_else(|message| ConfigPatchResult::Rejected {
        snapshot: app.native_config_service.snapshot(),
        message,
    });
    let (outcome, snapshot) = match result {
        ConfigPatchResult::Applied { snapshot, undo } => (
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: Some(undo.get()),
            },
            snapshot,
        ),
        ConfigPatchResult::Unchanged { snapshot } => (
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: None,
            },
            snapshot,
        ),
        ConfigPatchResult::Conflict { snapshot, .. } => (
            ConfigPatchOutcome::Conflict {
                revision: snapshot.revision,
            },
            snapshot,
        ),
        ConfigPatchResult::Rejected { snapshot, message } => {
            (ConfigPatchOutcome::Rejected { message }, snapshot)
        }
    };
    app.publish_native_config_origin(origin, outcome.clone(), Some(snapshot), false, None);
    outcome
}

fn file(app: &App) -> String {
    app.native_config_service.snapshot().text.to_string()
}

fn applied(outcome: &ConfigPatchOutcome) -> bool {
    matches!(outcome, ConfigPatchOutcome::Applied { .. })
}

#[test]
fn a_second_click_on_a_saving_switch_is_a_second_toggle() {
    let switch = "settings/control/cursor_blink";
    let action = "settings/set/cursor_blink";
    for follow in [true, false] {
        let mut app = settings_app("", SettingsRoute::CursorMotion);
        app.dispatch_native_event(
            WindowId(0),
            AppEvent::FocusChanged(Some(UiKey::new(switch))),
        )
        .unwrap();
        let before = file(&app);
        assert_eq!(control(&app, switch), (SemanticValue::Bool(true), true));
        press(&mut app, action, None);
        assert_eq!(
            control(&app, switch),
            (SemanticValue::Bool(false), true),
            "the first click shows Off at once, and the switch takes the next"
        );
        press(&mut app, action, None);
        assert_eq!(
            control(&app, switch),
            (SemanticValue::Bool(true), true),
            "the second click is taken and shown"
        );
        let follows = queued_follows(&app);
        assert_eq!(follows.len(), 2, "both clicks are writes");
        assert!(follows[0].is_empty());
        assert_eq!(follows[1].len(), 1, "the second follows the first");
        if !follow {
            for request in &mut app.native_config_pending {
                if let NativeConfigOrigin::View { follows, .. } = &mut request.origin {
                    follows.clear();
                }
            }
        }
        app.native_config_inflight = false;
        let first = land_next(&mut app, None);
        assert!(applied(&first), "{first:?}");
        assert_eq!(control(&app, switch).0, SemanticValue::Bool(true));
        let second = land_next(&mut app, None);
        if !follow {
            assert!(
                matches!(second, ConfigPatchOutcome::Conflict { .. }),
                "unfollowed, the page's own first write makes the second conflict: {second:?}"
            );
            continue;
        }
        assert!(applied(&second), "{second:?}");
        assert_eq!(file(&app), before, "two toggles: the file ends as it began");
        assert_eq!(control(&app, switch), (SemanticValue::Bool(true), true));
        assert_eq!(status(&app).as_deref(), Some("Cursor blink turned on"));
        assert!(
            app.native_config_view_landings.is_empty(),
            "nothing left to follow"
        );
    }
}

#[test]
fn a_failed_save_goes_back_and_says_so() {
    let switch = "settings/control/cursor_blink";
    let action = "settings/set/cursor_blink";
    let mut app = settings_app("", SettingsRoute::CursorMotion);
    press(&mut app, action, None);
    assert_eq!(control(&app, switch).0, SemanticValue::Bool(false));
    app.native_config_inflight = false;
    land_next(&mut app, Some("disk full"));
    assert_eq!(
        control(&app, switch),
        (SemanticValue::Bool(true), true),
        "a refused write goes back to what the file holds"
    );
    assert_eq!(
        status(&app).as_deref(),
        Some("Cursor blink was not saved: disk full")
    );
    assert_eq!(file(&app), "");

    let mut app = settings_app("", SettingsRoute::CursorMotion);
    press(&mut app, action, None);
    press(&mut app, action, None);
    app.native_config_inflight = false;
    land_next(&mut app, Some("disk full"));
    assert_eq!(control(&app, switch).0, SemanticValue::Bool(true));
    let second = land_next(&mut app, None);
    assert!(!applied(&second), "{second:?}");
    assert_eq!(control(&app, switch), (SemanticValue::Bool(true), true));
    assert_eq!(
        status(&app).as_deref(),
        Some("Cursor blink was not saved: disk full"),
        "the first failure's words, not a conflict the page made itself"
    );
    assert_eq!(file(&app), "");
}

#[test]
fn every_control_kind_takes_a_press_while_it_saves() {
    type Step = (&'static str, Option<SemanticInput>);
    let arrow = |app: &mut App, right: bool| {
        app.dispatch_native_event(
            WindowId(0),
            AppEvent::TextInput(if right {
                crate::native_app::TextInputEvent::Right { extend: false }
            } else {
                crate::native_app::TextInputEvent::Left { extend: false }
            }),
        )
        .unwrap();
    };
    let cases: [(&str, SettingsRoute, &str, [Step; 2], &str); 3] = [
        (
            "a segment",
            SettingsRoute::CursorMotion,
            "",
            [
                ("settings/segment/cursor_style/1", None),
                ("settings/segment/cursor_style/0", None),
            ],
            "",
        ),
        (
            "a slider released twice",
            SettingsRoute::Home,
            "",
            [
                ("settings/set/font_px", Some(SemanticInput::Number(20.0))),
                ("settings/set/font_px", Some(SemanticInput::Number(24.0))),
            ],
            "font_px = 24\n",
        ),
        (
            "a Reset after a change",
            SettingsRoute::CursorMotion,
            "",
            [
                ("settings/set/cursor_blink", None),
                ("settings/reset/cursor_blink", None),
            ],
            "",
        ),
    ];
    for (name, route, text, steps, after) in cases {
        let mut app = settings_app(text, route);
        for (action, value) in steps {
            press(&mut app, action, value);
        }
        let follows = queued_follows(&app);
        assert_eq!(follows.len(), 2, "{name}: both presses are writes");
        assert_eq!(follows[1].len(), 1, "{name}: the second follows the first");
        app.native_config_inflight = false;
        for _ in 0..2 {
            let outcome = land_next(&mut app, None);
            assert!(applied(&outcome), "{name}: {outcome:?}");
        }
        assert_eq!(file(&app), after, "{name}");
    }

    let mut app = settings_app("font_px = 14\n", SettingsRoute::Home);
    app.dispatch_native_event(
        WindowId(0),
        AppEvent::FocusChanged(Some(UiKey::new("settings/control/font_px"))),
    )
    .unwrap();
    arrow(&mut app, true);
    arrow(&mut app, true);
    assert_eq!(queued_follows(&app).len(), 2, "both steps are writes");
    app.native_config_inflight = false;
    for _ in 0..2 {
        let outcome = land_next(&mut app, None);
        assert!(applied(&outcome), "{outcome:?}");
    }
    assert_eq!(file(&app), "font_px = 16\n", "two steps up from 14");

    let mut app = settings_app("theme = \"Dracula\"\n", SettingsRoute::Appearance);
    for theme in ["Nord", "Dracula"] {
        press(&mut app, "settings/set/theme", None);
        let compiled = app.compiled_native_ui(WindowId(0)).unwrap();
        let choice = compiled
            .semantics
            .iter()
            .find(|node| {
                node.key.as_str().starts_with("settings/choice/theme/")
                    && node.label.starts_with(theme)
            })
            .unwrap_or_else(|| panic!("the picker lists {theme}"))
            .key
            .as_str()
            .to_string();
        let (_, enabled) = control(&app, &choice);
        assert!(enabled, "{theme} can be chosen while the last choice saves");
        press(&mut app, &choice, None);
    }
    let follows = queued_follows(&app);
    assert_eq!(follows.len(), 2, "both choices are writes");
    assert_eq!(follows[1].len(), 1, "the second follows the first");
    app.native_config_inflight = false;
    for _ in 0..2 {
        let outcome = land_next(&mut app, None);
        assert!(applied(&outcome), "{outcome:?}");
    }
    assert_eq!(file(&app), "theme = \"Dracula\"\n");
}

fn undo_button(app: &App) -> Option<String> {
    let compiled = app.compiled_native_ui(WindowId(0)).unwrap();
    compiled
        .semantic(&UiKey::new("settings/undo"))
        .map(|node| node.label.clone())
}

#[test]
fn an_undo_pressed_while_its_change_saves_takes_it_back() {
    let switch = "settings/control/cursor_blink";
    let action = "settings/set/cursor_blink";
    for follow in [true, false] {
        let mut app = settings_app("", SettingsRoute::CursorMotion);
        app.dispatch_native_event(
            WindowId(0),
            AppEvent::FocusChanged(Some(UiKey::new(switch))),
        )
        .unwrap();
        let before = file(&app);
        press(&mut app, action, None);
        assert_eq!(control(&app, switch).0, SemanticValue::Bool(false));
        assert_eq!(
            undo_button(&app).as_deref(),
            Some("Undo"),
            "Undo is offered while the click saves"
        );
        press(&mut app, "settings/undo", None);
        assert_eq!(
            control(&app, switch).0,
            SemanticValue::Bool(true),
            "what the Undo puts back shows at once"
        );
        assert_eq!(status(&app).as_deref(), Some("Undoing…"));
        assert_eq!(queued_follows(&app).len(), 2, "the click and its Undo");
        if !follow {
            for request in &mut app.native_config_pending {
                if let NativeConfigOrigin::View { follows, .. } = &mut request.origin {
                    follows.clear();
                }
            }
        }
        app.native_config_inflight = false;
        let click = land_next(&mut app, None);
        assert!(applied(&click), "{click:?}");
        let undo = land_next(&mut app, None);
        if !follow {
            assert!(
                !applied(&undo),
                "unfollowed, the Undo never learns the click's token: {undo:?}"
            );
            assert_eq!(file(&app), "cursor_blink = false\n");
            continue;
        }
        assert!(applied(&undo), "{undo:?}");
        assert_eq!(file(&app), before, "the click was taken back");
        assert_eq!(control(&app, switch).0, SemanticValue::Bool(true));
        assert_eq!(status(&app).as_deref(), Some("Restored Cursor blink: On"));
        assert_eq!(undo_button(&app).as_deref(), Some("Redo"));

        app.native_config_inflight = true;
        press(&mut app, "settings/undo", None);
        assert_eq!(control(&app, switch).0, SemanticValue::Bool(false));
        assert_eq!(status(&app).as_deref(), Some("Redoing…"));
        assert_eq!(
            undo_button(&app).as_deref(),
            Some("Undo"),
            "the Redo on its way is offered for undoing"
        );
        press(&mut app, "settings/undo", None);
        assert_eq!(control(&app, switch).0, SemanticValue::Bool(true));
        assert_eq!(queued_follows(&app).len(), 2, "the Redo and its Undo");
        app.native_config_inflight = false;
        for _ in 0..2 {
            let outcome = land_next(&mut app, None);
            assert!(applied(&outcome), "{outcome:?}");
        }
        assert_eq!(file(&app), before, "Redo, then Undo of it");
        assert!(
            app.native_config_view_landings.is_empty(),
            "nothing left to follow"
        );
    }
}

#[test]
fn an_undo_of_a_refused_write_keeps_its_reason() {
    let switch = "settings/control/cursor_blink";
    let mut app = settings_app("", SettingsRoute::CursorMotion);
    press(&mut app, "settings/set/cursor_blink", None);
    press(&mut app, "settings/undo", None);
    app.native_config_inflight = false;
    land_next(&mut app, Some("disk full"));
    let undo = land_next(&mut app, None);
    assert!(!applied(&undo), "{undo:?}");
    assert_eq!(file(&app), "");
    assert_eq!(control(&app, switch), (SemanticValue::Bool(true), true));
    assert_eq!(
        status(&app).as_deref(),
        Some("Cursor blink was not saved: disk full")
    );
    assert_eq!(undo_button(&app), None, "nothing to undo");
}

#[test]
fn a_keep_and_its_revert_within_one_save_land_in_order() {
    let source = "theme = \"Dracula\"\n";
    let mut app = settings_app(source, SettingsRoute::Themes);
    let card = |app: &App, label: &str| {
        let compiled = app.compiled_native_ui(WindowId(0)).unwrap();
        compiled
            .semantics
            .iter()
            .find(|node| {
                node.key.as_str().starts_with("settings/showcase/card/")
                    && node.label.starts_with(label)
            })
            .unwrap_or_else(|| panic!("the chooser lists {label}"))
            .key
            .as_str()
            .to_string()
    };
    let nord = card(&app, "Nord");
    press(&mut app, &nord, None);
    press(&mut app, "settings/showcase/keep", None);
    assert!(
        control(&app, "settings/showcase/revert").1,
        "Revert takes a press while the Keep saves"
    );
    press(&mut app, "settings/showcase/revert", None);
    let follows = queued_follows(&app);
    assert_eq!(follows.len(), 2, "the Keep and its Revert are writes");
    assert!(follows[0].is_empty());
    assert_eq!(follows[1].len(), 1, "the Revert follows the Keep");
    app.native_config_inflight = false;
    for _ in 0..2 {
        let outcome = land_next(&mut app, None);
        assert!(applied(&outcome), "{outcome:?}");
    }
    assert_eq!(file(&app), source, "the file ends as it began");
    let (_, _, _, state) = app.native_settings_view_target().unwrap();
    assert_eq!(
        state.showcase.trial,
        crate::native_settings::showcase_page::ShowcaseTrial::Idle
    );
    assert_eq!(
        app.native_config_service.snapshot().config.theme.as_deref(),
        Some("Dracula")
    );
}

/// A SLIDER STEPS ONTO THE AUTOMATIC SIZE AND ON PAST IT: from one under the platform's automatic
/// font size (12 px on macOS, 15 on Linux), two Right presses while the first still saves write
/// the removal the automatic size is, then the size past it. Before, the removal in flight showed
/// the file's old size, so the second press stepped onto the automatic size again and wrote
/// nothing (Linux's 14 px plus two steps ended at 15). NEGATIVE CONTROL: from one over it, the
/// steps are two plain writes.
#[test]
fn a_slider_steps_onto_the_automatic_size_and_past_it() {
    let automatic = crate::FONT_PX;
    for (start, writes) in [
        (automatic - 1.0, [None, Some(automatic + 1.0)]),
        (
            automatic + 1.0,
            [Some(automatic + 2.0), Some(automatic + 3.0)],
        ),
    ] {
        let mut app = settings_app(&format!("font_px = {start}\n"), SettingsRoute::Home);
        app.dispatch_native_event(
            WindowId(0),
            AppEvent::FocusChanged(Some(UiKey::new("settings/control/font_px"))),
        )
        .unwrap();
        let mut shown = Vec::new();
        for _ in 0..2 {
            app.dispatch_native_event(
                WindowId(0),
                AppEvent::TextInput(crate::native_app::TextInputEvent::Right { extend: false }),
            )
            .unwrap();
            shown.push(control(&app, "settings/control/font_px").0);
        }
        let number = |px: f32| SemanticValue::Number {
            value: f64::from(px),
            minimum: 6.0,
            maximum: 200.0,
        };
        let steps = writes.map(|write| number(write.unwrap_or(automatic)));
        assert_eq!(
            shown, steps,
            "from {start}: each press shows the size it writes"
        );
        let edits: Vec<Option<String>> = app
            .native_config_pending
            .iter()
            .map(|request| match &request.work {
                NativeConfigWork::Patch(patch) => patch
                    .edits
                    .iter()
                    .find(|edit| edit.key == crate::prefs::EDIT_FONT_PX)
                    .and_then(|edit| edit.value.clone()),
                _ => panic!("from {start}: a font size write"),
            })
            .collect();
        assert_eq!(
            edits,
            writes.map(|write| write.map(|px| px.to_string())),
            "from {start}: both presses are writes"
        );
        app.native_config_inflight = false;
        for _ in 0..2 {
            let outcome = land_next(&mut app, None);
            assert!(applied(&outcome), "from {start}: {outcome:?}");
        }
        assert_eq!(
            file(&app),
            format!(
                "font_px = {}\n",
                writes[1].expect("the last step writes a size")
            ),
            "from {start}"
        );
    }
}
