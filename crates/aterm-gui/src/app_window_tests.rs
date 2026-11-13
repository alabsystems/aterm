// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use crate::feed_session_for_test as feed;

/// A headless window made after launch is at the launch's `--scale`, as its metrics are, so
/// its overlay cards measure their points at the pixels its cells are drawn at: at
/// `--scale 2` a second window's Overview holds the columns a 1x one does (§14.16).
#[test]
fn a_headless_window_made_after_launch_is_at_the_launchs_scale() {
    let mut app = App::headless_for_test();
    app.headless_scale = 2.0;
    let second = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    assert_eq!(app.windows[&second].scale, 2.0, "the launch's scale, not 1");
    app.windows
        .get_mut(&second)
        .expect("the window")
        .input_scratch
        .cells = vec![Vec::new(); 24];
    assert_eq!(app.frontmost_window, Some(second));
    assert!(app.open_overview().is_ok());
    let t = app
        .overlay_coordinate_transform(second)
        .expect("the open card's transform");
    assert_eq!(
        t.geom.px_per_pt, 2.0,
        "two of the window's pixels to a point"
    );
}

#[test]
fn a_change_at_initial_subscription_reaches_both_accessibility_readings() {
    use std::cell::Cell;

    use crate::native_appearance::AppearancePreferences;

    struct Subscription<'a>(&'a Cell<bool>);

    impl Drop for Subscription<'_> {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }

    let changed = AppearancePreferences {
        high_contrast: true,
        reduced_transparency: true,
        differentiate_without_color: true,
        ..AppearancePreferences::default()
    };
    let capture = |sample_early: bool| {
        let state = Cell::new((false, AppearancePreferences::default()));
        let subscribed = Cell::new(false);
        let queued = Cell::new(0);
        // Negative control: preserve the original pre-subscription snapshot.
        let early = sample_early.then(|| state.get());
        let (observer, reduce_motion, appearance) = observe_initial_accessibility(
            || {
                // The OS changes and posts immediately before registration. Nothing
                // receives that notification; the snapshot must recover its new state.
                state.set((true, changed));
                if subscribed.get() {
                    queued.set(queued.get() + 1);
                }
                subscribed.set(true);
                Subscription(&subscribed)
            },
            || {
                assert!(subscribed.get(), "the snapshot keeps its observer alive");
                early.unwrap_or_else(|| state.get())
            },
        );
        assert_eq!(queued.get(), 0, "no notification can repair this snapshot");
        assert!(subscribed.get(), "the caller retains the observer");
        drop(observer);
        assert!(!subscribed.get());
        (reduce_motion, appearance)
    };
    assert_eq!(capture(false), (true, changed));
    assert_eq!(
        capture(true),
        (false, AppearancePreferences::default()),
        "sampling before subscription loses the change in both readings"
    );
}

#[test]
fn a_cold_restores_first_window_carries_its_frame_unless_sized_otherwise() {
    let layout = |maximized, inner_px| crate::restore::WindowLayout {
        rows: 66,
        cols: 146,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized,
        inner_px,
        presence_row: false,
        show: crate::restore::WindowShow::UNKNOWN,
        tabs: Vec::new(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: None,
        workers_in_front: false,
        restored_tabs: Vec::new(),
    };
    let quit = PhysicalSize::new(2238, 2168);
    let framed = layout(None, Some([2238, 2168]));
    let seed = |layout, lines, columns| {
        CarriedFrame::cold_first_seed(layout, lines, columns).map(|seed| (seed.grid, seed.frame))
    };
    assert_eq!(seed(&framed, None, None), Some(((66, 146), Some(quit))));
    assert_eq!(
        seed(&framed, None, None).and_then(|(_, frame)| frame),
        CarriedFrame::of(&framed).map(|frame| frame.inner_px),
        "as the windows after it carry theirs"
    );
    assert_eq!(seed(&framed, Some(40), None), Some(((40, 146), None)));
    assert_eq!(seed(&framed, None, Some(9)), Some(((66, 20), None)));
    let maximized = layout(Some(true), Some([2238, 2168]));
    assert_eq!(seed(&maximized, None, None), None);
    assert_eq!(seed(&maximized, Some(40), Some(100)), None);
    let unframed = layout(Some(false), None);
    assert_eq!(seed(&unframed, None, None), Some(((66, 146), None)));
}

#[test]
fn a_carried_frame_is_kept_where_it_holds_the_grid_or_the_deck_moved_under_it() {
    let cell = (14_usize, 28_usize);
    let grid = (66_u16, 146_u16);
    let grid_of = |px: PhysicalSize<u32>| {
        (
            (px.height as usize / cell.1) as u16,
            (px.width as usize / cell.0) as u16,
        )
    };
    let exact = PhysicalSize::new(146 * 14, 66 * 28);
    let by = |w: u32, h: u32| PhysicalSize::new(exact.width + w, exact.height + h);
    let kept = |carried, moved| kept_carried_frame(exact, carried, cell, grid, moved, grid_of);
    assert_eq!(kept(exact, false), Some(exact));
    assert_eq!(
        kept(by(13, 27), false),
        Some(by(13, 27)),
        "under a cell to spare"
    );
    assert_eq!(kept(by(14, 0), false), None, "a column to spare");
    assert_eq!(kept(by(0, 28), false), None, "a row to spare");
    let short = PhysicalSize::new(exact.width, exact.height - 1);
    assert_eq!(kept(short, false), None, "short of the grid");
    let restyled = by(5, 3 * 28);
    assert_eq!(kept(restyled, false), None);
    assert_eq!(
        kept(restyled, true),
        Some(restyled),
        "the deck moved under it"
    );
    assert_eq!(
        kept(by(14, 3 * 28), true),
        None,
        "its columns are not the grid's"
    );
}

#[test]
fn the_os_caption_drops_a_leading_spinner_frame() {
    assert_eq!(
        os_window_title("\u{25d0} Aterm OSX shortcuts and font — Running claude"),
        "Aterm OSX shortcuts and font — Running claude"
    );
    assert_eq!(os_window_title("\u{280b} cargo build"), "cargo build");
    assert_eq!(os_window_title("cargo build"), "cargo build");
    assert_eq!(os_window_title("\u{25d0}"), "\u{25d0}");
    assert_eq!(os_window_title("\u{25d0}x"), "\u{25d0}x");
}

#[test]
fn a_running_block_arms_every_close_gate_without_a_child_process() {
    let app = crate::App::headless_for_test();
    let wid = WindowId(0);
    assert!(
        !app.session_foreground_busy(0),
        "a fresh stub session is idle"
    );
    assert!(!app.window_has_foreground_job(wid));
    assert!(!app.any_foreground_job());

    feed(
        &app,
        0,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07Start-Sleep 60\n\x1b]133;C\x07",
    );
    assert!(
        app.session_foreground_busy(0),
        "133;C opened, no 133;D: the shell said a command is running"
    );
    assert!(
        app.window_has_foreground_job(wid),
        "the window close gate (✕, Close Tab, ctl close) sees it"
    );
    assert!(app.any_foreground_job(), "the whole-app Quit gate sees it");
    assert!(
        !app.session_foreground_busy(99),
        "an unknown session id is idle, never a wedge"
    );

    feed(&app, 0, b"\x1b]133;D;0\x07");
    assert!(!app.session_foreground_busy(0), "133;D: the job is over");
    assert!(!app.window_has_foreground_job(wid));
    assert!(!app.any_foreground_job());
}

