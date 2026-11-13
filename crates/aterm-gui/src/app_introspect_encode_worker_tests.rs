// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::app_render::apply_overlay_at;
use crate::control_auth::ensure_private_dir;
use std::time::Duration;

#[cfg(target_os = "macos")]
#[test]
fn image_renders_the_frame_and_never_photographs_the_window() {
    let mut app = App::headless_for_test();
    let photographs = || OS_WINDOW_PHOTOGRAPHS.with(std::cell::Cell::get);
    let before = photographs();
    let dir = aterm_tempfile::TempDir::new().expect("an image dir");
    ensure_private_dir(dir.path()).expect("a private dir");
    let (tx, rx) = std::sync::mpsc::channel();
    app.render_image(crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: control_auth::ConfinedImage::for_test(dir.path(), "frame.png"),
        clean: false,
        session: None,
        want_bytes: true,
        want_metadata: false,
        frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
        cancel: crate::control::CaptureCancellation::new(),
        reply: tx,
    });
    let (width, height, png) = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the image reply")
        .expect("a headless image succeeds")
        .value;
    assert!(width > 0 && height > 0, "{width}x{height}");
    assert!(
        png.is_some_and(|png| png.starts_with(b"\x89PNG")),
        "the PNG over the wire"
    );
    assert_eq!(
        photographs(),
        before,
        "`image` asked the OS for no photograph"
    );
    assert!(
        app.window_rgba_of(WindowId(0)).is_err(),
        "a headless window has nothing on screen to photograph"
    );
    assert_eq!(
        photographs(),
        before + 1,
        "control: a photograph asked for counts"
    );
}

fn unique_dir(label: &str) -> std::path::PathBuf {
    let nonce = aterm_time::since_epoch()
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("aterm-{label}-{}-{nonce}", std::process::id()))
}

fn confined(dir: &std::path::Path, name: &str) -> control_auth::ConfinedImage {
    control_auth::ConfinedImage::for_test(dir, name)
}

fn retained_capture(
    app: &App,
    input: aterm_render::RenderInput,
    visuals: crate::app_render::HostVisualState,
) -> ResolvedCapture {
    let (plan, route) = app
        .active_visible_frame_layout(crate::WindowId(0))
        .expect("a visible layout");
    ResolvedCapture {
        source: CaptureSource::Rerender {
            serial: Some(1),
            visuals,
        },
        model: Some(input),
        context: CaptureContext {
            plan,
            route,
            terminal_grid: None,
            recorded: None,
        },
    }
}

fn visual_capture_pixels(
    app: &mut App,
    presented: Option<(
        aterm_render::RenderInput,
        crate::app_render::HostVisualState,
    )>,
) -> Frame {
    let (reply, rx) = std::sync::mpsc::channel();
    let req = image_req_for_test(
        control_auth::ConfinedImage::in_memory(),
        true,
        true,
        None,
        reply,
    );
    if let Some((input, visuals)) = presented {
        let capture = retained_capture(app, input, visuals);
        app.image_tail(crate::WindowId(0), capture, req);
    } else {
        app.render_image(req);
    }
    let (_, _, png) = rx
        .recv_timeout(WORKER_PATIENCE)
        .expect("visual capture worker reply")
        .expect("visual capture succeeds")
        .value;
    rgb_frame_of_png(&png.expect("capture PNG bytes"))
}

fn snapshot_capture_pixels(app: &mut App, dir: &std::path::Path, name: &str) -> Frame {
    let path = dir.join(name);
    let done = snapshot_sidecar_path(&path, ".done");
    app.snapshot_at_path(path.display().to_string());
    let deadline = std::time::Instant::now() + WORKER_PATIENCE;
    while !done.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the snapshot was published"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    rgb_frame_of_png(&std::fs::read(&path).expect("snapshot PNG"))
}

fn rgb_frame_of_png(png: &[u8]) -> Frame {
    let (rgba, width, height) = aterm_render::decode_png_rgba8(png).unwrap();
    let (pixels, remainder) = rgba.as_chunks::<4>();
    assert!(remainder.is_empty());
    Frame {
        width,
        height,
        pixels: pixels
            .iter()
            .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
            .collect(),
    }
}

#[test]
fn capture_visual_policy_terminal_fallback_keeps_charging_over_a_palette() {
    type Capture = fn(&mut App, &std::path::Path, &str) -> Frame;
    let image: Capture = |app, _, _| visual_capture_pixels(app, None);
    let snapshot: Capture = snapshot_capture_pixels;
    for (verb, capture) in [("image", image), ("snapshot", snapshot)] {
        charging_stays_over_a_palette(verb, capture);
    }
}

