// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One editor view's own state (its selections, viewport, minibuffer and pending keys) and where its carets move: grapheme, word and line motion over the document's text.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

use aterm_buffer::Seq;
use aterm_grapheme::GraphemeClusters;

use super::chord::KeyChord;
use super::command::{EditorCommand, PrefixArgument};
use super::grid::Face;
use super::minibuffer::Minibuffer;
use super::projection::{
    EditorLines, byte_at_editor_display_column, clamp_to_char_boundary, line_end, line_start,
    line_text_end,
};
use crate::store::{DocumentId, DocumentViewId, EditDelta, rebase_position};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub const fn caret(position: usize) -> Self {
        Self {
            anchor: position,
            head: position,
        }
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    #[cfg(any(test, debug_assertions))]
    pub const fn is_caret(&self) -> bool {
        self.anchor == self.head
    }

    pub(super) const fn collapse(&mut self, position: usize) {
        self.anchor = position;
        self.head = position;
    }

    pub(super) const fn move_head(&mut self, position: usize, preserve_anchor: bool) {
        if preserve_anchor {
            self.head = position;
        } else {
            self.collapse(position);
        }
    }

    fn rebase(&mut self, deltas: &[EditDelta]) {
        self.anchor = rebase_position(self.anchor, deltas);
        self.head = rebase_position(self.head, deltas);
    }
}

/// State that deliberately belongs to one visible editor view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorBufferView {
    pub document: DocumentId,
    pub document_view: DocumentViewId,
    pub selections: Vec<Selection>,
    pub primary: usize,
    pub viewport_anchor: usize,
    /// Renderer-derived body capacity installed by the host for this exact
    /// view/window.
    viewport_lines: usize,
    pub folds: Vec<Range<usize>>,
    pub minibuffer: Minibuffer,
    input_lifetime: Arc<()>,
    /// The last accepted query belongs to this view, never to a terminal or another editor view:
    /// the person's, accepted by input that carries [`super::CommandAuthority::Clipboard`].
    pub(super) last_search_query: String,
    /// [`Self::last_search_query`] for input without clipboard authority (in aterm, a control
    /// client's): a query is what was typed, as private as a kill, so neither input recalls the
    /// other's (`C-s C-s`, search-next: [`super::EditorWorkspace::remembered_search`]).
    pub(super) guest_last_search_query: String,
    /// `set-mark-command` activates a persistent region anchor.
    pub mark_active: bool,
    pub prefix_hud: Option<String>,
    pub(super) desired_column: Option<usize>,
    pub(super) chord_prefix: Vec<KeyChord>,
    pub(super) prefix_argument: Option<PrefixArgument>,
    pub(super) anchor_seq: Seq,
    /// The command this view ran last (Emacs's `last-command`): a kill after a kill joins it, and `C-l` after `C-l` moves on to the next place.
    pub(super) last_command: Option<EditorCommand>,
    /// Where `C-l` last put the caret's line: the middle, the top, then the bottom.
    pub(super) recenter_phase: u8,
    /// How the view draws its document: text, or a table's grid.
    pub face: Face,
}

impl EditorBufferView {
    pub fn new(document: DocumentId, document_view: DocumentViewId, anchor_seq: Seq) -> Self {
        Self {
            document,
            document_view,
            selections: vec![Selection::caret(0)],
            primary: 0,
            viewport_anchor: 0,
            viewport_lines: 4,
            folds: Vec::new(),
            minibuffer: Minibuffer::Inactive,
            input_lifetime: Arc::new(()),
            last_search_query: String::new(),
            guest_last_search_query: String::new(),
            mark_active: false,
            prefix_hud: None,
            desired_column: None,
            chord_prefix: Vec::new(),
            prefix_argument: None,
            anchor_seq,
            last_command: None,
            recenter_phase: 0,
            face: Face::Text,
        }
    }

    /// The primary selection: the one `primary` names, the last when `primary` is past the end, and a caret at the start when a host left the view with none (`selections` is public), rather than a panic.
    pub fn primary_selection(&self) -> &Selection {
        const NO_SELECTION: Selection = Selection::caret(0);
        self.selections
            .get(self.primary.min(self.selections.len().saturating_sub(1)))
            .unwrap_or(&NO_SELECTION)
    }

