// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, CloseOutcome, GestureOrigin, SelectionType, WindowId, stub_session, term_lock};
use std::time::{Duration, Instant};

fn materialize_resident_pet(app: &mut App, wid: WindowId, now: Instant) {
    let ws = app.windows.get_mut(&wid).expect("test window");
    ws.fx
        .companion_mut()
        .set_species(aterm_effects::kitty_pet::PetSpecies::Dog);
    let frame = crate::app_render::pet_tick_for_test(
        ws,
        aterm_effects::host::CaptureMode::StaticCapture,
        aterm_effects::kitty_pet::PetSense {
            caret_drawn: true,
            now,
            caret: Some((4, 12)),
            rows: 24,
            cols: 80,
            cell_w: 8,
            cell_h: 16,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        },
    );
    assert!(frame.alpha > 0 && ws.fx.companion_mut().brain().is_active());
    assert!(
        ws.fx.companion_mut().hit_rect().is_some(),
        "the live resident owns this frame's hit target"
    );
}

fn earn_flying_cursor_cat(app: &mut App, wid: WindowId, now: Instant) {
    let ws = app.windows.get_mut(&wid).expect("test window");
    for i in 0..120 {
        ws.fx
            .companions_mut()
            .cursor_cat
            .on_key(now + Duration::from_millis(i * 40), true);
    }
    assert!(
        ws.fx.companions_mut().cursor_cat.is_active(),
        "fixture earns an ordinary flight"
    );
}

fn project_cursor_companion_owner_lifecycle(
    model: &aterm_spec::derive::Model,
    app: &App,
    wid: WindowId,
    phase: i64,
) -> aterm_spec::interp::State {
    let ws = app.windows.get(&wid).expect("test window");
    let mut state = model.init_state();
    state.insert("phase", phase);
    state.insert(
        "pet_visible",
        i64::from(ws.fx.companion().brain().is_active()),
    );
    state.insert(
        "hit_target",
        i64::from(ws.fx.companion().hit_rect().is_some()),
    );
    // The brain's identity, never the owner's configured species (which no retire,
    // drain or rebuild moves, so reading it would make `DurableIdentitySurvives`
    // unobservable). `set_species` writes through.
    state.insert(
        "durable_identity",
        i64::from(ws.fx.companion().brain().species() == aterm_effects::kitty_pet::PetSpecies::Dog),
    );
    state
}

fn assert_cursor_companion_owner_transition(
    model: &aterm_spec::derive::Model,
    action: &'static str,
    before: &aterm_spec::interp::State,
    after: &aterm_spec::interp::State,
) {
    assert_eq!(
        model.successors(action, before).as_slice(),
        std::slice::from_ref(after),
        "shipping companion transition must conform specifically to {action}"
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "shipping companion transition violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
}

/// A copy reads the terminal the gesture was addressed to (`copy_selection_in(wid)`, for both
/// the ⌘-C arm and `Action::Copy`), not `frontmost_window`, which can move ahead of OS focus.
#[test]
fn a_copy_resolves_the_routed_window_not_the_frontmost_one() {
    use aterm_core::selection::SelectionSide;

    let mut app = App::headless_for_test();
    let wid_a = WindowId(0);
    let term_a = app.front_terminal(wid_a).expect("terminal A").term.clone();
    {
        let mut t = term_lock(&term_a);
        t.process(b"hello world\r\n");
        let sel = t.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    let sid = app.next_session_id;
    let wid_b = app.insert_logical_window(stub_session(sid), 24, 80);
    assert_eq!(app.frontmost_window, Some(wid_b));
    assert!(
        !app.copy_selection(),
        "the frontmost form resolves window B, which has no selection"
    );
    assert!(
        app.copy_selection_in(wid_a),
        "the routed form reads the window it was asked for"
    );
    assert_eq!(
        crate::control::PBPASTE_STUB
            .with(|b| b.borrow().clone())
            .as_deref(),
        Some("hello")
    );
}

/// Projection shared by the `FocusModifierCache` refinement anchors and its Tier-1
/// runner. `fresh_ctrl`: an authoritative Ctrl snapshot arrived since the last
/// `FocusOut`; `delivered_ctrl`: the encoder's output for a driven `l`.
pub(crate) fn project_focus_modifier_cache(
    app: &App,
    wid: WindowId,
    fresh_ctrl: i64,
    delivered_ctrl: i64,
) -> std::collections::BTreeMap<&'static str, i64> {
    use aterm_window::keyboard::ModifiersState;

    let ws = &app.windows[&wid];
    [
        ("focused", i64::from(ws.focused)),
        (
            "cached_ctrl",
            i64::from(ws.mods.contains(ModifiersState::CONTROL)),
        ),
        ("fresh_ctrl", fresh_ctrl),
        ("delivered_ctrl", delivered_ctrl),
    ]
    .into_iter()
    .collect()
}

/// `on_resize` never leaves a zero-dimension grid even for a degenerate size (macOS
/// `Resized` payloads can be unreliable): the grid fit clamps rows and cols to ≥ 1, and a
/// later normal size recovers full height. Headless drives the passed-size fallback.
#[test]
fn on_resize_never_yields_a_degenerate_or_wedged_grid() {
    use aterm_window::dpi::PhysicalSize;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    app.on_resize(wid, PhysicalSize::new(1600, 1000));
    let (big_rows, big_cols) = {
        let Some(ws) = app.windows.get(&wid) else {
            panic!("front window must exist")
        };
        (ws.rows, ws.cols)
    };
    assert!(
        big_rows >= 1 && big_cols >= 1,
        "sane grid: {big_rows}x{big_cols}"
    );

    // A spurious 1×1 `Resized`.
    app.on_resize(wid, PhysicalSize::new(1, 1));
    {
        let Some(ws) = app.windows.get(&wid) else {
            panic!("front window must exist")
        };
        assert!(
            ws.rows >= 1 && ws.cols >= 1,
            "clamped grid: {}x{}",
            ws.rows,
            ws.cols
        );
    }

    app.on_resize(wid, PhysicalSize::new(1600, 1000));
    {
        let Some(ws) = app.windows.get(&wid) else {
            panic!("front window must exist")
        };
        assert_eq!(
            (ws.rows, ws.cols),
            (big_rows, big_cols),
            "grid recovers full size"
        );
    }
}

/// Refill window 0's capture buffer from the engine and splice the band, as a
/// redraw does.
fn compose_band_for_test(app: &mut App) {
    let wid = WindowId(0);
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let ws = app.windows.get_mut(&wid).expect("window 0");
        let mut term = crate::term_lock(&terminal);
        term.cell_frame_into(&mut ws.input_scratch, 24, 80);
    }
    app.splice_message_band(wid, aterm_render::Theme::default());
}

/// The composed band is sealed on the row that meets the terminal.
#[test]
fn the_composed_band_is_sealed_on_the_row_that_meets_the_terminal() {
    use aterm_messages::{Message, Severity, tags};
    let ink = crate::chrome_band::band_colors(aterm_render::Theme::default()).label;
    // A row's closing line, drawn as underlines or as a rail as the cell
    // height allows (`message_band::closing_line`).
    let line = |app: &App, row: usize| {
        crate::message_band::closing_line(&app.windows[&WindowId(0)].input_scratch, row)
    };

    // Padded: two rows committed, one dismissed inside the shrink quiet.
    let mut app = App::headless_for_test();
    app.post_message(Message::new(tags::SESSION, Severity::Info, "first"));
    let second = app.post_message(Message::new(tags::SESSION, Severity::Info, "second"));
    assert_eq!(app.message_band_rows, 2, "two rows committed");
    app.messages.dismiss(second, std::time::Instant::now());
    app.sync_messages();
    assert_eq!(
        app.message_band_rows, 2,
        "the count lags the dismissal, so the splice PADS — the case under test"
    );
    compose_band_for_test(&mut app);
    // The row above keeps only the stack's divider, never the seam's ink.
    let colors = crate::chrome_band::band_colors(aterm_render::Theme::default());
    let divider = crate::chrome_band::mix3(
        colors.bar_bg,
        colors.label,
        crate::message_band::DIVIDER_ALPHA,
    );
    assert_eq!(
        line(&app, 0),
        Some(divider),
        "the seam must leave the row above"
    );
    assert_eq!(
        line(&app, 1),
        Some(ink),
        "the padded row meets the terminal, so it carries the seam"
    );
}

#[test]
fn native_tab_install_and_close_are_atomic_without_a_pty_mirror() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let update = crate::update_screen::UpdateState::from_status(1, "0.1.0", None, false);
    let native_app = crate::native_app::NativeApp::Settings(Box::new(
        crate::native_settings::SettingsApp::new(update),
    ));
    let native_view = crate::native_app::AppViewState::Settings(Box::new(
        crate::native_settings::SettingsViewState::new(&app.config),
    ));
    let presentation = crate::tab_model::TabPresentation {
        title: "Settings".to_string(),
        icon: Some(crate::tab_model::TabIconKind::Settings),
        indicators: crate::tab_model::TabIndicators::default(),
        conn: None,
        closable: true,
        tooltip: Some("aterm Settings".to_string()),
    };

    let (instance, tab_id, view_id) = app
        .install_new_native_tab(wid, native_app, native_view, presentation)
        .expect("native tab transaction");
    let Some(ws) = app.windows.get(&wid) else {
        panic!("host window")
    };
    assert_eq!(ws.layouts.len(), 1, "native tab fabricated no PaneTree");
    assert_eq!(ws.tabs.count, 1, "native tab fabricated no TabIndex slot");
    assert_eq!(ws.tab_set.len(), 2);
    assert_eq!(ws.tab_set.active_id(), Some(tab_id));
    assert!(app.native_runtime.app(instance).is_some());
    assert!(app.native_runtime.view_state(view_id).is_some());
    assert!(
        app.active_handle.lock().unwrap().is_none(),
        "native front content must not retain a hidden terminal target"
    );
    assert!(app.structural_invariants_ok());

    let removed = app
        .windows
        .get_mut(&wid)
        .and_then(|ws| ws.tab_set.remove(tab_id))
        .expect("installed stable tab");
    assert_eq!(removed.root.leaves(), vec![view_id]);
    assert!(app.remove_view_link(view_id).is_some());
    assert!(app.native_runtime.view_state(view_id).is_none());
    assert!(app.native_runtime.app(instance).is_none());
    app.sync_active_session();
    assert!(
        app.active_handle.lock().unwrap().is_some(),
        "returning to the terminal republishes its compatibility target"
    );
    assert!(app.structural_invariants_ok());

    let rejected = app.install_native_tab(
        WindowId(u64::MAX),
        instance,
        crate::native_app::AppViewState::Settings(Box::new(
            crate::native_settings::SettingsViewState::new(&app.config),
        )),
        crate::tab_model::TabPresentation::terminal("unreachable"),
    );
    assert_eq!(
        rejected,
        Err(crate::app_tabs::NativeTabInstallError::UnknownWindow)
    );
    assert!(app.structural_invariants_ok());
}