#[test]
fn a_co_viewed_running_block_exempts_the_window_gate_but_not_quit() {
    let mut app = crate::App::headless_for_test();
    let wid = WindowId(0);
    feed(
        &app,
        0,
        b"\x1b]133;A\x07$ \x1b]133;B\x07while :; do :; done\n\x1b]133;C\x07",
    );
    assert!(
        app.window_has_foreground_job(wid),
        "precondition: busy, sole viewer"
    );

    app.pool.attach(0); // a second window now shows session 0
    assert_eq!(app.pool.views(0), Some(2));
    assert!(
        !app.window_has_foreground_job(wid),
        "closing this window only detaches a view: the job survives elsewhere"
    );
    assert!(
        app.any_foreground_job(),
        "Quit would hang up every view, so the job still counts there"
    );

    assert!(
        !app.pool.detach(0),
        "back to one view; the session lives on"
    );
    assert!(
        app.window_has_foreground_job(wid),
        "the sole viewer again: this window's close would hang the job up"
    );
}

#[test]
fn early_reveal_backdrop_is_the_colour_the_first_frame_paints() {
    let mut app = crate::App::headless_for_test();
    for appearance in [
        aterm_types::Appearance::Light,
        aterm_types::Appearance::Dark,
    ] {
        app.sync_app_theme_to_appearance(appearance);
        let backdrop = app.early_reveal_backdrop();
        let tc = app
            .config
            .applied_terminal_config_for_with_assets(appearance, &app.config_assets.themes);
        let cell = tc.default_background;
        assert_eq!(
            [cell.r, cell.g, cell.b],
            crate::settings::u32_rgb(backdrop),
            "{appearance:?}: an unstyled cell of the first frame must paint the erase colour"
        );
        assert_eq!(
            crate::app_render::present_band_bg(aterm_core::render::COLOR_UNSET, app.theme.bg),
            backdrop,
            "{appearance:?}: the bands around the grid must paint the erase colour"
        );
    }
    app.theme.bg = 0xFF11_1318;
    assert_eq!(
        app.early_reveal_backdrop(),
        0x11_1318,
        "the brush is seeded 24-bit: a stray alpha byte is masked off"
    );
}

const SCREEN: WorkAreaPts = WorkAreaPts {
    x: 0.0,
    y: 25.0,
    w: 1440.0,
    h: 875.0,
};

#[test]
fn a_new_window_never_lands_exactly_on_the_window_it_was_opened_from() {
    let anchor = (400.0, 200.0);
    let placed = cascade_top_left_pts(anchor, CASCADE_STEP_PTS, (900.0, 600.0), SCREEN)
        .expect("ordinary geometry places the window");
    assert_ne!(
        placed, anchor,
        "the regression: winit centres every new window, so a same-sized window \
         lands pixel-exactly on its parent"
    );
    assert_eq!(
        placed,
        (anchor.0 + CASCADE_STEP_PTS, anchor.1 + CASCADE_STEP_PTS),
        "an ordinary cascade is one diagonal step down-right"
    );
    assert!(placed.0 > anchor.0 && placed.1 > anchor.1);
}

#[test]
fn the_cascade_wraps_per_axis_instead_of_marching_off_screen() {
    let window = (900.0, 600.0);
    let low = (300.0, 720.0);
    let placed = cascade_top_left_pts(low, CASCADE_STEP_PTS, window, SCREEN)
        .expect("ordinary geometry places the window");
    assert_eq!(placed, (low.0 + CASCADE_STEP_PTS, SCREEN.y));

    let far_right = (1260.0, 100.0);
    let placed = cascade_top_left_pts(far_right, CASCADE_STEP_PTS, window, SCREEN)
        .expect("ordinary geometry places the window");
    assert_eq!(placed, (SCREEN.x, far_right.1 + CASCADE_STEP_PTS));

    let corner = (1260.0, 720.0);
    assert_eq!(
        cascade_top_left_pts(corner, CASCADE_STEP_PTS, window, SCREEN),
        Some((SCREEN.x, SCREEN.y))
    );
}

#[test]
fn a_screen_filling_parent_is_still_stepped_off_not_wrapped_back_onto() {
    let filled = (SCREEN.x, SCREEN.y);
    let frame = (SCREEN.w, SCREEN.h);
    let placed = cascade_top_left_pts(filled, CASCADE_STEP_PTS, frame, SCREEN)
        .expect("a screen-filling window is still placed");
    assert_eq!(
        placed,
        (SCREEN.x + CASCADE_STEP_PTS, SCREEN.y + CASCADE_STEP_PTS)
    );
    assert_ne!(placed, filled, "the child must not land on its parent");
}

#[test]
fn every_placement_keeps_a_grabbable_corner_on_screen() {
    let inside = |(x, y): (f64, f64), window: (f64, f64)| {
        let keep_x = window.0.min(CASCADE_MIN_VISIBLE_PTS);
        let keep_y = window.1.min(CASCADE_MIN_VISIBLE_PTS);
        x >= SCREEN.x
            && y >= SCREEN.y
            && x + keep_x <= SCREEN.x + SCREEN.w
            && y + keep_y <= SCREEN.y + SCREEN.h
    };

    let huge = (2000.0, 1400.0);
    let placed = cascade_top_left_pts((100.0, 100.0), CASCADE_STEP_PTS, huge, SCREEN)
        .expect("an oversized window is still placed");
    assert!(inside(placed, huge), "{placed:?} left the usable screen");

    let window = (900.0, 600.0);
    let placed = cascade_top_left_pts((5000.0, 5000.0), CASCADE_STEP_PTS, window, SCREEN)
        .expect("an off-screen anchor is still placed");
    assert!(inside(placed, window), "{placed:?} followed the anchor off");

    let tiny = (90.0, 40.0);
    let placed = cascade_top_left_pts((1420.0, 880.0), CASCADE_STEP_PTS, tiny, SCREEN)
        .expect("a tiny window is still placed");
    assert!(inside(placed, tiny), "{placed:?} left the usable screen");
    assert!(placed.0 + tiny.0 <= SCREEN.x + SCREEN.w);
    assert!(placed.1 + tiny.1 <= SCREEN.y + SCREEN.h);
}

#[test]
fn degenerate_geometry_leaves_the_os_placement_alone() {
    let empty = WorkAreaPts {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    };
    assert_eq!(
        cascade_top_left_pts((10.0, 10.0), CASCADE_STEP_PTS, (900.0, 600.0), empty),
        None,
        "a 0x0 monitor must not be treated as a placement target"
    );
    assert_eq!(
        cascade_top_left_pts((f64::NAN, 10.0), CASCADE_STEP_PTS, (900.0, 600.0), SCREEN),
        None,
        "a non-finite coordinate must not reach the clamp"
    );
    assert_eq!(
        cascade_top_left_pts(
            (10.0, 10.0),
            CASCADE_STEP_PTS,
            (f64::INFINITY, 600.0),
            SCREEN
        ),
        None,
    );
}

