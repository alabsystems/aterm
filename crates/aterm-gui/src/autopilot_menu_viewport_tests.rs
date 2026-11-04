// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual popup input and painting on a roster larger than the frame.

use super::*;
use crate::claude_footer::probe;
use crate::fact_menu::{AccountMenuRow, SessionWeek, account_menu_rows_from};
use aterm_agent::harness::accounts::{Roster, Row, State};
use aterm_types::keyboard::{Key, KeyEventType, NamedKey};
use aterm_window::event::MouseScrollDelta;
use std::collections::BTreeSet;
use std::time::Duration;

fn accounts(disabled: usize) -> (Vec<AccountMenuRow>, bool) {
    let roster = Roster {
        rows: (0..64)
            .map(|n| Row {
                label: format!("pool-{n:02}"),
                dir: Some(format!("/synthetic/pool-{n:02}")),
                ..Row::default()
            })
            .collect(),
    };
    let enrolled = roster.rows.iter().map(|row| row.label.clone()).collect();
    let mut state = State::default();
    for row in roster.rows.iter().take(disabled) {
        aterm_agent::harness::accounts::note_auth(
            &mut state,
            &row.label,
            aterm_agent::harness::accounts::Auth::SignedOut,
            1_793_000_000,
        );
    }
    account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        false,
        None,
        None,
        SessionWeek::default(),
        1_793_000_000,
    )
}

fn fixture(cols: u16, disabled: usize) -> (App, WindowId, u64) {
    let mut facts = probe::facts();
    facts.subscription = Some(aterm_agent::harness::profile::SubscriptionDisplay {
        email: Some("pool-00@example.test".into()),
        weekly: Some("weekly 42% left".into()),
        label: Some("pool-00".into()),
        ..Default::default()
    });
    let (mut app, wid, session) = probe::glass_with(&probe::AT_REST, facts);
    assert!(app.proxy.is_none(), "no live account reader");
    app.config.serious_mode = Some(true);
    app.config.cursor_trail = Some(false);
    app.config.cursor_trail_style = Some("off".into());
    app.recompute_sparkle();
    app.pool
        .get(session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .set_agent_reader_program_for_test(Some(aterm_phase::Program::Claude));
    assert!(app.apply_term_resize(wid, 14, cols));
    probe::feed(&app, session, &probe::screen_at(&probe::AT_REST, 14, cols));
    frame(&mut app, wid);
    app.hold_account_menu_reads_for_test();
    let anchor = app
        .fact_anchor(wid, session, Fact::Account)
        .expect("real account footer");
    app.open_fact_menu(wid, session, Fact::Account, anchor, true);
    let (choices, unpinned) = accounts(disabled);
    assert!(app.complete_account_menu_read_for_test(Ok((choices, unpinned))));
    assert_eq!(
        app.autopilot_menu
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .filter(|row| matches!(
                row,
                MenuRow::Item {
                    act: MenuAct::Fact(Fact::Account, FactAct::UseAccount(_)),
                    ..
                }
            ))
            .count(),
        64
    );
    assert!(
        !app.autopilot_menu
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|row| matches!(row, MenuRow::Plot { .. })),
        "fallback menu without Outlook"
    );
    (app, wid, session)
}