/// An OS light↔dark switch re-resolves a `dark:…,light:…` split `theme`: Light moves
/// `self.theme` and the engine factory bg to the light side, and Dark restores the dark side.
#[test]
fn os_appearance_switch_retthemes_split_theme() {
    let mut app = App::headless_for_test();
    app.config =
        aterm_toml::from_str(r#"theme = "dark:Dracula,light:GitHub Light""#).expect("valid toml");
    // Seed the startup (Dark default) resolution.
    app.os_appearance = aterm_types::Appearance::Dark;
    app.theme = app.config.theme_for(aterm_types::Appearance::Dark);
    let dark_bg = app.theme.bg;

    app.sync_app_theme_to_appearance(aterm_types::Appearance::Light);
    assert_eq!(app.os_appearance, aterm_types::Appearance::Light);
    assert_eq!(
        app.theme.bg, 0x00FF_FFFF,
        "switched to GitHub Light's white bg"
    );
    assert_ne!(app.theme.bg, dark_bg);
    // Future tabs inherit the light side via the factory's engine config.
    assert_eq!(
        app.session_factory
            .terminal_config
            .as_ref()
            .expect("factory config set")
            .default_background,
        aterm_types::Rgb::new(0xff, 0xff, 0xff)
    );

    app.sync_app_theme_to_appearance(aterm_types::Appearance::Dark);
    assert_eq!(app.os_appearance, aterm_types::Appearance::Dark);
    assert_eq!(app.theme.bg, dark_bg);
}

/// "Move Tab to New Window" moves the front window's active tab into a new frontmost
/// window: the source loses it, the new window holds exactly it under its stable
/// `TabId`/`ViewId`, the pool's session count is unchanged, and a single-tab source is a
/// no-op.
#[test]
fn detach_active_tab_moves_view_to_new_window() {
    let mut app = App::headless_for_test();
    let sid1 = app.next_session_id;
    app.push_stub_tab(WindowId(0), stub_session(sid1)); // tabs [0, sid1], sid1 active
    let pool_before = app.pool.iter().count();
    let (moved_tab_id, moved_view_id) = {
        let tab = app.windows[&WindowId(0)]
            .tab_set
            .tab_at(1)
            .expect("second stable tab");
        (tab.id, tab.root.leaves()[0])
    };
    let focuses = |app: &App, wid: WindowId| {
        app.windows[&wid]
            .layouts
            .iter()
            .map(|t| t.focus())
            .collect::<Vec<_>>()
    };

    // The logical half of the detach (no OS window attach) returns the new id.
    let wid_b = app
        .detach_active_tab_logical()
        .expect("a 2-tab source detaches its active tab");
    assert!(wid_b > WindowId(0), "minted a strictly-increasing WindowId");
    assert_eq!(app.frontmost_window, Some(wid_b));
    assert_eq!(focuses(&app, WindowId(0)), vec![0], "source lost the tab");
    assert_eq!(focuses(&app, wid_b), vec![sid1], "B holds exactly it");
    let moved = app.windows[&wid_b].tab_set.active().expect("moved tab");
    assert_eq!(moved.id, moved_tab_id, "detach preserves stable TabId");
    assert_eq!(moved.root.leaves(), vec![moved_view_id], "…and ViewId");
    assert!(app.view_store.contains(moved_view_id));
    assert_eq!(app.pool.iter().count(), pool_before, "pool unchanged");
    assert!(app.structural_invariants_ok());

    // Window B now has a single tab: detach is refused and nothing changes.
    assert_eq!(app.detach_active_tab_logical(), None);
    assert_eq!(app.windows.len(), 2, "no window minted on a no-op");
    assert_eq!(app.frontmost_window, Some(wid_b));
    assert_eq!(app.pool.iter().count(), pool_before);
}

/// "Move Tab to Next Window": the front window's active tab moves to the next existing
/// window (wrapping), becoming its active tab, with the pool unchanged and the
/// destination frontmost. (1) A source with two tabs keeps the other and the moved
/// session reflows to the destination geometry; (2) a single-tab source is closed
/// without double-detaching the view. Fewer than two windows is a no-op.
#[test]
fn migrate_active_tab_moves_view_to_next_window() {
    let focuses = |app: &App, wid: WindowId| {
        app.windows[&wid]
            .layouts
            .iter()
            .map(|t| t.focus())
            .collect::<Vec<_>>()
    };

    // No-op with one window.
    let mut app = App::headless_for_test();
    let _ = app.migrate_active_tab_to_next_window();
    assert_eq!(app.windows.len(), 1, "single window: migrate is a no-op");
    assert_eq!(focuses(&app, WindowId(0)), vec![0]);

    // Case 1: the source survives. B is 40x120; window 0 holds [0, sid2].
    let mut app = App::headless_for_test();
    let sid1 = app.next_session_id;
    let wid_b = app.insert_logical_window(stub_session(sid1), 40, 120);
    let sid2 = app.next_session_id;
    app.push_stub_tab(WindowId(0), stub_session(sid2));
    app.frontmost_window = Some(WindowId(0));
    app.sync_active_session();
    let pool_before = app.pool.iter().count();
    let _ = app.migrate_active_tab_to_next_window();
    assert_eq!(app.windows.len(), 2);
    assert_eq!(app.frontmost_window, Some(wid_b), "focus follows the tab");
    assert_eq!(focuses(&app, WindowId(0)), vec![0], "source lost the tab");
    assert_eq!(focuses(&app, wid_b), vec![sid1, sid2], "appended to B");
    assert_eq!(app.windows[&wid_b].tabs.active, 1, "and active there");
    assert_eq!(app.pool.iter().count(), pool_before, "pool unchanged");
    let t = super::term_lock(&app.pool.get(sid2).expect("s2 pooled").term);
    assert_eq!(
        (t.rows(), t.cols()),
        (40, 120),
        "the moved session reflowed to the destination geometry"
    );
    drop(t);
    assert!(app.structural_invariants_ok());

    // Case 2: the source empties and closes.
    let mut app = App::headless_for_test();
    let sid1 = app.next_session_id;
    let wid_b = app.insert_logical_window(stub_session(sid1), 24, 80);
    app.frontmost_window = Some(WindowId(0));
    app.sync_active_session();
    let _ = app.migrate_active_tab_to_next_window();
    assert!(
        !app.windows.contains_key(&WindowId(0)),
        "emptied source closed"
    );
    assert_eq!(app.frontmost_window, Some(wid_b));
    assert_eq!(focuses(&app, wid_b), vec![sid1, 0], "appended to B");
    assert_eq!(app.windows[&wid_b].tabs.active, 1);
    assert_eq!(
        app.pool.iter().count(),
        2,
        "closing the emptied source does NOT detach the already-moved view"
    );
    assert!(app.structural_invariants_ok());
}

/// `window_of_session` prefers the front window, else the lowest id (the `ls` roster rule).
#[test]
fn window_of_session_prefers_the_front_window_then_the_lowest_id() {
    let mut app = App::headless_for_test();
    assert_eq!(app.window_of_session(0), Some(WindowId(0)));

    let front = app
        .open_active_session_in_new_window_logical()
        .expect("the front window has an active session to share");
    assert!(front > WindowId(0), "a strictly-greater new window");
    assert_eq!(app.frontmost_window, Some(front));

    // The roster's rule, so `@<sid> spawn` and `ls` agree.
    assert_eq!(
        app.window_of_session(0),
        Some(front),
        "the front window containing the session wins over the lower id"
    );

    // With no front window holding it, the lowest id that does.
    app.frontmost_window = None;
    assert_eq!(app.window_of_session(0), Some(WindowId(0)));
}

/// "Open Active Session in New Window" adds a view: both windows show one pooled session
/// (views 1→2); closing one viewer keeps it, and closing the last drops it and exits.
#[test]
fn open_active_session_in_new_window_shares_one_session_across_two_windows() {
    let mut app = App::headless_for_test();
    let new_wid = app
        .open_active_session_in_new_window_logical()
        .expect("the front window has an active session to open in a new window");
    assert_eq!(
        app.frontmost_window,
        Some(new_wid),
        "the new window is frontmost"
    );
    for wid in [WindowId(0), new_wid] {
        assert_eq!(
            app.windows[&wid]
                .front_terminal()
                .expect("front terminal")
                .session,
            0,
            "{wid:?} shows session 0"
        );
    }
    // A SHARED view, not a second session: one pooled session, views 1→2.
    assert_eq!(app.pool.iter().count(), 1);
    assert_eq!(app.pool.views(0), Some(2), "two windows now view session 0");
    assert!(app.structural_invariants_ok());

    // Closing one viewer releases one view: the session and its PTY survive, and only the
    // original window displays it.
    assert_eq!(app.close_window_logical(new_wid), CloseOutcome::Stay);
    assert_eq!(app.pool.views(0), Some(1), "views 2→1");
    assert_eq!(
        app.windows_displaying(0).collect::<Vec<_>>(),
        vec![WindowId(0)],
        "only the surviving viewer displays session 0",
    );
    assert!(app.structural_invariants_ok());

    // Closing the last viewer drops the session and, as the last window, exits the app.
    assert_eq!(
        app.close_window_logical(WindowId(0)),
        CloseOutcome::Exit,
        "closing the last window exits the app",
    );
    assert!(
        app.pool.get(0).is_none(),
        "the last viewer leaving drops the session"
    );
    assert_eq!(app.pool.iter().count(), 0, "the pool is empty");
}

/// `sync_window`'s shared re-fit gate stays closed for a never-shared pool (no
/// O(tabs × panes) `resize_panes` walk per tab switch), latches open when a share is
/// created, and stays latched after the share ends so the survivor grows back from the
/// foreground-min.
#[test]
fn sync_window_refit_gate_latches_once_a_share_exists() {
    let mut app = App::headless_for_test();
    assert!(!app.pool.any_shared(), "one view per session at start");
    app.sync_window(WindowId(0));
    assert!(!app.ever_shared, "never-shared pool keeps the gate closed");

    // Cmd-Shift-O: views 1→2; the creation path's sync observes the share.
    let new_wid = app
        .open_active_session_in_new_window_logical()
        .expect("the front window has an active session to share");
    assert!(app.pool.any_shared(), "the share is live");
    assert!(app.ever_shared, "share creation latches the re-fit gate");

    // The share ends (views 2→1): the latch must survive so survivors re-fit.
    assert_eq!(app.close_window_logical(new_wid), CloseOutcome::Stay);
    assert!(!app.pool.any_shared(), "no live share remains");
    assert!(
        app.ever_shared,
        "the gate stays latched after the share ends"
    );
}

/// Copy-on-select fires on a completed drag only when opted in; the toggle off or a
/// plain click never fires. The selection stays intact after the copy.
#[test]
fn copy_on_select_fires_only_on_completed_drag_when_enabled() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    // Arm a real (non-empty) selection over some on-screen text, then mark the
    // window mid-drag so `finish_selection` treats the release as a completed drag.
    fn arm_drag(app: &mut App) {
        let terminal = app.front_terminal(WindowId(0)).expect("front terminal");
        {
            let mut term = crate::term_lock(&terminal.term);
            term.process(b"hello world");
            let sel = term.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 4, SelectionSide::Right);
        }
        let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
        ws.selecting = true;
        ws.sel_dragged = true;
    }

    // Off.
    let mut app = App::headless_for_test();
    app.copy_on_select = false;
    arm_drag(&mut app);
    assert!(
        !app.finish_selection(WindowId(0), false),
        "copy-on-select must NOT fire when the toggle is off"
    );

    // On.
    let mut app = App::headless_for_test();
    app.copy_on_select = true;
    arm_drag(&mut app);
    assert!(
        app.finish_selection(WindowId(0), false),
        "copy-on-select must fire on a completed drag when enabled"
    );
    {
        assert!(
            crate::term_lock(
                &app.front_terminal(WindowId(0))
                    .expect("front terminal")
                    .term
            )
            .text_selection()
            .has_selection(),
            "the selection is preserved after copy-on-select (Cmd-C still works)"
        );
    }

    // On, a plain click.
    let mut app = App::headless_for_test();
    app.copy_on_select = true;
    {
        let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
        ws.selecting = true;
        ws.sel_dragged = false; // a press+release within one cell: a deselecting click
    }
    assert!(
        !app.finish_selection(WindowId(0), false),
        "a plain click never auto-copies, even when enabled"
    );

    // A drag injected by a scoped-edge `mouse` verb (`suppress_copy_on_select`) completes
    // the selection but never auto-copies: a `WriteInput` edge cannot exfiltrate to the
    // clipboard.
    let mut app = App::headless_for_test();
    app.copy_on_select = true;
    arm_drag(&mut app);
    assert!(
        !app.finish_selection(WindowId(0), true),
        "a scoped-edge injected gesture must NOT auto-copy (exfil fence)"
    );
    {
        assert!(
            crate::term_lock(
                &app.front_terminal(WindowId(0))
                    .expect("front terminal")
                    .term
            )
            .text_selection()
            .has_selection(),
            "the selection is still completed for a scoped-edge gesture (nav, not exfil)"
        );
    }
}

/// A co-viewed session's single `Wake::Exit` closes it in every viewing window
/// (`exit_session_logical` releases all views).
#[test]
fn shared_session_exit_closes_every_viewer_not_just_the_first() {
    let mut app = App::headless_for_test();
    // Share session 0 into a 2nd window (Cmd-Shift-O): views 1→2, both display it.
    let new_wid = app
        .open_active_session_in_new_window_logical()
        .expect("the front window has an active session to open in a new window");
    assert_eq!(
        app.pool.views(0),
        Some(2),
        "the share gives session 0 two views"
    );
    {
        let mut viewers: Vec<WindowId> = app.windows_displaying(0).collect();
        viewers.sort();
        let mut expected = vec![WindowId(0), new_wid];
        expected.sort();
        assert_eq!(
            viewers, expected,
            "both windows (original + the share) display the session"
        );
    }

    // One exit closes the session in both viewers; each window's only tab was the shared
    // session, so both report a last-tab close.
    let to_close = app.exit_session_logical(0);
    assert_eq!(
        to_close.len(),
        2,
        "BOTH viewers' last tab closed on the shared exit — not just the first owner",
    );
    // Escalate each (the el-free twin of the arm's `close_window`/`el.exit()`).
    for wid in &to_close {
        app.close_window_logical(*wid);
    }

    assert_eq!(
        app.pool.views(0),
        None,
        "every view of the exited session was released"
    );
    assert!(
        app.pool.get(0).is_none(),
        "the fully-exited session is dropped from the pool"
    );
    assert_eq!(
        app.windows_displaying(0).count(),
        0,
        "no window still displays the exited session"
    );
    assert!(
        app.windows.is_empty(),
        "both single-session windows closed on the shared exit"
    );
}

