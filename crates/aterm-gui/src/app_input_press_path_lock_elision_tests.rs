// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{publish_release_relevance, sampled_release_relevance};
use crate::input::{InputEvent, Source};
use crate::{App, WindowId, term_lock};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
use winit::event::ElementState;
use winit::keyboard::{KeyCode, PhysicalKey};

#[cfg(unix)]
fn press(ch: char) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(ch),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

/// A real winit key event, so the tests below drive the shipping `on_key`
/// routing (where the press-path snap lives) rather than the seam alone.
fn character_event(ch: char, state: ElementState) -> winit::event::KeyEvent {
    let text = winit::keyboard::SmolStr::new(ch.to_string());
    winit::event::KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::KeyA),
        winit::keyboard::Key::Character(text.clone()),
        (state == ElementState::Pressed).then_some(text),
        winit::keyboard::KeyLocation::Standard,
        state,
        false,
    )
}

/// An App whose session capability (not merely the window's mirror) writes
/// to an observer pipe: the release path routes through
/// `pool.get(session).ctx.sink`, so a window-only mirror would report
/// silence for bytes that really flowed.
///
/// Returns the pipe's nonblocking read end. The sink owns the write end, as
/// a shipping session owns its master: the fd closes with the last
/// `Arc<SinkWriter>`, so no write of this App's can reach a parallel test's
/// pipe on a recycled number.
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

/// Drive the real key route, including trail licensing and the Cmd-S/R
/// boundary. The fence classifies the press `on_key` builds anyway
/// (`physical_press`, dispatched as built), so an ordinary typed press
/// normalizes nothing: only a binding lookup, the paste test of a fenced
/// chord or the Cmd-S/R check asks for the base key.
#[cfg(unix)]
#[test]
fn ordinary_typed_press_normalizes_no_base_key() {
    let (mut app, pipe) = app_observing_pty();
    super::BASE_KEY_NORMALIZATIONS.with(|count| count.set(0));
    app.on_key(WindowId(0), character_event('a', ElementState::Pressed));
    let normalizations = super::BASE_KEY_NORMALIZATIONS.with(|count| count.replace(0));
    assert_eq!(drain(pipe), b"a", "the typed byte must still reach the PTY");
    assert_eq!(normalizations, 0, "no normalization for a typed press");
}

/// A keypad press and its release carry one identity. `build_key_input`
/// resolves a physical KP_5 to `Numpad5` at press time, the press owner
/// stores that key, and the release replays it, so under kitty
/// `DISAMBIGUATE | REPORT_EVENT_TYPES` the pair is `CSI 57404 u` then
/// `CSI 57404;1:3 u`, never a keypad press paired with a main-row release.
#[cfg(unix)]
#[test]
fn a_keypad_press_and_its_release_report_the_same_keypad_key() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>3u");
    let kp5 = |state: ElementState| {
        let five = winit::keyboard::SmolStr::new("5");
        winit::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::Numpad5),
            winit::keyboard::Key::Character(five.clone()),
            (state == ElementState::Pressed).then_some(five),
            winit::keyboard::KeyLocation::Numpad,
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

