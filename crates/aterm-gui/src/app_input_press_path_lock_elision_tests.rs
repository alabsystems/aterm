// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{publish_release_relevance, sampled_release_relevance};
use crate::input::{InputEvent, Source};
use crate::{App, WindowId, term_lock};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
use aterm_window::event::ElementState;
use aterm_window::keyboard::{KeyCode, PhysicalKey};

/// A real winit key event, so the tests below drive the shipping `on_key`
/// routing (where the press-path snap lives) rather than the seam alone.
fn character_event(ch: char, state: ElementState) -> aterm_window::event::KeyEvent {
    let text = aterm_window::keyboard::SmolStr::new(ch.to_string());
    aterm_window::event::KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::KeyA),
        aterm_window::keyboard::Key::Character(text.clone()),
        (state == ElementState::Pressed).then_some(text),
        aterm_window::keyboard::KeyLocation::Standard,
        state,
        false,
    )
}

/// An App whose session capability (not merely the window's mirror) writes to
/// an observer pipe, since the release path writes through
/// `pool.get(session).ctx.sink`. Returns the nonblocking read end. The sink owns
/// the write end, so the fd closes with the last `Arc<SinkWriter>` and no write
/// can reach a parallel test's pipe on a recycled number.
#[cfg(unix)]
fn app_observing_pty() -> (App, i32) {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    (
        App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer))),
        pipe[0],
    )
}

#[cfg(unix)]
fn drain(pipe: i32) -> Vec<u8> {
    let mut bytes = [0u8; 64];
    let read = unsafe { libc::read(pipe, bytes.as_mut_ptr().cast(), bytes.len()) };
    if read <= 0 {
        return Vec::new();
    }
    bytes[..read as usize].to_vec()
}

/// A keypad press and its release carry one identity: the press owner stores the
/// `Numpad5` `build_key_input` resolved, so under kitty `DISAMBIGUATE |
/// REPORT_EVENT_TYPES` the pair is `CSI 57404 u` then `CSI 57404;1:3 u`.
#[cfg(unix)]
#[test]
fn a_keypad_press_and_its_release_report_the_same_keypad_key() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>3u");
    let kp5 = |state: ElementState| {
        let five = aterm_window::keyboard::SmolStr::new("5");
        aterm_window::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::Numpad5),
            aterm_window::keyboard::Key::Character(five.clone()),
            (state == ElementState::Pressed).then_some(five),
            aterm_window::keyboard::KeyLocation::Numpad,
            state,
            false,
        )
    };
    app.on_key(wid, kp5(ElementState::Pressed));
    assert_eq!(
        drain(pipe),
        b"\x1b[57404u".to_vec(),
        "press: the keypad 5, not the main-row 5"
    );
    app.on_key(wid, kp5(ElementState::Released));
    assert_eq!(
        drain(pipe),
        b"\x1b[57404;1:3u".to_vec(),
        "release: the stored keypad key is replayed"
    );
}

/// The window geometry the glow-licence tests tick against.
#[cfg(unix)]
fn glow_geom() -> crate::cursor_glow::Geom {
    crate::cursor_glow::Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    }
}

/// An observing App under Rainbow Kitty with its glow settled at the origin.
#[cfg(unix)]
fn rainbow_app() -> (App, i32, crate::cursor_glow::GlowConfig) {
    let (mut app, pipe) = app_observing_pty();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let cfg = app.glow_config();
    let _ = glow_tick(&mut app, &cfg, (0, 0), std::time::Instant::now());
    (app, pipe, cfg)
}

/// A person's press of `key` under `mods` through the seam.
#[cfg(unix)]
fn human_press(app: &mut App, key: Key, mods: Modifiers) {
    let ev = InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    assert_eq!(
        app.input(WindowId(0), ev, Source::Human),
        crate::input::InputOutcome::Ok
    );
}

/// One glow frame with the caret at `caret`; the glow's spawn count after it.
#[cfg(unix)]
fn glow_tick(
    app: &mut App,
    cfg: &crate::cursor_glow::GlowConfig,
    caret: (u16, u16),
    at: std::time::Instant,
) -> u64 {
    let glow = &mut app
        .windows
        .get_mut(&WindowId(0))
        .unwrap()
        .fx
        .cursor_mut()
        .glow;
    glow.tick(Some(caret), at, cfg, glow_geom(), &mut Vec::new());
    glow.spawns()
}

