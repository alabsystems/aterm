// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{publish_release_relevance, sampled_release_relevance};
use crate::input::{InputEvent, Source};
use crate::{App, WindowId, term_lock};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
use aterm_window::event::ElementState;
use aterm_window::keyboard::{KeyCode, PhysicalKey};

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
/// `pool.get(session).ctx.sink`.
#[cfg(unix)]
fn app_observing_pty() -> (App, i32) {
    app_observing_pty_with(false)
}

/// [`app_observing_pty`], with the write end non-blocking and the sink told so when `immediate`:
/// the master a guarded control write (`if=`) needs for its immediate write.
#[cfg(unix)]
fn app_observing_pty_with(immediate: bool) -> (App, i32) {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let ends: &[i32] = if immediate { &pipe } else { &pipe[..1] };
    for &fd in ends {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
    }
    let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let sink = Arc::new(SinkWriter::new_owned(writer));
    sink.note_master_nonblocking(immediate);
    (App::headless_for_test_with_sink(sink), pipe[0])
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

/// A test's one clock for its presses, its fences and its frames, a step at a time: the engine
/// judges a press against the frame that echoes it, so a loaded host that stalls this thread
/// between them ages no hint. It pins this thread's input clock
/// ([`crate::app_input::pin_input_clock`]), which a press and a licence fence read, and lets it
/// go when dropped.
#[cfg(unix)]
struct Tape(std::cell::Cell<std::time::Instant>);

#[cfg(unix)]
impl Tape {
    fn new() -> Self {
        Self(std::cell::Cell::new(std::time::Instant::now()))
    }

    /// The next step, 4 ms on: what the test presses or fences before the next step is stamped
    /// with it, and a frame given it draws at it.
    fn step(&self) -> std::time::Instant {
        let at = self.0.get() + std::time::Duration::from_millis(4);
        self.0.set(at);
        crate::app_input::pin_input_clock(Some(at));
        at
    }
}

#[cfg(unix)]
impl Drop for Tape {
    fn drop(&mut self) {
        crate::app_input::pin_input_clock(None);
    }
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

/// A person's pointer gesture on the grid.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum Gesture {
    Click,
    /// A left press dragged along row 2 and released, as when selecting a path to copy it.
    Drag,
    Wheel,
}

/// Drive `gesture` over window row 2 through the real pointer handlers.
#[cfg(unix)]
fn pointer_gesture(app: &mut App, wid: WindowId, gesture: Gesture) {
    use aterm_window::event::{MouseButton, MouseScrollDelta};

    let to = |app: &mut App, col: u16| {
        let (cw, ch) = app.win_cell_size(wid);
        let strip = f64::from(app.tab_strip_rows) * ch as f64;
        let x = app.win_pad(wid) as f64 + cw as f64 * (f64::from(col) + 0.5);
        let y = (app.win_pad_top(wid) + app.win_head(wid)) as f64 + strip + ch as f64 * 2.5;
        app.on_cursor_moved(wid, x, y);
        assert_eq!(app.windows[&wid].last_mouse_cell, (2, col));
    };
    to(app, 3);
    match gesture {
        Gesture::Click | Gesture::Drag => {
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            if matches!(gesture, Gesture::Drag) {
                for col in 4..=9 {
                    to(app, col);
                }
            }
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        }
        Gesture::Wheel => app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -1.0)),
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

/// Admission history belongs to the views that produced it, not the tab or
/// same-session mirror that happens to be front when the diagnostics are read.
#[cfg(unix)]
#[test]
fn trail_admission_provenance_survives_tab_and_shared_session_view_changes() {
    use std::time::Instant;

    let (mut app, pipe, cfg) = rainbow_app();
    let wid = WindowId(0);
    let source = app.front_terminal(wid).unwrap().session;
    let source_view = app.windows[&wid].tab_set.active().unwrap().focus;
    let config = app.native_config_service.snapshot();
    let field = |row: &str, key: &str, value: u64| {
        let expected = format!("{key}={value}");
        row.split_ascii_whitespace().any(|token| token == expected)
    };
    let check_target = |app: &App, owner: WindowId, view: crate::tab_model::ViewId, sid| {
        let mut failures = Vec::new();
        let rows = app.trail_admissions(Some(0)).unwrap();
        let target = rows.iter().find(|row| row.starts_with("trail-target "));
        let status = app.trail_status().unwrap();
        for (key, value) in [("window", owner.0), ("view", view.get()), ("session", sid)] {
            if !target.is_some_and(|row| field(row, key, value)) {
                failures.push(format!("admission target lacks {key}={value}: {rows:?}"));
            }
            if !field(&status, key, value) {
                failures.push(format!("standing target lacks {key}={value}"));
            }
        }
        assert_eq!(
            rows.iter()
                .filter(|row| row.starts_with("admission "))
                .count(),
            0,
            "the target row does not spend the requested admission limit"
        );
        failures
    };
    let mut failures = check_target(&app, wid, source_view, source);
    app.on_key(wid, character_event('a', ElementState::Pressed));
    app.on_key(wid, character_event('a', ElementState::Released));
    assert_eq!(drain(pipe), b"a", "the actual key reaches A's sink");
    term_lock(&app.front_terminal(wid).unwrap().term).process(b"a");
    glow_tick(&mut app, &cfg, (0, 1), Instant::now());
    let first = *app.windows[&wid]
        .fx
        .cursor()
        .glow
        .admission_log()
        .last()
        .unwrap();
    assert_eq!(first.phase, crate::cursor_glow::AdmissionPhase::Licensed);
    assert_eq!(first.licence, "key");
    assert_eq!((first.origin, first.target), ((0, 0), (0, 1)));
    assert!(first.seq > 0);

    let sibling = app.next_session_id;
    let sink = app.pool.get(source).unwrap().ctx.sink.clone();
    app.push_stub_tab(wid, crate::stub_session_with_sink(sibling, sink));
    let sibling_view = app.windows[&wid].tab_set.active().unwrap().focus;
    assert_eq!(app.front_terminal(wid).unwrap().session, sibling);
    assert_ne!(source_view, sibling_view);
    assert_eq!(
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .admission_log()
            .copied()
            .collect::<Vec<_>>(),
        vec![first],
        "switching targets retains the actual historical admission"
    );
    failures.extend(check_target(&app, wid, sibling_view, sibling));
    glow_tick(&mut app, &cfg, (0, 0), Instant::now());
    app.on_key(wid, character_event('b', ElementState::Pressed));
    app.on_key(wid, character_event('b', ElementState::Released));
    assert_eq!(drain(pipe), b"b", "the actual key reaches B's sink");
    term_lock(&app.front_terminal(wid).unwrap().term).process(b"b");
    glow_tick(&mut app, &cfg, (0, 1), Instant::now());
    let records = app.windows[&wid]
        .fx
        .cursor()
        .glow
        .admission_log()
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0], first, "new input never rewrites old records");
    assert_eq!(
        records[1].phase,
        crate::cursor_glow::AdmissionPhase::Licensed
    );
    assert!(records[1].seq > first.seq);
    let rows = app.trail_admissions(Some(1)).unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.starts_with("admission "))
            .count(),
        1
    );
    for (record, view, session) in [
        (records[0], source_view, source),
        (records[1], sibling_view, sibling),
    ] {
        let row = record.line(Instant::now());
        if !field(&row, "source_view", view.get()) || !field(&row, "source_session", session) {
            failures.push(format!(
                "sequence {} lost its producing view/session: {row}",
                record.seq
            ));
        }
    }
    app.switch_tab_in(wid, 0);
    assert_eq!(app.front_terminal(wid).unwrap().session, source);
    assert_eq!(
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .admission_log()
            .copied()
            .collect::<Vec<_>>(),
        records
    );
    failures.extend(check_target(&app, wid, source_view, source));

    let mirror = app.open_active_session_in_new_window_logical().unwrap();
    let mirror_view = app.windows[&mirror].tab_set.active().unwrap().focus;
    assert_ne!(mirror_view, source_view);
    assert_eq!(app.frontmost_window, Some(mirror));
    assert_eq!(app.front_terminal(mirror).unwrap().session, source);
    failures.extend(check_target(&app, mirror, mirror_view, source));
    assert!(
        app.windows[&mirror]
            .fx
            .cursor()
            .glow
            .admission_log()
            .next()
            .is_none()
    );
    assert!(app.structural_invariants_ok());
    assert_eq!(app.native_config_service.snapshot(), config);
    drop(app);
    unsafe { libc::close(pipe) };
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A focus roundtrip and a status repaint leave ordinary input measurable.
#[cfg(unix)]
#[test]
fn focus_reports_preserve_composer_typing_without_unparking_program_output() {
    use std::time::Instant;

    let wid = WindowId(0);
    let focus = |app: &mut App, focused: bool, os: bool| {
        if os {
            app.on_focus(wid, focused);
        } else {
            assert_eq!(
                app.input(wid, InputEvent::Focus(focused), Source::Human),
                crate::input::InputOutcome::Ok
            );
        }
    };
    for os_focus in [false, true] {
        {
            let (mut app, pipe, cfg) = rainbow_app();
            app.ensure_sparkle();
            let term = app.front_terminal(wid).unwrap().term.clone();
            term_lock(&term).process(b"\x1b[?1049h\x1b[4;1H> ");
            let mut row = vec![' '; 40];
            row[0] = '>';
            let start = Instant::now();
            {
                let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
                glow.note_context(true);
                glow.observe_caret_drawn(true);
                glow.observe_print_anchor_glyph(Some((3, 2, 1)), Some(' '));
                glow.observe_row(3, 2, &row, start);
            }
            glow_tick(&mut app, &cfg, (3, 2), start);
            focus(&mut app, false, os_focus);
            term_lock(&term).process(b"\x1b[?2026h\x1b[1;1Hwork\x1b[4;3H\x1b[?2026l");
            let status_at = Instant::now();
            {
                let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
                glow.observe_print_anchor_glyph(Some((0, 5, 2)), Some('k'));
                glow.observe_row(3, 2, &row, status_at);
            }
            glow_tick(&mut app, &cfg, (3, 2), status_at);
            focus(&mut app, true, os_focus);
            // The focus-in report can repaint a status line too, before the
            // person types. Its print anchor remains away from the composer.
            term_lock(&term).process(b"\x1b[?2026h\x1b[1;1Hback\x1b[4;3H\x1b[?2026l");
            let focused_at = Instant::now();
            {
                let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
                glow.observe_print_anchor_glyph(Some((0, 5, 3)), Some('k'));
                glow.observe_row(3, 2, &row, focused_at);
            }
            glow_tick(&mut app, &cfg, (3, 2), focused_at);
            assert_eq!(
                app.windows.get(&wid).unwrap().fx.cursor().glow.spawns(),
                0,
                "focus and unrelated output mint no typing"
            );
            for (i, glyph) in "reuse".chars().enumerate() {
                // The real input arm records the exact glyph and sends it to the
                // child. The row sample below is the child's visible echo.
                human_press(&mut app, Key::Character(glyph), Modifiers::empty());
                {
                    let mut term = term_lock(&term);
                    term.process(glyph.to_string().as_bytes());
                    // A plain echo never hides the caret inside DEC 2026, so
                    // the renderer has no repaint-blink cue to forward.
                    assert_eq!(term.repaint_blink_epoch(), 0);
                }
                let col = 2 + i as u16;
                row[usize::from(col)] = glyph;
                let echo = Instant::now();
                let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
                glow.observe_print_anchor_glyph(Some((3, col + 1, 4 + i as u64)), Some(glyph));
                glow.observe_row(3, col + 1, &row, echo);
                glow.tick(Some((3, col + 1)), echo, &cfg, glow_geom(), &mut Vec::new());
            }
            let glow = &app.windows.get(&wid).unwrap().fx.cursor().glow;
            let cols: Vec<_> = glow
                .v2_ribbon()
                .unwrap()
                .cells()
                .iter()
                .filter(|c| c.row == 3)
                .map(|c| c.col)
                .collect();
            assert!(
                (2..7).all(|col| cols.contains(&col)),
                "ordinary typing after focus must light (OS focus={os_focus}): {cols:?}"
            );
            assert_eq!(glow.spawns(), 5);
            let _ = drain(pipe);
            unsafe { libc::close(pipe) };
        }
        // Without a key, focus and background caret motion earn no typing,
        // including when a program moves its caret back to the composer.
        let (mut app, pipe, cfg) = rainbow_app();
        app.ensure_sparkle();
        focus(&mut app, false, os_focus);
        for (row, col) in [(3, 2), (0, 6), (3, 2)] {
            let at = Instant::now();
            let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
            glow.note_context(true);
            glow.observe_caret_drawn(true);
            glow.observe_print_anchor_glyph(Some((0, 6, 1)), Some('g'));
            glow.observe_row(row, col, &[' '; 40], at);
            glow.tick(Some((row, col)), at, &cfg, glow_geom(), &mut Vec::new());
        }
        focus(&mut app, true, os_focus);
        assert_eq!(app.windows.get(&wid).unwrap().fx.cursor().glow.spawns(), 0);
        let _ = drain(pipe);
        unsafe { libc::close(pipe) };
    }
}