fn key(app: &mut App, wid: WindowId, key: NamedKey) {
    assert!(app.autopilot_menu_input_event(
        wid,
        &crate::input::InputEvent::Key {
            key: Key::Named(key),
            mods: Default::default(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        }
    ));
}

/// Require the selected logical action to have a real painted row and matching hit target.
fn selected(app: &App, wid: WindowId) -> String {
    let menu = app.autopilot_menu.as_ref().unwrap();
    let rect = menu.rect.expect("painted menu");
    let i = menu.highlight.expect("selected action");
    let visible = menu.viewport.range(menu.rows.len(), rect.h - 2);
    assert!(visible.contains(&i), "selection {i} outside {visible:?}");
    let MenuRow::Item {
        act,
        label,
        enabled: true,
        ..
    } = &menu.rows[i]
    else {
        panic!("selection must name an enabled action");
    };
    let row = rect.row + 1 + i - visible.start;
    assert_eq!(
        item_at(rect, &menu.rows, menu.viewport, row, rect.col + 1),
        Some(i)
    );
    let drawn: String = app.windows[&wid].input_scratch.cells[row]
        .iter()
        .skip(rect.col)
        .take(rect.w)
        .map(|cell| cell.ch)
        .collect();
    assert!(
        drawn.contains(label.split_whitespace().next().unwrap()),
        "{i}: {drawn}"
    );
    #[cfg(a11y_tree)]
    {
        let grid = app.autopilot_grid_menu(wid).expect("published menu");
        let item = &grid.items[grid.selected.expect("published selection")];
        assert_eq!(item.row + usize::from(app.chrome_rows(wid)), row);
    }
    act.key()
}

fn capture(app: &mut App, wid: WindowId, path: &std::path::Path) {
    let png = crate::scripted_redraw_fixture::headless(app, |app| {
        let (reply, receive) = std::sync::mpsc::channel();
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
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .unwrap()
            .value
            .2
            .unwrap()
    });
    std::fs::write(path, png).unwrap();
    selected(app, wid);
}

#[test]
fn large_account_popup_viewport_gallery() {
    let (gallery, _permit) =
        crate::scripted_redraw_fixture::gallery_directory("ATERM_POPUP_VIEWPORT_GALLERY");
    let mut manifest = String::from(
        "# Synthetic headless App/Painter popup; no provider or credentials.\nscene\tcolumns\trows\tfile\n",
    );
    for cols in [120, 60] {
        let (mut app, wid, _) = fixture(cols, 0);
        frame(&mut app, wid); // The former bug starts only after geometry has been painted.
        assert_eq!(selected(&app, wid), "fact-menu/account/use/pool-00");
        let file = format!("pool-initial-{cols}.png");
        capture(&mut app, wid, &gallery.join(&file));
        manifest.push_str(&format!("initial\t{cols}\t14\t{file}\n"));
        let mut seen = BTreeSet::new();
        for _ in 0..66 {
            let at = selected(&app, wid);
            seen.insert(at.clone());
            let scene = match at.as_str() {
                "fact-menu/account/use/pool-63" => Some("last-account"),
                "fact-menu/account/account-settings" => Some("tail-controls"),
                _ => None,
            };
            if let Some(scene) = scene {
                let file = format!("pool-{scene}-{cols}.png");
                capture(&mut app, wid, &gallery.join(&file));
                manifest.push_str(&format!("{scene}\t{cols}\t14\t{file}\n"));
            }
            key(&mut app, wid, NamedKey::ArrowDown);
            frame(&mut app, wid);
        }
        let expected: BTreeSet<_> = (0..64)
            .map(|n| format!("fact-menu/account/use/pool-{n:02}"))
            .chain([
                "fact-menu/account/auto-rotate".into(),
                "fact-menu/account/account-settings".into(),
            ])
            .collect();
        assert_eq!(seen, expected);
        assert_eq!(
            selected(&app, wid),
            "fact-menu/account/use/pool-00",
            "wraps after all actions"
        );
        for (key_name, expected) in [
            (NamedKey::End, "fact-menu/account/account-settings"),
            (NamedKey::Home, "fact-menu/account/use/pool-00"),
            (NamedKey::ArrowUp, "fact-menu/account/account-settings"),
        ] {
            key(&mut app, wid, key_name);
            frame(&mut app, wid);
            assert_eq!(selected(&app, wid), expected);
        }
        key(&mut app, wid, NamedKey::PageUp);
        frame(&mut app, wid);
        assert_ne!(selected(&app, wid), "fact-menu/account/account-settings");
        key(&mut app, wid, NamedKey::PageDown);
        frame(&mut app, wid);
        assert_eq!(selected(&app, wid), "fact-menu/account/account-settings");
        app.close_autopilot_menu();
    }
    std::fs::write(gallery.join("manifest.tsv"), manifest).unwrap();
}

#[test]
fn an_unpainted_or_previously_hidden_action_cannot_be_chosen() {
    let (mut app, wid, session) = fixture(60, 56);
    let before = pair(&app, session);
    key(&mut app, wid, NamedKey::Enter);
    assert!(
        app.autopilot_menu.is_some(),
        "no pixels yet: no action dispatched"
    );
    frame(&mut app, wid);
    assert_eq!(selected(&app, wid), "fact-menu/account/use/pool-56");
    assert!(app.autopilot_menu.as_ref().unwrap().viewport.first() > 0);
    key(&mut app, wid, NamedKey::End);
    assert!(app.autopilot_menu.as_ref().unwrap().rect.is_none());
    key(&mut app, wid, NamedKey::Enter);
    assert!(
        app.autopilot_menu.is_some(),
        "pending viewport has no hit targets"
    );
    assert_eq!(pair(&app, session), before);
    frame(&mut app, wid);
    assert_eq!(selected(&app, wid), "fact-menu/account/account-settings");
    for rows in [24, 14] {
        assert!(app.apply_term_resize(wid, rows, 60));
        probe::feed(&app, session, &probe::screen_at(&probe::AT_REST, rows, 60));
        frame(&mut app, wid);
        assert!(
            app.autopilot_menu.is_none(),
            "a relocated footer retires the old anchor"
        );
        let anchor = app
            .fact_anchor(wid, session, Fact::Account)
            .expect("resized real footer");
        app.open_fact_menu(wid, session, Fact::Account, anchor, true);
        assert!(app.complete_account_menu_read_for_test(Ok(accounts(56))));
        frame(&mut app, wid);
        key(&mut app, wid, NamedKey::End);
        frame(&mut app, wid);
        assert_eq!(selected(&app, wid), "fact-menu/account/account-settings");
    }
    // A new Name row cannot keep the old positions, so this refresh really compacts
    // geometry instead of retaining the intentional gaps of removed choices.
    let fresh = crate::fact_menu::rows(
        Fact::Account,
        &crate::fact_menu::FactMenuFacts {
            title: "Replacement pool".into(),
            name: Some("A renamed pane whose title needs its own menu row".into()),
            cli: Some(aterm_agent::harness::accounts::Cli::Kimi),
            accounts: Vec::new(),
            ..Default::default()
        },
    );
    let menu = app.autopilot_menu.as_mut().unwrap();
    assert!(menu.refresh_rows(fresh, "Replacement pool".into(), None));
    frame(&mut app, wid);
    assert_eq!(selected(&app, wid), "fact-menu/account/account-settings");
    assert_eq!(app.autopilot_menu.as_ref().unwrap().viewport.first(), 0);
    app.close_autopilot_menu();
}

#[test]
fn page_keys_start_at_the_visible_page_after_pointer_scrolling() {
    let (mut app, wid, session) = fixture(60, 0);
    frame(&mut app, wid);
    app.close_autopilot_menu();
    let anchor = app.fact_anchor(wid, session, Fact::Account).unwrap();
    app.open_fact_menu(wid, session, Fact::Account, anchor, false);
    assert!(app.complete_account_menu_read_for_test(Ok(accounts(0))));
    frame(&mut app, wid);
    assert!(app.autopilot_menu.as_ref().unwrap().highlight.is_none());
    key(&mut app, wid, NamedKey::PageUp);
    frame(&mut app, wid);
    assert_eq!(selected(&app, wid), "fact-menu/account/use/pool-00");
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -20.0));
    frame(&mut app, wid);
    let menu = app.autopilot_menu.as_ref().unwrap();
    let first = menu.viewport.first();
    let shown = menu.page_rows;
    assert!(first > 0 && menu.highlight.is_none());
    key(&mut app, wid, NamedKey::PageDown);
    frame(&mut app, wid);
    selected(&app, wid);
    assert!(app.autopilot_menu.as_ref().unwrap().viewport.first() >= first + shown);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -1.0));
    frame(&mut app, wid);
    let menu = app.autopilot_menu.as_ref().unwrap();
    let first = menu.viewport.first();
    assert!(menu.highlight.is_none());
    key(&mut app, wid, NamedKey::PageUp);
    frame(&mut app, wid);
    selected(&app, wid);
    assert!(app.autopilot_menu.as_ref().unwrap().viewport.first() <= first - shown);
    app.close_autopilot_menu();
}