/// Closing the frontmost window re-points to the most recently focused survivor (the one
/// the OS raises); with no focus history (headless) it falls back to the lowest live id,
/// the deterministic choice automation relies on. Closing a non-front window leaves
/// frontmost alone, an unknown id is a fail-closed `Stay`, and the last close is `Exit`.
#[test]
fn close_front_repoints_to_mru_survivor_else_lowest_id() {
    // No focus history (headless): the lowest live id.
    let mut app = App::headless_for_test();
    let s1 = app.next_session_id;
    let _w1 = app.insert_logical_window(stub_session(s1), 24, 80);
    let s2 = app.next_session_id;
    let w2 = app.insert_logical_window(stub_session(s2), 24, 80);
    assert_eq!(
        app.frontmost_window,
        Some(w2),
        "the newest window is frontmost"
    );
    assert!(
        app.focus_order.is_empty(),
        "no OS focus events fired in headless"
    );
    assert_eq!(app.close_window_logical(w2), CloseOutcome::Stay);
    assert_eq!(
        app.frontmost_window,
        Some(WindowId(0)),
        "no focus history → lowest live id survivor (deterministic)",
    );

    // With focus history: the most recently focused survivor.
    let mut app = App::headless_for_test();
    let s1 = app.next_session_id;
    let w1 = app.insert_logical_window(stub_session(s1), 24, 80);
    let s2 = app.next_session_id;
    let w2 = app.insert_logical_window(stub_session(s2), 24, 80);
    // The OS focuses windows 0, then 1, then 2 (2 ends up front). MRU tail = w2.
    app.note_window_focused(WindowId(0));
    app.note_window_focused(w1);
    app.note_window_focused(w2);
    assert_eq!(app.frontmost_window, Some(w2));
    assert_eq!(app.close_window_logical(w2), CloseOutcome::Stay);
    assert_eq!(
        app.frontmost_window,
        Some(w1),
        "MRU survivor (w1) is chosen over the lowest id (window 0)",
    );
    assert!(
        !app.focus_order.contains(&w2),
        "the closed window left the focus stack"
    );
    assert!(app.structural_invariants_ok());

    assert_eq!(app.close_window_logical(WindowId(0)), CloseOutcome::Stay);
    assert_eq!(app.frontmost_window, Some(w1), "closed a sibling");
    assert_eq!(app.pool.iter().count(), 1, "only its session dropped");
    assert_eq!(app.close_window_logical(WindowId(999)), CloseOutcome::Stay);
    assert_eq!(app.close_window_logical(w1), CloseOutcome::Exit);
    assert_eq!(app.frontmost_window, None);
    assert_eq!(app.pool.iter().count(), 0);
}

/// A pane-space mutation retires departed panes only: zoom removes no pane, so each pane keeps
/// its episodes.
#[test]
fn pane_space_change_retires_only_departed_panes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.split_active_stub_tab(wid); // 2 panes: zoom can toggle
    app.recompute_sparkle();
    let (cfg, lexicon) = app
        .sparkle
        .as_ref()
        .map(|r| (r.cfg.clone(), r.lexicon.clone()))
        .expect("sparkle words default on");
    let now = Instant::now();
    // Scan both panes into the engine, each bound to its own pane key —
    // exactly what a composed frame does.
    let epoch = {
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let Some(ws) = app.windows.get_mut(&wid) else {
            panic!("window 0")
        };
        let (rows, cols) = (ws.rows as usize, ws.cols as usize);
        let mut term = term_lock(&terminal);
        let epoch = term.damage_epoch();
        for pane in [0u64, sid] {
            ws.fx.decos_mut().bind_pane(pane, (0, 0));
            ws.fx
                .decos_mut()
                .rescan(&term, rows, cols, &lexicon, &cfg, epoch, now);
        }
        epoch
    };
    assert!(
        !app.windows[&wid].fx.decos().needs_rescan(epoch),
        "the bound pane is scanned at this epoch"
    );
    // Zoom removes no pane: the live pane keeps its scan and episodes.
    app.toggle_pane_zoom();
    assert!(
        !app.windows[&wid].fx.decos().needs_rescan(epoch),
        "a zoom toggle must not wipe a pane that is still in the layout"
    );
    // Closing a pane retires it: `sid` is the pane the loop left bound, so the engine
    // drops that binding and needs a fresh scan.
    app.toggle_pane_zoom(); // unzoom, so the close path runs on the real split
    app.close_session(wid, sid);
    assert!(
        app.windows[&wid].fx.decos().needs_rescan(epoch),
        "the departed pane took its scan state with it"
    );
}

/// A split that cannot fit is refused before anything is spawned: `next_session_id`, pool,
/// registry and tree are untouched.
#[test]
fn a_split_that_cannot_fit_is_refused_before_any_shell_is_spawned() {
    use crate::pane;
    let mut app = App::headless_for_test(); // window 0 = 24x80, session 0
    let wid = WindowId(0);

    // Control: 24x80 has room for a left/right split, so the size gate passes
    // and the call dies further down, at the harness's `proxy: None`.
    let allowed = app
        .split_focused_pane_in(pane::SplitDir::Vertical, None)
        .expect_err("headless harness has no event loop to spawn into");
    assert_eq!(
        allowed, "no event loop to host the new pane",
        "a 24x80 window must get PAST the size gate"
    );

    // Now shrink below what a left/right split needs (2*16 + 1 = 33 columns).
    assert!(app.apply_term_resize(wid, 24, 32), "geometry applies");
    let sessions_before = app.pool.iter().count();
    let next_id_before = app.next_session_id;

    let refusal = app
        .split_focused_pane_in(pane::SplitDir::Vertical, None)
        .expect_err("a 32-column pane cannot hold two 16-column panes");
    assert!(
        refusal.contains("32x24") && refusal.contains("33x4"),
        "the refusal names the pane it measured and the size it needed: {refusal}"
    );

    assert_eq!(
        app.pool.iter().count(),
        sessions_before,
        "no session pooled"
    );
    assert_eq!(
        app.next_session_id, next_id_before,
        "not even a session id was burned"
    );
    assert_eq!(
        app.active_tree(wid).map(pane::PaneTree::len),
        Some(1),
        "the tree is untouched — no invisible pane"
    );

    // The refusal posts a band row: a short title, the whole reason behind Details.
    let row = app
        .messages
        .live_rows()
        .find(|l| {
            l.msg
                .detail
                .first()
                .is_some_and(|d| d.contains("32\u{00d7}24") && d.contains("33\u{00d7}4"))
        })
        .expect("a refused split posts a row naming the sizes");
    assert_eq!(row.msg.severity, aterm_messages::Severity::Error);

    // The other axis still fits in 24 rows (2*3 + 1 = 7 needed), so the gate
    // is per-direction, not a blanket "this window is too small".
    assert_eq!(
        app.split_focused_pane_in(pane::SplitDir::Horizontal, None)
            .expect_err("still no event loop"),
        "no event loop to host the new pane",
        "a top/bottom split of a 24-row pane is fine"
    );
    assert!(app.structural_invariants_ok());
}

/// Zoom cannot smuggle a split past the minimum: splitting un-zooms, so
/// `App::focused_pane_cells` reports the same size zoomed and unzoomed, and the
/// shipping path refuses either way.
#[test]
fn a_zoomed_pane_is_measured_unzoomed_so_zoom_cannot_smuggle_a_split_through() {
    use crate::pane;
    let mut app = App::headless_for_test(); // window 0 = 24x80, session 0
    let wid = WindowId(0);
    app.split_active_stub_tab(wid); // 80 -> 40 | divider | 39, focus right

    // Narrow the window until the focused half is under a left/right split's
    // 33 columns while the whole window is still comfortably over it.
    assert!(app.apply_term_resize(wid, 24, 40), "geometry applies");
    assert_eq!(
        app.focused_pane_cells(wid),
        Some((24, 19)),
        "the focused half of a 40-column window"
    );

    // Zoom, through the shipping action a Cmd-Shift-Return takes. On screen
    // this pane is now the whole 40-column window…
    app.toggle_pane_zoom();
    assert_eq!(
        app.active_tree(wid).map(|t| t.compute_layout(24, 40).len()),
        Some(1),
        "zoom really is showing one full-window pane"
    );
    // …and 40 columns would clear the 33-column bar, so a gate reading the zoomed
    // geometry would let this split through.
    assert!(
        pane::split_fits_in(pane::SplitDir::Vertical, 24, 40),
        "the zoomed rect is big enough — this is the temptation"
    );
    assert_eq!(
        app.focused_pane_cells(wid),
        Some((24, 19)),
        "zoom must not inflate the size the split gate measures"
    );

    let refusal = app
        .split_focused_pane_in(pane::SplitDir::Vertical, None)
        .expect_err("a zoomed 19-column pane is still a 19-column pane");
    assert!(
        refusal.contains("19x24"),
        "the refusal names the UNZOOMED pane: {refusal}"
    );
    assert_eq!(
        app.active_tree(wid).map(pane::PaneTree::len),
        Some(2),
        "the tree is untouched"
    );
    assert!(app.structural_invariants_ok());
}

/// Repeated splits end in refusals instead of unusable panes, and a layout over-split by
/// shrinking the window keeps every pane inside the grid.
#[test]
fn repeated_splits_stop_at_the_minimum_and_shrinking_never_leaves_the_grid() {
    use crate::pane;
    let mut app = App::headless_for_test(); // 24x80
    let wid = WindowId(0);

    // Alternate directions thirteen times through the shipping `split_focused_pane_in`.
    // A gesture that passes the gate stops at `proxy: None` in the harness; that error
    // is the signal to stage the pane the real path would spawn.
    let mut refused = Vec::new();
    for i in 0..13 {
        let dir = if i % 2 == 0 {
            pane::SplitDir::Vertical
        } else {
            pane::SplitDir::Horizontal
        };
        let err = app
            .split_focused_pane_in(dir, None)
            .expect_err("headless: nothing can actually spawn");
        if err == "no event loop to host the new pane" {
            app.split_active_stub_tab_dir(wid, dir);
        } else {
            assert!(
                err.starts_with("Split refused: this pane is "),
                "the only other outcome is the size refusal, got: {err}"
            );
            refused.push(err);
        }
    }
    assert!(
        !refused.is_empty(),
        "the loop must stop minting panes at some point"
    );
    let tree = app.active_tree(wid).expect("terminal tab");
    let panes = tree.len();
    assert!(
        panes < 14,
        "the audit got 14 panes out of these gestures; got {panes}"
    );
    let rects = tree.compute_layout(24, 80);
    for r in &rects {
        assert!(
            r.rows >= pane::MIN_PANE_ROWS && r.cols >= pane::MIN_PANE_COLS,
            "every pane the app CREATED is usable: {r:?}"
        );
        assert!(
            r.row_off + r.rows <= 24 && r.col_off + r.cols <= 80,
            "and on the grid: {r:?}"
        );
    }
    assert!(
        rects.iter().any(|r| r.session == tree.focus()),
        "focus is on a pane that exists in the layout"
    );

    // Shrink far below what the layout needs: panes go sub-minimum but none leaves the
    // grid or is dropped, and growing back is exact.
    let before = app.active_tree(wid).unwrap().compute_layout(24, 80);
    assert!(app.apply_term_resize(wid, 3, 8), "shrink applies");
    let tiny = app.active_tree(wid).unwrap().compute_layout(3, 8);
    assert_eq!(tiny.len(), panes, "shrinking collapses no pane");
    for r in &tiny {
        assert!(r.rows >= 1 && r.cols >= 1, "no 0-extent pane: {r:?}");
        assert!(
            r.row_off < 3 && r.col_off < 8,
            "origin inside the 8x3 grid: {r:?}"
        );
        assert!(
            r.row_off + r.rows <= 3 && r.col_off + r.cols <= 8,
            "extent inside the 8x3 grid: {r:?}"
        );
    }
    assert!(app.apply_term_resize(wid, 24, 80), "grow back applies");
    assert_eq!(
        app.active_tree(wid).unwrap().compute_layout(24, 80),
        before,
        "shrink → grow restores the exact geometry"
    );
    assert!(app.structural_invariants_ok());
}

/// Closing a window with a split tab detaches every pane's view: the pool drops by the
/// pane count.
#[test]
fn close_window_with_split_tab_detaches_every_pane() {
    let mut app = App::headless_for_test(); // window 0, session 0
    // A 2nd window so closing window 0 is a Stay (not the last-window exit).
    let s1 = app.next_session_id;
    let w1 = app.insert_logical_window(stub_session(s1), 24, 80);
    // Split window 0's single tab into two panes (session 0 + a fresh session).
    let split_sid = app.split_active_stub_tab(WindowId(0));
    assert_eq!(
        app.pool.iter().count(),
        3,
        "w0's two split panes + w1's session"
    );
    assert!(app.pool.get(0).is_some() && app.pool.get(split_sid).is_some());

    assert_eq!(app.close_window_logical(WindowId(0)), CloseOutcome::Stay);
    assert!(
        app.pool.get(0).is_none(),
        "split pane A released on window close"
    );
    assert!(
        app.pool.get(split_sid).is_none(),
        "split pane B released on window close"
    );
    assert_eq!(
        app.pool.iter().count(),
        1,
        "pool dropped by EXACTLY the 2 panes"
    );
    assert_eq!(app.frontmost_window, Some(w1));
    assert!(app.structural_invariants_ok());
}

