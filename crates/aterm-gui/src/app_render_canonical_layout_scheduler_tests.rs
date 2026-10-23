// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{reset_visible_leaf_plan_builds, visible_leaf_plan_builds};

fn assert_planned_frame_builds_once(mut app: App, expected: crate::VisibleContentRoute) {
    let wid = WindowId(0);
    reset_visible_leaf_plan_builds();
    let (plan, route) = app.active_visible_frame_layout(wid).expect("frame layout");
    assert_eq!(route, expected, "negative-control fixture route");
    app.windows.get_mut(&wid).unwrap().panes_stale = true;
    if app.windows[&wid].panes_stale {
        let site = crate::resize_ledger::Site::caller(
            crate::resize_ledger::Cause::Redraw,
            std::panic::Location::caller(),
        );
        app.resize_panes_scoped_with_active_plan(wid, true, Some(&plan), site);
    }
    app.prepare_layout_coordinate_space_from_plan(wid, route, &plan);
    let prepared = match route {
        crate::VisibleContentRoute::Terminal { .. } => true,
        crate::VisibleContentRoute::Native { .. } => {
            app.prepare_native_input_scratch_from_plan(wid, &plan)
        }
        crate::VisibleContentRoute::Heterogeneous => app
            .prepare_heterogeneous_input_scratch_with_cursor_fx_from_plan(
                wid,
                Some(ComposedCursorFxClock::Advance(Instant::now())),
                &plan,
            )
            .is_some(),
    };
    assert!(prepared, "planned route preparation must succeed");
    app.splice_link_target_from_plan(wid, &plan);
    let successful_route = match route {
        crate::VisibleContentRoute::Terminal { .. } => SuccessfulPresentRoute::Terminal,
        crate::VisibleContentRoute::Heterogeneous => SuccessfulPresentRoute::Heterogeneous,
        crate::VisibleContentRoute::Native { view, .. } => SuccessfulPresentRoute::Native {
            view,
            presented_stamp: app.windows[&wid]
                .native_ui_compiled
                .as_ref()
                .map(|frame| frame.stamp),
        },
    };
    app.first_present_done = true;
    app.finalize_successful_present(
        wid,
        crate::metrics::StartupPresentTiming::collapsed(Instant::now()),
        0,
        None,
        successful_route,
        route,
        HostVisualState::default(),
        &plan,
    );
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "route, geometry, preparation, cursor effects, chrome and finalization reuse one plan",
    );

    reset_visible_leaf_plan_builds();
    let (_, route) = app.active_visible_frame_layout(wid).expect("outer layout");
    let prepared = match route {
        crate::VisibleContentRoute::Terminal { .. } => {
            app.prepare_layout_coordinate_space(wid, route);
            true
        }
        crate::VisibleContentRoute::Native { .. } => app.prepare_native_input_scratch(wid),
        crate::VisibleContentRoute::Heterogeneous => app
            .prepare_heterogeneous_input_scratch_with_cursor_fx(wid, None)
            .is_some(),
    };
    assert!(prepared, "standalone negative-control preparation");
    assert_eq!(
        visible_leaf_plan_builds(),
        2,
        "an unthreaded outer frame plus standalone consumer rebuilds the plan",
    );
}

#[test]
fn every_frame_route_threads_one_visible_plan() {
    type Fixture = fn() -> (App, crate::VisibleContentRoute);
    let fixtures: [Fixture; 3] = [
        || {
            (
                App::headless_for_test(),
                crate::VisibleContentRoute::Terminal { composed: false },
            )
        },
        || {
            let mut app = App::headless_for_test();
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
            let (instance, view) = app.active_native_view(WindowId(0)).expect("native front");
            (app, crate::VisibleContentRoute::Native { instance, view })
        },
        || {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
            (app, crate::VisibleContentRoute::Heterogeneous)
        },
    ];
    for fixture in fixtures {
        let (app, route) = fixture();
        assert_planned_frame_builds_once(app, route);
    }
}