/// A press that arms its own licence class at dispatch — a plain Enter
/// (`note_return`), a plain Tab (`note_user_gesture`; was: the 8-cell black
/// notch), a bare ⌃V (Claude Code's clipboard-image insert, whose `[Image #1] `
/// echo is laid as one delivered insert) and an IME / dead-key commit — keeps
/// the banked typed stamps of glyphs whose echoes are still in flight (the
/// pre-fix `clear_typed` emptied the bank), and its own response move is
/// licensed; bare, on an empty bank, only its own arm can fund that move. The
/// Control release between a ⌃V and its echo (`clear_move_license`, which wipes
/// the bank) must not kill the insert. The class-aware take and consume-once
/// laws are the engine's (`a_glyph_echo_does_not_spend_the_enter_pressed_behind_it`).
#[cfg(unix)]
#[test]
fn a_self_licensing_press_keeps_banked_stamps_and_licenses_its_response() {
    use std::time::{Duration, Instant};

    use aterm_types::keyboard::NamedKey;

    enum Act {
        Press(Key, Modifiers),
        Commit(&'static str),
    }
    let none = Modifiers::empty();
    let ctrl_v = || Act::Press(Key::Character('v'), Modifiers::CTRL);
    // (label, act, response row, response cells past the caret, echo delay ms,
    //  release fence, licence)
    let cases = [
        (
            "Enter",
            Act::Press(Key::Named(NamedKey::Enter), none),
            2,
            None,
            32,
            false,
            None,
        ),
        (
            "Tab",
            Act::Press(Key::Named(NamedKey::Tab), none),
            0,
            Some(6),
            32,
            false,
            None,
        ),
        ("Ctrl-V", ctrl_v(), 0, Some(11), 300, false, Some("insert")),
        (
            "Ctrl-V, release fence",
            ctrl_v(),
            0,
            Some(11),
            300,
            true,
            Some("insert"),
        ),
        (
            "IME commit",
            Act::Commit("f\u{e9}"),
            0,
            Some(2),
            32,
            false,
            None,
        ),
    ];
    for (label, act, row, cells, delay, fence, licence) in cases {
        for banked in [false, true] {
            if fence && !banked {
                continue;
            }
            let (mut app, pipe, cfg) = rainbow_app();
            if banked {
                human_press(&mut app, Key::Character('g'), none);
                human_press(&mut app, Key::Character('i'), none);
            }
            match &act {
                Act::Press(key, mods) => human_press(&mut app, key.clone(), *mods),
                Act::Commit(text) => app.on_ime_commit(WindowId(0), (*text).to_string()),
            }
            let _ = drain(pipe);
            if fence {
                app.clear_move_license(WindowId(0));
            }
            let e1 = Instant::now();
            let ms = Duration::from_millis;
            let mut spawns = 0;
            let mut caret = 0;
            if banked {
                for (i, col) in [1u16, 2].into_iter().enumerate() {
                    spawns = glow_tick(&mut app, &cfg, (0, col), e1 + ms(16 * i as u64));
                    caret = col;
                }
                if !fence {
                    assert_eq!(spawns, 2, "{label}: banked typed stamps survive");
                }
            }
            let target = (row, cells.map_or(0, |cells| caret + cells));
            assert_eq!(
                glow_tick(&mut app, &cfg, target, e1 + ms(delay)),
                spawns + 1,
                "{label} (banked {banked}): its own response move is licensed"
            );
            if let Some(licence) = licence {
                let glow = &app.windows[&WindowId(0)].fx.cursor().glow;
                assert_eq!(
                    glow.admission_log().last().map(|r| r.licence),
                    Some(licence),
                    "{label} (banked {banked})"
                );
            }
        }
    }
}

/// A press keeps the glow's hand on its row exactly when the seam does. The
/// engine reads the hand's row (where the last licensed typed echo landed) to
/// tell a status tick inside a hesitated key's window from that key's echo.
/// Every route a press takes (`on_key`'s pre-dispatch fence, `App::input`'s entry
/// fence, the dispatch arm, a modifier edge, `control_input::control_fence`, the
/// menu's fence) closes the licence cohort ahead of it, and all of them ask the
/// one classification, `press_fence`, whether the hand keeps its row.
///
/// Observed: 3 s after the press the next key is followed 3 ms later by a status
/// row's advance ending on that key's glyph; a hand kept on the caret's row
/// refuses it `program-row`, a hand taken off lets `spawn` judge it as the echo.
/// Kept: line edits, a Shift edge, a paste, ⌘V, Enter into a program's composer.
/// Taken: Ctrl-R (zle's minibuffer), Escape (vi's `/` after it), ⌘← (no line
/// edit), Enter at a shell prompt.
#[cfg(unix)]
#[test]
fn a_press_keeps_the_glow_s_hand_on_its_row_exactly_when_the_seam_does() {
    use std::time::{Duration, Instant};

    use aterm_effects::rainbow_kitty::TypedClass;
    use aterm_types::keyboard::NamedKey;
    use aterm_window::keyboard::{
        Key as WinitKey, KeyLocation, ModifiersState, NamedKey as WinitNamed, SmolStr,
    };

    use crate::cursor_glow::CursorGlow;
    use crate::input::PasteFraming;

    enum Via {
        /// An event through the seam, as a person's.
        Seam(InputEvent),
        /// A plain Enter through the seam into the line `Line` owns.
        Enter(Line),
        /// A physical press through `on_key`, under the window's modifiers.
        Winit(KeyCode, WinitKey, ModifiersState),
        /// A bare modifier edge.
        ModifierEdge(ModifiersState),
        /// A control verb: `serve_session_verb`'s fence (`control_fence`), then the
        /// event the verb's arm delivers through the seam (`key`'s `parse_key`,
        /// `ctrl`'s `parse_ctrl`, `paste`'s `paste_event`).
        Control(&'static str, &'static str),
        /// The menu's Paste: its fence, then the clipboard's delivery.
        MenuPaste,
    }
    /// Who owns the line an Enter submits.
    enum Line {
        Shell,
        AltScreen,
        Running,
    }
    let press = |key: Key, mods: Modifiers| InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let seam = |key: NamedKey| Via::Seam(press(Key::Named(key), Modifiers::empty()));
    let ctrl = |c: char| Via::Seam(press(Key::Character(c), Modifiers::CTRL));
    let physical = |code: KeyCode, key: WinitNamed, mods: ModifiersState| {
        Via::Winit(code, WinitKey::Named(key), mods)
    };
    let geom = glow_geom();
    let none = ModifiersState::empty();
    let mut cases = vec![
        ("Backspace", seam(NamedKey::Backspace), true),
        ("Left", seam(NamedKey::ArrowLeft), true),
        ("Home", seam(NamedKey::Home), true),
        ("Ctrl-A", ctrl('a'), true),
        ("Ctrl-W", ctrl('w'), true),
        ("Delete", seam(NamedKey::Delete), true),
        (
            "physical Backspace",
            physical(KeyCode::Backspace, WinitNamed::Backspace, none),
            true,
        ),
        (
            "physical Left",
            physical(KeyCode::ArrowLeft, WinitNamed::ArrowLeft, none),
            true,
        ),
        ("Shift edge", Via::ModifierEdge(ModifiersState::SHIFT), true),
        (
            "paste",
            Via::Seam(InputEvent::Paste(
                "hello".to_string(),
                PasteFraming::AtDrain,
            )),
            true,
        ),
        ("menu Paste", Via::MenuPaste, true),
        ("Enter, alternate screen", Via::Enter(Line::AltScreen), true),
        ("Enter, a command running", Via::Enter(Line::Running), true),
        ("ctl key left", Via::Control("key", "left"), true),
        ("ctl key ctrl+w", Via::Control("key", "ctrl+w"), true),
        ("ctl ctrl w", Via::Control("ctrl", "w"), true),
        ("ctl key shiftleft", Via::Control("key", "shiftleft"), true),
        (
            "ctl key left type=release",
            Via::Control("key", "left type=release"),
            true,
        ),
        ("ctl paste", Via::Control("paste", "hello"), true),
        ("Ctrl-R", ctrl('r'), false),
        ("Escape", seam(NamedKey::Escape), false),
        ("Enter, a shell prompt", Via::Enter(Line::Shell), false),
        ("ctl key esc", Via::Control("key", "esc"), false),
    ];
    if super::HARDCODED_SUPER_CHORDS {
        // ⌘-chords are the window's own there (Linux hands Super to the PTY).
        let cmd_v = WinitKey::Character(SmolStr::new("v"));
        cases.push((
            "physical ⌘V",
            Via::Winit(KeyCode::KeyV, cmd_v, ModifiersState::SUPER),
            true,
        ));
        let cmd_left = physical(
            KeyCode::ArrowLeft,
            WinitNamed::ArrowLeft,
            ModifiersState::SUPER,
        );
        cases.push(("physical ⌘←", cmd_left, false));
    }
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("hello".into()));
    let mut wrong = Vec::new();
    for (label, via, keeps) in cases {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_string());
        let glow_cfg = app.glow_config();
        let mut out = Vec::new();
        let mut seq = 0u64;
        let mut frame =
            |glow: &mut CursorGlow, print: Option<(u16, u16, char)>, col: u16, at: Instant| {
                if let Some((row, end, glyph)) = print {
                    seq += 1;
                    glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
                }
                glow.observe_caret_drawn(true);
                glow.tick(Some((0, col)), at, &glow_cfg, geom, &mut out);
            };
        // A status row's first print (no brand), then `h` and its echo:
        // the licensed typed move puts the hand on row 0.
        frame(
            &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow,
            Some((3, 11, '✻')),
            0,
            Instant::now(),
        );
        assert_eq!(
            app.input(
                wid,
                press(Key::Character('h'), Modifiers::empty()),
                Source::Human
            ),
            crate::input::InputOutcome::Ok
        );
        frame(
            &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow,
            None,
            1,
            Instant::now(),
        );
        match via {
            Via::Seam(ev) => {
                assert_eq!(
                    app.input(wid, ev, Source::Human),
                    crate::input::InputOutcome::Ok,
                    "{label}"
                );
            }
            Via::Enter(line) => {
                let term = app.front_terminal(wid).expect("terminal").term.clone();
                match line {
                    Line::Shell => {}
                    Line::AltScreen => term_lock(&term).process(b"\x1b[?1049h"),
                    Line::Running => {
                        term_lock(&term).process(b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;C\x07");
                        assert!(
                            crate::quit_safety::shell_executing(&term_lock(&term)),
                            "PRECONDITION ({label}): the shell says a command runs"
                        );
                    }
                }
                let enter = press(Key::Named(NamedKey::Enter), Modifiers::empty());
                assert_eq!(
                    app.input(wid, enter, Source::Human),
                    crate::input::InputOutcome::Ok,
                    "{label}"
                );
            }
            Via::Winit(code, key, mods) => {
                app.windows.get_mut(&wid).unwrap().mods = mods;
                let text = match &key {
                    WinitKey::Character(c) => Some(c.clone()),
                    _ => None,
                };
                app.on_key(
                    wid,
                    aterm_window::event::KeyEvent::synthetic_for_test(
                        PhysicalKey::Code(code),
                        key,
                        text,
                        KeyLocation::Standard,
                        ElementState::Pressed,
                        false,
                    ),
                );
            }
            Via::ModifierEdge(mods) => app.on_modifiers_changed(wid, mods),
            Via::Control(verb, body) => {
                // The dispatch records its fence on the session; the seam's
                // look applies it before the press arms anything.
                let session = app.front_terminal(wid).unwrap().session;
                if let Some(keep_hand) = crate::control::control_fence(
                    aterm_types::control_verbs::input_of(verb),
                    verb,
                    body,
                    false,
                ) {
                    app.pool
                        .get(session)
                        .expect("the front session")
                        .ctx
                        .licence
                        .fence(keep_hand);
                }
                let ev = match verb {
                    "key" => crate::control::parse_key(body),
                    "ctrl" => crate::control::parse_ctrl(body),
                    _ => Some(crate::control::paste_event(body)),
                }
                .expect("a well-formed body");
                assert_eq!(
                    app.input(wid, ev, Source::Controller),
                    crate::input::InputOutcome::Ok,
                    "{label}"
                );
            }
            Via::MenuPaste => {
                app.fence_menu_action(crate::menu::MenuAction::Paste);
                app.paste_clipboard();
            }
        }
        let _ = drain(pipe);
        // The next key, 3 s on the glow's clock, and the status row's
        // advance ending on its glyph 3 ms after its press.
        let later = Instant::now() + Duration::from_secs(3);
        let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
        glow.note_typed_expected(later, 1, false, TypedClass::Glyph, 'x');
        frame(
            glow,
            Some((3, 18, 'x')),
            1,
            later + Duration::from_millis(3),
        );
        let verdict = glow
            .admission_log()
            .find(|r| r.origin == (3, 11) && r.target == (3, 18))
            .map(|r| r.reason);
        if (verdict == Some(CursorGlow::DECLINE_PROGRAM_ROW)) != keeps {
            wrong.push((label, verdict));
        }
    }
    assert!(
        wrong.is_empty(),
        "the status tick's verdict where the press kept the hand on its row \
         wrongly (label, verdict): {wrong:?}"
    );
}

