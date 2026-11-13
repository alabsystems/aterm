// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{PointerAction, WheelDir, press_selection_kind};
use aterm_core::selection::{SelectionSide, SelectionType};

#[test]
fn a_still_pointer_keeps_what_it_saw_and_a_press_goes_where_it_looked() {
    use super::NativePointerRest as Rest;
    use crate::native_ui::UiKey;
    use crate::tab_model::ViewId;
    let (view, other) = (ViewId::from_stored(1), ViewId::from_stored(2));
    let row = UiKey::new("harness/session/9/autopilot/land");
    let next = UiKey::new("harness/session/10/autopilot/land");
    let bulk = UiKey::new("harness/session/all/autopilot/land");
    let at = (10.0, 20.0);
    let rest = Rest::after_motion(None, at, Some((view, &row, false)));
    let still = Rest::after_motion(rest.clone(), at, Some((view, &bulk, true)));
    assert_eq!(still, rest, "the same pixel again is no motion");
    assert!(!Rest::looked_at(still.as_ref(), at, (view, &bulk, true)));
    assert!(Rest::looked_at(still.as_ref(), at, (view, &row, false)));
    assert!(
        Rest::looked_at(still.as_ref(), at, (view, &next, false)),
        "rows as before"
    );
    let lit = Rest::on(at, (view, &bulk, true), None);
    assert!(
        Rest::looked_at(Some(&lit), at, (view, &bulk, true)),
        "the next press takes it"
    );
    let on_bulk = Rest::after_motion(None, at, Some((view, &bulk, true)));
    assert!(!Rest::looked_at(on_bulk.as_ref(), at, (view, &row, false)));
    assert!(!Rest::looked_at(on_bulk.as_ref(), at, (other, &bulk, true)));
    assert!(Rest::looked_at(on_bulk.as_ref(), at, (view, &bulk, true)));
    let lit = Rest::on(at, (view, &row, false), None);
    assert!(
        Rest::looked_at(Some(&lit), at, (view, &row, false)),
        "the next press takes it"
    );
    let nothing = Rest::after_motion(None, at, None);
    assert!(!Rest::looked_at(nothing.as_ref(), at, (view, &bulk, true)));
    assert!(Rest::looked_at(nothing.as_ref(), at, (view, &row, false)));
    assert!(
        Rest::looked_at(None, at, (view, &bulk, true)),
        "a scroll forgot it"
    );
    let moved = Rest::after_motion(still, (11.0, 20.0), Some((view, &bulk, true)));
    assert!(Rest::looked_at(
        moved.as_ref(),
        (11.0, 20.0),
        (view, &bulk, true)
    ));
    assert!(Rest::looked_at(
        moved.as_ref(),
        (12.0, 20.0),
        (view, &row, false)
    ));
}

#[test]
fn a_glassless_mixed_tab_s_native_leaf_takes_the_pointer() {
    use super::NativePointerTarget;
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorStyles));
    let _ = app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let native_view = app
        .active_visible_leaf_plan(wid)
        .expect("a mixed plan")
        .leaves
        .iter()
        .find(|leaf| !leaf.focused)
        .expect("the settings leaf")
        .view;
    assert!(
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.tab_set.active_mut())
            .is_some_and(|tab| tab.set_focus(native_view)),
        "the native leaf is live"
    );
    app.sync_window(wid);
    assert_eq!(
        app.active_visible_frame_layout(wid).map(|(_, route)| route),
        Some(crate::VisibleContentRoute::Heterogeneous),
        "PRECONDITION: a mixed tab"
    );
    let (_, view) = app
        .active_native_view(wid)
        .expect("PRECONDITION: the native leaf has focus");
    assert!(
        app.commit_glassless_native_frame(wid),
        "the glassless mixed frame commits"
    );
    let compiled = app.compiled_native_ui(wid).expect("the page compiles");
    let row = compiled
        .focus_order
        .iter()
        .find(|key| key.as_str().starts_with("settings/cursor-style/"))
        .cloned()
        .expect("a style row");
    app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
        row.as_str().to_string(),
    )))
    .expect("the move aims at the native leaf's row");
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.clone()),
        Some(row),
        "the row is hovered"
    );
}

#[test]
fn pointer_feedback_keeps_presented_hits_without_certifying_pending_content() {
    use super::{NativePointerTarget, PointerAt};
    use crate::native_settings::SettingsRoute;
    use aterm_window::event::{ElementState, MouseButton};

    let (mut app, wid, view) = settings_at_1000_by_600(SettingsRoute::KeyboardInput);
    prepare_presented_native(&mut app, wid);
    let key = format!("settings/nav{}", SettingsRoute::WindowTabs.path());
    let (x, y) = app
        .native_pointer_target_px(wid, &NativePointerTarget::Key(key.clone()))
        .unwrap();
    let paint = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap()
        .paint_serial;
    app.on_cursor_moved(wid, x, y);
    let cache = &app.windows[&wid].leaf_render_cache[&view];
    assert!(cache.native_damage.is_some(), "hover still owes a paint");
    assert_eq!(cache.native.as_ref().unwrap().paint_serial, paint);
    assert!(app.retained_native_leaf_artifact(wid, view, true).is_none());
    let cell = app.pointer_readback(wid);
    assert!(matches!(
        app.pointer_reply(cell),
        Ok(PointerAt::Native { key: Some(hit), .. }) if hit == key
    ));
    let feedback = app.native_paint_serial(wid, view);
    app.on_cursor_moved(wid, x, y);
    assert_eq!(app.native_paint_serial(wid, view), feedback);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .pressed
            .as_ref()
            .map(|key| key.as_str()),
        Some(key.as_str()),
    );
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let crate::native_app::AppViewState::Settings(settings) =
        app.native_runtime.view_state(view).unwrap()
    else {
        unreachable!();
    };
    assert_eq!(settings.route, SettingsRoute::WindowTabs);
    assert!(
        app.pointer_reply(cell).is_err(),
        "new content is not presented"
    );

    prepare_presented_native(&mut app, wid);
    app.invalidate_native_ui_cache(wid);
    app.apply_native_hover(wid, None);
    app.apply_native_hover(
        wid,
        Some((view, crate::native_ui::UiKey::new(key.as_str()))),
    );
    assert!(
        app.pointer_reply(cell).is_err(),
        "feedback cannot bless generic invalidation"
    );
    let paint = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap()
        .paint_serial;
    assert!(app.native_paint_serial(wid, view) > paint);
    assert!(app.retained_native_leaf_artifact(wid, view, true).is_none());
}

#[test]
fn a_glassless_native_pointer_hovers_says_where_and_leaving_clears_it() {
    use super::{NativePointerTarget, PointerAt};
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let at = app
        .pointer_request(PointerAction::Move { row: 2, col: 3 })
        .expect("a terminal move");
    assert!(matches!(at, PointerAt::Grid { cell: (2, 3), .. }));
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorStyles));
    let (_, view) = app.active_native_view(wid).expect("Settings in front");
    let compiled = app.compiled_native_ui(wid).expect("the page compiles");
    let row = compiled
        .focus_order
        .iter()
        .find(|key| key.as_str().starts_with("settings/cursor-style/"))
        .cloned()
        .expect("a style row");
    let hovered = |app: &crate::App| {
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.clone())
    };
    let at = app
        .pointer_request(PointerAction::MoveNative(NativePointerTarget::Key(
            row.as_str().to_string(),
        )))
        .expect("the move aims at the row");
    assert_eq!(hovered(&app), Some(row.clone()), "the row is hovered");
    match at {
        PointerAt::Native {
            view: at,
            key: Some(key),
            ..
        } => {
            assert_eq!(at, view.get());
            assert_eq!(key, row.as_str());
        }
        other => panic!("a native answer, never a grid cell: {other:?}"),
    }
    let at = app
        .pointer_request(PointerAction::Leave)
        .expect("the pointer leaves");
    assert_eq!(hovered(&app), None, "leaving clears the hover");
    assert_eq!(at, PointerAt::Absent);
}

#[test]
fn a_page_scrolled_under_a_still_pointer_is_hovered_where_it_now_is() {
    use super::{NativePointerTarget, PointerAt, WheelUnit};
    let (mut app, wid, view) =
        settings_at_1000_by_600(crate::native_settings::SettingsRoute::CursorStyles);
    let compiled = app.compiled_native_ui(wid).expect("the page compiles");
    let row = compiled
        .focus_order
        .iter()
        .filter(|key| key.as_str().starts_with("settings/cursor-style/"))
        .nth(1)
        .cloned()
        .expect("a style row");
    let hovered = |app: &crate::App| {
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.clone())
            .map(|key| key.as_str().to_string())
    };
    app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
        row.as_str().to_string(),
    )))
    .expect("the move aims at the row");
    assert_eq!(
        hovered(&app).as_deref(),
        Some(row.as_str()),
        "CONTROL: the row is hovered"
    );
    let cell = app
        .pointer_cmd(PointerAction::Wheel {
            dx: 0.0,
            dy: 3.0,
            unit: WheelUnit::Lines,
            aim: None,
        })
        .expect("the wheel scrolls the list");
    let under = match app.pointer_reply(cell) {
        Ok(PointerAt::Native { key, .. }) => key,
        other => panic!("a native answer: {other:?}"),
    };
    assert_ne!(
        under.as_deref(),
        Some(row.as_str()),
        "the wheel moved the list under the pointer"
    );
    assert_eq!(hovered(&app), under, "hovered where the pointer now is");
    let tried = app
        .compiled_native_ui(wid)
        .expect("the page compiles")
        .paint
        .iter()
        .find_map(|node| match &node.content {
            crate::native_ui::UiContent::SettingsPreview(spec) => {
                Some(spec.cursor.effective_trail_style_raw().to_string())
            }
            _ => None,
        })
        .expect("the preview");
    let shown = under
        .as_deref()
        .and_then(|key| key.strip_prefix("settings/cursor-style/"))
        .map(|style| style.replace('-', " "));
    if let Some(shown) = shown {
        assert!(
            tried.eq_ignore_ascii_case(&shown),
            "the preview tries {tried:?}, the row under the pointer is {shown:?}"
        );
    }
}

/// A click on a rail row from a sub-page leaves that row under the still pointer and lit,
/// and a second click keeps its page: no page moves the rail's rows. CONTROL: the first
/// click opens the row's page.
#[test]
fn a_second_click_on_a_rail_row_opens_the_page_the_first_did() {
    use super::{NativePointerTarget, PointerAt};
    use crate::native_settings::SettingsRoute;
    for (from, row) in [
        (SettingsRoute::Themes, SettingsRoute::Wallpaper),
        (SettingsRoute::Ai, SettingsRoute::Security),
        (SettingsRoute::Tabs, SettingsRoute::Security),
    ] {
        let (mut app, _wid, view) =
            settings_sized_in(crate::App::headless_for_test(), from, (1_000, 900));
        let key = format!("settings/nav{}", row.path());
        let route = |app: &crate::App| match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(settings)) => settings.route,
            _ => unreachable!(),
        };
        let hovered = |app: &crate::App| {
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().hovered.clone())
                .map(|key| key.as_str().to_string())
        };
        app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
            key.clone(),
        )))
        .expect("the move aims at the row");
        for click in 0..2 {
            let cell = app.pointer_cmd(PointerAction::Click).expect("a click");
            assert_eq!(route(&app), row, "{from:?}: click {click} opens {row:?}");
            let under = match app.pointer_reply(cell) {
                Ok(PointerAt::Native { key, .. }) => key,
                other => panic!("a native answer: {other:?}"),
            };
            assert_eq!(
                under.as_deref(),
                Some(key.as_str()),
                "{from:?}: click {click} leaves the row under the pointer"
            );
            assert_eq!(
                hovered(&app).as_deref(),
                Some(key.as_str()),
                "{from:?}: click {click} leaves the row lit"
            );
        }
    }
}

/// The frame after a press is hovered where the still pointer then is, as a wheel's is:
/// All Settings opens the category sheet, whose Done takes its place, and the button the
/// press replaced no longer reads as hovered. CONTROL: before the click All Settings is the
/// hovered control.
#[test]
fn the_frame_after_a_press_is_hovered_where_the_pointer_is() {
    use super::{NativePointerTarget, PointerAt};
    let (mut app, _wid, view) = settings_sized_in(
        crate::App::headless_for_test(),
        crate::native_settings::SettingsRoute::KeyboardInput,
        (584, 348),
    );
    let hovered = |app: &crate::App| {
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.clone())
            .map(|key| key.as_str().to_string())
    };
    app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
        "settings/compact-navigation".into(),
    )))
    .expect("the move aims at All Settings");
    assert_eq!(
        hovered(&app).as_deref(),
        Some("settings/compact-navigation"),
        "CONTROL: All Settings is hovered"
    );
    let cell = app.pointer_cmd(PointerAction::Click).expect("a click");
    let under = match app.pointer_reply(cell) {
        Ok(PointerAt::Native { key, .. }) => key,
        other => panic!("a native answer: {other:?}"),
    };
    assert_eq!(
        under.as_deref(),
        Some("settings/compact-navigation-close"),
        "the sheet's Done is under the pointer"
    );
    assert_eq!(hovered(&app), under, "and it is the hovered control");
}

fn settings_at_1000_by_600(
    route: crate::native_settings::SettingsRoute,
) -> (crate::App, crate::WindowId, crate::tab_model::ViewId) {
    settings_at_1000_by_600_in(crate::App::headless_for_test(), route)
}

fn settings_at_1000_by_600_in(
    app: crate::App,
    route: crate::native_settings::SettingsRoute,
) -> (crate::App, crate::WindowId, crate::tab_model::ViewId) {
    settings_sized_in(app, route, (1_000, 600))
}

fn settings_sized_in(
    mut app: crate::App,
    route: crate::native_settings::SettingsRoute,
    (width, height): (usize, usize),
) -> (crate::App, crate::WindowId, crate::tab_model::ViewId) {
    let wid = crate::WindowId(0);
    let (cell_w, cell_h) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = u16::try_from((width - pad * 2) / cell_w.max(1)).unwrap();
        window.rows = u16::try_from((height - pad * 2) / cell_h.max(1)).unwrap();
    }
    assert!(app.open_settings_tab(route));
    let (_, view) = app.active_native_view(wid).expect("Settings in front");
    (app, wid, view)
}

#[test]
fn a_glassless_pointers_reply_reads_the_frame_its_click_left() {
    use super::{NativePointerTarget, PointerAt};
    let (mut app, wid, view) =
        settings_at_1000_by_600(crate::native_settings::SettingsRoute::KeyboardInput);
    let switch = crate::native_ui::UiKey::new("settings/control/copy_on_select");
    let look = |app: &crate::App| {
        let compiled = app.compiled_native_ui(wid).expect("the page compiles");
        compiled
            .semantic(&switch)
            .map(|node| (node.value.clone(), node.state))
    };
    let native_reply = |app: &crate::App, cell| match app.pointer_reply(cell) {
        Ok(PointerAt::Native { view: at, .. }) => assert_eq!(at, view.get()),
        other => panic!("the reply reads the frame after the event: {other:?}"),
    };
    let before = look(&app);
    assert!(
        app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
            switch.as_str().to_string()
        )))
        .is_ok()
    );
    let cell = app.pointer_cmd(PointerAction::Click).expect("a click");
    assert_ne!(look(&app), before, "the click acted");
    native_reply(&app, cell);
    let _ = app.dispatch_native_event(wid, crate::native_app::AppEvent::ScrollLines(1));
    assert_eq!(
        app.pointer_reply(cell),
        Err(
            "the page under the pointer changed and its next frame is not presented yet \
             (pointer status reads it)"
                .to_string()
        )
    );
    let cell = app.pointer_cmd(PointerAction::Status).expect("status");
    native_reply(&app, cell);
}

#[test]
fn a_pointer_reply_on_glass_reads_the_frame_its_event_left() {
    use super::{NativePointerTarget, PointerAt};
    use crate::native_settings::SettingsRoute;
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    // Keep the render-cost feedback latch fixed: this test scripts a failed
    // commit, not a wall-clock-dependent motion-policy transition.
    let motion_anchor = std::time::Instant::now();
    app.perf_reduced = true;
    app.perf_flip_at = Some(motion_anchor);
    app.perf_shed_dwell = std::time::Duration::MAX;
    let (mut app, wid, view) = settings_at_1000_by_600_in(app, SettingsRoute::KeyboardInput);
    assert!(
        !app.commit_glassless_native_frame(wid),
        "the window has glass"
    );
    // The chrome fonts the first present would install after it, so the page it
    // presents is not redrawn for them.
    app.sync_chrome_fonts();
    app.redraw_window(wid);
    let first_key = format!("settings/nav{}", SettingsRoute::WindowTabs.path());
    let second_key = format!("settings/nav{}", SettingsRoute::KeyboardInput.path());
    let artifact = app
        .retained_native_leaf_artifact(wid, view, true)
        .expect("the initial Settings page is presented");
    for key in [&first_key, &second_key] {
        let shown = artifact
            .compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == key.as_str())
            .expect("both fixed rail rows are presented");
        let hit = artifact
            .compiled
            .hits
            .iter()
            .find(|hit| hit.key.as_str() == key.as_str())
            .expect("both fixed rail rows are actionable");
        assert!(!shown.clip.is_empty(), "{key} is visible");
        assert!(!hit.rect.is_empty(), "{key} has a pointer target");
        assert!(
            shown.clip.contains(
                hit.rect.x + hit.rect.width / 2.0,
                hit.rect.y + hit.rect.height / 2.0,
            ),
            "{key}'s pointer target lies inside its visible clip"
        );
    }
    let reply =
        |app: &mut crate::App, action: PointerAction| match app.pointer_request(action.clone()) {
            Ok(PointerAt::Native { view: at, key, .. }) => {
                assert_eq!(at, view.get());
                key
            }
            other => panic!("the reply for {action:?} reads the presented frame: {other:?}"),
        };
    let state = |app: &crate::App| match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Settings(settings)) => {
            (settings.route, settings.common.content_epoch)
        }
        _ => panic!("Settings remains attached"),
    };
    let untouched = state(&app);
    assert_eq!(untouched.0, SettingsRoute::KeyboardInput);
    assert!(
        app.pointer_request(PointerAction::Click).is_err(),
        "no position is a pre-dispatch refusal"
    );
    assert!(
        app.pointer_request(PointerAction::MoveNative(NativePointerTarget::Key(
            "settings/no-such-control".into()
        )))
        .is_err(),
        "an invalid target is a pre-dispatch refusal"
    );
    assert_eq!(state(&app), untouched);
    let hovered = reply(
        &mut app,
        PointerAction::MoveNative(NativePointerTarget::Key(first_key.clone())),
    );
    assert_eq!(
        hovered.as_deref(),
        Some(first_key.as_str()),
        "the hover is presented"
    );
    let before = state(&app);
    let _ = reply(&mut app, PointerAction::Click);
    assert_eq!(
        state(&app).0,
        SettingsRoute::WindowTabs,
        "the click navigated"
    );
    assert_ne!(state(&app).1, before.1, "the click changed actual content");
    let status = reply(&mut app, PointerAction::Status);
    let (x, y) = app.windows[&wid].last_cursor_px;
    let (artifact, local_x, local_y) = app
        .retained_native_leaf_at_pointer(wid, x, y)
        .expect("the frame under the pointer is presented");
    assert_eq!(
        status,
        artifact
            .compiled
            .hit_test(local_x, local_y)
            .map(|hit| hit.key.as_str().to_string()),
        "the reply names what the page now holds there"
    );
    let surface = crate::scripted_redraw_fixture::surface;
    let commits = surface(&mut app).commits();
    let _ = reply(&mut app, PointerAction::Status);
    assert_eq!(surface(&mut app).commits(), commits, "nothing changed");
    let hovered = reply(
        &mut app,
        PointerAction::MoveNative(NativePointerTarget::Key(second_key.clone())),
    );
    assert_eq!(
        hovered.as_deref(),
        Some(second_key.as_str()),
        "the next rail row is presented"
    );
    assert!(app.perf_reduced, "the scripted motion latch stays fixed");
    assert_eq!(app.perf_flip_at, Some(motion_anchor));
    assert_eq!(app.perf_shed_dwell, std::time::Duration::MAX);
    surface(&mut app).fail_next(crate::present::scripted::ScriptedOutcome::FailCommit);
    let before_commits = surface(&mut app).commits();
    let before_click = state(&app);
    let dispatched = app.pointer_request(PointerAction::Click);
    assert_eq!(
        state(&app).0,
        SettingsRoute::KeyboardInput,
        "the failed present cannot undo the real navigation click"
    );
    assert_ne!(
        state(&app).1,
        before_click.1,
        "the failed present cannot undo the actual content change"
    );
    assert_eq!(
        surface(&mut app).commits(),
        before_commits,
        "the scripted failed commit leaves no new frame"
    );
    let after_click = state(&app);
    let cell = app.pointer_readback(wid);
    assert_eq!(
        app.pointer_reply(cell),
        Err(
            "the page under the pointer changed and its next frame is not presented yet \
             (pointer status reads it)"
                .to_string()
        )
    );
    // Hold the existing retry policy parked so this read-only control cannot
    // opportunistically present a later frame or depend on a wall-clock backoff.
    app.windows.get_mut(&wid).unwrap().present_retry.parked = true;
    let commits = surface(&mut app).commits();
    assert!(
        app.pointer_request(PointerAction::Status).is_err(),
        "Status still refuses unavailable positional readback"
    );
    assert_eq!(state(&app), after_click, "Status never replays the click");
    assert_eq!(
        surface(&mut app).commits(),
        commits,
        "the parked surface stays unpresented"
    );
    assert_eq!(
        dispatched,
        Ok(PointerAt::Dispatched {
            readback_error:
                "the page under the pointer changed and its next frame is not presented yet \
                             (pointer status reads it)"
                    .to_string(),
        }),
        "an already-dispatched click must not look like a rejected request"
    );
}

/// AN AIMED POINTER VERB ON GLASS AIMS AT THE PAGE AS IT NOW STANDS: a page that changed
/// after its last present (a press's save landing recompiles it) is presented before
/// `pointer move key=` or an aimed `wheel` aims, as a glassless window commits it first. On
/// glass the aim read only a presented frame, so right after a click it answered `no
/// presented native frame to aim at` until something else presented the page.
#[test]
fn an_aimed_pointer_on_glass_presents_the_changed_page_before_it_aims() {
    use super::{NativePointerTarget, PointerAt, WheelUnit};
    use crate::native_settings::SettingsRoute;
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    app.perf_reduced = true;
    app.perf_flip_at = Some(std::time::Instant::now());
    app.perf_shed_dwell = std::time::Duration::MAX;
    let (mut app, wid, view) = settings_at_1000_by_600_in(app, SettingsRoute::KeyboardInput);
    assert!(
        !app.commit_glassless_native_frame(wid),
        "the window has glass"
    );
    app.sync_chrome_fonts();
    app.redraw_window(wid);
    let key = "settings/control/copy_on_select".to_string();
    let aims = [
        PointerAction::MoveNative(NativePointerTarget::Key(key.clone())),
        PointerAction::Wheel {
            dx: 0.0,
            dy: 1.0,
            unit: WheelUnit::Lines,
            aim: Some(NativePointerTarget::Key(key)),
        },
    ];
    for action in aims {
        // What a landed save does to the page: it changes, so its presented raster is stale.
        assert!(app.native_runtime.change_view_state(view, |state| {
            let common = state.common_mut();
            common.presentation_revision = common.presentation_revision.saturating_add(1);
            true
        }));
        assert!(
            app.retained_native_leaf_artifact(wid, view, true).is_none(),
            "precondition: the changed page is not presented"
        );
        match app.pointer_request(action.clone()) {
            Ok(PointerAt::Native { view: at, .. }) => assert_eq!(at, view.get()),
            other => panic!("{action:?} aims at the page it presents first: {other:?}"),
        }
    }
}