/// Divider drag-to-resize through the real `App` glue: a press on the divider arms it,
/// motion moves the boundary, release ends it. `last_mouse_window_cell` is seeded
/// directly; in a 24x80 window a fresh vertical split's divider is at column 40.
#[test]
fn divider_drag_resizes_split_headless() {
    let mut app = App::headless_for_test(); // window 0 = 24x80, session 0
    let new_sid = app.split_active_stub_tab(WindowId(0)); // 0 | new_sid, divider at col 40
    let wid = WindowId(0);

    let left_before = app
        .active_tree(wid)
        .unwrap()
        .compute_layout(24, 80)
        .into_iter()
        .find(|r| r.session == 0)
        .unwrap()
        .cols;
    assert_eq!(left_before, 40, "fresh split is 50/50");

    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_mouse_window_cell = (5, 10);
    }
    assert!(
        !app.begin_divider_drag(wid),
        "a press inside a pane never grabs the divider"
    );
    assert!(app.windows[&wid].divider_drag.is_none());

    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_mouse_window_cell = (5, 40);
    }
    assert!(
        app.begin_divider_drag(wid),
        "press on the divider arms a drag"
    );
    assert!(app.windows[&wid].divider_drag.is_some());

    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_mouse_window_cell = (5, 20);
    }
    app.drag_divider(wid);
    let left_mid = app
        .active_tree(wid)
        .unwrap()
        .compute_layout(24, 80)
        .into_iter()
        .find(|r| r.session == 0)
        .unwrap()
        .cols;
    assert_eq!(left_mid, 20, "drag to col 20 moved the boundary to 20 cols");

    {
        let t = super::term_lock(&app.pool.get(0).unwrap().term);
        assert_eq!(
            t.cols(),
            20,
            "resize_panes reflowed the left pane to 20 cols"
        );
    }

    // Drag past the left edge (col 0) right away, inside the relayout throttle
    // (`RESIZE_THROTTLE` coalesces it), then release: the release flushes the final
    // position, clamped so no pane is zero-width, and the engine gets that size.
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_mouse_window_cell = (5, 0);
    }
    app.drag_divider(wid);
    assert!(app.finish_divider_drag(wid), "release ends the active drag");
    let layout = app.active_tree(wid).unwrap().compute_layout(24, 80);
    for r in &layout {
        assert!(r.cols >= 1, "clamp keeps every pane non-empty: {r:?}");
    }
    let left_final = layout.iter().find(|r| r.session == 0).unwrap().cols;
    assert!(left_final < 20, "release flushed the coalesced final step");
    assert_eq!(
        super::term_lock(&app.pool.get(0).unwrap().term).cols(),
        left_final,
        "…and the engine got the final size"
    );

    // A second release is a no-op.
    assert!(app.windows[&wid].divider_drag.is_none());
    assert!(
        !app.finish_divider_drag(wid),
        "no drag in flight → release is a no-op"
    );
    let _ = new_sid;
    assert!(app.structural_invariants_ok());
}

/// Every committed resize is a coordinate-space reset: both cursor engines retire
/// their old-layout anchors, and no gesture age (mid-drag, settled drag, out-of-band
/// `resize`/font zoom) licenses a reflow streak. Pinned twice: a same-cell completion
/// (a later program move cannot borrow the resize) and, per gesture age, a real
/// reflow landing (the first tick in the new space only seeds).
#[test]
fn committed_resize_retires_geometry_and_cannot_leak_its_reflow_licence() {
    use crate::cursor_glow::Geom;
    use crate::cursor_trail::TrailConfig;

    let wid = WindowId(0);
    let trail_cfg = TrailConfig {
        enabled: true,
        duration: Duration::from_millis(300),
        max_len: 24,
        color: 0x0050_FA7B,
        intensity: 0.0,
        warmth: 0.0,
    };
    let geom = |rows: usize, cols: usize| Geom {
        cw: 8,
        ch: 16,
        rows,
        cols,
        origin_x: 0,
        origin_y: 0,
        win_w: u16::try_from(cols * 8).unwrap(),
        win_h: u16::try_from(rows * 16).unwrap(),
        head: 0,
    };
    // Seed both engines at `cell`, resize to `rows x cols`, then tick at the cursor's
    // post-resize cell: nothing may paint. Returns the (before, after) cursor cells.
    let run = |app: &mut App, cell: (u16, u16), rows: u16, cols: u16| {
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_string());
        let glow_cfg = app.glow_config();
        let (mut glow_out, mut trail_out) = (Vec::new(), Vec::new());
        let seed = Instant::now();
        let ws = app.windows.get_mut(&wid).unwrap();
        let cursor = ws.fx.cursor_mut();
        cursor
            .glow
            .tick(Some(cell), seed, &glow_cfg, geom(24, 80), &mut glow_out);
        cursor
            .trail
            .tick(Some(cell), seed, &trail_cfg, &mut trail_out);
        assert!(app.apply_term_resize(wid, rows, cols), "geometry applies");
        assert!(
            !app.windows[&wid].fx.cursor().glow.reflow_hint_armed(),
            "a resize commit must NOT license a streak"
        );
        let term = app.pool.get(0).unwrap().term.clone();
        let after = term_lock(&term).cursor();
        let next = if (after.row, after.col) == cell {
            (cell.0, cell.1 + 1)
        } else {
            (after.row, after.col)
        };
        let moved = seed + Duration::from_millis(4);
        let ws = app.windows.get_mut(&wid).unwrap();
        let cursor = ws.fx.cursor_mut();
        let geom = geom(usize::from(rows), usize::from(cols));
        let fp = (
            cursor
                .glow
                .tick(Some(next), moved, &glow_cfg, geom, &mut glow_out),
            cursor
                .trail
                .tick(Some(next), moved, &trail_cfg, &mut trail_out),
        );
        assert_eq!(fp, (0, 0));
        assert!(
            glow_out.is_empty()
                && trail_out.is_empty()
                && cursor.glow.halos().is_empty()
                && cursor.glow.under_quads().is_empty(),
            "a first tick in the new layout cannot bridge to the old one"
        );
        (cell, (after.row, after.col))
    };

    // Same-cell completion: an empty terminal's caret stays at (0, 0).
    let mut still = App::headless_for_test();
    let (before, after) = run(&mut still, (0, 0), 30, 100);
    assert_eq!(before, after, "fixture: the caret does not move");

    let settled = Instant::now() - crate::RESIZE_THROTTLE - Duration::from_millis(5);
    for (gesture, stamp) in [
        ("mid-drag", Some(Instant::now())),
        ("settled drag", Some(settled)),
        ("out-of-band", None),
    ] {
        let mut app = App::headless_for_test();
        app.windows.get_mut(&wid).unwrap().last_resized_event_at = stamp;
        // Shrinking a live logical line wraps the caret into the next row.
        let term = app.pool.get(0).unwrap().term.clone();
        term_lock(&term).process(&[b'x'; 70]);
        let cell = {
            let c = term_lock(&term).cursor();
            (c.row, c.col)
        };
        let (before, after) = run(&mut app, cell, 28, 40);
        assert_ne!(before, after, "{gesture}: fixture must reflow the caret");
    }
}

/// An all-tabs width settle over a multi-tab, multi-pane window gives every pane its
/// off-screen history back, whatever the worker order or concurrency: a job dropped without
/// `finish_resize_offload`/`abort_resize_offload` leaves the history detached. Quiescence is
/// polled per session, since other tests submit reflow jobs concurrently.
#[test]
fn an_all_tabs_width_settle_returns_every_panes_history() {
    fn history(app: &App, id: u64) -> usize {
        let term = app.pool.get(id).expect("pooled session").term.clone();
        let t = super::term_lock(&term);
        t.grid().scrollback_lines()
    }
    fn settled(app: &App, ids: &[u64]) -> bool {
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            if ids.iter().all(|&id| history(app, id) > 0) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.rows = 24;
        ws.cols = 100;
    }
    app.resize_panes_scoped(wid, false);
    // Tabs x two panes, sized to hand off more jobs than the pool ceiling, so the peak
    // assertion below can actually fail for unbounded spawning.
    let ceiling = crate::app_render::reflow_worker_ceiling();
    let tabs = ceiling.div_ceil(2) + 1;
    let _ = app.split_active_stub_tab(wid);
    for _ in 0..tabs - 1 {
        let sid = app.next_session_id;
        app.push_stub_tab(wid, stub_session(sid));
        let _ = app.split_active_stub_tab(wid);
    }
    let mut ids: Vec<u64> = app.pool.iter().map(|s| s.id).collect();
    ids.sort_unstable();
    assert_eq!(ids.len(), tabs * 2, "{tabs} tabs x two panes");
    assert!(
        ids.len() > ceiling,
        "this settle hands off {} job(s) against a pool ceiling of {ceiling} — \
         it cannot distinguish a bounded pool from a thread per job",
        ids.len()
    );

    // Real off-screen history in every pane — an empty store re-attaches
    // vacuously and would prove nothing.
    for &id in &ids {
        let term = app.pool.get(id).expect("pooled session").term.clone();
        let mut t = super::term_lock(&term);
        for i in 0..200u32 {
            t.process(format!("line {i:04} of session {id}\r\n").as_bytes());
        }
    }
    assert!(settled(&app, &ids), "the staging resizes converged");
    let before: Vec<usize> = ids.iter().map(|&id| history(&app, id)).collect();
    assert!(
        before.iter().all(|&n| n >= 100),
        "every pane must own real off-screen history before the settle"
    );

    // The settle: one eager all-tabs width change over every pane.
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.cols = 72;
    }
    app.resize_panes_scoped(wid, false);
    assert!(
        settled(&app, &ids),
        "a pane never got its history back — the settle dropped a job and \
         wedged that session's detach window"
    );
    for (&id, &n0) in ids.iter().zip(before.iter()) {
        let n = history(&app, id);
        assert!(
            n >= n0,
            "session {id}: the rewrap lost history ({n0} -> {n} lines)"
        );
    }
    // Concurrency, from the process-global high-water gauge: valid alongside other
    // tests' jobs; fails if any instant exceeded the bound.
    let peak =
        usize::try_from(crate::app_render::reflow_gauge::snapshot().peak).unwrap_or(usize::MAX);
    assert!(
        peak <= ceiling,
        "{peak} reflow workers ran at once, above the pool ceiling {ceiling} — \
         a settle's concurrency is scaling with the workspace again"
    );
    assert!(app.structural_invariants_ok());
}

/// A live-drag (`active_only`) pass reflows the active tab and defers a plain background tab,
/// but still sizes a shared (`views > 1`) session in a background tab to its foreground
/// viewers' minimum.
#[test]
fn a_scoped_resize_still_reaches_a_shared_background_tab() {
    let mut app = App::headless_for_test(); // window 0 = 24x80, tab 0 = session 0
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session(1)); // tab 1 = session 1 (plain)
    app.push_stub_tab(wid, stub_session(2)); // tab 2 = session 2
    // Share tab 0's session into a second window with a distinct grid, so the shared
    // minimum is an observable target rather than "unchanged".
    app.switch_tab_in(wid, 0);
    let other = app
        .open_active_session_in_new_window_logical()
        .expect("the front window has an active session to share");
    if let Some(ws) = app.windows.get_mut(&other) {
        ws.rows = 20;
        ws.cols = 60;
    }
    app.frontmost_window = Some(wid);
    app.switch_tab_in(wid, 2);
    assert_eq!(app.pool.views(0), Some(2), "session 0 is shared");

    let size = |app: &App, id: u64| {
        let t = super::term_lock(&app.pool.get(id).expect("pooled session").term);
        (t.rows(), t.cols())
    };
    // Grow window 0's grid and run one live-drag (active-only) pass.
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.rows = 30;
        ws.cols = 100;
    }
    app.resize_panes_scoped(wid, true);

    // (Active-tab reflow and plain background deferral are
    // `live_drag_resize_scopes_to_active_tab_then_flushes_on_switch`.)
    assert_eq!(
        size(&app, 0),
        (20, 60),
        "the SHARED session in a BACKGROUND tab is still driven to the \
         element-wise min across its foreground viewers"
    );
    assert!(app.structural_invariants_ok());
}

/// A latency epoch two seconds old, so a stamp aged up to that is non-zero
/// (zero reads as UNARMED) without waiting for the clock to get there.
fn aged_epoch() -> Instant {
    Instant::now()
        .checked_sub(Duration::from_secs(2))
        .expect("uptime")
}

/// An early-out closes the stamps it proved idle: a burst before a compose that
/// presented nothing is discarded; one landing after the compose began survives.
#[test]
fn an_early_out_discards_stamps_that_predate_its_compose_and_keeps_later_ones() {
    use std::sync::atomic::Ordering::Relaxed;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session(1));
    app.switch_tab_in(wid, 0); // session 0 is what this glass shows
    // Age the epoch so a stamp 10 ms back is non-zero (zero reads as unarmed).
    app.lat_epoch = aged_epoch();
    let stamp_of = |app: &App| {
        app.pool
            .get(0)
            .expect("the visible pane's session")
            .last_output_ns
            .clone()
    };
    // Establish the visible set with one present-time walk.
    assert_eq!(app.present_latency_ns(wid), 0, "nothing armed");

    // A burst 10 ms ago, then a compose now that finds nothing to present:
    // the stamp is closed, and the next present books nothing for it.
    let ten_ms_ago = app.lat_epoch.elapsed().as_nanos() as u64 - 10_000_000;
    stamp_of(&app).store(ten_ms_ago, Relaxed);
    app.discard_unpresented_output_stamps(wid, Instant::now());
    assert_eq!(
        stamp_of(&app).load(Relaxed),
        0,
        "a stamp older than the idle compose is closed"
    );
    assert_eq!(
        app.present_latency_ns(wid),
        0,
        "so the next present books nothing for the idle stretch"
    );

    // A burst that lands after the compose began is the next frame's edge:
    // it survives the discard and the next present books it.
    let composed_at = Instant::now()
        .checked_sub(Duration::from_millis(2))
        .expect("uptime");
    let later = app.lat_epoch.elapsed().as_nanos() as u64;
    stamp_of(&app).store(later, Relaxed);
    app.discard_unpresented_output_stamps(wid, composed_at);
    assert_eq!(
        stamp_of(&app).load(Relaxed),
        later,
        "an edge after the compose began is kept"
    );
    assert!(
        app.present_latency_ns(wid) > 0,
        "and the next present books it"
    );
}

