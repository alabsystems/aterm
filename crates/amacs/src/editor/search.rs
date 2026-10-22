// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Incremental search matches forward from its origin as it is typed.
//! Explicit next/previous navigation shares those matches and wraps at an edge.

use std::ops::Range;

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
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let (query, origin) = match &view.minibuffer {
            Minibuffer::Search { query, origin } => (query.clone(), *origin),
            _ => return Ok(vec![EditorEffect::Bell]),
        };
        let snapshot = snapshot_for_view(store, view)?;
        let origin = clamp_to_char_boundary(&snapshot.text, origin.min(snapshot.text.len()));
        let found = if query.is_empty() {
            Some(origin..origin)
        } else {
            search_match(&snapshot.text, &query, origin, true, false)
        };
        Ok(show_match(view, &query, found, origin))
    }

    pub(super) fn navigate_search(
        &self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        forward: bool,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let query = match &mut view.minibuffer {
            Minibuffer::Search { query, .. } => {
                if query.is_empty() {
                    query.clone_from(&view.last_search_query);
                }
                query.clone()
            }
            Minibuffer::Inactive => view.last_search_query.clone(),
            _ => return Ok(vec![EditorEffect::Bell]),
        };
        if query.is_empty() {
            return Ok(vec![EditorEffect::Status(
                "There is no search to step — find something first".to_string(),
            )]);
        }
        let snapshot = snapshot_for_view(store, view)?;
        let selection = view.primary_selection().range();
        let start = if forward {
            selection.end
        } else {
            selection.start
        };
        let start = clamp_to_char_boundary(&snapshot.text, start.min(snapshot.text.len()));
        let found = search_match(&snapshot.text, &query, start, forward, true);
        let origin = match &view.minibuffer {
            Minibuffer::Search { origin, .. } => *origin,
            _ => view.primary_selection().head,
        };
        let origin = clamp_to_char_boundary(&snapshot.text, origin.min(snapshot.text.len()));
        Ok(show_match(view, &query, found, origin))
    }
}

/// Literal, case-sensitive matches. A step excludes the current selection by
/// starting at its end (forward) or start (backward); wrapping can select that
/// same match again when it is the only one.
fn search_match(
    text: &str,
    query: &str,
    start: usize,
    forward: bool,
    wrap: bool,
) -> Option<Range<usize>> {
    let at = if forward {
        text[start..]
            .find(query)
            .map(|offset| start + offset)
            .or_else(|| wrap.then(|| text.find(query)).flatten())
    } else {
        text[..start]
            .rfind(query)
            .or_else(|| wrap.then(|| text.rfind(query)).flatten())
    }?;
    Some(at..at + query.len())
}

fn show_match(
    view: &mut EditorBufferView,
    query: &str,
    found: Option<Range<usize>>,
    origin: usize,
) -> Vec<EditorEffect> {
    view.mark_active = false;
    view.primary = 0;
    if let Some(range) = found {
        view.selections = vec![Selection {
            anchor: range.start,
            head: range.end,
        }];
        if query.is_empty() {
            Vec::new()
        } else {
            vec![EditorEffect::Status(format!("I-search: {query}"))]
        }
    } else {
        view.selections = vec![Selection::caret(origin)];
        vec![EditorEffect::Status(format!("Failing I-search: {query}"))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditorCommand, KeyChord, editor};

    #[test]
    fn explicit_navigation_wraps_in_both_directions_and_keeps_the_accepted_query_per_view() {
        let (mut store, mut workspace, mut view, document) = editor("α needle β needle γ needle");
        let original = store.snapshot(document).unwrap();
        let matches: Vec<_> = original
            .text
            .match_indices("needle")
            .map(|(at, query)| at..at + query.len())
            .collect();

        workspace
            .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
            .unwrap();
        workspace
            .insert_text(&mut store, &mut view, "needle")
            .unwrap();
        assert_eq!(view.primary_selection().range(), matches[0]);
        for (chord, index) in [("C-s", 1), ("C-r", 0)] {
            workspace
                .handle_chord(&mut store, &mut view, KeyChord::parse(chord).unwrap())
                .unwrap();
            assert_eq!(view.primary_selection().range(), matches[index]);
        }
        for (command, index) in [
            (EditorCommand::SearchNext, 1),
            (EditorCommand::SearchPrevious, 0),
            (EditorCommand::SearchPrevious, 2),
            (EditorCommand::SearchNext, 0),
        ] {
            workspace.execute(&mut store, &mut view, command).unwrap();
            assert_eq!(view.primary_selection().range(), matches[index]);
        }

        workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert!(!view.minibuffer_active());
        assert!(view.has_search_query());
        for (command, index) in [
            (EditorCommand::SearchNext, 1),
            (EditorCommand::SearchPrevious, 0),
            (EditorCommand::SearchPrevious, 2),
            (EditorCommand::SearchNext, 0),
        ] {
            workspace.execute(&mut store, &mut view, command).unwrap();
            assert_eq!(view.primary_selection().range(), matches[index]);
            assert!(!view.minibuffer_active());
        }

        let mut other = workspace
            .attach(&mut store, document, crate::store::DocumentViewId(92))
            .unwrap();
        assert!(!other.has_search_query());
        workspace
            .execute(&mut store, &mut other, EditorCommand::SearchNext)
            .unwrap();
        assert_eq!(other.primary_selection(), &Selection::caret(0));
        let after = store.snapshot(document).unwrap();
        assert_eq!(after.seq, original.seq);
        assert_eq!(after.text, original.text);
    }

    #[test]
    fn incremental_typing_stays_at_its_origin_until_explicit_navigation_wraps() {
        let (mut store, mut workspace, mut view, _) = editor("needle α");
        view.selections = vec![Selection::caret("needle α".len())];
        workspace
            .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
            .unwrap();
        workspace
            .insert_text(&mut store, &mut view, "needle")
            .unwrap();
        assert_eq!(
            view.primary_selection(),
            &Selection::caret("needle α".len())
        );
        workspace
            .execute(&mut store, &mut view, EditorCommand::SearchNext)
            .unwrap();
        assert_eq!(view.primary_selection().range(), 0..6);
        workspace
            .execute(&mut store, &mut view, EditorCommand::Abort)
            .unwrap();
        assert_eq!(
            view.primary_selection(),
            &Selection::caret("needle α".len())
        );
    }

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