/// A plain Enter at the real input boundary arms `note_return` (its
/// coalesced response move is licensed, consume-once) without wiping the
/// banked typed stamps of keys whose echoes are still in flight.
///
/// A stamp is a licence only while its press is unpaid, and the Return's
/// take is class-aware: a same-row forward hop with a typed pair is a
/// glyph's echo, never the Return's move, so `g`'s and `i`'s echoes are
/// licensed by their own presses, the Return is kept for the row change it
/// licenses, and the flood after that is refused.
#[cfg(unix)]
#[test]
fn plain_enter_licenses_its_response_and_keeps_banked_stamps() {
    use std::time::{Duration, Instant};

    use aterm_types::keyboard::NamedKey;

    use crate::cursor_glow::Geom;

    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let glow_cfg = app.glow_config();
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let t0 = Instant::now();
    let mut out = Vec::new();
    // Phase A: bare Enter on an empty bank. The response move can only be
    // funded by `note_return` itself, so a leftover glyph stamp cannot mask
    // a missing arm.
    {
        let (mut bare, bare_pipe) = app_observing_pty();
        bare.config.cursor_trail = Some(true);
        bare.config.cursor_trail_style = Some("rainbow kitty".to_string());
        let bare_cfg = bare.glow_config();
        let b0 = Instant::now();
        bare.windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .glow
            .tick(Some((0, 0)), b0, &bare_cfg, geom, &mut out);
        assert_eq!(
            bare.input(
                wid,
                InputEvent::Key {
                    key: Key::Named(NamedKey::Enter),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human
            ),
            crate::input::InputOutcome::Ok
        );
        let _ = drain(bare_pipe);
        let ws = bare.windows.get_mut(&wid).unwrap();
        let b1 = Instant::now();
        ws.fx
            .cursor_mut()
            .glow
            .tick(Some((2, 0)), b1, &bare_cfg, geom, &mut out);
        assert_eq!(
            ws.fx.cursor_mut().glow.spawns(),
            1,
            "a bare Enter's response move is licensed by note_return alone"
        );
        // …and consumed ONCE: the program flood after it stays dark.
        ws.fx.cursor_mut().glow.tick(
            Some((3, 0)),
            b1 + Duration::from_millis(16),
            &bare_cfg,
            geom,
            &mut out,
        );
        assert_eq!(
            ws.fx.cursor_mut().glow.spawns(),
            1,
            "the return license is depth-1/consume-once — floods still decline"
        );
    }
    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .tick(Some((0, 0)), t0, &glow_cfg, geom, &mut out);
    // Two real glyphs bank stamps; their echoes are in flight when Enter lands.
    for ch in ['g', 'i'] {
        assert_eq!(
            app.input(
                wid,
                InputEvent::Key {
                    key: Key::Character(ch),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human
            ),
            crate::input::InputOutcome::Ok,
            "glyph key must land on the observing PTY"
        );
    }
    assert_eq!(
        app.input(
            wid,
            InputEvent::Key {
                key: Key::Named(NamedKey::Enter),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
            Source::Human
        ),
        crate::input::InputOutcome::Ok
    );
    let _ = drain(pipe);
    let ws = app.windows.get_mut(&wid).unwrap();
    // The first observed move is `g`'s echo (a same-row `+1` with a typed
    // pair), licensed by its own stamp and press and not the Return's: the
    // Return hint is kept for its own row change and `i`'s press stays in
    // flight.
    let e1 = Instant::now();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((0, 1)), e1, &glow_cfg, geom, &mut out);
    assert_eq!(
        ws.fx.cursor_mut().glow.spawns(),
        1,
        "the first glyph's echo is licensed"
    );
    assert_eq!(
        ws.fx.cursor_mut().glow.in_flight_tally().credits,
        1,
        "`i` is still in flight after `g`'s echo — the Return forgot nothing"
    );
    // …the bank survived it (the second glyph's stamp is still there —
    // the pre-fix `clear_typed` would have emptied it)…
    let e2 = e1 + Duration::from_millis(16);
    assert!(
        ws.fx.cursor_mut().glow.move_licensed(e2),
        "banked typed stamps must survive a plain Enter"
    );
    // …and licenses `i`'s echo: the in-flight glyph echo that precedes
    // a Return's own move is lit by its credit (§17.3).
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((0, 2)), e2, &glow_cfg, geom, &mut out);
    assert_eq!(
        ws.fx.cursor_mut().glow.spawns(),
        2,
        "the in-flight glyph echo after a plain Enter is licensed"
    );
    assert_eq!(ws.fx.cursor_mut().glow.in_flight_tally().credits, 0);
    // The Return's own move — the row change — is licensed by
    // `note_return` (was: deterministic no-fresh-hint +1 per Enter).
    ws.fx.cursor_mut().glow.tick(
        Some((2, 0)),
        e1 + Duration::from_millis(32),
        &glow_cfg,
        geom,
        &mut out,
    );
    assert_eq!(
        ws.fx.cursor_mut().glow.spawns(),
        3,
        "Enter's own response move must be licensed"
    );
    // Depth-1/consume-once: everything is spent — a program flood declines.
    ws.fx.cursor_mut().glow.tick(
        Some((3, 0)),
        e1 + Duration::from_millis(48),
        &glow_cfg,
        geom,
        &mut out,
    );
    assert_eq!(
        ws.fx.cursor_mut().glow.spawns(),
        3,
        "the return license is consume-once — a program flood after it stays dark"
    );
}

/// Only a plain Tab is an insert gesture. ⇧Tab is Claude Code's mode cycle
/// and readline's reverse menu-complete (a repaint, not a completion to
/// lay), and a modified Tab is whatever the app binds it to; neither arms
/// the delivered-insert stamp. A bare Tab does.
#[cfg(unix)]
#[test]
fn only_a_plain_tab_arms_the_delivered_insert_stamp() {
    use aterm_types::keyboard::NamedKey;

    use super::classify_press;

    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let tab = |mods: Modifiers| InputEvent::Key {
        key: Key::Named(NamedKey::Tab),
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    assert_eq!(
        app.input(wid, tab(Modifiers::SHIFT), Source::Human),
        crate::input::InputOutcome::Ok
    );
    let _ = drain(pipe);
    assert_eq!(
        app.windows
            .get(&wid)
            .unwrap()
            .fx
            .cursor()
            .glow
            .insert_tally()
            .delivered,
        0,
        "a Shift+Tab arms no delivered insert"
    );
    assert_eq!(
        app.input(wid, tab(Modifiers::empty()), Source::Human),
        crate::input::InputOutcome::Ok
    );
    let _ = drain(pipe);
    assert_eq!(
        app.windows
            .get(&wid)
            .unwrap()
            .fx
            .cursor()
            .glow
            .insert_tally()
            .delivered,
        1,
        "a plain Tab is a delivered insert of unknown width"
    );
    // The classifier's own word, every modifier: the gesture class stays
    // (a reverse menu-complete is a completion sweep to the classic
    // trail), the insert does not.
    for mods in [
        Modifiers::SHIFT,
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::SUPER,
        Modifiers::SHIFT | Modifiers::CTRL,
    ] {
        let ev = tab(mods);
        let class = classify_press(&ev);
        assert!(
            class.tab_key && !class.insert_tab,
            "{mods:?}: a modified Tab keeps the gesture and arms no insert"
        );
    }
    let plain_ev = tab(Modifiers::empty());
    let plain = classify_press(&plain_ev);
    assert!(plain.tab_key && plain.insert_tab);
}

/// A plain Tab arms the user gesture licence (its completion sweep is
/// licensed) and keeps the banked typed stamps (the supersede shape inside
/// `note_user_gesture`).
#[cfg(unix)]
#[test]
fn tab_arms_a_user_gesture_and_keeps_banked_stamps() {
    use std::time::{Duration, Instant};

    use aterm_types::keyboard::NamedKey;

    use crate::cursor_glow::Geom;

    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let glow_cfg = app.glow_config();
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let t0 = Instant::now();
    let mut out = Vec::new();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .tick(Some((0, 0)), t0, &glow_cfg, geom, &mut out);
    for ch in ['l', 's'] {
        assert_eq!(
            app.input(
                wid,
                InputEvent::Key {
                    key: Key::Character(ch),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human
            ),
            crate::input::InputOutcome::Ok
        );
    }
    assert_eq!(
        app.input(
            wid,
            InputEvent::Key {
                key: Key::Named(NamedKey::Tab),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
            Source::Human
        ),
        crate::input::InputOutcome::Ok
    );
    let _ = drain(pipe);
    let ws = app.windows.get_mut(&wid).unwrap();
    // The tab sweep is licensed by the gesture hint…
    let e1 = Instant::now();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((0, 8)), e1, &glow_cfg, geom, &mut out);
    assert_eq!(
        ws.fx.cursor_mut().glow.spawns(),
        1,
        "the Tab completion sweep must be licensed (was: the 8-cell black notch)"
    );
    // …and the surviving banked stamp still licenses the in-flight echo.
    ws.fx.cursor_mut().glow.tick(
        Some((0, 9)),
        e1 + Duration::from_millis(16),
        &glow_cfg,
        geom,
        &mut out,
    );
    assert_eq!(
        ws.fx.cursor_mut().glow.spawns(),
        2,
        "banked typed stamps must survive a mid-burst Tab"
    );

    // BARE Tab on an empty bank: the sweep can only be funded by
    // `note_user_gesture` itself — the direct red for the arm.
    let (mut bare, bare_pipe) = app_observing_pty();
    bare.config.cursor_trail = Some(true);
    bare.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let bare_cfg = bare.glow_config();
    let b0 = Instant::now();
    bare.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .tick(Some((0, 0)), b0, &bare_cfg, geom, &mut out);
    assert_eq!(
        bare.input(
            wid,
            InputEvent::Key {
                key: Key::Named(NamedKey::Tab),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
            Source::Human
        ),
        crate::input::InputOutcome::Ok
    );
    let _ = drain(bare_pipe);
    let ws = bare.windows.get_mut(&wid).unwrap();
    let b1 = Instant::now();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((0, 8)), b1, &bare_cfg, geom, &mut out);
    assert_eq!(
        ws.fx.cursor_mut().glow.spawns(),
        1,
        "a bare Tab's sweep is licensed by note_user_gesture alone"
    );
    // …and consumed ONCE: the program move after it stays dark.
    ws.fx.cursor_mut().glow.tick(
        Some((3, 0)),
        b1 + Duration::from_millis(16),
        &bare_cfg,
        geom,
        &mut out,
    );
    assert_eq!(
        ws.fx.cursor_mut().glow.spawns(),
        1,
        "the gesture license is consume-once — floods still decline"
    );
}

/// A bare ⌃V (Claude Code's clipboard-image insert) arms the
/// delivered-insert class at the dispatch boundary beside the gesture
/// class, bank-preserving: in-flight glyph echoes stay licensed and the
/// app's `[Image #1] ` echo (an 11-cell same-row hop) is laid as one insert
/// sweep. The Control release between the press and the echo runs
/// `clear_move_license` (which wipes the typed bank) and must not kill the
/// insert; the second half re-runs the shape with that fence. A raw ⌃V
/// that echoes nothing (a shell's quoted-insert) leaves a stamp that
/// expires unspent.
#[cfg(unix)]
#[test]
fn a_ctrl_v_press_arms_an_insert_and_keeps_banked_stamps() {
    use std::time::{Duration, Instant};

    use crate::cursor_glow::Geom;

    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let press = |app: &mut App, wid: WindowId, key: Key, mods: Modifiers| {
        assert_eq!(
            app.input(
                wid,
                InputEvent::Key {
                    key,
                    mods,
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human
            ),
            crate::input::InputOutcome::Ok
        );
    };
    for release_fence in [false, true] {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_string());
        let glow_cfg = app.glow_config();
        let t0 = Instant::now();
        let mut out = Vec::new();
        app.windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .glow
            .tick(Some((0, 0)), t0, &glow_cfg, geom, &mut out);
        press(&mut app, wid, Key::Character('h'), Modifiers::empty());
        press(&mut app, wid, Key::Character('i'), Modifiers::empty());
        press(&mut app, wid, Key::Character('v'), Modifiers::CTRL);
        let _ = drain(pipe);
        if release_fence {
            app.clear_move_license(wid);
        }
        let ws = app.windows.get_mut(&wid).unwrap();
        let e1 = Instant::now();
        ws.fx
            .cursor_mut()
            .glow
            .tick(Some((0, 1)), e1, &glow_cfg, geom, &mut out);
        ws.fx.cursor_mut().glow.tick(
            Some((0, 2)),
            e1 + Duration::from_millis(16),
            &glow_cfg,
            geom,
            &mut out,
        );
        if !release_fence {
            assert_eq!(
                ws.fx.cursor_mut().glow.spawns(),
                2,
                "banked typed stamps must survive a ⌃V press"
            );
        }
        let before = ws.fx.cursor_mut().glow.spawns();
        // The app's placeholder echo, 11 cells, laid as one insert.
        ws.fx.cursor_mut().glow.tick(
            Some((0, 13)),
            e1 + Duration::from_millis(300),
            &glow_cfg,
            geom,
            &mut out,
        );
        assert_eq!(
            ws.fx.cursor_mut().glow.spawns(),
            before + 1,
            "the ⌃V placeholder echo is licensed (release fence: {release_fence})"
        );
        assert_eq!(
            ws.fx
                .cursor_mut()
                .glow
                .admission_log()
                .last()
                .map(|r| r.licence),
            Some("insert"),
            "…as a delivered insert (release fence: {release_fence})"
        );
        // Consume-once: the next keyless hop declines.
        ws.fx.cursor_mut().glow.tick(
            Some((0, 20)),
            e1 + Duration::from_millis(320),
            &glow_cfg,
            geom,
            &mut out,
        );
        assert_eq!(
            ws.fx.cursor_mut().glow.spawns(),
            before + 1,
            "one ⌃V, one insert"
        );
    }
}

/// A press keeps the glow's hand on its row exactly when the seam does.
/// The engine reads the hand's row (where the last licensed typed echo
/// landed) to tell a status tick inside a hesitated key's window from that
/// key's echo. Every route a press takes closes the licence cohort ahead of
/// it (`on_key`'s pre-dispatch fence, `App::input`'s entry fence, the
/// dispatch arm, a modifier edge, the control socket's fence
/// (`control_input::control_fence`, fed as `handle` feeds it), the menu's
/// fence), and whether the hand keeps its row is the one classification
/// (`press_fence`) every route asks.
///
/// Observed, not read: after the press, the next key (3 s on, on the glow's
/// clock) is followed 3 ms later by a status row's advance ending on that
/// key's glyph. A hand kept on the caret's row refuses it `program-row`; a
/// hand taken off lets `spawn` judge it as the key's echo.
///
/// Kept: Backspace, an arrow, Home, Ctrl-A, Ctrl-W and Delete through the
/// seam, a physical Backspace and Left, a Shift edge, a paste through the
/// seam, ⌘V through `on_key` and the menu, Enter into a program's composer
/// (the alternate screen, or a command the shell says is running), and
/// behind the control fence `key left`, `key ctrl+w`, `ctrl w`,
/// `key shiftleft`, `key left type=release` and `paste`. Taken: Ctrl-R
/// (zle's minibuffer), Escape (vi's `/` after it), ⌘← (the window's
/// command, no line edit), Enter at a shell prompt, and `key esc` behind
/// the control fence.
#[cfg(unix)]
#[test]
fn a_press_keeps_the_glow_s_hand_on_its_row_exactly_when_the_seam_does() {
    use std::time::{Duration, Instant};

    use aterm_effects::rainbow_kitty::TypedClass;
    use aterm_types::keyboard::NamedKey;
    use winit::keyboard::{
        Key as WinitKey, KeyLocation, ModifiersState, NamedKey as WinitNamed, SmolStr,
    };

    use crate::cursor_glow::{CursorGlow, Geom};
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
        /// A control verb: `handle`'s fence (`control_fence`), then the
        /// event the verb's own command posts through the seam
        /// (`cmd_key`'s `parse_key`, `cmd_ctrl`'s `parse_ctrl`,
        /// `cmd_paste`'s `paste_event`).
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
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
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
                    winit::event::KeyEvent::synthetic_for_test(
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
                let session = app.front_terminal(wid).unwrap().session;
                if let Some(keep_hand) = crate::control::control_fence(verb, body, false) {
                    assert!(app.clear_move_license_for_session(session, keep_hand));
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
/// the same gate the paste's delivery edge applies. The gesture class is
/// armed in every style (the classic trail's lockstep).
#[cfg(unix)]
#[test]
fn a_tab_or_ctrl_v_arms_an_insert_under_rainbow_kitty_only() {
    use aterm_types::keyboard::NamedKey;

    let press = |app: &mut App, wid: WindowId, key: Key, mods: Modifiers| {
        assert_eq!(
            app.input(
                wid,
                InputEvent::Key {
                    key,
                    mods,
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human
            ),
            crate::input::InputOutcome::Ok
        );
    };
    for (style, armed) in [("rainbow kitty", 2u64), ("lumen", 0), ("comet", 0)] {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some(style.to_string());
        app.kitty_cursor_enabled_cache = None;
        press(&mut app, wid, Key::Named(NamedKey::Tab), Modifiers::empty());
        press(&mut app, wid, Key::Character('v'), Modifiers::CTRL);
        let _ = drain(pipe);
        let ws = app.windows.get(&wid).unwrap();
        assert_eq!(
            ws.fx.cursor().glow.insert_tally().delivered,
            armed,
            "{style}: a bare Tab and a bare ⌃V arm the insert for Rainbow Kitty only"
        );
        assert!(
            ws.fx
                .cursor()
                .trail
                .move_licensed(std::time::Instant::now()),
            "{style}: the gesture class is armed in every style"
        );
    }
}

/// An IME / dead-key commit that reaches the PTY keeps the banked stamps of
/// keys still in flight: two keys typed, then `é` committed, and all three
/// echoes light.
#[cfg(unix)]
#[test]
fn an_ime_commit_keeps_in_flight_typed_stamps() {
    use std::time::{Duration, Instant};

    use crate::cursor_glow::Geom;

    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let glow_cfg = app.glow_config();
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let t0 = Instant::now();
    let mut out = Vec::new();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .tick(Some((0, 0)), t0, &glow_cfg, geom, &mut out);
    for ch in ['c', 'a'] {
        assert_eq!(
            app.input(
                wid,
                InputEvent::Key {
                    key: Key::Character(ch),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human
            ),
            crate::input::InputOutcome::Ok
        );
    }
    app.on_ime_commit(wid, "f\u{e9}".to_string());
    let _ = drain(pipe);
    let ws = app.windows.get_mut(&wid).unwrap();
    let e1 = Instant::now();
    for (i, col) in [1u16, 2, 4].into_iter().enumerate() {
        ws.fx.cursor_mut().glow.tick(
            Some((0, col)),
            e1 + Duration::from_millis(16 * i as u64),
            &glow_cfg,
            geom,
            &mut out,
        );
    }
    assert_eq!(
        ws.fx.cursor_mut().glow.spawns(),
        3,
        "the two in-flight glyph echoes and the commit's own echo all light"
    );
}

use std::sync::{Arc, Mutex};

use aterm_core::selection::SelectionType;
use aterm_core::terminal::Terminal;
use winit::keyboard::ModifiersState;

/// A bare modifier press leaves `on_key` before the lock-state RPC, the
/// press scope and the encoder: no `lock_modifiers` call, no bytes, the
/// physical press recorded as Consumed (so its release pairs and is
/// swallowed), and `input_hot` left dark. Under kitty
/// REPORT_ALL_KEYS_AS_ESC the modifier is a reported key: the RPC runs and
/// `ESC[57441u` is written.
#[cfg(unix)]
#[test]
fn a_bare_modifier_press_skips_the_ladder_unless_kitty_reports_all_keys() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static RPC: AtomicUsize = AtomicUsize::new(0);
    fn counting_lock_modifiers() -> aterm_types::keyboard::Modifiers {
        RPC.fetch_add(1, Ordering::Relaxed);
        aterm_types::keyboard::Modifiers::empty()
    }
    let (mut app, pipe) = app_observing_pty();
    app.lock_modifiers = counting_lock_modifiers;
    let wid = WindowId(0);
    let shift = |state| modifier_event(winit::keyboard::NamedKey::Shift, KeyCode::ShiftLeft, state);

    RPC.store(0, Ordering::Relaxed);
    app.on_key(wid, shift(ElementState::Pressed));
    assert_eq!(
        RPC.load(Ordering::Relaxed),
        0,
        "legacy mode: no IOKit RPC for a bare Shift"
    );
    assert!(
        drain(pipe).is_empty(),
        "legacy mode: a bare Shift writes nothing"
    );
    assert!(
        matches!(
            app.physical_press_owners
                .get(&PhysicalKey::Code(KeyCode::ShiftLeft)),
            Some(crate::PhysicalPressOwner::Consumed { .. })
        ),
        "the press must be recorded Consumed so its release pairs and is swallowed"
    );
    assert!(
        !app.windows[&wid].input_hot,
        "a bare modifier must not arm the keystroke-priority frame admission"
    );
    app.on_key(wid, shift(ElementState::Released));
    assert!(drain(pipe).is_empty(), "…and its release is swallowed");

    // Negotiate report-all on the front terminal (the reader would do this
    // under the lock; the mirror republishes at the end of `process`).
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>8u");
    app.on_key(wid, shift(ElementState::Pressed));
    assert_eq!(
        RPC.load(Ordering::Relaxed),
        1,
        "report-all: the modifier is a real key"
    );
    // kitty reports a modifier key with its own modifier bit set: Shift is
    // `57441` with mods `1+1 = 2` — `ESC [ 57441 ; 2 u`.
    assert_eq!(
        drain(pipe),
        b"\x1b[57441;2u".to_vec(),
        "report-all: the bare Shift press is reported"
    );
    // The right Shift is its own kitty key, `RIGHT_SHIFT` 57447: winit names
    // the modifier without a side (the side is the event's location).
    app.on_key(
        wid,
        winit::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::ShiftRight),
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Shift),
            None,
            winit::keyboard::KeyLocation::Right,
            ElementState::Pressed,
            false,
        ),
    );
    assert_eq!(
        drain(pipe),
        b"\x1b[57447;2u".to_vec(),
        "report-all: the right Shift is reported as RIGHT_SHIFT"
    );
}

/// kitty reports a modifier's release with the state it leaves: with both
/// Shifts down and one let go, `shift` is still in force, so the release
/// carries the bit (`CSI 57441;2:3u`); the last one's release clears it
/// (`CSI 57447;1:3u`), as does a lone Shift's. The window decides from its
/// physical-press record.
#[cfg(unix)]
#[test]
fn a_modifier_release_keeps_its_bit_while_the_other_side_is_held() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    // Report all keys (8) and event types (2): releases are reported.
    term_lock(&term).process(b"\x1b[>10u");
    let shift = |code: KeyCode, state: ElementState| {
        winit::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(code),
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Shift),
            None,
            if code == KeyCode::ShiftRight {
                winit::keyboard::KeyLocation::Right
            } else {
                winit::keyboard::KeyLocation::Left
            },
            state,
            false,
        )
    };
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

/// Unmapped physical keys have no wire encoding in either dialect, so they
/// skip HID. Mapped keys keep the live read even in legacy mode, because a
/// Kitty push may arrive before the seam encodes the event.
#[cfg(target_os = "macos")]
#[test]
fn unmapped_mac_keys_skip_hid_while_mapped_keys_keep_legacy_and_kitty_parity() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static READS: AtomicUsize = AtomicUsize::new(0);
    fn read_caps() -> Modifiers {
        READS.fetch_add(1, Ordering::Relaxed);
        Modifiers::CAPS_LOCK
    }
    let arrow = |state| {
        winit::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::ArrowUp),
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowUp),
            None,
            winit::keyboard::KeyLocation::Standard,
            state,
            false,
        )
    };
    let unmapped = || {
        winit::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::BrowserBack),
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::BrowserBack),
            None,
            winit::keyboard::KeyLocation::Standard,
            ElementState::Pressed,
            false,
        )
    };
    let (mut app, pipe) = app_observing_pty();
    app.lock_modifiers = read_caps;
    let wid = WindowId(0);
    READS.store(0, Ordering::Relaxed);

    app.on_key(wid, unmapped());
    assert!(drain(pipe).is_empty());
    assert_eq!(READS.load(Ordering::Relaxed), 0);
    // Caps Lock (the HID read) capitalizes the letter: xterm applies Lock,
    // kitty and Terminal.app send the OS's 'A'. It used to type 'a'.
    app.on_key(wid, character_event('a', ElementState::Pressed));
    assert_eq!(drain(pipe), b"A");
    app.on_key(wid, arrow(ElementState::Pressed));
    assert_eq!(drain(pipe), b"\x1b[A");
    assert_eq!(READS.load(Ordering::Relaxed), 2);
    app.on_key(wid, arrow(ElementState::Released));
    assert!(drain(pipe).is_empty());

    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>1u");
    app.on_key(wid, unmapped());
    assert!(drain(pipe).is_empty());
    assert_eq!(READS.load(Ordering::Relaxed), 2);
    app.on_key(wid, arrow(ElementState::Pressed));
    assert_eq!(drain(pipe), b"\x1b[1;65A");
    assert_eq!(READS.load(Ordering::Relaxed), 3);
}

