// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Modal minibuffer queries (`M-x`, `isearch`, `switch-to-buffer`, `goto-line`, and prompts).

use super::command::{EditorCommand, EditorCompletionAction, Invocation, command_completions};
use super::projection::byte_of_line;
use super::view::{EditorBufferView, Selection, previous_boundary};
use super::workspace::{EditorEffect, EditorError, EditorWorkspace, snapshot_for_view};
use crate::store::DocumentStore;

const MINIBUFFER_QUERY_LIMIT: usize = 4 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Minibuffer {
    #[default]
    Inactive,
    Command {
        query: String,
        selected: usize,
    },
    Search {
        query: String,
        origin: usize,
    },
    Buffer {
        query: String,
    },
    GotoLine {
        query: String,
        origin: usize,
    },
    /// Active prompt for a command's typed arguments ([`super::EditorCommand::prompts`]).
    Prompt {
        invocation: Invocation,
        query: String,
    },
}

impl Minibuffer {
    /// Returns the prompt label for a command prompt, or `None` for other minibuffer modes.
    pub fn prompt_label(&self) -> Option<&'static str> {
        match self {
            Self::Prompt { invocation, .. } => {
                invocation.next_prompt().map(|prompt| prompt.label())
            }
            _ => None,
        }
    }
}

impl EditorWorkspace {
    /// Routes committed text into the active minibuffer, bounded at UTF-8 char boundaries.
    pub fn insert_minibuffer_text(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        text: &str,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let (complete, search) = match &mut view.minibuffer {
            Minibuffer::Command { query, selected } => {
                let complete = push_bounded(query, text, MINIBUFFER_QUERY_LIMIT);
                *selected = 0;
                (complete, false)
            }
            Minibuffer::Search { query, .. } => {
                (push_bounded(query, text, MINIBUFFER_QUERY_LIMIT), true)
            }
            Minibuffer::Buffer { query }
            | Minibuffer::GotoLine { query, .. }
            | Minibuffer::Prompt { query, .. } => {
                (push_bounded(query, text, MINIBUFFER_QUERY_LIMIT), false)
            }
            Minibuffer::Inactive => {
                return Ok(vec![EditorEffect::Bell]);
            }
        };
        if search {
            let mut effects = self.update_incremental_search(store, view)?;
            if !complete {
                effects.push(EditorEffect::Status(format!(
                    "Minibuffer input is limited to {MINIBUFFER_QUERY_LIMIT} bytes"
                )));
            }
            return Ok(effects);
        }
        if complete {
            Ok(Vec::new())
        } else {
            Ok(vec![EditorEffect::Status(format!(
                "Minibuffer input is limited to {MINIBUFFER_QUERY_LIMIT} bytes"
            ))])
        }
    }

    /// Deletes the previous complete grapheme from the active minibuffer query.
    pub fn minibuffer_backspace(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let search = matches!(view.minibuffer, Minibuffer::Search { .. });
        let query = match &mut view.minibuffer {
            Minibuffer::Command { query, selected } => {
                *selected = 0;
                query
            }
            Minibuffer::Search { query, .. }
            | Minibuffer::Buffer { query }
            | Minibuffer::GotoLine { query, .. }
            | Minibuffer::Prompt { query, .. } => query,
            Minibuffer::Inactive => {
                return Ok(vec![EditorEffect::Bell]);
            }
        };
        let boundary = previous_boundary(query, query.len());
        query.truncate(boundary);
        if search {
            self.update_incremental_search(store, view)
        } else {
            Ok(Vec::new())
        }
    }

