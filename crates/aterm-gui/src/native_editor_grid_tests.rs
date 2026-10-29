// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native Editor's grid face end to end (docs/DESIGN-amacs-grid-2026-10-28.md, slice G1,
//! test T8): a `.csv` or `.tsv` file opens as a grid, a text file as text, a table too large for
//! the grid as text with the reason on its status line; and the owner's edit (the last cell of a
//! row, a correction with a comma and quotes, Return, ⌘S) changes on disk only that cell's bytes,
//! a BOM and CRLF line ends held, ⌘Z takes it back and ⌘E flips to the text. Every table here is
//! synthetic, of the owner's file's shape: no test reads a real file.

use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

use crate::WindowId;
use crate::input::InputEvent;
use crate::native_app::{AppEvent, AppKind, AppViewState, TextInputEvent};

const HEADER: &str = "computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction (leave blank if my guess is right)";

/// The owner's shape with a BOM and CRLF line ends throughout, a line break (CRLF too) inside a
/// quoted cell, a short row, and no final line end.
fn crlf_table() -> Vec<u8> {
    let mut bytes = "\u{feff}".as_bytes().to_vec();
    bytes.extend_from_slice(
        [
            HEADER,
            "m1,alice@example.test,Claude,email,\"The sign-in page said \"\"check your inbox\"\", so email.\",",
            "m2,bob@example.test,\"Claude, GitHub\",SMS,\"Two reasons: first\r\nthe number is on file\",",
            "m3,carol@example.test,Claude",
            "m4,dave@example.test,Claude,authenticator,\"Codes by email, they said\",",
        ]
        .join("\r\n")
        .as_bytes(),
    );
    bytes
}

/// CRLF records with an LF inside a quoted cell: line ends no reader holds, kept as typed.
const MIXED: &str = "name,note,fix\r\nm1,\"two\nlines\",\r\nm2,plain,\r\n";

struct Scratch {
    _dir: aterm_tempfile::TempDir,
    path: std::path::PathBuf,
}

fn scratch(name: &str, bytes: &[u8]) -> Scratch {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-grid-")
        .tempdir()
        .unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    Scratch { _dir: dir, path }
}

fn open(app: &mut crate::App, path: &std::path::Path) -> crate::tab_model::ViewId {
    let uri = crate::native_document_host::path_to_file_uri(path).unwrap();
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    app.active_native_view(WindowId(0)).unwrap().1
}

fn state(app: &crate::App, view: crate::tab_model::ViewId) -> &crate::native_app::EditorViewState {
    match app.native_runtime.view_state(view) {
        Some(AppViewState::Editor(state)) => state,
        _ => panic!("an Editor view"),
    }
}

fn text(app: &crate::App, view: crate::tab_model::ViewId) -> String {
    let instance = match app.view_store.get(view) {
        Some(crate::tab_model::View::Native(native)) => native.instance,
        _ => panic!("a native view"),
    };
    let document = app.native_runtime.document_id(instance).unwrap();
    app.document_store
        .snapshot(document)
        .unwrap()
        .text
        .to_string()
}

fn key(app: &mut crate::App, key: Key, mods: Modifiers) {
    let event = InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    assert!(app.native_input_event(WindowId(0), &event), "{event:?}");
}

fn named(app: &mut crate::App, named: NamedKey) {
    key(app, Key::Named(named), Modifiers::empty());
}

fn command(app: &mut crate::App, letter: char) {
    key(app, Key::Character(letter), Modifiers::SUPER);
}

fn type_text(app: &mut crate::App, typed: &str) {
    app.dispatch_native_event(
        WindowId(0),
        AppEvent::TextInput(TextInputEvent::Commit(typed.to_string())),
    )
    .unwrap();
}

/// The current cell (record, column) of a grid view.
fn cell(app: &crate::App, view: crate::tab_model::ViewId) -> (usize, usize) {
    let buffer = state(app, view).buffer.as_ref().unwrap();
    let delimiter = buffer.grid().expect("a grid").delimiter;
    let table = amacs::table::Table::scan(&text(app, view), delimiter).unwrap();
    crate::native_editor::current_cell(&table, buffer)
}

fn grid_shown(app: &crate::App, view: crate::tab_model::ViewId) -> bool {
    state(app, view)
        .buffer
        .as_ref()
        .is_some_and(|buffer| buffer.grid().is_some())
}

