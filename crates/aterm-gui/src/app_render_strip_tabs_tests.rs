// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What lies under the strip's seam ([`tab_bar::StripFloor`]), as the strip
//! splice reads it from the frame's state: the selected tab opens into the
//! grid, a split's headers and a native page, and stands closed over a band.

use crate::{App, WindowId, tab_bar, term_lock};

/// Fill the window's render scratch from the engine, as the redraw does before
/// its splices.
fn fill_scratch(app: &mut App, wid: WindowId) {
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (ws.rows as usize, ws.cols as usize)
    };
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let ws = app.windows.get_mut(&wid).expect("window");
    let mut term = term_lock(&terminal);
    term.cell_frame_into(&mut ws.input_scratch, rows, cols);
}

/// Start a find for `query` the way typing does, and run it.
fn find(app: &mut App, wid: WindowId, query: &str) {
    app.search_enter();
    app.windows
        .get_mut(&wid)
        .expect("window")
        .search
        .as_mut()
        .expect("find is open")
        .set_query(query.to_string());
    app.search_recompute();
}

/// THE FLOOR FOLLOWS THE FRAME (Windows tabs): an unsplit terminal is the
/// ground; the find panel on the top row and the message band are the band's
/// tone, which the selected tab cannot take; a split's pane headers wear the
/// ground (the Fluent chrome), so the tab opens into them; Settings in front is
/// its page's surface. NEGATIVE CONTROL: the 2026-10-18 splice read no floor,
/// and the selected tab was the ground over all of them (the judges'
/// `selected-tab-opens-onto-grey-band`, `settings-tab-ground-mismatch`).
#[test]
fn the_strip_floor_is_what_the_frame_puts_under_the_seam() {
    if !tab_bar::SELECTED_TAB_IS_GROUND {
        return;
    }
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let header = app.chrome_palette(crate::app_render::ChromeSurface::Header);
    let band = crate::chrome_band::band_colors(header).bar_bg;
    let ground = crate::settings::u32_rgb(header.theme.bg);

    assert_eq!(app.strip_floor(wid, 0, header), tab_bar::StripFloor::Ground);
    assert_eq!(
        app.strip_floor(wid, 1, header),
        tab_bar::StripFloor::Surface(band),
        "the message band under the strip"
    );

    // The find panel on the top row (its match sits clear of it).
    term_lock(&app.pool.get(0).expect("session 0").term).process(b"\r\n\r\n\r\n\r\nhit\r\n");
    find(&mut app, wid, "hit");
    fill_scratch(&mut app, wid);
    let panel = crate::chrome_band::band_colors(
        app.chrome_palette(crate::app_render::ChromeSurface::OverGrid(wid)),
    )
    .bar_bg;
    assert_eq!(
        app.strip_floor(wid, 0, header),
        tab_bar::StripFloor::Surface(panel),
        "the find panel under the strip"
    );
    app.search_cancel();
    assert_eq!(app.strip_floor(wid, 0, header), tab_bar::StripFloor::Ground);

    // A split: its headers wear the ground, so the tab opens into them. The
    // floor is read off the tab's split tree: the frame's one plan stays one.
    app.split_active_stub_tab(wid);
    crate::reset_visible_leaf_plan_builds();
    let floor = app.strip_floor(wid, 0, header);
    assert_eq!(
        crate::visible_leaf_plan_builds(),
        0,
        "the floor plans nothing"
    );
    assert_eq!(
        floor,
        tab_bar::StripFloor::Surface(ground),
        "a split's headers wear the terminal's ground"
    );

    // Settings in front: its page's surface, which stands apart from the band.
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let page = crate::settings::Roles::from_theme(
        app.chrome_palette(crate::app_render::ChromeSurface::NativePage),
    )
    .surface;
    assert_eq!(
        app.strip_floor(wid, 0, header),
        tab_bar::StripFloor::Surface(page),
        "Settings in front"
    );
}

/// The strip splice's reading of the find panel ([`App::find_panel_meets_strip`])
/// agrees with where `splice_find_bar` then paints it: on the top row while the
/// current match is clear of it, floated to the bottom while the match sits
/// under it (the strip then opens onto the grid again).
#[test]
fn find_panel_placement_matches_the_strip_floor() {
    for (output, top) in [
        (&b"\r\n\r\n\r\n\r\nhit one\r\nhit two\r\n"[..], true),
        (&b"hit one\r\nhit two\r\n"[..], false),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        term_lock(&app.pool.get(0).expect("session 0").term).process(output);
        find(&mut app, wid, "hit");
        fill_scratch(&mut app, wid);
        let meets = app.find_panel_meets_strip(wid);
        app.splice_tab_strip_with(wid, 1);
        app.splice_find_bar(wid);
        let band = app.windows[&wid]
            .find_bar_hit
            .as_ref()
            .expect("the panel recorded its geometry")
            .band
            .clone();
        assert_eq!(band.start == 0, top, "the panel's placement: {band:?}");
        assert_eq!(
            meets, top,
            "the strip read the panel on the top row as {meets}, the splice painted {band:?}"
        );
    }
}

/// The floor is a strip cache term: opening the find panel under an unchanged
/// strip rebuilds the band, which closes the seam under the selected tab.
/// NEGATIVE CONTROL: a cache keyed without the floor kept the open tab over the
/// panel until the next title or hover change.
#[cfg(any(windows, target_os = "linux"))]
#[test]
fn a_floor_change_rebuilds_the_strip_band() {
    if !tab_bar::SELECTED_TAB_IS_GROUND {
        return;
    }
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    if !crate::tray_raster::strip_band_ui_ready() {
        return;
    }
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    term_lock(&app.pool.get(0).expect("session 0").term).process(b"\r\n\r\n\r\n\r\nhit\r\n");
    fill_scratch(&mut app, wid);
    let under_the_tab = |app: &App| {
        let ws = &app.windows[&wid];
        let tab = ws
            .tab_segments
            .iter()
            .find(|seg| matches!(seg.kind, tab_bar::TabHit::Select(0)))
            .expect("the tab");
        let refs = ws.cached_strip_band.first().expect("the band rastered");
        let image = &refs.first().expect("a covered cell").1.image;
        let aterm_core::grid::extra::ImageFormat::RawRgba8 { width, height } = image.format else {
            panic!("the band is RawRgba8");
        };
        let (cell_w, _) = app.win_cell_size(wid);
        let x = usize::from(tab.start_col) * cell_w + cell_w * 2;
        let at = (usize::from(height - 1) * usize::from(width) + x) * 4;
        [image.bytes[at], image.bytes[at + 1], image.bytes[at + 2]]
    };
    app.splice_tab_strip_with(wid, 1);
    let open = under_the_tab(&app);
    find(&mut app, wid, "hit");
    fill_scratch(&mut app, wid);
    app.splice_tab_strip_with(wid, 1);
    let closed = under_the_tab(&app);
    assert_ne!(
        open, closed,
        "the band under the selected tab must change when the find panel opens under it"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}
