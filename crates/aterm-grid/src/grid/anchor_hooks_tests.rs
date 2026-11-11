// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::{AnchorRole, Grid};

fn grid_with(text: &[&str]) -> Grid {
    let mut g = Grid::with_scrollback(4, 10, 100);
    for (r, line) in text.iter().enumerate() {
        g.set_cursor(u16::try_from(r).unwrap(), 0);
        for ch in line.chars() {
            g.write_char(ch);
        }
    }
    g
}

#[test]
fn the_lay_scan_reads_the_cursor_row_from_its_column_and_every_row_below() {
    let g = grid_with(&["ab", "", "  x"]);
    assert!(g.has_ink_from(0, 1), "'b' at column 1");
    assert!(g.has_ink_from(1, 0), "row 2's 'x' lies below");
    assert!(!g.has_ink_from(3, 0));
    let g = grid_with(&["ab"]);
    assert!(!g.has_ink_from(0, 2), "nothing from column 2 on");
    assert!(g.has_ink_from(0, 0));

    let mut g = Grid::with_scrollback(4, 10, 100);
    g.write_char_at_cursor_packed(
        'p',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::PROTECTED,
    );
    assert!(g.has_ink_from(0, 0), "a protected glyph is not a spacer");
    let mut g = Grid::with_scrollback(4, 10, 100);
    assert!(g.write_wide_char_at_cursor_packed_ecols(
        '界',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::PROTECTED,
        10,
    ));
    assert!(g.has_ink_from(0, 0), "the protected wide lead is ink");
    assert!(
        !g.has_ink_from(0, 1),
        "its genuine spacer is not another glyph"
    );
}

/// One `rows`×`cols` inline image.
fn image(rows: u16, cols: u16) -> std::sync::Arc<crate::ImageData> {
    std::sync::Arc::new(crate::ImageData {
        bytes: vec![7u8; 16],
        format: crate::ImageFormat::Png,
        cols,
        rows,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: crate::ImageScaling::Fit,
        source_rect: None,
    })
}

fn tile(image: &std::sync::Arc<crate::ImageData>, cell_col: u16) -> crate::ImageRef {
    crate::ImageRef {
        image: std::sync::Arc::clone(image),
        cell_row: 0,
        cell_col,
        kitty: None,
    }
}

/// Ink is anything an erased default cell does not show, whatever draws
/// it: a reversed, underlined, struck or overlined blank, a blank under
/// an image, a combining mark on a space, a linked space. A bold, dim,
/// italic, blinking, hidden or superscript space, or one in a foreground
/// colour, shows nothing.
#[test]
fn the_lay_scan_sees_every_ink_an_erased_cell_does_not_show() {
    let blank_with = |flags: crate::CellFlags, colors: crate::PackedColors| {
        let mut g = Grid::with_scrollback(4, 10, 100);
        g.set_cursor(2, 3);
        g.write_char_at_cursor_packed(' ', colors, flags);
        g.has_ink_from(1, 0)
    };
    let plain = crate::PackedColors::DEFAULT;
    for (flags, why) in [
        (crate::CellFlags::INVERSE, "reverse video"),
        (crate::CellFlags::UNDERLINE, "an underline"),
        (crate::CellFlags::DOUBLE_UNDERLINE, "a double underline"),
        (crate::CellFlags::CURLY_UNDERLINE, "a curly underline"),
        (crate::CellFlags::DOTTED_UNDERLINE, "a dotted underline"),
        (crate::CellFlags::DASHED_UNDERLINE, "a dashed underline"),
        (crate::CellFlags::STRIKETHROUGH, "a strikethrough"),
        (crate::CellFlags::OVERLINE, "an overline"),
    ] {
        assert!(blank_with(flags, plain), "{why} on a space is ink");
    }
    for (flags, why) in [
        (crate::CellFlags::BOLD, "bold"),
        (crate::CellFlags::DIM, "dim"),
        (crate::CellFlags::ITALIC, "italic"),
        (crate::CellFlags::BLINK, "blink"),
        (crate::CellFlags::HIDDEN, "hidden"),
        (crate::CellFlags::SUPERSCRIPT, "superscript"),
        (crate::CellFlags::SUBSCRIPT, "subscript"),
    ] {
        assert!(!blank_with(flags, plain), "a {why} space shows nothing");
    }
    assert!(
        !blank_with(
            crate::CellFlags::empty(),
            crate::PackedColors::with_indexed_fg(1)
        ),
        "a foreground colour shows nothing on a space"
    );
    assert!(
        blank_with(
            crate::CellFlags::empty(),
            crate::PackedColors::with_indexed_bg(1)
        ),
        "a background is ink"
    );

    let picture = image(1, 2);
    let mut g = Grid::with_scrollback(4, 10, 100);
    g.set_cell_image(3, 4, tile(&picture, 0));
    assert!(g.has_ink_from(1, 0), "an image below");
    assert!(g.has_ink_from(3, 4), "an image at the cursor");
    assert!(!g.has_ink_from(3, 5), "nothing right of it");

    let mut g = Grid::with_scrollback(4, 10, 100);
    g.set_cursor(2, 0);
    g.write_char(' ');
    g.cell_extra_mut(2, 0).add_combining('\u{301}');
    assert!(g.has_ink_from(1, 0), "an accent on a space");
    let mut g = Grid::with_scrollback(4, 10, 100);
    g.cell_extra_mut(2, 1)
        .set_hyperlink(Some(std::sync::Arc::from("https://example.com")));
    assert!(g.has_ink_from(1, 0), "a linked space");

    let mut g = Grid::with_scrollback(4, 10, 100);
    g.set_cursor(2, 0);
    assert!(g.write_wide_char_at_cursor_packed_ecols('界', plain, crate::CellFlags::empty(), 10,));
    g.set_cell_image(2, 1, tile(&picture, 1));
    assert!(g.has_ink_from(2, 1), "the spacer's image tile");
}

