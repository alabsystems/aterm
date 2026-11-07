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

/// macOS's dialog answers a person: their press of the Settings row asks once, and nothing that
/// merely writes or reads the key does. `take_asks` counts on this thread, where the inert center
/// lives.
#[cfg(target_os = "macos")]
mod macos_notice_ask {
    use super::*;
    use crate::notice_center::take_asks;

    const ROW: &str = "settings/set/allow_notifications";
    const ROW_KEY: &str = "settings/control/allow_notifications";
    const OTHER_ROW: &str = "settings/set/allow_osc52_query";
    const UNDO: &str = "settings/undo";
    const OFF: &str = "";
    const ON: &str = "allow_notifications = true\n";

    type By = fn(&mut App, &str, &str);

    /// Every way a press of `action` on `key` comes with no person known to stand behind it.
    const UNVOUCHED: [(&str, By); 4] = [
        ("`aterm ctl act`", |app, key, action| {
            let (_, view) = app.active_native_view(WindowId(0)).unwrap();
            app.act_app(crate::app_control::ActRequest {
                view,
                ui_key: key.to_string(),
                action: action.to_string(),
                value: None,
            })
            .unwrap();
        }),
        ("a control client's input", |app, _, action| {
            app.with_controller_mark(true, |app| press(app, action, None));
        }),
        ("the accessibility tree", |app, _, action| {
            app.with_assistive_source(|app| press(app, action, None));
        }),
        ("another process's input", |app, _, action| {
            app.with_origin_mark(aterm_window::event::InputOrigin::Relayed, |app| {
                press(app, action, None);
            });
        }),
    ];

    fn pressed(text: &str, action: &str) -> App {
        let mut app = settings_app(text, SettingsRoute::Security);
        press(&mut app, action, None);
        app.native_config_inflight = false;
        app
    }

    fn saved_on(app: &App) -> bool {
        let snapshot = app.native_config_service.snapshot();
        snapshot.config.allow_notifications == Some(true)
    }

    /// `press`, with the lane held for it and let go after, so the write lands next.
    fn land_press(app: &mut App, press: impl FnOnce(&mut App)) {
        app.native_config_inflight = true;
        press(app);
        app.native_config_inflight = false;
        let landed = land_next(app, None);
        assert!(applied(&landed), "{landed:?}");
    }