#[test]
fn a_headless_pointer_wheel_takes_the_wheels_own_path_into_a_native_view() {
    use super::{NativePointerTarget, PointerAt, WheelUnit};
    use crate::native_app::AppViewState;
    let (mut app, wid, view) =
        settings_at_1000_by_600(crate::native_settings::SettingsRoute::Terminal);
    let page_scroll = |app: &crate::App| match app.native_runtime.view_state(view) {
        Some(AppViewState::Settings(settings)) => settings.page_scroll,
        _ => panic!("Settings state"),
    };
    let lines = |dy: f64, aim: Option<&str>| PointerAction::Wheel {
        dx: 0.0,
        dy,
        unit: WheelUnit::Lines,
        aim: aim.map(|key| NativePointerTarget::Key(key.to_string())),
    };
    let native_reply = |app: &crate::App, cell| match app.pointer_reply(cell) {
        Ok(PointerAt::Native { view: at, .. }) => assert_eq!(at, view.get()),
        other => panic!("the reply reads the frame after the event: {other:?}"),
    };
    assert_eq!(
        app.pointer_cmd(lines(1.0, None)),
        Err("the pointer holds no position (move it first)".to_string())
    );
    assert_eq!(page_scroll(&app), 0);
    let compiled = app.compiled_native_ui(wid).expect("the page compiles");
    let on_screen = |prefix: &str| {
        compiled
            .paint
            .iter()
            .find(|node| {
                node.key.as_str().starts_with(prefix)
                    && node.clip.width > 0.0
                    && node.clip.height > 0.0
            })
            .map(|node| node.key.as_str().to_string())
            .unwrap_or_else(|| panic!("a {prefix} control on screen"))
    };
    let (row, nav) = (on_screen("settings/control/"), on_screen("settings/nav/"));
    let cell = app
        .pointer_cmd(lines(2.0, Some(&row)))
        .expect("a wheel aimed at a row");
    assert_eq!(page_scroll(&app), 2, "one stop a line");
    native_reply(&app, cell);
    let cell = app
        .pointer_cmd(lines(-1.0, None))
        .expect("a wheel where it rests");
    assert_eq!(page_scroll(&app), 1);
    native_reply(&app, cell);
    let sideways = PointerAction::Wheel {
        dx: 3.0,
        dy: 0.0,
        unit: WheelUnit::Lines,
        aim: None,
    };
    assert!(app.pointer_cmd(sideways).is_ok());
    assert_eq!(page_scroll(&app), 1);
    let (_, cell_h) = app.win_cell_size(wid);
    let scale = app.windows[&wid].scale;
    let points = PointerAction::Wheel {
        dx: 0.0,
        dy: 2.0 * cell_h as f64 / scale,
        unit: WheelUnit::Points,
        aim: None,
    };
    assert!(app.pointer_cmd(points).is_ok());
    assert_eq!(page_scroll(&app), 3, "points bank into whole stops");
    assert!(app.pointer_cmd(lines(1.0, Some(&nav))).is_ok());
    assert_eq!(page_scroll(&app), 3, "the sidebar's wheel leaves the list");
}

#[test]
fn a_pointer_wheel_over_a_terminal_scrolls_its_history() {
    use super::WheelUnit;
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let term = app.front_terminal(wid).expect("a terminal").term.clone();
    {
        let mut terminal = crate::term_lock(&term);
        for line in 0..100 {
            terminal.process(format!("history {line}\r\n").as_bytes());
        }
    }
    let wheel = |dy: f64| PointerAction::Wheel {
        dx: 0.0,
        dy,
        unit: WheelUnit::Lines,
        aim: None,
    };
    assert!(
        app.pointer_cmd(PointerAction::Move { row: 2, col: 3 })
            .is_ok()
    );
    assert!(app.pointer_cmd(wheel(1.0)).is_ok());
    assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
    assert!(
        app.windows[&wid].scroll_glide.is_none(),
        "down at the bottom"
    );
    assert!(app.pointer_cmd(wheel(-3.0)).is_ok());
    assert!(
        app.windows[&wid].scroll_glide.is_some()
            || crate::term_lock(&term).grid().display_offset() > 0,
        "up reaches the history"
    );
}

#[test]
fn a_wheel_over_an_open_picker_moves_the_hover_only_when_the_list_moves() {
    use super::{NativePointerTarget, PointerAt, WheelUnit};
    use crate::native_app::AppViewState;
    let (mut app, wid, view) =
        settings_at_1000_by_600(crate::native_settings::SettingsRoute::Appearance);
    let aim = |key: &str| PointerAction::MoveNative(NativePointerTarget::Key(key.into()));
    let wheel = |dy: f64| PointerAction::Wheel {
        dx: 0.0,
        dy,
        unit: WheelUnit::Lines,
        aim: None,
    };
    let tiles = |app: &crate::App| {
        app.compiled_native_ui(wid)
            .expect("the page compiles")
            .paint
            .iter()
            .filter(|node| {
                node.key.as_str().starts_with("settings/choice/theme/") && node.clip.height > 0.0
            })
            .map(|node| node.key.as_str().to_string())
            .collect::<Vec<_>>()
    };
    let hovered = |app: &crate::App| match app.native_runtime.view_state(view) {
        Some(AppViewState::Settings(settings)) => settings
            .common
            .hovered
            .as_ref()
            .map(|key| key.as_str().to_string()),
        _ => panic!("Settings state"),
    };
    assert!(app.pointer_cmd(aim("settings/control/theme")).is_ok());
    assert!(app.pointer_cmd(PointerAction::Click).is_ok());
    let top = tiles(&app);
    let tile = top.get(1).cloned().expect("the picker opened on its tiles");
    assert!(app.pointer_cmd(aim(&tile)).is_ok());
    assert_eq!(hovered(&app), Some(tile.clone()));
    assert!(app.pointer_cmd(wheel(-1.0)).is_ok());
    assert_eq!(tiles(&app), top, "the list was at its top");
    assert_eq!(hovered(&app), Some(tile.clone()), "nothing moved under it");
    let cell = app.pointer_cmd(wheel(1.0)).expect("the wheel scrolls");
    assert_ne!(tiles(&app), top, "the list scrolled");
    let under = match app.pointer_reply(cell) {
        Ok(PointerAt::Native { key, .. }) => key,
        other => panic!("a native answer: {other:?}"),
    };
    assert_ne!(under.as_deref(), Some(tile.as_str()), "the tile moved away");
    assert_eq!(hovered(&app), under, "the hover went with the move");
}

#[test]
fn a_pointer_wheel_puts_a_press_nudge_back_on_the_lists_stops() {
    use super::{NativePointerTarget, WheelUnit};
    use crate::native_ui::LogicalRect;
    let wheel = |dy: f64| PointerAction::Wheel {
        dx: 0.0,
        dy,
        unit: WheelUnit::Lines,
        aim: None,
    };
    let place = |app: &crate::App, wid| {
        app.compiled_native_ui(wid)
            .expect("the page compiles")
            .semantics
            .iter()
            .filter(|node| !node.offscreen && node.key.as_str().starts_with("settings/control/"))
            .map(|node| (node.key.as_str().to_string(), node.rect.y, node.rect.height))
            .collect::<Vec<_>>()
    };
    let security = |size| {
        let (app, wid, _) = settings_sized_in(
            crate::App::headless_for_test(),
            crate::native_settings::SettingsRoute::Security,
            size,
        );
        let compiled = app.compiled_native_ui(wid).expect("the page compiles");
        let cut = compiled
            .paint
            .iter()
            .filter(|node| matches!(node.content, crate::native_ui::UiContent::Switch(_)))
            .find_map(|node| {
                let picture = compiled
                    .list_pictures
                    .iter()
                    .find(|picture| picture.key == node.key)?
                    .rect;
                // In the strip and cut by the list's foot: one drawn there whole works
                // itself ([`crate::native_ui::CompiledUi::picture_hits`]).
                (compiled.hits.iter().all(|hit| hit.key != node.key)
                    && compiled.picture_hits.iter().all(|hit| hit.key != node.key)
                    && picture.y == node.rect.y
                    && picture.height > 4.0
                    && node.fade.is_some_and(|(top, _)| top <= node.rect.y + 0.5))
                .then(|| (node.key.as_str().to_string(), node.rect, picture))
            })?;
        Some((app, wid, cut))
    };
    let press = |app: &mut crate::App,
                 wid,
                 (cut, rect, picture): &(String, LogicalRect, LogicalRect)| {
        let (x, y) = (picture.x + picture.width / 2.0, picture.y + 2.0);
        assert!(
            app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Point(x, y)))
                .is_ok()
        );
        let on = app
            .compiled_native_ui(wid)
            .expect("the page compiles")
            .paint
            .iter()
            .find_map(|node| match &node.content {
                crate::native_ui::UiContent::Switch(control) if node.key.as_str() == cut => {
                    Some(control.value.clone())
                }
                _ => None,
            })
            .expect("the switch is drawn");
        assert!(app.pointer_cmd(PointerAction::Click).is_ok());
        let nudged = place(app, wid);
        let y1 = nudged
            .iter()
            .find(|(key, ..)| key == cut)
            .map(|(_, y, _)| *y)
            .expect("the switch is live now");
        assert!(
            y1 < rect.y - 0.5,
            "{cut}: nudged up by its part under the room's edge, not by a stop: {rect:?} -> {y1}"
        );
        let compiled = app.compiled_native_ui(wid).expect("the page compiles");
        assert_eq!(
            compiled
                .semantic(&crate::native_ui::UiKey::new(cut.as_str()))
                .map(|node| node.value.clone()),
            Some(on),
            "it turned nothing"
        );
        nudged
    };
    let mut checked = 0;
    for size in [(600, 360), (640, 400), (738, 418), (800, 480), (1_000, 600)] {
        let Some((mut app, wid, cut)) = security(size) else {
            continue;
        };
        let top = place(&app, wid);
        let (x, y) = (cut.2.x + cut.2.width / 2.0, cut.2.y + 2.0);
        assert!(
            app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Point(x, y)))
                .is_ok()
        );
        assert!(app.pointer_cmd(wheel(1.0)).is_ok());
        let one_on = place(&app, wid);
        assert_ne!(one_on, top, "{size:?}: the wheel scrolled the list");
        assert!(app.pointer_cmd(wheel(-1.0)).is_ok());
        assert_eq!(place(&app, wid), top, "{size:?}: back at the top");
        let nudged = press(&mut app, wid, &cut);
        assert!(app.pointer_cmd(PointerAction::Status).is_ok());
        assert_eq!(place(&app, wid), nudged, "{size:?}: the nudge stands");
        assert!(app.pointer_cmd(wheel(1.0)).is_ok());
        assert_eq!(
            place(&app, wid),
            one_on,
            "{size:?}: down a line from the nudge is a stop on"
        );
        let (mut app, wid, cut) = security(size).expect("the same page");
        let _ = press(&mut app, wid, &cut);
        assert!(app.pointer_cmd(wheel(-1.0)).is_ok());
        assert_eq!(
            place(&app, wid),
            top,
            "{size:?}: up a line from the nudge is the top"
        );
        checked += 1;
    }
    assert!(checked > 0, "some size cuts a switch at the list's bottom");
}

fn file_uri(path: &std::path::Path) -> String {
    crate::native_document_host::path_to_file_uri(path).unwrap()
}

#[test]
fn the_hand_the_press_and_the_caption_agree_on_a_plain_url() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    crate::term_lock(&term).process(
        b"Docs: https://google.com\x1b[8m.evil.example/login\x1b[28m (official)\r\n\
          Docs: \x1b[48;2;17;19;24mhttps://google.com\x1b[38;2;40;43;50m.evil.example/login\x1b[0m\r\n\
          Docs: https://example.com/path/to/page\r\n\
          Docs: \x1b[38;2;33;34;44;48;2;40;42;54mhttps://example.com/path\x1b[0m\r\n",
    );
    app.on_modifiers_changed(wid, link_modifier());
    let (cw, ch) = app.win_cell_size(wid);
    let strip_px = f64::from(app.tab_strip_rows) * ch as f64;
    let hand = |app: &mut crate::App, row: u16, col: u16| {
        let x = app.win_pad(wid) as f64 + cw as f64 * (f64::from(col) + 0.5);
        let y = (app.win_pad_top(wid) + app.win_head(wid)) as f64
            + strip_px
            + ch as f64 * (f64::from(row) + 0.5);
        app.on_cursor_moved(wid, x, y);
        assert_eq!(app.windows[&wid].last_mouse_cell, (row, col));
        app.windows[&wid].hover_pointer
    };
    for row in 0..3 {
        for col in 0..80 {
            let offered = hand(&mut app, row, col);
            assert_eq!(
                offered,
                app.link_under_pointer(wid).is_some(),
                "({row}, {col})"
            );
        }
    }
    let band = |app: &mut crate::App| {
        assert!(
            app.prepare_terminal_capture_grid_with_cursor_fx(
                wid,
                crate::app_render::ComposedCursorFxClock::Advance(std::time::Instant::now()),
            )
            .is_some()
        );
        app.splice_link_target(wid);
        let rows = &app.windows[&wid].input_scratch.cells;
        rows.last()
            .expect("rows")
            .iter()
            .map(|cell| cell.ch)
            .collect::<String>()
    };
    assert!(hand(&mut app, 0, 20), "the visible URL takes the hand");
    assert!(
        !band(&mut app).contains("google.com"),
        "and is its own disclosure"
    );
    assert!(!hand(&mut app, 0, 30), "a cell that looks blank never does");
    let whole = "https://google.com.evil.example/login";
    assert!(hand(&mut app, 1, 30), "a faint tail is still the URL's");
    assert_eq!(app.link_under_pointer(wid).as_deref(), Some(whole));
    let caption = band(&mut app);
    assert!(
        caption.contains(whole),
        "the band names it first: {caption:?}"
    );
    assert!(hand(&mut app, 2, 20));
    let disclosed = |app: &crate::App| app.windows[&wid].link_hover;
    assert_eq!(disclosed(&app), None, "an ordinary URL takes no row");
    app.render_knobs.minimum_contrast = 4.5;
    assert!(hand(&mut app, 3, 20));
    assert_eq!(
        disclosed(&app),
        None,
        "a URL the configured floor lifts takes no row"
    );
}

#[test]
fn divider_wobble_across_a_cell_edge_never_regrids_the_siblings() {
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let top = app.focused_session_id(wid).unwrap();
    let bottom = app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
    let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
    let col = cols / 4;
    let divider = (0..rows)
        .find(|&r| {
            app.active_tree(wid)
                .expect("terminal split tree")
                .divider_at(r, col, rows, cols)
                .is_some()
        })
        .expect("the split has a divider row");
    let ch = app.pointer_geometry(wid).cell.1;
    assert!(
        ch / 4 >= 2,
        "a cell tall enough for a 1 px nudge to sit in the band"
    );
    let engine_rows = |app: &crate::App| {
        let rows = |sid| crate::term_lock(&app.pool.get(sid).unwrap().term).rows();
        (rows(top), rows(bottom))
    };
    let before = engine_rows(&app);

    let (x, y) = app.cell_centre_px(wid, divider, col).unwrap();
    let (_, below) = app.cell_centre_px(wid, divider + 1, col).unwrap();
    #[allow(clippy::cast_precision_loss, reason = "a cell height in px")]
    let nudge = below - (ch / 2) as f64 + 1.0;
    assert_eq!(
        app.pixel_to_cell(wid, x, nudge).0,
        divider + 1,
        "the nudge really crosses into the next cell"
    );

    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert!(
        app.windows[&wid].divider_drag.is_some(),
        "press arms the drag"
    );
    let mut seen = vec![engine_rows(&app)];
    app.on_cursor_moved(wid, x, nudge);
    seen.push(engine_rows(&app));
    app.on_cursor_moved(wid, x, y);
    seen.push(engine_rows(&app));
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    seen.push(engine_rows(&app));
    assert!(
        app.windows[&wid].divider_drag.is_none(),
        "release ends the drag"
    );
    assert!(
        seen.iter().all(|&s| s == before),
        "a wobble inside the dead band never re-grids either pane: \
         before={before:?} seen={seen:?}"
    );

    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert!(
        app.windows[&wid].divider_drag.is_some(),
        "press re-arms the drag"
    );
    app.on_cursor_moved(wid, x, below);
    assert_eq!(
        engine_rows(&app),
        (before.0 + 1, before.1 - 1),
        "a real one-row move applies at once — the band adds no latency"
    );
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_eq!(engine_rows(&app), (before.0 + 1, before.1 - 1));
    assert!(app.structural_invariants_ok());
}

#[test]
fn divider_drag_flushes_a_coalesced_step_on_a_move_inside_the_band() {
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let top = app.focused_session_id(wid).unwrap();
    let bottom = app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
    let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
    let col = cols / 4;
    let divider = (0..rows)
        .find(|&r| {
            app.active_tree(wid)
                .expect("terminal split tree")
                .divider_at(r, col, rows, cols)
                .is_some()
        })
        .expect("the split has a divider row");
    let engine_rows = |app: &crate::App| {
        let rows = |sid| crate::term_lock(&app.pool.get(sid).unwrap().term).rows();
        (rows(top), rows(bottom))
    };
    let before = engine_rows(&app);
    let (x, y) = app.cell_centre_px(wid, divider, col).unwrap();
    let (_, one) = app.cell_centre_px(wid, divider + 1, col).unwrap();
    let (_, two) = app.cell_centre_px(wid, divider + 2, col).unwrap();

    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_cursor_moved(wid, x, one);
    assert_eq!(
        engine_rows(&app),
        (before.0 + 1, before.1 - 1),
        "the first step is the leading edge"
    );
    super::PANE_DRAG_APPLIED_AT.with(|c| c.set(Some((wid, std::time::Instant::now()))));
    app.on_cursor_moved(wid, x, two);
    assert_eq!(
        super::PANE_DRAG_PENDING.with(|c| c.get()),
        Some(wid),
        "the in-window step is pending, not applied"
    );
    assert_eq!(engine_rows(&app), (before.0 + 1, before.1 - 1));
    let past = std::time::Instant::now()
        .checked_sub(crate::RESIZE_THROTTLE * 2)
        .expect("a monotonic clock older than two throttle windows");
    super::PANE_DRAG_APPLIED_AT.with(|c| c.set(Some((wid, past))));
    app.on_cursor_moved(wid, x, two + 1.0);
    assert_eq!(
        engine_rows(&app),
        (before.0 + 2, before.1 - 2),
        "a move inside the band flushes the coalesced step"
    );
    assert_eq!(super::PANE_DRAG_PENDING.with(|c| c.get()), None);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_eq!(engine_rows(&app), (before.0 + 2, before.1 - 2));
    assert!(app.structural_invariants_ok());
}

#[test]
fn divider_drag_offset_is_read_against_the_clamped_cell() {
    use super::divider_drag_off_axis as off;
    assert_eq!(off(187, 9, 20), 7, "inside the cell: the real offset");
    assert_eq!(off(203, 9, 20), 19, "past the last row: its far pixel");
    assert_eq!(off(455, 9, 20), 19, "far past it: still its far pixel");
    assert_eq!(off(3, 1, 20), 0, "before the cell: its first pixel");
    assert_eq!(off(5, 0, 0), 0, "no geometry: no offset");
    assert_eq!(super::divider_drag_axis(8, 9, off(203, 9, 20), 20), 9);
}

fn pane_header_point(app: &crate::App, wid: crate::WindowId, index: usize) -> (f64, f64) {
    let plan = app.active_visible_leaf_plan(wid).expect("split plan");
    let header = plan.leaves[index].header.expect("pane header");
    app.cell_centre_px(wid, header.origin.y as u16, header.origin.x as u16 + 1)
        .expect("header cell")
}

#[test]
fn pane_header_click_focuses_without_selecting_or_reporting_any_button() {
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let left = app.focused_session_id(wid).unwrap();
    app.split_active_stub_tab(wid);
    let header = pane_header_point(&app, wid, 0);
    app.on_cursor_moved(wid, header.0, header.1);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert_eq!(app.focused_session_id(wid), Some(left));
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(!app.windows[&wid].selecting);

    let term = app.front_terminal(wid).unwrap().term.clone();
    for tracking in [false, true] {
        crate::term_lock(&term).process(if tracking {
            b"\x1b[?1003h"
        } else {
            b"\x1b[?1003l"
        });
        for button in [
            MouseButton::Left,
            MouseButton::Middle,
            MouseButton::Right,
            MouseButton::Back,
            MouseButton::Forward,
        ] {
            app.on_mouse_input(wid, ElementState::Pressed, button);
            assert_eq!(app.windows[&wid].reported_buttons, 0);
            assert!(!app.windows[&wid].selecting);
            app.on_mouse_input(wid, ElementState::Released, button);
            assert!(app.windows[&wid].held_mouse_button.is_none());
        }
    }
    crate::term_lock(&term).process(b"\x1b[?1003l");
    let (row, col, _, _) = app.focused_pane_rect(wid);
    let content = app.cell_centre_px(wid, row, col + 1).unwrap();
    app.on_cursor_moved(wid, content.0, content.1);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert!(app.windows[&wid].selecting);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
}

#[test]
fn pane_header_hover_and_wheel_never_reach_terminal_input() {
    use aterm_window::event::MouseScrollDelta;

    let (mut app, wid, _, _) = stacked_autoscroll_fixture();
    let term = app.front_terminal(wid).unwrap().term.clone();
    let header = pane_header_point(&app, wid, 1);
    for tracking in [false, true] {
        crate::term_lock(&term).process(if tracking {
            b"\x1b[?1003h"
        } else {
            b"\x1b[?1003l"
        });
        let before = crate::term_lock_acquisitions_on_this_thread();
        app.on_cursor_moved(wid, header.0, header.1);
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.5));
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.5, 0.0));
        app.on_mouse_wheel(
            wid,
            MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(0.0, 0.5)),
        );
        assert_eq!(
            crate::term_lock_acquisitions_on_this_thread(),
            before,
            "header hover/wheel never enters the terminal seam, including any-motion tracking"
        );
        let window = &app.windows[&wid];
        assert_eq!(
            (window.scroll_residual, window.scroll_residual_x),
            (0.0, 0.0)
        );
        assert!(window.scroll_glide.is_none());
        assert!(!window.hover_grid_owned);
        assert!(window.hover_pointer && !window.native_text_cursor);
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
    }
    assert!(
        app.pane_header_at_with(wid, app.pointer_geometry(wid), -1.0, header.1)
            .is_none()
    );
    assert!(
        app.pane_header_at_with(wid, app.pointer_geometry(wid), header.0, -1.0)
            .is_none()
    );
}

