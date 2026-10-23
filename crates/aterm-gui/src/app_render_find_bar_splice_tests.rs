// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};

/// splices, so the splice under test has a real grid to overwrite.
fn fill_scratch(app: &mut App, wid: WindowId) -> (usize, usize) {
    let (rows, cols) = {
        let ws = app.windows.get(&wid).unwrap();
        (ws.rows as usize, ws.cols as usize)
    };
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let ws = app.windows.get_mut(&wid).unwrap();
    let mut term = term_lock(&terminal);
    term.cell_frame_into(&mut ws.input_scratch, rows, cols);
    (rows, cols)
}

fn row_text(app: &App, wid: WindowId, idx: usize) -> String {
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    cells
        .get(idx)
        .map(|row| row.iter().map(|c| c.ch).collect())
        .unwrap_or_default()
}

fn top_row_text(app: &App, wid: WindowId) -> String {
    row_text(app, wid, 0)
}

/// through the splice's recorded geometry.
fn field_row_text(app: &App, wid: WindowId) -> String {
    let hit = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("the panel recorded its geometry");
    row_text(app, wid, app.tab_strip_rows as usize + hit.row)
}

/// that don't care which row of the panel a run landed on.
fn panel_text(app: &App, wid: WindowId) -> String {
    let hit = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("the panel recorded its geometry");
    let strip = app.tab_strip_rows as usize;
    hit.band
        .clone()
        .map(|row| row_text(app, wid, strip + row))
        .collect::<Vec<_>>()
        .join("\n")
}

/// that keeps the caret on a char boundary at the end of the text.
fn seed_query(app: &mut App, wid: WindowId, query: &str) {
    app.windows
        .get_mut(&wid)
        .unwrap()
        .search
        .as_mut()
        .unwrap()
        .set_query(query.to_string());
}

/// below the band, so this is the default top placement.
#[test]
fn find_bar_appears_on_top_row_while_searching() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\r\n\r\n\r\n\r\nfoo and foo here\r\n");

    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    assert!(
        !top_row_text(&app, wid).contains("Find:"),
        "no find bar before Cmd-F"
    );

    app.search_enter();
    seed_query(&mut app, wid, "foo");
    app.search_recompute();
    assert_eq!(
        app.windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .matches
            .len(),
        2,
        "engine finds both 'foo' hits"
    );

    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let band = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("panel geometry")
        .band
        .clone();
    assert_eq!(
        band,
        0..crate::find_bar::FIND_BAR_ROWS,
        "the panel owns the top band"
    );
    let row = field_row_text(&app, wid);
    assert!(
        row.contains("Find: "),
        "find panel shows the prompt: {row:?}"
    );
    assert!(row.contains("foo"), "find panel shows the query: {row:?}");
    assert!(
        row.contains("1/2"),
        "find panel shows the match position: {row:?}"
    );
    assert!(
        row_text(&app, wid, 4).contains("foo and foo here"),
        "the match row stays uncovered below the panel"
    );

    app.search_exit();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    assert!(
        !top_row_text(&app, wid).contains("Find:"),
        "find bar gone after Esc"
    );
}

/// then the hints); the field row always survives, exactly `cols` wide.
#[test]
fn find_panel_degrades_on_a_terminal_shorter_than_the_band() {
    for rows in [1u16, 2, 3] {
        let mut app = App::headless_for_test();
        let wid = app.insert_logical_window(crate::stub_session(1), rows, 60);
        app.frontmost_window = Some(wid);
        app.search_enter();
        seed_query(&mut app, wid, "q");
        let (_, cols) = fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let hit = app.windows[&wid]
            .find_bar_hit
            .clone()
            .expect("the panel still paints on a short terminal");
        assert_eq!(
            hit.band,
            0..usize::from(rows).min(crate::find_bar::FIND_BAR_ROWS),
            "band clamps to the rows available (rows={rows})"
        );
        assert!(hit.band.contains(&hit.row), "the field row survives");
        let cells = &app.windows[&wid].input_scratch.cells;
        for row in hit.band.clone() {
            assert_eq!(cells[row].len(), cols, "rows={rows}");
        }
        assert!(
            field_row_text(&app, wid).contains("Find: "),
            "rows={rows} keeps the field"
        );
    }
}

/// two binary searches plus the visible rows, not total history.
#[test]
fn find_bar_100k_history_work_is_logarithmic_plus_visible() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_enter();
    let (rows, _) = fill_scratch(&mut app, wid);
    let base_y = app.windows[&wid].input_scratch.base_y;
    let display_offset = app.windows[&wid].input_scratch.display_offset;
    assert_eq!(display_offset, 0, "fixture starts at the live viewport");
    {
        let search = app.windows.get_mut(&wid).unwrap().search.as_mut().unwrap();
        search.query = "x".into();
        search.matches = (-99_976..24).map(|row| (row, 0, 0)).collect();
        search.current = 99_999;
        search.match_base_y = base_y;
        assert_eq!(search.matches.len(), 100_000);
    }

    app.splice_find_bar(wid);

    let ws = &app.windows[&wid];
    assert!(
        ws.find_bar_match_work <= rows + 40,
        "two <=17-step lower bounds plus <=rows visible visits; got {} for {rows} rows",
        ws.find_bar_match_work
    );
    assert!(
        ws.find_bar_match_work >= rows,
        "fixture must witness the visible match visits"
    );
}

/// is at home), the splice hides it so its block doesn't paint over the bar's caret.
#[test]
fn find_bar_hides_cursor_on_covered_row() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\x1b[1;1H");
    app.search_enter();
    seed_query(&mut app, wid, "x");
    app.search_recompute();
    let (rows, _cols) = fill_scratch(&mut app, wid);
    assert!(rows > 1);
    assert_eq!(app.windows.get(&wid).unwrap().input_scratch.cursor_row, 0);
    app.splice_find_bar(wid);
    assert!(
        !app.windows.get(&wid).unwrap().input_scratch.cursor_visible,
        "cursor hidden while the find bar covers its row"
    );
}

#[test]
fn find_bar_owns_its_entire_effect_band_without_sparkle_or_sprite_bleed() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_enter();
    seed_query(&mut app, wid, "absent");
    app.search_recompute();
    fill_scratch(&mut app, wid);
    let (_, ch) = app.win_cell_size(wid);
    let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
    input.ink.push(aterm_render::InkCell {
        row: 0,
        col: 0,
        color: [255, 0, 255],
    });
    input.word_decorations.push(aterm_render::WordDecoration {
        row: 1,
        col: 0,
        dx: 0,
        dy: -1,
        glyph: aterm_render::DecoGlyph::Star4,
        blend: aterm_render::DecoBlend::Add,
        color: 0x00ff_00ff,
        alpha: 255,
    });
    input.nova_add.push(aterm_render::GlowQuad {
        row: 0,
        ..Default::default()
    });
    input.cat_quads.push(aterm_render::SpriteQuad {
        row: 0,
        ..Default::default()
    });
    input.rain_quads.push(aterm_render::SpriteQuad {
        row: 0,
        ..Default::default()
    });
    input.rain_add.push(aterm_render::RainHalo {
        row: 0,
        ..Default::default()
    });
    input.free_sprites.push(aterm_core::render::FreeSprite {
        y: 0,
        h: u16::try_from(ch).unwrap_or(u16::MAX),
        w: 1,
        ..Default::default()
    });

    app.splice_find_bar(wid);
    let input = &app.windows[&wid].input_scratch;
    assert!(input.ink.is_empty());
    assert!(input.word_decorations.is_empty());
    assert!(input.nova_add.is_empty());
    assert!(input.cat_quads.is_empty());
    assert!(input.rain_quads.is_empty());
    assert!(input.rain_add.is_empty());
    assert!(input.free_sprites.is_empty());
}

#[test]
fn bottom_floated_find_bar_scrubs_upward_neighbour_decoration_spill() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_enter();
    fill_scratch(&mut app, wid);
    let rows = app.windows[&wid].input_scratch.rows;
    let base_y = app.windows[&wid].input_scratch.base_y;
    let band_rows = crate::find_bar::FIND_BAR_ROWS;
    assert!(rows >= band_rows + 2);
    {
        let search = app.windows.get_mut(&wid).unwrap().search.as_mut().unwrap();
        search.query = "x".into();
        search.matches = vec![(0, 0, 0)];
        search.current = 0;
        search.match_base_y = base_y;
    }
    app.windows
        .get_mut(&wid)
        .unwrap()
        .input_scratch
        .word_decorations
        .push(aterm_render::WordDecoration {
            row: u16::try_from(rows - band_rows - 1).unwrap_or(u16::MAX),
            col: 0,
            dx: 0,
            dy: 1,
            glyph: aterm_render::DecoGlyph::Star4,
            blend: aterm_render::DecoBlend::Add,
            color: 0x00ff_00ff,
            alpha: 255,
        });
    app.splice_find_bar(wid);
    let ws = &app.windows[&wid];
    let hit = ws.find_bar_hit.as_ref().unwrap();
    assert_eq!(
        hit.band,
        rows - band_rows..rows,
        "panel floated to the bottom"
    );
    let field_offset = usize::from(band_rows >= 3);
    assert_eq!(
        hit.row,
        rows - band_rows + field_offset,
        "field row inside that band"
    );
    assert!(ws.input_scratch.word_decorations.is_empty());
}

