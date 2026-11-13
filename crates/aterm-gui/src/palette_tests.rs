// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// At 2 device pixels per point the palette is the 1x palette at twice the pixels: its
/// corner radius cap, shadow offsets, border, row insets and selection ring are points.
/// The query field alone keeps its device-pixel inset and line, so at 2x its box clears its
/// type as it does on main. The cell (12 x 25) is tall enough that the 14-point radius cap
/// binds (§14.16). This is the macOS and Linux card on every platform; the Windows (Fluent)
/// card has its own test below.
#[test]
fn the_palette_wears_its_corners_shadow_and_ring_in_points() {
    let at = |k: f32| SettingsGeom {
        cw: 12.0 * k,
        ch: 25.0 * k,
        font_px: 18.0 * k,
        cols: 120,
        panel_rows: 40,
        px_per_pt: k,
    };
    let theme = aterm_render::Theme::default;
    let mut state = PaletteState::new();
    for typed in ["", "s"] {
        for c in typed.chars() {
            state.push_char(c);
        }
        let mut one = palette_tray_in(&state, &at(1.0), theme(), false).prims;
        let mut two = palette_tray_in(&state, &at(2.0), theme(), false).prims;
        let DrawPrim::Stroke { radius, .. } = one[3] else {
            panic!("the card's border: {:?}", one[3]);
        };
        assert_eq!(radius, 14.0, "the 1x radius is at its cap");
        // The query field is the first stroke after the title, on the card's second row.
        let query = |prims: &[DrawPrim]| {
            prims
                .iter()
                .skip(4)
                .position(|p| matches!(p, DrawPrim::Stroke { .. }))
                .map(|i| i + 4)
                .expect("the query field")
        };
        let (q1, q2) = (query(&one), query(&two));
        assert_eq!(q1, q2, "palette {typed:?}: the query field's place");
        for (k, prim) in [(1.0_f32, &one[q1]), (2.0, &two[q2])] {
            let DrawPrim::Stroke { h, width, .. } = *prim else {
                unreachable!()
            };
            assert_eq!(
                (h, width),
                (25.0 * k - 4.0, 1.25),
                "palette {typed:?} at {k}x: the query field's box is its row less 2 pixels \
                 each side, its line 1.25 pixels"
            );
        }
        one.remove(q1);
        two.remove(q2);
        assert_drawn_at_scale(&one, &two, 2.0, &[], &format!("palette {typed:?}"));
    }
}

/// The Windows palette ([`PALETTE_FLUENT`]) is its 1x card at twice the pixels too: its
/// filter box's corners, rim, accent foot and caret, its selected row's corners and accent
/// pill, and its scroll thumb are points. Drawn through [`palette_tray_in`], so it runs on
/// every platform. Two lengths follow measured text, not the scale: the field's clip ends
/// where the count's words begin, and a typed query's caret follows the query. The cell
/// (12 x 24) keeps the box's rounded height and centring whole at both scales (§14.16).
#[test]
fn a_fluent_palette_wears_its_filter_box_pill_and_thumb_in_points() {
    let at = |k: f32| SettingsGeom {
        cw: 12.0 * k,
        ch: 24.0 * k,
        font_px: 18.0 * k,
        cols: 120,
        panel_rows: 40,
        px_per_pt: k,
    };
    let theme = aterm_render::Theme::default;
    let accent = rgba(Roles::from_theme(theme()).accent, 0xFF);
    let mut state = PaletteState::new();
    for typed in ["", "s"] {
        for c in typed.chars() {
            state.push_char(c);
        }
        let draw = |k: f32| palette_tray_in(&state, &at(k), theme(), true).prims;
        let (mut one, mut two) = (draw(1.0), draw(2.0));
        let DrawPrim::Stroke { radius, .. } = one[3] else {
            panic!("the card's border: {:?}", one[3]);
        };
        assert_eq!(radius, 14.0, "the 1x radius is at its cap");
        let bars = |prims: &[DrawPrim], k: f32| {
            prims
                .iter()
                .filter(|p| matches!(p, DrawPrim::Panel { w, .. } if *w == 3.0 * k))
                .count()
        };
        if typed.is_empty() {
            assert_eq!(
                bars(&one, 1.0),
                2,
                "the selected row's pill and the scroll thumb"
            );
        }
        assert!(
            one.iter()
                .any(|p| matches!(p, DrawPrim::Panel { fill, .. } if *fill == accent)),
            "palette {typed:?}: the accent pill and the filter box's accent foot"
        );
        // Blank the two lengths measured text sets: the filter field's clip (the second clip
        // after the box's rim, the first stroke past the card's border) and, typed, its caret.
        let blank_measured = |prims: &mut [DrawPrim]| {
            let rim = 4 + prims[4..]
                .iter()
                .position(|p| matches!(p, DrawPrim::Stroke { .. }))
                .expect("the filter box's rim");
            let clip = rim
                + prims[rim..]
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| matches!(p, DrawPrim::ClipPush { .. }))
                    .nth(1)
                    .map(|(i, _)| i)
                    .expect("the field's clip");
            let caret = clip
                + prims[clip..]
                    .iter()
                    .position(|p| matches!(p, DrawPrim::Panel { .. }))
                    .expect("the caret");
            if let DrawPrim::ClipPush { w, .. } = &mut prims[clip] {
                *w = 0.0;
            }
            if !typed.is_empty()
                && let DrawPrim::Panel { x, .. } = &mut prims[caret]
            {
                *x = 0.0;
            }
        };
        blank_measured(&mut one);
        blank_measured(&mut two);
        assert_drawn_at_scale(&one, &two, 2.0, &[], &format!("fluent palette {typed:?}"));
    }
}

#[test]
fn palette_flattens_every_command() {
    let s = PaletteState::new();
    assert!(s.rows.iter().any(|r| r.action == MenuAction::About));
    assert!(s.rows.iter().any(|r| r.action == MenuAction::Copy));
    assert!(s.rows.iter().any(|r| r.action == MenuAction::Help));
    let model_items = MENU_MODEL
        .iter()
        .flat_map(|sec| sec.entries.iter())
        .flat_map(crate::menu::MenuEntry::items)
        .filter(|(_, action, _, _)| action.offered_on_this_platform())
        .count();
    let palette_only = crate::menu::PALETTE_ONLY
        .iter()
        .filter(|(_, _, action)| action.offered_on_this_platform())
        .count();
    assert_eq!(s.rows.len(), model_items + palette_only);
    let mut chips: Vec<&str> = s.rows.iter().map(|r| r.section.as_ref()).collect();
    chips.dedup();
    let mut bar = vec![
        "aterm", "File", "Edit", "View", "Agents", "Window", "Help", "Version",
    ];
    if cfg!(target_os = "macos") {
        bar.push(crate::menu::MACHINE_SECTION);
    }
    if PALETTE_APP_MENU_LAST {
        bar.rotate_left(1);
    }
    assert_eq!(chips, bar);
    for (section, _, action) in crate::menu::PALETTE_ONLY {
        let rows: Vec<_> = s.rows.iter().filter(|r| r.action == *action).collect();
        if action.offered_on_this_platform() {
            assert_eq!(rows.len(), 1, "{action:?} has one palette row");
            assert_eq!(rows[0].section, *section);
            assert_eq!(s.action_by_name(&format!("{action:?}")), Ok(*action));
        } else {
            assert!(rows.is_empty(), "{action:?} is not offered");
            assert!(s.action_by_name(&format!("{action:?}")).is_err());
        }
    }
}

/// WHERE THE PALETTE OPENS: on Windows the application menu (About, Settings…, Exit) comes after every other section, so the list opens on the everyday commands rather than on About aterm; macOS and Linux keep the menu bar's order.
#[test]
fn the_application_menu_comes_last_on_windows() {
    let s = PaletteState::new();
    let app = MENU_MODEL[0].title;
    let first = s.rows.first().expect("rows");
    let last_section = &s.rows.last().expect("rows").section;
    if cfg!(windows) {
        assert_ne!(first.section, app, "opens on {:?}", first.label);
        assert_eq!(last_section, app, "the application menu is last");
        assert_eq!(first.action, MenuAction::NewWindow);
    } else {
        assert_eq!(first.action, MenuAction::About, "the menu bar's order");
    }
}

