// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, image_req_for_test, terminal_capture_text};
use crate::{WindowId, control_auth, term_lock};
use std::time::{Duration, Instant};

fn split_fixture() -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let right = app.split_active_stub_tab(wid);
    let left_term = app.pool.get(0).expect("left pane").term.clone();
    let right_term = app.pool.get(right).expect("right pane").term.clone();
    term_lock(&left_term).process(b"\x1b]11;#102030\x07LEFT");
    term_lock(&right_term).process(b"\x1b]11;#405060\x07RIGHT");
    (app, wid, right)
}

fn confined(dir: &std::path::Path, name: &str) -> control_auth::ConfinedImage {
    control_auth::ConfinedImage::for_test(dir, name)
}

#[test]
fn recording_image_keeps_the_exact_successful_present_cursor_layers() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let font_px = app.windows[&wid].metrics.font_px;
    let single = app
        .prepare_terminal_capture_grid(wid)
        .expect("single-pane model");
    assert!(
        !single.composed,
        "regression covers the normal one-pane route"
    );
    let capture_leaf_snapshot_seqs: Vec<_> =
        single.leaves.iter().map(|leaf| leaf.snapshot_seq).collect();
    let capture_leaf_content_seqs: Vec<_> =
        single.leaves.iter().map(|leaf| leaf.content_seq).collect();
    assert_eq!(capture_leaf_snapshot_seqs.len(), 1);
    let window = app.windows.get_mut(&wid).unwrap();
    window.capture_leaf_snapshot_seqs = capture_leaf_snapshot_seqs;
    window.capture_leaf_content_seqs = capture_leaf_content_seqs;

    let mut gpu = match aterm_gpu::GpuRenderer::new(font_px, app.theme) {
        Ok(gpu) => gpu,
        Err(error) => {
            aterm_spec::host::could_not_run(format_args!("no GPU renderer on this host: {error}"));
        }
    };
    gpu.set_pad(12);
    gpu.set_pad_top(2);
    assert_eq!((gpu.pad(), gpu.pad_top()), (12, 2));
    app.backend = crate::BackendSlot::Ready(crate::Backend::Gpu(crate::GpuBackend::new(gpu)));
    app.bind_window_renderer_state(wid);
    let (input_rows, input_cols) = {
        let input = &app.windows[&wid].input_scratch;
        (input.rows, input.cols)
    };
    let (width, height) = app.backend.frame_size(input_rows, input_cols);
    let (_, cell_h) = app.backend.cell_size();
    let mut window_gpu = aterm_gpu::WindowGpu::new();
    app.backend
        .gpu_mut()
        .expect("installed GPU fixture")
        .virtual_begin(
            &mut window_gpu,
            u32::try_from(width).expect("fixture width"),
            u32::try_from(height).expect("fixture height"),
            aterm_gpu::video_tap::CaptureOpts {
                half_res: false,
                budget_bytes: aterm_gpu::video_tap::DEFAULT_BUDGET,
                fps_cap: None,
                requested_ms: 0,
            },
        )
        .expect("real virtual recording target");
    app.windows.get_mut(&wid).unwrap().present = Some(crate::PresentTarget::Virtual { window_gpu });

    let root = std::env::temp_dir().join(format!(
        "aterm-recording-image-retain-{}-{}",
        std::process::id(),
        crate::metrics::now_us()
    ));
    let _ = std::fs::remove_dir_all(&root);
    control_auth::ensure_private_dir(&root).expect("private capture root");
    let video_dir = control_auth::confine_video_dir(&root).expect("recording directory");
    let (video_reply, _video_rx) = std::sync::mpsc::channel();
    app.video_rec = Some(crate::VideoRec {
        window: wid,
        deadline: Instant::now() + Duration::from_secs(3_600),
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
        dir: video_dir,
        handoff: None,
        cancel: crate::VideoCancellation::new(),
        reply: video_reply,
    });

    let (early_reply, early_rx) = std::sync::mpsc::channel();
    app.render_image(image_req_for_test(
        confined(&root, "too-early.png"),
        false,
        true,
        None,
        early_reply,
    ));
    early_rx
        .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
        .expect("pre-baseline image reply")
        .expect_err("a recording with no successful frame must defer");

    let trail_color = 0x00FA_01B5;
    let kitty_color = [0x22, 0xDD, 0x66, 0xFF];
    {
        let window = app.windows.get_mut(&wid).expect("recorded window");
        window.input_scratch.cursor_trail = vec![aterm_render::TrailCell {
            row: 4,
            col: 12,
            alpha: 255,
        }];
        window.input_scratch.cursor_trail_color = trail_color;
        window.input_scratch.cursor_glow_add = vec![aterm_render::GlowQuad {
            row: 4,
            x: 1,
            y: 1,
            w: 1,
            h: 1,
            color: 0x0001_0101,
            alpha: 0,
            color2: 0x0001_0101,
            alpha2: 0,
        }];
        let kitty = aterm_render::fx::bitmap(
            aterm_core::render::FxTexels::Rgba8Straight,
            2,
            2,
            kitty_color.repeat(4),
        )
        .expect("a 2x2 kitty art");
        aterm_render::sprites::SpriteLayers::new(aterm_render::sprites::WORD_SPRITE_SALT).publish(
            &[aterm_render::sprites::FreeSprite::whole(
                30,
                i32::try_from(6 * cell_h).expect("fixture kitty y"),
                kitty,
            )],
            &mut window.input_scratch.fx_layers,
        );
    }
    app.bind_window_renderer_state(wid);
    app.present_input_scratch_for_test(wid, false, None)
        .expect("the real virtual submission succeeds");
    let route = app.active_visible_content_route(wid).unwrap();
    app.finalize_successful_terminal_present_for_test(
        wid,
        route,
        crate::app_render::HostVisualState::default(),
    );
    let successful_serial = app.windows[&wid].capture_present_serial;
    app.windows.get_mut(&wid).unwrap().last_present = None;

    app.prepare_terminal_capture_grid(wid)
        .expect("interleaved terminal snapshot");
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.capture_leaf_snapshot_seqs.clear();
        window.capture_leaf_content_seqs.clear();
        let scratch = &mut window.input_scratch;
        scratch.cursor_trail.clear();
        scratch.cursor_glow_add.clear();
        scratch
            .fx_layers
            .retain(|l| aterm_render::sprites::FreeSprite::of_layer(l).is_none());
    }
    let retained = app
        .recording_presented_frame_capture(wid, true, false)
        .expect("resident successful frame survives scratch interleave");
    assert_eq!(retained.input.cursor_trail.len(), 1);
    assert_eq!(retained.input.cursor_trail_color, trail_color);
    assert_eq!(retained.input.cursor_glow_add.len(), 1);
    assert_eq!(
        retained.input.cursor_glow_add[0].y, 1,
        "resident semantic capture undoes the nonzero pad transport shift"
    );
    assert_eq!(
        aterm_render::sprites::published(&retained.input.fx_layers).len(),
        1
    );
    drop(retained);

    app.windows.get_mut(&wid).unwrap().blink_phase = false;
    assert!(
        app.recording_presented_frame_capture(wid, true, false)
            .is_err(),
        "renderer-state drift must defer rather than reraster old input with a new blink phase"
    );
    assert!(
        app.recording_presented_destination_capture(wid).is_ok(),
        "live renderer drift cannot invalidate already-presented destination pixels"
    );
    app.windows.get_mut(&wid).unwrap().blink_phase = true;

    let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    let (reply, rx) = std::sync::mpsc::channel();
    app.render_image(image_req_for_test(
        confined(&root, "retained.png"),
        false,
        true,
        Some(&metadata),
        reply,
    ));
    let (_, _, png) = rx
        .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
        .expect("image worker reply")
        .expect("recording still succeeds")
        .value;
    let png = png.expect("the retained model is encoded");
    let (rgba, _, _) = aterm_render::decode_png_rgba8(&png).expect("encoded RGBA8");
    let expected = [0xFA, 0x01, 0xB5];
    assert!(
        rgba.as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[..3] == expected),
        "the exact encoded pixels retain the opaque rainbow-trail sentinel"
    );
    assert!(
        rgba.as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel.as_slice() == kitty_color.as_slice()),
        "the exact encoded pixels retain the free-sprite/kitty sentinel"
    );
    assert_eq!(
        metadata.get().expect("image metadata").capture_serial,
        successful_serial,
        "the still identifies the same successful present as the recording"
    );
    let staged = &app.windows[&wid].input_scratch;
    assert!(staged.cursor_trail.is_empty());
    assert!(staged.cursor_glow_add.is_empty());

    let scratch_bg = 0x0013_57A9;
    app.windows.get_mut(&wid).unwrap().input_scratch.default_bg = scratch_bg;
    let plain_still = |app: &mut App, name: &str| {
        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: confined(&root, name),
            clean: true,
            session: None,
            want_bytes: true,
            want_metadata: true,
            frame_metadata: std::sync::Arc::clone(&metadata),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        });
        let (_, _, png) = rx
            .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
            .expect("plain still reply")
            .expect("the plain recording still succeeds")
            .value;
        let (rgba, width, height) =
            aterm_render::decode_png_rgba8(&png.expect("the plain still is encoded"))
                .expect("encoded RGBA8");
        let pixels: Vec<u32> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
            .collect();
        let serial = metadata.get().expect("plain still metadata").capture_serial;
        (width, height, pixels, serial)
    };
    let (width, height, absent, serial) = plain_still(&mut app, "plain.png");
    assert_eq!(
        serial, successful_serial,
        "the plain still is bound to the recorded present"
    );
    assert!(
        !absent.contains(&trail_color),
        "the plain still strips the rainbow-trail sentinel"
    );
    assert!(
        !absent.contains(&scratch_bg),
        "the plain still rasterizes the resident model, not the newer scratch"
    );
    assert_eq!(
        app.recording_presented_frame_capture(wid, true, false)
            .expect("resident frame after the plain still")
            .input
            .cursor_trail
            .len(),
        1,
        "the strip took an owned copy; the resident model keeps its trail"
    );

    app.level_up = Some(crate::level_up::LevelUp::charging_continued(
        9,
        Instant::now(),
        0.0,
    ));
    let overlay = app
        .host_visual_state(wid, Instant::now())
        .overlay
        .expect("a live glow");
    let (_, _, glowing, serial) = plain_still(&mut app, "plain-glow.png");
    assert_eq!(serial, successful_serial);
    assert_eq!(
        glowing[0], absent[0],
        "a retained None overlay is authoritative: the live glow is not resampled"
    );
    app.level_up = None;

    let mut resampled = absent.clone();
    crate::app_render::apply_overlay_at(
        &mut resampled,
        width,
        height,
        0,
        0,
        width,
        height,
        overlay,
    );
    assert_ne!(resampled[0], absent[0]);
    let mut scratch = app.take_rerender_input(wid);
    scratch.clear_overlays();
    let scratch_frame = app
        .rerender_capture(
            wid,
            &mut scratch,
            crate::app_render::HostVisualState::default(),
        )
        .expect("a rerender of the scratch");
    assert!(
        scratch_frame
            .pixels
            .iter()
            .any(|&pixel| pixel & 0x00FF_FFFF == scratch_bg)
    );

    if let Some(crate::PresentTarget::Virtual { window_gpu }) =
        app.windows.get_mut(&wid).unwrap().present.as_mut()
    {
        window_gpu.invalidate_present();
    }
    assert!(
        app.recording_presented_destination_capture(wid).is_ok(),
        "semantic invalidation leaves the last successful virtual destination readable"
    );
    app.present_input_scratch_for_test(wid, false, None)
        .expect("stage a newer virtual destination without success authority");
    assert!(
        app.recording_presented_destination_capture(wid).is_err(),
        "a newer destination generation cannot pair with the old successful serial"
    );

    let _ = app.video_rec.take();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn single_capture_dynamic_cursor_tracks_osc10_in_state_and_pixels() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut term = term_lock(&terminal);
        term.process(b"\x1b]21;cursor=\x07\x1b]10;#21C365\x07\x1b[2 q");
        assert_eq!(term.cursor_color(), None);
    }

    let capture = app
        .prepare_terminal_capture_grid(wid)
        .expect("single terminal capture");
    assert!(!capture.composed);
    let input = app.windows[&wid].input_scratch.clone();
    assert_eq!(input.cursor_color, 0x0021_C365);

    let Some(mut renderer) =
        aterm_render::Renderer::from_system(16.0, aterm_render::Theme::default())
    else {
        aterm_spec::host::could_not_run("no system monospace font on this host");
    };
    let (cw, ch) = renderer.cell_size();
    let frame = renderer.render_input(&input);
    assert_eq!(
        frame
            .pixels
            .iter()
            .filter(|&&pixel| pixel == 0x0021_C365)
            .count(),
        cw * ch,
        "the capture's steady blank cursor pixels follow OSC 10 after OSC 21 cursor="
    );
}

