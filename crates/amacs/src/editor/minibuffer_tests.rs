// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::editor::{EditorCommand, KeyChord, KeyResolution, editor};
use aterm_models_native::native_editor_command_palette_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

#[test]
fn input_lifetime_survives_query_edits_navigation_and_rebase_but_not_reopening() {
    let (mut store, mut workspace, mut view, document) = editor("needle then needle");
    let document_input = view.input_lifetime();
    workspace
        .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
        .unwrap();
    let search_input = view.input_lifetime();
    assert!(!std::sync::Arc::ptr_eq(&document_input, &search_input));
    workspace
        .insert_text(&mut store, &mut view, "needle")
        .unwrap();
    workspace.minibuffer_backspace(&store, &mut view).unwrap();
    workspace.insert_text(&mut store, &mut view, "e").unwrap();
    for command in [EditorCommand::SearchNext, EditorCommand::SearchPrevious] {
        workspace.execute(&mut store, &mut view, command).unwrap();
    }
    assert!(std::sync::Arc::ptr_eq(
        &search_input,
        &view.input_lifetime()
    ));

    let mut writer = workspace
        .attach(&mut store, document, crate::store::DocumentViewId(92))
        .unwrap();
    workspace
        .insert_text(&mut store, &mut writer, "prefix ")
        .unwrap();
    for (seq, deltas) in workspace.take_commits(document) {
        view.observe_external(seq, &deltas);
    }
    assert!(std::sync::Arc::ptr_eq(
        &search_input,
        &view.input_lifetime()
    ));

    workspace
        .execute(&mut store, &mut view, EditorCommand::Abort)
        .unwrap();
    let cancelled = view.input_lifetime();
    assert!(!std::sync::Arc::ptr_eq(&search_input, &cancelled));
    workspace
        .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
        .unwrap();
    let reopened = view.input_lifetime();
    assert!(!std::sync::Arc::ptr_eq(&cancelled, &reopened));
    assert!(!std::sync::Arc::ptr_eq(&search_input, &reopened));
    workspace
        .insert_text(&mut store, &mut view, "needle")
        .unwrap();
    let selections = view.selections.clone();
    view.clear_minibuffer();
    assert_eq!(view.selections, selections);
    assert!(!view.minibuffer_active());
    assert!(!std::sync::Arc::ptr_eq(&reopened, &view.input_lifetime()));
}

#[test]
fn refused_modal_submission_keeps_its_input_lifetime() {
    for (command, query) in [
        (EditorCommand::ExecuteCommand, "no-such-editor-command"),
        (EditorCommand::GotoLine, "0"),
    ] {
        let (mut store, mut workspace, mut view, document) = editor("unchanged");
        let original = store.snapshot(document).unwrap();
        workspace.execute(&mut store, &mut view, command).unwrap();
        workspace.insert_text(&mut store, &mut view, query).unwrap();
        let input = view.input_lifetime();
        let minibuffer = view.minibuffer.clone();
        workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert_eq!(view.minibuffer, minibuffer);
        assert!(std::sync::Arc::ptr_eq(&input, &view.input_lifetime()));
        let after = store.snapshot(document).unwrap();
        assert_eq!(after.seq, original.seq);
        assert_eq!(after.text, original.text);
    }
}