/// Windows words: Window ▸ Zoom is Maximize (Restore Down on a maximized window, the caption button's own words) and Quit is Exit; macOS and Linux keep Zoom and Quit aterm.
#[test]
fn window_rows_use_this_platforms_words() {
    let label = |maximized: bool, action: MenuAction| {
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            maximized,
            ..Default::default()
        });
        s.rows
            .iter()
            .find(|r| r.action == action)
            .map(|r| r.label.to_string())
            .expect("the row is listed")
    };
    if cfg!(windows) {
        assert_eq!(label(false, MenuAction::Zoom), "Maximize");
        assert_eq!(label(true, MenuAction::Zoom), "Restore Down");
        assert_eq!(label(false, MenuAction::Quit), "Exit");
    } else {
        assert_eq!(label(false, MenuAction::Zoom), "Zoom");
        assert_eq!(label(true, MenuAction::Zoom), "Zoom");
        assert_eq!(label(false, MenuAction::Quit), "Quit aterm");
    }
}

/// THE AGENTS ROWS resolve from their live bits: Next That Needs You only with a local need, Overview only with a window, Read the Fleet and Fleet Machines only in a fleet, Summarize greyed with its block and titled for the fleet or this Mac; the session rows (Take Over, Hand Back, Harness Log, Set Role…, Reset Terminal) need a front terminal; the reads and the bus commands never grey for want of one.
#[test]
fn the_agents_rows_resolve_from_their_live_bits() {
    let mut s = PaletteState::new();
    let row = |s: &PaletteState, a: MenuAction| {
        s.rows
            .iter()
            .find(|r| r.action == a)
            .cloned()
            .unwrap_or_else(|| panic!("{a:?} has a row"))
    };
    s.resolve(&PaletteLive::default());
    for a in [
        MenuAction::NextNeed,
        MenuAction::Overview,
        MenuAction::ReadFleet,
        MenuAction::FleetMachines,
        MenuAction::TakeOver,
        MenuAction::HandBack,
        MenuAction::HarnessLog,
        MenuAction::SetRole,
        MenuAction::ResetTerminal,
    ] {
        assert!(!row(&s, a).enabled, "{a:?} greys without its fact");
        assert!(
            s.action_by_name(&format!("{a:?}"))
                .is_err_and(|e| e.contains("disabled")),
            "{a:?} refuses with the disabled reason"
        );
    }
    for a in [
        MenuAction::ReadThisMac,
        MenuAction::SummarizeFleet,
        MenuAction::FabricStatus,
        MenuAction::FabricOn,
        MenuAction::FabricOff,
    ] {
        assert!(row(&s, a).enabled, "{a:?} is instance-wide");
    }
    assert_eq!(
        row(&s, MenuAction::SummarizeFleet).label,
        crate::menu::summarize_title(false)
    );
    s.resolve(&PaletteLive {
        terminal_front: true,
        can_rename: true,
        agents: crate::menu::AgentsLive {
            needs_local: true,
            in_fleet: true,
            any_window: true,
            summarize_block: Some("(claude not found)".to_string()),
        },
        ..PaletteLive::default()
    });
    for a in [
        MenuAction::NextNeed,
        MenuAction::Overview,
        MenuAction::ReadFleet,
        MenuAction::FleetMachines,
        MenuAction::TakeOver,
        MenuAction::HarnessLog,
        MenuAction::SetRole,
    ] {
        assert!(row(&s, a).enabled, "{a:?} live with its fact");
    }
    assert!(
        !row(&s, MenuAction::SummarizeFleet).enabled,
        "its block greys it"
    );
    assert_eq!(
        row(&s, MenuAction::SummarizeFleet).label,
        crate::menu::summarize_title(true)
    );
}

/// The person's hold pair follows the live hold — Take Over live without the person's hold, Hand Back live with it (and `invoke` refuses the greyed one as disabled).
#[test]
fn the_person_pair_follows_the_hold() {
    let mut s = PaletteState::new();
    let row = |s: &PaletteState, a: MenuAction| {
        s.rows
            .iter()
            .find(|r| r.action == a)
            .cloned()
            .unwrap_or_else(|| panic!("{a:?} has a row"))
    };
    let live = PaletteLive {
        terminal_front: true,
        can_rename: true,
        ..PaletteLive::default()
    };
    s.resolve(&live);
    assert!(row(&s, MenuAction::TakeOver).enabled);
    assert!(!row(&s, MenuAction::HandBack).enabled);
    s.resolve(&PaletteLive {
        front_person_hold: true,
        ..live.clone()
    });
    assert!(!row(&s, MenuAction::TakeOver).enabled);
    assert!(row(&s, MenuAction::HandBack).enabled);
    assert!(
        s.action_by_name("TakeOver")
            .is_err_and(|e| e.contains("disabled")),
        "a greyed Take Over refuses with the disabled reason"
    );
    s.resolve(&PaletteLive {
        front_person_hold: true,
        front_remote_hold: true,
        ..live.clone()
    });
    assert!(row(&s, MenuAction::TakeOver).enabled);
    assert!(row(&s, MenuAction::HandBack).enabled);
    s.resolve(&live);
    assert!(row(&s, MenuAction::TakeOver).enabled);
    assert!(!row(&s, MenuAction::HandBack).enabled);
}

/// The accelerator projection, both platforms in one table: macOS keeps every string; elsewhere `Cmd-` alternatives drop, a `·`-separated pair keeps its portable half, and a lone `Cmd-*` becomes empty.
#[test]
fn platform_accel_keeps_only_chords_that_exist_here() {
    let cases = [
        ("Cmd-S \u{b7} C-x C-s", "Cmd-S \u{b7} C-x C-s", "C-x C-s"),
        ("Cmd-Z \u{b7} C-/", "Cmd-Z \u{b7} C-/", "C-/"),
        ("Cmd-F", "Cmd-F", ""),
        ("Cmd-Shift-Z", "Cmd-Shift-Z", ""),
        ("C-s", "C-s", "C-s"),
        ("M-g g", "M-g g", "M-g g"),
        ("Shift-F8", "Shift-F8", "Shift-F8"),
        ("Esc", "Esc", "Esc"),
        ("Ctrl-Shift-F", "Ctrl-Shift-F", "Ctrl-Shift-F"),
        ("", "", ""),
    ];
    for (source, mac, other) in cases {
        let want = if cfg!(target_os = "macos") {
            mac
        } else {
            other
        };
        assert_eq!(platform_accel(source), want, "{source:?}");
    }
}

/// `resolve` writes `PaletteLive::menu_accels` chords into menu rows' `shortcut` slot, so `row_accel` shows the chord that works here; an action with no hint keeps its platform default, and a re-resolve without hints clears them.
#[test]
fn resolve_applies_menu_accel_hints_to_row_accel() {
    let by_action = |s: &PaletteState, action: MenuAction| {
        let row = s.rows.iter().find(|r| r.action == action).expect("row");
        row_accel(row)
    };
    let mut s = PaletteState::new();
    s.resolve(&PaletteLive {
        menu_accels: vec![
            (MenuAction::Copy, "Ctrl+Shift+C".to_string()),
            (MenuAction::ToggleFullScreen, "F11".to_string()),
        ],
        has_selection: true,
        ..Default::default()
    });
    assert_eq!(by_action(&s, MenuAction::Copy), "Ctrl+Shift+C");
    assert_eq!(by_action(&s, MenuAction::ToggleFullScreen), "F11");
    let label = |s: &PaletteState| {
        let row = s
            .rows
            .iter()
            .find(|r| r.action == MenuAction::ToggleFullScreen)
            .expect("row");
        (row.label.to_string(), row.checked)
    };
    let mut full = PaletteState::new();
    full.resolve(&PaletteLive {
        fullscreen: true,
        ..Default::default()
    });
    assert_eq!(label(&full), ("Exit Full Screen".to_string(), None));
    full.resolve(&PaletteLive::default());
    assert_eq!(label(&full), ("Enter Full Screen".to_string(), None));
    let close = |s: &PaletteState| {
        s.rows
            .iter()
            .find(|r| r.action == MenuAction::CloseTab)
            .expect("row")
            .label
            .to_string()
    };
    full.resolve(&PaletteLive {
        front_tab_split: true,
        ..Default::default()
    });
    assert_eq!(close(&full), "Close Pane");
    full.resolve(&PaletteLive::default());
    assert_eq!(close(&full), "Close Tab");
    assert_eq!(
        by_action(&s, MenuAction::Paste),
        if cfg!(target_os = "macos") {
            "\u{2318}V"
        } else {
            ""
        }
    );
    s.resolve(&PaletteLive {
        has_selection: true,
        ..Default::default()
    });
    assert_eq!(
        by_action(&s, MenuAction::ToggleFullScreen),
        if cfg!(target_os = "macos") {
            "\u{2303}\u{2318}F"
        } else {
            ""
        }
    );
}

