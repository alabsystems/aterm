// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DK-4, docked by default, across a restore and an update: a window comes back with its dock
//! at the grid it had, and no program is resized but by the one booked `site=dock` commit a
//! window that cannot be born wider pays.

use super::tests::{
    adopt_as, bare_leaf, grid_of, handed_off_shell, handoff_into_moved_window, owe_restore_regrid,
    present_scripted, resize_sites, window_of,
};
use crate::app_window::DockBirth;
use crate::{App, WindowId, restore};

const WID: WindowId = WindowId(0);

/// A birth case: the row's dock field, the setting, the cells the window is born with, and the
/// record it keeps.
type BirthCase = (Option<u16>, Option<bool>, u16, Option<u16>);

/// `[navigator] dock` set to `on`, so the process environment (a scratch world, CI) cannot
/// decide it.
fn dock_setting(app: &mut App, on: Option<bool>) {
    app.config.navigator = Some(crate::app_config::NavigatorConfig { dock: on });
}

/// A windowed app's birth seat of window `wid`: the setting on and not headless.
fn seat_windowed(app: &mut App, wid: WindowId) -> bool {
    dock_setting(app, Some(true));
    app.headless = false;
    app.seat_birth_dock(wid)
}

/// A layout row restored with no dock record, held (maximized) or not, at `cols` columns.
fn row(dock: Option<u16>, held: bool, cols: u16) -> restore::WindowLayout {
    restore::WindowLayout {
        cols,
        maximized: held.then_some(true),
        dock,
        ..window_of(vec![restore::RestoredSplitTree::leaf(
            restore::RestoredView::Terminal(bare_leaf()),
        )])
    }
}

/// A successor whose bootstrap window and its three shells stand at one grid: shell 0 adopted,
/// shells 1 and 2 handed over and placed by the restore.
fn handed_over_desk() -> (App, Vec<u64>) {
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    adopt_as(&mut new, 0, 0);
    let leaf = |n| {
        restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                local_id: Some(n),
                ..bare_leaf()
            },
        ))
    };
    new.seamless_adopt = vec![handed_off_shell(1), handed_off_shell(2)];
    new.restore_into_window(WID, window_of(vec![leaf(0), leaf(1), leaf(2)]));
    assert!(
        new.seamless_adopt.is_empty(),
        "PRECONDITION: the layout placed both handed-over shells"
    );
    let shells = shells_of(&new, WID);
    assert_eq!(shells.len(), 3, "PRECONDITION: three shells");
    let grid = (new.windows[&WID].rows, new.windows[&WID].cols);
    for id in &shells {
        assert_eq!(
            grid_of(&new, *id),
            grid,
            "PRECONDITION: shell {id} stands at its window's grid"
        );
    }
    (new, shells)
}

/// The session of every tab of window `wid`, in tab order.
fn shells_of(app: &App, wid: WindowId) -> Vec<u64> {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .filter_map(|tab| {
            app.view_store
                .get(tab.focus)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
        })
        .collect()
}

/// A window `cols` wide holding three programs at its grid, its own as booked so far.
fn three_programs_at(cols: u16) -> (App, Vec<u64>, Vec<Vec<String>>) {
    let mut app = App::headless_for_test();
    let rows = app.windows[&WID].rows;
    crate::app_workers_tests::person_tab(&mut app);
    crate::app_workers_tests::person_tab(&mut app);
    assert!(app.apply_term_resize(WID, rows, cols));
    let shells = shells_of(&app, WID);
    assert_eq!(shells.len(), 3, "PRECONDITION: three programs");
    for id in &shells {
        assert_eq!(grid_of(&app, *id), (rows, cols), "PRECONDITION: shell {id}");
    }
    let before = shells.iter().map(|id| resize_sites(&app, *id)).collect();
    (app, shells, before)
}

