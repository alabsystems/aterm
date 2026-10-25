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
    /// The last accepted query belongs to this view, never to a terminal or another editor view.
    pub(super) last_search_query: String,
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
            mark_active: false,
            prefix_hud: None,
            desired_column: None,
            chord_prefix: Vec::new(),
            prefix_argument: None,
            anchor_seq,
            last_command: None,
            recenter_phase: 0,
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

    pub const fn has_search_query(&self) -> bool {
        match &self.minibuffer {
            Minibuffer::Search { query, .. } => {
                !query.is_empty() || !self.last_search_query.is_empty()
            }
            Minibuffer::Inactive => !self.last_search_query.is_empty(),
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

    /// Install the renderer's capacity.
    pub fn reconcile_viewport_with(
        &mut self,
        lines: &EditorLines<'_>,
        visible_lines: usize,
    ) -> bool {
        let before_anchor = self.viewport_anchor;
        let before_lines = self.viewport_lines;
        self.viewport_lines = visible_line_capacity(visible_lines);
        if before_lines == self.viewport_lines {
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

    /// Rebase view-local coordinates after a commit initiated by another controller.
    pub fn observe_external(&mut self, seq: Seq, deltas: &[EditDelta]) {
        for selection in &mut self.selections {
            selection.rebase(deltas);
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
    let position = position.min(text.len());
    text[..position]
        .grapheme_indices()
        .map(|(at, _)| at)
        .last()
        .unwrap_or(0)
}

pub(super) fn next_boundary(text: &str, position: usize) -> usize {
    let position = position.min(text.len());
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
mod tests {
    use super::*;
    use crate::editor::projection::{byte_of_line, line_number};
    use crate::editor::{EditorCommand, editor};
    use aterm_models_native::native_editor_viewport_model;
    use aterm_spec::interp::admits;

    #[test]
    fn visible_line_capacity_is_the_clamp_it_replaced() {
        for lines in [0, 1, 2, 255, 256, 257, usize::MAX] {
            assert_eq!(visible_line_capacity(lines), lines.clamp(1, 256));
        }
    }

    /// `selections` is public, so a host can empty it; the primary selection
    /// is then a caret at 0 (it used to panic), and an out-of-range `primary`
    /// still names the last selection.
    #[test]
    fn primary_selection_of_an_emptied_view_is_a_caret_at_zero() {
        let (_, _, mut view, _) = editor("alpha\nbeta");
        view.selections = vec![Selection::caret(2), Selection::caret(7)];
        view.primary = 9;
        assert_eq!(view.primary_selection(), &Selection::caret(7));
        view.selections.clear();
        assert_eq!(view.primary_selection(), &Selection::caret(0));
        view.primary = 0;
        assert_eq!(view.primary_selection(), &Selection::caret(0));
    }

    /// THE PAINTED VIEW IS THE STORED VIEW: reconciling an unchanged capacity
    /// (what every frame does on a copy) keeps a view scrolled away from the
    /// caret where it was scrolled to, so the wheel scrolls what is painted
    /// and nothing builds up unseen; a changed capacity (a resize) reveals the
    /// caret.
    #[test]
    fn an_unchanged_capacity_never_pulls_a_scrolled_view_back_to_the_caret() {
        let text = (0..300)
            .map(|line| format!("line {line:03}\n"))
            .collect::<String>();
        let (_, _, mut view, _) = editor(&text);
        let lines = EditorLines::scanning(&text);
        view.reconcile_viewport(&text, 35);
        view.scroll_lines(&text, 3);
        let mut painted = view.clone();
        painted.reconcile_viewport_with(&lines, 35);
        assert_eq!(lines.number_at(painted.viewport_anchor), 3);
        assert_eq!(painted.viewport_anchor, view.viewport_anchor);
        view.scroll_lines(&text, -1);
        let mut painted = view.clone();
        painted.reconcile_viewport_with(&lines, 35);
        assert_eq!(
            lines.number_at(painted.viewport_anchor),
            2,
            "one line back paints"
        );
        let mut resized = view.clone();
        view.scroll_lines(&text, 100);
        resized.viewport_anchor = view.viewport_anchor;
        resized.reconcile_viewport_with(&lines, 20);
        assert_eq!(lines.number_at(resized.viewport_anchor), 0);
    }

    #[test]
    fn vertical_motion_preserves_a_bounded_text_column() {
        let (mut store, mut workspace, mut view, _) = editor("alpha\nβ\ngamma");
        view.selections = vec![Selection::caret(4)];
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
            .unwrap();
        assert_eq!(view.primary_selection().head, "alpha\nβ".len());
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
            .unwrap();
        assert_eq!(view.primary_selection().head, "alpha\nβ\ngamm".len());
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineUp)
            .unwrap();
        assert_eq!(view.primary_selection().head, "alpha\nβ".len());
    }

    #[test]
    fn vertical_motion_preserves_display_cells_across_zwj_and_cjk_lines() {
        let text = "abcd\n👩‍💻xyz\n中xyz";
        let (mut store, mut workspace, mut view, _) = editor(text);
        view.selections = vec![Selection::caret("abcd".len())];

        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
            .unwrap();
        let emoji_target = "abcd\n".len() + "👩‍💻xy".len();
        assert_eq!(view.primary_selection().head, emoji_target);
        assert!(
            text.grapheme_indices()
                .any(|(byte, _)| byte == emoji_target)
        );

        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
            .unwrap();
        let cjk_target = "abcd\n👩‍💻xyz\n".len() + "中xy".len();
        assert_eq!(view.primary_selection().head, cjk_target);
        assert!(text.grapheme_indices().any(|(byte, _)| byte == cjk_target));

        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineUp)
            .unwrap();
        assert_eq!(view.primary_selection().head, emoji_target);
    }

    #[test]
    fn viewport_scroll_and_caret_reveal_use_stable_line_anchors() {
        let text = (0..100)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (_, _, mut view, _) = editor(&text);
        view.scroll_lines(&text, 40);
        assert_eq!(&text[view.viewport_anchor..][..8], "line 40\n");
        view.selections = vec![Selection::caret(byte_of_line(&text, 90))];
        view.ensure_primary_visible(&text, 20);
        assert_eq!(line_number(&text, view.viewport_anchor), 73);
        view.selections = vec![Selection::caret(byte_of_line(&text, 5))];
        view.ensure_primary_visible(&text, 20);
        assert_eq!(line_number(&text, view.viewport_anchor), 5);
    }

    #[test]
    fn viewport_scroll_stores_only_presentable_anchors_at_every_capacity() {
        let text = (0..12)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (_, _, mut view, _) = editor(&text);
        view.reconcile_viewport(&text, 4);

        view.scroll_lines(&text, i32::MAX);
        assert_eq!(line_number(&text, view.viewport_anchor), 9);
        let bottom = view.viewport_anchor;
        view.scroll_lines(&text, -1);
        assert_eq!(line_number(&text, view.viewport_anchor), 8);
        assert!(view.viewport_anchor < bottom, "one reverse step moves now");

        for short in ["", "one", "one\ntwo"] {
            let (_, _, mut short_view, _) = editor(short);
            short_view.reconcile_viewport(short, 40);
            short_view.scroll_lines(short, i32::MAX);
            assert_eq!(short_view.viewport_anchor, 0);
            short_view.scroll_lines(short, i32::MIN);
            assert_eq!(short_view.viewport_anchor, 0);
        }

        let (_, _, mut zero_view, _) = editor(&text);
        zero_view.viewport_lines = 0;
        zero_view.scroll_lines(&text, i32::MAX);
        assert_eq!(line_number(&text, zero_view.viewport_anchor), 12);
        zero_view.scroll_lines(&text, -1);
        assert_eq!(line_number(&text, zero_view.viewport_anchor), 11);
    }

    #[test]
    fn real_viewport_reconciliation_conforms_to_renderer_capacity_model() {
        let text = (0..30)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (_, _, mut view, _) = editor(&text);
        view.selections = vec![Selection::caret(byte_of_line(&text, 20))];
        let short_text = "# Manual\nfont_px = 14\n";
        let (_, _, mut short_view, _) = editor(short_text);
        short_view.viewport_anchor = short_text.len();
        short_view.selections = vec![Selection::caret(short_text.len())];

        let model = native_editor_viewport_model();
        let before = model.init_state();
        assert!(view.reconcile_viewport(&text, 8));
        assert!(short_view.reconcile_viewport(short_text, 40));
        let mut after = before.clone();
        after.insert(
            "anchor_line",
            line_number(&text, view.viewport_anchor) as i64,
        );
        after.insert("visible_lines", view.viewport_lines() as i64);
        after.insert(
            "short_anchor_line",
            line_number(short_text, short_view.viewport_anchor) as i64,
        );
        after.insert("short_visible_lines", short_view.viewport_lines() as i64);
        after.insert("resized", 1);
        assert_eq!(admits(&model, &before, &after), Some("Resize"));
        assert!(model.check_invariant("CaretVisibleAfterResize", &after));
        assert!(model.check_invariant("ShortDocumentFullyVisible", &after));

        let scroll_text = (0..12)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (_, _, mut scroll_view, _) = editor(&scroll_text);
        scroll_view.reconcile_viewport(&scroll_text, 4);
        scroll_view.scroll_lines(&scroll_text, i32::MAX);
        let scroll_before = model.init_state();
        let mut at_bottom = scroll_before.clone();
        at_bottom.insert(
            "scroll_anchor_line",
            line_number(&scroll_text, scroll_view.viewport_anchor) as i64,
        );
        at_bottom.insert("scroll_phase", 1);
        assert_eq!(
            admits(&model, &scroll_before, &at_bottom),
            Some("Overscroll")
        );
        assert!(model.check_invariant("StoredScrollAnchorPresentable", &at_bottom));
        scroll_view.scroll_lines(&scroll_text, -1);
        let mut one_line_up = at_bottom.clone();
        one_line_up.insert(
            "scroll_anchor_line",
            line_number(&scroll_text, scroll_view.viewport_anchor) as i64,
        );
        one_line_up.insert("scroll_phase", 2);
        assert_eq!(
            admits(&model, &at_bottom, &one_line_up),
            Some("ReverseScroll")
        );
        assert!(model.check_invariant("FirstReverseStepMoves", &one_line_up));

        let mut fixed_desktop = before.clone();
        fixed_desktop.insert("visible_lines", 8);
        fixed_desktop.insert("resized", 1);
        assert_eq!(admits(&model, &before, &fixed_desktop), None);
        assert!(!model.check_invariant("CaretVisibleAfterResize", &fixed_desktop));

        let mut stranded_short = after;
        stranded_short.insert("short_anchor_line", 2);
        assert_eq!(admits(&model, &before, &stranded_short), None);
        assert!(!model.check_invariant("ShortDocumentFullyVisible", &stranded_short));
    }

    #[test]
    fn pointer_selection_collapses_extends_and_clamps_to_utf8() {
        let text = "aé🦀z\nsecond";
        let (_, _, mut view, _) = editor(text);

        assert!(view.pointer_select(text, 2, false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret(1));

        assert!(view.pointer_select(text, "aé🦀".len(), true, 4));
        assert_eq!(view.primary_selection().range(), 1.."aé🦀".len());

        let first_line_end = text.find('\n').unwrap();
        assert!(view.pointer_select(text, first_line_end, false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret(first_line_end));

        assert!(view.pointer_select(text, usize::MAX, false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret(text.len()));
        assert!(!view.pointer_select(text, text.len(), false, 4));
    }

    #[test]
    fn pointer_selection_clamps_to_complete_user_visible_graphemes() {
        let text = "ae\u{301}👩‍💻z";
        let (_, _, mut view, _) = editor(text);

        assert!(view.pointer_select(text, "ae".len(), false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret("a".len()));

        let emoji_start = "ae\u{301}".len();
        assert!(view.pointer_select(text, emoji_start + "👩".len(), false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret(emoji_start));

        let crlf = "ab\r\ncd";
        let (_, _, mut view, _) = editor(crlf);
        assert!(view.pointer_select(crlf, 3, false, 4));
        assert_eq!(
            view.primary_selection(),
            &Selection::caret(2),
            "the interior of CRLF retreats to the start of the pair"
        );
        assert!(!view.pointer_select(crlf, 2, false, 4));
    }

    #[test]
    fn set_mark_persists_across_motion_and_kill_region() {
        let (mut store, mut workspace, mut view, document) = editor("alpha beta");
        workspace
            .execute(&mut store, &mut view, EditorCommand::SetMark)
            .unwrap();
        for _ in 0..5 {
            workspace
                .execute(&mut store, &mut view, EditorCommand::MoveForward)
                .unwrap();
        }
        assert!(view.mark_active);
        assert_eq!(view.primary_selection(), &Selection { anchor: 0, head: 5 });

        workspace
            .execute(&mut store, &mut view, EditorCommand::KillRegion)
            .unwrap();
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), " beta");
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("alpha")
        );
        assert!(!view.mark_active);
        assert!(view.primary_selection().is_caret());
    }

    /// The words M-f then M-b find in `text`, as byte ranges.
    fn motion_words(text: &str) -> Vec<(usize, usize)> {
        let mut words = Vec::new();
        let mut at = 0;
        loop {
            let end = next_word_boundary(text, at, 1);
            let start = previous_word_boundary(text, end, 1);
            if end == at || start < at {
                return words;
            }
            words.push((start, end));
            at = end;
        }
    }

    /// The words a double-click selects in `text`, widened to the grapheme
    /// clusters they touch: the editor's caret never lands inside a cluster.
    fn clicked_words(text: &str) -> Vec<(usize, usize)> {
        let smart = aterm_core::selection::SmartSelection::new();
        let mut bounds: Vec<usize> = text.grapheme_indices().map(|(at, _)| at).collect();
        bounds.push(text.len());
        let mut words: Vec<(usize, usize)> = text
            .char_indices()
            .filter_map(|(at, _)| smart.word_boundaries_at(text, at))
            .map(|(start, end)| {
                let floor = bounds.iter().rev().find(|&&b| b <= start).copied();
                let ceil = bounds.iter().find(|&&b| b >= end).copied();
                (floor.unwrap_or(0), ceil.unwrap_or(text.len()))
            })
            .collect();
        words.dedup();
        words
    }

    /// M-f / M-b find the words a double-click selects. They used to call a word
    /// any run of grapheme clusters that START with a letter, so a cluster led by
    /// a prepended number sign (`۝١`) was no word, and a ZWJ or ZWNJ, which joins
    /// the cluster before it, welded `foo` and `bar` into one word where a
    /// double-click (and the find field) see two: an invisible character must not
    /// weld two words together.
    #[test]
    fn word_motion_finds_the_words_a_double_click_selects() {
        for word in [
            "\u{06DD}\u{0661}\u{0662}\u{0663}",
            "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
            "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}",
            "\u{09B0}\u{200D}\u{09CD}\u{09AF}\u{09BE}\u{09AC}",
            "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}",
            "\u{182C}\u{1820}\u{1837}\u{180E}\u{1820}",
        ] {
            let text = format!("x {word} y");
            let end = 2 + word.len();
            assert_eq!(
                motion_words(&text),
                [(0, 1), (2, end), (end + 1, end + 2)],
                "{word:?}"
            );
            assert_eq!(motion_words(&text), clicked_words(&text), "{word:?}");
        }
        for (text, words) in [
            ("foo\u{200D}bar", [(0, 6), (6, 9)]),
            ("foo\u{200C}bar", [(0, 6), (6, 9)]),
            ("abc\u{202E}def", [(0, 3), (6, 9)]),
            ("a\u{2060}b", [(0, 1), (4, 5)]),
            ("x\u{200E}y", [(0, 1), (4, 5)]),
            ("ab\u{180E}cd", [(0, 2), (5, 7)]),
        ] {
            assert_eq!(motion_words(text), words, "{text:?}");
            assert_eq!(motion_words(text), clicked_words(text), "{text:?}");
        }
    }

    /// M-f with a count reads the line once, from the last word break before the
    /// caret: its cost is the distance it moves, pinned here by the words it reads.
    #[test]
    fn forward_word_applies_a_count_in_one_pass_from_the_last_break() {
        let text = "ab.".repeat(64 * 1024);
        let (mut store, mut workspace, mut view, _) = editor(&text);
        view.selections = vec![Selection::caret(0)];
        view.prefix_argument = Some(PrefixArgument::Numeric {
            value: 1000,
            open: true,
        });
        EDITOR_WORDS_READ.with(|n| n.set(0));
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveWordForward)
            .unwrap();
        assert_eq!(view.primary_selection().head, 1000 * 3 - 1);
        let read = EDITOR_WORDS_READ.with(std::cell::Cell::get);
        assert!(read <= 1001, "C-u 1000 M-f read {read} words");

        view.selections = vec![Selection::caret(30_000 * 3)];
        EDITOR_WORDS_READ.with(|n| n.set(0));
        for _ in 0..1000 {
            workspace
                .execute(&mut store, &mut view, EditorCommand::MoveWordForward)
                .unwrap();
        }
        assert_eq!(view.primary_selection().head, 31_000 * 3 - 1);
        let read = EDITOR_WORDS_READ.with(std::cell::Cell::get);
        assert!(read <= 2 * 1000, "1000 M-f read {read} words");
    }

    #[test]
    fn backward_word_scans_a_long_token_once_and_applies_counts_in_one_pass() {
        let token = "a".repeat(256 * 1024);
        let text = format!("{token} one two three");
        let one = token.len() + 1;
        let two = one + "one ".len();
        let three = two + "two ".len();

        assert_eq!(previous_word_boundary(&text, text.len(), 1), three);
        assert_eq!(previous_word_boundary(&text, text.len(), 2), two);
        assert_eq!(previous_word_boundary(&text, text.len(), 3), one);
        assert_eq!(previous_word_boundary(&text, three, 1), two);
        assert_eq!(previous_word_boundary(&text, text.len(), 4), 0);
    }

    #[test]
    fn motion_treats_combining_and_zwj_sequences_as_one_character() {
        let text = "e\u{301}👩‍💻z";
        let (mut store, mut workspace, mut view, _) = editor(text);
        view.selections = vec![Selection::caret(text.len())];
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveBackward)
            .unwrap();
        assert_eq!(view.primary_selection().head, "e\u{301}👩‍💻".len());
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveBackward)
            .unwrap();
        assert_eq!(view.primary_selection().head, "e\u{301}".len());
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveBackward)
            .unwrap();
        assert_eq!(view.primary_selection().head, 0);
    }
}