fn charging_stays_over_a_palette(
    verb: &str,
    capture: fn(&mut App, &std::path::Path, &str) -> Frame,
) {
    let dir = unique_dir("capture-visual-charging");
    ensure_private_dir(&dir).unwrap();
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    app.palette_enter();
    assert!(app.windows[&wid].overlay_open());
    assert!(app.windows[&wid].os_window.is_none());
    assert_eq!(app.windows[&wid].capture_present_serial, 0);
    let baseline = capture(&mut app, &dir, "baseline.png");

    let now = Instant::now();
    app.level_up = Some(crate::level_up::LevelUp::charging_continued(7, now, 0.0));
    let window = app.windows.get_mut(&wid).unwrap();
    window.drag_hover = true;
    window.bell_flash.ring(now);
    let visuals = app.host_visual_state(wid, now);
    assert!(!visuals.invert, "a modal still suppresses the bell");
    let overlay = visuals.overlay.expect("charging remains admitted");
    let charged = capture(&mut app, &dir, "charging.png");
    assert_eq!(
        (charged.width, charged.height),
        (baseline.width, baseline.height),
        "{verb}"
    );
    let mut expected = baseline.clone();
    apply_overlay_at(
        &mut expected.pixels,
        expected.width,
        expected.height,
        0,
        0,
        expected.width,
        expected.height,
        overlay,
    );
    assert_ne!(expected.pixels[0], baseline.pixels[0], "{verb}");
    assert_eq!(
        charged.pixels[0], expected.pixels[0],
        "{verb}: the charging rim, over the palette, with no bell"
    );

    app.level_up = None;
    assert_eq!(app.host_visual_state(wid, Instant::now()).overlay, None);
    let committed = capture(&mut app, &dir, "committed.png");
    assert_eq!(
        committed.pixels[0], baseline.pixels[0],
        "{verb}: the drag yields to the modal"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capture_visual_policy_retained_absence_ignores_a_new_live_glow() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(app.prepare_native_input_scratch(wid));
    let retained = app.windows[&wid].input_scratch.clone();
    let absent = crate::app_render::HostVisualState::default();
    let baseline = visual_capture_pixels(&mut app, Some((retained.clone(), absent)));
    app.level_up = Some(crate::level_up::LevelUp::charging_continued(
        8,
        Instant::now(),
        0.0,
    ));
    let overlay = app
        .host_visual_state(wid, Instant::now())
        .overlay
        .expect("live glow");
    let captured = visual_capture_pixels(&mut app, Some((retained.clone(), absent)));
    assert_eq!(
        captured.pixels, baseline.pixels,
        "retained None is authoritative, not a request to resample"
    );

    let painted = visual_capture_pixels(
        &mut app,
        Some((
            retained,
            crate::app_render::HostVisualState {
                overlay: Some(overlay),
                ..absent
            },
        )),
    );
    assert_ne!(
        painted.pixels[0], baseline.pixels[0],
        "the retained overlay path really reaches encoded pixels"
    );
}

#[test]
fn encode_worker_replies_fifo_and_only_after_write() {
    let root = unique_dir("encode-worker");
    ensure_private_dir(&root).unwrap();
    let mut app = App::headless_for_test();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut paths = Vec::new();
    for (i, side) in [1usize, 2, 3].into_iter().enumerate() {
        let target = control_auth::confine_automatic_image_path(&root, &format!("shot{i}"))
            .expect("confine automatic image target");
        paths.push(target.display_path());
        app.submit_encode_job(EncodeJob::Image {
            pixels: CapturePixels::packed(Frame {
                width: side,
                height: side,
                pixels: vec![0u32; side * side],
            })
            .expect("a whole frame"),
            metadata: None,
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target,
            want_bytes: false,
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx.clone(),
        });
    }
    for (i, side) in [1u32, 2, 3].into_iter().enumerate() {
        let mut retained = rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("worker reply")
            .expect("encode succeeds");
        assert_eq!(
            retained.value,
            (side, side, None),
            "reply {i} out of FIFO order"
        );
        retained
            .retention
            .as_mut()
            .expect("file reply carries exact handles")
            .prepare_write()
            .expect("wire-edge identity remains exact");
        let bytes = std::fs::read(&paths[i]).expect("file written before reply");
        assert!(
            bytes.starts_with(&[0x89, b'P', b'N', b'G']),
            "PNG signature missing"
        );
    }
    assert!(app.encode_tx.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_saturated_encoder_answers_busy_before_capturing() {
    let dir = unique_dir("capture-permit-busy");
    ensure_private_dir(&dir).unwrap();
    let mut app = App::headless_for_test();
    let held = CapturePermit::try_reserve(&app.capture_bytes, CAPTURE_BYTE_BUDGET)
        .expect("the budget is empty");
    let bodies = CAPTURE_BODIES.with(std::cell::Cell::get);
    let (reply, rx) = std::sync::mpsc::channel();
    app.render_image(image_req_for_test(
        confined(&dir, "busy.png"),
        true,
        true,
        None,
        reply,
    ));
    let error = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the refusal replies at once")
        .expect_err("a saturated encoder refuses the capture");
    assert!(error.contains(ENCODE_QUEUE_FULL), "{error}");
    assert_eq!(
        CAPTURE_BODIES.with(std::cell::Cell::get),
        bodies,
        "no capture body ran: nothing was photographed"
    );
    assert!(app.encode_tx.is_none(), "nothing reached the encoder");
    assert!(app.capture_permit.is_none());
    assert_eq!(
        app.capture_bytes.load(std::sync::atomic::Ordering::Acquire),
        CAPTURE_BYTE_BUDGET,
        "the refusal reserved nothing"
    );

    let path = dir.join("snapshot.png");
    let done = snapshot_sidecar_path(&path, ".done");
    std::fs::write(&done, b"stale\n").unwrap();
    app.snapshot_at_path(path.display().to_string());
    assert!(!done.exists(), "a refused snapshot publishes no completion");
    assert!(!path.exists(), "and writes no pixels");
    assert_eq!(CAPTURE_BODIES.with(std::cell::Cell::get), bodies);

    drop(held);
    let frame = visual_capture_pixels(&mut app, None);
    assert!(
        frame.width > 0 && frame.height > 0,
        "admitted once the budget frees"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[derive(Clone, Copy, Debug)]
enum CaptureKind {
    Image,
    Snapshot,
    #[cfg(any(target_os = "macos", windows))]
    Window,
}

impl CaptureKind {
    const ALL: &[CaptureKind] = &[
        CaptureKind::Image,
        CaptureKind::Snapshot,
        #[cfg(any(target_os = "macos", windows))]
        CaptureKind::Window,
    ];

    fn enter(
        self,
        app: &mut App,
        dir: &std::path::Path,
        name: &str,
        admitted: bool,
    ) -> Option<Result<(), String>> {
        match self {
            CaptureKind::Image => {
                let (reply, rx) = std::sync::mpsc::channel();
                app.render_image(crate::control::ImageReq {
                    handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                    target: confined(dir, name),
                    clean: true,
                    session: None,
                    want_bytes: false,
                    want_metadata: false,
                    frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
                    cancel: crate::control::CaptureCancellation::new(),
                    reply,
                });
                Some(
                    rx.recv_timeout(WORKER_PATIENCE)
                        .expect("the image answers")
                        .map(drop),
                )
            }
            CaptureKind::Snapshot => {
                let path = dir.join(name);
                let done = snapshot_sidecar_path(&path, ".done");
                app.snapshot_at_path(path.display().to_string());
                if admitted {
                    let deadline = std::time::Instant::now() + WORKER_PATIENCE;
                    while !done.exists() {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "the admitted snapshot was published"
                        );
                        std::thread::sleep(Duration::from_millis(2));
                    }
                } else {
                    assert!(!done.exists() && !path.exists(), "nothing was written");
                }
                None
            }
            #[cfg(any(target_os = "macos", windows))]
            CaptureKind::Window => {
                let (reply, rx) = std::sync::mpsc::channel();
                app.capture_window(
                    crate::control::ReplyRetentionPermit::from_own_budget(),
                    confined(dir, name),
                    crate::control::CaptureCancellation::new(),
                    reply,
                );
                Some(
                    rx.recv_timeout(WORKER_PATIENCE)
                        .expect("the window capture answers")
                        .map(drop),
                )
            }
        }
    }
}

#[test]
fn run_capture_admits_every_kind_once() {
    let dir = unique_dir("capture-door");
    ensure_private_dir(&dir).unwrap();
    let mut app = App::headless_for_test();
    let held = CapturePermit::try_reserve(&app.capture_bytes, CAPTURE_BYTE_BUDGET)
        .expect("the budget is empty");
    for &kind in CaptureKind::ALL {
        let bodies = capture_bodies();
        if let Some(reply) = kind.enter(&mut app, &dir, &format!("busy-{kind:?}.png"), false) {
            let error = reply.expect_err("a saturated encoder refuses the capture");
            assert!(error.contains(ENCODE_QUEUE_FULL), "{kind:?}: {error}");
        }
        assert_eq!(capture_bodies(), bodies, "{kind:?}: no body ran");
    }
    drop(held);
    for &kind in CaptureKind::ALL {
        let bodies = capture_bodies();
        let reply = kind.enter(&mut app, &dir, &format!("{kind:?}.png"), true);
        assert_eq!(
            capture_bodies(),
            bodies + 1,
            "{kind:?}: admitted once ({reply:?})"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn feed_a_blocked_encoder(
    app: &mut App,
    mut admit: impl FnMut(&mut App) -> bool,
    mut job: impl FnMut(u32) -> EncodeJob,
) -> (Vec<u32>, Vec<usize>, std::sync::mpsc::Receiver<EncodeWork>) {
    let (tx, blocked) = std::sync::mpsc::sync_channel::<EncodeWork>(64);
    app.encode_tx = Some(tx);
    let mut admitted = Vec::new();
    let mut in_flight = Vec::new();
    for i in 0..20_u32 {
        if !admit(app) {
            continue;
        }
        app.submit_encode_job(job(i));
        app.capture_permit = None;
        admitted.push(i);
        in_flight.push(app.capture_bytes.load(std::sync::atomic::Ordering::Acquire));
    }
    (admitted, in_flight, blocked)
}

fn image_job(
    pixels: CapturePixels,
    reply: &std::sync::mpsc::Sender<crate::control::ImageReply>,
) -> EncodeJob {
    EncodeJob::Image {
        pixels,
        metadata: None,
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: control_auth::ConfinedImage::for_test(&std::env::temp_dir(), "unused"),
        want_bytes: true,
        cancel: crate::control::CaptureCancellation::new(),
        reply: reply.clone(),
    }
}

fn assert_each_holds_its_peak(label: &str, in_flight: &[usize], peak: usize) {
    for (n, &bytes) in in_flight.iter().enumerate() {
        assert_eq!(bytes, (n + 1) * peak, "{label}: capture {n} holds its peak");
        assert!(
            bytes <= CAPTURE_BYTE_BUDGET.max(peak),
            "{label}: {bytes} bytes in flight past the budget"
        );
    }
    assert_eq!(
        in_flight.len(),
        (CAPTURE_BYTE_BUDGET / peak).max(1),
        "{label}: exactly as many as the budget holds"
    );
}

#[test]
fn a_blocked_encoder_holds_at_most_its_byte_budget() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let (width, height) = (5120_usize, 2880_usize);
    let peak_bytes = width * height * 4 + capture_encode_overhead(width, height);
    app.windows.get_mut(&wid).unwrap().win_px = Some(aterm_window::dpi::PhysicalSize::new(
        u32::try_from(width).unwrap(),
        u32::try_from(height).unwrap(),
    ));
    let charge = CaptureCharge::Frame(DestinationDemand::Preferred);
    assert_eq!(app.capture_bytes_estimate(wid, charge), peak_bytes);
    let (reply, _replies) = std::sync::mpsc::channel();
    let (admitted, in_flight, blocked) = feed_a_blocked_encoder(
        &mut app,
        |app| app.admit_capture(Some(wid), charge),
        |i| {
            let frame = Frame {
                width,
                height,
                pixels: vec![i; width * height],
            };
            image_job(CapturePixels::packed(frame).expect("a whole frame"), &reply)
        },
    );
    assert!(admitted.len() < 20, "the budget refused once full");
    assert_each_holds_its_peak("5K", &in_flight, peak_bytes);
    for expect in &admitted {
        let work = blocked.try_recv().expect("an admitted capture was queued");
        match &work.job {
            EncodeJob::Image { pixels, .. } => {
                let mut row = Vec::new();
                pixels.rgba_row(0, &mut row);
                let blue = u8::try_from(*expect).expect("a small index");
                assert_eq!(row[..4], [0, 0, blue, 255], "FIFO order");
            }
            _ => panic!("an image job"),
        }
        drop(work);
    }
    assert!(blocked.try_recv().is_err(), "no refused capture was queued");
    assert_eq!(
        app.capture_bytes.load(std::sync::atomic::Ordering::Acquire),
        0,
        "draining the encoder returned every byte"
    );
}

#[test]
fn an_admitted_capture_is_charged_what_admission_reserved() {
    use aterm_gpu::video_tap::RawCapture;

    let (reply, _replies) = std::sync::mpsc::channel();
    let mapped = |width: usize, height: usize, bpp: usize| {
        let row = (width * bpp).div_ceil(256) * 256;
        (vec![0_u8; row * height], row)
    };

    let (width, height) = (1440_usize, 900_usize);
    let f16 = || {
        let (bytes, row) = mapped(width, height, 8);
        CapturePixels::raw(
            RawCapture::scrgb_f16_for_test(bytes, width as u32, height as u32, row)
                .expect("an f16 destination"),
        )
    };
    let f16_peak = f16().peak_bytes();
    let f16_estimate = frame_capture_peak(width, height, Some(f16().allocation()));
    assert_eq!(f16_estimate, f16_peak, "the f16 estimate is its charge");
    let mut app = App::headless_for_test();
    let (_, in_flight, _queued) = feed_a_blocked_encoder(
        &mut app,
        |app| app.admit_capture_bytes(f16_estimate),
        |_| image_job(f16(), &reply),
    );
    assert_each_holds_its_peak("f16", &in_flight, f16_peak);

    let frame_only = frame_capture_peak(width, height, None);
    assert!(frame_only < f16_peak);
    let mut app = App::headless_for_test();
    let (_, in_flight, _queued) = feed_a_blocked_encoder(
        &mut app,
        |app| app.admit_capture_bytes(frame_only),
        |_| image_job(f16(), &reply),
    );
    assert!(
        in_flight.iter().any(|&bytes| bytes > CAPTURE_BYTE_BUDGET),
        "negative control: the frame-only estimate overruns the budget"
    );

    let cut = || {
        let (bytes, row) = mapped(width + 1, height + 1, 4);
        let raw = RawCapture::bgra8_srgb_for_test(bytes, width as u32 + 1, height as u32 + 1, row)
            .expect("an 8-bit destination");
        CapturePixels::raw(raw)
            .cut_to(aterm_render::PixelRect {
                x: 1,
                y: 0,
                width,
                height,
            })
            .expect("the frame is inside its surface")
    };
    let cut_peak = cut().peak_bytes();
    assert_eq!(
        frame_capture_peak(width, height, Some(cut().allocation())),
        cut_peak,
        "the cut destination's estimate is its charge"
    );
    assert!(cut_peak > frame_capture_peak(width, height, None));
    let mut app = App::headless_for_test();
    let (_, in_flight, _queued) = feed_a_blocked_encoder(
        &mut app,
        |app| app.admit_capture_bytes(cut_peak),
        |_| image_job(cut(), &reply),
    );
    assert_each_holds_its_peak("cut", &in_flight, cut_peak);

    #[cfg(any(target_os = "macos", windows))]
    {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.win_px = Some(aterm_window::dpi::PhysicalSize::new(
            width as u32,
            height as u32,
        ));
        #[cfg(target_os = "macos")]
        let chrome_height = 56;
        #[cfg(target_os = "macos")]
        {
            ws.metrics.head = chrome_height;
        }
        let (window_reply, _window_replies) = std::sync::mpsc::channel();
        let stitch = || WindowStitch {
            shot: PlatformShot {
                pixels: vec![0; width * height * 4],
                width: width as u32,
                height: height as u32,
                encoding: ShotEncoding::NATIVE,
                #[cfg(target_os = "macos")]
                chrome: Some(ChromeLayer {
                    x: 0,
                    y: 0,
                    width: width as u32,
                    height: chrome_height as u32,
                    pixels: ChromePixels::Bitmap {
                        bytes: vec![0; width * chrome_height * 4],
                        desc: BitmapDesc {
                            width,
                            height: chrome_height,
                            bytes_per_row: width * 4,
                            bits_per_component: 8,
                            bits_per_pixel: 32,
                            bitmap_info: 1,
                            rgb: true,
                        },
                    },
                }),
            },
            client: CapturePixels::softbuffer(vec![0; width * height], width, height)
                .expect("a whole client"),
            rect: WindowClientRect {
                x: 0,
                y: 0,
                width: width as u32,
                height: height as u32,
            },
        };
        let window_peak = stitch().peak_bytes();
        assert_eq!(
            app.capture_bytes_estimate(wid, CaptureCharge::Window),
            window_peak,
            "the window estimate is its stitch's charge"
        );
        // The AppKit chrome and outer shape cost more than a frame; a
        // Windows stitch carries neither, so its client copy and row
        // fit inside the encoder's own peak.
        #[cfg(target_os = "macos")]
        assert!(window_peak > frame_capture_peak(width, height, None));
        #[cfg(windows)]
        assert_eq!(window_peak, frame_capture_peak(width, height, None));
        let (_, in_flight, _queued) = feed_a_blocked_encoder(
            &mut app,
            |app| app.admit_capture(Some(wid), CaptureCharge::Window),
            |_| EncodeJob::Window {
                stitch: stitch(),
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                target: control_auth::ConfinedImage::for_test(&std::env::temp_dir(), "unused"),
                cancel: crate::control::CaptureCancellation::new(),
                reply: window_reply.clone(),
            },
        );
        assert_each_holds_its_peak("window", &in_flight, window_peak);
    }
}

fn blank_metadata() -> crate::control::ImageFrameMetadata {
    crate::control::ImageFrameMetadata {
        frame_kind: "terminal",
        phase: "staged",
        window: 0,
        view: None,
        generation: None,
        config_revision: None,
        update_revision: None,
        document_seq: None,
        presentation_revision: None,
        paint_revision: None,
        capture_serial: 0,
        width: 0,
        height: 0,
        pixel_fingerprint: 0,
        compiled_fingerprint: None,
        raster_fingerprint: 0,
        raster_model_fingerprint: 0,
        raster_geometry: 0,
        overlay_fingerprint: 0,
        theme_fingerprint: 0,
        leaves: Vec::new(),
    }
}

#[test]
fn a_captures_bytes_are_free_before_its_answer() {
    let held = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for case in ["encoded", "cancelled", "unencodable", "republished"] {
        let (width, height) = if case == "unencodable" {
            (usize::try_from(u64::from(u32::MAX) + 1).expect("64-bit"), 0)
        } else {
            (64_usize, 64_usize)
        };
        let pixels = CapturePixels::packed(Frame {
            width,
            height,
            pixels: vec![0u32; width * height],
        })
        .expect("a whole frame");
        let metadata = (case == "republished").then(|| {
            let cell = std::sync::Arc::new(std::sync::OnceLock::new());
            let published = blank_metadata();
            cell.set(published.clone()).expect("first");
            Box::new(PendingMetadata {
                metadata: published,
                raster_is_pixels: false,
                cell,
            })
        });
        let permit =
            CapturePermit::try_reserve(&held, pixels.peak_bytes()).expect("the budget is empty");
        let cancel = crate::control::CaptureCancellation::new();
        if case == "cancelled" {
            assert!(cancel.cancel());
        }
        let (reply, rx) = std::sync::mpsc::channel::<Result<_, String>>();
        let observed = std::sync::Arc::clone(&held);
        let reader = std::thread::spawn(move || {
            let answer = rx.recv().expect("an answer");
            (
                answer.map(drop),
                observed.load(std::sync::atomic::Ordering::Acquire),
            )
        });
        run_encode_job(
            EncodeJob::Image {
                pixels,
                metadata,
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                target: control_auth::ConfinedImage::for_test(&std::env::temp_dir(), "unused"),
                want_bytes: true,
                cancel,
                reply,
            },
            Some(permit),
        );
        let (answer, at_answer) = reader.join().expect("reader");
        match case {
            "encoded" => assert!(answer.is_ok(), "{case}: {answer:?}"),
            "cancelled" => assert!(answer.is_err_and(|e| e.contains("cancelled")), "{case}"),
            "unencodable" => assert!(
                answer.is_err_and(|e| e.contains("image PNG encode failed")),
                "{case}"
            ),
            _ => assert!(
                answer.is_err_and(|e| e.contains("already initialized")),
                "{case}"
            ),
        }
        assert_eq!(
            at_answer, 0,
            "{case}: the capture's bytes were free when the answer arrived"
        );
    }
}

#[test]
fn no_png_deflate_runs_on_the_submitting_thread() {
    let mut app = App::headless_for_test();
    let here = std::thread::current().id();
    let frame = visual_capture_pixels(&mut app, None);
    assert!(frame.width > 0);
    let held = CapturePermit::try_reserve(&app.capture_bytes, CAPTURE_BYTE_BUDGET * 2)
        .or_else(|| CapturePermit::try_reserve(&app.capture_bytes, CAPTURE_BYTE_BUDGET));
    let (reply, rx) = std::sync::mpsc::channel();
    app.render_image(image_req_for_test(
        control_auth::ConfinedImage::in_memory(),
        true,
        true,
        None,
        reply,
    ));
    let _ = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    drop(held);
    let encoders = ENCODE_THREADS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(!encoders.is_empty(), "the capture was encoded");
    assert!(
        !encoders.contains(&here),
        "a PNG deflate ran on the submitting (event-loop) thread"
    );
}

#[test]
fn rejected_encode_jobs_publish_nothing() {
    let root = unique_dir("encode-queue-reject");
    ensure_private_dir(&root).unwrap();
    let target =
        control_auth::confine_automatic_image_path(&root, "rejected").expect("rejected target");
    let image_path = target.display_path();
    let (reply, rx) = std::sync::mpsc::channel();
    reject_encode_job(
        EncodeJob::Image {
            pixels: CapturePixels::packed(Frame {
                width: 2,
                height: 2,
                pixels: vec![0; 4],
            })
            .expect("a whole frame"),
            metadata: None,
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target,
            want_bytes: false,
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        },
        ENCODE_QUEUE_FULL,
    );
    let error = rx
        .recv_timeout(WORKER_PATIENCE)
        .expect("rejection reply")
        .expect_err("a saturated capture is rejected");
    assert!(error.contains(ENCODE_QUEUE_FULL), "{error}");
    assert!(!image_path.exists(), "rejection publishes no image");

    let path = root.join("snapshot.png");
    let done = snapshot_sidecar_path(&path, ".done");
    std::fs::write(&done, b"stale\n").unwrap();
    let transaction = begin_snapshot_generation(&path).expect("snapshot transaction");
    assert!(!done.exists(), "begin removes the stale marker");
    reject_encode_job(
        EncodeJob::Snapshot {
            pixels: CapturePixels::packed(Frame {
                width: 1,
                height: 1,
                pixels: vec![0],
            })
            .expect("a whole frame"),
            text: "unpublished".to_string(),
            transaction,
        },
        ENCODE_QUEUE_FULL,
    );
    assert!(!path.exists(), "rejection must not write snapshot pixels");
    assert!(!done.exists(), "rejection must not publish `.done`");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn capture_regression_snapshot_done_is_a_commit_marker() {
    let dir = unique_dir("snapshot-commit");
    ensure_private_dir(&dir).unwrap();
    let path = dir.join("snapshot.png");
    let text_path = snapshot_sidecar_path(&path, ".txt");
    let done_path = snapshot_sidecar_path(&path, ".done");
    let frame = Frame {
        width: 1,
        height: 1,
        pixels: vec![0x0011_2233],
    };

    std::fs::write(&done_path, b"stale\n").unwrap();
    std::fs::create_dir(&text_path).unwrap();
    let transaction = begin_snapshot_generation(&path).unwrap();
    let error = write_snapshot_artifacts(&encode_frame_png(&frame), "visible text", &transaction)
        .expect_err("a failed payload write must abort the snapshot");
    assert!(
        error.contains("text write failed"),
        "precise error: {error}"
    );
    assert!(
        !done_path.exists(),
        "the stale/partial attempt must own no completion marker"
    );
    assert!(
        !path.exists(),
        "a payload written before the failure is cleaned up"
    );

    std::fs::remove_dir(&text_path).unwrap();
    let transaction = begin_snapshot_generation(&path).unwrap();
    write_snapshot_artifacts(&encode_frame_png(&frame), "visible text", &transaction)
        .expect("a complete snapshot commits");
    assert!(path.is_file() && text_path.is_file() && done_path.is_file());
    assert_eq!(std::fs::read_to_string(&text_path).unwrap(), "visible text");
    let done = std::fs::read_to_string(&done_path).unwrap();
    assert!(done.starts_with("1x1\ngeneration="));
    assert!(done.contains("png_sha256=") && done.contains("text_sha256="));
    assert!(done.contains(&format!(
        "png_sha256={}",
        sha256_hex(&std::fs::read(&path).unwrap())
    )));
    assert!(done.contains(&format!(
        "text_sha256={}",
        sha256_hex(&std::fs::read(&text_path).unwrap())
    )));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn snapshot_begin_unlinks_stale_marker_before_directory_sync() {
    let dir = unique_dir("snapshot-clear-sync-order");
    ensure_private_dir(&dir).unwrap();
    let path = dir.join("snapshot.png");
    let done = sidecar_name(path.file_name().unwrap(), ".done");
    std::fs::write(dir.join(&done), b"stale").unwrap();
    let target = SnapshotTarget {
        path,
        dir: crate::pinned_dir::PinnedDir::open_resolved(&dir).unwrap(),
        png: std::ffi::OsString::from("snapshot.png"),
        text: std::ffi::OsString::from("snapshot.png.txt"),
        done,
    };
    let sync_observed = std::cell::Cell::new(false);

    clear_snapshot_completion_with_sync(&target, |pinned| {
        assert!(
            !target.dir.path().join(&target.done).exists(),
            "the stale marker must be absent before the durability barrier"
        );
        sync_observed.set(true);
        pinned.sync()
    })
    .unwrap();

    assert!(sync_observed.get(), "the containing directory was synced");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn image_reply_barrier_keeps_exact_guard_and_fails_closed_on_ancestor_swap() {
    use std::os::unix::fs::symlink;

    let root = unique_dir("image-reply-barrier");
    let images = root.join("images");
    let outside = unique_dir("image-reply-outside");
    ensure_private_dir(&images).unwrap();
    ensure_private_dir(&outside).unwrap();
    let target = crate::control_auth::ConfinedImage::for_test(&images, "shot.png");
    let file = target.write_private(b"png").unwrap();
    let moved = root.join("images-moved");
    let images_for_hook = images.clone();
    let moved_for_hook = moved.clone();
    let outside_for_hook = outside.clone();
    let (reply, result) = std::sync::mpsc::channel();

    send_capture_reply_after_validation_with_hook(
        PendingCaptureReply {
            target,
            file,
            lease: None,
            permit: crate::control::ReplyRetentionPermit::from_own_budget(),
        },
        &reply,
        (1_u32, 1_u32, None::<Vec<u8>>),
        "image",
        move || {
            std::fs::rename(&images_for_hook, &moved_for_hook).unwrap();
            symlink(&outside_for_hook, &images_for_hook).unwrap();
        },
    );
    assert!(result.recv().unwrap().is_err());
    assert!(
        std::fs::read_dir(&moved).unwrap().next().is_none(),
        "the exact failed artifact is removed through its retained handle"
    );
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());

    let _ = std::fs::remove_file(images);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn image_reply_guard_defers_prune_and_preserves_its_exact_path() {
    let sock = unique_dir("image-reply-before-prune");
    ensure_private_dir(&sock).unwrap();
    let target = control_auth::confine_automatic_image_path(&sock, "image").unwrap();
    let path = target.display_path().to_path_buf();
    let parent = path.parent().unwrap().to_path_buf();
    let lease = control_auth::acquire_capture_name_lease(&target, || false)
        .expect("automatic lease acquisition")
        .expect("lease queued automatic capture");
    let file = target.write_private(b"fresh-png").unwrap();
    for _ in 0..(control_auth::AUTO_IMAGE_KEEP + 5) {
        let newer = control_auth::automatic_capture_name("image");
        std::fs::write(parent.join(newer), b"newer").unwrap();
    }
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    send_capture_reply_after_validation(
        PendingCaptureReply {
            target,
            file,
            lease: Some(lease),
            permit: crate::control::ReplyRetentionPermit::from_own_budget(),
        },
        &reply_tx,
        (1_u32, 1_u32, None::<Vec<u8>>),
        "image",
    );
    let mut retained = reply_rx
        .recv_timeout(WORKER_PATIENCE)
        .expect("worker handed the exact guard to the control reply")
        .expect("capture reply remains valid");
    assert_eq!(
        retained.value,
        (1, 1, None),
        "the guard carries the original wire payload"
    );
    assert!(
        std::fs::read_dir(&parent).unwrap().count() > control_auth::AUTO_IMAGE_KEEP,
        "retention has not run while the reply guard is queued"
    );
    retained
        .retention
        .as_mut()
        .expect("file reply carries a guard")
        .prepare_write()
        .expect("wire-edge identity validates");
    drop(retained);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"fresh-png",
        "the post-wire sweep reserves one keep slot for this exact path"
    );

    let _ = std::fs::remove_dir_all(sock);
}

#[test]
fn explicit_capture_namespace_stays_busy_across_distinct_names_until_wire_release() {
    let root = unique_dir("image-namespace-wire-lease");
    ensure_private_dir(&root).unwrap();
    let first_target = crate::control_auth::ConfinedImage::for_test(&root, "shot.png");
    let first_path = first_target.display_path();
    let first_cancel = crate::control::CaptureCancellation::new();
    let first_lease = crate::control_auth::acquire_capture_name_lease(&first_target, || {
        first_cancel.is_cancelled()
    })
    .expect("first lease acquisition")
    .expect("first name lease");
    let first_file = first_target
        .write_private_authorized(b"first", || first_cancel.authorize_commit())
        .unwrap();
    let (first_tx, first_rx) = std::sync::mpsc::channel();
    send_capture_reply_after_validation(
        PendingCaptureReply {
            target: first_target,
            file: first_file,
            lease: Some(first_lease),
            permit: crate::control::ReplyRetentionPermit::from_own_budget(),
        },
        &first_tx,
        (),
        "image",
    );
    let mut first = first_rx.recv().unwrap().unwrap();

    let second_target = crate::control_auth::ConfinedImage::for_test(&root, "other.png");
    let second_path = second_target.display_path();
    let second_cancel = crate::control::CaptureCancellation::new();
    let busy = crate::control_auth::acquire_capture_name_lease_with_wait(
        &second_target,
        || second_cancel.is_cancelled(),
        std::time::Duration::ZERO,
    )
    .expect_err("a differently named explicit capture still contends on the namespace");
    assert_eq!(busy.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(
        std::fs::read(&first_path).unwrap(),
        b"first",
        "a busy successor cannot mutate the first guarded artifact"
    );
    assert!(
        !second_path.exists(),
        "the distinct successor cannot publish while the namespace is leased"
    );
    first
        .retention
        .as_mut()
        .unwrap()
        .prepare_write()
        .expect("first identity validates at its wire edge");
    assert_eq!(std::fs::read(&first_path).unwrap(), b"first");
    drop(first);

    let second_lease = crate::control_auth::acquire_capture_name_lease(&second_target, || false)
        .expect("retry after release")
        .expect("second name lease");
    let second_file = second_target
        .write_private_authorized(b"second", || true)
        .unwrap();
    let (second_tx, second_rx) = std::sync::mpsc::channel();
    send_capture_reply_after_validation(
        PendingCaptureReply {
            target: second_target,
            file: second_file,
            lease: Some(second_lease),
            permit: crate::control::ReplyRetentionPermit::from_own_budget(),
        },
        &second_tx,
        (),
        "image",
    );
    let mut second = second_rx.recv().unwrap().unwrap();
    second
        .retention
        .as_mut()
        .unwrap()
        .prepare_write()
        .expect("second identity validates at its wire edge");
    assert_eq!(std::fs::read(&second_path).unwrap(), b"second");
    assert_eq!(
        std::fs::read(&first_path).unwrap(),
        b"first",
        "a distinct successor never overwrites the first result"
    );
    drop(second);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn capture_timeout_cancellation_wins_before_final_name_publish() {
    let root = unique_dir("image-cancel-before-publish");
    ensure_private_dir(&root).unwrap();
    let target = crate::control_auth::ConfinedImage::for_test(&root, "shot.png");
    let path = target.display_path();
    let cancel = crate::control::CaptureCancellation::new();
    assert!(cancel.cancel());

    let error = target
        .write_private_authorized(b"never-visible", || cancel.authorize_commit())
        .expect_err("cancelled publication cannot acquire the final name");

    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(!path.exists());
    assert!(
        std::fs::read_dir(&root).unwrap().next().is_none(),
        "the fully written private temporary is cleaned when cancellation wins"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn video_publication_bundle_drops_guards_before_unpublished_directory_on_panic() {
    let sock = unique_dir("video-windows-unwind");
    ensure_private_dir(&sock).unwrap();
    let recording = control_auth::confine_video_dir(&sock).unwrap();
    let path = recording.path().to_path_buf();
    let frame = recording
        .write_sealed_frame(std::ffi::OsStr::new("frame-000000.png"), b"png")
        .unwrap();
    let index = recording
        .write_new_private(
            std::ffi::OsStr::new("index.json"),
            b"{\"frames\":[{\"file\":\"frame-000000.png\"}]}",
        )
        .unwrap();

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _publication = VideoPublication {
            frame_seals: vec![frame],
            index_file: index,
            published_marker: None,
            dir: recording,
        };
        panic!("injected before publication");
    }));
    assert!(unwind.is_err());
    assert!(
        !path.exists(),
        "the index deny-delete handle must release before directory cleanup"
    );

    let retained = control_auth::confine_video_dir(&sock).unwrap();
    let retained_path = retained.path().to_path_buf();
    let frame = retained
        .write_sealed_frame(std::ffi::OsStr::new("frame-000000.png"), b"png")
        .unwrap();
    let index = retained
        .write_new_private(
            std::ffi::OsStr::new("index.json"),
            b"{\"frames\":[{\"file\":\"frame-000000.png\"}]}",
        )
        .unwrap();
    let mut publication = VideoPublication {
        frame_seals: vec![frame],
        index_file: index,
        published_marker: None,
        dir: retained,
    };
    publication.prepare().unwrap();
    publication.publish_marker().unwrap();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _publication = publication;
        panic!("injected after publication");
    }));
    assert!(unwind.is_err());
    assert!(
        retained_path.join("index.json").is_file(),
        "publication is irrevocably retained before any observable reply"
    );

    let _ = std::fs::remove_dir_all(sock);
}

#[test]
fn video_reply_guard_owns_publication_until_wire_prepare() {
    let sock = unique_dir("video-reply-before-prune");
    ensure_private_dir(&sock).unwrap();
    let recording = control_auth::confine_video_dir(&sock).unwrap();
    let path = recording.path().to_path_buf();
    let frame = recording
        .write_sealed_frame(std::ffi::OsStr::new("frame-000000.png"), b"png")
        .unwrap();
    let index = recording
        .write_new_private(
            std::ffi::OsStr::new("index.json"),
            b"{\"frames\":[{\"file\":\"frame-000000.png\"}]}",
        )
        .unwrap();
    let mut publication = VideoPublication {
        frame_seals: vec![frame],
        index_file: index,
        published_marker: None,
        dir: recording,
    };
    publication.prepare().unwrap();
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    send_video_reply_with_retention(
        publication,
        crate::control::ReplyRetentionPermit::from_own_budget(),
        &reply_tx,
        "OK video\n".to_string(),
    );
    let mut retained = reply_rx
        .recv_timeout(WORKER_PATIENCE)
        .expect("worker handed publication to the control writer");
    assert_eq!(retained.value, "OK video\n");
    assert!(path.join("index.json").is_file());
    for _ in 0..11 {
        let mut later = control_auth::confine_video_dir(&sock).unwrap();
        let later_index = later
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        later.publish(&[], &later_index).unwrap();
        let later_marker = later.publish_marker().unwrap();
        drop(later_marker);
        drop(later_index);
        drop(later);
    }
    assert!(
        path.join("index.json").is_file(),
        "later recording sweeps cannot quarantine a queued video OK"
    );
    retained
        .retention
        .as_mut()
        .expect("video OK carries its publication retention")
        .prepare_write()
        .expect("wire-edge video validation succeeds");
    drop(retained);
    assert!(
        path.join("index.json").is_file(),
        "wire preparation makes publication irrevocable before OK bytes"
    );

    let _ = std::fs::remove_dir_all(sock);
}

#[cfg(unix)]
#[test]
fn failed_video_marker_validation_still_runs_bounded_prune() {
    let sock = unique_dir("video-post-publish-prune");
    ensure_private_dir(&sock).unwrap();
    let recording = control_auth::confine_video_dir(&sock).unwrap();
    let path = recording.path().to_path_buf();
    let root = path.parent().unwrap().to_path_buf();
    let oldest = root.join("rec-00000000000000000000-100");
    for sequence in 100..112 {
        let fixture = root.join(format!("rec-00000000000000000000-{sequence:03}"));
        std::fs::create_dir(&fixture).unwrap();
        std::fs::write(fixture.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(
            fixture.join(control_auth::VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();
    }

    let index = recording
        .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
        .unwrap();
    let mut publication = VideoPublication {
        frame_seals: Vec::new(),
        index_file: index,
        published_marker: None,
        dir: recording,
    };
    publication.prepare().unwrap();

    let marker = path.join(control_auth::VIDEO_PUBLISHED_FILE);
    let displaced_marker = path.join(".published-displaced");
    let marker_for_hook = marker.clone();
    let mut retention = VideoReplyRetention { publication };
    let error = retention
        .prepare_write_with(move |publication| {
            publication.publish_marker_with_test_hook(move || {
                std::fs::rename(&marker_for_hook, &displaced_marker).unwrap();
                std::fs::write(&marker_for_hook, b"replacement").unwrap();
            })
        })
        .expect_err("replacement after visibility must fail marker identity validation");
    assert!(error.contains("video publish marker failed"));
    assert!(
        retention.publication.is_published(),
        "the atomic visibility boundary, not the fallible return, owns publication state"
    );
    assert!(
        marker.is_file(),
        "the once-visible recording remains complete"
    );

    drop(retention);
    control_auth::wait_for_artifact_cleanup_for_test(&path);
    assert!(
        !oldest.exists(),
        "a post-publication failure still runs the bounded retention sweep"
    );
    assert!(
        path.join("index.json").is_file() && marker.is_file(),
        "the irrevocable fresh recording survives its own sweep"
    );

    let _ = std::fs::remove_dir_all(sock);
}

#[cfg(unix)]
#[test]
fn snapshot_transaction_rejects_ancestor_replacement_without_leaking_payload() {
    use std::os::unix::fs::symlink;

    let root = unique_dir("snapshot-ancestor-swap");
    let outside = unique_dir("snapshot-ancestor-outside");
    let snapshots = root.join("snapshots");
    ensure_private_dir(&snapshots).unwrap();
    ensure_private_dir(&outside).unwrap();
    let path = snapshots.join("snapshot.png");
    let transaction = begin_snapshot_generation(&path).unwrap();
    let frame = Frame {
        width: 1,
        height: 1,
        pixels: vec![0x0011_2233],
    };
    let moved = root.join("snapshots-moved");
    let snapshots_for_hook = snapshots.clone();
    let outside_for_hook = outside.clone();
    let moved_for_hook = moved.clone();

    let error = write_snapshot_artifacts_with_hooks(
        &encode_frame_png(&frame),
        "private text",
        &transaction,
        move || {
            std::fs::rename(&snapshots_for_hook, &moved_for_hook).unwrap();
            symlink(&outside_for_hook, &snapshots_for_hook).unwrap();
        },
        || panic!("a replaced ancestor shows no completion marker"),
    )
    .expect_err("a replaced ancestor cannot commit a completion marker");
    assert!(error.contains("identity changed"), "precise error: {error}");
    assert!(
        std::fs::read_dir(&outside).unwrap().next().is_none(),
        "the replacement directory receives no payload or marker"
    );
    assert!(
        std::fs::read_dir(&moved).unwrap().next().is_none(),
        "the retained original directory is cleaned handle-relatively"
    );

    let _ = std::fs::remove_file(&snapshots);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

/// `.done` is the writer's last call on its directory: a consumer that
/// removes the directory the moment the marker appears (as the route-chrome
/// tests drop their tempdir) leaves the writer nothing to fail on.
#[cfg(unix)]
#[test]
fn snapshot_done_is_the_writers_last_call_on_its_directory() {
    let dir = unique_dir("snapshot-done-last");
    ensure_private_dir(&dir).unwrap();
    let path = dir.join("snapshot.png");
    let done = snapshot_sidecar_path(&path, ".done");
    let transaction = begin_snapshot_generation(&path).unwrap();
    let frame = Frame {
        width: 1,
        height: 1,
        pixels: vec![0x0011_2233],
    };
    let consumer_dir = dir.clone();
    write_snapshot_artifacts_with_hooks(
        &encode_frame_png(&frame),
        "visible text",
        &transaction,
        || {},
        move || {
            assert!(done.is_file(), "the marker is visible");
            std::fs::remove_dir_all(&consumer_dir).unwrap();
        },
    )
    .expect("a consumer that removes the directory at the marker fails nothing after it");
    assert!(!dir.exists());
}

#[test]
fn snapshot_generation_fence_rejects_old_worker_after_new_begin() {
    let model = aterm_models_misc::snapshot_generation_commit_model();
    let mut state = model.init_state();
    assert_eq!(snapshot_commit_plan(1, 1), SnapshotCommitPlan::Publish);
    assert!(model.fire("CommitCurrent", &mut state));
    assert!(model.fire("BeginNew", &mut state));
    assert_eq!(snapshot_commit_plan(1, 2), SnapshotCommitPlan::DiscardStale);
    assert!(model.fire("CommitOld", &mut state));
    assert!(model.check_invariant("CommittedPayloadIsCurrent", &state));

    let dir = unique_dir("snapshot-generation");
    ensure_private_dir(&dir).unwrap();
    let path = dir.join("snapshot.png");
    let done_path = snapshot_sidecar_path(&path, ".done");
    let frame = Frame {
        width: 1,
        height: 1,
        pixels: vec![0x0011_2233],
    };
    let transaction_a = begin_snapshot_generation(&path).unwrap();
    let transaction_b = begin_snapshot_generation(&path).unwrap();
    assert!(
        !done_path.exists(),
        "B begin invalidates every prior marker"
    );
    let stale = write_snapshot_artifacts(&encode_frame_png(&frame), "A", &transaction_a)
        .expect_err("A cannot commit after B begins");
    assert!(stale.contains("superseded"));
    assert!(
        !done_path.exists(),
        "once B begin returns, A can never republish the fixed marker"
    );
    write_snapshot_artifacts(&encode_frame_png(&frame), "B", &transaction_b)
        .expect("the current generation commits");
    assert!(done_path.is_file());

    assert!(model.fire("SelectCurrent", &mut state));
    assert_eq!(snapshot_commit_plan(2, 2), SnapshotCommitPlan::Publish);
    assert!(model.fire("CommitCurrent", &mut state));
    let mut bad = state.clone();
    bad.insert("payload", 1);
    assert!(
        !model.check_invariant("CommittedPayloadIsCurrent", &bad),
        "negative control: a current marker certifying stale payload is rejected"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn snapshot_generation_transaction_cannot_certify_stale_payload() {
    let dir = unique_dir("snapshot-payload-identity");
    ensure_private_dir(&dir).unwrap();
    let path = dir.join("snapshot.png");
    let text_path = snapshot_sidecar_path(&path, ".txt");
    let done_path = snapshot_sidecar_path(&path, ".done");
    let frame_a = Frame {
        width: 1,
        height: 1,
        pixels: vec![0x00AA_1020],
    };
    let frame_b = Frame {
        width: 1,
        height: 1,
        pixels: vec![0x0010_BB30],
    };
    let transaction_a = begin_snapshot_generation(&path).unwrap();
    let transaction_b = begin_snapshot_generation(&path).unwrap();

    let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    write_snapshot_artifacts_with_hooks(
        &encode_frame_png(&frame_b),
        "B payload",
        &transaction_b,
        move || {
            std::thread::spawn(move || {
                attempted_tx.send(()).unwrap();
                let result = write_snapshot_artifacts(
                    &encode_frame_png(&frame_a),
                    "A payload",
                    &transaction_a,
                );
                result_tx.send(result).unwrap();
            });
            attempted_rx.recv().unwrap();
        },
        || {},
    )
    .expect("B transaction commits");
    let stale = result_rx
        .recv_timeout(WORKER_PATIENCE)
        .expect("stale A finishes after B unlocks")
        .expect_err("A remains stale");
    assert!(stale.contains("superseded"));

    let png = std::fs::read(&path).unwrap();
    let (rgba, width, height) = aterm_render::decode_png_rgba8(&png).unwrap();
    assert_eq!((width, height), (1, 1));
    assert_eq!(rgba, frame_b.rgba_bytes(), "B marker certifies B pixels");
    assert_eq!(std::fs::read_to_string(text_path).unwrap(), "B payload");
    let done = std::fs::read_to_string(done_path).unwrap();
    assert!(done.starts_with("1x1\ngeneration="));
    assert!(done.contains("png_sha256=") && done.contains("text_sha256="));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capture_regression_platform_neutral_rgba_encoder_is_available() {
    let png = encode_rgba8_png(&[0x11, 0x22, 0x33, 0x80], 1, 1)
        .expect("the video/window encoder is available on every host");
    let mut reader = aterm_png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .expect("PNG header");
    assert_eq!(
        reader.info().srgb,
        Some(aterm_png::SrgbRenderingIntent::Perceptual)
    );
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).expect("PNG pixels");
    assert_eq!(&rgba[..info.buffer_size()], &[0x11, 0x22, 0x33, 0x80]);
}

fn pixel_take(frames: u64) -> aterm_gpu::video_tap::VideoTake {
    aterm_gpu::video_tap::VideoTake {
        frames: (1..=frames)
            .map(|seq| aterm_gpu::video_tap::CapturedFrame {
                seq,
                t_us: seq,
                w: 1,
                h: 1,
                rgba: vec![
                    u8::try_from(seq).expect("fixture sequence fits u8"),
                    0,
                    0,
                    255,
                ],
            })
            .collect(),
        dropped: 0,
        evicted: 0,
        decimated: 0,
        fps_cap: None,
        budget_bytes: 16 << 20,
        requested_ms: 100,
        w: 1,
        h: 1,
        device_px: (1, 1),
        half_res: false,
        format: "rgba8",
        resized_early_stop: false,
    }
}

fn run_video_dump(
    dir: control_auth::ConfinedVideoDir,
    take: aterm_gpu::video_tap::VideoTake,
    cancelled: bool,
    started_us: u64,
) -> (
    std::sync::mpsc::Receiver<crate::control::Retained<String>>,
    std::sync::Arc<crate::VideoExportState>,
) {
    let (reply, rx) = std::sync::mpsc::channel();
    let cancel = crate::VideoCancellation::new();
    let export = std::sync::Arc::new(crate::VideoExportState::default());
    let permit = export
        .try_begin(cancel.clone())
        .expect("fresh export permit");
    if cancelled {
        let _ = cancel.cancel();
    }
    run_encode_job(
        EncodeJob::VideoDump {
            take: take.into(),
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            mode: crate::VideoMode::SwapchainTap,
            keys_enabled: false,
            inputs: Vec::new(),
            pace_ticks: Vec::new(),
            unlogged_inputs: 0,
            unlogged_other_window: 0,
            unlogged_over_cap: 0,
            started_us,
            dir: Box::new(dir),
            reply,
            cancel,
            permit,
        },
        None,
    );
    (rx, export)
}

#[test]
fn capture_regression_video_write_failure_has_no_completion_artifact() {
    let root = unique_dir("video-export-failure");
    ensure_private_dir(&root).unwrap();
    let dir = crate::control_auth::confine_video_dir(&root).expect("server-minted video dir");
    let dir_path = dir.path().to_path_buf();
    std::fs::create_dir(dir_path.join("frame_0001.png")).unwrap();
    let (rx, _export) = run_video_dump(dir, pixel_take(1), false, 0);
    let error = rx.recv().expect("failure reply");
    assert!(error.starts_with("ERR video:"), "protocol error: {error}");
    assert!(
        !dir_path.join("index.json").exists(),
        "a failed export never publishes its completion artifact"
    );
    assert!(
        !dir_path.exists(),
        "the server-owned partial export is removed rather than leaked forever"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn video_export_frame_seals_keep_file_descriptors_bounded() {
    if !crate::control_auth::enter_low_nofile_test_child(16) {
        return;
    }

    let root = crate::test_scratch::short_dir("the video root");
    ensure_private_dir(root.path()).unwrap();
    let dir = crate::control_auth::confine_video_dir(root.path()).expect("server-minted video dir");
    let dir_path = dir.path().to_path_buf();
    let (rx, _export) = run_video_dump(dir, pixel_take(32), false, 0);
    let mut retained = rx.recv().expect("video reply");
    assert!(
        retained.value.starts_with("OK frames=32 "),
        "bounded export completes: {}",
        retained.value
    );
    retained
        .retention
        .as_mut()
        .expect("successful video reply owns publication retention")
        .prepare_write()
        .expect("all sealed frames revalidate at the wire edge");
    assert!(dir_path.join("index.json").is_file());
    assert!(dir_path.join("frame_0032.png").is_file());
    drop(retained);
}

#[test]
fn cancelled_video_export_aborts_dir_and_releases_busy_permit() {
    let root = unique_dir("video-export-cancel");
    ensure_private_dir(&root).unwrap();
    let dir = crate::control_auth::confine_video_dir(&root).expect("server-minted video dir");
    let dir_path = dir.path().to_path_buf();
    let (rx, export) = run_video_dump(dir, pixel_take(1), true, 0);
    assert!(
        rx.recv()
            .expect("cancel reply")
            .contains("request cancelled before export")
    );
    assert!(
        !dir_path.exists(),
        "cancellation removes the unpublished partial directory"
    );
    assert!(
        !export.is_busy(),
        "worker acknowledgement drops the sole export permit"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn terminal_image_metadata_binds_exact_snapshot_and_encoded_pixels() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let terminal = app.front_terminal_mirror(wid).expect("terminal front");
    crate::term_lock(&terminal.term).process(b"exact terminal metadata");
    let (view, session) = match app.windows[&wid].front_content.unwrap() {
        crate::front_content::FrontContent::Terminal { view, session } => (view, session),
        crate::front_content::FrontContent::Native { .. } => panic!("terminal front expected"),
    };

    let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    let (tx, rx) = std::sync::mpsc::channel();
    crate::reset_visible_leaf_plan_builds();
    app.render_image(image_req_for_test(
        control_auth::ConfinedImage::in_memory(),
        false,
        true,
        Some(&frame_metadata),
        tx,
    ));
    assert_eq!(
        crate::visible_leaf_plan_builds(),
        1,
        "full terminal image routing, preparation, hover and metadata share one visible plan",
    );
    let (width, height, png) = rx
        .recv_timeout(WORKER_PATIENCE)
        .expect("terminal image reply")
        .expect("terminal image succeeds")
        .value;
    let png = png.expect("wire capture includes PNG bytes");
    let metadata = frame_metadata
        .get()
        .expect("terminal frame metadata is bound");
    assert_eq!(metadata.frame_kind, "terminal");
    assert_eq!(
        metadata.phase, "staged",
        "a headless frame is staged in its turn"
    );
    assert_eq!(metadata.view, Some(view.get()));
    assert_eq!((metadata.width, metadata.height), (width, height));
    assert_eq!(metadata.compiled_fingerprint, None);
    assert_eq!(metadata.leaves.len(), 1);
    let leaf = &metadata.leaves[0];
    assert_eq!(leaf.kind, "terminal");
    assert_eq!(leaf.view, view.get());
    assert_eq!(leaf.session, Some(session));
    assert!(leaf.focused);

    let retained = &app.windows[&wid].input_scratch;
    assert_eq!(leaf.snapshot_seq, Some(retained.snapshot_seq));
    assert_eq!(
        metadata.raster_model_fingerprint,
        App::terminal_snapshot_fingerprint(view.get(), session, retained),
    );
    assert_eq!(
        metadata.raster_geometry,
        App::terminal_geometry_fingerprint(width as usize, height as usize, retained),
    );

    let (rgba, decoded_width, decoded_height) =
        aterm_render::decode_png_rgba8(&png).expect("encoded terminal PNG decodes");
    assert_eq!(
        (decoded_width as u32, decoded_height as u32),
        (width, height)
    );
    let (rgba_pixels, remainder) = rgba.as_chunks::<4>();
    assert!(remainder.is_empty(), "decoded RGBA8 has complete pixels");
    let pixels = rgba_pixels
        .iter()
        .map(|pixel| (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2]))
        .collect::<Vec<_>>();
    let encoded_pixel_fingerprint =
        App::image_pixel_fingerprint(decoded_width, decoded_height, &pixels);
    assert_eq!(metadata.pixel_fingerprint, encoded_pixel_fingerprint);
    assert_eq!(metadata.raster_fingerprint, encoded_pixel_fingerprint);
    assert_eq!(leaf.raster_fingerprint, Some(encoded_pixel_fingerprint));

    let wire = metadata.wire_fields();
    assert!(wire.contains("frame-kind=terminal"));
    assert!(wire.contains(&format!("view={}", view.get())));
    assert!(wire.contains(&format!("v{}:terminal:s{}:f1:", view.get(), session)));
    assert!(wire.contains(&format!("seq{}", retained.snapshot_seq)));
}

#[test]
fn native_image_metadata_retains_presented_provenance_during_invalidation() {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    // Focus on post-present invalidation after startup font synchronization.
    app.first_present_done = true;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (_, view) = app.active_native_view(wid).unwrap();
    #[cfg(a11y_tree)]
    {
        app.windows.get_mut(&wid).unwrap().a11y_active = true;
    }
    app.redraw_window(wid);
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let ((width, height), _) = crate::scripted_redraw_fixture::glass(&mut app);
    let facts = || ImageFrameFacts {
        phase: "presented",
        capture_serial: None,
        width: width.try_into().unwrap(),
        height: height.try_into().unwrap(),
    };
    let before = app
        .native_image_metadata_from_plan(wid, &plan, facts())
        .unwrap();
    let raster = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap();
    let (stamp, pixels, paint_serial) = (raster.stamp, raster.rgba.as_ptr(), raster.paint_serial);
    assert!(raster.presented);
    assert!(app.retained_native_leaf_artifact(wid, view, true).is_some());
    #[cfg(a11y_tree)]
    {
        // The present published its tree; a later preparation stages the next one.
        assert!(app.windows[&wid].native_a11y_published.is_some());
        assert!(app.prepare_native_input_scratch(wid));
        assert!(app.windows[&wid].native_a11y_staged.is_some());
    }

    // A successful present may invalidate its next compile after a load
    // policy transition, before capture reads that present's metadata.
    // Keep the model stamp unchanged here as a negative control for input:
    // stamp equality alone cannot establish that dirty paint is current.
    app.invalidate_native_ui_cache(wid);
    let retained = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap();
    assert_eq!(retained.stamp, stamp);
    assert_eq!(retained.rgba.as_ptr(), pixels);
    assert_eq!(retained.paint_serial, paint_serial);
    assert!(app.cached_native_ui(wid).is_none());
    assert!(app.retained_native_leaf_artifact(wid, view, true).is_none());
    assert!(
        app.retained_native_leaf_artifact(wid, view, false)
            .is_none()
    );
    #[cfg(a11y_tree)]
    {
        assert!(app.windows[&wid].native_a11y_staged.is_none());
        assert!(matches!(
            app.take_native_accessibility_update_from_plan(wid, &plan),
            Some(Ok(_))
        ));
    }
    let after = app
        .native_image_metadata_from_plan(wid, &plan, facts())
        .unwrap();
    assert_eq!(after.capture_serial, before.capture_serial);
    assert_eq!(after.compiled_fingerprint, before.compiled_fingerprint);
    assert_eq!(after.raster_fingerprint, before.raster_fingerprint);
    assert_eq!(
        after.leaves[0].raster_fingerprint,
        before.leaves[0].raster_fingerprint
    );
    assert_eq!(after.paint_revision, before.paint_revision);

    let compiles = crate::app_native::native_ui_compile_count();
    app.redraw_window(wid);
    assert!(crate::app_native::native_ui_compile_count() > compiles);
    let cache = &app.windows[&wid].leaf_render_cache[&view];
    assert!(cache.native_damage.is_none());
    let raster = cache.native.as_ref().unwrap();
    assert_eq!(raster.paint_serial, cache.native_paint_serial);
    assert_ne!(raster.paint_serial, paint_serial);
    assert!(app.retained_native_leaf_artifact(wid, view, true).is_some());
}

fn image_metadata_now(app: &mut App, dir: &std::path::Path) -> crate::control::ImageFrameMetadata {
    let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    let (tx, rx) = std::sync::mpsc::channel();
    app.render_image(crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: confined(dir, "meta.png"),
        clean: false,
        session: None,
        want_bytes: true,
        want_metadata: true,
        frame_metadata: std::sync::Arc::clone(&metadata),
        cancel: crate::control::CaptureCancellation::new(),
        reply: tx,
    });
    rx.recv_timeout(WORKER_PATIENCE)
        .expect("image reply")
        .expect("image succeeds");
    metadata.get().cloned().expect("frame metadata is bound")
}

#[test]
fn native_raster_fingerprints_are_hashed_once_per_raster_pass() {
    let dir = unique_dir("native-raster-fingerprint-once");
    ensure_private_dir(&dir).unwrap();
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("Settings view");

    let first = image_metadata_now(&mut app, &dir);
    let leaf_fingerprint = first.leaves[0].raster_fingerprint.expect("native leaf");
    let window = &app.windows[&wid];
    let raster = window.leaf_render_cache[&view].native.as_ref().unwrap();
    assert_eq!(
        raster.raster_fingerprint_once.get(),
        Some(&leaf_fingerprint),
        "the leaf raster keeps its hash"
    );
    assert_eq!(
        leaf_fingerprint,
        hash_raster(raster.width, raster.height, &raster.rgba)
    );
    let card = window
        .route_card
        .as_ref()
        .or(window.settings_card.as_ref())
        .expect("the native card");
    assert_eq!(
        card.raster_fingerprint_once.get(),
        Some(&first.raster_fingerprint),
        "the card keeps its hash"
    );
    assert_eq!(
        first.raster_fingerprint,
        hash_raster(card.pw, card.ph, &card.rgba)
    );

    let second = image_metadata_now(&mut app, &dir);
    assert_eq!(second.leaves[0].raster_fingerprint, Some(leaf_fingerprint));
    assert_eq!(second.raster_fingerprint, first.raster_fingerprint);

    app.invalidate_native_view_cache(
        wid,
        view,
        crate::native_app::DamageRegion::Rect {
            x: 7,
            y: 11,
            width: 19,
            height: 23,
        },
    );
    assert!(app.prepare_native_input_scratch(wid));
    let raster = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap();
    assert!(
        matches!(raster.last_work, crate::NativeRasterWork::Region { .. }),
        "the damage took a regional pass"
    );
    assert_eq!(
        raster.raster_fingerprint_once.get(),
        None,
        "the pass took the stored hash"
    );
    let third = image_metadata_now(&mut app, &dir);
    let raster = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap();
    assert_eq!(
        third.leaves[0].raster_fingerprint,
        Some(hash_raster(raster.width, raster.height, &raster.rgba)),
        "the next capture hashes the patched pixels"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn native_image_metadata_binds_dimensions_to_the_same_audit_frame() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    app.config.show_build_badge = Some(true);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
    let (_, view) = app.active_native_view(wid).expect("Settings view");
    let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    let (tx, rx) = std::sync::mpsc::channel();
    app.render_image(crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: control_auth::ConfinedImage::in_memory(),
        clean: false,
        session: None,
        want_bytes: true,
        want_metadata: true,
        frame_metadata: std::sync::Arc::clone(&metadata),
        cancel: crate::control::CaptureCancellation::new(),
        reply: tx,
    });
    let (width, height, png) = rx
        .recv_timeout(WORKER_PATIENCE)
        .expect("native image reply")
        .expect("native image succeeds")
        .value;
    assert!(width > 0 && height > 0);
    assert!(png.is_some());

    let metadata = metadata.get().expect("native frame metadata is bound");
    let route_card = app.windows[&wid]
        .route_card
        .as_ref()
        .expect("enabled build badge creates a composed route card");
    let native_card = app.windows[&wid]
        .settings_card
        .as_ref()
        .expect("native card remains retained");
    assert_ne!(
        route_card.fp, native_card.fp,
        "negative control: route and base native cards have distinct identities"
    );
    assert_eq!(
        metadata.raster_model_fingerprint, route_card.fp,
        "metadata names the route card actually passed to the renderer"
    );
    assert_eq!(
        metadata.raster_geometry, route_card.geom,
        "metadata geometry comes from the rendered route card"
    );
    assert_eq!(
        metadata.phase, "staged",
        "headless capture has no OS application-present target"
    );
    assert_eq!(metadata.frame_kind, "native");
    assert_eq!(metadata.view, Some(view.get()));
    assert_eq!((metadata.width, metadata.height), (width, height));
    assert_ne!(metadata.pixel_fingerprint, 0);
    let generation = metadata.generation.unwrap();
    let config_revision = metadata.config_revision.unwrap();
    let update_revision = metadata.update_revision.unwrap();
    let presentation_revision = metadata.presentation_revision.unwrap();
    let paint_revision = metadata.paint_revision.unwrap();
    let geometry = metadata.leaves[0].geometry.unwrap();
    let compiled_fingerprint = metadata.compiled_fingerprint.unwrap();
    let audit = app
        .inspect_app(crate::app_control::InspectRequest::View {
            view,
            projection: aterm_types::app_inspection::InspectionProjection::Audit,
        })
        .unwrap();
    let source = &audit[1];
    for field in [
        format!("source={}", metadata.phase),
        format!("view={}", metadata.view.unwrap()),
        format!("generation={generation}"),
        format!("geometry={geometry:016x}"),
        format!("config-revision={config_revision}"),
        format!("update-revision={update_revision}"),
        format!("presentation-revision={presentation_revision}"),
        format!("paint-revision={paint_revision:016x}"),
        "model-current=true".to_string(),
        format!("capture-serial={}", metadata.capture_serial),
        format!("compiled-fingerprint={compiled_fingerprint:016x}"),
    ] {
        assert!(
            source.contains(&field),
            "Audit/source missing {field}: {source}"
        );
    }
    assert!(audit.iter().any(|line| {
        line.starts_with("paint-audit ")
            && line.contains(&format!("compiled-fingerprint={compiled_fingerprint:016x}"))
    }));
}

#[test]
fn native_image_metadata_changes_for_paint_only_pixel_identity() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));

    let capture = |app: &mut App| {
        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (tx, rx) = std::sync::mpsc::channel();
        crate::reset_visible_leaf_plan_builds();
        app.render_image(image_req_for_test(
            control_auth::ConfinedImage::in_memory(),
            false,
            true,
            Some(&metadata),
            tx,
        ));
        assert_eq!(
            crate::visible_leaf_plan_builds(),
            1,
            "full native image routing, preparation, hover and metadata share one visible plan",
        );
        let image = rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("paint identity image reply")
            .expect("paint identity image succeeds")
            .value;
        assert!(image.2.is_some());
        metadata.get().cloned().unwrap()
    };

    let before = capture(&mut app);
    app.theme.bg ^= 0x0008_1018;
    app.theme.fg ^= 0x0010_0804;
    app.installed_colors
        .tint_theme_for_test(0x0008_1018, 0x0010_0804);
    let after = capture(&mut app);
    assert_eq!(before.frame_kind, "native");
    assert_eq!(before.view, after.view);
    assert_eq!(before.generation, after.generation);
    assert_eq!(before.config_revision, after.config_revision);
    assert_eq!(
        before.compiled_fingerprint, after.compiled_fingerprint,
        "theme-only paint does not invent a different semantic tree"
    );
    assert_ne!(before.paint_revision, after.paint_revision);
    assert_ne!(before.theme_fingerprint, after.theme_fingerprint);
    assert_ne!(
        before.raster_fingerprint, after.raster_fingerprint,
        "retained native raster bytes identify paint-only changes"
    );
    assert_ne!(
        before.pixel_fingerprint, after.pixel_fingerprint,
        "the metadata binds the exact final RGBA frame"
    );
}

#[test]
fn snapshot_text_tracks_terminal_native_and_mixed_visible_routes() {
    let wid = WindowId(0);

    let mut terminal_app = App::headless_for_test();
    terminal_app.tab_strip_rows = 0;
    let terminal = terminal_app.front_terminal(wid).unwrap().term.clone();
    crate::term_lock(&terminal).process(b"TERMINAL_SNAPSHOT_MARKER");
    terminal_app
        .prepare_terminal_capture_grid(wid)
        .expect("terminal route stages");
    let terminal_route = terminal_app.active_visible_content_route(wid).unwrap();
    let terminal_input = terminal_app.windows[&wid].input_scratch.clone();
    let terminal_text = terminal_app.snapshot_visible_text(
        wid,
        terminal_route,
        &terminal_app.active_visible_leaf_plan(wid).unwrap(),
        &terminal_input,
        0,
        usize::from(terminal_app.windows[&wid].cols),
    );
    assert_eq!(
        terminal_text,
        terminal_capture_text(
            &terminal_input,
            0,
            usize::from(terminal_app.windows[&wid].cols)
        ),
        "terminal-only SIGUSR1 text remains byte-for-byte compatible"
    );
    assert!(terminal_text.contains("TERMINAL_SNAPSHOT_MARKER"));

    let mut native_app = App::headless_for_test();
    assert!(native_app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(native_app.prepare_native_input_scratch(wid));
    let (_, native_view) = native_app.active_native_view(wid).unwrap();
    let native_route = native_app.active_visible_content_route(wid).unwrap();
    let native_input = native_app.windows[&wid].input_scratch.clone();
    let native_text = native_app.snapshot_visible_text(
        wid,
        native_route,
        &native_app.active_visible_leaf_plan(wid).unwrap(),
        &native_input,
        usize::from(native_app.tab_strip_rows),
        usize::from(native_app.windows[&wid].cols),
    );
    assert!(native_text.contains(&format!("[native view={} focused=true]", native_view.get())));
    assert!(
        native_text.contains("text key="),
        "native SIGUSR1 text projects the retained semantic tree: {native_text}"
    );

    let (session, terminal_view) =
        native_app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let mixed_terminal = native_app.pool.get(session).unwrap().term.clone();
    crate::term_lock(&mixed_terminal).process(b"MIXED_SNAPSHOT_MARKER");
    assert!(
        native_app
            .prepare_heterogeneous_input_scratch(wid)
            .is_some()
    );
    let mixed_route = native_app.active_visible_content_route(wid).unwrap();
    let mixed_input = native_app.windows[&wid].input_scratch.clone();
    let mixed_text = native_app.snapshot_visible_text(
        wid,
        mixed_route,
        &native_app.active_visible_leaf_plan(wid).unwrap(),
        &mixed_input,
        usize::from(native_app.tab_strip_rows),
        usize::from(native_app.windows[&wid].cols),
    );
    assert!(mixed_text.contains("MIXED_SNAPSHOT_MARKER"));
    assert!(mixed_text.contains(&format!("[native view={}", native_view.get())));
    assert!(mixed_text.contains("text key="));
    assert_ne!(
        native_app.windows[&wid].tab_set.active().unwrap().focus,
        native_view,
        "negative control: native semantics are retained even while terminal view {} owns focus",
        terminal_view.get()
    );

    let base_card_fp = native_app.windows[&wid]
        .settings_card
        .as_ref()
        .expect("mixed native layer is rasterized")
        .fp;
    native_app.palette_enter();
    let palette_lines = native_app.windows[&wid]
        .palette()
        .expect("palette is visibly open")
        .controls_lines();
    assert!(
        native_app
            .prepare_heterogeneous_input_scratch(wid)
            .is_some()
    );
    let palette_card_fp = native_app.windows[&wid]
        .settings_card
        .as_ref()
        .expect("visible palette is lowered into the mixed PNG route card")
        .fp;
    assert_ne!(
        base_card_fp, palette_card_fp,
        "negative control: opening the palette changes the raster captured by the PNG route"
    );
    let palette_route = native_app.active_visible_content_route(wid).unwrap();
    let palette_input = native_app.windows[&wid].input_scratch.clone();
    let palette_text = native_app.snapshot_visible_text(
        wid,
        palette_route,
        &native_app.active_visible_leaf_plan(wid).unwrap(),
        &palette_input,
        usize::from(native_app.tab_strip_rows),
        usize::from(native_app.windows[&wid].cols),
    );
    assert!(palette_text.contains("MIXED_SNAPSHOT_MARKER"));
    assert!(palette_text.contains(&format!("[native view={}", native_view.get())));
    let expected_palette_suffix = format!("{}\n", palette_lines.join("\n"));
    assert!(
        palette_text.ends_with(&expected_palette_suffix),
        "SIGUSR1 text must append the exact serializer for the palette painted in the PNG: {palette_text}"
    );
}

#[test]
fn mixed_composite_paints_every_terminal_selection_and_anchors_the_focused_one() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let retained_at = Instant::now();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native_view) = app.active_native_view(wid).expect("Settings view");
    let (terminal_session, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let terminal = app
        .pool
        .get(terminal_session)
        .expect("terminal session")
        .term
        .clone();
    {
        let mut terminal = crate::term_lock(&terminal);
        terminal.process(b"\x1b]17;rgb:21/43/65\x07\x1b]19;rgb:fe/dc/ba\x07");
        let selection = terminal.text_selection_mut();
        selection.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(2, 2, SelectionSide::Right);
        selection.complete_selection();
    }
    let plan = app.active_visible_leaf_plan(wid).expect("mixed plan");
    let terminal_leaf = plan
        .leaves
        .iter()
        .find(|leaf| leaf.view == terminal_view)
        .expect("terminal leaf");
    let native_leaf = plan
        .leaves
        .iter()
        .find(|leaf| leaf.view == native_view)
        .expect("native leaf");
    let terminal_rect = (
        terminal_leaf.rect.origin.y.round().max(0.0) as usize,
        terminal_leaf.rect.origin.x.round().max(0.0) as usize,
        terminal_leaf.rect.size.height.round().max(1.0) as usize,
        terminal_leaf.rect.size.width.round().max(1.0) as usize,
    );
    let native_point = (
        native_leaf.rect.origin.y.round().max(0.0) as usize,
        native_leaf.rect.origin.x.round().max(0.0) as usize,
    );
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.fx.cursor_mut().glow_scratch = vec![aterm_render::GlowQuad {
            row: u16::try_from(terminal_rect.0).unwrap(),
            x: 0,
            y: 0,
            w: u16::MAX,
            h: u16::MAX,
            color: 0x0042_84C6,
            alpha: 0,
            color2: 0x0042_84C6,
            alpha2: 0,
        }];
        window.fx.cursor_mut().trail_scratch = vec![
            aterm_render::TrailCell {
                row: terminal_rect.0,
                col: terminal_rect.1,
                alpha: 210,
            },
            aterm_render::TrailCell {
                row: native_point.0,
                col: native_point.1,
                alpha: 1,
            },
        ];
        window.composed_cursor_effect_valid = true;
        window.composed_cursor_effect_session = Some(terminal_session);
        window.composed_cursor_fill = Some(0x0012_3456);
        window.composed_cursor_trail_color = 0x0065_4321;
    }

    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(crate::app_render::ComposedCursorFxClock::Retain {
                observed_at: retained_at,
            }),
        )
        .is_some()
    );
    let input = &app.windows[&wid].input_scratch;
    let (row, col, pane_rows, pane_cols) = terminal_rect;
    assert_eq!(
        input.selection_clip,
        Some(aterm_render::SelectionClip::new(
            row,
            row + pane_rows,
            col,
            col + pane_cols,
        ))
    );
    assert_eq!(input.selection_bg, 0x0021_4365);
    assert_eq!(input.selection_fg, 0x00fe_dcba);
    assert!(input.selection_contains_cell(row + 1, col + pane_cols - 1, false, false));
    assert_eq!(input.cursor_glow_add.len(), 1);
    assert_eq!(
        input.cursor_trail,
        vec![aterm_render::TrailCell {
            row: terminal_rect.0,
            col: terminal_rect.1,
            alpha: 210,
        }],
        "a mixed frame projects effects only into its focused terminal leaf"
    );
    assert_eq!(input.cursor_fill_override, Some(0x0012_3456));

    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(native_view);
    app.sync_window(wid);
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(crate::app_render::ComposedCursorFxClock::Retain {
                observed_at: retained_at,
            }),
        )
        .is_some()
    );
    let input = &app.windows[&wid].input_scratch;
    assert!(!input.selection.has_selection());
    assert_eq!(input.selection_clip, None);
    assert_eq!(input.selections[0].bg, 0x0021_4365);
    assert!(
        input.selection_contains_cell(row + 1, col + pane_cols - 1, false, false),
        "the unfocused terminal pane keeps painting its selection"
    );
    assert!(
        input.cursor_glow_add.is_empty()
            && input.cursor_trail.is_empty()
            && input.cursor_fill_override.is_none(),
        "native focus clears the terminal-only cursor-effect authority"
    );
    assert!(
        !app.windows[&wid].composed_cursor_effect_valid,
        "a later terminal focus cannot resurrect native-focused stale effects"
    );
}

#[test]
fn headless_native_capture_composes_every_mixed_leaf_for_either_focus() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native_view) = app.active_native_view(wid).expect("Settings view");
    let (_, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert_eq!(
        app.windows[&wid].tab_set.active().unwrap().focus,
        terminal_view
    );

    let capture = |app: &mut App| {
        let (tx, rx) = std::sync::mpsc::channel();
        let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        crate::reset_visible_leaf_plan_builds();
        app.render_image(image_req_for_test(
            control_auth::ConfinedImage::in_memory(),
            false,
            true,
            Some(&frame_metadata),
            tx,
        ));
        assert_eq!(
            crate::visible_leaf_plan_builds(),
            1,
            "full mixed image routing, preparation, hover and metadata share one visible plan",
        );
        let image = rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("mixed capture reply")
            .expect("mixed capture succeeds")
            .value;
        (image, frame_metadata.get().cloned().unwrap())
    };
    let (terminal_focused, terminal_metadata) = capture(&mut app);
    assert!(terminal_focused.0 > 0 && terminal_focused.1 > 0);
    assert!(terminal_focused.2.is_some());
    assert_eq!(terminal_metadata.frame_kind, "composite");
    assert_eq!(terminal_metadata.view, None);
    assert_eq!(terminal_metadata.leaves.len(), 2);
    let terminal_wire = terminal_metadata.wire_fields();
    assert!(terminal_wire.contains("frame-kind=composite"));
    assert!(terminal_wire.contains("view=-"));
    assert!(terminal_wire.contains("compiled-fingerprint=-"));
    assert!(terminal_wire.contains("leaf-count=2"));
    assert!(terminal_metadata.leaves.iter().any(|leaf| {
        leaf.kind == "terminal" && leaf.focused && leaf.view == terminal_view.get()
    }));
    assert!(
        terminal_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "native" && !leaf.focused && leaf.view == native_view.get()
        })
    );
    assert!(app.windows[&wid].settings_card.is_some());
    assert!(
        app.windows[&wid].leaf_render_cache[&native_view]
            .native
            .is_some(),
        "terminal focus still captures the native sibling"
    );

    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(native_view);
    app.sync_window(wid);
    let (native_focused, native_metadata) = capture(&mut app);
    assert_eq!(
        (native_focused.0, native_focused.1),
        (terminal_focused.0, terminal_focused.1)
    );
    assert!(native_focused.2.is_some());
    assert_eq!(native_metadata.frame_kind, "composite");
    assert_eq!(native_metadata.view, None);
    assert!(
        native_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "native" && leaf.focused && leaf.view == native_view.get()
        })
    );
    assert!(native_metadata.leaves.iter().any(|leaf| {
        leaf.kind == "terminal" && !leaf.focused && leaf.view == terminal_view.get()
    }));
    assert!(
        app.windows[&wid]
            .leaf_render_cache
            .contains_key(&terminal_view),
        "native focus still captures the terminal sibling"
    );
}

