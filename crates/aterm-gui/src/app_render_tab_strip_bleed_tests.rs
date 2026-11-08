// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, tab_bar};

#[test]
fn composing_a_strip_hands_the_renderer_its_band_and_seam_for_the_padding() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.splice_tab_strip_with(wid, 1);
    let want = tab_bar::strip_bleed_tones(app.chrome_palette_theme());
    match (app.backend.chrome_bleed(), want) {
        (Some(bleed), Some((color, seam))) => {
            assert_eq!(bleed.rows, 1, "exactly the strip's rows are chrome");
            assert_eq!(bleed.color, color, "…in the strip's OWN band tone");
            assert_eq!(bleed.seam, seam, "…and the seam continues into the gutters");
            assert_ne!(
                bleed.color,
                app.theme.bg & 0x00ff_ffff,
                "a band that equals the terminal background is not a band"
            );
        }
        (got, None) => assert_eq!(got, None, "no band ⇒ no bleed"),
        (None, Some(_)) => panic!("a chrome band was never handed to the renderer"),
    }

    app.tab_strip_rows = 0;
    app.splice_tab_strip_with(wid, 1);
    assert_eq!(app.backend.chrome_bleed(), None, "no strip ⇒ no bleed");
}

#[test]
fn composing_a_strip_rails_its_seam_on_the_bands_last_pixel_rows() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.splice_tab_strip_with(wid, 1);
    let Some((_, Some(seam))) = tab_bar::strip_bleed_tones(app.chrome_palette_theme()) else {
        return;
    };
    let ws = &app.windows[&wid];
    if !tab_bar::STRIP_WINDOWS_PAINT {
        assert!(
            ws.input_scratch
                .chrome_rasters
                .iter()
                .all(|raster| raster.row != 0 || raster.rail.is_empty()),
            "no rail on Linux's strip"
        );
        let seam = [(seam >> 16) as u8, (seam >> 8) as u8, seam as u8];
        assert!(
            ws.input_scratch.cells[0].iter().all(|cell| {
                cell.underline == aterm_core::terminal::UnderlineStyle::Single
                    && cell.underline_color == Some(seam)
            }),
            "every strip cell carries the underline seam"
        );
        return;
    }
    let rail = ws
        .input_scratch
        .chrome_rasters
        .iter()
        .find(|raster| raster.row == 0)
        .expect("the strip's last row carries the seam rail");
    let (cell_w, _) = app.win_cell_size(wid);
    let frame_w = usize::from(ws.cols) * cell_w + 2 * app.win_pad(wid);
    assert_eq!(rail.rail.len(), frame_w, "edge to edge, gutters included");
    assert!(
        rail.rail.iter().all(|&tone| tone == seam),
        "in the seam tone"
    );
    assert_eq!(
        usize::from(rail.rail_h),
        tab_bar::strip_seam_px(ws.scale as f32)
    );
    assert!(
        aterm_render::chrome_rail_covers_gutters(Some(rail)),
        "the rail retires the bleed's gutter seam for this row"
    );
    assert!(
        ws.input_scratch.cells[0]
            .iter()
            .all(|cell| cell.underline == aterm_core::terminal::UnderlineStyle::None),
        "no strip cell draws the old underline seam"
    );
}

/// Where the strip carries the version label (Windows), the opt-in build badge's
/// top-right anchor sits below the strip's rows instead of on the label, and moves with
/// their count.
#[cfg(windows)]
#[test]
fn the_build_badge_sits_below_the_strip() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.show_build_badge = Some(true);
    let (_, ch) = app.win_cell_size(wid);
    let base = (app.win_pad_top(wid) + app.win_head(wid)) as u32;
    let dy = |app: &mut App, rows: u16| {
        app.tab_strip_rows = rows;
        app.splice_build_badge(wid);
        app.windows[&wid]
            .badge_card
            .as_ref()
            .map(|card| card.dy)
            .expect("the badge is on")
    };
    let without = dy(&mut app, 0);
    let one = dy(&mut app, 1);
    assert!(
        one >= base + ch as u32,
        "the badge starts below the strip row: dy {one} < {base} + {ch}"
    );
    assert_eq!(one - without, ch as u32, "exactly one strip row lower");
    let two = dy(&mut app, 2);
    assert_eq!(two - one, ch as u32, "…and it follows the strip's rows");
}
