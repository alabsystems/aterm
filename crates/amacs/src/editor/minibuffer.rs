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
        let trimmed = query.trim();
        let Ok(line) = trimmed
            .strip_prefix('+')
            .map_or(trimmed, |_| "")
            .parse::<usize>()
        else {
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
        self.grid_reveal_caret(store, view);
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
#[path = "minibuffer_tests.rs"]
mod tests;
