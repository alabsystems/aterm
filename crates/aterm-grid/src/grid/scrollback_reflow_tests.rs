// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn projection_image(z_index: i32) -> Arc<crate::ImageData> {
    Arc::new(crate::ImageData {
        bytes: vec![7; 256],
        format: crate::ImageFormat::Png,
        cols: 16,
        rows: 2,
        z_index,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: crate::ImageScaling::Fit,
        source_rect: None,
    })
}

fn projected_tiles(lines: &[Line]) -> Vec<(usize, u16, u16, u16)> {
    lines
        .iter()
        .enumerate()
        .flat_map(|(row, line)| {
            line.images().unwrap_or(&[]).iter().flat_map(move |span| {
                (span.start_col..span.end_col).map(move |col| {
                    let (image_row, image_col) = span.tile_at(col).unwrap();
                    (row, col, image_row, image_col)
                })
            })
        })
        .collect()
}

#[test]
fn wrapped_source_images_split_join_and_survive_retained_tails() {
    let image = projection_image(0);
    let witness = Arc::downgrade(&image);
    let mut head = Line::from("abcd");
    head.set_images(vec![ImageSpan::new(1, 4, 0, 1, Arc::clone(&image))]);
    let mut tail = Line::from("efgh");
    tail.set_wrapped(true);
    tail.set_images(vec![ImageSpan::new(0, 4, 0, 4, Arc::clone(&image))]);
    let input = vec![head, tail];
    let joined = reflow_scrollback_lines(&input, 8);
    assert_eq!(joined.len(), 1);
    assert_eq!(
        joined[0].images().unwrap().len(),
        1,
        "contiguous tiles coalesce"
    );
    assert_eq!(
        projected_tiles(&joined),
        (1..8).map(|col| (0, col, 0, col)).collect::<Vec<_>>()
    );
    let split = reflow_scrollback_lines(&input, 3);
    assert_eq!(
        projected_tiles(&split),
        vec![
            (0, 1, 0, 1),
            (0, 2, 0, 2),
            (1, 0, 0, 3),
            (1, 1, 0, 4),
            (1, 2, 0, 5),
            (2, 0, 0, 6),
            (2, 1, 0, 7),
        ]
    );
    for keep in 0..=split.len() + 1 {
        let newest = reflow_newest_rows(&input, 3, keep);
        assert_semantically_equal(&newest, &split[split.len().saturating_sub(keep)..]);
    }
    drop(input);
    drop(image);
    assert!(witness.upgrade().is_some());
    assert!(
        joined
            .iter()
            .chain(&split)
            .flat_map(|line| line.images().unwrap_or(&[]))
            .all(|span| Arc::ptr_eq(&span.image, &witness.upgrade().unwrap()))
    );
    drop(joined);
    drop(split);
    assert!(witness.upgrade().is_none());
}

#[test]
fn image_projection_respects_wide_early_wrap_and_tile_offsets() {
    let wide = CellAttrs::new(
        CellAttrs::DEFAULT.fg,
        CellAttrs::DEFAULT.bg,
        crate::CellFlags::WIDE.bits(),
    );
    let mut tail = Line::with_attrs("日z", [wide, CellAttrs::DEFAULT].into_iter().collect());
    tail.set_wrapped(true);
    tail.set_images(vec![ImageSpan::new(0, 3, 1, 2, projection_image(0))]);
    let input = [Line::from("ab"), tail];
    let out = reflow_scrollback_lines(&input, 3);
    assert_eq!(out[0].as_str(), Some("ab"));
    assert_eq!(out[1].as_str(), Some("日z"));
    assert_eq!(
        projected_tiles(&out),
        vec![(1, 0, 1, 2), (1, 1, 1, 3), (1, 2, 1, 4)]
    );
    let narrow = reflow_scrollback_lines(&input, 1);
    assert_eq!(projected_tiles(&narrow), vec![(2, 0, 1, 2), (3, 0, 1, 4)]);
}

#[test]
fn image_only_rows_clip_without_creating_text_or_wraps() {
    let mut line = Line::new();
    line.set_images(vec![ImageSpan::new(0, 12, 1, 2, projection_image(0))]);
    let out = reflow_scrollback_lines(&[line], 5);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].as_str(), Some(""));
    assert_eq!(
        projected_tiles(&out),
        (0..5).map(|col| (0, col, 1, col + 2)).collect::<Vec<_>>()
    );
}