/// The insert arm is Rainbow Kitty's alone: a bare Tab and a bare ⌃V arm
/// the delivered-insert licence under Rainbow Kitty and no other style,
/// the same gate the paste's delivery edge applies. Only a PLAIN Tab is an
/// insert: ⇧Tab (Claude Code's mode cycle, readline's reverse menu-complete)
/// and a modified Tab arm none, yet each keeps the gesture class (a completion
/// sweep to the classic trail), which every style arms.
#[cfg(unix)]
#[test]
fn a_tab_or_ctrl_v_arms_an_insert_under_rainbow_kitty_only() {
    use aterm_types::keyboard::NamedKey;

    let tab = Key::Named(NamedKey::Tab);
    for (style, armed) in [("rainbow kitty", 2u64), ("lumen", 0), ("comet", 0)] {
        let (mut app, pipe) = app_observing_pty();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some(style.to_string());
        app.kitty_cursor_enabled_cache = None;
        for mods in [
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::SHIFT | Modifiers::CTRL,
        ] {
            app.clear_move_license(WindowId(0));
            human_press(&mut app, tab.clone(), mods);
            assert!(
                app.windows[&WindowId(0)]
                    .fx
                    .cursor()
                    .trail
                    .move_licensed(std::time::Instant::now()),
                "{style} {mods:?}: the gesture class is armed in every style"
            );
        }
        human_press(&mut app, tab.clone(), Modifiers::empty());
        human_press(&mut app, Key::Character('v'), Modifiers::CTRL);
        let _ = drain(pipe);
        assert_eq!(
            app.windows[&WindowId(0)]
                .fx
                .cursor()
                .glow
                .insert_tally()
                .delivered,
            armed,
            "{style}: a bare Tab and a bare ⌃V (never a modified Tab) arm the \
             insert for Rainbow Kitty only"
        );
    }
}