/// The HID read is retained for a mapped key even while the mirror says
/// legacy: the PTY reader may publish a Kitty push immediately afterward.
/// Switch modes *inside* the injected read, before the egress seam samples
/// its mode, and require the newly selected CSI-u wire form to carry Caps.
#[cfg(target_os = "macos")]
#[test]
fn mapped_press_keeps_caps_when_kitty_arrives_between_hid_and_egress() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static READS: AtomicUsize = AtomicUsize::new(0);
    static SWITCH: Mutex<Option<Arc<Mutex<Terminal>>>> = Mutex::new(None);
    fn read_caps_and_switch_mode() -> Modifiers {
        READS.fetch_add(1, Ordering::Relaxed);
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
    READS.store(0, Ordering::Relaxed);
    app.lock_modifiers = read_caps_and_switch_mode;
    app.on_key(
        wid,
        winit::event::KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::ArrowUp),
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowUp),
            None,
            winit::keyboard::KeyLocation::Standard,
            ElementState::Pressed,
            false,
        ),
    );
    assert_eq!(READS.load(Ordering::Relaxed), 1);
    assert_eq!(drain(pipe), b"\x1b[1;65A");
}

/// A bare MODIFIER or LOCK key as winit really delivers it.
fn modifier_event(
    named: winit::keyboard::NamedKey,
    code: KeyCode,
    state: ElementState,
) -> winit::event::KeyEvent {
    winit::event::KeyEvent::synthetic_for_test(
        PhysicalKey::Code(code),
        winit::keyboard::Key::Named(named),
        None,
        winit::keyboard::KeyLocation::Standard,
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
    use winit::keyboard::NamedKey as WNamed;
    let down = modifier_event(WNamed::ArrowDown, KeyCode::ArrowDown, ElementState::Pressed);
    let up = modifier_event(WNamed::ArrowUp, KeyCode::ArrowUp, ElementState::Pressed);

    // ⌘-arrow is part of the hardcoded Cmd suite: live on macOS/Windows,
    // off on Linux, where Super+arrow is a desktop chord (GNOME
    // tiling/unmaximize) and must fall through.
    if super::HARDCODED_SUPER_CHORDS {
        assert!(matches!(
            super::scrollback_chord(ModifiersState::SUPER, &down),
            Some(ScrollIntent::Bottom)
        ));
        assert!(matches!(
            super::scrollback_chord(ModifiersState::SUPER, &up),
            Some(ScrollIntent::Top)
        ));
    } else {
        assert!(super::scrollback_chord(ModifiersState::SUPER, &down).is_none());
        assert!(super::scrollback_chord(ModifiersState::SUPER, &up).is_none());
    }
    // ⌘⌥arrow stays pane focus; the new chord must not shadow it.
    assert!(
        super::scrollback_chord(ModifiersState::SUPER | ModifiersState::ALT, &down).is_none(),
        "cmd+alt+arrow belongs to pane focus"
    );
    // …and the existing Shift forms are untouched.
    let end = modifier_event(WNamed::End, KeyCode::End, ElementState::Pressed);
    assert!(matches!(
        super::scrollback_chord(ModifiersState::SHIFT, &end),
        Some(ScrollIntent::Bottom)
    ));
    // A bare arrow still belongs to the PTY.
    assert!(super::scrollback_chord(ModifiersState::empty(), &down).is_none());
}

/// Selection custody for modifier keys the engine has no `NamedKey` for:
/// AltGr and laptop/macOS `Fn`. `map_logical_key` returns `None`, so the
/// press never reaches the seam and the terminal half is safe. The window
/// half needs an explicit arm: `cancel_press_scroll_motion` runs off
/// `keymap::press_is_inert`, which answers from the winit key, so resting
/// on AltGr must not kill an in-flight momentum scroll. Asserted on
/// `scroll_frac_px`, the banked sub-row residual that cancel drops.
#[test]
fn unmapped_modifier_keys_are_inert_too() {
    use winit::keyboard::NamedKey as WNamed;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = scrolled_back_with_a_selection(&mut app, wid);
    let offset_before = term_lock(&term).grid().display_offset();

    for named in [WNamed::AltGraph, WNamed::Fn, WNamed::FnLock] {
        // Bank a sub-row scroll residual, as an in-flight wheel glide would.
        app.windows.get_mut(&wid).expect("window").scroll_frac_px = 7;
        app.on_key(
            wid,
            modifier_event(named, KeyCode::AltRight, ElementState::Pressed),
        );
        assert_eq!(
            app.windows[&wid].scroll_frac_px, 7,
            "bare {named:?} must not cancel an in-flight scroll"
        );
        let t = term_lock(&term);
        assert_eq!(
            t.grid().display_offset(),
            offset_before,
            "bare {named:?} must not move the viewport"
        );
        assert!(
            t.text_selection().has_selection(),
            "bare {named:?} must not clear the selection"
        );
    }
    // Control: a real typing press still cancels the glide and snaps.
    app.on_key(wid, character_event('a', ElementState::Pressed));
    assert_eq!(
        app.windows[&wid].scroll_frac_px, 0,
        "typing still drops the banked residual"
    );
    assert_eq!(term_lock(&term).grid().display_offset(), 0);
}

/// Selection custody through the shipping `App::on_key` in the event order
/// macOS produces: winit queues the bare modifier's `KeyboardInput` before
/// the matching `ModifiersChanged`, so on the ⌘ keydown
/// `ws.mods.super_key()` is still `false`, the bare-Cmd swallow does not
/// fire, and the press reaches the seam. It must not snap the viewport or
/// clear the selection. Asserted at every step and for both orderings.
#[test]
fn bare_command_keydown_is_inert_in_either_modifier_order() {
    for settled_first in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = scrolled_back_with_a_selection(&mut app, wid);
        let offset_before = term_lock(&term).grid().display_offset();

        if settled_first {
            app.windows.get_mut(&wid).expect("window").mods = ModifiersState::SUPER;
        }
        app.on_key(
            wid,
            modifier_event(
                winit::keyboard::NamedKey::Super,
                KeyCode::SuperLeft,
                ElementState::Pressed,
            ),
        );
        {
            let t = term_lock(&term);
            assert_eq!(
                t.grid().display_offset(),
                offset_before,
                "bare Command must not move the viewport (settled_first={settled_first})"
            );
            assert!(
                t.text_selection().has_selection(),
                "bare Command must not clear the selection (settled_first={settled_first})"
            );
        }

        // …and now the ModifiersChanged winit queued second finally lands.
        app.windows.get_mut(&wid).expect("window").mods = ModifiersState::SUPER;
        {
            let t = term_lock(&term);
            assert_eq!(t.grid().display_offset(), offset_before);
            assert!(t.text_selection().has_selection());
            // What ⌘-C will read, asserted at the selection rather than
            // through the `c` press and the clipboard it writes.
            let text = t.selection_to_string().expect("selection resolves to text");
            assert!(
                !text.is_empty(),
                "the copy has something to copy (settled_first={settled_first})"
            );
        }
    }
}

