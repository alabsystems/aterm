// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Hidden cursor custody must retain a freshly observed resident home even
//! when the pane exceeds the bounded world map.
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetSense};
use aterm_effects::pet_world::{PetPane, PetWorldFacts};
use std::time::{Duration, Instant};

fn tick(
    term: &mut Terminal,
    brain: &mut PetBrain,
    now: &mut Instant,
    rows: u16,
    cols: u16,
) -> PetFrame {
    tick_with_surface(term, brain, now, rows, cols, false)
}

fn tick_with_surface(
    term: &mut Terminal,
    brain: &mut PetBrain,
    now: &mut Instant,
    rows: u16,
    cols: u16,
    incoherent: bool,
) -> PetFrame {
    *now += Duration::from_millis(16);
    let mut input = term.cell_frame(usize::from(rows), usize::from(cols));
    if incoherent {
        // A snapshot extracted before a scroll the facts already saw: the
        // observation refuses it. (A sub-row scroll offset is coherent — the
        // renderer translates the pet's sprite with the grid band.)
        input.base_y += 1;
    }
    let facts = PetWorldFacts::read(term, 7);
    brain.observe_console(&input, &facts, PetPane::full(&input));
    brain.set_console_presentable(true);
    brain.tick(PetSense {
        caret_drawn: true,
        now: *now,
        caret: input
            .cursor_visible
            .then_some((input.cursor_row as u16, input.cursor_col as u16)),
        wrapped: false,
        rows,
        cols,
        cell_w: 8,
        cell_h: 16,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
    })
}

fn check_hidden(rows: u16, cols: u16, caret: (u16, u16)) {
    let mut term = Terminal::new(rows, cols);
    term.process(format!("\x1b]11;#111318\x07\x1b[{};{}H", caret.0 + 1, caret.1 + 1).as_bytes());
    let mut brain = PetBrain::default();
    let mut now = Instant::now();
    let mut shown = tick(&mut term, &mut brain, &mut now, rows, cols);
    for _ in 0..64 {
        shown = tick(&mut term, &mut brain, &mut now, rows, cols);
    }
    assert_eq!(shown.alpha, 255, "must first materialize at {rows}x{cols}");
    term.process(b"\x1b[?25l");
    let hidden = tick(&mut term, &mut brain, &mut now, rows, cols);
    assert!(
        hidden.alpha > 0,
        "hidden-caret resident lost on {rows}x{cols}: {}",
        brain.console_reason()
    );
    assert!(
        (hidden.col - shown.col).abs() < 1.0 && (hidden.row - shown.row).abs() < 1.0,
        "{rows}x{cols}: hiding the caret must hold its existing home"
    );
    // Losing one observation must not lose the remembered sampling center.
    let unknown = tick_with_surface(&mut term, &mut brain, &mut now, rows, cols, true);
    assert_eq!(
        unknown.alpha, hidden.alpha,
        "{rows}x{cols}: an unknown frame holds the body on glass rather than blinking it"
    );
    assert!((unknown.col - hidden.col).abs() < 0.01 && (unknown.row - hidden.row).abs() < 0.01);
    let recovered = tick(&mut term, &mut brain, &mut now, rows, cols);
    assert_eq!(recovered.alpha, hidden.alpha);
    assert!((recovered.col - hidden.col).abs() < 0.01 && (recovered.row - hidden.row).abs() < 0.01);
    assert_eq!(brain.console_input_seq(), 0);

    // The hint re-samples current cells; it cannot preserve old permission
    // after the user selects the entire surface.
    use aterm_core::selection::{SelectionSide, SelectionType};
    term.text_selection_mut()
        .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    term.text_selection_mut()
        .update_selection(i32::from(rows) - 1, cols - 1, SelectionSide::Right);
    assert_eq!(tick(&mut term, &mut brain, &mut now, rows, cols).alpha, 0);
}

/// A pane inside the bounded world map (the control), wider, taller, and
/// both.
#[test]
fn a_hidden_caret_keeps_the_resident_home_at_every_pane_size() {
    for (rows, cols, caret) in [
        (58, 116, (55, 2)),
        (58, 600, (55, 2)),
        (200, 116, (190, 2)),
        (200, 600, (190, 10)),
    ] {
        check_hidden(rows, cols, caret);
    }
}