/// A press that arms its own licence class at dispatch — a plain Enter
/// (`note_return`), a plain Tab (`note_user_gesture`; was: the 8-cell black
/// notch), a bare ⌃V (Claude Code's clipboard-image insert, whose `[Image #1] `
/// echo is laid as one delivered insert) and an IME / dead-key commit — keeps
/// the banked typed stamps of glyphs whose echoes are still in flight (the
/// pre-fix `clear_typed` emptied the bank), and its own response move is
/// licensed; bare, on an empty bank, only its own arm can fund that move.
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
        /// A physical press through `on_key` with a keybindings table that binds
        /// `ctrl+shift+c` to Copy, as Linux and Windows bind it by default, and
        /// `ctrl+shift+l` to Select All. Their bare control keys are generic
        /// boundaries, so only the binding keeps the hand; `ctrl+shift+a`, the
        /// default Select All, carries Ctrl-A, a navigation key that keeps it anyway.
        Bound(KeyCode, &'static str, ModifiersState),
        /// A bare modifier edge.
        ModifierEdge(ModifiersState),
        /// A control verb: `serve_session_verb`'s fence (`control_fence`), then the
        /// event the verb's arm delivers through the seam (`key`'s `parse_key`,
        /// `ctrl`'s `parse_ctrl`, `mouse`'s `parse_mouse`, `paste`'s `paste_event`).
        Control(&'static str, &'static str),
        /// The menu's Paste: its fence, then the clipboard's delivery.
        MenuPaste,
        /// The menu's fence for a command that sends the program nothing (Copy,
        /// Select All), then the command.
        Menu(crate::menu::MenuAction),
        /// A click on the native tab bar's chip for the tab already in front.
        ActiveTabChip,
        /// A click on the window's presence pill.
        Pill,
        /// A tab's context menu opening (its chip right-clicked).
        #[cfg(target_os = "macos")]
        TabMenuOpening,
        /// A pointer gesture through the real handlers; `true` puts a mouse-tracking
        /// app (DEC 1003 with SGR reports) in front first.
        Pointer(Gesture, bool),
    }
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
        ("menu Copy", Via::Menu(crate::menu::MenuAction::Copy), true),
        (
            "menu Select All",
            Via::Menu(crate::menu::MenuAction::SelectAll),
            true,
        ),
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
        ("click", Via::Pointer(Gesture::Click, false), true),
        ("drag", Via::Pointer(Gesture::Drag, false), true),
        ("wheel", Via::Pointer(Gesture::Wheel, false), true),
        (
            "click, tracking app",
            Via::Pointer(Gesture::Click, true),
            true,
        ),
        (
            "drag, tracking app",
            Via::Pointer(Gesture::Drag, true),
            true,
        ),
        (
            "wheel, tracking app",
            Via::Pointer(Gesture::Wheel, true),
            true,
        ),
        (
            "ctl mouse press",
            Via::Control("mouse", "press left 2 3"),
            true,
        ),
        (
            "ctl mouse drag",
            Via::Control("mouse", "move left 2 5"),
            true,
        ),
        (
            "ctl mouse wheel",
            Via::Control("mouse", "wheelup left 2 3"),
            true,
        ),
        ("active tab chip", Via::ActiveTabChip, true),
        ("pill", Via::Pill, true),
        (
            "bound Copy, ctrl+shift+c",
            Via::Bound(
                KeyCode::KeyC,
                "c",
                ModifiersState::CONTROL | ModifiersState::SHIFT,
            ),
            true,
        ),
        (
            "bound Select All, ctrl+shift+l",
            Via::Bound(
                KeyCode::KeyL,
                "l",
                ModifiersState::CONTROL | ModifiersState::SHIFT,
            ),
            true,
        ),
        (
            "unbound ctrl+shift+x beside them",
            Via::Bound(
                KeyCode::KeyX,
                "x",
                ModifiersState::CONTROL | ModifiersState::SHIFT,
            ),
            false,
        ),
        ("Ctrl-R", ctrl('r'), false),
        ("Escape", seam(NamedKey::Escape), false),
        ("Enter, a shell prompt", Via::Enter(Line::Shell), false),
        ("ctl key esc", Via::Control("key", "esc"), false),
    ];
    #[cfg(target_os = "macos")]
    cases.push(("tab context menu opening", Via::TabMenuOpening, true));
    if super::HARDCODED_SUPER_CHORDS {
        let cmd_c = WinitKey::Character(SmolStr::new("c"));
        cases.push((
            "physical ⌘C",
            Via::Winit(KeyCode::KeyC, cmd_c, ModifiersState::SUPER),
            true,
        ));
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
            Via::Bound(code, ch, mods) => {
                app.keybindings = crate::keybinding::Keybindings::from_config(Some(
                    &[
                        ("ctrl+shift+c".to_string(), "copy".to_string()),
                        ("ctrl+shift+l".to_string(), "select_all".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                ));
                app.windows.get_mut(&wid).unwrap().mods = mods;
                app.on_key(
                    wid,
                    aterm_window::event::KeyEvent::synthetic_for_test(
                        PhysicalKey::Code(code),
                        WinitKey::Character(SmolStr::new(ch)),
                        Some(SmolStr::new(ch)),
                        KeyLocation::Standard,
                        ElementState::Pressed,
                        false,
                    ),
                );
            }
            Via::ModifierEdge(mods) => app.on_modifiers_changed(wid, mods),
            Via::Control(verb, body) => {
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
                    "mouse" => crate::control::parse_mouse(body).ok(),
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
            Via::Menu(action) => {
                app.fence_menu_action(action);
                if action == crate::menu::MenuAction::Copy {
                    let _ = app.copy_selection();
                } else {
                    app.select_all_in(wid);
                }
            }
            Via::ActiveTabChip => {
                let tab = app.windows[&wid].tab_set.active_id().expect("a tab");
                assert!(
                    app.apply_native_tab_command(
                        wid,
                        crate::app_tabs::NativeTabCommand::Select(tab)
                    )
                );
            }
            Via::Pill => {
                let session = app.front_terminal(wid).expect("terminal").session;
                app.on_pill_wake(wid, session, crate::presence::PillHit::Go);
            }
            #[cfg(target_os = "macos")]
            Via::TabMenuOpening => app.on_tab_context_menu_opening(wid),
            Via::Pointer(gesture, tracking) => {
                if tracking {
                    let term = app.front_terminal(wid).expect("terminal").term.clone();
                    term_lock(&term).process(b"\x1b[?1003h\x1b[?1006h");
                }
                pointer_gesture(&mut app, wid, gesture);
            }
        }
        let _ = drain(pipe);
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

/// A drag across a path in a mouse-tracking app (Claude Code's transcript) keeps
/// the composer's keys lit, and so does copying the selection after it. The app
/// repaints its selection highlight above the caret and draws the caret back; had
/// the drag or the copy drawn the generic boundary (the negative control,
/// `clear_move_license`), that repaint would park the caret's row and every later
/// key typed on it would be refused `program-row`.
#[cfg(unix)]
#[test]
fn a_drag_into_a_tracking_app_keeps_the_composer_s_keys_lit() {
    use std::time::Instant;

    use aterm_effects::cursor_glow::AdmissionRecord;

    use crate::cursor_glow::CursorGlow;

    #[derive(Debug)]
    enum Between {
        Pointer(Gesture),
        /// A drag, then the menu's Copy (⌘C on macOS).
        DragThenMenuCopy,
        /// A drag, then ⌘C as `on_key` reads it.
        DragThenPhysicalCopy,
        GenericBoundary,
    }
    let geom = glow_geom();
    let tape = Tape::new();
    let mut cases = vec![
        Between::Pointer(Gesture::Drag),
        Between::Pointer(Gesture::Click),
        Between::Pointer(Gesture::Wheel),
        Between::DragThenMenuCopy,
        Between::GenericBoundary,
    ];
    if super::HARDCODED_SUPER_CHORDS {
        cases.push(Between::DragThenPhysicalCopy);
    }
    for between in cases {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_string());
        let cfg = app.glow_config();
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"\x1b[?1003h\x1b[?1006h");
        let mut out = Vec::new();
        let mut seq = 0u64;
        // One frame: the app's last print ended at `print`; its caret is drawn at `caret`.
        let mut frame =
            |app: &mut App, print: Option<(u16, u16, char)>, caret: (u16, u16), at: Instant| {
                let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
                if let Some((row, end, glyph)) = print {
                    seq += 1;
                    glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
                }
                glow.observe_caret_drawn(true);
                glow.tick(Some(caret), at, &cfg, geom, &mut out);
            };
        let key = |app: &mut App, c: char| {
            tape.step();
            assert_eq!(
                app.input(
                    wid,
                    InputEvent::Key {
                        key: Key::Character(c),
                        mods: Modifiers::empty(),
                        base_layout: None,
                        layout_text: None,
                        event_type: KeyEventType::Press,
                    },
                    Source::Human,
                ),
                crate::input::InputOutcome::Ok
            );
        };
        frame(&mut app, None, (0, 0), tape.step());
        key(&mut app, 'h');
        frame(&mut app, Some((0, 1, 'h')), (0, 1), tape.step());
        // The gesture, the copy and the boundary are stamped at the step after the echo.
        tape.step();
        match between {
            Between::Pointer(gesture) => pointer_gesture(&mut app, wid, gesture),
            Between::DragThenMenuCopy | Between::DragThenPhysicalCopy => {
                pointer_gesture(&mut app, wid, Gesture::Drag);
            }
            Between::GenericBoundary => app.clear_move_license(wid),
        }
        // The app's selection highlight over the path two rows down, the caret drawn back.
        frame(&mut app, Some((2, 10, 's')), (0, 1), tape.step());
        tape.step();
        match between {
            Between::DragThenMenuCopy => {
                app.fence_menu_action(crate::menu::MenuAction::Copy);
                let _ = app.copy_selection();
            }
            Between::DragThenPhysicalCopy => {
                use aterm_window::keyboard::{
                    Key as WinitKey, KeyLocation, ModifiersState, SmolStr,
                };

                app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SUPER;
                app.on_key(
                    wid,
                    aterm_window::event::KeyEvent::synthetic_for_test(
                        PhysicalKey::Code(KeyCode::KeyC),
                        WinitKey::Character(SmolStr::new("c")),
                        Some(SmolStr::new("c")),
                        KeyLocation::Standard,
                        ElementState::Pressed,
                        false,
                    ),
                );
                app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
            }
            Between::Pointer(_) | Between::GenericBoundary => {}
        }
        // A quiet frame: the app's last print is still the highlight off the caret's row.
        frame(&mut app, None, (0, 1), tape.step());
        let mut verdicts = Vec::new();
        for (i, c) in ['x', 'y', 'z'].into_iter().enumerate() {
            let col = 2 + i as u16;
            key(&mut app, c);
            frame(&mut app, Some((0, col, c)), (0, col), tape.step());
            verdicts.push(
                app.windows[&wid]
                    .fx
                    .cursor()
                    .glow
                    .admission_log()
                    .find(|r| r.origin == (0, col - 1) && r.target == (0, col))
                    .map(|r| (r.reason, r.licence)),
            );
        }
        let _ = drain(pipe);
        match between {
            Between::GenericBoundary => {
                assert!(
                    verdicts
                        .iter()
                        .all(|v| v
                            .is_some_and(|(reason, _)| reason == CursorGlow::DECLINE_PROGRAM_ROW)),
                    "negative control: the generic boundary parks the composer's row: {verdicts:?}"
                )
            }
            lit => {
                assert!(
                    verdicts
                        .iter()
                        .all(|v| v
                            .is_some_and(|(_, licence)| licence != AdmissionRecord::LICENCE_NONE)),
                    "{lit:?}: every key typed after it lights its echo: {verdicts:?}"
                )
            }
        }
    }
}

/// Escape, Ctrl-C or a scrollback jump in an agent's composer (Claude Code's
/// fullscreen prompt) keeps the composer's keys lit, by hand or through the control
/// socket. The agent repaints another row after it (Claude's "Esc again to clear"
/// footer) and draws the caret back; a generic boundary would let that repaint park
/// the composer's row and refuse every later key typed on it `program-row`, which is
/// what a shell in front and an editor the agent handed the terminal to still get: the
/// negative controls. In Claude's vim INSERT, at its `❯` prompt or in shell mode's `!`
/// box, the boundary still doubts the row: an Escape enters NORMAL, whose keys are
/// motions and stay dark, and a key typed while `-- INSERT --` shows (again after `i`, or
/// still after another boundary) settles the row and lights. The shell and the editor
/// show `-- INSERT --` too, and stay dark: only an agent's composer settles.
#[cfg(unix)]
#[test]
fn an_escape_or_ctrl_c_in_an_agent_s_composer_keeps_its_keys_lit() {
    use std::time::Instant;

    use aterm_effects::cursor_glow::AdmissionRecord;
    use aterm_types::keyboard::NamedKey;
    use aterm_window::keyboard::{Key as WinitKey, KeyLocation, NamedKey as WNamed, SmolStr};

    use crate::cursor_glow::CursorGlow;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Front {
        Agent,
        /// Claude with vim's `-- INSERT --` indicator under its composer.
        VimInsert,
        /// Claude in shell mode (`!`), vim's `-- INSERT --` opening the footer under its box
        /// (measured on 2.1.311: `-- INSERT -- ! for shell mode`).
        VimInsertShellMode,
        Shell,
        HandedToAnEditor,
    }
    #[derive(Clone, Copy, Debug)]
    enum Boundary {
        /// Escape as `on_key` reads it.
        PhysicalEscape,
        /// Ctrl-C as `on_key` reads it.
        PhysicalCtrlC,
        /// A control `key esc`: the fence it records, then its press through the seam.
        ControlKeyEsc,
        /// A control `key ctrl+c`, the same way.
        ControlKeyCtrlC,
        /// A guarded control `key esc` (`if=`): the control thread records its fence and writes
        /// the PTY itself, and the window applies the fence at its next frame, after the agent
        /// has drawn what the Escape did.
        GuardedControlKeyEsc,
        /// A control verb's recorded hand-off, applied by `reconcile_licence`.
        OwedFence,
        /// A scrollback jump to the live screen.
        ScrollView,
    }
    const AGENT: i32 = i32::MAX - 2;
    const SHELL: i32 = i32::MAX - 1;
    let geom = glow_geom();
    let tape = Tape::new();
    let fronts = [
        Front::Agent,
        Front::VimInsert,
        Front::VimInsertShellMode,
        Front::Shell,
        Front::HandedToAnEditor,
    ];
    let boundaries = [
        Boundary::PhysicalEscape,
        Boundary::PhysicalCtrlC,
        Boundary::ControlKeyEsc,
        Boundary::ControlKeyCtrlC,
        Boundary::GuardedControlKeyEsc,
        Boundary::OwedFence,
        Boundary::ScrollView,
    ];
    for (front, boundary) in fronts
        .into_iter()
        .flat_map(|front| boundaries.map(|boundary| (front, boundary)))
    {
        let (mut app, pipe) =
            app_observing_pty_with(matches!(boundary, Boundary::GuardedControlKeyEsc));
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_string());
        let cfg = app.glow_config();
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        // Claude's fullscreen composer frame and the footer under it, its caret in the composer.
        let rule = "─".repeat(60);
        const INSERT_FOOTER: &str = "-- INSERT -- ⏵⏵ auto mode on (shift+tab to cycle)";
        const PLAIN_FOOTER: &str = "⏵⏵ auto mode on (shift+tab to cycle)";
        const SHELL_INSERT_FOOTER: &str = "-- INSERT -- ! for shell mode";
        const SHELL_FOOTER: &str = "! for shell mode";
        let vim = matches!(front, Front::VimInsert | Front::VimInsertShellMode);
        // A shell or an editor showing `-- INSERT --` too: no agent composer, so its INSERT
        // frame makes nothing editable (`agent_composer_in_front`).
        let (footer, normal_footer) = match front {
            Front::VimInsertShellMode => (SHELL_INSERT_FOOTER, SHELL_FOOTER),
            Front::VimInsert | Front::Shell | Front::HandedToAnEditor => {
                (INSERT_FOOTER, PLAIN_FOOTER)
            }
            Front::Agent => (PLAIN_FOOTER, PLAIN_FOOTER),
        };
        let caret = if front == Front::VimInsertShellMode {
            '!'
        } else {
            '❯'
        };
        let escapes = matches!(
            boundary,
            Boundary::PhysicalEscape | Boundary::ControlKeyEsc | Boundary::GuardedControlKeyEsc
        );
        let interrupts = matches!(
            boundary,
            Boundary::PhysicalCtrlC | Boundary::ControlKeyCtrlC
        );
        term_lock(&term).process(
            format!(
                "\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[6;1H{rule}\x1b[7;1H{caret} \x1b[8;1H{rule}\
                 \x1b[9;1H  {footer}\x1b[7;3H"
            )
            .as_bytes(),
        );
        let (group, program) = match front {
            Front::Agent
            | Front::VimInsert
            | Front::VimInsertShellMode
            | Front::HandedToAnEditor => (AGENT, "claude"),
            Front::Shell => (SHELL, "zsh"),
        };
        let session = app.front_terminal(wid).expect("terminal").session;
        app.session_factory.fg_probe = |master| master;
        app.session_factory.handed_terminal = match front {
            Front::HandedToAnEditor => |_| true,
            Front::Agent | Front::VimInsert | Front::VimInsertShellMode | Front::Shell => |_| false,
        };
        app.pool.sessions.get_mut(&session).unwrap().session.master = group;
        {
            let ctx = app.pool.get(session).unwrap().ctx.clone();
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(group);
            timeline.set_program(group, Some(program.into()));
        }
        assert_eq!(
            app.front_agent_composer(wid),
            front == Front::Agent || vim,
            "{front:?}"
        );
        assert_eq!(
            app.boundary_keeps_hand(wid),
            front == Front::Agent,
            "{front:?}: only an agent's composer outside vim's INSERT keeps the hand"
        );
        let mut out = Vec::new();
        let mut seq = 0u64;
        let mut echo_row = vec![' '; geom.cols];
        // One frame: the app's last print ended at `print`; its caret is drawn at `caret`.
        let mut frame =
            |app: &mut App, print: Option<(u16, u16, char)>, caret: (u16, u16), at: Instant| {
                if let Some((0, end, glyph)) = print {
                    term_lock(&term).process(format!("\x1b[7;{}H{glyph}", end + 2).as_bytes());
                }
                let ctx = &app.pool.get(session).unwrap().ctx;
                let observed = crate::vim_custody::capture(&term_lock(&term), ctx, false);
                let editable = observed.is_some_and(|frame| ctx.licence.vim.frame(frame))
                    && app.agent_composer_in_front(wid, false);
                let entry = observed.and_then(|frame| ctx.licence.vim.entry_at(frame));
                let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
                if let Some((row, end, glyph)) = print {
                    if row == 0 && end > 0 {
                        echo_row[usize::from(end - 1)] = glyph;
                    }
                    seq += 1;
                    glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
                }
                glow.observe_caret_drawn(true);
                glow.observe_row(caret.0, caret.1, &echo_row, at);
                glow.note_composer_entry(entry);
                glow.note_composer_insert_frame(editable);
                glow.tick(Some(caret), at, &cfg, geom, &mut out);
            };
        let key = |app: &mut App, c: char| {
            tape.step();
            assert_eq!(
                app.input(
                    wid,
                    InputEvent::Key {
                        key: Key::Character(c),
                        mods: Modifiers::empty(),
                        base_layout: None,
                        layout_text: None,
                        event_type: KeyEventType::Press,
                    },
                    Source::Human,
                ),
                crate::input::InputOutcome::Ok
            );
        };
        let physical = |app: &mut App, code: KeyCode, logical: WinitKey, mods: ModifiersState| {
            tape.step();
            app.windows.get_mut(&wid).unwrap().mods = mods;
            app.on_key(
                wid,
                aterm_window::event::KeyEvent::synthetic_for_test(
                    PhysicalKey::Code(code),
                    logical,
                    None,
                    KeyLocation::Standard,
                    ElementState::Pressed,
                    false,
                ),
            );
            app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        };
        frame(&mut app, None, (0, 0), tape.step());
        key(&mut app, 'h');
        frame(&mut app, Some((0, 1, 'h')), (0, 1), tape.step());
        // A control boundary's fence and press, or a jump, are stamped at the step after the echo.
        tape.step();
        match boundary {
            Boundary::PhysicalEscape => physical(
                &mut app,
                KeyCode::Escape,
                WinitKey::Named(WNamed::Escape),
                ModifiersState::empty(),
            ),
            Boundary::PhysicalCtrlC => physical(
                &mut app,
                KeyCode::KeyC,
                WinitKey::Character(SmolStr::new("c")),
                ModifiersState::CONTROL,
            ),
            Boundary::ControlKeyEsc => {
                if let Some(keep_hand) = crate::control::control_fence(
                    aterm_types::control_verbs::input_of("key"),
                    "key",
                    "esc",
                    false,
                ) {
                    app.pool.get(session).unwrap().ctx.licence.fence(keep_hand);
                }
                let esc = crate::control::parse_key("esc").expect("a named key");
                assert!(matches!(
                    esc,
                    InputEvent::Key {
                        key: Key::Named(NamedKey::Escape),
                        ..
                    }
                ));
                assert_eq!(
                    app.input(wid, esc, Source::Controller),
                    crate::input::InputOutcome::Ok
                );
            }
            Boundary::ControlKeyCtrlC => {
                if let Some(keep_hand) = crate::control::control_fence(
                    aterm_types::control_verbs::input_of("key"),
                    "key",
                    "ctrl+c",
                    false,
                ) {
                    app.pool.get(session).unwrap().ctx.licence.fence(keep_hand);
                }
                let ctrl_c = crate::control::parse_key("ctrl+c").expect("a control key");
                assert_eq!(
                    app.input(wid, ctrl_c, Source::Controller),
                    crate::input::InputOutcome::Ok
                );
            }
            Boundary::GuardedControlKeyEsc => {
                let ctx = app.pool.get(session).unwrap().ctx.clone();
                if let Some(keep_hand) = crate::control::control_fence(
                    aterm_types::control_verbs::input_of("key"),
                    "key",
                    "esc",
                    true,
                ) {
                    ctx.licence.fence(keep_hand);
                }
                let guard = crate::control::compile_guard(Some("─")).unwrap().unwrap();
                assert_eq!(
                    crate::control::cmd_key_guarded(&term, &ctx, guard.as_ref(), "esc"),
                    "OK\n",
                    "the guard matches the composer's rule"
                );
            }
            Boundary::OwedFence => {
                app.pool.get(session).unwrap().ctx.licence.fence(false);
                app.reconcile_licence(wid);
            }
            Boundary::ScrollView => {
                let _ = app.input(
                    wid,
                    InputEvent::ScrollView(crate::input::ScrollIntent::Bottom),
                    Source::Human,
                );
            }
        }
        if vim && escapes {
            // Claude's Escape enters NORMAL: the INSERT indicator leaves the footer.
            term_lock(&term)
                .process(format!("\x1b[9;1H\x1b[2K  {normal_footer}\x1b[7;3H").as_bytes());
        }
        if vim && interrupts {
            // Claude's Ctrl-C keeps INSERT but draws its exit hint where the indicator was, and
            // keeps it over the next key's echo (measured on 2.1.311, at both prompts).
            term_lock(&term).process(b"\x1b[9;1H\x1b[2K  Press Ctrl-C again to exit\x1b[7;3H");
        }
        if matches!(boundary, Boundary::GuardedControlKeyEsc) {
            // The window applies the guarded write's fence at its next frame, after that repaint.
            app.reconcile_licence(wid);
        }
        // The agent's footer hint two rows down, the caret drawn back on the composer.
        frame(&mut app, Some((2, 10, 's')), (0, 1), tape.step());
        // A quiet frame: the app's last print is still the footer, off the caret's row.
        frame(&mut app, None, (0, 1), tape.step());
        type Frame<'f> = dyn FnMut(&mut App, Option<(u16, u16, char)>, (u16, u16), Instant) + 'f;
        let type_at = |app: &mut App, frame: &mut Frame<'_>, glyphs: &[char], col0: u16| {
            let mut verdicts = Vec::new();
            for (i, &c) in glyphs.iter().enumerate() {
                let col = col0 + i as u16;
                key(app, c);
                frame(app, Some((0, col, c)), (0, col), tape.step());
                verdicts.push(
                    app.windows[&wid]
                        .fx
                        .cursor()
                        .glow
                        .admission_log()
                        .find(|r| r.origin == (0, col - 1) && r.target == (0, col))
                        .map(|r| (r.reason, r.licence)),
                );
            }
            verdicts
        };
        let lit = |verdicts: &[Option<(&str, &str)>]| {
            verdicts
                .iter()
                .all(|v| v.is_some_and(|(_, licence)| licence != AdmissionRecord::LICENCE_NONE))
        };
        let dark = |verdicts: &[Option<(&str, &str)>]| {
            verdicts
                .iter()
                .all(|v| v.is_some_and(|(reason, _)| reason == CursorGlow::DECLINE_PROGRAM_ROW))
        };
        let verdicts = type_at(&mut app, &mut frame, &['x', 'y', 'z'], 2);
        match front {
            Front::Agent => assert!(
                lit(&verdicts),
                "{boundary:?} in Claude's composer: every key typed after it lights its echo: \
                 {verdicts:?}"
            ),
            Front::VimInsert | Front::VimInsertShellMode if !escapes => assert!(
                lit(&verdicts),
                "{boundary:?} leaves vim in INSERT ({front:?}): the keys typed after it settle \
                 the row and light: {verdicts:?}"
            ),
            Front::VimInsert | Front::VimInsertShellMode => assert!(
                dark(&verdicts),
                "{boundary:?} enters vim's NORMAL ({front:?}): its motions stay dark: \
                 {verdicts:?}"
            ),
            Front::Shell | Front::HandedToAnEditor => assert!(
                dark(&verdicts),
                "negative control, {boundary:?} with {front:?} in front: the generic boundary \
                 parks the caret's row: {verdicts:?}"
            ),
        }
        if vim && escapes {
            // `i` returns to INSERT: Claude echoes nothing for it and puts the indicator back.
            key(&mut app, 'i');
            // The first text key can arrive before Claude redraws that indicator.
            key(&mut app, 'p');
            term_lock(&term).process(format!("\x1b[9;1H\x1b[2K  {footer}\x1b[7;3H").as_bytes());
            frame(&mut app, Some((2, 12, 'T')), (0, 4), tape.step());
            frame(&mut app, None, (0, 4), tape.step());
            frame(&mut app, Some((0, 5, 'p')), (0, 5), tape.step());
            assert!(
                app.windows[&wid]
                    .fx
                    .cursor()
                    .glow
                    .admission_log()
                    .any(|r| r.target == (0, 5) && r.licence != AdmissionRecord::LICENCE_NONE),
                "first typed key before INSERT repaint must light its later echo"
            );
            let verdicts = type_at(&mut app, &mut frame, &['q'], 6);
            assert!(
                lit(&verdicts),
                "{boundary:?}, then `i`: the keys typed in INSERT again light: {verdicts:?}"
            );
        }
        let _ = drain(pipe);
    }
}

