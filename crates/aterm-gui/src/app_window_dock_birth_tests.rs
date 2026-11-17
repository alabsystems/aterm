// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DK-4, docked by default: the birth rule, the setting's default, a new window's inheritance,
//! a headless window, a default dock wider than its screen, the origin slide and a failed attach.

use super::{
    BirthDock, BirthScreens, DOCK_CELLS, DesktopPlane, DockBirth, DockRow, MonitorRectPx,
    WorkAreaPts, birth_dock, docked_origin_x, docks_by_default, frame_is_placed, monitor_holding,
};
use crate::{App, WindowId};
use aterm_window::dpi::{PhysicalPosition, Position};

const WID: WindowId = WindowId(0);

/// A row of the default's table: the setting, headless, which variables are set, and whether
/// windows are born docked.
type DefaultRow<'a> = (Option<bool>, bool, &'a dyn Fn(&str) -> bool, bool);

/// An app whose windows are born as a person's are: not headless, `[navigator] dock = true` set
/// explicitly, so the process environment (a scratch world, CI) cannot decide it.
pub(super) fn docking_app() -> App {
    let mut app = App::headless_for_test();
    dock_setting(&mut app, Some(true));
    app.headless = false;
    app
}

/// `[navigator] dock` set to `on`.
pub(super) fn dock_setting(app: &mut App, on: Option<bool>) {
    app.config.navigator = Some(crate::app_config::NavigatorConfig { dock: on });
}

/// The `site=` of each resize session `id`'s ledger booked, oldest first.
fn resize_sites(app: &App, id: u64) -> Vec<String> {
    let reply = app
        .pool
        .get(id)
        .expect("a live shell")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .resizes()
        .reply(0, None);
    reply
        .lines()
        .filter(|l| l.starts_with("resize "))
        .filter_map(|l| l.split(' ').find(|w| w.starts_with("site=")))
        .map(str::to_owned)
        .collect()
}

/// A row's record `pref` as its person left it (or never decided), with the birth `birth`.
const fn person(pref: Option<u16>, birth: DockBirth) -> DockRow {
    DockRow {
        pref,
        by_default: false,
        birth,
    }
}

/// The frame window `wid` attaches at for its own grid with no carried frame.
fn own_frame(app: &App, wid: WindowId) -> aterm_window::dpi::PhysicalSize<u32> {
    let ws = &app.windows[&wid];
    app.attach_frame_px(wid, ws.rows, ws.cols, None)
}

#[test]
fn birth_dock_is_its_table() {
    use BirthDock::{Hidden, Inside, Shown};
    // (record, the default wrote it, setting on, held, room, born).
    let rows: [(Option<u16>, bool, bool, bool, bool, BirthDock); 17] = [
        // Off: what the default decides is undocked, an undecided window and one the default
        // docked alike.
        (Some(30), true, false, false, true, Hidden),
        (Some(30), true, false, true, false, Hidden),
        (None, false, false, false, true, Hidden),
        // A person's Show is theirs, whatever the setting says, held or not.
        (Some(30), false, false, false, true, Shown(30)),
        (Some(30), false, false, true, false, Shown(30)),
        // A person's Hide is kept.
        (Some(0), false, true, false, true, Hidden),
        (Some(0), false, false, false, true, Hidden),
        // A record of a shown dock is born with it, held or not.
        (Some(30), false, true, false, true, Shown(30)),
        (Some(30), false, true, true, false, Shown(30)),
        (Some(30), true, true, false, true, Shown(30)),
        (Some(30), true, true, true, false, Shown(30)),
        // At this build's width, whatever width the row carries: none is born a screen wide.
        (Some(2000), false, true, false, true, Shown(DOCK_CELLS)),
        (Some(7), false, false, false, true, Shown(DOCK_CELLS)),
        // Never decided: born wider where it can be, else owed inside.
        (None, false, true, false, true, Shown(DOCK_CELLS)),
        (None, false, true, true, true, Inside(DOCK_CELLS)),
        (None, false, true, false, false, Inside(DOCK_CELLS)),
        (None, false, true, true, false, Inside(DOCK_CELLS)),
    ];
    for (pref, by_default, on, held, room, want) in rows {
        assert_eq!(
            birth_dock(pref, by_default, on, held, room),
            want,
            "pref={pref:?} by_default={by_default} on={on} held={held} room={room}"
        );
    }
}

