// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn view(query: &str) -> FindBarView {
    FindBarView {
        query: query.to_string(),
        cursor: query.len(),
        idx: 1,
        total: 0,
        case_sensitive: false,
        is_regex: false,
        regex_error: false,
        truncated: false,
        searched_lines: None,
        match_capped: false,
        cut_short: false,
        stale: false,
        history_away: false,
    }
}

fn paint(v: &FindBarView, cols: usize) -> FindBarPaint {
    find_bar_paint(
        v,
        cols,
        FIND_BAR_ROWS,
        Theme::default(),
        FindSeam::Top,
        true,
    )
}

fn text(row: &[RenderCell]) -> String {
    row.iter().map(|cell| cell.ch).collect()
}

/// The whole panel as one string, rows joined.
fn all(p: &FindBarPaint) -> String {
    p.rows
        .iter()
        .map(|r| text(r))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The well's contents as a string, and the column the block caret landed on.
fn well(p: &FindBarPaint) -> (String, usize) {
    let row = &p.rows[p.field_row];
    let c = chrome_band::band_colors(Theme::default());
    let caret = p
        .field_cols
        .clone()
        .find(|&col| row[col].bg == caret_ink(&c))
        .expect("the caret is always inside the well");
    (
        row[p.field_cols.clone()].iter().map(|c| c.ch).collect(),
        caret,
    )
}

/// The well's geometry depends on the window width only, never on the status text, so it cannot move under the caret mid-edit.
#[test]
fn well_geometry_is_independent_of_the_status_text() {
    for cols in [60usize, 80, 100, 132, 200] {
        let empty = paint(&view(""), cols).field_cols;
        for v in [
            FindBarView {
                idx: 1,
                total: 3,
                ..view("q")
            },
            FindBarView {
                idx: 100_000,
                total: 100_000,
                truncated: true,
                ..view("q")
            },
            view("zzz"),
            FindBarView {
                truncated: true,
                ..view("zzz")
            },
            FindBarView {
                is_regex: true,
                regex_error: true,
                ..view("(")
            },
            FindBarView {
                case_sensitive: true,
                is_regex: true,
                ..view("q")
            },
        ] {
            assert_eq!(
                paint(&v, cols).field_cols,
                empty,
                "cols={cols}: the well moved with the status/toggle state"
            );
        }
    }
}

/// The caret is a reverse-video block that costs no cell, so the text never shifts.
#[test]
fn caret_is_a_block_that_does_not_move_the_text() {
    let cols = 90;
    let (text_at_end, caret_end) = well(&paint(&view("abc"), cols));
    let (text_at_start, caret_start) = well(&paint(
        &FindBarView {
            cursor: 0,
            ..view("abc")
        },
        cols,
    ));
    let (text_mid, caret_mid) = well(&paint(
        &FindBarView {
            cursor: 1,
            ..view("abc")
        },
        cols,
    ));
    assert_eq!(text_at_end, text_at_start, "the text never reflows");
    assert_eq!(text_at_end, text_mid);
    assert!(text_at_end.starts_with("abc"), "{text_at_end:?}");
    let field_start = paint(&view("abc"), cols).field_cols.start;
    assert_eq!(caret_start - field_start, 0, "caret on `a`");
    assert_eq!(caret_mid - field_start, 1, "caret on `b`");
    assert_eq!(caret_end - field_start, 3, "caret past the last character");
    let c = chrome_band::band_colors(Theme::default());
    let p = paint(
        &FindBarView {
            cursor: 1,
            ..view("abc")
        },
        cols,
    );
    let cell = &p.rows[p.field_row][caret_mid];
    assert_eq!(
        (cell.ch, cell.bg, cell.fg),
        ('b', caret_ink(&c), c.field_bg)
    );
}

/// An overlong query scrolls to keep the caret visible, with the clipped edges marked.
#[test]
fn overlong_query_scrolls_to_the_caret() {
    let cols = 90;
    let long: String = ('a'..='z').cycle().take(400).collect();
    let end = paint(&view(&long), cols);
    let width = end.field_cols.len();
    assert_eq!(
        end.field_scroll,
        long.chars().count() + 1 - width,
        "caret at the end pins the well to the tail"
    );
    let (shown, _) = well(&end);
    assert!(
        shown.starts_with(SCROLL_LEFT),
        "clipped head marked: {shown}"
    );

    let head = paint(
        &FindBarView {
            cursor: 0,
            ..view(&long)
        },
        cols,
    );
    assert_eq!(head.field_scroll, 0, "^A scrolls the well back to the head");
    let (shown, caret) = well(&head);
    assert_eq!(caret, head.field_cols.start);
    assert!(
        shown.ends_with(SCROLL_RIGHT),
        "clipped tail marked: {shown}"
    );

    let mid = paint(
        &FindBarView {
            cursor: 200,
            ..view(&long)
        },
        cols,
    );
    let (_, caret) = well(&mid);
    assert!(
        mid.field_cols.contains(&caret),
        "the caret is inside the well at every edit position"
    );
}

/// The clickable indicator spans line up EXACTLY with the painted `[Aa]` / `[.*]` cells (brackets included — a click on the bracket is not a dead pixel), or on Windows the padded ` Aa ` / ` .* ` chips, and the toolbar's buttons with their padded `↑` / `↓` / `✕`.
#[test]
fn indicator_cols_match_painted_cells() {
    let cols = 90;
    let p = paint(&view("q"), cols);
    let row = &p.rows[p.field_row];
    let case = p.case_cols.clone().expect("[Aa] drawn at this width");
    let regex = p.regex_cols.clone().expect("[.*] drawn at this width");
    let at = |span: Range<usize>| row[span].iter().map(|c| c.ch).collect::<String>();
    let (aa, re) = if FIND_TOOLBAR {
        (" Aa ", " .* ")
    } else {
        ("[Aa]", "[.*]")
    };
    assert_eq!(at(case), aa, "case span covers the painted toggle");
    assert_eq!(at(regex), re, "regex span covers the painted toggle");
    let buttons = [
        p.prev_cols.clone(),
        p.next_cols.clone(),
        p.close_cols.clone(),
    ];
    if FIND_TOOLBAR {
        for (span, glyph) in buttons.into_iter().zip(TOOLBAR_BUTTONS) {
            let span = span.expect("the buttons are drawn at this width");
            assert_eq!(at(span), format!(" {glyph} "), "a button covers its glyph");
        }
    } else {
        assert!(
            buttons.iter().all(Option::is_none),
            "no buttons off Windows"
        );
    }
}

/// NARROW WINDOWS KEEP THE ANSWER. When the row cannot hold everything, the ornament goes first: the toggles are dropped so the match/no-match readout survives — a search that found nothing must never look like one that found seven.
#[test]
fn the_status_outranks_the_toggles_when_space_runs_out() {
    let (narrow_cols, min_field) = if FIND_TOOLBAR {
        (36, TOOLBAR_MIN_FIELD)
    } else {
        (52, MIN_FIELD)
    };
    let narrow = paint(&view("zzz"), narrow_cols);
    assert!(
        narrow.case_cols.is_none() && narrow.regex_cols.is_none(),
        "the toggles are dropped first"
    );
    let s = all(&narrow);
    assert!(
        s.contains("no matches") || s.contains("0 hits"),
        "the answer survives, in whatever form fits: {s}"
    );
    assert!(
        narrow.field_cols.len() >= min_field,
        "and so does the field"
    );
    let roomy = paint(&view("zzz"), 90);
    assert!(roomy.case_cols.is_some());
    assert!(all(&roomy).contains("no matches"));
    let tiny = paint(&view("q"), 24);
    assert!(!tiny.field_cols.is_empty());
}

/// A SPLIT PANE KEEPS THE TOGGLES on Windows ([`FIND_TOOLBAR`]): at the 55 columns half of a 111-column window gives a pane, and wider, the toolbar paints the bracketed toggles beside the count and the buttons. The control: the right-edge layout drops them at 55 columns.
#[test]
fn the_toolbar_keeps_its_toggles_in_a_split_pane() {
    let v = FindBarView {
        idx: 2,
        total: 39,
        ..view("Merge")
    };
    let c = chrome_band::band_colors(Theme::default());
    let well_start = LEFT_PAD + PROMPT.chars().count();
    assert!(
        field_layout(&v, &c, 55, well_start).indicators.is_none(),
        "the right-edge layout drops the toggles at 55 columns"
    );
    if !FIND_TOOLBAR {
        return;
    }
    for cols in [55usize, 60, 80, 111, 200] {
        let p = paint(&v, cols);
        let case = p.case_cols.clone().expect("the toggles are kept");
        let regex = p.regex_cols.clone().expect("the toggles are kept");
        let prev = p.prev_cols.clone().expect("the buttons fit");
        let next = p.next_cols.clone().expect("the buttons fit");
        let close = p.close_cols.clone().expect("the buttons fit");
        assert!(p.field_cols.len() >= TOOLBAR_MIN_FIELD, "{cols}");
        assert!(p.field_cols.len() <= TOOLBAR_MAX_FIELD, "{cols}");
        let row: Vec<char> = p.rows[p.field_row].iter().map(|cell| cell.ch).collect();
        let count = row
            .windows(4)
            .position(|w| w == ['2', '/', '3', '9'])
            .expect("the count is painted");
        assert_eq!(
            p.field_cols.end + 1,
            case.start,
            "{cols}: the toggles follow the well"
        );
        assert_eq!(case.end + 1, regex.start, "{cols}: a cell apart");
        assert!(regex.end < count, "{cols}: then the count");
        assert_eq!(
            count + 5,
            prev.start,
            "{cols}: the count sits by the arrows"
        );
        assert_eq!(prev.end, next.start, "{cols}");
        assert_eq!(next.end, close.start, "{cols}");
        assert!(
            close.end <= cols - RIGHT_PAD,
            "{cols}: inside the right margin"
        );
    }
    let p = paint(&v, 45);
    assert!(
        p.close_cols.is_none() && p.case_cols.is_some(),
        "the buttons go first"
    );
}

/// THE TOOLBAR ENDS ON ITS CLOSE BUTTON at every width that draws one ([`FIND_TOOLBAR`]): the count and `↑ ↓ ✕` stand against the right margin, `✕` last, the count by the arrows, while the well and its toggles keep the left; a wide pane's surplus is band between the two groups, and the well stops at [`TOOLBAR_MAX_FIELD`].
#[test]
fn the_toolbar_ends_on_its_close_button_at_every_width() {
    if !FIND_TOOLBAR {
        return;
    }
    let v = FindBarView {
        idx: 2,
        total: 39,
        ..view("Merge")
    };
    let mut drew_buttons = 0;
    for cols in 34..=240usize {
        let p = paint(&v, cols);
        let row: String = p.rows[p.field_row].iter().map(|cell| cell.ch).collect();
        let count_end = row
            .find("2/39")
            .map(|at| row[..at].chars().count() + 4)
            .expect("the count outranks every control");
        assert!(p.field_cols.len() <= TOOLBAR_MAX_FIELD, "{cols}");
        match (p.prev_cols.clone(), p.close_cols.clone()) {
            (Some(prev), Some(close)) => {
                drew_buttons += 1;
                assert_eq!(close.end, cols - RIGHT_PAD, "{cols}: `✕` ends the row");
                assert_eq!(count_end + 1, prev.start, "{cols}: the count by `↑`");
                let regex = p
                    .regex_cols
                    .clone()
                    .expect("the toggles outlast the buttons");
                assert!(regex.end < count_end - 4, "{cols}: two groups, disjoint");
            }
            (None, None) => {
                assert_eq!(
                    count_end,
                    cols - RIGHT_PAD,
                    "{cols}: the count at the margin"
                );
            }
            spans => panic!("{cols}: the buttons go together: {spans:?}"),
        }
        if let Some(case) = p.case_cols.clone() {
            assert_eq!(
                case.start,
                p.field_cols.end + 1,
                "{cols}: toggles by the well"
            );
        }
    }
    assert!(
        drew_buttons > 150,
        "the sweep must judge the buttons: {drew_buttons}"
    );
    let wide = paint(&v, 111);
    assert_eq!(wide.field_cols.len(), TOOLBAR_MAX_FIELD);
    let gap = wide.prev_cols.expect("buttons").start - wide.regex_cols.expect("toggles").end;
    assert!(gap > 20, "the band between the groups: {gap} cells");
}

/// AN OFF TOGGLE IS STILL A BUTTON ([`FIND_TOOLBAR`]): its chip wears the band's resting control fill ([`toggle_rest_fill`]), at least [`TOGGLE_REST_STEP`]:1 off the band, with its glyphs in the band's words at WCAG AA on it, in every built-in theme, light or dark; an ON toggle wears the accent ([`toggle_on_colors`]).
#[test]
fn an_off_toggle_rests_on_a_control_fill_in_every_builtin_theme() {
    use aterm_messages::ink::contrast;
    if !FIND_TOOLBAR {
        return;
    }
    let light = chrome_band::band_colors(Theme::from(
        &aterm_types::scheme::builtin("Default Light").expect("builtin"),
    ));
    assert!(
        contrast(light.chip_ground, light.bar_bg) < TOGGLE_REST_STEP,
        "CONTROL: Default Light's resting chip is under the step"
    );
    let dark = chrome_band::band_colors(Theme::default());
    assert_eq!(
        toggle_rest_fill(&dark),
        dark.chip_ground,
        "Default keeps its chip"
    );
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
        let theme = Theme::from(&scheme);
        let c = chrome_band::band_colors(theme);
        let fill = toggle_rest_fill(&c);
        assert!(
            contrast(fill, c.bar_bg) >= TOGGLE_REST_STEP,
            "{name}: the off chip {fill:?} stands off the band {:?}",
            c.bar_bg
        );
        assert!(
            contrast(fill, c.bar_bg) < TOGGLE_REST_STEP + 0.1 || fill == c.chip_ground,
            "{name}: stepped no further than the floor"
        );
        for (case_sensitive, is_regex) in [(false, false), (true, false), (false, true)] {
            let v = FindBarView {
                case_sensitive,
                is_regex,
                ..view("q")
            };
            let p = find_bar_paint(&v, 111, FIND_BAR_ROWS, theme, FindSeam::Top, true);
            let row = &p.rows[p.field_row];
            for (span, on) in [
                (p.case_cols.clone().expect("drawn"), case_sensitive),
                (p.regex_cols.clone().expect("drawn"), is_regex),
            ] {
                let gap = row[span.end].bg;
                assert_eq!(
                    gap, c.bar_bg,
                    "{name}: the band between and after the chips"
                );
                for cell in &row[span] {
                    if on {
                        assert_eq!(cell.bg, c.accent, "{name}: ON is the accent");
                        assert!(contrast(cell.fg, cell.bg) >= 4.5, "{name}: ON ink");
                    } else {
                        assert_eq!(cell.bg, fill, "{name}: OFF rests on a fill");
                        assert_ne!(cell.bg, c.bar_bg, "{name}: a step off the band");
                        assert!(
                            contrast(cell.fg, cell.bg) >= 4.5,
                            "{name}: {:?} on {:?}",
                            cell.fg,
                            cell.bg
                        );
                    }
                }
            }
        }
    }
}

