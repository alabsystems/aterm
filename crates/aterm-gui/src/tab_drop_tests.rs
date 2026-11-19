// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_desktop_drop_and_a_return_before_a_tab_keep_the_same_live_view() {
    let mut app = App::headless_for_test();
    let home = app.tab_home_window().unwrap();
    let before = app.windows[&home].tab_set.active().unwrap().id;
    let session = app.next_session_id;
    app.push_stub_tab(home, crate::stub_session(session));
    let tab = app.windows[&home].tab_set.active().unwrap().clone();
    assert!(app.drop_tab_outside(home, tab.id));
    let aux = app.frontmost_window.unwrap();
    assert_ne!(home, aux);
    assert_eq!(app.windows[&aux].tab_set.shown_len(), 1);
    let stale = TabId::from_stored(u64::MAX);
    assert!(!app.drop_tab_on_strip(aux, tab.id, home, Some(stale)));
    assert!(!app.drop_tab_on_strip(home, before, aux, None));
    assert!(app.drop_tab_on_strip(aux, tab.id, home, Some(before)));
    assert_eq!(app.windows.len(), 1);
    assert_eq!(app.windows[&home].tab_set.shown()[0].id, tab.id);
    assert_eq!(
        app.windows[&home].tab_set.active().unwrap().focus,
        tab.focus
    );
    assert_eq!(app.focused_session_id(home), Some(session));
    assert!(app.structural_invariants_ok());
}

#[test]
fn older_auxiliary_layouts_keep_their_selected_session_and_collect_the_rest_in_order() {
    let mut app = App::headless_for_test();
    let home = app.tab_home_window().unwrap();
    let home_tab = app.windows[&home].tab_set.active_id().unwrap();
    let first = app.next_session_id;
    let aux = app.insert_logical_window(crate::stub_session(first), 24, 80);
    let first_tab = app.windows[&aux].tab_set.active_id().unwrap();
    let second = app.next_session_id;
    app.push_stub_tab(aux, crate::stub_session(second));
    let keep = app.windows[&aux].tab_set.active_id().unwrap();
    let third = app.next_session_id;
    app.push_stub_tab(aux, crate::stub_session(third));
    let third_tab = app.windows[&aux].tab_set.active_id().unwrap();
    app.switch_tab_in(aux, 1);
    app.collect_restored_session_tabs();
    app.collect_restored_session_tabs();
    assert_eq!(app.frontmost_window, Some(aux));
    assert_eq!(app.windows[&aux].tab_set.active_id(), Some(keep));
    assert_eq!(app.windows[&aux].tab_set.shown_len(), 1);
    assert_eq!(app.windows[&home].tab_set.active_id(), Some(home_tab));
    let ids: Vec<_> = app.windows[&home]
        .tab_set
        .shown()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids, [home_tab, first_tab, third_tab]);
    assert!(app.structural_invariants_ok());
}

#[test]
fn compositor_surface_drop_uses_the_pressed_id_and_destination_coordinates() {
    let mut app = App::headless_for_test();
    let home = app.tab_home_window().unwrap();
    let session = app.next_session_id;
    let aux = app.insert_logical_window(crate::stub_session(session), 24, 80);
    let tab = app.windows[&aux].tab_set.active_id().unwrap();
    app.tab_strip_rows = 1;
    app.config.tab_strip_rows = Some(1);
    app.splice_tab_strip_with(home, 1);
    let (ox, oy) = app.frame_origin(home);
    let (cw, ch) = app.win_cell_size(home);
    let x = ox as f64 + (app.win_pad(home) + cw / 2) as f64;
    let y = oy as f64 + (app.win_head(home) + app.win_pad_top(home) + ch / 2) as f64;
    assert!(app.strip_col_at(home, x, y).is_some());
    app.finish_surface_drop(aux, [STRIP_TAB, u64::MAX], Some((home, x, y)));
    assert!(app.windows.contains_key(&aux));
    app.finish_surface_drop(aux, [STRIP_TAB, tab.get()], Some((home, x, y)));
    assert_eq!(app.windows[&home].tab_set.active_id(), Some(tab));
    assert!(!app.windows.contains_key(&aux));
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_main_tab_dropped_over_an_auxiliary_stays_put_and_a_desktop_drop_detaches() {
    let mut app = App::headless_for_test();
    let home = app.tab_home_window().unwrap();
    let session = app.next_session_id;
    app.push_stub_tab(home, crate::stub_session(session));
    let tab = app.windows[&home].tab_set.active_id().unwrap();
    let aux = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    app.tab_strip_rows = 1;
    app.config.tab_strip_rows = Some(1);
    app.splice_tab_strip_with(aux, 1);
    let (ox, oy) = app.frame_origin(aux);
    let (cw, ch) = app.win_cell_size(aux);
    let x = ox as f64 + (app.win_pad(aux) + cw / 2) as f64;
    let y = oy as f64 + (app.win_head(aux) + app.win_pad_top(aux) + ch / 2) as f64;
    assert!(app.strip_col_at(aux, x, y).is_some());
    app.finish_surface_drop(home, [STRIP_TAB, tab.get()], Some((aux, x, y)));
    assert_eq!(app.windows.len(), 2);
    assert!(app.windows[&home].tab_set.get(tab).is_some());
    app.finish_surface_drop(home, [STRIP_TAB, tab.get()], None);
    assert_eq!(app.windows.len(), 3);
    assert!(app.windows[&home].tab_set.get(tab).is_none());
    assert_eq!(
        app.windows[&app.frontmost_window.unwrap()]
            .tab_set
            .active_id(),
        Some(tab)
    );
    assert!(app.structural_invariants_ok());
}
