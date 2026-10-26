// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for pane-focus arrow chord pass-through when no pane lies in that direction.

use super::{PhysicalReleaseTrace, take_physical_release_trace};
use crate::{App, PhysicalPressOwner, WindowId};
use aterm_types::keyboard::{Key as TKey, KeyEventType, Modifiers, NamedKey as TNamed};
use aterm_window::event::{ElementState, KeyEvent};
use aterm_window::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

/// One arrow, as the four tests below press it.
#[derive(Clone, Copy, Debug)]
struct Arrow {
    code: KeyCode,
    key: NamedKey,
    engine: TNamed,
    /// The legacy encoding of Alt + this arrow (xterm's modifier parameter 3).
    alt_bytes: &'static [u8],
}

const LEFT: Arrow = Arrow {
    code: KeyCode::ArrowLeft,
    key: NamedKey::ArrowLeft,
    engine: TNamed::ArrowLeft,
    alt_bytes: b"\x1b[1;3D",
};
const RIGHT: Arrow = Arrow {
    code: KeyCode::ArrowRight,
    key: NamedKey::ArrowRight,
    engine: TNamed::ArrowRight,
    alt_bytes: b"\x1b[1;3C",
};
const UP: Arrow = Arrow {
    code: KeyCode::ArrowUp,
    key: NamedKey::ArrowUp,
    engine: TNamed::ArrowUp,
    alt_bytes: b"\x1b[1;3A",
};
const DOWN: Arrow = Arrow {
    code: KeyCode::ArrowDown,
    key: NamedKey::ArrowDown,
    engine: TNamed::ArrowDown,
    alt_bytes: b"\x1b[1;3B",
};

fn event(arrow: Arrow, state: ElementState, repeat: bool) -> KeyEvent {
    KeyEvent::synthetic_for_test(
        PhysicalKey::Code(arrow.code),
        Key::Named(arrow.key),
        None,
        KeyLocation::Standard,
        state,
        repeat,
    )
}

fn app_with_alt_arrow_pane_focus() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    app.keybindings = crate::keybinding::Keybindings::resolved(Some(
        &[
            ("alt+left", "focus_pane_left"),
            ("alt+right", "focus_pane_right"),
            ("alt+up", "focus_pane_up"),
            ("alt+down", "focus_pane_down"),
        ]
        .into_iter()
        .map(|(chord, action)| (chord.to_string(), action.to_string()))
        .collect(),
    ));
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("window").mods = ModifiersState::ALT;
    (app, wid)
}

fn press(app: &mut App, wid: WindowId, arrow: Arrow) {
    app.on_key(wid, event(arrow, ElementState::Pressed, false));
}

fn release(app: &mut App, wid: WindowId, arrow: Arrow) {
    app.on_key(wid, event(arrow, ElementState::Released, false));
}

fn focus(app: &App, wid: WindowId) -> u64 {
    app.windows[&wid].layouts[0].focus()
}

fn owner(app: &App, arrow: Arrow) -> Option<PhysicalPressOwner> {
    app.physical_press_owners
        .get(&PhysicalKey::Code(arrow.code))
        .cloned()
}

fn forwarded(app: &App, arrow: Arrow) -> (u64, Vec<u8>) {
    let Some(PhysicalPressOwner::Forwarded {
        session,
        key,
        mods,
        base_layout,
        layout_text,
        ..
    }) = owner(app, arrow)
    else {
        panic!(
            "{:?} was not forwarded to the program: {:?}",
            arrow.key,
            owner(app, arrow)
        );
    };
    assert_eq!(key, TKey::Named(arrow.engine), "{:?}", arrow.key);
    assert_eq!(mods, Modifiers::ALT, "{:?}", arrow.key);
    let mode = app
        .pool
        .get(session)
        .expect("the forwarded session")
        .ctx
        .modes
        .keyboard_mode();
    let bytes = aterm_types::keyboard::encode_key_with_text(
        &key,
        mods,
        mode,
        KeyEventType::Press,
        base_layout,
        layout_text,
    );
    (session, bytes)
}

