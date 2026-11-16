// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DK-4, docked by default: the birth rule, the setting's default, a new window's inheritance,
//! a headless window, a default dock wider than its screen, the origin slide and a failed attach.

use super::{
    BirthDock, DOCK_CELLS, DockBirth, MonitorRectPx, birth_dock, docked_origin_x, docks_by_default,
};
use crate::{App, WindowId};

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

/// The frame window `wid` attaches at for its own grid with no carried frame.
fn own_frame(app: &App, wid: WindowId) -> aterm_window::dpi::PhysicalSize<u32> {
    let ws = &app.windows[&wid];
    app.attach_frame_px(wid, ws.rows, ws.cols, None)
}

#[test]
fn birth_dock_is_its_table() {
    use BirthDock::{Hidden, Inside, Shown};
    let rows: [(Option<u16>, bool, bool, bool, BirthDock); 11] = [
        // Off: no window is born docked, whatever its record says.
        (Some(30), false, false, true, Hidden),
        (None, false, false, true, Hidden),
        // A person's Hide is kept.
        (Some(0), true, false, true, Hidden),
        // A record of a shown dock is born with it, held or not.
        (Some(30), true, false, true, Shown(30)),
        (Some(30), true, true, false, Shown(30)),
        // At this build's width, whatever width the row carries: none is born a screen wide.
        (Some(2000), true, false, true, Shown(DOCK_CELLS)),
        (Some(7), true, false, true, Shown(DOCK_CELLS)),
        // Never decided: born wider where it can be, else owed inside.
        (None, true, false, true, Shown(DOCK_CELLS)),
        (None, true, true, true, Inside(DOCK_CELLS)),
        (None, true, false, false, Inside(DOCK_CELLS)),
        (None, true, true, false, Inside(DOCK_CELLS)),
    ];
    for (pref, on, held, room, want) in rows {
        assert_eq!(
            birth_dock(pref, on, held, room),
            want,
            "pref={pref:?} on={on} held={held} room={room}"
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
    app.take_dock_record(WID, Some(30), birth);
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
    app.take_dock_record(WID, Some(30), birth);
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
            None,
            DockBirth {
                restored,
                ..DockBirth::default()
            },
        );
        let bare = own_frame(&app, WID);
        assert!(app.seat_birth_dock(WID));
        let view = app.windows[&WID].dock.view.expect("seated");
        let scale = app.windows[&WID].scale;
        let docked_pts = f64::from(own_frame(&app, WID).width) / scale;

        // CONTROL: a screen that holds the docked frame keeps it.
        assert!(!app.fit_birth_dock_to(WID, docked_pts, Some(docked_pts)));
        assert_eq!(app.windows[&WID].dock.cells, 30);

        assert!(app.fit_birth_dock_to(WID, docked_pts, Some(docked_pts - 1.0)));
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
        app.take_dock_record(WID, Some(30), DockBirth::default());
        assert!(app.seat_birth_dock(WID));
        assert!(!app.fit_birth_dock_to(WID, 2000.0, Some(100.0)));
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
    app.take_dock_record(WID, None, DockBirth::default());
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
        app.restored_origin_among(WID, &[screen], 1500, (600, 100), 300)
    }));
    app.backend = ready;
    assert_eq!(
        placed.ok(),
        Some((500, 100)),
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
        app.take_dock_record(WID, None, DockBirth::default());
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
    app.take_dock_record(WID, None, DockBirth::default());
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
