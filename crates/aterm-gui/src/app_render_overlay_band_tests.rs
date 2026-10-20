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

    // The scalar `selection`/`selection_clip` goes with the list it
    // duplicates: `selection_row_key` and the per-cell predicate fall back to
    // the scalar when `selections` is empty.
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

/// THE CHROME SURFACES PICK THEIR THEME (chrome-8): with `window_theme` forced
/// light over a dark terminal theme, the header and every card or band over the
/// grid (find panel, banners, tab menu, settings overlay) take the forced light
/// palette, so they match the message band; what paints into the pane's cells
/// (find's highlight) and the grid itself keep the terminal's. A program's OSC
/// 11 background moves Content and, under Auto, OverGrid, never the header or
/// the grid. NEGATIVE CONTROL: under Auto, OverGrid is exactly the overlay card
/// theme, and a forced palette is not the terminal's.
#[test]
fn chrome_palette_surfaces_pick_their_theme() {
    use crate::app_render::ChromeSurface;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.prepare_terminal_capture_grid(wid).unwrap();
    let terminal = app.theme;
    let theme_of = |app: &App, surface| app.chrome_palette(surface).theme;
    // Auto: every surface is the terminal's; OSC 11 moves Content and OverGrid.
    assert_eq!(
        theme_of(&app, ChromeSurface::OverGrid(wid)),
        app.overlay_card_theme(wid)
    );
    let recoloured = 0x0012_0034;
    app.windows.get_mut(&wid).unwrap().input_scratch.default_bg = recoloured;
    assert_eq!(theme_of(&app, ChromeSurface::Content(wid)).bg, recoloured);
    assert_eq!(theme_of(&app, ChromeSurface::OverGrid(wid)).bg, recoloured);
    assert_eq!(theme_of(&app, ChromeSurface::Header), terminal);
    assert_eq!(theme_of(&app, ChromeSurface::Grid), terminal);
    // Forced light over the dark terminal theme.
    app.window_theme = crate::app_config::WindowTheme::Light;
    let header = theme_of(&app, ChromeSurface::Header);
    assert_eq!(
        header.bg,
        crate::native_appearance::FORCED_LIGHT_CHROME_BG,
        "CONTROL: forced"
    );
    assert_eq!(theme_of(&app, ChromeSurface::OverGrid(wid)), header);
    assert_eq!(theme_of(&app, ChromeSurface::Content(wid)).bg, recoloured);
    assert_eq!(theme_of(&app, ChromeSurface::Grid), terminal);
    // The band and a banner over the grid wear one ground.
    let band = crate::chrome_band::band_colors(app.chrome_palette(ChromeSurface::Header));
    let banner = crate::paste_banner::banner_rows(
        "ls",
        80,
        2,
        app.chrome_palette(ChromeSurface::OverGrid(wid)),
    );
    assert_eq!(banner[0][0].bg, band.bar_bg);
}

/// Every destination pixel a list of 1:1 free sprites paints, and the atlas
/// texel it samples there (`flip_x` mirrors the column).
fn free_texels(
    sprites: &[aterm_core::render::FreeSprite],
) -> std::collections::BTreeMap<(i64, i64), (i64, i64)> {
    let mut out = std::collections::BTreeMap::new();
    for s in sprites {
        for dy in 0..i64::from(s.h) {
            for dx in 0..i64::from(s.w) {
                let ax = if s.flip_x {
                    i64::from(s.ax) + i64::from(s.aw) - 1 - dx
                } else {
                    i64::from(s.ax) + dx
                };
                let at = (i64::from(s.x) + dx, i64::from(s.y) + dy);
                assert!(
                    out.insert(at, (ax, i64::from(s.ay) + dy)).is_none(),
                    "no two parts paint one pixel: {at:?}"
                );
            }
        }
    }
    out
}