#[test]
fn the_cascade_anchors_on_the_window_the_user_was_in_never_on_the_new_one() {
    let (a, b, new) = (WindowId(0), WindowId(1), WindowId(2));
    let attached = [a, b, new];
    assert_eq!(
        cascade_anchor_of(new, &[a, b], &attached),
        Some(b),
        "the most recently focused OTHER window is the anchor"
    );
    assert_eq!(cascade_anchor_of(new, &[a, b, new], &attached), Some(b));
    assert_eq!(cascade_anchor_of(new, &[], &attached), Some(b));
    assert_eq!(cascade_anchor_of(new, &[b, a], &[a, new]), Some(a));
    assert_eq!(cascade_anchor_of(new, &[], &[new]), None);
    assert_eq!(cascade_anchor_of(new, &[new], &[]), None);
}

#[test]
fn first_successful_target_offers_milestones_even_after_early_backend_finalize() {
    let app = App::headless_for_test();
    assert!(
        !app.backend.is_pending(),
        "regression precondition: an early Wake already finalized the backend"
    );
    let now = Instant::now();
    let milestones = crate::metrics::StartupAttachMilestones::new([now; 7]);
    let (attached, publication) = present_target_install_outcome(false, milestones);
    assert!(
        !attached && publication.is_none(),
        "negative control: a stale wid must fail even after surface creation"
    );
    let (attached, publication) = present_target_install_outcome(true, milestones);
    assert!(
        attached && publication.is_some(),
        "a successfully installed target must offer its complete milestones"
    );
}