/// An inert press is inert about the move licence too. Option+Left/Right
/// have no key-time cue, so a word move's sound is minted when the echo's
/// caret delta is licensed by the press's `nav_hint`. macOS delivers
/// `flags_changed` as a synthesized `KeyboardInput` Pressed for
/// `Named(Alt|Shift|…)`; a bare modifier types, moves and writes nothing,
/// so it must not clear the previous hop's licence while its echo is in
/// flight. Every intent-bearing press still does.
#[test]
fn a_bare_modifier_press_does_not_revoke_a_word_moves_licence() {
    use std::time::Instant;

    use winit::keyboard::NamedKey as WNamed;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    for (named, code, keeps) in [
        (WNamed::Alt, KeyCode::AltLeft, true),
        (WNamed::Shift, KeyCode::ShiftLeft, true),
        (WNamed::Control, KeyCode::ControlLeft, true),
        (WNamed::Super, KeyCode::SuperLeft, true),
        (WNamed::CapsLock, KeyCode::CapsLock, true),
        // The control: an intent-bearing press still closes the cohort.
        (WNamed::Backspace, KeyCode::Backspace, false),
        (WNamed::Escape, KeyCode::Escape, false),
    ] {
        let now = Instant::now();
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            // Option+Left: the press stamps the only licence a word move
            // ever gets, and its echo has not landed yet.
            ws.fx.cursor_mut().glow.note_motion(now);
            ws.fx.cursor_mut().trail.note_navigation(now);
            assert!(ws.fx.cursor_mut().glow.move_licensed(now));
        }
        app.on_key(wid, modifier_event(named, code, ElementState::Pressed));
        let ws = &app.windows[&wid];
        assert_eq!(
            (
                ws.fx.cursor().glow.move_licensed(now),
                ws.fx.cursor().trail.move_licensed(now)
            ),
            (keeps, keeps),
            "{named:?}: a bare modifier expresses no intent and must not \
             revoke the word move's licence, while a real chord must"
        );
    }
}

