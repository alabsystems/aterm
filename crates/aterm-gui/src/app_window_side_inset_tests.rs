// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(not(unix))]
use crate::scripted_redraw_fixture::scripted_app;
use crate::scripted_redraw_fixture::{feed, glass};
use crate::{App, WindowId};
use aterm_window::dpi::PhysicalSize;
use aterm_window::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase};

const WID: WindowId = WindowId(0);

#[test]
fn a_side_inset_drops_exactly_its_columns_and_moves_no_cell() {
    let mut app = App::headless_for_test();
    app.backend.set_pad(12);
    {
        let mut ws = app.windows.get_mut(&WID).unwrap();
        ws.metrics.pad = 12;
        ws.metrics.pad_top = 12;
    }
    let (rows, cols) = (24, 80);
    let (cw, ch) = app.win_cell_size(WID);
    assert!(cw > 1);
    let base = app.window_frame_px_for(WID, rows, cols);
    let raw = PhysicalSize::new(base.width + (cw - 1) as u32, base.height);
    let dims = app.grid_dims_for(WID, raw);
    let program = app.front_terminal(WID).unwrap().term.clone();
    let program_grid = {
        let t = crate::term_lock(&program);
        (t.rows(), t.cols())
    };
    let least = app.least_window_size(WID);
    let headless = headless_surface(&app, (base.width as usize, base.height as usize));
    app.windows.get_mut(&WID).unwrap().win_px = Some(base);
    let origin = app.frame_origin(WID);
    let pad = app.win_pad(WID);
    let top = app.win_head(WID) + app.win_pad_top(WID) + usize::from(app.chrome_rows(WID)) * ch;
    let samples: Vec<_> = [(0, 0), (12, 40), (23, 79)]
        .into_iter()
        .map(|(r, c)| {
            let x = origin.0 as f64 + (pad + c * cw + cw / 2) as f64;
            let y = origin.1 as f64 + (top + r * ch + ch / 2) as f64;
            (x, y, app.pixel_to_cell(WID, x, y))
        })
        .collect();
    for k in [1u16, 3, 30] {
        app.set_side_inset_for_test(WID, k);
        let inset = usize::from(k) * cw;
        assert_eq!(app.win_side_inset_px(WID), inset);
        {
            let t = crate::term_lock(&program);
            assert_eq!(
                (t.rows(), t.cols()),
                program_grid,
                "the program saw no resize"
            );
        }
        assert_eq!(app.grid_dims_for(WID, raw), (dims.0, dims.1 - k));
        let p0 = aterm_render::pad_split(raw.width as usize, pad, cw);
        let pk = aterm_render::pad_split(raw.width as usize - inset, pad, cw);
        assert_eq!((p0.pad_lo, p0.pad_hi), (pk.pad_lo, pk.pad_hi));
        let grown = app.window_frame_px_for(WID, rows, cols);
        assert_eq!(
            grown,
            PhysicalSize::new(base.width + inset as u32, base.height)
        );
        assert_eq!(app.grid_dims_for(WID, grown), (rows, cols));
        let min = app.least_window_size(WID);
        assert_eq!(
            min,
            PhysicalSize::new(least.width + inset as u32, least.height)
        );
        assert_eq!(
            headless_surface(&app, (base.width as usize, base.height as usize)),
            (headless.0 + inset, headless.1)
        );
        app.windows.get_mut(&WID).unwrap().win_px = Some(grown);
        assert_eq!(app.frame_origin(WID), (origin.0 + inset as i64, origin.1));
        for &(x, y, cell) in &samples {
            assert_eq!(app.pixel_to_cell(WID, x + inset as f64, y), cell);
        }
        assert_eq!(
            (app.windows[&WID].rows, app.windows[&WID].cols),
            (rows, cols),
            "the setter never re-grids a tab"
        );
    }
    app.set_side_inset_for_test(WID, 0);
    app.windows.get_mut(&WID).unwrap().win_px = Some(base);
    assert_eq!(app.window_frame_px_for(WID, rows, cols), base);
    assert_eq!(app.grid_dims_for(WID, raw), dims);
    assert_eq!(app.least_window_size(WID), least);
    assert_eq!(
        headless_surface(&app, (base.width as usize, base.height as usize)),
        headless
    );
    assert_eq!(app.frame_origin(WID), origin);
    app.set_side_inset_for_test(WID, 3);
    app.set_font_size(app.font_px + 2.0);
    let (new_cw, _) = app.win_cell_size(WID);
    assert_eq!(app.win_side_inset_px(WID), 3 * new_cw);
    let size = app.window_frame_px_for(WID, rows, cols);
    let with = app.grid_dims_for(WID, size);
    app.set_side_inset_for_test(WID, 0);
    let without = app.grid_dims_for(WID, size);
    assert_eq!(with, (without.0, without.1 - 3));
    app.windows.get_mut(&WID).unwrap().win_px = None;
    let ready = std::mem::replace(&mut app.backend, crate::BackendSlot::Pending(None));
    let startup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(app.win_side_inset_px(WID), 0);
        assert_eq!(app.frame_origin(WID), (0, 0));
    }));
    app.backend = ready;
    assert!(
        startup.is_ok(),
        "product-zero geometry must not read a pending backend"
    );
}

