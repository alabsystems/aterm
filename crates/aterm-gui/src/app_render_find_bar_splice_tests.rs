// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};

/// Fill the window's render scratch from the engine, as the redraw does before its
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

/// Frame text of the find panel's field row (`Find: ` plus the well), located
/// through the splice's recorded geometry.
fn field_row_text(app: &App, wid: WindowId) -> String {
    let hit = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("the panel recorded its geometry");
    row_text(app, wid, app.tab_strip_rows as usize + hit.row)
}

/// The whole find panel band as one string (rows joined) — for content assertions
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

/// Seed the open find's query the way a user's typing does — through the one seam
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

/// The Cmd-F find panel appears on the top terminal rows once find is active,
/// using the real search path (`search_recompute`) so the match count comes
/// from the engine; before searching the splice is a no-op. The matches sit
/// below the band, so this is the default top placement.
#[test]
fn find_bar_appears_on_top_row_while_searching() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    // Two "foo" hits on live row 4 — clear of the rows the panel now owns.
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

    // Render + splice: the panel's band owns the top rows, its field row shows the
    // query and the 1-based position, and the matches' own row is left uncovered.
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

/// A terminal shorter than the panel shrinks the band (dropping the pad,
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

/// With 100,000 sorted hits and one screen visible, the compositor's work is
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

/// When the terminal cursor sits on the top line (⌘F on a fresh screen whose prompt
/// is at home), the splice hides it so its block doesn't paint over the bar's caret.
#[test]
fn find_bar_hides_cursor_on_covered_row() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    // Park the cursor at home, under the band.
    term_lock(&term).process(b"\x1b[1;1H");
    app.search_enter();
    seed_query(&mut app, wid, "x");
    app.search_recompute();
    let (rows, _cols) = fill_scratch(&mut app, wid);
    // Precondition: the cursor is on row 0, inside the default top band.
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
            // The row directly above the floated band, jittering one pixel down into
            // it; row-tag-only scrubbing would miss this neighbour spill.
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
    assert_eq!(hit.row, rows - band_rows + 1, "field row inside that band");
    assert!(ws.input_scratch.word_decorations.is_empty());
}

/// End-to-end: `search_recompute` honors the case + regex toggles through the real
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

    // Default literal, case-insensitive: `foo` hits foo/Foo/FOO = 3.
    set(&mut app, &mut |s| s.query = "foo".to_string());
    app.search_recompute();
    assert_eq!(total(&app), 3, "case-insensitive foo");

    // Case-sensitive: only the lowercase `foo`.
    set(&mut app, &mut |s| s.case_sensitive = true);
    app.search_recompute();
    assert_eq!(total(&app), 1, "case-sensitive foo");

    // Regex, case-insensitive again: `f.o` matches all three.
    set(&mut app, &mut |s| {
        s.case_sensitive = false;
        s.is_regex = true;
        s.query = "f.o".to_string();
    });
    app.search_recompute();
    assert_eq!(total(&app), 3, "regex f.o");

    // Invalid regex → flagged, zero matches (the bar's "bad regex" state).
    set(&mut app, &mut |s| s.query = "(".to_string());
    app.search_recompute();
    let ws = app.windows.get(&wid).unwrap();
    let s = ws.search.as_ref().unwrap();
    assert!(s.regex_error, "bad regex flagged");
    assert!(s.matches.is_empty(), "bad regex yields no matches");
}

/// The highlight-all colour `splice_find_bar` paints: the default theme's
/// search-match token (`aterm_messages::tokens::ChromeTokens::search_match`; no
/// OSC 11 in headless, and Default's accent is its green cursor whatever the
/// palette). It must stand apart from the selection the current match is
/// painted in, or the tests below could not tell a tinted cell from a selected
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

/// A hit straddling a soft wrap is one match: the tint covers the tail
/// columns of its first row and the head columns of the wrapped row, and
/// nothing else on either.
#[test]
fn a_hit_that_straddles_a_soft_wrap_is_tinted_on_both_rows_it_occupies() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let cols = app.windows.get(&wid).unwrap().cols as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    // Four blank rows keep the hit clear of the panel's band; "wrapmatch"
    // then starts 5 columns before the wrap and finishes 4 columns after it.
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