/// THE CARD IS ON TOP (surfaces-F3): an open menu card takes its own cells
/// back from the caret, the cell-tagged streams, the light and the sprites —
/// and only its own: what lies beside it on the same rows keeps painting, and
/// a sprite half under it keeps its other half, sampling the very texels it
/// sampled there (mirrored or not).
#[test]
fn a_menu_card_takes_its_cells_back_from_the_caret_and_every_overlay() {
    use aterm_core::render::{FreeSprite, GlowQuad, SpriteQuad, TrailCell};
    // Cells 10x20 px, pad 5; the card covers frame rows 2..5, cols 4..10:
    // grid-interior x 40..100, y 40..100.
    let card = crate::tab_menu::MenuRect {
        row: 2,
        col: 4,
        w: 6,
        h: 3,
    };
    let (cell, pad) = ((10, 20), 5);
    let fresh = |cursor: (usize, usize)| {
        let mut input = aterm_render::RenderInput::empty();
        input.rows = 8;
        input.cols = 16;
        input.cells = vec![Vec::new(); 8];
        (input.cursor_row, input.cursor_col) = cursor;
        input.cursor_visible = true;
        input
    };

    // The caret: hidden on the card, standing beside it on the same row.
    let mut under = fresh((3, 6));
    super::scrub_overlay_card(&mut under, card, cell, pad);
    assert!(!under.cursor_visible, "the caret under the card is hidden");
    let mut beside = fresh((3, 3));
    super::scrub_overlay_card(&mut beside, card, cell, pad);
    assert!(
        beside.cursor_visible,
        "CONTROL: the caret beside the card stands"
    );

    let mut input = fresh((0, 0));
    input.cursor_trail = vec![
        TrailCell {
            row: 3,
            col: 5,
            alpha: 9,
        },
        TrailCell {
            row: 3,
            col: 11,
            alpha: 9,
        },
    ];
    // Window-absolute light: `pad` lies to the grid's left. The quad at
    // window x 40 ends at grid x 39, off the card — it stays only where the
    // pad is honoured.
    let glow = |x: u16| GlowQuad {
        row: 3,
        x,
        y: 65,
        w: 4,
        h: 20,
        ..GlowQuad::default()
    };
    input.cursor_glow_add = vec![glow(55), glow(40), glow(105)];
    // A pet 40 px wide straddling the card's right edge, a mirrored twin
    // straddling its left edge, a sprite all on the card, one all off it,
    // and one scaled (not 1:1) touching it.
    let pet = FreeSprite {
        x: 80,
        y: 50,
        w: 40,
        h: 20,
        ax: 200,
        ay: 10,
        aw: 40,
        ah: 20,
        ..FreeSprite::default()
    };
    let flipped = FreeSprite {
        x: 20,
        y: 80,
        flip_x: true,
        ..pet
    };
    let inside = FreeSprite {
        x: 50,
        y: 50,
        w: 10,
        h: 10,
        aw: 10,
        ah: 10,
        ..pet
    };
    let away = FreeSprite { x: 130, ..pet };
    let scaled = FreeSprite { aw: 20, ..inside };
    input.free_sprites = vec![pet, flipped, inside, away, scaled];
    input.cat_quads = vec![SpriteQuad {
        row: 2,
        x: 30,
        y: 40,
        w: 20,
        h: 20,
        ax: 0,
        ay: 0,
        aw: 20,
        ah: 20,
        ..SpriteQuad::default()
    }];
    let before = free_texels(&input.free_sprites[..4]);

    super::scrub_overlay_card(&mut input, card, cell, pad);

    assert_eq!(
        input.cursor_trail.iter().map(|t| t.col).collect::<Vec<_>>(),
        [11],
        "the trail cell on the card goes, the one beside it stays"
    );
    assert_eq!(
        input
            .cursor_glow_add
            .iter()
            .map(|q| q.x)
            .collect::<Vec<_>>(),
        [40, 105],
        "the light on the card goes; the light off it, pad honoured, stays"
    );
    let on_card = |(x, y): (i64, i64)| (40..100).contains(&x) && (40..100).contains(&y);
    let expected: std::collections::BTreeMap<_, _> =
        before.into_iter().filter(|(at, _)| !on_card(*at)).collect();
    let after = free_texels(&input.free_sprites);
    assert_eq!(
        after, expected,
        "every pixel off the card paints the texel it painted, and none on it"
    );
    assert!(
        input.free_sprites.contains(&away),
        "a sprite off the card is untouched"
    );
    assert_eq!(
        input
            .cat_quads
            .iter()
            .map(|q| (q.x, q.w, q.ax, q.aw, q.row))
            .collect::<Vec<_>>(),
        [(30, 10, 0, 10, 2)],
        "a row-tagged sprite keeps its part off the card, in its own band"
    );
}