#[test]
fn the_pointer_and_the_picture_agree_with_a_side_inset() {
    #[cfg(unix)]
    let (mut wire, sink) = observer();
    #[cfg(unix)]
    let mut app = crate::scripted_redraw_fixture::scripted_app_with_sink(sink);
    #[cfg(not(unix))]
    let mut app = scripted_app();
    feed(
        &app,
        b"\x1b]11;#102030\x07inset picture\r\nsecond line\x1b[?25l",
    );
    let (rows, cols) = (app.windows[&WID].rows, app.windows[&WID].cols);
    let frame = app.window_frame_px_for(WID, rows, cols);
    let (cw, ch) = app.win_cell_size(WID);
    let inset = 4 * cw;
    let zero_size = PhysicalSize::new(frame.width + 3, frame.height + 2);
    app.windows.get_mut(&WID).unwrap().win_px = Some(zero_size);
    app.redraw_window(WID);
    let zero_glass = glass(&mut app);
    app.set_side_inset_for_test(WID, 4);
    let size = PhysicalSize::new(frame.width + inset as u32 + 3, frame.height + 2);
    app.windows.get_mut(&WID).unwrap().win_px = Some(size);
    app.redraw_window(WID);
    let ((dw, dh), pixels) = glass(&mut app);
    assert_eq!((dw, dh), (size.width as usize, size.height as usize));
    let x0 =
        aterm_render::band_offset_after(dw, frame.width as usize, aterm_render::SideInsetPx(inset));
    let y0 = aterm_render::band_offset_y(dh, frame.height as usize);
    assert_eq!(app.frame_origin(WID), (x0, y0));
    assert!(x0 >= inset as i64);
    let sid = app.focused_session_id(WID).unwrap();
    let dims = app.dims_snapshot(sid, rows.into(), cols.into());
    assert_eq!(dims.offset_x, x0);
    assert_eq!(dims.band_left, x0 as u32);
    let ((fw, fh, rgba), _, _) = image_meta(&mut app, false);
    assert_eq!((fw, fh), (frame.width as usize, frame.height as usize));
    assert_eq!(
        rgba,
        glass_rgba(&mut app, (x0 as usize, y0 as usize, fw, fh))
    );
    let edge_rows = match app.backend.ready_mut() {
        crate::Backend::Cpu(r) => r.chrome_extent_px(fh),
        _ => panic!("CPU fixture"),
    };
    for y in 0..dh {
        let source_y = usize::try_from(y as i64 - y0).ok().filter(|&sy| sy < fh);
        let expected = if source_y.is_some_and(|sy| edge_rows.contains(&sy)) {
            pixels[y * dw + x0 as usize]
        } else {
            0x102030
        };
        assert!(
            pixels[y * dw..y * dw + inset]
                .iter()
                .all(|&p| p == expected),
            "inset surface row {y}: bands outside chrome, continued edges inside chrome"
        );
    }
    let pad = app.win_pad(WID);
    let top = app.win_head(WID) + app.win_pad_top(WID) + usize::from(app.chrome_rows(WID)) * ch;
    let ((clean_w, clean_h, independent), phase, _) = image_meta(&mut app, true);
    assert_eq!(
        phase, "presented",
        "clean rerender retains its successful present serial"
    );
    assert_eq!((clean_w, clean_h), (fw, fh));
    for (r, c) in [(0, 0), (rows / 2, cols / 2), (rows - 1, cols - 1)] {
        let x = pad + usize::from(c) * cw + cw / 2;
        let y = top + usize::from(r) * ch + ch / 2;
        assert_eq!(
            app.pixel_to_cell(WID, x0 as f64 + x as f64, y0 as f64 + y as f64),
            (r, c)
        );
        let pixel = pixels[(y0 as usize + y) * dw + x0 as usize + x];
        assert_eq!(
            [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8, 255],
            independent[(y * fw + x) * 4..(y * fw + x) * 4 + 4]
        );
    }
    // With the surface unchanged, a placement change must repaint the cached frame.
    app.set_side_inset_for_test(WID, 2);
    app.redraw_window(WID);
    let moved = app.frame_origin(WID);
    assert_eq!(
        rgba,
        glass_rgba(&mut app, (moved.0 as usize, moved.1 as usize, fw, fh))
    );
    app.set_side_inset_for_test(WID, 0);
    app.windows.get_mut(&WID).unwrap().win_px = Some(zero_size);
    app.redraw_window(WID);
    assert_eq!(
        glass(&mut app),
        zero_glass,
        "returning to zero reproduces every surface pixel"
    );
    app.set_side_inset_for_test(WID, 4);
    app.windows.get_mut(&WID).unwrap().win_px = Some(size);
    app.redraw_window(WID);
    app.on_cursor_moved(WID, (inset / 2) as f64, (top + ch / 2) as f64);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert!(
        !app.windows[&WID].selecting,
        "a blank inset cannot select column zero"
    );
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    feed(&app, b"\x1b[?1000h\x1b[?1006h");
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert_eq!(
        app.windows[&WID].reported_buttons, 0,
        "the program received no press"
    );
    app.on_mouse_wheel_phased(
        WID,
        MouseScrollDelta::LineDelta(0.0, 3.0),
        TouchPhase::Moved,
    );
    assert_eq!(app.windows[&WID].reported_buttons, 0);
    #[cfg(unix)]
    {
        assert_eq!(
            written_before_marker(&mut app, sid, &mut wire),
            "",
            "neither press nor wheel writes to the program"
        );
        app.on_cursor_moved(
            WID,
            x0 as f64 + (pad + cw / 2) as f64,
            y0 as f64 + (top + ch / 2) as f64,
        );
        app.on_mouse_wheel_phased(
            WID,
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
        );
        assert!(
            written_before_marker(&mut app, sid, &mut wire).contains("\x1b[<"),
            "the same wheel in the pane reaches the mouse-reporting program"
        );
        app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
        assert_ne!(app.windows[&WID].reported_buttons, 0);
        app.on_cursor_moved(WID, (inset / 2) as f64, (top + ch / 2) as f64);
        app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
        assert_eq!(
            app.windows[&WID].reported_buttons, 0,
            "a release in the inset still ends its pane's press"
        );
        assert!(written_before_marker(&mut app, sid, &mut wire).contains("\x1b[<"));
    }
    app.windows.get_mut(&WID).unwrap().win_px = Some(PhysicalSize::new(
        frame.width + (2 * cw) as u32,
        frame.height,
    ));
    app.windows.get_mut(&WID).unwrap().lens_card = Some(crate::SettingsCard {
        rgba: vec![],
        pw: cw as u32,
        ph: ch as u32,
        dx: frame.width - cw as u32,
        dy: top as u32,
        fp: 0,
        geom: 0,
        raster_fingerprint_once: Default::default(),
        scrim: None,
        serial: 0,
        delta: None,
    });
    assert!(
        !app.stats_lens_within_surface(WID),
        "the right-edge lens is cropped after the inset"
    );
    app.set_side_inset_for_test(WID, 0);
    assert!(
        app.stats_lens_within_surface(WID),
        "negative control: without the inset it fits"
    );
    crate::app_render::side_inset_tests::assert_pet_pointer_picture_agreement();
}

