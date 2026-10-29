// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A control client never reaches the person's clipboard (`selection.copy` and `selection.paste`
//! ask for `ActionAuthority::Clipboard`, which `controller_reaches` refuses). The person copies
//! passwords: a client that could paste the clipboard into a terminal it reads, or copy what it
//! put on screen onto it, could read the one or plant the other. Each test drives a control
//! client's path the way its verb does (`invoke` and `pointer` under
//! [`App::with_gesture_source`], `key` and `mouse` through [`App::input`] as
//! [`Source::Controller`]) and the person's own path beside it, which must keep working. The
//! clipboard is the test build's board (`clipboard::PBPASTE_STUB`); no test touches the system's.

use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use aterm_session::sink::SinkWriter;

use crate::app_take_over::CLIPBOARD_REFUSED;
use crate::input::Source;
use crate::menu::MenuAction;
use crate::{App, WindowId};

const SECRET: &str = "s3cret-pass";
const WID: WindowId = WindowId(0);

/// A headless App whose front session's PTY input the returned stream reads.
fn app_reading_pty() -> (App, UnixStream, Arc<SinkWriter>) {
    let (reader, writer) = UnixStream::pair().expect("an input observer");
    reader.set_nonblocking(true).expect("nonblocking");
    let sink = Arc::new(SinkWriter::new_owned(writer.into()));
    (App::headless_for_test_with_sink(sink.clone()), reader, sink)
}

/// Everything the program has been sent so far, once the sink's ordered egress drained.
fn program_read(reader: &mut UnixStream, sink: &SinkWriter) -> String {
    super::paste_order::await_drain_for_test(sink);
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("reading the program's input: {e}"),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn set_board(text: Option<&str>) {
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = text.map(str::to_owned));
}

fn board() -> Option<String> {
    crate::control::PBPASTE_STUB.with(|board| board.borrow().clone())
}

/// Edit ▸ Copy or Paste as [`App::dispatch_menu_action`] runs it (fence, the rename editor's
/// claim, then the arm), which `invoke Copy|Paste` reaches under [`App::invoke_attributed`]'s
/// controller mark; `controller` false is the person's menu.
fn edit_menu(app: &mut App, action: MenuAction, controller: bool) {
    app.pending_action_refusal = None;
    app.with_gesture_source(controller, |app| edit_menu_body(app, action));
}

/// The arm of [`App::dispatch_menu_action`] Edit ▸ Copy and Paste run, under whatever mark the
/// caller set.
fn edit_menu_body(app: &mut App, action: MenuAction) {
    app.fence_menu_action(action);
    if app.divert_menu_action_around_rename(action) {
        return;
    }
    match action {
        MenuAction::Copy => {
            let _ = app.copy_selection();
        }
        MenuAction::Paste => app.paste_into_front_field(),
        other => panic!("not an Edit action: {other:?}"),
    }
}

/// The person's physical key, through the shipping `on_key`.
fn physical_cmd(app: &mut App, ch: &str) {
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr};
    let code = match ch {
        "c" => KeyCode::KeyC,
        "v" => KeyCode::KeyV,
        other => panic!("no key code for {other}"),
    };
    app.windows.get_mut(&WID).unwrap().mods = ModifiersState::SUPER;
    app.on_key(
        WID,
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(code),
            Key::Character(SmolStr::new(ch)),
            Some(SmolStr::new(ch)),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        ),
    );
    app.windows.get_mut(&WID).unwrap().mods = ModifiersState::empty();
}

/// Select the front terminal's top line, as a person's triple-click would.
fn select_top_line(app: &mut App) {
    let term = app
        .front_terminal(WID)
        .expect("a front terminal")
        .term
        .clone();
    let _ = crate::control::select_line(&mut crate::term_lock(&term), 0);
}

