// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::native_app::{ActionInvocation, AppEvent};
use crate::native_settings::SettingsRoute;
use crate::{App, WindowId};

struct Observed {
    pointer: bool,
    strict: bool,
    pending: bool,
    unchanged: bool,
    unguarded_hit: bool,
}

fn native_case(case: i64) -> Observed {
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    let wid = WindowId(0);
    app.first_present_done = true;
    app.perf_reduced = true;
    app.perf_flip_at = Some(std::time::Instant::now());
    app.perf_shed_dwell = std::time::Duration::MAX;
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    app.windows.get_mut(&wid).unwrap().cols = ((1_000 - 2 * pad) / cw.max(1)).try_into().unwrap();
    app.windows.get_mut(&wid).unwrap().rows = ((600 - 2 * pad) / ch.max(1)).try_into().unwrap();
    assert!(app.open_settings_tab(SettingsRoute::KeyboardInput));
    let (_, view) = app.active_native_view(wid).unwrap();
    app.sync_chrome_fonts();
    app.redraw_window(wid);
    let (stamp, serial, pixels, first, second, navigate) = {
        let artifact = app.retained_native_leaf_artifact(wid, view, true).unwrap();
        let target = |route: SettingsRoute| {
            let key = format!("settings/nav{}", route.path());
            let hit = artifact
                .compiled
                .hits
                .iter()
                .find(|hit| hit.key.as_str() == key)
                .unwrap();
            let x = f64::from(i32::try_from(artifact.device_x).expect("fixture pointer x"))
                + f64::from(hit.rect.x + hit.rect.width / 2.0) * artifact.scale;
            let y = f64::from(i32::try_from(artifact.device_y).expect("fixture pointer y"))
                + f64::from(hit.rect.y + hit.rect.height / 2.0) * artifact.scale;
            ((x, y), hit.action.clone())
        };
        let (first, navigate) = target(SettingsRoute::WindowTabs);
        let (second, _) = target(SettingsRoute::KeyboardInput);
        let raster = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .unwrap();
        (
            raster.stamp,
            raster.paint_serial,
            raster.rgba.clone(),
            first,
            second,
            navigate,
        )
    };
    let change_content = |app: &mut App| {
        app.dispatch_native_event(
            wid,
            AppEvent::Action(ActionInvocation {
                id: navigate.clone(),
                value: None,
            }),
        )
        .unwrap();
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            unreachable!()
        };
        assert_eq!(state.route, SettingsRoute::WindowTabs);
    };
    match case {
        1 => {}
        2 | 3 | 9 => {
            app.on_cursor_moved(wid, first.0, first.1);
            if case == 3 {
                app.on_cursor_moved(wid, second.0, second.1);
            }
            if case == 9 {
                app.redraw_window(wid);
            }
        }
        4 => {
            app.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
            app.invalidate_native_pointer_style(wid, view);
        }
        5 => {
            app.on_cursor_moved(wid, first.0, first.1);
            change_content(&mut app);
        }
        6 => {
            app.windows.get_mut(&wid).unwrap().cols += 1;
        }
        7 => {
            app.palette_enter();
            assert!(app.windows[&wid].overlay.is_some());
        }
        8 => {
            change_content(&mut app);
            let commits = crate::scripted_redraw_fixture::surface(&mut app).commits();
            crate::scripted_redraw_fixture::surface(&mut app)
                .fail_next(crate::present::scripted::ScriptedOutcome::FailCommit);
            app.redraw_window(wid);
            assert_eq!(
                crate::scripted_redraw_fixture::surface(&mut app).commits(),
                commits
            );
        }
        _ => unreachable!(),
    }
    let pointer = app.retained_native_pointer_artifact(wid, view).is_some();
    let strict = app.retained_native_leaf_artifact(wid, view, true).is_some();
    let cache = &app.windows[&wid].leaf_render_cache[&view];
    let raster = cache
        .native
        .as_ref()
        .expect("raster provenance remains available");
    let pending = cache.native_pointer_style_only
        && cache.native_damage.is_some()
        && raster.paint_serial != cache.native_paint_serial;
    let unchanged = raster.stamp == stamp && raster.paint_serial == serial && raster.rgba == pixels;
    let hit = raster.compiled.hits.first().unwrap();
    let unguarded_hit = raster
        .compiled
        .hit_test(
            hit.rect.x + hit.rect.width / 2.0,
            hit.rect.y + hit.rect.height / 2.0,
        )
        .is_some();
    Observed {
        pointer,
        strict,
        pending,
        unchanged,
        unguarded_hit,
    }
}

/// Probe the shipping App and its scripted CPU presenter, not a parallel freshness predicate.
pub(crate) fn pointer_paint_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Read" => {
            let observed = native_case(state["case"]);
            next.insert("phase", 1);
            for (field, value) in [
                ("pointer", observed.pointer),
                ("strict", observed.strict),
                ("pending", observed.pending),
                ("unchanged", observed.unchanged),
            ] {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn presented_pointer_feedback_conforms_without_weakening_generic_input_fences() {
    let model = aterm_models_render::native_pointer_paint_model();
    assert_eq!(model.mutant_dials(), (1..=12).collect::<Vec<_>>());
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let states = aterm_spec::interp::reachable_states(&model);
    assert_eq!(states.len(), 18);
    for state in states {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = pointer_paint_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let observed = native_case(2);
    let mut old = model.init_state();
    old.insert("phase", 1);
    old.insert("case", 2);
    old.insert("pointer", i64::from(observed.strict));
    assert!(observed.pointer && !observed.strict);
    assert!(!model.check_invariant("PointerStyleRetainsInput", &old));
    for case in 4..=8 {
        let observed = native_case(case);
        assert!(!observed.pointer && observed.unguarded_hit);
        let mut unguarded = model.init_state();
        unguarded.insert("phase", 1);
        unguarded.insert("case", case);
        unguarded.insert("pointer", i64::from(observed.unguarded_hit));
        let law = if case == 4 {
            "GenericInvalidationIsNotFeedback"
        } else {
            "ChangedInputIsRejected"
        };
        assert!(!model.check_invariant(law, &unguarded));
    }
}