#[test]
fn pane_header_crossing_preserves_selection_and_reported_button_releases() {
    use aterm_window::event::{ElementState, MouseButton};

    let (mut app, wid, _, bottom) = stacked_autoscroll_fixture();
    let (row, col, _, _) = app.focused_pane_rect(wid);
    let content = app.cell_centre_px(wid, row + 1, col + 2).unwrap();
    let header = pane_header_point(&app, wid, 1);
    let term = app.front_terminal(wid).unwrap().term.clone();
    for tracking in [false, true] {
        crate::term_lock(&term).process(if tracking {
            b"\x1b[?1003h"
        } else {
            b"\x1b[?1003l"
        });
        app.on_cursor_moved(wid, content.0, content.1);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert_eq!(app.windows[&wid].selecting, !tracking);
        assert_ne!(app.windows[&wid].reported_buttons, 0);
        app.on_cursor_moved(wid, header.0, header.1);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert!(!app.windows[&wid].selecting);
        assert!(app.windows[&wid].held_mouse_button.is_none());
        assert_eq!(app.windows[&wid].reported_buttons, 0);
        assert_eq!(app.focused_session_id(wid), Some(bottom));
    }
}

#[test]
fn pane_header_focuses_native_leaf_and_does_not_scroll_its_content() {
    use crate::native_app::AppViewState;
    use aterm_window::event::{ElementState, MouseButton, MouseScrollDelta};

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native) = app.active_native_view(wid).unwrap();
    app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let header = pane_header_point(&app, wid, 0);
    app.on_cursor_moved(wid, header.0, header.1);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_eq!(
        app.active_native_view(wid).map(|(_, view)| view),
        Some(native)
    );
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(native) else {
        panic!("Settings view");
    };
    settings.scroll_page_to(1);
    app.on_cursor_moved(wid, header.0, header.1);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(native) else {
        panic!("Settings view");
    };
    assert_eq!(settings.page_scroll, 1);
    assert!(settings.common.pressed.is_none());
    assert!(settings.common.hovered.is_none());
    assert!(app.windows[&wid].hover_pointer);
}

#[test]
fn the_pointer_verb_reports_only_where_the_pointer_is() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    assert_eq!(
        app.pointer_cmd(PointerAction::Status),
        Ok(None),
        "a window that has never seen the pointer claims no position"
    );
    assert_eq!(
        app.pointer_cmd(PointerAction::Move { row: 3, col: 5 }),
        Ok(Some((3, 5)))
    );
    let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
    for (row, col) in [(rows, 0), (0, cols)] {
        assert_eq!(
            app.pointer_cmd(PointerAction::Move { row, col }),
            Err("cell is outside the grid".to_string())
        );
    }
    assert_eq!(
        app.pointer_cmd(PointerAction::Status),
        Ok(Some((3, 5))),
        "a refused move left the pointer where it was"
    );
    assert_eq!(app.pointer_cmd(PointerAction::Leave), Ok(None));
    assert_eq!(
        app.pointer_cmd(PointerAction::Status),
        Ok(None),
        "the departed pointer's last cell must not be reported as where it is"
    );
    assert_eq!(app.windows[&wid].last_mouse_window_cell, (3, 5));
}

#[test]
fn every_cell_maps_to_a_pixel_that_maps_back_to_that_cell() {
    let app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
    for row in 0..rows {
        for col in 0..cols {
            let (x, y) = app
                .cell_centre_px(wid, row, col)
                .expect("an in-grid cell has a centre");
            assert_eq!(
                app.pixel_to_cell(wid, x, y),
                (row, col),
                "cell ({row}, {col}) round-trips through pixel ({x}, {y})"
            );
        }
    }
}

fn native_palette_row_center(app: &crate::App, wid: crate::WindowId, slot: usize) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let head = app.win_head(wid);
    let window = &app.windows[&wid];
    let scale = window.scale.max(f64::EPSILON);
    let geom = crate::settings::SettingsGeom {
        cw: cw as f32 / scale as f32,
        ch: ch as f32 / scale as f32,
        font_px: 13.0,
        cols: window.cols as usize,
        panel_rows: window.overlay_rows(),
        px_per_pt: 1.0,
    };
    let (rx, ry, rw, rh) =
        crate::palette::palette_row_rect(window.palette().expect("open palette"), &geom, slot)
            .expect("visible palette row");
    let (ox, oy) = app.frame_origin(wid);
    (
        ox as f64 + pad as f64 + f64::from(rx + rw * 0.5) * scale,
        oy as f64
            + (head + usize::from(app.chrome_rows(wid)) * ch) as f64
            + f64::from(ry + rh * 0.5) * scale,
    )
}

fn native_content_to_window(
    app: &mut crate::App,
    wid: crate::WindowId,
    x: f32,
    y: f32,
) -> (f64, f64) {
    let view = prepare_presented_native(app, wid);
    let artifact = app
        .retained_native_leaf_artifact(wid, view, true)
        .expect("presented native semantic artifact");
    (
        artifact.device_x as f64 + f64::from(x) * artifact.scale,
        artifact.device_y as f64 + f64::from(y) * artifact.scale,
    )
}

#[track_caller]
fn prepare_presented_native(
    app: &mut crate::App,
    wid: crate::WindowId,
) -> crate::tab_model::ViewId {
    let (_, view) = app.active_native_view(wid).expect("active native view");
    assert!(app.prepare_native_input_scratch(wid));
    app.windows
        .get_mut(&wid)
        .and_then(|window| window.leaf_render_cache.get_mut(&view))
        .and_then(|cache| cache.native.as_mut())
        .expect("retained native raster")
        .presented = true;
    assert!(
        app.retained_native_leaf_artifact(wid, view, true).is_some(),
        "prepared native artifact must satisfy the shipping glass guard"
    );
    view
}

fn presented_native_ui(app: &mut crate::App, wid: crate::WindowId) -> crate::native_ui::CompiledUi {
    let view = prepare_presented_native(app, wid);
    app.retained_native_leaf_artifact(wid, view, true)
        .expect("presented native semantic artifact")
        .compiled
        .clone()
}

fn native_text_boundary_point(
    app: &mut crate::App,
    wid: crate::WindowId,
    key: &crate::native_ui::UiKey,
    byte: usize,
) -> (f64, f64) {
    let compiled = presented_native_ui(app, wid);
    let paint = compiled
        .paint
        .iter()
        .find(|node| &node.key == key)
        .expect("painted text field");
    let crate::native_ui::UiContent::TextField(control) = &paint.content else {
        panic!("target is a text field");
    };
    let text = control.spec.input.as_ref().map_or(
        match &control.value {
            crate::native_ui::SemanticValue::Text(value) => value.as_str(),
            _ => "",
        },
        |input| input.text.as_str(),
    );
    let geometry = crate::native_ui::text_field_geometry(control, paint.rect, 13.0);
    let x = crate::native_ui::text_field_x_for_byte(text, &geometry, byte, 13.0);
    assert_eq!(
        compiled.text_field_byte_at(key, x),
        Some(byte),
        "test point must invert through shipping hit geometry"
    );
    native_content_to_window(app, wid, x, paint.rect.y + paint.rect.height * 0.5)
}

#[test]
fn a_press_on_a_switch_cut_in_the_lists_strip_scrolls_it_in_and_works_nothing() {
    use crate::native_ui::UiContent;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    let wid = WindowId(0);
    // A picture the list's foot cuts: one the strip draws whole works itself
    // ([`crate::native_ui::CompiledUi::picture_hits`]).
    let switch_picture = |compiled: &crate::native_ui::CompiledUi| {
        compiled.list_pictures.iter().find_map(|picture| {
            let paint = compiled.paint.iter().find(|node| node.key == picture.key)?;
            let UiContent::Switch(control) = &paint.content else {
                return None;
            };
            let cut = compiled
                .picture_hits
                .iter()
                .all(|hit| hit.key != picture.key);
            (cut && picture.rect.height >= 12.0).then(|| (picture.clone(), control.value.clone()))
        })
    };
    let mut found = None;
    // Where a row's foot falls under the room's edge is the faces' to say: the sizes macOS's cut
    // a switch at first, then a sweep of window heights for this host's.
    let sizes = [(760, 460), (760, 520), (640, 400), (1_100, 740)]
        .into_iter()
        .chain((360..=900).step_by(7).map(|height| (760, height)));
    'sizes: for (width, height) in sizes {
        let mut app = App::headless_for_test();
        app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(width, height));
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let mut before = None;
        for _ in 0..40 {
            let compiled = presented_native_ui(&mut app, wid);
            if let Some(picture) = switch_picture(&compiled) {
                found = Some((app, picture));
                break 'sizes;
            }
            // A line that moved nothing: the list's end.
            let screen = compiled
                .paint
                .iter()
                .map(|node| (node.key.clone(), node.rect.y.to_bits()))
                .collect::<Vec<_>>();
            if before.as_ref() == Some(&screen) {
                break;
            }
            before = Some(screen);
            let _ = app.dispatch_native_view_event(
                wid,
                view,
                crate::native_app::AppEvent::ScrollLines(1),
            );
        }
    }
    let (mut app, (picture, value)) =
        found.expect("a screen of Top Settings cuts a switch in its strip");
    let (x, y) = native_content_to_window(
        &mut app,
        wid,
        picture.rect.x + picture.rect.width * 0.5,
        picture.rect.y + picture.rect.height * 0.5,
    );
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let compiled = presented_native_ui(&mut app, wid);
    let key = &picture.key;
    let paint = compiled
        .paint
        .iter()
        .find(|node| &node.key == key)
        .expect("the switch is drawn");
    let hit = compiled
        .hits
        .iter()
        .find(|hit| &hit.key == key)
        .unwrap_or_else(|| panic!("{key:?} came into the room, live: {:?}", paint.rect));
    assert_eq!(hit.rect, paint.rect, "{key:?} is whole in the room");
    let semantic = compiled.semantic(key).expect("live");
    assert_eq!(semantic.value, value, "the press worked nothing");
    assert!(
        semantic
            .state
            .is_some_and(|state| !state.busy && !state.pressed),
        "and asked for no change: {:?}",
        semantic.state
    );
}

#[test]
fn a_press_on_a_preview_cut_in_the_lists_strip_scrolls_it_in_and_pauses_nothing() {
    use crate::native_ui::UiContent;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    // A picture the list's foot cuts: one the strip draws whole works itself
    // ([`crate::native_ui::CompiledUi::picture_hits`]).
    let preview_picture = |compiled: &crate::native_ui::CompiledUi| {
        compiled.list_pictures.iter().find_map(|picture| {
            let paint = compiled.paint.iter().find(|node| node.key == picture.key)?;
            let UiContent::SettingsPreview(spec) = &paint.content else {
                return None;
            };
            let cut = compiled
                .picture_hits
                .iter()
                .all(|hit| hit.key != picture.key);
            (cut && picture.rect.height >= 12.0).then(|| (picture.clone(), spec.paused))
        })
    };
    let wid = WindowId(0);
    let mut found = None;
    for (width, height) in [(760, 520), (1_100, 740), (760, 460)] {
        let mut app = App::headless_for_test();
        // Top Settings as it lists with a trail drawn (Windows draws none by default).
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            crate::app_config::trail_drawn(""),
        )
        .unwrap();
        app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(width, height));
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        for _ in 0..12 {
            if let Some(picture) = preview_picture(&presented_native_ui(&mut app, wid)) {
                found = Some((app, picture));
                break;
            }
            let _ = app.dispatch_native_view_event(
                wid,
                view,
                crate::native_app::AppEvent::ScrollLines(1),
            );
        }
        if found.is_some() {
            break;
        }
    }
    let (mut app, (picture, paused)) =
        found.expect("a screen of Top Settings draws a preview in its strip");
    let (x, y) = native_content_to_window(
        &mut app,
        wid,
        picture.rect.x + picture.rect.width * 0.5,
        picture.rect.y + picture.rect.height * 0.5,
    );
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let compiled = presented_native_ui(&mut app, wid);
    let key = &picture.key;
    let paint = compiled
        .paint
        .iter()
        .find(|node| &node.key == key)
        .expect("the preview is drawn");
    let hit = compiled
        .hits
        .iter()
        .find(|hit| &hit.key == key)
        .unwrap_or_else(|| panic!("{key:?} came into the room, live: {:?}", paint.rect));
    assert_eq!(hit.rect, paint.rect, "{key:?} is whole in the room");
    let UiContent::SettingsPreview(spec) = &paint.content else {
        unreachable!()
    };
    assert_eq!(spec.paused, paused, "the press paused nothing");
}

/// A STYLE ROW THE LIST'S STRIP DRAWS WHOLE IS A ROW TO THE POINTER
/// ([`crate::native_ui::CompiledUi::picture_hits`]): at 1074x684 Cursor Styles draws a style
/// whole under its fade; the pointer over it previews it, and one click saves it and brings
/// it whole into the room, live. It was inert: pointing previewed nothing, and a
/// click only scrolled it into the room, saving nothing (lab fv2-cs-d150, Flying kitty).
#[test]
fn a_style_row_whole_under_the_fade_previews_and_saves_in_one_click() {
    use aterm_window::event::{ElementState, MouseButton};

    let (mut app, wid, view) = settings_sized_in(
        crate::App::headless_for_test(),
        crate::native_settings::SettingsRoute::CursorStyles,
        (1_074, 684),
    );
    let compiled = presented_native_ui(&mut app, wid);
    let hit = compiled
        .picture_hits
        .iter()
        .find(|hit| hit.key.as_str().starts_with("settings/cursor-style/"))
        .cloned()
        .expect("the landing draws a style row whole under the fade");
    assert!(compiled.semantic(&hit.key).is_none(), "it is in the strip");
    let style = hit
        .key
        .as_str()
        .strip_prefix("settings/cursor-style/")
        .unwrap()
        .replace('-', " ");
    let (x, y) = native_content_to_window(
        &mut app,
        wid,
        hit.rect.x + hit.rect.width * 0.5,
        hit.rect.y + hit.rect.height * 0.5,
    );
    app.on_cursor_moved(wid, x, y);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.clone()),
        Some(hit.key.clone()),
        "the pointer is on the row"
    );
    let tried = presented_native_ui(&mut app, wid)
        .paint
        .iter()
        .find_map(|node| match &node.content {
            crate::native_ui::UiContent::SettingsPreview(spec) => {
                Some(spec.cursor.effective_trail_style_raw().to_string())
            }
            _ => None,
        })
        .expect("the preview");
    assert!(
        tried.eq_ignore_ascii_case(&style),
        "pointing previews {style:?}, the preview tries {tried:?}"
    );
    let writes = app.native_config_pending.len();
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    // The write goes to aterm.toml off this thread: one click asked for it.
    assert_eq!(
        app.native_config_pending.len(),
        writes + 1,
        "one click saves {style:?}"
    );
    let after = presented_native_ui(&mut app, wid);
    let paint = after
        .paint
        .iter()
        .find(|node| node.key == hit.key)
        .expect("the row is drawn");
    let live = after
        .hits
        .iter()
        .find(|found| found.key == hit.key)
        .unwrap_or_else(|| panic!("{:?} came into the room, live: {:?}", hit.key, paint.rect));
    assert_eq!(live.rect, paint.rect, "whole in the room");
    if let Some(bar) = after
        .paint
        .iter()
        .find(|node| node.key.as_str() == "settings/feedback")
    {
        assert!(
            paint.rect.bottom() <= bar.rect.y + 0.5,
            "the bar {:?} covers the row {:?}",
            bar.rect,
            paint.rect
        );
    }
}

#[test]
fn palette_over_native_tab_swallows_pointer_before_underlying_hit_test() {
    use crate::native_app::AppViewState;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("Settings active");
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
        panic!("Settings state");
    };
    settings.search_input = crate::native_text_input::TextInputState::new("écho".to_string());
    settings.search_input.set_selection(2, 2);
    settings.set_search("écho".to_string());
    assert!(app.prepare_native_input_scratch(wid));
    app.windows
        .get_mut(&wid)
        .unwrap()
        .leaf_render_cache
        .get_mut(&view)
        .unwrap()
        .native
        .as_mut()
        .unwrap()
        .presented = true;
    let artifact = app
        .retained_native_leaf_artifact(wid, view, true)
        .expect("presented Settings UI");
    let hit = artifact
        .compiled
        .hits
        .iter()
        .find(|hit| hit.key.as_str() == "settings/search")
        .expect("actionable Settings search field");
    let scale = artifact.scale;
    let x = artifact.device_x as f64 + f64::from(hit.rect.x + hit.rect.width * 0.5) * scale;
    let y = artifact.device_y as f64 + f64::from(hit.rect.y + hit.rect.height * 0.5) * scale;

    let point = app.native_content_point(wid, x, y).unwrap();
    assert!(artifact.compiled.hit_test(point.0, point.1).is_some());
    app.on_cursor_moved(wid, x, y);
    let before_focus = app
        .native_runtime
        .view_state(view)
        .and_then(|state| state.common().last_focus.clone());
    let before_selection = match app.native_runtime.view_state(view).unwrap() {
        AppViewState::Settings(settings) => settings.search_input.selection().clone(),
        _ => unreachable!(),
    };
    assert!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.as_ref())
            .is_some(),
        "underlying hover is live before the modal opens"
    );

    app.palette_enter();
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let common = app.native_runtime.view_state(view).unwrap().common();
    assert!(common.hovered.is_none());
    assert!(common.pressed.is_none());
    assert_eq!(
        common.last_focus, before_focus,
        "click never reached Settings"
    );
    let AppViewState::Settings(settings) = app.native_runtime.view_state(view).unwrap() else {
        unreachable!();
    };
    assert_eq!(settings.search_input.selection(), &before_selection);
    assert!(app.windows[&wid].palette().is_some(), "modal remains open");
    assert!(!app.windows[&wid].native_text_cursor);
    assert!(!app.windows[&wid].hover_pointer);

    app.palette_exit();
    assert!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.as_ref())
            .is_some(),
        "closing immediately restores hover/cursor ownership to revealed content"
    );
}

#[test]
fn settings_text_pointer_click_drag_and_shift_click_use_shipping_reducer_path() {
    use crate::native_app::AppViewState;
    use crate::native_text_input::TextInputState;
    use crate::native_ui::UiKey;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};
    use aterm_window::keyboard::ModifiersState;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("Settings active");
    let suffix = "Aé👩‍💻末";
    let text = format!("{}{}", "long-prefix-".repeat(400), suffix);
    let suffix_start = text.len() - suffix.len();
    let woman_start = suffix_start + "Aé".len();
    let woman_end = woman_start + "👩‍💻".len();
    let e_start = suffix_start + "A".len();
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
        panic!("Settings state");
    };
    settings.search_input = TextInputState::new(text.clone());
    settings.set_search(text);
    settings.common.last_focus = Some(UiKey::new("settings/search"));

    let key = UiKey::new("settings/search");
    let (press_x, press_y) = native_text_boundary_point(&mut app, wid, &key, woman_start);
    app.on_cursor_moved(wid, press_x, press_y);
    assert!(app.windows[&wid].native_text_cursor);
    assert!(!app.windows[&wid].hover_pointer);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
        panic!("Settings state");
    };
    assert_eq!(
        settings.search_input.selection().range(),
        woman_start..woman_start
    );
    assert_eq!(settings.common.pressed.as_ref(), Some(&key));

    let (release_x, release_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
    app.on_cursor_moved(wid, release_x, release_y);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
        panic!("Settings state");
    };
    assert_eq!(settings.search_input.selected_text(), "👩‍💻");
    assert!(settings.common.pressed.is_none());

    let (click_x, click_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
    app.on_cursor_moved(wid, click_x, click_y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    let (click_x, click_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
    app.on_cursor_moved(wid, click_x, click_y);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
        unreachable!();
    };
    assert_eq!(
        settings.search_input.selection().range(),
        woman_end..woman_end
    );

    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SHIFT;
    let (shift_x, shift_y) = native_text_boundary_point(&mut app, wid, &key, e_start);
    app.on_cursor_moved(wid, shift_x, shift_y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    let (shift_x, shift_y) = native_text_boundary_point(&mut app, wid, &key, e_start);
    app.on_cursor_moved(wid, shift_x, shift_y);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
        unreachable!();
    };
    assert_eq!(settings.search_input.selection().anchor, woman_end);
    assert_eq!(settings.search_input.selection().head, e_start);
    assert_eq!(settings.search_input.selected_text(), "é👩‍💻");
}

#[test]
fn settings_color_field_padding_and_swatch_clamp_to_committed_edges() {
    use crate::native_app::AppViewState;
    use crate::native_text_input::TextInputState;
    use crate::native_ui::UiKey;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    let (_, view) = app.active_native_view(wid).expect("Settings active");
    let field_key = crate::prefs::EDIT_FOREGROUND;
    let value = "#123456";
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
        panic!("Settings state");
    };
    settings.search_input = TextInputState::new("foreground".to_string());
    settings.set_search("foreground".to_string());
    settings.scroll_page_to(0);
    settings
        .legacy
        .fields
        .iter_mut()
        .find(|field| field.key == field_key)
        .expect("foreground preference")
        .seed = Some(value.to_string());

    let key = UiKey::new(format!("settings/control/{field_key}"));
    let edge_point = |app: &mut App, trailing: bool| {
        let compiled = presented_native_ui(app, wid);
        let paint = compiled
            .paint
            .iter()
            .find(|node| node.key == key)
            .expect("painted foreground field");
        let x = if trailing {
            paint.rect.right() - 1.0
        } else {
            paint.rect.x + 1.0
        };
        assert_eq!(
            compiled.text_field_byte_at(&key, x),
            Some(if trailing { value.len() } else { 0 })
        );
        native_content_to_window(app, wid, x, paint.rect.y + paint.rect.height * 0.5)
    };

    let (x, y) = edge_point(&mut app, true);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    let (x, y) = edge_point(&mut app, true);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
        unreachable!();
    };
    assert_eq!(settings.editing_field.as_deref(), Some(field_key));
    assert_eq!(
        settings.field_inputs[field_key].selection().range(),
        value.len()..value.len(),
        "swatch click places an end caret"
    );

    let (x, y) = edge_point(&mut app, false);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    let (x, y) = edge_point(&mut app, false);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
        unreachable!();
    };
    assert_eq!(
        settings.field_inputs[field_key].selection().range(),
        0..0,
        "leading padding click places a start caret"
    );

    let (x, y) = edge_point(&mut app, false);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    let (x, y) = edge_point(&mut app, true);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
        unreachable!();
    };
    assert_eq!(settings.field_inputs[field_key].selected_text(), value);
}