/// App path, and flags an invalid regex — the machinery the ⌥⌘C/⌥⌘R chords drive.
#[test]
fn recompute_honors_case_and_regex_toggles() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"foo Foo FOO bar\r\n");
    app.search_enter();

    let total = |app: &App| {
        app.windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .matches
            .len()
    };
    let set = |app: &mut App, f: &mut dyn FnMut(&mut crate::app_search::SearchState)| {
        f(app.windows.get_mut(&wid).unwrap().search.as_mut().unwrap());
    };

    set(&mut app, &mut |s| s.query = "foo".to_string());
    app.search_recompute();
    assert_eq!(total(&app), 3, "case-insensitive foo");

    set(&mut app, &mut |s| s.case_sensitive = true);
    app.search_recompute();
    assert_eq!(total(&app), 1, "case-sensitive foo");

    set(&mut app, &mut |s| {
        s.case_sensitive = false;
        s.is_regex = true;
        s.query = "f.o".to_string();
    });
    app.search_recompute();
    assert_eq!(total(&app), 3, "regex f.o");

    set(&mut app, &mut |s| s.query = "(".to_string());
    app.search_recompute();
    let ws = app.windows.get(&wid).unwrap();
    let s = ws.search.as_ref().unwrap();
    assert!(s.regex_error, "bad regex flagged");
    assert!(s.matches.is_empty(), "bad regex yields no matches");
}

/// one.
fn highlight_bg() -> [u8; 3] {
    let t = aterm_render::Theme::default();
    let selection = crate::settings::u32_rgb(t.selection);
    let hi = crate::chrome_band::ChromePalette::from(t)
        .tokens()
        .search_match(selection);
    assert_ne!(hi, selection);
    hi
}

/// nothing else on either.
#[test]
fn a_hit_that_straddles_a_soft_wrap_is_tinted_on_both_rows_it_occupies() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let cols = app.windows.get(&wid).unwrap().cols as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut input = b"\r\n\r\n\r\n\r\n".to_vec();
    input.extend(std::iter::repeat_n(b'a', cols - 5));
    input.extend_from_slice(b"wrapmatch");
    input.extend(std::iter::repeat_n(b'b', 20));
    term_lock(&term).process(&input);

    app.search_enter();
    seed_query(&mut app, wid, "wrapmatch");
    app.search_recompute();
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().matches,
        vec![(
            4,
            u16::try_from(cols - 5).unwrap(),
            u32::try_from(cols + 3).unwrap()
        )],
        "one hit, ending four columns into the wrapped row"
    );
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("wrapmatch"),
        "the armed selection is the whole word, not the part before the wrap"
    );
    assert_eq!(
        app.windows.get(&wid).unwrap().input_scratch.display_offset,
        0,
        "a live match keeps the viewport at the bottom"
    );

    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let hi = highlight_bg();
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    for (col, cell) in cells[4].iter().enumerate().take(cols).skip(cols - 5) {
        assert_eq!(cell.bg, hi, "row 4 col {col} tinted");
    }
    for (col, cell) in cells[5].iter().enumerate().take(4) {
        assert_eq!(cell.bg, hi, "row 5 col {col} tinted");
    }
    assert_ne!(
        cells[4][cols - 6].bg,
        hi,
        "the column before the hit stays untinted"
    );
    assert_ne!(
        cells[5][4].bg, hi,
        "the column after the hit stays untinted"
    );
}

/// current match's selection lights the same cells.
#[test]
fn highlight_all_tints_the_cells_that_paint_an_rtl_match() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(
        "\r\n\r\n\r\n\r\nabc \u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{05E2}\u{05D5}\u{05DC}\u{05DD} def\r\n"
            .as_bytes(),
    );
    app.search_enter();
    seed_query(&mut app, wid, "\u{05E2}\u{05D5}\u{05DC}\u{05DD}");
    app.search_recompute();
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().matches,
        vec![(4, 9, 12)],
        "the search reports logical columns"
    );
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let hi = highlight_bg();
    let scratch = &app.windows[&wid].input_scratch;
    let tinted: Vec<usize> = scratch.cells[4]
        .iter()
        .enumerate()
        .filter(|(_, cell)| cell.bg == hi)
        .map(|(col, _)| col)
        .collect();
    assert_eq!(tinted, vec![4, 5, 6, 7], "the cells that paint עולם");
    let painted: String = scratch.cells[4][4..=7].iter().map(|c| c.ch).collect();
    assert_eq!(painted, "\u{05DD}\u{05DC}\u{05D5}\u{05E2}");
    let lit: Vec<usize> = (0..scratch.cells[4].len())
        .filter(|&col| scratch.selection_contains_cell(4, col, false, false))
        .collect();
    assert_eq!(lit, tinted, "the current match's selection lights them too");
}

/// steer a selection on that row onto the band's cells.
#[test]
fn a_band_painted_over_a_bidi_row_drops_its_map() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let cols = app.windows[&wid].cols as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term)
        .process("\u{05D0}\u{05D1}\u{05D2} x\r\n\r\n\r\n\r\nab \u{05D3}\u{05D4} cd".as_bytes());
    app.search_enter();
    seed_query(&mut app, wid, "cd");
    app.search_recompute();
    fill_scratch(&mut app, wid);
    assert!(
        !app.windows[&wid]
            .input_scratch
            .visual_to_logical_row(0)
            .is_empty()
    );
    app.splice_find_bar(wid);
    let band = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("panel")
        .band
        .clone();
    assert!(band.contains(&0), "the panel covers row 0");
    assert!(
        app.windows[&wid]
            .input_scratch
            .visual_to_logical_row(0)
            .is_empty(),
        "the find panel's row carries no map"
    );

    fill_scratch(&mut app, wid);
    assert!(
        !app.windows[&wid]
            .input_scratch
            .visual_to_logical_row(4)
            .is_empty()
    );
    let cell_h = app.win_cell_size(wid).1;
    let blank = crate::message_band::blank_band_row(cols, app.chrome_palette_theme());
    app.splice_band_rows_at(wid, 4, vec![blank], cols, cell_h);
    assert!(
        app.windows[&wid]
            .input_scratch
            .visual_to_logical_row(4)
            .is_empty(),
        "a spliced band row carries no map"
    );
}

/// row below the top-anchored band.
#[test]
fn highlight_all_maps_scrollback_current_below_top_bar() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut input = Vec::new();
    for i in 0..3 {
        input.extend_from_slice(format!("topfill {i}\r\n").as_bytes());
    }
    for _ in 0..(rows + 6) {
        input.extend_from_slice(b"hit here\r\n"); // "hit" at cols 0..=2
    }
    term_lock(&term).process(&input);
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    let cur_row = app
        .windows
        .get(&wid)
        .unwrap()
        .search
        .as_ref()
        .unwrap()
        .matches[0]
        .0;
    assert!(cur_row < 0, "top match is in scrollback (row {cur_row})");
    fill_scratch(&mut app, wid);
    assert!(
        app.windows.get(&wid).unwrap().input_scratch.display_offset > 0,
        "viewport scrolled up to show the top match"
    );
    app.splice_find_bar(wid);
    let hi = highlight_bg();
    let landing = crate::find_bar::FIND_BAR_ROWS;
    let panel = panel_text(&app, wid);
    assert!(panel.contains("hit"), "panel echoes the query: {panel:?}");
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    for (col, cell) in cells[landing][0..=2].iter().enumerate() {
        assert_eq!(
            cell.bg, hi,
            "scrollback match tinted at screen row {landing} (below the panel), col {col}"
        );
    }
}

/// to an overline facing the content above.
#[test]
fn find_bar_floats_to_bottom_when_current_match_on_top() {
    use aterm_core::terminal::UnderlineStyle;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"hit");
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    let cur = app
        .windows
        .get(&wid)
        .unwrap()
        .search
        .as_ref()
        .unwrap()
        .matches[0]
        .0;
    assert_eq!(cur, 0, "current match is on the top row");

    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let band_rows = crate::find_bar::FIND_BAR_ROWS;
    let band = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("panel geometry")
        .band
        .clone();
    assert_eq!(band, rows - band_rows..rows, "panel floated to the bottom");
    let field = field_row_text(&app, wid);
    assert!(
        field.contains("Find: ") && field.contains("hit"),
        "floated panel keeps its field: {field:?}"
    );
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    let top: String = cells[0].iter().map(|c| c.ch).collect();
    assert!(
        top.contains("hit"),
        "current match still visible on top: {top:?}"
    );
    assert!(
        cells[band.start].iter().any(|c| c.overline),
        "bottom-floated panel draws an overline seam on its top edge"
    );
    assert!(
        band.clone().all(|row| cells[row]
            .iter()
            .all(|c| c.underline == UnderlineStyle::None)),
        "bottom-floated panel drops the underline seam"
    );
}

/// the highlight-all tint fail closed until search recomputes.
#[test]
fn find_bar_suppresses_stale_absolute_row_geometry() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"hit\r\nhit");
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().matches[0].0,
        0,
        "fixture current match is on the top row"
    );

    fill_scratch(&mut app, wid);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        let search_revision = ws.search.as_ref().unwrap().match_absolute_row_revision;
        assert_eq!(
            ws.input_scratch.absolute_row_revision, search_revision,
            "freshly extracted fixture starts coherent"
        );
        ws.input_scratch.absolute_row_revision = search_revision.saturating_add(1);
    }

    app.splice_find_bar(wid);
    let field = field_row_text(&app, wid);
    assert!(
        field.contains("Find: ") && field.contains("hit"),
        "stale geometry does not hide the panel: {field:?}"
    );
    let ws = &app.windows[&wid];
    assert_eq!(
        ws.find_bar_hit.as_ref().map(|hit| hit.band.clone()),
        Some(0..crate::find_bar::FIND_BAR_ROWS),
        "stale current-row geometry cannot float the panel"
    );
    let hi = highlight_bg();
    assert!(
        ws.input_scratch
            .cells
            .iter()
            .flatten()
            .all(|cell| cell.bg != hi),
        "stale cached matches cannot tint a newer frame"
    );
    assert_eq!(
        ws.find_bar_match_work, 0,
        "stale-frame gate skips the cached match walk"
    );
}