/// THE CARET WEARS THE FOCUS LINE'S ACCENT on Windows ([`caret_ink`]): with an authored blue accent the caret is that blue, floored to AA in the well, as the well's focus line is the accent.
#[test]
fn the_caret_wears_the_accent_the_focus_line_does() {
    use aterm_messages::ink::contrast;
    let blue = [0x4C, 0xC2, 0xFF];
    let palette = chrome_band::ChromePalette {
        theme: Theme::default(),
        ansi: None,
        authored: aterm_messages::tokens::AuthoredChrome {
            accent: Some(blue),
            ..aterm_messages::tokens::AuthoredChrome::NONE
        },
    };
    let c = chrome_band::band_colors(palette);
    let p = find_bar_paint(&view("ab"), 90, FIND_BAR_ROWS, palette, FindSeam::Top, true);
    let caret = p.rows[p.field_row][p.field_cols.start + 2].bg;
    assert_eq!(caret, caret_ink(&c));
    assert!(contrast(caret, c.field_bg) >= WELL_INK);
    if FIND_TOOLBAR {
        assert_eq!(caret, c.accent, "the accent clears AA in the well as is");
        assert_ne!(caret, c.caret, "not the cursor's colour");
        assert_eq!(p.well_line, Some(c.accent), "one mark with the focus line");
    } else {
        assert_eq!(caret, c.caret);
    }
}

