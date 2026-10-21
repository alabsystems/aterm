// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! NO PRESS IS DROPPED: a Settings control takes a press while its last
//! change is still saving. These run the real Settings reducer, the host's
//! enqueue and the serialized config lane; only the worker's file write is
//! stood in for, by landing each queued request in order.

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
    // The worker owns a write: every press from here waits in the lane, as
    // a second click within one save does.
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

/// The control `key` as the page draws it now: its value, and whether it
/// takes a press.
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

/// The writes queued from the view, in order: what each follows.
fn queued_follows(app: &App) -> Vec<Vec<crate::native_app::OperationId>> {
    app.native_config_pending
        .iter()
        .map(|request| match &request.origin {
            NativeConfigOrigin::View { follows, .. } => follows.clone(),
            _ => panic!("a Settings write"),
        })
        .collect()
}

/// The lane head lands as the worker would land it: reduced at the lane
/// head (its following base included), then published with the snapshot it
/// produced. `refuse` stands in for a write the worker could not make.
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
    // A request the lane head refuses is answered as the lane answers it:
    // refused, with what the service holds.
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

/// TWO CLICKS ON A SWITCH WITHIN ONE SAVE ARE TWO TOGGLES. The first click
/// shows Off at once and the switch takes the second, which shows On and
/// follows the first; the lane lands both, each its own step, and the file
/// ends as it began. Negative control: the same second write checked at the
/// revision the page last read (what it carried before it followed the
/// first) conflicts with the page's own first write, so the follow is what
/// lands it; the guard this replaced drew the switch disabled after the
/// first click, so the second never reached the page at all.
#[test]
fn a_second_click_on_a_saving_switch_is_a_second_toggle() {
    let switch = "settings/control/cursor_blink";
    let action = "settings/set/cursor_blink";
    for follow in [true, false] {
        let mut app = settings_app("", SettingsRoute::CursorMotion);
        // The keyboard on the switch brings its row onto the screen.
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

/// A WRITE THAT FAILS SAYS SO AND THE CONTROL GOES BACK to what the file
/// holds, with the press composed over it: the second click, made on the
/// first's value, fails with it, and the status keeps the first one's
/// reason.
#[test]
fn a_failed_save_goes_back_and_says_so() {
    let switch = "settings/control/cursor_blink";
    let action = "settings/set/cursor_blink";
    // One click, refused.
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

    // Two clicks, the first refused: the second was composed over a value
    // the file never held, so it lands nothing, and the reason stays.
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

/// EVERY CONTROL KIND TAKES A PRESS WHILE ITS LAST ONE SAVES: a segment, a
/// slider's arrow key (a step), a slider's release (a value), a picker's
/// choice and a Reset. Each pair of presses lands as two writes in order,
/// the second following the first, and the file holds the second's value.
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

    // A slider's arrow keys: two steps within one save are two steps.
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

    // A picker: Color theme chosen twice, the second from the picker the
    // page draws after the first, before the first has saved.
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

/// The label of the status bar's Undo button as the page draws it now.
fn undo_button(app: &App) -> Option<String> {
    let compiled = app.compiled_native_ui(WindowId(0)).unwrap();
    compiled
        .semantic(&UiKey::new("settings/undo"))
        .map(|node| node.label.clone())
}

/// UNDO IS TAKEN WHILE THE CHANGE IT NAMES SAVES. A click on a switch, then
/// Undo before the click's write lands: the Undo follows the write in the
/// lane and takes back what it landed with, so the file ends as it began,
/// and the switch shows its old value from the press on. The Redo it then
/// offers and the Undo after it, pressed at once, the second while the
/// first saves, land as two steps.
/// Negative control: the same Undo queued without following the write
/// never learns what it landed with, and takes nothing back.
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

        // Redo, then Undo at once while the Redo saves (a double click on
        // the button): two steps, the second taking back the first.
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

/// AN UNDO OF A WRITE THAT FAILED TAKES NOTHING BACK AND SAYS NOTHING NEW:
/// the write's keys went back to what the file holds, and the status keeps
/// the write's own reason.
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

/// THEMES' KEEP AND ITS REVERT WITHIN ONE SAVE ARE TWO WRITES IN ORDER,
/// through the real reducer, the host's enqueue and the lane: the theme is
/// tried, Keep is pressed while the worker still owns a write, and Revert,
/// drawn enabled meanwhile, is taken and follows the Keep; the lane lands
/// both and aterm.toml ends as it began, the trial over and Dracula in use.
/// Before, Revert was drawn disabled while the Keep saved, so the press was
/// dropped and the kept theme stayed.
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
