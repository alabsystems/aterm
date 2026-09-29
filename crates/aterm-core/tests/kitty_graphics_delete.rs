// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kitty graphics `a=d` delete semantics against the real engine.
//!
//! The spec invariants, each of which the delete arm once violated
//! (`handler_actions.rs` `delete_kitty_placements`):
//!
//!   * a LOWERCASE selector deletes PLACEMENTS and KEEPS the transmitted
//!     data — the id stays placeable, which preview cyclers (yazi, icat)
//!     rely on; UPPERCASE also frees the data of every image it named or
//!     cleared that has no placement left on screen;
//!   * a selector addresses SPECIFIC placements — by image id (optionally one
//!     placement id), image number, id range, cell, cell + z-index, column,
//!     row or z-index — and is never license to clear the whole store, nor to
//!     erase an image another protocol drew.
//!
//! Placements are observed through `Terminal::images_row` — the same per-cell
//! image-extras surface the renderer reads, so "cleared" here is exactly
//! "erased on the next repaint". Whether the STORE still holds an image is
//! observed through a Unicode placeholder (`placeable`), which draws straight
//! from the store and moves nothing: no put, no cursor policy, no scroll.

use aterm_core::terminal::Terminal;

/// Columns each test image spans (`c=` cells).
const IMG_COLS: usize = 2;

/// Move the cursor to the 0-based cell `(row, col)`.
fn goto(term: &mut Terminal, row: u16, col: u16) {
    term.process(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
}

/// Transmit-and-display a tiny RGBA image under `id` (with `extra` control
/// keys), 2×2 source pixels shown as a `c=2,r=1` (two-cell, one-row) placement
/// at `(row, col)`.
fn show_at(term: &mut Terminal, id: u32, extra: &str, row: u16, col: u16) {
    goto(term, row, col);
    let raw = [255u8, 0, 0, 255].repeat(4); // 2x2 px RGBA
    let mut seq = format!("\x1b_Ga=T,f=32,s=2,v=2,i={id},c=2,r=1,q=2{extra};").into_bytes();
    seq.extend_from_slice(
        aterm_codec::base64::encode(&raw)
            .expect("encode")
            .as_bytes(),
    );
    seq.extend_from_slice(b"\x1b\\");
    term.process(&seq);
}

/// Put the stored image `id` (with `extra` control keys) at `(row, col)`.
fn put_at(term: &mut Terminal, id: u32, extra: &str, row: u16, col: u16) {
    goto(term, row, col);
    control(term, &format!("a=p,i={id},q=2{extra}"));
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

/// The scratch cell `placeable` prints its placeholder in: mid-screen, on a
/// row no test places an image on.
const PROBE: (u16, u16) = (4, 10);

/// Whether the store still holds `id`: a Unicode placeholder naming it (its
/// id in the foreground colour, tile (0, 0) in the diacritics) draws it. The
/// placeholder reads the store directly, so the probe neither places nor
/// deletes anything and cannot scroll; the cell is erased afterwards.
fn placeable(term: &mut Terminal, id: u32) -> bool {
    let (row, col) = PROBE;
    assert!(
        term.images_row(usize::from(row)).is_empty(),
        "the probe row is the probe's alone"
    );
    goto(term, row, col);
    let [_, r, g, b] = id.to_be_bytes();
    term.process(format!("\x1b[38;2;{r};{g};{b}m\u{10EEEE}\u{0305}\u{0305}\x1b[m").as_bytes());
    let drawn = !term.images_row(usize::from(row)).is_empty();
    goto(term, row, col);
    term.process(b"\x1b[X");
    assert!(
        term.images_row(usize::from(row)).is_empty(),
        "the probe erased its placeholder"
    );
    drawn
}

#[test]
fn delete_all_lowercase_clears_placements_and_keeps_the_store() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    assert_eq!(rows_with_images(&term, 8), vec![0], "placed at row 0");

    control(&mut term, "a=d,d=a");
    assert!(
        rows_with_images(&term, 8).is_empty(),
        "d=a cleared every visible placement"
    );
    assert!(
        placeable(&mut term, 1),
        "lowercase delete kept the transmitted data — a=p re-places id 1"
    );
}

#[test]
fn delete_all_uppercase_frees_the_store_too() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    control(&mut term, "a=d,d=A");
    assert!(rows_with_images(&term, 8).is_empty(), "placements cleared");
    assert!(
        !placeable(&mut term, 1),
        "uppercase delete freed the data — id 1 is no longer placeable"
    );
}

#[test]
fn delete_by_id_touches_only_that_image_and_keeps_its_data() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    show_at(&mut term, 2, "", 1, 0);
    assert_eq!(rows_with_images(&term, 8), vec![0, 1]);

    control(&mut term, "a=d,d=i,i=1");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![1],
        "only image 1's placement cleared; image 2 untouched"
    );
    assert!(
        placeable(&mut term, 1),
        "lowercase by-id delete kept image 1's data — it re-places"
    );
}