#[test]
fn automatic_window_chrome_remains_system_owned_independent_of_terminal_palette() {
    let mut app = App::headless_for_test();
    app.window_theme = crate::app_config::WindowTheme::Auto;
    app.theme.bg = 0x000000;
    assert_eq!(
        app.window_theme_for_chrome(),
        crate::app_config::WindowTheme::Auto
    );
    app.theme.bg = 0xffffff;
    assert_eq!(
        app.window_theme_for_chrome(),
        crate::app_config::WindowTheme::Auto,
        "terminal colors must not silently replace system-following chrome"
    );

    app.window_theme = crate::app_config::WindowTheme::Dark;
    assert_eq!(
        app.window_theme_for_chrome(),
        crate::app_config::WindowTheme::Dark,
        "an explicit chrome override remains explicit"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_csd_chrome_resolves_auto_from_terminal_theme_darkness() {
    let mut app = App::headless_for_test();
    app.window_theme = crate::app_config::WindowTheme::Auto;
    app.theme.bg = 0x000000;
    assert_eq!(
        app.chrome_theme_for_apprt(),
        crate::app_config::WindowTheme::Dark,
        "a dark terminal theme must not sit under a light-grey CSD slab"
    );
    app.theme.bg = 0xffffff;
    assert_eq!(
        app.chrome_theme_for_apprt(),
        crate::app_config::WindowTheme::Light,
        "a light terminal theme resolves the light CSD variant"
    );
    app.window_theme = crate::app_config::WindowTheme::Light;
    app.theme.bg = 0x000000;
    assert_eq!(
        app.chrome_theme_for_apprt(),
        crate::app_config::WindowTheme::Light,
        "an explicit chrome override remains the operator's word"
    );
}

#[test]
fn chrome_painters_resolve_window_theme_to_a_real_palette() {
    let mut app = App::headless_for_test();
    app.window_theme = crate::app_config::WindowTheme::Auto;
    let terminal = app.theme;
    assert!(
        crate::tab_bar::theme_is_dark(terminal.bg),
        "precondition: the default terminal theme is dark"
    );
    let auto = app.chrome_palette_theme();
    assert_eq!(
        (auto.fg, auto.bg, auto.cursor, auto.selection),
        (
            terminal.fg,
            terminal.bg,
            terminal.cursor,
            terminal.selection
        ),
        "Auto keeps chrome an extension of the terminal theme"
    );

    app.window_theme = crate::app_config::WindowTheme::Dark;
    let matching = app.chrome_palette_theme();
    assert_eq!(
        (matching.fg, matching.bg),
        (terminal.fg, terminal.bg),
        "forcing the terminal theme's own side keeps theme-derived chrome"
    );

    app.window_theme = crate::app_config::WindowTheme::Light;
    let forced = app.chrome_palette_theme();
    assert_eq!(
        forced.bg,
        crate::native_appearance::FORCED_LIGHT_CHROME_BG,
        "window_theme=light on a dark terminal theme paints the authored light chrome"
    );
    assert_eq!(forced.fg, crate::native_appearance::FORCED_LIGHT_CHROME_FG);
    assert!(
        !crate::tab_bar::theme_is_dark(forced.bg),
        "the forced palette classifies as the forced side"
    );
    assert_eq!(
        forced.selection, terminal.selection,
        "the accent identity is carried from the terminal theme"
    );
    assert_eq!(
        app.chrome_theme_for_apprt(),
        crate::app_config::WindowTheme::Light
    );
}

#[test]
fn native_pages_draw_from_the_theme_before_color_overrides() {
    let mut app = App::headless_for_test();
    let base = app.theme;
    app.config.foreground = Some("#FFAA00".to_string());
    app.config.background = Some("#202040".to_string());
    app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    assert_eq!(app.theme.fg, 0x00FF_AA00, "the terminal takes the override");
    let page = app.native_page_palette_theme();
    assert_eq!(
        (page.fg, page.bg),
        (base.fg, base.bg),
        "the pages keep the theme's own colors"
    );
    app.window_theme = crate::app_config::WindowTheme::Light;
    assert_eq!(
        app.native_page_palette_theme().bg,
        crate::native_appearance::FORCED_LIGHT_CHROME_BG
    );
}

#[test]
fn the_page_palette_follows_the_installed_resolution_not_the_live_config() {
    let mut app = App::headless_for_test();
    app.config.foreground = Some("#FFAA00".to_string());
    app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    let (terminal, page) = (app.theme, app.native_page_palette_theme());
    app.config.theme = Some("Nord".to_string());
    assert_eq!(app.theme, terminal, "nothing installed yet");
    assert_eq!(
        app.native_page_palette_theme(),
        page,
        "the page waits for the same install the terminal waits for"
    );
    app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    let nord = aterm_render::Theme::from(&aterm_types::scheme::builtin("Nord").expect("Nord"));
    assert_eq!(app.theme.fg, 0x00FF_AA00, "the terminal keeps its override");
    assert_eq!(
        app.native_page_palette_theme(),
        crate::native_appearance::resolve_chrome_palette(app.window_theme, nord),
        "the page moves to the new theme with the install"
    );
}

#[test]
fn native_pages_follow_the_theme_trial_with_terminal_color_overrides() {
    for foreground in [None, Some("#FFAA00".to_string())] {
        let mut app = App::headless_for_test();
        app.config = crate::app_config::Config {
            theme: Some("Default".to_string()),
            foreground: foreground.clone(),
            ..crate::app_config::Config::default()
        };
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        let saved_page = app.native_page_palette_theme();
        let trial_page = crate::app_config::Config {
            theme: Some("Nord".to_string()),
            ..crate::app_config::Config::default()
        }
        .theme_for_with_assets(app.os_appearance, &app.config_assets.themes);
        assert_ne!(saved_page.bg, trial_page.bg, "the trial changes the page");

        app.begin_theme_preview(
            "Nord",
            crate::control_theme::PreviewHold::Settings { keeping: false },
        )
        .expect("the built-in trial is available");
        let shown = app.native_page_palette_theme();
        assert_eq!(
            (shown.fg, shown.bg, shown.cursor, shown.selection),
            (
                trial_page.fg,
                trial_page.bg,
                trial_page.cursor,
                trial_page.selection
            ),
            "the page draws the trial before terminal overrides: {foreground:?}"
        );
        if foreground.is_some() {
            assert_eq!(app.theme.fg, 0x00FF_AA00, "the terminal keeps its override");
            assert_ne!(
                shown.fg, app.theme.fg,
                "the page does not inherit that override"
            );
        }

        app.window_theme = crate::app_config::WindowTheme::Light;
        assert_eq!(
            app.native_page_palette_theme().bg,
            crate::native_appearance::FORCED_LIGHT_CHROME_BG,
            "forcing the window light still overrides the trial's dark side"
        );
        app.window_theme = crate::app_config::WindowTheme::Auto;
        app.revert_theme_preview(Instant::now());
        let reverted = app.native_page_palette_theme();
        assert_eq!(
            (
                reverted.fg,
                reverted.bg,
                reverted.cursor,
                reverted.selection
            ),
            (
                saved_page.fg,
                saved_page.bg,
                saved_page.cursor,
                saved_page.selection
            ),
            "revert restores the saved page palette: {foreground:?}"
        );
        assert!(app.theme_preview.is_none());
        assert_eq!(
            app.config.foreground, foreground,
            "a trial never writes config"
        );
        assert_eq!(app.config.theme.as_deref(), Some("Default"));
    }
}

#[test]
fn forcing_a_side_moves_the_pages_across_the_former_classifier_band() {
    let mut app = App::headless_for_test();
    for gray in [0x76_u32, 0x80, 0x93, 0x96, 0x97, 0xA0, 0xA6, 0xAD] {
        let bg = (gray << 16) | (gray << 8) | gray;
        app.theme.bg = bg;
        for (window_theme, want_dark) in [
            (crate::app_config::WindowTheme::Light, false),
            (crate::app_config::WindowTheme::Dark, true),
        ] {
            app.window_theme = window_theme;
            let chrome = app.chrome_palette_theme();
            let side = [
                ((chrome.bg >> 16) & 0xff) as u8,
                ((chrome.bg >> 8) & 0xff) as u8,
                (chrome.bg & 0xff) as u8,
            ];
            assert_eq!(
                crate::native_appearance::surface_is_dark(side),
                want_dark,
                "#{gray:02X} under {window_theme:?}: the ROLE pipeline must build \
                 the forced side, whether the theme passed through or the \
                 authored palette replaced it"
            );
            assert_eq!(
                crate::tab_bar::theme_is_dark(chrome.bg),
                want_dark,
                "#{gray:02X} under {window_theme:?}: and the strip must agree"
            );
        }
    }
}

#[test]
fn window_created_after_config_asset_publication_inherits_custom_art() {
    let mut app = App::headless_for_test();
    let rgba: Arc<[u8]> = Arc::from([0xff, 0x22, 0xaa, 0xff]);
    let assets = Arc::new(crate::app_config::ConfigAssetCatalog {
        trail_style: crate::app_config::TrailStyleCache::empty(),
        wallpaper: crate::app_config::WallpaperAsset::None,
        kitty_sprite: crate::app_config::KittySpriteAsset::Ready {
            source_id: Arc::from("test.png"),
            w: 1,
            h: 1,
            rgba: Arc::clone(&rgba),
            fp: 7,
        },
        themes: crate::app_config::ThemeCatalog::empty(),
        sparkle_spec_consumers: Default::default(),
    });
    assert_eq!(app.publish_config_assets(assets), 1);
    let sid = app.next_session_id;
    let wid = app.insert_logical_window(crate::stub_session(sid), 24, 80);
    assert!(
        app.windows[&wid].fx.decos().has_custom_kitty_sprite(),
        "a post-publication window must not silently fall back to built-in art"
    );
    assert!(Arc::ptr_eq(
        app.windows[&wid].fx.decos().kitty_sprite_rgba().unwrap(),
        &rgba
    ));
}

#[test]
fn every_font_size_draws_at_its_window_scale() {
    assert_eq!(crate::font_px_for_scale(FONT_PX, 1.0), FONT_PX);
    for scale in [1.25, 1.5, 2.0, 3.0] {
        assert_eq!(
            crate::font_px_for_scale(FONT_PX, scale),
            (FONT_PX * scale as f32).round(),
            "the default at {scale}"
        );
    }
    assert_eq!(crate::font_px_for_scale(13.0, 2.0), 26.0);
    assert!(
        crate::font_px_for_scale(FONT_PX + 1.0, 2.0) > crate::font_px_for_scale(FONT_PX, 2.0),
        "one step up from this platform's default grows the text at 2×"
    );
    assert_eq!(
        crate::font_px_for_scale(12.5, 1.0),
        12.5,
        "1× keeps a fraction"
    );
    for scale in [1.25, 2.0] {
        for size in 8..36 {
            let (a, b) = (size as f32, (size + 1) as f32);
            assert!(
                crate::font_px_for_scale(b, scale) > crate::font_px_for_scale(a, scale),
                "a step up from {a} at {scale} grows the text"
            );
        }
    }
    assert_eq!(crate::font_px_for_scale(150.0, 2.0), FONT_PX_MAX);
    assert_eq!(crate::font_px_for_scale(f32::NAN, 2.0), FONT_PX);
    assert_eq!(font_size_of_device_px(26.0, 2.0), 13.0);
    assert_eq!(font_size_of_device_px(15.0, 1.25), 12.0);
    assert_eq!(font_size_of_device_px(13.0, 1.0), 13.0);
}

#[test]
fn native_chrome_title_never_overwrites_parked_terminal_identity() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let local_id = app
        .store
        .read()
        .unwrap_or_else(|poison| poison.into_inner())
        .snapshot()
        .first()
        .expect("headless app registers its terminal")
        .local_id;
    app.publish_active_terminal_title(wid, "~/project");
    assert_eq!(
        app.store
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .by_local(local_id)
            .map(|session| session.title.clone()),
        Some("~/project".to_string())
    );

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    assert!(app.active_native_view(wid).is_some());
    app.publish_active_terminal_title(wid, "Settings");
    assert_eq!(
        app.store
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .by_local(local_id)
            .map(|session| session.title.clone()),
        Some("~/project".to_string()),
        "native presentation title must not corrupt parked terminal metadata"
    );
}

#[test]
fn window_title_identity_matches_user_osc_cwd_fallback_precedence() {
    let app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.pool.get(0).expect("session 0");
    let mut slot = String::new();

    assert_eq!(app.window_title_identity(wid, "", &mut slot), Some(0));
    assert_eq!(slot, "aterm");

    crate::term_lock(&session.term)
        .process(b"\x1b]7;file://localhost/aterm-proof/window-title\x07");
    let _ = session
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .set(
            "description",
            Some("Runs the focused release checks".to_string()),
        );
    let session_id = app.window_title_identity(wid, "", &mut slot);
    assert_eq!(session_id, Some(0));
    assert_eq!(
        slot, "/aterm-proof/window-title",
        "OSC-7 cwd is the title, and an authored description is not joined into it"
    );
    assert_eq!(
        app.chrome_labels.present(
            session_id,
            &slot,
            crate::chrome_label::ChromeSurface::WindowTitle,
        ),
        "/aterm-proof/window-title",
        "the window title is the identity title alone"
    );

    app.window_title_identity(wid, "vim src/main.rs", &mut slot);
    assert_eq!(slot, "vim src/main.rs", "a live OSC 0/2 title outranks cwd");
    let _ = session
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .set("title", Some("release builder".to_string()));
    app.window_title_identity(wid, "vim src/main.rs", &mut slot);
    assert_eq!(
        slot, "release builder",
        "the operator title is the top rung"
    );
}

#[test]
fn a_mixed_tab_s_untitled_terminal_titles_the_window_by_its_cwd() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    crate::term_lock(&app.pool.get(session).expect("the split's shell").term)
        .process(b"\x1b]7;file://localhost/aterm-proof/mixed-title\x07");
    let title = app
        .prepare_heterogeneous_input_scratch(wid)
        .expect("a mixed frame");
    let mut slot = String::new();
    assert_eq!(
        app.window_title_identity(wid, &title, &mut slot),
        Some(session)
    );
    assert_eq!(
        slot, "/aterm-proof/mixed-title",
        "an untitled terminal leaf titles the window by its cwd, as a terminal tab does"
    );
}

