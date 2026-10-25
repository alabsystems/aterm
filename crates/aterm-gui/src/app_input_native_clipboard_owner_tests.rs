// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use amacs::editor::{EditorCommand, Minibuffer};

use crate::input::{InputEvent, InputOutcome, Source};
use crate::native_app::{AppEvent, AppKind, AppViewState};
use crate::{App, WindowId};

fn editor(
    app: &mut App,
    wid: WindowId,
    path: &std::path::Path,
) -> (crate::tab_model::ViewId, crate::document_store::DocumentId) {
    std::fs::write(path, "needle one\nneedle two\n").unwrap();
    app.open_document_tab_in_window(
        wid,
        AppKind::Editor,
        &crate::native_document_host::path_to_file_uri(path).unwrap(),
    )
    .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    (view, app.native_runtime.document_id(instance).unwrap())
}

fn buffer(app: &App, view: crate::tab_model::ViewId) -> &crate::native_editor::EditorBufferView {
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor view");
    };
    state.buffer.as_ref().unwrap()
}

fn command(app: &mut App, wid: WindowId, command: EditorCommand) {
    app.dispatch_native_event(wid, AppEvent::EditorCommand(command))
        .unwrap();
}

fn text(app: &mut App, wid: WindowId, text: &str) {
    assert_eq!(
        app.input(wid, InputEvent::Text(text.to_string()), Source::Human),
        InputOutcome::Ok
    );
}

fn enter(app: &mut App, wid: WindowId) {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
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
}

#[test]
fn edit_selection_commands_use_native_documents_without_editing_them() {
    for kind in [AppKind::Editor, AppKind::Markdown] {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let path = dir.path().join("source.md");
        let source = "# Héllo 🦀\n\nExact source, including **markup**.\n";
        std::fs::write(&path, source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let terminal = app.pool.get(0).unwrap().term.clone();
        app.open_document_tab(
            kind,
            &crate::native_document_host::path_to_file_uri(&path).unwrap(),
        )
        .unwrap();
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let before = app.document_store.snapshot(document).unwrap();
        crate::control::pbcopy("keep clipboard");
        assert!(!app.copy_selection());
        assert_eq!(crate::control::pbpaste().as_deref(), Some("keep clipboard"));

        app.select_all();
        assert_eq!(app.native_selection_text(wid).as_deref(), Some(source));
        assert!(app.copy_selection());
        assert_eq!(crate::control::pbpaste().as_deref(), Some(source));
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!((after.seq, after.text), (before.seq, before.text));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        assert!(crate::term_lock(&terminal).selection_to_string().is_none());
    }
}

#[test]
fn native_copy_readiness_uses_the_same_valid_unicode_ranges_as_copy() {
    use crate::native_editor::Selection;
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = dir.path().join("unicode.txt");
    let source = "αβ\nneedle 🦀\n";
    std::fs::write(&path, source).unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(
        AppKind::Editor,
        &crate::native_document_host::path_to_file_uri(&path).unwrap(),
    )
    .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let before = app.document_store.snapshot(document).unwrap();
    for (ranges, expected) in [
        (vec![0..4, 12..16], Some("αβ\n🦀")),
        (vec![1..4, 0..usize::MAX], None),
        (vec![1..4, 5..11, 0..usize::MAX], Some("needle")),
        (vec![0..0, 4..4], None),
    ] {
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
            unreachable!();
        };
        state.buffer.as_mut().unwrap().selections = ranges
            .into_iter()
            .map(|range| Selection {
                anchor: range.start,
                head: range.end,
            })
            .collect();
        crate::control::pbcopy("keep clipboard");
        assert_eq!(app.native_has_selection(wid), expected.is_some());
        assert_eq!(app.native_selection_text(wid).as_deref(), expected);
        assert_eq!(app.copy_selection(), expected.is_some());
        assert_eq!(
            crate::control::pbpaste().as_deref(),
            expected.or(Some("keep clipboard"))
        );
    }
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!((after.seq, after.text), (before.seq, before.text));
}