/// On a BiDi row the hit's logical columns are painted elsewhere: in
/// `abc שלום עולם def` the search finds עולם at logical 9..=12, which the frame
/// paints reversed at 4..=7, while 9..=12 paint שלום. The tint follows the
/// frame's visual→logical map onto the cells that show the match, and the
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

/// A band painted over a BiDi row is host content in logical order, so the row's
/// visual→logical map goes with the cells it replaced: the find panel's band and a generic
/// band splice (`splice_band_rows_at`) both drop it, so the terminal row's permutation cannot
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

/// Highlight-all maps a scrollback match through `display_offset`
/// (`screen = sel_row + display_offset`): the current match lands, tinted, on the first screen
/// row below the top-anchored band.
#[test]
fn highlight_all_maps_scrollback_current_below_top_bar() {
    // Pushes matches into scrollback: serialize against the test that lowers the
    // process-global search cap.
    let _serial = crate::control::search_cap_test_guard();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    // Non-matching filler above the first hit gives the oldest match history
    // above it (so it lands below the bar rather than clamping at the top);
    // more matching lines than the window push early hits into scrollback.
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

/// When the current match sits on the top row, the bar floats to the bottom and its seam flips
/// to an overline facing the content above.
#[test]
fn find_bar_floats_to_bottom_when_current_match_on_top() {
    use aterm_core::terminal::UnderlineStyle;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    // The only match sits at home, where the default top band would cover it.
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

/// A frame extracted after a non-uniform absolute-row splice never uses
/// pre-splice cached match rows: the bar stays, but adaptive placement and
/// the highlight-all tint fail closed until search recomputes.
#[test]
fn find_bar_suppresses_stale_absolute_row_geometry() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    // Current match at row 0 would normally float the bar to the bottom;
    // a second visible match witnesses whether highlight-all still ran.
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
        // Model a newer frame extracted after a protected-footer splice,
        // before the UI wake has recomputed the open search.
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

/// A width reflow (edge drag, font zoom, divider move) rewraps retained
/// history and renumbers every absolute row without moving any
/// `absolute_row_revision`, so cached match rows would tint the wrong line.
/// The overlay fails closed until the search recomputes, as
/// `finalize_resize` drops the text selection.
#[test]
fn find_bar_suppresses_matches_a_width_reflow_renumbered() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let cols = app.windows[&wid].cols;
    // Twelve tagged lines that soft-wrap at the starting width and push
    // history into scrollback, so a stale landing hits a different
    // match-shaped token rather than blank space.
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

    // The reflow, with no output behind it — the bare geometry change an
    // edge drag or a font zoom is.
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
    // A painted tint claims "this text matched", so every lit row must still hold the query.
    for row in tinted_rows(&app, wid, hi) {
        let text = row_text(&app, wid, row);
        assert!(
            text.contains("P09="),
            "the tint claims screen row {row} matched the query, but it reads {text:?}"
        );
    }
    // And the whole batch is refused until a recompute, because a rewrap is
    // not a uniform `base_y` delta and there is nothing honest to re-anchor.
    assert!(
        tinted_rows(&app, wid, hi).is_empty(),
        "renumbered match rows cannot tint a post-reflow frame"
    );
    assert_eq!(
        app.windows[&wid].find_bar_match_work, 0,
        "the gate skips the cached match walk outright"
    );

    // A recompute repairs it: the tint returns to the real hit.
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

/// A width reflow with empty scrollback also renumbers rows: every splice-site
/// bump early-returns, but `resize_with_reflow_mode` raises
/// `history_renumber_epoch` for every width rewrap, so the tint still fails
/// closed.
#[test]
fn find_bar_suppresses_matches_a_viewport_only_width_reflow_moved() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let cols = app.windows[&wid].cols;
    // Six lines 1.5 screens wide: two rows each at the starting width, three
    // after halving (eighteen rows, inside a 24-row viewport), so nothing
    // reaches history.
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

    // A pure width drag: rows constant, no output behind it.
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

/// A non-current highlighted match whose fg is close to the tint is lifted
/// to a legible (WCAG) contrast; the current match's fg is left to the
/// renderer's own selection-fg floor.
#[test]
fn highlight_floors_noncurrent_fg_for_contrast() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let hi = highlight_bg();
    // Row 0's default-fg hit is current; row 1's is not, and its fg is the tint itself
    // (1:1 contrast).
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

    // An independent WCAG check (standard sRGB relative luminance), not just
    // `== floor_selection_fg(..)`.
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

/// The match-case / regex modes persist across find sessions rather than
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
    // Close and reopen — the sticky app state seeds the new session.
    app.search_exit();
    app.search_enter();
    let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
    assert!(s.case_sensitive, "case toggle persisted across sessions");
    assert!(s.is_regex, "regex toggle persisted across sessions");
}

/// A click on the `Aa` / `.*` indicators, hit-tested against the geometry
/// `splice_find_bar` recorded, flips match-case / regex (and the sticky
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
    // On the panel but off the indicators: consumed (the band is chrome), flips nothing.
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

    // A click in the well moves the caret to the character under the pointer.
    let field = hit.field_cols.clone();
    let caret_col = u16::try_from(field.start + 3).unwrap();
    assert!(
        app.find_bar_click(wid, row, caret_col),
        "well click consumed"
    );
    let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
    assert_eq!(s.query, "target", "clicking the well never edits the text");
    assert_eq!(s.cursor, 3, "caret landed on the clicked character");
    // Both sides of the caret: the well maps cell to character directly (the
    // block caret takes no cell), so a click right of the caret is not one short.
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
    let past = u16::try_from(field.start + 40).unwrap();
    assert!(app.find_bar_click(wid, row, past), "well click consumed");
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().cursor,
        "target".len(),
        "a click past the text parks the caret at the end"
    );
}

