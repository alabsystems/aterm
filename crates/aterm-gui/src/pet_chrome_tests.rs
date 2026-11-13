// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