#[test]
fn copy_in_a_sibling_editor_tracks_every_edit_of_a_keyboard_macro() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = dir.path().join("shared.txt");
    let mut app = App::headless_for_test();
    let (a, b) = (WindowId(0), WindowId(1));
    let (_, document) = editor(&mut app, a, &path);
    app.install_window_state(b, crate::stub_session(99), 24, 80);
    let (other_view, shared) = editor(&mut app, b, &path);
    assert_eq!(document, shared);
    command(&mut app, a, EditorCommand::StartMacro);
    text(&mut app, a, "x");
    text(&mut app, a, "y");
    command(&mut app, a, EditorCommand::EndMacro);
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(other_view) else {
        unreachable!();
    };
    state.buffer.as_mut().unwrap().selections =
        vec![crate::native_editor::Selection { anchor: 2, head: 8 }];
    assert!(app.copy_selection_in(b));
    assert_eq!(crate::control::pbpaste().as_deref(), Some("needle"));
    command(&mut app, a, EditorCommand::BeginningOfBuffer);
    command(&mut app, a, EditorCommand::PlayMacro);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "xyxyneedle one\nneedle two\n"
    );
    assert_eq!(buffer(&app, other_view).primary_selection().range(), 4..10);
    assert!(app.copy_selection_in(b));
    assert_eq!(crate::control::pbpaste().as_deref(), Some("needle"));
}

#[test]
fn select_all_keeps_its_event_window_when_another_editor_is_frontmost() {
    for native in [false, true] {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let mut app = App::headless_for_test();
        let (a, b) = (WindowId(0), WindowId(1));
        let first = if native {
            Some(editor(&mut app, a, &dir.path().join("a.txt")))
        } else {
            crate::term_lock(&app.pool.get(0).unwrap().term).process(b"original terminal");
            None
        };
        app.install_window_state(b, crate::stub_session(99), 24, 80);
        let (other_view, other_document) = editor(&mut app, b, &dir.path().join("b.txt"));
        let before = app.document_store.snapshot(other_document).unwrap();
        let other_selection = buffer(&app, other_view).selections.clone();
        assert_eq!(app.frontmost_window, Some(b));
        app.dispatch_action(a, crate::keybinding::Action::SelectAll);
        assert!(app.copy_selection_in(a));
        if let Some((_, document)) = first {
            assert_eq!(
                crate::control::pbpaste().unwrap().as_str(),
                app.document_store.snapshot(document).unwrap().text.as_ref()
            );
        } else {
            assert!(
                crate::control::pbpaste()
                    .unwrap()
                    .contains("original terminal")
            );
        }
        assert_eq!(buffer(&app, other_view).selections, other_selection);
        let after = app.document_store.snapshot(other_document).unwrap();
        assert_eq!((after.seq, after.text), (before.seq, before.text));
        assert_eq!(app.frontmost_window, Some(b));
    }
}

#[test]
fn copy_and_select_all_leave_the_document_behind_an_editor_prompt_alone() {
    for (prompt, query) in [
        (EditorCommand::GotoLine, "2"),
        (EditorCommand::ExecuteCommand, "undo"),
        (EditorCommand::IncrementalSearch, "needle"),
    ] {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (view, document) = editor(&mut app, wid, &dir.path().join("one.txt"));
        app.select_all();
        assert!(
            app.copy_selection(),
            "negative control: the body owns a selection"
        );
        command(&mut app, wid, prompt);
        text(&mut app, wid, query);
        let before = app.document_store.snapshot(document).unwrap();
        let selection = buffer(&app, view).selections.clone();
        let minibuffer = format!("{:?}", buffer(&app, view).minibuffer);
        crate::control::pbcopy("keep clipboard");
        assert!(!app.copy_selection());
        app.select_all();
        assert_eq!(crate::control::pbpaste().as_deref(), Some("keep clipboard"));
        assert_eq!(buffer(&app, view).selections, selection);
        assert_eq!(format!("{:?}", buffer(&app, view).minibuffer), minibuffer);
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!((after.seq, after.text), (before.seq, before.text));
        command(&mut app, wid, EditorCommand::Abort);
        assert!(!buffer(&app, view).minibuffer_active());
        app.select_all();
        assert!(
            app.copy_selection(),
            "cancelling returns ownership to the body"
        );
    }
}

#[test]
fn selection_commands_cannot_reach_a_document_behind_an_overlay() {
    for overview in [false, true] {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (view, document) = editor(&mut app, wid, &dir.path().join("one.txt"));
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
            unreachable!();
        };
        state.buffer.as_mut().unwrap().selections =
            vec![crate::native_editor::Selection { anchor: 0, head: 6 }];
        assert!(
            app.copy_selection(),
            "negative control: a real selection is available"
        );
        if overview {
            app.open_overview().unwrap();
        } else {
            app.palette_enter();
            text(&mut app, wid, "filter");
        }
        let before = app.document_store.snapshot(document).unwrap();
        let selection = buffer(&app, view).selections.clone();
        crate::control::pbcopy("keep clipboard");
        assert!(!app.copy_selection());
        app.select_all();
        assert_eq!(crate::control::pbpaste().as_deref(), Some("keep clipboard"));
        assert_eq!(buffer(&app, view).selections, selection);
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!((after.seq, after.text), (before.seq, before.text));
        if !overview {
            assert_eq!(app.windows[&wid].palette().unwrap().query(), "filter");
        }
    }
}