fn consumed(app: &App, wid: WindowId, arrow: Arrow) -> bool {
    app.windows[&wid]
        .consumed_press_keys
        .contains(&PhysicalKey::Code(arrow.code))
}

#[test]
fn alt_arrows_on_a_one_pane_tab_reach_the_program() {
    let (mut app, wid) = app_with_alt_arrow_pane_focus();
    let session = app.front_terminal(wid).expect("terminal").session;
    for arrow in [LEFT, RIGHT, UP, DOWN] {
        press(&mut app, wid, arrow);
        assert!(
            !consumed(&app, wid, arrow),
            "{:?} is not swallowed",
            arrow.key
        );
        assert_eq!(
            forwarded(&app, arrow),
            (session, arrow.alt_bytes.to_vec()),
            "{:?}",
            arrow.key
        );
        release(&mut app, wid, arrow);
    }
}

#[test]
fn alt_left_into_a_split_moves_focus_and_the_program_sees_nothing() {
    let (mut app, wid) = app_with_alt_arrow_pane_focus();
    let right_pane = app.split_active_stub_tab(wid);
    assert_eq!(focus(&app, wid), right_pane, "a split focuses its new pane");

    press(&mut app, wid, LEFT);
    assert_eq!(focus(&app, wid), 0, "Alt+← moved focus to the left pane");
    assert!(
        matches!(owner(&app, LEFT), Some(PhysicalPressOwner::Consumed { window }) if window == wid),
        "the press is the GUI's: {:?}",
        owner(&app, LEFT)
    );
    assert!(consumed(&app, wid, LEFT));
    release(&mut app, wid, LEFT);
    assert!(
        matches!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Consumed { .. })
        ),
        "its release is swallowed with it"
    );

    press(&mut app, wid, RIGHT);
    assert_eq!(focus(&app, wid), right_pane, "Alt+→ moves back");
    assert!(consumed(&app, wid, RIGHT));
    release(&mut app, wid, RIGHT);
}

#[test]
fn at_the_edge_of_a_split_or_zoomed_the_chord_passes_through() {
    let (mut app, wid) = app_with_alt_arrow_pane_focus();
    let right_pane = app.split_active_stub_tab(wid);

    for arrow in [RIGHT, UP, DOWN] {
        press(&mut app, wid, arrow);
        assert_eq!(
            focus(&app, wid),
            right_pane,
            "{:?} moved nothing",
            arrow.key
        );
        assert_eq!(
            forwarded(&app, arrow),
            (right_pane, arrow.alt_bytes.to_vec()),
            "{:?} reached the focused pane's program",
            arrow.key
        );
        release(&mut app, wid, arrow);
    }

    app.toggle_pane_zoom();
    press(&mut app, wid, LEFT);
    assert_eq!(focus(&app, wid), right_pane);
    assert_eq!(forwarded(&app, LEFT), (right_pane, LEFT.alt_bytes.to_vec()));
    release(&mut app, wid, LEFT);
    app.toggle_pane_zoom();

    press(&mut app, wid, LEFT);
    assert_eq!(focus(&app, wid), 0);
    assert!(consumed(&app, wid, LEFT));
    release(&mut app, wid, LEFT);

    press(&mut app, wid, LEFT);
    assert_eq!(focus(&app, wid), 0);
    assert_eq!(forwarded(&app, LEFT), (0, LEFT.alt_bytes.to_vec()));
    release(&mut app, wid, LEFT);
}