/// A row's fingerprint reads what its extras paint as well as its cells:
/// an image stacked on a cell that already had extras, a link, an
/// underline colour. And the row keeps that value in the ring's history.
#[test]
fn a_fingerprint_reads_images_links_and_underline_colours_in_either_tier() {
    let mut g = Grid::with_scrollback(4, 10, 100);
    g.write_char('e');
    g.cell_extra_mut(0, 0).add_combining('\u{301}');
    g.write_char('x');
    let mut seen = vec![g.live_row_fingerprint(0).unwrap()];
    let mut changed = |g: &Grid, why: &str| {
        let now = g.live_row_fingerprint(0).unwrap();
        assert!(!seen.contains(&now), "{why} moves the fingerprint");
        seen.push(now);
    };
    let picture = image(1, 2);
    g.stack_cell_image(0, 0, tile(&picture, 0));
    changed(&g, "an image over a cell that had extras");
    g.stack_cell_image(0, 0, tile(&image(1, 2), 0));
    changed(&g, "a second layer");
    g.cell_extra_mut(0, 1)
        .set_hyperlink(Some(std::sync::Arc::from("https://example.com")));
    changed(&g, "a link");
    g.cell_extra_mut(0, 1)
        .set_hyperlink_id(Some(std::sync::Arc::from("id")));
    changed(&g, "a link's id");
    g.cell_extra_mut(0, 1).set_underline_color(Some([1, 2, 3]));
    changed(&g, "an underline colour");
    g.cell_extra_mut(0, 1)
        .set_underline_color_u32(Some(0x02_00_00_05));
    changed(&g, "an indexed underline colour");

    let on_screen = g.live_row_fingerprint(0);
    let absolute = g.visible_to_absolute(0);
    g.set_cursor(3, 0);
    for _ in 0..2 {
        g.line_feed();
    }
    assert!(g.visible_to_absolute(0) > absolute, "the row is history");
    assert_eq!(g.row_fingerprint_at_absolute(absolute), on_screen);

    let mut g = Grid::with_scrollback(4, 10, 100);
    assert!(g.write_wide_char_at_cursor_packed_ecols(
        '界',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::empty(),
        10,
    ));
    g.set_cursor(0, 2);
    g.write_emoji_autowrap_fast(
        '😀',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::COMPLEX,
    );
    g.set_cell_image(0, 1, tile(&picture, 1));
    assert!(g.row(0).unwrap().is_cell_wide_continuation(1));
    let cells = g.row(0).unwrap().as_slice().to_vec();
    let before = g.live_row_fingerprint(0).unwrap();
    let replacement = image(1, 2);
    g.set_cell_image(0, 1, tile(&replacement, 1));
    assert_eq!(g.row(0).unwrap().as_slice(), cells);
    let replaced = g.live_row_fingerprint(0).unwrap();
    assert_ne!(replaced, before, "the spacer's image payload changed");
    assert_eq!(g.live_row_fingerprint(0), Some(replaced));
    let absolute = g.visible_to_absolute(0);
    g.set_cursor(3, 0);
    g.line_feed();
    assert_eq!(g.row_fingerprint_at_absolute(absolute), Some(replaced));

    let tagged = |cell_col, serial| crate::ImageRef {
        kitty: Some(crate::KittyPlacementTag {
            image_id: 7,
            placement_id: u32::from(cell_col) + 1,
            image_serial: std::num::NonZeroU32::new(1).unwrap(),
            serial: std::num::NonZeroU32::new(serial).unwrap(),
        }),
        ..tile(&picture, cell_col)
    };
    let mut g = Grid::with_scrollback(4, 10, 100);
    g.set_cell_image(0, 0, tagged(0, 2));
    g.stack_cell_image(0, 0, tagged(1, 3));
    let cells = g.row(0).unwrap().as_slice().to_vec();
    let before = g.live_row_fingerprint(0).unwrap();
    assert_eq!(
        g.cell_extra(0, 0)
            .unwrap()
            .images()
            .iter()
            .map(|t| t.cell_col)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    g.set_cell_image(0, 0, tagged(1, 3));
    g.stack_cell_image(0, 0, tagged(0, 4));
    assert_eq!(g.row(0).unwrap().as_slice(), cells);
    assert_eq!(
        g.cell_extra(0, 0)
            .unwrap()
            .images()
            .iter()
            .map(|t| t.cell_col)
            .collect::<Vec<_>>(),
        [1, 0]
    );
    let reordered = g.live_row_fingerprint(0).unwrap();
    assert_ne!(reordered, before, "the image tile paint order changed");
    let absolute = g.visible_to_absolute(0);
    g.set_cursor(3, 0);
    g.line_feed();
    assert_eq!(g.row_fingerprint_at_absolute(absolute), Some(reordered));
}

#[test]
fn a_fingerprint_moves_iff_the_row_changes() {
    let mut g = grid_with(&["abc", "abc"]);
    let a = g.live_row_fingerprint(0).unwrap();
    assert_eq!(a, g.live_row_fingerprint(1).unwrap());
    g.set_cursor(0, 1);
    g.write_char('X');
    assert_ne!(a, g.live_row_fingerprint(0).unwrap());
    assert_eq!(g.live_row_fingerprint(9), None);

    let mut g = Grid::with_scrollback(4, 10, 100);
    g.write_emoji_autowrap_fast(
        '😀',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::COMPLEX,
    );
    let first = g.live_row_fingerprint(0).unwrap();
    assert_eq!(g.live_row_fingerprint(0), Some(first));
    g.set_cursor(0, 0);
    g.write_emoji_autowrap_fast(
        '🚀',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::COMPLEX,
    );
    assert_ne!(g.live_row_fingerprint(0), Some(first), "ring glyph changed");

    let colors = crate::PackedColors::DEFAULT.with_rgb_fg().with_rgb_bg();
    g.set_cursor(1, 0);
    g.write_char_at_cursor_packed('x', colors, crate::CellFlags::empty());
    g.extras_mut()
        .set_rgb_ring_range(1, 0, 1, Some([1, 2, 3]), Some([4, 5, 6]), 4, 10);
    let first = g.live_row_fingerprint(1).unwrap();
    assert_eq!(g.live_row_fingerprint(1), Some(first));
    g.extras_mut()
        .set_rgb_ring_range(1, 0, 1, Some([3, 2, 1]), Some([4, 5, 6]), 4, 10);
    let second = g.live_row_fingerprint(1).unwrap();
    assert_ne!(second, first, "ring foreground changed");
    g.extras_mut()
        .set_rgb_ring_range(1, 0, 1, Some([3, 2, 1]), Some([6, 5, 4]), 4, 10);
    assert_ne!(
        g.live_row_fingerprint(1),
        Some(second),
        "ring background changed"
    );

    for flags in [
        crate::CellFlags::COMPLEX,
        crate::CellFlags::COMPLEX | crate::CellFlags::PROTECTED,
    ] {
        let mut g = Grid::with_scrollback(4, 10, 100);
        g.write_emoji_autowrap_fast('😀', crate::PackedColors::DEFAULT, flags);
        let first = g.live_row_fingerprint(0).unwrap();
        g.set_cursor(0, 0);
        g.write_emoji_autowrap_fast('🚀', crate::PackedColors::DEFAULT, flags);
        let changed = g.live_row_fingerprint(0).unwrap();
        assert_ne!(
            changed, first,
            "protected and ordinary dense glyphs are resolved"
        );
        let absolute = g.visible_to_absolute(0);
        g.set_cursor(3, 0);
        g.line_feed();
        assert_eq!(g.row_fingerprint_at_absolute(absolute), Some(changed));
    }

    let mut g = Grid::with_scrollback(4, 10, 100);
    let colors = crate::PackedColors::DEFAULT.with_rgb_fg().with_rgb_bg();
    g.write_char_at_cursor_packed('p', colors, crate::CellFlags::PROTECTED);
    g.extras_mut()
        .set_rgb_ring_range(0, 0, 1, Some([1, 2, 3]), Some([4, 5, 6]), 4, 10);
    let first = g.live_row_fingerprint(0).unwrap();
    g.extras_mut()
        .set_rgb_ring_range(0, 0, 1, Some([3, 2, 1]), Some([4, 5, 6]), 4, 10);
    let changed = g.live_row_fingerprint(0).unwrap();
    assert_ne!(changed, first, "protected RGB payload is resolved");
    let absolute = g.visible_to_absolute(0);
    g.set_cursor(3, 0);
    g.line_feed();
    assert_eq!(g.row_fingerprint_at_absolute(absolute), Some(changed));

    let mut g = Grid::with_scrollback(4, 10, 100);
    g.write_char_at_cursor_packed('x', colors, crate::CellFlags::empty());
    assert_eq!(
        g.live_row_fingerprint(0),
        None,
        "missing required RGB cannot vouch for a row"
    );
    let absolute = g.visible_to_absolute(0);
    g.set_cursor(3, 0);
    g.line_feed();
    assert_eq!(g.row_fingerprint_at_absolute(absolute), None);
}

/// Grid with `history` lines scrolled off and numbered lines on screen.
fn numbered(rows: u16, history: usize) -> Grid {
    let mut g = Grid::with_scrollback(rows, 10, 1000);
    for n in 0..history + usize::from(rows) {
        g.carriage_return();
        for ch in format!("L{n}").chars() {
            g.write_char(ch);
        }
        if n + 1 < history + usize::from(rows) {
            g.line_feed();
        }
    }
    g
}

#[test]
fn insert_and_delete_lines_carry_anchors_with_their_band() {
    let mut g = numbered(8, 0);
    let base = g.visible_to_absolute(0);
    let below_cursor = g.anchors_mut().place(base + 4, AnchorRole::Start).unwrap();
    let above_cursor = g.anchors_mut().place(base + 1, AnchorRole::End).unwrap();
    g.set_cursor(2, 0);
    g.insert_lines(2);
    assert_eq!(g.anchors().row(below_cursor), Some(base + 6));
    assert_eq!(
        g.anchors().row(above_cursor),
        Some(base + 1),
        "above the band"
    );
    g.delete_lines(3);
    assert_eq!(g.anchors().row(below_cursor), Some(base + 3));
    assert!(!g.anchors().lost());

    let mut g = numbered(4, 0);
    let base = g.visible_to_absolute(0);
    let before = g.anchors_mut().place(base, AnchorRole::Start).unwrap();
    let end = g.anchors_mut().place(base + 1, AnchorRole::End).unwrap();
    let next = g.anchors_mut().place(base + 1, AnchorRole::Start).unwrap();
    g.set_cursor(1, 0);
    g.insert_lines(1);
    assert_eq!(g.anchors().row(before), Some(base));
    assert_eq!(
        g.anchors().row(end),
        Some(base + 1),
        "inserted row excluded"
    );
    assert_eq!(
        g.anchors().row(next),
        Some(base + 1),
        "inserted row joins the span starting there"
    );
}

#[test]
fn region_scrolls_carry_anchors_and_a_full_screen_scroll_keeps_their_keys() {
    let mut g = numbered(8, 0);
    let base = g.visible_to_absolute(0);
    let id = g.anchors_mut().place(base + 5, AnchorRole::Start).unwrap();
    g.set_scroll_region(2, 6);
    g.scroll_region_down(1);
    assert_eq!(g.anchors().row(id), Some(base + 6));
    g.scroll_region_up(3);
    assert_eq!(g.anchors().row(id), Some(base + 3));
    g.set_scroll_region(0, 7);
    g.set_cursor(7, 0);
    g.line_feed();
    assert_eq!(g.anchors().row(id), Some(base + 3), "history keeps the key");
    assert!(!g.anchors().lost());
}

#[test]
fn the_archival_scroll_splices_and_an_end_at_the_boundary_stays() {
    let mut g = numbered(8, 2);
    let base = g.visible_to_absolute(0);
    let end = g.anchors_mut().place(base + 6, AnchorRole::End).unwrap();
    let footer = g.anchors_mut().place(base + 6, AnchorRole::Start).unwrap();
    let inside = g.anchors_mut().place(base + 3, AnchorRole::Start).unwrap();
    g.set_scroll_region(0, 5);
    g.set_cursor(5, 0);
    g.line_feed();
    assert_eq!(g.anchors().row(end), Some(base + 6), "End == at stays");
    assert_eq!(
        g.anchors().row(footer),
        Some(base + 7),
        "Start == at shifts"
    );
    assert_eq!(
        g.anchors().row(inside),
        Some(base + 3),
        "the region keeps its keys"
    );
    assert!(!g.anchors().lost());
}

#[test]
fn a_margined_line_insert_loses_the_anchors() {
    let mut g = numbered(8, 0);
    let row = g.visible_to_absolute(3);
    g.anchors_mut().place(row, AnchorRole::Start).unwrap();
    g.set_horizontal_margins(2, 5);
    g.set_cursor(1, 2);
    g.insert_lines_margined(1, true);
    assert!(g.anchors().lost());
}

#[test]
fn ed3_clamps_an_anchor_in_history_to_the_oldest_row() {
    let mut g = numbered(4, 6);
    let oldest = g.oldest_absolute_row();
    let start = g
        .anchors_mut()
        .place(oldest + 1, AnchorRole::Start)
        .unwrap();
    let screen = g.visible_to_absolute(1);
    let kept = g.anchors_mut().place(screen, AnchorRole::End).unwrap();
    g.erase_scrollback();
    assert_eq!(g.anchors().row(start), Some(g.oldest_absolute_row()));
    assert_eq!(g.anchors().row(kept), Some(screen));
    assert!(!g.anchors().lost());
}

#[test]
fn an_unscroll_loses_the_anchors_unless_the_knob_keeps_them() {
    for knob in [false, true] {
        let mut g = numbered(4, 6);
        let row = g.visible_to_absolute(1);
        g.anchors_mut().place(row, AnchorRole::Start).unwrap();
        let run = |g: &mut Grid| assert_eq!(g.unscroll_from_scrollback(2), 2);
        if knob {
            crate::with_knob(crate::FenceKnob::UnlentRenumberKeeps, || run(&mut g));
        } else {
            run(&mut g);
        }
        assert_eq!(g.anchors().lost(), !knob);
    }
}

#[test]
fn a_rows_only_resize_keeps_every_anchor_on_its_row() {
    let mut g = numbered(8, 4);
    let base = g.visible_to_absolute(0);
    let id = g.anchors_mut().place(base + 2, AnchorRole::Start).unwrap();
    g.resize(5, 10);
    assert_eq!(g.anchors().row(id), Some(base + 2));
    let epoch = g.anchors().seal_epoch();
    g.resize(8, 10);
    assert_eq!(g.anchors().row(id), Some(base + 2));
    assert!(!g.anchors().lost());
    assert_ne!(
        g.anchors().seal_epoch(),
        epoch,
        "a reveal rekeys sealed rows"
    );
}

#[test]
fn an_unlent_width_rewrap_loses_the_anchors() {
    let mut g = numbered(4, 2);
    let row = g.visible_to_absolute(1);
    g.anchors_mut().place(row, AnchorRole::Start).unwrap();
    g.resize(4, 12);
    assert!(g.anchors().lost());
}

#[test]
fn a_swap_invalidation_keeps_anchors_and_a_plain_one_loses_them() {
    let mut g = grid_with(&["a"]);
    let id = g.anchors_mut().place(0, AnchorRole::Start).unwrap();
    g.invalidate_host_coordinates_for_swap();
    assert!(!g.anchors().lost());
    assert_eq!(g.anchors().row(id), Some(0));
    g.invalidate_host_coordinates();
    assert!(g.anchors().lost());
}