/// A completed simple selection from `from` to `to` (frame rows, columns).
fn simple_selection(from: (i32, u16), to: (i32, u16)) -> aterm_core::selection::TextSelection {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let mut selection = aterm_core::selection::TextSelection::new();
    selection.start_selection(from.0, from.1, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(to.0, to.1, SelectionSide::Right);
    selection.complete_selection();
    selection
}

/// Every frame cell `input`'s selection tints, by the renderer's predicate.
fn tinted(input: &aterm_render::RenderInput) -> std::collections::BTreeSet<(usize, usize)> {
    (0..input.rows)
        .flat_map(|r| (0..input.cols).map(move |c| (r, c)))
        .filter(|&(r, c)| input.selection_contains_cell(r, c, false, false))
        .collect()
}

/// THE CARD IS ON TOP OF A SELECTION, A METER AND A JITTERED SPARKLE
/// (review of surfaces-F3): the selection tint is the renderer's own mask,
/// one clip rectangle, so a selection the card covers part of keeps the side
/// of the card holding the most of it and tints none of the card; one the
/// card covers whole goes, and a split's other panes keep theirs untouched,
/// in order. A meter row (`ChromeRaster`) on the card's rows is retired, one
/// beside it stays. A sparkle word goes where its jittered cell meets the
/// card, wherever its tag is. NEGATIVE CONTROLS: a selection clear of the
/// card keeps its clip (`None`); a sparkle jittered away from the card stays.
#[test]
fn a_menu_card_untints_a_selection_and_retires_a_meter_and_a_sparkle_under_it() {
    use aterm_core::render::{ChromeRaster, PaneSelection, SelectionClip, WordDecoration};
    // Cells 10x20 px; the card covers frame rows 2..5, cols 4..10.
    let card = crate::tab_menu::MenuRect {
        row: 2,
        col: 4,
        w: 6,
        h: 3,
    };
    let on_card = |&(r, c): &(usize, usize)| (2..5).contains(&r) && (4..10).contains(&c);
    let fresh = |cols: usize| {
        let mut input = aterm_render::RenderInput::empty();
        input.rows = 8;
        input.cols = cols;
        input.cells = vec![Vec::new(); 8];
        input
    };

    // One terminal: rows 1..=3 selected across the frame. The card covers
    // 12 of its 48 cells; right of it keeps 18, more than above it (16).
    let mut input = fresh(16);
    input.selection = simple_selection((1, 0), (3, 15));
    let before = tinted(&input);
    assert_eq!(before.len(), 48);
    super::scrub_overlay_card(&mut input, card, (10, 20), 5);
    assert_eq!(input.selection_clip, Some(SelectionClip::new(0, 8, 10, 16)));
    let after = tinted(&input);
    assert!(after.iter().all(|cell| !on_card(cell)), "{after:?}");
    assert_eq!(after.len(), 18, "the side holding the most of it");
    assert!(after.is_subset(&before));
    // CONTROL: a selection clear of the card keeps its clip.
    let mut clear = fresh(16);
    clear.selection = simple_selection((6, 0), (7, 15));
    super::scrub_overlay_card(&mut clear, card, (10, 20), 5);
    assert_eq!(clear.selection_clip, None, "a selection clear of the card");
    assert_eq!(tinted(&clear).len(), 32);

    // A split of three panes, 8 columns each. The left pane's whole-pane
    // selection keeps the side left of the card; the middle one's, all under
    // the card, goes; the right one's, clear of it, stays as it was.
    let pane = |from, to, clip| PaneSelection {
        selection: simple_selection(from, to),
        clip,
        bg: aterm_core::render::COLOR_UNSET,
        fg: aterm_core::render::COLOR_UNSET,
        inactive: false,
    };
    let left = pane((0, 0), (7, 7), SelectionClip::new(0, 8, 0, 8));
    let middle = pane((3, 8), (3, 9), SelectionClip::new(0, 8, 8, 16));
    let right = pane((0, 16), (7, 23), SelectionClip::new(0, 8, 16, 24));
    let mut input = fresh(24);
    input.selections = vec![left.clone(), middle.clone(), right.clone()];
    input.selection = left.selection.clone();
    super::scrub_overlay_card(&mut input, card, (10, 20), 5);
    assert_eq!(
        input.selections.iter().map(|e| e.clip).collect::<Vec<_>>(),
        [SelectionClip::new(0, 8, 0, 4), right.clip],
        "the left pane keeps the side left of the card, the middle one goes, in order"
    );
    assert!(tinted(&input).iter().all(|cell| !on_card(cell)));
    // The list emptied: the scalar, the focused pane's duplicate, goes too.
    let mut input = fresh(24);
    input.selections = vec![middle.clone()];
    input.selection = middle.selection.clone();
    super::scrub_overlay_card(&mut input, card, (10, 20), 5);
    assert!(!input.any_selection(), "nothing the card covers is tinted");

    // A meter row on the card's rows is retired; one beside them stays.
    let meter = |row| ChromeRaster {
        row,
        ground: std::sync::Arc::from(vec![0x0011_2233; 240]),
        rail: std::sync::Arc::from(Vec::new()),
        rail_h: 0,
        clear_rail: false,
        own: Vec::new(),
        split: None,
        rings: Vec::new(),
        icons: Vec::new(),
    };
    let mut input = fresh(24);
    input.chrome_rasters = vec![meter(3), meter(6)];
    // Sparkle words, a cell each (10x20 px) moved by their jitter: tagged
    // above the card but jittered into it; tagged left of it, jittered into
    // it; on it; and jittered away from it (stays).
    let deco = |row, col, dx, dy| WordDecoration {
        row,
        col,
        dx,
        dy,
        glyph: aterm_core::render::DecoGlyph::default(),
        blend: aterm_core::render::DecoBlend::default(),
        color: 0x00ff_ffff,
        alpha: 255,
    };
    input.word_decorations = vec![
        deco(1, 5, 0, 5),
        deco(1, 5, 0, -2),
        deco(3, 3, 3, 0),
        deco(3, 3, -3, 0),
        deco(3, 5, 0, 0),
    ];
    super::scrub_overlay_card(&mut input, card, (10, 20), 5);
    assert_eq!(
        input
            .chrome_rasters
            .iter()
            .map(|m| m.row)
            .collect::<Vec<_>>(),
        [6],
        "the meter on the card's rows is retired, the one off them stays"
    );
    assert_eq!(
        input
            .word_decorations
            .iter()
            .map(|d| (d.row, d.col, d.dx, d.dy))
            .collect::<Vec<_>>(),
        [(1, 5, 0, -2), (3, 3, -3, 0)],
        "a sparkle goes where its jittered cell meets the card, and only there"
    );
}

/// Whether a card raster holds the terminal's orange ink: a pixel whose red
/// leads its blue by more than 60 at more than a quarter coverage.
fn has_orange_ink(rgba: &[u8]) -> bool {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .any(|px| px[3] > 0x40 && i32::from(px[0]) - i32::from(px[2]) > 60)
}

/// A colour picked for the terminal never inks a floating card on a native
/// page: over Settings the build badge takes the page's palette whole (its
/// ground and its ink), while over the terminal tab the same badge still
/// wears the terminal's colours.
#[test]
fn a_floating_chrome_card_on_a_native_page_ignores_terminal_color_overrides() {
    let overridden = |app: &mut App| {
        app.config.foreground = Some("#FFAA00".to_string());
        app.config.background = Some("#002244".to_string());
        app.config.show_build_badge = Some(true);
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    };
    let wid = WindowId(0);

    let mut page = App::headless_for_test();
    overridden(&mut page);
    assert!(page.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(page.prepare_native_input_scratch(wid));
    let theme = page.chrome_card_theme(wid);
    let palette = page.native_page_palette_theme();
    assert_eq!(
        (theme.fg, theme.bg, theme.cursor),
        (palette.fg, palette.bg, palette.cursor),
        "the card over a page is the page's palette"
    );
    assert_eq!(
        page.overlay_card_theme(wid),
        palette,
        "an overlay card over a page takes the same palette"
    );
    page.splice_build_badge(wid);
    let badge = page.windows[&wid]
        .badge_card
        .as_ref()
        .expect("the badge painted");
    assert!(
        !has_orange_ink(&badge.rgba),
        "the badge over Settings carries no terminal-orange ink"
    );

    // Negative control: the same overrides over the terminal tab ink the
    // badge orange, so the probe can tell the two apart.
    let mut terminal = App::headless_for_test();
    overridden(&mut terminal);
    terminal.prepare_terminal_capture_grid(wid).unwrap();
    assert_eq!(terminal.chrome_card_theme(wid).fg, 0x00FF_AA00);
    terminal.splice_build_badge(wid);
    let badge = terminal.windows[&wid]
        .badge_card
        .as_ref()
        .expect("the badge painted");
    assert!(
        has_orange_ink(&badge.rgba),
        "over the terminal the badge wears the terminal's foreground"
    );
}