#[test]
fn window_title_cwd_fallback_never_waits_for_the_terminal_parser() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .tab_title_cache
        .insert(
            0,
            crate::app_tabs::CapturedTitle {
                title: "~/cached-worktree".to_string(),
                untitled: false,
            },
        );
    let term = app.pool.get(0).expect("session 0").term.clone();
    let _parser_guard = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut slot = String::new();
    app.window_title_identity(wid, "", &mut slot);
    assert_eq!(
        slot, "~/cached-worktree",
        "a contended parser keeps the last stable label instead of blocking or flickering"
    );
}

#[test]
fn spinner_phase_only_store_titles_are_coalesced() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let local_id = app
        .store
        .read()
        .unwrap_or_else(|poison| poison.into_inner())
        .snapshot()
        .first()
        .expect("headless app registers its terminal")
        .local_id;

    let stored_title = |app: &App| {
        app.store
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .by_local(local_id)
            .map(|session| session.title.clone())
    };

    app.publish_active_terminal_title(wid, "⠋ aterm");
    assert_eq!(stored_title(&app), Some("⠋ aterm".to_string()));

    app.publish_active_terminal_title(wid, "⠙ aterm");
    app.publish_active_terminal_title(wid, "⠹ aterm");
    assert_eq!(stored_title(&app), Some("⠋ aterm".to_string()));

    app.publish_active_terminal_title(wid, "⠸ project");
    assert_eq!(stored_title(&app), Some("⠸ project".to_string()));
    app.publish_active_terminal_title(wid, "project");
    assert_eq!(stored_title(&app), Some("project".to_string()));
}

#[test]
fn heterogeneous_window_teardown_enumerates_every_terminal_view() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native_view) = app.active_native_view(wid).expect("Settings focused");
    let (mixed_session, mixed_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);

    let legacy_projection = app.windows[&wid]
        .layouts
        .iter()
        .flat_map(crate::pane::PaneTree::sessions)
        .collect::<Vec<_>>();
    assert!(
        !legacy_projection.contains(&mixed_session),
        "negative control: all-terminal layouts omit a heterogeneous leaf"
    );
    let canonical = app.window_terminal_sessions(wid);
    assert!(
        canonical.contains(&0),
        "ordinary terminal tab remains owned"
    );
    assert!(
        canonical.contains(&mixed_session),
        "job safety and teardown enumerate the mixed terminal leaf"
    );
    assert!(app.pool.get(mixed_session).is_some());

    assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
    assert!(app.pool.get(0).is_none());
    assert!(app.pool.get(mixed_session).is_none());
    assert!(app.view_store.get(native_view).is_none());
    assert!(app.view_store.get(mixed_view).is_none());
}

#[test]
fn close_safety_counts_all_same_session_views_inside_the_window() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let second_view = app
        .view_store
        .insert_terminal(0)
        .expect("duplicate terminal view identity");
    app.pool.attach(0);
    assert!(
        app.windows
            .get_mut(&wid)
            .and_then(|window| window.tab_set.active_mut())
            .is_some_and(
                |tab| tab.split_focused(crate::tab_model::SplitAxis::Horizontal, second_view,)
            )
    );
    app.sync_window(wid);

    assert_eq!(app.pool.views(0), Some(2));
    assert_eq!(
        app.window_terminal_sessions(wid)
            .into_iter()
            .filter(|session| *session == 0)
            .count(),
        2
    );
    assert!(
        app.window_drops_last_session_view(wid, 0),
        "both global views disappear with this one window"
    );
    assert_ne!(
        app.pool.views(0),
        Some(1),
        "negative control: the retired views==1 rule would miss this close"
    );
}

fn desk() -> Vec<MonitorRectPx> {
    vec![
        MonitorRectPx {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
            scale: 1.0,
        },
        MonitorRectPx {
            x: 1920.0,
            y: 0.0,
            w: 2560.0,
            h: 1440.0,
            scale: 2.0,
        },
    ]
}

#[test]
fn restored_positions_on_live_monitors_are_kept() {
    assert!(restored_origin_visible((100.0, 100.0), &desk()));
    assert!(restored_origin_visible((2400.0, 300.0), &desk()));
    assert!(restored_origin_visible((0.0, 0.0), &desk()));
    assert!(restored_origin_visible((1920.0, 0.0), &desk()));
}

#[test]
fn restored_positions_on_a_departed_monitor_are_refused() {
    let undocked = vec![MonitorRectPx {
        x: 0.0,
        y: 0.0,
        w: 1920.0,
        h: 1080.0,
        scale: 1.0,
    }];
    assert!(restored_origin_visible((2400.0, 300.0), &desk()));
    assert!(!restored_origin_visible((2400.0, 300.0), &undocked));
    assert!(!restored_origin_visible((-32000.0, -32000.0), &desk()));
    assert!(!restored_origin_visible((0.0, 5000.0), &desk()));
}

