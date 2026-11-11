// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::editor::{CloseWhy, CommandAuthority, Invocation, Keymap, Platform, READ_ONLY_DOCUMENT};
use crate::store::{DocumentId, DocumentViewId};

/// Whether `effects` asks the host to close the view for `why`.
fn closes(effects: &[EditorEffect], why: CloseWhy) -> bool {
    effects.contains(&EditorEffect::CloseView { why })
}
use crate::table::fixtures::{ALL_CRLF, MAIN, MIXED, STRAY};

fn grid_editor(text: &str) -> (DocumentStore, EditorWorkspace, EditorBufferView, DocumentId) {
    grid_editor_named("file:///tmp/accounts.csv", text)
}

fn grid_editor_named(
    uri: &str,
    text: &str,
) -> (DocumentStore, EditorWorkspace, EditorBufferView, DocumentId) {
    let mut store = DocumentStore::for_test();
    let document = store.open(uri.into(), text.into());
    let mut workspace = EditorWorkspace::with_keymap(Keymap::emacs(Profile::Gui(Platform::Mac)));
    let mut view = workspace
        .attach(&mut store, document, DocumentViewId(31))
        .unwrap();
    view.reconcile_viewport(text, 20);
    workspace
        .set_face(&store, &mut view, Some(Delimiter::Comma))
        .unwrap();
    (store, workspace, view, document)
}

fn key(key: Key, mods: Modifiers) -> EditorInput {
    EditorInput::Key {
        key,
        mods,
        text: None,
    }
}

fn named(named: NamedKey) -> EditorInput {
    key(Key::Named(named), Modifiers::empty())
}

fn ch(character: char, mods: Modifiers) -> EditorInput {
    key(Key::Character(character), mods)
}

fn text_of(store: &DocumentStore, document: DocumentId) -> String {
    store.snapshot(document).unwrap().text.to_string()
}

fn cell_of(
    store: &DocumentStore,
    workspace: &mut EditorWorkspace,
    view: &EditorBufferView,
) -> (usize, usize) {
    let snapshot = store.snapshot(view.document).unwrap();
    let table = workspace.grid_table(&snapshot, Delimiter::Comma).unwrap();
    current_cell(&table, view)
}

fn press(
    store: &mut DocumentStore,
    workspace: &mut EditorWorkspace,
    view: &mut EditorBufferView,
    inputs: &[EditorInput],
) -> Vec<EditorEffect> {
    let mut effects = Vec::new();
    for input in inputs {
        effects = workspace
            .handle_input(store, view, input.clone())
            .unwrap()
            .effects;
    }
    effects
}

fn goto(
    store: &mut DocumentStore,
    workspace: &mut EditorWorkspace,
    view: &mut EditorBufferView,
    record: usize,
    column: usize,
) {
    press(
        store,
        workspace,
        view,
        &[EditorInput::GridPointer {
            record,
            column,
            open: false,
        }],
    );
    assert_eq!(cell_of(store, workspace, view), (record, column));
}

/// A read-only document's grid (a revealed secret's, opened `edit … ro=1`) moves between
/// cells and copies one, but every key, paste, cut, row command, undo and save that would
/// change it says so and changes nothing: no cell opens and the text stays as it was.
/// NEGATIVE CONTROL: with `open_draft` skipping its read-only check, typing opens a cell:
/// `` x: a cell opened ``.
#[test]
fn a_read_only_grid_moves_and_copies_but_changes_nothing() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
    store.set_read_only(document).unwrap();
    goto(&mut store, &mut workspace, &mut view, 1, 1);
    let refused = |effects: &[EditorEffect]| {
        effects == [EditorEffect::Status(READ_ONLY_DOCUMENT.to_string())]
    };
    let changes: [(&str, Vec<EditorInput>); 12] = [
        ("x", vec![ch('x', Modifiers::empty())]),
        ("space", vec![named(NamedKey::Space)]),
        ("enter", vec![named(NamedKey::Enter)]),
        ("f2", vec![named(NamedKey::F2)]),
        ("paste", vec![EditorInput::Paste("pasted".into())]),
        ("backspace", vec![named(NamedKey::Backspace)]),
        ("delete", vec![named(NamedKey::Delete)]),
        ("C-k", vec![ch('k', Modifiers::CTRL)]),
        ("C-o", vec![ch('o', Modifiers::CTRL)]),
        ("cmd-z", vec![ch('z', Modifiers::SUPER)]),
        ("cmd-s", vec![ch('s', Modifiers::SUPER)]),
        (
            "double-click",
            vec![EditorInput::GridPointer {
                record: 1,
                column: 1,
                open: true,
            }],
        ),
    ];
    for (name, inputs) in &changes {
        let effects = press(&mut store, &mut workspace, &mut view, inputs);
        assert!(view.grid_draft().is_none(), "{name}: a cell opened");
        assert!(refused(&effects), "{name}: {effects:?}");
        assert_eq!(text_of(&store, document), MAIN, "{name}");
    }
    for command in [EditorCommand::GridInsertRow, EditorCommand::GridDeleteRow] {
        let effects = workspace.execute(&mut store, &mut view, command).unwrap();
        assert!(refused(&effects), "{command:?}: {effects:?}");
    }
    assert_eq!(text_of(&store, document), MAIN);
    // Moving and copying are reading.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::ArrowRight)],
    );
    assert_eq!(cell_of(&store, &mut workspace, &view), (1, 2));
    let copied = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('c', Modifiers::SUPER)],
    );
    assert!(!refused(&copied), "{copied:?}");

    // The same keys change a writable grid.
    let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
    goto(&mut store, &mut workspace, &mut view, 1, 1);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('x', Modifiers::empty()), named(NamedKey::Enter)],
    );
    assert_ne!(text_of(&store, document), MAIN);
}

#[test]
fn a_table_opens_on_the_cell_its_caret_is_in() {
    let (store, mut workspace, view, _) = grid_editor(MAIN);
    assert!(view.grid().is_some());
    assert_eq!(cell_of(&store, &mut workspace, &view), (0, 0));
    let big = format!(
        "a,b\n{}",
        "x,y\n".repeat(crate::table::MAX_TABLE_RECORDS + 1)
    );
    let mut store = DocumentStore::for_test();
    let document = store.open("file:///tmp/big.csv".into(), big);
    let mut workspace = EditorWorkspace::new();
    let mut view = workspace
        .attach(&mut store, document, DocumentViewId(3))
        .unwrap();
    let refusal = workspace
        .set_face(&store, &mut view, Some(Delimiter::Comma))
        .unwrap_err();
    assert!(refusal.starts_with("Opened as text"), "{refusal}");
    assert!(view.grid().is_none());
}

/// T5: every G1 key, from a known cursor, gives the documented cursor, draft and effects.
#[test]
fn t5_the_reducer_table() {
    use Modifiers as M;
    let none = M::empty();
    // (name, start, inputs, cursor after, draft text after)
    type Row<'a> = (
        &'a str,
        (usize, usize),
        Vec<EditorInput>,
        (usize, usize),
        Option<&'a str>,
    );
    let rows: &[Row<'_>] = &[
        (
            "right",
            (1, 1),
            vec![named(NamedKey::ArrowRight)],
            (1, 2),
            None,
        ),
        (
            "left",
            (1, 1),
            vec![named(NamedKey::ArrowLeft)],
            (1, 0),
            None,
        ),
        (
            "down",
            (1, 1),
            vec![named(NamedKey::ArrowDown)],
            (2, 1),
            None,
        ),
        ("up", (1, 1), vec![named(NamedKey::ArrowUp)], (0, 1), None),
        ("C-f", (1, 1), vec![ch('f', M::CTRL)], (1, 2), None),
        ("C-b", (1, 1), vec![ch('b', M::CTRL)], (1, 0), None),
        ("C-n", (1, 1), vec![ch('n', M::CTRL)], (2, 1), None),
        ("C-p", (1, 1), vec![ch('p', M::CTRL)], (0, 1), None),
        (
            "up stops at the header",
            (0, 1),
            vec![named(NamedKey::ArrowUp)],
            (0, 1),
            None,
        ),
        (
            "down stops at the end",
            (4, 1),
            vec![named(NamedKey::ArrowDown)],
            (4, 1),
            None,
        ),
        ("tab", (1, 1), vec![named(NamedKey::Tab)], (1, 2), None),
        (
            "tab wraps",
            (1, 6),
            vec![named(NamedKey::Tab)],
            (2, 0),
            None,
        ),
        (
            "S-tab wraps",
            (2, 0),
            vec![key(Key::Named(NamedKey::Tab), M::SHIFT)],
            (1, 6),
            None,
        ),
        ("home", (1, 3), vec![named(NamedKey::Home)], (1, 0), None),
        ("end", (1, 0), vec![named(NamedKey::End)], (1, 5), None),
        ("C-a", (1, 3), vec![ch('a', M::CTRL)], (1, 0), None),
        (
            "C-e on a short row",
            (3, 0),
            vec![ch('e', M::CTRL)],
            (3, 5),
            None,
        ),
        (
            "C-e on a long row",
            (4, 0),
            vec![ch('e', M::CTRL)],
            (4, 6),
            None,
        ),
        ("M->", (1, 2), vec![ch('>', M::ALT)], (4, 2), None),
        ("M-<", (3, 2), vec![ch('<', M::ALT)], (0, 2), None),
        (
            "page down",
            (0, 1),
            vec![named(NamedKey::PageDown)],
            (4, 1),
            None,
        ),
        (
            "page up",
            (4, 1),
            vec![named(NamedKey::PageUp)],
            (0, 1),
            None,
        ),
        ("C-v", (0, 1), vec![ch('v', M::CTRL)], (4, 1), None),
        ("M-v", (4, 1), vec![ch('v', M::ALT)], (0, 1), None),
        (
            "C-u 2 C-n",
            (0, 1),
            vec![ch('u', M::CTRL), ch('2', none), ch('n', M::CTRL)],
            (2, 1),
            None,
        ),
        (
            "return opens",
            (1, 2),
            vec![named(NamedKey::Enter)],
            (1, 2),
            Some("Claude"),
        ),
        (
            "F2 opens",
            (2, 2),
            vec![named(NamedKey::F2)],
            (2, 2),
            Some("Claude, GitHub"),
        ),
        (
            "typing replaces",
            (1, 2),
            vec![ch('x', none)],
            (1, 2),
            Some("x"),
        ),
        (
            "space replaces",
            (1, 2),
            vec![named(NamedKey::Space)],
            (1, 2),
            Some(" "),
        ),
        (
            "a missing cell opens empty",
            (3, 5),
            vec![named(NamedKey::Enter)],
            (3, 5),
            Some(""),
        ),
        (
            "escape drops",
            (1, 2),
            vec![ch('x', none), named(NamedKey::Escape)],
            (1, 2),
            None,
        ),
        (
            "C-g drops",
            (1, 2),
            vec![ch('x', none), ch('g', M::CTRL)],
            (1, 2),
            None,
        ),
        (
            "draft edits",
            (1, 2),
            vec![
                named(NamedKey::Enter),
                named(NamedKey::Backspace),
                ch('e', none),
                ch('a', M::CTRL),
                ch('d', M::CTRL),
                ch('X', none),
            ],
            (1, 2),
            Some("Xlaude"),
        ),
        (
            "M-RET breaks a line",
            (1, 2),
            vec![
                ch('a', none),
                key(Key::Named(NamedKey::Enter), M::ALT),
                ch('b', none),
            ],
            (1, 2),
            Some("a\nb"),
        ),
        (
            "C-k kills to the line end",
            (1, 2),
            vec![
                named(NamedKey::Enter),
                ch('a', M::CTRL),
                ch('f', M::CTRL),
                ch('k', M::CTRL),
            ],
            (1, 2),
            Some("C"),
        ),
        (
            "⌘Z restores the draft",
            (1, 2),
            vec![named(NamedKey::Enter), ch('z', none), ch('z', M::SUPER)],
            (1, 2),
            Some("Claude"),
        ),
        (
            "⌘E is refused while editing",
            (1, 2),
            vec![ch('q', none), ch('e', M::SUPER)],
            (1, 2),
            Some("q"),
        ),
        (
            "C-g cancels a prefix",
            (1, 2),
            vec![ch('x', M::CTRL), ch('g', M::CTRL)],
            (1, 2),
            None,
        ),
    ];
    for (name, start, inputs, expected, draft) in rows {
        let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
        goto(&mut store, &mut workspace, &mut view, start.0, start.1);
        let before = store.snapshot(document).unwrap().seq;
        press(&mut store, &mut workspace, &mut view, inputs);
        assert_eq!(cell_of(&store, &mut workspace, &view), *expected, "{name}");
        assert_eq!(
            view.grid_draft().map(|draft| draft.text.as_str()),
            *draft,
            "{name}"
        );
        assert_eq!(
            store.snapshot(document).unwrap().seq,
            before,
            "{name}: no edit"
        );
        assert!(view.grid().is_some(), "{name}");
    }
}