#[test]
fn settings_text_release_rejects_a_detached_native_view() {
    use crate::native_app::AppViewState;
    use crate::native_text_input::TextInputState;
    use crate::native_ui::UiKey;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("Settings active");
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
        panic!("Settings state");
    };
    settings.search_input = TextInputState::new("écho".to_string());
    settings.set_search("écho".to_string());
    settings.common.last_focus = Some(UiKey::new("settings/search"));

    let key = UiKey::new("settings/search");
    let (x, y) = native_text_boundary_point(&mut app, wid, &key, 2);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    let (release_x, release_y) = native_text_boundary_point(&mut app, wid, &key, 5);
    app.on_cursor_moved(wid, release_x, release_y);

    let detached = app
        .native_runtime
        .take_view_state(view)
        .expect("detach live Settings view");
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let AppViewState::Settings(settings) = detached else {
        unreachable!();
    };
    assert_eq!(settings.search_input.selection().range(), 2..2);
    assert_eq!(settings.search_input.value(), "écho");
}

#[test]
fn text_field_pointer_payload_does_not_replace_button_slider_or_document_paths() {
    use crate::native_app::{AppKind, AppViewState};
    use crate::native_ui::{UiContent, UiKey};
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, settings_view) = app.active_native_view(wid).expect("Settings active");
    app.dispatch_native_event(
        wid,
        crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/compact-navigation"),
            value: None,
        }),
    )
    .unwrap();
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(settings_view)
    else {
        panic!("Settings state");
    };
    settings.scroll_page_to(0);

    let compiled = presented_native_ui(&mut app, wid);
    let route_button = compiled
        .hits
        .iter()
        .find(|hit| hit.action.as_str() == "settings/route/modified")
        .expect("visible Modified route button");
    let route_key = route_button.key.clone();
    assert_eq!(
        compiled.text_field_byte_at(&route_key, route_button.rect.x),
        None
    );
    let (x, y) = native_content_to_window(
        &mut app,
        wid,
        route_button.rect.x + route_button.rect.width * 0.5,
        route_button.rect.y + route_button.rect.height * 0.5,
    );
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(settings_view)
    else {
        panic!("Settings state");
    };
    assert_eq!(settings.route, crate::native_settings::SettingsRoute::Home);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(settings_view)
    else {
        unreachable!();
    };
    assert_eq!(
        settings.route,
        crate::native_settings::SettingsRoute::Modified
    );

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(settings_view)
    else {
        panic!("same singleton Settings view");
    };
    settings
        .legacy
        .fields
        .iter_mut()
        .find(|field| field.key == crate::prefs::EDIT_FONT_PX)
        .expect("font size field")
        .seed = Some("14".to_string());
    settings.search_input = crate::native_text_input::TextInputState::new("font size".to_string());
    settings.set_search("font size".to_string());
    let slider_key = UiKey::new(format!("settings/control/{}", crate::prefs::EDIT_FONT_PX));
    let mut found = None;
    for scroll in 0..8 {
        let Some(AppViewState::Settings(settings)) =
            app.native_runtime.view_state_mut(settings_view)
        else {
            unreachable!();
        };
        settings.scroll_page_to(scroll);
        let compiled = presented_native_ui(&mut app, wid);
        if let Some(index) = compiled
            .paint
            .iter()
            .position(|node| node.key == slider_key)
        {
            found = Some((compiled, index));
            break;
        }
    }
    let (compiled, index) = found.expect("painted font slider");
    let slider = &compiled.paint[index];
    assert!(matches!(slider.content, UiContent::Slider(_)));
    let geometry = crate::native_ui::slider_geometry(slider.rect);
    let slider_x = geometry.track_x + (geometry.track_right - geometry.track_x) * 0.75;
    assert!(compiled.slider_value_at(&slider_key, slider_x).is_some());
    assert_eq!(compiled.text_field_byte_at(&slider_key, slider_x), None);
    let (x, y) = native_content_to_window(
        &mut app,
        wid,
        slider_x,
        slider.rect.y + slider.rect.height * 0.5,
    );
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(settings_view)
    else {
        unreachable!();
    };
    assert_ne!(
        settings.editing_field.as_deref(),
        Some(crate::prefs::EDIT_FONT_PX),
        "slider remains numeric rather than becoming a text editor"
    );

    let dir = std::env::temp_dir().join(format!(
        "aterm-pointer-negative-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pointer.txt");
    std::fs::write(&path, "alpha βeta\nsecond\n").unwrap();
    let uri = file_uri(&path);
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    let (editor_instance, editor_view) = app.active_native_view(wid).expect("Editor active");
    let document = app.native_runtime.document_id(editor_instance).unwrap();
    let before = app.document_store.snapshot(document).unwrap();
    let compiled = presented_native_ui(&mut app, wid);
    let editor_key = UiKey::new("editor/buffer");
    let editor = compiled
        .paint
        .iter()
        .find(|node| node.key == editor_key)
        .expect("painted editor viewport");
    let UiContent::TextViewport(spec) = &editor.content else {
        panic!("editor body remains a text viewport");
    };
    let geometry = spec.geometry(editor.rect);
    let content_x = geometry.text_x + 36.0;
    let content_y = geometry.body_y + geometry.line_h * 0.5;
    let expected = crate::native_ui::text_viewport_byte_at(spec, editor.rect, content_x, content_y)
        .expect("materialized source byte");
    assert_eq!(compiled.text_field_byte_at(&editor_key, content_x), None);
    let (x, y) = native_content_to_window(&mut app, wid, content_x, content_y);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
        panic!("Editor state");
    };
    assert_eq!(
        editor.buffer.as_ref().unwrap().primary_selection().head,
        expected
    );
    assert_eq!(editor.common.last_focus.as_ref(), Some(&editor_key));
    assert!(
        !editor.common.focus_visible,
        "pointer focus keeps its quiet style"
    );
    let selected = editor.buffer.as_ref().unwrap().primary_selection().clone();
    let mut failures = Vec::new();
    let clicked = presented_native_ui(&mut app, wid);
    let viewport = clicked
        .paint
        .iter()
        .find(|node| node.key == editor_key)
        .unwrap();
    let UiContent::TextViewport(spec) = &viewport.content else {
        panic!("editor viewport")
    };
    if !spec.focused {
        failures.push("pointer-focused buffer did not retain actual input focus");
    }
    if clicked.ime_caret_rect().is_none() {
        failures.push("pointer-focused buffer supplied no IME caret anchor");
    }
    let prims = clicked.tray(aterm_render::Theme::default(), 13.0).prims;
    assert!(
        !prims.iter().any(|prim| matches!(prim,
            crate::widget::DrawPrim::Stroke { x, y, w, h, radius, .. }
                if (*x, *y, *w, *h, *radius) == (
                    viewport.rect.x + 0.5, viewport.rect.y + 0.5,
                    viewport.rect.width - 1.0, viewport.rect.height - 1.0, 10.0
                )
        )),
        "a pointer focus does not acquire the keyboard focus ring"
    );
    app.on_ime_preedit(wid, "日".to_string(), Some((0, 0)));
    let composing = presented_native_ui(&mut app, wid);
    if composing.ime_caret_rect().is_none() {
        failures.push("pointer-owned composition supplied no IME caret anchor");
    }
    let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
        panic!("Editor state")
    };
    assert_eq!(editor.preedit, "日");
    assert_eq!(
        editor.buffer.as_ref().unwrap().primary_selection(),
        &selected
    );
    app.on_ime_preedit(wid, String::new(), None);
    let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
        panic!("Editor state")
    };
    assert!(editor.preedit.is_empty());
    assert_eq!(
        editor.buffer.as_ref().unwrap().primary_selection(),
        &selected
    );
    assert!(!editor.common.focus_visible);
    let mut toolbar_failures = Vec::new();
    let inspect_anchor = |compiled: &crate::native_ui::CompiledUi,
                          prompt: bool,
                          label: &str,
                          failures: &mut Vec<String>| {
        let viewport = compiled
            .paint
            .iter()
            .find(|node| node.key == editor_key)
            .unwrap();
        let UiContent::TextViewport(spec) = &viewport.content else {
            panic!("editor viewport")
        };
        assert!(!spec.focused, "the toolbar retains control focus: {label}");
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        assert!(
            !prims.iter().any(|prim| matches!(prim,
                crate::widget::DrawPrim::Stroke { x, y, w, h, radius, .. }
                    if (*x, *y, *w, *h, *radius) == (
                        viewport.rect.x + 0.5, viewport.rect.y + 0.5,
                        viewport.rect.width - 1.0, viewport.rect.height - 1.0, 10.0
                    )
            )),
            "toolbar focus does not give the viewport an outer ring: {label}"
        );
        if let Some(caret) = compiled.ime_caret_rect() {
            let geometry = spec.geometry(viewport.rect);
            assert_eq!(
                caret.y >= geometry.body_y + geometry.body_h,
                prompt,
                "{label}"
            );
            if !prims.iter().any(|prim| {
                matches!(prim,
                    crate::widget::DrawPrim::Stroke { x, y, w, h, .. }
                        if (*x, *y, *w, *h) == (caret.x, caret.y, caret.width, caret.height)
                )
            }) {
                failures.push(format!("{label}: the text target's caret is not painted"));
            }
        } else {
            failures.push(format!("{label}: the text target has no IME anchor"));
        }
    };
    let key = |key, mods| crate::input::InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    };
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let abort = key(Key::Character('g'), Modifiers::CTRL);
    for button in [
        "editor/find-button",
        "editor/goto-line-button",
        "editor/commands-button",
    ] {
        let compiled = presented_native_ui(&mut app, wid);
        let hit = compiled
            .hits
            .iter()
            .find(|hit| hit.key.as_str() == button)
            .expect("enabled toolbar button");
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            hit.rect.x + hit.rect.width * 0.5,
            hit.rect.y + hit.rect.height * 0.5,
        );
        app.on_cursor_moved(wid, x, y);
        for state in [ElementState::Pressed, ElementState::Released] {
            app.mouse_input_then_fold(wid, state, MouseButton::Left);
        }
        let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
            panic!("Editor state")
        };
        assert_eq!(
            editor.common.last_focus.as_ref().map(UiKey::as_str),
            Some(button)
        );
        assert!(
            !editor.common.focus_visible,
            "pointer toolbar focus stays quiet"
        );
        use crate::native_editor::Minibuffer;
        assert!(
            matches!(
                (button, &editor.buffer.as_ref().unwrap().minibuffer),
                ("editor/find-button", Minibuffer::Search { .. })
                    | ("editor/goto-line-button", Minibuffer::GotoLine { .. })
                    | ("editor/commands-button", Minibuffer::Command { .. })
            ),
            "the actual toolbar click opens its prompt: {button}"
        );
        let compiled = presented_native_ui(&mut app, wid);
        inspect_anchor(&compiled, true, button, &mut toolbar_failures);
        assert!(app.native_input_event(wid, &crate::input::InputEvent::Text("λ".to_string())));
        let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
            panic!("Editor state")
        };
        let query = match &editor.buffer.as_ref().unwrap().minibuffer {
            Minibuffer::Search { query, .. }
            | Minibuffer::GotoLine { query, .. }
            | Minibuffer::Command { query, .. } => query,
            _ => panic!("toolbar prompt remains open"),
        };
        assert_eq!(query, "λ", "text still belongs to this prompt: {button}");
        app.on_ime_preedit(wid, "日".to_string(), Some((0, 0)));
        let compiled = presented_native_ui(&mut app, wid);
        inspect_anchor(&compiled, true, button, &mut toolbar_failures);
        let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
            panic!("Editor state")
        };
        assert_eq!(editor.preedit, "日");
        assert_eq!(
            editor.buffer.as_ref().unwrap().primary_selection(),
            &selected
        );
        app.on_ime_preedit(wid, String::new(), None);
        app.on_ime_commit(wid, String::new());
        assert!(app.native_input_event(wid, &abort));
        let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
            panic!("Editor state")
        };
        assert!(editor.preedit.is_empty());
        assert!(!editor.buffer.as_ref().unwrap().minibuffer_active());
        assert_eq!(
            editor.buffer.as_ref().unwrap().primary_selection(),
            &selected
        );
    }
    for prompt in [false, true] {
        app.focus_native_control(wid, editor_view, editor_key.clone(), false)
            .unwrap();
        if prompt {
            app.dispatch_native_event(
                wid,
                crate::native_app::AppEvent::EditorCommand(
                    crate::native_editor::EditorCommand::IncrementalSearch,
                ),
            )
            .unwrap();
        }
        let navigation = if prompt {
            key(Key::Named(NamedKey::Tab), Modifiers::SHIFT)
        } else {
            key(Key::Named(NamedKey::F6), Modifiers::empty())
        };
        assert!(app.native_input_event(wid, &navigation));
        let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
            panic!("Editor state")
        };
        assert_eq!(
            editor.common.last_focus.as_ref().map(UiKey::as_str),
            Some("editor/find-button")
        );
        assert!(
            editor.common.focus_visible,
            "keyboard navigation retains its toolbar ring"
        );
        let label = if prompt {
            "search then Shift+Tab"
        } else {
            "document then F6"
        };
        let compiled = presented_native_ui(&mut app, wid);
        inspect_anchor(&compiled, prompt, label, &mut toolbar_failures);
        app.on_ime_preedit(wid, "日".to_string(), Some((0, 0)));
        let compiled = presented_native_ui(&mut app, wid);
        inspect_anchor(&compiled, prompt, label, &mut toolbar_failures);
        let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
            panic!("Editor state")
        };
        assert_eq!(editor.preedit, "日");
        assert_eq!(
            editor.buffer.as_ref().unwrap().primary_selection(),
            &selected
        );
        app.on_ime_preedit(wid, String::new(), None);
        app.on_ime_commit(wid, String::new());
        if prompt {
            assert!(app.native_input_event(wid, &abort));
        }
    }
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!((after.seq, after.text), (before.seq, before.text));
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "alpha βeta\nsecond\n"
    );
    assert!(app.structural_invariants_ok());
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        toolbar_failures.is_empty(),
        "{}",
        toolbar_failures.join("\n")
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn wheel_scrolled_native_palette_hit_activates_the_newly_painted_row() {
    use crate::native_app::AppViewState;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton, MouseScrollDelta};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("Settings active");
    assert!(app.prepare_native_input_scratch(wid));
    app.palette_enter();
    for c in "Settings:".chars() {
        app.palette_filter_push(c);
    }
    let palette = app.windows[&wid].palette().unwrap();
    let count = palette.filtered().len();
    let body = palette.scroll_extent().2;
    assert!(count > body, "Settings commands exceed the visible band");
    let about_index = palette
        .controls_lines()
        .iter()
        .filter(|line| line.starts_with("menu row "))
        .position(|line| line.contains("action=settings/route/about"))
        .expect("About route command");
    let wanted_scroll = about_index.saturating_sub(body - 1);
    assert!(wanted_scroll > 0, "About route exercises wheel scrolling");
    let about_slot = about_index - wanted_scroll;
    let before_page_scroll = match app.native_runtime.view_state(view).unwrap() {
        AppViewState::Settings(settings) => settings.page_scroll,
        _ => panic!("Settings state"),
    };

    let (x, y) = native_palette_row_center(&app, wid, about_slot);
    app.on_cursor_moved(wid, x, y);
    assert!(app.windows[&wid].palette().unwrap().pointer_over_row());
    assert!(
        app.windows[&wid].hover_pointer,
        "row advertises pointer affordance"
    );
    app.on_mouse_wheel(
        wid,
        MouseScrollDelta::LineDelta(0.0, -(wanted_scroll as f32)),
    );
    let palette = app.windows[&wid].palette().unwrap();
    assert_eq!(palette.scroll_extent().0, wanted_scroll);
    assert!(matches!(
        palette.selected_target(),
        Some(crate::palette::PaletteTarget::Native(target))
            if target.action.as_str() == "settings/route/about"
    ));
    assert_eq!(
        match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Settings(settings) => settings.page_scroll,
            _ => unreachable!(),
        },
        before_page_scroll,
        "modal wheel never reaches the native Settings scroller"
    );

    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert!(app.windows[&wid].palette().is_some(), "press only arms");
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(
        app.windows[&wid].palette().is_none(),
        "matched release activates"
    );
    let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
        panic!("Settings state");
    };
    assert_eq!(settings.route, crate::native_settings::SettingsRoute::About);
}

#[test]
fn pointer_release_rejects_a_native_command_after_its_view_generation_dies() {
    use crate::native_app::AppViewState;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("Settings active");
    assert!(app.prepare_native_input_scratch(wid));
    app.palette_enter();
    for c in "Settings: Go to Themes".chars() {
        app.palette_filter_push(c);
    }
    assert_eq!(app.windows[&wid].palette().unwrap().filtered().len(), 1);
    let (x, y) = native_palette_row_center(&app, wid, 0);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);

    let detached = app
        .native_runtime
        .take_view_state(view)
        .expect("detach live Settings view");
    let before_route = match &detached {
        AppViewState::Settings(settings) => settings.route,
        _ => panic!("Settings state"),
    };
    assert_eq!(app.native_runtime.view_generation(view), None);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);

    assert!(app.windows[&wid].palette().is_none(), "release was settled");
    let AppViewState::Settings(settings) = detached else {
        panic!("Settings state");
    };
    assert_eq!(
        settings.route, before_route,
        "stale generation receives no route action"
    );
}

#[test]
fn terminal_palette_pointer_strips_frame_remainder_and_top_origin_once() {
    use crate::{App, WindowId};
    use aterm_window::dpi::PhysicalSize;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.backend.set_pad(12);
    app.backend.set_pad_top(4);
    app.backend.set_head(3);
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.metrics.pad = 12;
        ws.metrics.pad_top = 4;
        ws.input_scratch.cells = vec![Vec::new(); 24];
    }
    app.palette_enter();
    assert_eq!(
        (app.win_pad(wid), app.win_pad_top(wid), app.win_head(wid)),
        (12, 4, 3),
        "fixture geometry reaches the pointer seams"
    );

    let (cw, ch) = app.win_cell_size(wid);
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (
            usize::from(ws.rows) + usize::from(app.chrome_rows(wid)),
            usize::from(ws.cols),
        )
    };
    let rw = cw.saturating_sub(1).max(4);
    let rh = ch.saturating_sub(1).max(4);
    app.windows.get_mut(&wid).unwrap().win_px = Some(PhysicalSize::new(
        (cols * cw + 24 + rw) as u32,
        (rows * ch + 4 + 12 + 3 + rh) as u32,
    ));
    let (ox, oy) = app.frame_origin(wid);
    assert!(ox > 0, "fixture needs the horizontal remainder band");
    if cfg!(target_os = "linux") {
        assert_eq!(oy, 0, "Linux pins the frame top");
    } else {
        assert!(oy > 0, "fixture needs the vertical remainder band");
    }

    let window = &app.windows[&wid];
    let palette = window.palette().expect("open palette");
    let geom = crate::settings::SettingsGeom {
        cw: cw as f32,
        ch: ch as f32,
        font_px: app.win_font_px(wid),
        cols,
        panel_rows: window.overlay_rows(),
        px_per_pt: window.scale as f32,
    };
    let visible = 3;
    let to_window = |x: f32, y: f32| {
        (
            ox as f64 + 12.0 + f64::from(x),
            oy as f64 + 4.0 + 3.0 + f64::from(y),
        )
    };
    for slot in 0..visible {
        let (rx, ry, rw, rh) =
            crate::palette::palette_row_rect(palette, &geom, slot).expect("painted row");
        let want = Some(slot);
        for (cx, cy, what) in [
            (rx + 0.5, ry + 0.5, "top-left corner"),
            (rx + rw - 0.5, ry + rh - 0.5, "bottom-right corner"),
            (rx + rw * 0.5, ry + rh * 0.5, "centre"),
        ] {
            let (x, y) = to_window(cx, cy);
            assert_eq!(
                app.palette_row_at_pointer(wid, x, y),
                want,
                "slot {slot} {what}: origin stripped exactly once"
            );
        }
        for (cx, cy, band) in [
            (rx - 0.5, ry + rh * 0.5, "left"),
            (rx + rw + 0.5, ry + rh * 0.5, "right"),
            (rx + rw * 0.5, ry - 0.5, "top"),
            (rx + rw * 0.5, ry + rh + 0.5, "bottom"),
        ] {
            let (x, y) = to_window(cx, cy);
            assert_eq!(
                app.palette_row_at_pointer(wid, x, y),
                None,
                "slot {slot} {band} exterior band misses"
            );
        }
    }
}

/// On a Retina window a pointer lands on a palette row as far right as the row is painted: past
/// where a card capped at 820 device pixels would end, and not one pixel further.
#[test]
fn a_retina_pointer_lands_on_a_palette_row_past_a_device_pixel_cap() {
    use crate::{App, WindowId};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.cols = 400;
        ws.scale = 2.0;
        ws.input_scratch.cells = vec![Vec::new(); 50];
    }
    app.palette_enter();
    let (cw, ch) = app.win_cell_size(wid);
    let window = &app.windows[&wid];
    let painted = crate::settings::SettingsGeom {
        cw: cw as f32,
        ch: ch as f32,
        font_px: app.win_font_px(wid),
        cols: usize::from(window.cols),
        panel_rows: window.overlay_rows(),
        px_per_pt: 2.0,
    };
    let t = app
        .overlay_coordinate_transform(wid)
        .expect("the open palette's transform");
    assert_eq!(t.scale, 1.0, "the terminal route: device-pixel geometry");
    let (ox, oy) = app.frame_origin(wid);
    let to_window = |x: f32, y: f32| {
        (
            ox as f64 + t.origin_x + f64::from(x),
            oy as f64 + t.origin_y + f64::from(y),
        )
    };
    let palette = window.palette().expect("open palette");
    for slot in 0..3 {
        let (rx, ry, rw, rh) =
            crate::palette::palette_row_rect(palette, &painted, slot).expect("a painted row");
        assert!(
            rw > 820.0,
            "the 2x row ({rw} px) reaches past a card capped at 820 px"
        );
        let (x, y) = to_window(rx + rw - 0.5, ry + rh * 0.5);
        assert_eq!(
            app.palette_row_at_pointer(wid, x, y),
            Some(slot),
            "slot {slot}'s right end"
        );
        let (x, y) = to_window(rx + rw + 0.5, ry + rh * 0.5);
        assert_eq!(
            app.palette_row_at_pointer(wid, x, y),
            None,
            "slot {slot}: past its painted right end"
        );
    }
}