/// Screen rows carrying the highlight-all tint, in frame order.
fn tinted_rows(app: &App, wid: WindowId, hi: [u8; 3]) -> Vec<usize> {
    app.windows[&wid]
        .input_scratch
        .cells
        .iter()
        .enumerate()
        .filter(|(_, row)| row.iter().any(|cell| cell.bg == hi))
        .map(|(idx, _)| idx)
        .collect()
}

/// `finalize_resize` drops the text selection.
#[test]
fn find_bar_suppresses_matches_a_width_reflow_renumbered() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let cols = app.windows[&wid].cols;
    {
        let tail = "-".repeat(usize::from(cols) + usize::from(cols) / 2);
        let mut t = term_lock(&term);
        for i in 1..=12u32 {
            t.process(format!("P{i:02}={tail}\r\n").as_bytes());
        }
    }
    app.search_enter();
    seed_query(&mut app, wid, "P09=");
    app.search_recompute();
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().matches.len(),
        1,
        "fixture: exactly one hit"
    );

    let hi = highlight_bg();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let lit_before = tinted_rows(&app, wid, hi);
    assert_eq!(lit_before.len(), 1, "baseline tints exactly one row");
    assert!(
        row_text(&app, wid, lit_before[0]).starts_with("P09="),
        "baseline tint is on the text that matched: {:?}",
        row_text(&app, wid, lit_before[0])
    );

    let (term_rows, term_cols) = {
        let t = term_lock(&term);
        (t.rows(), t.cols())
    };
    let epoch_before = term_lock(&term).grid().history_renumber_epoch();
    let revision_before = term_lock(&term).absolute_row_revision();
    let narrower = term_cols.saturating_sub(9).max(8);
    term_lock(&term).resize(term_rows, narrower);
    app.windows.get_mut(&wid).unwrap().cols = narrower;
    assert!(
        term_lock(&term).grid().history_renumber_epoch() > epoch_before,
        "fixture: the width rewrap renumbers retained history"
    );
    assert_eq!(
        term_lock(&term).absolute_row_revision(),
        revision_before,
        "...and the footer revision, the tint's only former gate, does not move"
    );

    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);

    let field = field_row_text(&app, wid);
    assert!(
        field.contains("Find: ") && field.contains("P09="),
        "a reflow does not hide the panel: {field:?}"
    );
    for row in tinted_rows(&app, wid, hi) {
        let text = row_text(&app, wid, row);
        assert!(
            text.contains("P09="),
            "the tint claims screen row {row} matched the query, but it reads {text:?}"
        );
    }
    assert!(
        tinted_rows(&app, wid, hi).is_empty(),
        "renumbered match rows cannot tint a post-reflow frame"
    );
    assert_eq!(
        app.windows[&wid].find_bar_match_work, 0,
        "the gate skips the cached match walk outright"
    );

    app.search_recompute();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let lit_after = tinted_rows(&app, wid, hi);
    assert_eq!(
        lit_after.len(),
        1,
        "the recomputed batch tints one row again"
    );
    assert!(
        row_text(&app, wid, lit_after[0]).starts_with("P09="),
        "...and it is the line that actually matched: {:?}",
        row_text(&app, wid, lit_after[0])
    );
}

/// closed.
#[test]
fn find_bar_suppresses_matches_a_viewport_only_width_reflow_moved() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let cols = app.windows[&wid].cols;
    {
        let tail = "-".repeat(usize::from(cols) + usize::from(cols) / 2 - 4);
        let mut t = term_lock(&term);
        for i in 1..=6u32 {
            t.process(format!("Q{i:02}={tail}\r\n").as_bytes());
        }
    }
    assert_eq!(
        term_lock(&term).grid().scrollback_lines(),
        0,
        "fixture: the whole point is a reflow with an EMPTY scrollback — \
         nothing here may cross the live/history boundary"
    );

    app.search_enter();
    seed_query(&mut app, wid, "Q05=");
    app.search_recompute();
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().matches.len(),
        1,
        "fixture: exactly one hit"
    );

    let hi = highlight_bg();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let lit_before = tinted_rows(&app, wid, hi);
    assert_eq!(lit_before.len(), 1, "baseline tints exactly one row");
    assert!(
        row_text(&app, wid, lit_before[0]).starts_with("Q05="),
        "baseline tint is on the text that matched: {:?}",
        row_text(&app, wid, lit_before[0])
    );

    let (term_rows, term_cols) = {
        let t = term_lock(&term);
        (t.rows(), t.cols())
    };
    let revision_before = term_lock(&term).absolute_row_revision();
    let narrower = (term_cols / 2).max(8);
    term_lock(&term).resize(term_rows, narrower);
    app.windows.get_mut(&wid).unwrap().cols = narrower;
    assert_eq!(
        term_lock(&term).grid().scrollback_lines(),
        0,
        "fixture: the rewrap still spliced nothing into history — this is \
         the half the splice-site epoch bumps cannot see"
    );
    assert_eq!(
        term_lock(&term).absolute_row_revision(),
        revision_before,
        "...and the footer revision, the tint's only former gate, does not move"
    );

    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);

    let field = field_row_text(&app, wid);
    assert!(
        field.contains("Find: ") && field.contains("Q05="),
        "a reflow does not hide the panel: {field:?}"
    );
    for row in tinted_rows(&app, wid, hi) {
        let text = row_text(&app, wid, row);
        assert!(
            text.contains("Q05="),
            "the tint claims screen row {row} matched the query, but it reads {text:?}"
        );
    }
    assert!(
        tinted_rows(&app, wid, hi).is_empty(),
        "renumbered match rows cannot tint a post-reflow frame"
    );

    app.search_recompute();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let lit_after = tinted_rows(&app, wid, hi);
    assert_eq!(
        lit_after.len(),
        1,
        "the recomputed batch tints one row again"
    );
    assert!(
        row_text(&app, wid, lit_after[0]).starts_with("Q05="),
        "...and it is the line that actually matched: {:?}",
        row_text(&app, wid, lit_after[0])
    );
}

/// renderer's own selection-fg floor.
#[test]
fn highlight_floors_noncurrent_fg_for_contrast() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let hi = highlight_bg();
    let sgr = format!("\x1b[38;2;{};{};{}m", hi[0], hi[1], hi[2]);
    let input = format!("hit\r\n{sgr}hit\x1b[0m\r\n");
    term_lock(&term).process(input.as_bytes());
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();

    fill_scratch(&mut app, wid);
    let cur_fg_before = app.windows.get(&wid).unwrap().input_scratch.cells[0][0].fg;
    app.splice_find_bar(wid);

    let expected = crate::settings::u32_rgb(aterm_render::floor_selection_fg(
        aterm_render::rgb_to_u32(hi),
        aterm_render::rgb_to_u32(hi),
    ));
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    for (col, cell) in cells[1][0..=2].iter().enumerate() {
        assert_eq!(cell.bg, hi, "non-current match bg tinted (col {col})");
        assert_ne!(
            cell.fg, hi,
            "non-current match fg lifted off the tint (col {col})"
        );
        assert_eq!(
            cell.fg, expected,
            "non-current match fg floored (col {col})"
        );
    }
    for (col, cell) in cells[0][0..=2].iter().enumerate() {
        assert_eq!(
            cell.fg, cur_fg_before,
            "current match fg untouched (col {col})"
        );
    }

    fn wcag_ratio(a: [u8; 3], b: [u8; 3]) -> f32 {
        fn lin(c: u8) -> f32 {
            let s = f32::from(c) / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        }
        let lum = |c: [u8; 3]| 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2]);
        let (la, lb) = (lum(a), lum(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }
    assert!(
        wcag_ratio(hi, hi) < 1.01,
        "sanity: tint-on-tint (the fg the floor replaced) is the 1:1 worst case"
    );
    let achieved = wcag_ratio(expected, hi);
    assert!(
        achieved >= 4.4,
        "floored fg independently clears WCAG ~4.5:1 against the tint (got {achieved:.2}:1)"
    );
}

/// resetting to the literal / case-insensitive defaults.
#[test]
fn find_toggles_persist_across_sessions() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_enter();
    {
        let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
        assert!(
            !s.case_sensitive && !s.is_regex,
            "defaults: literal, insensitive"
        );
    }
    app.search_toggle_case();
    app.search_toggle_regex();
    app.search_exit();
    app.search_enter();
    let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
    assert!(s.case_sensitive, "case toggle persisted across sessions");
    assert!(s.is_regex, "regex toggle persisted across sessions");
}