#[test]
fn joined_image_fragments_keep_draw_key_and_equal_key_order() {
    for (head_z, tail_z) in [(3, -1), (0, 0)] {
        let head_image = projection_image(head_z);
        let tail_image = projection_image(tail_z);
        let mut head = Line::from("a");
        head.set_images(vec![ImageSpan::new(0, 4, 0, 0, Arc::clone(&head_image))]);
        let mut tail = Line::from("b");
        tail.set_wrapped(true);
        tail.set_images(vec![ImageSpan::new(0, 1, 0, 0, Arc::clone(&tail_image))]);
        let out = reflow_scrollback_lines(&[head, tail], 8);
        let spans = out[0].images().unwrap();
        assert_eq!(spans.len(), 2);
        let expected = if head_z == tail_z {
            [&head_image, &tail_image]
        } else {
            [&tail_image, &head_image]
        };
        for (actual, expected) in spans.iter().zip(expected) {
            assert!(Arc::ptr_eq(&actual.image, expected));
        }
    }
}

fn styled(text: &str, wrapped: bool) -> Line {
    let mut l = Line::from(text);
    l.set_wrapped(wrapped);
    l
}

#[test]
fn rewrap_resplits_each_logical_line_at_the_new_width() {
    type Rows<'a> = &'a [(&'a str, bool)];
    let cases: &[(&str, Rows, u16, Rows)] = &[
        (
            "shrink",
            &[("ABCDEFGHIJ", false)],
            4,
            &[("ABCD", false), ("EFGH", true), ("IJ", true)],
        ),
        (
            "grow merges a run",
            &[("ABCD", false), ("EFGH", true), ("IJ", true)],
            20,
            &[("ABCDEFGHIJ", false)],
        ),
        (
            "hard newlines stay",
            &[("ABC", false), ("DEF", false)],
            20,
            &[("ABC", false), ("DEF", false)],
        ),
        ("blank survives", &[("", false)], 10, &[("", false)]),
        (
            "wide char wraps whole",
            &[("世界", false)],
            3,
            &[("世", false), ("界", true)],
        ),
    ];
    for &(name, input, cols, want) in cases {
        let lines: Vec<Line> = input.iter().map(|&(t, w)| styled(t, w)).collect();
        let got: Vec<(String, bool)> = reflow_scrollback_lines(&lines, cols)
            .iter()
            .map(|l| (l.as_str().unwrap_or("").to_string(), l.is_wrapped()))
            .collect();
        let want: Vec<(String, bool)> = want.iter().map(|&(t, w)| (t.to_string(), w)).collect();
        assert_eq!(got, want, "{name}");
    }

    let original = vec![styled("The quick brown fox jumps", false)];
    let round_trip = reflow_scrollback_lines(&reflow_scrollback_lines(&original, 7), 40);
    assert_eq!(round_trip.len(), 1);
    assert_eq!(round_trip[0].as_str(), Some("The quick brown fox jumps"));
}

#[test]
fn rewrap_keeps_spans_on_their_cells_across_a_split() {
    use std::sync::Arc;
    let mut line = Line::with_hyperlinks(
        "ABCD",
        Rle::new(),
        vec![
            HyperlinkSpan::new(0, 1, Arc::from("a")),
            HyperlinkSpan::new(2, 3, Arc::from("b")),
        ],
    );
    line.set_underline_colors(vec![
        UnderlineColorSpan::new(0, 1, 0x01_FF_00_00),
        UnderlineColorSpan::new(2, 3, 0x02_00_00_03),
    ]);
    let out = reflow_scrollback_lines(&[line], 2);
    let texts: Vec<_> = out.iter().map(|l| l.as_str()).collect();
    assert_eq!(texts, [Some("AB"), Some("CD")]);
    for (row, url, color) in [(0, "a", 0x01_FF_00_00), (1, "b", 0x02_00_00_03)] {
        assert_eq!(out[row].get_hyperlink_span(0).map(|s| &*s.url), Some(url));
        assert_eq!(out[row].get_underline_color(0), Some(color));
        assert_eq!(out[row].get_hyperlink_span(1), None, "row {row} gap");
        assert_eq!(out[row].get_underline_color(1), None, "row {row} gap");
    }
}

#[test]
fn rewrap_preserves_stored_ambiguous_width_with_narrow_control() {
    use crate::CellFlags;

    const AMBIGUOUS: char = '\u{00B7}';
    assert_eq!(aterm_grapheme::char_width(AMBIGUOUS), 1);
    assert_eq!(aterm_grapheme::char_width_cjk(AMBIGUOUS), 2);

    let wide = CellAttrs::new(
        CellAttrs::DEFAULT.fg,
        CellAttrs::DEFAULT.bg,
        CellFlags::WIDE.bits(),
    );
    let wide_line = Line::with_attrs(
        "\u{00B7}Z",
        [wide, CellAttrs::DEFAULT].into_iter().collect(),
    );
    let out = reflow_scrollback_lines(&[wide_line], 2);
    assert_eq!(out.len(), 2, "stored-wide middle dot occupies both columns");
    assert_eq!(out[0].as_str(), Some("\u{00B7}"));
    assert_eq!(out[1].as_str(), Some("Z"));
    assert!(
        CellFlags::from_bits(out[0].get_attr(0).flags).contains(CellFlags::WIDE),
        "rewrap keeps the write-time WIDE authority for later materialization"
    );

    let narrow_line = Line::from("\u{00B7}Z");
    let narrow = reflow_scrollback_lines(&[narrow_line], 2);
    assert_eq!(narrow.len(), 1, "narrow control still fits both glyphs");
    assert_eq!(narrow[0].as_str(), Some("\u{00B7}Z"));
}

#[test]
fn rewrap_vs15_overrides_inconsistent_stored_wide_flag() {
    use crate::CellFlags;

    let stored_wide = CellAttrs::new(
        CellAttrs::DEFAULT.fg,
        CellAttrs::DEFAULT.bg,
        CellFlags::WIDE.bits(),
    );
    let line = Line::with_attrs(
        "\u{231A}\u{FE0E}Z",
        [stored_wide, stored_wide, CellAttrs::DEFAULT]
            .into_iter()
            .collect(),
    );
    let out = reflow_scrollback_lines(&[line], 2);
    assert_eq!(
        out.len(),
        1,
        "VS15 keeps the watch narrow despite WIDE attrs"
    );
    assert_eq!(out[0].as_str(), Some("\u{231A}\u{FE0E}Z"));
}

fn assert_semantically_equal(fast: &[Line], reference: &[Line]) {
    assert_eq!(fast.len(), reference.len(), "line counts diverge");
    for (idx, (a, b)) in fast.iter().zip(reference).enumerate() {
        assert_eq!(a.as_str(), b.as_str(), "text diverges at line {idx}");
        assert_eq!(
            a.is_wrapped(),
            b.is_wrapped(),
            "wrap flag diverges at line {idx}"
        );
        assert_eq!(a.images(), b.images(), "image spans diverge at line {idx}");
        let chars = a.as_str().map_or(0, |t| t.chars().count());
        for ci in 0..chars {
            assert_eq!(
                a.get_attr(ci),
                b.get_attr(ci),
                "attrs diverge at line {idx} char {ci}"
            );
        }
        for col in 0..64u16 {
            assert_eq!(
                a.get_hyperlink_span(col)
                    .map(|s| (s.url.clone(), s.id.clone())),
                b.get_hyperlink_span(col)
                    .map(|s| (s.url.clone(), s.id.clone())),
                "hyperlink diverges at line {idx} col {col}"
            );
            assert_eq!(
                a.get_underline_color(col),
                b.get_underline_color(col),
                "underline colour diverges at line {idx} col {col}"
            );
        }
    }
}

#[test]
fn reflow_passthrough_matches_full_path_on_mixed_corpus() {
    use std::sync::Arc;

    let styled_attr = CellAttrs::new(0x01_11_22_33, CellAttrs::DEFAULT.bg, 0);
    let wide_attr = CellAttrs::new(
        CellAttrs::DEFAULT.fg,
        CellAttrs::DEFAULT.bg,
        crate::CellFlags::WIDE.bits(),
    );
    let mut corpus: Vec<Line> = Vec::new();
    for i in 0..40 {
        corpus.push(styled(&format!("ls -la {i}"), false));
        corpus.push(Line::with_attrs(
            "AB",
            [styled_attr, CellAttrs::DEFAULT].into_iter().collect(),
        ));
        corpus.push(styled("", false));
        corpus.push(styled("ABCDEFGH", false));
        corpus.push(styled("IJKLMNOP", true));
        corpus.push(styled("世界世界", false));
        corpus.push(Line::with_attrs(
            "\u{231A}\u{FE0E}Z",
            [wide_attr, wide_attr, CellAttrs::DEFAULT]
                .into_iter()
                .collect(),
        ));
        corpus.push(Line::with_attrs(
            "WZ",
            [wide_attr, CellAttrs::DEFAULT].into_iter().collect(),
        ));
        corpus.push(Line::with_hyperlinks(
            "ABCD",
            Rle::new(),
            vec![
                HyperlinkSpan::new(0, 2, Arc::from("u0")),
                HyperlinkSpan::new(2, 4, Arc::from("u1")),
            ],
        ));
        let mut ul = Line::with_hyperlinks("ABCD", Rle::new(), Vec::new());
        ul.set_underline_colors(vec![UnderlineColorSpan::new(0, 2, 0x01_FF_00_00)]);
        corpus.push(ul);
        corpus.push(styled(&format!("L{i}-{}", "x".repeat(50)), false));
    }

    for new_cols in [11u16, 23, 40, 200] {
        let _ = crate::test_counters::take_reflow_passthrough_lines();
        let fast = reflow_scrollback_lines(&corpus, new_cols);
        let fired = crate::test_counters::take_reflow_passthrough_lines();
        let reference = reflow_scrollback_lines_reference(&corpus, new_cols);
        assert_semantically_equal(&fast, &reference);
        assert!(
            fired >= 80,
            "passthrough must fire on the eligible majority at {new_cols} \
             cols (fired {fired})"
        );
    }
}

#[test]
fn reflow_passthrough_never_fires_on_wrap_changing_input() {
    let _ = crate::test_counters::take_reflow_passthrough_lines();
    let long: Vec<Line> = (0..10)
        .map(|i| styled(&format!("L{i}-{}", "x".repeat(60)), false))
        .collect();
    let out = reflow_scrollback_lines(&long, 40);
    assert!(out.len() > 10, "sanity: the long lines actually wrapped");
    assert_eq!(
        crate::test_counters::take_reflow_passthrough_lines(),
        0,
        "no passthrough when the wrap points change"
    );
    let cjk = vec![styled("世界", false)];
    let _ = reflow_scrollback_lines(&cjk, 40);
    assert_eq!(
        crate::test_counters::take_reflow_passthrough_lines(),
        0,
        "no passthrough for non-ASCII content"
    );
    let run = vec![styled("ABC", false), styled("DEF", true)];
    let _ = reflow_scrollback_lines(&run, 40);
    assert_eq!(
        crate::test_counters::take_reflow_passthrough_lines(),
        0,
        "no passthrough for multi-row logical lines"
    );
}

#[test]
fn decoded_hyperlinks_borrow_sources_until_output_owns_the_span() {
    use std::sync::Arc;
    let url: Arc<str> = Arc::from("https://example.com");
    let id: Arc<str> = Arc::from("id");
    let source = Line::with_hyperlinks(
        &"x".repeat(4096),
        Rle::new(),
        vec![HyperlinkSpan::with_id(
            0,
            4096,
            url.clone(),
            Some(id.clone()),
        )],
    );
    let owners = (Arc::strong_count(&url), Arc::strong_count(&id));
    let mut units = Vec::new();
    collect_units(&source, &mut units);
    assert_eq!(units.len(), 4096);
    assert_eq!(
        (Arc::strong_count(&url), Arc::strong_count(&id)),
        owners,
        "decoded cells must not retain a URL and ID separately per unit"
    );
    let output = build_line(&units, false);
    assert_eq!(
        (Arc::strong_count(&url), Arc::strong_count(&id)),
        owners,
        "the output shares one payload rather than separately cloning URL and ID"
    );
    drop(units);
    drop(source);
    let span = output
        .get_hyperlink_span(4095)
        .expect("output owns its span");
    assert!(Arc::ptr_eq(&span.url, &url));
    assert_eq!(span.id.as_deref(), Some("id"));
    assert_eq!(output.as_str().map(str::len), Some(4096));
}

#[test]
fn mapped_history_point_keeps_cell_and_wrap_boundaries() {
    let input = [
        styled("AB", false),
        styled("日C", true),
        styled("", true),
        styled("D", false),
    ];
    type Case = (u16, usize, [(usize, u16); 7]);
    let cases: &[Case] = &[
        (
            0,
            5,
            [(0, 0), (1, 0), (2, 0), (3, 0), (3, 1), (4, 0), (4, 1)],
        ),
        (
            1,
            5,
            [(0, 0), (1, 0), (2, 0), (3, 0), (3, 1), (4, 0), (4, 1)],
        ),
        (
            2,
            4,
            [(0, 0), (0, 1), (1, 0), (2, 0), (2, 1), (3, 0), (3, 1)],
        ),
        (
            3,
            3,
            [(0, 0), (0, 1), (1, 0), (1, 2), (1, 3), (2, 0), (2, 1)],
        ),
        (
            4,
            3,
            [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (2, 0), (2, 1)],
        ),
        (
            20,
            2,
            [(0, 0), (0, 1), (0, 2), (0, 4), (0, 5), (1, 0), (1, 1)],
        ),
    ];
    for &(cols, rows, [a, b, wide, c, end, d, d_end]) in cases {
        for (line, cells, expected) in [
            (0, 0, a),
            (0, 1, b),
            (0, 2, wide),
            (0, 99, wide),
            (1, 0, wide),
            (1, 1, wide),
            (1, 2, c),
            (1, 3, end),
            (1, 99, end),
            (2, 0, end),
            (2, 99, end),
            (3, 0, d),
            (3, 1, d_end),
            (3, 99, d_end),
        ] {
            assert_eq!(
                rewrap_cell_row(&input, cols, line, cells),
                (rows, Some(expected)),
                "width={cols}, line={line}, cells={cells}"
            );
        }
        assert_eq!(rewrap_cell_row(&input, cols, 4, 0), (rows, None));
    }
    assert_eq!(rewrap_cell_row(&[], 0, 0, 0), (0, None));
    assert_eq!(
        rewrap_cell_row(&[styled("", false)], 1, 0, 99),
        (1, Some((0, 0)))
    );
    let joined = [styled("e\u{301}👩‍💻Z", false)];
    for (cells, expected) in [
        (0, (0, 0)),
        (1, (1, 0)),
        (2, (1, 0)),
        (3, (2, 0)),
        (4, (2, 1)),
    ] {
        assert_eq!(rewrap_cell_row(&joined, 2, 0, cells), (3, Some(expected)));
    }
}

#[test]
fn newest_rows_builds_only_retained_rows_of_a_logical_line() {
    let input = [styled(&"x".repeat(16_384), false)];
    BUILT_HISTORY_ROWS.with(|count| count.set(0));
    let whole = reflow_scrollback_lines(&input, 1);
    assert_eq!(BUILT_HISTORY_ROWS.with(|count| count.replace(0)), 16_384);
    for rows in [0, 1, 4, 40] {
        let newest = reflow_newest_rows(&input, 1, rows);
        let built = BUILT_HISTORY_ROWS.with(|count| count.replace(0));
        assert!(built <= rows, "built {built} rows for a {rows}-row request");
        assert_semantically_equal(&newest, &whole[whole.len().saturating_sub(rows)..]);
    }
}

#[test]
fn reflow_newest_rows_is_the_tail_of_the_whole_rewrap() {
    let mut corpus = vec![styled("orphan tail", true)];
    let mut decorated = Line::with_hyperlinks(
        "e\u{301}日👩‍💻ABCDEFGHIJ",
        Rle::with_value(CellAttrs::new(0x01_112233, 0x01_aabbcc, 0), 32),
        vec![HyperlinkSpan::new(0, 8, std::sync::Arc::from("u"))],
    );
    decorated.set_underline_colors(vec![UnderlineColorSpan::new(2, 9, 0x01_123456)]);
    corpus.push(decorated);
    let image = std::sync::Arc::new(crate::ImageData {
        bytes: vec![7; 256],
        format: crate::ImageFormat::Png,
        cols: 2,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: crate::ImageScaling::Fit,
        source_rect: None,
    });
    for text in ["image-bearing-long-row", ""] {
        let mut line = styled(text, false);
        line.set_images(vec![ImageSpan::new(0, 2, 0, 0, image.clone())]);
        corpus.push(line);
    }
    for i in 0..12 {
        corpus.push(styled(&format!("R{i}-ABCDEFGH"), false));
        corpus.push(styled("IJKLMNOP", true));
        corpus.push(styled("QR", true));
        corpus.push(styled("", false));
        corpus.push(styled("世界世界世", false));
        corpus.push(styled(&format!("short {i}"), false));
    }
    for new_cols in [0u16, 1, 3, 7, 11, 40] {
        let whole = reflow_scrollback_lines(&corpus, new_cols);
        for rows in 0..=whole.len() + 2 {
            let newest = reflow_newest_rows(&corpus, new_cols, rows);
            assert_semantically_equal(&newest, &whole[whole.len().saturating_sub(rows)..]);
        }
    }

    let plain: Vec<Line> = (0..1000).map(|i| styled(&format!("p{i}"), false)).collect();
    let _ = crate::test_counters::take_reflow_passthrough_lines();
    let newest = reflow_newest_rows(&plain, 40, 1);
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].as_str(), Some("p999"));
    assert_eq!(
        crate::test_counters::take_reflow_passthrough_lines(),
        1,
        "one row asked of single-row lines rewraps the newest one only"
    );
}