    /// Identity of the current document or minibuffer input lifetime. Hosts
    /// compare captured tokens with [`Arc::ptr_eq`] before delivering delayed
    /// input; ordinary query edits and document rebases keep this identity.
    pub fn input_lifetime(&self) -> Arc<()> {
        Arc::clone(&self.input_lifetime)
    }

    /// Close the minibuffer and retire delayed input without moving selections.
    pub fn clear_minibuffer(&mut self) {
        self.minibuffer = Minibuffer::Inactive;
        self.input_lifetime = Arc::new(());
    }

    /// Find may start or repeat a search without interrupting another prompt.
    pub const fn can_start_search(&self) -> bool {
        matches!(
            self.minibuffer,
            Minibuffer::Inactive | Minibuffer::Search { .. }
        )
    }

    /// Whether Find Next/Previous has a query to step here: the open search's, or one some input
    /// accepted. A step by input whose own remembered query is empty says there is none to step
    /// ([`super::EditorWorkspace::remembered_search`]).
    pub const fn has_search_query(&self) -> bool {
        let remembered =
            !self.last_search_query.is_empty() || !self.guest_last_search_query.is_empty();
        match &self.minibuffer {
            Minibuffer::Search { query, .. } => !query.is_empty() || remembered,
            Minibuffer::Inactive => remembered,
            _ => false,
        }
    }

    /// The document sequence this view's coordinates are expressed at.
    #[cfg(any(test, debug_assertions))]
    pub const fn anchor_seq(&self) -> Seq {
        self.anchor_seq
    }

    pub fn scroll_lines(&mut self, text: &str, delta: i32) {
        self.scroll_lines_with(&EditorLines::scanning(text), delta);
    }

