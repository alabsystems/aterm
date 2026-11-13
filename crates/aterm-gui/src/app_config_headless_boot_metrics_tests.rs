// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn asymmetric_vertical_padding_round_trips_resize_with_remainder() {
    use aterm_window::dpi::PhysicalSize;

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 0;
    app.backend.set_pad(12);
    app.backend.set_pad_top(2);
    app.backend.set_head(5);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.metrics.pad = 12;
        ws.metrics.pad_top = 2;
        ws.metrics.head = 5;
    }
    let (cw, ch) = app.win_cell_size(wid);
    let (rows, cols) = (24usize, 80usize);
    let exact = PhysicalSize::new((cols * cw + 24) as u32, (rows * ch + 5 + 2 + 12) as u32);
    assert_eq!(app.grid_dims_for(wid, exact), (24, 80));

    let remainder = ch.saturating_sub(1);
    let with_remainder = PhysicalSize::new(exact.width, exact.height + remainder as u32);
    assert_eq!(app.grid_dims_for(wid, with_remainder), (24, 80));

    let old_symmetric_rows =
        aterm_render::pad_split((exact.height as usize).saturating_sub(5), 12, ch).cells;
    assert!(
        old_symmetric_rows < rows,
        "negative control: conserving 2*pad cannot invert the shorter visible frame"
    );
}

#[test]
fn headless_boot_band_survives_a_delta_config_reload() {
    let mut app = crate::App::headless_for_test();
    app.backend.set_pad(12);
    app.backend.set_head(60);
    app.seed_headless_boot_metrics();
    let armed = app.backend.frame_size(24, 80);
    app.refresh_all_window_metrics();
    let ws = app.windows.get(&crate::WindowId(0)).expect("window 0");
    assert_eq!(ws.metrics.head, 60, "the record keeps the sealed band");
    assert_eq!(ws.metrics.pad, 12, "the record keeps the sealed pad");
    assert_eq!(
        ws.metrics.pad_top, 12,
        "the record keeps the sealed symmetric top origin"
    );
    app.apply_window_scale(crate::WindowId(0));
    assert_eq!(app.backend.head(), 60, "the band survives the reload");
    assert_eq!(app.backend.pad(), 12, "the boot pad survives the reload");
    assert_eq!(
        app.backend.pad_top(),
        12,
        "the top origin survives the reload"
    );
    assert_eq!(
        app.backend.frame_size(24, 80),
        armed,
        "the first present's frame still matches the armed tap geometry"
    );
}