#[test]
fn delayed_native_paste_keeps_the_same_field_through_typing_and_other_window_focus() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let mut app = App::headless_for_test();
    let (a, b) = (WindowId(0), WindowId(1));
    let (view, document) = editor(&mut app, a, &dir.path().join("one.txt"));
    let owner = app.clipboard_recipient(a).unwrap();
    app.complete_clipboard_paste_in(a, owner, "prefix ".to_string(), Source::Human);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "prefix needle one\nneedle two\n"
    );

    app.find_requested_in(a);
    let owner = app.clipboard_recipient(a).unwrap();
    text(&mut app, a, "n");
    app.install_window_state(b, crate::stub_session(99), 24, 80);
    assert_eq!(app.frontmost_window, Some(b));
    let before = app.document_store.snapshot(document).unwrap();
    let other_sink = app.pool.get(99).unwrap().ctx.sink.clone();
    let other_epoch = other_sink.input_epoch();
    app.complete_clipboard_paste_in(a, owner, "eedle".to_string(), Source::Human);
    assert!(
        matches!(&buffer(&app, view).minibuffer, Minibuffer::Search { query, .. } if query == "needle")
    );
    assert!(!buffer(&app, view).primary_selection().is_caret());
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!(after.text, before.text);
    assert_eq!(after.seq, before.seq);
    assert_eq!(other_sink.input_epoch(), other_epoch);
}

#[test]
fn delayed_native_paste_cannot_follow_a_retired_editor_input_surface() {
    for transition in [
        "cancel",
        "reopen",
        "accept",
        "goto-line",
        "generic-prompt",
        "native-away-back",
        "terminal",
        "palette",
        "palette-away-back",
    ] {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (view, document) = editor(&mut app, wid, &dir.path().join("one.txt"));
        app.find_requested_in(wid);
        text(&mut app, wid, "n");
        let owner = app.clipboard_recipient(wid).unwrap();
        match transition {
            "cancel" => command(&mut app, wid, EditorCommand::Abort),
            "reopen" => {
                command(&mut app, wid, EditorCommand::Abort);
                app.find_requested_in(wid);
                text(&mut app, wid, "n");
            }
            "accept" => enter(&mut app, wid),
            "goto-line" => {
                command(&mut app, wid, EditorCommand::Abort);
                command(&mut app, wid, EditorCommand::GotoLine);
                text(&mut app, wid, "2");
            }
            "generic-prompt" => {
                command(&mut app, wid, EditorCommand::Abort);
                command(&mut app, wid, EditorCommand::ExecuteCommand);
                text(&mut app, wid, "kill-buffer");
                enter(&mut app, wid);
                assert!(matches!(
                    buffer(&app, view).minibuffer,
                    Minibuffer::Prompt { .. }
                ));
            }
            "native-away-back" => {
                assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
                app.switch_tab_in(wid, 1);
                assert_eq!(app.active_native_view(wid).unwrap().1, view);
            }
            "terminal" => app.switch_tab_in(wid, 0),
            "palette" => app.palette_enter(),
            "palette-away-back" => {
                app.palette_enter();
                app.palette_exit();
            }
            _ => unreachable!(),
        }
        let before = app.document_store.snapshot(document).unwrap();
        let prompt_before = buffer(&app, view).minibuffer.clone();
        let palette_before = app.windows[&wid]
            .palette()
            .map(|state| state.query().to_string());
        let terminal_sink = app.pool.get(0).unwrap().ctx.sink.clone();
        let terminal_epoch = terminal_sink.input_epoch();
        app.complete_clipboard_paste_in(wid, owner, "MUST-NOT-PASTE".to_string(), Source::Human);
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.text, before.text, "{transition}");
        assert_eq!(after.seq, before.seq, "{transition}");
        assert_eq!(buffer(&app, view).minibuffer, prompt_before, "{transition}");
        assert_eq!(
            app.windows[&wid]
                .palette()
                .map(|state| state.query().to_string()),
            palette_before,
            "{transition}"
        );
        assert_eq!(
            terminal_sink.input_epoch(),
            terminal_epoch,
            "{transition}: no PTY input attempt"
        );
    }
}