#[test]
fn popup_wheel_residuals_and_end_phases_do_not_change_the_covered_pane() {
    let (mut app, wid, session) = fixture(60, 0);
    frame(&mut app, wid);
    // A partial gesture already owned by the pane cannot advance the new popup.
    app.windows.get_mut(&wid).unwrap().scroll_residual = -0.75;
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -0.25));
    assert_eq!(app.autopilot_menu.as_ref().unwrap().viewport.first(), 0);
    assert_eq!(app.windows[&wid].scroll_residual, -0.75);
    app.close_autopilot_menu();
    let anchor = app.fact_anchor(wid, session, Fact::Account).unwrap();
    app.open_fact_menu(wid, session, Fact::Account, anchor, false);
    assert!(app.complete_account_menu_read_for_test(Ok(accounts(0))));
    frame(&mut app, wid);
    app.windows.get_mut(&wid).unwrap().scroll_residual = 0.0;
    app.windows.get_mut(&wid).unwrap().scroll_residual_x = 0.0;
    for delta in [
        MouseScrollDelta::LineDelta(0.0, -0.75),
        MouseScrollDelta::LineDelta(0.75, 0.0),
    ] {
        app.on_mouse_wheel(wid, delta);
    }
    assert_eq!(app.windows[&wid].scroll_residual, 0.0);
    assert_eq!(app.windows[&wid].scroll_residual_x, 0.0);
    let term = app.pool.get(session).unwrap().term.clone();
    let cell_h = app.win_cell_size(wid).1 as i64;
    let future = Instant::now() + Duration::from_secs(60);
    app.windows.get_mut(&wid).unwrap().scroll_glide = Some(crate::ScrollGlideState {
        glide: crate::scroll_motion::Glide::tracked(0, cell_h, future),
        term: term.clone(),
        cell_h,
        engine_row: 0,
        max_rows: 0,
        next_tick: future,
    });
    let before = app.windows[&wid].scroll_glide.as_ref().unwrap();
    let before = (before.glide.settle_start(), before.next_tick);
    for phase in [
        aterm_window::event::TouchPhase::Ended,
        aterm_window::event::TouchPhase::Cancelled,
    ] {
        app.on_mouse_wheel_phased(
            wid,
            MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(0.0, 0.0)),
            phase,
        );
        let after = app.windows[&wid].scroll_glide.as_ref().unwrap();
        assert_eq!((after.glide.settle_start(), after.next_tick), before);
    }
    // Remove the owned fixture's glide before resuming pane input.
    app.windows.get_mut(&wid).unwrap().scroll_glide = None;
    key(&mut app, wid, NamedKey::Escape);
    let (x, y) = app.frame_cell_centre(wid, (5, 2));
    app.on_cursor_moved(wid, x, y);
    for delta in [
        MouseScrollDelta::LineDelta(0.0, -0.25),
        MouseScrollDelta::LineDelta(0.25, 0.0),
    ] {
        app.on_mouse_wheel(wid, delta);
    }
    assert_eq!(app.windows[&wid].scroll_residual, -0.25);
    assert_eq!(app.windows[&wid].scroll_residual_x, 0.25);
    assert_eq!(term_lock(&term).grid().display_offset(), 0);
}