#[test]
fn restored_positions_need_a_grabbable_corner_not_just_intersection() {
    assert!(!restored_origin_visible((1920.0 - 10.0, 100.0), &desk()));
    assert!(!restored_origin_visible((100.0, 1080.0 - 10.0), &desk()));
    assert!(!restored_origin_visible(
        (1920.0 + 2560.0 - 200.0, 100.0),
        &desk()
    ));
    assert!(!restored_origin_visible((f64::NAN, 10.0), &desk()));
    assert!(!restored_origin_visible((10.0, 10.0), &[]));
    assert!(!restored_origin_visible(
        (10.0, 10.0),
        &[MonitorRectPx {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            scale: 1.0,
        }]
    ));
}

fn arm_settle(app: &mut App, rows: u16, cols: u16, budget: u8) -> PhysicalSize<u32> {
    let wid = WindowId(0);
    app.windows
        .get_mut(&wid)
        .expect("headless window 0")
        .initial_frame_settle = Some(crate::InitialFrameSettle {
        rows,
        cols,
        deadline: Instant::now() + std::time::Duration::from_secs(60),
        corrections: budget,
        corrected: false,
    });
    app.window_frame_px_for(wid, rows, cols)
}

fn settle_state(app: &App) -> Option<crate::InitialFrameSettle> {
    app.windows
        .get(&WindowId(0))
        .and_then(|ws| ws.initial_frame_settle)
}

#[test]
fn initial_frame_settle_waits_out_the_csd_header_echo_then_disarms() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let intended = arm_settle(&mut app, 24, 80, 3);

    app.settle_initial_frame(wid, intended);
    let s = settle_state(&app).expect("an initially-correct frame must stay armed");
    assert!(!s.corrected, "no correction was needed yet");
    assert_eq!(s.corrections, 3);

    let stolen = PhysicalSize::new(intended.width, intended.height - 35);
    app.settle_initial_frame(wid, stolen);
    let s = settle_state(&app).expect("a spent correction stays armed for its own echo");
    assert!(s.corrected);
    assert_eq!(
        s.corrections, 2,
        "the mismatch spends exactly one correction"
    );

    app.settle_initial_frame(wid, intended);
    assert!(
        settle_state(&app).is_none(),
        "intended size after a correction = settled: disarm"
    );
}

#[test]
fn initial_frame_settle_accepts_a_band_absorbed_fractional_scale_residue() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let intended = arm_settle(&mut app, 24, 80, 3);
    let (cell_w, _) = app.win_cell_size(wid);

    let residue = PhysicalSize::new(intended.width + 1, intended.height);
    assert_eq!(
        app.grid_dims_for(wid, residue),
        (24, 80),
        "negative control: a 1 px residue must NOT move the grid"
    );
    app.settle_initial_frame(wid, residue);
    let s = settle_state(&app).expect("still armed — the lossy echo may yet arrive");
    assert_eq!(
        s.corrections, 3,
        "a band-absorbed residue must spend NO correction (pre-fix: spent one, \
         then two more, on every fractional-scale launch)"
    );
    assert!(!s.corrected);

    let short = PhysicalSize::new(
        intended.width - u32::try_from(cell_w).expect("cell width fits u32"),
        intended.height,
    );
    assert_ne!(
        app.grid_dims_for(wid, short),
        (24, 80),
        "negative control: a whole cell must move the grid"
    );
    app.settle_initial_frame(wid, short);
    assert_eq!(
        settle_state(&app).expect("armed").corrections,
        2,
        "a real grid change still spends exactly one correction"
    );
}

#[test]
fn initial_frame_settle_budget_bounds_the_fight() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let intended = arm_settle(&mut app, 24, 80, 1);
    let insisted = PhysicalSize::new(1920, 1080);

    app.settle_initial_frame(wid, insisted);
    assert_eq!(
        settle_state(&app)
            .expect("one correction spent")
            .corrections,
        0
    );

    app.settle_initial_frame(wid, insisted);
    assert!(
        settle_state(&app).is_none(),
        "budget exhausted: the insisted size is accepted for good"
    );
    assert_ne!(
        intended, insisted,
        "negative control: the fight was real, not a matching no-op"
    );
}

#[test]
fn initial_frame_settle_deadline_expires_without_correcting() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let intended = arm_settle(&mut app, 24, 80, 3);
    app.windows
        .get_mut(&wid)
        .unwrap()
        .initial_frame_settle
        .as_mut()
        .unwrap()
        .deadline = Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .expect("the machine has been up a second");

    app.settle_initial_frame(wid, PhysicalSize::new(intended.width, intended.height - 35));
    assert!(
        settle_state(&app).is_none(),
        "expiry disarms instead of spending a correction"
    );
}

#[test]
fn initial_frame_settle_keeps_the_csd_echo_off_the_engine_and_pty() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let intended = arm_settle(&mut app, 24, 80, 3);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let rows = |app: &App| {
        (
            term.lock().expect("term").rows(),
            app.windows.get(&wid).expect("window 0").rows,
        )
    };
    let echo = PhysicalSize::new(intended.width, intended.height - 35);
    let (echo_rows, echo_cols) = app.grid_dims_for(wid, echo);
    assert!(
        echo_rows < 24 && echo_cols == 80,
        "negative control: the echo is a real grid change: {echo_rows}x{echo_cols}"
    );

    let mut seen = Vec::new();
    app.apply_window_resized_grid(wid, intended); // first configure: right
    seen.push(rows(&app));
    app.apply_window_resized_grid(wid, echo); // activation echo
    seen.push(rows(&app));
    app.apply_window_resized_grid(wid, intended); // the correction lands
    seen.push(rows(&app));
    app.flush_pending_resize(wid); // the trailing settle, if still armed
    seen.push(rows(&app));
    assert_eq!(
        seen,
        [(24, 24); 4],
        "the engine (and so the PTY winsize) must stay at the attach grid \
         through the echo — pre-fix it read 24 -> 21 -> 24"
    );
    assert!(
        settle_state(&app).is_none(),
        "the corrected frame settles the arm"
    );

    let intended = arm_settle(&mut app, 24, 80, 3);
    app.apply_window_resized_grid(wid, intended);
    app.apply_window_resized_grid(wid, echo);
    app.apply_window_resized_grid(wid, echo);
    assert_eq!(rows(&app), (24, 24), "a repeated echo is vetoed too");
    assert_eq!(
        settle_state(&app).expect("still correcting").corrections,
        1,
        "each vetoed echo spent one correction"
    );
    app.apply_window_resized_grid(wid, intended);
    assert_eq!(rows(&app), (24, 24));
    assert!(settle_state(&app).is_none());
}