    pub fn scroll_lines_with(&mut self, lines: &EditorLines<'_>, delta: i32) {
        let total_lines = lines.total();
        let last_full_viewport_anchor = total_lines.saturating_sub(self.viewport_lines());
        let current = lines
            .number_at(self.viewport_anchor)
            .min(last_full_viewport_anchor);
        let target = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            current.saturating_add(delta as usize)
        }
        .min(last_full_viewport_anchor);
        self.viewport_anchor = lines.byte_of(target);
    }

    pub fn viewport_lines(&self) -> usize {
        self.viewport_lines.max(1)
    }

    pub fn reconcile_viewport(&mut self, text: &str, visible_lines: usize) -> bool {
        self.reconcile_viewport_with(&EditorLines::scanning(text), visible_lines)
    }

    /// Install the renderer's capacity. A changed capacity (a resize) reveals the caret, but not
    /// while a prompt holds the keyboard: the rows its footer takes or gives back as it wraps
    /// leave the document where the reader scrolled it, the caret behind the prompt unrevealed.
    pub fn reconcile_viewport_with(
        &mut self,
        lines: &EditorLines<'_>,
        visible_lines: usize,
    ) -> bool {
        let before_anchor = self.viewport_anchor;
        let before_lines = self.viewport_lines;
        self.viewport_lines = visible_line_capacity(visible_lines);
        if before_lines == self.viewport_lines || self.minibuffer_active() {
            let last_full_viewport_anchor = lines.total().saturating_sub(self.viewport_lines);
            let anchor = lines
                .number_at(self.viewport_anchor)
                .min(last_full_viewport_anchor);
            self.viewport_anchor = lines.byte_of(anchor);
        } else {
            self.ensure_primary_visible_with(lines, self.viewport_lines);
        }
        before_anchor != self.viewport_anchor || before_lines != self.viewport_lines
    }

    pub fn ensure_primary_visible(&mut self, text: &str, visible_lines: usize) {
        self.ensure_primary_visible_with(&EditorLines::scanning(text), visible_lines);
    }

    pub fn ensure_primary_visible_with(&mut self, lines: &EditorLines<'_>, visible_lines: usize) {
        let visible_lines = visible_line_capacity(visible_lines);
        self.viewport_lines = visible_lines;
        let caret_line = lines.number_at(self.primary_selection().head);
        let total_lines = lines.total();
        let last_full_viewport_anchor = total_lines.saturating_sub(visible_lines);
        let mut anchor_line = lines
            .number_at(self.viewport_anchor)
            .min(last_full_viewport_anchor);
        if caret_line < anchor_line {
            anchor_line = caret_line;
        } else if caret_line >= anchor_line.saturating_add(visible_lines) {
            anchor_line = caret_line.saturating_sub(visible_lines.saturating_sub(3));
        }
        anchor_line = anchor_line.min(last_full_viewport_anchor);
        self.viewport_anchor = lines.byte_of(anchor_line);
    }

    /// Place or extend the primary selection from a pointer hit expressed as a canonical document byte offset.
    pub fn pointer_select(
        &mut self,
        text: &str,
        position: usize,
        extend: bool,
        visible_lines: usize,
    ) -> bool {
        if self.minibuffer_active() {
            return false;
        }
        let position = clamp_to_grapheme_boundary(text, position.min(text.len()));
        let before = self.selections.clone();
        let before_primary = self.primary;
        if extend && !self.selections.is_empty() {
            self.primary = self.primary.min(self.selections.len().saturating_sub(1));
            if let Some(selection) = self.selections.get_mut(self.primary) {
                selection.head = position;
            }
        } else {
            self.selections.clear();
            self.selections.push(Selection::caret(position));
            self.primary = 0;
        }
        self.desired_column = None;
        self.mark_active = false;
        self.chord_prefix.clear();
        self.prefix_hud = None;
        self.prefix_argument = None;
        self.last_command = None;
        self.ensure_primary_visible(text, visible_lines);
        before_primary != self.primary || before != self.selections
    }

    pub fn cancel_transient(&mut self) {
        if let Minibuffer::Search { origin, .. } | Minibuffer::GotoLine { origin, .. } =
            &self.minibuffer
        {
            let primary = self.primary.min(self.selections.len().saturating_sub(1));
            if let Some(selection) = self.selections.get_mut(primary) {
                selection.collapse(*origin);
            }
        }
        self.chord_prefix.clear();
        self.prefix_hud = None;
        self.prefix_argument = None;
        self.clear_minibuffer();
        self.mark_active = false;
        self.last_command = None;
    }

    pub const fn minibuffer_active(&self) -> bool {
        matches!(
            self.minibuffer,
            Minibuffer::Command { .. }
                | Minibuffer::Search { .. }
                | Minibuffer::Buffer { .. }
                | Minibuffer::GotoLine { .. }
                | Minibuffer::Prompt { .. }
        )
    }

    /// `recenter-top-bottom` (C-l): scroll so the primary caret's line sits at the middle of the view, then, as the key repeats (`again`), at the top, then at the bottom.
    pub(super) fn recenter_top_bottom(
        &mut self,
        lines: &EditorLines<'_>,
        argument: Option<PrefixArgument>,
        again: bool,
    ) {
        let height = self.viewport_lines();
        let last_row = height.saturating_sub(1);
        let row = match argument {
            Some(PrefixArgument::Numeric { value, .. }) => {
                let from_edge = usize::try_from(value.unsigned_abs()).unwrap_or(usize::MAX);
                if value >= 0 {
                    from_edge.min(last_row)
                } else {
                    height.saturating_sub(from_edge)
                }
            }
            Some(PrefixArgument::Raw(_)) => height / 2,
            None => {
                self.recenter_phase = if again {
                    self.recenter_phase.saturating_add(1) % 3
                } else {
                    0
                };
                match self.recenter_phase {
                    0 => height / 2,
                    1 => 0,
                    _ => last_row,
                }
            }
        };
        let caret_line = lines.number_at(self.primary_selection().head);
        let last_full_viewport_anchor = lines.total().saturating_sub(height);
        let anchor = caret_line
            .saturating_sub(row)
            .min(last_full_viewport_anchor);
        self.viewport_anchor = lines.byte_of(anchor);
    }

    pub const fn chord_pending(&self) -> bool {
        !self.chord_prefix.is_empty()
    }

    /// Whether ESC alone is pending, Meta for the next key the keymap can spell.
    pub(crate) fn escape_pending(&self) -> bool {
        matches!(self.chord_prefix.as_slice(), [chord] if chord.is_escape())
    }

    /// End a pending ESC that no key took.
    pub(crate) fn end_escape_prefix(&mut self) -> bool {
        if !self.escape_pending() {
            return false;
        }
        self.chord_prefix.clear();
        self.prefix_hud = self.prefix_argument.map(PrefixArgument::hud);
        true
    }

    /// Open `minibuffer`, ending any key sequence still pending.
    pub(super) fn open_minibuffer(&mut self, minibuffer: Minibuffer) {
        self.chord_prefix.clear();
        self.prefix_hud = None;
        self.minibuffer = minibuffer;
        self.input_lifetime = Arc::new(());
    }

    /// Rebase view-local coordinates after a commit initiated by another controller, an open
    /// grid cell's record ([`super::grid::CellDraft::observe`]) included.
    pub fn observe_external(&mut self, seq: Seq, deltas: &[EditDelta]) {
        for selection in &mut self.selections {
            selection.rebase(deltas);
        }
        if let Face::Grid(grid) = &mut self.face
            && let Some(draft) = grid.draft.as_mut()
        {
            draft.observe(deltas);
        }
        self.viewport_anchor = rebase_position(self.viewport_anchor, deltas);
        if let Minibuffer::Search { origin, .. } | Minibuffer::GotoLine { origin, .. } =
            &mut self.minibuffer
        {
            *origin = rebase_position(*origin, deltas);
        }
        self.anchor_seq = seq;
        self.desired_column = None;
    }

    pub(super) fn observe_own(&mut self, seq: Seq, deltas: &[EditDelta]) {
        for selection in &mut self.selections {
            let position = rebase_position(selection.range().start, deltas);
            selection.collapse(position);
        }
        self.viewport_anchor = rebase_position(self.viewport_anchor, deltas);
        self.anchor_seq = seq;
        self.desired_column = None;
        self.mark_active = false;
    }

    /// Take the pending `C-u` argument for the one command being dispatched, clearing its HUD in the same step, so it can never reach a later one.
    pub(super) fn take_prefix_argument(&mut self) -> Option<PrefixArgument> {
        let argument = self.prefix_argument.take();
        if argument.is_some() {
            self.prefix_hud = None;
        }
        argument
    }

    /// Fold one decimal digit into a pending `C-u` argument (`C-u 2 C-k`).
    pub(super) fn accept_argument_digit(&mut self, key: &str) -> bool {
        let mut chars = key.chars();
        let (Some(digit), None) = (chars.next().and_then(|c| c.to_digit(10)), chars.next()) else {
            return false;
        };
        let Some(argument) = self
            .prefix_argument
            .and_then(|argument| argument.after_digit(digit))
        else {
            return false;
        };
        self.prefix_argument = Some(argument);
        self.prefix_hud = Some(argument.hud());
        true
    }
}

