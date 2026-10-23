// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Editor key translation characterization tests ([`App::native_input_event`], [`long`]).

use crate::input::InputEvent;
use crate::native_app::{AppEvent, AppKind, AppViewState};
use crate::{App, WindowId};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

/// Initial multi-line document for every table row.
fn long() -> String {
    let mut text = String::from("alpha beta\ngamma\n");
    for line in 3..=40 {
        text.push_str(&format!("line {line}\n"));
    }
    text
}

/// Where every row's caret starts: the `b` of `beta`.
const CARET: usize = 6;

fn press(key: Key, mods: Modifiers) -> InputEvent {
    InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

/// A character press whose layout typed `text` (Shift+a types `A`).
fn typed(character: char, mods: Modifiers, text: char) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(character),
        mods,
        base_layout: None,
        layout_text: Some(text),
        event_type: KeyEventType::Press,
    }
}

fn ch(character: char) -> InputEvent {
    press(Key::Character(character), Modifiers::empty())
}

fn named(key: NamedKey) -> InputEvent {
    press(Key::Named(key), Modifiers::empty())
}

fn ctrl(character: char) -> InputEvent {
    press(Key::Character(character), Modifiers::CTRL)
}

fn alt(character: char) -> InputEvent {
    press(Key::Character(character), Modifiers::ALT)
}

fn cmd(character: char) -> InputEvent {
    press(Key::Character(character), Modifiers::SUPER)
}

/// The chord that opens incremental search on this host.
fn isearch() -> InputEvent {
    if cfg!(windows) { alt('s') } else { ctrl('s') }
}

/// One row: press `press` after `before` and read `expect`.
struct Row {
    name: &'static str,
    before: Vec<InputEvent>,
    press: InputEvent,
    expect: String,
}

fn row(
    name: &'static str,
    before: Vec<InputEvent>,
    press: InputEvent,
    expect: impl Into<String>,
) -> Row {
    Row {
        name,
        before,
        press,
        expect: expect.into(),
    }
}

/// A fresh Editor on [`long`] in window 0, caret at [`CARET`], focus on the buffer.
fn editor(
    path: &std::path::Path,
) -> (
    App,
    crate::tab_model::ViewId,
    crate::document_store::DocumentId,
) {
    std::fs::write(path, long()).unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let uri = crate::native_document_host::path_to_file_uri(path).unwrap();
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::EditorSetSelection {
            anchor: CARET,
            head: CARET,
        },
    )
    .unwrap();
    app.native_runtime
        .view_state_mut(view)
        .unwrap()
        .common_mut()
        .last_focus = Some(crate::native_ui::UiKey::new("editor/buffer"));
    (app, view, document)
}

/// How many whole document rows `app`'s editor shows.
fn screen(app: &App) -> usize {
    let wid = WindowId(0);
    let (instance, _) = app.active_native_view(wid).expect("the editor tab");
    let Some(crate::native_app::NativeApp::Editor(editor)) = app.native_runtime.app(instance)
    else {
        panic!("the active tab is not an editor");
    };
    crate::native_ui::editor_visible_line_capacity_with_palette(
        app.native_ui_viewport(wid).expect("the editor's viewport"),
        crate::native_ui::EditorPalette::None,
        editor.toolbar(),
    )
}