#[test]
fn a_headless_window_starts_with_the_dock_hidden() {
    // The setting's default: on, except headless, a scratch world or CI; an explicit `true`
    // wins over the scratch world, nothing over headless.
    let marker = aterm_types::world::MARKER;
    let none = |_: &str| false;
    let world = |name: &str| name == marker;
    let ci = |name: &str| name == "CI";
    let table: [DefaultRow<'_>; 7] = [
        (None, false, &none, true),
        (None, false, &world, false),
        (None, false, &ci, false),
        (Some(true), false, &world, true),
        (Some(true), true, &none, false),
        (Some(false), false, &none, false),
        (None, true, &none, false),
    ];
    for (setting, headless, set, want) in table {
        assert_eq!(
            docks_by_default(setting, headless, set),
            want,
            "setting={setting:?} headless={headless}"
        );
    }

    // A headless window, with the setting on and a row that says shown, stays hidden.
    let mut app = App::headless_for_test();
    dock_setting(&mut app, Some(true));
    let birth = DockBirth {
        restored: true,
        ..DockBirth::default()
    };
    app.take_dock_record(WID, person(Some(30), birth));
    assert!(!app.seat_birth_dock(WID));
    let ws = &app.windows[&WID];
    assert_eq!(ws.dock.cells, 0);
    assert!(ws.dock.view.is_none());
    assert_eq!(
        ws.dock.pref,
        Some(30),
        "the record stays for a windowed launch"
    );
    assert_eq!(app.dock_view(WID), None);

    // CONTROL: the same app not headless is born with it.
    let mut app = App::headless_for_test();
    dock_setting(&mut app, Some(true));
    app.take_dock_record(WID, person(Some(30), birth));
    app.headless = false;
    assert!(app.seat_birth_dock(WID));
    assert_eq!(app.windows[&WID].dock.cells, 30);
    assert!(app.dock_view(WID).is_some());
}

/// Window `child` was born with a dock of its own at `parent`'s state: 30 cells, a view that
/// is not the parent's, and a frame 30 cells wider than its grid's.
fn assert_born_docked_like(app: &mut App, parent: WindowId, child: WindowId, how: &str) {
    assert_eq!(
        app.windows[&child].dock.pref,
        Some(30),
        "{how}: the child took the parent's record"
    );
    let bare = own_frame(app, child);
    assert!(app.seat_birth_dock(child), "{how}: seated");
    let ws = &app.windows[&child];
    assert_eq!(ws.dock.cells, 30, "{how}");
    let view = app.dock_view(child).expect("a dock view of its own");
    assert_ne!(Some(view), app.dock_view(parent), "{how}: its own view");
    let cw = app.win_cell_size(child).0;
    let docked = own_frame(app, child);
    assert_eq!(
        docked.width,
        bare.width + 30 * u32::try_from(cw).unwrap(),
        "{how}: born wider by the dock"
    );
    assert_eq!(docked.height, bare.height, "{how}");
    let (rows, cols) = (ws.rows, ws.cols);
    assert_eq!(app.grid_dims_for(child, docked), (rows, cols), "{how}");
}

#[test]
fn a_new_window_takes_the_dock_state_of_the_one_it_came_from() {
    let now = std::time::Instant::now();
    // A parent the person showed the dock in.
    let mut app = docking_app();
    app.windows.get_mut(&WID).unwrap().win_px = Some(own_frame(&app, WID));
    app.set_dock_at(WID, 30, now)
        .expect("the parent has room for the dock");
    assert_eq!(app.windows[&WID].dock.pref, Some(30));

    // A new window (Cmd-N's logical half).
    let session = app.next_session_id;
    let child = app.insert_logical_window(crate::stub_session(session), 24, 80);
    assert_born_docked_like(&mut app, WID, child, "new window");

    // Open Active Session in New Window, from the parent.
    app.frontmost_window = Some(WID);
    let viewed = app
        .open_active_session_in_new_window_logical()
        .expect("a view of the parent's session");
    assert_born_docked_like(&mut app, WID, viewed, "session in new window");

    // Move Tab to New Window, from the parent.
    app.frontmost_window = Some(WID);
    crate::app_workers_tests::person_tab(&mut app);
    let detached = app.detach_active_tab_logical().expect("a torn-off tab");
    assert_born_docked_like(&mut app, WID, detached, "detached tab");

    // A detach whose source is not the front window takes the source's state.
    crate::app_workers_tests::person_tab(&mut app);
    let session = app.next_session_id;
    let hidden_front = app.insert_logical_window(crate::stub_session(session), 24, 80);
    app.windows.get_mut(&hidden_front).unwrap().dock.pref = Some(0);
    app.frontmost_window = Some(hidden_front);
    let from_back = app
        .detach_active_tab_in_window_logical(WID)
        .expect("a torn-off tab of the window behind");
    assert_born_docked_like(&mut app, WID, from_back, "detached from behind the front");

    // CONTROL: a parent its person hid gives a hidden child, the setting on: the inheritance,
    // not the default, decided.
    let mut app = docking_app();
    app.windows.get_mut(&WID).unwrap().win_px = Some(own_frame(&app, WID));
    app.set_dock_at(WID, 30, now).unwrap();
    app.set_dock_at(WID, 0, now + std::time::Duration::from_secs(3600))
        .unwrap();
    assert_eq!(app.windows[&WID].dock.cells, 0, "PRECONDITION: hidden");
    assert_eq!(app.windows[&WID].dock.pref, Some(0));
    let session = app.next_session_id;
    let child = app.insert_logical_window(crate::stub_session(session), 24, 80);
    assert_eq!(app.windows[&child].dock.pref, Some(0));
    assert!(!app.seat_birth_dock(child));
    assert_eq!(app.windows[&child].dock.cells, 0);
    assert_eq!(app.dock_view(child), None);
}