/// Every other bare modifier and lock key is inert too. Shift matters
/// beyond ⌘-C: clearing the selection on its keydown would make
/// shift-click-to-extend impossible on macOS.
#[test]
fn every_bare_modifier_and_lock_key_is_inert() {
    use winit::keyboard::NamedKey as WNamed;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = scrolled_back_with_a_selection(&mut app, wid);
    let offset_before = term_lock(&term).grid().display_offset();
    for (named, code) in [
        (WNamed::Shift, KeyCode::ShiftLeft),
        (WNamed::Control, KeyCode::ControlLeft),
        (WNamed::Alt, KeyCode::AltLeft),
        (WNamed::Super, KeyCode::SuperRight),
        (WNamed::CapsLock, KeyCode::CapsLock),
        (WNamed::NumLock, KeyCode::NumLock),
        (WNamed::ScrollLock, KeyCode::ScrollLock),
    ] {
        app.on_key(wid, modifier_event(named, code, ElementState::Pressed));
        let t = term_lock(&term);
        assert_eq!(
            t.grid().display_offset(),
            offset_before,
            "bare {named:?} must not move the viewport"
        );
        assert!(
            t.text_selection().has_selection(),
            "bare {named:?} must not clear the selection"
        );
    }
    // The narrowing is surgical: an ordinary character still snaps and
    // deselects, which is what a user means by "start typing and jump back".
    app.on_key(wid, character_event('a', ElementState::Pressed));
    let t = term_lock(&term);
    assert_eq!(t.grid().display_offset(), 0, "typing still snaps");
    assert!(
        !t.text_selection().has_selection(),
        "typing still deselects"
    );
}

