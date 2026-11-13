// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use crate::App;
use crate::native_app::{ActionInvocation, AppEffect, AppEvent, SemanticInput};
use crate::native_ui::ActionId;
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::trail_sound::{SoundGesture, SoundKind, SoundVoice};

#[test]
fn an_idle_pass_and_a_frame_of_the_clock_keep_the_chrome_decided() {
    use crate::native_app::AppViewState;
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(1_078, 360));
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).expect("Settings active");
    let content = |app: &App| match app.native_runtime.view_state(view) {
        Some(AppViewState::Settings(state)) => state.common.content_epoch,
        _ => unreachable!(),
    };
    let remembered = || crate::native_settings::CHROME_REMEMBERED.with(std::cell::Cell::get);
    for _ in 0..3 {
        app.sync_settings_views();
        let _ = app.compiled_native_ui(wid).expect("a page");
    }
    let (before, decided) = (content(&app), remembered());
    for step in 0..4 {
        app.sync_settings_views();
        app.on_cursor_moved(wid, 4.0 + f64::from(step), 4.0);
        let _ = app.compiled_native_ui(wid).expect("a page");
    }
    assert_eq!(content(&app), before, "nothing the page shows changed");
    assert_eq!(
        remembered() - decided,
        4,
        "each frame took the chrome decided"
    );
}

#[test]
fn settings_session_runtime_is_published_from_local_state() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::WindowTabs));
    let (_, view) = app.active_native_view(crate::WindowId(0)).unwrap();
    let expected = crate::native_settings::SessionRuntime {
        desk: app.desk,
        journal: app.session_log_journal.status(),
        keeper: crate::keeper_link::status(),
    };
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        unreachable!()
    };
    state.session_runtime = None;
    app.sync_settings_session_runtime();
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        unreachable!()
    };
    assert_eq!(state.session_runtime, Some(expected));
    assert_eq!(
        expected.journal.phase, None,
        "headless fixture starts no writer"
    );
}

#[test]
fn askpass_settings_host_reports_a_failed_private_marker_operation() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let marker = dir.path().join("askpass-off");
    std::fs::create_dir(&marker).unwrap();
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
    let (_, view) = app.active_native_view(wid).unwrap();
    app.apply_askpass_off_requests(
        vec![(
            wid,
            view,
            false,
            crate::native_settings::OwnerYes::for_test(),
        )],
        |off, _owner| {
            assert!(!off);
            std::fs::remove_file(&marker)
        },
    );
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        unreachable!();
    };
    let said = state.feedback.as_deref().unwrap();
    assert!(said.starts_with("Could not change"), "{said}");
    assert!(!said.contains("allowed again"), "{said}");
    assert!(marker.is_dir(), "the relay still has its off entry");
}

#[test]
fn askpass_settings_host_supersedes_a_second_direction_without_reporting_it_saved() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
    let (instance, first) = app.active_native_view(wid).unwrap();
    let second = app.view_store.insert_native(instance).unwrap();
    let mut state =
        crate::native_settings::SettingsViewState::new(&crate::app_config::Config::default());
    state.navigate(crate::native_settings::SettingsRoute::Security);
    app.native_runtime
        .attach_view(
            second,
            instance,
            crate::native_app::AppViewState::Settings(Box::new(state)),
        )
        .unwrap();
    let mut writes = Vec::new();
    app.apply_askpass_off_requests(
        vec![
            (
                wid,
                first,
                true,
                crate::native_settings::OwnerYes::for_test(),
            ),
            (
                wid,
                second,
                false,
                crate::native_settings::OwnerYes::for_test(),
            ),
        ],
        |off, _owner| {
            writes.push(off);
            Ok(())
        },
    );
    assert_eq!(writes, vec![true], "only the first confirmed request runs");
    let Some(crate::native_app::AppViewState::Settings(first)) =
        app.native_runtime.view_state(first)
    else {
        unreachable!();
    };
    assert!(first.feedback.as_deref().unwrap().contains("turned off"));
    let Some(crate::native_app::AppViewState::Settings(second)) =
        app.native_runtime.view_state(second)
    else {
        unreachable!();
    };
    let said = second.feedback.as_deref().unwrap();
    assert!(said.contains("this request did not run"), "{said}");
    assert!(!said.contains("allowed again"), "{said}");
}

#[derive(Clone, Copy, Debug)]
enum CursorDependent {
    Rainbow,
    Droplet,
    BeamRod,
    Comet,
    Phaser,
    Trail,
    Cat,
    WordDecoration,
}

const CURSOR_DEPENDENTS: [CursorDependent; 8] = [
    CursorDependent::Rainbow,
    CursorDependent::Droplet,
    CursorDependent::BeamRod,
    CursorDependent::Comet,
    CursorDependent::Phaser,
    CursorDependent::Trail,
    CursorDependent::Cat,
    CursorDependent::WordDecoration,
];

fn activate_non_word_dependent(ws: &mut crate::WindowState, effect: CursorDependent, now: Instant) {
    let geom = crate::geom_24x80_for_test();
    let cur = Some((2, 4));
    let mut quads = Vec::new();
    match effect {
        CursorDependent::Rainbow => {
            ws.fx.cursor_mut().rainbow.tick(
                cur,
                now,
                1.0,
                true,
                true,
                geom,
                &crate::cursor_rainbow::RainbowConfig {
                    light: aterm_effects::cursor_glow::Light::OFF,
                    enabled: true,
                    intensity: 1.0,
                    blinking: true,
                    base: None,
                    head_rgb: None,
                    paint: None,
                    ground: None,
                    flare_at: None,
                },
                &mut quads,
            );
            assert!(ws.fx.cursor_mut().rainbow.is_active());
        }
        CursorDependent::Droplet => {
            ws.fx.cursor_mut().droplet.tick(
                cur,
                now,
                1.0,
                geom,
                &crate::cursor_droplet::DropletConfig {
                    enabled: true,
                    intensity: 1.0,
                    ground: None,
                },
                &mut quads,
            );
            assert!(ws.fx.cursor_mut().droplet.is_active());
        }
        CursorDependent::BeamRod => {
            ws.fx.cursor_mut().beamrod.tick(
                cur,
                now,
                1.0,
                geom,
                &crate::cursor_beam::BeamRodConfig {
                    light: aterm_effects::cursor_glow::Light::OFF,
                    enabled: true,
                    intensity: 1.0,
                    color: 0x00FF_44CC,
                    haze: 0x0022_1144,
                    bar: false,
                    shimmer: false,
                    dark_theme: true,
                    theme_fg: 0x00C8_D3F5,
                    theme_bg: 0x001A_1B26,
                    pinned: false,
                    chosen: false,
                },
                &mut quads,
                &mut Vec::new(),
            );
            assert!(ws.fx.cursor_mut().beamrod.is_active());
        }
        CursorDependent::Comet => {
            ws.fx.cursor_mut().comet.tick(
                cur,
                now,
                1.0,
                geom,
                &crate::cursor_comet::CometConfig {
                    light: aterm_effects::cursor_glow::Light::OFF,
                    enabled: true,
                    intensity: 1.0,
                    color: 0x0044_CCFF,
                    accent: 0x00DD_FFFF,
                    dark_theme: true,
                    theme_fg: 0x00C8_D3F5,
                    theme_bg: 0x001A_1B26,
                    pinned: false,
                    chosen: false,
                },
                &mut quads,
                &mut Vec::new(),
            );
            assert!(ws.fx.cursor_mut().comet.is_active());
        }
        CursorDependent::Phaser => {
            ws.fx.cursor_mut().phaser.tick(
                cur,
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
                &mut quads,
            );
            assert!(ws.fx.cursor_mut().phaser.is_active());
        }
        CursorDependent::Trail => {
            let cfg = crate::cursor_trail::TrailConfig {
                enabled: true,
                duration: Duration::from_secs(1),
                max_len: 12,
                color: 0x00FF_44CC,
                intensity: 0.0,
                warmth: 0.0,
            };
            let mut cells = Vec::new();
            ws.fx
                .cursor_mut()
                .trail
                .tick(Some((2, 1)), now, &cfg, &mut cells);
            ws.fx.cursor_mut().trail.note_synthetic_move(now);
            ws.fx.cursor_mut().trail.tick(cur, now, &cfg, &mut cells);
            assert!(ws.fx.cursor_mut().trail.is_active());
        }
        CursorDependent::Cat => {
            ws.fx
                .companions_mut()
                .cursor_cat
                .on_collect(now, aterm_effects::kitty_registry::KittyLook::default());
            assert!(ws.fx.companions_mut().cursor_cat.is_active());
        }
        CursorDependent::WordDecoration => {
            panic!("word decorations need the terminal snapshot helper")
        }
    }
}

fn activate_word_decoration(app: &mut App, now: Instant) {
    let wid = crate::WindowId(0);
    let terminal = app
        .front_terminal(wid)
        .expect("terminal fixture")
        .term
        .clone();
    {
        let ws = app.windows.get_mut(&wid).expect("window 0");
        ws.pending_deco_birth = Some(now);
        let mut term = crate::term_lock(&terminal);
        term.process(b"\r\nhello kitty friend");
        term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
    }
    app.capture_words(wid, now);
    assert!(app.windows[&wid].fx.decos().is_active(now));
}

fn activate_dependent(app: &mut App, effect: CursorDependent, now: Instant) {
    if matches!(effect, CursorDependent::WordDecoration) {
        activate_word_decoration(app, now);
    } else {
        activate_non_word_dependent(
            app.windows.get_mut(&crate::WindowId(0)).expect("window 0"),
            effect,
            now,
        );
    }
}