/// The vim INSERT reading waits for a terminal the PTY reader holds for a batch: the reader
/// hands it over once the UI announces itself (`ui_waiting`), and the Escape still reads
/// `-- INSERT --` and doubts the composer's row. A read that gave up on the held terminal would
/// see no indicator and keep the hand, lighting the NORMAL motions after it.
#[cfg(unix)]
#[test]
fn the_vim_insert_reading_waits_for_the_reader_s_handoff() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const AGENT: i32 = i32::MAX - 2;
    let (mut app, _pipe) = app_observing_pty();
    let wid = WindowId(0);
    let front = app.front_terminal(wid).expect("terminal").clone();
    let rule = "─".repeat(60);
    term_lock(&front.term).process(
        format!(
            "\x1b[?1049h\x1b[6;1H{rule}\x1b[7;1H❯ \x1b[8;1H{rule}\
             \x1b[9;1H  -- INSERT -- ⏵⏵ auto mode on (shift+tab to cycle)\x1b[7;3H"
        )
        .as_bytes(),
    );
    app.session_factory.fg_probe = |master| master;
    app.session_factory.handed_terminal = |_| false;
    app.pool
        .sessions
        .get_mut(&front.session)
        .unwrap()
        .session
        .master = AGENT;
    {
        let ctx = app.pool.get(front.session).unwrap().ctx.clone();
        let mut timeline = ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(AGENT);
        timeline.set_program(AGENT, Some("claude".into()));
    }
    assert!(
        !app.boundary_keeps_hand(wid),
        "PRECONDITION: an idle vim INSERT keeps the generic boundary"
    );
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let reader = {
        let (term, waiting, done) = (front.term.clone(), front.ui_waiting.clone(), done.clone());
        std::thread::spawn(move || {
            let batch = term.lock().unwrap();
            held_tx.send(()).unwrap();
            // The reader's batch ends once the UI announces itself (a one-minute hang detector).
            let start = Instant::now();
            while waiting.load(Ordering::Acquire) == 0
                && !done.load(Ordering::Acquire)
                && start.elapsed() < Duration::from_secs(60)
            {
                std::thread::yield_now();
            }
            drop(batch);
        })
    };
    held_rx.recv().unwrap();
    let keeps = app.boundary_keeps_hand(wid);
    done.store(true, Ordering::Release);
    reader.join().unwrap();
    assert!(
        !keeps,
        "an Escape while the reader holds the terminal still reads `-- INSERT --` and doubts \
         the composer's row"
    );
}