#[test]
fn in_grid_strip_hits_survive_a_side_inset() {
    let mut app = App::headless_for_test();
    app.tab_strip_rows = 1;
    app.config.tab_strip_rows = Some(1);
    let (_, ch) = app.win_cell_size(WID);
    let head = app.synthetic_strip_head_px(1.0, ch);
    app.backend.set_head(head);
    app.windows.get_mut(&WID).unwrap().metrics.head = head;
    app.splice_tab_strip_with(WID, 1);
    let segments = app.windows[&WID].tab_segments.clone();
    let chip = segments
        .iter()
        .find(|s| matches!(s.kind, crate::tab_bar::TabHit::Select(_)))
        .expect("a terminal chip");
    let chip_col = chip.start_col;
    let frame = app.window_frame_px_for(WID, app.windows[&WID].rows, app.windows[&WID].cols);
    let (cw, ch) = app.win_cell_size(WID);
    app.windows.get_mut(&WID).unwrap().win_px = Some(frame);
    let x = (app.win_pad(WID) + usize::from(chip_col) * cw + cw / 2) as f64;
    let y = (app.win_head(WID) + app.win_pad_top(WID) + ch / 2) as f64;
    let before = app.strip_col_at(WID, x, y);
    assert!(before.is_some(), "the strip fixture has a hit");
    let before_chip = crate::tab_bar::hit_test(&segments, before.unwrap());
    assert!(matches!(
        before_chip,
        Some(crate::tab_bar::TabHit::Select(_))
    ));
    app.set_side_inset_for_test(WID, 4);
    app.windows.get_mut(&WID).unwrap().win_px = Some(PhysicalSize::new(
        frame.width + (4 * cw) as u32,
        frame.height,
    ));
    let after = app.strip_col_at(WID, x + (4 * cw) as f64, y);
    assert_eq!(after, before);
    assert_eq!(
        crate::tab_bar::hit_test(&segments, after.unwrap()),
        before_chip
    );
    app.on_cursor_moved(WID, x + (4 * cw) as f64, y);
    assert!(
        app.windows[&WID].strip_hover.is_some(),
        "the translated chip lights"
    );
    assert_eq!(
        app.strip_col_at(WID, (2 * cw) as f64, y),
        None,
        "the inset cannot hit the first chip"
    );
    app.on_cursor_moved(WID, (2 * cw) as f64, y);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert!(app.windows[&WID].strip_drag.is_none());
    assert!(!app.windows[&WID].selecting);
}

