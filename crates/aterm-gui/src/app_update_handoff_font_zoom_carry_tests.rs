// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;
use crate::app_config::{LaunchFont, successor_font_px};
use crate::session_store::SessionHandoff;

#[test]
fn a_zoomed_font_crosses_the_handoff() {
    let mut parent = App::headless_for_test();
    parent
        .windows
        .get_mut(&crate::WindowId(0))
        .expect("window 0")
        .scale = 2.0;
    parent.font_size = 12.0;
    parent.default_font_size = 12.0;
    parent.font_px = 24.0;
    let unzoomed = parent.handoff_window_carry(0).expect("a window");
    assert_eq!(
        (
            unzoomed.font_px_milli,
            unzoomed.font_reset_px_milli,
            unzoomed.font_size_milli,
            unzoomed.font_reset_size_milli
        ),
        (None, None, None, None)
    );
    let handoff = |window| SessionHandoff {
        resources: None,
        schema: SessionHandoff::SCHEMA,
        sessions: Vec::new(),
        window: Some(window),
        connections: Vec::new(),
        next_turn_id: None,
        outgoing_build: None,
        held: Vec::new(),
        roster_seq: None,
        fabric_attached: Vec::new(),
        fleet_told: Vec::new(),
        mirror_dropped: Vec::new(),
        mirror_served: false,
        machine_names: Vec::new(),
    };
    let wire = handoff(unzoomed).to_toml().expect("serializes");
    assert!(!wire.contains("font_"), "no key for no zoom: {wire}");

    assert!(parent.set_font_size_with(12.0 + crate::FONT_ZOOM_STEP, |_| true));
    assert_eq!(parent.font_px, 26.0, "a step at 2x is two device px");
    let zoomed = parent.handoff_window_carry(0).expect("a window");
    assert_eq!(
        (zoomed.font_size_milli, zoomed.font_reset_size_milli),
        (Some(13_000), Some(12_000))
    );
    assert_eq!(
        (zoomed.font_px_milli, zoomed.font_reset_px_milli),
        (Some(26_000), Some(24_000)),
        "an older successor reads the same zoom in device px"
    );
    let wire = handoff(zoomed.clone()).to_toml().expect("serializes");
    let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
    let carried = read.window.expect("the window carry");
    assert_eq!(carried, zoomed, "the pairs round-trip");

    let launch = successor_font_px(12.0, Some(&carried));
    assert_eq!(
        launch,
        LaunchFont {
            size: 13.0,
            reset: 12.0,
            from_device: false
        }
    );
    let mut successor = App::headless_for_test();
    successor.font_size = launch.size;
    successor.default_font_size = launch.reset;
    assert_eq!(
        successor.handoff_font_zoom().size,
        Some((13_000, 12_000)),
        "and the next handoff carries it on"
    );
    assert!(successor.set_font_size_with(successor.default_font_size, |_| true));
    assert!((successor.font_size - 12.0).abs() < f32::EPSILON);
    assert_eq!(
        successor.handoff_font_zoom(),
        crate::app_config::CarriedZoom::default(),
        "a font back at its reset size is no zoom"
    );

    let mut older = carried.clone();
    older.font_size_milli = None;
    older.font_reset_size_milli = None;
    assert_eq!(
        successor_font_px(12.0, Some(&older)),
        LaunchFont {
            size: 26.0,
            reset: 24.0,
            from_device: true
        }
    );
    assert_eq!(
        crate::app_window::font_size_of_device_px(26.0, 2.0),
        13.0,
        "the device pair reads back as the size it drew"
    );
    let mut bare = older.clone();
    bare.font_px_milli = None;
    bare.font_reset_px_milli = None;
    let config = LaunchFont {
        size: 14.0,
        reset: 14.0,
        from_device: false,
    };
    assert_eq!(successor_font_px(14.0, Some(&bare)), config);
    assert_eq!(successor_font_px(14.0, None), config);
    for (px, reset) in [
        (Some(13_000), None),
        (None, Some(12_000)),
        (Some(0), Some(12_000)),
        (Some(13_000), Some(5_999)),
        (Some(200_001), Some(12_000)),
        (Some(u32::MAX), Some(12_000)),
    ] {
        let mut hostile = bare.clone();
        hostile.font_size_milli = px;
        hostile.font_reset_size_milli = reset;
        assert_eq!(
            successor_font_px(14.0, Some(&hostile)),
            config,
            "{px:?} {reset:?}"
        );
    }
    let mut edge = bare;
    edge.font_size_milli = Some(200_000);
    edge.font_reset_size_milli = Some(6_000);
    assert_eq!(
        successor_font_px(14.0, Some(&edge)),
        LaunchFont {
            size: 200.0,
            reset: 6.0,
            from_device: false
        }
    );
}