/// [`accel_is_mac_only`] directly (`platform_accel` is `cfg!`-selected, so a table through it runs one column per build), in every spelling of a macOS chord.
#[test]
fn accel_mac_only_catches_every_spelling_of_a_macos_chord() {
    for mac_only in [
        "Cmd-S",
        "Cmd+S",
        "Cmd-Shift-Z",
        "Shift-Cmd-Z",
        "Command-S",
        "\u{2318}S",
        "Shift-\u{2318}S",
        "Opt-Left",
        "Option-Left",
        "\u{2325}\u{2318}C",
    ] {
        assert!(
            accel_is_mac_only(mac_only),
            "{mac_only:?} names a chord that does not exist off macOS"
        );
    }
    for portable in [
        "C-s",
        "C-x C-s",
        "C-/",
        "M-g g",
        "M-x",
        "M-s",
        "Alt-Left",
        "Ctrl-A",
        "Ctrl-Shift-F",
        "Shift-F8",
        "F8",
        "Esc",
        "",
    ] {
        assert!(!accel_is_mac_only(portable), "{portable:?} exists here");
    }
}

#[test]
fn native_commands_prepend_one_scoped_identity_safe_section() {
    let window = crate::WindowId(7);
    let instance = crate::tab_model::AppInstanceId::from_stored(11);
    let view = crate::tab_model::ViewId::from_stored(13);
    let menu = PaletteState::new();
    let menu_save = menu
        .rows
        .iter()
        .find(|row| row.action == MenuAction::Save)
        .unwrap();
    let scope = NativeCommandScope {
        window,
        instance,
        view,
        generation: 17,
        section: "Editor App".to_string(),
        commands: vec![
            Command {
                id: ActionId::new("editor/save"),
                title: "Save".to_string(),
                shortcut: Some("Cmd-S".to_string()),
                enabled: true,
            },
            Command {
                id: ActionId::new("editor/disabled"),
                title: "Unavailable".to_string(),
                shortcut: None,
                enabled: false,
            },
        ],
    };
    let mut s = PaletteState::new().with_native_commands(scope.clone());
    s.resolve(&PaletteLive::default());

    assert_eq!(s.rows[0].section, "Editor App");
    assert_eq!(s.rows[0].label, "Unavailable");
    assert!(!s.rows[0].enabled);
    let saves: Vec<_> = s
        .rows
        .iter()
        .filter(|row| row.menu() == Some(MenuAction::Save))
        .collect();
    assert_eq!(saves.len(), 1, "the shared command is offered only once");
    let save = saves[0];
    assert_eq!(save.section, menu_save.section);
    assert_eq!(save.label, menu_save.label);
    assert_eq!(row_accel(save), row_accel(menu_save));
    assert!(save.enabled, "the captured view owns its enabled metadata");
    assert_eq!(s.action_by_name("Save"), Ok(MenuAction::Save));
    assert!(matches!(
        &save.action,
        PaletteTarget::Native(target)
            if target.window == window
                && target.instance == instance
                && target.view == view
                && target.generation == 17
                && target.action.as_str() == "editor/save"
    ));
    assert!(
        matches!(&s.rows[1].action, PaletteTarget::Menu(_)),
        "global menu order follows the scoped app section"
    );

    let controls = s.controls_lines();
    let expect_line = format!("accel={:?}", row_accel(menu_save));
    assert!(controls.iter().any(|line| {
        line.contains("target=native window=7 instance=11 view=13 generation=17")
            && line.contains("action=editor/save")
            && line.contains(&expect_line)
    }));

    let old_save = save.action.clone();
    let before = s.fingerprint();
    let save_slot = s
        .filtered()
        .iter()
        .position(|&index| s.rows[index].menu() == Some(MenuAction::Save))
        .unwrap();
    let _ = s.pointer_press(Some(save_slot));
    s.replace_native_commands(None);
    s.resolve(&PaletteLive::default());
    assert!(!s.pointer_release(Some(save_slot)).1);
    let save = s
        .rows
        .iter()
        .find(|row| row.menu() == Some(MenuAction::Save))
        .unwrap();
    assert_eq!(save.action, PaletteTarget::Menu(MenuAction::Save));
    assert_eq!(save.section, menu_save.section);
    assert_eq!(save.label, menu_save.label);
    assert!(!save.enabled, "a terminal has no document to save");
    assert!(
        s.rows
            .iter()
            .all(|row| !matches!(row.action, PaletteTarget::Native(_)))
    );

    s.replace_native_commands(Some(NativeCommandScope {
        view: crate::tab_model::ViewId::from_stored(19),
        generation: 23,
        ..scope.clone()
    }));
    let save = s
        .rows
        .iter()
        .find(|row| row.menu() == Some(MenuAction::Save))
        .unwrap();
    assert!(save.enabled);
    assert_ne!(
        save.action, old_save,
        "replacement owns a fresh native target"
    );
    assert_ne!(s.fingerprint(), before);

    let recovery = PaletteState::native_close_recovery(NativeCommandScope {
        view,
        generation: 17,
        ..scope
    });
    assert_eq!(recovery.rows.len(), 2);
    assert!(recovery.is_native_close_recovery_for(window, view));
    assert_eq!(recovery.rows[0].section, "Editor App");
    assert_eq!(recovery.rows[0].label, "Save");
    assert_eq!(recovery.rows[0].menu_alias, None);
}

fn themes() -> Vec<PaletteTheme> {
    [
        ("Default", aterm_types::Appearance::Dark, true),
        ("Gruvbox Dark", aterm_types::Appearance::Dark, false),
        ("Gruvbox Light", aterm_types::Appearance::Light, false),
    ]
    .into_iter()
    .map(|(id, appearance, current)| PaletteTheme {
        id: id.to_string(),
        label: id.to_string(),
        appearance,
        current,
    })
    .collect()
}

/// The Theme section follows the menu rows, each row naming its theme as its target and its appearance as the hint; the scoped palette shows only those rows, starts on the checked one, and its highlight is an edge: asked once per move, never for the row it opened on, `Some(None)` when it leaves the section.
#[test]
fn theme_rows_scope_and_highlight_edge() {
    let mut s = PaletteState::new().with_themes(themes());
    let menu_rows = PaletteState::new().rows.len();
    assert_eq!(s.rows.len(), menu_rows + 4);
    assert!(
        s.rows[..menu_rows]
            .iter()
            .all(|r| r.action.menu().is_some())
    );
    let browse = s.rows.last().expect("the Browse row");
    assert_eq!(browse.section, THEME_SECTION);
    assert!(browse.action == MenuAction::BrowseThemes);
    let rebuilt = PaletteState::new()
        .with_themes(themes())
        .with_themes(themes());
    assert_eq!(rebuilt.rows.len(), menu_rows + 4);
    assert_eq!(
        s.take_theme_highlight_change(),
        None,
        "opening asks nothing"
    );
    let lines = s.controls_lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("target=theme id=\"Gruvbox Light\"")
                && l.contains("section=\"Theme\"")),
        "{lines:?}"
    );
    for c in "gruv".chars() {
        s.push_char(c);
    }
    assert_eq!(s.highlighted_theme(), Some("Gruvbox Dark"));
    assert_eq!(
        s.take_theme_highlight_change(),
        Some(Some("Gruvbox Dark".to_string()))
    );
    assert_eq!(s.take_theme_highlight_change(), None, "unmoved, unasked");
    s.move_selection(1);
    assert_eq!(
        s.take_theme_highlight_change(),
        Some(Some("Gruvbox Light".to_string()))
    );
    while !s.query.is_empty() {
        s.edit(crate::app_search::SearchEdit::DeleteBack);
    }
    assert_eq!(
        s.take_theme_highlight_change(),
        Some(None),
        "left the section"
    );

    let mut scoped = PaletteState::new().with_themes(themes()).themes_only();
    assert!(scoped.is_themes_only());
    assert_eq!(scoped.filtered().len(), 4);
    assert_eq!(scoped.scroll_extent().1, 4, "the count is of the scope");
    assert_eq!(
        scoped.highlighted_theme(),
        Some("Default"),
        "on the checked row"
    );
    assert_eq!(scoped.take_theme_highlight_change(), None);
    scoped.move_selection(-1);
    assert_eq!(
        scoped.highlighted_theme(),
        None,
        "wraps in scope, onto Browse all themes…"
    );
    assert_eq!(scoped.take_theme_highlight_change(), Some(None));
    scoped.move_selection(-1);
    assert_eq!(scoped.highlighted_theme(), Some("Gruvbox Light"));
    scoped.replace_native_commands(None);
    assert_eq!(scoped.filtered().len(), 4);
}

