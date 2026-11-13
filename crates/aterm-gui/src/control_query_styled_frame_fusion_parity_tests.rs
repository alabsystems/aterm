// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    NO_LINK, StyledCellSnap, StyledFrameSnapshot, StyledLink, StyledRowSnap, gather_styled_frame,
    serialize_styled_frame,
};
use aterm_core::terminal::Terminal;

fn reference_snapshot(t: &Terminal) -> StyledFrameSnapshot {
    let rows = t.rows() as usize;
    let cols = t.cols() as usize;
    let blank = t.implicit_blank_render_cell();
    let any_extras = !t.grid().extras().is_empty();
    let mut cells: Vec<StyledRowSnap> = Vec::new();
    let mut links: Vec<StyledLink> = Vec::new();
    for r in 0..rows {
        let rendered = t.render_row_at_screen(r);
        let view = t.grid().screen_row_view(r as u16);
        let mut glyphs = String::new();
        let mut row_cells: Vec<StyledCellSnap> = Vec::new();
        for c in 0..cols {
            let start = glyphs.len();
            glyphs.push_str(&t.cell_grapheme(r, c).unwrap_or_default());
            let extra = if any_extras {
                t.grid().cell_extra(r as u16, c as u16)
            } else {
                None
            };
            let url = extra.and_then(|e| e.hyperlink());
            let id = extra.and_then(|e| e.hyperlink_id());
            let link = match url {
                None => NO_LINK,
                Some(u) => {
                    let pos = links
                        .iter()
                        .position(|l| *l.url == **u && l.id.as_deref() == id.map(|i| &**i));
                    let pos = pos.unwrap_or_else(|| {
                        links.push(StyledLink {
                            url: std::sync::Arc::clone(u),
                            id: id.map(std::sync::Arc::clone),
                        });
                        links.len() - 1
                    });
                    u32::try_from(pos).expect("small link table")
                }
            };
            row_cells.push(StyledCellSnap {
                cell: rendered.get(c).copied().unwrap_or(blank),
                glyph: start..glyphs.len(),
                wide_lead: view.cell(c as u16).is_some_and(|cell| cell.is_wide()),
                blink: view
                    .cell(c as u16)
                    .is_some_and(|cell| cell.flags().contains(aterm_core::grid::CellFlags::BLINK)),
                link,
            });
        }
        cells.push(StyledRowSnap {
            glyphs,
            cells: row_cells,
        });
    }
    let mut snap = gather_styled_frame(t);
    snap.cells = cells;
    snap.links = links;
    snap
}

fn corpus() -> Vec<(&'static str, Terminal)> {
    let mut out: Vec<(&'static str, Terminal)> = Vec::new();
    out.push(("blank", Terminal::new(4, 12)));
    let mut t = Terminal::new(4, 12);
    t.process(b"hello");
    out.push(("plain_ascii", t));
    let mut t = Terminal::new(3, 10);
    t.process("\u{4f60}\u{597d}ab".as_bytes());
    out.push(("wide_cjk", t));
    let mut t = Terminal::new(2, 8);
    t.process("e\u{0301}x".as_bytes());
    out.push(("combining_nfd", t));
    let mut t = Terminal::new(3, 10);
    t.process(b"\x1b[4:3m\x1b[58:2::255:0:0mUU\x1b[m plain");
    out.push(("extras_but_no_link", t));
    let mut t = Terminal::new(3, 20);
    t.process(b"pre \x1b]8;id=k1;https://example.com\x1b\\LINK\x1b]8;;\x1b\\ post");
    out.push(("one_link_with_id", t));
    let mut t = Terminal::new(2, 20);
    t.process(b"\x1b]8;id=a;https://x.example\x1b\\AA\x1b]8;;\x1b\\");
    t.process(b"\x1b]8;id=b;https://x.example\x1b\\BB\x1b]8;;\x1b\\");
    out.push(("same_url_two_ids", t));
    let mut t = Terminal::new(2, 16);
    t.process(b"\x1b]8;;https://noid.example\x1b\\N\x1b]8;;\x1b\\");
    out.push(("link_without_id", t));
    let mut t = Terminal::new(4, 24);
    for i in 0..24u32 {
        t.process(format!("\x1b]8;id=i{i};https://d{i}.example\x1b\\x\x1b]8;;\x1b\\").as_bytes());
    }
    out.push(("hyperlink_dense", t));
    let mut t = Terminal::new(4, 20);
    for i in 0..40u32 {
        t.process(
            format!("\x1b]8;id=s{i};https://s{i}.example\x1b\\row{i}\x1b]8;;\x1b\\\r\n").as_bytes(),
        );
    }
    out.push(("scrolled_links", t));
    let mut t = Terminal::new(3, 10);
    t.process(b"\x1b[?5hrev");
    out.push(("reverse_video", t));
    let mut t = Terminal::new(3, 12);
    t.process(b"\x1b]1337;File=inline=1;width=2;height=1:iVBORw0KGgoAAAAA\x1b\\");
    t.process(b"\x1b]8;;https://img.example\x1b\\L\x1b]8;;\x1b\\");
    out.push(("image_and_link", t));
    let mut t = Terminal::new(3, 12);
    t.process(b"\x1b[2;1H\x1b#6\x1b]8;;https://dw.example\x1b\\W\x1b]8;;\x1b\\");
    out.push(("double_width_line", t));
    out.push(("single_column", Terminal::new(3, 1)));
    let mut t = Terminal::new(2, 10);
    t.process("a\u{1F600}b".as_bytes());
    out.push(("ring_emoji", t));
    let mut t = Terminal::new(2, 10);
    t.process(b"ab");
    t.grid_mut()
        .set_cell_complex_char(0, 0, "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}");
    out.push(("map_cluster", t));
    let mut t = Terminal::new(2, 10);
    t.process("\x1b[1\"qP\u{4f60}Q\x1b[0\"qz".as_bytes());
    out.push(("decsca_protected", t));
    out
}

#[test]
fn one_walk_link_gather_matches_the_per_cell_reference_over_a_corpus() {
    for (name, t) in &corpus() {
        assert_eq!(
            serialize_styled_frame(&gather_styled_frame(t)),
            serialize_styled_frame(&reference_snapshot(t)),
            "styled frame diverged on corpus case {name}"
        );
    }
}