#[test]
fn a_zoom_out_after_a_handoff_restores_the_window_it_zoomed_in() {
    use crate::WindowId;
    use aterm_window::dpi::PhysicalSize;
    let w0 = WindowId(0);
    let grid = |app: &App| (app.windows[&w0].rows, app.windows[&w0].cols);
    let zoom = |app: &mut App, px: f32, window: PhysicalSize<u32>| {
        app.set_font_size(px);
        app.windows.get_mut(&w0).expect("window 0").win_px = Some(window);
        app.on_resize(w0, window);
    };

    let mut parent = App::headless_for_test();
    let base = parent.font_size;
    parent.default_font_size = base;
    let window = parent.window_frame_px_for(w0, 29, 100);
    zoom(&mut parent, base, window);
    assert_eq!(grid(&parent), (29, 100));
    zoom(&mut parent, base + crate::FONT_ZOOM_STEP, window);
    let zoomed = grid(&parent);
    assert_ne!(
        parent.window_frame_px_for(w0, zoomed.0, zoomed.1),
        window,
        "the zoomed grid leaves a remainder in the window"
    );

    let wire = SessionHandoff {
        resources: None,
        schema: SessionHandoff::SCHEMA,
        sessions: Vec::new(),
        window: parent.handoff_window_carry(0),
        connections: Vec::new(),
        next_turn_id: None,
        outgoing_build: None,
        held: Vec::new(),
        roster_seq: None,
        fabric_attached: Vec::new(),
        fleet_told: Vec::new(),
        mirror_dropped: Vec::new(),
        mirror_served: false,
        machine_names: Vec::new(),
    }
    .to_toml()
    .expect("serializes");
    let carried = SessionHandoff::from_toml(&wire)
        .expect("the successor reads it")
        .window
        .expect("the window carry");
    assert_eq!((carried.rows, carried.cols), zoomed);
    let inner = carried
        .inner_px
        .map(|[width, height]| PhysicalSize::new(width, height));

    let mut successor = App::headless_for_test();
    let launch = successor_font_px(base, Some(&carried));
    successor.set_font_size(launch.size);
    successor.default_font_size = launch.reset;
    let frame = successor.attach_frame_px(w0, carried.rows, carried.cols, inner);
    assert_eq!(frame, window, "window 0 comes up at its parent's size");
    zoom(&mut successor, launch.size, frame);
    assert_eq!(grid(&successor), zoomed, "and holds its parent's grid");

    zoom(&mut successor, launch.size - crate::FONT_ZOOM_STEP, frame);
    assert_eq!(
        grid(&successor),
        (29, 100),
        "the zoom out fits the grid it had"
    );
    assert_eq!(successor.window_frame_px_for(w0, 29, 100), window);

    let exact = parent.window_frame_px_for(w0, zoomed.0, zoomed.1);
    assert_eq!(parent.attach_frame_px(w0, zoomed.0, zoomed.1, None), exact);
    for foreign in [
        PhysicalSize::new(window.width + 64, window.height),
        PhysicalSize::new(exact.width - 1, window.height),
        PhysicalSize::new(u32::MAX, u32::MAX),
    ] {
        assert_eq!(
            parent.attach_frame_px(w0, zoomed.0, zoomed.1, Some(foreign)),
            exact,
            "{foreign:?}"
        );
    }
}