/// The theme picker previews on the terminal behind the card, so the palette lifts its scrim while it picks a theme: scoped to the Theme section, or with a Theme row highlighted.
#[test]
fn the_scrim_lifts_while_the_palette_picks_a_theme() {
    use crate::overlay::OverlayModel as _;
    assert_eq!(palette_scrim().is_some(), cfg!(windows));
    let mut s = PaletteState::new().with_themes(themes());
    assert_eq!(s.highlighted_theme(), None);
    assert_eq!(
        s.scrim(),
        palette_scrim(),
        "the command list dims the window"
    );
    for c in "gruv".chars() {
        s.push_char(c);
    }
    assert_eq!(s.highlighted_theme(), Some("Gruvbox Dark"));
    assert_eq!(s.scrim(), None, "a highlighted theme previews undimmed");
    while !s.query.is_empty() {
        s.edit(crate::app_search::SearchEdit::DeleteBack);
    }
    assert_eq!(s.highlighted_theme(), None);
    assert_eq!(s.scrim(), palette_scrim(), "back on a command");

    let mut scoped = PaletteState::new().with_themes(themes()).themes_only();
    assert_eq!(scoped.scrim(), None, "Choose Theme…");
    scoped.move_selection(-1);
    assert_eq!(scoped.highlighted_theme(), None, "on Browse all themes…");
    assert_eq!(scoped.scrim(), None, "the whole scope stays undimmed");
}

#[test]
fn filter_narrows_the_list() {
    let mut s = PaletteState::new();
    let all = s.filtered().len();
    for c in "settings".chars() {
        s.push_char(c);
    }
    let narrowed = s.filtered();
    assert!(narrowed.len() < all, "filtering narrows the set");
    assert!(
        narrowed
            .iter()
            .any(|&i| s.rows[i].action == MenuAction::ToggleSettings),
        "the row named for it survives"
    );
    assert!(
        narrowed.iter().all(|&i| fuzzy_subsequence(
            "settings",
            s.rows[i]
                .label
                .chars()
                .chain("  ".chars())
                .chain(s.rows[i].section.chars())
                .map(|c| c.to_ascii_lowercase())
        )),
        "every survivor is a subsequence match"
    );
    s.edit(crate::app_search::SearchEdit::DeleteBack);
    assert!(s.filtered().len() >= narrowed.len());
}

/// The filter edits through the shared one-line field model with its caret held at the end: typing appends, the backward kills delete, and the motions and forward kills (which need a caret before the end) change nothing.
#[test]
fn edit_applies_the_field_model_with_the_caret_at_the_end() {
    use crate::app_search::SearchEdit;
    let mut s = PaletteState::new();
    assert!(s.edit(SearchEdit::Insert("new e".to_string())));
    assert_eq!(s.query, "new e");
    assert!(s.edit(SearchEdit::DeleteBack));
    assert_eq!(s.query, "new ");
    assert!(s.filtered().len() > 1, "{:?}", s.controls_lines());
    s.move_selection(1);
    assert_eq!(s.selected, 1);
    for unchanged in [
        SearchEdit::MoveStart,
        SearchEdit::MoveEnd,
        SearchEdit::MoveCharLeft,
        SearchEdit::MoveWordLeft,
        SearchEdit::KillToEnd,
        SearchEdit::DeleteForward,
        SearchEdit::DeleteWordForward,
    ] {
        assert!(!s.edit(unchanged.clone()), "{unchanged:?}");
        assert_eq!(s.query, "new ", "{unchanged:?}");
    }
    assert_eq!(
        s.selected, 1,
        "an edit that changed nothing keeps the cursor"
    );
    assert!(s.edit(SearchEdit::Insert("tab".to_string())));
    assert_eq!(s.query, "new tab");
    assert_eq!(s.selected, 0, "a changed query re-narrows from the top");
    assert!(s.edit(SearchEdit::DeleteWordBack));
    assert_eq!(s.query, "new ");
    assert!(s.edit(SearchEdit::KillToStart));
    assert_eq!(s.query, "");
    assert!(!s.edit(SearchEdit::DeleteBack), "nothing left to delete");
}

#[test]
fn activate_yields_the_selected_action() {
    let mut s = PaletteState::new();
    for c in "copy".chars() {
        s.push_char(c);
    }
    assert_eq!(s.selected_action(), Some(MenuAction::Copy));
    let live = PaletteLive::default();
    s.resolve(&live);
    assert_eq!(
        s.selected_action(),
        None,
        "a disabled command does not activate"
    );
    s.resolve(&PaletteLive {
        has_selection: true,
        ..Default::default()
    });
    assert_eq!(s.selected_action(), Some(MenuAction::Copy));
}

#[test]
fn serious_mode_row_tracks_process_policy_on_every_content_kind() {
    let mut state = PaletteState::new();
    state.resolve(&PaletteLive {
        serious_mode: true,
        native_tab_active: true,
        ..Default::default()
    });
    let serious = state
        .rows
        .iter()
        .find(|row| row.action == MenuAction::ToggleSeriousMode)
        .expect("View menu contributes Serious Mode");
    assert_eq!(serious.checked, Some(true));
    assert!(
        serious.enabled,
        "the process-wide policy is not terminal-only"
    );

    state.resolve(&PaletteLive::default());
    let serious = state
        .rows
        .iter()
        .find(|row| row.action == MenuAction::ToggleSeriousMode)
        .unwrap();
    assert_eq!(serious.checked, Some(false));
    assert!(serious.enabled, "it remains available while windowless");
}

/// The View ▸ Matrix Rain row checks with the front session's effective rain and disables with no front terminal (a native whole tab, or no window).
#[test]
fn matrix_rain_row_checkmark_tracks_front_session_and_disables_native() {
    let mut s = PaletteState::new();
    s.resolve(&PaletteLive {
        rain_on: true,
        terminal_front: true,
        can_rename: true,
        ..Default::default()
    });
    let rain = s
        .rows
        .iter()
        .find(|r| r.action == MenuAction::ToggleMatrixRain)
        .expect("View menu contributes Matrix Rain");
    assert_eq!(rain.checked, Some(true));
    assert!(rain.enabled, "terminal front: toggleable");

    s.resolve(&PaletteLive {
        rain_on: false,
        native_tab_active: true,
        ..Default::default()
    });
    let rain = s
        .rows
        .iter()
        .find(|r| r.action == MenuAction::ToggleMatrixRain)
        .unwrap();
    assert_eq!(rain.checked, Some(false));
    assert!(!rain.enabled, "native whole tab: no session to toggle");

    s.resolve(&PaletteLive::default());
    let rain = s
        .rows
        .iter()
        .find(|r| r.action == MenuAction::ToggleMatrixRain)
        .unwrap();
    assert!(
        !rain.enabled,
        "no window: nothing to toggle, refuse honestly"
    );
}

/// The View ▸ Favourite This Kitty row checks when the launch kitty is the pin and, unlike the rain toggle, stays enabled with no front terminal: the launch kitty is process-wide.
#[test]
fn favourite_row_checks_when_pinned_and_stays_enabled_without_a_terminal() {
    let mut s = PaletteState::new();
    s.resolve(&PaletteLive {
        kitty_favourited: true,
        terminal_front: true,
        can_rename: true,
        ..Default::default()
    });
    let row = |s: &PaletteState| {
        s.rows
            .iter()
            .find(|r| r.action == MenuAction::FavouriteKitty)
            .map(|r| (r.checked, r.enabled))
            .expect("View menu contributes Favourite This Kitty")
    };
    assert_eq!(
        row(&s),
        (Some(true), true),
        "terminal front: pinned and promotable"
    );

    s.resolve(&PaletteLive {
        native_tab_active: true,
        ..Default::default()
    });
    assert_eq!(
        row(&s),
        (Some(false), true),
        "native whole tab: the launch kitty is still there to pin"
    );

    s.resolve(&PaletteLive::default());
    assert_eq!(
        row(&s),
        (Some(false), true),
        "no window: the launch kitty is process-wide, still promotable"
    );
}