#[test]
fn accepted_prompts_and_same_kind_replacements_get_new_input_lifetimes() {
    let (mut store, mut workspace, mut view, _) = editor("needle");
    workspace
        .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
        .unwrap();
    workspace
        .insert_text(&mut store, &mut view, "kill-buffer")
        .unwrap();
    let command_input = view.input_lifetime();
    workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert!(matches!(
        &view.minibuffer,
        Minibuffer::Prompt { invocation, .. } if invocation.command == EditorCommand::KillBuffer
    ));
    let prompt_input = view.input_lifetime();
    assert!(!std::sync::Arc::ptr_eq(&command_input, &prompt_input));
    workspace
        .insert_text(&mut store, &mut view, "other")
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &prompt_input,
        &view.input_lifetime()
    ));
    let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert_eq!(
        effects,
        [EditorEffect::KillBuffer {
            query: "other".into()
        }]
    );
    assert!(!view.minibuffer_active());
    assert!(!std::sync::Arc::ptr_eq(
        &prompt_input,
        &view.input_lifetime()
    ));

    workspace
        .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
        .unwrap();
    workspace
        .insert_text(&mut store, &mut view, "execute-extended-command")
        .unwrap();
    let before_replacement = view.input_lifetime();
    workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert!(matches!(&view.minibuffer, Minibuffer::Command { query, .. } if query.is_empty()));
    assert!(!std::sync::Arc::ptr_eq(
        &before_replacement,
        &view.input_lifetime()
    ));
    view.clear_minibuffer();

    for (command, query) in [
        (EditorCommand::IncrementalSearch, "needle"),
        (EditorCommand::GotoLine, "1"),
        (EditorCommand::SwitchBuffer, "other"),
    ] {
        workspace.execute(&mut store, &mut view, command).unwrap();
        workspace.insert_text(&mut store, &mut view, query).unwrap();
        let input = view.input_lifetime();
        workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert!(!view.minibuffer_active());
        assert!(!std::sync::Arc::ptr_eq(&input, &view.input_lifetime()));
    }
}

fn palette_projection(
    model: &Model,
    view: &EditorBufferView,
    query_phase: i64,
    query_changed: i64,
    submitted: i64,
    exact_selected_dispatch: i64,
) -> State {
    let mut state = model.init_state();
    match &view.minibuffer {
        Minibuffer::Command { query, selected } => {
            state.insert("mode", 1);
            state.insert("query_phase", query_phase);
            state.insert(
                "results",
                i64::try_from(command_completions(query).len().min(4))
                    .expect("bounded completion count"),
            );
            state.insert(
                "selected",
                i64::try_from(*selected).expect("bounded selection"),
            );
        }
        _ => {
            state.insert("mode", 0);
            state.insert("query_phase", 0);
            state.insert("results", 0);
            state.insert("selected", 0);
        }
    }
    state.insert("query_changed", query_changed);
    state.insert("submitted", submitted);
    state.insert("exact_selected_dispatch", exact_selected_dispatch);
    state
}

fn assert_palette_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(admits(model, before, after), Some(action));
    for invariant in &model.invariants {
        assert!(model.check_invariant(invariant.name, after));
    }
}

#[test]
fn unknown_command_and_buffer_query_are_safe_typed_outcomes() {
    let (mut store, mut workspace, mut view, document) = editor("unchanged");
    let original = store.snapshot(document).unwrap();
    workspace
        .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
        .unwrap();
    workspace
        .insert_text(&mut store, &mut view, "kill-everything-maybe")
        .unwrap();
    let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert!(matches!(view.minibuffer, Minibuffer::Command { .. }));
    assert!(
        matches!(effects.as_slice(), [EditorEffect::Status(message)] if message.contains("No editor command"))
    );
    let after_unknown = store.snapshot(document).unwrap();
    assert_eq!(after_unknown.seq, original.seq);
    assert_eq!(after_unknown.text, original.text);

    view.cancel_transient();
    workspace
        .execute(&mut store, &mut view, EditorCommand::SwitchBuffer)
        .unwrap();
    workspace
        .insert_text(&mut store, &mut view, "other.md")
        .unwrap();
    let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert_eq!(
        effects,
        vec![EditorEffect::SwitchBuffer {
            query: "other.md".to_string()
        }]
    );
    let after_buffer = store.snapshot(document).unwrap();
    assert_eq!(after_buffer.seq, original.seq);
    assert_eq!(after_buffer.text, original.text);
}

