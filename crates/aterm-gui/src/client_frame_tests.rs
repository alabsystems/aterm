// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const FRAME: ClientFrame = ClientFrame {
    minimize: true,
    maximize: true,
    window_menu: true,
    maximized: false,
    edges: true,
    header: true,
    layout: ButtonLayout::FALLBACK,
};

#[test]
fn buttons_follow_the_compositor_and_the_maximized_state() {
    let all: Vec<_> = FRAME.buttons().collect();
    assert_eq!(
        all,
        [
            CaptionButton::Minimize,
            CaptionButton::Maximize,
            CaptionButton::Close
        ]
    );
    let maximized = ClientFrame {
        maximized: true,
        ..FRAME
    };
    assert_eq!(
        maximized.buttons().nth(1),
        Some(CaptionButton::Restore),
        "a maximized window's middle button restores"
    );
    let bare = ClientFrame {
        minimize: false,
        maximize: false,
        ..FRAME
    };
    assert_eq!(bare.buttons().collect::<Vec<_>>(), [CaptionButton::Close]);
    let fullscreen = ClientFrame {
        edges: false,
        header: false,
        ..FRAME
    };
    assert_eq!(fullscreen.buttons().count(), 0);
    assert_eq!(
        CaptionSlot::measure(Some(fullscreen), 9, 1.0),
        CaptionSlot::NONE
    );
}

/// sctk-adwaita 0.10.1's own answers (tests/recorder-adwaita): for each
/// `button-layout` value, or none, the two ends its config read split it
/// into, and the buttons each end showed under each set of compositor
/// capabilities.
const RECORDED: &str = include_str!("../tests/recorded/sctk-adwaita-0.10.1-frame.tsv");

fn field<'a>(row: &[&'a str], name: &str) -> &'a str {
    row.iter()
        .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("no {name} in {row:?}"))
}

fn names(buttons: impl Iterator<Item = CaptionButton>) -> String {
    let names: Vec<&str> = buttons
        .map(|b| match b {
            CaptionButton::Minimize => "minimize",
            CaptionButton::Maximize => "maximize",
            CaptionButton::Restore => "restore",
            CaptionButton::Close => "close",
        })
        .collect();
    if names.is_empty() {
        "-".into()
    } else {
        names.join(",")
    }
}

#[test]
fn the_layout_reads_every_value_as_sctk_adwaita_did() {
    let mut rows = 0;
    for line in RECORDED.lines().filter(|l| l.starts_with("layout\t")) {
        let row: Vec<&str> = line.split('\t').collect();
        let value = match field(&row, "value") {
            "<no portal>" => None,
            quoted => {
                let v = quoted
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .expect("a quoted value");
                assert!(!v.contains('\\'), "no escapes in the corpus: {v}");
                Some(v)
            }
        };
        let read = value.and_then(ButtonLayout::sides_of);
        let read_s = read.map_or("-".to_string(), |(l, r)| format!("{l}|{r}"));
        assert_eq!(read_s, field(&row, "read"), "{line}");
        let frame = ClientFrame {
            maximize: field(&row, "maximize") == "1",
            minimize: field(&row, "minimize") == "1",
            layout: value
                .and_then(ButtonLayout::parse)
                .unwrap_or(ButtonLayout::FALLBACK),
            ..FRAME
        };
        assert_eq!(
            names(frame.buttons_on(Side::Left)),
            field(&row, "left"),
            "{line}"
        );
        assert_eq!(
            names(frame.buttons_on(Side::Right)),
            field(&row, "right"),
            "{line}"
        );
        rows += 1;
    }
    assert_eq!(rows, 92, "every recorded row was read");
}

#[test]
fn the_fallback_is_minimize_maximize_close_on_the_right() {
    let gnome = ButtonLayout::parse("appmenu:close").expect("a layout");
    assert_ne!(gnome, ButtonLayout::FALLBACK);
    let frame = ClientFrame {
        layout: gnome,
        ..FRAME
    };
    assert_eq!(names(frame.buttons()), "close");
    assert_eq!(
        names(FRAME.buttons_on(Side::Right)),
        "minimize,maximize,close"
    );
    assert_eq!(names(FRAME.buttons_on(Side::Left)), "-");
    for nothing in [
        "",
        ":",
        "appmenu:",
        "minimize,maximize,close",
        "Close:Minimize",
    ] {
        assert_eq!(ButtonLayout::parse(nothing), None, "{nothing:?}");
    }
}

