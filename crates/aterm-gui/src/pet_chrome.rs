// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S CHROME ON THE GRID, AS THE RESIDENT PET SEES IT. aterm paints some of its own chrome INTO grid cells after the pet has looked at the frame: the footer's one write (`claude_footer`: Claude Code's rule — its chips and facts — the harness card's line, and the Codex chips, which sit on the BLANK row Codex keeps over its status row) and the Autopilot menu.

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
    /// The transient overlays among them (the Autopilot menu's card), held
    /// and gathered the same way ([`PetChrome::note_overlay`]).
    held_overlays: Vec<PetRect>,
    next_overlays: Vec<PetRect>,
    /// The focused pane of this pass in frame cells `(row, col, rows, cols)`:
    /// a write outside it is another pane's, which the pet does not live in.
    pane: Option<(usize, usize, usize, usize)>,
    /// Whether a pet observation read `held` since the last commit.
    read: std::cell::Cell<bool>,
    /// Whether [`PetChrome::begin`] opened the pass a commit would end.
    begun: bool,
}

impl PetChrome {
    /// The cells the pet takes as occupied, marked read: the pass that moves them owes the window the frame that shows them to the pet.
    pub(crate) fn held(&self) -> &[PetRect] {
        self.read.set(true);
        &self.held
    }

    /// The transient overlays the pet takes as occupied, marked read the same way: the Autopilot menu's card ([`PetChrome::note_overlay`]).
    pub(crate) fn held_overlays(&self) -> &[PetRect] {
        self.read.set(true);
        &self.held_overlays
    }

    /// Start a pass over the focused pane at `pane` (frame cells `(row, col, rows, cols)`; `None`: no focused terminal pane, so nothing is noted).
    pub(crate) fn begin(&mut self, pane: Option<(usize, usize, usize, usize)>) {
        self.next.clear();
        self.next_overlays.clear();
        self.pane = pane;
        self.begun = true;
    }

    /// A splice painted `cols` cells from frame cell `(row, col)` down `rows` rows: the part inside the focused pane, pane-local.
    pub(crate) fn note(&mut self, row: usize, col: usize, rows: usize, cols: usize) {
        if let Some(rect) = self.pane_local(row, col, rows, cols) {
            self.next.push(rect);
        }
    }

    /// A splice painted `cells` from frame cell `(row, col)`: the runs of them that carry the host's words and marks, as [`PetChrome::note`] notes a write.
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

    /// As [`PetChrome::note`], for a TRANSIENT overlay (the Autopilot menu's card): protected
    /// alike, but never ground the pet's rest seat is solved on.
    pub(crate) fn note_overlay(&mut self, row: usize, col: usize, rows: usize, cols: usize) {
        if let Some(rect) = self.pane_local(row, col, rows, cols) {
            self.next_overlays.push(rect);
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
        let moved = self.next != self.held || self.next_overlays != self.held_overlays;
        if moved {
            std::mem::swap(&mut self.next, &mut self.held);
            std::mem::swap(&mut self.next_overlays, &mut self.held_overlays);
        }
        self.pane = None;
        moved && self.read.replace(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A write is kept pane-local and clipped to the focused pane; another
    /// pane's write is no pet's business; a pass that paints what the last
    /// one did owes nothing, and one that moves the cells owes a frame only
    /// where a pet read the old ones.
    #[test]
    fn writes_are_pane_local_and_a_moved_set_owes_the_pet_a_frame() {
        let mut chrome = PetChrome::default();
        let pane = Some((1, 73, 49, 71));
        chrome.begin(pane);
        chrome.note(47, 74, 1, 6);
        chrome.note(30, 2, 1, 10);
        chrome.note(48, 140, 1, 10);
        assert!(!chrome.commit(), "no pet read anything yet");
        assert_eq!(
            chrome.held(),
            [
                PetRect::new(46.0, 1.0, 1.0, 6.0),
                PetRect::new(47.0, 67.0, 1.0, 4.0)
            ]
        );
        chrome.begin(pane);
        chrome.note(47, 74, 1, 6);
        chrome.note(48, 140, 1, 10);
        assert!(!chrome.commit(), "a still pass owes nothing");
        let _ = chrome.held();
        chrome.begin(pane);
        chrome.note(47, 74, 1, 9);
        assert!(chrome.commit(), "moved cells owe the pet a frame");
        assert_eq!(chrome.held(), [PetRect::new(46.0, 1.0, 1.0, 9.0)]);
        chrome.begin(pane);
        chrome.note(47, 74, 1, 9);
        assert!(!chrome.commit(), "and only one");
        chrome.begin(None);
        chrome.note(47, 74, 1, 9);
        assert!(chrome.commit());
        assert!(chrome.held().is_empty());
        let mut quiet = PetChrome::default();
        quiet.begin(pane);
        quiet.note(47, 74, 1, 6);
        assert!(!quiet.commit(), "no pet read: nothing owed");
    }

    /// THE MENU IS AN OVERLAY, NOT GROUND: a card noted as an overlay is
    /// held apart from the chrome ([`PetChrome::held_overlays`], never in
    /// [`PetChrome::held`]), and a pass where only it moves (the menu opens
    /// or closes over still chrome) owes a pet that read the old sets a
    /// frame.
    #[test]
    fn a_menu_is_held_apart_from_the_chrome_and_its_move_owes_a_frame() {
        let pane = Some((1, 0, 30, 100));
        let mut chrome = PetChrome::default();
        chrome.begin(pane);
        chrome.note(28, 0, 1, 100);
        let _ = chrome.commit();
        let _ = chrome.held();
        chrome.begin(pane);
        chrome.note(28, 0, 1, 100);
        chrome.note_overlay(10, 2, 11, 40);
        assert!(
            chrome.commit(),
            "the menu opened under a pet that read the old sets"
        );
        assert_eq!(
            chrome.held(),
            [PetRect::new(27.0, 0.0, 1.0, 100.0)],
            "the chrome alone"
        );
        assert_eq!(chrome.held_overlays(), [PetRect::new(9.0, 2.0, 11.0, 40.0)]);
        chrome.begin(pane);
        chrome.note(28, 0, 1, 100);
        assert!(chrome.commit(), "the menu closed: owed again");
        assert!(chrome.held_overlays().is_empty());
    }

    /// A COMMIT WITH NO PASS BEGUN KEEPS WHAT IS HELD (the capture routes
    /// paint the menu, which commits, where no footer pass began): after a
    /// moved commit `next` holds the older set, and a bare commit that swapped
    /// it back would hand the pet the cells of two frames ago and owe the
    /// window a redraw for them.
    #[test]
    fn a_commit_with_no_pass_begun_keeps_the_held_set() {
        let pane = Some((0, 0, 30, 80));
        let (six, nine) = (
            [PetRect::new(28.0, 2.0, 1.0, 6.0)],
            [PetRect::new(28.0, 2.0, 1.0, 9.0)],
        );
        let mut chrome = PetChrome::default();
        chrome.begin(pane);
        chrome.note(28, 2, 1, 6);
        let _ = chrome.commit();
        assert_eq!(chrome.held(), six);
        chrome.begin(pane);
        chrome.note(28, 2, 1, 9);
        assert!(chrome.commit(), "the chips grew under a pet that read them");
        assert_eq!(chrome.held(), nine);
        assert!(!chrome.commit(), "no pass begun: nothing owed");
        assert_eq!(chrome.held(), nine, "no pass begun: the held set stands");
        chrome.begin(pane);
        chrome.note(28, 2, 1, 6);
        assert!(chrome.commit());
        assert_eq!(chrome.held(), six);
    }
}