/// A cell opened after an update's initial preparation is outside its durable document. The
/// final check keeps it in its view, even behind another tab, until Return or Escape resolves it.
#[cfg(unix)]
#[test]
fn a_cell_changed_after_update_preparation_blocks_final_handoff() {
    use crate::app_tabs::ClosePreflightVisibility;

    for (dirty, resolve) in [
        (false, NamedKey::Enter),
        (false, NamedKey::Escape),
        (true, NamedKey::Enter),
        (true, NamedKey::Escape),
    ] {
        let csv = scratch("late-cell.csv", b"name,note\nm1,old\n");
        let plain = scratch("control.txt", b"ordinary text\n");
        let mut app = crate::App::headless_for_test();
        app.use_document_journal_root_for_test(csv._dir.path().join("drafts"));
        app.carry_drafts_as_windowed_for_test();
        let text_view = open(&mut app, &plain.path);
        let view = open(&mut app, &csv.path);
        let document = state(&app, view).buffer.as_ref().unwrap().document;
        named(&mut app, NamedKey::ArrowDown);
        named(&mut app, NamedKey::ArrowRight);
        let original = if dirty {
            type_text(&mut app, "journaled");
            named(&mut app, NamedKey::Enter);
            assert!(matches!(
                app.draft_carry(document),
                crate::app_documents::DraftCarry::Carried { .. }
            ));
            "journaled"
        } else {
            "old"
        };

        // This is the real initial preparation shared by automatic and manual updates.
        let scope = app.update_close_scope();
        assert!(
            app.prepare_all_native_shutdown(scope, ClosePreflightVisibility::Quiet)
                .unwrap()
        );
        assert!(app.revalidate_native_update_safety().is_ok());
        let before = app.document_store.snapshot(document).unwrap();
        let baseline_dirty = app.document_store.dirty(document);
        assert_eq!(baseline_dirty, Some(dirty));

        // Merely opening a cell loses no value on restore and must remain admissible.
        named(&mut app, NamedKey::Enter);
        assert!(app.revalidate_native_update_safety().is_ok());
        assert!(
            state(&app, view)
                .buffer
                .as_ref()
                .unwrap()
                .grid_draft()
                .is_some()
        );
        type_text(&mut app, " changed");
        let draft_text = format!("{original} changed");
        assert_eq!(
            state(&app, view)
                .buffer
                .as_ref()
                .unwrap()
                .grid_draft()
                .unwrap()
                .text,
            draft_text
        );
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, before.seq);
        assert_eq!(after.text, before.text);
        assert_eq!(app.document_store.dirty(document), baseline_dirty);

        // The last read-only check must find every view, not just the foreground editor or
        // documents whose sequence advanced. Omitting the cell blocker admits this handoff.
        assert_eq!(open(&mut app, &plain.path), text_view);
        for _ in 0..2 {
            assert!(
                app.revalidate_native_update_safety().is_err(),
                "a changed cell draft is absent from the document journal and restore layout"
            );
            assert_eq!(
                state(&app, view)
                    .buffer
                    .as_ref()
                    .unwrap()
                    .grid_draft()
                    .unwrap()
                    .text,
                draft_text
            );
            let kept = app.document_store.snapshot(document).unwrap();
            assert_eq!(kept.seq, before.seq);
            assert_eq!(kept.text, before.text);
            assert_eq!(app.document_store.dirty(document), baseline_dirty);
        }

        assert_eq!(open(&mut app, &csv.path), view);
        named(&mut app, resolve);
        assert!(
            state(&app, view)
                .buffer
                .as_ref()
                .unwrap()
                .grid_draft()
                .is_none()
        );
        let resolved = app.document_store.snapshot(document).unwrap();
        if resolve == NamedKey::Enter {
            assert_ne!(resolved.seq, before.seq);
            assert_eq!(&*resolved.text, format!("name,note\nm1,{draft_text}\n"));
            assert_eq!(app.document_store.dirty(document), Some(true));
        } else {
            assert_eq!(resolved.seq, before.seq);
            assert_eq!(resolved.text, before.text);
            assert_eq!(app.document_store.dirty(document), baseline_dirty);
        }
        assert!(app.revalidate_native_update_safety().is_ok());

        // A normal text-view edit remains carried by the existing document journal.
        assert_eq!(open(&mut app, &plain.path), text_view);
        type_text(&mut app, "text edit ");
        assert!(!grid_shown(&app, text_view));
        assert!(app.revalidate_native_update_safety().is_ok());
    }
}