/// Windows no button is recorded, and a click on the well moves no match.
#[test]
fn clicking_the_toolbar_buttons_steps_and_closes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"one hit\r\ntwo hit\r\nthree hit\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    let current = |app: &App| app.windows[&wid].search.as_ref().map(|s| s.current);
    let paint = |app: &mut App| {
        fill_scratch(app, wid);
        app.splice_find_bar(wid);
        app.windows[&wid]
            .find_bar_hit
            .clone()
            .expect("splice recorded the panel geometry")
    };
    let hit = paint(&mut app);
    let row = u16::try_from(hit.row).unwrap();
    let col = |span: &std::ops::Range<usize>| u16::try_from(span.start + 1).unwrap();
    assert_eq!(
        hit.next_cols.is_some(),
        cfg!(windows),
        "buttons on Windows only"
    );
    let before = current(&app);
    let well = u16::try_from(hit.field_cols.start).unwrap();
    assert!(app.find_bar_click(wid, row, well));
    assert_eq!(current(&app), before, "CONTROL: the well moves no match");
    let (Some(prev), Some(next), Some(close)) = (hit.prev_cols, hit.next_cols, hit.close_cols)
    else {
        return;
    };
    let raster = app.windows[&wid]
        .input_scratch
        .chrome_raster(hit.row)
        .expect("the field row carries a raster for the close icon");
    assert_eq!(
        raster.icon_at(close.start + 1),
        Some(aterm_render::BandIcon::Error)
    );
    assert_eq!(raster.icons.len(), 1, "the arrows keep the face's glyphs");
    assert!(app.find_bar_click(wid, row, col(&next)), "↓ consumed");
    let stepped = current(&app);
    assert_ne!(stepped, before, "↓ stepped to the next match");
    let hit = paint(&mut app);
    assert_eq!(hit.prev_cols, Some(prev.clone()), "the buttons hold still");
    assert!(app.find_bar_click(wid, row, col(&prev)), "↑ consumed");
    assert_eq!(current(&app), before, "↑ stepped back");
    let _ = paint(&mut app);
    assert!(app.find_bar_click(wid, row, col(&close)), "✕ consumed");
    assert!(app.windows[&wid].search.is_none(), "✕ closed the bar");
}

/// default); a click off the indicators changes nothing.
#[test]
fn clicking_indicators_toggles_via_recorded_geometry() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"click target here\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "target");
    app.search_recompute();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let hit = app
        .windows
        .get(&wid)
        .unwrap()
        .find_bar_hit
        .clone()
        .expect("splice recorded the indicator geometry");
    let row = u16::try_from(hit.row).unwrap();
    let case_col = u16::try_from(hit.case_cols.expect("Aa drawn").start).unwrap();
    let regex_col = u16::try_from(hit.regex_cols.expect(".* drawn").start).unwrap();

    assert!(app.find_bar_click(wid, row, case_col), "Aa click consumed");
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .case_sensitive
    );
    assert!(app.search_sticky_case, "click updated the sticky default");
    assert!(app.find_bar_click(wid, row, regex_col), ".* click consumed");
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .is_regex
    );
    assert!(app.search_sticky_regex);
    assert!(
        app.find_bar_click(wid, row, 0),
        "a click on the panel is consumed"
    );
    let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
    assert!(
        s.case_sensitive && s.is_regex,
        "off-indicator click changed no toggle"
    );
    let below = u16::try_from(hit.band.end).unwrap();
    assert!(
        !app.find_bar_click(wid, below, 0),
        "a click below the panel is not consumed"
    );

    let field = hit.field_cols.clone();
    let caret_col = u16::try_from(field.start + 3).unwrap();
    assert!(
        app.find_bar_click(wid, row, caret_col),
        "well click consumed"
    );
    let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
    assert_eq!(s.query, "target", "clicking the well never edits the text");
    assert_eq!(s.cursor, 3, "caret landed on the clicked character");
    app.search_edit_in(wid, crate::app_search::SearchEdit::MoveStart);
    let right_of_caret = u16::try_from(field.start + 4).unwrap();
    assert!(
        app.find_bar_click(wid, row, right_of_caret),
        "well click consumed"
    );
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().cursor,
        4,
        "a click right of the caret lands on the clicked character"
    );
    let past = u16::try_from(field.end - 1).unwrap();
    assert!(
        field.len() > "target".len() + 1,
        "the well has room past the text"
    );
    assert!(app.find_bar_click(wid, row, past), "well click consumed");
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().cursor,
        "target".len(),
        "a click past the text parks the caret at the end"
    );
}

/// matches are tinted at shifted rows, and the strip row is untouched.
#[test]
fn find_bar_splices_below_tab_strip() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"hit one\r\nhit two\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();

    let (rows, _) = fill_scratch(&mut app, wid); // input_scratch = `rows` terminal rows
    app.splice_tab_strip_with(wid, 1); // prepend the 1-row strip → `rows + 1` frame rows
    app.splice_find_bar(wid);

    let hi = highlight_bg();
    let band_rows = crate::find_bar::FIND_BAR_ROWS;
    let hit = app.windows[&wid]
        .find_bar_hit
        .clone()
        .expect("panel geometry");
    assert_eq!(
        hit.band,
        rows - band_rows..rows,
        "panel on the frame bottom"
    );
    let field = field_row_text(&app, wid);
    assert!(
        field.contains("Find: ") && field.contains("hit"),
        "the field row rides inside that band: {field:?}"
    );
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    assert_eq!(cells.len(), rows + 1, "strip added exactly one frame row");
    let above_band: String = cells[rows - band_rows].iter().map(|c| c.ch).collect();
    assert!(
        !above_band.contains("Find:"),
        "panel is NOT one row high (would be the off-by-strip bug): {above_band:?}"
    );
    for (term_row, frame_row) in [(0usize, 1usize), (1, 2)] {
        for (col, cell) in cells[frame_row][0..=2].iter().enumerate() {
            assert_eq!(
                cell.bg, hi,
                "match from term row {term_row} tinted at frame row {frame_row}, col {col}"
            );
        }
    }
    assert!(
        cells[0].iter().all(|c| c.bg != hi),
        "the tab-strip row is untouched by the highlight tint"
    );
}

/// shifted rows.
#[test]
fn find_bar_default_top_sits_below_tab_strip() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\r\n\r\n\r\n\r\nhit one\r\nhit two\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();

    let (rows, _) = fill_scratch(&mut app, wid);
    app.splice_tab_strip_with(wid, 1);
    app.splice_find_bar(wid);

    let hi = highlight_bg();
    let hit = app.windows[&wid]
        .find_bar_hit
        .clone()
        .expect("panel geometry");
    assert_eq!(
        hit.band,
        0..crate::find_bar::FIND_BAR_ROWS,
        "panel band starts at the first TERMINAL row (below the strip)"
    );
    let field = field_row_text(&app, wid);
    assert!(
        field.contains("Find: ") && field.contains("hit"),
        "panel directly below the tab strip: {field:?}"
    );
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    assert_eq!(cells.len(), rows + 1, "strip added exactly one frame row");
    let strip_row: String = cells[0].iter().map(|c| c.ch).collect();
    assert!(
        !strip_row.contains("Find:"),
        "the strip row keeps its own chrome: {strip_row:?}"
    );
    for (term_row, frame_row) in [(4usize, 5usize), (5, 6)] {
        for (col, cell) in cells[frame_row][0..=2].iter().enumerate() {
            assert_eq!(
                cell.bg, hi,
                "match from term row {term_row} tinted at frame row {frame_row}, col {col}"
            );
        }
    }
}

/// selected (ready for ⌘C), and remembers the query for empty-query recall.
#[test]
fn accept_keeps_viewport_and_remembers_query() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut input = Vec::new();
    for _ in 0..(rows + 6) {
        input.extend_from_slice(b"hit here\r\n");
    }
    term_lock(&term).process(&input);

    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    let offset = term_lock(&term).grid().display_offset();
    assert!(offset > 0, "find navigation scrolled into scrollback");

    app.search_accept();
    assert!(
        app.windows.get(&wid).unwrap().search.is_none(),
        "accept closed find mode"
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        offset,
        "accept left the viewport at the match"
    );
    assert!(
        term_lock(&term).selection_to_string().is_some(),
        "accept kept the match selected"
    );
    assert_eq!(
        app.search_last_query, "hit",
        "accept remembered the query for recall"
    );
    let (accepted_session, _, _, accepted_row, accepted_start, accepted_end) = app
        .search_last_anchor
        .expect("accept retained the absolute current-match anchor");
    assert_eq!(accepted_session, 0, "anchor is bound to session 0");
    let accepted_anchor = (accepted_row, accepted_start, accepted_end);

    let _ = app.search_find_again(true);
    let resumed = app.windows[&wid]
        .search
        .as_ref()
        .expect("Find Next reopened the accepted query");
    assert_eq!(resumed.query, "hit");
    let (row, start, end) = resumed.current_match().expect("resumed current match");
    assert!(
        (resumed.match_base_y + i64::from(row), start, end) > accepted_anchor,
        "Find Next resumed strictly after the accepted match"
    );
}

#[test]
fn find_again_never_reuses_an_absolute_anchor_from_another_session() {
    let mut app = App::headless_for_test();
    let first = WindowId(0);
    let first_term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&first_term).process(b"hit one\r\nhit two");
    app.search_enter();
    app.windows
        .get_mut(&first)
        .unwrap()
        .search
        .as_mut()
        .unwrap()
        .query = "hit".into();
    app.search_recompute();
    app.search_accept();
    assert_eq!(app.search_last_anchor.unwrap().0, 0);

    let second = app.insert_logical_window(crate::stub_session(1), 24, 80);
    app.frontmost_window = Some(second);
    let second_term = app.pool.get(1).expect("session 1").term.clone();
    term_lock(&second_term).process(b"hit alpha\r\nhit beta");
    let _ = app.search_find_again(true);
    let search = app.windows[&second]
        .search
        .as_ref()
        .expect("app-sticky query opens in the second session");
    assert_eq!(search.query, "hit");
    assert_eq!(
        search.current, 0,
        "foreign absolute coordinates are ignored; forward search starts at the first local match"
    );
}