/// What each of `shells` booked since `before`.
fn booked_since(app: &App, shells: &[u64], before: &[Vec<String>]) -> Vec<Vec<String>> {
    shells
        .iter()
        .zip(before)
        .map(|(id, before)| resize_sites(app, *id)[before.len()..].to_vec())
        .collect()
}

fn close_pass(app: &mut App) {
    app.close_restore_pass(&[], &crate::task_launch::RestoredTasks::default());
}

#[test]
fn an_upgrade_docks_the_navigator_and_resizes_no_handed_off_shell() {
    // `grew`: the window took the frame its attach asked for. CONTROL (`!grew`): it did not, so
    // the dock's 30 cells come out of the grid and the ledger read sees every resize.
    for grew in [true, false] {
        let (mut new, shells) = handed_over_desk();
        let (r, c) = (new.windows[&WID].rows, new.windows[&WID].cols);
        let wl = row(None, false, c);
        new.take_dock_record(WID, wl.dock, DockBirth::of_row(&wl));
        // The carried frame: the predecessor's window, which held no dock.
        let old = new.window_frame_px_for(WID, r, c);
        assert!(seat_windowed(&mut new, WID), "grew={grew}: seated");
        let ws = &new.windows[&WID];
        assert_eq!(ws.dock.cells, 30, "grew={grew}");
        assert_eq!(
            ws.dock.pref,
            Some(30),
            "a default docking writes the record"
        );
        assert!(new.dock_view(WID).is_some());
        let size = new.attach_frame_px(WID, r, c, Some(old));
        let cw = u32::try_from(new.win_cell_size(WID).0).unwrap();
        assert_eq!(size.width, old.width + 30 * cw, "born wider by the dock");
        assert_eq!(size.height, old.height);
        assert_eq!(
            new.grid_dims_for(WID, size),
            (r, c),
            "at the grid it carried"
        );

        new.on_resize(WID, if grew { size } else { old });
        close_pass(&mut new);
        new.settle_messages(std::time::Instant::now());
        for id in &shells {
            if grew {
                assert_eq!(grid_of(&new, *id), (r, c), "shell {id}");
                assert!(
                    resize_sites(&new, *id).is_empty(),
                    "shell {id} was resized: {:?}",
                    resize_sites(&new, *id)
                );
            } else {
                assert_eq!(grid_of(&new, *id), (r, c - 30), "CONTROL: shell {id}");
                assert!(
                    !resize_sites(&new, *id).is_empty(),
                    "CONTROL: the ledger read sees shell {id}'s resize"
                );
            }
        }
    }
}