/// A `Wake::Output` stamp armed while its pane was off this window's glass is never
/// booked as latency, and booking touches only the active tab's panes. The last block
/// arms a hidden pane and reveals it with no present in between: the stamp must be
/// discarded at the reveal.
#[test]
fn a_revealed_panes_stale_output_stamp_is_discarded_not_booked() {
    use std::sync::atomic::Ordering::Relaxed;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session(1)); // tab 1 = session 1 (now active)
    app.switch_tab_in(wid, 0); // tab 0 (session 0) is what this glass shows
    // The epoch must be older than any simulated age, or `now - age` saturates to
    // zero (the unarmed sentinel) and every assertion passes vacuously.
    app.lat_epoch = aged_epoch();
    let arm = |app: &App, id: u64, age_ns: u64| {
        let now = app.lat_epoch.elapsed().as_nanos() as u64;
        let stamp = now - age_ns;
        assert!(stamp > 0, "a zero stamp reads as UNARMED, not as aged");
        app.pool
            .get(id)
            .expect("pooled session")
            .last_output_ns
            .store(stamp, Relaxed);
    };
    // Establish the visible set with one present-time walk.
    assert_eq!(
        app.present_latency_ns(wid),
        0,
        "nothing armed, nothing booked"
    );

    // Parity 1: a burst in the hidden tab is not this glass's latency.
    arm(&app, 1, 1_000_000_000);
    assert_eq!(
        app.present_latency_ns(wid),
        0,
        "a hidden pane's wait must never be booked against the visible \
         window's present"
    );
    // Parity 2: the visible pane's burst is booked, then consumed.
    arm(&app, 0, 2_000_000);
    let dt = app.present_latency_ns(wid);
    assert!(
        dt >= 2_000_000,
        "the visible pane's 2 ms wait must be booked (got {dt} ns)"
    );
    assert_eq!(
        app.present_latency_ns(wid),
        0,
        "a booked stamp must be consumed so the next burst restarts the clock"
    );

    // Arm the hidden pane and reveal it with no present in between.
    arm(&app, 1, 1_000_000_000);
    app.switch_tab_in(wid, 1);
    assert_eq!(
        app.present_latency_ns(wid),
        0,
        "a stamp that aged through an unwatched interval must be discarded \
         at the reveal, not booked as a second of render latency"
    );
    // ...and the pane keeps working: a burst while it is on the glass books normally.
    arm(&app, 1, 3_000_000);
    let dt = app.present_latency_ns(wid);
    assert!(
        dt >= 3_000_000,
        "the newly revealed pane must book its own honest waits (got {dt} ns)"
    );
}

/// A window off glass books no drop or taint, so its reveal is the seam: a stamp aged
/// through occlusion is discarded at the reveal present, a later burst books normally,
/// and an occlusion that never ends discards nothing.
#[test]
fn an_occlusion_reveal_discards_the_stale_output_stamp_not_books_it() {
    use std::sync::atomic::Ordering::Relaxed;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // The epoch must be older than any simulated age, or `now - age` saturates
    // to the walk's "unarmed" zero sentinel (see the MPT-3 test above).
    app.lat_epoch = aged_epoch();
    let arm = |app: &App, age_ns: u64| {
        let stamp = app.lat_epoch.elapsed().as_nanos() as u64 - age_ns;
        app.pool
            .get(0)
            .expect("pooled session")
            .last_output_ns
            .store(stamp, Relaxed);
    };
    // Establish the visible set with one present-time walk.
    assert_eq!(app.present_latency_ns(wid), 0, "nothing armed yet");

    // Occluded, not yet revealed: a present while hidden still books (the content
    // reached a surface); only the reveal edge discards.
    app.on_occlusion_changed(wid, true);
    arm(&app, 2_000_000);
    let dt = app.present_latency_ns(wid);
    assert!(
        dt >= 2_000_000,
        "an occlusion edge alone must not delete a real sample (got {dt} ns)"
    );

    // Output arrives, the window stays hidden ~1 s with no present, then is revealed:
    // same pane, same tab, so the visible-set membership compare alone sees no reveal.
    arm(&app, 1_000_000_000);
    app.on_occlusion_changed(wid, false);
    assert_eq!(
        app.present_latency_ns(wid),
        0,
        "a stamp that aged while the window was off glass must be discarded \
         at the reveal present, not booked as a second of render latency"
    );
    // ...and the window keeps measuring: a burst while it is on glass books.
    arm(&app, 3_000_000);
    let dt = app.present_latency_ns(wid);
    assert!(
        dt >= 3_000_000,
        "the revealed window must book its own honest waits (got {dt} ns)"
    );
}

/// The O(1) deadline gate before the status sweep never defers a classification the
/// whole-pool scan would make: it cannot close while a session is due, and a brand-new
/// session (no slot, due at once) forces it open via the pool's insert epoch.
#[test]
fn the_status_gate_never_defers_a_classification_the_scan_would_make() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    assert!(
        app.config.tab_status_or_default(),
        "the fixture must have the classifier ON, or this proves nothing"
    );
    // First sweep: session 0 has no slot, so it is due and gets classified.
    assert!(
        app.session_status.due(0, t0),
        "an unobserved session is due"
    );
    let _ = app.observe_session_statuses(t0);
    let inside = t0 + std::time::Duration::from_millis(1);
    assert!(
        !app.session_status.due(0, inside),
        "a just-classified session is rate-limited for one interval"
    );
    // Nothing is due, so the sweep answers empty, as the O(sessions) scan would, in O(1).
    assert!(
        app.observe_session_statuses(inside).is_empty(),
        "a sweep inside the interval publishes nothing"
    );

    // A new session is classified on the very next wake: its insert moves the pool epoch,
    // which opens the gate though every known slot is still rate-limited.
    app.push_stub_tab(wid, stub_session(1));
    let after = inside + std::time::Duration::from_millis(1);
    assert!(
        app.session_status.due(1, after),
        "the newcomer has no slot, so the scan would have classified it"
    );
    let _ = app.observe_session_statuses(after);
    assert!(
        !app.session_status.due(1, after),
        "the newcomer was NOT classified on the next wake — the gate \
         deferred work the whole-pool scan would have done"
    );

    // Past the interval everything is due again and the sweep resumes.
    let later = t0 + std::time::Duration::from_secs(2);
    assert!(app.session_status.due(0, later), "the interval expires");
    let _ = app.observe_session_statuses(later);
    assert!(
        !app.session_status.due(0, later),
        "the expired session was re-observed"
    );
}

/// A session skipped because its terminal lock was held (`try_lock`) still has no slot, so the
/// gate retries it on the next wake rather than bank the pool epoch.
#[test]
fn the_status_gate_stays_open_over_a_session_it_could_not_classify() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    let _ = app.observe_session_statuses(t0); // session 0 earns a slot
    let inside = t0 + std::time::Duration::from_millis(1);
    assert!(
        app.observe_session_statuses(inside).is_empty(),
        "the gate closes once every live session is classified and current"
    );

    // A newcomer whose terminal lock is held, standing in for its PTY
    // reader mid-batch. The insert opens the gate; the held lock is what
    // makes the sweep unable to act on it.
    app.push_stub_tab(wid, stub_session(1));
    let term = app.pool.get(1).expect("pooled session").term.clone();
    let held = super::term_lock(&term);
    let t1 = inside + std::time::Duration::from_millis(1);
    let _ = app.observe_session_statuses(t1);
    assert!(
        app.session_status.due(1, t1),
        "the contended session was not classified — the premise of this test"
    );
    drop(held);

    // The next wake must still reach it, as the whole-pool scan would.
    let t2 = t1 + std::time::Duration::from_millis(1);
    let _ = app.observe_session_statuses(t2);
    assert!(
        !app.session_status.due(1, t2),
        "the gate closed over a session it had never classified — its first \
         status now waits out another session's whole interval"
    );
}

/// The per-window drift gate still fires when a window starts labelling a session, and
/// stays quiet in the steady state (a false negative leaves a stale label; a false
/// positive re-flushes at the debounce ceiling forever).
#[test]
fn the_title_drift_gate_fires_for_a_newly_labeling_window_and_not_otherwise() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    // One flush reaches the steady state: the App-level consumed epoch equals the live one,
    // so the per-window conjunction decides.
    app.observe_title_drift(0, t0);
    let live = {
        let t = super::term_lock(&app.pool.get(0).expect("pooled session").term);
        t.title_epoch()
    };
    assert_eq!(
        app.title_drift.seen.get(&0).copied(),
        Some(live),
        "the first observation consumes the live epoch"
    );
    let flushed_at = app.title_drift.last_flush.get(&0).copied();
    assert!(flushed_at.is_some(), "the first observation flushed");

    // Steady state: nothing changed, so no flush and nothing pending.
    let quiet = t0 + std::time::Duration::from_millis(500);
    app.observe_title_drift(0, quiet);
    assert_eq!(
        app.title_drift.last_flush.get(&0).copied(),
        flushed_at,
        "an unchanged session must not re-flush — at the debounce ceiling \
         that is 10 whole-strip rebuilds a second, forever"
    );
    assert!(
        !app.title_drift.pending.contains(&0),
        "an unchanged session must not go pending"
    );

    // A window that just started labelling it: drop its per-window epoch cache (what a
    // tab move or open leaves); the gate must act.
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.tab_title_epochs.remove(&0);
    }
    let later = quiet + std::time::Duration::from_millis(500);
    app.observe_title_drift(0, later);
    assert!(
        app.title_drift.last_flush.get(&0).copied() != flushed_at
            || app.title_drift.pending.contains(&0),
        "a window that started labeling this session since the last flush \
         was not refreshed — its tab keeps a stale label"
    );
}

/// A mid-drag reflow (`resize_live_drag`) sizes only the active tab's panes; a background tab
/// defers until the trailing settle or the tab-switch flush `redraw_window` performs. Drives
/// `resize_panes_scoped`, the scope both apply paths route to.
#[test]
fn live_drag_resize_scopes_to_active_tab_then_flushes_on_switch() {
    let mut app = App::headless_for_test(); // window 0 = 24x80, session 0, tab 0
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session(1)); // tab 1 = session 1 (now active)
    app.switch_tab_in(wid, 0); // make tab 0 (session 0) the active tab
    assert_eq!(app.windows[&wid].tabs.active, 0);

    let size = |app: &App, id: u64| {
        let t = super::term_lock(&app.pool.get(id).unwrap().term);
        (t.rows(), t.cols())
    };
    assert_eq!(
        size(&app, 0),
        (24, 80),
        "both tabs start at the window grid"
    );
    assert_eq!(size(&app, 1), (24, 80));

    // Grow the window grid and apply a live-drag tick (active-tab scope).
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.rows = 30;
        ws.cols = 100;
    }
    app.resize_panes_scoped(wid, true);
    assert_eq!(size(&app, 0), (30, 100), "the ACTIVE tab reflows this tick");
    assert_eq!(
        size(&app, 1),
        (24, 80),
        "the BACKGROUND tab is deferred, not reflowed"
    );
    assert!(
        app.windows[&wid].panes_stale,
        "a deferred background tab marks the window panes_stale"
    );

    // Switching to the deferred tab does not reflow it (a plain `switch_tab_in` leaves
    // non-shared panes alone); the scoped flush `redraw_window` runs before presenting it does.
    app.switch_tab_in(wid, 1);
    assert_eq!(
        size(&app, 1),
        (24, 80),
        "switching alone does not reflow a deferred tab"
    );
    app.resize_panes_scoped(wid, true);
    assert_eq!(
        size(&app, 1),
        (30, 100),
        "the tab-switch flush sizes the newly-active tab to the current grid"
    );

    // A non-drag AllTabs pass (the trailing settle / any other caller) clears the
    // stale flag and leaves every tab current.
    app.resize_panes_scoped(wid, false);
    assert!(!app.windows[&wid].panes_stale, "AllTabs clears panes_stale");
}

