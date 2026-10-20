// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn retained_native_raster_bytes(app: &App, wid: WindowId) -> usize {
    app.windows[&wid]
        .leaf_render_cache
        .values()
        .filter_map(|cache| cache.native.as_ref())
        .map(|raster| raster.rgba.capacity())
        .sum()
}

fn about_view(app: &App) -> crate::native_app::AppViewState {
    let mut state = crate::native_settings::SettingsViewState::new(&app.config);
    state.navigate(crate::native_settings::SettingsRoute::About);
    crate::native_app::AppViewState::Settings(Box::new(state))
}

#[test]
fn native_leaf_cache_releases_hidden_tab_rasters_without_repainting_visible_leaf() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (instance, first) = app.active_native_view(wid).unwrap();
    let presentation = app.windows[&wid]
        .tab_set
        .active()
        .unwrap()
        .presentation
        .clone();
    let mut views = vec![first];

    // Paint through the plan entry alone, skipping the plan-binding
    // retirement: every visited live tab keeps its raster.
    for visit in 0..6 {
        if visit != 0 {
            let (_, view) = app
                .install_native_tab(wid, instance, about_view(&app), presentation.clone())
                .unwrap();
            views.push(view);
        }
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        assert!(app.prepare_native_input_scratch_from_plan(wid, &plan));
    }
    assert_eq!(app.windows[&wid].leaf_render_cache.len(), views.len());
    let before = retained_native_raster_bytes(&app, wid);
    let current = *views.last().unwrap();
    let raster = app.windows[&wid].leaf_render_cache[&current]
        .native
        .as_ref()
        .unwrap();
    let current_bytes = raster.rgba.capacity();
    let current_ptr = raster.rgba.as_ptr();
    let compiled_ptr = raster.compiled.paint.as_ptr();
    let full_rasters = raster.full_rasters;
    let compilations = crate::app_native::native_ui_compile_count();
    assert!(before >= current_bytes * 6 && current_bytes > 0);

    // The shipping capture/paint entry prunes that history immediately and
    // preserves the current allocation and compiled scene byte for byte.
    assert!(app.prepare_native_input_scratch(wid));
    assert_eq!(app.windows[&wid].leaf_render_cache.len(), 1);
    assert_eq!(retained_native_raster_bytes(&app, wid), current_bytes);
    let raster = app.windows[&wid].leaf_render_cache[&current]
        .native
        .as_ref()
        .unwrap();
    assert_eq!(raster.rgba.as_ptr(), current_ptr);
    assert_eq!(raster.compiled.paint.as_ptr(), compiled_ptr);
    assert_eq!(raster.full_rasters, full_rasters);
    assert_eq!(crate::app_native::native_ui_compile_count(), compilations);
    eprintln!(
        "native leaf cache: six visited tabs retained {before} RGBA bytes before, {current_bytes} after"
    );

    // Further ordinary visits remain bounded; tabs and semantic view state
    // stay live and can repaint on return. A terminal capture releases all.
    for index in 1..=views.len() {
        app.switch_tab_in(wid, index);
        assert!(app.prepare_native_input_scratch(wid));
        assert_eq!(app.windows[&wid].leaf_render_cache.len(), 1);
        assert!(
            views
                .iter()
                .all(|view| app.native_runtime.view_state(*view).is_some())
        );
    }
    app.switch_tab_in(wid, 0);
    assert!(app.prepare_terminal_capture_grid(wid).is_some());
    assert!(app.windows[&wid].leaf_render_cache.is_empty());
    assert_eq!(retained_native_raster_bytes(&app, wid), 0);
    assert_eq!(app.windows[&wid].tab_set.len(), views.len() + 1);
}

fn leaf_cache_projection(
    app: &App,
    wid: WindowId,
    first: crate::tab_model::ViewId,
    second: crate::tab_model::ViewId,
) -> aterm_spec::interp::State {
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let cached = |view| {
        app.windows[&wid]
            .leaf_render_cache
            .get(&view)
            .is_some_and(|cache| cache.native.is_some())
    };
    [
        ("first_visible", i64::from(plan.leaf(first).is_some())),
        ("second_visible", i64::from(plan.leaf(second).is_some())),
        ("first_cached", i64::from(cached(first))),
        ("second_cached", i64::from(cached(second))),
    ]
    .into_iter()
    .collect()
}

#[test]
fn native_leaf_cache_lifetime_conforms_across_split_zoom_tab_and_capture() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (instance, first) = app.active_native_view(wid).unwrap();
    let second = app
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            instance,
            about_view(&app),
        )
        .unwrap();
    let (_, terminal) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    let model = aterm_models_render::native_leaf_cache_lifetime_model();
    let mut expected = model.init_state();

    for action in [
        "BindSplit",
        "Paint",
        "BindSplit",
        "Paint",
        "BindFirst",
        "Paint",
        "BindSecond",
        "Paint",
        "BindSplit",
        "Paint",
        "BindTerminal",
        "Paint",
        "BindFirst",
        "Paint",
        "BindTerminal",
        "BindSplit",
        "Paint",
    ] {
        if action == "Paint" {
            match app.active_visible_content_route(wid).unwrap() {
                crate::VisibleContentRoute::Native { .. } => {
                    assert!(app.prepare_native_input_scratch(wid));
                }
                crate::VisibleContentRoute::Heterogeneous => {
                    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
                    assert!(app.windows[&wid].leaf_render_cache.contains_key(&terminal));
                }
                crate::VisibleContentRoute::Terminal { .. } => {
                    assert!(app.prepare_terminal_capture_grid(wid).is_some());
                }
            }
        } else {
            app.switch_tab_in(wid, usize::from(action != "BindTerminal"));
            if action != "BindTerminal" {
                let tab = app
                    .windows
                    .get_mut(&wid)
                    .unwrap()
                    .tab_set
                    .active_mut()
                    .unwrap();
                tab.focus = if action == "BindSecond" {
                    second
                } else {
                    first
                };
                tab.zoomed = action != "BindSplit";
                app.resync_active_or_window(wid);
            }
            let (plan, route) = app.active_visible_frame_layout(wid).unwrap();
            let retained: Vec<_> = app.windows[&wid]
                .leaf_render_cache
                .iter()
                .filter(|(view, _)| plan.leaf(**view).is_some())
                .filter_map(|(view, cache)| {
                    cache
                        .native
                        .as_ref()
                        .map(|raster| (*view, raster.rgba.as_ptr()))
                })
                .collect();
            let old = leaf_cache_projection(&app, wid, first, second);
            if (action == "BindFirst" && old["second_cached"] != 0)
                || (action == "BindTerminal" && old["first_cached"] + old["second_cached"] != 0)
            {
                assert!(
                    !model.check_invariant("OnlyVisibleLeavesRetainRasters", &old),
                    "negative control: skipping {action}'s binding retains a real hidden raster"
                );
            }
            app.prepare_layout_coordinate_space_from_plan(wid, route, &plan);
            for (view, ptr) in retained {
                assert_eq!(
                    app.windows[&wid].leaf_render_cache[&view]
                        .native
                        .as_ref()
                        .unwrap()
                        .rgba
                        .as_ptr(),
                    ptr
                );
            }
        }
        assert!(model.fire(action, &mut expected));
        assert_eq!(
            leaf_cache_projection(&app, wid, first, second),
            expected,
            "{action}"
        );
        assert!(model.check_invariant("OnlyVisibleLeavesRetainRasters", &expected));
    }
}