#[test]
fn terminal_capture_threads_one_fresh_visible_plan() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = Instant::now();

    reset_visible_leaf_plan_builds();
    let (request_plan, request_route) = app
        .active_visible_frame_layout(wid)
        .expect("request layout");
    assert_eq!(
        request_route,
        crate::VisibleContentRoute::Terminal { composed: false }
    );
    let (grid, plan) = match app.prepare_terminal_capture_grid_with_cursor_fx_for_plan_outcome(
        wid,
        request_plan,
        ComposedCursorFxClock::Advance(now),
    ) {
        CapturePreparation::Ready(prepared) => prepared,
        other => panic!("terminal capture was not ready: {other:?}"),
    };
    assert!(!grid.composed);
    app.splice_link_target_from_plan(wid, &plan);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "request routing, capture, cursor effects and chrome reuse one owned plan",
    );

    reset_visible_leaf_plan_builds();
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Retain {
                observed_at: now + Duration::from_millis(1),
            },
        )
        .is_some()
    );
    assert_eq!(visible_leaf_plan_builds(), 1, "single-pane Retain");

    app.split_active_stub_tab(wid);
    for (label, clock) in [
        (
            "composed Advance",
            ComposedCursorFxClock::Advance(now + Duration::from_millis(2)),
        ),
        (
            "composed Retain",
            ComposedCursorFxClock::Retain {
                observed_at: now + Duration::from_millis(3),
            },
        ),
    ] {
        reset_visible_leaf_plan_builds();
        let grid = app
            .prepare_terminal_capture_grid_with_cursor_fx(wid, clock)
            .expect(label);
        assert!(grid.composed, "{label}");
        assert_eq!(visible_leaf_plan_builds(), 1, "{label}");
    }

    reset_visible_leaf_plan_builds();
    let route = app
        .active_visible_content_route(wid)
        .expect("terminal route");
    assert!(app.prepare_layout_coordinate_space(wid, route));
    assert!(app.active_visible_leaf_plan(wid).is_some());
    assert_eq!(visible_leaf_plan_builds(), 3);
}

fn charge_cursor_body(state: &mut WindowState, now: Instant) {
    let geom = crate::geom_24x80_for_test();
    let mut halo = Vec::new();
    state.fx.cursor_mut().rainbow.tick(
        Some((2, 3)),
        now,
        1.0,
        true,
        true,
        geom,
        &crate::cursor_rainbow::RainbowConfig {
            enabled: true,
            intensity: 1.0,
            blinking: false,
            base: None,
            head_rgb: None,
            paint: None,
            ground: None,
            flare_at: None,
        },
        &mut halo,
    );
    state.fx.cursor_mut().droplet.tick(
        Some((2, 3)),
        now,
        1.0,
        geom,
        &crate::cursor_droplet::DropletConfig {
            enabled: true,
            intensity: 1.0,
            ground: None,
        },
        &mut halo,
    );
    state.fx.cursor_mut().beamrod.tick(
        Some((2, 3)),
        now,
        1.0,
        geom,
        &crate::cursor_beam::BeamRodConfig {
            enabled: true,
            intensity: 1.0,
            color: 0x0050_FA7B,
            haze: 0x0018_3040,
            bar: false,
            shimmer: false,
            dark_theme: true,
            theme_fg: 0x00C8_D3F5,
            theme_bg: 0x001A_1B26,
            pinned: false,
        },
        &mut halo,
        &mut Vec::new(),
    );
    state.fx.cursor_mut().comet.tick(
        Some((2, 3)),
        now,
        1.0,
        geom,
        &crate::cursor_comet::CometConfig {
            enabled: true,
            intensity: 1.0,
            color: 0x0050_FA7B,
            accent: 0x00FF_FFFF,
            dark_theme: true,
            theme_fg: 0x00C8_D3F5,
            theme_bg: 0x001A_1B26,
            pinned: false,
        },
        &mut halo,
        &mut Vec::new(),
    );
    state.fx.cursor_mut().phaser.tick(
        Some((2, 3)),
        now,
        0.25,
        1.0,
        true,
        geom,
        &crate::cursor_phaser::PhaserConfig {
            enabled: true,
            intensity: 1.0,
            base: None,
        },
        &mut halo,
    );
    assert!(
        state.fx.cursor_mut().rainbow.is_active()
            && state.fx.cursor_mut().droplet.is_active()
            && state.fx.cursor_mut().beamrod.is_active()
            && state.fx.cursor_mut().comet.is_active()
            && state.fx.cursor_mut().phaser.is_active(),
        "fixture needs every retained cursor body engine charged"
    );
}