/// A failed search tints the query itself.
#[test]
fn a_failed_search_colours_the_query_itself() {
    let cols = 90;
    let c = chrome_band::band_colors(Theme::default());
    let miss = paint(&view("zzz"), cols);
    let row = &miss.rows[miss.field_row];
    assert_eq!(row[miss.field_cols.start].fg, c.warn, "no match ⇒ warn ink");
    let hit = paint(
        &FindBarView {
            idx: 1,
            total: 2,
            ..view("zzz")
        },
        cols,
    );
    let row = &hit.rows[hit.field_row];
    assert_eq!(row[hit.field_cols.start].fg, c.value, "matched ⇒ value ink");
    let bad = paint(
        &FindBarView {
            is_regex: true,
            regex_error: true,
            ..view("(")
        },
        cols,
    );
    let row = &bad.rows[bad.field_row];
    assert_eq!(row[bad.field_cols.start].fg, c.warn, "bad regex ⇒ warn ink");
}

/// The seam rides the content-facing edge and runs edge to edge. The query well's own edge
/// ([`well_rule`], the well's cells) is the well's, not a seam.
#[test]
fn seam_faces_the_content_and_is_unbroken() {
    use aterm_core::terminal::UnderlineStyle;
    let cols = 90;
    let off_the_well = |p: &FindBarPaint| -> Vec<RenderCell> {
        p.rows
            .iter()
            .enumerate()
            .flat_map(|(r, row)| {
                row.iter()
                    .enumerate()
                    .filter(move |(col, _)| r != p.field_row || !p.field_cols.contains(col))
                    .map(|(_, cell)| *cell)
            })
            .collect()
    };
    let bottom = find_bar_paint(
        &view("q"),
        cols,
        FIND_BAR_ROWS,
        Theme::default(),
        FindSeam::Top,
        true,
    );
    assert!(
        bottom.rows[0].iter().all(|c| c.overline),
        "bottom panel → an unbroken overline on its top edge"
    );
    assert!(
        off_the_well(&bottom)
            .iter()
            .all(|c| c.underline == UnderlineStyle::None),
        "bottom panel → no underline"
    );
    let top = find_bar_paint(
        &view("q"),
        cols,
        FIND_BAR_ROWS,
        Theme::default(),
        FindSeam::BottomUnderline,
        true,
    );
    assert!(
        top.rows[FIND_BAR_ROWS - 1]
            .iter()
            .all(|c| c.underline == UnderlineStyle::Single),
        "top panel on a short cell → an unbroken underline on its bottom edge"
    );
    assert!(
        top.rows.iter().flatten().all(|c| !c.overline),
        "top panel → no overline"
    );
    let railed = find_bar_paint(
        &view("q"),
        cols,
        FIND_BAR_ROWS,
        Theme::default(),
        FindSeam::BottomRail,
        true,
    );
    assert!(
        railed.rows.iter().flatten().all(|c| !c.overline)
            && off_the_well(&railed)
                .iter()
                .all(|c| c.underline == UnderlineStyle::None),
        "top panel with a rail → no cell rule at all: the rail is the splice's"
    );
    assert_eq!(
        railed.seam,
        chrome_band::band_colors(Theme::default()).seam,
        "the rail's tone is the band's seam"
    );
    let band = chrome_band::band_colors(Theme::default());
    for height in [1_usize, 2] {
        let short = find_bar_paint(
            &view("nomatch"),
            cols,
            height,
            Theme::default(),
            FindSeam::Top,
            true,
        );
        let seam = &short.rows[0];
        assert!(
            seam.iter()
                .all(|c| c.overline && c.overline_color == Some(band.label)),
            "{height}-row panel: the seam is one tone across the field row, breaks/tones at {:?}",
            seam.iter()
                .enumerate()
                .filter(|(_, c)| !c.overline || c.overline_color != Some(band.label))
                .map(|(col, c)| (col, c.overline, c.overline_color))
                .collect::<Vec<_>>()
        );
        let inks: std::collections::BTreeSet<[u8; 3]> = seam.iter().map(|c| c.fg).collect();
        assert!(
            inks.len() > 1,
            "the field row is meant to carry several inks, so this proof is not vacuous: {inks:?}"
        );
    }
}