#[test]
fn a_passed_through_press_keeps_its_repeats_and_release() {
    let (mut app, wid) = app_with_alt_arrow_pane_focus();
    let session = app.front_terminal(wid).expect("terminal").session;

    press(&mut app, wid, LEFT);
    app.on_key(wid, event(LEFT, ElementState::Pressed, true));
    let repeat = take_physical_release_trace();
    assert!(
        matches!(
            &repeat,
            Some(PhysicalReleaseTrace::Forwarded {
                session: s,
                key: TKey::Named(TNamed::ArrowLeft),
                mods: Modifiers::ALT,
                event_type: KeyEventType::Repeat,
                ..
            }) if *s == session
        ),
        "the repeat followed its press to the program: {repeat:?}"
    );
    release(&mut app, wid, LEFT);
    let released = take_physical_release_trace();
    assert!(
        matches!(
            &released,
            Some(PhysicalReleaseTrace::Forwarded {
                session: s,
                key: TKey::Named(TNamed::ArrowLeft),
                event_type: KeyEventType::Release,
                ..
            }) if *s == session
        ),
        "the release went to the encoder, not the swallow: {released:?}"
    );
    assert!(owner(&app, LEFT).is_none(), "the hold is over");

    let right_pane = app.split_active_stub_tab(wid);
    press(&mut app, wid, LEFT);
    assert_eq!(focus(&app, wid), 0);
    app.on_key(wid, event(LEFT, ElementState::Pressed, true));
    assert_eq!(focus(&app, wid), 0, "the repeat moved focus no further");
    assert!(
        !matches!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Forwarded { .. })
        ),
        "nothing of the claimed hold reached the program"
    );
    release(&mut app, wid, LEFT);
    assert!(
        matches!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Consumed { .. })
        ),
        "its release is swallowed"
    );
    assert_ne!(right_pane, 0);
}

/// The SEEDED chords (`Keybindings::PLATFORM_DEFAULT_PAIRS`, off macOS) follow the
/// rule: the unedited Windows/Linux config passes Alt+← through on a one-pane tab
/// and moves focus in a split.
#[cfg(not(target_os = "macos"))]
#[test]
fn the_seeded_alt_arrows_follow_the_same_rule() {
    let mut app = App::headless_for_test();
    app.keybindings = crate::keybinding::Keybindings::platform_defaults();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("window").mods = ModifiersState::ALT;
    let session = app.front_terminal(wid).expect("terminal").session;

    press(&mut app, wid, LEFT);
    assert_eq!(forwarded(&app, LEFT), (session, LEFT.alt_bytes.to_vec()));
    release(&mut app, wid, LEFT);

    let right_pane = app.split_active_stub_tab(wid);
    assert_eq!(focus(&app, wid), right_pane);
    press(&mut app, wid, LEFT);
    assert_eq!(focus(&app, wid), 0, "the seed still moves focus in a split");
    assert!(consumed(&app, wid, LEFT));
    release(&mut app, wid, LEFT);
}

#[test]
fn over_a_native_view_alt_left_reaches_the_field_as_word_motion() {
    use crate::native_app::{AppEvent, TextInputEvent};
    let (mut app, wid) = app_with_alt_arrow_pane_focus();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .expect("focus the Settings search field");
    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Commit("cursor trail".to_string())),
    )
    .expect("type into it");
    let (_, view) = app.active_native_view(wid).expect("Settings is frontmost");
    let caret = |app: &App| match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Settings(state)) => {
            state.search_input.selection().range()
        }
        _ => panic!("Settings view"),
    };
    let end = "cursor trail".len();
    assert_eq!(caret(&app), end..end);

    press(&mut app, wid, LEFT);
    assert_eq!(caret(&app), 7..7, "Alt+← walked back one word in the field");
    release(&mut app, wid, LEFT);
}