#[test]
fn mixed_plain_capture_strips_effects_from_an_owned_copy_only() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let terminal_leaf = app
        .active_visible_leaf_plan(wid)
        .unwrap()
        .leaf(terminal_view)
        .expect("terminal leaf")
        .clone();
    let trail = aterm_render::TrailCell {
        row: usize::from(app.tab_strip_rows)
            + terminal_leaf.rect.origin.y.round().max(0.0) as usize
            + (terminal_leaf.rect.size.height * 0.5).floor().max(0.0) as usize,
        col: terminal_leaf.rect.origin.x.round().max(0.0) as usize
            + (terminal_leaf.rect.size.width * 0.5).floor().max(0.0) as usize,
        alpha: 255,
    };
    let decoration = aterm_render::WordDecoration {
        row: u16::try_from(trail.row).expect("fixture row fits"),
        col: u16::try_from(trail.col).expect("fixture column fits"),
        dx: 0,
        dy: 0,
        glyph: aterm_render::DecoGlyph::Paw,
        blend: aterm_render::DecoBlend::Over,
        color: 0x00FF_00FF,
        alpha: 255,
    };
    {
        let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
        assert!(trail.row < input.rows && trail.col < input.cols);
        input.cursor_trail = vec![trail];
        input.cursor_trail_color = 0x00FF_FFFF;
        input.word_decorations = vec![decoration];
    }
    let presented = app.windows[&wid].input_scratch.clone();
    let capture = |app: &mut App, clean: bool, input: aterm_render::RenderInput| {
        let (tx, rx) = std::sync::mpsc::channel();
        let capture = retained_capture(app, input, crate::app_render::HostVisualState::default());
        app.image_tail(
            wid,
            capture,
            crate::control::ImageReq {
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                target: control_auth::ConfinedImage::in_memory(),
                clean,
                session: None,
                want_bytes: true,
                want_metadata: false,
                frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
                cancel: crate::control::CaptureCancellation::new(),
                reply: tx,
            },
        );
        rx.recv_timeout(WORKER_PATIENCE)
            .expect("mixed plain reply")
            .expect("mixed plain capture")
            .value
            .2
            .expect("inline PNG bytes")
    };

    let styled = capture(&mut app, false, presented.clone());
    let plain = capture(&mut app, true, presented);
    let png_fingerprint = |bytes: &[u8]| {
        use std::hash::{Hash, Hasher};

        let mut hash = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hash);
        hash.finish()
    };
    assert_ne!(
        png_fingerprint(&styled),
        png_fingerprint(&plain),
        "the styled mixed frame contains terminal effects that plain removes"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.cursor_trail,
        vec![trail],
        "plain capture never mutates the retained application-present frame"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.word_decorations,
        vec![decoration],
        "plain capture never mutates retained sparkle decorations"
    );
}

