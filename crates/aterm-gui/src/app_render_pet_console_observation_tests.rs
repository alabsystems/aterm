// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::pet_console_exclusions;
use aterm_core::terminal::Terminal;
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};

#[test]
fn late_native_overlays_reserve_real_cells_before_pet_planning() {
    let mut term = Terminal::new(20, 60);
    term.process(b"\x1b]11;#000000\x07\x1b[6;20H");
    let input = term.cell_frame(20, 60);
    let facts = PetWorldFacts::read(&term, 7);
    let chrome = [PetRect::new(15.0, 30.0, 1.0, 5.0)];
    let menu = [PetRect::new(10.0, 50.0, 2.0, 6.0)];
    let exclusions =
        pet_console_exclusions(&input, true, Some((7, 3, 9, 12)), true, &chrome, &menu, &[]);
    assert_eq!(exclusions.ink().len(), 2, "the preedit row and the guesses");
    assert_eq!(
        exclusions.overlays().len(),
        3,
        "two find bands and the menu"
    );
    assert_eq!(exclusions.chrome(), &chrome[..]);
    let mut world = PetWorld::default();
    assert!(world.observe_with_reserve(
        &input,
        &facts,
        PetPane::full(&input),
        exclusions.reserve(),
    ));
    for (row, col) in [
        (5.0, 40.0),
        (7.0, 6.0),
        (0.0, 40.0),
        (19.0, 40.0),
        (15.0, 31.0),
        (10.0, 52.0),
    ] {
        assert!(!world.clear(PetRect::new(row, col, 1.0, 1.0), 0.0));
    }
    assert!(world.clear(PetRect::new(12.0, 40.0, 2.0, 6.0), 0.2));
    for (row, col) in [(5.0, 40.0), (7.0, 6.0), (8.0, 11.0)] {
        let cell = PetRect::new(row, col, 1.0, 1.0);
        assert!(
            world.under_text_clear(cell, 0.0),
            "typed ink at ({row},{col})"
        );
        assert_eq!(world.ink_clearance(cell, 0.0), Some(false), "drawn under");
    }
    for (row, col) in [(0.0, 40.0), (19.0, 40.0), (15.0, 31.0), (10.0, 52.0)] {
        let cell = PetRect::new(row, col, 1.0, 1.0);
        assert!(
            !world.under_text_clear(cell, 0.0),
            "protected at ({row},{col})"
        );
    }
    let none = pet_console_exclusions(&input, false, None, false, &[], &[], &[]);
    assert!(
        none.overlays().is_empty()
            && none.chrome().is_empty()
            && none.ink().is_empty()
            && none.blanked().is_empty()
    );

    let mut term = Terminal::new(10, 40);
    term.process("line\r\n".repeat(15).as_bytes());
    term.scroll_display(3);
    let input = term.cell_frame(10, 40);
    assert_ne!(input.display_offset, 0);
    let blank = [PetRect::new(2.0, 0.0, 1.0, 10.0)];
    let history = pet_console_exclusions(&input, true, Some((3, 4, 4, 5)), false, &[], &[], &blank);
    assert!(
        history.overlays().is_empty()
            && history.chrome().is_empty()
            && history.ink().is_empty()
            && history.blanked().is_empty()
    );
}

/// THE GLASS THE HOST BLANKS IS THE BLANK THE PERSON SEES: the program's letters under a
/// blanked span ([`aterm_effects::pet_world::PetReserve::blanked`], the tail of Claude Code's
/// mode row the footer rewrites) are no ink to the pet, while its letters past the span and
/// the host's words over the row's start ([`aterm_effects::pet_world::PetReserve::protected`])
/// stay what they were.
#[test]
fn blanked_glass_reads_as_the_blank_the_person_sees() {
    let mut term = Terminal::new(20, 80);
    let letters = "x".repeat(70);
    term.process(format!("\x1b]11;#000000\x07\x1b[16;1H{letters}\x1b[3;1H").as_bytes());
    let input = term.cell_frame(20, 80);
    let facts = PetWorldFacts::read(&term, 7);
    let words = [PetRect::new(15.0, 0.0, 1.0, 17.0)];
    let blank = [PetRect::new(15.0, 17.0, 1.0, 43.0)];
    let seen = |blanked: &[PetRect]| {
        let exclusions = pet_console_exclusions(&input, false, None, false, &words, &[], blanked);
        assert_eq!(exclusions.blanked(), blanked);
        let mut world = PetWorld::default();
        assert!(world.observe_with_reserve(
            &input,
            &facts,
            PetPane::full(&input),
            exclusions.reserve(),
        ));
        world
    };
    let (world, unblanked) = (seen(&blank), seen(&[]));
    let tail = PetRect::new(15.0, 30.0, 1.0, 6.0);
    assert!(world.clear(tail, 0.0), "the blanked tail is free glass");
    assert!(
        !unblanked.clear(tail, 0.0) && unblanked.under_text_clear(tail, 0.0),
        "CONTROL: unblanked, the letters there are ink"
    );
    for (what, cell) in [
        ("the host's words", PetRect::new(15.0, 10.0, 1.0, 1.0)),
        (
            "the program's letters past the blank",
            PetRect::new(15.0, 65.0, 1.0, 1.0),
        ),
    ] {
        assert_eq!(
            (world.clear(cell, 0.0), world.under_text_clear(cell, 0.0)),
            (
                unblanked.clear(cell, 0.0),
                unblanked.under_text_clear(cell, 0.0)
            ),
            "{what} read as they did"
        );
        assert!(!world.clear(cell, 0.0), "{what} are not free glass");
    }
}
