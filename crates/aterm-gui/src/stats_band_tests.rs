// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The laws of the per-window System Resources band that need no other module's private
//! helper, and the two-window fixture the rest share.

use aterm_window::dpi::PhysicalSize;

use crate::stats_deck::tests::{Attached, attach_stats, new_sites, sites_of};
use crate::stats_deck::{StatsBars, chrome_head_px, reserved_deck_px};
use crate::stats_plan::{DeckForm, DeckSizePref};
use crate::{App, WindowId};

/// The fixture's titlebar band, in points.
pub(crate) const BAND_PTS: f64 = 28.0;
/// The fixture's display scale.
pub(crate) const SCALE: f64 = 2.0;
/// The fixture's 12 pt padding at 2×.
pub(crate) const PAD_PX: usize = 24;

/// Two windows of one process, each drawing its stats bars as glass would: `first`, the
/// process's first window (`App::headless_for_test`'s), which carries the System Resources
/// band, and `second`, created later, which does not. Each window's head is seeded from
/// `deck_head_px` as an attach seeds it, and follows its form at each resize as an attached
/// window's does.
pub(crate) struct TwoWindows {
    pub(crate) app: App,
    pub(crate) first: WindowId,
    pub(crate) second: WindowId,
    /// The stats bars' guard ([`attach_stats`]): bind it for the test's whole body.
    pub(crate) attached: Attached,
}

/// [`TwoWindows`], `first` in front.
pub(crate) fn two_windows() -> TwoWindows {
    let attached = attach_stats();
    let mut app = App::headless_for_test();
    let first = WindowId(0);
    let session = app.next_session_id;
    let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
    for wid in [first, second] {
        seat(&mut app, wid);
    }
    app.frontmost_window = Some(first);
    TwoWindows {
        app,
        first,
        second,
        attached,
    }
}

/// Window `wid` as an attach leaves it: at the fixture's scale, band and pad, its head the one
/// its own form gives.
pub(crate) fn seat(app: &mut App, wid: WindowId) {
    {
        let mut ws = app.windows.get_mut(&wid).expect("the window");
        ws.scale = SCALE;
        ws.head_pts = BAND_PTS;
        ws.metrics.pad = PAD_PX;
        ws.head_follows_form_for_test = true;
    }
    let head = app.deck_head_px(wid, BAND_PTS, SCALE, 0);
    app.windows.get_mut(&wid).expect("the window").metrics.head = head;
}

/// Window `wid` grows to two tabs, the second split: its three sessions, the first tab's first.
pub(crate) fn three_panes_in(app: &mut App, wid: WindowId) -> Vec<u64> {
    let first = app
        .focused_session_id(wid)
        .expect("the first tab's session");
    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));
    let split = app.split_active_stub_tab(wid);
    assert_eq!(app.windows[&wid].tab_set.tabs().len(), 2, "PRECONDITION");
    vec![first, second, split]
}

fn set_bars(app: &mut App, word: &str) {
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some(word.into());
}

fn grid(app: &App, wid: WindowId) -> (u16, u16) {
    (app.windows[&wid].rows, app.windows[&wid].cols)
}

/// The titlebar band alone and the System Resources band's reservation, in device px.
fn band_and_deck(app: &App, wid: WindowId) -> (usize, usize) {
    let (_, cell) = app.win_cell_size(wid);
    (
        chrome_head_px(BAND_PTS, SCALE, StatsBars::Off, DeckForm::Folded, cell),
        reserved_deck_px(StatsBars::Deck, DeckForm::Folded, SCALE, cell),
    )
}

