// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Incremental search and explicit next/previous match navigation.

use std::ops::Range;

use super::command::CommandAuthority;
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

    /// Keeps a selected match only while its current bytes still match the query.
    pub(super) fn accept_search_match(
        &self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        query: &str,
        origin: usize,
    ) -> Result<bool, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        if snapshot.text.get(view.primary_selection().range()) == Some(query) {
            return Ok(true);
        }
        let origin = clamp_to_char_boundary(&snapshot.text, origin.min(snapshot.text.len()));
        let found = search_match(&snapshot.text, query, origin, true, false);
        let matched = found.is_some();
        select_match(view, found, origin);
        Ok(matched)
    }

    /// The query the input being reduced last accepted in `view`, which `C-s C-s` and
    /// search-next recall: the person's ([`EditorBufferView::last_search_query`]) for input that
    /// carries [`CommandAuthority::Clipboard`], else the guest's, as the kill ring is picked
    /// ([`Self::kills`]). A query is what was typed (a token checked for and not found, say), so
    /// input without that authority (in aterm, a control client's) never recalls the person's
    /// into a minibuffer it reads, nor steps the selection to its matches.
    pub(super) fn remembered_search<'v>(&self, view: &'v EditorBufferView) -> &'v str {
        if self.granted.contains(&CommandAuthority::Clipboard) {
            &view.last_search_query
        } else {
            &view.guest_last_search_query
        }
    }

    /// Keep `query` as the one the input being reduced accepted in `view`
    /// ([`Self::remembered_search`]).
    pub(super) fn remember_search(&self, view: &mut EditorBufferView, query: &str) {
        let slot = if self.granted.contains(&CommandAuthority::Clipboard) {
            &mut view.last_search_query
        } else {
            &mut view.guest_last_search_query
        };
        slot.clear();
        slot.push_str(query);
    }

    pub(super) fn navigate_search(
        &self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        forward: bool,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let remembered = self.remembered_search(view).to_owned();
        let query = match &mut view.minibuffer {
            Minibuffer::Search { query, .. } => {
                if query.is_empty() {
                    *query = remembered;
                }
                query.clone()
            }
            Minibuffer::Inactive => remembered,
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
    let matched = found.is_some();
    select_match(view, found, origin);
    if matched {
        if query.is_empty() {
            vec![EditorEffect::Status(String::new())]
        } else {
            vec![EditorEffect::Status(format!("I-search: {query}"))]
        }
    } else {
        vec![EditorEffect::Status(format!("Failing I-search: {query}"))]
    }
}

fn select_match(view: &mut EditorBufferView, found: Option<Range<usize>>, origin: usize) {
    view.mark_active = false;
    view.primary = 0;
    if let Some(range) = found {
        view.selections = vec![Selection {
            anchor: range.start,
            head: range.end,
        }];
    } else {
        view.selections = vec![Selection::caret(origin)];
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

    /// A REMEMBERED SEARCH IS RECALLED ONLY BY THE INPUT THAT ACCEPTED IT: the query the person
    /// accepted is what they typed (a token checked for, found or not), as private as their
    /// kills, so input without clipboard authority (in aterm, a control client's) recalls a query
    /// of its own. Its `C-s C-s` never puts the person's query in the minibuffer it reads, its
    /// search-next never steps the selection to the person's matches, its own accepted query
    /// never replaces theirs, and the person's `C-s C-s` still recalls their own.
    /// NEGATIVE CONTROL: against main's code at this leaf (`147b22abdc`), one query served both:
    /// the guest's search-next stepped the selection to the person's match (`4..11`); so it did
    /// again with `remembered_search` mutated to pick the person's query for every input.
    #[test]
    fn a_remembered_search_is_recalled_only_by_the_input_that_accepted_it() {
        fn press(
            store: &mut DocumentStore,
            workspace: &mut EditorWorkspace,
            view: &mut EditorBufferView,
            keys: &str,
        ) -> Vec<EditorEffect> {
            let mut effects = Vec::new();
            for key in keys.split_whitespace() {
                effects.extend(
                    workspace
                        .handle_chord(store, view, KeyChord::parse(key).unwrap())
                        .unwrap(),
                );
            }
            effects
        }
        let recalled = |view: &EditorBufferView| match &view.minibuffer {
            Minibuffer::Search { query, .. } => Some(query.clone()),
            _ => None,
        };
        let guest = || {
            vec![
                CommandAuthority::Read,
                CommandAuthority::Edit,
                CommandAuthority::Save,
            ]
        };
        let (mut store, mut workspace, mut view, _) = editor("one hunter2 two");

        press(&mut store, &mut workspace, &mut view, "C-s");
        workspace
            .insert_text(&mut store, &mut view, "hunter2")
            .unwrap();
        workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert_eq!(
            view.primary_selection().range(),
            4..11,
            "PRECONDITION: the person's search found it"
        );

        workspace.granted = guest();
        view.selections = vec![Selection::caret(0)];
        workspace
            .execute(&mut store, &mut view, EditorCommand::SearchNext)
            .unwrap();
        assert_eq!(
            view.primary_selection(),
            &Selection::caret(0),
            "the guest's search-next"
        );
        press(&mut store, &mut workspace, &mut view, "C-s C-s");
        assert_eq!(recalled(&view).as_deref(), Some(""), "the guest's C-s C-s");
        workspace.insert_text(&mut store, &mut view, "two").unwrap();
        workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        press(&mut store, &mut workspace, &mut view, "C-s C-s");
        assert_eq!(
            recalled(&view).as_deref(),
            Some("two"),
            "the guest's own query"
        );
        press(&mut store, &mut workspace, &mut view, "C-g");

        workspace.granted = CommandAuthority::ALL.to_vec();
        press(&mut store, &mut workspace, &mut view, "C-s C-s");
        assert_eq!(
            recalled(&view).as_deref(),
            Some("hunter2"),
            "the person's C-s C-s"
        );
    }

    /// On Windows `C-s` saves and isearch is `M-s`: in a search, `M-s`
    /// searches again, as `C-s` does wherever it starts the search
    /// (`C-s` and `C-r` keep doing so everywhere).
    #[test]
    fn the_chord_that_starts_a_search_searches_again() {
        use crate::editor::{Keymap, Platform, Profile};
        for (platform, again) in [(Platform::Windows, "M-s"), (Platform::Mac, "C-s")] {
            let (mut store, _, _, document) = editor("needle α needle β needle");
            let mut workspace = EditorWorkspace::with_keymap(Keymap::emacs(Profile::Gui(platform)));
            let mut view = workspace
                .attach(&mut store, document, crate::store::DocumentViewId(93))
                .unwrap();
            let chord = |s: &str| KeyChord::parse(s).unwrap();
            workspace
                .handle_chord(&mut store, &mut view, chord(again))
                .unwrap();
            assert!(view.minibuffer_active(), "{platform:?}: {again} starts it");
            workspace
                .insert_text(&mut store, &mut view, "needle")
                .unwrap();
            assert_eq!(view.primary_selection().range(), 0..6);
            for (press, at) in [(again, 10), ("C-s", 20), ("C-r", 10)] {
                workspace
                    .handle_chord(&mut store, &mut view, chord(press))
                    .unwrap();
                assert_eq!(
                    view.primary_selection().range(),
                    at..at + 6,
                    "{platform:?}: {press}"
                );
            }
        }
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
    fn clearing_successful_and_failed_search_queries_clears_status_and_restores_origin() {
        for query in ["needle", "missing"] {
            let (mut store, mut workspace, mut view, document) = editor("α🦀 needle tail");
            let original = store.snapshot(document).unwrap();
            let origin = "α".len();
            view.selections = vec![Selection::caret(origin)];
            workspace
                .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
                .unwrap();
            let effects = workspace.insert_text(&mut store, &mut view, query).unwrap();
            assert!(matches!(
                effects.as_slice(),
                [EditorEffect::Status(message)] if !message.is_empty()
            ));
            if query == "needle" {
                assert_eq!(
                    original.text.get(view.primary_selection().range()),
                    Some(query)
                );
            } else {
                assert_eq!(view.primary_selection(), &Selection::caret(origin));
            }

            for _ in query.chars().skip(1) {
                workspace.minibuffer_backspace(&store, &mut view).unwrap();
            }
            let effects = workspace.minibuffer_backspace(&store, &mut view).unwrap();
            assert_eq!(effects, [EditorEffect::Status(String::new())]);
            assert!(matches!(
                &view.minibuffer,
                Minibuffer::Search { query, origin: at } if query.is_empty() && *at == origin
            ));
            assert_eq!(view.primary_selection(), &Selection::caret(origin));
            assert!(
                original
                    .text
                    .is_char_boundary(view.primary_selection().head)
            );
            let after = store.snapshot(document).unwrap();
            assert_eq!(after.seq, original.seq);
            assert_eq!(after.text, original.text);
        }
    }

    #[test]
    fn accepting_search_rechecks_external_edits_and_preserves_a_valid_later_match() {
        for (name, text, changed, inserted, next, expected_match) in [
            (
                "insertion within the selected match",
                "α needle tail needle",
                6..6,
                "🦀",
                false,
                Some(0),
            ),
            (
                "removal of the only match",
                "α needle tail",
                3..9,
                "",
                false,
                None,
            ),
            (
                "a failed search gains a match",
                "α hay tail",
                3..6,
                "needle",
                false,
                Some(0),
            ),
            (
                "the second match remains valid",
                "α needle β needle γ",
                0..0,
                "🙂 ",
                true,
                Some(1),
            ),
        ] {
            let (mut store, mut workspace, mut view, document) = editor(text);
            let mut writer = workspace
                .attach(&mut store, document, crate::store::DocumentViewId(92))
                .unwrap();
            workspace
                .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
                .unwrap();
            workspace
                .insert_text(&mut store, &mut view, "needle")
                .unwrap();
            if next {
                workspace
                    .execute(&mut store, &mut view, EditorCommand::SearchNext)
                    .unwrap();
            }

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
            let before_submit = store.snapshot(document).unwrap();
            let expected = expected_match.map(|index| {
                let (start, query) = before_submit
                    .text
                    .match_indices("needle")
                    .nth(index)
                    .unwrap();
                start..start + query.len()
            });

            let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
            assert!(!view.minibuffer_active(), "{name}");
            assert_eq!(view.last_search_query, "needle", "{name}");
            if let Some(expected) = expected {
                assert_eq!(view.primary_selection().range(), expected, "{name}");
                assert_eq!(effects, [EditorEffect::Status(String::new())], "{name}");
            } else {
                assert_eq!(view.primary_selection(), &Selection::caret(0), "{name}");
                assert_eq!(
                    effects,
                    [EditorEffect::Status("Not found".into())],
                    "{name}"
                );
            }
            let after_submit = store.snapshot(document).unwrap();
            assert_eq!(after_submit.seq, before_submit.seq, "{name}");
            assert_eq!(after_submit.text, before_submit.text, "{name}");
        }
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