/// A CONTROL CLIENT'S `invoke Paste` SENDS A TERMINAL NO CLIPBOARD: Edit ▸ Paste under the
/// controller mark writes nothing to the front session's PTY and answers the `invoke` with
/// [`CLIPBOARD_REFUSED`]; the person's Edit ▸ Paste writes the clipboard to it.
/// NEGATIVE CONTROL: against main's code at this leaf (`64ab1a704f`), the controller's paste wrote
/// `s3cret-pass` to the PTY (`paste_clipboard_into` had no check).
#[test]
fn a_controllers_invoked_paste_sends_a_terminal_no_clipboard() {
    let (mut app, mut reader, sink) = app_reading_pty();
    set_board(Some(SECRET));
    let _ = program_read(&mut reader, &sink);

    edit_menu(&mut app, MenuAction::Paste, true);
    let sent = program_read(&mut reader, &sink);
    assert!(
        !sent.contains(SECRET),
        "the controller's paste sent {sent:?}"
    );
    assert_eq!(
        app.pending_action_refusal.as_deref(),
        Some(CLIPBOARD_REFUSED),
        "the invoke answers why"
    );
    assert!(!app.person_gesture_refused(), "the mark ends with the act");

    edit_menu(&mut app, MenuAction::Paste, false);
    let sent = program_read(&mut reader, &sink);
    assert!(sent.contains(SECRET), "the person's paste sent {sent:?}");
    assert_eq!(app.pending_action_refusal, None);
    set_board(None);
}

/// A CONTROL CLIENT'S ⌘V KEY SENDS A TERMINAL NO CLIPBOARD: `key cmd+v` reaches the session as a
/// key (the seam never runs the person's hard-wired ⌘V or a Paste binding), so the program gets
/// whatever that key encodes and no clipboard text; the person's physical ⌘V pastes it.
/// NEGATIVE CONTROL: none on main: this path never read the clipboard (the test pins that it stays
/// closed; it fails with `paste_clipboard_into` called for a `SUPER`+`v` key in
/// `input_to_session_inner`, the mutant it was run against).
#[test]
fn a_controllers_cmd_v_key_sends_a_terminal_no_clipboard() {
    let (mut app, mut reader, sink) = app_reading_pty();
    set_board(Some(SECRET));
    let _ = program_read(&mut reader, &sink);

    let cmd_v = crate::control::parse_key("cmd+v").expect("a key");
    let _ = app.input(WID, cmd_v, Source::Controller);
    let sent = program_read(&mut reader, &sink);
    assert!(!sent.contains(SECRET), "the controller's ⌘V sent {sent:?}");

    if super::HARDCODED_SUPER_CHORDS {
        physical_cmd(&mut app, "v");
        let sent = program_read(&mut reader, &sink);
        assert!(sent.contains(SECRET), "the person's ⌘V sent {sent:?}");
    }
    set_board(None);
}

/// A CONTROL CLIENT'S `invoke Paste` PUTS NO CLIPBOARD IN THE RENAME OR FIND FIELD: with the
/// in-grid rename field open, and then with the find field open, Edit ▸ Paste under the
/// controller mark leaves the field as it was (a client reads both back: the tab's title, `find
/// status`'s query); the person's Edit ▸ Paste inserts the clipboard into each.
/// NEGATIVE CONTROL: against main's code at this leaf (`64ab1a704f`), the controller's paste put
/// `s3cret-pass` in the rename field (`rename_paste_in`) and in the find query (`search_paste_in`).
#[test]
fn a_controllers_invoked_paste_fills_no_rename_or_find_field() {
    set_board(Some(SECRET));
    let mut app = App::headless_for_test();
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(WID));
    edit_menu(&mut app, MenuAction::Paste, true);
    let field = |app: &App| app.inline_rename_edit(WID).map(|edit| edit.text.clone());
    assert_eq!(field(&app).as_deref(), Some(""), "the controller's paste");
    assert_eq!(
        app.pending_action_refusal.as_deref(),
        Some(CLIPBOARD_REFUSED)
    );
    edit_menu(&mut app, MenuAction::Paste, false);
    assert_eq!(field(&app).as_deref(), Some(SECRET), "the person's paste");

    let mut app = App::headless_for_test();
    app.search_enter();
    let query = |app: &App| {
        app.windows[&WID]
            .search
            .as_ref()
            .map(|search| search.query.clone())
    };
    edit_menu(&mut app, MenuAction::Paste, true);
    assert_eq!(query(&app).as_deref(), Some(""), "the controller's paste");
    assert_eq!(
        app.pending_action_refusal.as_deref(),
        Some(CLIPBOARD_REFUSED)
    );
    edit_menu(&mut app, MenuAction::Paste, false);
    assert_eq!(query(&app).as_deref(), Some(SECRET), "the person's paste");
    set_board(None);
}