/// What the editor shows after a key, as one line ([`long`]).
fn seen(
    app: &App,
    view: crate::tab_model::ViewId,
    document: crate::document_store::DocumentId,
) -> String {
    let text = app.document_store.snapshot(document).unwrap().text;
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("the editor view is gone");
    };
    let buffer = state.buffer.as_ref().expect("attached editor buffer");
    let mut out = Vec::new();
    if *text != *long() {
        let head = text.split_inclusive('\n').take(2).collect::<String>();
        out.push(format!("text={:?}", head.replace('\n', "⏎")));
    }
    if app.document_store.dirty(document) == Some(true) {
        out.push("dirty".to_string());
    }
    let selection = buffer.primary_selection();
    if selection.anchor == selection.head {
        out.push(format!("sel={}", selection.head));
    } else {
        out.push(format!("sel={}..{}", selection.anchor, selection.head));
    }
    if buffer.selections.len() != 1 {
        out.push(format!("carets={}", buffer.selections.len()));
    }
    if buffer.mark_active {
        out.push("mark".to_string());
    }
    if buffer.chord_pending() {
        out.push("pending".to_string());
    }
    if let Some(hud) = &buffer.prefix_hud {
        let keys = hud.split(" — ").next().unwrap_or(hud);
        out.push(format!("hud={keys}"));
    }
    match &buffer.minibuffer {
        crate::native_editor::Minibuffer::Inactive => {}
        crate::native_editor::Minibuffer::Command { query, selected } => {
            out.push(format!("mb=M-x:{query:?}#{selected}"));
        }
        crate::native_editor::Minibuffer::Search { query, .. } => {
            out.push(format!("mb=isearch:{query:?}"));
        }
        crate::native_editor::Minibuffer::Buffer { query } => {
            out.push(format!("mb=buffer:{query:?}"));
        }
        crate::native_editor::Minibuffer::GotoLine { query, .. } => {
            out.push(format!("mb=goto:{query:?}"));
        }
        crate::native_editor::Minibuffer::Prompt { invocation, query } => {
            out.push(format!("mb={}:{query:?}", invocation.command.name()));
        }
    }
    let focus = state
        .common
        .last_focus
        .as_ref()
        .map(crate::native_ui::UiKey::as_str);
    if focus != Some("editor/buffer") {
        out.push(format!("focus={}", focus.unwrap_or("none")));
    }
    let top = text[..buffer.viewport_anchor.min(text.len())]
        .matches('\n')
        .count();
    if top != 0 {
        out.push(format!("top={top}"));
    }
    if let Some(status) = &state.status {
        out.push(format!("status={status:?}"));
    }
    out.join(" ")
}