/// In Claude's vim mode, `i` and the key after it typed before Claude repaints its footer with
/// `-- INSERT --` (both keys inside one repaint) find NORMAL at their press: the `i` is only an
/// entry the session's vim custody holds, and the key is not yet editable. The headless
/// capture's frame that shows the indicator makes the composer editable before the glow judges
/// the key's echo, which lights. A key whose echo lands under no indicator stays dark; so do
/// NORMAL's motions after an `i` and an Escape typed inside one repaint, whose INSERT frame the
/// agent draws on the way back to NORMAL; and so does the same race in a shell (negative
/// control: only an agent's composer is editable). An Escape right after the INSERT echo, then a
/// pause, recalls nothing across the Escape: the row stays parked. After `i` and a Ctrl-C inside
/// one repaint, Claude stays in INSERT under its exit hint, and a key typed there lights.
///
/// Every press and frame runs at an instant the test injects. A second pass runs the same keys
/// with the wall clock ahead of that clock by more than the scenario's span, the case a loaded
/// machine makes, and must give the same verdicts: a wall-clock read on the press or frame path
/// fails it, as the licence boundary an Escape or a Ctrl-C stamped on the wall clock did.
#[cfg(unix)]
#[test]
fn a_key_typed_before_claude_shows_insert_lights_on_the_frame_that_shows_it() {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use aterm_effects::cursor_glow::AdmissionRecord;
    use aterm_types::keyboard::NamedKey;

    use crate::app_render::ComposedCursorFxClock;

    const AGENT: i32 = i32::MAX - 2;
    const SHELL: i32 = i32::MAX - 1;
    const INSERT_FOOTER: &str = "-- INSERT -- ⏵⏵ auto mode on (shift+tab to cycle)";
    const PLAIN_FOOTER: &str = "⏵⏵ auto mode on (shift+tab to cycle)";
    /// A press after the step before: keys typed in quick succession.
    const KEY: Duration = Duration::from_millis(4);
    /// A frame after the step before: a 60 Hz display.
    const FRAME: Duration = Duration::from_millis(16);
    /// The frames of a person's pause (100 ms), drawn while the trail animates.
    const PAUSE_FRAMES: usize = 6;
    /// How much later than every injected instant the second pass's wall clock reads: the
    /// trail's longest typed-key window (`TYPE_HINT_FRESH`, 250 ms).
    const WALL_AHEAD: Duration = Duration::from_millis(250);
    /// One admission: its reason, licence, origin and target.
    type Verdict = (&'static str, &'static str, (u16, u16), (u16, u16));
    for (group, program) in [(AGENT, "claude"), (SHELL, "zsh")] {
        // The scenario's span on the test's clock, which the first pass measures.
        let mut span = Duration::ZERO;
        for wall_ahead in [false, true] {
            let (mut app, pipe) = app_observing_pty();
            let wid = WindowId(0);
            app.config.cursor_trail = Some(true);
            app.config.cursor_trail_style = Some("rainbow kitty".to_string());
            let front = app.front_terminal(wid).expect("terminal").clone();
            app.session_factory.fg_probe = |master| master;
            app.session_factory.handed_terminal = |_| false;
            app.pool
                .sessions
                .get_mut(&front.session)
                .unwrap()
                .session
                .master = group;
            {
                let ctx = app.pool.get(front.session).unwrap().ctx.clone();
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(group);
                timeline.set_program(group, Some(program.into()));
            }
            // Claude's fullscreen composer in vim's INSERT, its caret on row 6 (0-based).
            let rule = "─".repeat(60);
            term_lock(&front.term).process(
                format!(
                    "\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[6;1H{rule}\x1b[7;1H❯ \x1b[8;1H{rule}\
                     \x1b[9;1H  {INSERT_FOOTER}\x1b[7;3H"
                )
                .as_bytes(),
            );
            // The test's clock, started after the setup as the trail rig's is, so every instant the
            // setup stamped is earlier than every injected one. A step pins this thread's host clock
            // (the input path's clock) to its instant until the step's call returns.
            let start = crate::metrics::instant_at_us(crate::metrics::us_at(Instant::now()) + 1)
                .expect("the host clock's anchor does not overflow");
            let clock = Cell::new(start);
            // The second pass lets the wall clock run ahead of the test's clock by more than the
            // whole scenario's span, so a wall-clock read anywhere in the press or frame path
            // lands later than every injected instant (by `WALL_AHEAD`), as on a machine too
            // loaded to keep up; its verdicts must be the first pass's.
            if wall_ahead {
                std::thread::sleep(span + WALL_AHEAD);
            }
            let program = if wall_ahead {
                format!("{program} (the wall clock ahead)")
            } else {
                program.to_owned()
            };
            let step = |by: Duration| {
                let at = clock.get() + by;
                clock.set(at);
                crate::metrics::pin_host_clock(Some(at));
                at
            };
            let frame = |app: &mut App| {
                let at = step(FRAME);
                let captured = app
                    .prepare_terminal_capture_grid_with_cursor_fx(
                        wid,
                        ComposedCursorFxClock::Advance(at),
                    )
                    .is_some();
                crate::metrics::pin_host_clock(None);
                assert!(captured, "a headless capture");
            };
            let press_with = |app: &mut App, key: Key, mods: Modifiers| {
                step(KEY);
                let outcome = app.input(
                    wid,
                    InputEvent::Key {
                        key,
                        mods,
                        base_layout: None,
                        layout_text: None,
                        event_type: KeyEventType::Press,
                    },
                    Source::Human,
                );
                crate::metrics::pin_host_clock(None);
                assert_eq!(outcome, crate::input::InputOutcome::Ok);
            };
            let press = |app: &mut App, key: Key| press_with(app, key, Modifiers::empty());
            // The program's answer to a key: `bytes` processed, then two frames.
            let answer = |app: &mut App, bytes: &str| {
                term_lock(&front.term).process(bytes.as_bytes());
                frame(app);
                frame(app);
            };
            let last_seq = |app: &App| {
                app.windows[&wid]
                    .fx
                    .cursor()
                    .glow
                    .admission_log()
                    .last()
                    .map_or(0, |r| r.seq)
            };
            let verdicts_since = |app: &App, since: u64| -> Vec<Verdict> {
                app.windows[&wid]
                    .fx
                    .cursor()
                    .glow
                    .admission_log()
                    .filter(|r| r.seq > since)
                    .map(|r| (r.reason, r.licence, r.origin, r.target))
                    .collect()
            };
            let lit = |verdicts: &[Verdict]| {
                !verdicts.is_empty()
                    && verdicts
                        .iter()
                        .all(|&(_, licence, ..)| licence != AdmissionRecord::LICENCE_NONE)
            };
            let dark = |verdicts: &[Verdict]| {
                verdicts
                    .iter()
                    .all(|&(_, licence, ..)| licence == AdmissionRecord::LICENCE_NONE)
            };
            frame(&mut app);
            for c in ['a', 'b'] {
                press(&mut app, Key::Character(c));
                answer(&mut app, &c.to_string());
            }
            // Escape enters NORMAL: the indicator leaves the footer and the caret steps back.
            press(&mut app, Key::Named(NamedKey::Escape));
            answer(
                &mut app,
                &format!("\x1b[9;1H\x1b[2K  {PLAIN_FOOTER}\x1b[7;4H"),
            );
            assert!(
                app.windows[&wid].fx.cursor().glow.caret_row_parked(),
                "{program}: the repaint after the Escape parks the doubted row"
            );
            // A key's echo under no indicator stays dark.
            let since = last_seq(&app);
            press(&mut app, Key::Character('q'));
            answer(&mut app, "q");
            let verdicts = verdicts_since(&app, since);
            assert!(
                !verdicts.is_empty() && dark(&verdicts),
                "{program}: a key whose echo lands under no indicator stays dark: {verdicts:?}"
            );
            // `i` and `x` before the repaint: the next frame shows the indicator and the echo.
            let since = last_seq(&app);
            press(&mut app, Key::Character('i'));
            press(&mut app, Key::Character('x'));
            answer(
                &mut app,
                &format!("\x1b[9;1H\x1b[2K  {INSERT_FOOTER}\x1b[7;5Hx"),
            );
            let verdicts = verdicts_since(&app, since);
            if group == AGENT {
                assert!(
                    lit(&verdicts),
                    "{program}: the key typed before INSERT showed lights on the frame that shows \
                     it: {verdicts:?}"
                );
            } else {
                assert!(
                    !verdicts.is_empty() && dark(&verdicts),
                    "{program}: no agent composer, so the INSERT frame licenses nothing: \
                     {verdicts:?}"
                );
            }
            // NORMAL again, Escape typed right after the echo: its repaint steps the caret back
            // behind the echo before the caret rested, and a pause follows. Nothing recalls the echo
            // across the Escape: the row stays parked and nothing lights.
            let since = last_seq(&app);
            press(&mut app, Key::Named(NamedKey::Escape));
            answer(
                &mut app,
                &format!("\x1b[9;1H\x1b[2K  {PLAIN_FOOTER}\x1b[7;5H"),
            );
            for _ in 0..PAUSE_FRAMES {
                frame(&mut app);
            }
            let verdicts = verdicts_since(&app, since);
            assert!(
                dark(&verdicts) && app.windows[&wid].fx.cursor().glow.caret_row_parked(),
                "{program}: a pause after an Escape typed right after the echo recalls nothing \
                 across the Escape: {verdicts:?}"
            );
            // `0` to the text's start, then `i` and an Escape before the repaint: Claude draws
            // INSERT for the `i` and NORMAL for the Escape, and NORMAL's `l` stays dark.
            press(&mut app, Key::Character('0'));
            answer(&mut app, "\x1b[7;3H");
            press(&mut app, Key::Character('i'));
            press(&mut app, Key::Named(NamedKey::Escape));
            answer(
                &mut app,
                &format!("\x1b[9;1H\x1b[2K  {INSERT_FOOTER}\x1b[7;3H"),
            );
            answer(
                &mut app,
                &format!("\x1b[9;1H\x1b[2K  {PLAIN_FOOTER}\x1b[7;3H"),
            );
            let since = last_seq(&app);
            press(&mut app, Key::Character('l'));
            answer(&mut app, "\x1b[7;4H");
            let verdicts = verdicts_since(&app, since);
            assert!(
                dark(&verdicts),
                "{program}: NORMAL's motion after `i` and an Escape stays dark: {verdicts:?}"
            );
            // `i`, an Escape and `l`, all before the repaint: the INSERT frame drawn for the `i`
            // comes after the `l` was pressed, and still licenses nothing for the motion.
            let since = last_seq(&app);
            press(&mut app, Key::Character('i'));
            press(&mut app, Key::Named(NamedKey::Escape));
            press(&mut app, Key::Character('l'));
            answer(
                &mut app,
                &format!("\x1b[9;1H\x1b[2K  {INSERT_FOOTER}\x1b[7;4H"),
            );
            answer(
                &mut app,
                &format!("\x1b[9;1H\x1b[2K  {PLAIN_FOOTER}\x1b[7;5H"),
            );
            let verdicts = verdicts_since(&app, since);
            assert!(
                dark(&verdicts),
                "{program}: `i`, Escape and `l` inside one repaint: the motion stays dark: \
                 {verdicts:?}"
            );
            // `i` and a Ctrl-C before the repaint: Claude stays in INSERT across the Ctrl-C, draws
            // INSERT for the `i` and then its exit hint over the indicator; a key typed under the
            // hint lights.
            press(&mut app, Key::Character('0'));
            answer(&mut app, "\x1b[7;3H");
            press(&mut app, Key::Character('i'));
            press_with(&mut app, Key::Character('c'), Modifiers::CTRL);
            answer(
                &mut app,
                &format!("\x1b[9;1H\x1b[2K  {INSERT_FOOTER}\x1b[7;3H"),
            );
            answer(
                &mut app,
                "\x1b[9;1H\x1b[2K  Press Ctrl-C again to exit\x1b[7;3H",
            );
            // `z` inserted at the text's start: the row's text shifts right under it.
            let since = last_seq(&app);
            press(&mut app, Key::Character('z'));
            answer(&mut app, "\x1b[7;3Hzaqx\x1b[7;4H");
            let verdicts = verdicts_since(&app, since);
            if group == AGENT {
                assert!(
                    lit(&verdicts),
                    "{program}: a key typed under Ctrl-C's hint after `i` and the Ctrl-C lights: \
                     {verdicts:?}"
                );
            } else {
                assert!(
                    !verdicts.is_empty() && dark(&verdicts),
                    "{program}: a key typed under the hint stays dark: {verdicts:?}"
                );
            }
            span = clock.get().saturating_duration_since(start);
            let _ = drain(pipe);
        }
    }
}

