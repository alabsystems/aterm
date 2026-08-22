// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kitty graphics `a=d` delete semantics against the real engine.
//!
//! Two spec invariants, each of which the delete arm violated before this
//! suite existed (`handler_actions.rs handle_complete_kitty_command`):
//!
//!   * a LOWERCASE selector deletes PLACEMENTS and KEEPS the transmitted
//!     data — the id stays placeable, which preview cyclers (yazi, icat)
//!     rely on; UPPERCASE also frees the data;
//!   * a selector addresses SPECIFIC placements — it is never license to
//!     clear the whole store. (Previously every selector except `i`/`I`
//!     nuked the entire store, and NO selector ever cleared a placed cell,
//!     so the "deleted" image stayed visible while its data vanished.)
//!
//! Placements are observed through `Terminal::images_row` — the same per-cell
//! image-extras surface the renderer reads, so "cleared" here is exactly
//! "erased on the next repaint".

use aterm_core::terminal::Terminal;

/// Columns each test image spans (`c=` cells).
const IMG_COLS: usize = 2;

/// Transmit-and-display a tiny RGBA image under `id`, 2×2 source pixels shown
/// as a `c=2,r=1` (two-cell, one-row) placement at the current cursor.
fn transmit_and_display(term: &mut Terminal, id: u32) {
    let raw = [255u8, 0, 0, 255].repeat(4); // 2x2 px RGBA
    let mut seq = format!("\x1b_Ga=T,f=32,s=2,v=2,i={id},c=2,r=1;").into_bytes();
    seq.extend_from_slice(
        aterm_codec::base64::encode(&raw)
            .expect("encode")
            .as_bytes(),
    );
    seq.extend_from_slice(b"\x1b\\");
    term.process(&seq);
}

/// Issue a bare control-only kitty command (no payload), e.g. `a=d,d=a`.
fn control(term: &mut Terminal, body: &str) {
    term.process(format!("\x1b_G{body}\x1b\\").as_bytes());
}

/// The visible rows that carry at least one image cell.
fn rows_with_images(term: &Terminal, rows: usize) -> Vec<usize> {
    (0..rows)
        .filter(|&r| !term.images_row(r).is_empty())
        .collect()
}

#[test]
fn delete_all_lowercase_clears_placements_and_keeps_the_store() {
    let mut term = Terminal::new(8, 20);
    transmit_and_display(&mut term, 1);
    assert_eq!(rows_with_images(&term, 8), vec![0], "placed at row 0");

    control(&mut term, "a=d,d=a");
    assert!(
        rows_with_images(&term, 8).is_empty(),
        "d=a cleared every visible placement"
    );

    // The data survived: the id places again from the store.
    control(&mut term, "a=p,i=1");
    assert!(
        !rows_with_images(&term, 8).is_empty(),
        "lowercase delete kept the transmitted data — a=p re-places id 1"
    );
}

#[test]
fn delete_all_uppercase_frees_the_store_too() {
    let mut term = Terminal::new(8, 20);
    transmit_and_display(&mut term, 1);
    control(&mut term, "a=d,d=A");
    assert!(rows_with_images(&term, 8).is_empty(), "placements cleared");

    control(&mut term, "a=p,i=1");
    assert!(
        rows_with_images(&term, 8).is_empty(),
        "uppercase delete freed the data — id 1 is no longer placeable"
    );
}

#[test]
fn delete_by_id_touches_only_that_image_and_keeps_its_data() {
    let mut term = Terminal::new(8, 20);
    transmit_and_display(&mut term, 1); // row 0
    transmit_and_display(&mut term, 2); // row 1 (cursor advanced below image 1)
    assert_eq!(rows_with_images(&term, 8), vec![0, 1]);

    control(&mut term, "a=d,d=i,i=1");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![1],
        "only image 1's placement cleared; image 2 untouched"
    );

    control(&mut term, "a=p,i=1");
    assert_eq!(
        rows_with_images(&term, 8).len(),
        2,
        "lowercase by-id delete kept image 1's data — it re-places"
    );
}

#[test]
fn delete_by_id_uppercase_frees_that_data_and_no_other() {
    let mut term = Terminal::new(8, 20);
    transmit_and_display(&mut term, 1);
    transmit_and_display(&mut term, 2);

    control(&mut term, "a=d,d=I,i=1");
    assert_eq!(rows_with_images(&term, 8), vec![1], "image 1 gone from screen");

    control(&mut term, "a=p,i=1");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![1],
        "image 1's data freed — a=p places nothing"
    );
    control(&mut term, "a=p,i=2");
    assert_eq!(
        rows_with_images(&term, 8).len(),
        2,
        "image 2's data untouched — it still places"
    );
}

#[test]
fn delete_at_cursor_clears_the_covering_placement_in_full() {
    let mut term = Terminal::new(8, 20);
    transmit_and_display(&mut term, 1);
    // The cursor advanced below the image; move it back ONTO the image row.
    term.process(b"\x1b[1;1H");
    control(&mut term, "a=d,d=c");
    assert!(
        rows_with_images(&term, 8).is_empty(),
        "d=c cleared the placement covering the cursor cell"
    );
    // Lowercase: the data survives.
    control(&mut term, "a=p,i=1");
    assert!(
        !rows_with_images(&term, 8).is_empty(),
        "d=c kept the transmitted data"
    );
}

#[test]
fn delete_at_cursor_over_empty_cell_is_a_no_op() {
    let mut term = Terminal::new(8, 20);
    transmit_and_display(&mut term, 1);
    // Cursor already sits BELOW the image after placement.
    control(&mut term, "a=d,d=c");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0],
        "no placement under the cursor — nothing cleared"
    );
}

/// The selectors this engine cannot address yet (by point, number, placement
/// id, column/row/z) must delete NOTHING — deleting nothing is recoverable;
/// the old behavior answered every one of them by destroying the whole store.
#[test]
fn unsupported_selectors_delete_nothing() {
    for selector in ["d=n", "d=p", "d=q", "d=x", "d=y", "d=z"] {
        let mut term = Terminal::new(8, 20);
        transmit_and_display(&mut term, 1);
        control(&mut term, &format!("a=d,{selector}"));
        assert_eq!(
            rows_with_images(&term, 8),
            vec![0],
            "{selector}: unsupported selector left the placement alone"
        );
        control(&mut term, "a=p,i=1");
        assert_eq!(
            term.images_row(0).len(),
            IMG_COLS,
            "{selector}: the store survived too (row 0 still fully placed)"
        );
    }
}