#[test]
fn split_capture_grid_composes_sparse_panes_without_present_side_effects() {
    let (mut app, wid, right) = split_fixture();
    let plan = app
        .active_visible_leaf_plan(wid)
        .expect("visible split plan");
    let rects: Vec<_> = plan
        .leaves
        .iter()
        .map(|leaf| crate::pane::PaneRect {
            session: app
                .view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                .expect("terminal leaf"),
            row_off: leaf.rect.origin.y.round() as u16,
            col_off: leaf.rect.origin.x.round() as u16,
            rows: leaf.rect.size.height.round() as u16,
            cols: leaf.rect.size.width.round() as u16,
        })
        .collect();
    let left_rect = rects.iter().find(|rect| rect.session == 0).unwrap();
    let right_rect = rects.iter().find(|rect| rect.session == right).unwrap();
    let left_term = app.pool.get(0).unwrap().term.clone();
    let left_epoch = term_lock(&left_term).damage_epoch();
    let left_coords = term_lock(&left_term)
        .content_scroll_state()
        .coordinate_generation();
    let predictor_now = Instant::now();
    let predictor_deadline = {
        let window = app.windows.get_mut(&wid).unwrap();
        window
            .predictor
            .set_mode(crate::predict::PredictMode::Always);
        assert!(window.predictor.predict_char(
            '?',
            (0, 0),
            left_coords,
            window.cols,
            predictor_now
        ));
        window.predictor.next_deadline()
    };

    assert!(app.windows[&wid].last_present.is_none());
    let capture = app
        .prepare_terminal_capture_grid(wid)
        .expect("terminal split capture");
    assert!(capture.composed);
    assert_eq!(capture.leaves.len(), 2);
    assert_eq!(
        capture
            .leaves
            .iter()
            .filter(|leaf| leaf.focused)
            .map(|leaf| leaf.session)
            .collect::<Vec<_>>(),
        vec![right],
        "capture inventory identifies exactly the focused pane"
    );
    let ws = &app.windows[&wid];
    let content_row = usize::from(left_rect.row_off);
    assert_eq!(left_rect.row_off, right_rect.row_off);
    let row = &ws.input_scratch.cells[content_row];
    assert_eq!(row[usize::from(left_rect.col_off)].ch, 'L');
    assert_eq!(row[usize::from(right_rect.col_off)].ch, 'R');
    assert_eq!(
        row[usize::from(left_rect.col_off + left_rect.cols - 1)].bg,
        [0x10, 0x20, 0x30],
        "left sparse tail uses the left terminal's implicit OSC-11 blank"
    );
    assert_eq!(
        row[usize::from(right_rect.col_off + right_rect.cols - 1)].bg,
        [0x40, 0x50, 0x60],
        "right sparse tail uses the right terminal's implicit OSC-11 blank"
    );
    assert_eq!(ws.input_scratch.cursor_row, usize::from(right_rect.row_off));
    assert!(
        (usize::from(right_rect.col_off)..usize::from(right_rect.col_off + right_rect.cols))
            .contains(&ws.input_scratch.cursor_col),
        "the only visible cursor is offset into the focused pane"
    );
    assert_eq!(
        ws.input_scratch.default_bg_spans[content_row].len(),
        2,
        "both panes retain independent live default-background provenance"
    );
    assert_eq!(
        ws.input_scratch.default_bg_at(
            content_row,
            usize::from(left_rect.col_off + left_rect.cols - 1),
        ),
        0x0010_2030,
    );
    assert_eq!(
        ws.input_scratch.default_bg_at(
            content_row,
            usize::from(right_rect.col_off + right_rect.cols - 1),
        ),
        0x0040_5060,
    );
    assert!(
        ws.last_present.is_none(),
        "capture is not an application-present decision"
    );
    assert_eq!(
        ws.predictor.next_deadline(),
        predictor_deadline,
        "capture does not advance predictor state"
    );
    assert!(!ws.predictor.idle());

    term_lock(&left_term).process(b"!");
    assert_eq!(
        term_lock(&left_term).damage_epoch(),
        left_epoch,
        "capture does not consume terminal damage"
    );
}