/// A split's frame holds every pane's cells, but vim's mode is read from the focused session's
/// own terminal: beside a neighbour Claude in vim's INSERT, drawn on the same rows, a key the
/// focused Claude echoes in NORMAL stays dark, and lights once the focused footer shows
/// `-- INSERT --` again (the control: the split's frame path can light).
#[cfg(unix)]
#[test]
fn a_split_reads_vim_s_mode_from_the_focused_pane_s_own_terminal() {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Arc;

    use aterm_effects::cursor_glow::AdmissionRecord;
    use aterm_session::sink::SinkWriter;
    use aterm_types::keyboard::NamedKey;

    use crate::app_render::ComposedCursorFxClock;
    use crate::cursor_glow::CursorGlow;

    const AGENT: i32 = i32::MAX - 2;
    let (mut app, left_pipe) = app_observing_pty();
    let tape = Tape::new();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let left = app.front_terminal(wid).expect("terminal").clone();
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let right = app.split_active_stub_tab_dir_with_sink(
        wid,
        crate::pane::SplitDir::Vertical,
        Arc::new(SinkWriter::new_owned(writer)),
    );
    let front = app.front_terminal(wid).expect("terminal").clone();
    assert_eq!(
        front.session, right,
        "the fresh split focuses the right pane"
    );
    app.session_factory.fg_probe = |master| master;
    app.session_factory.handed_terminal = |_| false;
    for session in [left.session, right] {
        app.pool.sessions.get_mut(&session).unwrap().session.master = AGENT;
        let ctx = app.pool.get(session).unwrap().ctx.clone();
        let mut timeline = ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(AGENT);
        timeline.set_program(AGENT, Some("claude".into()));
    }
    // Two Claudes side by side, each a fullscreen composer in vim's INSERT on the same rows.
    let rule = "─".repeat(30);
    for session in [left.session, right] {
        let term = app.pool.get(session).unwrap().term.clone();
        term_lock(&term).process(
            format!(
                "\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[6;1H{rule}\x1b[7;1H❯ \x1b[8;1H{rule}\
                 \x1b[9;1H  -- INSERT --\x1b[7;3H"
            )
            .as_bytes(),
        );
    }
    let frame = |app: &mut App| {
        assert!(
            app.prepare_terminal_capture_grid_with_cursor_fx(
                wid,
                ComposedCursorFxClock::Advance(tape.step())
            )
            .is_some(),
            "a headless capture"
        );
    };
    let press = |app: &mut App, key: Key| {
        tape.step();
        assert_eq!(
            app.input(
                wid,
                InputEvent::Key {
                    key,
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human,
            ),
            crate::input::InputOutcome::Ok
        );
    };
    let answer = |app: &mut App, bytes: &str| {
        term_lock(&front.term).process(bytes.as_bytes());
        frame(app);
        frame(app);
    };
    let typed = |app: &mut App, c: char| {
        let since = app.windows[&wid]
            .fx
            .cursor()
            .glow
            .admission_log()
            .last()
            .map_or(0, |r| r.seq);
        press(app, Key::Character(c));
        answer(app, &c.to_string());
        let verdicts: Vec<_> = app.windows[&wid]
            .fx
            .cursor()
            .glow
            .admission_log()
            .filter(|r| r.seq > since)
            .map(|r| (r.reason, r.licence))
            .collect();
        verdicts
    };
    frame(&mut app);
    for c in ['a', 'b', 'c'] {
        let _ = typed(&mut app, c);
    }
    // The focused Claude's Escape enters NORMAL; its neighbour stays in INSERT.
    press(&mut app, Key::Named(NamedKey::Escape));
    answer(&mut app, "\x1b[9;1H\x1b[2K  ? for shortcuts\x1b[7;5H");
    assert!(
        app.windows[&wid].fx.cursor().glow.caret_row_parked(),
        "the repaint after the Escape parks the doubted row"
    );
    let footer = app.windows[&wid]
        .input_scratch
        .cells
        .iter()
        .map(|line| {
            line.iter()
                .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                .collect::<String>()
        })
        .find(|row| row.contains("? for shortcuts"))
        .expect("the focused footer is in the frame's cells");
    assert!(
        footer.trim_start().starts_with("-- INSERT --"),
        "PRECONDITION: the frame's row holds the neighbour's indicator before the focused \
         footer: {footer:?}"
    );
    // An echo at the caret in the focused NORMAL: only its own footer says it is no insert.
    let verdicts = typed(&mut app, 'x');
    assert!(
        !verdicts.is_empty()
            && verdicts
                .iter()
                .all(|&(reason, _)| reason == CursorGlow::DECLINE_PROGRAM_ROW),
        "a key echoed in the focused NORMAL stays dark beside a neighbour in INSERT: \
         {verdicts:?}"
    );
    // `i` puts the focused footer back in INSERT: the same echo lights.
    press(&mut app, Key::Character('i'));
    answer(&mut app, "\x1b[9;1H\x1b[2K  -- INSERT --\x1b[7;6H");
    let verdicts = typed(&mut app, 'y');
    assert!(
        !verdicts.is_empty()
            && verdicts
                .iter()
                .all(|&(_, licence)| licence != AdmissionRecord::LICENCE_NONE),
        "control: the focused footer in INSERT lights the echo: {verdicts:?}"
    );
    let _ = drain(left_pipe);
    let _ = drain(pipe[0]);
}

/// The insert arm is Rainbow Kitty's alone: a bare Tab and a bare ⌃V arm
/// the delivered-insert licence under Rainbow Kitty and no other style,
/// the same gate the paste's delivery edge applies.
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
/// (`CSI 57447;1:3u`), as does a lone Shift's.
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

#[test]
fn select_all_selects_the_visible_screen_without_snapping() {
    use aterm_core::selection::SelectionSide;

    let mut failures = Vec::new();
    for (label, split) in [
        ("single terminal", None),
        ("top/bottom", Some(crate::pane::SplitDir::Horizontal)),
        ("left/right", Some(crate::pane::SplitDir::Vertical)),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sibling = split.map(|dir| {
            let sibling = app
                .front_terminal(wid)
                .expect("original terminal")
                .term
                .clone();
            app.split_active_stub_tab_dir(wid, dir);
            sibling
        });
        let sibling_before = sibling.as_ref().map(|sibling| {
            let mut term = term_lock(sibling);
            term.process(b"SIBLING");
            let selection = term.text_selection_mut();
            selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, 6, SelectionSide::Right);
            selection.complete_selection();
            (
                term.text_selection().clone(),
                (0..usize::from(term.grid().rows()))
                    .map(|row| term.row_text(row))
                    .collect::<Vec<_>>(),
                term.grid().display_offset(),
            )
        });
        let terminal = app
            .front_terminal(wid)
            .expect("focused terminal")
            .term
            .clone();
        let (off, rows, cols) = {
            let mut term = term_lock(&terminal);
            let text = (0..128)
                .map(|row| format!("line-{row:03}\r\n"))
                .collect::<String>();
            term.process(text.as_bytes());
            term.scroll_to_top();
            let off = term.grid().display_offset() as i32;
            assert_ne!(off, 0, "viewport is in history");
            (off, term.grid().rows(), term.grid().cols())
        };
        match split {
            Some(crate::pane::SplitDir::Horizontal) => {
                assert!(rows < app.windows[&wid].rows, "shorter pane precondition");
            }
            Some(crate::pane::SplitDir::Vertical) => {
                assert!(cols < app.windows[&wid].cols, "narrower pane precondition");
            }
            None => assert_eq!(rows, app.windows[&wid].rows),
        }
        let expected = (0..rows)
            .map(|row| format!("line-{row:03}"))
            .collect::<Vec<_>>()
            .join("\n");

        app.select_all();

        {
            let term = term_lock(&terminal);
            assert_eq!(
                term.grid().display_offset() as i32,
                off,
                "Select All must not move the viewport"
            );
            let selection = term.text_selection();
            assert!(selection.has_selection(), "a selection was made");
            assert_eq!(
                selection.start().row,
                -off,
                "the selection starts at the TOP VISIBLE row, not the live top"
            );
            if split.is_none() {
                assert_eq!(
                    selection.end().row,
                    i32::from(rows) - 1 - off,
                    "…and ends at the bottom visible row"
                );
            }
            let bounds = (
                selection.start().row,
                selection.start().col,
                selection.end().row,
                selection.end().col,
            );
            let expected_bounds = (-off, 0, i32::from(rows) - 1 - off, cols - 1);
            let copied = term.selection_to_string();
            if bounds != expected_bounds || copied.as_deref() != Some(expected.as_str()) {
                failures.push(format!(
                    "{label}: bounds={bounds:?}, expected_bounds={expected_bounds:?}, copied={copied:?}, expected={expected:?}"
                ));
            }
        }
        if let (Some(sibling), Some(before)) = (sibling.as_ref(), sibling_before) {
            let term = term_lock(sibling);
            assert_eq!(
                (
                    term.text_selection().clone(),
                    (0..usize::from(term.grid().rows()))
                        .map(|row| term.row_text(row))
                        .collect::<Vec<_>>(),
                    term.grid().display_offset(),
                ),
                before,
                "Select All leaves sibling selection, contents and viewport unchanged"
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn cmd_arrows_are_the_deliberate_return_to_live() {
    use crate::input::ScrollIntent;
    use aterm_window::keyboard::NamedKey as WNamed;
    let down = modifier_event(WNamed::ArrowDown, KeyCode::ArrowDown, ElementState::Pressed);
    let up = modifier_event(WNamed::ArrowUp, KeyCode::ArrowUp, ElementState::Pressed);

    if super::HARDCODED_SUPER_CHORDS {
        assert!(matches!(
            super::scrollback_chord(ModifiersState::SUPER, &up),
            Some(ScrollIntent::Top)
        ));
    } else {
        assert!(super::scrollback_chord(ModifiersState::SUPER, &down).is_none());
        assert!(super::scrollback_chord(ModifiersState::SUPER, &up).is_none());
    }
    assert!(
        super::scrollback_chord(ModifiersState::SUPER | ModifiersState::ALT, &down).is_none(),
        "cmd+alt+arrow belongs to pane focus"
    );
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
    assert!(super::scrollback_chord(ModifiersState::empty(), &down).is_none());
}

/// Selection custody: a bare Cmd chord no shortcut claims is swallowed
/// (terminals never forward Cmd combos) and writes nothing, so it does not move
/// the viewport; its window half still drops the banked glide residual.
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
    publish_release_relevance(0, false);
    assert_eq!(sampled_release_relevance(0), Some(false));
}

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

        let _ = scrolled_back_with_a_selection(&mut app, wid);
        let offset = term_lock(&term).grid().display_offset();

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

#[test]
fn a_bare_modifier_or_lock_key_is_inert() {
    use aterm_window::keyboard::NamedKey as WNamed;
    fn park(app: &mut App, wid: WindowId, deadline: std::time::Instant) -> std::time::Instant {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.blink_phase = false;
        ws.next_blink = Some(deadline);
        ws.scroll_frac_px = 7;
        let now = std::time::Instant::now();
        ws.fx.cursor_mut().glow.note_motion(now);
        ws.fx.cursor_mut().trail.note_navigation(now);
        now
    }
    fn licensed(app: &App, wid: WindowId, at: std::time::Instant) -> (bool, bool) {
        let cursor = app.windows[&wid].fx.cursor();
        (
            cursor.glow.move_licensed(at),
            cursor.trail.move_licensed(at),
        )
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = scrolled_back_with_a_selection(&mut app, wid);
    let offset_before = term_lock(&term).grid().display_offset();
    let assert_inert = |app: &App, parked: std::time::Instant, what: &str| {
        let ws = &app.windows[&wid];
        assert!(!ws.blink_phase, "{what} must not snap the cursor solid");
        assert_eq!(
            ws.next_blink,
            Some(deadline),
            "{what} must not restart the blink period"
        );
        assert_eq!(ws.scroll_frac_px, 7, "{what} must not cancel the glide");
        assert_eq!(
            licensed(app, wid, parked),
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
        let parked = park(&mut app, wid, deadline);
        app.on_key(wid, modifier_event(named, code, ElementState::Pressed));
        app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::ALT);
        app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());
        assert_inert(&app, parked, &format!("bare {named:?}"));
    }

    let parked = park(&mut app, wid, deadline);
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
    assert_inert(&app, parked, "a seam-routed bare Shift");

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

    for (named, code) in [
        (WNamed::Escape, KeyCode::Escape),
        (WNamed::Backspace, KeyCode::Backspace),
    ] {
        let parked = park(&mut app, wid, deadline);
        app.on_key(wid, modifier_event(named, code, ElementState::Pressed));
        assert_eq!(
            licensed(&app, wid, parked),
            (false, false),
            "{named:?} revokes it"
        );
    }

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