fn activate_all_terminal_dependents(app: &mut App, now: Instant) {
    activate_dependent(app, CursorDependent::WordDecoration, now);
    for effect in CURSOR_DEPENDENTS {
        if matches!(effect, CursorDependent::WordDecoration) {
            continue;
        }
        activate_dependent(app, effect, now);
    }
    let ws = app.windows.get_mut(&crate::WindowId(0)).expect("window 0");
    ws.next_trail_tick = Some(now + Duration::from_millis(16));
    ws.last_trail_fire = Some(now);
    ws.note_deco_animating(now);
    assert!(ws.cursor_fx_active(now, true));
    assert!(ws.terminal_effect_frame_active(now, true));
    assert!(ws.deco_anim_frame_active(now));
}

fn assert_native_effect_scheduler_parked(app: &App, now: Instant) {
    let ws = &app.windows[&crate::WindowId(0)];
    assert!(
        ws.front_terminal().is_none(),
        "native front owns no terminal"
    );
    assert!(ws.fx.cursor().rainbow.is_active());
    assert!(ws.fx.cursor().droplet.is_active());
    assert!(ws.fx.cursor().beamrod.is_active());
    assert!(ws.fx.cursor().comet.is_active());
    assert!(ws.fx.cursor().phaser.is_active());
    assert!(ws.fx.companions().cursor_cat.is_active());
    assert!(ws.fx.decos().is_active(now));
    assert!(!ws.cursor_fx_active(now, true));
    assert!(!ws.cursor_fx_active(now, false));
    assert!(!ws.terminal_effect_frame_active(now, true));
    assert_eq!(ws.static_cursor_cat_deadline(now, false, false), None);
    assert!(!ws.deco_anim_frame_active(now));
    assert_eq!(ws.next_trail_tick, None);
    assert_eq!(ws.last_trail_fire, None);
}

#[test]
fn every_cursor_dependent_uses_the_shared_frame_cadence_set() {
    for effect in CURSOR_DEPENDENTS {
        let mut app = App::headless_for_test();
        let now = Instant::now();
        activate_dependent(&mut app, effect, now);
        let ws = app.windows.get_mut(&crate::WindowId(0)).expect("window 0");
        if matches!(effect, CursorDependent::WordDecoration) {
            ws.fx.companions_mut().cursor_cat = crate::kitty_cursor::CursorCat::default();
        }
        let dependents = ws.cursor_dependents_need_frame_cadence(now, true);
        assert!(dependents, "{effect:?} must request frame cadence");
        assert_eq!(
            ws.cursor_fx_active(now, true),
            ws.fx.cursor_mut().glow.is_active() || dependents,
            "activity/cadence equality drifted for {effect:?}"
        );
        assert!(
            ws.terminal_effect_frame_active(now, true),
            "{effect:?} must reach the real scheduler predicate"
        );
    }
}

#[test]
fn same_terminal_sync_preserves_live_effects_and_deadline() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let now = Instant::now();
    activate_dependent(&mut app, CursorDependent::Trail, now);
    let deadline = now + Duration::from_millis(16);
    app.windows.get_mut(&wid).expect("window 0").next_trail_tick = Some(deadline);

    let front = app.windows[&wid].front_content;
    app.sync_window(wid);

    let ws = &app.windows[&wid];
    assert_eq!(
        ws.front_content, front,
        "ordinary sync kept the same terminal"
    );
    assert!(
        ws.fx.cursor().trail.is_active(),
        "ordinary sync must not wipe live effects"
    );
    assert!(ws.cursor_fx_active(now, true));
    assert_eq!(
        ws.next_trail_tick,
        Some(deadline),
        "ordinary terminal bookkeeping preserves the armed cadence"
    );
}

#[test]
fn a_headless_security_page_publishes_no_consent_posture_and_touches_no_probe() {
    let mut app = App::headless_for_test();
    assert!(app.headless, "the guard under test is the headless one");
    assert!(
        app.open_settings_tab(crate::native_settings::SettingsRoute::Security),
        "the Security page opens headless like any other route"
    );
    app.sync_settings_consent_posture();

    let (_, _, view, _) = app
        .native_settings_view_target()
        .expect("the Security view is live");
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        unreachable!()
    };
    assert_eq!(
        state.route,
        crate::native_settings::SettingsRoute::Security,
        "the view really is on the page that carries the block"
    );
    assert!(
        state.macos_access_for_test().is_none(),
        "a headless instance publishes no posture, so there is nothing to press"
    );
}

#[test]
fn settings_and_about_native_routes_park_terminal_effect_scheduler() {
    for route in [
        crate::native_settings::SettingsRoute::Home,
        crate::native_settings::SettingsRoute::About,
    ] {
        let mut app = App::headless_for_test();
        let now = Instant::now();
        activate_all_terminal_dependents(&mut app, now);
        assert!(app.open_settings_tab(route), "open native route {route:?}");
        assert_native_effect_scheduler_parked(&app, now);
    }
}

#[test]
fn generic_native_tab_parks_terminal_effect_scheduler() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let now = Instant::now();
    activate_all_terminal_dependents(&mut app, now);
    app.install_new_native_tab(
        wid,
        crate::native_app::NativeApp::Recovery(crate::native_app::RecoveryApp {
            restore_tag: "scheduler-test".to_string(),
            reason: "native cadence regression".to_string(),
            metadata: String::new(),
            capability: None,
        }),
        crate::native_app::AppViewState::Recovery(crate::native_app::RecoveryViewState::default()),
        crate::tab_model::TabPresentation {
            title: "Native".to_string(),
            icon: Some(crate::tab_model::TabIconKind::Recovery),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: None,
        },
    )
    .expect("install generic native tab");
    assert_native_effect_scheduler_parked(&app, now);
}

fn emitted_settings_patch_base(app: &mut App) -> u64 {
    let (instance, view) = app
        .active_native_view(crate::WindowId(0))
        .expect("active Settings view");
    let outcome = app
        .native_runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(format!(
                    "settings/set/{}",
                    crate::prefs::EDIT_COPY_ON_SELECT
                )),
                value: Some(SemanticInput::Bool(false)),
            }),
        )
        .expect("Settings action dispatches");
    outcome
        .effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::ConfigPatch { patch, .. } => Some(patch.base_revision),
            _ => None,
        })
        .expect("Settings emits a config patch")
}

#[test]
fn settings_reopen_after_external_change_uses_the_snapshot_revision() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::KeyboardInput));
    assert!(app.close_settings_tabs());

    app.native_config_service
        .replace_external("copy_on_select = true\n".to_string())
        .expect("valid external config");
    let revision = app.native_config_service.snapshot().revision;
    assert!(revision > 1);

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::KeyboardInput));
    assert_eq!(emitted_settings_patch_base(&mut app), revision);
}

#[test]
fn native_settings_tab_reuses_identity_and_never_fabricates_a_terminal() {
    const ROOT: &str = "ATERM_NATIVE_SETTINGS_TAB_IDENTITY_ROOT";
    if !crate::test_alone::in_child() {
        let root = aterm_tempfile::TempDir::new().unwrap();
        crate::test_alone::run_alone_with(&[
            (ROOT, root.path().as_os_str()),
            ("XDG_CONFIG_HOME", root.path().as_os_str()),
        ]);
        return;
    }

    let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let (terminal_tabs, terminal_layouts, sessions) = {
        let ws = app.windows.get(&wid).unwrap();
        (ws.tabs.count, ws.layouts.len(), app.pool.sessions.len())
    };

    let mut first_tab = None;
    for route in crate::native_settings::SettingsRoute::ALL {
        assert!(app.open_settings_tab(route), "open native route {route:?}");
        let active_tab = app
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_id())
            .expect("Settings tab");
        let canonical_tab = *first_tab.get_or_insert(active_tab);
        assert_eq!(
            active_tab, canonical_tab,
            "every pane reuses the same Settings presentation"
        );
        let (instance, _) = app
            .active_native_view(wid)
            .expect("active native Settings view");
        assert_eq!(
            app.native_runtime.app(instance).map(|native| native.kind()),
            Some(crate::native_app::AppKind::Settings),
            "{route:?} stays inside the native Settings app"
        );
        assert!(app.prepare_native_input_scratch(wid), "render {route:?}");
        let ws = app.windows.get(&wid).unwrap();
        assert!(ws.front_terminal().is_none(), "{route:?} owns no PTY");
        assert!(ws.settings_card.is_some(), "{route:?} compiled native UI");
        assert_eq!(ws.tabs.count, terminal_tabs);
        assert_eq!(ws.layouts.len(), terminal_layouts);
        assert_eq!(
            app.pool.sessions.len(),
            sessions,
            "native Settings creates no Session"
        );
    }

    assert!(
        app.native_runtime.config_editor_document().is_none(),
        "going to the Manual page opens no editor"
    );
    app.open_config_editor_in_front()
        .expect("Open aterm.toml opens the editor");
    assert!(
        root.join("aterm/aterm.toml").is_file(),
        "Manual must resolve only the isolated config file"
    );
    let (manual_instance, _) = app.active_native_view(wid).expect("Manual editor tab");
    assert_eq!(
        app.native_runtime
            .app(manual_instance)
            .map(|native| native.kind()),
        Some(crate::native_app::AppKind::Editor)
    );
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert_eq!(
        app.windows.get(&wid).and_then(|ws| ws.tab_set.active_id()),
        first_tab,
        "returning from Manual reuses the canonical Settings tab"
    );
    assert!(app.settings_tab_open());

    assert!(app.close_settings_tabs());
    assert!(!app.settings_tab_open());
    assert_eq!(app.windows.get(&wid).unwrap().tabs.count, terminal_tabs);
    assert_eq!(
        app.windows.get(&wid).unwrap().layouts.len(),
        terminal_layouts
    );
    assert_eq!(app.pool.sessions.len(), sessions);
}