#[test]
fn t8_a_table_opens_as_a_grid_and_a_text_file_as_text() {
    let mut app = crate::App::headless_for_test();
    let csv = scratch("accounts.csv", &crlf_table());
    let view = open(&mut app, &csv.path);
    assert!(grid_shown(&app, view), "a .csv opens as a grid");
    assert_eq!(cell(&app, view), (0, 0));

    let tsv = scratch("accounts.tsv", b"a\tb\n1\t2\n");
    let view = open(&mut app, &tsv.path);
    assert_eq!(
        state(&app, view)
            .buffer
            .as_ref()
            .and_then(|buffer| buffer.grid())
            .map(|grid| grid.delimiter),
        Some(amacs::table::Delimiter::Tab)
    );

    let notes = scratch("notes.txt", b"a,b\n1,2\n");
    let view = open(&mut app, &notes.path);
    assert!(!grid_shown(&app, view), "a text file stays text");
    assert_eq!(state(&app, view).status, None);

    // Past the grid's 4 MiB, a table opens as text and says why.
    let row = "m1,alice@example.test,Claude,email,a long note about codes,\n";
    let big = row.repeat(amacs::table::MAX_TABLE_BYTES / row.len() + 2);
    let large = scratch("large.csv", big.as_bytes());
    let view = open(&mut app, &large.path);
    assert!(!grid_shown(&app, view));
    let status = state(&app, view).status.clone().unwrap_or_default();
    assert!(status.starts_with("Opened as text"), "{status}");
}

#[test]
fn t8_the_owners_edit_changes_only_that_cell_on_disk() {
    let mut app = crate::App::headless_for_test();
    let original = crlf_table();
    let csv = scratch("accounts.csv", &original);
    let view = open(&mut app, &csv.path);
    let opened = text(&app, view);

    // Row 3 (the header is row 1), its last column, empty.
    named(&mut app, NamedKey::ArrowDown);
    named(&mut app, NamedKey::ArrowDown);
    named(&mut app, NamedKey::End);
    assert_eq!(cell(&app, view), (2, 5));
    type_text(&mut app, "fix, \"quoted\"");
    assert!(
        state(&app, view)
            .buffer
            .as_ref()
            .and_then(|buffer| buffer.grid_draft())
            .is_some_and(|draft| draft.text == "fix, \"quoted\""),
        "typing opens the cell editor"
    );
    assert_eq!(text(&app, view), opened, "nothing lands before Return");
    named(&mut app, NamedKey::Enter);
    assert_eq!(cell(&app, view), (3, 5), "Return moves down");
    command(&mut app, 's');

    let saved = std::fs::read(&csv.path).unwrap();
    let cell_end = {
        let row3 = b"the number is on file\",";
        let at = original
            .windows(row3.len())
            .position(|window| window == row3)
            .unwrap();
        at + row3.len()
    };
    let mut expected = original[..cell_end].to_vec();
    expected.extend_from_slice(b"\"fix, \"\"quoted\"\"\"");
    expected.extend_from_slice(&original[cell_end..]);
    assert_eq!(
        String::from_utf8_lossy(&saved),
        String::from_utf8_lossy(&expected),
        "only the cell's bytes change; the BOM and every CRLF stay"
    );

    // ⌘Z takes the edit back and puts the cursor on its cell.
    command(&mut app, 'z');
    assert_eq!(text(&app, view), opened);
    assert_eq!(cell(&app, view), (2, 5));

    // ⌘E shows the text, and again the grid.
    command(&mut app, 'e');
    assert!(!grid_shown(&app, view));
    command(&mut app, 'e');
    assert!(grid_shown(&app, view));
}

#[test]
fn t8_a_mixed_line_end_table_round_trips_byte_exact() {
    let mut app = crate::App::headless_for_test();
    let csv = scratch("mixed.csv", MIXED.as_bytes());
    let view = open(&mut app, &csv.path);
    assert!(grid_shown(&app, view));
    named(&mut app, NamedKey::ArrowDown);
    named(&mut app, NamedKey::End);
    assert_eq!(cell(&app, view), (1, 2));
    type_text(&mut app, "a,b");
    named(&mut app, NamedKey::Enter);
    command(&mut app, 's');
    assert_eq!(
        std::fs::read_to_string(&csv.path).unwrap(),
        "name,note,fix\r\nm1,\"two\nlines\",\"a,b\"\r\nm2,plain,\r\n"
    );
}

