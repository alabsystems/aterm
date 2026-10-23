// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Capture barrier tests: binding captures to forced presents and verifying destination vs rerender paths.

use super::{
    App, CapturePixels, CapturePng, CapturePresentBudget, CaptureRequest, DestinationDemand,
    PresentTake, PresentedCapture, SourceFacts, SourceKind, capture_source_kind, ran_pixel_work,
    snapshot_sidecar_path,
};
use crate::WindowId;
use crate::app_config::{BackgroundMaterial, RenderKnobs};
use crate::app_render::HostVisualState;
use crate::present::scripted::ScriptedOutcome;
use crate::scripted_redraw_fixture::{feed, glass, headless, scripted_app, surface};
use crate::work_counts;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const WID: WindowId = WindowId(0);

/// A wait for an answer that must come: a hang detector, not a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// Force a present of the front window with a fresh capture present budget.
fn barrier(app: &mut App, take: PresentTake) -> Result<Option<PresentedCapture>, String> {
    app.capture_present_budget = CapturePresentBudget::default();
    app.present_before_capture(WID, None, take)
}

/// Drop every present a barrier attempts.
fn drop_every_attempt(app: &mut App) {
    for _ in 0..crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT {
        surface(app).fail_next(ScriptedOutcome::FailCommit);
    }
}

fn private_dir(label: &str) -> std::path::PathBuf {
    let nonce = aterm_time::since_epoch()
        .expect("clock after epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aterm-capture-barrier-{label}-{}-{nonce}",
        std::process::id()
    ));
    crate::control_auth::ensure_private_dir(&dir).expect("capture dir");
    dir
}

/// Wait for a SIGUSR1 snapshot of the front window and return its decoded `(width, height, rgba)` and text.
fn snapshot(app: &mut App, dir: &std::path::Path, name: &str) -> ((usize, usize, Vec<u8>), String) {
    let path = dir.join(name);
    let done = snapshot_sidecar_path(&path, ".done");
    app.snapshot_at_path(path.display().to_string());
    let deadline = Instant::now() + HANG;
    while !done.exists() {
        assert!(Instant::now() < deadline, "the snapshot was published");
        std::thread::sleep(Duration::from_millis(2));
    }
    let (rgba, width, height) =
        aterm_render::decode_png_rgba8(&std::fs::read(&path).expect("snapshot PNG"))
            .expect("a PNG");
    let text = std::fs::read_to_string(snapshot_sidecar_path(&path, ".txt")).expect("text");
    ((width, height, rgba), text)
}

/// A headless `image` of the front window, decoded to `(width, height, rgba)`.
fn headless_image(app: &mut App, clean: bool) -> (usize, usize, Vec<u8>) {
    headless(app, |app| {
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean,
            session: None,
            want_bytes: true,
            want_metadata: false,
            frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
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
        (width, height, rgba)
    })
}

/// Run `image --meta` on the front window and return `((width, height, rgba), phase, capture_serial)`.
fn image_meta(app: &mut App, clean: bool) -> ((usize, usize, Vec<u8>), &'static str, u64) {
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
fn glass_rgba(app: &mut App, rect: (usize, usize, usize, usize)) -> Vec<u8> {
    let ((surface_w, _), pixels) = glass(app);
    let (x0, y0, width, height) = rect;
    (y0..y0 + height)
        .flat_map(|y| pixels[y * surface_w + x0..y * surface_w + x0 + width].to_vec())
        .flat_map(|pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8, 0xFF])
        .collect()
}

/// Configured wallpaper asset stripped by `image plain`.
fn wallpaper() -> std::sync::Arc<crate::app_config::ConfigAssetCatalog> {
    std::sync::Arc::new(crate::app_config::ConfigAssetCatalog {
        wallpaper: crate::app_config::WallpaperAsset::Ready {
            source_id: "test-wallpaper".into(),
            w: 2,
            h: 2,
            rgba: std::sync::Arc::from([0x20_u8, 0x60, 0xE0, 0xFF].repeat(4)),
            fp: 0xA61EE,
        },
        ..crate::app_config::ConfigAssetCatalog::default()
    })
}