#[test]
fn native_route_card_composes_global_priority_and_modal_suppression() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let host = |rgba: [u8; 4], fp: u64| crate::SettingsCard {
        scrim: None,
        rgba: rgba.to_vec(),
        pw: 1,
        ph: 1,
        dx: 1,
        dy: 0,
        fp,
        geom: fp,
    };
    {
        let window = app.windows.get_mut(&wid).expect("window");
        window.settings_card = Some(crate::SettingsCard {
            scrim: None,
            rgba: vec![0, 0, 0, 255, 0, 0, 0, 255],
            pw: 2,
            ph: 1,
            dx: 0,
            dy: 0,
            fp: 1,
            geom: 1,
        });
        window.badge_card = Some(host([0, 0, 255, 255], 2));
    }

    assert!(app.compose_native_route_card(wid));
    assert_eq!(
        &app.windows[&wid]
            .route_card
            .as_ref()
            .expect("combined route card")
            .rgba[4..8],
        &[0, 0, 255, 255],
        "the badge stays visible over the retained native base"
    );

    app.palette_enter();
    assert!(app.windows[&wid].overlay.is_some());
    assert!(app.compose_native_route_card(wid));
    assert!(
        app.windows[&wid].route_card.is_none(),
        "a modal suppresses global cards and uses the modal-lowered native base"
    );
}

fn assert_bytes_outside_region_unchanged(
    before: &[u8],
    after: &[u8],
    width: u32,
    height: u32,
    region: crate::tray_raster::RasterRect,
) {
    for y in 0..height {
        for x in 0..width {
            if x >= region.x
                && x < region.x + region.width
                && y >= region.y
                && y < region.y + region.height
            {
                continue;
            }
            let index = ((y * width + x) * 4) as usize;
            assert_eq!(
                &after[index..index + 4],
                &before[index..index + 4],
                "regional repaint changed device pixel ({x}, {y}) outside its tile"
            );
        }
    }
}

fn paint(
    key: &str,
    rect: crate::native_ui::LogicalRect,
    text: &str,
) -> crate::native_ui::PaintNode {
    crate::native_ui::PaintNode {
        key: crate::native_ui::UiKey::new(key),
        rect,
        clip: rect,
        content: crate::native_ui::UiContent::Text(crate::native_ui::TextSpec::text(text)),
        peek: false,
        fade: None,
    }
}

#[test]
fn compiler_diff_narrows_unknown_damage_to_changed_paint_bounds() {
    let bounds = crate::native_ui::LogicalRect::new(0.0, 0.0, 800.0, 600.0);
    let unchanged = paint(
        "unchanged",
        crate::native_ui::LogicalRect::new(24.0, 24.0, 160.0, 32.0),
        "Stable",
    );
    let changed_rect = crate::native_ui::LogicalRect::new(212.25, 91.5, 180.5, 44.25);
    let before = crate::native_ui::CompiledUi {
        bounds,
        paint: vec![unchanged.clone(), paint("changed", changed_rect, "Before")],
        ..crate::native_ui::CompiledUi::default()
    };
    let after = crate::native_ui::CompiledUi {
        bounds,
        paint: vec![unchanged, paint("changed", changed_rect, "After")],
        ..crate::native_ui::CompiledUi::default()
    };
    assert_eq!(
        compiled_native_paint_damage(&before, &after),
        Some(crate::native_app::DamageRegion::Rect {
            x: 212,
            y: 91,
            width: 181,
            height: 45,
        })
    );
    assert_eq!(
        union_native_render_damage(
            crate::native_app::DamageRegion::All,
            compiled_native_paint_damage(&before, &after).unwrap(),
        ),
        crate::native_app::DamageRegion::All,
        "a reducer's full semantic repaint must never become a transparent tile patch"
    );
    assert_eq!(
        union_native_render_damage(
            crate::native_app::DamageRegion::Rect {
                x: 220,
                y: 100,
                width: 8,
                height: 9,
            },
            compiled_native_paint_damage(&before, &after).unwrap(),
        ),
        crate::native_app::DamageRegion::Rect {
            x: 212,
            y: 91,
            width: 181,
            height: 45,
        },
        "an explicitly local repaint still uses the compiler-derived tile"
    );
}