#[test]
fn a_finished_move_reaches_the_projection() {
    let mut app = App::headless_for_test();
    let (tx, rx) = std::sync::mpsc::channel();
    assert!(app.claimant_retire.start(
        "x".to_string(),
        vec![std::path::PathBuf::from("/A/x.app.rollback")],
        move || {
            let _ = tx.send(());
        },
    ));
    rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the worker finished");
    app.claimant_retire_refusal =
        Some("a move is already under way, or its worker could not start");
    let before = app.consent.census_generation_for_test();
    app.on_claimant_retired();
    assert_eq!(
        app.claimant_retire_refusal, None,
        "the move's end supersedes it"
    );
    assert!(
        !app.claimant_retire.take_arrival(),
        "the wake folded the arrival"
    );
    assert_eq!(
        app.consent.census_generation_for_test(),
        before + 1,
        "and invalidated the census, since the disk changed"
    );
    let access = app.macos_access_projection(true);
    assert_eq!(
        access.retired,
        Some(vec![(
            std::path::PathBuf::from("/A/x.app.rollback"),
            aterm_containment::consent::Retired::NotOffered
        )]),
        "the inert census offers nothing, so it stayed"
    );

    app.claimant_retire.set_report_for_test(vec![(
        std::path::PathBuf::from("/A/x.app.rollback"),
        aterm_containment::consent::Retired::Moved,
    )]);
    let moved = app.macos_access_projection(true);
    assert_eq!(
        moved.retired,
        Some(vec![(
            std::path::PathBuf::from("/A/x.app.rollback"),
            aterm_containment::consent::Retired::Moved
        )])
    );
}

#[test]
fn a_published_move_renders_on_the_security_page() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
    let (_, view) = app.active_native_view(wid).expect("Settings view");
    app.claimant_retire.set_report_for_test(vec![(
        std::path::PathBuf::from("/A/x.app.rollback"),
        aterm_containment::consent::Retired::Moved,
    )]);
    let access = app.macos_access_projection(true);
    app.publish_macos_access(
        &[(wid, view)],
        &access,
        crate::native_settings::ConsentGestures::inert(),
    );
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("a Settings view");
    };
    let published = state.macos_access_for_test().expect("published");
    assert_eq!(
        published.retired,
        Some(vec![(
            std::path::PathBuf::from("/A/x.app.rollback"),
            aterm_containment::consent::Retired::Moved
        )]),
        "the view holds the move the projection carried"
    );
}

#[test]
fn a_consent_publication_repaints_only_the_changed_security_view() {
    let mut app = App::headless_for_test();
    let security = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
    let (_, view) = app.active_native_view(security).expect("Settings view");
    let terminal = crate::WindowId(1);
    app.install_window_state(terminal, crate::stub_session(1), 24, 80);
    let windows = [security, terminal];
    let publish = |app: &mut App, access: &crate::native_settings::MacosAccess| {
        app.publish_macos_access(
            &[(security, view)],
            access,
            crate::native_settings::ConsentGestures::inert(),
        );
    };
    let present = |app: &mut App| {
        let mut term = aterm_core::terminal::Terminal::new(2, 3);
        let key = crate::early_out_tests::frame_key(&mut term, false, false, None);
        for wid in windows {
            app.windows
                .get_mut(&wid)
                .expect("window")
                .stamp_present_decision(key);
        }
    };
    let presented = |app: &App| windows.map(|wid| app.windows[&wid].last_present.is_some());

    let before = app.macos_access_projection(true);
    publish(&mut app, &before);
    present(&mut app);
    let after = crate::native_settings::MacosAccess {
        fda: if before.fda == aterm_containment::FdaState::Granted {
            aterm_containment::FdaState::Denied
        } else {
            aterm_containment::FdaState::Granted
        },
        ..before.clone()
    };
    publish(&mut app, &after);
    assert_eq!(
        presented(&app),
        [false, true],
        "the changed Security view repaints; the terminal window keeps its frame"
    );
    present(&mut app);
    publish(&mut app, &after);
    assert_eq!(
        presented(&app),
        [true, true],
        "an unchanged block repaints nothing"
    );
}

#[test]
fn a_hidden_security_page_starts_no_fda_or_devtools_probe() {
    use crate::consent_state::SlotKind;
    use aterm_containment::{DevToolProbe, DevToolProbeLabel, FdaProbe, ProbeGate};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DEVTOOLS: AtomicUsize = AtomicUsize::new(0);
    static FDA: AtomicUsize = AtomicUsize::new(0);
    fn devtools(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        DEVTOOLS.fetch_add(1, Ordering::AcqRel);
        DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest)
    }
    fn fda(_gate: ProbeGate) -> FdaProbe {
        FDA.fetch_add(1, Ordering::AcqRel);
        aterm_containment::consent::classify_probe(aterm_containment::ProbeOutcome::Ok)
    }
    fn subject() -> Option<&'static str> {
        Some("com.example.subject")
    }
    let mut app = App::headless_for_test();
    app.consent.set_devtools_arms_for_test(devtools, subject);
    app.consent.set_fda_arm_for_test(fda);
    let (tx, rx) = std::sync::mpsc::channel();
    app.consent
        .set_slot_wake(std::sync::Arc::new(move |kind: SlotKind| {
            let _ = tx.send(kind);
        }));
    let runs = || {
        [
            FDA.load(Ordering::Acquire),
            DEVTOOLS.load(Ordering::Acquire),
        ]
    };
    let before = runs();
    let ran = || {
        let now = runs();
        [now[0] - before[0], now[1] - before[1]]
    };
    let idle = |app: &App| {
        app.consent.fda_slot_idle_for_test() && app.consent.devtools_slot_idle_for_test()
    };
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
    let (settings, _, _, view) = app.settings_tab_in_window(wid).expect("a Settings tab");
    let publish = |app: &mut App| {
        for _ in 0..3 {
            app.publish_security_page(vec![(wid, view)]);
        }
    };

    let other = (0..app.windows.get_mut(&wid).expect("window 0").tab_set.len())
        .find(|&index| index != settings)
        .expect("the window holds another tab");
    assert!(
        app.windows
            .get_mut(&wid)
            .expect("window 0")
            .tab_set
            .switch_to_index(other)
    );
    assert!(!app.settings_view_on_screen(wid, view));
    publish(&mut app);
    assert_eq!(ran(), [0, 0], "a background tab ran one");
    assert!(idle(&app));

    assert!(
        app.windows
            .get_mut(&wid)
            .expect("window 0")
            .tab_set
            .switch_to_index(settings)
    );
    app.windows.get_mut(&wid).expect("window 0").occluded = true;
    assert!(!app.settings_view_on_screen(wid, view));
    publish(&mut app);
    assert_eq!(ran(), [0, 0], "an occluded window ran one");
    assert!(idle(&app));

    app.windows.get_mut(&wid).expect("window 0").occluded = false;
    assert!(app.settings_view_on_screen(wid, view));
    publish(&mut app);
    let mut published = Vec::new();
    while !(published.contains(&SlotKind::Fda) && published.contains(&SlotKind::Devtools)) {
        published.push(
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the workers publish"),
        );
    }
    publish(&mut app);
    assert_eq!(ran(), [1, 1]);
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("a Settings view");
    };
    let access = state.macos_access_for_test().expect("published");
    assert_eq!(
        (access.fda, access.devtools.probe.label),
        (
            aterm_containment::FdaState::Granted,
            DevToolProbeLabel::NoRecentRequest
        ),
        "the page reads the answers it got"
    );
}

#[test]
fn a_confirmed_warm_up_that_starts_nothing_says_why() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
    let (_, view) = app.active_native_view(wid).expect("Settings view");
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        panic!("a Settings view");
    };
    state.record_consent_warmup_for_test();
    app.sync_settings_consent_posture();
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        panic!("a Settings view");
    };
    assert!(!state.take_consent_warmup_request(), "drained");
    let why = crate::consent_warmup::StartOutcome::Refused
        .refusal()
        .expect("a refusal");
    assert!(
        state
            .feedback
            .as_deref()
            .is_some_and(|said| said.contains(why)),
        "the view says why: {:?}",
        state.feedback
    );
}

#[test]
fn a_confirmed_debugger_change_on_a_headless_instance_starts_nothing_and_says_why() {
    use aterm_containment::consent::DebuggerChange;
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
    let (_, view) = app.active_native_view(wid).expect("Settings view");
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        panic!("a Settings view");
    };
    state.record_debugger_change_for_test(DebuggerChange::On);
    app.sync_settings_consent_posture();
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        panic!("a Settings view");
    };
    assert_eq!(state.take_debugger_change_request(), None, "drained");
    let facts = app.debugger_access_facts();
    assert_eq!(
        facts.refusal,
        Some(crate::debugger_access::Refusal::NoWindow)
    );
    assert_eq!(facts.in_flight, None);
    assert_eq!(
        facts.probe.label,
        aterm_containment::consent::DebuggerProbeLabel::RefusedDisabled,
        "the inert probe arm: nothing was consulted"
    );
    assert!(!facts.offers(DebuggerChange::On) && !facts.offers(DebuggerChange::Off));
    assert_eq!(
        app.begin_debugger_access_change(
            DebuggerChange::Off,
            crate::native_settings::OwnerAnswer::yes_for_test()
        ),
        Err(crate::debugger_access::Refusal::NoWindow)
    );
    app.tick_debugger_access();
    assert!(
        !app.debugger_change.records_asked(),
        "headless: nothing is read"
    );
    assert_eq!(
        app.next_debugger_change_deadline(std::time::Instant::now()),
        None
    );
}