#[test]
fn video_dump_reply_keeps_path_last_and_reports_honest_coverage() {
    let root = unique_dir("video-dump");
    ensure_private_dir(&root).unwrap();
    let dir = crate::control_auth::confine_video_dir(&root).expect("server-minted video dir");
    let dir_path = dir.path().to_path_buf();
    let frame = |seq: u64, t_us: u64, rgba: Vec<u8>| aterm_gpu::video_tap::CapturedFrame {
        seq,
        t_us,
        w: 2,
        h: 2,
        rgba,
    };
    let mut lit = vec![0u8; 16];
    lit[5] = 1;
    let frames = [frame(4, 1_000, vec![0u8; 16]), frame(5, 2_000, lit)];
    let fps = frames
        .each_ref()
        .map(crate::video_key_analysis::frame_fingerprint);
    assert_ne!(fps[0], fps[1]);
    let rows = [
        format!(
            "{{\"n\":1,\"seq\":4,\"t_us\":1000,\"fp\":{},\"delta\":0,",
            fps[0]
        ),
        format!(
            "{{\"n\":2,\"seq\":5,\"t_us\":2000,\"fp\":{},\"delta\":1,",
            fps[1]
        ),
    ];
    let take = aterm_gpu::video_tap::VideoTake {
        frames: frames.into(),
        dropped: 1,   // ring skips (mid-stream)
        evicted: 3,   // budget evictions (head truncation)
        decimated: 2, // fps= gate (deliberate, not a loss)
        fps_cap: Some(10),
        budget_bytes: 64 << 20,
        requested_ms: 8_000,
        w: 2,
        h: 2,
        device_px: (4, 4),
        half_res: true,
        format: "rgba8",
        resized_early_stop: false,
    };
    let (rx, _export) = run_video_dump(dir, take, false, 500);
    let reply = rx.recv().expect("dump reply");
    assert!(reply.starts_with("OK "), "reply: {reply}");
    let toks: Vec<&str> = reply.split_whitespace().collect();
    assert_eq!(
        toks.last().copied(),
        Some(dir_path.join("index.json").display().to_string().as_str()),
        "the path must remain the LAST whitespace token"
    );
    assert!(toks.contains(&"frames=2"), "reply: {reply}");
    assert!(
        toks.contains(&"dropped=4"),
        "dropped stays the TOTAL loss (1 ring + 3 evicted): {reply}"
    );
    let ht = toks.iter().position(|t| *t == "head_truncated=true");
    assert!(
        ht.is_some_and(|i| i < toks.len() - 1),
        "head_truncated goes strictly BEFORE the path: {reply}"
    );
    let index = std::fs::read_to_string(dir_path.join("index.json")).expect("index.json written");
    for key in [
        "\"head_truncated\": true",
        "\"evicted_frames\": 3",
        "\"ring_skipped\": 1",
        "\"decimated_frames\": 2",
        "\"requested_ms\": 8000",
        "\"covered_us\": [1000, 2000]",
        "\"fps_cap\": 10",
        "\"budget_mib\": 64",
        "\"mode\": \"swapchain-tap\"",
    ] {
        assert!(index.contains(key), "index.json missing {key}:\n{index}");
    }
    for row in &rows {
        assert!(
            index.contains(row.as_str()),
            "index.json missing {row}:\n{index}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn headless_capture_emits_and_collects_an_eligible_cat() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    app.config.cursor_trail = Some(false);
    let t0 = Instant::now();
    {
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let ws = app.windows.get_mut(&wid).expect("window 0");
        ws.pending_deco_birth = Some(t0);
        let mut term = crate::term_lock(&terminal);
        term.process(b"\r\n\r\n\r\nhello kitty friend");
        term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
    }

    let first_capture = t0 + Duration::from_secs(15);
    app.capture_words(wid, first_capture);

    {
        let ws = &app.windows[&wid];
        assert!(
            !aterm_render::sprites::published(&ws.input_scratch.fx_layers).is_empty(),
            "first headless capture after output includes the peeking cat"
        );
        assert!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers)
                .iter()
                .all(|s| !s.art.is_empty()),
            "a visible cat must publish its atlas"
        );
    }
    assert_eq!(app.kitty_log.log().sightings, 1);
    assert!(
        app.kitty_log
            .log()
            .collectibles
            .iter()
            .any(|item| item.count > 0),
        "the same visible sprite must be observed as a collectible"
    );
    assert_eq!(
        app.kitty_log.favourite_look(),
        None,
        "an ambient discovery never becomes the companion"
    );
    assert!(
        !app.windows[&wid].fx.companions().cursor_cat.is_active(),
        "and no discovery hello starts from a capture"
    );

    app.capture_words(wid, first_capture + Duration::from_secs(30));
    assert!(
        aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
            .iter()
            .all(|sprite| sprite.z != aterm_render::sprites::FreeZ::OverText),
        "an ambient discovery presents no over-text cursor hello"
    );
    assert_eq!(
        app.kitty_log.log().sightings,
        1,
        "a no-damage capture continues the episode without recollecting it"
    );

    app.capture_words(wid, first_capture + Duration::from_secs(40));
    assert!(
        aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
            .iter()
            .all(|sprite| sprite.z != aterm_render::sprites::FreeZ::UnderText),
        "the preview is created only by a damaged rescan"
    );
    assert_eq!(app.kitty_log.log().sightings, 1);
}