#[test]
fn focused_split_selection_is_identical_in_live_and_capture_composites() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let (mut app, wid, right) = split_fixture();
    let (rows, cols) = {
        let window = &app.windows[&wid];
        (usize::from(window.rows), usize::from(window.cols))
    };
    let rects = app
        .active_tree(wid)
        .expect("split tree")
        .compute_layout(rows as u16, cols as u16);
    let right_rect = rects.iter().find(|rect| rect.session == right).unwrap();
    let right_term = app.pool.get(right).expect("right terminal").term.clone();
    {
        let mut terminal = term_lock(&right_term);
        terminal.process(b"\x1b]17;rgb:12/34/56\x07\x1b]19;rgb:fe/cd/32\x07");
        let selection = terminal.text_selection_mut();
        selection.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(2, 2, SelectionSide::Right);
        selection.complete_selection();
    }

    let assert_projection = |input: &aterm_render::RenderInput, lane: &str| {
        let row0 = usize::from(right_rect.row_off);
        let col0 = usize::from(right_rect.col_off);
        let row_end = row0 + usize::from(right_rect.rows);
        let col_end = col0 + usize::from(right_rect.cols);
        assert_eq!(
            input.selection_clip,
            Some(aterm_render::SelectionClip::new(
                row0, row_end, col0, col_end,
            )),
            "{lane}: the renderer clip is the focused pane rectangle"
        );
        assert_eq!(input.selection_bg, 0x0012_3456, "{lane}: OSC 17");
        assert_eq!(input.selection_fg, 0x00fe_cd32, "{lane}: OSC 19");
        assert!(input.selection_contains_cell(row0, col0 + 1, false, false));
        assert!(input.selection_contains_cell(row0 + 2, col0 + 2, false, false));
        assert!(
            !input.selection_contains_cell(row0 + 2, col0 + 3, false, false),
            "{lane}: the final endpoint remains exact"
        );
    };

    assert!(
        app.redraw_compose(
            wid,
            rows,
            cols,
            false,
            false,
            None,
            0,
            std::time::Instant::now(),
        )
        .is_some(),
        "the focused selection change presents"
    );
    assert_projection(&app.windows[&wid].input_scratch, "live");

    let capture = app
        .prepare_terminal_capture_grid(wid)
        .expect("split capture");
    assert!(capture.composed);
    assert_projection(&app.windows[&wid].input_scratch, "capture");
}