const HANG: std::time::Duration = std::time::Duration::from_secs(60);
pub(super) fn image_meta(
    app: &mut App,
    clean: bool,
) -> ((usize, usize, Vec<u8>), &'static str, u64) {
    let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    let (reply, rx) = std::sync::mpsc::channel();
    app.render_image(crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: crate::control_auth::ConfinedImage::in_memory(),
        clean,
        session: None,
        want_bytes: true,
        want_metadata: true,
        frame_metadata: std::sync::Arc::clone(&metadata),
        cancel: crate::control::CaptureCancellation::new(),
        reply,
    });
    let (_, _, png) = rx
        .recv_timeout(HANG)
        .expect("image reply")
        .expect("image succeeds")
        .value;
    let (rgba, width, height) =
        aterm_render::decode_png_rgba8(&png.expect("PNG bytes")).expect("a PNG");
    let metadata = metadata.get().expect("image metadata");
    (
        (width, height, rgba),
        metadata.phase,
        metadata.capture_serial,
    )
}

/// Straight RGBA pixels of the CPU surface inside `rect`.
pub(super) fn glass_rgba(app: &mut App, rect: (usize, usize, usize, usize)) -> Vec<u8> {
    let ((surface_w, _), pixels) = glass(app);
    let (x0, y0, width, height) = rect;
    (y0..y0 + height)
        .flat_map(|y| pixels[y * surface_w + x0..y * surface_w + x0 + width].to_vec())
        .flat_map(|pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8, 0xFF])
        .collect()
}

#[cfg(unix)]
use crate::input::{InputEvent, PasteFraming, Source};
#[cfg(unix)]
use aterm_session::sink::SinkWriter;
#[cfg(unix)]
use std::{
    io::{ErrorKind, Read},
    os::unix::net::UnixStream,
    sync::Arc,
    time::{Duration, Instant},
};
#[cfg(unix)]
pub(super) fn observer() -> (UnixStream, Arc<SinkWriter>) {
    let (reader, writer) = UnixStream::pair().expect("private input observer");
    reader.set_nonblocking(true).unwrap();
    (reader, Arc::new(SinkWriter::new_owned(writer.into())))
}

#[cfg(unix)]
fn drain(reader: &mut UnixStream) -> String {
    let mut out = Vec::new();
    let mut bytes = [0u8; 256];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&bytes[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => panic!("reading input: {e}"),
        }
    }
    String::from_utf8(out).expect("SGR reports are ASCII")
}

#[cfg(unix)]
pub(super) fn written_before_marker(app: &mut App, session: u64, wire: &mut UnixStream) -> String {
    const MARK: &str = "<marker>";
    app.input_to_session(
        WID,
        InputEvent::Paste(MARK.to_owned(), PasteFraming::Gesture { bracketed: false }),
        Source::Controller,
        Some(session),
        crate::app_input::PressPhase::Initial,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = String::new();
    loop {
        seen += &drain(wire);
        if let Some(before) = seen.strip_suffix(MARK) {
            return before.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "the marker never reached session {session}: {seen:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn headless_surface(app: &App, frame: (usize, usize)) -> (usize, usize) {
    App::headless_surface_px(frame, aterm_render::SideInsetPx(app.win_side_inset_px(WID)))
}

#[test]
fn side_inset_headless_cpu_picture_pointer_and_capture_share_the_surface() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    feed(&app, b"\x1b]11;#102030\x07\x1b[?25lheadless inset");
    let bare = app.window_frame_px_for(WID, app.windows[&WID].rows, app.windows[&WID].cols);
    app.set_side_inset_for_test(WID, 4);
    assert!(app.windows[&WID].win_px.is_none());
    let inset = app.win_side_inset_px(WID);
    app.redraw_window(WID);
    let ((w, h), pixels) = glass(&mut app);
    assert_eq!((w, h), (bare.width as usize + inset, bare.height as usize));
    assert_eq!(app.frame_origin(WID), (inset as i64, 0));
    assert!(pixels[..inset].iter().all(|p| *p == 0x102030));
    let ((fw, fh, rgba), _, _) = image_meta(&mut app, false);
    assert_eq!((fw, fh), (bare.width as usize, bare.height as usize));
    assert_eq!(rgba, glass_rgba(&mut app, (inset, 0, fw, fh)));
}