/// A PALETTE ROW A CONTROL CLIENT CHOSE RUNS AS ITS: the palette posts its Edit rows to the event
/// loop, so the origin it posts carries the chooser's mark ([`App::dispatch_marked_menu_action`]
/// runs the row under it), and the row is not cancelled for the mark having ended by then. The
/// person's row carries none.
/// NEGATIVE CONTROL: against main's code at this leaf (`64ab1a704f`), the origin carried no mark,
/// so a control client's palette Paste ran unmarked at the wake and pasted the clipboard as the
/// person's.
#[test]
fn a_palette_row_a_controller_chose_keeps_its_mark_across_the_post() {
    let mut app = App::headless_for_test();
    let origin = app.with_gesture_source(true, |app| app.palette_menu_origin(WID));
    let origin = origin.expect("a palette origin");
    assert!(origin.controller(), "the chooser's mark rides the post");
    assert_eq!(
        app.menu_action_for_origin(MenuAction::Paste, Some(origin)),
        Some(MenuAction::Paste),
        "the row still runs once the mark ended"
    );
    let person = app.palette_menu_origin(WID).expect("a palette origin");
    assert!(!person.controller(), "the person's row carries no mark");
}

/// A CONTROL CLIENT'S `invoke Copy` AND ⌘C KEY LEAVE THE CLIPBOARD ALONE: with a terminal line
/// selected, Edit ▸ Copy under the controller mark, and `key cmd+c` through the seam, keep the
/// clipboard's text, and the `invoke` answers why; the person's Edit ▸ Copy and physical ⌘C copy
/// the selection.
/// NEGATIVE CONTROL: against main's code at this leaf (`64ab1a704f`), the controller's Edit ▸ Copy
/// wrote the selected line to the clipboard (`copy_in` refused a control client only on a native
/// view); its ⌘C key never did (the seam sends the key to the session).
#[test]
fn a_controllers_copy_leaves_the_clipboard_alone() {
    let mut app = App::headless_for_test();
    let term = app
        .front_terminal(WID)
        .expect("a front terminal")
        .term
        .clone();
    crate::term_lock(&term).process(b"on-screen words\r\n");
    select_top_line(&mut app);
    let selected = app.window_selection_text(WID).expect("a selection");
    set_board(Some("before"));

    edit_menu(&mut app, MenuAction::Copy, true);
    assert_eq!(board().as_deref(), Some("before"), "the controller's Copy");
    assert_eq!(
        app.pending_action_refusal.as_deref(),
        Some(CLIPBOARD_REFUSED)
    );
    let cmd_c = crate::control::parse_key("cmd+c").expect("a key");
    let _ = app.input(WID, cmd_c, Source::Controller);
    assert_eq!(board().as_deref(), Some("before"), "the controller's ⌘C");

    select_top_line(&mut app);
    edit_menu(&mut app, MenuAction::Copy, false);
    assert_eq!(board(), Some(selected.clone()), "the person's Copy");
    if super::HARDCODED_SUPER_CHORDS {
        set_board(Some("before"));
        physical_cmd(&mut app, "c");
        assert_eq!(board(), Some(selected), "the person's ⌘C");
    }
    set_board(None);
}