/// A seam-bound keystroke lands the whole press-path snap without a
/// caller-side `snap_to_bottom` (a third terminal-mutex acquisition per
/// key): the seam's lock scope pulls the viewport to the live bottom, and
/// the lock-free window half cancels the wheel glide's banked residual so
/// no momentum tail eases the view off the prompt.
#[cfg(unix)]
#[test]
fn a_seam_bound_press_snaps_the_viewport_and_kills_the_glide_residual() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    {
        let mut t = term_lock(&term);
        t.process("x\r\n".repeat(64).as_bytes());
        t.scroll_to_top();
        assert_ne!(t.grid().display_offset(), 0, "viewport is in history");
    }
    app.windows.get_mut(&wid).expect("window").scroll_frac_px = 7;

    app.on_key(wid, character_event('a', ElementState::Pressed));

    assert_eq!(
        term_lock(&term).grid().display_offset(),
        0,
        "the seam's own snap must still pull the view to the live bottom"
    );
    assert_eq!(
        app.windows[&wid].scroll_frac_px, 0,
        "the glide residual must still be dropped at the press"
    );
    assert_eq!(drain(pipe), b"a".to_vec(), "and the key still types");
    unsafe { libc::close(pipe) };
}

/// Selection custody: a bare Cmd chord no shortcut claims is swallowed
/// (terminals never forward Cmd combos), and swallowing it does not move
/// the viewport; it writes nothing, so it expresses no typing intent.
/// Typing still snaps (pinned by
/// `a_seam_bound_press_snaps_the_viewport_and_kills_the_glide_residual`).
/// The window half still runs: `j` is not a modifier, so the banked glide
/// residual is dropped.
///
/// On Linux there is no swallow (`HARDCODED_SUPER_CHORDS` off): Super
/// belongs to the desktop, so an unclaimed Super chord falls through to the
/// seam encoder, whose legacy encoding ignores the Super bit as
/// xterm/gnome-terminal do. Super+J types `j` and snaps like any keystroke.
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
    app.on_modifiers_changed(wid, winit::keyboard::ModifiersState::SUPER);

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