#[test]
fn delete_by_id_uppercase_frees_that_data_and_no_other() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    show_at(&mut term, 2, "", 1, 0);

    control(&mut term, "a=d,d=I,i=1");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![1],
        "image 1 gone from screen"
    );
    assert!(!placeable(&mut term, 1), "image 1's data freed");
    assert!(placeable(&mut term, 2), "image 2's data untouched");
}

/// `d=i` with a `p=` deletes ONE placement of the image; without it, every
/// placement of the image goes (the control).
#[test]
fn delete_by_id_and_placement_id_removes_only_that_placement() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, ",p=1", 0, 0);
    put_at(&mut term, 1, ",p=2", 2, 0);
    assert_eq!(rows_with_images(&term, 8), vec![0, 2]);

    control(&mut term, "a=d,d=I,i=1,p=1");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![2],
        "only placement (1, 1) was deleted"
    );
    assert!(
        placeable(&mut term, 1),
        "uppercase with p= keeps data another placement still shows"
    );

    control(&mut term, "a=d,d=i,i=1");
    assert!(
        rows_with_images(&term, 8).is_empty(),
        "control: without p= every placement of the image goes"
    );
}

/// A put that names an existing `(i, p)` MOVES that placement; unnamed puts
/// each add a placement (the control).
#[test]
fn put_with_an_existing_placement_id_moves_it() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, ",p=7", 0, 0);
    put_at(&mut term, 1, ",p=7", 3, 0);
    assert_eq!(
        rows_with_images(&term, 8),
        vec![3],
        "the second put of (1, 7) moved it from row 0 to row 3"
    );

    put_at(&mut term, 1, "", 5, 0);
    put_at(&mut term, 1, "", 6, 0);
    assert_eq!(
        rows_with_images(&term, 8),
        vec![3, 5, 6],
        "control: unnamed puts add placements"
    );
}

/// `I=` numbers: the terminal assigns the id, and `d=n` addresses the newest
/// image under the number.
#[test]
fn delete_by_number_addresses_the_newest_image_with_that_number() {
    let mut term = Terminal::new(8, 20);
    let raw = [0u8, 255, 0, 255].repeat(4);
    let b64 = aterm_codec::base64::encode(&raw).expect("encode");
    term.process(format!("\x1b_Ga=t,f=32,s=2,v=2,I=42,q=2;{b64}\x1b\\").as_bytes());
    goto(&mut term, 1, 0);
    control(&mut term, "a=p,I=42,c=2,r=1,q=2");
    assert_eq!(rows_with_images(&term, 8), vec![1], "a=p by number places");

    control(&mut term, "a=d,d=n,I=41");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![1],
        "another number deletes nothing"
    );
    control(&mut term, "a=d,d=N,I=42");
    assert!(rows_with_images(&term, 8).is_empty(), "d=N cleared it");
    goto(&mut term, 1, 0);
    control(&mut term, "a=p,I=42,c=2,r=1,q=2");
    assert!(
        rows_with_images(&term, 8).is_empty(),
        "d=N freed the data: the number no longer resolves"
    );
}

#[test]
fn delete_at_point_clears_only_the_placement_covering_that_cell() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    show_at(&mut term, 2, "", 2, 4);

    control(&mut term, "a=d,d=p,x=1,y=2");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0, 2],
        "an empty cell addresses nothing"
    );
    // (x=6, y=3) is the second column of image 2's placement.
    control(&mut term, "a=d,d=p,x=6,y=3");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0],
        "the placement covering (col 5, row 2) is gone, in full"
    );
    assert!(placeable(&mut term, 2), "lowercase kept image 2's data");
}

#[test]
fn uppercase_point_delete_frees_data_only_when_no_placement_remains() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    put_at(&mut term, 1, "", 3, 0);

    control(&mut term, "a=d,d=P,x=1,y=1");
    assert_eq!(rows_with_images(&term, 8), vec![3]);
    assert!(
        placeable(&mut term, 1),
        "another placement of image 1 is on screen: its data stays"
    );
    control(&mut term, "a=d,d=P,x=1,y=4");
    assert!(rows_with_images(&term, 8).is_empty());
    assert!(
        !placeable(&mut term, 1),
        "the last placement went: its data went with it"
    );
}

#[test]
fn delete_at_point_with_z_index_needs_the_z_to_match() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, ",z=-1", 0, 0);

    control(&mut term, "a=d,d=q,x=1,y=1");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0],
        "d=q defaults to z=0, which this placement is not"
    );
    control(&mut term, "a=d,d=q,x=1,y=1,z=-1");
    assert!(
        rows_with_images(&term, 8).is_empty(),
        "cell and z-index both match"
    );
}

