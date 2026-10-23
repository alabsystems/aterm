// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance for the native editor's mark and minibuffer lifecycle against
//! [`native_editor_modal_model`].

#![cfg(test)]

use aterm_buffer::Seq;
use aterm_models_native::native_editor_modal_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

use crate::conformance_support::assert_transition;
use crate::document_store::{DocumentId, DocumentStore, DocumentViewId};
use crate::native_editor::{
    EditorBufferView, EditorCommand, EditorEffect, EditorWorkspace, Minibuffer,
};

#[derive(Clone, Copy)]
struct ProjectionFacts {
    baseline: Seq,
    authorized_edits: i64,
    mark_origin: usize,
    search_origin: usize,
    last_exit: i64,
    prompt_base: i64,
}

fn project(
    model: &Model,
    store: &DocumentStore,
    document: DocumentId,
    view: &EditorBufferView,
    facts: ProjectionFacts,
) -> State {
    let snapshot = store.snapshot(document).expect("live editor document");
    let selection = view.primary_selection();
    let (mode, query, active_search_origin) = match &view.minibuffer {
        Minibuffer::Inactive => (0, 0, None),
        Minibuffer::Command { query, .. } => (1, query.chars().count(), None),
        Minibuffer::Search { query, origin } => (2, query.chars().count(), Some(*origin)),
        Minibuffer::Buffer { query } => (3, query.chars().count(), None),
        Minibuffer::GotoLine { query, origin } => (
            4,
            query
                .parse::<usize>()
                .ok()
                .and_then(|line| line.checked_sub(1))
                .unwrap_or(0),
            Some(*origin),
        ),
        Minibuffer::Prompt { query, .. } => (5, query.chars().count(), None),
    };
    let mut state = model.init_state();
    state.insert("mode", mode);
    state.insert("query", i64::try_from(query).expect("bounded test query"));
    state.insert(
        "caret",
        i64::try_from(selection.head).expect("bounded test caret"),
    );
    state.insert(
        "anchor",
        i64::try_from(selection.anchor).expect("bounded test anchor"),
    );
    state.insert(
        "mark_origin",
        i64::try_from(facts.mark_origin).expect("bounded test mark"),
    );
    state.insert("mark_active", i64::from(view.mark_active));
    state.insert(
        "search_origin",
        i64::try_from(active_search_origin.unwrap_or(facts.search_origin))
            .expect("bounded test search origin"),
    );
    state.insert(
        "document_edits",
        i64::try_from(snapshot.seq.0.saturating_sub(facts.baseline.0))
            .expect("bounded test sequence"),
    );
    state.insert("authorized_edits", facts.authorized_edits);
    state.insert("last_exit", facts.last_exit);
    state.insert("prompt_base", facts.prompt_base);
    state
}