#[test]
fn reopen_closed_tab_row_tracks_live_snapshot_availability() {
    let mut s = PaletteState::new();
    s.resolve(&PaletteLive::default());
    let reopen = s
        .rows
        .iter()
        .find(|row| row.action == MenuAction::ReopenClosedTab)
        .expect("File menu contributes Reopen Closed Native Tab");
    assert!(!reopen.enabled);

    s.resolve(&PaletteLive {
        can_reopen_closed_tab: true,
        ..PaletteLive::default()
    });
    assert!(
        s.rows
            .iter()
            .find(|row| row.action == MenuAction::ReopenClosedTab)
            .unwrap()
            .enabled
    );
}

#[test]
fn local_file_open_rows_track_platform_picker_availability() {
    let mut state = PaletteState::new();
    let actions = [MenuAction::OpenEditor];

    state.resolve(&PaletteLive {
        local_file_picker_available: false,
        ..PaletteLive::default()
    });
    for action in actions {
        let row = state.rows.iter().find(|row| row.action == action).unwrap();
        assert!(!row.enabled, "{action:?} disabled without a picker");
    }

    state.resolve(&PaletteLive {
        local_file_picker_available: true,
        ..PaletteLive::default()
    });
    for action in actions {
        let row = state.rows.iter().find(|row| row.action == action).unwrap();
        assert!(row.enabled, "{action:?} enabled with a picker");
    }
}

#[test]
fn move_wraps_over_filtered_set() {
    let mut s = PaletteState::new();
    for c in "tab".chars() {
        s.push_char(c);
    }
    let n = s.filtered().len();
    assert!(n >= 2, "expected several tab commands");
    s.move_selection(-1);
    assert_eq!(s.selected, n - 1, "up from row 0 wraps to the last row");
    s.move_selection(1);
    assert_eq!(s.selected, 0, "and back to the top");
}

#[test]
fn fingerprint_tracks_query_and_cursor() {
    let mut s = PaletteState::new();
    let a = s.fingerprint();
    assert_ne!(
        a, 0,
        "an open overlay never hashes to the closed sentinel 0"
    );
    s.push_char('f');
    let b = s.fingerprint();
    assert_ne!(b, a, "typing changes the fingerprint");
    s.move_selection(1);
    assert_ne!(s.fingerprint(), b, "moving changes the fingerprint");
}

/// Staged: resolve adds the one-click "↑ Install aterm v<staged> now" row at the head of the Version section, enabled, full-alpha, and activatable to `ApplyUpdate`.
#[test]
fn resolve_staged_adds_one_click_update_row() {
    let model = aterm_models_misc::native_update_menu_activation_model();
    let mut model_state = model.init_state();
    assert!(model.fire("StageUpdate", &mut model_state));

    let mut s = PaletteState::new();
    s.resolve(&PaletteLive {
        staged: Some((999, "9.9".to_string())),
        ..Default::default()
    });
    assert!(model.fire("RefreshStagedVersionMenu", &mut model_state));
    let i = s
        .rows
        .iter()
        .position(|r| r.action == MenuAction::ApplyUpdate)
        .expect("staged resolve keeps the ApplyUpdate row");
    let row = &s.rows[i];
    assert_eq!(row.section, "Version");
    assert_eq!(row.label, "\u{2191} Install aterm v9.9 now");
    assert!(!row.label.contains("restart"), "{}", row.label);
    assert!(row.enabled, "one click must work");
    assert_eq!(s.rows[i + 1].action, MenuAction::Version);
    assert_eq!(s.row_alpha(&s.rows[i], Instant::now()), 1.0);
    for c in "install aterm".chars() {
        s.push_char(c);
    }
    assert_eq!(s.selected_action(), Some(MenuAction::ApplyUpdate));
    assert!(model.fire("DecodeApplyTag", &mut model_state));
    assert!(model.fire("DispatchApply", &mut model_state));
    assert_eq!(model_state["apply_dispatched"], 1);
    assert!(
        s.controls_lines()
            .iter()
            .any(|l| l.contains("action=ApplyUpdate") && l.contains("Install aterm v9.9")),
        "controls menu shows the staged row"
    );
}

/// Neither staged nor realized: the ApplyUpdate row is removed, and a later resolve with a staged build re-inserts it.
#[test]
fn resolve_without_update_state_removes_the_row() {
    let mut s = PaletteState::new();
    assert_eq!(
        s.rows.iter().any(|r| r.action == MenuAction::ApplyUpdate),
        MenuAction::ApplyUpdate.offered_on_this_platform(),
        "the static model carries the row where an updater exists"
    );
    s.resolve(&PaletteLive::default());
    assert!(
        !s.rows.iter().any(|r| r.action == MenuAction::ApplyUpdate),
        "no update state ⇒ no update row"
    );
    s.resolve(&PaletteLive {
        staged: Some((999, "9.9".to_string())),
        ..Default::default()
    });
    assert!(s.rows.iter().any(|r| r.action == MenuAction::ApplyUpdate));
}

/// Update commands remain available in the palette and control interface
/// where the platform has an updater, without a separate menu-bar row.
#[test]
fn update_rows_follow_the_platform_updater_on_every_surface() {
    let mut s = PaletteState::new();
    s.resolve(&PaletteLive::default());
    let row = s
        .rows
        .iter()
        .find(|r| r.action == MenuAction::SoftwareUpdate);
    let enabled = row.is_some_and(|r| r.enabled);
    assert_eq!(
        row.is_some(),
        enabled,
        "listed means actionable: no permanently grey row"
    );
    assert_eq!(
        enabled,
        MenuAction::SoftwareUpdate.offered_on_this_platform()
    );
    assert_eq!(enabled, aterm_update::enabled());
    if cfg!(windows) {
        assert!(!enabled, "Windows has no in-app updater");
        assert!(
            s.action_by_name("SoftwareUpdate").is_err(),
            "`invoke` refuses it"
        );
    }
    let chrome =
        crate::menu::menu_chrome_lines(apply_row_label(&PaletteLive::default()).as_deref(), false);
    let chrome_offers = chrome
        .iter()
        .any(|line| line.contains("Check for Updates…"));
    assert!(
        !chrome.iter().any(|line| line.contains("Install")),
        "nothing staged: `chrome` lists no install row either: {chrome:?}"
    );
    assert!(!chrome_offers, "updates live inside Settings: {chrome:?}");
    assert_eq!(
        s.controls_lines()
            .iter()
            .any(|line| line.contains("action=SoftwareUpdate")),
        enabled,
        "`controls menu` lists it exactly where it is offered"
    );
    assert!(
        !s.rows.iter().any(|r| r.action == MenuAction::ApplyUpdate),
        "nothing staged, so no apply-now row"
    );
}

/// Realized: the post-update row fades (full at boot, partial mid-TTL, zero at TTL), freezes at full under reduced motion, and yields to a staged build.
#[test]
fn resolve_realized_adds_fading_row() {
    use crate::relaunch_notice::REALIZED_ARROW_TTL;
    let now = Instant::now();
    let mut s = PaletteState::new();
    s.resolve(&PaletteLive {
        realized: Some(("9.9".to_string(), now)),
        ..Default::default()
    });
    let row = s
        .rows
        .iter()
        .find(|r| r.action == MenuAction::ApplyUpdate)
        .expect("realized resolve keeps the row");
    assert_eq!(row.label, "\u{2191} Updated to aterm v9.9");
    assert_eq!(s.row_alpha(row, now), 1.0, "full at spawn");
    let mid = s.row_alpha(row, now + REALIZED_ARROW_TTL / 2);
    assert!(mid > 0.0 && mid < 1.0, "mid-TTL alpha {mid}");
    assert_eq!(s.row_alpha(row, now + REALIZED_ARROW_TTL), 0.0, "expired");
    let about = s
        .rows
        .iter()
        .find(|r| r.action == MenuAction::Version)
        .unwrap();
    assert_eq!(s.row_alpha(about, now + REALIZED_ARROW_TTL / 2), 1.0);
    s.resolve(&PaletteLive {
        realized: Some(("9.9".to_string(), now)),
        reduced_motion: true,
        ..Default::default()
    });
    let frozen = s
        .rows
        .iter()
        .find(|r| r.action == MenuAction::ApplyUpdate)
        .unwrap();
    assert_eq!(s.row_alpha(frozen, now + REALIZED_ARROW_TTL / 2), 1.0);
    s.resolve(&PaletteLive {
        staged: Some((999, "10.0".to_string())),
        realized: Some(("9.9".to_string(), now)),
        ..Default::default()
    });
    let row = s
        .rows
        .iter()
        .find(|r| r.action == MenuAction::ApplyUpdate)
        .unwrap();
    assert!(row.label.contains("Install aterm v10.0"), "{:?}", row.label);
}

