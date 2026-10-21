// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control socket's licence fence ([`crate::licence_fence`]) lands in the
//! frame path: a fence recorded on a session before its write is applied by the
//! cursor frame before that frame judges the output the write caused, end to end
//! through `redraw_window`.

use crate::scripted_redraw_fixture::{feed, scripted_app};
use crate::{App, WindowId};
use std::time::Instant;

const WID: WindowId = WindowId(0);

fn lit_app() -> App {
    lit(scripted_app())
}

/// [`lit_app`] whose session writes to a pipe, so a press through the input
/// seam reaches its PTY (a failed write revokes the press's stamps), with the
/// read end that keeps the pipe open.
#[cfg(unix)]
fn lit_app_writing() -> (App, std::io::PipeReader) {
    let (reader, writer) = std::io::pipe().expect("a pipe for the session's writes");
    let sink = aterm_session::sink::SinkWriter::new_owned(std::os::fd::OwnedFd::from(writer));
    let app = crate::scripted_redraw_fixture::scripted_app_with_sink(std::sync::Arc::new(sink));
    (lit(app), reader)
}

fn lit(mut app: App) -> App {
    app.config.serious_mode = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.motion = Some("full".into());
    app.config.trail_sounds = Some(false);
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    app.windows.get_mut(&WID).expect("window").focused = true;
    app
}

/// Records a control fence on the front session, as the dispatch does before
/// a driven write.
fn fence_front(app: &App, keep_hand: bool) {
    let session = app.front_terminal(WID).expect("front terminal").session;
    fence_session(app, session, keep_hand);
}

/// Records a control fence on `session`, presented or not.
fn fence_session(app: &App, session: u64, keep_hand: bool) {
    app.pool
        .get(session)
        .expect("a live session")
        .ctx
        .licence
        .fence(keep_hand);
}

/// A typed press's stamps, armed as the input seam arms them.
fn arm_typed_press(app: &mut App, now: Instant) {
    let ws = app.windows.get_mut(&WID).expect("window");
    ws.fx.cursor_mut().glow.note_typed_cells(now, 1);
    ws.fx.cursor_mut().trail.note_typed(now);
}

/// A navigation press's stamps (an arrow, a word motion), armed as the input
/// seam arms them. Its licence is the stamp alone: a typed press's one-cell
/// echo is also licensed by the press's own credit while it is in flight
/// (`CursorHintLicense::InFlightPressEchoMintsLight`), which no boundary
/// closes, so only a stamp-licensed move shows what a fence closes.
fn arm_navigation_press(app: &mut App, now: Instant) {
    let ws = app.windows.get_mut(&WID).expect("window");
    ws.fx.cursor_mut().glow.note_navigation(now);
    ws.fx.cursor_mut().trail.note_navigation(now);
}

/// The glow's `(licensed, declined)` tallies around one move: the caret seeded
/// and presented (the window's first look), a navigation press's stamp armed, a
/// control fence recorded or not, then the move the driven write caused,
/// presented.
fn move_tallies(fenced: bool) -> ((u64, u64), (u64, u64)) {
    let mut app = lit_app();
    feed(&app, b"\x1b[2J\x1b[6;4H");
    app.redraw_window(WID);
    arm_navigation_press(&mut app, Instant::now());
    let tally = |app: &App| {
        let tally = app.windows[&WID].fx.cursor().glow.admission_tally();
        (tally.licensed, tally.declined)
    };
    let before = tally(&app);
    if fenced {
        fence_front(&app, false);
    }
    feed(&app, b"\x1b[6;8H");
    app.redraw_window(WID);
    assert!(
        app.windows[&WID].last_present.is_some(),
        "the move presented"
    );
    (before, tally(&app))
}

/// A fence recorded before the write lands before the frame judges the move
/// the write caused: the stamp a person's press armed earlier licenses
/// nothing, and the move is declined. Negative control: with no fence the same
/// stamp licenses the same move.
#[test]
fn a_fence_lands_before_the_output_it_causes() {
    let (before, after) = move_tallies(false);
    assert_eq!(
        after,
        (before.0 + 1, before.1),
        "unfenced, the armed stamp licenses the move"
    );
    let (before, after) = move_tallies(true);
    assert_eq!(
        after,
        (before.0, before.1 + 1),
        "fenced, the move is judged without the older stamp"
    );
}