#[test]
fn a_capture_is_bound_only_to_a_completed_present() {
    for native in [false, true] {
        let mut app = scripted_app();
        if native {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        } else {
            feed(&app, b"A");
        }
        let serial = app.windows[&WID].capture_present_serial;

        drop_every_attempt(&mut app);
        let error = barrier(&mut app, PresentTake::Model).expect_err("every present dropped");
        assert!(error.contains("could not synchronize"), "{error}");
        assert_eq!(app.windows[&WID].capture_present_serial, serial);

        app.windows.get_mut(&WID).expect("window").drag_hover = true;
        let presented = barrier(&mut app, PresentTake::Model)
            .expect("the present completes")
            .expect("the scripted surface is windowed");
        let ws = &app.windows[&WID];
        assert_eq!(presented.serial, serial + 1, "native={native}");
        assert_eq!(ws.capture_present_serial, presented.serial);
        assert_eq!(
            presented.visuals,
            HostVisualState {
                invert: ws.capture_present_invert,
                overlay: ws.capture_present_overlay,
            }
        );
        assert!(
            presented.visuals.overlay.is_some(),
            "native={native}: the drop wash was painted"
        );
        assert!(presented.client.is_none(), "a model take arms no tap");

        if !native {
            let copy = app.take_rerender_input(WID);
            assert_eq!(copy.cells[0][0].ch, 'A');
            app.windows
                .get_mut(&WID)
                .expect("window")
                .input_scratch
                .cells[0][0]
                .ch = 'B';
            assert_eq!(copy.cells[0][0].ch, 'A');
        }
    }
}

#[test]
fn a_split_capture_binds_the_presented_divider_generation() {
    let mut app = scripted_app();
    app.split_active_stub_tab(WID);
    let (rows, cols) = {
        let window = &app.windows[&WID];
        (window.rows, window.cols)
    };
    assert!(
        barrier(&mut app, PresentTake::Model)
            .expect("presented")
            .is_some()
    );
    let grid = app
        .presented_terminal_capture_grid(WID)
        .expect("split inventory");
    assert!(grid.composed);
    assert_eq!(grid.leaves.len(), 2);
    let widths_a: Vec<_> = grid.leaves.iter().map(|leaf| leaf.cols).collect();

    let hit = app
        .active_tree(WID)
        .expect("tree")
        .divider_at(5, 40, rows, cols)
        .expect("initial divider");
    let ratio = app
        .active_tree(WID)
        .expect("tree")
        .ratio_for_pointer(&hit, 5, 20)
        .expect("new divider ratio");
    assert!(
        app.active_tree_mut(WID)
            .expect("tree")
            .set_divider_ratio(&hit, ratio)
    );
    assert!(
        app.sync_tab_model_from_layout(WID, 0),
        "the shipping terminal-divider path updates canonical geometry too"
    );

    drop_every_attempt(&mut app);
    assert!(barrier(&mut app, PresentTake::Model).is_err());

    assert!(
        barrier(&mut app, PresentTake::Model)
            .expect("presented")
            .is_some()
    );
    let grid = app
        .presented_terminal_capture_grid(WID)
        .expect("split inventory");
    let widths_b: Vec<_> = grid.leaves.iter().map(|leaf| leaf.cols).collect();
    assert_ne!(widths_b, widths_a);
    assert_eq!(widths_b.iter().sum::<usize>() + 1, usize::from(cols));
}