#[test]
fn a_zoom_out_after_a_handoff_restores_every_window_it_zoomed_in() {
    use crate::WindowId;
    use crate::app_window::CarriedFrame;
    use aterm_window::dpi::PhysicalSize;
    let (w0, w1) = (WindowId(0), WindowId(1));
    let grid = |app: &App, wid: WindowId| (app.windows[&wid].rows, app.windows[&wid].cols);
    let zoom = |app: &mut App, px: f32, windows: &[(WindowId, PhysicalSize<u32>)]| {
        app.set_font_size(px);
        for &(wid, size) in windows {
            app.windows.get_mut(&wid).expect("a window").win_px = Some(size);
            app.on_resize(wid, size);
        }
    };

    let mut parent = App::headless_for_test();
    let base = parent.font_size;
    parent.default_font_size = base;
    assert_eq!(
        parent.insert_logical_window(crate::stub_session(1), 40, 120),
        w1
    );
    let desk = [
        (w0, parent.window_frame_px_for(w0, 29, 100)),
        (w1, parent.window_frame_px_for(w1, 40, 120)),
    ];
    zoom(&mut parent, base, &desk);
    assert_eq!(
        (grid(&parent, w0), grid(&parent, w1)),
        ((29, 100), (40, 120))
    );
    zoom(&mut parent, base + crate::FONT_ZOOM_STEP, &desk);
    let zoomed = grid(&parent, w1);
    assert_ne!(
        zoomed,
        grid(&parent, w0),
        "the windows keep their own grids"
    );
    assert_ne!(
        parent.window_frame_px_for(w1, zoomed.0, zoomed.1),
        desk[1].1,
        "the zoomed grid leaves a remainder in window 1"
    );

    let pending = parent.capture_handoff_layout();
    let wire = pending.to_toml().expect("the parent serializes its layout");
    let layout = crate::restore::RestoreManifest::from_toml(&wire).expect("the child parses it");
    assert_eq!(layout, pending, "the size round-trips the wire");
    assert_eq!((layout.windows[1].rows, layout.windows[1].cols), zoomed);

    let carry = parent.handoff_window_carry(0).expect("the window carry");
    let mut successor = App::headless_for_test();
    let launch = successor_font_px(base, Some(&carry));
    zoom(
        &mut successor,
        launch.size,
        &[(w0, parent.windows[&w0].win_px.expect("sized"))],
    );
    successor.default_font_size = launch.reset;
    let ((rows, cols), carried) = successor.new_window_frame(CarriedFrame::of(&layout.windows[1]));
    assert_eq!(
        successor.insert_logical_window(crate::stub_session(1), rows, cols),
        w1
    );
    let frame = successor.attach_frame_px(w1, rows, cols, carried);
    assert_eq!(frame, desk[1].1, "window 1 comes up at its parent's size");
    zoom(&mut successor, launch.size, &[(w1, frame)]);
    assert_eq!(grid(&successor, w1), zoomed, "and holds its parent's grid");

    zoom(
        &mut successor,
        launch.size - crate::FONT_ZOOM_STEP,
        &[(w1, frame)],
    );
    assert_eq!(
        grid(&successor, w1),
        (40, 120),
        "the zoom out fits the grid it had"
    );
    assert_eq!(successor.window_frame_px_for(w1, 40, 120), desk[1].1);

    let dragged = PhysicalSize::new(desk[1].1.width + 1, desk[1].1.height);
    zoom(&mut parent, base + crate::FONT_ZOOM_STEP, &[(w1, dragged)]);
    assert_eq!(grid(&parent, w1), zoomed, "PRECONDITION: inside one cell");
    let live = parent.capture_handoff_layout();
    assert_ne!(live, pending, "PRECONDITION: the row's size moved");
    assert_eq!(
        super::commit_layout_topology(&live),
        super::commit_layout_topology(&pending)
    );
}