    #[test]
    fn a_person_turning_program_notices_on_asks_macos_and_nothing_else_does() {
        let _ = take_asks();

        let mut app = pressed(OFF, ROW);
        assert_eq!(
            take_asks(),
            0,
            "a press asks only once its write has landed"
        );
        let landed = land_next(&mut app, None);
        assert!(applied(&landed), "{landed:?}");
        assert!(saved_on(&app));
        assert_eq!(take_asks(), 1, "a press off to on asks once");

        for (text, action, on, case) in [
            (ON, ROW, false, "a press on to off"),
            (ON, OTHER_ROW, true, "another row's press with the key on"),
            (
                OFF,
                OTHER_ROW,
                false,
                "another row's press with the key off",
            ),
        ] {
            let mut app = pressed(text, action);
            let landed = land_next(&mut app, None);
            assert!(applied(&landed), "{case}: {landed:?}");
            assert_eq!(saved_on(&app), on, "{case}");
            assert_eq!(take_asks(), 0, "{case} asks nothing");
        }

        for refused in [
            ConfigPatchOutcome::Rejected {
                message: "disk full".to_string(),
            },
            ConfigPatchOutcome::Indeterminate {
                message: "the disk went away".to_string(),
            },
            ConfigPatchOutcome::Conflict { revision: 7 },
        ] {
            let mut app = pressed(OFF, ROW);
            let request = app.native_config_pending.pop_front().unwrap();
            let (origin, result) = app.reduce_native_config_request(request);
            let Ok(ConfigPatchResult::Applied { snapshot, .. }) = result else {
                panic!("the press would have applied");
            };
            assert_eq!(snapshot.config.allow_notifications, Some(true));
            app.publish_native_config_origin(origin, refused.clone(), Some(snapshot), false, None);
            assert_eq!(take_asks(), 0, "{refused:?} is not a saved press");
        }
        let mut app = pressed(OFF, ROW);
        land_next(&mut app, Some("disk full"));
        assert_eq!(take_asks(), 0, "a refused write has nothing to read");

        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::new(OFF.to_string()).unwrap();
        assert_eq!(
            complete_control_field(
                &mut app,
                crate::prefs::EDIT_ALLOW_NOTIFICATIONS,
                Some("true")
            ),
            Ok("saved: allow_notifications = true".to_string())
        );
        assert!(saved_on(&app));
        assert_eq!(app.config.allow_notifications, None);
        assert_eq!(take_asks(), 0, "an agent's `settings set` asks nothing");

        let dir = aterm_tempfile::tempdir().expect("scratch config");
        let path = dir.path().join("aterm.toml");
        crate::test_scratch::write_config(&path, ON);
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        let config = crate::app_config::Config::parse(ON).unwrap();
        let assets = config.resolve_asset_catalog();
        let path_feed_fps = config.path_feed_fingerprints();
        let sparkle = config.prepare_sparkle_runtime();
        let mut app = App::headless_for_test();
        assert_eq!(app.config.allow_notifications, None);
        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                config,
                values: std::collections::BTreeMap::new(),
                disk_faults: std::sync::Arc::default(),
                assets,
                path_feed_fps,
                sparkle,
                fonts: None,
                warnings: Vec::new(),
                font_refusals: Vec::new(),
                reason: crate::app_colorscheme::ThemeChange::ConfigReload,
            },
        );
        assert_eq!(app.config.allow_notifications, Some(true));
        assert_eq!(take_asks(), 0, "a watcher generation asks nothing");
    }

    /// A PRESS NO PERSON STANDS BEHIND SAVES AND ASKS NOTHING: each way a press comes unvouched
    /// writes the row, and its Undo writes it back, with macOS never asked, so a program driving a
    /// Settings tab raises no dialog. What it leaves is the held state a line in the file leaves,
    /// which a person's off and on still lifts. NEGATIVE CONTROL: a person's own Undo asks, and
    /// `a_person_turning_program_notices_on_asks_macos_and_nothing_else_does` their own press.
    #[test]
    fn a_press_no_person_stands_behind_saves_the_row_and_asks_macos_nothing() {
        let _ = take_asks();
        for (who, by) in UNVOUCHED {
            let mut app = settings_app(OFF, SettingsRoute::Security);
            land_press(&mut app, |app| by(app, ROW_KEY, ROW));
            assert!(saved_on(&app), "{who}: the write lands");
            assert_eq!(take_asks(), 0, "{who} asks nothing");

            land_press(&mut app, |app| press(app, ROW, None));
            assert!(!saved_on(&app));
            land_press(&mut app, |app| press(app, ROW, None));
            assert!(saved_on(&app));
            assert_eq!(take_asks(), 1, "after {who}, a person's off and on asks");

            let mut app = settings_app(ON, SettingsRoute::Security);
            land_press(&mut app, |app| press(app, ROW, None));
            land_press(&mut app, |app| by(app, UNDO, UNDO));
            assert!(saved_on(&app), "{who}: the Undo lands");
            assert_eq!(take_asks(), 0, "{who}'s Undo asks nothing");
        }

        let mut app = settings_app(ON, SettingsRoute::Security);
        land_press(&mut app, |app| press(app, ROW, None));
        land_press(&mut app, |app| press(app, UNDO, None));
        assert!(saved_on(&app));
        assert_eq!(take_asks(), 1, "a person's Undo turns it on and asks");
    }

    /// THE LANE, NOT THE RUNNING CONFIG, SAYS WHAT A WRITE TURNED: `App::config` changes only when
    /// a write's generation is admitted, and these writes land before that. So a quick off and on
    /// from on, the remedy the log line names, finds the key off where the running config still
    /// reads on, and asks; and another row saved behind the press finds it on where the running
    /// config still reads off, and does not ask again. NEGATIVE CONTROL: the same off and on with
    /// a generation admitted between the presses asks once too.
    #[test]
    fn a_quick_off_and_on_asks_macos_once_and_a_row_saved_behind_the_ask_asks_nothing() {
        let _ = take_asks();

        let mut app = settings_app(ON, SettingsRoute::Security);
        press(&mut app, ROW, None);
        press(&mut app, ROW, None);
        assert_eq!(queued_follows(&app).len(), 2, "both presses are writes");
        app.native_config_inflight = false;
        let off = land_next(&mut app, None);
        assert!(applied(&off), "{off:?}");
        assert!(!saved_on(&app));
        assert_eq!(take_asks(), 0, "turning it off asks nothing");
        let on = land_next(&mut app, None);
        assert!(applied(&on), "{on:?}");
        assert!(saved_on(&app));
        assert_eq!(app.config.allow_notifications, Some(true), "still the old");
        assert_eq!(take_asks(), 1, "the quick off and on asks");

        let mut app = settings_app(ON, SettingsRoute::Security);
        land_press(&mut app, |app| press(app, ROW, None));
        app.config = (*app.native_config_service.snapshot().config).clone();
        assert_eq!(app.config.allow_notifications, None, "off is the key gone");
        land_press(&mut app, |app| press(app, ROW, None));
        assert_eq!(take_asks(), 1, "the slow off and on asks");

        let mut app = settings_app(OFF, SettingsRoute::Security);
        land_press(&mut app, |app| press(app, ROW, None));
        assert_eq!(take_asks(), 1, "the press asks");
        land_press(&mut app, |app| press(app, OTHER_ROW, None));
        assert!(saved_on(&app));
        assert_eq!(app.config.allow_notifications, None, "still the old");
        assert_eq!(take_asks(), 0, "another row's press does not ask again");
    }
}