fn cursor_bodies_are_retired(state: &WindowState) -> bool {
    !state.fx.cursor().rainbow.is_active()
        && !state.fx.cursor().droplet.is_active()
        && !state.fx.cursor().beamrod.is_active()
        && !state.fx.cursor().comet.is_active()
        && !state.fx.cursor().phaser.is_active()
}

fn legacy_projection_is_split(state: &WindowState) -> bool {
    state
        .layouts
        .get(state.tabs.active)
        .is_some_and(|tree| tree.len() > 1)
}

#[test]
fn stable_single_pane_control_keeps_body_cadence_and_single_pane_latches() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = Instant::now();
    let route = app.active_visible_content_route(wid).unwrap();
    assert!(!app.prepare_layout_coordinate_space(wid, route));
    {
        let state = app.windows.get_mut(&wid).unwrap();
        state.focused = true;
        state.last_composed = Some(true);
        state.pill_shown = true;
        state.composed_cursor_effect_valid = true;
        state.composed_cursor_effect_session = Some(0);
        charge_cursor_body(state, now);
        assert!(!state.is_split());
        assert!(!legacy_projection_is_split(state));
    }

    assert!(!app.prepare_layout_coordinate_space(wid, route));
    let state = &app.windows[&wid];
    assert_eq!(state.last_composed, Some(false));
    assert!(
        state.pill_shown,
        "single-pane scroll paint remains supported"
    );
    assert!(!state.composed_cursor_effect_valid);
    assert_eq!(state.composed_cursor_effect_session, None);
    assert!(
        state.cursor_dependents_need_frame_cadence(now, false),
        "the single-pane control still schedules its charged cursor body"
    );
    assert!(state.terminal_effect_frame_active(now, false));
}

#[test]
fn pure_split_control_clears_single_pane_latches_and_includes_body_cadence() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.split_active_stub_tab(wid);
    let now = Instant::now();
    let route = app.active_visible_content_route(wid).unwrap();
    assert!(app.prepare_layout_coordinate_space(wid, route));
    {
        let state = app.windows.get_mut(&wid).unwrap();
        state.focused = true;
        state.last_composed = Some(false);
        state.pill_shown = true;
        state.fx.cursor_mut().glow.note_kill(now, true);
        charge_cursor_body(state, now);
        assert!(state.is_split());
        assert!(
            legacy_projection_is_split(state),
            "pure terminal split is the positive legacy-projection control"
        );
    }

    assert!(app.prepare_layout_coordinate_space(wid, route));
    let state = &app.windows[&wid];
    assert_eq!(state.last_composed, Some(true));
    assert!(!state.pill_shown);
    assert!(
        state.fx.cursor().glow.is_active(),
        "an unchanged composed coordinate keeps its charged glow"
    );
    assert!(
        state.fx.cursor().rainbow.is_active(),
        "the composed renderer owns the same live rainbow body"
    );
    assert!(state.cursor_dependents_need_frame_cadence(now, false));
    assert!(state.cursor_fx_active(now, false));
    assert!(
        state.terminal_effect_frame_active(now, false),
        "a live composed body owns the pure-split animation cadence"
    );
}

#[test]
fn same_class_zoom_transition_resets_coordinate_bound_cursor_effects() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.split_active_stub_tab(wid);
    let route = app.active_visible_content_route(wid).unwrap();
    assert!(app.prepare_layout_coordinate_space(wid, route));

    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .note_kill(Instant::now(), true);
    charge_cursor_body(app.windows.get_mut(&wid).unwrap(), Instant::now());
    let old_key = app.windows[&wid].last_layout_coordinate_space;
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .toggle_zoom()
    );

    let zoomed_route = app.active_visible_content_route(wid).unwrap();
    assert_eq!(
        zoomed_route, route,
        "both layouts are composed, so the retired boolean misses the edge"
    );
    assert!(app.prepare_layout_coordinate_space(wid, zoomed_route));
    assert_ne!(app.windows[&wid].last_layout_coordinate_space, old_key);
    assert!(!app.windows[&wid].fx.cursor().glow.is_active());
    assert!(!app.windows[&wid].fx.cursor().rainbow.is_active());
}