#[test]
fn t8_closing_with_a_cell_open_commits_it_first() {
    let mut app = crate::App::headless_for_test();
    let csv = scratch("close.csv", b"a,b\n1,\n");
    let view = open(&mut app, &csv.path);
    named(&mut app, NamedKey::ArrowDown);
    named(&mut app, NamedKey::End);
    type_text(&mut app, "two");
    assert!(app.commit_grid_draft_before_close(view));
    assert_eq!(text(&app, view), "a,b\n1,two\n");
    assert!(
        state(&app, view)
            .buffer
            .as_ref()
            .is_some_and(|buffer| buffer.grid_draft().is_none())
    );

    // A cell another writer changed under the draft refuses, and the close with it.
    named(&mut app, NamedKey::ArrowUp);
    type_text(&mut app, "x");
    assert_eq!(cell(&app, view), (0, 1), "the header's \"b\"");
    let instance = match app.view_store.get(view) {
        Some(crate::tab_model::View::Native(native)) => native.instance,
        _ => panic!("a native view"),
    };
    let document = app.native_runtime.document_id(instance).unwrap();
    let snapshot = app.document_store.snapshot(document).unwrap();
    let outcome = app.document_store.transact(
        document,
        snapshot.seq,
        vec![amacs::store::TextEdit {
            range: 2..3,
            insert: "B".to_string(),
        }],
    );
    let amacs::store::DocumentTxnOutcome::Committed { seq, deltas, .. } = outcome else {
        panic!("{outcome:?}");
    };
    // The view hears the other writer, as a peer view's commit is published to it.
    match app.native_runtime.view_state_mut(view) {
        Some(AppViewState::Editor(state)) => state
            .buffer
            .as_mut()
            .unwrap()
            .observe_external(seq, &deltas),
        _ => panic!("an Editor view"),
    }
    assert!(!app.commit_grid_draft_before_close(view));
    let status = state(&app, view).status.clone().unwrap_or_default();
    assert!(status.contains("changed while you edited"), "{status}");
}

#[test]
fn t8_the_footer_and_header_say_where_the_cursor_is() {
    let mut app = crate::App::headless_for_test();
    let csv = scratch("accounts.csv", &crlf_table());
    let _ = open(&mut app, &csv.path);
    named(&mut app, NamedKey::ArrowDown);
    named(&mut app, NamedKey::End);
    let compiled = app.compiled_native_ui(WindowId(0)).unwrap();
    let spec = compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            crate::native_ui::UiContent::TextViewport(spec) => Some(spec),
            _ => None,
        })
        .unwrap();
    assert!(spec.projection.is_none() && spec.grid.is_some());
    assert_eq!(
        spec.cursor_label.as_deref(),
        Some("Row 2 of 5 · your correction (leave blank if my guess is right)")
    );
    let status = spec.semantic_status.clone().unwrap_or_default();
    assert!(
        status.starts_with("Row 2 of 5 · Col 6 of 6 · Ln 2"),
        "{status}"
    );
}

/// The active viewport's grid as the frame draws it, and its footer's words.
fn drawn(app: &mut crate::App) -> (crate::native_editor::GridProjection, String) {
    let compiled = app.compiled_native_ui(WindowId(0)).unwrap();
    let spec = compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            crate::native_ui::UiContent::TextViewport(spec) => Some(spec.clone()),
            _ => None,
        })
        .unwrap();
    (
        *spec.grid.expect("a grid"),
        spec.semantic_status.unwrap_or_default(),
    )
}

