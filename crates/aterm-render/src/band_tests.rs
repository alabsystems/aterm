// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_messages::ink::{BarBase, ThemeInks};

fn inks() -> BandInks {
    BandInks::derive(
        ThemeInks {
            bg: [0x1e, 0x1e, 0x2e],
            fg: [0xcd, 0xd6, 0xf4],
            cursor: [0xf5, 0xe0, 0xdc],
        },
        None,
        BarBase::Blend,
    )
}

/// A resolved cell's continuation is the render cell's `wide`, so the renderer draws a
/// CJK lead cell across both.
#[test]
fn a_continuation_is_the_render_cells_wide_half() {
    let c = inks();
    let k = |ch, continuation| aterm_messages::ink::InkedCell {
        ch,
        fg: c.value,
        bg: c.bar_bg,
        bold: false,
        text_presentation: false,
        continuation,
    };
    let (rows, _, _) = rows(vec![Resolved {
        cells: vec![k('\u{62c5}', false), k(' ', true), k('x', false)],
        metered: false,
        raster: None,
    }]);
    let wide: Vec<bool> = rows[0].iter().map(|r| r.wide).collect();
    assert_eq!(wide, [false, true, false]);
    assert_eq!(crate::materialized_cell_span(&rows[0], 0), 2);
    assert_eq!(crate::materialized_cell_span(&rows[0], 2), 1);
}

/// Every char the renderer draws as a band icon is the engine's icon of that char, and
/// no other char is an engine icon; every icon is the engine's, a glass mark or a need
/// pictogram, and only the engine's have a band glyph.
#[test]
fn band_icon_ids_match_the_renderers() {
    for ch in (0..=0x10_ffffu32).filter_map(char::from_u32) {
        assert_eq!(
            BandIcon::for_char(ch),
            Icon::for_char(ch).map(band_icon),
            "{ch:?}"
        );
    }
    for icon in BandIcon::ALL {
        match BandIcon::glass_mark(icon.ch()) {
            Some(mark) => {
                assert_eq!(mark, icon);
                assert_eq!(Icon::for_char(icon.ch()), None, "{icon:?}: no band glyph");
            }
            None if icon.is_pictogram() => {
                assert_eq!(Icon::for_char(icon.ch()), None, "{icon:?}: no band glyph");
            }
            None => assert_eq!(Icon::for_char(icon.ch()).map(band_icon), Some(icon)),
        }
    }
}

/// No committed row leaves the frame untouched; a short cache is padded and a long one
/// trimmed, and only the composed stack's last row carries the seam.
#[test]
fn compose_pads_trims_and_seals_the_stack_it_composes() {
    let c = inks();
    let at = BandFrame {
        cols: 4,
        cell_w: 8,
        cell_h: 16,
        pad: 2,
        lo: 0,
        frame_w: 36,
        grid_top: 2,
        room: None,
    };
    let engine = || RenderInput {
        rows: 2,
        cols: 4,
        cells: vec![vec![cell('x', [1, 1, 1], [2, 2, 2], false, false); 4]; 2],
        ..Default::default()
    };
    let row = |ch| vec![cell(ch, c.value, c.bar_bg, false, false); 4];
    let pixels = HostRowPixels::Translate;
    let mut f = engine();
    assert_eq!(
        compose_band(
            &mut f,
            &[row('a')],
            &[None],
            0,
            &c,
            false,
            at,
            &mut Vec::new(),
            pixels
        ),
        0
    );
    assert!(f == engine(), "no committed row: the frame is untouched");
    let mut f = engine();
    let n = compose_band(
        &mut f,
        &[row('a')],
        &[None],
        3,
        &c,
        false,
        at,
        &mut Vec::new(),
        pixels,
    );
    assert_eq!((n, f.rows), (3, 5), "one painted row padded to three");
    assert_eq!(f.cells[1], {
        let mut r = blank_band_row(4, &c);
        for k in &mut r {
            k.underline = UnderlineStyle::Single;
            k.underline_color = Some(mix3(c.bar_bg, c.label, DIVIDER_ALPHA));
        }
        r
    });
    assert!(
        f.cells[2]
            .iter()
            .all(|k| k.underline_color == Some(c.label)),
        "the seam"
    );
    assert_eq!(f.cells[3][0].ch, 'x', "the terminal starts under the band");
    let mut f = engine();
    let n = compose_band(
        &mut f,
        &[row('a'), row('b'), row('c')],
        &[None, None, None],
        2,
        &c,
        true,
        at,
        &mut Vec::new(),
        pixels,
    );
    assert_eq!(
        (n, f.cells[1][0].ch),
        (2, 'b'),
        "three painted rows trimmed to two"
    );
    assert_eq!(
        f.cells[0][0].underline_color,
        Some(c.label),
        "under a forced palette the divider is the seam's own ink"
    );
}