#[test]
fn desktop_settings_route_repaint_keeps_full_damage_authoritative() {
    let bounds = crate::native_ui::LogicalRect::new(0.0, 0.0, 1_200.0, 810.0);
    let stable_sidebar = paint(
        "settings/navigation",
        crate::native_ui::LogicalRect::new(0.0, 0.0, 216.0, 810.0),
        "Settings navigation",
    );
    let route_rect = crate::native_ui::LogicalRect::new(216.0, 0.0, 984.0, 810.0);
    let before = crate::native_ui::CompiledUi {
        bounds,
        paint: vec![
            stable_sidebar.clone(),
            paint("settings/page", route_rect, "Top Settings"),
        ],
        ..crate::native_ui::CompiledUi::default()
    };
    let after = crate::native_ui::CompiledUi {
        bounds,
        paint: vec![
            stable_sidebar,
            paint("settings/page", route_rect, "Packages"),
        ],
        ..crate::native_ui::CompiledUi::default()
    };
    let derived = compiled_native_paint_damage(&before, &after).unwrap();
    assert!(matches!(
        derived,
        crate::native_app::DamageRegion::Rect { .. }
    ));
    assert_eq!(
        union_native_render_damage(crate::native_app::DamageRegion::All, derived),
        crate::native_app::DamageRegion::All,
        "Top -> Packages at the shipped desktop viewport must repaint the retained page, including its sidebar"
    );

    // The same through the real Settings reducer and retained raster: the
    // route action asks for a full repaint and the next preparation does one.
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    assert!(app.prepare_native_input_scratch(wid));
    let full_rasters = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("Top Settings retained raster")
        .full_rasters;

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Packages));
    assert!(app.prepare_native_input_scratch(wid));
    let packages = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("Packages retained raster");
    assert!(matches!(
        packages.last_work,
        crate::NativeRasterWork::Full { .. }
    ));
    assert_eq!(packages.full_rasters, full_rasters + 1);
}

#[test]
fn compiler_diff_promotes_reflow_to_full_damage() {
    let bounds = crate::native_ui::LogicalRect::new(0.0, 0.0, 400.0, 300.0);
    let before = crate::native_ui::CompiledUi {
        bounds,
        paint: vec![paint(
            "moving",
            crate::native_ui::LogicalRect::new(10.0, 20.0, 40.0, 30.0),
            "Move",
        )],
        ..crate::native_ui::CompiledUi::default()
    };
    let after = crate::native_ui::CompiledUi {
        bounds,
        paint: vec![paint(
            "moving",
            crate::native_ui::LogicalRect::new(110.0, 80.0, 40.0, 30.0),
            "Move",
        )],
        ..crate::native_ui::CompiledUi::default()
    };
    assert_eq!(
        compiled_native_paint_damage(&before, &after),
        Some(crate::native_app::DamageRegion::All)
    );
}

#[test]
fn single_native_retains_and_patches_one_device_tile() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    assert!(app.prepare_native_input_scratch(wid));

    let initial = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("ordinary native tab has one retained leaf raster");
    let allocation = initial.rgba.as_ptr() as usize;
    let before = initial.rgba.clone();
    let full_rasters = initial.full_rasters;
    let regional_rasters = initial.regional_rasters;
    app.invalidate_native_view_cache(
        wid,
        view,
        crate::native_app::DamageRegion::Rect {
            x: 7,
            y: 11,
            width: 19,
            height: 23,
        },
    );

    assert!(app.prepare_native_input_scratch(wid));
    let patched = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("retained raster after local damage");
    assert_eq!(
        patched.rgba.as_ptr() as usize,
        allocation,
        "single-native regional work must patch the retained allocation"
    );
    let crate::NativeRasterWork::Region { rect, pixels } = patched.last_work else {
        panic!("single-native local damage widened to a full raster")
    };
    assert_eq!(pixels, rect.pixels());
    assert!(pixels < u64::from(patched.width) * u64::from(patched.height));
    assert_eq!(patched.full_rasters, full_rasters);
    assert_eq!(patched.regional_rasters, regional_rasters + 1);
    assert_eq!(
        app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("presented native card after regional work")
            .rgba,
        patched.rgba,
        "the settings_card presentation mirrors the newly patched retained leaf"
    );
    assert_bytes_outside_region_unchanged(
        &before,
        &patched.rgba,
        patched.width,
        patched.height,
        rect,
    );

    let full_before_theme = patched.full_rasters;
    let regional_before_theme = patched.regional_rasters;
    app.theme.fg ^= 0x0001_0101;
    app.invalidate_native_view_cache(
        wid,
        view,
        crate::native_app::DamageRegion::Rect {
            x: 7,
            y: 11,
            width: 19,
            height: 23,
        },
    );
    assert!(app.prepare_native_input_scratch(wid));
    let themed = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("theme-changed raster");
    assert!(matches!(
        themed.last_work,
        crate::NativeRasterWork::Full { .. }
    ));
    assert_eq!(themed.full_rasters, full_before_theme + 1);
    assert_eq!(themed.regional_rasters, regional_before_theme);

    let full_before_geometry = themed.full_rasters;
    let regional_before_geometry = themed.regional_rasters;
    app.windows.get_mut(&wid).unwrap().rows += 1;
    app.invalidate_native_view_cache(
        wid,
        view,
        crate::native_app::DamageRegion::Rect {
            x: 7,
            y: 11,
            width: 19,
            height: 23,
        },
    );
    assert!(app.prepare_native_input_scratch(wid));
    let resized = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("geometry-changed raster");
    assert!(matches!(
        resized.last_work,
        crate::NativeRasterWork::Full { .. }
    ));
    assert_eq!(resized.full_rasters, full_before_geometry + 1);
    assert_eq!(resized.regional_rasters, regional_before_geometry);
}