/// The fingerprint folds the realized row's elapsed bucket while unexpired, so an open palette re-presents per fade step; under reduced motion it does not.
#[test]
fn fingerprint_folds_the_realized_bucket() {
    use std::time::Duration;

    use crate::relaunch_notice::REALIZED_BUCKET;
    let mk = |since: Instant, reduced: bool| {
        let mut s = PaletteState::new();
        s.resolve(&PaletteLive {
            realized: Some(("9.9".to_string(), since)),
            reduced_motion: reduced,
            ..Default::default()
        });
        s.fingerprint()
    };
    let now = Instant::now();
    let bucket0 = mk(now, false);
    let bucket1 = mk(now - REALIZED_BUCKET - Duration::from_secs(1), false);
    assert_ne!(
        bucket0, bucket1,
        "a bucket edge re-fingerprints (→ one present)"
    );
    let frozen0 = mk(now, true);
    let frozen1 = mk(now - REALIZED_BUCKET - Duration::from_secs(1), true);
    assert_eq!(frozen0, frozen1, "reduced motion pins the fp");
}

#[test]
fn floating_card_is_centered_width_capped_and_clamped_on_small_viewports() {
    let state = PaletteState::new();
    assert_eq!(
        <PaletteState as crate::overlay::OverlayModel>::wanted_rows(&state, 40),
        40,
        "the floating card receives the whole viewport for centering"
    );
    let wide = SettingsGeom {
        cw: 9.0,
        ch: 20.0,
        font_px: 14.0,
        cols: 200,
        panel_rows: 40,
        px_per_pt: 1.0,
    };
    let layout = palette_layout(&state, &wide);
    let (x, y, width, height) = layout.card;
    let tray_w = wide.cols as f32 * wide.cw;
    let tray_h = wide.panel_rows as f32 * wide.ch;
    assert_eq!(width, MAX_CARD_WIDTH_PT);
    assert_eq!(height, state.wanted_rows() as f32 * wide.ch);
    assert!(((x * 2.0 + width) - tray_w).abs() < f32::EPSILON);
    assert!(((y * 2.0 + height) - tray_h).abs() < f32::EPSILON);

    let small = SettingsGeom {
        cw: 9.0,
        ch: 20.0,
        font_px: 14.0,
        cols: 3,
        panel_rows: 2,
        px_per_pt: 1.0,
    };
    let layout = palette_layout(&state, &small);
    let (x, y, width, height) = layout.card;
    let tray_w = small.cols as f32 * small.cw;
    let tray_h = small.panel_rows as f32 * small.ch;
    assert!(x >= 0.0 && y >= 0.0);
    assert!(x + width <= tray_w && y + height <= tray_h);
    assert_eq!(width, small.cw * 2.0, "margins collapse before the card");
    assert_eq!(height, tray_h, "height clamps to the available rows");
    assert_eq!(layout.body_rows, 0, "chrome cannot overlap command rows");
}

/// The same window at backing scale `k`: its cells, type and width cap scaled together.
fn geom_at(k: f32) -> SettingsGeom {
    SettingsGeom {
        cw: 9.0 * k,
        ch: 20.0 * k,
        font_px: 14.0 * k,
        cols: 144,
        panel_rows: 40,
        px_per_pt: k,
    }
}

/// A window too narrow for the cap at backing scale `k`, whose cell (7 points) makes two cells
/// less than the margin's 16-point floor, so the floor decides the card's width.
fn narrow_at(k: f32) -> SettingsGeom {
    SettingsGeom {
        cw: 7.0 * k,
        cols: 60,
        ..geom_at(k)
    }
}

#[test]
fn the_card_is_one_width_in_points_at_1x_and_2x_and_says_the_same() {
    let state = PaletteState::new();
    for at in [geom_at, narrow_at] {
        let (g1, g2) = (at(1.0), at(2.0));
        let (c1, c2) = (
            palette_layout(&state, &g1).card,
            palette_layout(&state, &g2).card,
        );
        for (one, two) in [(c1.0, c2.0), (c1.1, c2.1), (c1.2, c2.2), (c1.3, c2.3)] {
            assert!(
                (two / g2.px_per_pt - one).abs() < 0.01,
                "the 2x card {c2:?} is the 1x card {c1:?} in points"
            );
        }
        let words = |g: &SettingsGeom| -> Vec<String> {
            texts(&palette_tray(&state, g, Theme::default()))
                .into_iter()
                .map(|(s, ..)| s)
                .collect()
        };
        assert_eq!(
            words(&g2),
            words(&g1),
            "the 2x card cuts its rows where the 1x card does"
        );
    }
    let card_w = |g: SettingsGeom| palette_layout(&state, &g).card.2;
    assert_eq!(
        card_w(geom_at(1.0)),
        MAX_CARD_WIDTH_PT,
        "the wide card at its widest"
    );
    let narrow = narrow_at(1.0);
    assert_eq!(
        card_w(narrow),
        narrow.cols as f32 * narrow.cw - 2.0 * 16.0,
        "the narrow card: the tray less a 16-point margin on each side"
    );
}

#[test]
fn every_painted_row_is_contained_by_the_card_and_outside_points_miss() {
    let state = PaletteState::new();
    let geom = SettingsGeom {
        cw: 9.0,
        ch: 20.0,
        font_px: 14.0,
        cols: 180,
        panel_rows: 36,
        px_per_pt: 1.0,
    };
    let layout = palette_layout(&state, &geom);
    let (card_x, card_y, card_w, card_h) = layout.card;
    for slot in 0..layout.body_rows {
        let (x, y, width, height) = palette_row_rect(&state, &geom, slot).unwrap();
        assert!(x >= card_x && y >= card_y);
        assert!(x + width <= card_x + card_w);
        assert!(y + height <= card_y + card_h);
    }
    assert_eq!(
        palette_row_hit(&state, &geom, card_x - 1.0, card_y + card_h * 0.5),
        None
    );
    assert_eq!(
        palette_row_hit(&state, &geom, card_x + card_w + 1.0, card_y + card_h * 0.5,),
        None
    );
    assert_eq!(palette_row_hit(&state, &geom, card_x, card_y - 1.0), None);
    assert_eq!(
        palette_row_hit(&state, &geom, card_x, card_y + card_h + 1.0),
        None
    );
}

#[test]
fn pointer_hit_rect_is_the_exact_visible_selection_rect_even_when_scrolled() {
    let mut state = PaletteState::new();
    state.selected = state.filtered().len() - 1;
    state.clamp_scroll();
    assert!(state.scroll > 0, "fixture exercises the scrolled band");
    let geom = SettingsGeom {
        cw: 9.0,
        ch: 20.0,
        font_px: 14.0,
        cols: 80,
        panel_rows: state.wanted_rows(),
        px_per_pt: 1.0,
    };

    for slot in 0..state.body() {
        let (x, y, width, height) = palette_row_rect(&state, &geom, slot).unwrap();
        assert_eq!(
            palette_row_hit(&state, &geom, x + width * 0.5, y + height * 0.5),
            Some(state.scroll + slot)
        );
    }
    let (x, y, width, height) = palette_row_rect(&state, &geom, 0).unwrap();
    assert_eq!(palette_row_hit(&state, &geom, x - 0.01, y), None);
    assert_eq!(palette_row_hit(&state, &geom, x + width, y), None);
    assert_eq!(palette_row_hit(&state, &geom, x, y - 0.01), None);
    assert_eq!(palette_row_hit(&state, &geom, x, y + height), None);

    state.selected = state.scroll;
    let tray = palette_tray(&state, &geom, Theme::default());
    assert!(tray.prims.iter().any(|primitive| {
        matches!(primitive, DrawPrim::Panel { x: px, y: py, w, h, .. }
            if *px == x && *py == y && *w == width && *h == height)
    }));
}

