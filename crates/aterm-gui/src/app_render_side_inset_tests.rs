// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::scripted_redraw_fixture::{feed, glass, scripted_app};
const WID: WindowId = WindowId(0);

/// Judge the real single and composed handoffs by the pet brain's pointer-play state.
pub(crate) fn assert_pet_pointer_picture_agreement() {
    let t0 = Instant::now();
    for split in [false, true] {
        let mut play = Vec::new();
        for inset in [0u16, 40] {
            let mut app = scripted_app();
            app.config.serious_mode = Some(false);
            app.config.cursor_pet = Some("cat".into());
            app.config.cursor_trail = Some(true);
            app.config.cursor_trail_style = Some("rainbow kitty".into());
            app.recompute_sparkle();
            if split {
                app.split_active_stub_tab(WID);
            }
            feed(&app, b"\x1b[6;12H$ cat");
            app.set_side_inset_for_test(WID, inset);
            let frame =
                app.window_frame_px_for(WID, app.windows[&WID].rows, app.windows[&WID].cols);
            app.windows.get_mut(&WID).unwrap().win_px = Some(frame);
            // Both brains start with the pointer off the surface, not with the legacy
            // retained (0,0) memo counting as a corner sample in only the zero-inset case.
            app.on_cursor_moved(WID, -1.0, -1.0);
            app.on_cursor_left(WID);
            for step in 0..20 {
                app.redraw_window_at(WID, t0 + Duration::from_millis(step * 16));
            }
            let (x0, x1, y0, y1) = app.windows[&WID]
                .fx
                .companion()
                .hit_rect()
                .expect("a drawn pet");
            let (ox, oy) = app.frame_origin(WID);
            let at = (
                f64::from(x0 + x1) / 2.0 + ox as f64,
                f64::from(y0 + y1) / 2.0 + oy as f64,
            );
            app.on_cursor_moved(WID, at.0, at.1);
            app.redraw_window_at(WID, t0 + Duration::from_millis(400));
            play.push(
                app.windows[&WID]
                    .fx
                    .state_words(t0)
                    .get("pet.play")
                    .expect("pet pointer-play words"),
            );
        }
        assert_eq!(
            play[0], play[1],
            "split={split}: the same painted pet sees the same pointer after placement"
        );
    }
}

#[test]
fn window_resource_strip_side_pixels_preserve_the_terminal_tray_and_paste_floor() {
    let size = (9, 5);
    let source: Vec<_> = (0..3)
        .flat_map(|y| (0..9).flat_map(move |x| [x + 1, y + 1, 0x70, 255]))
        .collect();
    let card = crate::SettingsCard {
        rgba: source,
        pw: 9,
        ph: 3,
        dx: 0,
        dy: 1,
        fp: 1,
        geom: 1,
        serial: 1,
        delta: None,
        scrim: None,
        raster_fingerprint_once: std::sync::OnceLock::new(),
    };
    let strip = resource_strip_quad(true, Some(&card), None, 2, (1, None)).unwrap();
    let interior = 0x456789;
    for (x, frame_width) in [(-2, 6), (0, 4), (2, 4), (5, 4), (0, 9)] {
        let mut pixels = vec![interior; size.0 * size.1];
        composite_resource_strip_edges(&mut pixels, size, (x, 0), frame_width, strip);
        for py in 0..size.1 {
            for px in 0..size.0 {
                let inside = (x..x + frame_width as i64).contains(&(px as i64));
                let expected = if !inside && (2..4).contains(&py) {
                    (((px + 1) as u32) << 16) | ((py as u32) << 8) | 0x70
                } else {
                    interior
                };
                assert_eq!(
                    pixels[py * size.0 + px],
                    expected,
                    "frame {x}/{frame_width}, pixel {px}/{py}"
                );
            }
        }
    }
    let modal = crate::SettingsCard {
        scrim: Some([0, 0, 0, 128]),
        ..card
    };
    assert!(resource_strip_quad(true, Some(&modal), Some(&modal), 0, (1, None)).is_none());
}

