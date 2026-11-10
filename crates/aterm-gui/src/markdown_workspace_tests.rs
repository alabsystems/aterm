// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Document, input and edit-session invariants across Markdown presentation modes.

use std::sync::mpsc::TryRecvError;
use std::time::Instant;

use super::tests::{caller, edit, request, wait};
use crate::app_doc::DocAnswer;
use crate::input::{InputEvent, PasteFraming};
use crate::native_app::{
    ActionInvocation, AppEvent, AppKind, AppViewState, EditorViewState, MarkdownPane,
    MarkdownWorkspaceMode, NativeApp, TextInputEvent,
};
use crate::native_ui::{ActionId, UiKey};
use crate::tab_model::ViewId;
use crate::{App, WindowId};
use amacs::protocol::edit::{AwaitKey, EditOutcome, EditResult};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

fn state(app: &App, view: ViewId) -> &EditorViewState {
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("the document retains Editor ownership in every mode");
    };
    state
}

fn source_status(app: &App) -> (bool, bool) {
    let compiled = app.compiled_native_ui(WindowId(0)).unwrap();
    compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            crate::native_ui::UiContent::TextViewport(spec) => Some((spec.dirty, spec.saving)),
            _ => None,
        })
        .expect("Edit and Split expose the source viewport")
}

fn action(app: &mut App, id: &str) {
    app.dispatch_native_event(
        WindowId(0),
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(id),
            value: None,
        }),
    )
    .unwrap();
}

fn command(app: &mut App, letter: char) {
    assert!(app.native_input_event(
        WindowId(0),
        &InputEvent::Key {
            key: Key::Character(letter),
            mods: Modifiers::SUPER,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
    ));
}

fn open(name: &str, source: &str) -> (App, aterm_tempfile::TempDir, std::path::PathBuf, ViewId) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, source).unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Markdown, &uri).unwrap();
    let (_, view) = app.active_native_view(WindowId(0)).unwrap();
    (app, dir, path, view)
}

#[test]
fn markdown_modes_share_view_buffer_preview_and_undo_without_implicit_save() {
    let original = "# Original\n\nA paragraph.\n";
    let (mut app, _dir, path, view) = open("continuity.md", original);
    let wid = WindowId(0);
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let tab = app.windows[&wid].tab_set.active_id();
    let views = app.view_store.len();
    assert_eq!(
        state(&app, view).markdown.as_ref().unwrap().mode,
        MarkdownWorkspaceMode::Read
    );

    command(&mut app, 'e');
    assert_eq!(
        state(&app, view).markdown.as_ref().unwrap().mode,
        MarkdownWorkspaceMode::Edit
    );
    app.dispatch_native_event(
        wid,
        AppEvent::EditorSetSelection {
            anchor: 2,
            head: 10,
        },
    )
    .unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Commit("Changed".to_string())),
    )
    .unwrap();
    let edited = app.document_store.snapshot(document).unwrap();
    assert_eq!(edited.text.as_ref(), "# Changed\n\nA paragraph.\n");
    assert!(app.editor_workspace.can_undo(document));
    let selection = state(&app, view)
        .buffer
        .as_ref()
        .unwrap()
        .primary_selection()
        .clone();

    for mode in [
        "editor/markdown/split",
        "editor/markdown/read",
        "editor/markdown/edit",
    ] {
        action(&mut app, mode);
        assert_eq!(app.active_native_view(wid), Some((instance, view)));
        assert_eq!(app.windows[&wid].tab_set.active_id(), tab);
        assert_eq!(app.view_store.len(), views);
        assert_eq!(app.document_store.view_count(document), Some(1));
        let current = app.document_store.snapshot(document).unwrap();
        assert_eq!(
            (current.seq, current.text),
            (edited.seq, edited.text.clone())
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        if state(&app, view).markdown.as_ref().unwrap().mode != MarkdownWorkspaceMode::Read {
            assert_eq!(
                source_status(&app),
                (true, false),
                "source inspection retains the document's unsaved state"
            );
        }
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("editor");
        };
        assert_eq!(
            editor.markdown.as_ref().unwrap().parsed,
            crate::native_markdown::parse(edited.text.as_ref())
        );
    }
    assert_eq!(
        state(&app, view)
            .buffer
            .as_ref()
            .unwrap()
            .primary_selection(),
        &selection
    );
    let held = app.hold_document_save_for_test(document);
    assert_eq!(source_status(&app), (true, true));
    app.land_held_document_save_for_test(document, view, held)
        .unwrap();
    assert_eq!(source_status(&app), (false, false));
    command(&mut app, 'z');
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        original
    );
}