#[test]
fn the_capture_model_hash_sees_every_panes_selection() {
    use aterm_core::selection::{SelectionSide, SelectionType, TextSelection};
    use std::hash::Hasher as _;
    let hash = |input: &aterm_render::RenderInput| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        crate::App::hash_terminal_render_model(input, &mut h);
        h.finish()
    };
    let pane = |c1: u16| {
        let mut selection = TextSelection::default();
        selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(0, c1, SelectionSide::Right);
        selection.complete_selection();
        aterm_core::render::PaneSelection {
            selection,
            clip: aterm_core::render::SelectionClip {
                row_start: 0,
                row_end: 2,
                col_start: 0,
                col_end: 8,
            },
            bg: aterm_core::render::COLOR_UNSET,
            fg: aterm_core::render::COLOR_UNSET,
            inactive: true,
        }
    };
    let mut term = aterm_core::terminal::Terminal::new(2, 8);
    term.process(b"abcdefgh");
    let base = term.cell_frame(2, 8);
    let mut one = base.clone();
    one.selections = vec![pane(2)];
    let mut moved = base.clone();
    moved.selections = vec![pane(5)];
    let mut bare = base.clone();
    bare.selections = vec![aterm_core::render::PaneSelection {
        selection: TextSelection::default(),
        ..pane(2)
    }];
    assert_eq!(hash(&one), hash(&one.clone()), "deterministic");
    assert_ne!(
        hash(&one),
        hash(&moved),
        "an unfocused pane's highlight moved"
    );
    assert_ne!(hash(&base), hash(&bare), "an entry is not the empty list");
    assert_ne!(hash(&bare), hash(&one), "a selection is not a bare entry");
}