/// T5: Return commits one undo frame over the cell's bytes, Escape writes nothing, undo
/// from a text view of the same document takes a grid commit back, and ⌘Z in the grid puts
/// the cursor on the cell it changed.
#[test]
fn t5_commits_escape_and_one_shared_history() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
    goto(&mut store, &mut workspace, &mut view, 2, 5);
    let before = store.snapshot(document).unwrap().seq;
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[
            ch('n', Modifiers::empty()),
            ch('o', Modifiers::empty()),
            named(NamedKey::Escape),
        ],
    );
    assert_eq!(
        store.snapshot(document).unwrap().seq,
        before,
        "Escape writes nothing"
    );
    assert_eq!(text_of(&store, document), MAIN);
    assert!(!workspace.can_undo(document));

    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Text("fix, ".into())],
    );
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Paste("\"quoted\"".into())],
    );
    assert_eq!(view.grid_draft().unwrap().text, "fix, \"quoted\"");
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Enter)],
    );
    let committed = text_of(&store, document);
    assert_eq!(
        committed,
        MAIN.replace(
            "then the code\",\n",
            "then the code\",\"fix, \"\"quoted\"\"\"\n"
        )
    );
    assert_eq!(
        store.snapshot(document).unwrap().seq.0,
        before.0 + 1,
        "one transaction"
    );
    assert_eq!(
        cell_of(&store, &mut workspace, &view),
        (3, 5),
        "Return moves down"
    );
    assert!(view.grid_draft().is_none());

    // Committing a cell's own value is no edit, no undo step and no dirty mark.
    let seq = store.snapshot(document).unwrap().seq;
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[
            named(NamedKey::ArrowUp),
            named(NamedKey::Enter),
            named(NamedKey::Enter),
        ],
    );
    assert_eq!(store.snapshot(document).unwrap().seq, seq);

    // Undo from a text view of the same document takes the grid's commit back.
    let mut text_view = workspace
        .attach(&mut store, document, DocumentViewId(32))
        .unwrap();
    workspace
        .execute(&mut store, &mut text_view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text_of(&store, document), MAIN);
    for (seq, deltas) in workspace.take_commits(document) {
        view.observe_external(seq, &deltas);
    }
    // Redo from the grid, and ⌘Z there: the cursor lands on the changed cell.
    goto(&mut store, &mut workspace, &mut view, 0, 0);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('z', Modifiers::SUPER | Modifiers::SHIFT)],
    );
    assert_eq!(text_of(&store, document), committed);
    assert_eq!(cell_of(&store, &mut workspace, &view), (2, 5));
    goto(&mut store, &mut workspace, &mut view, 4, 0);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('z', Modifiers::SUPER)],
    );
    assert_eq!(text_of(&store, document), MAIN);
    assert_eq!(cell_of(&store, &mut workspace, &view), (2, 5));
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('?', Modifiers::CTRL)],
    );
    assert_eq!(text_of(&store, document), committed, "C-? redoes");
}

#[test]
fn delete_and_paste_are_one_undo_step_each() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
    goto(&mut store, &mut workspace, &mut view, 2, 2);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Backspace)],
    );
    assert_eq!(
        text_of(&store, document),
        MAIN.replace("\"Claude, GitHub\"", "\"\"")
    );
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Paste("a,b\n".into())],
    );
    assert_eq!(
        text_of(&store, document),
        MAIN.replace("\"Claude, GitHub\"", "\"a,b\"")
    );
    assert_eq!(cell_of(&store, &mut workspace, &view), (2, 2));
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('/', Modifiers::CTRL)],
    );
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('x', Modifiers::CTRL), ch('u', Modifiers::empty())],
    );
    assert_eq!(text_of(&store, document), MAIN);
    let seq = store.snapshot(document).unwrap().seq;
    goto(&mut store, &mut workspace, &mut view, 1, 5);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Delete)],
    );
    assert_eq!(
        store.snapshot(document).unwrap().seq,
        seq,
        "clearing an empty cell is no edit"
    );
}

#[test]
fn saving_finishing_and_flipping() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
    goto(&mut store, &mut workspace, &mut view, 1, 5);
    let effects = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('s', Modifiers::SUPER)],
    );
    assert!(matches!(
        effects.as_slice(),
        [EditorEffect::SaveDocument { .. }]
    ));
    let effects = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('x', Modifiers::CTRL), ch('s', Modifiers::CTRL)],
    );
    assert!(matches!(
        effects.as_slice(),
        [EditorEffect::SaveDocument { .. }]
    ));

    // ⌘S with a cell open commits it, then saves.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('y', Modifiers::empty())],
    );
    let effects = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('s', Modifiers::SUPER)],
    );
    assert!(matches!(
        effects.as_slice(),
        [EditorEffect::SaveDocument { .. }]
    ));
    assert!(text_of(&store, document).contains("so email.\",y\n"));

    // C-c C-c with a cell open commits, then finishes; C-c C-k drops it, then aborts.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::ArrowRight), ch('z', Modifiers::empty())],
    );
    let effects = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('c', Modifiers::CTRL), ch('c', Modifiers::CTRL)],
    );
    assert!(closes(&effects, CloseWhy::Finish), "{effects:?}");
    let committed = text_of(&store, document);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('w', Modifiers::empty())],
    );
    let effects = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('c', Modifiers::CTRL), ch('k', Modifiers::CTRL)],
    );
    assert!(closes(&effects, CloseWhy::Abort), "{effects:?}");
    assert_eq!(text_of(&store, document), committed);

    // ⌘E and C-c C-t flip to the text and back, keeping the place.
    goto(&mut store, &mut workspace, &mut view, 2, 2);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('e', Modifiers::SUPER)],
    );
    assert!(view.grid().is_none());
    let head = view.primary_selection().head;
    assert_eq!(&committed[head..head + 6], "Claude");
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('c', Modifiers::CTRL), ch('t', Modifiers::CTRL)],
    );
    assert_eq!(cell_of(&store, &mut workspace, &view), (2, 2));
    // A file that is not a table has no grid.
    let mut store = DocumentStore::for_test();
    let document = store.open("file:///tmp/notes.txt".into(), "a,b\n".into());
    let mut workspace = EditorWorkspace::new();
    let mut view = workspace
        .attach(&mut store, document, DocumentViewId(5))
        .unwrap();
    let effects = workspace
        .execute(&mut store, &mut view, EditorCommand::ToggleGridView)
        .unwrap();
    assert!(view.grid().is_none());
    assert!(
        matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.starts_with("Not a table"))
    );
    // The terminal face shows a table as text, and says where the grid is.
    let mut store = DocumentStore::for_test();
    let document = store.open("file:///tmp/t.csv".into(), "a,b\n".into());
    let mut workspace = EditorWorkspace::with_keymap(Keymap::emacs(Profile::Terminal));
    let mut view = workspace
        .attach(&mut store, document, DocumentViewId(6))
        .unwrap();
    let effects = workspace
        .execute(&mut store, &mut view, EditorCommand::ToggleGridView)
        .unwrap();
    assert!(view.grid().is_none());
    assert_eq!(
        effects,
        vec![EditorEffect::Status(TERMINAL_TABLE.to_string())]
    );
}

