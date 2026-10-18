// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S CHROME ON THE GRID, AS THE RESIDENT PET SEES IT. aterm paints
//! some of its own chrome INTO grid cells after the pet has looked at the
//! frame: the footer's one write (`claude_footer`: Claude Code's rule — its
//! chips and facts — the harness card's line, and the Codex chips, which sit
//! on the BLANK row Codex keeps over its status row) and the Autopilot menu.
//! The pet judges a seat by the grid it observed (`aterm_effects::pet_world`),
//! so on that blank Codex row the chips looked like empty ground and the cat
//! sat on them, hiding the timer chip (the Together run, 2026-10-16:
//! `∞ ⤓[cat]36`).
//!
//! So every cell those splices write over the FOCUSED pane is recorded here,
//! pane-local, and the next observation takes them as exclusions
//! (`app_render::pet_console_exclusions`): protected, like a selection or the
//! find bar — the pet never rests on them and seats itself beside them.
//!
//! * ONE FRAME LATE, NEVER LEFT STALE. The splices run after the pet's tick,
//!   so a frame's pet reads the cells the previous frame painted ([`held`]).
//!   Where a pass paints a different set than the one its pet read, the
//!   window is owed one more frame ([`PetChrome::commit`]), whose pet reads
//!   the new set — so chips that appear, grow or move are never sat on for
//!   longer than that frame, and nothing at rest differs: a still frame
//!   paints what it painted before and owes nothing.
//! * AT REST, NOTHING. Collecting is a clear and a few pushes into buffers
//!   kept for their capacity, made only on a frame that runs anyway; no
//!   clock, no lock, no wake.
//! * ONLY A PASS ENDS A PASS. A route that paints the menu with no footer
//!   pass before it (a capture whose layout is not the window's, where
//!   `splice_claude_footer` does not run) began nothing, so its commit keeps
//!   the held set as it stands and owes nothing.
//!
//! [`held`]: PetChrome::held

use aterm_effects::pet_world::PetRect;

/// The cells the host's chrome splices painted over the focused pane, for
/// the resident pet (module docs). Held on the window
/// (`WindowState::pet_chrome`).
#[derive(Debug, Default)]
pub(crate) struct PetChrome {
    /// What the pet reads: the cells the last finished pass painted,
    /// pane-local.
    held: Vec<PetRect>,
    /// This pass's, being gathered; swapped into `held` at its commit.
    next: Vec<PetRect>,
    /// The focused pane of this pass in frame cells `(row, col, rows, cols)`:
    /// a write outside it is another pane's, which the pet does not live in.
    pane: Option<(usize, usize, usize, usize)>,
    /// Whether a pet observation read `held` since the last commit.
    read: std::cell::Cell<bool>,
    /// Whether [`PetChrome::begin`] opened the pass a commit would end.
    begun: bool,
}

impl PetChrome {
    /// The cells the pet takes as occupied, marked read: the pass that
    /// moves them owes the window the frame that shows them to the pet.
    pub(crate) fn held(&self) -> &[PetRect] {
        self.read.set(true);
        &self.held
    }

    /// Start a pass over the focused pane at `pane` (frame cells `(row, col,
    /// rows, cols)`; `None`: no focused terminal pane, so nothing is noted).
    pub(crate) fn begin(&mut self, pane: Option<(usize, usize, usize, usize)>) {
        self.next.clear();
        self.pane = pane;
        self.begun = true;
    }

    /// A splice painted `cols` cells from frame cell `(row, col)` down `rows`
    /// rows: the part inside the focused pane, pane-local.
    pub(crate) fn note(&mut self, row: usize, col: usize, rows: usize, cols: usize) {
        let Some((pr, pc, prows, pcols)) = self.pane else {
            return;
        };
        let (r0, r1) = (row.max(pr), (row + rows).min(pr + prows));
        let (c0, c1) = (col.max(pc), (col + cols).min(pc + pcols));
        if r0 >= r1 || c0 >= c1 {
            return;
        }
        self.next.push(PetRect::new(
            (r0 - pr) as f32,
            (c0 - pc) as f32,
            (r1 - r0) as f32,
            (c1 - c0) as f32,
        ));
    }

    /// End the pass: what it gathered becomes what the pet reads. `true`
    /// when that moved and a pet read the old set since the last commit —
    /// the window owes the pet one more frame. With no pass begun since the
    /// last commit, nothing: `next` then holds the set before last (the
    /// swap's), which is no pass's.
    pub(crate) fn commit(&mut self) -> bool {
        if !std::mem::take(&mut self.begun) {
            return false;
        }
        let moved = self.next != self.held;
        if moved {
            std::mem::swap(&mut self.next, &mut self.held);
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
    ///
    /// NEGATIVE CONTROL: the same passes with no pet reading owe nothing.
    #[test]
    fn writes_are_pane_local_and_a_moved_set_owes_the_pet_a_frame() {
        let mut chrome = PetChrome::default();
        // The focused pane: frame rows 1.. (a strip row above), columns 73..144.
        let pane = Some((1, 73, 49, 71));
        chrome.begin(pane);
        chrome.note(47, 74, 1, 6); // the Codex chips, frame cells
        chrome.note(30, 2, 1, 10); // the sibling pane's rule
        chrome.note(48, 140, 1, 10); // clipped at the pane's right edge
        assert!(!chrome.commit(), "no pet read anything yet");
        assert_eq!(
            chrome.held(),
            [
                PetRect::new(46.0, 1.0, 1.0, 6.0),
                PetRect::new(47.0, 67.0, 1.0, 4.0)
            ]
        );
        // At rest: the same cells again owe nothing.
        chrome.begin(pane);
        chrome.note(47, 74, 1, 6);
        chrome.note(48, 140, 1, 10);
        assert!(!chrome.commit(), "a still pass owes nothing");
        // The chips grow (a timer): the pet read the old set, so a frame is owed,
        // once.
        let _ = chrome.held();
        chrome.begin(pane);
        chrome.note(47, 74, 1, 9);
        assert!(chrome.commit(), "moved cells owe the pet a frame");
        assert_eq!(chrome.held(), [PetRect::new(46.0, 1.0, 1.0, 9.0)]);
        chrome.begin(pane);
        chrome.note(47, 74, 1, 9);
        assert!(!chrome.commit(), "and only one");
        // No focused pane: nothing is noted, and the cleared set moves.
        chrome.begin(None);
        chrome.note(47, 74, 1, 9);
        assert!(chrome.commit());
        assert!(chrome.held().is_empty());
        // Negative control: moved, but unread.
        let mut quiet = PetChrome::default();
        quiet.begin(pane);
        quiet.note(47, 74, 1, 6);
        assert!(!quiet.commit(), "no pet read: nothing owed");
    }

    /// A COMMIT WITH NO PASS BEGUN KEEPS WHAT IS HELD (the capture routes
    /// paint the menu, which commits, where no footer pass began): after a
    /// moved commit `next` holds the older set, and a bare commit that swapped
    /// it back would hand the pet the cells of two frames ago and owe the
    /// window a redraw for them.
    ///
    /// NEGATIVE CONTROL: the same commit after a `begin` that notes the
    /// older set does move the held set back, and owes the frame.
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
        // Negative control: a real pass that paints the older cells again.
        chrome.begin(pane);
        chrome.note(28, 2, 1, 6);
        assert!(chrome.commit());
        assert_eq!(chrome.held(), six);
    }
}
