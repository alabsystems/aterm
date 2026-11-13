// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn labels_remove_control_sequences_without_splitting_graphemes() {
    assert_eq!(label(2, "🧑🏽‍💻\n", "a\tb\u{202e}c\u{2069}"), "2  🧑🏽‍💻  abc");
    let family = "👨‍👩‍👧‍👦";
    let icon = family.repeat(9);
    assert_eq!(
        label(1, &icon, "shell"),
        format!("1  {}  shell", family.repeat(8))
    );
    let title = format!("{}e\u{301}", "a".repeat(511));
    assert!(label(1, "", &title).ends_with("e\u{301}"));
    assert_eq!(label(1, "", &format!("e{}", "\u{301}".repeat(5000))), "1  ");
}

#[test]
fn fitted_labels_only_cut_at_grapheme_boundaries() {
    let text = "Cafe\u{301} 👩🏽‍💻 界 long title";
    let mut boundaries: Vec<_> = text.grapheme_indices().map(|(at, _)| at).collect();
    boundaries.push(text.len());
    let px = 13.0;
    for width in [0.0, 8.0, 16.0, 30.0, 45.0, 60.0, 80.0, 120.0, 500.0] {
        let fitted = fit_label(text, width, px, TextFace::Ui);
        assert!(tray_raster::ui_text_width_for(TextFace::Ui, &fitted, px) <= width);
        let prefix = fitted.strip_suffix('…').unwrap_or(&fitted);
        assert!(text.starts_with(prefix));
        assert!(
            boundaries.contains(&prefix.len()),
            "split grapheme: {fitted:?}"
        );
    }
}

/// [`semantic_row`] under the Classic split chrome, whose markers are the
/// `▸`/`└` branch glyphs.
fn classic_row(
    label: &str,
    cols: usize,
    focused: bool,
    theme: aterm_render::Theme,
    indicators: TabIndicators,
) -> SemanticRow {
    crate::pane_chrome::with_split_chrome(crate::pane_chrome::SplitChrome::Classic, || {
        semantic_row(label, cols, focused, theme, indicators)
    })
}

/// The Fluent header (Windows) has no top seam of its own (the strip's rule
/// or a divider's hairline is above it) and marks the focused pane with a
/// dot, the other with nothing.
#[test]
fn a_fluent_header_has_no_top_seam_and_dots_the_focused_pane() {
    let theme = aterm_render::Theme::default();
    let indicators = TabIndicators::default();
    let fluent = |focused| {
        crate::pane_chrome::with_split_chrome(crate::pane_chrome::SplitChrome::Fluent, || {
            semantic_row("1  shell", 12, focused, theme, indicators)
        })
    };
    let (focused, other) = (fluent(true), fluent(false));
    assert!(
        focused
            .cells
            .iter()
            .chain(&other.cells)
            .all(|c| !c.overline)
    );
    assert_eq!(focused.cells[0].ch, '•');
    assert_eq!(other.cells[0].ch, ' ');
    let classic = classic_row("1  shell", 12, true, theme, indicators);
    assert!(classic.cells.iter().all(|c| c.overline), "CONTROL");
    assert_eq!(classic.cells[0].ch, '▸', "CONTROL");
}