/// THE CURRENT MATCH IS UNMISTAKABLE in every built-in theme: its fill stands [`CURRENT_RISE`]:1 off the background and [`CURRENT_STEP`]:1 off the tint every other match wears — a step in luminance.
#[test]
fn the_current_match_stands_apart_from_the_others_in_every_builtin_theme() {
    use crate::settings::u32_rgb;
    use aterm_messages::ink::contrast;
    let old = Theme::default();
    let others = chrome_band::ChromePalette::from(old)
        .tokens()
        .search_match(u32_rgb(old.selection));
    assert!(
        contrast(u32_rgb(old.selection), others) < CURRENT_STEP,
        "the selection-coloured current match was indistinct from the others"
    );
    let names = aterm_types::scheme::builtin_names();
    assert!(names.len() > 4, "the sweep must actually sweep");
    for name in names {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
        let theme = Theme::from(&scheme);
        let tokens = chrome_band::ChromePalette::from(theme).tokens();
        let others = tokens.search_match(u32_rgb(theme.selection));
        let fill = current_match_fill(&tokens, others);
        assert!(
            contrast(fill, tokens.bg) >= CURRENT_RISE,
            "{name}: the current match {fill:?} must stand off the background {:?}",
            tokens.bg
        );
        assert!(
            contrast(fill, others) >= CURRENT_STEP,
            "{name}: the current match {fill:?} must stand off the other matches {others:?}"
        );
    }
}

/// THE CURRENT MATCH'S TEXT READS AT FULL STRENGTH: its ink is the background the fill stands off, walked toward its pole for AAA, in every built-in theme: at least AA always, AAA wherever a pole reaches it, the default theme's own background on its green and white on Default Light's dark green.
#[test]
fn the_current_match_text_wears_the_pole_its_fill_stands_off() {
    use crate::settings::u32_rgb;
    use aterm_messages::ink::contrast;
    let fill_of = |theme: Theme| {
        let tokens = chrome_band::ChromePalette::from(theme).tokens();
        let fill = current_match_fill(&tokens, tokens.search_match(u32_rgb(theme.selection)));
        (tokens, fill)
    };
    let theme = Theme::default();
    let (tokens, fill) = fill_of(theme);
    let floored = u32_rgb(aterm_render::floor_selection_fg(
        theme.fg,
        aterm_render::rgb_to_u32(fill),
    ));
    assert!(
        contrast(floored, fill) < 6.0,
        "the old floor's grey: {floored:?} on {fill:?}"
    );
    assert_eq!(current_match_ink(&tokens, fill), tokens.bg);
    let light = aterm_types::scheme::builtin("Default Light").expect("a built-in");
    let (tokens, fill) = fill_of(Theme::from(&light));
    let ink = current_match_ink(&tokens, fill);
    assert!(
        contrast(ink, fill) > contrast(tokens.bg, fill),
        "past the page's own ink"
    );
    assert_eq!(ink, [0xFF; 3], "white, the most a light side reaches");
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
        let (tokens, fill) = fill_of(Theme::from(&scheme));
        let ink = current_match_ink(&tokens, fill);
        let reach = contrast([0; 3], fill).max(contrast([0xFF; 3], fill));
        assert!(
            contrast(ink, fill) >= 4.5,
            "{name}: {ink:?} on the current match {fill:?}, AA at least"
        );
        if reach >= CURRENT_INK {
            assert!(
                contrast(ink, fill) >= CURRENT_INK,
                "{name}: {ink:?} on {fill:?}, AAA where a pole reaches it"
            );
        }
    }
}

/// EVERY FIELD SHOWS A 3:1 BOUNDARY, ONE RULE ([`well_edge`], audit r20 ED5): on Windows the
/// query well closes on the accent focus line, which the splice lays as a rail; where the bar
/// cannot draw a rail (a cell under the rail floor, a face without the room, a pane's band
/// that cannot lift its words, a one-row band whose seam rail owns the row's foot) the well
/// drew neither the line nor an edge. There the well's cells carry the focus line as their
/// underline; off Windows a faint well carries its border.
#[test]
fn every_find_well_shows_its_edge_whether_or_not_a_rail_can_be_drawn() {
    use aterm_core::terminal::UnderlineStyle;
    let light = Theme::from(&aterm_types::scheme::builtin("GitHub Light").expect("built-in"));
    for theme in [Theme::default(), light] {
        let c = chrome_band::band_colors(theme);
        assert!(
            aterm_messages::ink::contrast(c.accent, c.bar_bg) >= 3.0,
            "{theme:?}: the focus line stands 3:1 off the band"
        );
        assert_eq!(
            well_edge(&c, true, true),
            (Some(c.accent), None),
            "{theme:?}: the rail draws the focus line"
        );
        assert_eq!(
            well_edge(&c, true, false),
            (Some(c.accent), Some(c.accent)),
            "{theme:?}: without a rail the well's cells underline the focus line"
        );
        assert_eq!(
            well_edge(&c, false, false),
            (None, c.well_rule),
            "{theme:?}: off Windows, the faint well's border"
        );
        assert!(c.well_rule.is_some(), "{theme:?}: premise: a faint well");

        for well_rail in [true, false] {
            let p = find_bar_paint(
                &view("abc"),
                90,
                FIND_BAR_ROWS,
                theme,
                FindSeam::Top,
                well_rail,
            );
            let well = &p.rows[p.field_row][p.field_cols.clone()];
            let (line, underline) = well_edge(&c, WELL_ACCENT_LINE, well_rail);
            assert_eq!(p.well_line, line, "{theme:?} rail={well_rail}");
            assert!(
                well.iter().all(|cell| match underline {
                    Some(ink) =>
                        cell.underline == UnderlineStyle::Single
                            && cell.underline_color == Some(ink),
                    None => cell.underline == UnderlineStyle::None,
                }),
                "{theme:?} rail={well_rail}: the well's cells carry {underline:?}"
            );
        }
    }
}

/// A ONE-ROW FIND BAND KEEPS ITS WELL'S EDGE UNDER ITS SEAM: the band's foot is the well's
/// row there, and the seam's underline, laid over the whole row, painted the well's edge (its
/// 3:1 border, or Windows' focus line where no rail draws it) in the seam's grey. The seam
/// runs across the band's foot but the well's cells, which keep their edge.
#[test]
fn a_one_row_find_band_keeps_the_well_edge_under_its_seam() {
    use aterm_core::terminal::UnderlineStyle;
    let light = Theme::from(&aterm_types::scheme::builtin("GitHub Light").expect("built-in"));
    for theme in [Theme::default(), light] {
        let c = chrome_band::band_colors(theme);
        let one = find_bar_paint(&view("abc"), 90, 1, theme, FindSeam::BottomUnderline, false);
        let row = &one.rows[0];
        let (_, underline) = well_edge(&c, WELL_ACCENT_LINE, false);
        assert!(
            underline.is_some(),
            "{theme:?}: premise: the well carries its edge"
        );
        for (col, cell) in row.iter().enumerate() {
            let want = if one.field_cols.contains(&col) {
                underline
            } else {
                Some(c.seam)
            };
            assert_eq!(
                (cell.underline, cell.underline_color),
                (UnderlineStyle::Single, want),
                "{theme:?}: column {col} of a one-row band"
            );
        }
    }
}