#[test]
fn a_manifest_with_no_dock_field_restores_docked() {
    // (a) The record on the wire: a row with no `dock` is undecided, and an undecided row
    // writes none; hidden and shown round-trip.
    let text = restore::RestoreManifest::new(vec![row(None, false, 80)])
        .to_toml()
        .expect("serialize");
    assert!(
        !text.contains("dock"),
        "an undecided row writes no dock: {text}"
    );
    let back = restore::RestoreManifest::from_toml(&text).expect("parse");
    assert_eq!(back.windows[0].dock, None);
    for dock in [Some(0), Some(30)] {
        let text = restore::RestoreManifest::new(vec![row(dock, false, 80)])
            .to_toml()
            .expect("serialize");
        let back = restore::RestoreManifest::from_toml(&text).expect("parse");
        assert_eq!(back.windows[0].dock, dock, "{text}");
    }

    // Each birth site, with a row that says `dock`, under the setting `on`: the cells the
    // window is born with and the record it then holds.
    type Site = fn(&mut App, &restore::WindowLayout) -> WindowId;
    let sites: [(&str, Site); 4] = [
        ("native restore window", |app, wl| {
            let wid = app.create_native_restore_window(wl.rows, wl.cols);
            app.take_dock_record(wid, wl.dock, DockBirth::of_row(wl));
            wid
        }),
        ("native sliced fill", |app, wl| {
            let mut filling = app
                .begin_native_sliced_fill(wl.clone())
                .expect("a native fill");
            let host = filling.pending_host.take().expect("a pending host");
            assert_eq!(host.dock.pref, wl.dock, "the host took the row's record");
            assert!(host.dock.birth.restored);
            app.windows.insert(filling.wid, host);
            filling.wid
        }),
        // `create_window_internal_connected`'s logical half (its shell spawn needs an event
        // loop): the window `create_window_logical` installs, then the row's record.
        ("shell restore window", |app, wl| {
            let session = app.next_session_id;
            let wid = app.insert_logical_window(crate::stub_session(session), wl.rows, wl.cols);
            app.take_dock_record(wid, wl.dock, DockBirth::of_row(wl));
            wid
        }),
        ("bootstrap window", |app, wl| {
            let manifest = restore::RestoreManifest::new(vec![wl.clone()]);
            let (dock, birth) = restore::bootstrap_dock(Some(&manifest), None);
            assert!(birth.restored);
            app.take_dock_record(WID, dock, birth);
            WID
        }),
    ];
    let cases: [BirthCase; 4] = [
        // No field, the setting on: born docked, and the record now says so.
        (None, Some(true), 30, Some(30)),
        (Some(30), Some(true), 30, Some(30)),
        // CONTROL: a Hide stays hidden.
        (Some(0), Some(true), 0, Some(0)),
        // The setting off over a shown row: hidden, the record undecided again, so it is
        // written beside the undocked grid and frame as what it is.
        (Some(30), Some(false), 0, None),
    ];
    for (name, site) in sites {
        for (dock, on, cells, pref) in cases {
            let mut app = App::headless_for_test();
            dock_setting(&mut app, on);
            app.headless = false;
            let wid = site(&mut app, &row(dock, false, 80));
            app.seat_birth_dock(wid);
            let ws = &app.windows[&wid];
            assert_eq!(ws.dock.cells, cells, "{name}: dock={dock:?} on={on:?}");
            assert_eq!(ws.dock.pref, pref, "{name}: dock={dock:?} on={on:?}");
            assert_eq!(
                app.dock_view(wid).is_some(),
                cells != 0,
                "{name}: dock={dock:?} on={on:?}"
            );
        }
    }
}