#[test]
fn a_refused_initial_frame_correction_still_lands_the_real_size() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let intended = arm_settle(&mut app, 24, 80, 3);
    let echo = PhysicalSize::new(intended.width, intended.height - 35);
    let (echo_rows, _) = app.grid_dims_for(wid, echo);
    assert!(
        echo_rows < 24,
        "PRECONDITION: the echo is a real grid change"
    );
    app.apply_window_resized_grid(wid, intended);
    app.apply_window_resized_grid(wid, echo);
    assert_eq!(
        app.windows.get(&wid).expect("window 0").pending_resize,
        Some(echo),
        "the vetoed size is parked, not dropped"
    );
    assert!(
        app.windows
            .get(&wid)
            .expect("window 0")
            .next_resize_settle
            .is_some(),
        "and its trailing settle is armed"
    );
    app.flush_pending_resize(wid);
    assert_eq!(
        app.windows.get(&wid).expect("window 0").rows,
        echo_rows,
        "no correction arrived: the window's real size is the truth"
    );
}

#[test]
fn a_vetoed_async_echo_is_parked_past_a_configure_round_trip() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let intended = arm_settle(&mut app, 24, 80, 3);
    let echo = PhysicalSize::new(intended.width, intended.height - 35);
    app.apply_window_resized_grid(wid, intended);
    let before = Instant::now();
    app.apply_window_resized_grid(wid, echo);
    let parked = app
        .windows
        .get(&wid)
        .expect("window 0")
        .next_resize_settle
        .expect("the vetoed echo is parked");
    assert!(
        parked >= before + App::SETTLE_VETO_FALLBACK,
        "the fallback must outlast a configure round trip, not one throttle \
         window (pre-fix: now + {:?})",
        crate::RESIZE_THROTTLE
    );
    assert!(
        App::SETTLE_VETO_FALLBACK > crate::RESIZE_THROTTLE,
        "negative control: the park is longer than the ordinary throttle"
    );

    // A settle ending short of the fallback, on a clock held still so its 20 ms cannot lapse
    // before the echo comes: the echo's park is the settle's own deadline.
    let now = Instant::now();
    SETTLE_NOW_FOR_TEST.with(|clock| clock.set(Some(now)));
    let intended = arm_settle(&mut app, 24, 80, 3);
    let deadline = now + std::time::Duration::from_millis(20);
    app.windows
        .get_mut(&wid)
        .and_then(|ws| ws.initial_frame_settle.as_mut())
        .expect("armed")
        .deadline = deadline;
    app.apply_window_resized_grid(wid, intended);
    app.apply_window_resized_grid(wid, echo);
    SETTLE_NOW_FOR_TEST.with(|clock| clock.set(None));
    assert_eq!(
        app.windows.get(&wid).expect("window 0").next_resize_settle,
        Some(deadline),
        "the park never outlives the settle"
    );
}

#[test]
fn a_synchronous_settle_grant_applies_the_granted_size_in_place_of_the_echo() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let intended = arm_settle(&mut app, 24, 80, 3);
    let echo = PhysicalSize::new(intended.width, intended.height - 35);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let rows = |app: &App| {
        (
            term.lock().expect("term").rows(),
            app.windows.get(&wid).expect("window 0").rows,
        )
    };
    app.apply_window_resized_grid(wid, intended);
    assert_eq!(rows(&app), (24, 24));
    let deadline = settle_state(&app).expect("armed").deadline;
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.pending_resize = Some(echo);
        ws.next_resize_settle = Some(Instant::now());
    }

    app.settle_take_over(wid, echo, Some(intended), deadline);
    let ws = app.windows.get(&wid).expect("window 0");
    assert_eq!(
        ws.pending_resize, None,
        "the grant cancels any parked reflow"
    );
    assert_eq!(ws.next_resize_settle, None);
    assert_eq!(ws.win_px, Some(intended), "the applied size is recorded");
    assert_eq!(
        rows(&app),
        (24, 24),
        "the engine never sees the echo on a synchronous grant"
    );

    let tiled = PhysicalSize::new(intended.width, intended.height + 200);
    app.settle_take_over(wid, echo, Some(tiled), deadline);
    assert_eq!(app.windows.get(&wid).expect("window 0").win_px, Some(tiled));
    assert_eq!(
        rows(&app).1,
        app.grid_dims_for(wid, tiled).0,
        "negative control: the granted size really drives the grid"
    );
    assert_ne!(rows(&app), (24, 24));
}

#[test]
#[cfg(target_os = "linux")]
fn a_driven_grid_resize_disarms_the_initial_frame_settle() {
    let mut app = App::headless_for_test();
    arm_settle(&mut app, 24, 80, 3);
    app.apply_grid_resize(30, 100);
    assert!(
        settle_state(&app).is_none(),
        "apply_grid_resize must clear the settle before requesting its own size"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn xtwinops_manipulations_post_a_window_op_wake_and_reports_post_nothing() {
    use aterm_types::WindowOperation as W;
    let manipulations = [
        W::DeIconify,
        W::Iconify,
        W::RaiseWindow,
        W::LowerWindow,
        W::RefreshWindow,
        W::ResizeWindowPixels {
            height: 300,
            width: 500,
        },
        W::ResizeWindowCells { rows: 30, cols: 90 },
        W::RestoreMaximized,
        W::MaximizeWindow,
        W::MaximizeVertically,
        W::MaximizeHorizontally,
        W::UndoFullscreen,
        W::EnterFullscreen,
        W::ToggleFullscreen,
    ];
    for op in manipulations {
        match crate::spawn::window_op_wake(op.clone(), 7, WindowId(3)) {
            Some(crate::Wake::WindowOp {
                session,
                window,
                op: routed,
            }) => {
                assert_eq!(session, 7, "{op:?} must stamp the requesting session");
                assert_eq!(window, WindowId(3), "{op:?} must stamp the owner window");
                assert_eq!(routed, op, "the operation must survive the post unchanged");
            }
            other => panic!("{op:?} must post a WindowOp wake, got {other:?}"),
        }
    }
    let silent = [
        W::MoveWindow { x: 10, y: 10 },
        W::ReportWindowState,
        W::ReportWindowPosition,
        W::ReportTextAreaPosition,
        W::ReportTextAreaSizePixels,
        W::ReportWindowSizePixels,
        W::ReportScreenSizePixels,
        W::ReportCellSizePixels,
        W::ReportTextAreaSizeCells,
        W::ReportScreenSizeCells,
        W::ReportIconLabel,
        W::ReportWindowTitle,
        W::PushTitle {
            icon: true,
            window: true,
        },
        W::PopTitle {
            icon: true,
            window: true,
        },
    ];
    for op in silent {
        assert!(
            crate::spawn::window_op_wake(op.clone(), 7, WindowId(3)).is_none(),
            "{op:?} must not ride the wake"
        );
    }
}

#[test]
fn a_cell_resize_is_honored_only_for_the_focused_session() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let focused = app.split_active_stub_tab(wid);
    assert_ne!(focused, 0, "the split spawns and focuses a second session");
    let dims = |app: &App| {
        let ws = &app.windows[&wid];
        (ws.rows, ws.cols)
    };
    let before = dims(&app);

    app.on_window_op(
        wid,
        0,
        aterm_types::WindowOperation::ResizeWindowCells { rows: 30, cols: 90 },
    );
    assert_eq!(
        dims(&app),
        before,
        "a background pane must not re-grid the window"
    );

    app.on_window_op(
        wid,
        focused,
        aterm_types::WindowOperation::ResizeWindowCells { rows: 30, cols: 90 },
    );
    assert_eq!(dims(&app), (30, 90), "the focused session re-grids");
}

/// `resize px` on a headless window (no window manager to ask) re-grids it from the pixels
/// as a drag's `Resized` does, where it once answered OK and changed nothing; with no window
/// to resize it answers an ERR, never OK.
#[test]
fn a_headless_window_takes_the_pixel_size_it_is_asked_for() {
    use crate::input::{InputEvent, InputOutcome, Source};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let grid = |app: &App| (app.windows[&wid].rows, app.windows[&wid].cols);
    assert_ne!(grid(&app), (30, 50), "precondition: another grid");
    let frame = app.window_frame_px_for(wid, 30, 50);
    let asked = InputEvent::ResizeWindowPx {
        width: frame.width,
        height: frame.height,
    };
    assert_eq!(
        app.input(wid, asked, Source::Controller),
        InputOutcome::Resized(crate::input::WindowResize::Px {
            asked: (frame.width, frame.height),
            given: (frame.width, frame.height),
        })
    );
    assert_eq!(grid(&app), (30, 50), "the grid follows the pixels");
    let (cw, ch) = app.win_cell_size(wid);
    let between = app.window_frame_px_for(wid, 20, 40);
    let asked = InputEvent::ResizeWindowPx {
        width: between.width + cw as u32 - 1,
        height: between.height + ch as u32 - 1,
    };
    // Past the throttle window, so this one is not coalesced into a later settle.
    app.windows.get_mut(&wid).unwrap().last_resize_at = None;
    assert!(matches!(
        app.input(wid, asked, Source::Controller),
        InputOutcome::Resized(_)
    ));
    assert_eq!(
        grid(&app),
        (20, 40),
        "pixels a cell does not fill leave the grid that fits"
    );
    assert!(matches!(
        app.resize_window_to_px(WindowId(77), PhysicalSize::new(800, 600)),
        InputOutcome::NotApplied(_)
    ));
}

/// A program's pixel resize (XTWINOPS `CSI 4 ; h ; w t`) re-grids a headless window too.
#[test]
fn a_window_op_pixel_resize_re_grids_a_headless_window() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let frame = app.window_frame_px_for(wid, 18, 44);
    app.on_window_op(
        wid,
        0,
        aterm_types::WindowOperation::ResizeWindowPixels {
            height: u16::try_from(frame.height).unwrap(),
            width: u16::try_from(frame.width).unwrap(),
        },
    );
    let ws = &app.windows[&wid];
    assert_eq!((ws.rows, ws.cols), (18, 44));
}