/// Focus loss scrubs the previous ambient modifier snapshot. Every later Winit
/// report is authoritative, including a fresh pre-`Focused(true)` snapshot;
/// without any later report, the post-focus ordinary `l` remains plain.
#[test]
fn focus_loss_scrubs_stale_modifiers_without_breaking_real_chords() {
    use aterm_models_misc::focus_modifier_cache_model;
    #[cfg(unix)]
    use aterm_session::sink::SinkWriter;
    use aterm_spec::derive::Model;
    use aterm_types::keyboard::{Key, KeyEventType, KeyboardMode, encode_key_with_event};
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{
        Key as WinitKey, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr,
    };
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::sync::Arc;

    fn codex_l_bytes(mods: ModifiersState, event_type: KeyEventType) -> Vec<u8> {
        let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES
            | KeyboardMode::REPORT_EVENT_TYPES
            | KeyboardMode::REPORT_ALTERNATE_KEYS;
        // Lock modifiers as the headless `App` reads them (`keymap::no_lock_modifiers`),
        // so the bytes do not depend on the machine's Caps Lock.
        encode_key_with_event(
            &Key::Character('l'),
            crate::keymap::modifiers_from_winit(mods) | crate::keymap::no_lock_modifiers(),
            mode,
            event_type,
        )
    }

    fn l_event(state: ElementState) -> KeyEvent {
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyL),
            WinitKey::Character(SmolStr::new("l")),
            (state == ElementState::Pressed).then(|| SmolStr::new("l")),
            KeyLocation::Standard,
            state,
            false,
        )
    }

    #[cfg(unix)]
    fn read_pty_bytes(fd: i32) -> Vec<u8> {
        let mut bytes = [0u8; 64];
        // `on_key` returns only after the interactive sink accepted every
        // byte; the observing pipe is empty before each event, so one read
        // returns that complete, sub-PIPE_BUF key frame.
        let read = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        assert!(
            read > 0,
            "real App::on_key produced no observable PTY bytes"
        );
        bytes[..read as usize].to_vec()
    }

    fn assert_conforms(
        model: &Model,
        action: &str,
        prev: &BTreeMap<&'static str, i64>,
        next: &BTreeMap<&'static str, i64>,
    ) {
        let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
            model,
            &[],
            prev,
            next,
            Some(action),
            "App focus-modifier-cache conformance",
        );
        assert!(
            ok,
            "real {action} transition {prev:?} -> {next:?} must conform\n{evidence}"
        );
    }

    let wid = WindowId(0);
    let model = focus_modifier_cache_model();
    #[cfg(unix)]
    let mut pipe = [0; 2];
    #[cfg(unix)]
    assert_eq!(
        unsafe { libc::pipe(pipe.as_mut_ptr()) },
        0,
        "PTY observer pipe"
    );
    #[cfg(unix)]
    let read_flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    #[cfg(unix)]
    assert!(read_flags >= 0, "read pipe flags");
    #[cfg(unix)]
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, read_flags | libc::O_NONBLOCK) },
        0,
        "nonblocking PTY observer"
    );
    #[cfg(unix)]
    let mut app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new(pipe[1])));
    #[cfg(not(unix))]
    let mut app = App::headless_for_test();
    term_lock(&app.pool.get(0).expect("session 0").term).process(b"\x1b[>7u");
    let mut state = project_focus_modifier_cache(&app, wid, 0, 0);
    assert_eq!(
        state,
        model.init_state(),
        "real App/model initial states drifted"
    );

    // A focused Ctrl report is authoritative, but the following focus-loss
    // edge unconditionally clears that previous ambient snapshot.
    app.on_modifiers_changed(wid, ModifiersState::CONTROL);
    let next = project_focus_modifier_cache(&app, wid, 1, 0);
    assert_conforms(&model, "ReportCtrl", &state, &next);
    state = next;
    app.on_focus(wid, false);
    let next = project_focus_modifier_cache(&app, wid, 0, 0);
    assert_conforms(&model, "FocusOut", &state, &next);
    state = next;
    assert_eq!(app.windows[&wid].mods, ModifiersState::empty());

    // With no later report, FocusIn keeps the clean cache and `l` is plain.
    app.on_focus(wid, true);
    let next = project_focus_modifier_cache(&app, wid, 0, 0);
    assert_conforms(&model, "FocusIn", &state, &next);
    state = next;
    app.on_key(wid, l_event(ElementState::Pressed));
    #[cfg(unix)]
    let bytes = read_pty_bytes(pipe[0]);
    #[cfg(not(unix))]
    let bytes = codex_l_bytes(app.windows[&wid].mods, KeyEventType::Press);
    assert_eq!(bytes, b"l");
    let ctrl_press = codex_l_bytes(ModifiersState::CONTROL, KeyEventType::Press);
    let next = project_focus_modifier_cache(&app, wid, 0, i64::from(bytes == ctrl_press));
    assert_conforms(&model, "PressL", &state, &next);
    state = next;
    let physical_l = PhysicalKey::Code(KeyCode::KeyL);
    assert!(
        app.physical_press_owners.contains_key(&physical_l),
        "the real press installs its immutable release owner"
    );

    // Duplicate platform focus events are legal and the reset is idempotent.
    app.on_focus(wid, false);
    let next = project_focus_modifier_cache(&app, wid, 0, 0);
    assert_conforms(&model, "FocusOut", &state, &next);
    state = next;
    assert!(
        app.physical_press_owners.contains_key(&physical_l),
        "focus loss scrubs only ambient modifiers, never physical press ownership"
    );
    app.on_focus(wid, false);
    let next = project_focus_modifier_cache(&app, wid, 0, 0);
    assert_conforms(&model, "FocusOut", &state, &next);
    state = next;

    // Winit can publish the current held modifiers before Focused(true), with
    // no later repair event. Every post-reset snapshot stays authoritative.
    app.on_modifiers_changed(wid, ModifiersState::CONTROL);
    let next = project_focus_modifier_cache(&app, wid, 1, 0);
    assert_conforms(&model, "ReportCtrl", &state, &next);
    state = next;
    assert_eq!(app.windows[&wid].mods, ModifiersState::CONTROL);
    app.on_focus(wid, true);
    let next = project_focus_modifier_cache(&app, wid, 1, 0);
    assert_conforms(&model, "FocusIn", &state, &next);
    state = next;
    assert_eq!(app.windows[&wid].mods, ModifiersState::CONTROL);

    // The plain press remained held across FocusOut and a fresh Ctrl report.
    // Its real release must use the immutable press-time (plain) modifiers,
    // not the now-current Ctrl cache, and must retire ownership exactly once.
    // A text-producing release is byte-silent in Codex mode; the exact Ctrl press
    // read below proves no stray release prefix was queued before that frame.
    app.on_key(wid, l_event(ElementState::Released));
    assert!(
        !app.physical_press_owners.contains_key(&physical_l),
        "the matching release retires the held physical owner"
    );

    // Pin the actual Codex-mode egress after the vendored Windows ordering,
    // not merely the cached bit: no second ModifiersChanged is required.
    app.on_key(wid, l_event(ElementState::Pressed));
    #[cfg(unix)]
    let bytes = read_pty_bytes(pipe[0]);
    #[cfg(not(unix))]
    let bytes = codex_l_bytes(app.windows[&wid].mods, KeyEventType::Press);
    let ctrl_press = codex_l_bytes(ModifiersState::CONTROL, KeyEventType::Press);
    assert_eq!(
        bytes, b"\x1b[108;5u",
        "headless: no lock bits, so the byte pin is exact"
    );
    let next = project_focus_modifier_cache(&app, wid, 1, i64::from(bytes == ctrl_press));
    assert_conforms(&model, "PressL", &state, &next);
    state = next;
    assert!(app.physical_press_owners.contains_key(&physical_l));
    app.on_key(wid, l_event(ElementState::Released));
    #[cfg(unix)]
    let bytes = read_pty_bytes(pipe[0]);
    #[cfg(not(unix))]
    let bytes = codex_l_bytes(ModifiersState::CONTROL, KeyEventType::Release);
    assert_eq!(
        bytes, b"\x1b[108;5:3u",
        "real release keeps the same authoritative Ctrl modifier"
    );
    assert!(!app.physical_press_owners.contains_key(&physical_l));

    // A genuine focused Ctrl-up remains authoritative too.
    app.on_modifiers_changed(wid, ModifiersState::empty());
    let next = project_focus_modifier_cache(&app, wid, 0, 0);
    assert_conforms(&model, "ReportNone", &state, &next);
    state = next;

    // Negative control: preload Ctrl again, then prove a focus-loss transition
    // retaining it is rejected by the model.
    app.on_modifiers_changed(wid, ModifiersState::CONTROL);
    let next = project_focus_modifier_cache(&app, wid, 1, 0);
    assert_conforms(&model, "ReportCtrl", &state, &next);
    state = next;
    let mut stale_focus_out = project_focus_modifier_cache(&app, wid, 0, 0);
    stale_focus_out.insert("focused", 0);
    stale_focus_out.insert("cached_ctrl", 1);
    let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &state,
        &stale_focus_out,
        Some("FocusOut"),
        "App focus-modifier-cache negative control",
    );
    assert!(
        !ok,
        "stale Ctrl retained by FocusOut must be rejected\n{evidence}"
    );

    #[cfg(unix)]
    {
        drop(app);
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }
}

/// Attach-time focus adoption: a successful attach takes the OS window's key state
/// before the first frame with no `Focused` event; a surfaceless window keeps its
/// declared focus.
#[test]
fn attached_window_adopts_os_focus_without_waiting_for_an_event() {
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    let model = aterm_models_misc::focus_modifier_cache_model();
    let before = project_focus_modifier_cache(&app, wid, 0, 0);
    assert!(app.windows[&wid].focused);

    // No OS surface means controller-declared focus, including an explicit
    // focus-out. Attaching an already-key window also needs no synthetic edge.
    app.adopt_initial_focus(wid, None);
    app.adopt_initial_focus(wid, Some(true));
    assert_eq!(project_focus_modifier_cache(&app, wid, 0, 0), before);

    // This is the successful CPU/GPU attach tail. No Focused event follows:
    // a background/handoff window must still demote its first presentation.
    app.adopt_initial_focus(wid, Some(false));
    let after = project_focus_modifier_cache(&app, wid, 0, 0);
    let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &after,
        Some("FocusOut"),
        "initial OS focus",
    );
    assert!(ok, "{evidence}");
    assert!(!app.windows[&wid].focused);
    assert!(app.focus_reported.is_none());
    assert!(!app.cursor_fx_focus(wid, app.windows[&wid].focused, Instant::now()));
    app.adopt_initial_focus(wid, None);
    assert_eq!(project_focus_modifier_cache(&app, wid, 0, 0), after);

    // Negative control: keeping the constructor seed cannot satisfy the OS blur transition.
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &before,
        Some("FocusOut"),
        "stale initial focus",
    );
    assert!(!ok);
    app.on_focus(wid, true);
    assert!(app.windows[&wid].focused, "normal later focus still works");
    // A declared-unfocused window that attaches key is promoted at once.
    app.on_focus(wid, false);
    app.adopt_initial_focus(wid, Some(true));
    assert!(app.windows[&wid].focused, "attached key: promoted");
}

/// winit's own order replayed through [`App::window_focus_event`]: a creation-time
/// `Focused(false)` after attach, then `Focused(true)` when the window becomes key.
/// The blur changes nothing, the key edge leaves the window focused holding the DEC
/// 1004 claim, and a window never made key stays demoted.
#[test]
fn winits_creation_time_blur_then_key_ends_focused_with_one_claim() {
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    app.frontmost_window = Some(wid);
    app.adopt_initial_focus(wid, Some(false));
    let demoted = project_focus_modifier_cache(&app, wid, 0, 0);
    assert!(!app.windows[&wid].focused, "demoted before its first frame");
    assert_eq!(app.focus_report_holder(), None);

    app.window_focus_event(wid, false);
    assert_eq!(
        project_focus_modifier_cache(&app, wid, 0, 0),
        demoted,
        "winit's creation-time blur changes nothing"
    );

    app.window_focus_event(wid, true);
    assert!(app.windows[&wid].focused, "the key edge focuses it");
    let holder = app.focus_report_holder();
    assert!(holder.is_some(), "the window holds the DEC-1004 claim");
    assert_eq!(app.focus_reported, holder, "…and the account says so, once");
    assert!(app.cursor_fx_focus(wid, true, Instant::now()));

    // Never made key: the creation-time blur alone.
    let mut behind = App::headless_for_test();
    behind.frontmost_window = Some(wid);
    behind.adopt_initial_focus(wid, Some(false));
    behind.window_focus_event(wid, false);
    assert!(!behind.windows[&wid].focused);
    assert_eq!(behind.focus_report_holder(), None);
    assert!(!behind.cursor_fx_focus(wid, false, Instant::now()));
}

/// Focus loss during a held Left-button drag resets window-local gesture state
/// (`divider_drag`, `selecting`), since the release may never arrive; keyboard press
/// ownership and physical tracking (`held_mouse_button`/`reported_buttons`) are kept so
/// a returning release still pairs.
#[test]
fn on_focus_loss_clears_stuck_pointer_gesture() {
    let mut app = App::headless_for_test(); // window 0 = 24x80, session 0
    let new_sid = app.split_active_stub_tab(WindowId(0)); // divider @ col 40
    let wid = WindowId(0);

    // Arm a divider drag (press on the divider column 40 of the fresh 50/50 split)
    // and pile on the rest of the in-flight gesture state a real drag could hold.
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_mouse_window_cell = (5, 40);
    }
    assert!(
        app.begin_divider_drag(wid),
        "press on the divider arms a drag"
    );
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.selecting = true;
        ws.gesture = Some(GestureOrigin {
            row: 0,
            end_row: 0,
            start_col: 0,
            end_col: 5,
            kind: SelectionType::Semantic,
        });
        ws.next_autoscroll = Some(Instant::now());
        ws.hover_pointer = true;
        // Physical/tracking state that must survive the focus round-trip.
        ws.held_mouse_button = Some(aterm_types::mouse::MouseButton::Left);
        ws.reported_buttons = 1;
    }
    assert!(app.windows[&wid].divider_drag.is_some());

    // Focus is stolen and the Left-release never comes back to this window.
    app.on_focus(wid, false);

    let ws = &app.windows[&wid];
    assert!(
        ws.divider_drag.is_none(),
        "stuck divider drag dropped on focus loss"
    );
    assert!(!ws.selecting, "stuck selection dropped on focus loss");
    assert!(ws.gesture.is_none(), "gesture origin dropped on focus loss");
    assert!(
        ws.next_autoscroll.is_none(),
        "autoscroll disarmed on focus loss"
    );
    assert!(!ws.hover_pointer, "hover pointer reset on focus loss");
    // Physical/mouse-tracking state is intentionally preserved.
    assert_eq!(
        ws.held_mouse_button,
        Some(aterm_types::mouse::MouseButton::Left),
        "physical held-button state is NOT cleared"
    );
    assert_eq!(
        ws.reported_buttons, 1,
        "reported-button mask is NOT cleared"
    );
    let _ = new_sid;
    assert!(app.structural_invariants_ok());
}