#[test]
fn side_inset_cpu_partial_damage_refreshes_a_leading_chrome_band() {
    let mut app = scripted_app();
    app.backend.set_pad(12);
    app.backend.set_pad_top(12);
    app.windows.get_mut(&WID).unwrap().metrics.pad = 12;
    app.windows.get_mut(&WID).unwrap().metrics.pad_top = 12;
    feed(&app, b"\x1b[?25lchrome");
    app.redraw_window(WID);
    let bare = app.frame_px(app.windows[&WID].rows, app.windows[&WID].cols);
    app.set_side_inset_for_test(WID, 4);
    app.windows.get_mut(&WID).unwrap().win_px = Some(bare);
    let bleed = |color| {
        Some(aterm_render::ChromeBleed {
            first: 0,
            rows: 1,
            color,
            seam: None,
            top_extends_cells: false,
            row_edges: [None; aterm_render::CHROME_ROW_EDGES],
        })
    };
    app.backend.set_chrome_bleed(bleed(0x102030));
    app.present_input_scratch_for_test(WID, false, None)
        .unwrap();
    let before = glass(&mut app);
    app.backend.set_chrome_bleed(bleed(0x405060));
    app.windows.get_mut(&WID).unwrap().input_scratch.cells[0][0].bg = [0x40, 0x50, 0x60];
    app.present_input_scratch_for_test(WID, false, None)
        .unwrap();
    assert_eq!(
        app.windows[&WID].cpu_cache.last_damage(),
        aterm_render::DamageOutcome::Rows
    );
    let after = glass(&mut app);
    let (ox, oy) = app.frame_origin(WID);
    assert!(
        ox > 0
            && bare.width
                <= app
                    .frame_px(app.windows[&WID].rows, app.windows[&WID].cols)
                    .width
    );
    let y = oy as usize + app.win_pad_top(WID) + app.win_head(WID) + app.win_cell_size(WID).1 / 2;
    assert_ne!(
        before.1[y * before.0.0],
        after.1[y * after.0.0],
        "the leading chrome band must change with its edge"
    );
    app.windows.get_mut(&WID).unwrap().cpu_cache.invalidate();
    app.present_input_scratch_for_test(WID, false, None)
        .unwrap();
    assert_eq!(
        after,
        glass(&mut app),
        "partial destination equals an independent full copy"
    );
}

fn recorded_virtual() -> (App, aterm_tempfile::TempDir, (usize, usize)) {
    let mut app = App::headless_for_test();
    feed(&app, b"\x1b]11;#102030\x07\x1b[?25lrecorded inset");
    let font_px = app.windows[&WID].metrics.font_px;
    let mut gpu = aterm_gpu::GpuRenderer::new(font_px, app.theme)
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format_args!("no GPU renderer: {e}")));
    gpu.warm_effect_pipelines_now();
    app.backend = crate::BackendSlot::Ready(crate::Backend::Gpu(crate::GpuBackend::new(gpu)));
    app.bind_window_renderer_state(WID);
    let capture = app.prepare_terminal_capture_grid(WID).unwrap();
    app.windows
        .get_mut(&WID)
        .unwrap()
        .capture_leaf_snapshot_seqs = capture.leaves.iter().map(|l| l.snapshot_seq).collect();
    app.windows.get_mut(&WID).unwrap().capture_leaf_content_seqs =
        capture.leaves.iter().map(|l| l.content_seq).collect();
    app.set_side_inset_for_test(WID, 4);
    assert!(app.windows[&WID].win_px.is_none());
    let bare = app.frame_px(
        app.windows[&WID].rows + app.chrome_rows(WID),
        app.windows[&WID].cols,
    );
    let surface = app.headless_frame_px(WID).unwrap();
    assert_eq!(
        surface.0 as usize,
        bare.width as usize + app.win_side_inset_px(WID)
    );
    let mut window_gpu = aterm_gpu::WindowGpu::new();
    app.backend
        .gpu_mut()
        .unwrap()
        .virtual_begin(
            &mut window_gpu,
            surface.0,
            surface.1,
            aterm_gpu::video_tap::CaptureOpts {
                half_res: false,
                budget_bytes: aterm_gpu::video_tap::DEFAULT_BUDGET,
                fps_cap: None,
                requested_ms: 0,
            },
        )
        .unwrap();
    app.windows.get_mut(&WID).unwrap().present = Some(PresentTarget::Virtual { window_gpu });
    let root = aterm_tempfile::Builder::new()
        .prefix("dk1-recorded-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    crate::control_auth::ensure_private_dir(root.path()).unwrap();
    let (reply, _) = std::sync::mpsc::channel();
    app.video_rec = Some(crate::VideoRec {
        window: WID,
        deadline: Instant::now() + Duration::from_secs(3600),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: crate::VideoMode::OffscreenPresentReal,
        next_frame: None,
        presented: None,
        dir: crate::control_auth::confine_video_dir(root.path()).unwrap(),
        handoff: None,
        cancel: crate::VideoCancellation::new(),
        reply,
    });
    assert!(
        matches!(
            app.present_input_scratch_for_test(WID, false, None)
                .unwrap(),
            Some(_)
        ),
        "a real Virtual present"
    );
    let pixels = virtual_surface(&mut app);
    let inset = app.win_side_inset_px(WID);
    let background = |p: &[u8]| {
        p.iter()
            .zip([0x10u8, 0x20, 0x30, 0xFF])
            .all(|(v, w)| v.abs_diff(w) <= 1)
    };
    for y in 0..pixels.h as usize {
        for x in 0..inset {
            let offset = (y * pixels.w as usize + x) * 4;
            assert!(
                background(&pixels.rgba[offset..offset + 4]),
                "the real GUI GPU present paints the side inset at ({x},{y})"
            );
        }
    }
    let (cw, ch) = app.win_cell_size(WID);
    let x0 = inset + app.win_pad(WID);
    let y0 = app.win_pad_top(WID) + app.win_head(WID);
    assert!(
        (y0..y0 + ch).any(|y| (x0..x0 + 2 * cw).any(|x| {
            let offset = (y * pixels.w as usize + x) * 4;
            !background(&pixels.rgba[offset..offset + 4])
        })),
        "positive control: the first two frame cells contain the recorded text"
    );
    let route = app.active_visible_content_route(WID).unwrap();
    app.finalize_successful_terminal_present_for_test(WID, route, HostVisualState::default());
    assert!(
        app.video_rec.as_ref().unwrap().presented.is_some(),
        "the recording owns the successful frame"
    );
    (app, root, (bare.width as usize, bare.height as usize))
}