#[test]
fn authoritative_headless_split_capture_retires_charged_bodies_after_divider_move() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.split_active_stub_tab(wid);
    assert!(app.prepare_terminal_capture_grid(wid).is_some());

    let now = Instant::now();
    let state = app.windows.get_mut(&wid).expect("headless window");
    state.fx.cursor_mut().glow.note_kill(now, true);
    charge_cursor_body(state, now);
    let divider = app.active_visible_leaf_plan(wid).unwrap().dividers[0]
        .path
        .clone();
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_divider_ratio(&divider, 0.7)
    );
    assert!(
        app.windows[&wid].fx.cursor().glow.is_active()
            && !cursor_bodies_are_retired(&app.windows[&wid]),
        "negative control: the charged old geometry survives until capture preparation"
    );

    assert!(app.prepare_terminal_capture_grid(wid).is_some());
    assert!(!app.windows[&wid].fx.cursor().glow.is_active());
    assert!(
        cursor_bodies_are_retired(&app.windows[&wid]),
        "authoritative split capture retires every body bound to the old divider"
    );
}

#[test]
fn authoritative_headless_mixed_zoom_capture_retires_charged_bodies() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());

    let now = Instant::now();
    let state = app.windows.get_mut(&wid).expect("headless window");
    state.fx.cursor_mut().glow.note_kill(now, true);
    charge_cursor_body(state, now);
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .toggle_zoom()
    );
    assert!(
        app.windows[&wid].fx.cursor().glow.is_active()
            && !cursor_bodies_are_retired(&app.windows[&wid]),
        "negative control: zoom alone does not bypass the capture preparation seam"
    );

    let route = app
        .active_visible_content_route(wid)
        .expect("zoomed visible route");
    let prepared = match route {
        crate::VisibleContentRoute::Terminal { .. } => {
            app.prepare_terminal_capture_grid(wid).is_some()
        }
        crate::VisibleContentRoute::Native { .. } => app.prepare_native_input_scratch(wid),
        crate::VisibleContentRoute::Heterogeneous => {
            app.prepare_heterogeneous_input_scratch(wid).is_some()
        }
    };
    assert!(prepared, "the authoritative zoomed route prepares a frame");
    assert!(!app.windows[&wid].fx.cursor().glow.is_active());
    assert!(
        cursor_bodies_are_retired(&app.windows[&wid]),
        "mixed-to-zoom capture retires every body bound to the old pane geometry"
    );
}

#[test]
fn authoritative_headless_tab_capture_retires_charged_bodies() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.prepare_terminal_capture_grid(wid).is_some());

    let now = Instant::now();
    let state = app.windows.get_mut(&wid).expect("headless window");
    state.fx.cursor_mut().glow.note_kill(now, true);
    charge_cursor_body(state, now);
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    assert!(
        !cursor_bodies_are_retired(&app.windows[&wid]),
        "negative control: the body engines remain charged until capture preparation"
    );
    assert!(
        !app.windows[&wid].fx.cursor().glow.is_active(),
        "the ordinary front-switch seam independently retires the classic glow"
    );

    assert!(app.prepare_terminal_capture_grid(wid).is_some());
    assert!(!app.windows[&wid].fx.cursor().glow.is_active());
    assert!(
        cursor_bodies_are_retired(&app.windows[&wid]),
        "authoritative tab capture retires every body from the prior view"
    );
}

#[test]
fn physical_surface_or_scale_change_resets_coordinate_bound_cursor_effects() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().win_px =
        Some(aterm_window::dpi::PhysicalSize::new(640, 384));
    let route = app.active_visible_content_route(wid).unwrap();
    assert!(!app.prepare_layout_coordinate_space(wid, route));

    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .note_kill(Instant::now(), true);
    let scale_key = app.windows[&wid].last_layout_coordinate_space;
    app.windows.get_mut(&wid).unwrap().scale = 2.0;
    assert!(!app.prepare_layout_coordinate_space(wid, route));
    assert_ne!(
        app.windows[&wid].last_layout_coordinate_space, scale_key,
        "DPI scale participates even when the cell-space route is unchanged"
    );
    assert!(!app.windows[&wid].fx.cursor().glow.is_active());

    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .note_kill(Instant::now(), true);
    let surface_key = app.windows[&wid].last_layout_coordinate_space;
    app.windows.get_mut(&wid).unwrap().win_px =
        Some(aterm_window::dpi::PhysicalSize::new(641, 384));
    assert!(!app.prepare_layout_coordinate_space(wid, route));
    assert_ne!(
        app.windows[&wid].last_layout_coordinate_space, surface_key,
        "sub-cell physical resize participates even when rows and columns do not"
    );
    assert!(!app.windows[&wid].fx.cursor().glow.is_active());
}