/// The `Focused` arm reaches the ribbon's ember: a typed key lays a lit cell, the window
/// blurs inside its typed wake (amplitude stays up), and the body is fading half-way
/// through the ember and gone at its end.
#[test]
fn on_focus_loss_embers_the_rainbow_ribbon_instead_of_cutting_it() {
    use std::time::{Duration, Instant};

    use crate::app_render::CursorFxInputs;
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    let peak = |app: &App| -> u8 {
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .under_quads()
            .iter()
            .map(|q| q.alpha)
            .max()
            .unwrap_or(0)
    };
    let t0 = Instant::now();
    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 2));
    app.cursor_frame(wid, seed).expect("seed cursor engines");
    // One typed key, echoed one cell forward: the ribbon lays its cell.
    let typed = t0 + Duration::from_millis(1);
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.last_key_at = Some(typed);
        ws.fx.cursor_mut().cadence.on_keystroke(typed);
        ws.fx.cursor_mut().glow.note_typed(typed);
    }
    let mut live = CursorFxInputs::sample_for_test(t0 + Duration::from_millis(2));
    live.cur = Some((2, 3));
    app.cursor_frame(wid, live).expect("live cursor tick");
    assert!(
        peak(&app) > 0,
        "precondition: the typed key laid a lit ribbon cell"
    );
    // Baseline on the settled cell: one frame after the key the cell is still inside its
    // 18 ms `edge-in` attack, so settle it first.
    let mut settled = CursorFxInputs::sample_for_test(t0 + Duration::from_millis(40));
    settled.cur = Some((2, 3));
    app.cursor_frame(wid, settled).expect("the attack settles");
    let full = peak(&app);

    // The blur, on the arm's own clock (`Instant::now()` inside it).
    app.on_focus(wid, false);
    let blurred = Instant::now();
    assert!(!app.windows[&wid].focused);
    let mut mid = CursorFxInputs::sample_for_test(blurred + Duration::from_millis(150));
    mid.cur = Some((2, 3));
    app.cursor_frame(wid, mid)
        .expect("the typed wake keeps the frame");
    let half = peak(&app);
    assert!(
        half > 0 && half < full,
        "half-way through the ember the body is spending ({full} -> {half}), not cut and not held"
    );
    let mut end = CursorFxInputs::sample_for_test(blurred + Duration::from_millis(320));
    end.cur = Some((2, 3));
    app.cursor_frame(wid, end)
        .expect("the typed wake keeps the frame");
    assert_eq!(peak(&app), 0, "at the ember's end the body is exactly gone");
    assert!(
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .v2_ribbon()
            .is_some_and(|r| r.at_rest()),
        "…and the pool is empty, the engine still owning the frame"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn on_focus_loss_pauses_collection_hello_before_any_redraw() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    let Some(ws) = app.windows.get_mut(&wid) else {
        panic!("headless fixture has window 0");
    };
    ws.fx
        .companions_mut()
        .cursor_cat
        .on_collect(t0, aterm_effects::kitty_registry::KittyLook::default());
    assert!(
        app.windows[&wid]
            .fx
            .companions()
            .cursor_cat
            .static_deadline(t0)
            .is_some()
    );

    // No redraw is driven after this event. The event handler itself must
    // freeze the promise rather than relying on a compositor callback.
    app.on_focus(wid, false);
    let much_later = t0 + std::time::Duration::from_secs(30);
    assert_eq!(
        app.windows[&wid]
            .fx
            .companions()
            .cursor_cat
            .static_deadline(much_later),
        None,
        "an unfocused hello arms no wall-clock wake"
    );
    let Some(ws) = app.windows.get_mut(&wid) else {
        panic!("headless fixture retains window 0");
    };
    assert!(
        ws.fx
            .companions_mut()
            .cursor_cat
            .static_frame(much_later)
            .collection_hello,
        "with no redraw delivered, the hello remains protected"
    );
}

#[test]
fn pane_and_tab_owner_switches_retire_only_coordinate_bound_companions() {
    for pane_switch in [true, false] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let now = Instant::now();
        let model = aterm_models_effects::cursor_companion_owner_lifecycle_model();
        if pane_switch {
            app.split_active_stub_tab(wid);
        }
        let old_session = app.focused_session_id(wid).expect("focused terminal");
        app.windows
            .get_mut(&wid)
            .expect("test window")
            .fx
            .companion_mut()
            .set_species(aterm_effects::kitty_pet::PetSpecies::Dog);
        let cold = project_cursor_companion_owner_lifecycle(&model, &app, wid, 0);
        materialize_resident_pet(&mut app, wid, now);
        let materialized = project_cursor_companion_owner_lifecycle(&model, &app, wid, 1);
        assert_cursor_companion_owner_transition(&model, "Materialize", &cold, &materialized);
        earn_flying_cursor_cat(&mut app, wid, now);
        {
            let ws = app.windows.get_mut(&wid).expect("test window");
            // Baseline the pet's completion latch on the old stream (seq 7); the owner
            // edge must forget it.
            let seeded = crate::app_render::pet_prepare_for_test(
                ws,
                now,
                old_session,
                Some((4, 12)),
                Some((7, 0, None)),
            );
            let _ = ws.fx.companion_mut().tick(seeded);
        }

        if pane_switch {
            // The production pointer-focus path orders tree focus, tab-model
            // sync, pane-local pointer rebasing, then canonical sync_window.
            app.windows
                .get_mut(&wid)
                .expect("test window")
                .last_mouse_window_cell = (5, 10);
            assert!(app.focus_pane_under_pointer(wid));
        } else {
            let sid = app.next_session_id;
            app.push_stub_tab(wid, stub_session(sid));
        }

        let ws = &app.windows[&wid];
        let retired = project_cursor_companion_owner_lifecycle(
            &model,
            &app,
            wid,
            if pane_switch { 2 } else { 3 },
        );
        assert_cursor_companion_owner_transition(
            &model,
            if pane_switch {
                "PaneOwnerSwitch"
            } else {
                "TabOwnerSwitch"
            },
            &materialized,
            &retired,
        );
        assert_ne!(app.focused_session_id(wid), Some(old_session));
        assert!(
            !ws.fx.companion().brain().is_active() && !ws.fx.companion().brain().needs_frames(),
            "{} switch retires the old resident coordinates",
            if pane_switch { "pane" } else { "tab" }
        );
        assert_eq!(
            ws.fx.companion().brain().species(),
            aterm_effects::kitty_pet::PetSpecies::Dog,
            "durable pet identity survives the owner edge"
        );
        assert!(ws.fx.companion().hit_rect().is_none());
        assert!(
            !ws.fx.companions().cursor_cat.is_active(),
            "an ordinary cursor-owned flight cannot jump to the new owner"
        );
        // The owner edge forgot the old stream's latch: its next completion is a silent
        // baseline (without the edge, seq 8 would read as a completion).
        let ws = app.windows.get_mut(&wid).expect("test window");
        let probe =
            crate::app_render::pet_prepare_for_test(ws, now, old_session, None, Some((8, 1, None)));
        assert_eq!(
            probe.completion, None,
            "the old stream's completion replayed"
        );
        let _ = ws.fx.companion_mut().tick(probe);
    }

    // Negative control: the flying cat's collection promise is owner-independent; the
    // pane edge grounds its placement but must not consume that appearance.
    let mut promised = App::headless_for_test();
    let wid = WindowId(0);
    promised.split_active_stub_tab(wid);
    let now = Instant::now();
    {
        let cat = &mut promised
            .windows
            .get_mut(&wid)
            .expect("test window")
            .fx
            .companions_mut()
            .cursor_cat;
        cat.on_collect(now, aterm_effects::kitty_registry::KittyLook::default());
        cat.on_motion_pulse(aterm_effects::cursor_glow::CursorCatMotionPulse {
            at: now,
            kind: aterm_effects::cursor_glow::CursorCatMotionKind::FoldForward,
        });
        assert!(
            cat.placement_frame(now).fold.is_some(),
            "negative control: the promised body owns stale outgoing placement"
        );
    }
    promised
        .windows
        .get_mut(&wid)
        .expect("test window")
        .last_mouse_window_cell = (5, 10);
    assert!(promised.focus_pane_under_pointer(wid));
    let frame = promised
        .windows
        .get_mut(&wid)
        .expect("test window")
        .fx
        .companions_mut()
        .cursor_cat
        .static_frame(now);
    assert!(frame.collection_hello && frame.alpha > 0);
    assert!(
        promised.windows[&wid]
            .fx
            .companions()
            .cursor_cat
            .placement_frame(now)
            .fold
            .is_none(),
        "the promise survives, but its old-owner fold is re-anchored"
    );
}

#[test]
fn screen_buffer_switches_retire_coordinates_and_preserve_companion_identity_and_song() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("focused terminal");
    let term = app.pool.get(session).expect("pooled terminal").term.clone();
    let model = aterm_models_effects::cursor_companion_owner_lifecycle_model();
    let now = Instant::now();

    let (terminal_id, initial_alt) = {
        let term = term_lock(&term);
        (term.render_identity(), term.is_alternate_screen())
    };
    assert!(!initial_alt, "fixture begins on the main screen");
    assert!(!crate::app_render::sync_cursor_effect_coordinate_space(
        app.windows.get_mut(&wid).expect("test window"),
        terminal_id,
        initial_alt,
    ));

    // Song/note state is mouth-relative, not terminal-grid-relative. It is
    // a durable companion promise and must survive both screen edges.
    {
        let ws = app.windows.get_mut(&wid).expect("test window");
        let repeats = aterm_effects::kitty_sing::SING_ARM_REPEATS;
        for i in 0..repeats {
            ws.fx.companions_mut().kitty_sing.note_char(
                now + Duration::from_millis(u64::from(i)),
                session,
                'a',
            );
        }
        let song_now = now + Duration::from_millis(u64::from(repeats));
        assert!(ws.fx.companions_mut().kitty_sing.is_armed(song_now));
        let c = ws.fx.companions_mut();
        c.music_notes
            .update(song_now, true, c.kitty_sing.beat(song_now));
        assert!(ws.fx.companions_mut().music_notes.is_active());
        ws.fx.companions_mut().sing_riff_bar = Some(7);
    }

    for (sequence, expected_alt) in [
        (b"\x1b[?1049h\x1b[20;70H".as_slice(), true),
        (b"\x1b[?1049l".as_slice(), false),
    ] {
        materialize_resident_pet(&mut app, wid, now);
        earn_flying_cursor_cat(&mut app, wid, now);
        {
            let ws = app.windows.get_mut(&wid).expect("test window");
            ws.fx.companions_mut().cursor_cat.on_motion_pulse(
                aterm_effects::cursor_glow::CursorCatMotionPulse {
                    at: now,
                    kind: aterm_effects::cursor_glow::CursorCatMotionKind::FoldForward,
                },
            );
            assert!(
                ws.fx
                    .companions_mut()
                    .cursor_cat
                    .placement_frame(now)
                    .fold
                    .is_some()
            );
            // Baseline the pet's completion latch on the old stream (seq 7); the screen
            // edge must forget it.
            let seeded = crate::app_render::pet_prepare_for_test(
                ws,
                now,
                session,
                Some((4, 12)),
                Some((7, 0, None)),
            );
            let _ = ws.fx.companion_mut().tick(seeded);
        }
        let materialized = project_cursor_companion_owner_lifecycle(&model, &app, wid, 1);

        term_lock(&term).process(sequence);
        let (next_terminal_id, next_alt) = {
            let term = term_lock(&term);
            (term.render_identity(), term.is_alternate_screen())
        };
        assert_eq!(next_terminal_id, terminal_id, "same terminal identity");
        assert_eq!(next_alt, expected_alt, "screen edge reached");
        assert!(crate::app_render::sync_cursor_effect_coordinate_space(
            app.windows.get_mut(&wid).expect("test window"),
            next_terminal_id,
            next_alt,
        ));

        let retired = project_cursor_companion_owner_lifecycle(&model, &app, wid, 7);
        assert_cursor_companion_owner_transition(
            &model,
            "ScreenBufferSwitch",
            &materialized,
            &retired,
        );
        let ws = &app.windows[&wid];
        assert!(
            !ws.fx.companion().brain().is_active() && !ws.fx.companion().brain().needs_frames()
        );
        assert_eq!(
            ws.fx.companion().brain().species(),
            aterm_effects::kitty_pet::PetSpecies::Dog,
            "durable pet identity survives the screen edge"
        );
        assert!(ws.fx.companion().hit_rect().is_none());
        assert!(!ws.fx.companions().cursor_cat.is_active());
        assert!(
            ws.fx
                .companions()
                .cursor_cat
                .placement_frame(now)
                .fold
                .is_none()
        );
        assert!(
            ws.fx
                .companions()
                .kitty_sing
                .is_armed(now + Duration::from_millis(20))
        );
        assert!(ws.fx.companions().music_notes.is_active());
        assert_eq!(ws.fx.companions().sing_riff_bar, Some(7));
        // The owner edge forgot the old stream's latch: its next completion is a silent
        // baseline (without the edge, seq 8 would read as a completion).
        let ws = app.windows.get_mut(&wid).expect("test window");
        let probe =
            crate::app_render::pet_prepare_for_test(ws, now, session, None, Some((8, 1, None)));
        assert_eq!(
            probe.completion, None,
            "the old stream's completion replayed"
        );
        let _ = ws.fx.companion_mut().tick(probe);
    }
}