#[test]
fn side_inset_recorded_virtual_images_cut_the_frame_in_both_capture_routes() {
    let (mut app, root, bare) = recorded_virtual();
    for metadata in [false, true] {
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: metadata,
            whole_client: false,
            presentation: None,
            frame_metadata: Arc::new(std::sync::OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        });
        let (_, _, png) = rx
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .unwrap()
            .value;
        let (_, w, h) = aterm_render::decode_png_rgba8(&png.unwrap()).unwrap();
        assert_eq!(
            (w, h),
            bare,
            "metadata={metadata}: image is the frame, never the Virtual surface"
        );
    }
    app.video_rec.take();
    drop(app);
    // On Windows this process keeps handles under `root` open without delete sharing until it
    // exits: the instance lease (`control_auth::hold_current_instance_lease`) and, that let go,
    // still one on the instance's own folder, `video\p<pid>-…` (ATERM-KI-1115-58210), so the
    // folder cannot go before then and is left in `%TEMP%\.ubd`; elsewhere it goes now.
    let removed = std::fs::remove_dir_all(root.keep());
    if cfg!(not(windows)) {
        removed.unwrap();
    }
}

#[cfg(target_os = "macos")]
#[test]
fn side_inset_virtual_overlay_uses_the_window_inset_and_the_headless_surface() {
    let (mut app, root, _) = recorded_virtual();
    if let Some(PresentTarget::Virtual { window_gpu }) =
        app.windows.get_mut(&WID).unwrap().present.as_mut()
    {
        window_gpu.set_side_inset_px(0);
    } else {
        panic!("Virtual fixture");
    }
    let (_, result) = app
        .present_deck_overlay(WID, false, None, Instant::now())
        .expect("native overlay path");
    assert!(
        matches!(result, Ok(Some(_))),
        "the GUI restores the inset before the resident-shape admission"
    );
    let expected = app.headless_frame_px(WID).unwrap();
    let App {
        backend, windows, ..
    } = &mut app;
    let Some(PresentTarget::Virtual { window_gpu }) = windows[&WID].present.as_ref() else {
        panic!("Virtual fixture")
    };
    let destination = backend
        .gpu_mut()
        .unwrap()
        .virtual_presented_snapshot_current(window_gpu, window_gpu.resident_input_epoch(), 1)
        .unwrap()
        .convert()
        .unwrap();
    assert_eq!((destination.w, destination.h), expected);
    app.video_rec.take();
    drop(app);
    std::fs::remove_dir_all(root.keep()).unwrap();
}

fn virtual_surface(app: &mut App) -> aterm_gpu::video_tap::CapturedFrame {
    let App {
        backend, windows, ..
    } = app;
    let Some(PresentTarget::Virtual { window_gpu }) = windows[&WID].present.as_ref() else {
        panic!("Virtual fixture")
    };
    backend
        .gpu_mut()
        .unwrap()
        .virtual_presented_snapshot_current(window_gpu, window_gpu.resident_input_epoch(), 1)
        .unwrap()
        .convert()
        .unwrap()
}
