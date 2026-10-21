// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Incremental search: the isearch minibuffer's query, matched forward from
//! its origin as it is typed.

use super::minibuffer::Minibuffer;
use super::projection::clamp_to_char_boundary;
use super::view::{EditorBufferView, Selection};
use super::workspace::{EditorEffect, EditorError, EditorWorkspace, snapshot_for_view};
use crate::store::DocumentStore;

impl EditorWorkspace {
    pub(super) fn update_incremental_search(
        &self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        after_current: bool,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let (query, origin) = match &view.minibuffer {
            Minibuffer::Search { query, origin } => (query.clone(), *origin),
            _ => return Ok(vec![EditorEffect::Bell]),
        };
        let snapshot = snapshot_for_view(store, view)?;
        let origin = clamp_to_char_boundary(&snapshot.text, origin.min(snapshot.text.len()));
        let start = if after_current && !query.is_empty() {
            clamp_to_char_boundary(
                &snapshot.text,
                view.primary_selection().head.min(snapshot.text.len()),
            )
        } else {
            origin
        };
        let found = if query.is_empty() {
            Some(origin..origin)
        } else {
            snapshot.text[start..].find(&query).map(|offset| {
                let at = start.saturating_add(offset);
                at..at.saturating_add(query.len())
            })
        };
        view.mark_active = false;
        if let Some(range) = found {
            view.selections = vec![Selection {
                anchor: range.start,
                head: range.end,
            }];
            view.primary = 0;
            return Ok(if query.is_empty() {
                Vec::new()
            } else {
                vec![EditorEffect::Status(format!("I-search: {query}"))]
            });
        }
        view.selections = vec![Selection::caret(origin)];
        view.primary = 0;
        Ok(vec![EditorEffect::Status(format!(
            "Failing I-search: {query}"
        ))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditorCommand, editor};

    #[test]
    fn modal_queries_search_and_execute_without_typing_into_document() {
        let (mut store, mut workspace, mut view, document) = editor("zero needle tail");
        let original = store.snapshot(document).unwrap();

        workspace
            .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
            .unwrap();
        workspace
            .insert_text(&mut store, &mut view, "needle")
            .unwrap();
        let after_search = store.snapshot(document).unwrap();
        assert_eq!(after_search.seq, original.seq);
        assert_eq!(after_search.text, original.text);
        assert_eq!(view.primary_selection().range(), 5..11);
        workspace
            .execute(&mut store, &mut view, EditorCommand::Abort)
            .unwrap();
        assert_eq!(view.primary_selection(), &Selection::caret(0));

        workspace
            .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
            .unwrap();
        let selection_before_query = view.primary_selection().clone();
        assert!(!view.pointer_select(original.text.as_ref(), original.text.len(), false, 12));
        assert_eq!(view.primary_selection(), &selection_before_query);
        workspace
            .insert_text(&mut store, &mut view, "forward-char")
            .unwrap();
        let after_query = store.snapshot(document).unwrap();
        assert_eq!(after_query.seq, original.seq);
        assert_eq!(after_query.text, original.text);
        workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert_eq!(view.primary_selection(), &Selection::caret(1));
        let after_command = store.snapshot(document).unwrap();
        assert_eq!(after_command.seq, original.seq);
        assert_eq!(after_command.text, original.text);
    }
}
