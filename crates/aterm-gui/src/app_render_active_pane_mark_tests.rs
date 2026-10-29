// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ActivePaneMark, divider_cell, fill_divider_grid};
use crate::pane_chrome::{SplitChrome, with_split_chrome};
use aterm_render::{RenderInput, Theme};

fn stripes(dst: &RenderInput, theme: Theme) -> Vec<(usize, usize, char)> {
    let seam = divider_cell(theme);
    let mut out = Vec::new();
    for (r, row) in dst.cells.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if *cell != seam {
                out.push((r, c, cell.ch));
            }
        }
    }
    out
}

#[test]
fn the_mark_inks_the_divider_lines_that_touch_the_active_pane_and_no_others() {
    with_split_chrome(SplitChrome::Classic, || {
        let theme = Theme::default();
        let mut dst = RenderInput::empty();
        fill_divider_grid(
            &mut dst,
            5,
            11,
            theme,
            Some(ActivePaneMark {
                row: 3,
                col: 6,
                rows: 2,
                cols: 5,
            }),
        );
        let mut inked = stripes(&dst, theme);
        inked.sort_unstable();

        let top: Vec<_> = (5..11).map(|c| (2, c, '▁')).collect();
        let left: Vec<_> = (3..5).map(|r| (r, 5, '▐')).collect();
        let mut expected = top;
        expected.extend(left);
        expected.sort_unstable();
        assert_eq!(
            inked, expected,
            "the mark is the divider row above the pane (reaching one cell left, to \
             the corner) plus the divider column beside it — and nothing else"
        );
        assert_eq!(
            dst.cells[2][4],
            divider_cell(theme),
            "the divider row LEFT of the active pane's corner is a different pane's \
             border and stays plain"
        );
    });
}

#[test]
fn a_pane_with_a_divider_below_and_beside_it_is_inked_on_those_two_sides() {
    with_split_chrome(SplitChrome::Classic, || {
        let theme = Theme::default();
        let mut dst = RenderInput::empty();
        fill_divider_grid(
            &mut dst,
            5,
            11,
            theme,
            Some(ActivePaneMark {
                row: 0,
                col: 0,
                rows: 2,
                cols: 5,
            }),
        );
        let mut inked = stripes(&dst, theme);
        inked.sort_unstable();

        let mut expected: Vec<_> = (0..6).map(|c| (2, c, '▔')).collect();
        expected.extend((0..2).map(|r| (r, 5, '▌')));
        expected.sort_unstable();
        assert_eq!(
            inked, expected,
            "the divider row below the pane is inked along its TOP edge and the \
             column beside it along its LEFT edge — both facing the pane"
        );
        assert!(
            dst.cells[0][..5].iter().all(|cell| cell.ch == ' '),
            "and the pane's own first row is untouched: the mark never enters a \
             rectangle a pane owns"
        );
    });
}

#[test]
fn a_pane_that_fills_the_window_stamps_nothing() {
    with_split_chrome(SplitChrome::Classic, || {
        let theme = Theme::default();
        let mut dst = RenderInput::empty();
        fill_divider_grid(
            &mut dst,
            3,
            7,
            theme,
            ActivePaneMark::resolve(true, true, (0, 0, 3, 7)),
        );
        assert!(
            stripes(&dst, theme).is_empty(),
            "every edge line falls off the grid"
        );
    });
}

#[test]
fn the_setting_and_the_windows_own_focus_each_withhold_the_mark() {
    let rect = (0, 0, 2, 3);
    assert_eq!(
        ActivePaneMark::resolve(false, true, rect),
        None,
        "the setting off marks nothing"
    );
    assert_eq!(
        ActivePaneMark::resolve(true, false, rect),
        None,
        "a window that does not have the keyboard claims no pane"
    );
    assert_eq!(
        ActivePaneMark::resolve(true, true, rect),
        Some(ActivePaneMark {
            row: 0,
            col: 0,
            rows: 2,
            cols: 3
        })
    );
    assert_eq!(
        ActivePaneMark::resolve(true, true, (0, 0, 0, 3)),
        None,
        "a degenerate pane has no edge to hug"
    );
}

#[test]
fn the_repaint_key_term_is_zero_for_no_mark_and_moves_with_the_rectangle() {
    assert_eq!(ActivePaneMark::key_term(None), 0);
    let left = ActivePaneMark::resolve(true, true, (0, 0, 4, 5));
    let right = ActivePaneMark::resolve(true, true, (0, 6, 4, 5));
    let tall = ActivePaneMark::resolve(true, true, (0, 0, 5, 5));
    let wide = ActivePaneMark::resolve(true, true, (0, 0, 4, 6));
    let lower = ActivePaneMark::resolve(true, true, (1, 0, 4, 5));
    let terms = [
        ActivePaneMark::key_term(left),
        ActivePaneMark::key_term(right),
        ActivePaneMark::key_term(tall),
        ActivePaneMark::key_term(wide),
        ActivePaneMark::key_term(lower),
    ];
    for (i, &a) in terms.iter().enumerate() {
        assert_ne!(a, 0);
        for &b in &terms[i + 1..] {
            assert_ne!(a, b, "distinct pane rectangles must pack to distinct keys");
        }
    }
}

#[test]
fn divider_tracks_high_contrast() {
    with_split_chrome(SplitChrome::Classic, || {
        use crate::chrome_band::{ChromePalette, hc_fixtures};
        for name in aterm_types::scheme::builtin_names() {
            let theme = Theme::from(&aterm_types::scheme::builtin(name).expect("a built-in"));
            let bg = crate::settings::u32_rgb(theme.bg);
            let derived = ChromePalette::from(theme).tokens().seam_on(bg);
            assert_eq!(divider_cell(theme).bg, derived, "{name}");
            assert_eq!(divider_cell(theme).fg, derived, "{name}");
            let bordered = ChromePalette {
                authored: aterm_messages::tokens::AuthoredChrome {
                    border: Some([0x5a; 3]),
                    ..aterm_messages::tokens::AuthoredChrome::NONE
                },
                ..ChromePalette::from(theme)
            };
            assert_eq!(
                divider_cell(bordered).bg,
                bordered.tokens().seam_on(bg),
                "{name}"
            );
            assert_ne!(divider_cell(bordered).bg, derived, "{name}: CONTROL");
            for (scheme, hc) in hc_fixtures::STOCK {
                let cell = hc_fixtures::with_forced(hc, || divider_cell(bordered));
                assert_eq!(
                    (cell.bg, cell.fg),
                    (hc.window_text, hc.window_text),
                    "{name} {scheme}"
                );
                assert_ne!(
                    divider_cell(theme).bg,
                    hc.window_text,
                    "{name} {scheme}: CONTROL"
                );
            }
        }
    });
}