#[test]
fn a_capture_of_another_tab_carries_only_that_tabs_words() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let t0 = Instant::now();
    app.recompute_sparkle();
    let session_a = app.front_terminal(wid).expect("front terminal").session;
    let write = |app: &App, session: u64, bytes: &[u8]| {
        let term = app.pool.get(session).expect("pane session").term.clone();
        crate::term_lock(&term).process(bytes);
    };
    write(&app, session_a, b"\r\n\r\n\r\nhello kitty friend");
    {
        let ws = app.windows.get_mut(&wid).expect("window 0");
        ws.pending_deco_birth = Some(t0);
    }
    app.capture_words(wid, t0);
    let (cell_w, cell_h) = {
        let (w, h) = app.win_cell_size(wid);
        (w as i32, h as i32)
    };
    assert!(
        cell_h > 0 && cell_w > 0,
        "PRECONDITION: the fixture has real cell metrics"
    );
    let cat_at_col = |app: &App, col: u16| {
        aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
            .iter()
            .any(|s| (s.x - i32::from(col) * cell_w).abs() <= cell_w)
    };
    let first_word_col: u16 = 6;
    assert!(
        cat_at_col(&app, first_word_col),
        "PRECONDITION: tab A's own cat really stands at its own word"
    );

    let session_b = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(session_b));
    assert_eq!(
        app.front_terminal(wid).expect("front terminal").session,
        session_b,
        "PRECONDITION: the new tab is the one on glass now"
    );
    let tab_b_col: u16 = 30;
    write(
        &app,
        session_b,
        format!("\x1b[6;{}Hkitty waits", tab_b_col + 1).as_bytes(),
    );
    {
        let ws = app.windows.get_mut(&wid).expect("window 0");
        ws.pending_deco_birth = Some(t0);
    }
    let at = t0 + Duration::from_millis(16);
    app.capture_words(wid, at);
    assert!(
        cat_at_col(&app, tab_b_col),
        "tab B's OWN cat must be the cat in this image"
    );
    assert!(
        !cat_at_col(&app, first_word_col),
        "and tab A's word list must not have survived the switch — this \
         capture is of tab B's grid, where that column is blank"
    );

    app.switch_tab_in(wid, 0);
    {
        let ws = app.windows.get_mut(&wid).expect("window 0");
        ws.pending_deco_birth = Some(at);
    }
    assert_eq!(
        app.front_terminal(wid).expect("front terminal").session,
        session_a,
        "PRECONDITION: tab A is front again"
    );
    app.capture_words(wid, at + Duration::from_millis(16));
    assert!(
        cat_at_col(&app, first_word_col),
        "and tab A's own capture has its own cat back"
    );
}

