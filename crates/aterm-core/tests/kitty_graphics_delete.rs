// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kitty graphics `a=d` delete semantics tests.

use aterm_core::terminal::Terminal;

const IMG_COLS: usize = 2;

fn goto(term: &mut Terminal, row: u16, col: u16) {
    term.process(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
}

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

fn put_at(term: &mut Terminal, id: u32, extra: &str, row: u16, col: u16) {
    goto(term, row, col);
    control(term, &format!("a=p,i={id},q=2{extra}"));
}

fn control(term: &mut Terminal, body: &str) {
    term.process(format!("\x1b_G{body}\x1b\\").as_bytes());
}

fn rows_with_images(term: &Terminal, rows: usize) -> Vec<usize> {
    (0..rows)
        .filter(|&r| !term.images_row(r).is_empty())
        .collect()
}

const PROBE: (u16, u16) = (4, 10);

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
    control(&mut term, "a=d,d=c");
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0],
        "no placement under the cursor — nothing cleared"
    );
}

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

fn history_placements(term: &Terminal) -> Vec<(u32, u32)> {
    let mut placements = Vec::new();
    for line in term.grid().history_lines_from(0).flatten() {
        for span in line.images().unwrap_or_default() {
            if let Some(tag) = span.kitty {
                let named = (tag.image_id, tag.placement_id);
                if !placements.contains(&named) {
                    placements.push(named);
                }
            }
        }
    }
    placements.sort_unstable();
    placements
}

fn history_fixture(tier: &str) -> Terminal {
    let ring = if tier == "ring" { 128 } else { 0 };
    let mut term = Terminal::with_scrollback(
        8,
        20,
        ring,
        aterm_core::scrollback::Scrollback::new(128, 128, 8_000_000),
    );
    term.set_compress_offload_active(true);
    show_at(&mut term, 1, ",p=7", 0, 0);
    show_at(&mut term, 2, ",p=8", 1, 0);
    goto(&mut term, 7, 0);
    for _ in 0..20 {
        term.process(b"text\r\n");
    }
    if tier == "hot" {
        assert!(term.lazy_backlog_len() > 0, "history starts staged");
        assert_eq!(
            term.drain_lazy_bounded(128),
            0,
            "the entire staged backlog was drained"
        );
        assert!(
            term.grid()
                .scrollback()
                .unwrap()
                .any_image(&|span| span.kitty.is_some()),
            "the hot store owns image spans"
        );
    } else if tier == "lazy" {
        assert!(term.lazy_backlog_len() > 0, "history stays staged");
        assert!(
            !term.grid().scrollback().unwrap().any_image(&|_| true),
            "the staged images are not in the hot store"
        );
    }
    assert_eq!(history_placements(&term), vec![(1, 7), (2, 8)]);
    assert!(rows_with_images(&term, 8).is_empty());
    assert_eq!(term.grid().ring_buffer_scrollback() > 0, tier == "ring");
    term
}

#[test]
fn id_deletion_reaches_scrolled_placements_in_each_uncompressed_tier() {
    for tier in ["ring", "lazy", "hot"] {
        let mut term = history_fixture(tier);
        put_at(&mut term, 1, ",p=9", 2, 0);
        control(&mut term, "a=d,d=I,i=1,p=7");
        assert_eq!(
            history_placements(&term),
            vec![(2, 8)],
            "{tier}: named history placement goes"
        );
        assert_eq!(rows_with_images(&term, 8), vec![2]);
        assert!(
            placeable(&mut term, 1),
            "{tier}: the other placement retains data"
        );
        control(&mut term, "a=d,d=i,i=1");
        assert!(
            placeable(&mut term, 1),
            "{tier}: lowercase keeps unplaced data"
        );
        control(&mut term, "a=d,d=I,i=1");
        assert!(
            !placeable(&mut term, 1),
            "{tier}: uppercase frees unplaced data"
        );
        control(&mut term, "a=d,d=I,i=2");
        assert!(history_placements(&term).is_empty());
        assert!(!placeable(&mut term, 2));
        term.scroll_to_top();
        assert!(
            term.cell_frame(8, 20).images.iter().all(Vec::is_empty),
            "{tier}: no rendered ghost"
        );
    }
}

#[test]
fn visible_deletion_preserves_scrollback_and_the_data_it_owns() {
    for tier in ["ring", "lazy", "hot"] {
        for selector in ["a", "A", "P,x=1,y=3", "Z"] {
            let mut term = history_fixture(tier);
            put_at(&mut term, 1, ",p=9", 2, 0);
            control(&mut term, &format!("a=d,d={selector}"));
            assert!(
                rows_with_images(&term, 8).is_empty(),
                "{tier} {selector}: live placement cleared"
            );
            assert_eq!(
                history_placements(&term),
                vec![(1, 7), (2, 8)],
                "{tier} {selector}: history remains"
            );
            assert!(
                placeable(&mut term, 1),
                "{tier} {selector}: history still owns image 1"
            );
            assert!(
                placeable(&mut term, 2),
                "{tier} {selector}: history still owns image 2"
            );
            term.scroll_to_top();
            assert_eq!(
                term.cell_frame(8, 20)
                    .images
                    .iter()
                    .map(Vec::len)
                    .sum::<usize>(),
                4
            );
        }
    }
}