#[test]
fn shipping_editor_mark_and_minibuffer_trace_conforms_with_negative_controls() {
    let model = native_editor_modal_model();
    let mut store = crate::test_document_store();
    let document = store.open("mem://editor-modal-conformance".into(), "abc".into());
    let cap = model
        .consts
        .iter()
        .find_map(|(name, value)| (*name == "Cap").then_some(*value))
        .expect("NativeEditorModal declares Cap");
    assert_eq!(usize::try_from(cap).unwrap(), "abc".len());
    let mut workspace = EditorWorkspace::new();
    let mut view = workspace
        .attach(&mut store, document, DocumentViewId(701))
        .expect("attach genuine editor view");
    let mut facts = ProjectionFacts {
        baseline: store.snapshot(document).unwrap().seq,
        authorized_edits: 0,
        mark_origin: 0,
        search_origin: 0,
        last_exit: 0,
        prompt_base: 0,
    };
    assert_eq!(
        project(&model, &store, document, &view, facts),
        model.init_state()
    );

    let before_mark = project(&model, &store, document, &view, facts);
    workspace
        .execute(&mut store, &mut view, EditorCommand::SetMark)
        .unwrap();
    facts.mark_origin = view.primary_selection().anchor;
    let after_mark = project(&model, &store, document, &view, facts);
    assert_transition(&model, &before_mark, &after_mark, "SetMark");

    let before_move = after_mark;
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveForward)
        .unwrap();
    let after_move = project(&model, &store, document, &view, facts);
    assert_transition(&model, &before_move, &after_move, "Move");

    let mut collapsed_mark = after_move.clone();
    collapsed_mark.insert("anchor", after_move["caret"]);
    assert_eq!(admits(&model, &before_move, &collapsed_mark), None);
    assert!(!model.check_invariant("MarkPinned", &collapsed_mark));

    let before_kill = after_move;
    workspace
        .execute(&mut store, &mut view, EditorCommand::KillRegion)
        .unwrap();
    facts.authorized_edits += 1;
    let after_kill = project(&model, &store, document, &view, facts);
    assert_transition(&model, &before_kill, &after_kill, "KillRegion");
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "bc");

    let before_command = after_kill;
    workspace
        .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
        .unwrap();
    facts.last_exit = 0;
    let command_open = project(&model, &store, document, &view, facts);
    assert_transition(&model, &before_command, &command_open, "OpenCommand");
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    let command_typed = project(&model, &store, document, &view, facts);
    assert_transition(&model, &command_open, &command_typed, "MinibufferType");

    let mut leaked_input = command_typed.clone();
    leaked_input.insert("document_edits", command_typed["document_edits"] + 1);
    assert_eq!(admits(&model, &command_open, &leaked_input), None);
    assert!(!model.check_invariant("MinibufferCannotEditDocument", &leaked_input));

    workspace
        .execute(&mut store, &mut view, EditorCommand::Abort)
        .unwrap();
    facts.last_exit = 1;
    let command_aborted = project(&model, &store, document, &view, facts);
    assert_transition(&model, &command_typed, &command_aborted, "AbortCommand");

    let before_search = command_aborted;
    workspace
        .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
        .unwrap();
    facts.last_exit = 0;
    facts.search_origin = view.primary_selection().head;
    let search_open = project(&model, &store, document, &view, facts);
    assert_transition(&model, &before_search, &search_open, "OpenSearch");
    workspace.insert_text(&mut store, &mut view, "b").unwrap();
    let search_typed = project(&model, &store, document, &view, facts);
    assert_transition(&model, &search_open, &search_typed, "MinibufferType");

    let mut overrun = search_typed.clone();
    overrun.insert("caret", cap + 1);
    assert_eq!(admits(&model, &search_open, &overrun), None);
    assert!(!model.check_invariant("CaretBounded", &overrun));
    workspace.minibuffer_backspace(&store, &mut view).unwrap();
    let search_erased = project(&model, &store, document, &view, facts);
    assert_transition(&model, &search_typed, &search_erased, "MinibufferBackspace");
    workspace.insert_text(&mut store, &mut view, "b").unwrap();
    let search_retyped = project(&model, &store, document, &view, facts);
    assert_transition(&model, &search_erased, &search_retyped, "MinibufferType");
    workspace
        .execute(&mut store, &mut view, EditorCommand::Abort)
        .unwrap();
    facts.last_exit = 1;
    let search_aborted = project(&model, &store, document, &view, facts);
    assert_transition(&model, &search_retyped, &search_aborted, "AbortSearch");
    assert_eq!(view.primary_selection().head, facts.search_origin);

    let before_buffer = search_aborted;
    workspace
        .execute(&mut store, &mut view, EditorCommand::SwitchBuffer)
        .unwrap();
    facts.last_exit = 0;
    let buffer_open = project(&model, &store, document, &view, facts);
    assert_transition(&model, &before_buffer, &buffer_open, "OpenBuffer");
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    let buffer_typed = project(&model, &store, document, &view, facts);
    assert_transition(&model, &buffer_open, &buffer_typed, "MinibufferType");
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "bc");
    assert_eq!(
        workspace.submit_minibuffer(&mut store, &mut view).unwrap(),
        vec![EditorEffect::SwitchBuffer {
            query: "x".to_string()
        }]
    );
    facts.last_exit = 0;
    let buffer_submitted = project(&model, &store, document, &view, facts);
    assert_transition(&model, &buffer_typed, &buffer_submitted, "Submit");

    let before_prompt = buffer_submitted;
    workspace
        .execute(&mut store, &mut view, EditorCommand::KillBuffer)
        .unwrap();
    facts.prompt_base = before_prompt["document_edits"];
    let prompt_open = project(&model, &store, document, &view, facts);
    assert_transition(&model, &before_prompt, &prompt_open, "OpenPrompt");
    assert_eq!(
        view.minibuffer.prompt_label(),
        Some("Kill buffer (default this buffer): ")
    );
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    let prompt_typed = project(&model, &store, document, &view, facts);
    assert_transition(&model, &prompt_open, &prompt_typed, "MinibufferType");
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "bc");

    let mut prompt_leak = prompt_typed.clone();
    prompt_leak.insert("document_edits", prompt_typed["document_edits"] + 1);
    prompt_leak.insert("authorized_edits", prompt_typed["authorized_edits"] + 1);
    assert_eq!(admits(&model, &prompt_open, &prompt_leak), None);
    assert!(model.check_invariant("MinibufferCannotEditDocument", &prompt_leak));
    assert!(!model.check_invariant("PromptNeverEditsDocument", &prompt_leak));

    workspace.minibuffer_backspace(&store, &mut view).unwrap();
    let prompt_erased = project(&model, &store, document, &view, facts);
    assert_transition(&model, &prompt_typed, &prompt_erased, "MinibufferBackspace");
    workspace
        .execute(&mut store, &mut view, EditorCommand::Abort)
        .unwrap();
    facts.last_exit = 1;
    let prompt_aborted = project(&model, &store, document, &view, facts);
    assert_transition(&model, &prompt_erased, &prompt_aborted, "AbortPrompt");

    workspace
        .execute(&mut store, &mut view, EditorCommand::KillBuffer)
        .unwrap();
    facts.last_exit = 0;
    facts.prompt_base = prompt_aborted["document_edits"];
    let prompt_reopened = project(&model, &store, document, &view, facts);
    assert_transition(&model, &prompt_aborted, &prompt_reopened, "OpenPrompt");
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    let prompt_answered = project(&model, &store, document, &view, facts);
    assert_transition(&model, &prompt_reopened, &prompt_answered, "MinibufferType");
    assert_eq!(
        workspace.submit_minibuffer(&mut store, &mut view).unwrap(),
        vec![EditorEffect::KillBuffer {
            query: "x".to_string()
        }]
    );
    let prompt_submitted = project(&model, &store, document, &view, facts);
    assert_transition(&model, &prompt_answered, &prompt_submitted, "SubmitPrompt");
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "bc");

    let mut goto_store = crate::test_document_store();
    let goto_document = goto_store.open("mem://editor-goto-conformance".into(), "\nx".into());
    let mut goto_workspace = EditorWorkspace::new();
    let mut goto_view = goto_workspace
        .attach(&mut goto_store, goto_document, DocumentViewId(702))
        .unwrap();
    let goto_facts = ProjectionFacts {
        baseline: goto_store.snapshot(goto_document).unwrap().seq,
        authorized_edits: 0,
        mark_origin: 0,
        search_origin: 0,
        last_exit: 0,
        prompt_base: 0,
    };
    let goto_initial = project(&model, &goto_store, goto_document, &goto_view, goto_facts);
    assert_eq!(goto_initial, model.init_state());
    goto_workspace
        .execute(&mut goto_store, &mut goto_view, EditorCommand::GotoLine)
        .unwrap();
    let goto_open = project(&model, &goto_store, goto_document, &goto_view, goto_facts);
    assert_transition(&model, &goto_initial, &goto_open, "OpenGoto");
    goto_workspace
        .insert_text(&mut goto_store, &mut goto_view, "2")
        .unwrap();
    let goto_typed = project(&model, &goto_store, goto_document, &goto_view, goto_facts);
    assert_transition(&model, &goto_open, &goto_typed, "MinibufferType");
    let mut goto_leak = goto_typed.clone();
    goto_leak.insert("document_edits", 1);
    assert_eq!(admits(&model, &goto_open, &goto_leak), None);
    assert!(!model.check_invariant("MinibufferCannotEditDocument", &goto_leak));
    goto_workspace
        .submit_minibuffer(&mut goto_store, &mut goto_view)
        .unwrap();
    let goto_submitted = project(&model, &goto_store, goto_document, &goto_view, goto_facts);
    assert_transition(&model, &goto_typed, &goto_submitted, "SubmitGoto");
    assert_eq!(goto_view.primary_selection().head, 1);
}

#[test]
fn prompt_edits_doc_mutant_breaks_only_the_prompt_law() {
    use aterm_spec::interp::{violated_invariants, with_buggy};
    let model = native_editor_modal_model();
    assert!(violated_invariants(&with_buggy(&model, 0)).is_empty());
    let broken = violated_invariants(&with_buggy(&model, 2));
    assert!(broken.contains("PromptNeverEditsDocument"), "{broken:?}");
    assert!(
        !broken.contains("MinibufferCannotEditDocument"),
        "{broken:?}"
    );
}