/// The table, for an editor that shows `screen` whole rows ([`screen`]).
fn rows(screen: usize) -> Vec<Row> {
    use NamedKey as N;
    let text = long();
    let line_start = |line: usize| {
        text.split_inclusive('\n')
            .take(line)
            .map(str::len)
            .sum::<usize>()
    };
    let lines = text.matches('\n').count() + 1;
    let page = screen.saturating_sub(2);
    assert!(
        page > 0 && page <= lines - screen.min(lines),
        "the fixture's editor shows {screen} of {lines} rows"
    );
    let shift = Modifiers::SHIFT;
    let mac = cfg!(target_os = "macos");
    let windows = cfg!(windows);
    vec![
        row(
            "x",
            vec![],
            ch('x'),
            r#"text="alpha xbeta⏎gamma⏎" dirty sel=7"#,
        ),
        row(
            "Shift+a types the layout's A",
            vec![],
            typed('a', shift, 'A'),
            r#"text="alpha Abeta⏎gamma⏎" dirty sel=7"#,
        ),
        row(
            "Shift+2 types the layout's \"",
            vec![],
            typed('2', shift, '"'),
            r#"text="alpha \"beta⏎gamma⏎" dirty sel=7"#,
        ),
        row(
            "Space",
            vec![],
            named(N::Space),
            r#"text="alpha  beta⏎gamma⏎" dirty sel=7"#,
        ),
        row(
            "Shift+Space",
            vec![],
            press(Key::Named(N::Space), shift),
            r#"text="alpha  beta⏎gamma⏎" dirty sel=7"#,
        ),
        row(
            "Backspace",
            vec![],
            named(N::Backspace),
            r#"text="alphabeta⏎gamma⏎" dirty sel=5"#,
        ),
        row(
            "Delete",
            vec![],
            named(N::Delete),
            r#"text="alpha eta⏎gamma⏎" dirty sel=6"#,
        ),
        row("Left", vec![], named(N::ArrowLeft), "sel=5"),
        row("Right", vec![], named(N::ArrowRight), "sel=7"),
        row(
            "Shift+Left extends",
            vec![],
            press(Key::Named(N::ArrowLeft), shift),
            "sel=6..5",
        ),
        row(
            "Shift+Right extends",
            vec![],
            press(Key::Named(N::ArrowRight), shift),
            "sel=6..7",
        ),
        row("Up", vec![], named(N::ArrowUp), "sel=6"),
        row("Down", vec![], named(N::ArrowDown), "sel=16"),
        row(
            "Shift+Down",
            vec![],
            press(Key::Named(N::ArrowDown), shift),
            "sel=16",
        ),
        row("Home", vec![], named(N::Home), "sel=0"),
        row("End", vec![], named(N::End), "sel=10"),
        row(
            "Enter",
            vec![],
            named(N::Enter),
            r#"text="alpha ⏎beta⏎" dirty sel=7"#,
        ),
        row(
            "Shift+Enter",
            vec![],
            press(Key::Named(N::Enter), shift),
            r#"text="alpha ⏎beta⏎" dirty sel=7"#,
        ),
        row("Escape", vec![], named(N::Escape), r#"sel=6 status="Quit""#),
        row(
            "C-Space Escape drops the region",
            vec![press(Key::Named(N::Space), Modifiers::CTRL)],
            named(N::Escape),
            r#"sel=6 status="Quit""#,
        ),
        row(
            "Escape Escape",
            vec![named(N::Escape)],
            named(N::Escape),
            r#"sel=6 status="Quit""#,
        ),
        row(
            "Escape x",
            vec![named(N::Escape)],
            ch('x'),
            r#"text="alpha xbeta⏎gamma⏎" dirty sel=7 status="Quit""#,
        ),
        row(
            "Escape Down",
            vec![named(N::Escape)],
            named(N::ArrowDown),
            r#"sel=16 status="Quit""#,
        ),
        row(
            "Escape Down d",
            vec![named(N::Escape), named(N::ArrowDown)],
            ch('d'),
            r#"text="alpha beta⏎gammad⏎" dirty sel=17 status="Quit""#,
        ),
        row(
            "Escape ⌘Z undoes",
            vec![ch('x'), named(N::Escape)],
            cmd('z'),
            r#"sel=6 status="No unsaved changes""#,
        ),
        row(
            "Escape Shift+Tab",
            vec![named(N::Escape)],
            press(Key::Named(N::Tab), shift),
            r#"sel=6 focus=editor/commands-button status="Quit""#,
        ),
        row(
            "PageDown",
            vec![],
            named(N::PageDown),
            format!("sel={} top={page}", line_start(page) + CARET),
        ),
        row(
            "PageUp after PageDown",
            vec![named(N::PageDown)],
            named(N::PageUp),
            "sel=6",
        ),
        row("F5 does nothing", vec![], named(N::F5), "sel=6"),
        row("Insert does nothing", vec![], named(N::Insert), "sel=6"),
        row(
            "Numpad5 types 5",
            vec![],
            named(N::Numpad5),
            r#"text="alpha 5beta⏎gamma⏎" dirty sel=7"#,
        ),
        row(
            "NumpadEnter is Enter",
            vec![],
            named(N::NumpadEnter),
            r#"text="alpha ⏎beta⏎" dirty sel=7"#,
        ),
        row(
            "a release does nothing",
            vec![],
            InputEvent::Key {
                key: Key::Character('x'),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Release,
            },
            "sel=6",
        ),
        row(
            "committed text",
            vec![],
            InputEvent::Text("é".to_string()),
            r#"text="alpha ébeta⏎gamma⏎" dirty sel=8"#,
        ),
        row(
            "a paste",
            vec![],
            InputEvent::Paste("one\ntwo".to_string(), crate::input::PasteFraming::AtDrain),
            r#"text="alpha one⏎twobeta⏎" dirty sel=13"#,
        ),
        row(
            "Tab",
            vec![],
            named(N::Tab),
            r#"text="alpha   beta⏎gamma⏎" dirty sel=8"#,
        ),
        row(
            "Shift+Tab",
            vec![],
            press(Key::Named(N::Tab), shift),
            "sel=6 focus=editor/commands-button",
        ),
        row(
            "Ctrl+Tab",
            vec![],
            press(Key::Named(N::Tab), Modifiers::CTRL),
            "sel=6 focus=editor/find-button",
        ),
        row(
            "Ctrl+Shift+Tab",
            vec![],
            press(Key::Named(N::Tab), Modifiers::CTRL | shift),
            "sel=6 focus=editor/commands-button",
        ),
        row("F6", vec![], named(N::F6), "sel=6 focus=editor/find-button"),
        row(
            "Shift+F6",
            vec![],
            press(Key::Named(N::F6), shift),
            "sel=6 focus=editor/commands-button",
        ),
        row(
            "⌘S saves",
            vec![ch('x')],
            cmd('s'),
            r#"text="alpha xbeta⏎gamma⏎" sel=7 status="Saved""#,
        ),
        row(
            "⌘A selects all",
            vec![],
            cmd('a'),
            format!("sel=0..{} top={}", text.len(), lines - screen),
        ),
        row(
            "⌘Z undoes",
            vec![ch('x')],
            cmd('z'),
            r#"sel=6 status="No unsaved changes""#,
        ),
        row(
            "⌘⇧Z redoes",
            vec![ch('x'), cmd('z')],
            press(Key::Character('z'), Modifiers::SUPER | shift),
            r#"text="alpha xbeta⏎gamma⏎" dirty sel=7 status="No unsaved changes""#,
        ),
        row(
            "⌘Y redoes",
            vec![ch('x'), cmd('z')],
            cmd('y'),
            r#"text="alpha xbeta⏎gamma⏎" dirty sel=7 status="No unsaved changes""#,
        ),
        row("⌘C does nothing here", vec![], cmd('c'), "sel=6"),
        row(
            "Hyper+x does nothing",
            vec![],
            press(Key::Character('x'), Modifiers::HYPER),
            "sel=6",
        ),
        row("C-f", vec![], ctrl('f'), "sel=7"),
        row("C-b", vec![], ctrl('b'), "sel=5"),
        row("C-a", vec![], ctrl('a'), "sel=0"),
        row("C-e", vec![], ctrl('e'), "sel=10"),
        row("C-n", vec![], ctrl('n'), "sel=16"),
        row("C-p", vec![ctrl('n')], ctrl('p'), "sel=6"),
        row(
            "C-d",
            vec![],
            ctrl('d'),
            r#"text="alpha eta⏎gamma⏎" dirty sel=6"#,
        ),
        row("M-f", vec![], alt('f'), "sel=10"),
        row("M-b", vec![], alt('b'), "sel=0"),
        row(
            "Meta+f",
            vec![],
            press(Key::Character('f'), Modifiers::META),
            "sel=10",
        ),
        row(
            "M-left",
            vec![],
            press(Key::Named(N::ArrowLeft), Modifiers::ALT),
            "sel=0",
        ),
        row(
            "M-right",
            vec![],
            press(Key::Named(N::ArrowRight), Modifiers::ALT),
            "sel=10",
        ),
        row(
            "C-left",
            vec![],
            press(Key::Named(N::ArrowLeft), Modifiers::CTRL),
            if mac {
                r#"sel=6 status="C-left is not bound""#
            } else {
                "sel=0"
            },
        ),
        row(
            "C-right",
            vec![],
            press(Key::Named(N::ArrowRight), Modifiers::CTRL),
            if mac {
                r#"sel=6 status="C-right is not bound""#
            } else {
                "sel=10"
            },
        ),
        row(
            "the isearch chord",
            vec![],
            isearch(),
            r#"sel=6 mb=isearch:"""#,
        ),
        row(
            "C-s",
            vec![ch('x')],
            ctrl('s'),
            if windows {
                r#"text="alpha xbeta⏎gamma⏎" sel=7 status="Saved""#
            } else {
                r#"text="alpha xbeta⏎gamma⏎" dirty sel=7 mb=isearch:"""#
            },
        ),
        row(
            "M-s",
            vec![],
            alt('s'),
            if windows {
                r#"sel=6 mb=isearch:"""#
            } else {
                r#"sel=6 status="M-s is not bound""#
            },
        ),
        row(
            "C-Space sets the mark",
            vec![],
            press(Key::Named(N::Space), Modifiers::CTRL),
            r#"sel=6 mark status="Mark set""#,
        ),
        row("C-g", vec![], ctrl('g'), r#"sel=6 status="Quit""#),
        row(
            "C-Backspace",
            vec![],
            press(Key::Named(N::Backspace), Modifiers::CTRL),
            if mac {
                r#"sel=6 status="C-backspace is not bound""#
            } else {
                r#"text="beta⏎gamma⏎" dirty sel=0"#
            },
        ),
        row(
            "M-d kills a word",
            vec![],
            alt('d'),
            r#"text="alpha ⏎gamma⏎" dirty sel=6"#,
        ),
        row(
            "M-Backspace kills a word back",
            vec![],
            press(Key::Named(N::Backspace), Modifiers::ALT),
            r#"text="beta⏎gamma⏎" dirty sel=0"#,
        ),
        row(
            "C-Space M-f M-w copies",
            vec![press(Key::Named(N::Space), Modifiers::CTRL), alt('f')],
            alt('w'),
            r#"sel=10 status="Mark set""#,
        ),
        row(
            "C-o opens a line",
            vec![],
            ctrl('o'),
            r#"text="alpha ⏎beta⏎" dirty sel=6"#,
        ),
        row(
            "C-t transposes",
            vec![],
            ctrl('t'),
            r#"text="alphab eta⏎gamma⏎" dirty sel=7"#,
        ),
        row(
            "M-< is the buffer's start",
            vec![],
            press(Key::Character(','), Modifiers::ALT | shift),
            r#"sel=0 status="Mark set""#,
        ),
        row(
            "M-> is the buffer's end",
            vec![],
            press(Key::Character('.'), Modifiers::ALT | shift),
            format!(
                r#"sel={} top={} status="Mark set""#,
                text.len(),
                lines - screen
            ),
        ),
        row(
            "C-? redoes",
            vec![ch('x'), ctrl('/')],
            press(Key::Character('/'), Modifiers::CTRL | shift),
            r#"text="alpha xbeta⏎gamma⏎" dirty sel=7 status="No unsaved changes""#,
        ),
        row("M-3 C-f", vec![alt('3')], ctrl('f'), "sel=9"),
        row(
            "C-x h marks the buffer",
            vec![ctrl('x')],
            ch('h'),
            r#"sel=314..0 mark status="Mark set""#,
        ),
        row(
            "C-x C-x without a mark",
            vec![ctrl('x')],
            ctrl('x'),
            r#"sel=6 status="No mark set in this buffer""#,
        ),
        row(
            "C-x k asks which buffer",
            vec![ctrl('x')],
            ch('k'),
            r#"sel=6 mb=kill-buffer:"""#,
        ),
        row(
            "C-c C-c with no client waiting",
            vec![ctrl('c')],
            ctrl('c'),
            r#"sel=6 status="nothing is waiting on this file; C-x C-s saves""#,
        ),
        row(
            "C-c C-k with no client waiting",
            vec![ctrl('c')],
            ctrl('k'),
            r#"sel=6 status="nothing is waiting on this file; C-x C-s saves""#,
        ),
        row(
            "M-Enter",
            vec![],
            press(Key::Named(N::Enter), Modifiers::ALT),
            r#"sel=6 status="M-enter is not bound""#,
        ),
        row(
            "C-S-a",
            vec![],
            press(Key::Character('a'), Modifiers::CTRL | shift),
            r#"sel=6 status="C-S-a is not bound""#,
        ),
        row(
            "C-Up is dropped",
            vec![],
            press(Key::Named(N::ArrowUp), Modifiers::CTRL),
            "sel=6",
        ),
        row(
            "C-F5 is dropped",
            vec![],
            press(Key::Named(N::F5), Modifiers::CTRL),
            "sel=6",
        ),
        row("C-x", vec![], ctrl('x'), "sel=6 pending hud=C-x"),
        row(
            "C-x C-s saves",
            vec![ch('x'), ctrl('x')],
            ctrl('s'),
            r#"text="alpha xbeta⏎gamma⏎" sel=7 status="Saved""#,
        ),
        row(
            "C-x u undoes",
            vec![ch('x'), ctrl('x')],
            ch('u'),
            r#"sel=6 status="No unsaved changes""#,
        ),
        row(
            "C-x z",
            vec![ctrl('x')],
            ch('z'),
            r#"sel=6 status="C-x z is not bound""#,
        ),
        row(
            "C-x Escape",
            vec![ctrl('x')],
            named(N::Escape),
            r#"sel=6 status="C-x escape is not bound""#,
        ),
        row(
            "C-x Tab",
            vec![ctrl('x')],
            named(N::Tab),
            r#"sel=6 status="C-x tab is not bound""#,
        ),
        row(
            "C-x Space",
            vec![ctrl('x')],
            named(N::Space),
            r#"sel=6 status="C-x space is not bound""#,
        ),
        row(
            "C-x Up is dropped",
            vec![ctrl('x')],
            named(N::ArrowUp),
            "sel=6 pending hud=C-x",
        ),
        row(
            "C-x F6",
            vec![ctrl('x')],
            named(N::F6),
            "sel=6 pending hud=C-x focus=editor/find-button",
        ),
        row(
            "C-x ⌘S saves",
            vec![ch('x'), ctrl('x')],
            cmd('s'),
            r#"text="alpha xbeta⏎gamma⏎" sel=7 pending hud=C-x status="Saved""#,
        ),
        row(
            "C-x ⌘A",
            vec![ctrl('x')],
            cmd('a'),
            r#"sel=6 status="C-x a is not bound""#,
        ),
        row("M-g g", vec![alt('g')], ch('g'), r#"sel=6 mb=goto:"""#),
        row("C-u", vec![], ctrl('u'), "sel=6 hud=C-u 4"),
        row("C-u 3", vec![ctrl('u')], ch('3'), "sel=6 hud=C-u 3"),
        row("C-u 3 C-f", vec![ctrl('u'), ch('3')], ctrl('f'), "sel=9"),
        row(
            "C-u 3 x",
            vec![ctrl('u'), ch('3')],
            ch('x'),
            r#"text="alpha xbeta⏎gamma⏎" dirty sel=7"#,
        ),
        row("C-u C-n", vec![ctrl('u')], ctrl('n'), "sel=37"),
        row(
            "M-x",
            vec![],
            alt('x'),
            r##"sel=6 mb=M-x:""#0 status="Type a command name""##,
        ),
        row(
            "M-x Tab completes",
            vec![alt('x'), ch('u'), ch('n'), ch('d')],
            named(N::Tab),
            r##"sel=6 mb=M-x:"undo"#0 status="Type a command name""##,
        ),
        row(
            "M-x Down",
            vec![alt('x')],
            named(N::ArrowDown),
            r##"sel=6 mb=M-x:""#1 status="Type a command name""##,
        ),
        row(
            "M-x Up",
            vec![alt('x')],
            named(N::ArrowUp),
            r##"sel=6 mb=M-x:""#7 status="Type a command name""##,
        ),
        row(
            "M-x Enter runs",
            vec![ch('x'), alt('x'), ch('u'), ch('n'), ch('d'), ch('o')],
            named(N::Enter),
            r#"sel=6 status="No unsaved changes""#,
        ),
        row(
            "M-x Escape",
            vec![alt('x')],
            named(N::Escape),
            r#"sel=6 status="Quit""#,
        ),
        row(
            "M-x Backspace",
            vec![alt('x'), ch('a'), ch('b')],
            named(N::Backspace),
            r##"sel=6 mb=M-x:"a"#0 status="Type a command name""##,
        ),
        row(
            "M-x Left is refused",
            vec![alt('x')],
            named(N::ArrowLeft),
            r##"sel=6 mb=M-x:""#0 status="Minibuffer active — type, Backspace, Enter, or C-g""##,
        ),
        row(
            "M-x Shift+Tab",
            vec![alt('x')],
            press(Key::Named(N::Tab), shift),
            r##"sel=6 mb=M-x:"save-buffer"#0 status="Type a command name""##,
        ),
        row(
            "M-x Ctrl+Tab",
            vec![alt('x')],
            press(Key::Named(N::Tab), Modifiers::CTRL),
            r##"sel=6 mb=M-x:"save-buffer"#0 status="Type a command name""##,
        ),
        row(
            "isearch types",
            vec![isearch()],
            ch('g'),
            r#"sel=11..12 mb=isearch:"g" status="I-search: g""#,
        ),
        row(
            "isearch again",
            vec![isearch(), ch('a')],
            isearch(),
            r#"sel=12..13 mb=isearch:"a" status="I-search: a""#,
        ),
        row(
            "isearch Escape",
            vec![isearch(), ch('g')],
            named(N::Escape),
            r#"sel=6 status="Quit""#,
        ),
        row(
            "isearch Enter",
            vec![isearch(), ch('g')],
            named(N::Enter),
            r#"sel=11..12 status="""#,
        ),
        row(
            "isearch PageDown is refused",
            vec![isearch()],
            named(N::PageDown),
            r#"sel=6 mb=isearch:"" status="Minibuffer active — Enter accepts; C-g cancels""#,
        ),
        row(
            "isearch Up",
            vec![isearch()],
            named(N::ArrowUp),
            r#"sel=6 mb=isearch:"" status="Minibuffer active — Enter accepts; C-g cancels""#,
        ),
        row(
            "isearch Tab",
            vec![isearch()],
            named(N::Tab),
            r#"sel=6 mb=isearch:"" status="Minibuffer active — Enter accepts; C-g cancels""#,
        ),
        row(
            "isearch Shift+Tab",
            vec![isearch()],
            press(Key::Named(N::Tab), shift),
            r#"sel=6 mb=isearch:"" focus=editor/commands-button"#,
        ),
        row(
            "isearch C-f",
            vec![isearch()],
            ctrl('f'),
            r#"sel=6 mb=isearch:"" status="Minibuffer active — Enter accepts; C-g cancels""#,
        ),
        row(
            "goto Enter",
            vec![alt('g'), ch('g'), ch('3')],
            named(N::Enter),
            r#"sel=17 top=2 status="Line 3""#,
        ),
    ]
}

/// Every row of the table, against a fresh editor each.
#[test]
fn editor_keys_do_what_the_table_says() {
    let dir = std::env::temp_dir().join(format!("aterm-editor-input-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wid = WindowId(0);
    let screen = {
        let (app, _, _) = editor(&dir.join("screen.txt"));
        screen(&app)
    };
    let mut wrong = Vec::new();
    for (index, row) in rows(screen).into_iter().enumerate() {
        let (mut app, view, document) = editor(&dir.join(format!("row-{index}.txt")));
        for key in &row.before {
            assert!(app.native_input_event(wid, key), "{}: before key", row.name);
        }
        assert!(
            app.native_input_event(wid, &row.press),
            "{}: the key",
            row.name
        );
        let got = seen(&app, view, document);
        if got != row.expect {
            wrong.push(format!(
                "{}\n    got:    {got}\n    wanted: {}",
                row.name, row.expect
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        wrong.is_empty(),
        "{} rows differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// Escape, then a minibuffer opened by a button rather than a key (Find, Go
/// to Line, Commands): the minibuffer takes the next keys. (A pending ESC,
/// when a GUI's Escape was Meta, once held every key but C-g.)
#[test]
fn escape_then_find_still_types() {
    let dir = std::env::temp_dir().join(format!("aterm-editor-esc-find-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wid = WindowId(0);
    for (action, typed) in [
        (
            "editor/find",
            r#"sel=11..12 mb=isearch:"g" status="I-search: g""#,
        ),
        ("editor/goto-line", r#"sel=6 mb=goto:"3" status="Quit""#),
        (
            "editor/commands",
            r##"sel=6 mb=M-x:"3"#0 status="Type a command name""##,
        ),
    ] {
        let (mut app, view, document) = editor(&dir.join("esc-find.txt"));
        assert!(app.native_input_event(wid, &named(NamedKey::Escape)));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(action),
                value: None,
            }),
        )
        .unwrap();
        let key = if action == "editor/find" { 'g' } else { '3' };
        assert!(app.native_input_event(wid, &ch(key)));
        assert_eq!(seen(&app, view, document), typed, "{action}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Tab in an Editor buffer indents to the next tab stop through the native
/// input boundary, where it used to move focus to the toolbar; the workspace
/// it reaches uses the GUI keymap of this platform.
#[test]
fn tab_indents_in_editor_buffer() {
    let dir = std::env::temp_dir().join(format!("aterm-editor-tab-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut app, view, document) = editor(&dir.join("tab.txt"));
    assert_eq!(
        app.editor_workspace.keymap.profile(),
        amacs::editor::Profile::Gui(amacs::editor::Platform::HOST)
    );
    let wid = WindowId(0);
    assert!(app.native_input_event(wid, &named(NamedKey::Tab)));
    assert!(app.native_input_event(wid, &named(NamedKey::Tab)));
    let text = app.document_store.snapshot(document).unwrap().text;
    assert!(text.starts_with("alpha       beta\n"), "{text:?}");
    let outcome = seen(&app, view, document);
    assert!(outcome.contains(" sel=12"), "{outcome}");
    assert!(!seen(&app, view, document).contains("focus="));
    assert!(app.native_input_event(wid, &cmd('z')));
    let text = app.document_store.snapshot(document).unwrap().text;
    assert!(text.starts_with("alpha   beta\n"), "{text:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// F6 and Shift+F6 walk keyboard focus through an Editor's controls in the
/// compiled focus order, both ways and around, and never edit the buffer.
#[test]
fn f6_moves_native_focus() {
    let dir = std::env::temp_dir().join(format!("aterm-editor-f6-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut app, view, document) = editor(&dir.join("f6.txt"));
    let wid = WindowId(0);
    let order = app.compiled_native_ui(wid).unwrap().focus_order;
    assert!(order.len() > 2, "{order:?}");
    let focus = |app: &App| {
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .last_focus
            .clone()
    };
    let buffer = order.iter().position(|key| key.as_str() == "editor/buffer");
    let first = buffer.map_or(0, |at| (at + 1) % order.len());
    assert!(app.native_input_event(wid, &named(NamedKey::F6)));
    assert_eq!(focus(&app).as_ref(), Some(&order[first]));
    assert!(app.native_input_event(wid, &named(NamedKey::F6)));
    assert_eq!(
        focus(&app).as_ref(),
        Some(&order[(first + 1) % order.len()])
    );
    let shift_f6 = press(Key::Named(NamedKey::F6), Modifiers::SHIFT);
    assert!(app.native_input_event(wid, &shift_f6));
    assert_eq!(focus(&app).as_ref(), Some(&order[first]));
    assert_eq!(
        *app.document_store.snapshot(document).unwrap().text,
        *long(),
        "focus keys never edit the buffer"
    );

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    let (_, settings) = app.active_native_view(wid).unwrap();
    let before = app
        .native_runtime
        .view_state(settings)
        .unwrap()
        .common()
        .last_focus
        .clone();
    assert!(app.native_input_event(wid, &named(NamedKey::F6)));
    let after = app
        .native_runtime
        .view_state(settings)
        .unwrap()
        .common()
        .last_focus
        .clone();
    assert!(
        after.is_some() && after != before,
        "{before:?} -> {after:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Ctrl+Tab over an Editor is still the host's next tab
/// (`keybinding.rs`'s `ctrl+tab = next_tab`): it switches tabs and never
/// reaches the buffer, whose Tab now indents. Windows and Linux seed the
/// binding; macOS's Next Tab is ⌘⇧], so there the row is the user's own,
/// which wins over the Editor the same way.
#[test]
fn ctrl_tab_still_switches_tabs() {
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{
        Key as WinitKey, KeyCode, KeyLocation, ModifiersState, NamedKey as WinitNamed, PhysicalKey,
    };

    let dir = std::env::temp_dir().join(format!("aterm-editor-ctrl-tab-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut app, _, document) = editor(&dir.join("ctrl-tab.txt"));
    let wid = WindowId(0);
    let editor_tab = app.windows[&wid].tab_set.active().unwrap().id;
    assert!(app.windows[&wid].tab_set.tabs().len() >= 2);

    let mut table = std::collections::BTreeMap::new();
    if cfg!(target_os = "macos") {
        table.insert("ctrl+tab".to_string(), "next_tab".to_string());
    }
    app.keybindings = crate::keybinding::Keybindings::resolved(Some(&table));
    app.frontmost_window = Some(wid);
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL;
    app.on_key(
        wid,
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::Tab),
            WinitKey::Named(WinitNamed::Tab),
            None,
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        ),
    );
    assert_ne!(
        app.windows[&wid].tab_set.active().unwrap().id,
        editor_tab,
        "Ctrl+Tab moved to the next tab"
    );
    assert_eq!(
        *app.document_store.snapshot(document).unwrap().text,
        *long(),
        "Ctrl+Tab never indents the buffer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