#[test]
fn search_puts_the_cursor_on_the_match() {
    let (mut store, mut workspace, mut view, _) = grid_editor(MAIN);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('s', Modifiers::CTRL)],
    );
    for character in "carol".chars() {
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch(character, Modifiers::empty())],
        );
    }
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Enter)],
    );
    assert!(!view.minibuffer_active());
    assert_eq!(cell_of(&store, &mut workspace, &view), (3, 1));
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::ArrowRight)],
    );
    assert_eq!(cell_of(&store, &mut workspace, &view), (3, 2));
}

/// T6: a foreign commit to the drafted cell makes Return refuse and keep the draft; one to
/// another cell lets the commit land on the right cell.
#[test]
fn t6_a_draft_commits_only_over_the_value_it_opened_with() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
    goto(&mut store, &mut workspace, &mut view, 2, 3);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Enter), ch('!', Modifiers::empty())],
    );
    let mut other = workspace
        .attach(&mut store, document, DocumentViewId(33))
        .unwrap();
    let sms = MAIN.find("SMS").unwrap();
    other.selections = vec![Selection::caret(sms)];
    workspace
        .insert_text(&mut store, &mut other, "MMS or ")
        .unwrap();
    for (seq, deltas) in workspace.take_commits(document) {
        view.observe_external(seq, &deltas);
    }
    let foreign = text_of(&store, document);
    let effects = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Enter)],
    );
    assert_eq!(effects, [EditorEffect::Status(CELL_CHANGED.to_string())]);
    assert_eq!(view.grid_draft().unwrap().text, "SMS!");
    assert_eq!(text_of(&store, document), foreign, "the foreign text stays");
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Escape)],
    );

    // A foreign commit to another cell: the draft lands on its own cell.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Enter), ch('?', Modifiers::empty())],
    );
    other.selections = vec![Selection::caret(foreign.find("m4").unwrap())];
    workspace
        .insert_text(&mut store, &mut other, "big ")
        .unwrap();
    for (seq, deltas) in workspace.take_commits(document) {
        view.observe_external(seq, &deltas);
    }
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Enter)],
    );
    let landed = text_of(&store, document);
    assert!(landed.contains(",MMS or SMS?,"), "{landed}");
    assert!(landed.contains("\nbig m4,"), "{landed}");
}

/// T7: in a grid, every command either is refused and leaves the text alone, or changes at
/// most the current cell's bytes; a row command (G2) changes at most its own row's.
#[test]
fn t7_commands_that_edit_raw_text_are_refused() {
    for command in EditorCommand::ALL {
        let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
        goto(&mut store, &mut workspace, &mut view, 2, 2);
        workspace.kill_ring.push_front("zz".to_string());
        let snapshot = store.snapshot(document).unwrap();
        let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
        let rows = matches!(
            command,
            EditorCommand::OpenLine | EditorCommand::GridInsertRow | EditorCommand::GridDeleteRow
        );
        let own = if rows {
            table.records[2].raw.start..table.records[2].terminator.end
        } else {
            table.cell(2, 2).unwrap().raw.clone()
        };
        let result = workspace.invoke(&mut store, &mut view, Invocation::new(command));
        let after = text_of(&store, document);
        let changed_outside = after.len() + own.len() < MAIN.len()
            || after[..own.start] != MAIN[..own.start]
            || after[after.len() - (MAIN.len() - own.end)..] != MAIN[own.end..];
        assert!(
            !changed_outside,
            "{} edited outside its {}: {after:?}",
            command.name(),
            if rows { "row" } else { "cell" }
        );
        if !command.allowed_in_grid() {
            assert_eq!(after, MAIN, "{}", command.name());
            let effects = result.unwrap();
            assert!(
                matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.contains("not in the grid view")),
                "{}: {effects:?}",
                command.name()
            );
        }
        assert!(
            view.grid().is_some() || command == EditorCommand::ToggleGridView,
            "{}",
            command.name()
        );
    }
}

#[test]
fn closing_commits_an_open_draft_first() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
    goto(&mut store, &mut workspace, &mut view, 1, 3);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Text("done".into())],
    );
    workspace
        .commit_before_close(&mut store, &mut view)
        .unwrap();
    assert!(view.grid_draft().is_none());
    assert_eq!(
        text_of(&store, document),
        MIXED.replace("and two\",\r\n", "and two\",done\r\n"),
        "the mixed file's CRLF records and LF cell are kept byte for byte"
    );
    // A refused commit refuses the close and keeps the text.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Enter), ch('!', Modifiers::empty())],
    );
    let mut other = workspace
        .attach(&mut store, document, DocumentViewId(34))
        .unwrap();
    other.selections = vec![Selection::caret(
        text_of(&store, document).find("done").unwrap(),
    )];
    workspace.insert_text(&mut store, &mut other, "un").unwrap();
    for (seq, deltas) in workspace.take_commits(document) {
        view.observe_external(seq, &deltas);
    }
    assert_eq!(
        workspace.commit_before_close(&mut store, &mut view),
        Err(CELL_CHANGED.to_string())
    );
    assert_eq!(view.grid_draft().unwrap().text, "done!");
}

/// A revealed secret's grid, concealed by its `password` column: what its readers get holds
/// every address, every header and the table's shape, and no password, the current cell's
/// included; copying a password still copies it (the person's clipboard), and says so without
/// quoting it, while the kill ring every document yanks from keeps none of it and a macro's
/// replay copies nothing; copying an address quotes it, and the ring keeps that no more than a
/// password, since a private document's kills all stay off it. A document that conceals
/// everything hides the headers too, and a plain grid (nothing concealed) reads its passwords
/// out, so the search finds a value where one is.
/// NEGATIVE CONTROL: with `cell_concealed` answering `false` for a data cell, the reading
/// holds the passwords: `` a password reached the reading ``.
#[test]
fn a_concealed_grid_reads_out_no_secret_and_every_other_cell() {
    const SECRETS: [&str; 2] = ["hunter2-Zq9", "s3cr3t-Wv7"];
    let text = format!(
        "addr,password,note\nada@example.test,{},first\nbob@example.test,{},second\n",
        SECRETS[0], SECRETS[1]
    );
    let (mut store, mut workspace, mut view, document) = grid_editor(&text);
    let password = Concealed::Columns(vec!["password".into()]);
    store.set_concealed(document, password.clone()).unwrap();
    goto(&mut store, &mut workspace, &mut view, 1, 1);
    let snapshot = store.snapshot(document).unwrap();
    let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
    let projection = project_grid(&snapshot.text, &table, &view, 20, 80);
    let holds = |reading: &GridReading, wanted: &str| format!("{reading:?}").contains(wanted);

    let reading = read_grid(&snapshot.text, &table, &projection, Some(&password));
    for secret in SECRETS {
        assert!(!holds(&reading, secret), "a password reached the reading");
    }
    assert_eq!(reading.cursor, CellReading::Hidden);
    assert_eq!(reading.cursor.words(), HIDDEN_VALUE);
    assert_eq!((reading.rows, reading.columns), (2, 3));
    for wanted in [
        "addr",
        "password",
        "note",
        "ada@example.test",
        "bob@example.test",
    ] {
        assert!(holds(&reading, wanted), "{wanted} is read out: {reading:?}");
    }
    assert_eq!(
        reading.records[0].1,
        vec![
            CellReading::Value("ada@example.test".into()),
            CellReading::Hidden,
            CellReading::Value("first".into()),
        ]
    );

    let copied = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('c', Modifiers::SUPER)],
    );
    assert!(
        copied.iter().any(|effect| matches!(
            effect,
            EditorEffect::KillToClipboard { text } if text == SECRETS[0]
        )),
        "the person's copy holds the value: {copied:?}"
    );
    assert!(copied.contains(&EditorEffect::Status(COPIED_HIDDEN.to_string())));
    assert!(!format!("{copied:?}").contains("Copied “"), "{copied:?}");
    assert!(
        !workspace.kill_ring.iter().any(|kill| kill == SECRETS[0]),
        "the ring every document yanks from keeps no hidden value"
    );
    assert_eq!(workspace.grid_kill, None);
    // A macro's replay is no keystroke of the person's: it copies no hidden value.
    workspace.playing_macro = true;
    let replayed = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('c', Modifiers::SUPER)],
    );
    workspace.playing_macro = false;
    assert!(
        replayed.contains(&EditorEffect::Status(CONCEALED_STAYS.to_string())),
        "{replayed:?}"
    );
    goto(&mut store, &mut workspace, &mut view, 1, 0);
    let copied = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('c', Modifiers::SUPER)],
    );
    assert!(
        copied.contains(&EditorEffect::Status(
            "Copied “ada@example.test”".to_string()
        )),
        "{copied:?}"
    );
    assert!(
        copied.iter().any(|effect| matches!(
            effect,
            EditorEffect::KillToClipboard { text } if text == "ada@example.test"
        )),
        "{copied:?}"
    );
    assert!(
        !workspace
            .kill_ring
            .iter()
            .any(|kill| kill == "ada@example.test"),
        "a private document's copies stay off the ring, its plain cells' included"
    );

    assert_eq!(
        reading.cursor_words(),
        format!("Row 2, password: {HIDDEN_VALUE}")
    );

    let all = read_grid(&snapshot.text, &table, &projection, Some(&Concealed::All));
    assert!(
        all.headers
            .iter()
            .map(|(_, header)| header)
            .chain(all.records.iter().flat_map(|(_, cells)| cells))
            .chain([&all.cursor, &all.cursor_header])
            .all(|cell| *cell == CellReading::Hidden),
        "{all:?}"
    );
    assert_eq!(
        all.cursor_words(),
        format!("Row 2, column 2: {HIDDEN_VALUE}")
    );

    let plain = read_grid(&snapshot.text, &table, &projection, None);
    assert!(
        SECRETS.iter().all(|secret| holds(&plain, secret)),
        "a plain grid reads its passwords out"
    );
}