/// anchor resumes at the first forward hit.
#[test]
fn find_again_refuses_an_absolute_anchor_a_width_reflow_renumbered() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let cols = app.windows[&wid].cols;
    {
        let tail = "-".repeat(usize::from(cols) + usize::from(cols) / 2);
        let mut t = term_lock(&term);
        for i in 0..12u32 {
            let mark = if matches!(i, 1 | 5 | 9) { "hit" } else { "pad" };
            t.process(format!("{mark}{i:02}={tail}\r\n").as_bytes());
        }
    }
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    app.search_step(true);
    app.search_accept();
    let (_, accepted_revision, accepted_epoch, ..) = app
        .search_last_anchor
        .expect("accept parks the absolute current-match anchor");

    let (term_rows, term_cols) = {
        let t = term_lock(&term);
        (t.rows(), t.cols())
    };
    let narrower = term_cols.saturating_sub(9).max(8);
    term_lock(&term).resize(term_rows, narrower);
    app.windows.get_mut(&wid).unwrap().cols = narrower;
    assert_eq!(
        term_lock(&term).absolute_row_revision(),
        accepted_revision,
        "fixture: the reflow moves no footer revision — the old gate sees nothing"
    );
    assert!(
        term_lock(&term).grid().history_renumber_epoch() > accepted_epoch,
        "fixture: …while the renumber epoch, which does see it, advanced"
    );

    app.search_find_again(true)
        .expect("Find Next resumes the accepted query");
    let resumed = app.windows[&wid]
        .search
        .as_ref()
        .expect("Find Next reopened the accepted query");
    assert_eq!(resumed.query, "hit");
    assert_eq!(
        resumed.current, 0,
        "a renumbered absolute anchor is ignored; forward search starts at the first hit"
    );
}

#[test]
fn accepted_anchor_keeps_the_revision_its_match_was_computed_in() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows[&wid].rows;
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"hit");
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();

    let region_bottom = rows - 2;
    term_lock(&term)
        .process(format!("\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes());
    assert_eq!(term_lock(&term).absolute_row_revision(), 1);
    assert_eq!(
        app.windows[&wid]
            .search
            .as_ref()
            .unwrap()
            .match_absolute_row_revision,
        0,
        "fixture intentionally accepts before the output wake refresh"
    );

    app.search_accept();
    let (_, accepted_revision, ..) = app
        .search_last_anchor
        .expect("accepted query retains its match anchor");
    assert_eq!(
        accepted_revision, 1,
        "accept must refresh stale coordinates and retain the revision actually searched"
    );
}

#[test]
fn active_search_defers_then_recomputes_after_codex_footer_row_splice() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows[&wid].rows;
    let term = app.pool.get(0).expect("session 0").term.clone();

    term_lock(&term).process(format!("\x1b[{rows};1HFOOTERNEEDLE").as_bytes());
    app.search_enter();
    seed_query(&mut app, wid, "FOOTERNEEDLE");
    app.search_recompute();
    assert_eq!(
        app.windows[&wid]
            .search
            .as_ref()
            .unwrap()
            .match_absolute_row_revision,
        0
    );

    let region_bottom = rows - 2;
    term_lock(&term).process(
        format!("\x1b[1;1HA\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes(),
    );
    assert_eq!(term_lock(&term).absolute_row_revision(), 1);

    app.search_refresh_for_output(0);
    let stale = app.windows[&wid]
        .search
        .as_ref()
        .expect("find remains open while invalidated");
    assert!(stale.results_dirty, "streaming output defers the rebuild");
    assert_eq!(stale.match_absolute_row_revision, 0);
    assert!(
        term_lock(&term).selection_to_string().is_none(),
        "a stale protected-footer match is never highlighted"
    );

    app.search_repeat(true);
    let search = app.windows[&wid]
        .search
        .as_ref()
        .expect("repeat refreshes the invalidated find");
    assert!(!search.results_dirty);
    assert_eq!(search.match_absolute_row_revision, 1);
    let (row, _, _) = search
        .current_match()
        .expect("footer match remains searchable");
    assert_eq!(
        search.match_base_y + i64::from(row),
        i64::from(rows),
        "the footer match follows the logical-row insertion"
    );
}

/// does not remember the cancelled query.
#[test]
fn cancel_restores_origin_viewport() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut input = Vec::new();
    for _ in 0..(rows + 8) {
        input.extend_from_slice(b"hit here\r\n");
    }
    term_lock(&term).process(&input);
    term_lock(&term).scroll_display(3);
    assert_eq!(term_lock(&term).grid().display_offset(), 3);

    let live_top = u64::try_from(term_lock(&term).grid().base_y()).unwrap();
    app.search_enter();
    {
        let origin = app
            .find_origins
            .get(&(0, wid))
            .expect("enter parked the origin viewport");
        assert_eq!(
            origin.top_visible_absolute_row,
            live_top - 3,
            "the parked anchor is the absolute row the user was reading"
        );
        assert!(!origin.was_live, "the user was scrolled back, not tailing");
    }
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    assert_ne!(
        term_lock(&term).grid().display_offset(),
        3,
        "find navigation moved the viewport away from the origin"
    );

    app.search_cancel();
    assert!(
        app.windows.get(&wid).unwrap().search.is_none(),
        "cancel closed find mode"
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        3,
        "cancel restored the origin viewport"
    );
    assert!(
        term_lock(&term).selection_to_string().is_none(),
        "cancel cleared the match highlight"
    );
    assert!(
        app.search_last_query.is_empty(),
        "a cancelled query is not remembered"
    );
}

/// anchored row lands one line up in history.
#[test]
fn cancel_at_the_live_tail_survives_a_codex_footer_splice() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows[&wid].rows;
    let term = app.pool.get(0).expect("session 0").term.clone();

    app.search_enter();
    assert!(
        app.find_origins
            .get(&(0, wid))
            .expect("enter parked the origin viewport")
            .was_live,
        "the fixture opens find at the live bottom"
    );
    let region_bottom = rows - 2;
    term_lock(&term).process(
        format!("\x1b[1;1HA\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes(),
    );
    assert_eq!(term_lock(&term).absolute_row_revision(), 1);
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        0,
        "output at the live tail remains at the live tail"
    );

    app.search_cancel();
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        0,
        "a tail follower is handed the tail back, not the row the splice archived"
    );
}

/// further `^S` steps normally, and with nothing accepted it is a no-op.
#[test]
fn repeat_recalls_last_accepted_query() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"hit a\r\nhit b\r\nhit c\r\n");

    let state = |app: &App| {
        let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
        (s.query.clone(), s.matches.len(), s.current)
    };

    app.search_enter();
    app.search_repeat(true);
    let (q, n, _) = state(&app);
    assert!(q.is_empty() && n == 0, "nothing to recall on a fresh app");

    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    app.search_accept();

    app.search_enter();
    app.search_repeat(true);
    assert_eq!(state(&app), ("hit".to_string(), 3, 0), "forward recall");
    app.search_repeat(true);
    assert_eq!(state(&app).2, 1, "second ^S steps to the next match");
    app.search_cancel();

    app.search_enter();
    app.search_repeat(false);
    assert_eq!(state(&app), ("hit".to_string(), 3, 2), "backward recall");
}

/// match-case and its sticky default.
#[test]
fn find_bar_toggle_fires_through_on_mouse_input() {
    use aterm_window::event::{ElementState, MouseButton};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"target line here\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "target");
    app.search_recompute();
    {
        let metrics = &mut app.windows.get_mut(&wid).unwrap().metrics;
        metrics.pad = 12;
        metrics.pad_top = 4;
        metrics.head = 3;
    }
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let hit = app
        .windows
        .get(&wid)
        .unwrap()
        .find_bar_hit
        .clone()
        .expect("splice recorded the indicator geometry");
    let case = hit.case_cols.expect("Aa drawn");
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let pad_top = app.win_pad_top(wid);
    let head = app.win_head(wid);
    let frame_row = app.tab_strip_rows as usize + hit.row;
    let x = (pad + case.start * cw) as f64 + cw as f64 / 2.0;
    let y = (pad_top + head + frame_row * ch) as f64 + ch as f64 / 2.0;
    app.windows.get_mut(&wid).unwrap().last_cursor_px = (x, y);
    assert!(
        !app.windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .case_sensitive,
        "case-sensitive off before the click"
    );
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .case_sensitive,
        "a left press on `Aa` toggled match-case through the full handler"
    );
    assert!(
        app.search_sticky_case,
        "the dispatch updated the sticky default too"
    );
}

/// only presses inside the bar's cell band.
#[test]
fn find_bar_pixel_gate_rejects_pad_clamp_click() {
    use aterm_window::event::{ElementState, MouseButton};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"target line here\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "target");
    app.search_recompute();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let hit = app
        .windows
        .get(&wid)
        .unwrap()
        .find_bar_hit
        .clone()
        .expect("splice recorded the indicator geometry");
    let case = hit.case_cols.expect("Aa drawn");
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let clamp_row = hit.band.end - 1;
    let frame_row = app.tab_strip_rows as usize + clamp_row;
    let x = (pad + case.start * cw) as f64 + cw as f64 / 2.0;
    let y_below = (pad + (frame_row + 1) * ch) as f64 + 2.0;
    assert_eq!(
        hit.band.end, rows,
        "panel floated to the bottom (the clamp target)"
    );
    assert_eq!(
        app.pixel_to_cell(wid, x, y_below).0 as usize,
        clamp_row,
        "the below-grid press clamps into the panel band"
    );
    assert!(
        !app.find_bar_pixel_hit(wid, x, y_below),
        "the pixel gate rejects the pad press the row-clamp would have admitted"
    );
    app.windows.get_mut(&wid).unwrap().last_cursor_px = (x, y_below);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert!(
        !app.windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .case_sensitive,
        "a below-grid pad press must NOT toggle match-case"
    );
    assert!(
        !app.search_sticky_case,
        "the sticky default is untouched by the rejected press"
    );
}