/// THE QUERY WELL KEEPS A BOUNDARY: a theme's well stands only a step off the band (Default 1.4:1, GitHub Light's white well on its grey band 1.2:1), too faint to show where the field is, so off Windows an unbroken border is stamped across the whole well, as on every control's 3:1 edge, and on Windows the accent focus line marks its foot instead; under every stock Windows High-Contrast scheme the well's fill matches the band, and the border is stamped there too.
#[test]
fn the_query_well_keeps_a_boundary_under_a_forced_palette() {
    use aterm_core::terminal::UnderlineStyle;
    let cols = 90;
    let light = Theme::from(&aterm_types::scheme::builtin("GitHub Light").expect("built-in"));
    for theme in [Theme::default(), light] {
        let band = chrome_band::band_colors(theme);
        assert!(
            aterm_messages::ink::contrast(band.field_bg, band.bar_bg) < 3.0,
            "{theme:?}: the fixture is only interesting because the well is faint"
        );
        let plain = find_bar_paint(
            &view("abc"),
            cols,
            FIND_BAR_ROWS,
            theme,
            FindSeam::Top,
            true,
        );
        let well = &plain.rows[plain.field_row][plain.field_cols.clone()];
        if WELL_ACCENT_LINE {
            assert!(
                well.iter().all(|c| c.underline == UnderlineStyle::None),
                "{theme:?}: the focus line is the well's edge on Windows"
            );
            assert_eq!(plain.well_line, Some(band.accent), "{theme:?}");
        } else {
            assert!(
                well.iter().all(|c| c.underline == UnderlineStyle::Single
                    && c.underline_color == band.well_rule
                    && band.well_rule.is_some()),
                "{theme:?}: a faint well carries an unbroken border, caret cell included"
            );
            assert_eq!(
                plain.well_line, None,
                "{theme:?}: no focus line off Windows"
            );
        }
    }

    for (name, palette) in chrome_band::hc_fixtures::STOCK {
        chrome_band::hc_fixtures::with_forced(palette, || {
            let c = chrome_band::band_colors(Theme::default());
            assert_eq!(
                c.field_bg, c.bar_bg,
                "{name}: the fixture is only interesting because the tones collapse"
            );
            let p = paint(&view("abc"), cols);
            let well = &p.rows[p.field_row][p.field_cols.clone()];
            assert!(
                well.iter()
                    .all(|cell| cell.underline == UnderlineStyle::Single
                        && cell.underline_color == c.well_rule),
                "{name}: the well must carry an unbroken border, caret cell included"
            );
            assert_eq!(
                p.rows[p.field_row][p.field_cols.start - 1].underline,
                UnderlineStyle::None,
                "{name}: the border marks the field, not the whole row"
            );
            assert_eq!(p.well_line, None, "{name}: the border, not a focus line");
        });
    }
}

/// An ACTIVE toggle changes colour at the SAME width, so its state is visible across the window and nothing reflows when it flips: reversed (ink and ground swap) off Windows, the accent on Windows ([`toggle_on_colors`]).
#[test]
fn active_toggle_flips_at_a_fixed_width() {
    let cols = 90;
    let c = chrome_band::band_colors(Theme::default());
    let cell_at = |p: &FindBarPaint, span: Range<usize>| {
        let row = &p.rows[p.field_row];
        (row[span.start + 1].fg, row[span.start + 1].bg)
    };
    let off = paint(&view("q"), cols);
    let on = paint(
        &FindBarView {
            case_sensitive: true,
            ..view("q")
        },
        cols,
    );
    assert_eq!(
        off.case_cols, on.case_cols,
        "the toggle's geometry never changes with its state"
    );
    let inactive = if FIND_TOOLBAR {
        (
            chrome_band::ensure_contrast(c.value, toggle_rest_fill(&c), TOGGLE_INK),
            toggle_rest_fill(&c),
        )
    } else {
        (c.label, c.bar_bg)
    };
    assert_eq!(
        cell_at(&off, off.case_cols.clone().unwrap()),
        inactive,
        "inactive = dim on the band, or on its resting fill"
    );
    let active = if FIND_TOOLBAR {
        (
            chrome_band::ensure_contrast(c.on_accent, c.accent, TOGGLE_INK),
            c.accent,
        )
    } else {
        (c.bar_bg, c.value)
    };
    assert_eq!(
        cell_at(&on, on.case_cols.clone().unwrap()),
        active,
        "active = the accent on Windows, reversed elsewhere"
    );
}

/// While a rewrap holds the history away, a miss never reads `no matches` at any width and the query keeps its ordinary ink; below the narrowest status zone the bar says nothing.
#[test]
fn a_search_while_the_history_is_away_never_says_no_matches() {
    let c = chrome_band::band_colors(Theme::default());
    let away = |stale: bool| FindBarView {
        truncated: true,
        history_away: true,
        stale,
        ..view("needle")
    };
    let field_row = |v: &FindBarView, cols: usize| {
        let p = paint(v, cols);
        let row = text(&p.rows[p.field_row]);
        let ink = p.rows[p.field_row][p.field_cols.start].fg;
        (row, ink)
    };
    for cols in 40..=220 {
        for stale in [false, true] {
            let (row, ink) = field_row(&away(stale), cols);
            assert!(
                !row.contains("no matches") && !row.contains("0 hits"),
                "{cols}: {row}"
            );
            assert_ne!(ink, c.warn, "{cols}: the query is not a failure");
        }
    }
    let (row, ink) = field_row(&view("needle"), 80);
    assert!(row.contains("no matches"), "{row}");
    assert_eq!(ink, c.warn);
    let (row, _) = field_row(
        &FindBarView {
            idx: 1,
            total: 3,
            ..away(false)
        },
        60,
    );
    assert!(row.contains("1/3+"), "{row}");
}

/// A count is painted bare only when it is an exact census: output since marks it `…`, a floor `+`, both `+…`, and no width sheds a marker to fit.
#[test]
fn a_count_the_screen_has_moved_past_is_never_painted_bare() {
    let status_row = |v: &FindBarView, cols: usize| {
        let p = paint(v, cols);
        text(&p.rows[p.field_row])
    };
    let counted = FindBarView {
        idx: 1,
        total: 6,
        ..view("needle")
    };
    let bare = status_row(&counted, 90);
    assert!(bare.contains("1/6"), "{bare}");
    assert!(
        !bare.contains('…') && !bare.contains('+'),
        "an exact census is stated plainly: {bare}"
    );

    let moved = FindBarView {
        idx: 1,
        total: 6,
        stale: true,
        ..view("needle")
    };
    let mut widths_checked = 0;
    for cols in [40usize, 50, 60, 72, 90, 140, 200] {
        if !status_row(&counted, cols).contains("1/6") {
            continue;
        }
        widths_checked += 1;
        let painted = status_row(&moved, cols);
        assert!(
            painted.contains('…') || painted.contains("(stale)"),
            "cols={cols} dropped the staleness marker: {painted}"
        );
    }
    assert!(widths_checked >= 4, "the width sweep must actually sweep");
    let both = FindBarView {
        truncated: true,
        stale: true,
        ..FindBarView {
            idx: 1,
            total: 6,
            ..view("needle")
        }
    };
    let painted = status_row(&both, 90);
    assert!(painted.contains("1/6+…"), "{painted}");

    let none = FindBarView {
        idx: 1,
        total: 0,
        stale: true,
        ..view("needle")
    };
    let narrow = status_row(&none, 60);
    assert!(narrow.contains('…'), "{narrow}");
}