/// Input without clipboard authority (in aterm, a control client's, an accessibility
/// client's or another process's posted keys) neither copies nor cuts a concealed
/// document's value, not even to a kill ring of its own, from which it could yank the value
/// where the window's readers read it; the cut it makes in a plain grid lands on that ring.
/// NEGATIVE CONTROL: with `execute_with` skipping its concealed check, C-k cuts the password
/// onto the guest's ring and the document loses it: `` the guest's cut ``.
#[test]
fn input_without_clipboard_authority_takes_no_concealed_value() {
    const SECRET: &str = "hunter2-Zq9";
    let text = format!("addr,password\nada@example.test,{SECRET}\n");
    for concealed in [true, false] {
        let (mut store, mut workspace, mut view, document) = grid_editor(&text);
        if concealed {
            store
                .set_concealed(document, Concealed::Columns(vec!["password".into()]))
                .unwrap();
        }
        goto(&mut store, &mut workspace, &mut view, 1, 1);
        workspace.granted = vec![
            CommandAuthority::Read,
            CommandAuthority::Edit,
            CommandAuthority::Save,
        ];
        let copied = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('c', Modifiers::SUPER)],
        );
        assert!(
            !format!("{copied:?}").contains(SECRET),
            "no copy without clipboard authority: {copied:?}"
        );
        let cut = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('k', Modifiers::CTRL)],
        );
        let guest_holds = workspace.guest_grid_kill.as_deref() == Some(SECRET)
            || workspace.guest_kill_ring.iter().any(|kill| kill == SECRET);
        assert_eq!(guest_holds, !concealed, "the guest's cut: {cut:?}");
        assert_eq!(
            store.snapshot(document).unwrap().text.contains(SECRET),
            concealed,
            "the guest's cut"
        );
        assert_eq!(
            cut.contains(&EditorEffect::Status(CONCEALED_STAYS.to_string())),
            concealed,
            "{cut:?}"
        );
        assert!(workspace.kill_ring.is_empty(), "the person's ring");
    }
}

#[test]
fn the_projection_lays_out_header_rows_and_panel() {
    let (mut store, mut workspace, mut view, _) = grid_editor(MAIN);
    goto(&mut store, &mut workspace, &mut view, 2, 4);
    let snapshot = store.snapshot(view.document).unwrap();
    let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
    let projection = project_grid(&snapshot.text, &table, &view, 20, 60);
    assert_eq!(projection.records, 5);
    assert_eq!(projection.columns, 7);
    assert_eq!(projection.cursor.record, 2);
    assert_eq!(projection.cursor.column, 4);
    assert_eq!(projection.cursor.line, 3);
    assert!(
        projection.shown.iter().any(|column| column.index == 4),
        "the cursor's column shows"
    );
    assert!(projection.hidden_left);
    assert_eq!(projection.header.record, 0);
    assert_eq!(projection.rows.first().map(|row| row.record), Some(1));
    let row3 = projection.rows.iter().find(|row| row.record == 3).unwrap();
    let shown_five = projection.shown.iter().position(|column| column.index == 5);
    if let Some(index) = shown_five {
        assert!(row3.cells[index].missing, "m3 lacks column 6");
    }
    let at = projection
        .shown
        .iter()
        .position(|column| column.index == 4)
        .unwrap();
    let drawn = &projection
        .rows
        .iter()
        .find(|row| row.record == 2)
        .unwrap()
        .cells[at]
        .text;
    assert!(drawn.ends_with('…') || drawn.ends_with('⏎'), "{drawn}");
    assert_eq!(projection.panel.label, "Row 3 · why I guessed");
    assert_eq!(
        projection.panel.lines.first().map(String::as_str),
        Some("Two reasons: first")
    );
    assert_eq!(projection.panel.caret, None);

    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Enter)],
    );
    let projection = project_grid(&snapshot.text, &table, &view, 20, 60);
    assert!(projection.panel.editing);
    assert_eq!(
        projection.panel.caret,
        Some((1, "the phone prompt, then the code".len()))
    );
    assert_eq!(GridRows::of(20), GridRows { data: 12, panel: 6 });
    assert_eq!(GridRows::of(60), GridRows { data: 50, panel: 8 });
    assert_eq!(GridRows::of(1), GridRows { data: 1, panel: 1 });
}

/// A grid told its width keeps its columns where the face shows them: a move or a press back
/// inside what shows scrolls nothing sideways (the lab saw a click on a shown cell jump the
/// columns under the pointer). And an open cell shows what is typed into it.
#[test]
fn the_columns_stay_where_they_show_and_an_open_cell_shows_its_draft() {
    let (mut store, mut workspace, mut view, _) = grid_editor(MAIN);
    // Wide enough for the last two columns, not the last three.
    let cells = 100;
    assert!(view.set_grid_width(cells));
    assert!(!view.set_grid_width(cells), "unchanged");
    // The first scrolled column, after the pinned first column once the grid has scrolled.
    let first_shown = |store: &DocumentStore, view: &EditorBufferView| {
        let snapshot = store.snapshot(view.document).unwrap();
        let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
        let projection = project_grid(&snapshot.text, &table, view, 20, cells);
        projection.shown[usize::from(projection.pinned)].index
    };
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::End)],
    );
    let scrolled = first_shown(&store, &view);
    assert!(scrolled > 0, "the last column scrolls the grid");
    assert_eq!(view.grid().unwrap().left_column, scrolled);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::ArrowLeft)],
    );
    assert_eq!(first_shown(&store, &view), scrolled, "a move inside stays");
    workspace
        .handle_input(
            &mut store,
            &mut view,
            EditorInput::GridPointer {
                record: 2,
                column: scrolled,
                open: false,
            },
        )
        .unwrap();
    assert_eq!(
        first_shown(&store, &view),
        scrolled,
        "a press on a shown cell stays"
    );

    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('z', Modifiers::empty())],
    );
    let snapshot = store.snapshot(view.document).unwrap();
    let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
    let projection = project_grid(&snapshot.text, &table, &view, 20, cells);
    let at = projection
        .shown
        .iter()
        .position(|shown| shown.index == scrolled)
        .unwrap();
    let row = projection.rows.iter().find(|row| row.record == 2).unwrap();
    assert_eq!(row.cells[at].text, "z");
    assert_eq!(
        projection.draft.as_ref().map(|draft| draft.text.as_str()),
        Some("z")
    );
}

#[test]
fn the_wheel_scrolls_rows_and_leaves_the_cursor() {
    let text = format!(
        "h1,h2\n{}",
        (0..200).map(|n| format!("r{n},v{n}\n")).collect::<String>()
    );
    let (mut store, mut workspace, mut view, _) = grid_editor(&text);
    workspace.scroll_view(&store, &mut view, 30).unwrap();
    assert_eq!(view.grid().unwrap().top_record, 31);
    assert_eq!(cell_of(&store, &mut workspace, &view), (0, 0));
    workspace.scroll_view(&store, &mut view, 10_000).unwrap();
    assert_eq!(view.grid().unwrap().top_record, 201 - GridRows::of(20).data);
    workspace.scroll_view(&store, &mut view, -10_000).unwrap();
    assert_eq!(view.grid().unwrap().top_record, 1);
    goto(&mut store, &mut workspace, &mut view, 150, 1);
    let top = view.grid().unwrap().top_record;
    assert!(top <= 150 && 150 < top + GridRows::of(20).data, "{top}");
    // A TSV document splits on tabs.
    let (store, mut workspace, mut view, _) = grid_editor_named("file:///tmp/x.tsv", "a\tb,c\n");
    workspace.set_face(&store, &mut view, None).unwrap();
    workspace
        .set_face(&store, &mut view, Some(Delimiter::Tab))
        .unwrap();
    let snapshot = store.snapshot(view.document).unwrap();
    let table = workspace.grid_table(&snapshot, Delimiter::Tab).unwrap();
    assert_eq!(table.records[0].cells.len(), 2);
}

/// The owner's shape with CRLF between records, an LF inside a quoted cell, a short row (m3)
/// and no final newline; m5's correction cell is the last of the text.
const RACE: &str = concat!(
    "computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction (leave blank if my guess is right)\r\n",
    "m1,alice@example.test,Claude,email,\"The sign-in page said \"\"check your inbox\"\", so email.\",\r\n",
    "m2,bob@example.test,\"Claude, GitHub\",SMS,\"two\nlines\",\r\n",
    "m3,carol@example.test,Claude\r\n",
    "m5,erin@example.test,Claude,SMS,phone on file,",
);

/// The file on disk becomes `text` and the buffer follows it as the editor does: one reload
/// frame through the workspace's writer, which the view hears as the GUI publishes it.
fn rewrite(
    store: &mut DocumentStore,
    workspace: &mut EditorWorkspace,
    view: &mut EditorBufferView,
    document: DocumentId,
    text: &str,
) {
    let base = store.snapshot(document).unwrap().seq;
    let (seq, deltas) = workspace
        .replace_text(store, document, base, text, crate::editor::Origin::Disk)
        .expect("the reload commits");
    assert!(!deltas.is_empty(), "the reload changed the text");
    view.observe_external(seq, &deltas);
}

/// Open `cell`, type `typed`, and return the status a first Return gives.
fn type_into(
    store: &mut DocumentStore,
    workspace: &mut EditorWorkspace,
    view: &mut EditorBufferView,
    cell: (usize, usize),
    typed: &str,
) {
    goto(store, workspace, view, cell.0, cell.1);
    press(store, workspace, view, &[EditorInput::Text(typed.into())]);
    assert_eq!(view.grid_draft().unwrap().text, typed);
}