#[test]
fn a_slot_lays_out_both_ends_and_sheds_from_both() {
    let layout = ButtonLayout::parse("close,minimize:maximize").expect("a layout");
    let frame = ClientFrame { layout, ..FRAME };
    let slot = CaptionSlot::measure(Some(frame), 9, 1.0);
    assert_eq!(names(slot.on(Side::Left)), "close,minimize");
    assert_eq!(names(slot.on(Side::Right)), "maximize");
    assert_eq!(slot.side_cols(Side::Left), 8);
    assert_eq!(slot.side_cols(Side::Right), 4);
    assert_eq!(slot.cols(), 12);
    let fit = slot.fit(14, 6);
    assert_eq!(names(fit.buttons()), "close,maximize");
    assert_eq!(fit.side_cols(Side::Left), 4);
    let fit = slot.fit(10, 6);
    assert_eq!(names(fit.buttons()), "close");
    assert_eq!(slot.fit(4, 6), CaptionSlot::NONE);
    let maximized = ClientFrame {
        maximized: true,
        ..frame
    };
    assert_eq!(
        names(CaptionSlot::measure(Some(maximized), 9, 1.0).on(Side::Right)),
        "restore"
    );
}

#[test]
fn a_window_that_draws_its_own_frame_keeps_a_header_row() {
    assert_eq!(strip_rows_with_header(0, true), 1);
    assert_eq!(strip_rows_with_header(1, true), 1);
    assert_eq!(strip_rows_with_header(3, true), 3);
    assert_eq!(strip_rows_with_header(0, false), 0);
    assert_eq!(strip_rows_with_header(2, false), 2);
}

#[test]
fn a_slot_takes_whole_cells_at_every_scale() {
    for (cell_w, scale, cols) in [(9, 1.0, 4), (14, 1.5, 4), (18, 2.0, 4), (20, 1.0, 2)] {
        let slot = CaptionSlot::measure(Some(FRAME), cell_w, scale);
        assert_eq!(slot.button_cols(), cols, "cell {cell_w} px at {scale}");
        assert!(
            usize::from(slot.button_cols()) * cell_w
                >= (BUTTON_SLOT_LOGICAL_PX * scale).round() as usize,
            "the slot holds the button and its spacing"
        );
        assert_eq!(slot.cols(), 3 * cols);
    }
    assert_eq!(CaptionSlot::measure(None, 9, 1.0), CaptionSlot::NONE);
    assert_eq!(CaptionSlot::measure(Some(FRAME), 0, 1.0), CaptionSlot::NONE);
    assert_eq!(CaptionSlot::NONE.cols(), 0);
}

#[test]
fn a_narrow_strip_sheds_minimize_then_maximize_and_keeps_close() {
    let slot = CaptionSlot::measure(Some(FRAME), 9, 1.0);
    assert_eq!(slot.fit(80, 6), slot, "room for all three");
    let fit = slot.fit(14, 6);
    assert_eq!(
        fit.buttons().collect::<Vec<_>>(),
        [CaptionButton::Maximize, CaptionButton::Close]
    );
    let fit = slot.fit(10, 6);
    assert_eq!(fit.buttons().collect::<Vec<_>>(), [CaptionButton::Close]);
    assert_eq!(
        slot.fit(5, 6).buttons().collect::<Vec<_>>(),
        [CaptionButton::Close]
    );
    assert_eq!(slot.fit(4, 6), CaptionSlot::NONE);
}

/// The margins of the default 744×528 test window: a 12 px pad around an
/// 80×24 grid of 9×21 px cells, the strip and its band above it.
const PADDED: FrameMargins = FrameMargins {
    left: 12.0,
    top: 23.0,
    right: 12.0,
    bottom: 12.0,
};

