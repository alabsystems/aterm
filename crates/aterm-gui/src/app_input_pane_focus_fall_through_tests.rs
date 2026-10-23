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

/// The four Alt+arrows bound to pane focus by a `[keybindings]` table, as the
/// platform seeds them off macOS. A user's table is the case every platform shares
/// (macOS seeds none), and the rule is the action's, so it is the one pinned here;
/// `the_seeded_alt_arrows_follow_the_same_rule` covers the seed itself. Alt is held.
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

/// The session a press was forwarded to and the bytes the seam writes for it:
/// `encode_key_with_text`, the seam's own encoder (`input::seam_egress_inner`),
/// over the press `on_key` booked and that session's live keyboard mode. Panics
/// when the press was not forwarded to the program at all.
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

/// A one-pane tab has nowhere to move focus, so every Alt+arrow reaches the
/// program: booked as a forwarded press (not a GUI-consumed one), in the focused
/// session, encoded as xterm's Alt+arrow. Alt+←/→ are the word motion the shell
/// integrations bind. Before the rule the chord was claimed and did nothing.
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

/// With a pane that way the chord is still pane focus: focus moves, the press is
/// the GUI's (consumed, its release swallowed) and nothing reaches the program.
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

/// At the edge of a split, and on a zoomed pane, the chord passes through to the
/// FOCUSED pane's program and leaves focus where it was, while a direction that
/// has a pane still moves.
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

/// A passed-through press is booked exactly like any PTY-bound press, so its
/// auto-repeat replays it to the same session and its release reaches the
/// encoder rather than being swallowed. A claimed press's repeat is held and
/// moves focus no further.
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

/// Over a native view (Settings) the same rule holds: a pane-focus chord with no
/// pane that way is no command, so the key reaches the view's editor, where
/// Alt+← is a Settings field's word motion (`app_native`'s `alt_word`). It used to
/// be claimed as pane focus there and the caret stayed put.
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

/// A pane-focus binding OFF the arrows keeps its key with nowhere to move
/// (`keybinding::pane_focus_passes_through`): `ctrl+j = "focus_pane_down"` on a
/// one-pane tab is the GUI's press, a no-op, and never Ctrl+J's LF to the shell,
/// which bash runs as accept-line (the half-typed command). The control: unbound,
/// the same press is forwarded and encodes to that LF.
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