use std::sync::{Arc, Mutex};

use aterm_core::selection::SelectionType;
use aterm_core::terminal::Terminal;
use aterm_window::keyboard::ModifiersState;

/// kitty reports a modifier's release with the state it leaves: with both
/// Shifts down and one let go, `shift` is still in force, so the release
/// carries the bit (`CSI 57441;2:3u`); the last one's release clears it
/// (`CSI 57447;1:3u`), as does a lone Shift's. The window decides from its
/// physical-press record. Control: in legacy mode a bare Shift writes nothing
/// and its release is swallowed with it.
#[cfg(unix)]
#[test]
fn a_modifier_release_keeps_its_bit_while_the_other_side_is_held() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    let shift = |code: KeyCode, state: ElementState| {
        aterm_window::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(code),
            aterm_window::keyboard::Key::Named(aterm_window::keyboard::NamedKey::Shift),
            None,
            if code == KeyCode::ShiftRight {
                aterm_window::keyboard::KeyLocation::Right
            } else {
                aterm_window::keyboard::KeyLocation::Left
            },
            state,
            false,
        )
    };
    app.on_key(wid, shift(KeyCode::ShiftLeft, ElementState::Pressed));
    app.on_key(wid, shift(KeyCode::ShiftLeft, ElementState::Released));
    assert!(
        drain(pipe).is_empty(),
        "legacy: a bare Shift writes nothing"
    );
    // Report all keys (8) and event types (2): releases are reported.
    term_lock(&term).process(b"\x1b[>10u");
    app.on_key(wid, shift(KeyCode::ShiftLeft, ElementState::Pressed));
    assert_eq!(drain(pipe), b"\x1b[57441;2u".to_vec(), "left down");
    app.on_key(wid, shift(KeyCode::ShiftRight, ElementState::Pressed));
    assert_eq!(drain(pipe), b"\x1b[57447;2u".to_vec(), "right down");
    app.on_key(wid, shift(KeyCode::ShiftLeft, ElementState::Released));
    assert_eq!(
        drain(pipe),
        b"\x1b[57441;2:3u".to_vec(),
        "left up while right is held: shift is still in force"
    );
    app.on_key(wid, shift(KeyCode::ShiftRight, ElementState::Released));
    assert_eq!(
        drain(pipe),
        b"\x1b[57447;1:3u".to_vec(),
        "the last Shift up clears the bit"
    );
    // A lone Shift: its release clears the bit, whatever its press saw.
    app.on_key(wid, shift(KeyCode::ShiftLeft, ElementState::Pressed));
    assert_eq!(drain(pipe), b"\x1b[57441;2u".to_vec(), "lone down");
    app.on_key(wid, shift(KeyCode::ShiftLeft, ElementState::Released));
    assert_eq!(drain(pipe), b"\x1b[57441;1:3u".to_vec(), "lone up");
}

/// The live lock-state read reaches the wire in both dialects: Caps Lock
/// capitalizes a letter (xterm applies Lock; kitty and Terminal.app send the
/// OS's 'A' — it used to type 'a') and rides a kitty arrow as its `caps_lock`
/// bit, while an unmapped physical key has no encoding in either and writes
/// nothing.
#[cfg(target_os = "macos")]
#[test]
fn caps_lock_reaches_legacy_and_kitty_while_unmapped_keys_write_nothing() {
    fn read_caps() -> Modifiers {
        Modifiers::CAPS_LOCK
    }
    let arrow = |state| {
        aterm_window::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::ArrowUp),
            aterm_window::keyboard::Key::Named(aterm_window::keyboard::NamedKey::ArrowUp),
            None,
            aterm_window::keyboard::KeyLocation::Standard,
            state,
            false,
        )
    };
    let unmapped = || {
        aterm_window::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::BrowserBack),
            aterm_window::keyboard::Key::Named(aterm_window::keyboard::NamedKey::BrowserBack),
            None,
            aterm_window::keyboard::KeyLocation::Standard,
            ElementState::Pressed,
            false,
        )
    };
    let (mut app, pipe) = app_observing_pty();
    app.lock_modifiers = read_caps;
    let wid = WindowId(0);

    app.on_key(wid, unmapped());
    assert!(drain(pipe).is_empty());
    app.on_key(wid, character_event('a', ElementState::Pressed));
    assert_eq!(drain(pipe), b"A");
    app.on_key(wid, arrow(ElementState::Pressed));
    assert_eq!(drain(pipe), b"\x1b[A");
    app.on_key(wid, arrow(ElementState::Released));
    assert!(drain(pipe).is_empty());

    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>1u");
    app.on_key(wid, unmapped());
    assert!(drain(pipe).is_empty());
    app.on_key(wid, arrow(ElementState::Pressed));
    assert_eq!(drain(pipe), b"\x1b[1;65A");
}