/// A badge row's icons are laid in a list the frame's last refill retired, as the host's
/// marks' are (`RenderInput::chrome_icon_list`), so with one or two badge rows up every
/// list circulates. In the baseline native host's order, without Find or Build adornments
/// (the terminal's cells laid again, the band composed, then line D's five marks and line B's
/// one, each row taking a list as `WindowCards::paint_icon` does), every refilled frame's icon
/// lists, the badges' and the marks', are the ones the frame before retired, no mark outgrows
/// the list it took, and the spares stay within the pane's rows, for more refills than the pane
/// has rows.
/// NEGATIVE CONTROL: collecting a badge row's icons into a new list (`on_frame` before it
/// took one) hands line D a badge's one-slot list, which its marks outgrow on every refilled
/// frame (run once, restored).
#[test]
fn a_badge_rows_icon_lists_circulate_with_the_marks() {
    use aterm_core::terminal::Terminal;
    let c = inks();
    let (pane, cols) = (6u16, 4u16);
    let (rows_n, cols_n) = (usize::from(pane), usize::from(cols));
    let at = BandFrame {
        cols: cols_n,
        cell_w: 8,
        cell_h: 16,
        pad: 2,
        lo: 0,
        frame_w: 36,
        grid_top: 0,
        room: None,
    };
    for badges in [1usize, 2] {
        let rows = vec![vec![cell('!', c.value, c.bar_bg, false, false); cols_n]; badges];
        let badge = RowRaster {
            icons: vec![(0, Icon::Warn)],
            ..RowRaster::default()
        };
        let rasters = vec![Some(badge); badges];
        let mut term = Terminal::new(pane, cols);
        let mut f = RenderInput::empty();
        let mut pool = Vec::new();
        let mut retired: Vec<*const ChromeIcon> = Vec::new();
        for refill in 0..3 * rows_n {
            let at_refill = format!("{badges} badge row(s), refill {refill}");
            term.cell_frame_into(&mut f, rows_n, cols_n);
            assert!(
                f.chrome_icon_spares.len() <= rows_n,
                "{at_refill}: at most a list per row"
            );
            let n = compose_band(
                &mut f,
                &rows,
                &rasters,
                badges,
                &c,
                false,
                at,
                &mut pool,
                HostRowPixels::Placed,
            );
            assert_eq!(n, badges, "{at_refill}: the band is up");
            for (line, row, marks) in [("D", f.rows - 2, 5u16), ("B", f.rows - 1, 1)] {
                let mut icons = f.chrome_icon_list();
                let taken = icons.as_ptr();
                if refill > 0 {
                    assert!(
                        icons.capacity() >= usize::from(marks),
                        "{at_refill}: line {line}'s marks outgrow the list they took"
                    );
                }
                icons.extend((0..marks).map(|col| ChromeIcon {
                    col,
                    icon: BandIcon::Diamond,
                }));
                assert!(
                    refill == 0 || icons.as_ptr() == taken,
                    "{at_refill}: line {line}'s list moved despite its capacity"
                );
                f.chrome_rasters.push(ChromeRaster {
                    row: u16::try_from(row).expect("a frame row"),
                    ground: std::sync::Arc::from(Vec::new()),
                    rail: std::sync::Arc::from(Vec::new()),
                    rail_h: 0,
                    clear_rail: false,
                    own: Vec::new(),
                    split: None,
                    rings: Vec::new(),
                    icons,
                });
            }
            let mut held: Vec<*const ChromeIcon> = f
                .chrome_rasters
                .iter()
                .filter(|r| r.icons.capacity() > 0)
                .map(|r| r.icons.as_ptr())
                .collect();
            held.sort_unstable();
            assert_eq!(held.len(), badges + 2, "{at_refill}: every row's icons");
            if refill > 0 {
                assert_eq!(
                    held, retired,
                    "{at_refill}: every icon list is one the refill retired"
                );
            }
            retired = held;
        }
    }
}

/// Band row `i` is chrome row `first + i`, each metered row carries its packed gutter tones,
/// and only the committed rows carry any.
#[test]
fn band_row_edges_place_each_metered_row_at_its_chrome_row() {
    let edges = [
        Some(([0x11, 0x22, 0x33], [0x44, 0x55, 0x66])),
        None,
        Some(([1, 2, 3], [4, 5, 6])),
    ];
    let got = band_row_edges(2, 3, &edges);
    assert_eq!(
        got[0],
        Some(crate::ChromeRowEdges {
            row: 2,
            left: 0x0011_2233,
            right: 0x0044_5566,
        })
    );
    assert_eq!(got[1], None);
    assert_eq!(
        got[2].map(|e| (e.row, e.left, e.right)),
        Some((4, 0x0001_0203, 0x0004_0506))
    );
    let trimmed = band_row_edges(0, 1, &edges);
    assert_eq!(trimmed[0].map(|e| e.row), Some(0));
    assert_eq!(
        &trimmed[1..],
        &[None, None],
        "an uncommitted row is not on glass"
    );
}