/// A CONTROL CLIENT'S DOUBLE-CLICK COPIES NOTHING ON SELECT: with `copy_on_select` on, an Owner
/// client's `mouse press left … count=2` then `mouse release` (the Owner scope's events carry no
/// copy suppression), and a `pointer click` twice, select the word but keep the clipboard's text;
/// the same gestures as the person's copy the word.
/// NEGATIVE CONTROL: against main's code at this leaf (`64ab1a704f`), both wrote the word to the
/// clipboard (copy-on-select's `copy_selection_in` had no check).
#[test]
fn a_controllers_double_click_copies_nothing_on_select() {
    use crate::app_mouse::PointerAction;
    let mut wrong = Vec::new();
    for (how, controller) in [
        ("mouse", true),
        ("mouse", false),
        ("pointer", true),
        ("pointer", false),
    ] {
        let mut app = App::headless_for_test();
        app.copy_on_select = true;
        let term = app
            .front_terminal(WID)
            .expect("a front terminal")
            .term
            .clone();
        crate::term_lock(&term).process(b"secretword other\r\n");
        set_board(Some("before"));
        match how {
            "mouse" => {
                let src = if controller {
                    Source::Controller
                } else {
                    Source::Human
                };
                for body in ["press left 0 2 count=2", "release left 0 2 count=2"] {
                    let ev = crate::control::parse_mouse(body).expect("a mouse event");
                    let _ = app.input(WID, ev, src);
                }
            }
            _ => app.with_gesture_source(controller, |app| {
                app.pointer_cmd(PointerAction::Move { row: 0, col: 2 })
                    .expect("the pointer moves");
                for _ in 0..2 {
                    app.pointer_cmd(PointerAction::Click).expect("a click");
                }
            }),
        }
        assert_eq!(
            app.window_selection_text(WID).as_deref(),
            Some("secretword"),
            "PRECONDITION ({how}, controller={controller}): the double-click selected the word"
        );
        let want = if controller { "before" } else { "secretword" };
        if board().as_deref() != Some(want) {
            wrong.push((how, controller, board()));
        }
    }
    set_board(None);
    assert!(
        wrong.is_empty(),
        "(gesture, controller, clipboard): {wrong:?}"
    );
}

/// A CONTROL CLIENT'S TAB-MENU COPY ROWS LEAVE THE CLIPBOARD ALONE: Copy Session ID and Copy
/// Working Directory under the controller mark (the in-grid tab menu's rows, `invoke
/// CopySessionId|CopyCwd`) keep the clipboard's text and answer why; the person's Copy Session
/// ID copies the sid.
/// NEGATIVE CONTROL: against main's code at this leaf (`64ab1a704f`), the controller's row wrote
/// the sid to the clipboard.
#[test]
fn a_controllers_tab_menu_copy_rows_leave_the_clipboard_alone() {
    let mut app = App::headless_for_test();
    let tab = app.windows[&WID].tab_set.active().expect("a tab");
    let (tab, view) = (tab.id, tab.focus);
    let origin = crate::session_chrome::TerminalMenuOrigin { view, session: 0 };
    let sid = app.session_id_text(0).expect("the front session's sid");
    set_board(Some("before"));
    for action in [MenuAction::CopySessionId, MenuAction::CopyCwd] {
        app.pending_action_refusal = None;
        app.with_gesture_source(true, |app| {
            app.dispatch_tab_menu_session_action(WID, tab, origin, action);
        });
        assert_eq!(
            board().as_deref(),
            Some("before"),
            "the controller's {action:?}"
        );
        assert_eq!(
            app.pending_action_refusal.as_deref(),
            Some(CLIPBOARD_REFUSED)
        );
    }
    app.dispatch_tab_menu_session_action(WID, tab, origin, MenuAction::CopySessionId);
    assert_eq!(board(), Some(sid), "the person's Copy Session ID");
    set_board(None);
}

/// A CONTROL CLIENT'S `act` ON A NATIVE COPY BUTTON COPIES NOTHING: About's Copy Build
/// Information through `act` keeps the clipboard's text and the view says why; the person's
/// press copies the build information.
/// NEGATIVE CONTROL: against main's code at this leaf (`64ab1a704f`), the `act` wrote the build
/// information to the clipboard (`AppEffect::Clipboard` ran for any gesture).
#[test]
fn a_controllers_act_on_a_native_copy_button_copies_nothing() {
    use crate::native_app::ActionInvocation;
    use crate::native_ui::{ActionId, UiKey};
    const COPY: &str = "about/copy-build-info";
    let mut app = App::headless_for_test();
    assert!(app.open_about_tab_in_window(WID));
    let (_, view) = app.active_native_view(WID).expect("About in front");
    set_board(Some("before"));
    app.act_app(crate::app_control::ActRequest {
        view,
        ui_key: COPY.to_string(),
        action: COPY.to_string(),
        value: None,
    })
    .expect("the act is handled");
    assert_eq!(board().as_deref(), Some("before"), "the controller's act");
    assert!(!app.person_gesture_refused(), "the mark ends with the act");

    app.dispatch_native_control_action(
        WID,
        view,
        UiKey::new(COPY),
        ActionInvocation {
            id: ActionId::new(COPY.to_string()),
            value: None,
        },
        true,
    )
    .expect("the press is handled");
    assert_eq!(
        board(),
        Some(crate::about::provenance_text()),
        "the person's press"
    );
    set_board(None);
}