#[test]
fn pointer_release_requires_same_exact_enabled_target() {
    let mut state = PaletteState::new();
    assert!(state.pointer_press(Some(0)));
    let (_, activate) = state.pointer_release(Some(1));
    assert!(
        !activate,
        "pressing one row and releasing on another is inert"
    );

    let _ = state.pointer_press(Some(1));
    let (_, activate) = state.pointer_release(Some(1));
    assert!(activate, "same enabled target activates on release");

    for c in "copy".chars() {
        state.push_char(c);
    }
    state.resolve(&PaletteLive::default());
    let copy = state
        .filtered()
        .iter()
        .position(|&row| state.rows[row].action == MenuAction::Copy)
        .unwrap();
    let _ = state.pointer_press(Some(copy));
    let (_, activate) = state.pointer_release(Some(copy));
    assert!(!activate, "disabled rows remain pointer-inert");
}

/// The palette's geometry at 1x on the Windows shipping font: a 23-pixel row.
fn fluent_geom() -> SettingsGeom {
    SettingsGeom {
        cw: 9.0,
        ch: 23.0,
        font_px: 14.0,
        cols: 111,
        panel_rows: 35,
        px_per_pt: 1.0,
    }
}

/// Every Text prim of a tray as `(string, baseline, size, ink)`.
fn texts(tray: &TrayInput) -> Vec<(String, f32, f32, [u8; 3])> {
    tray.prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Text {
                s,
                baseline,
                px,
                color,
                ..
            } => Some((s.clone(), *baseline, *px, [color[0], color[1], color[2]])),
            _ => None,
        })
        .collect()
}

fn light_theme() -> Theme {
    Theme::from(&aterm_types::scheme::builtin("Default Light").expect("a built-in"))
}

/// SECONDARY WORDS WEAR THE SECONDARY INK, DISABLED ROWS ALONE THE GREY: in a Fluent palette ([`PALETTE_FLUENT`]) the placeholder, the count, an enabled row's section chip and the footer are `text_secondary` (AA on the card); only a disabled row's label and chip keep `text_tertiary`.
#[test]
fn fluent_secondary_words_wear_the_secondary_ink() {
    let g = fluent_geom();
    for theme in [Theme::default(), light_theme()] {
        let r = Roles::from_theme(theme);
        assert_ne!(r.text_secondary, r.text_tertiary);
        let want = if PALETTE_FLUENT {
            r.text_secondary
        } else {
            r.text_tertiary
        };
        let mut state = PaletteState::new();
        state.resolve(&PaletteLive::default());
        let tray = palette_tray(&state, &g, theme);
        let t = texts(&tray);
        let ink_of = |needle: &dyn Fn(&str) -> bool| {
            t.iter()
                .find(|(s, ..)| needle(s))
                .map(|&(_, _, _, ink)| ink)
                .expect("the word is painted")
        };
        assert_eq!(
            ink_of(&|s| s.contains("Esc close") || s.contains("esc close")),
            want
        );
        let count = format!("{}/{}", state.filtered().len(), state.scope_len());
        assert_eq!(ink_of(&|s| s == count), want, "the count");
        let placeholder = if PALETTE_FLUENT {
            "Search commands"
        } else {
            "\u{203a} type to filter"
        };
        assert_eq!(ink_of(&|s| s == placeholder), want, "the placeholder");
        let (mut enabled, mut disabled) = (0, 0);
        for &idx in state.filtered().iter().take(state.body()) {
            let row = &state.rows[idx];
            let Some(&(_, base, _, label_ink)) = t.iter().find(|(s, ..)| *s == row.label) else {
                continue;
            };
            let chip = t
                .iter()
                .find(|(s, b, ..)| *s == row.section && (*b - base).abs() < 0.01)
                .map(|&(_, _, _, ink)| ink)
                .expect("the row's chip");
            if row.enabled {
                enabled += 1;
                assert_eq!(label_ink, r.text_primary, "{}", row.label);
                assert_eq!(chip, want, "{}: an enabled row's chip", row.label);
            } else {
                disabled += 1;
                assert_eq!(label_ink, r.text_tertiary, "{}", row.label);
                assert_eq!(
                    chip, r.text_tertiary,
                    "{}: a disabled row's chip",
                    row.label
                );
            }
        }
        assert!(
            enabled > 3 && disabled > 0,
            "{enabled} enabled, {disabled} disabled"
        );
    }
}

/// THE SEARCH FIELD'S RIM HOLDS 3:1 ON THE CARD (audit r17 CA1, r20 MC1): the field's fill is the card's own tone (or a few percent off it), so its rim is its only boundary and wears the control edge every Settings field wears, never the 1.5:1 separator, in a dark and two light themes, in the palette this platform draws and in the Fluent filter box.
#[test]
fn the_search_fields_rim_holds_three_to_one_on_the_card() {
    use crate::native_appearance::contrast_ratio;
    let github = Theme::from(&aterm_types::scheme::builtin("GitHub Light").expect("built-in"));
    let g = fluent_geom();
    for theme in [Theme::default(), light_theme(), github] {
        let r = Roles::from_theme(theme);
        let state = PaletteState::new();
        let card = palette_layout(&state, &g).card;
        let (card_x, card_y, card_w, _) = card;
        let in_field = |prims: &[DrawPrim]| -> Vec<[u8; 4]> {
            prims
                .iter()
                .filter_map(|prim| match *prim {
                    DrawPrim::Stroke {
                        x, y, w, h, color, ..
                    } if x > card_x
                        && w < card_w - 2.0
                        && y > card_y
                        && y + h <= card_y + 2.0 * g.ch + 0.5 =>
                    {
                        Some(color)
                    }
                    _ => None,
                })
                .collect()
        };
        let mut filter_box = Vec::new();
        palette_filter_box(
            &mut filter_box,
            &state,
            &g,
            &r,
            card,
            state.filtered().len(),
        );
        for (layout, prims) in [
            (
                "this platform's palette",
                palette_tray(&state, &g, theme).prims,
            ),
            ("the Fluent filter box", filter_box),
        ] {
            let rims = in_field(&prims);
            assert_eq!(rims.len(), 1, "{theme:?}, {layout}: the field's one rim");
            let [red, green, blue, alpha] = rims[0];
            let over = |ink: u8, ground: u8| {
                ((f32::from(ink) * f32::from(alpha)
                    + f32::from(ground) * (255.0 - f32::from(alpha)))
                    / 255.0)
                    .round() as u8
            };
            let shown = [
                over(red, r.surface[0]),
                over(green, r.surface[1]),
                over(blue, r.surface[2]),
            ];
            let ratio = contrast_ratio(shown, r.surface);
            assert!(
                ratio >= 3.0,
                "{theme:?}, {layout}: the rim is {ratio:.2}:1 on the card"
            );
        }
    }
}

/// THE SELECTED ROW IS A FILL AND A PILL in a Fluent palette, as the Settings rail selects: a neutral fill on the row's rectangle and a 3-wide accent pill at its leading edge, no accent outline.
#[test]
fn fluent_selection_is_a_neutral_fill_and_an_accent_pill() {
    let g = fluent_geom();
    for theme in [Theme::default(), light_theme()] {
        let r = Roles::from_theme(theme);
        let state = PaletteState::new();
        let (x, y, w, h) = palette_row_rect(&state, &g, state.selected).unwrap();
        let tray = palette_tray(&state, &g, theme);
        let accent_ring = tray
            .prims
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Stroke { color, .. } if color[..3] == r.accent));
        let fill = tray.prims.iter().find_map(|prim| match prim {
            DrawPrim::Panel {
                x: px,
                y: py,
                w: pw,
                h: ph,
                fill,
                ..
            } if (*px, *py, *pw, *ph) == (x, y, w, h) => Some(*fill),
            _ => None,
        });
        let pill = tray.prims.iter().any(|prim| {
            matches!(prim, DrawPrim::Panel { x: px, w: pw, h: ph, fill, .. }
                if *px == x && *pw == 3.0 && *ph < h && fill[..3] == r.accent)
        });
        if PALETTE_FLUENT {
            assert_eq!(fill, Some(rgba(palette_selected_fill(&r), 0xFF)));
            assert!(pill, "the accent pill");
            assert!(!accent_ring, "no accent outline");
        } else {
            assert_eq!(fill, Some(rgba(r.accent, 0x22)));
            assert!(!pill && accent_ring);
        }
    }
}