/// THE FLUENT HEADER WEARS ITS PANE'S GROUND ([`header_ground`]): its cells and
/// its raster are the terminal's background, so the strip's selected tab opens
/// into it, and on every bundled scheme its words keep AA and its dot and rule
/// 3:1 there.
#[test]
fn a_fluent_header_wears_its_panes_ground() {
    use crate::pane_chrome::{SplitChrome, with_split_chrome};
    for name in aterm_types::scheme::builtin_names() {
        let theme =
            aterm_render::Theme::from(&aterm_types::scheme::builtin(name).expect("builtin"));
        let ground = crate::settings::u32_rgb(theme.bg);
        let band = chrome_band::band_colors(theme);
        let fluent = with_split_chrome(SplitChrome::Fluent, || header_colors(theme));
        assert_eq!(fluent.bar_bg, ground, "{name}: the Fluent header's fill");
        let row = with_split_chrome(SplitChrome::Fluent, || {
            semantic_row("1  shell", 12, true, theme, TabIndicators::default())
        });
        assert!(
            row.cells.iter().all(|c| c.bg == ground),
            "{name}: its cells"
        );
        for (what, ink, floor) in [
            ("value", fluent.value, 4.5),
            ("label", fluent.label, 4.5),
            ("warn", fluent.warn, 4.5),
            ("accent", fluent.accent, 3.0),
        ] {
            let ratio = chrome_band::contrast(ink, ground);
            assert!(
                ratio >= floor - 0.01,
                "{name}: the {what} ink {ink:?} is {ratio:.2}:1 on the ground"
            );
        }
        let classic = with_split_chrome(SplitChrome::Classic, || header_colors(theme));
        assert_eq!(classic.bar_bg, band.bar_bg, "{name}: CONTROL");
    }
    if !tray_raster::strip_band_ui_ready() {
        tray_raster::prepare_ui_fonts_for_direct_view_test();
    }
    if tray_raster::strip_band_ui_ready() {
        let theme = aterm_render::Theme::default();
        let colors = with_split_chrome(SplitChrome::Fluent, || header_colors(theme));
        let image = with_split_chrome(SplitChrome::Fluent, || {
            raster(
                "1  shell",
                20,
                (9, 23),
                1.0,
                false,
                TabIndicators::default(),
                colors,
                None,
            )
        })
        .expect("the header rasters");
        let width = 20 * 9;
        let at = (2 * width + width - 2) * 4;
        assert_eq!(
            &image.bytes[at..at + 3],
            crate::settings::u32_rgb(theme.bg).as_slice(),
            "the raster's fill is the ground"
        );
        tray_raster::clear_ui_fonts_for_test();
    }
}

/// AN UNFOCUSED FLUENT HEADER IS CLOSED AGAINST ITS GRID: on every bundled
/// scheme its foot, the hairline under it, holds [`HEADER_FOOT_FLOOR`]
/// against the ground the header and the grid share, in the raster too; a
/// foot that already cleared it (Default's 2.04:1) is the same quiet line
/// as before, and the Classic header keeps its line.
#[test]
fn an_unfocused_fluent_header_keeps_its_foot_off_the_grid() {
    use crate::pane_chrome::{SplitChrome, with_split_chrome};
    let opaque = |c: crate::widget::Rgba| [c[0], c[1], c[2]];
    for name in aterm_types::scheme::builtin_names() {
        let theme =
            aterm_render::Theme::from(&aterm_types::scheme::builtin(name).expect("builtin"));
        let colors = with_split_chrome(SplitChrome::Fluent, || header_colors(theme));
        let foot = header_foot(&colors, true);
        let quiet = rgba(colors.label, HEADER_FOOT_ALPHA);
        if foot != quiet {
            assert_eq!(foot[3], 255, "{name}: a floored foot is opaque");
            let ratio = chrome_band::contrast(opaque(foot), colors.bar_bg);
            assert!(
                (HEADER_FOOT_FLOOR - 0.01..HEADER_FOOT_FLOOR + 0.1).contains(&ratio),
                "{name}: the foot is {ratio:.2}:1 on the ground, not just over the floor"
            );
        }
        assert_eq!(header_foot(&colors, false), quiet, "{name}: Classic");
    }
    let dark = with_split_chrome(SplitChrome::Fluent, || {
        header_colors(aterm_render::Theme::default())
    });
    assert_eq!(
        header_foot(&dark, true),
        rgba(dark.label, HEADER_FOOT_ALPHA),
        "Default's foot clears the floor and is drawn as before"
    );
    let light_theme =
        aterm_render::Theme::from(&aterm_types::scheme::builtin("Default Light").expect("builtin"));
    let light = with_split_chrome(SplitChrome::Fluent, || header_colors(light_theme));
    assert_ne!(
        header_foot(&light, true),
        rgba(light.label, HEADER_FOOT_ALPHA),
        "CONTROL: Default Light's quiet foot is under the floor"
    );
    if !tray_raster::strip_band_ui_ready() {
        tray_raster::prepare_ui_fonts_for_direct_view_test();
    }
    if tray_raster::strip_band_ui_ready() {
        let (cols, cw, ch) = (20usize, 9usize, 23usize);
        let image = with_split_chrome(SplitChrome::Fluent, || {
            raster(
                "2  shell",
                cols,
                (cw, ch),
                1.0,
                false,
                TabIndicators::default(),
                light,
                None,
            )
        })
        .expect("the header rasters");
        let width = cols * cw;
        let at = ((ch - 1) * width + width - 2) * 4;
        let foot: [u8; 3] = [image.bytes[at], image.bytes[at + 1], image.bytes[at + 2]];
        let ratio = chrome_band::contrast(foot, light.bar_bg);
        assert!(
            ratio >= HEADER_FOOT_FLOOR - 0.02,
            "the raster's foot {foot:?} is {ratio:.2}:1 on {:?}",
            light.bar_bg
        );
        tray_raster::clear_ui_fonts_for_test();
    }
}