pub(super) fn previous_boundary(text: &str, position: usize) -> usize {
    let position = clamp_to_char_boundary(text, position);
    if position == 0 {
        return 0;
    }
    let start = line_start(text, position);
    if position == start {
        return if start >= 2 && text.as_bytes().get(start - 2..start) == Some(b"\r\n") {
            start - 2
        } else {
            start - 1
        };
    }
    text[start..position]
        .grapheme_indices()
        .map(|(at, _)| start.saturating_add(at))
        .last()
        .unwrap_or(start)
}

pub(super) fn next_boundary(text: &str, position: usize) -> usize {
    let position = clamp_to_char_boundary(text, position);
    if position == text.len() {
        return position;
    }
    position.saturating_add(text[position..].graphemes().next().map_or(0, str::len))
}

/// The words of `text` from byte `from` on (0, or a place no word spans), each
/// widened to the grapheme clusters it touches so the caret never lands inside one.
fn editor_words(text: &str, from: usize) -> impl Iterator<Item = Range<usize>> + '_ {
    let tail = text.get(from..).unwrap_or_default();
    let mut clusters = tail
        .grapheme_indices()
        .map(|(at, cluster)| at..at.saturating_add(cluster.len()))
        .peekable();
    aterm_grapheme::words(tail, aterm_grapheme::is_word_char).map(move |word| {
        while clusters
            .next_if(|cluster| cluster.end <= word.start)
            .is_some()
        {}
        let start = clusters
            .peek()
            .map_or(word.start, |cluster| cluster.start.min(word.start));
        while clusters.next_if(|cluster| cluster.end < word.end).is_some() {}
        let end = clusters
            .peek()
            .map_or(word.end, |cluster| cluster.end.max(word.end));
        #[cfg(any(test, debug_assertions))]
        EDITOR_WORDS_READ.with(|n| n.set(n.get().saturating_add(1)));
        from.saturating_add(start)..from.saturating_add(end)
    })
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// Test seam: how many words [`editor_words`] has read on this thread.
    static EDITOR_WORDS_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn previous_word_boundary(text: &str, position: usize, count: usize) -> usize {
    let position = clamp_to_char_boundary(text, position.min(text.len()));
    let count = count.clamp(1, 10_000);
    let mut recent_starts = VecDeque::with_capacity(count);
    for word in editor_words(text, 0) {
        if word.start >= position {
            break;
        }
        if recent_starts.len() == count {
            recent_starts.pop_front();
        }
        recent_starts.push_back(word.start);
    }
    if recent_starts.len() == count {
        recent_starts.front().copied().unwrap_or(0)
    } else {
        0
    }
}

