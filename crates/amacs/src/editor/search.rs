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
        &mut self,
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
        let effects = show_match(view, &query, found, origin);
        self.grid_reveal_caret(store, view);
        Ok(effects)
    }

    /// Keeps a selected match only while its current bytes still match the query.
    pub(super) fn accept_search_match(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        query: &str,
        origin: usize,
    ) -> Result<bool, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        if snapshot.text.get(view.primary_selection().range()) == Some(query) {
            self.grid_reveal_caret(store, view);
            return Ok(true);
        }
        let origin = clamp_to_char_boundary(&snapshot.text, origin.min(snapshot.text.len()));
        let found = search_match(&snapshot.text, query, origin, true, false);
        let matched = found.is_some();
        select_match(view, found, origin);
        self.grid_reveal_caret(store, view);
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
        &mut self,
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
        let effects = show_match(view, &query, found, origin);
        self.grid_reveal_caret(store, view);
        Ok(effects)
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
#[path = "search_tests.rs"]
mod tests;