/// THE FILTER BOX HOLDS ITS WORDS: a Fluent palette's filter box is one and a half rows tall, and the placeholder's ascenders and descenders and the count sit inside it, clear of its hairline and its focus line, with the accent focus line along its foot.
#[test]
fn fluent_filter_box_holds_its_words() {
    let g = fluent_geom();
    let body = TypeStep::Body.px(g.font_px).get();
    let old_bottom = g.ch + 2.0 + (g.ch - 4.0);
    assert!(
        row_baseline(g.ch, g.ch, body) + body * 0.25 > old_bottom - 3.0,
        "the old field's descenders crowd its stroke"
    );
    if !PALETTE_FLUENT {
        return;
    }
    for theme in [Theme::default(), light_theme()] {
        let r = Roles::from_theme(theme);
        let state = PaletteState::new();
        let tray = palette_tray(&state, &g, theme);
        let control = rgba(
            crate::native_ui::control_rest_fill_over(r.surface, &r),
            0xFF,
        );
        let (bx, by, bw, bh) = tray
            .prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Panel {
                    x, y, w, h, fill, ..
                } if *fill == control => Some((*x, *y, *w, *h)),
                _ => None,
            })
            .expect("the filter box");
        assert!(bh >= g.ch * 1.4, "{bh}");
        let (card_x, card_y, card_w, _) = palette_layout(&state, &g).card;
        assert!(
            by > card_y && by + bh < card_y + 2.0 * g.ch,
            "over the top two rows"
        );
        let row0 = palette_row_rect(&state, &g, 0).unwrap();
        assert_eq!((bx, bw), (row0.0, row0.2), "aligned with the command rows");
        assert!(bx > card_x && bx + bw < card_x + card_w);
        let count = format!("{}/{}", state.filtered().len(), state.scope_len());
        for (s, base, px, _) in texts(&tray)
            .into_iter()
            .filter(|(s, ..)| s == "Search commands" || *s == count)
        {
            assert!(base - px * 0.8 >= by + 2.0, "{s}: ascenders inside");
            assert!(
                base + px * 0.25 <= by + bh - 4.0,
                "{s}: descenders clear the foot"
            );
        }
        assert!(
            tray.prims.iter().any(|prim| matches!(prim,
                DrawPrim::Panel { y, h, fill, .. }
                    if *y == by && *h == bh && fill[..3] == r.accent)),
            "the accent focus line, clipped to the foot"
        );
        assert!(
            !texts(&tray).iter().any(|(s, ..)| s == "Commands"),
            "no title: the placeholder names the scope"
        );
    }
}

#[test]
fn wheel_scroll_clamps_at_both_ends_and_keeps_selection_visible() {
    let mut state = PaletteState::new();
    let count = state.filtered().len();
    let body = state.body();
    assert!(count > body, "fixture has a scrollable command set");

    assert!(state.scroll_by(isize::MAX));
    assert_eq!(state.scroll, count - body, "bottom clamp");
    assert!(state.selected >= state.scroll);
    assert!(state.selected < state.scroll + body);
    assert!(!state.scroll_by(1), "wheel cannot move beyond bottom");

    assert!(state.scroll_by(isize::MIN));
    assert_eq!(state.scroll, 0, "top clamp");
    assert!(state.selected < body);
    assert!(!state.scroll_by(-1), "wheel cannot move beyond top");
}

/// The a11y MenuItem set equals `filtered()` (the rows the card paints and `controls menu` counts as `shown=`), focus follows `selected`, and a disabled row omits `Click`.
#[cfg(a11y_tree)]
#[test]
fn palette_a11y_lists_filtered_rows() {
    use aterm_a11y::model::{Action, Role};

    let mut s = PaletteState::new();
    for c in "tab".chars() {
        s.push_char(c);
    }
    let vis = s.filtered();
    assert!(vis.len() >= 2, "expected several tab commands");
    let update = palette_a11y(&s);

    let items = update
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == Role::MenuItem)
        .count();
    assert_eq!(items, vis.len(), "one MenuItem per filtered row");
    for (slot, &i) in vis.iter().enumerate() {
        let id = s.a11y_node_id(slot);
        let (_, node) = update.nodes.iter().find(|(nid, _)| *nid == id).unwrap();
        assert_eq!(node.label(), Some(s.rows[i].label.as_ref()));
    }
    assert_eq!(update.focus, s.a11y_node_id(s.selected));
    let header = s
        .controls_lines()
        .into_iter()
        .find(|l| l.starts_with("menu rows="))
        .unwrap();
    assert!(header.contains(&format!("shown={}", vis.len())));

    let mut s2 = PaletteState::new();
    for c in "copy".chars() {
        s2.push_char(c);
    }
    s2.resolve(&PaletteLive::default());
    let vis2 = s2.filtered();
    let copy_slot = vis2
        .iter()
        .position(|&i| s2.rows[i].action == MenuAction::Copy)
        .expect("Copy row present");
    let t2 = palette_a11y(&s2);
    let (_, copy_node) = t2
        .nodes
        .iter()
        .find(|(nid, _)| *nid == s2.a11y_node_id(copy_slot))
        .unwrap();
    assert!(
        !copy_node.supports_action(Action::Click),
        "disabled Copy has no Click"
    );
    assert!(
        copy_node.supports_action(Action::Focus),
        "disabled Copy is still focusable"
    );
}

#[cfg(a11y_tree)]
#[test]
fn native_rows_have_the_same_a11y_title_scope_shortcut_and_enabled_state() {
    use aterm_a11y::model::Action;

    let mut s = PaletteState::new().with_native_commands(NativeCommandScope {
        window: crate::WindowId(3),
        instance: crate::tab_model::AppInstanceId::from_stored(5),
        view: crate::tab_model::ViewId::from_stored(8),
        generation: 13,
        section: "Settings App".to_string(),
        commands: vec![
            Command {
                id: ActionId::new("settings/search"),
                title: "Settings: Search".to_string(),
                shortcut: Some("Cmd-F".to_string()),
                enabled: true,
            },
            Command {
                id: ActionId::new("settings/undo"),
                title: "Settings: Undo Last Change".to_string(),
                shortcut: Some("Cmd-Z".to_string()),
                enabled: false,
            },
        ],
    });
    for c in "settings".chars() {
        s.push_char(c);
    }
    let update = palette_a11y(&s);
    let (_, search) = update
        .nodes
        .iter()
        .find(|(id, _)| *id == s.a11y_node_id(0))
        .expect("first native command");
    assert_eq!(search.label(), Some("Settings: Search"));
    assert_eq!(
        search.description(),
        Some(if cfg!(target_os = "macos") {
            "Settings App \u{b7} Cmd-F"
        } else {
            "Settings App"
        })
    );
    assert!(search.supports_action(Action::Click));

    let undo_slot = s
        .filtered()
        .iter()
        .position(|&index| s.rows[index].label == "Settings: Undo Last Change")
        .expect("disabled native command remains discoverable");
    let (_, undo) = update
        .nodes
        .iter()
        .find(|(id, _)| *id == s.a11y_node_id(undo_slot))
        .expect("disabled native command node");
    assert!(!undo.supports_action(Action::Click));
    assert!(undo.supports_action(Action::Focus));
}

/// A screen-reader Click that arrives after a native tab switch fails closed instead of selecting the same slot in the replacement app.
#[cfg(a11y_tree)]
#[test]
fn stale_native_a11y_row_cannot_redirect_after_scope_replacement() {
    let scope = |view, generation, action: &'static str| NativeCommandScope {
        window: crate::WindowId(3),
        instance: crate::tab_model::AppInstanceId::from_stored(5),
        view: crate::tab_model::ViewId::from_stored(view),
        generation,
        section: "Native App".to_string(),
        commands: vec![Command {
            id: ActionId::new(action),
            title: "First command".to_string(),
            shortcut: None,
            enabled: true,
        }],
    };
    let mut state = PaletteState::new().with_native_commands(scope(8, 13, "first/run"));
    let stale = state.a11y_node_id(0);
    assert_eq!(state.a11y_filtered_index(stale), Some(0));

    state.replace_native_commands(Some(scope(9, 14, "second/run")));

    assert_ne!(state.a11y_node_id(0), stale);
    assert_eq!(state.a11y_filtered_index(stale), None);
}