#[test]
fn goto_line_word_motion_and_revert_are_real_emacs_commands() {
    let (mut store, mut workspace, mut view, document) = editor("zero\none two\nthree four\n");
    view.selections = vec![Selection::caret("zero\none two\nthree".len())];
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveWordBackward)
        .unwrap();
    assert_eq!(
        &store.snapshot(document).unwrap().text[view.primary_selection().head..],
        "three four\n"
    );
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveWordForward)
        .unwrap();
    assert_eq!(
        &store.snapshot(document).unwrap().text[view.primary_selection().head..],
        " four\n"
    );

    workspace
        .execute(&mut store, &mut view, EditorCommand::GotoLine)
        .unwrap();
    workspace
        .insert_minibuffer_text(&store, &mut view, "2")
        .unwrap();
    workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert_eq!(
        view.primary_selection(),
        &Selection::caret(byte_of_line(&store.snapshot(document).unwrap().text, 1))
    );
    workspace
        .execute(&mut store, &mut view, EditorCommand::GotoLine)
        .unwrap();
    workspace
        .insert_minibuffer_text(&store, &mut view, "999")
        .unwrap();
    workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    let text = store.snapshot(document).unwrap().text;
    assert_eq!(view.primary_selection(), &Selection::caret(text.len()));

    let effects = workspace
        .execute(&mut store, &mut view, EditorCommand::RevertBuffer)
        .unwrap();
    assert_eq!(effects, [EditorEffect::RevertDocument { document }]);
    assert_eq!(
        workspace.keymap.resolve(&[KeyChord::parse("M-g").unwrap()]),
        KeyResolution::Prefix {
            display: "M-g".to_string(),
            reachable: vec!["g  goto-line".to_string()],
        }
    );
}

#[test]
fn invalid_goto_line_stays_modal_and_c_g_restores_origin() {
    let (mut store, mut workspace, mut view, _) = editor("one\ntwo\n");
    view.selections = vec![Selection::caret(4)];
    workspace
        .execute(&mut store, &mut view, EditorCommand::GotoLine)
        .unwrap();
    workspace
        .insert_minibuffer_text(&store, &mut view, "zero")
        .unwrap();
    let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert!(matches!(view.minibuffer, Minibuffer::GotoLine { .. }));
    assert!(
        matches!(effects.as_slice(), [EditorEffect::Status(message)] if message.contains("positive integer"))
    );
    workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("C-g").unwrap())
        .unwrap();
    assert_eq!(view.primary_selection(), &Selection::caret(4));
    assert!(matches!(view.minibuffer, Minibuffer::Inactive));

    workspace
        .execute(&mut store, &mut view, EditorCommand::GotoLine)
        .unwrap();
    workspace
        .insert_minibuffer_text(&store, &mut view, "+2")
        .unwrap();
    let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert!(matches!(view.minibuffer, Minibuffer::GotoLine { .. }));
    assert!(
        matches!(effects.as_slice(), [EditorEffect::Status(message)] if message.contains("positive integer"))
    );
    workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("C-g").unwrap())
        .unwrap();
}

#[test]
fn external_edits_rebase_search_and_goto_line_cancel_origins() {
    use crate::store::DocumentViewId;

    let source = "xα needle";
    let prefixed = "long prefix\nxα needle";
    for command in [EditorCommand::IncrementalSearch, EditorCommand::GotoLine] {
        for (text, origin, changed, inserted, expected) in [
            (source, "xα".len(), 0..0, "🦀", "🦀xα".len()),
            (source, "xα".len(), 3..3, "🦀", "xα🦀".len()),
            (
                prefixed,
                "long prefix\nxα".len(),
                0.."long prefix\n".len(),
                "",
                "xα".len(),
            ),
            (source, "xα".len(), 0..source.len(), "λ", "λ".len()),
        ] {
            let (mut store, mut workspace, mut view, document) = editor(text);
            let mut writer = workspace
                .attach(&mut store, document, DocumentViewId(92))
                .unwrap();
            view.selections = vec![Selection::caret(origin)];
            workspace.execute(&mut store, &mut view, command).unwrap();
            let query = if command == EditorCommand::IncrementalSearch {
                "needle"
            } else {
                "invalid line"
            };
            workspace
                .insert_minibuffer_text(&store, &mut view, query)
                .unwrap();
            writer.selections = vec![Selection {
                anchor: changed.start,
                head: changed.end,
            }];
            workspace
                .insert_text(&mut store, &mut writer, inserted)
                .unwrap();
            for (seq, deltas) in workspace.take_commits(document) {
                view.observe_external(seq, &deltas);
            }
            let before_cancel = store.snapshot(document).unwrap();
            assert!(before_cancel.text.is_char_boundary(expected));

            workspace
                .handle_chord(&mut store, &mut view, KeyChord::parse("C-g").unwrap())
                .unwrap();
            assert_eq!(
                view.primary_selection(),
                &Selection::caret(expected),
                "{command:?}: {text:?}, insertion {inserted:?}"
            );
            assert!(
                before_cancel
                    .text
                    .is_char_boundary(view.primary_selection().head)
            );
            assert!(matches!(view.minibuffer, Minibuffer::Inactive));
            assert_eq!(view.anchor_seq(), before_cancel.seq);
            let after_cancel = store.snapshot(document).unwrap();
            assert_eq!(after_cancel.seq, before_cancel.seq);
            assert_eq!(after_cancel.text, before_cancel.text);
        }
    }
}