#[test]
fn the_capture_model_hash_sees_a_rows_bidi_map() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    use std::hash::Hasher as _;
    let hash = |input: &aterm_render::RenderInput| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        crate::App::hash_terminal_render_model(input, &mut h);
        h.finish()
    };
    let mut term = aterm_core::terminal::Terminal::new(2, 8);
    term.process("\u{05D0}\u{05D0}\u{05D0}\u{05D0}".as_bytes());
    let sel = term.text_selection_mut();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 0, SelectionSide::Right);
    sel.complete_selection();
    let reordered = term.cell_frame(2, 8);
    assert_eq!(reordered.visual_to_logical_row(0), [3, 2, 1, 0]);
    let mut unmapped = reordered.clone();
    unmapped.visual_to_logical.clear();
    assert_eq!(reordered.cells, unmapped.cells, "the same cells");
    assert!(reordered.selection_contains_cell(0, 3, false, false));
    assert!(unmapped.selection_contains_cell(0, 0, false, false));
    assert_ne!(hash(&reordered), hash(&unmapped), "a different highlight");
    let mut empty_rows = unmapped.clone();
    empty_rows.visual_to_logical = vec![Vec::new(); 2];
    assert_eq!(hash(&unmapped), hash(&empty_rows), "both the identity");
}