/// A draft is tied to its own cell, not to the caret or to a value that matches: when another
/// writer changes the rows around it, the commit writes nothing until the person has seen
/// which cell it would write; a change elsewhere lets it land on its own cell at once. (The
/// review saw an empty correction land in a row an agent appended, in a deleted row's
/// neighbour, and in a column appended to every row: empty matched empty there.)
#[test]
fn a_draft_lands_only_in_its_own_cell_when_the_rows_move() {
    let enter = [named(NamedKey::Enter)];
    // A row appended after the drafted last row (no final newline: the append starts at the
    // cell's own end, where the caret sits).
    let (mut store, mut workspace, mut view, document) = grid_editor(RACE);
    type_into(&mut store, &mut workspace, &mut view, (4, 5), "fix");
    let appended = format!("{RACE}\r\nm9,zed@example.test,Claude,email,added by an agent,");
    rewrite(&mut store, &mut workspace, &mut view, document, &appended);
    let effects = press(&mut store, &mut workspace, &mut view, &enter);
    assert!(
        matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.contains("row 5 (m5)")),
        "{effects:?}"
    );
    assert_eq!(text_of(&store, document), appended, "nothing written yet");
    assert_eq!(view.grid_draft().unwrap().text, "fix", "the text is kept");
    assert_eq!(cell_of(&store, &mut workspace, &view), (4, 5));
    press(&mut store, &mut workspace, &mut view, &enter);
    assert_eq!(
        text_of(&store, document),
        appended.replace("phone on file,\r\n", "phone on file,fix\r\n"),
        "the second Return writes m5's cell, and m9 keeps its own"
    );

    // A column appended to every row: the draft stays on its own column.
    let (mut store, mut workspace, mut view, document) = grid_editor(RACE);
    type_into(&mut store, &mut workspace, &mut view, (2, 5), "fix");
    let widened = RACE
        .split("\r\n")
        .map(|row| format!("{row},"))
        .collect::<Vec<_>>()
        .join("\r\n");
    rewrite(&mut store, &mut workspace, &mut view, document, &widened);
    let effects = press(&mut store, &mut workspace, &mut view, &enter);
    assert!(
        matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.contains("row 3 (m2) · your correction")),
        "{effects:?}"
    );
    assert_eq!(text_of(&store, document), widened);
    press(&mut store, &mut workspace, &mut view, &enter);
    assert!(
        text_of(&store, document).contains("\"two\nlines\",fix,\r\n"),
        "{:?}",
        text_of(&store, document)
    );

    // The drafted last row deleted, or replaced in place by another machine's row: the
    // commit refuses and names the cell a second Return would write.
    let with_newline = format!("{RACE}\r\n");
    let last_row = "m5,erin@example.test,Claude,SMS,phone on file,\r\n";
    for rewritten in [
        with_newline.replace(last_row, ""),
        with_newline.replace(
            last_row,
            "m7,gus@example.test,Claude,email,other machine,\r\n",
        ),
    ] {
        let (mut store, mut workspace, mut view, document) = grid_editor(&with_newline);
        type_into(&mut store, &mut workspace, &mut view, (4, 5), "fix");
        rewrite(&mut store, &mut workspace, &mut view, document, &rewritten);
        let effects = press(&mut store, &mut workspace, &mut view, &enter);
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.starts_with("The file changed around this cell")),
            "{rewritten:?}: {effects:?}"
        );
        assert_eq!(text_of(&store, document), rewritten, "nothing written");
        assert_eq!(view.grid_draft().unwrap().text, "fix");
    }

    // A change wholly before the drafted row moves it and nothing more: the commit lands
    // on its own cell at once.
    let (mut store, mut workspace, mut view, document) = grid_editor(RACE);
    type_into(&mut store, &mut workspace, &mut view, (4, 5), "fix");
    let earlier = RACE.replace("alice@", "alice.b@");
    rewrite(&mut store, &mut workspace, &mut view, document, &earlier);
    let effects = press(&mut store, &mut workspace, &mut view, &enter);
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(
        text_of(&store, document),
        earlier.replace("phone on file,", "phone on file,fix")
    );
}

/// A cell whose quote never closes holds every later row: the grid neither opens, clears,
/// pastes into nor commits it, so no key drops those rows (the review saw one Backspace
/// empty the rest of the file).
#[test]
fn a_cell_that_never_closes_is_not_changed() {
    const UNCLOSED: &str = concat!(
        "computer,email,services,why I guessed,fix\n",
        "m1,alice@example.test,Claude,\"5 inch screen, so SMS,\n",
        "m2,bob@example.test,Claude,plain,\n",
        "m3,carol@example.test,Claude,plain,\n",
    );
    let (mut store, mut workspace, mut view, document) = grid_editor(UNCLOSED);
    goto(&mut store, &mut workspace, &mut view, 1, 3);
    let refused = |effects: &[EditorEffect]| matches!(effects, [EditorEffect::Status(words)] if words == CELL_UNCLOSED);
    for input in [
        named(NamedKey::Backspace),
        named(NamedKey::Delete),
        EditorInput::Paste("x".into()),
        ch('x', Modifiers::empty()),
        named(NamedKey::Enter),
    ] {
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            std::slice::from_ref(&input),
        );
        assert!(refused(&effects), "{input:?}: {effects:?}");
        assert_eq!(text_of(&store, document), UNCLOSED, "{input:?}");
        assert!(view.grid_draft().is_none(), "{input:?}");
    }
    // The cells before it still edit in place.
    goto(&mut store, &mut workspace, &mut view, 1, 2);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Text("Codex".into()), named(NamedKey::Enter)],
    );
    assert_eq!(
        text_of(&store, document),
        UNCLOSED.replace("test,Claude,\"5", "test,Codex,\"5")
    );
}

/// The cell editor has its own undo and redo: ⌘Z takes back the last run of typing, ⌘⇧Z and
/// ⌘Y bring it back, and neither ever wipes what was typed (the review saw ⌘⇧Z empty a typed
/// correction).
#[test]
fn the_cell_editor_undoes_and_redoes_its_own_typing() {
    let (mut store, mut workspace, mut view, _) = grid_editor(MAIN);
    let draft = |view: &EditorBufferView| view.grid_draft().unwrap().text.clone();
    let cmd = |character: char| ch(character, Modifiers::SUPER);
    let cmd_shift = |character: char| ch(character, Modifiers::SUPER | Modifiers::SHIFT);
    // Typing replaces "email": ⌘⇧Z with nothing undone keeps it, ⌘Z gives the value back.
    goto(&mut store, &mut workspace, &mut view, 1, 3);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Text("typed a long reason".into())],
    );
    press(&mut store, &mut workspace, &mut view, &[cmd_shift('z')]);
    assert_eq!(
        draft(&view),
        "typed a long reason",
        "⌘⇧Z with nothing undone"
    );
    press(&mut store, &mut workspace, &mut view, &[cmd('z')]);
    assert_eq!(draft(&view), "email");
    press(&mut store, &mut workspace, &mut view, &[cmd_shift('z')]);
    assert_eq!(draft(&view), "typed a long reason", "⌘⇧Z redoes");
    // A caret move ends a run: two runs are two undos; ⌘Y redoes, C-/ undoes.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[
            named(NamedKey::Home),
            ch('A', Modifiers::empty()),
            ch(':', Modifiers::empty()),
            named(NamedKey::End),
            named(NamedKey::Backspace),
            named(NamedKey::Backspace),
        ],
    );
    assert_eq!(draft(&view), "A:typed a long reas");
    press(&mut store, &mut workspace, &mut view, &[cmd('z')]);
    assert_eq!(
        draft(&view),
        "A:typed a long reason",
        "the deletes are one step"
    );
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('/', Modifiers::CTRL)],
    );
    assert_eq!(draft(&view), "typed a long reason", "C-/ undoes too");
    press(&mut store, &mut workspace, &mut view, &[cmd('y')]);
    assert_eq!(draft(&view), "A:typed a long reason", "⌘Y redoes");
    // A paste is its own step; a new edit ends what redo could bring back.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Paste("!".into()), cmd('z')],
    );
    assert_eq!(draft(&view), "A:typed a long reason");
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('?', Modifiers::empty()), cmd_shift('z')],
    );
    assert_eq!(
        draft(&view),
        "A:typed a long reason?",
        "nothing left to redo"
    );
    // Opened on its value (Return), undo back to the start is the value.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Escape)],
    );
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[
            named(NamedKey::Enter),
            ch('!', Modifiers::empty()),
            cmd('z'),
            cmd('z'),
        ],
    );
    assert_eq!(draft(&view), "email");
}

