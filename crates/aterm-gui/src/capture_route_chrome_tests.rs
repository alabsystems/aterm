// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Chrome composition tests across present and capture routes.

use super::{ChromeStep, trace};
use crate::scripted_redraw_fixture::{glass, headless, scripted_app};
use crate::{App, VisibleContentRoute, WindowId};
use std::time::{Duration, Instant};

const WID: WindowId = WindowId(0);

const HANG: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
enum Shape {
    TerminalSingle,
    TerminalComposed,
    Native,
    Heterogeneous,
}

impl Shape {
    const ALL: [Shape; 4] = [
        Shape::TerminalSingle,
        Shape::TerminalComposed,
        Shape::Native,
        Shape::Heterogeneous,
    ];

    fn shows(self, route: VisibleContentRoute) -> bool {
        match self {
            Shape::TerminalSingle => route == VisibleContentRoute::Terminal { composed: false },
            Shape::TerminalComposed => route == VisibleContentRoute::Terminal { composed: true },
            Shape::Native => matches!(route, VisibleContentRoute::Native { .. }),
            Shape::Heterogeneous => route == VisibleContentRoute::Heterogeneous,
        }
    }
}

fn shaped(shape: Shape) -> App {
    let mut app = scripted_app();
    match shape {
        Shape::TerminalSingle => {}
        Shape::TerminalComposed => {
            app.split_active_stub_tab(WID);
        }
        Shape::Native => {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        }
        Shape::Heterogeneous => {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
            app.split_active_with_stub_terminal(WID, crate::tab_model::SplitAxis::Horizontal);
        }
    }
    let (_, route) = app
        .active_visible_frame_layout(WID)
        .expect("a visible layout");
    assert!(shape.shows(route), "{shape:?} shows {route:?}");
    app
}

fn present(app: &mut App) -> Vec<u32> {
    let serial = app.windows[&WID].capture_present_serial;
    app.windows.get_mut(&WID).expect("window").last_present = None;
    app.redraw_window(WID);
    assert_eq!(
        app.windows[&WID].capture_present_serial,
        serial + 1,
        "the redraw presented"
    );
    glass(app).1
}

fn image(app: &mut App) -> (usize, usize, Vec<u8>) {
    headless(app, image_now)
}