#[test]
fn an_unfocused_pane_selection_paints_and_is_what_a_copy_resolves() {
    use crate::menu::{MenuAction, menu_live, set_menu_live};
    use aterm_core::selection::{SelectionSide, SelectionType};

    let before = menu_live();
    let mut readiness_failures = Vec::new();
    let mut check_readiness = |app: &App, label: &str, expected: bool| {
        app.publish_menu_live();
        let native = menu_live().has_selection;
        let live = app.palette_live().has_selection;
        let palette =
            app.palette_snapshot(WindowId(0)).action_by_name("Copy") == Ok(MenuAction::Copy);
        if native != expected || live != expected || palette != expected {
            readiness_failures.push(format!(
                "{label}: expected={expected}, native={native}, live={live}, palette={palette}"
            ));
        }
    };
    let (mut app, wid, right) = split_fixture();
    check_readiness(&app, "no split selection", false);
    let (rows, cols) = {
        let window = &app.windows[&wid];
        (usize::from(window.rows), usize::from(window.cols))
    };
    let rects = app
        .active_tree(wid)
        .expect("split tree")
        .compute_layout(rows as u16, cols as u16);
    let left_rect = *rects.iter().find(|rect| rect.session == 0).unwrap();
    assert_eq!(
        app.active_tree(wid).unwrap().focus(),
        right,
        "the split fixture focuses the NEW pane, so session 0 is unfocused"
    );

    let left_term = app.pool.get(0).expect("left pane").term.clone();
    {
        let mut terminal = term_lock(&left_term);
        let selection = terminal.text_selection_mut();
        selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(0, 3, SelectionSide::Right);
        selection.complete_selection();
    }

    assert!(
        app.redraw_compose(
            wid,
            rows,
            cols,
            false,
            false,
            None,
            0,
            std::time::Instant::now(),
        )
        .is_some(),
        "an unfocused pane's new selection must reach the present early-out"
    );
    let input = &app.windows[&wid].input_scratch;
    assert!(
        input.selection_contains_cell(
            usize::from(left_rect.row_off),
            usize::from(left_rect.col_off),
            false,
            false
        ),
        "the unfocused pane's selection PAINTS (this is the whole item)"
    );

    assert_eq!(
        app.window_selection_text(wid).as_deref(),
        Some("LEFT"),
        "⌘-C resolves the pane whose highlight is on screen"
    );
    check_readiness(&app, "unfocused visible selection", true);

    for prior in [true, false] {
        set_menu_live(crate::menu::MenuLive {
            has_selection: prior,
            ..menu_live()
        });
        {
            let _held = term_lock(&left_term);
            app.publish_menu_live();
            assert_eq!(
                menu_live().has_selection,
                prior,
                "an unread sibling keeps the prior menu bit without blocking"
            );
        }
    }
    check_readiness(&app, "unlocked visible sibling", true);

    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .toggle_zoom()
    );
    assert_eq!(app.active_visible_leaf_plan(wid).unwrap().leaves.len(), 1);
    assert_eq!(
        app.window_selection_text(wid),
        None,
        "zoom hides the selected sibling from Copy"
    );
    check_readiness(&app, "selected sibling hidden by zoom", false);
    assert!(
        !app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .toggle_zoom()
    );
    assert_eq!(app.window_selection_text(wid).as_deref(), Some("LEFT"));
    check_readiness(&app, "selected sibling visible after unzoom", true);

    let right_term = app.pool.get(right).expect("right pane").term.clone();
    {
        let mut terminal = term_lock(&right_term);
        let selection = terminal.text_selection_mut();
        selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(0, 4, SelectionSide::Right);
        selection.complete_selection();
    }
    assert_eq!(
        app.window_selection_text(wid).as_deref(),
        Some("RIGHT"),
        "the focused pane wins whenever it has a selection at all"
    );
    check_readiness(&app, "focused visible selection", true);

    {
        let mut terminal = term_lock(&right_term);
        let selection = terminal.text_selection_mut();
        selection.start_selection(3, 0, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(3, 6, SelectionSide::Right);
        selection.complete_selection();
        assert!(
            terminal.selection_to_string().is_none(),
            "precondition: this selection is real but resolves to no text"
        );
        assert!(
            terminal.text_selection().has_selection(),
            "precondition: …and the pane genuinely holds it"
        );
    }
    assert_eq!(
        app.window_selection_text(wid),
        None,
        "a focused pane holding a whitespace-only selection copies NOTHING — it \
         must never fall through to a sibling's text"
    );
    check_readiness(&app, "focused blank selection still owns Copy", true);

    let mut mixed = App::headless_for_test();
    assert!(mixed.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native_view) = mixed.active_native_view(wid).expect("Settings view");
    let (session, _) =
        mixed.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let terminal = mixed.pool.get(session).unwrap().term.clone();
    {
        let mut terminal = term_lock(&terminal);
        terminal.process(b"VISIBLE");
        let selection = terminal.text_selection_mut();
        selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(0, 6, SelectionSide::Right);
        selection.complete_selection();
    }
    assert_eq!(mixed.window_selection_text(wid).as_deref(), Some("VISIBLE"));
    check_readiness(&mixed, "focused terminal in a mixed tab", true);
    assert!(
        mixed
            .windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(native_view)
    );
    mixed.sync_window(wid);
    assert!(mixed.active_native_view(wid).is_some());
    assert_eq!(
        mixed.window_selection_text(wid).as_deref(),
        Some("VISIBLE"),
        "the selected terminal sibling remains visible"
    );
    assert_eq!(mixed.native_selection_text(wid), None);
    assert!(
        !mixed.copy_selection_in(wid),
        "native focus cannot borrow the terminal sibling's selection"
    );
    check_readiness(
        &mixed,
        "native focus above a selected terminal sibling",
        false,
    );

    set_menu_live(before);
    assert!(
        readiness_failures.is_empty(),
        "{}",
        readiness_failures.join("\n")
    );
}

