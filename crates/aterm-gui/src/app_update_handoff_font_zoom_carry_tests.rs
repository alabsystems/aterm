// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;
use crate::app_config::{LaunchFont, successor_font_px};
use crate::session_store::SessionHandoff;

/// A Retina window auto-scaled to 24 px and zoomed to 26 px (Cmd-=) hands over the zoom and
/// the Cmd-0 size: the successor (config: unpinned 12 px) comes up at 26 px, pinned, with
/// Cmd-0 returning to 24 px, and carries it on. An unzoomed window writes no key, and an older
/// producer's carry keeps the config's font.
#[test]
fn a_zoomed_font_crosses_the_handoff() {
    let mut parent = App::headless_for_test();
    parent.font_px = 24.0;
    parent.default_font_px = 24.0;
    parent.font_px_explicit = false;
    let unzoomed = parent.handoff_window_carry(0).expect("a window");
    assert_eq!(
        (unzoomed.font_px_milli, unzoomed.font_reset_px_milli),
        (None, None)
    );
    let wire = SessionHandoff {
        schema: SessionHandoff::SCHEMA,
        sessions: Vec::new(),
        window: Some(unzoomed),
        connections: Vec::new(),
        next_turn_id: None,
        outgoing_build: None,
        held: Vec::new(),
        roster_seq: None,
        fabric_attached: Vec::new(),
    }
    .to_toml()
    .expect("serializes");
    assert!(!wire.contains("font_"), "no key for no zoom: {wire}");

    // Cmd-= (the rebuild seam standing in for the renderer).
    assert!(parent.set_font_px_with(26.0, |_| true));
    assert!(parent.font_px_explicit, "a zoom pins the size");
    let zoomed = parent.handoff_window_carry(0).expect("a window");
    assert_eq!(
        (zoomed.font_px_milli, zoomed.font_reset_px_milli),
        (Some(26_000), Some(24_000))
    );
    let wire = SessionHandoff {
        schema: SessionHandoff::SCHEMA,
        sessions: Vec::new(),
        window: Some(zoomed.clone()),
        connections: Vec::new(),
        next_turn_id: None,
        outgoing_build: None,
        held: Vec::new(),
        roster_seq: None,
        fabric_attached: Vec::new(),
    }
    .to_toml()
    .expect("serializes");
    let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
    let carried = read.window.expect("the window carry");
    assert_eq!(carried, zoomed, "the pair round-trips");

    // The successor, whose config asks for the unpinned 12 px base.
    let launch = successor_font_px(12.0, false, Some(&carried));
    assert_eq!(
        launch,
        LaunchFont {
            px: 26.0,
            explicit: true,
            reset_px: 24.0
        }
    );
    assert_eq!(
        crate::app_window::hidpi_target_font_px(launch.explicit, 2.0),
        None,
        "the first window's Retina auto-scale leaves a carried zoom be"
    );
    let mut successor = App::headless_for_test();
    successor.font_px = launch.px;
    successor.font_px_explicit = launch.explicit;
    successor.default_font_px = launch.reset_px;
    assert_eq!(
        successor.handoff_font_zoom(),
        (Some(26_000), Some(24_000)),
        "and the next handoff carries it on"
    );
    // Cmd-0.
    assert!(successor.set_font_px_with(successor.default_font_px, |_| true));
    assert!((successor.font_px - 24.0).abs() < f32::EPSILON);
    assert_eq!(
        successor.handoff_font_zoom(),
        (None, None),
        "a font back at its reset size is no zoom"
    );

    // An older producer (no pair), and a fresh launch: the config's font.
    let mut older = carried.clone();
    older.font_px_milli = None;
    older.font_reset_px_milli = None;
    let config = LaunchFont {
        px: 14.0,
        explicit: true,
        reset_px: 14.0,
    };
    assert_eq!(successor_font_px(14.0, true, Some(&older)), config);
    assert_eq!(successor_font_px(14.0, true, None), config);
    // Negative controls: a pair with a half missing or out of bounds is ignored
    // whole, never clamped.
    for (px, reset) in [
        (Some(26_000), None),
        (None, Some(24_000)),
        (Some(0), Some(24_000)),
        (Some(26_000), Some(5_999)),
        (Some(200_001), Some(24_000)),
        (Some(u32::MAX), Some(24_000)),
    ] {
        let mut hostile = carried.clone();
        hostile.font_px_milli = px;
        hostile.font_reset_px_milli = reset;
        assert_eq!(
            successor_font_px(14.0, true, Some(&hostile)),
            config,
            "{px:?} {reset:?}"
        );
    }
    // The bounds themselves are admitted.
    let mut edge = carried;
    edge.font_px_milli = Some(200_000);
    edge.font_reset_px_milli = Some(6_000);
    assert_eq!(
        successor_font_px(14.0, true, Some(&edge)),
        LaunchFont {
            px: 200.0,
            explicit: true,
            reset_px: 6.0
        }
    );
}