#[test]
fn minibuffer_input_cap_is_utf8_safe_and_does_not_edit_document() {
    let (mut store, mut workspace, mut view, document) = editor("safe");
    workspace
        .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
        .unwrap();
    let oversized = "🦀".repeat(MINIBUFFER_QUERY_LIMIT);
    workspace
        .insert_text(&mut store, &mut view, &oversized)
        .unwrap();
    let Minibuffer::Command { query, .. } = &view.minibuffer else {
        panic!("command minibuffer remains active");
    };
    assert!(query.len() <= MINIBUFFER_QUERY_LIMIT);
    assert!(query.is_char_boundary(query.len()));
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "safe");
}

#[test]
fn tab_completes_selected_name_without_running_and_navigation_wraps() {
    let (mut store, mut workspace, mut view, _) = editor("abc");
    workspace
        .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
        .unwrap();
    workspace
        .insert_minibuffer_text(&store, &mut view, "move-")
        .unwrap();
    workspace
        .command_completion_action(&mut store, &mut view, EditorCompletionAction::Previous)
        .unwrap();
    assert!(matches!(
        view.minibuffer,
        Minibuffer::Command { selected: 1, .. }
    ));
    workspace
        .command_completion_action(&mut store, &mut view, EditorCompletionAction::Complete)
        .unwrap();
    assert!(matches!(
        &view.minibuffer,
        Minibuffer::Command { query, selected: 0 } if query == "move-end-of-line"
    ));
    assert_eq!(view.primary_selection().head, 0, "Tab never executes");
}

#[test]
fn shipping_command_palette_conforms_to_selection_reset_and_exact_submit_model() {
    let model = native_editor_command_palette_model();
    let (mut store, mut workspace, mut view, _) = editor("abc");
    let initial = palette_projection(&model, &view, 0, 0, 0, 0);
    assert_eq!(initial, model.init_state());

    workspace
        .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
        .unwrap();
    let opened = palette_projection(&model, &view, 0, 0, 0, 0);
    assert_palette_transition(&model, &initial, &opened, "Open");

    workspace
        .insert_minibuffer_text(&store, &mut view, "move-")
        .unwrap();
    let broad = palette_projection(&model, &view, 1, 1, 0, 0);
    assert_palette_transition(&model, &opened, &broad, "TypeBroad");
    workspace
        .command_completion_action(&mut store, &mut view, EditorCompletionAction::Next)
        .unwrap();
    let moved = palette_projection(&model, &view, 1, 0, 0, 0);
    assert_palette_transition(&model, &broad, &moved, "MoveNext");

    workspace
        .insert_minibuffer_text(&store, &mut view, "b")
        .unwrap();
    let refined = palette_projection(&model, &view, 2, 1, 0, 0);
    assert_palette_transition(&model, &moved, &refined, "Refine");
    workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    let completed = palette_projection(&model, &view, 0, 0, 1, 1);
    assert_palette_transition(&model, &refined, &completed, "Submit");
    assert_eq!(
        workspace.command_history.back().map(String::as_str),
        Some("move-beginning-of-line"),
        "Enter dispatches the exact selected typed command"
    );

    let mut stale_selection = refined.clone();
    stale_selection.insert("selected", 1);
    assert_eq!(admits(&model, &moved, &stale_selection), None);
    assert!(!model.check_invariant("SelectionWithinResults", &stale_selection));
    assert!(!model.check_invariant("QueryChangeResetsSelection", &stale_selection));

    let mut nearest_unknown_dispatch = completed.clone();
    nearest_unknown_dispatch.insert("exact_selected_dispatch", 0);
    assert_eq!(admits(&model, &refined, &nearest_unknown_dispatch), None);
    assert!(!model.check_invariant("SubmitIsExactSelected", &nearest_unknown_dispatch));
}
