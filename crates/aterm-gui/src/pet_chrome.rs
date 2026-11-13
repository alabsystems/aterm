// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S CHROME ON THE GRID, AS THE RESIDENT PET SEES IT. aterm paints some of its own chrome INTO grid cells after the pet has looked at the frame: the footer's one write (`claude_footer`: Claude Code's rule — its chips and facts — the harness card's line, and the Codex chips, which sit on the BLANK row Codex keeps over its status row) and a fact menu's card. Where that write rewrites a row of the program's (Claude Code's mode row), the words it lays are chrome and the rest of the row it blanks is glass ([`PetChrome::note_blanked`]): the pet, which read the program's cells, reads the program's letters under that blank as the nothing the person sees.

use aterm_core::terminal::RenderCell;
use aterm_effects::pet_world::PetRect;

/// The stroke a composer's rule is drawn in ([`PetChrome::note_painted`]).
const RULE_STROKE: char = '\u{2500}';

/// The cells the host's chrome splices painted over the focused pane, for the resident pet (module docs).
#[derive(Debug, Default)]
pub(crate) struct PetChrome {
    /// What the pet reads: the cells the last finished pass painted,
    /// pane-local.
    held: Vec<PetRect>,
    /// This pass's, being gathered; swapped into `held` at its commit.
    next: Vec<PetRect>,
    /// The transient overlays among them (a fact menu's card), held
    /// and gathered the same way ([`PetChrome::note_overlay`]).
    held_overlays: Vec<PetRect>,
    next_overlays: Vec<PetRect>,
    /// The glass the pass painted blank over the program's cells ([`PetChrome::note_blanked`]),
    /// held and gathered the same way.
    held_blanked: Vec<PetRect>,
    next_blanked: Vec<PetRect>,
    /// The focused pane of this pass in frame cells `(row, col, rows, cols)`:
    /// a write outside it is another pane's, which the pet does not live in.
    pane: Option<(usize, usize, usize, usize)>,
    /// Whether a pet observation read `held` since the last commit.
    read: std::cell::Cell<bool>,
    /// Whether [`PetChrome::begin`] opened the pass a commit would end.
    begun: bool,
    /// The content sequence of the frame the chrome in `held` was painted over, and of the one
    /// this pass paints over ([`PetChrome::painted_over`]): the pet reads `held` over the next
    /// frame, and carries it with the rows it was painted on where they have moved since.
    held_over: Option<u64>,
    next_over: Option<u64>,
}

impl PetChrome {
    /// The cells the pet takes as occupied, marked read: the pass that moves them owes the window the frame that shows them to the pet.
    pub(crate) fn held(&self) -> &[PetRect] {
        self.read.set(true);
        &self.held
    }

    /// The transient overlays the pet takes as occupied, marked read the same way: a fact menu's card ([`PetChrome::note_overlay`]).
    pub(crate) fn held_overlays(&self) -> &[PetRect] {
        self.read.set(true);
        &self.held_overlays
    }

    /// The glass the pet takes as blank whatever the program drew under it, marked read the same
    /// way ([`PetChrome::note_blanked`]; `aterm_effects::pet_world::PetReserve::blanked`).
    pub(crate) fn held_blanked(&self) -> &[PetRect] {
        self.read.set(true);
        &self.held_blanked
    }

    /// Start a pass over the focused pane at `pane` (frame cells `(row, col, rows, cols)`; `None`: no focused terminal pane, so nothing is noted).
    pub(crate) fn begin(&mut self, pane: Option<(usize, usize, usize, usize)>) {
        self.next.clear();
        self.next_overlays.clear();
        self.next_blanked.clear();
        self.pane = pane;
        self.begun = true;
        self.next_over = None;
    }

    /// This pass paints over the frame whose content sequence is `seq` (`RenderInput::content_seq`).
    pub(crate) fn painted_over(&mut self, seq: u64) {
        self.next_over = Some(seq);
    }