#[test]
fn a_held_window_restored_with_a_dock_books_one_dock_resize_and_none_under_restore() {
    // Maximized, then full screen: born without the dock, which goes inside once the restore
    // settles, booked once under `site=dock`.
    for show_fullscreen in [false, true] {
        let (mut app, shells, before) = three_programs_at(140);
        let mut wl = row(None, !show_fullscreen, 140);
        wl.show.fullscreen = show_fullscreen.then_some(true);
        app.take_dock_record(WID, wl.dock, DockBirth::of_row(&wl));
        assert!(!seat_windowed(&mut app, WID), "held: not born wider");
        assert_eq!(app.windows[&WID].dock.cells, 0);
        assert!(app.windows[&WID].dock.owes_birth_dock());
        let now = std::time::Instant::now();
        app.settle_messages(now);
        assert!(
            booked_since(&app, &shells, &before)
                .iter()
                .all(Vec::is_empty),
            "fullscreen={show_fullscreen}: nothing before the restore closes"
        );
        close_pass(&mut app);
        app.settle_messages(now);
        let rows = app.windows[&WID].rows;
        for id in &shells {
            assert_eq!(grid_of(&app, *id), (rows, 110), "shell {id}");
        }
        assert_eq!(
            booked_since(&app, &shells, &before),
            vec![vec!["site=dock".to_owned()]; 3],
            "fullscreen={show_fullscreen}"
        );
        app.settle_messages(now);
        assert_eq!(
            booked_since(&app, &shells, &before),
            vec![vec!["site=dock".to_owned()]; 3],
            "a second park adds nothing"
        );
        assert!(!app.windows[&WID].dock.owes_birth_dock());
        assert_eq!(app.capture_restore_manifest().windows[0].dock, Some(30));
    }

    // The restore's re-grid owed to a size still to land: nothing until a frame pays it, then
    // one park books the one dock row.
    {
        let (mut app, shells, before) = three_programs_at(140);
        let wl = row(None, true, 140);
        app.take_dock_record(WID, wl.dock, DockBirth::of_row(&wl));
        seat_windowed(&mut app, WID);
        let t0 = std::time::Instant::now();
        owe_restore_regrid(&mut app, WID, false, t0);
        app.settle_messages(t0);
        assert!(
            booked_since(&app, &shells, &before)
                .iter()
                .all(Vec::is_empty)
        );
        assert_eq!(app.windows[&WID].dock.cells, 0, "owed: not paid yet");
        present_scripted(&mut app, WID);
        app.redraw_window_at(WID, t0);
        assert!(app.windows[&WID].restore_regrid_owed.is_none());
        assert_eq!(app.windows[&WID].dock.cells, 0, "the frame pays no dock");
        app.settle_messages(t0);
        assert_eq!(app.windows[&WID].dock.cells, 30);
        assert_eq!(
            booked_since(&app, &shells, &before),
            vec![vec!["site=dock".to_owned()]; 3]
        );
    }

    // A handoff whose shells were parked at another grid: the restore re-grids them, then the
    // dock goes inside, in that order and once each.
    {
        let (mut app, handed) = handoff_into_moved_window((27, 140));
        let wl = row(None, true, 140);
        app.take_dock_record(WID, wl.dock, DockBirth::of_row(&wl));
        seat_windowed(&mut app, WID);
        close_pass(&mut app);
        app.settle_messages(std::time::Instant::now());
        for id in &handed {
            assert_eq!(grid_of(&app, *id), (27, 110), "shell {id}");
            assert_eq!(resize_sites(&app, *id), ["site=restore", "site=dock"]);
        }
    }

    // CONTROL: the same pay made inside the restore's entry point, the scope guard passed by,
    // books the dock's resize under `site=restore`: the pay sits outside every scope for this.
    {
        let (mut app, shells, before) = three_programs_at(140);
        let wl = row(None, true, 140);
        app.take_dock_record(WID, wl.dock, DockBirth::of_row(&wl));
        seat_windowed(&mut app, WID);
        close_pass(&mut app);
        {
            let _restore =
                crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Restore);
            app.settle_messages(std::time::Instant::now());
            assert_eq!(
                app.windows[&WID].dock.cells, 0,
                "CONTROL: the guard holds it"
            );
            app.settle_dock_fall_unscoped(std::time::Instant::now());
        }
        assert_eq!(
            booked_since(&app, &shells, &before),
            vec![vec!["site=restore".to_owned()]; 3],
            "CONTROL"
        );
    }
}