/// A tab brought back to the front shows its terminal at its pane's grid, though a
/// cross-session `resize` re-gridded the session's engine while the tab was behind (it once
/// stayed at that grid, and a headless window, which draws no frame, never laid it out).
#[test]
fn a_tab_brought_to_the_front_shows_its_terminal_at_its_panes_grid() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = (app.windows[&wid].rows, app.windows[&wid].cols);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let term = app.pool.get(0).expect("session 0").term.clone();
    // What `@0 resize 10 30` does from the control thread, and the wake it posts.
    crate::term_lock(&term).resize(10, 30);
    app.owe_pane_regrid(0);
    app.switch_tab_in(wid, 0);
    let engine = {
        let t = crate::term_lock(&term);
        (t.rows(), t.cols())
    };
    assert_eq!(engine, window, "the pane's grid, not the one set behind");
}

/// A SESSION A WINDOW LAYS OUT AS A PANE TAKES ITS GRID FROM THAT WINDOW, AND `pane_grid_of`
/// SAYS FOR HOW LONG: behind Settings, the terminal's tab is behind, so a grid set behind its
/// pane lasts until the tab comes to the front, when the window gives it the grid named; with
/// the tab in front the window lays the pane out at its next frame (a cross-session `resize`
/// refuses); a session no window lays out is named by none, and its grid is the resize's.
/// NEGATIVE CONTROL: the cross-session resize answered a bare `OK` for a grid the window undid
/// as the tab came to the front.
#[test]
fn a_session_a_window_lays_out_names_its_window_its_panes_grid_and_whether_it_is_in_front() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let window = (app.windows[&wid].rows, app.windows[&wid].cols);
    assert_eq!(
        app.pane_grid_of(0),
        Some(crate::app_render::SessionPane {
            window: wid,
            grid: window,
            in_front: false,
        })
    );
    app.switch_tab_in(wid, 0);
    assert_eq!(
        app.pane_grid_of(0),
        Some(crate::app_render::SessionPane {
            window: wid,
            grid: window,
            in_front: true,
        })
    );
    assert_eq!(app.pane_grid_of(9_999), None);
}

/// `dims` WITH AN APP TAB IN FRONT READS THE WINDOW: with Settings in front and no session
/// named, the snapshot is the front window's own grid and geometry, `session=none`, so a
/// script reads back what a resize gave the window (it answered `ERR no active terminal`).
#[test]
fn dims_with_an_app_tab_in_front_reads_the_window() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let snapshot = app.window_dims_snapshot().expect("a front window");
    let grid = (
        u32::from(app.windows[&wid].rows),
        u32::from(app.windows[&wid].cols),
    );
    assert_eq!((snapshot.rows, snapshot.cols), grid);
    assert_eq!((snapshot.window_rows, snapshot.window_cols), grid);
    assert_eq!((snapshot.session, snapshot.window), (None, Some(0)));
    let line = crate::control::serialize_dims(&snapshot);
    assert!(
        line.starts_with(&format!("OK {} {} ", grid.0, grid.1)),
        "{line}"
    );
    assert!(line.contains(" session=none "), "{line}");
}

#[test]
fn a_window_op_re_resolves_the_stale_spawn_stamp() {
    let mut app = App::headless_for_test();
    let sid = app.next_session_id;
    let wid_b = app.insert_logical_window(crate::stub_session(sid), 24, 80);

    assert_eq!(app.window_op_target(sid, WindowId(0)), Some(wid_b));
    assert_eq!(app.window_op_target(sid, WindowId(77)), Some(wid_b));
    assert_eq!(app.window_op_target(9999, WindowId(0)), Some(WindowId(0)));
    assert_eq!(app.window_op_target(9999, WindowId(77)), None);

    app.on_window_op(
        WindowId(0),
        sid,
        aterm_types::WindowOperation::ResizeWindowCells { rows: 30, cols: 90 },
    );
    let dims = |app: &App, wid: WindowId| {
        let ws = &app.windows[&wid];
        (ws.rows, ws.cols)
    };
    assert_eq!(
        dims(&app, wid_b),
        (30, 90),
        "the operation lands on the window the session lives in"
    );
    assert_eq!(
        dims(&app, WindowId(0)),
        (24, 80),
        "the stamped window is untouched"
    );

    app.on_window_op(
        WindowId(77),
        9999,
        aterm_types::WindowOperation::ResizeWindowCells { rows: 50, cols: 50 },
    );
    assert_eq!(dims(&app, wid_b), (30, 90));
    assert_eq!(dims(&app, WindowId(0)), (24, 80));
}