#[test]
fn edges_and_corners_resize_and_the_interior_does_not() {
    let (w, h) = (744.0, 528.0);
    let at = |x, y| edge_at(x, y, w, h, 1.0, PADDED);
    assert_eq!(at(0.0, 300.0), Some(FrameEdge::West));
    assert_eq!(at(743.0, 300.0), Some(FrameEdge::East));
    assert_eq!(at(300.0, 0.0), Some(FrameEdge::North));
    assert_eq!(at(300.0, 527.0), Some(FrameEdge::South));
    assert_eq!(at(1.0, 1.0), Some(FrameEdge::NorthWest));
    assert_eq!(at(12.0, 1.0), Some(FrameEdge::NorthWest));
    assert_eq!(at(1.0, 12.0), Some(FrameEdge::NorthWest));
    assert_eq!(at(740.0, 520.0), Some(FrameEdge::SouthEast));
    assert_eq!(at(735.0, 2.0), Some(FrameEdge::NorthEast));
    assert_eq!(at(3.0, 520.0), Some(FrameEdge::SouthWest));
    assert_eq!(at(300.0, 300.0), None);
    assert_eq!(at(12.0, 300.0), None);
    assert_eq!(at(300.0, 4.0), None);
    assert_eq!(at(-1.0, 300.0), None);
    assert_eq!(at(744.0, 300.0), None);
    assert_eq!(at(f64::NAN, 300.0), None);
}

#[test]
fn no_handle_reaches_past_the_padding_into_the_grid() {
    let (w, h) = (720.0, 525.0);
    let flush = FrameMargins::around_grid(w, h, 0.0, 21.0, 720.0, 525.0);
    assert_eq!(
        flush,
        FrameMargins {
            left: 0.0,
            top: 21.0,
            right: 0.0,
            bottom: 0.0,
        }
    );
    let at = |x, y| edge_at(x, y, w, h, 1.0, flush);
    assert_eq!(at(0.0, 300.0), None);
    assert_eq!(at(719.5, 300.0), None);
    assert_eq!(at(300.0, 524.5), None);
    assert_eq!(at(0.0, 524.0), None, "the bottom-left cell");
    assert_eq!(at(300.0, 1.0), Some(FrameEdge::North));
    assert_eq!(at(0.0, 1.0), Some(FrameEdge::NorthWest));
    let thin = FrameMargins::around_grid(724.0, 527.0, 2.0, 21.0, 722.0, 525.0);
    let at = |x, y| edge_at(x, y, 724.0, 527.0, 1.0, thin);
    assert_eq!(at(1.5, 300.0), Some(FrameEdge::West));
    assert_eq!(at(2.0, 300.0), None);
    assert_eq!(at(722.0, 300.0), Some(FrameEdge::East));
    assert_eq!(at(721.5, 300.0), None);
    assert_eq!(at(300.0, 525.0), Some(FrameEdge::South));
    assert_eq!(at(300.0, 524.5), None);
    assert_eq!(
        edge_at(5.5, 300.0, 744.0, 528.0, 1.0, PADDED),
        Some(FrameEdge::West)
    );
    assert_eq!(
        FrameMargins::around_grid(700.0, 500.0, -10.0, 21.0, 710.0, f64::NAN),
        FrameMargins {
            left: 0.0,
            top: 21.0,
            right: 0.0,
            bottom: 0.0,
        }
    );
}

#[test]
fn the_handles_scale_with_the_window() {
    let wide = FrameMargins {
        left: 24.0,
        top: 46.0,
        right: 24.0,
        bottom: 24.0,
    };
    assert_eq!(
        edge_at(11.0, 300.0, 1488.0, 1056.0, 2.0, wide),
        Some(FrameEdge::West)
    );
    assert_eq!(edge_at(13.0, 300.0, 1488.0, 1056.0, 2.0, wide), None);
    assert_eq!(edge_at(11.0, 300.0, 1488.0, 1056.0, 1.0, wide), None);
}

#[test]
fn each_handle_shows_its_own_cursor() {
    let edges = [
        FrameEdge::North,
        FrameEdge::NorthEast,
        FrameEdge::East,
        FrameEdge::SouthEast,
        FrameEdge::South,
        FrameEdge::SouthWest,
        FrameEdge::West,
        FrameEdge::NorthWest,
    ];
    let icons: std::collections::HashSet<_> = edges.into_iter().map(FrameEdge::cursor).collect();
    assert_eq!(icons.len(), edges.len());
    assert_eq!(FrameEdge::North.cursor(), CursorIcon::NResize);
    assert_eq!(FrameEdge::SouthWest.cursor(), CursorIcon::SwResize);
}