/// The owner's file in a window 110 cells wide: on the correction column the grid still
/// shows which computer and which guess the row is (the first column pinned, long columns
/// capped to a quarter of the row), and a sideways scroll reaches every column with the
/// cursor left where it was.
#[test]
fn the_first_column_stays_pinned_and_the_columns_scroll_sideways() {
    let cells = 110;
    let (mut store, mut workspace, mut view, _) = grid_editor(RACE);
    view.set_grid_width(cells);
    let projection = |store: &DocumentStore, view: &EditorBufferView| {
        let snapshot = store.snapshot(view.document).unwrap();
        let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
        project_grid(&snapshot.text, &table, view, 20, cells)
    };
    let shown = |store: &DocumentStore, view: &EditorBufferView| {
        projection(store, view)
            .shown
            .iter()
            .map(|shown| shown.index)
            .collect::<Vec<_>>()
    };
    goto(&mut store, &mut workspace, &mut view, 1, 0);
    assert!(
        !projection(&store, &view).pinned,
        "nothing to pin at the start"
    );
    assert!(projection(&store, &view).hidden_right);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::End)],
    );
    let end = projection(&store, &view);
    assert!(end.pinned, "{:?}", shown(&store, &view));
    assert_eq!(shown(&store, &view), vec![0, 3, 4, 5]);
    assert!(end.hidden_left && !end.hidden_right);
    assert!(
        end.shown.iter().all(|shown| shown.width <= cells / 4),
        "no column takes more than a quarter of the row: {:?}",
        end.shown
    );
    // Left and a press on a shown cell keep the columns where they show.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::ArrowLeft)],
    );
    assert_eq!(shown(&store, &view), vec![0, 3, 4, 5]);
    goto(&mut store, &mut workspace, &mut view, 2, 3);
    assert_eq!(shown(&store, &view), vec![0, 3, 4, 5]);

    // From the first column, sideways scrolls reach the last and come back; the cursor stays.
    goto(&mut store, &mut workspace, &mut view, 2, 0);
    let start = shown(&store, &view);
    let scroll = |store: &mut DocumentStore,
                  workspace: &mut EditorWorkspace,
                  view: &mut EditorBufferView,
                  columns: i32| {
        press(
            store,
            workspace,
            view,
            &[EditorInput::GridScroll { columns }],
        );
    };
    scroll(&mut store, &mut workspace, &mut view, 1);
    let once = shown(&store, &view);
    assert_ne!(once, start, "the first step shows a column it hid");
    scroll(&mut store, &mut workspace, &mut view, 1);
    assert_eq!(shown(&store, &view), vec![0, 3, 4, 5]);
    scroll(&mut store, &mut workspace, &mut view, 5);
    assert_eq!(
        shown(&store, &view),
        vec![0, 3, 4, 5],
        "no further than the last"
    );
    assert_eq!(cell_of(&store, &mut workspace, &view), (2, 0));
    scroll(&mut store, &mut workspace, &mut view, -1);
    assert_eq!(shown(&store, &view), once);
    scroll(&mut store, &mut workspace, &mut view, -1);
    assert_eq!(shown(&store, &view), start);
    // Scrolled away from the cursor, a move brings its column back.
    scroll(&mut store, &mut workspace, &mut view, 2);
    goto(&mut store, &mut workspace, &mut view, 2, 1);
    assert!(shown(&store, &view).contains(&1));
    // With a cell open, a sideways scroll keeps the open cell in view.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Enter)],
    );
    let editing = shown(&store, &view);
    scroll(&mut store, &mut workspace, &mut view, 3);
    assert_eq!(shown(&store, &view), editing);
}

/// The panel is as tall as the current cell wraps to, so a short cell leaves the rows to the
/// grid; a move keeps the cursor inside the rows the tallest panel leaves, so a long cell's
/// panel never covers its row.
#[test]
fn the_panel_fits_its_cell_and_never_covers_the_cursor() {
    let long = "word ".repeat(100);
    let text = format!(
        "h1,h2\n{}",
        (0..60)
            .map(|n| format!("r{n},{}\n", if n == 30 { long.as_str() } else { "v" }))
            .collect::<String>()
    );
    let (mut store, mut workspace, mut view, _) = grid_editor(&text);
    view.set_grid_width(60);
    let project = |store: &DocumentStore, view: &EditorBufferView| {
        let snapshot = store.snapshot(view.document).unwrap();
        let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
        project_grid(&snapshot.text, &table, view, 20, 60)
    };
    goto(&mut store, &mut workspace, &mut view, 5, 1);
    let short = project(&store, &view);
    assert_eq!(short.rows_layout, GridRows { data: 17, panel: 1 });
    assert_eq!(short.rows.len(), 17);
    for record in 6..=31 {
        goto(&mut store, &mut workspace, &mut view, record, 1);
        let projection = project(&store, &view);
        assert!(
            projection.rows.iter().any(|row| row.record == record),
            "row {record} shows with a panel of {}",
            projection.rows_layout.panel
        );
    }
    let tall = project(&store, &view);
    assert_eq!(
        tall.rows_layout,
        GridRows::of(20),
        "a long cell's panel is the tallest"
    );
    assert!(tall.panel.total_lines > tall.rows_layout.panel);
}

/// G2: M-w and ⌘C copy the cell's value, not its quoted bytes, onto the kill ring and the
/// clipboard, and leave the document alone; a copy never joins the kill before it, and an
/// empty or missing cell copies nothing.
#[test]
fn g2_a_copy_takes_the_value_not_its_quoted_bytes() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
    goto(&mut store, &mut workspace, &mut view, 2, 5);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Paste("say \"hi\"".into())],
    );
    let pasted = text_of(&store, document);
    assert!(pasted.contains(",\"say \"\"hi\"\"\"\n"), "{pasted}");
    let seq = store.snapshot(document).unwrap().seq;
    for copy in [ch('w', Modifiers::ALT), ch('c', Modifiers::SUPER)] {
        workspace.kill_ring.clear();
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            std::slice::from_ref(&copy),
        );
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("say \"hi\""),
            "{copy:?}"
        );
        assert!(
            effects.contains(&EditorEffect::KillToClipboard {
                text: "say \"hi\"".into()
            }),
            "{copy:?}: {effects:?}"
        );
        assert_eq!(store.snapshot(document).unwrap().seq, seq, "{copy:?}");
        assert_eq!(cell_of(&store, &mut workspace, &view), (2, 5));
    }
    goto(&mut store, &mut workspace, &mut view, 1, 1);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('w', Modifiers::ALT)],
    );
    assert_eq!(
        workspace.kill_ring.iter().take(2).collect::<Vec<_>>(),
        ["alice@example.test", "say \"hi\""]
    );
    for (record, column) in [(1, 5), (3, 5)] {
        goto(&mut store, &mut workspace, &mut view, record, column);
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('w', Modifiers::ALT)],
        );
        assert_eq!(effects, [EditorEffect::Status(EMPTY_CELL.to_string())]);
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("alice@example.test")
        );
    }
}

/// G2: C-w, C-k and ⌘X cut the cell: its value onto the kill ring and the clipboard, the cell
/// cleared as one undo frame over its own bytes, which undo gives back. An empty cell cuts
/// nothing.
#[test]
fn g2_a_cut_clears_the_cell_in_one_undo_frame() {
    for cut in [
        ch('w', Modifiers::CTRL),
        ch('k', Modifiers::CTRL),
        ch('x', Modifiers::SUPER),
    ] {
        let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
        goto(&mut store, &mut workspace, &mut view, 2, 2);
        let before = store.snapshot(document).unwrap().seq;
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            std::slice::from_ref(&cut),
        );
        assert_eq!(
            text_of(&store, document),
            MIXED.replace("\"Said \"\"no\"\"\"", "\"\""),
            "{cut:?}"
        );
        assert_eq!(
            store.snapshot(document).unwrap().seq.0,
            before.0 + 1,
            "{cut:?}"
        );
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("Said \"no\""),
            "{cut:?}"
        );
        assert!(
            effects.contains(&EditorEffect::KillToClipboard {
                text: "Said \"no\"".into()
            }),
            "{cut:?}: {effects:?}"
        );
        assert_eq!(cell_of(&store, &mut workspace, &view), (2, 2));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('z', Modifiers::SUPER)],
        );
        assert_eq!(
            text_of(&store, document),
            MIXED,
            "{cut:?}: undo gives it back"
        );
        assert!(!workspace.can_undo(document), "{cut:?}: one undo frame");
    }
    let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
    goto(&mut store, &mut workspace, &mut view, 1, 3);
    let effects = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('w', Modifiers::CTRL)],
    );
    assert_eq!(effects, [EditorEffect::Status(EMPTY_CELL.to_string())]);
    assert_eq!(text_of(&store, document), MIXED);
    assert!(workspace.kill_ring.is_empty());
}

/// G2: C-y puts the newest kill in the cell as one undo frame, quoted as the cell needs and
/// with one final line break dropped (a killed line's); an empty kill ring says so.
#[test]
fn g2_yank_puts_the_newest_kill_in_the_cell() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
    let mut probe = view.clone();
    assert_eq!(
        workspace.handle_input(&mut store, &mut probe, ch('y', Modifiers::CTRL)),
        Err(EditorError::NoKill)
    );
    goto(&mut store, &mut workspace, &mut view, 2, 1);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('w', Modifiers::ALT)],
    );
    goto(&mut store, &mut workspace, &mut view, 1, 3);
    let before = store.snapshot(document).unwrap().seq;
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('y', Modifiers::CTRL)],
    );
    let yanked = MIXED.replace("and two\",\r\n", "and two\",frank@example.test\r\n");
    assert_eq!(text_of(&store, document), yanked);
    assert_eq!(store.snapshot(document).unwrap().seq.0, before.0 + 1);
    assert_eq!(cell_of(&store, &mut workspace, &view), (1, 3));
    workspace.kill_ring.push_front("a, b\n".into());
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('y', Modifiers::CTRL)],
    );
    assert_eq!(
        text_of(&store, document),
        MIXED.replace("and two\",\r\n", "and two\",\"a, b\"\r\n")
    );
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('z', Modifiers::SUPER)],
    );
    assert_eq!(text_of(&store, document), yanked);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('z', Modifiers::SUPER)],
    );
    assert_eq!(text_of(&store, document), MIXED);
}