#[test]
fn read_rejects_hidden_editor_mutations_and_can_save_the_unsaved_buffer() {
    let (mut app, _dir, path, view) = open("input.md", "# Base\n");
    let wid = WindowId(0);
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    command(&mut app, 'e');
    app.dispatch_native_event(wid, AppEvent::InsertText("draft ".to_string()))
        .unwrap();
    command(&mut app, 'e');
    let before = app.document_store.snapshot(document).unwrap();
    let selection = state(&app, view)
        .buffer
        .as_ref()
        .unwrap()
        .primary_selection()
        .clone();
    for event in [
        InputEvent::Text("keyboard".to_string()),
        InputEvent::Paste("clipboard".to_string(), PasteFraming::AtDrain),
    ] {
        assert!(app.native_input_event(wid, &event));
    }
    for event in [
        AppEvent::InsertText("stale paste".to_string()),
        AppEvent::TextInput(TextInputEvent::Commit("late IME".to_string())),
        AppEvent::TextInput(TextInputEvent::Preedit {
            text: "marked".to_string(),
            selection: Some(0..6),
        }),
    ] {
        app.dispatch_native_event(wid, event).unwrap();
    }
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!((after.seq, after.text), (before.seq, before.text.clone()));
    assert_eq!(
        state(&app, view)
            .buffer
            .as_ref()
            .unwrap()
            .primary_selection(),
        &selection
    );
    assert!(state(&app, view).preedit.is_empty());
    command(&mut app, 's');
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before.text.as_ref()
    );
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert_eq!(
        state(&app, view).markdown.as_ref().unwrap().mode,
        MarkdownWorkspaceMode::Read
    );
}

#[test]
fn split_wheel_scrolls_the_pointed_pane_without_changing_keyboard_focus() {
    let source = (0..90)
        .map(|n| format!("## Section {n}\n\nA paragraph.\n\n"))
        .collect::<String>();
    let (mut app, _dir, _path, view) = open("scroll.md", &source);
    let wid = WindowId(0);
    action(&mut app, "editor/markdown/split");
    app.compiled_native_ui(wid).unwrap();
    let selection = state(&app, view)
        .buffer
        .as_ref()
        .unwrap()
        .primary_selection()
        .clone();
    let reader_before = state(&app, view).markdown.as_ref().unwrap().reader.clone();
    app.dispatch_native_event(
        wid,
        AppEvent::ScrollAt {
            key: UiKey::new("editor/markdown/reader-pane"),
            lines: 8,
        },
    )
    .unwrap();
    let workspace = state(&app, view).markdown.as_ref().unwrap();
    assert_eq!(workspace.focus, MarkdownPane::Editor);
    assert_ne!(
        (workspace.reader.source_anchor, workspace.reader.visual_row),
        (reader_before.source_anchor, reader_before.visual_row)
    );
    assert_eq!(
        state(&app, view)
            .buffer
            .as_ref()
            .unwrap()
            .primary_selection(),
        &selection
    );
    action(&mut app, "editor/markdown/focus-reader");
    let reader_before = state(&app, view).markdown.as_ref().unwrap().reader.clone();
    app.dispatch_native_event(
        wid,
        AppEvent::ScrollAt {
            key: UiKey::new("editor/buffer"),
            lines: 5,
        },
    )
    .unwrap();
    let workspace = state(&app, view).markdown.as_ref().unwrap();
    assert_eq!(workspace.focus, MarkdownPane::Reader);
    assert_eq!(
        (workspace.reader.source_anchor, workspace.reader.visual_row),
        (reader_before.source_anchor, reader_before.visual_row)
    );
    command(&mut app, 'e');
    let workspace = state(&app, view).markdown.as_ref().unwrap();
    assert_eq!(workspace.mode, MarkdownWorkspaceMode::Split);
    assert_eq!(workspace.focus, MarkdownPane::Editor);
}