/// THE `WF(Notice)` SITE, App level (debugger design §5.1, §5.4): while a
/// change is under way the App folds the lane's `RESULT_RECHECK` into the
/// park, and the park's own consent-lane tick takes a result whose wake
/// never came and re-probes.
#[cfg(target_os = "macos")]
#[test]
fn a_debugger_change_under_way_is_folded_into_the_park_and_read_by_its_tick() {
    use crate::debugger_access::{DebuggerChangeLane, Outcome, RESULT_RECHECK};
    use crate::metrics::DeadlineOwner;
    use aterm_containment::consent::{
        DebuggerChange, DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
        DevtoolsChangeRun, DevtoolsChild,
    };
    let fold = |app: &App, now: std::time::Instant| {
        let (mut deadline, mut owner) = (None, crate::deadline_heal::DeadlineFold::default());
        app.fold_consent_lane_deadlines(now, &mut deadline, &mut owner);
        (deadline, owner.owner())
    };
    let mut app = App::headless_for_test();
    app.debugger_change = DebuggerChangeLane::with_liveness(|_| false);
    let now = std::time::Instant::now();
    app.tick_consent_lanes(now);
    assert!(app.debugger_change.records_asked(), "the records are read");
    assert_eq!(fold(&app, now).0, None, "an idle lane arms nothing");
    app.debugger_change.observe(
        DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        },
        1,
    );
    let job = app
        .debugger_change
        .begin(
            DebuggerChange::On,
            crate::native_settings::OwnerAnswer::yes_for_test(),
        )
        .expect("a confirmed press over the precondition")
        .job();
    let slot = app.debugger_change.slot();
    let child = DevtoolsChild {
        pid: 4242,
        birth: None,
    };
    slot.publish_spawned(job, child);
    app.tick_consent_lanes(now);
    let flight = app.debugger_access_facts().in_flight;
    assert!(flight.is_some(), "the park read the spawned child");
    assert_eq!(
        fold(&app, now),
        (Some(now + RESULT_RECHECK), DeadlineOwner::DebuggerChange),
        "a child in flight keeps the park re-reading"
    );
    slot.publish_result(
        job,
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: String::new(),
            stderr_head: String::new(),
        },
    );
    app.tick_consent_lanes(now);
    let facts = app.debugger_access_facts();
    assert_eq!(facts.in_flight, None, "the result was taken");
    assert_eq!(
        facts.last_outcome,
        Some(Outcome::Unobserved(DebuggerChange::On)),
        "and re-probed: the inert arm reads nothing, and says so"
    );
    assert_eq!(fold(&app, now).0, None, "ended: nothing left to arm");
}

/// The Developer Tools hand-over at the park: one under way is folded into the park and
/// read by the consent lane's tick.
#[cfg(target_os = "macos")]
#[test]
fn a_devtools_hand_over_under_way_is_folded_into_the_park_and_read_by_its_tick() {
    use crate::devtools_helper::{DONE_FOR, HandOver, Origin, PANEL_TICK, Phase, test_arms};
    use crate::metrics::DeadlineOwner;
    let fold = |app: &App, now: std::time::Instant| {
        let (mut deadline, mut owner) = (None, crate::deadline_heal::DeadlineFold::default());
        app.fold_consent_lane_deadlines(now, &mut deadline, &mut owner);
        (deadline, owner.owner())
    };
    let mut app = App::headless_for_test();
    let now = std::time::Instant::now();
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Digest),
        HandOver::Headless,
        "a headless instance does nothing"
    );
    test_arms::stand_in(&mut app, test_arms::opening());
    assert!(app.devtools_watch.wants_read(now), "the launch read");
    let _ = app
        .devtools_watch
        .observe(aterm_containment::DevToolGrant::NotGranted, true);
    assert_eq!(fold(&app, now).0, None, "nothing waits");
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Automatic),
        HandOver::Queued,
        "the window's own waits for a quiet moment"
    );
    assert_eq!(
        fold(&app, now).0,
        None,
        "no aterm window focused: the focus gain wakes the loop"
    );
    assert_eq!(app.hand_devtools_to_owner(Origin::Digest), HandOver::Opened);
    assert_eq!(
        (test_arms::opens(), test_arms::opened_notes()),
        (1, 1),
        "the page, and the shared marker"
    );
    assert_eq!(
        app.devtools_helper.queued(),
        Some(Origin::Automatic),
        "no revision stands, so the press's page is not its own: the window's own waits on"
    );
    assert!(
        app.devtools_watch.wants_read(now),
        "the owner's return reads"
    );
    assert_eq!(
        app.devtools_helper.panel().map(|p| p.phase()),
        Some(Phase::Explain)
    );
    let (deadline, owner) = fold(&app, now);
    assert_eq!(owner, DeadlineOwner::DevToolsHandOver);
    assert!(deadline.is_some_and(|at| at <= now + PANEL_TICK));
    assert_eq!(
        app.devtools_helper.refreshes_started(),
        0,
        "not switched on yet: macOS is asked nothing"
    );
    let t1 = std::time::Instant::now() + PANEL_TICK;
    let start = std::time::Instant::now();
    while app.devtools_helper.panel().map(|p| p.phase()) != Some(Phase::Done) {
        assert!(start.elapsed() < Duration::from_secs(60), "hang");
        app.tick_consent_lanes(t1);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        app.devtools_helper.panel().map(|p| p.phase()),
        Some(Phase::Done),
        "on: the done words"
    );
    assert_eq!(
        app.devtools_helper.refreshes_started(),
        1,
        "switched on: macOS is asked about aterm again, once"
    );
    app.tick_consent_lanes(t1 + DONE_FOR);
    assert!(app.devtools_helper.panel().is_none(), "done closes itself");
    assert_eq!(
        fold(&app, t1 + DONE_FOR).0,
        None,
        "ended: nothing left to arm"
    );
    assert_eq!(
        app.hand_devtools_to_owner(Origin::Ctl),
        HandOver::Refused(crate::devtools_helper::Refusal::Granted),
        "the panel's read is the lane's reading now"
    );
}

/// THE DEVELOPER TOOLS CARD'S BUTTON is the one hand-over: its press is
/// recorded on the view, the park performs it
/// through `App::hand_devtools_to_owner(Origin::Settings)` — the page, and
/// the helper panel beside it — and what it did lands on the view's bar
/// with the route in words.
#[cfg(target_os = "macos")]
#[test]
fn the_developer_tools_cards_press_is_the_one_hand_over() {
    use crate::devtools_helper::{Origin, Phase, test_arms};
    use crate::native_settings::{MACOS_ACCESS_OPEN_DEVTOOLS, SettingsRoute};
    let words = crate::menu::privacy_settings_path_words(crate::menu::PrivacyPane::DeveloperTools);
    let press_and_park = |app: &mut App| {
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(SettingsRoute::Security));
        let (_, view) = app.active_native_view(wid).expect("Settings view");
        let access = app.macos_access_projection(false);
        app.publish_macos_access(
            &[(wid, view)],
            &access,
            crate::native_settings::ConsentGestures::confirming(),
        );
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(MACOS_ACCESS_OPEN_DEVTOOLS),
                value: None,
            }),
        )
        .unwrap();
        app.sync_settings_consent_posture();
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("a Settings view");
        };
        assert!(!state.take_devtools_hand_over_request(), "drained");
        state.feedback.clone().expect("the bar says what happened")
    };
    let not_opened = format!("System Settings did not open. The setting is at {words}.");

    let mut headless = App::headless_for_test();
    assert_eq!(press_and_park(&mut headless), not_opened);
    assert!(headless.devtools_helper.panel().is_none());

    let mut app = App::headless_for_test();
    test_arms::stand_in(&mut app, test_arms::declining());
    let opens = test_arms::opens();
    assert_eq!(
        press_and_park(&mut app),
        format!("Opened System Settings \u{2014} look under {words}.")
    );
    assert_eq!(
        test_arms::opens(),
        opens + 1,
        "one page, whatever was declined"
    );
    let panel = app.devtools_helper.panel().expect("the helper panel");
    assert_eq!(
        (panel.origin(), panel.phase()),
        (Origin::Settings, Phase::Explain)
    );
    assert!(
        app.devtools_watch.wants_read(std::time::Instant::now()),
        "the owner's return reads the list"
    );

    let mut refused = App::headless_for_test();
    test_arms::stand_in(&mut refused, test_arms::refusing());
    let opens = test_arms::opens();
    assert_eq!(press_and_park(&mut refused), not_opened);
    assert_eq!(test_arms::opens(), opens + 1, "it was asked");
    assert!(refused.devtools_helper.panel().is_none());
}