#[test]
fn layout_coordinate_change_rebases_cursor_kitty_and_pet_placement() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let route = app.active_visible_content_route(wid).unwrap();
    assert!(!app.prepare_layout_coordinate_space(wid, route));
    let now = Instant::now();

    {
        let state = app.windows.get_mut(&wid).unwrap();
        state
            .fx
            .companion_mut()
            .set_species(aterm_effects::kitty_pet::PetSpecies::Dog);
        let pet = crate::app_render::pet_tick_for_test(
            state,
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
        assert!(pet.alpha > 0 && state.fx.companion_mut().brain().is_active());
        assert!(
            state.fx.companion_mut().hit_rect().is_some(),
            "the live resident owns this frame's hit target"
        );

        for index in 0..120 {
            state
                .fx
                .companions_mut()
                .cursor_cat
                .on_key(now + Duration::from_millis(index * 40), true);
        }
        assert!(state.fx.companions_mut().cursor_cat.is_active());
        state.fx.companions_mut().cursor_cat.on_motion_pulse(
            aterm_effects::cursor_glow::CursorCatMotionPulse {
                at: now,
                kind: aterm_effects::cursor_glow::CursorCatMotionKind::FoldForward,
            },
        );
        state.composed_cursor_effect_valid = true;
        state.composed_cursor_fill = Some(0x0011_2233);
        state.composed_cursor_style_override = Some(CursorStyle::Bolt);
        state.input_scratch.cursor_fill_override = Some(0x0044_5566);
        state.input_scratch.cursor_effect_style_override = Some(CursorStyle::Bolt);
    }

    app.windows.get_mut(&wid).unwrap().scale = 2.0;
    assert!(!app.prepare_layout_coordinate_space(wid, route));
    let state = app.windows.get_mut(&wid).unwrap();
    assert!(
        state.fx.companion_mut().brain().is_active(),
        "a re-grid of the focused view keeps the resident's body"
    );
    assert!(state.fx.companion_mut().hit_rect().is_none());
    assert_eq!(
        state.fx.companion_mut().brain().species(),
        aterm_effects::kitty_pet::PetSpecies::Dog,
        "layout rebasing preserves the configured companion identity"
    );
    assert!(!state.fx.companions_mut().cursor_cat.is_active());
    assert!(
        state
            .fx
            .companions_mut()
            .cursor_cat
            .placement_frame(now)
            .fold
            .is_none()
    );
    assert!(!state.composed_cursor_effect_valid);
    assert!(state.composed_cursor_fill.is_none());
    assert!(state.composed_cursor_style_override.is_none());
    assert!(state.input_scratch.cursor_fill_override.is_none());
    assert!(state.input_scratch.cursor_effect_style_override.is_none());

    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    {
        let state = app.windows.get_mut(&wid).unwrap();
        let pet = crate::app_render::pet_tick_for_test(
            state,
            aterm_effects::host::CaptureMode::StaticCapture,
            aterm_effects::kitty_pet::PetSense {
                caret_drawn: true,
                now: now + Duration::from_secs(1),
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
        assert!(
            pet.alpha > 0,
            "control fixture: the resident is drawn again"
        );
    }
    let route = app.active_visible_content_route(wid).unwrap();
    assert!(!app.prepare_layout_coordinate_space(wid, route));
    let state = app.windows.get_mut(&wid).unwrap();
    assert!(
        !state.fx.companion_mut().brain().is_active()
            && !state.fx.companion_mut().brain().needs_frames(),
        "another view's surface retires the resident's coordinates"
    );
    assert!(state.fx.companion_mut().hit_rect().is_none());
    assert_eq!(
        state.fx.companion_mut().brain().species(),
        aterm_effects::kitty_pet::PetSpecies::Dog
    );
}

#[test]
fn shipping_layout_coordinate_reset_conforms_to_derived_model() {
    let model = aterm_models_misc::layout_coordinate_reset_model();
    let mut modeled = model.init_state();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.split_active_stub_tab(wid);
    let route = app.active_visible_content_route(wid).unwrap();
    assert!(app.prepare_layout_coordinate_space(wid, route));
    let old_key = app.windows[&wid]
        .last_layout_coordinate_space
        .expect("initial real coordinate key");

    assert!(model.fire("Charge", &mut modeled));
    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .note_kill(Instant::now(), true);
    assert_eq!(
        i64::from(app.windows[&wid].fx.cursor().glow.is_active()),
        modeled["charged"],
        "real and modeled effects enter the same charged state"
    );

    let divider = app.active_visible_leaf_plan(wid).unwrap().dividers[0]
        .path
        .clone();
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_divider_ratio(&divider, 0.7)
    );
    assert!(model.fire("ChangeCoordinate", &mut modeled));
    assert_eq!(modeled["prepared"], 0);
    assert_eq!(
        app.active_visible_content_route(wid),
        Some(route),
        "negative control: the coarse render route does not observe the divider move"
    );
    assert_eq!(
        app.windows[&wid].last_layout_coordinate_space,
        Some(old_key),
        "the old binding remains until the shipping prepare seam runs"
    );
    assert!(
        app.windows[&wid].fx.cursor().glow.is_active(),
        "negative control: skipping Prepare would carry a charged stale effect"
    );

    assert!(model.action_enabled("Prepare", &modeled));
    assert!(app.prepare_layout_coordinate_space(wid, route));
    assert!(model.fire("Prepare", &mut modeled));
    assert_ne!(
        app.windows[&wid].last_layout_coordinate_space,
        Some(old_key)
    );
    assert_eq!(
        i64::from(app.windows[&wid].fx.cursor().glow.is_active()),
        modeled["charged"]
    );
    assert_eq!(modeled["bound_coordinate"], modeled["coordinate"]);

    assert!(model.fire("Present", &mut modeled));
    assert!(model.check_invariant("NoStaleCoordinatePresent", &modeled));
    assert!(model.check_invariant("PreparedEffectsMatchCoordinate", &modeled));
}

#[test]
fn mixed_split_uses_canonical_root_and_schedules_the_composed_body() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let now = Instant::now();
    let route = app.active_visible_content_route(wid).unwrap();
    assert!(app.prepare_layout_coordinate_space(wid, route));
    {
        let state = app.windows.get_mut(&wid).unwrap();
        state.focused = true;
        state.last_composed = Some(false);
        state.pill_shown = true;
        state.fx.cursor_mut().glow.note_kill(now, true);
        charge_cursor_body(state, now);
        assert_eq!(state.tab_set.active().unwrap().root.len(), 2);
        assert!(
            !legacy_projection_is_split(state),
            "the mixed fixture reproduces the one-pane compatibility projection"
        );
        assert!(
            state.is_split(),
            "scheduler truth comes from the canonical mixed root"
        );
    }
    assert!(app.active_tab_contains_native(wid));

    assert!(app.prepare_layout_coordinate_space(wid, route));
    let state = &app.windows[&wid];
    assert_eq!(state.last_composed, Some(true));
    assert!(!state.pill_shown);
    assert!(state.fx.cursor().glow.is_active());
    assert!(
        state.fx.cursor().rainbow.is_active(),
        "the focused terminal body remains live in the mixed composed frame"
    );
    assert!(state.cursor_dependents_need_frame_cadence(now, false));
    assert!(state.cursor_fx_active(now, false));
    assert!(
        state.terminal_effect_frame_active(now, false),
        "the canonical mixed root keeps its live body on the animation lane"
    );
}