#[test]
fn focus_loss_retires_pet_only_when_cursor_effects_are_unpresentable() {
    let wid = WindowId(0);
    let now = Instant::now();
    let model = aterm_models_effects::cursor_companion_owner_lifecycle_model();

    let mut unwatched = App::headless_for_test();
    unwatched
        .windows
        .get_mut(&wid)
        .expect("test window")
        .fx
        .companion_mut()
        .set_species(aterm_effects::kitty_pet::PetSpecies::Dog);
    let cold = project_cursor_companion_owner_lifecycle(&model, &unwatched, wid, 0);
    materialize_resident_pet(&mut unwatched, wid, now);
    let materialized = project_cursor_companion_owner_lifecycle(&model, &unwatched, wid, 1);
    assert_cursor_companion_owner_transition(&model, "Materialize", &cold, &materialized);
    unwatched.on_focus(wid, false);
    let retired = project_cursor_companion_owner_lifecycle(&model, &unwatched, wid, 4);
    assert_cursor_companion_owner_transition(
        &model,
        "UnpresentableFocusLoss",
        &materialized,
        &retired,
    );
    let ws = &unwatched.windows[&wid];
    assert!(!ws.fx.companion().brain().is_active() && !ws.fx.companion().brain().needs_frames());
    assert!(ws.fx.companion().hit_rect().is_none());

    // A recent PTY-bound key is the renderer's synthetic focus pin. Raw
    // OS focus loss must not retire the body while this same input still
    // makes cursor effects presentable.
    let mut typed = App::headless_for_test();
    typed
        .windows
        .get_mut(&wid)
        .expect("test window")
        .fx
        .companion_mut()
        .set_species(aterm_effects::kitty_pet::PetSpecies::Dog);
    let cold = project_cursor_companion_owner_lifecycle(&model, &typed, wid, 0);
    materialize_resident_pet(&mut typed, wid, now);
    let materialized = project_cursor_companion_owner_lifecycle(&model, &typed, wid, 1);
    assert_cursor_companion_owner_transition(&model, "Materialize", &cold, &materialized);
    typed
        .windows
        .get_mut(&wid)
        .expect("test window")
        .last_key_at = Some(now);
    typed.on_focus(wid, false);
    let pinned = project_cursor_companion_owner_lifecycle(&model, &typed, wid, 5);
    assert_cursor_companion_owner_transition(&model, "TypedWakeFocusLoss", &materialized, &pinned);
    let ws = &typed.windows[&wid];
    assert!(ws.fx.companion().brain().is_active());
    assert!(ws.fx.companion().hit_rect().is_some());
    // Negative control: the over-repair (`Buggy=1`) on this real window, retiring on the
    // raw blur by rebuilding the brain and dropping its hit target. Its projection is
    // the mutant's successor, which the healthy transition refuses.
    typed
        .windows
        .get_mut(&wid)
        .expect("test window")
        .fx
        .companion_mut()
        .drain();
    let rebuilt = project_cursor_companion_owner_lifecycle(&model, &typed, wid, 5);
    assert_eq!(
        aterm_spec::interp::with_buggy(&model, 1).successors("TypedWakeFocusLoss", &materialized),
        vec![rebuilt.clone()],
        "the replayed rebuild is the Buggy=1 focus loss"
    );
    assert!(
        !model
            .successors("TypedWakeFocusLoss", &materialized)
            .contains(&rebuilt)
    );
    assert!(!model.check_invariant("PresentationPinsPreserveTheSighting", &rebuilt));
    assert!(!model.check_invariant("DurableIdentitySurvives", &rebuilt));
}

#[test]
fn recording_pin_preserves_pet_across_raw_focus_loss() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = Instant::now();
    let model = aterm_models_effects::cursor_companion_owner_lifecycle_model();
    app.windows
        .get_mut(&wid)
        .expect("test window")
        .fx
        .companion_mut()
        .set_species(aterm_effects::kitty_pet::PetSpecies::Dog);
    let cold = project_cursor_companion_owner_lifecycle(&model, &app, wid, 0);
    materialize_resident_pet(&mut app, wid, now);
    let materialized = project_cursor_companion_owner_lifecycle(&model, &app, wid, 1);
    assert_cursor_companion_owner_transition(&model, "Materialize", &cold, &materialized);

    let root = std::env::temp_dir().join(format!(
        "aterm-pet-focus-video-{}-{}",
        std::process::id(),
        crate::metrics::now_us()
    ));
    let _ = std::fs::remove_dir_all(&root);
    crate::control_auth::ensure_private_dir(&root).expect("private recording root");
    let dir = crate::control_auth::confine_video_dir(&root).expect("confined recording dir");
    let (reply, _rx) = std::sync::mpsc::channel();
    app.video_rec = Some(super::VideoRec {
        window: wid,
        deadline: now + Duration::from_secs(3),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        mode: super::VideoMode::OffscreenPresentReal,
        next_frame: None,
        presented: None,
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        dir,
        handoff: None,
        cancel: super::VideoCancellation::new(),
        reply,
    });

    app.on_focus(wid, false);
    let pinned = project_cursor_companion_owner_lifecycle(&model, &app, wid, 6);
    assert_cursor_companion_owner_transition(&model, "RecordingFocusLoss", &materialized, &pinned);
    let ws = &app.windows[&wid];
    assert!(ws.fx.companion().brain().is_active());
    assert!(ws.fx.companion().hit_rect().is_some());

    let _ = app.video_rec.take();
    let _ = std::fs::remove_dir_all(root);
}

/// `close_active_tab` returns the frontmost window whose last tab it closed, so the
/// escalation flags that window, not the event-stamped one.
#[test]
fn close_active_last_tab_reports_the_frontmost_window() {
    let mut app = App::headless_for_test(); // window 0 (single tab), session 0
    // A 2nd window; window 0 stays single-tab and is made the front.
    let s1 = app.next_session_id;
    let w1 = app.insert_logical_window(stub_session(s1), 24, 80);
    app.frontmost_window = Some(WindowId(0));
    app.sync_active_session();
    assert_eq!(
        app.close_active_tab(),
        Some(WindowId(0)),
        "the LAST tab of the frontmost window → escalate-close THAT window",
    );
    let s2 = app.next_session_id;
    app.push_stub_tab(w1, stub_session(s2)); // w1 now has 2 tabs
    app.frontmost_window = Some(w1);
    app.sync_active_session();
    assert_eq!(
        app.close_active_tab(),
        None,
        "closing one of several tabs does not escalate"
    );
}

/// The GPU-attach-failure rollback of "Move Tab to New Window" returns the tab to the
/// source window instead of closing the new one (which would drop the session's only
/// view and kill the shell).
#[test]
fn detach_rollback_returns_the_moved_session_no_data_loss() {
    let mut app = App::headless_for_test(); // window 0, session 0
    // A 2nd tab on window 0 so a detach is allowed (source needs >1 tab).
    let s1 = app.next_session_id;
    app.push_stub_tab(WindowId(0), stub_session(s1)); // tabs [0, s1], active=s1
    let pool_before = app.pool.iter().count();

    // Detach the active tab (s1) into a new window B, a pure view-move: s1 is now B's only
    // view.
    let wid_b = app
        .detach_active_tab_logical()
        .expect("detach moves the active tab to B");

    // Simulate the GPU-attach failure → rollback.
    app.detach_rollback_logical(Some(WindowId(0)), wid_b);

    assert!(
        app.pool.get(s1).is_some(),
        "rollback PRESERVES the moved session — no data loss"
    );
    assert_eq!(
        app.pool.iter().count(),
        pool_before,
        "rollback detached/dropped nothing"
    );
    assert!(
        !app.windows.contains_key(&wid_b),
        "the failed new window is dropped"
    );
    assert_eq!(app.windows.len(), 1, "back to one window");
    assert!(
        app.windows
            .get(&WindowId(0))
            .is_some_and(|ws| ws.layouts.iter().any(|t| t.contains(s1))),
        "the tab is back in the source window",
    );
    assert_eq!(app.frontmost_window, Some(WindowId(0)));
    assert!(app.structural_invariants_ok());
}

/// A shared session is sized to the minimum of its foreground (active-tab) viewers; a
/// background-tab viewer does not shrink it.
#[test]
fn shared_target_geometry_uses_foreground_viewers_only() {
    let mut app = App::headless_for_test(); // window 0 = 24x80, session 0
    let bwid = app
        .open_active_session_in_new_window_logical()
        .expect("share session 0 into window B");
    assert_eq!(app.pool.views(0), Some(2));
    // Simulate a drag-resize of B to a larger size.
    if let Some(ws) = app.windows.get_mut(&bwid) {
        ws.rows = 60;
        ws.cols = 200;
    }
    assert_eq!(
        app.shared_target_geometry(0),
        (24, 80),
        "both foreground → min"
    );

    // Background S in window 0 (a new tab there becomes active).
    let s2 = app.next_session_id;
    app.push_stub_tab(WindowId(0), stub_session(s2));
    assert_eq!(
        app.shared_target_geometry(0),
        (60, 200),
        "a background-tab viewer is ignored; the foreground viewer's size wins",
    );
}

/// `App::move_tab` (behind `tab move <from> <to>` and the native mouse drag) permutes the
/// window's `layouts` and fixes `tabs.active` so the same session stays selected, keeping
/// `count == layouts.len()` and `active < count`.
#[test]
fn move_tab_reorders_and_tracks_active() {
    let mut app = App::headless_for_test();
    let s1 = app.next_session_id;
    app.push_stub_tab(WindowId(0), stub_session(s1));
    let s2 = app.next_session_id;
    app.push_stub_tab(WindowId(0), stub_session(s2));
    let order = |app: &App| {
        app.windows[&WindowId(0)]
            .layouts
            .iter()
            .map(|t| t.focus())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&app), vec![0, s1, s2], "three tabs in append order");
    assert_eq!(app.windows[&WindowId(0)].tabs.active, 2);

    // Move the active tab (index 2) to the front (index 0): the session follows,
    // so active becomes 0 and the order rotates s2 to the front.
    app.move_tab(WindowId(0), 2, 0);
    assert_eq!(
        order(&app),
        vec![s2, 0, s1],
        "active tab moved to the front"
    );
    assert_eq!(
        app.windows[&WindowId(0)].tabs.active,
        0,
        "active follows the moved tab"
    );
    assert!(app.structural_invariants_ok());

    // Move a tab from after the active to before it: active shifts up one (the
    // viewed session is unchanged). Order is [s2,0,s1]; move index 2 (s1) to 0.
    app.move_tab(WindowId(0), 2, 0);
    assert_eq!(order(&app), vec![s1, s2, 0]);
    assert_eq!(
        app.windows[&WindowId(0)].tabs.active,
        1,
        "active shifted to follow s2"
    );

    // Out-of-range / identity moves are no-ops.
    let snapshot = order(&app);
    let active_before = app.windows[&WindowId(0)].tabs.active;
    app.move_tab(WindowId(0), 1, 1); // identity
    app.move_tab(WindowId(0), 9, 0); // from out of range
    app.move_tab(WindowId(0), 0, 9); // to out of range
    assert_eq!(order(&app), snapshot, "no-op moves leave order unchanged");
    assert_eq!(app.windows[&WindowId(0)].tabs.active, active_before);
    assert!(app.structural_invariants_ok());
}

/// The global control `ActiveHandle` follows the front window's active tab across an
/// append and a close, not just the per-window mirror.
#[test]
fn active_handle_follows_front_tab_append_and_close() {
    let mut app = App::headless_for_test();
    let active = |app: &App| {
        app.active_handle
            .lock()
            .unwrap()
            .as_ref()
            .expect("front terminal")
            .id
    };
    assert_eq!(active(&app), 0, "starts on session 0");

    // Append a tab on the front window → the global handle follows to it.
    let s1 = app.next_session_id;
    app.push_stub_tab(WindowId(0), stub_session(s1));
    assert_eq!(
        app.windows[&WindowId(0)].tabs.active,
        1,
        "new tab is active"
    );
    assert_eq!(
        active(&app),
        s1,
        "global handle follows the appended front tab"
    );

    // Close the active tab (Cmd-W) → switches to tab 0; the handle follows back.
    app.close_active_tab();
    assert_eq!(
        app.windows[&WindowId(0)].tabs.active,
        0,
        "switched back to tab 0"
    );
    assert_eq!(
        active(&app),
        0,
        "global handle follows the close-induced switch"
    );
    assert!(app.structural_invariants_ok());
}

/// Closing the focused pane of a split tab re-points the global handle at the surviving
/// sibling; so do the split itself and every pane-focus move (`focus_pane_in`,
/// click-to-focus), so `front_routed` answers for the pane on screen.
#[test]
fn active_handle_follows_split_pane_close() {
    let mut app = App::headless_for_test();
    let active = |app: &App| {
        app.active_handle
            .lock()
            .unwrap()
            .as_ref()
            .expect("front terminal")
            .id
    };
    // Split the front window's active tab: a new (focused) pane, session s1.
    let s1 = app.split_active_stub_tab(WindowId(0));
    assert_eq!(
        active(&app),
        s1,
        "the split's new, focused pane is the global active"
    );
    // A pane-focus move each way: the handle follows the focus.
    assert!(app.focus_pane_in(WindowId(0), crate::pane::FocusDir::Left));
    assert_eq!(active(&app), 0, "handle follows a pane-focus move");
    assert!(app.focus_pane_in(WindowId(0), crate::pane::FocusDir::Right));
    assert_eq!(active(&app), s1, "…and back");

    // Close the focused pane → collapses onto the sibling (session 0), which
    // becomes focused; the global handle must follow it, not stay on closed s1.
    app.close_active_tab();
    assert_eq!(
        active(&app),
        0,
        "handle follows to the surviving sibling, not the closed pane",
    );
    assert!(app.structural_invariants_ok());
}