/// G2: C-o adds a row below with the header's number of empty cells and the line break of the
/// row above (a break before it when the file had no final newline) and moves to its first
/// cell; C-S-backspace deletes the row; `grid-insert-row` and `grid-delete-row` (the toolbar's
/// buttons, M-x) do the same. Each is one undo frame over its own row's bytes, on the CRLF,
/// mixed and no-final-newline fixtures, and an undo of a delete puts the cursor on the row
/// put back. *Control (review):* before the cursor fix, undoing Delete Row on a last row with
/// no line break left the cursor at the end of the row above, (1, 3) on the mixed fixture.
#[test]
fn g2_rows_are_added_and_deleted_as_one_undo_frame() {
    let c_o = ch('o', Modifiers::CTRL);
    let kill_row = key(
        Key::Named(NamedKey::Backspace),
        Modifiers::CTRL | Modifiers::SHIFT,
    );
    type Row = (
        &'static str,
        (usize, usize),
        EditorInput,
        String,
        (usize, usize),
    );
    let rows: Vec<Row> = vec![
        (
            MIXED,
            (1, 2),
            c_o.clone(),
            MIXED.replace("and two\",\r\n", "and two\",\r\n,,,\r\n"),
            (2, 0),
        ),
        (
            MIXED,
            (2, 3),
            c_o.clone(),
            format!("{MIXED}\r\n,,,"),
            (3, 0),
        ),
        (
            MIXED,
            (0, 1),
            c_o.clone(),
            MIXED.replacen("fix\r\n", "fix\r\n,,,\r\n", 1),
            (1, 0),
        ),
        (
            ALL_CRLF,
            (2, 0),
            c_o.clone(),
            format!("{ALL_CRLF},,,\r\n"),
            (3, 0),
        ),
        (
            ALL_CRLF,
            (1, 1),
            c_o.clone(),
            ALL_CRLF.replace("line two\",\r\n", "line two\",\r\n,,,\r\n"),
            (2, 0),
        ),
        (
            RACE,
            (3, 2),
            c_o.clone(),
            RACE.replace("Claude\r\n", "Claude\r\n,,,,,\r\n"),
            (4, 0),
        ),
        (
            RACE,
            (4, 5),
            c_o.clone(),
            format!("{RACE}\r\n,,,,,"),
            (5, 0),
        ),
        (
            MIXED,
            (2, 2),
            kill_row.clone(),
            MIXED.replace("\r\nm2,frank@example.test,\"Said \"\"no\"\"\",later", ""),
            (1, 2),
        ),
        (
            MIXED,
            (1, 0),
            kill_row.clone(),
            MIXED.replace("m1,erin@example.test,\"one\nand two\",\r\n", ""),
            (1, 0),
        ),
        (
            ALL_CRLF,
            (1, 3),
            kill_row.clone(),
            ALL_CRLF.replace("m1,gina@example.test,\"line one\r\nline two\",\r\n", ""),
            (1, 3),
        ),
        (
            ALL_CRLF,
            (2, 1),
            kill_row.clone(),
            ALL_CRLF.replace("m2,hal@example.test,plain,\r\n", ""),
            (1, 1),
        ),
        (
            RACE,
            (4, 1),
            kill_row.clone(),
            RACE.replace("\r\nm5,erin@example.test,Claude,SMS,phone on file,", ""),
            (3, 1),
        ),
    ];
    for (fixture, start, input, expected, after) in rows {
        let name = format!("{input:?} at {start:?} of {fixture:?}");
        let (mut store, mut workspace, mut view, document) = grid_editor(fixture);
        goto(&mut store, &mut workspace, &mut view, start.0, start.1);
        let before = store.snapshot(document).unwrap().seq;
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            std::slice::from_ref(&input),
        );
        assert!(effects.is_empty(), "{name}: {effects:?}");
        assert_eq!(text_of(&store, document), expected, "{name}");
        assert_eq!(
            store.snapshot(document).unwrap().seq.0,
            before.0 + 1,
            "{name}: one transaction"
        );
        assert_eq!(cell_of(&store, &mut workspace, &view), after, "{name}");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('z', Modifiers::SUPER)],
        );
        assert_eq!(text_of(&store, document), fixture, "{name}: undo");
        assert!(!workspace.can_undo(document), "{name}: one undo frame");
        if input == kill_row {
            // The cell the undo changed: the row put back, a last row with no line break too.
            assert_eq!(
                cell_of(&store, &mut workspace, &view),
                (start.0, 0),
                "{name}: undo puts the cursor on the row put back"
            );
        }
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('z', Modifiers::SUPER | Modifiers::SHIFT)],
        );
        assert_eq!(text_of(&store, document), expected, "{name}: redo");
        if input == c_o {
            assert_eq!(
                cell_of(&store, &mut workspace, &view),
                after,
                "{name}: redo puts the cursor on the row added again"
            );
        }

        // The command the toolbar button and M-x run does what the key does.
        let command = if input == c_o {
            EditorCommand::GridInsertRow
        } else {
            EditorCommand::GridDeleteRow
        };
        let (mut store, mut workspace, mut view, document) = grid_editor(fixture);
        goto(&mut store, &mut workspace, &mut view, start.0, start.1);
        workspace
            .invoke(&mut store, &mut view, Invocation::new(command))
            .unwrap();
        assert_eq!(text_of(&store, document), expected, "{name}: {command:?}");
        assert_eq!(cell_of(&store, &mut workspace, &view), after, "{name}");
    }
}

/// G2: a row command waits while a cell is open, says where it works in a text view, and
/// leaves alone a row whose quote never closes (it holds every row after it).
#[test]
fn g2_rows_wait_for_an_open_cell_and_spare_an_unclosed_row() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
    goto(&mut store, &mut workspace, &mut view, 1, 3);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Text("done".into())],
    );
    let effects = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('o', Modifiers::CTRL)],
    );
    assert!(
        matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.starts_with("C-o is not a cell key")),
        "{effects:?}"
    );
    for command in [EditorCommand::GridInsertRow, EditorCommand::GridDeleteRow] {
        let effects = workspace.execute(&mut store, &mut view, command).unwrap();
        assert_eq!(effects, [EditorEffect::Status(DRAFT_HOLDS.to_string())]);
    }
    assert_eq!(view.grid_draft().unwrap().text, "done");
    assert_eq!(text_of(&store, document), MIXED);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Escape), ch('e', Modifiers::SUPER)],
    );
    assert!(view.grid().is_none());
    for command in [EditorCommand::GridInsertRow, EditorCommand::GridDeleteRow] {
        let effects = workspace.execute(&mut store, &mut view, command).unwrap();
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.starts_with(&format!("{} works in a table's grid view", command.name()))),
            "{effects:?}"
        );
    }
    assert_eq!(text_of(&store, document), MIXED);

    let (mut store, mut workspace, mut view, document) = grid_editor(STRAY);
    goto(&mut store, &mut workspace, &mut view, 2, 0);
    for input in [
        ch('o', Modifiers::CTRL),
        key(
            Key::Named(NamedKey::Backspace),
            Modifiers::CTRL | Modifiers::SHIFT,
        ),
    ] {
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            std::slice::from_ref(&input),
        );
        assert_eq!(
            effects,
            [EditorEffect::Status(ROW_HOLDS_REST.to_string())],
            "{input:?}"
        );
        assert_eq!(text_of(&store, document), STRAY, "{input:?}");
    }
    goto(&mut store, &mut workspace, &mut view, 0, 0);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('o', Modifiers::CTRL)],
    );
    assert_eq!(
        text_of(&store, document),
        STRAY.replacen("d\n", "d\n,,\n", 1)
    );

    // A row between a lone CR and a blank LF line: deleting it would drop the blank line too.
    let joined = "a,b\r1,2\n\n3,4\n";
    let (mut store, mut workspace, mut view, document) = grid_editor(joined);
    goto(&mut store, &mut workspace, &mut view, 1, 0);
    let effects = press(
        &mut store,
        &mut workspace,
        &mut view,
        &[key(
            Key::Named(NamedKey::Backspace),
            Modifiers::CTRL | Modifiers::SHIFT,
        )],
    );
    assert_eq!(
        effects,
        [EditorEffect::Status(ROW_JOINS_BREAKS.to_string())]
    );
    assert_eq!(text_of(&store, document), joined);
}

/// G2 review: the terminal face never shows a grid, so its row commands (C-S-backspace,
/// `M-x grid-insert-row`, `grid-delete-row`) point to an aterm Editor tab instead of the C-c
/// C-t the terminal refuses. *Control:* before this fix they said to press C-c C-t.
#[test]
fn g2_row_commands_in_the_terminal_point_to_an_editor_tab() {
    let mut store = DocumentStore::for_test();
    let document = store.open("file:///tmp/t.csv".into(), "a,b\n".into());
    let mut workspace = EditorWorkspace::with_keymap(Keymap::emacs(Profile::Terminal));
    let mut view = workspace
        .attach(&mut store, document, DocumentViewId(6))
        .unwrap();
    for command in [EditorCommand::GridInsertRow, EditorCommand::GridDeleteRow] {
        let effects = workspace.execute(&mut store, &mut view, command).unwrap();
        assert_eq!(
            effects,
            vec![EditorEffect::Status(TERMINAL_TABLE.to_string())],
            "{command:?}"
        );
    }
    let effects = workspace
        .handle_input(
            &mut store,
            &mut view,
            key(
                Key::Named(NamedKey::Backspace),
                Modifiers::CTRL | Modifiers::SHIFT,
            ),
        )
        .unwrap()
        .effects;
    assert_eq!(
        effects,
        vec![EditorEffect::Status(TERMINAL_TABLE.to_string())]
    );
    assert_eq!(text_of(&store, document), "a,b\n");
}

