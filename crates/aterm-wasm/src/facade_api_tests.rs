// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;

fn assert_row_export_matches_cells(t: &WebTerminal, first: u32, count: u32, cols: u16) {
    let records = (first..first + count)
        .map(|row| {
            let text = t.row_text(row as u16).expect("visible row");
            let widths: String = (0..cols)
                .map(|col| {
                    if t.cell_is_wide(row as u16, col) == Some(true) {
                        '2'
                    } else {
                        '1'
                    }
                })
                .collect();
            let width_field = if widths.contains('2') {
                format!(",\"widths\":\"{widths}\"")
            } else {
                String::new()
            };
            format!(
                "{{\"text\":{},\"wrapped\":{},\"len\":{}{width_field}}}",
                json_string(&text),
                t.row_is_wrapped(row as u16).expect("visible row"),
                t.row_len(row as u16).expect("visible row")
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        t.row_range_json(first, count).as_deref(),
        Some(format!("[{records}]").as_str()),
        "first={first}, count={count}, cols={cols}"
    );
}

#[test]
fn osc_event_json_preserves_codes_order_escaping_and_drain_boundaries() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = WebTerminal::new(4, 8, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    assert!(t.take_osc_events().is_none());
    t.process("\x1b]7;file:///a\"\\漢\x07".as_bytes());
    t.process(b"\x1b]133;D;42\x07");
    t.process(b"\x1b]7;file://host/b\x07");
    assert_eq!(
        t.take_osc_events().as_deref(),
        Some(r#"[[7,"/a\"\\漢"],[133,"D;exit=42"],[7,"//host/b"]]"#)
    );
    assert!(t.take_osc_events().is_none());
    t.process(b"\x1b]7;file:///fresh\x07");
    assert_eq!(t.take_osc_events().as_deref(), Some(r#"[[7,"/fresh"]]"#));
    assert!(t.take_osc_events().is_none());
}

#[test]
fn row_range_json_matches_individual_exports_across_wide_cells_and_bounds() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    for input in ["plain\"\\", "漢x", "a漢b", "abcdef漢", "🙂漢", "abcdefghi"] {
        let mut t = WebTerminal::new(4, 8, font, 14.0, 0, 0, 0, 0).expect("bundled face");
        t.process(input.as_bytes());
        for (first, count) in [(0, 0), (0, 4), (1, 2), (4, 0)] {
            assert_row_export_matches_cells(&t, first, count, 8);
        }
        for (first, count) in [(4, 1), (u32::MAX, 1), (1, u32::MAX)] {
            assert!(t.row_range_json(first, count).is_none());
        }
    }
}

#[test]
fn row_export_occupied_prefix_keeps_wide_padding_erasure_and_history() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = WebTerminal::new(4, 160, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    for input in [
        "a漢b\r\nx🙂y\r\nshort\r\n",
        "\x1b[1;1H\x1b[2K",
        "\x1b[2;3H\x1b[K",
        "\x1b[1;150H漢",
        "\x1b[1;150Hn",
        "\x1b[44m\x1b[3;160H漢\x1b[0m",
        "\x1b[41m\x1b[2;1H\x1b[2K\x1b[0m",
    ] {
        t.process(input.as_bytes());
        assert_row_export_matches_cells(&t, 0, 4, 160);
    }
    for i in 0..12 {
        t.process(format!("\r\nL{i}漢").as_bytes());
    }
    t.scroll_lines(1000);
    assert!(t.term.grid().display_offset() > 0);
    assert!(!t.term.grid().visible_row_view(0).is_empty());
    assert_row_export_matches_cells(&t, 0, 4, 160);
}

#[test]
fn row_export_live_hint_is_conservative_across_screen_switches() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = WebTerminal::new(2, 8, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    t.process(b"plain");
    t.term
        .grid_mut()
        .row_mut(0)
        .expect("live row")
        .mark_has_wide_chars();
    assert_row_export_matches_cells(&t, 0, 2, 8);
    for input in ["\r漢abc", "\x1b[?1049hASCII", "\r🙂", "\x1b[?1049l"] {
        t.process(input.as_bytes());
        assert_row_export_matches_cells(&t, 0, 2, 8);
    }
}

#[test]
fn row_export_reflowed_history_uses_the_materialized_view() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = WebTerminal::new(4, 40, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    let scrollback = aterm_core::scrollback::Scrollback::new(64, 512, 64_000_000);
    t.term = Terminal::with_scrollback(4, 40, 0, scrollback);
    for i in 0..20 {
        t.process(format!("L{i}-漢-abcdefghij\r\n").as_bytes());
    }
    t.resize(4, 12).expect("admitted geometry");
    t.scroll_lines(1000);
    assert!(t.term.grid().display_offset() > 0);
    assert!(
        t.term.grid().row(0).is_none(),
        "history is outside the row ring"
    );
    assert!(!t.term.grid().visible_row_view(0).is_empty());
    assert_row_export_matches_cells(&t, 0, 4, 12);
}

#[test]
fn row_export_missing_grid_row_keeps_the_unknown_length_fallback() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = WebTerminal::new(2, 8, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    t.rows = 3;
    assert_eq!(
        t.row_range_json(2, 1).as_deref(),
        Some(r#"[{"text":"","wrapped":false,"len":8}]"#)
    );
    t.rows = 2;
}

#[test]
fn search_summary_preserves_rows_escaping_caps_and_search_modes() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = WebTerminal::new(4, 40, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    t.process("a a \\\" 漢a\r\nb b \\\\ 漢A\r\na b\r\n".as_bytes());
    for (query, sensitive, regex) in [
        ("a", true, false),
        ("a", false, false),
        ("漢", true, false),
        ("a|b", true, true),
        ("^", true, true),
        ("absent", true, false),
        ("(", true, true),
        ("", true, false),
    ] {
        for completed in [false, true] {
            t.search_index_release();
            if completed && !query.is_empty() && query != "(" {
                let step = t.search_budgeted(query, sensitive, regex, None, 100);
                assert!(step.complete(), "small fixture finishes in one step");
            }
            for cap in [0, 1, 2, u32::MAX] {
                let expected = match t.term.search_summary_results(query, sensitive, regex) {
                    Ok(results) if !query.is_empty() => {
                        let total = results.matches.len();
                        let limit = if cap == 0 {
                            total
                        } else {
                            (cap as usize).min(total)
                        };
                        let records = results.matches[..limit]
                            .iter()
                            .map(|m| {
                                let text = t.term.abs_row_text(m.line as u64).unwrap_or_default();
                                format!(
                                    "{{\"absRow\":{},\"col\":{},\"len\":{},\"snippet\":{}}}",
                                    m.line,
                                    m.start_col,
                                    m.len(),
                                    json_string(&text)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(",");
                        format!(
                            "{{\"matches\":[{records}],\"total\":{total},\"incomplete\":{}}}",
                            results.incomplete
                        )
                    }
                    _ => "{\"matches\":[],\"total\":0,\"incomplete\":false}".to_owned(),
                };
                assert_eq!(
                    t.search_summary(query, sensitive, regex, cap).as_deref(),
                    Some(expected.as_str()),
                    "query={query:?}, sensitive={sensitive}, regex={regex}, cap={cap}, completed={completed}"
                );
            }
        }
    }
}

/// A main-screen row cached at a key the alt screen comes to share is not served for the alt screen.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn display_row_cache_distinguishes_main_and_alt_screen() {
    let mut t = crate::system_font_terminal(5, 20, 16.0);
    for _ in 0..30 {
        t.process(b"\r\n");
    }
    t.process(b"\x1b[HM");
    let main_gen = t.term.grid().content_gen();
    assert_eq!(t.cell_text(0, 0), "M", "primed main cell at (0,0)");

    t.process(b"\x1b[?1049h");
    t.process(b"\x1b[HA");
    assert!(
        t.term.grid().content_gen() <= main_gen,
        "alt buffer starts below main_gen so we can drive it up to collide"
    );
    let mut guard = 0;
    while t.term.grid().content_gen() != main_gen && guard < 10_000 {
        let g: &[u8] = if guard % 2 == 0 {
            b"\x1b[2;1Hx"
        } else {
            b"\x1b[2;1Hy"
        };
        t.process(g);
        guard += 1;
    }
    assert_eq!(
        t.term.grid().content_gen(),
        main_gen,
        "manufactured the content_gen collision across the swap"
    );
    assert_eq!(
        t.cell_text(0, 0),
        "A",
        "alt-screen cell must not be served from the main-screen cache at a coinciding key"
    );
}

/// Extreme `i32` selection rows stay clipped to the viewport without wrapping.
#[test]
fn hostile_selection_rows_stay_offscreen_without_wrapping() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = WebTerminal::new(3, 8, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    for _ in 0..8 {
        t.process(b"line\r\n");
    }
    t.selection_start(i32::MAX, 0);
    t.selection_extend(i32::MAX, 7);
    t.scroll_lines(4);
    assert!(
        t.selection_range().is_none(),
        "a far-below row must stay below"
    );

    t.selection_start(i32::MIN, 0);
    t.selection_extend(i32::MAX, 7);
    t.selection_finish();
    let range = t.selection_range().expect("selection crosses the viewport");
    assert_eq!((range.start_y(), range.end_y()), (0, 2));
    assert_eq!((range.start_x(), range.end_x()), (0, 7));

    assert!(t.selection_line(i32::MIN, 0).is_none());
    assert!(t.selection_range().is_none());
    assert!(t.selection_word(i32::MIN, 0).is_none());
    assert!(t.selection_range().is_none());
}

#[test]
fn command_output_json_preserves_empty_text_escapes_and_exit_codes() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    for (exit_marker, exit_json) in [
        ("skip", "null"),
        ("", "0"),
        (";0", "0"),
        (";2147483647", "2147483647"),
        (";-2147483648", "-2147483648"),
    ] {
        for (text, encoded) in [("", ""), ("a\"b\\漢", r#"a\"b\\漢"#)] {
            let mut t = WebTerminal::new(8, 80, font, 14.0, 0, 0, 0, 0).expect("bundled face");
            t.process(b"\x1b]133;A\x07$ cmd\x1b]133;B\x07\r\n\x1b]133;C\x07");
            if !text.is_empty() {
                t.process(text.as_bytes());
                t.process(b"\r\n");
            }
            if exit_marker != "skip" {
                t.process(format!("\x1b]133;D{exit_marker}\x07").as_bytes());
            }
            t.process(b"\x1b]133;A\x07");
            assert_eq!(
                t.last_command_output().as_deref(),
                Some(
                    format!(r#"{{"status":"ok","text":"{encoded}","exitCode":{exit_json}}}"#)
                        .as_str()
                )
            );
        }
    }
}

#[test]
fn cell_width_metadata_matches_graphemes_through_live_history_and_alternate_rows() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = WebTerminal::new(4, 8, font, 14.0, 0, 0, 0, 0).unwrap();
    for input in [
        "a漢e\u{301}🙂",
        "\r\nnext\r\n漢\r\nlast\r\nmore",
        "\x1b[?1049h漢x",
        "\x1b[2J",
        "\x1b[?1049l",
    ] {
        t.process(input.as_bytes());
        for scroll in [0, 2, -2] {
            t.scroll_lines(scroll);
            for row in 0..=4u16 {
                let expected = t.term.display_row_grapheme_cells(usize::from(row));
                for col in 0..=8u16 {
                    let wide = expected
                        .as_ref()
                        .and_then(|cells| cells.get(usize::from(col)))
                        .map(|(_, w)| *w);
                    let text = expected
                        .as_ref()
                        .and_then(|cells| cells.get(usize::from(col)))
                        .map_or("", |(text, _)| text.as_str());
                    assert_eq!(
                        t.cell_text(row, col),
                        text,
                        "text row={row} col={col} scroll={scroll}"
                    );
                    assert_eq!(
                        t.cell_is_wide(row, col),
                        wide,
                        "row={row} col={col} scroll={scroll}"
                    );
                }
            }
        }
    }
}

#[test]
fn link_kind_matches_the_original_ascii_lowercase_classifier() {
    fn original(s: &str) -> u8 {
        let lower = s.to_ascii_lowercase();
        if ["http://", "https://", "ftp://", "file://", "www."]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
        {
            1
        } else if s.starts_with('/')
            || s.starts_with("./")
            || s.starts_with("../")
            || s.starts_with("~/")
            || (s.contains('/') && !s.contains("://"))
        {
            2
        } else {
            3
        }
    }
    for s in [
        "",
        "http",
        "HTTP://",
        "hTtPs://example.test/漢",
        "FTP://x",
        "FiLe:///tmp/x",
        "WwW.example.test",
        "/tmp/x",
        "./x",
        "../x",
        "~/x",
        "a/b",
        "custom://x",
        "漢🙂",
        "a漢b/c",
        "http漢/",
        "word",
    ] {
        assert_eq!(classify(s), original(s), "{s:?}");
    }
    for prefix in [
        "http://", "https://", "ftp://", "file://", "www.", "other://",
    ] {
        for mask in 0..(1usize << prefix.len()) {
            let case: String = prefix
                .bytes()
                .enumerate()
                .map(|(i, b)| {
                    char::from(if mask & (1 << i) == 0 {
                        b
                    } else {
                        b.to_ascii_uppercase()
                    })
                })
                .collect();
            for tail in ["", "x", "漢🙂/tail", "%20", "\n"] {
                let input = format!("{case}{tail}");
                assert_eq!(classify(&input), original(&input), "{input:?}");
            }
        }
    }
    for byte in 0u8..=127 {
        for tail in ["", "://", "/x", "漢🙂"] {
            let input = format!("{}{tail}", char::from(byte));
            assert_eq!(classify(&input), original(&input), "{input:?}");
        }
    }
}