/// `pointer move item=<n>` aims where the hover reads row `n` back on a window whose frame
/// sits inside a larger window (a non-zero frame origin), on the terminal route and over a
/// Settings page on a Retina window, where the card's geometry is points the transform scales
/// by 2: each aim, read back through the hover's own path ([`App::overlay_card_point`]), is
/// the centre of that row and selects it (§14.16).
#[test]
fn an_item_aim_is_the_rows_centre_on_a_letterboxed_or_retina_window() {
    use crate::{App, WindowId};
    use aterm_window::dpi::PhysicalSize;

    let wid = WindowId(0);
    let letterbox = |app: &mut App| {
        let (cw, ch) = app.win_cell_size(wid);
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (
                usize::from(ws.rows) + usize::from(app.chrome_rows(wid)),
                usize::from(ws.cols),
            )
        };
        let (pad, top) = (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid));
        app.windows.get_mut(&wid).expect("the window").win_px = Some(PhysicalSize::new(
            (cols * cw + 2 * pad + cw - 1) as u32,
            (rows * ch + top + pad + ch - 1) as u32,
        ));
        assert!(
            app.frame_origin(wid).0 > 0,
            "the frame sits inside its window"
        );
    };
    let mut terminal = App::headless_for_test();
    terminal
        .windows
        .get_mut(&wid)
        .expect("the window")
        .input_scratch
        .cells = vec![Vec::new(); 24];
    terminal.palette_enter();
    letterbox(&mut terminal);
    let mut retina = App::headless_for_test();
    assert!(retina.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    retina.windows.get_mut(&wid).expect("the window").scale = 2.0;
    retina.palette_enter();
    letterbox(&mut retina);
    assert_eq!(
        retina.overlay_coordinate_transform(wid).map(|t| t.scale),
        Some(2.0),
        "over a page the card's points are scaled to the window's pixels"
    );
    for (what, app) in [
        ("letterboxed", &terminal),
        ("Retina, over Settings", &retina),
    ] {
        let palette = app.windows[&wid].palette().expect("the open palette");
        let mut rows = 0;
        while let Ok((x, y)) = app.overlay_card_item_px(wid, rows) {
            let (t, lx, ly) = app
                .overlay_card_point(wid, x, y)
                .expect("the card's transform");
            let (cx, cy) = crate::palette::palette_row_centre(palette, &t.geom, rows)
                .expect("a row the card shows");
            assert!(
                (lx - cx).abs() < 1e-3 && (ly - cy).abs() < 1e-3,
                "{what}: item {rows} aims at ({x}, {y}), which the hover reads as card point \
                 ({lx}, {ly}), not the row's centre ({cx}, {cy})"
            );
            assert_eq!(
                app.palette_row_at_pointer(wid, x, y),
                Some(rows),
                "{what}: item {rows}"
            );
            rows += 1;
        }
        assert!(rows >= 3, "{what}: the card shows {rows} rows");
    }
}

#[test]
fn click_below_terminal_palette_is_swallowed_without_starting_selection() {
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.palette_enter();
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let head = app.win_head(wid);
    let panel_rows = app.windows[&wid].overlay_rows();
    let (ox, oy) = app.frame_origin(wid);
    let x = ox as f64 + (pad + cw * 3) as f64;
    let y = oy as f64 + (pad + head + ch * (panel_rows + 2)) as f64;
    assert_eq!(app.palette_row_at_pointer(wid, x, y), None);
    let before_cell = app.windows[&wid].last_mouse_cell;

    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);

    let window = &app.windows[&wid];
    assert!(
        window.palette().is_some(),
        "outside click does not dismiss modal"
    );
    assert!(
        !window.selecting,
        "terminal selection gesture never started"
    );
    assert_eq!(
        window.last_mouse_cell, before_cell,
        "terminal pointer seam was bypassed"
    );
    assert!(
        !crate::term_lock(&window.front_terminal().expect("front terminal").term)
            .text_selection()
            .has_selection()
    );
}

#[test]
fn palette_wheel_never_reaches_terminal_scrollback() {
    use crate::{App, WindowId};
    use aterm_window::event::MouseScrollDelta;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut terminal = crate::term_lock(&term);
        for line in 0..100 {
            terminal.process(format!("history {line}\r\n").as_bytes());
        }
        assert!(terminal.grid().scrollback_lines() > 0);
        assert_eq!(terminal.grid().display_offset(), 0);
    }

    app.palette_enter();
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
    assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
    assert!(
        app.windows[&wid].scroll_glide.is_none(),
        "modal wheel cannot arm terminal smooth scrolling"
    );

    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.75));
    app.palette_exit();
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.5));
    assert!(app.windows[&wid].scroll_glide.is_none());
    assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);

    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
    assert!(
        app.windows[&wid].scroll_glide.is_some()
            || crate::term_lock(&term).grid().display_offset() > 0
    );
}

#[test]
fn editor_pointer_selection_updates_only_the_exact_active_native_view() {
    use crate::native_app::{AppKind, AppViewState};
    use crate::{App, WindowId};

    let dir = std::env::temp_dir().join(format!(
        "aterm-editor-pointer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pointer.md");
    std::fs::write(&path, "alpha\nβeta\n").unwrap();
    let uri = file_uri(&path);

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    let (_, view) = app.active_native_view(wid).expect("editor active");

    assert!(app.native_editor_pointer_select(wid, view, 5, false, 12));
    assert!(app.native_editor_pointer_select(wid, view, 1, true, 12));
    let selection = match app.native_runtime.view_state(view).unwrap() {
        AppViewState::Editor(state) => state.buffer.as_ref().unwrap().primary_selection().clone(),
        _ => panic!("editor view changed kind"),
    };
    assert_eq!(selection.anchor, 5);
    assert_eq!(selection.head, 1);

    app.switch_tab_in(wid, 0);
    assert!(!app.native_editor_pointer_select(wid, view, 0, false, 12));
    let after = match app.native_runtime.view_state(view).unwrap() {
        AppViewState::Editor(state) => state.buffer.as_ref().unwrap().primary_selection().clone(),
        _ => panic!("editor view changed kind"),
    };
    assert_eq!(after, selection);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn enum_chip_click_opens_the_choice_picker_and_commits_nothing() {
    use crate::native_ui::UiContent;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Sound));
    let key = crate::native_ui::UiKey::new(format!(
        "settings/control/{}",
        crate::prefs::EDIT_TRAIL_SOUND_STYLE
    ));
    let mut compiled = presented_native_ui(&mut app, wid);
    for _ in 0..40 {
        // On screen, not in the peek of the next stop below.
        if compiled
            .paint
            .iter()
            .any(|node| node.key == key && !node.peek)
        {
            break;
        }
        app.dispatch_native_event(wid, crate::native_app::AppEvent::ScrollLines(1))
            .unwrap();
        compiled = presented_native_ui(&mut app, wid);
    }
    let chip = compiled
        .paint
        .iter()
        .find(|node| node.key == key && !node.peek)
        .expect("enum control painted");
    let UiContent::Button(control) = &chip.content else {
        panic!("popup-chip enum row paints a Button, got another control");
    };
    let committed = control.spec.label.clone();
    let chip_rect = chip.rect;

    let click = |app: &mut App, x: f64, y: f64| {
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    };

    let (x, y) = native_content_to_window(
        &mut app,
        wid,
        chip_rect.x + chip_rect.width / 2.0,
        chip_rect.y + chip_rect.height / 2.0,
    );
    click(&mut app, x, y);
    let open = presented_native_ui(&mut app, wid);
    assert!(
        open.paint.iter().any(|node| {
            node.key.as_str()
                == format!(
                    "settings/choice-picker/{}",
                    crate::prefs::EDIT_TRAIL_SOUND_STYLE
                )
        }),
        "a click on a popup-chip enum row opens the ChoicePicker"
    );

    let done = open
        .paint
        .iter()
        .find(|node| node.key.as_str() == "settings/choice-close")
        .expect("picker paints its Cancel control");
    let (dx, dy) = native_content_to_window(
        &mut app,
        wid,
        done.rect.x + done.rect.width / 2.0,
        done.rect.y + done.rect.height / 2.0,
    );
    click(&mut app, dx, dy);
    let closed = presented_native_ui(&mut app, wid);
    assert!(
        !closed
            .paint
            .iter()
            .any(|node| node.key.as_str().starts_with("settings/choice-picker/")),
        "Cancel dismisses the pull-down"
    );
    let chip = closed
        .paint
        .iter()
        .find(|node| node.key == key)
        .expect("enum control repaints after dismissal");
    let UiContent::Button(control) = &chip.content else {
        panic!("popup-chip enum row paints a Button, got another control");
    };
    assert_eq!(
        control.spec.label, committed,
        "opening + dismissing the pull-down never cycles the committed value"
    );
}

#[test]
fn markdown_divider_drag_outside_its_leaf_clamps_without_changing_document_identity() {
    use crate::native_app::{AppKind, AppViewState, MarkdownWorkspaceMode, NativeApp};
    use crate::{App, WindowId};

    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = dir.path().join("divider.md");
    std::fs::write(&path, "# Heading\n\nA paragraph.\n").unwrap();
    let wid = WindowId(0);
    for (width, stacked) in [(1400, false), (500, true)] {
        let mut app = App::headless_for_test();
        app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(width, 900));
        app.open_document_tab(AppKind::Markdown, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let snapshot = app.document_store.snapshot(document).unwrap();
        let generation = app.native_runtime.view_generation(view);
        let tabs = app.windows[&wid].tab_set.len();
        assert!(app.native_runtime.set_editor_markdown_mode(
            instance,
            view,
            MarkdownWorkspaceMode::Split
        ));
        let viewport = app.native_ui_viewport_for(wid, view).unwrap();
        let layout = match (
            app.native_runtime.app(instance),
            app.native_runtime.view_state(view),
        ) {
            (Some(NativeApp::Editor(editor)), Some(AppViewState::Editor(state))) => {
                editor.workspace_layout(state, viewport)
            }
            _ => panic!("Markdown editor"),
        };
        assert_eq!(layout.stacked, stacked);
        let divider = layout.divider.unwrap();
        let center = (
            divider.x + divider.width / 2.0,
            divider.y + divider.height / 2.0,
        );
        let (x, y) = native_content_to_window(&mut app, wid, center.0, center.1);
        assert!(app.markdown_workspace_pointer(wid, x, y, None, true));
        for (outside, expected) in [
            (-500.0, 200),
            (viewport.width.max(viewport.height) + 500.0, 800),
        ] {
            let (x, y) = if stacked {
                native_content_to_window(&mut app, wid, center.0, outside)
            } else {
                native_content_to_window(&mut app, wid, outside, center.1)
            };
            assert!(
                app.retained_native_leaf_at_pointer(wid, x, y).is_none(),
                "the pointer is outside every native leaf"
            );
            assert!(app.markdown_workspace_pointer(wid, x, y, None, false));
            let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
                panic!("editor");
            };
            assert_eq!(state.markdown.as_ref().unwrap().split_per_mille, expected);
            assert_eq!(app.active_native_view(wid), Some((instance, view)));
            assert_eq!(app.native_runtime.view_generation(view), generation);
            assert_eq!(app.windows[&wid].tab_set.len(), tabs);
            let after = app.document_store.snapshot(document).unwrap();
            assert_eq!(
                (after.seq, after.text),
                (snapshot.seq, snapshot.text.clone())
            );
        }
        app.settle_native_pointer_press(wid);
        prepare_presented_native(&mut app, wid);
        let layout = match (
            app.native_runtime.app(instance),
            app.native_runtime.view_state(view),
        ) {
            (Some(NativeApp::Editor(editor)), Some(AppViewState::Editor(state))) => {
                editor.workspace_layout(state, viewport)
            }
            _ => panic!("Markdown editor"),
        };
        let source = layout.editor.unwrap();
        let (x, y) = native_content_to_window(&mut app, wid, source.x + 2.0, source.y + 2.0);
        let before = app.native_runtime.view_state(view).unwrap().common();
        let epoch = before.content_epoch;
        let lifetime = before.input_lifetime();
        assert!(!app.markdown_workspace_pointer(wid, x, y, None, true));
        let after = app.native_runtime.view_state(view).unwrap().common();
        assert_eq!(
            after.content_epoch, epoch,
            "clicking the focused pane changes no workspace state"
        );
        assert!(std::sync::Arc::ptr_eq(&lifetime, &after.input_lifetime()));
        let reader = layout.reader.unwrap();
        let (x, y) = native_content_to_window(&mut app, wid, reader.x + 2.0, reader.y + 2.0);
        assert!(!app.markdown_workspace_pointer(wid, x, y, None, true));
        assert!(!std::sync::Arc::ptr_eq(
            &lifetime,
            &app.native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .input_lifetime()
        ));
        assert!(
            app.retained_native_pointer_artifact(wid, view).is_some(),
            "changing pane focus preserves same-press hit geometry"
        );
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn markdown_reader_pointer_keeps_source_composition_until_it_commits() {
    use crate::native_app::{
        AppKind, AppViewState, MarkdownPane, MarkdownWorkspaceMode, NativeApp,
    };
    use crate::{App, WindowId};

    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = dir.path().join("composition.md");
    std::fs::write(&path, "# Heading\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    assert!(app.native_runtime.set_editor_markdown_mode(
        instance,
        view,
        MarkdownWorkspaceMode::Split
    ));
    app.on_ime_preedit(wid, "composed".into(), Some((8, 8)));
    let viewport = app.native_ui_viewport_for(wid, view).unwrap();
    let reader = match (
        app.native_runtime.app(instance),
        app.native_runtime.view_state(view),
    ) {
        (Some(NativeApp::Editor(editor)), Some(AppViewState::Editor(state))) => {
            assert!(
                !state.preedit.is_empty(),
                "the source owns an active composition"
            );
            editor.workspace_layout(state, viewport).reader.unwrap()
        }
        _ => panic!("Markdown editor"),
    };
    let (x, y) = native_content_to_window(&mut app, wid, reader.x + 2.0, reader.y + 2.0);
    let lifetime = app
        .native_runtime
        .view_state(view)
        .unwrap()
        .common()
        .input_lifetime();
    assert!(
        app.markdown_workspace_pointer(wid, x, y, None, true),
        "a blocked focus change consumes the whole reader press"
    );
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor");
    };
    assert_eq!(state.markdown.as_ref().unwrap().focus, MarkdownPane::Editor);
    assert!(std::sync::Arc::ptr_eq(
        &lifetime,
        &state.common.input_lifetime()
    ));
    app.on_ime_commit(wid, "composed".into());
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "composed# Heading\n"
    );
    app.compiled_native_ui(wid)
        .expect("semantic tree after first IME commit");
    prepare_presented_native(&mut app, wid);
    assert!(!app.markdown_workspace_pointer(wid, x, y, None, true));
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor");
    };
    assert!(state.preedit.is_empty());
    assert_eq!(state.markdown.as_ref().unwrap().focus, MarkdownPane::Reader);
    app.compiled_native_ui(wid)
        .expect("semantic tree after reader focus");
    prepare_presented_native(&mut app, wid);
    assert!(app.native_runtime.focus_markdown_editor(instance, view));
    app.on_ime_preedit(wid, "next".into(), Some((4, 4)));
    app.compiled_native_ui(wid)
        .expect("semantic tree after second IME preedit");
    prepare_presented_native(&mut app, wid);
    app.on_ime_commit(wid, "next".into());
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "composednext# Heading\n",
        "the host's source-focus helper settles input identity before the next composition"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn markdown_pointer_selects_the_exact_authored_block_through_semantic_hit_testing() {
    use crate::native_app::{AppKind, AppViewState};
    use crate::native_ui::UiContent;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton};

    let dir = std::env::temp_dir().join(format!(
        "aterm-markdown-pointer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pointer.md");
    let source = "# **Heading**\n\nBody [link](https://example.com).\n";
    std::fs::write(&path, source).unwrap();
    let uri = file_uri(&path);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &uri).unwrap();
    let (_, view) = app.active_native_view(wid).expect("Markdown active");
    let compiled = presented_native_ui(&mut app, wid);
    let block = compiled
        .paint
        .iter()
        .find(|node| {
            matches!(&node.content, UiContent::MarkdownBlock(_))
                && node.key.as_str() == "markdown/block/0"
        })
        .expect("first Markdown block painted");
    let expected = match &block.content {
        UiContent::MarkdownBlock(spec) => spec.source.clone(),
        _ => unreachable!(),
    };
    let (x, y) = native_content_to_window(
        &mut app,
        wid,
        block.rect.x + block.rect.width / 2.0,
        block.rect.y + block.rect.height / 2.0,
    );
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("Markdown workspace changed kind");
    };
    assert_eq!(
        state.markdown.as_ref().unwrap().reader.selection,
        Some(expected.clone())
    );
    assert_eq!(&source[expected], "# **Heading**\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn wheel_axis_matches_the_old_vertical_guard_and_adds_the_horizontal_one() {
    use super::{WheelAxis, wheel_axis};
    assert_eq!(wheel_axis(0.0, 1.0, 0.0), Some(WheelAxis::Vertical));
    assert_eq!(wheel_axis(0.0, -1.0, 0.0), Some(WheelAxis::Vertical));
    assert_eq!(wheel_axis(0.4, -1.0, 0.0), Some(WheelAxis::Vertical));
    assert_eq!(wheel_axis(1.0, 0.0, 0.0), Some(WheelAxis::Horizontal));
    assert_eq!(wheel_axis(-1.0, 0.4, 0.0), Some(WheelAxis::Horizontal));
    assert_eq!(wheel_axis(0.0, 0.0, 0.0), None);
    assert_eq!(wheel_axis(1.0, 1.0, 0.0), None);
    assert_eq!(wheel_axis(-2.0, 2.0, 0.0), None);
    assert_eq!(wheel_axis(0.0, f64::EPSILON / 2.0, f64::EPSILON), None);
    assert_eq!(wheel_axis(f64::EPSILON / 2.0, 0.0, f64::EPSILON), None);
}

#[test]
fn the_two_wheel_axes_bank_into_separate_window_fields() {
    use crate::{App, WindowId};
    use aterm_window::event::MouseScrollDelta;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let near = |got: f64, want: f64, what: &str| {
        assert!((got - want).abs() < 1e-6, "{what}: got {got}, want ~{want}");
    };
    let residuals = |app: &App| {
        let ws = &app.windows[&wid];
        (ws.scroll_residual_x, ws.scroll_residual)
    };
    assert_eq!(residuals(&app), (0.0, 0.0), "both banks start empty");

    assert_eq!(
        app.wheel_notches(wid, MouseScrollDelta::LineDelta(0.6, 0.0)),
        None
    );
    near(residuals(&app).0, 0.6, "x banked in the x field");
    assert_eq!(residuals(&app).1, 0.0, "the y field is untouched by x");
    assert_eq!(
        app.wheel_notches(wid, MouseScrollDelta::LineDelta(0.0, 0.6)),
        None
    );
    near(residuals(&app).0, 0.6, "the x bank survived a y event");
    near(residuals(&app).1, 0.6, "y banked in the y field");

    assert_eq!(
        app.wheel_notches(wid, MouseScrollDelta::LineDelta(0.0, 0.6)),
        Some((WheelDir::Up, 1)),
        "the vertical bank pays off from vertical motion alone"
    );
    near(
        residuals(&app).0,
        0.6,
        "the vertical payoff must not drain the horizontal bank",
    );
    assert_eq!(
        app.wheel_notches(wid, MouseScrollDelta::LineDelta(-0.6, 0.0)),
        None,
        "a horizontal reversal forfeits only the horizontal bank"
    );
    near(
        residuals(&app).0,
        -0.6,
        "the reversal restarted the x bank in its own direction",
    );
}

#[test]
fn horizontal_flicks_report_both_xterm_buttons_in_the_right_direction() {
    use aterm_window::dpi::PhysicalPosition;
    use aterm_window::event::MouseScrollDelta;

    use crate::{App, WindowId};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    assert_eq!(
        app.wheel_notches(wid, MouseScrollDelta::LineDelta(1.0, 0.0)),
        Some((WheelDir::Left, 1)),
        "+x (content moves right) = the reader walks LEFT"
    );
    assert_eq!(
        app.wheel_notches(wid, MouseScrollDelta::LineDelta(-1.0, 0.0)),
        Some((WheelDir::Right, 1)),
        "-x = the reader walks RIGHT (winit's Windows backend negates WM_MOUSEHWHEEL)"
    );
    assert_eq!(
        app.wheel_notches(wid, MouseScrollDelta::LineDelta(-3.0, 0.0)),
        Some((WheelDir::Right, 3))
    );

    let cw = app.win_cell_size(wid).0.max(1) as f64;
    assert_eq!(
        app.wheel_notches(
            wid,
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(cw, 0.0))
        ),
        Some((WheelDir::Left, 1)),
        "+x pixels agree with +x lines"
    );
    assert_eq!(
        app.wheel_notches(
            wid,
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(-cw, 0.0))
        ),
        Some((WheelDir::Right, 1))
    );
}

#[test]
fn horizontal_flicks_move_no_local_viewport_anywhere() {
    use aterm_window::event::MouseScrollDelta;

    use crate::native_app::AppViewState;
    use crate::{App, WindowId};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut terminal = crate::term_lock(&term);
        for line in 0..100 {
            terminal.process(format!("history {line}\r\n").as_bytes());
        }
        assert!(terminal.grid().scrollback_lines() > 0);
    }

    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(3.0, 0.0));
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(-3.0, 0.0));
    assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
    assert!(
        app.windows[&wid].scroll_glide.is_none(),
        "a horizontal flick never arms the vertical smooth scroll"
    );
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
    assert!(
        app.windows[&wid].scroll_glide.is_some()
            || crate::term_lock(&term).grid().display_offset() > 0,
        "negative control: the vertical axis still scrolls"
    );

    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (_, view) = app.active_native_view(wid).expect("Settings active");
    let page_scroll = |app: &App| match app.native_runtime.view_state(view).unwrap() {
        AppViewState::Settings(settings) => settings.page_scroll,
        _ => panic!("Settings state"),
    };
    let before = page_scroll(&app);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(5.0, 0.0));
    assert_eq!(
        page_scroll(&app),
        before,
        "a horizontal flick never scrolls a native view"
    );
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -5.0));
    assert_ne!(
        page_scroll(&app),
        before,
        "negative control: the vertical axis still reaches the native scroller"
    );

    let mut app = App::headless_for_test();
    app.palette_enter();
    let scroll_of = |app: &App| app.windows[&wid].palette().unwrap().scroll_extent().0;
    let before = scroll_of(&app);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(5.0, 0.0));
    assert_eq!(
        scroll_of(&app),
        before,
        "a horizontal flick never scrolls the palette card"
    );
}

#[test]
fn shift_is_the_tracking_bypass_off_macos() {
    use super::tracking_bypass_held;
    use aterm_window::keyboard::ModifiersState as M;
    assert!(tracking_bypass_held(M::ALT));
    assert!(tracking_bypass_held(M::ALT | M::SHIFT));
    assert_eq!(
        tracking_bypass_held(M::SHIFT),
        cfg!(not(target_os = "macos"))
    );
    assert!(!tracking_bypass_held(M::empty()));
    assert!(!tracking_bypass_held(M::CONTROL));
    assert!(!tracking_bypass_held(M::SUPER));
}

#[test]
fn option_drag_kind_depends_on_who_owns_the_mouse() {
    assert_eq!(
        press_selection_kind(true, false),
        SelectionType::Block,
        "local Option+drag stays the rectangular block selection"
    );
    assert_eq!(
        press_selection_kind(true, true),
        SelectionType::Simple,
        "bypass Option+drag selects normally (the modifier bought the bypass)"
    );
    assert_eq!(press_selection_kind(false, false), SelectionType::Simple);
    assert_eq!(press_selection_kind(false, true), SelectionType::Simple);
}