fn image_now(app: &mut App) -> (usize, usize, Vec<u8>) {
    let (reply, rx) = std::sync::mpsc::channel();
    app.render_image(crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: crate::control_auth::ConfinedImage::in_memory(),
        clean: false,
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
}

fn snapshot(app: &mut App) {
    headless(app, snapshot_now);
}

fn snapshot_now(app: &mut App) {
    let nonce = aterm_time::since_epoch()
        .expect("clock after epoch")
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("aterm-route-chrome-{}-{nonce}", std::process::id()));
    crate::control_auth::ensure_private_dir(&dir).expect("snapshot dir");
    let path = dir.join("snapshot.png");
    app.snapshot_at_path(path.display().to_string());
    let mut done = path.into_os_string();
    done.push(".done");
    let done = std::path::PathBuf::from(done);
    let deadline = Instant::now() + HANG;
    while !done.exists() {
        assert!(Instant::now() < deadline, "the snapshot was published");
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn one_order(present: &[ChromeStep], capture: &[ChromeStep]) -> bool {
    present == capture
}

#[test]
fn every_route_layers_chrome_through_one_composition() {
    for shape in Shape::ALL {
        let mut app = shaped(shape);
        let (_, route) = app.active_visible_frame_layout(WID).expect("layout");
        let _ = trace::take();
        present(&mut app);
        let (presents, presented) = trace::take();
        image(&mut app);
        let (images, imaged) = trace::take();
        snapshot(&mut app);
        let (snapshots, snapshotted) = trace::take();
        assert_eq!(
            (presents, images, snapshots),
            (1, 1, 1),
            "{shape:?}: a present, an image and a snapshot each compose once"
        );
        assert!(
            one_order(&presented, &imaged),
            "{shape:?}: image {imaged:?} vs present {presented:?}"
        );
        assert!(
            one_order(&presented, &snapshotted),
            "{shape:?}: snapshot {snapshotted:?} vs present {presented:?}"
        );
        assert_eq!(
            presented,
            ChromeStep::order(route).collect::<Vec<_>>(),
            "{shape:?}: the present ran every layer of its route"
        );
        let terminal = matches!(route, VisibleContentRoute::Terminal { .. });
        for (step, on_terminal) in [
            (ChromeStep::PaneOverlay, true),
            (ChromeStep::SettingsPanel, true),
            (ChromeStep::Wallpaper, true),
            (ChromeStep::RouteCard, false),
        ] {
            assert_eq!(
                presented.contains(&step),
                on_terminal == terminal,
                "{shape:?}: {step:?}"
            );
        }
        assert!(presented.contains(&ChromeStep::StatsDeck), "{shape:?}");
    }

    let mut app = shaped(Shape::TerminalSingle);
    let _ = trace::take();
    present(&mut app);
    let (_, presented) = trace::take();
    {
        let _mask = trace::mask(&[ChromeStep::BuildBadge]);
        image(&mut app);
    }
    let (images, imaged) = trace::take();
    assert_eq!(images, 1);
    assert!(
        !one_order(&presented, &imaged),
        "a capture without the badge is not the present's order: {imaged:?}"
    );
}

#[test]
fn native_present_is_unchanged_by_the_footer_menu_and_link_steps() {
    const STEPS: [ChromeStep; 3] = [
        ChromeStep::ClaudeFooter,
        ChromeStep::AutopilotMenu,
        ChromeStep::LinkTarget,
    ];
    let mut app = shaped(Shape::Native);
    // The chrome fonts the first present would install after it, so the page it presents
    // is not redrawn for them in the next.
    app.sync_chrome_fonts();
    let masked = {
        let _mask = trace::mask(&STEPS);
        present(&mut app)
    };
    assert_eq!(
        present(&mut app),
        masked,
        "native Settings: the steps change no pixel"
    );

    let mut app = shaped(Shape::Heterogeneous);
    let (term, session) = {
        let front = app
            .front_terminal(WID)
            .expect("the terminal leaf is focused");
        (front.term.clone(), front.session)
    };
    crate::term_lock(&term)
        .process(b"\x1b]8;;https://evil.example/steal\x1b\\google.com\x1b]8;;\x1b\\");
    present(&mut app);
    let (plan, _) = app.active_visible_frame_layout(WID).expect("layout");
    let leaf = plan
        .leaves
        .iter()
        .find(|leaf| {
            app.view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                == Some(session)
        })
        .expect("the terminal leaf");
    let (cw, ch) = app.win_cell_size(WID);
    let x = app.win_pad(WID) as f64 + (f64::from(leaf.rect.origin.x) + 2.5) * cw as f64;
    let y = (app.win_pad_top(WID) + app.win_head(WID)) as f64
        + f64::from(app.tab_strip_rows) * ch as f64
        + (f64::from(leaf.rect.origin.y) + 0.5) * ch as f64;
    app.on_cursor_moved(WID, x, y);
    assert!(
        app.windows[&WID].link_hover.is_some(),
        "precondition: the pointer rests on the link"
    );
    let masked = {
        let _mask = trace::mask(&STEPS);
        present(&mut app)
    };
    assert_ne!(
        present(&mut app),
        masked,
        "the caption is on the glass only through the link step"
    );
}

#[test]
fn headless_capture_shows_the_wallpaper() {
    let mut app = scripted_app();
    app.tab_strip_rows = 1;
    app.push_stub_tab(WID, crate::stub_session(app.next_session_id));
    app.splice_tab_strip(WID);
    let wallpaper = std::sync::Arc::new(crate::app_config::ConfigAssetCatalog {
        wallpaper: crate::app_config::WallpaperAsset::Ready {
            source_id: "test-wallpaper".into(),
            w: 2,
            h: 2,
            rgba: std::sync::Arc::from([0xE0_u8, 0x20, 0x20, 0xFF].repeat(4)),
            fp: 0x57A11,
        },
        ..crate::app_config::ConfigAssetCatalog::default()
    });
    app.config_assets = std::sync::Arc::clone(&wallpaper);
    let papered = image(&mut app);
    assert_eq!(
        image(&mut app),
        papered,
        "precondition: a capture is repeatable"
    );

    app.config_assets = std::sync::Arc::default();
    let bare = image(&mut app);
    assert_eq!((bare.0, bare.1), (papered.0, papered.1));
    assert_ne!(bare, papered, "the wallpaper is in the capture");
}

#[test]
fn terminal_present_visuals_equal_host_visual_state() {
    let mut app = scripted_app();
    let rung = Instant::now() + Duration::from_secs(3600);
    let during = rung + Duration::from_millis(50);
    let after = rung + aterm_core::bell::FLASH_DURATION + Duration::from_millis(50);
    app.level_up = Some(crate::level_up::LevelUp::charging_continued(7, rung, 0.0));
    {
        let ws = app.windows.get_mut(&WID).expect("window");
        ws.bell_flash.ring(rung);
        ws.drag_hover = true;
    }
    let present_at = |app: &mut App, at: Instant| {
        let serial = app.windows[&WID].capture_present_serial;
        app.windows.get_mut(&WID).expect("window").last_present = None;
        app.redraw_window_at(WID, at);
        let ws = &app.windows[&WID];
        assert_eq!(
            ws.capture_present_serial,
            serial + 1,
            "the redraw presented"
        );
        let painted = crate::app_render::HostVisualState {
            invert: ws.capture_present_invert,
            overlay: ws.capture_present_overlay,
        };
        (painted, app.host_visual_state(WID, at))
    };

    let (painted, policy) = present_at(&mut app, during);
    assert_eq!(painted, policy);
    assert!(painted.invert && painted.overlay.is_some());
    let wash = painted.overlay;

    app.windows.get_mut(&WID).expect("window").drag_hover = false;
    let (painted, policy) = present_at(&mut app, during);
    assert_eq!(painted, policy);
    assert!(painted.invert && painted.overlay.is_some() && painted.overlay != wash);

    app.windows.get_mut(&WID).expect("window").drag_hover = true;
    app.palette_enter();
    assert!(app.windows[&WID].overlay_open());
    let (painted, policy) = present_at(&mut app, during);
    assert_eq!(painted, policy);
    assert!(!painted.invert && painted.overlay.is_some() && painted.overlay != wash);

    app.palette_exit();
    let ws = app.windows.get_mut(&WID).expect("window");
    assert!(!ws.overlay_open());
    ws.drag_hover = false;
    assert!(ws.bell_flash.is_active(Instant::now()));
    let (painted, policy) = present_at(&mut app, after);
    assert_eq!(painted, policy);
    assert!(!painted.invert, "the bell is judged at the frame clock");
    let (painted, _) = present_at(&mut app, during);
    assert!(painted.invert, "and inside the flash it inverts");
}