/// While a cell is open its text is in no save and no journal, so a leftover `Saved`, `No unsaved
/// changes` or `Draft autosaved` never stands in the footer for it (the review saw all three over
/// typed text); the footer says how to keep or drop the cell. Save is offered for the open cell's
/// text in a clean document, and keeps the cell, then saves.
#[test]
fn an_open_cell_is_never_called_saved_and_save_keeps_it() {
    let mut app = crate::App::headless_for_test();
    let csv = scratch("accounts.csv", &crlf_table());
    let view = open(&mut app, &csv.path);
    named(&mut app, NamedKey::ArrowDown);
    named(&mut app, NamedKey::End);
    assert!(!app.can_save_document(WindowId(0)), "a clean document");
    type_text(&mut app, "late fix");
    for leftover in ["Saved", "No unsaved changes", "Draft autosaved"] {
        app.set_editor_view_status(view, leftover);
        let (_, footer) = drawn(&mut app);
        assert!(!footer.contains(leftover), "{leftover}: {footer}");
        assert!(footer.starts_with("Return keeps · Esc drops"), "{footer}");
    }
    // Another status (a refusal, a reload) still shows, beside the cell's place.
    app.set_editor_view_status(view, "Reloaded changes from disk");
    let (_, footer) = drawn(&mut app);
    assert!(
        footer.starts_with("Reloaded changes from disk · Row 2 of 5"),
        "{footer}"
    );

    assert!(
        app.can_save_document(WindowId(0)),
        "Save is offered for the cell's text"
    );
    app.dispatch_native_event(
        WindowId(0),
        AppEvent::EditorCommand(crate::native_editor::EditorCommand::Save),
    )
    .unwrap();
    let saved = String::from_utf8(std::fs::read(&csv.path).unwrap()).unwrap();
    assert!(
        saved.contains("so email.\",late fix\r\n"),
        "Save kept the cell, then saved: {saved:?}"
    );
    named(&mut app, NamedKey::Escape);
    assert!(!app.can_save_document(WindowId(0)));
}

/// A sideways wheel notch over a grid scrolls it a column, and a trackpad swipe's cells bank
/// toward one; the cursor stays where it was (the review found no mouse way to the columns off
/// to the side).
#[test]
fn a_sideways_wheel_shows_the_columns_off_to_the_side() {
    use aterm_window::dpi::PhysicalPosition;
    use aterm_window::event::MouseScrollDelta;
    let header = (1..=16)
        .map(|n| format!("column number {n}"))
        .collect::<Vec<_>>()
        .join(",");
    let row = (1..=16)
        .map(|n| format!("value {n}@example.test"))
        .collect::<Vec<_>>()
        .join(",");
    let csv = scratch("wide.csv", format!("{header}\n{row}\n{row}\n").as_bytes());
    let mut app = crate::App::headless_for_test();
    let view = open(&mut app, &csv.path);
    named(&mut app, NamedKey::ArrowDown);
    let first_scrolled =
        |grid: &crate::native_editor::GridProjection| grid.shown[usize::from(grid.pinned)].index;
    let (start, _) = drawn(&mut app);
    assert!(start.hidden_right, "{:?}", start.shown);
    assert_eq!(first_scrolled(&start), 0);
    // winit's -x is the reader walking right.
    app.on_mouse_wheel(WindowId(0), MouseScrollDelta::LineDelta(-1.0, 0.0));
    let (right, _) = drawn(&mut app);
    assert!(right.pinned, "{:?}", right.shown);
    assert!(first_scrolled(&right) >= 2, "{:?}", right.shown);
    assert_eq!(cell(&app, view), (1, 0), "the cursor stays");
    app.on_mouse_wheel(WindowId(0), MouseScrollDelta::LineDelta(-2.0, 0.0));
    let (further, _) = drawn(&mut app);
    assert_eq!(first_scrolled(&further), first_scrolled(&right) + 2);
    app.on_mouse_wheel(WindowId(0), MouseScrollDelta::LineDelta(3.0, 0.0));
    assert_eq!(first_scrolled(&drawn(&mut app).0), 0, "and back");
    // A swipe of a few cells moves nothing until it adds up to a column.
    let cell_w = app.win_cell_size(WindowId(0)).0.max(1) as f64;
    app.on_mouse_wheel(
        WindowId(0),
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(-cell_w * 2.0, 0.0)),
    );
    assert_eq!(first_scrolled(&drawn(&mut app).0), 0, "two cells bank");
    app.on_mouse_wheel(
        WindowId(0),
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(-cell_w * 10.0, 0.0)),
    );
    assert!(
        first_scrolled(&drawn(&mut app).0) >= 2,
        "twelve cells scroll"
    );
    assert_eq!(cell(&app, view), (1, 0));
}