#[test]
fn right_press_plan_matches_the_windows_convention() {
    use super::{RightPressPlan, right_press_plan};
    for gesture_on in [false, true] {
        for has_selection in [false, true] {
            for tracking in [false, true] {
                assert_eq!(
                    right_press_plan(gesture_on, true, tracking, has_selection),
                    RightPressPlan::Chrome,
                    "chrome must swallow (gesture_on={gesture_on}, tracking={tracking})"
                );
            }
            assert_eq!(
                right_press_plan(gesture_on, false, true, has_selection),
                RightPressPlan::Seam,
                "tracking keeps the button (gesture_on={gesture_on})"
            );
        }
    }
    assert_eq!(
        right_press_plan(true, false, false, true),
        RightPressPlan::Copy,
        "selection exists: copy (and clear) it"
    );
    assert_eq!(
        right_press_plan(true, false, false, false),
        RightPressPlan::Paste,
        "no selection: paste"
    );
    assert_eq!(
        right_press_plan(false, false, false, true),
        RightPressPlan::Seam
    );
    assert_eq!(
        right_press_plan(false, false, false, false),
        RightPressPlan::Seam
    );
}

#[test]
fn pointer_geometry_tracks_the_band_offset() {
    use crate::{App, WindowId};
    use aterm_window::dpi::PhysicalSize;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.backend.set_pad(12);
    app.backend.set_pad_top(2);
    app.windows.get_mut(&wid).unwrap().metrics.pad = 12;
    app.windows.get_mut(&wid).unwrap().metrics.pad_top = 2;
    assert_eq!(
        app.frame_origin(wid),
        (0, 0),
        "headless must be the identity"
    );

    let (cw, ch) = app.cell_size();
    let pad = app.backend.pad();
    let pad_top = app.backend.pad_top();
    let (rows, cols) = {
        let ws = app.windows.get(&wid).expect("seeded window");
        (ws.rows as usize, ws.cols as usize)
    };
    let fit = PhysicalSize::new(
        (cols * cw + 2 * pad) as u32,
        (rows * ch + pad_top + pad) as u32,
    );
    app.windows.get_mut(&wid).expect("window").win_px = Some(fit);
    assert_eq!(
        app.frame_origin(wid),
        (0, 0),
        "exact fit must be the identity"
    );
    let (px, py) = ((pad + 5 * cw) as f64 + 1.0, (pad_top + 2 * ch) as f64 + 1.0);
    let at_fit = app.pixel_to_cell(wid, px, py);

    let (rw, rh) = (cw.saturating_sub(1).max(1), ch.saturating_sub(1).max(1));
    app.windows.get_mut(&wid).expect("window").win_px = Some(PhysicalSize::new(
        fit.width + rw as u32,
        fit.height + rh as u32,
    ));
    let (ox, oy) = (rw / 2, if cfg!(target_os = "linux") { 0 } else { rh / 2 });
    assert_eq!(
        app.frame_origin(wid),
        (ox as i64, oy as i64),
        "a sub-cell remainder must land exactly where the presenter's bands do"
    );
    assert_eq!(
        app.pixel_to_cell(wid, px + ox as f64, py + oy as f64),
        at_fit,
        "a click over the same on-glass cell must map to the same cell"
    );
    let (_, col) = app.pixel_to_cell(wid, 1.0, py + oy as f64);
    assert_eq!(col, 0, "a band click clamps to the leading cell");

    app.windows.get_mut(&wid).expect("window").win_px = Some(PhysicalSize::new(
        fit.width.saturating_sub(3).max(1),
        fit.height.saturating_sub(5).max(1),
    ));
    let cropped_origin = app.frame_origin(wid);
    assert!(cropped_origin.0 < 0);
    if cfg!(target_os = "linux") {
        assert_eq!(cropped_origin.1, 0, "a top-pinned crop keeps the frame top");
    } else {
        assert!(cropped_origin.1 < 0);
    }
    assert_eq!(
        app.window_to_frame(wid, 0.0, 0.0),
        (-cropped_origin.0 as f64, -cropped_origin.1 as f64),
        "pointer transform must retain the source-crop origin"
    );
}

#[test]
fn release_settles_local_selection_after_tracking_flips_off_mid_drag() {
    use crate::{App, WindowId, term_lock};
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};
    use aterm_window::keyboard::ModifiersState;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(b"\x1b[?1000h");
    assert!(term_lock(&term).mouse_tracking_enabled());
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::ALT;
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    assert!(
        app.windows.get(&wid).unwrap().selecting,
        "the Option-bypass press starts a local selection"
    );
    term_lock(&term).process(b"\x1b[?1000l");
    assert!(!term_lock(&term).mouse_tracking_enabled());
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    assert!(
        !app.windows.get(&wid).unwrap().selecting,
        "release settles the selection (selecting cleared) even though tracking flipped off"
    );
    assert_eq!(
        app.windows.get(&wid).unwrap().reported_buttons & 1,
        0,
        "the Left (selection) reported bit is cleared on settle"
    );
}

#[test]
fn a_thumb_press_never_clears_a_live_drags_reported_bit() {
    use aterm_types::mouse::MouseButton;
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    use crate::{App, WindowId, term_lock};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(b"\x1b[?1000h");
    assert!(term_lock(&term).mouse_tracking_enabled());
    let bits = |app: &App| app.windows[&wid].reported_buttons;
    let bit = |b: MouseButton| 1u8 << (b.slot() & 7);

    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    assert_ne!(
        bits(&app) & bit(MouseButton::Left),
        0,
        "Left press reported"
    );
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Back);
    assert_ne!(
        bits(&app) & bit(MouseButton::Back),
        0,
        "Back press reported"
    );
    assert_ne!(
        bits(&app) & bit(MouseButton::Left),
        0,
        "the thumb press must not disturb the live Left drag"
    );
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Back);
    assert_eq!(bits(&app) & bit(MouseButton::Back), 0, "Back bit cleared");
    assert_ne!(
        bits(&app) & bit(MouseButton::Left),
        0,
        "the thumb RELEASE must not clear the live drag's press bit — the \
         orphaned-release bug"
    );
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    assert_eq!(bits(&app), 0, "every bit paired off");

    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Forward);
    assert_eq!(bits(&app), bit(MouseButton::Forward));
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Other(9));
    assert_eq!(
        bits(&app),
        bit(MouseButton::Forward),
        "an unmapped device button touches no bit"
    );
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Forward);
    assert_eq!(bits(&app), 0);
}

#[test]
fn pet_wake_is_edge_gated_on_cell_displacement_and_the_brains_own_cadence() {
    use super::pet_wake_wanted;
    let cell = (10, 20);
    let seen = Some((100.0, 200.0));
    assert!(
        !pet_wake_wanted(false, false, None, (0.0, 0.0), cell),
        "no pet drawn: never"
    );
    assert!(
        !pet_wake_wanted(true, true, None, (0.0, 0.0), cell),
        "the brain's own lane is armed: never (it samples on its ticks)"
    );
    assert!(
        pet_wake_wanted(true, false, None, (0.0, 0.0), cell),
        "never sampled: the first event asks"
    );
    for (dx, dy) in [(0.0, 0.0), (9.9, 0.0), (0.0, 19.9), (-9.9, -19.9)] {
        assert!(
            !pet_wake_wanted(true, false, seen, (100.0 + dx, 200.0 + dy), cell),
            "sub-cell motion ({dx}, {dy}) asks for nothing"
        );
    }
    for (dx, dy) in [(10.0, 0.0), (0.0, 20.0), (-10.0, 0.0), (0.0, -20.0)] {
        assert!(
            pet_wake_wanted(true, false, seen, (100.0 + dx, 200.0 + dy), cell),
            "a cell of travel ({dx}, {dy}) on either axis asks once"
        );
    }
    assert!(pet_wake_wanted(true, false, seen, (101.0, 200.0), (0, 0)));
}

#[test]
fn a_click_on_the_pet_pets_and_never_starts_a_selection() {
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (x0, x1, y0, y1) = {
        let ws = app.windows.get_mut(&wid).unwrap();
        let pet = crate::app_render::pet_owner_tick_for_test(
            ws,
            aterm_effects::host::CaptureMode::StaticCapture,
            aterm_effects::kitty_pet::PetSense {
                caret_drawn: true,
                now: std::time::Instant::now(),
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
        assert!(pet.on_glass, "fixture: the resident is drawn");
        ws.fx
            .companion_mut()
            .hit_rect()
            .expect("the drawn body is the hit target")
    };
    let (fx, fy) = (f64::from(x0 + x1) / 2.0, f64::from(y0 + y1) / 2.0);
    let (wx, wy) = {
        let (ox, oy) = app.frame_origin(wid);
        (fx + ox as f64, fy + oy as f64)
    };
    app.on_cursor_moved(wid, wx, wy);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    {
        let ws = app.windows.get(&wid).unwrap();
        assert_eq!(
            ws.fx.companion().brain().pending_pets(),
            1,
            "the press latched a pet (note, never act)"
        );
        assert!(!ws.selecting, "and was consumed before the selection layer");
    }
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    assert!(
        !app.windows.get(&wid).unwrap().selecting,
        "the orphan release is dropped (press/release stay paired)"
    );
    app.on_cursor_moved(wid, 420.0, 300.0);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    {
        let ws = app.windows.get(&wid).unwrap();
        assert_eq!(
            ws.fx.companion().brain().pending_pets(),
            1,
            "no second pet off the body"
        );
        assert!(ws.selecting, "a plain terminal press still selects");
    }
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn a_right_press_on_a_chip_pops_the_menu_and_never_reaches_the_grid() {
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid) as f64;
    let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
    app.splice_tab_strip_with(wid, 1);
    let chip = app.windows[&wid]
        .tab_segments
        .iter()
        .find_map(|seg| match seg.kind {
            crate::tab_bar::TabHit::Select(0) => Some(seg.start_col + 1),
            _ => None,
        })
        .expect("the lone tab has a chip");
    let chip_pt = (
        pad + f64::from(chip) * cw as f64 + cw as f64 * 0.5,
        top + ch as f64 * 0.5,
    );
    assert!(
        app.strip_col_at(wid, chip_pt.0, chip_pt.1).is_some(),
        "fixture point must be ON the strip"
    );

    app.on_cursor_moved(wid, chip_pt.0, chip_pt.1);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Right);
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Right);
    let menu = app.windows[&wid]
        .tab_menu
        .as_ref()
        .expect("the right press popped the tab's menu");
    assert!(
        menu.entries
            .iter()
            .any(|e| matches!(e, crate::session_chrome::TabMenuEntry::Action { .. })),
        "…carrying the composed model's action rows"
    );
    assert_eq!(
        app.windows[&wid].reported_buttons, 0,
        "a press on chrome is never reported to the terminal"
    );

    let plus = app.windows[&wid]
        .tab_segments
        .iter()
        .find_map(|seg| (seg.kind == crate::tab_bar::TabHit::NewTab).then_some(seg.start_col))
        .expect("a one-tab strip still offers the + segment");
    let tabs_before = app.windows[&wid].tab_set.len();
    let plus_pt = (
        pad + f64::from(plus) * cw as f64 + cw as f64 * 0.5,
        top + ch as f64 * 0.5,
    );
    app.on_cursor_moved(wid, plus_pt.0, plus_pt.1);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "click-away dismissed the card"
    );
    assert_eq!(
        app.windows[&wid].tab_set.len(),
        tabs_before,
        "…and the dismissing press did NOT also open a tab"
    );
    assert_eq!(app.windows[&wid].reported_buttons, 0);
}

#[cfg(windows)]
#[test]
fn the_bare_band_system_menu_arms_on_press_and_fires_on_release() {
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid) as f64;
    let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
    app.splice_tab_strip_with(wid, 1);
    let bare = app.windows[&wid]
        .tab_segments
        .iter()
        .find(|seg| seg.kind == crate::tab_bar::TabHit::NewTab)
        .map(|seg| seg.end_col)
        .expect("the strip laid out its +")
        + 2;
    let bare_pt = (
        pad + f64::from(bare) * cw as f64 + cw as f64 * 0.5,
        top + ch as f64 * 0.5,
    );
    assert!(
        app.strip_col_at(wid, bare_pt.0, bare_pt.1).is_some(),
        "fixture point must be ON the strip"
    );
    assert!(
        crate::tab_bar::hit_test(&app.windows[&wid].tab_segments, bare).is_none(),
        "fixture point must be the BARE band, not a chip or control"
    );

    app.on_cursor_moved(wid, bare_pt.0, bare_pt.1);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Right);
    assert!(
        app.windows[&wid].band_menu_release_pending,
        "the PRESS armed the latch instead of popping — a popup posted \
         here is discarded under winit's mouse capture"
    );
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "the bare band pops the SYSTEM menu, never the tab card"
    );
    assert_eq!(
        app.windows[&wid].reported_buttons, 0,
        "a press on chrome is never reported to the terminal"
    );

    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Right);
    assert!(
        !app.windows[&wid].band_menu_release_pending,
        "the RELEASE spent the latch — the popup post lives behind this \
         spend, after winit's ReleaseCapture"
    );
    assert_eq!(
        app.windows[&wid].reported_buttons, 0,
        "…and the consumed release leaked nothing to the terminal"
    );

    let chip = app.windows[&wid]
        .tab_segments
        .iter()
        .find_map(|seg| match seg.kind {
            crate::tab_bar::TabHit::Select(0) => Some(seg.start_col + 1),
            _ => None,
        })
        .expect("the lone tab has a chip");
    let chip_pt = (
        pad + f64::from(chip) * cw as f64 + cw as f64 * 0.5,
        top + ch as f64 * 0.5,
    );
    app.on_cursor_moved(wid, chip_pt.0, chip_pt.1);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Right);
    assert!(
        !app.windows[&wid].band_menu_release_pending,
        "a chip press must not arm the system menu"
    );
    assert!(
        app.windows[&wid].tab_menu.is_some(),
        "…because it pops the tab card"
    );
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Right);
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn hovering_the_new_tab_button_records_it_and_repaints_the_strip() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid) as f64;
    let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
    app.splice_tab_strip_with(wid, 1);
    let seg = |want_plus: bool| {
        app.windows[&wid]
            .tab_segments
            .iter()
            .find(|s| matches!(s.kind, crate::tab_bar::TabHit::NewTab) == want_plus)
            .copied()
            .expect("the strip laid out both a chip and a +")
    };
    let plus = seg(true);
    let point = |col: u16| {
        (
            pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        )
    };
    let button_col = plus.start_col + 1;
    let bg_of =
        |app: &crate::App, col: u16| app.windows[&wid].cached_strip_rows[0][col as usize].bg;
    let resting = bg_of(&app, button_col);

    let p = point(button_col);
    app.on_cursor_moved(wid, p.0, p.1);
    assert!(
        app.windows[&wid].strip_hover_new_tab,
        "the pointer is on the `+`"
    );
    assert_eq!(
        app.windows[&wid].strip_hover, None,
        "…and the `+` is not a tab, so no chip is hovered"
    );

    app.splice_tab_strip_with(wid, 1);
    let hot = bg_of(&app, button_col);
    assert_ne!(
        resting, hot,
        "the repaint must reach the rows — a cache key without the flag \
         would hit and paint the resting button forever"
    );
    assert_eq!(
        hot,
        crate::tab_bar::strip_hover_bg_for_test(app.chrome_palette_theme()),
        "the button takes the band's own hover material"
    );

    app.on_cursor_left(wid);
    assert!(
        !app.windows[&wid].strip_hover_new_tab,
        "leaving the window clears the `+` wash"
    );
    app.splice_tab_strip_with(wid, 1);
    assert_eq!(
        bg_of(&app, button_col),
        resting,
        "…and the clear reaches the painted row"
    );
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn closing_an_overlay_replays_no_pointer_the_window_has_lost() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid) as f64;
    let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
    app.splice_tab_strip_with(wid, 1);
    let chip = app.windows[&wid]
        .tab_segments
        .iter()
        .find_map(|seg| match seg.kind {
            crate::tab_bar::TabHit::Select(0) => Some(seg.start_col + 2),
            _ => None,
        })
        .expect("the lone tab has a chip");
    let on_tab = (
        pad + f64::from(chip) * cw as f64 + cw as f64 * 0.5,
        top + ch as f64 * 0.5,
    );
    app.on_cursor_moved(wid, on_tab.0, on_tab.1);
    assert_eq!(
        app.windows[&wid].strip_hover,
        Some(0),
        "CONTROL: the tab lit"
    );
    app.on_cursor_left(wid);
    assert_eq!(app.windows[&wid].strip_hover, None, "leaving cleared it");

    type Close = fn(&mut crate::App, crate::WindowId);
    let closes: [(&str, Close); 2] = [
        ("the palette", |app, _| app.palette_exit()),
        ("an overlay card", |app, wid| {
            app.close_overlay_card(wid, |ws| ws.palette().is_some());
        }),
    ];
    for (what, close) in closes {
        app.palette_enter();
        assert!(app.windows[&wid].palette().is_some(), "{what}: opened");
        close(&mut app, wid);
        assert!(app.windows[&wid].palette().is_none(), "{what}: closed");
        let ws = &app.windows[&wid];
        assert!(
            !ws.pointer_position_known,
            "{what}: the close claimed a pointer the window does not hold"
        );
        assert_eq!(ws.strip_hover, None, "{what}: a phantom hover lit tab 0");
    }

    app.on_cursor_moved(wid, on_tab.0, on_tab.1);
    app.palette_enter();
    app.palette_exit();
    assert_eq!(
        app.windows[&wid].strip_hover,
        Some(0),
        "a held pointer is still replayed on close"
    );
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn a_strip_hover_change_moves_the_repaint_key() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid) as f64;
    let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
    app.splice_tab_strip_with(wid, 1);
    let seg = |kind: crate::tab_bar::TabHit| {
        app.windows[&wid]
            .tab_segments
            .iter()
            .find(|s| s.kind == kind)
            .copied()
            .expect("the strip laid out a chip and a +")
    };
    let chip = seg(crate::tab_bar::TabHit::Select(0));
    let plus = seg(crate::tab_bar::TabHit::NewTab);
    let point = |col: u16| {
        (
            pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        )
    };
    let rest = app.redraw_tab_strip_state(wid);
    assert_ne!(rest, 0, "a live strip has a fingerprint");

    let p = point(chip.start_col + 1);
    app.on_cursor_moved(wid, p.0, p.1);
    assert_eq!(app.windows[&wid].strip_hover, Some(0), "on the chip");
    let on_chip = app.redraw_tab_strip_state(wid);
    assert_ne!(on_chip, rest, "lighting a chip moves the repaint key");

    let p = point(chip.start_col + 2);
    app.on_cursor_moved(wid, p.0, p.1);
    assert_eq!(
        app.redraw_tab_strip_state(wid),
        on_chip,
        "motion within the chip repaints nothing"
    );

    let p = point(plus.start_col + 1);
    app.on_cursor_moved(wid, p.0, p.1);
    assert!(app.windows[&wid].strip_hover_new_tab, "on the `+`");
    let on_plus = app.redraw_tab_strip_state(wid);
    assert_ne!(
        on_plus, on_chip,
        "the light moving to the `+` moves the key"
    );
    assert_ne!(on_plus, rest);

    app.on_cursor_left(wid);
    assert_eq!(
        app.redraw_tab_strip_state(wid),
        rest,
        "nothing lit: the resting strip's fingerprint, byte for byte"
    );
}

#[cfg(windows)]
#[test]
fn hovering_the_version_label_records_it_and_repaints_the_strip() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    assert!(
        crate::tray_raster::strip_band_ui_ready(),
        "Windows always has a UI face to measure the label in"
    );
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid) as f64;
    let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
    app.splice_tab_strip_with(wid, 1);
    let label = app.windows[&wid]
        .tab_segments
        .iter()
        .find(|s| matches!(s.kind, crate::tab_bar::TabHit::Version { .. }))
        .copied()
        .expect("the strip lays the version label out once the UI face is in");
    let plus = app.windows[&wid]
        .tab_segments
        .iter()
        .find(|s| s.kind == crate::tab_bar::TabHit::NewTab)
        .copied()
        .expect("the strip offers the +");
    let point = |col: u16| {
        (
            pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        )
    };
    let bg_of =
        |app: &crate::App, col: u16| app.windows[&wid].cached_strip_rows[0][col as usize].bg;
    let resting = bg_of(&app, label.start_col);
    let resting_key = app.redraw_tab_strip_state(wid);

    let p = point(label.start_col + 3);
    app.on_cursor_moved(wid, p.0, p.1);
    assert!(
        app.windows[&wid].strip_hover_version,
        "the pointer is on the label"
    );
    assert!(!app.windows[&wid].strip_hover_new_tab, "…not on the `+`");
    assert_eq!(app.windows[&wid].strip_hover, None, "…and on no chip");
    assert_ne!(
        app.redraw_tab_strip_state(wid),
        resting_key,
        "the label's hover reaches the glass on its own"
    );

    app.splice_tab_strip_with(wid, 1);
    let hot = bg_of(&app, label.start_col);
    assert_ne!(resting, hot, "the repaint must reach the rows");
    assert_eq!(
        hot,
        crate::tab_bar::strip_hover_bg_for_test(app.chrome_palette_theme()),
        "the label takes the band's own hover material"
    );

    app.on_cursor_left(wid);
    assert!(
        !app.windows[&wid].strip_hover_version,
        "leaving clears the label"
    );
    app.splice_tab_strip_with(wid, 1);
    assert_eq!(bg_of(&app, label.start_col), resting);
    assert_eq!(
        app.redraw_tab_strip_state(wid),
        resting_key,
        "…and the repaint key is the resting one again"
    );

    app.on_cursor_moved(wid, p.0, p.1);
    assert!(app.windows[&wid].strip_hover_version, "re-entry relights");
    let q = point(plus.start_col + 1);
    app.on_cursor_moved(wid, q.0, q.1);
    assert!(
        !app.windows[&wid].strip_hover_version,
        "leaving the label clears it"
    );
    assert!(app.windows[&wid].strip_hover_new_tab, "…and lights the `+`");
    app.splice_tab_strip_with(wid, 1);
    assert_eq!(
        bg_of(&app, label.start_col),
        resting,
        "the label is back to resting"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn hover_cursor_resolves_grid_tracking_divider_and_strip_by_location() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid) as f64;
    let strip_px = app.tab_strip_rows as f64 * ch as f64;
    let grid = (pad + cw as f64 * 10.5, pad + strip_px + ch as f64 * 5.5);
    assert!(
        app.strip_col_at(wid, grid.0, grid.1).is_none(),
        "fixture point must be off the strip"
    );
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_cursor_px = grid;
        ws.last_mouse_window_cell = (5, 10);
    }

    app.update_hover_cursor(wid);
    let state = |app: &crate::App| {
        let ws = &app.windows[&wid];
        (ws.hover_pointer, ws.native_text_cursor)
    };
    assert_eq!(state(&app), (false, true), "grid = I-beam");

    let term = app.front_terminal(wid).expect("terminal").term.clone();
    crate::term_lock(&term).process(b"\x1b[?1000h");
    app.update_hover_cursor(wid);
    assert_eq!(state(&app), (false, false), "tracking = arrow");
    crate::term_lock(&term).process(b"\x1b[?1000l");
    app.update_hover_cursor(wid);
    assert_eq!(state(&app), (false, true), "tracking off = I-beam again");

    let _sid = app.split_active_stub_tab(wid);
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_mouse_window_cell = (5, 40);
    }
    app.update_hover_cursor(wid);
    assert_eq!(state(&app), (true, true), "divider = resize encoding");
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_mouse_window_cell = (5, 10);
    }
    app.update_hover_cursor(wid);
    assert_eq!(state(&app), (false, true), "off the seam = I-beam");

    {
        app.tab_strip_rows = 1;
        let strip_px = ch as f64;
        let strip_pt = (pad + cw as f64 * 2.5, pad + strip_px * 0.5);
        assert!(
            app.strip_col_at(wid, strip_pt.0, strip_pt.1).is_some(),
            "fixture point must be ON the strip"
        );
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_cursor_px = strip_pt;
            ws.pointer_position_known = true;
        }
        app.on_modifiers_changed(wid, link_modifier());
        assert_eq!(
            state(&app),
            (false, false),
            "a link-modifier tap over the strip paints chrome's arrow, not an I-beam"
        );
        app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());
    }
    app.tab_strip_rows = 0;

    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_cursor_px = grid;
        ws.last_mouse_window_cell = (5, 10);
    }
    app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::SHIFT);
    assert_eq!(
        state(&app),
        (false, false),
        "a Shift edge must not re-resolve the hover (no lock, no probe)"
    );
    app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());
    app.on_modifiers_changed(wid, link_modifier());
    assert_eq!(
        state(&app),
        (false, true),
        "a link-modifier edge resolves the grid"
    );
    app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());

    let term = app.front_terminal(wid).expect("terminal").term.clone();
    let acq = crate::term_lock_acquisitions_on_this_thread;
    {
        let mut held = crate::term_lock(&term);
        held.process(b"\x1b[?1000h");
        let before = acq();
        app.update_hover_cursor(wid);
        assert_eq!(
            acq() - before,
            0,
            "the mode is read from the mirror, lock-free"
        );
        assert_eq!(
            state(&app),
            (false, false),
            "tracking resolves to the arrow while the mutex is busy"
        );
        held.process(b"\x1b[?1000l");
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_cursor_px = (grid.0 + cw as f64 * 2.0, grid.1);
            ws.last_mouse_window_cell = (5, 12);
        }
        let before = acq();
        app.update_hover_cursor(wid);
        assert_eq!(acq() - before, 0, "a busy mutex is never waited on");
        assert_eq!(
            state(&app),
            (false, false),
            "…the probe is deferred: the previous cursor stands, nothing blocked"
        );
    }
    app.update_hover_cursor(wid);
    assert_eq!(
        state(&app),
        (false, true),
        "lock free again: the I-beam resolves"
    );
}