/// The end of the `count`-th word that ends after `position` (or the end of the text), read in one pass from the last word break before the caret, so M-f costs the distance it moves, whatever its count.
pub(super) fn next_word_boundary(text: &str, position: usize, count: usize) -> usize {
    let position = clamp_to_char_boundary(text, position.min(text.len()));
    let from = aterm_grapheme::word_break_before(text, position, aterm_grapheme::is_word_char);
    editor_words(text, from)
        .filter(|word| word.end > position)
        .nth(count.clamp(1, 10_000).saturating_sub(1))
        .map_or(text.len(), |word| word.end)
}

pub(super) fn previous_line_position(text: &str, position: usize, desired_column: usize) -> usize {
    let position = position.min(text.len());
    let current_start = line_start(text, position);
    if current_start == 0 {
        return position;
    }
    let previous_end = current_start.saturating_sub(1);
    let previous_start = line_start(text, previous_end);
    byte_at_editor_display_column(
        text,
        previous_start,
        line_text_end(text, previous_start),
        desired_column,
    )
}

pub(super) fn next_line_position(text: &str, position: usize, desired_column: usize) -> usize {
    let position = position.min(text.len());
    let current_end = line_end(text, position);
    if current_end >= text.len() {
        return position;
    }
    let next_start = current_end.saturating_add(1);
    let next_end = line_text_end(text, next_start);
    byte_at_editor_display_column(text, next_start, next_end, desired_column)
}

/// Apply a one-line motion `count` times.
pub(super) fn repeat_line_motion(
    position: usize,
    count: usize,
    step: impl Fn(usize) -> usize,
) -> usize {
    let mut at = position;
    for _ in 0..count {
        let next = step(at);
        if next == at {
            break;
        }
        at = next;
    }
    at
}

trust_contract_fn! {
    /// How many lines a viewport holds when the renderer offers
    /// `visible_lines`: at least one and at most 256, the clamp the view has
    /// always applied (`visible_lines.clamp(1, 256)`).
    #[expect(
        clippy::manual_clamp,
        reason = "Trust kernel-imports a select, not a call to `Ord::clamp` (E6 refuses calls)"
    )]
    fn visible_line_capacity(visible_lines: usize) -> usize
    clauses [ensures result >= 1 && result <= 256 by visible_line_capacity_within_bounds
             ensures visible_lines < 1 || visible_lines > 256 || result == visible_lines by visible_line_capacity_keeps_an_in_range_value]
    {
        if visible_lines < 1 {
            1
        } else if visible_lines > 256 {
            256
        } else {
            visible_lines
        }
    }
}