#[test]
fn delayed_palette_paste_accepts_the_original_overlay_but_not_its_replacement() {
    for native in [false, true] {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let document = native.then(|| editor(&mut app, wid, &dir.path().join("one.txt")).1);
        let before = document.map(|document| app.document_store.snapshot(document).unwrap());
        let sink = app.pool.get(0).unwrap().ctx.sink.clone();
        let epoch = sink.input_epoch();
        app.palette_enter();
        let owner = app.clipboard_recipient(wid).unwrap();
        text(&mut app, wid, "fi");
        app.complete_clipboard_paste_in(wid, owner, "nd".to_string(), Source::Human);
        assert_eq!(app.windows[&wid].palette().unwrap().query(), "find");
        let owner = app.clipboard_recipient(wid).unwrap();
        app.palette_exit();
        app.palette_enter();
        app.complete_clipboard_paste_in(wid, owner, "MUST-NOT-PASTE".to_string(), Source::Human);
        assert_eq!(app.windows[&wid].palette().unwrap().query(), "");
        assert_eq!(
            sink.input_epoch(),
            epoch,
            "paste never attempts hidden PTY input"
        );
        if let Some(document) = document {
            let after = app.document_store.snapshot(document).unwrap();
            assert_eq!(after.text, before.as_ref().unwrap().text);
            assert_eq!(after.seq, before.as_ref().unwrap().seq);
        }
    }
}

#[test]
fn edit_paste_follows_the_palette_above_terminal_find_and_overview_swallows_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sink = app.pool.get(0).unwrap().ctx.sink.clone();
    app.search_enter();
    app.search_edit_in(
        wid,
        crate::app_search::SearchEdit::Insert("held".to_string()),
    );
    app.palette_enter();
    assert_eq!(
        app.find_field_window(),
        Some(wid),
        "the hidden terminal Find is still open"
    );
    let epoch = sink.input_epoch();
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("find\nnext".to_string()));
    app.paste_into_front_field();
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
    assert_eq!(app.windows[&wid].palette().unwrap().query(), "findnext");
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().query, "held");
    assert_eq!(sink.input_epoch(), epoch);
    assert!(
        app.paste_banner.is_none(),
        "overlay text does not ask about terminal multiline execution"
    );
    app.palette_exit();
    assert_eq!(app.find_field_window(), Some(wid));

    app.open_overview().unwrap();
    assert_eq!(app.find_field_window(), Some(wid));
    assert!(app.clipboard_recipient(wid).is_none());
    crate::control::PBPASTE_STUB
        .with(|board| *board.borrow_mut() = Some("MUST-NOT-PASTE".to_string()));
    app.paste_into_front_field();
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
    assert!(app.windows[&wid].overview().is_some());
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().query, "held");
    assert_eq!(sink.input_epoch(), epoch);
}

#[test]
fn delayed_settings_paste_is_bound_to_the_original_search_field_lifetime() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.find_requested_in(wid);
    let (_, view) = app.active_native_view(wid).unwrap();
    let query = |app: &App| {
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
            panic!("Settings");
        };
        state.search_input.value().to_string()
    };
    let owner = app.clipboard_recipient(wid).unwrap();
    text(&mut app, wid, "fo");
    app.complete_clipboard_paste_in(wid, owner, "nt".to_string(), Source::Human);
    assert_eq!(query(&app), "font");
    let owner = app.clipboard_recipient(wid).unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search-cancel"),
            value: None,
        }),
    )
    .unwrap();
    app.find_requested_in(wid);
    app.complete_clipboard_paste_in(wid, owner, "MUST-NOT-PASTE".to_string(), Source::Human);
    assert_eq!(query(&app), "");
}