#[test]
fn a_default_dock_wider_than_its_screen_is_not_born() {
    for restored in [false, true] {
        let mut app = docking_app();
        app.take_dock_record(
            WID,
            person(
                None,
                DockBirth {
                    restored,
                    ..DockBirth::default()
                },
            ),
        );
        let bare = own_frame(&app, WID);
        assert!(app.seat_birth_dock(WID));
        let view = app.windows[&WID].dock.view.expect("seated");
        let scale = app.windows[&WID].scale;
        let docked_pts = f64::from(own_frame(&app, WID).width) / scale;

        // CONTROL: a screen that holds the docked frame keeps it.
        assert!(!app.fit_birth_dock_to(WID, docked_pts, Some(docked_pts), false));
        assert_eq!(app.windows[&WID].dock.cells, 30);

        assert!(app.fit_birth_dock_to(WID, docked_pts, Some(docked_pts - 1.0), false));
        let ws = &app.windows[&WID];
        assert_eq!(ws.dock.cells, 0);
        assert!(ws.dock.view.is_none());
        assert!(app.view_store.get(view).is_none(), "its view was released");
        assert_eq!(ws.dock.pref, None, "restored={restored}: still undecided");
        assert!(
            ws.dock.owes_birth_dock(),
            "restored={restored}: owed inside"
        );
        assert_eq!(own_frame(&app, WID), bare, "the window attaches undocked");

        // A dock its record asked for is not fitted away.
        let mut app = docking_app();
        app.take_dock_record(WID, person(Some(30), DockBirth::default()));
        assert!(app.seat_birth_dock(WID));
        assert!(!app.fit_birth_dock_to(WID, 2000.0, Some(100.0), false));
        assert!(
            !app.fit_birth_dock_to(WID, 2000.0, Some(100.0), true),
            "nor placed in a tile"
        );
        assert_eq!(app.windows[&WID].dock.cells, 30);
    }
}

#[test]
fn a_docked_origin_slides_left_only_by_what_the_dock_pushed_past_the_screen() {
    let screen = (0, 2000);
    // Fits docked: not moved.
    assert_eq!(docked_origin_x(100, 1500, 300, screen), 100);
    // Fitted undocked, crosses the edge by 100 px docked: slides 100 px left.
    assert_eq!(docked_origin_x(600, 1500, 300, screen), 500);
    // Fitted exactly undocked: slides by the whole dock.
    assert_eq!(docked_origin_x(800, 1500, 300, screen), 500);
    // Already off the edge before the dock: not moved.
    assert_eq!(docked_origin_x(900, 1500, 300, screen), 900);
    // Clamped at the screen's left edge.
    assert_eq!(docked_origin_x(50, 2100, 300, screen), 0);
    // A screen right of the origin's: its own left edge and width.
    assert_eq!(docked_origin_x(2600, 1500, 300, (2000, 2000)), 2500);
}

/// Two displays side by side as macOS reports them, the left at scale 2 (1440 x 900 pt) and the
/// right at scale 1 (1920 x 1080 pt): each rectangle's px are its points times its own scale,
/// so the two px rectangles overlap from 1440 to 2880.
const MIXED_SCALE_DESK: [MonitorRectPx; 2] = [
    MonitorRectPx {
        x: 0.0,
        y: 0.0,
        w: 2880.0,
        h: 1800.0,
        scale: 2.0,
    },
    MonitorRectPx {
        x: 1440.0,
        y: 0.0,
        w: 1920.0,
        h: 1080.0,
        scale: 1.0,
    },
];

/// A windowed app whose window was born with a default dock from a row read at scale `scale`.
fn born_default_from_row_at(scale: Option<f64>) -> App {
    let mut app = docking_app();
    let birth = DockBirth {
        restored: true,
        scale,
        ..DockBirth::default()
    };
    app.take_dock_record(WID, person(None, birth));
    assert!(app.seat_birth_dock(WID), "PRECONDITION: a default dock");
    app
}

/// Where a window of scale `window_scale` lands, in points, when it is put at `place`: macOS's
/// `set_outer_position` reads a place in px at the window's scale when it is moved.
fn lands_at(place: Position, window_scale: f64) -> (f64, f64) {
    let at = place.to_logical::<f64>(window_scale);
    (at.x, at.y)
}