/// The first window reserves its expanded head and every new auxiliary window reserves
/// the compact resource lanes. Each window can expand independently.
#[test]
fn a_processs_first_window_carries_the_band_and_a_later_window_takes_the_sill() {
    let TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = two_windows();
    assert_eq!(app.stats_bars_form(), StatsBars::Deck, "the default");
    assert!(app.windows[&first].carries_band, "the first window");
    assert!(!app.windows[&second].carries_band, "a later window");
    let (band, deck) = band_and_deck(&app, first);
    assert!(deck > 0, "PRECONDITION: the band takes room");

    assert_eq!(app.deck_head_px(first, BAND_PTS, SCALE, 0), band + deck);
    assert_eq!(app.win_head(first), band + deck, "in its birth geometry");
    assert_eq!(app.deck_rows_px(first).map(|rows| rows.len()), Some(deck));
    let frame = app.deck_frame(first).expect("the first window's bars");
    assert_eq!(frame.form, StatsBars::Deck);

    let compact = crate::stats_sill::compact_height(SCALE);
    assert_eq!(app.deck_head_px(second, BAND_PTS, SCALE, 0), band + compact);
    assert_eq!(
        app.win_head(second),
        band + compact,
        "titlebar and compact resources"
    );
    assert_eq!(app.deck_rows_px(second).unwrap().len(), compact);
    assert_eq!(app.deck_drawn_px(second), compact);
    let frame = app.deck_frame(second).expect("the later window's bars");
    assert_eq!(frame.form, StatsBars::Sill);

    // CONTROL: the same later window, born with the bit.
    app.windows.get_mut(&second).expect("window").carries_band = true;
    seat(&mut app, second);
    assert_eq!(app.win_head(second), band + deck, "CONTROL");
    let frame = app.deck_frame(second).expect("CONTROL: its bars");
    assert_eq!(frame.form, StatsBars::Deck, "CONTROL");
}