#[test]
fn delayed_settings_paste_cannot_recreate_a_cancelled_draft_at_the_same_focus_key() {
    use crate::native_app::TextInputEvent;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let key = crate::prefs::EDIT_SHELL;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Terminal));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(format!("settings/set/{key}")),
            value: None,
        }),
    )
    .unwrap();
    app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::SelectAll))
        .unwrap();
    text(&mut app, wid, "/pending");
    let (_, view) = app.active_native_view(wid).unwrap();
    let settings = |app: &App| {
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
            panic!("Settings");
        };
        (
            state.common.last_focus.clone(),
            state.editing_field.clone(),
            state
                .field_inputs
                .get(key)
                .map(|input| input.value().to_string()),
        )
    };
    let owner = app.clipboard_recipient(wid).unwrap();
    text(&mut app, wid, "-kept");
    app.complete_clipboard_paste_in(wid, owner, "-paste".to_string(), Source::Human);
    assert_eq!(settings(&app).2.as_deref(), Some("/pending-kept-paste"));

    let owner = app.clipboard_recipient(wid).unwrap();
    let focus = settings(&app).0;
    app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::Cancel))
        .unwrap();
    assert_eq!(
        settings(&app),
        (focus, None, None),
        "cancellation keeps the control focused but removes its draft"
    );
    app.complete_clipboard_paste_in(wid, owner, "MUST-NOT-PASTE".to_string(), Source::Human);
    assert_eq!(settings(&app).1, None);
    assert_eq!(settings(&app).2, None);

    let fresh = app.clipboard_recipient(wid).unwrap();
    app.complete_clipboard_paste_in(wid, fresh, "fresh".to_string(), Source::Human);
    assert_eq!(settings(&app).1.as_deref(), Some(key));
    assert!(settings(&app).2.unwrap().ends_with("fresh"));
}

#[cfg(unix)]
#[test]
fn physical_and_configured_paste_use_the_event_window_for_editor_and_terminal() {
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{
        Key as WinitKey, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
    };
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    for native in [false, true] {
        for route in ["physical", "configured", "selection"] {
            if (route == "physical" || route == "selection") && cfg!(target_os = "linux") {
                continue;
            }
            let dir = aterm_tempfile::TempDir::new().unwrap();
            let (mut reader, writer) = UnixStream::pair().unwrap();
            reader.set_nonblocking(true).unwrap();
            let sink = Arc::new(aterm_session::sink::SinkWriter::new(writer.as_raw_fd()));
            let mut app = App::headless_for_test_with_sink(Arc::clone(&sink));
            let (a, b) = (WindowId(0), WindowId(1));
            let original = native.then(|| editor(&mut app, a, &dir.path().join("one.txt")));
            app.install_window_state(b, crate::stub_session(99), 24, 80);
            let (_, other_document) = editor(&mut app, b, &dir.path().join("two.txt"));
            let other_before = app.document_store.snapshot(other_document).unwrap();
            assert_eq!(app.frontmost_window, Some(b));
            app.keybindings = crate::keybinding::Keybindings::resolved(Some(
                &[
                    ("f2".to_string(), "paste".to_string()),
                    ("f3".to_string(), "paste_selection".to_string()),
                ]
                .into_iter()
                .collect(),
            ));
            let (code, key, mods) = match route {
                "physical" => (
                    KeyCode::KeyV,
                    WinitKey::Character(SmolStr::new("v")),
                    ModifiersState::SUPER,
                ),
                "configured" => (
                    KeyCode::F2,
                    WinitKey::Named(NamedKey::F2),
                    ModifiersState::empty(),
                ),
                "selection" => (
                    KeyCode::F3,
                    WinitKey::Named(NamedKey::F3),
                    ModifiersState::empty(),
                ),
                _ => unreachable!(),
            };
            crate::control::PBPASTE_STUB
                .with(|board| *board.borrow_mut() = Some("CLIP".to_string()));
            app.windows.get_mut(&a).unwrap().mods = mods;
            for state in [ElementState::Pressed, ElementState::Released] {
                app.on_key(
                    a,
                    KeyEvent::synthetic_for_test(
                        PhysicalKey::Code(code),
                        key.clone(),
                        None,
                        KeyLocation::Standard,
                        state,
                        false,
                    ),
                );
            }
            crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
            crate::app_input::paste_order::await_drain_for_test(&sink);
            if let Some((_, document)) = original {
                assert_eq!(
                    app.document_store.snapshot(document).unwrap().text.as_ref(),
                    "CLIPneedle one\nneedle two\n",
                    "{route}: original editor"
                );
                let mut bytes = [0; 32];
                assert!(
                    matches!(reader.read(&mut bytes), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                    "native paste never reaches PTY"
                );
            } else {
                let mut bytes = [0; 32];
                let n = reader
                    .read(&mut bytes)
                    .expect("terminal received the paste");
                assert_eq!(&bytes[..n], b"CLIP", "{route}: original terminal");
            }
            let other_after = app.document_store.snapshot(other_document).unwrap();
            assert_eq!(
                other_after.text, other_before.text,
                "{route}: unrelated editor"
            );
            assert_eq!(
                other_after.seq, other_before.seq,
                "{route}: unrelated editor"
            );
            assert_eq!(app.frontmost_window, Some(b));
        }
    }
}