/// THE FOCUSED FLUENT HEADER'S RULE LIES ON WHOLE PIXEL ROWS from the
/// pane's first pixel column to its last ([`focus_rule_rows`]: two rows at
/// 100%, three at 150%, four at 200%, at the row's foot), the rows the focus
/// ring's side line turns into (`crate::pane_chrome::focus_ring_rects`); the
/// row above it is not the accent.
#[test]
fn the_focused_fluent_rule_lies_on_whole_rows_from_the_panes_edge() {
    use crate::pane_chrome::{SplitChrome, with_split_chrome};
    assert_eq!(focus_rule_rows(23), (21, 2));
    assert_eq!(focus_rule_rows(35), (32, 3));
    assert_eq!(focus_rule_rows(46), (42, 4));
    let old_top = 23.0f32 - 2.0 * (23.0 / 24.0);
    assert!(
        old_top.fract() > 0.0,
        "CONTROL: the old rule's top row was partial"
    );
    if !tray_raster::strip_band_ui_ready() {
        tray_raster::prepare_ui_fonts_for_direct_view_test();
    }
    if !tray_raster::strip_band_ui_ready() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let colors = with_split_chrome(SplitChrome::Fluent, || {
        header_colors(aterm_render::Theme::default())
    });
    for (cw, ch, scale) in [(9usize, 23usize, 1.0f32), (18, 46, 2.0)] {
        let cols = 20;
        let image = with_split_chrome(SplitChrome::Fluent, || {
            raster(
                "2  shell",
                cols,
                (cw, ch),
                scale,
                true,
                TabIndicators::default(),
                colors,
                None,
            )
        })
        .expect("the header rasters");
        let width = cols * cw;
        let px = |x: usize, y: usize| {
            let at = (y * width + x) * 4;
            [image.bytes[at], image.bytes[at + 1], image.bytes[at + 2]]
        };
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 1);
        let (top, thick) = focus_rule_rows(ch as u32);
        let (top, thick) = (top as usize, thick as usize);
        for x in [0, 1, width / 2, width - 1] {
            for y in top..top + thick {
                assert!(
                    near(px(x, y), colors.accent),
                    "{scale}x: ({x}, {y}) is {:?}, not the accent {:?}",
                    px(x, y),
                    colors.accent
                );
            }
            assert!(
                !near(px(x, top - 1), colors.accent),
                "{scale}x: the row above the rule at x {x}"
            );
        }
    }
    tray_raster::clear_ui_fonts_for_test();
}

/// THE HEADER READS BELOW THE TAB: under the Fluent chrome the pane
/// header's label is the Secondary step of the strip's own chrome base, at
/// every scale smaller than the tab title above it, in the same face.
#[test]
fn the_fluent_header_label_sits_a_step_below_the_tab_title() {
    for (row_h, scale) in [(23.0f32, 1.0f32), (29.0, 1.25), (35.0, 1.5), (46.0, 2.0)] {
        let tab = crate::tab_bar::chrome_label_base_px(row_h + 9.0 * scale, scale);
        let header = header_label_px(row_h, scale, true).get();
        assert!(
            header < tab && header >= tab * 0.85,
            "{scale}x: header {header} px against the tab's {tab} px"
        );
    }
    assert!((header_label_px(23.0, 1.0, true).get() - 11.7).abs() < 0.01);
    let classic = header_label_px(23.0, 1.0, false).get();
    assert!(
        classic > crate::tab_bar::chrome_label_base_px(32.0, 1.0),
        "CONTROL: the cell-sized label outweighs the tab: {classic}"
    );
}

#[test]
fn semantic_rows_preserve_clusters_and_mark_only_wide_continuations() {
    let row = classic_row(
        "e\u{301}👩‍💻界Z",
        7,
        true,
        aterm_render::Theme::default(),
        TabIndicators::default(),
    );
    assert_eq!(
        row.cells.iter().map(|c| c.ch).collect::<String>(),
        "▸e👩 界 Z"
    );
    assert_eq!(row.combining, vec![(1, vec!['\u{301}'].into_boxed_slice())]);
    assert_eq!(row.clusters, vec![(2, Box::<str>::from("👩‍💻"))]);
    for (col, cell) in row.cells.iter().enumerate() {
        assert_eq!(cell.wide, matches!(col, 3 | 5));
    }
    let clipped = classic_row(
        "e\u{301}👩‍💻",
        3,
        false,
        aterm_render::Theme::default(),
        TabIndicators::default(),
    );
    assert_eq!(
        clipped.cells.iter().map(|c| c.ch).collect::<String>(),
        "└e…"
    );
    assert!(
        clipped.clusters.is_empty(),
        "a wide cluster never straddles the pane edge"
    );
}

