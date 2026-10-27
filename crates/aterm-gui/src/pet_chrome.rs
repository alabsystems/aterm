// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S CHROME ON THE GRID, AS THE RESIDENT PET SEES IT. aterm paints some of its own chrome INTO grid cells after the pet has looked at the frame: the footer's one write (`claude_footer`: Claude Code's rule — its chips and facts — the harness card's line, and the Codex chips, which sit on the BLANK row Codex keeps over its status row) and the Autopilot menu. Where that write rewrites a row of the program's (Claude Code's mode row), the words it lays are chrome and the rest of the row it blanks is glass ([`PetChrome::note_blanked`]): the pet, which read the program's cells, reads the program's letters under that blank as the nothing the person sees.

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

    /// The transient overlays the pet takes as occupied, marked read the same way: the Autopilot menu's card ([`PetChrome::note_overlay`]).
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

    /// As [`PetChrome::note`], for a TRANSIENT overlay (the Autopilot menu's card): protected
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

    /// THE BLANKED GLASS IS HELD APART FROM THE CHROME: a rewrite's blank tail noted as blanked
    /// is never in [`PetChrome::held`] (ground the pet may stand on, not the host's ink), it is
    /// kept pane-local and clipped like a write, and a pass where only it moves owes a pet that
    /// read the old sets a frame.
    #[test]
    fn blanked_glass_is_held_apart_from_the_chrome_and_its_move_owes_a_frame() {
        let pane = Some((1, 10, 30, 100));
        let mut chrome = PetChrome::default();
        chrome.begin(pane);
        chrome.note(29, 10, 1, 17);
        chrome.note_blanked(29, 27, 60);
        chrome.note_blanked(30, 10, 200);
        let _ = chrome.commit();
        assert_eq!(chrome.held(), [PetRect::new(28.0, 0.0, 1.0, 17.0)]);
        assert_eq!(
            chrome.held_blanked(),
            [
                PetRect::new(28.0, 17.0, 1.0, 60.0),
                PetRect::new(29.0, 0.0, 1.0, 100.0)
            ]
        );
        chrome.begin(pane);
        chrome.note(29, 10, 1, 17);
        chrome.note_blanked(29, 27, 60);
        chrome.note_blanked(30, 10, 200);
        assert!(!chrome.commit(), "a still pass owes nothing");
        chrome.begin(pane);
        chrome.note(29, 10, 1, 17);
        chrome.note_blanked(29, 30, 57);
        chrome.note_blanked(30, 10, 200);
        assert!(chrome.commit(), "the blank moved under a pet that read it");
        assert_eq!(chrome.held()[0], PetRect::new(28.0, 0.0, 1.0, 17.0));
        assert_eq!(
            chrome.held_blanked()[0],
            PetRect::new(28.0, 20.0, 1.0, 57.0)
        );
        chrome.begin(None);
        chrome.note_blanked(29, 27, 60);
        assert!(chrome.commit());
        assert!(
            chrome.held_blanked().is_empty(),
            "no focused pane: nothing noted"
        );
    }

    /// THE HELD CHROME NAMES THE FRAME IT WAS PAINTED OVER: the pet reads it over the next
    /// frame and carries it with the rows it was painted on where they moved since
    /// (`aterm_effects::pet_world::PetReserve::painted_over`). A pass that paints the same cells
    /// over a newer frame names the newer frame; one that names none leaves none; a commit with
    /// no pass begun keeps the frame with the cells.
    #[test]
    fn the_held_chrome_names_the_frame_it_was_painted_over() {
        let pane = Some((0, 0, 30, 80));
        let mut chrome = PetChrome::default();
        assert_eq!(chrome.held_painted_over(), None);
        chrome.begin(pane);
        chrome.painted_over(7);
        chrome.note(28, 2, 1, 6);
        let _ = chrome.commit();
        assert_eq!(chrome.held_painted_over(), Some(7));
        chrome.begin(pane);
        chrome.painted_over(8);
        chrome.note(28, 2, 1, 6);
        assert!(!chrome.commit(), "the same cells: nothing owed");
        assert_eq!(chrome.held_painted_over(), Some(8), "over the newer frame");
        assert!(!chrome.commit());
        assert_eq!(chrome.held_painted_over(), Some(8), "no pass begun");
        chrome.begin(pane);
        chrome.note(28, 2, 1, 6);
        let _ = chrome.commit();
        assert_eq!(
            chrome.held_painted_over(),
            None,
            "a pass that named no frame"
        );
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