/// Legacy shell (no Kitty flags): the press publishes "releases encode
/// nothing", so the forwarded release skips `seam_egress` and the terminal
/// mutex entirely. Byte-identical: the seam would write nothing and report
/// `Delivery::Full`, as the release path does.
#[cfg(unix)]
#[test]
fn legacy_release_is_byte_silent_and_skips_the_seam() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).expect("terminal").session;
    let physical = PhysicalKey::Code(KeyCode::KeyA);

    let _ = app.input(wid, press('a'), Source::Human);
    assert_eq!(drain(pipe), b"a".to_vec(), "the press itself still types");
    assert_eq!(
        sampled_release_relevance(session),
        Some(false),
        "a legacy press must publish that its release is byte-silent"
    );

    app.note_forwarded_press_key(
        wid,
        physical,
        false,
        Key::Character('a'),
        Modifiers::empty(),
        None,
        None,
    );
    app.release_physical_press(wid, physical);
    assert_eq!(
        drain(pipe),
        Vec::<u8>::new(),
        "a legacy release emits nothing"
    );
    assert!(
        matches!(
            super::take_physical_release_trace(),
            Some(super::PhysicalReleaseTrace::Forwarded {
                delivery: crate::input::Delivery::Full,
                ..
            })
        ),
        "the elided release still reports the seam's own verdict"
    );
    unsafe { libc::close(pipe) };
}

/// Negative control: the guard never eats a real release report. With
/// Kitty `REPORT_EVENT_TYPES` negotiated (`CSI > 2 u`) the press publishes
/// "relevant" and the CSI-u release report reaches the PTY. Ctrl+A, because
/// a bare printable key produces text and has no representable release
/// even in this mode.
#[cfg(unix)]
#[test]
fn kitty_event_type_release_still_reports() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).expect("terminal").session;
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>2u");
    let physical = PhysicalKey::Code(KeyCode::KeyA);

    let _ = app.input(
        wid,
        InputEvent::Key {
            key: Key::Character('a'),
            mods: Modifiers::CTRL,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        Source::Human,
    );
    let _ = drain(pipe);
    assert_eq!(
        sampled_release_relevance(session),
        Some(true),
        "an event-type press must keep its release on the seam"
    );

    app.note_forwarded_press_key(
        wid,
        physical,
        false,
        Key::Character('a'),
        Modifiers::CTRL,
        None,
        None,
    );
    app.release_physical_press(wid, physical);
    assert_eq!(
        drain(pipe),
        b"\x1b[97;5:3u".to_vec(),
        "REPORT_EVENT_TYPES releases must still reach the app"
    );
    unsafe { libc::close(pipe) };
}

/// Selection custody for the two byte-producing press payloads that carry
/// no event type: a `[key_sequences]`-bound chord forwards
/// `InputEvent::KeySequence(bytes)`, and the `option_as_meta = false` /
/// IME-commit fallback forwards `InputEvent::Text` (a key on a non-US
/// layout). `repeat_literal` replays the payload verbatim, so the seam
/// cannot read the repeat off the event; [`PressPhase`] carries it, so an
/// auto-repeat does not re-run the press-path snap+clear.
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
        app.forward_literal_press(wid, physical, payload.clone());
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

        // Mid-hold the user wheels back into history and drags a new
        // selection — the exact case Phase 1 exists to protect.
        let _ = scrolled_back_with_a_selection(&mut app, wid);
        let offset = term_lock(&term).grid().display_offset();

        // …and the key is STILL down: more auto-repeat ticks arrive.
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