#[test]
fn find_again_from_read_reveals_the_selected_source_match() {
    let (mut app, _dir, _path, view) = open("find.md", "# needle\n\nAnother needle.\n");
    let wid = WindowId(0);
    app.find_requested_in(wid);
    assert_eq!(
        state(&app, view).markdown.as_ref().unwrap().mode,
        MarkdownWorkspaceMode::Edit
    );
    assert!(app.native_input_event(wid, &InputEvent::Text("needle".to_string())));
    assert!(app.native_input_event(
        wid,
        &InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        }
    ));
    let first = state(&app, view)
        .buffer
        .as_ref()
        .unwrap()
        .primary_selection()
        .clone();
    action(&mut app, "editor/markdown/read");
    app.search_find_again(true).unwrap();
    assert_eq!(
        state(&app, view).markdown.as_ref().unwrap().mode,
        MarkdownWorkspaceMode::Edit
    );
    assert_ne!(
        state(&app, view)
            .buffer
            .as_ref()
            .unwrap()
            .primary_selection(),
        &first
    );
    action(&mut app, "editor/markdown/split");
    action(&mut app, "editor/markdown/focus-reader");
    app.search_find_again(false).unwrap();
    let workspace = state(&app, view).markdown.as_ref().unwrap();
    assert_eq!(workspace.mode, MarkdownWorkspaceMode::Split);
    assert_eq!(workspace.focus, MarkdownPane::Editor);
    assert_eq!(
        state(&app, view)
            .buffer
            .as_ref()
            .unwrap()
            .primary_selection(),
        &first
    );
}

#[test]
fn read_and_split_keep_editor_wait_and_document_read_live_until_finish() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("ticket.md");
    std::fs::write(&path, "# Base\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = super::tests::app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let (ticket, opened) = edit(&mut app, sid, request(&uri, None));
    assert!(opened.starts_with("OK "), "{opened}");
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let (_, ticket_answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    let (_, uri_answer) = wait(&mut app, AwaitKey::Uri(uri.clone()));
    app.dispatch_native_event(wid, AppEvent::InsertText("draft ".to_string()))
        .unwrap();
    let before = app.document_store.snapshot(document).unwrap();
    for mode in [
        "editor/markdown/read",
        "editor/markdown/split",
        "editor/markdown/edit",
        "editor/markdown/read",
    ] {
        action(&mut app, mode);
        assert_eq!(app.active_native_view(wid), Some((instance, view)));
        assert_eq!(ticket_answer.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(uri_answer.try_recv(), Err(TryRecvError::Empty));
        // The wire parser adds canonical spellings, including macOS /var aliases.
        // Exercise that public boundary rather than constructing an incomplete URI query.
        let mut read_live = false;
        let reply = crate::control_doc::serve_doc(&format!("read {uri}"), &mut |query| {
            let answer = app.doc_answer(query, Instant::now());
            let DocAnswer::Text(live) = &answer else {
                panic!("Read mode must expose the live editor document: {answer:?}");
            };
            assert_eq!(live.text, before.text);
            read_live = true;
            Ok(answer)
        });
        assert!(
            read_live,
            "the read reached the canonical document: {reply}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Base\n");
    }
    app.finish_edit_view(view);
    for answer in [&ticket_answer, &uri_answer] {
        let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
        assert_eq!(
            (outcome.result, outcome.saved),
            (EditResult::Finished, true)
        );
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before.text.as_ref()
    );
}