/// A windowed snapshot writes the destination its own present submitted and the text of the model that present rendered.
#[test]
fn a_windowed_snapshot_is_the_presented_destination() {
    let mut app = scripted_app();
    feed(&app, b"hello");
    app.windows.get_mut(&WID).expect("window").last_present = None;
    app.redraw_window(WID);
    let ((fw, fh), _) = glass(&mut app);
    let surface_size = (fw + 3, fh + 2);
    app.windows.get_mut(&WID).expect("window").win_px = Some(aterm_window::dpi::PhysicalSize::new(
        u32::try_from(surface_size.0).expect("width"),
        u32::try_from(surface_size.1).expect("height"),
    ));
    let dir = private_dir("snapshot");

    let ((width, height, rgba), text) = snapshot(&mut app, &dir, "windowed.png");
    let (size, on_glass) = glass(&mut app);
    assert_eq!(
        size, surface_size,
        "the barrier presented at the surface size"
    );
    assert_eq!((width, height), size);
    let expected: Vec<u8> = on_glass
        .iter()
        .flat_map(|&pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8, 0xFF])
        .collect();
    assert!(rgba == expected, "the snapshot is the glass, byte for byte");
    assert!(text.contains("hello"), "{text:?}");

    let ((headless_width, _, _), headless_text) =
        headless(&mut app, |app| snapshot(app, &dir, "headless.png"));
    assert_ne!(headless_width, width);
    assert!(headless_text.contains("hello"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A windowed `image` during a recording is bound to its own barrier rather than the recording's last present.
#[test]
fn windowed_recording_image_is_bound_to_its_barrier() {
    let mut app = scripted_app();
    feed(&app, b"recorded");
    let dir = private_dir("recording");
    let (video_reply, _video_rx) = std::sync::mpsc::channel();
    app.video_rec = Some(crate::VideoRec {
        window: WID,
        deadline: Instant::now() + Duration::from_secs(3_600),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: crate::VideoMode::SwapchainTap,
        next_frame: None,
        presented: None,
        dir: crate::control_auth::confine_video_dir(&dir).expect("recording directory"),
        handoff: None,
        cancel: crate::VideoCancellation::new(),
        reply: video_reply,
    });
    let image = |app: &mut App| {
        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: true,
            frame_metadata: std::sync::Arc::clone(&metadata),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        });
        rx.recv_timeout(HANG)
            .expect("image reply")
            .map(|_| metadata.get().expect("image metadata").capture_serial)
    };

    let before = app.windows[&WID].capture_present_serial;
    let serial = image(&mut app).expect("the windowed image is bound to its barrier");
    assert!(serial > before, "the barrier presented");
    assert_eq!(serial, app.windows[&WID].capture_present_serial);

    let error = headless(&mut app, image).expect_err("a headless still waits for the recording");
    assert!(
        error.contains("recording has not presented a frame yet"),
        "{error}"
    );
    let _ = app.video_rec.take();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Windowed snapshot and `window` captures read the presented model in place without cloning `RenderInput`.
#[test]
fn exact_and_window_captures_clone_no_render_input() {
    let mut app = scripted_app();
    feed(&app, b"model");
    let dir = private_dir("clones");
    let _ = work_counts::take_capture_inputs_cloned();

    let _ = snapshot(&mut app, &dir, "snapshot.png");
    assert_eq!(work_counts::take_capture_inputs_cloned(), 0, "snapshot");

    #[cfg(any(target_os = "macos", windows))]
    {
        let commits = surface(&mut app).commits();
        let (reply, rx) = std::sync::mpsc::channel();
        app.capture_window(
            crate::control::ReplyRetentionPermit::from_own_budget(),
            crate::control_auth::ConfinedImage::for_test(&dir, "window.png"),
            crate::control::CaptureCancellation::new(),
            reply,
        );
        let answer = rx.recv_timeout(HANG).expect("the window capture answers");
        assert!(
            answer.is_err(),
            "a scripted surface has no platform window to photograph"
        );
        assert_eq!(
            surface(&mut app).commits(),
            commits + 1,
            "the window barrier presented and tapped the destination"
        );
        assert_eq!(work_counts::take_capture_inputs_cloned(), 0, "window");
    }

    let (reply, rx) = std::sync::mpsc::channel();
    app.render_image(crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: crate::control_auth::ConfinedImage::in_memory(),
        clean: true,
        session: None,
        want_bytes: true,
        want_metadata: false,
        frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
        cancel: crate::control::CaptureCancellation::new(),
        reply,
    });
    let _ = rx
        .recv_timeout(HANG)
        .expect("image reply")
        .expect("image succeeds");
    assert_eq!(work_counts::take_capture_inputs_cloned(), 1, "image plain");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Capture-forced presents book their latency sample to `capture_presents` rather than ordinary presents.
#[test]
fn forced_capture_presents_are_tainted() {
    let mut app = scripted_app();
    app.windows.get_mut(&WID).expect("window").last_present = None;
    app.redraw_window(WID);
    let arm_output = |app: &App| {
        let now = u64::try_from(app.lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
        for session in app.pool.iter() {
            session
                .last_output_ns
                .store(now.saturating_sub(1_000).max(1), Ordering::Relaxed);
        }
    };
    let _ = work_counts::take_present_samples();

    arm_output(&app);
    assert!(
        barrier(&mut app, PresentTake::Model)
            .expect("presented")
            .is_some()
    );
    assert_eq!(
        work_counts::take_present_samples(),
        (1, 0),
        "the barrier's present booked one capture_presents sample"
    );

    arm_output(&app);
    app.windows.get_mut(&WID).expect("window").last_present = None;
    app.redraw_window(WID);
    assert_eq!(
        work_counts::take_present_samples(),
        (0, 1),
        "an ordinary present books its sample and no capture_presents sample"
    );
}

/// Headless `image` and headless snapshot produce identical frames across all tab shapes.
#[test]
fn headless_snapshot_and_image_rerender_agree() {
    let dir = private_dir("rerender");
    for shape in ["terminal", "split", "native", "mixed"] {
        let mut app = scripted_app();
        app.config_assets = wallpaper();
        feed(&app, b"agree");
        match shape {
            "split" => {
                app.split_active_stub_tab(WID);
            }
            "native" => {
                assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
            }
            "mixed" => {
                assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
                app.split_active_with_stub_terminal(WID, crate::tab_model::SplitAxis::Horizontal);
            }
            _ => {}
        }
        let imaged = headless_image(&mut app, false);
        let (snapshotted, _) =
            headless(&mut app, |app| snapshot(app, &dir, &format!("{shape}.png")));
        assert_eq!(
            (snapshotted.0, snapshotted.1),
            (imaged.0, imaged.1),
            "{shape}"
        );
        assert!(
            snapshotted.2 == imaged.2,
            "{shape}: the snapshot is the image's frame"
        );
    }

    let mut app = scripted_app();
    app.config_assets = wallpaper();
    let (snapshotted, _) = headless(&mut app, |app| snapshot(app, &dir, "wallpaper.png"));
    let plain = headless_image(&mut app, true);
    assert_eq!((plain.0, plain.1), (snapshotted.0, snapshotted.1));
    assert!(plain.2 != snapshotted.2, "the plain image has no wallpaper");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A windowed `image` crops the presented destination to the frame placement without rerendering.
#[test]
fn windowed_image_is_the_presented_destination() {
    let mut app = scripted_app();
    app.config_assets = wallpaper();
    feed(&app, b"destination");
    app.windows.get_mut(&WID).expect("window").last_present = None;
    app.redraw_window(WID);
    let ((fw, fh), _) = glass(&mut app);
    let set_surface = |app: &mut App, (width, height): (usize, usize)| {
        app.windows.get_mut(&WID).expect("window").win_px =
            Some(aterm_window::dpi::PhysicalSize::new(
                u32::try_from(width).expect("width"),
                u32::try_from(height).expect("height"),
            ));
    };

    set_surface(&mut app, (fw + 3, fh + 2));
    let _ = work_counts::take_capture_inputs_cloned();
    let ((width, height, rgba), phase, serial) = image_meta(&mut app, false);
    assert_eq!(
        work_counts::take_capture_inputs_cloned(),
        0,
        "the image rendered nothing of its own"
    );
    let (surface, _) = glass(&mut app);
    assert_eq!(
        surface,
        (fw + 3, fh + 2),
        "the barrier presented at the surface size"
    );
    assert_eq!(
        (width, height),
        (fw, fh),
        "the image is the frame, not the surface"
    );
    let x0 = usize::try_from(aterm_render::band_offset(fw + 3, fw)).expect("a band");
    let y0 = usize::try_from(aterm_render::band_offset_y(fh + 2, fh)).expect("a band");
    assert!(
        rgba == glass_rgba(&mut app, (x0, y0, fw, fh)),
        "the image is the glass inside the frame's placement"
    );
    assert_eq!(phase, "destination");
    assert_eq!(serial, app.windows[&WID].capture_present_serial);

    let ((plain_width, plain_height, plain), plain_phase, _) = image_meta(&mut app, true);
    assert_eq!(work_counts::take_capture_inputs_cloned(), 1, "image plain");
    assert_eq!((plain_width, plain_height), (fw, fh));
    assert_eq!(
        plain_phase, "presented",
        "a plain image rerenders the present's model"
    );
    assert!(
        plain != glass_rgba(&mut app, (x0, y0, fw, fh)),
        "the plain image is not the glass"
    );

    set_surface(&mut app, (fw - 5, fh - 3));
    let ((width, height, rgba), phase, _) = image_meta(&mut app, false);
    assert_eq!((width, height), (fw - 5, fh - 3));
    assert!(rgba == glass_rgba(&mut app, (0, 0, fw - 5, fh - 3)));
    assert_eq!(phase, "destination");
}

/// Verify [`capture_source_kind`] across all `(windowed, clean, translucent, copyable, recording)` combinations and platform guards.
#[test]
fn image_source_policy_table() {
    use SourceKind::{Destination as D, Rerender as R};
    /// `(windowed, clean, translucent, copyable, recording)`.
    type Row = (bool, bool, bool, bool, bool);
    let table: [(Row, SourceKind); 32] = [
        ((false, false, false, false, false), R),
        ((false, false, false, false, true), D),
        ((false, false, false, true, false), R),
        ((false, false, false, true, true), D),
        ((false, false, true, false, false), R),
        ((false, false, true, false, true), D),
        ((false, false, true, true, false), R),
        ((false, false, true, true, true), D),
        ((false, true, false, false, false), R),
        ((false, true, false, false, true), R),
        ((false, true, false, true, false), R),
        ((false, true, false, true, true), R),
        ((false, true, true, false, false), R),
        ((false, true, true, false, true), R),
        ((false, true, true, true, false), R),
        ((false, true, true, true, true), R),
        ((true, false, false, false, false), R),
        ((true, false, false, false, true), R),
        ((true, false, false, true, false), D),
        ((true, false, false, true, true), D),
        ((true, false, true, false, false), R),
        ((true, false, true, false, true), R),
        ((true, false, true, true, false), R),
        ((true, false, true, true, true), R),
        ((true, true, false, false, false), R),
        ((true, true, false, false, true), R),
        ((true, true, false, true, false), R),
        ((true, true, false, true, true), R),
        ((true, true, true, false, false), R),
        ((true, true, true, false, true), R),
        ((true, true, true, true, false), R),
        ((true, true, true, true, true), R),
    ];
    let facts = |windowed, translucent, copyable, recording| SourceFacts {
        windowed,
        translucent,
        guards: Ok(()),
        copyable,
        recording,
    };
    let image = DestinationDemand::of_image;
    for ((windowed, clean, translucent, copyable, recording), expected) in table {
        let row = (windowed, clean, translucent, copyable, recording);
        assert_eq!(
            capture_source_kind(
                image(clean),
                facts(windowed, translucent, copyable, recording)
            ),
            Ok(expected),
            "{row:?}"
        );
    }

    let opaque = capture_source_kind(image(false), facts(true, false, true, false));
    let translucent = capture_source_kind(image(false), facts(true, true, true, false));
    assert_eq!((opaque, translucent), (Ok(D), Ok(R)));

    let snapshot = DestinationDemand::Required;
    let solid = RenderKnobs {
        background_opacity: 1.0,
        background_material: BackgroundMaterial::None,
        ..RenderKnobs::default()
    };
    let see_through = RenderKnobs {
        background_opacity: 0.75,
        ..solid
    };
    let material = RenderKnobs {
        background_material: BackgroundMaterial::Hud,
        ..solid
    };
    let (mac, win, linux) = (crate::MAC_CAPTURE, crate::WIN_CAPTURE, crate::LINUX_CAPTURE);
    let blended = Err(crate::TRANSLUCENT_CAPTURE_ERROR);
    let unphotographed = Err(crate::MATERIAL_CAPTURE_ERROR);
    let platforms = [
        ("macOS solid", mac, &solid, Ok(D), Ok(D)),
        ("macOS translucent", mac, &see_through, Ok(R), blended),
        ("macOS material", mac, &material, Ok(D), Ok(D)),
        ("Windows solid", win, &solid, Ok(D), Ok(D)),
        ("Windows translucent", win, &see_through, Ok(R), Ok(D)),
        ("Windows material", win, &material, Ok(R), unphotographed),
        ("Linux solid", linux, &solid, Ok(D), Ok(D)),
        ("Linux translucent", linux, &see_through, Ok(R), Ok(D)),
        ("Linux material", linux, &material, Ok(D), Ok(D)),
    ];
    for (row, traits, knobs, image_kind, snapshot_kind) in platforms {
        let window = SourceFacts {
            windowed: true,
            translucent: crate::translucent_gpu_client(knobs, true, true),
            guards: crate::platform_capture_guards(traits, knobs, true, true),
            copyable: true,
            recording: false,
        };
        assert_eq!(
            capture_source_kind(image(false), window),
            image_kind,
            "{row}: image"
        );
        assert_eq!(
            capture_source_kind(snapshot, window),
            snapshot_kind,
            "{row}: snapshot"
        );
    }
    assert_eq!(
        capture_source_kind(snapshot, facts(true, false, false, false)),
        Ok(D)
    );
    assert_eq!(
        capture_source_kind(snapshot, facts(false, false, false, true)),
        Ok(D)
    );
    assert_eq!(
        capture_source_kind(snapshot, facts(false, false, false, false)),
        Ok(R)
    );
}

/// Destination-copying captures (`image` and `Snapshot`) arm the client tap while `image plain` does not.
#[test]
fn image_requests_own_the_tap_while_waiting() {
    let image = |clean: bool| {
        let (reply, _rx) = std::sync::mpsc::channel();
        CaptureRequest::Image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean,
            session: None,
            want_bytes: true,
            want_metadata: false,
            frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        })
    };
    assert!(image(false).takes_client_tap());
    assert!(!image(true).takes_client_tap(), "image plain rerenders");
    assert!(
        CaptureRequest::Snapshot {
            path: String::new()
        }
        .takes_client_tap()
    );
}

/// Windowed snapshot and `image --meta` run pixel conversion, cropping, fingerprinting, and PNG encoding on the worker thread.
#[test]
fn no_pixel_work_runs_on_the_submitting_thread() {
    let mut app = scripted_app();
    feed(&app, b"acquire");
    app.windows.get_mut(&WID).expect("window").last_present = None;
    app.redraw_window(WID);
    let ((fw, fh), _) = glass(&mut app);
    app.windows.get_mut(&WID).expect("window").win_px = Some(aterm_window::dpi::PhysicalSize::new(
        u32::try_from(fw + 3).expect("width"),
        u32::try_from(fh + 2).expect("height"),
    ));
    let dir = private_dir("acquire");
    let here = std::thread::current().id();

    let ((width, height, _), _) = snapshot(&mut app, &dir, "snapshot.png");
    assert_eq!((width, height), (fw + 3, fh + 2), "the whole destination");
    let ((width, height, _), phase, _) = image_meta(&mut app, false);
    assert_eq!((width, height), (fw, fh), "cut to its frame");
    assert_eq!(phase, "destination");
    assert!(
        !ran_pixel_work(here),
        "the thread that took the captures ran none of their pixel work"
    );

    let pixels = CapturePixels::softbuffer(vec![0; 4], 2, 2).expect("a 2x2 softbuffer");
    CapturePng::encode(&pixels).expect("encodes");
    assert!(ran_pixel_work(here));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The encode worker publishes `image --meta` frame metadata before sending the reply.
#[test]
fn metadata_is_set_before_the_reply() {
    let mut app = scripted_app();
    feed(&app, b"published");
    let request = |app: &mut App, metadata: &std::sync::Arc<std::sync::OnceLock<_>>| {
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: true,
            frame_metadata: std::sync::Arc::clone(metadata),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        });
        rx.recv_timeout(HANG).expect("image reply")
    };

    let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    let (_, _, png) = request(&mut app, &metadata).expect("image succeeds").value;
    let published: crate::control::ImageFrameMetadata = metadata
        .get()
        .cloned()
        .expect("published before the reply arrived");
    let (rgba, width, height) =
        aterm_render::decode_png_rgba8(&png.expect("PNG bytes")).expect("a PNG");
    let packed: Vec<u32> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[r, g, b, a]| {
            (u32::from(255 - a) << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        })
        .collect();
    let encoded = App::image_pixel_fingerprint(width, height, &packed);
    assert_eq!(published.phase, "destination");
    assert_eq!(published.pixel_fingerprint, encoded);
    assert_eq!(published.raster_fingerprint, encoded);
    assert_eq!(published.leaves[0].raster_fingerprint, Some(encoded));

    let first = crate::control::ImageFrameMetadata {
        pixel_fingerprint: 7,
        ..published
    };
    let metadata = std::sync::Arc::new(std::sync::OnceLock::from(first.clone()));
    let error = request(&mut app, &metadata).expect_err("already published");
    assert!(error.contains("already initialized"), "{error}");
    assert_eq!(metadata.get(), Some(&first));
}