#[test]
fn headless_capture_preserves_complete_curse_wince_for_next_frame() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let t0 = Instant::now();
    {
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let ws = app.windows.get_mut(&wid).expect("window 0");
        ws.fx
            .companions_mut()
            .cursor_cat
            .on_collect(t0, aterm_effects::kitty_registry::KittyLook::default());
        ws.fx
            .companions_mut()
            .cursor_cat
            .set_collection_presentable(t0, false);
        ws.pending_deco_birth = Some(t0);
        ws.fx.decos_mut().note_typed_edit(t0, None);
        let mut term = crate::term_lock(&terminal);
        term.process(b"fuck");
        term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
    }

    let capture_at = t0 + Duration::from_millis(16);
    app.capture_words(wid, capture_at);
    let frame = app
        .windows
        .get_mut(&wid)
        .expect("window 0")
        .fx
        .companions_mut()
        .cursor_cat
        .static_frame(capture_at + Duration::from_millis(1));
    assert_eq!(
        frame.reaction,
        aterm_effects::kitty_cursor::CatReaction::Wince,
        "the silent capture drain must retain the visual wince"
    );
    assert_eq!(
        frame.pose,
        aterm_effects::kitty_cursor::CatPose::STILL,
        "headless reduced-motion capture keeps the wince expression static"
    );
}