/// The person's pointer over the front terminal's cell (`row`, `col`), as the window reports it.
fn person_pointer_at(app: &mut App, row: u16, col: u16) {
    let (cw, ch) = app.win_cell_size(WID);
    let strip = f64::from(app.tab_strip_rows) * ch as f64;
    let x = app.win_pad(WID) as f64 + cw as f64 * (f64::from(col) + 0.5);
    let y = (app.win_pad_top(WID) + app.win_head(WID)) as f64
        + strip
        + ch as f64 * (f64::from(row) + 0.5);
    app.on_cursor_moved(WID, x, y);
}

/// A SELECTION A CONTROL CLIENT STARTED OR STEERED COPIES NOTHING ON THE PERSON'S RELEASE: with
/// `copy_on_select` on, a client's `mouse press` and `mouse move`, an Owner client's double-click
/// press, and a fleet member's (whose events carry the edge scope's copy suppression), each left
/// unreleased, and a person's press a client's `mouse move` then drags, select the text, and the
/// person's next left release (one no grid press of theirs started, as after a ⌘-click on a
/// link, or the release of the press the client dragged) keeps the clipboard's text; the person's
/// own press, drag and release copy the selection.
/// NEGATIVE CONTROL: against main's code at this leaf (`4d738a3c55`), every client case wrote the
/// selection to the clipboard (`finish_selection` asked only whether the release was a client's).
#[test]
fn a_selection_a_controller_started_or_steered_copies_nothing_on_the_persons_release() {
    use aterm_window::event::{ElementState, MouseButton};
    let controller = |app: &mut App, body: &str, fleet: bool| {
        let mut ev = crate::control::parse_mouse(body).expect("a mouse event");
        if fleet
            && let crate::input::InputEvent::MouseButton {
                suppress_copy_on_select,
                ..
            } = &mut ev
        {
            *suppress_copy_on_select = true;
        }
        let _ = app.input(WID, ev, Source::Controller);
    };
    let mut wrong = Vec::new();
    for case in [
        "client press, move",
        "client double-click press",
        "fleet double-click press",
        "person press, client move",
        "person press, move",
    ] {
        let mut app = App::headless_for_test();
        app.copy_on_select = true;
        let term = app
            .front_terminal(WID)
            .expect("a front terminal")
            .term
            .clone();
        crate::term_lock(&term).process(b"plantedtext other\r\n");
        set_board(Some("before"));
        match case {
            "client press, move" => {
                person_pointer_at(&mut app, 0, 11);
                controller(&mut app, "press left 0 0", false);
                controller(&mut app, "move left 0 11", false);
            }
            "client double-click press" => {
                person_pointer_at(&mut app, 0, 11);
                controller(&mut app, "press left 0 2 count=2", false);
            }
            "fleet double-click press" => {
                person_pointer_at(&mut app, 0, 11);
                controller(&mut app, "press left 0 2 count=2", true);
            }
            "person press, client move" => {
                person_pointer_at(&mut app, 0, 0);
                app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
                controller(&mut app, "move left 0 11", false);
            }
            _ => {
                person_pointer_at(&mut app, 0, 0);
                app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
                person_pointer_at(&mut app, 0, 11);
            }
        }
        app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
        let selected = app
            .window_selection_text(WID)
            .filter(|text| !text.is_empty());
        assert!(
            !app.windows[&WID].selecting && selected.is_some(),
            "PRECONDITION ({case}): the person's release finished a selection: {selected:?}"
        );
        let want = if case == "person press, move" {
            selected
        } else {
            Some("before".to_string())
        };
        if board() != want {
            wrong.push((case, board()));
        }
    }
    set_board(None);
    assert!(wrong.is_empty(), "(case, clipboard): {wrong:?}");
}

