// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn opening_pops_the_composed_model() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let opened_from = crate::turn_ledger::now_ms();
    assert!(
        app.open_tab_context_menu(wid, 0, 5, false),
        "a terminal tab always has a menu"
    );
    let opened_by = crate::turn_ledger::now_ms();
    let menu = app.windows[&wid].tab_menu.as_ref().expect("menu is open");
    assert_eq!(menu.anchor_col, 5, "anchored where the press landed");
    assert_eq!(menu.highlight, None, "a mouse open lights no row");
    let titles = app.tab_titles(wid);
    let composed = app.tab_chrome_ext(wid, &titles);
    let card = &app.windows[&wid].tab_menu.as_ref().unwrap().entries;
    assert!(
        [opened_from, opened_by]
            .iter()
            .any(|&at| *card == crate::session_chrome::menu_shown_at(&composed[0].menu, at)),
        "the card renders the one composed model, never a parallel list \
         (its action bits: `composed_chrome_reaches_presentation_and_menu_actions_are_honest`):\
         \n{card:?}\n{:?}",
        composed[0].menu
    );
    assert!(app.close_tab_menu(wid), "close reports it dismissed one");
    assert!(!app.close_tab_menu(wid), "…and is idempotent");
    assert!(app.windows[&wid].tab_menu_rect.is_none());
}

#[test]
fn a_keyboard_open_lights_the_first_enabled_action() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_active_tab_context_menu(wid));
    let menu = app.windows[&wid].tab_menu.as_ref().expect("open");
    let lit = menu.highlight.expect("⇧F10 enters with a row selected");
    assert!(
        crate::tab_menu::is_selectable(&menu.entries[lit]),
        "and it is an ENABLED action, never a header or a greyed row"
    );
    assert!(menu.keyboard);
}

#[test]
fn no_strip_means_no_menu() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 0;
    assert!(!app.open_active_tab_context_menu(wid));
    assert!(!app.open_tab_context_menu(wid, 0, 0, false));
    assert!(app.windows[&wid].tab_menu.is_none());
}

#[test]
fn right_clicking_a_background_tab_selects_it_first() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    assert!(
        app.windows[&wid].tab_set.len() >= 2,
        "two tabs to switch between"
    );
    let active = app.windows[&wid].tab_set.active_index().unwrap();
    let other = usize::from(active == 0);
    assert!(app.open_tab_context_menu(wid, other, 0, false));
    assert_eq!(
        app.windows[&wid].tab_set.active_index(),
        Some(other),
        "the menu's subject became the front tab"
    );
    let menu = app.windows[&wid].tab_menu.as_ref().expect("open");
    assert_eq!(
        menu.tab,
        app.windows[&wid].tab_set.tabs()[other].id,
        "…and the card captured that tab's STABLE id, not its slot"
    );
}