#[test]
fn a_ctrl_letter_pane_focus_binding_never_hands_its_byte_to_the_shell() {
    use aterm_window::keyboard::SmolStr;
    let ctrl_j = |state| {
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyJ),
            Key::Character(SmolStr::new("j")),
            None,
            KeyLocation::Standard,
            state,
            false,
        )
    };
    let owner_j = |app: &App| {
        app.physical_press_owners
            .get(&PhysicalKey::Code(KeyCode::KeyJ))
            .cloned()
    };
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("window").mods = ModifiersState::CONTROL;
    let session = app.front_terminal(wid).expect("terminal").session;

    app.on_key(wid, ctrl_j(ElementState::Pressed));
    let Some(PhysicalPressOwner::Forwarded {
        session: to,
        key,
        mods,
        base_layout,
        layout_text,
        ..
    }) = owner_j(&app)
    else {
        panic!("unbound Ctrl+J is forwarded: {:?}", owner_j(&app));
    };
    assert_eq!(to, session);
    let mode = app
        .pool
        .get(session)
        .expect("session")
        .ctx
        .modes
        .keyboard_mode();
    assert_eq!(
        aterm_types::keyboard::encode_key_with_text(
            &key,
            mods,
            mode,
            KeyEventType::Press,
            base_layout,
            layout_text
        ),
        b"\n"
    );
    app.on_key(wid, ctrl_j(ElementState::Released));
    assert!(matches!(
        take_physical_release_trace(),
        Some(PhysicalReleaseTrace::Forwarded { .. })
    ));

    app.keybindings = crate::keybinding::Keybindings::resolved(Some(
        &[("ctrl+j".to_string(), "focus_pane_down".to_string())]
            .into_iter()
            .collect(),
    ));
    app.on_key(wid, ctrl_j(ElementState::Pressed));
    assert!(
        matches!(owner_j(&app), Some(PhysicalPressOwner::Consumed { window }) if window == wid),
        "the press is the GUI's: {:?}",
        owner_j(&app)
    );
    assert!(
        app.windows[&wid]
            .consumed_press_keys
            .contains(&PhysicalKey::Code(KeyCode::KeyJ))
    );
    app.on_key(wid, ctrl_j(ElementState::Released));
    assert!(
        matches!(
            take_physical_release_trace(),
            Some(PhysicalReleaseTrace::Consumed { .. })
        ),
        "its release is swallowed with it"
    );
}