    /// Accepts and executes the active minibuffer query.
    pub fn submit_minibuffer(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        match std::mem::take(&mut view.minibuffer) {
            Minibuffer::Command { query, selected } => {
                let candidates = command_completions(&query);
                let Some(command) = candidates.get(selected).copied() else {
                    view.minibuffer = Minibuffer::Command {
                        query: query.clone(),
                        selected: 0,
                    };
                    return Ok(vec![EditorEffect::Status(format!(
                        "No editor command matches `{query}`"
                    ))]);
                };
                view.clear_minibuffer();
                self.execute(store, view, command)
            }
            Minibuffer::Search { query, origin } => {
                view.clear_minibuffer();
                let status = if query.is_empty() {
                    "Search cancelled"
                } else {
                    let matched = self.accept_search_match(store, view, &query, origin)?;
                    self.remember_search(view, &query);
                    if matched { "" } else { "Not found" }
                };
                Ok(vec![EditorEffect::Status(status.to_string())])
            }
            Minibuffer::Buffer { query } => {
                view.clear_minibuffer();
                self.record_answers(EditorCommand::SwitchBuffer, std::slice::from_ref(&query));
                Ok(vec![EditorEffect::SwitchBuffer { query }])
            }
            Minibuffer::GotoLine { query, origin } => match self.goto_line(store, view, &query) {
                Ok(Ok(effects)) => {
                    view.clear_minibuffer();
                    self.record_answers(EditorCommand::GotoLine, std::slice::from_ref(&query));
                    Ok(effects)
                }
                Ok(Err(refusal)) => {
                    view.minibuffer = Minibuffer::GotoLine { query, origin };
                    Ok(vec![EditorEffect::Status(refusal)])
                }
                Err(error) => {
                    view.clear_minibuffer();
                    Err(error)
                }
            },
            Minibuffer::Prompt {
                mut invocation,
                query,
            } => {
                invocation.args.push(query);
                if invocation.next_prompt().is_some() {
                    view.open_minibuffer(Minibuffer::Prompt {
                        invocation,
                        query: String::new(),
                    });
                    return Ok(Vec::new());
                }
                view.clear_minibuffer();
                self.answer_prompt(store, view, invocation)
            }
            Minibuffer::Inactive => Ok(vec![EditorEffect::Bell]),
        }
    }

    /// Moves the caret to the 1-based line named by `query`, clamping past the last line.
    pub(super) fn goto_line(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        query: &str,
    ) -> Result<Result<Vec<EditorEffect>, String>, EditorError> {
        let Ok(line) = query.trim().parse::<usize>() else {
            return Ok(Err("Line number must be a positive integer".to_string()));
        };
        if line == 0 {
            return Ok(Err("Line numbers start at 1".to_string()));
        }
        let snapshot = snapshot_for_view(store, view)?;
        let target = byte_of_line(&snapshot.text, line.saturating_sub(1));
        view.selections = vec![Selection::caret(target)];
        view.primary = 0;
        view.viewport_anchor = target;
        view.mark_active = false;
        let landed = snapshot.text[..target]
            .matches('\n')
            .count()
            .saturating_add(1);
        Ok(Ok(vec![EditorEffect::Status(if landed < line {
            format!("Line {landed} (last line)")
        } else {
            format!("Line {line}")
        })]))
    }

    /// Navigates, completes, or activates the `M-x` command completion list.
    pub fn command_completion_action(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        action: EditorCompletionAction,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let (query, selected) = match &view.minibuffer {
            Minibuffer::Command { query, selected } => (query.clone(), *selected),
            _ => return Ok(vec![EditorEffect::Bell]),
        };
        let candidates = command_completions(&query);
        let count = candidates.len();
        if count == 0 {
            return Ok(vec![EditorEffect::Status(format!(
                "No editor command matches `{query}`"
            ))]);
        }
        let last = count - 1;
        match action {
            EditorCompletionAction::Previous | EditorCompletionAction::Next => {
                let next = if action == EditorCompletionAction::Previous {
                    selected.checked_sub(1).unwrap_or(last)
                } else {
                    selected.saturating_add(1) % count
                };
                if let Minibuffer::Command { selected, .. } = &mut view.minibuffer {
                    *selected = next;
                }
                Ok(Vec::new())
            }
            EditorCompletionAction::Complete => {
                if let (
                    Some(candidate),
                    Minibuffer::Command {
                        query,
                        selected: current,
                    },
                ) = (candidates.get(selected.min(last)), &mut view.minibuffer)
                {
                    *query = candidate.name().to_string();
                    *current = 0;
                }
                Ok(Vec::new())
            }
            EditorCompletionAction::Choose(index) => {
                if index >= count {
                    return Ok(vec![EditorEffect::Bell]);
                }
                if let Minibuffer::Command { selected, .. } = &mut view.minibuffer {
                    *selected = index;
                }
                self.submit_minibuffer(store, view)
            }
        }
    }

    /// Blocks non-text document editing gestures while a minibuffer owns input.
    pub fn minibuffer_blocks_document_input(
        &self,
        view: &EditorBufferView,
    ) -> Option<Vec<EditorEffect>> {
        view.minibuffer_active().then(|| {
            vec![EditorEffect::Status(
                "Minibuffer active — type, Backspace, Enter, or C-g".to_string(),
            )]
        })
    }
}

fn push_bounded(target: &mut String, input: &str, limit: usize) -> bool {
    let remaining = limit.saturating_sub(target.len());
    if input.len() <= remaining {
        target.push_str(input);
        return true;
    }
    let mut end = remaining.min(input.len());
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&input[..end]);
    false
}

#[cfg(test)]
mod tests {
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

    fn assert_palette_transition(
        model: &Model,
        before: &State,
        after: &State,
        action: &'static str,
    ) {
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
}