#[test]
fn native_chrome_redraw_reuses_compiled_tree_but_real_damage_rebuilds() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    assert!(app.prepare_native_input_scratch(wid));
    let compiled_count = crate::app_native::native_ui_compile_count();
    let window = &app.windows[&wid];
    let raster = window.leaf_render_cache[&view].native.as_ref().unwrap();
    let leaf_allocation = raster.rgba.as_ptr();
    let compiled_allocation = raster.compiled.paint.as_ptr();
    let pixels = raster.rgba.clone();
    let fingerprint = raster.compiled.fingerprint();
    let full_rasters = raster.full_rasters;
    let observer_allocation = window
        .native_ui_compiled
        .as_ref()
        .unwrap()
        .compiled
        .paint
        .as_ptr();
    assert!(!raster.compiled.paint.is_empty());
    assert!(!raster.compiled.semantics.is_empty());

    for _ in 0..3 {
        app.windows
            .get_mut(&wid)
            .unwrap()
            .native_ui_compiled
            .as_mut()
            .unwrap()
            .phase = crate::app_native::NativeCompiledPhase::Presented;
        assert!(app.prepare_native_input_scratch(wid));
        let window = &app.windows[&wid];
        let raster = window.leaf_render_cache[&view].native.as_ref().unwrap();
        let observer = window.native_ui_compiled.as_ref().unwrap();
        assert_eq!(crate::app_native::native_ui_compile_count(), compiled_count);
        assert_eq!(raster.rgba.as_ptr(), leaf_allocation);
        assert_eq!(raster.compiled.paint.as_ptr(), compiled_allocation);
        assert_eq!(observer.compiled.paint.as_ptr(), observer_allocation);
        assert_eq!(
            observer.phase,
            crate::app_native::NativeCompiledPhase::Staged
        );
        assert_eq!(observer.compiled.fingerprint(), fingerprint);
        assert_eq!(raster.full_rasters, full_rasters);
        assert_eq!(window.settings_card.as_ref().unwrap().rgba, pixels);
        assert!(
            app.retained_native_leaf_artifact(wid, view, false)
                .is_some()
        );
    }

    // A modal changes the presented composite, while the page underneath
    // remains the same retained paint/hit-test/accessibility authority.
    app.palette_enter();
    assert!(app.prepare_native_input_scratch(wid));
    assert_eq!(crate::app_native::native_ui_compile_count(), compiled_count);
    assert_ne!(
        app.windows[&wid].settings_card.as_ref().unwrap().rgba,
        pixels
    );
    app.windows.get_mut(&wid).unwrap().overlay = None;
    assert!(app.prepare_native_input_scratch(wid));
    assert_eq!(
        app.windows[&wid].settings_card.as_ref().unwrap().rgba,
        pixels
    );

    // Negative controls: explicit preview damage and semantic route edits
    // must still enter the genuine compiler and replace retained semantics.
    app.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
    assert!(app.prepare_native_input_scratch(wid));
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        compiled_count + 1
    );
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Packages));
    assert!(app.prepare_native_input_scratch(wid));
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        compiled_count + 2
    );
    assert_ne!(
        app.windows[&wid]
            .native_ui_compiled
            .as_ref()
            .unwrap()
            .compiled
            .fingerprint(),
        fingerprint
    );
}

#[test]
fn settings_preview_tick_patches_only_the_retained_preview_band() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    assert!(app.prepare_native_input_scratch(wid));
    let before = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("retained Cursor & Motion raster");
    let full_rasters = before.full_rasters;
    let regional_rasters = before.regional_rasters;
    let total_pixels = u64::from(before.width) * u64::from(before.height);
    let before_pixels = before.rgba.clone();

    assert!(
        app.invalidate_active_native_settings_preview(wid, 720),
        "the default live cursor preview is ticking"
    );
    assert!(app.prepare_native_input_scratch(wid));
    let ticked = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("retained raster after preview tick");
    let crate::NativeRasterWork::Region { rect, pixels } = ticked.last_work else {
        panic!("a preview tick widened to a full-page raster")
    };
    assert!(pixels < total_pixels);
    assert_eq!(ticked.full_rasters, full_rasters);
    assert_eq!(ticked.regional_rasters, regional_rasters + 1);
    assert_bytes_outside_region_unchanged(
        &before_pixels,
        &ticked.rgba,
        ticked.width,
        ticked.height,
        rect,
    );
}

#[test]
fn ineligible_native_preview_skips_font_polling_then_recording_converges() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().rows = 50;
    let mut renderer = aterm_render::Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .unwrap();
    renderer.set_runtime_font_discovery(false);
    let font_job = crate::tray_raster::install_pending_chrome_fonts_for_test(renderer);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
    let (_, view) = app.active_native_view(wid).unwrap();
    assert!(app.prepare_native_input_scratch(wid));
    let candidate = app
        .prepare_native_semantic_font(wid, view, 0)
        .unwrap()
        .candidate;
    let before = crate::tray_raster::semantic_font_snapshot_for_test(&candidate);
    assert!(before.pending);
    font_job.complete();

    // A queued completion makes polling observable: a sampler that ran here
    // would install it though this park cannot use the result. Focused and
    // unfocused unrecorded headless views skip the sampler, as does an
    // overlay-covered recording.
    for focused in [true, false] {
        app.windows.get_mut(&wid).unwrap().focused = focused;
        for phase_ms in [33, 66, 99] {
            assert_eq!(
                app.native_settings_preview_for_tick(wid, phase_ms, false),
                None
            );
            assert_eq!(
                crate::tray_raster::semantic_font_snapshot_for_test(&candidate),
                before
            );
        }
    }
    app.palette_enter();
    assert!(app.windows[&wid].overlay.is_some());
    assert_eq!(app.native_settings_preview_for_tick(wid, 132, true), None);
    assert_eq!(
        crate::tray_raster::semantic_font_snapshot_for_test(&candidate),
        before
    );
    app.windows.get_mut(&wid).unwrap().overlay = None;

    // The recording exception still observes completion on an unfocused
    // headless window and keeps its final ready repaint alive.
    assert_eq!(
        app.native_settings_preview_for_tick(wid, 165, true),
        Some((view, crate::settings_preview::PreviewAnimation::Continuous))
    );
    let ready = crate::tray_raster::semantic_font_snapshot_for_test(&candidate);
    assert!(!ready.pending);
    assert_ne!(ready.ready_epoch, before.ready_epoch);
    assert!(app.invalidate_active_native_settings_preview(wid, 165));
    assert!(app.prepare_native_input_scratch(wid));
    assert_eq!(app.native_settings_preview_for_tick(wid, 198, true), None);
}

fn project_native_preview_font_convergence(
    app: &App,
    wid: WindowId,
    view: crate::tab_model::ViewId,
    initial_epoch: u64,
    mut observed: aterm_spec::interp::State,
    decision: Option<bool>,
) -> aterm_spec::interp::State {
    let cache = &app.windows[&wid].leaf_render_cache[&view];
    let retained = cache
        .native
        .as_ref()
        .unwrap()
        .compiled
        .paint
        .iter()
        .find_map(|paint| match &paint.content {
            crate::native_ui::UiContent::SettingsPreview(spec) => Some(spec),
            _ => None,
        })
        .expect("retained Typography preview");
    // Observe one candidate's completion cycle without polling its worker.
    // Superseded results belong to the separate prewarm handshake model.
    let current = crate::tray_raster::semantic_font_snapshot_for_test(&retained.font_candidate);
    observed.insert("pending", i64::from(current.pending));
    observed.insert("ready", i64::from(current.ready_epoch != initial_epoch));
    observed.insert(
        "retained",
        i64::from(retained.font_ready_epoch != initial_epoch),
    );
    observed.insert("dirty", i64::from(cache.native_damage.is_some()));
    if let Some(decision) = decision {
        observed.insert("decision", i64::from(decision));
    }
    observed
}

