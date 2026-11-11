// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::editor::editor;
use crate::editor::kill::YANK_POP_NEEDS_YANK;
use crate::store::DocumentTxnOutcome;

#[test]
fn directory_navigation_uses_exact_targets_and_cannot_edit() {
    use crate::input::EditorInput;
    use aterm_types::keyboard::{Key, Modifiers};
    let (mut store, mut workspace, mut view, document) = editor("");
    store
        .set_directory(
            document,
            crate::store::DirectoryListing {
                text: "  escaped\\nname\n".into(),
                parent: Some("file:///parent".into()),
                entries: vec![(0..16, "file:///parent/escaped%0Aname".into())],
            },
        )
        .unwrap();
    view.anchor_seq = store.snapshot(document).unwrap().seq;
    let before = store.snapshot(document).unwrap();
    assert!(
        workspace
            .insert_text(&mut store, &mut view, "")
            .unwrap()
            .is_empty()
    );
    assert!(
        workspace
            .handle_input(&mut store, &mut view, EditorInput::Paste(String::new()))
            .unwrap()
            .effects
            .is_empty()
    );
    assert_eq!(
        workspace
            .execute(&mut store, &mut view, EditorCommand::Newline)
            .unwrap(),
        vec![EditorEffect::OpenPath {
            uri: "file:///parent/escaped%0Aname".into()
        }]
    );
    for (key, expected) in [
        (
            '^',
            EditorEffect::OpenPath {
                uri: "file:///parent".into(),
            },
        ),
        ('g', EditorEffect::RefreshDirectory { document }),
    ] {
        assert_eq!(
            workspace
                .handle_input(
                    &mut store,
                    &mut view,
                    EditorInput::Key {
                        key: Key::Character(if key == '^' { '6' } else { key }),
                        mods: if key == '^' {
                            Modifiers::SHIFT
                        } else {
                            Modifiers::empty()
                        },
                        text: Some(key.to_string()),
                    }
                )
                .unwrap()
                .effects,
            vec![expected]
        );
    }
    workspace
        .insert_text(&mut store, &mut view, "oops")
        .unwrap();
    workspace
        .execute(&mut store, &mut view, EditorCommand::DeleteForward)
        .unwrap();
    assert!(matches!(
        store.replace_text(document, before.seq, "oops"),
        DocumentTxnOutcome::Rejected(_)
    ));
    assert_eq!(store.snapshot(document).unwrap().text, before.text);
    assert_eq!(store.dirty(document), Some(false));
    workspace
        .granted
        .retain(|authority| *authority != CommandAuthority::Files);
    assert!(matches!(
        workspace
            .execute(&mut store, &mut view, EditorCommand::Newline)
            .unwrap()
            .as_slice(),
        [EditorEffect::Status(_)]
    ));
}

#[test]
fn multi_selection_edit_is_one_document_transaction() {
    let (mut store, mut workspace, mut view, document) = editor("one two");
    let before = store.snapshot(document).unwrap().seq;
    view.selections = vec![Selection::caret(0), Selection::caret(4)];
    workspace.insert_text(&mut store, &mut view, "_").unwrap();
    let snapshot = store.snapshot(document).unwrap();
    assert_eq!(snapshot.seq.0, before.0 + 1);
    assert_eq!(snapshot.text.as_ref(), "_one _two");
    assert_eq!(
        view.selections,
        vec![Selection::caret(1), Selection::caret(6)]
    );
}

#[test]
fn stale_view_changes_neither_text_nor_selection() {
    let (mut store, mut workspace, mut view, document) = editor("abc");
    let base = store.snapshot(document).unwrap().seq;
    assert!(matches!(
        store.transact(
            document,
            base,
            vec![TextEdit {
                range: 0..0,
                insert: "x".into(),
            }]
        ),
        DocumentTxnOutcome::Committed { .. }
    ));
    let before_selection = view.selections.clone();
    assert!(matches!(
        workspace.insert_text(&mut store, &mut view, "y"),
        Err(EditorError::StaleView { .. })
    ));
    assert_eq!(view.selections, before_selection);
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "xabc");
}

#[test]
fn prefix_trie_never_times_out_and_c_g_cancels() {
    let (mut store, mut workspace, mut view, _) = editor("abc");
    workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("C-x").unwrap())
        .unwrap();
    assert!(view.prefix_hud.as_deref().unwrap().contains("C-s"));
    workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("C-g").unwrap())
        .unwrap();
    assert_eq!(view.prefix_hud, None);
    assert!(matches!(view.minibuffer, Minibuffer::Inactive));
}

#[test]
fn unbound_keys_and_idle_bound_commands_say_their_own_reason() {
    let (mut store, mut workspace, mut view, _) = editor("abc");
    workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("C-x").unwrap())
        .unwrap();
    let effects = workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("z").unwrap())
        .unwrap();
    assert!(
        matches!(effects.as_slice(), [EditorEffect::Status(message)] if message.contains("not bound")),
        "{effects:?}"
    );
    let effects = workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("C-w").unwrap())
        .unwrap();
    assert!(
        matches!(effects.as_slice(), [EditorEffect::Status(message)] if !message.contains("not bound")),
        "{effects:?}"
    );
    let effects = workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("M-y").unwrap())
        .unwrap();
    assert_eq!(
        effects,
        [EditorEffect::Status(YANK_POP_NEEDS_YANK.to_string())]
    );
}

#[test]
fn c_x_c_s_emits_typed_save_at_exact_sequence() {
    let (mut store, mut workspace, mut view, document) = editor("abc");
    workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("C-x").unwrap())
        .unwrap();
    let effects = workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("C-s").unwrap())
        .unwrap();
    assert_eq!(
        effects,
        vec![EditorEffect::SaveDocument {
            document,
            seq: store.snapshot(document).unwrap().seq,
        }]
    );
}