fn link_modifier() -> aterm_window::keyboard::ModifiersState {
    #[cfg(target_os = "macos")]
    {
        aterm_window::keyboard::ModifiersState::SUPER
    }
    #[cfg(not(target_os = "macos"))]
    {
        aterm_window::keyboard::ModifiersState::CONTROL
    }
}

fn tab_order(app: &crate::App, wid: crate::WindowId) -> Vec<crate::tab_model::TabId> {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .map(|tab| tab.id)
        .collect()
}

fn chip_span(app: &crate::App, wid: crate::WindowId, index: usize) -> (u16, u16) {
    let seg = app.windows[&wid]
        .tab_segments
        .iter()
        .find(|seg| seg.kind == crate::tab_bar::TabHit::Select(index))
        .expect("the tab has a laid-out chip");
    (seg.start_col, seg.end_col)
}

fn strip_point(app: &crate::App, wid: crate::WindowId, col: u16) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid) as f64;
    let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
    let point = (
        pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
        top + ch as f64 * 0.5,
    );
    assert_eq!(
        app.strip_col_at(wid, point.0, point.1),
        Some(col),
        "fixture point must land on strip column {col}"
    );
    point
}

fn app_with_three_chips() -> (crate::App, crate::WindowId, Vec<crate::tab_model::TabId>) {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.splice_tab_strip(wid);
    let order = tab_order(&app, wid);
    assert_eq!(order.len(), 3, "three chips to shuffle");
    (app, wid, order)
}

#[test]
fn a_sweep_across_two_chips_walks_the_tab_two_places() {
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    let (mut app, wid, before) = app_with_three_chips();
    let grab = strip_point(&app, wid, chip_span(&app, wid, 2).0 + 1);
    app.on_cursor_moved(wid, grab.0, grab.1);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);

    let over = strip_point(&app, wid, chip_span(&app, wid, 0).0);
    app.on_cursor_moved(wid, over.0, over.1);
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);

    assert_eq!(
        tab_order(&app, wid),
        vec![before[2], before[0], before[1]],
        "the dragged chip walked all the way to the front"
    );
    assert_eq!(app.windows[&wid].tabs.active, 0);
}

#[test]
fn a_press_that_never_crosses_a_midpoint_never_reorders() {
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    let (mut app, wid, before) = app_with_three_chips();
    let (start, end) = chip_span(&app, wid, 0);
    let grab = strip_point(&app, wid, start + 1);
    app.on_cursor_moved(wid, grab.0, grab.1);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    assert!(
        app.windows[&wid].strip_drag.is_some(),
        "the chip IS held — this test is about what a held chip does NOT do"
    );

    app.on_cursor_moved(wid, grab.0 + 0.25, grab.1 + 0.25);
    assert_eq!(tab_order(&app, wid), before, "jitter is not a drag");

    let inside = strip_point(&app, wid, end - 1);
    app.on_cursor_moved(wid, inside.0, inside.1);
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    assert_eq!(
        tab_order(&app, wid),
        before,
        "travel within the chip's own share crosses nothing"
    );
}

#[test]
fn an_interruption_mid_drag_stops_the_held_chip() {
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    for interruption in ["focus loss", "context menu", "palette"] {
        let (mut app, wid, before) = app_with_three_chips();
        let (grab_start, _) = chip_span(&app, wid, 0);
        let grab = strip_point(&app, wid, grab_start + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows[&wid].strip_drag.is_some(),
            "{interruption}: the chip is held"
        );
        match interruption {
            "focus loss" => {
                app.on_focus(wid, false);
            }
            "context menu" => {
                assert!(
                    app.open_tab_context_menu(wid, 0, grab_start + 1, false),
                    "the chip has a composed menu"
                );
            }
            _ => {
                app.toggle_palette();
                assert!(
                    app.windows[&wid].palette().is_some(),
                    "fixture guard: the palette really opened"
                );
            }
        }
        let frozen = interruption == "palette";
        assert_eq!(
            app.windows[&wid].strip_drag.is_some(),
            frozen,
            "{interruption}: the grab is dropped (or, under the palette, frozen)"
        );
        let (start, end) = chip_span(&app, wid, 1);
        let over = strip_point(&app, wid, start + (end - start) / 2);
        app.on_cursor_moved(wid, over.0, over.1);
        assert_eq!(
            tab_order(&app, wid),
            before,
            "{interruption}: motion reorders nothing"
        );
        if frozen {
            app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
            assert!(
                app.windows[&wid].strip_drag.is_none(),
                "the release disarmed through the hoisted disarm"
            );
        }
    }
}

#[test]
fn a_press_that_is_no_grab_arms_no_reorder() {
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    for renaming in [false, true] {
        let (mut app, wid, before) = app_with_three_chips();
        let col = if renaming {
            assert!(
                app.begin_session_rename(wid, before[0]),
                "fixture guard: the editor opened on chip 0"
            );
            chip_span(&app, wid, 0).0 + 1
        } else {
            let band = app.windows[&wid]
                .tab_segments
                .iter()
                .find(|seg| seg.kind == crate::tab_bar::TabHit::NewTab)
                .map(|seg| seg.end_col)
                .expect("a laid-out strip");
            assert!(
                crate::tab_bar::hit_test(&app.windows[&wid].tab_segments, band).is_none(),
                "fixture column must be bare band"
            );
            band
        };
        let point = strip_point(&app, wid, col);
        app.on_cursor_moved(wid, point.0, point.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows[&wid].strip_drag.is_none(),
            "renaming={renaming}: the press never arms a drag"
        );
        if renaming {
            assert!(
                app.inline_rename_edit(wid).is_some(),
                "…and the editor is still open"
            );
        }
        let (start, end) = chip_span(&app, wid, 1);
        let over = strip_point(&app, wid, start + (end - start) / 2);
        app.on_cursor_moved(wid, over.0, over.1);
        assert_eq!(
            tab_order(&app, wid),
            before,
            "renaming={renaming}: a sweep past a midpoint moves nothing"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    }
}

#[test]
fn a_lone_chip_has_nowhere_to_go() {
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    app.splice_tab_strip(wid);
    let before = tab_order(&app, wid);
    assert_eq!(before.len(), 1);

    let (start, end) = chip_span(&app, wid, 0);
    let grab = strip_point(&app, wid, start);
    app.on_cursor_moved(wid, grab.0, grab.1);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    for col in start..end {
        let point = strip_point(&app, wid, col);
        app.on_cursor_moved(wid, point.0, point.1);
    }
    assert_eq!(
        app.windows[&wid].strip_drag.map(|drag| drag.origin_col),
        Some(start),
        "a lone chip has no neighbour to cross, so it never took a step"
    );
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    assert_eq!(tab_order(&app, wid), before);
    assert_eq!(app.windows[&wid].tab_set.len(), 1);
}

#[test]
fn under_pressure_the_wide_active_chip_marches_across_the_strip() {
    use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    for _ in 0..7 {
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    }
    app.splice_tab_strip(wid);
    let before = tab_order(&app, wid);
    assert_eq!(before.len(), 8, "eight chips to force the pressure layout");

    let seated = |app: &crate::App| -> Vec<usize> {
        app.windows[&wid]
            .tab_segments
            .iter()
            .filter_map(|seg| match seg.kind {
                crate::tab_bar::TabHit::Select(index) => Some(index),
                _ => None,
            })
            .collect()
    };
    let held = seated(&app)[0];
    assert!(
        seated(&app).len() < before.len(),
        "eight tabs on an 80-column strip must be more than it can seat"
    );

    let grab = strip_point(&app, wid, chip_span(&app, wid, held).0 + 1);
    app.on_cursor_moved(wid, grab.0, grab.1);
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    assert_eq!(
        app.windows[&wid].tabs.active, held,
        "the press selected the chip under it"
    );
    app.splice_tab_strip(wid); // the repaint the selection requested

    let width = |index: usize| {
        let (start, end) = chip_span(&app, wid, index);
        end - start
    };
    let widths: Vec<u16> = seated(&app).iter().map(|&i| width(i)).collect();
    assert!(
        widths.windows(2).all(|pair| pair[0] == pair[1]),
        "a seated page divides its band equally: {widths:?}"
    );
    assert!(
        widths[0] > 80 / before.len() as u16,
        "eight tabs on this strip must engage the pressure layout: a seated \
         chip ({} cols) has to beat the equal eighth it refused to shrink to",
        widths[0]
    );

    let mut motions = 0;
    let mut last_col = None::<u16>;
    while app.windows[&wid].tabs.active + 1 < before.len() {
        motions += 1;
        assert!(
            motions <= before.len(),
            "the march must terminate: {:?}",
            tab_order(&app, wid)
        );
        let was = app.windows[&wid].tabs.active;
        let col = if seated(&app).contains(&(was + 1)) {
            let (start, end) = chip_span(&app, wid, was + 1);
            start + (end - start) / 2
        } else {
            chip_span(&app, wid, was).1
        }
        .max(last_col.map_or(0, |c| c + 1));
        last_col = Some(col);
        let over = strip_point(&app, wid, col);
        app.on_cursor_moved(wid, over.0, over.1);
        let now = app.windows[&wid].tabs.active;
        assert!(
            now > was,
            "motion {motions}: the held chip advanced (was {was}, now {now})"
        );
        assert_eq!(
            tab_order(&app, wid)[now],
            before[held],
            "motion {motions}: the dragged tab is the one that moved, and it \
             is still the selected one"
        );
        app.splice_tab_strip(wid);
        app.on_cursor_moved(wid, over.0, over.1);
        assert_eq!(
            app.windows[&wid].tabs.active, now,
            "motion {motions}: a settled pointer must not oscillate after \
             the repaint"
        );
        assert_eq!(tab_order(&app, wid)[now], before[held]);
    }
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);

    let mut expect: Vec<_> = before.clone();
    let moved = expect.remove(held);
    expect.push(moved);
    assert_eq!(
        tab_order(&app, wid),
        expect,
        "the chip marched from the front of the strip to the back"
    );
    assert_eq!(app.windows[&wid].tabs.active, before.len() - 1);
    assert!(app.windows[&wid].strip_drag.is_none());
}

#[test]
fn a_line_or_word_drag_re_anchors_on_its_origin_after_output_scrolls_the_grid() {
    use crate::{App, WindowId, term_lock};

    for word in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let tag = if word { "w" } else { "L" };
        term_lock(&term).process(&numbered_screen(tag));

        if word {
            app.select_word_click(wid, 5, 0);
        } else {
            app.select_line_click(wid, 5, 0);
        }
        let origin = format!("{tag}05");
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some(origin.as_str()),
            "precondition: the click selected {origin}"
        );

        term_lock(&term).process(b"\r\nX\r\nY\r\nZ");
        assert_eq!(
            app.windows[&wid].gesture.map(|g| g.row),
            Some(5),
            "precondition: the captured gesture row is NOT compensated — that is the trap"
        );
        assert!(
            term_lock(&term).text_selection().has_selection(),
            "precondition: ordinary output must not clear the selection (Phase 4)"
        );

        app.drag_selection(wid, 8, 0);

        let text = term_lock(&term)
            .selection_to_string()
            .expect("the drag resolves to text");
        assert_eq!(
            text.lines().next(),
            Some(origin.as_str()),
            "word={word}: the drag must stay anchored on its ORIGIN; anchoring on the \
             stale gesture row would start the copy at row 8 — text the user never \
             selected"
        );
        if !word {
            assert_eq!(
                text.lines().last(),
                Some("L11"),
                "…and reach the hovered line"
            );
        }
    }
}

#[test]
fn a_drag_with_no_live_selection_falls_back_to_the_captured_gesture_row() {
    use crate::{App, WindowId, term_lock};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(&numbered_screen("L"));

    app.select_line_click(wid, 5, 0);
    term_lock(&term).text_selection_mut().clear();

    app.drag_selection(wid, 8, 0);
    let text = term_lock(&term)
        .selection_to_string()
        .expect("the fallback still builds a selection");
    assert_eq!(
        text.lines().next(),
        Some("L05"),
        "with nothing live to read, the captured gesture row is the origin"
    );
}

fn wrapped_command_screen() -> (Vec<u8>, String) {
    const HEAD: &str =
        "docker run --rm -v $PWD:/w -w /w --network host --name bld-boxy aterm-build rust";
    const TAIL: &str = ":1.85 cargo build --release --features gpu,wayland";
    assert_eq!(HEAD.len(), 80, "the head must fill the 80-column row");
    let cmd = format!("{HEAD}{TAIL}");
    let mut seed = cmd.clone().into_bytes();
    seed.extend_from_slice(b"\r\necho AFTER");
    (seed, cmd)
}

#[test]
fn a_triple_click_copies_the_whole_soft_wrapped_command() {
    use crate::{App, WindowId, term_lock};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let (seed, cmd) = wrapped_command_screen();
    term_lock(&term).process(&seed);
    assert_eq!(
        term_lock(&term).get_line_text(0, None).map(|t| t.len()),
        Some(80),
        "precondition: row 0 is full, so the command wraps onto row 1"
    );

    app.select_line_click(wid, 0, 10);

    let copied = term_lock(&term)
        .selection_to_string()
        .expect("the triple-click selected something");
    assert_eq!(
        copied.len(),
        cmd.len(),
        "the clipboard must carry all {} bytes of the command, not the 80 \
         that fit on the clicked row",
        cmd.len()
    );
    assert_eq!(copied, cmd);
    assert!(
        !copied.contains('\n'),
        "a soft wrap must not become a newline in the copy: {copied:?}"
    );
    assert!(
        !copied.contains("echo AFTER"),
        "the HARD newline after the command must still end the selection"
    );
    assert_eq!(
        app.windows[&wid].gesture.map(|g| (g.row, g.end_row)),
        Some((0, 1)),
        "the armed gesture origin must be the whole logical line"
    );
}

#[test]
fn a_double_click_copies_a_word_straddling_the_wrap_whole() {
    use crate::{App, WindowId, term_lock};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let filler = "word ".repeat(14) + "abc ";
    assert_eq!(filler.len(), 74);
    term_lock(&term).process(format!("{filler}STRADDLEWORD tail").as_bytes());

    app.select_word_click(wid, 0, 76);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("STRADDLEWORD"),
        "the double-click must reach across the soft wrap for the rest of the word"
    );
    assert_eq!(
        app.windows[&wid]
            .gesture
            .map(|g| ((g.row, g.start_col), (g.end_row, g.end_col))),
        Some(((0, 74), (1, 5))),
        "the armed origin word must be the whole word, on both rows"
    );
}

#[test]
fn a_drag_from_a_wrapped_line_keeps_the_whole_origin_line() {
    use crate::{App, WindowId, term_lock};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let (mut seed, cmd) = wrapped_command_screen();
    let mut screen = b"BEFORE\r\n".to_vec();
    screen.append(&mut seed);
    term_lock(&term).process(&screen);
    assert!(
        term_lock(&term).row_continues_previous(2),
        "precondition: the command wraps from row 1 onto row 2"
    );

    app.select_line_click(wid, 1, 10);
    app.drag_selection(wid, 3, 0);
    assert_eq!(
        term_lock(&term)
            .selection_to_string()
            .expect("the drag resolves to text"),
        format!("{cmd}\necho AFTER"),
        "the whole wrapped origin line, then the hovered line — one newline \
         between them, at the HARD break only"
    );

    app.drag_selection(wid, 0, 0);
    assert_eq!(
        term_lock(&term)
            .selection_to_string()
            .expect("the drag resolves to text"),
        format!("BEFORE\n{cmd}"),
        "dragging up past the origin must keep the whole wrapped origin line; \
         anchoring on the clicked ROW would stop the copy at the window width"
    );

    app.drag_selection(wid, 2, 0);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some(cmd.as_str()),
        "dragging within the origin logical line must keep it whole"
    );
}

fn numbered_screen(tag: &str) -> Vec<u8> {
    let mut seed = Vec::new();
    for r in 0..24u16 {
        if r > 0 {
            seed.extend_from_slice(b"\r\n");
        }
        seed.extend_from_slice(format!("{tag}{r:02}").as_bytes());
    }
    seed
}

#[test]
fn blur_or_a_tab_switch_settles_the_in_flight_selection() {
    use aterm_core::selection::SelectionState;

    use crate::{App, WindowId, stub_session, term_lock};

    for tab_switch in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(b"hello world");

        app.windows.get_mut(&wid).expect("window").last_mouse_cell = (0, 0);
        app.begin_selection(wid, SelectionType::Simple);
        app.drag_selection(wid, 0, 5);
        assert_eq!(
            term_lock(&term).text_selection().state(),
            SelectionState::InProgress,
            "precondition: the sweep leaves the engine mid-gesture"
        );

        if tab_switch {
            let sid = app.next_session_id;
            app.push_stub_tab(wid, stub_session(sid));
        } else {
            app.on_focus(wid, false);
        }

        assert_eq!(
            term_lock(&term).text_selection().state(),
            SelectionState::Complete,
            "tab_switch={tab_switch}: COMPLETE the selection, not strand it InProgress"
        );
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("hello"),
            "tab_switch={tab_switch}: the settled selection still names the swept text"
        );
        if !tab_switch {
            assert!(
                app.extend_selection_to(wid, 0, 10, SelectionSide::Right),
                "the settled selection is extendable — the InProgress zombie was not"
            );
        }
    }
}

#[test]
fn a_selection_on_a_double_width_line_takes_the_characters_under_the_pointer() {
    use crate::{App, WindowId, term_lock};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(b"\x1b#6HELLO WORLD\r\nplain text");

    app.windows.get_mut(&wid).expect("window").last_mouse_cell = (0, 0);
    app.begin_selection(wid, SelectionType::Simple);
    app.drag_selection(wid, 0, 9);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("HELLO"),
        "a drag over the cells HELLO is drawn in"
    );
    app.finish_selection(wid, true);

    app.seam_left_press(wid, 0, 12, 2, false);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("WORLD"),
        "a double-click on the W drawn at screen cell 12"
    );
    app.finish_selection(wid, true);

    app.windows.get_mut(&wid).expect("window").last_mouse_cell = (0, 0);
    app.begin_selection(wid, SelectionType::Simple);
    app.drag_selection(wid, 0, 3);
    app.finish_selection(wid, true);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("HE"),
        "precondition: a selection to extend"
    );
    let ws = app.windows.get_mut(&wid).expect("window");
    ws.last_mouse_cell = (0, 9);
    ws.last_mouse_side = SelectionSide::Left;
    app.shift_extend_press(wid, std::time::Instant::now());
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("HELLO"),
        "a shift-click on the right half of the O"
    );

    app.windows.get_mut(&wid).expect("window").last_mouse_cell = (1, 0);
    app.begin_selection(wid, SelectionType::Simple);
    app.drag_selection(wid, 1, 5);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("plain")
    );
}

/// THE GRID COUNTS A RUN OF PRESSES BY WHEN EACH HAPPENED: a press at the last instant of a
/// double click's interval after the first continues the run and selects the word under
/// it, and a press a millisecond after the interval starts a new run where a triple click's
/// third would select the line; a shift-click starts a run the same way. Each press states
/// its time, so how long the work between presses takes never moves one across the interval.
#[test]
fn the_grids_click_streak_counts_each_press_by_when_it_happened() {
    use crate::{App, WindowId, term_lock};
    use aterm_window::event::{ElementState, MouseButton};
    use aterm_window::keyboard::ModifiersState;
    use std::time::{Duration, Instant};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(b"hello world");
    let (x, y) = app
        .cell_centre_px(wid, 0, 1)
        .expect("cell (0, 1) is on the grid");
    app.on_cursor_moved(wid, x, y);
    let interval = Duration::from_millis(
        u64::try_from(crate::multi_click_ms()).expect("a double click's interval"),
    );
    // A minute ago, so a press or a stamp that read the clock instead of its stated time
    // lands on the wrong side of the interval.
    let first = Instant::now()
        .checked_sub(Duration::from_secs(60))
        .expect("a minute of uptime");
    let past = interval + Duration::from_millis(1);
    let shifted = first + interval * 4;
    let mut counts = Vec::new();
    for (k, (at, mods)) in [
        (first, ModifiersState::empty()),
        (first + interval, ModifiersState::empty()),
        (first + interval + past, ModifiersState::empty()),
        (shifted, ModifiersState::SHIFT),
        (shifted + interval, ModifiersState::empty()),
        (shifted + interval * 3, ModifiersState::SHIFT),
        (shifted + interval * 3 + past, ModifiersState::empty()),
    ]
    .into_iter()
    .enumerate()
    {
        app.windows.get_mut(&wid).expect("window").mods = mods;
        app.on_mouse_input_at(wid, ElementState::Pressed, MouseButton::Left, at);
        counts.push(app.windows[&wid].click_count);
        if k == 1 {
            assert_eq!(
                term_lock(&term).selection_to_string().as_deref(),
                Some("hello"),
                "the second press, at the interval's last instant, selects the word"
            );
        }
        app.on_mouse_input_at(wid, ElementState::Released, MouseButton::Left, at);
    }
    assert_eq!(
        counts,
        [1, 2, 1, 1, 2, 1, 1],
        "a press a millisecond after the interval starts a new run, after a press or a \
         shift-click"
    );
}