/// A press the person makes after the fence keeps its stamp through the
/// frame: the input seam applies the fence before the press arms anything,
/// whether the press comes through `App::input` or winit's `on_key` (which
/// hands its press to the seam), so the frame owes nothing more. Negative
/// control: the same stamp armed after the fence without the seam's look is
/// closed by the frame's.
#[cfg(unix)]
#[test]
fn a_press_after_the_fence_keeps_its_licence_through_the_frame() {
    use crate::input::{InputEvent, InputOutcome, Source};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    use aterm_window::keyboard::{KeyCode, KeyLocation, PhysicalKey};

    // The glow's licence, read at the press's own instant after the frame: a
    // stamp the frame left standing licenses it.
    let survives = |press: fn(&mut App)| {
        let (mut app, _pty) = lit_app_writing();
        feed(&app, b"\x1b[2J\x1b[6;4H");
        app.redraw_window(WID);
        fence_front(&app, false);
        let pressed = Instant::now();
        press(&mut app);
        app.redraw_window(WID);
        app.windows[&WID].fx.cursor().glow.move_licensed(pressed)
    };
    let seam = survives(|app| {
        let left = InputEvent::Key {
            key: Key::Named(NamedKey::ArrowLeft),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        };
        assert_eq!(app.input(WID, left, Source::Human), InputOutcome::Ok);
    });
    assert!(seam, "a press through App::input keeps its stamp");
    let winit = survives(|app| {
        app.on_key(
            WID,
            aterm_window::event::KeyEvent::synthetic_for_test(
                PhysicalKey::Code(KeyCode::ArrowLeft),
                aterm_window::keyboard::Key::Named(aterm_window::keyboard::NamedKey::ArrowLeft),
                None,
                KeyLocation::Standard,
                aterm_window::event::ElementState::Pressed,
                false,
            ),
        );
    });
    assert!(winit, "a press through on_key keeps its stamp");
    let unseen = survives(|app| arm_navigation_press(app, Instant::now()));
    assert!(
        !unseen,
        "a stamp armed past the seam's look is closed by the frame's"
    );
}

/// A switch to another session closes the licence the window armed against
/// the session it leaves (`App::sync_window`'s `fx.front_replaced` curtains
/// the glow), so until the window's first look after the switch, which adopts
/// the new session's counters, no stamp stands for a fence on either session
/// to misjudge: a fence on the new session recorded in that interval, which
/// the window never owes, finds the licence already closed. Negative control:
/// with no switch the same stamp survives the frame.
#[test]
fn a_switch_closes_the_licence_armed_against_the_session_it_leaves() {
    let survives = |switch: bool| {
        let mut app = lit_app();
        let b = app.next_session_id;
        app.push_stub_tab(WID, crate::stub_session(b));
        app.switch_tab_in(WID, 0);
        app.redraw_window(WID);
        let armed = Instant::now();
        arm_navigation_press(&mut app, armed);
        if switch {
            app.switch_tab_in(WID, 1);
            assert_eq!(app.front_terminal(WID).map(|t| t.session), Some(b));
            fence_session(&app, b, false);
        }
        app.redraw_window(WID);
        app.windows[&WID].fx.cursor().glow.move_licensed(armed)
    };
    assert!(survives(false), "with no switch the stamp stands");
    assert!(
        !survives(true),
        "the switch closed the stamp armed against the session it left"
    );
}

/// The look binds to the licence model's swallow clear
/// (`CursorHintLicense::SwallowedKeyClearsLicense`): the epoch store is the
/// boundary, and the look applying it before the engine's read is the clear.
/// The model's `hint` is read off the engine. Negative control: a look with
/// no fence recorded leaves the stamp standing, which the model refuses as
/// that transition.
#[test]
fn a_fence_and_its_look_are_the_licence_models_swallow_clear() {
    let model = aterm_models_effects::cursor_hint_license_model();
    for fenced in [true, false] {
        let mut app = lit_app();
        feed(&app, b"\x1b[2J\x1b[6;4H");
        app.redraw_window(WID);
        let now = Instant::now();
        arm_typed_press(&mut app, now);
        let hint = |app: &App| i64::from(app.windows[&WID].fx.cursor().glow.move_licensed(now));
        let mut armed = model.init_state();
        armed.insert("hint", hint(&app));
        armed.insert("arms", 1);
        armed.insert("credit_arms", 1);
        if fenced {
            fence_front(&app, false);
        }
        app.reconcile_licence(WID);
        let mut after = armed.clone();
        after.insert("hint", hint(&app));
        after.insert("cleared", 1);
        after.insert("swallowed", 1);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &armed,
            &after,
            Some("SwallowedKeyClearsLicense"),
            "control fence applied by the window's look",
        );
        assert_eq!(ok, fenced, "fenced={fenced}: {why}");
    }
}