#[test]
fn native_wheel_stays_with_the_popup_and_moved_rows_require_a_fresh_pointer() {
    let (mut app, wid, session) = fixture(60, 0);
    frame(&mut app, wid);
    let menu = app.autopilot_menu.as_ref().unwrap();
    let rect = menu.rect.unwrap();
    let hit = menu.highlight.unwrap();
    let (x, y) = app.frame_cell_centre(wid, (rect.row + 1 + hit, rect.col + 2));
    app.on_cursor_moved(wid, x, y);
    let before = pair(&app, session);
    let term = app.pool.get(session).unwrap().term.clone();
    let scroll = term_lock(&term).grid().display_offset();
    let fp = fingerprint(app.autopilot_menu.as_ref(), wid);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -3.0));
    assert!(app.autopilot_menu.as_ref().unwrap().rect.is_none());
    assert_ne!(fingerprint(app.autopilot_menu.as_ref(), wid), fp);
    frame(&mut app, wid);
    let menu = app.autopilot_menu.as_ref().unwrap();
    assert_eq!(menu.viewport.first(), 3);
    assert!(menu.highlight.is_none());
    assert!(app.autopilot_menu_press(wid, true, x, y, Instant::now()));
    assert!(
        app.autopilot_menu.is_some(),
        "scroll moved a different choice under the stationary pointer"
    );
    assert_eq!(pair(&app, session), before);
    frame(&mut app, wid);
    selected(&app, wid);
    let cell_h = app.win_cell_size(wid).1 as f64;
    for delta in [
        MouseScrollDelta::LineDelta(1.0, 0.0),
        MouseScrollDelta::LineDelta(0.0, 0.0),
        MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(0.0, -cell_h / 4.0)),
    ] {
        app.on_mouse_wheel(wid, delta);
        assert!(app.autopilot_menu.is_some());
        assert_eq!(app.autopilot_menu.as_ref().unwrap().viewport.first(), 3);
    }
    app.on_mouse_wheel(
        wid,
        MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(0.0, -cell_h)),
    );
    frame(&mut app, wid);
    assert_eq!(app.autopilot_menu.as_ref().unwrap().viewport.first(), 4);
    assert_eq!(term_lock(&term).grid().display_offset(), scroll);
    assert_eq!(app.windows[&wid].scroll_frac_px, 0);
    app.close_autopilot_menu();
}
