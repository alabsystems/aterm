// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId};

/// An overlay band narrows each pane's selection independently, so the
/// fail-safe drop (a band strictly inside one pane's box) costs only that
/// pane's highlight.
#[test]
fn overlay_band_narrows_each_pane_selection_independently() {
    use aterm_core::render::{PaneSelection, SelectionClip};
    use aterm_core::selection::{SelectionSide, SelectionType};

    let pane_selection = |from: (i32, u16), to: (i32, u16), clip: SelectionClip| {
        let mut selection = aterm_core::selection::TextSelection::new();
        selection.start_selection(from.0, from.1, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(to.0, to.1, SelectionSide::Right);
        selection.complete_selection();
        PaneSelection {
            selection,
            clip,
            bg: aterm_core::render::COLOR_UNSET,
            fg: aterm_core::render::COLOR_UNSET,
            inactive: false,
        }
    };

    let mut input = aterm_render::RenderInput::empty();
    input.rows = 8;
    input.cols = 8;
    input.cells = vec![Vec::new(); 8];
    // Left pane's selection is rows 0..2, clear of the band; the right pane's
    // spans the pane, so the band lands strictly inside it.
    input.selections = vec![
        pane_selection((0, 0), (1, 3), SelectionClip::new(0, 8, 0, 4)),
        pane_selection((0, 4), (7, 7), SelectionClip::new(0, 8, 4, 8)),
    ];

    super::scrub_overlay_row_band(&mut input, 3..5, 16);

    assert_eq!(
        input.selections.len(),
        1,
        "only the pane the band actually crosses loses its highlight"
    );
    assert_eq!(
        input.selections[0].clip,
        SelectionClip::new(0, 8, 0, 4),
        "the untouched pane keeps its box exactly"
    );
    assert!(
        input.selection_contains_cell(1, 1, false, false),
        "the untouched pane keeps painting"
    );
    assert!(
        !input.selection_contains_cell(6, 5, false, false),
        "the crossed pane fails safe below the band"
    );

    // The scalar `selection`/`selection_clip` must not outlive the list it
    // duplicates: a composed frame stamps both for the focused pane, and
    // `selection_row_key` and the per-cell predicate fall back to the scalar
    // when `selections` is empty. Leaving it after the band drops the last
    // entry would resurrect the unnarrowed highlight.
    let mut composed = aterm_render::RenderInput::empty();
    composed.rows = 8;
    composed.cols = 8;
    composed.cells = vec![Vec::new(); 8];
    let only = pane_selection((0, 0), (7, 7), SelectionClip::new(0, 8, 0, 8));
    composed.selection = only.selection.clone();
    composed.selection_clip = Some(only.clip);
    composed.selections = vec![only];
    assert!(
        composed.selection_contains_cell(6, 5, false, false),
        "precondition: the pane paints before the scrub"
    );

    // A band strictly interior to the only pane's box: not expressible as one
    // rectangle, so the entry fails safe and drops, emptying the list.
    super::scrub_overlay_row_band(&mut composed, 3..5, 16);

    assert!(
        composed.selections.is_empty(),
        "the only entry drops — an interior band is not narrowable"
    );
    assert!(
        !composed.selection.has_selection(),
        "…and the scalar it duplicated must go with it, or the fallback path \
         resurrects the un-narrowed highlight"
    );
    for row in 0..8usize {
        for col in 0..8usize {
            assert!(
                !composed.selection_contains_cell(row, col, false, false),
                "nothing may paint as selected after the list empties ({row},{col})"
            );
        }
    }

    // An edge band is narrowable, so nothing is dropped at all.
    let mut edge = aterm_render::RenderInput::empty();
    edge.rows = 8;
    edge.cols = 8;
    edge.cells = vec![Vec::new(); 8];
    edge.selections = vec![pane_selection(
        (0, 4),
        (7, 7),
        SelectionClip::new(0, 8, 4, 8),
    )];
    super::scrub_overlay_row_band(&mut edge, 0..2, 16);
    assert_eq!(
        edge.selections.len(),
        1,
        "an edge band narrows rather than erases"
    );
    assert_eq!(edge.selections[0].clip, SelectionClip::new(2, 8, 4, 8));
    assert!(!edge.selection_contains_cell(1, 5, false, false));
    assert!(edge.selection_contains_cell(3, 5, false, false));
}

/// Floating chrome cards (badge, notice, progress) follow the live OSC-11
/// background only while chrome is the terminal; when `window_theme` forces
/// chrome against the terminal palette they take the page's palette.
#[test]
fn a_floating_chrome_card_takes_the_page_palette_over_the_terminal_background() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.prepare_terminal_capture_grid(wid).unwrap();
    let recoloured = 0x0012_0034;
    app.windows.get_mut(&wid).unwrap().input_scratch.default_bg = recoloured;
    assert_eq!(
        app.chrome_card_theme(wid).bg,
        recoloured,
        "chrome that IS the terminal still follows the program's background"
    );
    #[cfg(target_os = "linux")]
    {
        app.window_theme = crate::app_config::WindowTheme::Light;
        let forced = app.chrome_card_theme(wid);
        assert_eq!(
            (forced.bg, forced.fg),
            (
                crate::native_appearance::FORCED_LIGHT_CHROME_BG,
                crate::native_appearance::FORCED_LIGHT_CHROME_FG
            ),
            "a forced-light page's cards are painted on the forced-light palette"
        );
        assert_eq!(
            forced.selection, app.theme.selection,
            "the accent identity is still the terminal theme's"
        );
    }
}

/// Robi's bubble over a forced-light native route rasters light, and dark
/// over the dark terminal theme; the staleness key carries the palette.
#[cfg(target_os = "linux")]
#[test]
fn robis_bubble_over_a_forced_light_page_is_a_light_bubble() {
    fn pill_luma(app: &mut App, wid: WindowId) -> f32 {
        app.splice_robi_bubble(wid);
        let card = app.windows[&wid]
            .bubble_card
            .as_ref()
            .expect("the bubble splice painted a card");
        // Sample the bubble's body at the card centre (shadow, corners and
        // badge disc are at the edges).
        let (x, y) = (card.pw / 2, card.ph / 2);
        let i = ((y * card.pw + x) * 4) as usize;
        let px = &card.rgba[i..i + 4];
        0.2126f32.mul_add(
            f32::from(px[0]),
            0.7152f32.mul_add(f32::from(px[1]), 0.0722 * f32::from(px[2])),
        )
    }

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(app.prepare_native_input_scratch(wid));
    // A tip of SPACES: the bubble's centre then lands on its body, past the
    // badge disc, with no ink over it.
    app.robi_bubble = Some(crate::robi_bubble::RobiBubble::new(
        "                ",
        None,
        std::time::Instant::now() - std::time::Duration::from_millis(400),
    ));

    assert!(
        crate::tab_bar::theme_is_dark(app.theme.bg),
        "precondition: the default terminal theme is dark"
    );
    app.window_theme = crate::app_config::WindowTheme::Auto;
    let terminal_toned = pill_luma(&mut app, wid);
    app.window_theme = crate::app_config::WindowTheme::Light;
    let forced_light = pill_luma(&mut app, wid);

    assert!(
        terminal_toned < 96.0,
        "chrome that extends the terminal keeps a dark bubble: {terminal_toned:.1}"
    );
    assert!(
        forced_light > 160.0,
        "a forced-light page carries a light bubble: {forced_light:.1}"
    );
}