/// A key event as `-[NSApp currentEvent]` names it while it is dispatched, with a stamp no other
/// key in this thread has: one an `hwkey` burst posted (`posted`, noted in its ledger first, as
/// `hwkey::post` notes it) or the person's.
fn current_key(keycode: u16, posted: bool) -> crate::hwkey::PostedKey {
    thread_local! {
        static STAMPS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let stamp = STAMPS.with(|stamps| {
        stamps.set(stamps.get() + 1);
        stamps.get()
    });
    let key = crate::hwkey::PostedKey {
        kind: crate::hwkey::NS_EVENT_TYPE_KEY_DOWN,
        keycode,
        timestamp: 1000.0 + f64::from(stamp),
    };
    if posted {
        crate::hwkey::note_posted(&[key]).expect("room");
    }
    key
}

/// Run `f` while `key` is the event AppKit is dispatching (the test build's stand-in).
fn dispatching<R>(key: crate::hwkey::PostedKey, f: impl FnOnce() -> R) -> R {
    crate::hwkey::CURRENT_KEY_STUB.with(|current| current.set(Some(key)));
    let out = f();
    crate::hwkey::CURRENT_KEY_STUB.with(|current| current.set(None));
    out
}

/// One press of `ch` with `mods` held through the winit key arm's dispatch
/// ([`App::dispatch_hw_key`], the path a physical key and an `hwkey` post both take): an `hwkey`
/// burst's (`posted`) or the person's, told apart as the event arrives
/// (`crate::arriving_key_posted`).
fn hw_key(app: &mut App, ch: &str, mods: aterm_window::keyboard::ModifiersState, posted: bool) {
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{
        Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
    };
    let (code, keycode, key) = match ch {
        "c" => (KeyCode::KeyC, 8, Key::Character(SmolStr::new(ch))),
        "v" => (KeyCode::KeyV, 9, Key::Character(SmolStr::new(ch))),
        "x" => (KeyCode::KeyX, 7, Key::Character(SmolStr::new(ch))),
        "\r" => (KeyCode::Enter, 0x24, Key::Named(NamedKey::Enter)),
        "\u{1b}" => (KeyCode::Escape, 0x35, Key::Named(NamedKey::Escape)),
        other => panic!("no key code for {other:?}"),
    };
    app.windows.get_mut(&WID).unwrap().mods = mods;
    let event = KeyEvent::synthetic_for_test(
        PhysicalKey::Code(code),
        key,
        Some(SmolStr::new(ch)),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    let arrived = aterm_window::event::WindowEvent::KeyboardInput {
        device_id: aterm_window::event::DeviceId::dummy(),
        event: event.clone(),
        is_synthetic: false,
    };
    dispatching(current_key(keycode, posted), || {
        let posted = crate::arriving_key_posted(&arrived);
        app.dispatch_hw_key(WID, event, posted);
    });
    app.windows.get_mut(&WID).unwrap().mods = ModifiersState::empty();
}

/// A KEY `hwkey` POSTED REACHES NO CLIPBOARD BY THE PERSON'S KEY PATH: `hwkey v mods=cmd` and
/// `hwkey c mods=cmd` post real key events, which arrive by the winit key arm as a person's press
/// does ([`App::dispatch_hw_key`]); the dispatch carries the controller mark, so its ⌘V sends the
/// front session no clipboard and its ⌘C leaves the clipboard alone, while its plain key still
/// types. The person's ⌘V, ⌘C and typing by the same arm paste, copy and type.
/// NEGATIVE CONTROL: with the dispatch left unmarked (`dispatch_hw_key` as on main,
/// `147b22abdc`), the posted ⌘V wrote `s3cret-pass` to the PTY.
#[test]
fn a_key_hwkey_posted_reaches_no_clipboard_by_the_persons_key_path() {
    use aterm_window::keyboard::ModifiersState;
    if !super::HARDCODED_SUPER_CHORDS {
        return;
    }
    let (mut app, mut reader, sink) = app_reading_pty();
    set_board(Some(SECRET));
    let _ = program_read(&mut reader, &sink);

    hw_key(&mut app, "v", ModifiersState::SUPER, true);
    let sent = program_read(&mut reader, &sink);
    assert!(!sent.contains(SECRET), "hwkey's ⌘V sent {sent:?}");
    assert!(!app.person_gesture_refused(), "the mark ends with the key");
    hw_key(&mut app, "x", ModifiersState::empty(), true);
    assert_eq!(
        program_read(&mut reader, &sink),
        "x",
        "hwkey's plain key types"
    );

    let term = app
        .front_terminal(WID)
        .expect("a front terminal")
        .term
        .clone();
    crate::term_lock(&term).process(b"on-screen words\r\n");
    select_top_line(&mut app);
    let selected = app.window_selection_text(WID).expect("a selection");
    hw_key(&mut app, "c", ModifiersState::SUPER, true);
    assert_eq!(board().as_deref(), Some(SECRET), "hwkey's ⌘C");

    select_top_line(&mut app);
    hw_key(&mut app, "c", ModifiersState::SUPER, false);
    assert_eq!(board(), Some(selected), "the person's ⌘C");
    set_board(Some(SECRET));
    hw_key(&mut app, "v", ModifiersState::SUPER, false);
    let sent = program_read(&mut reader, &sink);
    assert!(sent.contains(SECRET), "the person's ⌘V sent {sent:?}");
    hw_key(&mut app, "x", ModifiersState::empty(), false);
    assert_eq!(program_read(&mut reader, &sink), "x", "the person's typing");
    set_board(None);
}

/// AN EDIT ITEM A KEY `hwkey` POSTED CHOSE REACHES NO CLIPBOARD: ⌘V and ⌘C reach Edit ▸ Paste and
/// Copy by their key equivalents, whose native relay posts the item to the event loop; the relay
/// asks whether the key being dispatched is one a burst posted
/// ([`crate::tab_spawn::native_post_by_controller`]), so that item runs marked
/// ([`App::run_posted_menu_action`]): its Paste sends no clipboard and its Copy writes none, and
/// both answer why. The person's items paste and copy.
/// NEGATIVE CONTROL: with the relay posting no mark (as on main, `147b22abdc`), the posted ⌘V's
/// Paste wrote `s3cret-pass` to the PTY.
/// A KEY `hwkey` POSTED ANSWERS NO IN-WINDOW QUESTION: the paste question holds text from the
/// person's clipboard and the close question their work. A posted Return or Escape arrives by the
/// person's key path ([`App::dispatch_hw_key`]) under the controller mark, so it leaves each
/// question standing and reaches no program; the person's Return pastes and their Escape answers
/// the close question no. A control client answers a close with `confirm yes|no`.
/// NEGATIVE CONTROL: with `confirm_key`'s `posted` arm removed, the posted Return answered the
/// paste question yes.
#[test]
fn a_key_hwkey_posted_answers_no_in_window_question() {
    use aterm_window::keyboard::ModifiersState;
    let (mut app, mut reader, sink) = app_reading_pty();
    let _ = program_read(&mut reader, &sink);
    let session = app.focused_session_id(WID);
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        WID,
        session,
        format!("{SECRET}\nline two\n"),
        Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
    for key in ["\r", "\u{1b}"] {
        hw_key(&mut app, key, ModifiersState::empty(), true);
        assert!(
            app.paste_banner.is_some(),
            "a posted {key:?} answered the paste question"
        );
    }
    assert_eq!(
        program_read(&mut reader, &sink),
        "",
        "a posted key reached the program"
    );
    hw_key(&mut app, "\r", ModifiersState::empty(), false);
    assert!(app.paste_banner.is_none(), "the person's Return answers it");
    assert!(
        program_read(&mut reader, &sink).contains(SECRET),
        "and pastes"
    );

    let prompt =
        crate::quit_safety::confirm_prompt(false, true, true).expect("a busy close prompts");
    app.close_banner = Some(crate::close_confirm::PendingClose::new(
        WID,
        prompt,
        crate::close_confirm::CloseReplay::Window(WID),
        crate::session_store::ExitActor::Human,
    ));
    for key in ["\r", "\u{1b}"] {
        hw_key(&mut app, key, ModifiersState::empty(), true);
        assert!(
            app.close_banner.is_some(),
            "a posted {key:?} answered the close question"
        );
    }
    hw_key(&mut app, "\u{1b}", ModifiersState::empty(), false);
    assert!(app.close_banner.is_none(), "the person's Escape answers it");
    assert!(app.windows.contains_key(&WID), "no, so the window stays");
}

#[test]
fn an_edit_item_a_key_hwkey_posted_chose_reaches_no_clipboard() {
    let (mut app, mut reader, sink) = app_reading_pty();
    set_board(Some(SECRET));
    let _ = program_read(&mut reader, &sink);
    let item = |app: &mut App, action: MenuAction, keycode: u16, posted: bool| {
        app.pending_action_refusal = None;
        let by_controller = dispatching(
            current_key(keycode, posted),
            crate::tab_spawn::native_post_by_controller,
        );
        app.run_posted_menu_action(action, None, by_controller, edit_menu_body);
    };

    item(&mut app, MenuAction::Paste, 9, true);
    let sent = program_read(&mut reader, &sink);
    assert!(!sent.contains(SECRET), "hwkey's Paste sent {sent:?}");
    assert_eq!(
        app.pending_action_refusal.as_deref(),
        Some(CLIPBOARD_REFUSED)
    );
    let term = app
        .front_terminal(WID)
        .expect("a front terminal")
        .term
        .clone();
    crate::term_lock(&term).process(b"on-screen words\r\n");
    select_top_line(&mut app);
    let selected = app.window_selection_text(WID).expect("a selection");
    item(&mut app, MenuAction::Copy, 8, true);
    assert_eq!(board().as_deref(), Some(SECRET), "hwkey's Copy");
    assert_eq!(
        app.pending_action_refusal.as_deref(),
        Some(CLIPBOARD_REFUSED)
    );

    select_top_line(&mut app);
    item(&mut app, MenuAction::Copy, 8, false);
    assert_eq!(board(), Some(selected), "the person's Copy");
    set_board(Some(SECRET));
    item(&mut app, MenuAction::Paste, 9, false);
    let sent = program_read(&mut reader, &sink);
    assert!(sent.contains(SECRET), "the person's Paste sent {sent:?}");
    assert_eq!(app.pending_action_refusal, None);
    set_board(None);
}

/// A KEY `hwkey` POSTED INTO THE STATS LENS COPIES NOTHING: a keyboard lens takes ⌘⇧C (its CPU
/// diagnostic report) and ⌘C or a Copy binding (its sentence) before any binding, by the key
/// path an `hwkey` key arrives on ([`App::dispatch_hw_key`]); neither chord is an Edit item's key
/// equivalent while the lens has it, so the lens itself refuses the controller mark: the
/// clipboard keeps what it held. The person's ⌘⇧C and ⌘C copy the report and the sentence.
/// NEGATIVE CONTROL: with the lens's two copies unguarded (as on main, `147b22abdc`), the posted
/// ⌘⇧C put the CPU report on the board.
#[test]
fn a_key_hwkey_posted_into_the_stats_lens_copies_nothing() {
    use aterm_window::keyboard::ModifiersState;

    use crate::stats_lens::{Lens, Opened};
    use crate::stats_paint::{Bar, Instrument};
    let mut app = App::headless_for_test();
    let snap = Arc::new(crate::stats_deck::tests::snapshot());
    app.vitals.snapshot = Some(snap.clone());
    let open = |app: &mut App| {
        app.windows.get_mut(&WID).unwrap().stats_lens.open = Some(Lens {
            focus: Instrument::Cpu,
            bar: Bar::Global,
            how: Opened::Keyboard,
        });
    };
    let report = crate::stats_lens::cpu_diagnostic_report(&snap);
    let shift_cmd = ModifiersState::SUPER | ModifiersState::SHIFT;

    set_board(Some(SECRET));
    open(&mut app);
    hw_key(&mut app, "c", shift_cmd, true);
    assert_eq!(board().as_deref(), Some(SECRET), "hwkey's ⌘⇧C");
    assert!(
        app.windows[&WID].stats_lens.open.is_some(),
        "the lens took the chord"
    );
    hw_key(&mut app, "c", ModifiersState::SUPER, true);
    assert_eq!(board().as_deref(), Some(SECRET), "hwkey's ⌘C");

    hw_key(&mut app, "c", shift_cmd, false);
    assert_eq!(board(), Some(report), "the person's ⌘⇧C");
    let sentence = app.stats_lens_sentence(WID).expect("a sentence");
    hw_key(&mut app, "c", ModifiersState::SUPER, false);
    assert_eq!(board(), Some(sentence), "the person's ⌘C");
    set_board(None);
}