    /// The content sequence of the frame the chrome [`PetChrome::held`] gives was painted over
    /// (`aterm_effects::pet_world::PetReserve::painted_over`).
    pub(crate) fn held_painted_over(&self) -> Option<u64> {
        self.held_over
    }

    /// A splice painted `cols` cells from frame cell `(row, col)` down `rows` rows: the part inside the focused pane, pane-local.
    pub(crate) fn note(&mut self, row: usize, col: usize, rows: usize, cols: usize) {
        if let Some(rect) = self.pane_local(row, col, rows, cols) {
            self.next.push(rect);
        }
    }

    /// A splice painted `cells` from frame cell `(row, col)`: the runs of them that carry the host's words and marks, as [`PetChrome::note`] notes a write. A rule's strokes between them are the program's (Claude Code's lower rule around the footer's chips and facts); the top rule the footer repaints with its lockup and switch is the host's line and is noted whole with [`PetChrome::note`] instead.
    pub(crate) fn note_painted(&mut self, row: usize, col: usize, cells: &[RenderCell]) {
        let mut run: Option<usize> = None;
        for (i, cell) in cells.iter().enumerate() {
            match (cell.ch == RULE_STROKE, run) {
                (false, None) => run = Some(i),
                (true, Some(start)) => {
                    self.note(row, col + start, 1, i - start);
                    run = None;
                }
                _ => {}
            }
        }
        if let Some(start) = run {
            self.note(row, col + start, 1, cells.len() - start);
        }
    }

    /// As [`PetChrome::note`], for a TRANSIENT overlay (a fact menu's card): protected
    /// alike, but never ground the pet's rest seat is solved on.
    pub(crate) fn note_overlay(&mut self, row: usize, col: usize, rows: usize, cols: usize) {
        if let Some(rect) = self.pane_local(row, col, rows, cols) {
            self.next_overlays.push(rect);
        }
    }

    /// A rewrite of the program's row painted blank glass over `cols` of its cells from frame
    /// cell `(row, col)` (the tail of Claude Code's mode row past the words the rewrite laid, and
    /// the row its effort hint wrapped to): the program's letters there are hidden, so the cells
    /// are the pet's free glass, not chrome. Covering them hides nothing the person sees.
    pub(crate) fn note_blanked(&mut self, row: usize, col: usize, cols: usize) {
        if let Some(rect) = self.pane_local(row, col, 1, cols) {
            self.next_blanked.push(rect);
        }
    }

    /// The part of a write inside the focused pane, pane-local.
    fn pane_local(&self, row: usize, col: usize, rows: usize, cols: usize) -> Option<PetRect> {
        let (pr, pc, prows, pcols) = self.pane?;
        let (r0, r1) = (row.max(pr), (row + rows).min(pr + prows));
        let (c0, c1) = (col.max(pc), (col + cols).min(pc + pcols));
        (r0 < r1 && c0 < c1).then(|| {
            PetRect::new(
                (r0 - pr) as f32,
                (c0 - pc) as f32,
                (r1 - r0) as f32,
                (c1 - c0) as f32,
            )
        })
    }

    /// End the pass: what it gathered becomes what the pet reads.
    pub(crate) fn commit(&mut self) -> bool {
        if !std::mem::take(&mut self.begun) {
            return false;
        }
        let moved = self.next != self.held
            || self.next_overlays != self.held_overlays
            || self.next_blanked != self.held_blanked;
        self.held_over = self.next_over;
        if moved {
            std::mem::swap(&mut self.next, &mut self.held);
            std::mem::swap(&mut self.next_overlays, &mut self.held_overlays);
            std::mem::swap(&mut self.next_blanked, &mut self.held_blanked);
        }
        self.pane = None;
        moved && self.read.replace(false)
    }
}

#[cfg(test)]
#[path = "pet_chrome_tests.rs"]
mod tests;