/// The HID read is retained for a mapped key even while the mirror says
/// legacy: the PTY reader may publish a Kitty push immediately afterward.
/// Switch modes *inside* the injected read, before the egress seam samples
/// its mode, and require the newly selected CSI-u wire form to carry Caps.
#[cfg(target_os = "macos")]
#[test]
fn mapped_press_keeps_caps_when_kitty_arrives_between_hid_and_egress() {
    static SWITCH: Mutex<Option<Arc<Mutex<Terminal>>>> = Mutex::new(None);
    fn read_caps_and_switch_mode() -> Modifiers {
        let term = SWITCH
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .expect("one pending mode transition");
        term_lock(&term).process(b"\x1b[>1u");
        Modifiers::CAPS_LOCK
    }

    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    *SWITCH.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(term);
    app.lock_modifiers = read_caps_and_switch_mode;
    app.on_key(
        wid,
        aterm_window::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::ArrowUp),
            aterm_window::keyboard::Key::Named(aterm_window::keyboard::NamedKey::ArrowUp),
            None,
            aterm_window::keyboard::KeyLocation::Standard,
            ElementState::Pressed,
            false,
        ),
    );
    assert_eq!(drain(pipe), b"\x1b[1;65A");
}

/// A bare modifier or lock key as winit really delivers it.
fn modifier_event(
    named: aterm_window::keyboard::NamedKey,
    code: KeyCode,
    state: ElementState,
) -> aterm_window::event::KeyEvent {
    aterm_window::event::KeyEvent::synthetic_for_test(
        PhysicalKey::Code(code),
        aterm_window::keyboard::Key::Named(named),
        None,
        aterm_window::keyboard::KeyLocation::Standard,
        state,
        false,
    )
}

/// A terminal scrolled into history with a completed selection on screen —
/// the state the user is in the instant before they reach for ⌘-C.
fn scrolled_back_with_a_selection(app: &mut App, wid: WindowId) -> Arc<Mutex<Terminal>> {
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    let mut t = term_lock(&term);
    t.process("hello world\r\n".repeat(64).as_bytes());
    t.scroll_to_top();
    assert_ne!(t.grid().display_offset(), 0, "viewport is in history");
    let sel = t.text_selection_mut();
    sel.start_selection(
        0,
        0,
        aterm_core::selection::SelectionSide::Left,
        SelectionType::Simple,
    );
    sel.update_selection(0, 4, aterm_core::selection::SelectionSide::Right);
    sel.complete_selection();
    assert!(t.text_selection().has_selection());
    drop(t);
    term
}

/// Selection custody: ⌘-A selects what you are looking at. The visible rows
/// are mapped through the live `display_offset` (as the mouse path maps a
/// click), so Select All while reading history neither snaps to live nor
/// loses your place.
#[test]
fn select_all_selects_the_visible_screen_without_snapping() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    let off = {
        let mut t = term_lock(&term);
        t.process("line\r\n".repeat(64).as_bytes());
        t.scroll_to_top();
        let off = t.grid().display_offset() as i32;
        assert_ne!(off, 0, "viewport is in history");
        off
    };
    let rows = i32::from(app.windows[&wid].rows);

    app.select_all();

    let t = term_lock(&term);
    assert_eq!(
        t.grid().display_offset() as i32,
        off,
        "Select All must not move the viewport"
    );
    let sel = t.text_selection();
    assert!(sel.has_selection(), "a selection was made");
    assert_eq!(
        sel.start().row,
        -off,
        "the selection starts at the TOP VISIBLE row, not the live top"
    );
    assert_eq!(
        sel.end().row,
        rows - 1 - off,
        "…and ends at the bottom visible row"
    );
}

/// Selection custody: ⌘↓ / ⌘↑ are the deliberate return to live. Routed as
/// a `ScrollIntent`, which moves the viewport without snapping and without
/// clearing, so you return to live with your selection intact.
#[test]
fn cmd_arrows_are_the_deliberate_return_to_live() {
    use crate::input::ScrollIntent;
    use aterm_window::keyboard::NamedKey as WNamed;
    let down = modifier_event(WNamed::ArrowDown, KeyCode::ArrowDown, ElementState::Pressed);
    let up = modifier_event(WNamed::ArrowUp, KeyCode::ArrowUp, ElementState::Pressed);

    // ⌘-arrow is part of the hardcoded Cmd suite: live on macOS/Windows,
    // off on Linux, where Super+arrow is a desktop chord (GNOME
    // tiling/unmaximize) and must fall through.
    // ⌘↓ end to end: `cmd_down_returns_to_live_through_on_key_without_clearing_the_selection`.
    if super::HARDCODED_SUPER_CHORDS {
        assert!(matches!(
            super::scrollback_chord(ModifiersState::SUPER, &up),
            Some(ScrollIntent::Top)
        ));
    } else {
        assert!(super::scrollback_chord(ModifiersState::SUPER, &down).is_none());
        assert!(super::scrollback_chord(ModifiersState::SUPER, &up).is_none());
    }
    // ⌘⌥arrow stays pane focus; ⌘-arrow must not shadow it.
    assert!(
        super::scrollback_chord(ModifiersState::SUPER | ModifiersState::ALT, &down).is_none(),
        "cmd+alt+arrow belongs to pane focus"
    );
    // …and the existing Shift forms are untouched: Shift+End off Windows, and
    // Windows Terminal's Ctrl+Shift+End on Windows, where Shift+End is
    // PSReadLine's SelectLine and reaches the shell.
    let end = modifier_event(WNamed::End, KeyCode::End, ElementState::Pressed);
    let jump = if cfg!(windows) {
        ModifiersState::CONTROL | ModifiersState::SHIFT
    } else {
        ModifiersState::SHIFT
    };
    assert!(matches!(
        super::scrollback_chord(jump, &end),
        Some(ScrollIntent::Bottom)
    ));
    assert_eq!(
        super::scrollback_chord(ModifiersState::SHIFT, &end).is_none(),
        cfg!(windows)
    );
    let page_up = modifier_event(WNamed::PageUp, KeyCode::PageUp, ElementState::Pressed);
    assert!(matches!(
        super::scrollback_chord(ModifiersState::SHIFT, &page_up),
        Some(ScrollIntent::Up)
    ));
    assert_eq!(
        super::scrollback_chord(ModifiersState::CONTROL | ModifiersState::SHIFT, &page_up)
            .is_some(),
        cfg!(windows),
        "Ctrl+Shift+PageUp pages on Windows only"
    );
    // A bare arrow still belongs to the PTY.
    assert!(super::scrollback_chord(ModifiersState::empty(), &down).is_none());
}