#[test]
fn a_window_that_cannot_keep_eighty_columns_starts_without_the_dock() {
    // 109 columns: no room for 80 beside the column, so it starts without it and stays
    // undecided.
    let (mut app, shells, before) = three_programs_at(109);
    let wl = row(None, true, 109);
    app.take_dock_record(WID, wl.dock, DockBirth::of_row(&wl));
    seat_windowed(&mut app, WID);
    close_pass(&mut app);
    app.settle_messages(std::time::Instant::now());
    let ws = &app.windows[&WID];
    assert_eq!(ws.dock.cells, 0);
    assert!(app.dock_view(WID).is_none());
    assert!(ws.dock.view.is_none(), "no view was made");
    assert!(!ws.dock.owes_birth_dock());
    assert_eq!(ws.dock.pref, None, "still undecided");
    assert!(
        booked_since(&app, &shells, &before)
            .iter()
            .all(Vec::is_empty)
    );
    let captured = app.capture_restore_manifest();
    assert_eq!(captured.windows[0].dock, None);
    assert!(
        !captured.to_toml().expect("serialize").contains("dock"),
        "the captured row has no dock key"
    );

    // View > Show Navigator still works there, within the explicit-Show floor.
    let now = std::time::Instant::now();
    app.set_dock_at(WID, 30, now)
        .expect("a person's Show fits the least window");
    let rows = app.windows[&WID].rows;
    for id in &shells {
        assert_eq!(grid_of(&app, *id), (rows, 79), "shell {id}");
    }
    assert_eq!(
        booked_since(&app, &shells, &before),
        vec![vec!["site=dock".to_owned()]; 3]
    );
    assert_eq!(app.windows[&WID].dock.pref, Some(30));

    // CONTROL: 110 columns keeps 80, and docks inside with exactly one dock resize.
    let (mut app, shells, before) = three_programs_at(110);
    let wl = row(None, true, 110);
    app.take_dock_record(WID, wl.dock, DockBirth::of_row(&wl));
    seat_windowed(&mut app, WID);
    close_pass(&mut app);
    app.settle_messages(std::time::Instant::now());
    assert_eq!(app.windows[&WID].dock.cells, 30, "CONTROL");
    let rows = app.windows[&WID].rows;
    for id in &shells {
        assert_eq!(grid_of(&app, *id), (rows, 80), "CONTROL: shell {id}");
    }
    assert_eq!(
        booked_since(&app, &shells, &before),
        vec![vec!["site=dock".to_owned()]; 3],
        "CONTROL"
    );
}

#[test]
fn a_window_saved_with_the_dock_shown_restores_at_the_frame_it_had() {
    // Review findings F5 and F24: the grid a capture writes excludes the dock and its frame
    // includes it, so the row must say the dock was shown.
    let mut old = App::headless_for_test();
    let rows = old.windows[&WID].rows;
    assert!(old.apply_term_resize(WID, rows, 120));
    let frame = old.window_frame_px_for(WID, rows, 120);
    old.windows.get_mut(&WID).unwrap().win_px = Some(frame);
    old.set_dock_at(WID, 30, std::time::Instant::now())
        .expect("room for the dock");
    assert_eq!(
        old.windows[&WID].cols, 90,
        "PRECONDITION: the dock went inside"
    );
    for handoff in [false, true] {
        let manifest = old.capture_manifest(handoff, false);
        let saved = manifest.windows[0].clone();
        assert_eq!(saved.dock, Some(30), "handoff={handoff}");
        assert_eq!((saved.rows, saved.cols), (rows, 90));
        let carried = saved
            .inner_px
            .map(|[w, h]| aterm_window::dpi::PhysicalSize::new(w, h));
        assert_eq!(carried, Some(frame), "handoff={handoff}: the whole window");

        for on in [Some(true), Some(false)] {
            let mut new = App::headless_for_test();
            new.take_dock_record(WID, saved.dock, DockBirth::of_row(&saved));
            dock_setting(&mut new, on);
            new.headless = false;
            new.seat_birth_dock(WID);
            let size = new.attach_frame_px(WID, saved.rows, saved.cols, carried);
            if on == Some(false) {
                // CONTROL: born without the dock, the carried frame is refused: the window
                // comes back 30 cells narrower, its grid kept.
                assert!(size.width < frame.width, "CONTROL: handoff={handoff}");
            } else {
                assert_eq!(size, frame, "handoff={handoff} on={on:?}: the frame it had");
                assert_eq!(new.grid_dims_for(WID, size), (rows, 90));
            }
        }
    }
}