/// G2: ⌘C and Edit ▸ Copy take a grid cell's value, never its quoted bytes, to the clipboard and
/// the kill ring, so C-y puts it in another cell; with a cell open they take its text. A control
/// client's ⌘C copies nothing, a cell open or not. *Control (review):* before the open-cell check
/// a control client's ⌘C on an open cell wrote its typed text to the clipboard.
#[test]
fn g2_copy_takes_the_cells_value_and_yank_puts_it_in_another_cell() {
    let mut app = crate::App::headless_for_test();
    let csv = scratch("accounts.csv", &crlf_table());
    let view = open(&mut app, &csv.path);
    let value = "The sign-in page said \"check your inbox\", so email.";
    named(&mut app, NamedKey::ArrowDown);
    for _ in 0..4 {
        named(&mut app, NamedKey::ArrowRight);
    }
    assert_eq!(cell(&app, view), (1, 4));
    assert_eq!(
        app.native_selection_text(WindowId(0)).as_deref(),
        Some(value)
    );
    // A control client's ⌘C has no clipboard authority: the editor refuses it, and the clipboard
    // keeps what it held.
    assert!(crate::control::pbcopy("before"));
    assert!(!app.with_gesture_source(true, |app| app.copy_in(WindowId(0))));
    assert_eq!(crate::control::pbpaste().as_deref(), Some("before"));
    assert!(app.copy_in(WindowId(0)));
    assert_eq!(crate::control::pbpaste().as_deref(), Some(value));
    // C-y on the row's empty correction puts the copied value there, quoted as it needs.
    named(&mut app, NamedKey::ArrowRight);
    key(&mut app, Key::Character('y'), Modifiers::CTRL);
    assert!(
        text(&app, view).contains(
            "so email.\",\"The sign-in page said \"\"check your inbox\"\", so email.\"\n"
        ),
        "{:?}",
        text(&app, view)
    );
    // An empty cell has nothing to copy; an open cell copies its text.
    named(&mut app, NamedKey::ArrowDown);
    named(&mut app, NamedKey::ArrowDown);
    assert_eq!(cell(&app, view), (3, 5), "m3 lacks the cell");
    assert_eq!(app.native_selection_text(WindowId(0)), None);
    type_text(&mut app, "typed");
    assert_eq!(
        app.native_selection_text(WindowId(0)).as_deref(),
        Some("typed")
    );
    assert!(crate::control::pbcopy("before"));
    assert!(!app.with_gesture_source(true, |app| app.copy_in(WindowId(0))));
    assert_eq!(
        crate::control::pbpaste().as_deref(),
        Some("before"),
        "a control client's ⌘C on an open cell"
    );
    assert!(app.copy_selection());
    assert_eq!(crate::control::pbpaste().as_deref(), Some("typed"));
}

/// G2: the toolbar's Add Row and Delete Row, C-o and C-S-Backspace change on disk only the row's
/// own bytes: a BOM and CRLF file with no final newline gains `\r\n` and empty cells and still
/// ends without one, and a mixed file's added row ends with the CRLF of the row above it.
#[test]
fn g2_rows_added_and_deleted_save_byte_exact() {
    use crate::native_app::ActionInvocation;
    let mut app = crate::App::headless_for_test();
    let original = crlf_table();
    let csv = scratch("accounts.csv", &original);
    let view = open(&mut app, &csv.path);
    let compiled = app.compiled_native_ui(WindowId(0)).unwrap();
    for button in [
        "editor/grid-insert-row-button",
        "editor/grid-delete-row-button",
    ] {
        assert!(
            compiled
                .semantic(&crate::native_ui::UiKey::new(button))
                .is_some(),
            "{button}"
        );
    }
    // The last row (no final newline): C-o adds a row after it and moves there.
    for _ in 0..4 {
        named(&mut app, NamedKey::ArrowDown);
    }
    assert_eq!(cell(&app, view), (4, 0));
    key(&mut app, Key::Character('o'), Modifiers::CTRL);
    assert_eq!(cell(&app, view), (5, 0), "the cursor moves to the new row");
    type_text(&mut app, "m5");
    named(&mut app, NamedKey::Enter);
    command(&mut app, 's');
    let mut expected = original.clone();
    expected.extend_from_slice(b"\r\nm5,,,,,");
    assert_eq!(
        String::from_utf8_lossy(&std::fs::read(&csv.path).unwrap()),
        String::from_utf8_lossy(&expected)
    );
    // Delete Row on the toolbar takes it back off, the file ending as it did.
    let (_, native_view) = app.active_native_view(WindowId(0)).unwrap();
    assert_eq!(native_view, view);
    app.dispatch_native_event(
        WindowId(0),
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/grid-delete-row"),
            value: None,
        }),
    )
    .unwrap();
    command(&mut app, 's');
    assert_eq!(std::fs::read(&csv.path).unwrap(), original);

    // The mixed file: Add Row under m1 ends with m1's CRLF; C-S-Backspace deletes m2.
    let mixed = scratch("mixed.csv", MIXED.as_bytes());
    let view = open(&mut app, &mixed.path);
    named(&mut app, NamedKey::ArrowDown);
    app.dispatch_native_event(
        WindowId(0),
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/grid-insert-row"),
            value: None,
        }),
    )
    .unwrap();
    assert_eq!(cell(&app, view), (2, 0));
    named(&mut app, NamedKey::ArrowDown);
    key(
        &mut app,
        Key::Named(NamedKey::Backspace),
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    command(&mut app, 's');
    assert_eq!(
        std::fs::read_to_string(&mixed.path).unwrap(),
        "name,note,fix\r\nm1,\"two\nlines\",\r\n,,\r\n"
    );
    // ⌘Z takes each back, one row operation at a time.
    command(&mut app, 'z');
    assert_eq!(
        text(&app, view),
        "name,note,fix\r\nm1,\"two\nlines\",\r\n,,\r\nm2,plain,\r\n"
    );
    command(&mut app, 'z');
    assert_eq!(text(&app, view), MIXED);
}