#[test]
fn unchanged_split_and_zoom_captures_keep_stable_model_identity() {
    let (mut app, wid, _) = split_fixture();
    let first = app.prepare_terminal_capture_grid(wid).unwrap();
    let first_fp = App::terminal_capture_fingerprint(&first, &app.windows[&wid].input_scratch);
    let second = app.prepare_terminal_capture_grid(wid).unwrap();
    assert_eq!(
        App::terminal_capture_fingerprint(&second, &app.windows[&wid].input_scratch),
        first_fp
    );
    let left = app.pool.get(0).unwrap().term.clone();
    crate::term_lock(&left).process(b"!");
    let changed = app.prepare_terminal_capture_grid(wid).unwrap();
    assert_ne!(
        App::terminal_capture_fingerprint(&changed, &app.windows[&wid].input_scratch),
        first_fp
    );

    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .toggle_zoom()
    );
    let zoom_first = app.prepare_terminal_capture_grid(wid).unwrap();
    assert!(zoom_first.composed);
    assert_eq!(zoom_first.leaves.len(), 1, "zoom exposes one split leaf");
    let zoom_fp = App::terminal_capture_fingerprint(&zoom_first, &app.windows[&wid].input_scratch);
    let zoom_second = app.prepare_terminal_capture_grid(wid).unwrap();
    assert_eq!(
        App::terminal_capture_fingerprint(&zoom_second, &app.windows[&wid].input_scratch),
        zoom_fp
    );
}