/// Selection custody: an inert modifier press means no snap, no clear, no
/// blink reset and no glide cancel. Reaching for ⇧ to shift-click-extend
/// while scrolled back must not snap the cursor solid or restart its
/// period. The gate is on inertness only, never on auto-repeat: a repeat
/// tick and an ordinary press still keep the cursor solid.
#[test]
fn a_bare_modifier_leaves_the_blink_alone_while_typing_and_repeats_reset_it() {
    use winit::keyboard::NamedKey as WNamed;
    // Park the cursor mid-blink-OFF with a live blink deadline, as a settled
    // idle window between two blinks really sits.
    fn park_blink_off(app: &mut App, wid: WindowId, deadline: std::time::Instant) {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.blink_phase = false;
        ws.next_blink = Some(deadline);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    // The `on_key` half: winit's real bare-modifier events. `AltGraph`/`Fn`
    // have no engine key, so they never reach the seam and rely on
    // `on_key`'s own gate, as for the glide cancel in
    // `unmapped_modifier_keys_are_inert_too`.
    for (named, code) in [
        (WNamed::Shift, KeyCode::ShiftLeft),
        (WNamed::Super, KeyCode::SuperLeft),
        (WNamed::Control, KeyCode::ControlLeft),
        (WNamed::Alt, KeyCode::AltLeft),
        (WNamed::CapsLock, KeyCode::CapsLock),
        (WNamed::AltGraph, KeyCode::AltRight),
        (WNamed::Fn, KeyCode::Fn),
    ] {
        park_blink_off(&mut app, wid, deadline);
        app.on_key(wid, modifier_event(named, code, ElementState::Pressed));
        let ws = &app.windows[&wid];
        assert!(
            !ws.blink_phase,
            "bare {named:?} must not snap the cursor solid"
        );
        assert_eq!(
            ws.next_blink,
            Some(deadline),
            "bare {named:?} must not restart the blink period"
        );
    }

    // The SEAM half: the same press arriving as an engine event (a controller
    // verb, or a held modifier routed to its press-time session).
    park_blink_off(&mut app, wid, deadline);
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
    let ws = &app.windows[&wid];
    assert!(!ws.blink_phase, "the seam must honour the same contract");
    assert_eq!(ws.next_blink, Some(deadline), "…period included");

    // Control 1: an auto-repeat tick of a held ordinary key takes no
    // custody, but the cursor stays solid for the whole hold (the blink is
    // never gated on `PressKind::disturbs()`).
    park_blink_off(&mut app, wid, deadline);
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

    // CONTROL 2: ordinary typing, unchanged.
    park_blink_off(&mut app, wid, deadline);
    app.on_key(wid, character_event('a', ElementState::Pressed));
    assert!(
        app.windows[&wid].blink_phase,
        "typing still makes the cursor solid"
    );
    assert_ne!(
        app.windows[&wid].next_blink,
        Some(deadline),
        "…and still restarts the blink period"
    );
}

/// Selection custody's hidden-session press gate. `input_to_hidden_session`
/// is reached only by auto-repeat ticks of a held key aimed at a session no
/// window fronts any more; they must not reset that background session's
/// reading position.
///
/// The hidden-session lock is unconditional and taken on the UI thread, so
/// an inert press must take it once and do nothing under it (`disturbs`
/// short-circuits before every read). Pinned as an operation count, not a
/// wall-clock number: a second acquisition or a self-deadlock fails here.
/// The viewport and selection half is
/// `a_repeat_into_a_hidden_session_leaves_its_viewport_and_selection_alone`.
#[test]
fn an_inert_press_to_a_hidden_session_takes_the_terminal_lock_once() {
    use crate::stub_session;
    use aterm_types::keyboard::{Key as EngineKey, KeyEventType, Modifiers};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let hidden_session = 0u64;
    let _hidden_term = scrolled_back_with_a_selection(&mut app, wid);
    let sid = app.next_session_id;
    app.push_stub_tab(wid, stub_session(sid));

    let bare_shift = InputEvent::Key {
        key: EngineKey::Named(aterm_types::keyboard::NamedKey::ShiftLeft),
        mods: Modifiers::SHIFT,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let acq = crate::term_lock_acquisitions_on_this_thread;
    let before = acq();
    for _ in 0..200 {
        app.input_to_hidden_session(
            hidden_session,
            bare_shift.clone(),
            crate::app_input::PressPhase::Initial,
        );
    }
    assert_eq!(
        acq() - before,
        200,
        "one terminal-lock acquisition per inert hidden-session delivery"
    );
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

    // The auto-repeat ticks of the hold: the SAME press continuing. They must
    // not take custody a second time.
    for _ in 0..4 {
        app.input_to_hidden_session(
            hidden_session,
            held(KeyEventType::Repeat),
            crate::app_input::PressPhase::Repeat,
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
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        offset,
        "a bare modifier expresses no intent, hidden or not"
    );

    // …and the CONTROL: a genuine press still takes custody, so the gate is a
    // scoped exclusion and not a blanket "hidden input never disturbs".
    app.input_to_hidden_session(
        hidden_session,
        held(KeyEventType::Press),
        crate::app_input::PressPhase::Initial,
    );
    let t = term_lock(&term);
    assert_eq!(
        t.grid().display_offset(),
        0,
        "a real press into a hidden session still snaps it to live"
    );
    assert!(!t.text_selection().has_selection(), "…and still deselects");
}

/// A pinned control request (`Wake::Input { session: Some(..) }`) whose
/// session fronts no window by the time it is applied: a view-bound gesture
/// has nothing to act on and is dropped. It is neither handed to the
/// keyboard-only hidden seam (whose press custody would class it a typing
/// press, snapping and deselecting the hidden session) nor retargeted to the
/// tab now on screen.
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

/// The `display_offset() != 0` guard the hidden path never had, pinned on the
/// shared helper that now owns it. The guard is what makes the returned
/// `scrolled` flag mean "the reading position ACTUALLY moved" — the change-gated
/// repaint the seam runs off it — so a helper that snapped unconditionally would
/// report a viewport move on every keystroke of a live terminal.
#[test]
fn press_custody_reports_only_the_disturbances_it_actually_caused() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::Terminal;

    let mut live = Terminal::new(24, 40);
    live.process("line\r\n".repeat(64).as_bytes());
    assert_eq!(live.grid().display_offset(), 0, "precondition: at the tail");
    assert_eq!(
        super::apply_press_custody(&mut live, super::PressKind::Typing),
        (false, false),
        "nothing to snap and nothing to clear: a disturbing press changed nothing"
    );

    let mut scrolled = Terminal::new(24, 40);
    scrolled.process("line\r\n".repeat(64).as_bytes());
    scrolled.scroll_to_top();
    assert_ne!(scrolled.grid().display_offset(), 0);
    {
        let sel = scrolled.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 3, SelectionSide::Right);
        sel.complete_selection();
    }
    assert_eq!(
        super::apply_press_custody(&mut scrolled, super::PressKind::Inert),
        (false, false),
        "a NON-disturbing press must short-circuit before either read"
    );
    assert_ne!(
        scrolled.grid().display_offset(),
        0,
        "…and must leave the viewport where the user put it"
    );
    assert!(scrolled.text_selection().has_selection());

    assert_eq!(
        super::apply_press_custody(&mut scrolled, super::PressKind::Typing),
        (true, true),
        "a disturbing press over a scrolled-back selection reports BOTH effects"
    );
    assert_eq!(scrolled.grid().display_offset(), 0);
    assert!(!scrolled.text_selection().has_selection());
}

/// Selection custody: ⌘↓ returns to live with the selection intact, driven
/// through the shipping `App::on_key`
/// (`cmd_arrows_are_the_deliberate_return_to_live` covers only the pure
/// `scrollback_chord` classifier).
#[test]
fn cmd_down_returns_to_live_through_on_key_without_clearing_the_selection() {
    use winit::keyboard::NamedKey as WNamed;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = scrolled_back_with_a_selection(&mut app, wid);
    let offset_before = term_lock(&term).grid().display_offset();
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
    if super::HARDCODED_SUPER_CHORDS {
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
    } else {
        // On Linux ⌘(Super) belongs to the desktop: the chord falls through
        // to the encoder and lands in the PTY like any other key, so the
        // viewport returns to live and the selection clears. The claimed
        // chord this test guards exists only where the suite is compiled in.
        let _ = offset_before;
        assert_eq!(
            t.grid().display_offset(),
            0,
            "an unclaimed chord reaches the PTY, and input returns the viewport to live"
        );
        assert_eq!(
            t.selection_to_string(),
            None,
            "…clearing the selection exactly as typing any key does"
        );
    }
}