#[test]
fn capture_rescan_refreshes_cells_after_snapshot_race() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let now = Instant::now();
    {
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let ws = app.windows.get_mut(&wid).expect("window 0");
        let mut term = crate::term_lock(&terminal);
        term.process(b"ordinary text");
        term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);

        ws.pending_deco_birth = Some(now);
        term.process(b"\r\n\r\n\r\nhello kitty friend");
    }

    app.capture_words(wid, now);

    let ws = &app.windows[&wid];
    assert!(
        !aterm_render::sprites::published(&ws.input_scratch.fx_layers).is_empty(),
        "the splice rescans the post-snapshot word from fresh cells"
    );
    assert_eq!(
        app.kitty_log.log().sightings,
        1,
        "the fresh word's collectible is not consumed against stale cells"
    );
}

#[test]
fn capture_birth_stamp_is_bounded_and_headless_preview_is_stable() {
    let now = Instant::now();
    assert_eq!(bounded_capture_birth(now, None), None);
    assert_eq!(
        bounded_capture_birth(now, Some(now + Duration::from_secs(1))),
        Some(now),
        "a future stamp sanitizes to capture time"
    );
    assert_eq!(
        now.duration_since(
            bounded_capture_birth(now, Some(now - Duration::from_secs(60))).expect("bounded stamp"),
        ),
        CAPTURE_BIRTH_MAX_AGE,
        "stale stamps clamp to the episode-retention horizon"
    );
    assert_eq!(
        now.duration_since(capture_rescan_birth(
            now,
            Some(now - Duration::from_secs(60)),
            true,
        )),
        CAPTURE_PREVIEW_AGE,
        "windowless first presentation samples a visible pose, not wall time"
    );
    assert_eq!(
        now.duration_since(capture_rescan_birth(
            now,
            Some(now - Duration::from_secs(6)),
            false,
        )),
        CAPTURE_PREVIEW_AGE,
        "an occluded window's capture-first presentation cannot age unseen past Done"
    );
    assert_eq!(
        now.duration_since(capture_rescan_birth(
            now,
            Some(now - Duration::from_millis(100)),
            false,
        )),
        Duration::from_millis(100),
        "a genuinely fresh windowed capture preserves its output timing"
    );
}