#[test]
fn fronting_a_native_tab_strips_the_terminal_frames_decoration_and_its_pet() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let terminal_route = app
        .active_visible_content_route(wid)
        .expect("the headless fixture starts on a terminal tab");
    assert!(!app.prepare_layout_coordinate_space(wid, terminal_route));

    let now = Instant::now();
    {
        let ws = app.windows.get_mut(&wid).expect("headless window");
        let sense = |now, caret| aterm_effects::kitty_pet::PetSense {
            caret_drawn: true,
            now,
            caret,
            rows: 24,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        };
        let _ = crate::app_render::pet_tick_for_test(
            ws,
            aterm_effects::host::CaptureMode::Present,
            sense(now, Some((2, 2))),
        );
        let _ = crate::app_render::pet_tick_for_test(
            ws,
            aterm_effects::host::CaptureMode::Present,
            sense(now + Duration::from_millis(16), Some((2, 2))),
        );
        let _ = crate::app_render::pet_tick_for_test(
            ws,
            aterm_effects::host::CaptureMode::Present,
            sense(now + Duration::from_millis(420), Some((2, 34))),
        );
        assert!(
            ws.fx.companion_mut().brain().is_active()
                && ws.fx.companion_mut().brain().needs_frames(),
            "fixture: a pet walking after a moved caret is on glass and owes frames"
        );
        ws.input_scratch.free_sprites = vec![aterm_core::render::FreeSprite::default(); 2];
        ws.input_scratch.nova_add = vec![aterm_core::render::GlowQuad::default()];
        ws.input_scratch.cursor_glow_add = vec![aterm_core::render::GlowQuad::default()];
    }

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Packages));
    assert!(app.prepare_native_input_scratch(wid));

    let ws = &app.windows[&wid];
    assert!(
        ws.input_scratch.free_sprites.is_empty() && ws.input_scratch.free_atlas.is_none(),
        "a native frame carries no free sprite the terminal left behind \
         ({} survived)",
        ws.input_scratch.free_sprites.len()
    );
    assert!(
        ws.input_scratch.nova_add.is_empty() && ws.input_scratch.cursor_glow_add.is_empty(),
        "…nor the supernova and cursor light beside them"
    );
    assert!(
        !ws.fx.companion().brain().is_active(),
        "…and the brain that would have re-emitted them holds no body or mote"
    );
    assert!(
        !ws.fx.companion().brain().needs_frames(),
        "…nor a frame debt this route can never tick back down"
    );
}

