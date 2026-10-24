// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId};

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
    input.selections = vec![
        pane_selection((0, 0), (1, 3), SelectionClip::new(0, 8, 0, 4)),
        pane_selection((0, 4), (7, 7), SelectionClip::new(0, 8, 4, 8)),
    ];

    super::scrub_overlay_row_band(&mut input, 3..5, aterm_render::RowBands::new(0, 16));

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

    super::scrub_overlay_row_band(&mut composed, 3..5, aterm_render::RowBands::new(0, 16));

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

    let mut edge = aterm_render::RenderInput::empty();
    edge.rows = 8;
    edge.cols = 8;
    edge.cells = vec![Vec::new(); 8];
    edge.selections = vec![pane_selection(
        (0, 4),
        (7, 7),
        SelectionClip::new(0, 8, 4, 8),
    )];
    super::scrub_overlay_row_band(&mut edge, 0..2, aterm_render::RowBands::new(0, 16));
    assert_eq!(
        edge.selections.len(),
        1,
        "an edge band narrows rather than erases"
    );
    assert_eq!(edge.selections[0].clip, SelectionClip::new(2, 8, 4, 8));
    assert!(!edge.selection_contains_cell(1, 5, false, false));
    assert!(edge.selection_contains_cell(3, 5, false, false));
}

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

#[test]
fn chrome_palette_surfaces_pick_their_theme() {
    use crate::app_render::ChromeSurface;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.prepare_terminal_capture_grid(wid).unwrap();
    let terminal = app.theme;
    let theme_of = |app: &App, surface| app.chrome_palette(surface).theme;
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
    let band = crate::chrome_band::band_colors(app.chrome_palette(ChromeSurface::Header));
    let banner = crate::paste_banner::banner_rows(
        "ls",
        80,
        2,
        app.chrome_palette(ChromeSurface::OverGrid(wid)),
    );
    assert_eq!(banner[0][0].bg, band.bar_bg);
}

/// Every frame pixel `input`'s fx layers may paint: their slices against a
/// `w × h` frame whose grid starts at `(pad, 0)` in `rows` bands of
/// `cell_h`, past every occluder (`aterm_render::slice_layers`, what both
/// renderers draw from).
fn fx_pixels(
    input: &aterm_render::RenderInput,
    (w, h): (usize, usize),
    pad: usize,
    (cell_h, rows): (usize, usize),
) -> std::collections::BTreeSet<(usize, usize)> {
    struct Px<'o>(&'o mut std::collections::BTreeSet<(usize, usize)>);
    impl<'a> aterm_render::FxSliceSink<'a> for Px<'_> {
        fn item(
            &mut self,
            _: &'a aterm_core::render::FxLayer,
            _: &'a aterm_core::render::FxItem,
            _: (i64, i64),
        ) -> bool {
            true
        }

        fn slice(&mut self, s: aterm_render::FxSlice, _: usize) {
            for y in s.y0..s.y1 {
                for x in s.x0..s.x1 {
                    self.0.insert((x, y));
                }
            }
        }
    }
    let frame = aterm_render::FxFrame {
        w,
        h,
        pad,
        grid_top: 0,
        cell_h,
        rows,
    };
    let mut out = std::collections::BTreeSet::new();
    aterm_render::slice_layers(
        &frame,
        &input.fx_layers,
        &input.fx_occluders,
        |_| true,
        |_| true,
        &mut aterm_render::FxSlicer::default(),
        &mut Px(&mut out),
    );
    out
}