#[test]
fn range_deletion_and_reused_ids_reach_history_without_resurrecting_it() {
    for tier in ["ring", "lazy", "hot"] {
        let mut term = history_fixture(tier);
        put_at(&mut term, 1, ",p=7", 2, 0);
        assert_eq!(
            history_placements(&term),
            vec![(2, 8)],
            "{tier}: a named put moves its historical placement"
        );
        assert_eq!(rows_with_images(&term, 8), vec![2]);
        show_at(&mut term, 2, ",p=10", 3, 0);
        assert!(
            history_placements(&term).is_empty(),
            "{tier}: retransmitting an id replaces its history too"
        );
        control(&mut term, "a=d,d=R,x=1,y=2");
        assert!(rows_with_images(&term, 8).is_empty());
        assert!(!placeable(&mut term, 1) && !placeable(&mut term, 2));
        term.resize(12, 12);
        term.resize(8, 20);
        assert!(
            history_placements(&term).is_empty(),
            "{tier}: resize cannot restore old tiles"
        );

        let mut term = history_fixture(tier);
        control(&mut term, "a=d,d=r,x=1,y=1");
        assert_eq!(history_placements(&term), vec![(2, 8)]);
        assert!(placeable(&mut term, 1), "lowercase range retains the data");
        control(&mut term, "a=d,d=R,x=2,y=2");
        assert!(history_placements(&term).is_empty());
        assert!(
            !placeable(&mut term, 2),
            "uppercase range frees its last historical owner"
        );
    }
}

#[test]
fn number_deletion_reaches_only_the_newest_numbered_history_image() {
    for upper in [false, true] {
        let mut term = Terminal::new(8, 20);
        let raw = [0u8, 255, 0, 255].repeat(4);
        let b64 = aterm_codec::base64::encode(&raw).expect("encode");
        for (row, placement) in [(0, 7), (1, 8)] {
            goto(&mut term, row, 0);
            term.process(
                format!("\x1b_Ga=T,f=32,s=2,v=2,I=42,p={placement},c=2,r=1,q=2;{b64}\x1b\\")
                    .as_bytes(),
            );
        }
        goto(&mut term, 7, 0);
        for _ in 0..20 {
            term.process(b"text\r\n");
        }
        let before = history_placements(&term);
        assert_eq!(before.len(), 2, "two images have the same number");
        assert_eq!(before[0].1, 7);
        assert_eq!(before[1].1, 8);
        let selector = if upper { 'N' } else { 'n' };
        control(&mut term, &format!("a=d,d={selector},I=42"));
        assert_eq!(history_placements(&term), vec![before[0]]);
        goto(&mut term, 2, 0);
        control(&mut term, "a=p,I=42,c=2,r=1,q=2");
        assert_eq!(rows_with_images(&term, 8).is_empty(), upper);
        assert!(
            placeable(&mut term, before[0].0),
            "the older image is still owned"
        );
    }
}

#[test]
fn deleting_a_scrolled_kitty_keeps_other_protocols_and_text() {
    let mut term = history_fixture("lazy");
    goto(&mut term, 0, 0);
    let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
    let b64 = aterm_codec::base64::encode(&png).expect("encode");
    term.process(format!("\x1b]1337;File=inline=1;width=2;height=1:{b64}\x1b\\").as_bytes());
    goto(&mut term, 7, 0);
    for _ in 0..8 {
        term.process(b"kept\r\n");
    }
    let text: Vec<_> = term
        .grid()
        .history_lines_from(0)
        .flatten()
        .map(|line| line.to_string())
        .collect();
    let images = |term: &Terminal| {
        term.grid()
            .history_lines_from(0)
            .flatten()
            .map(|line| line.image_count())
            .sum::<usize>()
    };
    assert_eq!(images(&term), 3, "two Kitty spans and one other protocol");
    control(&mut term, "a=d,d=R,x=1,y=2");
    assert_eq!(images(&term), 1, "only the other protocol remains");
    assert_eq!(
        term.grid()
            .history_lines_from(0)
            .flatten()
            .map(|line| line.to_string())
            .collect::<Vec<_>>(),
        text
    );
}

#[test]
fn id_deletion_removes_both_halves_of_a_partly_scrolled_placement() {
    let mut term = Terminal::new(8, 20);
    goto(&mut term, 0, 0);
    let raw = [255u8, 0, 0, 255].repeat(4);
    let b64 = aterm_codec::base64::encode(&raw).expect("encode");
    term.process(format!("\x1b_Ga=T,f=32,s=2,v=2,i=1,p=7,c=2,r=3,q=2;{b64}\x1b\\").as_bytes());
    goto(&mut term, 7, 0);
    term.process(b"\r\n");
    assert_eq!(
        history_placements(&term),
        vec![(1, 7)],
        "the first footprint row is in history"
    );
    assert_eq!(
        rows_with_images(&term, 8),
        vec![0, 1],
        "the other two rows stay live"
    );
    control(&mut term, "a=d,d=i,i=1,p=7");
    assert!(history_placements(&term).is_empty());
    assert!(rows_with_images(&term, 8).is_empty());
    assert!(
        placeable(&mut term, 1),
        "lowercase retains data after clearing both halves"
    );
}