/// The successor's window 0 attaches at the parent's raw inner size, holding exactly the
/// carried zoomed grid, so zooming back out fits the pre-zoom grid and window. Negative
/// controls: an older carry (no size) and a size that is not a remainder of this grid both get
/// the exact frame.
#[test]
fn a_zoom_out_after_a_handoff_restores_the_window_it_zoomed_in() {
    use crate::WindowId;
    use winit::dpi::PhysicalSize;
    let w0 = WindowId(0);
    let grid = |app: &App| (app.windows[&w0].rows, app.windows[&w0].cols);
    // The windowed zoom's re-grid, which `rebuild_backend` runs only for a
    // window with an OS surface.
    let zoom = |app: &mut App, px: f32, window: PhysicalSize<u32>| {
        app.set_font_px(px);
        app.windows.get_mut(&w0).expect("window 0").win_px = Some(window);
        app.on_resize(w0, window);
    };

    let mut parent = App::headless_for_test();
    let base = parent.font_px;
    parent.default_font_px = base;
    parent.font_px_explicit = false;
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
        schema: SessionHandoff::SCHEMA,
        sessions: Vec::new(),
        window: parent.handoff_window_carry(0),
        connections: Vec::new(),
        next_turn_id: None,
        outgoing_build: None,
        held: Vec::new(),
        roster_seq: None,
        fabric_attached: Vec::new(),
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

    // The successor: the carried font and grid, window 0 attached.
    let mut successor = App::headless_for_test();
    let launch = successor_font_px(base, false, Some(&carried));
    successor.set_font_px(launch.px);
    successor.default_font_px = launch.reset_px;
    let frame = successor.attach_frame_px(w0, carried.rows, carried.cols, inner);
    assert_eq!(frame, window, "window 0 comes up at its parent's size");
    zoom(&mut successor, launch.px, frame);
    assert_eq!(grid(&successor), zoomed, "and holds its parent's grid");

    // Cmd--.
    zoom(&mut successor, launch.px - crate::FONT_ZOOM_STEP, frame);
    assert_eq!(
        grid(&successor),
        (29, 100),
        "the zoom out fits the grid it had"
    );
    assert_eq!(successor.window_frame_px_for(w0, 29, 100), window);

    // Negative controls: the exact frame of the carried grid.
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

/// Every window crosses whole: the layout row carries window 1's raw size across the
/// sidecar's TOML, so the successor recreates it at its parent's size and zooming out fits the
/// former grid. A drag inside one cell changes the row's size but not the Commit's topology.
#[test]
fn a_zoom_out_after_a_handoff_restores_every_window_it_zoomed_in() {
    use crate::WindowId;
    use crate::app_window::CarriedFrame;
    use winit::dpi::PhysicalSize;
    let (w0, w1) = (WindowId(0), WindowId(1));
    let grid = |app: &App, wid: WindowId| (app.windows[&wid].rows, app.windows[&wid].cols);
    // The windowed zoom's re-grid of every window inside its own size,
    // which `rebuild_backend` runs only for windows with an OS surface.
    let zoom = |app: &mut App, px: f32, windows: &[(WindowId, PhysicalSize<u32>)]| {
        app.set_font_px(px);
        for &(wid, size) in windows {
            app.windows.get_mut(&wid).expect("a window").win_px = Some(size);
            app.on_resize(wid, size);
        }
    };

    // The parent: window 0 at 29×100, window 1 at 40×120.
    let mut parent = App::headless_for_test();
    let base = parent.font_px;
    parent.default_font_px = base;
    parent.font_px_explicit = false;
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

    // The layout the update carries, across the sidecar's wire.
    let pending = parent.capture_handoff_layout();
    let wire = pending.to_toml().expect("the parent serializes its layout");
    let layout = crate::restore::RestoreManifest::from_toml(&wire).expect("the child parses it");
    assert_eq!(layout, pending, "the size round-trips the wire");
    assert_eq!((layout.windows[1].rows, layout.windows[1].cols), zoomed);

    // The successor: the carried font; window 0, the bootstrap, at its carried grid; window 1
    // created from its row.
    let carry = parent.handoff_window_carry(0).expect("the window carry");
    let mut successor = App::headless_for_test();
    let launch = successor_font_px(base, false, Some(&carry));
    zoom(
        &mut successor,
        launch.px,
        &[(w0, parent.windows[&w0].win_px.expect("sized"))],
    );
    successor.default_font_px = launch.reset_px;
    let ((rows, cols), carried) = successor.new_window_frame(CarriedFrame::of(&layout.windows[1]));
    assert_eq!(
        successor.insert_logical_window(crate::stub_session(1), rows, cols),
        w1
    );
    let frame = successor.attach_frame_px(w1, rows, cols, carried);
    assert_eq!(frame, desk[1].1, "window 1 comes up at its parent's size");
    zoom(&mut successor, launch.px, &[(w1, frame)]);
    assert_eq!(grid(&successor, w1), zoomed, "and holds its parent's grid");

    // Cmd--.
    zoom(
        &mut successor,
        launch.px - crate::FONT_ZOOM_STEP,
        &[(w1, frame)],
    );
    assert_eq!(
        grid(&successor, w1),
        (40, 120),
        "the zoom out fits the grid it had"
    );
    assert_eq!(successor.window_frame_px_for(w1, 40, 120), desk[1].1);

    // A drag of the parent's window 1 inside one cell while the successor
    // boots moves the row's size and not the Commit's topology.
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