/// MIXED-SCALE DISPLAYS. A restored window born with a default dock is slid onto the display
/// its saved origin lies on, judged in one unit: on macOS's plane of points each display's
/// rectangle at its own scale and the origin at the scale its row read it at, whatever order
/// the displays are listed in; the slide is made in the row's px, and the window lands at the
/// point those px stand for, whatever scale it has when it is moved. CONTROL: on a plane of px
/// (Windows, X11) the px rectangles are the plane and the slide is the px slide.
#[test]
fn a_docked_window_on_displays_of_different_scale_is_slid_on_its_own_display() {
    let [left, right] = MIXED_SCALE_DESK;
    for monitors in [[left, right], [right, left]] {
        // On the right display at 2800 pt (2800 px at scale 1), inside the left's px too.
        assert_eq!(
            monitor_holding(DesktopPlane::Points, &monitors, (2800, 100), 1.0),
            Some(right),
            "{monitors:?}"
        );
        // On the left display at 1000 pt (2000 px at scale 2), inside the right's px too.
        assert_eq!(
            monitor_holding(DesktopPlane::Points, &monitors, (2000, 100), 2.0),
            Some(left),
            "{monitors:?}"
        );

        // The right display's window: a 600 pt docked frame at 2800 pt crosses its 3360 pt edge
        // by 40 pt, and undocked (300 pt less) fitted: slid 40 pt.
        let app = born_default_from_row_at(Some(1.0));
        let place = app.restored_origin_among(
            WID,
            DesktopPlane::Points,
            &monitors,
            (600, 1.0),
            (2800, 100),
            300,
        );
        assert_eq!(lands_at(place, 1.0), (2760.0, 100.0), "{monitors:?}");
        // The same row while the window stands on the scale-2 display, its frame and dock read
        // there: the same place, where it lands at that scale.
        let place = app.restored_origin_among(
            WID,
            DesktopPlane::Points,
            &monitors,
            (1200, 2.0),
            (2800, 100),
            600,
        );
        assert_eq!(
            lands_at(place, 2.0),
            (2760.0, 100.0),
            "{monitors:?}: frame read at another scale"
        );
        // The left display's window: at 1000 pt a 600 pt docked frame crosses its 1440 pt edge
        // by 160 pt: slid 160 pt, 320 of the row's px.
        let app = born_default_from_row_at(Some(2.0));
        let place = app.restored_origin_among(
            WID,
            DesktopPlane::Points,
            &monitors,
            (1200, 2.0),
            (2000, 100),
            600,
        );
        assert_eq!(lands_at(place, 2.0), (840.0, 50.0), "{monitors:?}");
    }

    // The row carries the scale its px were read at; one without it reads none.
    let mut wl = crate::restore::RestoreManifest::one_terminal_for_test(None).windows[0].clone();
    (wl.outer_x, wl.outer_y, wl.scale) = (Some(2000), Some(100), Some(2.0));
    let text = crate::restore::RestoreManifest::new(vec![wl.clone()])
        .to_toml()
        .expect("serialize");
    let back = crate::restore::RestoreManifest::from_toml(&text).expect("parse");
    assert_eq!(back.windows[0].scale, Some(2.0), "{text}");
    assert_eq!(DockBirth::of_row(&back.windows[0]).scale, Some(2.0));
    wl.scale = None;
    let text = crate::restore::RestoreManifest::new(vec![wl])
        .to_toml()
        .expect("serialize");
    assert!(!text.contains("\nscale ="), "{text}");
    let back = crate::restore::RestoreManifest::from_toml(&text).expect("parse");
    assert_eq!(DockBirth::of_row(&back.windows[0]).scale, None);

    // CONTROL: a plane of px, the right display laid at the left's px edge.
    let right_px = MonitorRectPx { x: 2880.0, ..right };
    let px_desk = [left, right_px];
    assert_eq!(
        monitor_holding(DesktopPlane::Pixels, &px_desk, (3000, 100), 1.0),
        Some(right_px)
    );
    assert_eq!(
        monitor_holding(DesktopPlane::Pixels, &px_desk, (2000, 100), 2.0),
        Some(left)
    );
    let app = born_default_from_row_at(Some(1.0));
    assert_eq!(
        app.restored_origin_among(
            WID,
            DesktopPlane::Pixels,
            &px_desk,
            (600, 1.0),
            (4300, 100),
            300
        ),
        Position::Physical(PhysicalPosition::new(4200, 100)),
        "CONTROL: the px slide"
    );
}