#[test]
fn capture_during_freeze_preserves_and_resumes_episodes() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let t0 = Instant::now();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    app.recompute_sparkle();
    {
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let ws = app.windows.get_mut(&wid).expect("window 0");
        let mut term = crate::term_lock(&terminal);
        term.process(b"a happy kitty naps");
        term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
    }
    app.capture_words(wid, t0);
    assert!(
        app.windows[&wid].fx.decos().is_active(t0),
        "the fixture's feline effect is animating (sparkle defaults on)"
    );
    {
        let ws = app.windows.get_mut(&wid).expect("window 0");
        ws.fx
            .companions_mut()
            .cursor_cat
            .on_collect(t0, aterm_effects::kitty_registry::KittyLook::default());
        ws.fx
            .companions_mut()
            .cursor_cat
            .set_collection_presentable(t0, false);
    }
    app.perf_reduced = true;
    app.capture_words(wid, t0 + Duration::from_millis(100));
    {
        let ws = &app.windows[&wid];
        assert!(ws.input_scratch.word_decorations.is_empty());
        assert!(ws.input_scratch.ink.is_empty());
        assert!(ws.input_scratch.nova_add.is_empty());
        assert_eq!(
            ws.fx
                .companions()
                .cursor_cat
                .static_deadline(t0 + Duration::from_millis(100)),
            None,
            "a suppressed capture keeps the collection hello paused"
        );
    }
    {
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let ws = app.windows.get_mut(&wid).expect("window 0");
        let mut term = crate::term_lock(&terminal);
        term.process(b" zzz");
        term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
    }
    app.capture_words(wid, t0 + Duration::from_secs(20));
    assert!(
        app.windows[&wid].input_scratch.word_decorations.is_empty(),
        "still suspended: cleared channels"
    );
    assert_eq!(
        app.windows[&wid]
            .fx
            .companions()
            .cursor_cat
            .static_deadline(t0 + Duration::from_secs(20)),
        None,
        "twenty seconds of suppressed captures consume no hello time"
    );
    app.perf_reduced = false;
    let t1 = t0 + Duration::from_secs(20) + Duration::from_millis(50);
    app.capture_words(wid, t1);
    assert!(
        app.windows[&wid].fx.decos().is_active(t1),
        "episodes resume where they paused (freeze/thaw, not reset)"
    );
    assert!(
        aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
            .iter()
            .any(|sprite| sprite.z == aterm_render::sprites::FreeZ::UnderText
                && *sprite != captured_resident_body_for_test(&app, wid)),
        "the resumed ambient episode emits its own cat; the resident cannot buy this pass"
    );
}

#[test]
fn load_shed_capture_retires_an_unowned_pet_before_its_early_return() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    app.recompute_sparkle();
    app.sparkle = None;
    app.capture_words(wid, t0);
    assert!(
        app.windows[&wid].fx.companion().brain().is_active(),
        "negative control: an owned capture materializes the resident pet"
    );

    app.perf_reduced = true;
    assert!(
        app.load_shed_active(),
        "fixture reaches the early-return arm"
    );
    app.capture_words(wid, t0 + Duration::from_millis(16));
    assert!(
        app.windows[&wid].fx.companion().brain().is_active(),
        "load shedding keeps the resident without revoking ownership"
    );
    assert!(
        app.windows[&wid]
            .fx
            .companion()
            .next_change_deadline(t0 + Duration::from_millis(16))
            .is_some()
    );

    app.config.cursor_trail = Some(false);
    assert!(
        app.load_shed_active(),
        "owner removal does not end load shedding"
    );
    app.capture_words(wid, t0 + Duration::from_millis(32));
    let ws = &app.windows[&wid];
    assert!(
        !ws.fx.companion().brain().is_active() && !ws.fx.companion().brain().needs_frames(),
        "owner removal must retire the brain before load shedding returns"
    );
    assert!(
        ws.fx.free_sprites().is_empty()
            && aterm_render::sprites::published(&ws.input_scratch.fx_layers).is_empty(),
        "the retired capture publishes no companion projection"
    );
    assert_eq!(
        ws.fx.companion().hit_rect(),
        None,
        "owner removal clears body publication too"
    );
}

#[test]
fn encode_worker_survives_dropped_reply_receiver() {
    let dir = std::env::temp_dir().join(format!("aterm-encode-dead-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let mut app = App::headless_for_test();
    let (dead_tx, dead_rx) = std::sync::mpsc::channel();
    drop(dead_rx);
    app.submit_encode_job(EncodeJob::Image {
        pixels: CapturePixels::packed(Frame {
            width: 1,
            height: 1,
            pixels: vec![0u32; 1],
        })
        .expect("a whole frame"),
        metadata: None,
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: confined(&dir, "dead.png"),
        want_bytes: false,
        cancel: crate::control::CaptureCancellation::new(),
        reply: dead_tx,
    });
    let (tx, rx) = std::sync::mpsc::channel();
    app.submit_encode_job(EncodeJob::Image {
        pixels: CapturePixels::packed(Frame {
            width: 2,
            height: 2,
            pixels: vec![0u32; 4],
        })
        .expect("a whole frame"),
        metadata: None,
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: confined(&dir, "live.png"),
        want_bytes: false,
        cancel: crate::control::CaptureCancellation::new(),
        reply: tx,
    });
    let mut retained = rx
        .recv()
        .expect("worker alive after dead client")
        .expect("second encode succeeds");
    assert_eq!(retained.value, (2, 2, None));
    retained
        .retention
        .as_mut()
        .expect("file reply carries exact handles")
        .prepare_write()
        .expect("live reply validates at the wire edge");
    assert!(!dir.join("dead.png").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inline_image_retains_admission_through_the_wire_handoff() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-inline-image-retention-{}",
        std::process::id()
    ));
    ensure_private_dir(&dir).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    run_encode_job(
        EncodeJob::Image {
            pixels: CapturePixels::packed(Frame {
                width: 2,
                height: 2,
                pixels: vec![0u32; 4],
            })
            .expect("a whole frame"),
            metadata: None,
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: confined(&dir, "unused.png"),
            want_bytes: true,
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        },
        None,
    );
    let mut retained = rx.recv().expect("inline reply").expect("inline encode");
    assert!(retained.value.2.is_some(), "inline reply carries PNG bytes");
    retained
        .retention
        .as_mut()
        .expect("byte reply retains its admission permit")
        .prepare_write()
        .expect("in-memory wire payload is ready");
    assert!(
        !dir.join("unused.png").exists(),
        "inline capture never publishes a file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