/// G2: a table's editor compiles at every window width, in the grid and in the text: whatever the
/// toolbar shortens a button to is a word of the button's name (WCAG 2.5.3, Label in Name, which a
/// debug compile enforces). A 110-column lab window once shortened Delete Row to `−Row` and the
/// view would not draw.
#[test]
fn g2_a_tables_editor_compiles_at_every_width() {
    let mut app = crate::App::headless_for_test();
    let csv = scratch("accounts.csv", &crlf_table());
    let view = open(&mut app, &csv.path);
    for grid in [true, false] {
        assert_eq!(grid_shown(&app, view), grid);
        let (instance, shown) = app.active_native_view(WindowId(0)).unwrap();
        for width in (240..=1_600).step_by(8) {
            let viewport = crate::native_ui::LogicalRect::new(0.0, 0.0, width as f32, 420.0);
            if let Err(why) = app.compiled_native_ui_for(WindowId(0), instance, shown, viewport) {
                panic!("grid={grid} at {width}pt: {why}");
            }
        }
        command(&mut app, 'e');
    }
}

/// G2 review: a cell value holding CRLF, copied from a file held byte for byte (mixed line ends),
/// and yanked into a cell of a file whose every line ends CRLF (held with LF) saves as plain CRLF
/// inside the cell, the rest of the file byte-exact. *Control:* before the editor took
/// `crlf_on_disk`, the yank wrote the CRLF into the LF-held text and the file saved `\r\r\n`.
#[test]
fn g2_a_crlf_value_yanked_into_a_crlf_file_saves_plain_crlf() {
    let mut app = crate::App::headless_for_test();
    let mixed = scratch("mixed.csv", b"name,note\r\nm1,\"a\r\nb\"\nm2,x\r\n");
    let mixed_view = open(&mut app, &mixed.path);
    assert!(grid_shown(&app, mixed_view));
    named(&mut app, NamedKey::ArrowDown);
    named(&mut app, NamedKey::ArrowRight);
    assert_eq!(cell(&app, mixed_view), (1, 1));
    assert!(app.copy_in(WindowId(0)));
    assert_eq!(
        app.editor_workspace.kill_ring.front().map(String::as_str),
        Some("a\r\nb"),
        "a byte-held cell keeps its CRLF"
    );

    let original = crlf_table();
    let csv = scratch("accounts.csv", &original);
    let view = open(&mut app, &csv.path);
    named(&mut app, NamedKey::ArrowDown);
    for _ in 0..5 {
        named(&mut app, NamedKey::ArrowRight);
    }
    assert_eq!(cell(&app, view), (1, 5));
    key(&mut app, Key::Character('y'), Modifiers::CTRL);
    assert!(!text(&app, view).contains('\r'), "{:?}", text(&app, view));
    command(&mut app, 's');
    let saved = std::fs::read(&csv.path).unwrap();
    let expected = String::from_utf8(original).unwrap().replacen(
        "so email.\",\r\n",
        "so email.\",\"a\r\nb\"\r\n",
        1,
    );
    assert_eq!(String::from_utf8_lossy(&saved), expected);
}