/// Every width × height (zero-width, degraded 1/2-row, overlong query) stays exactly `cols` wide and never panics.
#[test]
fn narrow_short_and_overlong_never_panic() {
    for cols in [0usize, 1, 4, 8, 12, 20, 40, 200] {
        for height in [1usize, 2, 3] {
            for (cursor, seam) in [
                (0usize, FindSeam::Top),
                (7, FindSeam::BottomRail),
                (200, FindSeam::BottomUnderline),
            ] {
                let p = find_bar_paint(
                    &FindBarView {
                        cursor,
                        idx: 5,
                        total: 9,
                        ..view(&"x".repeat(200))
                    },
                    cols,
                    height,
                    Theme::default(),
                    seam,
                    true,
                );
                assert_eq!(p.rows.len(), height, "cols={cols} height={height}");
                assert!(p.rows.iter().all(|r| r.len() == cols));
                assert!(p.field_row < height);
                assert!(p.field_cols.end <= cols.max(p.field_cols.start));
            }
        }
    }
    let one = find_bar_paint(&view("kept"), 90, 1, Theme::default(), FindSeam::Top, true);
    assert_eq!(one.field_row, 0);
    assert!(text(&one.rows[0]).contains("kept"));
}

/// A multi-byte query is painted and scrolled by character; a mid-codepoint cursor floors.
#[test]
fn multibyte_query_is_char_indexed() {
    let cols = 90;
    let p = paint(
        &FindBarView {
            cursor: "é".len(),
            ..view("éßπ")
        },
        cols,
    );
    let (shown, caret) = well(&p);
    assert!(shown.starts_with("éßπ"), "{shown}");
    assert_eq!(caret - p.field_cols.start, 1, "the caret is on ß");
    let p = paint(
        &FindBarView {
            cursor: 1,
            ..view("éßπ")
        },
        cols,
    );
    let (_, caret) = well(&p);
    assert_eq!(caret - p.field_cols.start, 0, "floored back onto é");
}

/// AN ON TOGGLE WEARS THE ACCENT ([`FIND_TOOLBAR`]), as Windows Terminal's search box checks one: the band's accent under its on-accent ink at WCAG AA, in every built-in theme, and under every stock High Contrast scheme `COLOR_HIGHLIGHT` under `COLOR_HIGHLIGHTTEXT`.
#[test]
fn an_on_toggle_wears_the_accent_with_its_on_accent_ink() {
    use aterm_messages::ink::contrast;
    if !FIND_TOOLBAR {
        return;
    }
    let on = |theme: Theme| {
        let v = FindBarView {
            case_sensitive: true,
            ..view("q")
        };
        let p = find_bar_paint(&v, 111, FIND_BAR_ROWS, theme, FindSeam::Top, true);
        let span = p.case_cols.clone().expect("drawn");
        let cell = &p.rows[p.field_row][span.start + 1];
        (cell.fg, cell.bg)
    };
    for name in aterm_types::scheme::builtin_names() {
        let theme = Theme::from(&aterm_types::scheme::builtin(name).expect("a built-in"));
        let c = chrome_band::band_colors(theme);
        let (fg, bg) = on(theme);
        assert_eq!(bg, c.accent, "{name}: the accent");
        assert!(contrast(fg, bg) >= TOGGLE_INK, "{name}: {fg:?} on {bg:?}");
        if name.starts_with("Default") {
            assert_ne!(bg, c.value, "{name}: CONTROL: not the reversed words");
        }
    }
    for (name, palette) in chrome_band::hc_fixtures::STOCK {
        chrome_band::hc_fixtures::with_forced(palette, || {
            assert_eq!(
                on(Theme::default()),
                (palette.highlight_text, palette.highlight),
                "{name}"
            );
        });
    }
}

/// Install this host's UI faces on the test thread; `false` where it has none (a fontless host has nothing to assert, as the strip band's tests).
fn with_ui_faces() -> bool {
    tray_raster::prepare_ui_fonts_for_direct_view_test();
    tray_raster::strip_band_ui_ready()
}

/// The 100 % and 200 % cell boxes of the Windows default face.
const X1: FluentGeom = FluentGeom {
    cell_w: 9,
    cell_h: 23,
    scale: 1.0,
};
const X2: FluentGeom = FluentGeom {
    cell_w: 18,
    cell_h: 47,
    scale: 2.0,
};

fn fluent(
    v: &FindBarView,
    g: FluentGeom,
    theme: Theme,
    preedit: Option<Range<usize>>,
) -> (FindBarPaint, Arc<FluentBar>) {
    let paint = find_bar_paint(v, 111, FIND_BAR_ROWS, theme, FindSeam::BottomRail, true);
    let bar = fluent_bar(&FluentInput {
        view: v,
        paint: &paint,
        colors: chrome_band::band_colors(theme),
        geom: g,
        seam: FindSeam::BottomRail,
        preedit,
    })
    .expect("the Windows bar draws its face");
    (paint, bar)
}

fn pixel(image: &ImageData, x: f32, y: f32) -> [u8; 3] {
    let ImageFormat::RawRgba8 { width, .. } = image.format else {
        panic!("a raw raster");
    };
    let i = (y as usize * usize::from(width) + x as usize) * 4;
    [image.bytes[i], image.bytes[i + 1], image.bytes[i + 2]]
}

/// The x extent of the pixels unlike `ground` in `xs` × `ys`.
fn ink_span(
    image: &ImageData,
    xs: Range<usize>,
    ys: Range<usize>,
    ground: [u8; 3],
) -> Option<Range<usize>> {
    let (mut lo, mut hi) = (usize::MAX, 0);
    for y in ys {
        for x in xs.clone() {
            if pixel(image, x as f32, y as f32) != ground {
                (lo, hi) = (lo.min(x), hi.max(x + 1));
            }
        }
    }
    (hi > lo).then_some(lo..hi)
}