#[test]
fn a_menu_card_takes_its_cells_back_from_the_caret_and_every_overlay() {
    use aterm_core::render::{GlowQuad, TrailCell};
    use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers, published};
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

    let mut under = fresh((3, 6));
    super::scrub_overlay_card(&mut under, card, cell, (pad, 0));
    assert!(!under.cursor_visible, "the caret under the card is hidden");
    let mut beside = fresh((3, 3));
    super::scrub_overlay_card(&mut beside, card, cell, (pad, 0));
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
    let glow = |x: u16| GlowQuad {
        row: 3,
        x,
        y: 65,
        w: 4,
        h: 20,
        ..GlowQuad::default()
    };
    input.cursor_glow_add = vec![glow(55), glow(40), glow(105)];
    let art = |w: u16, h: u16| {
        let data = vec![255; usize::from(w) * usize::from(h) * 4];
        aterm_render::fx::bitmap(aterm_core::render::FxTexels::Rgba8Straight, w, h, data)
            .expect("an art")
    };
    let pet = FreeSprite {
        z: FreeZ::UnderText,
        ..FreeSprite::whole(80, 50, art(40, 20))
    };
    let flipped = FreeSprite {
        x: 20,
        y: 80,
        flip_x: true,
        ..pet.clone()
    };
    let inside = FreeSprite::whole(50, 50, art(10, 10));
    let away = FreeSprite {
        x: 130,
        ..pet.clone()
    };
    let scaled = FreeSprite {
        w: 10,
        ..FreeSprite::whole(50, 50, art(20, 10))
    };
    let cat = FreeSprite::whole(30, 40, art(20, 20));
    let sprites = [pet, flipped, inside, away, scaled, cat];
    SpriteLayers::new(1).publish(&sprites, &mut input.fx_layers);
    let frame = ((16 * 10 + 2 * 5, 8 * 20), 5, (20, 8));
    let before = fx_pixels(&input, frame.0, frame.1, frame.2);

    super::scrub_overlay_card(&mut input, card, cell, (pad, 0));

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
    assert_eq!(
        published(&input.fx_layers),
        sprites,
        "every companion layer stands whole: the card occludes, it cuts nothing"
    );
    let on_card = |&(x, y): &(usize, usize)| (45..105).contains(&x) && (40..100).contains(&y);
    let expected: std::collections::BTreeSet<_> =
        before.iter().copied().filter(|at| !on_card(at)).collect();
    assert!(
        before.iter().any(on_card) && expected.len() < before.len(),
        "the sprites reach under the card (non-vacuous)"
    );
    assert_eq!(
        fx_pixels(&input, frame.0, frame.1, frame.2),
        expected,
        "every pixel off the card stays drawable, and none on it"
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

#[test]
fn a_menu_card_untints_a_selection_and_retires_a_meter_and_a_sparkle_under_it() {
    use aterm_core::render::{ChromeRaster, PaneSelection, SelectionClip, WordDecoration};
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

    let mut input = fresh(16);
    input.selection = simple_selection((1, 0), (3, 15));
    let before = tinted(&input);
    assert_eq!(before.len(), 48);
    super::scrub_overlay_card(&mut input, card, (10, 20), (5, 0));
    assert_eq!(input.selection_clip, Some(SelectionClip::new(0, 8, 10, 16)));
    let after = tinted(&input);
    assert!(after.iter().all(|cell| !on_card(cell)), "{after:?}");
    assert_eq!(after.len(), 18, "the side holding the most of it");
    assert!(after.is_subset(&before));
    let mut clear = fresh(16);
    clear.selection = simple_selection((6, 0), (7, 15));
    super::scrub_overlay_card(&mut clear, card, (10, 20), (5, 0));
    assert_eq!(clear.selection_clip, None, "a selection clear of the card");
    assert_eq!(tinted(&clear).len(), 32);

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
    super::scrub_overlay_card(&mut input, card, (10, 20), (5, 0));
    assert_eq!(
        input.selections.iter().map(|e| e.clip).collect::<Vec<_>>(),
        [SelectionClip::new(0, 8, 0, 4), right.clip],
        "the left pane keeps the side left of the card, the middle one goes, in order"
    );
    assert!(tinted(&input).iter().all(|cell| !on_card(cell)));
    let mut input = fresh(24);
    input.selections = vec![middle.clone()];
    input.selection = middle.selection.clone();
    super::scrub_overlay_card(&mut input, card, (10, 20), (5, 0));
    assert!(!input.any_selection(), "nothing the card covers is tinted");

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
    super::scrub_overlay_card(&mut input, card, (10, 20), (5, 0));
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

#[test]
fn a_menu_card_cuts_a_chrome_rows_icons_at_its_edge() {
    use aterm_core::render::{BandIcon, ChromeIcon, ChromeRaster, ChromeRing, InkSplit};
    let card = crate::tab_menu::MenuRect {
        row: 2,
        col: 4,
        w: 6,
        h: 3,
    };
    let icon = |col| ChromeIcon {
        col,
        icon: BandIcon::Disc,
    };
    let badge = |col| ChromeIcon {
        col,
        icon: BandIcon::Info,
    };
    let raster = |row, icons: Vec<ChromeIcon>| ChromeRaster {
        row,
        ground: std::sync::Arc::from(vec![0x0011_2233; 160]),
        rail: std::sync::Arc::from(vec![0x0044_5566; 160]),
        rail_h: 2,
        clear_rail: true,
        own: vec![(0, 2)],
        split: Some(InkSplit {
            col: 1,
            x: 3,
            ink: 0x00ff_ffff,
            bg: 0,
        }),
        rings: vec![ChromeRing {
            start: 12,
            end: 15,
            ring: 0x00ff_ffff,
            inner: 0,
            seam: None,
        }],
        icons,
    };
    let mut input = aterm_render::RenderInput::empty();
    input.rows = 8;
    input.cols = 16;
    let blank = aterm_core::terminal::RenderCell::default();
    input.cells = vec![vec![blank; 16]; 8];
    for row in &mut input.cells[2..5] {
        row[4].ch = '\u{2502}';
    }
    input.chrome_rasters = vec![
        raster(2, vec![icon(10)]),
        raster(3, vec![icon(3), icon(5), badge(10), icon(14)]),
        raster(4, Vec::new()),
        raster(6, vec![icon(5)]),
    ];
    let off_rows = input.chrome_rasters[3].clone();
    super::scrub_overlay_card(&mut input, card, (10, 20), (5, 0));
    let kept = input
        .chrome_rasters
        .iter()
        .find(|m| m.row == 3)
        .expect("the row keeps its icons beside the card");
    assert_eq!(
        kept.icons,
        [icon(3), icon(14)],
        "the icons beside the card stay; the one on it and the badge that would spill into it go"
    );
    assert_eq!(
        input
            .chrome_rasters
            .iter()
            .find(|m| m.row == 2)
            .map(|m| m.icons.clone()),
        Some(vec![icon(10)]),
        "a glass mark beside the card's blank cell stays: it never spills"
    );
    assert!(
        kept.ground.is_empty()
            && kept.rail.is_empty()
            && kept.rail_h == 0
            && !kept.clear_rail
            && kept.own.is_empty()
            && kept.split.is_none()
            && kept.rings.is_empty(),
        "what spans the row goes: {kept:?}"
    );
    assert_eq!(
        input
            .chrome_rasters
            .iter()
            .map(|m| m.row)
            .collect::<Vec<_>>(),
        [2, 3, 6],
        "CONTROL: a raster on the card's rows with nothing per cell goes"
    );
    assert_eq!(
        input.chrome_rasters[2], off_rows,
        "CONTROL: a raster off the card's rows is untouched"
    );
}

/// Whether a card raster holds the terminal's orange ink.
fn has_orange_ink(rgba: &[u8]) -> bool {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .any(|px| px[3] > 0x40 && i32::from(px[0]) - i32::from(px[2]) > 60)
}

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