/// grid after the search while the match stays on screen, and the tint follows the match's row.
#[test]
fn splice_reanchors_highlight_after_output_scrolls_in() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut input = Vec::new();
    for i in 0..10 {
        input.extend_from_slice(format!("above {i}\r\n").as_bytes());
    }
    input.extend_from_slice(b"UNIQNEEDLE\r\n");
    for i in 0..(rows - 12) {
        input.extend_from_slice(format!("below {i}\r\n").as_bytes());
    }
    term_lock(&term).process(&input);
    app.search_enter();
    seed_query(&mut app, wid, "UNIQNEEDLE");
    app.search_recompute();
    let match_base_y = app
        .windows
        .get(&wid)
        .unwrap()
        .search
        .as_ref()
        .unwrap()
        .match_base_y;

    let mut more = Vec::new();
    for i in 0..5 {
        more.extend_from_slice(format!("stream {i}\r\n").as_bytes());
    }
    term_lock(&term).process(&more);

    fill_scratch(&mut app, wid); // re-extract: input_scratch.base_y has advanced
    let base_y_now = app.windows.get(&wid).unwrap().input_scratch.base_y;
    assert!(
        base_y_now > match_base_y,
        "output scrolled the grid in since the search (delta = {} > 0)",
        base_y_now - match_base_y
    );

    app.splice_find_bar(wid);
    let bar_row = app
        .windows
        .get(&wid)
        .unwrap()
        .find_bar_hit
        .as_ref()
        .map(|h| h.row);
    let hi = highlight_bg();
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    let needle_row = cells
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != bar_row)
        .find(|(_, r)| {
            r.iter()
                .map(|c| c.ch)
                .collect::<String>()
                .contains("UNIQNEEDLE")
        })
        .map(|(i, _)| i)
        .expect("UNIQNEEDLE still visible after streaming");
    assert!(
        cells[needle_row][0..10].iter().all(|c| c.bg == hi),
        "the re-anchored highlight tints the row UNIQNEEDLE occupies NOW (frame row {needle_row})"
    );
}

/// column rejects (so the column clamp cannot land it on an indicator).
#[test]
fn find_bar_pixel_hit_boundaries_and_right_edge() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"boundary target\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "target");
    app.search_recompute();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let hit = app
        .windows
        .get(&wid)
        .unwrap()
        .find_bar_hit
        .clone()
        .expect("bar geometry");
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let pad_top = app.win_pad_top(wid);
    let head = app.win_head(wid);
    let cols = app.windows.get(&wid).unwrap().cols as usize;
    let frame_row = app.tab_strip_rows as usize + hit.band.start;
    let top = (pad_top + head + frame_row * ch) as f64;
    let height = (hit.band.len() * ch) as f64;
    let x_in = (pad + 5 * cw) as f64 + cw as f64 / 2.0; // a column well inside the grid
    assert!(
        app.find_bar_pixel_hit(wid, x_in, top),
        "py == top (first band row) accepts"
    );
    assert!(
        app.find_bar_pixel_hit(wid, x_in, top + height - 1.0),
        "py == top+height-1 (last band row) accepts"
    );
    assert!(
        !app.find_bar_pixel_hit(wid, x_in, top - 1.0),
        "py == top-1 (row above) rejects"
    );
    assert!(
        !app.find_bar_pixel_hit(wid, x_in, top + height),
        "py == top+height (first row below) rejects"
    );
    let x_past = (pad + cols * cw) as f64 + 1.0;
    assert!(
        !app.find_bar_pixel_hit(wid, x_past, top + ch as f64 / 2.0),
        "a press past the right grid edge rejects (no column-clamp false toggle)"
    );
    assert!(
        app.find_bar_pixel_hit(wid, x_in, top + ch as f64 / 2.0),
        "an in-band, in-grid press accepts"
    );
}

/// every pane, hid both pane's top rows, and highlight-all was switched off.
#[test]
fn find_in_a_split_rides_the_focused_pane_with_highlight_all() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.split_active_stub_tab(wid);
    let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
    let leaf = plan
        .leaves
        .iter()
        .find(|leaf| leaf.focused)
        .expect("a focused leaf");
    let at = |v: f32| v.round() as usize;
    let (row0, col0) = (at(leaf.rect.origin.y), at(leaf.rect.origin.x));
    let width = at(leaf.rect.size.width);
    assert!(
        col0 > 0,
        "the fixture is side by side: the focused pane is the right one"
    );
    let sibling = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&sibling).process(
        "LEFT hit\r\n"
            .repeat(crate::find_bar::FIND_BAR_ROWS)
            .as_bytes(),
    );
    let focused = app.pool.get(session).expect("focused pane").term.clone();
    term_lock(&focused).process(b"\r\n\r\n\r\n\r\naa hit one\r\nbb hit two\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (usize::from(ws.rows), usize::from(ws.cols))
    };
    app.redraw_compose(
        wid,
        rows,
        cols,
        false,
        false,
        None,
        0,
        std::time::Instant::now(),
    )
    .expect("a composed split frame");
    app.splice_find_bar(wid);

    let hit = app.windows[&wid]
        .find_bar_hit
        .clone()
        .expect("panel geometry");
    assert_eq!(
        hit.cols,
        col0..col0 + width,
        "the panel covers the focused pane's columns"
    );
    assert_eq!(
        hit.band,
        row0..row0 + crate::find_bar::FIND_BAR_ROWS,
        "the panel rides the focused pane's top rows"
    );
    let cells = &app.windows[&wid].input_scratch.cells;
    let text = |row: usize, span: std::ops::Range<usize>| -> String {
        cells[row][span].iter().map(|c| c.ch).collect()
    };
    let field = text(hit.row, col0..col0 + width);
    assert!(
        field.contains("Find: hit"),
        "the field sits in the pane: {field:?}"
    );
    for row in hit.band.clone() {
        let left = text(row, 0..col0);
        assert!(
            left.starts_with("LEFT hit"),
            "the sibling keeps its own cells beside the panel (row {row}): {left:?}"
        );
    }
    let hi = highlight_bg();
    for pane_row in [4, 5] {
        let row = row0 + pane_row;
        let tinted: String = cells[row][col0..col0 + width]
            .iter()
            .filter(|c| c.bg == hi)
            .map(|c| c.ch)
            .collect();
        assert_eq!(
            tinted, "hit",
            "pane row {pane_row}: the hit is tinted where it is painted"
        );
    }
    for (row, line) in cells.iter().enumerate() {
        assert!(
            line[..col0].iter().all(|c| c.bg != hi),
            "the sibling is not tinted (row {row})"
        );
    }
    let room = app.backend.band_room();
    for raster in &app.windows[&wid].input_scratch.chrome_rasters {
        assert_eq!(
            aterm_render::chrome_fit(raster, room).0,
            0,
            "the {} px rail on frame row {} lifts its row at room {room:?}",
            raster.rail_h,
            raster.row
        );
    }
    let row = u16::try_from(hit.row).expect("row");
    assert!(
        !app.find_bar_click(wid, row, 1),
        "a click on the sibling is not consumed"
    );
    assert!(
        app.find_bar_click(wid, row, u16::try_from(col0 + 1).expect("col")),
        "a click on the panel is"
    );
}

/// its `✕` short of the end; off Windows no button is recorded.
#[test]
fn the_toolbar_ends_at_its_panes_margin_full_and_split() {
    let wid = WindowId(0);
    let margin = 2;
    let last_mark = |app: &App, span: std::ops::Range<usize>| {
        let ws = &app.windows[&wid];
        let hit = ws.find_bar_hit.clone().expect("geometry");
        let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
        let row = &ws.input_scratch.cells[strip + hit.row];
        let last = span
            .rev()
            .find(|&col| row.get(col).is_some_and(|c| c.ch != ' '))
            .expect("the field row is painted");
        (last, hit.close_cols)
    };
    let open = |app: &mut App, session: u64| {
        let term = app.pool.get(session).expect("session").term.clone();
        term_lock(&term).process(b"one hit\r\ntwo hit\r\n");
        app.search_enter();
        seed_query(app, wid, "hit");
        app.search_recompute();
    };

    let wide = |app: &mut App| app.windows.get_mut(&wid).expect("window").cols = 111;
    let mut app = App::headless_for_test();
    wide(&mut app);
    open(&mut app, 0);
    let (_, cols) = fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let (last, close) = last_mark(&app, 0..cols);
    if !crate::find_bar::FIND_TOOLBAR {
        assert!(close.is_none(), "CONTROL: no buttons off Windows");
        return;
    }
    let close = close.expect("a full-width pane draws the buttons");
    assert_eq!(close.end, cols - margin, "the `✕` ends the full band");
    assert_eq!(last, close.start + 1, "and is its last mark");

    let mut app = App::headless_for_test();
    wide(&mut app);
    let session = app.split_active_stub_tab(wid);
    open(&mut app, session);
    let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
    let leaf = plan
        .leaves
        .iter()
        .find(|l| l.focused)
        .expect("focused leaf");
    let col0 = leaf.rect.origin.x.round() as usize;
    let end = col0 + leaf.rect.size.width.round() as usize;
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (usize::from(ws.rows), usize::from(ws.cols))
    };
    app.redraw_compose(
        wid,
        rows,
        cols,
        false,
        false,
        None,
        0,
        std::time::Instant::now(),
    )
    .expect("a composed split frame");
    app.splice_find_bar(wid);
    let (last, close) = last_mark(&app, col0..end);
    assert!(
        end - col0 < 55 || close.is_some(),
        "a {}-column pane keeps its buttons",
        end - col0
    );
    match close {
        Some(close) => {
            assert_eq!(close.end, end - margin, "the `✕` ends the pane's band");
            assert_eq!(last, close.start + 1);
        }
        None => assert_eq!(last + 1, end - margin, "the count ends the pane's band"),
    }
}