/// A PRESS ON A LIST READ EMPTY waits for `spctl developer-mode
/// enable-terminal` before the page can open: the card's bar says the page is
/// on its way, and when the run ends it says how it came out — opened, opened
/// with macOS showing no list because the run failed (and the panel says why
/// and offers Show it), or (the negative control) not — instead of a promise
/// left standing.
#[cfg(target_os = "macos")]
#[test]
fn a_cards_press_that_waits_for_spctl_hears_how_the_page_came_out() {
    use crate::devtools_helper::test_arms;
    use crate::native_settings::{MACOS_ACCESS_OPEN_DEVTOOLS, SettingsRoute};
    let words = crate::menu::privacy_settings_path_words(crate::menu::PrivacyPane::DeveloperTools);
    for (reach, came_out) in [
        (
            test_arms::opening(),
            format!("Opened System Settings \u{2014} look under {words}."),
        ),
        (
            test_arms::spctl_failing(),
            format!(
                "Opened System Settings, but macOS did not show the Developer Tools list \
                 (spctl exited 1). The setting is at {words}."
            ),
        ),
        (
            test_arms::refusing(),
            format!("System Settings did not open. The setting is at {words}."),
        ),
    ] {
        let mut app = App::headless_for_test();
        test_arms::stand_in(&mut app, reach);
        app.devtools_helper.note_reading(
            test_arms::reading(
                aterm_containment::DevToolGrant::NotGranted,
                test_arms::absent(0),
            ),
            std::time::Instant::now(),
        );
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(SettingsRoute::Security));
        let (_, view) = app.active_native_view(wid).expect("Settings view");
        let access = app.macos_access_projection(false);
        app.publish_macos_access(
            &[(wid, view)],
            &access,
            crate::native_settings::ConsentGestures::confirming(),
        );
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(MACOS_ACCESS_OPEN_DEVTOOLS),
                value: None,
            }),
        )
        .unwrap();
        app.sync_settings_consent_posture();
        let feedback = |app: &mut App| {
            let Some(crate::native_app::AppViewState::Settings(state)) =
                app.native_runtime.view_state_mut(view)
            else {
                panic!("a Settings view");
            };
            state.feedback.clone().expect("the bar says what happened")
        };
        assert!(
            feedback(&mut app).starts_with("Asking macOS to show the Developer Tools list"),
            "{}",
            feedback(&mut app)
        );
        let start = std::time::Instant::now();
        while app.devtools_helper.listing().is_some() {
            assert!(start.elapsed() < std::time::Duration::from_secs(60), "hang");
            app.tick_devtools_hand_over(std::time::Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(feedback(&mut app), came_out);
        if let Some(panel) = app.devtools_helper.panel() {
            let content = panel.content();
            let failed = content
                .texts()
                .any(|(text, _)| text.contains("macOS did not show the list (spctl exited 1)"));
            assert_eq!(failed, came_out.contains("did not show"), "{content:?}");
            assert_eq!(
                content
                    .buttons()
                    .contains(&crate::devtools_helper::Button::ShowList),
                failed,
                "Show it again only after a failed run: {content:?}"
            );
        }
    }
}

/// THE AUTOMATIC STEP WAITS FOR A FULL DISK ACCESS READING STILL OUT, NEVER TAKES IT
/// FOR OFF: the window's own reading lives `[privacy] probe_interval_ms` and the
/// step's quiet moment comes `QUIET_FOR` after it was asked, so the read made then
/// sends a new probe with no answer yet. Taken for off, it left the step to a focus
/// gain a minute or more later (a fresh macOS 27 VM, after Full Disk Access's Quit &
/// Reopen). The step waits a panel tick, with the answer of any `spctl` run it
/// carries, and the grant read on, brings the page up; a reading that answered off
/// is refused as before, and nothing waits (the negative control).
#[cfg(target_os = "macos")]
#[test]
fn the_automatic_step_waits_for_a_full_disk_access_reading_still_out() {
    use crate::devtools_helper::{HandOver, Moment, Origin, PANEL_TICK, Refusal, test_arms};
    use aterm_containment::consent::{FdaProbe, FdaState, ProbeGate, ProbeLabel};
    use std::time::{Duration, Instant};
    fn granted(_: ProbeGate) -> FdaProbe {
        FdaProbe {
            state: FdaState::Granted,
            label: ProbeLabel::OpenOk,
        }
    }
    fn denied(_: ProbeGate) -> FdaProbe {
        FdaProbe {
            state: FdaState::Denied,
            label: ProbeLabel::OpenEperm,
        }
    }
    // A step of an approved setup revision, aterm off the list, and no Full Disk
    // Access reading yet: the first read sends the probe. Its answer outlives the
    // test's pace, so only that first read is out.
    let app_with = |fda: fn(ProbeGate) -> FdaProbe| {
        let mut app = App::headless_for_test();
        test_arms::stand_in(&mut app, test_arms::opening());
        app.observe_initial_setup_permissions(Some(crate::InitialSetupPermissionPolicy {
            revision: 1,
            allowed: true,
        }));
        app.set_privacy_for_test(crate::app_config::PrivacyConfig {
            probe_interval_ms: Some(3_600_000),
            ..Default::default()
        });
        app.consent.set_fda_arm_for_test(fda);
        app
    };
    let answered = |app: &App| {
        let start = Instant::now();
        while app.consent.fda_in_flight_for_test().is_some() {
            assert!(start.elapsed() < Duration::from_secs(60), "hang");
            std::thread::sleep(Duration::from_millis(1));
        }
    };

    let mut app = app_with(granted);
    let before = Instant::now();
    assert_eq!(
        app.devtools_hand_over(Origin::Automatic, Moment::Quiet),
        HandOver::Queued,
        "a reading still out is no answer"
    );
    assert_eq!(app.devtools_helper.queued(), Some(Origin::Automatic));
    assert!(
        app.devtools_helper.next_deadline(before, || true) >= Some(before + PANEL_TICK),
        "it looks again a panel tick later, by when the probe has answered"
    );
    answered(&app);
    let opens = test_arms::opens();
    let (origin, listed) = app
        .devtools_helper
        .take_queued(Instant::now(), true)
        .expect("held");
    assert_eq!(
        app.devtools_hand_over_carrying(origin, Moment::Quiet, listed),
        HandOver::Opened,
        "the grant read on: the page comes up"
    );
    assert_eq!(test_arms::opens(), opens + 1);

    // An `spctl` run's answer waits with it, so the run is not made again.
    let mut listed = app_with(granted);
    let at = Instant::now();
    assert_eq!(
        listed.devtools_hand_over_after_listing(Origin::Automatic, Ok(()), at),
        HandOver::Queued
    );
    assert!(
        listed.devtools_helper.hand_over_due(at + PANEL_TICK),
        "a panel tick later too, its run's answer or not"
    );
    assert_eq!(
        listed.devtools_helper.take_queued(Instant::now(), true),
        Some((Origin::Automatic, Some(Ok(())))),
        "held with the run's answer"
    );

    let mut off = app_with(denied);
    assert_eq!(
        off.devtools_hand_over(Origin::Automatic, Moment::Quiet),
        HandOver::Queued
    );
    answered(&off);
    let (origin, listed) = off
        .devtools_helper
        .take_queued(Instant::now(), true)
        .expect("held");
    assert_eq!(
        off.devtools_hand_over_carrying(origin, Moment::Quiet, listed),
        HandOver::Refused(Refusal::NoFullDiskAccess),
        "a reading that answered off is refused as before"
    );
    assert_eq!(off.devtools_helper.queued(), None, "and nothing waits");
}