/// A RESTORED WINDOW LANDS ON THE DISPLAY ITS ROW WAS READ ON. macOS makes a restored window on
/// the main display, at that display's scale, and only then moves it: the place it is moved to
/// is the point its row's px stand for at the row's scale, docked or not, so on displays of
/// different scale it lands on the display it was saved on. The desk: [`MIXED_SCALE_DESK`], the
/// main display the left one at scale 2, the row read on the right one at scale 1. CONTROL: a
/// row from before the scale was written is read at the window's scale, as before; on a plane
/// of px the place is the row's px.
#[test]
fn a_restored_window_lands_where_its_row_was_read_whatever_scale_it_is_born_at() {
    let [left, right] = MIXED_SCALE_DESK;
    for monitors in [[left, right], [right, left]] {
        // Docked by default, saved at 1500 pt on the right display: the 840 pt docked frame
        // (1680 px at the window's scale 2, its dock 480) fits that display's 3360 pt edge, so
        // it is not slid, and lands on the right display, not at 750 pt on the left one.
        let app = born_default_from_row_at(Some(1.0));
        let place = app.restored_origin_among(
            WID,
            DesktopPlane::Points,
            &monitors,
            (1680, 2.0),
            (1500, 100),
            480,
        );
        let (x, _) = lands_at(place, 2.0);
        assert_eq!(lands_at(place, 2.0), (1500.0, 100.0), "{monitors:?}");
        assert!(
            (right.x..right.x + right.w).contains(&x) && x + 840.0 <= right.x + right.w,
            "{monitors:?}: on the display it was saved on, whole"
        );
    }

    // Undocked, the row read on the right display at 2800 pt (2800 px at scale 1).
    let mut app = docking_app();
    let birth = |scale| DockBirth {
        restored: true,
        origin: Some((2800, 100)),
        scale,
        ..DockBirth::default()
    };
    app.take_dock_record(WID, person(Some(0), birth(Some(1.0))));
    assert!(
        !app.seat_birth_dock(WID),
        "PRECONDITION: no dock, so no slide"
    );
    let place = app.restored_origin_among(
        WID,
        DesktopPlane::Points,
        &MIXED_SCALE_DESK,
        (1200, 2.0),
        (2800, 100),
        0,
    );
    assert_eq!(lands_at(place, 2.0), (2800.0, 100.0), "undocked");
    // CONTROL: a row with no scale is read at the window's.
    app.take_dock_record(WID, person(Some(0), birth(None)));
    let place = app.restored_origin_among(
        WID,
        DesktopPlane::Points,
        &MIXED_SCALE_DESK,
        (1200, 2.0),
        (2800, 100),
        0,
    );
    assert_eq!(lands_at(place, 2.0), (1400.0, 50.0), "CONTROL: no scale");
    // CONTROL: a plane of px places the row's px.
    app.take_dock_record(WID, person(Some(0), birth(Some(1.0))));
    assert_eq!(
        app.restored_origin_among(
            WID,
            DesktopPlane::Pixels,
            &MIXED_SCALE_DESK,
            (1200, 2.0),
            (2800, 100),
            0,
        ),
        Position::Physical(PhysicalPosition::new(2800, 100)),
        "CONTROL: a plane of px"
    );
}

/// A rectangle in points.
const fn pts(x: f64, y: f64, w: f64, h: f64) -> WorkAreaPts {
    WorkAreaPts { x, y, w, h }
}

/// THE PLACED-FRAME RULE: a frame that meets a side of its screen's visible frame and spans a
/// tile row (the whole visible height, or its top or bottom half) stands in a tile; a frame
/// that floats, however large, does not.
#[test]
fn a_frame_in_a_tile_of_its_screen_is_placed_and_a_floating_one_is_not() {
    // A 1440 x 900 pt screen less a 25 pt menu bar.
    let visible = pts(0.0, 25.0, 1440.0, 875.0);
    let rows: [(&str, WorkAreaPts, bool); 14] = [
        ("left half", pts(0.0, 25.0, 720.0, 875.0), true),
        ("right half", pts(720.0, 25.0, 720.0, 875.0), true),
        ("left half with margins", pts(8.0, 33.0, 708.0, 859.0), true),
        (
            "right half with margins",
            pts(724.0, 33.0, 708.0, 859.0),
            true,
        ),
        ("top-left quarter", pts(0.0, 25.0, 720.0, 437.5), true),
        (
            "bottom-right quarter",
            pts(720.0, 462.5, 720.0, 437.5),
            true,
        ),
        ("fill", pts(0.0, 25.0, 1440.0, 875.0), true),
        ("a third at the left", pts(0.0, 25.0, 480.0, 875.0), true),
        // CONTROL: floating.
        ("floating", pts(200.0, 150.0, 800.0, 600.0), false),
        (
            "full height, touching no side",
            pts(300.0, 25.0, 800.0, 875.0),
            false,
        ),
        (
            "at the left edge, not a row",
            pts(0.0, 100.0, 800.0, 600.0),
            false,
        ),
        (
            "in the top-left corner, not a row",
            pts(0.0, 25.0, 800.0, 600.0),
            false,
        ),
        ("one edge 17 pt off", pts(17.0, 25.0, 700.0, 875.0), false),
        ("no area", pts(0.0, 25.0, 0.0, 875.0), false),
    ];
    for (name, frame, placed) in rows {
        assert_eq!(frame_is_placed(frame, visible), placed, "{name}");
    }
    // A screen right of the main one: its own visible frame.
    let right = pts(1440.0, 0.0, 1920.0, 1055.0);
    assert!(frame_is_placed(pts(2400.0, 0.0, 960.0, 1055.0), right));
    assert!(!frame_is_placed(pts(2400.0, 0.0, 960.0, 1055.0), visible));
}