#[test]
fn pane_focus_uses_the_event_windows_neighbors_and_press_owner() {
    let mut failures = Vec::new();
    for (split, left_edge, zoomed, front_split) in [
        (true, false, false, false),
        (false, false, false, true),
        (true, false, false, true),
        (true, true, false, true),
        (true, false, true, true),
    ] {
        for literal in [false, true] {
            let (mut app, source) = app_with_alt_arrow_pane_focus();
            if split {
                app.split_active_stub_tab(source);
            }
            if left_edge {
                assert!(app.focus_pane_in(source, crate::pane::FocusDir::Left));
            }
            if zoomed {
                app.toggle_pane_zoom();
            }
            if literal {
                app.key_sequences = crate::keybinding::KeySequences::from_config(Some(
                    &[("alt+left".to_string(), "literal-left".to_string())]
                        .into_iter()
                        .collect(),
                ));
            }
            let before_source = focus(&app, source);
            let next = app.next_session_id;
            let front = app.insert_logical_window(crate::stub_session(next), 24, 80);
            if front_split {
                app.split_active_stub_tab(front);
            }
            let before_front = app.windows[&front].tab_set.active().unwrap().clone();
            assert_eq!(app.frontmost_window, Some(front));
            let moves = split && !left_edge && !zoomed;
            let context = format!(
                "split={split} edge={left_edge} zoom={zoomed} front_split={front_split} literal={literal}"
            );

            press(&mut app, source, LEFT);
            if focus(&app, source) != if moves { 0 } else { before_source } {
                failures.push(format!("{context}: focus did not follow the event window"));
            }
            let expected_owner = match owner(&app, LEFT) {
                Some(PhysicalPressOwner::Consumed { window }) => moves && window == source,
                Some(PhysicalPressOwner::Literal {
                    window,
                    session,
                    event: crate::input::InputEvent::KeySequence(bytes),
                    ..
                }) => {
                    !moves
                        && literal
                        && window == source
                        && session == before_source
                        && bytes.as_slice() == b"literal-left"
                }
                Some(PhysicalPressOwner::Forwarded {
                    window, session, ..
                }) => {
                    !moves
                        && !literal
                        && window == source
                        && session == before_source
                        && forwarded(&app, LEFT) == (before_source, LEFT.alt_bytes.to_vec())
                }
                _ => false,
            };
            if !expected_owner {
                failures.push(format!(
                    "{context}: wrong press owner: {:?}",
                    owner(&app, LEFT)
                ));
            }
            if app.windows[&front].tab_set.active() != Some(&before_front) {
                failures.push(format!("{context}: the unrelated front pane changed"));
            }
            let source_after_press = app.windows[&source].tab_set.active().unwrap().clone();
            let front_after_press = app.windows[&front].tab_set.active().unwrap().clone();
            app.on_key(front, event(LEFT, ElementState::Pressed, true));
            assert_eq!(
                app.windows[&source].tab_set.active(),
                Some(&source_after_press)
            );
            assert_eq!(
                app.windows[&front].tab_set.active(),
                Some(&front_after_press)
            );
            if expected_owner && !moves {
                let trace = take_physical_release_trace();
                assert!(
                    match trace {
                        Some(PhysicalReleaseTrace::Literal {
                            press_window,
                            session,
                            repeated: true,
                            ..
                        }) => literal && press_window == source && session == before_source,
                        Some(PhysicalReleaseTrace::Forwarded {
                            press_window,
                            session,
                            event_type: KeyEventType::Repeat,
                            ..
                        }) => !literal && press_window == source && session == before_source,
                        _ => false,
                    },
                    "{context}: repeats retain their actual press-time route"
                );
            }
            release(&mut app, front, LEFT);
            assert!(
                owner(&app, LEFT).is_none(),
                "{context}: release ends the hold"
            );
            assert!(!consumed(&app, source, LEFT));
            assert_eq!(app.frontmost_window, Some(front));
            assert!(app.structural_invariants_ok());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn configured_and_hardcoded_focus_keep_native_event_window_ownership() {
    let mut failures = Vec::new();
    for native in [false, true] {
        for hardcoded in [false, true] {
            if hardcoded && !super::HARDCODED_SUPER_CHORDS {
                continue;
            }
            let (mut app, source) = app_with_alt_arrow_pane_focus();
            let left = if native {
                assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
                let (instance, left) = app.active_native_view(source).unwrap();
                app.split_active_with_native(
                    source,
                    crate::tab_model::SplitAxis::Horizontal,
                    instance,
                    crate::native_app::AppViewState::Settings(Box::new(
                        crate::native_settings::SettingsViewState::new(&app.config),
                    )),
                )
                .unwrap();
                left
            } else {
                let left = app.windows[&source].tab_set.active().unwrap().focus;
                app.split_active_stub_tab(source);
                left
            };
            let source_before = app.windows[&source].tab_set.active().unwrap().clone();
            let next = app.next_session_id;
            let front = app.insert_logical_window(crate::stub_session(next), 24, 80);
            app.split_active_stub_tab(front);
            let front_before = app.windows[&front].tab_set.active().unwrap().clone();
            app.windows.get_mut(&source).unwrap().mods = if hardcoded {
                ModifiersState::SUPER | ModifiersState::ALT
            } else {
                ModifiersState::ALT
            };
            press(&mut app, source, LEFT);
            let after = app.windows[&source].tab_set.active().unwrap();
            if after.focus != left {
                failures.push(format!(
                    "native={native} hardcoded={hardcoded}: source pane did not take focus"
                ));
            }
            assert_eq!(after.id, source_before.id);
            assert_eq!(after.root, source_before.root);
            if app.windows[&front].tab_set.active() != Some(&front_before) {
                failures.push(format!(
                    "native={native} hardcoded={hardcoded}: front window changed"
                ));
            }
            assert!(
                matches!(owner(&app, LEFT), Some(PhysicalPressOwner::Consumed { window }) if window == source)
            );
            let after = app.windows[&source].tab_set.active().unwrap().clone();
            let front_after = app.windows[&front].tab_set.active().unwrap().clone();
            app.on_key(front, event(LEFT, ElementState::Pressed, true));
            assert_eq!(app.windows[&source].tab_set.active(), Some(&after));
            assert_eq!(app.windows[&front].tab_set.active(), Some(&front_after));
            release(&mut app, front, LEFT);
            assert!(
                matches!(take_physical_release_trace(), Some(PhysicalReleaseTrace::Consumed { press_window, arrival_window }) if press_window == source && arrival_window == front)
            );
            assert!(owner(&app, LEFT).is_none());
            assert_eq!(app.frontmost_window, Some(front));
            assert!(app.structural_invariants_ok());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn configured_and_hardcoded_zoom_keep_the_event_window() {
    let mut failures = Vec::new();
    for (split, native) in [(false, false), (true, false), (true, true)] {
        for hardcoded in [false, true] {
            if hardcoded && !super::HARDCODED_SUPER_CHORDS {
                continue;
            }
            let mut app = App::headless_for_test();
            let source = WindowId(0);
            if native {
                assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
                let (instance, _) = app.active_native_view(source).unwrap();
                app.split_active_with_native(
                    source,
                    crate::tab_model::SplitAxis::Horizontal,
                    instance,
                    crate::native_app::AppViewState::Settings(Box::new(
                        crate::native_settings::SettingsViewState::new(&app.config),
                    )),
                )
                .unwrap();
            } else if split {
                app.split_active_stub_tab(source);
            }
            let source_before = app.windows[&source].tab_set.active().unwrap().clone();
            let next = app.next_session_id;
            let front = app.insert_logical_window(crate::stub_session(next), 24, 80);
            app.split_active_stub_tab(front);
            let front_before = app.windows[&front].tab_set.active().unwrap().clone();
            let (mods, code, logical) = if hardcoded {
                (
                    ModifiersState::SUPER | ModifiersState::SHIFT,
                    KeyCode::Enter,
                    Key::Named(NamedKey::Enter),
                )
            } else {
                app.keybindings = crate::keybinding::Keybindings::resolved(Some(
                    &[("ctrl+shift+z".to_string(), "toggle_pane_zoom".to_string())]
                        .into_iter()
                        .collect(),
                ));
                (
                    ModifiersState::CONTROL | ModifiersState::SHIFT,
                    KeyCode::KeyZ,
                    Key::Character("z".into()),
                )
            };
            app.windows.get_mut(&source).unwrap().mods = mods;
            let zoom_key = |state, repeat| {
                KeyEvent::synthetic_for_test(
                    PhysicalKey::Code(code),
                    logical.clone(),
                    None,
                    KeyLocation::Standard,
                    state,
                    repeat,
                )
            };
            app.on_key(source, zoom_key(ElementState::Pressed, false));
            let after = app.windows[&source].tab_set.active().unwrap();
            if after.zoomed != split {
                failures.push(format!(
                    "split={split} native={native} hardcoded={hardcoded}: zoom missed source"
                ));
            }
            assert_eq!(after.id, source_before.id);
            assert_eq!(after.root, source_before.root);
            assert_eq!(after.focus, source_before.focus);
            if !native && app.active_tree(source).unwrap().is_zoomed() != split {
                failures.push(format!(
                    "split={split} hardcoded={hardcoded}: terminal projection zoom mismatch"
                ));
            }
            if app.windows[&front].tab_set.active() != Some(&front_before) {
                failures.push(format!("split={split} native={native} hardcoded={hardcoded}: unrelated front zoom changed"));
            }
            assert!(
                matches!(app.physical_press_owners.get(&PhysicalKey::Code(code)), Some(PhysicalPressOwner::Consumed { window }) if *window == source)
            );
            let source_after = app.windows[&source].tab_set.active().unwrap().clone();
            let front_after = app.windows[&front].tab_set.active().unwrap().clone();
            app.on_key(front, zoom_key(ElementState::Pressed, true));
            assert_eq!(app.windows[&source].tab_set.active(), Some(&source_after));
            assert_eq!(app.windows[&front].tab_set.active(), Some(&front_after));
            app.on_key(front, zoom_key(ElementState::Released, false));
            assert!(
                matches!(take_physical_release_trace(), Some(PhysicalReleaseTrace::Consumed { press_window, arrival_window }) if press_window == source && arrival_window == front)
            );
            assert!(
                !app.physical_press_owners
                    .contains_key(&PhysicalKey::Code(code))
            );
            assert_eq!(app.frontmost_window, Some(front));
            assert!(app.structural_invariants_ok());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