/// Selection custody: a bare Cmd chord no shortcut claims is swallowed
/// (terminals never forward Cmd combos) and writes nothing, so it does not move
/// the viewport; its window half still drops the banked glide residual. On Linux
/// (`HARDCODED_SUPER_CHORDS` off) Super belongs to the desktop: the chord falls
/// through to the seam, whose legacy encoding ignores Super as xterm does, so
/// Super+J types `j` and snaps.
#[cfg(unix)]
#[test]
fn a_swallowed_cmd_chord_no_longer_snaps_the_viewport() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    let offset_before = {
        let mut t = term_lock(&term);
        t.process("x\r\n".repeat(64).as_bytes());
        t.scroll_to_top();
        let off = t.grid().display_offset();
        assert_ne!(off, 0, "viewport is in history");
        off
    };
    app.windows.get_mut(&wid).expect("window").scroll_frac_px = 7;
    app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::SUPER);

    app.on_key(wid, character_event('j', ElementState::Pressed));

    if super::HARDCODED_SUPER_CHORDS {
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            offset_before,
            "a swallowed Cmd chord must leave the reading position alone"
        );
        assert_eq!(
            drain(pipe),
            Vec::<u8>::new(),
            "and it must never leak a byte to the PTY"
        );
    } else {
        assert_eq!(
            drain(pipe),
            b"j".to_vec(),
            "on Linux the unclaimed Super chord reaches the PTY (legacy \
             encoding ignores Super, like xterm)"
        );
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "…and a genuine typing press snaps to live like any keystroke"
        );
    }
    assert_eq!(
        app.windows[&wid].scroll_frac_px, 0,
        "the window half still drops the banked glide residual"
    );
    unsafe { libc::close(pipe) };
}

/// The slot is keyed on the session: a sample published for one session
/// never answers for another (a legacy shell's "releases are byte-silent"
/// verdict must not swallow a Kitty app's release reports next door).
#[test]
fn a_sample_answers_only_for_the_session_it_was_taken_on() {
    publish_release_relevance(41, false);
    assert_eq!(sampled_release_relevance(41), Some(false));
    assert_eq!(
        sampled_release_relevance(42),
        None,
        "no cross-session answer"
    );
    publish_release_relevance(41, true);
    assert_eq!(sampled_release_relevance(41), Some(true));
    // Session 0 is a real id and must not collide with the "unsampled" encoding.
    publish_release_relevance(0, false);
    assert_eq!(sampled_release_relevance(0), Some(false));
}

/// Selection custody for the two byte-producing press payloads with no event
/// type: a `[key_sequences]` chord's `InputEvent::KeySequence` and the
/// `option_as_meta = false` / IME-commit fallback's `InputEvent::Text`.
/// `repeat_literal` replays the payload verbatim, so `PressPhase` carries the
/// repeat and an auto-repeat does not re-run the press-path snap+clear.
#[test]
fn a_held_literal_payload_repeats_without_re_taking_the_reading_position() {
    for (code, payload) in [
        (KeyCode::KeyP, InputEvent::KeySequence(b"\x1b[99~".to_vec())),
        (KeyCode::KeyE, InputEvent::Text("é".to_string())),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let physical = PhysicalKey::Code(code);
        let term = scrolled_back_with_a_selection(&mut app, wid);

        // The press still takes custody: the control proving this payload
        // reaches the press path, so the repeat assertion cannot pass by
        // absence.
        app.forward_literal_press(wid, physical, payload.clone(), None);
        {
            let t = term_lock(&term);
            assert_eq!(
                t.grid().display_offset(),
                0,
                "the first landing of {payload:?} must still snap to live"
            );
            assert!(
                !t.text_selection().has_selection(),
                "the first landing of {payload:?} must still deselect"
            );
        }

        // Mid-hold the user wheels back into history and drags a new selection.
        let _ = scrolled_back_with_a_selection(&mut app, wid);
        let offset = term_lock(&term).grid().display_offset();

        // …and the key is still down: more auto-repeat ticks arrive.
        for tick in 0..3 {
            app.route_physical_repeat(wid, physical);
            let t = term_lock(&term);
            assert_eq!(
                t.grid().display_offset(),
                offset,
                "repeat {tick} of {payload:?} snapped the viewport back to live"
            );
            assert!(
                t.text_selection().has_selection(),
                "repeat {tick} of {payload:?} cleared the mid-hold selection"
            );
        }
    }
}