/// Run `drive` and assert it entered `action`'s shipping refinement anchor.
/// Entry alone does not tell a transition from a no-op; the caller validates
/// the state change.
fn drive_native_preview_action<T>(action: &str, drive: impl FnOnce() -> T) -> T {
    use aterm_spec::xref;

    assert!(xref::reset_entered_anchors());
    let out = drive();
    let entered = xref::entered_anchor_ids();
    xref::disarm_entered_anchors();
    let anchor = xref::refinements()
        .find(|anchor| anchor.machine == "NativePreviewFontConvergence" && anchor.action == action)
        .expect("every preview action is anchored to its shipping site");
    assert!(
        entered.contains(anchor.entry_id),
        "{action} did not enter its shipping refinement: {}",
        anchor.rust_method
    );
    out
}

#[test]
fn static_native_preview_paints_the_completed_font_before_disarming() {
    use aterm_spec::interp::State;

    let model = aterm_models_misc::native_preview_font_convergence_model();
    let preview = |app: &App, wid, view| {
        app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .unwrap()
            .compiled
            .paint
            .iter()
            .find_map(|paint| match &paint.content {
                crate::native_ui::UiContent::SettingsPreview(spec) => Some(spec.clone()),
                _ => None,
            })
            .expect("visible Typography preview")
    };
    // Completion may first be observed while arming the next wake, or by
    // the already-due deadline itself. Both must retain the final repaint.
    for (poll_before_due, serious) in [(true, false), (false, false), (true, true), (false, true)] {
        let mut app = App::headless_for_test();
        app.set_serious_mode(serious);
        let wid = WindowId(0);
        app.windows.get_mut(&wid).unwrap().rows = 50;
        let mut renderer = aterm_render::Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .unwrap();
        renderer.set_runtime_font_discovery(false);
        let font_job = crate::tray_raster::install_pending_chrome_fonts_for_test(renderer);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
        let (_, view) = app.active_native_view(wid).unwrap();
        assert!(app.prepare_native_input_scratch(wid));
        let before = preview(&app, wid, view);
        assert!(before.prepared_font.snapshot.pending);
        assert_eq!(
            before.animation(),
            crate::settings_preview::PreviewAnimation::None
        );
        let initial_pixels = app.windows[&wid]
            .settings_card
            .as_ref()
            .unwrap()
            .rgba
            .clone();
        let initial_compiles = crate::app_native::native_ui_compile_count();
        let initial_decision = drive_native_preview_action("Decide", || {
            app.active_native_settings_preview(wid, 0).is_some()
        });
        assert!(initial_decision);

        let bind = |action: &str, previous: &State, app: &App, decision: Option<bool>| {
            let observed = project_native_preview_font_convergence(
                app,
                wid,
                view,
                before.font_ready_epoch,
                model.successors(action, previous)[0].clone(),
                decision,
            );
            let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                previous,
                &observed,
                Some(action),
                "real Settings font request, completion and retained repaint",
            );
            assert!(ok, "font convergence {action}: {why}");
            // Entry alone is insufficient: omitting each real state
            // change must fail the same transition validation.
            let missing_fields: &[&str] = match action {
                "Finish" => &["pending", "ready"],
                "Invalidate" => &["dirty"],
                "Paint" => &["retained", "dirty"],
                _ => &[],
            };
            if !missing_fields.is_empty() {
                let mut missing = observed.clone();
                for &field in missing_fields {
                    missing.insert(field, previous[field]);
                }
                let (ok, _) = aterm_spec::verify::validate_transition_tiered(
                    &model,
                    &[],
                    previous,
                    &missing,
                    Some(action),
                    "negative control: entered site without its state change",
                );
                assert!(!ok, "{action} must change the real projected state");
            }
            observed
        };
        let requested = bind("Decide", &model.init_state(), &app, Some(initial_decision));
        font_job.complete();

        // A queued worker result is not installed yet. Projection must
        // remain observational and cannot supply the missing Finish step.
        assert_eq!(
            project_native_preview_font_convergence(
                &app,
                wid,
                view,
                before.font_ready_epoch,
                requested.clone(),
                None,
            ),
            requested
        );

        // Isolate the shipping request/poll seam before the arm predicate
        // so completion and cadence each have their own observed transition.
        if poll_before_due {
            drive_native_preview_action("Finish", || {
                app.prepare_native_semantic_font(wid, view, 33).unwrap()
            });
            let ready = bind("Finish", &requested, &app, None);
            let decision = drive_native_preview_action("Decide", || {
                app.active_native_settings_preview(wid, 33).is_some()
            });
            let armed = bind("Decide", &ready, &app, Some(decision));
            let mut abandoned = armed.clone();
            abandoned.insert("decision", 0);
            let (ok, _) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                &ready,
                &abandoned,
                Some("Decide"),
                "negative control: pending-only cadence loses the ready frame",
            );
            assert!(
                !ok,
                "the historical pending-only predicate must be rejected"
            );
            assert!(drive_native_preview_action("Invalidate", || {
                app.invalidate_active_native_settings_preview(wid, 33)
            }));
            let dirty = bind("Invalidate", &armed, &app, None);
            assert!(drive_native_preview_action("Paint", || {
                app.prepare_native_input_scratch(wid)
            }));
            let painted = bind("Paint", &dirty, &app, None);
            let decision = drive_native_preview_action("Decide", || {
                app.active_native_settings_preview(wid, 66).is_some()
            });
            bind("Decide", &painted, &app, Some(decision));
        } else {
            assert!(
                app.invalidate_active_native_settings_preview(wid, 33),
                "the due deadline must also install and repaint its completed result"
            );
            assert!(app.prepare_native_input_scratch(wid));
        }

        let after = preview(&app, wid, view);
        assert!(!after.prepared_font.snapshot.pending);
        assert_ne!(after.font_ready_epoch, before.font_ready_epoch);
        assert_ne!(after.font_status, before.font_status);
        assert_ne!(
            app.windows[&wid].settings_card.as_ref().unwrap().rgba,
            initial_pixels,
            "the loading-to-ready change must reach the presented pixels"
        );
        assert_eq!(
            crate::app_native::native_ui_compile_count(),
            initial_compiles + 1
        );
        assert!(
            app.active_native_settings_preview(wid, 66).is_none(),
            "the ready static page returns to idle after its final repaint"
        );
        assert!(!app.invalidate_active_native_settings_preview(wid, 99));
        assert!(app.prepare_native_input_scratch(wid));
        assert_eq!(
            crate::app_native::native_ui_compile_count(),
            initial_compiles + 1
        );
        if serious {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
            assert!(app.prepare_native_input_scratch(wid));
            assert!(
                app.active_native_settings_preview(wid, 132).is_none(),
                "font convergence must not enable cursor animation in Serious Mode"
            );
        }
    }
}