/// A title paints in exactly the cells it was measured in: Devanagari half forms the
/// grid folds after a ZWJ stay in the row, and Sinhala's second cluster joins the first's cell.
#[test]
fn header_rows_paint_titles_in_the_cells_they_measure() {
    let theme = aterm_render::Theme::default();
    let indicators = TabIndicators::default();
    for title in [
        "\u{915}\u{94d}\u{200d}\u{937}",
        "\u{d9a}\u{dca}\u{200d}\u{dba}",
        "\u{1f44d}\u{200d}\u{200d}\u{1f44d}",
        "\u{a9}\u{fe0f}",
        "\u{928}\u{94d}\u{926}\u{940}",
    ] {
        for n in [1, 3, 8, 40] {
            let text = label(1, "", &title.repeat(n));
            for cols in 2..60 {
                let row = semantic_row(&text, cols, cols % 2 == 0, theme, indicators);
                assert_eq!(row.cells.len(), cols);
                assert!(
                    row.clusters.iter().all(|(at, _)| *at < cols)
                        && row.combining.iter().all(|(at, _)| *at < cols),
                    "{title:?} x{n} in {cols} columns"
                );
            }
        }
    }
    let row = classic_row("\u{d9a}\u{dca}\u{200d}\u{dba}Z", 4, true, theme, indicators);
    assert_eq!(
        row.cells.iter().map(|c| c.ch).collect::<String>(),
        "\u{25b8}\u{d9a}Z "
    );
    assert_eq!(
        row.combining,
        vec![(1, vec!['\u{dca}', '\u{200d}', '\u{dba}'].into_boxed_slice())]
    );
}

#[test]
fn header_cache_reuses_pixels_and_refreshes_title_focus_and_native_icon() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.split_active_stub_tab(wid);
    let mut plan = app.active_visible_leaf_plan(wid).unwrap();
    let first = app.refresh_pane_headers(wid, &plan);
    let pixels = app.windows[&wid].pane_headers.entries[0].image.clone();
    let cells = app.windows[&wid].pane_headers.entries[0]
        .semantic
        .cells
        .as_ptr();
    assert_eq!(app.refresh_pane_headers(wid, &plan), first);
    assert_eq!(
        app.windows[&wid].pane_headers.entries[0]
            .semantic
            .cells
            .as_ptr(),
        cells
    );
    assert_eq!(
        pixels.as_ref().map(Arc::as_ptr),
        app.windows[&wid].pane_headers.entries[0]
            .image
            .as_ref()
            .map(Arc::as_ptr)
    );
    let session = app
        .view_store
        .get(plan.leaves[0].view)
        .copied()
        .unwrap()
        .terminal_session()
        .unwrap();
    crate::term_lock(&app.pool.get(session).unwrap().term).process(b"\x1b]2;renamed pane\x07");
    let renamed = app.refresh_pane_headers(wid, &plan);
    assert_ne!(renamed, first);
    assert_eq!(
        app.windows[&wid].pane_headers.entries[0].title,
        "renamed pane"
    );
    plan.focused = plan.leaves[0].view;
    for leaf in &mut plan.leaves {
        leaf.focused = leaf.view == plan.focused;
    }
    let focused = app.refresh_pane_headers(wid, &plan);
    assert_ne!(focused, renamed);
    assert_eq!(
        app.pool
            .get(session)
            .unwrap()
            .ctx
            .meta
            .lock()
            .unwrap()
            .set("attention", Some("Needs review".into())),
        Some(true)
    );
    app.refresh_presence_session(session);
    assert_ne!(app.refresh_pane_headers(wid, &plan), focused);
    assert!(
        app.windows[&wid].pane_headers.entries[0]
            .semantic
            .cells
            .iter()
            .any(|cell| cell.ch == '!')
    );

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    app.refresh_pane_headers(wid, &plan);
    assert_eq!(
        app.windows[&wid].pane_headers.entries[0].native_icon,
        Some(TabIconKind::Settings)
    );
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .zoomed = true;
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    assert_eq!(app.refresh_pane_headers(wid, &plan), 0);
    assert!(app.windows[&wid].pane_headers.entries.is_empty());
}