/// A HAND-OVER THE OWNER DID NOT PRESS, JUDGED QUIET BEFORE ITS `spctl` RUN, IS
/// JUDGED AGAIN WHEN THE RUN ENDS (the run takes as long as syspolicyd takes): the
/// moment has passed — here no aterm window is focused — so System Settings does
/// not take focus; the hand-over waits for the next quiet moment with the run's
/// answer, and then opens the page without running `spctl` again, the panel
/// saying Terminal is aterm's doing.
#[cfg(target_os = "macos")]
#[test]
fn a_hand_over_judged_quiet_before_spctl_is_judged_again_when_it_ends() {
    use crate::devtools_helper::{HandOver, Moment, Origin, test_arms};
    use aterm_containment::DevToolGrant;
    let run_out = |app: &mut App| {
        let start = std::time::Instant::now();
        while app.devtools_helper.listing().is_some() {
            assert!(start.elapsed() < std::time::Duration::from_secs(60), "hang");
            app.tick_devtools_hand_over(std::time::Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    let empty = || test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(0));
    let mut app = App::headless_for_test();
    test_arms::stand_in(&mut app, test_arms::opening());
    // A step of an approved setup revision, with Full Disk Access on.
    app.observe_initial_setup_permissions(Some(crate::InitialSetupPermissionPolicy {
        revision: 1,
        allowed: true,
    }));
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(
        aterm_containment::consent::FdaProbe {
            state: aterm_containment::consent::FdaState::Granted,
            label: aterm_containment::consent::ProbeLabel::OpenOk,
        },
    );
    app.devtools_helper
        .note_reading(empty(), std::time::Instant::now());
    let opens = test_arms::opens();
    assert_eq!(
        app.devtools_hand_over(Origin::Automatic, Moment::Quiet),
        HandOver::Listing
    );
    run_out(&mut app);
    assert_eq!(
        test_arms::opens(),
        opens,
        "the moment passed: System Settings does not take focus"
    );
    assert!(app.devtools_helper.panel().is_none());
    assert_eq!(
        app.devtools_helper.queued(),
        Some(Origin::Automatic),
        "held for the next quiet moment"
    );
    let (origin, listed) = app
        .devtools_helper
        .take_queued(std::time::Instant::now(), true)
        .expect("held");
    assert_eq!(listed, Some(Ok(())), "with the run's answer");
    assert_eq!(
        app.devtools_hand_over_carrying(origin, Moment::Quiet, listed),
        HandOver::Opened
    );
    assert!(app.devtools_helper.listing().is_none(), "no second run");
    assert_eq!(test_arms::opens(), opens + 1);
    let panel = app.devtools_helper.panel().expect("the helper panel");
    assert!(
        panel
            .content()
            .texts()
            .any(|(text, _)| text == atpkg::devtools::only_aterm(true))
    );

    let mut granted = App::headless_for_test();
    test_arms::stand_in(&mut granted, test_arms::opening());
    granted
        .devtools_helper
        .note_reading(empty(), std::time::Instant::now());
    assert_eq!(
        granted.devtools_hand_over(Origin::Ctl, Moment::Quiet),
        HandOver::Listing
    );
    granted.devtools_helper.note_reading(
        test_arms::reading(DevToolGrant::Granted, test_arms::on()),
        std::time::Instant::now(),
    );
    let opens = test_arms::opens();
    run_out(&mut granted);
    assert_eq!(test_arms::opens(), opens, "nothing opens");
    assert_eq!(granted.devtools_helper.queued(), None, "refused: dropped");

    let mut pressed = App::headless_for_test();
    test_arms::stand_in(&mut pressed, test_arms::opening());
    pressed
        .devtools_helper
        .note_reading(empty(), std::time::Instant::now());
    assert_eq!(
        pressed.hand_devtools_to_owner(Origin::Digest),
        HandOver::Listing
    );
    let opens = test_arms::opens();
    run_out(&mut pressed);
    assert_eq!(test_arms::opens(), opens + 1, "the owner asked");
    assert!(pressed.devtools_helper.panel().is_some());
}

/// THE WINDOW'S OWN HAND-OVER, REFUSED AS UNKNOWN AT ITS QUIET MOMENT (a later
/// reading read nothing: no Full Disk Access, no new program in tccd's window), is
/// no decision against it: the next `not_granted` reading asks for it again,
/// rather than the once-per-process latch keeping it from ever coming back in this
/// process.
#[cfg(target_os = "macos")]
#[test]
fn an_automatic_hand_over_refused_as_unknown_asks_again_at_the_next_reading() {
    use crate::devtools_helper::{HandOver, Moment, Origin, Reading, Refusal, test_arms};
    use crate::devtools_watch::Step;
    use aterm_containment::DevToolGrant;
    for (reach, reading, why, asks_again) in [
        (
            test_arms::opening(),
            Reading::UNREAD,
            Refusal::Unknown,
            true,
        ),
        (
            test_arms::declining(),
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
            Refusal::Declined,
            false,
        ),
    ] {
        let mut app = App::headless_for_test();
        test_arms::stand_in(&mut app, reach);
        // A step of an approved setup revision, with Full Disk Access on.
        app.observe_initial_setup_permissions(Some(crate::InitialSetupPermissionPolicy {
            revision: 1,
            allowed: true,
        }));
        app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(
            aterm_containment::consent::FdaProbe {
                state: aterm_containment::consent::FdaState::Granted,
                label: aterm_containment::consent::ProbeLabel::OpenOk,
            },
        );
        assert_eq!(
            app.devtools_watch.observe(DevToolGrant::NotGranted, false),
            Step::NotOn { hand_over: true },
            "the first reading asks"
        );
        app.devtools_helper
            .note_reading(reading, std::time::Instant::now());
        let opens = test_arms::opens();
        assert_eq!(
            app.devtools_hand_over(Origin::Automatic, Moment::Quiet),
            HandOver::Refused(why)
        );
        assert_eq!(test_arms::opens(), opens, "nothing opens");
        assert_eq!(
            app.devtools_watch.observe(DevToolGrant::NotGranted, false),
            Step::NotOn {
                hand_over: asks_again
            },
            "{why:?}"
        );
    }
}

/// What the DEVELOPER TOOLS card is told: while the window's own re-read is
/// out, the switch the hand-over lane last read stands in (no flicker to
/// "Checking…" on every focus gain); and when aterm last opened the page is
/// read from the shared marker ONCE — not per park — taken from the lane
/// when the lane writes it, and read again only after a focus gain.
#[cfg(target_os = "macos")]
#[test]
fn the_devtools_card_reads_the_lanes_switch_and_the_opened_marker_once() {
    use crate::devtools_helper::{Origin, test_arms};
    use std::cell::Cell;
    thread_local! {
        static READS: Cell<u32> = const { Cell::new(0) };
    }
    let reach = test_arms::marked(|| {
        READS.with(|reads| reads.set(reads.get() + 1));
        let now = aterm_time::unix_secs().unwrap_or(0);
        Some(("0.0.0".to_string(), now.saturating_sub(5 * 60)))
    });
    let mut app = App::headless_for_test();
    test_arms::stand_in(&mut app, reach);
    let facts = app.macos_access_projection(false).devtools;
    assert!(facts.probe.label == aterm_containment::DevToolProbeLabel::Pending);
    assert_eq!(
        (facts.list, facts.grant),
        (
            test_arms::absent(2),
            aterm_containment::DevToolGrant::NotGranted
        ),
        "the lane's last read switch stands in while the row is out"
    );
    assert!(
        facts
            .opened_minutes
            .is_some_and(|minutes| (4..=6).contains(&minutes)),
        "{:?}",
        facts.opened_minutes
    );
    for _ in 0..3 {
        let _ = app.macos_access_projection(false);
    }
    assert_eq!(
        READS.with(Cell::get),
        1,
        "the marker is read once, not per park"
    );

    assert!(app.hand_devtools_to_owner(Origin::Digest).brings_owner());
    assert_eq!(
        app.macos_access_projection(false).devtools.opened_minutes,
        Some(0),
        "the lane's own write, without reading the file"
    );
    assert_eq!(READS.with(Cell::get), 1);

    app.devtools_helper.forget_opened();
    let again = app.macos_access_projection(false).devtools.opened_minutes;
    assert!(again.is_some_and(|minutes| minutes >= 4), "{again:?}");
    assert_eq!(
        READS.with(Cell::get),
        2,
        "a focus gain reads it again, once"
    );
}

/// `aterm ctl devtools hand-over` is the one hand-over with `Origin::Ctl`, held to the
/// window's own rules because every in-session client may send it: it never opens the page
/// as it asks but waits for a quiet moment, and the window's refusals hold it. (Its `now` is
/// `permissions walk now devtools`, the walk's page: `system_state_access::permission_walk_tests`.)
#[cfg(target_os = "macos")]
#[test]
fn the_control_verbs_ask_is_held_to_the_windows_own_rules() {
    use crate::devtools_helper::{HandOver, Origin, Reading, Refusal, test_arms};
    use aterm_containment::DevToolGrant;
    const ASK: Origin = Origin::Ctl;
    let ask = |app: &mut App| app.hand_devtools_to_owner(ASK);

    let mut headless = App::headless_for_test();
    assert_eq!(ask(&mut headless), HandOver::Headless);

    let mut app = App::headless_for_test();
    test_arms::stand_in(&mut app, test_arms::opening());
    let opens = test_arms::opens();
    let t = std::time::Instant::now();
    assert!(app.devtools_watch.wants_read(t), "the launch read");
    assert_eq!(
        ask(&mut app),
        HandOver::Queued,
        "the bare form waits for a quiet moment"
    );
    assert_eq!(app.devtools_helper.queued(), Some(ASK));
    assert!(app.devtools_helper.panel().is_none(), "nothing shown yet");
    assert_eq!(test_arms::opens(), opens, "nothing opened as it asked");
    assert!(
        !app.devtools_watch.wants_read(t),
        "nothing has brought the owner anywhere yet"
    );
    assert_eq!(ask(&mut app), HandOver::Queued, "one waits");

    let refusals = [
        (test_arms::declining(), None, Refusal::Declined),
        (
            test_arms::opening(),
            Some(Reading::UNREAD),
            Refusal::Unknown,
        ),
        (
            test_arms::opening(),
            Some(test_arms::reading(DevToolGrant::Granted, test_arms::on())),
            Refusal::Granted,
        ),
        (test_arms::unstable(), None, Refusal::Unstable),
        (
            test_arms::opened(|window| (90 < window).then_some(90)),
            None,
            Refusal::Recent,
        ),
    ];
    for (reach, reading, why) in refusals {
        let mut app = App::headless_for_test();
        test_arms::stand_in(&mut app, reach);
        if let Some(reading) = reading {
            app.devtools_helper
                .note_reading(reading, std::time::Instant::now());
        }
        assert_eq!(ask(&mut app), HandOver::Refused(why));
        assert!(app.devtools_helper.panel().is_none(), "{why:?}");
        assert_eq!(app.devtools_helper.queued(), None, "{why:?}");
    }
}

#[test]
fn a_debugger_change_that_reads_unchanged_is_a_decline_and_an_observed_one_a_record() {
    use crate::debugger_access::{DebuggerRecord, DriftStep, LaneStep, Outcome};
    use aterm_containment::consent::DebuggerChange;
    let mut app = App::headless_for_test();
    assert!(
        !app.owner_quiet
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_quiet_now_for_test()
    );
    app.apply_debugger_step(LaneStep {
        outcome: Some(Outcome::Changed(DebuggerChange::On)),
        drift: DriftStep {
            record: Some(DebuggerRecord::On),
            write: None,
        },
        ..LaneStep::default()
    });
    assert!(
        !app.owner_quiet
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_quiet_now_for_test(),
        "a change that landed is no decline"
    );
    let record = app
        .messages
        .log()
        .records()
        .next_back()
        .expect("the change is on record");
    assert_eq!(
        record.key.as_deref(),
        Some(crate::message_reporters::KEY_DEBUGGER_ACCESS)
    );
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::KEY_DEBUGGER_ACCESS)
            .is_none(),
        "a record never takes the glass"
    );
    app.apply_debugger_step(LaneStep {
        outcome: Some(Outcome::NotChanged(DebuggerChange::On)),
        ..LaneStep::default()
    });
    assert!(
        app.owner_quiet
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_quiet_now_for_test(),
        "Cancel in macOS's dialog quiets the page like Cancel in aterm's"
    );
}

