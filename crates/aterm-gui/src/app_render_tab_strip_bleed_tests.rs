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

#[test]
fn composing_a_metered_band_row_hands_the_renderer_its_edge_tones() {
    use aterm_messages::{Hold, Message, Meter, Severity, tags};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 0;
    let live = Hold::Live {
        stale_after: aterm_messages::STALE_UPDATE,
    };
    app.post_message(
        Message::new(tags::UPDATE, Severity::Info, "aterm update v0.99.0")
            .meter(Meter {
                fill_permille: Some(1000),
                ..Meter::default()
            })
            .hold(live),
    );
    app.post_message(Message::new(tags::TOOLCHAIN, Severity::Info, "a plain row").hold(live));
    assert_eq!(app.message_band_rows, 2);
    app.splice_tab_strip_with(wid, 0);
    let bleed = app.backend.chrome_bleed().expect("band rows are chrome");
    assert_eq!(bleed.rows, 2);
    let ws = &app.windows[&wid];
    let cols = usize::from(ws.cols);
    let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    let metered = ws
        .cached_band_edges
        .iter()
        .position(Option::is_some)
        .expect("the update row is metered");
    let row = &ws.cached_band_rows[metered];
    let accent = pack(crate::chrome_band::band_colors(app.chrome_palette_theme()).accent);
    assert_eq!(
        bleed.gutter_tones(metered),
        (pack(row[0].bg), pack(row[cols - 1].bg)),
        "the gutters continue the metered row's own edge cells"
    );
    assert_eq!(
        bleed.gutter_tones(metered),
        (accent, accent),
        "a full meter runs through both gutters in the accent"
    );
    let plain = 1 - metered;
    assert_eq!(ws.cached_band_edges[plain], None);
    assert_eq!(
        bleed.gutter_tones(plain),
        (bleed.color, bleed.color),
        "an unmetered row keeps the band tone"
    );
}

#[test]
fn mixed_frame_retires_meter_rasters_when_the_message_band_disappears() {
    use aterm_messages::{Hold, Message, Meter, Severity, tags};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 0;
    app.windows.get_mut(&wid).unwrap().rows = 40;
    app.windows.get_mut(&wid).unwrap().cols = 140;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    crate::term_lock(&app.pool.get(session).unwrap().term)
        .process(b"\x1b[2J\x1b[Hterminal stays visible");
    let id = app.post_message(
        Message::new(tags::UPDATE, Severity::Info, "a live meter")
            .meter(Meter {
                fill_permille: Some(500),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: aterm_messages::STALE_UPDATE,
            }),
    );
    assert_eq!(app.message_band_rows, 1);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    assert!(
        !app.windows[&wid].input_scratch.chrome_rasters.is_empty(),
        "the fixture paints a real meter before its removal"
    );
    assert!(app.withdraw_message(id));
    app.settle_messages(
        std::time::Instant::now()
            + aterm_messages::SHRINK_QUIET
            + std::time::Duration::from_millis(1),
    );
    assert_eq!(app.message_band_rows, 0);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let input = &app.windows[&wid].input_scratch;
    assert!(
        input.chrome_rasters.is_empty(),
        "the removed meter cannot paint over the next mixed frame"
    );
    assert!(input.cells.iter().any(|row| {
        row.iter()
            .map(|cell| cell.ch)
            .collect::<String>()
            .contains("terminal stays visible")
    }));
    assert!(app.windows[&wid].settings_card.is_some());
}

#[test]
fn a_band_spliced_over_a_metered_row_drops_its_gutter_tones_and_raster() {
    use aterm_messages::{Hold, Message, Meter, Severity, tags};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 0;
    let live = Hold::Live {
        stale_after: aterm_messages::STALE_UPDATE,
    };
    for title in ["aterm update v0.99.0", "aterm update v0.99.1"] {
        app.post_message(
            Message::new(tags::UPDATE, Severity::Info, title)
                .meter(Meter {
                    fill_permille: Some(1000),
                    ..Meter::default()
                })
                .hold(live),
        );
    }
    app.splice_tab_strip_with(wid, 0);
    let bleed = app.backend.chrome_bleed().expect("band rows are chrome");
    let metered: Vec<usize> = bleed.row_edges.iter().flatten().map(|e| e.row).collect();
    assert!(
        metered.len() >= 2,
        "fixture: two metered rows carry tones ({metered:?})"
    );
    let (covered, kept) = (metered[0], metered[1]);
    let input = &app.windows[&wid].input_scratch;
    assert!(input.chrome_raster(covered).is_some());
    let kept_raster = input
        .chrome_raster(kept)
        .expect("second meter raster")
        .clone();
    let cols = usize::from(app.windows[&wid].cols);
    let cell_h = app.win_cell_size(wid).1;
    let blank = crate::message_band::blank_band_row(cols, app.chrome_palette_theme());
    app.splice_band_rows_at(wid, 0, vec![blank; covered + 1], cols, cell_h);
    let after = app.backend.chrome_bleed().expect("still chrome");
    assert_eq!(
        after.gutter_tones(covered),
        (after.color, after.color),
        "the covered row's gutters take the band tone"
    );
    assert_eq!(
        after.gutter_tones(kept),
        bleed.gutter_tones(kept),
        "an uncovered row keeps its own tones"
    );
    let input = &app.windows[&wid].input_scratch;
    assert!(input.chrome_raster(covered).is_none());
    assert_eq!(input.chrome_raster(kept), Some(&kept_raster));
}

#[test]
fn the_band_bleeds_below_a_strip_with_no_surface() {
    use aterm_messages::{Hold, Message, Severity, tags};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.post_message(
        Message::new(tags::UPDATE, Severity::Info, "a band row").hold(Hold::Live {
            stale_after: aterm_messages::STALE_UPDATE,
        }),
    );
    app.splice_tab_strip_with(wid, 1);
    let bleed = app
        .backend
        .chrome_bleed()
        .expect("the band rows are chrome");
    assert_eq!(bleed.rows, 1 + usize::from(app.message_band_rows));
    if tab_bar::strip_bleed_tones(app.chrome_palette_theme()).is_none() {
        assert_eq!(bleed.first, 1, "a strip with no surface keeps the padding");
    } else {
        assert_eq!(
            bleed.first, 0,
            "a strip with a surface bleeds with the band"
        );
    }
}

#[test]
fn band_row_edges_place_each_metered_row_at_its_chrome_row() {
    let edges = [
        Some(([0x11, 0x22, 0x33], [0x44, 0x55, 0x66])),
        None,
        Some(([1, 2, 3], [4, 5, 6])),
    ];
    let got = super::band_row_edges(2, 3, &edges);
    assert_eq!(
        got[0],
        Some(aterm_render::ChromeRowEdges {
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
    let trimmed = super::band_row_edges(0, 1, &edges);
    assert_eq!(trimmed[0].map(|e| e.row), Some(0));
    assert_eq!(
        &trimmed[1..],
        &[None, None],
        "an uncommitted row is not on glass"
    );
}