#[test]
fn window_resource_strip_keeps_its_laid_hits_and_width_when_the_left_panel_appears() {
    use crate::stats_paint::HitKind;
    use aterm_window::event::{ElementState, MouseButton};

    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let TwoWindows {
        mut app,
        first,
        attached: _attached,
        ..
    } = two_windows();
    app.vitals.snapshot = Some(std::sync::Arc::new(crate::stats_deck::tests::snapshot()));
    let size = app.window_frame_px_for(first, 44, 150);
    app.on_resize(first, size);
    let bare = app.terminal_frame_width_px(first);
    let now = std::time::Instant::now();
    let mut expected_hit = None;
    for (step, cells) in [0, crate::app_window::DOCK_CELLS, 0]
        .into_iter()
        .enumerate()
    {
        app.set_roster_dock_at(
            first,
            cells,
            now + std::time::Duration::from_secs(60 * step as u64),
        )
        .unwrap();
        assert_eq!(app.windows[&first].dock.cells, cells);
        let frame = app.deck_frame(first).unwrap();
        let terminal_width = app.terminal_frame_width_px(first);
        assert_eq!(
            terminal_width,
            bare - usize::from(cells) * app.win_cell_size(first).0
        );
        assert_eq!(frame.span.x, 0);
        assert_eq!(frame.span.width, size.width);
        assert_eq!(app.network_band_px(first).0, size.width as f32);
        #[cfg(a11y_tree)]
        {
            let bounds = app.stats_a11y(first).unwrap().bounds.unwrap();
            assert_eq!((bounds.x0, bounds.x1), (0.0, f64::from(size.width)));
        }
        app.splice_stats_deck(first);
        let (input, crop) = {
            let ws = &app.windows[&first];
            let full = ws.deck_card.as_ref().unwrap();
            assert_eq!(full.pw, size.width);
            let inside = ws.frame_deck_card().unwrap();
            assert_eq!(inside.pw as usize, terminal_width.min(size.width as usize));
            let panel_end =
                f32::from(crate::app_window::DOCK_CELLS) * app.win_cell_size(first).0 as f32;
            let (x, y, w, h) = ws
                .stats_hits
                .borrow()
                .iter()
                .rev()
                .filter(|hit| matches!(hit.kind, HitKind::Row(_, _)))
                .map(|hit| hit.rect)
                .find(|(x, _, w, h)| *x >= 0.0 && *x < panel_end - 4.0 && *w > 0.0 && *h > 0.0)
                .expect("an instrument over the left panel");
            let px = (x + (x + w).min(panel_end)) / 2.0;
            (
                (
                    f64::from(px),
                    app.frame_origin(first).1 as f64 + f64::from(frame.dy) + f64::from(y + h * 0.5),
                ),
                ws.frame_resource_strip.is_some(),
            )
        };
        if cells != 0 {
            assert!(crop);
        }
        app.on_cursor_moved(first, input.0, input.1);
        let hit = app.stats_hit_under_pointer(first).unwrap();
        if let Some(previous) = expected_hit {
            assert_eq!(hit.kind, previous);
        }
        expected_hit = Some(hit.kind);
        if cells != 0 {
            assert!(app.in_side_inset(first, input.0));
            app.on_mouse_input(first, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(first, ElementState::Released, MouseButton::Left);
            assert_eq!(
                app.windows[&first].stats_lens.open.unwrap().focus,
                hit.instrument()
            );
            app.windows.get_mut(&first).unwrap().stats_lens.open = None;
        }
    }
    // A minimized physical client has no capture or hit surface. Restoring it seats the Deck again.
    for empty in [
        PhysicalSize::new(0, size.height),
        PhysicalSize::new(size.width, 0),
    ] {
        app.windows.get_mut(&first).unwrap().win_px = Some(empty);
        app.splice_stats_deck(first);
        assert!(app.deck_frame(first).is_none());
        assert!(app.windows[&first].frame_deck_card().is_none());
    }
    app.windows.get_mut(&first).unwrap().win_px = Some(size);
    app.splice_stats_deck(first);
    assert_eq!(
        app.windows[&first].frame_deck_card().unwrap().pw,
        size.width
    );
    set_bars(&mut app, "sill");
    let sill = app.deck_frame(first).unwrap();
    assert_eq!(sill.form, StatsBars::Sill);
    let rows = app.deck_rows_px(first).unwrap();
    assert_eq!(sill.dy as usize, rows.start);
    assert_eq!(sill.geom.h as usize, rows.len());
    set_bars(&mut app, "off");
    assert!(app.deck_frame(first).is_none());
}

/// THE FORM. A window's own form is the setting in that window: `off` is Off and `sill` the
/// Sill whatever the bit, and `deck` is the Deck only where the window carries the band. The
/// setting keeps its three words.
#[test]
fn the_stats_bars_setting_gives_the_deck_only_to_a_window_that_carries_the_band() {
    use StatsBars::{Deck, Off, Sill};
    for (setting, carries, want) in [
        (Off, true, Off),
        (Off, false, Off),
        (Sill, true, Sill),
        (Sill, false, Sill),
        (Deck, true, Deck),
        (Deck, false, Sill),
    ] {
        assert_eq!(setting.in_window(carries), want, "{setting:?}, {carries}");
    }
    for word in ["deck", "sill", "off"] {
        let form = StatsBars::parse(Some(word));
        assert_eq!(form.word(), word, "the setting's words stand");
        assert_eq!(form.in_window(true), form, "a window with the band: {word}");
    }
    assert_eq!(StatsBars::parse(None), Deck, "deck is still the default");

    let TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = two_windows();
    for (word, carrying, without) in [
        ("deck", Deck, Sill),
        ("sill", Sill, Sill),
        ("off", Off, Off),
    ] {
        set_bars(&mut app, word);
        assert_eq!(
            app.stats_bars_form().word(),
            word,
            "the setting is the word"
        );
        assert_eq!(
            app.window_bars(first),
            carrying,
            "{word}: the band's window"
        );
        assert_eq!(app.window_bars(second), without, "{word}: a later window");
        assert_eq!(
            app.window_bars(WindowId(99)),
            without,
            "{word}: no such window"
        );
    }
}

/// THE ACT. Showing the band in a window that lacks it, and taking it away again, re-grids each
/// tab of that window exactly once per toggle, booked `site=deck`, inside the frame the window
/// keeps; no other window's sessions book anything or change grid. Under `sill` the act does
/// nothing. CONTROL: toggling the other window books in the other window, so the ledger sees a
/// visit to it.
#[test]
fn showing_the_band_in_a_window_regrids_its_tabs_once_booked_deck_and_no_other_windows() {
    let TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = two_windows();
    let here = three_panes_in(&mut app, second);
    let there = three_panes_in(&mut app, first);
    let (band, deck) = band_and_deck(&app, second);
    let frame = app.held_inner_px(second).expect("the window's frame");
    let sill_grid = grid(&app, second);
    let far_grid = grid(&app, first);

    for (shown, head) in [
        (true, band + deck),
        (false, band + crate::stats_sill::compact_height(SCALE)),
    ] {
        let at = if shown { "shown here" } else { "taken away" };
        let before = (sites_of(&app, &here), sites_of(&app, &there));
        let was = grid(&app, second);
        assert!(app.toggle_window_band(second), "{at}");
        assert_eq!(app.windows[&second].carries_band, shown, "{at}");
        assert_eq!(app.win_head(second), head, "{at}");
        assert_eq!(
            app.held_inner_px(second),
            Some(frame),
            "the frame is kept, {at}"
        );
        let now = grid(&app, second);
        assert_eq!(
            now,
            app.grid_dims_for(second, frame),
            "the grid its frame holds under its new head, {at}"
        );
        assert_ne!(now, was, "PRECONDITION: the head moved the grid, {at}");
        let booked = new_sites(&before.0, &sites_of(&app, &here));
        assert!(
            booked.iter().all(|b| b == &["site=deck"]),
            "one re-grid each, booked deck: {booked:?}, {at}"
        );
        let far = new_sites(&before.1, &sites_of(&app, &there));
        assert!(
            far.iter().all(Vec::is_empty),
            "the other window: {far:?}, {at}"
        );
        assert_eq!(grid(&app, first), far_grid, "the other window's grid, {at}");
        assert_eq!(
            app.window_bars(second),
            if shown {
                StatsBars::Deck
            } else {
                StatsBars::Sill
            },
            "{at}"
        );
    }
    assert_eq!(grid(&app, second), sill_grid, "back where it started");
    assert!(app.windows[&first].carries_band, "the band moved nowhere");

    // The act drops the window's lens.
    app.windows
        .get_mut(&second)
        .expect("window")
        .stats_lens
        .last = Some((
        crate::stats_paint::Instrument::Disk,
        crate::stats_paint::Bar::Global,
    ));
    assert!(app.toggle_window_band(second));
    assert_eq!(app.windows[&second].stats_lens.last, None, "its lens slot");
    assert!(app.toggle_window_band(second));

    // Under `sill` the bit is inert and the act does nothing.
    set_bars(&mut app, "sill");
    let before = (sites_of(&app, &here), sites_of(&app, &there));
    assert!(!app.toggle_window_band(second), "sill: nothing to move");
    assert!(!app.windows[&second].carries_band, "sill: the bit is kept");
    assert_eq!(sites_of(&app, &here), before.0, "sill");
    assert_eq!(sites_of(&app, &there), before.1, "sill");
    set_bars(&mut app, "deck");

    // CONTROL: a toggle of the other window is seen there, and not here.
    let before = (sites_of(&app, &here), sites_of(&app, &there));
    assert!(app.toggle_window_band(first));
    let far = new_sites(&before.1, &sites_of(&app, &there));
    assert!(
        far.iter().all(|b| b == &["site=deck"]),
        "CONTROL: the window toggled books: {far:?}"
    );
    assert_eq!(sites_of(&app, &here), before.0, "CONTROL: and only it");
    assert_ne!(grid(&app, first), far_grid, "CONTROL: its grid moved");
}

/// The act is the person's: a control client's gesture (its key into the palette reaches the
/// same dispatch) moves no band and is told why; the person's own then moves it.
#[test]
fn a_controllers_gesture_cannot_move_the_band_and_a_persons_can() {
    let TwoWindows {
        mut app,
        first: _first,
        second,
        attached: _attached,
    } = two_windows();
    assert!(!app.with_gesture_source(true, |app| app.toggle_window_band(second)));
    assert!(!app.windows[&second].carries_band, "a controller's");
    assert_eq!(
        app.pending_action_refusal.take().as_deref(),
        Some(crate::stats_band::BAND_PERSON_ONLY)
    );
    assert!(app.with_gesture_source(false, |app| app.toggle_window_band(second)));
    assert!(app.windows[&second].carries_band, "the person's");
    assert_eq!(app.pending_action_refusal, None);
}

/// THE CONFIG RELOAD. A form switch (`deck` to `sill` and back) and a deck-size switch re-grid,
/// booked `site=deck`, only the windows whose own head moved: the one that carries the band.
/// The window without it keeps its grid and books nothing.
#[test]
fn a_form_switch_in_the_config_regrids_only_the_windows_that_carry_the_band() {
    let TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = two_windows();
    let carrying = three_panes_in(&mut app, first);
    let without = three_panes_in(&mut app, second);
    let scratch = aterm_tempfile::tempdir().expect("scratch dir");
    let dir = scratch.path().to_path_buf();
    let apply = |app: &mut App, bars: &str, size: DeckSizePref| {
        let text = format!(
            "[sys]\nstats_bars = \"{bars}\"\nstats_deck_size = \"{}\"\n",
            size.word()
        );
        crate::app_colorscheme::install_resolved_colors::apply_config_text(app, &dir, &text);
    };
    apply(&mut app, "deck", DeckSizePref::Auto);

    for (bars, size, regrids) in [
        ("sill", DeckSizePref::Auto, true),
        ("sill", DeckSizePref::Auto, false),
        ("deck", DeckSizePref::Auto, true),
        ("deck", DeckSizePref::Full, true),
        ("deck", DeckSizePref::Auto, true),
        ("off", DeckSizePref::Auto, true),
        ("off", DeckSizePref::Full, false),
        ("deck", DeckSizePref::Auto, true),
    ] {
        let at = format!("to {bars} {size:?}");
        let before = (sites_of(&app, &carrying), sites_of(&app, &without));
        let previous = grid(&app, second);
        let was = grid(&app, first);
        apply(&mut app, bars, size);
        assert_eq!(app.stats_bars_form().word(), bars, "{at}");
        let booked = new_sites(&before.0, &sites_of(&app, &carrying));
        if regrids {
            assert_ne!(grid(&app, first), was, "PRECONDITION: its head moved, {at}");
            assert!(
                booked.iter().all(|b| b == &["site=deck"]),
                "one re-grid each, booked deck: {booked:?}, {at}"
            );
        } else {
            assert_eq!(grid(&app, first), was, "{at}");
            assert!(booked.iter().all(Vec::is_empty), "{booked:?}, {at}");
        }
        let far = new_sites(&before.1, &sites_of(&app, &without));
        if grid(&app, second) == previous {
            assert!(far.iter().all(Vec::is_empty), "{far:?}, {at}");
        } else {
            assert!(far.iter().all(|b| b == &["site=deck"]), "{far:?}, {at}");
        }
    }
}

/// A HEAD OF ITS OWN. Two windows of one display differ in head, and each window's frame for a
/// grid is sized under its own, whatever head the renderer last held: `grid_dims_for` of that
/// frame is the grid asked, and `ctl resize` on one window after the other drew gives it the
/// grid asked. CONTROL: the frame under the renderer's head, where that is the other window's,
/// holds another grid.
#[test]
fn a_windows_frame_is_sized_under_its_own_head_whichever_window_drew_last() {
    let TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = two_windows();
    let heads = (app.win_head(first), app.win_head(second));
    assert_ne!(heads.0, heads.1, "PRECONDITION: the heads differ");
    // The renderer at the windows' own pad, so only its head is another window's.
    app.backend.set_pad(PAD_PX);
    let pad_top = app.win_pad_top(first);
    app.backend.set_pad_top(pad_top);
    let asked = (31, 97);
    let frames = [first, second].map(|wid| app.window_frame_px_for(wid, asked.0, asked.1));
    assert_eq!(
        i64::from(frames[0].height) - i64::from(frames[1].height),
        heads.0 as i64 - heads.1 as i64,
        "the frames differ by the heads"
    );
    for drew_last in [heads.0, heads.1, 0, heads.0 + heads.1] {
        app.backend.set_head(drew_last);
        for (wid, frame) in [first, second].into_iter().zip(frames) {
            let at = format!("window {} with the renderer's head at {drew_last}", wid.0);
            assert_eq!(
                app.window_frame_px_for(wid, asked.0, asked.1),
                frame,
                "{at}"
            );
            assert_eq!(app.grid_dims_for(wid, frame), asked, "{at}");
        }
    }

    // CONTROL: the frame under the renderer's head, as sized before each window had its own.
    app.backend.set_head(heads.0);
    let chrome = app.chrome_rows(second);
    let under_renderer = app.frame_px(asked.0 + chrome, asked.1);
    assert_ne!(
        app.grid_dims_for(second, under_renderer),
        asked,
        "CONTROL: the other window's head sizes a frame that holds another grid"
    );

    // `ctl resize` on the window without the band, after the band's window drew.
    app.frontmost_window = Some(second);
    assert_eq!(app.apply_grid_resize(asked.0, asked.1), asked);
    assert_eq!(grid(&app, second), asked);
    let held = app.held_inner_px(second).expect("its frame");
    assert_eq!(held, frames[1], "the frame under its own head");
    assert_eq!(app.grid_dims_for(second, held), asked);
}

/// ARRIVAL. A window whose carried frame was sized with the band while it does not carry it,
/// and the reverse, takes its carried grid's own frame: its height changes by the band, its
/// grid does not, and nothing is booked. CONTROL: a frame sized under the window's own form is
/// kept with what it has to spare.
#[test]
fn a_window_restored_under_the_other_form_keeps_its_grid_and_takes_its_grids_own_frame() {
    let TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = two_windows();
    let sessions = [
        three_panes_in(&mut app, first),
        three_panes_in(&mut app, second),
    ];
    for (wid, sessions) in [first, second].into_iter().zip(sessions) {
        let carries = app.windows[&wid].carries_band;
        let at = format!("window {} (carries the band: {carries})", wid.0);
        let (rows, cols) = grid(&app, wid);
        let (_, expanded) = band_and_deck(&app, wid);
        let deck = expanded - crate::stats_sill::compact_height(SCALE);
        let (_, cell) = app.win_cell_size(wid);
        // The frame the other form sized for this grid.
        app.windows.get_mut(&wid).expect("window").carries_band = !carries;
        seat(&mut app, wid);
        let other = app.window_frame_px_for(wid, rows, cols);
        app.windows.get_mut(&wid).expect("window").carries_band = carries;
        // The attach: the head first, then the frame, then the one re-grid it may owe.
        let head = app.attach_deck_head(wid, BAND_PTS, SCALE);
        app.windows.get_mut(&wid).expect("window").metrics.head = head;
        let own = app.window_frame_px_for(wid, rows, cols);
        assert_eq!(
            i64::from(other.height) - i64::from(own.height),
            if carries { -(deck as i64) } else { deck as i64 },
            "PRECONDITION: the frames differ by the band, {at}"
        );
        let before = sites_of(&app, &sessions);
        let size = app.attach_frame_px(wid, rows, cols, Some(other));
        assert_eq!(size, own, "its carried grid's own frame, {at}");
        app.regrid_to_attach_frame(wid, size);
        assert_eq!(grid(&app, wid), (rows, cols), "its grid, {at}");
        assert_eq!(
            sites_of(&app, &sessions),
            before,
            "no new resizes row, {at}"
        );

        // The same in the deck's other form: a frame the band sized Full, carried into a
        // window that takes Folded. Only the window that carries the band reads the excess as
        // its deck (it keeps that frame and re-grids to it once); the one without the band
        // finds no deck for it and takes its grid's own frame.
        let full = {
            let mut ws = app.windows.get_mut(&wid).expect("window");
            ws.carries_band = true;
            ws.deck_form = crate::stats_plan::DeckForm::Full;
            drop(ws);
            let head = app.deck_head_px(wid, BAND_PTS, SCALE, 0);
            app.windows.get_mut(&wid).expect("window").metrics.head = head;
            let full = app.window_frame_px_for(wid, rows, cols);
            app.windows.get_mut(&wid).expect("window").carries_band = carries;
            let head = app.attach_deck_head(wid, BAND_PTS, SCALE);
            app.windows.get_mut(&wid).expect("window").metrics.head = head;
            full
        };
        assert_eq!(
            app.windows[&wid].deck_form,
            crate::stats_plan::DeckForm::Folded,
            "PRECONDITION: the attach takes Folded, {at}"
        );
        assert!(
            full.height > own.height,
            "PRECONDITION: Full is taller, {at}"
        );
        let size = app.attach_frame_px(wid, rows, cols, Some(full));
        if carries {
            assert_eq!(
                size, full,
                "CONTROL: the band's window keeps the Full frame"
            );
        } else {
            assert_eq!(
                size, own,
                "no deck is found for the band's Full frame, {at}"
            );
            app.regrid_to_attach_frame(wid, size);
            assert_eq!(grid(&app, wid), (rows, cols), "its grid, {at}");
            assert_eq!(sites_of(&app, &sessions), before, "no `site=deck`, {at}");
        }

        // CONTROL: a frame its own form sized, with half a cell to spare, is kept.
        let spare = PhysicalSize::new(own.width, own.height + (cell / 2) as u32);
        assert_eq!(
            app.attach_frame_px(wid, rows, cols, Some(spare)),
            spare,
            "CONTROL: {at}"
        );
    }
}

/// THE ROW. The palette's command and the published menu state read the front window: the
/// check is its bit, and the command is idle unless the setting is `deck`.
#[test]
fn the_rows_check_follows_the_front_window_and_it_is_idle_unless_the_setting_is_deck() {
    let TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = two_windows();
    let row = crate::menu::MenuAction::ShowSystemResourcesHere;
    let resolved = |app: &App| {
        let mut palette = crate::palette::PaletteState::new();
        palette.resolve(&app.palette_live());
        palette.action_by_name("ShowSystemResourcesHere")
    };
    // The check a palette resolved now draws on the row.
    let row_check = |app: &App| {
        let mut palette = crate::palette::PaletteState::new();
        palette.resolve(&app.palette_live());
        palette
            .rows()
            .iter()
            .find(|r| r.menu() == Some(row))
            .expect("the palette carries the row")
            .checked
    };
    for (front, here) in [(first, true), (second, false)] {
        app.frontmost_window = Some(front);
        assert_eq!(app.palette_live().band_here, here, "window {}", front.0);
        app.publish_menu_live();
        assert_eq!(crate::menu::menu_live().band_here, here);
        assert_eq!(crate::menu::native_menu_checked(row), Some(here));
        assert_eq!(resolved(&app), Ok(row), "live under deck");
        assert_eq!(row_check(&app), Some(here), "the palette row's check");
    }
    assert!(app.toggle_window_band(second));
    assert!(app.palette_live().band_here, "the act moves the check");
    // Under `sill` and `off` the row is idle and its check is still the window's bit, on and
    // off.
    for word in ["sill", "off"] {
        set_bars(&mut app, word);
        assert!(resolved(&app).is_err(), "idle under {word}");
        for (front, here) in [(second, true), (first, true)] {
            app.frontmost_window = Some(front);
            assert_eq!(app.palette_live().band_here, here, "kept under {word}");
            app.publish_menu_live();
            assert_eq!(crate::menu::native_menu_checked(row), Some(here), "{word}");
            assert_eq!(row_check(&app), Some(here), "{word}: the palette row");
        }
        app.windows.get_mut(&first).expect("window").carries_band = false;
        app.publish_menu_live();
        assert_eq!(crate::menu::native_menu_checked(row), Some(false), "{word}");
        assert_eq!(row_check(&app), Some(false), "{word}: the palette row");
        app.windows.get_mut(&first).expect("window").carries_band = true;
    }
}

/// A command palette already open when the band moves shows the row's new check: the act
/// re-resolves it, so Enter on the row does what its check says.
#[test]
fn a_palette_open_across_the_act_shows_the_rows_new_check() {
    let TwoWindows {
        mut app,
        first,
        second: _second,
        attached: _attached,
    } = two_windows();
    let row = crate::menu::MenuAction::ShowSystemResourcesHere;
    let open_check = |app: &App| {
        app.windows[&first]
            .palette()
            .expect("the palette is open")
            .rows()
            .iter()
            .find(|r| r.menu() == Some(row))
            .expect("the palette carries the row")
            .checked
    };
    app.palette_enter();
    assert_eq!(
        open_check(&app),
        Some(true),
        "PRECONDITION: resolved at open"
    );
    assert!(app.toggle_window_band(first));
    assert_eq!(
        open_check(&app),
        Some(false),
        "the open row follows the bit"
    );
    assert!(app.toggle_window_band(first));
    assert_eq!(open_check(&app), Some(true));
}

/// The accessibility tree reads the Network State band's meters where the band is drawn: in the
/// window that carries it, and under a window's open Network State card. A window that shows
/// the Sill draws no such band and announces none.
#[cfg(a11y_tree)]
#[test]
fn the_tree_reads_the_network_state_band_only_where_it_is_drawn() {
    let TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = two_windows();
    let band = |app: &App, wid: WindowId| -> Vec<String> {
        let f = app.deck_frame(wid).expect("the window's frame");
        crate::network_deck::meters(
            &app.network_snapshot,
            &app.lab_deck,
            app.fleet_field.as_deref(),
            (f.geom.w, f.geom.scale),
        )
        .into_iter()
        .map(|(name, _)| name)
        .collect()
    };
    let read = |app: &App, wid: WindowId| -> Vec<String> {
        let stats = app.stats_a11y(wid).expect("the stats are read");
        stats.group.meters.iter().map(|m| m.name.clone()).collect()
    };
    let names = band(&app, first);
    assert!(!names.is_empty(), "PRECONDITION: the band has meters");
    let deck = read(&app, first);
    assert!(
        names.iter().all(|n| deck.contains(n)),
        "CONTROL: the band's window reads them: {deck:?}"
    );
    let names = band(&app, second);
    let sill = read(&app, second);
    assert!(
        names.iter().all(|n| !sill.contains(n)),
        "the Sill window reads none of {names:?}: {sill:?}"
    );
    assert!(app.open_network_lens_in(second), "View ▸ Network State");
    let carded = read(&app, second);
    assert!(
        names.iter().all(|n| carded.contains(n)),
        "its open card is read: {carded:?}"
    );
}

#[test]
fn the_client_presents_the_same_resource_raster_over_the_dock_and_terminal() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("deck".into());
    crate::stats_band::tests::seat(&mut app, wid);
    app.apply_window_scale(wid);
    let size = app.held_inner_px(wid).unwrap();
    app.windows.get_mut(&wid).unwrap().win_px = Some(size);
    app.on_resize(wid, size);
    app.redraw_window(wid);
    app.set_dock(wid, 30).unwrap();
    app.publish_navigators(std::time::Instant::now());
    app.redraw_window(wid);

    let ((width, height), pixels) = crate::scripted_redraw_fixture::glass(&mut app);
    let (fx, fy) = app.frame_origin(wid);
    let seam = usize::try_from(fx).expect("the dock precedes the terminal");
    let ws = &app.windows[&wid];
    assert!(ws.frame_resource_strip.is_some());
    let full = ws.deck_card.as_ref().unwrap();
    assert_eq!(full.pw as usize, width);
    let mut checked = [0, 0];
    for y in 0..full.ph as usize {
        let yy = fy + i64::from(full.dy) + y as i64;
        if !(0..height as i64).contains(&yy) {
            continue;
        }
        for x in seam.saturating_sub(10)..(seam + 10).min(full.pw as usize) {
            let xx = x as i64;
            if !(0..width as i64).contains(&xx) {
                continue;
            }
            let offset = (y * full.pw as usize + x) * 4;
            let rgba = &full.rgba[offset..offset + 4];
            if rgba[3] == 255 {
                let color = u32::from_be_bytes([0, rgba[0], rgba[1], rgba[2]]);
                assert_eq!(
                    pixels[yy as usize * width + xx as usize] & 0xffffff,
                    color,
                    "header pixel ({x}, {y})"
                );
                checked[usize::from(x >= seam)] += 1;
            }
        }
    }
    assert!(
        checked.iter().all(|n| *n > 0),
        "both sides of the seam were visible: {checked:?}"
    );
}