/// The birth fit reads the screen its saved origin lies on, judged on its own display: a
/// 1600 pt docked frame saved on the 1920 pt scale-1 display of [`MIXED_SCALE_DESK`] is kept,
/// though its px also lie on the 1440 pt scale-2 display, which could not hold it.
#[test]
fn the_birth_fit_reads_the_display_the_saved_origin_is_on() {
    for monitors in [MIXED_SCALE_DESK, [MIXED_SCALE_DESK[1], MIXED_SCALE_DESK[0]]] {
        let mut app = docking_app();
        let birth = DockBirth {
            restored: true,
            origin: Some((1500, 100)),
            scale: Some(1.0),
            ..DockBirth::default()
        };
        app.take_dock_record(WID, person(None, birth));
        assert!(app.seat_birth_dock(WID));
        let screens = BirthScreens {
            plane: DesktopPlane::Points,
            monitors: &monitors,
            visible: &[],
            current: None,
            scale: 1.0,
        };
        assert!(
            !app.fit_birth_dock_on(WID, &screens, 1600.0, None),
            "{monitors:?}"
        );
        assert_eq!(app.windows[&WID].dock.cells, 30);
        // CONTROL: wider than that display too.
        assert!(
            app.fit_birth_dock_on(WID, &screens, 2000.0, None),
            "{monitors:?}"
        );
        assert_eq!(app.windows[&WID].dock.cells, 0);
    }
}

/// A TILE IS JUDGED ONLY AGAINST THE SCREENS THE WINDOW SYSTEM NAMES. macOS names each screen's
/// visible frame; Windows and Linux name none, and there no saved frame is judged placed. The
/// screen the window stands on now is no stand-in: the Windows early reveal fits the dock before
/// it moves the window to its saved place and the attach fits it again after, so the two would
/// read different screens, and the window, shown docked, would be narrowed on glass at its
/// attach. The desk: two 1920 x 1080 px displays side by side, the saved frame the right half
/// of the second one's work area. CONTROL: the same frame among named visible frames stands in
/// a tile, and its dock goes inside.
#[test]
fn a_tile_is_judged_against_named_screens_never_the_one_the_window_stands_on() {
    let monitor = |x| MonitorRectPx {
        x,
        y: 0.0,
        w: 1920.0,
        h: 1080.0,
        scale: 1.0,
    };
    let monitors = [monitor(0.0), monitor(1920.0)];
    let (first, second) = (
        pts(0.0, 0.0, 1920.0, 1040.0),
        pts(1920.0, 0.0, 1920.0, 1040.0),
    );
    let born = || {
        let mut app = docking_app();
        let birth = DockBirth {
            restored: true,
            origin: Some((2880, 0)),
            scale: Some(1.0),
            ..DockBirth::default()
        };
        app.take_dock_record(WID, person(None, birth));
        assert!(app.seat_birth_dock(WID), "PRECONDITION: a default dock");
        app
    };
    // The saved half is 960 x 1040 px; docked, 1200 pt, which either screen holds.
    let saved = Some((960.0, 1040.0));
    let mut app = born();
    for (at, current) in [("the early reveal", first), ("the attach", second)] {
        let screens = BirthScreens {
            plane: DesktopPlane::Pixels,
            monitors: &monitors,
            visible: &[],
            current: Some(current),
            scale: 1.0,
        };
        assert!(
            !app.fit_birth_dock_on(WID, &screens, 1200.0, saved),
            "{at}: no screen named, no tile judged"
        );
        assert_eq!(app.windows[&WID].dock.cells, 30, "{at}: born beside it");
    }

    // CONTROL: the screens named, the half stands in a tile of the second one's.
    let mut app = born();
    let screens = BirthScreens {
        plane: DesktopPlane::Pixels,
        monitors: &monitors,
        visible: &[first, second],
        current: Some(first),
        scale: 1.0,
    };
    assert!(
        app.fit_birth_dock_on(WID, &screens, 1200.0, saved),
        "CONTROL"
    );
    assert_eq!(app.windows[&WID].dock.cells, 0, "CONTROL: owed inside");
}