/// G2 review: a value the grid copied or cut comes back whole on C-y or a paste, its final
/// line break included: C-w then C-y in place leaves the bytes as they were for a value
/// ending in LF or CRLF, for one ending in two breaks and for a lone break, and M-w then C-y
/// (or a paste of that same text) in another cell writes the value copied. A kill from the
/// text view still drops one final break. *Control:* the yank before this fix put every kill
/// through `cell_text`, so `"x\n"` came back `"x"` and a lone-break cell stayed `""`.
#[test]
fn g2_a_grid_value_yanks_back_whole() {
    let cut = ch('w', Modifiers::CTRL);
    let yank = ch('y', Modifiers::CTRL);
    for cell in ["\"x\n\"", "\"\n\"", "\"line\r\n\"", "\"a\nb\n\n\""] {
        let text = format!("h1,h2\r\n{cell},y\r\n");
        let (mut store, mut workspace, mut view, document) = grid_editor(&text);
        goto(&mut store, &mut workspace, &mut view, 1, 0);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            std::slice::from_ref(&cut),
        );
        assert_eq!(
            text_of(&store, document),
            "h1,h2\r\n\"\",y\r\n",
            "{cell}: cleared, its quotes kept"
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            std::slice::from_ref(&yank),
        );
        assert_eq!(
            text_of(&store, document),
            text,
            "{cell}: cut and yank in place"
        );
    }
    let text = "h1,h2\n\"a\nb\n\",\n";
    let copied = "h1,h2\n\"a\nb\n\",\"a\nb\n\"\n";
    let (mut store, mut workspace, mut view, document) = grid_editor(text);
    goto(&mut store, &mut workspace, &mut view, 1, 0);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('w', Modifiers::ALT)],
    );
    goto(&mut store, &mut workspace, &mut view, 1, 1);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        std::slice::from_ref(&yank),
    );
    assert_eq!(text_of(&store, document), copied, "M-w then C-y");
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[
            ch('z', Modifiers::SUPER),
            EditorInput::Paste("a\nb\n".into()),
        ],
    );
    assert_eq!(text_of(&store, document), copied, "M-w then a paste");
    // A text kill is not a cell's value: its final break goes.
    workspace.record_kill("z\n".to_string(), None, false);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        std::slice::from_ref(&yank),
    );
    assert_eq!(
        text_of(&store, document),
        "h1,h2\n\"a\nb\n\",\"z\"\n",
        "the quoted cell keeps its quotes; the kill's break goes"
    );
}

/// G2 review: a document held with LF for CRLF on disk (`crlf_on_disk`, which the GUI sets)
/// takes a yanked kill's CRLF as LF, in the grid and in the text view, so its file does not
/// save CR CR LF. *Control:* with `crlf_on_disk` off, the same yank writes the CRLF, as every
/// yank did before this fix.
#[test]
fn g2_a_yank_into_a_crlf_held_document_writes_lf() {
    let text = "h1,h2\na,\n";
    for held in [true, false] {
        let (mut store, mut workspace, mut view, document) = grid_editor(text);
        workspace.crlf_on_disk = held;
        workspace.kill_ring.push_front("x\r\ny".into());
        goto(&mut store, &mut workspace, &mut view, 1, 1);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('y', Modifiers::CTRL)],
        );
        let cell = if held { "\"x\ny\"" } else { "\"x\r\ny\"" };
        assert_eq!(
            text_of(&store, document),
            format!("h1,h2\na,{cell}\n"),
            "held={held}"
        );

        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('z', Modifiers::SUPER), ch('e', Modifiers::SUPER)],
        );
        assert!(view.grid().is_none());
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('y', Modifiers::CTRL)],
        );
        let after = text_of(&store, document);
        assert_eq!(after.contains('\r'), !held, "held={held}: {after:?}");
        assert!(
            after.contains(if held { "x\ny" } else { "x\r\ny" }),
            "{after:?}"
        );
    }
}

/// G2: a press in the cell panel puts the cell's caret at the boundary nearest it, opening
/// the cell first when it was closed; nothing lands before Return, and a press inside a wide
/// character lands on a boundary.
#[test]
fn g2_a_press_in_the_panel_places_the_caret() {
    let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
    let project = |store: &DocumentStore, view: &EditorBufferView| {
        let snapshot = store.snapshot(view.document).unwrap();
        let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
        project_grid(&snapshot.text, &table, view, 20, 60)
    };
    goto(&mut store, &mut workspace, &mut view, 2, 4);
    let value = "Two reasons: first\nthe phone prompt, then the code";
    let grid = project(&store, &view);
    assert_eq!(grid.panel_value(), value);
    assert_eq!(grid.panel.ranges, [0..18, 19..value.len()]);
    assert_eq!(
        grid.panel_byte(1, 8),
        Some(23),
        "the second line, four cells (eight half cells) in"
    );
    assert_eq!(
        grid.panel_byte(1, 9),
        Some(24),
        "the second half of a cell: after it"
    );
    assert_eq!(
        grid.panel_byte(0, 800),
        Some(18),
        "past a line's end, its end"
    );
    assert_eq!(grid.panel_byte(2, 0), None, "past the shown lines");
    let seq = store.snapshot(document).unwrap().seq;
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::GridPanelPointer { byte: 23 }],
    );
    let draft = view.grid_draft().expect("the press opens the cell");
    assert_eq!((draft.text.as_str(), draft.caret), (value, 23));
    assert_eq!(project(&store, &view).panel_byte(0, 6), Some(3));
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[
            EditorInput::GridPanelPointer { byte: 3 },
            ch('!', Modifiers::empty()),
        ],
    );
    assert_eq!(
        view.grid_draft().unwrap().text,
        "Two! reasons: first\nthe phone prompt, then the code"
    );
    assert_eq!(
        store.snapshot(document).unwrap().seq,
        seq,
        "nothing lands yet"
    );
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[named(NamedKey::Escape)],
    );
    goto(&mut store, &mut workspace, &mut view, 1, 5);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::Text("日本語".into()), named(NamedKey::Enter)],
    );
    goto(&mut store, &mut workspace, &mut view, 1, 5);
    let grid = project(&store, &view);
    assert_eq!(grid.panel_value(), "日本語");
    assert_eq!(grid.panel_byte(0, 0), Some(0));
    assert_eq!(
        grid.panel_byte(0, 1),
        Some(0),
        "the second quarter of a wide character: before it"
    );
    assert_eq!(grid.panel_byte(0, 2), Some(3), "its far half: after it");
    assert_eq!(grid.panel_byte(0, 5), Some(3));
    assert_eq!(grid.panel_byte(0, 6), Some(6));
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[EditorInput::GridPanelPointer { byte: 4 }],
    );
    assert_eq!(
        view.grid_draft().unwrap().caret,
        3,
        "inside a character: its start"
    );
}

#[test]
fn search_and_goto_line_sync_grid_column_and_scroll_and_c_g_restores_origin() {
    let (mut store, mut workspace, mut view, _) = grid_editor(MAIN);
    goto(&mut store, &mut workspace, &mut view, 1, 6);
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('g', Modifiers::CTRL)],
    );
    assert_eq!(
        cell_of(&store, &mut workspace, &view),
        (1, 6),
        "C-g outside a search/goto-line minibuffer keeps a missing cell's column"
    );

    // From missing cell (1, 6), searching for "authenticator" (the last present cell of short
    // row 3) must land on (3, 3), not falsely retain column 6.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[
            ch('s', Modifiers::CTRL),
            EditorInput::Text("authenticator".into()),
            named(NamedKey::Enter),
        ],
    );
    assert_eq!(cell_of(&store, &mut workspace, &view), (3, 3));

    // In a tall table with a short viewport, search, search-next, C-g, goto-line, and
    // recenter-top-bottom keep the grid cursor and scroll in sync.
    let mut csv = String::from("id,name,notes\n");
    for idx in 1..=20 {
        csv.push_str(&format!("r{idx},row-{idx},val-{idx}\n"));
    }
    let (mut store, mut workspace, mut view, _) = grid_editor(&csv);
    view.reconcile_viewport(&csv, 8);
    goto(&mut store, &mut workspace, &mut view, 1, 2);
    workspace.scroll_grid_columns(&store, &mut view, 1).unwrap();
    assert!(view.grid().unwrap().scrolled);

    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('s', Modifiers::CTRL), EditorInput::Text("val-18".into())],
    );
    assert_eq!(cell_of(&store, &mut workspace, &view), (18, 2));
    assert!(!view.grid().unwrap().scrolled);
    let data = GridRows::of(view.viewport_lines()).data;
    let top = view.grid().unwrap().top_record;
    assert!(
        (top..top + data).contains(&18),
        "incremental search scrolls top_record ({top}) to show row 18 (data={data})"
    );

    // Cancelling incremental search with C-g restores origin (1, 2) and scrolls back to it.
    press(
        &mut store,
        &mut workspace,
        &mut view,
        &[ch('g', Modifiers::CTRL)],
    );
    assert_eq!(cell_of(&store, &mut workspace, &view), (1, 2));
    let top = view.grid().unwrap().top_record;
    assert!((top..top + data).contains(&1));

    // goto-line jumps to row 19 column 0 and scrolls it into view.
    workspace
        .invoke(
            &mut store,
            &mut view,
            crate::editor::Invocation {
                command: EditorCommand::GotoLine,
                count: None,
                args: vec!["20".into()],
            },
        )
        .unwrap();
    assert_eq!(cell_of(&store, &mut workspace, &view), (19, 0));
    let top = view.grid().unwrap().top_record;
    assert!((top..top + data).contains(&19));

    // RecenterTopBottom on the last row clamps top_record to max_top.
    goto(&mut store, &mut workspace, &mut view, 20, 0);
    workspace
        .execute(&mut store, &mut view, EditorCommand::RecenterTopBottom)
        .unwrap();
    let max_top = 21usize.saturating_sub(data).max(1);
    assert_eq!(view.grid().unwrap().top_record, max_top);
}

#[test]
fn undo_agent_edit_in_grid_places_cursor_on_undone_cell() {
    use crate::editor::{Origin, Request};
    use crate::store::TextEdit;

    let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
    let snapshot = store.snapshot(document).unwrap();
    let target = snapshot.text.find("authenticator").unwrap();
    let (seq, deltas) = workspace
        .apply(
            &mut store,
            document,
            snapshot.seq,
            Request {
                edits: vec![TextEdit {
                    range: target..target + "authenticator".len(),
                    insert: "passkey".into(),
                }],
                origin: Origin::Agent {
                    principal: "@1".into(),
                    label: "update factor".into(),
                },
            },
        )
        .unwrap();
    view.observe_external(seq, &deltas);
    goto(&mut store, &mut workspace, &mut view, 1, 0);
    workspace
        .execute(&mut store, &mut view, EditorCommand::UndoAgentEdit)
        .unwrap();
    assert_eq!(cell_of(&store, &mut workspace, &view), (3, 3));
    assert_eq!(text_of(&store, document), MAIN);
}