#[test]
fn appearance_paint_identity_forces_full_native_raster() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (instance, view) = app.active_native_view(wid).expect("native Settings view");
    assert!(app.prepare_native_input_scratch(wid));
    let mut cache = app
        .windows
        .get_mut(&wid)
        .unwrap()
        .leaf_render_cache
        .remove(&view)
        .unwrap();
    let retained = cache.native.as_ref().expect("initial retained raster");
    let previous_stamp = retained.stamp;
    let compiled = retained.compiled.clone();
    let width = retained.width;
    let height = retained.height;
    let full_rasters = retained.full_rasters;
    let regional_rasters = retained.regional_rasters;
    let appearance_stamp = crate::app_native::NativeUiCompileStamp {
        // Native appearance preferences are folded into paint_revision.
        // Vary that identity directly so this test remains isolated from the
        // process-global platform preference observer used by parallel tests.
        paint_revision: previous_stamp.paint_revision.wrapping_add(1),
        ..previous_stamp
    };
    let viewport = app.native_ui_viewport(wid).unwrap();

    retain_native_leaf_raster(
        &mut cache,
        crate::app_native::NativeLeafScene {
            stamp: appearance_stamp,
            instance,
            view,
            viewport,
            damage: crate::native_app::DamageRegion::Rect {
                x: 7,
                y: 11,
                width: 19,
                height: 23,
            },
            compiled,
        },
        width,
        height,
        1.0,
        app.chrome_palette_theme().into(),
        app.theme,
    );
    let raster = cache.native.as_ref().unwrap();
    assert!(matches!(
        raster.last_work,
        crate::NativeRasterWork::Full { .. }
    ));
    assert_eq!(raster.full_rasters, full_rasters + 1);
    assert_eq!(raster.regional_rasters, regional_rasters);
}

#[test]
fn heterogeneous_modal_changes_fingerprint_and_is_painted_last() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native_view) = app.active_native_view(wid).expect("native Settings view");
    let (_, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let plan = app
        .active_visible_leaf_plan(wid)
        .expect("mixed visible plan");
    let native_leaf = plan.leaf(native_view).expect("native leaf").clone();
    let terminal_leaf = plan.leaf(terminal_view).expect("terminal leaf").clone();
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let base = app.windows[&wid]
        .settings_card
        .as_ref()
        .expect("mixed native tray");
    let base_fp = base.fp;
    let base_rgba = base.rgba.clone();
    let (width, height) = (base.pw, base.ph);

    app.palette_enter();
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let combined = app.windows[&wid]
        .settings_card
        .as_ref()
        .expect("mixed native + modal tray");
    assert_ne!(
        combined.fp, base_fp,
        "modal fingerprint joins the cache key"
    );
    assert_ne!(combined.rgba, base_rgba, "modal changes captured pixels");

    let (cw, ch) = app.win_cell_size(wid);
    let scale = app.windows[&wid].scale.max(f64::EPSILON) as f32;
    let mut modal_prims = Vec::new();
    assert!(app.append_native_modal_prims(wid, &mut modal_prims, 0.0, None));
    let modal =
        crate::tray_raster::rasterize_tray_pixels(&modal_prims, width, height, scale, [0, 0, 0, 0]);
    let device_rect = |leaf: crate::tab_model::VisibleLeaf| {
        (
            (leaf.rect.origin.x * cw as f32).round() as u32,
            (leaf.rect.origin.y * ch as f32).round() as u32,
            (leaf.rect.size.width * cw as f32).round() as u32,
            (leaf.rect.size.height * ch as f32).round() as u32,
        )
    };
    let find_modal_pixel = |leaf: crate::tab_model::VisibleLeaf, opaque: bool| {
        let (x, y, w, h) = device_rect(leaf);
        (y..y.saturating_add(h).min(height)).find_map(|py| {
            (x..x.saturating_add(w).min(width)).find_map(|px| {
                let index = ((py * width + px) * 4) as usize;
                let alpha = modal[index + 3];
                (alpha > 0
                    && (!opaque || alpha == u8::MAX)
                    && modal[index..index + 4] != base_rgba[index..index + 4])
                    .then_some(index)
            })
        })
    };
    let terminal_pixel = find_modal_pixel(terminal_leaf, false)
        .expect("palette paints over the terminal lane as well as native leaves");
    assert_eq!(
        &combined.rgba[terminal_pixel..terminal_pixel + 4],
        &modal[terminal_pixel..terminal_pixel + 4],
        "transparent terminal lane receives the modal raster"
    );
    let native_pixel = find_modal_pixel(native_leaf, true)
        .expect("an opaque palette pixel overlaps the native leaf");
    assert_eq!(
        &combined.rgba[native_pixel..native_pixel + 4],
        &modal[native_pixel..native_pixel + 4],
        "opaque modal pixels replace native pixels, proving last-paint order"
    );
}