/// No room below the words: no rail at all.
#[test]
fn a_pane_scoped_rail_never_lifts_its_row() {
    use super::unlifted_rail_h;
    assert_eq!(unlifted_rail_h(2, (2, 2)), Some(1), "the well line thins");
    assert_eq!(unlifted_rail_h(1, (2, 2)), Some(1), "the 1 px seam fits");
    assert_eq!(
        unlifted_rail_h(2, (4, 3)),
        Some(2),
        "room for the whole line"
    );
    assert_eq!(unlifted_rail_h(1, (2, 1)), None, "even 1 px would lift");
    let well = super::find_rail(0, 0..1, [0; 3], 2);
    assert_eq!(aterm_render::chrome_fit(&well, (2, 2)).0, 1);
}

/// pixels above the edge); elsewhere it stays the face's underline. Never both.
#[test]
fn the_top_panel_closes_on_a_rail_under_its_last_row() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\r\n\r\n\r\n\r\nzz hit\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    let (_, cols) = fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let (cell_w, cell_h) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let rails = aterm_render::band::seam_can_rail(
        cell_h,
        aterm_render::band::native_host_room(|| app.backend.band_room()),
    );
    if cfg!(windows) {
        assert!(rails, "the Windows default cell takes the rail");
    }
    let hit = app.windows[&wid]
        .find_bar_hit
        .clone()
        .expect("panel geometry");
    assert_eq!(hit.band.start, 0, "the panel rides the top");
    let last = hit.band.end - 1;
    let input = &app.windows[&wid].input_scratch;
    let underlined = hit
        .band
        .clone()
        .flat_map(|row| input.cells[row].iter())
        .any(|c| c.underline != aterm_core::terminal::UnderlineStyle::None);
    let raster = input.chrome_raster(last);
    if rails {
        let raster = raster.expect("a rail on the band's last row");
        assert_eq!((raster.rail_h, raster.clear_rail), (1, true));
        assert!(raster.ground.is_empty(), "the cells keep their own ground");
        assert_eq!(
            raster.rail.len(),
            pad + cols * cell_w,
            "under the cells, not the gutter"
        );
        assert!(
            raster.rail[..pad]
                .iter()
                .all(|&p| p == aterm_render::ChromeRaster::KEEP)
        );
        let band = crate::chrome_band::band_colors(
            app.chrome_palette(crate::app_render::ChromeSurface::OverGrid(wid)),
        );
        let ink = aterm_render::rgb_to_u32(band.seam);
        assert!(
            raster.rail[pad..].iter().all(|&p| p == ink),
            "one tone, edge to edge"
        );
        assert!(
            !underlined,
            "no underline crosses the hints: the rail is the rule"
        );
        let well = input.chrome_raster(hit.row);
        if crate::find_bar::WELL_ACCENT_LINE {
            let well = well.expect("the well's focus line");
            assert_eq!((well.rail_h, well.clear_rail), (2, true));
            let (x0, x1) = (
                pad + hit.field_cols.start * cell_w,
                pad + hit.field_cols.end * cell_w,
            );
            assert_eq!(well.rail.len(), x1, "it ends at the well's right edge");
            assert!(
                well.rail[..x0]
                    .iter()
                    .all(|&p| p == aterm_render::ChromeRaster::KEEP)
            );
            let accent = aterm_render::rgb_to_u32(band.accent);
            assert!(
                well.rail[x0..].iter().all(|&p| p == accent),
                "the band's accent"
            );
        } else {
            assert!(well.is_none(), "no focus line off Windows");
        }
    } else {
        assert!(
            raster.is_none(),
            "a short cell keeps the underline, without a rail"
        );
        assert!(underlined, "the underline is the rule");
    }
}

/// colours.
#[test]
fn the_current_match_is_painted_in_its_own_fill() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\r\n\r\n\r\n\r\nxx hit\r\nyy hit\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();
    fill_scratch(&mut app, wid);
    let before = app.windows[&wid].input_scratch.selection_bg;
    app.splice_find_bar(wid);
    let t = aterm_render::Theme::default();
    let tokens = crate::chrome_band::ChromePalette::from(t).tokens();
    let fill = crate::find_bar::current_match_fill(&tokens, highlight_bg());
    let input = &app.windows[&wid].input_scratch;
    assert_eq!(input.selection_bg, aterm_render::rgb_to_u32(fill));
    let ink = crate::find_bar::current_match_ink(&tokens, fill);
    assert_eq!(input.selection_fg, aterm_render::rgb_to_u32(ink));
    assert_eq!(
        ink, tokens.bg,
        "the default theme's background clears AAA as is"
    );
    assert!(
        aterm_messages::ink::contrast(ink, fill) >= 10.0,
        "the text reads at full strength on the fill, not a mid-grey floor"
    );
    assert_ne!(
        input.selection_bg, before,
        "the fill replaced the selection colour"
    );

    let mut app = App::headless_for_test();
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"nothing to see\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "absent");
    app.search_recompute();
    fill_scratch(&mut app, wid);
    let before = app.windows[&wid].input_scratch.selection_bg;
    app.splice_find_bar(wid);
    assert_eq!(
        app.windows[&wid].input_scratch.selection_bg, before,
        "CONTROL: no current match, no repaint of the selection"
    );
}

/// the band's label tone.
#[test]
fn authored_search_match_and_border_reach_the_find_splice() {
    let paint = |config: &str| {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config = aterm_toml::from_str(config).expect("valid toml");
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"aa hit bb\r\ncc hit dd\r\n");
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let cells = &app.windows[&wid].input_scratch.cells;
        let tint = cells[1][3].bg;
        let seam = cells
            .iter()
            .flatten()
            .find_map(|cell| {
                if cell.overline {
                    cell.overline_color
                } else if cell.underline != aterm_core::terminal::UnderlineStyle::None {
                    cell.underline_color
                } else {
                    None
                }
            })
            .expect("the panel seals its edge");
        let band = crate::chrome_band::band_colors(
            app.chrome_palette(crate::app_render::ChromeSurface::OverGrid(wid)),
        );
        (tint, seam, band)
    };
    let bg = crate::settings::u32_rgb(aterm_render::Theme::default().bg);
    let (tint, seam, band) =
        paint("search_match_color = \"#335533\"\nborder_color = \"#5a5a5a\"\n");
    assert_eq!(
        tint,
        aterm_messages::ink::ensure_contrast([0x33, 0x55, 0x33], bg, 1.5)
    );
    assert_eq!(seam, band.seam);
    assert_eq!(
        seam,
        aterm_messages::ink::ensure_contrast([0x5a; 3], band.bar_bg, 1.5)
    );
    let (tint, seam, band) = paint("");
    assert_eq!(tint, highlight_bg(), "CONTROL: the derived tint");
    assert_eq!(seam, band.label, "CONTROL: the label seam");
}

/// installed (`faces`) or cleared: the Fluent face's precondition.
fn painted_with_faces(faces: bool, lines: &[u8], query: &str) -> App {
    let mut app = App::headless_for_test();
    if faces {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    } else {
        crate::tray_raster::clear_ui_fonts_for_test();
    }
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(lines);
    app.search_enter();
    seed_query(&mut app, wid, query);
    app.search_recompute();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    app
}