fn stacked_autoscroll_fixture() -> (crate::App, crate::WindowId, u64, u64) {
    use crate::{App, WindowId, pane, term_lock};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let top = app.focused_session_id(wid).expect("top terminal");
    let bottom = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
    let plan = app
        .active_visible_leaf_plan(wid)
        .expect("stacked visible plan");
    assert_eq!(plan.dividers.len(), 1);
    assert_eq!(
        plan.dividers[0].axis,
        crate::tab_model::SplitAxis::Vertical,
        "fixture must stack panes rather than place them side by side"
    );
    assert_eq!(app.focused_session_id(wid), Some(bottom));

    for session in [top, bottom] {
        let term = app.pool.get(session).expect("pooled terminal").term.clone();
        let mut terminal = term_lock(&term);
        for line in 0..100 {
            terminal.process(format!("pane {session} history {line}\r\n").as_bytes());
        }
        assert!(terminal.grid().scrollback_lines() > 8);
        assert_eq!(terminal.grid().display_offset(), 0);
    }
    (app, wid, top, bottom)
}

#[test]
fn split_selection_motion_reuses_the_mapped_pane_rect() {
    use crate::{reset_visible_leaf_plan_builds, visible_leaf_plan_builds};

    let (mut app, wid, _, _) = stacked_autoscroll_fixture();
    let (row, col, rows, cols) = app.focused_pane_rect(wid);
    assert!(rows > 2 && cols > 2, "fixture needs an interior pane cell");
    let (x, y) = app
        .cell_centre_px(wid, row + 1, col + 1)
        .expect("interior bottom-pane pixel");

    reset_visible_leaf_plan_builds();
    app.on_cursor_moved(wid, x, y);
    let plain_motion_builds = visible_leaf_plan_builds();
    assert_eq!(
        plain_motion_builds, 1,
        "split motion must build a single VisibleLeafPlan across header, cell, hover, and routing"
    );

    app.begin_selection(wid, SelectionType::Simple);
    reset_visible_leaf_plan_builds();
    app.on_cursor_moved(wid, x, y);
    assert_eq!(
        visible_leaf_plan_builds(),
        plain_motion_builds,
        "autoscroll must reuse refresh_mouse_cell's pane rect"
    );
}

#[test]
fn zoomed_split_pointer_motion_and_hover_build_no_leaf_plans() {
    use crate::{reset_visible_leaf_plan_builds, visible_leaf_plan_builds};

    let (mut app, wid, _, _) = stacked_autoscroll_fixture();
    app.toggle_pane_zoom();
    assert!(app.active_tab_is_single_focused_pane(wid));
    let (x, y) = app
        .cell_centre_px(wid, 3, 5)
        .expect("interior zoomed pixel");

    reset_visible_leaf_plan_builds();
    app.on_cursor_moved(wid, x, y);
    app.update_hover_cursor(wid);
    assert_eq!(
        visible_leaf_plan_builds(),
        0,
        "a zoomed split tab occupies the full grid and needs no VisibleLeafPlan on pointer motion"
    );
}

#[test]
fn native_settings_pointer_motion_and_rehover_build_one_leaf_plan() {
    use crate::{App, WindowId, reset_visible_leaf_plan_builds, visible_leaf_plan_builds};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let _ = app.commit_glassless_native_frame(wid);

    reset_visible_leaf_plan_builds();
    app.on_cursor_moved(wid, 120.0, 140.0);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "native pointer motion must reuse a single VisibleLeafPlan across editor target, hover hit, and divider checks"
    );

    app.windows.get_mut(&wid).expect("window").native_rehover = true;
    reset_visible_leaf_plan_builds();
    app.refresh_native_hover_for_frame(wid);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "refresh_native_hover_for_frame must reuse a single VisibleLeafPlan"
    );
}

#[test]
fn mouse_input_and_wheel_reuse_a_single_leaf_plan_or_skip_it_on_single_pane_tabs() {
    use super::{ElementState, MouseScrollDelta, WinitMouseButton};
    use crate::{App, reset_visible_leaf_plan_builds, visible_leaf_plan_builds};

    // 1. Single-pane and zoomed-split terminal tabs build 0 plans on press, release, and wheel.
    let (mut app, wid, _, bottom) = stacked_autoscroll_fixture();
    app.toggle_pane_zoom();
    let (x, y) = app.cell_centre_px(wid, 3, 5).expect("interior cell");
    app.on_cursor_moved(wid, x, y);
    reset_visible_leaf_plan_builds();
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    assert_eq!(
        visible_leaf_plan_builds(),
        0,
        "zoomed/single-pane terminal press, release, and wheel need no VisibleLeafPlan"
    );

    // 2. Unzoomed multi-pane split terminal builds 1 plan per press, release, and wheel.
    app.toggle_pane_zoom();
    let (row, col, _, _) = app.focused_pane_rect(wid);
    let (x, y) = app
        .cell_centre_px(wid, row + 1, col + 1)
        .expect("interior split cell");
    app.on_cursor_moved(wid, x, y);

    reset_visible_leaf_plan_builds();
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "split terminal press must share one VisibleLeafPlan"
    );
    reset_visible_leaf_plan_builds();
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "split terminal release must share one VisibleLeafPlan"
    );
    reset_visible_leaf_plan_builds();
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "split terminal wheel must share one VisibleLeafPlan"
    );

    // 3. Rewound split terminal pane builds 1 plan on press and release, and 0 on rewind_presented.
    let mut rewind_view = crate::rewind::RewindView::new(0);
    rewind_view.shown = Some(app.pool.get(bottom).expect("bottom session").term.clone());
    app.pool.set_rewind(bottom, Some(rewind_view));
    reset_visible_leaf_plan_builds();
    app.rewind_presented(wid);
    assert_eq!(
        visible_leaf_plan_builds(),
        0,
        "rewind_presented visits active tab leaves without building a VisibleLeafPlan"
    );
    reset_visible_leaf_plan_builds();
    app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "rewound split terminal press must share one VisibleLeafPlan across band check and selection"
    );
    reset_visible_leaf_plan_builds();
    app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "rewound split terminal release must share one VisibleLeafPlan"
    );
    app.pool.set_rewind(bottom, None);

    // 4. Native Settings and heterogeneous split build 1 plan on commit_glassless_native_frame, press, release, and wheel.
    let mut native_app = App::headless_for_test();
    assert!(native_app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    native_app.on_cursor_moved(wid, 120.0, 140.0);
    native_app
        .windows
        .get_mut(&wid)
        .expect("window")
        .native_rehover = true;
    reset_visible_leaf_plan_builds();
    assert!(native_app.commit_glassless_native_frame(wid));
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "commit_glassless_native_frame must share one VisibleLeafPlan across scratch preparation and rehover"
    );

    reset_visible_leaf_plan_builds();
    native_app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "native Settings press must share one VisibleLeafPlan"
    );
    reset_visible_leaf_plan_builds();
    native_app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "native Settings release must share one VisibleLeafPlan"
    );
    reset_visible_leaf_plan_builds();
    native_app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -1.0));
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "native Settings wheel must share one VisibleLeafPlan"
    );

    let mut mixed_app = App::headless_for_test();
    mixed_app.open_allocation(true).expect("open mixed split");
    mixed_app.on_cursor_moved(wid, 120.0, 140.0);
    mixed_app
        .windows
        .get_mut(&wid)
        .expect("window")
        .native_rehover = true;
    reset_visible_leaf_plan_builds();
    assert!(mixed_app.commit_glassless_native_frame(wid));
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "heterogeneous commit_glassless_native_frame must share one VisibleLeafPlan"
    );
    reset_visible_leaf_plan_builds();
    mixed_app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -1.0));
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "heterogeneous split wheel must share one VisibleLeafPlan"
    );

    // 5. Palette live, menu live, IME recipient, and overlay coordinate transform build 0 plans on split tabs.
    reset_visible_leaf_plan_builds();
    let _ = app.palette_live();
    app.publish_menu_live();
    let _ = app.overlay_coordinate_transform(wid);
    let _ = mixed_app.overlay_coordinate_transform(wid);
    app.dispatch_ime_event(
        wid,
        aterm_window::event::Ime::Enabled,
        aterm_window::event::InputOrigin::Person,
    );
    app.dispatch_ime_event(
        wid,
        aterm_window::event::Ime::Preedit("a".to_owned(), Some((1, 1))),
        aterm_window::event::InputOrigin::Person,
    );
    let _ = app.preedit_owner(wid);
    app.dispatch_ime_event(
        wid,
        aterm_window::event::Ime::Disabled,
        aterm_window::event::InputOrigin::Person,
    );
    assert_eq!(
        visible_leaf_plan_builds(),
        0,
        "palette_live, publish_menu_live, overlay_coordinate_transform, and dispatch_ime_event need no VisibleLeafPlan"
    );

    // 6. Single-pane/zoomed focus_pane_would_move, single-pane shared_target_geometry, and drawn_chip_lines build 0 plans.
    let mut single_native_app = App::headless_for_test();
    assert!(
        single_native_app.open_settings_tab(crate::native_settings::SettingsRoute::CursorStyles)
    );
    reset_visible_leaf_plan_builds();
    for dir in [
        crate::pane::FocusDir::Left,
        crate::pane::FocusDir::Right,
        crate::pane::FocusDir::Up,
        crate::pane::FocusDir::Down,
    ] {
        assert!(!single_native_app.focus_pane_would_move(wid, dir));
    }
    let _ = single_native_app.shared_target_geometry(0);
    let _ = single_native_app.drawn_chip_lines();
    let _ = app.drawn_chip_lines();
    let _ = mixed_app.drawn_chip_lines();
    assert_eq!(
        visible_leaf_plan_builds(),
        0,
        "single-pane native focus_pane_would_move, shared_target_geometry, and unsupervised drawn_chip_lines need no VisibleLeafPlan"
    );

    #[cfg(a11y_tree)]
    {
        mixed_app.windows.get_mut(&wid).expect("window").a11y_active = true;
        reset_visible_leaf_plan_builds();
        mixed_app.push_a11y_tree(wid);
        assert_eq!(
            visible_leaf_plan_builds(),
            1,
            "push_a11y_tree must share a single VisibleLeafPlan across client_reads, native a11y, and chrome"
        );
    }
}

#[test]
fn a_stacked_pane_autoscrolls_at_its_own_edge() {
    use crate::term_lock;

    for top_pane in [true, false] {
        let (mut app, wid, top, bottom) = stacked_autoscroll_fixture();
        if top_pane {
            assert!(
                app.active_tree_mut(wid)
                    .expect("terminal pane tree")
                    .set_focus(top)
            );
            let active = app.windows[&wid].tabs.active;
            assert!(app.sync_tab_model_from_layout(wid, active));
            app.sync_window(wid);
            assert_eq!(app.focused_session_id(wid), Some(top));
        }
        let sid = if top_pane { top } else { bottom };
        let term = app.pool.get(sid).expect("pane terminal").term.clone();
        if top_pane {
            term_lock(&term).scroll_display(5);
        }
        app.windows.get_mut(&wid).expect("window").selecting = true;

        let (pane_row, _, pane_rows, _) = app.focused_pane_rect(wid);
        assert!(pane_rows < app.windows[&wid].rows);
        let ch = app.win_cell_size(wid).1.max(1);
        let strip_px = usize::from(app.chrome_rows(wid)) * ch;
        let top_px = app.win_pad_top(wid) + app.win_head(wid);
        let pointer = if top_pane {
            (top_px + strip_px + usize::from(pane_row.saturating_add(pane_rows)) * ch) as f64
        } else {
            assert!(pane_row > 0, "the new terminal is the bottom pane");
            (top_px + strip_px + usize::from(pane_row) * ch) as f64 - 1.0
        };
        assert_eq!(
            crate::app_render::selection_autoscroll_lines(
                pointer,
                top_px,
                strip_px,
                ch,
                app.windows[&wid].rows,
            ),
            0,
            "top_pane={top_pane}: negative control: inside the whole-window grid"
        );
        let (_, frame_y) = app.frame_origin(wid);
        assert!(app.selection_autoscroll(wid, frame_y as f64 + pointer));
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            if top_pane { 4 } else { 1 },
            "top_pane={top_pane}: crossing the pane's own edge scrolls it"
        );
    }
}

#[test]
fn tab_strip_selection_motion_refreshes_and_extends_the_endpoint_immediately() {
    use crate::{App, WindowId, term_lock};

    fn run(with_history: bool) -> (usize, i32, u16, (u16, u16), bool) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.splice_tab_strip(wid);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        if with_history {
            term_lock(&term).process(&numbered_screen("L"));
            term_lock(&term).process(b"\r\nL24");
            assert_eq!(term_lock(&term).grid().scrollback_lines(), 1);
        }
        app.windows.get_mut(&wid).expect("window").last_mouse_cell = (8, 3);
        app.begin_selection(wid, SelectionType::Simple);

        let (cw, ch) = app.win_cell_size(wid);
        let (frame_x, frame_y) = app.frame_origin(wid);
        let strip_col = 7u16;
        let x = frame_x as f64 + (app.win_pad(wid) + usize::from(strip_col) * cw + cw / 2) as f64;
        let y = frame_y as f64 + (app.win_pad_top(wid) + app.win_head(wid) + ch / 2) as f64;
        assert_eq!(app.strip_col_at(wid, x, y), Some(strip_col));

        app.on_cursor_moved(wid, x, y);

        let terminal = term_lock(&term);
        (
            terminal.grid().display_offset(),
            terminal.text_selection().end().row,
            terminal.text_selection().end().col,
            app.windows[&wid].last_mouse_cell,
            app.windows[&wid].next_autoscroll.is_some(),
        )
    }

    assert_eq!(run(true), (1, -1, 7, (0, 7), true));
    assert_eq!(
        run(false),
        (0, 0, 7, (0, 7), true),
        "endpoint motion does not depend on viewport motion"
    );
}

#[test]
fn selection_autoscroll_disarms_when_the_front_terminal_is_gone() {
    use std::time::Instant;

    use crate::{App, WindowId};

    let wid = WindowId(0);
    let mut native = App::headless_for_test();
    assert!(native.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    {
        let window = native.windows.get_mut(&wid).expect("headless window");
        window.selecting = true;
        window.next_autoscroll = Some(Instant::now());
    }
    let past_bottom = 10_000.0;
    let guard_ch = native.win_cell_size(wid).1;
    assert_ne!(
        crate::app_render::selection_autoscroll_lines(
            past_bottom,
            native.win_pad_top(wid) + native.win_head(wid),
            usize::from(native.chrome_rows(wid)) * guard_ch,
            guard_ch,
            native.windows[&wid].rows,
        ),
        0,
        "fixture must place the pointer past a grid edge"
    );
    assert!(!native.selection_autoscroll(wid, past_bottom));
    assert!(
        native.windows[&wid].next_autoscroll.is_none(),
        "a retired front terminal must not leave an already-due wake armed"
    );

    let mut terminal = App::headless_for_test();
    terminal
        .windows
        .get_mut(&wid)
        .expect("headless window")
        .selecting = true;
    assert!(!terminal.selection_autoscroll(wid, past_bottom));
    assert!(
        terminal.windows[&wid].next_autoscroll.is_some(),
        "a live edge drag must still rearm"
    );
}

#[test]
fn a_selection_drag_above_the_window_top_scrolls_into_history() {
    use crate::{App, WindowId};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut terminal = crate::term_lock(&term);
        for line in 0..100 {
            terminal.process(format!("history {line}\r\n").as_bytes());
        }
        assert!(terminal.grid().scrollback_lines() > 8);
        assert_eq!(
            terminal.grid().display_offset(),
            0,
            "the drag starts at the live bottom"
        );
    }
    app.windows
        .get_mut(&wid)
        .expect("headless window")
        .selecting = true;

    assert!(
        app.selection_autoscroll(wid, -1.0),
        "one pixel above the top edge scrolls one line into history"
    );
    assert_eq!(crate::term_lock(&term).grid().display_offset(), 1);
    assert!(app.windows[&wid].next_autoscroll.is_some());

    let ch = app.win_cell_size(wid).1 as f64;
    assert!(app.selection_autoscroll(wid, -4.0 * ch));
    assert_eq!(crate::term_lock(&term).grid().display_offset(), 6);
}

#[test]
fn unfocused_pane_clamped_cell_exposes_no_focused_links_and_banners_shield_dividers_and_non_left_clicks()
 {
    use aterm_window::event::{ElementState, MouseButton};
    use aterm_window::window::CursorIcon;

    use crate::{FindBarHit, term_lock};

    let (mut app, wid, top, bottom) = stacked_autoscroll_fixture();
    let bottom_term = app.pool.get(bottom).expect("bottom terminal").term.clone();
    term_lock(&bottom_term).process(b"\x1b[2J\x1b[Hhttps://example.com");
    let (bottom_row, bottom_col, ..) = app.focused_pane_rect(wid);

    let (bx, by) = app
        .cell_centre_px(wid, bottom_row, bottom_col + 2)
        .expect("focused pane row 0");
    app.on_cursor_moved(wid, bx, by);
    assert_eq!(
        app.link_under_pointer(wid).as_deref(),
        Some("https://example.com")
    );

    let (tx, ty) = app.cell_centre_px(wid, 0, 2).expect("unfocused top pane");
    app.on_cursor_moved(wid, tx, ty);
    assert!(!app.pointer_is_inside_focused_pane(wid));
    assert_eq!(
        app.windows[&wid].last_mouse_cell,
        (0, 2),
        "last_mouse_cell clamps to the focused pane's top edge"
    );
    assert_eq!(app.link_under_pointer(wid), None);
    assert_eq!(app.path_link_under_pointer(wid), None);

    let (top_row, _, top_rows, _) = app.visible_session_rect(wid, top).expect("top pane");
    let div_row = top_row + top_rows;
    let (dx, dy) = app.cell_centre_px(wid, div_row, 2).expect("divider cell");
    app.on_cursor_moved(wid, dx, dy);
    assert_eq!(
        app.divider_cursor_under_pointer(wid),
        Some(CursorIcon::RowResize)
    );

    let laid = app.glass_grid(wid);
    let cols = usize::from(app.windows[&wid].cols);
    app.windows.get_mut(&wid).expect("window").find_bar_hit = Some(FindBarHit {
        row: usize::from(div_row),
        case_cols: None,
        regex_cols: None,
        prev_cols: None,
        next_cols: None,
        close_cols: None,
        field_cols: 0..cols,
        field_scroll: 0,
        band: usize::from(div_row)..usize::from(div_row) + 1,
        cols: 0..cols,
        laid,
    });
    assert_eq!(app.divider_cursor_under_pointer(wid), None);
    assert!(!app.begin_divider_drag(wid));

    for session in [top, bottom] {
        let term = app.pool.get(session).expect("pane terminal").term.clone();
        term_lock(&term).process(b"\x1b[?1003h\x1b[?1006h");
    }
    for button in [MouseButton::Middle, MouseButton::Right] {
        app.on_mouse_input(wid, ElementState::Pressed, button);
        assert_eq!(
            app.windows[&wid].reported_buttons, 0,
            "find bar must swallow {button:?} press"
        );
        app.on_mouse_input(wid, ElementState::Released, button);
    }
}

#[test]
fn find_bar_shields_native_hover_and_routes_click_when_native_view_is_active() {
    use aterm_window::event::{ElementState, MouseButton};

    use crate::native_settings::SettingsRoute;
    use crate::{App, FindBarHit, WindowId};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("Settings view");
    assert!(app.commit_glassless_native_frame(wid));

    let (key, px, py) = {
        let artifact = app
            .retained_native_pointer_artifact(wid, view)
            .expect("presented Settings frame");
        let hit = artifact.compiled.hits.first().expect("hit target");
        let x = artifact.device_x as f64
            + f64::from(hit.rect.x + hit.rect.width * 0.5) * artifact.scale;
        let y = artifact.device_y as f64
            + f64::from(hit.rect.y + hit.rect.height * 0.5) * artifact.scale;
        (hit.key.clone(), x, y)
    };
    app.on_cursor_moved(wid, px, py);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.clone()),
        Some(key),
        "control: hovering uncovered native control sets hovered"
    );

    let (frame_row, _) = app.frame_cell_at(wid, px, py).expect("frame cell");
    let term_row = frame_row.saturating_sub(usize::from(app.chrome_rows(wid)));
    let cols = usize::from(app.windows[&wid].cols);
    let laid = app.glass_grid(wid);
    app.windows.get_mut(&wid).expect("window").search =
        Some(crate::app_search::SearchState::default());
    app.windows.get_mut(&wid).expect("window").find_bar_hit = Some(FindBarHit {
        row: term_row,
        case_cols: Some(0..cols),
        regex_cols: None,
        prev_cols: None,
        next_cols: None,
        close_cols: None,
        field_cols: 0..0,
        field_scroll: 0,
        band: term_row..term_row + 1,
        cols: 0..cols,
        laid,
    });

    app.on_cursor_moved(wid, px, py);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.clone()),
        None,
        "find bar shields native hover hit underneath it"
    );

    app.frontmost_window = Some(WindowId(99));
    let before_case = app.windows[&wid]
        .search
        .as_ref()
        .expect("search")
        .case_sensitive;
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_ne!(
        app.windows[&wid]
            .search
            .as_ref()
            .expect("search")
            .case_sensitive,
        before_case,
        "left click on find bar case toggle targets wid even when frontmost_window differs"
    );

    app.windows
        .get_mut(&wid)
        .expect("window")
        .find_bar_hit
        .as_mut()
        .expect("find bar hit")
        .case_cols = None;
    app.windows
        .get_mut(&wid)
        .expect("window")
        .find_bar_hit
        .as_mut()
        .expect("find bar hit")
        .regex_cols = Some(0..cols);
    let before_regex = app.windows[&wid].search.as_ref().expect("search").is_regex;
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_ne!(
        app.windows[&wid].search.as_ref().expect("search").is_regex,
        before_regex,
        "left click on find bar regex toggle targets wid even when frontmost_window differs"
    );
}