/// THE WINDOWS BAR SETS ITS WORDS IN THE UI FACE ([`fluent_bar`]): `Find:`, the count and the hint row are drawn at the UI face's own widths, the field row at the tab title's size and the hints a step below, each band row one raster the panel's width over its cells, as the strip and the pane headers draw theirs.
#[test]
fn the_windows_bar_sets_its_words_in_the_ui_face() {
    if !FIND_TOOLBAR {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let c = chrome_band::band_colors(Theme::default());
    let v = FindBarView {
        idx: 2,
        total: 18,
        ..view("Merge")
    };
    let (p, bar) = fluent(&v, X1, Theme::default(), None);
    assert_eq!(bar.rows.len(), FIND_BAR_ROWS);
    for image in &bar.rows {
        assert_eq!(
            image.format,
            ImageFormat::RawRgba8 {
                width: 999,
                height: 23
            }
        );
        assert_eq!((image.cols, image.rows, image.z_index), (111, 1, 0));
    }
    let field = &bar.rows[p.field_row];
    let body = X1.body().get();
    let measure = |s: &str, px: f32| tray_raster::ui_text_width_for(TextFace::Ui, s, px);
    let well = well_box(&p.field_cols, X1);
    let label = ink_span(field, 0..well.x0 as usize, 0..23, c.bar_bg).expect("drawn");
    assert!(
        label.start.abs_diff(X1.x(LEFT_PAD) as usize) <= 2,
        "{label:?}"
    );
    let face = measure("Find:", body);
    let inked = label.len() as f32;
    assert!(
        inked <= face && inked >= face - 4.0,
        "{label:?}: inside the UI face's {face} px advance"
    );
    assert!(label.len() < 5 * 9 * 3 / 4, "CONTROL: under the cells' run");
    let status = p.status_cols.clone().expect("a count");
    assert_eq!(text(&p.rows[p.field_row][status.clone()]), "2/18");
    let count = ink_span(
        field,
        X1.x(status.start - 4) as usize..X1.x(status.end) as usize + 2,
        0..23,
        c.bar_bg,
    )
    .expect("drawn");
    assert!(
        count.end.abs_diff(X1.x(status.end) as usize) <= 2,
        "{count:?}"
    );
    let face = measure("2/18", body);
    // A run's partial pixels ink a pixel more than its advance, as the hints'.
    assert!(
        count.len() as f32 <= face + 1.0 && count.len() as f32 >= face - 4.0,
        "{count:?}: inside the UI face's {face} px advance"
    );
    let hints = &bar.rows[p.hint_row.expect("a hint row")];
    let small = X1.hint().get();
    assert!(small < body, "the hints a step below");
    let items: Vec<&str> = hint_text(111).split("  ").collect();
    let wide = items.iter().map(|item| measure(item, small)).sum::<f32>()
        + X1.epx(HINT_GAP_EPX) * (items.len() - 1) as f32;
    let drawn = ink_span(hints, 0..999, 0..22, c.bar_bg).expect("drawn");
    assert!(drawn.start.abs_diff(X1.x(LEFT_PAD) as usize) <= 2);
    let inked = drawn.len() as f32;
    assert!(
        inked <= wide + 1.0 && inked >= wide - 4.0,
        "{drawn:?}: {wide}"
    );
    assert!(
        (drawn.len() as f32) < hint_text(111).chars().count() as f32 * 9.0 * 0.75,
        "CONTROL: under the cells' run"
    );
    assert!(
        (0..999).all(|x| pixel(hints, x as f32, 22.0) == p.seam),
        "the seam on the band's last pixel row"
    );
}

/// THE QUERY BOX AND THE TOGGLES ARE FLUENT CONTROLS ([`fluent_bar`]): at 100 % and 200 % each stands [`CONTROL_INSET_EPX`] off the seam above the band and as far off the field row's foot, one height, its corners rounded [`CONTROL_RADIUS_EPX`]; the box closes on its accent focus line inside its corners.
#[test]
fn the_box_and_the_toggles_are_inset_rounded_controls() {
    if !FIND_TOOLBAR {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let c = chrome_band::band_colors(Theme::default());
    let rest = toggle_rest_fill(&c);
    let v = FindBarView {
        idx: 2,
        total: 18,
        ..view("Merge")
    };
    for g in [X1, X2] {
        let (p, bar) = fluent(&v, g, Theme::default(), None);
        let field = &bar.rows[p.field_row];
        let (y0, y1) = g.control_y();
        assert_eq!(y0, 3.0 * g.scale, "{g:?}: 3 epx off the seam");
        assert_eq!(g.cell_h as f32 - y1, y0, "{g:?}: as far off the foot");
        let r = g.epx(CONTROL_RADIUS_EPX);
        assert_eq!(r, 4.0 * g.scale, "{g:?}: 4 epx corners");
        let well = well_box(&p.field_cols, g);
        let bx = well.x1 - g.epx(BOX_PAD_EPX) - 2.0;
        assert_eq!(
            pixel(field, bx, y0 - 1.0),
            c.bar_bg,
            "{g:?}: band over the box"
        );
        assert_eq!(pixel(field, bx, y0), c.field_bg, "{g:?}: the box");
        assert_eq!(
            pixel(field, bx, y1 - 1.0),
            c.accent,
            "{g:?}: its focus line"
        );
        assert_eq!(pixel(field, bx, y1), c.bar_bg, "{g:?}: band under it");
        assert_ne!(
            pixel(field, well.x0, y0),
            c.field_bg,
            "{g:?}: a round corner"
        );
        assert_eq!(pixel(field, well.x0 + r, y0 + r), c.field_bg);
        assert_ne!(
            pixel(field, well.x0, y1 - 1.0),
            c.accent,
            "{g:?}: line in corner"
        );
        let mid = ((y0 + y1) / 2.0).floor();
        for span in [p.case_cols.clone(), p.regex_cols.clone()] {
            let span = span.expect("the toggles are drawn");
            let xa = g.x(span.start) + g.epx(CHIP_SIDE_EPX);
            assert_eq!(pixel(field, xa + r, y0 - 1.0), c.bar_bg, "{g:?}: band over");
            assert_eq!(pixel(field, xa + r, y0), rest, "{g:?}: the chip's top");
            assert_eq!(pixel(field, xa + r, y1 - 1.0), rest, "{g:?}: its foot");
            assert_eq!(pixel(field, xa + r, y1), c.bar_bg, "{g:?}: band under");
            assert_eq!(pixel(field, xa, mid), rest, "{g:?}: its side");
            assert_ne!(pixel(field, xa, y0), rest, "{g:?}: a round corner");
            assert_eq!(
                p.rows[p.field_row][span.start].bg, rest,
                "CONTROL: the cell under it fills the row"
            );
        }
    }
}

/// AN ON TOGGLE IS DRAWN IN THE ACCENT ([`fluent_bar`]): its chip is the band's accent with its word in the on-accent ink, dark or light, where an off chip keeps the resting fill.
#[test]
fn an_on_toggle_is_drawn_in_the_accent() {
    use aterm_messages::ink::contrast;
    if !FIND_TOOLBAR {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let light = Theme::from(&aterm_types::scheme::builtin("Default Light").expect("built-in"));
    for theme in [Theme::default(), light] {
        let c = chrome_band::band_colors(theme);
        let v = FindBarView {
            case_sensitive: true,
            idx: 1,
            total: 3,
            ..view("Merge")
        };
        let (p, bar) = fluent(&v, X1, theme, None);
        let field = &bar.rows[p.field_row];
        let (y0, y1) = X1.control_y();
        let mid = ((y0 + y1) / 2.0).floor();
        let (on_fill, on_ink) = toggle_on_colors(&c);
        let case = p.case_cols.clone().expect("drawn");
        let (xa, xb) = (X1.x(case.start) + 1.0, X1.x(case.end) - 1.0);
        assert_eq!(pixel(field, xa, mid), on_fill);
        assert_eq!(on_fill, c.accent);
        let word = (y0 as usize..y1 as usize)
            .flat_map(|y| (xa as usize..xb as usize).map(move |x| (x, y)))
            .map(|(x, y)| pixel(field, x as f32, y as f32))
            .max_by(|a, b| contrast(*a, on_fill).total_cmp(&contrast(*b, on_fill)))
            .expect("pixels");
        assert!(
            contrast(word, on_fill) >= 3.0,
            "{word:?} reads on the accent"
        );
        assert!(
            contrast(word, on_ink) < 1.6,
            "{word:?} is the on-accent ink"
        );
        let regex = p.regex_cols.clone().expect("drawn");
        assert_eq!(
            pixel(field, X1.x(regex.start) + 1.0, mid),
            toggle_rest_fill(&c),
            "CONTROL: the off chip rests"
        );
    }
}

/// THE QUERY BOX SCROLLS AND HIT-TESTS IN THE FACE IT DRAWS ([`well_scroll`], [`char_at`], [`well_click`]): the caret stands the query's measured width in; a query past the box scrolls by the least that keeps the caret in; a click lands on the nearest character boundary, anywhere in the box, the prompt's cells it took included.
#[test]
fn the_query_box_scrolls_and_hit_tests_in_its_own_face() {
    if !FIND_TOOLBAR {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let px = X1.body().get();
    let caret_w = X1.caret_w();
    let v = FindBarView {
        idx: 1,
        total: 3,
        ..view("Merge")
    };
    let (p, bar) = fluent(&v, X1, Theme::default(), None);
    let well = well_box(&p.field_cols, X1);
    let chars: Vec<char> = "Merge".chars().collect();
    assert_eq!(bar.scroll, 0);
    assert_eq!(bar.caret_x, (well.text_x0 + ui_width(&chars, px)).round());
    for i in 0..=chars.len() {
        let x = ui_width(&chars[..i], px);
        assert_eq!(char_at(&chars, 0, x, 400.0, px), i, "boundary {i}");
    }
    assert_eq!(
        char_at(&chars, 0, 1000.0, 400.0, px),
        chars.len(),
        "past: the end"
    );
    assert_eq!(char_at(&chars, 0, -3.0, 400.0, px), 0);
    let long: Vec<char> = "search the screen and the scrollback "
        .repeat(8)
        .chars()
        .collect();
    let room = 120.0;
    let s = well_scroll(&long, long.len(), room, caret_w, px);
    assert!(s > 0);
    assert!(
        ui_width(&long[s..], px) + caret_w <= room,
        "the caret is in"
    );
    assert!(
        ui_width(&long[s - 1..], px) + caret_w > room,
        "by the least scroll"
    );
    assert_eq!(
        well_scroll(&long, 0, room, caret_w, px),
        0,
        "Home: the head"
    );
    assert!(
        well.x0 < X1.x(p.field_cols.start),
        "it starts in the prompt's cells"
    );
    let at = |x: f32| well_click("Merge", 0, &p.field_cols, X1, x);
    assert_eq!(at(well.text_x0 + ui_width(&chars[..3], px)), Some(3));
    assert_eq!(at(well.x0 + 1.0), Some(0));
    assert_eq!(at(well.x0 - 1.0), None, "the label is not the box");
    assert_eq!(at(well.x1 + 1.0), None);
    let target = "target here";
    let x = X1.x(p.field_cols.start + 6) + 4.0;
    let by_face = well_click(target, 0, &p.field_cols, X1, x).expect("in the box");
    let tc: Vec<char> = target.chars().collect();
    let want = char_at(&tc, 0, x - well.text_x0, 400.0, px);
    assert_eq!(by_face, want);
    assert_ne!(by_face, 6, "CONTROL: one character per cell is elsewhere");
}

/// THE COMPOSITION IS UNDERLINED UNDER ITS OWN CHARACTERS IN THE BOX ([`fluent_bar`]), in the band's words, and nowhere else.
#[test]
fn the_composition_is_underlined_under_its_own_characters() {
    if !FIND_TOOLBAR {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let c = chrome_band::band_colors(Theme::default());
    let v = FindBarView {
        idx: 1,
        total: 3,
        ..view("abcd")
    };
    let (p, composed) = fluent(&v, X1, Theme::default(), Some(2..4));
    let (_, plain) = fluent(&v, X1, Theme::default(), None);
    let (a, b) = (&composed.rows[p.field_row], &plain.rows[p.field_row]);
    let changed: Vec<(usize, usize)> = (0..23)
        .flat_map(|y| (0..999).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(a, x as f32, y as f32) != pixel(b, x as f32, y as f32))
        .collect();
    assert!(!changed.is_empty(), "the composition is underlined");
    let well = well_box(&p.field_cols, X1);
    let chars: Vec<char> = "abcd".chars().collect();
    let px = X1.body().get();
    let from = (well.text_x0 + ui_width(&chars[..2], px)).round() as usize;
    let to = (well.text_x0 + ui_width(&chars, px)).round() as usize;
    for &(x, y) in &changed {
        assert!((from..to + 1).contains(&x), "x {x} outside {from}..{to}");
        assert_eq!(pixel(a, x as f32, y as f32), c.value);
    }
}

/// UNDER A FORCED PALETTE THE BOX KEEPS AN EDGE ([`fluent_bar`]): every stock High Contrast scheme sets the field and the band one colour, so the box carries a border in the window text and no accent focus line, and an on toggle is `COLOR_HIGHLIGHT`.
#[test]
fn the_box_keeps_an_edge_under_a_forced_palette() {
    use aterm_messages::ink::contrast;
    if !FIND_TOOLBAR {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    for (name, palette) in chrome_band::hc_fixtures::STOCK {
        chrome_band::hc_fixtures::with_forced(palette, || {
            let c = chrome_band::band_colors(Theme::default());
            let v = FindBarView {
                case_sensitive: true,
                idx: 1,
                total: 3,
                ..view("Merge")
            };
            let (p, bar) = fluent(&v, X1, Theme::default(), None);
            assert_eq!(p.well_line, None, "{name}");
            let field = &bar.rows[p.field_row];
            let (y0, y1) = X1.control_y();
            let well = well_box(&p.field_cols, X1);
            let bx = well.x1 - 12.0;
            let mid = ((y0 + y1) / 2.0).floor();
            assert_eq!(pixel(field, bx, mid), c.field_bg, "{name}: inside");
            for y in [y0, y1 - 1.0] {
                let edge = pixel(field, bx, y);
                assert!(
                    contrast(edge, c.field_bg) >= 3.0,
                    "{name}: the border at {y}"
                );
                assert_ne!(edge, palette.highlight, "{name}: no focus line");
            }
            let case = p.case_cols.clone().expect("drawn");
            assert_eq!(
                pixel(field, X1.x(case.start) + 1.0, mid),
                palette.highlight,
                "{name}"
            );
        });
    }
}

/// THE CELLS STAND where no UI face is installed, for a query no face draws (a colour emoji), and off Windows ([`fluent_ready`]).
#[test]
fn the_cells_stand_without_a_face_for_the_query() {
    let bar = |query: &str| {
        let v = view(query);
        let paint = find_bar_paint(
            &v,
            111,
            FIND_BAR_ROWS,
            Theme::default(),
            FindSeam::Top,
            true,
        );
        fluent_bar(&FluentInput {
            view: &v,
            paint: &paint,
            colors: chrome_band::band_colors(Theme::default()),
            geom: X1,
            seam: FindSeam::Top,
            preedit: None,
        })
        .is_some()
    };
    tray_raster::clear_ui_fonts_for_test();
    assert!(!bar("Merge"), "no UI face: the cells");
    if with_ui_faces() {
        assert_eq!(bar("Merge"), FIND_TOOLBAR, "the face on Windows only");
        assert!(!bar("Merge \u{1F642}"), "a colour emoji keeps the cells");
    }
}