#[test]
fn delete_by_column_and_by_row() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0); // cols 0-1, row 0
    show_at(&mut term, 2, "", 2, 5); // cols 5-6, row 2

    control(&mut term, "a=d,d=x,x=7");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0],
        "column 7 (1-based) crosses only image 2"
    );
    control(&mut term, "a=d,d=y,y=2");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0],
        "row 2 (1-based) is empty now"
    );
    control(&mut term, "a=d,d=y,y=1");
    assert!(rows_with_images(&term, 8).is_empty(), "row 1 held image 1");
}

#[test]
fn delete_by_z_index() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    show_at(&mut term, 2, ",z=-1", 2, 0);

    control(&mut term, "a=d,d=z,z=-1");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0],
        "only the z=-1 placement"
    );
    control(&mut term, "a=d,d=z");
    assert!(rows_with_images(&term, 8).is_empty(), "z defaults to 0");
}

#[test]
fn delete_by_id_range() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 3, "", 0, 0);
    show_at(&mut term, 4, "", 1, 0);
    show_at(&mut term, 9, "", 2, 0);

    control(&mut term, "a=d,d=R,x=3,y=4");
    assert_eq!(rows_with_images(&term, 8), vec![2], "ids 3..=4 cleared");
    assert!(!placeable(&mut term, 3) && !placeable(&mut term, 4));
    assert!(placeable(&mut term, 9), "id 9 is outside the range");
}

#[test]
fn delete_at_cursor_clears_the_covering_placement_in_full() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    goto(&mut term, 0, 1); // the image's second cell
    control(&mut term, "a=d,d=c");
    assert!(
        rows_with_images(&term, 8).is_empty(),
        "d=c cleared the placement covering the cursor cell"
    );
    assert!(placeable(&mut term, 1), "d=c kept the transmitted data");
}

#[test]
fn delete_at_cursor_over_empty_cell_is_a_no_op() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    // Kitty leaves the cursor just past the image, on an empty cell.
    control(&mut term, "a=d,d=c");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0],
        "no placement under the cursor — nothing cleared"
    );
}

/// A Kitty delete never erases an image another protocol drew: `d=a` clears
/// the Kitty placement and leaves the iTerm2 one.
#[test]
fn kitty_delete_never_erases_another_protocols_image() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    goto(&mut term, 3, 0);
    let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
    let b64 = aterm_codec::base64::encode(&png).expect("encode");
    term.process(format!("\x1b]1337;File=inline=1;width=2;height=1:{b64}\x1b\\").as_bytes());
    assert_eq!(rows_with_images(&term, 8), vec![0, 3]);

    control(&mut term, "a=d,d=A");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![3],
        "the iTerm2 image is not a Kitty delete's to erase"
    );
}

/// A selector missing its image or its coordinates — or naming a cell no
/// screen has, up to the largest `u32` — addresses nothing: all delete NOTHING
/// (recoverable), and none panics, where the old engine answered every
/// unknown selector by destroying the store.
#[test]
fn unaddressed_selectors_delete_nothing() {
    for selector in [
        "d=f",
        "d=F",
        "d=F,i=2",
        "d=n",
        "d=p",
        "d=q",
        "d=x",
        "d=y",
        "d=r,x=5",
        "d=p,x=65536,y=65536",
        "d=q,x=4294967295,y=4294967295",
        "d=x,x=65536",
        "d=y,y=4294967295",
    ] {
        let mut term = Terminal::new(8, 20);
        show_at(&mut term, 1, "", 0, 0);
        control(&mut term, &format!("a=d,{selector}"));
        assert_eq!(
            term.images_row(0).len(),
            IMG_COLS,
            "{selector}: the placement is untouched"
        );
        assert!(
            placeable(&mut term, 1),
            "{selector}: the store survived too"
        );
    }
}

/// `d=f` on an image with a single frame keeps it; `d=F` deletes the image
/// outright — its placements and its data — as kitty does.
#[test]
fn frame_delete_of_a_single_frame_image() {
    let mut term = Terminal::new(8, 20);
    show_at(&mut term, 1, "", 0, 0);
    show_at(&mut term, 2, "", 1, 0);

    control(&mut term, "a=d,d=f,i=1");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0, 1],
        "d=f: a lone frame stays"
    );
    assert!(placeable(&mut term, 1), "d=f kept image 1's data");

    control(&mut term, "a=d,d=F,i=1");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![1],
        "d=F on a lone frame deleted image 1's placement"
    );
    assert!(!placeable(&mut term, 1), "and its data");
    assert!(placeable(&mut term, 2), "image 2 is not touched");
}