/// Selection custody: a bare modifier or lock key is inert. No snap, no clear
/// (Shift's keydown clearing the selection would make shift-click-extend
/// impossible on macOS), no glide cancel, no blink reset, and no revoked move
/// licence: Option+Left/Right have no key-time cue, so a word move's sound is
/// minted when its echo's caret delta is licensed by the press's `nav_hint`,
/// and macOS delivers `flags_changed` as a synthesized Pressed for
/// `Named(Alt|Shift|…)`, then `ModifiersChanged` edges as Option is released,
/// while that echo is in flight. AltGr and `Fn` have no engine key
/// (`map_logical_key` is `None`), so they never reach the seam and rely on
/// `on_key`'s own `keymap::press_is_inert` gate. The gate is on inertness only,
/// never on auto-repeat: a repeat tick keeps the cursor solid, and ordinary
/// typing still snaps, deselects, drops the glide's banked residual and
/// restarts the blink, while an intent-bearing chord revokes the licence.
#[test]
fn a_bare_modifier_or_lock_key_is_inert() {
    use aterm_window::keyboard::NamedKey as WNamed;
    // Park the cursor mid-blink-OFF with a live blink deadline, bank a
    // sub-row scroll residual, as an in-flight wheel glide would, and stamp
    // the only licence a word move gets (Option+Left, echo not landed yet).
    fn park(app: &mut App, wid: WindowId, deadline: std::time::Instant) {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.blink_phase = false;
        ws.next_blink = Some(deadline);
        ws.scroll_frac_px = 7;
        let now = std::time::Instant::now();
        ws.fx.cursor_mut().glow.note_motion(now);
        ws.fx.cursor_mut().trail.note_navigation(now);
    }
    fn licensed(app: &App, wid: WindowId) -> (bool, bool) {
        let cursor = app.windows[&wid].fx.cursor();
        let now = std::time::Instant::now();
        (
            cursor.glow.move_licensed(now),
            cursor.trail.move_licensed(now),
        )
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = scrolled_back_with_a_selection(&mut app, wid);
    let offset_before = term_lock(&term).grid().display_offset();
    let assert_inert = |app: &App, what: &str| {
        let ws = &app.windows[&wid];
        assert!(!ws.blink_phase, "{what} must not snap the cursor solid");
        assert_eq!(
            ws.next_blink,
            Some(deadline),
            "{what} must not restart the blink period"
        );
        assert_eq!(ws.scroll_frac_px, 7, "{what} must not cancel the glide");
        assert_eq!(
            licensed(app, wid),
            (true, true),
            "{what} must not revoke the word move's licence"
        );
        let t = term_lock(&term);
        assert_eq!(
            t.grid().display_offset(),
            offset_before,
            "{what} must not move the viewport"
        );
        assert!(
            t.text_selection().has_selection(),
            "{what} must not clear the selection"
        );
    };

    for (named, code) in [
        (WNamed::Shift, KeyCode::ShiftLeft),
        (WNamed::Control, KeyCode::ControlLeft),
        (WNamed::Alt, KeyCode::AltLeft),
        (WNamed::Super, KeyCode::SuperRight),
        (WNamed::CapsLock, KeyCode::CapsLock),
        (WNamed::NumLock, KeyCode::NumLock),
        (WNamed::ScrollLock, KeyCode::ScrollLock),
        (WNamed::AltGraph, KeyCode::AltRight),
        (WNamed::Fn, KeyCode::Fn),
        (WNamed::FnLock, KeyCode::AltRight),
    ] {
        park(&mut app, wid, deadline);
        app.on_key(wid, modifier_event(named, code, ElementState::Pressed));
        // Option released before the echo lands (was: `on_modifiers_changed`
        // revoked the `nav_hint`, and the word move failed `no-fresh-hint`).
        app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::ALT);
        app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());
        assert_inert(&app, &format!("bare {named:?}"));
    }

    // The seam half: the same press arriving as an engine event (a controller
    // verb, or a held modifier routed to its press-time session).
    park(&mut app, wid, deadline);
    let _ = app.input(
        wid,
        InputEvent::Key {
            key: Key::Named(aterm_types::keyboard::NamedKey::ShiftLeft),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        Source::Human,
    );
    assert_inert(&app, "a seam-routed bare Shift");

    // An auto-repeat tick of a held ordinary key keeps the cursor solid for
    // the whole hold (the blink is never gated on `PressKind::disturbs()`).
    park(&mut app, wid, deadline);
    let _ = app.input(
        wid,
        InputEvent::Key {
            key: Key::Character('a'),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Repeat,
        },
        Source::Human,
    );
    assert!(
        app.windows[&wid].blink_phase,
        "a repeat tick must keep the cursor solid mid-hold"
    );

    // Control: an intent-bearing press closes the licence cohort.
    for (named, code) in [
        (WNamed::Escape, KeyCode::Escape),
        (WNamed::Backspace, KeyCode::Backspace),
    ] {
        park(&mut app, wid, deadline);
        app.on_key(wid, modifier_event(named, code, ElementState::Pressed));
        assert_eq!(licensed(&app, wid), (false, false), "{named:?} revokes it");
    }

    // Control: ordinary typing.
    let _ = scrolled_back_with_a_selection(&mut app, wid);
    park(&mut app, wid, deadline);
    app.on_key(wid, character_event('a', ElementState::Pressed));
    let ws = &app.windows[&wid];
    assert!(ws.blink_phase, "typing still makes the cursor solid");
    assert_ne!(ws.next_blink, Some(deadline), "…and restarts the blink");
    assert_eq!(ws.scroll_frac_px, 0, "…and drops the banked residual");
    let t = term_lock(&term);
    assert_eq!(t.grid().display_offset(), 0, "…and snaps to live");
    assert!(!t.text_selection().has_selection(), "…and deselects");
}