/// A mixed-route bail after a leaf rastered drops the reusable card: a
/// regional patch moves neither `fp` nor `geom` and spends its `native_damage`
/// at once, so a card kept after a later leaf bails would serve pre-patch
/// pixels until unrelated input moved.
#[test]
fn heterogeneous_bail_after_a_leaf_rasters_drops_the_reusable_card() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native_view) = app.active_native_view(wid).expect("native Settings view");
    let (session, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let order: Vec<_> = app
        .active_visible_leaf_plan(wid)
        .expect("mixed visible plan")
        .leaves
        .iter()
        .map(|leaf| leaf.view)
        .collect();
    assert_eq!(
        order,
        vec![native_view, terminal_view],
        "the native leaf must compose BEFORE the terminal one, or the bail \
         precedes every raster and this regression cannot be reproduced"
    );

    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let resident = app.windows[&wid]
        .settings_card
        .as_ref()
        .expect("mixed native tray");
    let (fp, geom) = (resident.fp, resident.geom);
    // Stand-in for the pre-patch pixels: bytes no rebuilt composite contains,
    // so a reused card shows.
    app.windows
        .get_mut(&wid)
        .unwrap()
        .settings_card
        .as_mut()
        .unwrap()
        .rgba
        .fill(0xFF);

    // A regional patch of an otherwise identical stamp — the one shape whose
    // new pixels `fp` cannot see — followed by a sibling that transiently
    // disappears from the pool. Its view still resolves, so the route stays
    // heterogeneous and the loop reaches (and bails on) the missing session.
    app.invalidate_native_view_cache(
        wid,
        native_view,
        crate::native_app::DamageRegion::Rect {
            x: 7,
            y: 11,
            width: 19,
            height: 23,
        },
    );
    let pooled = app
        .pool
        .sessions
        .remove(&session)
        .expect("stub session is pooled");
    assert!(
        app.prepare_heterogeneous_input_scratch(wid).is_none(),
        "a missing session abandons the mixed frame"
    );
    let patched = app.windows[&wid].leaf_render_cache[&native_view]
        .native
        .as_ref()
        .expect("retained native leaf raster");
    assert!(
        matches!(patched.last_work, crate::NativeRasterWork::Region { .. }),
        "the native leaf really did re-raster before the sibling bailed"
    );
    assert!(
        app.windows[&wid].settings_card.is_none(),
        "a bail after a leaf rastered must leave no card for the reuse gate to serve"
    );

    app.pool.sessions.insert(session, pooled);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let rebuilt = app.windows[&wid]
        .settings_card
        .as_ref()
        .expect("the abandoned composite is rebuilt on the next frame");
    assert_eq!(
        (rebuilt.fp, rebuilt.geom),
        (fp, geom),
        "the patch moved neither gate input — this is exactly the frame the \
         reuse gate would have served from the resident card"
    );
    assert!(
        rebuilt.rgba.iter().any(|&byte| byte != 0xFF),
        "the abandoned frame's composite must not survive into the next one"
    );
}

/// A mixed native/terminal split has one focused metadata authority. OSC 12
/// and absolute-row anchors follow a focused terminal's exact extracted
/// snapshot, while focusing the native sibling returns those terminal-only
/// fields to neutral instead of retaining a stale prior frame.
#[test]
fn heterogeneous_focus_projects_terminal_cursor_color_and_row_metadata() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native_view) = app.active_native_view(wid).expect("native Settings view");
    let (session, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let terminal = app.pool.get(session).expect("stub terminal").term.clone();
    {
        let mut terminal = crate::term_lock(&terminal);
        terminal.process(b"\x1b]12;#123456\x07");
        for _ in 0..100 {
            terminal.process(b"scroll\r\n");
        }
    }
    {
        let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
        input.cursor_color = 0x00AB_CDEF;
        input.base_y = i64::MAX;
        input.absolute_row_revision = u64::MAX;
        input.history_renumber_epoch = u64::MAX;
    }

    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let window = &app.windows[&wid];
    let pane = &window.leaf_render_cache[&terminal_view].input;
    assert_eq!(window.input_scratch.cursor_color, 0x0012_3456);
    assert_eq!(window.input_scratch.base_y, pane.base_y);
    assert_eq!(
        window.input_scratch.absolute_row_revision,
        pane.absolute_row_revision
    );
    // Both row-identity stamps travel together: a width reflow renumbers
    // history without touching the footer revision, so a projection that
    // carried only the revision would hand the find overlay a fresh-looking
    // key for a frame whose rows moved.
    assert_eq!(
        window.input_scratch.history_renumber_epoch,
        pane.history_renumber_epoch
    );
    assert!(window.input_scratch.base_y > 0, "fixture scrolled");

    {
        let mut terminal = crate::term_lock(&terminal);
        terminal.process(b"\x1b]21;cursor=\x07\x1b]10;#654321\x07");
        assert_eq!(terminal.cursor_color(), None);
    }
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    assert_eq!(
        app.windows[&wid].input_scratch.cursor_color, 0x0065_4321,
        "a focused terminal in a heterogeneous split resolves its dynamic cursor from OSC 10"
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
    {
        let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
        input.cursor_color = 0x00AB_CDEF;
        input.base_y = 99;
        input.absolute_row_revision = 99;
        input.history_renumber_epoch = 99;
    }
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let input = &app.windows[&wid].input_scratch;
    assert_eq!(input.cursor_color, aterm_core::render::COLOR_UNSET);
    assert_eq!(
        (
            input.base_y,
            input.absolute_row_revision,
            input.history_renumber_epoch
        ),
        (0, 0, 0)
    );
    assert!(
        !input.cursor_visible,
        "an unfocused terminal cannot leak its cursor into a native-focused frame"
    );
}

/// A KEY NEVER COMPILES A PAGE THE WINDOW ALREADY COMPILED: with a long
/// Search result list warm on glass and the keyboard on one of its controls,
/// arrows and page keys reach the page without entering the semantic
/// compiler (they used to compile the whole page, every result row, per key,
/// just to ask whether a text field had the keyboard); the next redraw
/// compiles once. Return on a focused control after a warm redraw reads the
/// retained tree and fires its action with no compile of its own.
#[test]
fn keys_reuse_the_retained_compile_instead_of_compiling_the_page() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    let press = |key| crate::input::InputEvent::Key {
        key: Key::Named(key),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Terminal));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    let focus_control = |app: &mut App, key: &str| {
        app.dispatch_native_event(
            wid,
            crate::native_app::AppEvent::FocusChanged(Some(crate::native_ui::UiKey::new(key))),
        )
        .unwrap();
    };
    focus_control(&mut app, "settings/search");
    app.dispatch_native_event(
        wid,
        crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
            "e".to_string(),
        )),
    )
    .unwrap();
    assert!(app.prepare_native_input_scratch(wid));
    let first = app
        .cached_native_ui(wid)
        .expect("the redraw retained its compile")
        .compiled
        .semantics
        .iter()
        .find(|node| {
            (node.key.as_str().starts_with("settings/control/")
                || node.key.as_str().starts_with("settings/search/page/"))
                && node.role != crate::native_ui::SemanticRole::TextField
        })
        .map(|node| node.key.clone())
        .unwrap_or_else(|| {
            panic!(
                "a control among the results: {:?}",
                app.cached_native_ui(wid)
                    .unwrap()
                    .compiled
                    .semantics
                    .iter()
                    .map(|node| node.key.as_str())
                    .collect::<Vec<_>>()
            )
        });
    focus_control(&mut app, first.as_str());
    assert!(app.prepare_native_input_scratch(wid));
    let warm = crate::app_native::native_ui_compile_count();
    for _ in 0..10 {
        assert!(app.native_input_event(wid, &press(NamedKey::ArrowDown)));
    }
    for _ in 0..3 {
        assert!(app.native_input_event(wid, &press(NamedKey::PageDown)));
    }
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        warm,
        "keys compile nothing"
    );
    assert!(app.prepare_native_input_scratch(wid));
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        warm + 1,
        "the redraw compiles once"
    );
    // Return on the focused control reads the retained tree.
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("Settings view");
    };
    let focused = state
        .common
        .last_focus
        .clone()
        .expect("the keyboard is in the list");
    assert!(app.cached_native_ui(wid).is_some());
    let before = crate::app_native::native_ui_compile_count();
    assert!(app.native_input_event(wid, &press(NamedKey::Enter)));
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        before,
        "Return on {} compiled the page",
        focused.as_str()
    );
}