#[test]
fn image_capture_reports_split_composite_and_splices_chrome_once() {
    let (mut app, wid, right) = split_fixture();
    app.config.cursor_trail = Some(true);
    app.recompute_sparkle();
    app.sparkle = None;
    app.windows.get_mut(&wid).expect("window").focused = true;
    app.tab_strip_rows = 1;
    let base_rows = usize::from(app.windows[&wid].rows);
    let cols = usize::from(app.windows[&wid].cols);
    let plan = app
        .active_visible_leaf_plan(wid)
        .expect("visible split plan");
    let content_row = plan.leaves[0].rect.origin.y.round() as usize;
    assert!(
        plan.leaves
            .iter()
            .all(|leaf| leaf.rect.origin.y.round() as usize == content_row)
    );

    let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    let (tx, rx) = std::sync::mpsc::channel();
    app.render_image(image_req_for_test(
        control_auth::ConfinedImage::in_memory(),
        false,
        true,
        Some(&metadata),
        tx,
    ));
    let (_, _, png) = rx
        .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
        .expect("image worker reply")
        .expect("split image succeeds")
        .value;
    assert!(png.is_some(), "real image path encoded the composed frame");

    let metadata = metadata.get().expect("image metadata");
    assert_eq!(metadata.frame_kind, "composite");
    assert_eq!(
        metadata.view, None,
        "a composite makes no singular-view claim"
    );
    assert_eq!(metadata.leaves.len(), 2);
    assert_eq!(
        metadata
            .leaves
            .iter()
            .map(|leaf| (leaf.session.unwrap(), leaf.focused))
            .collect::<Vec<_>>(),
        vec![(0, false), (right, true)]
    );
    assert!(
        metadata
            .leaves
            .iter()
            .all(|leaf| leaf.snapshot_seq.is_some()),
        "each visible terminal binds its own engine snapshot"
    );

    let ws = &app.windows[&wid];
    assert_eq!(
        ws.input_scratch.rows,
        base_rows + 1,
        "one tab-strip row, exactly once"
    );
    assert_eq!(ws.input_scratch.cells.len(), base_rows + 1);
    let text = terminal_capture_text(&ws.input_scratch, 1, cols);
    assert!(text.lines().nth(content_row).unwrap().contains("LEFT"));
    assert!(text.lines().nth(content_row).unwrap().contains("RIGHT"));
    assert!(
        ws.fx.companion().brain().is_active(),
        "the production composed-image route advances the default resident pet"
    );
    assert_eq!(
        aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
        1,
        "the split capture carries exactly one resident companion, never zero or a pet+kitty pair"
    );
    assert_eq!(
        text.lines().count(),
        base_rows,
        "SIGUSR1 text projection excludes chrome but retains the full composite grid"
    );
    assert!(
        ws.last_present.is_none(),
        "image capture does not stamp application-present"
    );
}