verify_island! {
    clean {
        theorem visible_line_capacity_one_le_cap : Nat.le (UInt64.toNat 1) (UInt64.toNat 256) := by
          decide

        theorem visible_line_capacity_within_bounds (visible_lines : UInt64) :
            And (Nat.le (UInt64.toNat 1) (UInt64.toNat (trust_import_amacs__editor__view__visible_line_capacity visible_lines)))
                (Nat.le (UInt64.toNat (trust_import_amacs__editor__view__visible_line_capacity visible_lines)) (UInt64.toNat 256)) :=
          @Decidable.casesOn (LT.lt visible_lines 1)
            (fun (d : Decidable (LT.lt visible_lines 1)) =>
              And (Nat.le (UInt64.toNat 1) (UInt64.toNat (@ite UInt64 (LT.lt visible_lines 1) d 1 (@ite UInt64 (LT.lt 256 visible_lines) (instDecidableUInt64Lt 256 visible_lines) 256 visible_lines))))
                  (Nat.le (UInt64.toNat (@ite UInt64 (LT.lt visible_lines 1) d 1 (@ite UInt64 (LT.lt 256 visible_lines) (instDecidableUInt64Lt 256 visible_lines) 256 visible_lines))) (UInt64.toNat 256)))
            (instDecidableUInt64Lt visible_lines 1)
            (fun (at_least_one : Not (LT.lt visible_lines 1)) =>
              @Decidable.casesOn (LT.lt 256 visible_lines)
                (fun (e : Decidable (LT.lt 256 visible_lines)) =>
                  And (Nat.le (UInt64.toNat 1) (UInt64.toNat (@ite UInt64 (LT.lt 256 visible_lines) e 256 visible_lines)))
                      (Nat.le (UInt64.toNat (@ite UInt64 (LT.lt 256 visible_lines) e 256 visible_lines)) (UInt64.toNat 256)))
                (instDecidableUInt64Lt 256 visible_lines)
                (fun (at_most_cap : Not (LT.lt 256 visible_lines)) => And.intro (Nat.ge_of_not_lt at_least_one) (Nat.ge_of_not_lt at_most_cap))
                (fun (_ : LT.lt 256 visible_lines) => And.intro visible_line_capacity_one_le_cap (Nat.le_refl (UInt64.toNat 256))))
            (fun (_ : LT.lt visible_lines 1) => And.intro (Nat.le_refl (UInt64.toNat 1)) visible_line_capacity_one_le_cap)

        theorem visible_line_capacity_keeps_an_in_range_value (visible_lines : UInt64) :
            Or (Or (LT.lt visible_lines 1) (LT.lt 256 visible_lines)) (Eq (trust_import_amacs__editor__view__visible_line_capacity visible_lines) visible_lines) :=
          @Decidable.casesOn (LT.lt visible_lines 1)
            (fun (d : Decidable (LT.lt visible_lines 1)) =>
              Or (Or (LT.lt visible_lines 1) (LT.lt 256 visible_lines))
                 (Eq (@ite UInt64 (LT.lt visible_lines 1) d 1 (@ite UInt64 (LT.lt 256 visible_lines) (instDecidableUInt64Lt 256 visible_lines) 256 visible_lines)) visible_lines))
            (instDecidableUInt64Lt visible_lines 1)
            (fun (_ : Not (LT.lt visible_lines 1)) =>
              @Decidable.casesOn (LT.lt 256 visible_lines)
                (fun (e : Decidable (LT.lt 256 visible_lines)) =>
                  Or (Or (LT.lt visible_lines 1) (LT.lt 256 visible_lines)) (Eq (@ite UInt64 (LT.lt 256 visible_lines) e 256 visible_lines) visible_lines))
                (instDecidableUInt64Lt 256 visible_lines)
                (fun (_ : Not (LT.lt 256 visible_lines)) => Or.inr (Eq.refl visible_lines))
                (fun (above : LT.lt 256 visible_lines) => Or.inl (Or.inr above)))
            (fun (below : LT.lt visible_lines 1) => Or.inl (Or.inl below))
    }
}

/// Clamp an externally supplied editor position to the preceding user-visible character boundary.
pub(super) fn clamp_to_grapheme_boundary(text: &str, position: usize) -> usize {
    let position = clamp_to_char_boundary(text, position);
    if position == 0 || position == text.len() {
        return position;
    }
    let start = line_start(text, position);
    let end = line_end(text, position);
    let scan_end = end.saturating_add(usize::from(end < text.len()));
    text[start..scan_end]
        .grapheme_indices()
        .map(|(offset, _)| start.saturating_add(offset))
        .take_while(|boundary| *boundary <= position)
        .last()
        .unwrap_or(start)
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