/// THE DEMO'S REST IS DRAWN: a Themes demo that comes to rest asks for no
/// more motion, so the poll that sees it rest still asks for one last edge
/// while the retained raster shows a playing frame, and that repaint draws
/// the settled frame, whatever phase the last input left the tape at.
/// Negative control: once the settled frame is drawn the poll goes idle.
#[test]
fn a_resting_theme_demo_draws_its_settled_frame_once() {
    use crate::settings_theme_demo::{DEMO_LOOP_MS, frame_key};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 160;
        window.rows = 50;
        window.focused = true;
    }
    // The page has been open ten minutes, its tape touched mid-loop.
    let open_for = std::time::Duration::from_secs(600);
    app.lat_epoch = std::time::Instant::now()
        .checked_sub(open_for)
        .expect("a clock ten minutes deep");
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    let set_last_input = |app: &mut App, at: u64| {
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("a Settings view")
        };
        state.showcase.input_pending.set(false);
        state.showcase.last_input_phase_ms.set(at);
        // The tape is mid-loop, away from the frame it opens on.
        state
            .showcase
            .demo_epoch_ms
            .set(Some(at.saturating_sub(DEMO_LOOP_MS / 3)));
    };
    let now = u64::try_from(open_for.as_millis()).unwrap();
    set_last_input(&mut app, now - 3 * DEMO_LOOP_MS + 4_321);
    assert!(app.prepare_native_input_scratch(wid));
    let drawn = |app: &App| {
        app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("retained Themes raster")
            .compiled
            .paint
            .iter()
            .find_map(|paint| match &paint.content {
                crate::native_ui::UiContent::SettingsPreview(spec) => spec.demo_frame(),
                _ => None,
            })
            .expect("the demo is drawn")
    };
    let settled = frame_key(0, false);
    assert_ne!(
        drawn(&app),
        settled,
        "the control: a playing frame is drawn"
    );

    // Three loops pass with nothing touched.
    set_last_input(&mut app, 0);
    let phase = now + 7;
    assert_eq!(
        app.active_native_settings_preview(wid, phase),
        Some((
            view,
            crate::settings_preview::PreviewAnimation::BlinkEdge { after_ms: 1 }
        ))
    );
    assert!(app.invalidate_active_native_settings_preview(wid, phase));
    assert!(app.prepare_native_input_scratch(wid));
    assert_eq!(drawn(&app), settled);
    assert_eq!(app.active_native_settings_preview(wid, phase), None);
}

/// A Themes demo tick repaints its terminal grid, never its window: the
/// regional raster a tick asks for covers the grid alone, a fraction of the
/// demo's node. Negative control: any other scene's tick covers its whole
/// node.
#[test]
fn a_theme_demo_tick_repaints_only_its_grid() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 160;
        window.rows = 50;
        window.focused = true;
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    let (_, view) = app.active_native_view(wid).expect("native Settings view");
    assert!(app.prepare_native_input_scratch(wid));
    let (node, spec) = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("retained Themes raster")
        .compiled
        .paint
        .iter()
        .find_map(|paint| match &paint.content {
            crate::native_ui::UiContent::SettingsPreview(spec) => {
                Some((paint.rect, (**spec).clone()))
            }
            _ => None,
        })
        .expect("the demo is drawn");
    let grid = spec.tick_rect(node);
    // The grid is the frame's inside, under its tab strip: a tick never
    // repaints the strip, the frame's hairline or the shadow around it. No
    // share of the node says this, as the strip and the frame are a fixed
    // band and the grid grows with the window: at 160×50 cells the grid is
    // 93% of the demo in Windows' 9×23 cell.
    assert!(
        grid.y >= node.y + crate::settings_preview::DEMO_STRIP_HEIGHT
            && grid.x > node.x
            && grid.right() < node.right()
            && grid.bottom() < node.bottom(),
        "a tick's grid {grid:?} reaches the demo's window {node:?}"
    );
    let phase = u64::try_from(app.lat_epoch.elapsed().as_millis()).unwrap() + 1;
    assert!(
        app.invalidate_active_native_settings_preview(wid, phase),
        "the demo plays after the page opens"
    );
    assert!(app.prepare_native_input_scratch(wid));
    let raster = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap();
    let crate::NativeRasterWork::Region { pixels, rect } = raster.last_work else {
        panic!("a demo tick widened to a full-page raster")
    };
    let scale = app.windows[&wid].scale as f32;
    let grid_pixels = ((grid.width + 2.0) * (grid.height + 2.0) * scale * scale) as u64;
    assert!(
        pixels <= grid_pixels,
        "{pixels} > {grid_pixels}: {rect:?} grid {grid:?} node {node:?} scale {scale}"
    );

    let mut cursor = spec.clone();
    cursor.scene = crate::settings_preview::PreviewScene::CursorMotion;
    assert_eq!(cursor.tick_rect(node), node);
}