/// A WRITTEN RECORD AGREES WITH THE GRID AND FRAME BESIDE IT. A window the setting kept
/// undocked over a row that said shown lives undocked: its row must not say shown, or a birth
/// after the setting comes back on is a record's (no screen fit, no slide, a held window born
/// wider than its screen and re-gridded under `site=restore`) rather than the default's. Round
/// trip: captured with the setting off, born with it on, a free row is a default birth (fitted
/// and slid) and a held row owes the dock inside, paid as one `site=dock` resize. A record the
/// window does not stand with for any reason (its view failed) is written undecided; a Hide
/// still waiting is written shown, as its grid still is.
#[test]
fn a_window_the_setting_kept_undocked_writes_no_shown_record() {
    for held in [false, true] {
        let (mut app, _, _) = three_programs_at(140);
        let wl = row(Some(30), held, 140);
        app.take_dock_record(WID, wl.dock, DockBirth::of_row(&wl));
        dock_setting(&mut app, Some(false));
        app.headless = false;
        assert!(!app.seat_birth_dock(WID), "held={held}: the setting hid it");
        assert_eq!(app.windows[&WID].dock.cells, 0);
        let frame = {
            let ws = &app.windows[&WID];
            app.window_frame_px_for(WID, ws.rows, ws.cols)
        };
        app.windows.get_mut(&WID).unwrap().win_px = Some(frame);
        let saved = app.capture_manifest(false, false).windows[0].clone();
        assert_eq!(
            saved.dock, None,
            "held={held}: no shown record beside an undocked grid"
        );

        // The setting back on: the default's birth, not a record's.
        let (mut new, shells_new, before_new) = three_programs_at(saved.cols);
        let mut saved = saved;
        saved.maximized = held.then_some(true);
        new.take_dock_record(WID, saved.dock, DockBirth::of_row(&saved));
        let seated = seat_windowed(&mut new, WID);
        if held {
            assert!(!seated, "a held row is not born wider than its screen");
            assert!(new.windows[&WID].dock.owes_birth_dock());
            close_pass(&mut new);
            new.settle_messages(std::time::Instant::now());
            assert_eq!(new.windows[&WID].dock.cells, 30);
            assert_eq!(
                booked_since(&new, &shells_new, &before_new),
                vec![vec!["site=dock".to_owned()]; 3],
                "one dock resize per program, none under site=restore"
            );
        } else {
            assert!(seated);
            assert!(
                new.windows[&WID].dock.born_default,
                "a default birth: fitted to its screen and slid"
            );
        }
    }

    // A shown record the window does not stand with (its view could not be made) is written
    // undecided.
    let mut app = App::headless_for_test();
    app.headless = false;
    let frame = {
        let ws = &app.windows[&WID];
        app.window_frame_px_for(WID, ws.rows, ws.cols)
    };
    app.windows.get_mut(&WID).unwrap().win_px = Some(frame);
    app.windows.get_mut(&WID).unwrap().dock.pref = Some(30);
    assert_eq!(app.capture_manifest(false, false).windows[0].dock, None);

    // CONTROL: a Hide waiting for the regrow floor still stands at 30 cells, and is written so.
    let now = std::time::Instant::now();
    app.set_dock_at(WID, 30, now).expect("room for the dock");
    app.set_dock_at(WID, 0, now)
        .expect("a hide is never refused");
    assert_eq!(
        app.windows[&WID].dock.cells, 30,
        "PRECONDITION: the Hide waits"
    );
    for handoff in [false, true] {
        assert_eq!(
            app.capture_manifest(handoff, false).windows[0].dock,
            Some(30),
            "CONTROL: handoff={handoff}"
        );
    }
}

#[test]
fn a_hide_is_carried_by_a_quit_layout_and_a_handoff_layout() {
    let mut app = App::headless_for_test();
    let frame = {
        let ws = &app.windows[&WID];
        app.window_frame_px_for(WID, ws.rows, ws.cols)
    };
    app.windows.get_mut(&WID).unwrap().win_px = Some(frame);
    let now = std::time::Instant::now();
    assert_eq!(app.capture_manifest(false, false).windows[0].dock, None);
    app.set_dock_at(WID, 30, now).unwrap();
    app.set_dock_at(WID, 0, now + std::time::Duration::from_secs(3600))
        .unwrap();
    assert_eq!(app.windows[&WID].dock.cells, 0, "PRECONDITION: hidden");
    for handoff in [false, true] {
        assert_eq!(
            app.capture_manifest(handoff, false).windows[0].dock,
            Some(0),
            "handoff={handoff}"
        );
    }
}