#[test]
fn the_park_drains_every_settings_view_and_shares_the_decline_record() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
    let (instance, first) = app.active_native_view(wid).expect("Settings view");
    let second = app
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            instance,
            crate::native_app::AppViewState::Settings(Box::new(
                crate::native_settings::SettingsViewState::new(&app.config),
            )),
        )
        .expect("second Settings presentation");
    let settings_tab = app.windows[&wid].tab_set.active_id().unwrap();
    let settings_index = app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| tab.id == settings_tab)
        .unwrap();
    for _ in 0..32 {
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    }
    assert!(app.active_native_view(wid).is_none());
    assert_eq!(
        app.settings_tabs_in_window(wid),
        [
            (settings_index, settings_tab, instance, first),
            (settings_index, settings_tab, instance, second),
        ]
    );
    for (view, route) in [
        (first, crate::native_settings::SettingsRoute::Security),
        (second, crate::native_settings::SettingsRoute::Appearance),
    ] {
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("a Settings view");
        };
        state.record_claimant_retire_for_test(vec![std::path::PathBuf::from("/A/x.app.rollback")]);
        state.navigate(route);
    }
    assert_eq!(app.settings_tabs_in_window(wid).len(), 2);
    app.owner_quiet
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .confirm_both_for_test();
    app.sync_settings_consent_posture();
    assert!(
        !app.owner_quiet
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .has_confirmed_for_test(),
        "the park settles every yes it takes up"
    );
    for view in [first, second] {
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("a Settings view");
        };
        assert_eq!(state.take_claimant_retire_request(), None, "drained");
        assert!(
            std::sync::Arc::ptr_eq(state.owner_quiet_for_test(), &app.owner_quiet),
            "every view holds the process's record"
        );
    }
    assert!(
        app.claimant_retire_refusal.is_some(),
        "drained, then refused here because a headless instance starts no worker"
    );
}

#[test]
fn settings_presentation_split_discovery_focus_and_close_preserve_terminal_siblings() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (instance, first_settings) = app.active_native_view(wid).expect("Settings view");
    let second_settings = app
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            instance,
            crate::native_app::AppViewState::Settings(Box::new(
                crate::native_settings::SettingsViewState::new(&app.config),
            )),
        )
        .expect("second Settings presentation");
    let (terminal_session, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    assert_eq!(app.focused_session_id(wid), Some(terminal_session));
    assert_eq!(app.settings_tabs_in_window(wid).len(), 2);
    assert!(
        app.settings_tab_open(),
        "nonfocused Settings leaves remain open"
    );

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (_, focused_settings) = app.active_native_view(wid).expect("Settings focused");
    assert!(
        [first_settings, second_settings].contains(&focused_settings),
        "open focuses an existing Settings leaf"
    );
    assert_eq!(
        app.settings_tabs_in_window(wid).len(),
        2,
        "open never duplicates a visible nonfocused presentation"
    );

    assert!(app.close_settings_tabs());
    assert!(!app.settings_tab_open());
    assert!(matches!(
        app.view_store.get(terminal_view),
        Some(crate::tab_model::View::Terminal(terminal))
            if terminal.session == terminal_session
    ));
    assert!(app.pool.get(terminal_session).is_some());
    assert!(
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .any(|tab| tab.root.contains(terminal_view))
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn settings_presentation_introspection_uses_nonfocused_split_leaf_viewport() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (instance, settings) = app.active_native_view(wid).expect("Settings view");
    let (_, terminal) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert!(app.active_native_view(wid).is_none());
    assert_eq!(
        app.windows[&wid].tab_set.active().map(|tab| tab.focus),
        Some(terminal)
    );

    let full = app.native_ui_viewport(wid).expect("window viewport");
    let exact = app
        .native_ui_viewport_for(wid, settings)
        .expect("Settings leaf viewport");
    assert!(
        exact.width < full.width,
        "split leaf is narrower than its window"
    );
    let expected = app
        .compiled_native_ui_for(wid, instance, settings, exact)
        .expect("compile exact Settings leaf")
        .controls_lines();
    let emitted = app
        .read_aux_controls(crate::app_introspect::AuxTarget::Prefs)
        .into_iter()
        .filter(|line| line.starts_with("ui "))
        .collect::<Vec<_>>();
    assert_eq!(emitted, expected);
}

#[test]
fn settings_presentation_explicit_open_is_idempotent_per_front_window() {
    let mut app = App::headless_for_test();
    let first = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let first_instance = app.settings_tabs_in_window(first)[0].2;

    let session = app.next_session_id;
    let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
    assert_eq!(app.frontmost_window, Some(second));
    assert!(app.settings_tabs_in_window(second).is_empty());
    assert!(
        app.settings_tab_open(),
        "the first window still presents Settings"
    );

    assert!(app.apply_settings_open_request(Some(true)));
    let second_presentations = app.settings_tabs_in_window(second);
    assert_eq!(second_presentations.len(), 1);
    assert_eq!(second_presentations[0].2, first_instance);
    assert_eq!(
        app.windows
            .keys()
            .copied()
            .flat_map(|wid| app.settings_tabs_in_window(wid))
            .filter(|(_, _, instance, _)| *instance == first_instance)
            .count(),
        2,
        "both windows share one Settings controller"
    );

    assert!(app.apply_settings_open_request(Some(true)));
    assert_eq!(
        app.settings_tabs_in_window(second).len(),
        1,
        "a repeated explicit open reuses the requesting window's view"
    );
}

#[test]
fn settings_last_tab_close_arms_teardown_and_reopen_cancels_the_stale_flag() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert_eq!(app.windows[&wid].tab_set.len(), 2);

    assert!(!app.close_tab_at(wid, 0));
    assert_eq!(app.windows[&wid].tab_set.len(), 1);
    assert!(app.settings_tab_open());

    assert!(app.close_settings_tabs());
    assert!(!app.settings_tab_open());
    assert!(app.windows[&wid].tab_set.is_empty());
    assert!(
        app.windows[&wid].pending_close,
        "the main-loop Settings wake must escalate this empty window"
    );
    assert!(
        app.windows[&wid].pending_close_attribution.is_some(),
        "the deferred close stashed its attribution for the teardown"
    );

    assert!(app.apply_settings_open_request(Some(true)));
    assert_eq!(app.windows[&wid].tab_set.len(), 1);
    assert!(app.settings_tab_open());
    assert!(
        !app.windows[&wid].pending_close,
        "reopening a real Settings tab cancels stale empty-window teardown"
    );
    assert!(
        app.windows[&wid].pending_close_attribution.is_none(),
        "the cancelled teardown never runs, so its attribution must not \
         survive to be journalled on this window's NEXT close"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn control_route_open_invalidates_and_rebuilds_the_full_native_frame() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    assert!(app.prepare_native_input_scratch(wid));
    let (about_fp, about_pixels) = app
        .windows
        .get(&wid)
        .and_then(|window| window.settings_card.as_ref())
        .map(|card| (card.fp, card.rgba.clone()))
        .expect("About materializes one complete native tray");

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    assert!(
        app.windows
            .get(&wid)
            .is_some_and(|window| window.settings_card.is_none()),
        "the typed repaint effect discards the previous route raster"
    );
    assert!(app.prepare_native_input_scratch(wid));
    let (appearance_fp, appearance_pixels) = app
        .windows
        .get(&wid)
        .and_then(|window| window.settings_card.as_ref())
        .map(|card| (card.fp, card.rgba.clone()))
        .expect("Appearance rematerializes one complete native tray");
    assert_ne!(about_fp, appearance_fp);
    assert_ne!(about_pixels, appearance_pixels);

    let compiled = app.compiled_native_ui(wid).unwrap();
    assert!(
        compiled
            .semantic(&crate::native_ui::UiKey::new("settings/preview/appearance"))
            .is_some(),
        "the compact frame presents Appearance's exact live preview even when its redundant heading is shed"
    );
    assert!(
        compiled
            .semantic(&crate::native_ui::UiKey::new("about/hero"))
            .is_none(),
        "the rebuilt frame has no stale About subtree"
    );
}

#[test]
fn section_control_navigates_the_typed_native_route() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    for route in [
        crate::native_settings::SettingsRoute::CursorMotion,
        crate::native_settings::SettingsRoute::TextFonts,
        crate::native_settings::SettingsRoute::Security,
    ] {
        app.settings_show_route(route)
            .expect("open native Settings route");
        let (_, view) = app
            .active_native_view(crate::WindowId(0))
            .expect("active Settings view");
        let crate::native_app::AppViewState::Settings(state) =
            app.native_runtime.view_state(view).expect("view state")
        else {
            panic!("Settings view kind");
        };
        assert_eq!(state.route, route);
    }
    assert!(app.native_settings_legacy_state().is_some());
}

#[test]
fn menu_find_focuses_native_settings_search_not_parked_terminal_find() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.find_requested();
    let (_, view) = app.active_native_view(wid).expect("active Settings view");
    let crate::native_app::AppViewState::Settings(state) =
        app.native_runtime.view_state(view).expect("Settings state")
    else {
        panic!("Settings view kind");
    };
    assert_eq!(
        state.common.last_focus.as_ref().map(|key| key.as_str()),
        Some("settings/search")
    );
    assert!(
        app.windows.get(&wid).is_some_and(|ws| ws.search.is_none()),
        "the parked terminal never enters find mode"
    );
}