/// ever laid.
#[test]
fn the_windows_bar_lays_its_fluent_face_over_the_band() {
    let wid = WindowId(0);
    let lines = b"\r\n\r\n\r\n\r\none hit\r\ntwo hit\r\n";
    let band_rows = |app: &App| {
        let ws = &app.windows[&wid];
        let hit = ws.find_bar_hit.clone().expect("geometry");
        let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
        (hit.clone(), strip + hit.band.start..strip + hit.band.end)
    };
    let app = painted_with_faces(true, lines, "hit");
    let (hit, rows) = band_rows(&app);
    let input = &app.windows[&wid].input_scratch;
    let laid = |input: &aterm_render::RenderInput, row: usize| {
        input
            .images
            .get(row)
            .is_some_and(|images| !images.is_empty())
    };
    if !crate::find_bar::FIND_TOOLBAR || !crate::tray_raster::strip_band_ui_ready() {
        assert!(
            rows.clone().all(|row| !laid(input, row)),
            "CONTROL: no face off Windows"
        );
        if !crate::find_bar::FIND_TOOLBAR {
            return;
        }
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    for row in rows.clone() {
        let images = &input.images[row];
        assert_eq!(
            images.len(),
            hit.cols.len(),
            "row {row}: a raster cell a column"
        );
        let first = &images[0].1.image;
        for (i, (col, cell)) in images.iter().enumerate() {
            assert_eq!(*col, hit.cols.start + i);
            assert_eq!(usize::from(cell.cell_col), i);
            assert!(
                std::sync::Arc::ptr_eq(&cell.image, first),
                "one raster a row"
            );
            assert_eq!(cell.image.z_index, 0, "over the cells' glyphs");
        }
        assert!(
            input.cells[row].iter().all(|cell| {
                cell.underline == aterm_core::terminal::UnderlineStyle::None && !cell.overline
            }),
            "row {row}: no cell rule draws over the raster"
        );
        assert!(
            input.chrome_raster(row).is_none(),
            "row {row}: no rail, focus line or cross raster"
        );
    }
    let field = field_row_text(&app, wid);
    assert!(
        field.contains("Find: hit") && field.contains("1/2"),
        "the cells keep the words: {field:?}"
    );

    let cold = painted_with_faces(false, lines, "hit");
    let (hit, rows) = band_rows(&cold);
    let input = &cold.windows[&wid].input_scratch;
    assert!(
        rows.clone().all(|row| !laid(input, row)),
        "CONTROL: no UI face, no raster"
    );
    let close = hit.close_cols.expect("the buttons");
    let raster = input
        .chrome_raster(rows.start + (hit.row - hit.band.start))
        .expect("CONTROL: the cell bar draws its cross on a raster");
    assert_eq!(
        raster.icon_at(close.start + 1),
        Some(aterm_render::BandIcon::Error)
    );
}

/// characters short.
#[test]
fn a_press_in_the_fluent_box_lands_on_the_character_under_the_pointer() {
    use aterm_window::event::{ElementState, MouseButton};
    let wid = WindowId(0);
    let query = "needle in the hay";
    let mut app = painted_with_faces(true, b"a needle in the haystack\r\n", query);
    if !crate::find_bar::FIND_TOOLBAR {
        return;
    }
    if !crate::tray_raster::strip_band_ui_ready() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let hit = app.windows[&wid].find_bar_hit.clone().expect("geometry");
    let (cell_w, cell_h) = app.win_cell_size(wid);
    let geom = crate::find_bar::FluentGeom {
        cell_w,
        cell_h,
        scale: app.windows[&wid].scale as f32,
    };
    let col0 = hit.cols.start;
    let field = hit.field_cols.start - col0..hit.field_cols.end - col0;
    let well = crate::find_bar::well_box(&field, geom);
    let (ox, oy) = app.frame_origin(wid);
    let pad = app.win_pad(wid);
    let strip = app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows);
    let y = (oy as usize + app.win_pad_top(wid) + app.win_head(wid) + (strip + hit.row) * cell_h)
        as f64
        + cell_h as f64 / 2.0;
    let window_x = |panel_x: f32| (ox as usize + pad + col0 * cell_w) as f64 + f64::from(panel_x);
    let press = |app: &mut App, panel_x: f32| {
        app.windows.get_mut(&wid).expect("window").last_cursor_px = (window_x(panel_x), y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        app.windows[&wid].search.as_ref().expect("searching").cursor
    };
    let body = crate::type_scale::TypeStep::Body
        .px(crate::tab_bar::chrome_label_base_px(
            cell_h as f32,
            geom.scale,
        ))
        .get();
    let width =
        |s: &str| crate::tray_raster::ui_text_width_for(crate::widget::TextFace::Ui, s, body);
    let x = well.text_x0 + width("needle in") + 1.0;
    assert_eq!(
        press(&mut app, x),
        "needle in".len(),
        "the boundary under it"
    );
    let by_cell = hit.field_scroll + (x as usize / cell_w).saturating_sub(field.start);
    assert_ne!(
        by_cell,
        "needle in".len(),
        "CONTROL: by cell it is elsewhere"
    );
    assert!(well.x0 < geom.cell_w as f32 * field.start as f32);
    assert_eq!(press(&mut app, well.x0 + 1.0), 0, "the box's own inset");
    let s = app.windows[&wid].search.as_ref().expect("searching");
    assert_eq!(s.query, query, "a press never edits the text");
}

/// NEGATIVE CONTROL: the cell bar underlines the composed cell.
#[test]
fn the_fluent_box_owns_the_composition_underline() {
    let wid = WindowId(0);
    let compose = |faces: bool| {
        let mut app = painted_with_faces(faces, b"$ ", "ab");
        app.on_ime_preedit(wid, "cd".to_string(), Some((0, 2)));
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        app
    };
    let underlined = |app: &App| {
        let ws = &app.windows[&wid];
        let hit = ws.find_bar_hit.as_ref().expect("geometry");
        let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
        ws.input_scratch.cells[strip + hit.row]
            .iter()
            .any(|cell| cell.underline != aterm_core::terminal::UnderlineStyle::None)
    };
    let cold = compose(false);
    assert!(
        underlined(&cold),
        "CONTROL: the cell bar underlines the cell"
    );
    let app = compose(true);
    assert!(
        app.windows[&wid].last_ime_rect.is_some(),
        "anchored on the field"
    );
    if crate::find_bar::FIND_TOOLBAR && crate::tray_raster::strip_band_ui_ready() {
        assert!(!underlined(&app), "the raster draws it, no cell does");
    }
}

/// one of its toolbar's buttons or toggles, on its field row.
fn control_at(hit: &crate::FindBarHit, (row, col): (usize, usize)) -> Option<&'static str> {
    if row != hit.row {
        return None;
    }
    [
        ("Aa", &hit.case_cols),
        (".*", &hit.regex_cols),
        ("prev", &hit.prev_cols),
        ("next", &hit.next_cols),
        ("close", &hit.close_cols),
    ]
    .into_iter()
    .find_map(|(name, span)| {
        span.as_ref()
            .is_some_and(|s| s.contains(&col))
            .then_some(name)
    })
}

/// whether find is open.
fn find_state(app: &App, wid: WindowId) -> Option<(bool, bool, usize)> {
    let s = app.windows[&wid].search.as_ref()?;
    Some((s.case_sensitive, s.is_regex, s.current))
}

/// the same pixel is the control's, which a press fires.
#[test]
fn a_regrid_leaves_the_find_bars_last_band_answering_nothing() {
    use aterm_window::event::{ElementState, MouseButton};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut out = Vec::new();
    for i in 0..8 {
        out.extend_from_slice(format!("filler {i}\r\n").as_bytes());
    }
    out.extend_from_slice(b"target one\r\ntarget two\r\n");
    term_lock(&term).process(&out);
    app.search_enter();
    seed_query(&mut app, wid, "target");
    app.search_recompute();
    let frame = |app: &mut App| {
        fill_scratch(app, wid);
        app.splice_find_bar(wid);
        app.windows[&wid]
            .find_bar_hit
            .clone()
            .expect("bar geometry")
    };
    let hit = frame(&mut app);
    assert_eq!(hit.band.start, 0, "fixture: the bar rides the top rows");
    let laid = hit.laid;
    assert_eq!(laid, app.glass_grid(wid), "stamped with its grid");
    let chrome = usize::from(app.chrome_rows(wid));
    let term_cell = |app: &App, x: f64, y: f64| {
        let (row, col) = app.frame_cell_at(wid, x, y)?;
        Some((row.checked_sub(chrome)?, col))
    };
    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy) = app.frame_origin(wid);
    let (pad, top) = (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid));
    let cells: Vec<((usize, usize), (f64, f64))> = hit
        .cols
        .clone()
        .map(|col| {
            let x = ox as f64 + (pad + col * cw + cw / 2) as f64;
            let y = oy as f64 + (top + (chrome + hit.row) * ch + ch / 2) as f64;
            assert_eq!(
                term_cell(&app, x, y),
                Some((hit.row, col)),
                "the pixel names the cell"
            );
            ((hit.row, col), (x, y))
        })
        .collect();
    assert!(
        cells
            .iter()
            .any(|&(cell, _)| control_at(&hit, cell).is_some()),
        "the bar drew a control"
    );
    let base = app.font_size;
    let zoom = |app: &mut App, size: f32| assert!(app.set_font_size_with(size, |_| true));
    let (size, was, now, (x, y)) = (1..=16u8)
        .find_map(|step| {
            let size = base + f32::from(step);
            zoom(&mut app, size);
            let found = cells.iter().find_map(|&(cell, (x, y))| {
                let was = control_at(&hit, cell);
                let now = control_at(&hit, term_cell(&app, x, y)?)?;
                (Some(now) != was).then_some((size, was, now, (x, y)))
            });
            zoom(&mut app, base);
            found
        })
        .expect("a zoom puts a control the glass does not show under a pixel");
    let on = |app: &App| {
        app.find_bar_cell_at(wid, x, y)
            .map(|(row, col)| control_at(&hit, (usize::from(row), usize::from(col))))
    };

    assert_eq!(app.glass_grid(wid), laid, "the zoom undone");
    assert_eq!(
        on(&app),
        Some(was),
        "CONTROL: the painted cell under the pointer"
    );

    zoom(&mut app, size);
    let zoomed = app.glass_grid(wid);
    assert_ne!(zoomed, laid, "the zoom re-grids the window");
    let set_laid = |app: &mut App, laid| {
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.find_bar_hit.as_mut())
            .expect("bar")
            .laid = laid;
    };
    let press = |app: &mut App| {
        app.windows.get_mut(&wid).expect("window").last_cursor_px = (x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    };
    let before = find_state(&app, wid);
    assert!(before.is_some(), "find is open");
    set_laid(&mut app, zoomed);
    assert_eq!(
        on(&app),
        Some(Some(now)),
        "CONTROL: unstamped, the stale band puts a control under the press"
    );
    press(&mut app);
    let fired = find_state(&app, wid);
    assert_ne!(fired, before, "CONTROL: and the press fires it");
    press(&mut app);
    assert_eq!(find_state(&app, wid), before, "fixture: the toggle undone");

    set_laid(&mut app, laid);
    assert_eq!(
        app.find_bar_cell_at(wid, x, y),
        None,
        "a band laid on another grid answers nothing"
    );
    press(&mut app);
    assert_eq!(
        find_state(&app, wid),
        before,
        "the press fired no control of the stale band"
    );

    let relaid = frame(&mut app);
    assert_eq!(relaid.laid, zoomed, "laid again on the zoomed grid");
    let cell = term_cell(&app, x, y).expect("on the grid");
    assert_eq!(
        app.find_bar_cell_at(wid, x, y)
            .map(|(row, col)| (usize::from(row), usize::from(col))),
        Some(cell),
        "the bar laid again answers"
    );
    assert_eq!(
        control_at(&relaid, cell),
        Some(now),
        "with the control it paints there"
    );
}