/// With a tab strip prepended, `splice_find_bar` places the bar and match
/// tints by frame index (`strip + terminal_row`). The current match is on
/// the top terminal row, so the bar floats to the true frame bottom, the
/// matches are tinted at shifted rows, and the strip row is untouched.
#[test]
fn find_bar_splices_below_tab_strip() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let term = app.pool.get(0).expect("session 0").term.clone();
    // "hit" on terminal rows 0 and 1 (two matches; matches[0] is the current one).
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
    // The band is in terminal rows: ending on the frame bottom puts it at
    // `rows - band_rows..rows`, not one row high.
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

/// Default top placement with a tab strip: the bar lands at frame row
/// `strip + 0`, directly below the strip, and match tints land at their
/// shifted rows.
#[test]
fn find_bar_default_top_sits_below_tab_strip() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let term = app.pool.get(0).expect("session 0").term.clone();
    // "hit" on terminal rows 4 and 5 — clear of the band the panel defaults to.
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

/// Accept (⏎): closing find keeps the viewport, keeps the current match
/// selected (ready for ⌘C), and remembers the query for empty-query recall.
#[test]
fn accept_keeps_viewport_and_remembers_query() {
    // Pushes matches into scrollback — serialize against the search-cap mutation test.
    let _serial = crate::control::search_cap_test_guard();
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

    // Cmd-G after Enter reopens the accepted query and continues strictly
    // after that match.
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

/// Cmd-G's parked anchor is an absolute row, so it is refused when either
/// the footer revision or the width-reflow renumber stamp moved; a refused
/// anchor resumes at the first forward hit.
#[test]
fn find_again_refuses_an_absolute_anchor_a_width_reflow_renumbered() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let cols = app.windows[&wid].cols;
    // Wrapped, scrolled-back history: the rows a narrowing rewrap renumbers.
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
    let _serial = crate::control::search_cap_test_guard();
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

    // Codex's full-width 1..rows-2 history area keeps two footer rows fixed
    // while inserting a row just above them. The displaced top row must be
    // written: never-written rows scroll without archival, splice or bump.
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

/// Cancel (⎋ / ^G) restores the viewport captured at `search_enter`, even
/// after navigating deep into scrollback, clears the match highlight, and
/// does not remember the cancelled query.
#[test]
fn cancel_restores_origin_viewport() {
    let _serial = crate::control::search_cap_test_guard();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut input = Vec::new();
    for _ in 0..(rows + 8) {
        input.extend_from_slice(b"hit here\r\n");
    }
    term_lock(&term).process(&input);
    // Reading 3 lines above the live bottom at ⌘F.
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

/// A find opened at the live tail cancels back to the live tail, even across
/// a protected-footer splice that archives the row the viewport showed.
/// `FindOrigin::was_live` keeps a tail follower on the tail; without it the
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
    // Row 0 must be written for the displaced row to archive and splice.
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

/// `^S`/`^R` on an empty query recall the last accepted query (emacs
/// `C-s C-s`): forward lands on the first match, backward on the last, a
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

/// A left press on the `Aa` indicator through the full `on_mouse_input` seam
/// (modal misses, strip miss, find-bar pixel gate, cell map, toggle) flips
/// match-case and its sticky default.
#[test]
fn find_bar_toggle_fires_through_on_mouse_input() {
    use winit::event::{ElementState, MouseButton};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"target line here\r\n");
    app.search_enter();
    seed_query(&mut app, wid, "target");
    app.search_recompute();
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    // An attached native window: 12 px on every edge but a 4 px top inset
    // with a 3 px chrome band above it (asymmetric padding).
    {
        let metrics = &mut app.windows.get_mut(&wid).unwrap().metrics;
        metrics.pad = 12;
        metrics.pad_top = 4;
        metrics.head = 3;
    }
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
    // The centre of `Aa`'s first cell.
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

/// A left press in the bottom `pad` border that `pixel_to_cell` clamps onto
/// the bar's bottom row does not toggle a mode: the pixel-band gate admits
/// only presses inside the bar's cell band.
#[test]
fn find_bar_pixel_gate_rejects_pad_clamp_click() {
    use winit::event::{ElementState, MouseButton};
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
    // `Aa`'s x, two pixels into the bottom pad below the band.
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

/// The match re-anchor (`delta = base_y_now − match_base_y`) at delta > 0: output scrolls the
/// grid after the search while the match stays on screen, and the tint follows the match's row.
#[test]
fn splice_reanchors_highlight_after_output_scrolls_in() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows.get(&wid).unwrap().rows as usize;
    let term = app.pool.get(0).expect("session 0").term.clone();
    // The needle mid-screen, with no scrollback yet.
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

    // More output scrolls the grid (delta > 0) while the needle stays on screen.
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
    // Skip the bar's row: its query echo also reads UNIQNEEDLE.
    let bar_row = app
        .windows
        .get(&wid)
        .unwrap()
        .find_bar_hit
        .as_ref()
        .map(|h| h.row);
    let hi = highlight_bg();
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    // The row that shows UNIQNEEDLE now must be tinted; a tint ignoring
    // delta would land on a filler row.
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

/// `find_bar_pixel_hit` admits a click only inside the panel's cell band
/// `[top, top + rows*ch)` and inside the grid columns: the first/last device
/// rows of the band accept, one px outside rejects, and a press past the last
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

/// In a split the splice cannot place match tints in the focused pane, so
/// highlight-all is suppressed while the bar row is still spliced.
#[test]
fn highlight_all_suppressed_in_split() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"hit one\r\nhit two\r\n"); // two live matches
    app.search_enter();
    seed_query(&mut app, wid, "hit");
    app.search_recompute();

    let split = app
        .active_tree_mut(wid)
        .expect("active tree")
        .split_focused(crate::pane::SplitDir::Vertical, 1);
    assert!(split, "the tab split into two panes");
    assert!(
        app.active_tree(wid).is_some_and(|t| t.len() > 1),
        "the window is now multi-pane"
    );

    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let hi = highlight_bg();
    let field = field_row_text(&app, wid);
    assert!(
        field.contains("Find: ") && field.contains("hit"),
        "the find panel is still spliced in a split: {field:?}"
    );
    let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
    for (r, row) in cells.iter().enumerate() {
        assert!(
            row.iter().all(|c| c.bg != hi),
            "no highlight-all tint anywhere in a split (row {r})"
        );
    }
}

/// AN AUTHORED SEARCH MATCH AND BORDER REACH THE FIND SPLICE: with
/// `search_match_color` and `border_color` in `aterm.toml`, every visible
/// non-current match is the authored tint lifted to its rise off the live
/// background, and the panel's seam is the authored border floored on the band.
/// NEGATIVE CONTROL: without the keys the tint is the derived one and the seam
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
        // Row 1's hit is not the current match (the current one is the
        // selection), so it wears the tint.
        let tint = cells[1][3].bg;
        // The panel seals its content-facing edge: an overline when it floats to
        // the bottom (the current match is under the top rows here), else an
        // underline.
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