fn captured_typed(app: &mut App) -> Vec<(SoundVoice, f32)> {
    app.trail_audio
        .take_captured_for_test()
        .into_iter()
        .map(|e| {
            assert_eq!(
                e.kind,
                SoundGesture::Trail(SoundKind::Typed),
                "an audition is one keystroke, nothing else"
            );
            assert!(!e.bed, "the audition never feeds the bed");
            assert_eq!(e.pan, 0.0);
            (e.voice, e.gain)
        })
        .collect()
}

fn app_with_capture() -> App {
    let mut app = App::headless_for_test();
    app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
    app
}

#[test]
fn the_audition_is_gated_like_a_keystroke() {
    let mut app = app_with_capture();
    app.config.trail_sounds = Some(false);
    app.audition_typing_sound(SoundVoice::Marimba);
    assert!(captured_typed(&mut app).is_empty(), "master off");
    assert_eq!(app.typing_sound_auditioned, SoundVoice::Marimba);
    app.config.trail_sounds = Some(true);
    app.config.trail_sound_volume = Some(0.0);
    app.audition_typing_sound(SoundVoice::Felt);
    assert!(captured_typed(&mut app).is_empty(), "muted");
    app.config.trail_sound_volume = Some(0.4);
    app.serious_mode = true;
    app.audition_typing_sound(SoundVoice::Typewriter);
    assert!(captured_typed(&mut app).is_empty(), "serious mode");
    app.serious_mode = false;
    app.audition_typing_sound(SoundVoice::Typewriter);
    assert_eq!(
        captured_typed(&mut app),
        vec![(SoundVoice::Typewriter, 0.4)]
    );
    app.trail_audio = crate::trail_audio::TrailAudio::new(false);
    app.audition_typing_sound(SoundVoice::Mech);
    assert_eq!(app.typing_sound_auditioned, SoundVoice::Mech);
}

#[test]
fn a_config_swap_auditions_only_a_changed_voice_and_never_twice() {
    let mut app = app_with_capture();
    let mut next = app.config.clone();
    assert_eq!(app.typing_sound_to_audition_on_swap(&next), None);
    next.trail_sound_style = Some("droplet".into());
    assert_eq!(
        app.typing_sound_to_audition_on_swap(&next),
        Some(SoundVoice::Of(GlowStyle::Water))
    );
    app.audition_typing_sound(SoundVoice::Of(GlowStyle::Water));
    assert_eq!(captured_typed(&mut app).len(), 1);
    assert_eq!(app.typing_sound_to_audition_on_swap(&next), None);
    next.trail_sound_style = Some(" Water ".into());
    assert_eq!(app.typing_sound_to_audition_on_swap(&next), None);
    next.trail_sound_style = None;
    assert_eq!(
        app.typing_sound_to_audition_on_swap(&next),
        Some(SoundVoice::Style)
    );
}

/// The window's `a11y_active` latch gates tree building: down until an OS client
/// attaches (every OS window has an adapter), up on attach, down on detach.
#[cfg(a11y_tree)]
#[test]
fn accessibility_publish_latches_on_attach_and_clears_on_detach() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let winit_id = aterm_window::window::WindowId::from(1u64);
    app.winit_to_window.insert(winit_id, wid);

    assert!(
        !app.windows[&wid].a11y_active,
        "no OS a11y client has attached, so presents must not build a tree"
    );
    app.on_accessibility_event(crate::a11y_adapter::Event {
        window_id: winit_id,
        window_event: crate::a11y_adapter::WindowEvent::InitialTreeRequested,
    });
    assert!(
        app.windows[&wid].a11y_active,
        "InitialTreeRequested is the attach edge — publishing must resume"
    );
    app.on_accessibility_event(crate::a11y_adapter::Event {
        window_id: winit_id,
        window_event: crate::a11y_adapter::WindowEvent::AccessibilityDeactivated,
    });
    assert!(
        !app.windows[&wid].a11y_active,
        "the client detached; a re-attach re-fires InitialTreeRequested"
    );
}

/// Each tab of the in-grid strip announces its own terminal's theme, though Settings and
/// mixed tabs have no all-terminal projection to count by.
#[cfg(a11y_tree)]
#[test]
fn the_tab_strip_names_each_tab_s_own_theme_past_native_and_mixed_tabs() {
    use crate::session_theme::{SessionTheme, SessionThemeOrigin};
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (mixed, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let last = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(last));
    let theme = |selector: &str| {
        Some(SessionTheme {
            selector: selector.to_string(),
            origin: SessionThemeOrigin::Ctl,
        })
    };
    app.set_session_theme(mixed, theme("Nord"));
    app.set_session_theme(last, theme("Dracula"));
    assert_eq!(
        app.windows[&wid].layouts.len(),
        2,
        "fixture: only the first and last tabs are all-terminal"
    );
    app.splice_tab_strip_with(wid, 1);

    let themes: Vec<_> = app
        .grid_a11y_tabs(wid)
        .into_iter()
        .map(|tab| (tab.index, tab.theme))
        .collect();
    assert_eq!(
        themes,
        vec![
            (0, None),
            (1, Some("Nord".to_string())),
            (2, Some("Dracula".to_string())),
        ],
        "the mixed tab reads its focused terminal's theme and the last tab its own"
    );
}

#[cfg(a11y_tree)]
#[test]
fn split_accessibility_consumes_the_presented_plan_not_live_layout() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    app.split_active_stub_tab(wid);
    let (rows, cols) = {
        let window = &app.windows[&wid];
        (usize::from(window.rows), usize::from(window.cols))
    };
    assert!(
        app.redraw_compose(wid, rows, cols, false, false, None, 0, Instant::now())
            .is_some(),
        "the two-pane fixture materializes a composed frame"
    );
    let presented = app
        .active_visible_leaf_plan(wid)
        .expect("presented canonical plan");
    assert_eq!(presented.leaves.len(), 2);
    let presented_focus = presented.focused;
    let presented_focus_leaf = presented
        .leaf(presented_focus)
        .expect("presented focused leaf")
        .clone();

    app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
    let live = app
        .active_visible_leaf_plan(wid)
        .expect("new live canonical plan");
    assert_eq!(live.leaves.len(), 3, "live cardinality really advanced");
    assert_ne!(
        live.leaf(presented_focus)
            .expect("the formerly focused leaf survives")
            .rect,
        presented_focus_leaf.rect,
        "live geometry really diverged from the frame on glass"
    );

    let panes = app.grid_a11y_panes(wid, &presented);
    assert_eq!(
        panes.len(),
        presented.leaves.len(),
        "accessibility keeps the presented two-pane cardinality"
    );
    let slot = presented
        .leaves
        .iter()
        .position(|leaf| leaf.view == presented_focus)
        .unwrap();
    let pane = &panes[slot];
    assert_eq!(
        (pane.row_off, pane.col_off, pane.snap.rows, pane.snap.cols),
        (
            presented_focus_leaf.rect.origin.y.round().max(0.0) as usize,
            presented_focus_leaf.rect.origin.x.round().max(0.0) as usize,
            (presented_focus_leaf.rect.size.height.round() as usize).max(1),
            (presented_focus_leaf.rect.size.width.round() as usize).max(1),
        ),
        "pane geometry comes from the exact successful-present plan"
    );
    assert!(
        pane.focused,
        "focus is likewise the presented frame's focus, not the newer live tree's"
    );
}

/// Even a stale or unsupported a11y request retires an older cursor-move licence.
#[cfg(a11y_tree)]
#[test]
fn accessibility_action_supersedes_pending_cursor_candidate() {
    use aterm_a11y::model::{Action, ActionRequest, NodeId, TreeId};

    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("lumen".to_string());
    let glow_cfg = app.glow_config();
    let trail_cfg = crate::cursor_trail::TrailConfig {
        enabled: true,
        duration: Duration::from_millis(300),
        max_len: 24,
        color: 0x0050_FA7B,
        intensity: 0.5,
        warmth: 0.0,
    };
    let geom = crate::cursor_glow::Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let now = Instant::now();
    let mut glow = Vec::new();
    let mut trail = Vec::new();
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.fx
            .cursor_mut()
            .glow
            .tick(Some((0, 0)), now, &glow_cfg, geom, &mut glow);
        ws.fx
            .cursor_mut()
            .trail
            .tick(Some((0, 0)), now, &trail_cfg, &mut trail);
        ws.fx.cursor_mut().glow.note_synthetic_move(now);
        ws.fx.cursor_mut().trail.note_synthetic_move(now);
        assert!(ws.fx.cursor_mut().glow.move_licensed(now));
        assert!(ws.fx.cursor_mut().trail.move_licensed(now));
    }

    let winit_id = aterm_window::window::WindowId::from(17u64);
    app.winit_to_window.insert(winit_id, wid);
    app.on_accessibility_event(crate::a11y_adapter::Event {
        window_id: winit_id,
        window_event: crate::a11y_adapter::WindowEvent::ActionRequested(ActionRequest {
            context: None,
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: NodeId(999_999),
            data: None,
        }),
    });
    let ws = app.windows.get_mut(&wid).unwrap();
    assert!(!ws.fx.cursor_mut().glow.move_licensed(now));
    assert!(!ws.fx.cursor_mut().trail.move_licensed(now));
    let moved = now + Duration::from_millis(1);
    assert_eq!(
        ws.fx
            .cursor_mut()
            .glow
            .tick(Some((0, 1)), moved, &glow_cfg, geom, &mut glow),
        0
    );
    assert_eq!(
        ws.fx
            .cursor_mut()
            .trail
            .tick(Some((0, 1)), moved, &trail_cfg, &mut trail),
        0
    );
    assert!(glow.is_empty() && trail.is_empty());
}
