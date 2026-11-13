// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::tab_bar::TabHit;

fn compose(app: &mut App, wid: WindowId) {
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (ws.rows, ws.cols)
    };
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        let mut term = term_lock(&terminal);
        term.resize(rows, cols);
        term.cell_frame_into(&mut ws.input_scratch, usize::from(rows), usize::from(cols));
    }
    app.splice_tab_strip(wid);
}

fn present_raster(app: &mut App, wid: WindowId) {
    let App {
        windows, backend, ..
    } = app;
    let ws = windows.get(&wid).expect("window");
    let Backend::Cpu(renderer) = backend.ready_mut() else {
        panic!("headless test backend is CPU")
    };
    let _ = renderer.render_input(&ws.input_scratch);
}

fn band_covers(app: &App, wid: WindowId, col: usize) -> bool {
    let band = &app.windows[&wid].cached_strip_band;
    let Some(row) = band.first() else {
        return false;
    };
    let Some((_, first)) = row.first() else {
        return false;
    };
    row.iter()
        .any(|(c, r)| *c == col && Arc::ptr_eq(&r.image, &first.image))
}

fn finalize(app: &mut App, wid: WindowId) {
    let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
    app.finalize_successful_present(
        wid,
        crate::metrics::StartupPresentTiming::collapsed(Instant::now()),
        0,
        None,
        SuccessfulPresentRoute::Terminal,
        crate::VisibleContentRoute::Terminal { composed: false },
        HostVisualState::default(),
        None,
        &plan,
    );
}

#[test]
fn a_cjk_title_is_a_band_title_on_its_first_frame_and_asks_one_frame_more() {
    let Some(path) = crate::tray_raster::cjk_chain_face_path_for_test() else {
        aterm_spec::host::could_not_run("no CJK chain face here");
    };
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.first_present_done = true;
    let session = crate::stub_session(app.next_session_id);
    app.push_stub_tab(wid, session);
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut term = term_lock(&terminal);
        term.process("\x1b[2 q\x1b]0;日本語 shell\x07".as_bytes());
        assert!(
            !crate::blink_clock_armed(term.cursor_visible(), term.cursor_style(), false),
            "a steady cursor: the blink clock is not armed"
        );
    }
    let seed = {
        let App {
            backend,
            font_px,
            theme,
            ..
        } = &mut app;
        let Backend::Cpu(renderer) = backend.ready_mut() else {
            panic!("headless test backend is CPU")
        };
        renderer.set_runtime_font_discovery(false);
        renderer.set_config_fallback_fonts(&[path.to_string()]);
        let _ = renderer.seal_admitted_font_sources();
        renderer
            .fork_semantic_surface(*font_px, *theme)
            .expect("a sealed generation forks")
    };
    crate::tray_raster::install_dormant_chrome_fonts_for_test(seed);
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    if !crate::tray_raster::strip_band_ui_ready() {
        crate::tray_raster::clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }

    assert!(
        !crate::tray_raster::chrome_cascade_reaches_for_test('日'),
        "CONTROL: nothing has parsed the CJK face before frame 1"
    );
    compose(&mut app, wid);
    let seg = app.windows[&wid]
        .tab_segments
        .iter()
        .find(|s| matches!(s.kind, TabHit::Select(1)))
        .copied()
        .expect("the CJK tab's segment");
    let probe = usize::from(seg.start_col) + 1;
    let first_key = app.windows[&wid].strip_band_key;
    assert!(
        band_covers(&app, wid, probe),
        "frame 1: the CJK segment is band-covered on its first frame"
    );
    present_raster(&mut app, wid);
    {
        let key = crate::early_out_tests::frame_key(&mut term_lock(&terminal), false, false, None);
        app.windows
            .get_mut(&wid)
            .expect("window")
            .stamp_present_decision(key);
    }
    assert!(app.windows[&wid].last_present.is_some());
    finalize(&mut app, wid);
    assert!(
        app.windows[&wid].last_present.is_none(),
        "the follow-up cleared the retained key after the first present"
    );
    let mut requests = 0;
    assert!(app.rearm_for_strip_band_landing(wid, || requests += 1));
    assert_eq!(
        requests, 1,
        "a redraw was requested after the first present"
    );

    compose(&mut app, wid);
    let second_key = app.windows[&wid].strip_band_key;
    assert_eq!(
        (first_key.0, first_key.1, first_key.2, first_key.3),
        (second_key.0, second_key.1, second_key.2, second_key.3),
        "the geometry terms did not move"
    );
    assert_ne!(
        first_key.4, second_key.4,
        "the landing moved the band key's epoch term"
    );
    assert!(
        band_covers(&app, wid, probe),
        "frame 2: the CJK segment is band-covered"
    );
    let mut requests = 0;
    assert!(!app.rearm_for_strip_band_landing(wid, || requests += 1));
    assert_eq!(requests, 0, "a settled band requests no frame");
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn no_landing_no_rearm() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let renderer = aterm_render::Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    crate::tray_raster::install_dormant_chrome_fonts_for_test(renderer);
    let mut requests = 0;
    app.tab_strip_rows = 0;
    assert!(
        !app.rearm_for_strip_band_landing(wid, || requests += 1),
        "no strip: nothing to land"
    );
    app.tab_strip_rows = 1;
    compose(&mut app, wid);
    assert!(
        !app.rearm_for_strip_band_landing(wid, || requests += 1),
        "a settled band's re-read is not a landing"
    );
    assert_eq!(requests, 0);
    crate::tray_raster::clear_ui_fonts_for_test();
}