#[test]
fn modal_transform_tracks_visible_route_across_mixed_focus_and_zoom() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (instance, native_view) = app.active_native_view(wid).expect("native view");
    let (_, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    app.windows.get_mut(&wid).unwrap().scale = 2.0;
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    app.palette_enter();

    assert_eq!(
        app.active_visible_content_route(wid),
        Some(crate::VisibleContentRoute::Heterogeneous)
    );
    let mixed = app
        .overlay_coordinate_transform(wid)
        .expect("mixed modal transform");
    assert_eq!(mixed.scale, 2.0);
    assert_eq!(mixed.geom.font_px, 13.0);
    assert_eq!(
        mixed.origin_y,
        app.native_content_origin_y(wid) as f64,
        "terminal focus does not divert a visible mixed card into terminal coordinates"
    );

    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .toggle_zoom()
    );
    assert_eq!(
        app.active_visible_content_route(wid),
        Some(crate::VisibleContentRoute::Terminal { composed: true })
    );
    let zoomed_terminal = app
        .overlay_coordinate_transform(wid)
        .expect("zoomed terminal transform");
    assert_eq!(zoomed_terminal.scale, 1.0);
    assert_eq!(zoomed_terminal.geom.font_px, app.win_font_px(wid));
    assert_eq!(
        zoomed_terminal.origin_y,
        (app.win_pad_top(wid) + app.win_head(wid)) as f64,
        "a hidden native sibling cannot move terminal modal hit geometry"
    );

    assert!(
        !app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .toggle_zoom()
    );
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(native_view)
    );
    app.sync_window(wid);
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .toggle_zoom()
    );
    assert_eq!(
        app.active_visible_content_route(wid),
        Some(crate::VisibleContentRoute::Native {
            instance,
            view: native_view,
        })
    );
    let stale_spliced_rows = app.windows[&wid].input_scratch.cells.len();
    app.windows.get_mut(&wid).unwrap().rows = 4;
    assert!(
        stale_spliced_rows > usize::from(app.windows[&wid].rows),
        "fixture retains a prior tab-strip/content scratch height"
    );
    let zoomed_native = app
        .overlay_coordinate_transform(wid)
        .expect("zoomed native transform");
    assert_eq!(zoomed_native.scale, 2.0);
    assert_eq!(zoomed_native.origin_y, mixed.origin_y);
    assert_eq!(
        zoomed_native.geom.panel_rows,
        usize::from(app.windows[&wid].rows),
        "native modal rows clamp to content height, never a previously spliced strip"
    );
    assert_ne!(native_view, terminal_view);
}