#[test]
fn a_repeat_into_a_hidden_session_leaves_its_viewport_and_selection_alone() {
    use crate::stub_session;
    use aterm_types::keyboard::{Key as EngineKey, KeyEventType, Modifiers};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let hidden_session = 0u64;
    let term = scrolled_back_with_a_selection(&mut app, wid);
    let offset = term_lock(&term).grid().display_offset();

    // Push a second tab so session 0 is no longer fronted by any window,
    // the state `input_to_hidden_session` exists for.
    let sid = app.next_session_id;
    app.push_stub_tab(wid, stub_session(sid));
    assert_ne!(
        app.front_terminal(wid).map(|t| t.session),
        Some(hidden_session),
        "precondition: session 0 is hidden"
    );

    let held = |event_type| InputEvent::Key {
        key: EngineKey::Character('j'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type,
    };

    // The hold's auto-repeat ticks (the same press continuing) must not take
    // custody a second time.
    for _ in 0..4 {
        app.input_to_hidden_session(
            hidden_session,
            held(KeyEventType::Repeat),
            crate::app_input::PressPhase::Repeat,
            None,
        );
    }
    {
        let t = term_lock(&term);
        assert_eq!(
            t.grid().display_offset(),
            offset,
            "an auto-repeat tick must not snap a hidden session's viewport"
        );
        assert!(
            t.text_selection().has_selection(),
            "an auto-repeat tick must not clear a hidden session's selection"
        );
    }

    // A bare modifier is inert on this path too.
    app.input_to_hidden_session(
        hidden_session,
        InputEvent::Key {
            key: EngineKey::Named(aterm_types::keyboard::NamedKey::ShiftLeft),
            mods: Modifiers::SHIFT,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        crate::app_input::PressPhase::Initial,
        None,
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        offset,
        "a bare modifier expresses no intent, hidden or not"
    );

    // …and the control: a genuine press still takes custody, so the gate is a
    // scoped exclusion, not a blanket "hidden input never disturbs".
    app.input_to_hidden_session(
        hidden_session,
        held(KeyEventType::Press),
        crate::app_input::PressPhase::Initial,
        None,
    );
    let t = term_lock(&term);
    assert_eq!(
        t.grid().display_offset(),
        0,
        "a real press into a hidden session still snaps it to live"
    );
    assert!(!t.text_selection().has_selection(), "…and still deselects");
}

/// A pinned control request (`Wake::Input { session: Some(..) }`) for a session
/// that fronts no window is dropped when view-bound: neither handed to the
/// keyboard-only hidden seam (which would snap and deselect) nor retargeted.
#[test]
fn a_pinned_view_gesture_for_a_hidden_session_is_dropped() {
    use crate::input::{InputOutcome, ScrollIntent};
    use crate::stub_session;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let hidden_session = 0u64;
    let term = scrolled_back_with_a_selection(&mut app, wid);
    let offset = term_lock(&term).grid().display_offset();
    let sid = app.next_session_id;
    app.push_stub_tab(wid, stub_session(sid));

    for ev in [
        InputEvent::Focus(true),
        InputEvent::ScrollView(ScrollIntent::Up),
        InputEvent::Resize {
            rows: 10,
            cols: 20,
            echo_to_window: true,
        },
    ] {
        let outcome = app.input_to_session(
            wid,
            ev,
            Source::Controller,
            Some(hidden_session),
            crate::app_input::PressPhase::Initial,
        );
        assert!(matches!(outcome, InputOutcome::Ok));
    }
    let t = term_lock(&term);
    assert_eq!(t.grid().display_offset(), offset, "viewport left alone");
    assert!(t.text_selection().has_selection(), "selection left alone");
}

/// Selection custody: ⌘↓ through the shipping `App::on_key` returns to live
/// with the selection intact where the hardcoded ⌘ suite is compiled in (on
/// Linux Super is the desktop's: `cmd_arrows_are_the_deliberate_return_to_live`
/// and `a_swallowed_cmd_chord_no_longer_snaps_the_viewport`).
#[test]
fn cmd_down_returns_to_live_through_on_key_without_clearing_the_selection() {
    use aterm_window::keyboard::NamedKey as WNamed;

    if !super::HARDCODED_SUPER_CHORDS {
        return;
    }
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = scrolled_back_with_a_selection(&mut app, wid);
    let text_before = term_lock(&term).selection_to_string();
    assert!(
        text_before.is_some(),
        "precondition: a resolvable selection"
    );

    app.on_modifiers_changed(wid, ModifiersState::SUPER);
    app.on_key(
        wid,
        modifier_event(WNamed::ArrowDown, KeyCode::ArrowDown, ElementState::Pressed),
    );

    let t = term_lock(&term);
    assert_eq!(
        t.grid().display_offset(),
        0,
        "⌘↓ returns the viewport to live"
    );
    assert_eq!(
        t.selection_to_string(),
        text_before,
        "…and the selection it was introduced to preserve survives it, \
         naming the same text"
    );
}

/// A single-cell printable `InputEvent::Text` commit (`ctl type` or single-char
/// IME commit) arms predictive local echo just like a `Key::Character` press,
/// while multi-char or double-width text commits flush pending predictions.
#[cfg(unix)]
#[test]
fn single_char_text_event_arms_predictive_local_echo() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    app.config.predictive_echo = Some("always".into());
    app.predict_mode_cache = None;

    assert_eq!(
        app.input(wid, InputEvent::Text("q".into()), Source::Controller),
        crate::input::InputOutcome::Ok
    );
    assert_eq!(drain(pipe), b"q");
    assert!(
        !app.windows[&wid].predictor.idle(),
        "a single-width printable Text commit arms predictive local echo"
    );

    // Multi-char text commits flush any pending prediction.
    assert_eq!(
        app.input(wid, InputEvent::Text("ab".into()), Source::Controller),
        crate::input::InputOutcome::Ok
    );
    assert_eq!(drain(pipe), b"ab");
    assert!(
        app.windows[&wid].predictor.idle(),
        "a multi-char Text commit flushes pending predictions"
    );
}