#[test]
fn a_failed_attach_leaves_no_dock_view() {
    let mut app = docking_app();
    app.windows.get_mut(&WID).unwrap().win_px = Some(own_frame(&app, WID));
    crate::app_workers_tests::person_tab(&mut app);
    let detached = app.detach_active_tab_logical().expect("a torn-off tab");
    let views = app.view_store.len();
    assert!(app.seat_birth_dock(detached));
    let view = app.windows[&detached].dock.view.expect("seated");
    assert_eq!(app.view_store.len(), views + 1);
    // What `attach_os_window` does when the OS window or its surface fails.
    app.unseat_birth_dock(detached);
    app.detach_rollback_logical(Some(WID), detached);
    assert!(app.view_store.get(view).is_none());
    assert_eq!(app.view_store.len(), views);
    assert!(!app.windows.contains_key(&detached));
}

#[test]
fn a_dock_fall_is_not_paid_inside_another_entry_point() {
    let mut app = docking_app();
    app.windows.get_mut(&WID).unwrap().win_px = Some(own_frame(&app, WID));
    let session = app.focused_session_id(WID).unwrap();
    let now = std::time::Instant::now();
    app.set_dock_at(WID, 30, now).unwrap();
    app.set_dock_at(WID, 0, now).unwrap();
    assert_eq!(
        app.windows[&WID].dock.cells, 30,
        "PRECONDITION: the Hide waits"
    );
    let later = now + std::time::Duration::from_secs(3600);
    let before = resize_sites(&app, session);
    {
        let _restore = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Restore);
        assert!(!app.settle_dock_fall(later));
    }
    assert_eq!(
        app.windows[&WID].dock.cells, 30,
        "nothing paid in the scope"
    );
    assert_eq!(resize_sites(&app, session), before);
    assert!(app.settle_dock_fall(later));
    assert_eq!(app.windows[&WID].dock.cells, 0);
    let after = resize_sites(&app, session);
    assert_eq!(&after[before.len()..], ["site=dock"]);
}

/// A WINDOW LANDED OFF THE LOOP TAKES THE RECORD OF THE WINDOW IT WAS OPENED FROM. Cmd-N books
/// its shell off the loop and lands the window later; the person may focus another window in
/// between. The booking reserves its origin, and the landing gives the new window that window's
/// record, not the front's at the landing. CONTROL: an origin closed since leaves the front's.
#[test]
fn a_window_landed_off_the_loop_takes_the_record_of_the_window_it_was_opened_from() {
    for origin_closes in [false, true] {
        let mut app = docking_app();
        app.tab_spawns.unsent = true;
        app.windows.get_mut(&WID).unwrap().dock.pref = Some(30);
        let session = app.next_session_id;
        let hid = app.insert_logical_window(crate::stub_session(session), 24, 80);
        app.windows.get_mut(&hid).unwrap().dock.pref = Some(0);
        app.frontmost_window = Some(hid);
        let (id, window) = (app.next_session_id, WindowId(app.next_window_id));
        app.open_new_window();
        assert_eq!(
            (app.next_session_id, app.next_window_id),
            (id + 1, window.0 + 1),
            "PRECONDITION: Cmd-N booked a shell and reserved a window"
        );
        let booked = (id, window);
        // The person focuses the docked window while the shell spawns.
        app.frontmost_window = Some(WID);
        if origin_closes {
            app.close_window_logical(hid);
        }
        assert!(
            app.install_spawned_tab(crate::tab_spawn::TabSpawned {
                id: booked.0,
                window: booked.1,
                session: Ok(crate::stub_session(booked.0)),
                reader_gate: None,
            }),
            "PRECONDITION: the window landed"
        );
        let want = if origin_closes { Some(30) } else { Some(0) };
        assert_eq!(
            app.windows[&booked.1].dock.pref, want,
            "origin_closes={origin_closes}"
        );
    }
}

/// THE WINDOWS EARLY REVEAL PLACES A DEFAULT DOCK WITHOUT THE RENDERER. It runs before the
/// backend joins, where any metric read panics; the slide takes the dock's width the reveal
/// sized its frame by. A panic there was every Windows cold launch over a non-maximized row.
#[test]
fn the_early_reveal_places_a_default_dock_without_the_renderer() {
    let mut app = docking_app();
    app.take_dock_record(WID, person(None, DockBirth::default()));
    assert!(app.seat_birth_dock(WID), "PRECONDITION: a default dock");
    let screen = MonitorRectPx {
        x: 0.0,
        y: 0.0,
        w: 2000.0,
        h: 1200.0,
        scale: 1.0,
    };
    let ready = std::mem::replace(&mut app.backend, crate::BackendSlot::Pending(None));
    let placed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.restored_origin_among(
            WID,
            DesktopPlane::NATIVE,
            &[screen],
            (1500, 1.0),
            (600, 100),
            300,
        )
    }));
    app.backend = ready;
    assert_eq!(
        placed.ok().map(|place| lands_at(place, 1.0)),
        Some((500.0, 100.0)),
        "slid by what the dock pushed past the edge, reading no renderer metric"
    );
}

/// THE OS'S ANSWER TO A DEFAULT DOCK (Linux). A tiler sizes a window to its tile whatever it
/// asked: the docked frame is refused, and the initial-frame settle's corrections cannot change
/// that. The size it then lets through (a vetoed size's fallback, or the corrections spent) is
/// applied without the dock, so the tile grids the carried columns and no program sees a
/// resize under `site=window`; the dock is owed inside and paid as one `site=dock` resize where
/// 80 columns stay, else the window stays undocked and undecided. CONTROL: an X11 echo of the
/// asked size proves nothing, and a docked frame granted after a correction keeps the dock for
/// good, a person's narrower drag included.
#[test]
fn a_default_dock_the_os_refuses_the_wider_frame_for_goes_inside() {
    let now = std::time::Instant::now();
    for (cols, spent) in [(140, false), (140, true), (100, false)] {
        let mut app = docking_app();
        let rows = app.windows[&WID].rows;
        crate::app_workers_tests::person_tab(&mut app);
        crate::app_workers_tests::person_tab(&mut app);
        assert!(app.apply_term_resize(WID, rows, cols));
        let shells = app.window_terminal_sessions(WID);
        assert_eq!(shells.len(), 3, "PRECONDITION: three programs");
        let tile = own_frame(&app, WID);
        app.take_dock_record(WID, person(None, DockBirth::default()));
        assert!(
            app.seat_birth_dock(WID),
            "PRECONDITION: born wider by default"
        );
        let docked = own_frame(&app, WID);
        app.windows.get_mut(&WID).unwrap().win_px = Some(docked);
        app.windows.get_mut(&WID).unwrap().initial_frame_settle = Some(crate::InitialFrameSettle {
            rows,
            cols,
            deadline: now + std::time::Duration::from_secs(60),
            corrections: 3,
            corrected: false,
        });
        let before: Vec<_> = shells.iter().map(|id| resize_sites(&app, *id)).collect();
        // X11 echoes the asked frame at attach.
        app.apply_window_resized_grid(WID, docked);
        assert_eq!(app.windows[&WID].dock.cells, 30, "an echo judges nothing");
        // The tiler's tile, refused by the settle, which asks again.
        app.apply_window_resized_grid(WID, tile);
        if spent {
            for _ in 0..3 {
                app.apply_window_resized_grid(WID, tile);
            }
        } else {
            app.flush_pending_resize(WID);
        }
        let ws = &app.windows[&WID];
        assert_eq!(ws.dock.cells, 0, "cols={cols} spent={spent}: unseated");
        assert!(ws.dock.view.is_none());
        assert_eq!(ws.dock.pref, None, "still undecided");
        assert_eq!(
            (ws.rows, ws.cols),
            (rows, cols),
            "the tile grids the carried columns"
        );
        for (id, before) in shells.iter().zip(&before) {
            assert_eq!(
                &resize_sites(&app, *id)[before.len()..],
                &[] as &[String],
                "cols={cols} spent={spent}: shell {id} was resized by the refusal"
            );
        }
        app.settle_messages(now);
        let docked_inside = cols >= 110;
        assert_eq!(
            app.windows[&WID].dock.cells,
            if docked_inside { 30 } else { 0 }
        );
        for (id, before) in shells.iter().zip(&before) {
            let want: &[&str] = if docked_inside { &["site=dock"] } else { &[] };
            assert_eq!(
                &resize_sites(&app, *id)[before.len()..],
                want,
                "cols={cols} spent={spent}: shell {id}"
            );
        }
    }

    // CONTROL: granted after a correction, the dock stands for good.
    let mut app = docking_app();
    let (rows, cols) = (app.windows[&WID].rows, app.windows[&WID].cols);
    let tile = own_frame(&app, WID);
    app.take_dock_record(WID, person(None, DockBirth::default()));
    assert!(app.seat_birth_dock(WID));
    let docked = own_frame(&app, WID);
    app.windows.get_mut(&WID).unwrap().win_px = Some(docked);
    app.windows.get_mut(&WID).unwrap().initial_frame_settle = Some(crate::InitialFrameSettle {
        rows,
        cols,
        deadline: now + std::time::Duration::from_secs(60),
        corrections: 3,
        corrected: false,
    });
    app.apply_window_resized_grid(WID, tile);
    app.apply_window_resized_grid(WID, docked);
    assert!(
        !app.windows[&WID].dock.born_default,
        "CONTROL: judged for good"
    );
    app.apply_window_resized_grid(WID, tile);
    app.flush_pending_resize(WID);
    assert_eq!(
        app.windows[&WID].dock.cells, 30,
        "CONTROL: a later drag keeps it"
    );
}
